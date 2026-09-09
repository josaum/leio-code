//! Integration coverage for the Rust `Command::new` cross-language edge.
//!
//! Mirror of `tests/python_subprocess_edges.rs` — stage a tempdir, build the
//! index, assert what `find_subprocess_callers` (or the raw iterator)
//! surfaces.

use std::fs;
use std::path::Path;

use leio_code::indexer::{build_or_update_index, default_index_path};
use leio_code::query::find_subprocess_callers;
use tempfile::TempDir;

fn stage_rs(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(&path, body).expect("write rs file");
}

fn build(root: &Path) -> leio_code::model::RepoIndex {
    // Disable the DuckDB sidecar so the test runs in constrained envs —
    // find_subprocess_callers reads straight off the in-memory index.
    // SAFETY: tests are single-threaded by default for env mutation.
    unsafe { std::env::set_var("LEIO_DISABLE_SEARCH_SIDECAR", "1") };
    let index_path = default_index_path(root);
    build_or_update_index(root, &index_path, true).expect("build index")
}

#[test]
fn detects_use_std_process_command() {
    let tmp = TempDir::new().unwrap();
    stage_rs(
        tmp.path(),
        "src/main.rs",
        "use std::process::Command;\n\
         fn main() {\n\
             Command::new(\"leio-code\").arg(\"--help\").status().unwrap();\n\
         }\n",
    );
    let index = build(tmp.path());
    let env = find_subprocess_callers(&index, "leio-code");
    assert_eq!(env.evidence.len(), 1, "envelope: {env:?}");
    assert_eq!(env.evidence[0].path, "src/main.rs");
    assert_eq!(env.evidence[0].line, Some(3));
}

#[test]
fn detects_use_tokio_process_command() {
    let tmp = TempDir::new().unwrap();
    stage_rs(
        tmp.path(),
        "src/main.rs",
        "use tokio::process::Command;\n\
         async fn run() {\n\
             Command::new(\"leio-code\").spawn().unwrap();\n\
         }\n",
    );
    let index = build(tmp.path());
    let env = find_subprocess_callers(&index, "leio-code");
    assert_eq!(env.evidence.len(), 1);
    assert_eq!(env.evidence[0].line, Some(3));
}

#[test]
fn detects_fully_qualified_std() {
    let tmp = TempDir::new().unwrap();
    stage_rs(
        tmp.path(),
        "src/main.rs",
        "fn main() {\n\
             std::process::Command::new(\"leio-code\").status().unwrap();\n\
         }\n",
    );
    let index = build(tmp.path());
    let env = find_subprocess_callers(&index, "leio-code");
    assert_eq!(env.evidence.len(), 1);
}

#[test]
fn detects_fully_qualified_tokio() {
    let tmp = TempDir::new().unwrap();
    stage_rs(
        tmp.path(),
        "src/main.rs",
        "async fn run() {\n\
             tokio::process::Command::new(\"leio-code\").spawn().unwrap();\n\
         }\n",
    );
    let index = build(tmp.path());
    let env = find_subprocess_callers(&index, "leio-code");
    assert_eq!(env.evidence.len(), 1);
}

#[test]
fn detects_with_absolute_path() {
    let tmp = TempDir::new().unwrap();
    stage_rs(
        tmp.path(),
        "src/main.rs",
        "use std::process::Command;\n\
         fn main() {\n\
             Command::new(\"/usr/local/bin/leio-code\").status().unwrap();\n\
         }\n",
    );
    let index = build(tmp.path());
    // We index by literal as written — basename normalization happens later
    // in Phase 2 resolution. Keep the assertion honest.
    let env = find_subprocess_callers(&index, "/usr/local/bin/leio-code");
    assert_eq!(env.evidence.len(), 1, "literal path stored verbatim");
}

