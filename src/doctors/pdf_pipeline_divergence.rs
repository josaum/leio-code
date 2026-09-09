//! pdf-pipeline-divergence doctor.
//!
//! The gateway has historically grown TWO independent PDF code paths:
//!
//!   1. `example-gateway/src/server/handlers/parse.rs` (the canonical
//!      `/api/parse` HTTP handler) — delegates to the Python extractor over
//!      Arrow Flight (fastpdf/Chandra → pdf-fast underneath) and, since
//!      2026-05-14, falls back to the classical Rust `OcrPipelineTrait` /
//!      `HybridPipeline` on Python OCR failure. This path is *wired* and
//!      handles every inbound PDF in production. The
//!      [`parse-hybrid-fallback`] doctor pins its fallback shape.
//!
//!   2. `example-gateway/src/pdf_handler.rs` — a separate `pdfium_render`-
//!      backed extractor with its own `extract_text_from_pdf` /
//!      `process_pdf_for_schema` surface. The module is `pub mod`-declared in
//!      `src/lib.rs:132` and documented in `example-gateway/CLAUDE.md`, but
//!      nothing inside the gateway crate actually calls it. The file is
//!      feature-gated on `pdfium`, which (per its own comment) "isn't in
//!      Cargo.toml features".
//!
//! Two paths, one of which is unreachable, is exactly the kind of slow drift
//! LEIO Code is meant to catch:
//!
//!   - If `pdf_handler.rs` is dead, it confuses contributors who arrive via
//!     CLAUDE.md or TODO.md, claims a `pdfium` feature that doesn't exist,
//!     and risks being re-imported as the "obvious" PDF handler — diverging
//!     from the Flight + classical-OCR shape `parse.rs` codifies.
//!   - If `pdf_handler.rs` is intentionally there for a future wiring, that
//!     intent should live next to the module declaration, not in TODO.md.
//!
//! This doctor asserts one of the following holds:
//!
//!   a) `pdf_handler.rs` is removed.
//!   b) `pdf_handler.rs` exists AND is reachable from at least one
//!      caller inside `example-gateway/src/` (excluding the file itself).
//!   c) `pdf_handler.rs` exists but the `pub mod pdf_handler;` declaration
//!      in `example-gateway/src/lib.rs` is feature-gated, gating the dead
//!      code out of the default build.
//!
//! Anything else is divergence-shaped drift and warns. The doctor stays in
//! the registry as a tripwire after the drift is resolved so the same class
//! of orphan-pipeline cannot recur silently.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct PdfPipelineDivergenceDoctor;

impl Doctor for PdfPipelineDivergenceDoctor {
    fn name(&self) -> &'static str {
        "pdf-pipeline-divergence"
    }

    fn description(&self) -> &'static str {
        "Verifies the gateway has exactly one active PDF pipeline: either pdf_handler.rs is removed/feature-gated, or it has a caller inside the gateway crate. Catches orphan pdfium-render path drifting from the canonical /api/parse → Flight + OcrPipelineTrait shape."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_pdf_pipeline_divergence(root)
    }
}

const PDF_HANDLER_REL: &str = "example-gateway/src/pdf_handler.rs";
const GATEWAY_LIB_REL: &str = "example-gateway/src/lib.rs";
const GATEWAY_SRC_REL: &str = "example-gateway/src";
const PARSE_HANDLER_REL: &str = "example-gateway/src/server/handlers/parse.rs";

