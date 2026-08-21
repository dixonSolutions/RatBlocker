//! URL validation, normalization and first/third-party determination.
//!
//! Deliberately dependency-free so the core stays small enough to compile to
//! WebAssembly. Hosts are expected to arrive already punycoded (every platform
//! API RatBlocker consumes does this); non-ASCII hosts are lowercased and used
//! as-is rather than rejected.

use std::collections::HashSet;
use std::sync::OnceLock;

use crate::types::Party;

/// A URL split into the pieces the matcher cares about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUrl {
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
    pub path: String,
    pub query: String,
    /// Lowercased `scheme://host[:port]/path?query`, fragment removed. This is
    /// the string every network pattern is matched against.
    pub normalized: String,
}

impl ParsedUrl {
    /// Host plus port, as it appears in the normalized URL.
    pub fn authority(&self) -> String {
        match self.port {
            Some(p) => format!("{}:{}", self.host, p),
            None => self.host.clone(),
        }
    }
}

/// Reasons a URL cannot be filtered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UrlError {
    #[error("URL has no scheme separator")]
    MissingScheme,
    #[error("URL scheme is not filterable")]
    UnsupportedScheme,
    #[error("URL has an empty host")]
    EmptyHost,
    #[error("URL host contains an invalid character")]
    InvalidHost,
    #[error("URL exceeds the maximum supported length")]
    TooLong,
}

/// Hard cap so a hostile filter list or page cannot force unbounded work.
pub const MAX_URL_LEN: usize = 8 * 1024;

/// Schemes RatBlocker will make decisions about.
fn is_filterable_scheme(scheme: &str) -> bool {
    matches!(
        scheme,
        "http" | "https" | "ws" | "wss" | "ftp" | "stun" | "turn"
    )
}

/// Parse and normalize a URL for matching.
pub fn parse(raw: &str) -> Result<ParsedUrl, UrlError> {
    if raw.len() > MAX_URL_LEN {
        return Err(UrlError::TooLong);
    }
    let raw = raw.trim();

    let (scheme, rest) = raw.split_once("://").ok_or(UrlError::MissingScheme)?;
    let scheme = scheme.to_ascii_lowercase();
    if !is_filterable_scheme(&scheme) {
        return Err(UrlError::UnsupportedScheme);
    }

    // Strip the fragment first: it is never sent to the server.
    let rest = rest.split('#').next().unwrap_or(rest);

    // Authority ends at the first '/', '?' or end of string.
    let auth_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(auth_end);

    // Drop any userinfo; it is not part of the filtering surface.
    let authority = authority.rsplit('@').next().unwrap_or(authority);

    let (host_part, port) = split_host_port(authority)?;
    let host = host_part.to_ascii_lowercase();
    if host.is_empty() {
        return Err(UrlError::EmptyHost);
    }
    if host
        .bytes()
        .any(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\\' | b'"' | b'<' | b'>'))
    {
        return Err(UrlError::InvalidHost);
    }

    let (path, query) = match tail.split_once('?') {
        Some((p, q)) => (p, q),
        None => (tail, ""),
    };
    let path = if path.is_empty() { "/" } else { path };

    let mut normalized = String::with_capacity(raw.len());
    normalized.push_str(&scheme);
    normalized.push_str("://");
    normalized.push_str(&host);
    if let Some(p) = port {
        if !is_default_port(&scheme, p) {
            normalized.push(':');
            normalized.push_str(&p.to_string());
        }
    }
    normalized.push_str(&path.to_ascii_lowercase());
    if !query.is_empty() {
        normalized.push('?');
        normalized.push_str(&query.to_ascii_lowercase());
    }

    Ok(ParsedUrl {
        scheme,
        host,
        port,
        path: path.to_string(),
        query: query.to_string(),
        normalized,
    })
}

fn is_default_port(scheme: &str, port: u16) -> bool {
    matches!(
        (scheme, port),
        ("http", 80) | ("https", 443) | ("ws", 80) | ("wss", 443) | ("ftp", 21)
    )
}

fn split_host_port(authority: &str) -> Result<(&str, Option<u16>), UrlError> {
    // IPv6 literals are bracketed and may contain colons.
    if let Some(close) = authority.find(']') {
        if authority.starts_with('[') {
            let host = &authority[..=close];
            let rest = &authority[close + 1..];
            let port = match rest.strip_prefix(':') {
                Some(p) if !p.is_empty() => Some(p.parse().map_err(|_| UrlError::InvalidHost)?),
                Some(_) => None,
                None if rest.is_empty() => None,
                None => return Err(UrlError::InvalidHost),
            };
            return Ok((host, port));
        }
        return Err(UrlError::InvalidHost);
    }
    match authority.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() => {
            Ok((h, Some(p.parse().map_err(|_| UrlError::InvalidHost)?)))
        }
        Some((h, _)) => Ok((h, None)),
        None => Ok((authority, None)),
    }
}

/// Normalize a bare hostname (used by the DNS proxy and hosts-file rules).
pub fn normalize_host(host: &str) -> Result<String, UrlError> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return Err(UrlError::EmptyHost);
    }
    if host.len() > 253 {
        return Err(UrlError::TooLong);
    }
    if host.split('.').any(|label| label.is_empty() || label.len() > 63) {
        return Err(UrlError::InvalidHost);
    }
    if host.bytes().any(|b| {
        !(b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_') || !b.is_ascii())
    }) {
        return Err(UrlError::InvalidHost);
    }
    Ok(host)
}

