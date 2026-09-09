//! Workspace-wide Rust ABI baseline gate (Arrow / ORT / DuckDB / Parquet).
//!
//! AGENTS.md pins Arrow, ONNX Runtime (`ort`), and DuckDB workspace-wide and
//! requires the whole family to move together. `scripts/check-rust-dependency-baseline.py`
//! is the authoritative baseline in CI; this doctor enforces the SAME pins
//! continuously and discoverably so manifest or lockfile drift (e.g. a newly added
//! `arrow-*` dep on a different version) is caught by `leio-code doctor` / `audit`,
//! not only in CI.
//!
//! The baseline versions are read FROM the Python script so there is a single
//! source of truth.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{git_tracked_files, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

const BASELINE_SCRIPT_REL: &str = "scripts/check-rust-dependency-baseline.py";

// Published crates that are versioned in lockstep by apache/arrow-rs. Keep an
// explicit allowlist: independent crates such as arrow-convert and arrow-udf
// use unrelated release lines despite sharing the prefix.
const ARROW_RS_ARROW_PACKAGES: &[&str] = &[
    "arrow",
    "arrow-arith",
    "arrow-array",
    "arrow-avro",
    "arrow-buffer",
    "arrow-cast",
    "arrow-csv",
    "arrow-data",
    "arrow-flight",
    "arrow-ipc",
    "arrow-json",
    "arrow-ord",
    "arrow-pyarrow",
    "arrow-row",
    "arrow-schema",
    "arrow-select",
    "arrow-string",
];

const ARROW_RS_PARQUET_PACKAGES: &[&str] = &[
    "parquet",
    "parquet-geospatial",
    "parquet-variant",
    "parquet-variant-compute",
    "parquet-variant-json",
    "parquet_derive",
];

pub struct RustDependencyBaselineDoctor;

impl Doctor for RustDependencyBaselineDoctor {
    fn name(&self) -> &'static str {
        "rust-dependency-baseline"
    }

    fn description(&self) -> &'static str {
        "Enforces the workspace-wide Arrow/ORT/DuckDB/Parquet ABI baseline (from scripts/check-rust-dependency-baseline.py) across every tracked Cargo.toml, so a drifting or newly added pinned crate is caught continuously, not only in CI."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_rust_dependency_baseline(root)
    }
}

/// Parse the `BASELINES: dict[str, str] = { "crate": "version", ... }` block from
/// the Python baseline script. Single source of truth for the pins.
fn parse_baselines(src: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut in_block = false;
    for line in src.lines() {
        let trimmed = line.trim();
        if !in_block {
            if trimmed.starts_with("BASELINES") && trimmed.contains('{') {
                in_block = true;
            }
            continue;
        }
        if trimmed.starts_with('}') {
            break;
        }
        // Expect: "name": "version",
        let Some((raw_key, raw_val)) = trimmed.split_once(':') else {
            continue;
        };
        let key = raw_key.trim().trim_matches(&['"', ',', ' '][..]);
        let val = raw_val
            .trim()
            .trim_end_matches(',')
            .trim()
            .trim_matches('"');
        if !key.is_empty() && !val.is_empty() && !key.contains(' ') {
            out.insert(key.to_string(), val.to_string());
        }
    }
    out
}

fn equivalent_version(actual: &str, expected: &str) -> bool {
    // A bare Cargo version (e.g. `58.3.0`) is a caret requirement, so it is
    // not equivalent to the exact baseline `=58.3.0`.
    if expected.starts_with('=') {
        return actual == expected;
    }
    actual == expected || actual == format!("={expected}")
}

/// Extract (actual_crate_name, version) from a dependency spec value, honoring
/// `package = "..."` renames. Returns None when no version is present (path/git).
fn dep_name_version(name: &str, spec: &toml::Value) -> Option<(String, String)> {
    match spec {
        toml::Value::String(s) => Some((name.to_string(), s.trim().to_string())),
        toml::Value::Table(t) => {
            let actual = t
                .get("package")
                .and_then(toml::Value::as_str)
                .unwrap_or(name)
                .to_string();
            let version = t.get("version").and_then(toml::Value::as_str)?;
            Some((actual, version.trim().to_string()))
        }
        _ => None,
    }
}

