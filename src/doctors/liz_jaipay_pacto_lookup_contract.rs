//! Liz/Jai-Pay Pacto lookup contract doctor.
//!
//! Drift observed 2026-05-05:
//! - a business no-match from Pacto was surfaced as HTTP 404, which made Liz log
//!   a technical Jai-Pay lookup failure instead of continuing the CPF-not-found
//!   conversation path.
//! - Pacto cobrança must request `EA` + `AT` because Pacto can leave overdue
//!   parcels tagged as EA; the temporal due-date gate decides cobrança.
//! - an explicit unit/gymId from the Liz conversation could be overwritten by
//!   automatic CPF-based unit resolution, causing a known-unit lookup to query a
//!   different Pacto credential and incorrectly report no open installments.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct LizJaiPayPactoLookupContractDoctor;

impl Doctor for LizJaiPayPactoLookupContractDoctor {
    fn name(&self) -> &'static str {
        "liz-jaipay-pacto-lookup-contract"
    }

    fn description(&self) -> &'static str {
        "Checks that Liz and Jai-Pay keep Pacto lookup no-match as a business payload and preserve explicit unit routing."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_liz_jaipay_pacto_lookup_contract(root)
    }
}

struct Check<'a> {
    path: &'a str,
    label: &'a str,
    needles: &'a [&'a str],
    forbidden: &'a [&'a str],
}

pub fn doctor_liz_jaipay_pacto_lookup_contract(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut entities = Vec::new();

    let checks = [
        Check {
            path: "cartridges/jaipay/routes/fitness.py",
            label: "Python Jai-Pay cartridge proxy (routes)",
            needles: &[
                "/collections/overdue/pacto-lookup",
                "gymId",
                "pactoEmpresaId",
                "documentNumber",
            ],
            forbidden: &[],
        },
        Check {
            path: "cartridges/jaipay/utils.py",
            label: "Python Jai-Pay cartridge proxy (utils)",
            needles: &[
                "\"situacoes\": [\"EA\", \"AT\"]",
                "_normalize_pacto_installment",
                "lookupSituacoes",
                "member_not_found",
                "_is_pacto_no_record_error",
                "nenhum cliente encontrado",
                "nenhum registro encontrado",
            ],
            forbidden: &[],
        },
        Check {
            path: "jai-pay/src/app/api/collections/overdue/pacto-lookup/route.ts",
            label: "Next Jai-Pay Pacto lookup route",
            needles: &[
                "emptyLookup",
                "success: false",
                "reason",
                "member_not_found",
                "no_open_installments",
                "installments: []",
                "parcelas: []",
                "totalAmountCents: 0",
                "statusForTechnicalError",
                "status: 400",
                "requestedGymId",
                "resolvedUnit?.unit?.pactoCredentialAlias",
                "resolvedUnit?.unit?.pactoAlias",
                "lookupSituacoes",
                "situacoes: [\"EA\", \"AT\"]",
                "memberPayload",
                "normalizeInstallment",
                "rawParcelas",
            ],
            forbidden: &["{ status: 404 }", "status: 404"],
        },
        Check {
            path: "jai-pay/src/app/api/collections/overdue/pacto-link/route.ts",
            label: "Next Jai-Pay Pacto payment-link route",
            needles: &[
                "requestedGymId",
                "resolvedUnit?.unit?.pactoCredentialAlias",
                "resolvedUnit?.unit?.pactoAlias",
                "resolvedUnit: resolvedUnit?.unit ?? null",
            ],
            forbidden: &[],
        },
        Check {
            path: "jai-pay/src/app/api/collections/overdue/payment-link/route.ts",
            label: "Next Jai-Pay overdue payment-link route",
            needles: &[
                "pactoClienteId",
                "getClient",
                "documentFromPactoClient",
                "resolvePactoCredential",
                "collectionPaymentMethods = [\"pix\", \"credit_card\"]",
                "Either documentNumber or pactoClienteId is required",
            ],
            forbidden: &[],
        },
        Check {
            path: "cartridges/liz_cobranca/agent.py",
            label: "Liz business no-match handling",
            needles: &[
                "lookup.get(\"success\") is False",
                "business no-match",
                "\"reason\": reason",
                "\"sucesso\": False",
                "\"parcelas\": []",
                "\"reason\": \"technical_failure\"",
                "\"technical_failure\": True",
                "temporarily_unavailable",
            ],
            forbidden: &[],
        },
        Check {
            path: "cartridges/liz_cobranca/tasks.py",
            label: "Liz technical-failure user messaging",
            needles: &[
                "debitos.get(\"technical_failure\")",
                "LizTemplates.temporarily_unavailable()",
                "LizTemplates.client_not_found(cpf)",
            ],
            forbidden: &[],
        },
        Check {
            path: "cartridges/liz_cobranca/jaipay_service.py",
            label: "Liz Jai-Pay technical error logging",
            needles: &[
                "Jai-Pay Pacto lookup technical failure",
                "status_code=%s",
                "path=%s",
                "gymId=%s",
                "pactoEmpresaId=%s",
                "reason=%s",
                "pacto_cliente_id",
                "\"pactoClienteId\"",
            ],
            forbidden: &[],
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
                kind: "liz_jaipay_pacto_lookup_missing_file".to_string(),
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
                kind: "liz_jaipay_pacto_lookup_missing_wiring".to_string(),
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
                kind: "liz_jaipay_pacto_lookup_forbidden_404".to_string(),
                path: check.path.to_string(),
                line: None,
                detail: format!("forbidden: {}", forbidden_present.join(", ")),
            });
        }
    }

    entities.push(json!({
        "doctor": "liz-jaipay-pacto-lookup-contract",
        "checks_passed": passed,
        "checks_total": 6,
        "business_no_match_contract": {
            "http_status": 200,
            "success": false,
            "reasons": ["member_not_found", "no_open_installments"],
            "empty_fields": ["parcelas", "installments"],
            "totalAmountCents": 0
        },
        "pacto_no_record_contract": "Pacto provider messages `Nenhum cliente encontrado` and `Nenhum Registro Encontrado` must be normalized to no-match payloads, not leaked as technical 500s.",
        "explicit_unit_contract": "When gymId is provided by the conversation, it must select the Pacto credential before any CPF-based fallback resolver.",
        "payment_link_locator_contract": "The overdue payment-link endpoint accepts either documentNumber or pactoClienteId; pactoClienteId is resolved through the unit-scoped Pacto credential before generating the Jai Pay link.",
        "pacto_status_contract": "Debtor-facing Pacto cobrança must request EA + AT and then apply the temporal due-date gate; future EA installments must not be listed or charged.",
        "technical_error_contract": "Pacto, credential, resolver, and network failures must remain HTTP 5xx and raise JaiPayCollectionsError in Liz service code",
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_liz_jaipay_pacto_lookup_contract"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            "Liz/Jai-Pay Pacto lookup contract keeps no-match as business payload".to_string()
        } else {
            format!(
                "Liz/Jai-Pay Pacto lookup contract drift: {} warning(s)",
                warnings.len()
            )
        },
        confidence: if warnings.is_empty() { 0.94 } else { 0.62 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "tenant": "fitness_exclusive",
            "route": "/v2/jaipay/collections/overdue/pacto-lookup",
            "payment_link_route": "/v2/jaipay/collections/overdue/payment-link",
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "leio_liz_jaipay_pacto_lookup_{}_{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn flags_missing_route_files() {
        let root = temp_root("missing");
        let envelope = doctor_liz_jaipay_pacto_lookup_contract(&root);
        assert!(!envelope.warnings.is_empty());
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("Pacto lookup route"))
        );
    }
}