// ---------------------------------------------------------------------------
// Public Suffix List
// ---------------------------------------------------------------------------

/// Vendored copy of the Public Suffix List (MPL-2.0). See
/// `core/data/public_suffix_list.LICENSE` for provenance and attribution.
const PSL_DATA: &str = include_str!("../data/public_suffix_list.txt");

struct PublicSuffixList {
    exact: HashSet<&'static str>,
    wildcard: HashSet<&'static str>,
    exception: HashSet<&'static str>,
}

fn psl() -> &'static PublicSuffixList {
    static PSL: OnceLock<PublicSuffixList> = OnceLock::new();
    PSL.get_or_init(|| {
        let mut list = PublicSuffixList {
            exact: HashSet::with_capacity(10_500),
            wildcard: HashSet::new(),
            exception: HashSet::new(),
        };
        for line in PSL_DATA.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(body) = line.strip_prefix("!") {
                list.exception.insert(body);
            } else if let Some(body) = line.strip_prefix("*.") {
                list.wildcard.insert(body);
            } else {
                list.exact.insert(line);
            }
        }
        list
    })
}

/// The registrable domain ("eTLD+1") of a host, e.g. `a.b.example.co.uk` ->
/// `example.co.uk`. Returns the host unchanged for IP literals and for hosts
/// that are themselves a public suffix.
pub fn registrable_domain(host: &str) -> &str {
    if host.starts_with('[') || host.parse::<std::net::IpAddr>().is_ok() {
        return host;
    }
    let list = psl();
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return host;
    }

    // Longest matching rule wins; an exception rule wins outright.
    let mut suffix_labels = 1usize;
    for i in 0..labels.len() {
        let candidate = &host[offset_of_label(host, &labels, i)..];
        if list.exception.contains(candidate) {
            // The public suffix is the exception rule minus its first label.
            suffix_labels = labels.len() - i - 1;
            break;
        }
        let len = labels.len() - i;
        if list.exact.contains(candidate) {
            suffix_labels = suffix_labels.max(len);
        }
        if i + 1 < labels.len() {
            let parent = &host[offset_of_label(host, &labels, i + 1)..];
            if list.wildcard.contains(parent) {
                suffix_labels = suffix_labels.max(len);
            }
        }
    }

    let take = (suffix_labels + 1).min(labels.len());
    &host[offset_of_label(host, &labels, labels.len() - take)..]
}

/// Byte offset of label `i` within `host`.
fn offset_of_label(host: &str, labels: &[&str], i: usize) -> usize {
    let mut off = 0;
    for l in &labels[..i] {
        off += l.len() + 1;
    }
    debug_assert!(off <= host.len());
    off
}

/// Compare a request host against the host of the page that caused it.
pub fn party_of(request_host: &str, source_host: Option<&str>) -> Party {
    match source_host {
        None => Party::Unknown,
        Some(src) if src.is_empty() => Party::Unknown,
        Some(src) => {
            if registrable_domain(request_host) == registrable_domain(src) {
                Party::First
            } else {
                Party::Third
            }
        }
    }
}

/// True when `s` is exactly a public suffix (`com`, `co.uk`, `github.io`).
pub fn is_public_suffix(s: &str) -> bool {
    let list = psl();
    if list.exact.contains(s) {
        return true;
    }
    // `*.ck` makes `foo.ck` a public suffix.
    match s.split_once('.') {
        Some((_, parent)) => list.wildcard.contains(parent) && !list.exception.contains(s),
        None => false,
    }
}

/// Validate a domain as it may appear in `$domain=` or a cosmetic rule prefix.
///
/// EasyList allows a trailing `.*` to stand for "any public suffix", as in
/// `amazon.*`, which covers `amazon.com`, `amazon.co.uk` and the rest.
pub fn normalize_domain_pattern(s: &str) -> Result<String, UrlError> {
    match s.strip_suffix(".*") {
        Some(base) => Ok(format!("{}.*", normalize_host(base)?)),
        None => normalize_host(s),
    }
}

/// Match a host against a domain entry, honouring a trailing `.*`.
pub fn domain_pattern_matches(host: &str, pattern: &str) -> bool {
    let Some(base) = pattern.strip_suffix(".*") else {
        return host_matches_domain(host, pattern);
    };
    // Try every label boundary so `www.amazon.co.uk` matches `amazon.*`.
    let mut offset = 0usize;
    loop {
        let candidate = &host[offset..];
        if let Some(rest) = candidate.strip_prefix(base) {
            if let Some(tld) = rest.strip_prefix('.') {
                if !tld.is_empty() && is_public_suffix(tld) {
                    return true;
                }
            }
        }
        match candidate.find('.') {
            Some(dot) => offset += dot + 1,
            None => return false,
        }
    }
}

/// True when `host` is `domain` or a subdomain of it.
pub fn host_matches_domain(host: &str, domain: &str) -> bool {
    if host == domain {
        return true;
    }
    host.len() > domain.len()
        && host.ends_with(domain)
        && host.as_bytes()[host.len() - domain.len() - 1] == b'.'
}
