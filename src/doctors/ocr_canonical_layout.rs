//! Doctor: `ocr-canonical-layout`
//!
//! Static invariants for the consolidated OCR layout (May 2026):
//!
//! 1. `example-gateway/models/` is the canonical bundle directory. There is
//!    no nested `oar-ocr/` subdirectory — that legacy layout was
//!    consolidated; if a future sync recreates it the runtime will silently
//!    pick the nested copy via the discovery preference and we'll have two
//!    sources of truth again.
//! 2. The workspace root has a `models -> example-gateway/models` symlink
//!    so `canonical_models_dir()` resolution is unambiguous regardless of
//!    `cwd` (developer workstations, ops snapshots, container working
//!    directories).
//! 3. The architecture doc `docs/architecture/ocr-pipeline.md` exists and
//!    references the current env-var schema. When we add or rename a
//!    knob, the doc must be updated or this doctor fires — the doc and
//!    the code stay in sync by audit pressure.
//! 4. When the gitignored local OCR bundle is present, the five canonical
//!    critical OCR files are present in the gateway models dir at the
//!    workspace root. Production bundles may be at `EXAMPLE_MODELS_DIR`
//!    instead; that case is covered by the runtime `ocr-models-on-disk`
//!    doctor. A clean source checkout is not expected to commit model weights
//!    or a `models` symlink.
//!
//! This doctor is in the `BASELINE_DOCTOR_NAMES` set, so it runs on every
//! `leio-code doctor baseline` and on every `status --strict`.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct OcrCanonicalLayoutDoctor;

impl Doctor for OcrCanonicalLayoutDoctor {
    fn name(&self) -> &'static str {
        "ocr-canonical-layout"
    }

    fn description(&self) -> &'static str {
        "Asserts the consolidated OCR layout: single bundle dir, no legacy \
         oar-ocr/ subdir, workspace symlink, and the architecture doc \
         references the current env-var schema."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        run(root)
    }
}

/// Critical OCR filenames that must exist in the canonical bundle.  These
/// are the *minimum* set the consolidated `accuracy` profile needs to run
/// the full pipeline (PP-OCRv6_small det + rec, paired dict, preferred
/// layout, all three pre-stages).  The runtime `ocr-models-on-disk` doctor
/// covers profile-conditional audits.
const CRITICAL_FILES: &[&str] = &[
    "pp-ocrv6_small_det.onnx",
    "pp-ocrv6_small_rec.onnx",
    "ppocrv6_dict.txt",
    "pp-doclayout_plus-l.onnx",
    "pp-lcnet_x1_0_doc_ori.ort",
    "pp-lcnet_x1_0_textline_ori.ort",
    "uvdoc.ort",
];

/// Env vars the architecture doc must reference.  Adding a new knob to
/// `OcrTuning::from_env()` without updating `docs/architecture/ocr-pipeline.md`
/// fires this doctor.
const DOCUMENTED_ENV_VARS: &[&str] = &[
    "EXAMPLE_OCR_PROFILE",
    "EXAMPLE_MODELS_DIR",
    "EXAMPLE_ENABLE_COREML",
    "EXAMPLE_FORCE_CUDA",
    "EXAMPLE_OCR_AUTO_INVERT",
    "OAR_DET_LONG_SIDE",
    "OAR_DET_MAX_SIDE",
    "OAR_DET_SCORE_THRESHOLD",
    "OAR_DET_BOX_THRESHOLD",
    "OAR_DET_UNCLIP_RATIO",
    "OAR_REC_SCORE_THRESHOLD",
    "OAR_REC_MAX_TEXT_LENGTH",
    "OAR_INTRA_THREADS",
    "OAR_INTER_THREADS",
    "OAR_REGION_BATCH_SIZE",
];

/// Doctor names the doc claims enforce the consolidation.  Keeping this
/// list in the doctor (rather than only in the doc) ensures we can't
/// quietly add a fourth doctor without updating the doc.
const REFERENCED_DOCTORS: &[&str] = &[
    "gateway-ocr-pipeline",
    "ocr-models-on-disk",
    "ocr-canonical-layout",
];

