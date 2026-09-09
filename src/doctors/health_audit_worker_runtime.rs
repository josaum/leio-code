//! `health-audit-worker-runtime` doctor.
//!
//! The 2026-07-31 production incident left a solo Celery process CPU-bound
//! inside contract ingestion while RabbitMQ reported thirty ready messages
//! and zero consumers. XML glosa audits shared that queue, the worker had no
//! healthcheck, and the task had no whole-task deadline. This doctor keeps the
//! repaired worker topology in the pre-deploy gate.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use regex::Regex;
use serde_json::json;
use serde_yaml::{Mapping as YamlMapping, Value as YamlValue};

use super::Doctor;
use super::utils::query_id;
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

const COMPOSE_PATH: &str = "example-api/docker-compose.health-audit.yml";
const CELERY_CONFIG_PATH: &str = "example-api/example/celery_app/celeryconfig.py";
const TASKS_PATH: &str = "cartridges/health_audit/tasks.py";
const ROUTER_PATH: &str = "cartridges/health_audit/router.py";
const CONTRACT_AIRGAPPED_PATH: &str = "cartridges/health_audit/services/contract_airgapped.py";
const CONTRACT_SOURCES_PATH: &str = "cartridges/health_audit/services/contract_sources.py";
const CONTRACT_ROUTES_PATH: &str = "cartridges/health_audit/routes/contracts.py";
const XML_GLOSA_QUEUE_HELPER_PATH: &str = "cartridges/health_audit/services/xml_glosa_queue.py";
const HOTFIX_DOCKERFILE_PATH: &str = "deploy/hotfix/health-audit-ingest/Dockerfile.api";
const EXTRACTION_TEMPLATE_PATH: &str = "cartridges/health_audit/domain/extraction_template.py";
const TENANT_AUDIT_PATH: &str = "cartridges/health_audit/tenant_audit.py";
const PROFILE_PATH: &str = "deploy/profiles/health_audit.env";
const EXTRACTOR_OCR_PATH: &str = "example-extractor/src-tauri/src/ocr.rs";

const AUDIT_QUEUE_ENV: &str = "HEALTH_AUDIT_AUDIT_QUEUE";
const XML_GLOSA_QUEUE_ENV: &str = "HEALTH_AUDIT_XML_GLOSA_QUEUE";
const XML_GLOSA_LOCAL_FALLBACK_ENV: &str = "HEALTH_AUDIT_XML_GLOSA_LOCAL_FALLBACK_ENABLED";
const CONTRACT_QUEUE_ENV: &str = "HEALTH_AUDIT_CONTRACT_INGEST_QUEUE";
const PREWARM_ENV: &str = "HEALTH_AUDIT_EMBED_PREWARM_ENABLED";
const BROKER_HEARTBEAT_ENV: &str = "CELERY_BROKER_HEARTBEAT_SECONDS";

const GIB: u64 = 1024 * 1024 * 1024;
const AUDIT_WORKER_MIN_MEMORY_BYTES: u64 = 3 * GIB;
const CONTRACT_WORKER_MIN_MEMORY_BYTES: u64 = 3 * GIB / 2;
const WORKER_AGGREGATE_MAX_MEMORY_BYTES: u64 = 6 * GIB;

const PROFILE_DEFAULTS: &[(&str, &str)] = &[
    (AUDIT_QUEUE_ENV, "health-audit-audits"),
    (XML_GLOSA_QUEUE_ENV, "health-audit-audits"),
    (XML_GLOSA_LOCAL_FALLBACK_ENV, "false"),
    (CONTRACT_QUEUE_ENV, "health-audit-contracts"),
    ("HEALTH_AUDIT_AUDIT_SOFT_TIME_LIMIT_SECONDS", "600"),
    ("HEALTH_AUDIT_AUDIT_TIME_LIMIT_SECONDS", "660"),
    ("HEALTH_AUDIT_CONTRACT_SOFT_TIME_LIMIT_SECONDS", "1800"),
    ("HEALTH_AUDIT_CONTRACT_TIME_LIMIT_SECONDS", "1860"),
    (BROKER_HEARTBEAT_ENV, "30"),
    ("HEALTH_AUDIT_OCR_PROFILE", "accuracy"),
    ("HEALTH_AUDIT_OCR_IMAGE_MAX_LONG_SIDE", "2048"),
    (
        "HEALTH_AUDIT_CONTRACT_RULE_SOURCES",
        "llm-structured-output,deterministic-text-structurer",
    ),
];

const ROUTE_CONTRACTS: &[(&str, &[&str])] = &[
    (
        "cartridge.health_audit.ingest_contract",
        &[
            CONTRACT_QUEUE_ENV,
            "contract_queue",
            "CONTRACT_INGEST_QUEUE",
            "health-audit-contracts",
        ],
    ),
    (
        "cartridge.health_audit.ingest_tiss",
        &[
            AUDIT_QUEUE_ENV,
            "audit_queue",
            "AUDIT_QUEUE",
            "health-audit-audits",
        ],
    ),
    (
        "cartridge.health_audit.xml_glosa_audit",
        &[
            XML_GLOSA_QUEUE_ENV,
            "xml_glosa_queue",
            "XML_GLOSA_QUEUE",
            "health-audit-audits",
        ],
    ),
    (
        "cartridge.health_audit.preauth_tiss",
        &[
            AUDIT_QUEUE_ENV,
            "audit_queue",
            "AUDIT_QUEUE",
            "health-audit-audits",
        ],
    ),
];

const ANNOTATION_CONTRACTS: &[(&str, &str, &str)] = &[
    (
        "cartridge.health_audit.ingest_contract",
        "HEALTH_AUDIT_CONTRACT_SOFT_TIME_LIMIT_SECONDS",
        "HEALTH_AUDIT_CONTRACT_TIME_LIMIT_SECONDS",
    ),
    (
        "cartridge.health_audit.ingest_tiss",
        "HEALTH_AUDIT_AUDIT_SOFT_TIME_LIMIT_SECONDS",
        "HEALTH_AUDIT_AUDIT_TIME_LIMIT_SECONDS",
    ),
    (
        "cartridge.health_audit.xml_glosa_audit",
        "HEALTH_AUDIT_AUDIT_SOFT_TIME_LIMIT_SECONDS",
        "HEALTH_AUDIT_AUDIT_TIME_LIMIT_SECONDS",
    ),
    (
        "cartridge.health_audit.preauth_tiss",
        "HEALTH_AUDIT_AUDIT_SOFT_TIME_LIMIT_SECONDS",
        "HEALTH_AUDIT_AUDIT_TIME_LIMIT_SECONDS",
    ),
];

struct Finding {
    rule_id: &'static str,
    path: &'static str,
    line: Option<usize>,
    message: String,
}

pub struct HealthAuditWorkerRuntimeDoctor;

impl Doctor for HealthAuditWorkerRuntimeDoctor {
    fn name(&self) -> &'static str {
        "health-audit-worker-runtime"
    }

    fn description(&self) -> &'static str {
        "Validates isolated prefork Health Audit workers, targeted healthchecks, \
         bounded memory, finite broker/task deadlines, task routing, shared-profile \
         queue fallbacks, disabled standalone XML local fallback, role-specific \
         embedding prewarm, linear contract-price/rule/provenance finalization, \
         durable merged-review status, bounded PostgreSQL tenant attestation, \
         point-image overlays, non-empty OCR batch evidence, profile-scoped \
         heartbeats, sidecar-scoped accuracy OCR, and production defaults."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_health_audit_worker_runtime(root)
    }
}

