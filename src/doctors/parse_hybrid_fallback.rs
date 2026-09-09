//! parse-hybrid-fallback doctor.
//!
//! On 2026-05-14 the production `/api/parse` handler returned a stub for
//! every inbound PDF because the Python OCR (PaddleOCR via Arrow Flight)
//! was crashing on a paddlepaddle 3.x PIR/OneDNN bug. The gateway has a
//! perfectly healthy classical Rust OCR runtime (Layer A — HybridPipeline
//! over OcrPipelineTrait in `example-gateway/src/ocr_pipeline.rs`), but
//! `/api/parse` does NOT fall back to it. When Python OCR fails, the
//! handler logs `fastpdf/OCR extraction failed for /api/parse; returning
//! stub` and gives up.
//!
//! This doctor asserts that the parse handler either:
//!   1. Calls into `OcrPipelineTrait` / `HybridPipeline` for OCR-eligible
//!      extensions (so a Python failure cascades through the classical
//!      Rust path), OR
//!   2. Has an explicit Rust-side fallback for the same extensions when
//!      the extractor binary path errors out.
//!
//! The current handler does neither — it just returns the stub. Until the
//! refactor lands, this doctor warns on every audit. After the refactor,
//! it stays as a tripwire so the regression cannot recur.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct ParseHybridFallbackDoctor;

impl Doctor for ParseHybridFallbackDoctor {
    fn name(&self) -> &'static str {
        "parse-hybrid-fallback"
    }

    fn description(&self) -> &'static str {
        "Verifies /api/parse falls back to classical Rust OCR (HybridPipeline / OcrPipelineTrait) when the Python OCR delegation errors, instead of returning a stub."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_parse_hybrid_fallback(root)
    }
}

const PARSE_HANDLER_REL: &str = "example-gateway/src/server/handlers/parse.rs";
const STUB_MARKER: &str = "fastpdf/OCR extraction failed for /api/parse; returning stub";
const HYBRID_MARKER: &str = "HybridPipeline";
const TRAIT_MARKER: &str = "OcrPipelineTrait";

pub fn doctor_parse_hybrid_fallback(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();
    let mut io = Vec::new();

    let parse_path = root.join(PARSE_HANDLER_REL);
    if !parse_path.is_file() {
        entities.push(json!({
            "doctor": "parse-hybrid-fallback",
            "skipped": true,
            "reason": format!("{} not found", PARSE_HANDLER_REL),
        }));
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_parse_hybrid_fallback"),
            kind: "doctor".to_string(),
            summary: format!("{} missing; skipped", PARSE_HANDLER_REL),
            confidence: 0.9,
            entities,
            evidence,
            warnings,
            meta: None,
            timing_ms: started.elapsed().as_millis(),
        };
    }

    let body = match read_text(&parse_path, &mut io) {
        Some(b) => b,
        None => {
            warnings.extend(io);
            return QueryEnvelope {
                schema_version: crate::model::SCHEMA_VERSION.to_string(),
                query_id: query_id("doctor_parse_hybrid_fallback"),
                kind: "doctor".to_string(),
                summary: format!("could not read {}", PARSE_HANDLER_REL),
                confidence: 0.5,
                entities,
                evidence,
                warnings,
                meta: None,
                timing_ms: started.elapsed().as_millis(),
            };
        }
    };
    warnings.extend(io);

    let has_stub_path = body.contains(STUB_MARKER);
    let has_hybrid = body.contains(HYBRID_MARKER) || body.contains(TRAIT_MARKER);

    // Drift = the parse handler has a stub-return path AND no reference to
    // the classical OCR pipeline. Either of:
    //   - removing the stub path entirely (handler always returns success or
    //     explicit error from a pipeline), OR
    //   - wiring HybridPipeline/OcrPipelineTrait so the stub becomes a final
    //     fallback after classical OCR has also been tried
    // makes this doctor green.
    if has_stub_path && !has_hybrid {
        let stub_line = find_line(&body, STUB_MARKER);
        warnings.push(format!(
            "{}: /api/parse returns a stub when Python OCR fails and does not fall back to classical Rust OCR (HybridPipeline/OcrPipelineTrait)",
            PARSE_HANDLER_REL
        ));
        evidence.push(EvidenceItem {
            kind: "parse_no_hybrid_fallback".to_string(),
            path: PARSE_HANDLER_REL.to_string(),
            line: stub_line,
            detail: "stub-return path present without OcrPipelineTrait/HybridPipeline reference; Python OCR failures degrade to a no-op for users".to_string(),
        });
    }

    entities.push(json!({
        "doctor": "parse-hybrid-fallback",
        "parse_handler": PARSE_HANDLER_REL,
        "has_stub_path": has_stub_path,
        "has_hybrid_or_trait_reference": has_hybrid,
    }));

    let summary = if warnings.is_empty() {
        format!(
            "{} either has no stub-return path or references the classical OCR pipeline",
            PARSE_HANDLER_REL
        )
    } else {
        "/api/parse falls back to a stub on Python OCR failure; classical Rust pipeline is not consulted".to_string()
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_parse_hybrid_fallback"),
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
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_repo(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-parse-hybrid-{label}-{}-{nanos}",
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

    const HANDLER_NO_FALLBACK: &str = r#"
        // POST /api/parse
        match runner.run(&bytes).await {
            Ok(payload) => return Ok(payload),
            Err(err) => {
                tracing::warn!("fastpdf/OCR extraction failed for /api/parse; returning stub");
            }
        }
        return stub_response();
    "#;

    const HANDLER_WITH_HYBRID: &str = r#"
        // POST /api/parse
        match runner.run(&bytes).await {
            Ok(payload) => return Ok(payload),
            Err(err) => {
                tracing::warn!("fastpdf/OCR extraction failed for /api/parse; returning stub");
                // Fall through to classical Rust OCR
                let pipeline: Box<dyn OcrPipelineTrait> = HybridPipeline::new(...);
                return pipeline.process_file(&bytes).await;
            }
        }
    "#;

    const HANDLER_NO_STUB: &str = r#"
        // POST /api/parse — always returns success or error from the runner
        runner.run(&bytes).await
    "#;

    #[test]
    fn no_fallback_is_flagged() {
        let root = temp_repo("nofb");
        write(&root, PARSE_HANDLER_REL, HANDLER_NO_FALLBACK);
        let env = doctor_parse_hybrid_fallback(&root);
        assert_eq!(env.warnings.len(), 1, "{:?}", env.warnings);
        assert!(env.warnings[0].contains("classical Rust OCR"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn hybrid_fallback_is_silent() {
        let root = temp_repo("hybrid");
        write(&root, PARSE_HANDLER_REL, HANDLER_WITH_HYBRID);
        let env = doctor_parse_hybrid_fallback(&root);
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn no_stub_path_is_silent() {
        let root = temp_repo("nostub");
        write(&root, PARSE_HANDLER_REL, HANDLER_NO_STUB);
        let env = doctor_parse_hybrid_fallback(&root);
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_handler_skipped_silently() {
        let root = temp_repo("missing");
        let env = doctor_parse_hybrid_fallback(&root);
        assert!(env.warnings.is_empty());
        let _ = fs::remove_dir_all(root);
    }
}
