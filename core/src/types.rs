//! Platform-neutral data model shared by every RatBlocker frontend.
//!
//! The variants documented in `docs/architecture.md` are all present and keep
//! their documented names. A few extra `ResourceType` variants exist on top of
//! them because EasyList options such as `$subdocument` and `$ping` cannot be
//! represented otherwise; they collapse to `Other` for consumers that only know
//! the documented set.

use serde::{Deserialize, Serialize};

/// What the engine decided to do with a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterDecision {
    Allow,
    Block,
    Redirect,
    RemoveParameters,
}

impl FilterDecision {
    /// True when the request must not reach the network unchanged.
    pub fn is_intervention(self) -> bool {
        !matches!(self, FilterDecision::Allow)
    }
}

/// Kind of resource a request is fetching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceType {
    Document,
    Script,
    Image,
    Stylesheet,
    Font,
    Media,
    WebSocket,
    XmlHttpRequest,
    Other,
    // Extensions beyond the documented set, required for EasyList option parity.
    Subdocument,
    Object,
    Ping,
    CspReport,
}

impl ResourceType {
    /// Single-bit mask used by the option matcher.
    pub const fn mask(self) -> u32 {
        match self {
            ResourceType::Document => 1 << 0,
            ResourceType::Script => 1 << 1,
            ResourceType::Image => 1 << 2,
            ResourceType::Stylesheet => 1 << 3,
            ResourceType::Font => 1 << 4,
            ResourceType::Media => 1 << 5,
            ResourceType::WebSocket => 1 << 6,
            ResourceType::XmlHttpRequest => 1 << 7,
            ResourceType::Other => 1 << 8,
            ResourceType::Subdocument => 1 << 9,
            ResourceType::Object => 1 << 10,
            ResourceType::Ping => 1 << 11,
            ResourceType::CspReport => 1 << 12,
        }
    }

    /// Every bit a rule with no explicit type option should match.
    pub const ALL: u32 = (1 << 13) - 1;

    /// Parse an EasyList type option. Returns `None` for options that are not
    /// resource types (`third-party`, `domain=`, ...).
    pub fn from_option(name: &str) -> Option<Self> {
        Some(match name {
            "document" | "doc" | "main_frame" => ResourceType::Document,
            "script" => ResourceType::Script,
            "image" | "background" => ResourceType::Image,
            "stylesheet" | "css" => ResourceType::Stylesheet,
            "font" => ResourceType::Font,
            "media" => ResourceType::Media,
            "websocket" => ResourceType::WebSocket,
            "xmlhttprequest" | "xhr" => ResourceType::XmlHttpRequest,
            "subdocument" | "frame" => ResourceType::Subdocument,
            "object" | "object-subrequest" => ResourceType::Object,
            "ping" | "beacon" => ResourceType::Ping,
            "csp_report" => ResourceType::CspReport,
            "other" => ResourceType::Other,
            _ => return None,
        })
    }

    /// Best-effort guess from a file extension, used by DNS/system layers that
    /// only observe a URL and by the compatibility test corpus.
    pub fn from_path(path: &str) -> Self {
        let tail = path.rsplit('/').next().unwrap_or(path);
        let ext = tail.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        let ext = ext.split(['?', '#']).next().unwrap_or(ext);
        match ext.to_ascii_lowercase().as_str() {
            "js" | "mjs" | "cjs" => ResourceType::Script,
            "css" => ResourceType::Stylesheet,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" | "avif" => ResourceType::Image,
            "woff" | "woff2" | "ttf" | "otf" | "eot" => ResourceType::Font,
            "mp4" | "webm" | "mp3" | "ogg" | "m4a" | "m3u8" => ResourceType::Media,
            "html" | "htm" => ResourceType::Document,
            _ => ResourceType::Other,
        }
    }
}

/// Whether a request goes to the same site as the page that triggered it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Party {
    First,
    Third,
    /// No source URL was available (top-level navigation, DNS-only layers).
    Unknown,
}

/// Everything the engine needs to decide about one request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestContext {
    pub request_url: String,
    pub source_url: Option<String>,
    pub application_id: Option<String>,
    pub resource_type: ResourceType,
    /// True when the navigation created a new browser window or tab.
    #[serde(default)]
    pub is_popup: bool,
}

impl RequestContext {
    pub fn new(request_url: impl Into<String>, resource_type: ResourceType) -> Self {
        Self {
            request_url: request_url.into(),
            source_url: None,
            application_id: None,
            resource_type,
            is_popup: false,
        }
    }

    pub fn with_source(mut self, source_url: impl Into<String>) -> Self {
        self.source_url = Some(source_url.into());
        self
    }

    pub fn with_application(mut self, application_id: impl Into<String>) -> Self {
        self.application_id = Some(application_id.into());
        self
    }

    pub fn as_popup(mut self) -> Self {
        self.is_popup = true;
        self
    }
}

/// The engine's answer, plus whatever the decision needs to be carried out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterResult {
    pub decision: FilterDecision,
    pub matched_rule_id: Option<String>,
    /// Set when `decision == Redirect`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub redirect_to: Option<String>,
    /// Set when `decision == RemoveParameters`; the URL with them stripped.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub rewritten_url: Option<String>,
    /// Names of the query parameters that were removed.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub removed_parameters: Vec<String>,
}

impl FilterResult {
    pub fn allow() -> Self {
        Self {
            decision: FilterDecision::Allow,
            matched_rule_id: None,
            redirect_to: None,
            rewritten_url: None,
            removed_parameters: Vec::new(),
        }
    }

    /// An allow decision that records which exception rule produced it.
    pub fn allowed_by(rule_id: impl Into<String>) -> Self {
        Self {
            matched_rule_id: Some(rule_id.into()),
            ..Self::allow()
        }
    }

    pub fn blocked_by(rule_id: impl Into<String>) -> Self {
        Self {
            decision: FilterDecision::Block,
            matched_rule_id: Some(rule_id.into()),
            ..Self::allow()
        }
    }
}
