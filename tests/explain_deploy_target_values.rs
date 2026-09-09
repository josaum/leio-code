//! Integration tests for value bindings on `explain deploy-target`.
//!
//! The behaviors under test (P1 #5 follow-up of leio-code/ROADMAP.md):
//!   1. `explain deploy-target FOO` surfaces resolved values for every env var
//!      declared by FOO's `backend_profile` and `secret_set`.
//!   2. Secret-keyed names are redacted by default; `--show-secrets` reveals.
//!   3. `.env.local` (higher precedence) wins over the backend profile.
//!   4. Targets without a profile / secret_set still expose an empty
//!      `var_bindings` object (so the field is part of the stable contract).
//!   5. The previously shipped entity fields are still present (additive
//!      contract — we must not break MCP / Apps-SDK consumers).
//!
//! Tests construct a synthetic repo with `tempfile::TempDir` and drive
//! `build_or_update_index` → `explain_deploy_target`. No mocks.

use std::fs;
use std::path::Path;

use leio_code::indexer::build_or_update_index;
use leio_code::query::explain_deploy_target;
use leio_code::value_resolution::ValueResolutionOpts;
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

/// A target with a profile that declares a non-secret variable surfaces the
/// raw value under `var_bindings.<NAME>.effective.display`.
#[test]
fn target_surfaces_var_bindings_for_profile_vars() {
    let repo = write_repo(&[
        (
            "deploy/targets/foo.toml",
            "name = \"foo\"\nbackend_profile = \"bar\"\n",
        ),
        ("deploy/profiles/bar.env", "FOO=hello\n"),
    ]);
    let index = build(repo.path());

    let envelope =
        explain_deploy_target(&index, "foo", repo.path(), ValueResolutionOpts::default());

    let entity = &envelope.entities[0];
    let vb = &entity["var_bindings"];
    assert!(vb.is_object(), "var_bindings should be an object: {vb}");

    let foo = &vb["FOO"];
    assert_eq!(foo["is_secret"], false);
    assert_eq!(foo["effective"]["state"], "set");
    assert_eq!(foo["effective"]["display"], "hello");
    assert_eq!(foo["effective"]["redacted"], false);

    let bindings = foo["bindings"].as_array().expect("bindings array");
    assert_eq!(bindings.len(), 1, "one source, one binding: {bindings:?}");
    assert_eq!(bindings[0]["source"]["kind"], "deploy_profile");
}

/// Secret-keyed names declared by the profile are redacted by default.
#[test]
fn target_secret_keys_are_redacted_by_default() {
    let repo = write_repo(&[
        (
            "deploy/targets/foo.toml",
            "name = \"foo\"\nbackend_profile = \"bar\"\n",
        ),
        ("deploy/profiles/bar.env", "API_KEY=sk-secret\n"),
    ]);
    let index = build(repo.path());

    let envelope =
        explain_deploy_target(&index, "foo", repo.path(), ValueResolutionOpts::default());
    let entity = &envelope.entities[0];

    let api = &entity["var_bindings"]["API_KEY"];
    assert_eq!(api["is_secret"], true);
    assert_eq!(api["effective"]["state"], "set");
    assert_eq!(api["effective"]["redacted"], true);
    let display = api["effective"]["display"].as_str().unwrap();
    assert!(
        display.starts_with("[redacted,"),
        "expected redaction marker, got {display}"
    );
    assert!(
        !display.contains("sk-secret"),
        "raw secret leaked: {display}"
    );
}

/// `--show-secrets` reveals the raw value for secret-keyed names.
#[test]
fn target_show_secrets_reveals_raw() {
    let repo = write_repo(&[
        (
            "deploy/targets/foo.toml",
            "name = \"foo\"\nbackend_profile = \"bar\"\n",
        ),
        ("deploy/profiles/bar.env", "API_KEY=sk-secret\n"),
    ]);
    let index = build(repo.path());

    let envelope = explain_deploy_target(
        &index,
        "foo",
        repo.path(),
        ValueResolutionOpts { show_secrets: true },
    );
    let entity = &envelope.entities[0];

    let api = &entity["var_bindings"]["API_KEY"];
    assert_eq!(api["effective"]["state"], "set");
    assert_eq!(api["effective"]["redacted"], false);
    assert_eq!(api["effective"]["display"], "sk-secret");
}

