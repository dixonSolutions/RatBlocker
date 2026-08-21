//! The on-disk configuration model from `docs/architecture.md` §18, plus its
//! versioned migration path.
//!
//! Core only defines the shape and the migrations. Where it is stored — SQLite
//! on Linux and Android, extension storage in browsers — is the platform's
//! decision.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::rule_engine::{ApplicationPolicy, EngineConfig};

/// Current configuration schema version.
pub const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterSubscription {
    pub id: String,
    #[serde(default = "crate::storage::default_true")]
    pub enabled: bool,
    /// Absent for the lists bundled with RatBlocker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Third-party subscriptions require explicit trust before they are used.
    #[serde(default)]
    pub trusted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateSettings {
    #[serde(default = "crate::storage::default_true")]
    pub automatic: bool,
    #[serde(default = "default_interval")]
    pub interval_hours: u32,
}

fn default_interval() -> u32 {
    24
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self {
            automatic: true,
            interval_hours: default_interval(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacySettings {
    /// Off by default (§17).
    #[serde(default)]
    pub statistics_enabled: bool,
    #[serde(default)]
    pub request_logging_enabled: bool,
    #[serde(default)]
    pub per_domain_statistics: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkPolicy {
    /// Disable filtering entirely on this network.
    #[serde(default)]
    pub bypass: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Configuration {
    pub version: u32,
    #[serde(default = "crate::storage::default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub filter_subscriptions: Vec<FilterSubscription>,
    #[serde(default)]
    pub custom_rules: Vec<String>,
    #[serde(default)]
    pub allowlisted_domains: Vec<String>,
    #[serde(default)]
    pub application_policies: HashMap<String, ApplicationPolicy>,
    #[serde(default)]
    pub network_policies: HashMap<String, NetworkPolicy>,
    #[serde(default)]
    pub updates: UpdateSettings,
    #[serde(default)]
    pub privacy: PrivacySettings,
}

pub(crate) fn default_true() -> bool {
    true
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            enabled: true,
            filter_subscriptions: vec![
                FilterSubscription {
                    id: "easylist".into(),
                    enabled: true,
                    url: None,
                    title: Some("EasyList".into()),
                    trusted: true,
                },
                FilterSubscription {
                    id: "easyprivacy".into(),
                    enabled: true,
                    url: None,
                    title: Some("EasyPrivacy".into()),
                    trusted: true,
                },
            ],
            custom_rules: Vec::new(),
            allowlisted_domains: Vec::new(),
            application_policies: HashMap::new(),
            network_policies: HashMap::new(),
            updates: UpdateSettings::default(),
            privacy: PrivacySettings::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("configuration version {0} is newer than this build supports ({CURRENT_VERSION})")]
    FutureVersion(u32),
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

impl Configuration {
    /// Bring a configuration loaded from disk up to `CURRENT_VERSION`.
    ///
    /// Migrations are additive and each step is covered by a test, so a
    /// downgrade is detectable and a corrupt file never silently loses data.
    pub fn migrate(self) -> Result<Self, ConfigError> {
        #[allow(unused_mut)]
        let mut this = self;
        if this.version > CURRENT_VERSION {
            return Err(ConfigError::FutureVersion(this.version));
        }
        while this.version < CURRENT_VERSION {
            match this.version {
                // Future migrations land here, one arm per version bump.
                v => {
                    return Err(ConfigError::Invalid(format!(
                        "no migration from version {v}"
                    )))
                }
            }
        }
        this.validate()?;
        Ok(this)
    }

    /// Reject values that would produce a broken or unsafe engine.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.updates.interval_hours == 0 || self.updates.interval_hours > 24 * 30 {
            return Err(ConfigError::Invalid(
                "updates.interval_hours must be between 1 and 720".into(),
            ));
        }
        for d in &self.allowlisted_domains {
            crate::url::normalize_host(d)
                .map_err(|_| ConfigError::Invalid(format!("bad allowlist domain: {d}")))?;
        }
        for s in &self.filter_subscriptions {
            if s.id.is_empty() || !s.id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
                return Err(ConfigError::Invalid(format!(
                    "subscription id must be alphanumeric: {:?}",
                    s.id
                )));
            }
            if let Some(url) = &s.url {
                if !url.starts_with("https://") {
                    return Err(ConfigError::Invalid(format!(
                        "subscription {} must use https", s.id
                    )));
                }
            }
        }
        Ok(())
    }

    /// Project the parts of the configuration the engine consults per request.
    pub fn engine_config(&self) -> EngineConfig {
        EngineConfig {
            allowlisted_domains: self
                .allowlisted_domains
                .iter()
                .filter_map(|d| crate::url::normalize_host(d).ok())
                .collect(),
            application_policies: self.application_policies.clone(),
            enabled: self.enabled,
        }
    }
}
