//! Filter acquisition, compilation and activation.
//!
//! Implements the update pipeline from `docs/architecture.md` §15. The shape
//! that matters: nothing downloaded is ever compiled straight over the live
//! database. A download is bounded, validated and compiled into a staging
//! file; only a staging file that produced a working engine is renamed into
//! place, and the previous database is retained so a bad update can be rolled
//! back.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use ratblocker_core::parser::ListFormat;
use ratblocker_core::rule_engine::EngineBuilder;
use ratblocker_core::{Configuration, Engine, RuleDatabase};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::config::DaemonSettings;
use crate::state::DaemonState;

/// How long a single list download may take.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
/// Shortest interval between two update runs, so a client cannot be used to
/// hammer a filter-list provider (§15: update frequency limits).
const MIN_UPDATE_INTERVAL: Duration = Duration::from_secs(300);

/// What an update run did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct UpdateReport {
    pub lists: Vec<ListReport>,
    pub network_rules: usize,
    pub cosmetic_rules: usize,
    pub rejected_lines: usize,
    pub activated: bool,
    pub rolled_back: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListReport {
    pub id: String,
    pub source: String,
    pub bytes: usize,
    pub checksum: String,
    pub rules: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub struct Updater {
    settings: DaemonSettings,
    http: reqwest::Client,
    state: OnceLock<Arc<DaemonState>>,
    last_run: std::sync::Mutex<Option<std::time::Instant>>,
}

impl Updater {
    pub fn new(settings: DaemonSettings) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(DOWNLOAD_TIMEOUT)
            .connect_timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::limited(3))
            .user_agent(concat!("RatBlocker/", env!("CARGO_PKG_VERSION")))
            // The `cookies` feature is deliberately not enabled, so the client
            // has no cookie store at all: an update cannot carry identity with
            // it even accidentally.
            .build()
            .context("building the HTTP client")?;
        Ok(Self {
            settings,
            http,
            state: OnceLock::new(),
            last_run: std::sync::Mutex::new(None),
        })
    }

    pub fn attach(&self, state: Arc<DaemonState>) {
        let _ = self.state.set(state);
    }

    fn state(&self) -> Result<&Arc<DaemonState>> {
        self.state.get().context("updater is not attached to daemon state")
    }

    pub fn settings(&self) -> &DaemonSettings {
        &self.settings
    }

    /// A stable, filesystem-safe id derived from a subscription URL.
    pub fn subscription_id(url: &str) -> String {
        let digest = Sha256::digest(url.as_bytes());
        format!("sub-{:x}", digest)[..16].to_string()
    }

    // -- Configuration -----------------------------------------------------

    pub async fn load_configuration(&self) -> Result<Configuration> {
        let path = &self.settings.paths.configuration;
        if !path.exists() {
            let config = Configuration::default();
            self.save_configuration(&config).await?;
            return Ok(config);
        }
        let text = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("reading {}", path.display()))?;
        let config: Configuration = serde_yaml_ng::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;
        config.migrate().context("migrating configuration")
    }

    pub async fn save_configuration(&self, config: &Configuration) -> Result<()> {
        config.validate()?;
        let path = &self.settings.paths.configuration;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        let text = serde_yaml_ng::to_string(config)?;
        write_atomic(path, text.as_bytes()).await
    }

    // -- Engine ------------------------------------------------------------

    /// Build an engine from the database on disk plus the user's own rules.
    pub async fn build_engine(&self, config: &Configuration) -> Result<Engine> {
        let path = &self.settings.paths.database;
        let db = if path.exists() {
            let bytes = tokio::fs::read(path)
                .await
                .with_context(|| format!("reading {}", path.display()))?;
            postcard::from_bytes::<RuleDatabase>(&bytes)
                .with_context(|| format!("decoding {}", path.display()))?
        } else {
            tracing::warn!(path = %path.display(), "no rule database yet; starting with no rules");
            RuleDatabase::new()
        };
        db.check_version()?;
        let user_rules = config.custom_rules.join("\n");
        Engine::from_database(db, &user_rules, config.engine_config())
            .context("building the filtering engine")
    }

    /// Rebuild and atomically install the engine from the current database.
    pub async fn rebuild_engine(&self) -> Result<()> {
        let state = self.state()?;
        let config = state.config();
        let engine = self.build_engine(&config).await?;
        let rules = engine.rule_count();
        state.replace_engine(engine);
        tracing::info!(rules, "filter engine reloaded");
        Ok(())
    }

    // -- Updates -----------------------------------------------------------

    /// Fetch, validate, compile and activate the configured filter lists.
    pub async fn update(&self, config: &Configuration) -> Result<UpdateReport> {
        {
            let mut last = self.last_run.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(previous) = *last {
                let elapsed = previous.elapsed();
                if elapsed < MIN_UPDATE_INTERVAL {
                    bail!(
                        "an update ran {}s ago; wait {}s",
                        elapsed.as_secs(),
                        (MIN_UPDATE_INTERVAL - elapsed).as_secs()
                    );
                }
            }
            *last = Some(std::time::Instant::now());
        }

        let mut report = UpdateReport::default();
        let mut builder = EngineBuilder::new();
        let mut any_content = false;

        for subscription in config.filter_subscriptions.iter().filter(|s| s.enabled) {
            match self.acquire(subscription).await {
                Ok((source, content)) => {
                    let checksum = format!("{:x}", Sha256::digest(content.as_bytes()));
                    let before = builder.stats().network_rules + builder.stats().cosmetic_rules;
                    let format = ListFormat::detect(&content);
                    builder.add_list(&subscription.id, &content, format);
                    builder.set_source_provenance(
                        &subscription.id,
                        subscription.url.clone(),
                        Some(checksum.clone()),
                    );
                    let after = builder.stats().network_rules + builder.stats().cosmetic_rules;
                    report.lists.push(ListReport {
                        id: subscription.id.clone(),
                        source,
                        bytes: content.len(),
                        checksum,
                        rules: after - before,
                        error: None,
                    });
                    any_content = true;
                }
                Err(error) => {
                    // One unreachable list must not discard every other list.
                    tracing::warn!(id = %subscription.id, %error, "list unavailable");
                    report.lists.push(ListReport {
                        id: subscription.id.clone(),
                        source: subscription.url.clone().unwrap_or_else(|| "bundled".into()),
                        bytes: 0,
                        checksum: String::new(),
                        rules: 0,
                        error: Some(error.to_string()),
                    });
                }
            }
        }

        if !any_content {
            report.message = Some("no filter list could be read; keeping the current rules".into());
            return Ok(report);
        }

        report.rejected_lines = builder.stats().rejected;
        let database = builder.into_database();
        report.network_rules = database.network.len();
        report.cosmetic_rules = database.cosmetic.len();

        // Compile into staging, prove it loads, then activate atomically.
        let staging = self.settings.paths.staging.join("rules.rbdb");
        if let Some(parent) = staging.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        let encoded = postcard::to_stdvec(&database).context("serializing rule database")?;
        write_atomic(&staging, &encoded).await?;

        let user_rules = config.custom_rules.join("\n");
        let candidate = Engine::from_database(database, &user_rules, config.engine_config())
            .context("the compiled database did not produce a working engine")?;

        // Keep the current database as last-known-good before replacing it.
        let live = &self.settings.paths.database;
        if live.exists() {
            tokio::fs::copy(live, &self.settings.paths.last_known_good)
                .await
                .context("saving the last-known-good database")?;
        }
        tokio::fs::rename(&staging, live)
            .await
            .context("activating the new database")?;

        let rules = candidate.rule_count();
        self.state()?.replace_engine(candidate);
        report.activated = true;
        tracing::info!(rules, lists = report.lists.len(), "filters updated");
        Ok(report)
    }

    /// Restore the previous database after a bad update (§15).
    pub async fn roll_back(&self) -> Result<()> {
        let backup = &self.settings.paths.last_known_good;
        if !backup.exists() {
            bail!("no last-known-good database to roll back to");
        }
        tokio::fs::copy(backup, &self.settings.paths.database)
            .await
            .context("restoring the last-known-good database")?;
        self.rebuild_engine().await
    }

    /// Read a list from disk, or download it under strict limits.
    async fn acquire(
        &self,
        subscription: &ratblocker_core::storage::FilterSubscription,
    ) -> Result<(String, String)> {
        let Some(url) = &subscription.url else {
            // A bundled list: read it from the package's own directory.
            let path = self
                .settings
                .paths
                .bundled_filters
                .join(format!("{}.txt", subscription.id));
            // The id is validated by `Configuration::validate` to be
            // alphanumeric, so it cannot escape this directory.
            let content = tokio::fs::read_to_string(&path)
                .await
                .with_context(|| format!("reading bundled list {}", path.display()))?;
            return Ok((format!("bundled:{}", path.display()), content));
        };

        if !url.starts_with("https://") {
            bail!("filter subscriptions must use https");
        }
        if !subscription.trusted {
            bail!("subscription is not trusted; enable it explicitly before it is used");
        }

        let response = self
            .http
            .get(url)
            .send()
            .await
            .with_context(|| format!("requesting {url}"))?
            .error_for_status()
            .with_context(|| format!("{url} returned an error status"))?;

        // Refuse a declared length over the limit before reading a byte.
        if let Some(len) = response.content_length() {
            if len > self.settings.max_download_bytes {
                bail!(
                    "{url} declares {len} bytes, over the {} byte limit",
                    self.settings.max_download_bytes
                );
            }
        }

        // Stream, so an undeclared or lying length cannot exhaust memory.
        let mut body = Vec::new();
        let mut stream = response;
        while let Some(chunk) = stream.chunk().await.context("reading response body")? {
            if body.len() + chunk.len() > self.settings.max_download_bytes as usize {
                bail!(
                    "{url} exceeded the {} byte limit mid-download",
                    self.settings.max_download_bytes
                );
            }
            body.extend_from_slice(&chunk);
        }

        let content = String::from_utf8(body)
            .with_context(|| format!("{url} is not valid UTF-8"))?;
        Ok((url.clone(), content))
    }

    /// Verify a detached Ed25519 signature over RatBlocker's own metadata.
    ///
    /// Third-party subscriptions are not signed by us and are governed by the
    /// explicit-trust flag instead; this path covers the lists and metadata
    /// RatBlocker itself publishes (§15).
    pub fn verify_signature(&self, data: &[u8], signature_b64: &str) -> Result<()> {
        let key_path = &self.settings.paths.trusted_key;
        let key_text = std::fs::read_to_string(key_path)
            .with_context(|| format!("reading trusted key {}", key_path.display()))?;
        verify_detached(data, signature_b64, key_text.trim())
    }
}

