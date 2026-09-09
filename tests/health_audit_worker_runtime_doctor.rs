use std::fs;
use std::path::Path;
use std::process::Command;

use leio_code::doctors::health_audit_worker_runtime::doctor_health_audit_worker_runtime;
use leio_code::doctors::{ci_doctor_names, doctor_names_for_profile};
use tempfile::TempDir;

const GOOD_COMPOSE: &str = r#"
x-health-audit-env: &health-audit-env
  HEALTH_AUDIT_XML_GLOSA_LOCAL_FALLBACK_ENABLED: ${HEALTH_AUDIT_XML_GLOSA_LOCAL_FALLBACK_ENABLED:-false}
  CELERY_BROKER_HEARTBEAT_SECONDS: ${CELERY_BROKER_HEARTBEAT_SECONDS:-30}
  CELERY_BROKER_HEARTBEAT_CHECKRATE: ${CELERY_BROKER_HEARTBEAT_CHECKRATE:-2}

services:
  api:
    environment:
      <<: *health-audit-env
  worker:
    command: >-
      /bin/sh -lc 'exec celery -A example.celery_app.app worker -P prefork
      --concurrency=1
      -Q "${HEALTH_AUDIT_AUDIT_QUEUE:-health-audit-audits}"
      --hostname=health-audit-audits@%h'
    mem_limit: 4096m
    environment:
      <<: *health-audit-env
      HEALTH_AUDIT_EMBED_PREWARM_ENABLED: "false"
    healthcheck:
      test:
        - CMD-SHELL
        - >-
          celery -A example.celery_app.app inspect ping
          --destination "health-audit-audits@${HOSTNAME}" --timeout=5 |
          grep -q pong
  contract-worker:
    command: >-
      /bin/sh -lc 'exec celery -A example.celery_app.app worker -P prefork
      --concurrency=1
      -Q "${HEALTH_AUDIT_CONTRACT_INGEST_QUEUE:-health-audit-contracts}"
      --hostname=health-audit-contracts@%h'
    mem_limit: 2048m
    environment:
      <<: *health-audit-env
      HEALTH_AUDIT_EMBED_PREWARM_ENABLED: "false"
    healthcheck:
      test:
        - CMD-SHELL
        - >-
          celery -A example.celery_app.app inspect ping
          --destination "health-audit-contracts@${HOSTNAME}" --timeout=5 |
          grep -q pong
  ocr-sidecar:
    environment:
      EXAMPLE_OCR_PROFILE: ${HEALTH_AUDIT_OCR_PROFILE:-accuracy}
      EXAMPLE_OCR_IMAGE_MAX_LONG_SIDE: ${HEALTH_AUDIT_OCR_IMAGE_MAX_LONG_SIDE:-2048}
"#;

const GOOD_CELERY_CONFIG: &str = r#"
import os

def env_int(name, default):
    return int(os.getenv(name, str(default)))

_broker_heartbeat_raw = os.getenv("CELERY_BROKER_HEARTBEAT_SECONDS", "").strip()
broker_heartbeat = int(_broker_heartbeat_raw) if _broker_heartbeat_raw else None
broker_heartbeat_checkrate = 2
task_track_started = True

audit_queue = os.getenv("HEALTH_AUDIT_AUDIT_QUEUE", "celery")
xml_glosa_queue = os.getenv("HEALTH_AUDIT_XML_GLOSA_QUEUE", audit_queue)
contract_queue = os.getenv(
    "HEALTH_AUDIT_CONTRACT_INGEST_QUEUE", "celery"
)

task_routes = {
    "cartridge.health_audit.ingest_contract": {"queue": contract_queue},
    "cartridge.health_audit.ingest_tiss": {"queue": audit_queue},
    "cartridge.health_audit.xml_glosa_audit": {"queue": xml_glosa_queue},
    "cartridge.health_audit.preauth_tiss": {"queue": audit_queue},
}

task_annotations = {
    "cartridge.health_audit.ingest_contract": {
        "soft_time_limit": env_int(
            "HEALTH_AUDIT_CONTRACT_SOFT_TIME_LIMIT_SECONDS", 1800
        ),
        "time_limit": env_int(
            "HEALTH_AUDIT_CONTRACT_TIME_LIMIT_SECONDS", 1860
        ),
    },
    "cartridge.health_audit.ingest_tiss": {
        "soft_time_limit": env_int(
            "HEALTH_AUDIT_AUDIT_SOFT_TIME_LIMIT_SECONDS", 600
        ),
        "time_limit": env_int(
            "HEALTH_AUDIT_AUDIT_TIME_LIMIT_SECONDS", 660
        ),
    },
    "cartridge.health_audit.xml_glosa_audit": {
        "soft_time_limit": env_int(
            "HEALTH_AUDIT_AUDIT_SOFT_TIME_LIMIT_SECONDS", 600
        ),
        "time_limit": env_int(
            "HEALTH_AUDIT_AUDIT_TIME_LIMIT_SECONDS", 660
        ),
    },
    "cartridge.health_audit.preauth_tiss": {
        "soft_time_limit": env_int(
            "HEALTH_AUDIT_AUDIT_SOFT_TIME_LIMIT_SECONDS", 600
        ),
        "time_limit": env_int(
            "HEALTH_AUDIT_AUDIT_TIME_LIMIT_SECONDS", 660
        ),
    },
}
"#;

const GOOD_PROFILE: &str = r#"
HEALTH_AUDIT_AUDIT_QUEUE=health-audit-audits
HEALTH_AUDIT_XML_GLOSA_QUEUE=health-audit-audits
HEALTH_AUDIT_XML_GLOSA_LOCAL_FALLBACK_ENABLED=false
HEALTH_AUDIT_CONTRACT_INGEST_QUEUE=health-audit-contracts
HEALTH_AUDIT_AUDIT_SOFT_TIME_LIMIT_SECONDS=600
HEALTH_AUDIT_AUDIT_TIME_LIMIT_SECONDS=660
HEALTH_AUDIT_CONTRACT_SOFT_TIME_LIMIT_SECONDS=1800
HEALTH_AUDIT_CONTRACT_TIME_LIMIT_SECONDS=1860
CELERY_BROKER_HEARTBEAT_SECONDS=30
CELERY_BROKER_HEARTBEAT_CHECKRATE=2
HEALTH_AUDIT_OCR_PROFILE=accuracy
HEALTH_AUDIT_OCR_IMAGE_MAX_LONG_SIDE=2048
HEALTH_AUDIT_CONTRACT_RULE_SOURCES=llm-structured-output,deterministic-text-structurer
"#;

const GOOD_EXTRACTOR_OCR: &str = r#"
use pdf_fast_core::{PdfDocument as BytePdfDocument, PdfImagePlacement, PdfPage};

enum PdfFastPageTextSource { Semantic, OarCandidate }

fn select_pdf_fast_page_text(semantic_text: String) -> (PdfFastPageTextSource, String) {
    if !semantic_text.is_empty() {
        return (PdfFastPageTextSource::Semantic, semantic_text);
    }
    (PdfFastPageTextSource::OarCandidate, String::new())
}

fn should_use_native_pdf_tables(source: PdfFastPageTextSource) -> bool {
    source == PdfFastPageTextSource::Semantic
}

fn process_pdf_fast(doc: &BytePdfDocument) {
    let pages = doc.pages();
    let text_source = PdfFastPageTextSource::Semantic;
    let _tables = if should_use_native_pdf_tables(text_source) { vec![] } else { vec![] };
    let _ = run_oar_ocr_chunk(doc, &pages, &[0]);
}

fn run_oar_ocr_chunk(doc: &BytePdfDocument, pages: &[PdfPage], page_indices: &[usize]) {
    for page_index in page_indices {
        let page = pages.get(*page_index).unwrap();
        let _ = page_image_for_ocr(doc, page);
    }
}

fn page_image_for_ocr(doc: &BytePdfDocument, page: &PdfPage) {
    let image_stream_count = page.image_streams.len();
    let image_placements = doc.page_image_placements(page.page_index).unwrap_or_default();
    if image_stream_count > 1 || image_placements.len() > 1 {
        tracing::info!("rasterizing composite PDF page for OCR");
    }
    let _ = doc.page_rasterized_png(page.page_index, 150);
}

fn should_use_direct_embedded_page_image(
    page_width: f32,
    page_height: f32,
    overlap_width: f32,
    overlap_height: f32,
    _placements: &[PdfImagePlacement],
) -> bool {
    overlap_width >= page_width * 0.90 && overlap_height >= page_height * 0.90
}
"#;

const GOOD_HOTFIX_DOCKERFILE: &str = r#"
FROM scratch
COPY --chown=1000:27 cartridges/health_audit/domain/extraction_template.py \
    /home/appuser/cartridges/health_audit/domain/extraction_template.py
COPY --chown=1000:27 cartridges/health_audit/tenant_audit.py \
    /home/appuser/cartridges/health_audit/tenant_audit.py
