//! The decision pipeline described in `docs/architecture.md` §4, and the
//! `Engine` that owns every index a decision needs.

pub mod cosmetic;
pub mod database;
pub mod dnr;
pub mod rules;

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::matcher::{MatchContext, NetworkIndex};
use crate::parser::{parse_line, ListFormat, ParsedLine, RejectReason};
use crate::types::{FilterDecision, FilterResult, Party, RequestContext, ResourceType};
use crate::url::{self, ParsedUrl};
use cosmetic::CosmeticIndex;
use database::{RuleDatabase, SourceInfo, DATABASE_FORMAT_VERSION};
use rules::{CosmeticRule, ExceptionScope, NetworkRule, RemoveParam};

/// What RatBlocker does with traffic from a given application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationPolicy {
    /// Apply the full pipeline.
    Filter,
    /// Pass everything through untouched (the app is excluded from filtering).
    Bypass,
}

/// Runtime configuration the engine consults on every request.
///
/// `Default` is hand-written: deriving it would silently produce
/// `enabled: false`, because serde's `default =` attribute only applies when
/// deserializing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    /// Domains the user has allowlisted. Matched against the *source* host for
    /// subresources and the request host for top-level navigations.
    #[serde(default)]
    pub allowlisted_domains: HashSet<String>,
    #[serde(default)]
    pub application_policies: HashMap<String, ApplicationPolicy>,
    /// Master switch. When false the engine allows everything.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            allowlisted_domains: HashSet::new(),
            application_policies: HashMap::new(),
            enabled: true,
        }
    }
}

/// A line that could not be turned into a rule, with enough context to report.
#[derive(Debug, Clone)]
pub struct RejectedLine {
    pub list_id: String,
    pub line_number: usize,
    pub reason: RejectReason,
    pub raw: String,
}

/// Counts produced while loading rules.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadStats {
    pub network_rules: usize,
    pub exception_rules: usize,
    pub removeparam_rules: usize,
    pub cosmetic_rules: usize,
    pub rejected: usize,
    pub badfilter_applied: usize,
}

/// Builds an `Engine` from one or more filter lists.
#[derive(Debug, Default)]
pub struct EngineBuilder {
    block: Vec<NetworkRule>,
    allow: Vec<NetworkRule>,
    removeparam: Vec<NetworkRule>,
    cosmetic: Vec<CosmeticRule>,
    user_block: Vec<NetworkRule>,
    user_allow: Vec<NetworkRule>,
    badfilters: Vec<NetworkRule>,
    rejected: Vec<RejectedLine>,
    stats: LoadStats,
    sources: Vec<SourceInfo>,
}

