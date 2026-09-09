//! `layout-contract-platform` doctor.
//!
//! Guards the layout contract introduced when `example-extraction-contracts`
//! gained a `layout` module and `example-platform/example-office` was wired to
//! produce reading-order-aware `DocumentLayout` from PDFs. Sibling to
//! [`super::layout_fast_spectral_contract`], which guards the *gateway* side of
//! the same engine; this doctor guards the *platform / contract* side.
//!
//! The wiring lives in five places, and each is a silent-failure risk:
//!
//!   1. The contract types in `example-extraction-contracts/src/layout.rs`
//!      (`DocumentLayout` / `LayoutRegion` / `EntityProvenance`). Delete these
//!      and downstream spatial provenance has nowhere to live.
//!   2. The contract module export + the optional fields on `ExtractionResult`
//!      / `ExtractionEntity`. Drop the `pub mod layout` or the fields and the
//!      contract is unreachable from the extraction surface.
//!   3. `example-office`'s dependency on the engine (`layout-fast-core`), the
//!      detector (`pdf-fast-core`), and the contract crate. Remove any and the
//!      PDF layout path can't be built.
//!   4. The conversion in `example-office/src/layout.rs`: the RoutedEngine
//!      call-site and the permutation safety net (`is_valid_permutation`).
//!      Without the safety net the engine could silently drop a block.
//!   5. `parse_pdf` wiring the `layout` field on `ParsedDocument`. Drop it and
//!      every PDF parse silently loses its reading order.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct LayoutContractPlatformDoctor;

impl Doctor for LayoutContractPlatformDoctor {
    fn name(&self) -> &'static str {
        "layout-contract-platform"
    }

    fn description(&self) -> &'static str {
        "Guards the document-layout contract wiring: the layout types + optional fields in example-extraction-contracts, example-office's dependency on layout-fast-core/pdf-fast-core/the contract crate, the RoutedEngine conversion with its permutation safety net, and the ParsedDocument.layout field wired by parse_pdf."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_layout_contract_platform(root)
    }
}

const CONTRACT_LAYOUT_REL: &str = "example-extraction-contracts/src/layout.rs";
const CONTRACT_LIB_REL: &str = "example-extraction-contracts/src/lib.rs";
const OFFICE_CARGO_REL: &str = "example-platform/example-office/Cargo.toml";
const OFFICE_LAYOUT_REL: &str = "example-platform/example-office/src/layout.rs";
const OFFICE_PARSER_REL: &str = "example-platform/example-office/src/parser.rs";

/// `(relative path, substring that must be present, drift message)`.
const CHECKS: &[(&str, &str, &str)] = &[
    (
        CONTRACT_LAYOUT_REL,
        "pub struct DocumentLayout",
        "layout contract type `DocumentLayout` missing — the wire contract for reading-order layout has no home",
    ),
    (
        CONTRACT_LAYOUT_REL,
        "pub struct LayoutRegion",
        "layout contract type `LayoutRegion` missing",
    ),
    (
        CONTRACT_LAYOUT_REL,
        "pub struct EntityProvenance",
        "layout contract type `EntityProvenance` missing — extracted entities lose spatial provenance",
    ),
    (
        CONTRACT_LIB_REL,
        "pub mod layout",
        "`pub mod layout` export missing from example-extraction-contracts — the contract is unreachable",
    ),
    (
        CONTRACT_LIB_REL,
        "pub layout: Option<DocumentLayout>",
        "`ExtractionResult.layout` field missing — extraction results can no longer carry layout",
    ),
    (
        CONTRACT_LIB_REL,
        "pub provenance: Option<EntityProvenance>",
        "`ExtractionEntity.provenance` field missing — entities can no longer carry spatial provenance",
    ),
    (
        OFFICE_CARGO_REL,
        "layout-fast-core",
        "example-office no longer depends on layout-fast-core — the reading-order engine is unavailable",
    ),
    (
        OFFICE_CARGO_REL,
        "pdf-fast-core",
        "example-office no longer depends on pdf-fast-core — positioned PDF blocks are unavailable",
    ),
    (
        OFFICE_CARGO_REL,
        "example-extraction-contracts",
        "example-office no longer depends on example-extraction-contracts — the layout contract type is unavailable",
    ),
    (
        OFFICE_LAYOUT_REL,
        "pub fn document_layout_from_pdf",
        "`document_layout_from_pdf` entry point missing — parse_pdf has nothing to call",
    ),
    (
        OFFICE_LAYOUT_REL,
        "RoutedEngine::with_default_config",
        "RoutedEngine call-site missing in example-office/layout.rs — reading order falls back to emission order (loses the 92.3% routed policy)",
    ),
    (
        OFFICE_LAYOUT_REL,
        "fn is_valid_permutation",
        "permutation safety net (`is_valid_permutation`) missing — the engine could silently drop or duplicate a block",
    ),
    (
        OFFICE_PARSER_REL,
        "pub layout: Option<example_extraction_contracts::DocumentLayout>",
        "`ParsedDocument.layout` field missing — parsed documents lose their reading-order layout",
    ),
    (
        OFFICE_PARSER_REL,
        "document_layout_from_pdf(path)",
        "parse_pdf no longer calls `document_layout_from_pdf` — every PDF parse silently loses its layout",
    ),
];