RUN python -m py_compile \
    /home/appuser/cartridges/health_audit/domain/extraction_template.py \
    /home/appuser/cartridges/health_audit/tenant_audit.py
"#;

const GOOD_TENANT_AUDIT: &str = r#"
def _collect_postgres_payload_counts(cursor, spec, expected_tenant):
    payload = spec.payload_column
    cursor.execute(
        f"""
        SELECT tenant_id, {payload} -> 'tenant_id'
        FROM {spec.table}
        """
    )
    while True:
        rows = cursor.fetchmany(64)
        if not rows:
            break
    return (0, 0, 0, 0, 0, 0, 0)

def _collect_postgres_summary(cursor, spec, expected_tenant):
    payload_counts = _collect_postgres_payload_counts(cursor, spec, expected_tenant)
    return payload_counts
"#;

const GOOD_TASKS: &str = r#"
import os

from celery.signals import worker_process_init

CONTRACT_QUEUE = os.getenv("HEALTH_AUDIT_CONTRACT_INGEST_QUEUE", "celery")
AUDIT_QUEUE = os.getenv("HEALTH_AUDIT_AUDIT_QUEUE", "celery")
SECOND_AUDIT_QUEUE = os.getenv("HEALTH_AUDIT_AUDIT_QUEUE", "celery")
XML_GLOSA_QUEUE = os.getenv("HEALTH_AUDIT_XML_GLOSA_QUEUE", "celery")

def embedding_prewarm_enabled():
    return os.getenv("HEALTH_AUDIT_EMBED_PREWARM_ENABLED", "true").lower() == "true"

@worker_process_init.connect
def prewarm_embedding_engine(**kwargs):
    if not embedding_prewarm_enabled():
        return
"#;

const GOOD_ROUTER: &str = r#"
import os

XML_WORKER_QUEUE = os.getenv("HEALTH_AUDIT_XML_GLOSA_QUEUE", "celery")
XML_DISPATCH_QUEUE = os.getenv("HEALTH_AUDIT_XML_GLOSA_QUEUE", "celery")
XML_LOG_QUEUE = os.getenv("HEALTH_AUDIT_XML_GLOSA_QUEUE", "celery")

def _native_pdf_contract_extractor_env():
    return {"CONTRACT_EXTRACTOR_PIPELINE": "auto"}

def _run_contract_extractor(*args, **kwargs):
    return {"contract_extraction": {}}

def _contract_extractor_payload_to_extraction(payload):
    return payload.get("contract_extraction")

def process_contract_ingest_job():
    extractor_payload = _run_contract_extractor(
        extractor_env=_native_pdf_contract_extractor_env(),
    )
    structured_extraction = _contract_extractor_payload_to_extraction(extractor_payload)
    if structured_extraction is None:
        return "native-text-stream"
    return structured_extraction

def _persist_contract_record(contract_record, *, merge_with_existing=False):
    return copy.deepcopy(contract_record)

def _finalize_contract_ingest():
    contract_record = {"unresolved_price_candidates": []}
    persisted_contract_record = _persist_contract_record(
        contract_record,
        merge_with_existing=True,
    )
    if isinstance(persisted_contract_record, dict):
        contract_record = persisted_contract_record
    ttl_preview = _build_contract_ttl(contract_record)
    return ttl_preview

def _contract_page_batch_has_pdf_evidence(payload):
    field_values = payload.get("field_values") or {}
    if any(str(value).strip() for value in field_values.values()):
        return True
    for entity in payload.get("entities") or []:
        if isinstance(entity, dict) and entity.get("text"):
            return True
    return any(
        _table_html_to_text(table).strip()
        for table in payload.get("tables_html") or []
    )

def _run_contract_page_batches(payload):
    def _run_batch():
        return payload

    payload = _run_batch()
    if payload.get("available") and _contract_page_batch_has_pdf_evidence(payload):
        return payload
    return None

def _extract_contract_document_batched(batch_results, batch_failures):
    if not batch_results or batch_failures:
        return _extract_contract_document()
    return batch_results

def _contract_extraction_has_pdf_evidence(extraction):
    structured_evidence = dict(extraction)
    structured_evidence["text"] = ""
    return _contract_page_batch_has_pdf_evidence(structured_evidence)

def _validate_contract_extraction_result(extraction):
    if _contract_extraction_has_pdf_evidence(extraction):
        return extraction
    raise ValueError("empty")

def _build_contract_record():
    return None

def _contract_requires_non_degraded_extraction():
    return True

def _compile_contract_rule_base(contract):
    contract_has_formal_pdf_structure = _contract_has_formal_pdf_structure(contract)
    referenced_bundle_source_contract_ids = {
        source.get("source_contract_id")
        for source in contract.get("price_limit_sources", {}).values()
    }
    formal_pdf_bundle_source_contract_ids = {
        source.get("contract_id")
        for source in contract.get("bundle_sources", [])
        if source.get("contract_id") in referenced_bundle_source_contract_ids
        and _parse_positive_int(source.get("structured_pricing_rule_count"), 0)
    }
    for code in contract.get("price_limits", {}):
        _price_limit_execution_policy(
            contract,
            code,
            {},
            contract_has_formal_pdf_structure=contract_has_formal_pdf_structure,
            formal_pdf_bundle_source_contract_ids=formal_pdf_bundle_source_contract_ids,
        )
"#;

const GOOD_PRICE_LOOKUP: &str = r#"

HEALTH_AUDIT_SEMANTIC_PRICE_INFERENCE_MAX_UNIQUE_CANDIDATES = 256

def _resolve_tuss_codes_batch(raw_codes, repo):
    try:
        return repo.validate_codes_batch(raw_codes)
    except Exception:
        logger.exception("preserving normalized codes")
        return _raw_code_resolutions()

def _semantic_price_candidate_fingerprint(term, price):
    return f"{term}|{price}"

def _extract_contract_facts(working_text, lines):
    """A literal legacy `for raw_code in re.findall(...)` is documentation only."""
    # for raw_code in re.findall(...): for line in lines: if raw_code in line
    valid_codes = {}
    for code, price, source_line in _index_priced_code_lines(lines):
        valid_codes[code] = price
    _resolve_tuss_codes_batch(list(valid_codes), repo=None)
    semantic_candidate_fingerprints = set()
    for term, price in _extract_semantic_price_candidates(lines):
        fingerprint = _semantic_price_candidate_fingerprint(term, price)
        if fingerprint in semantic_candidate_fingerprints:
            continue
        semantic_candidate_fingerprints.add(fingerprint)
        if len(semantic_candidate_fingerprints) > HEALTH_AUDIT_SEMANTIC_PRICE_INFERENCE_MAX_UNIQUE_CANDIDATES:
            raise ValueError("semantic candidate budget exceeded")
    return None
"#;

const GOOD_CONTRACT_ROUTES: &str = r#"
import os

CONTRACT_DISPATCH_QUEUE = os.getenv(
    "HEALTH_AUDIT_CONTRACT_INGEST_QUEUE", "celery"
)
"#;

const GOOD_CONTRACT_AIRGAPPED: &str = r#"
def _contract_rule_fingerprint(rule):
    return str(sorted(rule.items()))

def _append_unique_contract_rule(target, rule, *, fingerprints=None):
    fingerprint = _contract_rule_fingerprint(rule)
    if fingerprint not in fingerprints:
        fingerprints.add(fingerprint)
        target.append(rule)

def _extract_airgapped_pricing_rules(lines, filename):
    rules = []
    fingerprints: set[str] = set()
    for line in lines:
        _append_unique_contract_rule(
            rules,
            {"description": line},
            fingerprints=fingerprints,
        )
    return rules

def _extract_airgapped_operational_rules(lines, filename):
    rules = []
    fingerprints: set[str] = set()
    for line in lines:
        _append_unique_contract_rule(
            rules,
            {"description": line},
            fingerprints=fingerprints,
        )
    return rules

def _extract_airgapped_auth_rules(lines, filename):
    rules = []
    fingerprints: set[str] = set()
    for line in lines:
        _append_unique_contract_rule(
            rules,
            {"description": line},
            fingerprints=fingerprints,
        )
    return rules
"#;

const GOOD_CONTRACT_SOURCES: &str = r#"
import copy

def _synchronize_current_price_provenance(contract, source_records):
    source_by_code_and_id = {}
    sources_by_code_and_filename = {}
    for source in source_records:
        for code in source.get("price_limits", {}):
            source_by_code_and_id[(code, source.get("source_id"))] = source
            sources_by_code_and_filename[(code, source.get("filename"))] = [source]
    for code, evidence in contract.get("price_limit_sources", {}).items():
        source = source_by_code_and_id.get((code, evidence.get("source_id")))
        if source is not None:
            source_price_sources = source["price_limit_sources"]
            source_price_sources[code] = evidence
    return source_records

