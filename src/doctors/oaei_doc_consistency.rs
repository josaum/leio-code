//! OAEI documentation-consistency doctor.
//!
//! Locks documented OAEI benchmark numbers to the reproducible harness so a stale or
//! fabricated F1 cannot silently ship in a secondary doc surface.
//!
//! Provenance is the product (root `CLAUDE.md`, "Data Integrity"): a benchmark number
//! quoted in a doc is a claim anyone citing the repo will trust. Two surfaces drifted:
//! `docs/concepts/category-theory.md` reported a stale Anatomy `0.882` / Conference
//! `0.696`, and `example-oll/ALIGNMENT_BENCHMARK.md` presented a *simulated-embedding*
//! `0.993` in a way that read as a real OAEI result (the real LE-JEPA/Graph-JEPA Anatomy
//! score is ~0.0). The reproducible harness (`example-align/src/bin/full_oaei_bench.rs`
//! via `scripts/run_anatomy_sota.sh` / `run_anatomy_uberon.sh`) and the source-of-truth
//! docs (`example-align/docs/SOTA_PUSH_2026-06.md`, `OAEI_CAMPAIGN_SUBMISSION.md`)
//! reproduce Anatomy no-BK **0.877** (P 0.918 / R 0.840), Anatomy+UBERON **0.914**, and
//! Conference ra1-proxy **0.789**.
//!
//! This doctor does NOT re-run the benchmark (that needs the OAEI data + minutes of
//! compute). It asserts that:
//!   1. the source-of-truth docs still state the canonical anchors (so this doctor's
//!      pinned expectations can't silently fall out of sync with a real harness change —
//!      a real improvement changes the source-of-truth doc first, which forces these
//!      contracts to be updated deliberately);
//!   2. secondary surfaces quote the canonical values and never re-introduce the known
//!      stale ones; and
//!   3. the simulated `0.993` in `ALIGNMENT_BENCHMARK.md` stays labeled `SIMULATED`.
//!
//! As a doctor, drift surfaces on every `audit --strict` / `doctor oaei-doc-consistency`
//! run instead of at citation time.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct OaeiDocConsistencyDoctor;

impl Doctor for OaeiDocConsistencyDoctor {
    fn name(&self) -> &'static str {
        "oaei-doc-consistency"
    }

    fn description(&self) -> &'static str {
        "Locks documented OAEI benchmark numbers to the reproducible example-align harness: \
         canonical anchors (Anatomy 0.877 no-BK / 0.914 UBERON, Conference 0.789 ra1-proxy) \
         must be present in the source-of-truth and secondary docs, stale values (0.882 / 0.696) \
         must not reappear, and the simulated 0.993 must stay labeled SIMULATED."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_oaei_doc_consistency(root)
    }
}

/// A per-doc consistency contract, checked by simple substring presence.
///
/// Substring checks (not regex) are deliberate: they are false-positive-free on these
/// specific files and cannot themselves drift. `must_contain` anchors force coordinated
/// updates; `must_not_contain` bans the exact known-stale strings; `guard_if_present`
/// enforces "if token A is present, label B must also be present".
struct DocContract {
    /// repo-relative POSIX path to the tracked doc.
    path: &'static str,
    /// canonical values / markers that MUST appear in the file.
    must_contain: &'static [&'static str],
    /// known-stale / fabricated values that MUST NOT appear in the file.
    must_not_contain: &'static [&'static str],
    /// conditional guards: if `.0` appears, `.1` MUST also appear.
    guard_if_present: &'static [(&'static str, &'static str)],
}

/// Canonical OAEI numbers, pinned from the source of truth
/// `example-align/docs/SOTA_PUSH_2026-06.md` (FINAL STATUS) + `OAEI_CAMPAIGN_SUBMISSION.md`,
/// reproduced 2026-07-02. If the harness genuinely improves, update the source-of-truth
/// docs first — the `must_contain` checks below will then fail and force this table to be
/// revised deliberately, which is the intended behavior.
const CONTRACTS: &[DocContract] = &[
    // Source of truth (example-align). Requiring the anchors here validates this doctor's
    // pinned expectations against the doc it derives from.
    DocContract {
        path: "example-align/docs/OAEI_CAMPAIGN_SUBMISSION.md",
        must_contain: &["0.877", "0.914", "0.789"],
        must_not_contain: &[],
        guard_if_present: &[],
    },
    DocContract {
        path: "example-align/docs/SOTA_PUSH_2026-06.md",
        must_contain: &["0.877", "0.914"],
        must_not_contain: &[],
        guard_if_present: &[],
    },
    // Secondary surface: must quote the canonical no-BK Anatomy F1 (0.877) and the
    // ra1-proxy Conference F1 (0.789), and must never re-introduce the stale numbers.
    DocContract {
        path: "docs/concepts/category-theory.md",
        must_contain: &["0.877", "0.789"],
        must_not_contain: &["0.882", "0.696"],
        guard_if_present: &[],
    },
    // Simulated benchmark: the headline 0.993 must stay labeled SIMULATED so it cannot be
    // mistaken for a real OAEI result. If 0.993 is removed entirely (a valid cleanup) the
    // guard simply does not fire.
    DocContract {
        path: "example-oll/ALIGNMENT_BENCHMARK.md",
        must_contain: &[],
        must_not_contain: &[],
        guard_if_present: &[("0.993", "SIMULATED")],
    },
];

