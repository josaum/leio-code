//! Plusoft routing contract doctor.
//!
//! Drift observed 2026-05-07:
//! - Plusoft corrected the Assurant URA campaign code for homologation from 44 to 43.
//! - Shell-rendered env files need quoting because the environment-specific campaign
//!   map uses `;` as the separator.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct PlusoftRoutingContractDoctor;

impl Doctor for PlusoftRoutingContractDoctor {
    fn name(&self) -> &'static str {
        "plusoft-routing-contract"
    }

    fn description(&self) -> &'static str {
        "Checks that Assurant Plusoft campaign routing uses the corrected 43 homol / 77 prod JAI-retain contract and shell-safe env rendering."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_plusoft_routing_contract(root)
    }
}

struct Check<'a> {
    path: &'a str,
    label: &'a str,
    needles: &'a [&'a str],
    forbidden: &'a [&'a str],
}

pub fn doctor_plusoft_routing_contract(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut entities = Vec::new();

    let checks = [
        Check {
            path: "deploy/defaults.env",
            label: "deploy default Plusoft routing config",
            needles: &[
                "PLUSOFT_JAI_CAMPAIGN_CODES_BY_ENVIRONMENT=${PLUSOFT_JAI_CAMPAIGN_CODES_BY_ENVIRONMENT:-homol:43;prod:77}",
            ],
            forbidden: &["homol:44;prod:77"],
        },
        Check {
            path: "deploy/secrets.env.example",
            label: "root deploy secret example Plusoft routing config",
            needles: &["PLUSOFT_JAI_CAMPAIGN_CODES_BY_ENVIRONMENT=\"homol:43;prod:77\""],
            forbidden: &["homol:44;prod:77"],
        },
        Check {
            path: "cartridges/plusoft/README.md",
            label: "Plusoft cartridge README",
            needles: &["PLUSOFT_JAI_CAMPAIGN_CODES_BY_ENVIRONMENT=\"homol:43;prod:77\""],
            forbidden: &["homol:44;prod:77"],
        },
        Check {
            path: "example-api/scripts/seed_sara_assurant.py",
            label: "Sara Assurant deploy seed",
            needles: &[
                "\"campaign_codes_homol\": [\"43\"]",
                "\"campaign_codes_prod\": [\"77\"]",
            ],
            forbidden: &["\"campaign_codes_homol\": [\"44\"]"],
        },
        Check {
            path: "cartridges/insurance_agent/seed.py",
            label: "insurance_agent seed",
            needles: &[
                "\"campaign_codes_homol\": [\"43\"]",
                "\"campaign_codes_prod\": [\"77\"]",
            ],
            forbidden: &["\"campaign_codes_homol\": [\"44\"]"],
        },
        Check {
            path: "example-api/example/agents/routing/__init__.py",
            label: "legacy Python routing fallback",
            needles: &[
                "43 homol, 77 prod",
                "config.get(\"campaign_codes_homol\", [\"43\"])",
            ],
            forbidden: &[
                "44 homol, 77 prod",
                "config.get(\"campaign_codes_homol\", [\"44\"])",
            ],
        },
        Check {
            path: "deploy/scripts/render_runtime_env.py",
            label: "runtime env renderer",
            needles: &["import shlex", "shlex.quote(value)", "if value == \"\":"],
            forbidden: &["handle.write(f\"{name}={value}"],
        },
        Check {
            path: "example-api/example/tests/misc/test_render_runtime_env.py",
            label: "runtime env renderer regression test",
            needles: &[
                "test_render_runtime_env_quotes_shell_unsafe_values",
                "homol:43;prod:77",
                "PLUSOFT_JAI_CAMPAIGN_CODES_BY_ENVIRONMENT='homol:43;prod:77'",
            ],
            forbidden: &[],
        },
    ];

    let checks_total = checks.len();
    let mut passed = 0usize;
    for check in checks {
        let full_path = root.join(check.path);
        let mut io_warnings = Vec::new();
        let Some(body) = read_text(&full_path, &mut io_warnings) else {
            warnings.push(format!(
                "{} missing or unreadable: {}",
                check.label, check.path
            ));
            warnings.extend(io_warnings);
            evidence.push(EvidenceItem {
                kind: "plusoft_routing_contract_missing_file".to_string(),
                path: check.path.to_string(),
                line: None,
                detail: check.label.to_string(),
            });
            continue;
        };

        let missing = check
            .needles
            .iter()
            .filter(|needle| !body.contains(**needle))
            .copied()
            .collect::<Vec<_>>();
        let forbidden_present = check
            .forbidden
            .iter()
            .filter(|needle| body.contains(**needle))
            .copied()
            .collect::<Vec<_>>();

        if missing.is_empty() && forbidden_present.is_empty() {
            passed += 1;
            continue;
        }

        if !missing.is_empty() {
            warnings.push(format!(
                "{} drift in {}: missing {}",
                check.label,
                check.path,
                missing.join(", ")
            ));
            evidence.push(EvidenceItem {
                kind: "plusoft_routing_contract_missing_anchor".to_string(),
                path: check.path.to_string(),
                line: None,
                detail: format!("missing: {}", missing.join(", ")),
            });
        }
        if !forbidden_present.is_empty() {
            warnings.push(format!(
                "{} drift in {}: forbidden {}",
                check.label,
                check.path,
                forbidden_present.join(", ")
            ));
            evidence.push(EvidenceItem {
                kind: "plusoft_routing_contract_stale_campaign_code".to_string(),
                path: check.path.to_string(),
                line: None,
                detail: format!("forbidden: {}", forbidden_present.join(", ")),
            });
        }
    }

    entities.push(json!({
        "doctor": "plusoft-routing-contract",
        "checks_passed": passed,
        "checks_total": checks_total,
        "assurant_jai_campaign_codes_by_environment": {
            "homol": [43],
            "prod": [77]
        },
        "env_var": "PLUSOFT_JAI_CAMPAIGN_CODES_BY_ENVIRONMENT",
        "source": "Plusoft email correction from 2026-03-18",
        "shell_rendering_contract": "Values containing semicolon must render with shell-safe quoting because deploy scripts source the rendered .env.",
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_plusoft_routing_contract"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            "Plusoft Assurant routing contract uses 43 homol / 77 prod and shell-safe env rendering"
                .to_string()
        } else {
            format!(
                "Plusoft Assurant routing contract drift: {} warning(s)",
                warnings.len()
            )
        },
        confidence: if warnings.is_empty() { 0.95 } else { 0.64 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "tenant": "assurant",
            "cartridge": "plusoft",
            "route": "/v2/plusoft/campaigns/pending-person/evaluate",
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "leio_plusoft_routing_contract_{}_{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write(root: &Path, relative: &str, body: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn flags_missing_contract_files() {
        let root = temp_root("missing");
        let envelope = doctor_plusoft_routing_contract(&root);
        assert!(!envelope.warnings.is_empty());
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("deploy default Plusoft routing config"))
        );
    }

    #[test]
    fn accepts_corrected_campaign_contract() {
        let root = temp_root("valid");
        write(
            &root,
            "deploy/defaults.env",
            "PLUSOFT_JAI_CAMPAIGN_CODES_BY_ENVIRONMENT=${PLUSOFT_JAI_CAMPAIGN_CODES_BY_ENVIRONMENT:-homol:43;prod:77}\n",
        );
        write(
            &root,
            "deploy/secrets.env.example",
            "PLUSOFT_JAI_CAMPAIGN_CODES_BY_ENVIRONMENT=\"homol:43;prod:77\"\n",
        );
        write(
            &root,
            "cartridges/plusoft/README.md",
            "export PLUSOFT_JAI_CAMPAIGN_CODES_BY_ENVIRONMENT=\"homol:43;prod:77\"\n",
        );
        write(
            &root,
            "example-api/scripts/seed_sara_assurant.py",
            "\"campaign_codes_homol\": [\"43\"]\n\"campaign_codes_prod\": [\"77\"]\n",
        );
        write(
            &root,
            "cartridges/insurance_agent/seed.py",
            "\"campaign_codes_homol\": [\"43\"]\n\"campaign_codes_prod\": [\"77\"]\n",
        );
        write(
            &root,
            "example-api/example/agents/routing/__init__.py",
            "43 homol, 77 prod\nconfig.get(\"campaign_codes_homol\", [\"43\"])\n",
        );
        write(
            &root,
            "deploy/scripts/render_runtime_env.py",
            "import shlex\nif value == \"\":\nshlex.quote(value)\n",
        );
        write(
            &root,
            "example-api/example/tests/misc/test_render_runtime_env.py",
            "test_render_runtime_env_quotes_shell_unsafe_values\nhomol:43;prod:77\nPLUSOFT_JAI_CAMPAIGN_CODES_BY_ENVIRONMENT='homol:43;prod:77'\n",
        );

        let envelope = doctor_plusoft_routing_contract(&root);
        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
    }
}
