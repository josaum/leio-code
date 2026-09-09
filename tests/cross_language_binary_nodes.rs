//! Integration tests for the binary node registry and cross-language spawn
//! edge resolution (P0 #2 Phase 2).
//!
//! Each test stages a synthetic tempdir with the relevant manifests / source
//! files, runs the indexer, and asserts what `index.cross_language` contains.
//! The pattern mirrors `tests/cross_language_rust_process.rs`.

use std::fs;
use std::path::Path;

use leio_code::indexer::{build_or_update_index, default_index_path};
use leio_code::model::BinaryNodeSource;
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

// ---------------------------------------------------------------------------
// Binary node discovery tests
// ---------------------------------------------------------------------------

#[test]
fn cargo_explicit_bin_detected() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "Cargo.toml",
        "[package]\nname = \"myapp\"\nversion = \"0.1.0\"\n\n[[bin]]\nname = \"mytool\"\npath = \"src/main.rs\"\n",
    );
    let index = build(tmp.path());
    let found: Vec<_> = index
        .cross_language
        .binaries
        .iter()
        .filter(|b| b.source == BinaryNodeSource::CargoExplicit && b.name == "mytool")
        .collect();
    assert_eq!(found.len(), 1, "should detect explicit [[bin]] entry");
    assert_eq!(found[0].path, "Cargo.toml");
}

#[test]
fn cargo_implicit_from_main_rs() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "Cargo.toml",
        "[package]\nname = \"mypkg\"\nversion = \"0.1.0\"\n",
    );
    write(tmp.path(), "src/main.rs", "fn main() {}\n");
    let index = build(tmp.path());
    let found: Vec<_> = index
        .cross_language
        .binaries
        .iter()
        .filter(|b| b.source == BinaryNodeSource::CargoImplicit && b.name == "mypkg")
        .collect();
    assert_eq!(
        found.len(),
        1,
        "should detect CargoImplicit from src/main.rs"
    );
    assert_eq!(found[0].path, "src/main.rs");
}

#[test]
fn cargo_bin_dir_file_detected() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "src/bin/tool.rs", "fn main() {}\n");
    let index = build(tmp.path());
    let found: Vec<_> = index
        .cross_language
        .binaries
        .iter()
        .filter(|b| b.source == BinaryNodeSource::CargoBin && b.name == "tool")
        .collect();
    assert_eq!(found.len(), 1, "should detect src/bin/tool.rs");
    assert_eq!(found[0].path, "src/bin/tool.rs");
}

#[test]
fn npm_bin_object_detected() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "package.json",
        r#"{"name":"my-app","version":"1.0.0","bin":{"my-cli":"./index.js"}}"#,
    );
    let index = build(tmp.path());
    let found: Vec<_> = index
        .cross_language
        .binaries
        .iter()
        .filter(|b| b.source == BinaryNodeSource::NpmBin && b.name == "my-cli")
        .collect();
    assert_eq!(found.len(), 1, "should detect npm bin object entry");
    assert_eq!(found[0].path, "package.json");
}

// ---------------------------------------------------------------------------
// Resolution tests
// ---------------------------------------------------------------------------

#[test]
fn python_spawn_resolves_to_cargo_binary() {
    let tmp = TempDir::new().unwrap();
    // A Cargo binary named "leio-code" (implicit from src/main.rs)
    write(
        tmp.path(),
        "Cargo.toml",
        "[package]\nname = \"leio-code\"\nversion = \"0.1.0\"\n",
    );
    write(tmp.path(), "src/main.rs", "fn main() {}\n");
    // A Python file that spawns it
    write(
        tmp.path(),
        "scripts/run.py",
        "import subprocess\nsubprocess.run([\"leio-code\", \"--help\"])\n",
    );
    let index = build(tmp.path());
    assert!(
        !index.cross_language.resolved_spawns.is_empty(),
        "should have at least one resolved spawn edge"
    );
    let edge = index
        .cross_language
        .resolved_spawns
        .iter()
        .find(|e| e.callee_name == "leio-code")
        .expect("spawn edge for leio-code");
    assert_eq!(edge.caller_path, "scripts/run.py");
    assert_eq!(edge.caller_line, 2);
    assert_eq!(edge.confidence, 95);
    assert_eq!(edge.callee_path, "src/main.rs");
}

#[test]
fn unknown_binary_name_produces_no_resolved_edge() {
    let tmp = TempDir::new().unwrap();
    // Cargo binary "leio-code"
    write(
        tmp.path(),
        "Cargo.toml",
        "[package]\nname = \"leio-code\"\nversion = \"0.1.0\"\n",
    );
    write(tmp.path(), "src/main.rs", "fn main() {}\n");
    // Python spawns a name that does not match any binary
    write(
        tmp.path(),
        "scripts/run.py",
        "import subprocess\nsubprocess.run([\"unknown-bin\", \"--help\"])\n",
    );
    let index = build(tmp.path());
    let matching: Vec<_> = index
        .cross_language
        .resolved_spawns
        .iter()
        .filter(|e| e.callee_name == "unknown-bin")
        .collect();
    assert!(
        matching.is_empty(),
        "no resolved edge for unrecognized binary name"
    );
}
