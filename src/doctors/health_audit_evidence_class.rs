use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

/// Guards the glosa evidence-completeness triage label (auto_glosa vs
/// potential_glosa). Five drifts would each silently break the label's
/// contract in a can't-fail auditing domain:
///   1. the pure classifier + its two ratifiable tables must exist in the
///      policy module (`classify_glosa_evidence`, `_REQUIRED_CLAIM_FIELDS`,
///      and the class constants);
///   2. the router must WIRE the classifier into the live audit path (a
///      dropped call makes every finding silently unlabeled);
///   3. the router must SURFACE the label — per-item keys plus the summary
///      rollup that drives the review triage queue;
///   4. the default-potential invariant test must not be deleted (it is the
///      safety net that keeps `auto_glosa` a conservative allowlist);
///   5. ORTHOGONALITY — the classifier must be computed AFTER the disposition
///      is resolved, so an evidence label can never feed the denial ladder.
pub struct HealthAuditEvidenceClassDoctor;

const POLICY_PY: &str = "cartridges/health_audit/services/contract_glosa_policy.py";
const ROUTER_PY: &str = "cartridges/health_audit/router.py";
const TEST_PY: &str = "cartridges/health_audit/tests/test_evidence_completeness.py";

impl Doctor for HealthAuditEvidenceClassDoctor {
    fn name(&self) -> &'static str {
        "health-audit-evidence-class"
    }

    fn description(&self) -> &'static str {
        "The glosa evidence-completeness label (auto_glosa vs potential_glosa) \
         stays wired into the audit path, surfaced in the summary, orthogonal \
         to disposition, and defended by its default-potential invariant test."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_health_audit_evidence_class(root)
    }
}

pub fn doctor_health_audit_evidence_class(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings: Vec<String> = Vec::new();
    let mut evidence: Vec<EvidenceItem> = Vec::new();

    let policy = root.join(POLICY_PY);
    let router = root.join(ROUTER_PY);
    let test = root.join(TEST_PY);

    if !policy.exists() || !router.exists() {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor.health-audit-evidence-class"),
            kind: "doctor".to_string(),
            summary: "health-audit-evidence-class: policy/router not present; skipping".to_string(),
            confidence: 0.5,
            entities: vec![],
            evidence,
            warnings,
            meta: Some(json!({"skipped": true})),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    let policy_src = read_text(&policy, &mut warnings).unwrap_or_default();
    let router_src = read_text(&router, &mut warnings).unwrap_or_default();

    // 1. The pure classifier and its two ratifiable tables live in the policy.
    let policy_needs: [(&str, &str); 4] = [
        ("def classify_glosa_evidence(", "classifier function"),
        ("_REQUIRED_CLAIM_FIELDS", "claim-field requirement table"),
        (
            "GLOSA_CLASS_AUTO = \"auto_glosa\"",
            "auto_glosa class constant",
        ),
        (
            "GLOSA_CLASS_POTENTIAL = \"potential_glosa\"",
            "potential_glosa class constant",
        ),
    ];
    for (needle, label) in policy_needs {
        if !policy_src.contains(needle) {
            warnings.push(format!("{POLICY_PY} is missing the {label} (`{needle}`)"));
        }
    }

    // 2. The router wires the classifier into the live audit path.
    let call_offset = router_src.find("classify_glosa_evidence(");
    if call_offset.is_none() {
        warnings.push(format!(
            "{ROUTER_PY} never calls classify_glosa_evidence(...) — the evidence label is unwired"
        ));
    }

    // 3. The router surfaces the label: per-item keys + the summary rollup.
    let router_keys: [(&str, &str); 3] = [
        (
            "\"glosa_evidence_class\"",
            "per-item glosa_evidence_class key",
        ),
        (
            "\"evidence_completeness\"",
            "per-item evidence_completeness key",
        ),
        (
            "\"evidence_class_counts\"",
            "summary evidence_class_counts rollup (triage queue)",
        ),
    ];
    for (needle, label) in router_keys {
        if !router_src.contains(needle) {
            warnings.push(format!("{ROUTER_PY} does not surface the {label}"));
        }
    }

    // 4. The default-potential invariant test must not be deleted.
    if !test.exists() {
        warnings.push(format!(
            "{TEST_PY} is missing — the evidence taxonomy is untested"
        ));
    } else {
        let test_src = read_text(&test, &mut warnings).unwrap_or_default();
        if !test_src.contains("test_unmapped_rule_type_defaults_potential") {
            warnings.push(format!(
                "{TEST_PY} lost test_unmapped_rule_type_defaults_potential — the default-potential invariant is no longer locked"
            ));
        }
    }

    // 5. Orthogonality: the classifier is computed AFTER the disposition is
    //    resolved, so an evidence label can never influence the denial ladder.
    if let Some(call) = call_offset {
        match router_src.find("_resolve_item_finalization(") {
            Some(finalize) if call > finalize => {}
            Some(_) => warnings.push(format!(
                "{ROUTER_PY}: classify_glosa_evidence(...) runs BEFORE _resolve_item_finalization(...) — the evidence label must be orthogonal to (computed after) disposition"
            )),
            None => warnings.push(format!(
                "{ROUTER_PY}: cannot confirm evidence/disposition ordering (_resolve_item_finalization not found)"
            )),
        }
    }

    evidence.push(EvidenceItem {
        kind: "health-audit-evidence-class".to_string(),
        path: policy.display().to_string(),
        line: None,
        detail: "classifier + ratifiable tables (auto allowlist, required-claim-field map)"
            .to_string(),
    });
    evidence.push(EvidenceItem {
        kind: "health-audit-evidence-class".to_string(),
        path: router.display().to_string(),
        line: None,
        detail: "wired post-disposition; surfaces per-item label + summary triage rollup"
            .to_string(),
    });

    let summary = if warnings.is_empty() {
        "health-audit-evidence-class: label wired, surfaced, orthogonal, and invariant-locked"
            .to_string()
    } else {
        format!(
            "health-audit-evidence-class: {} contract issue(s)",
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
        query_id: query_id("doctor.health-audit-evidence-class"),
        kind: "doctor".to_string(),
        summary,
        confidence,
        entities: vec![json!({
            "classifier_wired": call_offset.is_some(),
            "invariant_test_present": test.exists(),
        })],
        evidence,
        warnings,
        meta: Some(json!({
            "policy": POLICY_PY,
            "router": ROUTER_PY,
            "test": TEST_PY,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}
