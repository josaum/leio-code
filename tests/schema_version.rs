//! Tests for the `schema_version` field on `QueryEnvelope` and on the
//! diagnostic-format root documents. Locks in the contract in
//! `docs/output-schema.md` §7: `schema_version` is the first JSON key,
//! pinned to `"1.0"` until a breaking change ships.
//!
//! Why both a value check and a first-key check: pinning the value alone
//! lets a future refactor silently move the field below others, breaking
//! consumers that read the version *before* parsing the rest of the
//! envelope (which is the whole point of the field). The first-key check
//! goes through `to_string`, not `to_value`, because `serde_json::Map`
//! sorts alphabetically without the `preserve_order` feature — and
//! `schema_version` would happen to sort first today by accident, masking
//! a real regression.

use leio_code::diagnostics::{DiagFormat, RunMeta, render};
use leio_code::indexer::{build_or_update_index, default_index_path};
use leio_code::model::SCHEMA_VERSION;
use leio_code::query::find_subprocess_callers;
use tempfile::TempDir;

fn empty_index() -> (TempDir, leio_code::model::RepoIndex) {
    let tmp = TempDir::new().expect("tempdir");
    let index_path = default_index_path(tmp.path());
    let index = build_or_update_index(tmp.path(), &index_path, true).expect("build index");
    (tmp, index)
}

#[test]
fn query_envelope_carries_schema_version_one_dot_zero() {
    let (_tmp, index) = empty_index();
    let envelope = find_subprocess_callers(&index, "leio-code");
    assert_eq!(envelope.schema_version, "1.0");
    assert_eq!(envelope.schema_version, SCHEMA_VERSION);
}

#[test]
fn query_envelope_serializes_schema_version_as_first_key() {
    let (_tmp, index) = empty_index();
    let envelope = find_subprocess_callers(&index, "leio-code");

    // `to_string` walks the struct in declaration order; `to_value` does
    // not without the `preserve_order` feature on `serde_json`. So we
    // check the raw text, not the parsed `Value`.
    let raw = serde_json::to_string(&envelope).expect("serialize envelope");
    assert!(
        raw.starts_with(r#"{"schema_version":"1.0""#),
        "schema_version must be the first key; got: {}",
        &raw[..raw.len().min(80)]
    );
}

#[test]
fn diagnostic_json_format_carries_schema_version_as_first_key() {
    let (_tmp, index) = empty_index();
    let envelope = find_subprocess_callers(&index, "leio-code");
    let meta = RunMeta {
        index_version: leio_code::indexer::index_version(),
        commit_sha: "test".to_string(),
        tool_version: env!("CARGO_PKG_VERSION"),
    };
    let json = render(&envelope, DiagFormat::Json, &meta);
    // `to_string_pretty` indents but the first key still appears first.
    let trimmed = json.trim_start_matches(['{', ' ', '\n']);
    assert!(
        trimmed.starts_with(r#""schema_version": "1.0""#),
        "schema_version must be the first key of the JSON diagnostic root; got prefix: {}",
        &json[..json.len().min(120)]
    );
}

#[test]
fn diagnostic_sarif_format_carries_schema_version_at_root_and_in_properties() {
    let (_tmp, index) = empty_index();
    let envelope = find_subprocess_callers(&index, "leio-code");
    let meta = RunMeta {
        index_version: leio_code::indexer::index_version(),
        commit_sha: "test".to_string(),
        tool_version: env!("CARGO_PKG_VERSION"),
    };
    let sarif = render(&envelope, DiagFormat::Sarif, &meta);

    // SARIF root: `schema_version` is first, alongside (not replacing)
    // the SARIF format's own `"version": "2.1.0"`.
    let trimmed = sarif.trim_start_matches(['{', ' ', '\n']);
    assert!(
        trimmed.starts_with(r#""schema_version": "1.0""#),
        "schema_version must be the first key of the SARIF root; got prefix: {}",
        &sarif[..sarif.len().min(120)]
    );
    // Mirrored under runs[0].invocations[0].properties for strict consumers.
    let parsed: serde_json::Value = serde_json::from_str(&sarif).expect("parse sarif");
    assert_eq!(
        parsed
            .pointer("/runs/0/invocations/0/properties/schema_version")
            .and_then(|v| v.as_str()),
        Some("1.0")
    );
    // SARIF format version is unchanged.
    assert_eq!(
        parsed.get("version").and_then(|v| v.as_str()),
        Some("2.1.0")
    );
}
