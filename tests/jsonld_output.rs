//! Integration tests for the JSON-LD output surface (P1 #4 in ROADMAP.md).
//!
//! Exercises `render_envelope_as_jsonld` against the real `find_env_vars` and
//! `explain_env_var` pipelines (via `build_or_update_index` against a synthetic
//! repo) and the `apply_where_filter` jq-subset filter.

use std::fs;
use std::path::Path;

use leio_code::indexer::{build_or_update_index, default_index_path};
use leio_code::jsonld::{CONTEXT, apply_where_filter, render_envelope_as_jsonld};
use leio_code::query::{explain_env_var, find_env_vars};
use leio_code::value_resolution::ValueResolutionOpts;
use tempfile::TempDir;

fn build_synthetic_repo_with_env() -> (TempDir, leio_code::model::RepoIndex) {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    fs::write(
        root.join(".env"),
        "FOO=bar\nDATABASE_URL=postgres://localhost/db\n",
    )
    .expect("write .env");
    let index_path = default_index_path(root);
    let index = build_or_update_index(root, &index_path, true).expect("build index");
    (tmp, index)
}

#[test]
fn find_envvar_format_jsonld_has_context_and_type() {
    let (_tmp, index) = build_synthetic_repo_with_env();
    let envelope = find_env_vars(&index, "FOO");

    let doc = render_envelope_as_jsonld(&envelope);
    assert_eq!(doc.get("@context").and_then(|v| v.as_str()), Some(CONTEXT));
    assert_eq!(
        doc.get("@type").and_then(|v| v.as_str()),
        Some("FindResult")
    );
    assert!(
        doc.get("@id")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty()),
        "@id should be present and non-empty"
    );

    let entities = doc
        .get("entities")
        .and_then(|v| v.as_array())
        .expect("entities array");
    assert!(!entities.is_empty(), "expected at least one entity");
    assert_eq!(
        entities[0].get("@type").and_then(|v| v.as_str()),
        Some("EnvVar")
    );
}

#[test]
fn find_envvar_jsonld_has_full_path_provenance() {
    let (tmp, index) = build_synthetic_repo_with_env();
    leio_code::jsonld::bind_repo(tmp.path());
    let envelope = find_env_vars(&index, "FOO");
    let doc = render_envelope_as_jsonld(&envelope);
    let full = doc["entities"][0]["fullPath"].as_str().expect("fullPath");
    assert!(
        Path::new(full).is_absolute(),
        "expected absolute fullPath, got {full}"
    );
    assert!(full.ends_with(".env"), "{full}");
    assert!(
        doc["entities"][0]["@id"]
            .as_str()
            .unwrap()
            .starts_with("file://")
    );
    let used = doc
        .get("http://www.w3.org/ns/prov#used")
        .and_then(|v| v.as_array())
        .expect("prov:used");
    assert!(!used.is_empty());
    assert_eq!(used[0]["fullPath"].as_str(), Some(full));
    assert!(doc["checkout"]["repoId"].as_str().is_some());
    assert!(doc["worktree"]["worktree"].as_str().is_some());
}

#[test]
fn explain_envvar_format_jsonld_has_context() {
    let (_tmp, index) = build_synthetic_repo_with_env();
    let envelope = explain_env_var(&index, "FOO", ValueResolutionOpts::default());

    let doc = render_envelope_as_jsonld(&envelope);
    assert_eq!(doc.get("@context").and_then(|v| v.as_str()), Some(CONTEXT));
    assert_eq!(
        doc.get("@type").and_then(|v| v.as_str()),
        Some("ExplainResult")
    );
}

#[test]
fn jsonld_output_roundtrips_through_jq_subset() {
    let (_tmp, index) = build_synthetic_repo_with_env();
    let envelope = find_env_vars(&index, "");
    let doc = render_envelope_as_jsonld(&envelope);

    let filtered =
        apply_where_filter(&doc, r#".entities[] | select(.name == "FOO")"#).expect("filter ok");
    let entities = filtered
        .get("entities")
        .and_then(|v| v.as_array())
        .expect("filtered entities");
    assert_eq!(entities.len(), 1, "expected exactly one entity named FOO");
    assert_eq!(
        entities[0].get("name").and_then(|v| v.as_str()),
        Some("FOO")
    );
    // Envelope-level metadata survives filtering.
    assert_eq!(
        filtered.get("@context").and_then(|v| v.as_str()),
        Some(CONTEXT)
    );
}

#[test]
fn where_with_contains() {
    let (_tmp, index) = build_synthetic_repo_with_env();
    let envelope = find_env_vars(&index, "");
    let doc = render_envelope_as_jsonld(&envelope);

    // `contains` on a string field is substring containment. The env-var entity
    // has a `path` field whose value ends with `.env`; this filter selects
    // every entity whose path string contains the literal ".env".
    let filtered = apply_where_filter(&doc, r#".entities[] | select(.path | contains(".env"))"#)
        .expect("filter ok");
    let entities = filtered
        .get("entities")
        .and_then(|v| v.as_array())
        .expect("filtered entities");
    assert!(
        !entities.is_empty(),
        "expected at least one .env entity to match"
    );
    for entity in entities {
        let path = entity
            .get("path")
            .and_then(|v| v.as_str())
            .expect("entity has path");
        assert!(path.contains(".env"), "matched path missing `.env`: {path}");
    }
}

#[test]
fn where_unsupported_grammar_returns_error() {
    let (_tmp, index) = build_synthetic_repo_with_env();
    let envelope = find_env_vars(&index, "");
    let doc = render_envelope_as_jsonld(&envelope);

    let err = apply_where_filter(&doc, ".entities | map(.) | length > 3")
        .expect_err("unsupported grammar must error");
    let msg = format!("{err}");
    assert!(
        msg.to_ascii_lowercase().contains("unsupported")
            || msg.to_ascii_lowercase().contains("expected"),
        "error should explain unsupported grammar; got: {msg}"
    );
}
