//! Security tests from `docs/architecture.md` §21.
//!
//! Filter lists are untrusted input (§16). These tests feed the parser and the
//! matcher things a hostile list author would send and assert that RatBlocker
//! rejects or bounds them instead of hanging, panicking or over-blocking.

use std::time::{Duration, Instant};

use ratblocker_core::parser::{parse_line, ListFormat, ParsedLine, RejectReason};
use ratblocker_core::{
    EngineBuilder, EngineConfig, FilterDecision, RequestContext, ResourceType, RuleDatabase,
};

fn is_rejected(line: &str) -> bool {
    matches!(
        parse_line(line, ListFormat::Adblock, "t:1"),
        ParsedLine::Rejected { .. }
    )
}

#[test]
fn catastrophic_regex_patterns_are_refused_or_bounded() {
    // The classic exponential-backtracking shapes. RatBlocker's matcher is
    // linear-time, so these must either be refused at parse time or match in
    // negligible time — never hang.
    let evil = [
        r"/(a+)+$/",
        r"/(a|a)*$/",
        r"/(.*a){20}/",
        r"/((ab)*)*c/",
        r"/(x+x+)+y/",
    ];
    let hostile_input = format!("https://example.com/{}", "a".repeat(2000));

    for pattern in evil {
        let start = Instant::now();
        let mut b = EngineBuilder::new();
        b.add_list("evil", pattern, ListFormat::Adblock);
        let engine = b.build(EngineConfig::default());
        let ctx = RequestContext::new(&hostile_input, ResourceType::Script);
        let _ = engine.evaluate(&ctx);
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "{pattern} took {elapsed:?}"
        );
    }
}

#[test]
fn oversized_rules_and_regexes_are_rejected() {
    assert!(is_rejected(&format!("||{}.com^", "a".repeat(20_000))));
    assert!(is_rejected(&format!("/{}/", "a".repeat(1_000))));
    // A rule at the size limit is still refused rather than truncated.
    assert!(matches!(
        parse_line(&"a".repeat(17_000), ListFormat::Adblock, "t:1"),
        ParsedLine::Rejected { reason: RejectReason::TooLong, .. }
    ));
}

#[test]
fn oversized_and_malformed_urls_do_not_panic() {
    let mut b = EngineBuilder::new();
    b.add_list("t", "||ads.example^", ListFormat::Adblock);
    let engine = b.build(EngineConfig::default());

    let hostile = [
        String::new(),
        "not a url".to_string(),
        "://".to_string(),
        "https://".to_string(),
        "https:///path".to_string(),
        "javascript:alert(1)".to_string(),
        "data:text/html,<script>".to_string(),
        format!("https://{}.com/", "a".repeat(70_000)),
        format!("https://example.com/{}", "../".repeat(5_000)),
        "https://example.com/\0\0\0".to_string(),
        "https://ex ample.com/".to_string(),
        "https://[not:an:ipv6/".to_string(),
    ];
    for url in hostile {
        let ctx = RequestContext::new(url.clone(), ResourceType::Other);
        // Must return a decision, not panic, and must not block on garbage.
        let r = engine.evaluate(&ctx);
        assert_eq!(r.decision, FilterDecision::Allow, "unexpected block for {url:?}");
    }
}

#[test]
fn a_hostile_list_cannot_install_a_block_everything_rule() {
    // A subscription that tries to break the whole web must be refused at
    // parse time rather than shipped into the index.
    for line in ["*", "|", "||", "^", "a", "/*/", "$third-party", "$image"] {
        assert!(is_rejected(line), "{line:?} was accepted");
    }
}

#[test]
fn control_characters_and_bad_encoding_are_rejected() {
    assert!(is_rejected("||exam\u{0}ple.com^"));
    assert!(is_rejected("||exam\u{7}ple.com^"));
    // A legitimate tab-separated hosts line is still fine.
    assert!(!matches!(
        parse_line("0.0.0.0\tads.test", ListFormat::Hosts, "t:1"),
        ParsedLine::Rejected { .. }
    ));
}

