//! OCR-Flight wiring doctor.
//!
//! On 2026-05-14 the production VM's `ocr-sidecar` (Rust gateway) was missing
//! `EXAMPLE_OCR_FLIGHT_URL` in its compose env. The `/api/parse` handler fell
//! back to a stale default (`host.docker.internal:18111`) and failed with
//! `Connection refused` on every inbound PDF. `example-api` had the env set;
//! the gateway service did not — classic per-service env drift.
//!
//! This doctor scans the workspace compose files and asserts that every
//! service whose container runs the Rust gateway binary (`one_file_gateway`)
//! or builds from `example-gateway/Dockerfile` has `EXAMPLE_OCR_FLIGHT_URL`
//! declared in its `environment:` block.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct OcrFlightWiringDoctor;

impl Doctor for OcrFlightWiringDoctor {
    fn name(&self) -> &'static str {
        "ocr-flight-wiring"
    }

    fn description(&self) -> &'static str {
        "Verifies every compose service running the Rust gateway has EXAMPLE_OCR_FLIGHT_URL set in its environment block."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_ocr_flight_wiring(root)
    }
}

const COMPOSE_FILES: &[&str] = &[
    "example-api/docker-compose.yml",
    "example-api/docker-compose.minimal.yml",
    "example-api/docker-compose.yml.optimized",
];

const REQUIRED_ENV: &str = "EXAMPLE_OCR_FLIGHT_URL";
const GATEWAY_BINARY: &str = "one_file_gateway";
const GATEWAY_DOCKERFILE: &str = "example-gateway/Dockerfile";
const HEALTH_AUDIT_OCR: &str = "cartridges/health_audit/ocr.py";
const HEALTH_AUDIT_ROUTER: &str = "cartridges/health_audit/router.py";
const FLIGHT_AUTH_RESOLVER: &str = "resolve_flight_authorization_header";
const FLIGHT_AUTH_REFRESHER: &str = "refresh_scanned_contract_flight_authorization";
const FLIGHT_AUTH_ENV: &str = "EXAMPLE_FLIGHT_AUTHORIZATION";