pub fn doctor_health_audit_worker_runtime(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut findings = Vec::new();

    let compose = read_required(root, COMPOSE_PATH, &mut findings);
    let celery_config = read_required(root, CELERY_CONFIG_PATH, &mut findings);
    let tasks = read_required(root, TASKS_PATH, &mut findings);
    let router = read_required(root, ROUTER_PATH, &mut findings);
    let contract_airgapped = read_required(root, CONTRACT_AIRGAPPED_PATH, &mut findings);
    let contract_sources = read_required(root, CONTRACT_SOURCES_PATH, &mut findings);
    let contract_routes = read_required(root, CONTRACT_ROUTES_PATH, &mut findings);
    let tenant_audit = read_required(root, TENANT_AUDIT_PATH, &mut findings);
    let hotfix_dockerfile = read_required(root, HOTFIX_DOCKERFILE_PATH, &mut findings);
    let xml_glosa_queue_helper =
        std::fs::read_to_string(root.join(XML_GLOSA_QUEUE_HELPER_PATH)).ok();
    let profile = read_required(root, PROFILE_PATH, &mut findings);
    let extractor_ocr = read_required(root, EXTRACTOR_OCR_PATH, &mut findings);
    let centralized_xml_queue_is_used = tasks
        .as_deref()
        .is_some_and(source_uses_centralized_xml_queue_resolver)
        || router
            .as_deref()
            .is_some_and(source_uses_centralized_xml_queue_resolver);
    let centralized_xml_queue_is_safe = xml_glosa_queue_helper
        .as_deref()
        .is_some_and(xml_queue_helper_keeps_celery_fallback);
    if centralized_xml_queue_is_used && !centralized_xml_queue_is_safe {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_xml_queue_fallback",
            path: XML_GLOSA_QUEUE_HELPER_PATH,
            line: xml_glosa_queue_helper
                .as_deref()
                .and_then(|source| line_of(source, "resolve_xml_glosa_queue")),
            message: format!(
                "{XML_GLOSA_QUEUE_HELPER_PATH} centralized XML queue resolver must \
                 fall back through the audit queue to `celery` for shared profiles"
            ),
        });
    }

    if let Some(source) = compose.as_deref() {
        check_compose(source, &mut findings);
    }
    if let Some(source) = celery_config.as_deref() {
        check_celery_config(source, &mut findings);
        check_shared_profile_queue_fallbacks(
            source,
            CELERY_CONFIG_PATH,
            &[
                (AUDIT_QUEUE_ENV, 1),
                (XML_GLOSA_QUEUE_ENV, 1),
                (CONTRACT_QUEUE_ENV, 1),
            ],
            centralized_xml_queue_is_safe,
            &mut findings,
        );
    }
    if let Some(source) = tasks.as_deref() {
        if !source.contains(PREWARM_ENV) {
            findings.push(Finding {
                rule_id: "health_audit_worker_runtime_prewarm_unused",
                path: TASKS_PATH,
                line: None,
                message: format!(
                    "{TASKS_PATH} worker-process prewarm hook does not consume \
                     {PREWARM_ENV}; role-specific Compose settings are inert"
                ),
            });
        }
        check_shared_profile_queue_fallbacks(
            source,
            TASKS_PATH,
            &[
                (AUDIT_QUEUE_ENV, 2),
                (XML_GLOSA_QUEUE_ENV, 1),
                (CONTRACT_QUEUE_ENV, 1),
            ],
            centralized_xml_queue_is_safe,
            &mut findings,
        );
    }
    if let Some(source) = router.as_deref() {
        check_shared_profile_queue_fallbacks(
            source,
            ROUTER_PATH,
            &[(XML_GLOSA_QUEUE_ENV, 3)],
            centralized_xml_queue_is_safe,
            &mut findings,
        );
        check_contract_finalize_provenance(source, &mut findings);
        check_contract_price_lookup_complexity(source, &mut findings);
        check_contract_structured_text_extraction(source, &mut findings);
        check_contract_batch_evidence(source, &mut findings);
        check_contract_rule_base_complexity(source, &mut findings);
    }
    if let Some(source) = contract_airgapped.as_deref() {
        check_contract_rule_deduplication_complexity(source, &mut findings);
    }
    if let Some(source) = contract_sources.as_deref() {
        check_contract_source_rebuild_complexity(source, &mut findings);
    }
    if let Some(source) = contract_routes.as_deref() {
        check_shared_profile_queue_fallbacks(
            source,
            CONTRACT_ROUTES_PATH,
            &[(CONTRACT_QUEUE_ENV, 1)],
            centralized_xml_queue_is_safe,
            &mut findings,
        );
    }
    if let Some(source) = tenant_audit.as_deref() {
        check_postgres_tenant_audit_payload_projection(source, &mut findings);
    }
    if let Some(source) = hotfix_dockerfile.as_deref() {
        check_hotfix_extraction_template_overlay(source, root, &mut findings);
        check_hotfix_tenant_audit_overlay(source, root, &mut findings);
    }
    if let Some(source) = profile.as_deref() {
        check_profile(source, &mut findings);
    }
    if let Some(source) = extractor_ocr.as_deref() {
        check_extractor_ocr_contract(source, &mut findings);
    }

    let warnings = findings
        .iter()
        .map(|finding| finding.message.clone())
        .collect::<Vec<_>>();
    let evidence = findings
        .iter()
        .map(|finding| EvidenceItem {
            kind: finding.rule_id.to_string(),
            path: finding.path.to_string(),
            line: finding.line,
            detail: finding.message.clone(),
        })
        .collect::<Vec<_>>();
    let entities = findings
        .iter()
        .map(|finding| {
            json!({
                "rule_id": finding.rule_id,
                "severity": "error",
                "path": finding.path,
                "line": finding.line,
                "message": finding.message,
            })
        })
        .collect::<Vec<_>>();

    let summary = if findings.is_empty() {
        "health-audit-worker-runtime: isolated workers, memory, healthchecks, \
         routing/fallbacks, deadlines, on-demand PDF provenance, linear contract \
         price/rule/source finalization, durable merged-review status, batched TUSS \
         validation, bounded PostgreSQL tenant attestation, point-image overlays, \
         OCR batch evidence/source safety, profile-scoped heartbeat, and defaults \
         are coherent"
            .to_string()
    } else {
        format!(
            "health-audit-worker-runtime: {} worker-runtime drift(s)",
            findings.len()
        )
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_health_audit_worker_runtime"),
        kind: "doctor".to_string(),
        summary,
        confidence: if findings.is_empty() { 0.98 } else { 0.68 },
        entities: entities.clone(),
        evidence,
        warnings,
        meta: Some(json!({
            "diagnostics": entities,
            "compose": COMPOSE_PATH,
            "celery_config": CELERY_CONFIG_PATH,
            "tasks": TASKS_PATH,
            "router": ROUTER_PATH,
            "contract_airgapped": CONTRACT_AIRGAPPED_PATH,
            "contract_sources": CONTRACT_SOURCES_PATH,
            "contract_routes": CONTRACT_ROUTES_PATH,
            "tenant_audit": TENANT_AUDIT_PATH,
            "hotfix_dockerfile": HOTFIX_DOCKERFILE_PATH,
            "extraction_template": EXTRACTION_TEMPLATE_PATH,
            "xml_glosa_queue_helper": XML_GLOSA_QUEUE_HELPER_PATH,
            "profile": PROFILE_PATH,
            "extractor_ocr": EXTRACTOR_OCR_PATH,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn check_contract_finalize_provenance(source: &str, findings: &mut Vec<Finding>) {
    let Some(finalize_body) = python_function_body(source, "_finalize_contract_ingest") else {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_finalize_missing",
            path: ROUTER_PATH,
            line: line_of(source, "_finalize_contract_ingest"),
            message: format!(
                "{ROUTER_PATH} must retain `_finalize_contract_ingest` so the \
                 production ingest critical path remains auditable"
            ),
        });
        return;
    };
    if finalize_body.contains("_enrich_price_limit_sources_with_pdf_bboxes(") {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_sync_pdf_provenance",
            path: ROUTER_PATH,
            line: line_of(source, "_enrich_price_limit_sources_with_pdf_bboxes"),
            message: format!(
                "{ROUTER_PATH} `_finalize_contract_ingest` must not scan whole PDFs \
                 for bbox provenance; keep bbox lookup on-demand so large contracts \
                 can reach a terminal ingest status before the Celery hard deadline"
            ),
        });
    }

    let durable_assignment =
        finalize_body.find("persisted_contract_record = _persist_contract_record");
    let durable_rebind = finalize_body.find("contract_record = persisted_contract_record");
    let ttl_build = finalize_body.find("_build_contract_ttl(contract_record)");
    let persist_returns_record = python_function_body(source, "_persist_contract_record")
        .is_some_and(|body| body.contains("return copy.deepcopy("));
    let durable_record_drives_status = matches!(
        (durable_assignment, durable_rebind, ttl_build),
        (Some(persist), Some(rebind), Some(ttl)) if persist < rebind && rebind < ttl
    );
    if !persist_returns_record || !durable_record_drives_status {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_premerge_finalize_status",
            path: ROUTER_PATH,
            line: line_of(source, "_persist_contract_record("),
            message: format!(
                "{ROUTER_PATH} `_persist_contract_record` must return the durable \
                 merged relationship and `_finalize_contract_ingest` must use it \
                 for TTL, counts, and status; a fresh attachment must not hide \
                 an existing unresolved review candidate"
            ),
        });
    }
}

fn check_hotfix_extraction_template_overlay(
    source: &str,
    root: &Path,
    findings: &mut Vec<Finding>,
) {
    let destination = format!("/home/appuser/{EXTRACTION_TEMPLATE_PATH}");
    let overlay_is_real = root.join(EXTRACTION_TEMPLATE_PATH).is_file()
        && source.contains(&format!("COPY --chown=1000:27 {EXTRACTION_TEMPLATE_PATH}"))
        && source.contains(&destination);
    if !overlay_is_real {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_hotfix_template_overlay",
            path: HOTFIX_DOCKERFILE_PATH,
            line: line_of(source, "extraction_template.py"),
            message: format!(
                "{HOTFIX_DOCKERFILE_PATH} must copy the real \
                 `{EXTRACTION_TEMPLATE_PATH}` into `{destination}`; a point image \
                 with a non-domain extraction_template.py path will fail before \
                 py_compile and cannot carry the Rust payload schema"
            ),
        });
    }
}

