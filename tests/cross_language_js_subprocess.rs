//! Integration coverage for the JavaScript / TypeScript `child_process`
//! subprocess cross-language edge — Phase 1 of P0 #2.
//!
//! Mirrors `tests/python_subprocess_edges.rs`: each test stages a tempdir,
//! runs the indexer, and asserts what `find_subprocess_callers` surfaces.

use std::fs;
use std::path::Path;

use leio_code::indexer::{build_or_update_index, default_index_path};
use leio_code::query::find_subprocess_callers;
use tempfile::TempDir;

fn stage(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(&path, body).expect("write file");
}

fn build(root: &Path) -> leio_code::model::RepoIndex {
    // Disable the DuckDB sidecar so the test runs even when the
    // bundled sqlite/duckdb fail to link in constrained environments.
    // SAFETY: tests are single-threaded by default for env mutation.
    unsafe { std::env::set_var("LEIO_DISABLE_SEARCH_SIDECAR", "1") };
    let index_path = default_index_path(root);
    build_or_update_index(root, &index_path, true).expect("build index")
}

#[test]
fn detects_child_process_spawn() {
    let tmp = TempDir::new().unwrap();
    stage(
        tmp.path(),
        "src/foo.js",
        "child_process.spawn(\"leio-code\", [\"--help\"]);\n",
    );
    let index = build(tmp.path());
    let env = find_subprocess_callers(&index, "leio-code");
    assert_eq!(env.evidence.len(), 1, "envelope: {env:?}");
    assert_eq!(env.evidence[0].path, "src/foo.js");
    assert_eq!(env.evidence[0].line, Some(1));
}

#[test]
fn detects_child_process_exec_string_form() {
    let tmp = TempDir::new().unwrap();
    stage(
        tmp.path(),
        "a.ts",
        "child_process.exec(\"leio-code --help\");\n",
    );
    let index = build(tmp.path());
    let env = find_subprocess_callers(&index, "leio-code");
    assert_eq!(env.evidence.len(), 1, "envelope: {env:?}");
    assert_eq!(env.evidence[0].path, "a.ts");
}

#[test]
#[allow(non_snake_case)]
fn detects_child_process_execFile() {
    let tmp = TempDir::new().unwrap();
    stage(
        tmp.path(),
        "a.tsx",
        "child_process.execFile(\"leio-code\", [\"--help\"]);\n",
    );
    let index = build(tmp.path());
    let env = find_subprocess_callers(&index, "leio-code");
    assert_eq!(env.evidence.len(), 1, "envelope: {env:?}");
    assert_eq!(env.evidence[0].path, "a.tsx");
}

#[test]
fn detects_child_process_fork() {
    let tmp = TempDir::new().unwrap();
    stage(
        tmp.path(),
        "a.js",
        "child_process.fork(\"script.js\", [\"--quiet\"]);\n",
    );
    let index = build(tmp.path());
    let env = find_subprocess_callers(&index, "script.js");
    assert_eq!(env.evidence.len(), 1, "envelope: {env:?}");
}

#[test]
fn detects_bare_spawn_with_require_import() {
    let tmp = TempDir::new().unwrap();
    stage(
        tmp.path(),
        "a.js",
        "const { spawn } = require(\"child_process\");\n\
         spawn(\"leio-code\", [\"--help\"]);\n",
    );
    let index = build(tmp.path());
    let env = find_subprocess_callers(&index, "leio-code");
    assert_eq!(env.evidence.len(), 1, "envelope: {env:?}");
    assert_eq!(env.evidence[0].line, Some(2));
}

#[test]
fn detects_bare_spawn_with_es_import() {
    let tmp = TempDir::new().unwrap();
    stage(
        tmp.path(),
        "a.ts",
        "import { spawn } from \"child_process\";\n\
         spawn(\"leio-code\", [\"--help\"]);\n",
    );
    let index = build(tmp.path());
    let env = find_subprocess_callers(&index, "leio-code");
    assert_eq!(env.evidence.len(), 1, "envelope: {env:?}");
    assert_eq!(env.evidence[0].line, Some(2));
}

#[test]
fn skips_template_string_first_arg() {
    let tmp = TempDir::new().unwrap();
    stage(
        tmp.path(),
        "a.js",
        "const { spawn } = require(\"child_process\");\n\
         spawn(`leio-${x}`, [\"--help\"]);\n",
    );
    let index = build(tmp.path());
    let any = leio_code::model::RepoIndex::all_subprocess_calls(&index).count();
    assert_eq!(any, 0, "template literal first arg must not emit edges");
}

#[test]
fn skips_variable_first_arg() {
    let tmp = TempDir::new().unwrap();
    stage(
        tmp.path(),
        "a.js",
        "const { spawn } = require(\"child_process\");\n\
         const cmd = \"leio-code\";\n\
         spawn(cmd, [\"--help\"]);\n",
    );
    let index = build(tmp.path());
    let env = find_subprocess_callers(&index, "leio-code");
    assert_eq!(env.evidence.len(), 0, "variable first arg: {env:?}");
}

#[test]
fn skips_bare_spawn_without_child_process_import() {
    let tmp = TempDir::new().unwrap();
    // No child_process import — `spawn` here is some other function (e.g.
    // tape's spawn, threads.js, ...). Must not emit.
    stage(tmp.path(), "a.js", "spawn(\"leio-code\", [\"--help\"]);\n");
    let index = build(tmp.path());
    let any = leio_code::model::RepoIndex::all_subprocess_calls(&index).count();
    assert_eq!(any, 0, "bare spawn without import must not emit edges");
}
