//! Doctor: `ocr-models-on-disk`
//!
//! Audits the **actual model bundle** present on disk against the active
//! `EXAMPLE_OCR_PROFILE`.  The static `gateway-ocr-pipeline` doctor proves
//! that every consumer is *capable* of using the right model when it's
//! present; this doctor proves that the *right model is, in fact, present*
//! at the canonical models directory.
//!
//! Catches the "deploy time" failure modes the static doctor cannot:
//!
//! - Accuracy profile + only INT8 quantised recogniser on disk
//!   (silent 1–4 % CER hit).
//! - Accuracy profile + only the Latin mobile recogniser on disk (the
//!   multilingual server FP32 model was never copied to the host).
//! - Recogniser present but the matching dictionary is missing.
//! - Pre-stage models (orientation, textline-orientation, UVDoc) missing.
//! - Layout model is the older `pp-docblocklayout` only, with the better
//!   `pp-doclayout_plus-l` not on disk.
//!
//! The doctor degrades gracefully when no models directory is reachable
//! — it emits a single informational entity (\"no models bundle reachable
//! from this snapshot, skipping disk audit\") and returns zero warnings,
//! so it can run on developer workstations that don't carry the model
//! cache.  It is therefore safe to include in
//! [`super::BASELINE_DOCTOR_NAMES`].

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::query_id;
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct OcrModelsOnDiskDoctor;

impl Doctor for OcrModelsOnDiskDoctor {
    fn name(&self) -> &'static str {
        "ocr-models-on-disk"
    }

    fn description(&self) -> &'static str {
        "Audits the OCR model bundle on disk against EXAMPLE_OCR_PROFILE: \
         FP32 vs INT8, server vs Latin, paired dict, all three pre-stages, \
         and the preferred layout model. Soft-skips when no models dir is reachable."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        run(root)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Profile + filename catalogue
//
// Kept narrow on purpose: this doctor only cares about the recognisers /
// detectors / dictionaries / pre-stages / layouts that appear in the
// workspace's accuracy-tier ordering. The full candidate list lives in
// `example-ocr-models::OcrAsset::candidates()` — the static
// `gateway-ocr-pipeline` doctor pins that surface so the constants below
// can't silently drift away from it.
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Profile {
    Accuracy,
    Balanced,
    Latency,
}

impl Profile {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accuracy => "accuracy",
            Self::Balanced => "balanced",
            Self::Latency => "latency",
        }
    }
}

fn active_profile() -> Profile {
    match std::env::var("EXAMPLE_OCR_PROFILE")
        .ok()
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("latency") | Some("speed") | Some("fast") => Profile::Latency,
        Some("balanced") | Some("default") => Profile::Balanced,
        _ => Profile::Accuracy,
    }
}

/// Recogniser names in **accuracy-first** order.
const RECOGNISERS_ACCURACY_FIRST: &[&str] = &[
    "pp-ocrv6_small_rec.ort",
    "pp-ocrv6_small_rec.onnx",
    "pp-ocrv6_small_rec_int8.ort",
];

const DETECTORS_ACCURACY_FIRST: &[&str] = &[
    "pp-ocrv6_small_det.ort",
    "pp-ocrv6_small_det.onnx",
    "pp-ocrv6_small_det_int8.ort",
];

const LAYOUTS_ACCURACY_FIRST: &[&str] = &[
    "pp-doclayout_plus-l.ort",
    "pp-doclayout_plus-l.onnx",
    "pp-docblocklayout.ort",
    "pp-docblocklayout.onnx",
    "pp-doclayout_plus-l_int8.ort",
    "pp-docblocklayout_int8.ort",
];

const PRESTAGE_DOC_ORIENTATION: &[&str] = &[
    "pp-lcnet_x1_0_doc_ori.ort",
    "pp-lcnet_x1_0_doc_ori.onnx",
    "pp-lcnet_x1_0_doc_ori_int8.ort",
];

const PRESTAGE_TEXTLINE_ORIENTATION: &[&str] = &[
    "pp-lcnet_x1_0_textline_ori.ort",
    "pp-lcnet_x1_0_textline_ori.onnx",
    "pp-lcnet_x0_25_textline_ori.ort",
    "pp-lcnet_x0_25_textline_ori.onnx",
];

const PRESTAGE_RECTIFICATION: &[&str] = &["uvdoc.ort", "uvdoc.onnx", "uvdoc_int8.ort"];

const DICT_MULTILINGUAL: &str = "ppocrv6_dict.txt";
const DICT_LATIN: &str = "ppocrv6_latin_dict.txt";

