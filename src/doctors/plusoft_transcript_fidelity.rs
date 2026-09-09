//! Plusoft transcript fidelity doctor.
//!
//! Guards the Sara -> Plusoft handoff transcript against the 2026-06-02 P0
//! regression where the payload history diverged from the customer-visible
//! WhatsApp conversation. The contract is source-level: Python webhook ingress
//! must dual-write inbound turns to `messages:{tenant}`, the Sara Plusoft
//! transcript builder must prefer complete runtime history over LLM agent
//! memory, merge the mirror when the runtime stream is one-sided, cover legacy
//! hot-state keys for conversations already open during deploy, and the
//! no-handover retained-history path must reuse the same transcript. The
//! dedicated `/v2/plusoft/ingest` Infobip path must reuse the same canonical
//! persistence contract before it queues Sara.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct PlusoftTranscriptFidelityDoctor;

impl Doctor for PlusoftTranscriptFidelityDoctor {
    fn name(&self) -> &'static str {
        "plusoft-transcript-fidelity"
    }

    fn description(&self) -> &'static str {
        "Checks that Sara/Plusoft handoff and retained-history transcripts are sourced from canonical WhatsApp runtime messages, with legacy hot-state fallback, not stale agent memory."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_plusoft_transcript_fidelity(root)
    }
}

struct Check<'a> {
    path: &'a str,
    label: &'a str,
    needles: &'a [&'a str],
}