fn collect_dep_tables<'a>(doc: &'a toml::Value, tables: &mut Vec<&'a toml::value::Table>) {
    let keys = ["dependencies", "dev-dependencies", "build-dependencies"];
    if let Some(root) = doc.as_table() {
        for key in keys {
            if let Some(toml::Value::Table(t)) = root.get(key) {
                tables.push(t);
            }
        }
        if let Some(toml::Value::Table(ws)) = root.get("workspace")
            && let Some(toml::Value::Table(t)) = ws.get("dependencies")
        {
            tables.push(t);
        }
        if let Some(toml::Value::Table(targets)) = root.get("target") {
            for target_cfg in targets.values() {
                if let Some(cfg) = target_cfg.as_table() {
                    for key in keys {
                        if let Some(toml::Value::Table(t)) = cfg.get(key) {
                            tables.push(t);
                        }
                    }
                }
            }
        }
    }
}

fn lock_package_baseline<'a>(
    name: &str,
    baselines: &'a BTreeMap<String, String>,
) -> Option<&'a str> {
    if ARROW_RS_ARROW_PACKAGES.contains(&name) {
        return baselines
            .get("arrow")
            .map(String::as_str)
            .map(|version| version.strip_prefix('=').unwrap_or(version));
    }
    if ARROW_RS_PARQUET_PACKAGES.contains(&name) {
        return baselines
            .get("parquet")
            .map(String::as_str)
            .map(|version| version.strip_prefix('=').unwrap_or(version));
    }
    None
}

fn manifest_dependency_baseline<'a>(
    name: &str,
    baselines: &'a BTreeMap<String, String>,
) -> Option<&'a str> {
    baselines.get(name).map(String::as_str).or_else(|| {
        if ARROW_RS_ARROW_PACKAGES.contains(&name) {
            baselines.get("arrow").map(String::as_str)
        } else if ARROW_RS_PARQUET_PACKAGES.contains(&name) {
            baselines.get("parquet").map(String::as_str)
        } else {
            None
        }
    })
}

