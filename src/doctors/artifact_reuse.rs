// Rust guideline compliant 2026-02-21
//! `artifact-reuse` doctor.
//!
//! Enforces the consolidated `artifacts/` reorg: committed PyO3 wheels live
//! only under `artifacts/wheels/manylinux/`, no consumer still references the
//! retired `wheels/` root, the phantom unscoped `*-fast-node` registry pins are
//! gone from the frontend apps, and `artifacts/manifest.json` indexes the
//! tracked/unignored wheel artifacts.
//!
//! This is the enforcement surface that keeps the reorg from silently eroding:
//! a stray wheel, a stale consumer path, a resurrected phantom pin, or a
//! manifest that drifts from tracked/unignored artifacts each fire a named
//! warning here before they reach a build or a deploy.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, git_tracked_files, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

/// Canonical home for committed manylinux PyO3 wheels.
const WHEEL_HOME: &str = "artifacts/wheels/manylinux";

/// Retired wheel root. No committed `.whl` and no consumer path may live here.
///
/// The reorg moved every shipped wheel out of `wheels/` into [`WHEEL_HOME`].
/// A `.whl` reappearing under `wheels/` (outside the `_quarantine` scratch
/// area) is a regression, not a new artifact home.
const RETIRED_WHEEL_ROOT: &str = "wheels/";

/// Scratch sub-path under the retired root that is allowed to hold wheels.
///
/// `wheels/_quarantine/` is a gitignored local staging area (see the root
/// `.gitignore`); committed wheels still may not live there, but on-disk
/// scratch files must not trip the stray-wheel scan.
const RETIRED_ROOT_QUARANTINE: &str = "wheels/_quarantine/";

/// Consumer files that must resolve wheels through `artifacts/`, never `wheels/`.
///
/// These are the build/bootstrap surfaces that install the wheelhouse. Each is
/// scanned for a repo-relative `wheels/` source reference; container-internal
/// paths (`/build/wheels/`, `/tmp/source-wheels/`) are explicitly tolerated.
const CONSUMER_FILES: &[&str] = &[
    "example-api/Dockerfile",
    "example-api/Dockerfile.optimized",
    "scripts/bootstrap-root-python-venv.sh",
    "deploy/scripts/validate.sh",
    "Makefile",
];

/// Container-internal `wheels/` substrings tolerated inside consumer files.
///
/// A consumer may `COPY artifacts/wheels/manylinux/ /build/wheels/` and then
/// glob `/build/wheels/*.whl`; those are paths inside the image, not the
/// retired repo root, so they are not drift.
const TOLERATED_CONSUMER_NEEDLES: &[&str] = &[
    "artifacts/wheels",
    "/build/wheels",
    "/tmp/source-wheels",
    "wheels/macos-arm64",
    "wheels/_quarantine",
];

/// The four phantom unscoped `*-fast-node` registry pins.
///
/// These bare names resolved to the public npm registry instead of the in-repo
/// `@example/<x>-fast-node` workspace packages. They were removed from the
/// frontend `package.json` dependency blocks; their reappearance there is a
/// supply-chain regression.
const PHANTOM_FAST_NODE_PINS: &[&str] = &[
    "docx-fast-node",
    "email-fast-node",
    "pptx-fast-node",
    "xlsx-fast-node",
];

/// Frontend `package.json` files that must stay free of the phantom pins.
const FRONTEND_PACKAGE_JSON: &[&str] = &["example-ops/package.json"];

pub struct ArtifactReuseDoctor;

impl Doctor for ArtifactReuseDoctor {
    fn name(&self) -> &'static str {
        "artifact-reuse"
    }

    fn description(&self) -> &'static str {
        "Checks the artifacts/ reorg: wheels live only under artifacts/wheels/manylinux/, no consumer references the retired wheels/ root, phantom fast-node pins are gone from example-ops, and artifacts/manifest.json lists tracked/unignored wheels."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_artifact_reuse(root)
    }
}

