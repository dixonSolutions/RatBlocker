//! Conversion of RatBlocker rules into Chromium `declarativeNetRequest` rules.
//!
//! Chromium's MV3 API cannot express everything EasyList can, and it enforces
//! hard numeric limits. Both facts are handled here rather than being
//! discovered at extension load time: rules that cannot be represented are
//! reported, and rules that do not fit the budget are dropped in a documented
//! priority order and counted.

use serde::Serialize;

use super::rules::{Anchor, ExceptionScope, NetworkRule, PartyConstraint, RemoveParam};
use super::EngineBuilder;
use crate::parser::ListFormat;
use crate::types::ResourceType;

/// Rules guaranteed to be available across all enabled static rulesets.
/// Chromium's `GUARANTEED_MINIMUM_STATIC_RULES`.
pub const MAX_STATIC_RULES: usize = 30_000;
/// Chromium's `MAX_NUMBER_OF_REGEX_RULES`.
pub const MAX_REGEX_RULES: usize = 1_000;

/// The neutral stand-in resources RatBlocker ships, mapping the token a filter
/// list writes to the file the extension serves.
///
/// This table is the single source of truth: the filter compiler writes these
/// files, the Chromium converter points `extensionPath` at them, and the
/// Firefox adapter resolves them through `runtime.getURL`. A `$redirect` whose
/// token is not listed here is converted to a plain block, so a request never
/// escapes to the network just because a stand-in is missing.
pub const REDIRECT_RESOURCES: &[(&str, &str)] = &[
    ("noopjs", "noop.js"),
    ("noop.js", "noop.js"),
    ("blank-js", "noop.js"),
    ("noopframe", "noop.html"),
    ("noophtml", "noop.html"),
    ("noop.html", "noop.html"),
    ("blank-html", "noop.html"),
    ("noopcss", "noop.css"),
    ("noop.css", "noop.css"),
    ("blank-css", "noop.css"),
    ("noop.txt", "noop.txt"),
    ("blank-text", "noop.txt"),
    ("noop.gif", "noop.gif"),
    ("1x1.gif", "noop.gif"),
    ("blank-gif", "noop.gif"),
    ("noop.mp4", "noop.mp4"),
    ("blank-mp4", "noop.mp4"),
    ("noop.mp3", "noop.mp3"),
    ("blank-mp3", "noop.mp3"),
];

/// The file a redirect token resolves to, if RatBlocker ships one.
pub fn redirect_file(token: &str) -> Option<&'static str> {
    let lower = token.to_ascii_lowercase();
    REDIRECT_RESOURCES
        .iter()
        .find(|(name, _)| *name == token || *name == lower)
        .map(|(_, file)| *file)
}