pub fn doctor_pdf_pipeline_divergence(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();
    let mut io = Vec::new();

    let pdf_handler_path = root.join(PDF_HANDLER_REL);
    let pdf_handler_present = pdf_handler_path.is_file();
    let parse_handler_present = root.join(PARSE_HANDLER_REL).is_file();

    // Case (a): pdf_handler.rs is gone. Nothing to police.
    if !pdf_handler_present {
        entities.push(json!({
            "doctor": "pdf-pipeline-divergence",
            "pdf_handler_present": false,
            "parse_handler_present": parse_handler_present,
        }));
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_pdf_pipeline_divergence"),
            kind: "doctor".to_string(),
            summary: format!(
                "{} not present; only /api/parse path active",
                PDF_HANDLER_REL
            ),
            confidence: 0.95,
            entities,
            evidence,
            warnings,
            meta: None,
            timing_ms: started.elapsed().as_millis(),
        };
    }

    // Check whether `pub mod pdf_handler;` is feature-gated in lib.rs.
    let lib_path = root.join(GATEWAY_LIB_REL);
    let lib_body = read_text(&lib_path, &mut io).unwrap_or_default();
    warnings.append(&mut io);
    let mod_declaration_gated = lib_body.lines().any(|line| {
        // `#[cfg(...)] pub mod pdf_handler;` or a preceding `#[cfg(...)]`
        // line above the declaration. Cheap two-line check.
        line.contains("pub mod pdf_handler") && line.contains("#[cfg")
    }) || {
        // Two-line variant: cfg attribute on its own line above the decl.
        let lines: Vec<&str> = lib_body.lines().collect();
        lines
            .windows(2)
            .any(|w| w[0].trim_start().starts_with("#[cfg") && w[1].contains("pub mod pdf_handler"))
    };

    let mod_declared =
        lib_body.contains("pub mod pdf_handler") || lib_body.contains("mod pdf_handler");

    // Count callers within the gateway crate, excluding the orphan file
    // itself. We look for `pdf_handler::` symbol-path references or the
    // bare imported function names `extract_text_from_pdf` /
    // `process_pdf_for_schema` in any file under example-gateway/src/.
    let src_dir = root.join(GATEWAY_SRC_REL);
    let mut caller_paths: Vec<String> = Vec::new();
    walk_rust_files(&src_dir, &mut |file_path| {
        // Skip the orphan file itself.
        if file_path == pdf_handler_path {
            return;
        }
        let body = match std::fs::read_to_string(file_path) {
            Ok(s) => s,
            Err(_) => return,
        };
        if (body.contains("pdf_handler::")
            || body.contains("use crate::pdf_handler")
            || body.contains("use super::pdf_handler"))
            && let Ok(rel) = file_path.strip_prefix(root)
        {
            caller_paths.push(rel.display().to_string());
        }
    });

    // Case (c): module declaration is feature-gated. Default build does
    // not pull in the dead code. That's a defensible state — the
    // pdfium path is opt-in for the rare builder who wants it.
    if mod_declared && mod_declaration_gated && caller_paths.is_empty() {
        evidence.push(EvidenceItem {
            kind: "pdf_handler_feature_gated".to_string(),
            path: GATEWAY_LIB_REL.to_string(),
            line: None,
            detail: "pub mod pdf_handler is feature-gated; dead code excluded from default build"
                .to_string(),
        });
        entities.push(json!({
            "doctor": "pdf-pipeline-divergence",
            "pdf_handler_present": true,
            "mod_declared": true,
            "mod_declaration_gated": true,
            "caller_count": 0,
        }));
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_pdf_pipeline_divergence"),
            kind: "doctor".to_string(),
            summary: format!(
                "{} is present but its module declaration is feature-gated",
                PDF_HANDLER_REL
            ),
            confidence: 0.85,
            entities,
            evidence,
            warnings,
            meta: None,
            timing_ms: started.elapsed().as_millis(),
        };
    }

    // Case (b): there's at least one caller. The path is alive. Whether
    // having two pipelines is desirable is a separate architectural call;
    // this doctor only flags the *silent orphan* shape, not "two callers
    // is too many". (Add a stricter doctor if needed.)
    if !caller_paths.is_empty() {
        for caller in &caller_paths {
            evidence.push(EvidenceItem {
                kind: "pdf_handler_caller".to_string(),
                path: caller.clone(),
                line: None,
                detail: "references pdf_handler::* — orphan check satisfied".to_string(),
            });
        }
        entities.push(json!({
            "doctor": "pdf-pipeline-divergence",
            "pdf_handler_present": true,
            "mod_declared": mod_declared,
            "caller_count": caller_paths.len(),
            "callers": caller_paths,
        }));
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_pdf_pipeline_divergence"),
            kind: "doctor".to_string(),
            summary: format!(
                "{} has {} caller(s) inside the gateway crate",
                PDF_HANDLER_REL,
                caller_paths.len()
            ),
            confidence: 0.9,
            entities,
            evidence,
            warnings,
            meta: None,
            timing_ms: started.elapsed().as_millis(),
        };
    }

    // Drift: module is declared (not feature-gated) but has no callers in
    // the same crate. This is the current 2026-05-18 state of the
    // workspace.
    warnings.push(format!(
        "{} is declared (`pub mod pdf_handler;`) in {} but has 0 callers inside example-gateway/src/; the canonical PDF path lives in {}",
        PDF_HANDLER_REL, GATEWAY_LIB_REL, PARSE_HANDLER_REL
    ));
    evidence.push(EvidenceItem {
        kind: "pdf_handler_orphan".to_string(),
        path: PDF_HANDLER_REL.to_string(),
        line: None,
        detail: format!(
            "pub fns `extract_text_from_pdf` and `process_pdf_for_schema` have no callers inside the gateway crate; module is exposed via `pub mod pdf_handler;` in {} without a feature gate",
            GATEWAY_LIB_REL
        ),
    });
    if mod_declared {
        evidence.push(EvidenceItem {
            kind: "pdf_handler_declaration".to_string(),
            path: GATEWAY_LIB_REL.to_string(),
            line: None,
            detail: "`pub mod pdf_handler;` exposes the orphan to downstream consumers".to_string(),
        });
    }
    entities.push(json!({
        "doctor": "pdf-pipeline-divergence",
        "pdf_handler_present": true,
        "mod_declared": mod_declared,
        "mod_declaration_gated": mod_declaration_gated,
        "caller_count": 0,
        "remediation_options": [
            format!("remove {} (and the pub mod line) — canonical /api/parse path covers all PDF traffic", PDF_HANDLER_REL),
            format!("feature-gate the `pub mod pdf_handler;` declaration in {} on `pdfium`", GATEWAY_LIB_REL),
            "wire pdf_handler::* into an actual handler (router or service) — and surface why it diverges from parse.rs".to_string(),
        ],
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_pdf_pipeline_divergence"),
        kind: "doctor".to_string(),
        summary: format!(
            "{} declared without callers — orphan pipeline divergent from {}",
            PDF_HANDLER_REL, PARSE_HANDLER_REL
        ),
        confidence: 0.7,
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn walk_rust_files(dir: &Path, visit: &mut dyn FnMut(&Path)) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_rust_files(&path, visit);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            visit(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_repo(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-pdf-divergence-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn green_when_pdf_handler_missing() {
        let root = temp_repo("a");
        write(&root, "example-gateway/src/lib.rs", "pub mod app;\n");
        write(
            &root,
            "example-gateway/src/server/handlers/parse.rs",
            "// parse handler\n",
        );
        let env = doctor_pdf_pipeline_divergence(&root);
        assert!(env.warnings.is_empty(), "warnings: {:?}", env.warnings);
        assert!(env.summary.contains("not present"));
    }

    #[test]
    fn warns_when_pdf_handler_orphan_and_declared_ungated() {
        let root = temp_repo("b");
        write(
            &root,
            "example-gateway/src/lib.rs",
            "pub mod app;\npub mod pdf_handler;\n",
        );
        write(
            &root,
            "example-gateway/src/pdf_handler.rs",
            "pub fn extract_text_from_pdf() {}\n",
        );
        write(
            &root,
            "example-gateway/src/server/handlers/parse.rs",
            "// parse handler does not use pdf_handler\n",
        );
        let env = doctor_pdf_pipeline_divergence(&root);
        assert_eq!(env.warnings.len(), 1, "expected 1 warning");
        assert!(env.summary.contains("orphan"));
    }

    #[test]
    fn green_when_pdf_handler_has_caller_in_gateway_src() {
        let root = temp_repo("c");
        write(
            &root,
            "example-gateway/src/lib.rs",
            "pub mod pdf_handler;\npub mod some_route;\n",
        );
        write(
            &root,
            "example-gateway/src/pdf_handler.rs",
            "pub fn extract_text_from_pdf() {}\n",
        );
        // A caller that imports the symbol.
        write(
            &root,
            "example-gateway/src/some_route.rs",
            "use crate::pdf_handler::extract_text_from_pdf;\n",
        );
        let env = doctor_pdf_pipeline_divergence(&root);
        assert!(env.warnings.is_empty(), "warnings: {:?}", env.warnings);
        assert!(env.summary.contains("caller"));
    }

    #[test]
    fn green_when_declaration_feature_gated() {
        let root = temp_repo("d");
        // Feature-gated declaration: the dead code is excluded from the
        // default build, so even with 0 callers we accept the state.
        write(
            &root,
            "example-gateway/src/lib.rs",
            "#[cfg(feature = \"pdfium\")]\npub mod pdf_handler;\n",
        );
        write(
            &root,
            "example-gateway/src/pdf_handler.rs",
            "pub fn extract_text_from_pdf() {}\n",
        );
        let env = doctor_pdf_pipeline_divergence(&root);
        assert!(env.warnings.is_empty(), "warnings: {:?}", env.warnings);
        assert!(env.summary.contains("feature-gated"));
    }
}
