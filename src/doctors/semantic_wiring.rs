use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct SemanticWiringDoctor;

impl Doctor for SemanticWiringDoctor {
    fn name(&self) -> &'static str {
        "semantic-wiring"
    }

    fn description(&self) -> &'static str {
        "Checks semantic event-store wiring against the intended semantic DB path."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_semantic_wiring(index, root)
    }
}

pub fn doctor_semantic_wiring(_index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let main_path = root.join("example-gateway/src/main.rs");
    let event_store_path = root.join("example-gateway/src/semantic/event_store.rs");
    let handler_paths = [
        root.join("example-gateway/src/ingest/whatsapp/mod.rs"),
        root.join("example-gateway/src/server/handlers/agents.rs"),
        root.join("example-gateway/src/api/agents/executions.rs"),
        root.join("example-gateway/src/api/agents/handlers.rs"),
        root.join("example-gateway/src/ops_console/routes/search.rs"),
    ];

    let main_src = read_text(&main_path, &mut warnings);
    let event_store_src = read_text(&event_store_path, &mut warnings);

    let has_semantic_queue = main_src
        .as_deref()
        .is_some_and(|src| src.contains("let semantic_db_write_queue"));
    let store_uses_semantic_queue = main_src.as_deref().is_some_and(|src| {
        src.contains("SemanticEventStore::new(") && src.contains("semantic_db_write_queue.clone()")
    });
    let store_constructor_is_queue_only = event_store_src
        .as_deref()
        .is_some_and(|src| src.contains("pub fn new(wq: DbWriteQueue) -> Self"));
    let store_still_has_db_field = event_store_src
        .as_deref()
        .is_some_and(has_semantic_store_db_field);

    if !has_semantic_queue {
        warnings.push("main.rs does not define a dedicated semantic_db_write_queue".to_string());
    }
    if !store_uses_semantic_queue {
        warnings.push(
            "main.rs does not wire SemanticEventStore to semantic_db_write_queue".to_string(),
        );
    }
    if !store_constructor_is_queue_only {
        warnings.push("SemanticEventStore constructor is not queue-only".to_string());
    }
    if store_still_has_db_field {
        warnings.push(
            "SemanticEventStore still carries a direct db field, which reopens wiring ambiguity"
                .to_string(),
        );
    }

    if let Some(src) = &main_src {
        if let Some(line) = find_line(src, "let semantic_db_write_queue") {
            evidence.push(EvidenceItem {
                kind: "semantic_wiring".to_string(),
                path: main_path.display().to_string(),
                line: Some(line),
                detail: "dedicated semantic write queue".to_string(),
            });
        }
        if let Some(line) = find_line(src, "SemanticEventStore::new(") {
            evidence.push(EvidenceItem {
                kind: "semantic_wiring".to_string(),
                path: main_path.display().to_string(),
                line: Some(line),
                detail: "shared semantic event store initialization".to_string(),
            });
        }
    }

    if let Some(src) = &event_store_src
        && let Some(line) = find_line(src, "pub fn new(wq: DbWriteQueue) -> Self")
    {
        evidence.push(EvidenceItem {
            kind: "semantic_wiring".to_string(),
            path: event_store_path.display().to_string(),
            line: Some(line),
            detail: "queue-only semantic event store constructor".to_string(),
        });
    }

    let mut reconstructed_handlers = Vec::new();
    for path in handler_paths {
        if let Some(src) = read_text(&path, &mut warnings) {
            let reconstructs = src.contains("SemanticEventStore::new(");
            let uses_shared_store = src.contains("state.semantic_event_store.as_ref()");
            if reconstructs {
                warnings.push(format!(
                    "{} reconstructs SemanticEventStore locally",
                    path.display()
                ));
            }
            entities.push(json!({
                "path": path.display().to_string(),
                "reconstructs_store": reconstructs,
                "uses_shared_store": uses_shared_store,
            }));
            if uses_shared_store
                && let Some(line) = find_line(&src, "state.semantic_event_store.as_ref()")
            {
                evidence.push(EvidenceItem {
                    kind: "semantic_wiring".to_string(),
                    path: path.display().to_string(),
                    line: Some(line),
                    detail: "handler uses shared semantic store".to_string(),
                });
            }
            if reconstructs {
                reconstructed_handlers.push(path.display().to_string());
            }
        }
    }

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_semantic_wiring"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked semantic event wiring, found {} warnings and {} handler regressions",
            warnings.len(),
            reconstructed_handlers.len()
        ),
        confidence: if warnings.is_empty() { 0.96 } else { 0.65 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn has_semantic_store_db_field(src: &str) -> bool {
    let Some(struct_start) = src.find("pub struct SemanticEventStore") else {
        return false;
    };
    let Some(body_offset) = src[struct_start..].find('{') else {
        return false;
    };
    let body_start = struct_start + body_offset + 1;
    let Some(body_end_offset) = src[body_start..].find('}') else {
        return false;
    };
    let body = &src[body_start..body_start + body_end_offset];
    body.lines()
        .map(str::trim)
        .any(|line| line.starts_with("db:") || line.starts_with("pub db:"))
}
