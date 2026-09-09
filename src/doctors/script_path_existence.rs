//! Script path existence doctor.
//!
//! Catches the `deploy_infer.py:MODAL_SCRIPT` class of bug: a script (Python
//! or shell) references a sibling file by a hard-coded relative path, and
//! the referenced file has been renamed/moved/deleted so the reference
//! points at nothing.
//!
//! The 2026-05-27 audit surfaced this exact pattern: `example-api/scripts/
//! deploy_infer.py` had `MODAL_SCRIPT = SCRIPT_DIR / "modal_example_infer_
//! qwen35_27b.py"` while the real file was `modal_example_infer_qwen36_27b
//! .py`. A `cargo build` / `pytest` won't catch this — the reference fires
//! only at deploy time.
//!
//! Scope (intentionally narrow):
//! - Python files under `example-api/scripts/`, `scripts/`, `deploy/`
//! - For each, scan for these path-construction patterns:
//!   - `Path(...) / "..."` (including `SCRIPT_DIR / "..."`)
//!   - `Path(__file__).parent / "..."`
//!   - `subprocess.run([..., "...py", ...])`
//! - Resolve the literal relative to the file's parent dir
//! - Warn if the resolved file does not exist
//!
//! Not in scope:
//! - Dynamic paths (variables, f-strings) — too many false positives
//! - Test/fixture scripts — they intentionally reference missing paths
//! - Anything outside the three deploy/scripts surfaces

use std::path::{Path, PathBuf};
use std::time::Instant;

use ignore::WalkBuilder;
use regex::Regex;
use serde_json::json;

use super::Doctor;
use super::utils::query_id;
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct ScriptPathExistenceDoctor;

impl Doctor for ScriptPathExistenceDoctor {
    fn name(&self) -> &'static str {
        "script-path-existence"
    }

    fn description(&self) -> &'static str {
        "Validates that file-path literals in deploy/scripts Python files reference files that actually exist. Catches the `deploy_infer.py:MODAL_SCRIPT` class of stale-path bug surfaced 2026-05-27."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_script_path_existence(root)
    }
}

/// Surfaces scanned. Repo-relative POSIX prefixes.
const SCAN_PREFIXES: &[&str] = &["example-api/scripts/", "scripts/", "deploy/"];

/// File extensions scanned.
const SCAN_EXTENSIONS: &[&str] = &["py"];

/// Per-file allowlist. Entries are repo-relative POSIX paths whose
/// path-literal references are intentionally stale (e.g. a script that
/// constructs a path for a file the user is supposed to create).
pub const SCRIPT_PATH_EXISTENCE_ALLOWLIST: &[&str] = &[
    // The doctor itself; documentation examples shouldn't fire.
    "leio-code/src/doctors/script_path_existence.rs",
];

/// Skip patterns inside file paths.
const SKIP_PATH_FRAGMENTS: &[&str] = &["/tests/", "/test/", "/__pycache__/", "/fixtures/"];

