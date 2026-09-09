use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct ConversationIdentityDoctor;

impl Doctor for ConversationIdentityDoctor {
    fn name(&self) -> &'static str {
        "conversation-identity"
    }

    fn description(&self) -> &'static str {
        "Checks that conversation_id is the canonical primary identity across Rust and Python: \
         reverse indexes exist, DuckDB schema includes conversation_id, handoff tickets carry it, \
         event normalization auto-extracts it, and tenant validation accepts it."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_conversation_identity(root)
    }
}

pub fn doctor_conversation_identity(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    // -----------------------------------------------------------------------
    // Rust: session_identity.rs — must expose tenant validation
    // -----------------------------------------------------------------------
    let rust_identity_path = root.join("example-gateway/src/session_identity.rs");
    let rust_identity_src = read_text(&rust_identity_path, &mut warnings);

    let has_conversation_tenant_validator = rust_identity_src
        .as_deref()
        .is_some_and(|src| src.contains("fn conversation_id_belongs_to_tenant("));

    if !has_conversation_tenant_validator {
        warnings.push(
            "session_identity.rs missing conversation_id_belongs_to_tenant() validator".into(),
        );
    } else if let (Some(src), Some(line)) = (
        rust_identity_src.as_deref(),
        rust_identity_src
            .as_deref()
            .and_then(|s| find_line(s, "fn conversation_id_belongs_to_tenant(")),
    ) {
        let _ = src;
        evidence.push(EvidenceItem {
            kind: "conversation_identity".into(),
            path: rust_identity_path.display().to_string(),
            line: Some(line),
            detail: "Rust exposes conversation_id tenant validator".into(),
        });
    }

    entities.push(json!({
        "path": rust_identity_path.display().to_string(),
        "has_conversation_tenant_validator": has_conversation_tenant_validator,
    }));

    // -----------------------------------------------------------------------
    // Rust: sessions.rs — must have reverse index + load_by_conversation_id
    // -----------------------------------------------------------------------
    let rust_sessions_path = root.join("example-gateway/src/sessions.rs");
    let rust_sessions_src = read_text(&rust_sessions_path, &mut warnings);

    let has_reverse_key = rust_sessions_src
        .as_deref()
        .is_some_and(|src| src.contains("fn conversation_reverse_key("));
    let has_load_by_conversation = rust_sessions_src
        .as_deref()
        .is_some_and(|src| src.contains("pub async fn load_by_conversation_id("));
    let archives_conversation_id = rust_sessions_src
        .as_deref()
        .is_some_and(|src| src.contains("conversation_id") && src.contains("archive_to_duckdb"));

    if !has_reverse_key {
        warnings.push("sessions.rs missing conversation_reverse_key() helper".into());
    }
    if !has_load_by_conversation {
        warnings.push("sessions.rs missing load_by_conversation_id() method".into());
    }

    if let Some(line) = rust_sessions_src
        .as_deref()
        .and_then(|s| find_line(s, "fn load_by_conversation_id("))
    {
        evidence.push(EvidenceItem {
            kind: "conversation_identity".into(),
            path: rust_sessions_path.display().to_string(),
            line: Some(line),
            detail: "SessionManager exposes conversation_id lookup".into(),
        });
    }
    if let Some(line) = rust_sessions_src
        .as_deref()
        .and_then(|s| find_line(s, "fn conversation_reverse_key("))
    {
        evidence.push(EvidenceItem {
            kind: "conversation_identity".into(),
            path: rust_sessions_path.display().to_string(),
            line: Some(line),
            detail: "Redis reverse index key helper for conversation_id".into(),
        });
    }

    entities.push(json!({
        "path": rust_sessions_path.display().to_string(),
        "has_reverse_key": has_reverse_key,
        "has_load_by_conversation_id": has_load_by_conversation,
        "archives_conversation_id": archives_conversation_id,
    }));

    // -----------------------------------------------------------------------
    // Rust: DuckDB schema — sessions table must have conversation_id column
    // -----------------------------------------------------------------------
    let schema_path = root.join("example-gateway/src/storage/schema.sql");
    let schema_src = read_text(&schema_path, &mut warnings);

    let schema_has_conversation_id = schema_src.as_deref().is_some_and(|src| {
        src.contains("conversation_id") && src.contains("idx_sessions_conversation_id")
    });

    if !schema_has_conversation_id {
        warnings
            .push("storage/schema.sql sessions table missing conversation_id column/index".into());
    } else if let Some(line) = schema_src
        .as_deref()
        .and_then(|s| find_line(s, "idx_sessions_conversation_id"))
    {
        evidence.push(EvidenceItem {
            kind: "conversation_identity".into(),
            path: schema_path.display().to_string(),
            line: Some(line),
            detail: "DuckDB sessions table has conversation_id unique index".into(),
        });
    }

    entities.push(json!({
        "path": schema_path.display().to_string(),
        "schema_has_conversation_id": schema_has_conversation_id,
    }));

    // -----------------------------------------------------------------------
    // Rust: handoff.rs — HandoffTicket must carry conversation_id
    // -----------------------------------------------------------------------
    let handoff_path = root.join("example-gateway/src/handoff.rs");
    let handoff_src = read_text(&handoff_path, &mut warnings);

    let handoff_has_conversation_id = handoff_src
        .as_deref()
        .is_some_and(|src| src.contains("pub conversation_id: Option<String>"));

    if !handoff_has_conversation_id {
        warnings.push("handoff.rs HandoffTicket missing conversation_id field".into());
    } else if let Some(line) = handoff_src
        .as_deref()
        .and_then(|s| find_line(s, "pub conversation_id: Option<String>"))
    {
        evidence.push(EvidenceItem {
            kind: "conversation_identity".into(),
            path: handoff_path.display().to_string(),
            line: Some(line),
            detail: "HandoffTicket carries canonical conversation_id".into(),
        });
    }

    entities.push(json!({
        "path": handoff_path.display().to_string(),
        "handoff_has_conversation_id": handoff_has_conversation_id,
    }));

    // -----------------------------------------------------------------------
    // Python: session_identity.py — must expose conversation_id_belongs_to_tenant
    // -----------------------------------------------------------------------
    let py_identity_path =
        root.join("example-api/example/integrations/whatsapp/session_identity.py");
    let py_identity_src = read_text(&py_identity_path, &mut warnings);

    let py_has_tenant_validator = py_identity_src
        .as_deref()
        .is_some_and(|src| src.contains("def conversation_id_belongs_to_tenant("));

    if !py_has_tenant_validator {
        warnings.push(
            "session_identity.py missing conversation_id_belongs_to_tenant() validator".into(),
        );
    } else if let Some(line) = py_identity_src
        .as_deref()
        .and_then(|s| find_line(s, "def conversation_id_belongs_to_tenant("))
    {
        evidence.push(EvidenceItem {
            kind: "conversation_identity".into(),
            path: py_identity_path.display().to_string(),
            line: Some(line),
            detail: "Python exposes conversation_id tenant validator".into(),
        });
    }

    entities.push(json!({
        "path": py_identity_path.display().to_string(),
        "py_has_tenant_validator": py_has_tenant_validator,
    }));

    // -----------------------------------------------------------------------
    // Python: agents/session.py — must have reverse index
    // -----------------------------------------------------------------------
    let py_session_path = root.join("example-api/example/agents/session.py");
    let py_session_src = read_text(&py_session_path, &mut warnings);

    let py_has_reverse_index = py_session_src
        .as_deref()
        .is_some_and(|src| src.contains("conversation_session:"));
    let py_has_from_conversation = py_session_src
        .as_deref()
        .is_some_and(|src| src.contains("def from_conversation_id("));

    if !py_has_reverse_index {
        warnings.push("agents/session.py missing conversation_session reverse index".into());
    }
    if !py_has_from_conversation {
        warnings.push("agents/session.py missing from_conversation_id() class method".into());
    }

    if let Some(line) = py_session_src
        .as_deref()
        .and_then(|s| find_line(s, "def from_conversation_id("))
    {
        evidence.push(EvidenceItem {
            kind: "conversation_identity".into(),
            path: py_session_path.display().to_string(),
            line: Some(line),
            detail: "Python RedisSession supports lookup by conversation_id".into(),
        });
    }

    entities.push(json!({
        "path": py_session_path.display().to_string(),
        "py_has_reverse_index": py_has_reverse_index,
        "py_has_from_conversation_id": py_has_from_conversation,
    }));

    // -----------------------------------------------------------------------
    // Python: events.py — must auto-extract conversation_id
    // -----------------------------------------------------------------------
    let py_events_path = root.join("example-api/example/events.py");
    let py_events_src = read_text(&py_events_path, &mut warnings);

    let py_events_has_conversation_in_reserved = py_events_src.as_deref().is_some_and(|src| {
        src.contains("\"conversation_id\"") && src.contains("_RESERVED_EVENT_KEYS")
    });
    let py_events_auto_extracts = py_events_src
        .as_deref()
        .is_some_and(|src| src.contains("conversation_id_from_session_id"));

    if !py_events_has_conversation_in_reserved {
        warnings.push("events.py missing conversation_id in _RESERVED_EVENT_KEYS".into());
    }
    if !py_events_auto_extracts {
        warnings.push("events.py does not auto-extract conversation_id from session_id".into());
    }

    if let Some(line) = py_events_src
        .as_deref()
        .and_then(|s| find_line(s, "conversation_id_from_session_id"))
    {
        evidence.push(EvidenceItem {
            kind: "conversation_identity".into(),
            path: py_events_path.display().to_string(),
            line: Some(line),
            detail: "Event normalization auto-extracts conversation_id from session_id".into(),
        });
    }

    entities.push(json!({
        "path": py_events_path.display().to_string(),
        "conversation_id_in_reserved_keys": py_events_has_conversation_in_reserved,
        "auto_extracts_conversation_id": py_events_auto_extracts,
    }));

    // -----------------------------------------------------------------------
    // Python: routers/ops.py — must accept conversation_id validation
    // -----------------------------------------------------------------------
    let py_ops_path = root.join("example-api/example/routers/ops.py");
    let py_ops_src = read_text(&py_ops_path, &mut warnings);

    let py_ops_uses_conversation_validation = py_ops_src
        .as_deref()
        .is_some_and(|src| src.contains("conversation_id_belongs_to_tenant"));

    if !py_ops_uses_conversation_validation {
        warnings
            .push("routers/ops.py does not validate conversation_id for tenant ownership".into());
    } else if let Some(line) = py_ops_src
        .as_deref()
        .and_then(|s| find_line(s, "conversation_id_belongs_to_tenant"))
    {
        evidence.push(EvidenceItem {
            kind: "conversation_identity".into(),
            path: py_ops_path.display().to_string(),
            line: Some(line),
            detail: "Ops validation accepts conversation_id for tenant ownership check".into(),
        });
    }

    entities.push(json!({
        "path": py_ops_path.display().to_string(),
        "uses_conversation_id_validation": py_ops_uses_conversation_validation,
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_conversation_identity"),
        kind: "doctor".into(),
        summary: format!(
            "checked conversation identity canonicalization, found {} warnings",
            warnings.len()
        ),
        confidence: if warnings.is_empty() { 0.96 } else { 0.65 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}