pub fn doctor_plusoft_transcript_fidelity(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();

    let checks = [
        Check {
            path: "example-api/example/integrations/whatsapp/routers/webhook.py",
            label: "Python WhatsApp/Infobip ingress canonical message stream",
            needles: &[
                "def _messages_stream_maxlen",
                "def _append_inbound_runtime_message",
                "client.xadd(",
                "f\"messages:{tenant_id}\"",
                "\"conversation_id\": conversation_id",
                "\"memory_session_id\": memory_session_id",
                "\"direction\": \"inbound\"",
                "\"message_id\": message_id or \"\"",
                "message_timestamp=message.get(\"timestamp\")",
                "message_timestamp=result.get(\"receivedAt\")",
            ],
        },
        Check {
            path: "example-api/example/agents/tasks.py",
            label: "Sara Plusoft handoff and retained-history runtime-history preference",
            needles: &[
                "def _build_sara_plusoft_transcript",
                "tenant_id: str | None = None",
                "conversation_id: str | None = None",
                "business_phone_id: str | None = None",
                "user_phone: str | None = None",
                "current_message_text: str | None = None",
                "messages:{tenant_key}",
                "client.xrevrange(",
                "conversation:{conversation_key}",
                "def _add_legacy_history_keys",
                "session:{phone_id}:{sender}:history",
                "RedisSession(session_id, url=redis_url)",
                "runtime_usercodes = {",
                "runtime_is_bidirectional = {",
                "issubset(runtime_usercodes)",
                "current_message_text=message_text",
                "def _append_sara_plusoft_agent_history_message",
                "terminal_text.endswith(f\"\\n{clean_text}\")",
                "def _persist_sara_plusoft_retained_history",
                "RetainedHistoryBatchRequest.model_validate",
                "enviar_historico_retido",
                "final_node_should_reset",
                "plusoft_retained_history_result",
            ],
        },
        Check {
            path: "example-api/example/tests/api/test_agent_loop_task.py",
            label: "Sara Plusoft transcript and retained-history regression tests",
            needles: &[
                "test_sara_plusoft_history_prefers_runtime_stream_and_current_turn",
                "test_sara_plusoft_history_falls_back_to_legacy_hot_state_for_open_sessions",
                "test_sara_plusoft_history_merges_agent_mirror_when_stream_has_only_jai_turns",
                "test_sara_handoff_history_reappends_transfer_after_customer_reply",
                "test_sara_plusoft_retained_history_uses_runtime_transcript_and_final_output",
                "test_sara_final_node_without_handoff_persists_retained_history_before_reset",
                "\"text\": \"Quero cancelar seguro\"",
                "\"usercode\": \"JAI\"",
                "\"text\": \"Cliente\"",
                "plusoft_retained_history",
            ],
        },
        Check {
            path: "example-api/example/tests/api/test_whatsapp_router.py",
            label: "Python webhook canonical stream regression test",
            needles: &[
                "test_persist_inbound_message_dual_writes_canonical_runtime_stream",
                "stream_key == \"messages:tenant_a\"",
                "fields[\"conversation_id\"] == conversation_id",
            ],
        },
        Check {
            path: "cartridges/plusoft/router.py",
            label: "Dedicated Plusoft Infobip ingress canonical persistence",
            needles: &[
                "def _dispatch_jai_agent_for_infobip",
                "build_conversation_id(",
                "_append_inbound_runtime_message(",
                "redis_client=redis_client",
                "message_timestamp=result.get(\"receivedAt\")",
            ],
        },
        Check {
            path: "cartridges/plusoft/tests/test_plusoft_contracts.py",
            label: "Dedicated Plusoft Infobip ingress regression test",
            needles: &[
                "test_infobip_agent_dispatch_persists_canonical_inbound_before_queue",
                "assert call_order == [\"persist\", \"dispatch\"]",
                "canonical_conversation_id = build_conversation_id(",
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
                kind: "plusoft_transcript_fidelity_missing_file".to_string(),
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
            continue;
        }

        warnings.push(format!(
            "{} drift in {}: missing {}",
            check.label,
            check.path,
            missing.join(", ")
        ));
        evidence.push(EvidenceItem {
            kind: "plusoft_transcript_fidelity_missing_anchor".to_string(),
            path: check.path.to_string(),
            line: None,
            detail: format!("missing: {}", missing.join(", ")),
        });
    }

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_plusoft_transcript_fidelity"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            "Plusoft transcript fidelity contract present: Python ingress writes canonical history and Sara handoff/retained-history read it first".to_string()
        } else {
            format!(
                "Plusoft transcript fidelity contract drift: {} warning(s)",
                warnings.len()
            )
        },
        confidence: if warnings.is_empty() { 0.95 } else { 0.62 },
        entities: vec![json!({
            "doctor": "plusoft-transcript-fidelity",
            "tenant": "assurant",
            "runtime_stream": "messages:{tenant}",
            "handoff": "sara_plusoft_handoff",
            "retained_history": "jai_retained_conversation",
            "legacy_hot_state": "session:{phone}:{sender}:history",
            "checks_passed": passed,
            "checks_total": 6,
        })],
        evidence,
        warnings,
        meta: Some(json!({
            "tenant": "assurant",
            "cartridge": "plusoft",
            "incident_date": "2026-06-02",
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
            "leio_plusoft_transcript_fidelity_{}_{}",
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
    fn flags_missing_python_ingress_stream_write() {
        let root = temp_root("drift");
        write(
            &root,
            "example-api/example/integrations/whatsapp/routers/webhook.py",
            "def _persist_inbound_message():\n    pass\n",
        );
        write(
            &root,
            "example-api/example/agents/tasks.py",
            "def _build_sara_plusoft_transcript(tenant_id: str | None = None, conversation_id: str | None = None, business_phone_id: str | None = None, user_phone: str | None = None, current_message_text: str | None = None):\n    stream_key = f\"messages:{tenant_key}\"\n    client.xrevrange(stream_key)\n    client.get(f\"conversation:{conversation_key}\")\n    def _add_legacy_history_keys():\n        key = f\"session:{phone_id}:{sender}:history\"\n    RedisSession(session_id, url=redis_url)\n    runtime_usercodes = {entry[\"usercode\"] for entry in candidates}\n    runtime_is_bidirectional = {\"0\", \"JAI\"}.issubset(runtime_usercodes)\n    current_message_text=message_text\n\ndef _append_sara_plusoft_agent_history_message():\n    terminal_text.endswith(f\"\\n{clean_text}\")\n\ndef _persist_sara_plusoft_retained_history():\n    RetainedHistoryBatchRequest.model_validate({})\n    enviar_historico_retido(request)\n    final_node_should_reset = True\n    plusoft_retained_history_result = {}\n",
        );
        write(
            &root,
            "example-api/example/tests/api/test_agent_loop_task.py",
            "def test_sara_plusoft_history_prefers_runtime_stream_and_current_turn():\n    assert {\"usercode\": \"0\", \"text\": \"Quero cancelar seguro\"}\n    assert {\"usercode\": \"JAI\"}\n    assert {\"text\": \"Cliente\"}\n\ndef test_sara_plusoft_history_falls_back_to_legacy_hot_state_for_open_sessions():\n    assert \"session:108079528970614:5511999999999:history\"\n\ndef test_sara_plusoft_history_merges_agent_mirror_when_stream_has_only_jai_turns():\n    assert \"complete mirror\"\n\ndef test_sara_handoff_history_reappends_transfer_after_customer_reply():\n    assert \"terminal handoff\"\n\ndef test_sara_plusoft_retained_history_uses_runtime_transcript_and_final_output():\n    assert \"plusoft_retained_history\"\n\ndef test_sara_final_node_without_handoff_persists_retained_history_before_reset():\n    assert \"plusoft_retained_history\"\n",
        );
        write(
            &root,
            "example-api/example/tests/api/test_whatsapp_router.py",
            "def test_persist_inbound_message_dual_writes_canonical_runtime_stream():\n    assert stream_key == \"messages:tenant_a\"\n    assert fields[\"conversation_id\"] == conversation_id\n",
        );
        write(
            &root,
            "cartridges/plusoft/router.py",
            "def _dispatch_jai_agent_for_infobip():\n    session_id = build_conversation_id()\n    _append_inbound_runtime_message(message_timestamp=result.get(\"receivedAt\"), redis_client=redis_client)\n",
        );
        write(
            &root,
            "cartridges/plusoft/tests/test_plusoft_contracts.py",
            "def test_infobip_agent_dispatch_persists_canonical_inbound_before_queue():\n    canonical_conversation_id = build_conversation_id()\n    assert call_order == [\"persist\", \"dispatch\"]\n",
        );

        let envelope = doctor_plusoft_transcript_fidelity(&root);

        assert!(!envelope.warnings.is_empty());
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("canonical message stream"))
        );
    }

    #[test]
    fn flags_missing_bidirectional_runtime_merge_guard() {
        let root = temp_root("missing_bidirectional_guard");
        write(
            &root,
            "example-api/example/integrations/whatsapp/routers/webhook.py",
            "def _messages_stream_maxlen(): pass\ndef _append_inbound_runtime_message(): pass\nclient.xadd(f\"messages:{tenant_id}\", {\"conversation_id\": conversation_id, \"memory_session_id\": memory_session_id, \"direction\": \"inbound\", \"message_id\": message_id or \"\"})\nmessage_timestamp=message.get(\"timestamp\")\nmessage_timestamp=result.get(\"receivedAt\")\n",
        );
        write(
            &root,
            "example-api/example/agents/tasks.py",
            "def _build_sara_plusoft_transcript(tenant_id: str | None = None, conversation_id: str | None = None, business_phone_id: str | None = None, user_phone: str | None = None, current_message_text: str | None = None):\n    stream_key = f\"messages:{tenant_key}\"\n    client.xrevrange(stream_key)\n    client.get(f\"conversation:{conversation_key}\")\n    def _add_legacy_history_keys():\n        key = f\"session:{phone_id}:{sender}:history\"\n    RedisSession(session_id, url=redis_url)\n    current_message_text=message_text\n\ndef _persist_sara_plusoft_retained_history():\n    RetainedHistoryBatchRequest.model_validate({})\n    enviar_historico_retido(request)\n    final_node_should_reset = True\n    plusoft_retained_history_result = {}\n",
        );
        write(
            &root,
            "example-api/example/tests/api/test_agent_loop_task.py",
            "def test_sara_plusoft_history_prefers_runtime_stream_and_current_turn():\n    assert {\"usercode\": \"0\", \"text\": \"Quero cancelar seguro\"}\n    assert {\"usercode\": \"JAI\"}\n    assert {\"text\": \"Cliente\"}\n\ndef test_sara_plusoft_history_falls_back_to_legacy_hot_state_for_open_sessions():\n    assert \"session:108079528970614:5511999999999:history\"\n\ndef test_sara_plusoft_retained_history_uses_runtime_transcript_and_final_output():\n    assert \"plusoft_retained_history\"\n\ndef test_sara_final_node_without_handoff_persists_retained_history_before_reset():\n    assert \"plusoft_retained_history\"\n",
        );
        write(
            &root,
            "example-api/example/tests/api/test_whatsapp_router.py",
            "def test_persist_inbound_message_dual_writes_canonical_runtime_stream():\n    assert stream_key == \"messages:tenant_a\"\n    assert fields[\"conversation_id\"] == conversation_id\n",
        );
        write(
            &root,
            "cartridges/plusoft/router.py",
            "def _dispatch_jai_agent_for_infobip():\n    session_id = build_conversation_id()\n    _append_inbound_runtime_message(message_timestamp=result.get(\"receivedAt\"), redis_client=redis_client)\n",
        );
        write(
            &root,
            "cartridges/plusoft/tests/test_plusoft_contracts.py",
            "def test_infobip_agent_dispatch_persists_canonical_inbound_before_queue():\n    canonical_conversation_id = build_conversation_id()\n    assert call_order == [\"persist\", \"dispatch\"]\n",
        );

        let envelope = doctor_plusoft_transcript_fidelity(&root);

        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("runtime-history preference"))
        );
    }

    #[test]
    fn clean_tree_yields_no_warnings() {
        let root = temp_root("clean");
        write(
            &root,
            "example-api/example/integrations/whatsapp/routers/webhook.py",
            "def _messages_stream_maxlen(): pass\ndef _append_inbound_runtime_message(): pass\nclient.xadd(f\"messages:{tenant_id}\", {\"conversation_id\": conversation_id, \"memory_session_id\": memory_session_id, \"direction\": \"inbound\", \"message_id\": message_id or \"\"})\nmessage_timestamp=message.get(\"timestamp\")\nmessage_timestamp=result.get(\"receivedAt\")\n",
        );
        write(
            &root,
            "example-api/example/agents/tasks.py",
            "def _build_sara_plusoft_transcript(tenant_id: str | None = None, conversation_id: str | None = None, business_phone_id: str | None = None, user_phone: str | None = None, current_message_text: str | None = None):\n    stream_key = f\"messages:{tenant_key}\"\n    client.xrevrange(stream_key)\n    client.get(f\"conversation:{conversation_key}\")\n    def _add_legacy_history_keys():\n        key = f\"session:{phone_id}:{sender}:history\"\n    RedisSession(session_id, url=redis_url)\n    runtime_usercodes = {entry[\"usercode\"] for entry in candidates}\n    runtime_is_bidirectional = {\"0\", \"JAI\"}.issubset(runtime_usercodes)\n    current_message_text=message_text\n\ndef _append_sara_plusoft_agent_history_message():\n    terminal_text.endswith(f\"\\n{clean_text}\")\n\ndef _persist_sara_plusoft_retained_history():\n    RetainedHistoryBatchRequest.model_validate({})\n    enviar_historico_retido(request)\n    final_node_should_reset = True\n    plusoft_retained_history_result = {}\n",
        );
        write(
            &root,
            "example-api/example/tests/api/test_agent_loop_task.py",
            "def test_sara_plusoft_history_prefers_runtime_stream_and_current_turn():\n    assert {\"usercode\": \"0\", \"text\": \"Quero cancelar seguro\"}\n    assert {\"usercode\": \"JAI\"}\n    assert {\"text\": \"Cliente\"}\n\ndef test_sara_plusoft_history_falls_back_to_legacy_hot_state_for_open_sessions():\n    assert \"session:108079528970614:5511999999999:history\"\n\ndef test_sara_plusoft_history_merges_agent_mirror_when_stream_has_only_jai_turns():\n    assert \"complete mirror\"\n\ndef test_sara_handoff_history_reappends_transfer_after_customer_reply():\n    assert \"terminal handoff\"\n\ndef test_sara_plusoft_retained_history_uses_runtime_transcript_and_final_output():\n    assert \"plusoft_retained_history\"\n\ndef test_sara_final_node_without_handoff_persists_retained_history_before_reset():\n    assert \"plusoft_retained_history\"\n",
        );
        write(
            &root,
            "example-api/example/tests/api/test_whatsapp_router.py",
            "def test_persist_inbound_message_dual_writes_canonical_runtime_stream():\n    assert stream_key == \"messages:tenant_a\"\n    assert fields[\"conversation_id\"] == conversation_id\n",
        );
        write(
            &root,
            "cartridges/plusoft/router.py",
            "def _dispatch_jai_agent_for_infobip():\n    session_id = build_conversation_id()\n    _append_inbound_runtime_message(message_timestamp=result.get(\"receivedAt\"), redis_client=redis_client)\n",
        );
        write(
            &root,
            "cartridges/plusoft/tests/test_plusoft_contracts.py",
            "def test_infobip_agent_dispatch_persists_canonical_inbound_before_queue():\n    canonical_conversation_id = build_conversation_id()\n    assert call_order == [\"persist\", \"dispatch\"]\n",
        );

        let envelope = doctor_plusoft_transcript_fidelity(&root);

        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
    }
}