impl EngineBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add every line of a subscription list, recording its declared metadata
    /// and licence so attribution survives compilation (§20).
    pub fn add_list(&mut self, list_id: &str, content: &str, format: ListFormat) {
        let before = self.rule_total();
        let mut info = SourceInfo {
            id: list_id.to_string(),
            ..Default::default()
        };
        for (n, line) in content.lines().enumerate() {
            let id = format!("{list_id}:{}", n + 1);
            let parsed = parse_line(line, format, &id);
            if let ParsedLine::Metadata { key, value } = &parsed {
                match key.as_str() {
                    "title" => info.title = Some(value.clone()),
                    "version" => info.version = Some(value.clone()),
                    "homepage" => info.homepage = Some(value.clone()),
                    "licence" | "license" => info.license = Some(value.clone()),
                    _ => {}
                }
            }
            self.ingest(parsed, list_id, n + 1, false);
        }
        info.rule_count = self.rule_total() - before;
        self.sources.push(info);
    }

    /// Attach the URL and checksum the list was fetched with.
    pub fn set_source_provenance(
        &mut self,
        list_id: &str,
        url: Option<String>,
        checksum: Option<String>,
    ) {
        if let Some(s) = self.sources.iter_mut().find(|s| s.id == list_id) {
            s.url = url;
            s.checksum = checksum;
        }
    }

    fn rule_total(&self) -> usize {
        self.block.len() + self.allow.len() + self.removeparam.len() + self.cosmetic.len()
    }

    /// Add rules the user wrote themselves. These take precedence over
    /// subscriptions and are never cancelled by `$badfilter`.
    pub fn add_user_rules(&mut self, content: &str) {
        for (n, line) in content.lines().enumerate() {
            let id = format!("user:{}", n + 1);
            self.ingest(parse_line(line, ListFormat::Adblock, &id), "user", n + 1, true);
        }
    }

    fn ingest(&mut self, parsed: ParsedLine, list_id: &str, line_number: usize, user: bool) {
        match parsed {
            ParsedLine::Empty | ParsedLine::Comment | ParsedLine::Metadata { .. } => {}
            ParsedLine::Network(rule) => {
                let rule = *rule;
                if rule.is_exception {
                    self.stats.exception_rules += 1;
                    if user {
                        self.user_allow.push(rule);
                    } else {
                        self.allow.push(rule);
                    }
                } else if rule.options.removeparam.is_some() {
                    self.stats.removeparam_rules += 1;
                    self.removeparam.push(rule);
                } else {
                    self.stats.network_rules += 1;
                    if user {
                        self.user_block.push(rule);
                    } else {
                        self.block.push(rule);
                    }
                }
            }
            ParsedLine::Cosmetic(rule) => {
                self.stats.cosmetic_rules += 1;
                self.cosmetic.push(*rule);
            }
            ParsedLine::BadFilter(rule) => self.badfilters.push(*rule),
            ParsedLine::Rejected { reason, raw } => {
                self.stats.rejected += 1;
                self.rejected.push(RejectedLine {
                    list_id: list_id.to_string(),
                    line_number,
                    reason,
                    raw,
                });
            }
        }
    }

    pub fn rejected(&self) -> &[RejectedLine] {
        &self.rejected
    }

    pub fn stats(&self) -> &LoadStats {
        &self.stats
    }

    /// Emit the serializable database. User rules are deliberately excluded:
    /// they live in the configuration, not in a compiled subscription database.
    pub fn into_database(self) -> RuleDatabase {
        self.split().0
    }

    /// Build an engine directly from everything added so far.
    pub fn build(self, config: EngineConfig) -> Engine {
        let (db, user_block, user_allow) = self.split();
        Engine::assemble(db, user_block, user_allow, config)
    }

    /// Same as `split`, exposed within the crate for the DNR compiler.
    pub(crate) fn split_for_dnr(self) -> (RuleDatabase, Vec<NetworkRule>, Vec<NetworkRule>) {
        self.split()
    }

    /// Apply `$badfilter` cancellations and separate subscription rules from
    /// user rules.
    fn split(mut self) -> (RuleDatabase, Vec<NetworkRule>, Vec<NetworkRule>) {
        if !self.badfilters.is_empty() {
            let cancelled: HashSet<String> =
                self.badfilters.iter().map(|r| badfilter_key(r)).collect();
            let before = self.block.len() + self.allow.len();
            self.block.retain(|r| !cancelled.contains(&badfilter_key(r)));
            self.allow.retain(|r| !cancelled.contains(&badfilter_key(r)));
            self.stats.badfilter_applied = before - (self.block.len() + self.allow.len());
        }
        let db = RuleDatabase {
            format_version: DATABASE_FORMAT_VERSION,
            sources: self.sources,
            network: self.block,
            exceptions: self.allow,
            removeparam: self.removeparam,
            cosmetic: self.cosmetic,
            stats: self.stats,
        };
        (db, self.user_block, self.user_allow)
    }
}

/// `$badfilter` matches a rule with the identical text minus the option itself.
fn badfilter_key(rule: &NetworkRule) -> String {
    rule.raw
        .replace(",badfilter", "")
        .replace("badfilter,", "")
        .replace("$badfilter", "")
        .trim_end_matches('$')
        .to_string()
}

/// The compiled filtering engine.
#[derive(Debug)]
pub struct Engine {
    block: NetworkIndex,
    allow: NetworkIndex,
    removeparam: NetworkIndex,
    user_block: NetworkIndex,
    user_allow: NetworkIndex,
    cosmetic: CosmeticIndex,
    config: EngineConfig,
    load_stats: LoadStats,
    sources: Vec<SourceInfo>,
    dropped_rules: Vec<String>,
    dns_enforceable: usize,
}

impl Engine {
    /// Build an engine from a compiled database plus the user's own rules.
    pub fn from_database(
        db: RuleDatabase,
        user_rules: &str,
        config: EngineConfig,
    ) -> Result<Self, database::DatabaseError> {
        db.check_version()?;
        let mut b = EngineBuilder::new();
        b.add_user_rules(user_rules);
        let (_, user_block, user_allow) = b.split();
        Ok(Engine::assemble(db, user_block, user_allow, config))
    }

