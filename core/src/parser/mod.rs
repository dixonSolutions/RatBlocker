//! Parsing of EasyList/Adblock Plus syntax, hosts files and plain domain lists
//! into RatBlocker's normalized rule model.
//!
//! The parser never trusts its input: every rule is validated here so that a
//! hostile or merely broken subscription cannot produce a rule the matcher
//! would spend unbounded time on. Rejections are reported, not silently
//! dropped, so the compiler can surface them.

use crate::rule_engine::rules::{
    Anchor, CosmeticRule, DomainConstraint, ExceptionScope, NetworkRule, PartyConstraint,
    RemoveParam, RuleOptions,
};
use crate::types::ResourceType;
use crate::url::{normalize_domain_pattern, normalize_host};

/// Longest rule line accepted. EasyList's longest real rules are well under 1K.
pub const MAX_RULE_LEN: usize = 16 * 1024;
/// Longest `/regex/` body accepted.
pub const MAX_REGEX_LEN: usize = 512;
/// A pattern with no anchor needs at least this many literal characters,
/// otherwise it degenerates into a full scan of every URL.
pub const MIN_UNANCHORED_LITERAL: usize = 3;

/// Why a line was not turned into a rule.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RejectReason {
    #[error("line exceeds {MAX_RULE_LEN} bytes")]
    TooLong,
    #[error("line is not valid UTF-8 or contains control characters")]
    BadEncoding,
    #[error("malformed domain: {0}")]
    MalformedDomain(String),
    #[error("empty pattern")]
    EmptyPattern,
    #[error("pattern is too generic to index (needs {MIN_UNANCHORED_LITERAL}+ literal characters)")]
    TooGeneric,
    #[error("regular expression is too long or too expensive")]
    UnsafeRegex,
    #[error("unknown rule option: {0}")]
    UnknownOption(String),
    #[error("unsupported rule option: {0}")]
    UnsupportedOption(String),
    #[error("invalid value for option {0}")]
    InvalidOptionValue(String),
    #[error("redirect target is not a bundled resource name: {0}")]
    UnsafeRedirectTarget(String),
    #[error("unsupported cosmetic rule syntax")]
    UnsupportedCosmetic,
    #[error("empty CSS selector")]
    EmptySelector,
}

/// Result of parsing a single line.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedLine {
    Empty,
    Comment,
    /// `! Title: EasyList`
    Metadata { key: String, value: String },
    Network(Box<NetworkRule>),
    Cosmetic(Box<CosmeticRule>),
    /// `$badfilter` — cancels an identical rule from another list.
    BadFilter(Box<NetworkRule>),
    Rejected {
        reason: RejectReason,
        raw: String,
    },
}

/// How to interpret the lines of a source list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListFormat {
    /// EasyList / Adblock Plus syntax.
    Adblock,
    /// `0.0.0.0 tracker.example` hosts file.
    Hosts,
    /// One bare domain per line.
    Domains,
}

impl ListFormat {
    /// Guess the format from the first lines of a list.
    ///
    /// Adblock syntax is decisive: hosts files and domain lists never contain
    /// `||`, `@@` or `##`, so a single such line settles it.
    pub fn detect(sample: &str) -> ListFormat {
        let mut hosts_hits = 0usize;
        let mut domain_hits = 0usize;
        for line in sample.lines().take(500) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // The standard Adblock header, wherever it appears.
            if line.starts_with("[Adblock") {
                return ListFormat::Adblock;
            }
            if line.starts_with('!') {
                continue;
            }
            if line.starts_with('#') && !line.starts_with("##") {
                // A hosts-file comment, not a generic cosmetic rule.
                continue;
            }
            if line.starts_with("||")
                || line.starts_with("@@")
                || line.contains("##")
                || line.contains("#@#")
                || line.contains("$domain=")
                || line.contains("$third-party")
            {
                return ListFormat::Adblock;
            }
            let mut fields = line.split_whitespace();
            match (fields.next(), fields.next()) {
                (Some(addr), Some(host))
                    if matches!(addr, "0.0.0.0" | "127.0.0.1" | "::1" | "::")
                        && normalize_host(host).is_ok() =>
                {
                    hosts_hits += 1
                }
                (Some(only), None) if normalize_host(only).is_ok() => domain_hits += 1,
                _ => {}
            }
        }
        if hosts_hits > domain_hits {
            ListFormat::Hosts
        } else if domain_hits > 0 {
            ListFormat::Domains
        } else {
            ListFormat::Adblock
        }
    }
}

