//! Shared helpers for the RatBlocker cross-cutting test suites.

use std::path::{Path, PathBuf};

/// Locate a compiled rule database, if one has been built.
///
/// The heavy suites are skipped rather than failed when `dist/rules.rbdb` is
/// absent, so a clean checkout still runs `cargo test` successfully.
pub fn compiled_database() -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .join("dist/rules.rbdb");
    p.exists().then_some(p)
}

/// A spread of URLs resembling what a browser actually requests on a page
/// load: first-party documents, CDN assets, analytics and ad calls.
pub const URL_CORPUS: &[(&str, &str)] = &[
    ("https://www.theguardian.com/uk", ""),
    ("https://assets.guim.co.uk/static/frontend/main.js", "https://www.theguardian.com/uk"),
    ("https://www.google-analytics.com/analytics.js", "https://www.theguardian.com/uk"),
    ("https://securepubads.g.doubleclick.net/tag/js/gpt.js", "https://www.theguardian.com/uk"),
    ("https://cdn.example.org/images/hero.jpg", "https://www.theguardian.com/uk"),
    ("https://www.facebook.com/tr?id=1&ev=PageView", "https://shop.example.com/"),
    ("https://connect.facebook.net/en_US/fbevents.js", "https://shop.example.com/"),
    ("https://shop.example.com/api/cart", "https://shop.example.com/"),
    ("https://fonts.gstatic.com/s/roboto/v30/font.woff2", "https://shop.example.com/"),
    ("https://static.doubleclick.net/instream/ad_status.js", "https://www.youtube.com/"),
    ("https://i.ytimg.com/vi/abc/hqdefault.jpg", "https://www.youtube.com/"),
    ("https://scorecardresearch.com/beacon.js", "https://news.example.net/"),
    ("https://news.example.net/article/1?utm_source=twitter", ""),
    ("https://ads.pubmatic.com/AdServer/js/pwt/x.js", "https://news.example.net/"),
    ("https://example.com/", ""),
];
