//! Integration tests for the Phase 7 (P0 #2) CLI polish: `find binary`,
//! `find route`, `find callers`, `explain binary`, `explain route`.
//!
//! These exercise the full CLI surface end-to-end by shelling out to the
//! `leio-code` binary against a synthetic tempdir that declares one Cargo
//! binary, one Flask route, and one Python subprocess caller of the binary.
//! The fixture is intentionally minimal so the assertions stay anchored on
//! the new query/CLI plumbing rather than on the depth of the detectors.

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_leio-code")
}

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(&path, body).expect("write file");
}

/// Stage a synthetic polyglot repo:
/// - Cargo `[[bin]]` named `leio-tool` (binary node)
/// - Flask route `GET /api/users` (route node)
/// - Python `subprocess.run(["leio-tool", ...])` (spawn caller)
/// - JS `fetch("/api/users")` (http caller)
/// - Python `subprocess.run([variable])` (unresolved spawn)
fn stage_repo() -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    // Disable the DuckDB sidecar so the index path is the only state.
    // SAFETY: cargo's test profile runs tests single-threaded by default;
    // we only mutate the env once per test process.
    unsafe { std::env::set_var("LEIO_DISABLE_SEARCH_SIDECAR", "1") };

    write(
        root,
        "Cargo.toml",
        "[package]\nname=\"polish-fixture\"\nversion=\"0.1.0\"\n\n[[bin]]\nname=\"leio-tool\"\npath=\"src/main.rs\"\n",
    );
    write(root, "src/main.rs", "fn main() {}\n");

    write(
        root,
        "server.py",
        "from flask import Flask\napp = Flask(__name__)\n\n@app.route(\"/api/users\", methods=[\"GET\"])\ndef users():\n    return \"ok\"\n",
    );

    // Spawn caller of `leio-tool`.
    write(
        root,
        "caller.py",
        "import subprocess\nsubprocess.run([\"leio-tool\", \"--help\"])\nx = some_var\nsubprocess.run([x, \"--help\"])\n",
    );

    // HTTP caller of `/api/users`.
    write(
        root,
        "client.js",
        "fetch(\"http://localhost/api/users\");\n",
    );

    tmp
}