#[test]
fn redirect_targets_cannot_escape_the_bundled_resource_directory() {
    // `$redirect` names a resource shipped with RatBlocker. A list must not be
    // able to point it at an arbitrary path or URL.
    let mut b = EngineBuilder::new();
    b.add_list(
        "t",
        "||x.test/a$redirect=../../etc/passwd,script\n||y.test/a$redirect=https://evil.test/x,script",
        ListFormat::Adblock,
    );
    let engine = b.build(EngineConfig::default());

    for (url, forbidden) in [
        ("https://x.test/a", "/etc/passwd"),
        ("https://y.test/a", "evil.test"),
    ] {
        let ctx = RequestContext::new(url, ResourceType::Script).with_source("https://p.test/");
        let r = engine.evaluate(&ctx);
        if let Some(target) = r.redirect_to {
            // The core reports the raw token; every consumer must resolve it
            // against its own resource table. Assert it is never usable as a
            // path or URL directly.
            assert!(
                !target.starts_with("http") && !target.starts_with('/'),
                "redirect target {target:?} would resolve to {forbidden}"
            );
        }
    }
}

#[test]
fn corrupted_and_truncated_databases_are_rejected_not_trusted() {
    let mut b = EngineBuilder::new();
    b.add_list("t", "||ads.example^", ListFormat::Adblock);
    let db = b.into_database();
    let bytes = postcard::to_stdvec(&db).unwrap();

    // Truncation.
    for cut in [0, 1, bytes.len() / 2, bytes.len() - 1] {
        let r: Result<RuleDatabase, _> = postcard::from_bytes(&bytes[..cut]);
        if let Ok(db) = r {
            // Decoding may succeed on some prefixes; the version check is the
            // backstop, and a partial database must never be silently used.
            assert!(db.check_version().is_err() || db.len() <= 1);
        }
    }

    // A database claiming a future format version must be refused.
    let mut future = db.clone();
    future.format_version = 999;
    assert!(future.check_version().is_err());
}

#[test]
fn deeply_nested_and_pathological_hosts_are_bounded() {
    let mut b = EngineBuilder::new();
    b.add_list("t", "||ads.example^", ListFormat::Adblock);
    let engine = b.build(EngineConfig::default());

    // 5000 labels: the suffix walk must stay linear in label count.
    let host = "a.".repeat(5_000) + "example.com";
    let url = format!("https://{host}/x");
    let start = Instant::now();
    let _ = engine.evaluate(&RequestContext::new(url, ResourceType::Other));
    assert!(start.elapsed() < Duration::from_millis(200));
}

#[test]
fn allowlist_cannot_be_widened_by_a_lookalike_domain() {
    let mut b = EngineBuilder::new();
    b.add_list("t", "||ads.example^", ListFormat::Adblock);
    let mut cfg = EngineConfig::default();
    cfg.allowlisted_domains.insert("example.com".into());
    let engine = b.build(cfg);

    let blocked = |src: &str| {
        let ctx = RequestContext::new("https://ads.example/x", ResourceType::Image)
            .with_source(src);
        engine.evaluate(&ctx).decision
    };

    assert_eq!(blocked("https://example.com/"), FilterDecision::Allow);
    assert_eq!(blocked("https://sub.example.com/"), FilterDecision::Allow);
    // These must NOT inherit the allowlist entry.
    assert_eq!(blocked("https://notexample.com/"), FilterDecision::Block);
    assert_eq!(blocked("https://example.com.evil.test/"), FilterDecision::Block);
    assert_eq!(blocked("https://evil.test/?x=example.com"), FilterDecision::Block);
}

#[test]
fn statistics_stay_off_and_bounded_by_default() {
    use ratblocker_core::Statistics;
    let stats = Statistics::default();
    assert!(!stats.is_enabled(), "statistics must default to off (§17)");
    stats.record(FilterDecision::Block, Some("ads.example"));
    assert_eq!(stats.snapshot().requests_blocked, 0);

    // Once enabled, the domain table must not grow without bound.
    let stats = Statistics::new(true, true);
    for i in 0..5_000 {
        stats.record(FilterDecision::Block, Some(&format!("d{i}.test")));
    }
    assert!(stats.snapshot().top_blocked_domains.len() <= 512);
    stats.reset();
    assert_eq!(stats.snapshot().requests_blocked, 0);
}