pub fn doctor_script_path_existence(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();
    let mut files_scanned = 0usize;
    let mut refs_checked = 0usize;

    // Patterns: capture group 1 is the file name literal.
    //
    // We deliberately match ONLY well-known "this-file's-own-directory" names
    // on the LHS of `/`. Matching arbitrary all-caps vars (e.g. `ROOT`,
    // `BASE_DIR`) produces false positives because those vars typically
    // resolve to the workspace root via `.parent.parent`, not the file's
    // directory.
    //
    // Recognized LHS forms (case-sensitive Python idioms):
    //   - `Path(__file__).parent`
    //   - `Path(__file__).resolve().parent`
    //   - `SCRIPT_DIR` (the deploy_infer.py convention)
    //   - `HERE` / `THIS_DIR` (common alternate names)
    //
    // Subprocess pattern only matches `subprocess.X(["python|bash|sh", "name.ext", ...])`
    // which is unambiguous — the second list element is the script to run.
    let patterns: Vec<Regex> = vec![
        Regex::new(r#"(?:Path\(__file__\)(?:\.resolve\(\))?\.parent|SCRIPT_DIR|HERE|THIS_DIR)\s*/\s*"([^"]+\.(?:py|sh|toml|yaml|yml|json|ttl|owl|sql))""#).expect("path-slash regex"),
        Regex::new(r#"subprocess\.(?:run|Popen|call|check_call|check_output)\s*\(\s*\[\s*"(?:python\d?|bash|sh|/usr/bin/env\s+\w+)"\s*,\s*"([^"]+\.(?:py|sh))""#).expect("subprocess-list regex"),
    ];

    for prefix in SCAN_PREFIXES {
        let surface_root = root.join(prefix);
        if !surface_root.exists() {
            continue;
        }
        let walker = WalkBuilder::new(&surface_root)
            .hidden(false)
            .git_ignore(true)
            .git_exclude(true)
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
            if SCRIPT_PATH_EXISTENCE_ALLOWLIST.contains(&rel.as_str()) {
                continue;
            }
            let ext_ok = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| SCAN_EXTENSIONS.contains(&e))
                .unwrap_or(false);
            if !ext_ok {
                continue;
            }

            let contents = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(_) => continue,
            };
            files_scanned += 1;

            let file_parent = match path.parent() {
                Some(p) => p,
                None => continue,
            };

            for (idx, line) in contents.lines().enumerate() {
                let line_num = idx + 1;
                for pattern in &patterns {
                    for caps in pattern.captures_iter(line) {
                        let target = match caps.get(1) {
                            Some(m) => m.as_str(),
                            None => continue,
                        };
                        // Skip targets that look templated, glob-ish, or relative
                        // to something other than the file's own directory.
                        if target.contains('*') || target.contains('{') || target.contains("..") {
                            continue;
                        }
                        refs_checked += 1;
                        let target_path: PathBuf = if target.starts_with('/') {
                            // Absolute path — resolve against root.
                            PathBuf::from(target.trim_start_matches('/'))
                        } else {
                            file_parent.join(target)
                        };
                        if !target_path.exists() {
                            let target_rel = target_path
                                .strip_prefix(root)
                                .map(|p| p.to_string_lossy().replace('\\', "/"))
                                .unwrap_or_else(|_| target_path.display().to_string());
                            warnings.push(format!(
                                "{rel}:{line_num} references `{target}` but resolved path `{target_rel}` does not exist"
                            ));
                            entities.push(json!({
                                "doctor": "script-path-existence",
                                "file": rel,
                                "line": line_num,
                                "literal": target,
                                "resolved": target_rel,
                            }));
                            evidence.push(EvidenceItem {
                                kind: "stale_script_path".to_string(),
                                path: rel.clone(),
                                line: Some(line_num),
                                detail: format!(
                                    "literal `{target}` resolved to non-existent `{target_rel}`"
                                ),
                            });
                        }
                    }
                }
            }
        }
    }

    let summary = format!(
        "script-path-existence: scanned {files_scanned} files across {} surfaces, checked {refs_checked} path literals, found {} stale reference(s)",
        SCAN_PREFIXES.len(),
        warnings.len()
    );

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_script_path_existence"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.95 } else { 0.82 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "surfaces": SCAN_PREFIXES,
            "files_scanned": files_scanned,
            "refs_checked": refs_checked,
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
            "leio-code-script-path-{label}-{}-{nanos}",
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
    fn flags_stale_modal_script_reference() {
        let dir = unique_tempdir("modal-stale");
        write_file(
            &dir.join("example-api/scripts/deploy_infer.py"),
            "from pathlib import Path\nSCRIPT_DIR = Path(__file__).parent\nMODAL_SCRIPT = SCRIPT_DIR / \"modal_example_infer_qwen35_27b.py\"\n",
        );
        // The qwen36 file exists; the qwen35 reference does NOT.
        write_file(
            &dir.join("example-api/scripts/modal_example_infer_qwen36_27b.py"),
            "# placeholder modal entry\n",
        );

        let envelope = doctor_script_path_existence(&dir);
        assert!(
            envelope.warnings.iter().any(|w| w.contains("qwen35")),
            "expected stale qwen35 path to be flagged, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn does_not_flag_existing_path() {
        let dir = unique_tempdir("modal-clean");
        write_file(
            &dir.join("example-api/scripts/deploy_infer.py"),
            "from pathlib import Path\nSCRIPT_DIR = Path(__file__).parent\nMODAL_SCRIPT = SCRIPT_DIR / \"modal_example_infer_qwen36_27b.py\"\n",
        );
        write_file(
            &dir.join("example-api/scripts/modal_example_infer_qwen36_27b.py"),
            "# placeholder modal entry\n",
        );

        let envelope = doctor_script_path_existence(&dir);
        assert!(
            envelope.warnings.is_empty(),
            "expected no warnings, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flags_subprocess_call_to_missing_script() {
        let dir = unique_tempdir("subprocess-stale");
        write_file(
            &dir.join("scripts/runner.py"),
            "import subprocess\nsubprocess.run([\"python\", \"missing_helper.py\"], check=True)\n",
        );
        // Note: NO `scripts/missing_helper.py` written.

        let envelope = doctor_script_path_existence(&dir);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("missing_helper.py")),
            "expected subprocess miss to be flagged, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_paths_with_glob_or_dot_segments() {
        let dir = unique_tempdir("glob-skip");
        write_file(
            &dir.join("scripts/runner.py"),
            "from pathlib import Path\n\
             SCRIPT_DIR = Path(__file__).parent\n\
             GLOB = SCRIPT_DIR / \"some_*.py\"\n\
             PARENT = SCRIPT_DIR / \"../sibling/x.py\"\n",
        );

        let envelope = doctor_script_path_existence(&dir);
        assert!(
            envelope.warnings.is_empty(),
            "glob and `..` references should be skipped, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
