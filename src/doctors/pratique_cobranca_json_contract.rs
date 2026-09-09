//! Pratique Cobranca JSON payment-link contract doctor.
//!
//! Production probe on 2026-05-06 confirmed the live JSON feed contained only
//! overdue rows relative to that date, with every loaded row carrying
//! `cpf`, `empresa`, and `link_pagamento`. This doctor keeps the code contract
//! aligned with that operational source of truth.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct PratiqueCobrancaJsonContractDoctor;

impl Doctor for PratiqueCobrancaJsonContractDoctor {
    fn name(&self) -> &'static str {
        "pratique-cobranca-json-contract"
    }

    fn description(&self) -> &'static str {
        "Checks that Pratique Cobranca uses the live JSON feed, requires unit+CPF, and only returns rows with payment links."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_pratique_cobranca_json_contract(root)
    }
}

struct Check<'a> {
    path: &'a str,
    label: &'a str,
    needles: &'a [&'a str],
    forbidden: &'a [&'a str],
}

pub fn doctor_pratique_cobranca_json_contract(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut entities = Vec::new();

    let checks = [
        Check {
            path: "cartridges/pratique_cobranca/service.py",
            label: "Pratique JSON service",
            needles: &[
                "DEFAULT_JSON_URL",
                "https://pratiquetecnologia.com.br/webhooks/caixa/jai.json",
                "DEFAULT_UNIT_CATALOG_PATH",
                "PRATIQUE_COBRANCA_JSON_URL",
                "PRATIQUE_UNIT_CATALOG_PATH",
                "link_pagamento",
                "if not cpf or not empresa or not link",
                "datavencimento",
                "async def list_units",
                "async def resolve_unit",
                "async def lookup",
                "PratiqueUnitCatalogEntry",
                "_resolve_unit_from_catalog",
                "_normalize_text(record.empresa) == normalized_empresa",
                "record.cpf == normalized_cpf",
            ],
            forbidden: &[
                "DEFAULT_EMBED_URL",
                "PRATIQUE_EMBED_URL",
                "_embed_texts",
                "_ensure_unit_embeddings",
            ],
        },
        Check {
            path: "cartridges/pratique_cobranca/agent.py",
            label: "Pratique agent payment flow",
            needles: &[
                "def extract_cpf",
                "\\d{11}",
                "async def list_units",
                "async def resolve_unit",
                "async def lookup",
                "ask_unit",
                "ask_cpf",
                "payment_link",
                "record.link_pagamento",
            ],
            forbidden: &[],
        },
        Check {
            path: "cartridges/pratique_cobranca/flow_endpoint.py",
            label: "Pratique WhatsApp Flow data exchange",
            needles: &[
                "await service.list_units()",
                "selected_unit",
                "cpf",
                "await service.lookup(empresa=empresa, cpf=cpf)",
                "payment_link",
                "record.link_pagamento",
            ],
            forbidden: &[],
        },
        Check {
            path: "cartridges/pratique_cobranca/cartridge.toml",
            label: "Pratique cartridge public Flow endpoint",
            needles: &["[auth]", "public_paths", "\"/flows/data\""],
            forbidden: &[],
        },
        Check {
            path: "cartridges/pratique_cobranca/assets/unit_catalog.json",
            label: "Pratique official unit catalog",
            needles: &[
                "Cidade Nova",
                "Rua Dr. Júlio Otaviano Ferreira",
                "Guarani",
                "Morro Alto",
                "Jequitibá",
                "São Gabriel",
            ],
            forbidden: &[],
        },
        Check {
            path: "cartridges/pratique_cobranca/assets/payment_lookup_flow.json",
            label: "Pratique WhatsApp Flow definition",
            needles: &[
                "selected_unit",
                "unit_options",
                "\"name\": \"cpf\"",
                "payment_link",
            ],
            forbidden: &[],
        },
        Check {
            path: "cartridges/pratique_cobranca/tests/test_service.py",
            label: "Pratique unit resolver regressions",
            needles: &[
                "Rua Dr Julio Otaviano Ferreira",
                "Pratique Gabriela Varela",
                "Pratique Jequitiba",
                "Guarani bairro Tupi",
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
                kind: "pratique_cobranca_json_contract_missing_file".to_string(),
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
                kind: "pratique_cobranca_json_contract_missing_wiring".to_string(),
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
                kind: "pratique_cobranca_json_contract_forbidden_wiring".to_string(),
                path: check.path.to_string(),
                line: None,
                detail: format!("forbidden: {}", forbidden_present.join(", ")),
            });
        }
    }

    entities.push(json!({
        "doctor": "pratique-cobranca-json-contract",
        "checks_passed": passed,
        "checks_total": checks_total,
        "feed_contract": {
            "source_env": "PRATIQUE_COBRANCA_JSON_URL",
            "default_url": "https://pratiquetecnologia.com.br/webhooks/caixa/jai.json",
            "required_fields": ["cpf", "empresa", "link_pagamento"],
            "business_scope": "overdue cobranças only"
        },
        "unit_catalog_contract": {
            "source": "cartridges/pratique_cobranca/assets/unit_catalog.json",
            "local_generator": "cartridges/pratique_cobranca/scripts/build_unit_catalog.py",
            "matching_rule": "unit resolution must use official unit labels, aliases, and addresses before the cobrança feed lookup"
        },
        "conversation_contract": "Pratique must collect unidade and CPF before returning link_pagamento from the JSON row.",
        "production_probe": {
            "date": "2026-05-06",
            "record_count": 5641,
            "unit_count": 80,
            "future_due_count_after_2026_05_06": 0
        }
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_pratique_cobranca_json_contract"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            "Pratique Cobranca JSON payment-link contract is wired".to_string()
        } else {
            format!(
                "Pratique Cobranca JSON payment-link contract drift: {} warning(s)",
                warnings.len()
            )
        },
        confidence: if warnings.is_empty() { 0.93 } else { 0.62 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "tenant": "pratique",
            "agent": "pratique_cobranca",
            "flow_route": "/v2/pratique-cobranca/flows/data",
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "leio_pratique_cobranca_json_contract_{}_{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn flags_missing_contract_files() {
        let root = temp_root("missing");
        let envelope = doctor_pratique_cobranca_json_contract(&root);
        assert!(!envelope.warnings.is_empty());
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("Pratique JSON service"))
        );
    }
}