/// Run the four `artifact-reuse` checks against `root` and build an envelope.
///
/// # Examples
///
/// ```ignore
/// let envelope = doctor_artifact_reuse(repo_root);
/// assert!(envelope.warnings.is_empty(), "reorg is consistent");
/// ```
pub fn doctor_artifact_reuse(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let on_disk_wheels = check_wheel_home(root, &mut warnings, &mut evidence);
    check_no_consumer_references_retired_root(root, &mut warnings, &mut evidence);
    check_phantom_fast_node_pins(root, &mut warnings, &mut evidence);
    check_manifest_lists_on_disk_wheels(root, &on_disk_wheels, &mut warnings, &mut evidence);

    let ready = warnings.is_empty();
    entities.push(json!({
        "doctor": "artifact-reuse",
        "wheel_home": WHEEL_HOME,
        "on_disk_wheel_count": on_disk_wheels.len(),
        "git_visible_wheel_count": on_disk_wheels.len(),
        "retired_roots": ["release-artifacts/", "build-artifacts/", "leio-code/releases/", RETIRED_WHEEL_ROOT],
        "ready": ready,
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_artifact_reuse"),
        kind: "doctor".to_string(),
        summary: if ready {
            "artifacts/ reorg is consistent: wheels centralized, consumers repointed, manifest in sync".to_string()
        } else {
            format!("artifact-reuse contract has {} warning(s)", warnings.len())
        },
        confidence: if ready { 0.94 } else { 0.7 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

/// (a) Committed wheels live ONLY under [`WHEEL_HOME`]; none stray in `wheels/`.
///
/// Returns the set of tracked/unignored wheel file names under [`WHEEL_HOME`],
/// used downstream by the manifest cross-check.
fn check_wheel_home(
    root: &Path,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) -> Vec<String> {
    let wheel_dir = root.join(WHEEL_HOME);
    let on_disk_wheels = git_visible_wheel_names(root, &wheel_dir);

    if on_disk_wheels.is_empty() {
        warnings.push(format!(
            "{WHEEL_HOME}/ is missing or contains no committed .whl artifacts"
        ));
    } else {
        evidence.push(EvidenceItem {
            kind: "wheel_home".to_string(),
            path: wheel_dir.display().to_string(),
            line: None,
            detail: format!(
                "{} wheel(s) centralized under {WHEEL_HOME}",
                on_disk_wheels.len()
            ),
        });
    }

    // Stray on-disk wheels under the retired root (excluding the quarantine
    // scratch area) are an immediate inconsistency, present or committed.
    let retired_dir = root.join(RETIRED_WHEEL_ROOT);
    if let Ok(entries) = fs::read_dir(&retired_dir) {
        for entry in entries.filter_map(Result::ok) {
            if let Some(name) = entry.file_name().to_str()
                && name.ends_with(".whl")
            {
                warnings.push(format!(
                        "stray wheel under retired root {RETIRED_WHEEL_ROOT}{name}: move it to {WHEEL_HOME}/"
                    ));
            }
        }
    }

    // Provenance: any git-tracked `.whl` outside the canonical home is drift,
    // even if the on-disk reorg already moved the file.
    if let Some(tracked) = git_tracked_files(root) {
        for path in tracked.iter().filter(|path| path.ends_with(".whl")) {
            let canonical = path.starts_with(WHEEL_HOME);
            let quarantine = path.starts_with(RETIRED_ROOT_QUARANTINE);
            if !canonical && !quarantine {
                warnings.push(format!("committed wheel outside {WHEEL_HOME}/: {path}"));
            }
        }
    }

    on_disk_wheels
}

/// Return wheel names visible to git status: tracked plus untracked/unignored.
///
/// Ignored local wheel caches may exist beside the committed wheelhouse during
/// local build refreshes, but they are not reusable artifacts and must not force
/// `artifacts/manifest.json` churn.
fn git_visible_wheel_names(root: &Path, wheel_dir: &Path) -> Vec<String> {
    let output = Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            WHEEL_HOME,
        ])
        .current_dir(root)
        .output();
    if let Ok(output) = output
        && output.status.success()
    {
        let mut wheels: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| line.ends_with(".whl"))
            .filter_map(|line| Path::new(line).file_name()?.to_str().map(str::to_string))
            .collect();
        wheels.sort();
        return wheels;
    }

    let mut wheels: Vec<String> = fs::read_dir(wheel_dir)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.ends_with(".whl"))
        .collect();
    wheels.sort();
    wheels
}

/// (b) No consumer file references the retired repo-relative `wheels/` path.
fn check_no_consumer_references_retired_root(
    root: &Path,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) {
    for rel in CONSUMER_FILES {
        let path = root.join(rel);
        // Consumers are optional surfaces; absence is not drift.
        let Ok(src) = fs::read_to_string(&path) else {
            continue;
        };

        let mut flagged = false;
        for (idx, line) in src.lines().enumerate() {
            let references_retired_root = line.contains(RETIRED_WHEEL_ROOT)
                || line.contains("$ROOT/wheels")
                || line.contains("${ROOT}/wheels");
            if !references_retired_root {
                continue;
            }
            let tolerated = TOLERATED_CONSUMER_NEEDLES
                .iter()
                .any(|needle| line.contains(needle));
            if tolerated {
                continue;
            }
            warnings.push(format!(
                "{rel}:{} references retired wheel root `{RETIRED_WHEEL_ROOT}` — repoint to {WHEEL_HOME}/",
                idx + 1
            ));
            flagged = true;
        }

        if !flagged {
            evidence.push(EvidenceItem {
                kind: "consumer_repointed".to_string(),
                path: path.display().to_string(),
                line: None,
                detail: format!("{rel} resolves wheels via artifacts/, not the retired root"),
            });
        }
    }
}

/// (c) The phantom `*-fast-node` registry pins are gone from frontend deps.
fn check_phantom_fast_node_pins(
    root: &Path,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) {
    for rel in FRONTEND_PACKAGE_JSON {
        let path = root.join(rel);
        let Ok(src) = fs::read_to_string(&path) else {
            continue;
        };

        let mut any_pin = false;
        for pin in PHANTOM_FAST_NODE_PINS {
            // A registry pin is a JSON dependency key: `"docx-fast-node":`.
            // The scoped workspace name `@example/docx-fast-node` is fine, so
            // require the bare key (opening quote, no `/` before it).
            let needle = format!("\"{pin}\":");
            if let Some(line) = find_line(&src, &needle) {
                warnings.push(format!(
                    "{rel}:{line} still pins phantom registry package `{pin}` — use the @example/{pin} workspace package"
                ));
                any_pin = true;
            }
        }

        if !any_pin {
            evidence.push(EvidenceItem {
                kind: "phantom_pins_removed".to_string(),
                path: path.display().to_string(),
                line: None,
                detail: format!("{rel} carries no phantom *-fast-node registry pins"),
            });
        }
    }
}

/// (d) `artifacts/manifest.json` exists and lists every tracked/unignored wheel.
fn check_manifest_lists_on_disk_wheels(
    root: &Path,
    on_disk_wheels: &[String],
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) {
    let manifest_path = root.join("artifacts/manifest.json");
    let Some(src) = read_text(&manifest_path, warnings) else {
        // read_text already pushed a "failed to read" warning.
        return;
    };

    let manifest: serde_json::Value = match serde_json::from_str(&src) {
        Ok(value) => value,
        Err(err) => {
            warnings.push(format!("artifacts/manifest.json is not valid JSON: {err}"));
            return;
        }
    };

    let listed_paths: Vec<&str> = manifest
        .get("artifacts")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("path").and_then(serde_json::Value::as_str))
                .collect()
        })
        .unwrap_or_default();

    if listed_paths.is_empty() {
        warnings.push(
            "artifacts/manifest.json has no `artifacts[].path` entries to cross-check".to_string(),
        );
        return;
    }

    let mut missing = Vec::new();
    for wheel in on_disk_wheels {
        let expected = format!("{WHEEL_HOME}/{wheel}");
        if !listed_paths.iter().any(|path| *path == expected) {
            missing.push(wheel.clone());
        }
    }

    if missing.is_empty() {
        evidence.push(EvidenceItem {
            kind: "manifest_in_sync".to_string(),
            path: manifest_path.display().to_string(),
            line: None,
            detail: format!(
                "manifest lists all {} tracked/unignored wheel(s) under {WHEEL_HOME}",
                on_disk_wheels.len()
            ),
        });
    } else {
        warnings.push(format!(
            "artifacts/manifest.json is missing {} on-disk wheel(s): {} — regenerate via artifacts/gen-manifest.py",
            missing.len(),
            missing.join(", ")
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    fn unique_tempdir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("artifact_reuse_{tag}_{nanos}"));
        fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(&path, body).expect("write file");
    }

    /// Stage a minimal but consistent reorg: one wheel under the canonical home,
    /// a manifest listing it, repointed consumers, and clean frontend deps.
    fn write_consistent_repo(root: &Path) {
        write(
            root,
            "artifacts/wheels/manylinux/pdf_fast-0.1.0-cp38-abi3-manylinux_2_34_x86_64.whl",
            "PK\x03\x04",
        );
        write(
            root,
            "artifacts/manifest.json",
            r#"{"artifacts":[{"class":"python-wheel","name":"pdf_fast","path":"artifacts/wheels/manylinux/pdf_fast-0.1.0-cp38-abi3-manylinux_2_34_x86_64.whl"}]}"#,
        );
        write(
            root,
            "example-api/Dockerfile",
            "COPY artifacts/wheels/manylinux/ /build/wheels/\nRUN ls /build/wheels/*.whl\n",
        );
        write(
            root,
            "example-ops/package.json",
            "{\n  \"dependencies\": {\n    \"@example/ops-core\": \"workspace:*\"\n  }\n}\n",
        );
    }

    #[test]
    fn consistent_reorg_is_clean() {
        let dir = unique_tempdir("clean");
        write_consistent_repo(&dir);

        let envelope = doctor_artifact_reuse(&dir);

        assert!(
            envelope.warnings.is_empty(),
            "consistent reorg must not warn, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignored_wheel_cache_is_not_manifest_drift() {
        let dir = unique_tempdir("ignored");
        let output = Command::new("git")
            .arg("init")
            .current_dir(&dir)
            .output()
            .expect("run git init");
        assert!(
            output.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        write(
            &dir,
            ".gitignore",
            "artifacts/wheels/manylinux/*-macosx_*.whl\nartifacts/wheels/manylinux/fast_stamp-*.whl\n",
        );
        write_consistent_repo(&dir);
        write(
            &dir,
            "artifacts/wheels/manylinux/pdf_fast-0.1.0-cp38-abi3-macosx_11_0_arm64.whl",
            "PK\x03\x04",
        );
        write(
            &dir,
            "artifacts/wheels/manylinux/fast_stamp-0.1.0-cp38-abi3-manylinux_2_34_x86_64.whl",
            "PK\x03\x04",
        );

        let envelope = doctor_artifact_reuse(&dir);

        assert!(
            envelope.warnings.is_empty(),
            "ignored local wheel caches must not warn, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stray_wheel_under_retired_root_warns() {
        let dir = unique_tempdir("stray");
        write_consistent_repo(&dir);
        // A wheel left behind in the retired root is the regression we guard.
        write(
            &dir,
            "wheels/pdf_fast-0.1.0-cp38-abi3-manylinux_2_34_x86_64.whl",
            "PK\x03\x04",
        );

        let envelope = doctor_artifact_reuse(&dir);

        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("stray wheel under retired root")),
            "expected stray-wheel warning, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn consumer_referencing_retired_root_warns() {
        let dir = unique_tempdir("consumer");
        write_consistent_repo(&dir);
        // A consumer COPYing from the repo-relative retired root is drift; the
        // bare `wheels/` source token must not be tolerated.
        write(
            &dir,
            "scripts/bootstrap-root-python-venv.sh",
            "#!/usr/bin/env bash\nuv pip install wheels/pdf_fast-0.1.0.whl\n",
        );

        let envelope = doctor_artifact_reuse(&dir);

        assert!(
            envelope.warnings.iter().any(|warning| {
                warning.contains("bootstrap-root-python-venv.sh")
                    && warning.contains("retired wheel root")
            }),
            "expected retired-root consumer warning, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn deploy_preflight_referencing_retired_root_warns() {
        let dir = unique_tempdir("deploy-preflight");
        write_consistent_repo(&dir);
        write(
            &dir,
            "deploy/scripts/validate.sh",
            "find $ROOT/wheels -maxdepth 1 -name '*.whl'\n",
        );

        let envelope = doctor_artifact_reuse(&dir);

        assert!(
            envelope.warnings.iter().any(|warning| {
                warning.contains("deploy/scripts/validate.sh")
                    && warning.contains("retired wheel root")
            }),
            "expected deploy-preflight retired-root warning, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn container_internal_wheels_path_is_tolerated() {
        let dir = unique_tempdir("container");
        write_consistent_repo(&dir);
        // The Dockerfile globs `/build/wheels/*.whl` — a path inside the image,
        // not the retired repo root. It must NOT warn.
        let envelope = doctor_artifact_reuse(&dir);
        assert!(
            !envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("Dockerfile")),
            "container-internal wheels path must be tolerated, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn phantom_fast_node_pin_warns() {
        let dir = unique_tempdir("phantom");
        write_consistent_repo(&dir);
        // Resurrect a phantom unscoped pin in a frontend package.json.
        write(
            &dir,
            "example-ops/package.json",
            "{\n  \"dependencies\": {\n    \"docx-fast-node\": \"0.1.0\"\n  }\n}\n",
        );

        let envelope = doctor_artifact_reuse(&dir);

        assert!(
            envelope.warnings.iter().any(|warning| {
                warning.contains("example-ops/package.json") && warning.contains("docx-fast-node")
            }),
            "expected phantom-pin warning, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scoped_workspace_package_is_not_a_phantom_pin() {
        let dir = unique_tempdir("scoped");
        write_consistent_repo(&dir);
        // The legitimate @example/<x>-fast-node workspace package must not trip
        // the bare-name phantom-pin check.
        write(
            &dir,
            "example-ops/package.json",
            "{\n  \"dependencies\": {\n    \"@example/docx-fast-node\": \"workspace:*\"\n  }\n}\n",
        );

        let envelope = doctor_artifact_reuse(&dir);

        assert!(
            !envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("phantom registry package")),
            "scoped workspace package must not warn, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_missing_on_disk_wheel_warns() {
        let dir = unique_tempdir("manifest");
        write_consistent_repo(&dir);
        // A second wheel on disk that the manifest does not list is drift.
        write(
            &dir,
            "artifacts/wheels/manylinux/docx_fast-0.1.0-cp38-abi3-manylinux_2_34_x86_64.whl",
            "PK\x03\x04",
        );

        let envelope = doctor_artifact_reuse(&dir);

        assert!(
            envelope.warnings.iter().any(|warning| {
                warning.contains("manifest.json is missing") && warning.contains("docx_fast")
            }),
            "expected manifest-drift warning, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_manifest_warns() {
        let dir = unique_tempdir("nomanifest");
        write_consistent_repo(&dir);
        fs::remove_file(dir.join("artifacts/manifest.json")).expect("remove manifest");

        let envelope = doctor_artifact_reuse(&dir);

        assert!(
            !envelope.warnings.is_empty(),
            "missing manifest must warn, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
