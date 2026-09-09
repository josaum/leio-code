//! Manus residue doctor.
//!
//! Locks in the architectural deannexation from the `manus.*` vendor stack.
//!
//! Per the workspace non-negotiable invariants ("Outbound messaging terminates in Rust",
//! "the LLM navigates the graph; it does not invent the workflow"), this workspace must
//! not depend on any `manus.space`, `manus.im`, or `forge.manus.*` surface.
//! Without a structural check, a future agent or developer could re-introduce a manus
//! dependency unnoticed. As a doctor, the violation surfaces on every
//! `verify` / `doctor manus-residue` run, with hard-error severity.

use std::path::{Path, PathBuf};
use std::time::Instant;

use ignore::WalkBuilder;
use regex::Regex;
use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct ManusResidueDoctor;

impl Doctor for ManusResidueDoctor {
    fn name(&self) -> &'static str {
        "manus-residue"
    }

    fn description(&self) -> &'static str {
        "Locks in workspace deannexation from the manus.* vendor: bans `manus.space`, `manus.im`, and `forge.manus.*` references in source files outside an explicit, in-source allowlist."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_manus_residue(root)
    }
}

/// Per-file allowlist for the manus-residue doctor.
///
/// Paths are repo-relative POSIX paths. An entry here grants the file permission to
/// reference banned manus tokens. New entries should be added only with deliberate
/// architectural review.
///
pub const MANUS_RESIDUE_ALLOWLIST: &[&str] = &[];

/// Banned manus.* token patterns. Matched case-insensitively against the lowercased line.
///
/// These are the substrings the doctor walks for; the surrounding regex matcher is
/// constructed once for evidence trimming.
const BANNED_PATTERNS: &[&str] = &["manus.space", "manus.im", "forge.manus"];

/// Source extensions that are scanned for banned manus tokens.
///
/// Mirrors the workspace's runtime-source surface: TS/JS, Python, Rust, Go, and the
/// configuration formats commonly used to wire URLs and endpoints (TOML, YAML).
const SCAN_EXTENSIONS: &[&str] = &[
    "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "rs", "go", "toml", "yaml", "yml",
];

/// Filenames that are scanned even though they have no extension match (env-style files
/// frequently document `MANUS_*` URLs that downstream code then accidentally consumes).
const SCAN_FILENAMES: &[&str] = &[".env.example", ".env.local.example"];

/// Directories that are skipped entirely. These are vendored, generated, or out-of-scope
/// for the workspace's architectural deannexation policy.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "dist",
    "target",
    ".leio-code",
    ".git",
    ".claude",
    "wheels",
    ".next",
    ".turbo",
    "build",
    "coverage",
    ".venv",
    "venv",
    "__pycache__",
    "vendor",
    "vendored",
];

/// Filenames that are always skipped (docs, changelogs, lockfiles).
const SKIP_FILENAMES: &[&str] = &[
    "README.md",
    "CHANGELOG.md",
    "Cargo.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "uv.lock",
    "poetry.lock",
];

