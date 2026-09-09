//! `compose-worker-mem-budget` doctor.
//!
//! Static drift class: a celery worker that boots from the `example-api`
//! image has its `mem_limit` quietly lowered below the Python import floor
//! and starts OOM-killing in production. The 2026-05-21 outage was triggered
//! by a 384m cap on `worker-beat`; the regression class covers every worker
//! that inherits the same import surface.
//!
//! This doctor codifies the same contract as
//! `example-api/example/tests/misc/test_compose_env_defaults.py::test_api_importing_workers_have_safe_mem_budget`
//! so `leio-code audit --strict` catches the drift in CI even when the
//! Python test suite isn't run.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::query_id;
use crate::diagnostics::{Diagnostic, Severity};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

/// Service-name → minimum-acceptable `mem_limit` mapping. Keep aligned with
/// the Python regression test referenced in the module docs.
const API_IMPORTING_WORKERS: &[(&str, &str)] = &[
    ("worker-beat", "768m"),
    ("worker-chatwoot", "768m"),
    ("worker-predict", "768m"),
    ("worker-sara", "1536m"),
    ("worker-liz", "1536m"),
    ("worker-pratique", "1536m"),
];

const COMPOSE_PATH: &str = "example-api/docker-compose.yml";

pub struct ComposeWorkerMemBudgetDoctor;

impl Doctor for ComposeWorkerMemBudgetDoctor {
    fn name(&self) -> &'static str {
        "compose-worker-mem-budget"
    }

    fn description(&self) -> &'static str {
        "Validates that every celery worker booting from the example-api image \
         meets the minimum memory contract. The 2026-05-21 outage was triggered \
         by a 384m cap on worker-beat that masked as a celery hang; this rule \
         catches the same drift on any sibling worker before it lands."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_compose_worker_mem_budget(root)
    }
}

