//! layout-fast-spectral-contract doctor.
//!
//! Guards the production wiring of `layout-fast-core::RoutedEngine` into
//! the two reading-order paths that consume it:
//!   * the OCR path — `example-gateway::ocr_pipeline::…::reading_order`
//!   * the native-PDF-text path — `pdf-fast-py` `full_text(reading_order)`
//!
//! Shipped via PRs #114 (spectral engine) → #119 (gateway wiring) →
//! PR 7 (`OcrTuning` lift) → #272/#273 (`RoutedEngine`, +6.4pp to 92.3%
//! on olmOCR-bench `multi_column`) → the prod swap that retired the
//! standalone spectral wiring this doctor used to pin.
//!
//! Drift signals this doctor catches:
//!
//!   1. The gateway's `reading_order` sub-module disappeared, was
//!      gutted, or no longer references
//!      `layout_fast_core::RoutedEngine::shared` (or the legacy
//!      `with_default_config`). Without that reference the env flag
//!      still flips a boolean but no ordering happens — silent
//!      regression to emission order.
//!   2. A standalone `SpectralEngine::new` was reintroduced in the
//!      reorder path. That bypasses the recursive arm + the five
//!      geometry overrides and silently drops the page off the 92.3%
//!      routed policy back to the ~85.9% spectral-only baseline.
//!   3. The permutation contract check was removed. The engine could
//!      in principle drop or duplicate region ids; without the
//!      length+seen check the OCR pipeline would silently lose text.
//!   4. The kill-switch field `spectral_reading_order` was renamed or
//!      removed in `example-ocr-models::OcrTuning`. The doctor pins the
//!      field name so a rename triggers a CI signal rather than a
//!      runtime "reorder did nothing" report from production. (σ is now
//!      pinned inside `RoutedEngine`; the deprecated `spectral_sigma_*`
//!      env vars are no longer required and are no longer pinned.)
//!   5. The native-text path (`pdf-fast-py`) lost its `merge_paragraphs`
//!      call, its `RoutedEngine` reference, the `reading_order` PyO3
//!      param, or the Python kwarg.
//!
//! Failure mode: ANY signal → `warn`. All green → silent.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct LayoutFastSpectralContractDoctor;

impl Doctor for LayoutFastSpectralContractDoctor {
    fn name(&self) -> &'static str {
        "layout-fast-spectral-contract"
    }

    fn description(&self) -> &'static str {
        "Guards the RoutedEngine → reading_order wiring in example-gateway and pdf-fast-py: pins the RoutedEngine reference, the kill-switch flag, the arm metric, the permutation contract check, and flags any standalone-SpectralEngine regression that would bypass the 92.3% routed policy."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_layout_fast_spectral_contract(root)
    }
}

const GATEWAY_REL: &str = "example-gateway/src/ocr_pipeline.rs";
const TUNING_REL: &str = "office-parsers-rs/example-ocr-models/src/lib.rs";
const PDF_FAST_PY_RS_REL: &str = "office-parsers-rs/pdf-fast-py/src/lib.rs";
const PDF_FAST_PY_INIT_REL: &str = "office-parsers-rs/pdf-fast-py/pdf_fast/__init__.py";
const PDF_FAST_CORE_PARAGRAPH_REL: &str = "office-parsers-rs/pdf-fast-core/src/paragraph.rs";

// Drift signals on the gateway side.
const READING_ORDER_MODULE_MARKER: &str = "pub mod reading_order";
// Prefer `RoutedEngine::shared()` (process-wide reuse); accept the
// legacy `with_default_config()` clone path so older snapshots stay green.
const ENGINE_MARKER: &str = "layout_fast_core::RoutedEngine::shared";
const ENGINE_MARKER_LEGACY: &str = "layout_fast_core::RoutedEngine::with_default_config";
const TUNING_FLAG_MARKER: &str = "OcrTuning::from_env().spectral_reading_order";
const PERMUTATION_MARKER: &str = "order.len() != regions.len()";
const ARM_METRIC_MARKER: &str = "example_ocr_reading_order_arm_total";
// Anti-regression: the routed swap must not be reverted to a standalone
// `SpectralEngine`. That bypasses the recursive arm + the five geometry
// overrides and drops the page off the 92.3% routed policy back to the
// ~85.9% spectral-only baseline.
const STANDALONE_SPECTRAL_PATTERN: &str = "SpectralEngine::new";

