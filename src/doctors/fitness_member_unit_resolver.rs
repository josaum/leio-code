//! Fitness member-unit resolver doctor.
//!
//! Drift observed 2026-05-05: Liz could receive a known CPF and still ask the
//! user for a Fitness Exclusive unit because the unit resolution path was
//! prompt-driven instead of grounded in CRM/recovery state and deterministic
//! provider search.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct FitnessMemberUnitResolverDoctor;

impl Doctor for FitnessMemberUnitResolverDoctor {
    fn name(&self) -> &'static str {
        "fitness-member-unit-resolver"
    }

    fn description(&self) -> &'static str {
        "Checks that Fitness Exclusive CPF-to-unit routing stays deterministic: CRM, recovery installments, exact similarity, and Pacto unit sweep."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_fitness_member_unit_resolver(root)
    }
}

struct Check<'a> {
    path: &'a str,
    label: &'a str,
    needles: &'a [&'a str],
}

pub fn doctor_fitness_member_unit_resolver(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut entities = Vec::new();

    let checks = [
        Check {
            path: "jai-pay/src/lib/fitness-member-unit-resolver.ts",
            label: "Jai Pay deterministic unit resolver",
            needles: &[
                "resolveFitnessMemberUnitByDocument",
                "normalizedExactSimilarity",
                "resolveFromCrmMember",
                "resolveFromRecoveryInstallment",
                "resolveFromHumanConfirmedOverride",
                "resolveFromPactoUnitSweep",
                "matchMethod: \"normalized_exact_similarity\"",
                "pactoAlias: \"barbalha\"",
                "pactoCredentialAlias",
                "resolvePactoCredentialAliasForUnit",
                "9f7050377caae2c3a10f00b3ada56ef248e9beb1d293ea21220892fdebbdae69",
            ],
        },
        Check {
            path: "jai-pay/src/app/api/fitness/member-unit/resolve/route.ts",
            label: "Jai Pay resolver API route",
            needles: &["documentNumber", "resolveFitnessMemberUnitByDocument"],
        },
        Check {
            path: "cartridges/jaipay/routes/fitness.py",
            label: "Python Jai Pay cartridge proxy",
            needles: &[
                "/fitness/member-unit/resolve",
                "/api/fitness/member-unit/resolve",
                "documentNumber",
            ],
        },
        Check {
            path: "cartridges/liz_cobranca/jaipay_service.py",
            label: "Liz Jai Pay client",
            needles: &[
                "resolve_fitness_member_unit",
                "/v2/jaipay/fitness/member-unit/resolve",
                "documentNumber",
            ],
        },
        Check {
            path: "cartridges/liz_cobranca/tasks.py",
            label: "Liz deterministic workflow",
            needles: &[
                "_resolve_locator_from_document",
                "resolve_fitness_member_unit",
                "if not locator:",
                "_store_locator(event.session_id, locator)",
            ],
        },
        Check {
            path: "cartridges/liz_cobranca/tests/test_whatsapp_workflow.py",
            label: "Liz workflow regression test",
            needles: &[
                "test_cpf_first_uses_deterministic_document_unit_resolution",
                "99101971387",
                "barbalha",
            ],
        },
    ];

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
                kind: "fitness_member_unit_resolver_missing_file".to_string(),
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
        if missing.is_empty() {
            passed += 1;
        } else {
            warnings.push(format!(
                "{} drift in {}: missing {}",
                check.label,
                check.path,
                missing.join(", ")
            ));
            evidence.push(EvidenceItem {
                kind: "fitness_member_unit_resolver_missing_wiring".to_string(),
                path: check.path.to_string(),
                line: None,
                detail: format!("missing: {}", missing.join(", ")),
            });
        }
    }

    entities.push(json!({
        "doctor": "fitness-member-unit-resolver",
        "checks_passed": passed,
        "checks_total": 6,
        "routing_contract": "CPF unit resolution must be deterministic and grounded before Liz asks the user for unidade",
        "forbidden_pattern": "fuzzy or LLM-selected unit routing",
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_fitness_member_unit_resolver"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            "Fitness member-unit resolver wiring is deterministic and grounded".to_string()
        } else {
            format!(
                "Fitness member-unit resolver drift: {} warning(s)",
                warnings.len()
            )
        },
        confidence: if warnings.is_empty() { 0.94 } else { 0.62 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "tenant": "fitness_exclusive",
            "known_human_confirmed_document_hash": "9f7050377caae2c3a10f00b3ada56ef248e9beb1d293ea21220892fdebbdae69",
            "known_human_confirmed_unit": "barbalha",
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "leio_fitness_member_unit_resolver_{}_{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn flags_missing_resolver() {
        let root = temp_root("missing");
        let envelope = doctor_fitness_member_unit_resolver(&root);
        assert!(!envelope.warnings.is_empty());
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("deterministic unit resolver"))
        );
    }
}
