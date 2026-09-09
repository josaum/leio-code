//! Contract gate for office-parsers Arrow / IPC verification wiring.
//!
//! The parser cores expose Arrow `RecordBatch` APIs and selected `*-node`
//! bindings expose Arrow IPC `Buffer`s. This doctor ensures the repo keeps core
//! Arrow feature gates, stable RecordBatch exporters, IPC writer paths, and real
//! decode tests wired into the operational verification path instead of
//! regressing to shallow "non-empty buffer" smoke.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

struct CoreArrowSurface {
    crate_name: &'static str,
    module_file: Option<&'static str>,
    record_batch_sentinel: &'static str,
    ipc_sentinel: Option<&'static str>,
}

const CORE_ARROW_SURFACES: &[CoreArrowSurface] = &[
    CoreArrowSurface {
        crate_name: "chunk-fast-core",
        module_file: Some("src/arrow.rs"),
        record_batch_sentinel: "chunks_to_record_batch",
        ipc_sentinel: None,
    },
    CoreArrowSurface {
        crate_name: "docx-fast-core",
        module_file: Some("src/arrow.rs"),
        record_batch_sentinel: "docx_document_to_arrow_table_batches",
        ipc_sentinel: Some("parse_docx_to_arrow_ipc"),
    },
    CoreArrowSurface {
        crate_name: "email-fast-core",
        module_file: Some("src/arrow.rs"),
        record_batch_sentinel: "emails_to_record_batch",
        ipc_sentinel: Some("parse_emails_to_arrow_ipc"),
    },
    CoreArrowSurface {
        crate_name: "fca-fast-core",
        module_file: Some("src/arrow.rs"),
        record_batch_sentinel: "memberships_to_record_batch",
        ipc_sentinel: None,
    },
    CoreArrowSurface {
        crate_name: "gliner-core",
        module_file: Some("src/arrow.rs"),
        record_batch_sentinel: "output_to_record_batches",
        ipc_sentinel: None,
    },
    CoreArrowSurface {
        crate_name: "pdf-fast-core",
        module_file: None,
        record_batch_sentinel: "semantic_text_runs_record_batch",
        ipc_sentinel: None,
    },
    CoreArrowSurface {
        crate_name: "pptx-fast-core",
        module_file: Some("src/arrow.rs"),
        record_batch_sentinel: "pptx_document_to_arrow_table_batches",
        ipc_sentinel: Some("parse_pptx_to_arrow_ipc"),
    },
    CoreArrowSurface {
        crate_name: "xlsx-fast-core",
        module_file: Some("src/arrow.rs"),
        record_batch_sentinel: "parse_xlsx_to_arrow_batches",
        ipc_sentinel: Some("parse_xlsx_to_arrow_ipc"),
    },
    CoreArrowSurface {
        crate_name: "web-fast-core",
        module_file: Some("src/arrow.rs"),
        record_batch_sentinel: "web_blocks_to_record_batch",
        ipc_sentinel: Some("write_web_blocks_ipc"),
    },
    CoreArrowSurface {
        crate_name: "xsd-fast-core",
        module_file: Some("src/arrow.rs"),
        record_batch_sentinel: "to_schema_description_batch",
        ipc_sentinel: Some("write_schema_ipc"),
    },
];

pub struct OfficeParsersArrowIpcDoctor;

impl Doctor for OfficeParsersArrowIpcDoctor {
    fn name(&self) -> &'static str {
        "office-parsers-arrow-ipc"
    }

    fn description(&self) -> &'static str {
        "Ensures office-parsers core Arrow RecordBatch surfaces and Node Arrow IPC surfaces stay feature-gated, workspace-pinned, backed by stable export sentinels, and wired into make node-arrow-test / verify-ci --node."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_office_parsers_arrow_ipc(root)
    }
}