/// Verify `signature_b64` over `data` using a base64 Ed25519 public key.
pub fn verify_detached(data: &[u8], signature_b64: &str, public_key_b64: &str) -> Result<()> {
    use base64::Engine as _;
    let engine = base64::engine::general_purpose::STANDARD;

    let key_bytes = engine
        .decode(public_key_b64)
        .context("trusted key is not valid base64")?;
    let key_bytes: [u8; 32] = key_bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("trusted key must be 32 bytes"))?;
    let key = VerifyingKey::from_bytes(&key_bytes).context("trusted key is not a valid Ed25519 key")?;

    let sig_bytes = engine
        .decode(signature_b64)
        .context("signature is not valid base64")?;
    let sig_bytes: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("signature must be 64 bytes"))?;
    let signature = Signature::from_bytes(&sig_bytes);

    key.verify(data, &signature)
        .context("signature does not match the trusted key")
}

/// Write through a temporary file and rename, so a reader never observes a
/// partial file and a failed write leaves the previous content intact.
async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp: PathBuf = path.with_extension("tmp");
    let mut file = tokio::fs::File::create(&tmp)
        .await
        .with_context(|| format!("creating {}", tmp.display()))?;
    file.write_all(bytes).await?;
    // Durability before the rename: a crash must not leave a renamed-but-empty
    // file where a valid database used to be.
    file.sync_all().await?;
    drop(file);
    tokio::fs::rename(&tmp, path)
        .await
        .with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

/// Unused placeholder to keep `HashMap` imported for future policy maps.
#[allow(dead_code)]
type PolicyMap = HashMap<String, String>;