// Drift signal on the OcrTuning side — only the kill-switch flag is
// pinned now. σ is owned by `RoutedEngine::shared()` /
// `with_default_config()`; the `spectral_sigma_x` / `_y` env vars are
// deprecated and the reorder path no longer reads them, so the doctor
// no longer requires them.
const FIELD_FLAG: &str = "spectral_reading_order";

// Drift signals on the native-text path (PR #123 wired
// `parse_pdf_text(reading_order=True)` through pdf-fast-py).
//
// All PyO3-side markers are shaped as call-sites (suffixed with `(`)
// rather than bare paths so doc-comment prose that mentions
// `pdf_fast_core::paragraph::merge_paragraphs` or
// `layout_fast_core::SpectralEngine` doesn't satisfy the check on its
// own. Combined with `extract_full_text_body` scoping the search to
// the function body, this addresses Codex P2 on #124.
const FULL_TEXT_FN_MARKER: &str = "fn full_text";
const COMPUTE_FULL_TEXT_FN_MARKER: &str = "fn compute_full_text";
const COMPUTE_FULL_TEXT_CALL_MARKER: &str = "compute_full_text(";
const PYO3_MERGE_MARKER: &str = "pdf_fast_core::paragraph::merge_paragraphs(";
const PYO3_ENGINE_MARKER: &str = "layout_fast_core::RoutedEngine::shared(";
const PYO3_ENGINE_MARKER_LEGACY: &str = "layout_fast_core::RoutedEngine::with_default_config(";
const PYO3_READING_ORDER_PARAM: &str = "reading_order: bool";
const PY_READING_ORDER_KWARG: &str = "reading_order: bool = False";
const PARAGRAPH_MERGE_FN: &str = "pub fn merge_paragraphs";

