//! Integration tests for shell-environment provenance in `explain env-var`.
//!
//! Shell env is checked at query time (not indexed). A variable present in the
//! process environment surfaces as a `ShellEnv` binding with precedence -1,
//! which sorts it BEFORE all file-based bindings.
//!
//! Isolation: each test uses a unique variable name to avoid interference from
//! parallel test runs. The variable is removed from the environment after the
//! assertion block.

use std::fs;
use std::path::Path;

use leio_code::indexer::build_or_update_index;
use leio_code::query::explain_env_var;
use leio_code::value_resolution::ValueResolutionOpts;
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

/// A non-secret shell variable surfaces as a `ShellEnv` binding with its raw
/// value shown (not redacted) and sorts before any file-based bindings.
#[test]
fn shell_env_non_secret_var_surfaces_with_value() {
    // NB: must not end in any of the secret suffixes (`_SECRET`, `_KEY`,
    // `_TOKEN`, `_PASSWORD`, `_PWD`, `_PRIVATE_KEY`, `_CREDENTIAL[S]`)
    // checked by `value_resolution::is_secret_key`, otherwise the test
    // variable will be redacted by design.
    let var = "LEIO_TEST_SHELL_PLAIN_VAR";
    // Safety: test-only, unique name, restored before function returns.
    unsafe { std::env::set_var(var, "hello_from_shell") };

    let repo = write_repo(&[(".env", "LEIO_TEST_SHELL_PLAIN_VAR=file_value\n")]);
    let index = build(repo.path());

    let envelope = explain_env_var(&index, var, ValueResolutionOpts::default());
    let entity = &envelope.entities[0];
    let bindings = entity["value_bindings"].as_array().expect("value_bindings");

    // Must have at least two bindings: ShellEnv first, EnvFile second.
    assert!(
        bindings.len() >= 2,
        "expected ShellEnv + EnvFile bindings, got {bindings:?}"
    );

    // The ShellEnv binding should be first (precedence -1 sorts lowest).
    let shell_b = &bindings[0];
    assert_eq!(
        shell_b["source"]["kind"], "shell_env",
        "first binding should be shell_env"
    );
    assert_eq!(shell_b["state"], "set");
    assert_eq!(shell_b["display"], "hello_from_shell");
    assert_eq!(shell_b["redacted"], false);

    // The `effective` binding must reflect the shell value.
    assert_eq!(entity["effective"]["display"], "hello_from_shell");
    assert_eq!(entity["effective"]["source"]["kind"], "shell_env");

    unsafe { std::env::remove_var(var) };
}

/// A secret-named shell variable must be redacted with the standard
/// `[redacted, N chars, sha256:…]` format.
#[test]
fn shell_env_secret_var_is_redacted() {
    let var = "LEIO_TEST_SHELL_SECRET";
    unsafe { std::env::set_var(var, "supersecret_from_shell") };

    let repo = write_repo(&[]);
    let index = build(repo.path());

    let envelope = explain_env_var(&index, var, ValueResolutionOpts::default());
    let entity = &envelope.entities[0];
    let bindings = entity["value_bindings"].as_array().expect("value_bindings");

    assert_eq!(bindings.len(), 1, "expected exactly one ShellEnv binding");

    let b = &bindings[0];
    assert_eq!(b["source"]["kind"], "shell_env");
    assert_eq!(b["state"], "set");
    assert_eq!(b["redacted"], true);

    let display = b["display"].as_str().unwrap();
    assert!(
        display.starts_with("[redacted,"),
        "expected redaction marker, got {display}"
    );
    assert!(
        !display.contains("supersecret_from_shell"),
        "raw value leaked: {display}"
    );
    assert!(
        display.contains("sha256:"),
        "expected sha256 fingerprint, got {display}"
    );

    unsafe { std::env::remove_var(var) };
}

/// When the variable is NOT set in the shell, no ShellEnv binding is emitted.
/// File-based bindings are unaffected.
#[test]
fn shell_env_absent_var_emits_no_binding() {
    let var = "LEIO_TEST_SHELL_ABSENT_VAR_XQ7";
    // Ensure it's not set.
    unsafe { std::env::remove_var(var) };

    let repo = write_repo(&[(".env", "LEIO_TEST_SHELL_ABSENT_VAR_XQ7=file_only\n")]);
    let index = build(repo.path());

    let envelope = explain_env_var(&index, var, ValueResolutionOpts::default());
    let entity = &envelope.entities[0];
    let bindings = entity["value_bindings"].as_array().expect("value_bindings");

    // Only the file binding — no ShellEnv entry.
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0]["source"]["kind"], "env_file");
}
