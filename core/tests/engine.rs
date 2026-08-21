//! Unit coverage for the areas `docs/architecture.md` §21 calls out: domain
//! matching, URL normalization, exception rules, resource types, allowlist
//! precedence, parser edge cases and configuration migrations.

use ratblocker_core::parser::{parse_line, ListFormat, ParsedLine, RejectReason};
use ratblocker_core::rule_engine::rules::{Anchor, PartyConstraint};
use ratblocker_core::storage::Configuration;
use ratblocker_core::url;
use ratblocker_core::{
    EngineBuilder, EngineConfig, FilterDecision, RequestContext, ResourceType,
};

fn engine(rules: &str) -> ratblocker_core::Engine {
    let mut b = EngineBuilder::new();
    b.add_list("test", rules, ListFormat::Adblock);
    b.build(EngineConfig::default())
}

fn decide(e: &ratblocker_core::Engine, req: &str, src: &str, ty: ResourceType) -> FilterDecision {
    let mut ctx = RequestContext::new(req, ty);
    if !src.is_empty() {
        ctx.source_url = Some(src.to_string());
    }
    e.evaluate(&ctx).decision
}

// ---------------------------------------------------------------------------
// URL normalization
// ---------------------------------------------------------------------------

#[test]
fn normalization_lowercases_host_and_drops_fragment() {
    let u = url::parse("HTTPS://Ads.Example.COM:443/Path?A=1#frag").unwrap();
    assert_eq!(u.host, "ads.example.com");
    assert_eq!(u.port, Some(443));
    assert_eq!(u.normalized, "https://ads.example.com/path?a=1");
    // Path and query keep their original case for rewriting.
    assert_eq!(u.path, "/Path");
    assert_eq!(u.query, "A=1");
}

#[test]
fn normalization_keeps_non_default_ports() {
    let u = url::parse("http://example.com:8080/x").unwrap();
    assert_eq!(u.normalized, "http://example.com:8080/x");
}

#[test]
fn normalization_strips_userinfo() {
    let u = url::parse("https://user:pw@example.com/x").unwrap();
    assert_eq!(u.host, "example.com");
}

#[test]
fn normalization_handles_ipv6_literals() {
    let u = url::parse("http://[2001:db8::1]:8080/a").unwrap();
    assert_eq!(u.host, "[2001:db8::1]");
    assert_eq!(u.port, Some(8080));
}

#[test]
fn normalization_rejects_unfilterable_urls() {
    assert!(url::parse("data:text/html,hi").is_err());
    assert!(url::parse("about:blank").is_err());
    assert!(url::parse("chrome-extension://abc/x.js").is_err());
    assert!(url::parse(&format!("https://e.com/{}", "a".repeat(9000))).is_err());
}

#[test]
fn empty_path_normalizes_to_root() {
    assert_eq!(url::parse("https://example.com").unwrap().normalized, "https://example.com/");
}

// ---------------------------------------------------------------------------
// Domain matching / public suffix handling
// ---------------------------------------------------------------------------

#[test]
fn registrable_domain_uses_the_public_suffix_list() {
    assert_eq!(url::registrable_domain("a.b.example.com"), "example.com");
    assert_eq!(url::registrable_domain("www.example.co.uk"), "example.co.uk");
    assert_eq!(url::registrable_domain("shop.example.com.au"), "example.com.au");
    // Private section entries are separate sites.
    assert_eq!(url::registrable_domain("me.github.io"), "me.github.io");
    // A host that is itself a public suffix has no registrable domain.
    assert_eq!(url::registrable_domain("com"), "com");
    // IP literals are returned unchanged.
    assert_eq!(url::registrable_domain("192.0.2.1"), "192.0.2.1");
}

#[test]
fn third_party_is_decided_by_registrable_domain() {
    let e = engine("||tracker.example^$third-party");
    // Same registrable domain -> first party -> not blocked.
    assert_eq!(
        decide(&e, "https://tracker.example/t.gif", "https://www.tracker.example/", ResourceType::Image),
        FilterDecision::Allow
    );
    assert_eq!(
        decide(&e, "https://tracker.example/t.gif", "https://news.site/", ResourceType::Image),
        FilterDecision::Block
    );
}

