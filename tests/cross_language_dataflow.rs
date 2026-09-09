//! Integration tests for the Phase 8 light-dataflow pass.
//!
//! Each test stages a tempdir, runs `build_or_update_index`, and asserts on
//! `index.cross_language.resolved_http_edges` (`match_kind` + `confidence`),
//! `index.cross_language.resolved_spawns` (`confidence`), or
//! `index.cross_language.unresolved_edges` (`reason`). Mirrors the pattern
//! from `tests/cross_language_http_route_templates.rs`.
//!
//! See `docs/light-dataflow-design.md` for the contract these tests pin.

use std::fs;
use std::path::Path;

use leio_code::indexer::{build_or_update_index, default_index_path};
use leio_code::model::{MatchKind, UnresolvedReason};
use tempfile::TempDir;

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(&path, body).expect("write file");
}

fn build(root: &Path) -> leio_code::model::RepoIndex {
    // SAFETY: tests run single-threaded by default in cargo's `test` profile.
    unsafe { std::env::set_var("LEIO_DISABLE_SEARCH_SIDECAR", "1") };
    let index_path = default_index_path(root);
    build_or_update_index(root, &index_path, true).expect("build index")
}

// ===========================================================================
// PYTHON HTTP
// ===========================================================================

#[test]
fn python_bare_name_literal_resolves_to_dataflow_literal() {
    // Pattern 1: variable bound to literal, used as URL.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "from flask import Flask\napp = Flask(__name__)\n@app.route(\"/users\")\ndef u(): return 1\n",
    );
    write(
        tmp.path(),
        "client.py",
        "import requests\ndef fetch():\n    url = \"/users\"\n    return requests.get(url)\n",
    );
    let index = build(tmp.path());
    let edges = &index.cross_language.resolved_http_edges;
    let dataflow_edges: Vec<_> = edges
        .iter()
        .filter(|e| e.match_kind == MatchKind::DataflowLiteral)
        .collect();
    assert_eq!(
        dataflow_edges.len(),
        1,
        "expected one DataflowLiteral edge; got: {:#?}",
        edges
    );
    assert_eq!(dataflow_edges[0].confidence, 80);
    assert_eq!(dataflow_edges[0].route_path, "/users");
}

#[test]
fn python_fstring_with_bound_base_resolves_to_dataflow_template() {
    // Pattern 2: f-string with one literal-bound placeholder and one
    // function-param placeholder. Expect substitution to replace `base`
    // and leave `{uid}`, matching `/users/{id}` at confidence 75.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "from fastapi import FastAPI\napp = FastAPI()\n@app.get(\"/users/{id}\")\ndef u(): return 1\n",
    );
    write(
        tmp.path(),
        "client.py",
        "import requests\ndef fetch(uid):\n    base = \"/users\"\n    return requests.get(f\"{base}/{uid}\")\n",
    );
    let index = build(tmp.path());
    let edges = &index.cross_language.resolved_http_edges;
    let dataflow_edges: Vec<_> = edges
        .iter()
        .filter(|e| e.match_kind == MatchKind::DataflowTemplate)
        .collect();
    assert_eq!(
        dataflow_edges.len(),
        1,
        "expected one DataflowTemplate edge; got: {:#?}",
        edges
    );
    assert_eq!(dataflow_edges[0].confidence, 75);
}

#[test]
fn python_concat_resolves_to_dataflow_literal() {
    // Pattern 3: `base + "/users"` where `base` is literal-bound.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "from flask import Flask\napp = Flask(__name__)\n@app.route(\"/users\")\ndef u(): return 1\n",
    );
    write(
        tmp.path(),
        "client.py",
        "import requests\ndef fetch():\n    base = \"\"\n    return requests.get(base + \"/users\")\n",
    );
    let index = build(tmp.path());
    let edges = &index.cross_language.resolved_http_edges;
    let dataflow_edges: Vec<_> = edges
        .iter()
        .filter(|e| e.match_kind == MatchKind::DataflowLiteral)
        .collect();
    assert_eq!(
        dataflow_edges.len(),
        1,
        "expected one DataflowLiteral concat edge; got: {:#?}",
        edges
    );
}

