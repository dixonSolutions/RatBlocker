//! The versioned, serializable rule database produced by the filter compiler
//! and loaded by every runtime.
//!
//! Indexes are *not* serialized: they are rebuilt on load, which is fast and
//! keeps the on-disk format independent of internal index layout.

use serde::{Deserialize, Serialize};

use super::rules::{CosmeticRule, NetworkRule};
use super::LoadStats;

/// Bumped whenever the serialized shape changes incompatibly.
pub const DATABASE_FORMAT_VERSION: u32 = 1;

/// Provenance and licensing for one compiled source list.
///
/// Recorded per list rather than per database because, as §20 requires,
/// RatBlocker must not assume every third-party list shares a licence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    /// Verbatim licence line from the list, when it declares one.
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// SHA-256 of the source text the rules were compiled from.
    #[serde(default)]
    pub checksum: Option<String>,
    pub rule_count: usize,
}

/// A compiled set of subscription rules.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuleDatabase {
    pub format_version: u32,
    pub sources: Vec<SourceInfo>,
    pub network: Vec<NetworkRule>,
    pub exceptions: Vec<NetworkRule>,
    pub removeparam: Vec<NetworkRule>,
    pub cosmetic: Vec<CosmeticRule>,
    pub stats: LoadStats,
}

#[derive(Debug, thiserror::Error)]
pub enum DatabaseError {
    #[error("rule database format version {found} is not supported (expected {expected})")]
    UnsupportedVersion { found: u32, expected: u32 },
}

impl RuleDatabase {
    pub fn new() -> Self {
        Self {
            format_version: DATABASE_FORMAT_VERSION,
            ..Default::default()
        }
    }

    /// Total rules of every kind.
    pub fn len(&self) -> usize {
        self.network.len() + self.exceptions.len() + self.removeparam.len() + self.cosmetic.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Refuse a database this build cannot interpret.
    pub fn check_version(&self) -> Result<(), DatabaseError> {
        if self.format_version != DATABASE_FORMAT_VERSION {
            return Err(DatabaseError::UnsupportedVersion {
                found: self.format_version,
                expected: DATABASE_FORMAT_VERSION,
            });
        }
        Ok(())
    }

    /// Attribution block for every bundled list, for display in the UI and for
    /// inclusion in distribution packages.
    pub fn attribution(&self) -> String {
        let mut out = String::new();
        for s in &self.sources {
            out.push_str(s.title.as_deref().unwrap_or(&s.id));
            if let Some(v) = &s.version {
                out.push_str(&format!(" (version {v})"));
            }
            out.push('\n');
            if let Some(u) = &s.url {
                out.push_str(&format!("  Source:  {u}\n"));
            }
            if let Some(h) = &s.homepage {
                out.push_str(&format!("  Home:    {h}\n"));
            }
            out.push_str(&format!(
                "  Licence: {}\n",
                s.license.as_deref().unwrap_or("not declared by the list")
            ));
            out.push_str(&format!("  Rules:   {}\n\n", s.rule_count));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The database is stored in a non-self-describing binary format, where a
    /// field omitted on serialize would be misread on deserialize. Guard
    /// against a `skip_serializing_if` creeping back into these types.
    #[test]
    fn database_types_round_trip_through_a_non_self_describing_format() {
        use crate::parser::ListFormat;
        use crate::rule_engine::EngineBuilder;

        let mut b = EngineBuilder::new();
        b.add_list(
            "t",
            "! Title: T\n||ads.example^$third-party,domain=a.test|~b.test\n@@||ads.example/ok^\nx.test##.ad\n$removeparam=utm_source,domain=y.test",
            ListFormat::Adblock,
        );
        let db = b.into_database();
        let bytes = postcard::to_stdvec(&db).unwrap();
        let back: RuleDatabase = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back.format_version, db.format_version);
        assert_eq!(back.network.len(), db.network.len());
        assert_eq!(back.exceptions.len(), db.exceptions.len());
        assert_eq!(back.removeparam.len(), db.removeparam.len());
        assert_eq!(back.cosmetic.len(), db.cosmetic.len());
        assert_eq!(back.sources, db.sources);
        assert_eq!(back.network[0], db.network[0]);
    }
}