def rebuild_contract_from_sources(contract, source_records):
    for source in source_records:
        source_price_sources = copy.deepcopy(source.get("price_limit_sources") or {})
        for code, raw_price in source.get("price_limits", {}).items():
            source_price_sources[code] = {"price": raw_price}
    return contract
"#;

const GOOD_XML_GLOSA_QUEUE_HELPER: &str = r#"
def resolve_xml_glosa_queue(
    xml_queue: str | None,
    audit_queue: str | None,
) -> str:
    return str(xml_queue or audit_queue or "celery").strip() or "celery"
"#;

const CENTRALIZED_XML_TASKS: &str = r#"
import os

from celery.signals import worker_process_init

CONTRACT_QUEUE = os.getenv("HEALTH_AUDIT_CONTRACT_INGEST_QUEUE", "celery")
AUDIT_QUEUE = os.getenv("HEALTH_AUDIT_AUDIT_QUEUE", "celery")
SECOND_AUDIT_QUEUE = os.getenv("HEALTH_AUDIT_AUDIT_QUEUE", "celery")

def _xml_glosa_queue_name() -> str:
    from .services.xml_glosa_queue import resolve_xml_glosa_queue

    return resolve_xml_glosa_queue(
        os.getenv("HEALTH_AUDIT_XML_GLOSA_QUEUE"),
        os.getenv("HEALTH_AUDIT_AUDIT_QUEUE"),
    )

def embedding_prewarm_enabled():
    return os.getenv("HEALTH_AUDIT_EMBED_PREWARM_ENABLED", "true").lower() == "true"

@worker_process_init.connect
def prewarm_embedding_engine(**kwargs):
    if not embedding_prewarm_enabled():
        return

XML_GLOSA_QUEUE = _xml_glosa_queue_name()
"#;

const CENTRALIZED_XML_ROUTER: &str = r#"
import os

def _xml_glosa_queue_name() -> str:
    from .services.xml_glosa_queue import resolve_xml_glosa_queue

    return resolve_xml_glosa_queue(
        os.getenv("HEALTH_AUDIT_XML_GLOSA_QUEUE"),
        os.getenv("HEALTH_AUDIT_AUDIT_QUEUE"),
    )

XML_WORKER_QUEUE = _xml_glosa_queue_name()
XML_DISPATCH_QUEUE = _xml_glosa_queue_name()
XML_LOG_QUEUE = _xml_glosa_queue_name()
"#;

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture parent");
    }
    fs::write(path, body).expect("write fixture");
}

fn write_runtime(root: &Path, compose: &str, celery_config: &str, profile: &str) {
    write(root, "example-api/docker-compose.health-audit.yml", compose);
    write(
        root,
        "example-api/example/celery_app/celeryconfig.py",
        celery_config,
    );
    write(root, "deploy/profiles/health_audit.env", profile);
    write(
        root,
        "deploy/hotfix/health-audit-ingest/Dockerfile.api",
        GOOD_HOTFIX_DOCKERFILE,
    );
    write(
        root,
        "cartridges/health_audit/domain/extraction_template.py",
        "class ContractExtractionTemplate:\n    pass\n",
    );
    write(
        root,
        "cartridges/health_audit/tenant_audit.py",
        GOOD_TENANT_AUDIT,
    );
    write(
        root,
        "example-extractor/src-tauri/src/ocr.rs",
        GOOD_EXTRACTOR_OCR,
    );
    write(root, "cartridges/health_audit/tasks.py", GOOD_TASKS);
    let router = format!("{GOOD_ROUTER}{GOOD_PRICE_LOOKUP}");
    write(root, "cartridges/health_audit/router.py", &router);
    write(
        root,
        "cartridges/health_audit/services/contract_airgapped.py",
        GOOD_CONTRACT_AIRGAPPED,
    );
    write(
        root,
        "cartridges/health_audit/services/contract_sources.py",
        GOOD_CONTRACT_SOURCES,
    );
    write(
        root,
        "cartridges/health_audit/routes/contracts.py",
        GOOD_CONTRACT_ROUTES,
    );
    write(
        root,
        "cartridges/health_audit/services/xml_glosa_queue.py",
        GOOD_XML_GLOSA_QUEUE_HELPER,
    );
}

fn warning_text(root: &Path) -> String {
    doctor_health_audit_worker_runtime(root).warnings.join("\n")
}

// Why: the worker runtime is a Example deployment contract. If this doctor
// drops out of the profile registry, `doctor all` and `audit --strict` stop
// guarding the production queue topology.
#[test]
fn health_audit_worker_runtime_is_registered_for_example() {
    let names = doctor_names_for_profile("example");

    assert!(
        names.contains(&"health-audit-worker-runtime"),
        "health-audit-worker-runtime must be registered for example, got: {names:?}"
    );
}

// Why: this outage class must run in the fast file-local CI doctor pack, not
// only when an operator remembers to invoke the full catalog.
#[test]
fn health_audit_worker_runtime_is_in_the_ci_pack() {
    assert!(
        ci_doctor_names().contains(&"health-audit-worker-runtime"),
        "health-audit-worker-runtime must gate the CI doctor pack"
    );
}

// Why: registry-only wiring is insufficient for operators. The production
// preflight invokes this doctor by name through the CLI, so Clap must accept
// the same stable name exposed by MCP and Apps SDK.
#[test]
fn health_audit_worker_runtime_is_exposed_by_the_cli() {
    let output = Command::new(env!("CARGO_BIN_EXE_leio-code"))
        .args(["doctor", "--help"])
        .output()
        .expect("run leio-code doctor --help");

    assert!(output.status.success(), "doctor --help must succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("health-audit-worker-runtime"),
        "CLI doctor values must expose health-audit-worker-runtime, got:\n{stdout}"
    );
}

// Why: the complete contract must remain quiet; otherwise the doctor would
// block a safe Health Audit release.
#[test]
fn healthy_worker_runtime_passes() {
    let tmp = TempDir::new().expect("tempdir");
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);

    let envelope = doctor_health_audit_worker_runtime(tmp.path());

    assert!(
        envelope.warnings.is_empty(),
        "healthy runtime must pass, got: {:?}",
        envelope.warnings
    );
}

#[test]
fn deterministic_contract_structurer_must_be_authoritative_in_production() {
    let tmp = TempDir::new().expect("tempdir");
    let unsafe_profile = GOOD_PROFILE.replace(
        "HEALTH_AUDIT_CONTRACT_RULE_SOURCES=llm-structured-output,deterministic-text-structurer",
        "HEALTH_AUDIT_CONTRACT_RULE_SOURCES=llm-structured-output",
    );
    write_runtime(
        tmp.path(),
        GOOD_COMPOSE,
        GOOD_CELERY_CONFIG,
        &unsafe_profile,
    );

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains(
            "HEALTH_AUDIT_CONTRACT_RULE_SOURCES=llm-structured-output,deterministic-text-structurer"
        ),
        "excluding the deterministic Rust structurer must warn, got: {warnings}"
    );
}

// Why: pdf-fast's glyph-reference preview is diagnostic-only. Promoting it into
// the selected page text can silently turn a broken CMap into contract facts.
#[test]
fn glyph_reference_diagnostic_text_must_not_be_promoted_to_facts() {
    let tmp = TempDir::new().expect("tempdir");
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    let unsafe_ocr = GOOD_EXTRACTOR_OCR.replace(
        "if !semantic_text.is_empty() {",
        "let diagnostic = page_vector_text_as_string();\n    if !semantic_text.is_empty() {",
    );
    write(
        tmp.path(),
        "example-extractor/src-tauri/src/ocr.rs",
        &unsafe_ocr.replace(
            "(PdfFastPageTextSource::OarCandidate, String::new())",
            "(PdfFastPageTextSource::OarCandidate, diagnostic)",
        ),
    );

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("glyph-reference") && warnings.contains("contract facts"),
        "reference diagnostic promotion must warn, got: {warnings}"
    );
}

// Why: OCR batches must reuse the resolved pages. Reopening the document in
// the image selector breaks page identity and multiplies parser work.
#[test]
fn ocr_batches_must_pass_resolved_pages_without_reresolving_them() {
    let tmp = TempDir::new().expect("tempdir");
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    let unsafe_ocr = GOOD_EXTRACTOR_OCR
        .replace("pages: &[PdfPage]", "_pages: &[PdfPage]")
        .replace(
            "let image_stream_count = page.image_streams.len();",
            "let _pages = doc.pages();\n    let image_stream_count = page.image_streams.len();",
        );
    write(
        tmp.path(),
        "example-extractor/src-tauri/src/ocr.rs",
        &unsafe_ocr,
    );

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("resolved `&[PdfPage]`") && warnings.contains("doc.pages()"),
        "OCR page reuse drift must warn, got: {warnings}"
    );
}