/// Parse one line. `id` is the stable rule identifier to assign.
pub fn parse_line(line: &str, format: ListFormat, id: &str) -> ParsedLine {
    if line.len() > MAX_RULE_LEN {
        return reject(RejectReason::TooLong, line);
    }
    if line.bytes().any(|b| b < 0x09 || (0x0b..0x20).contains(&b)) {
        return reject(RejectReason::BadEncoding, line);
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return ParsedLine::Empty;
    }
    match format {
        ListFormat::Adblock => parse_adblock_line(trimmed, id),
        ListFormat::Hosts => parse_hosts_line(trimmed, id),
        ListFormat::Domains => parse_domain_line(trimmed, id),
    }
}

fn reject(reason: RejectReason, raw: &str) -> ParsedLine {
    ParsedLine::Rejected {
        reason,
        raw: raw.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Adblock Plus syntax
// ---------------------------------------------------------------------------

fn parse_adblock_line(line: &str, id: &str) -> ParsedLine {
    if line.starts_with('[') && line.ends_with(']') {
        return ParsedLine::Comment;
    }
    if let Some(rest) = line.strip_prefix('!') {
        let rest = rest.trim();
        if let Some((key, value)) = rest.split_once(':') {
            let key = key.trim();
            if matches!(
                key.to_ascii_lowercase().as_str(),
                "title" | "version" | "expires" | "homepage" | "licence" | "license" | "last modified" | "redirect"
            ) {
                return ParsedLine::Metadata {
                    key: key.to_ascii_lowercase(),
                    value: value.trim().to_string(),
                };
            }
        }
        return ParsedLine::Comment;
    }

    // Cosmetic separators must be checked before `$` option parsing, because a
    // CSS selector may legitimately contain `$`.
    if let Some(kind) = find_cosmetic_separator(line) {
        return parse_cosmetic(line, kind, id);
    }

    parse_network(line, id)
}

#[derive(Clone, Copy)]
struct CosmeticSep {
    at: usize,
    len: usize,
    exception: bool,
    supported: bool,
}

fn find_cosmetic_separator(line: &str) -> Option<CosmeticSep> {
    // Ordered longest-first so `#@#` is not mistaken for `##`.
    const SEPS: &[(&str, bool, bool)] = &[
        ("#@#", true, true),
        ("#?#", false, false),   // procedural (:has-text etc.)
        ("#@?#", true, false),
        ("#$#", false, false),   // CSS injection / scriptlets
        ("#@$#", true, false),
        ("##", false, true),
    ];
    let mut best: Option<CosmeticSep> = None;
    for (sep, exception, supported) in SEPS {
        if let Some(at) = line.find(sep) {
            let cand = CosmeticSep {
                at,
                len: sep.len(),
                exception: *exception,
                supported: *supported,
            };
            best = match best {
                // Earliest separator wins; on a tie the longer one wins.
                Some(b) if b.at < cand.at || (b.at == cand.at && b.len >= cand.len) => Some(b),
                _ => Some(cand),
            };
        }
    }
    best
}

fn parse_cosmetic(line: &str, sep: CosmeticSep, id: &str) -> ParsedLine {
    if !sep.supported {
        return reject(RejectReason::UnsupportedCosmetic, line);
    }
    let (domains_part, selector) = line.split_at(sep.at);
    let selector = selector[sep.len..].trim();
    if selector.is_empty() {
        return reject(RejectReason::EmptySelector, line);
    }
    // Scriptlet injection is out of scope: it would mean shipping remotely
    // supplied executable code, which the update policy forbids.
    if selector.starts_with("+js(") {
        return reject(RejectReason::UnsupportedCosmetic, line);
    }

    let mut included = Vec::new();
    let mut excluded = Vec::new();
    for entry in domains_part.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (negated, host) = match entry.strip_prefix('~') {
            Some(h) => (true, h),
            None => (false, entry),
        };
        match normalize_domain_pattern(host) {
            Ok(h) if negated => excluded.push(h),
            Ok(h) => included.push(h),
            Err(_) => return reject(RejectReason::MalformedDomain(host.to_string()), line),
        }
    }

    ParsedLine::Cosmetic(Box::new(CosmeticRule {
        id: id.to_string(),
        included_domains: included,
        excluded_domains: excluded,
        selector: selector.to_string(),
        is_exception: sep.exception,
    }))
}

fn parse_network(line: &str, id: &str) -> ParsedLine {
    let (body, is_exception) = match line.strip_prefix("@@") {
        Some(rest) => (rest, true),
        None => (line, false),
    };

    let (pattern, option_str) = split_options(body);
    if pattern.is_empty() && option_str.is_none() {
        return reject(RejectReason::EmptyPattern, line);
    }

    let mut options = RuleOptions::default();
    let mut badfilter = false;
    if let Some(opts) = option_str {
        match parse_options(opts, &mut options) {
            Ok(bf) => badfilter = bf,
            Err(reason) => return reject(reason, line),
        }
    }

    let rule = match build_pattern(pattern, id, line, is_exception, options) {
        Ok(r) => r,
        Err(reason) => return reject(reason, line),
    };

    if badfilter {
        ParsedLine::BadFilter(Box::new(rule))
    } else {
        ParsedLine::Network(Box::new(rule))
    }
}

/// Split `pattern$options`, ignoring a `$` that is inside a `/regex/` body.
fn split_options(body: &str) -> (&str, Option<&str>) {
    let bytes = body.as_bytes();
    let is_regex = bytes.first() == Some(&b'/');
    if is_regex {
        // Options begin after the closing slash of the regex.
        if let Some(close) = body.rfind('/') {
            if close > 0 {
                let tail = &body[close + 1..];
                return match tail.strip_prefix('$') {
                    Some(o) => (&body[..close + 1], Some(o)),
                    None => (body, None),
                };
            }
        }
        return (body, None);
    }
    match body.rfind('$') {
        Some(i) => (&body[..i], Some(&body[i + 1..])),
        None => (body, None),
    }
}

fn build_pattern(
    pattern: &str,
    id: &str,
    raw: &str,
    is_exception: bool,
    options: RuleOptions,
) -> Result<NetworkRule, RejectReason> {
    let mut rule = NetworkRule {
        id: id.to_string(),
        raw: raw.to_string(),
        anchor: Anchor::None,
        host_anchor: None,
        parts: Vec::new(),
        right_anchored: false,
        regex: None,
        is_exception,
        options,
    };

    // Whitespace never appears in a valid pattern; a rule containing it is a
    // malformed line rather than a filter for a host with a space in it.
    if pattern.bytes().any(|b| b.is_ascii_whitespace()) {
        return Err(RejectReason::MalformedDomain(pattern.to_string()));
    }

    // `/foo/` is a regular expression, but only when it is not just a path.
    if pattern.len() > 2 && pattern.starts_with('/') && pattern.ends_with('/') {
        let body = &pattern[1..pattern.len() - 1];
        if body.len() > MAX_REGEX_LEN {
            return Err(RejectReason::UnsafeRegex);
        }
        // The engine uses a linear-time regex implementation, so the risk is
        // compiled-program size rather than backtracking blowup.
        if !crate::matcher::regex_is_acceptable(body) {
            return Err(RejectReason::UnsafeRegex);
        }
        rule.regex = Some(body.to_string());
        return Ok(rule);
    }

    let mut p = pattern;
    if let Some(rest) = p.strip_prefix("||") {
        rule.anchor = Anchor::Hostname;
        p = rest;
    } else if let Some(rest) = p.strip_prefix('|') {
        rule.anchor = Anchor::Start;
        p = rest;
    }
    if let Some(rest) = p.strip_suffix('|') {
        rule.right_anchored = true;
        p = rest;
    }

    if p.is_empty() {
        // A bare `$removeparam` / `$redirect` rule with no pattern applies to
        // every URL; that is legitimate but must stay opt-in via options.
        // A rule with no pattern applies to every URL. That is only meaningful
        // — and only safe — when some option narrows it down.
        let scoped = rule.options.removeparam.is_some()
            || rule.options.redirect.is_some()
            || !rule.options.domains.is_empty()
            || !rule.options.apps.is_empty();
        if scoped {
            rule.parts = Vec::new();
            return Ok(rule);
        }
        return Err(RejectReason::EmptyPattern);
    }

    rule.parts = p
        .split('*')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    if rule.anchor == Anchor::Hostname {
        let first = rule.parts.first().ok_or(RejectReason::EmptyPattern)?;
        let host: String = first
            .chars()
            .take_while(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_') || !c.is_ascii()
            })
            .collect();
        if host.is_empty() {
            return Err(RejectReason::MalformedDomain(first.clone()));
        }
        // Validate the anchored host, but tolerate a partial label such as
        // `||ads.` which EasyList uses as a prefix match.
        let probe = host.trim_end_matches('.');
        if !probe.is_empty() && normalize_host(probe).is_err() {
            return Err(RejectReason::MalformedDomain(host));
        }
        rule.host_anchor = Some(host.to_ascii_lowercase());
    }

    let longest = rule.parts.iter().map(|p| p.len()).max().unwrap_or(0);
    if rule.anchor == Anchor::None && !rule.right_anchored && longest < MIN_UNANCHORED_LITERAL {
        return Err(RejectReason::TooGeneric);
    }

    if !rule.options.match_case {
        for part in &mut rule.parts {
            *part = part.to_ascii_lowercase();
        }
    }

    Ok(rule)
}

