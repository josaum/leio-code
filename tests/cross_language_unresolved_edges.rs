//! Integration tests for `UnresolvedEdge` (P0 #2 Phase 3).
//!
//! Each test stages a synthetic tempdir with one offending file, runs the
//! indexer, and asserts that the expected `UnresolvedReason` shows up on
//! `index.cross_language.unresolved_edges`. Pattern mirrors
//! `tests/cross_language_binary_nodes.rs`.

use std::fs;
use std::path::Path;

use leio_code::indexer::{build_or_update_index, default_index_path};
use leio_code::model::UnresolvedReason;
use tempfile::TempDir;

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(&path, body).expect("write file");
}

fn build(root: &Path) -> leio_code::model::RepoIndex {
    // Disable DuckDB sidecar — we only need the JSON index.
    // SAFETY: test suite is single-threaded by default.
    unsafe { std::env::set_var("LEIO_DISABLE_SEARCH_SIDECAR", "1") };
    let index_path = default_index_path(root);
    build_or_update_index(root, &index_path, true).expect("build index")
}

/// Helper: count unresolved edges in the given path whose reason matches.
fn unresolved_for(
    index: &leio_code::model::RepoIndex,
    rel_path: &str,
    reason: &UnresolvedReason,
) -> usize {
    index
        .cross_language
        .unresolved_edges
        .iter()
        .filter(|e| e.source_path == rel_path && &e.reason == reason)
        .count()
}

// ---------------------------------------------------------------------------
// Python
// ---------------------------------------------------------------------------

#[test]
fn python_variable_first_arg_is_non_literal() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "a.py",
        "import subprocess\nsubprocess.run([variable_name, \"--help\"])\n",
    );
    let index = build(tmp.path());
    assert_eq!(
        unresolved_for(&index, "a.py", &UnresolvedReason::NonLiteralFirstArg),
        1,
        "expected one NonLiteralFirstArg unresolved edge: {:?}",
        index.cross_language.unresolved_edges
    );
}

#[test]
fn python_shell_true_is_shell_invocation() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "a.py",
        "import subprocess\nsubprocess.run(\"ls -la\", shell=True)\n",
    );
    let index = build(tmp.path());
    assert_eq!(
        unresolved_for(&index, "a.py", &UnresolvedReason::ShellInvocation),
        1,
        "expected one ShellInvocation unresolved edge: {:?}",
        index.cross_language.unresolved_edges
    );
}

#[test]
fn python_fstring_first_arg_is_template() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "a.py",
        "import subprocess\nsubprocess.run([f\"bin-{x}\", \"--help\"])\n",
    );
    let index = build(tmp.path());
    assert_eq!(
        unresolved_for(&index, "a.py", &UnresolvedReason::TemplateOrConcat),
        1,
        "expected one TemplateOrConcat unresolved edge: {:?}",
        index.cross_language.unresolved_edges
    );
}

// ---------------------------------------------------------------------------
// JavaScript
// ---------------------------------------------------------------------------

#[test]
fn js_variable_first_arg_with_import_is_non_literal() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "a.js",
        "const { spawn } = require(\"child_process\");\nspawn(varName, []);\n",
    );
    let index = build(tmp.path());
    assert_eq!(
        unresolved_for(&index, "a.js", &UnresolvedReason::NonLiteralFirstArg),
        1,
        "expected one NonLiteralFirstArg unresolved edge: {:?}",
        index.cross_language.unresolved_edges
    );
}

#[test]
fn js_template_literal_is_template_or_concat() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "a.js", "child_process.spawn(`bin-${x}`, []);\n");
    let index = build(tmp.path());
    assert_eq!(
        unresolved_for(&index, "a.js", &UnresolvedReason::TemplateOrConcat),
        1,
        "expected one TemplateOrConcat unresolved edge: {:?}",
        index.cross_language.unresolved_edges
    );
}

// ---------------------------------------------------------------------------
// Rust
// ---------------------------------------------------------------------------

#[test]
fn rust_variable_first_arg_with_use_is_non_literal() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "a.rs",
        "use std::process::Command;\nfn run(p: &str) { Command::new(p); }\n",
    );
    let index = build(tmp.path());
    assert_eq!(
        unresolved_for(&index, "a.rs", &UnresolvedReason::NonLiteralFirstArg),
        1,
        "expected one NonLiteralFirstArg unresolved edge: {:?}",
        index.cross_language.unresolved_edges
    );
}

#[test]
fn rust_bare_command_new_is_ambiguous_import() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "a.rs", "fn main() { Command::new(\"foo\"); }\n");
    let index = build(tmp.path());
    // No resolved spawn (the ambiguous case never produced one), AND now we
    // emit an unresolved edge instead of dropping silently.
    let resolved_for_file: Vec<_> = index
        .files
        .iter()
        .find(|f| f.path == "a.rs")
        .map(|f| f.subprocess_calls.clone())
        .unwrap_or_default();
    assert!(
        resolved_for_file.is_empty(),
        "ambiguous Command::new must not emit a resolved spawn: {resolved_for_file:?}"
    );
    assert_eq!(
        unresolved_for(&index, "a.rs", &UnresolvedReason::AmbiguousCommandImport),
        1,
        "expected one AmbiguousCommandImport unresolved edge: {:?}",
        index.cross_language.unresolved_edges
    );
}

// ---------------------------------------------------------------------------
// Makefile
// ---------------------------------------------------------------------------

#[test]
fn makefile_variable_expansion_is_unresolved() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "Makefile", "x:\n\t$(CARGO) build\n");
    let index = build(tmp.path());
    assert_eq!(
        unresolved_for(&index, "Makefile", &UnresolvedReason::MakefileVariable),
        1,
        "expected one MakefileVariable unresolved edge: {:?}",
        index.cross_language.unresolved_edges
    );
}

// ---------------------------------------------------------------------------
// npm scripts
// ---------------------------------------------------------------------------

#[test]
fn npm_script_variable_is_unresolved() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "package.json",
        r#"{"name":"x","version":"1.0.0","scripts":{"run":"${RUNNER} foo"}}"#,
    );
    let index = build(tmp.path());
    assert_eq!(
        unresolved_for(&index, "package.json", &UnresolvedReason::NpmVariable),
        1,
        "expected one NpmVariable unresolved edge: {:?}",
        index.cross_language.unresolved_edges
    );
}
