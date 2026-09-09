//! Modal app naming coherence doctor.
//!
//! Validates that Modal app names referenced in config (`*.env*`, `deploy/`,
//! `CLAUDE.md`, agent YAML) correspond to actual `modal_*.py` scripts that
//! exist in `example-api/scripts/`.
//!
//! Surfaced by the 2026-05-27 audit: `vigoros/CLAUDE.md` had
//! `EXAMPLE_VLLM_BASE_URL=https://josaum--example-infer-qwen35-27b-serve.modal.run`
//! pointing at a Modal app `example-infer-qwen35-27b` that doesn't exist
//! (real app is `example-infer-qwen36-27b`, served by
//! `modal_example_infer_qwen36_27b.py`). Same drift hit `example-api/scripts/
//! deploy_infer.py:MODAL_SCRIPT` — caught by the sibling `script-path-
//! existence` doctor.
//!
//! Mapping rule:
//!   Modal URL fragment       `example-infer-qwen36-27b`
//!   → required script file   `example-api/scripts/modal_example_infer_qwen36_27b.py`
//!   (hyphens → underscores, `modal_` prefix, `.py` suffix)
//!
//! Scope:
//! - `.env*`, `.env.*.example` files anywhere
//! - `deploy/**/*.toml` and `deploy/**/*.yaml`
//! - All `CLAUDE.md` files
//!
//! Not in scope:
//! - Runtime-discovered Modal apps (Flight tunnels) — those legitimately
//!   change per container.
//! - Apps for non-example projects (the regex is `example-infer-*-*`-anchored).

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use ignore::WalkBuilder;
use regex::Regex;
use serde_json::json;

use super::Doctor;
use super::utils::query_id;
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct ModalAppNamingCoherenceDoctor;

impl Doctor for ModalAppNamingCoherenceDoctor {
    fn name(&self) -> &'static str {
        "modal-app-naming-coherence"
    }

    fn description(&self) -> &'static str {
        "Cross-checks `example-infer-*` Modal app names referenced in config / docs against existing `example-api/scripts/modal_example_infer_*.py` scripts. Catches the qwen35→qwen36 URL drift class surfaced 2026-05-27."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_modal_app_naming_coherence(root)
    }
}

/// Per-file allowlist: files that intentionally mention Modal app names
/// that may not exist (historical docs, deprecation notes, etc.).
pub const MODAL_APP_NAMING_ALLOWLIST: &[&str] = &[
    // The doctor itself.
    "leio-code/src/doctors/modal_app_naming_coherence.rs",
];

/// File patterns scanned.
const SCAN_FILENAMES_EXACT: &[&str] = &["CLAUDE.md"];
const SCAN_FILENAME_PREFIXES: &[&str] = &[".env"];
const SCAN_DIRS_WITH_EXTS: &[(&str, &[&str])] = &[("deploy/", &["toml", "yaml", "yml"])];

const SKIP_PATH_FRAGMENTS: &[&str] = &[
    "/node_modules/",
    "/target/",
    "/dist/",
    "/.next/",
    "/.next-ops/",
    "/__pycache__/",
];