pub fn doctor_manus_residue(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings: Vec<String> = Vec::new();
    let mut entities: Vec<serde_json::Value> = Vec::new();
    let mut evidence: Vec<EvidenceItem> = Vec::new();

    let line_pattern = match Regex::new(
        r"(?i)(forge\.manus[a-zA-Z0-9._/-]*|[a-zA-Z0-9-]*\.manus\.space[a-zA-Z0-9./_-]*|[a-zA-Z0-9-]*\.manus\.im[a-zA-Z0-9./_-]*|manus\.space|manus\.im)",
    ) {
        Ok(re) => re,
        Err(err) => {
            warnings.push(format!("manus-residue regex compile failed: {err}"));
            return finalize_envelope(started, warnings, entities, evidence, 0);
        }
    };

    let scanned_paths = collect_scan_targets(root);
    let mut scanned_count = 0usize;
    let mut allowlisted_hits = 0usize;
    let mut violations = 0usize;

    for path in scanned_paths {
        scanned_count += 1;
        let rel = match path.strip_prefix(root) {
            Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };

        let allowlisted = MANUS_RESIDUE_ALLOWLIST
            .iter()
            .any(|allowed| *allowed == rel);

        let mut io_warnings: Vec<String> = Vec::new();
        let content = match read_text(&path, &mut io_warnings) {
            Some(content) => content,
            None => {
                warnings.extend(io_warnings);
                continue;
            }
        };

        for (idx, raw_line) in content.lines().enumerate() {
            let lower = raw_line.to_ascii_lowercase();
            let Some(matched_pattern) = BANNED_PATTERNS
                .iter()
                .find(|pattern| lower.contains(*pattern))
            else {
                continue;
            };

            // Extract a tighter match for evidence (e.g. "forge.manus.im/v1/...").
            let match_token = line_pattern
                .find(raw_line)
                .map(|m| m.as_str().to_string())
                .unwrap_or_else(|| (*matched_pattern).to_string());

            let snippet = trim_snippet(raw_line);

            if allowlisted {
                allowlisted_hits += 1;
                evidence.push(EvidenceItem {
                    kind: "manus_residue_allowlisted".to_string(),
                    path: rel.clone(),
                    line: Some(idx + 1),
                    detail: format!(
                        "allowlisted manus reference (`{matched_pattern}`) at line {} -- {}",
                        idx + 1,
                        snippet
                    ),
                });
                continue;
            }

            violations += 1;
            warnings.push(format!(
                "{rel}:{}: banned manus reference `{}` -- {}",
                idx + 1,
                match_token,
                snippet
            ));
            evidence.push(EvidenceItem {
                kind: "manus_residue_violation".to_string(),
                path: rel.clone(),
                line: Some(idx + 1),
                detail: format!(
                    "banned manus reference matched pattern `{}` (token=`{}`): {}",
                    matched_pattern, match_token, snippet
                ),
            });
        }
    }

    entities.push(json!({
        "doctor": "manus-residue",
        "scanned_files": scanned_count,
        "violations": violations,
        "allowlisted_hits": allowlisted_hits,
        "allowlist_size": MANUS_RESIDUE_ALLOWLIST.len(),
        "banned_patterns": BANNED_PATTERNS,
    }));

    finalize_envelope(started, warnings, entities, evidence, violations)
}

fn finalize_envelope(
    started: Instant,
    warnings: Vec<String>,
    entities: Vec<serde_json::Value>,
    evidence: Vec<EvidenceItem>,
    violations: usize,
) -> QueryEnvelope {
    let summary = if warnings.is_empty() {
        "no manus.* residue found in scanned source files".to_string()
    } else {
        format!(
            "found {} manus.* residue violation(s) across scanned source files",
            violations
        )
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_manus_residue"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.96 } else { 0.6 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn collect_scan_targets(root: &Path) -> Vec<PathBuf> {
    let mut builder = WalkBuilder::new(root);
    builder.hidden(false);
    builder.git_ignore(true);
    builder.git_global(true);
    builder.git_exclude(true);
    builder.filter_entry(|entry| {
        let Some(name) = entry.file_name().to_str() else {
            return true;
        };
        if entry.path().is_dir() {
            return !is_skipped_dir(name);
        }
        true
    });

    let mut paths = Vec::new();
    for dent in builder.build().flatten() {
        let path = dent.path();
        if !path.is_file() {
            continue;
        }
        if !should_scan_file(path) {
            continue;
        }
        paths.push(path.to_path_buf());
    }
    paths
}

fn is_skipped_dir(name: &str) -> bool {
    SKIP_DIRS.contains(&name)
}

fn should_scan_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    if SKIP_FILENAMES.contains(&name) {
        return false;
    }
    // Skip the doctor source tree itself — these files DEFINE the
    // banned patterns as lint targets, so scanning them yields a
    // self-referential false positive cascade (50+ warnings on the
    // doctor's own pattern table). Production code paths never live
    // under leio-code/src/doctors/, so the exclusion is safe.
    let path_str = path.to_string_lossy();
    if path_str.contains("/leio-code/src/doctors/")
        || path_str.contains("\\leio-code\\src\\doctors\\")
    {
        return false;
    }

    if SCAN_FILENAMES.contains(&name) {
        return true;
    }

    let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
        return false;
    };

    // Markdown / docs are explicitly excluded so the doctor never bans documentation
    // about historical manus references.
    if extension.eq_ignore_ascii_case("md") {
        return false;
    }

    SCAN_EXTENSIONS
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(extension))
}

