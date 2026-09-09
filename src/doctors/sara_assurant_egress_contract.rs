//! Sara Assurant egress contract doctor.
//!
//! Incident class covered:
//! - Meta and Infobip Sara lines share the same agent, tenant, and customer
//!   population, so stale Plusoft handover metadata must not make the Meta
//!   line dispatch as Infobip.
//! - Python receives the gateway request as `queued`, but Rust performs the
//!   real provider call asynchronously. After the provider accepts the message,
//!   Rust must reconcile the Redis outbound marker to `sent`, otherwise ops
//!   keeps seeing a customer-visible message as stuck.
//! - Conversation/session identity must stay scoped to the business phone
//!   line so a Meta-origin turn cannot egress on the Infobip broker line.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct SaraAssurantEgressContractDoctor;

impl Doctor for SaraAssurantEgressContractDoctor {
    fn name(&self) -> &'static str {
        "sara-assurant-egress-contract"
    }

    fn description(&self) -> &'static str {
        "Checks Sara Assurant Meta/Infobip routing, cross-line egress rejection, and async WhatsApp outbound-marker reconciliation."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_sara_assurant_egress_contract(root)
    }
}

struct Check<'a> {
    path: &'a str,
    label: &'a str,
    needles: &'a [&'a str],
    forbidden: &'a [&'a str],
}

