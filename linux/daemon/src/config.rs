//! Daemon settings — the parts that are the administrator's business, kept
//! separate from the user-facing configuration in `ratblocker_core::storage`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::dns::message::BlockResponse;
use crate::dns::upstream::Upstream;

/// Refuse to load a settings file larger than this.
const MAX_SETTINGS_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheSettings {
    #[serde(default = "default_cache_entries")]
    pub entries: usize,
    /// Floor applied to upstream TTLs, so a one-second TTL cannot turn every
    /// page load into an upstream round trip.
    #[serde(default = "default_min_ttl")]
    pub minimum_ttl_seconds: u64,
}

fn default_cache_entries() -> usize {
    4096
}
fn default_min_ttl() -> u64 {
    30
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            entries: default_cache_entries(),
            minimum_ttl_seconds: default_min_ttl(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Paths {
    /// User-facing configuration (allowlist, subscriptions, privacy).
    #[serde(default = "default_config_path")]
    pub configuration: PathBuf,
    /// The active compiled rule database.
    #[serde(default = "default_database_path")]
    pub database: PathBuf,
    /// Where a downloaded update is assembled before activation.
    #[serde(default = "default_staging_path")]
    pub staging: PathBuf,
    /// The previous database, kept for rollback (§15).
    #[serde(default = "default_backup_path")]
    pub last_known_good: PathBuf,
    /// Filter lists shipped with RatBlocker itself.
    #[serde(default = "default_bundled_path")]
    pub bundled_filters: PathBuf,
    /// Ed25519 public key trusted for RatBlocker's own signed metadata.
    #[serde(default = "default_trusted_key_path")]
    pub trusted_key: PathBuf,
}

fn default_bundled_path() -> PathBuf {
    PathBuf::from("/usr/share/ratblocker/filters")
}
fn default_trusted_key_path() -> PathBuf {
    PathBuf::from("/usr/share/ratblocker/update-signing.pub")
}

fn default_config_path() -> PathBuf {
    PathBuf::from("/var/lib/ratblocker/config.yaml")
}
fn default_database_path() -> PathBuf {
    PathBuf::from("/var/lib/ratblocker/rules.rbdb")
}
fn default_staging_path() -> PathBuf {
    PathBuf::from("/var/lib/ratblocker/staging")
}
fn default_backup_path() -> PathBuf {
    PathBuf::from("/var/lib/ratblocker/rules.rbdb.last-known-good")
}

impl Default for Paths {
    fn default() -> Self {
        Self {
            configuration: default_config_path(),
            database: default_database_path(),
            staging: default_staging_path(),
            last_known_good: default_backup_path(),
            bundled_filters: default_bundled_path(),
            trusted_key: default_trusted_key_path(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonSettings {
    /// Addresses the DNS proxy binds. Local addresses only.
    #[serde(default = "default_listen")]
    pub listen: Vec<SocketAddr>,
    #[serde(default = "default_upstreams")]
    pub upstream: Vec<Upstream>,
    #[serde(default = "default_upstream_timeout")]
    pub upstream_timeout_seconds: u64,
    #[serde(default)]
    pub cache: CacheSettings,
    #[serde(default)]
    pub block_response: BlockResponse,
    /// TTL served with a synthesized block answer.
    #[serde(default = "default_block_ttl")]
    pub block_ttl_seconds: u32,
    #[serde(default)]
    pub paths: Paths,
    /// Largest filter list the updater will download.
    #[serde(default = "default_max_download")]
    pub max_download_bytes: u64,
}

fn default_listen() -> Vec<SocketAddr> {
    // Not 127.0.0.1:53 or 127.0.0.53:53 — systemd-resolved already owns those
    // on a typical Ubuntu system, and taking them would be a fight.
    vec!["127.0.0.2:53".parse().expect("valid default listen address")]
}

fn default_upstreams() -> Vec<Upstream> {
    // The machine's own resolvers first: on a network that only permits its
    // own DNS server, a public resolver simply times out. Quad9 is kept as a
    // fallback for the case where no system resolver can be read.
    vec![
        Upstream::System,
        Upstream::Plain { address: "9.9.9.9:53".parse().unwrap() },
        Upstream::Plain { address: "149.112.112.112:53".parse().unwrap() },
    ]
}

fn default_upstream_timeout() -> u64 {
    5
}
fn default_block_ttl() -> u32 {
    60
}
fn default_max_download() -> u64 {
    64 * 1024 * 1024
}

impl Default for DaemonSettings {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            upstream: default_upstreams(),
            upstream_timeout_seconds: default_upstream_timeout(),
            cache: CacheSettings::default(),
            block_response: BlockResponse::default(),
            block_ttl_seconds: default_block_ttl(),
            paths: Paths::default(),
            max_download_bytes: default_max_download(),
        }
    }
}

impl DaemonSettings {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            tracing::info!(path = %path.display(), "no settings file; using defaults");
            return Ok(Self::default());
        }
        let meta = std::fs::metadata(path)?;
        if meta.len() > MAX_SETTINGS_BYTES {
            anyhow::bail!("{} is implausibly large for a settings file", path.display());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let settings: Self = serde_yaml_ng::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn validate(&self) -> Result<()> {
        if self.listen.is_empty() {
            anyhow::bail!("at least one listen address is required");
        }
        for addr in &self.listen {
            let ip = addr.ip();
            let local = ip.is_loopback()
                || matches!(ip, std::net::IpAddr::V4(v4) if v4.is_private() || v4.is_link_local());
            if !local {
                anyhow::bail!(
                    "refusing to listen on {addr}: RatBlocker must not be reachable as an open resolver"
                );
            }
        }
        if self.upstream.is_empty() {
            anyhow::bail!("at least one upstream resolver is required");
        }
        if self.upstream_timeout_seconds == 0 || self.upstream_timeout_seconds > 60 {
            anyhow::bail!("upstream_timeout_seconds must be between 1 and 60");
        }
        Ok(())
    }

    pub fn upstream_timeout(&self) -> Duration {
        Duration::from_secs(self.upstream_timeout_seconds)
    }

    pub fn cache_floor(&self) -> Duration {
        Duration::from_secs(self.cache.minimum_ttl_seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_local_only() {
        DaemonSettings::default().validate().unwrap();
    }

    #[test]
    fn a_routable_listen_address_is_refused() {
        let mut s = DaemonSettings::default();
        s.listen = vec!["0.0.0.0:53".parse().unwrap()];
        assert!(s.validate().is_err(), "0.0.0.0 would be an open resolver");
        s.listen = vec!["8.8.8.8:53".parse().unwrap()];
        assert!(s.validate().is_err());
    }

    #[test]
    fn settings_round_trip_through_yaml() {
        let text = serde_yaml_ng::to_string(&DaemonSettings::default()).unwrap();
        let back: DaemonSettings = serde_yaml_ng::from_str(&text).unwrap();
        assert_eq!(back.listen, DaemonSettings::default().listen);
    }
}
