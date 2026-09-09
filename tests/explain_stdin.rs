//! Integration tests for `leio-code explain --stdin`.
//!
//! Closes the P1 #4 follow-up "find → jq → explain" loop. The test contract:
//!   1. A whole JSON-LD envelope on stdin is expanded via `entities[]`.
//!   2. A JSON array on stdin is iterated.
//!   3. A stream of concatenated/JSONL objects on stdin is iterated.
//!   4. Unknown `@type` is skipped with a stderr warning (NOT a crash).
//!   5. Missing required field (`name`/`key`/`route`) is skipped with a warning.
//!   6. `--format=jsonld` produces a JSON array of envelopes.
//!   7. `--format=text` produces dividers between envelopes.
//!   8. End-to-end smoke: `find env-var --format=jsonld | explain --stdin` works.
//!
//! Each test stages a synthetic repo with a `.env` file declaring the env vars
//! we want to explain, runs the binary with stdin piped in, and asserts on
//! stdout/stderr/exit-code.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{Value, json};
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

/// Stage a synthetic repo with a `.env` declaring FOO/BAR/BAZ and a Rust
/// source file that references them. This gives `explain env-var <NAME>`
/// something to chew on for each name.
fn stage_repo() -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    // Single-threaded test harness: this is safe enough to keep the index
    // small and predictable.
    unsafe { std::env::set_var("LEIO_DISABLE_SEARCH_SIDECAR", "1") };

    write(
        root,
        ".env",
        "FOO=foo_value\nBAR=bar_value\nBAZ=baz_value\n",
    );
    write(
        root,
        "src/main.rs",
        r#"fn main() {
    let _ = std::env::var("FOO");
    let _ = std::env::var("BAR");
    let _ = std::env::var("BAZ");
}
"#,
    );
    write(
        root,
        "Cargo.toml",
        "[package]\nname=\"stdin-fixture\"\nversion=\"0.1.0\"\n",
    );
    tmp
}

