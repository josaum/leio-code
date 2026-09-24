//! Tests for `doctor --explain <rule-id>` — the static-text rule registry and
//! the CLI presentation layer on top of existing doctor envelopes.
//!
//! These are intentionally tight: the registry is static, the CLI shells out
//! to a real binary, and there is no working-tree mutation to verify. See
//! ROADMAP.md P2 #6.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use leio_code::diagnostics::{all_rule_docs, rule_doc};

#[test]
fn rule_doc_returns_some_for_known_rule() {
    for id in ["redis_no_prefix", "redis_no_ttl"] {
        let doc = rule_doc(id).unwrap_or_else(|| panic!("rule `{id}` should be registered"));
        assert_eq!(doc.rule_id, id);
        assert!(
            !doc.description.trim().is_empty(),
            "{id}: empty description"
        );
        assert!(!doc.fix_advice.trim().is_empty(), "{id}: empty fix_advice");
        assert!(!doc.citation.trim().is_empty(), "{id}: empty citation");
    }
}

#[test]
fn rule_doc_returns_none_for_unknown() {
    assert!(rule_doc("not_a_real_rule").is_none());
    assert!(rule_doc("").is_none());
}

#[test]
fn all_rule_docs_is_non_empty_and_unique() {
    let docs = all_rule_docs();
    assert!(
        docs.len() == 2,
        "expected two reusable Redis rule docs, got {}",
        docs.len()
    );
    let mut seen: HashSet<&str> = HashSet::new();
    for d in docs {
        assert!(
            seen.insert(d.rule_id),
            "duplicate rule_id `{}` in registry",
            d.rule_id
        );
        assert!(
            !d.description.trim().is_empty(),
            "{}: empty description",
            d.rule_id
        );
        assert!(
            !d.fix_advice.trim().is_empty(),
            "{}: empty fix_advice",
            d.rule_id
        );
        assert!(
            !d.citation.trim().is_empty(),
            "{}: empty citation",
            d.rule_id
        );
    }
}

/// Path to a directory that exists (so `canonical_repo` succeeds) but contains
/// no Example content (so all profile-specific doctors short-circuit).
fn empty_repo_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("leio-code-doctor-explain-smoke");
    std::fs::create_dir_all(&dir).expect("create temp repo dir");
    dir
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_leio-code")
}

#[test]
fn cli_explain_list_lists_all_rules() {
    let repo = empty_repo_dir();
    let output = Command::new(bin())
        .args([
            "--repo",
            repo.to_str().unwrap(),
            "doctor",
            "all",
            "--explain",
            "list",
        ])
        .output()
        .expect("spawn leio-code");
    assert!(
        output.status.success(),
        "explain list should exit 0, got {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for doc in all_rule_docs() {
        assert!(
            stdout.contains(doc.rule_id),
            "rule_id `{}` missing from --explain list output:\n{stdout}",
            doc.rule_id
        );
    }
}

#[test]
fn cli_explain_unknown_rule_exits_nonzero() {
    let repo = empty_repo_dir();
    let output = Command::new(bin())
        .args([
            "--repo",
            repo.to_str().unwrap(),
            "doctor",
            "all",
            "--explain",
            "not_a_real_rule",
        ])
        .output()
        .expect("spawn leio-code");
    assert_eq!(
        output.status.code(),
        Some(2),
        "unknown rule should exit 2; got {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not_a_real_rule"),
        "stderr should mention the bad rule id: {stderr}"
    );
}

#[test]
fn cli_explain_known_rule_prints_static_text() {
    // Even when there are no violations (empty repo / wrong profile), the
    // static description / citation / fix advice must still be emitted so a
    // user can paste it into a review.
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
        "explain known rule on empty repo should exit 0 (no violations); got {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("redis_no_prefix"),
        "missing rule id header: {stdout}"
    );
    assert!(
        stdout.contains("Citation:"),
        "missing citation block: {stdout}"
    );
    assert!(
        stdout.contains("Conceptual fix:"),
        "missing fix block: {stdout}"
    );
    assert!(
        stdout.contains("Violations"),
        "missing violations section: {stdout}"
    );
}
