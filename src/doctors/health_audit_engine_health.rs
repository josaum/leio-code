use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

/// Guards the health-audit engine-degradation health surface. Both the
/// `fast_stamp` reference engine (five glosa reason-rule families) and the
/// BGE-M3 embedding engine degrade *silently* to fallbacks while audits still
/// return success — a silent recall/quality collapse in a can't-fail domain.
/// Their health must stay visible on the live `/status`:
///   1. `fast_stamp_adapter.py` must expose `fast_stamp_status()` + the named
///      `RULE_FAMILIES` (so /status names exactly what is disabled);
///   2. `engine.py` must expose `embedding_status()`;
///   3. the live `/status` handler (`router.py`) must surface BOTH under
///      `status_payload["engines"]` (a merge once dropped embedding_status).
pub struct HealthAuditEngineHealthDoctor;

const ADAPTER_PY: &str = "cartridges/health_audit/fast_stamp_adapter.py";
const ENGINE_PY: &str = "cartridges/health_audit/engine.py";
const ROUTER_PY: &str = "cartridges/health_audit/router.py";

impl Doctor for HealthAuditEngineHealthDoctor {
    fn name(&self) -> &'static str {
        "health-audit-engine-health"
    }

    fn description(&self) -> &'static str {
        "The fast_stamp and embedding engines' silent degradation stays visible \
         on the live /status via fast_stamp_status()/embedding_status() wired \
         under status_payload[\"engines\"]."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_health_audit_engine_health(root)
    }
}

pub fn doctor_health_audit_engine_health(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings: Vec<String> = Vec::new();
    let mut evidence: Vec<EvidenceItem> = Vec::new();

    let adapter = root.join(ADAPTER_PY);
    let engine = root.join(ENGINE_PY);
    let router = root.join(ROUTER_PY);

    if !adapter.exists() || !engine.exists() || !router.exists() {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor.health-audit-engine-health"),
            kind: "doctor".to_string(),
            summary: "health-audit-engine-health: sources not all present; skipping".to_string(),
            confidence: 0.5,
            entities: vec![],
            evidence,
            warnings,
            meta: Some(json!({"skipped": true})),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    let adapter_src = read_text(&adapter, &mut warnings).unwrap_or_default();
    let engine_src = read_text(&engine, &mut warnings).unwrap_or_default();
    let router_src = read_text(&router, &mut warnings).unwrap_or_default();

    // 1. fast_stamp exposes its status fn + names the rule families it carries.
    if !adapter_src.contains("def fast_stamp_status(") {
        warnings.push(format!(
            "{ADAPTER_PY} is missing fast_stamp_status() — fast_stamp degrades with no health signal"
        ));
    }
    if !adapter_src.contains("RULE_FAMILIES") {
        warnings.push(format!(
            "{ADAPTER_PY} lost RULE_FAMILIES — /status can no longer name the disabled rule families"
        ));
    }

    // 2. embedding exposes its status fn.
    if !engine_src.contains("def embedding_status(") {
        warnings.push(format!("{ENGINE_PY} is missing embedding_status()"));
    }

    // 3. The live /status surfaces BOTH engines under status_payload["engines"].
    if !router_src.contains("status_payload[\"engines\"]") {
        warnings.push(format!(
            "{ROUTER_PY} /status does not expose status_payload[\"engines\"] — engine health is invisible in prod"
        ));
    } else {
        for (needle, label) in [
            ("embedding_status()", "embedding_status"),
            ("fast_stamp_status()", "fast_stamp_status"),
        ] {
            if !router_src.contains(needle) {
                warnings.push(format!(
                    "{ROUTER_PY} /status does not call {label} — that engine's health is unreported"
                ));
            }
        }
    }

    evidence.push(EvidenceItem {
        kind: "health-audit-engine-health".to_string(),
        path: router.display().to_string(),
        line: None,
        detail: "both engines surfaced on the live /status".to_string(),
    });

    let summary = if warnings.is_empty() {
        "health-audit-engine-health: fast_stamp + embedding degradation visible on /status"
            .to_string()
    } else {
        format!(
            "health-audit-engine-health: {} health-surface issue(s)",
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
        query_id: query_id("doctor.health-audit-engine-health"),
        kind: "doctor".to_string(),
        summary,
        confidence,
        entities: vec![json!({
            "fast_stamp_status": adapter_src.contains("def fast_stamp_status("),
            "embedding_status": engine_src.contains("def embedding_status("),
            "engines_surfaced": router_src.contains("status_payload[\"engines\"]"),
        })],
        evidence,
        warnings,
        meta: Some(json!({
            "adapter": ADAPTER_PY,
            "engine": ENGINE_PY,
            "router": ROUTER_PY,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}