fn trim_snippet(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.len() <= 200 {
        return trimmed.to_string();
    }
    let mut shortened: String = trimmed.chars().take(197).collect();
    shortened.push_str("...");
    shortened
}

#[cfg(test)]
mod tests {
    use super::doctor_manus_residue;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_tempdir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-manus-residue-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(path, contents).expect("write fixture file");
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn clean_repo_emits_zero_warnings() {
        let dir = unique_tempdir("clean");
        // A handful of innocent files across multiple languages, no manus references.
        write_file(
            &dir.join("apps/example/src/index.ts"),
            "export const greeting = 'hello world';\n",
        );
        write_file(
            &dir.join("services/example/main.py"),
            "def greet():\n    return 'hello world'\n",
        );
        write_file(
            &dir.join("crates/example/src/lib.rs"),
            "pub fn greet() -> &'static str { \"hello world\" }\n",
        );

        let envelope = doctor_manus_residue(&dir);

        assert!(
            envelope.warnings.is_empty(),
            "expected no warnings on clean fixture, got: {:?}",
            envelope.warnings
        );

        cleanup(&dir);
    }

    #[test]
    fn typescript_file_with_forge_manus_url_is_flagged() {
        let dir = unique_tempdir("ts-drift");
        let path = dir.join("apps/example/src/llm.ts");
        write_file(
            &path,
            r#"const FORGE_BASE = "https://forge.manus.im/v1/chat";
export async function call() {
  return fetch(FORGE_BASE);
}
"#,
        );

        let envelope = doctor_manus_residue(&dir);

        assert_eq!(
            envelope.warnings.len(),
            1,
            "expected one warning for forge.manus.im, got: {:?}",
            envelope.warnings
        );
        let warning = &envelope.warnings[0];
        assert!(warning.contains("apps/example/src/llm.ts"));
        assert!(warning.contains("forge.manus"));

        cleanup(&dir);
    }

    #[test]
    fn env_example_with_manus_url_is_flagged() {
        let dir = unique_tempdir("env-drift");
        write_file(
            &dir.join(".env.example"),
            "MANUS_BASE_URL=https://forge.manus.im/v1\nOTHER_VAR=value\n",
        );

        let envelope = doctor_manus_residue(&dir);

        assert_eq!(
            envelope.warnings.len(),
            1,
            "expected one warning for .env.example, got: {:?}",
            envelope.warnings
        );
        assert!(envelope.warnings[0].contains(".env.example"));

        cleanup(&dir);
    }

    #[test]
    fn env_local_example_with_manus_url_is_flagged() {
        let dir = unique_tempdir("env-local-drift");
        write_file(
            &dir.join(".env.local.example"),
            "FORGE_URL=https://app.manus.space/api\n",
        );

        let envelope = doctor_manus_residue(&dir);

        assert_eq!(
            envelope.warnings.len(),
            1,
            "expected one warning for .env.local.example, got: {:?}",
            envelope.warnings
        );
        assert!(envelope.warnings[0].contains(".env.local.example"));

        cleanup(&dir);
    }

    #[test]
    fn markdown_file_with_manus_reference_is_not_flagged() {
        let dir = unique_tempdir("md-skip");
        write_file(
            &dir.join("docs/migration.md"),
            "We used to depend on https://forge.manus.im — now we do not.\n",
        );
        write_file(
            &dir.join("README.md"),
            "Historical note: manus.space adapter has been removed.\n",
        );

        let envelope = doctor_manus_residue(&dir);

        assert!(
            envelope.warnings.is_empty(),
            "markdown with manus references must be skipped, got warnings: {:?}",
            envelope.warnings
        );

        cleanup(&dir);
    }