pub fn doctor_oaei_doc_consistency(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings: Vec<String> = Vec::new();
    let mut entities: Vec<serde_json::Value> = Vec::new();
    let mut evidence: Vec<EvidenceItem> = Vec::new();

    let mut checked_docs = 0usize;
    let mut violations = 0usize;

    for contract in CONTRACTS {
        let path = root.join(contract.path);
        if !path.is_file() {
            violations += 1;
            warnings.push(format!(
                "{}: tracked OAEI doc is missing (expected to hold canonical benchmark numbers)",
                contract.path
            ));
            evidence.push(EvidenceItem {
                kind: "oaei_doc_missing".to_string(),
                path: contract.path.to_string(),
                line: None,
                detail: "tracked OAEI doc not found on disk".to_string(),
            });
            continue;
        }

        let mut io_warnings: Vec<String> = Vec::new();
        let content = match read_text(&path, &mut io_warnings) {
            Some(content) => content,
            None => {
                warnings.extend(io_warnings);
                continue;
            }
        };
        checked_docs += 1;

        for token in contract.must_contain {
            if !content.contains(token) {
                violations += 1;
                warnings.push(format!(
                    "{}: canonical OAEI value `{}` is missing (source of truth: example-align/docs/SOTA_PUSH_2026-06.md)",
                    contract.path, token
                ));
                evidence.push(EvidenceItem {
                    kind: "oaei_canonical_missing".to_string(),
                    path: contract.path.to_string(),
                    line: None,
                    detail: format!(
                        "expected canonical value `{token}` not found; reproduce via scripts/run_anatomy_sota.sh / run_anatomy_uberon.sh"
                    ),
                });
            }
        }

        for token in contract.must_not_contain {
            if let Some(line) = first_line_containing(&content, token) {
                violations += 1;
                warnings.push(format!(
                    "{}:{}: stale/forbidden OAEI value `{}` present (canonical numbers are 0.877 / 0.914 / 0.789 per SOTA_PUSH_2026-06.md)",
                    contract.path, line, token
                ));
                evidence.push(EvidenceItem {
                    kind: "oaei_stale_value".to_string(),
                    path: contract.path.to_string(),
                    line: Some(line),
                    detail: format!("forbidden stale value `{token}` must not appear in this doc"),
                });
            }
        }

        for (token, required_label) in contract.guard_if_present {
            if content.contains(token) && !content.contains(required_label) {
                violations += 1;
                warnings.push(format!(
                    "{}: `{}` is present but the `{}` label is missing — a simulated/synthetic number is not clearly marked as such",
                    contract.path, token, required_label
                ));
                evidence.push(EvidenceItem {
                    kind: "oaei_unguarded_simulated".to_string(),
                    path: contract.path.to_string(),
                    line: first_line_containing(&content, token),
                    detail: format!(
                        "value `{token}` must be co-located with a `{required_label}` marker so it cannot be read as a real OAEI result"
                    ),
                });
            }
        }
    }

    entities.push(json!({
        "doctor": "oaei-doc-consistency",
        "checked_docs": checked_docs,
        "tracked_docs": CONTRACTS.len(),
        "violations": violations,
    }));

    let summary = if warnings.is_empty() {
        format!(
            "OAEI doc numbers match the reproducible harness across {checked_docs} tracked doc(s)"
        )
    } else {
        format!("found {violations} OAEI doc-consistency violation(s) across tracked docs")
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_oaei_doc_consistency"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.96 } else { 0.6 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

/// 1-indexed line number of the first line containing `needle`, if any.
fn first_line_containing(content: &str, needle: &str) -> Option<usize> {
    content
        .lines()
        .position(|line| line.contains(needle))
        .map(|idx| idx + 1)
}

#[cfg(test)]
mod tests {
    use super::doctor_oaei_doc_consistency;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_tempdir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-oaei-doc-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(path, contents).expect("write fixture file");
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    /// Writes all four tracked docs in a canonical, consistent state.
    fn write_canonical_fixture(dir: &Path) {
        write_file(
            &dir.join("example-align/docs/OAEI_CAMPAIGN_SUBMISSION.md"),
            "Anatomy 0.877 no-BK; Anatomy+UBERON 0.914; Conference ra1-proxy 0.789.\n",
        );
        write_file(
            &dir.join("example-align/docs/SOTA_PUSH_2026-06.md"),
            "FINAL STATUS: no-BK 0.877, UBERON 0.914.\n",
        );
        write_file(
            &dir.join("docs/concepts/category-theory.md"),
            "OAEI: Anatomy no-BK 0.877, Conference ra1-proxy 0.789.\n",
        );
        write_file(
            &dir.join("example-oll/ALIGNMENT_BENCHMARK.md"),
            "SIMULATED sanity check: F1 = 0.993 at threshold 0.6. Real SOTA lives in example-align.\n",
        );
    }

    #[test]
    fn canonical_fixture_emits_zero_warnings() {
        let dir = unique_tempdir("clean");
        write_canonical_fixture(&dir);

        let envelope = doctor_oaei_doc_consistency(&dir);

        assert!(
            envelope.warnings.is_empty(),
            "expected no warnings on canonical fixture, got: {:?}",
            envelope.warnings
        );
        cleanup(&dir);
    }

    #[test]
    fn stale_anatomy_and_conference_values_are_flagged() {
        let dir = unique_tempdir("stale");
        write_canonical_fixture(&dir);
        // Regress category-theory.md to the exact stale numbers this doctor guards.
        write_file(
            &dir.join("docs/concepts/category-theory.md"),
            "OAEI: Anatomy 0.882, Conference 0.696.\n",
        );

        let envelope = doctor_oaei_doc_consistency(&dir);

        // Two stale values present + two canonical values missing = 4 violations.
        assert_eq!(
            envelope.warnings.len(),
            4,
            "expected 4 warnings (2 stale + 2 missing canonical), got: {:?}",
            envelope.warnings
        );
        assert!(
            envelope.warnings.iter().any(|w| w.contains("0.882")),
            "expected a warning about stale 0.882"
        );
        assert!(
            envelope.warnings.iter().any(|w| w.contains("0.696")),
            "expected a warning about stale 0.696"
        );
        assert!(
            envelope
                .evidence
                .iter()
                .any(|e| e.kind == "oaei_stale_value" && e.line == Some(1)),
            "expected stale-value evidence with a line number"
        );
        cleanup(&dir);
    }

    #[test]
    fn unguarded_simulated_number_is_flagged() {
        let dir = unique_tempdir("unguarded-sim");
        write_canonical_fixture(&dir);
        // Keep 0.993 but strip the SIMULATED label — the exact fabrication risk.
        write_file(
            &dir.join("example-oll/ALIGNMENT_BENCHMARK.md"),
            "Best: F1 = 0.993 at threshold 0.6.\n",
        );

        let envelope = doctor_oaei_doc_consistency(&dir);

        assert_eq!(
            envelope.warnings.len(),
            1,
            "expected one warning for unguarded 0.993, got: {:?}",
            envelope.warnings
        );
        assert!(
            envelope
                .evidence
                .iter()
                .any(|e| e.kind == "oaei_unguarded_simulated"),
            "expected unguarded-simulated evidence, got: {:?}",
            envelope.evidence
        );
        cleanup(&dir);
    }

    #[test]
    fn removing_simulated_number_entirely_is_allowed() {
        let dir = unique_tempdir("sim-removed");
        write_canonical_fixture(&dir);
        // A valid cleanup: no 0.993 at all → guard must not fire.
        write_file(
            &dir.join("example-oll/ALIGNMENT_BENCHMARK.md"),
            "Real OAEI Anatomy F1 is ~0.0 under raw cosine; production SOTA is 0.877 in example-align.\n",
        );

        let envelope = doctor_oaei_doc_consistency(&dir);

        assert!(
            envelope.warnings.is_empty(),
            "removing 0.993 entirely must not fire the guard, got: {:?}",
            envelope.warnings
        );
        cleanup(&dir);
    }

    #[test]
    fn missing_source_of_truth_doc_is_flagged() {
        let dir = unique_tempdir("missing-sot");
        write_canonical_fixture(&dir);
        // Delete the source-of-truth campaign doc.
        let _ = fs::remove_file(dir.join("example-align/docs/OAEI_CAMPAIGN_SUBMISSION.md"));

        let envelope = doctor_oaei_doc_consistency(&dir);

        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("OAEI_CAMPAIGN_SUBMISSION.md") && w.contains("missing")),
            "expected a missing-doc warning, got: {:?}",
            envelope.warnings
        );
        cleanup(&dir);
    }

    #[test]
    fn source_of_truth_dropping_canonical_anchor_is_flagged() {
        let dir = unique_tempdir("sot-drift");
        write_canonical_fixture(&dir);
        // Simulate a harness change that removed 0.877 from the source of truth without
        // updating this doctor — must_contain should catch it and force review.
        write_file(
            &dir.join("example-align/docs/SOTA_PUSH_2026-06.md"),
            "FINAL STATUS: no-BK 0.910, UBERON 0.914.\n",
        );

        let envelope = doctor_oaei_doc_consistency(&dir);

        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("SOTA_PUSH_2026-06.md") && w.contains("0.877")),
            "expected a missing-canonical-anchor warning, got: {:?}",
            envelope.warnings
        );
        cleanup(&dir);
    }
}