fn check_hotfix_tenant_audit_overlay(source: &str, root: &Path, findings: &mut Vec<Finding>) {
    let destination = format!("/home/appuser/{TENANT_AUDIT_PATH}");
    let overlay_is_real = root.join(TENANT_AUDIT_PATH).is_file()
        && source.contains(&format!("COPY --chown=1000:27 {TENANT_AUDIT_PATH}"))
        && source.matches(&destination).count() >= 2;
    if !overlay_is_real {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_hotfix_tenant_audit_overlay",
            path: HOTFIX_DOCKERFILE_PATH,
            line: line_of(source, "tenant_audit.py"),
            message: format!(
                "{HOTFIX_DOCKERFILE_PATH} must copy and py_compile the real \
                 `{TENANT_AUDIT_PATH}` into `{destination}`; otherwise candidate \
                 tenancy attestation can run the OOM-prone base-image module"
            ),
        });
    }
}

fn check_postgres_tenant_audit_payload_projection(source: &str, findings: &mut Vec<Finding>) {
    let helper = python_function_body(source, "_collect_postgres_payload_counts");
    let summary = python_function_body(source, "_collect_postgres_summary");
    let bounded_projection = helper.is_some_and(|body| {
        let normalized = body.split_whitespace().collect::<Vec<_>>().join(" ");
        body.matches("cursor.execute(").count() == 1
            && body.matches("-> 'tenant_id'").count() == 1
            && body.contains("fetchmany(")
            && !body.contains("jsonb_typeof")
            && !body.contains("count(*) FILTER")
            && !body.contains("payload::text")
            && normalized.contains("SELECT tenant_id, {payload} -> 'tenant_id' FROM {spec.table}")
    });
    let summary_uses_projection =
        summary.is_some_and(|body| body.contains("_collect_postgres_payload_counts("));
    if !bounded_projection || !summary_uses_projection {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_pg_tenant_audit_jsonb_fanout",
            path: TENANT_AUDIT_PATH,
            line: line_of(source, "_collect_postgres_payload_counts"),
            message: format!(
                "{TENANT_AUDIT_PATH} must project JSONB `tenant_id` once per row \
                 without selecting/casting the full payload, then count bounded \
                 fetchmany batches; repeated or full JSONB materialization can \
                 detoast a large contract enough times to OOM PostgreSQL"
            ),
        });
    }
}

fn check_contract_price_lookup_complexity(source: &str, findings: &mut Vec<Finding>) {
    let Some(facts_body) = python_function_body(source, "_extract_contract_facts") else {
        return;
    };

    // This is a positive contract rather than a snapshot of one former
    // implementation. Keeping the index as a named helper makes the hot-path
    // complexity auditable even when variable names or the regex engine change.
    if !facts_body.contains("_index_priced_code_lines(lines)") {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_linear_price_index_missing",
            path: ROUTER_PATH,
            line: line_of(source, "def _extract_contract_facts"),
            message: format!(
                "{ROUTER_PATH} `_extract_contract_facts` must use \
                 `_index_priced_code_lines(lines)` for linear native-text contract \
                 price lookup"
            ),
        });
    }

    let batch_helper = python_function_body(source, "_resolve_tuss_codes_batch");
    let batch_helper_uses_batch =
        batch_helper.is_some_and(|body| body.contains("validate_codes_batch"));
    if !facts_body.contains("_resolve_tuss_codes_batch(") || !batch_helper_uses_batch {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_batch_tuss_validation_missing",
            path: ROUTER_PATH,
            line: line_of(source, "def _extract_contract_facts"),
            message: format!(
                "{ROUTER_PATH} `_extract_contract_facts` must resolve native-text \
                 price codes through `_resolve_tuss_codes_batch` and the repository \
                 `validate_codes_batch` seam; per-code validation can issue four \
                 DuckDB queries for every code in a large contract"
            ),
        });
    }

    let batch_failure_is_fail_soft = batch_helper.is_some_and(|body| {
        body.contains("preserving normalized codes")
            && body.contains("return _raw_code_resolutions()")
    });
    if batch_helper_uses_batch && !batch_failure_is_fail_soft {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_batch_tuss_failure_fanout",
            path: ROUTER_PATH,
            line: line_of(source, "def _resolve_tuss_codes_batch"),
            message: format!(
                "{ROUTER_PATH} `_resolve_tuss_codes_batch` must preserve normalized \
                 codes when batch validation fails; falling back to per-code \
                 validation can reopen tens of thousands of DuckDB queries"
            ),
        });
    }

    if facts_body.contains("_extract_semantic_price_candidates")
        && facts_body.contains("not valid_codes")
    {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_mixed_semantic_price_omission",
            path: ROUTER_PATH,
            line: line_of(source, "_extract_semantic_price_candidates"),
            message: format!(
                "{ROUTER_PATH} must not gate semantic price inference with \
                 `not valid_codes`; mixed contracts can contain explicit code+price \
                 rows and separate uncoded price terms that still need an auditable \
                 TUSS resolution"
            ),
        });
    }

    if facts_body.contains("_extract_semantic_price_candidates") {
        let semantic_fingerprint_is_cached = facts_body.contains("semantic_candidate_fingerprints")
            && source.contains("def _semantic_price_candidate_fingerprint");
        let semantic_budget_is_explicit = source
            .contains("HEALTH_AUDIT_SEMANTIC_PRICE_INFERENCE_MAX_UNIQUE_CANDIDATES")
            && facts_body.contains("HEALTH_AUDIT_SEMANTIC_PRICE_INFERENCE_MAX_UNIQUE_CANDIDATES")
            && facts_body.contains("raise ValueError");
        if !semantic_fingerprint_is_cached || !semantic_budget_is_explicit {
            findings.push(Finding {
                rule_id: "health_audit_worker_runtime_semantic_price_fanout",
                path: ROUTER_PATH,
                line: line_of(source, "def _extract_contract_facts"),
                message: format!(
                    "{ROUTER_PATH} mixed native-text semantic price inference must \
                     deduplicate term+price candidates with \
                     `_semantic_price_candidate_fingerprint`, cache the fingerprint \
                     set, and enforce the explicit \
                     `HEALTH_AUDIT_SEMANTIC_PRICE_INFERENCE_MAX_UNIQUE_CANDIDATES` \
                     query budget; do not reopen an unbounded TUSS/ontology fan-out \
                     or impose a page cap"
                ),
            });
        }
    }

    let executable_body = python_code_without_comments_and_docstrings(facts_body);
    if let Some(relative_line) = nested_price_line_rescan_line(&executable_body) {
        let function_line = line_of(source, "def _extract_contract_facts").unwrap_or(1);
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_quadratic_price_lookup",
            path: ROUTER_PATH,
            line: Some(function_line + relative_line),
            message: format!(
                "{ROUTER_PATH} contract price finalization contains a quadratic \
                 code-by-line rescan; keep the single-pass \
                 `_index_priced_code_lines(lines)` helper and avoid nested \
                 code-to-line scans so full native-text PDFs finish without page \
                 limits"
            ),
        });
    }
}

fn check_contract_structured_text_extraction(source: &str, findings: &mut Vec<Finding>) {
    let Some(job_body) = python_function_body(source, "process_contract_ingest_job") else {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_structured_text_extractor_missing",
            path: ROUTER_PATH,
            line: line_of(source, "def process_contract_ingest_job"),
            message: format!(
                "{ROUTER_PATH} must retain `process_contract_ingest_job` and run the \
                 canonical structured contract extractor for text-rich PDFs"
            ),
        });
        return;
    };

    let runs_canonical_extractor = job_body.contains("_run_contract_extractor(")
        && job_body.contains("_native_pdf_contract_extractor_env")
        && job_body.contains("_contract_extractor_payload_to_extraction");
    let native_text_is_only_a_fallback = job_body.contains("native-text-stream")
        && job_body.contains("structured_extraction is None");
    if !runs_canonical_extractor || !native_text_is_only_a_fallback {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_structured_text_extractor_bypass",
            path: ROUTER_PATH,
            line: line_of(source, "def process_contract_ingest_job"),
            message: format!(
                "{ROUTER_PATH} text-rich async PDFs must invoke `_run_contract_extractor` \
                 with `_native_pdf_contract_extractor_env` and convert the payload via \
                 `_contract_extractor_payload_to_extraction`; `native-text-stream` may \
                 only run when `structured_extraction is None`, never as the default \
                 bypass of canonical structured rules"
            ),
        });
    }
}