fn push_missing(
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
    path: &Path,
    detail: &str,
) {
    warnings.push(format!("[office-parsers-arrow-ipc] {detail}"));
    evidence.push(EvidenceItem {
        kind: "office_parsers_arrow_ipc".to_string(),
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
                kind: "office_parsers_arrow_ipc".to_string(),
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

fn require_any_contains(
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
    path: &Path,
    src: Option<&str>,
    needles: &[&str],
    detail: &str,
) -> bool {
    match src {
        Some(text) => {
            if let Some(needle) = needles.iter().find(|needle| text.contains(**needle)) {
                evidence.push(EvidenceItem {
                    kind: "office_parsers_arrow_ipc".to_string(),
                    path: path.display().to_string(),
                    line: find_line(text, needle),
                    detail: detail.to_string(),
                });
                true
            } else {
                push_missing(warnings, evidence, path, detail);
                false
            }
        }
        None => {
            push_missing(warnings, evidence, path, detail);
            false
        }
    }
}

fn check_name(crate_name: &str, suffix: &str) -> String {
    format!("core_{}_{}", crate_name.replace('-', "_"), suffix)
}

fn core_arrow_surface_checks(
    root: &Path,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) -> Vec<(String, bool)> {
    let opr = root.join("office-parsers-rs");
    let mut checks = Vec::new();
    let mut read_warnings = Vec::new();

    for surface in CORE_ARROW_SURFACES {
        let crate_root = opr.join(surface.crate_name);
        let cargo_path = crate_root.join("Cargo.toml");
        let lib_path = crate_root.join("src/lib.rs");
        let cargo = read_text(&cargo_path, &mut read_warnings);
        let lib = read_text(&lib_path, &mut read_warnings);

        checks.push((
            check_name(surface.crate_name, "arrow_feature_declared"),
            require_any_contains(
                warnings,
                evidence,
                &cargo_path,
                cargo.as_deref(),
                &["arrow = [", "arrow       = ["],
                &format!(
                    "{} must declare an `arrow` feature for columnar interchange",
                    surface.crate_name
                ),
            ),
        ));

        checks.push((
            check_name(surface.crate_name, "feature_gated_api"),
            require_contains(
                warnings,
                evidence,
                &lib_path,
                lib.as_deref(),
                "#[cfg(feature = \"arrow\")]",
                &format!(
                    "{} must feature-gate Arrow APIs behind `feature = \"arrow\"`",
                    surface.crate_name
                ),
            ),
        ));

        if let Some(module_file) = surface.module_file {
            let module_path = crate_root.join(module_file);
            let module = read_text(&module_path, &mut read_warnings);

            checks.push((
                check_name(surface.crate_name, "exports_arrow_module"),
                require_contains(
                    warnings,
                    evidence,
                    &lib_path,
                    lib.as_deref(),
                    "pub mod arrow",
                    &format!(
                        "{} must expose its Arrow module from src/lib.rs",
                        surface.crate_name
                    ),
                ),
            ));

            checks.push((
                check_name(surface.crate_name, "uses_arrow_array"),
                require_contains(
                    warnings,
                    evidence,
                    &cargo_path,
                    cargo.as_deref(),
                    "arrow-array = { workspace = true",
                    &format!(
                        "{} must use workspace-pinned arrow-array",
                        surface.crate_name
                    ),
                ),
            ));

            checks.push((
                check_name(surface.crate_name, "uses_arrow_schema"),
                require_contains(
                    warnings,
                    evidence,
                    &cargo_path,
                    cargo.as_deref(),
                    "arrow-schema = { workspace = true",
                    &format!(
                        "{} must use workspace-pinned arrow-schema",
                        surface.crate_name
                    ),
                ),
            ));

            checks.push((
                check_name(surface.crate_name, "record_batch_api"),
                require_contains(
                    warnings,
                    evidence,
                    &module_path,
                    module.as_deref(),
                    "RecordBatch",
                    &format!(
                        "{} Arrow module must materialize RecordBatch values",
                        surface.crate_name
                    ),
                ),
            ));

            checks.push((
                check_name(surface.crate_name, "stable_export_sentinel"),
                require_contains(
                    warnings,
                    evidence,
                    &module_path,
                    module.as_deref(),
                    surface.record_batch_sentinel,
                    &format!(
                        "{} must keep `{}` as its stable RecordBatch export sentinel",
                        surface.crate_name, surface.record_batch_sentinel
                    ),
                ),
            ));

            if let Some(ipc_sentinel) = surface.ipc_sentinel {
                checks.push((
                    check_name(surface.crate_name, "uses_arrow_ipc"),
                    require_contains(
                        warnings,
                        evidence,
                        &cargo_path,
                        cargo.as_deref(),
                        "arrow-ipc = { workspace = true",
                        &format!(
                            "{} must use workspace-pinned arrow-ipc for IPC output",
                            surface.crate_name
                        ),
                    ),
                ));
                let has_writer = module
                    .as_deref()
                    .map(|text| text.contains("StreamWriter") || text.contains("FileWriter"))
                    .unwrap_or(false);
                if has_writer {
                    evidence.push(EvidenceItem {
                        kind: "office_parsers_arrow_ipc".to_string(),
                        path: module_path.display().to_string(),
                        line: module.as_deref().and_then(|text| {
                            find_line(text, "StreamWriter")
                                .or_else(|| find_line(text, "FileWriter"))
                        }),
                        detail: format!(
                            "{} Arrow module keeps an Arrow IPC writer path",
                            surface.crate_name
                        ),
                    });
                } else {
                    push_missing(
                        warnings,
                        evidence,
                        &module_path,
                        &format!(
                            "{} Arrow module must keep a StreamWriter/FileWriter IPC path",
                            surface.crate_name
                        ),
                    );
                }
                checks.push((check_name(surface.crate_name, "ipc_writer"), has_writer));
                checks.push((
                    check_name(surface.crate_name, "stable_ipc_sentinel"),
                    require_contains(
                        warnings,
                        evidence,
                        &module_path,
                        module.as_deref(),
                        ipc_sentinel,
                        &format!(
                            "{} must keep `{}` as its stable IPC export sentinel",
                            surface.crate_name, ipc_sentinel
                        ),
                    ),
                ));
            }
        } else {
            checks.push((
                check_name(surface.crate_name, "uses_workspace_arrow"),
                require_contains(
                    warnings,
                    evidence,
                    &cargo_path,
                    cargo.as_deref(),
                    "dep:arrow",
                    &format!(
                        "{} must use the workspace-pinned umbrella arrow crate",
                        surface.crate_name
                    ),
                ),
            ));
            checks.push((
                check_name(surface.crate_name, "record_batch_api"),
                require_contains(
                    warnings,
                    evidence,
                    &lib_path,
                    lib.as_deref(),
                    "RecordBatch",
                    &format!(
                        "{} inline Arrow API must materialize RecordBatch values",
                        surface.crate_name
                    ),
                ),
            ));
            checks.push((
                check_name(surface.crate_name, "stable_export_sentinel"),
                require_contains(
                    warnings,
                    evidence,
                    &lib_path,
                    lib.as_deref(),
                    surface.record_batch_sentinel,
                    &format!(
                        "{} must keep `{}` as its stable RecordBatch export sentinel",
                        surface.crate_name, surface.record_batch_sentinel
                    ),
                ),
            ));
        }
    }

    warnings.extend(
        read_warnings
            .into_iter()
            .map(|warning| format!("[office-parsers-arrow-ipc] {warning}")),
    );

    checks
}

pub fn doctor_office_parsers_arrow_ipc(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut read_warnings = Vec::new();

    let makefile_path = root.join("Makefile");
    let verify_ci_path = root.join("scripts/verify-ci.sh");
    let smoke_path = root.join("office-parsers-rs/scripts/smoke-node.mjs");
    let ops_health_path = root.join("scripts/ops/office-parsers-health.sh");
    let pdf_node_path = root.join("office-parsers-rs/pdf-fast-node/src/lib.rs");
    let xsd_node_path = root.join("office-parsers-rs/xsd-fast-node/src/lib.rs");
    let pdf_toml_path = root.join("office-parsers-rs/pdf-fast-node/Cargo.toml");
    let xsd_toml_path = root.join("office-parsers-rs/xsd-fast-node/Cargo.toml");

    let makefile = read_text(&makefile_path, &mut read_warnings);
    let verify_ci = read_text(&verify_ci_path, &mut read_warnings);
    let smoke = read_text(&smoke_path, &mut read_warnings);
    let ops_health = read_text(&ops_health_path, &mut read_warnings);
    let pdf_node = read_text(&pdf_node_path, &mut read_warnings);
    let xsd_node = read_text(&xsd_node_path, &mut read_warnings);
    let pdf_toml = read_text(&pdf_toml_path, &mut read_warnings);
    let xsd_toml = read_text(&xsd_toml_path, &mut read_warnings);
    warnings.extend(
        read_warnings
            .into_iter()
            .map(|warning| format!("[office-parsers-arrow-ipc] {warning}")),
    );

    let mut checks: Vec<(String, bool)> = vec![
        (
            "make_node_arrow_target",
            require_contains(
                &mut warnings,
                &mut evidence,
                &makefile_path,
                makefile.as_deref(),
                "node-arrow-test:",
                "Makefile must expose node-arrow-test for Arrow IPC decode checks",
            ),
        ),
        (
            "verify_ci_node_gate_runs_arrow_tests",
            require_contains(
                &mut warnings,
                &mut evidence,
                &verify_ci_path,
                verify_ci.as_deref(),
                "make node-arrow-test",
                "scripts/verify-ci.sh --node must run make node-arrow-test",
            ),
        ),
        (
            "verify_ci_node_gate_runs_clippy",
            require_contains(
                &mut warnings,
                &mut evidence,
                &verify_ci_path,
                verify_ci.as_deref(),
                "make node-clippy",
                "scripts/verify-ci.sh --node must run make node-clippy",
            ),
        ),
        (
            "make_arrow_core_test_target",
            require_contains(
                &mut warnings,
                &mut evidence,
                &makefile_path,
                makefile.as_deref(),
                "office-parsers-arrow-core-test:",
                "Makefile must expose office-parsers-arrow-core-test for core Arrow feature tests",
            ),
        ),
        (
            "node_smoke_structural_ipc_validator",
            require_contains(
                &mut warnings,
                &mut evidence,
                &smoke_path,
                smoke.as_deref(),
                "assertArrowIpcStream",
                "Node smoke must structurally validate Arrow IPC buffers",
            ),
        ),
        (
            "make_node_clippy_target",
            require_contains(
                &mut warnings,
                &mut evidence,
                &makefile_path,
                makefile.as_deref(),
                "node-clippy:",
                "Makefile must expose node-clippy for office-parsers Node binding lint",
            ),
        ),
        (
            "ops_health_runs_node_arrow_test",
            require_contains(
                &mut warnings,
                &mut evidence,
                &ops_health_path,
                ops_health.as_deref(),
                "node_arrow_test",
                "scripts/ops/office-parsers-health.sh must include node_arrow_test in its JSON ops health checks",
            ),
        ),
        (
            "ops_health_runs_arrow_core_test",
            require_contains(
                &mut warnings,
                &mut evidence,
                &ops_health_path,
                ops_health.as_deref(),
                "arrow_core_test",
                "scripts/ops/office-parsers-health.sh must include arrow_core_test in its JSON ops health checks",
            ),
        ),
        (
            "ops_health_runs_node_clippy",
            require_contains(
                &mut warnings,
                &mut evidence,
                &ops_health_path,
                ops_health.as_deref(),
                "node_clippy",
                "scripts/ops/office-parsers-health.sh must include node_clippy in its JSON ops health checks",
            ),
        ),
        (
            "ops_health_runs_leio_doctors",
            require_contains(
                &mut warnings,
                &mut evidence,
                &ops_health_path,
                ops_health.as_deref(),
                "office-parsers-arrow-ipc",
                "scripts/ops/office-parsers-health.sh must run the LEIO office-parsers-arrow-ipc doctor",
            ),
        ),
        (
            "pdf_real_ipc_decode_test",
            require_contains(
                &mut warnings,
                &mut evidence,
                &pdf_node_path,
                pdf_node.as_deref(),
                "semantic_runs_arrow_ipc_decodes_with_text_column",
                "pdf-fast-node must decode real semantic-runs IPC with Arrow StreamReader",
            ),
        ),
        (
            "xsd_real_ipc_decode_test",
            require_contains(
                &mut warnings,
                &mut evidence,
                &xsd_node_path,
                xsd_node.as_deref(),
                "schema_ipc_bytes_decode_to_expected_description_rows",
                "xsd-fast-node must decode schema IPC with Arrow StreamReader",
            ),
        ),
        (
            "pdf_arrow_ipc_feature_explicit",
            require_contains(
                &mut warnings,
                &mut evidence,
                &pdf_toml_path,
                pdf_toml.as_deref(),
                "\"ipc\"",
                "pdf-fast-node must depend on arrow with the ipc feature explicitly",
            ),
        ),
        (
            "xsd_arrow_ipc_feature_explicit",
            require_contains(
                &mut warnings,
                &mut evidence,
                &xsd_toml_path,
                xsd_toml.as_deref(),
                "\"ipc\"",
                "xsd-fast-node must depend on arrow with the ipc feature explicitly",
            ),
        ),
    ]
    .into_iter()
    .map(|(name, ok)| (name.to_string(), ok))
    .collect();

    checks.extend(core_arrow_surface_checks(
        root,
        &mut warnings,
        &mut evidence,
    ));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_office_parsers_arrow_ipc"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked office-parsers core Arrow + IPC ops verification wiring ({} checks), found {} warnings",
            checks.len(),
            warnings.len()
        ),
        confidence: if warnings.is_empty() { 0.97 } else { 0.6 },
        entities: vec![json!({
            "checks": checks
                .iter()
                .map(|(name, ok)| json!({ "name": name, "ok": ok }))
                .collect::<Vec<_>>(),
            "core_arrow_surfaces": CORE_ARROW_SURFACES
                .iter()
                .map(|surface| surface.crate_name)
                .collect::<Vec<_>>(),
        })],
        evidence,
        warnings,
        meta: Some(
            json!({ "surface": "office-parsers-rs core Arrow RecordBatch + Arrow IPC node bindings" }),
        ),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_core_arrow_surface(root: &Path, surface: &CoreArrowSurface) {
        let crate_root = root.join("office-parsers-rs").join(surface.crate_name);
        let src_root = crate_root.join("src");
        fs::create_dir_all(&src_root).unwrap();

        if surface.module_file.is_some() {
            let mut cargo =
                "[features]\narrow = [\"dep:arrow-array\", \"dep:arrow-schema\"".to_string();
            if surface.ipc_sentinel.is_some() {
                cargo.push_str(", \"dep:arrow-ipc\"");
            }
            cargo.push_str(
                "]\n[dependencies]\narrow-array = { workspace = true, optional = true }\narrow-schema = { workspace = true, optional = true }\n",
            );
            if surface.ipc_sentinel.is_some() {
                cargo.push_str("arrow-ipc = { workspace = true, optional = true }\n");
            }
            fs::write(crate_root.join("Cargo.toml"), cargo).unwrap();
            fs::write(
                src_root.join("lib.rs"),
                "#[cfg(feature = \"arrow\")]\npub mod arrow;\n",
            )
            .unwrap();

            let mut module = format!(
                "use arrow_array::RecordBatch;\npub fn {}() -> Option<RecordBatch> {{ None }}\n",
                surface.record_batch_sentinel
            );
            if let Some(ipc_sentinel) = surface.ipc_sentinel {
                module.push_str(&format!(
                    "use arrow_ipc::writer::StreamWriter;\npub fn {ipc_sentinel}() {{ let _ = \"StreamWriter\"; }}\n",
                ));
            }
            fs::write(src_root.join("arrow.rs"), module).unwrap();
        } else {
            fs::write(
                crate_root.join("Cargo.toml"),
                "[features]\narrow = [\"dep:arrow\"]\n[dependencies]\narrow = { workspace = true, optional = true }\n",
            )
            .unwrap();
            fs::write(
                src_root.join("lib.rs"),
                format!(
                    "#[cfg(feature = \"arrow\")]\nuse arrow::record_batch::RecordBatch;\npub fn {}() -> Option<RecordBatch> {{ None }}\n",
                    surface.record_batch_sentinel
                ),
            )
            .unwrap();
        }
    }

    #[test]
    fn clean_when_arrow_ipc_contract_is_wired() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        fs::create_dir_all(root.join("office-parsers-rs/scripts")).expect("mkdir scripts");
        fs::create_dir_all(root.join("office-parsers-rs/pdf-fast-node/src")).expect("mkdir pdf");
        fs::create_dir_all(root.join("office-parsers-rs/xsd-fast-node/src")).expect("mkdir xsd");
        fs::create_dir_all(root.join("scripts")).expect("mkdir root scripts");
        fs::create_dir_all(root.join("scripts/ops")).expect("mkdir ops");
        fs::write(
            root.join("Makefile"),
            "node-arrow-test:\noffice-parsers-arrow-core-test:\nnode-clippy:\n",
        )
        .expect("write Makefile");
        fs::write(
            root.join("scripts/verify-ci.sh"),
            "make node-arrow-test\nmake node-clippy\n",
        )
        .expect("write verify-ci");
        fs::write(
            root.join("scripts/ops/office-parsers-health.sh"),
            "run_check node_arrow_test make node-arrow-test\nrun_check arrow_core_test make office-parsers-arrow-core-test\nrun_check node_clippy make node-clippy\ncargo run -- doctor office-parsers-arrow-ipc\n",
        )
        .expect("write ops health");
        fs::write(
            root.join("office-parsers-rs/scripts/smoke-node.mjs"),
            "function assertArrowIpcStream() {}\n",
        )
        .expect("write smoke");
        fs::write(
            root.join("office-parsers-rs/pdf-fast-node/src/lib.rs"),
            "fn semantic_runs_arrow_ipc_decodes_with_text_column() {}\n",
        )
        .expect("write pdf lib");
        fs::write(
            root.join("office-parsers-rs/xsd-fast-node/src/lib.rs"),
            "fn schema_ipc_bytes_decode_to_expected_description_rows() {}\n",
        )
        .expect("write xsd lib");
        fs::write(
            root.join("office-parsers-rs/pdf-fast-node/Cargo.toml"),
            "arrow = { workspace = true, features = [\"ffi\", \"ipc\"] }\n",
        )
        .expect("write pdf toml");
        fs::write(
            root.join("office-parsers-rs/xsd-fast-node/Cargo.toml"),
            "arrow = { workspace = true, features = [\"ipc\"] }\n",
        )
        .expect("write xsd toml");
        for surface in CORE_ARROW_SURFACES {
            write_core_arrow_surface(root, surface);
        }

        let env = doctor_office_parsers_arrow_ipc(root);
        assert!(env.warnings.is_empty(), "warnings: {:?}", env.warnings);
    }

    #[test]
    fn flags_missing_verify_ci_wiring() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        fs::create_dir_all(root.join("scripts")).expect("mkdir scripts");
        fs::write(root.join("Makefile"), "node-arrow-test:\n").expect("write Makefile");
        fs::write(root.join("scripts/verify-ci.sh"), "make node-smoke\n").expect("write verify-ci");

        let env = doctor_office_parsers_arrow_ipc(root);
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("verify-ci.sh --node must run make node-arrow-test")),
            "warnings: {:?}",
            env.warnings
        );
    }

    #[test]
    fn flags_missing_core_arrow_module() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        for surface in CORE_ARROW_SURFACES {
            write_core_arrow_surface(root, surface);
        }
        fs::remove_file(root.join("office-parsers-rs/gliner-core/src/arrow.rs")).unwrap();

        let env = doctor_office_parsers_arrow_ipc(root);
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("gliner-core Arrow module must materialize RecordBatch")),
            "warnings: {:?}",
            env.warnings
        );
    }
}