// Why: native table text has the same CMap provenance as semantic text. It
// must be absent from OAR-candidate pages rather than bypass that safeguard.
#[test]
fn native_tables_must_remain_semantic_only() {
    let tmp = TempDir::new().expect("tempdir");
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(
        tmp.path(),
        "example-extractor/src-tauri/src/ocr.rs",
        &GOOD_EXTRACTOR_OCR.replace("source == PdfFastPageTextSource::Semantic", "true"),
    );

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("native tables") && warnings.contains("Semantic"),
        "native-table source drift must warn, got: {warnings}"
    );
}

// Why: a logo or tiled fragments cannot replace a full-page scan. Direct
// bitmap OCR needs geometric coverage; composites must use the page raster.
#[test]
fn ocr_image_selection_requires_full_page_geometry_and_composite_raster() {
    let tmp = TempDir::new().expect("tempdir");
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    let unsafe_ocr = GOOD_EXTRACTOR_OCR
        .replace(
            "overlap_width >= page_width * 0.90 && overlap_height >= page_height * 0.90",
            "true",
        )
        .replace(
            "rasterizing composite PDF page for OCR",
            "direct embedded image",
        )
        .replace("let _ = doc.page_rasterized_png(page.page_index, 150);", "");
    write(
        tmp.path(),
        "example-extractor/src-tauri/src/ocr.rs",
        &unsafe_ocr,
    );

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("full-page geometric overlap")
            && warnings.contains("composite pages through a full-page raster"),
        "OCR image selection drift must warn, got: {warnings}"
    );
}

// Why: full-resolution OCR is a terminal sidecar concern. Letting API or
// workers inherit it multiplies latency and memory outside the OCR boundary.
#[test]
fn ocr_accuracy_profile_and_2048_cap_must_be_sidecar_scoped() {
    let tmp = TempDir::new().expect("tempdir");
    let unsafe_compose = GOOD_COMPOSE.replacen(
        "      <<: *health-audit-env\n      HEALTH_AUDIT_EMBED_PREWARM_ENABLED: \"false\"",
        "      <<: *health-audit-env\n      EXAMPLE_OCR_PROFILE: ${HEALTH_AUDIT_OCR_PROFILE:-accuracy}\n      EXAMPLE_OCR_IMAGE_MAX_LONG_SIDE: ${HEALTH_AUDIT_OCR_IMAGE_MAX_LONG_SIDE:-2048}\n      HEALTH_AUDIT_EMBED_PREWARM_ENABLED: \"false\"",
        1,
    );
    let unsafe_profile = GOOD_PROFILE.replace(
        "HEALTH_AUDIT_OCR_IMAGE_MAX_LONG_SIDE=2048",
        "HEALTH_AUDIT_OCR_IMAGE_MAX_LONG_SIDE=640",
    );
    write_runtime(
        tmp.path(),
        &unsafe_compose,
        GOOD_CELERY_CONFIG,
        &unsafe_profile,
    );

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("`ocr-sidecar`")
            && warnings.contains("accuracy/2048")
            && warnings.contains("HEALTH_AUDIT_OCR_IMAGE_MAX_LONG_SIDE=2048"),
        "OCR sidecar scope/default drift must warn, got: {warnings}"
    );
}

// Why: the 835-page production contract finished extraction but entered
// finalizing near the soft deadline. A synchronous PyMuPDF bbox scan then ran
// in C long enough for Celery to hard-kill the task at 1860 s. Exact bbox
// provenance is already available through the bounded on-demand API.
#[test]
fn synchronous_pdf_provenance_in_finalize_warns() {
    let tmp = TempDir::new().expect("tempdir");
    let router = format!("{GOOD_ROUTER}{GOOD_PRICE_LOOKUP}").replace(
        "    ttl_preview = _build_contract_ttl(contract_record)",
        "    _enrich_price_limit_sources_with_pdf_bboxes(contract_record, pdf_path)\n    ttl_preview = _build_contract_ttl(contract_record)",
    );
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(tmp.path(), "cartridges/health_audit/router.py", &router);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("must not scan whole PDFs") && warnings.contains("bbox provenance"),
        "synchronous finalize provenance must warn, got: {warnings}"
    );
}

// Why: accumulated contracts retain unresolved review candidates across source
// merges. Computing status from only the latest attachment can report
// preview_generated while the durable ledger still requires manual review.
#[test]
fn merged_finalize_status_must_use_persisted_record_warns() {
    let tmp = TempDir::new().expect("tempdir");
    let router = format!("{GOOD_ROUTER}{GOOD_PRICE_LOOKUP}").replace(
        r#"    persisted_contract_record = _persist_contract_record(
        contract_record,
        merge_with_existing=True,
    )
    if isinstance(persisted_contract_record, dict):
        contract_record = persisted_contract_record
    ttl_preview = _build_contract_ttl(contract_record)"#,
        r#"    _persist_contract_record(contract_record, merge_with_existing=True)
    ttl_preview = _build_contract_ttl(contract_record)"#,
    );
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(tmp.path(), "cartridges/health_audit/router.py", &router);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("durable merged relationship")
            && warnings.contains("existing unresolved review candidate"),
        "pre-merge finalize status must warn, got: {warnings}"
    );
}

// Why: the point image is the only production delivery vehicle until the full
// build window. A typo that copies services/extraction_template.py makes the
// build fail before py_compile and silently defeats the Rust payload schema fix.
#[test]
fn hotfix_extraction_template_overlay_must_use_real_path_warns() {
    let tmp = TempDir::new().expect("tempdir");
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    let bad_dockerfile = GOOD_HOTFIX_DOCKERFILE.replace(
        "cartridges/health_audit/domain/extraction_template.py",
        "cartridges/health_audit/services/extraction_template.py",
    );
    write(
        tmp.path(),
        "deploy/hotfix/health-audit-ingest/Dockerfile.api",
        &bad_dockerfile,
    );

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("must copy the real")
            && warnings.contains("cannot carry the Rust payload schema"),
        "invalid point-image template overlay must warn, got: {warnings}"
    );
}

// Why: the candidate image runs tenant ownership attestation before activation.
// Leaving tenant_audit.py in the base image would preserve the repeated JSONB
// aggregate that OOM-killed PostgreSQL on the 129 MiB expanded contract.
#[test]
fn hotfix_tenant_audit_overlay_must_use_real_path_warns() {
    let tmp = TempDir::new().expect("tempdir");
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    let bad_dockerfile = GOOD_HOTFIX_DOCKERFILE.replace(
        "cartridges/health_audit/tenant_audit.py",
        "cartridges/health_audit/services/tenant_audit.py",
    );
    write(
        tmp.path(),
        "deploy/hotfix/health-audit-ingest/Dockerfile.api",
        &bad_dockerfile,
    );

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("must copy and py_compile")
            && warnings.contains("OOM-prone base-image module"),
        "missing tenant-audit point overlay must warn, got: {warnings}"
    );
}

// Why: repeated JSONB operators in one aggregate detoasted a 129 MiB expanded
// contract enough times to exceed the 768 MiB PostgreSQL cgroup. One projected
// scalar per row retains fail-closed ownership checks without that fan-out.
#[test]
fn postgres_tenant_audit_jsonb_filter_fanout_warns() {
    let tmp = TempDir::new().expect("tempdir");
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(
        tmp.path(),
        "cartridges/health_audit/tenant_audit.py",
        r#"
def _collect_postgres_payload_counts(cursor, spec, expected_tenant):
    cursor.execute(
        "SELECT count(*) FILTER (WHERE jsonb_typeof(payload) = 'object'), "
        "count(*) FILTER (WHERE payload -> 'tenant_id' IS NULL), "
        "count(*) FILTER (WHERE payload -> 'tenant_id' IS NOT NULL) "
        "FROM ha_contracts"
    )
    return cursor.fetchone()

def _collect_postgres_summary(cursor, spec, expected_tenant):
    return _collect_postgres_payload_counts(cursor, spec, expected_tenant)
"#,
    );

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("project JSONB `tenant_id` once per row")
            && warnings.contains("OOM PostgreSQL"),
        "JSONB aggregate fan-out must warn, got: {warnings}"
    );
}

// Why: projecting the full JSONB beside the small tenant scalar would move the
// 129 MiB expanded contract into the client even though the query traverses the
// tenant field only once. The guardrail must reject that memory regression too.
#[test]
fn postgres_tenant_audit_full_payload_projection_warns() {
    let tmp = TempDir::new().expect("tempdir");
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    let unsafe_audit = GOOD_TENANT_AUDIT.replace(
        "SELECT tenant_id, {payload} -> 'tenant_id'",
        "SELECT tenant_id, {payload}, {payload} -> 'tenant_id'",
    );
    write(
        tmp.path(),
        "cartridges/health_audit/tenant_audit.py",
        &unsafe_audit,
    );

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("without selecting/casting the full payload")
            && warnings.contains("OOM PostgreSQL"),
        "full JSONB projection must warn, got: {warnings}"
    );
}

