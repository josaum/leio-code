use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct EventEnvelopeDoctor;

impl Doctor for EventEnvelopeDoctor {
    fn name(&self) -> &'static str {
        "event-envelope"
    }

    fn description(&self) -> &'static str {
        "Checks canonical API event envelope dual-write and SSE normalization."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_event_envelope(index, root)
    }
}

pub fn doctor_event_envelope(_index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let events_path = root.join("example-api/example/events.py");
    let router_path = root.join("example-api/example/routers/events.py");

    let events_src = read_text(&events_path, &mut warnings);
    let router_src = read_text(&router_path, &mut warnings);

    let events_define_normalizer = events_src
        .as_deref()
        .is_some_and(|src| src.contains("def normalize_event_record(record: dict[str, object])"));
    let events_emit_dual_write = events_src.as_deref().is_some_and(|src| {
        src.contains("payload = normalize_event_record({\"type\": event_type, **data})")
            && src.contains("normalized[\"type\"] = event_type")
            && src.contains("normalized[\"ts\"] = ts")
            && src.contains("normalized[\"kind\"] = event_type")
            && src.contains("normalized[\"ts_utc\"] = ts_utc")
            && src.contains("normalized[\"event_id\"] = str(")
            && src.contains("normalized[\"correlation_id\"] = str(")
            && src.contains("normalized[\"actor_type\"] = str(")
            && src.contains("normalized[\"payload\"] = payload")
    });
    let router_normalizes_sse = router_src.as_deref().is_some_and(|src| {
        src.contains("from example.events import CHANNEL, _redis, normalize_event_record")
            && src.contains("def _normalize_sse_payload(raw: str) -> str:")
            && src.contains("return json.dumps(normalize_event_record(payload))")
            && src.contains("data = _normalize_sse_payload(data)")
    });

    if !events_define_normalizer {
        warnings.push("example.events does not expose normalize_event_record()".to_string());
    }
    if !events_emit_dual_write {
        warnings.push(
            "example.events emit() does not dual-write the canonical event envelope with legacy aliases"
                .to_string(),
        );
    }
    if !router_normalizes_sse {
        warnings.push(
            "example.routers.events does not normalize legacy SSE payloads into the canonical event envelope"
                .to_string(),
        );
    }

    if let Some(src) = &events_src {
        for (needle, detail) in [
            (
                "def normalize_event_record(record: dict[str, object])",
                "API event bus exposes canonical/legacy dual-write normalizer",
            ),
            (
                "payload = normalize_event_record({\"type\": event_type, **data})",
                "event publisher routes all API events through the canonical normalizer",
            ),
            (
                "normalized[\"payload\"] = payload",
                "canonical nested payload is materialized for API events",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "event_envelope".to_string(),
                    path: events_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            }
        }
    }

    if let Some(src) = &router_src {
        for (needle, detail) in [
            (
                "def _normalize_sse_payload(raw: str) -> str:",
                "SSE relay upgrades legacy event shapes before broadcasting",
            ),
            (
                "return json.dumps(normalize_event_record(payload))",
                "SSE relay reuses the canonical event normalizer",
            ),
            (
                "data = _normalize_sse_payload(data)",
                "SSE relay always emits normalized event payloads",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "event_envelope".to_string(),
                    path: router_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            }
        }
    }

    entities.push(json!({
        "doctor": "event-envelope",
        "events_path": events_path.display().to_string(),
        "router_path": router_path.display().to_string(),
        "dual_write": events_emit_dual_write,
        "router_normalization": router_normalizes_sse,
        "warning_count": warnings.len(),
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_event_envelope"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked API event envelope canonicalization, found {} warnings",
            warnings.len()
        ),
        confidence: if warnings.is_empty() { 0.98 } else { 0.67 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}