fn check_contract_batch_evidence(source: &str, findings: &mut Vec<Finding>) {
    let evidence_helper_section = source
        .find("def _contract_page_batch_has_pdf_evidence")
        .map(|helper_start| {
            let helper_tail = &source[helper_start..];
            let helper_end = helper_tail
                .find("\ndef _run_contract_page_batches")
                .unwrap_or(helper_tail.len());
            &helper_tail[..helper_end]
        });
    if !matches!(
        evidence_helper_section,
        Some(section)
            if section.contains("field_values.values()")
                && section.contains("isinstance(entity, dict)")
                && section.contains("_table_html_to_text")
    ) {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_empty_batch_placeholders",
            path: ROUTER_PATH,
            line: line_of(source, "def _contract_page_batch_has_pdf_evidence"),
            message: format!(
                "{ROUTER_PATH} page-batch evidence must inspect meaningful field \
                 values, entity text, and rendered table text; empty placeholder \
                 containers must not suppress retry or whole-document Flight OCR"
            ),
        });
    }

    let Some(batch_start) = source.find("def _run_contract_page_batches") else {
        return;
    };
    // `_run_contract_page_batches` contains a nested `_run_batch` helper. The
    // generic Python body slicer intentionally stops at the next `def`, so use
    // the next top-level function as this check's boundary and keep the actual
    // result-consumption guard in scope.
    let batch_tail = &source[batch_start..];
    let batch_end = batch_tail
        .find("\ndef _extract_contract_document_batched")
        .unwrap_or(batch_tail.len());
    let batch_section = &batch_tail[..batch_end];
    if !batch_section.contains("_contract_page_batch_has_pdf_evidence(payload)") {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_empty_batch_accepted",
            path: ROUTER_PATH,
            line: line_of(source, "def _run_contract_page_batches"),
            message: format!(
                "{ROUTER_PATH} must reject available-but-empty page batches with \
                 `_contract_page_batch_has_pdf_evidence(payload)` so scanned PDFs \
                 reach the whole-document Flight OCR fallback instead of failing \
                 later with an empty merged extraction"
            ),
        });
    }

    let Some(document_start) = source.find("def _extract_contract_document_batched") else {
        return;
    };
    let document_tail = &source[document_start..];
    let document_end = document_tail
        .find("\ndef _contract_requires_non_degraded_extraction")
        .unwrap_or(document_tail.len());
    let document_section = &document_tail[..document_end];
    if !document_section.contains("if not batch_results or batch_failures:") {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_partial_batch_merge",
            path: ROUTER_PATH,
            line: line_of(source, "def _extract_contract_document_batched"),
            message: format!(
                "{ROUTER_PATH} must route any unresolved page-batch failure to \
                 whole-document Flight OCR or a terminal error; never merge and \
                 persist a partial regulated contract"
            ),
        });
    }

    let final_evidence_is_structural = source
        .find("def _contract_extraction_has_pdf_evidence")
        .map(|evidence_start| {
            let evidence_tail = &source[evidence_start..];
            let evidence_end = evidence_tail
                .find("\ndef _validate_contract_extraction_result")
                .unwrap_or(evidence_tail.len());
            let evidence_section = &evidence_tail[..evidence_end];
            evidence_section.contains("structured_evidence[\"text\"] = \"\"")
                && evidence_section
                    .contains("_contract_page_batch_has_pdf_evidence(structured_evidence)")
        })
        .unwrap_or(false);
    let validation_has_empty_structured_bypass = source
        .find("def _validate_contract_extraction_result")
        .map(|validation_start| {
            let validation_tail = &source[validation_start..];
            let validation_end = validation_tail
                .find("\ndef _build_contract_record")
                .unwrap_or(validation_tail.len());
            validation_tail[..validation_end]
                .contains("_coerce_structured_contract_extraction(field_values) is not None")
        })
        .unwrap_or(false);
    if !final_evidence_is_structural || validation_has_empty_structured_bypass {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_empty_structured_pdf_evidence",
            path: ROUTER_PATH,
            line: line_of(source, "def _contract_extraction_has_pdf_evidence"),
            message: format!(
                "{ROUTER_PATH} final PDF validation must inspect meaningful \
                 structured leaves and must not accept an empty \
                 `contract_extraction` container as evidence"
            ),
        });
    }
}

fn check_contract_rule_base_complexity(source: &str, findings: &mut Vec<Finding>) {
    let Some(compile_body) = python_function_body(source, "_compile_contract_rule_base") else {
        return;
    };
    let caches_formal_structure =
        compile_body.contains("_contract_has_formal_pdf_structure(contract)");
    let reuses_formal_structure = compile_body
        .contains("contract_has_formal_pdf_structure=contract_has_formal_pdf_structure");
    let scopes_bundle_index_to_referenced_sources =
        compile_body.contains("referenced_bundle_source_contract_ids");
    let parses_legacy_counts_safely = compile_body.contains("_parse_positive_int(");
    if !caches_formal_structure
        || !reuses_formal_structure
        || !scopes_bundle_index_to_referenced_sources
        || !parses_legacy_counts_safely
    {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_quadratic_rule_base_pdf_scan",
            path: ROUTER_PATH,
            line: line_of(source, "def _compile_contract_rule_base"),
            message: format!(
                "{ROUTER_PATH} `_compile_contract_rule_base` must compute formal \
                 PDF structure once, scope its bundle index to referenced \
                 sources, tolerate legacy counts, and reuse the result for \
                 every price-limit rule; rescanning all structured rules per \
                 code is quadratic"
            ),
        });
    }
}

fn check_contract_rule_deduplication_complexity(source: &str, findings: &mut Vec<Finding>) {
    let extractors = [
        "_extract_airgapped_pricing_rules",
        "_extract_airgapped_operational_rules",
        "_extract_airgapped_auth_rules",
    ];
    let every_extractor_is_indexed = extractors.iter().all(|name| {
        python_function_body(source, name).is_some_and(|body| {
            body.contains("fingerprints: set[str] = set()")
                && body.contains("fingerprints=fingerprints")
        })
    });
    if !source.contains("def _contract_rule_fingerprint") || !every_extractor_is_indexed {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_quadratic_rule_deduplication",
            path: CONTRACT_AIRGAPPED_PATH,
            line: line_of(source, "def _extract_airgapped_pricing_rules"),
            message: format!(
                "{CONTRACT_AIRGAPPED_PATH} must share a fingerprint set while \
                 extracting pricing, operational, and authorization rules; \
                 serializing every prior rule for each new clause is quadratic \
                 on dense contracts"
            ),
        });
    }
}

fn check_contract_source_rebuild_complexity(source: &str, findings: &mut Vec<Finding>) {
    let synchronize_is_indexed =
        python_function_body(source, "_synchronize_current_price_provenance").is_some_and(|body| {
            body.contains("source_by_code_and_id")
                && body.contains("sources_by_code_and_filename")
                && !body.contains("source_price_sources = copy.deepcopy(")
        });
    if !synchronize_is_indexed {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_quadratic_source_provenance_sync",
            path: CONTRACT_SOURCES_PATH,
            line: line_of(source, "def _synchronize_current_price_provenance"),
            message: format!(
                "{CONTRACT_SOURCES_PATH} late price provenance synchronization \
                 must build code/source ownership indexes once and update the \
                 copied source ledger in place; scanning every source or copying \
                 the full provenance map for each code is quadratic"
            ),
        });
    }

    let Some(rebuild_body) = python_function_body(source, "rebuild_contract_from_sources") else {
        return;
    };
    let copy_position = rebuild_body.find("source_price_sources = copy.deepcopy(");
    let code_loop_position = rebuild_body.find("for code, raw_price in");
    if !matches!(
        (copy_position, code_loop_position),
        (Some(copy), Some(code_loop)) if copy < code_loop
    ) {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_quadratic_source_provenance_copy",
            path: CONTRACT_SOURCES_PATH,
            line: line_of(source, "def rebuild_contract_from_sources"),
            message: format!(
                "{CONTRACT_SOURCES_PATH} must copy each source price-provenance \
                 map once before iterating its codes; copying the full map per \
                 code is quadratic"
            ),
        });
    }
}

/// Removes the source forms that must not influence a lexical doctor. This is
/// intentionally small: it preserves line count/indentation for the loop-shape
/// check below while excluding ordinary comments and standalone triple-quoted
/// docstrings.
fn python_code_without_comments_and_docstrings(source: &str) -> String {
    let mut result = String::with_capacity(source.len());
    let mut triple_quote: Option<&str> = None;

    for line in source.lines() {
        let trimmed = line.trim_start();
        if let Some(delimiter) = triple_quote {
            if trimmed.contains(delimiter) {
                triple_quote = None;
            }
            result.push('\n');
            continue;
        }

        if let Some(delimiter) = ["\"\"\"", "'''"]
            .into_iter()
            .find(|delimiter| trimmed.starts_with(*delimiter))
        {
            if !trimmed[delimiter.len()..].contains(delimiter) {
                triple_quote = Some(delimiter);
            }
            result.push('\n');
            continue;
        }

        // Inline comments are not executable. This deliberately leaves quoted
        // strings alone; they cannot match a statement-shaped loop at line start.
        result.push_str(line.split('#').next().unwrap_or_default());
        result.push('\n');
    }

    result
}