    fn assemble(
        db: RuleDatabase,
        user_block: Vec<NetworkRule>,
        user_allow: Vec<NetworkRule>,
        config: EngineConfig,
    ) -> Self {
        let stats = db.stats.clone();
        let sources = db.sources.clone();
        // A DNS-only layer sees a hostname and nothing else, so rules carrying
        // a resource type, a party constraint or a `$domain=` scope cannot fire
        // there. Counting them up front lets the UI be honest about how much of
        // a list system-wide filtering actually covers (§25).
        let dns_enforceable = db.network.iter().filter(|r| r.is_dns_enforceable()).count();
        let (block, mut dropped) = NetworkIndex::build(db.network);
        let (allow, d2) = NetworkIndex::build(db.exceptions);
        let (removeparam, d3) = NetworkIndex::build(db.removeparam);
        let (user_block, d4) = NetworkIndex::build(user_block);
        let (user_allow, d5) = NetworkIndex::build(user_allow);
        dropped.extend(d2);
        dropped.extend(d3);
        dropped.extend(d4);
        dropped.extend(d5);
        Engine {
            block,
            allow,
            removeparam,
            user_block,
            user_allow,
            cosmetic: CosmeticIndex::build(db.cosmetic),
            config,
            load_stats: stats,
            sources,
            dropped_rules: dropped,
            dns_enforceable,
        }
    }

    /// How many loaded rules can be enforced from a hostname alone.
    pub fn dns_enforceable_count(&self) -> usize {
        self.dns_enforceable
    }

    /// Every indexed network rule, for diagnostics. Not on the hot path.
    pub fn all_rules(&self) -> impl Iterator<Item = &NetworkRule> {
        self.user_block
            .rules()
            .iter()
            .chain(self.user_allow.rules())
            .chain(self.block.rules())
            .chain(self.allow.rules())
            .chain(self.removeparam.rules())
    }

