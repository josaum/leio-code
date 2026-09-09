//! Integration tests for value bindings on `explain env-var`.
//!
//! The behaviors under test (P1 #5 of leio-code/ROADMAP.md):
//!   1. `explain env-var FOO` surfaces the resolved value when FOO is set in `.env`.
//!   2. `.env.local` overrides `.env` (higher precedence).
//!   3. Secret-keyed variables are redacted by default; raw value is hidden.
//!   4. `--show-secrets` reveals the raw value for secret-keyed variables.
//!   5. Unset variables report state=unset with no value.
//!   6. The JSON envelope shape is stable (`value_bindings` is an array of objects
//!      with `state`, `display`, `redacted`, `source.kind`, `source.path`).
//!
//! These tests use `tempfile::TempDir` to construct a synthetic repo on disk and
//! drive the full `build_or_update_index` → `explain_env_var` pipeline. No mocks.

use std::fs;
use std::path::Path;

use leio_code::indexer::build_or_update_index;
use leio_code::query::explain_env_var;
use leio_code::value_resolution::{ValueResolutionOpts, check_show_secrets_guard};
use tempfile::TempDir;

/// Write a synthetic repo. Each tuple is `(relative_path, content)`.
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

#[test]
fn explain_env_var_surfaces_value_from_dotenv() {
    let repo = write_repo(&[
        (".env", "FOO=bar\n"),
        (
            "src/main.rs",
            "fn main() { let _ = std::env::var(\"FOO\"); }\n",
        ),
    ]);
    let index = build(repo.path());

    let envelope = explain_env_var(&index, "FOO", ValueResolutionOpts::default());

    let entity = &envelope.entities[0];
    let bindings = entity["value_bindings"]
        .as_array()
        .expect("value_bindings array");
    assert_eq!(bindings.len(), 1, "expected one binding, got {bindings:?}");
    let b = &bindings[0];
    assert_eq!(b["state"], "set");
    assert_eq!(b["display"], "bar");
    assert_eq!(b["redacted"], false);
    assert_eq!(b["source"]["kind"], "env_file");
    assert_eq!(b["source"]["path"], ".env");
}

/// `.env.local` should sort BEFORE `.env` because it has higher precedence
/// (precedence value 0 vs 3). The `effective` field on the entity reflects
/// the top-precedence binding.
#[test]
fn dotenv_local_overrides_dotenv() {
    let repo = write_repo(&[
        (".env", "FOO=committed\n"),
        (".env.local", "FOO=local_override\n"),
    ]);
    let index = build(repo.path());

    let envelope = explain_env_var(&index, "FOO", ValueResolutionOpts::default());
    let entity = &envelope.entities[0];
    let bindings = entity["value_bindings"].as_array().unwrap();
    assert_eq!(bindings.len(), 2);
    // Sorted by precedence: .env.local (0) first, .env (3) second.
    assert_eq!(bindings[0]["source"]["path"], ".env.local");
    assert_eq!(bindings[0]["display"], "local_override");
    assert_eq!(bindings[1]["source"]["path"], ".env");
    assert_eq!(bindings[1]["display"], "committed");
    // The `effective` field surfaces the winner.
    assert_eq!(entity["effective"]["display"], "local_override");
}

#[test]
fn secret_keyed_var_is_redacted_by_default() {
    let repo = write_repo(&[(".env", "API_KEY=sk-live-supersecret\n")]);
    let index = build(repo.path());

    let envelope = explain_env_var(&index, "API_KEY", ValueResolutionOpts::default());
    let entity = &envelope.entities[0];

    assert_eq!(entity["is_secret"], true);
    let b = &entity["value_bindings"][0];
    assert_eq!(b["state"], "set");
    assert_eq!(b["redacted"], true);
    let display = b["display"].as_str().unwrap();
    assert!(
        display.starts_with("[redacted,"),
        "expected redaction marker, got {display}"
    );
    assert!(
        !display.contains("supersecret"),
        "raw value leaked: {display}"
    );
    // sha256 fingerprint lets us identify whether two redacted slots hold
    // the same value WITHOUT leaking either. Format: `sha256:<8 hex chars>`.
    assert!(
        display.contains("sha256:"),
        "expected sha256 fingerprint, got {display}"
    );
}