/// Returns the zero-based line containing a nested code-to-line rescan. The
/// names are captured from Python syntax, so `code`, `raw_code`, and renamed
/// line variables all remain covered.
fn nested_price_line_rescan_line(source: &str) -> Option<usize> {
    let code_loop = Regex::new(
        r"^(?P<indent>\s*)for\s+(?P<code>[A-Za-z_][A-Za-z0-9_]*)\s+in\s+.*\.(?:findall|finditer)\s*\([^)]*\bworking_text\b",
    )
    .expect("valid code discovery loop regex");
    let line_loop =
        Regex::new(r"^(?P<indent>\s*)for\s+(?P<line>[A-Za-z_][A-Za-z0-9_]*)\s+in\s+lines\s*:")
            .expect("valid line scan loop regex");
    let lines = source.lines().collect::<Vec<_>>();

    for (outer_index, outer) in lines.iter().enumerate() {
        let Some(code_capture) = code_loop.captures(outer) else {
            continue;
        };
        let outer_indent = code_capture.name("indent")?.as_str().len();
        let code_name = code_capture.name("code")?.as_str();

        for (inner_offset, candidate) in lines[outer_index + 1..].iter().enumerate() {
            let candidate_indent = candidate.len() - candidate.trim_start().len();
            if !candidate.trim().is_empty() && candidate_indent <= outer_indent {
                break;
            }
            let Some(line_capture) = line_loop.captures(candidate) else {
                continue;
            };
            if line_capture.name("indent")?.as_str().len() <= outer_indent {
                continue;
            }
            let line_name = line_capture.name("line")?.as_str();
            let membership = Regex::new(&format!(
                r"\b{}\s+in\s+{}\b",
                regex::escape(code_name),
                regex::escape(line_name),
            ))
            .expect("captured Python identifiers form a valid regex");

            for membership_candidate in lines[outer_index + inner_offset + 2..].iter() {
                let membership_indent =
                    membership_candidate.len() - membership_candidate.trim_start().len();
                if !membership_candidate.trim().is_empty()
                    && membership_indent <= line_capture.name("indent")?.as_str().len()
                {
                    break;
                }
                if membership.is_match(membership_candidate) {
                    return Some(outer_index);
                }
            }
        }
    }
    None
}

fn python_function_body<'a>(source: &'a str, function_name: &str) -> Option<&'a str> {
    let marker = format!("def {function_name}(");
    let start = source.find(&marker)?;
    let tail = &source[start..];
    let end = ["\ndef ", "\nclass ", "\n@"]
        .iter()
        .filter_map(|boundary| tail[marker.len()..].find(boundary))
        .map(|offset| marker.len() + offset)
        .min()
        .unwrap_or(tail.len());
    Some(&tail[..end])
}

fn read_required(root: &Path, rel: &'static str, findings: &mut Vec<Finding>) -> Option<String> {
    match std::fs::read_to_string(root.join(rel)) {
        Ok(source) => Some(source),
        Err(error) => {
            findings.push(Finding {
                rule_id: "health_audit_worker_runtime_missing_file",
                path: rel,
                line: None,
                message: format!("{rel} is required by the Health Audit worker runtime: {error}"),
            });
            None
        }
    }
}

fn check_compose(source: &str, findings: &mut Vec<Finding>) {
    let document = match serde_yaml::from_str::<YamlValue>(source) {
        Ok(document) => document,
        Err(error) => {
            findings.push(Finding {
                rule_id: "health_audit_worker_runtime_compose_parse",
                path: COMPOSE_PATH,
                line: None,
                message: format!("{COMPOSE_PATH} is not valid YAML: {error}"),
            });
            return;
        }
    };
    let Some(services) = document
        .as_mapping()
        .and_then(|mapping| yaml_mapping(mapping, "services"))
    else {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_services_missing",
            path: COMPOSE_PATH,
            line: None,
            message: format!("{COMPOSE_PATH} is missing its services mapping"),
        });
        return;
    };

    for service_name in ["api", "worker", "contract-worker"] {
        let fallback = yaml_mapping(services, service_name)
            .and_then(|service| environment_value(service, XML_GLOSA_LOCAL_FALLBACK_ENV));
        if !fallback.as_deref().is_some_and(|value| {
            compose_environment_has_default(value, XML_GLOSA_LOCAL_FALLBACK_ENV, "false")
        }) {
            findings.push(Finding {
                rule_id: "health_audit_worker_runtime_xml_local_fallback",
                path: COMPOSE_PATH,
                line: line_of(source, XML_GLOSA_LOCAL_FALLBACK_ENV),
                message: format!(
                    "{COMPOSE_PATH} `{service_name}` must receive \
                     {XML_GLOSA_LOCAL_FALLBACK_ENV} with the standalone production \
                     default false"
                ),
            });
        }
    }

    let Some(ocr_sidecar) = yaml_mapping(services, "ocr-sidecar") else {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_ocr_sidecar_missing",
            path: COMPOSE_PATH,
            line: None,
            message: format!(
                "{COMPOSE_PATH} must define the terminal `ocr-sidecar` with the Health Audit accuracy/2048 OCR contract"
            ),
        });
        return;
    };
    for (name, expected) in [
        ("EXAMPLE_OCR_PROFILE", "accuracy"),
        ("EXAMPLE_OCR_IMAGE_MAX_LONG_SIDE", "2048"),
    ] {
        let profile_name = match name {
            "EXAMPLE_OCR_PROFILE" => "HEALTH_AUDIT_OCR_PROFILE",
            "EXAMPLE_OCR_IMAGE_MAX_LONG_SIDE" => "HEALTH_AUDIT_OCR_IMAGE_MAX_LONG_SIDE",
            _ => unreachable!("fixed OCR environment contract"),
        };
        let configured = environment_value(ocr_sidecar, name);
        if !configured
            .as_deref()
            .is_some_and(|value| compose_environment_has_default(value, profile_name, expected))
        {
            findings.push(Finding {
                rule_id: "health_audit_worker_runtime_ocr_sidecar_accuracy",
                path: COMPOSE_PATH,
                line: line_of(source, name),
                message: format!(
                    "{COMPOSE_PATH} `ocr-sidecar` must set {name} from {profile_name} with the accuracy/2048 default `{expected}`"
                ),
            });
        }
    }
    for service_name in ["api", "worker", "contract-worker"] {
        let Some(service) = yaml_mapping(services, service_name) else {
            continue;
        };
        let uses_accuracy_profile = environment_value(service, "EXAMPLE_OCR_PROFILE")
            .as_deref()
            .is_some_and(|value| value.contains("accuracy"));
        let uses_full_page_cap = environment_value(service, "EXAMPLE_OCR_IMAGE_MAX_LONG_SIDE")
            .as_deref()
            .is_some_and(|value| value.contains("2048"));
        if uses_accuracy_profile || uses_full_page_cap {
            findings.push(Finding {
                rule_id: "health_audit_worker_runtime_ocr_sidecar_scope",
                path: COMPOSE_PATH,
                line: line_of(source, service_name),
                message: format!(
                    "{COMPOSE_PATH} must scope accuracy/2048 OCR to `ocr-sidecar`; `{service_name}` is a latency-oriented caller"
                ),
            });
        }
    }

    let workers = [
        (
            "worker",
            AUDIT_QUEUE_ENV,
            "health-audit-audits",
            "false",
            AUDIT_WORKER_MIN_MEMORY_BYTES,
            "3 GiB for governed audit and XML parser headroom",
        ),
        (
            "contract-worker",
            CONTRACT_QUEUE_ENV,
            "health-audit-contracts",
            "false",
            CONTRACT_WORKER_MIN_MEMORY_BYTES,
            "1.5 GiB for large-contract parser headroom",
        ),
    ];
    let mut worker_memory = BTreeMap::new();

    for (service_name, queue_env, queue_default, prewarm_expected, min_memory, min_memory_label) in
        workers
    {
        let Some(service) = yaml_mapping(services, service_name) else {
            findings.push(Finding {
                rule_id: "health_audit_worker_runtime_service_missing",
                path: COMPOSE_PATH,
                line: None,
                message: format!("{COMPOSE_PATH} must define a separate `{service_name}` service"),
            });
            continue;
        };

        match service_memory_limit_bytes(service) {
            Some(memory_bytes) => {
                worker_memory.insert(service_name, memory_bytes);
                if memory_bytes < min_memory {
                    findings.push(Finding {
                        rule_id: "health_audit_worker_runtime_memory_floor",
                        path: COMPOSE_PATH,
                        line: line_of(source, service_name),
                        message: format!(
                            "{COMPOSE_PATH} `{service_name}` mem_limit must be at least \
                             {min_memory_label}"
                        ),
                    });
                }
            }
            None => findings.push(Finding {
                rule_id: "health_audit_worker_runtime_memory_limit",
                path: COMPOSE_PATH,
                line: line_of(source, service_name),
                message: format!(
                    "{COMPOSE_PATH} `{service_name}` must declare a parseable mem_limit \
                     of at least {min_memory_label}"
                ),
            }),
        }

        let command = yaml_mapping_value(service, "command")
            .map(yaml_value_text)
            .unwrap_or_default();
        let normalized = command.split_whitespace().collect::<Vec<_>>().join(" ");
        if !uses_prefork(&normalized) {
            findings.push(Finding {
                rule_id: "health_audit_worker_runtime_pool",
                path: COMPOSE_PATH,
                line: line_of(source, service_name),
                message: format!(
                    "{COMPOSE_PATH} `{service_name}` must use the prefork pool; \
                     solo cannot maintain heartbeats while a task is active"
                ),
            });
        }
        if !has_concurrency_one(&normalized) {
            findings.push(Finding {
                rule_id: "health_audit_worker_runtime_concurrency",
                path: COMPOSE_PATH,
                line: line_of(source, service_name),
                message: format!("{COMPOSE_PATH} `{service_name}` must run with --concurrency=1"),
            });
        }
        if !has_queue_flag(&normalized)
            || !normalized.contains(queue_env)
            || !normalized.contains(queue_default)
        {
            findings.push(Finding {
                rule_id: "health_audit_worker_runtime_queue_consumer",
                path: COMPOSE_PATH,
                line: line_of(source, service_name),
                message: format!(
                    "{COMPOSE_PATH} `{service_name}` must consume its dedicated \
                     queue with -Q and {queue_env} (default `{queue_default}`)"
                ),
            });
        }

        let heartbeat = environment_value(service, BROKER_HEARTBEAT_ENV);
        if !heartbeat
            .as_deref()
            .is_some_and(|value| compose_environment_has_default(value, BROKER_HEARTBEAT_ENV, "30"))
        {
            findings.push(Finding {
                rule_id: "health_audit_worker_runtime_compose_heartbeat",
                path: COMPOSE_PATH,
                line: line_of(source, BROKER_HEARTBEAT_ENV),
                message: format!(
                    "{COMPOSE_PATH} `{service_name}` must receive \
                     {BROKER_HEARTBEAT_ENV} with the finite Health Audit default 30"
                ),
            });
        }

        let prewarm = environment_value(service, PREWARM_ENV);
        if prewarm.as_deref() != Some(prewarm_expected) {
            findings.push(Finding {
                rule_id: "health_audit_worker_runtime_prewarm_role",
                path: COMPOSE_PATH,
                line: line_of(source, service_name),
                message: format!(
                    "{COMPOSE_PATH} `{service_name}` must set {PREWARM_ENV}={prewarm_expected}; \
                     authoritative Health Audit workers never prewarm an embedding model"
                ),
            });
        }

        check_healthcheck(source, service_name, service, findings);
    }

    if worker_memory.len() == 2 {
        let aggregate = worker_memory
            .values()
            .copied()
            .fold(0u64, u64::saturating_add);
        if aggregate > WORKER_AGGREGATE_MAX_MEMORY_BYTES {
            findings.push(Finding {
                rule_id: "health_audit_worker_runtime_memory_aggregate",
                path: COMPOSE_PATH,
                line: line_of(source, "worker:"),
                message: format!(
                    "{COMPOSE_PATH} worker mem_limit aggregate must stay at or below \
                     6 GiB (the bounded production worker budget)"
                ),
            });
        }
    }
}

