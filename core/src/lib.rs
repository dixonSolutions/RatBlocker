//! RatBlocker's shared filtering core.
//!
//! Every RatBlocker frontend — the Linux daemon, the Android VPN service, both
//! browser extensions — makes its decisions here, so that a rule behaves
//! identically on every platform. The crate has no platform dependencies and
//! no I/O; callers supply rule text and requests and receive decisions.
//!
//! ```
//! use ratblocker_core::{EngineBuilder, EngineConfig, ListFormat, RequestContext, ResourceType, FilterDecision};
//!
//! let mut builder = EngineBuilder::new();
//! builder.add_list("demo", "||ads.example.com^", ListFormat::Adblock);
//! let engine = builder.build(EngineConfig::default());
//!
//! let request = RequestContext::new("https://ads.example.com/banner.png", ResourceType::Image)
//!     .with_source("https://news.example.org/");
//! assert_eq!(engine.evaluate(&request).decision, FilterDecision::Block);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod matcher;
pub mod parser;
pub mod rule_engine;
pub mod statistics;
pub mod storage;
pub mod types;
pub mod url;

pub use parser::{parse_line, ListFormat, ParsedLine, RejectReason};
pub use rule_engine::database::{RuleDatabase, SourceInfo};
pub use rule_engine::{
    ApplicationPolicy, Engine, EngineBuilder, EngineConfig, LoadStats, RejectedLine,
};
pub use statistics::{Statistics, StatisticsSnapshot};
pub use storage::Configuration;
pub use types::{FilterDecision, FilterResult, Party, RequestContext, ResourceType};

/// Version of the core API, exposed so frontends can assert compatibility.
pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");
