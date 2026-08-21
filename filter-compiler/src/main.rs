//! `ratblocker-compile` — turns filter lists into RatBlocker's rule database
//! and the per-platform rulesets each frontend needs.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use ratblocker_core::parser::ListFormat;
use ratblocker_core::rule_engine::rules::NetworkRule;
use ratblocker_core::rule_engine::{dnr, EngineBuilder};
use ratblocker_core::RuleDatabase;
use sha2::{Digest, Sha256};

/// Refuse to compile a single list larger than this (§5: oversized filter
/// lists must be rejected).
const MAX_LIST_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Parser)]
#[command(name = "ratblocker-compile", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compile lists into a rule database and per-platform rulesets.
    Build {
        /// `id=path` — repeatable.
        #[arg(long = "list", value_name = "ID=PATH", required = true)]
        lists: Vec<String>,
        /// Directory to write outputs into.
        #[arg(long, default_value = "dist")]
        out: PathBuf,
        /// Also emit a human-readable report of rejected lines.
        #[arg(long)]
        report_rejects: bool,
    },
    /// Parse lists and report coverage without writing anything.
    Stats {
        #[arg(long = "list", value_name = "ID=PATH", required = true)]
        lists: Vec<String>,
    },
    /// Explain the decision for one or more URLs against a compiled database.
    Match {
        /// Path to a compiled `rules.rbdb`.
        #[arg(long, default_value = "dist/rules.rbdb")]
        database: PathBuf,
        /// The page the request originates from.
        #[arg(long)]
        source: Option<String>,
        /// Resource type, e.g. script, image, document.
        #[arg(long, default_value = "other")]
        resource_type: String,
        /// URLs to test.
        #[arg(required = true)]
        urls: Vec<String>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Build { lists, out, report_rejects } => build(&lists, &out, report_rejects),
        Command::Stats { lists } => stats(&lists),
        Command::Match { database, source, resource_type, urls } => {
            explain(&database, source.as_deref(), &resource_type, &urls)
        }
    }
}

/// Load a database and report what the engine decides, and why.
fn explain(database: &Path, source: Option<&str>, resource_type: &str, urls: &[String]) -> Result<()> {
    let bytes = fs::read(database)
        .with_context(|| format!("cannot read {}", database.display()))?;
    let db: RuleDatabase = postcard::from_bytes(&bytes).context("decoding rule database")?;
    let engine = ratblocker_core::Engine::from_database(db, "", Default::default())?;

    let rt = match resource_type {
        "document" => ratblocker_core::ResourceType::Document,
        "script" => ratblocker_core::ResourceType::Script,
        "image" => ratblocker_core::ResourceType::Image,
        "stylesheet" => ratblocker_core::ResourceType::Stylesheet,
        "font" => ratblocker_core::ResourceType::Font,
        "media" => ratblocker_core::ResourceType::Media,
        "xhr" | "xmlhttprequest" => ratblocker_core::ResourceType::XmlHttpRequest,
        "websocket" => ratblocker_core::ResourceType::WebSocket,
        other => bail!("unknown resource type {other:?}"),
    };

    for url in urls {
        let mut ctx = ratblocker_core::RequestContext::new(url.clone(), rt);
        ctx.source_url = source.map(str::to_string);
        let r = engine.evaluate(&ctx);
        let rule = r
            .matched_rule_id
            .as_deref()
            .and_then(|id| find_rule_text(&engine, id))
            .unwrap_or_else(|| r.matched_rule_id.clone().unwrap_or_else(|| "-".into()));
        println!("{:<18?} {url}", r.decision);
        println!("{:<18} {rule}", "");
        if let Some(rewritten) = &r.rewritten_url {
            println!("{:<18} -> {rewritten}", "");
        }
    }
    Ok(())
}

/// Recover the original rule text behind a rule id, for readable diagnostics.
fn find_rule_text(engine: &ratblocker_core::Engine, id: &str) -> Option<String> {
    if id == "allowlist" || id.starts_with("app-policy:") {
        return Some(id.to_string());
    }
    engine
        .all_rules()
        .find(|r| r.id == id)
        .map(|r| format!("{}  [{}]", r.raw, r.id))
}

struct Loaded {
    id: String,
    path: PathBuf,
    content: String,
    checksum: String,
    format: ListFormat,
}

