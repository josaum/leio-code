//! Contract gate for the office-parsers-rs full clippy quality bar.
//!
//! The SOTA bar for `office-parsers-rs` is a whole-workspace clippy run with
//! `-D warnings --all-targets`. This doctor keeps that gate wired so it cannot
//! silently regress:
//!
//! - `make office-parsers-clippy` must exist and run `cargo clippy --workspace`
//!   with `-D warnings`.
//! - `office-parsers-rs/Cargo.toml` must exclude the pristine `vendor/oar-ocr`
//!   crates.io copy from workspace membership, so `--workspace` never lints
//!   third-party code we must not fork (checksum-verified vendor).
//! - The optional `office-parsers-clippy-features` target should lint every
//!   crate that declares an `arrow` feature with `-D warnings`.
//! - Deferred verify-ci and ops-health adoption is reported as informational
//!   evidence, not as a strict warning, until workspace wiring is authorized.

use std::fs;
use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct OfficeParsersClippyGateDoctor;

impl Doctor for OfficeParsersClippyGateDoctor {
    fn name(&self) -> &'static str {
        "office-parsers-clippy-gate"
    }

    fn description(&self) -> &'static str {
        "Ensures the office-parsers-rs default clippy gate stays strict and reports deferred Arrow-feature clippy coverage as informational evidence."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_office_parsers_clippy_gate(root)
    }
}

fn push_missing(
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
    path: &Path,
    detail: &str,
) {
    warnings.push(format!("[office-parsers-clippy-gate] {detail}"));
    evidence.push(EvidenceItem {
        kind: "office_parsers_clippy_gate".to_string(),
        path: path.display().to_string(),
        line: None,
        detail: detail.to_string(),
    });
}

fn require_contains(
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
    path: &Path,
    src: Option<&str>,
    needle: &str,
    detail: &str,
) -> bool {
    match src {
        Some(text) if text.contains(needle) => {
            evidence.push(EvidenceItem {
                kind: "office_parsers_clippy_gate".to_string(),
                path: path.display().to_string(),
                line: find_line(text, needle),
                detail: detail.to_string(),
            });
            true
        }
        _ => {
            push_missing(warnings, evidence, path, detail);
            false
        }
    }
}

fn push_informational(
    informational_findings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
    path: &Path,
    detail: impl Into<String>,
) {
    let detail = detail.into();
    informational_findings.push(detail.clone());
    evidence.push(EvidenceItem {
        kind: "office_parsers_clippy_gate_info".to_string(),
        path: path.display().to_string(),
        line: None,
        detail,
    });
}

fn make_target_block<'a>(makefile: &'a str, target: &str) -> Option<&'a str> {
    let marker = format!("{target}:");
    let start = makefile.find(&marker)?;
    let tail = &makefile[start..];
    let end = tail
        .match_indices('\n')
        .skip(1)
        .find_map(|(offset, _)| {
            let next = &tail[offset + 1..];
            let line = next.lines().next().unwrap_or_default();
            (!line.is_empty() && !line.starts_with([' ', '\t']) && line.ends_with(':'))
                .then_some(offset + 1)
        })
        .unwrap_or(tail.len());
    Some(&tail[..end])
}

fn discover_arrow_feature_crates(
    office_parsers_root: &Path,
    warnings: &mut Vec<String>,
) -> Vec<String> {
    let entries = match fs::read_dir(office_parsers_root) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!(
                "[office-parsers-clippy-gate] failed to list {}: {error}",
                office_parsers_root.display()
            ));
            return Vec::new();
        }
    };
    let mut crates = Vec::new();
    for entry in entries.flatten() {
        let manifest_path = entry.path().join("Cargo.toml");
        if !manifest_path.is_file() {
            continue;
        }
        let source = match fs::read_to_string(&manifest_path) {
            Ok(source) => source,
            Err(error) => {
                warnings.push(format!(
                    "[office-parsers-clippy-gate] failed to read {}: {error}",
                    manifest_path.display()
                ));
                continue;
            }
        };
        let manifest = match toml::from_str::<toml::Value>(&source) {
            Ok(manifest) => manifest,
            Err(error) => {
                warnings.push(format!(
                    "[office-parsers-clippy-gate] failed to parse {}: {error}",
                    manifest_path.display()
                ));
                continue;
            }
        };
        let has_arrow_feature = manifest
            .get("features")
            .and_then(toml::Value::as_table)
            .and_then(|features| features.get("arrow"))
            .and_then(toml::Value::as_array)
            .is_some();
        if !has_arrow_feature {
            continue;
        }
        match manifest
            .get("package")
            .and_then(toml::Value::as_table)
            .and_then(|package| package.get("name"))
            .and_then(toml::Value::as_str)
        {
            Some(name) => crates.push(name.to_string()),
            None => warnings.push(format!(
                "[office-parsers-clippy-gate] {} declares an arrow feature without package.name",
                manifest_path.display()
            )),
        }
    }
    crates.sort();
    crates
}