pub fn doctor_modal_app_naming_coherence(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();
    let mut files_scanned = 0usize;
    let mut refs_checked = 0usize;

    // 1. Enumerate existing modal_*.py files in example-api/scripts/.
    let scripts_dir = root.join("example-api/scripts");
    let mut existing_apps: BTreeSet<String> = BTreeSet::new();
    if scripts_dir.exists()
        && let Ok(entries) = std::fs::read_dir(&scripts_dir)
    {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if let Some(rest) = name_str.strip_prefix("modal_")
                && let Some(stem) = rest.strip_suffix(".py")
            {
                // `modal_example_infer_qwen36_27b.py` → `example-infer-qwen36-27b`
                let app_name = stem.replace('_', "-");
                existing_apps.insert(app_name);
            }
        }
    }

    // 2. Regex for Modal app name references.
    //
    // Matches: `example-infer-<flavor>-<size>` optionally followed by
    // `-serve` / `-web` / `-batch` (Modal serve mode suffix), with surrounding
    // word boundaries.
    //
    // Examples that match (capture is the bare app name without serve suffix):
    //   `josaum--example-infer-qwen36-27b-serve.modal.run` → `example-infer-qwen36-27b`
    //   `example-infer-qwen35-27b`                          → `example-infer-qwen35-27b`
    let pattern = Regex::new(r"\b(example-infer-[a-z0-9]+-[a-z0-9]+)(?:-(?:serve|web|batch))?\b")
        .expect("modal app regex");

    // 3. Walk scoped files and check each match.
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .build();

    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        if SKIP_PATH_FRAGMENTS.iter().any(|f| rel.contains(f)) {
            continue;
        }
        if MODAL_APP_NAMING_ALLOWLIST.contains(&rel.as_str()) {
            continue;
        }

        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();

        let matches_filename = SCAN_FILENAMES_EXACT.contains(&file_name)
            || SCAN_FILENAME_PREFIXES
                .iter()
                .any(|p| file_name.starts_with(p))
            // Also match dotted env files like `secrets.env.example`,
            // `customer_ops_unified.env.local`, etc. The `.env` segment
            // is the load-bearing token.
            || file_name.contains(".env");
        let matches_dir_ext = SCAN_DIRS_WITH_EXTS
            .iter()
            .any(|(dir, exts)| rel.starts_with(dir) && exts.contains(&ext));

        if !matches_filename && !matches_dir_ext {
            continue;
        }

        let contents = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        files_scanned += 1;

        // Track refs already warned within this file (dedupe).
        let mut seen_in_file: BTreeSet<String> = BTreeSet::new();

        for (idx, line) in contents.lines().enumerate() {
            let line_num = idx + 1;
            for caps in pattern.captures_iter(line) {
                let app = caps.get(1).map(|m| m.as_str().to_string());
                let Some(app) = app else { continue };
                refs_checked += 1;
                if existing_apps.contains(&app) {
                    continue;
                }
                let key = format!("{rel}::{app}");
                if !seen_in_file.insert(key) {
                    continue;
                }
                let expected_script =
                    format!("example-api/scripts/modal_{}.py", app.replace('-', "_"));
                warnings.push(format!(
                    "{rel}:{line_num} references Modal app `{app}` but no `{expected_script}` exists"
                ));
                entities.push(json!({
                    "doctor": "modal-app-naming-coherence",
                    "file": rel,
                    "line": line_num,
                    "app_name": app,
                    "expected_script": expected_script,
                    "existing_apps": existing_apps.iter().collect::<Vec<_>>(),
                }));
                evidence.push(EvidenceItem {
                    kind: "stale_modal_app".to_string(),
                    path: rel.clone(),
                    line: Some(line_num),
                    detail: format!(
                        "no `modal_{}.py` script in example-api/scripts/",
                        app.replace('-', "_")
                    ),
                });
            }
        }
    }

    let summary = format!(
        "modal-app-naming-coherence: scanned {files_scanned} files, found {} known Modal app(s), checked {refs_checked} reference(s), {} drifted",
        existing_apps.len(),
        warnings.len()
    );

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_modal_app_naming_coherence"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.95 } else { 0.82 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "files_scanned": files_scanned,
            "refs_checked": refs_checked,
            "existing_apps": existing_apps.iter().collect::<Vec<_>>(),
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_tempdir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-modal-naming-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(path, contents).expect("write fixture");
    }

    #[test]
    fn flags_stale_qwen_app_in_claudemd() {
        let dir = unique_tempdir("vigoros-qwen-stale");
        // Real script exists for qwen36 only.
        write_file(
            &dir.join("example-api/scripts/modal_example_infer_qwen36_27b.py"),
            "# modal app\n",
        );
        // vigoros doc references the stale qwen35 app.
        write_file(
            &dir.join("vigoros/CLAUDE.md"),
            "EXAMPLE_VLLM_BASE_URL=https://josaum--example-infer-qwen35-27b-serve.modal.run/v1\n",
        );

        let envelope = doctor_modal_app_naming_coherence(&dir);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("qwen35-27b") && w.contains("vigoros/CLAUDE.md")),
            "expected qwen35 drift to be flagged, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn does_not_flag_existing_app() {
        let dir = unique_tempdir("vigoros-qwen-clean");
        write_file(
            &dir.join("example-api/scripts/modal_example_infer_qwen36_27b.py"),
            "# modal app\n",
        );
        write_file(
            &dir.join("vigoros/CLAUDE.md"),
            "EXAMPLE_VLLM_BASE_URL=https://josaum--example-infer-qwen36-27b-serve.modal.run/v1\n",
        );

        let envelope = doctor_modal_app_naming_coherence(&dir);
        assert!(
            envelope.warnings.is_empty(),
            "expected no warnings for matching qwen36 app, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dedupes_multiple_refs_within_same_file() {
        let dir = unique_tempdir("vigoros-dedupe");
        write_file(
            &dir.join("example-api/scripts/modal_example_infer_qwen36_27b.py"),
            "# modal app\n",
        );
        // Same stale app referenced 3 times in one file.
        write_file(
            &dir.join("vigoros/CLAUDE.md"),
            "URL=https://josaum--example-infer-qwen35-27b-serve.modal.run/v1\n\
             FLIGHT=https://josaum--example-infer-qwen35-27b-serve.modal.run\n\
             HTTP=https://josaum--example-infer-qwen35-27b-serve.modal.run\n",
        );

        let envelope = doctor_modal_app_naming_coherence(&dir);
        let dup_count = envelope
            .warnings
            .iter()
            .filter(|w| w.contains("qwen35-27b"))
            .count();
        assert_eq!(
            dup_count, 1,
            "expected exactly one warning per (file, app), got {dup_count}: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scans_deploy_toml_and_env_examples() {
        let dir = unique_tempdir("multi-surface");
        write_file(
            &dir.join("example-api/scripts/modal_example_infer_qwen36_27b.py"),
            "# modal app\n",
        );
        write_file(
            &dir.join("deploy/secrets.env.example"),
            "EXAMPLE_INFER_HTTP_URL=https://josaum--example-infer-qwen35-27b-serve.modal.run\n",
        );
        write_file(
            &dir.join("deploy/targets/vigoros.toml"),
            "modal_url = \"josaum--example-infer-qwen35-27b-serve.modal.run\"\n",
        );

        let envelope = doctor_modal_app_naming_coherence(&dir);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("secrets.env.example")),
            ".env.example must be scanned: {:?}",
            envelope.warnings
        );
        assert!(
            envelope.warnings.iter().any(|w| w.contains("vigoros.toml")),
            "deploy/*.toml must be scanned: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
