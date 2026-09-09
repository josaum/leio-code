//! Plusoft handover payload contract doctor.
//!
//! Asserts the Plusoft handover payload enrichment (Gilderlan request,
//! 2026-05-13, fix #1) is present at the source. Plusoft asked the JAI handover
//! to carry público (cliente/lojista), nome do cliente, resumo de contexto, and
//! id_campanha (resolved via a campaign lookup). The June 2026 escalation added
//! four production gates: Sara must use the OpenAI Responses runtime with the
//! global OpenAI key,
//! same-turn "comunicar sinistro + CPF" must hand off, retained history must
//! carry `phone1`, `resumo_contexto` must be the ordered interaction array
//! Plusoft/Omni can process, the transfer-confirmation turn must be included in
//! the handover history, and successful handoffs must pause Sara with the active
//! business phone scope. Campaign membership is optional enrichment: lookup
//! failure or no active campaign must never block transfer, and no synthetic
//! campaign id may be invented. If any of those anchors regress, Plusoft is
//! blocked from testing or customers can fall into handover limbo.
//!
//! Needle-based (model: `plusoft_routing_contract.rs`): we assert stable string
//! anchors in the three source files that own this contract.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct PlusoftHandoverPayloadContractDoctor;

impl Doctor for PlusoftHandoverPayloadContractDoctor {
    fn name(&self) -> &'static str {
        "plusoft-handover-payload-contract"
    }

    fn description(&self) -> &'static str {
        "Checks that the Plusoft handover payload enrichment (publico, nome_cliente, resumo_contexto, id_campanha + campaign lookup) and its supporting Sara helpers are present at the source."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_plusoft_handover_payload_contract(root)
    }
}

struct Check<'a> {
    path: &'a str,
    label: &'a str,
    needles: &'a [&'a str],
    forbidden: &'a [&'a str],
}