#[test]
fn python_unresolved_bare_name_no_binding_drops() {
    // The variable is not bound to any literal in the function — emits
    // an UnresolvedEdge with NonLiteralFirstArg.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "client.py",
        "import requests\ndef fetch(url):\n    return requests.get(url)\n",
    );
    let index = build(tmp.path());
    let unresolved: Vec<_> = index
        .cross_language
        .unresolved_edges
        .iter()
        .filter(|u| u.edge_kind == "http_call")
        .collect();
    assert_eq!(
        unresolved.len(),
        1,
        "expected unresolved: {:#?}",
        unresolved
    );
    assert!(matches!(
        unresolved[0].reason,
        UnresolvedReason::NonLiteralFirstArg
    ));
}

#[test]
fn python_conditional_reassignment_emits_ambiguous() {
    // `url` is assigned to two distinct literals — the dataflow pass
    // refuses to guess and emits AmbiguousAssignment.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "client.py",
        "import requests\ndef fetch(flag):\n    url = \"/a\"\n    url = \"/b\"\n    return requests.get(url)\n",
    );
    let index = build(tmp.path());
    let ambig: Vec<_> = index
        .cross_language
        .unresolved_edges
        .iter()
        .filter(|u| matches!(u.reason, UnresolvedReason::AmbiguousAssignment))
        .collect();
    assert_eq!(
        ambig.len(),
        1,
        "expected AmbiguousAssignment: {:#?}",
        index.cross_language.unresolved_edges
    );
}

#[test]
fn python_cross_function_does_not_resolve() {
    // Out-of-scope: literal bound in one function, used in another.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "from flask import Flask\napp = Flask(__name__)\n@app.route(\"/users\")\ndef u(): return 1\n",
    );
    write(
        tmp.path(),
        "client.py",
        "import requests\nURL = \"/users\"\ndef fetch():\n    return requests.get(URL)\n",
    );
    let index = build(tmp.path());
    // Module-level constant → not detected by single-function dataflow.
    let dataflow_edges: Vec<_> = index
        .cross_language
        .resolved_http_edges
        .iter()
        .filter(|e| {
            matches!(
                e.match_kind,
                MatchKind::DataflowLiteral | MatchKind::DataflowTemplate
            )
        })
        .collect();
    assert!(
        dataflow_edges.is_empty(),
        "cross-function const should NOT resolve via dataflow"
    );
}

// ===========================================================================
// JS / TS HTTP
// ===========================================================================

#[test]
fn js_bare_name_literal_resolves_to_dataflow_literal() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "@app.route(\"/users\")\ndef u(): return 1\n",
    );
    write(
        tmp.path(),
        "client.ts",
        "function getUsers() {\n  const url = \"/users\";\n  return fetch(url);\n}\n",
    );
    let index = build(tmp.path());
    let dataflow_edges: Vec<_> = index
        .cross_language
        .resolved_http_edges
        .iter()
        .filter(|e| e.match_kind == MatchKind::DataflowLiteral)
        .collect();
    assert_eq!(
        dataflow_edges.len(),
        1,
        "expected one DataflowLiteral edge; got: {:#?}",
        index.cross_language.resolved_http_edges
    );
}

#[test]
fn js_template_literal_with_bound_base_resolves_to_dataflow_template() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.js",
        "const app = express();\napp.get(\"/users/:id\", h);\n",
    );
    write(
        tmp.path(),
        "client.ts",
        "function getUser(id) {\n  const base = \"/users\";\n  return axios.get(`${base}/${id}`);\n}\n",
    );
    let index = build(tmp.path());
    let dataflow_edges: Vec<_> = index
        .cross_language
        .resolved_http_edges
        .iter()
        .filter(|e| e.match_kind == MatchKind::DataflowTemplate)
        .collect();
    assert_eq!(
        dataflow_edges.len(),
        1,
        "expected one DataflowTemplate edge; got: {:#?}",
        index.cross_language.resolved_http_edges
    );
}

#[test]
fn js_concat_resolves_via_dataflow() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "@app.route(\"/users\")\ndef u(): return 1\n",
    );
    write(
        tmp.path(),
        "client.ts",
        "function f() {\n  const base = \"\";\n  return axios.get(base + \"/users\");\n}\n",
    );
    let index = build(tmp.path());
    let dataflow_edges: Vec<_> = index
        .cross_language
        .resolved_http_edges
        .iter()
        .filter(|e| matches!(e.match_kind, MatchKind::DataflowLiteral))
        .collect();
    assert_eq!(
        dataflow_edges.len(),
        1,
        "expected DataflowLiteral concat edge; got: {:#?}",
        index.cross_language.resolved_http_edges
    );
}

