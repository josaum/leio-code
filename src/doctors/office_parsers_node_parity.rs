//! Parity gate: every office-parsers `*-py` binding must have a matching
//! `*-node` binding.
//!
//! The workspace ships sovereign parser/engine cores with two thin binding
//! layers — PyO3 wheels (`*-py`) for the Python control plane and N-API
//! packages (`*-node`) for the TypeScript/Next.js surfaces. When a new `*-py`
//! crate lands without its `*-node` sibling, Node consumers silently fall back
//! to slower JS reimplementations or an HTTP hop. This doctor flags that drift
//! so the two binding families stay in lockstep (mirrors
//! `office-parsers-rs/scripts/check-node-parity.sh`).

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::query_id;
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

/// Base names allowed to ship Python-only (no `*-node` counterpart required).
/// Keep empty for full parity; add a base name here only with an explicit
/// architectural decision.
const PARITY_EXEMPT: &[&str] = &[];

pub struct OfficeParsersNodeParityDoctor;

impl Doctor for OfficeParsersNodeParityDoctor {
    fn name(&self) -> &'static str {
        "office-parsers-node-parity"
    }

    fn description(&self) -> &'static str {
        "Ensures every office-parsers `*-py` PyO3 binding has a matching `*-node` N-API binding so Python and TypeScript consumers share the same sovereign parser/engine cores instead of diverging into JS reimplementations."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_office_parsers_node_parity(root)
    }
}

/// Collect the set of `<base>` names for directories in `dir` ending in `suffix`.
fn base_names(dir: &Path, suffix: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(base) = name.strip_suffix(suffix) {
            out.insert(base.to_string());
        }
    }
    out
}

pub fn doctor_office_parsers_node_parity(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();

    let opr = root.join("office-parsers-rs");
    if !opr.is_dir() {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_office_parsers_node_parity"),
            kind: "doctor".to_string(),
            summary: "office-parsers-rs not present — node/py parity check skipped".to_string(),
            confidence: 0.9,
            entities: vec![json!({ "skipped": true, "reason": "office-parsers-rs absent" })],
            evidence,
            warnings,
            meta: Some(json!({ "office_parsers_rs_present": false })),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    let py = base_names(&opr, "-py");
    let node = base_names(&opr, "-node");

    let mut missing: Vec<String> = Vec::new();
    for base in &py {
        if PARITY_EXEMPT.contains(&base.as_str()) {
            continue;
        }
        if !node.contains(base) {
            missing.push(base.clone());
            warnings.push(format!(
                "[office-parsers-node-parity] {base}-py has no matching {base}-node N-API binding"
            ));
            evidence.push(EvidenceItem {
                kind: "office_parsers_node_parity".to_string(),
                path: opr.join(format!("{base}-py")).display().to_string(),
                line: None,
                detail: format!(
                    "Add office-parsers-rs/{base}-node mirroring {base}-py (see build-node.sh / check-node-parity.sh)"
                ),
            });
        }
    }

    let entities = vec![json!({
        "py_bindings": py.iter().collect::<Vec<_>>(),
        "node_bindings": node.iter().collect::<Vec<_>>(),
        "missing_node_bindings": missing,
        "exempt": PARITY_EXEMPT,
    })];

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_office_parsers_node_parity"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked office-parsers py/node binding parity ({} py, {} node), found {} missing node binding(s)",
            py.len(),
            node.len(),
            warnings.len()
        ),
        confidence: if warnings.is_empty() { 0.97 } else { 0.6 },
        entities,
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

    fn mkdirs(root: &Path, dirs: &[&str]) {
        for d in dirs {
            fs::create_dir_all(root.join("office-parsers-rs").join(d)).expect("mkdir");
        }
    }

    #[test]
    fn flags_py_without_node() {
        let tmp = TempDir::new().expect("tempdir");
        mkdirs(tmp.path(), &["foo-py", "foo-node", "bar-py", "bar-core"]);
        let env = doctor_office_parsers_node_parity(tmp.path());
        assert_eq!(env.warnings.len(), 1, "warnings: {:?}", env.warnings);
        assert!(env.warnings[0].contains("bar-py has no matching bar-node"));
        assert_eq!(env.evidence.len(), 1);
    }

    #[test]
    fn clean_when_every_py_has_node() {
        let tmp = TempDir::new().expect("tempdir");
        mkdirs(tmp.path(), &["foo-py", "foo-node", "baz-py", "baz-node"]);
        let env = doctor_office_parsers_node_parity(tmp.path());
        assert!(env.warnings.is_empty(), "warnings: {:?}", env.warnings);
    }

    #[test]
    fn skips_when_office_parsers_absent() {
        let tmp = TempDir::new().expect("tempdir");
        let env = doctor_office_parsers_node_parity(tmp.path());
        assert!(env.warnings.is_empty());
        assert!(env.summary.contains("skipped"));
    }
}
