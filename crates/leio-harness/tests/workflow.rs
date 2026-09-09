use leio_harness::workflow::apply;
use serde_json::json;
use std::{fs, path::Path};

#[test]
fn host_deployment_policy_applies_to_locked_step_selection() {
    use leio_harness::workflow::apply_with_deployment_policy;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let p = json!({"request":"check then deploy","constraints":[],"questions":[],"acceptance":["deploy gated"],"steps":[
        {"name":"check","kind":"check","argv":["/usr/bin/true"],"timeout_ms":1000,"retry_safe":true,"artifacts":[]},
        {"name":"deploy","kind":"deploy","argv":["/bin/sh","-c","touch deployed"],"timeout_ms":1000,"retry_safe":true,"artifacts":[]}
    ]});
    fs::write(root.join("plan.json"), serde_json::to_vec(&p).unwrap()).unwrap();
    fs::write(root.join("evidence.json"), b"{}").unwrap();
    let run = root.join("run");
    let state = apply(
        &run,
        "init",
        Some(&root.join("plan.json")),
        None,
        Some(root),
        true,
    )
    .unwrap();
    apply(
        &run,
        "evidence",
        Some(&root.join("evidence.json")),
        None,
        None,
        false,
    )
    .unwrap();
    apply(&run, "confirm", None, Some(&state.digest), None, false).unwrap();
    apply(&run, "approve", None, Some(&state.digest), None, false).unwrap();
    let checked = apply_with_deployment_policy(
        &run,
        "execute",
        None,
        Some(&state.digest),
        None,
        false,
        false,
    )
    .unwrap();
    assert_eq!(checked.completed, 1);
    assert!(
        apply_with_deployment_policy(
            &run,
            "execute",
            None,
            Some(&state.digest),
            None,
            false,
            false
        )
        .unwrap_err()
        .to_string()
        .contains("deployment disabled")
    );
    assert!(!root.join("deployed").exists());
    assert_eq!(
        apply(&run, "show", None, None, None, false)
            .unwrap()
            .completed,
        1
    );
}

#[test]
fn workflow_revision_archives_evidence_and_approval() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let digest = ready(root, false, "printf built > artifact.txt");
    let run = root.join("run");
    apply(
        &run,
        "revise",
        Some(&root.join("plan.json")),
        None,
        None,
        false,
    )
    .unwrap();
    let reopened = apply(&run, "show", None, None, None, false).unwrap();
    assert!(reopened.evidence.is_empty());
    assert!(!reopened.confirmed && !reopened.approved);
    let archived = reopened
        .history
        .iter()
        .find(|e| e["event"] == "superseded")
        .unwrap();
    assert_eq!(archived["revision"], 1);
    assert_eq!(archived["detail"]["evidence"][0]["coverage"], "fixture");
    assert_eq!(archived["detail"]["digest"], digest);
    assert_eq!(archived["detail"]["approved"], true);
    assert_eq!(archived["detail"]["confirmed"], true);
}

#[test]
fn workflow_repeated_plan_rejects_stale_revision_approval() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let old = ready(root, false, "printf built > artifact.txt");
    let run = root.join("run");
    plan(
        &root.join("other.json"),
        "printf changed > artifact.txt",
        false,
    );
    apply(
        &run,
        "revise",
        Some(&root.join("other.json")),
        None,
        None,
        false,
    )
    .unwrap();
    let state = apply(
        &run,
        "revise",
        Some(&root.join("plan.json")),
        None,
        None,
        false,
    )
    .unwrap();
    assert_eq!(state.revision, 3);
    assert_ne!(state.digest, old);
    apply(
        &run,
        "evidence",
        Some(&root.join("evidence.json")),
        None,
        None,
        false,
    )
    .unwrap();
    assert!(apply(&run, "confirm", None, Some(&old), None, false).is_err());
    apply(&run, "confirm", None, Some(&state.digest), None, false).unwrap();
    assert!(apply(&run, "approve", None, Some(&old), None, false).is_err());
    apply(&run, "approve", None, Some(&state.digest), None, false).unwrap();
    assert!(apply(&run, "execute", None, Some(&old), None, false).is_err());
}

#[test]
fn workflow_adapter_error_requires_reconciliation_before_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let digest = ready(root, true, "true");
    let run = root.join("run");
    fs::create_dir_all(run.join("attempt-1/stdout.log")).unwrap();
    let state = apply(&run, "execute", None, Some(&digest), None, false).unwrap();
    assert_eq!(state.state, "running");
    assert!(!state.approved);
    assert_eq!(state.completed, 0);
    assert!(apply(&run, "execute", None, Some(&digest), None, false).is_err());
    assert!(apply(&run, "approve", None, Some(&digest), None, false).is_err());
    assert!(
        apply(
            &run,
            "revise",
            Some(&root.join("plan.json")),
            None,
            None,
            false
        )
        .is_err()
    );
    fs::write(
        root.join("note.txt"),
        "The command has stopped and its effects have been inspected.",
    )
    .unwrap();
    let reconciled = apply(
        &run,
        "reconcile",
        Some(&root.join("note.txt")),
        Some(&digest),
        None,
        false,
    )
    .unwrap();
    assert_eq!(reconciled.state, "failed");
    assert!(!reconciled.approved);
}

