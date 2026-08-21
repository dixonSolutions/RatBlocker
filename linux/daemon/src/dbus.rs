//! The versioned system D-Bus API from `docs/architecture.md` §8.
//!
//! Authorization is enforced here, in the daemon, for every modifying call
//! (§8, §16). Frontends do not get to decide: a hidden button in the GTK app
//! or a disabled toggle in the GNOME extension is a hint to the user, never a
//! security control. Every mutating method asks Polkit about the *calling*
//! process before it does anything.
//!
//! Structured results are returned as JSON strings rather than nested D-Bus
//! variants. That keeps the interface stable as fields are added, and makes it
//! trivial to consume from the GNOME extension's JavaScript.

use std::sync::Arc;
use std::time::Duration;

use ratblocker_core::storage::FilterSubscription;
use zbus::message::Header;
use zbus::{fdo, interface, object_server::SignalEmitter};
use zbus_polkit::policykit1::{AuthorityProxy, CheckAuthorizationFlags, Subject};

use crate::state::DaemonState;
use crate::updater::Updater;

pub const SERVICE_NAME: &str = "io.github.ratblocker.Service";
pub const OBJECT_PATH: &str = "/io/github/ratblocker/Service";

/// Polkit action ids, matching `linux/packaging/polkit/*.policy`.
mod action {
    pub const TOGGLE: &str = "io.github.ratblocker.toggle";
    pub const CONFIGURE: &str = "io.github.ratblocker.configure";
    pub const UPDATE: &str = "io.github.ratblocker.update";
}

/// Longest pause the daemon will accept, so a mistake cannot disable filtering
/// indefinitely without an explicit `SetEnabled(false)`.
const MAX_PAUSE_SECONDS: u64 = 24 * 60 * 60;

pub struct Service {
    pub state: Arc<DaemonState>,
    pub updater: Arc<Updater>,
    /// The system-bus connection Polkit is reached over. The proxy borrows its
    /// connection, so the connection is what gets stored.
    system_bus: Option<zbus::Connection>,
    /// Development escape hatch. Only ever set for a session-bus daemon, where
    /// Polkit cannot identify a caller anyway; never in a real installation.
    authorization_disabled: bool,
}

impl Service {
    pub async fn new(
        state: Arc<DaemonState>,
        updater: Arc<Updater>,
        authorization_disabled: bool,
    ) -> Self {
        // Polkit always lives on the system bus, even when this service is
        // published on the session bus for development.
        let system_bus = if authorization_disabled {
            None
        } else {
            match zbus::Connection::system().await {
                Ok(system) => {
                    // Prove Polkit is actually reachable now, rather than
                    // discovering it is missing on the first privileged call.
                    if let Err(error) = AuthorityProxy::new(&system).await {
                        tracing::error!(%error, "Polkit unavailable: modifying calls will be refused");
                        None
                    } else {
                        Some(system)
                    }
                }
                Err(error) => {
                    tracing::error!(%error, "no system bus: modifying calls will be refused");
                    None
                }
            }
        };
        Self { state, updater, system_bus, authorization_disabled }
    }

    /// Ask Polkit whether the calling process may perform `action`.
    async fn authorize(&self, header: &Header<'_>, action: &str) -> fdo::Result<()> {
        if self.authorization_disabled {
            tracing::warn!(action, "authorization is disabled; allowing (development mode)");
            return Ok(());
        }
        let Some(system) = &self.system_bus else {
            return Err(fdo::Error::AccessDenied(
                "Polkit is unavailable, so this request cannot be authorized".into(),
            ));
        };
        let polkit = AuthorityProxy::new(system)
            .await
            .map_err(|e| fdo::Error::Failed(format!("cannot reach Polkit: {e}")))?;
        let subject = Subject::new_for_message_header(header)
            .map_err(|e| fdo::Error::Failed(format!("cannot identify caller: {e}")))?;
        let result = polkit
            .check_authorization(
                &subject,
                action,
                &std::collections::HashMap::new(),
                CheckAuthorizationFlags::AllowUserInteraction.into(),
                "",
            )
            .await
            .map_err(|e| fdo::Error::Failed(format!("authorization check failed: {e}")))?;
        if result.is_authorized {
            Ok(())
        } else {
            Err(fdo::Error::AccessDenied(format!(
                "not authorized for {action}"
            )))
        }
    }

