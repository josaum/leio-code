use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct ProdSurfaceHygieneDoctor;

impl Doctor for ProdSurfaceHygieneDoctor {
    fn name(&self) -> &'static str {
        "prod-surface-hygiene"
    }

    fn description(&self) -> &'static str {
        "Checks cheap prod hardening invariants for auth refresh, WhatsApp flow key handling, OCR Flight response budgeting, and gateway HTTP middleware."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_prod_surface_hygiene(index, root)
    }
}

pub fn doctor_prod_surface_hygiene(_index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let auth_path = root.join("example-api/example/routers/auth.py");
    let flow_path = root.join("example-api/example/integrations/whatsapp/flow_exchange.py");
    let ocr_path = root.join("example-gateway/src/flight/ocr.rs");
    let gateway_cargo_path = root.join("example-gateway/Cargo.toml");
    let gateway_main_path = root.join("example-gateway/src/main.rs");
    let auth_tests_path = root.join("example-api/example/tests/api/test_auth_router.py");
    let flow_tests_path = root.join("example-api/example/tests/api/test_whatsapp_flow_exchange.py");

    let auth_src = read_text(&auth_path, &mut warnings);
    let flow_src = read_text(&flow_path, &mut warnings);
    let ocr_src = read_text(&ocr_path, &mut warnings);
    let gateway_cargo_src = read_text(&gateway_cargo_path, &mut warnings);
    let gateway_main_src = read_text(&gateway_main_path, &mut warnings);
    let auth_tests_src = read_text(&auth_tests_path, &mut warnings);
    let flow_tests_src = read_text(&flow_tests_path, &mut warnings);

    if let Some(src) = auth_src.as_deref() {
        for (needle, detail) in [
            (
                "@limiter.limit(\"20/minute\")",
                "refresh endpoint is rate-limited against invalid-token storms",
            ),
            (
                "@router.post(\"/refresh\")",
                "canonical Python refresh route is present",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "code".to_string(),
                    path: auth_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            } else {
                warnings.push(format!(
                    "auth router missing prod hygiene invariant: {needle}"
                ));
            }
        }
    }

    if let Some(src) = flow_src.as_deref() {
        for (needle, detail) in [
            (
                "def _read_private_key_file(path: Path) -> str:",
                "WhatsApp flow key reads are wrapped so filesystem failures become controlled runtime errors",
            ),
            (
                "status_code=status.HTTP_503_SERVICE_UNAVAILABLE",
                "WhatsApp flow key failures degrade to 503 instead of unhandled exception spam",
            ),
            (
                "_emit_flow_event(\"flow_private_key_unavailable\")",
                "unified flow path emits an explicit event when the private key is unavailable",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "code".to_string(),
                    path: flow_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            } else {
                warnings.push(format!(
                    "WhatsApp flow exchange missing prod hygiene invariant: {needle}"
                ));
            }
        }
    }

    if let Some(src) = ocr_src.as_deref() {
        for (needle, detail) in [
            (
                "const OCR_FLIGHT_RESPONSE_BUDGET_BYTES: usize = 3 * 1024 * 1024;",
                "OCR Flight responses are capped below the default gRPC 4 MiB ceiling",
            ),
            (
                "fn fit_transport_budget(&mut self) {",
                "OCR Flight payloads compact themselves before Arrow serialization",
            ),
            (
                "fn serialize_tables_for_flight(tables_html: &[String]) -> String {",
                "table-heavy OCR payloads are structurally trimmed before transport",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "code".to_string(),
                    path: ocr_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            } else {
                warnings.push(format!(
                    "OCR Flight transport budget invariant missing: {needle}"
                ));
            }
        }
    }

    if let Some(src) = gateway_cargo_src.as_deref() {
        for (needle, detail) in [
            (
                "tower-http = { version = \"0.6\"",
                "gateway uses the workspace tower-http 0.6 middleware baseline",
            ),
            (
                "tower = { version = \"0.5\"",
                "gateway uses the workspace tower 0.5 service baseline",
            ),
            (
                "\"trace\", \"request-id\", \"sensitive-headers\", \"set-header\"",
                "gateway enables the tower-http hardening middleware feature set",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "code".to_string(),
                    path: gateway_cargo_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            } else {
                warnings.push(format!(
                    "gateway tower-http dependency hygiene invariant missing: {needle}"
                ));
            }
        }
    }

    if let Some(src) = gateway_main_src.as_deref() {
        for (needle, detail) in [
            (
                "GATEWAY_CORS_ALLOWED_ORIGINS",
                "gateway credentialed CORS is driven by an explicit production allowlist",
            ),
            (
                "AllowOrigin::predicate",
                "gateway credentialed CORS uses a constrained origin predicate instead of mirroring request origins",
            ),
            (
                "host.ends_with(\".getjai.com\")",
                "gateway allows any getjai.com subdomain for production frontends",
            ),
            (
                "host.ends_with(\".vercel.app\")",
                "gateway allows Vercel preview/deploy origins without opening arbitrary origins",
            ),
            (
                "SetSensitiveRequestHeadersLayer::new",
                "gateway marks sensitive request headers before HTTP tracing",
            ),
            (
                "SetRequestIdLayer::new",
                "gateway assigns request IDs at the tower layer",
            ),
            (
                "TraceLayer::new_for_http()",
                "gateway HTTP requests pass through tower-http tracing",
            ),
            (
                "SetSensitiveResponseHeadersLayer::new",
                "gateway marks sensitive response headers before traced responses are logged",
            ),
            (
                "TimeoutLayer::with_status_code",
                "gateway uses current tower-http timeout API with explicit timeout status",
            ),
            (
                "SetResponseHeaderLayer::if_not_present",
                "gateway applies security response headers at the tower layer",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "code".to_string(),
                    path: gateway_main_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            } else {
                warnings.push(format!(
                    "gateway HTTP middleware prod hygiene invariant missing: {needle}"
                ));
            }
        }

        if let Some(line) = find_line(src, "AllowOrigin::mirror_request()") {
            warnings.push(format!(
                "{}: gateway credentialed CORS still mirrors arbitrary origins at line {line}",
                gateway_main_path.display()
            ));
        }
    }

    if let Some(src) = auth_tests_src.as_deref() {
        if let Some(line) = find_line(src, "test_refresh_route_rate_limits_invalid_token_storms") {
            evidence.push(EvidenceItem {
                kind: "test".to_string(),
                path: auth_tests_path.display().to_string(),
                line: Some(line),
                detail: "auth refresh rate limiting has regression coverage".to_string(),
            });
        } else {
            warnings.push(
                "missing regression coverage for auth refresh invalid-token rate limiting"
                    .to_string(),
            );
        }
    }

    if let Some(src) = flow_tests_src.as_deref() {
        for (needle, detail) in [
            (
                "test_load_flow_private_key_pem_wraps_os_errors",
                "filesystem read failures for WhatsApp flow keys are regression-tested",
            ),
            (
                "test_handle_flow_exchange_request_returns_503_when_private_key_unavailable",
                "encrypted flow requests degrade cleanly when the private key is unavailable",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "test".to_string(),
                    path: flow_tests_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            } else {
                warnings.push(format!(
                    "missing prod hygiene regression coverage: {needle}"
                ));
            }
        }
    }

    entities.push(json!({
        "doctor": "prod-surface-hygiene",
        "checks": ["auth_refresh_limit", "whatsapp_flow_private_key", "ocr_flight_budget", "gateway_tower_http_hardening"],
        "warning_count": warnings.len(),
        "evidence_count": evidence.len(),
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_prod_surface_hygiene"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            "prod surfaces have explicit guardrails for auth refresh storms, WhatsApp flow key failures, OCR Flight transport budgets, and gateway HTTP middleware".to_string()
        } else {
            format!(
                "prod surface hygiene has {} warning(s) across auth, WhatsApp flow, OCR Flight transport, and gateway HTTP middleware",
                warnings.len()
            )
        },
        confidence: if warnings.is_empty() { 0.96 } else { 0.7 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}
