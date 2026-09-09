//! Checks ANS/TISS XSD bootstrap wiring under `cartridges/health_audit`.
//!
//! The actual XSD bundles live under gitignored `raw-data`; a clean source
//! checkout should not fail `doctor all` because operator data is absent. This
//! doctor fails source drift (missing scripts/validation wiring) and reports
//! missing local bundles as bootstrap evidence.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct HealthAuditAnsXsdDoctor;

impl Doctor for HealthAuditAnsXsdDoctor {
    fn name(&self) -> &'static str {
        "health-audit-ans-xsd"
    }

    fn description(&self) -> &'static str {
        "Checks Health Audit ANS XSD install: gov.br arquivos_schemas_ans_* trees (TISS packs, DIOPS, SIB, SIP, ressarcimento, OGU) and bootstrap scripts."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_health_audit_ans_xsd(index, root)
    }
}

fn resolve_schemas_root(root: &Path) -> PathBuf {
    let preferred = root.join("cartridges/health_audit/raw-data/reference/ans_schemas");
    let legacy = root.join("cartridges/health_audit/raw-data/zips");
    if preferred.join("arquivos_schemas_ans_tiss").is_dir()
        || preferred.join("arquivos_schemas_ans_diops").is_dir()
    {
        return preferred;
    }
    if legacy.join("arquivos_schemas_ans_tiss").is_dir() {
        return legacy;
    }
    preferred
}

pub fn doctor_health_audit_ans_xsd(_index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let schemas_root = resolve_schemas_root(root);
    let install_script = root.join("cartridges/health_audit/scripts/install_ans_schemas.py");
    let bootstrap_script = root.join("cartridges/health_audit/scripts/bootstrap_ans_xsd.py");
    let xsd_validation = root.join("cartridges/health_audit/xsd_validation.py");

    let checks: &[(&str, PathBuf)] = &[
        (
            "TISS root XSD",
            schemas_root
                .join("arquivos_schemas_ans_tiss")
                .join("tissV4_01_00.xsd"),
        ),
        (
            "TISS 4.02 pack",
            schemas_root
                .join("arquivos_schemas_ans_tiss")
                .join("tiss_pack_v4_02_00")
                .join("tissV4_02_00.xsd"),
        ),
        (
            "DIOPS",
            schemas_root
                .join("arquivos_schemas_ans_diops")
                .join("Diops2024.xsd"),
        ),
        (
            "SIB",
            schemas_root
                .join("arquivos_schemas_ans_sib")
                .join("sib.xsd"),
        ),
        (
            "SIP",
            schemas_root
                .join("arquivos_schemas_ans_sip")
                .join("sipV1_02.xsd"),
        ),
        (
            "Ressarcimento",
            schemas_root
                .join("arquivos_schemas_ans_ressarcimento")
                .join("ressarcV2_00.xsd"),
        ),
        (
            "OGU",
            schemas_root
                .join("arquivos_schemas_ans_ogu")
                .join("extracao.xsd"),
        ),
    ];

    let mut present = 0usize;
    for (label, path) in checks {
        if path.is_file() {
            present += 1;
            evidence.push(EvidenceItem {
                kind: "schema".to_string(),
                path: path.display().to_string(),
                line: None,
                detail: format!("ANS XSD present ({label})"),
            });
        } else {
            evidence.push(EvidenceItem {
                kind: "bootstrap_missing".to_string(),
                path: path.strip_prefix(root).unwrap_or(path).display().to_string(),
                line: None,
                detail: format!(
                    "local ANS XSD bundle marker absent ({label}); run `make health-audit-ans-bootstrap` when validating the local Health Audit data plane"
                ),
            });
        }
    }

    if install_script.is_file() {
        evidence.push(EvidenceItem {
            kind: "code".to_string(),
            path: install_script.display().to_string(),
            line: None,
            detail: "install_ans_schemas.py (all gov.br zips)".to_string(),
        });
    }
    if bootstrap_script.is_file() {
        evidence.push(EvidenceItem {
            kind: "code".to_string(),
            path: bootstrap_script.display().to_string(),
            line: None,
            detail: "bootstrap_ans_xsd.py (xsd_fast + zips + verify)".to_string(),
        });
    }
    if let Some(src) = read_text(&xsd_validation, &mut warnings)
        && let Some(line) = find_line(&src, "def validate_ans_xml")
    {
        evidence.push(EvidenceItem {
            kind: "code".to_string(),
            path: xsd_validation.display().to_string(),
            line: Some(line),
            detail: "validate_ans_xml entry point".to_string(),
        });
    }

    if !install_script.is_file() {
        warnings.push(
            "missing Health Audit ANS schema installer script: cartridges/health_audit/scripts/install_ans_schemas.py"
                .to_string(),
        );
    }
    if !bootstrap_script.is_file() {
        warnings.push(
            "missing Health Audit ANS bootstrap script: cartridges/health_audit/scripts/bootstrap_ans_xsd.py"
                .to_string(),
        );
    }
    if !schemas_root.exists() || present == 0 {
        evidence.push(EvidenceItem {
            kind: "bootstrap_missing".to_string(),
            path: schemas_root.display().to_string(),
            line: None,
            detail:
                "gitignored local ANS schema tree absent; source checkout keeps bootstrap scripts instead of committed XSD data"
                    .to_string(),
        });
    }

    entities.push(json!({
        "doctor": "health-audit-ans-xsd",
        "schemas_root": schemas_root.display().to_string(),
        "expected_bundle_files_present": present,
        "expected_bundle_files_total": checks.len(),
        "install_script_present": install_script.is_file(),
        "bootstrap_script_present": bootstrap_script.is_file(),
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_health_audit_ans_xsd"),
        kind: "doctor".to_string(),
        summary: format!(
            "health-audit ANS XSD bootstrap contract: {}/{} local bundle markers present, {} warnings, {} evidence items",
            present,
            checks.len(),
            warnings.len(),
            evidence.len()
        ),
        confidence: if warnings.is_empty() { 0.96 } else { 0.72 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}
