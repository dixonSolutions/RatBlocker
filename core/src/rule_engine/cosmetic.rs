//! Cosmetic (element-hiding) rule storage and lookup.
//!
//! Browser layers only: the system layers cannot hide page elements, which is
//! one of the documented limitations in `docs/architecture.md` §25.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::rules::CosmeticRule;
use crate::url::domain_pattern_matches;

/// Selectors to apply to one page.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CosmeticResponse {
    /// Selectors whose matching elements should be hidden.
    pub hide: Vec<String>,
}

impl CosmeticResponse {
    pub fn is_empty(&self) -> bool {
        self.hide.is_empty()
    }

    /// A single stylesheet applying every selector at once. Injecting one rule
    /// is dramatically cheaper than one rule per selector.
    pub fn to_stylesheet(&self) -> String {
        if self.hide.is_empty() {
            return String::new();
        }
        format!(
            "{} {{ display: none !important; }}",
            self.hide.join(",\n")
        )
    }
}

/// Cosmetic rules, split so a page lookup never scans site-specific rules that
/// belong to other sites.
#[derive(Debug, Default)]
pub struct CosmeticIndex {
    /// Selectors that apply to every site.
    generic: Vec<String>,
    /// Site-specific rules, keyed by each included domain.
    specific: HashMap<String, Vec<usize>>,
    /// Backing store for the site-specific rules.
    rules: Vec<CosmeticRule>,
    /// Rules whose domains use a `.*` wildcard suffix; they cannot be keyed by
    /// an exact host, so they are scanned linearly. EasyList has a few hundred.
    wildcard: Vec<usize>,
    /// Unhide rules keyed by domain, plus generic unhides.
    exceptions: HashMap<String, Vec<String>>,
    wildcard_exceptions: Vec<(String, String)>,
    generic_exceptions: Vec<String>,
}

impl CosmeticIndex {
    pub fn generic_count(&self) -> usize {
        self.generic.len()
    }

    pub fn specific_count(&self) -> usize {
        self.rules.len()
    }

    pub fn build(rules: Vec<CosmeticRule>) -> Self {
        let mut idx = CosmeticIndex::default();
        for rule in rules {
            if rule.is_exception {
                if rule.included_domains.is_empty() {
                    idx.generic_exceptions.push(rule.selector);
                } else {
                    for d in rule.included_domains {
                        if d.ends_with(".*") {
                            idx.wildcard_exceptions.push((d, rule.selector.clone()));
                        } else {
                            idx.exceptions
                                .entry(d)
                                .or_default()
                                .push(rule.selector.clone());
                        }
                    }
                }
                continue;
            }
            if rule.is_generic() && rule.excluded_domains.is_empty() {
                idx.generic.push(rule.selector);
            } else if rule.is_generic() {
                // Generic with exclusions: keep it in the specific store so the
                // exclusion list is honoured at lookup time.
                let i = idx.rules.len();
                idx.rules.push(rule);
                idx.specific.entry(String::new()).or_default().push(i);
            } else {
                let i = idx.rules.len();
                let mut wildcarded = false;
                for d in rule.included_domains.clone() {
                    if d.ends_with(".*") {
                        wildcarded = true;
                    } else {
                        idx.specific.entry(d).or_default().push(i);
                    }
                }
                if wildcarded {
                    idx.wildcard.push(i);
                }
                idx.rules.push(rule);
            }
        }
        idx.generic.sort_unstable();
        idx.generic.dedup();
        idx
    }

    /// Selectors for `host`, honouring generic/specific suppression.
    pub fn selectors_for(
        &self,
        host: &str,
        suppress_generic: bool,
        suppress_specific: bool,
    ) -> CosmeticResponse {
        let mut unhide: Vec<&str> = self.generic_exceptions.iter().map(String::as_str).collect();
        for domain in domain_chain(host) {
            if let Some(sels) = self.exceptions.get(domain) {
                unhide.extend(sels.iter().map(String::as_str));
            }
        }
        for (pattern, sel) in &self.wildcard_exceptions {
            if domain_pattern_matches(host, pattern) {
                unhide.push(sel.as_str());
            }
        }

        let mut hide: Vec<String> = Vec::new();

        if !suppress_generic {
            // Generic rules carrying exclusions live in `specific[""]`.
            if let Some(bucket) = self.specific.get("") {
                for &i in bucket {
                    let r = &self.rules[i];
                    if !r.excluded_domains.iter().any(|d| domain_pattern_matches(host, d)) {
                        hide.push(r.selector.clone());
                    }
                }
            }
            hide.extend(self.generic.iter().cloned());
        }

        if !suppress_specific {
            for &i in &self.wildcard {
                let r = &self.rules[i];
                if r.excluded_domains.iter().any(|d| domain_pattern_matches(host, d)) {
                    continue;
                }
                if r
                    .included_domains
                    .iter()
                    .any(|d| domain_pattern_matches(host, d))
                {
                    hide.push(r.selector.clone());
                }
            }
            for domain in domain_chain(host) {
                if let Some(bucket) = self.specific.get(domain) {
                    for &i in bucket {
                        let r = &self.rules[i];
                        if r.excluded_domains.iter().any(|d| domain_pattern_matches(host, d)) {
                            continue;
                        }
                        hide.push(r.selector.clone());
                    }
                }
            }
        }

        if !unhide.is_empty() {
            hide.retain(|s| !unhide.iter().any(|u| u == s));
        }
        hide.sort_unstable();
        hide.dedup();
        CosmeticResponse { hide }
    }
}

/// `a.b.example.com` -> `a.b.example.com`, `b.example.com`, `example.com`, `com`.
fn domain_chain(host: &str) -> impl Iterator<Item = &str> {
    std::iter::successors(Some(host), |h| {
        h.find('.').map(|dot| &h[dot + 1..]).filter(|s| !s.is_empty())
    })
}