pub fn doctor_rust_dependency_baseline(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut read_warnings = Vec::new();

    let script_path = root.join(BASELINE_SCRIPT_REL);
    let baselines = match read_text(&script_path, &mut read_warnings) {
        Some(src) => parse_baselines(&src),
        None => BTreeMap::new(),
    };
    warnings.extend(
        read_warnings
            .into_iter()
            .map(|w| format!("[rust-dependency-baseline] {w}")),
    );

    if baselines.is_empty() {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_rust_dependency_baseline"),
            kind: "doctor".to_string(),
            summary: format!(
                "no ABI baselines found (expected {BASELINE_SCRIPT_REL}) — baseline gate skipped"
            ),
            confidence: 0.5,
            entities: vec![json!({ "baseline_source": BASELINE_SCRIPT_REL, "baselines": 0 })],
            evidence,
            warnings,
            meta: Some(json!({ "baseline_source_present": false })),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    let tracked_files = match git_tracked_files(root) {
        Some(files) => files,
        None => {
            warnings.push(
                "[rust-dependency-baseline] failed to enumerate git-tracked files; manifest and lock scans are incomplete"
                    .to_string(),
            );
            Vec::new()
        }
    };
    let manifests: Vec<String> = tracked_files
        .iter()
        .filter(|p| p.ends_with("Cargo.toml"))
        .filter(|p| !p.contains("/target/") && !p.contains("/node_modules/"))
        .cloned()
        .collect();

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut manifests_scanned = 0usize;

    for rel in &manifests {
        let path = root.join(rel);
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(err) => {
                warnings.push(format!(
                    "[rust-dependency-baseline] {rel}: failed to read manifest: {err}"
                ));
                continue;
            }
        };
        let doc: toml::Value = match toml::from_str(&content) {
            Ok(v) => v,
            Err(err) => {
                warnings.push(format!(
                    "[rust-dependency-baseline] {rel}: invalid Cargo.toml: {err}"
                ));
                continue;
            }
        };
        manifests_scanned += 1;

        let mut tables = Vec::new();
        collect_dep_tables(&doc, &mut tables);
        for table in tables {
            for (dep_name, spec) in table {
                let Some((actual, version)) = dep_name_version(dep_name, spec) else {
                    continue;
                };
                let Some(expected) = manifest_dependency_baseline(&actual, &baselines) else {
                    continue;
                };
                if baselines.contains_key(&actual) {
                    seen.insert(actual.clone());
                }
                if !equivalent_version(&version, expected) {
                    warnings.push(format!(
                        "[rust-dependency-baseline] {rel}: {actual}={version:?} must stay on {expected:?}"
                    ));
                    evidence.push(EvidenceItem {
                        kind: "rust_dependency_baseline".to_string(),
                        path: rel.clone(),
                        line: super::utils::find_line(&content, &format!("{actual} ")),
                        detail: format!(
                            "{actual} pinned to {expected} workspace-wide (see {BASELINE_SCRIPT_REL})"
                        ),
                    });
                }
            }
        }
    }

    let missing: Vec<String> = baselines
        .keys()
        .filter(|c| !seen.contains(*c))
        .cloned()
        .collect();
    if !missing.is_empty() {
        warnings.push(format!(
            "[rust-dependency-baseline] baseline crates not found in any tracked manifest: {}",
            missing.join(", ")
        ));
    }

    let lockfiles: Vec<String> = tracked_files
        .iter()
        .filter(|path| path.ends_with("Cargo.lock"))
        .filter(|path| !path.contains("/target/") && !path.contains("/node_modules/"))
        .cloned()
        .collect();
    let mut lockfiles_scanned = 0usize;
    let mut locked_packages_scanned = 0usize;
    for rel in &lockfiles {
        let path = root.join(rel);
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(err) => {
                warnings.push(format!(
                    "[rust-dependency-baseline] {rel}: failed to read lockfile: {err}"
                ));
                continue;
            }
        };
        let document = match toml::from_str::<toml::Value>(&content) {
            Ok(document) => document,
            Err(err) => {
                warnings.push(format!(
                    "[rust-dependency-baseline] {rel}: invalid Cargo.lock: {err}"
                ));
                continue;
            }
        };
        lockfiles_scanned += 1;
        let Some(packages) = document.get("package").and_then(toml::Value::as_array) else {
            continue;
        };
        for package in packages {
            let Some(table) = package.as_table() else {
                continue;
            };
            let Some(name) = table.get("name").and_then(toml::Value::as_str) else {
                continue;
            };
            let Some(expected) = lock_package_baseline(name, &baselines) else {
                continue;
            };
            let version = table
                .get("version")
                .and_then(toml::Value::as_str)
                .unwrap_or_default();
            locked_packages_scanned += 1;
            if version != expected {
                warnings.push(format!(
                    "[rust-dependency-baseline] {rel}: locked {name}={version:?} must stay on {expected:?}"
                ));
                evidence.push(EvidenceItem {
                    kind: "rust_dependency_lock_baseline".to_string(),
                    path: rel.clone(),
                    line: super::utils::find_line(&content, &format!("name = \"{name}\"")),
                    detail: format!(
                        "locked {name} must match the {expected} Arrow/Parquet workspace line"
                    ),
                });
            }
        }
    }

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_rust_dependency_baseline"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked {} baseline crate pins across {} manifests and {} lockfiles, found {} drift warning(s)",
            baselines.len(),
            manifests_scanned,
            lockfiles_scanned,
            warnings.len()
        ),
        confidence: if warnings.is_empty() { 0.97 } else { 0.6 },
        entities: vec![json!({
            "baseline_source": BASELINE_SCRIPT_REL,
            "baselines": baselines,
            "crates_seen": seen.iter().collect::<Vec<_>>(),
            "missing": missing,
            "manifests_scanned": manifests_scanned,
            "lockfiles_scanned": lockfiles_scanned,
            "locked_packages_scanned": locked_packages_scanned,
        })],
        evidence,
        warnings,
        meta: Some(json!({ "baseline_source_present": true })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    fn git_init_add(root: &Path) {
        for args in [vec!["init", "-q"], vec!["add", "-A"]] {
            Command::new("git")
                .arg("-C")
                .arg(root)
                .args(&args)
                .output()
                .expect("git");
        }
    }

    const SCRIPT: &str = r#"
BASELINES: dict[str, str] = {
    "arrow": "=58.3.0",
    "ort": "=2.0.0-rc.12",
}
"#;

    #[test]
    fn parses_baselines_from_script() {
        let b = parse_baselines(SCRIPT);
        assert_eq!(b.get("arrow").map(String::as_str), Some("=58.3.0"));
        assert_eq!(b.get("ort").map(String::as_str), Some("=2.0.0-rc.12"));
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn equivalent_version_enforces_exact_baselines() {
        assert!(equivalent_version("=2.0.0-rc.12", "=2.0.0-rc.12"));
        assert!(!equivalent_version("2.0.0-rc.12", "=2.0.0-rc.12"));
        assert!(equivalent_version("=58.3.0", "=58.3.0"));
        assert!(!equivalent_version("58.3.0", "=58.3.0"));
        assert!(equivalent_version("=58.3.0", "58.3.0"));
        assert!(equivalent_version("58.3.0", "58.3.0"));
        assert!(!equivalent_version("58.2.0", "58.3.0"));
    }

    #[test]
    fn flags_drift_and_passes_clean() {
        let tmp = TempDir::new().expect("tmp");
        let root = tmp.path();
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(root.join(BASELINE_SCRIPT_REL), SCRIPT).unwrap();
        fs::create_dir_all(root.join("good/src")).unwrap();
        fs::write(
            root.join("good/Cargo.toml"),
            "[package]\nname=\"good\"\nversion=\"0.1.0\"\n[dependencies]\narrow = \"=58.3.0\"\nort = { version = \"=2.0.0-rc.12\" }\n",
        )
        .unwrap();
        fs::write(
            root.join("good/Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"arrow-array\"\nversion = \"58.3.0\"\n",
        )
        .unwrap();
        git_init_add(root);
        let clean = doctor_rust_dependency_baseline(root);
        assert!(clean.warnings.is_empty(), "warnings: {:?}", clean.warnings);

        fs::create_dir_all(root.join("bad")).unwrap();
        fs::write(
            root.join("bad/Cargo.toml"),
            "[package]\nname=\"bad\"\nversion=\"0.1.0\"\n[dependencies]\narrow = \"58.2.0\"\narrow-buffer = \"57\"\n",
        )
        .unwrap();
        fs::write(
            root.join("bad/Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"arrow-flight\"\nversion = \"58.2.0\"\n",
        )
        .unwrap();
        git_init_add(root);
        let drift = doctor_rust_dependency_baseline(root);
        assert!(
            drift
                .warnings
                .iter()
                .any(|w| w.contains("arrow=") && w.contains("58.3.0")),
            "warnings: {:?}",
            drift.warnings
        );
        assert!(
            drift
                .warnings
                .iter()
                .any(|warning| warning.contains("arrow-buffer") && warning.contains("=58.3.0")),
            "warnings: {:?}",
            drift.warnings
        );
        assert!(
            drift.warnings.iter().any(
                |warning| warning.contains("locked arrow-flight") && warning.contains("58.3.0")
            ),
            "warnings: {:?}",
            drift.warnings
        );
    }

    #[test]
    fn reports_invalid_tracked_cargo_files_and_ignores_unrelated_arrow_prefixes() {
        let tmp = TempDir::new().expect("tmp");
        let root = tmp.path();
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(root.join(BASELINE_SCRIPT_REL), SCRIPT).unwrap();
        fs::create_dir_all(root.join("good/src")).unwrap();
        fs::write(
            root.join("good/Cargo.toml"),
            "[package]\nname=\"good\"\nversion=\"0.1.0\"\n[dependencies]\narrow = \"=58.3.0\"\nort = \"=2.0.0-rc.12\"\n",
        )
        .unwrap();
        fs::write(
            root.join("good/Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"arrow-convert\"\nversion = \"0.10.0\"\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("broken")).unwrap();
        fs::write(root.join("broken/Cargo.lock"), "not = [valid").unwrap();
        git_init_add(root);

        let result = doctor_rust_dependency_baseline(root);
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("broken/Cargo.lock") && warning.contains("invalid")),
            "warnings: {:?}",
            result.warnings
        );
        assert!(
            !result
                .warnings
                .iter()
                .any(|warning| warning.contains("arrow-convert")),
            "warnings: {:?}",
            result.warnings
        );
    }
}
