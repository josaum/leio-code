//! Align Dockerfile path-dep coherence doctor.
//!
//! Catches the **exact deploy bug surfaced 2026-05-27 by PR #224**: a new
//! `path = "../foo"` dependency was added to `example-align/Cargo.toml`
//! (the `owl-fast-core` extraction) but the corresponding `COPY` line in
//! `example-align/Dockerfile` and the rsync line in
//! `scripts/docker-context.sh` (align section) were not updated.
//!
//! The mismatch is silent until Backend Publish runs:
//!
//! ```text
//! cargo chef prepare --recipe-path recipe.json
//! Caused by:
//!   failed to load manifest for dependency `owl-fast-core`
//!   failed to read `/workspace/office-parsers-rs/owl-fast-core/Cargo.toml`
//!   No such file or directory (os error 2)
//! ```
//!
//! Three-surface invariant the doctor enforces:
//! 1. Every `path = "..."` dep in `example-align/Cargo.toml` must have…
//! 2. …a matching `COPY <path-prefix>` line in `example-align/Dockerfile`
//!    (both planner AND builder stages — cargo chef needs it in both), AND
//! 3. …a matching `rsync ... <path-prefix>/ "$CTX/align/<path-prefix>/"`
//!    line in `scripts/docker-context.sh` align section.
//!
//! Scope:
//! - Only `example-align`. The same bug class later hit `example-server`;
//!   that surface is covered by `server-dockerfile-context-coherence`.
//!
//! Not in scope:
//! - Transitive path deps (owl-fast-core's own deps).
//! - Cross-Dockerfile drift (only checks example-align/Dockerfile).

use std::path::Path;
use std::time::Instant;

use regex::Regex;
use serde_json::json;

use super::Doctor;
use super::utils::query_id;
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct AlignDockerfilePathDepCoherenceDoctor;

impl Doctor for AlignDockerfilePathDepCoherenceDoctor {
    fn name(&self) -> &'static str {
        "align-dockerfile-path-dep-coherence"
    }

    fn description(&self) -> &'static str {
        "For every `path = \"../X\"` dep in example-align/Cargo.toml, validates that both example-align/Dockerfile (planner+builder COPY) and scripts/docker-context.sh (align rsync) include the path. Catches the owl-fast-core deploy bug from PR #224."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_align_dockerfile_path_dep_coherence(root)
    }
}

