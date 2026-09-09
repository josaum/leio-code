// Rust guideline compliant 2026-07-04
//! Composite gate for Python control-plane ↔ Rust data-plane contract drift.
//!
//! Runs the cross-language boundary doctors in parallel and adds a few
//! seam-specific checks that do not belong in any single child doctor:
//! - Flight action/command constants must come from the native wheel, not a
//!   silent Python string fallback in `contracts.py`.
//! - The PyO3 wheelhouse must ship `flight_contracts_py` alongside the other
//!   sovereign boundary wheels.

use std::path::Path;
use std::time::Instant;

use rayon::prelude::*;
use serde_json::json;

use super::Doctor;
use super::{run_doctor, utils::query_id};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

/// Child doctors that enforce Python↔Rust seams. Invoked only through this
/// composite so `doctor all` does not double-run them.
pub const PY_RUST_BOUNDARY_CHILD_DOCTORS: &[&str] = &[
    "event-envelope",
    "flight-auth",
    "flight-runtime-auth",
    "duckdb-contract",
    "onboarding-drift",
    "active-cartridges-env-drift",
    "operator-tenant-parity",
    "whatsapp-bsuid-webhook",
    "whatsapp-bsuid-crm",
    "whatsapp-display-name-only",
    "ocr-flight-wiring",
    "auth-brokering",
    "inference-contracts",
    "fast-wheelhouse-contract",
];

pub struct PyRustBoundaryDoctor;

impl Doctor for PyRustBoundaryDoctor {
    fn name(&self) -> &'static str {
        "py-rust-boundary"
    }

    fn description(&self) -> &'static str {
        "Composite gate for Python control-plane ↔ Rust data-plane drift: Flight auth/schemas, events, DuckDB ownership, onboarding/env parity, WhatsApp identity, and the flight_contracts_py wheel seam."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_py_rust_boundary(index, root)
    }
}

struct ChildRow {
    name: String,
    skipped: bool,
    summary: String,
    warning_count: usize,
    evidence_count: usize,
    query_id: String,
    warnings: Vec<String>,
    evidence: Vec<EvidenceItem>,
    timing_ms: u128,
}