pub fn doctor_sara_assurant_egress_contract(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let checks = [
        Check {
            path: "example-api/scripts/seed_sara_assurant.py",
            label: "Sara seed keeps Meta internal and Infobip external-bot real dispatch",
            needles: &[
                "PLUSOFT_META_HANDOVER_CONFIG = {}",
                "PLUSOFT_INFOBIP_HANDOVER_CONFIG = {",
                "\"simulation_mode\": False",
                "\"real_dispatch_enabled\": True",
                "SARA_INFOBIP_PHONE_LINE_ID = os.getenv(\"SARA_INFOBIP_PHONE_LINE_ID\", \"5511987771687\").strip()",
                "channel_provider=\"meta\"",
                "handover_type=\"internal\"",
                "channel_provider=\"infobip\"",
                "handover_type=\"external_bot\"",
                "not meta_route.get(\"handover_config_json\")",
                "infobip_handover.get(\"simulation_mode\") is False and infobip_handover.get(\"real_dispatch_enabled\") is True",
                "all(item.get(\"phone_number_id\") != SARA_INFOBIP_PHONE_LINE_ID for item in tenant_whatsapp)",
            ],
            forbidden: &[
                "PLUSOFT_META_HANDOVER_CONFIG = {**PLUSOFT_HANDOVER_BASE_CONFIG",
                "handover_config=PLUSOFT_HANDOVER_CONFIG",
            ],
        },
        Check {
            path: "example-api/example/hotpath_state.py",
            label: "Redis route projection deletes stale non-external handover_config_json and preserves human-confirmed routes",
            needles: &[
                "\"human_confirmed\"",
                "\"redis_hotpatch\"",
                "def _should_project_handover_config(",
                "normalized_type == \"external_bot\" and bool(handover_config)",
                "if not project_handover_config:",
                "mapping.pop(\"handover_config_json\", None)",
                "hdel(route_key, \"handover_config_json\")",
                "channel_provider=phone_line.channel_provider or \"meta\"",
                "channel_provider = str(phone_line.channel_provider or \"meta\").strip().lower()",
                "if channel_provider != \"meta\":",
            ],
            forbidden: &[],
        },
        Check {
            path: "example-gateway/src/server/handlers/egress.rs",
            label: "Gateway rejects cross-line WhatsApp egress and routes Infobip only from provider/config_json",
            needles: &[
                "whatsapp_identity_phone_mismatch(&intent)",
                "\"reason\": \"business_phone_id_mismatch\"",
                "fn whatsapp_identity_phone_id(intent: &EgressIntent) -> Option<String>",
                "for key in &[\"conversation_id\", \"agent_session_id\", \"session_id\"]",
                "fn route_indicates_infobip(route: &std::collections::HashMap<String, String>) -> bool",
                "for field in &[\"channel_provider\", \"provider\", \"broker\"]",
                "for field in &[\"config_json\"]",
                "!route_indicates_infobip(&meta_with_infobip_handoff)",
            ],
            forbidden: &[
                "for field in &[\"config_json\", \"handover_config_json\"]",
                "for field in &[\"handover_config_json\"]",
            ],
        },
        Check {
            path: "example-gateway/src/server/handlers/egress.rs",
            label: "Gateway reconciles WhatsApp outbound Redis markers after async provider dispatch",
            needles: &[
                "async fn record_whatsapp_outbound_marker(",
                "std::env::var(\"WHATSAPP_OUTBOUND_MARKER_PREFIX\")",
                "fn outbound_marker_keys(intent: &EgressIntent) -> Vec<String>",
                "outbound_metadata_string(intent, \"trigger_message_id\")",
                "outbound_metadata_string(intent, \"agent_session_id\")",
                "outbound_metadata_string(intent, \"session_id\")",
                "persist_egress_event(",
                "record_whatsapp_outbound_marker(\n                            &dispatch_state,\n                            &dispatch_intent,\n                            \"sent\",",
                "record_whatsapp_outbound_marker(\n                            &dispatch_state,\n                            &dispatch_intent,\n                            \"failed\",",
                ".arg(\"provider_message_id\")",
                ".arg(\"customer_visible_sent\")",
                "outbound_marker_keys_match_python_delivery_state_shape",
                "outbound_marker_keys_accept_legacy_session_id_metadata",
            ],
            forbidden: &[],
        },
        Check {
            path: "example-api/example/agents/tasks.py",
            label: "Sara refreshes unknown interactive replies with the canonical menu before the LLM",
            needles: &[
                "def _is_interactive_reply_event(",
                "def _event_text_for_deterministic_routing(",
                "message_text = _event_text_for_deterministic_routing(normalized_event)",
                "if _is_interactive_reply_event(normalized_event):",
                "\"workflow\": \"sara_plusoft_unknown_interactive_reply\"",
                "\"handoff_triggered\": False",
            ],
            forbidden: &[
                "cartridge_config.get(\"legacy_plusoft_menu\")",
                "\"plusoft_handover_menu_aliases\"",
            ],
        },
        Check {
            path: "example-api/scripts/seed_sara_assurant.py",
            label: "Sara seed has no legacy Plusoft button ids",
            needles: &[
                "\"plusoft_menu_interactive_buttons\": True",
                "\"intent_to_assunto\": {",
            ],
            forbidden: &[
                "\"legacy_plusoft_menu\"",
                "\"abrir_sinistro\"",
                "\"status_sinistro\"",
                "\"posicao_sinistro\"",
            ],
        },
        Check {
            path: "cartridges/insurance_agent/seed.py",
            label: "Sara cartridge seed has no legacy Plusoft button ids",
            needles: &[
                "\"plusoft_menu_interactive_buttons\": True",
                "\"intent_to_assunto\": {",
            ],
            forbidden: &[
                "\"legacy_plusoft_menu\"",
                "\"abrir_sinistro\"",
                "\"status_sinistro\"",
                "\"posicao_sinistro\"",
            ],
        },
        Check {
            path: "cartridges/insurance_agent/tools.py",
            label: "Sara domain tools do not expose a simulated Plusoft transfer",
            needles: &["def get_tools() -> list:"],
            forbidden: &["preparar_encaminhamento_plusoft"],
        },
        Check {
            path: "cartridges/insurance_agent/data/sara_system_prompt.md",
            label: "Sara prompt preserves pt-BR accents and delegates Plusoft dispatch to deterministic runtime",
            needles: &[
                "Responda sempre em português brasileiro com acentuação correta",
                "Nunca remova acentos nem use texto ASCII sem acentuação",
                "## Destinos Plusoft controlados pelo runtime",
                "1) Comunicar sinistro",
                "2) Evolução do sinistro",
                "3) Cancelar seguro",
                "Mapeamento operacional Plusoft:",
                "Não simule nem prometa a transferência por iniciativa própria; o runtime controla o disparo",
            ],
            forbidden: &[
                "## Destinos Plusoft em Treinamento",
                "fluxos legados Plusoft",
                "Plusoft legado",
                "1) abrir sinistro",
                "2) acompanhar status",
                "3) cancelamento",
            ],
        },
        Check {
            path: "cartridges/plusoft/router.py",
            label: "Plusoft ingest records successful JAI dispatch and message identity",
            needles: &[
                "queue={} message_id={} type={}",
                "dispatch_target=\"jai_agent\"",
                "dispatch_status=\"sent\"",
                "dispatch_detail=dispatch_result",
            ],
            forbidden: &[],
        },
        Check {
            path: "example-api/example/agents/tools/builtin/handoff.py",
            label: "Plusoft handover outcome captures provider response correlation",
            needles: &[
                "response_body_sha256: str | None = None",
                "response_body_bytes: int | None = None",
                "provider_request_id: str | None = None",
                "response_headers.get(\"x-request-id\")",
                "response_body_sha256=response_body_sha256",
            ],
            forbidden: &[],
        },
        Check {
            path: "docs/deployment/assurant-homol-sara.md",
            label: "Sara homol runbook targets the dedicated Assurant homol routing",
            needles: &[
                "`assurant-homol`",
                "`assurant_customer_support`",
                "**Compose project na VM:** `example-assurant-customer-support`",
                "`SARA_PROVIDER=openai`",
                "`SARA_MODEL=gpt-5.4-mini`",
                "docker compose -p example-assurant-customer-support logs",
            ],
            forbidden: &[
                "Sara WhatsApp + Plusoft homol | `example-platform`",
                "target `customer_ops_unified`",
                "docker compose -p example-platform",
                "`SARA_PROVIDER=gemini`",
            ],
        },
        Check {
            path: "deploy/targets/assurant_customer_support.toml",
            label: "Sara target owns the Infobip number-specific inbound resource",
            needles: &[
                "infobip_webhook_url = \"https://sara-homol.getjai.com/v2/plusoft/ingest\"",
                "infobip_resource_number = \"5511987771687\"",
                "infobip_resource_channel = \"WHATSAPP\"",
                "infobip_resource_format = \"MO_OTT_CONTACT\"",
            ],
            forbidden: &["infobip_webhook_url = \"https://api.getjai.com"],
        },
        Check {
            path: "deploy/scripts/deploy.sh",
            label: "Sara deploy reconciles Infobip number routing after ingress health",
            needles: &[
                "sync-infobip-inbound.py",
                "Synchronize Infobip number-specific inbound routing",
                "--number '$infobip_resource_number'",
                "--webhook-url '$public_ingress_probe_url'",
            ],
            forbidden: &["/ccaas/1/account/configuration"],
        },
        Check {
            path: "deploy/scripts/sync-infobip-inbound.py",
            label: "Infobip reconciler uses Resource Management and verifies readback",
            needles: &[
                "/resource-management/1/inbound-message-configurations",
                "INFOBIP_BASIC_AUTH_USERNAME",
                "INFOBIP_BASIC_AUTH_PASSWORD",
                "\"type\": \"HTTP_FORWARD\"",
                "verified.get(\"forwarding\") != desired",
            ],
            forbidden: &["/ccaas/1/account/configuration", "INFOBIP_API_KEY"],
        },
        Check {
            path: "deploy/profiles/assurant_customer_support.env",
            label: "dedicated Sara runtime profile preserves OpenAI, audio, and excludes Ops warmup",
            needles: &[
                "WHATSAPP_AUDIO_TRANSCRIPTION_ENABLED=true",
                "WHATSAPP_AUDIO_TRANSCRIPTION_PROVIDER=openai",
                "WHATSAPP_AUDIO_TRANSCRIPTION_OPENAI_BASE_URL=https://api.openai.com/v1",
                "WHATSAPP_AUDIO_TRANSCRIPTION_OPENAI_MODEL=gpt-4o-mini-transcribe",
                "WHATSAPP_AUDIO_TRANSCRIPTION_LANGUAGE=pt",
                "WHATSAPP_AUDIO_TRANSCRIPTION_PROMPT='Português brasileiro;",
                "SARA_PROVIDER=openai",
                "SARA_MODEL=gpt-5.4-mini",
                "SARA_OPENAI_BASE_URL=https://api.openai.com/v1",
                "SARA_OPENAI_API_KEY=${OPENAI_API_KEY}",
                "EXAMPLE_AGENT_USE_RESPONSES_API=true",
                "ASSURANT_SERVICE_CENTER_WARMUP_ENABLED=false",
                "EXAMPLE_ALIGN_REQUIRE_MODEL=false",
                "DISABLE_LOCAL_EMBEDDINGS=true",
                "EXAMPLE_API_FLIGHT_HOST_PORT=18815",
            ],
            forbidden: &["SARA_PROVIDER=gemini"],
        },
        Check {
            path: "cartridges/assurant/router.py",
            label: "Assurant router honors the Service Center warmup boundary",
            needles: &[
                "def _service_center_warmup_enabled()",
                "ASSURANT_SERVICE_CENTER_WARMUP_ENABLED",
                "if not _service_center_warmup_enabled():",
                "return {\"status\": \"disabled\"}",
            ],
            forbidden: &[],
        },
        Check {
            path: "deploy/scripts/deploy.sh",
            label: "deploy repairs shared model-cache ownership after root sidecars",
            needles: &[
                "Repairing shared model-cache permissions for appuser",
                "sudo chown -R 1000:27",
                "sudo chmod -R u+rwX,g+rwX",
            ],
            forbidden: &[],
        },
    ];

    run_checks(root, started, &checks)
}

