//! `ratblockerd` — the RatBlocker system filtering daemon.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use ratblocker_daemon::config::DaemonSettings;
use ratblocker_daemon::dbus::{Service, OBJECT_PATH, SERVICE_NAME};
use ratblocker_daemon::dns::cache::Cache;
use ratblocker_daemon::dns::{proxy, upstream, upstream::Resolver};
use ratblocker_daemon::state::DaemonState;
use ratblocker_daemon::updater::Updater;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "ratblockerd", version, about = "RatBlocker filtering daemon")]
struct Cli {
    /// Daemon settings file.
    #[arg(long, default_value = "/etc/ratblocker/daemon.yaml")]
    settings: PathBuf,
    /// Connect to the session bus instead of the system bus. For development:
    /// it needs no root and no installed D-Bus policy.
    #[arg(long)]
    session_bus: bool,
    /// Check the settings and rule database, then exit.
    #[arg(long)]
    check: bool,
    /// Skip Polkit authorization. Development only, and only meaningful with
    /// `--session-bus`, where Polkit cannot identify the caller regardless.
    #[arg(long, requires = "session_bus")]
    dev_no_authorization: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Both `ring` and `aws-lc-rs` end up in the dependency graph (rustls
    // directly, and again through reqwest), so rustls cannot pick one on its
    // own. Choose explicitly and early, before anything opens a TLS
    // connection, so DNS-over-TLS and filter downloads share one provider.
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("a TLS crypto provider was already installed"))?;

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("RATBLOCKER_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        // Logging is off by default beyond info level, and no request is ever
        // logged at info: request logging must be opted into (§17).
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let settings = DaemonSettings::load(&cli.settings)
        .with_context(|| format!("loading {}", cli.settings.display()))?;
    settings.validate()?;

    let updater = Arc::new(Updater::new(settings.clone())?);
    let config = updater.load_configuration().await?;
    let engine = updater.build_engine(&config).await?;

    tracing::info!(
        rules = engine.rule_count(),
        lists = engine.sources().len(),
        "filter engine ready"
    );

    if cli.check {
        println!("settings OK: {}", cli.settings.display());
        println!("rules loaded: {}", engine.rule_count());
        for source in engine.sources() {
            println!(
                "  {} {} ({} rules)",
                source.id,
                source.version.as_deref().unwrap_or("-"),
                source.rule_count
            );
        }
        return Ok(());
    }

    let resolver = Resolver::new(
        settings.upstream.clone(),
        settings.upstream_timeout(),
        &settings.listen,
    )?;
    let state = Arc::new(DaemonState::new(
        engine,
        config,
        resolver,
        Cache::new(settings.cache.entries),
        settings.block_response,
        settings.cache_floor(),
        settings.block_ttl_seconds,
    ));
    updater.attach(state.clone());

    // DNS listeners, one UDP and one TCP task per configured address.
    for addr in &settings.listen {
        let udp = tokio::spawn(proxy::run_udp(state.clone(), *addr));
        let tcp = tokio::spawn(proxy::run_tcp(state.clone(), *addr));
        tokio::spawn(async move {
            if let Ok(Err(error)) = udp.await {
                tracing::error!(error = format!("{error:#}"), "UDP listener stopped");
            }
        });
        tokio::spawn(async move {
            if let Ok(Err(error)) = tcp.await {
                tracing::error!(error = format!("{error:#}"), "TCP listener stopped");
            }
        });
    }

    // Follow the machine's own resolvers while they move under us. A VPN or a
    // change of network replaces them, and a proxy still pointed at the
    // previous set answers nothing — so this is what keeps DNS working across
    // a VPN connecting rather than breaking the moment the tunnel comes up.
    if state.resolver.follows_system() {
        let state = state.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(upstream::SYSTEM_POLL_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                state.refresh_upstreams();
            }
        });
    }

    // D-Bus.
    let connection = if cli.session_bus {
        zbus::connection::Builder::session()?
    } else {
        zbus::connection::Builder::system()?
    }
    .build()
    .await
    .context("connecting to D-Bus")?;

    if cli.dev_no_authorization {
        tracing::warn!("AUTHORIZATION DISABLED — development mode, do not use in an installation");
    }
    let service = Service::new(state.clone(), updater.clone(), cli.dev_no_authorization).await;
    connection
        .object_server()
        .at(OBJECT_PATH, service)
        .await
        .context("publishing the D-Bus object")?;
    connection
        .request_name(SERVICE_NAME)
        .await
        .with_context(|| format!("claiming the bus name {SERVICE_NAME}"))?;
    tracing::info!(bus = if cli.session_bus { "session" } else { "system" }, "D-Bus service ready");

    // Periodic updates, when the user has asked for them.
    {
        let updater = updater.clone();
        let state = state.clone();
        tokio::spawn(async move {
            loop {
                let config = state.config();
                let interval = std::time::Duration::from_secs(
                    u64::from(config.updates.interval_hours.max(1)) * 3600,
                );
                tokio::time::sleep(interval).await;
                if !state.config().updates.automatic {
                    continue;
                }
                match updater.update(&state.config()).await {
                    Ok(report) => tracing::info!(
                        lists = report.lists.len(),
                        rules = report.network_rules,
                        "scheduled update finished"
                    ),
                    Err(error) => tracing::warn!(%error, "scheduled update failed"),
                }
            }
        });
    }

    // Shut down cleanly so systemd's ExecStop can restore DNS settings.
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => tracing::info!("interrupted; shutting down"),
        _ = sigterm.recv() => tracing::info!("SIGTERM; shutting down"),
    }
    Ok(())
}
