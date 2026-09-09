use std::fs;
use std::path::Path;
use std::process::Command;

use leio_code::doctors::health_audit_audit_trail_durability::doctor_health_audit_audit_trail_durability;
use leio_code::doctors::{ci_doctor_names, doctor_names_for_profile};
use tempfile::TempDir;

fn write(root: &Path, relative: &str, body: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture directory");
    }
    fs::write(path, body).expect("write fixture");
}

fn write_complete_contract(root: &Path) {
    write(
        root,
        "cartridges/health_audit/provenance.py",
        r#"
class ProofPersistenceStatus:
    PENDING = "pending"
    COMPLETE = "complete"
    FAILED = "failed"
class ProofPersistenceResult:
    pass
"#,
    );
    write(
        root,
        "cartridges/health_audit/router.py",
        r#"
def reconcile_audit_run_provenance():
    reconcile_audit_trail_rows([], authoritative_run_ids={run_id})
    if ProofPersistenceStatus.PENDING:
        pass
    if ProofPersistenceStatus.COMPLETE:
        return {"requires_human_review": True}
    raise ValueError("automatic glosa item(s) missing proof rows")
def _apply_proof_persistence_result():
    pass
"#,
    );
    write(
        root,
        "cartridges/health_audit/governance_store.py",
        r#"
def reconcile_audit_trail_rows():
    authoritative_run_ids = None
    removed_stale = 0
    run_id = finding_id = None
def replace_rule_precedence():
    execute("BEGIN TRANSACTION")
    execute("ROLLBACK")
def _connection():
    with self._lock, self._lock_path.open("a+b") as lock_file:
        fcntl.flock(
            lock_file.fileno(),
            fcntl.LOCK_SH if self._read_only else fcntl.LOCK_EX,
        )
        try:
            with self._connection() as con:
                pass
        finally:
            con.close()
            fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)
"#,
    );
    write(
        root,
        "cartridges/health_audit/tests/test_governance_store.py",
        r#"
def test_shared_governance_store_serializes_writers_across_processes():
    assert writer_done.wait(timeout=5)
    assert processes[0].is_alive()
"#,
    );
    write(
        root,
        "cartridges/health_audit/routes/governance.py",
        r#"
@router.put("/governance/precedence")
def route():
    return replace_rule_precedence([])
"#,
    );
    write(
        root,
        "cartridges/health_audit/authz.py",
        r#"
HA_RULES_MANAGE = "ha:rules:manage"
POLICY = (("PUT", "/v2/health-audit/governance/precedence"), HA_RULES_MANAGE)
"#,
    );
    write(
        root,
        "cartridges/health_audit/tests/test_audit_trail_durability.py",
        r#"
def test_persist_run_records_pending_before_projection():
    pass
def test_persisted_run_marks_automatic_items_for_review_when_proof_fails():
    pass
def test_reconcile():
    repeated = ha_router.reconcile_audit_run_provenance(
def test_reconcile_audit_run_provenance_removes_stale_finding_rows():
    pass
"#,
    );
    write(
        root,
        "cartridges/health_audit/tiss_store.py",
        r#"
def _connection():
    fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX)
    try:
        with self._connection() as con:
            pass
    finally:
        con.close()
"#,
    );
    write(
        root,
        "cartridges/health_audit/tests/test_tiss_store.py",
        r#"
def test_shared_index_serializes_writers_across_processes():
    pass
"#,
    );
}

#[test]
fn doctor_is_registered_for_example_and_ci() {
    assert!(doctor_names_for_profile("example").contains(&"health-audit-audit-trail-durability"));
    assert!(ci_doctor_names().contains(&"health-audit-audit-trail-durability"));
}

#[test]
fn doctor_is_exposed_by_cli() {
    let output = Command::new(env!("CARGO_BIN_EXE_leio-code"))
        .args(["doctor", "--help"])
        .output()
        .expect("run doctor help");
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("health-audit-audit-trail-durability")
    );
}

#[test]
fn complete_contract_passes() {
    let tmp = TempDir::new().expect("tempdir");
    write_complete_contract(tmp.path());

    let result = doctor_health_audit_audit_trail_durability(tmp.path());

    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
}

#[test]
fn missing_fail_closed_marker_warns() {
    let tmp = TempDir::new().expect("tempdir");
    write_complete_contract(tmp.path());
    let router = tmp.path().join("cartridges/health_audit/router.py");
    let source = fs::read_to_string(&router).expect("read router");
    fs::write(
        &router,
        source.replace("\"requires_human_review\"", "\"review\""),
    )
    .expect("mutate fixture");

    let result = doctor_health_audit_audit_trail_durability(tmp.path());

    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("fail-closed human-review"))
    );
}

#[test]
fn missing_regression_test_warns_instead_of_skipping() {
    let tmp = TempDir::new().expect("tempdir");
    write_complete_contract(tmp.path());
    fs::remove_file(
        tmp.path()
            .join("cartridges/health_audit/tests/test_audit_trail_durability.py"),
    )
    .expect("remove fixture test");

    let result = doctor_health_audit_audit_trail_durability(tmp.path());

    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("test_audit_trail_durability.py is missing"))
    );
}

#[test]
fn missing_tiss_interprocess_lock_warns() {
    let tmp = TempDir::new().expect("tempdir");
    write_complete_contract(tmp.path());
    let tiss_store = tmp.path().join("cartridges/health_audit/tiss_store.py");
    let source = fs::read_to_string(&tiss_store).expect("read TISS store");
    fs::write(
        &tiss_store,
        source.replace(
            "fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX)",
            "threading.Lock()",
        ),
    )
    .expect("mutate fixture");

    let result = doctor_health_audit_audit_trail_durability(tmp.path());

    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("inter-process TISS-index lock"))
    );
}

#[test]
fn missing_governance_interprocess_lock_warns() {
    let tmp = TempDir::new().expect("tempdir");
    write_complete_contract(tmp.path());
    let store = tmp
        .path()
        .join("cartridges/health_audit/governance_store.py");
    let source = fs::read_to_string(&store).expect("read governance store");
    fs::write(
        &store,
        source.replace(
            "fcntl.LOCK_SH if self._read_only else fcntl.LOCK_EX",
            "threading.Lock()",
        ),
    )
    .expect("mutate fixture");

    let result = doctor_health_audit_audit_trail_durability(tmp.path());

    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("inter-process governance-store"))
    );
}

#[test]
fn retained_governance_connection_warns() {
    let tmp = TempDir::new().expect("tempdir");
    write_complete_contract(tmp.path());
    let store = tmp
        .path()
        .join("cartridges/health_audit/governance_store.py");
    let source = fs::read_to_string(&store).expect("read governance store");
    fs::write(
        &store,
        format!("{source}\nself._con = duckdb.connect(path)\n"),
    )
    .expect("mutate fixture");

    let result = doctor_health_audit_audit_trail_durability(tmp.path());

    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("process-wide DuckDB connection"))
    );
}

#[test]
fn missing_governance_lock_release_warns() {
    let tmp = TempDir::new().expect("tempdir");
    write_complete_contract(tmp.path());
    let store = tmp
        .path()
        .join("cartridges/health_audit/governance_store.py");
    let source = fs::read_to_string(&store).expect("read governance store");
    fs::write(
        &store,
        source.replace(
            "fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)",
            "pass # lock release removed",
        ),
    )
    .expect("mutate fixture");

    let result = doctor_health_audit_audit_trail_durability(tmp.path());

    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("explicit governance lock-file release"))
    );
}
