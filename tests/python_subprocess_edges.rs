//! Integration coverage for the Python subprocess cross-language edge.
//!
//! Each test stages a tempdir with one or more `.py` files, runs the indexer,
//! and asserts what `find_subprocess_callers` surfaces. The indexer is the
//! end-to-end seam we care about: detector → FileRecord → query envelope.

use std::fs;
use std::path::Path;

use leio_code::indexer::{build_or_update_index, default_index_path};
use leio_code::query::find_subprocess_callers;
use tempfile::TempDir;

fn stage_py(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(&path, body).expect("write py file");
}

fn build(root: &Path) -> leio_code::model::RepoIndex {
    // Disable the DuckDB sidecar so the test runs even when the
    // bundled sqlite/duckdb fail to link in constrained environments —
    // find_subprocess_callers reads straight off the in-memory index.
    // SAFETY: tests are single-threaded by default for env mutation.
    unsafe { std::env::set_var("LEIO_DISABLE_SEARCH_SIDECAR", "1") };
    let index_path = default_index_path(root);
    build_or_update_index(root, &index_path, true).expect("build index")
}

#[test]
fn detects_subprocess_run_with_literal_binary() {
    let tmp = TempDir::new().unwrap();
    stage_py(
        tmp.path(),
        "src/foo.py",
        "import subprocess\nsubprocess.run([\"leio-code\", \"--help\"])\n",
    );
    let index = build(tmp.path());
    let env = find_subprocess_callers(&index, "leio-code");
    assert_eq!(env.evidence.len(), 1, "envelope: {env:?}");
    assert_eq!(env.evidence[0].path, "src/foo.py");
    assert_eq!(env.evidence[0].line, Some(2));
}

#[test]
fn detects_subprocess_popen() {
    let tmp = TempDir::new().unwrap();
    stage_py(
        tmp.path(),
        "a.py",
        "import subprocess\nsubprocess.Popen([\"leio-code\", \"--json\", \"graph\"])\n",
    );
    let index = build(tmp.path());
    let env = find_subprocess_callers(&index, "leio-code");
    assert_eq!(env.evidence.len(), 1);
    assert_eq!(env.evidence[0].path, "a.py");
}

#[test]
fn detects_subprocess_check_output() {
    let tmp = TempDir::new().unwrap();
    stage_py(
        tmp.path(),
        "a.py",
        "import subprocess\nsubprocess.check_output([\"leio-code\"])\n",
    );
    let index = build(tmp.path());
    let env = find_subprocess_callers(&index, "leio-code");
    assert_eq!(env.evidence.len(), 1);
}

#[test]
fn detects_subprocess_check_call() {
    let tmp = TempDir::new().unwrap();
    stage_py(
        tmp.path(),
        "a.py",
        "import subprocess\nsubprocess.check_call([\"leio-code\"])\n",
    );
    let index = build(tmp.path());
    let env = find_subprocess_callers(&index, "leio-code");
    assert_eq!(env.evidence.len(), 1);
}

#[test]
fn detects_subprocess_call() {
    let tmp = TempDir::new().unwrap();
    stage_py(
        tmp.path(),
        "a.py",
        "import subprocess\nsubprocess.call([\"leio-code\"])\n",
    );
    let index = build(tmp.path());
    let env = find_subprocess_callers(&index, "leio-code");
    assert_eq!(env.evidence.len(), 1);
}

#[test]
fn skips_variable_first_arg() {
    let tmp = TempDir::new().unwrap();
    stage_py(
        tmp.path(),
        "a.py",
        "import subprocess\ncmd = [\"leio-code\", \"--help\"]\nsubprocess.run(cmd)\n",
    );
    let index = build(tmp.path());
    let env = find_subprocess_callers(&index, "leio-code");
    assert_eq!(env.evidence.len(), 0, "variable first arg: {env:?}");
}

#[test]
fn skips_format_string_first_arg() {
    let tmp = TempDir::new().unwrap();
    stage_py(
        tmp.path(),
        "a.py",
        "import subprocess\nsubprocess.run([f\"bin-{suffix}\", \"--help\"])\n",
    );
    let index = build(tmp.path());
    let env = find_subprocess_callers(&index, "bin-");
    assert_eq!(env.evidence.len(), 0);
    let any = leio_code::model::RepoIndex::all_subprocess_calls(&index).count();
    assert_eq!(any, 0, "f-string should leave the index empty");
}

#[test]
fn skips_shell_true() {
    let tmp = TempDir::new().unwrap();
    stage_py(
        tmp.path(),
        "a.py",
        "import subprocess\nsubprocess.run(\"leio-code --help\", shell=True)\n",
    );
    let index = build(tmp.path());
    let env = find_subprocess_callers(&index, "leio-code");
    assert_eq!(env.evidence.len(), 0);
}

#[test]
fn multiple_calls_one_file() {
    let tmp = TempDir::new().unwrap();
    stage_py(
        tmp.path(),
        "a.py",
        "import subprocess\n\
         subprocess.run([\"leio-code\", \"find\"])\n\
         subprocess.run([\"example-cli\", \"status\"])\n",
    );
    let index = build(tmp.path());

    let leio = find_subprocess_callers(&index, "leio-code");
    assert_eq!(leio.evidence.len(), 1);
    assert_eq!(leio.evidence[0].line, Some(2));

    let myc = find_subprocess_callers(&index, "example-cli");
    assert_eq!(myc.evidence.len(), 1);
    assert_eq!(myc.evidence[0].line, Some(3));
}

#[test]
fn multiple_callers_one_binary() {
    let tmp = TempDir::new().unwrap();
    stage_py(
        tmp.path(),
        "src/a.py",
        "import subprocess\nsubprocess.run([\"leio-code\", \"--help\"])\n",
    );
    stage_py(
        tmp.path(),
        "src/b.py",
        "import subprocess\nsubprocess.Popen([\"leio-code\", \"graph\"])\n",
    );
    let index = build(tmp.path());
    let env = find_subprocess_callers(&index, "leio-code");
    assert_eq!(env.evidence.len(), 2, "two callers: {env:?}");
    let mut paths: Vec<_> = env.evidence.iter().map(|e| e.path.as_str()).collect();
    paths.sort();
    assert_eq!(paths, vec!["src/a.py", "src/b.py"]);
}
