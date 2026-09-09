use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

/// Guards Arrow Flight channel/client reuse on the hot path. The audit found
/// per-RPC channel dials (fresh TCP+HTTP/2 handshake per call) on nine
/// surfaces. This locks the canonical reuse primitives so a regression can't
/// silently re-introduce per-call dialing or the silent 4 MiB gRPC cap:
///   1. the gateway Flight client caches channels (`FLIGHT_CHANNELS`) behind a
///      `flight_client_for` helper with `evict_flight_channel` on error;
///   2. that helper raises the inbound gRPC cap (`max_decoding_message_size`)
///      but NOT the outbound one (`max_encoding_message_size` would reject
///      large do_put quad batches — a regression the fix deliberately avoids);
///   3. the Python side exposes the canonical cached-client + reconnect + size
///      helpers in `example/flight/contracts.py`.
pub struct FlightChannelReuseDoctor;

const GATEWAY_CLIENT_RS: &str = "example-gateway/src/flight/client.rs";
const CONTRACTS_PY: &str = "example-api/example/flight/contracts.py";

impl Doctor for FlightChannelReuseDoctor {
    fn name(&self) -> &'static str {
        "flight-channel-reuse"
    }

    fn description(&self) -> &'static str {
        "Arrow Flight clients reuse cached channels (gateway flight_client_for + \
         Python get_cached_flight_client) with the decoding cap raised and no \
         per-RPC dialing regression."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_flight_channel_reuse(root)
    }
}

pub fn doctor_flight_channel_reuse(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings: Vec<String> = Vec::new();
    let mut evidence: Vec<EvidenceItem> = Vec::new();

    let client = root.join(GATEWAY_CLIENT_RS);
    let contracts = root.join(CONTRACTS_PY);

    if !client.exists() || !contracts.exists() {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor.flight-channel-reuse"),
            kind: "doctor".to_string(),
            summary: "flight-channel-reuse: Flight client sources not present; skipping"
                .to_string(),
            confidence: 0.5,
            entities: vec![],
            evidence,
            warnings,
            meta: Some(json!({"skipped": true})),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    let client_src = read_text(&client, &mut warnings).unwrap_or_default();
    let contracts_src = read_text(&contracts, &mut warnings).unwrap_or_default();

    // 1. Gateway channel cache + reuse helper + eviction.
    for (needle, label) in [
        ("FLIGHT_CHANNELS", "channel cache (FLIGHT_CHANNELS)"),
        ("fn flight_client_for", "reuse helper (flight_client_for)"),
        (
            "fn evict_flight_channel",
            "eviction on error (evict_flight_channel)",
        ),
    ] {
        if !client_src.contains(needle) {
            warnings.push(format!(
                "{GATEWAY_CLIENT_RS} lost the {label} — per-RPC dialing may regress"
            ));
        }
    }

    // 2. Inbound cap raised, outbound cap deliberately NOT capped.
    if !client_src.contains("max_decoding_message_size") {
        warnings.push(format!(
            "{GATEWAY_CLIENT_RS} no longer raises max_decoding_message_size — the silent 4 MiB inbound cap returns"
        ));
    }
    if client_src.contains("max_encoding_message_size") {
        warnings.push(format!(
            "{GATEWAY_CLIENT_RS} re-added max_encoding_message_size — this caps OUTBOUND frames and rejects large do_put quad batches (regression)"
        ));
    }

    // 3. Python canonical cached-client + reconnect + size helpers.
    for (needle, label) in [
        ("def get_cached_flight_client", "get_cached_flight_client"),
        (
            "def call_with_flight_reconnect",
            "call_with_flight_reconnect",
        ),
        (
            "def flight_generic_options",
            "flight_generic_options (message-size)",
        ),
        (
            "FLIGHT_TRANSPORT_ERRORS",
            "FLIGHT_TRANSPORT_ERRORS retry tuple",
        ),
    ] {
        if !contracts_src.contains(needle) {
            warnings.push(format!("{CONTRACTS_PY} is missing {label}"));
        }
    }

    evidence.push(EvidenceItem {
        kind: "flight-channel-reuse".to_string(),
        path: client.display().to_string(),
        line: None,
        detail: "cached channels + decoding cap; no outbound cap".to_string(),
    });
    evidence.push(EvidenceItem {
        kind: "flight-channel-reuse".to_string(),
        path: contracts.display().to_string(),
        line: None,
        detail: "canonical cached-client + reconnect + size helpers".to_string(),
    });

    let summary = if warnings.is_empty() {
        "flight-channel-reuse: gateway + Python Flight clients reuse cached channels".to_string()
    } else {
        format!("flight-channel-reuse: {} reuse issue(s)", warnings.len())
    };
    let confidence = if warnings.is_empty() {
        0.97_f32
    } else {
        0.6_f32
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor.flight-channel-reuse"),
        kind: "doctor".to_string(),
        summary,
        confidence,
        entities: vec![json!({
            "gateway_cache": client_src.contains("FLIGHT_CHANNELS"),
            "outbound_uncapped": !client_src.contains("max_encoding_message_size"),
            "python_cached_client": contracts_src.contains("def get_cached_flight_client"),
        })],
        evidence,
        warnings,
        meta: Some(json!({
            "gateway_client": GATEWAY_CLIENT_RS,
            "contracts": CONTRACTS_PY,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}
