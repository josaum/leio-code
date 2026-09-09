//! Output envelope shared by every LEIO query family.
//!
//! [`QueryEnvelope`] is the one JSON shape downstream consumers parse; it is
//! defined here so a runtime that only needs SPARQL grounding reads the same
//! contract as the full `leio-code` CLI without depending on it.
// Rust guideline compliant 2026-02-21

use serde::{Deserialize, Serialize};

/// Output contract version stamped on every [`QueryEnvelope`] and on every
/// diagnostic-format root document (SARIF + JSON).
///
/// Bumped per `docs/output-schema.md` §7 — adding a field is a minor bump,
/// removing or renaming a field is a major bump. Pinned to `"1.0"` until a
/// breaking change ships. This is a contract version, not a build version, so
/// it is intentionally not read from `Cargo.toml`.
pub const SCHEMA_VERSION: &str = "1.0";

/// One answer from any LEIO query family, with evidence and metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryEnvelope {
    /// Output contract version. See [`SCHEMA_VERSION`] and
    /// `docs/output-schema.md` §7. Declared first so it serializes as the
    /// first JSON key — downstream consumers can branch on it before
    /// parsing the rest of the envelope.
    pub schema_version: String,
    pub query_id: String,
    pub kind: String,
    pub summary: String,
    pub confidence: f32,
    pub entities: Vec<serde_json::Value>,
    pub evidence: Vec<EvidenceItem>,
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
    pub timing_ms: u128,
}

/// One `file:line` witness backing an envelope claim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceItem {
    pub kind: String,
    pub path: String,
    pub line: Option<usize>,
    pub detail: String,
}
