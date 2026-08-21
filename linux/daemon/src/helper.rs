//! `ratblocker-helper` — the minimal privileged helper.
//!
//! Per `docs/architecture.md` §16 this exposes only narrowly defined
//! operations and accepts no arbitrary paths or commands. It takes a
//! subcommand and nothing else: the listen address it installs is read from
//! the daemon's own settings file at a fixed location, not from the caller,
//! so a caller who can run the helper still cannot choose what it writes.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use ratblocker_daemon::config::DaemonSettings;

/// The only settings file the helper will read.
const SETTINGS: &str = "/etc/ratblocker/daemon.yaml";
/// Transient drop-in: it lives under /run, so a reboot restores stock DNS
/// even if the helper never gets to run its restore step.
const DROPIN_DIR: &str = "/run/systemd/resolved.conf.d";
const DROPIN: &str = "/run/systemd/resolved.conf.d/ratblocker.conf";

#[derive(Parser)]
#[command(name = "ratblocker-helper", version, about = "RatBlocker privileged helper")]
struct Cli {
    #[command(subcommand)]
    command: Op,
}

#[derive(Subcommand)]
enum Op {
    /// Point systemd-resolved at the RatBlocker DNS proxy.
    DnsApply,
    /// Restore the DNS configuration RatBlocker replaced.
    DnsRestore,
    /// Print what `dns-apply` would do, and change nothing.
    DnsShow,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Op::DnsApply => dns_apply(false),
        Op::DnsShow => dns_apply(true),
        Op::DnsRestore => dns_restore(),
    }
}

/// The address the daemon listens on, validated to be loopback.
fn listen_address() -> Result<String> {
    let settings = DaemonSettings::load(Path::new(SETTINGS))
        .with_context(|| format!("reading {SETTINGS}"))?;
    let addr = settings
        .listen
        .first()
        .context("no listen address configured")?;
    if !addr.ip().is_loopback() {
        // Belt and braces: `DaemonSettings::validate` already refuses this.
        bail!("refusing to point system DNS at the non-loopback address {addr}");
    }
    Ok(addr.ip().to_string())
}

fn dns_apply(dry_run: bool) -> Result<()> {
    let address = listen_address()?;
    let contents = format!(
        "# Written by ratblocker-helper. Transient: removed on reboot.\n\
         [Resolve]\n\
         DNS={address}\n\
         Domains=~.\n"
    );

    if dry_run {
        println!("would write {DROPIN}:\n{contents}");
        println!("would run: systemctl try-restart systemd-resolved");
        return Ok(());
    }

    std::fs::create_dir_all(DROPIN_DIR).with_context(|| format!("creating {DROPIN_DIR}"))?;
    write_atomic(Path::new(DROPIN), contents.as_bytes())?;
    restart_resolved()?;
    println!("system DNS now goes through RatBlocker at {address}");
    Ok(())
}

fn dns_restore() -> Result<()> {
    let path = Path::new(DROPIN);
    if path.exists() {
        std::fs::remove_file(path).with_context(|| format!("removing {DROPIN}"))?;
    }
    restart_resolved()?;
    println!("system DNS configuration restored");
    Ok(())
}

fn restart_resolved() -> Result<()> {
    // `try-restart` is a no-op when resolved is not running, which is the
    // right behaviour on a system that uses something else.
    let status = Command::new("systemctl")
        .args(["try-restart", "systemd-resolved"])
        .status()
        .context("running systemctl")?;
    if !status.success() {
        bail!("systemctl try-restart systemd-resolved failed: {status}");
    }
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp: PathBuf = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}