/// Run leio-code with `args`, feeding `stdin_input` on stdin. Returns
/// (stdout, stderr, exit-code).
fn run_with_stdin(repo: &Path, args: &[&str], stdin_input: &str) -> (String, String, i32) {
    let mut cmd = Command::new(bin());
    cmd.arg("--repo").arg(repo);
    for a in args {
        cmd.arg(a);
    }
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().expect("spawn leio-code");
    {
        let mut stdin = child.stdin.take().expect("take stdin");
        stdin
            .write_all(stdin_input.as_bytes())
            .expect("write stdin");
    }
    let out = child.wait_with_output().expect("wait leio-code");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

// ---------------------------------------------------------------------------
// 1. Input shapes
// ---------------------------------------------------------------------------

#[test]
fn whole_envelope_expands_entities() {
    let tmp = stage_repo();
    let envelope = json!({
        "@context": "https://ontology.getjai.com/leio-code/v1#",
        "@type": "FindResult",
        "query_id": "find_env-1",
        "entities": [
            {"@type": "EnvVar", "name": "FOO"},
            {"@type": "EnvVar", "name": "BAR"},
            {"@type": "EnvVar", "name": "BAZ"},
        ]
    });
    let (stdout, stderr, code) = run_with_stdin(
        tmp.path(),
        &["explain", "--stdin", "--format=json"],
        &envelope.to_string(),
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    let arr = parsed.as_array().expect("JSON array");
    assert_eq!(arr.len(), 3, "expected 3 envelopes, got {}", arr.len());
    // Each envelope should be an explain envelope.
    for env in arr {
        assert_eq!(env["kind"], "explain");
    }
}

#[test]
fn json_array_is_iterated() {
    let tmp = stage_repo();
    let input = json!([
        {"@type": "EnvVar", "name": "FOO"},
        {"@type": "EnvVar", "name": "BAR"},
        {"@type": "EnvVar", "name": "BAZ"},
    ]);
    let (stdout, stderr, code) = run_with_stdin(
        tmp.path(),
        &["explain", "--stdin", "--format=json"],
        &input.to_string(),
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(parsed.as_array().expect("array").len(), 3);
}

#[test]
fn jsonl_stream_is_iterated() {
    let tmp = stage_repo();
    let input = r#"{"@type":"EnvVar","name":"FOO"}
{"@type":"EnvVar","name":"BAR"}
{"@type":"EnvVar","name":"BAZ"}
"#;
    let (stdout, stderr, code) =
        run_with_stdin(tmp.path(), &["explain", "--stdin", "--format=json"], input);
    assert_eq!(code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(parsed.as_array().expect("array").len(), 3);
}

// ---------------------------------------------------------------------------
// 2. Skip behavior
// ---------------------------------------------------------------------------

#[test]
fn unknown_type_is_skipped_with_warning() {
    let tmp = stage_repo();
    let input = json!([
        {"@type": "EnvVar", "name": "FOO"},
        {"@type": "HttpCallSite", "caller_path": "x.py"},
        {"@type": "EnvVar", "name": "BAR"},
    ]);
    let (stdout, stderr, code) = run_with_stdin(
        tmp.path(),
        &["explain", "--stdin", "--format=json"],
        &input.to_string(),
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    // Only the two EnvVar entities yield envelopes.
    assert_eq!(parsed.as_array().expect("array").len(), 2);
    // Stderr must surface the unknown type both per-entity and in summary.
    assert!(
        stderr.contains("HttpCallSite"),
        "stderr should warn about HttpCallSite: {stderr}"
    );
    assert!(
        stderr.contains("skipped 1"),
        "stderr should summarize skips: {stderr}"
    );
}

#[test]
fn missing_required_field_is_skipped_with_warning() {
    let tmp = stage_repo();
    let input = json!([
        {"@type": "EnvVar", "name": "FOO"},
        {"@type": "EnvVar"},                        // missing name
        {"@type": "EnvVar", "name": "BAR"},
    ]);
    let (stdout, stderr, code) = run_with_stdin(
        tmp.path(),
        &["explain", "--stdin", "--format=json"],
        &input.to_string(),
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(parsed.as_array().expect("array").len(), 2);
    assert!(
        stderr.contains("missing required `name`"),
        "stderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// 3. Output formats
// ---------------------------------------------------------------------------

#[test]
fn jsonld_format_produces_array_of_jsonld_envelopes() {
    let tmp = stage_repo();
    let input = json!([
        {"@type": "EnvVar", "name": "FOO"},
        {"@type": "EnvVar", "name": "BAR"},
    ]);
    let (stdout, stderr, code) = run_with_stdin(
        tmp.path(),
        &["explain", "--stdin", "--format=jsonld"],
        &input.to_string(),
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    let arr = parsed.as_array().expect("array");
    assert_eq!(arr.len(), 2);
    // Each rendered envelope should carry JSON-LD framing.
    for env in arr {
        assert_eq!(env["@context"], "https://ontology.getjai.com/leio-code/v1#");
        assert_eq!(env["@type"], "ExplainResult");
        assert!(
            env["@id"]
                .as_str()
                .unwrap()
                .starts_with("urn:leio-code:query:")
        );
    }
}

#[test]
fn text_format_uses_dividers_between_envelopes() {
    let tmp = stage_repo();
    let input = json!([
        {"@type": "EnvVar", "name": "FOO"},
        {"@type": "EnvVar", "name": "BAR"},
        {"@type": "EnvVar", "name": "BAZ"},
    ]);
    let (stdout, stderr, code) = run_with_stdin(
        tmp.path(),
        &["explain", "--stdin", "--format=text"],
        &input.to_string(),
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    // Two dividers for three envelopes.
    let divider_count = stdout.matches("\n---\n").count();
    assert_eq!(
        divider_count, 2,
        "expected 2 dividers between 3 envelopes, got {divider_count}. stdout:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// 4. Flag interaction
// ---------------------------------------------------------------------------

#[test]
fn where_with_stdin_is_rejected() {
    let tmp = stage_repo();
    let input = json!([{"@type": "EnvVar", "name": "FOO"}]);
    let (_stdout, stderr, code) = run_with_stdin(
        tmp.path(),
        &["explain", "--stdin", "--where", ".entities[]"],
        &input.to_string(),
    );
    assert_ne!(code, 0, "expected non-zero exit");
    assert!(
        stderr.contains("--where is not supported with --stdin"),
        "stderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// 5. End-to-end pipeline smoke test
// ---------------------------------------------------------------------------

/// `find env-var --format=jsonld | explain --stdin --format=text`.
///
/// We don't shell out to a real pipe — we invoke `find` once, capture stdout,
/// then feed it back into `explain --stdin`. The point is the second invocation
/// can consume the first invocation's output without any hand-massaging.
#[test]
fn full_pipeline_find_then_explain_stdin() {
    let tmp = stage_repo();

    // Step 1: find env-var --format=jsonld
    // (find env-var takes a needle; FOO/BAR/BAZ share no common substring,
    // so we narrow to just FOO/BAR via a regex-ish 'A' substring that hits both,
    // then assert the pipe works for whatever entity count we got. The point
    // of the pipeline smoke is the wiring, not the find query coverage.)
    let find_out = Command::new(bin())
        .arg("--repo")
        .arg(tmp.path())
        .args(["find", "env-var", "A", "--format=jsonld"])
        .output()
        .expect("run find");
    assert_eq!(
        find_out.status.code().unwrap_or(-1),
        0,
        "find failed: stderr={}",
        String::from_utf8_lossy(&find_out.stderr)
    );
    let find_stdout = String::from_utf8_lossy(&find_out.stdout).to_string();
    // Sanity-check the find output is a JSON-LD envelope.
    let find_doc: Value = serde_json::from_str(&find_stdout).expect("find stdout is JSON");
    assert_eq!(
        find_doc["@context"],
        "https://ontology.getjai.com/leio-code/v1#"
    );
    let entity_count = find_doc["entities"].as_array().expect("entities").len();
    assert!(
        entity_count >= 2,
        "expected at least 2 env vars matching 'A' (BAR/BAZ), got {entity_count}"
    );

    // Step 2: feed back into explain --stdin
    let (stdout, stderr, code) = run_with_stdin(
        tmp.path(),
        &["explain", "--stdin", "--format=json"],
        &find_stdout,
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    let envelopes = parsed.as_array().expect("array");
    // One explain envelope per env var found.
    assert_eq!(envelopes.len(), entity_count);
    for env in envelopes {
        assert_eq!(env["kind"], "explain");
    }
}