#[test]
fn js_conditional_reassignment_emits_ambiguous() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "client.ts",
        "function f() {\n  const url = \"/a\";\n  const url2 = \"/b\";\n  // Two distinct literals share a name only when re-`let`. Mimic via reuse.\n  return fetch(url);\n}\n",
    );
    // The above isn't actually a reassignment (different names). Use let + reassignment.
    write(
        tmp.path(),
        "client.ts",
        "function f() {\n  let url = \"/a\";\n  url = \"/b\";\n  return fetch(url);\n}\n",
    );
    let index = build(tmp.path());
    let ambig: Vec<_> = index
        .cross_language
        .unresolved_edges
        .iter()
        .filter(|u| matches!(u.reason, UnresolvedReason::AmbiguousAssignment))
        .collect();
    assert_eq!(
        ambig.len(),
        1,
        "expected AmbiguousAssignment: {:#?}",
        index.cross_language.unresolved_edges
    );
}

#[test]
fn js_unresolved_bare_name_emits_non_literal() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "client.ts",
        "function f(url) {\n  return fetch(url);\n}\n",
    );
    let index = build(tmp.path());
    let unresolved: Vec<_> = index
        .cross_language
        .unresolved_edges
        .iter()
        .filter(|u| u.edge_kind == "http_call")
        .collect();
    assert_eq!(
        unresolved.len(),
        1,
        "got: {:#?}",
        index.cross_language.unresolved_edges
    );
    assert!(matches!(
        unresolved[0].reason,
        UnresolvedReason::NonLiteralFirstArg
    ));
}

// ===========================================================================
// RUST HTTP
// ===========================================================================

#[test]
fn rust_bare_name_literal_resolves_to_dataflow_literal() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "@app.route(\"/users\")\ndef u(): return 1\n",
    );
    write(
        tmp.path(),
        "client.rs",
        "async fn fetch_users() {\n  let url = \"/users\";\n  let _ = reqwest::get(&url).await;\n}\n",
    );
    let index = build(tmp.path());
    let dataflow_edges: Vec<_> = index
        .cross_language
        .resolved_http_edges
        .iter()
        .filter(|e| e.match_kind == MatchKind::DataflowLiteral)
        .collect();
    assert_eq!(
        dataflow_edges.len(),
        1,
        "expected DataflowLiteral; got: {:#?}",
        index.cross_language.resolved_http_edges
    );
}

#[test]
fn rust_format_with_bound_base_resolves_to_dataflow_template() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "@app.get(\"/users/{id}\")\ndef u(): return 1\n",
    );
    write(
        tmp.path(),
        "client.rs",
        "async fn fetch_user(id: u64) {\n  let base = \"/users\";\n  let _ = reqwest::get(format!(\"{}/{}\", base, id)).await;\n}\n",
    );
    let index = build(tmp.path());
    let dataflow_edges: Vec<_> = index
        .cross_language
        .resolved_http_edges
        .iter()
        .filter(|e| e.match_kind == MatchKind::DataflowTemplate)
        .collect();
    assert_eq!(
        dataflow_edges.len(),
        1,
        "expected DataflowTemplate; got: {:#?}",
        index.cross_language.resolved_http_edges
    );
}

#[test]
fn rust_unresolved_format_with_only_params_keeps_template_band() {
    // No literal bound — format!() with only function-param interpolations
    // falls back to Phase 6 Template (70), not DataflowTemplate.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "@app.get(\"/users/{id}\")\ndef u(): return 1\n",
    );
    write(
        tmp.path(),
        "client.rs",
        "async fn fetch_user(uid: u64) {\n  let _ = reqwest::get(format!(\"/users/{}\", uid)).await;\n}\n",
    );
    let index = build(tmp.path());
    let edges = &index.cross_language.resolved_http_edges;
    let template_edges: Vec<_> = edges
        .iter()
        .filter(|e| matches!(e.match_kind, MatchKind::Template))
        .collect();
    assert_eq!(
        template_edges.len(),
        1,
        "expected fallback Template edge; got: {:#?}",
        edges
    );
    assert_eq!(template_edges[0].confidence, 70);
}

