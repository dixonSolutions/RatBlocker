//! Rule indexing and matching.
//!
//! The design goal from `docs/architecture.md` is explicit: *RatBlocker must
//! not scan every rule for every request.* Rules are therefore filed into one
//! of three places at build time, and a lookup only ever examines the small
//! candidate set a request can possibly hit:
//!
//! * `hostname` — `||example.com^` style rules, keyed by the anchored host.
//!   A request needs one hash lookup per label of its hostname (~3).
//! * `tokens`   — everything else, keyed by the longest alphanumeric run in the
//!   pattern. A request needs one hash lookup per token in its URL.
//! * `unindexed` — regular expressions and patterns with no usable token. Kept
//!   deliberately small; the parser rejects patterns that would land here
//!   without at least an anchor.

use std::collections::HashMap;

use regex::{Regex, RegexBuilder};

use crate::rule_engine::rules::{Anchor, NetworkRule};
use crate::types::{Party, ResourceType};
use crate::url::ParsedUrl;

/// Upper bound on a compiled regular expression, in bytes. Keeps a hostile
/// filter list from exhausting memory through pattern size alone.
const REGEX_SIZE_LIMIT: usize = 64 * 1024;

/// Shortest alphanumeric run treated as an index token.
const MIN_TOKEN_LEN: usize = 3;

/// True when `body` compiles to a regular expression RatBlocker is willing to
/// run. The `regex` crate guarantees linear-time matching, so this checks size
/// and constructability rather than backtracking behaviour.
pub fn regex_is_acceptable(body: &str) -> bool {
    build_regex(body).is_some()
}

fn build_regex(body: &str) -> Option<Regex> {
    RegexBuilder::new(body)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .case_insensitive(true)
        .build()
        .ok()
}

/// `^` in Adblock syntax: any character that is not part of a name.
#[inline]
fn is_separator(b: u8) -> bool {
    !(b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'%'))
}

