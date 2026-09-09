use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct SessionHotStateDoctor;

impl Doctor for SessionHotStateDoctor {
    fn name(&self) -> &'static str {
        "session-hot-state"
    }

    fn description(&self) -> &'static str {
        "Checks canonical Redis session/history hot-state key usage."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_session_hot_state(index, root)
    }
}

pub fn doctor_session_hot_state(_index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let session_identity_path = root.join("example-gateway/src/session_identity.rs");
    let py_session_identity_path =
        root.join("example-api/example/integrations/whatsapp/session_identity.py");
    let py_tasks_path = root.join("example-api/example/agents/tasks.py");
    let py_handoff_path = root.join("example-api/example/agents/tools/handoff.py");
    let py_ops_path = root.join("example-api/example/routers/ops.py");
    let py_drain_worker_path = root.join("example-api/example/core/drain_worker.py");
    let sessions_path = root.join("example-gateway/src/sessions.rs");
    let runtime_streams_path = root.join("example-gateway/src/runtime_streams.rs");
    let routing_path = root.join("example-gateway/src/routing.rs");
    let messages_path = root.join("example-gateway/src/messages.rs");
    let ingest_whatsapp_path = root.join("example-gateway/src/ingest/whatsapp/mod.rs");
    let ops_messaging_path =
        root.join("example-gateway/src/ops_console/routes/helpers/messaging.rs");
    let whatsapp_path = root.join("example-gateway/src/integrations/tools/messaging/whatsapp.rs");

    let session_identity_src = read_text(&session_identity_path, &mut warnings);
    let py_session_identity_src = read_text(&py_session_identity_path, &mut warnings);
    let py_tasks_src = read_text(&py_tasks_path, &mut warnings);
    let py_handoff_src = read_text(&py_handoff_path, &mut warnings);
    let py_ops_src = read_text(&py_ops_path, &mut warnings);
    let py_drain_worker_src = read_text(&py_drain_worker_path, &mut warnings);
    let sessions_src = read_text(&sessions_path, &mut warnings);
    let runtime_streams_src = read_text(&runtime_streams_path, &mut warnings);
    let routing_src = read_text(&routing_path, &mut warnings);
    let messages_src = read_text(&messages_path, &mut warnings);
    let ingest_whatsapp_src = read_text(&ingest_whatsapp_path, &mut warnings);
    let ops_messaging_src = read_text(&ops_messaging_path, &mut warnings);
    let whatsapp_src = read_text(&whatsapp_path, &mut warnings);

    let has_session_identity_helpers = session_identity_src.as_deref().is_some_and(|src| {
        src.contains("pub fn conversation_id_from_session_id(")
            && src.contains("pub fn split_session_id(")
            && src.contains("pub fn build_conversation_id(")
            && src.contains("pub fn build_memory_session_id(")
    });
    let py_has_inbound_identity_helpers = py_session_identity_src.as_deref().is_some_and(|src| {
        src.contains("def resolve_inbound_conversation_id(")
            && src.contains("def resolve_inbound_memory_session_id(")
    });
    let py_has_session_identity_resolver = py_session_identity_src.as_deref().is_some_and(|src| {
        src.contains("def workflow_root_id_from_conversation_id(")
            && src.contains("class CanonicalSessionIdentity:")
            && src.contains("def resolve_session_identity(")
    });
    let has_canonical_helpers = sessions_src.as_deref().is_some_and(|src| {
        src.contains("pub(crate) fn session_key(") && src.contains("pub(crate) fn history_key(")
    });
    let routing_exposes_resolved_route = routing_src.as_deref().is_some_and(|src| {
        src.contains("pub struct ResolvedRoute")
            && src.contains("pub root_agent_id: String,")
            && src.contains("pub async fn find_resolved_route_async(")
            && src.contains("(\"root_agent_id\", root_agent_id),")
    });
    let ingest_imports_helpers = ingest_whatsapp_src.as_deref().is_some_and(|src| {
        src.contains(
            "use crate::session_identity::{build_conversation_id, build_memory_session_id, split_session_id};",
        )
            && src.contains("use crate::sessions::{history_key, session_key};")
    });
    let ingest_mirrors_canonical_hot_state = ingest_whatsapp_src.as_deref().is_some_and(|src| {
        src.contains("let canonical_hot_state = split_session_id(session_id).map(")
            && src.contains("let _: () = redis")
            && src.contains("Failed to mirror canonical session hot state")
            && src.contains("Failed to mirror canonical message hot state")
    });
    let ingest_reads_canonical_history_first = ingest_whatsapp_src.as_deref().is_some_and(|src| {
        src.contains("let mut candidate_keys = Vec::new();")
            && src.contains("candidate_keys.push(history_key(&phone_number_id, &sender));")
            && src.contains("let legacy_message_key = format!(\"messages:{}\", session_id);")
    });
    let runtime_streams_dual_write_identity = runtime_streams_src.as_deref().is_some_and(|src| {
        src.contains("fn resolve_message_identity_fields(")
            && src.contains("conversation_id: Option<&str>,")
            && src.contains("memory_session_id: Option<&str>,")
            && src.contains("root_agent_id: Option<&str>,")
            && src.contains("workflow_root_id: Option<&str>,")
            && src.contains("let identity = resolve_message_identity_fields(")
            && src.contains("(\"conversation_id\", identity.conversation_id.as_str())")
            && src.contains("(\"memory_session_id\", identity.memory_session_id.as_str())")
            && src.contains("(\"root_agent_id\", identity.root_agent_id.as_str())")
            && src.contains("(\"workflow_root_id\", identity.workflow_root_id.as_str())")
            && src.contains("(\"actor_type\", \"human\")")
            && src.contains("(\"actor_id\", sender_phone)")
            && src.contains("(\"actor_type\", actor_type)")
            && src.contains("(\"actor_id\", actor_id)")
    });
    let runtime_streams_dual_write_message_aliases =
        runtime_streams_src.as_deref().is_some_and(|src| {
            src.contains("fn unix_timestamp_field(timestamp: &str) -> String")
                && src.contains("(\"phone\", sender_phone)")
                && src.contains("(\"counterpart\", counterpart)")
                && src.contains("(\"text\", message_text)")
                && src.contains("(\"text_preview\", summary.as_str())")
                && src.contains("(\"ts_unix\", ts_unix.as_str())")
                && src.contains("(\"phone\", recipient_phone)")
                && src.contains("(\"counterpart\", recipient_phone)")
        });
    let gateway_message_cold_store_persists_canonical_identity =
        messages_src.as_deref().is_some_and(|src| {
            src.contains("conversation_id: Option<String>,")
                && src.contains("memory_session_id: Option<String>,")
                && src.contains("root_agent_id: Option<String>,")
                && src.contains("workflow_root_id: Option<String>,")
                && src.contains(
                    "conversation_id: entry.get::<String>(\"conversation_id\").and_then(non_empty),",
                )
                && src.contains(
                    "memory_session_id: entry.get::<String>(\"memory_session_id\").and_then(non_empty),",
                )
                && src.contains(
                    "root_agent_id: entry.get::<String>(\"root_agent_id\").and_then(non_empty),",
                )
                && src.contains(
                    "workflow_root_id: entry.get::<String>(\"workflow_root_id\").and_then(non_empty),",
                )
                && src.contains("conversation_id VARCHAR")
                && src.contains("memory_session_id VARCHAR")
                && src.contains("root_agent_id VARCHAR")
                && src.contains("workflow_root_id VARCHAR")
                && src.contains("INSERT OR IGNORE INTO messages (")
                && src.contains("message_id, tenant_id, session_id, conversation_id,")
                && src.contains("memory_session_id, root_agent_id, workflow_root_id, direction,")
                && src.contains("msg.conversation_id.as_deref()")
                && src.contains("msg.memory_session_id.as_deref()")
                && src.contains("msg.root_agent_id.as_deref()")
                && src.contains("msg.workflow_root_id.as_deref()")
        });
    let ingest_passes_explicit_message_identity =
        ingest_whatsapp_src.as_deref().is_some_and(|src| {
            src.contains("let stream_entry_id = runtime_streams::append_inbound_message(")
                && src.contains("Some(conversation_id)")
                && src.contains("Some(memory_session_id)")
                && src.contains("Some(root_agent_id)")
                && src.contains("Some(workflow_root_id)")
        });
    let ingest_uses_resolved_route_identity = ingest_whatsapp_src.as_deref().is_some_and(|src| {
        src.contains("match routing::find_resolved_route_async(state.clone(), phone).await {")
            && src.contains("let routing::ResolvedRoute {")
            && src.contains("\"_root_agent_id\": root_agent_id,")
            && src.contains("\"_conversation_id\": conversation_id,")
            && src.contains("\"_memory_session_id\": memory_session_id,")
    });
    let py_tasks_prefer_canonical_inbound_ids = py_tasks_src.as_deref().is_some_and(|src| {
        src.contains("resolve_inbound_conversation_id,")
            && src.contains("resolve_inbound_memory_session_id,")
            && src.contains("resolve_session_identity,")
            && src.contains("conversation_id = resolve_inbound_conversation_id(")
            && src.contains("session_id = resolve_inbound_memory_session_id(")
            && !src.contains("conversation_id_raw = normalized_event.get(\"session_id\")")
    });
    let py_tasks_build_step_handoff_identity = py_tasks_src.as_deref().is_some_and(|src| {
        src.contains("conversation_id = build_conversation_id(")
            && src.contains("session_id = build_memory_session_id(")
            && src.contains("conversation_id=conversation_id,")
            && src.contains("memory_session_id=session_id,")
            && src.contains("root_agent_id=workflow_root_id,")
            && src.contains("workflow_root_id=workflow_root_id,")
    });
    let py_tasks_handoff_event_dual_writes_identity = py_tasks_src.as_deref().is_some_and(|src| {
        src.contains("identity = resolve_session_identity(")
            && src.contains("payload[\"conversation_id\"] = identity.conversation_id or \"\"")
            && src.contains("payload[\"memory_session_id\"] = identity.memory_session_id or \"\"")
            && src.contains("payload[\"root_agent_id\"] = identity.root_agent_id or \"\"")
            && src.contains("payload[\"workflow_root_id\"] = identity.workflow_root_id or \"\"")
    });
    let py_handoff_stream_dual_writes_identity = py_handoff_src.as_deref().is_some_and(|src| {
        src.contains("identity = resolve_session_identity(")
            && src.contains("\"conversation_id\": identity.conversation_id or \"\",")
            && src.contains("\"memory_session_id\": identity.memory_session_id or \"\",")
            && src.contains("\"root_agent_id\": identity.root_agent_id or \"\",")
            && src.contains("\"workflow_root_id\": identity.workflow_root_id or \"\",")
    });
    let py_ops_resolution_stream_dual_writes_identity = py_ops_src.as_deref().is_some_and(|src| {
        src.contains("identity = resolve_session_identity(")
            && src.contains("\"agent_session_id\": identity.memory_session_id or session_id,")
            && src.contains("\"conversation_id\": identity.conversation_id or \"\",")
            && src.contains("\"memory_session_id\": identity.memory_session_id or \"\",")
            && src.contains("\"root_agent_id\": identity.root_agent_id or \"\",")
            && src.contains("\"workflow_root_id\": identity.workflow_root_id or \"\",")
    });
    let py_drain_worker_prefers_message_aliases =
        py_drain_worker_src.as_deref().is_some_and(|src| {
            src.contains("def _iso_from_unix(ts_unix: str | None) -> str | None:")
                && src.contains("phone = fields.get(\"phone\", \"\")")
                && src.contains("fields.get(\"counterpart\", fields.get(\"role\", \"unknown\"))")
                && src.contains("text = fields.get(\"text\", fields.get(\"message\", \"\"))")
                && src.contains(
                    "ts_utc = fields.get(\"ts_utc\") or _iso_from_unix(fields.get(\"ts_unix\")) or _utcnow_iso()",
                )
        });
    let py_drain_worker_persists_canonical_message_identity =
        py_drain_worker_src.as_deref().is_some_and(|src| {
            src.contains("def _int_from_unix(ts_unix: str | None) -> int | None:")
                && src.contains("counterpart = fields.get(")
                && src.contains("text_preview = fields.get(\"text_preview\", fields.get(\"message_summary\", text))")
                && src.contains("ts_unix = _int_from_unix(fields.get(\"ts_unix\"))")
                && src.contains("channel = fields.get(\"channel\", \"whatsapp\")")
                && src.contains("actor_type = fields.get(\"actor_type\", \"\")")
                && src.contains("actor_id = fields.get(\"actor_id\", \"\")")
                && src.contains("conversation_id = fields.get(\"conversation_id\", \"\") or None")
                && src.contains("memory_session_id = fields.get(\"memory_session_id\", \"\") or None")
                && src.contains("root_agent_id = fields.get(\"root_agent_id\", \"\") or None")
                && src.contains("workflow_root_id = fields.get(\"workflow_root_id\", \"\") or None")
                && src.contains("ALTER TABLE {table} ADD COLUMN IF NOT EXISTS conversation_id VARCHAR;")
                && src.contains("ALTER TABLE {table} ADD COLUMN IF NOT EXISTS memory_session_id VARCHAR;")
                && src.contains("ALTER TABLE {table} ADD COLUMN IF NOT EXISTS root_agent_id VARCHAR;")
                && src.contains("ALTER TABLE {table} ADD COLUMN IF NOT EXISTS workflow_root_id VARCHAR;")
                && src.contains("ALTER TABLE {table} ADD COLUMN IF NOT EXISTS actor_type VARCHAR;")
                && src.contains("ALTER TABLE {table} ADD COLUMN IF NOT EXISTS actor_id VARCHAR;")
                && src.contains("ALTER TABLE {table} ADD COLUMN IF NOT EXISTS counterpart VARCHAR;")
                && src.contains("ALTER TABLE {table} ADD COLUMN IF NOT EXISTS text_preview TEXT;")
                && src.contains("ALTER TABLE {table} ADD COLUMN IF NOT EXISTS ts_unix BIGINT;")
                && src.contains("CREATE INDEX IF NOT EXISTS idx_{table}_conversation")
                && src.contains("CREATE INDEX IF NOT EXISTS idx_{table}_memory_session")
        });
    let ops_messaging_uses_canonical_phone_identity =
        ops_messaging_src.as_deref().is_some_and(|src| {
            src.contains("fn resolve_phone_number_id_for_delivery(session_id: &str)")
                && src.contains("split_session_id(session_id)")
                && src.contains(
                    "Não foi possível determinar o phone_number_id canônico para enviar mensagem.",
                )
                && !src.contains(
                    "SELECT phone_number_id FROM whatsapp_phone_numbers WHERE tenant_id = ? ORDER BY phone_number_id LIMIT 1",
                )
        });
    let whatsapp_imports_helpers = whatsapp_src
        .as_deref()
        .is_some_and(|src| src.contains("use crate::sessions::{history_key, session_key};"));
    let whatsapp_uses_canonical_keys = whatsapp_src.as_deref().is_some_and(|src| {
        src.contains("let hot_session_key = session_key(phone_number_id, recipient_phone);")
            && src.contains("let history_list_key = history_key(phone_number_id, recipient_phone);")
    });
    let whatsapp_passes_explicit_message_identity = whatsapp_src.as_deref().is_some_and(|src| {
        (src.contains("ctx.conversation_id.as_deref()")
            || src.contains("let runtime_identity = resolve_tenant_message_identity("))
            && (src.contains("ctx.memory_session_id.as_deref()")
                || src.contains(
                    "(!memory_session_id.is_empty()).then_some(memory_session_id.as_str())",
                ))
            && (src.contains("ctx.root_agent_id.as_deref()")
                || src.contains("(!root_agent_id.is_empty()).then_some(root_agent_id.as_str())"))
            && (src.contains("ctx.workflow_root_id.as_deref()")
                || src.contains(
                    "(!workflow_root_id.is_empty()).then_some(workflow_root_id.as_str())",
                ))
            && src.contains("let stream_entry_id = runtime_streams::append_outbound_message(")
            && src.contains("conversation_id,")
            && src.contains("memory_session_id,")
            && src.contains("root_agent_id,")
            && src.contains("workflow_root_id,")
    });
    let whatsapp_uses_legacy_session_id_layout = whatsapp_src.as_deref().is_some_and(|src| {
        src.contains("session:{session_id}") || src.contains("messages:{session_id}")
    });

    if !has_session_identity_helpers {
        warnings.push(
            "session_identity.rs does not expose canonical conversation/session id helpers"
                .to_string(),
        );
    }
    if !has_canonical_helpers {
        warnings
            .push("sessions.rs does not expose canonical session/history key helpers".to_string());
    }
    if !py_has_inbound_identity_helpers {
        warnings.push(
            "Python WhatsApp session_identity.py does not expose canonical inbound conversation/memory-session helpers"
                .to_string(),
        );
    }
    if !routing_exposes_resolved_route {
        warnings.push(
            "routing.rs does not expose the enriched ResolvedRoute/root_agent_id contract"
                .to_string(),
        );
    }
    if !ingest_imports_helpers {
        warnings.push(
            "ingest/whatsapp/mod.rs does not import canonical session identity and hot-state helpers"
                .to_string(),
        );
    }
    if !ingest_mirrors_canonical_hot_state {
        warnings.push(
            "ingest/whatsapp/mod.rs does not mirror inbound hot state into canonical session/history keys"
                .to_string(),
        );
    }
    if !ingest_reads_canonical_history_first {
        warnings.push(
            "ingest/whatsapp/mod.rs does not read canonical Redis history before legacy fallback"
                .to_string(),
        );
    }
    if !runtime_streams_dual_write_identity {
        warnings.push(
            "runtime_streams.rs does not dual-write canonical conversation/session/root identity into messages:{tenant}"
                .to_string(),
        );
    }
    if !runtime_streams_dual_write_message_aliases {
        warnings.push(
            "runtime_streams.rs does not dual-write legacy/canonical message stream aliases for drain compatibility"
                .to_string(),
        );
    }
    if !gateway_message_cold_store_persists_canonical_identity {
        warnings.push(
            "example-gateway/src/messages.rs does not persist canonical message identity fields into the gateway DuckDB messages cold store"
                .to_string(),
        );
    }
    if !ingest_passes_explicit_message_identity {
        warnings.push(
            "ingest/whatsapp/mod.rs does not pass canonical conversation/session/root identity explicitly into runtime_streams::append_inbound_message"
                .to_string(),
        );
    }
    if !ingest_uses_resolved_route_identity {
        warnings.push(
            "ingest/whatsapp/mod.rs does not consume ResolvedRoute/root_agent_id for canonical routing context"
                .to_string(),
        );
    }
    if !py_tasks_prefer_canonical_inbound_ids {
        warnings.push(
            "example-api agent tasks still derive conversation identity from legacy session_id instead of canonical conversation_id/memory_session_id helpers"
                .to_string(),
        );
    }
    if !py_has_session_identity_resolver {
        warnings.push(
            "Python WhatsApp session_identity.py does not expose a shared canonical session resolver for producer paths"
                .to_string(),
        );
    }
    if !py_tasks_build_step_handoff_identity {
        warnings.push(
            "example-api step tool builder still fabricates legacy handoff session ids instead of canonical conversation/memory-session identity"
                .to_string(),
        );
    }
    if !py_tasks_handoff_event_dual_writes_identity {
        warnings.push(
            "example-api run_human_handoff_event does not dual-write canonical conversation/session/root identity into the human handoff stream"
                .to_string(),
        );
    }
    if !py_handoff_stream_dual_writes_identity {
        warnings.push(
            "example-api human escalation tool does not dual-write canonical conversation/session/root identity into exception_requested events"
                .to_string(),
        );
    }
    if !py_ops_resolution_stream_dual_writes_identity {
        warnings.push(
            "example-api ops exception resolution stream does not dual-write canonical conversation/session/root identity into exception_resolved events"
                .to_string(),
        );
    }
    if !py_drain_worker_prefers_message_aliases {
        warnings.push(
            "example-api drain_worker.py does not prefer canonical message-stream aliases before legacy fallback"
                .to_string(),
        );
    }
    if !py_drain_worker_persists_canonical_message_identity {
        warnings.push(
            "example-api drain_worker.py does not persist canonical message identity fields into PostgreSQL messages storage"
                .to_string(),
        );
    }
    if !ops_messaging_uses_canonical_phone_identity {
        warnings.push(
            "ops_console/routes/helpers/messaging.rs still guesses phone_number_id from tenant defaults instead of deriving it from the canonical session identity"
                .to_string(),
        );
    }
    if !whatsapp_imports_helpers {
        warnings.push(
            "whatsapp.rs does not import the canonical session/history key helpers".to_string(),
        );
    }
    if !whatsapp_uses_canonical_keys {
        warnings.push(
            "whatsapp.rs does not persist outbound hot state with session_key/history_key"
                .to_string(),
        );
    }
    if !whatsapp_passes_explicit_message_identity {
        warnings.push(
            "whatsapp.rs does not pass canonical conversation/session/root identity explicitly into runtime_streams::append_outbound_message"
                .to_string(),
        );
    }
    if whatsapp_uses_legacy_session_id_layout {
        warnings.push(
            "whatsapp.rs still contains legacy session:{session_id} or messages:{session_id} hot-state keys"
                .to_string(),
        );
    }

    if let Some(src) = &session_identity_src {
        if let Some(line) = find_line(src, "pub fn conversation_id_from_session_id(") {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: session_identity_path.display().to_string(),
                line: Some(line),
                detail: "canonical conversation id helper for session compatibility".to_string(),
            });
        }
        if let Some(line) = find_line(src, "pub fn split_session_id(") {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: session_identity_path.display().to_string(),
                line: Some(line),
                detail: "session id splitter accepts canonical and legacy layouts".to_string(),
            });
        }
        if let Some(line) = find_line(src, "pub fn build_conversation_id(") {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: session_identity_path.display().to_string(),
                line: Some(line),
                detail: "Rust mirrors the canonical WhatsApp conversation-id builder".to_string(),
            });
        }
        if let Some(line) = find_line(src, "pub fn build_memory_session_id(") {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: session_identity_path.display().to_string(),
                line: Some(line),
                detail: "Rust mirrors the canonical WhatsApp memory-session builder".to_string(),
            });
        }
    }

    if let Some(src) = &py_session_identity_src {
        if let Some(line) = find_line(src, "def resolve_inbound_conversation_id(") {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: py_session_identity_path.display().to_string(),
                line: Some(line),
                detail: "Python exposes canonical inbound conversation-id resolver".to_string(),
            });
        }
        if let Some(line) = find_line(src, "def resolve_inbound_memory_session_id(") {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: py_session_identity_path.display().to_string(),
                line: Some(line),
                detail: "Python exposes canonical inbound memory-session resolver".to_string(),
            });
        }
        if let Some(line) = find_line(src, "def resolve_session_identity(") {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: py_session_identity_path.display().to_string(),
                line: Some(line),
                detail: "Python exposes a shared canonical session resolver for producer paths"
                    .to_string(),
            });
        }
    }

    if let Some(src) = &py_drain_worker_src {
        for (needle, detail) in [
            (
                "def _iso_from_unix(ts_unix: str | None) -> str | None:",
                "Python drain worker can normalize canonical ts_unix into ts_utc",
            ),
            (
                "phone = fields.get(\"phone\", \"\")",
                "Python drain worker prefers canonical phone alias from message stream",
            ),
            (
                "text = fields.get(\"text\", fields.get(\"message\", \"\"))",
                "Python drain worker prefers canonical text alias before legacy message field",
            ),
            (
                "conversation_id = fields.get(\"conversation_id\", \"\") or None",
                "Python drain worker preserves canonical conversation_id in durable message storage",
            ),
            (
                "ALTER TABLE {table} ADD COLUMN IF NOT EXISTS conversation_id VARCHAR;",
                "PostgreSQL messages table upgrades in place to keep canonical message identity",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "session_hot_state".to_string(),
                    path: py_drain_worker_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            }
        }
    }

    if let Some(src) = &sessions_src {
        if let Some(line) = find_line(src, "pub(crate) fn session_key(") {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: sessions_path.display().to_string(),
                line: Some(line),
                detail: "canonical Redis session key helper".to_string(),
            });
        }
        if let Some(line) = find_line(src, "pub(crate) fn history_key(") {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: sessions_path.display().to_string(),
                line: Some(line),
                detail: "canonical Redis session history key helper".to_string(),
            });
        }
    }

    if let Some(src) = &runtime_streams_src {
        for (needle, detail) in [
            (
                "fn resolve_message_identity_fields(",
                "message stream accepts explicit canonical conversation/root identity inputs",
            ),
            (
                "(\"conversation_id\", identity.conversation_id.as_str())",
                "message stream persists canonical conversation id alongside legacy fields",
            ),
            (
                "(\"text_preview\", summary.as_str())",
                "message stream dual-writes canonical text aliases for drain compatibility",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "session_hot_state".to_string(),
                    path: runtime_streams_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            }
        }
    }

    if let Some(src) = &messages_src {
        for (needle, detail) in [
            (
                "conversation_id VARCHAR",
                "gateway messages cold-store DDL preserves canonical conversation identity",
            ),
            (
                "conversation_id: entry.get::<String>(\"conversation_id\").and_then(non_empty),",
                "gateway message drain decodes canonical conversation identity from Redis streams",
            ),
            (
                "INSERT OR IGNORE INTO messages (",
                "gateway message drain persists canonical identity into DuckDB cold storage",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "session_hot_state".to_string(),
                    path: messages_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            }
        }
    }

    if let Some(src) = &ops_messaging_src {
        for (needle, detail) in [
            (
                "fn resolve_phone_number_id_for_delivery(session_id: &str)",
                "ops-console delivery helper derives phone_number_id from canonical session identity",
            ),
            (
                "split_session_id(session_id)",
                "ops-console delivery helper refuses tenant-default phone routing guesses",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "session_hot_state".to_string(),
                    path: ops_messaging_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            }
        }
    }

    if let Some(src) = &routing_src {
        if let Some(line) = find_line(src, "pub struct ResolvedRoute") {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: routing_path.display().to_string(),
                line: Some(line),
                detail:
                    "routing exports enriched ResolvedRoute contract with workflow-root identity"
                        .to_string(),
            });
        }
        if let Some(line) = find_line(src, "(\"root_agent_id\", root_agent_id),") {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: routing_path.display().to_string(),
                line: Some(line),
                detail: "Redis route projection persists root_agent_id alongside agent_id"
                    .to_string(),
            });
        }
    }

    if let Some(src) = &ingest_whatsapp_src {
        if let Some(line) = find_line(
            src,
            "use crate::session_identity::{build_conversation_id, build_memory_session_id, split_session_id};",
        ) {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: ingest_whatsapp_path.display().to_string(),
                line: Some(line),
                detail: "inbound ingest imports canonical session identity helper".to_string(),
            });
        }
        if let Some(line) = find_line(
            src,
            "let canonical_hot_state = split_session_id(session_id).map(",
        ) {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: ingest_whatsapp_path.display().to_string(),
                line: Some(line),
                detail: "inbound ingest mirrors hot state into canonical session/history keys"
                    .to_string(),
            });
        }
        if let Some(line) = find_line(
            src,
            "candidate_keys.push(history_key(&phone_number_id, &sender));",
        ) {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: ingest_whatsapp_path.display().to_string(),
                line: Some(line),
                detail: "inbound Redis history reads canonical key before legacy fallback"
                    .to_string(),
            });
        }
        if let Some(line) = find_line(
            src,
            "match routing::find_resolved_route_async(state.clone(), phone).await {",
        ) {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: ingest_whatsapp_path.display().to_string(),
                line: Some(line),
                detail: "inbound routing consumes enriched ResolvedRoute contract".to_string(),
            });
        }
        if let Some(line) = find_line(src, "\"_root_agent_id\": root_agent_id,") {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: ingest_whatsapp_path.display().to_string(),
                line: Some(line),
                detail: "inbound agent input carries workflow-root identity for canonical session evolution"
                    .to_string(),
            });
        }
        if let Some(line) = find_line(src, "\"_conversation_id\": conversation_id,") {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: ingest_whatsapp_path.display().to_string(),
                line: Some(line),
                detail: "inbound agent input carries canonical conversation id alongside legacy session id"
                    .to_string(),
            });
        }
        if let Some(line) = find_line(
            src,
            "let stream_entry_id = runtime_streams::append_inbound_message(",
        ) {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: ingest_whatsapp_path.display().to_string(),
                line: Some(line),
                detail: "inbound message producer passes canonical identity explicitly into the runtime stream"
                    .to_string(),
            });
        }
    }

    if let Some(src) = &py_tasks_src {
        if let Some(line) = find_line(src, "conversation_id = resolve_inbound_conversation_id(") {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: py_tasks_path.display().to_string(),
                line: Some(line),
                detail: "agent tasks prefer canonical inbound conversation identity".to_string(),
            });
        }
        if let Some(line) = find_line(src, "session_id = resolve_inbound_memory_session_id(") {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: py_tasks_path.display().to_string(),
                line: Some(line),
                detail: "agent tasks prefer canonical inbound memory-session identity".to_string(),
            });
        }
        if let Some(line) = find_line(src, "conversation_id = build_conversation_id(") {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: py_tasks_path.display().to_string(),
                line: Some(line),
                detail: "workflow step handoff tooling now builds canonical conversation ids"
                    .to_string(),
            });
        }
        if let Some(line) = find_line(
            src,
            "payload[\"conversation_id\"] = identity.conversation_id or \"\"",
        ) {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: py_tasks_path.display().to_string(),
                line: Some(line),
                detail: "human handoff stream payload now carries canonical conversation identity"
                    .to_string(),
            });
        }
    }

    if let Some(src) = &py_handoff_src {
        if let Some(line) = find_line(src, "identity = resolve_session_identity(") {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: py_handoff_path.display().to_string(),
                line: Some(line),
                detail: "human escalation tool derives canonical identity from session context"
                    .to_string(),
            });
        }
        if let Some(line) = find_line(
            src,
            "\"conversation_id\": identity.conversation_id or \"\",",
        ) {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: py_handoff_path.display().to_string(),
                line: Some(line),
                detail: "exception_requested stream entries now dual-write canonical conversation identity".to_string(),
            });
        }
    }

    if let Some(src) = &py_ops_src {
        if let Some(line) = find_line(src, "identity = resolve_session_identity(") {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: py_ops_path.display().to_string(),
                line: Some(line),
                detail:
                    "ops exception resolution derives canonical identity from the resumed session"
                        .to_string(),
            });
        }
        if let Some(line) = find_line(
            src,
            "\"conversation_id\": identity.conversation_id or \"\",",
        ) {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: py_ops_path.display().to_string(),
                line: Some(line),
                detail: "exception_resolved stream entries now dual-write canonical conversation identity".to_string(),
            });
        }
    }

    if let Some(src) = &whatsapp_src {
        if let Some(line) = find_line(src, "use crate::sessions::{history_key, session_key};") {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: whatsapp_path.display().to_string(),
                line: Some(line),
                detail: "whatsapp tool imports canonical session key helpers".to_string(),
            });
        }
        if let Some(line) = find_line(
            src,
            "let hot_session_key = session_key(phone_number_id, recipient_phone);",
        ) {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: whatsapp_path.display().to_string(),
                line: Some(line),
                detail: "outbound hot state uses canonical session key".to_string(),
            });
        }
        if let Some(line) = find_line(
            src,
            "let history_list_key = history_key(phone_number_id, recipient_phone);",
        ) {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: whatsapp_path.display().to_string(),
                line: Some(line),
                detail: "outbound hot state uses canonical history key".to_string(),
            });
        }
        if let Some(line) = find_line(
            src,
            "let runtime_identity = resolve_tenant_message_identity(",
        )
        .or_else(|| find_line(src, "ctx.conversation_id.as_deref()"))
        {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: whatsapp_path.display().to_string(),
                line: Some(line),
                detail:
                    "outbound producer resolves and forwards canonical identity before runtime-stream writes"
                        .to_string(),
            });
        }
        if let Some(line) = find_line(
            src,
            "let stream_entry_id = runtime_streams::append_outbound_message(",
        ) {
            evidence.push(EvidenceItem {
                kind: "session_hot_state".to_string(),
                path: whatsapp_path.display().to_string(),
                line: Some(line),
                detail: "outbound message producer passes canonical identity explicitly into the runtime stream"
                    .to_string(),
            });
        }
    }

    entities.push(json!({
        "path": session_identity_path.display().to_string(),
        "has_session_identity_helpers": has_session_identity_helpers,
    }));
    entities.push(json!({
        "path": sessions_path.display().to_string(),
        "has_canonical_helpers": has_canonical_helpers,
    }));
    entities.push(json!({
        "path": py_session_identity_path.display().to_string(),
        "has_inbound_identity_helpers": py_has_inbound_identity_helpers,
        "has_session_identity_resolver": py_has_session_identity_resolver,
    }));
    entities.push(json!({
        "path": py_tasks_path.display().to_string(),
        "prefers_canonical_inbound_ids": py_tasks_prefer_canonical_inbound_ids,
        "builds_step_handoff_identity": py_tasks_build_step_handoff_identity,
        "handoff_event_dual_writes_identity": py_tasks_handoff_event_dual_writes_identity,
    }));
    entities.push(json!({
        "path": py_handoff_path.display().to_string(),
        "dual_writes_identity": py_handoff_stream_dual_writes_identity,
    }));
    entities.push(json!({
        "path": py_ops_path.display().to_string(),
        "resolution_stream_dual_writes_identity": py_ops_resolution_stream_dual_writes_identity,
    }));
    entities.push(json!({
        "path": routing_path.display().to_string(),
        "exposes_resolved_route": routing_exposes_resolved_route,
    }));
    entities.push(json!({
        "path": messages_path.display().to_string(),
        "gateway_cold_store_persists_canonical_identity": gateway_message_cold_store_persists_canonical_identity,
    }));
    entities.push(json!({
        "path": ingest_whatsapp_path.display().to_string(),
        "imports_helpers": ingest_imports_helpers,
        "mirrors_canonical_hot_state": ingest_mirrors_canonical_hot_state,
        "reads_canonical_history_first": ingest_reads_canonical_history_first,
        "passes_explicit_message_identity": ingest_passes_explicit_message_identity,
        "uses_resolved_route_identity": ingest_uses_resolved_route_identity,
    }));
    entities.push(json!({
        "path": whatsapp_path.display().to_string(),
        "imports_helpers": whatsapp_imports_helpers,
        "uses_canonical_keys": whatsapp_uses_canonical_keys,
        "passes_explicit_message_identity": whatsapp_passes_explicit_message_identity,
        "uses_legacy_session_id_layout": whatsapp_uses_legacy_session_id_layout,
    }));
    entities.push(json!({
        "path": ops_messaging_path.display().to_string(),
        "uses_canonical_phone_identity": ops_messaging_uses_canonical_phone_identity,
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_session_hot_state"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked gateway session hot-state layout, found {} warnings",
            warnings.len()
        ),
        confidence: if warnings.is_empty() { 0.96 } else { 0.68 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}