    /// Persist the user configuration and rebuild what depends on it.
    async fn save_configuration(&self, config: ratblocker_core::Configuration) -> fdo::Result<()> {
        config
            .validate()
            .map_err(|e| fdo::Error::InvalidArgs(e.to_string()))?;
        self.updater
            .save_configuration(&config)
            .await
            .map_err(|e| fdo::Error::Failed(format!("cannot save configuration: {e}")))?;
        self.state.set_config(config);
        self.updater
            .rebuild_engine()
            .await
            .map_err(|e| fdo::Error::Failed(format!("cannot reload filters: {e}")))?;
        Ok(())
    }
}

#[interface(name = "io.github.ratblocker.Service1")]
impl Service {
    /// Overall status, as a JSON object.
    async fn get_status(&self) -> String {
        let config = self.state.config();
        let engine = self.state.engine();
        let counters = &self.state.counters;
        use std::sync::atomic::Ordering::Relaxed;
        serde_json::json!({
            "enabled": config.enabled,
            "paused": self.state.is_paused(),
            "pause_remaining_seconds": self.state.pause_remaining(),
            "filtering_active": self.state.filtering_active(),
            "rules_loaded": engine.rule_count(),
            "dns_enforceable_rules": engine.dns_enforceable_count(),
            "core_version": ratblocker_core::CORE_VERSION,
            "daemon_version": env!("CARGO_PKG_VERSION"),
            "upstreams": self.state.resolver.describe(),
            "listen": self.updater.settings().listen.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "dns": {
                "queries": counters.queries.load(Relaxed),
                "blocked": counters.blocked.load(Relaxed),
                "forwarded": counters.forwarded.load(Relaxed),
                "errors": counters.errors.load(Relaxed),
                "refused_source": counters.refused_source.load(Relaxed),
            },
            "last_update": self.state.last_update.load(Relaxed),
            "sources": engine.sources(),
        })
        .to_string()
    }

    /// Turn filtering on or off.
    async fn set_enabled(
        &self,
        enabled: bool,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        self.authorize(&header, action::TOGGLE).await?;
        let mut config = self.state.config();
        config.enabled = enabled;
        self.state.resume();
        self.save_configuration(config).await?;
        Self::status_changed(&emitter, enabled, false).await?;
        tracing::info!(enabled, "filtering toggled");
        Ok(())
    }

    /// Suspend filtering for a while, then resume automatically.
    async fn pause(
        &self,
        duration_seconds: u64,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        self.authorize(&header, action::TOGGLE).await?;
        if duration_seconds == 0 {
            self.state.resume();
        } else {
            let seconds = duration_seconds.min(MAX_PAUSE_SECONDS);
            self.state.pause(Duration::from_secs(seconds));
            tracing::info!(seconds, "filtering paused");
        }
        Self::status_changed(&emitter, self.state.config().enabled, self.state.is_paused()).await?;
        Ok(())
    }

    /// Local counters, as a JSON object. Empty unless the user enabled them.
    async fn get_statistics(&self) -> String {
        serde_json::to_string(&self.state.statistics.snapshot()).unwrap_or_else(|_| "{}".into())
    }

    /// Clear every local counter (§17: one-click local data deletion).
    async fn reset_statistics(&self, #[zbus(header)] header: Header<'_>) -> fdo::Result<()> {
        self.authorize(&header, action::CONFIGURE).await?;
        self.state.statistics.reset();
        Ok(())
    }

    /// The user configuration, as JSON.
    async fn get_configuration(&self) -> String {
        serde_json::to_string(&self.state.config()).unwrap_or_else(|_| "{}".into())
    }

