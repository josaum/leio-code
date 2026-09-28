use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_leio-harness");

fn run_json(args: &[&str]) -> Value {
    let output = Command::new(BIN).args(args).output().expect("run harness");
    assert!(
        output.status.success(),
        "leio-harness {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("command stdout is JSON")
}

fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).expect("serialize fixture")).expect("write fixture");
}

#[test]
fn shm_cli_exercises_roundtrip_and_benchmark_modes() {
    let selftest = run_json(&["shm", "selftest", "--capacity", "8"]);
    assert_eq!(selftest["status"], "ok");
    assert_eq!(selftest["shared_memory_verified"], true);
    assert_eq!(selftest["consensus"]["status"], "ok");

    let bench = run_json(&["shm", "bench", "--iterations", "32", "--dimensions", "8"]);
    assert_eq!(bench["iterations"], 32);
    assert_eq!(bench["vector_dimensions"], 8);
    assert!(bench["operations_per_sec"].as_u64().unwrap() > 0);

    let consensus = run_json(&["shm", "consensus-bench", "--iterations", "16"]);
    assert_eq!(consensus["iterations"], 16);
    assert!(consensus["consensus_commits_per_sec"].as_u64().unwrap() > 0);
}

#[test]
fn vector_and_codeview_cli_routes_emit_structured_results() {
    let fixture = TempDir::new().expect("temp fixture");
    let a = fixture.path().join("a.json");
    let b = fixture.path().join("b.json");
    let candidates = fixture.path().join("candidates.json");
    let population = fixture.path().join("population.json");
    write_json(&a, &json!({"vector": [1.0, 0.0]}));
    write_json(&b, &json!({"vector": [0.0, 1.0]}));
    write_json(
        &candidates,
        &json!([
            {"id":"quality","vector":[1.0,0.0],"scores":{"quality":0.9,"speed":0.5},"evaluations":2},
            {"id":"speed","vector":[0.0,1.0],"scores":{"quality":0.5,"speed":0.9},"evaluations":1},
            {"id":"dominated","vector":[0.5,0.5],"scores":{"quality":0.4,"speed":0.4},"evaluations":0}
        ]),
    );
    write_json(
        &population,
        &json!([
            {"vector":[1.0,0.0]},
            {"vector":[0.0,1.0]},
            {"vector":[-1.0,0.0]},
            {"vector":[0.0,-1.0]}
        ]),
    );

    let merged = run_json(&[
        "gepa",
        "merge",
        "--a",
        a.to_str().unwrap(),
        "--b",
        b.to_str().unwrap(),
        "--t",
        "0.5",
    ]);
    assert_eq!(merged["vector"].as_array().unwrap().len(), 2);

    let mutated = run_json(&[
        "gepa",
        "mutate",
        "--parent",
        a.to_str().unwrap(),
        "--strength",
        "0.2",
        "--max-angle-deg",
        "5",
        "--seed",
        "7",
    ]);
    assert_eq!(mutated["vector"].as_array().unwrap().len(), 2);

    let anchor = run_json(&[
        "gepa",
        "anchor",
        "--vector",
        a.to_str().unwrap(),
        "--seed",
        b.to_str().unwrap(),
        "--beta",
        "0.5",
    ]);
    assert_eq!(anchor["anchor_penalty"], 0.5);

    let frontier = run_json(&[
        "gepa",
        "frontier",
        "--candidates",
        candidates.to_str().unwrap(),
    ]);
    assert_eq!(frontier["frontier"].as_array().unwrap().len(), 2);
    assert_eq!(frontier["least_explored"], "speed");

    let isotropy = run_json(&[
        "sigreg",
        "isotropy",
        "--rows",
        population.to_str().unwrap(),
        "--slices",
        "8",
        "--seed",
        "9",
    ]);
    assert_eq!(isotropy["population"], 4);
    assert!(isotropy["score"].is_number());

    let codeview = run_json(&[
        "codeview",
        "check",
        "--repo",
        fixture.path().to_str().unwrap(),
    ]);
    assert_eq!(codeview["fresh"], false);
    assert!(
        codeview["reason"]
            .as_str()
            .unwrap()
            .contains("missing or stale")
    );
}

#[test]
fn lease_cli_covers_list_heartbeat_and_expiration() {
    let fixture = TempDir::new().expect("temp fixture");
    let store = fixture.path().join("leases.json");
    let lease = fixture.path().join("lease.json");
    write_json(
        &lease,
        &json!({
            "agentId": "coverage-agent",
            "runId": "coverage-run",
            "worktreePath": "/tmp/coverage-worktree",
            "branch": "agents/coverage",
            "owner": "coverage-test",
            "heartbeat": "2026-01-01T00:00:00Z",
            "state": "active",
            "lockScopes": ["worktree"]
        }),
    );

    let acquired = run_json(&[
        "lease",
        "--store",
        store.to_str().unwrap(),
        "acquire",
        "--lease",
        lease.to_str().unwrap(),
    ]);
    assert_eq!(acquired["runId"], "coverage-run");

    let listed = run_json(&["lease", "--store", store.to_str().unwrap(), "list"]);
    assert_eq!(listed.as_array().unwrap().len(), 1);

    let expired = run_json(&[
        "lease",
        "--store",
        store.to_str().unwrap(),
        "expire",
        "--stale-after-ms",
        "1",
    ]);
    assert_eq!(expired.as_array().unwrap().len(), 1);

    let reacquired = run_json(&[
        "lease",
        "--store",
        store.to_str().unwrap(),
        "acquire",
        "--lease",
        lease.to_str().unwrap(),
    ]);
    assert_eq!(reacquired["state"], "active");

    let heartbeat = run_json(&[
        "lease",
        "--store",
        store.to_str().unwrap(),
        "heartbeat",
        "--run-id",
        "coverage-run",
    ]);
    assert_eq!(heartbeat["runId"], "coverage-run");
}