/// Invoke leio-code against `repo` with the given args. Returns (stdout, stderr, exit-code).
fn run_cli(repo: &Path, args: &[&str]) -> (String, String, i32) {
    let mut cmd = Command::new(bin());
    cmd.arg("--repo").arg(repo);
    for a in args {
        cmd.arg(a);
    }
    let out = cmd.output().expect("run leio-code");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

fn parse_json(stdout: &str) -> Value {
    serde_json::from_str(stdout).expect("stdout must be JSON")
}

// ---------------------------------------------------------------------------
// `find binary`
// ---------------------------------------------------------------------------

#[test]
fn find_binary_lists_all_when_no_filter() {
    let tmp = stage_repo();
    let (stdout, _stderr, code) = run_cli(tmp.path(), &["--json", "find", "binary"]);
    assert_eq!(code, 0, "stderr: {_stderr}");
    let env = parse_json(&stdout);
    let entities = env["entities"].as_array().expect("entities array");
    assert!(
        entities.iter().any(|e| e["name"] == "leio-tool"),
        "expected leio-tool in entities, got {entities:?}"
    );
}

#[test]
fn find_binary_filters_by_substring() {
    let tmp = stage_repo();
    let (stdout, _stderr, code) = run_cli(tmp.path(), &["--json", "find", "binary", "leio"]);
    assert_eq!(code, 0, "stderr: {_stderr}");
    let env = parse_json(&stdout);
    let names: Vec<&str> = env["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap_or(""))
        .collect();
    assert!(names.iter().all(|n| n.contains("leio")), "got {names:?}");
}

// ---------------------------------------------------------------------------
// `find route`
// ---------------------------------------------------------------------------

#[test]
fn find_route_locates_flask_route() {
    let tmp = stage_repo();
    let (stdout, _stderr, code) = run_cli(tmp.path(), &["--json", "find", "route", "/api/users"]);
    assert_eq!(code, 0, "stderr: {_stderr}");
    let env = parse_json(&stdout);
    let entities = env["entities"].as_array().unwrap();
    assert_eq!(entities.len(), 1, "expected one route, got {entities:?}");
    assert_eq!(entities[0]["route"], "/api/users");
    assert_eq!(entities[0]["method"], "GET");
}

#[test]
fn find_route_filters_by_method() {
    let tmp = stage_repo();
    let (stdout, _stderr, code) =
        run_cli(tmp.path(), &["--json", "find", "route", "--method", "POST"]);
    assert_eq!(code, 0, "stderr: {_stderr}");
    let env = parse_json(&stdout);
    let entities = env["entities"].as_array().unwrap();
    // No POST routes in fixture.
    assert!(
        entities.is_empty(),
        "expected zero POST routes, got {entities:?}"
    );
}

// ---------------------------------------------------------------------------
// `find callers` (dispatched)
// ---------------------------------------------------------------------------

#[test]
fn find_callers_binary_dispatches_to_spawn_lookup() {
    let tmp = stage_repo();
    let (stdout, _stderr, code) = run_cli(tmp.path(), &["--json", "find", "callers", "leio-tool"]);
    assert_eq!(code, 0, "stderr: {_stderr}");
    let env = parse_json(&stdout);
    // The query_id prefix should be the binary-mode one so JSON-LD can
    // discriminate downstream consumers.
    let qid = env["query_id"].as_str().unwrap();
    assert!(
        qid.starts_with("find_callers_binary-"),
        "expected find_callers_binary query_id, got {qid}"
    );
    let entities = env["entities"].as_array().unwrap();
    assert!(
        entities.iter().any(|e| e["path"] == "caller.py"),
        "expected caller.py in entities, got {entities:?}"
    );
}

#[test]
fn find_callers_route_dispatches_to_http_lookup() {
    let tmp = stage_repo();
    let (stdout, _stderr, code) = run_cli(tmp.path(), &["--json", "find", "callers", "/api/users"]);
    assert_eq!(code, 0, "stderr: {_stderr}");
    let env = parse_json(&stdout);
    let qid = env["query_id"].as_str().unwrap();
    assert!(
        qid.starts_with("find_callers_route-"),
        "expected find_callers_route query_id, got {qid}"
    );
    let entities = env["entities"].as_array().unwrap();
    assert!(
        entities.iter().any(|e| e["caller_path"] == "client.js"),
        "expected client.js in entities, got {entities:?}"
    );
}

// ---------------------------------------------------------------------------
// `explain binary` / `explain route`
// ---------------------------------------------------------------------------

#[test]
fn explain_binary_combines_declaration_callers_and_unresolved() {
    let tmp = stage_repo();
    let (stdout, _stderr, code) =
        run_cli(tmp.path(), &["--json", "explain", "binary", "leio-tool"]);
    assert_eq!(code, 0, "stderr: {_stderr}");
    let env = parse_json(&stdout);
    let entities = env["entities"].as_array().unwrap();
    // Must include the binary itself + at least one caller.
    let kinds: Vec<&str> = entities
        .iter()
        .map(|e| e["@kind"].as_str().unwrap_or(""))
        .collect();
    assert!(
        kinds.contains(&"Binary"),
        "expected Binary entity, kinds={kinds:?}"
    );
    assert!(
        kinds.contains(&"SpawnCallSite"),
        "expected SpawnCallSite entity, kinds={kinds:?}"
    );
    // Meta should carry the unresolved-by-reason grouping.
    let meta = &env["meta"];
    assert!(
        meta.get("unresolved_by_reason").is_some(),
        "expected meta.unresolved_by_reason"
    );
}

#[test]
fn explain_route_combines_declaration_and_http_callers() {
    let tmp = stage_repo();
    let (stdout, _stderr, code) =
        run_cli(tmp.path(), &["--json", "explain", "route", "/api/users"]);
    assert_eq!(code, 0, "stderr: {_stderr}");
    let env = parse_json(&stdout);
    let entities = env["entities"].as_array().unwrap();
    let kinds: Vec<&str> = entities
        .iter()
        .map(|e| e["@kind"].as_str().unwrap_or(""))
        .collect();
    assert!(kinds.contains(&"Route"), "kinds={kinds:?}");
    assert!(kinds.contains(&"HttpCallSite"), "kinds={kinds:?}");
}

// ---------------------------------------------------------------------------
// JSON-LD `@context` + per-entity `@type`
// ---------------------------------------------------------------------------

#[test]
fn jsonld_find_binary_emits_context_and_type() {
    let tmp = stage_repo();
    let (stdout, _stderr, code) = run_cli(tmp.path(), &["find", "binary", "--format", "jsonld"]);
    assert_eq!(code, 0, "stderr: {_stderr}");
    let env = parse_json(&stdout);
    assert_eq!(env["@context"], "https://ontology.getjai.com/leio-code/v1#");
    assert_eq!(env["@type"], "FindResult");
    let entities = env["entities"].as_array().unwrap();
    assert!(!entities.is_empty(), "expected at least one binary");
    for e in entities {
        assert_eq!(e["@type"], "Binary", "every entity must have @type=Binary");
    }
}

#[test]
fn jsonld_find_callers_binary_emits_spawn_call_site_type() {
    let tmp = stage_repo();
    let (stdout, _stderr, code) = run_cli(
        tmp.path(),
        &["find", "callers", "leio-tool", "--format", "jsonld"],
    );
    assert_eq!(code, 0, "stderr: {_stderr}");
    let env = parse_json(&stdout);
    let entities = env["entities"].as_array().unwrap();
    for e in entities {
        assert_eq!(e["@type"], "SpawnCallSite");
    }
}

#[test]
fn jsonld_find_callers_route_emits_http_call_site_type() {
    let tmp = stage_repo();
    let (stdout, _stderr, code) = run_cli(
        tmp.path(),
        &["find", "callers", "/api/users", "--format", "jsonld"],
    );
    assert_eq!(code, 0, "stderr: {_stderr}");
    let env = parse_json(&stdout);
    let entities = env["entities"].as_array().unwrap();
    for e in entities {
        assert_eq!(e["@type"], "HttpCallSite");
    }
}

// ---------------------------------------------------------------------------
// Text output for `find callers` surfaces grouped unresolved breakdown via
// summary + meta. The text renderer prints both, so the grouped counts are
// visible to the user even without a custom block format. See
// query.rs::find_subprocess_callers for the summary string.
// ---------------------------------------------------------------------------

#[test]
fn text_find_callers_surfaces_unresolved_breakdown() {
    let tmp = stage_repo();
    let (stdout, _stderr, code) = run_cli(tmp.path(), &["find", "callers", "leio-tool"]);
    assert_eq!(code, 0, "stderr: {_stderr}");
    // The summary names both the resolved count and the unresolved breakdown.
    // We don't assert the exact count of unresolved (detectors evolve) — only
    // that the breakdown surface is present in stdout.
    assert!(
        stdout.contains("subprocess caller(s) for `leio-tool`"),
        "expected resolved-caller summary, got:\n{stdout}"
    );
    assert!(
        stdout.contains("unresolved_by_reason") || stdout.contains("unresolved"),
        "expected unresolved breakdown surface in text output, got:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// Sanity: `find binary` accepts no needle without erroring out at clap level.
// ---------------------------------------------------------------------------

#[test]
fn find_binary_no_needle_is_accepted() {
    let tmp = stage_repo();
    let (_stdout, _stderr, code) = run_cli(tmp.path(), &["find", "binary"]);
    assert_eq!(code, 0, "find binary with no needle must be accepted");
}