fn run_checks(root: &Path, started: Instant, checks: &[Check<'_>]) -> QueryEnvelope {
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut entities = Vec::new();
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
                kind: "sara_assurant_egress_missing_file".to_string(),
                path: check.path.to_string(),
                line: None,
                detail: check.label.to_string(),
            });
            entities.push(json!({
                "doctor": "sara-assurant-egress-contract",
                "path": check.path,
                "label": check.label,
                "passed": false,
                "missing_file": true,
            }));
            continue;
        };

        let missing = check
            .needles
            .iter()
            .filter(|needle| !body.contains(**needle))
            .copied()
            .collect::<Vec<_>>();
        let forbidden_hits = check
            .forbidden
            .iter()
            .filter(|needle| body.contains(**needle))
            .copied()
            .collect::<Vec<_>>();

        let check_passed = missing.is_empty() && forbidden_hits.is_empty();
        if check_passed {
            passed += 1;
            if let Some(first_needle) = check.needles.first() {
                evidence.push(EvidenceItem {
                    kind: "sara_assurant_egress_contract".to_string(),
                    path: check.path.to_string(),
                    line: find_line(&body, first_needle),
                    detail: check.label.to_string(),
                });
            }
        }

        if !missing.is_empty() {
            warnings.push(format!(
                "{} drift in {}: missing {}",
                check.label,
                check.path,
                missing.join(", ")
            ));
        }
        if !forbidden_hits.is_empty() {
            warnings.push(format!(
                "{} drift in {}: forbidden {}",
                check.label,
                check.path,
                forbidden_hits.join(", ")
            ));
        }

        entities.push(json!({
            "doctor": "sara-assurant-egress-contract",
            "path": check.path,
            "label": check.label,
            "passed": check_passed,
            "required_anchors": check.needles.len(),
            "forbidden_anchors": check.forbidden.len(),
            "missing": missing,
            "forbidden_hits": forbidden_hits,
        }));
    }

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_sara_assurant_egress_contract"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked Sara Assurant Meta/Infobip egress contract, found {} warnings ({passed}/{total} surfaces passed)",
            warnings.len(),
            total = checks.len()
        ),
        confidence: if warnings.is_empty() { 0.95 } else { 0.62 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "doctor": "sara-assurant-egress-contract",
            "tenant": "assurant",
            "agent": "sara_assurant",
            "meta_phone_line_id": "108079528970614",
            "infobip_phone_line_id": "5511987771687",
            "checks_passed": passed,
            "checks_total": checks.len(),
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temp_repo(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-sara-egress-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn write_good_repo(root: &Path) {
        write(
            root,
            "example-api/scripts/seed_sara_assurant.py",
            r#"
PLUSOFT_META_HANDOVER_CONFIG = {}
PLUSOFT_INFOBIP_HANDOVER_CONFIG = {
    "simulation_mode": False,
    "real_dispatch_enabled": True,
}
SARA_INFOBIP_PHONE_LINE_ID = os.getenv("SARA_INFOBIP_PHONE_LINE_ID", "5511987771687").strip()
PhoneLineCRUD.create(channel_provider="meta", handover_type="internal")
PhoneLineCRUD.create(channel_provider="infobip", handover_type="external_bot")
_require_seed(not meta_route.get("handover_config_json"), "x")
_require_seed(infobip_handover.get("simulation_mode") is False and infobip_handover.get("real_dispatch_enabled") is True, "x")
_require_seed(all(item.get("phone_number_id") != SARA_INFOBIP_PHONE_LINE_ID for item in tenant_whatsapp), "x")
SARA_CONFIG = {
    "plusoft_menu_interactive_buttons": True,
    "intent_to_assunto": {"sinistro": "1 Comunicar sinistro"},
}
"#,
        );
        write(
            root,
            "cartridges/insurance_agent/seed.py",
            r#"
SARA_CONFIG = {
    "plusoft_menu_interactive_buttons": True,
    "intent_to_assunto": {"sinistro": "1 Comunicar sinistro"},
}
"#,
        );
        write(
            root,
            "example-api/example/hotpath_state.py",
            r#"
_HUMAN_CONFIRMED_PROJECTION_SOURCES = {"human_confirmed", "redis_hotpatch"}
channel_provider=phone_line.channel_provider or "meta"
channel_provider = str(phone_line.channel_provider or "meta").strip().lower()
if channel_provider != "meta":
    pass
def _should_project_handover_config(handover_type, handover_config):
    normalized_type = str(handover_type or "internal").strip().lower()
    return normalized_type == "external_bot" and bool(handover_config)
project_handover_config = _should_project_handover_config(handover_type, handover_config)
if not project_handover_config:
    mapping.pop("handover_config_json", None)
    hdel(route_key, "handover_config_json")
"#,
        );
        write(
            root,
            "example-gateway/src/server/handlers/egress.rs",
            r#"
fn route_indicates_infobip(route: &std::collections::HashMap<String, String>) -> bool {
    for field in &["channel_provider", "provider", "broker"] {}
    for field in &["config_json"] {}
    false
}
fn whatsapp_identity_phone_id(intent: &EgressIntent) -> Option<String> {
    for key in &["conversation_id", "agent_session_id", "session_id"] {}
    None
}
fn x() {
    whatsapp_identity_phone_mismatch(&intent);
    serde_json::json!({"reason": "business_phone_id_mismatch"});
    assert!(!route_indicates_infobip(&meta_with_infobip_handoff));
    persist_egress_event();
    record_whatsapp_outbound_marker(
                            &dispatch_state,
                            &dispatch_intent,
                            "sent",
    );
    record_whatsapp_outbound_marker(
                            &dispatch_state,
                            &dispatch_intent,
                            "failed",
    );
}
async fn record_whatsapp_outbound_marker() {
    std::env::var("WHATSAPP_OUTBOUND_MARKER_PREFIX");
    outbound_metadata_string(intent, "trigger_message_id");
    outbound_metadata_string(intent, "agent_session_id");
    outbound_metadata_string(intent, "session_id");
    cmd.arg("provider_message_id");
    cmd.arg("customer_visible_sent");
}
fn outbound_marker_keys(intent: &EgressIntent) -> Vec<String> { vec![] }
fn outbound_marker_keys_match_python_delivery_state_shape() {}
fn outbound_marker_keys_accept_legacy_session_id_metadata() {}
"#,
        );
        write(
            root,
            "example-api/example/agents/tasks.py",
            r#"
def _event_text_for_deterministic_routing(event):
    pass
def _is_interactive_reply_event(event):
    pass
message_text = _event_text_for_deterministic_routing(normalized_event)
if _is_interactive_reply_event(normalized_event):
    result = {"workflow": "sara_plusoft_unknown_interactive_reply", "handoff_triggered": False}
"#,
        );
        write(
            root,
            "cartridges/insurance_agent/tools.py",
            "def get_tools() -> list:\n    return []\n",
        );
        write(
            root,
            "cartridges/insurance_agent/data/sara_system_prompt.md",
            r#"
Responda sempre em português brasileiro com acentuação correta
Nunca remova acentos nem use texto ASCII sem acentuação
## Destinos Plusoft controlados pelo runtime
1) Comunicar sinistro
2) Evolução do sinistro
3) Cancelar seguro
Mapeamento operacional Plusoft:
Não simule nem prometa a transferência por iniciativa própria; o runtime controla o disparo
"#,
        );
        write(
            root,
            "cartridges/plusoft/router.py",
            r#"
logger.warning("queue={} message_id={} type={}")
_update_ingest_dispatch_result(
    dispatch_target="jai_agent",
    dispatch_status="sent",
    dispatch_detail=dispatch_result,
)
"#,
        );
        write(
            root,
            "example-api/example/agents/tools/builtin/handoff.py",
            r#"
response_body_sha256: str | None = None
response_body_bytes: int | None = None
provider_request_id: str | None = None
response_headers.get("x-request-id")
response_body_sha256=response_body_sha256
"#,
        );
        write(
            root,
            "docs/deployment/assurant-homol-sara.md",
            r#"
`assurant-homol`
`assurant_customer_support`
**Compose project na VM:** `example-assurant-customer-support`
`SARA_PROVIDER=openai`
`SARA_MODEL=gpt-5.4-mini`
docker compose -p example-assurant-customer-support logs
"#,
        );
        write(
            root,
            "deploy/profiles/assurant_customer_support.env",
            r#"
WHATSAPP_AUDIO_TRANSCRIPTION_ENABLED=true
WHATSAPP_AUDIO_TRANSCRIPTION_PROVIDER=openai
WHATSAPP_AUDIO_TRANSCRIPTION_OPENAI_BASE_URL=https://api.openai.com/v1
WHATSAPP_AUDIO_TRANSCRIPTION_OPENAI_MODEL=gpt-4o-mini-transcribe
WHATSAPP_AUDIO_TRANSCRIPTION_LANGUAGE=pt
WHATSAPP_AUDIO_TRANSCRIPTION_PROMPT='Português brasileiro; preserve nomes próprios, CPF, CNPJ e termos de seguro.'
SARA_PROVIDER=openai
SARA_MODEL=gpt-5.4-mini
SARA_OPENAI_BASE_URL=https://api.openai.com/v1
SARA_OPENAI_API_KEY=${OPENAI_API_KEY}
EXAMPLE_AGENT_USE_RESPONSES_API=true
ASSURANT_SERVICE_CENTER_WARMUP_ENABLED=false
EXAMPLE_ALIGN_REQUIRE_MODEL=false
DISABLE_LOCAL_EMBEDDINGS=true
EXAMPLE_API_FLIGHT_HOST_PORT=18815
"#,
        );
        write(
            root,
            "cartridges/assurant/router.py",
            r#"
def _service_center_warmup_enabled():
    return os.getenv("ASSURANT_SERVICE_CENTER_WARMUP_ENABLED", "true")

if not _service_center_warmup_enabled():
    return

if not _service_center_warmup_enabled():
    return {"status": "disabled"}
"#,
        );
        write(
            root,
            "deploy/targets/assurant_customer_support.toml",
            r#"
infobip_webhook_url = "https://sara-homol.getjai.com/v2/plusoft/ingest"
infobip_resource_number = "5511987771687"
infobip_resource_channel = "WHATSAPP"
infobip_resource_format = "MO_OTT_CONTACT"
"#,
        );
        write(
            root,
            "deploy/scripts/deploy.sh",
            r#"
sync_to_vm "$ROOT/deploy/scripts/sync-infobip-inbound.py" "/opt/example/"
step "Synchronize Infobip number-specific inbound routing"
python3 ./sync-infobip-inbound.py \
  --number '$infobip_resource_number' \
  --webhook-url '$public_ingress_probe_url'
echo 'Repairing shared model-cache permissions for appuser'
sudo chown -R 1000:27 "$model_cache_host_dir"
sudo chmod -R u+rwX,g+rwX "$model_cache_host_dir"
"#,
        );
        write(
            root,
            "deploy/scripts/sync-infobip-inbound.py",
            r#"
RESOURCE_PATH = "/resource-management/1/inbound-message-configurations"
username = os.getenv("INFOBIP_BASIC_AUTH_USERNAME", "").strip()
password = os.getenv("INFOBIP_BASIC_AUTH_PASSWORD", "").strip()
desired = {"type": "HTTP_FORWARD"}
if verified is None or verified.get("forwarding") != desired:
    raise RuntimeError()
"#,
        );
    }

    #[test]
    fn good_contract_passes() {
        let root = temp_repo("good");
        write_good_repo(&root);
        let env = doctor_sara_assurant_egress_contract(&root);
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_meta_handover_config_is_flagged() {
        let root = temp_repo("stale-meta");
        write_good_repo(&root);
        fs::write(
            root.join("example-api/scripts/seed_sara_assurant.py"),
            r#"
PLUSOFT_META_HANDOVER_CONFIG = {**PLUSOFT_HANDOVER_BASE_CONFIG}
PLUSOFT_INFOBIP_HANDOVER_CONFIG = {
    "simulation_mode": False,
    "real_dispatch_enabled": True,
}
SARA_INFOBIP_PHONE_LINE_ID = os.getenv("SARA_INFOBIP_PHONE_LINE_ID", "5511987771687").strip()
PhoneLineCRUD.create(channel_provider="meta", handover_type="internal")
PhoneLineCRUD.create(channel_provider="infobip", handover_type="external_bot")
_require_seed(not meta_route.get("handover_config_json"), "x")
_require_seed(infobip_handover.get("simulation_mode") is False and infobip_handover.get("real_dispatch_enabled") is True, "x")
_require_seed(all(item.get("phone_number_id") != SARA_INFOBIP_PHONE_LINE_ID for item in tenant_whatsapp), "x")
"#,
        )
        .unwrap();
        let env = doctor_sara_assurant_egress_contract(&root);
        assert!(
            env.warnings
                .iter()
                .any(|warning| warning.contains("forbidden")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_async_marker_reconciliation_is_flagged() {
        let root = temp_repo("marker");
        write_good_repo(&root);
        let path = root.join("example-gateway/src/server/handlers/egress.rs");
        let body = fs::read_to_string(&path).unwrap().replace(
            r#"    record_whatsapp_outbound_marker(
                            &dispatch_state,
                            &dispatch_intent,
                            "sent",
    );
"#,
            "",
        );
        fs::write(path, body).unwrap();
        let env = doctor_sara_assurant_egress_contract(&root);
        assert!(
            env.warnings
                .iter()
                .any(|warning| warning
                    .contains("Gateway reconciles WhatsApp outbound Redis markers")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn simulated_plusoft_tool_exposure_is_flagged() {
        let root = temp_repo("simulated-tool");
        write_good_repo(&root);
        fs::write(
            root.join("cartridges/insurance_agent/tools.py"),
            "def get_tools() -> list:\n    return [preparar_encaminhamento_plusoft]\n",
        )
        .unwrap();
        let env = doctor_sara_assurant_egress_contract(&root);
        assert!(
            env.warnings
                .iter()
                .any(|warning| warning.contains("forbidden")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_plusoft_button_seed_config_is_flagged() {
        let root = temp_repo("legacy-buttons");
        write_good_repo(&root);
        let path = root.join("example-api/scripts/seed_sara_assurant.py");
        let mut body = fs::read_to_string(&path).unwrap();
        body.push_str(
            r#"
SARA_CONFIG = {
    "legacy_plusoft_menu": {"abrir sinistro": "1 Comunicar sinistro"},
    "intent_to_assunto": {"abrir_sinistro": "1 Comunicar sinistro"},
}
"#,
        );
        fs::write(path, body).unwrap();

        let env = doctor_sara_assurant_egress_contract(&root);
        assert!(
            env.warnings
                .iter()
                .any(|warning| warning.contains("legacy Plusoft button ids")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }
}
