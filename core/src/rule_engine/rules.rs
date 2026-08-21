//! Normalized rule representation produced by the parser and consumed by the
//! matcher. Nothing here knows about EasyList syntax; that lives in `parser`.

use serde::{Deserialize, Serialize};

use crate::types::{Party, ResourceType};
use crate::url::domain_pattern_matches;

/// Where a pattern is allowed to start matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    /// `foo` — may match anywhere in the URL.
    None,
    /// `|http://foo` — must match at the very start of the URL.
    Start,
    /// `||example.com^` — must match at a hostname boundary.
    Hostname,
}

/// Which party a rule applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartyConstraint {
    Any,
    FirstOnly,
    ThirdOnly,
}

impl PartyConstraint {
    pub fn accepts(self, party: Party) -> bool {
        match (self, party) {
            (PartyConstraint::Any, _) => true,
            // With no source URL we cannot prove a party constraint holds, so a
            // constrained rule does not apply. This keeps DNS-layer filtering
            // from over-blocking on `$third-party` rules.
            (_, Party::Unknown) => false,
            (PartyConstraint::FirstOnly, Party::First) => true,
            (PartyConstraint::ThirdOnly, Party::Third) => true,
            _ => false,
        }
    }
}

/// `$domain=a.com|~b.com` style include/exclude list. Also used for `$app=`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainConstraint {
    #[serde(default)]
    pub included: Vec<String>,
    #[serde(default)]
    pub excluded: Vec<String>,
}

impl DomainConstraint {
    pub fn is_empty(&self) -> bool {
        self.included.is_empty() && self.excluded.is_empty()
    }

    /// Subdomain-aware match against a host (or an exact match for app ids).
    pub fn matches_host(&self, host: Option<&str>) -> bool {
        let Some(host) = host else {
            // An include list cannot be satisfied without a host.
            return self.included.is_empty();
        };
        if self
            .excluded
            .iter()
            .any(|d| domain_pattern_matches(host, d))
        {
            return false;
        }
        if self.included.is_empty() {
            return true;
        }
        self.included.iter().any(|d| domain_pattern_matches(host, d))
    }

    /// Exact match, used for application identifiers which are not hierarchical.
    pub fn matches_exact(&self, value: Option<&str>) -> bool {
        let Some(value) = value else {
            return self.included.is_empty();
        };
        if self.excluded.iter().any(|d| d == value) {
            return false;
        }
        if self.included.is_empty() {
            return true;
        }
        self.included.iter().any(|d| d == value)
    }
}

/// Parameter-removal payload for `$removeparam`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoveParam {
    /// `$removeparam` with no value: strip the entire query string.
    All,
    /// `$removeparam=utm_source`
    Named(Vec<String>),
    /// `$removeparam=~keep` — strip everything except these.
    Inverted(Vec<String>),
}


/// Scope modifiers that only make sense on exception (`@@`) rules.
///
/// `@@||example.com^$document` allowlists the whole document; `$elemhide` and
/// friends only switch off cosmetic filtering. Stored as a bitfield so a single
/// rule can carry several.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ExceptionScope(pub u8);

impl ExceptionScope {
    /// `$document` — suppress all filtering for the document.
    pub const DOCUMENT: u8 = 1 << 0;
    /// `$elemhide` — suppress all cosmetic rules.
    pub const ELEMHIDE: u8 = 1 << 1;
    /// `$generichide` — suppress generic cosmetic rules only.
    pub const GENERICHIDE: u8 = 1 << 2;
    /// `$specifichide` — suppress site-specific cosmetic rules only.
    pub const SPECIFICHIDE: u8 = 1 << 3;
    /// `$genericblock` — suppress generic network rules.
    pub const GENERICBLOCK: u8 = 1 << 4;

    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }
    pub fn has(&self, bit: u8) -> bool {
        self.0 & bit != 0
    }
    pub fn insert(&mut self, bit: u8) {
        self.0 |= bit;
    }
}

/// Options attached to a network rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleOptions {
    /// Bitmask of `ResourceType`s this rule applies to.
    pub resource_mask: u32,
    pub party: PartyConstraint,
    /// `Some(true)` for `$popup`, `Some(false)` for `$~popup`.
    #[serde(default)]
    pub popup: Option<bool>,
    #[serde(default)]
    pub domains: DomainConstraint,
    #[serde(default)]
    pub apps: DomainConstraint,
    /// `$denyallow=` — hosts this rule explicitly must not block.
    #[serde(default)]
    pub denyallow: Vec<String>,
    pub match_case: bool,
    /// `$important` outranks exception rules.
    pub important: bool,
    #[serde(default)]
    pub removeparam: Option<RemoveParam>,
    #[serde(default)]
    pub redirect: Option<String>,
    /// Only meaningful on exception rules.
    #[serde(default)]
    pub scope: ExceptionScope,
}

impl Default for RuleOptions {
    fn default() -> Self {
        Self {
            resource_mask: ResourceType::ALL,
            party: PartyConstraint::Any,
            popup: None,
            domains: DomainConstraint::default(),
            apps: DomainConstraint::default(),
            denyallow: Vec::new(),
            match_case: false,
            important: false,
            removeparam: None,
            redirect: None,
            scope: ExceptionScope::default(),
        }
    }
}

/// A parsed network filter rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkRule {
    /// Stable identifier: `<list-id>:<line-number>`.
    pub id: String,
    /// The original rule text, kept for diagnostics and for `$badfilter`.
    pub raw: String,
    pub anchor: Anchor,
    /// For `Anchor::Hostname`, the literal host prefix the pattern begins with.
    /// Lets the matcher file the rule in the hostname trie.
    #[serde(default)]
    pub host_anchor: Option<String>,
    /// Pattern split on `*`; every element must be found in order.
    pub parts: Vec<String>,
    /// `...|` — the pattern must end at the end of the URL.
    pub right_anchored: bool,
    /// Set for `/regex/` rules, which bypass `parts` entirely.
    #[serde(default)]
    pub regex: Option<String>,
    pub is_exception: bool,
    pub options: RuleOptions,
}

impl NetworkRule {
    /// True when the rule is a bare hostname block with no options that would
    /// need a URL — i.e. it can be enforced at the DNS layer too.
    pub fn is_dns_enforceable(&self) -> bool {
        !self.is_exception
            && self.regex.is_none()
            && self.options.resource_mask == ResourceType::ALL
            && self.options.party == PartyConstraint::Any
            && self.options.popup.is_none()
            && self.options.domains.is_empty()
            && self.options.removeparam.is_none()
            && self.options.redirect.is_none()
            && self.options.denyallow.is_empty()
            && matches!(self.anchor, Anchor::Hostname)
            && self.host_anchor.is_some()
            // `||example.com^` and `||example.com` cover the whole host; a rule
            // with a path tail does not.
            && self
                .parts
                .iter()
                .map(|p| p.as_str())
                .collect::<String>()
                .trim_start_matches(self.host_anchor.as_deref().unwrap_or(""))
                .chars()
                .all(|c| c == '^')
    }
}

/// Cosmetic (element hiding) rule. Browser layers only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CosmeticRule {
    pub id: String,
    /// Empty means generic: applies to every site.
    #[serde(default)]
    pub included_domains: Vec<String>,
    #[serde(default)]
    pub excluded_domains: Vec<String>,
    /// A CSS selector.
    pub selector: String,
    /// `#@#` — unhide.
    pub is_exception: bool,
}

impl CosmeticRule {
    pub fn is_generic(&self) -> bool {
        self.included_domains.is_empty()
    }
}