pub fn doctor_ocr_flight_wiring(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();
    let mut io = Vec::new();
    let mut checked = 0usize;
    let mut compose_violations: Vec<(String, String)> = Vec::new();
    let mut child_auth_violations: Vec<(String, String)> = Vec::new();

    for rel in COMPOSE_FILES {
        let path = root.join(rel);
        if !path.is_file() {
            continue;
        }
        let body = match read_text(&path, &mut io) {
            Some(b) => b,
            None => continue,
        };
        checked += 1;

        for service in extract_gateway_services(&body) {
            if !service.env_has(REQUIRED_ENV) {
                compose_violations.push((rel.to_string(), service.name.clone()));
                evidence.push(EvidenceItem {
                    kind: "ocr_flight_url_missing".to_string(),
                    path: rel.to_string(),
                    line: service.line,
                    detail: format!(
                        "compose service `{}` runs the Rust gateway but does not set {} in its environment block",
                        service.name, REQUIRED_ENV
                    ),
                });
            }
        }
    }

    inspect_health_audit_child_flight_auth(
        root,
        &mut warnings,
        &mut evidence,
        &mut io,
        &mut child_auth_violations,
    );

    warnings.extend(io);

    for (file, service) in &compose_violations {
        warnings.push(format!(
            "{}: service `{}` is missing {} — gateway /api/parse will fall back to a broken default",
            file, service, REQUIRED_ENV
        ));
    }

    entities.push(json!({
        "doctor": "ocr-flight-wiring",
        "compose_files_checked": checked,
        "required_env": REQUIRED_ENV,
        "compose_violations": compose_violations
            .iter()
            .map(|(f, s)| json!({"file": f, "service": s}))
            .collect::<Vec<_>>(),
        "child_auth_violations": child_auth_violations
            .iter()
            .map(|(f, contract)| json!({"file": f, "contract": contract}))
            .collect::<Vec<_>>(),
    }));

    let summary = if compose_violations.is_empty() && child_auth_violations.is_empty() {
        format!(
            "{} compose file(s) checked; every gateway service has {}",
            checked, REQUIRED_ENV
        )
    } else {
        let mut parts = Vec::new();
        if !compose_violations.is_empty() {
            parts.push(format!(
                "{} gateway compose service(s) missing {}",
                compose_violations.len(),
                REQUIRED_ENV
            ));
        }
        if !child_auth_violations.is_empty() {
            parts.push(format!(
                "{} Health Audit child Flight authorization contract violation(s)",
                child_auth_violations.len()
            ));
        }
        parts.join("; ")
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_ocr_flight_wiring"),
        kind: "doctor".to_string(),
        summary,
        confidence: if compose_violations.is_empty() && child_auth_violations.is_empty() {
            0.95
        } else {
            0.6
        },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn inspect_health_audit_child_flight_auth(
    root: &Path,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
    io: &mut Vec<String>,
    child_auth_violations: &mut Vec<(String, String)>,
) {
    let ocr_path = root.join(HEALTH_AUDIT_OCR);
    if ocr_path.is_file()
        && let Some(body) = read_text(&ocr_path, io)
    {
        let has_resolver = body.contains(&format!("{FLIGHT_AUTH_RESOLVER}()"));
        let exports_bearer = body.contains(FLIGHT_AUTH_ENV);
        let has_refresh_seam = body.contains(&format!("def {FLIGHT_AUTH_REFRESHER}("));
        if !has_resolver || !exports_bearer || !has_refresh_seam {
            child_auth_violations.push((
                HEALTH_AUDIT_OCR.to_string(),
                "scanned-contract child auth".to_string(),
            ));
            warnings.push(format!(
                    "{HEALTH_AUDIT_OCR}: scanned-contract child must resolve {FLIGHT_AUTH_RESOLVER}, export {FLIGHT_AUTH_ENV}, and expose {FLIGHT_AUTH_REFRESHER}"
                ));
            evidence.push(EvidenceItem {
                    kind: "scanned_contract_flight_authorization_missing".to_string(),
                    path: HEALTH_AUDIT_OCR.to_string(),
                    line: None,
                    detail: format!(
                        "scanned-contract child must mint the canonical Flight authorization via {FLIGHT_AUTH_RESOLVER} and pass it as {FLIGHT_AUTH_ENV}"
                    ),
                });
        }
    }

    let router_path = root.join(HEALTH_AUDIT_ROUTER);
    if router_path.is_file()
        && let Some(body) = read_text(&router_path, io)
    {
        let native_pdf_body = python_function_body(&body, "_native_pdf_contract_extractor_env");
        let clears_bearer = native_pdf_body.is_some_and(|function| {
            function.contains(&format!("env.pop(\"{FLIGHT_AUTH_ENV}\", None)"))
        });
        if native_pdf_body.is_some() && !clears_bearer {
            child_auth_violations.push((
                HEALTH_AUDIT_ROUTER.to_string(),
                "native PDF bearer cleanup".to_string(),
            ));
            warnings.push(format!(
                    "{HEALTH_AUDIT_ROUTER}: native PDF child disables Flight OCR and must remove {FLIGHT_AUTH_ENV}"
                ));
            evidence.push(EvidenceItem {
                kind: "native_pdf_flight_authorization_not_removed".to_string(),
                path: HEALTH_AUDIT_ROUTER.to_string(),
                line: None,
                detail: format!(
                    "native PDF child must remove {FLIGHT_AUTH_ENV} when Flight OCR is disabled"
                ),
            });
        }

        let contract_launch_body = python_function_body(&body, "_run_contract_extractor");
        if let Some(function) = contract_launch_body {
            let refreshes_per_launch =
                function.contains(&format!("{FLIGHT_AUTH_REFRESHER}(child_env)"));
            if !refreshes_per_launch {
                child_auth_violations.push((
                    HEALTH_AUDIT_ROUTER.to_string(),
                    "per-launch Flight authorization refresh".to_string(),
                ));
                warnings.push(format!(
                        "{HEALTH_AUDIT_ROUTER}: _run_contract_extractor must call {FLIGHT_AUTH_REFRESHER} for each child launch"
                    ));
                evidence.push(EvidenceItem {
                        kind: "scanned_contract_flight_authorization_not_refreshed_per_launch"
                            .to_string(),
                        path: HEALTH_AUDIT_ROUTER.to_string(),
                        line: None,
                        detail: format!(
                            "_run_contract_extractor must refresh the short-lived Flight authorization through {FLIGHT_AUTH_REFRESHER} for each launch"
                        ),
                    });
            }

            let strips_disabled_path_bearers = function.contains("if flight_disabled:")
                && function.contains(&format!("env.pop(\"{FLIGHT_AUTH_ENV}\", None)"))
                && function.contains(&format!("child_env.pop(\"{FLIGHT_AUTH_ENV}\", None)"));
            if !strips_disabled_path_bearers {
                child_auth_violations.push((
                    HEALTH_AUDIT_ROUTER.to_string(),
                    "disabled Flight final environment cleanup".to_string(),
                ));
                warnings.push(format!(
                        "{HEALTH_AUDIT_ROUTER}: _run_contract_extractor must remove {FLIGHT_AUTH_ENV} from the final child environment when Flight is disabled"
                    ));
                evidence.push(EvidenceItem {
                        kind: "disabled_flight_child_environment_authorization_not_removed"
                            .to_string(),
                        path: HEALTH_AUDIT_ROUTER.to_string(),
                        line: None,
                        detail: format!(
                            "_run_contract_extractor must remove ambient and extra {FLIGHT_AUTH_ENV} values before launching with Flight disabled"
                        ),
                    });
            }
        }
    }
}

fn python_function_body<'a>(source: &'a str, function: &str) -> Option<&'a str> {
    let marker = format!("def {function}(");
    let (_, after_header) = source.split_once(&marker)?;
    Some(after_header.split("\ndef ").next().unwrap_or(after_header))
}