pub fn doctor_plusoft_handover_payload_contract(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut entities = Vec::new();

    let checks = [
        Check {
            path: "example-api/example/agents/tools/builtin/handoff.py",
            label: "Plusoft handover serializer (_handoff_plusoft)",
            needles: &[
                "def _handoff_plusoft",
                "publico",
                "nome_cliente",
                "resumo_contexto",
                "id_campanha",
                "consultar_pessoa_pendente_em_campanhas",
                "Campaign lookup is optional enrichment",
                "dispatching without campaign enrichment",
                "metadados",
            ],
            forbidden: &[
                "\"status\": \"not_eligible\"",
                "\"reason\": \"no_active_campaign\"",
            ],
        },
        Check {
            path: "cartridges/plusoft/types.py",
            label: "PlusoftHandoverRequest handover fields",
            needles: &[
                "publico: Literal[\"cliente\", \"lojista\"]",
                "resumo_contexto: list[dict[str, Any]] | str | None = None",
                "Ordered JAI-side conversation history",
            ],
            forbidden: &[],
        },
        Check {
            path: "example-api/example/agents/tasks.py",
            label: "Sara Plusoft transcript, runtime, and same-turn handoff helpers",
            needles: &[
                "_build_sara_plusoft_transcript",
                "_sara_plusoft_publico_from_text",
                "_sara_plusoft_explicit_assunto_phrase",
                "_sara_plusoft_handover_transfer_message",
                "_append_sara_plusoft_agent_history_message",
                "_sara_plusoft_pause_extra",
                "comunicar sinistro",
                "sara_provider_override",
                "customer_phone\") or normalized_event.get(\"contact_wa_id\") or user_phone",
                "\"business_phone_id\": business_phone_id",
            ],
            forbidden: &["_plusoft_handoff_continues_normal_service"],
        },
        Check {
            path: "example-api/example/tests/api/test_agent_loop_task.py",
            label: "Sara Plusoft email-regression tests",
            needles: &[
                "test_resolve_sara_plusoft_handoff_assunto_detects_explicit_same_turn_phrase",
                "test_sara_runtime_respects_explicit_openai_provider",
                "test_sara_plusoft_retained_history_uses_runtime_transcript_and_final_output",
                "test_sara_live_handoff_without_campaign_id_still_transfers",
                "chat[\"userdata\"][\"phone1\"]",
                "assert \"Vou transferir\" in historico[-1][\"text\"]",
                "pause_kwargs[\"extra\"][\"business_phone_id\"]",
            ],
            forbidden: &["test_sara_live_no_active_campaign_continues_normal_service"],
        },
        Check {
            path: "deploy/profiles/customer_ops_unified.env",
            label: "customer_ops_unified Sara OpenAI deployment env",
            needles: &[
                "EXAMPLE_INFER_HTTP_URL=http://10.142.0.2:8816",
                "EXAMPLE_INFER_FLIGHT_URL=grpc://10.142.0.2:8815",
                "EXAMPLE_VLLM_BASE_URL=http://10.142.0.2:8816/v1",
                "SARA_PROVIDER=openai",
                "SARA_MODEL=gpt-5.4-mini",
                "SARA_OPENAI_BASE_URL=https://api.openai.com/v1",
                "SARA_OPENAI_API_KEY=${OPENAI_API_KEY}",
                "EXAMPLE_AGENT_USE_RESPONSES_API=true",
            ],
            forbidden: &[],
        },
    ];

    let mut passed = 0usize;
    let checks_total = checks.len();
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
                kind: "plusoft_handover_payload_missing_file".to_string(),
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
        let unexpected = check
            .forbidden
            .iter()
            .filter(|needle| body.contains(**needle))
            .copied()
            .collect::<Vec<_>>();

        if missing.is_empty() && unexpected.is_empty() {
            passed += 1;
            continue;
        }

        let mut drift = Vec::new();
        if !missing.is_empty() {
            drift.push(format!("missing {}", missing.join(", ")));
        }
        if !unexpected.is_empty() {
            drift.push(format!("forbidden {}", unexpected.join(", ")));
        }
        warnings.push(format!(
            "{} drift in {}: {}",
            check.label,
            check.path,
            drift.join("; ")
        ));
        evidence.push(EvidenceItem {
            kind: "plusoft_handover_payload_missing_anchor".to_string(),
            path: check.path.to_string(),
            line: None,
            detail: drift.join("; "),
        });
    }

    entities.push(json!({
        "doctor": "plusoft-handover-payload-contract",
        "checks_passed": passed,
        "checks_total": checks_total,
        "enrichment_fields": ["publico", "nome_cliente", "resumo_contexto", "id_campanha", "id_sessao", "phone1", "transfer_message", "business_phone_id"],
        "campaign_lookup": "optional id_campanha enrichment; no active campaign or lookup failure still dispatches handover without a synthetic id",
        "source": "Plusoft / Gilderlan / Laura handover requirements 2026-05-13..2026-06-29",
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_plusoft_handover_payload_contract"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            "Plusoft handover payload, local Sara runtime, same-turn handoff, and history contract present".to_string()
        } else {
            format!(
                "Plusoft handover payload contract drift: {} warning(s)",
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
            "leio_plusoft_handover_payload_{}_{}",
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

    fn write_valid(root: &Path) {
        write(
            root,
            "example-api/example/agents/tools/builtin/handoff.py",
            "async def _handoff_plusoft(conv, phone_line, config, reason):\n    # Campaign lookup is optional enrichment\n    publico = getattr(conv, \"publico\", None)\n    nome_cliente = None\n    resumo_contexto = None\n    id_campanha = None\n    from cartridges.plusoft.client import consultar_pessoa_pendente_em_campanhas\n    logger.warning(\"dispatching without campaign enrichment\")\n    metadados = {}\n    return {\"status\": \"ok\"}\n",
        );
        write(
            root,
            "cartridges/plusoft/types.py",
            "    publico: Literal[\"cliente\", \"lojista\"] | None = None\n    # Ordered JAI-side conversation history\n    resumo_contexto: list[dict[str, Any]] | str | None = None\n",
        );
        write(
            root,
            "example-api/example/agents/tasks.py",
            "def _build_sara_plusoft_transcript():\n    pass\ndef _sara_plusoft_publico_from_text(message_text):\n    return None\ndef _sara_plusoft_explicit_assunto_phrase(message_text):\n    if \"comunicar sinistro\" in message_text:\n        return \"1 Comunicar sinistro\"\ndef _sara_plusoft_handover_transfer_message(cartridge_config):\n    return \"Vou transferir\"\ndef _append_sara_plusoft_agent_history_message(historico, text):\n    return historico\ndef _sara_plusoft_pause_extra():\n    return {\"business_phone_id\": business_phone_id}\nsara_provider_override = \"openai\"\ncustomer_phone = str(normalized_event.get(\"customer_phone\") or normalized_event.get(\"contact_wa_id\") or user_phone or \"\")\n",
        );
        write(
            root,
            "example-api/example/tests/api/test_agent_loop_task.py",
            "def test_resolve_sara_plusoft_handoff_assunto_detects_explicit_same_turn_phrase(): pass\ndef test_sara_runtime_respects_explicit_openai_provider(): pass\ndef test_sara_live_handoff_without_campaign_id_still_transfers(): pass\ndef test_sara_plusoft_retained_history_uses_runtime_transcript_and_final_output():\n    assert chat[\"userdata\"][\"phone1\"]\nassert \"Vou transferir\" in historico[-1][\"text\"]\nassert pause_kwargs[\"extra\"][\"business_phone_id\"]\n",
        );
        write(
            root,
            "deploy/profiles/customer_ops_unified.env",
            "EXAMPLE_INFER_HTTP_URL=http://10.142.0.2:8816\nEXAMPLE_INFER_FLIGHT_URL=grpc://10.142.0.2:8815\nEXAMPLE_VLLM_BASE_URL=http://10.142.0.2:8816/v1\nSARA_PROVIDER=openai\nSARA_MODEL=gpt-5.4-mini\nSARA_OPENAI_BASE_URL=https://api.openai.com/v1\nSARA_OPENAI_API_KEY=${OPENAI_API_KEY}\nEXAMPLE_AGENT_USE_RESPONSES_API=true\n",
        );
    }

    #[test]
    fn flags_missing_enrichment_field() {
        let root = temp_root("drift");
        write_valid(&root);
        // Drop id_campanha + the campaign lookup from the serializer.
        write(
            &root,
            "example-api/example/agents/tools/builtin/handoff.py",
            "async def _handoff_plusoft(conv, phone_line, config, reason):\n    publico = None\n    nome_cliente = None\n    resumo_contexto = None\n    metadados = {}\n",
        );
        let envelope = doctor_plusoft_handover_payload_contract(&root);
        assert!(!envelope.warnings.is_empty());
        assert!(
            envelope.warnings.iter().any(|w| w.contains("id_campanha")
                && w.contains("consultar_pessoa_pendente_em_campanhas")),
            "{:?}",
            envelope.warnings
        );
    }

    #[test]
    fn flags_campaign_eligibility_gate() {
        let root = temp_root("campaign_gate");
        write_valid(&root);
        write(
            &root,
            "example-api/example/agents/tools/builtin/handoff.py",
            "async def _handoff_plusoft(conv, phone_line, config, reason):\n    # Campaign lookup is optional enrichment\n    publico = getattr(conv, \"publico\", None)\n    nome_cliente = None\n    resumo_contexto = None\n    id_campanha = None\n    from cartridges.plusoft.client import consultar_pessoa_pendente_em_campanhas\n    logger.warning(\"dispatching without campaign enrichment\")\n    metadados = {}\n    return {\"status\": \"not_eligible\", \"reason\": \"no_active_campaign\"}\n",
        );
        let envelope = doctor_plusoft_handover_payload_contract(&root);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("forbidden")
                    && warning.contains("no_active_campaign")),
            "{:?}",
            envelope.warnings
        );
    }

    #[test]
    fn accepts_valid_contract() {
        let root = temp_root("valid");
        write_valid(&root);
        let envelope = doctor_plusoft_handover_payload_contract(&root);
        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
    }
}
