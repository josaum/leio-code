use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

/// Guards the durable Health Audit proof contract:
/// - the primary run JSON records projection state before provenance work;
/// - incomplete proof projection fails closed to human review;
/// - audit rows are strict and idempotently reconciliable;
/// - governance rules and audit projections release DuckDB locks between operations;
/// - the shared TISS umbrella index releases DuckDB locks between operations;
/// - rule precedence changes use one transactional batch endpoint.
pub struct HealthAuditAuditTrailDurabilityDoctor;

const PROVENANCE_PY: &str = "cartridges/health_audit/provenance.py";
const ROUTER_PY: &str = "cartridges/health_audit/router.py";
const STORE_PY: &str = "cartridges/health_audit/governance_store.py";
const ROUTES_PY: &str = "cartridges/health_audit/routes/governance.py";
const AUTHZ_PY: &str = "cartridges/health_audit/authz.py";
const TEST_PY: &str = "cartridges/health_audit/tests/test_audit_trail_durability.py";
const STORE_TEST_PY: &str = "cartridges/health_audit/tests/test_governance_store.py";
const TISS_STORE_PY: &str = "cartridges/health_audit/tiss_store.py";
const TISS_TEST_PY: &str = "cartridges/health_audit/tests/test_tiss_store.py";

impl Doctor for HealthAuditAuditTrailDurabilityDoctor {
    fn name(&self) -> &'static str {
        "health-audit-audit-trail-durability"
    }

    fn description(&self) -> &'static str {
        "Health Audit proof projection remains observable and fail-closed, audit rows remain \
         strict and reconciliable, governance and TISS stores remain multi-process safe, and \
         rule precedence replacement stays atomic and authorized."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_health_audit_audit_trail_durability(root)
    }
}

fn require(source: &str, path: &str, requirements: &[(&str, &str)], warnings: &mut Vec<String>) {
    for (needle, label) in requirements {
        if !source.contains(needle) {
            warnings.push(format!("{path} is missing {label} (`{needle}`)"));
        }
    }
}