fn check_healthcheck(
    source: &str,
    service_name: &str,
    service: &YamlMapping,
    findings: &mut Vec<Finding>,
) {
    let Some(healthcheck) = yaml_mapping(service, "healthcheck") else {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_healthcheck",
            path: COMPOSE_PATH,
            line: line_of(source, service_name),
            message: format!(
                "{COMPOSE_PATH} `{service_name}` must define a targeted Celery healthcheck"
            ),
        });
        return;
    };

    if yaml_mapping_value(healthcheck, "disable").is_some_and(yaml_truthy) {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_healthcheck_disabled",
            path: COMPOSE_PATH,
            line: line_of(source, service_name),
            message: format!("{COMPOSE_PATH} `{service_name}` healthcheck must not be disabled"),
        });
        return;
    }

    let probe = yaml_mapping_value(healthcheck, "test")
        .map(yaml_value_text)
        .unwrap_or_default();
    let probe_lower = probe.to_ascii_lowercase();
    let has_destination =
        probe.contains("--destination") || probe.split_whitespace().any(|token| token == "-d");
    let has_runtime_hostname = probe.contains("HOSTNAME") || probe_lower.contains("$(hostname)");
    let targeted = probe.contains("inspect ping")
        && has_destination
        && has_runtime_hostname
        && probe.contains("pong");
    if !targeted {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_healthcheck_target",
            path: COMPOSE_PATH,
            line: line_of(source, service_name),
            message: format!(
                "{COMPOSE_PATH} `{service_name}` healthcheck must target its own \
                 node with inspect ping, a destination bound to its runtime \
                 hostname, and a required pong response"
            ),
        });
    }
}

fn check_celery_config(source: &str, findings: &mut Vec<Finding>) {
    let heartbeat_is_profile_scoped = python_assignment(source, "broker_heartbeat")
        .as_deref()
        .is_some_and(|assignment| generic_heartbeat_is_profile_scoped(source, assignment));
    if !heartbeat_is_profile_scoped {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_heartbeat_scope",
            path: CELERY_CONFIG_PATH,
            line: line_of(source, "broker_heartbeat"),
            message: format!(
                "{CELERY_CONFIG_PATH} broker_heartbeat must consume \
                 {BROKER_HEARTBEAT_ENV} while keeping the generic default None; \
                 only the Health Audit profile may enable a finite heartbeat"
            ),
        });
    }

    let checkrate_is_finite = python_assignment(source, "broker_heartbeat_checkrate")
        .as_deref()
        .is_some_and(finite_positive_value);
    if !checkrate_is_finite {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_heartbeat",
            path: CELERY_CONFIG_PATH,
            line: line_of(source, "broker_heartbeat_checkrate"),
            message: format!(
                "{CELERY_CONFIG_PATH} must set finite positive broker_heartbeat_checkrate"
            ),
        });
    }
    if python_assignment(source, "task_track_started").as_deref() != Some("True") {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_track_started",
            path: CELERY_CONFIG_PATH,
            line: line_of(source, "task_track_started"),
            message: format!("{CELERY_CONFIG_PATH} must set task_track_started = True"),
        });
    }

    for queue_env in [AUDIT_QUEUE_ENV, XML_GLOSA_QUEUE_ENV, CONTRACT_QUEUE_ENV] {
        if !source.contains(queue_env) {
            findings.push(Finding {
                rule_id: "health_audit_worker_runtime_route_env",
                path: CELERY_CONFIG_PATH,
                line: None,
                message: format!("{CELERY_CONFIG_PATH} task_routes must resolve {queue_env}"),
            });
        }
    }

    match extract_braced_assignment(source, "task_routes") {
        None => findings.push(Finding {
            rule_id: "health_audit_worker_runtime_routes_missing",
            path: CELERY_CONFIG_PATH,
            line: line_of(source, "task_routes"),
            message: format!(
                "{CELERY_CONFIG_PATH} must define task_routes for isolated Health Audit queues"
            ),
        }),
        Some(routes) => {
            for (task_name, queue_hints) in ROUTE_CONTRACTS {
                let valid = task_entry(routes, task_name).is_some_and(|entry| {
                    entry.contains("queue")
                        && route_entry_resolves_queue(source, entry, queue_hints)
                });
                if !valid {
                    findings.push(Finding {
                        rule_id: "health_audit_worker_runtime_task_route",
                        path: CELERY_CONFIG_PATH,
                        line: line_of(source, task_name),
                        message: format!(
                            "{CELERY_CONFIG_PATH} task_routes must route `{task_name}` \
                             to its dedicated Health Audit queue"
                        ),
                    });
                }
            }
        }
    }

    for (_, soft_env, hard_env) in ANNOTATION_CONTRACTS {
        for env_name in [*soft_env, *hard_env] {
            if !source.contains(env_name) {
                findings.push(Finding {
                    rule_id: "health_audit_worker_runtime_timeout_env",
                    path: CELERY_CONFIG_PATH,
                    line: None,
                    message: format!(
                        "{CELERY_CONFIG_PATH} task_annotations must resolve {env_name}"
                    ),
                });
            }
        }
    }

    match extract_braced_assignment(source, "task_annotations") {
        None => findings.push(Finding {
            rule_id: "health_audit_worker_runtime_annotations_missing",
            path: CELERY_CONFIG_PATH,
            line: line_of(source, "task_annotations"),
            message: format!(
                "{CELERY_CONFIG_PATH} must define task_annotations with whole-task \
                 soft and hard time limits"
            ),
        }),
        Some(annotations) => {
            for (task_name, _, _) in ANNOTATION_CONTRACTS {
                let valid = task_entry(annotations, task_name).is_some_and(|entry| {
                    entry.contains("soft_time_limit")
                        && (entry.contains("\"time_limit\"") || entry.contains("'time_limit'"))
                });
                if !valid {
                    findings.push(Finding {
                        rule_id: "health_audit_worker_runtime_task_timeout",
                        path: CELERY_CONFIG_PATH,
                        line: line_of(source, task_name),
                        message: format!(
                            "{CELERY_CONFIG_PATH} task_annotations must give `{task_name}` \
                             whole-task soft_time_limit and time_limit values"
                        ),
                    });
                }
            }
        }
    }
}