// Why: the 835-page native-text contract reached `finalizing` and then
// performed up to one full line scan for each of roughly 37k codes. That
// Python O(codes * lines) fallback can consume the entire Celery deadline even
// though pdf_fast already completed parsing.
#[test]
fn quadratic_contract_price_line_rescan_warns() {
    let tmp = TempDir::new().expect("tempdir");
    let router = format!(
        r#"{GOOD_ROUTER}

def _extract_contract_facts(working_text, lines):
    for raw_code in re.findall(r"\b\d{{8}}\b", working_text):
        for line in lines:
            if raw_code in line:
                return line
"#
    );
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(tmp.path(), "cartridges/health_audit/router.py", &router);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("quadratic") && warnings.contains("single-pass"),
        "quadratic code-by-line price lookup must warn, got: {warnings}"
    );
}

// Why: the doctor must detect the same O(codes * lines) shape after a future
// refactor swaps `re.findall` for the compiled TUSS-code regex and renames
// locals. The helper contract alone catches its absence; this proves the
// diagnostic also identifies the nested scan itself.
#[test]
fn compiled_regex_with_renamed_nested_price_rescan_warns() {
    let tmp = TempDir::new().expect("tempdir");
    let router = format!(
        r#"{GOOD_ROUTER}

def _extract_contract_facts(working_text, lines):
    for extracted_code in _EIGHT_DIGIT_CODE_RE.findall(working_text):
        for candidate_line in lines:
            if extracted_code in candidate_line:
                return candidate_line
"#
    );
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(tmp.path(), "cartridges/health_audit/router.py", &router);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("quadratic") && warnings.contains("single-pass"),
        "compiled-regex nested price lookup must warn, got: {warnings}"
    );
}

// Why: making the text scan linear is insufficient if every extracted code
// then calls DuckDB validate_code (up to four queries). The 835-page production
// proof exposed this second N x query fan-out after the first fix was deployed.
#[test]
fn per_code_tuss_validation_after_linear_index_warns() {
    let tmp = TempDir::new().expect("tempdir");
    let router = format!(
        r#"{GOOD_ROUTER}

def _extract_contract_facts(working_text, lines):
    for code, price, source_line in _index_priced_code_lines(lines):
        _resolve_tuss_code(code, repo)
    return None
"#
    );
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(tmp.path(), "cartridges/health_audit/router.py", &router);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("validate_codes_batch") && warnings.contains("per-code validation"),
        "per-code TUSS validation must warn, got: {warnings}"
    );
}

// Why: a single DuckDB connection error at the batch seam must not silently
// reopen the exact per-code query fan-out the batch helper was added to remove.
#[test]
fn failed_batch_tuss_validation_falling_back_per_code_warns() {
    let tmp = TempDir::new().expect("tempdir");
    let router = format!(
        r#"{GOOD_ROUTER}

def _resolve_tuss_codes_batch(raw_codes, repo):
    try:
        return repo.validate_codes_batch(raw_codes)
    except Exception:
        return {{code: _resolve_tuss_code(code, repo) for code in raw_codes}}

def _extract_contract_facts(working_text, lines):
    valid_codes = {{}}
    for code, price, source_line in _index_priced_code_lines(lines):
        valid_codes[code] = price
    _resolve_tuss_codes_batch(list(valid_codes), repo)
    if not valid_codes:
        _extract_semantic_price_candidates(lines)
    return valid_codes
"#
    );
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(tmp.path(), "cartridges/health_audit/router.py", &router);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("preserve normalized")
            && warnings.contains("falling back to per-code")
            && warnings.contains("tens of thousands"),
        "failed batch validation must not reopen per-code queries, got: {warnings}"
    );
}

// Why: a contract may mix explicit code+price rows with uncoded terms. Gating
// inference on the presence of *any* explicit code loses those terms instead
// of resolving them through the bounded, deduplicated fallback.
#[test]
fn semantic_price_inference_gated_by_explicit_prices_warns() {
    let tmp = TempDir::new().expect("tempdir");
    let router = format!(
        r#"{GOOD_ROUTER}

def _resolve_tuss_codes_batch(raw_codes, repo):
    return repo.validate_codes_batch(raw_codes)

def _extract_contract_facts(working_text, lines):
    valid_codes = {{}}
    for code, price, source_line in _index_priced_code_lines(lines):
        valid_codes[code] = price
    _resolve_tuss_codes_batch(list(valid_codes), repo)
    if not valid_codes:
        _extract_semantic_price_candidates(lines)
    return valid_codes
"#
    );
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(tmp.path(), "cartridges/health_audit/router.py", &router);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("must not gate semantic price inference")
            && warnings.contains("mixed contracts"),
        "mixed semantic inference must not be suppressed, got: {warnings}"
    );
}

// Why: allowing mixed contracts must not recreate the original fan-out. The
// fallback deduplicates term+price before lookup and has a query budget; this
// preserves whole-document parsing without a page count limit.
#[test]
fn mixed_semantic_price_inference_without_dedupe_and_budget_warns() {
    let tmp = TempDir::new().expect("tempdir");
    let router = format!(
        r#"{GOOD_ROUTER}

def _resolve_tuss_codes_batch(raw_codes, repo):
    return repo.validate_codes_batch(raw_codes)

def _extract_contract_facts(working_text, lines):
    valid_codes = {{}}
    for code, price, source_line in _index_priced_code_lines(lines):
        valid_codes[code] = price
    _resolve_tuss_codes_batch(list(valid_codes), repo)
    for term, price in _extract_semantic_price_candidates(lines):
        _infer_code_for_contract_term(term, price)
    return valid_codes
"#
    );
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(tmp.path(), "cartridges/health_audit/router.py", &router);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("deduplicate term+price")
            && warnings.contains("SEMANTIC_PRICE_INFERENCE_MAX_UNIQUE_CANDIDATES")
            && warnings.contains("page cap"),
        "mixed semantic fallback must have dedupe and a query budget, got: {warnings}"
    );
}

// Why: text-rich PDFs have canonical Rust structured rules. Calling only the
// Python native-text stream discards that output and reintroduces downstream
// rescans/fan-out even when the parser completed successfully.
#[test]
fn text_rich_pdf_structured_extractor_bypass_warns() {
    let tmp = TempDir::new().expect("tempdir");
    let router = format!("{GOOD_ROUTER}{GOOD_PRICE_LOOKUP}").replace(
        r#"def process_contract_ingest_job():
    extractor_payload = _run_contract_extractor(
        extractor_env=_native_pdf_contract_extractor_env(),
    )
    structured_extraction = _contract_extractor_payload_to_extraction(extractor_payload)
    if structured_extraction is None:
        return "native-text-stream"
    return structured_extraction"#,
        r#"def process_contract_ingest_job():
    return "native-text-stream""#,
    );
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(tmp.path(), "cartridges/health_audit/router.py", &router);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("text-rich async PDFs")
            && warnings.contains("_native_pdf_contract_extractor_env")
            && warnings.contains("may only run when `structured_extraction is None`"),
        "structured text extraction bypass must warn, got: {warnings}"
    );
}

#[test]
fn available_empty_contract_batches_warn() {
    let tmp = TempDir::new().expect("tempdir");
    let router = format!("{GOOD_ROUTER}{GOOD_PRICE_LOOKUP}").replace(
        "if payload.get(\"available\") and _contract_page_batch_has_pdf_evidence(payload):",
        "if payload.get(\"available\"):",
    );
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(tmp.path(), "cartridges/health_audit/router.py", &router);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("available-but-empty") && warnings.contains("Flight OCR"),
        "empty successful batches must warn, got: {warnings}"
    );
}

#[test]
fn partial_contract_batches_warn() {
    let tmp = TempDir::new().expect("tempdir");
    let router = format!("{GOOD_ROUTER}{GOOD_PRICE_LOOKUP}").replace(
        "if not batch_results or batch_failures:",
        "if not batch_results:",
    );
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(tmp.path(), "cartridges/health_audit/router.py", &router);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("partial regulated contract")
            && warnings.contains("whole-document Flight OCR"),
        "partial batch merges must warn, got: {warnings}"
    );
}

#[test]
fn empty_batch_placeholder_containers_warn() {
    let tmp = TempDir::new().expect("tempdir");
    let router = format!("{GOOD_ROUTER}{GOOD_PRICE_LOOKUP}")
        .replace("field_values.values()", "field_values")
        .replace("_table_html_to_text(table)", "str(table)");
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(tmp.path(), "cartridges/health_audit/router.py", &router);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("empty placeholder containers")
            && warnings.contains("meaningful field values"),
        "empty evidence placeholders must warn, got: {warnings}"
    );
}