    async fn add_allowlist_domain(
        &self,
        domain: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        self.authorize(&header, action::CONFIGURE).await?;
        // Normalize and validate before it reaches configuration or the engine.
        let domain = ratblocker_core::url::normalize_host(domain)
            .map_err(|e| fdo::Error::InvalidArgs(format!("{domain:?}: {e}")))?;
        let mut config = self.state.config();
        if !config.allowlisted_domains.contains(&domain) {
            config.allowlisted_domains.push(domain.clone());
            config.allowlisted_domains.sort();
        }
        self.save_configuration(config).await?;
        Self::configuration_changed(&emitter).await?;
        tracing::info!(%domain, "allowlisted");
        Ok(())
    }

    async fn remove_allowlist_domain(
        &self,
        domain: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        self.authorize(&header, action::CONFIGURE).await?;
        let domain = ratblocker_core::url::normalize_host(domain)
            .map_err(|e| fdo::Error::InvalidArgs(format!("{domain:?}: {e}")))?;
        let mut config = self.state.config();
        config.allowlisted_domains.retain(|d| *d != domain);
        self.save_configuration(config).await?;
        Self::configuration_changed(&emitter).await?;
        Ok(())
    }

    /// Subscribe to a filter list. Third-party lists start untrusted (§15).
    async fn add_filter_subscription(
        &self,
        url: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<String> {
        self.authorize(&header, action::CONFIGURE).await?;
        if !url.starts_with("https://") {
            return Err(fdo::Error::InvalidArgs(
                "filter subscriptions must use https".into(),
            ));
        }
        let id = Updater::subscription_id(url);
        let mut config = self.state.config();
        if config.filter_subscriptions.iter().any(|s| s.id == id) {
            return Err(fdo::Error::InvalidArgs("already subscribed".into()));
        }
        config.filter_subscriptions.push(FilterSubscription {
            id: id.clone(),
            enabled: true,
            url: Some(url.to_string()),
            title: None,
            trusted: false,
        });
        self.save_configuration(config).await?;
        Self::configuration_changed(&emitter).await?;
        Ok(id)
    }

    async fn remove_filter_subscription(
        &self,
        subscription_id: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        self.authorize(&header, action::CONFIGURE).await?;
        let mut config = self.state.config();
        let before = config.filter_subscriptions.len();
        config.filter_subscriptions.retain(|s| s.id != subscription_id);
        if config.filter_subscriptions.len() == before {
            return Err(fdo::Error::InvalidArgs("no such subscription".into()));
        }
        self.save_configuration(config).await?;
        Self::configuration_changed(&emitter).await?;
        Ok(())
    }

    /// Download and activate fresh filter lists.
    async fn update_filters(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<String> {
        self.authorize(&header, action::UPDATE).await?;
        match self.updater.update(&self.state.config()).await {
            Ok(report) => {
                let json = serde_json::to_string(&report).unwrap_or_else(|_| "{}".into());
                Self::filters_updated(&emitter, &json).await?;
                Ok(json)
            }
            Err(error) => {
                let message = error.to_string();
                Self::update_failed(&emitter, &message).await?;
                Err(fdo::Error::Failed(message))
            }
        }
    }

    /// Rebuild the engine from the database already on disk.
    async fn reload_filters(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        self.authorize(&header, action::UPDATE).await?;
        self.updater
            .rebuild_engine()
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        Self::filters_updated(&emitter, "{\"reloaded\":true}").await?;
        Ok(())
    }

    /// Whether filtering is switched on. Read-only: changing it goes through
    /// `SetEnabled`, which is authorized.
    #[zbus(property)]
    async fn enabled(&self) -> bool {
        self.state.config().enabled
    }

    #[zbus(property)]
    async fn paused(&self) -> bool {
        self.state.is_paused()
    }

    #[zbus(property)]
    async fn rules_loaded(&self) -> u64 {
        self.state.engine().rule_count() as u64
    }

    #[zbus(property)]
    async fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    // -- Signals -----------------------------------------------------------

    #[zbus(signal)]
    async fn status_changed(emitter: &SignalEmitter<'_>, enabled: bool, paused: bool)
        -> zbus::Result<()>;

    #[zbus(signal)]
    async fn statistics_changed(emitter: &SignalEmitter<'_>, snapshot: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn filters_updated(emitter: &SignalEmitter<'_>, report: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn update_failed(emitter: &SignalEmitter<'_>, reason: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn configuration_changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}