/// Two distinct secret keys must have distinct sha256 fingerprints, and
/// two identical raw secret values must have IDENTICAL fingerprints.
#[test]
fn sha256_fingerprint_is_stable_per_value() {
    let repo_a = write_repo(&[(".env", "API_KEY=value_one\n")]);
    let repo_b = write_repo(&[(".env", "API_KEY=value_one\n")]);
    let repo_c = write_repo(&[(".env", "API_KEY=value_two\n")]);

    let fp = |repo: &TempDir| {
        let idx = build(repo.path());
        let env = explain_env_var(&idx, "API_KEY", ValueResolutionOpts::default());
        env.entities[0]["value_bindings"][0]["display"]
            .as_str()
            .unwrap()
            .to_string()
    };

    let a = fp(&repo_a);
    let b = fp(&repo_b);
    let c = fp(&repo_c);
    assert_eq!(a, b, "same value should produce same fingerprint");
    assert_ne!(
        a, c,
        "different values should produce different fingerprints"
    );
}

/// `--show-secrets` outside a TTY MUST be refused unless the operator passes
/// `--i-know-what-i-am-doing`. This prevents accidentally piping raw secret
/// values into log collectors, file redirects, or CI captures.
#[test]
fn show_secrets_refused_when_not_tty_and_not_acknowledged() {
    // Pure-function guard: arguments are (show_secrets, override_ack, is_tty).
    assert!(
        check_show_secrets_guard(true, false, false).is_err(),
        "should refuse: not a TTY and no override"
    );
}

#[test]
fn show_secrets_allowed_when_tty() {
    assert!(check_show_secrets_guard(true, false, true).is_ok());
}

#[test]
fn show_secrets_allowed_when_acknowledged() {
    assert!(check_show_secrets_guard(true, true, false).is_ok());
}

#[test]
fn guard_inert_when_show_secrets_false() {
    assert!(check_show_secrets_guard(false, false, false).is_ok());
    assert!(check_show_secrets_guard(false, false, true).is_ok());
}

#[test]
fn show_secrets_reveals_raw_value() {
    let repo = write_repo(&[(".env", "API_KEY=sk-live-supersecret\n")]);
    let index = build(repo.path());

    let envelope = explain_env_var(
        &index,
        "API_KEY",
        ValueResolutionOpts { show_secrets: true },
    );
    let entity = &envelope.entities[0];
    let b = &entity["value_bindings"][0];

    assert_eq!(b["state"], "set");
    assert_eq!(b["redacted"], false);
    assert_eq!(b["display"], "sk-live-supersecret");
}

#[test]
fn unset_var_reports_unset_state() {
    let repo = write_repo(&[(".env", "OTHER=irrelevant\n")]);
    let index = build(repo.path());

    let envelope = explain_env_var(&index, "MISSING", ValueResolutionOpts::default());
    let entity = &envelope.entities[0];

    assert_eq!(entity["value_bindings"].as_array().unwrap().len(), 0);
    assert_eq!(entity["effective"]["state"], "unset");
}

#[test]
fn empty_value_reports_empty_state() {
    let repo = write_repo(&[(".env", "FOO=\n")]);
    let index = build(repo.path());

    let envelope = explain_env_var(&index, "FOO", ValueResolutionOpts::default());
    let entity = &envelope.entities[0];
    let bindings = entity["value_bindings"].as_array().unwrap();

    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0]["state"], "empty");
    assert_eq!(bindings[0]["display"], "");
}

/// The JSON envelope is the contract for the MCP wrapper and Apps-SDK
/// surfaces. This test pins the shape so future changes are visible in diffs.
#[test]
fn envelope_shape_is_stable() {
    let repo = write_repo(&[(".env", "FOO=bar\n")]);
    let index = build(repo.path());
    let envelope = explain_env_var(&index, "FOO", ValueResolutionOpts::default());

    let entity = &envelope.entities[0];
    // Required keys on the entity.
    for key in [
        "name",
        "is_secret",
        "code_uses",
        "profiles",
        "secret_sets",
        "value_bindings",
        "effective",
    ] {
        assert!(entity.get(key).is_some(), "missing entity field: {key}");
    }

    let b = &entity["value_bindings"][0];
    for key in ["state", "display", "redacted", "source"] {
        assert!(b.get(key).is_some(), "missing binding field: {key}");
    }
    for key in ["kind", "path", "precedence"] {
        assert!(
            b["source"].get(key).is_some(),
            "missing source field: {key}"
        );
    }
}