#[test]
fn empty_final_structured_container_warns() {
    let tmp = TempDir::new().expect("tempdir");
    let router = format!("{GOOD_ROUTER}{GOOD_PRICE_LOOKUP}")
        .replace(
            "    structured_evidence = dict(extraction)\n    structured_evidence[\"text\"] = \"\"\n    return _contract_page_batch_has_pdf_evidence(structured_evidence)",
            "    field_values = extraction.get(\"field_values\") or {}\n    return _coerce_structured_contract_extraction(field_values) is not None",
        )
        .replace(
            "    if _contract_extraction_has_pdf_evidence(extraction):",
            "    field_values = extraction.get(\"field_values\") or {}\n    if _coerce_structured_contract_extraction(field_values) is not None:",
        );
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(tmp.path(), "cartridges/health_audit/router.py", &router);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("empty `contract_extraction` container")
            && warnings.contains("meaningful structured leaves"),
        "empty final structured evidence must warn, got: {warnings}"
    );
}

#[test]
fn repeated_formal_pdf_rule_scan_warns() {
    let tmp = TempDir::new().expect("tempdir");
    let router = format!("{GOOD_ROUTER}{GOOD_PRICE_LOOKUP}")
        .replace(
            "    contract_has_formal_pdf_structure = _contract_has_formal_pdf_structure(contract)\n",
            "",
        )
        .replace(
            "            contract_has_formal_pdf_structure=contract_has_formal_pdf_structure,\n",
            "",
        );
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(tmp.path(), "cartridges/health_audit/router.py", &router);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("formal PDF structure once") && warnings.contains("quadratic"),
        "per-code formal PDF scans must warn, got: {warnings}"
    );
}

#[test]
fn formal_pdf_cache_must_scope_and_parse_legacy_metadata_safely() {
    let tmp = TempDir::new().expect("tempdir");
    let router = format!("{GOOD_ROUTER}{GOOD_PRICE_LOOKUP}")
        .replace(
            "referenced_bundle_source_contract_ids",
            "all_bundle_source_ids",
        )
        .replace(
            "_parse_positive_int(source.get(\"structured_pricing_rule_count\"), 0)",
            "int(source.get(\"structured_pricing_rule_count\") or 0)",
        );
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(tmp.path(), "cartridges/health_audit/router.py", &router);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("scope its bundle index to referenced sources")
            && warnings.contains("tolerate legacy counts"),
        "legacy bundle metadata cache drift must warn, got: {warnings}"
    );
}

#[test]
fn repeated_rule_serialization_deduplication_warns() {
    let tmp = TempDir::new().expect("tempdir");
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(
        tmp.path(),
        "cartridges/health_audit/services/contract_airgapped.py",
        r#"
def _append_unique_contract_rule(target, rule):
    for existing in target:
        if str(existing) == str(rule):
            return
    target.append(rule)

def _extract_airgapped_pricing_rules(lines, filename):
    rules = []
    for line in lines:
        _append_unique_contract_rule(rules, {"description": line})
    return rules
"#,
    );

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("fingerprint set") && warnings.contains("quadratic"),
        "per-rule full-list deduplication must warn, got: {warnings}"
    );
}

#[test]
fn every_airgapped_rule_extractor_requires_shared_fingerprints() {
    let tmp = TempDir::new().expect("tempdir");
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    let unsafe_airgap = GOOD_CONTRACT_AIRGAPPED
        .replace(
            "def _extract_airgapped_operational_rules(lines, filename):\n    rules = []\n    fingerprints: set[str] = set()",
            "def _extract_airgapped_operational_rules(lines, filename):\n    rules = []",
        )
        .replace(
            "def _extract_airgapped_auth_rules(lines, filename):\n    rules = []\n    fingerprints: set[str] = set()",
            "def _extract_airgapped_auth_rules(lines, filename):\n    rules = []",
        );
    write(
        tmp.path(),
        "cartridges/health_audit/services/contract_airgapped.py",
        &unsafe_airgap,
    );

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("pricing, operational, and authorization rules")
            && warnings.contains("quadratic"),
        "every rule family must share fingerprints, got: {warnings}"
    );
}

#[test]
fn repeated_full_source_provenance_copy_warns() {
    let tmp = TempDir::new().expect("tempdir");
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(
        tmp.path(),
        "cartridges/health_audit/services/contract_sources.py",
        r#"
import copy

def rebuild_contract_from_sources(contract, source_records):
    for source in source_records:
        for code, raw_price in source.get("price_limits", {}).items():
            source_price_sources = copy.deepcopy(
                source.get("price_limit_sources") or {}
            )
            source_price_sources[code] = {"price": raw_price}
    return contract
"#,
    );

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("copy each source price-provenance map once")
            && warnings.contains("quadratic"),
        "per-code full provenance copies must warn, got: {warnings}"
    );
}

#[test]
fn repeated_full_source_provenance_copy_during_synchronization_warns() {
    let tmp = TempDir::new().expect("tempdir");
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(
        tmp.path(),
        "cartridges/health_audit/services/contract_sources.py",
        r#"
import copy

def _synchronize_current_price_provenance(contract, source_records):
    sources = copy.deepcopy(source_records)
    for code, evidence in contract.get("price_limit_sources", {}).items():
        for source in sources:
            if code not in source.get("price_limits", {}):
                continue
            source_price_sources = copy.deepcopy(
                source.get("price_limit_sources") or {}
            )
            source_price_sources[code] = evidence
            source["price_limit_sources"] = source_price_sources
    return sources

def rebuild_contract_from_sources(contract, source_records):
    for source in source_records:
        source_price_sources = copy.deepcopy(source.get("price_limit_sources") or {})
        for code, raw_price in source.get("price_limits", {}).items():
            source_price_sources[code] = {"price": raw_price}
    return contract
"#,
    );

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("ownership indexes")
            && warnings.contains("copied source ledger in place")
            && warnings.contains("quadratic"),
        "late-provenance full-map copies must warn, got: {warnings}"
    );
}

// Why: Celery accepts `-d` as the exact alias of `--destination`, and a
// CMD-SHELL probe may obtain the container hostname with `$(hostname)`.
// Rejecting that equivalent shape would block a healthy production compose.
#[test]
fn short_destination_with_shell_hostname_is_targeted() {
    let tmp = TempDir::new().expect("tempdir");
    let compose = GOOD_COMPOSE
        .replace(
            r#"--destination "health-audit-audits@${HOSTNAME}""#,
            r#"-d "health-audit-audits@$$(hostname)""#,
        )
        .replace(
            r#"--destination "health-audit-contracts@${HOSTNAME}""#,
            r#"-d "health-audit-contracts@$$(hostname)""#,
        );
    write_runtime(tmp.path(), &compose, GOOD_CELERY_CONFIG, GOOD_PROFILE);

    let warnings = warning_text(tmp.path());

    assert!(
        !warnings.contains("healthcheck"),
        "equivalent targeted healthchecks must pass, got: {warnings}"
    );
}

// Why: generic solo-worker profiles must not inherit an always-on heartbeat.
// Health Audit enables it explicitly through its profile and Compose boundary.
#[test]
fn generic_none_with_profile_scoped_heartbeat_passes() {
    let tmp = TempDir::new().expect("tempdir");
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);

    let warnings = warning_text(tmp.path());

    assert!(
        !warnings.contains("broker_heartbeat"),
        "generic None with a finite Health Audit override must pass, got: {warnings}"
    );
}

// Why: a global finite default makes every Celery profile send heartbeats,
// including existing solo workers that cannot service them while busy.
#[test]
fn global_finite_heartbeat_default_is_rejected() {
    let tmp = TempDir::new().expect("tempdir");
    let celery_config = GOOD_CELERY_CONFIG.replace(
        "broker_heartbeat = int(_broker_heartbeat_raw) if _broker_heartbeat_raw else None",
        "broker_heartbeat = 30",
    );
    write_runtime(tmp.path(), GOOD_COMPOSE, &celery_config, GOOD_PROFILE);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("broker_heartbeat")
            && warnings.contains("generic")
            && warnings.contains("CELERY_BROKER_HEARTBEAT_SECONDS"),
        "global finite heartbeat default must warn, got: {warnings}"
    );
}

// Why: the generic default is deliberately disabled, so the standalone
// production profile must provide the finite heartbeat used for broker health.
#[test]
fn profile_scoped_heartbeat_default_is_required() {
    let tmp = TempDir::new().expect("tempdir");
    let profile = GOOD_PROFILE.replace("CELERY_BROKER_HEARTBEAT_SECONDS=30\n", "");
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, &profile);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("deploy/profiles/health_audit.env")
            && warnings.contains("CELERY_BROKER_HEARTBEAT_SECONDS=30"),
        "missing Health Audit heartbeat profile default must warn, got: {warnings}"
    );
}

// Why: a profile value is inert unless Compose carries it into both isolated
// worker processes with a finite standalone default.
#[test]
fn compose_scoped_heartbeat_default_is_required() {
    let tmp = TempDir::new().expect("tempdir");
    let compose = GOOD_COMPOSE.replace(
        "CELERY_BROKER_HEARTBEAT_SECONDS: ${CELERY_BROKER_HEARTBEAT_SECONDS:-30}",
        "CELERY_BROKER_HEARTBEAT_SECONDS: ${CELERY_BROKER_HEARTBEAT_SECONDS:-}",
    );
    write_runtime(tmp.path(), &compose, GOOD_CELERY_CONFIG, GOOD_PROFILE);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("docker-compose.health-audit.yml")
            && warnings.contains("CELERY_BROKER_HEARTBEAT_SECONDS")
            && warnings.contains("30"),
        "missing finite Compose heartbeat default must warn, got: {warnings}"
    );
}