#[test]
fn party_constrained_rules_do_not_fire_without_a_source() {
    // The DNS layer has no source URL; a $third-party rule must not over-block.
    let e = engine("||tracker.example^$third-party");
    assert_eq!(e.evaluate_host("tracker.example", None).decision, FilterDecision::Allow);
    let e2 = engine("||tracker.example^");
    assert_eq!(e2.evaluate_host("tracker.example", None).decision, FilterDecision::Block);
}

// ---------------------------------------------------------------------------
// Pattern semantics
// ---------------------------------------------------------------------------

#[test]
fn hostname_anchor_matches_subdomains_but_not_suffix_tricks() {
    let e = engine("||example.com^");
    assert_eq!(decide(&e, "https://example.com/a", "", ResourceType::Document), FilterDecision::Block);
    assert_eq!(decide(&e, "https://ads.example.com/a", "", ResourceType::Document), FilterDecision::Block);
    // `notexample.com` must NOT match `||example.com^`.
    assert_eq!(decide(&e, "https://notexample.com/a", "", ResourceType::Document), FilterDecision::Allow);
    // Nor may a look-alike that merely contains the string.
    assert_eq!(decide(&e, "https://evil.net/?q=example.com", "", ResourceType::Document), FilterDecision::Allow);
}

#[test]
fn separator_matches_end_of_url() {
    let e = engine("||ads.example.com^");
    assert_eq!(decide(&e, "https://ads.example.com", "", ResourceType::Document), FilterDecision::Block);
}

#[test]
fn wildcards_match_in_sequence() {
    let e = engine("/banner/*/img^");
    assert_eq!(
        decide(&e, "https://cdn.site/banner/deep/path/img?x=1", "", ResourceType::Image),
        FilterDecision::Block
    );
    assert_eq!(
        decide(&e, "https://cdn.site/img/banner/x", "", ResourceType::Image),
        FilterDecision::Allow
    );
}

#[test]
fn left_and_right_anchors_are_honoured() {
    let e = engine("|https://exact.example/path|");
    assert_eq!(decide(&e, "https://exact.example/path", "", ResourceType::Document), FilterDecision::Block);
    assert_eq!(decide(&e, "https://exact.example/path/more", "", ResourceType::Document), FilterDecision::Allow);
    assert_eq!(decide(&e, "https://x.test/?u=https://exact.example/path", "", ResourceType::Document), FilterDecision::Allow);
}

#[test]
fn regex_rules_match() {
    let e = engine(r"/^https?:\/\/ad[0-9]{2}\.example\./");
    assert_eq!(decide(&e, "https://ad42.example.com/x", "", ResourceType::Image), FilterDecision::Block);
    assert_eq!(decide(&e, "https://ad4.example.com/x", "", ResourceType::Image), FilterDecision::Allow);
}

// ---------------------------------------------------------------------------
// Resource types and options
// ---------------------------------------------------------------------------

#[test]
fn resource_type_options_restrict_matches() {
    let e = engine("||cdn.example^$script");
    assert_eq!(decide(&e, "https://cdn.example/a.js", "https://s.test/", ResourceType::Script), FilterDecision::Block);
    assert_eq!(decide(&e, "https://cdn.example/a.png", "https://s.test/", ResourceType::Image), FilterDecision::Allow);
}

#[test]
fn negated_resource_types_invert_the_mask() {
    let e = engine("||cdn.example^$~script");
    assert_eq!(decide(&e, "https://cdn.example/a.js", "https://s.test/", ResourceType::Script), FilterDecision::Allow);
    assert_eq!(decide(&e, "https://cdn.example/a.png", "https://s.test/", ResourceType::Image), FilterDecision::Block);
}

#[test]
fn domain_option_scopes_a_rule_to_sites() {
    let e = engine("||widget.example^$domain=allowed.test|~sub.allowed.test");
    assert_eq!(decide(&e, "https://widget.example/w", "https://allowed.test/", ResourceType::Script), FilterDecision::Block);
    assert_eq!(decide(&e, "https://widget.example/w", "https://sub.allowed.test/", ResourceType::Script), FilterDecision::Allow);
    assert_eq!(decide(&e, "https://widget.example/w", "https://other.test/", ResourceType::Script), FilterDecision::Allow);
}

