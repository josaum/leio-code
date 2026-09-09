use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

/// Guards the gRPC message-size convention for Python Flight clients. The audit
/// found five private copies of the `_DEFAULT_FLIGHT_MAX_MESSAGE_BYTES` +
/// generic_options block; the fix collapsed them into one shared
/// `flight_generic_options()` in `contracts.py`. Two drifts would silently
/// reinstate the 4 MiB gRPC cap on some clients:
///   1. a client-construction site stops routing through
///      `flight_generic_options()` (its channel falls back to the 4 MiB
///      default → large embed/adapter batches die with RESOURCE_EXHAUSTED);
///   2. a client re-introduces a PRIVATE `_DEFAULT_FLIGHT_MAX_MESSAGE_BYTES`
///      copy — the exact drift the dedup removed (raise the cap in one place,
///      miss the others).
pub struct GrpcMessageSizeDoctor;

const CONTRACTS_PY: &str = "example-api/example/flight/contracts.py";
// Client-construction sites that MUST route through the shared helper.
const CLIENT_SITES: [&str; 4] = [
    "example-api/example/core/inference.py",
    "example-api/example/flight/rust_bridge.py",
    "example-api/example/flight/gepa_client.py",
    "example-api/example/flight/ocr_client.py",
];

impl Doctor for GrpcMessageSizeDoctor {
    fn name(&self) -> &'static str {
        "grpc-message-size"
    }

    fn description(&self) -> &'static str {
        "Python Flight clients route through the shared flight_generic_options() \
         message-size helper, with no private _DEFAULT_FLIGHT_MAX_MESSAGE_BYTES \
         copies reinstating the 4 MiB gRPC cap."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_grpc_message_size(root)
    }
}

pub fn doctor_grpc_message_size(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings: Vec<String> = Vec::new();
    let mut evidence: Vec<EvidenceItem> = Vec::new();

    let contracts = root.join(CONTRACTS_PY);
    if !contracts.exists() {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor.grpc-message-size"),
            kind: "doctor".to_string(),
            summary: "grpc-message-size: contracts.py not present; skipping".to_string(),
            confidence: 0.5,
            entities: vec![],
            evidence,
            warnings,
            meta: Some(json!({"skipped": true})),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    // 1. The shared helper + its single canonical default constant.
    let contracts_src = read_text(&contracts, &mut warnings).unwrap_or_default();
    if !contracts_src.contains("def flight_generic_options") {
        warnings.push(format!(
            "{CONTRACTS_PY} is missing the shared flight_generic_options() helper"
        ));
    }
    if !contracts_src.contains("_DEFAULT_FLIGHT_MAX_MESSAGE_BYTES") {
        warnings.push(format!(
            "{CONTRACTS_PY} lost the canonical _DEFAULT_FLIGHT_MAX_MESSAGE_BYTES constant"
        ));
    }

    // 2 + 3. Each client site uses the shared helper and holds no private copy.
    let mut checked = 0;
    for rel in CLIENT_SITES {
        let path = root.join(rel);
        if !path.exists() {
            continue;
        }
        checked += 1;
        let src = read_text(&path, &mut warnings).unwrap_or_default();
        if !src.contains("flight_generic_options") {
            warnings.push(format!(
                "{rel} no longer routes through flight_generic_options() — its Flight channel falls back to the 4 MiB gRPC default"
            ));
        }
        if src.contains("_DEFAULT_FLIGHT_MAX_MESSAGE_BYTES") {
            warnings.push(format!(
                "{rel} re-introduced a private _DEFAULT_FLIGHT_MAX_MESSAGE_BYTES — collapse it into contracts.flight_generic_options()"
            ));
        }
    }

    evidence.push(EvidenceItem {
        kind: "grpc-message-size".to_string(),
        path: contracts.display().to_string(),
        line: None,
        detail: format!("shared helper; {checked} client site(s) checked"),
    });

    let summary = if warnings.is_empty() {
        format!(
            "grpc-message-size: {checked} Flight client(s) share flight_generic_options(), no private copies"
        )
    } else {
        format!(
            "grpc-message-size: {} message-size issue(s)",
            warnings.len()
        )
    };
    let confidence = if warnings.is_empty() {
        0.97_f32
    } else {
        0.6_f32
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor.grpc-message-size"),
        kind: "doctor".to_string(),
        summary,
        confidence,
        entities: vec![json!({
            "shared_helper": contracts_src.contains("def flight_generic_options"),
            "client_sites_checked": checked,
        })],
        evidence,
        warnings,
        meta: Some(json!({
            "contracts": CONTRACTS_PY,
            "client_sites": CLIENT_SITES,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}
