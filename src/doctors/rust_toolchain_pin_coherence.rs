//! Rust toolchain pin coherence doctor.
//!
//! Enforces a single project-level Rust version as the source of truth.
//! `rust-toolchain.toml` (`channel = "X.Y.Z"`) is canonical; every Docker
//! build that pins a Rust toolchain must agree on the same `X.Y` minor.
//!
//! A drifted base image either fails to compile workspace crates whose
//! `rust-toolchain.toml` demands a newer compiler, or silently builds against
//! a different toolchain than CI and local development. The doctor makes one
//! current-stable project toolchain an enforced invariant rather than a
//! periodic manual sweep.
//!
//! Four surfaces are checked against the canonical `X.Y`:
//! 1. `FROM rust:<ver>-<variant>` base images in every Dockerfile.
//! 2. `cargo-chef:latest-rust-<ver>-<variant>` builder images.
//! 3. Inline `channel = "<ver>"` pins written into Dockerfiles.
//! 4. The local gateway build-toolchain default.
//!
//! Scope (intentionally narrow, matching `align-dockerfile-path-dep-coherence`):
//! - Only the `X.Y` minor is compared, not the patch or the OS variant.
//!   `1.95-bookworm` and `1.95-trixie` both satisfy a `1.95.0` canonical
//!   pin — the distro variant is a deliberate per-image choice.
//! - Vendored / third-party trees are skipped (`vendor/`, `node_modules/`,
//!   `target/`, `.claude/worktrees`, …). We only govern first-party builds.
//! - CI workflows that use `dtolnay/rust-toolchain@stable` are *not* flagged:
//!   that action reads `rust-toolchain.toml` automatically, so it already
//!   tracks the canonical pin.

use std::path::Path;
use std::time::Instant;

use regex::Regex;
use serde_json::json;

use super::Doctor;
use super::utils::query_id;
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct RustToolchainPinCoherenceDoctor;

impl Doctor for RustToolchainPinCoherenceDoctor {
    fn name(&self) -> &'static str {
        "rust-toolchain-pin-coherence"
    }

    fn description(&self) -> &'static str {
        "Treats rust-toolchain.toml as the canonical current-stable Rust version and flags drift in Rust/cargo-chef Docker builders, inline pins, and the gateway local-build default."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_rust_toolchain_pin_coherence(root)
    }
}

/// Directories never scanned for Dockerfiles — vendored, generated, or
/// out-of-scope worktrees that we do not govern.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    ".claude",
    ".docker-context",
    ".leio-code",
    ".leio-kb",
    "vendor",
    "vendored",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".turbo",
];