fn load_lists(specs: &[String]) -> Result<Vec<Loaded>> {
    let mut out = Vec::new();
    for spec in specs {
        let (id, path) = spec
            .split_once('=')
            .with_context(|| format!("expected ID=PATH, got {spec:?}"))?;
        let path = PathBuf::from(path);
        let meta = fs::metadata(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        if meta.len() > MAX_LIST_BYTES {
            bail!(
                "{} is {} bytes, over the {MAX_LIST_BYTES} byte limit",
                path.display(),
                meta.len()
            );
        }
        let bytes = fs::read(&path)?;
        // Non-UTF-8 input is an unsupported encoding, not something to guess at.
        let content = String::from_utf8(bytes)
            .with_context(|| format!("{} is not valid UTF-8", path.display()))?;
        let checksum = format!("{:x}", Sha256::digest(content.as_bytes()));
        let format = ListFormat::detect(&content);
        out.push(Loaded { id: id.to_string(), path, content, checksum, format });
    }
    Ok(out)
}

fn assemble(lists: &[Loaded]) -> (RuleDatabase, BTreeMap<String, usize>, BTreeMap<String, Vec<String>>) {
    let mut builder = EngineBuilder::new();
    for l in lists {
        builder.add_list(&l.id, &l.content, l.format);
        builder.set_source_provenance(&l.id, None, Some(l.checksum.clone()));
    }
    // Group rejections by reason so a systematic parser gap is obvious.
    let mut by_reason: BTreeMap<String, usize> = BTreeMap::new();
    let mut samples: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for r in builder.rejected() {
        let key = match &r.reason {
            ratblocker_core::RejectReason::UnknownOption(o) => format!("unknown option: {o}"),
            ratblocker_core::RejectReason::UnsupportedOption(o) => format!("unsupported option: {o}"),
            ratblocker_core::RejectReason::MalformedDomain(_) => "malformed domain".to_string(),
            other => other.to_string(),
        };
        *by_reason.entry(key.clone()).or_default() += 1;
        let bucket = samples.entry(key).or_default();
        if bucket.len() < 5 {
            bucket.push(format!("{}:{} {}", r.list_id, r.line_number, r.raw));
        }
    }
    (builder.into_database(), by_reason, samples)
}

fn stats(specs: &[String]) -> Result<()> {
    let lists = load_lists(specs)?;
    for l in &lists {
        println!("{:<14} {:?}  {}", l.id, l.format, l.path.display());
    }
    let (db, by_reason, samples) = assemble(&lists);
    let s = &db.stats;
    let total_lines: usize = lists.iter().map(|l| l.content.lines().count()).sum();
    let accepted = s.network_rules + s.exception_rules + s.removeparam_rules + s.cosmetic_rules;
    println!("\nlines read        {total_lines}");
    println!("network rules     {}", s.network_rules);
    println!("exception rules   {}", s.exception_rules);
    println!("removeparam rules {}", s.removeparam_rules);
    println!("cosmetic rules    {}", s.cosmetic_rules);
    println!("badfilter applied {}", s.badfilter_applied);
    println!("rejected          {} ({:.2}% of non-comment lines)",
        s.rejected,
        100.0 * s.rejected as f64 / (accepted + s.rejected).max(1) as f64);
    println!("\nrejections by reason:");
    let mut sorted: Vec<_> = by_reason.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(a.1));
    for (reason, n) in sorted {
        println!("  {n:>7}  {reason}");
        for line in samples.get(reason).map(Vec::as_slice).unwrap_or(&[]) {
            let line: String = line.chars().take(140).collect();
            println!("           | {line}");
        }
    }
    Ok(())
}

