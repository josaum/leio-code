//! Doc-coverage ratchet for the office-parsers `*-core` crates.
//!
//! The parser cores expose the public API that every `*-py` / `*-node` / `*-flight`
//! binding re-exports. SOTA requires that API stay documented. Once a core is
//! doc-clean it is pinned with `#![deny(missing_docs)]` so undocumented public
//! items become a hard build error, not a silent warning. This doctor guards
//! that ratchet: cores that already enforce `deny(missing_docs)` must never
//! silently regress to `warn(...)` or drop the lint.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

/// `*-core` crates that have been documented and pinned to `deny(missing_docs)`.
/// Adding a core here after documenting it makes the ratchet permanent.
const DENY_DOC_CORES: &[&str] = &[
    "chunk-fast-core",
    "docx-fast-core",
    "email-fast-core",
    "fca-fast-core",
    "geo-fast-core",
    "gliner-core",
    "gliner2-core",
    "layout-fast-core",
    "mcts-fast-core",
    "owl-fast-core",
    "pdf-fast-core",
    "pptx-fast-core",
    "web-fast-core",
    "xlsx-fast-core",
    "xsd-fast-core",
];

pub struct OfficeParsersDocCoverageDoctor;

impl Doctor for OfficeParsersDocCoverageDoctor {
    fn name(&self) -> &'static str {
        "office-parsers-doc-coverage"
    }

    fn description(&self) -> &'static str {
        "Guards the office-parsers doc-coverage ratchet: cores documented and pinned to `#![deny(missing_docs)]` must not regress to `warn` or drop the lint, keeping the public parser API (re-exported by every py/node/flight binding) documented."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_office_parsers_doc_coverage(root)
    }
}

pub fn doctor_office_parsers_doc_coverage(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut read_warnings = Vec::new();

    let opr = root.join("office-parsers-rs");
    if !opr.is_dir() {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_office_parsers_doc_coverage"),
            kind: "doctor".to_string(),
            summary: "office-parsers-rs not present — doc-coverage ratchet skipped".to_string(),
            confidence: 0.9,
            entities: vec![json!({ "skipped": true })],
            evidence,
            warnings,
            meta: Some(json!({ "office_parsers_rs_present": false })),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    let mut enforced = Vec::new();
    for core in DENY_DOC_CORES {
        let rel = format!("office-parsers-rs/{core}/src/lib.rs");
        let src = read_text(&opr.join(format!("{core}/src/lib.rs")), &mut read_warnings);
        match src.as_deref() {
            Some(text) if text.contains("#![deny(missing_docs)]") => {
                enforced.push((*core).to_string());
                evidence.push(EvidenceItem {
                    kind: "office_parsers_doc_coverage".to_string(),
                    path: rel,
                    line: super::utils::find_line(text, "#![deny(missing_docs)]"),
                    detail: format!("{core} enforces deny(missing_docs)"),
                });
            }
            Some(_) => {
                warnings.push(format!(
                    "[office-parsers-doc-coverage] {core} must keep `#![deny(missing_docs)]` (doc-coverage ratchet regressed)"
                ));
                evidence.push(EvidenceItem {
                    kind: "office_parsers_doc_coverage".to_string(),
                    path: rel,
                    line: None,
                    detail: format!("{core} lost its deny(missing_docs) pin"),
                });
            }
            None => {
                warnings.push(format!(
                    "[office-parsers-doc-coverage] missing office-parsers-rs/{core}/src/lib.rs"
                ));
            }
        }
    }
    warnings.extend(
        read_warnings
            .into_iter()
            .map(|w| format!("[office-parsers-doc-coverage] {w}")),
    );

    // Surface `*-core` crates that do NOT yet enforce the ratchet, so the path
    // to full-workspace doc coverage stays discoverable via `leio-code doctor`.
    // These are informational (not warnings): documenting + pinning them is the
    // remaining SOTA work, tracked here rather than silently ignored.
    let mut candidate_cores: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&opr) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with("-core") {
                continue;
            }
            if DENY_DOC_CORES.contains(&name.as_str()) {
                continue;
            }
            if opr.join(&name).join("src/lib.rs").is_file() {
                candidate_cores.push(name);
            }
        }
    }
    candidate_cores.sort();

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_office_parsers_doc_coverage"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked doc-coverage ratchet: {}/{} pinned cores enforce deny(missing_docs), {} warning(s); {} core(s) not yet ratcheted",
            enforced.len(),
            DENY_DOC_CORES.len(),
            warnings.len(),
            candidate_cores.len()
        ),
        confidence: if warnings.is_empty() { 0.97 } else { 0.6 },
        entities: vec![json!({
            "deny_doc_cores": DENY_DOC_CORES,
            "enforced": enforced,
            "candidate_cores": candidate_cores,
        })],
        evidence,
        warnings,
        meta: Some(json!({ "office_parsers_rs_present": true })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_core(root: &Path, core: &str, first_line: &str) {
        let dir = root.join(format!("office-parsers-rs/{core}/src"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("lib.rs"), format!("{first_line}\n")).unwrap();
    }

    #[test]
    fn clean_when_all_cores_enforce_deny() {
        let tmp = TempDir::new().unwrap();
        for core in DENY_DOC_CORES {
            write_core(tmp.path(), core, "#![deny(missing_docs)]");
        }
        let env = doctor_office_parsers_doc_coverage(tmp.path());
        assert!(env.warnings.is_empty(), "warnings: {:?}", env.warnings);
    }

    #[test]
    fn flags_regression_to_warn() {
        let tmp = TempDir::new().unwrap();
        for core in DENY_DOC_CORES {
            write_core(tmp.path(), core, "#![deny(missing_docs)]");
        }
        // Regress one core back to warn.
        write_core(tmp.path(), "pdf-fast-core", "#![warn(missing_docs)]");
        let env = doctor_office_parsers_doc_coverage(tmp.path());
        assert!(
            env.warnings.iter().any(|w| w.contains("pdf-fast-core")),
            "warnings: {:?}",
            env.warnings
        );
    }
}