/// Extract the canonical `X.Y` minor (e.g. `"1.95"`) from a
/// `rust-toolchain.toml` body's `channel = "..."` line. Returns `None` when
/// the channel is a named track (`stable`, `nightly`) rather than a pinned
/// version, since there is no numeric minor to compare against.
fn canonical_minor(toolchain_text: &str) -> Option<String> {
    let channel_re = Regex::new(r#"(?m)^\s*channel\s*=\s*"([^"]+)""#).expect("channel regex");
    let caps = channel_re.captures(toolchain_text)?;
    let channel = caps.get(1)?.as_str().trim();
    minor_of(channel)
}

/// Reduce a version string like `1.95.0` / `1.95` to its `X.Y` minor
/// (`1.95`). Named channels (`stable`, `nightly`, `beta`) return `None`.
fn minor_of(version: &str) -> Option<String> {
    let mut parts = version.split('.');
    let major = parts.next()?;
    let minor = parts.next()?;
    if major.chars().all(|c| c.is_ascii_digit())
        && !major.is_empty()
        && minor.chars().all(|c| c.is_ascii_digit())
        && !minor.is_empty()
    {
        Some(format!("{major}.{minor}"))
    } else {
        None
    }
}

/// Recursively collect Dockerfile paths under `root`, skipping `SKIP_DIRS`.
/// A "Dockerfile" is any file whose name is `Dockerfile` or starts with
/// `Dockerfile.` (e.g. `Dockerfile.optimized`, `Dockerfile.linux-wheels`).
fn collect_dockerfiles(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if file_type.is_dir() {
                if SKIP_DIRS.contains(&name.as_ref()) {
                    continue;
                }
                stack.push(path);
            } else if name == "Dockerfile" || name.starts_with("Dockerfile.") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

pub fn doctor_rust_toolchain_pin_coherence(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let toolchain_path = root.join("rust-toolchain.toml");
    let Ok(toolchain_text) = std::fs::read_to_string(&toolchain_path) else {
        // No canonical pin — the doctor doesn't apply to this workspace.
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_rust_toolchain_pin_coherence"),
            kind: "doctor".to_string(),
            summary: "rust-toolchain-pin-coherence: rust-toolchain.toml not found — doctor skipped"
                .to_string(),
            confidence: 0.95,
            entities,
            evidence,
            warnings,
            meta: Some(json!({"reason": "rust_toolchain_toml_missing"})),
            timing_ms: started.elapsed().as_millis(),
        };
    };

    let Some(canonical) = canonical_minor(&toolchain_text) else {
        // Channel is a named track (stable/nightly) — nothing numeric to
        // enforce. Treat as a clean skip rather than a warning.
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_rust_toolchain_pin_coherence"),
            kind: "doctor".to_string(),
            summary: "rust-toolchain-pin-coherence: rust-toolchain.toml channel is not a pinned version — doctor skipped".to_string(),
            confidence: 0.95,
            entities,
            evidence,
            warnings,
            meta: Some(json!({"reason": "channel_not_pinned"})),
            timing_ms: started.elapsed().as_millis(),
        };
    };

    // `FROM rust:1.95-bookworm` / `FROM rust:1.95` / `FROM rust:1.95-slim-bookworm`
    // — capture the version token after `rust:` (before any `-variant` suffix).
    // The variant char class includes `-` so multi-segment tags like
    // `1.83-slim-bookworm` match as a whole (otherwise the suffix stops at the
    // first segment and the trailing delimiter fails the match entirely,
    // silently skipping a real class of tags). Terminator is whitespace or
    // end-of-input (so a tag on the final line without a trailing newline still
    // matches). Case-insensitive on FROM; digest pins (`@sha256:…`) have no
    // human-readable minor and are intentionally left alone.
    let from_re = Regex::new(r#"(?im)^\s*FROM\s+rust:([0-9][0-9.]*)(?:-[A-Za-z0-9._-]+)?(?:\s|$)"#)
        .expect("from re");
    let cargo_chef_re =
        Regex::new(r#"cargo-chef:latest-rust-([0-9][0-9.]*)(?:-[A-Za-z0-9._-]+)?(?:\s|$)"#)
            .expect("cargo-chef re");
    // Inline pinned toolchain written into a Dockerfile RUN (the wheels
    // builder pattern).
    let inline_re = Regex::new(r#"channel\s*=\s*\\?"([0-9][0-9.]*)\\?""#).expect("inline re");

    let mut checked = 0usize;
    for dockerfile in collect_dockerfiles(root) {
        let Ok(text) = std::fs::read_to_string(&dockerfile) else {
            continue;
        };
        let rel = dockerfile
            .strip_prefix(root)
            .unwrap_or(&dockerfile)
            .to_string_lossy()
            .to_string();

        for caps in from_re.captures_iter(&text) {
            let Some(ver) = caps.get(1) else { continue };
            let ver = ver.as_str();
            let Some(found) = minor_of(ver) else { continue };
            // Count only matches that parse as a real minor — mirrors the
            // inline loop below so "checked N pin(s)" never overcounts.
            checked += 1;
            if found != canonical {
                let line = line_of_match(&text, caps.get(0).map(|m| m.start()).unwrap_or(0));
                warnings.push(format!(
                    "{rel}: `FROM rust:{ver}` pins Rust {found}, but rust-toolchain.toml is {canonical}"
                ));
                entities.push(json!({
                    "doctor": "rust-toolchain-pin-coherence",
                    "surface": rel,
                    "kind": "from_base_image",
                    "found_minor": found,
                    "canonical_minor": canonical,
                    "found_version": ver,
                }));
                evidence.push(EvidenceItem {
                    kind: "rust_pin_drift".to_string(),
                    path: rel.clone(),
                    line: Some(line),
                    detail: format!("expected `FROM rust:{canonical}…`, found `FROM rust:{ver}`"),
                });
            }
        }

        for caps in cargo_chef_re.captures_iter(&text) {
            let Some(ver) = caps.get(1) else { continue };
            let ver = ver.as_str();
            let Some(found) = minor_of(ver) else { continue };
            checked += 1;
            if found != canonical {
                let line = line_of_match(&text, caps.get(0).map(|m| m.start()).unwrap_or(0));
                warnings.push(format!(
                    "{rel}: cargo-chef builder pins Rust {found}, but rust-toolchain.toml is {canonical}"
                ));
                entities.push(json!({
                    "doctor": "rust-toolchain-pin-coherence",
                    "surface": rel,
                    "kind": "cargo_chef_base_image",
                    "found_minor": found,
                    "canonical_minor": canonical,
                    "found_version": ver,
                }));
                evidence.push(EvidenceItem {
                    kind: "rust_pin_drift".to_string(),
                    path: rel.clone(),
                    line: Some(line),
                    detail: format!("expected cargo-chef Rust {canonical}, found Rust {found}"),
                });
            }
        }

        for caps in inline_re.captures_iter(&text) {
            let Some(ver) = caps.get(1) else { continue };
            let ver = ver.as_str();
            let Some(found) = minor_of(ver) else { continue };
            checked += 1;
            if found != canonical {
                let line = line_of_match(&text, caps.get(0).map(|m| m.start()).unwrap_or(0));
                warnings.push(format!(
                    "{rel}: inline `channel = \"{ver}\"` pins Rust {found}, but rust-toolchain.toml is {canonical}"
                ));
                entities.push(json!({
                    "doctor": "rust-toolchain-pin-coherence",
                    "surface": rel,
                    "kind": "inline_channel",
                    "found_minor": found,
                    "canonical_minor": canonical,
                    "found_version": ver,
                }));
                evidence.push(EvidenceItem {
                    kind: "rust_pin_drift".to_string(),
                    path: rel.clone(),
                    line: Some(line),
                    detail: format!(
                        "expected inline `channel = \"{canonical}…\"`, found `channel = \"{ver}\"`"
                    ),
                });
            }
        }
    }

    let gateway_start = root.join("example-gateway/start-gateway.sh");
    if let Ok(text) = std::fs::read_to_string(&gateway_start) {
        let build_re = Regex::new(r#"BUILD_TOOLCHAIN="\$\{BUILD_TOOLCHAIN:-([0-9][0-9.]*)\}""#)
            .expect("gateway build toolchain re");
        if let Some(caps) = build_re.captures(&text)
            && let Some(ver) = caps.get(1)
            && let Some(found) = minor_of(ver.as_str())
        {
            checked += 1;
            if found != canonical {
                let rel = "example-gateway/start-gateway.sh";
                let line = line_of_match(&text, caps.get(0).map(|m| m.start()).unwrap_or(0));
                warnings.push(format!(
                    "{rel}: BUILD_TOOLCHAIN pins Rust {found}, but rust-toolchain.toml is {canonical}"
                ));
                entities.push(json!({
                    "doctor": "rust-toolchain-pin-coherence",
                    "surface": rel,
                    "kind": "gateway_build_toolchain",
                    "found_minor": found,
                    "canonical_minor": canonical,
                    "found_version": ver.as_str(),
                }));
                evidence.push(EvidenceItem {
                    kind: "rust_pin_drift".to_string(),
                    path: rel.to_string(),
                    line: Some(line),
                    detail: format!("expected BUILD_TOOLCHAIN {canonical}, found {found}"),
                });
            }
        }
    }

    let summary = format!(
        "rust-toolchain-pin-coherence: canonical Rust {canonical}; checked {checked} pin(s), {} drift warning(s)",
        warnings.len()
    );

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_rust_toolchain_pin_coherence"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.95 } else { 0.85 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "canonical_minor": canonical,
            "pins_checked": checked,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// 1-based line number containing the byte offset `at`.
fn line_of_match(text: &str, at: usize) -> usize {
    text[..at.min(text.len())]
        .bytes()
        .filter(|&b| b == b'\n')
        .count()
        + 1
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
            "leio-code-rust-pin-{label}-{}-{nanos}",
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
    fn flags_drifted_base_image() {
        let dir = unique_tempdir("drift");
        write_file(
            &dir.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.95.0\"\n",
        );
        write_file(
            &dir.join("svc/Dockerfile"),
            "FROM rust:1.83-bookworm AS build\nWORKDIR /src\n",
        );
        let env = doctor_rust_toolchain_pin_coherence(&dir);
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("1.83") && w.contains("1.95")),
            "expected drift flagged, got: {:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn accepts_matching_minor_across_variants() {
        let dir = unique_tempdir("ok");
        write_file(
            &dir.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.95.0\"\n",
        );
        // Same minor, different OS variants — both fine.
        write_file(&dir.join("a/Dockerfile"), "FROM rust:1.95-bookworm AS b\n");
        write_file(
            &dir.join("b/Dockerfile.opt"),
            "FROM rust:1.95-trixie AS b\n",
        );
        let env = doctor_rust_toolchain_pin_coherence(&dir);
        assert!(
            env.warnings.is_empty(),
            "matching minors must not warn, got: {:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flags_drift_on_multi_segment_variant_tag() {
        // Regression: `rust:1.83-slim-bookworm` (two-segment variant) must still
        // be parsed and flagged. An earlier regex stopped the variant at the
        // first segment, so the trailing `-bookworm` failed the whole match and
        // the drifted pin was silently skipped.
        let dir = unique_tempdir("multiseg");
        write_file(
            &dir.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.95.0\"\n",
        );
        write_file(
            &dir.join("svc/Dockerfile"),
            "FROM rust:1.83-slim-bookworm AS build\nWORKDIR /src\n",
        );
        let env = doctor_rust_toolchain_pin_coherence(&dir);
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("1.83") && w.contains("1.95")),
            "expected multi-segment-variant drift flagged, got: {:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn accepts_matching_multi_segment_variant() {
        // A multi-segment variant on the canonical minor must NOT warn.
        let dir = unique_tempdir("multiseg-ok");
        write_file(
            &dir.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.95.0\"\n",
        );
        write_file(
            &dir.join("svc/Dockerfile"),
            "FROM rust:1.95-slim-bookworm AS build\n",
        );
        let env = doctor_rust_toolchain_pin_coherence(&dir);
        assert!(
            env.warnings.is_empty(),
            "matching multi-segment variant must not warn, got: {:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flags_inline_channel_drift() {
        let dir = unique_tempdir("inline");
        write_file(
            &dir.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.95.0\"\n",
        );
        write_file(
            &dir.join("wheels/Dockerfile.linux-wheels"),
            "FROM python:3.12\nRUN printf '[toolchain]\\nchannel = \"1.93.0\"\\n' > /io/rust-toolchain.toml\n",
        );
        let env = doctor_rust_toolchain_pin_coherence(&dir);
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("inline") && w.contains("1.93")),
            "expected inline drift flagged, got: {:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flags_cargo_chef_and_gateway_build_toolchain_drift() {
        let dir = unique_tempdir("cargo-chef-gateway");
        write_file(
            &dir.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.97.0\"\n",
        );
        write_file(
            &dir.join("Dockerfile.rust-base"),
            "FROM lukemathwalker/cargo-chef:latest-rust-1.94-bookworm AS build\n",
        );
        write_file(
            &dir.join("example-gateway/start-gateway.sh"),
            "BUILD_TOOLCHAIN=\"${BUILD_TOOLCHAIN:-1.92.0}\"\n",
        );
        let env = doctor_rust_toolchain_pin_coherence(&dir);
        assert!(
            env.warnings
                .iter()
                .any(|warning| warning.contains("cargo-chef") && warning.contains("1.94"))
        );
        assert!(
            env.warnings
                .iter()
                .any(|warning| warning.contains("BUILD_TOOLCHAIN") && warning.contains("1.92"))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_vendored_dockerfiles() {
        let dir = unique_tempdir("vendor");
        write_file(
            &dir.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.95.0\"\n",
        );
        // A drifted Dockerfile under vendor/ must be ignored.
        write_file(
            &dir.join("office-parsers-rs/vendor/oar/Dockerfile"),
            "FROM rust:1.70-bookworm AS build\n",
        );
        let env = doctor_rust_toolchain_pin_coherence(&dir);
        assert!(
            env.warnings.is_empty(),
            "vendored Dockerfiles must be skipped, got: {:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_when_channel_not_pinned() {
        let dir = unique_tempdir("named");
        write_file(
            &dir.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"stable\"\n",
        );
        write_file(
            &dir.join("svc/Dockerfile"),
            "FROM rust:1.83-bookworm AS b\n",
        );
        let env = doctor_rust_toolchain_pin_coherence(&dir);
        assert!(
            env.warnings.is_empty(),
            "named channel can't be enforced numerically, got: {:?}",
            env.warnings
        );
        assert!(env.summary.contains("skipped"));
        let _ = fs::remove_dir_all(&dir);
    }
}