fn build(specs: &[String], out: &Path, report_rejects: bool) -> Result<()> {
    let lists = load_lists(specs)?;
    let (db, by_reason, samples) = assemble(&lists);

    fs::create_dir_all(out)?;
    fs::create_dir_all(out.join("chromium"))?;

    // 1. Native database, consumed by the daemon, Android and the Firefox
    //    extension's WASM core.
    let encoded = postcard::to_stdvec(&db).context("serializing rule database")?;
    write_atomic(&out.join("rules.rbdb"), &encoded)?;
    let db_checksum = format!("{:x}", Sha256::digest(&encoded));

    // 2. Chromium: DNR handles ordinary requests; the core handles cosmetics
    //    and popup-context rules. Keep this database small because an MV3
    //    service worker rebuilds its indexes whenever it is woken.
    let dnr = emit_chromium(&db, &out.join("chromium"))?;
    let cosmetic_db = RuleDatabase {
        format_version: db.format_version,
        sources: db.sources.clone(),
        network: db.network.iter()
            .filter(|r| r.options.popup.is_some())
            .cloned()
            .collect(),
        // Popup blocks need normal exception precedence; scoped exceptions
        // also govern cosmetic filtering.
        exceptions: db.exceptions.clone(),
        removeparam: Vec::new(),
        cosmetic: db.cosmetic.clone(),
        stats: db.stats.clone(),
    };
    let cosmetic_encoded = postcard::to_stdvec(&cosmetic_db)?;
    write_atomic(&out.join("chromium/cosmetic.rbdb"), &cosmetic_encoded)?;

    // 3. Neutral stand-in resources named by `$redirect` rules.
    let redirect_dir = out.join("redirects");
    fs::create_dir_all(&redirect_dir)?;
    let mut redirect_files = Vec::new();
    for (file, bytes) in redirect_payloads() {
        write_atomic(&redirect_dir.join(file), &bytes)?;
        redirect_files.push(file);
    }

    // 4. Metadata, checksums and attribution.
    let metadata = serde_json::json!({
        "format_version": db.format_version,
        "rules": {
            "network": db.network.len(),
            "exceptions": db.exceptions.len(),
            "removeparam": db.removeparam.len(),
            "cosmetic": db.cosmetic.len(),
        },
        "rejected": db.stats.rejected,
        "rejected_by_reason": by_reason,
        "database": { "file": "rules.rbdb", "sha256": db_checksum, "bytes": encoded.len() },
        "chromium": dnr,
        "chromium_cosmetic_database": {
            "file": "chromium/cosmetic.rbdb",
            "sha256": format!("{:x}", Sha256::digest(&cosmetic_encoded)),
            "bytes": cosmetic_encoded.len(),
            "cosmetic_rules": cosmetic_db.cosmetic.len(),
            "exceptions": cosmetic_db.exceptions.len(),
            "popup_rules": cosmetic_db.network.len(),
        },
        "sources": db.sources,
        "redirect_resources": redirect_files,
    });
    write_atomic(
        &out.join("metadata.json"),
        serde_json::to_string_pretty(&metadata)?.as_bytes(),
    )?;
    write_atomic(&out.join("ATTRIBUTION.txt"), db.attribution().as_bytes())?;

    if report_rejects {
        let mut text = String::new();
        for (reason, n) in &by_reason {
            text.push_str(&format!("{n:>7}  {reason}\n"));
            for line in samples.get(reason).map(Vec::as_slice).unwrap_or(&[]) {
                text.push_str(&format!("         | {line}\n"));
            }
        }
        write_atomic(&out.join("rejected.txt"), text.as_bytes())?;
    }

    println!("wrote {}", out.display());
    println!(
        "  rules.rbdb          {:>9} bytes  ({} network, {} exception, {} removeparam, {} cosmetic)",
        encoded.len(),
        db.network.len(),
        db.exceptions.len(),
        db.removeparam.len(),
        db.cosmetic.len()
    );
    println!("  chromium rulesets   {}", dnr["rulesets"].as_array().map_or(0, |a| a.len()));
    println!(
        "  chromium rules      {} total = {} collapsed-domain + {} individual",
        dnr["kept"], dnr["collapsed_into_rules"], dnr["individual_rules"]
    );
    println!(
        "  chromium coverage   {} of {} candidates ({} domains collapsed, {} dropped)",
        dnr["kept"].as_u64().unwrap_or(0) + dnr["collapsed_domains"].as_u64().unwrap_or(0)
            - dnr["collapsed_into_rules"].as_u64().unwrap_or(0),
        dnr["candidates"],
        dnr["collapsed_domains"],
        dnr["dropped_over_budget"]
    );
    println!(
        "  cosmetic.rbdb       {:>9} bytes  ({} cosmetic, {} popup, {} exceptions)",
        cosmetic_encoded.len(),
        cosmetic_db.cosmetic.len(),
        cosmetic_db.network.len(),
        cosmetic_db.exceptions.len()
    );
    Ok(())
}