/// Options that change response contents or headers. RatBlocker does not
/// implement them, and silently ignoring them would mis-apply the rule.
const UNSUPPORTED_OPTIONS: &[&str] = &[
    "csp", "replace", "permissions", "inline-script", "inline-font", "empty", "mp4",
    "stealth", "cookie", "hls", "jsonprune", "urltransform", "referrerpolicy",
    "method", "header", "extension", "network", "to",
];

fn parse_options(opts: &str, out: &mut RuleOptions) -> Result<bool, RejectReason> {
    let mut positive_mask = 0u32;
    let mut negative_mask = 0u32;
    let mut badfilter = false;

    for opt in split_option_list(opts) {
        let opt = opt.trim();
        if opt.is_empty() {
            continue;
        }
        let (negated, opt) = match opt.strip_prefix('~') {
            Some(o) => (true, o),
            None => (false, opt),
        };
        let (name, value) = match opt.split_once('=') {
            Some((n, v)) => (n.trim().to_ascii_lowercase(), Some(v.trim())),
            None => (opt.trim().to_ascii_lowercase(), None),
        };

        if let Some(rt) = ResourceType::from_option(&name) {
            if negated {
                negative_mask |= rt.mask();
            } else {
                positive_mask |= rt.mask();
            }
            // `$document` on an exception rule is a scope, not just a type.
            if name == "document" && !negated {
                out.scope.insert(ExceptionScope::DOCUMENT);
            }
            continue;
        }

        match name.as_str() {
            "third-party" | "3p" => {
                out.party = if negated {
                    PartyConstraint::FirstOnly
                } else {
                    PartyConstraint::ThirdOnly
                }
            }
            "first-party" | "1p" => {
                out.party = if negated {
                    PartyConstraint::ThirdOnly
                } else {
                    PartyConstraint::FirstOnly
                }
            }
            "match-case" => out.match_case = !negated,
            "important" => out.important = !negated,
            "badfilter" => badfilter = true,
            "elemhide" | "ehide" => out.scope.insert(ExceptionScope::ELEMHIDE),
            "generichide" | "ghide" => out.scope.insert(ExceptionScope::GENERICHIDE),
            "specifichide" | "shide" => out.scope.insert(ExceptionScope::SPECIFICHIDE),
            "genericblock" => out.scope.insert(ExceptionScope::GENERICBLOCK),
            "all" => {
                positive_mask |= ResourceType::ALL;
                out.scope.insert(ExceptionScope::DOCUMENT);
            }
            "popup" => {
                // Popups are a navigation, i.e. a document request.
                positive_mask |= ResourceType::Document.mask();
            }
            "domain" | "from" => {
                let v = value.ok_or_else(|| RejectReason::InvalidOptionValue(name.clone()))?;
                out.domains = parse_domain_constraint(v, true)
                    .map_err(|d| RejectReason::MalformedDomain(d))?;
            }
            "app" => {
                let v = value.ok_or_else(|| RejectReason::InvalidOptionValue(name.clone()))?;
                out.apps = parse_domain_constraint(v, false)
                    .map_err(|d| RejectReason::MalformedDomain(d))?;
            }
            "denyallow" => {
                let v = value.ok_or_else(|| RejectReason::InvalidOptionValue(name.clone()))?;
                let c = parse_domain_constraint(v, true)
                    .map_err(|d| RejectReason::MalformedDomain(d))?;
                out.denyallow = c.included;
            }
            "removeparam" | "queryprune" => {
                out.removeparam = Some(match value {
                    None => RemoveParam::All,
                    Some(v) if v.is_empty() => RemoveParam::All,
                    Some(v) => {
                        // A regex-valued removeparam is not supported; only
                        // literal parameter names are.
                        if v.starts_with('/') {
                            return Err(RejectReason::UnsupportedOption(
                                "removeparam=/regex/".into(),
                            ));
                        }
                        let inverted = v.starts_with('~');
                        let names: Vec<String> = v
                            .split('|')
                            .map(|s| s.trim().trim_start_matches('~').to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                        if names.is_empty() {
                            return Err(RejectReason::InvalidOptionValue(name.clone()));
                        }
                        if inverted {
                            RemoveParam::Inverted(names)
                        } else {
                            RemoveParam::Named(names)
                        }
                    }
                });
            }
            "redirect" | "redirect-rule" | "rewrite" => {
                let v = value.unwrap_or("");
                if v.is_empty() {
                    // `@@...$redirect-rule` with no value disables redirect
                    // rules for the request; there is nothing to redirect to.
                    continue;
                }
                // ABP spells the same thing `$rewrite=abp-resource:blank-mp4`.
                let target = v.strip_prefix("abp-resource:").unwrap_or(v);
                // A redirect target names a resource RatBlocker ships. A filter
                // list must never be able to point it at a URL or a filesystem
                // path: that would turn a subscription into a redirector.
                if !is_safe_resource_name(target) {
                    return Err(RejectReason::UnsafeRedirectTarget(target.to_string()));
                }
                out.redirect = Some(target.to_string());
            }
            other if UNSUPPORTED_OPTIONS.contains(&other) => {
                return Err(RejectReason::UnsupportedOption(other.to_string()))
            }
            other => return Err(RejectReason::UnknownOption(other.to_string())),
        }
    }

    out.resource_mask = if positive_mask != 0 {
        positive_mask & !negative_mask
    } else {
        ResourceType::ALL & !negative_mask
    };
    if out.resource_mask == 0 {
        return Err(RejectReason::InvalidOptionValue("resource types".into()));
    }

    Ok(badfilter)
}

/// A `$redirect` / `$rewrite` target: the name of a resource bundled with
/// RatBlocker, never a path or a URL.
fn is_safe_resource_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Split an option list on commas.
///
/// A comma inside a regular-expression value must not split the list. Regex
/// values only ever appear immediately after `=`, so entering "regex mode"
/// requires that context — treating every `/` as a delimiter would mis-split
/// ordinary values that contain slashes.
fn split_option_list(opts: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut in_regex = false;
    let bytes = opts.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        let escaped = i > 0 && bytes[i - 1] == b'\\';
        match b {
            b'/' if !escaped && !in_regex && i > 0 && bytes[i - 1] == b'=' => in_regex = true,
            b'/' if !escaped && in_regex => in_regex = false,
            b',' if !in_regex => {
                out.push(&opts[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&opts[start..]);
    out
}

fn parse_domain_constraint(value: &str, as_host: bool) -> Result<DomainConstraint, String> {
    let mut c = DomainConstraint::default();
    for entry in value.split(['|', ',']) {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (negated, item) = match entry.strip_prefix('~') {
            Some(i) => (true, i),
            None => (false, entry),
        };
        let item = if as_host {
            normalize_domain_pattern(item).map_err(|_| item.to_string())?
        } else {
            item.to_string()
        };
        if negated {
            c.excluded.push(item);
        } else {
            c.included.push(item);
        }
    }
    Ok(c)
}

// ---------------------------------------------------------------------------
// Hosts files and plain domain lists
// ---------------------------------------------------------------------------

fn parse_hosts_line(line: &str, id: &str) -> ParsedLine {
    let line = line.split('#').next().unwrap_or(line).trim();
    if line.is_empty() {
        return ParsedLine::Comment;
    }
    let mut fields = line.split_whitespace();
    let addr = match fields.next() {
        Some(a) => a,
        None => return ParsedLine::Empty,
    };
    // Only sinkhole entries are blocking rules; real host mappings are not.
    if !matches!(addr, "0.0.0.0" | "127.0.0.1" | "::1" | "::" | "0.0.0.0.0") {
        return ParsedLine::Comment;
    }
    let mut rules = Vec::new();
    for host in fields {
        match normalize_host(host) {
            Ok(h) if h != "localhost" && h != "localhost.localdomain" && h != "broadcasthost" => {
                rules.push(h)
            }
            Ok(_) => {}
            Err(_) => return reject(RejectReason::MalformedDomain(host.to_string()), line),
        }
    }
    match rules.into_iter().next() {
        Some(host) => ParsedLine::Network(Box::new(host_block_rule(&host, id, line))),
        None => ParsedLine::Comment,
    }
}

fn parse_domain_line(line: &str, id: &str) -> ParsedLine {
    let line = line.split('#').next().unwrap_or(line).trim();
    if line.is_empty() {
        return ParsedLine::Comment;
    }
    match normalize_host(line) {
        Ok(h) => ParsedLine::Network(Box::new(host_block_rule(&h, id, line))),
        Err(_) => reject(RejectReason::MalformedDomain(line.to_string()), line),
    }
}

/// Build the equivalent of `||host^`.
fn host_block_rule(host: &str, id: &str, raw: &str) -> NetworkRule {
    NetworkRule {
        id: id.to_string(),
        raw: raw.to_string(),
        anchor: Anchor::Hostname,
        host_anchor: Some(host.to_string()),
        parts: vec![format!("{host}^")],
        right_anchored: false,
        regex: None,
        is_exception: false,
        options: RuleOptions::default(),
    }
}