pub fn doctor_layout_fast_spectral_contract(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();
    let mut io = Vec::new();

    let gateway_path = root.join(GATEWAY_REL);
    let tuning_path = root.join(TUNING_REL);

    if !gateway_path.is_file() {
        warnings.push(format!(
            "{} not found — cannot verify layout-fast spectral wiring",
            GATEWAY_REL
        ));
        entities.push(json!({
            "doctor": "layout-fast-spectral-contract",
            "skipped": false,
            "reason": format!("{} not found", GATEWAY_REL),
        }));
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_layout_fast_spectral_contract"),
            kind: "doctor".to_string(),
            summary: format!(
                "{} missing; layout-fast spectral contract cannot be verified",
                GATEWAY_REL
            ),
            confidence: 0.5,
            entities,
            evidence,
            warnings,
            meta: None,
            timing_ms: started.elapsed().as_millis(),
        };
    }

    let gateway_body = match read_text(&gateway_path, &mut io) {
        Some(b) => b,
        None => {
            warnings.extend(io);
            return QueryEnvelope {
                schema_version: crate::model::SCHEMA_VERSION.to_string(),
                query_id: query_id("doctor_layout_fast_spectral_contract"),
                kind: "doctor".to_string(),
                summary: format!("could not read {}", GATEWAY_REL),
                confidence: 0.5,
                entities,
                evidence,
                warnings,
                meta: None,
                timing_ms: started.elapsed().as_millis(),
            };
        }
    };
    warnings.extend(std::mem::take(&mut io));

    let Some(reading_order_body) = extract_braced_block(&gateway_body, READING_ORDER_MODULE_MARKER)
    else {
        warnings.push(format!(
            "{}: reading_order module missing — spectral reorder is unwired",
            GATEWAY_REL
        ));
        entities.push(json!({
            "doctor": "layout-fast-spectral-contract",
            "gateway_file": GATEWAY_REL,
            "tuning_file": TUNING_REL,
            "reading_order_module_present": false,
        }));
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_layout_fast_spectral_contract"),
            kind: "doctor".to_string(),
            summary: "reading_order module missing from gateway OCR pipeline".to_string(),
            confidence: 0.6,
            entities,
            evidence,
            warnings,
            meta: None,
            timing_ms: started.elapsed().as_millis(),
        };
    };

    let has_engine = reading_order_body.contains(ENGINE_MARKER)
        || reading_order_body.contains(ENGINE_MARKER_LEGACY);
    let has_flag = reading_order_body.contains(TUNING_FLAG_MARKER);
    let has_permutation_check = reading_order_body.contains(PERMUTATION_MARKER);
    let has_arm_metric = reading_order_body.contains(ARM_METRIC_MARKER);
    let has_standalone_spectral = reading_order_body.contains(STANDALONE_SPECTRAL_PATTERN);

    if !has_engine {
        warnings.push(format!(
            "{}: RoutedEngine construction missing inside reading_order — reorder is dead code",
            GATEWAY_REL
        ));
        evidence.push(EvidenceItem {
            kind: "routed_engine_unwired".to_string(),
            path: GATEWAY_REL.to_string(),
            line: None,
            detail: format!(
                "expected `{}` (or legacy `{}`) inside `reading_order`; without it the env flag flips a boolean and nothing happens",
                ENGINE_MARKER, ENGINE_MARKER_LEGACY
            ),
        });
    }

    if !has_flag {
        warnings.push(format!(
            "{}: kill-switch flag not consulted inside reading_order — EXAMPLE_OCR_SPECTRAL_READING_ORDER can no longer gate the reorder",
            GATEWAY_REL
        ));
        evidence.push(EvidenceItem {
            kind: "kill_switch_unread".to_string(),
            path: GATEWAY_REL.to_string(),
            line: None,
            detail: format!(
                "expected `{}` inside `reading_order` (via `is_enabled()`); it is the operator on/off switch for the routed reorder",
                TUNING_FLAG_MARKER
            ),
        });
    }

    if has_standalone_spectral {
        let line = find_line(&gateway_body, STANDALONE_SPECTRAL_PATTERN);
        warnings.push(format!(
            "{}:{}: standalone `SpectralEngine::new` reintroduced — bypasses the recursive arm + overrides, dropping the page off the 92.3% routed policy",
            GATEWAY_REL,
            line.map(|n| n.to_string())
                .unwrap_or_else(|| "?".to_string())
        ));
        evidence.push(EvidenceItem {
            kind: "standalone_spectral_regression".to_string(),
            path: GATEWAY_REL.to_string(),
            line,
            detail: format!(
                "found `{}`; the reorder must use `RoutedEngine::shared()` (or `with_default_config()`) so the bench-locked routing policy + σ apply",
                STANDALONE_SPECTRAL_PATTERN
            ),
        });
    }

    if !has_arm_metric {
        warnings.push(format!(
            "{}: routing-arm metric `{}` missing from reading_order — ops lose visibility into the arm distribution the 92.3% policy depends on",
            GATEWAY_REL, ARM_METRIC_MARKER
        ));
        evidence.push(EvidenceItem {
            kind: "arm_metric_missing".to_string(),
            path: GATEWAY_REL.to_string(),
            line: None,
            detail: format!(
                "expected `{}` counter inside `reading_order`; a sustained arm shift is the earliest signal the input shape drifted from the bench profile",
                ARM_METRIC_MARKER
            ),
        });
    }

    if !has_permutation_check {
        warnings.push(format!(
            "{}: permutation contract check (`order.len() != regions.len()`) missing — engine could silently drop regions",
            GATEWAY_REL
        ));
        evidence.push(EvidenceItem {
            kind: "permutation_check_missing".to_string(),
            path: GATEWAY_REL.to_string(),
            line: None,
            detail: "the reorder must verify every original region id appears exactly once; otherwise an engine bug erases OCR text".to_string(),
        });
    }

    // OcrTuning field-name pinning. Failures here are typically renames
    // that compile cleanly but break the doctor's drift signal and any
    // operator runbook referencing the env-var names.
    let mut tuning_field_status = json!({});
    if !tuning_path.is_file() {
        warnings.push(format!(
            "{} not found — cannot verify OcrTuning fields",
            TUNING_REL
        ));
    } else {
        let tuning_body = read_text(&tuning_path, &mut io).unwrap_or_default();
        warnings.extend(std::mem::take(&mut io));
        let has_flag = tuning_body.contains(FIELD_FLAG);
        tuning_field_status = json!({
            FIELD_FLAG: has_flag,
        });
        if !has_flag {
            warnings.push(format!(
                "{}: OcrTuning kill-switch field `{}` missing — likely rename or removal",
                TUNING_REL, FIELD_FLAG
            ));
            evidence.push(EvidenceItem {
                kind: "ocr_tuning_field_missing".to_string(),
                path: TUNING_REL.to_string(),
                line: None,
                detail: format!(
                    "expected struct field `{}` in OcrTuning; the gateway reorder gate and operator runbooks depend on this name",
                    FIELD_FLAG
                ),
            });
        }
    }

    // ── Native-text path (PR #123) ───────────────────────────────────
    //
    // The OCR side runs through the gateway above; the native-text
    // side runs through `pdf_fast.parse_pdf_text(reading_order=True)`.
    // Both share `paragraph::merge_paragraphs` + `SpectralEngine`. The
    // checks below pin the four places that wiring lives so the same
    // class of regression we already guard on the gateway also catches
    // gutting / drift on the Python wheel side.
    let core_paragraph_path = root.join(PDF_FAST_CORE_PARAGRAPH_REL);
    let pyo3_path = root.join(PDF_FAST_PY_RS_REL);
    let init_path = root.join(PDF_FAST_PY_INIT_REL);

    let core_has_merge = if core_paragraph_path.is_file() {
        let body = read_text(&core_paragraph_path, &mut io).unwrap_or_default();
        warnings.extend(std::mem::take(&mut io));
        body.contains(PARAGRAPH_MERGE_FN)
    } else {
        false
    };
    let pyo3_has_merge_call;
    let pyo3_has_engine_call;
    let pyo3_has_param;
    if pyo3_path.is_file() {
        let body = read_text(&pyo3_path, &mut io).unwrap_or_default();
        warnings.extend(std::mem::take(&mut io));
        // Param check is scoped to the `fn full_text` signature so a
        // helper or comment that mentions `reading_order: bool` does
        // not mask a PyO3 surface regression. The signature may span
        // multiple lines after rustfmt/PyO3 refactors.
        pyo3_has_param = extract_signature(&body, FULL_TEXT_FN_MARKER)
            .is_some_and(|signature| signature.contains(PYO3_READING_ORDER_PARAM));
        // Merge + engine call-site checks are scoped to the
        // `full_text` function body so the doc comments above the
        // function (which mention both fully-qualified paths in
        // prose) can't satisfy the check on their own. Call-site
        // shape (`merge_paragraphs(` / `SpectralEngine::new(`) is the
        // additional belt-and-braces layer.
        match extract_braced_block(&body, FULL_TEXT_FN_MARKER) {
            Some(fn_body) => {
                let helper_body = if fn_body.contains(COMPUTE_FULL_TEXT_CALL_MARKER) {
                    extract_braced_block(&body, COMPUTE_FULL_TEXT_FN_MARKER)
                } else {
                    None
                };
                pyo3_has_merge_call = fn_body.contains(PYO3_MERGE_MARKER)
                    || helper_body.is_some_and(|body| body.contains(PYO3_MERGE_MARKER));
                pyo3_has_engine_call = fn_body.contains(PYO3_ENGINE_MARKER)
                    || fn_body.contains(PYO3_ENGINE_MARKER_LEGACY)
                    || helper_body.is_some_and(|body| {
                        body.contains(PYO3_ENGINE_MARKER)
                            || body.contains(PYO3_ENGINE_MARKER_LEGACY)
                    });
            }
            None => {
                pyo3_has_merge_call = false;
                pyo3_has_engine_call = false;
            }
        }
    } else {
        pyo3_has_merge_call = false;
        pyo3_has_engine_call = false;
        pyo3_has_param = false;
    }
    let py_has_kwarg = if init_path.is_file() {
        let body = read_text(&init_path, &mut io).unwrap_or_default();
        warnings.extend(std::mem::take(&mut io));
        body.contains(PY_READING_ORDER_KWARG)
    } else {
        false
    };

    if core_paragraph_path.is_file() && !core_has_merge {
        warnings.push(format!(
            "{}: `pub fn merge_paragraphs` missing — the native-text reading-order pipeline (#122/#123) is unwired at the primitive layer",
            PDF_FAST_CORE_PARAGRAPH_REL
        ));
        evidence.push(EvidenceItem {
            kind: "paragraph_merge_primitive_missing".to_string(),
            path: PDF_FAST_CORE_PARAGRAPH_REL.to_string(),
            line: None,
            detail: format!(
                "expected `{}` in pdf-fast-core::paragraph; without it pdf-fast-py can't wire `parse_pdf_text(reading_order=True)`",
                PARAGRAPH_MERGE_FN
            ),
        });
    }
    if pyo3_path.is_file() {
        if !pyo3_has_merge_call {
            warnings.push(format!(
                "{}: `pdf_fast_core::paragraph::merge_paragraphs` not called — reading-order path skipped paragraph coalescing",
                PDF_FAST_PY_RS_REL
            ));
            evidence.push(EvidenceItem {
                kind: "pdf_fast_py_merge_unwired".to_string(),
                path: PDF_FAST_PY_RS_REL.to_string(),
                line: None,
                detail: format!(
                    "expected `{}` reference inside the reading-order branch; without it spectral runs on line-level runs (#118 showed that loses 28 pp accuracy)",
                    PYO3_MERGE_MARKER
                ),
            });
        }
        if !pyo3_has_engine_call {
            warnings.push(format!(
                "{}: `layout_fast_core::RoutedEngine` not referenced — reading-order kwarg is a no-op",
                PDF_FAST_PY_RS_REL
            ));
            evidence.push(EvidenceItem {
                kind: "pdf_fast_py_engine_unwired".to_string(),
                path: PDF_FAST_PY_RS_REL.to_string(),
                line: None,
                detail: format!(
                    "expected `{}` (or legacy `{}`) inside the reading-order branch",
                    PYO3_ENGINE_MARKER, PYO3_ENGINE_MARKER_LEGACY
                ),
            });
        }
        if !pyo3_has_param {
            warnings.push(format!(
                "{}: full_text/scan_pdf_full_text_py lost `reading_order: bool` parameter — Python kwarg can no longer reach Rust",
                PDF_FAST_PY_RS_REL
            ));
            evidence.push(EvidenceItem {
                kind: "pdf_fast_py_param_missing".to_string(),
                path: PDF_FAST_PY_RS_REL.to_string(),
                line: None,
                detail: format!(
                    "expected `{}` on the PyO3 surface; without it the kwarg threads silently",
                    PYO3_READING_ORDER_PARAM
                ),
            });
        }
    }
    if init_path.is_file() && !py_has_kwarg {
        warnings.push(format!(
            "{}: `parse_pdf_text(reading_order: bool = False)` kwarg missing — Python callers cannot opt in",
            PDF_FAST_PY_INIT_REL
        ));
        evidence.push(EvidenceItem {
            kind: "python_kwarg_missing".to_string(),
            path: PDF_FAST_PY_INIT_REL.to_string(),
            line: None,
            detail: format!(
                "expected `{}` on parse_pdf_text; this is the public surface of the native-text reading-order pipeline",
                PY_READING_ORDER_KWARG
            ),
        });
    }

    entities.push(json!({
        "doctor": "layout-fast-spectral-contract",
        "gateway_file": GATEWAY_REL,
        "tuning_file": TUNING_REL,
        "reading_order_module_present": true,
        "routed_engine_reference_present": has_engine,
        "kill_switch_flag_consulted": has_flag,
        "arm_metric_present": has_arm_metric,
        "permutation_check_present": has_permutation_check,
        "standalone_spectral_regression_present": has_standalone_spectral,
        "ocr_tuning_fields": tuning_field_status,
        "pdf_fast_py_native_text_path": {
            "pdf_fast_core_paragraph_module_present": core_has_merge,
            "pyo3_merge_paragraphs_call_present": pyo3_has_merge_call,
            "pyo3_routed_engine_call_present": pyo3_has_engine_call,
            "pyo3_reading_order_param_present": pyo3_has_param,
            "python_reading_order_kwarg_present": py_has_kwarg,
        },
    }));

    let summary = if warnings.is_empty() {
        "RoutedEngine wiring intact: engine referenced, kill-switch + arm metric present, permutation contract enforced, no standalone-spectral regression".to_string()
    } else {
        format!(
            "{} drift signal(s) in the layout-fast routed reading-order wiring",
            warnings.len()
        )
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_layout_fast_spectral_contract"),
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

fn extract_braced_block<'a>(source: &'a str, marker: &str) -> Option<&'a str> {
    let marker_start = source.find(marker)?;
    let after_marker = &source[marker_start..];
    let open_relative = after_marker.find('{')?;
    let open = marker_start + open_relative;
    let mut depth = 0usize;
    let mut body_start = None;

    for (idx, ch) in source[open..].char_indices() {
        let absolute = open + idx;
        match ch {
            '{' => {
                depth += 1;
                if body_start.is_none() {
                    body_start = Some(absolute + ch.len_utf8());
                }
            }
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return body_start.map(|start| &source[start..absolute]);
                }
            }
            _ => {}
        }
    }

    None
}