// ─────────────────────────────────────────────────────────────────────────────
// Candidate models-dir discovery (mirror of example-ocr-models::canonical_models_dir)
// ─────────────────────────────────────────────────────────────────────────────

fn locate_models_dirs(repo_root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();

    if let Ok(env_dir) = std::env::var("EXAMPLE_MODELS_DIR") {
        let p = PathBuf::from(env_dir);
        if p.is_dir() {
            out.push(p);
        }
    }

    // Common production locations on the gateway VM.
    for prod in ["/opt/example/models", "/data/models"] {
        let p = PathBuf::from(prod);
        if p.is_dir() {
            out.push(p);
        }
    }

    // In-repo locations (developer workstation / synced ops snapshot).
    for rel in [
        "example-gateway/models/oar-ocr",
        "example-gateway/models",
        "models",
    ] {
        let p = repo_root.join(rel);
        if p.is_dir() {
            out.push(p);
        }
    }

    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Audit logic
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Resolved {
    name: &'static str,
    path: PathBuf,
}

fn first_present(dir: &Path, candidates: &'static [&'static str]) -> Option<Resolved> {
    for name in candidates {
        let p = dir.join(name);
        if p.is_file() {
            return Some(Resolved { name, path: p });
        }
    }
    None
}

/// Reorder a candidate list to mirror `example-ocr-models::OcrAsset::candidates_for(profile)`:
/// under `latency`, INT8 variants are promoted to the front so the disk
/// doctor's resolution agrees with what the runtime would pick.
fn ordered_for(profile: Profile, candidates: &'static [&'static str]) -> Vec<&'static str> {
    match profile {
        Profile::Accuracy | Profile::Balanced => candidates.to_vec(),
        Profile::Latency => {
            let (int8, fp): (Vec<_>, Vec<_>) = candidates.iter().copied().partition(|n| is_int8(n));
            int8.into_iter().chain(fp).collect()
        }
    }
}

fn first_present_owned(dir: &Path, candidates: &[&'static str]) -> Option<Resolved> {
    for name in candidates {
        let p = dir.join(name);
        if p.is_file() {
            return Some(Resolved { name, path: p });
        }
    }
    None
}

fn classify_recogniser(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower.contains("latin_") {
        "latin"
    } else if lower.contains("pp-ocrv6_small_rec") {
        "multilingual"
    } else {
        "unknown"
    }
}

fn paired_dict(name: &str) -> &'static str {
    match classify_recogniser(name) {
        "multilingual" => DICT_MULTILINGUAL,
        "latin" => DICT_LATIN,
        _ => DICT_MULTILINGUAL,
    }
}

fn is_int8(name: &str) -> bool {
    name.contains("_int8.")
}

fn is_server(name: &str) -> bool {
    !name.contains("latin_")
}

// ─────────────────────────────────────────────────────────────────────────────
// Run
// ─────────────────────────────────────────────────────────────────────────────