// Why: the standalone production API must never execute an accepted XML audit
// in-process when the dedicated Celery worker is absent or dispatch fails.
// The generic/development runtime may retain its compatibility fallback.
#[test]
fn standalone_profile_disables_xml_glosa_local_fallback() {
    let tmp = TempDir::new().expect("tempdir");
    let profile = GOOD_PROFILE.replace(
        "HEALTH_AUDIT_XML_GLOSA_LOCAL_FALLBACK_ENABLED=false",
        "HEALTH_AUDIT_XML_GLOSA_LOCAL_FALLBACK_ENABLED=true",
    );
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, &profile);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("deploy/profiles/health_audit.env")
            && warnings.contains("HEALTH_AUDIT_XML_GLOSA_LOCAL_FALLBACK_ENABLED=false"),
        "enabled standalone profile fallback must warn, got: {warnings}"
    );
}

// Why: a safe profile value is inert unless the standalone Compose boundary
// carries an explicit disabled default into the API and both worker roles.
#[test]
fn standalone_compose_disables_xml_glosa_local_fallback_for_all_runtime_roles() {
    let tmp = TempDir::new().expect("tempdir");
    let compose = GOOD_COMPOSE.replace(
        "${HEALTH_AUDIT_XML_GLOSA_LOCAL_FALLBACK_ENABLED:-false}",
        "${HEALTH_AUDIT_XML_GLOSA_LOCAL_FALLBACK_ENABLED:-true}",
    );
    write_runtime(tmp.path(), &compose, GOOD_CELERY_CONFIG, GOOD_PROFILE);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("example-api/docker-compose.health-audit.yml")
            && warnings.contains("HEALTH_AUDIT_XML_GLOSA_LOCAL_FALLBACK_ENABLED")
            && warnings.contains("api")
            && warnings.contains("worker")
            && warnings.contains("contract-worker")
            && warnings.contains("false"),
        "enabled standalone Compose fallback must warn for every runtime role, got: {warnings}"
    );
}

// Why: this is a production-profile invariant, not a global compatibility
// change. Shared generic/dev deployments may continue to default the fallback
// on while the standalone Health Audit boundary forces it off.
#[test]
fn generic_compose_may_keep_xml_glosa_local_fallback_enabled() {
    let tmp = TempDir::new().expect("tempdir");
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(
        tmp.path(),
        "example-api/docker-compose.yml",
        "services:\n  api:\n    environment:\n      HEALTH_AUDIT_XML_GLOSA_LOCAL_FALLBACK_ENABLED: ${HEALTH_AUDIT_XML_GLOSA_LOCAL_FALLBACK_ENABLED:-true}\n",
    );

    let warnings = warning_text(tmp.path());

    assert!(
        !warnings.contains("HEALTH_AUDIT_XML_GLOSA_LOCAL_FALLBACK_ENABLED"),
        "generic/dev compatibility fallback must remain outside the standalone doctor: {warnings}"
    );
}

// Why: route variables may use descriptive module-level names. What matters
// is that the alias resolves to the canonical queue environment key.
#[test]
fn canonical_contract_queue_alias_passes() {
    let tmp = TempDir::new().expect("tempdir");
    let celery_config = GOOD_CELERY_CONFIG
        .replace(
            "contract_queue = os.getenv(",
            "HEALTH_AUDIT_CONTRACT_QUEUE = os.getenv(",
        )
        .replace(
            r#""cartridge.health_audit.ingest_contract": {"queue": contract_queue}"#,
            r#""cartridge.health_audit.ingest_contract": {"queue": HEALTH_AUDIT_CONTRACT_QUEUE}"#,
        );
    write_runtime(tmp.path(), GOOD_COMPOSE, &celery_config, GOOD_PROFILE);

    let warnings = warning_text(tmp.path());

    assert!(
        !warnings.contains("ingest_contract"),
        "canonical queue alias must pass, got: {warnings}"
    );
}

// Why: a solo worker cannot service broker heartbeats or remote-control
// health probes while a long contract task occupies its only process.
#[test]
fn solo_pool_is_rejected() {
    let tmp = TempDir::new().expect("tempdir");
    let compose = GOOD_COMPOSE.replacen("-P prefork", "-P solo", 1);
    write_runtime(tmp.path(), &compose, GOOD_CELERY_CONFIG, GOOD_PROFILE);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("prefork"),
        "solo pool warning must require prefork, got: {warnings}"
    );
}

// Why: an always-disabled Docker healthcheck let the dead production consumer
// remain `running` for hours while RabbitMQ had zero consumers.
#[test]
fn disabled_worker_healthcheck_is_rejected() {
    let tmp = TempDir::new().expect("tempdir");
    let compose = GOOD_COMPOSE.replacen(
        r#"healthcheck:
      test:
        - CMD-SHELL
        - >-
          celery -A example.celery_app.app inspect ping
          --destination "health-audit-audits@${HOSTNAME}" --timeout=5 |
          grep -q pong"#,
        "healthcheck:\n      disable: true",
        1,
    );
    write_runtime(tmp.path(), &compose, GOOD_CELERY_CONFIG, GOOD_PROFILE);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("healthcheck"),
        "disabled healthcheck must warn, got: {warnings}"
    );
}

// Why: a broad ping can be answered by a sibling while the intended worker
// has lost its consumer, so each healthcheck must target its own node.
#[test]
fn non_targeted_worker_healthcheck_is_rejected() {
    let tmp = TempDir::new().expect("tempdir");
    let compose = GOOD_COMPOSE.replacen(
        r#"--destination "health-audit-audits@${HOSTNAME}" --timeout=5"#,
        "--timeout=5",
        1,
    );
    write_runtime(tmp.path(), &compose, GOOD_CELERY_CONFIG, GOOD_PROFILE);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("target"),
        "non-targeted healthcheck must warn, got: {warnings}"
    );
}

// Why: more than one process per role would exceed the carefully bounded
// model memory footprint and reintroduce concurrent status-file writers.
#[test]
fn worker_concurrency_must_be_one() {
    let tmp = TempDir::new().expect("tempdir");
    let compose = GOOD_COMPOSE.replacen("--concurrency=1", "--concurrency=2", 1);
    write_runtime(tmp.path(), &compose, GOOD_CELERY_CONFIG, GOOD_PROFILE);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("concurrency=1"),
        "worker concurrency drift must warn, got: {warnings}"
    );
}

// Why: BGE-M3 is a ~568M-parameter float32 model. Its weights alone exceed
// 2 GiB, so the audit worker needs at least 3 GiB before Python, Torch, and
// inference activations are counted.
#[test]
fn audit_worker_memory_must_cover_bge_m3_float32() {
    let tmp = TempDir::new().expect("tempdir");
    let compose = GOOD_COMPOSE.replacen("mem_limit: 4096m", "mem_limit: 2048m", 1);
    write_runtime(tmp.path(), &compose, GOOD_CELERY_CONFIG, GOOD_PROFILE);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("worker") && warnings.contains("mem_limit") && warnings.contains("3 GiB"),
        "undersized audit-worker memory must warn, got: {warnings}"
    );
}

// Why: contract ingestion holds large PDFs and parser results in-process even
// though OCR inference lives in a sidecar. A 1 GiB cap is not a safe production
// floor for the 835-page contracts represented in the incident evidence.
#[test]
fn contract_worker_memory_must_keep_parser_headroom() {
    let tmp = TempDir::new().expect("tempdir");
    let compose = GOOD_COMPOSE.replacen("mem_limit: 2048m", "mem_limit: 1024m", 1);
    write_runtime(tmp.path(), &compose, GOOD_CELERY_CONFIG, GOOD_PROFILE);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("contract-worker")
            && warnings.contains("mem_limit")
            && warnings.contains("1.5 GiB"),
        "undersized contract-worker memory must warn, got: {warnings}"
    );
}

// Why: worker isolation must not silently increase the VM reservation beyond
// the previous 6 GiB single-worker budget.
#[test]
fn split_worker_memory_keeps_a_bounded_aggregate() {
    let tmp = TempDir::new().expect("tempdir");
    let compose = GOOD_COMPOSE.replacen("mem_limit: 2048m", "mem_limit: 3072m", 1);
    write_runtime(tmp.path(), &compose, GOOD_CELERY_CONFIG, GOOD_PROFILE);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("aggregate") && warnings.contains("6 GiB"),
        "aggregate worker memory drift must warn, got: {warnings}"
    );
}