#[test]
fn denyallow_carves_hosts_out_of_a_rule() {
    let e = engine("*/track^$denyallow=safe.example");
    assert_eq!(decide(&e, "https://x.test/track", "https://p.test/", ResourceType::Other), FilterDecision::Block);
    assert_eq!(decide(&e, "https://safe.example/track", "https://p.test/", ResourceType::Other), FilterDecision::Allow);
}

#[test]
fn app_option_scopes_a_rule_to_applications() {
    let e = engine("||metrics.example^$app=com.example.app");
    let mut ctx = RequestContext::new("https://metrics.example/m", ResourceType::Other);
    ctx.application_id = Some("com.example.app".into());
    assert_eq!(e.evaluate(&ctx).decision, FilterDecision::Block);
    ctx.application_id = Some("com.other.app".into());
    assert_eq!(e.evaluate(&ctx).decision, FilterDecision::Allow);
}

// ---------------------------------------------------------------------------
// Exceptions, importance and allowlist precedence
// ---------------------------------------------------------------------------

#[test]
fn exception_rules_beat_block_rules() {
    let e = engine("||ads.example^\n@@||ads.example/allowed^");
    assert_eq!(decide(&e, "https://ads.example/x", "", ResourceType::Image), FilterDecision::Block);
    assert_eq!(decide(&e, "https://ads.example/allowed/x", "", ResourceType::Image), FilterDecision::Allow);
}

#[test]
fn important_blocks_beat_exceptions() {
    let e = engine("||ads.example^$important\n@@||ads.example^");
    assert_eq!(decide(&e, "https://ads.example/x", "", ResourceType::Image), FilterDecision::Block);
}

#[test]
fn allowlist_outranks_every_rule_including_important() {
    let mut b = EngineBuilder::new();
    b.add_list("t", "||ads.example^$important", ListFormat::Adblock);
    let mut cfg = EngineConfig::default();
    cfg.allowlisted_domains.insert("news.test".into());
    let e = b.build(cfg);
    // Subresource on an allowlisted page.
    assert_eq!(decide(&e, "https://ads.example/x", "https://www.news.test/a", ResourceType::Image), FilterDecision::Allow);
    // Same request from a non-allowlisted page still blocks.
    assert_eq!(decide(&e, "https://ads.example/x", "https://other.test/a", ResourceType::Image), FilterDecision::Block);
}

#[test]
fn user_rules_take_precedence_over_subscriptions() {
    let mut b = EngineBuilder::new();
    b.add_list("t", "@@||ads.example^", ListFormat::Adblock);
    b.add_user_rules("||ads.example^");
    let e = b.build(EngineConfig::default());
    assert_eq!(decide(&e, "https://ads.example/x", "", ResourceType::Image), FilterDecision::Block);
}

#[test]
fn badfilter_cancels_a_matching_rule() {
    let e = engine("||ads.example^\n||ads.example^$badfilter");
    assert_eq!(decide(&e, "https://ads.example/x", "", ResourceType::Image), FilterDecision::Allow);
}

#[test]
fn disabled_engine_allows_everything() {
    let mut b = EngineBuilder::new();
    b.add_list("t", "||ads.example^", ListFormat::Adblock);
    let e = b.build(EngineConfig { enabled: false, ..Default::default() });
    assert_eq!(decide(&e, "https://ads.example/x", "", ResourceType::Image), FilterDecision::Allow);
}

// ---------------------------------------------------------------------------
// Modifier rules
// ---------------------------------------------------------------------------

#[test]
fn removeparam_strips_only_the_named_parameters() {
    let e = engine("$removeparam=utm_source|utm_medium");
    let ctx = RequestContext::new(
        "https://shop.test/p?id=7&utm_source=news&utm_medium=email#top",
        ResourceType::Document,
    );
    let r = e.evaluate(&ctx);
    assert_eq!(r.decision, FilterDecision::RemoveParameters);
    assert_eq!(r.rewritten_url.as_deref(), Some("https://shop.test/p?id=7#top"));
    assert_eq!(r.removed_parameters, vec!["utm_source", "utm_medium"]);
}

