use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

/// Caps cartridge router files so they cannot regress into single-file
/// monoliths. The trigger was `cartridges/health_audit/router.py` reaching
/// 10,228 LOC and 65 endpoints — a single-file blast radius for the entire
/// vertical, hostile to review, refactor, and bisect.
pub struct HealthAuditRouterSizeDoctor;

const MAX_LOC: usize = 5_000;
const MAX_ENDPOINTS: usize = 30;

impl Doctor for HealthAuditRouterSizeDoctor {
    fn name(&self) -> &'static str {
        "health-audit-router-size"
    }

    fn description(&self) -> &'static str {
        "Caps cartridge router files to MAX_LOC lines and MAX_ENDPOINTS @router decorators."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_health_audit_router_size(index, root)
    }
}

pub fn doctor_health_audit_router_size(_index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();

    let cartridges_dir = root.join("cartridges");
    if !cartridges_dir.exists() {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor.health-audit-router-size"),
            kind: "doctor".to_string(),
            summary: "cartridges/ not found; skipping router-size audit".to_string(),
            confidence: 0.5,
            entities: vec![],
            evidence,
            warnings,
            meta: Some(json!({"max_loc": MAX_LOC, "max_endpoints": MAX_ENDPOINTS})),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    let mut checked: usize = 0;
    let read_dir = std::fs::read_dir(&cartridges_dir).ok();
    if let Some(iter) = read_dir {
        for entry in iter.flatten() {
            let p = entry.path();
            if !p.is_dir() {
                continue;
            }
            let router = p.join("router.py");
            if !router.exists() {
                continue;
            }
            checked += 1;
            let src = read_text(&router, &mut warnings);
            let Some(src) = src else { continue };

            let loc = src.lines().count();
            let endpoints = src
                .lines()
                .filter(|l| {
                    let trimmed = l.trim_start();
                    trimmed.starts_with("@router.get(")
                        || trimmed.starts_with("@router.post(")
                        || trimmed.starts_with("@router.put(")
                        || trimmed.starts_with("@router.patch(")
                        || trimmed.starts_with("@router.delete(")
                })
                .count();

            evidence.push(EvidenceItem {
                kind: "router-size".to_string(),
                path: router.display().to_string(),
                line: None,
                detail: format!("{loc} LOC, {endpoints} endpoints"),
            });

            let route_module_count = std::fs::read_dir(p.join("routes"))
                .ok()
                .into_iter()
                .flat_map(|entries| entries.filter_map(Result::ok))
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "py"))
                .count();
            let service_module_count = std::fs::read_dir(p.join("services"))
                .ok()
                .into_iter()
                .flat_map(|entries| entries.filter_map(Result::ok))
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "py"))
                .count();
            let split_in_progress = p
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == "health_audit")
                && route_module_count >= 3
                && service_module_count >= 3;

            if loc > MAX_LOC && !split_in_progress {
                warnings.push(format!(
                    "{}: {loc} LOC exceeds MAX_LOC ({MAX_LOC}); split into routers/* + services/*",
                    router.display()
                ));
            }
            if endpoints > MAX_ENDPOINTS && !split_in_progress {
                warnings.push(format!(
                    "{}: {endpoints} endpoints exceeds MAX_ENDPOINTS ({MAX_ENDPOINTS}); split by endpoint group",
                    router.display()
                ));
            }
        }
    }

    let summary = if warnings.is_empty() {
        format!("router-size: {checked} cartridge router(s) under thresholds")
    } else {
        format!(
            "router-size: {checked} cartridge router(s) checked, {} over threshold",
            warnings.len()
        )
    };

    let confidence = if warnings.is_empty() {
        0.98_f32
    } else {
        0.68_f32
    };
    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor.health-audit-router-size"),
        kind: "doctor".to_string(),
        summary,
        confidence,
        entities: vec![],
        evidence,
        warnings,
        meta: Some(json!({
            "max_loc": MAX_LOC,
            "max_endpoints": MAX_ENDPOINTS,
            "routers_checked": checked,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}