fn check_shared_profile_queue_fallbacks(
    source: &str,
    path: &'static str,
    contracts: &[(&str, usize)],
    centralized_xml_queue_is_safe: bool,
    findings: &mut Vec<Finding>,
) {
    let calls = getenv_calls(source);
    for (queue_env, minimum_occurrences) in contracts {
        if *queue_env == XML_GLOSA_QUEUE_ENV
            && centralized_xml_queue_is_safe
            && source_uses_centralized_xml_queue_resolver(source)
        {
            continue;
        }
        let fallbacks = calls
            .iter()
            .filter(|(env_name, _)| env_name == queue_env)
            .map(|(_, fallback)| fallback)
            .collect::<Vec<_>>();
        if fallbacks.len() < *minimum_occurrences {
            findings.push(Finding {
                rule_id: "health_audit_worker_runtime_queue_fallback_missing",
                path,
                line: line_of(source, queue_env),
                message: format!(
                    "{path} must keep at least {minimum_occurrences} source fallback(s) \
                     for {queue_env}; shared profiles require the `celery` queue"
                ),
            });
            continue;
        }
        for fallback in fallbacks {
            if !queue_fallback_resolves_to_celery(source, fallback, 0) {
                findings.push(Finding {
                    rule_id: "health_audit_worker_runtime_queue_fallback",
                    path,
                    line: line_of(source, queue_env),
                    message: format!(
                        "{path} {queue_env} source fallback must resolve to `celery` \
                         for shared jcube/full profiles; got {fallback}"
                    ),
                });
            }
        }
    }
}

fn getenv_calls(source: &str) -> Vec<(String, String)> {
    let pattern = Regex::new(
        r#"(?s)os\.getenv\(\s*["'](?P<env>[A-Z][A-Z0-9_]*)["']\s*,\s*(?P<fallback>"[^"]*"|'[^']*'|[A-Za-z_][A-Za-z0-9_]*)\s*\)"#,
    )
    .expect("valid Python os.getenv fallback regex");
    pattern
        .captures_iter(source)
        .filter_map(|captures| {
            Some((
                captures.name("env")?.as_str().to_string(),
                captures.name("fallback")?.as_str().to_string(),
            ))
        })
        .collect()
}

fn queue_fallback_resolves_to_celery(source: &str, fallback: &str, depth: usize) -> bool {
    if depth > 8 {
        return false;
    }
    let normalized = fallback.trim();
    if normalized
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            normalized
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .is_some_and(|value| value == "celery")
    {
        return true;
    }
    if normalized.is_empty()
        || !normalized
            .chars()
            .all(|character| character == '_' || character.is_ascii_alphanumeric())
        || normalized
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_digit())
    {
        return false;
    }
    let Some(assignment) = python_assignment(source, normalized) else {
        return false;
    };
    if queue_fallback_resolves_to_celery(source, &assignment, depth + 1) {
        return true;
    }
    getenv_calls(&assignment)
        .first()
        .is_some_and(|(_, nested)| queue_fallback_resolves_to_celery(source, nested, depth + 1))
}

fn source_uses_centralized_xml_queue_resolver(source: &str) -> bool {
    python_call_expression(source, "resolve_xml_glosa_queue").is_some_and(|call| {
        call.contains(XML_GLOSA_QUEUE_ENV)
            && call.contains(AUDIT_QUEUE_ENV)
            && call.contains("os.getenv")
    })
}

fn xml_queue_helper_keeps_celery_fallback(source: &str) -> bool {
    let Some(function_start) = source.find("def resolve_xml_glosa_queue") else {
        return false;
    };
    let function = &source[function_start..];
    let function = function
        .find("\ndef ")
        .map(|end| &function[..end])
        .unwrap_or(function);
    let celery_literals =
        function.matches("\"celery\"").count() + function.matches("'celery'").count();
    function.contains("xml_queue")
        && function.contains("audit_queue")
        && function.contains("return")
        && celery_literals >= 2
}

fn python_call_expression<'a>(source: &'a str, function_name: &str) -> Option<&'a str> {
    let marker = format!("{function_name}(");
    let start = source.find(&marker)?;
    let expression = &source[start..];
    let mut depth = 0i32;
    let mut opened = false;
    for (offset, character) in expression.char_indices() {
        if character == '(' {
            depth += 1;
            opened = true;
        } else if character == ')' {
            depth -= 1;
            if opened && depth == 0 {
                return Some(&expression[..=offset]);
            }
        }
    }
    None
}

fn check_profile(source: &str, findings: &mut Vec<Finding>) {
    let values = parse_dotenv(source);
    for (name, expected) in PROFILE_DEFAULTS {
        if values.get(*name).map(String::as_str) != Some(*expected) {
            findings.push(Finding {
                rule_id: "health_audit_worker_runtime_profile_default",
                path: PROFILE_PATH,
                line: line_of(source, name),
                message: format!(
                    "{PROFILE_PATH} must declare {name}={expected} as the production default"
                ),
            });
        }
    }

    let audit = values.get(AUDIT_QUEUE_ENV);
    let xml_glosa = values.get(XML_GLOSA_QUEUE_ENV);
    let contract = values.get(CONTRACT_QUEUE_ENV);
    if contract.is_some() && (contract == audit || contract == xml_glosa) {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_queues_shared",
            path: PROFILE_PATH,
            line: line_of(source, CONTRACT_QUEUE_ENV),
            message: format!(
                "{PROFILE_PATH} contract ingestion and XML/audit queues must be distinct"
            ),
        });
    }
}

fn check_extractor_ocr_contract(source: &str, findings: &mut Vec<Finding>) {
    let selector = rust_function_body(source, "select_pdf_fast_page_text");
    if !selector.is_some_and(|body| {
        !body.contains("page_vector_text_as_string")
            && body.contains("PdfFastPageTextSource::OarCandidate")
            && body.contains("String::new()")
    }) {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_ocr_reference_facts",
            path: EXTRACTOR_OCR_PATH,
            line: line_of(source, "select_pdf_fast_page_text"),
            message: format!(
                "{EXTRACTOR_OCR_PATH} must keep glyph-reference diagnostic text out of contract facts; degraded CMap pages must return an empty OAR candidate"
            ),
        });
    }

    let batch_signature = rust_function_signature(source, "run_oar_ocr_chunk");
    let image_selector = rust_function_body(source, "page_image_for_ocr");
    if !batch_signature.is_some_and(|signature| signature.contains("pages: &[PdfPage]"))
        || image_selector.is_some_and(|body| body.contains("doc.pages()"))
    {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_ocr_resolved_pages",
            path: EXTRACTOR_OCR_PATH,
            line: line_of(source, "run_oar_ocr_chunk"),
            message: format!(
                "{EXTRACTOR_OCR_PATH} OCR batching must pass resolved `&[PdfPage]` values and page_image_for_ocr must never call `doc.pages()`"
            ),
        });
    }

    let tables = rust_function_body(source, "should_use_native_pdf_tables");
    if !tables.is_some_and(|body| body.contains("source == PdfFastPageTextSource::Semantic")) {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_ocr_native_tables",
            path: EXTRACTOR_OCR_PATH,
            line: line_of(source, "should_use_native_pdf_tables"),
            message: format!(
                "{EXTRACTOR_OCR_PATH} must gate native tables to Semantic pages so untrusted CMap text cannot bypass OAR"
            ),
        });
    }

    let direct_image = rust_function_body(source, "should_use_direct_embedded_page_image");
    if !direct_image.is_some_and(|body| {
        body.contains("overlap_width >= page_width * 0.90")
            && body.contains("overlap_height >= page_height * 0.90")
    }) {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_ocr_full_page_geometry",
            path: EXTRACTOR_OCR_PATH,
            line: line_of(source, "should_use_direct_embedded_page_image"),
            message: format!(
                "{EXTRACTOR_OCR_PATH} direct OCR images require full-page geometric overlap, not a logo or tiled fragment"
            ),
        });
    }
    if !image_selector.is_some_and(|body| {
        body.contains("rasterizing composite PDF page for OCR")
            && body.contains("page_rasterized_png")
    }) {
        findings.push(Finding {
            rule_id: "health_audit_worker_runtime_ocr_composite_raster",
            path: EXTRACTOR_OCR_PATH,
            line: line_of(source, "page_image_for_ocr"),
            message: format!(
                "{EXTRACTOR_OCR_PATH} must route composite pages through a full-page raster fallback before OCR"
            ),
        });
    }
}

fn rust_function_signature<'a>(source: &'a str, function_name: &str) -> Option<&'a str> {
    let marker = format!("fn {function_name}(");
    let start = source.find(&marker)?;
    let signature = &source[start..];
    let end = signature.find('{')?;
    Some(&signature[..end])
}