#[test]
fn removeparam_is_a_no_op_when_nothing_matches() {
    let e = engine("$removeparam=utm_source");
    let ctx = RequestContext::new("https://shop.test/p?id=7", ResourceType::Document);
    assert_eq!(e.evaluate(&ctx).decision, FilterDecision::Allow);
}

#[test]
fn redirect_rules_produce_a_redirect_decision() {
    let e = engine("||ads.example/track.js$redirect=noopjs,script");
    let ctx = RequestContext::new("https://ads.example/track.js", ResourceType::Script)
        .with_source("https://p.test/");
    let r = e.evaluate(&ctx);
    assert_eq!(r.decision, FilterDecision::Redirect);
    assert_eq!(r.redirect_to.as_deref(), Some("noopjs"));
}

// ---------------------------------------------------------------------------
// Cosmetic filtering
// ---------------------------------------------------------------------------

#[test]
fn cosmetic_rules_are_scoped_by_domain() {
    let mut b = EngineBuilder::new();
    b.add_list(
        "t",
        "##.generic-ad\nexample.test##.site-ad\nother.test##.other-ad\nexample.test#@#.generic-ad",
        ListFormat::Adblock,
    );
    let e = b.build(EngineConfig::default());

    let r = e.cosmetic_for("https://example.test/page");
    assert!(r.hide.contains(&".site-ad".to_string()));
    // The site-specific unhide removed the generic selector.
    assert!(!r.hide.contains(&".generic-ad".to_string()));
    assert!(!r.hide.contains(&".other-ad".to_string()));

    let r2 = e.cosmetic_for("https://third.test/page");
    assert_eq!(r2.hide, vec![".generic-ad".to_string()]);
}

#[test]
fn generichide_exception_suppresses_only_generic_selectors() {
    let mut b = EngineBuilder::new();
    b.add_list(
        "t",
        "##.generic-ad\nexample.test##.site-ad\n@@||example.test^$generichide",
        ListFormat::Adblock,
    );
    let e = b.build(EngineConfig::default());
    let r = e.cosmetic_for("https://example.test/page");
    assert_eq!(r.hide, vec![".site-ad".to_string()]);
}

#[test]
fn document_exception_disables_cosmetic_filtering() {
    let mut b = EngineBuilder::new();
    b.add_list("t", "##.generic-ad\n@@||example.test^$document", ListFormat::Adblock);
    let e = b.build(EngineConfig::default());
    assert!(e.cosmetic_for("https://example.test/p").is_empty());
    assert!(!e.cosmetic_for("https://other.test/p").is_empty());
}

// ---------------------------------------------------------------------------
// Parser edge cases and input rejection
// ---------------------------------------------------------------------------

fn parse(line: &str) -> ParsedLine {
    parse_line(line, ListFormat::Adblock, "t:1")
}

#[test]
fn comments_and_metadata_are_recognized() {
    assert_eq!(parse("[Adblock Plus 2.0]"), ParsedLine::Comment);
    assert_eq!(parse("! just a comment"), ParsedLine::Comment);
    match parse("! Title: EasyList") {
        ParsedLine::Metadata { key, value } => {
            assert_eq!(key, "title");
            assert_eq!(value, "EasyList");
        }
        other => panic!("expected metadata, got {other:?}"),
    }
}

#[test]
fn overly_generic_patterns_are_rejected() {
    assert!(matches!(parse("ad"), ParsedLine::Rejected { reason: RejectReason::TooGeneric, .. }));
    assert!(matches!(parse("*"), ParsedLine::Rejected { .. }));
}

#[test]
fn unsafe_and_oversized_regexes_are_rejected() {
    let huge = format!("/{}/", "a".repeat(600));
    assert!(matches!(parse(&huge), ParsedLine::Rejected { reason: RejectReason::UnsafeRegex, .. }));
    assert!(matches!(parse("/([a-z]+/"), ParsedLine::Rejected { reason: RejectReason::UnsafeRegex, .. }));
}