/// `.env.local` at the repo root has higher precedence than the deploy
/// profile, so it must be the effective source.
#[test]
fn target_with_dotenv_overrides_profile() {
    let repo = write_repo(&[
        (
            "deploy/targets/foo.toml",
            "name = \"foo\"\nbackend_profile = \"bar\"\n",
        ),
        ("deploy/profiles/bar.env", "FOO=from_profile\n"),
        (".env.local", "FOO=overridden\n"),
    ]);
    let index = build(repo.path());

    let envelope =
        explain_deploy_target(&index, "foo", repo.path(), ValueResolutionOpts::default());
    let entity = &envelope.entities[0];

    let foo = &entity["var_bindings"]["FOO"];
    assert_eq!(foo["effective"]["display"], "overridden");
    assert_eq!(foo["effective"]["source"]["kind"], "env_file");
    assert_eq!(foo["effective"]["source"]["path"], ".env.local");

    // Both bindings should still be listed, sorted by precedence (lowest first).
    let bindings = foo["bindings"].as_array().unwrap();
    assert_eq!(bindings.len(), 2);
    assert_eq!(bindings[0]["source"]["path"], ".env.local");
    assert_eq!(bindings[1]["source"]["kind"], "deploy_profile");
}

/// A target without a backend_profile or secret_set still exposes
/// `var_bindings` — as an empty object, not as a missing key. This keeps the
/// contract stable for downstream consumers.
#[test]
fn target_without_profile_or_secret_set_has_empty_var_bindings() {
    let repo = write_repo(&[("deploy/targets/foo.toml", "name = \"foo\"\n")]);
    let index = build(repo.path());

    let envelope =
        explain_deploy_target(&index, "foo", repo.path(), ValueResolutionOpts::default());
    let entity = &envelope.entities[0];

    let vb = entity.get("var_bindings").expect("var_bindings present");
    assert!(vb.is_object(), "var_bindings must be an object: {vb}");
    assert_eq!(
        vb.as_object().unwrap().len(),
        0,
        "no profile/secret_set declared → no vars"
    );
}

/// Pin the existing entity fields so the change is purely additive.
#[test]
fn existing_target_fields_remain_present() {
    let repo = write_repo(&[
        (
            "deploy/targets/foo.toml",
            "name = \"foo\"\nbackend_profile = \"bar\"\nsecret_set = \"baz\"\n",
        ),
        ("deploy/profiles/bar.env", "FOO=hello\n"),
        ("deploy/secret-sets/baz.env.example", "TOKEN=\n"),
    ]);
    let index = build(repo.path());

    let envelope =
        explain_deploy_target(&index, "foo", repo.path(), ValueResolutionOpts::default());
    let entity = &envelope.entities[0];

    for key in [
        "name",
        "path",
        "deploy_class",
        "topology",
        "ui_role",
        "ui_path",
        "frontend_project",
        "backend_profile",
        "readiness_target",
        "readiness_target_exists",
        "secret_set",
        "cartridges",
        "required_integrations",
        "health_checks",
        "smoke_suite",
        "smoke_exists",
        "smoke_target",
        "rollback_command",
        "rollback_exists",
        "rollback_target",
        "profile_vars",
        "secret_vars",
        "var_bindings",
    ] {
        assert!(entity.get(key).is_some(), "missing entity field: {key}");
    }
}

/// Vars declared by the target's secret_set are surfaced too. Most secret-set
/// `.env.example` entries are empty placeholders → effective state is `empty`.
#[test]
fn target_surfaces_secret_set_vars() {
    let repo = write_repo(&[
        (
            "deploy/targets/foo.toml",
            "name = \"foo\"\nsecret_set = \"baz\"\n",
        ),
        ("deploy/secret-sets/baz.env.example", "TOKEN=\n"),
    ]);
    let index = build(repo.path());

    let envelope =
        explain_deploy_target(&index, "foo", repo.path(), ValueResolutionOpts::default());
    let entity = &envelope.entities[0];

    let token = &entity["var_bindings"]["TOKEN"];
    assert_eq!(token["is_secret"], true);
    // The declaration is empty in the example file → state = empty.
    assert_eq!(token["effective"]["state"], "empty");
}