pub fn doctor_office_parsers_clippy_gate(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut informational_findings = Vec::new();
    let mut read_warnings = Vec::new();

    let makefile_path = root.join("Makefile");
    let cargo_toml_path = root.join("office-parsers-rs/Cargo.toml");
    let verify_ci_path = root.join("scripts/verify-ci.sh");
    let ops_health_path = root.join("scripts/ops/office-parsers-health.sh");

    let makefile = read_text(&makefile_path, &mut read_warnings);
    let cargo_toml = read_text(&cargo_toml_path, &mut read_warnings);
    let verify_ci = read_text(&verify_ci_path, &mut read_warnings);
    let ops_health = read_text(&ops_health_path, &mut read_warnings);
    warnings.extend(
        read_warnings
            .into_iter()
            .map(|warning| format!("[office-parsers-clippy-gate] {warning}")),
    );

    // The Makefile target must both exist and run a real `-D warnings`
    // workspace clippy (guard against it degrading to a no-op echo).
    let make_target_ok = require_contains(
        &mut warnings,
        &mut evidence,
        &makefile_path,
        makefile.as_deref(),
        "office-parsers-clippy:",
        "Makefile must expose the office-parsers-clippy target",
    );
    let make_workspace_clippy_ok = match makefile.as_deref() {
        Some(text) if text.contains("cargo clippy --workspace") && text.contains("-D warnings") => {
            evidence.push(EvidenceItem {
                kind: "office_parsers_clippy_gate".to_string(),
                path: makefile_path.display().to_string(),
                line: find_line(text, "cargo clippy --workspace"),
                detail: "office-parsers-clippy runs cargo clippy --workspace with -D warnings"
                    .to_string(),
            });
            true
        }
        _ => {
            push_missing(
                &mut warnings,
                &mut evidence,
                &makefile_path,
                "office-parsers-clippy must run `cargo clippy --workspace` with `-D warnings`",
            );
            false
        }
    };

    let features_target = makefile
        .as_deref()
        .and_then(|text| make_target_block(text, "office-parsers-clippy-features"));
    let make_features_target_ok = if features_target.is_some() {
        true
    } else {
        push_informational(
            &mut informational_findings,
            &mut evidence,
            &makefile_path,
            "Makefile should expose the office-parsers-clippy-features target (deferred workspace wiring)",
        );
        false
    };
    let make_features_arrow_clippy_ok = match features_target {
        Some(block)
            if block.contains("cargo clippy")
                && block.contains("--features arrow")
                && block.contains("-D warnings") =>
        {
            true
        }
        _ => {
            push_informational(
                &mut informational_findings,
                &mut evidence,
                &makefile_path,
                "office-parsers-clippy-features should run cargo clippy with --features arrow and -D warnings (deferred workspace wiring)",
            );
            false
        }
    };

    let arrow_feature_crates =
        discover_arrow_feature_crates(&root.join("office-parsers-rs"), &mut warnings);
    let mut missing_arrow_crates = Vec::new();
    for crate_name in &arrow_feature_crates {
        let needle = format!("-p {crate_name}");
        if !features_target.is_some_and(|block| block.contains(&needle)) {
            missing_arrow_crates.push(crate_name.clone());
            push_informational(
                &mut informational_findings,
                &mut evidence,
                &makefile_path,
                format!(
                    "office-parsers-clippy-features should cover Arrow crate `{crate_name}` with `{needle}` (deferred workspace wiring)"
                ),
            );
        }
    }
    let make_features_covers_all_arrow_cores = missing_arrow_crates.is_empty();

    let verify_ci_features_ok = verify_ci
        .as_deref()
        .is_some_and(|text| text.contains("make office-parsers-clippy-features"));
    if !verify_ci_features_ok {
        push_informational(
            &mut informational_findings,
            &mut evidence,
            &verify_ci_path,
            "scripts/verify-ci.sh should run make office-parsers-clippy-features (deferred workspace wiring)",
        );
    }
    let ops_health_features_ok = ops_health
        .as_deref()
        .is_some_and(|text| text.contains("office_parsers_clippy_features"));
    if !ops_health_features_ok {
        push_informational(
            &mut informational_findings,
            &mut evidence,
            &ops_health_path,
            "scripts/ops/office-parsers-health.sh should include office_parsers_clippy_features (deferred workspace wiring)",
        );
    }

    let checks = [
        ("make_office_parsers_clippy_target", make_target_ok),
        ("make_runs_workspace_clippy_deny", make_workspace_clippy_ok),
        (
            "make_office_parsers_clippy_features_target",
            make_features_target_ok,
        ),
        (
            "make_features_runs_arrow_clippy_deny",
            make_features_arrow_clippy_ok,
        ),
        (
            "make_features_covers_all_arrow_cores",
            make_features_covers_all_arrow_cores,
        ),
        (
            "cargo_toml_excludes_vendored_oar_ocr",
            require_contains(
                &mut warnings,
                &mut evidence,
                &cargo_toml_path,
                cargo_toml.as_deref(),
                "vendor/oar-ocr",
                "office-parsers-rs/Cargo.toml must exclude vendor/oar-ocr from workspace membership (pristine crates.io vendor must not be linted/forked)",
            ),
        ),
        (
            "verify_ci_runs_office_parsers_clippy",
            require_contains(
                &mut warnings,
                &mut evidence,
                &verify_ci_path,
                verify_ci.as_deref(),
                "make office-parsers-clippy",
                "scripts/verify-ci.sh must run make office-parsers-clippy",
            ),
        ),
        (
            "ops_health_runs_office_parsers_clippy",
            require_contains(
                &mut warnings,
                &mut evidence,
                &ops_health_path,
                ops_health.as_deref(),
                "office_parsers_clippy",
                "scripts/ops/office-parsers-health.sh must include office_parsers_clippy in its JSON ops health checks",
            ),
        ),
        (
            "verify_ci_runs_office_parsers_clippy_features",
            verify_ci_features_ok,
        ),
        (
            "ops_health_runs_office_parsers_clippy_features",
            ops_health_features_ok,
        ),
    ];

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_office_parsers_clippy_gate"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked office-parsers-rs clippy gate wiring ({} checks), found {} warnings",
            checks.len(),
            warnings.len()
        ),
        confidence: if warnings.is_empty() { 0.97 } else { 0.6 },
        entities: vec![json!({
            "checks": checks
                .iter()
                .map(|(name, ok)| json!({ "name": name, "ok": ok }))
                .collect::<Vec<_>>(),
        })],
        evidence,
        warnings,
        meta: Some(json!({
            "surface": "office-parsers-rs full clippy gate",
            "informational_findings": informational_findings,
            "arrow_feature_crates": arrow_feature_crates,
            "missing_arrow_feature_crates": missing_arrow_crates,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_clean(root: &Path) {
        fs::create_dir_all(root.join("office-parsers-rs")).expect("mkdir op");
        fs::create_dir_all(root.join("office-parsers-rs/alpha-fast-core")).expect("mkdir alpha");
        fs::create_dir_all(root.join("office-parsers-rs/beta-fast-core")).expect("mkdir beta");
        fs::create_dir_all(root.join("scripts/ops")).expect("mkdir ops");
        fs::write(
            root.join("Makefile"),
            concat!(
                "office-parsers-clippy:\n",
                "\tcd office-parsers-rs && cargo clippy --workspace --all-targets -- -D warnings\n",
                "office-parsers-clippy-features:\n",
                "\tcd office-parsers-rs && cargo clippy --all-targets --features arrow -p alpha-fast-core -p beta-fast-core -- -D warnings\n",
            ),
        )
        .expect("write Makefile");
        fs::write(
            root.join("office-parsers-rs/Cargo.toml"),
            "[workspace]\nresolver = \"2\"\nexclude = [\"vendor/oar-ocr\"]\n",
        )
        .expect("write Cargo.toml");
        for crate_name in ["alpha-fast-core", "beta-fast-core"] {
            fs::write(
                root.join(format!("office-parsers-rs/{crate_name}/Cargo.toml")),
                format!(
                    "[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\n\n[features]\narrow = []\n"
                ),
            )
            .expect("write member Cargo.toml");
        }
        fs::write(
            root.join("scripts/verify-ci.sh"),
            "make office-parsers-clippy\nmake office-parsers-clippy-features\n",
        )
        .expect("write verify-ci");
        fs::write(
            root.join("scripts/ops/office-parsers-health.sh"),
            concat!(
                "run_check office_parsers_clippy make office-parsers-clippy\n",
                "run_check office_parsers_clippy_features make office-parsers-clippy-features\n",
            ),
        )
        .expect("write ops health");
    }

    fn informational_findings(env: &QueryEnvelope) -> Vec<&str> {
        env.meta
            .as_ref()
            .and_then(|meta| meta.get("informational_findings"))
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect()
    }

    #[test]
    fn clean_when_clippy_gate_is_wired() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        write_clean(root);
        let env = doctor_office_parsers_clippy_gate(root);
        assert!(env.warnings.is_empty(), "warnings: {:?}", env.warnings);
        assert!(
            informational_findings(&env).is_empty(),
            "informational findings: {:?}",
            informational_findings(&env)
        );
    }

    #[test]
    fn flags_missing_vendor_exclusion() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        write_clean(root);
        // Drop the vendor exclusion — the gate would then lint pristine vendor.
        fs::write(
            root.join("office-parsers-rs/Cargo.toml"),
            "[workspace]\nresolver = \"2\"\n",
        )
        .expect("rewrite Cargo.toml");
        let env = doctor_office_parsers_clippy_gate(root);
        assert!(
            env.warnings.iter().any(|w| w.contains("vendor/oar-ocr")),
            "expected vendor-exclusion warning, got: {:?}",
            env.warnings
        );
    }

    #[test]
    fn flags_missing_verify_ci_wiring() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        write_clean(root);
        fs::write(root.join("scripts/verify-ci.sh"), "echo noop\n").expect("rewrite verify-ci");
        let env = doctor_office_parsers_clippy_gate(root);
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("make office-parsers-clippy")),
            "expected verify-ci warning, got: {:?}",
            env.warnings
        );
    }

    #[test]
    fn reports_missing_arrow_core_as_informational() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        write_clean(root);
        let makefile_path = root.join("Makefile");
        let makefile = fs::read_to_string(&makefile_path).expect("read Makefile");
        fs::write(&makefile_path, makefile.replace(" -p beta-fast-core", ""))
            .expect("rewrite Makefile");

        let env = doctor_office_parsers_clippy_gate(root);

        assert!(env.warnings.is_empty(), "warnings: {:?}", env.warnings);
        assert!(
            informational_findings(&env)
                .iter()
                .any(|finding| finding.contains("beta-fast-core")),
            "expected missing crate in informational findings, got: {:?}",
            informational_findings(&env)
        );
    }

    #[test]
    fn reports_missing_features_target_as_informational() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        write_clean(root);
        let makefile_path = root.join("Makefile");
        let makefile = fs::read_to_string(&makefile_path).expect("read Makefile");
        fs::write(
            &makefile_path,
            makefile.replace("office-parsers-clippy-features:", "features-disabled:"),
        )
        .expect("rewrite Makefile");

        let env = doctor_office_parsers_clippy_gate(root);

        assert!(env.warnings.is_empty(), "warnings: {:?}", env.warnings);
        assert!(
            informational_findings(&env)
                .iter()
                .any(|finding| finding.contains("office-parsers-clippy-features target")),
            "expected missing target in informational findings, got: {:?}",
            informational_findings(&env)
        );
    }
}
