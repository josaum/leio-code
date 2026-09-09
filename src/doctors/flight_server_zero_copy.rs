use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

/// Guards the vectorized ingest extraction in the main API Flight server. The
/// hot `_exchange_vectors` path once materialized ids + vectors with two
/// per-row `.as_py()` loops over the Arrow batch (O(n) Python round-trips per
/// ingest). The fix flattens each column in one call. Two drifts would
/// silently reinstate the per-row cost:
///   1. `_exchange_vectors` re-introduces an `.as_py() for i in range(...)`
///      per-row extraction loop;
///   2. the vectorized tokens (`to_pylist()` for ids, `flatten().to_numpy` for
///      vectors) or the equivalence regression test disappear.
pub struct FlightServerZeroCopyDoctor;

const SERVER_PY: &str = "example-api/example/flight/server.py";
const TEST_PY: &str = "example-api/example/tests/flight/test_exchange_vectors_zerocopy.py";

impl Doctor for FlightServerZeroCopyDoctor {
    fn name(&self) -> &'static str {
        "flight-server-zero-copy"
    }

    fn description(&self) -> &'static str {
        "The main API Flight server's _exchange_vectors ingest path extracts ids \
         and vectors with vectorized Arrow calls, not per-row .as_py() loops, and \
         keeps its equivalence regression test."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_flight_server_zero_copy(root)
    }
}

/// Body of `_exchange_vectors`: from its `def` to the next same-indent `def`.
fn exchange_vectors_body(src: &str) -> Option<&str> {
    let start = src.find("def _exchange_vectors(")?;
    let rest = &src[start..];
    // Next method at 4-space indent ends the body; fall back to end of file.
    let end = rest[1..]
        .find("\n    def ")
        .map(|i| i + 1)
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

pub fn doctor_flight_server_zero_copy(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings: Vec<String> = Vec::new();
    let mut evidence: Vec<EvidenceItem> = Vec::new();

    let server = root.join(SERVER_PY);
    let test = root.join(TEST_PY);

    if !server.exists() {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor.flight-server-zero-copy"),
            kind: "doctor".to_string(),
            summary: "flight-server-zero-copy: server.py not present; skipping".to_string(),
            confidence: 0.5,
            entities: vec![],
            evidence,
            warnings,
            meta: Some(json!({"skipped": true})),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    let server_src = read_text(&server, &mut warnings).unwrap_or_default();

    match exchange_vectors_body(&server_src) {
        None => warnings.push(format!("{SERVER_PY} no longer defines _exchange_vectors")),
        Some(body) => {
            // 1. No per-row as_py() extraction loop.
            if body.contains(".as_py() for i in range(") {
                warnings.push(format!(
                    "{SERVER_PY} _exchange_vectors reintroduced a per-row `.as_py() for i in range(...)` extraction loop — the vectorized ingest regressed"
                ));
            }
            // 2. Vectorized extraction tokens present.
            if !body.contains(".to_pylist()") {
                warnings.push(format!(
                    "{SERVER_PY} _exchange_vectors no longer extracts ids via to_pylist()"
                ));
            }
            if !body.contains("flatten().to_numpy") {
                warnings.push(format!(
                    "{SERVER_PY} _exchange_vectors no longer flattens the vector column via flatten().to_numpy(...)"
                ));
            }
        }
    }

    // 3. The equivalence regression test must not be deleted.
    if !test.exists() {
        warnings.push(format!(
            "{TEST_PY} is missing — the vectorized/per-row equivalence is no longer locked"
        ));
    }

    evidence.push(EvidenceItem {
        kind: "flight-server-zero-copy".to_string(),
        path: server.display().to_string(),
        line: None,
        detail: "_exchange_vectors uses vectorized Arrow extraction".to_string(),
    });

    let summary = if warnings.is_empty() {
        "flight-server-zero-copy: _exchange_vectors extracts vectors without per-row as_py()"
            .to_string()
    } else {
        format!("flight-server-zero-copy: {} issue(s)", warnings.len())
    };
    let confidence = if warnings.is_empty() {
        0.97_f32
    } else {
        0.6_f32
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor.flight-server-zero-copy"),
        kind: "doctor".to_string(),
        summary,
        confidence,
        entities: vec![json!({
            "test_present": test.exists(),
        })],
        evidence,
        warnings,
        meta: Some(json!({
            "server": SERVER_PY,
            "test": TEST_PY,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}
