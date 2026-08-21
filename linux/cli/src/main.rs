//! `ratblocker` — the command-line client.
//!
//! Everything here goes through the daemon's D-Bus API. The CLI holds no
//! privileges of its own and never touches the rule database, the DNS
//! configuration or the filter files directly, so it cannot be used to work
//! around the daemon's authorization (§19).

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::Value;

const SERVICE: &str = "io.github.ratblocker.Service";
const PATH: &str = "/io/github/ratblocker/Service";
const INTERFACE: &str = "io.github.ratblocker.Service1";

#[derive(Parser)]
#[command(name = "ratblocker", version, about = "Control the RatBlocker daemon")]
struct Cli {
    /// Talk to a daemon on the session bus (development).
    #[arg(long, global = true)]
    session_bus: bool,
    /// Print raw JSON instead of a formatted summary.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show whether filtering is on, and what is loaded.
    Status,
    /// Turn filtering on.
    Enable,
    /// Turn filtering off.
    Disable,
    /// Suspend filtering for a while, e.g. `10m`, `2h`, `30s`.
    Pause { duration: String },
    /// Resume immediately after a pause.
    Resume,
    /// Download and activate fresh filter lists.
    Update,
    /// Rebuild the engine from the database already on disk.
    Reload,
    /// Local blocked-request counters.
    Stats,
    /// Inspect filter subscriptions.
    Filters {
        #[command(subcommand)]
        command: FilterCommand,
    },
    /// Manage allowed domains.
    Allow {
        #[command(subcommand)]
        command: AllowCommand,
    },
}

#[derive(Subcommand)]
enum FilterCommand {
    /// List the configured subscriptions.
    List,
    /// Subscribe to a filter list over https.
    Add { url: String },
    /// Remove a subscription by id.
    Remove { subscription_id: String },
}

#[derive(Subcommand)]
enum AllowCommand {
    /// List allowed domains.
    List,
    /// Stop filtering on a domain and its subdomains.
    Add { domain: String },
    /// Resume filtering on a domain.
    Remove { domain: String },
}

/// Parse `30s`, `10m`, `2h` or a bare number of seconds.
fn parse_duration(text: &str) -> Result<u64> {
    let text = text.trim();
    let (value, multiplier) = match text.chars().last() {
        Some('s') => (&text[..text.len() - 1], 1),
        Some('m') => (&text[..text.len() - 1], 60),
        Some('h') => (&text[..text.len() - 1], 3600),
        _ => (text, 1),
    };
    let n: u64 = value
        .parse()
        .with_context(|| format!("{text:?} is not a duration like 30s, 10m or 2h"))?;
    Ok(n * multiplier)
}

struct Client {
    proxy: zbus::Proxy<'static>,
}

impl Client {
    async fn connect(session: bool) -> Result<Self> {
        let connection = if session {
            zbus::Connection::session().await
        } else {
            zbus::Connection::system().await
        }
        .context("connecting to D-Bus")?;

        let proxy = zbus::Proxy::new(&connection, SERVICE, PATH, INTERFACE)
            .await
            .with_context(|| {
                format!("the RatBlocker daemon is not available on {SERVICE}. Is ratblockerd running?")
            })?;
        Ok(Self { proxy })
    }

    async fn call_json(&self, method: &str) -> Result<Value> {
        let text: String = self.proxy.call(method, &()).await.map_err(describe)?;
        serde_json::from_str(&text).with_context(|| format!("{method} returned invalid JSON"))
    }
}

/// Turn a D-Bus error into something worth reading.
fn describe(error: zbus::Error) -> anyhow::Error {
    match &error {
        zbus::Error::MethodError(name, message, _) if name.as_str().ends_with("AccessDenied") => {
            anyhow::anyhow!(
                "not authorized: {}",
                message.as_deref().unwrap_or("the request was refused by Polkit")
            )
        }
        _ => anyhow::anyhow!(error),
    }
}

