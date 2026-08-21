//! Optional, local-only counters.
//!
//! Per `docs/architecture.md` §17 these are disabled by default, never leave
//! the device, and record no URLs — only counts and, when explicitly enabled,
//! the registrable domain of blocked requests.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::types::FilterDecision;

/// A snapshot suitable for display or D-Bus transport.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatisticsSnapshot {
    pub requests_seen: u64,
    pub requests_blocked: u64,
    pub requests_redirected: u64,
    pub parameters_removed: u64,
    /// Only populated when per-domain counting is enabled.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub top_blocked_domains: HashMap<String, u64>,
}

/// Thread-safe counters. Cheap enough to update on every request.
#[derive(Debug, Default)]
pub struct Statistics {
    enabled: std::sync::atomic::AtomicBool,
    per_domain: std::sync::atomic::AtomicBool,
    seen: AtomicU64,
    blocked: AtomicU64,
    redirected: AtomicU64,
    params_removed: AtomicU64,
    domains: std::sync::Mutex<HashMap<String, u64>>,
}

/// Cap on distinct domains retained, so memory stays bounded (§21).
const MAX_TRACKED_DOMAINS: usize = 512;

impl Statistics {
    pub fn new(enabled: bool, per_domain: bool) -> Self {
        Self {
            enabled: enabled.into(),
            per_domain: per_domain.into(),
            ..Default::default()
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn record(&self, decision: FilterDecision, blocked_domain: Option<&str>) {
        if !self.is_enabled() {
            return;
        }
        self.seen.fetch_add(1, Ordering::Relaxed);
        match decision {
            FilterDecision::Block => {
                self.blocked.fetch_add(1, Ordering::Relaxed);
                if self.per_domain.load(Ordering::Relaxed) {
                    if let Some(d) = blocked_domain {
                        if let Ok(mut map) = self.domains.lock() {
                            if map.len() < MAX_TRACKED_DOMAINS || map.contains_key(d) {
                                *map.entry(d.to_string()).or_insert(0) += 1;
                            }
                        }
                    }
                }
            }
            FilterDecision::Redirect => {
                self.redirected.fetch_add(1, Ordering::Relaxed);
            }
            FilterDecision::RemoveParameters => {
                self.params_removed.fetch_add(1, Ordering::Relaxed);
            }
            FilterDecision::Allow => {}
        }
    }

    pub fn snapshot(&self) -> StatisticsSnapshot {
        StatisticsSnapshot {
            requests_seen: self.seen.load(Ordering::Relaxed),
            requests_blocked: self.blocked.load(Ordering::Relaxed),
            requests_redirected: self.redirected.load(Ordering::Relaxed),
            parameters_removed: self.params_removed.load(Ordering::Relaxed),
            top_blocked_domains: self
                .domains
                .lock()
                .map(|m| m.clone())
                .unwrap_or_default(),
        }
    }

    /// One-click local data deletion (§17).
    pub fn reset(&self) {
        self.seen.store(0, Ordering::Relaxed);
        self.blocked.store(0, Ordering::Relaxed);
        self.redirected.store(0, Ordering::Relaxed);
        self.params_removed.store(0, Ordering::Relaxed);
        if let Ok(mut m) = self.domains.lock() {
            m.clear();
        }
    }
}