fn run(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings: Vec<String> = Vec::new();
    let mut entities: Vec<serde_json::Value> = Vec::new();
    let mut evidence: Vec<EvidenceItem> = Vec::new();

    let canonical_dir = root.join("example-gateway/models");
    let legacy_subdir = canonical_dir.join("oar-ocr");
    let workspace_symlink = root.join("models");
    let doc_path = root.join("docs/architecture/ocr-pipeline.md");

    // ── 1. Canonical bundle dir ─────────────────────────────────────────
    let canonical_present = canonical_dir.is_dir();
    if !canonical_present {
        evidence.push(EvidenceItem {
            kind: "ocr_canonical_layout".to_string(),
            path: canonical_dir.display().to_string(),
            line: None,
            detail:
                "gitignored local OCR bundle directory absent; runtime model presence is audited by ocr-models-on-disk"
                    .to_string(),
        });
    } else {
        evidence.push(EvidenceItem {
            kind: "ocr_canonical_layout".to_string(),
            path: canonical_dir.display().to_string(),
            line: None,
            detail: "canonical OCR bundle directory present".to_string(),
        });
    }

    // ── 2. No legacy `oar-ocr/` subdir ──────────────────────────────────
    let legacy_subdir_present = legacy_subdir.is_dir();
    let legacy_subdir_has_entries = legacy_subdir
        .read_dir()
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false);
    if legacy_subdir_present && legacy_subdir_has_entries {
        warnings.push(format!(
            "{}: legacy `oar-ocr/` subdirectory has reappeared — the consolidation (May 2026) was undone; runtime will silently prefer the nested copy and the workspace will have two sources of truth again. Move its contents up into the parent and `rmdir` the subdir.",
            legacy_subdir.display()
        ));
    } else if legacy_subdir_present {
        evidence.push(EvidenceItem {
            kind: "ocr_canonical_layout".to_string(),
            path: legacy_subdir.display().to_string(),
            line: None,
            detail: "empty legacy `oar-ocr/` directory present; no runtime OCR assets found there"
                .to_string(),
        });
    }

    // ── 3. Workspace `models` symlink (or dir) ──────────────────────────
    let symlink_present = workspace_symlink.is_symlink() || workspace_symlink.is_dir();
    let symlink_resolves_to_canonical = if workspace_symlink.is_symlink() {
        std::fs::read_link(&workspace_symlink)
            .map(|target| {
                let target_str = target.to_string_lossy().to_string();
                target_str == "example-gateway/models"
                    || target_str.ends_with("/example-gateway/models")
            })
            .unwrap_or(false)
    } else {
        // Not a symlink but a directory — acceptable as long as it's the
        // canonical one (rare case: developer copied the files instead of
        // symlinking).
        workspace_symlink.is_dir()
    };
    if !symlink_present {
        evidence.push(EvidenceItem {
            kind: "ocr_canonical_layout".to_string(),
            path: workspace_symlink.display().to_string(),
            line: None,
            detail:
                "gitignored workspace `models` symlink absent in clean checkout; runtime model presence is audited by ocr-models-on-disk"
                    .to_string(),
        });
    } else if workspace_symlink.is_symlink() && !symlink_resolves_to_canonical {
        warnings.push(format!(
            "{}: workspace `models` symlink does not point at `example-gateway/models` — runtime model resolution will diverge from the doctor's view",
            workspace_symlink.display()
        ));
    } else {
        evidence.push(EvidenceItem {
            kind: "ocr_canonical_layout".to_string(),
            path: workspace_symlink.display().to_string(),
            line: None,
            detail: if workspace_symlink.is_symlink() {
                "workspace `models` symlink resolves to canonical bundle".to_string()
            } else {
                "workspace `models` is a directory (unusual but acceptable)".to_string()
            },
        });
    }

    // ── 4. Critical files in the canonical bundle ───────────────────────
    if canonical_present {
        let mut missing = Vec::new();
        for name in CRITICAL_FILES {
            if !canonical_dir.join(name).is_file() {
                missing.push(*name);
            }
        }
        if missing.is_empty() {
            for name in CRITICAL_FILES {
                evidence.push(EvidenceItem {
                    kind: "ocr_canonical_layout".to_string(),
                    path: canonical_dir.join(name).display().to_string(),
                    line: None,
                    detail: format!("critical OCR asset present: {name}"),
                });
            }
        } else {
            warnings.push(format!(
                "{}: missing {} critical OCR file(s): {:?} — accuracy pipeline cannot run end-to-end",
                canonical_dir.display(),
                missing.len(),
                missing
            ));
        }
    }

    // ── 5. Architecture doc exists and stays in sync with env schema ────
    let doc_src = read_text(&doc_path, &mut warnings);
    let doc_present = doc_src.is_some();

    let mut undocumented_env_vars: Vec<&str> = Vec::new();
    let mut undocumented_doctors: Vec<&str> = Vec::new();
    if let Some(src) = doc_src.as_deref() {
        for var in DOCUMENTED_ENV_VARS {
            if !src.contains(var) {
                undocumented_env_vars.push(*var);
            }
        }
        for doctor in REFERENCED_DOCTORS {
            if !src.contains(doctor) {
                undocumented_doctors.push(*doctor);
            }
        }
        evidence.push(EvidenceItem {
            kind: "ocr_canonical_layout".to_string(),
            path: doc_path.display().to_string(),
            line: None,
            detail: "OCR architecture doc present at the canonical path".to_string(),
        });
    }
    if !doc_present {
        warnings.push(format!(
            "{}: canonical OCR architecture doc is missing — operators have no single reference for env knobs / routing modes / canonical layout",
            doc_path.display()
        ));
    }
    if !undocumented_env_vars.is_empty() {
        warnings.push(format!(
            "{}: doc does not reference env var(s): {:?} — runtime knobs and the documented schema are drifting apart",
            doc_path.display(),
            undocumented_env_vars
        ));
    }
    if !undocumented_doctors.is_empty() {
        warnings.push(format!(
            "{}: doc does not reference enforcement doctor(s): {:?} — operators won't know which `leio-code doctor` to run",
            doc_path.display(),
            undocumented_doctors
        ));
    }

    entities.push(json!({
        "canonical_dir": canonical_dir.display().to_string(),
        "canonical_present": canonical_present,
        "legacy_subdir_present": legacy_subdir_present,
        "legacy_subdir_has_entries": legacy_subdir_has_entries,
        "workspace_symlink_present": symlink_present,
        "workspace_symlink_resolves_to_canonical": symlink_resolves_to_canonical,
        "doc_present": doc_present,
        "undocumented_env_vars": undocumented_env_vars,
        "undocumented_doctors": undocumented_doctors,
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_ocr_canonical_layout"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked OCR canonical layout (one bundle dir, no legacy subdir, workspace symlink, in-sync architecture doc); found {} warnings",
            warnings.len()
        ),
        confidence: if warnings.is_empty() { 0.95 } else { 0.7 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "canonical_dir": canonical_dir.display().to_string(),
            "doc_path": doc_path.display().to_string(),
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}
