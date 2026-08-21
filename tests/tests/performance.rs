//! Performance targets from `docs/architecture.md` §21.
//!
//! These assert budgets that are generous relative to measured behaviour, so
//! they catch a regression in complexity rather than machine-to-machine noise.

use std::time::Instant;

use ratblocker_core::{
    Engine, EngineConfig, RequestContext, ResourceType, RuleDatabase,
};
use ratblocker_tests::{compiled_database, URL_CORPUS};

fn load() -> Option<Engine> {
    let path = compiled_database()?;
    let bytes = std::fs::read(path).ok()?;
    let db: RuleDatabase = postcard::from_bytes(&bytes).expect("database decodes");
    Some(Engine::from_database(db, "", EngineConfig::default()).expect("database version"))
}

#[test]
fn engine_builds_and_decides_within_budget() {
    let Some(engine) = load() else {
        eprintln!("skipping: dist/rules.rbdb not built");
        return;
    };

    println!("indexed rules: {}", engine.rule_count());
    assert!(
        engine.rule_count() > 50_000,
        "expected a realistic corpus, got {}",
        engine.rule_count()
    );

    // Warm up so the first-call page faults do not skew the measurement.
    for (url, src) in URL_CORPUS {
        let mut ctx = RequestContext::new(*url, ResourceType::Script);
        if !src.is_empty() {
            ctx.source_url = Some(src.to_string());
        }
        engine.evaluate(&ctx);
    }

    const ITERATIONS: usize = 200;
    let start = Instant::now();
    let mut blocked = 0usize;
    for _ in 0..ITERATIONS {
        for (url, src) in URL_CORPUS {
            let mut ctx = RequestContext::new(*url, ResourceType::Script);
            if !src.is_empty() {
                ctx.source_url = Some(src.to_string());
            }
            if engine.evaluate(&ctx).decision.is_intervention() {
                blocked += 1;
            }
        }
    }
    let total = start.elapsed();
    let decisions = ITERATIONS * URL_CORPUS.len();
    let per_decision = total / decisions as u32;

    println!(
        "{decisions} decisions in {total:?} ({per_decision:?} each, {} interventions)",
        blocked / ITERATIONS
    );

    // §21 requires decisions below 10ms. A single decision should be orders of
    // magnitude faster than that; anything approaching 1ms means the index has
    // stopped doing its job and requests are falling back to a scan.
    assert!(
        per_decision.as_micros() < 1_000,
        "decision took {per_decision:?}, budget is 1ms"
    );

    // The corpus deliberately contains well-known trackers; if none of them
    // match, the index is silently broken rather than merely slow.
    assert!(blocked > 0, "no requests matched a rule");
}

#[test]
fn index_construction_is_fast_enough_for_daemon_startup() {
    let Some(path) = compiled_database() else {
        eprintln!("skipping: dist/rules.rbdb not built");
        return;
    };
    let bytes = std::fs::read(path).unwrap();

    let start = Instant::now();
    let db: RuleDatabase = postcard::from_bytes(&bytes).unwrap();
    let decode = start.elapsed();

    let start = Instant::now();
    let engine = Engine::from_database(db, "", EngineConfig::default()).unwrap();
    let index = start.elapsed();

    println!("decode {decode:?}, index {index:?}, rules {}", engine.rule_count());
    // §21: fast daemon startup. Two seconds is a ceiling, not a goal.
    assert!(
        (decode + index).as_secs_f64() < 2.0,
        "startup took {:?}",
        decode + index
    );
}

#[test]
fn matching_does_not_degrade_with_url_length() {
    // A quadratic matcher would blow up on a long URL. Compare a short URL
    // against a very long one and require the cost to stay proportionate.
    let Some(engine) = load() else {
        eprintln!("skipping: dist/rules.rbdb not built");
        return;
    };
    let short = "https://example.com/a";
    let long = format!("https://example.com/a?{}", "k=v&".repeat(500));

    let time = |url: &str| {
        let ctx = RequestContext::new(url, ResourceType::Script);
        let start = Instant::now();
        for _ in 0..200 {
            engine.evaluate(&ctx);
        }
        start.elapsed()
    };

    let t_short = time(short);
    let t_long = time(&long);
    println!("short {t_short:?}, long {t_long:?}");
    // The long URL is ~100x the length; allow generous headroom but reject
    // anything that looks super-linear.
    assert!(
        t_long.as_secs_f64() < t_short.as_secs_f64() * 250.0 + 0.5,
        "long-URL matching scaled badly: {t_short:?} -> {t_long:?}"
    );
}
