//! Integration tests for `doctor --suggest <rule-id>`.
//!
//! - `--suggest redis_no_prefix` (High confidence, Some fn): stdout contains
//!   a unified diff fragment, exit 0.
//! - `--suggest redis_no_ttl` (Medium confidence, None fn): stdout says "no
//!   mechanical fix", exit 0.
//! - `--suggest unknown_rule`: exit 1.
//! - Backward compat: `--explain redis_no_prefix` still works, exit 0.
//!
//! These tests run against a real binary (CARGO_BIN_EXE_leio-code) but short-
//! circuit before building an index, so they are fast even on large repos.

use std::path::PathBuf;
use std::process::Command;

use leio_code::diagnostics::{SuggestConfidence, rule_doc};

/// Sanity-check the new fields on known rules without touching the CLI.
#[test]
fn redis_no_prefix_has_high_confidence_and_suggest_fn() {
    let doc = rule_doc("redis_no_prefix").expect("redis_no_prefix must be registered");
    assert_eq!(doc.suggest_confidence, SuggestConfidence::High);
    assert!(
        doc.suggest_fn.is_some(),
        "redis_no_prefix must have a suggest_fn"
    );
}

#[test]
fn redis_no_ttl_has_medium_confidence_and_no_suggest_fn() {
    let doc = rule_doc("redis_no_ttl").expect("redis_no_ttl must be registered");
    assert_eq!(doc.suggest_confidence, SuggestConfidence::Medium);
    assert!(
        doc.suggest_fn.is_none(),
        "redis_no_ttl should have no suggest_fn (medium confidence)"
    );
}

#[test]
fn all_rule_docs_have_suggest_confidence() {
    // Every rule must have an explicit confidence level (not just the new ones).
    for doc in leio_code::diagnostics::all_rule_docs() {
        // This just verifies the field is accessible — it can be any value.
        let _ = doc.suggest_confidence;
    }
}

/// Path to an empty temp dir that satisfies `canonical_repo` without
/// requiring a full Example checkout.
fn empty_repo_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("leio-code-doctor-suggest-smoke");
    std::fs::create_dir_all(&dir).expect("create temp repo dir");
    dir
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_leio-code")
}

// ── CLI tests ─────────────────────────────────────────────────────────────────

#[test]
fn cli_suggest_redis_no_prefix_emits_diff_and_exits_zero() {
    let repo = empty_repo_dir();
    let output = Command::new(bin())
        .args([
            "--repo",
            repo.to_str().unwrap(),
            "doctor",
            "all",
            "--suggest",
            "redis_no_prefix",
        ])
        .output()
        .expect("spawn leio-code");

    assert!(
        output.status.success(),
        "--suggest redis_no_prefix should exit 0; got {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Must contain a unified-diff marker.
    assert!(
        stdout.contains("---") && stdout.contains("+++"),
        "--suggest redis_no_prefix should emit a unified diff:\n{stdout}"
    );
    // Must mention the rule id so the output is self-describing.
    assert!(
        stdout.contains("redis_no_prefix"),
        "output should reference the rule id:\n{stdout}"
    );
}

#[test]
fn cli_suggest_redis_no_ttl_says_no_mechanical_fix_and_exits_zero() {
    let repo = empty_repo_dir();
    let output = Command::new(bin())
        .args([
            "--repo",
            repo.to_str().unwrap(),
            "doctor",
            "all",
            "--suggest",
            "redis_no_ttl",
        ])
        .output()
        .expect("spawn leio-code");

    assert!(
        output.status.success(),
        "--suggest redis_no_ttl should exit 0; got {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Must explain that no mechanical fix is available.
    assert!(
        stdout.to_lowercase().contains("no mechanical fix"),
        "--suggest redis_no_ttl should say 'no mechanical fix':\n{stdout}"
    );
}

#[test]
fn cli_suggest_unknown_rule_exits_one() {
    let repo = empty_repo_dir();
    let output = Command::new(bin())
        .args([
            "--repo",
            repo.to_str().unwrap(),
            "doctor",
            "all",
            "--suggest",
            "not_a_real_rule_xyz",
        ])
        .output()
        .expect("spawn leio-code");

    assert_eq!(
        output.status.code(),
        Some(1),
        "--suggest with unknown rule should exit 1; got {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not_a_real_rule_xyz"),
        "stderr should mention the bad rule id:\n{stderr}"
    );
}

/// Backward compat: `--explain` must still work after `--suggest` was added.
#[test]
fn cli_explain_redis_no_prefix_still_works() {
    let repo = empty_repo_dir();
    let output = Command::new(bin())
        .args([
            "--repo",
            repo.to_str().unwrap(),
            "doctor",
            "all",
            "--explain",
            "redis_no_prefix",
        ])
        .output()
        .expect("spawn leio-code");

    assert!(
        output.status.success(),
        "--explain redis_no_prefix should still exit 0; got {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Citation:"),
        "--explain output should still contain Citation block:\n{stdout}"
    );
}