fn extract_signature<'a>(source: &'a str, marker: &str) -> Option<&'a str> {
    let marker_start = source.find(marker)?;
    let after_marker = &source[marker_start..];
    let open_relative = after_marker.find('{')?;
    Some(&source[marker_start..marker_start + open_relative])
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
            "leio-code-spectral-contract-{label}-{}-{nanos}",
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

    const GATEWAY_HEALTHY: &str = r#"
        pub mod reading_order {
            use example_ocr_models::tuning::OcrTuning;
            static REORDER_ARM = "example_ocr_reading_order_arm_total";
            pub fn is_enabled() -> bool {
                OcrTuning::from_env().spectral_reading_order
            }
            pub fn reorder(...) {
                let engine = layout_fast_core::RoutedEngine::shared();
                count_arm(match engine.route_arm(&blocks) { _ => "spectral" });
                let order = engine.order(&blocks);
                if order.len() != regions.len() {
                    return passthrough;
                }
            }
        }
    "#;

    const GATEWAY_NO_ENGINE: &str = r#"
        pub mod reading_order {
            use example_ocr_models::tuning::OcrTuning;
            static REORDER_ARM = "example_ocr_reading_order_arm_total";
            pub fn is_enabled() -> bool {
                OcrTuning::from_env().spectral_reading_order
            }
            pub fn reorder(r) -> r {
                if order.len() != regions.len() { return r; }
                r
            } // gutted: no engine construction
        }
    "#;

    // Anti-regression fixture: someone reverted the routed swap back to a
    // standalone SpectralEngine, bypassing the recursive arm + overrides.
    const GATEWAY_STANDALONE_SPECTRAL: &str = r#"
        pub mod reading_order {
            use example_ocr_models::tuning::OcrTuning;
            static REORDER_ARM = "example_ocr_reading_order_arm_total";
            pub fn is_enabled() -> bool {
                OcrTuning::from_env().spectral_reading_order
            }
            pub fn reorder() {
                let engine = layout_fast_core::SpectralEngine::new(weights);
                let order = engine.order(&blocks);
                if order.len() != regions.len() { return r; }
            }
        }
    "#;

    const GATEWAY_NO_PERMUTATION_CHECK: &str = r#"
        pub mod reading_order {
            use example_ocr_models::tuning::OcrTuning;
            static REORDER_ARM = "example_ocr_reading_order_arm_total";
            pub fn is_enabled() -> bool {
                OcrTuning::from_env().spectral_reading_order
            }
            pub fn reorder() {
                let engine = layout_fast_core::RoutedEngine::with_default_config();
                let order = engine.order(&blocks);
                // No length check — silent drop possible
            }
        }
    "#;

    const TUNING_HEALTHY: &str = r#"
        pub struct OcrTuning {
            pub spectral_reading_order: bool,
            pub spectral_sigma_x: f32,
            pub spectral_sigma_y: f32,
        }
    "#;

    const TUNING_RENAMED: &str = r#"
        pub struct OcrTuning {
            pub spectral_reorder_enabled: bool,
            pub spectral_x_sigma: f32,
            pub spectral_y_sigma: f32,
        }
    "#;

    // Native-text path (pdf-fast-py + pdf-fast-core) fixtures — added
    // alongside PR #123. The doctor only fires its new checks when
    // these files exist; absence is treated as "test scope didn't
    // cover the native-text path" and stays silent, which is what
    // most pre-existing tests want.

    const CORE_PARAGRAPH_HEALTHY: &str = r#"
        pub fn merge_paragraphs(runs: &[PdfTextRun]) -> Vec<PdfParagraph> {
            // …
        }
    "#;

    const PYO3_HEALTHY: &str = r#"
        fn full_text(&self, reading_order: bool) -> PyResult<(String, FullTextDiagnostics)> {
            let paragraphs = pdf_fast_core::paragraph::merge_paragraphs(&runs);
            let engine = layout_fast_core::RoutedEngine::shared();
            engine.order(&blocks)
        }
    "#;

    const PYO3_HELPER_HEALTHY: &str = r#"
        fn full_text(
            &self,
            py: Python<'_>,
            reading_order: bool,
        ) -> PyResult<(String, FullTextDiagnostics)> {
            let path = self.path.clone();
            py.detach(move || {
                let doc = open_document(&path)?;
                compute_full_text(&doc, reading_order)
            })
        }

        fn compute_full_text(
            doc: &PdfDocument,
            reading_order: bool,
        ) -> PyResult<(String, FullTextDiagnostics)> {
            let paragraphs = pdf_fast_core::paragraph::merge_paragraphs(&runs);
            let engine = layout_fast_core::RoutedEngine::shared();
            engine.order(&blocks)
        }
    "#;

    const PYO3_UNWIRED: &str = r#"
        fn full_text(&self, reading_order: bool) -> PyResult<(String, FullTextDiagnostics)> {
            // kwarg threaded but neither merge nor engine called
            let _ = reading_order;
            Ok((emission_order_text(), diag))
        }
    "#;

    // PR #124 Codex P2 regression: doc-comment prose that mentions
    // both fully-qualified paths must NOT satisfy the check. The
    // doctor now scopes the merge/engine search to inside
    // `fn full_text(...) { ... }` AND requires call-site shape (`(`)
    // so even an in-scope reference like `// see merge_paragraphs`
    // wouldn't fire green.
    const PYO3_DOC_COMMENT_ONLY: &str = r#"
        /// When `reading_order` is true, runs are coalesced via
        /// `pdf_fast_core::paragraph::merge_paragraphs` and re-emitted
        /// in the order chosen by
        /// `layout_fast_core::RoutedEngine::with_default_config()`.
        fn full_text(&self, reading_order: bool) -> PyResult<(String, FullTextDiagnostics)> {
            // function body intentionally gutted — paths only appear
            // in the doc comment above
            let _ = reading_order;
            Ok((emission_order_text(), diag))
        }
    "#;

    const INIT_HEALTHY: &str = r#"
