//! Multi-line dotenv parser + display tests.
//!
//! Follow-up to P1 #5: the dotenv parser must consume multi-line quoted
//! values verbatim, and `explain env-var` must display them as either
//! `<first line> […N more lines]` (non-secret) or
//! `[redacted, N chars, sha256:<hex>]` (secret, with N covering the full body).

use std::fs;
use std::path::Path;

use leio_code::indexer::build_or_update_index;
use leio_code::model::DeclaredVar;
use leio_code::query::explain_env_var;
use leio_code::value_resolution::{ValueResolutionOpts, resolve_value_bindings};
use tempfile::TempDir;

fn write_repo(files: &[(&str, &str)]) -> TempDir {
    let tmp = tempfile::tempdir().expect("create tempdir");
    for (rel, content) in files {
        let full = tmp.path().join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(&full, content).expect("write file");
    }
    tmp
}

fn build(tmp: &Path) -> leio_code::model::RepoIndex {
    let index_path = tmp.join(".leio-code").join("index.json");
    fs::create_dir_all(index_path.parent().unwrap()).unwrap();
    build_or_update_index(tmp, &index_path, true).expect("build index")
}

fn find_var<'a>(index: &'a leio_code::model::RepoIndex, name: &str) -> &'a DeclaredVar {
    index
        .env_files
        .iter()
        .flat_map(|f| f.vars.iter())
        .find(|v| v.name == name)
        .unwrap_or_else(|| panic!("declared var {name} not found in env_files"))
}

#[test]
fn parses_double_quoted_multi_line_value() {
    let repo = write_repo(&[(".env", "PRIVATE_KEY=\"line1\nline2\nline3\"\n")]);
    let index = build(repo.path());

    let var = find_var(&index, "PRIVATE_KEY");
    assert_eq!(var.name, "PRIVATE_KEY");
    assert_eq!(
        var.raw_value.as_deref(),
        Some("line1\nline2\nline3"),
        "raw_value should preserve newlines verbatim, got {:?}",
        var.raw_value
    );
}

#[test]
fn parses_single_quoted_multi_line_value() {
    let repo = write_repo(&[(".env", "PRIVATE_KEY='line1\nline2\nline3'\n")]);
    let index = build(repo.path());

    let var = find_var(&index, "PRIVATE_KEY");
    assert_eq!(var.raw_value.as_deref(), Some("line1\nline2\nline3"));
}

#[test]
fn non_secret_multi_line_display_shows_more_lines_marker() {
    let repo = write_repo(&[(".env", "CONFIG=\"key1=v1\nkey2=v2\"\n")]);
    let index = build(repo.path());

    let envelope = explain_env_var(&index, "CONFIG", ValueResolutionOpts::default());
    let binding = &envelope.entities[0]["value_bindings"][0];
    assert_eq!(binding["state"], "set");
    assert_eq!(binding["redacted"], false);
    assert_eq!(binding["display"], "key1=v1 […1 more lines]");
}

#[test]
fn secret_multi_line_redaction_counts_all_chars() {
    let repo = write_repo(&[(".env", "PRIVATE_KEY=\"abc\ndef\"\n")]);
    let index = build(repo.path());

    let envelope = explain_env_var(&index, "PRIVATE_KEY", ValueResolutionOpts::default());
    let binding = &envelope.entities[0]["value_bindings"][0];
    assert_eq!(binding["redacted"], true);
    let display = binding["display"].as_str().unwrap();
    // "abc\ndef" => 7 chars including the newline.
    assert!(
        display.starts_with("[redacted, 7 chars, sha256:"),
        "expected redaction with 7-char count, got: {display}"
    );
}

/// Safer-skip policy: an unterminated opening quote DROPS the offending var,
/// rather than consuming the remainder of the file. This preserves any
/// well-formed declarations that follow.
#[test]
fn unterminated_quote_does_not_swallow_following_vars() {
    let repo = write_repo(&[(".env", "BAD=\"this opens but never closes\nGOOD=fine\n")]);
    let index = build(repo.path());

    let bad = index
        .env_files
        .iter()
        .flat_map(|f| f.vars.iter())
        .find(|v| v.name == "BAD");
    assert!(
        bad.is_none(),
        "BAD with unterminated quote should be dropped"
    );

    let good = find_var(&index, "GOOD");
    assert_eq!(good.raw_value.as_deref(), Some("fine"));
}

/// Older indexes (pre INDEX_VERSION=6) had no `raw_value` field. The resolver
/// must still display the `value_preview` correctly when `raw_value` is None.
#[test]
fn backwards_compat_with_existing_value_preview_only_indexes() {
    use leio_code::model::{EnvFileRecord, RepoIndex};

    let mut index = RepoIndex {
        version: 0,
        root: String::new(),
        indexed_at: String::new(),
        files: Vec::new(),
        deploy_targets: Vec::new(),
        profiles: Vec::new(),
        secret_sets: Vec::new(),
        env_files: Vec::new(),
        cross_language: Default::default(),
        k8s_configmaps: Vec::new(),
    };
    index.env_files.push(EnvFileRecord {
        path: ".env".to_string(),
        precedence: 3,
        vars: vec![DeclaredVar {
            name: "LEGACY".to_string(),
            value_preview: Some("foo".to_string()),
            raw_value: None,
        }],
    });

    let bindings = resolve_value_bindings("LEGACY", &index, &ValueResolutionOpts::default());
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].display, "foo");
}