#[test]
fn rust_conditional_reassignment_emits_ambiguous() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "client.rs",
        "async fn f() {\n  let mut url = \"/a\";\n  url = \"/b\";\n  let _ = reqwest::get(&url).await;\n}\n",
    );
    let index = build(tmp.path());
    let ambig: Vec<_> = index
        .cross_language
        .unresolved_edges
        .iter()
        .filter(|u| matches!(u.reason, UnresolvedReason::AmbiguousAssignment))
        .collect();
    assert_eq!(
        ambig.len(),
        1,
        "expected AmbiguousAssignment: {:#?}",
        index.cross_language.unresolved_edges
    );
}

// ===========================================================================
// MULTI-HOP / OUT-OF-SCOPE
// ===========================================================================

#[test]
fn multi_hop_through_function_call_does_not_resolve() {
    // The URL passes through a helper before use — out of scope.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "client.py",
        "import requests\ndef build():\n    return \"/users\"\ndef fetch():\n    url = build()\n    return requests.get(url)\n",
    );
    let index = build(tmp.path());
    let dataflow_edges: Vec<_> = index
        .cross_language
        .resolved_http_edges
        .iter()
        .filter(|e| {
            matches!(
                e.match_kind,
                MatchKind::DataflowLiteral | MatchKind::DataflowTemplate
            )
        })
        .collect();
    assert!(
        dataflow_edges.is_empty(),
        "function-call assignment is out of scope; got: {:#?}",
        dataflow_edges
    );
}

// ===========================================================================
// SUBPROCESS BINARY NAME DATAFLOW
// ===========================================================================

#[test]
fn python_subprocess_bare_name_resolves_to_binary_at_80() {
    // Bin name held in a local variable, then passed to subprocess.run.
    // Confidence drops from 95 (literal) to 80 (dataflow).
    let tmp = TempDir::new().unwrap();
    // Declare a known binary via Cargo.
    write(
        tmp.path(),
        "Cargo.toml",
        "[package]\nname = \"leio-code\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(tmp.path(), "src/main.rs", "fn main() {}\n");
    write(
        tmp.path(),
        "spawner.py",
        "import subprocess\ndef run():\n    bin = \"leio-code\"\n    subprocess.run([bin, \"--help\"])\n",
    );
    let index = build(tmp.path());
    let spawn_edges: Vec<_> = index
        .cross_language
        .resolved_spawns
        .iter()
        .filter(|e| e.callee_name == "leio-code")
        .collect();
    assert_eq!(
        spawn_edges.len(),
        1,
        "got: {:#?}",
        index.cross_language.resolved_spawns
    );
    assert_eq!(spawn_edges[0].confidence, 80, "dataflow band");
}

#[test]
fn rust_command_new_bare_name_resolves_to_binary_at_80() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "Cargo.toml",
        "[package]\nname = \"leio-code\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(tmp.path(), "src/main.rs", "fn main() {}\n");
    write(
        tmp.path(),
        "spawner.rs",
        "use std::process::Command;\nfn run() {\n  let bin = \"leio-code\";\n  Command::new(&bin).arg(\"--help\");\n}\n",
    );
    let index = build(tmp.path());
    let spawn_edges: Vec<_> = index
        .cross_language
        .resolved_spawns
        .iter()
        .filter(|e| e.callee_name == "leio-code")
        .collect();
    assert_eq!(
        spawn_edges.len(),
        1,
        "got: {:#?}",
        index.cross_language.resolved_spawns
    );
    assert_eq!(spawn_edges[0].confidence, 80);
}

#[test]
fn js_subprocess_bare_name_resolves_to_binary_at_80() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "Cargo.toml",
        "[package]\nname = \"leio-code\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(tmp.path(), "src/main.rs", "fn main() {}\n");
    write(
        tmp.path(),
        "spawner.js",
        "function run() {\n  const bin = \"leio-code\";\n  child_process.spawn(bin, [\"--help\"]);\n}\n",
    );
    let index = build(tmp.path());
    let spawn_edges: Vec<_> = index
        .cross_language
        .resolved_spawns
        .iter()
        .filter(|e| e.callee_name == "leio-code")
        .collect();
    assert_eq!(
        spawn_edges.len(),
        1,
        "got: {:#?}",
        index.cross_language.resolved_spawns
    );
    assert_eq!(spawn_edges[0].confidence, 80);
}