def parse_pdf_text(path, return_diagnostics: bool = False, reading_order: bool = False):
    return scan_pdf_full_text_py(path, reading_order)
    "#;

    const INIT_NO_KWARG: &str = r#"
def parse_pdf_text(path, return_diagnostics: bool = False):
    return scan_pdf_full_text_py(path)
    "#;

    #[test]
    fn native_text_path_healthy_when_all_present() {
        let root = temp_repo("native_ok");
        write(&root, GATEWAY_REL, GATEWAY_HEALTHY);
        write(&root, TUNING_REL, TUNING_HEALTHY);
        write(&root, PDF_FAST_CORE_PARAGRAPH_REL, CORE_PARAGRAPH_HEALTHY);
        write(&root, PDF_FAST_PY_RS_REL, PYO3_HEALTHY);
        write(&root, PDF_FAST_PY_INIT_REL, INIT_HEALTHY);
        let env = doctor_layout_fast_spectral_contract(&root);
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn native_text_path_healthy_when_full_text_delegates_to_helper() {
        let root = temp_repo("native_helper_ok");
        write(&root, GATEWAY_REL, GATEWAY_HEALTHY);
        write(&root, TUNING_REL, TUNING_HEALTHY);
        write(&root, PDF_FAST_CORE_PARAGRAPH_REL, CORE_PARAGRAPH_HEALTHY);
        write(&root, PDF_FAST_PY_RS_REL, PYO3_HELPER_HEALTHY);
        write(&root, PDF_FAST_PY_INIT_REL, INIT_HEALTHY);
        let env = doctor_layout_fast_spectral_contract(&root);
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn native_text_path_pyo3_unwired_flags() {
        let root = temp_repo("native_unwired");
        write(&root, GATEWAY_REL, GATEWAY_HEALTHY);
        write(&root, TUNING_REL, TUNING_HEALTHY);
        write(&root, PDF_FAST_CORE_PARAGRAPH_REL, CORE_PARAGRAPH_HEALTHY);
        write(&root, PDF_FAST_PY_RS_REL, PYO3_UNWIRED);
        write(&root, PDF_FAST_PY_INIT_REL, INIT_HEALTHY);
        let env = doctor_layout_fast_spectral_contract(&root);
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("merge_paragraphs` not called")),
            "expected merge-unwired warning, got: {:?}",
            env.warnings
        );
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("RoutedEngine` not referenced")),
            "expected engine-unwired warning, got: {:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn native_text_path_doc_comment_only_does_not_satisfy_check() {
        // Codex P2 on #124 regression: paths in the doc comment above
        // `fn full_text` must NOT satisfy the merge/engine call
        // checks. The doctor scopes the search to inside the function
        // body and requires call-site shape (`(`) so prose can't
        // mask a gutted body.
        let root = temp_repo("doc_comment_only");
        write(&root, GATEWAY_REL, GATEWAY_HEALTHY);
        write(&root, TUNING_REL, TUNING_HEALTHY);
        write(&root, PDF_FAST_CORE_PARAGRAPH_REL, CORE_PARAGRAPH_HEALTHY);
        write(&root, PDF_FAST_PY_RS_REL, PYO3_DOC_COMMENT_ONLY);
        write(&root, PDF_FAST_PY_INIT_REL, INIT_HEALTHY);
        let env = doctor_layout_fast_spectral_contract(&root);
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("merge_paragraphs` not called")),
            "expected merge-unwired warning when only doc comments name the path: {:?}",
            env.warnings
        );
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("RoutedEngine` not referenced")),
            "expected engine-unwired warning when only doc comments name the path: {:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn native_text_path_python_kwarg_missing_flags() {
        let root = temp_repo("native_no_kwarg");
        write(&root, GATEWAY_REL, GATEWAY_HEALTHY);
        write(&root, TUNING_REL, TUNING_HEALTHY);
        write(&root, PDF_FAST_CORE_PARAGRAPH_REL, CORE_PARAGRAPH_HEALTHY);
        write(&root, PDF_FAST_PY_RS_REL, PYO3_HEALTHY);
        write(&root, PDF_FAST_PY_INIT_REL, INIT_NO_KWARG);
        let env = doctor_layout_fast_spectral_contract(&root);
        assert!(
            env.warnings.iter().any(|w| w.contains("kwarg missing")),
            "expected python-kwarg-missing warning, got: {:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn native_text_path_silent_when_files_absent() {
        // Backward compat: the doctor was authored before PR #123. On
        // forks / older trees without pdf-fast-py wired, the doctor
        // must still report healthy gateway wiring without spamming
        // about absent native-text files.
        let root = temp_repo("native_absent");
        write(&root, GATEWAY_REL, GATEWAY_HEALTHY);
        write(&root, TUNING_REL, TUNING_HEALTHY);
        let env = doctor_layout_fast_spectral_contract(&root);
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn healthy_wiring_is_silent() {
        let root = temp_repo("ok");
        write(&root, GATEWAY_REL, GATEWAY_HEALTHY);
        write(&root, TUNING_REL, TUNING_HEALTHY);
        let env = doctor_layout_fast_spectral_contract(&root);
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_engine_reference_flags() {
        let root = temp_repo("noengine");
        write(&root, GATEWAY_REL, GATEWAY_NO_ENGINE);
        write(&root, TUNING_REL, TUNING_HEALTHY);
        let env = doctor_layout_fast_spectral_contract(&root);
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("RoutedEngine construction missing")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unrelated_tuning_use_outside_reading_order_does_not_mask_unwired_module() {
        let root = temp_repo("outside");
        write(
            &root,
            GATEWAY_REL,
            r#"
                pub mod reading_order {
                    pub fn reorder(r) -> r { r }
                }
                pub mod vl {
                    pub fn route() {
                        let tuning = OcrTuning::from_env();
                        let _ = tuning.spectral_reading_order;
                    }
                }
            "#,
        );
        write(&root, TUNING_REL, TUNING_HEALTHY);
        let env = doctor_layout_fast_spectral_contract(&root);
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("RoutedEngine construction missing")),
            "{:?}",
            env.warnings
        );
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("kill-switch flag not consulted")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn standalone_spectral_engine_is_flagged() {
        let root = temp_repo("standalone");
        write(&root, GATEWAY_REL, GATEWAY_STANDALONE_SPECTRAL);
        write(&root, TUNING_REL, TUNING_HEALTHY);
        let env = doctor_layout_fast_spectral_contract(&root);
        assert!(
            env.warnings.iter().any(|w| w.contains("reintroduced")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_permutation_check_flags() {
        let root = temp_repo("perm");
        write(&root, GATEWAY_REL, GATEWAY_NO_PERMUTATION_CHECK);
        write(&root, TUNING_REL, TUNING_HEALTHY);
        let env = doctor_layout_fast_spectral_contract(&root);
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("permutation contract")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn renamed_tuning_fields_flag() {
        let root = temp_repo("rename");
        write(&root, GATEWAY_REL, GATEWAY_HEALTHY);
        write(&root, TUNING_REL, TUNING_RENAMED);
        let env = doctor_layout_fast_spectral_contract(&root);
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("spectral_reading_order")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_gateway_file_warns() {
        let root = temp_repo("missing");
        let env = doctor_layout_fast_spectral_contract(&root);
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("cannot verify layout-fast spectral wiring")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }
}