// Why: neither authoritative worker may start a model thread or hold the glosa
// queue behind embedding initialization.
#[test]
fn authoritative_worker_embedding_prewarm_must_be_disabled() {
    let tmp = TempDir::new().expect("tempdir");
    let compose = GOOD_COMPOSE.replace(
        r#"HEALTH_AUDIT_EMBED_PREWARM_ENABLED: "false""#,
        r#"HEALTH_AUDIT_EMBED_PREWARM_ENABLED: "true""#,
    );
    write_runtime(tmp.path(), &compose, GOOD_CELERY_CONFIG, GOOD_PROFILE);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("prewarm"),
        "contract prewarm drift must warn, got: {warnings}"
    );
}

// Why: role-specific Compose values are inert unless the worker-process hook
// consumes the toggle before loading the embedding model.
#[test]
fn embedding_prewarm_toggle_must_be_consumed() {
    let tmp = TempDir::new().expect("tempdir");
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(
        tmp.path(),
        "cartridges/health_audit/tasks.py",
        &GOOD_TASKS.replace("HEALTH_AUDIT_EMBED_PREWARM_ENABLED", "IGNORED_PREWARM_FLAG"),
    );

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("HEALTH_AUDIT_EMBED_PREWARM_ENABLED"),
        "unused prewarm toggle must warn, got: {warnings}"
    );
}

// Why: contract OCR and interactive XML auditing on one effective queue lets
// a single pathological PDF starve every auditor-facing task.
#[test]
fn contract_and_xml_tasks_cannot_share_an_effective_queue() {
    let tmp = TempDir::new().expect("tempdir");
    let profile = GOOD_PROFILE.replace(
        "HEALTH_AUDIT_CONTRACT_INGEST_QUEUE=health-audit-contracts",
        "HEALTH_AUDIT_CONTRACT_INGEST_QUEUE=health-audit-audits",
    );
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, &profile);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("distinct"),
        "shared effective queues must warn, got: {warnings}"
    );
}

// Why: inner OCR subprocess timeouts do not bound the Celery task itself; the
// production task spun for more than twenty-one hours.
#[test]
fn whole_task_soft_and_hard_limits_are_required() {
    let tmp = TempDir::new().expect("tempdir");
    let celery_config = GOOD_CELERY_CONFIG.replace("task_annotations = {", "annotations = {");
    write_runtime(tmp.path(), GOOD_COMPOSE, &celery_config, GOOD_PROFILE);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("task_annotations"),
        "missing whole-task limits must warn, got: {warnings}"
    );
}

// Why: separate workers do not isolate traffic unless producers route every
// Health Audit task to the corresponding dedicated queue.
#[test]
fn per_task_routes_are_required() {
    let tmp = TempDir::new().expect("tempdir");
    let celery_config = GOOD_CELERY_CONFIG.replace("task_routes = {", "routes = {");
    write_runtime(tmp.path(), GOOD_COMPOSE, &celery_config, GOOD_PROFILE);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("task_routes"),
        "missing task routes must warn, got: {warnings}"
    );
}

// Why: health_audit is also loaded by shared `jcube` and `full` deployments,
// whose generic worker consumes `celery`. Only the standalone production
// profile may opt into the dedicated queue names.
#[test]
fn shared_profile_queue_fallbacks_must_remain_celery() {
    let cases = [
        (
            "cartridges/health_audit/tasks.py",
            GOOD_TASKS,
            r#"os.getenv("HEALTH_AUDIT_CONTRACT_INGEST_QUEUE", "celery")"#,
            r#"os.getenv("HEALTH_AUDIT_CONTRACT_INGEST_QUEUE", "health-audit-contracts")"#,
        ),
        (
            "example-api/example/celery_app/celeryconfig.py",
            GOOD_CELERY_CONFIG,
            r#"os.getenv("HEALTH_AUDIT_AUDIT_QUEUE", "celery")"#,
            r#"os.getenv("HEALTH_AUDIT_AUDIT_QUEUE", "health-audit-audits")"#,
        ),
        (
            "cartridges/health_audit/router.py",
            GOOD_ROUTER,
            r#"os.getenv("HEALTH_AUDIT_XML_GLOSA_QUEUE", "celery")"#,
            r#"os.getenv("HEALTH_AUDIT_XML_GLOSA_QUEUE", "health-audit-audits")"#,
        ),
        (
            "cartridges/health_audit/routes/contracts.py",
            GOOD_CONTRACT_ROUTES,
            r#""HEALTH_AUDIT_CONTRACT_INGEST_QUEUE", "celery""#,
            r#""HEALTH_AUDIT_CONTRACT_INGEST_QUEUE", "health-audit-contracts""#,
        ),
    ];

    for (path, source, safe, unsafe_fallback) in cases {
        let tmp = TempDir::new().expect("tempdir");
        write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
        write(tmp.path(), path, &source.replacen(safe, unsafe_fallback, 1));

        let warnings = warning_text(tmp.path());

        assert!(
            warnings.contains(path) && warnings.contains("fallback") && warnings.contains("celery"),
            "unsafe shared-profile fallback in {path} must warn, got: {warnings}"
        );
    }
}

// Why: task and router call sites may share one canonical queue resolver. The
// doctor must follow that safe indirection instead of demanding duplicated
// getenv fallbacks that can drift independently.
#[test]
fn centralized_xml_queue_resolver_preserves_shared_profile_fallback() {
    let tmp = TempDir::new().expect("tempdir");
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(
        tmp.path(),
        "cartridges/health_audit/tasks.py",
        CENTRALIZED_XML_TASKS,
    );
    write(
        tmp.path(),
        "cartridges/health_audit/router.py",
        CENTRALIZED_XML_ROUTER,
    );

    let warnings = warning_text(tmp.path());

    assert!(
        !warnings.contains("HEALTH_AUDIT_XML_GLOSA_QUEUE"),
        "canonical XML queue resolver must preserve the celery fallback, got: {warnings}"
    );
}

// Why: recognizing the resolver indirection is only safe while the canonical
// helper itself retains `celery` after both standalone queue inputs are empty.
#[test]
fn centralized_xml_queue_resolver_must_retain_celery_fallback() {
    let tmp = TempDir::new().expect("tempdir");
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, GOOD_PROFILE);
    write(
        tmp.path(),
        "cartridges/health_audit/tasks.py",
        CENTRALIZED_XML_TASKS,
    );
    write(
        tmp.path(),
        "cartridges/health_audit/router.py",
        CENTRALIZED_XML_ROUTER,
    );
    write(
        tmp.path(),
        "cartridges/health_audit/services/xml_glosa_queue.py",
        &GOOD_XML_GLOSA_QUEUE_HELPER.replace("\"celery\"", "\"health-audit-audits\""),
    );

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("cartridges/health_audit/services/xml_glosa_queue.py")
            && warnings.contains("celery"),
        "unsafe canonical XML queue resolver must warn, got: {warnings}"
    );
}

// Why: a profile-scoped heartbeat is inert if the generic config no longer
// consumes its environment key, and without STARTED state an operator cannot
// distinguish queued from active.
#[test]
fn heartbeat_and_started_tracking_are_required() {
    let tmp = TempDir::new().expect("tempdir");
    let celery_config = GOOD_CELERY_CONFIG
        .replace(
            "broker_heartbeat = int(_broker_heartbeat_raw) if _broker_heartbeat_raw else None",
            "broker_heartbeat = None",
        )
        .replace("task_track_started = True", "task_track_started = False");
    write_runtime(tmp.path(), GOOD_COMPOSE, &celery_config, GOOD_PROFILE);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("broker_heartbeat") && warnings.contains("task_track_started"),
        "heartbeat and started-state drift must warn, got: {warnings}"
    );
}

// Why: defaults in the deploy profile make queue isolation survive rendered
// Compose configuration instead of depending on an operator's local `.env`.
#[test]
fn profile_queue_defaults_are_required() {
    let tmp = TempDir::new().expect("tempdir");
    let profile = GOOD_PROFILE.replace(
        "HEALTH_AUDIT_CONTRACT_INGEST_QUEUE=health-audit-contracts\n",
        "",
    );
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, &profile);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("HEALTH_AUDIT_CONTRACT_INGEST_QUEUE"),
        "missing profile default must name the key, got: {warnings}"
    );
}

// Why: the timeout values must survive rendered production configuration and
// stay operator-tunable without a source rebuild.
#[test]
fn profile_timeout_defaults_are_required() {
    let tmp = TempDir::new().expect("tempdir");
    let profile = GOOD_PROFILE.replace("HEALTH_AUDIT_CONTRACT_TIME_LIMIT_SECONDS=1860\n", "");
    write_runtime(tmp.path(), GOOD_COMPOSE, GOOD_CELERY_CONFIG, &profile);

    let warnings = warning_text(tmp.path());

    assert!(
        warnings.contains("HEALTH_AUDIT_CONTRACT_TIME_LIMIT_SECONDS"),
        "missing timeout profile default must name the key, got: {warnings}"
    );
}