pub fn doctor_layout_contract_platform(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut entities = Vec::new();

    // Read each referenced file once; cache by relative path.
    let mut file_cache: Vec<(&str, Option<String>)> = Vec::new();
    for (rel, _, _) in CHECKS {
        if !file_cache.iter().any(|(p, _)| p == rel) {
            let path = root.join(rel);
            let body = if path.is_file() {
                read_text(&path, &mut warnings)
            } else {
                warnings.push(format!(
                    "{rel} not found — cannot verify layout contract wiring"
                ));
                None
            };
            file_cache.push((rel, body));
        }
    }

    let mut present = 0usize;
    for (rel, marker, message) in CHECKS {
        let body = file_cache
            .iter()
            .find(|(p, _)| p == rel)
            .and_then(|(_, b)| b.as_deref());
        let ok = body.is_some_and(|b| b.contains(marker));
        if ok {
            present += 1;
        } else if body.is_some() {
            // File present but marker absent → real drift (not just a
            // missing file, which is already warned above).
            warnings.push(format!("{rel}: {message}"));
            evidence.push(EvidenceItem {
                kind: "layout_contract_marker_missing".to_string(),
                path: rel.to_string(),
                line: None,
                detail: format!("expected `{marker}` in {rel}"),
            });
        }
    }

    entities.push(json!({
        "doctor": "layout-contract-platform",
        "checks_total": CHECKS.len(),
        "checks_present": present,
        "contract_layout_file": CONTRACT_LAYOUT_REL,
        "office_layout_file": OFFICE_LAYOUT_REL,
    }));

    let summary = if warnings.is_empty() {
        "layout contract wiring intact: contract types + optional fields present, example-office depends on the engine/detector/contract, RoutedEngine conversion + permutation safety net present, parse_pdf wires the layout field".to_string()
    } else {
        format!(
            "{} drift signal(s) in the document-layout contract / platform wiring",
            warnings.len()
        )
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_layout_contract_platform"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.9 } else { 0.6 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_repo(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "leio_layout_contract_platform_{label}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    /// Write a fully-wired fixture repo that satisfies every marker.
    fn write_green_repo(root: &Path) {
        write(
            root,
            CONTRACT_LAYOUT_REL,
            "pub struct DocumentLayout {}\npub struct LayoutRegion {}\npub struct EntityProvenance {}\n",
        );
        write(
            root,
            CONTRACT_LIB_REL,
            "pub mod layout;\npub layout: Option<DocumentLayout>,\npub provenance: Option<EntityProvenance>,\n",
        );
        write(
            root,
            OFFICE_CARGO_REL,
            "layout-fast-core = { workspace = true }\npdf-fast-core = { workspace = true }\nexample-extraction-contracts.workspace = true\n",
        );
        write(
            root,
            OFFICE_LAYOUT_REL,
            "pub fn document_layout_from_pdf() {}\nlet e = RoutedEngine::with_default_config();\nfn is_valid_permutation() {}\n",
        );
        write(
            root,
            OFFICE_PARSER_REL,
            "pub layout: Option<example_extraction_contracts::DocumentLayout>,\nlet layout = document_layout_from_pdf(path);\n",
        );
    }

    #[test]
    fn green_when_fully_wired() {
        let root = temp_repo("green");
        write_green_repo(&root);
        let env = doctor_layout_contract_platform(&root);
        assert!(
            env.warnings.is_empty(),
            "expected no warnings, got: {:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn flags_missing_parsed_document_field() {
        let root = temp_repo("missing_field");
        write_green_repo(&root);
        // Regress parser.rs: drop the layout field, keep the call.
        write(
            &root,
            OFFICE_PARSER_REL,
            "let layout = document_layout_from_pdf(path);\n",
        );
        let env = doctor_layout_contract_platform(&root);
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("ParsedDocument.layout")),
            "expected a ParsedDocument.layout drift warning, got: {:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn flags_missing_permutation_safety_net() {
        let root = temp_repo("missing_perm");
        write_green_repo(&root);
        write(
            &root,
            OFFICE_LAYOUT_REL,
            "pub fn document_layout_from_pdf() {}\nlet e = RoutedEngine::with_default_config();\n",
        );
        let env = doctor_layout_contract_platform(&root);
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("is_valid_permutation")),
            "expected a permutation-safety-net drift warning, got: {:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(&root);
    }
}