pub fn doctor_compose_worker_mem_budget(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut evidence: Vec<EvidenceItem> = Vec::new();

    let compose_path = root.join(COMPOSE_PATH);
    let Ok(compose) = std::fs::read_to_string(&compose_path) else {
        // No compose file → workspace doesn't model this facet; emit a
        // single info-level note and exit clean.
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_compose_worker_mem_budget"),
            kind: "doctor".to_string(),
            summary: format!("compose-worker-mem-budget: {COMPOSE_PATH} not present, skipped"),
            confidence: 100.0,
            entities: Vec::new(),
            evidence: Vec::new(),
            warnings: Vec::new(),
            meta: Some(
                json!({ "diagnostics": serde_json::Value::Array(Vec::new()), "skipped": true }),
            ),
            timing_ms: started.elapsed().as_millis(),
        };
    };

    for (service_name, expected_cap) in API_IMPORTING_WORKERS {
        let Some(block) = extract_service_block(&compose, service_name) else {
            diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                rule_id: "compose_worker_mem_budget_missing".to_string(),
                path: Some(COMPOSE_PATH.to_string()),
                line: None,
                message: format!(
                    "expected api-importing worker `{}` not found in {}; the \
                     memory contract may need updating if the worker was \
                     renamed or removed.",
                    service_name, COMPOSE_PATH
                ),
            });
            continue;
        };

        // Confirm the service actually boots from the API image.
        let uses_api_image =
            block.contains("jquant/example-api") || block.contains("EXAMPLE_API_IMAGE");
        if !uses_api_image {
            diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                rule_id: "compose_worker_mem_budget_decoupled".to_string(),
                path: Some(COMPOSE_PATH.to_string()),
                line: line_of_service_header(&compose, service_name),
                message: format!(
                    "service `{}` is in the api-importing contract but no \
                     longer boots from the example-api image; the contract \
                     entry may be stale.",
                    service_name
                ),
            });
            continue;
        }

        let expected_line = format!("mem_limit: {}", expected_cap);
        if !block.contains(&expected_line) {
            let actual_cap = block
                .lines()
                .find_map(|l| l.trim().strip_prefix("mem_limit: "))
                .unwrap_or("absent");
            let line_no = line_of_pattern(&compose, service_name, "mem_limit:");
            diagnostics.push(Diagnostic {
                severity: Severity::Error,
                rule_id: "compose_worker_mem_budget_too_low".to_string(),
                path: Some(COMPOSE_PATH.to_string()),
                line: line_no,
                message: format!(
                    "service `{}` mem_limit is `{}`; the api-importing \
                     contract requires `{}` to stay above the Python import \
                     floor inherited from the example-api image.",
                    service_name, actual_cap, expected_cap
                ),
            });
            evidence.push(EvidenceItem {
                kind: "compose_worker_mem_budget".to_string(),
                path: COMPOSE_PATH.to_string(),
                line: line_no,
                detail: format!(
                    "{} mem_limit `{}` is below the {} contract minimum",
                    service_name, actual_cap, expected_cap
                ),
            });
        }
    }

    let summary = if diagnostics.is_empty() {
        format!(
            "compose-worker-mem-budget: all {} api-importing workers meet the contract",
            API_IMPORTING_WORKERS.len()
        )
    } else {
        format!(
            "compose-worker-mem-budget: {} drift(s) across {} workers checked",
            diagnostics.len(),
            API_IMPORTING_WORKERS.len()
        )
    };

    let entities = diagnostics
        .iter()
        .map(|d| {
            json!({
                "rule_id": d.rule_id,
                "severity": d.severity.as_sarif_level(),
                "file": d.path,
                "line": d.line,
                "message": d.message,
            })
        })
        .collect();

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_compose_worker_mem_budget"),
        kind: "doctor".to_string(),
        summary,
        confidence: 95.0,
        entities,
        evidence,
        warnings: Vec::new(),
        meta: Some(json!({
            "diagnostics": diagnostics
                .iter()
                .map(|d| json!({
                    "rule_id": d.rule_id,
                    "severity": d.severity.as_sarif_level(),
                    "file": d.path,
                    "line": d.line,
                    "message": d.message,
                }))
                .collect::<Vec<_>>(),
            "workers_checked": API_IMPORTING_WORKERS.len(),
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// Extract the YAML block for a top-level service in a compose file.
/// Returns the lines from `  {service_name}:` up to the next sibling service
/// header or top-level key, exclusive.
fn extract_service_block<'a>(compose: &'a str, service_name: &str) -> Option<&'a str> {
    let needle = format!("  {}:", service_name);
    let start = compose.find(&needle)?;
    let rest = &compose[start + needle.len()..];
    // Find the next sibling service header: a newline followed by exactly
    // two spaces followed by a word character. A four-space-indented line
    // (`\n    image:`) must NOT match.
    let mut end_offset = rest.len();
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i] == b'\n'
            && bytes[i + 1] == b' '
            && bytes[i + 2] == b' '
            && bytes[i + 3] != b' '
            && (bytes[i + 3].is_ascii_alphabetic() || bytes[i + 3] == b'_')
        {
            end_offset = i;
            break;
        }
        i += 1;
    }
    // Also clip on top-level keys (`\nvolumes:`, etc.)
    for marker in ["\nvolumes:", "\nnetworks:", "\nx-"] {
        if let Some(j) = rest.find(marker)
            && j < end_offset
        {
            end_offset = j;
        }
    }
    Some(&compose[start..start + needle.len() + end_offset])
}

fn line_of_service_header(compose: &str, service_name: &str) -> Option<usize> {
    let needle = format!("  {}:", service_name);
    compose
        .find(&needle)
        .map(|offset| compose[..offset].bytes().filter(|b| *b == b'\n').count() + 1)
}