    /// Provenance and licensing of every compiled list.
    pub fn sources(&self) -> &[SourceInfo] {
        &self.sources
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    pub fn config_mut(&mut self) -> &mut EngineConfig {
        &mut self.config
    }

    pub fn load_stats(&self) -> &LoadStats {
        &self.load_stats
    }

    /// Rules discarded at index time because their regex would not compile.
    pub fn dropped_rules(&self) -> &[String] {
        &self.dropped_rules
    }

    pub fn cosmetic(&self) -> &CosmeticIndex {
        &self.cosmetic
    }

    /// Total indexed network rules across every index.
    pub fn rule_count(&self) -> usize {
        self.block.len()
            + self.allow.len()
            + self.removeparam.len()
            + self.user_block.len()
            + self.user_allow.len()
    }

    /// The full pipeline from `docs/architecture.md` §4.
    pub fn evaluate(&self, ctx: &RequestContext) -> FilterResult {
        // 1. Validate and normalize the URL. An unparseable or non-filterable
        //    URL is allowed: refusing to decide is safer than guessing.
        let Ok(url) = url::parse(&ctx.request_url) else {
            return FilterResult::allow();
        };

        if !self.config.enabled {
            return FilterResult::allow();
        }

        let source = ctx.source_url.as_deref().and_then(|s| url::parse(s).ok());
        let source_host = source.as_ref().map(|s| s.host.as_str());

        // For a top-level navigation the document *is* the site, so allowlist
        // checks must consider the request's own host.
        let allowlist_host = match (source_host, ctx.resource_type) {
            (Some(h), _) => Some(h),
            (None, _) => Some(url.host.as_str()),
        };

        // 2. User allowlist.
        if let Some(h) = allowlist_host {
            if self.is_allowlisted(h) {
                return FilterResult::allowed_by("allowlist");
            }
        }

        // 3. Application policy.
        if let Some(app) = ctx.application_id.as_deref() {
            if self.config.application_policies.get(app) == Some(&ApplicationPolicy::Bypass) {
                return FilterResult::allowed_by(format!("app-policy:{app}"));
            }
        }

        let party = url::party_of(&url.host, source_host);
        let mctx = MatchContext {
            url: &url,
            source_host,
            application_id: ctx.application_id.as_deref(),
            resource_type: ctx.resource_type,
            party,
        };

        // 4. Explicit user rules, in both directions, before any subscription.
        if let Some(r) = self.user_allow.find_match(&mctx) {
            return FilterResult::allowed_by(r.id.clone());
        }
        if let Some(r) = self.user_block.find_match(&mctx) {
            return self.decision_for(r, &url);
        }

        // 5. Subscription blocklists.
        let blocked = self.block.find_match(&mctx);

        // 6. Exception rules. `$important` blocks outrank them.
        let excepted = self.allow.find_match(&mctx);
        if let Some(b) = blocked {
            let important_block = b.options.important;
            let important_allow = excepted.map(|e| e.options.important).unwrap_or(false);
            match excepted {
                Some(e) if !important_block || important_allow => {
                    return FilterResult::allowed_by(e.id.clone())
                }
                _ => return self.decision_for(b, &url),
            }
        }
        if let Some(e) = excepted {
            // Nothing blocked, but record that an exception covered the request
            // so diagnostics can explain why.
            return FilterResult::allowed_by(e.id.clone());
        }

        // 7. Parameter removal applies only to requests that survived.
        if let Some(r) = self.removeparam.find_match(&mctx) {
            if let Some(result) = apply_removeparam(r, &url, &ctx.request_url) {
                return result;
            }
        }

        FilterResult::allow()
    }

    /// Convenience wrapper for the DNS layer, which only ever sees a hostname.
    pub fn evaluate_host(&self, host: &str, application_id: Option<&str>) -> FilterResult {
        let Ok(host) = url::normalize_host(host) else {
            return FilterResult::allow();
        };
        let mut ctx = RequestContext::new(format!("https://{host}/"), ResourceType::Other);
        ctx.application_id = application_id.map(str::to_string);
        self.evaluate(&ctx)
    }

    fn is_allowlisted(&self, host: &str) -> bool {
        if self.config.allowlisted_domains.is_empty() {
            return false;
        }
        if self.config.allowlisted_domains.contains(host) {
            return true;
        }
        // Walk parent domains so allowlisting `example.com` covers subdomains.
        let mut offset = 0usize;
        while let Some(dot) = host[offset..].find('.') {
            offset += dot + 1;
            if self.config.allowlisted_domains.contains(&host[offset..]) {
                return true;
            }
        }
        false
    }

    fn decision_for(&self, rule: &NetworkRule, url: &ParsedUrl) -> FilterResult {
        if let Some(target) = &rule.options.redirect {
            return FilterResult {
                decision: FilterDecision::Redirect,
                matched_rule_id: Some(rule.id.clone()),
                redirect_to: Some(target.clone()),
                rewritten_url: None,
                removed_parameters: Vec::new(),
            };
        }
        let _ = url;
        FilterResult::blocked_by(rule.id.clone())
    }

    /// Cosmetic selectors for a page, honouring `$elemhide` style exceptions.
    pub fn cosmetic_for(&self, page_url: &str) -> cosmetic::CosmeticResponse {
        let Ok(url) = url::parse(page_url) else {
            return cosmetic::CosmeticResponse::default();
        };
        if !self.config.enabled || self.is_allowlisted(&url.host) {
            return cosmetic::CosmeticResponse::default();
        }

        // An `@@$document`/`$elemhide` exception switches cosmetic filtering off.
        let doc_ctx = RequestContext::new(page_url, ResourceType::Document);
        let mctx_url = url.clone();
        let mctx = MatchContext {
            url: &mctx_url,
            source_host: Some(&url.host),
            application_id: None,
            resource_type: ResourceType::Document,
            party: Party::First,
        };
        let _ = doc_ctx;
        let mut suppress_generic = false;
        let mut suppress_specific = false;
        for idx in [&self.user_allow, &self.allow] {
            if let Some(e) = idx.find_match(&mctx) {
                let s = e.options.scope;
                if s.has(ExceptionScope::DOCUMENT) || s.has(ExceptionScope::ELEMHIDE) {
                    return cosmetic::CosmeticResponse::default();
                }
                if s.has(ExceptionScope::GENERICHIDE) {
                    suppress_generic = true;
                }
                if s.has(ExceptionScope::SPECIFICHIDE) {
                    suppress_specific = true;
                }
            }
        }
        self.cosmetic
            .selectors_for(&url.host, suppress_generic, suppress_specific)
    }
}

/// Build the rewritten URL for a `$removeparam` match, or `None` when the rule
/// would not actually change anything.
fn apply_removeparam(rule: &NetworkRule, url: &ParsedUrl, original: &str) -> Option<FilterResult> {
    let spec = rule.options.removeparam.as_ref()?;
    if url.query.is_empty() {
        return None;
    }

    let mut kept: Vec<&str> = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    for pair in url.query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let name = pair.split('=').next().unwrap_or(pair);
        let drop = match spec {
            RemoveParam::All => true,
            RemoveParam::Named(names) => names.iter().any(|n| n.eq_ignore_ascii_case(name)),
            RemoveParam::Inverted(names) => !names.iter().any(|n| n.eq_ignore_ascii_case(name)),
        };
        if drop {
            removed.push(name.to_string());
        } else {
            kept.push(pair);
        }
    }
    if removed.is_empty() {
        return None;
    }

    // Rebuild from the original URL so casing and encoding are preserved.
    let base = original.split('#').next().unwrap_or(original);
    let fragment = &original[base.len()..];
    let path_part = base.split('?').next().unwrap_or(base);
    let rewritten = if kept.is_empty() {
        format!("{path_part}{fragment}")
    } else {
        format!("{path_part}?{}{fragment}", kept.join("&"))
    };

    Some(FilterResult {
        decision: FilterDecision::RemoveParameters,
        matched_rule_id: Some(rule.id.clone()),
        redirect_to: None,
        rewritten_url: Some(rewritten),
        removed_parameters: removed,
    })
}
