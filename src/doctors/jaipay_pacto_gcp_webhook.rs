//! Jai Pay direct Pacto webhook contract doctor.
//!
//! Regression class: provider webhooks drifting back into the Vercel Jai Pay
//! route and reintroducing Supabase/Prisma pool pressure. The durable contract
//! is direct Pacto -> GCP/Example -> Redis Stream -> GCP worker -> Jai Pay DB
//! + RevOps projections.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct JaipayPactoGcpWebhookDoctor;

impl Doctor for JaipayPactoGcpWebhookDoctor {
    fn name(&self) -> &'static str {
        "jaipay-pacto-gcp-webhook"
    }

    fn description(&self) -> &'static str {
        "Ensures Pacto webhooks terminate in GCP/Example, are durably queued in Redis Streams, and are processed by a GCP worker instead of Vercel."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_jaipay_pacto_gcp_webhook(root)
    }
}

struct Check<'a> {
    path: &'a str,
    label: &'a str,
    needles: &'a [&'a str],
    forbidden: &'a [&'a str],
}

pub fn doctor_jaipay_pacto_gcp_webhook(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut entities = Vec::new();

    let checks = [
        Check {
            path: "example-api/example/integrations/jaipay/webhook.py",
            label: "GCP FastAPI webhook endpoint",
            needles: &[
                "@router.post(\"/pacto/webhook\"",
                "resolve_alias_for_chave",
                "enqueue_pacto_webhook",
                "status.HTTP_202_ACCEPTED",
                "durable ingest unavailable",
            ],
            forbidden: &[],
        },
        Check {
            path: "example-api/example/integrations/jaipay/pacto_webhook_ingest.py",
            label: "GCP durable ingest and processor",
            needles: &[
                "PACTO_STREAM_KEY",
                "PACTO_WEBHOOK_CHAVES",
                "are not webhook chaves",
                "record_observed_pacto_chave",
                "JAIPAY_DATABASE_URL",
                "xreadgroup",
                "xack",
                "MemberPaymentEvent",
                "MemberCheckin",
                "MemberContractEvent",
                "MemberHealthState",
                "GymBowtieState",
                "\"pactoUnitAlias\"",
                "AND m.\"gymId\"=%s",
                "def _billing_scope_clause",
                "JOIN \"BillingContract\" bc",
                "skipping unscoped paid-installment reconciliation",
                "wrap_event_jsonld",
                "events:jaipay",
            ],
            forbidden: &[],
        },
        Check {
            path: "jai-pay/prisma/schema.prisma",
            label: "Jai Pay unit-scoped Pacto event schema",
            needles: &[
                "pactoUnitAlias",
                "@@unique([pactoUnitAlias, pactoEmpresaCodigo, pactoCodigoCliente, dataRegistro])",
                "@@unique([pactoUnitAlias, pactoEmpresaCodigo, pactoContratoCodigo, produtosFingerprint])",
                "@@unique([pactoUnitAlias, pactoEmpresaCodigo, pactoRecibo])",
            ],
            forbidden: &[
                "@@unique([pactoEmpresaCodigo, pactoCodigoCliente, dataRegistro])",
                "@@unique([pactoEmpresaCodigo, pactoContratoCodigo, produtosFingerprint])",
                "@@unique([pactoEmpresaCodigo, pactoRecibo])",
            ],
        },
        Check {
            path: "jai-pay/prisma/migrations/20260520193000_pacto_unit_alias_event_keys/migration.sql",
            label: "Jai Pay unit-scoped Pacto event migration",
            needles: &[
                "ADD COLUMN \"pactoUnitAlias\"",
                "DROP INDEX IF EXISTS \"MemberPaymentEvent_pactoEmpresaCodigo_pactoRecibo_key\"",
                "ON \"MemberPaymentEvent\"(\"pactoUnitAlias\", \"pactoEmpresaCodigo\", \"pactoRecibo\")",
            ],
            forbidden: &[],
        },
        Check {
            path: "example-api/example/integrations/jaipay/pacto_webhook_tasks.py",
            label: "GCP Celery task",
            needles: &["example.jaipay_pacto_webhook_drain", "drain_pacto_webhooks"],
            forbidden: &[],
        },
        Check {
            path: "example-api/example/celery_app/celeryconfig.py",
            label: "GCP worker schedule",
            needles: &[
                "drain-jaipay-pacto-webhooks",
                "example.jaipay_pacto_webhook_drain",
            ],
            forbidden: &[],
        },
        Check {
            path: "example-api/docker-compose.yml",
            label: "GCP runtime env propagation",
            needles: &[
                "worker-beat:",
                "celery -A example.celery_app.app beat",
                "PACTO_WEBHOOK_CHAVES: ${PACTO_WEBHOOK_CHAVES:-}",
                "PACTO_WEBHOOK_CHAVE_LEARN_MODE: ${PACTO_WEBHOOK_CHAVE_LEARN_MODE:-disabled}",
                "PACTO_WEBHOOK_CHAVE_LEARN_IP_ALLOWLIST: ${PACTO_WEBHOOK_CHAVE_LEARN_IP_ALLOWLIST:-}",
                "JAIPAY_DATABASE_URL: ${JAIPAY_DATABASE_URL:-postgresql://jaipay:${JAIPAY_DB_PASSWORD:-jaipay_dev}@postgres:5432/jai_pay}",
                "JAIPAY_PACTO_WEBHOOK_STREAM",
                "JAIPAY_PACTO_WEBHOOK_CONSUMER_GROUP",
            ],
            forbidden: &[],
        },
        Check {
            path: "deploy/secret-sets/customer_ops_unified.env.example",
            label: "customer_ops_unified secret template",
            needles: &["PACTO_WEBHOOK_CHAVES=", "JAIPAY_DATABASE_URL="],
            forbidden: &[],
        },
        Check {
            path: "deploy/secret-sets/collections_platform.env.example",
            label: "collections_platform secret template",
            needles: &["PACTO_WEBHOOK_CHAVES=", "JAIPAY_DATABASE_URL="],
            forbidden: &[],
        },
        Check {
            path: "jai-pay/src/app/api/payments/pacto-webhook/route.ts",
            label: "Vercel decommission guard",
            needles: &[
                "webhook_moved_to_gcp",
                "https://api.getjai.com/v2/jaipay/pacto/webhook",
                "status: 503",
                "\"x-jaipay-webhook-target\": \"gcp\"",
            ],
            forbidden: &["dispatchPactoWebhook", "prisma.", "@/lib/prisma"],
        },
        Check {
            path: "jai-pay/src/lib/pacto-webhook-auth.ts",
            label: "Vercel-safe auth helpers",
            needles: &["verifyChaveForAlias", "await import(\"./prisma\")"],
            forbidden: &["import { prisma } from \"./prisma\""],
        },
    ];

    for check in checks {
        let full_path = root.join(check.path);
        let Some(src) = read_text(&full_path, &mut warnings) else {
            continue;
        };

        let missing = check
            .needles
            .iter()
            .filter(|needle| !src.contains(**needle))
            .copied()
            .collect::<Vec<_>>();
        let forbidden_hits = check
            .forbidden
            .iter()
            .filter(|needle| src.contains(**needle))
            .copied()
            .collect::<Vec<_>>();

        if !missing.is_empty() {
            warnings.push(format!(
                "{} missing required GCP webhook contract markers in {}: {}",
                check.label,
                check.path,
                missing.join(", ")
            ));
        }
        if !forbidden_hits.is_empty() {
            warnings.push(format!(
                "{} contains forbidden Vercel-processing markers in {}: {}",
                check.label,
                check.path,
                forbidden_hits.join(", ")
            ));
        }

        if missing.is_empty()
            && forbidden_hits.is_empty()
            && let Some(line) = check
                .needles
                .first()
                .and_then(|needle| find_line(&src, needle))
        {
            evidence.push(EvidenceItem {
                kind: "file".to_string(),
                path: check.path.to_string(),
                line: Some(line),
                detail: check.label.to_string(),
            });
        }

        entities.push(json!({
            "path": check.path,
            "label": check.label,
            "missing": missing,
            "forbidden_hits": forbidden_hits,
        }));
    }

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_jaipay_pacto_gcp_webhook"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            "Pacto webhooks terminate in GCP with Redis Stream durability and Vercel is a decommission guard".to_string()
        } else {
            format!(
                "found {} Jai Pay Pacto GCP webhook contract warning(s)",
                warnings.len()
            )
        },
        confidence: if warnings.is_empty() { 0.95 } else { 0.68 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({"doctor": "jaipay-pacto-gcp-webhook"})),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::doctor_jaipay_pacto_gcp_webhook;

    fn temp_root() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("leio-jaipay-pacto-gcp-{nanos}"));
        fs::create_dir_all(&root).expect("create temp root");
        root
    }

    fn write(root: &std::path::Path, path: &str, body: &str) {
        let full = root.join(path);
        fs::create_dir_all(full.parent().expect("parent")).expect("create parent");
        fs::write(full, body).expect("write fixture");
    }

    #[test]
    fn warns_when_vercel_route_processes_pacto_webhooks() {
        let root = temp_root();
        write(
            &root,
            "jai-pay/src/app/api/payments/pacto-webhook/route.ts",
            "import { prisma } from '@/lib/prisma'; dispatchPactoWebhook();",
        );

        let envelope = doctor_jaipay_pacto_gcp_webhook(&root);

        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("Vercel"))
        );
        let _ = fs::remove_dir_all(root);
    }
}