pub fn doctor_py_rust_boundary(index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    check_flight_contracts_seam(root, &mut warnings, &mut evidence, &mut entities);

    let mut rows: Vec<ChildRow> = PY_RUST_BOUNDARY_CHILD_DOCTORS
        .par_iter()
        .copied()
        .filter_map(|name| {
            let t0 = Instant::now();
            let envelope = run_doctor(name, index, root)?;
            let timing_ms = t0.elapsed().as_millis();
            let skipped = envelope.warnings.is_empty()
                && envelope
                    .summary
                    .contains("not available for workspace profile");
            Some(ChildRow {
                name: name.to_string(),
                skipped,
                summary: envelope.summary,
                warning_count: envelope.warnings.len(),
                evidence_count: envelope.evidence.len(),
                query_id: envelope.query_id,
                warnings: envelope.warnings,
                evidence: envelope.evidence,
                timing_ms,
            })
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));

    let mut total_warnings = warnings.len();
    let mut failing_children = 0usize;
    let mut ran_children = 0usize;
    let mut doctor_timings_ms = serde_json::Map::new();

    for row in rows {
        doctor_timings_ms.insert(row.name.clone(), json!(row.timing_ms));
        if row.skipped {
            entities.push(json!({
                "doctor": row.name,
                "skipped": true,
                "summary": row.summary,
                "timing_ms": row.timing_ms,
            }));
            continue;
        }
        ran_children += 1;
        if row.warning_count > 0 {
            failing_children += 1;
            evidence.extend(row.evidence);
        }
        total_warnings += row.warning_count;
        warnings.extend(
            row.warnings
                .iter()
                .map(|warning| format!("[{}] {}", row.name, warning)),
        );
        entities.push(json!({
            "doctor": row.name,
            "summary": row.summary,
            "warning_count": row.warning_count,
            "evidence_count": row.evidence_count,
            "query_id": row.query_id,
            "timing_ms": row.timing_ms,
        }));
    }

    let seam_warning_count = warnings
        .iter()
        .filter(|warning| warning.starts_with("[py-rust-boundary]"))
        .count();

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_py_rust_boundary"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked Python↔Rust boundary ({} child doctors + seam checks), found {} warnings ({} seam, {} child)",
            ran_children,
            total_warnings,
            seam_warning_count,
            total_warnings.saturating_sub(seam_warning_count)
        ),
        confidence: if total_warnings == 0 { 0.97 } else { 0.62 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "composite": true,
            "child_doctors": PY_RUST_BOUNDARY_CHILD_DOCTORS,
            "child_doctors_ran": ran_children,
            "failing_children": failing_children,
            "warning_count": total_warnings,
            "doctor_timings_ms": doctor_timings_ms,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn check_flight_contracts_seam(
    root: &Path,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
    entities: &mut Vec<serde_json::Value>,
) {
    let contracts_path = root.join("example-api/example/flight/contracts.py");
    let pyo3_docker_path = root.join("office-parsers-rs/Dockerfile.pyo3");
    let mut read_warnings = Vec::new();
    let contracts_src = super::utils::read_text(&contracts_path, &mut read_warnings);
    let pyo3_docker_src = super::utils::read_text(&pyo3_docker_path, &mut read_warnings);
    warnings.extend(read_warnings);

    let mut seam_checks = json!({
        "contracts_path": contracts_path.display().to_string(),
        "native_import": false,
        "silent_exception_fallback": false,
        "docker_builds_wheel": false,
    });

    if let Some(src) = contracts_src.as_deref() {
        let native_import = src.contains("import flight_contracts_py as _contracts");
        let silent_fallback =
            src.contains("except Exception:") && src.contains("_contracts = None");
        let import_error_only = src.contains("except ImportError:");

        seam_checks["native_import"] = json!(native_import);
        seam_checks["silent_exception_fallback"] = json!(silent_fallback);

        if !native_import {
            warnings.push(
                "[py-rust-boundary] example-api/example/flight/contracts.py must import flight_contracts_py as the canonical Flight action/schema surface"
                    .to_string(),
            );
        }
        if silent_fallback && !import_error_only {
            warnings.push(
                "[py-rust-boundary] example-api/example/flight/contracts.py must not swallow non-ImportError failures when loading flight_contracts_py — use `except ImportError` and fail loud in production"
                    .to_string(),
            );
        }
        if silent_fallback {
            if let Some(line) = super::utils::find_line(src, "_contracts = None") {
                evidence.push(EvidenceItem {
                    kind: "py_rust_boundary".to_string(),
                    path: contracts_path.display().to_string(),
                    line: Some(line),
                    detail: "Flight contracts fall back to Python string literals when the native wheel is absent".to_string(),
                });
            }
        } else if native_import {
            evidence.push(EvidenceItem {
                kind: "py_rust_boundary".to_string(),
                path: contracts_path.display().to_string(),
                line: super::utils::find_line(src, "import flight_contracts_py as _contracts"),
                detail: "Flight contracts require the native flight_contracts_py wheel".to_string(),
            });
        }
    } else {
        warnings
            .push("[py-rust-boundary] missing example-api/example/flight/contracts.py".to_string());
    }

    let docker_builds_wheel = pyo3_docker_src.as_deref().is_some_and(|src| {
        src.contains("flight-contracts-py/Cargo.toml") && src.contains("flight_contracts_py")
    });
    seam_checks["docker_builds_wheel"] = json!(docker_builds_wheel);
    if !docker_builds_wheel {
        warnings.push(
            "[py-rust-boundary] office-parsers-rs/Dockerfile.pyo3 must build flight_contracts_py into the API wheelhouse"
                .to_string(),
        );
    }

    entities.push(seam_checks);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn flags_silent_contract_fallback() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        fs::create_dir_all(root.join("example-api/example/flight")).expect("mkdir");
        fs::create_dir_all(root.join("office-parsers-rs")).expect("mkdir");
        fs::write(
            root.join("example-api/example/flight/contracts.py"),
            r#"
try:
    import flight_contracts_py as _contracts
except Exception:
    _contracts = None
"#,
        )
        .expect("write contracts");
        fs::write(
            root.join("office-parsers-rs/Dockerfile.pyo3"),
            "flight-contracts-py/Cargo.toml\nflight_contracts_py",
        )
        .expect("write dockerfile");

        let mut warnings = Vec::new();
        let mut evidence = Vec::new();
        let mut entities = Vec::new();
        check_flight_contracts_seam(root, &mut warnings, &mut evidence, &mut entities);

        assert!(
            warnings.iter().any(|w| w.contains("except ImportError")),
            "expected ImportError-only guidance, got {warnings:?}"
        );
        assert!(!evidence.is_empty());
        assert_eq!(entities.len(), 1);
    }
}