fn run(repo_root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings: Vec<String> = Vec::new();
    let mut entities: Vec<serde_json::Value> = Vec::new();
    let mut evidence: Vec<EvidenceItem> = Vec::new();

    let profile = active_profile();
    let dirs = locate_models_dirs(repo_root);

    if dirs.is_empty() {
        // No bundle reachable — degrade gracefully so the doctor stays
        // safe in the baseline preset on dev workstations.
        entities.push(json!({
            "active_profile": profile.as_str(),
            "models_dirs_searched": [
                std::env::var("EXAMPLE_MODELS_DIR").ok(),
                Some("/opt/example/models".to_string()),
                Some("/data/models".to_string()),
                Some(repo_root.join("example-gateway/models/oar-ocr").display().to_string()),
                Some(repo_root.join("example-gateway/models").display().to_string()),
                Some(repo_root.join("models").display().to_string()),
            ],
            "skipped": true,
            "reason": "no models bundle reachable from this snapshot",
        }));
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_ocr_models_on_disk"),
            kind: "doctor".to_string(),
            summary: format!(
                "no OCR models bundle reachable from `{}`; skipping disk audit (active profile = {})",
                repo_root.display(),
                profile.as_str()
            ),
            confidence: 0.95,
            entities,
            evidence: Vec::new(),
            warnings: Vec::new(),
            meta: Some(json!({
                "active_profile": profile.as_str(),
                "skipped": true,
            })),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    // Audit each candidate dir; the *first* dir that resolves all required
    // assets wins, but we report partial bundles for visibility.
    let mut chose: Option<PathBuf> = None;

    for dir in &dirs {
        // Profile-aware ordering so the doctor's resolution mirrors what
        // the runtime would pick under the same `EXAMPLE_OCR_PROFILE`.
        let det = first_present_owned(dir, &ordered_for(profile, DETECTORS_ACCURACY_FIRST));
        let rec = first_present_owned(dir, &ordered_for(profile, RECOGNISERS_ACCURACY_FIRST));
        let layout = first_present_owned(dir, &ordered_for(profile, LAYOUTS_ACCURACY_FIRST));
        // Pre-stages don't have meaningful INT8 vs FP32 trade-offs at this
        // scale; keep the accuracy-first order regardless of profile.
        let doc_ori = first_present(dir, PRESTAGE_DOC_ORIENTATION);
        let textline_ori = first_present(dir, PRESTAGE_TEXTLINE_ORIENTATION);
        let rectify = first_present(dir, PRESTAGE_RECTIFICATION);
        let multi_dict = dir.join(DICT_MULTILINGUAL).is_file();
        let latin_dict = dir.join(DICT_LATIN).is_file();

        // Per-dir entity. We always emit one so audit history is readable.
        entities.push(json!({
            "dir": dir.display().to_string(),
            "active_profile": profile.as_str(),
            "det_resolved": det.as_ref().map(|r| r.name),
            "rec_resolved": rec.as_ref().map(|r| r.name),
            "rec_family": rec.as_ref().map(|r| classify_recogniser(r.name)),
            "rec_paired_dict_present": rec
                .as_ref()
                .map(|r| dir.join(paired_dict(r.name)).is_file()),
            "layout_resolved": layout.as_ref().map(|r| r.name),
            "doc_orientation_present": doc_ori.is_some(),
            "textline_orientation_present": textline_ori.is_some(),
            "rectification_present": rectify.is_some(),
            "multilingual_dict_present": multi_dict,
            "latin_dict_present": latin_dict,
        }));

        // Skip dirs that don't have a recogniser at all — they're unrelated
        // bundles (e.g. gliner-only, embeddings-only).
        let Some(rec) = rec.as_ref() else { continue };

        if chose.is_some() {
            // Already chose a primary dir; don't re-warn for parallel dirs.
            continue;
        }
        chose = Some(dir.clone());

        // ── Hard checks against the chosen primary dir ──────────────────
        if det.is_none() {
            warnings.push(format!(
                "{}: no text-detection model present (looked for {:?})",
                dir.display(),
                DETECTORS_ACCURACY_FIRST,
            ));
        }
        if layout.is_none() {
            warnings.push(format!(
                "{}: no layout model present (looked for {:?})",
                dir.display(),
                LAYOUTS_ACCURACY_FIRST,
            ));
        }

        // Recogniser ↔ dict pairing on disk.
        let needed_dict = paired_dict(rec.name);
        let needed_dict_path = dir.join(needed_dict);
        if !needed_dict_path.is_file() {
            warnings.push(format!(
                "{}: recogniser {} requires dictionary `{}` but it is not on disk — OCR will silently produce gibberish",
                dir.display(),
                rec.name,
                needed_dict,
            ));
        } else {
            evidence.push(EvidenceItem {
                kind: "ocr_models_on_disk".to_string(),
                path: needed_dict_path.display().to_string(),
                line: None,
                detail: format!(
                    "recogniser {} pairs with {} — dictionary on disk",
                    rec.name, needed_dict
                ),
            });
        }

        // ── Profile-conditional checks ─────────────────────────────────
        match profile {
            Profile::Accuracy => {
                if !is_server(rec.name) {
                    warnings.push(format!(
                        "{}: profile=accuracy but only the Latin mobile recogniser ({}) resolved — the multilingual FP32 (`pp-ocrv6_small_rec.{{ort,onnx}}`) is missing; expect ~2–5% lower CER on Latin scripts and complete failure on non-Latin",
                        dir.display(),
                        rec.name,
                    ));
                }
                if is_int8(rec.name) {
                    warnings.push(format!(
                        "{}: profile=accuracy but only the INT8 recogniser ({}) resolved — expect 1–4% CER hit; copy the FP32 sibling onto the host",
                        dir.display(),
                        rec.name,
                    ));
                }
                if let Some(det) = det.as_ref()
                    && is_int8(det.name)
                {
                    warnings.push(format!(
                        "{}: profile=accuracy but only the INT8 detector ({}) resolved",
                        dir.display(),
                        det.name,
                    ));
                }
                if let Some(layout) = layout.as_ref()
                    && !layout.name.contains("pp-doclayout_plus-l")
                {
                    warnings.push(format!(
                            "{}: profile=accuracy but layout resolved to `{}` — `pp-doclayout_plus-l.{{ort,onnx}}` (better on tables and multi-column) is missing",
                            dir.display(),
                            layout.name,
                        ));
                }
                // Pre-stages: under accuracy they are essentially mandatory.
                if doc_ori.is_none() {
                    warnings.push(format!(
                        "{}: profile=accuracy missing doc-orientation pre-stage (`pp-lcnet_x1_0_doc_ori.{{ort,onnx}}`) — rotated scans will silently fail",
                        dir.display()
                    ));
                }
                if textline_ori.is_none() {
                    warnings.push(format!(
                        "{}: profile=accuracy missing textline-orientation pre-stage (`pp-lcnet_x1_0_textline_ori.{{ort,onnx}}`)",
                        dir.display()
                    ));
                }
                if rectify.is_none() {
                    warnings.push(format!(
                        "{}: profile=accuracy missing UVDoc rectification (`uvdoc.{{ort,onnx}}`) — photographed pages won't be unwarped",
                        dir.display()
                    ));
                }
            }
            Profile::Balanced => {
                if let Some(layout) = layout.as_ref()
                    && !layout.name.contains("pp-doclayout_plus-l")
                {
                    warnings.push(format!(
                            "{}: profile=balanced layout resolved to `{}` instead of `pp-doclayout_plus-l`",
                            dir.display(),
                            layout.name,
                        ));
                }
            }
            Profile::Latency => {
                // Under latency we already promoted INT8 in the candidate
                // ordering above, so a resolved FP32 recogniser means INT8
                // is genuinely not on disk.
                if !is_int8(rec.name) {
                    let stem = rec.name.trim_end_matches(".onnx").trim_end_matches(".ort");
                    warnings.push(format!(
                        "{}: profile=latency but no INT8 recogniser on disk; resolved FP32 ({}). Drop `{}_int8.ort` next to it for the latency win",
                        dir.display(),
                        rec.name,
                        stem,
                    ));
                }
            }
        }

        // Always evidence the resolved primary models so the auditor can
        // see what the gateway will actually load on this host.
        evidence.push(EvidenceItem {
            kind: "ocr_models_on_disk".to_string(),
            path: rec.path.display().to_string(),
            line: None,
            detail: format!(
                "active recogniser ({}) family={} int8={}",
                rec.name,
                classify_recogniser(rec.name),
                is_int8(rec.name)
            ),
        });
        if let Some(det) = det.as_ref() {
            evidence.push(EvidenceItem {
                kind: "ocr_models_on_disk".to_string(),
                path: det.path.display().to_string(),
                line: None,
                detail: format!("active detector ({}) int8={}", det.name, is_int8(det.name)),
            });
        }
        if let Some(layout) = layout.as_ref() {
            evidence.push(EvidenceItem {
                kind: "ocr_models_on_disk".to_string(),
                path: layout.path.display().to_string(),
                line: None,
                detail: format!("active layout ({})", layout.name),
            });
        }
        for (label, asset) in [
            ("doc_orientation", doc_ori.as_ref()),
            ("textline_orientation", textline_ori.as_ref()),
            ("rectification", rectify.as_ref()),
        ] {
            if let Some(asset) = asset {
                evidence.push(EvidenceItem {
                    kind: "ocr_models_on_disk".to_string(),
                    path: asset.path.display().to_string(),
                    line: None,
                    detail: format!("{label} pre-stage present ({})", asset.name),
                });
            }
        }
    }

    if chose.is_none() {
        warnings.push(format!(
            "no OCR recogniser resolved in any of the searched model directories ({} candidates)",
            dirs.len(),
        ));
    }

    let summary = if let Some(dir) = chose.as_ref() {
        format!(
            "audited OCR bundle at `{}` against profile=`{}`; {} warnings",
            dir.display(),
            profile.as_str(),
            warnings.len()
        )
    } else {
        format!(
            "no OCR bundle resolved a recogniser in any of {} candidate directories under profile=`{}`",
            dirs.len(),
            profile.as_str()
        )
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_ocr_models_on_disk"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.95 } else { 0.7 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "active_profile": profile.as_str(),
            "models_dirs_searched": dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>(),
            "primary_dir": chose.map(|d| d.display().to_string()),
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}