#[test]
fn skips_command_without_import() {
    let tmp = TempDir::new().unwrap();
    stage_rs(
        tmp.path(),
        "src/main.rs",
        // No `use std::process::Command;`, no fully-qualified form — bare
        // `Command::new` could be a user-defined type. Must not emit.
        "struct Command;\n\
         impl Command { fn new(_: &str) -> Self { Command } }\n\
         fn main() { Command::new(\"foo\"); }\n",
    );
    let index = build(tmp.path());
    let any = leio_code::model::RepoIndex::all_subprocess_calls(&index).count();
    assert_eq!(any, 0, "bare Command::new without import must not emit");
}

// NOTE: `skips_variable_first_arg` (asserting that `let bin = "leio-code";
// Command::new(bin)` produces no resolved spawn) was removed when Phase 8
// landed (commit e2e602107, "P0 #2 Phase 8 — one-hop dataflow resolution").
// Phase 8 intentionally resolves bare-name first args through one-hop
// in-function dataflow at confidence band 80. The positive coverage now
// lives in `tests/cross_language_dataflow.rs::
// rust_command_new_bare_name_resolves_to_binary_at_80`.

#[test]
fn skips_format_macro() {
    let tmp = TempDir::new().unwrap();
    stage_rs(
        tmp.path(),
        "src/main.rs",
        "use std::process::Command;\n\
         fn main() {\n\
             let x = 1;\n\
             Command::new(format!(\"bin-{}\", x)).status().unwrap();\n\
         }\n",
    );
    let index = build(tmp.path());
    let any = leio_code::model::RepoIndex::all_subprocess_calls(&index).count();
    assert_eq!(any, 0, "format!() must not emit");
}

#[test]
fn skips_in_raw_string() {
    // A `Command::new(\"foo\")` appearing inside a Rust raw string literal
    // (e.g. a documentation fixture or code-gen template) must not match,
    // because the surrounding `\"` characters in the source text are
    // escaped — they're not real quote delimiters that our regex would
    // anchor on. We verify with an `r#"..."#` raw string whose body
    // contains literal `\"foo\"` characters.
    //
    // Pure-textual regexes can't distinguish "code" from "string contents",
    // so we also include a `use std::process::Command;` import in the file
    // (representing a realistic snippet that *also* uses Command for real
    // elsewhere). The raw string itself must not produce an edge.
    let tmp = TempDir::new().unwrap();
    let body = "use std::process::Command;\n\
                fn main() {\n\
                    // Real call to keep the import live:\n\
                    Command::new(\"real-bin\").status().unwrap();\n\
                    // The string below contains *text* that looks like a\n\
                    // call but isn't one. The `\\\"` escapes here render\n\
                    // as literal backslash-quote inside the raw string,\n\
                    // which our regex (requiring bare `\"`) won't anchor.\n\
                    let _doc = r#\"Command::new(\\\"in-raw-string\\\")\"#;\n\
                }\n";
    stage_rs(tmp.path(), "src/main.rs", body);
    let index = build(tmp.path());
    let env = find_subprocess_callers(&index, "in-raw-string");
    assert_eq!(env.evidence.len(), 0, "raw-string fixture must not emit");
    let env_real = find_subprocess_callers(&index, "real-bin");
    assert_eq!(env_real.evidence.len(), 1, "real call still detected");
}

#[test]
fn multiple_calls_one_file() {
    let tmp = TempDir::new().unwrap();
    stage_rs(
        tmp.path(),
        "src/main.rs",
        "use std::process::Command;\n\
         fn main() {\n\
             Command::new(\"alpha\").status().unwrap();\n\
             Command::new(\"beta\").status().unwrap();\n\
         }\n",
    );
    let index = build(tmp.path());

    let a = find_subprocess_callers(&index, "alpha");
    assert_eq!(a.evidence.len(), 1);
    assert_eq!(a.evidence[0].line, Some(3));

    let b = find_subprocess_callers(&index, "beta");
    assert_eq!(b.evidence.len(), 1);
    assert_eq!(b.evidence[0].line, Some(4));
}