/// FNV-1a. Small, fast, and stable across runs so a compiled database stays
/// valid between processes.
#[inline]
fn hash_token(s: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in s {
        h ^= b.to_ascii_lowercase() as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// Alphanumeric runs of at least `MIN_TOKEN_LEN` bytes, hashed.
fn tokenize(s: &str) -> Vec<u64> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(16);
    let mut start = None;
    for i in 0..=bytes.len() {
        let alnum = i < bytes.len() && bytes[i].is_ascii_alphanumeric();
        match (alnum, start) {
            (true, None) => start = Some(i),
            (false, Some(s0)) => {
                if i - s0 >= MIN_TOKEN_LEN {
                    out.push(hash_token(&bytes[s0..i]));
                }
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// Pick the token a rule should be filed under: the longest alphanumeric run in
/// its pattern, which is the best available proxy for the rarest token.
fn best_token(rule: &NetworkRule) -> Option<u64> {
    let mut best: Option<(usize, u64)> = None;
    for part in &rule.parts {
        let bytes = part.as_bytes();
        let mut start = None;
        for i in 0..=bytes.len() {
            let alnum = i < bytes.len() && bytes[i].is_ascii_alphanumeric();
            match (alnum, start) {
                (true, None) => start = Some(i),
                (false, Some(s0)) => {
                    let len = i - s0;
                    if len >= MIN_TOKEN_LEN && best.map_or(true, |(bl, _)| len > bl) {
                        best = Some((len, hash_token(&bytes[s0..i])));
                    }
                    start = None;
                }
                _ => {}
            }
        }
    }
    best.map(|(_, h)| h)
}

// ---------------------------------------------------------------------------
// Pattern matching
// ---------------------------------------------------------------------------

/// Match `part` against `hay` starting exactly at `pos`. Returns the end offset.
fn part_match_at(hay: &[u8], pos: usize, part: &[u8]) -> Option<usize> {
    let mut i = pos;
    for (k, &pb) in part.iter().enumerate() {
        if pb == b'^' {
            if i >= hay.len() {
                // A trailing `^` also matches the end of the URL.
                return if k + 1 == part.len() { Some(i) } else { None };
            }
            if !is_separator(hay[i]) {
                return None;
            }
            i += 1;
        } else {
            if i >= hay.len() || hay[i] != pb {
                return None;
            }
            i += 1;
        }
    }
    Some(i)
}

/// Leftmost occurrence of `part` at or after `from`.
fn find_part(hay: &[u8], from: usize, part: &[u8]) -> Option<(usize, usize)> {
    // `<=` so a pattern ending in `^` can still match at end-of-string.
    for start in from..=hay.len() {
        if let Some(end) = part_match_at(hay, start, part) {
            return Some((start, end));
        }
    }
    None
}

/// Match every part in order, with the first part anchored at `start`.
fn match_parts_from(hay: &[u8], parts: &[String], start: usize, right_anchored: bool) -> bool {
    if parts.is_empty() {
        return true;
    }
    let last = parts.len() - 1;

    let mut pos = match part_match_at(hay, start, parts[0].as_bytes()) {
        Some(end) => end,
        None => return false,
    };
    if last == 0 {
        return !right_anchored || pos == hay.len();
    }

    for part in &parts[1..last] {
        match find_part(hay, pos, part.as_bytes()) {
            Some((_, end)) => pos = end,
            None => return false,
        }
    }

    let tail = parts[last].as_bytes();
    if right_anchored {
        // Scan forward for an occurrence that finishes exactly at the end.
        let mut from = pos;
        while let Some((s, end)) = find_part(hay, from, tail) {
            if end == hay.len() {
                return true;
            }
            from = s + 1;
            if from > hay.len() {
                break;
            }
        }
        false
    } else {
        find_part(hay, pos, tail).is_some()
    }
}

/// Byte offsets in `normalized` at which a `||` pattern may begin: the start of
/// the hostname, and every position just after a dot inside it.
fn hostname_anchor_positions(url: &ParsedUrl) -> Vec<usize> {
    let Some(scheme_end) = url.normalized.find("://") else {
        return Vec::new();
    };
    let host_start = scheme_end + 3;
    let host_len = url.host.len();
    let bytes = url.normalized.as_bytes();
    let mut positions = Vec::with_capacity(4);
    positions.push(host_start);
    for i in host_start..(host_start + host_len).min(bytes.len()) {
        if bytes[i] == b'.' {
            positions.push(i + 1);
        }
    }
    positions
}

/// Does this rule's *pattern* match the URL? Options are checked separately.
fn pattern_matches(rule: &NetworkRule, url: &ParsedUrl, regex: Option<&Regex>) -> bool {
    if let Some(re) = regex {
        return re.is_match(&url.normalized);
    }
    if rule.parts.is_empty() {
        // A pattern-less rule (bare `$removeparam`) applies to every URL.
        return true;
    }
    let hay = url.normalized.as_bytes();
    match rule.anchor {
        Anchor::Start => match_parts_from(hay, &rule.parts, 0, rule.right_anchored),
        Anchor::Hostname => hostname_anchor_positions(url)
            .into_iter()
            .any(|p| match_parts_from(hay, &rule.parts, p, rule.right_anchored)),
        Anchor::None => {
            let first = rule.parts[0].as_bytes();
            let mut from = 0usize;
            while let Some((s, _)) = find_part(hay, from, first) {
                if match_parts_from(hay, &rule.parts, s, rule.right_anchored) {
                    return true;
                }
                from = s + 1;
                if from > hay.len() {
                    break;
                }
            }
            false
        }
    }
}

/// Everything the option matcher needs about a request.
#[derive(Debug, Clone, Copy)]
pub struct MatchContext<'a> {
    pub url: &'a ParsedUrl,
    pub source_host: Option<&'a str>,
    pub application_id: Option<&'a str>,
    pub resource_type: ResourceType,
    pub party: Party,
    pub is_popup: bool,
}

/// Do this rule's options permit it to apply to the request?
fn options_match(rule: &NetworkRule, ctx: &MatchContext<'_>) -> bool {
    let o = &rule.options;
    let resource_matches = o.resource_mask & ctx.resource_type.mask() != 0;
    match o.popup {
        Some(true) if !ctx.is_popup && !resource_matches => return false,
        Some(false) if ctx.is_popup || !resource_matches => return false,
        None if !resource_matches => return false,
        _ => {}
    }
    if !o.party.accepts(ctx.party) {
        return false;
    }
    if !o.domains.is_empty() && !o.domains.matches_host(ctx.source_host) {
        return false;
    }
    if !o.apps.is_empty() && !o.apps.matches_exact(ctx.application_id) {
        return false;
    }
    if !o.denyallow.is_empty()
        && o.denyallow
            .iter()
            .any(|d| crate::url::host_matches_domain(&ctx.url.host, d))
    {
        return false;
    }
    true
}

// ---------------------------------------------------------------------------
// Index
// ---------------------------------------------------------------------------

/// An indexed set of network rules of one polarity (blocking or exception).
#[derive(Debug, Default)]
pub struct NetworkIndex {
    rules: Vec<NetworkRule>,
    hostname: HashMap<String, Vec<u32>>,
    tokens: HashMap<u64, Vec<u32>>,
    unindexed: Vec<u32>,
    regexes: HashMap<u32, Regex>,
}

impl NetworkIndex {
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn rules(&self) -> &[NetworkRule] {
        &self.rules
    }

    /// Build an index. Rules whose regular expression will not compile are
    /// dropped and reported, rather than being silently kept and never matched.
    pub fn build(rules: Vec<NetworkRule>) -> (Self, Vec<String>) {
        let mut idx = NetworkIndex {
            rules: Vec::with_capacity(rules.len()),
            ..Default::default()
        };
        let mut dropped = Vec::new();

        for rule in rules {
            let i = idx.rules.len() as u32;
            if let Some(body) = rule.regex.clone() {
                match build_regex(&body) {
                    Some(re) => {
                        idx.regexes.insert(i, re);
                        idx.unindexed.push(i);
                    }
                    None => {
                        dropped.push(rule.id.clone());
                        continue;
                    }
                }
            } else if let Some(host) = hostname_key(&rule) {
                idx.hostname.entry(host).or_default().push(i);
            } else if let Some(tok) = best_token(&rule) {
                idx.tokens.entry(tok).or_default().push(i);
            } else {
                idx.unindexed.push(i);
            }
            idx.rules.push(rule);
        }

        idx.hostname.shrink_to_fit();
        idx.tokens.shrink_to_fit();
        idx.rules.shrink_to_fit();
        (idx, dropped)
    }

    /// Highest-priority matching rule, or `None`. `$important` rules win over
    /// ordinary ones; otherwise the first match encountered is returned.
    pub fn find_match(&self, ctx: &MatchContext<'_>) -> Option<&NetworkRule> {
        let mut fallback: Option<&NetworkRule> = None;

        // 1. Hostname-anchored rules: one lookup per label suffix.
        if !self.hostname.is_empty() {
            let host = ctx.url.host.as_str();
            let mut offset = 0usize;
            loop {
                let suffix = &host[offset..];
                if let Some(bucket) = self.hostname.get(suffix) {
                    if let Some(r) = self.scan(bucket, ctx, &mut fallback) {
                        return Some(r);
                    }
                }
                match suffix.find('.') {
                    Some(dot) => offset += dot + 1,
                    None => break,
                }
            }
        }

        // 2. Token-indexed rules: one lookup per token in the URL.
        if !self.tokens.is_empty() {
            for tok in tokenize(&ctx.url.normalized) {
                if let Some(bucket) = self.tokens.get(&tok) {
                    if let Some(r) = self.scan(bucket, ctx, &mut fallback) {
                        return Some(r);
                    }
                }
            }
        }

        // 3. The small unindexed remainder.
        if let Some(r) = self.scan(&self.unindexed, ctx, &mut fallback) {
            return Some(r);
        }

        fallback
    }

    /// Test one bucket of candidate rules. Returns early only for `$important`
    /// matches; the first ordinary match is remembered in `fallback`.
    fn scan<'a>(
        &'a self,
        bucket: &[u32],
        ctx: &MatchContext<'_>,
        fallback: &mut Option<&'a NetworkRule>,
    ) -> Option<&'a NetworkRule> {
        for &i in bucket {
            let rule = &self.rules[i as usize];
            if !options_match(rule, ctx) {
                continue;
            }
            if !pattern_matches(rule, ctx.url, self.regexes.get(&i)) {
                continue;
            }
            if rule.options.important {
                return Some(rule);
            }
            if fallback.is_none() {
                *fallback = Some(rule);
            }
        }
        None
    }
}

/// The hostname bucket key for a rule, if it can use one. Rules anchored to a
/// partial label (`||ads.`) cannot, because the key would not be a real host.
fn hostname_key(rule: &NetworkRule) -> Option<String> {
    if rule.anchor != Anchor::Hostname {
        return None;
    }
    let host = rule.host_anchor.as_ref()?;
    if host.is_empty() || host.ends_with('.') || !host.contains('.') {
        return None;
    }
    // The pattern must continue at a boundary, otherwise `||exam` would be
    // filed under a host it does not actually anchor to.
    let first = rule.parts.first()?;
    let rest = first.strip_prefix(host.as_str())?;
    match rest.as_bytes().first() {
        None => Some(host.clone()),
        Some(&b) if b == b'^' || b == b'/' || b == b':' => Some(host.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separator_semantics() {
        assert!(is_separator(b'/'));
        assert!(is_separator(b'?'));
        assert!(!is_separator(b'a'));
        assert!(!is_separator(b'-'));
        assert!(!is_separator(b'.'));
    }

    #[test]
    fn trailing_caret_matches_end_of_url() {
        let hay = b"https://ads.example.com";
        assert_eq!(part_match_at(hay, 8, b"ads.example.com^"), Some(23));
    }

    #[test]
    fn tokens_ignore_short_runs() {
        let t = tokenize("https://a.io/ab/analytics.js");
        assert!(!t.is_empty());
        assert!(t.contains(&hash_token(b"analytics")));
        assert!(!t.contains(&hash_token(b"ab")));
    }
}