/// Action priorities. Chromium resolves ties by action type, but being
/// explicit keeps the ordering intentional rather than implied.
pub(crate) mod priority {
    pub const BLOCK: u32 = 1;
    pub const MODIFY: u32 = 2;
    pub const ALLOW: u32 = 3;
    pub const ALLOW_ALL: u32 = 4;
    pub const IMPORTANT_BLOCK: u32 = 5;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DnrRule {
    pub id: u32,
    pub priority: u32,
    pub action: DnrAction,
    pub condition: DnrCondition,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DnrAction {
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect: Option<DnrRedirect>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DnrRedirect {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transform: Option<DnrTransform>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DnrTransform {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_transform: Option<DnrQueryTransform>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DnrQueryTransform {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub remove_params: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DnrCondition {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url_filter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regex_filter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_url_filter_case_sensitive: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub resource_types: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain_type: Option<&'static str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub initiator_domains: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub excluded_initiator_domains: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub request_domains: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub excluded_request_domains: Vec<String>,
}

/// Why a rule could not be represented in DNR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unrepresentable {
    /// `$app=` has no meaning inside a browser.
    ApplicationScoped,
    /// DNR sees a document request but not whether it opened a new tab/window.
    PopupScoped,
    /// `$removeparam=~keep` cannot be expressed; DNR only removes named params.
    InvertedRemoveParam,
    /// Over the regex budget.
    RegexBudget,
    /// Pattern reduced to nothing DNR could match on.
    EmptyPattern,
}

/// Reassemble the Adblock-syntax pattern DNR's `urlFilter` understands.
fn url_filter(rule: &NetworkRule) -> Option<String> {
    if rule.parts.is_empty() {
        return None;
    }
    let mut s = String::new();
    match rule.anchor {
        Anchor::Hostname => s.push_str("||"),
        Anchor::Start => s.push('|'),
        Anchor::None => {}
    }
    s.push_str(&rule.parts.join("*"));
    if rule.right_anchored {
        s.push('|');
    }
    // DNR requires the filter to be ASCII.
    if !s.is_ascii() {
        return None;
    }
    Some(s)
}

fn resource_types(mask: u32) -> Vec<&'static str> {
    if mask == ResourceType::ALL {
        // Omitting resourceTypes means "all types except main_frame" in DNR, so
        // list them explicitly to keep parity with the core engine.
        return vec![
            "main_frame", "sub_frame", "stylesheet", "script", "image", "font", "object",
            "xmlhttprequest", "ping", "csp_report", "media", "websocket", "other",
        ];
    }
    let mut out = Vec::new();
    for (rt, name) in [
        (ResourceType::Document, "main_frame"),
        (ResourceType::Subdocument, "sub_frame"),
        (ResourceType::Stylesheet, "stylesheet"),
        (ResourceType::Script, "script"),
        (ResourceType::Image, "image"),
        (ResourceType::Font, "font"),
        (ResourceType::Object, "object"),
        (ResourceType::XmlHttpRequest, "xmlhttprequest"),
        (ResourceType::Ping, "ping"),
        (ResourceType::CspReport, "csp_report"),
        (ResourceType::Media, "media"),
        (ResourceType::WebSocket, "websocket"),
        (ResourceType::Other, "other"),
    ] {
        if mask & rt.mask() != 0 {
            out.push(name);
        }
    }
    out
}

/// Every resource type, as DNR names them.
pub fn all_resource_types() -> Vec<&'static str> {
    resource_types(ResourceType::ALL)
}

/// Collapse plain domain blocks into a handful of rules.
///
/// `||tracker.example^` with no options is by far the most common shape in
/// EasyList and EasyPrivacy — around 88,000 of them — and DNR's
/// `requestDomains` already matches a domain and its subdomains. Emitting one
/// rule per domain would blow the 30,000-rule budget on its own; emitting one
/// rule per few thousand domains costs almost nothing and keeps every one of
/// them. This is what makes near-complete coverage possible under MV3.
pub fn collapse_domains(domains: &[String], first_id: u32, chunk: usize) -> Vec<DnrRule> {
    let types = all_resource_types();
    domains
        .chunks(chunk.max(1))
        .enumerate()
        .map(|(i, group)| DnrRule {
            id: first_id + i as u32,
            priority: priority::BLOCK,
            action: DnrAction { kind: "block", redirect: None },
            condition: DnrCondition {
                request_domains: group.to_vec(),
                resource_types: types.clone(),
                ..Default::default()
            },
        })
        .collect()
}

/// True when a rule is a bare domain block that `collapse_domains` can absorb.
///
/// `$important` blocks are excluded: they need their own priority, and there
/// are few enough that keeping them separate costs nothing.
pub fn collapsible_domain(rule: &NetworkRule) -> Option<&str> {
    if !rule.is_dns_enforceable() || rule.options.important {
        return None;
    }
    rule.host_anchor.as_deref()
}

/// Convert one rule. `regex_budget` is decremented when a regex rule is used.
pub fn convert(
    rule: &NetworkRule,
    id: u32,
    regex_budget: &mut usize,
) -> Result<DnrRule, Unrepresentable> {
    if !rule.options.apps.is_empty() {
        return Err(Unrepresentable::ApplicationScoped);
    }
    // DNR can enforce the concrete resource-type half of `$popup,type`.
    // Popup-only and negated-popup rules still require runtime context.
    if rule.options.popup == Some(false)
        || (rule.options.popup == Some(true) && rule.options.resource_mask == 0)
    {
        return Err(Unrepresentable::PopupScoped);
    }

    let mut condition = DnrCondition {
        resource_types: resource_types(rule.options.resource_mask),
        domain_type: match rule.options.party {
            PartyConstraint::Any => None,
            PartyConstraint::FirstOnly => Some("firstParty"),
            PartyConstraint::ThirdOnly => Some("thirdParty"),
        },
        initiator_domains: rule.options.domains.included.clone(),
        excluded_initiator_domains: rule.options.domains.excluded.clone(),
        excluded_request_domains: rule.options.denyallow.clone(),
        ..Default::default()
    };

    if let Some(re) = &rule.regex {
        if *regex_budget == 0 {
            return Err(Unrepresentable::RegexBudget);
        }
        *regex_budget -= 1;
        condition.regex_filter = Some(re.clone());
    } else {
        match url_filter(rule) {
            Some(f) => {
                condition.url_filter = Some(f);
                if rule.options.match_case {
                    condition.is_url_filter_case_sensitive = Some(true);
                }
            }
            None => return Err(Unrepresentable::EmptyPattern),
        }
    }

    let (action, priority) = if rule.is_exception {
        // A `$document` exception must suppress filtering for the whole page.
        if rule.options.scope.has(ExceptionScope::DOCUMENT) {
            condition.resource_types = vec!["main_frame", "sub_frame"];
            (
                DnrAction { kind: "allowAllRequests", redirect: None },
                priority::ALLOW_ALL,
            )
        } else {
            (DnrAction { kind: "allow", redirect: None }, priority::ALLOW)
        }
    } else if let Some(spec) = &rule.options.removeparam {
        let remove_params = match spec {
            RemoveParam::Named(names) => names.clone(),
            // DNR has no "remove everything except" transform.
            RemoveParam::Inverted(_) => return Err(Unrepresentable::InvertedRemoveParam),
            RemoveParam::All => Vec::new(),
        };
        (
            DnrAction {
                kind: "redirect",
                redirect: Some(DnrRedirect {
                    extension_path: None,
                    transform: Some(DnrTransform {
                        query_transform: Some(DnrQueryTransform { remove_params }),
                    }),
                }),
            },
            priority::MODIFY,
        )
    } else if let Some(target) = &rule.options.redirect {
        match redirect_file(target) {
            Some(file) => (
                DnrAction {
                    kind: "redirect",
                    redirect: Some(DnrRedirect {
                        extension_path: Some(format!("/redirects/{}", sanitize_resource(file))),
                        transform: None,
                    }),
                },
                priority::MODIFY,
            ),
            // No stand-in for this token: block instead of redirecting to a
            // path that would 404.
            None => (DnrAction { kind: "block", redirect: None }, priority::BLOCK),
        }
    } else if rule.options.important {
        (DnrAction { kind: "block", redirect: None }, priority::IMPORTANT_BLOCK)
    } else {
        (DnrAction { kind: "block", redirect: None }, priority::BLOCK)
    };

    Ok(DnrRule { id, priority, action, condition })
}

/// Keep redirect targets to a safe file name; they name bundled resources.
/// The parser already refuses anything else, so this is a second line only.
fn sanitize_resource(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect()
}

/// Compile Adblock-syntax text straight to DNR rules.
///
/// Used for the user's own rules, which the Chromium extension installs as
/// dynamic rules at runtime. Ids start at `first_id` so they cannot collide
/// with the static rulesets. Returns the rules plus a message per line that
/// could not be represented.
pub fn compile_text(text: &str, first_id: u32) -> (Vec<DnrRule>, Vec<String>) {
    let mut builder = EngineBuilder::new();
    builder.add_user_rules(text);
    let problems_from_parser: Vec<String> = builder
        .rejected()
        .iter()
        .map(|r| format!("line {}: {}", r.line_number, r.reason))
        .collect();

    let (db, user_block, user_allow) = builder.split_for_dnr();
    let mut problems = problems_from_parser;
    let mut out = Vec::new();
    let mut regex_budget = MAX_REGEX_RULES;
    let mut id = first_id;

    // Exceptions first so they outrank blocks of equal priority.
    for rule in user_allow
        .iter()
        .chain(user_block.iter())
        .chain(db.network.iter())
        .chain(db.exceptions.iter())
        .chain(db.removeparam.iter())
    {
        match convert(rule, id, &mut regex_budget) {
            Ok(r) => {
                out.push(r);
                id += 1;
            }
            Err(why) => problems.push(format!("{}: cannot be represented ({why:?})", rule.raw)),
        }
    }
    let _ = ListFormat::Adblock;
    (out, problems)
}