fn rust_function_body<'a>(source: &'a str, function_name: &str) -> Option<&'a str> {
    let signature = rust_function_signature(source, function_name)?;
    let start = signature.as_ptr() as usize - source.as_ptr() as usize + signature.len();
    let mut depth = 0usize;
    for (offset, byte) in source.as_bytes()[start..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(&source[start..=start + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

fn uses_prefork(command: &str) -> bool {
    command.contains("-P prefork")
        || command.contains("--pool prefork")
        || command.contains("--pool=prefork")
}

fn has_concurrency_one(command: &str) -> bool {
    command.contains("--concurrency=1")
        || command.contains("--concurrency 1")
        || command.contains("-c 1")
}

fn has_queue_flag(command: &str) -> bool {
    command.contains("-Q ") || command.contains("--queues ") || command.contains("--queues=")
}

fn python_assignment(source: &str, name: &str) -> Option<String> {
    let mut lines = source.lines();
    while let Some(line) = lines.next() {
        let code = line.split('#').next().unwrap_or(line).trim();
        let Some(rest) = code.strip_prefix(name) else {
            continue;
        };
        let Some(value) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let mut expression = value.trim().to_string();
        let mut parenthesis_depth = delimiter_depth(&expression, '(', ')');
        while parenthesis_depth > 0 {
            let Some(next_line) = lines.next() else {
                break;
            };
            let next_code = next_line.split('#').next().unwrap_or(next_line).trim();
            if !next_code.is_empty() {
                expression.push(' ');
                expression.push_str(next_code);
                parenthesis_depth += delimiter_depth(next_code, '(', ')');
            }
        }
        return Some(expression);
    }
    None
}

fn finite_positive_value(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    !matches!(normalized.as_str(), "" | "none" | "false" | "0")
        && normalized.chars().any(|ch| matches!(ch, '1'..='9'))
}

fn generic_heartbeat_is_profile_scoped(source: &str, assignment: &str) -> bool {
    let falls_back_to_disabled = getenv_calls(source)
        .iter()
        .any(|(env, fallback)| env == BROKER_HEARTBEAT_ENV && python_disabled_value(fallback));
    falls_back_to_disabled
        && python_expression_contains_token(assignment, "None")
        && python_expression_resolves_env(source, assignment, BROKER_HEARTBEAT_ENV, 0)
}

fn python_disabled_value(value: &str) -> bool {
    let normalized = value.trim();
    normalized.eq_ignore_ascii_case("none")
        || normalized
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .or_else(|| {
                normalized
                    .strip_prefix('\'')
                    .and_then(|value| value.strip_suffix('\''))
            })
            .is_some_and(str::is_empty)
}

fn python_expression_contains_token(expression: &str, expected: &str) -> bool {
    expression
        .split(|character: char| !(character == '_' || character.is_ascii_alphanumeric()))
        .any(|token| token == expected)
}

fn python_expression_resolves_env(
    source: &str,
    expression: &str,
    env_name: &str,
    depth: usize,
) -> bool {
    if depth > 8 {
        return false;
    }
    if expression.contains(env_name) {
        return true;
    }
    expression
        .split(|character: char| !(character == '_' || character.is_ascii_alphanumeric()))
        .filter(|token| !token.is_empty())
        .any(|token| {
            python_assignment(source, token).is_some_and(|nested| {
                nested != expression
                    && python_expression_resolves_env(source, &nested, env_name, depth + 1)
            })
        })
}

fn delimiter_depth(value: &str, open: char, close: char) -> i32 {
    value.chars().fold(0, |depth, ch| {
        if ch == open {
            depth + 1
        } else if ch == close {
            depth - 1
        } else {
            depth
        }
    })
}

fn route_entry_resolves_queue(source: &str, entry: &str, queue_hints: &[&str]) -> bool {
    if queue_hints.iter().any(|hint| entry.contains(hint)) {
        return true;
    }

    entry
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .filter(|token| !token.is_empty())
        .any(|token| {
            python_assignment(source, token)
                .is_some_and(|assignment| queue_hints.iter().any(|hint| assignment.contains(hint)))
        })
}

fn extract_braced_assignment<'a>(source: &'a str, name: &str) -> Option<&'a str> {
    let mut offset = 0usize;
    let mut assignment_offset = None;
    for line in source.split_inclusive('\n') {
        let code = line.split('#').next().unwrap_or(line).trim_start();
        if let Some(rest) = code.strip_prefix(name)
            && rest.trim_start().starts_with('=')
        {
            assignment_offset = Some(offset);
            break;
        }
        offset += line.len();
    }
    let assignment_offset = assignment_offset?;
    let brace_offset = source[assignment_offset..].find('{')? + assignment_offset;
    let mut depth = 0usize;
    for (relative, byte) in source.as_bytes()[brace_offset..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(&source[brace_offset..=brace_offset + relative]);
                }
            }
            _ => {}
        }
    }
    None
}

fn task_entry<'a>(mapping: &'a str, task_name: &str) -> Option<&'a str> {
    let double_quoted = format!("\"{task_name}\"");
    let single_quoted = format!("'{task_name}'");
    let (start, needle_len) = mapping
        .find(&double_quoted)
        .map(|offset| (offset, double_quoted.len()))
        .or_else(|| {
            mapping
                .find(&single_quoted)
                .map(|offset| (offset, single_quoted.len()))
        })?;
    let after_name = start + needle_len;
    let rest = &mapping[after_name..];
    let next_double = rest.find("\"cartridge.health_audit.");
    let next_single = rest.find("'cartridge.health_audit.");
    let end = match (next_double, next_single) {
        (Some(a), Some(b)) => after_name + a.min(b),
        (Some(a), None) => after_name + a,
        (None, Some(b)) => after_name + b,
        (None, None) => mapping.len(),
    };
    Some(&mapping[start..end])
}

fn parse_dotenv(source: &str) -> BTreeMap<String, String> {
    source
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let line = line.strip_prefix("export ").unwrap_or(line);
            let (name, value) = line.split_once('=')?;
            let value = value
                .split(" #")
                .next()
                .unwrap_or(value)
                .trim()
                .trim_matches(['"', '\'']);
            Some((name.trim().to_string(), value.to_string()))
        })
        .collect()
}

fn yaml_mapping<'a>(mapping: &'a YamlMapping, key: &str) -> Option<&'a YamlMapping> {
    yaml_mapping_value(mapping, key).and_then(YamlValue::as_mapping)
}

fn yaml_mapping_value<'a>(mapping: &'a YamlMapping, key: &str) -> Option<&'a YamlValue> {
    mapping.get(YamlValue::String(key.to_string()))
}

fn yaml_value_text(value: &YamlValue) -> String {
    match value {
        YamlValue::String(value) => value.clone(),
        YamlValue::Bool(value) => value.to_string(),
        YamlValue::Number(value) => value.to_string(),
        YamlValue::Sequence(values) => values
            .iter()
            .map(yaml_value_text)
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

fn yaml_truthy(value: &YamlValue) -> bool {
    matches!(
        yaml_value_text(value).trim().to_ascii_lowercase().as_str(),
        "true" | "yes" | "1" | "on"
    )
}

fn environment_value(service: &YamlMapping, name: &str) -> Option<String> {
    let environment = yaml_mapping_value(service, "environment")?;
    if let Some(mapping) = environment.as_mapping() {
        return environment_mapping_value(mapping, name);
    }
    environment.as_sequence().and_then(|values| {
        values.iter().find_map(|value| {
            yaml_value_text(value)
                .strip_prefix(&format!("{name}="))
                .map(ToOwned::to_owned)
        })
    })
}

fn environment_mapping_value(mapping: &YamlMapping, name: &str) -> Option<String> {
    if let Some(value) = yaml_mapping_value(mapping, name) {
        return Some(yaml_value_text(value));
    }
    let merged = yaml_mapping_value(mapping, "<<")?;
    if let Some(merged_mapping) = merged.as_mapping() {
        return environment_mapping_value(merged_mapping, name);
    }
    merged.as_sequence().and_then(|values| {
        values.iter().find_map(|value| {
            value
                .as_mapping()
                .and_then(|mapping| environment_mapping_value(mapping, name))
        })
    })
}

fn compose_environment_has_default(value: &str, name: &str, expected: &str) -> bool {
    let normalized = value.trim().trim_matches(['"', '\'']);
    normalized == expected || normalized == format!("${{{name}:-{expected}}}")
}

fn service_memory_limit_bytes(service: &YamlMapping) -> Option<u64> {
    let raw = yaml_mapping_value(service, "mem_limit").map(yaml_value_text)?;
    parse_memory_limit_bytes(&raw)
}

fn parse_memory_limit_bytes(raw: &str) -> Option<u64> {
    let mut candidate = raw.trim().trim_matches(['"', '\'']);
    if let Some(inner) = candidate
        .strip_prefix("${")
        .and_then(|value| value.strip_suffix('}'))
    {
        candidate = inner.split_once(":-")?.1.trim();
    }

    let normalized = candidate.replace('_', "").to_ascii_lowercase();
    let number_end = normalized
        .find(|character: char| !(character.is_ascii_digit() || character == '.'))
        .unwrap_or(normalized.len());
    let amount = normalized[..number_end].parse::<f64>().ok()?;
    if !amount.is_finite() || amount <= 0.0 {
        return None;
    }
    let unit = normalized[number_end..].trim();
    let multiplier = match unit {
        "" | "b" => 1f64,
        "k" | "kb" | "ki" | "kib" => 1024f64,
        "m" | "mb" | "mi" | "mib" => (1024u64 * 1024) as f64,
        "g" | "gb" | "gi" | "gib" => GIB as f64,
        "t" | "tb" | "ti" | "tib" => (1024u64 * GIB) as f64,
        _ => return None,
    };
    let bytes = amount * multiplier;
    if !bytes.is_finite() || bytes > u64::MAX as f64 {
        return None;
    }
    Some(bytes.ceil() as u64)
}

fn line_of(source: &str, needle: &str) -> Option<usize> {
    source.find(needle).map(|offset| {
        source[..offset]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + 1
    })
}