pub fn doctor_align_dockerfile_path_dep_coherence(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let cargo = root.join("example-align/Cargo.toml");
    let dockerfile = root.join("example-align/Dockerfile");
    let ctx_script = root.join("scripts/docker-context.sh");

    let Ok(cargo_text) = std::fs::read_to_string(&cargo) else {
        // Cargo.toml absent — the doctor doesn't apply to this workspace.
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_align_dockerfile_path_dep_coherence"),
            kind: "doctor".to_string(),
            summary: "align-dockerfile-path-dep-coherence: example-align/Cargo.toml not found — doctor skipped".to_string(),
            confidence: 0.95,
            entities,
            evidence,
            warnings,
            meta: Some(json!({"reason": "cargo_toml_missing"})),
            timing_ms: started.elapsed().as_millis(),
        };
    };

    // Extract `path = "..."` values. Matches both:
    //   `owl-fast-core = { path = "../office-parsers-rs/owl-fast-core" }`
    //   `example-client = { path = "../example-api/sdks/example-client" }`
    //
    // Anchored to `path\s*=\s*"..."` so it doesn't trip on other quoted
    // strings (e.g. `description = "..."`).
    let path_re = Regex::new(r#"\bpath\s*=\s*"\.\./([^"]+)""#).expect("path regex");
    let mut path_deps: Vec<String> = Vec::new();
    for caps in path_re.captures_iter(&cargo_text) {
        if let Some(m) = caps.get(1) {
            let rel = m.as_str().to_string();
            if !path_deps.contains(&rel) {
                path_deps.push(rel);
            }
        }
    }

    let dockerfile_text = std::fs::read_to_string(&dockerfile).unwrap_or_default();
    let ctx_text = std::fs::read_to_string(&ctx_script).unwrap_or_default();

    // The Dockerfile has TWO stages (planner + builder) that both COPY
    // the deps. Count occurrences so we can flag "in one stage but not
    // the other" — cargo chef needs both.
    for dep in &path_deps {
        // The "top-level" of the dep path is what COPY targets. e.g.
        // dep = "office-parsers-rs/owl-fast-core" → top = "office-parsers-rs"
        // dep = "example-api/sdks/example-client" → top covers as nested COPY
        //
        // To be safe we just check the FULL dep path is referenced —
        // cargo path-deps need the manifest at the resolved location.

        // Count `COPY <dep>` lines (whitespace-tolerant).
        let copy_pat = format!(r"COPY\s+{}\s", regex::escape(dep));
        let copy_re = Regex::new(&copy_pat).expect("copy regex");
        let copy_count = copy_re.find_iter(&dockerfile_text).count();
        if copy_count < 2 {
            warnings.push(format!(
                "example-align/Cargo.toml path-dep `{dep}` appears in {copy_count} `COPY` line(s) in example-align/Dockerfile (need ≥2: planner + builder stages)"
            ));
            entities.push(json!({
                "doctor": "align-dockerfile-path-dep-coherence",
                "surface": "example-align/Dockerfile",
                "path_dep": dep,
                "copy_count": copy_count,
                "required_copy_count": 2,
            }));
            evidence.push(EvidenceItem {
                kind: "missing_dockerfile_copy".to_string(),
                path: "example-align/Dockerfile".to_string(),
                line: None,
                detail: format!("expected `COPY {dep} ./{dep}` in both planner and builder stages"),
            });
        }

        // Check that scripts/docker-context.sh's align section rsyncs the
        // dep. We approximate "align section" as: the file mentions
        // `CTX/align/` lines that include `<dep>`.
        let ctx_pat = format!(r"\$CTX/align/{}", regex::escape(dep));
        let ctx_re = Regex::new(&ctx_pat).expect("ctx regex");
        let ctx_hits = ctx_re.find_iter(&ctx_text).count();
        if ctx_hits == 0 {
            warnings.push(format!(
                "example-align/Cargo.toml path-dep `{dep}` is not rsync'd into the align context in scripts/docker-context.sh"
            ));
            entities.push(json!({
                "doctor": "align-dockerfile-path-dep-coherence",
                "surface": "scripts/docker-context.sh",
                "path_dep": dep,
                "rsync_count": 0,
            }));
            evidence.push(EvidenceItem {
                kind: "missing_context_rsync".to_string(),
                path: "scripts/docker-context.sh".to_string(),
                line: None,
                detail: format!(
                    "expected `rsync ... $ROOT/{dep}/ $CTX/align/{dep}/` in the align section"
                ),
            });
        }
    }

    let summary = format!(
        "align-dockerfile-path-dep-coherence: {} path-dep(s) in example-align/Cargo.toml, {} drift warning(s)",
        path_deps.len(),
        warnings.len()
    );

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_align_dockerfile_path_dep_coherence"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.95 } else { 0.85 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "path_deps": path_deps,
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
            "leio-code-align-pdep-{label}-{}-{nanos}",
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
    fn flags_path_dep_missing_from_dockerfile() {
        let dir = unique_tempdir("missing-copy");
        // Cargo.toml declares owl-fast-core as a path dep.
        write_file(
            &dir.join("example-align/Cargo.toml"),
            "[package]\nname = \"example-align\"\n\n[dependencies]\nowl-fast-core = { path = \"../office-parsers-rs/owl-fast-core\" }\n",
        );
        // Dockerfile copies example-align but NOT owl-fast-core.
        write_file(
            &dir.join("example-align/Dockerfile"),
            "FROM rust AS planner\nCOPY example-align ./example-align\nFROM rust AS builder\nCOPY example-align ./example-align\n",
        );
        // Context script also missing the rsync.
        write_file(
            &dir.join("scripts/docker-context.sh"),
            "rsync example-align/ \"$CTX/align/example-align/\"\n",
        );

        let envelope = doctor_align_dockerfile_path_dep_coherence(&dir);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("owl-fast-core") && w.contains("Dockerfile")),
            "expected Dockerfile drift to be flagged, got: {:?}",
            envelope.warnings
        );
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("owl-fast-core") && w.contains("docker-context.sh")),
            "expected context.sh drift to be flagged, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn does_not_flag_when_path_dep_is_wired_correctly() {
        let dir = unique_tempdir("wired-ok");
        write_file(
            &dir.join("example-align/Cargo.toml"),
            "[package]\nname = \"example-align\"\n\n[dependencies]\nowl-fast-core = { path = \"../office-parsers-rs/owl-fast-core\" }\n",
        );
        write_file(
            &dir.join("example-align/Dockerfile"),
            "FROM rust AS planner\nCOPY example-align ./example-align\nCOPY office-parsers-rs/owl-fast-core ./office-parsers-rs/owl-fast-core\nFROM rust AS builder\nCOPY example-align ./example-align\nCOPY office-parsers-rs/owl-fast-core ./office-parsers-rs/owl-fast-core\n",
        );
        write_file(
            &dir.join("scripts/docker-context.sh"),
            "rsync example-align/ \"$CTX/align/example-align/\"\nrsync office-parsers-rs/owl-fast-core/ \"$CTX/align/office-parsers-rs/owl-fast-core/\"\n",
        );

        let envelope = doctor_align_dockerfile_path_dep_coherence(&dir);
        assert!(
            envelope.warnings.is_empty(),
            "fully wired path dep should not warn, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flags_dockerfile_one_stage_but_not_the_other() {
        let dir = unique_tempdir("one-stage");
        write_file(
            &dir.join("example-align/Cargo.toml"),
            "[package]\nname = \"example-align\"\n\n[dependencies]\nowl-fast-core = { path = \"../office-parsers-rs/owl-fast-core\" }\n",
        );
        // ONE COPY (planner stage) but missing in builder stage.
        write_file(
            &dir.join("example-align/Dockerfile"),
            "FROM rust AS planner\nCOPY example-align ./example-align\nCOPY office-parsers-rs/owl-fast-core ./office-parsers-rs/owl-fast-core\nFROM rust AS builder\nCOPY example-align ./example-align\n",
        );
        write_file(
            &dir.join("scripts/docker-context.sh"),
            "rsync office-parsers-rs/owl-fast-core/ \"$CTX/align/office-parsers-rs/owl-fast-core/\"\n",
        );

        let envelope = doctor_align_dockerfile_path_dep_coherence(&dir);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("Dockerfile") && w.contains("need ≥2")),
            "expected single-stage Dockerfile drift to be flagged, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn returns_clean_when_no_path_deps_present() {
        let dir = unique_tempdir("no-path-deps");
        write_file(
            &dir.join("example-align/Cargo.toml"),
            "[package]\nname = \"example-align\"\n\n[dependencies]\nserde = \"1\"\noxrdf = \"0.2\"\n",
        );
        // No Dockerfile, no context script — irrelevant if there are no path deps.
        let envelope = doctor_align_dockerfile_path_dep_coherence(&dir);
        assert!(
            envelope.warnings.is_empty(),
            "expected no warnings when no path deps, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