#[test]
fn workflow_failed_command_retains_partial_artifact_hash() {
    use sha2::{Digest, Sha256};
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let digest = ready(root, false, "printf partial > artifact.txt; exit 1");
    let run = root.join("run");
    apply(&run, "execute", None, Some(&digest), None, false).unwrap();
    let state = apply(&run, "show", None, None, None, false).unwrap();
    assert_eq!(state.state, "failed");
    assert_eq!(state.completed, 0);
    let result = state
        .history
        .iter()
        .find(|e| e["event"] == "result")
        .unwrap();
    assert_eq!(result["detail"]["passed"], false);
    assert_eq!(result["detail"]["artifacts"][0]["path"], "artifact.txt");
    assert_eq!(
        result["detail"]["artifacts"][0]["sha256"],
        hex::encode(Sha256::digest(b"partial"))
    );
}
fn plan(path: &Path, command: &str, retry: bool) {
    fs::write(path, serde_json::to_vec(&json!({"request":"Create an artifact","constraints":[],"questions":[],"acceptance":["artifact exists"],"steps":[{"name":"build","kind":"build","argv":["/bin/sh","-c",command],"timeout_ms":5000,"retry_safe":retry,"artifacts":["artifact.txt"]}]})).unwrap()).unwrap();
}
fn ready(root: &Path, retry: bool, cmd: &str) -> String {
    plan(&root.join("plan.json"), cmd, retry);
    let run = root.join("run");
    let state = apply(
        &run,
        "init",
        Some(&root.join("plan.json")),
        None,
        Some(root),
        false,
    )
    .unwrap();
    fs::write(root.join("evidence.json"), "{\"coverage\":\"fixture\"}").unwrap();
    apply(
        &run,
        "evidence",
        Some(&root.join("evidence.json")),
        None,
        None,
        false,
    )
    .unwrap();
    apply(&run, "confirm", None, Some(&state.digest), None, false).unwrap();
    apply(&run, "approve", None, Some(&state.digest), None, false).unwrap();
    state.digest
}
#[test]
fn workflow_reopens_executes_and_records_real_artifact() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let digest = ready(root, false, "printf built > artifact.txt");
    let run = root.join("run");
    assert!(apply(&run, "execute", None, None, None, false).is_err());
    let state = apply(&run, "execute", None, Some(&digest), None, false).unwrap();
    assert_eq!(state.state, "completed");
    assert_eq!(state.completed, 1);
    assert_eq!(
        fs::read_to_string(root.join("artifact.txt")).unwrap(),
        "built"
    );
    assert!(
        serde_json::to_string(&state.history)
            .unwrap()
            .contains("sha256")
    );
    assert!(apply(&run, "execute", None, Some(&digest), None, false).is_err());
    let state = apply(
        &run,
        "notification-failed",
        None,
        Some("delivery unavailable"),
        None,
        false,
    )
    .unwrap();
    assert_eq!(state.state, "completed");
    assert_eq!(
        apply(&run, "show", None, None, None, false)
            .unwrap()
            .completed,
        1
    );
}
#[test]
fn workflow_revision_invalidates_approval_and_retains_history() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let old = ready(root, false, "printf built > artifact.txt");
    plan(
        &root.join("new.json"),
        "printf changed > artifact.txt",
        false,
    );
    let run = root.join("run");
    let state = apply(
        &run,
        "revise",
        Some(&root.join("new.json")),
        None,
        None,
        false,
    )
    .unwrap();
    assert!(!state.approved && !state.confirmed);
    assert_eq!(state.revision, 2);
    assert!(!state.history.is_empty());
    assert!(apply(&run, "execute", None, Some(&old), None, false).is_err());
}
#[test]
fn workflow_failed_unsafe_step_cannot_replay() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let digest = ready(root, false, "printf effect >> effects.txt; exit 1");
    let run = root.join("run");
    assert_eq!(
        apply(&run, "execute", None, Some(&digest), None, false)
            .unwrap()
            .state,
        "failed"
    );
    assert!(apply(&run, "execute", None, Some(&digest), None, false).is_err());
    assert_eq!(
        fs::read_to_string(root.join("effects.txt")).unwrap(),
        "effect"
    );
}
#[test]
fn workflow_exit_zero_without_artifact_is_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let digest = ready(root, true, "true");
    let state = apply(
        &root.join("run"),
        "execute",
        None,
        Some(&digest),
        None,
        false,
    )
    .unwrap();
    assert_eq!(state.state, "failed");
    assert_eq!(state.completed, 0);
}
#[test]
fn workflow_concurrent_owner_prevents_execution() {
    use fs2::FileExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let digest = ready(root, false, "printf built > artifact.txt");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("run/lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();
    assert!(
        apply(
            &root.join("run"),
            "execute",
            None,
            Some(&digest),
            None,
            false
        )
        .is_err()
    );
    assert!(!root.join("artifact.txt").exists());
}
#[test]
fn workflow_persists_jsonld_identity_and_activity_provenance() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    ready(root, false, "printf built > artifact.txt");
    let doc: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("run/state.jsonld")).unwrap()).unwrap();
    assert_eq!(doc["@type"], "Workflow");
    assert!(
        doc["@id"]
            .as_str()
            .unwrap()
            .starts_with("urn:leio:workflow:run:")
    );
    assert_eq!(doc["@context"]["prov"], "http://www.w3.org/ns/prov#");
    assert_eq!(doc["history"][0]["@type"], "prov:Activity");
}
#[test]
fn workflow_disabled_deploy_is_not_spawned() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    ready(root, false, "printf deployed > artifact.txt");
    let mut p: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("plan.json")).unwrap()).unwrap();
    p["steps"][0]["kind"] = json!("deploy");
    fs::write(root.join("plan.json"), serde_json::to_vec(&p).unwrap()).unwrap();
    let run = root.join("run");
    let state = apply(
        &run,
        "revise",
        Some(&root.join("plan.json")),
        None,
        None,
        false,
    )
    .unwrap();
    apply(
        &run,
        "evidence",
        Some(&root.join("evidence.json")),
        None,
        None,
        false,
    )
    .unwrap();
    apply(&run, "confirm", None, Some(&state.digest), None, false).unwrap();
    apply(&run, "approve", None, Some(&state.digest), None, false).unwrap();
    assert!(apply(&run, "execute", None, Some(&state.digest), None, false).is_err());
    assert!(!root.join("artifact.txt").exists());
}

