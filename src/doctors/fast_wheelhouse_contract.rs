//! fast-wheelhouse-contract doctor.
//!
//! Guards the repository-level PyO3 wheelhouse contract consumed by
//! `example-api`. This is intentionally not cartridge- or app-specific:
//! `artifacts/wheels/manylinux/`, `office-parsers-rs/Dockerfile.pyo3`, and the API Dockerfiles are
//! build/deploy surfaces for the whole Example repo.

use std::fs;
use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct FastWheelhouseContractDoctor;

impl Doctor for FastWheelhouseContractDoctor {
    fn name(&self) -> &'static str {
        "fast-wheelhouse-contract"
    }

    fn description(&self) -> &'static str {
        "Checks the repository-level PyO3 wheelhouse: committed linux/amd64+linux/arm64 wheels, portable CPU compilation, pyo3 builder crate list, API Docker install/import checks, and layout-fast inclusion."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_fast_wheelhouse_contract(root)
    }
}

#[derive(Clone, Copy)]
struct RequiredWheel {
    wheel_prefix: &'static str,
    crate_name: &'static str,
    import_module: &'static str,
    docker_required: bool,
}

const REQUIRED_WHEELS: &[RequiredWheel] = &[
    RequiredWheel {
        wheel_prefix: "pdf_fast",
        crate_name: "pdf-fast-py",
        import_module: "pdf_fast._core",
        docker_required: true,
    },
    RequiredWheel {
        wheel_prefix: "docx_fast",
        crate_name: "docx-fast-py",
        import_module: "docx_fast",
        docker_required: true,
    },
    RequiredWheel {
        wheel_prefix: "xlsx_fast",
        crate_name: "xlsx-fast-py",
        import_module: "xlsx_fast",
        docker_required: true,
    },
    RequiredWheel {
        wheel_prefix: "pptx_fast",
        crate_name: "pptx-fast-py",
        import_module: "pptx_fast",
        docker_required: true,
    },
    RequiredWheel {
        wheel_prefix: "email_fast",
        crate_name: "email-fast-py",
        import_module: "email_fast",
        docker_required: true,
    },
    RequiredWheel {
        wheel_prefix: "layout_fast",
        crate_name: "layout-fast-py",
        import_module: "layout_fast._core",
        docker_required: true,
    },
    RequiredWheel {
        wheel_prefix: "fca_fast",
        crate_name: "fca-fast-py",
        import_module: "fca_fast",
        docker_required: true,
    },
    RequiredWheel {
        wheel_prefix: "mcts_fast",
        crate_name: "mcts-fast-py",
        import_module: "mcts_fast_py._core",
        docker_required: true,
    },
    RequiredWheel {
        wheel_prefix: "align_fast",
        crate_name: "align-fast-py",
        import_module: "align_fast_py._core",
        docker_required: true,
    },
    RequiredWheel {
        wheel_prefix: "gliner_fast",
        crate_name: "gliner-fast-py",
        import_module: "gliner_fast_py._core",
        docker_required: false,
    },
    RequiredWheel {
        wheel_prefix: "flight_contracts_py",
        crate_name: "flight-contracts-py",
        import_module: "flight_contracts_py",
        docker_required: true,
    },
];

const REQUIRED_ARCHES: &[&str] = &["x86_64", "aarch64"];