/// Convert to DNR under Chromium's limits.
///
/// Plain domain blocks are collapsed into a few `requestDomains` rules first,
/// which is what makes the bulk of EasyList fit at all. Whatever is left is
/// converted individually, in a documented priority order: exceptions first
/// (dropping one causes visible breakage), then modifier rules, then the
/// remaining blocks. Anything that still does not fit is counted, not hidden.
fn emit_chromium(db: &RuleDatabase, dir: &Path) -> Result<serde_json::Value> {
    /// Domains per collapsed rule. Small enough to keep each rule's JSON
    /// modest, large enough that the whole corpus needs only a few dozen.
    const DOMAIN_CHUNK: usize = 2_500;

    // 1. Collapse the plain domain blocks.
    let mut domains: Vec<String> = Vec::new();
    let mut individual: Vec<&NetworkRule> = Vec::new();
    for rule in &db.network {
        match dnr::collapsible_domain(rule) {
            Some(host) => domains.push(host.to_string()),
            None => individual.push(rule),
        }
    }
    domains.sort_unstable();
    domains.dedup();
    // Drop any domain already covered by a shorter parent entry: DNR's
    // requestDomains matches subdomains, so `ads.example.com` next to
    // `example.com` is dead weight.
    let mut covered: Vec<String> = Vec::with_capacity(domains.len());
    for domain in &domains {
        let redundant = covered.last().is_some_and(|parent: &String| {
            domain.len() > parent.len()
                && domain.ends_with(parent.as_str())
                && domain.as_bytes()[domain.len() - parent.len() - 1] == b'.'
        });
        if !redundant {
            covered.push(domain.clone());
        }
    }
    // Parent-first ordering is needed for the check above to see the parent.
    covered.sort_by(|a, b| a.split('.').rev().cmp(b.split('.').rev()));
    let collapsed_domains = covered;

    let mut rules = dnr::collapse_domains(&collapsed_domains, 1, DOMAIN_CHUNK);
    let mut next_id = rules.len() as u32 + 1;

    // 2. Everything else, most important first.
    let mut ordered: Vec<&NetworkRule> = Vec::new();
    ordered.extend(db.exceptions.iter());
    ordered.extend(db.removeparam.iter());
    ordered.extend(individual.iter().copied().filter(|r| r.options.redirect.is_some()));
    ordered.extend(individual.iter().copied().filter(|r| r.options.important));
    ordered.extend(
        individual
            .iter()
            .copied()
            .filter(|r| r.host_anchor.is_some() && r.options.redirect.is_none() && !r.options.important),
    );
    ordered.extend(
        individual
            .iter()
            .copied()
            .filter(|r| r.host_anchor.is_none() && r.options.redirect.is_none() && !r.options.important),
    );

    let candidates = ordered.len();
    let mut regex_budget = dnr::MAX_REGEX_RULES;
    let mut unrepresentable: BTreeMap<String, usize> = BTreeMap::new();
    let mut over_budget = 0usize;

    for rule in ordered {
        if rules.len() >= dnr::MAX_STATIC_RULES {
            over_budget += 1;
            continue;
        }
        match dnr::convert(rule, next_id, &mut regex_budget) {
            Ok(converted) => {
                rules.push(converted);
                next_id += 1;
            }
            Err(why) => {
                *unrepresentable.entry(format!("{why:?}")).or_default() += 1;
            }
        }
    }

    // 3. Split across rulesets; Chromium caps how many may be enabled at once,
    //    so keep the count small and the chunks large.
    const CHUNK: usize = 10_000;
    let mut rulesets = Vec::new();
    for (i, chunk) in rules.chunks(CHUNK).enumerate() {
        let name = format!("ruleset_{i}");
        let file = format!("{name}.json");
        write_atomic(&dir.join(&file), serde_json::to_string(chunk)?.as_bytes())?;
        rulesets.push(serde_json::json!({
            "id": name,
            "enabled": true,
            "path": format!("rules/{file}"),
            "rule_count": chunk.len(),
        }));
    }

    if over_budget > 0 {
        println!("  note: {over_budget} Chromium rules did not fit the static budget");
    }

    Ok(serde_json::json!({
        "candidates": candidates + collapsed_domains.len(),
        "kept": rules.len(),
        "collapsed_domains": collapsed_domains.len(),
        "collapsed_into_rules": (collapsed_domains.len() + DOMAIN_CHUNK - 1) / DOMAIN_CHUNK,
        "individual_rules": rules.len() - (collapsed_domains.len() + DOMAIN_CHUNK - 1) / DOMAIN_CHUNK,
        "dropped_over_budget": over_budget,
        "unrepresentable": unrepresentable,
        "regex_rules_used": dnr::MAX_REGEX_RULES - regex_budget,
        "limits": {
            "max_static_rules": dnr::MAX_STATIC_RULES,
            "max_regex_rules": dnr::MAX_REGEX_RULES,
            "domains_per_collapsed_rule": DOMAIN_CHUNK,
        },
        "rulesets": rulesets,
    }))
}

/// Contents of the neutral stand-in resources. Each is the smallest valid
/// file of its type, so a blocked resource fails quietly instead of throwing.
fn redirect_payloads() -> Vec<(&'static str, Vec<u8>)> {
    // A 1x1 fully transparent GIF.
    const GIF: &[u8] = &[
        0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0x00,
        0x00, 0x00, 0xff, 0xff, 0xff, 0x21, 0xf9, 0x04, 0x01, 0x00, 0x00, 0x00, 0x00, 0x2c,
        0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x01, 0x44, 0x00, 0x3b,
    ];
    vec![
        ("noop.js", b"".to_vec()),
        ("noop.html", b"<!doctype html><title></title>".to_vec()),
        ("noop.css", b"".to_vec()),
        ("noop.txt", b"".to_vec()),
        ("noop.gif", GIF.to_vec()),
        ("noop.mp4", Vec::new()),
        ("noop.mp3", Vec::new()),
    ]
}

/// Write via a temporary file and rename, so a reader never sees a partial
/// file and a failed write leaves the previous version intact (§15).
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("new")
    ));
    fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