#[test]
fn unknown_and_unsupported_options_are_rejected_not_ignored() {
    assert!(matches!(
        parse("||x.test^$nosuchoption"),
        ParsedLine::Rejected { reason: RejectReason::UnknownOption(_), .. }
    ));
    assert!(matches!(
        parse("||x.test^$csp=script-src 'none'"),
        ParsedLine::Rejected { reason: RejectReason::UnsupportedOption(_), .. }
    ));
}

#[test]
fn malformed_domains_are_rejected() {
    assert!(matches!(
        parse("||exa mple.com^"),
        ParsedLine::Rejected { reason: RejectReason::MalformedDomain(_), .. }
    ));
    assert!(matches!(
        parse("||x.test^$domain=not a domain"),
        ParsedLine::Rejected { reason: RejectReason::MalformedDomain(_), .. }
    ));
}

#[test]
fn scriptlet_and_procedural_cosmetics_are_rejected() {
    assert!(matches!(
        parse("example.test##+js(setTimeout-defuser)"),
        ParsedLine::Rejected { reason: RejectReason::UnsupportedCosmetic, .. }
    ));
    assert!(matches!(
        parse("example.test#?#div:has-text(ad)"),
        ParsedLine::Rejected { reason: RejectReason::UnsupportedCosmetic, .. }
    ));
}

#[test]
fn options_are_parsed_off_regex_bodies_correctly() {
    match parse(r"/ads\$\d+/$script,third-party") {
        ParsedLine::Network(r) => {
            assert!(r.regex.is_some());
            assert_eq!(r.options.party, PartyConstraint::ThirdOnly);
            assert_eq!(r.options.resource_mask, ResourceType::Script.mask());
        }
        other => panic!("expected network rule, got {other:?}"),
    }
}