pub fn doctor_fast_wheelhouse_contract(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();

    let wheel_dir = root.join("artifacts").join("wheels").join("manylinux");
    let wheel_names: Vec<String> = fs::read_dir(&wheel_dir)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();

    if wheel_names.is_empty() {
        warnings.push(
            "artifacts/wheels/manylinux/ is missing or contains no wheel artifacts".to_string(),
        );
    }

    let mut missing_wheels = Vec::new();
    let mut present_modules = Vec::new();
    for wheel in REQUIRED_WHEELS {
        let missing_arches: Vec<&str> = REQUIRED_ARCHES
            .iter()
            .copied()
            .filter(|arch| {
                !wheel_names
                    .iter()
                    .any(|name| name.starts_with(wheel.wheel_prefix) && name.contains(arch))
            })
            .collect();

        if missing_arches.is_empty() {
            present_modules.push(wheel.import_module);
            evidence.push(EvidenceItem {
                kind: "fast_wheel".to_string(),
                path: wheel_dir.display().to_string(),
                line: None,
                detail: format!(
                    "wheelhouse contains linux/amd64 + linux/arm64 wheels for `{}`",
                    wheel.import_module
                ),
            });
        } else {
            missing_wheels.push(format!(
                "{}:{}",
                wheel.wheel_prefix,
                missing_arches.join("/")
            ));
        }
    }

    if !missing_wheels.is_empty() {
        warnings.push(format!(
            "wheelhouse is missing required production PyO3 wheels: {}",
            missing_wheels.join(", ")
        ));
    }

    let pyo3_docker_path = root.join("office-parsers-rs/Dockerfile.pyo3");
    let pyo3_docker = read_text(&pyo3_docker_path, &mut warnings);
    if let Some(src) = pyo3_docker.as_deref() {
        for wheel in REQUIRED_WHEELS {
            if let Some(line) = find_line(src, wheel.crate_name) {
                evidence.push(EvidenceItem {
                    kind: "pyo3_builder_crate".to_string(),
                    path: pyo3_docker_path.display().to_string(),
                    line: Some(line),
                    detail: format!("pyo3 wheel builder includes `{}`", wheel.crate_name),
                });
            } else {
                warnings.push(format!(
                    "office-parsers-rs/Dockerfile.pyo3 does not build `{}`",
                    wheel.crate_name
                ));
            }
        }

        for (needle, detail) in [
            (
                "PYO3_AMD64_RUSTFLAGS=\"-C target-cpu=x86-64\"",
                "linux/amd64 wheels target the portable x86-64 baseline",
            ),
            (
                "PYO3_ARM64_RUSTFLAGS=\"-C target-cpu=generic\"",
                "linux/arm64 wheels target the portable generic baseline",
            ),
            (
                "amd64) export RUSTFLAGS=\"${PYO3_AMD64_RUSTFLAGS}\"",
                "linux/amd64 wheel compilation applies the portable CPU flags",
            ),
            (
                "arm64) export RUSTFLAGS=\"${PYO3_ARM64_RUSTFLAGS}\"",
                "linux/arm64 wheel compilation applies the portable CPU flags",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "pyo3_portable_cpu".to_string(),
                    path: pyo3_docker_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            } else {
                warnings.push(format!(
                    "office-parsers-rs/Dockerfile.pyo3 missing portable CPU invariant `{needle}`"
                ));
            }
        }
    }

    for rel_path in ["example-api/Dockerfile", "example-api/Dockerfile.optimized"] {
        let path = root.join(rel_path);
        let src = read_text(&path, &mut warnings);
        let Some(src) = src.as_deref() else {
            continue;
        };

        for wheel in REQUIRED_WHEELS.iter().filter(|wheel| wheel.docker_required) {
            for needle in [
                format!("\"{}\": \"{}\"", wheel.crate_name, wheel.import_module),
                format!("\"{}\"", wheel.import_module),
            ] {
                if let Some(line) = find_line(src, &needle) {
                    evidence.push(EvidenceItem {
                        kind: "api_docker_fast_module".to_string(),
                        path: path.display().to_string(),
                        line: Some(line),
                        detail: format!("API Dockerfile requires `{}`", wheel.import_module),
                    });
                } else {
                    warnings.push(format!("{rel_path} does not require `{}`", needle));
                }
            }
        }

        for (needle, detail) in [
            (
                "importlib.import_module(name)",
                "API Dockerfile validates native wheels by importing modules",
            ),
            (
                "uv pip install --system --no-cache \"$@\"",
                "API Dockerfile installs bundled wheelhouse artifacts",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "api_docker_wheelhouse_contract".to_string(),
                    path: path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            } else {
                warnings.push(format!(
                    "{rel_path} missing wheelhouse invariant `{needle}`"
                ));
            }
        }

        for stale_needle in ["importlib.machinery", "SITE_ROOTS"] {
            if let Some(line) = find_line(src, stale_needle) {
                warnings.push(format!(
                    "{rel_path}:{line} still checks wheel files instead of importing native modules (`{stale_needle}`)"
                ));
            }
        }
    }

    let ready = warnings.is_empty();
    let entities = vec![json!({
        "doctor": "fast-wheelhouse-contract",
        "wheel_dir": "artifacts/wheels/manylinux",
        "required_arches": REQUIRED_ARCHES,
        "required_modules": REQUIRED_WHEELS.iter().map(|wheel| wheel.import_module).collect::<Vec<_>>(),
        "present_modules": present_modules,
        "missing_wheels": missing_wheels,
        "ready": ready,
    })];

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_fast_wheelhouse_contract"),
        kind: "doctor".to_string(),
        summary: if ready {
            "fast wheelhouse contract is intact for linux/amd64 and linux/arm64".to_string()
        } else {
            format!("fast wheelhouse contract has {} warning(s)", warnings.len())
        },
        confidence: if ready { 0.94 } else { 0.7 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::doctor_fast_wheelhouse_contract;

    #[test]
    fn wheelhouse_contains_repo_fast_modules() {
        // Asserts against the real workspace wheelhouse; absent standalone.
        let Some(root) = crate::test_workspace::workspace_root_with("example-api") else {
            eprintln!("skipped: no workspace repos beside this repo");
            return;
        };
        let envelope = doctor_fast_wheelhouse_contract(&root);

        assert!(
            envelope.warnings.is_empty(),
            "fast wheelhouse contract warnings: {:?}",
            envelope.warnings
        );
        assert!(
            envelope
                .evidence
                .iter()
                .any(|item| item.kind == "pyo3_portable_cpu"),
            "fast wheelhouse contract must prove portable CPU compilation"
        );
    }
}
