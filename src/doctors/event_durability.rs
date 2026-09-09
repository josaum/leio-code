use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct EventDurabilityDoctor;

impl Doctor for EventDurabilityDoctor {
    fn name(&self) -> &'static str {
        "event-durability"
    }

    fn description(&self) -> &'static str {
        "Checks that gateway event producers stay on the Redis-first durability path and preserve canonical event identity."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_event_durability(root)
    }
}

pub fn doctor_event_durability(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let streaming_path = root.join("example-gateway/src/events/streaming.rs");
    let manager_path = root.join("example-gateway/src/leases/manager.rs");
    let runtime_streams_path = root.join("example-gateway/src/runtime_streams.rs");
    let gateway_workers_path = root.join("example-gateway/src/server/workers.rs");
    let event_store_path = root.join("example-gateway/src/events/storage/store.rs");
    let py_drain_worker_path = root.join("example-api/example/core/drain_worker.py");

    let streaming_src = read_text(&streaming_path, &mut warnings);
    let manager_src = read_text(&manager_path, &mut warnings);
    let runtime_streams_src = read_text(&runtime_streams_path, &mut warnings);
    let gateway_workers_src = read_text(&gateway_workers_path, &mut warnings);
    let event_store_src = read_text(&event_store_path, &mut warnings);
    let py_drain_worker_src = read_text(&py_drain_worker_path, &mut warnings);

    let streaming_is_redis_first = streaming_src.as_deref().is_some_and(|src| {
        src.contains("runtime_streams::append_event(redis, &event).await")
            && src.contains("db_write_queue.fire_and_forget(\"domain_event\"")
            && src.contains("Redis unavailable; falling back to direct DuckDB domain_events append")
    });
    let lease_manager_bootstraps_queue = manager_src.as_deref().is_some_and(|src| {
        src.contains("DbWriteQueueConfig::from_env()")
            && src.contains(
                ".map(|_| DbWriteQueue::new(db.clone(), DbWriteQueueConfig::from_env()));",
            )
    });
    let lease_manager_uses_streaming_helper = manager_src.as_deref().is_some_and(|src| {
        src.contains("events::append_event_streamed(redis, db_write_queue.as_ref(), event, label)")
            && src.contains("db_write_queue.fire_and_forget(\"lease_event\"")
            && src.contains(
                "Tokio runtime unavailable; enqueueing {label} through the write queue fallback",
            )
    });
    let lease_manager_still_appends_directly = manager_src
        .as_deref()
        .is_some_and(|src| src.contains("events::append_event(&db, &event)"));
    let runtime_streams_dual_write_event_identity =
        runtime_streams_src.as_deref().is_some_and(|src| {
            src.contains(
                "fn resolve_event_identity_fields(event: &EventEnvelope) -> MessageIdentityFields",
            ) && src.contains("(\"conversation_id\", identity.conversation_id.as_str())")
                && src.contains("(\"memory_session_id\", identity.memory_session_id.as_str())")
                && src.contains("(\"root_agent_id\", identity.root_agent_id.as_str())")
                && src.contains("(\"workflow_root_id\", identity.workflow_root_id.as_str())")
        });
    let gateway_workers_persist_event_identity =
        gateway_workers_src.as_deref().is_some_and(|src| {
            src.contains("conversation_id: entry.get::<String>(\"conversation_id\").and_then(non_empty),")
                && src.contains("memory_session_id: entry.get::<String>(\"memory_session_id\").and_then(non_empty),")
                && src.contains("root_agent_id: entry.get::<String>(\"root_agent_id\").and_then(non_empty),")
                && src.contains("workflow_root_id: entry.get::<String>(\"workflow_root_id\").and_then(non_empty),")
                && src.contains("event.conversation_id.as_deref()")
                && src.contains("event.memory_session_id.as_deref()")
                && src.contains("event.root_agent_id.as_deref()")
                && src.contains("event.workflow_root_id.as_deref()")
        });
    let event_store_persists_event_identity = event_store_src.as_deref().is_some_and(|src| {
        src.contains(
            "fn resolve_event_identity_fields(event: &EventEnvelope) -> EventIdentityFields",
        ) && src.contains("conversation_id, memory_session_id, root_agent_id, workflow_root_id")
            && src.contains("identity.conversation_id.as_deref()")
            && src.contains("identity.memory_session_id.as_deref()")
            && src.contains("identity.root_agent_id.as_deref()")
            && src.contains("identity.workflow_root_id.as_deref()")
    });
    let py_drain_worker_persists_event_identity =
        py_drain_worker_src.as_deref().is_some_and(|src| {
            src.contains("conversation_id = fields.get(\"conversation_id\", \"\") or None")
                && src
                    .contains("memory_session_id = fields.get(\"memory_session_id\", \"\") or None")
                && src.contains("root_agent_id = fields.get(\"root_agent_id\", \"\") or None")
                && src.contains("workflow_root_id = fields.get(\"workflow_root_id\", \"\") or None")
                && src.contains(
                    "event_id, ts_utc, tenant_id, session_id, conversation_id, memory_session_id,",
                )
                && src.contains(
                    "ALTER TABLE {table} ADD COLUMN IF NOT EXISTS conversation_id VARCHAR;",
                )
                && src.contains(
                    "ALTER TABLE {table} ADD COLUMN IF NOT EXISTS memory_session_id VARCHAR;",
                )
                && src
                    .contains("ALTER TABLE {table} ADD COLUMN IF NOT EXISTS root_agent_id VARCHAR;")
                && src.contains(
                    "ALTER TABLE {table} ADD COLUMN IF NOT EXISTS workflow_root_id VARCHAR;",
                )
        });

    if !streaming_is_redis_first {
        warnings.push(
            "events/streaming.rs does not expose the expected Redis-first event durability path"
                .to_string(),
        );
    }
    if !lease_manager_bootstraps_queue {
        warnings.push(
            "LeaseManager::new() does not bootstrap a DbWriteQueue for legacy non-Redis paths"
                .to_string(),
        );
    }
    if !lease_manager_uses_streaming_helper {
        warnings.push(
            "LeaseManager does not route lease lifecycle events through the shared event durability helpers"
                .to_string(),
        );
    }
    if lease_manager_still_appends_directly {
        warnings.push(
            "LeaseManager still appends events directly to DuckDB instead of using the shared Redis-first durability path"
                .to_string(),
        );
    }
    if !runtime_streams_dual_write_event_identity {
        warnings.push(
            "runtime_streams.rs does not dual-write canonical conversation/session/root identity into events:{tenant}"
                .to_string(),
        );
    }
    if !gateway_workers_persist_event_identity {
        warnings.push(
            "gateway Redis drain worker does not persist canonical event identity fields into DuckDB domain_events"
                .to_string(),
        );
    }
    if !event_store_persists_event_identity {
        warnings.push(
            "gateway direct domain event store appends do not persist canonical event identity fields"
                .to_string(),
        );
    }
    if !py_drain_worker_persists_event_identity {
        warnings.push(
            "example-api drain_worker.py does not persist canonical event identity fields into PostgreSQL domain_events"
                .to_string(),
        );
    }

    for (path, src, needle, detail) in [
        (
            &streaming_path,
            streaming_src.as_ref(),
            "runtime_streams::append_event(redis, &event).await",
            "gateway event streaming sends domain events to Redis first",
        ),
        (
            &streaming_path,
            streaming_src.as_ref(),
            "db_write_queue.fire_and_forget(\"domain_event\"",
            "gateway event streaming falls back through the write queue instead of synchronous hot-path writes",
        ),
        (
            &manager_path,
            manager_src.as_ref(),
            "DbWriteQueueConfig::from_env()",
            "LeaseManager bootstraps a write queue for legacy non-streamed construction",
        ),
        (
            &manager_path,
            manager_src.as_ref(),
            "events::append_event_streamed(redis, db_write_queue.as_ref(), event, label)",
            "lease lifecycle events reuse the shared Redis-first event streamer",
        ),
        (
            &manager_path,
            manager_src.as_ref(),
            "db_write_queue.fire_and_forget(\"lease_event\"",
            "lease lifecycle fallback enqueues via the write queue instead of calling DuckDB directly",
        ),
        (
            &runtime_streams_path,
            runtime_streams_src.as_ref(),
            "(\"conversation_id\", identity.conversation_id.as_str())",
            "runtime event streams carry canonical conversation identity fields",
        ),
        (
            &gateway_workers_path,
            gateway_workers_src.as_ref(),
            "conversation_id: entry.get::<String>(\"conversation_id\").and_then(non_empty),",
            "gateway Redis drain reads canonical event identity fields from events:{tenant}",
        ),
        (
            &event_store_path,
            event_store_src.as_ref(),
            "fn resolve_event_identity_fields(event: &EventEnvelope) -> EventIdentityFields",
            "direct DuckDB event storage derives/persists canonical identity fields",
        ),
        (
            &py_drain_worker_path,
            py_drain_worker_src.as_ref(),
            "conversation_id = fields.get(\"conversation_id\", \"\") or None",
            "PostgreSQL drain persists canonical event identity fields",
        ),
    ] {
        if let Some(src) = src
            && let Some(line) = find_line(src, needle)
        {
            evidence.push(EvidenceItem {
                kind: "event_durability".to_string(),
                path: path.display().to_string(),
                line: Some(line),
                detail: detail.to_string(),
            });
        }
    }

    entities.push(json!({
        "path": streaming_path.display().to_string(),
        "redis_first": streaming_is_redis_first,
    }));
    entities.push(json!({
        "path": runtime_streams_path.display().to_string(),
        "dual_write_event_identity": runtime_streams_dual_write_event_identity,
    }));
    entities.push(json!({
        "path": manager_path.display().to_string(),
        "bootstraps_queue": lease_manager_bootstraps_queue,
        "uses_streaming_helper": lease_manager_uses_streaming_helper,
        "direct_append_present": lease_manager_still_appends_directly,
    }));
    entities.push(json!({
        "path": gateway_workers_path.display().to_string(),
        "persists_event_identity": gateway_workers_persist_event_identity,
    }));
    entities.push(json!({
        "path": event_store_path.display().to_string(),
        "persists_event_identity": event_store_persists_event_identity,
    }));
    entities.push(json!({
        "path": py_drain_worker_path.display().to_string(),
        "persists_event_identity": py_drain_worker_persists_event_identity,
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_event_durability"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked gateway event durability wiring, found {} warnings",
            warnings.len()
        ),
        confidence: if warnings.is_empty() { 0.97 } else { 0.68 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}