pub fn doctor_health_audit_audit_trail_durability(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();

    let required_paths = [
        PROVENANCE_PY,
        ROUTER_PY,
        STORE_PY,
        ROUTES_PY,
        AUTHZ_PY,
        TEST_PY,
        STORE_TEST_PY,
        TISS_STORE_PY,
        TISS_TEST_PY,
    ];
    if required_paths
        .iter()
        .take(5)
        .all(|path| !root.join(path).exists())
    {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor.health-audit-audit-trail-durability"),
            kind: "doctor".to_string(),
            summary:
                "health-audit-audit-trail-durability: cartridge surfaces not present; skipping"
                    .to_string(),
            confidence: 0.5,
            entities: vec![],
            evidence,
            warnings,
            meta: Some(json!({"skipped": true})),
            timing_ms: started.elapsed().as_millis(),
        };
    }
    for path in required_paths {
        if !root.join(path).exists() {
            warnings.push(format!("{path} is missing"));
        }
    }

    let provenance = read_text(&root.join(PROVENANCE_PY), &mut warnings).unwrap_or_default();
    let router = read_text(&root.join(ROUTER_PY), &mut warnings).unwrap_or_default();
    let store = read_text(&root.join(STORE_PY), &mut warnings).unwrap_or_default();
    let routes = read_text(&root.join(ROUTES_PY), &mut warnings).unwrap_or_default();
    let authz = read_text(&root.join(AUTHZ_PY), &mut warnings).unwrap_or_default();
    let tests = read_text(&root.join(TEST_PY), &mut warnings).unwrap_or_default();
    let store_tests = read_text(&root.join(STORE_TEST_PY), &mut warnings).unwrap_or_default();
    let tiss_store = read_text(&root.join(TISS_STORE_PY), &mut warnings).unwrap_or_default();
    let tiss_tests = read_text(&root.join(TISS_TEST_PY), &mut warnings).unwrap_or_default();

    require(
        &provenance,
        PROVENANCE_PY,
        &[
            (
                "class ProofPersistenceStatus",
                "proof persistence state enum",
            ),
            ("PENDING = \"pending\"", "pending proof state"),
            ("COMPLETE = \"complete\"", "complete proof state"),
            ("FAILED = \"failed\"", "failed proof state"),
            ("class ProofPersistenceResult", "structured proof result"),
        ],
        &mut warnings,
    );
    require(
        &router,
        ROUTER_PY,
        &[
            (
                "def reconcile_audit_run_provenance(",
                "idempotent provenance reconciliation entrypoint",
            ),
            (
                "ProofPersistenceStatus.PENDING",
                "durable pending state before proof projection",
            ),
            (
                "ProofPersistenceStatus.COMPLETE",
                "explicit proof completion check",
            ),
            (
                "reconcile_audit_trail_rows(",
                "audit-row reconciliation wiring",
            ),
            (
                "authoritative_run_ids={run_id}",
                "authoritative run-scoped reconciliation",
            ),
            (
                "\"requires_human_review\"",
                "fail-closed human-review projection",
            ),
            (
                "def _apply_proof_persistence_result(",
                "shared live/backfill fail-closed projection",
            ),
            (
                "automatic glosa item(s) missing proof rows",
                "automatic-glosa proof-row invariant",
            ),
        ],
        &mut warnings,
    );
    require(
        &store,
        STORE_PY,
        &[
            (
                "def reconcile_audit_trail_rows(",
                "idempotent audit-row reconciliation",
            ),
            (
                "authoritative_run_ids",
                "authoritative run identity for stale-row removal",
            ),
            ("removed_stale", "stale audit-row removal accounting"),
            (
                "def replace_rule_precedence(",
                "transactional precedence replacement",
            ),
            ("BEGIN TRANSACTION", "precedence transaction start"),
            ("ROLLBACK", "precedence transaction rollback"),
            ("run_id", "audit row run identity"),
            ("finding_id", "audit row finding identity"),
            (
                "fcntl.LOCK_SH if self._read_only else fcntl.LOCK_EX",
                "inter-process governance-store shared/read and exclusive/write lock",
            ),
            (
                "with self._lock, self._lock_path.open(\"a+b\") as lock_file:",
                "governance lock-file ownership context",
            ),
            (
                "with self._connection() as con:",
                "short-lived governance-store connections",
            ),
            (
                "con.close()",
                "governance DuckDB lock release after each operation",
            ),
            (
                "fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)",
                "explicit governance lock-file release",
            ),
        ],
        &mut warnings,
    );
    let retains_governance_connection =
        store.contains("self._con:") || store.contains("self._con =");
    if retains_governance_connection {
        warnings.push(format!(
            "{STORE_PY} retains a process-wide DuckDB connection (`self._con`)"
        ));
    }
    require(
        &routes,
        ROUTES_PY,
        &[
            (
                "@router.put(\"/governance/precedence\")",
                "atomic precedence HTTP route",
            ),
            ("replace_rule_precedence(", "atomic precedence store call"),
        ],
        &mut warnings,
    );
    require(
        &store_tests,
        STORE_TEST_PY,
        &[
            (
                "test_shared_governance_store_serializes_writers_across_processes",
                "multi-process governance-store regression test",
            ),
            (
                "assert processes[0].is_alive()",
                "holder-alive lock-release assertion",
            ),
            (
                "assert writer_done.wait(timeout=5)",
                "writer completion before holder process exit",
            ),
        ],
        &mut warnings,
    );
    require(
        &authz,
        AUTHZ_PY,
        &[
            (
                "(\"PUT\", \"/v2/health-audit/governance/precedence\")",
                "precedence authorization policy",
            ),
            ("HA_RULES_MANAGE", "rules-management permission"),
        ],
        &mut warnings,
    );
    require(
        &tests,
        TEST_PY,
        &[
            (
                "test_persist_run_records_pending_before_projection",
                "primary-before-projection regression test",
            ),
            (
                "test_persisted_run_marks_automatic_items_for_review_when_proof_fails",
                "fail-closed regression test",
            ),
            (
                "repeated = ha_router.reconcile_audit_run_provenance(",
                "reconciliation idempotence regression test",
            ),
            (
                "test_reconcile_audit_run_provenance_removes_stale_finding_rows",
                "orphaned finding-row regression test",
            ),
        ],
        &mut warnings,
    );
    require(
        &tiss_store,
        TISS_STORE_PY,
        &[
            (
                "fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX)",
                "inter-process TISS-index lock",
            ),
            (
                "with self._connection() as con:",
                "short-lived TISS-index connections",
            ),
            ("con.close()", "DuckDB lock release after each operation"),
        ],
        &mut warnings,
    );
    require(
        &tiss_tests,
        TISS_TEST_PY,
        &[(
            "test_shared_index_serializes_writers_across_processes",
            "multi-process TISS-index regression test",
        )],
        &mut warnings,
    );

    for (path, detail) in [
        (
            PROVENANCE_PY,
            "explicit pending, complete, and failed proof-projection states",
        ),
        (
            ROUTER_PY,
            "primary run persistence, reconciliation, and fail-closed review",
        ),
        (
            STORE_PY,
            "short-lived serialized connections, strict audit rows, and atomic precedence",
        ),
        (
            STORE_TEST_PY,
            "multi-process governance-store regression coverage",
        ),
        (
            TEST_PY,
            "durability, failure, and idempotence regression coverage",
        ),
        (
            TISS_STORE_PY,
            "short-lived DuckDB connections guarded across API and worker processes",
        ),
        (
            TISS_TEST_PY,
            "multi-process TISS umbrella regression coverage",
        ),
    ] {
        evidence.push(EvidenceItem {
            kind: "health-audit-audit-trail-durability".to_string(),
            path: root.join(path).display().to_string(),
            line: None,
            detail: detail.to_string(),
        });
    }

    let summary = if warnings.is_empty() {
        "health-audit-audit-trail-durability: proof persistence, reconciliation, fail-closed review, multi-process governance and TISS indexing, and atomic precedence are guarded".to_string()
    } else {
        format!(
            "health-audit-audit-trail-durability: {} contract issue(s)",
            warnings.len()
        )
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor.health-audit-audit-trail-durability"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.98 } else { 0.6 },
        entities: vec![json!({
            "proof_state_observable": provenance.contains("class ProofPersistenceStatus"),
            "reconciliation_wired": router.contains("reconcile_audit_trail_rows("),
            "precedence_atomic": store.contains("BEGIN TRANSACTION"),
            "precedence_authorized": authz.contains(
                "(\"PUT\", \"/v2/health-audit/governance/precedence\")"
            ),
            "governance_store_multiprocess_safe": store.contains("fcntl.flock(")
                && store.contains("fcntl.LOCK_SH if self._read_only else fcntl.LOCK_EX")
                && store.contains(
                "with self._lock, self._lock_path.open(\"a+b\") as lock_file:"
            ) && store.contains("con.close()")
                && store.contains("fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)")
                && !retains_governance_connection,
            "tiss_index_multiprocess_safe": tiss_store.contains(
                "fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX)"
            ) && tiss_store.contains("con.close()"),
        })],
        evidence,
        warnings,
        meta: Some(json!({
            "provenance": PROVENANCE_PY,
            "router": ROUTER_PY,
            "store": STORE_PY,
            "routes": ROUTES_PY,
            "authz": AUTHZ_PY,
            "test": TEST_PY,
            "store_test": STORE_TEST_PY,
            "tiss_store": TISS_STORE_PY,
            "tiss_test": TISS_TEST_PY,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}