#[test]
fn hostname_anchor_is_extracted_for_indexing() {
    match parse("||ads.example.com^$image") {
        ParsedLine::Network(r) => {
            assert_eq!(r.anchor, Anchor::Hostname);
            assert_eq!(r.host_anchor.as_deref(), Some("ads.example.com"));
        }
        other => panic!("expected network rule, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Hosts and domain list formats
// ---------------------------------------------------------------------------

#[test]
fn hosts_files_become_hostname_rules() {
    let mut b = EngineBuilder::new();
    b.add_list(
        "hosts",
        "# comment\n127.0.0.1 localhost\n0.0.0.0 tracker.test\n0.0.0.0 ads.test # inline",
        ListFormat::Hosts,
    );
    let e = b.build(EngineConfig::default());
    assert_eq!(decide(&e, "https://tracker.test/x", "", ResourceType::Other), FilterDecision::Block);
    assert_eq!(decide(&e, "https://ads.test/x", "", ResourceType::Other), FilterDecision::Block);
    // `localhost` must never become a blocking rule.
    assert_eq!(decide(&e, "http://localhost:8080/x", "", ResourceType::Other), FilterDecision::Allow);
}

#[test]
fn list_format_detection_works() {
    assert_eq!(ListFormat::detect("[Adblock Plus 2.0]\n||a.test^"), ListFormat::Adblock);
    assert_eq!(ListFormat::detect("0.0.0.0 a.test\n0.0.0.0 b.test"), ListFormat::Hosts);
    assert_eq!(ListFormat::detect("a.test\nb.test\nc.test"), ListFormat::Domains);
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[test]
fn default_configuration_is_valid_and_privacy_preserving() {
    let c = Configuration::default();
    c.validate().unwrap();
    assert!(!c.privacy.statistics_enabled);
    assert!(!c.privacy.request_logging_enabled);
    assert!(c.updates.automatic);
}

#[test]
fn configuration_rejects_future_versions_and_bad_values() {
    let mut c = Configuration::default();
    c.version = 99;
    assert!(c.clone().migrate().is_err());

    let mut c2 = Configuration::default();
    c2.version = 1;
    c2.updates.interval_hours = 0;
    assert!(c2.validate().is_err());

    let mut c3 = Configuration::default();
    c3.allowlisted_domains.push("not a domain".into());
    assert!(c3.validate().is_err());
}

#[test]
fn configuration_requires_https_subscriptions() {
    let mut c = Configuration::default();
    c.filter_subscriptions[0].url = Some("http://insecure.test/list.txt".into());
    assert!(c.validate().is_err());
    c.filter_subscriptions[0].url = Some("https://secure.test/list.txt".into());
    assert!(c.validate().is_ok());
}

#[test]
fn configuration_round_trips_through_json() {
    let c = Configuration::default();
    let json = serde_json::to_string(&c).unwrap();
    let back: Configuration = serde_json::from_str(&json).unwrap();
    assert_eq!(c, back);
}

// ---------------------------------------------------------------------------
// EasyList features found in the real lists
// ---------------------------------------------------------------------------

#[test]
fn wildcard_tld_domains_match_every_public_suffix() {
    assert!(url::domain_pattern_matches("www.amazon.co.uk", "amazon.*"));
    assert!(url::domain_pattern_matches("amazon.com", "amazon.*"));
    assert!(url::domain_pattern_matches("read.amazon.de", "read.amazon.*"));
    // Must not match a different registrable domain that merely shares a label.
    assert!(!url::domain_pattern_matches("amazon.evil.com", "amazon.*"));
    assert!(!url::domain_pattern_matches("notamazon.com", "amazon.*"));
}

#[test]
fn cosmetic_rules_support_wildcard_tlds() {
    let mut b = EngineBuilder::new();
    b.add_list("t", "crazygames.*##.mpu\nread.amazon.*##.kw-ads", ListFormat::Adblock);
    let e = b.build(EngineConfig::default());
    assert_eq!(e.cosmetic_for("https://www.crazygames.com/g").hide, vec![".mpu".to_string()]);
    assert_eq!(e.cosmetic_for("https://crazygames.co.uk/g").hide, vec![".mpu".to_string()]);
    assert_eq!(e.cosmetic_for("https://read.amazon.de/x").hide, vec![".kw-ads".to_string()]);
    assert!(e.cosmetic_for("https://example.test/").is_empty());
}

#[test]
fn domain_option_supports_wildcard_tlds() {
    let e = engine("||ads.example^$domain=shop.*");
    assert_eq!(decide(&e, "https://ads.example/x", "https://shop.co.uk/", ResourceType::Image), FilterDecision::Block);
    assert_eq!(decide(&e, "https://ads.example/x", "https://other.test/", ResourceType::Image), FilterDecision::Allow);
}

#[test]
fn pattern_less_rules_are_accepted_only_when_scoped() {
    // Scoped by $domain: valid, blocks everything on those sites.
    let e = engine("$websocket,domain=example.test");
    assert_eq!(decide(&e, "wss://anything.test/s", "https://example.test/", ResourceType::WebSocket), FilterDecision::Block);
    assert_eq!(decide(&e, "wss://anything.test/s", "https://other.test/", ResourceType::WebSocket), FilterDecision::Allow);
    // Unscoped: refused, because it would block the entire web.
    assert!(matches!(
        parse("$third-party"),
        ParsedLine::Rejected { reason: RejectReason::EmptyPattern, .. }
    ));
}

#[test]
fn abp_rewrite_is_treated_as_redirect() {
    let e = engine("||cdn.example/ad.mp4$rewrite=abp-resource:blank-mp4,domain=site.test");
    let ctx = RequestContext::new("https://cdn.example/ad.mp4", ResourceType::Media)
        .with_source("https://site.test/");
    let r = e.evaluate(&ctx);
    assert_eq!(r.decision, FilterDecision::Redirect);
    assert_eq!(r.redirect_to.as_deref(), Some("blank-mp4"));
}

#[test]
fn public_suffix_predicate_is_exact() {
    assert!(url::is_public_suffix("com"));
    assert!(url::is_public_suffix("co.uk"));
    assert!(url::is_public_suffix("github.io"));
    assert!(!url::is_public_suffix("example.com"));
}