struct ComposeService {
    name: String,
    line: Option<usize>,
    env_block: String,
}

impl ComposeService {
    fn env_has(&self, key: &str) -> bool {
        for line in self.env_block.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') {
                continue;
            }
            if trimmed.starts_with(&format!("{key}:")) || trimmed.starts_with(&format!("{key}=")) {
                return true;
            }
        }
        false
    }
}

/// Scan a docker-compose YAML for services whose container runs the Rust
/// gateway. Detection is structural and lenient: a service qualifies if any of
///   - its `command:` or `entrypoint:` references `one_file_gateway`
///   - its `build.dockerfile` references `example-gateway/Dockerfile`
///   - its name is exactly `ocr-sidecar`
///
/// Returns the `environment:` block as a raw string so the caller can check
/// for the presence of specific keys without taking a full YAML dependency.
fn extract_gateway_services(body: &str) -> Vec<ComposeService> {
    let lines: Vec<&str> = body.lines().collect();
    let mut services = Vec::new();

    let services_idx = match lines.iter().position(|l| l.trim_end() == "services:") {
        Some(i) => i,
        None => return services,
    };

    let mut i = services_idx + 1;
    while i < lines.len() {
        let line = lines[i];
        let indent = indent_of(line);

        // Top-level YAML key (zero indent, non-empty, non-comment) ends `services:`.
        if !line.trim().is_empty() && !line.trim_start().starts_with('#') && indent == 0 {
            break;
        }

        // Service header: exactly 2-space indent, ends with `:`, no further key after.
        if indent == 2 && line.trim_end().ends_with(':') && !line.trim_start().starts_with('-') {
            let name = line.trim().trim_end_matches(':').to_string();
            let service_start = i;

            // Walk the service body until we hit another 2-indent line or EOF.
            i += 1;
            let mut body_lines: Vec<&str> = Vec::new();
            while i < lines.len() {
                let l = lines[i];
                if !l.trim().is_empty() && indent_of(l) <= 2 {
                    break;
                }
                body_lines.push(l);
                i += 1;
            }

            let service_body = body_lines.join("\n");
            let qualifies = service_body.contains(GATEWAY_BINARY)
                || service_body.contains(GATEWAY_DOCKERFILE)
                || name == "ocr-sidecar";

            if qualifies {
                let env_block = extract_env_block(&body_lines);
                services.push(ComposeService {
                    name,
                    line: Some(service_start + 1),
                    env_block,
                });
            }
            continue;
        }
        i += 1;
    }

    services
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn extract_env_block(body_lines: &[&str]) -> String {
    let mut env_lines: Vec<&str> = Vec::new();
    let mut in_env = false;
    let mut env_indent: Option<usize> = None;

    for l in body_lines {
        let trimmed = l.trim_start();
        if !in_env {
            if trimmed == "environment:" || trimmed.starts_with("environment:") {
                in_env = true;
                env_indent = Some(indent_of(l));
            }
            continue;
        }
        let il = indent_of(l);
        if !l.trim().is_empty() && il <= env_indent.unwrap_or(0) {
            // Left the environment block.
            in_env = false;
            continue;
        }
        env_lines.push(l);
    }

    env_lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_repo(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-ocr-flight-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    const COMPOSE_WITH_ENV: &str = r#"services:
  api:
    image: example/api
    environment:
      EXAMPLE_OCR_FLIGHT_URL: grpc://api:8815
  ocr-sidecar:
    image: example/gateway
    command:
      - /app/one_file_gateway --no-tui
    environment:
      EXAMPLE_OCR_HTTP_BASE_URL: http://ocr-sidecar:9382
      EXAMPLE_OCR_FLIGHT_URL: grpc://api:8815
"#;

    const COMPOSE_WITHOUT_ENV: &str = r#"services:
  api:
    image: example/api
  ocr-sidecar:
    image: example/gateway
    command:
      - /app/one_file_gateway --no-tui
    environment:
      EXAMPLE_OCR_HTTP_BASE_URL: http://ocr-sidecar:9382
      GLINER_MODEL_DIR: /data/models/gliner
"#;

    #[test]
    fn wired_service_is_silent() {
        let root = temp_repo("ok");
        write(&root, "example-api/docker-compose.yml", COMPOSE_WITH_ENV);
        let env = doctor_ocr_flight_wiring(&root);
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_env_is_flagged() {
        let root = temp_repo("missing");
        write(&root, "example-api/docker-compose.yml", COMPOSE_WITHOUT_ENV);
        let env = doctor_ocr_flight_wiring(&root);
        assert_eq!(env.warnings.len(), 1, "{:?}", env.warnings);
        assert!(env.warnings[0].contains("ocr-sidecar"));
        assert!(env.warnings[0].contains("EXAMPLE_OCR_FLIGHT_URL"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_compose_files_skipped_silently() {
        let root = temp_repo("none");
        let env = doctor_ocr_flight_wiring(&root);
        assert!(env.warnings.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn flags_scanned_contract_child_without_canonical_flight_authorization() {
        let root = temp_repo("missing-scanned-auth");
        write(
            &root,
            "cartridges/health_audit/ocr.py",
            r#"
def build_scanned_contract_extractor_env(config):
    return {"EXAMPLE_EXTRACTOR_OCR_FLIGHT_URL": "grpc://ocr:9485"}
"#,
        );

        let env = doctor_ocr_flight_wiring(&root);

        assert!(
            env.warnings
                .iter()
                .any(|warning| warning.contains("resolve_flight_authorization_header")),
            "{:?}",
            env.warnings
        );
        assert!(
            !env.warnings
                .iter()
                .any(|warning| warning.contains("missing EXAMPLE_OCR_FLIGHT_URL")),
            "source drift must not be reported as a compose env violation: {:?}",
            env.warnings
        );
        assert!(
            env.summary
                .contains("Health Audit child Flight authorization"),
            "{:?}",
            env.summary
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn flags_native_pdf_child_that_keeps_flight_bearer() {
        let root = temp_repo("native-bearer-leak");
        write(
            &root,
            "cartridges/health_audit/router.py",
            r#"
def _native_pdf_contract_extractor_env():
    env = dict(_scanned_contract_extractor_env())
    env["CONTRACT_EXTRACTOR_DISABLE_FLIGHT_OCR"] = "1"
    return env
"#,
        );

        let env = doctor_ocr_flight_wiring(&root);

        assert!(
            env.warnings
                .iter()
                .any(|warning| warning.contains("EXAMPLE_FLIGHT_AUTHORIZATION")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn flags_contract_launch_without_fresh_auth_or_disabled_path_cleanup() {
        let root = temp_repo("launch-auth-drift");
        write(
            &root,
            "cartridges/health_audit/router.py",
            r#"
def _run_contract_extractor(upload_path, *, extra_env=None):
    env = os.environ.copy()
    if extra_env:
        child_env = dict(extra_env)
        flight_disabled = child_env.get("CONTRACT_EXTRACTOR_DISABLE_FLIGHT_OCR") == "1"
        if flight_disabled:
            pass
        env.update(child_env)
"#,
        );

        let env = doctor_ocr_flight_wiring(&root);

        assert!(
            env.warnings
                .iter()
                .any(|warning| warning.contains("refresh_scanned_contract_flight_authorization")),
            "{:?}",
            env.warnings
        );
        assert!(
            env.warnings
                .iter()
                .any(|warning| warning.contains("final child environment")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn accepts_scanned_and_native_children_with_isolated_flight_authorization() {
        let root = temp_repo("child-auth-wired");
        write(
            &root,
            "cartridges/health_audit/ocr.py",
            r#"
from example.flight.contracts import resolve_flight_authorization_header

def refresh_scanned_contract_flight_authorization(env):
    refreshed = dict(env)
    authorization = resolve_flight_authorization_header()
    if authorization:
        refreshed["EXAMPLE_FLIGHT_AUTHORIZATION"] = authorization
    return refreshed

def build_scanned_contract_extractor_env(config):
    env = {}
    return refresh_scanned_contract_flight_authorization(env)
"#,
        );
        write(
            &root,
            "cartridges/health_audit/router.py",
            r#"
def _native_pdf_contract_extractor_env():
    env = dict(_scanned_contract_extractor_env())
    env["CONTRACT_EXTRACTOR_DISABLE_FLIGHT_OCR"] = "1"
    env.pop("EXAMPLE_FLIGHT_AUTHORIZATION", None)
    return env

def _run_contract_extractor(upload_path, *, extra_env=None):
    env = os.environ.copy()
    if extra_env:
        child_env = dict(extra_env)
        flight_disabled = child_env.get("CONTRACT_EXTRACTOR_DISABLE_FLIGHT_OCR") == "1"
        env.pop("EXAMPLE_FLIGHT_AUTHORIZATION", None)
        if flight_disabled:
            child_env.pop("EXAMPLE_FLIGHT_AUTHORIZATION", None)
        else:
            child_env = refresh_scanned_contract_flight_authorization(child_env)
        env.update(child_env)
"#,
        );

        let env = doctor_ocr_flight_wiring(&root);

        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }
}
