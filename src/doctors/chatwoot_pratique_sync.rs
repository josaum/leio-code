//! Pratique Chatwoot realtime sync doctor.
//!
//! Production drift observed 2026-05-05: Chatwoot account/inbox config can exist
//! in ops/UI intent but the active Python WhatsApp webhook/Celery path must also
//! enqueue inbound/outbound sync, register the Celery task, expose Pratique's
//! account 99521 + inbox 1606 defaults, and require an API token via env/Redis.
//! This doctor is file-local and pre-deploy: it catches code/config wiring drift;
//! operators still need to provision `PRATIQUE_CHATWOOT_API_TOKEN` or Redis
//! `chatwoot:config:{phone_number_id}.api_token` in production.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct ChatwootPratiqueSyncDoctor;

impl Doctor for ChatwootPratiqueSyncDoctor {
    fn name(&self) -> &'static str {
        "chatwoot-pratique-sync"
    }

    fn description(&self) -> &'static str {
        "Checks that Pratique Cobrança WhatsApp messages are wired into Chatwoot account 99521 / inbox 1606 from the active Python webhook/Celery path."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_chatwoot_pratique_sync(root)
    }
}

struct Check<'a> {
    path: &'a str,
    needles: &'a [&'a str],
    label: &'a str,
}

pub fn doctor_chatwoot_pratique_sync(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut entities = Vec::new();

    let checks = [
        Check {
            path: "example-api/example/integrations/chatwoot_sync.py",
            label: "chatwoot sync integration module",
            needles: &[
                "PRATIQUE_PHONE_NUMBER_ID = \"451129211414233\"",
                "PRATIQUE_ACCOUNT_ID = \"99521\"",
                "PRATIQUE_INBOX_ID = \"1606\"",
                "TASK_NAME = \"tasks.chatwoot.sync_message\"",
                "chatwoot:config:{phone_id}",
                "chatwoot_api_token",
                "chatwoot_account_id",
                "chatwoot_inbox_id",
                "PRATIQUE_CHATWOOT_API_TOKEN",
                "CHATWOOT_API_TOKEN",
                "public/api/v1/inboxes",
                "_resolve_api_inbox_identifier",
                "_public_find_or_create_contact",
                "_public_find_or_create_conversation",
                "enqueue_inbound_message",
                "enqueue_outbound_message",
            ],
        },
        Check {
            path: "example-api/example/celery_app/app.py",
            label: "Celery task registration",
            needles: &["example.integrations.chatwoot_sync"],
        },
        Check {
            path: "example-api/example/integrations/whatsapp/routers/webhook.py",
            label: "inbound webhook enqueue",
            needles: &[
                "enqueue_inbound_message",
                "phone_number_id=phone_id",
                "message_id=message_id",
            ],
        },
        Check {
            path: "example-api/example/integrations/whatsapp/tool.py",
            label: "outbound tool enqueue",
            needles: &["enqueue_outbound_message", "handoff_id=response.handoff_id"],
        },
        Check {
            path: "example-api/example/routers/ops_console.py",
            label: "ops UI mirrors Chatwoot config to chatwoot:config:{phone_id} for Celery sync",
            needles: &[
                "_mirror_chatwoot_config_for_celery_sync",
                "chatwoot:config:",
                "_CHATWOOT_CELERY_SYNC_CONFIG_PREFIX",
            ],
        },
        Check {
            path: "example-api/scripts/backfill_chatwoot_history.py",
            label: "official Chatwoot history backfill writes into operational sessions/messages",
            needles: &[
                "Backfill Chatwoot inbox history into the operational Example DuckDB",
                "build_conversation_id",
                "stable_message_id",
                "sessions",
                "messages",
                "73098",
                "49108",
            ],
        },
        Check {
            path: "example-api/docker-compose.yml",
            label: "compose Chatwoot/Pratique env defaults",
            needles: &[
                "CHATWOOT_BASE_URL: ${CHATWOOT_BASE_URL:-https://app.chatwoot.com}",
                "CHATWOOT_API_TOKEN: ${CHATWOOT_API_TOKEN:-}",
                "CHATWOOT_SYNC_QUEUE: ${CHATWOOT_SYNC_QUEUE:-chatwoot_sync}",
                "worker-chatwoot:",
                "-Q chatwoot_sync",
                "PRATIQUE_CHATWOOT_ENABLED: ${PRATIQUE_CHATWOOT_ENABLED:-true}",
                "PRATIQUE_CHATWOOT_ACCOUNT_ID: ${PRATIQUE_CHATWOOT_ACCOUNT_ID:-99521}",
                "PRATIQUE_CHATWOOT_INBOX_ID: ${PRATIQUE_CHATWOOT_INBOX_ID:-1606}",
                "PRATIQUE_CHATWOOT_API_TOKEN: ${PRATIQUE_CHATWOOT_API_TOKEN:-}",
            ],
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
                kind: "chatwoot_pratique_sync_missing_file".to_string(),
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
        } else {
            warnings.push(format!(
                "{} drift in {}: missing {}",
                check.label,
                check.path,
                missing.join(", ")
            ));
            evidence.push(EvidenceItem {
                kind: "chatwoot_pratique_sync_missing_wiring".to_string(),
                path: check.path.to_string(),
                line: None,
                detail: format!("missing: {}", missing.join(", ")),
            });
        }
    }

    entities.push(json!({
        "doctor": "chatwoot-pratique-sync",
        "phone_number_id": "451129211414233",
        "chatwoot_account_id": "99521",
        "chatwoot_inbox_id": "1606",
        "checks_passed": passed,
        "checks_total": checks_total,
        "runtime_secret_required": "PRATIQUE_CHATWOOT_API_TOKEN or Redis chatwoot:config:{phone_number_id}.api_token",
        "production_probe": "docker exec example-worker-pratique-1 python - <<'PY' ... _resolve_config('451129211414233') ... PY",
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_chatwoot_pratique_sync"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            "Pratique WhatsApp → Chatwoot realtime sync wiring is present; runtime still requires a Chatwoot API token".to_string()
        } else {
            format!(
                "Pratique Chatwoot sync wiring drift: {} warning(s)",
                warnings.len()
            )
        },
        confidence: if warnings.is_empty() { 0.93 } else { 0.62 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "runtime_secret_required": true,
            "redis_config_key": "chatwoot:config:451129211414233",
            "task_name": "tasks.chatwoot.sync_message",
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "leio_chatwoot_pratique_sync_{}_{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn write(path: &Path, rel: &str, body: &str) {
        let full = path.join(rel);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, body).unwrap();
    }

    #[test]
    fn flags_missing_sync_module() {
        let tmp = temp_root("missing");
        let env = doctor_chatwoot_pratique_sync(&tmp);
        assert!(!env.warnings.is_empty());
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("chatwoot sync integration module"))
        );
    }

    #[test]
    fn passes_minimal_hardened_wiring() {
        let tmp = temp_root("passes");
        write(
            &tmp,
            "example-api/example/integrations/chatwoot_sync.py",
            r#"PRATIQUE_PHONE_NUMBER_ID = "451129211414233"
PRATIQUE_ACCOUNT_ID = "99521"
PRATIQUE_INBOX_ID = "1606"
TASK_NAME = "tasks.chatwoot.sync_message"
key = f"chatwoot:config:{phone_id}"
redis_cfg.get("chatwoot_api_token")
redis_cfg.get("chatwoot_account_id")
redis_cfg.get("chatwoot_inbox_id")
public/api/v1/inboxes
_resolve_api_inbox_identifier
_public_find_or_create_contact
_public_find_or_create_conversation
PRATIQUE_CHATWOOT_API_TOKEN
CHATWOOT_API_TOKEN
def enqueue_inbound_message(): pass
def enqueue_outbound_message(): pass
"#,
        );
        write(
            &tmp,
            "example-api/example/celery_app/app.py",
            "example.integrations.chatwoot_sync",
        );
        write(
            &tmp,
            "example-api/example/integrations/whatsapp/routers/webhook.py",
            "enqueue_inbound_message phone_number_id=phone_id message_id=message_id",
        );
        write(
            &tmp,
            "example-api/example/integrations/whatsapp/tool.py",
            "enqueue_outbound_message handoff_id=response.handoff_id",
        );
        write(
            &tmp,
            "example-api/example/agents/tasks.py",
            "enqueue_outbound_message metadata={\"source\": \"fallback\", \"trigger_message_id\": trigger_message_id}",
        );
        write(
            &tmp,
            "example-api/example/routers/ops_console.py",
            "_mirror_chatwoot_config_for_celery_sync chatwoot:config: _CHATWOOT_CELERY_SYNC_CONFIG_PREFIX",
        );
        write(
            &tmp,
            "example-api/scripts/backfill_chatwoot_history.py",
            "Backfill Chatwoot inbox history into the operational Example DuckDB build_conversation_id stable_message_id sessions messages 73098 49108",
        );
        write(
            &tmp,
            "example-api/docker-compose.yml",
            r#"CHATWOOT_BASE_URL: ${CHATWOOT_BASE_URL:-https://app.chatwoot.com}
CHATWOOT_API_TOKEN: ${CHATWOOT_API_TOKEN:-}
CHATWOOT_SYNC_QUEUE: ${CHATWOOT_SYNC_QUEUE:-chatwoot_sync}
worker-chatwoot:
-Q chatwoot_sync
PRATIQUE_CHATWOOT_ENABLED: ${PRATIQUE_CHATWOOT_ENABLED:-true}
PRATIQUE_CHATWOOT_ACCOUNT_ID: ${PRATIQUE_CHATWOOT_ACCOUNT_ID:-99521}
PRATIQUE_CHATWOOT_INBOX_ID: ${PRATIQUE_CHATWOOT_INBOX_ID:-1606}
PRATIQUE_CHATWOOT_API_TOKEN: ${PRATIQUE_CHATWOOT_API_TOKEN:-}
"#,
        );
        let env = doctor_chatwoot_pratique_sync(&tmp);
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
    }
}