fn duration_text(seconds: u64) -> String {
    match seconds {
        0 => "0s".into(),
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {}s", s / 60, s % 60),
        s => format!("{}h {}m", s / 3600, (s % 3600) / 60),
    }
}

fn print_status(status: &Value) {
    let enabled = status["enabled"].as_bool().unwrap_or(false);
    let paused = status["paused"].as_bool().unwrap_or(false);
    let active = status["filtering_active"].as_bool().unwrap_or(false);

    let state = if !enabled {
        "off".to_string()
    } else if paused {
        format!(
            "paused ({} remaining)",
            duration_text(status["pause_remaining_seconds"].as_u64().unwrap_or(0))
        )
    } else {
        "on".to_string()
    };

    println!("Protection   {state}");
    println!("Filtering    {}", if active { "active" } else { "not filtering" });
    let total = status["rules_loaded"].as_u64().unwrap_or(0);
    let dns_rules = status["dns_enforceable_rules"].as_u64().unwrap_or(0);
    println!("Rules        {total} loaded");
    // Worth stating plainly: a hostname-only layer cannot enforce a rule that
    // depends on the page, the resource type or the party (§25).
    println!("             {dns_rules} enforceable from a hostname alone (DNS layer)");
    if let Some(listen) = status["listen"].as_array() {
        let addresses: Vec<&str> = listen.iter().filter_map(Value::as_str).collect();
        println!("DNS proxy    {}", addresses.join(", "));
    }
    if let Some(upstreams) = status["upstreams"].as_array() {
        let list: Vec<&str> = upstreams.iter().filter_map(Value::as_str).collect();
        println!("Upstream     {}", list.join(", "));
    }
    let dns = &status["dns"];
    println!(
        "Queries      {} total, {} blocked, {} forwarded, {} errors",
        dns["queries"].as_u64().unwrap_or(0),
        dns["blocked"].as_u64().unwrap_or(0),
        dns["forwarded"].as_u64().unwrap_or(0),
        dns["errors"].as_u64().unwrap_or(0),
    );
    if let Some(sources) = status["sources"].as_array() {
        if !sources.is_empty() {
            println!("Lists");
            for source in sources {
                println!(
                    "  {:<16} {:<14} {} rules",
                    source["id"].as_str().unwrap_or("?"),
                    source["version"].as_str().unwrap_or("-"),
                    source["rule_count"].as_u64().unwrap_or(0),
                );
            }
        }
    }
    println!(
        "Versions     daemon {}, core {}",
        status["daemon_version"].as_str().unwrap_or("?"),
        status["core_version"].as_str().unwrap_or("?"),
    );
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = Client::connect(cli.session_bus).await?;

    match cli.command {
        Command::Status => {
            let status = client.call_json("GetStatus").await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                print_status(&status);
            }
        }

        Command::Enable | Command::Disable => {
            let enable = matches!(cli.command, Command::Enable);
            client
                .proxy
                .call::<_, _, ()>("SetEnabled", &(enable))
                .await
                .map_err(describe)?;
            println!("Protection {}", if enable { "on" } else { "off" });
        }

        Command::Pause { duration } => {
            let seconds = parse_duration(&duration)?;
            client
                .proxy
                .call::<_, _, ()>("Pause", &(seconds))
                .await
                .map_err(describe)?;
            println!("Paused for {}", duration_text(seconds));
        }

        Command::Resume => {
            client.proxy.call::<_, _, ()>("Pause", &(0u64)).await.map_err(describe)?;
            println!("Resumed");
        }

        Command::Update => {
            let text: String = client
                .proxy
                .call("UpdateFilters", &())
                .await
                .map_err(describe)?;
            let report: Value = serde_json::from_str(&text)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                for list in report["lists"].as_array().unwrap_or(&Vec::new()) {
                    match list["error"].as_str() {
                        Some(error) => println!(
                            "  {:<16} FAILED  {error}",
                            list["id"].as_str().unwrap_or("?")
                        ),
                        None => println!(
                            "  {:<16} {} rules from {}",
                            list["id"].as_str().unwrap_or("?"),
                            list["rules"].as_u64().unwrap_or(0),
                            list["source"].as_str().unwrap_or("-"),
                        ),
                    }
                }
                println!(
                    "{} network and {} cosmetic rules{}",
                    report["network_rules"].as_u64().unwrap_or(0),
                    report["cosmetic_rules"].as_u64().unwrap_or(0),
                    if report["activated"].as_bool().unwrap_or(false) {
                        " activated"
                    } else {
                        " (not activated)"
                    }
                );
                if let Some(message) = report["message"].as_str() {
                    println!("{message}");
                }
            }
        }

        Command::Reload => {
            client.proxy.call::<_, _, ()>("ReloadFilters", &()).await.map_err(describe)?;
            println!("Filters reloaded");
        }

        Command::Stats => {
            let stats = client.call_json("GetStatistics").await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&stats)?);
            } else if stats["requests_seen"].as_u64().unwrap_or(0) == 0 {
                println!("No counts recorded. Local statistics are off by default.");
            } else {
                println!("Seen     {}", stats["requests_seen"].as_u64().unwrap_or(0));
                println!("Blocked  {}", stats["requests_blocked"].as_u64().unwrap_or(0));
            }
        }

        Command::Filters { command } => match command {
            FilterCommand::List => {
                let config = client.call_json("GetConfiguration").await?;
                let subs = config["filter_subscriptions"].as_array().cloned().unwrap_or_default();
                if subs.is_empty() {
                    println!("No filter subscriptions.");
                }
                for sub in subs {
                    println!(
                        "{:<18} {:<9} {:<8} {}",
                        sub["id"].as_str().unwrap_or("?"),
                        if sub["enabled"].as_bool().unwrap_or(false) { "enabled" } else { "disabled" },
                        if sub["trusted"].as_bool().unwrap_or(false) { "trusted" } else { "untrusted" },
                        sub["url"].as_str().unwrap_or("bundled"),
                    );
                }
            }
            FilterCommand::Add { url } => {
                let id: String = client
                    .proxy
                    .call("AddFilterSubscription", &(url.as_str()))
                    .await
                    .map_err(describe)?;
                println!("Subscribed as {id}");
                println!("It is untrusted until you enable it explicitly; untrusted lists are not downloaded.");
            }
            FilterCommand::Remove { subscription_id } => {
                client
                    .proxy
                    .call::<_, _, ()>("RemoveFilterSubscription", &(subscription_id.as_str()))
                    .await
                    .map_err(describe)?;
                println!("Removed {subscription_id}");
            }
        },

        Command::Allow { command } => match command {
            AllowCommand::List => {
                let config = client.call_json("GetConfiguration").await?;
                let domains = config["allowlisted_domains"].as_array().cloned().unwrap_or_default();
                if domains.is_empty() {
                    println!("No allowed domains.");
                }
                for domain in domains {
                    println!("{}", domain.as_str().unwrap_or("?"));
                }
            }
            AllowCommand::Add { domain } => {
                client
                    .proxy
                    .call::<_, _, ()>("AddAllowlistDomain", &(domain.as_str()))
                    .await
                    .map_err(describe)?;
                println!("Filtering is off for {domain} and its subdomains");
            }
            AllowCommand::Remove { domain } => {
                client
                    .proxy
                    .call::<_, _, ()>("RemoveAllowlistDomain", &(domain.as_str()))
                    .await
                    .map_err(describe)?;
                println!("Filtering resumed for {domain}");
            }
        },
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_duration;

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration("30s").unwrap(), 30);
        assert_eq!(parse_duration("10m").unwrap(), 600);
        assert_eq!(parse_duration("2h").unwrap(), 7200);
        assert_eq!(parse_duration("45").unwrap(), 45);
        assert!(parse_duration("soon").is_err());
    }
}