fn line_of_pattern(compose: &str, service_name: &str, pattern: &str) -> Option<usize> {
    let block_start = compose.find(&format!("  {}:", service_name))?;
    let block_offset = compose[block_start..].find(pattern)?;
    let absolute = block_start + block_offset;
    Some(compose[..absolute].bytes().filter(|b| *b == b'\n').count() + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_compose(root: &Path, body: &str) {
        std::fs::create_dir_all(root.join("example-api")).unwrap();
        std::fs::write(root.join(COMPOSE_PATH), body).unwrap();
    }

    fn good_block(name: &str, mem: &str) -> String {
        format!(
            "  {name}:\n    image: ${{EXAMPLE_API_IMAGE:-jquant/example-api:latest}}\n    \
             healthcheck:\n      disable: true\n    mem_limit: {mem}\n"
        )
    }

    #[test]
    fn extract_block_stops_at_next_sibling() {
        let compose = format!(
            "services:\n{}\n{}\n",
            good_block("worker-beat", "768m"),
            good_block("worker-chatwoot", "768m"),
        );
        let block = extract_service_block(&compose, "worker-beat").unwrap();
        assert!(block.contains("mem_limit: 768m"));
        assert!(!block.contains("worker-chatwoot"));
    }

    #[test]
    fn all_workers_well_sized_produces_no_diagnostics() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let blocks: String = API_IMPORTING_WORKERS
            .iter()
            .map(|(n, m)| good_block(n, m))
            .collect();
        write_compose(root, &format!("services:\n{}\n", blocks));
        let env = doctor_compose_worker_mem_budget(root);
        let meta = env.meta.unwrap();
        let diagnostics = meta["diagnostics"].as_array().unwrap();
        assert!(
            diagnostics.is_empty(),
            "expected no diagnostics; got {diagnostics:?}"
        );
    }

    #[test]
    fn too_low_mem_limit_fires() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let mut blocks: String = String::new();
        for (n, m) in API_IMPORTING_WORKERS {
            // Drop worker-beat to 384m to simulate the prod drift.
            let mem = if *n == "worker-beat" { "384m" } else { *m };
            blocks.push_str(&good_block(n, mem));
            blocks.push('\n');
        }
        write_compose(root, &format!("services:\n{}", blocks));
        let env = doctor_compose_worker_mem_budget(root);
        let meta = env.meta.unwrap();
        let diagnostics = meta["diagnostics"].as_array().unwrap();
        let rule_ids: Vec<&str> = diagnostics
            .iter()
            .map(|d| d["rule_id"].as_str().unwrap())
            .collect();
        assert!(
            rule_ids.contains(&"compose_worker_mem_budget_too_low"),
            "expected too-low rule; got {rule_ids:?}"
        );
        // And the rule should specifically flag worker-beat.
        let messages: Vec<&str> = diagnostics
            .iter()
            .map(|d| d["message"].as_str().unwrap())
            .collect();
        assert!(
            messages.iter().any(|m| m.contains("worker-beat")),
            "expected worker-beat in messages; got {messages:?}"
        );
    }

    #[test]
    fn worker_decoupled_from_api_image_fires_decoupled_rule() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let mut blocks: String = String::new();
        for (n, m) in API_IMPORTING_WORKERS {
            if *n == "worker-beat" {
                // Replace API image with a hypothetical standalone image.
                blocks.push_str(&format!(
                    "  {n}:\n    image: jquant/standalone-beat:latest\n    mem_limit: {m}\n\n"
                ));
            } else {
                blocks.push_str(&good_block(n, m));
                blocks.push('\n');
            }
        }
        write_compose(root, &format!("services:\n{}", blocks));
        let env = doctor_compose_worker_mem_budget(root);
        let meta = env.meta.unwrap();
        let rule_ids: Vec<&str> = meta["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["rule_id"].as_str().unwrap())
            .collect();
        assert!(
            rule_ids.contains(&"compose_worker_mem_budget_decoupled"),
            "expected decoupled rule; got {rule_ids:?}"
        );
    }

    #[test]
    fn missing_compose_file_skips_cleanly() {
        let tmp = tempfile::TempDir::new().unwrap();
        let env = doctor_compose_worker_mem_budget(tmp.path());
        let meta = env.meta.unwrap();
        assert!(meta["skipped"].as_bool().unwrap_or(false));
        assert!(env.summary.contains("not present"));
    }
}