#[test]
fn workflow_safe_retry_preserves_approved_inputs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let digest = ready(root, true, "test -f ready && printf built > artifact.txt");
    let run = root.join("run");
    assert_eq!(
        apply(&run, "execute", None, Some(&digest), None, false)
            .unwrap()
            .state,
        "failed"
    );
    fs::write(root.join("ready"), "").unwrap();
    let state = apply(&run, "execute", None, Some(&digest), None, false).unwrap();
    assert_eq!(state.state, "completed");
    assert_eq!(state.attempts, 2);
    assert_eq!(state.digest, digest);
    assert_eq!(state.revision, 1);
}

#[test]
fn workflow_interrupted_step_requires_reconciliation_and_reapproval() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let digest = ready(root, true, "printf built > artifact.txt");
    let path = root.join("run/state.jsonld");
    let mut state: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    state["state"] = json!("running");
    fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    let run = root.join("run");
    assert!(apply(&run, "execute", None, Some(&digest), None, false).is_err());
    fs::write(
        root.join("note.txt"),
        "Process stopped; no artifact or external effect observed.",
    )
    .unwrap();
    let state = apply(
        &run,
        "reconcile",
        Some(&root.join("note.txt")),
        Some(&digest),
        None,
        false,
    )
    .unwrap();
    assert!(!state.approved);
    assert!(apply(&run, "execute", None, Some(&digest), None, false).is_err());
}

#[test]
fn workflow_reapproval_cannot_bypass_unsafe_retry_guard() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let digest = ready(root, false, "printf effect >> effects.txt; exit 1");
    let run = root.join("run");
    apply(&run, "execute", None, Some(&digest), None, false).unwrap();
    let state = apply(&run, "approve", None, Some(&digest), None, false).unwrap();
    assert_eq!(state.state, "failed");
    assert!(apply(&run, "execute", None, Some(&digest), None, false).is_err());
    assert_eq!(
        fs::read_to_string(root.join("effects.txt")).unwrap(),
        "effect"
    );
}

#[test]
fn workflow_partial_results_survive_later_failure_and_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    ready(root, false, "printf built >> artifact.txt");
    let mut plan: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("plan.json")).unwrap()).unwrap();
    plan["steps"].as_array_mut().unwrap().push(json!({"name":"check","kind":"check","argv":["/bin/sh","-c","test -f ready"],"timeout_ms":5000,"retry_safe":true,"artifacts":[]}));
    fs::write(root.join("plan.json"), serde_json::to_vec(&plan).unwrap()).unwrap();
    let run = root.join("run");
    let digest = apply(
        &run,
        "revise",
        Some(&root.join("plan.json")),
        None,
        None,
        false,
    )
    .unwrap()
    .digest;
    apply(
        &run,
        "evidence",
        Some(&root.join("evidence.json")),
        None,
        None,
        false,
    )
    .unwrap();
    apply(&run, "confirm", None, Some(&digest), None, false).unwrap();
    apply(&run, "approve", None, Some(&digest), None, false).unwrap();
    assert_eq!(
        apply(&run, "execute", None, Some(&digest), None, false)
            .unwrap()
            .completed,
        1
    );
    let failed = apply(&run, "execute", None, Some(&digest), None, false).unwrap();
    assert_eq!(failed.state, "failed");
    assert_eq!(failed.completed, 1);
    fs::write(root.join("ready"), "").unwrap();
    assert_eq!(
        apply(&run, "execute", None, Some(&digest), None, false)
            .unwrap()
            .state,
        "completed"
    );
    assert_eq!(
        fs::read_to_string(root.join("artifact.txt")).unwrap(),
        "built"
    );
}