    #[test]
    fn lockfile_with_manus_reference_is_not_flagged() {
        let dir = unique_tempdir("lock-skip");
        write_file(
            &dir.join("pnpm-lock.yaml"),
            "packages:\n  /@manus.space/foo@1.2.3:\n    resolution: ...\n",
        );
        write_file(&dir.join("Cargo.lock"), "# manus.im legacy reference\n");
        write_file(
            &dir.join("package-lock.json"),
            "{\n  \"resolved\": \"https://forge.manus.im/x\"\n}\n",
        );

        let envelope = doctor_manus_residue(&dir);

        assert!(
            envelope.warnings.is_empty(),
            "lockfiles with manus references must be skipped, got warnings: {:?}",
            envelope.warnings
        );

        cleanup(&dir);
    }

    #[test]
    fn vendored_node_modules_copy_is_not_flagged() {
        let dir = unique_tempdir("vendored-skip");
        write_file(
            &dir.join("apps/web/node_modules/some-pkg/dist/index.js"),
            "module.exports = { url: 'https://forge.manus.im/v1' };\n",
        );

        let envelope = doctor_manus_residue(&dir);

        assert!(
            envelope.warnings.is_empty(),
            "vendored node_modules must be skipped, got warnings: {:?}",
            envelope.warnings
        );

        cleanup(&dir);
    }

    #[test]
    fn target_and_dist_artifacts_are_not_flagged() {
        let dir = unique_tempdir("artifact-skip");
        write_file(
            &dir.join("crates/example/target/debug/build/info.rs"),
            "// build artifact: forge.manus.im was here\n",
        );
        write_file(
            &dir.join("apps/web/dist/bundle.js"),
            "// bundle: app.manus.space\n",
        );
        write_file(
            &dir.join("apps/web/.next/server/app.js"),
            "// next build: forge.manus.im\n",
        );

        let envelope = doctor_manus_residue(&dir);

        assert!(
            envelope.warnings.is_empty(),
            "build artifacts must be skipped, got warnings: {:?}",
            envelope.warnings
        );

        cleanup(&dir);
    }

    #[test]
    fn case_insensitive_match_against_uppercase_token() {
        let dir = unique_tempdir("case-insensitive");
        write_file(
            &dir.join("services/api/router.py"),
            "MANUS_URL = \"https://Forge.Manus.IM/v1\"\n",
        );

        let envelope = doctor_manus_residue(&dir);

        assert_eq!(
            envelope.warnings.len(),
            1,
            "expected case-insensitive match, got: {:?}",
            envelope.warnings
        );

        cleanup(&dir);
    }

    #[test]
    fn rust_python_go_yaml_toml_files_are_scanned() {
        let dir = unique_tempdir("multi-lang");
        write_file(
            &dir.join("crates/x/src/main.rs"),
            "fn url() -> &'static str { \"https://forge.manus.im\" }\n",
        );
        write_file(
            &dir.join("services/y/main.py"),
            "URL = 'https://manus.space/api'\n",
        );
        write_file(
            &dir.join("services/z/main.go"),
            "var url = \"https://forge.manus.im\"\n",
        );
        write_file(
            &dir.join("deploy/config.yaml"),
            "manus_url: https://forge.manus.im\n",
        );
        write_file(
            &dir.join("deploy/profile.toml"),
            "manus_url = \"https://manus.im/api\"\n",
        );

        let envelope = doctor_manus_residue(&dir);

        assert_eq!(
            envelope.warnings.len(),
            5,
            "expected one warning per language, got: {:?}",
            envelope.warnings
        );

        cleanup(&dir);
    }

    #[test]
    fn evidence_records_violation_with_line_and_pattern() {
        let dir = unique_tempdir("evidence");
        write_file(
            &dir.join("apps/x/src/wire.ts"),
            "// nothing on line 1\nconst URL = \"https://forge.manus.im/v1\";\n",
        );

        let envelope = doctor_manus_residue(&dir);

        assert_eq!(envelope.warnings.len(), 1);
        let violation = envelope
            .evidence
            .iter()
            .find(|item| item.kind == "manus_residue_violation")
            .expect("violation evidence present");
        assert_eq!(violation.line, Some(2));
        assert!(
            violation.detail.contains("forge.manus"),
            "evidence detail should mention matched pattern: {}",
            violation.detail
        );

        cleanup(&dir);
    }
}
