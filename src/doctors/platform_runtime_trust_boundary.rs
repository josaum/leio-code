//! Durable contract for the authenticated, tenant-scoped platform runtime.
//!
//! This doctor deliberately inspects active, scoped syntax instead of doing
//! whole-file substring searches. Rust comments and `#[cfg(test)]` items are
//! removed before named structs/functions are inspected, so regression
//! fixtures and historical comments cannot satisfy (or violate) production
//! invariants.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use regex::Regex;
use serde_json::json;
use serde_yaml::{Mapping as YamlMapping, Value as YamlValue};

use super::Doctor;
use super::utils::query_id;
use crate::model::{QueryEnvelope, RepoIndex};

const DOCTOR_NAME: &str = "platform-runtime-trust-boundary";

const PLATFORM_AUTH: &str = "example-platform/example-server/src/auth.rs";
const ALIGN_AUTH: &str = "example-align/src/auth.rs";
const ALIGN_API: &str = "example-align/src/api.rs";
const PLATFORM_LIB: &str = "example-platform/example-server/src/lib.rs";
const PLATFORM_EVENTS: &str = "example-platform/example-server/src/events.rs";
const PLATFORM_ROUTER: &str = "example-platform/example-server/src/router.rs";
const PLATFORM_DB: &str = "example-platform/example-server/src/db.rs";
const PLATFORM_MAIN: &str = "example-platform/example-server/src/main.rs";
const ALIGN_JOBS: &str = "example-align/src/job_queue.rs";
const ALIGN_DOCKERFILE: &str = "example-align/Dockerfile";
const PLATFORM_DOCKERFILE: &str = "example-platform/Dockerfile.server";
const PYTHON_TASKS: &str = "example-api/example/agents/tasks.py";
const PYTHON_REDIS: &str = "example-api/example/core/redis_client.py";
const COMPOSE_FILES: &[&str] = &[
    "example-api/docker-compose.yml",
    "example-api/docker-compose.health-audit.yml",
];

const TENANT_STATE_HANDLERS: &[&str] = &[
    "get_ingest_artifact",
    "ingest_document",
    "get_induction_config",
    "put_induction_config",
    "run_induction",
    "get_induction_metrics",
    "get_induction_export",
    "post_induction_export",
    "get_induction_concepts",
    "get_induction_hierarchy",
    "get_induction_mermaid",
    "get_induction_gaps",
    "generate_induction_report",
    "import_ttl",
    "navigator_query",
    "get_navigator_node",
    "import_tbox_from_turtle",
    "list_tboxes",
    "get_tbox_graph",
    "ingest_db_schema",
    "merge_db_abox",
];

const PROXY_HANDLERS: &[&str] = &[
    "ingest_document",
    "proxy_align",
    "proxy_align_learned",
    "proxy_job_status",
    "proxy_explain",
    "proxy_tune",
    "proxy_debug",
    "proxy_ocr_health",
    "proxy_ocr_passthrough",
    "navigator_query",
];

const ALIGN_ORIGIN_HANDLERS: &[&str] = &[
    "proxy_align",
    "proxy_align_learned",
    "proxy_job_status",
    "proxy_explain",
    "proxy_tune",
    "proxy_debug",
    "navigator_query",
];

const OCR_ORIGIN_HANDLERS: &[&str] = &[
    "ingest_document",
    "proxy_ocr_health",
    "proxy_ocr_passthrough",
];

const PLATFORM_TASKS: &[(&str, &str, &str)] = &[
    (
        "run_induction_job",
        "job_id",
        "_PLATFORM_INDUCTION_PERMISSIONS",
    ),
    (
        "run_alignment_job",
        "job_id",
        "_PLATFORM_ALIGNMENT_PERMISSIONS",
    ),
    (
        "run_postman_tooling_induction_job",
        "server_job_id",
        "_PLATFORM_INDUCTION_PERMISSIONS",
    ),
    (
        "run_openapi_tooling_induction_job",
        "server_job_id",
        "_PLATFORM_INDUCTION_PERMISSIONS",
    ),
];

pub struct PlatformRuntimeTrustBoundaryDoctor;

impl Doctor for PlatformRuntimeTrustBoundaryDoctor {
    fn name(&self) -> &'static str {
        DOCTOR_NAME
    }

    fn description(&self) -> &'static str {
        "Checks fail-closed platform auth, tenant-scoped runtime/DB/event state, authenticated Python jobs, compose trust wiring, and request-local bearer forwarding."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_platform_runtime_trust_boundary(root)
    }
}

pub fn doctor_platform_runtime_trust_boundary(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();

    check_auth(root, &mut warnings);
    check_compose(root, &mut warnings);
    check_container_users(root, &mut warnings);
    check_align_routes(root, &mut warnings);
    check_tenant_state_and_handlers(root, &mut warnings);
    check_transactional_induction(root, &mut warnings);
    check_database(root, &mut warnings);
    check_python_jobs(root, &mut warnings);
    check_completion_events(root, &mut warnings);
    check_bearer_forwarding(root, &mut warnings);
    check_proxy_origins(root, &mut warnings);

    let summary = if warnings.is_empty() {
        format!("{DOCTOR_NAME}: platform runtime trust boundary is coherent")
    } else {
        format!(
            "{DOCTOR_NAME}: {} runtime trust-boundary warning(s)",
            warnings.len()
        )
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_platform_runtime_trust_boundary"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.97 } else { 0.78 },
        entities: vec![json!({
            "doctor": DOCTOR_NAME,
            "compose_services_checked": COMPOSE_FILES.len() * 2,
            "tenant_handlers_checked": TENANT_STATE_HANDLERS.len() + 2,
            "proxy_handlers_checked": PROXY_HANDLERS.len(),
            "python_tasks_checked": PLATFORM_TASKS.len(),
            "warning_count": warnings.len(),
        })],
        evidence: Vec::new(),
        warnings,
        meta: Some(json!({
            "scope": "active production code; comments and cfg(test) excluded",
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn warn(warnings: &mut Vec<String>, group: &str, detail: impl AsRef<str>) {
    warnings.push(format!("[{DOCTOR_NAME}][{group}] {}", detail.as_ref()));
}

fn read_required(
    root: &Path,
    rel: &str,
    group: &str,
    warnings: &mut Vec<String>,
) -> Option<String> {
    match std::fs::read_to_string(root.join(rel)) {
        Ok(source) => Some(source),
        Err(error) => {
            warn(
                warnings,
                group,
                format!("{rel}: cannot read contract surface: {error}"),
            );
            None
        }
    }
}

fn read_active_rust(
    root: &Path,
    rel: &str,
    group: &str,
    warnings: &mut Vec<String>,
) -> Option<String> {
    read_required(root, rel, group, warnings).map(|source| active_rust(&source))
}

fn check_auth(root: &Path, warnings: &mut Vec<String>) {
    for rel in [PLATFORM_AUTH, ALIGN_AUTH] {
        let Some(source) = read_active_rust(root, rel, "auth", warnings) else {
            continue;
        };
        let from_settings = rust_fn(&source, "from_settings").unwrap_or_default();
        let claims = rust_named_block(&source, "struct", "ClaimsWire").unwrap_or_default();
        let normalize_tenant = rust_fn(&source, "normalize_tenant").unwrap_or_default();
        let required_from_pem = rust_fn(&source, "required_from_pem").unwrap_or_default();
        let authenticate = rust_fn(&source, "authenticate").unwrap_or_default();
        let from_verified_claims = rust_fn(&source, "from_verified_claims").unwrap_or_default();
        let jwt_middleware = rust_fn(&source, "jwt_auth_middleware").unwrap_or_default();
        let compact_settings = compact(&from_settings);
        let compact_claims = compact(&claims);
        let compact_normalize = compact(&normalize_tenant);
        let compact_authenticate = compact(&authenticate);
        let required_body = rust_block_body(&required_from_pem)
            .map(compact)
            .unwrap_or_default();
        let authenticate_body = rust_block_body(&authenticate)
            .map(compact)
            .unwrap_or_default();
        let normalize_tenant_body = rust_block_body(&normalize_tenant)
            .map(compact)
            .unwrap_or_default();
        let from_verified_claims_body = rust_block_body(&from_verified_claims)
            .map(compact)
            .unwrap_or_default();
        let jwt_middleware_body = rust_block_body(&jwt_middleware)
            .map(compact)
            .unwrap_or_default();
        let exact_required_body = compact(
            r#"
            let decoding_key = DecodingKey::from_ec_pem(pem)
                .map_err(|source| AuthConfigError::InvalidPublicKey { source })?;
            Ok(Self { mode: AuthMode::Required(decoding_key), })
            "#,
        );
        let exact_authenticate_body = compact(
            r#"
            let AuthMode::Required(key) = &self.mode else {
                return Ok(AuthContext::local_development());
            };
            let token = extract_bearer_token(headers).ok_or(AuthError::MissingBearer)?;
            let mut validation = Validation::new(Algorithm::ES256);
            validation.leeway = 0;
            validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
            validation.set_issuer(&[EXPECTED_ISSUER]);
            validation.set_audience(&[EXPECTED_AUDIENCE]);
            let claims = decode::<ClaimsWire>(token, key, &validation)
                .map_err(AuthError::InvalidToken)?
                .claims;
            AuthContext::from_verified_claims(claims)
            "#,
        );
        let exact_normalize_tenant_body = compact(
            r#"
            let tenant_id = match (tenant_id, namespace) {
                (Some(canonical), Some(legacy)) if canonical == legacy => canonical,
                (Some(_), Some(_)) => return Err(AuthError::InvalidTenant),
                (Some(canonical), None) => canonical,
                (None, Some(legacy)) => legacy,
                (None, None) => return Err(AuthError::InvalidTenant),
            };
            let bytes = tenant_id.as_bytes();
            let valid = (1..=128).contains(&bytes.len())
                && bytes[0].is_ascii_alphanumeric()
                && bytes[1..]
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
            if valid { Ok(tenant_id) } else { Err(AuthError::InvalidTenant) }
            "#,
        );
        let exact_from_verified_claims_body = compact(
            r#"
            if claims.sub.is_empty() { return Err(AuthError::EmptySubject); }
            let tenant_id = normalize_tenant(claims.tenant_id, claims.namespace)?;
            let (role, roles) = normalize_roles(claims.role, claims.roles)?;
            Ok(Self {
                subject: claims.sub,
                tenant_id,
                email: claims.email,
                role,
                roles,
                permissions: claims.permissions,
                token_id: claims.jti.filter(|token_id| !token_id.is_empty()),
                source: AuthSource::VerifiedToken,
            })
            "#,
        );
        let exact_jwt_middleware_body = compact(
            r#"
            match auth.authenticate(request.headers()) {
                Ok(context) => {
                    request.extensions_mut().insert(context);
                    next.run(request).await
                }
                Err(error) => {
                    tracing::warn!(reason = error.reason(), "JWT authentication rejected");
                    (StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
                }
            }
            "#,
        );

        let fail_closed = compact_settings.contains("disabled==Some(\"true\")")
            && compact_settings.contains("key_path.ok_or(AuthConfigError::MissingPublicKeyPath)?")
            && compact_settings.contains("required_from_pem")
            && compact_authenticate.contains("AuthMode::Required")
            && compact_authenticate.contains("Validation::new(Algorithm::ES256)")
            && compact_authenticate.contains("set_issuer(&[EXPECTED_ISSUER])")
            && compact_authenticate.contains("set_audience(&[EXPECTED_AUDIENCE])")
            && compact_authenticate.contains("MissingBearer")
            && compact_settings.matches("disabled_for_development").count() == 1
            && compact_settings.matches("DisabledForDevelopment").count() == 0
            && identifier_count(&from_settings, "disabled") == 2
            && word_count(&from_settings, "if") == 1
            && word_count(&from_settings, "return") == 1
            && compact_settings.ends_with("Self::required_from_pem(&pem)}")
            && required_body == exact_required_body
            && authenticate_body == exact_authenticate_body
            && normalize_tenant_body == exact_normalize_tenant_body
            && from_verified_claims_body == exact_from_verified_claims_body
            && jwt_middleware_body == exact_jwt_middleware_body;
        if !fail_closed {
            warn(
                warnings,
                "auth",
                format!(
                    "{rel}: authentication must default to required ES256 mode and only exact `EXAMPLE_AUTH_DISABLED=true` may bypass it"
                ),
            );
        }

        let accepts_both_claims = compact_claims.contains("tenant_id:Option<String>")
            && compact_claims.contains("namespace:Option<String>")
            && compact_normalize
                .contains("(Some(canonical),Some(legacy))ifcanonical==legacy=>canonical")
            && compact_normalize.contains("(Some(_),Some(_))=>returnErr(AuthError::InvalidTenant)")
            && compact_normalize.contains("(Some(canonical),None)")
            && compact_normalize.contains("(None,Some(legacy))")
            && compact_normalize.contains("(None,None)=>returnErr(AuthError::InvalidTenant)")
            && !compact_normalize.contains("_=>Ok(");
        if !accepts_both_claims {
            warn(
                warnings,
                "auth",
                format!(
                    "{rel}: ClaimsWire/normalize_tenant must preserve canonical `tenant_id`, legacy `namespace`, equal-dual acceptance, and conflict rejection"
                ),
            );
        }
    }
}

fn check_compose(root: &Path, warnings: &mut Vec<String>) {
    for rel in COMPOSE_FILES {
        let Some(source) = read_required(root, rel, "compose", warnings) else {
            continue;
        };
        let Ok(mut document) = serde_yaml::from_str::<YamlValue>(&source) else {
            warn(warnings, "compose", format!("{rel}: invalid compose YAML"));
            continue;
        };
        if document.apply_merge().is_err() {
            warn(
                warnings,
                "compose",
                format!("{rel}: cannot resolve compose YAML merge keys"),
            );
            continue;
        }
        let Some(root_mapping) = document.as_mapping() else {
            warn(
                warnings,
                "compose",
                format!("{rel}: compose root is not a mapping"),
            );
            continue;
        };
        let Some(services) = yaml_mapping(root_mapping, "services") else {
            warn(
                warnings,
                "compose",
                format!("{rel}: missing top-level services mapping"),
            );
            continue;
        };
        check_artifact_root_compose(rel, services, warnings);
        for service in ["example-align", "example-server"] {
            let Some(service_block) = yaml_mapping(services, service) else {
                warn(
                    warnings,
                    "compose",
                    format!("{rel}: missing top-level service `{service}`"),
                );
                continue;
            };
            let environment = yaml_mapping(service_block, "environment");
            let volumes = yaml_sequence(service_block, "volumes");
            let redis_dependency = yaml_mapping(service_block, "depends_on")
                .and_then(|depends_on| yaml_mapping(depends_on, "redis"));

            let required_env = [
                ("EXAMPLE_JWT_PUBLIC_KEY_PATH", "/secrets/jwt/jwt_public.pem"),
                ("REDIS_SERVICE_HOST", "redis"),
                ("REDIS_SERVICE_PORT", "6379"),
                ("REDIS_PASSWORD", "${REDIS_PASSWORD:-}"),
            ];
            for (key, value) in required_env {
                if environment.is_none_or(|mapping| yaml_scalar(mapping, key) != Some(value)) {
                    warn(
                        warnings,
                        "compose",
                        format!(
                            "{rel} service `{service}` lacks exact environment `{key}: {value}`"
                        ),
                    );
                }
            }
            let keypair_dir = "${JWT_KEYPAIR_DIR:-../deploy/secrets}";
            let public_key_source = "${JWT_KEYPAIR_DIR:-../deploy/secrets}/jwt_public.pem";
            let exact_public_key_mount = "${JWT_KEYPAIR_DIR:-../deploy/secrets}/jwt_public.pem:/secrets/jwt/jwt_public.pem:ro";
            if !volumes.is_some_and(|items| {
                items
                    .iter()
                    .any(|value| value.as_str() == Some(exact_public_key_mount))
            }) {
                warn(
                    warnings,
                    "compose",
                    format!(
                        "{rel} service `{service}` must mount only jwt_public.pem read-only at /secrets/jwt/jwt_public.pem"
                    ),
                );
            }
            if volumes.is_some_and(|items| {
                items
                    .iter()
                    .any(|value| yaml_volume_target(value) == Some("/secrets/jwt"))
            }) {
                warn(
                    warnings,
                    "compose",
                    format!(
                        "{rel} service `{service}` must not mount the JWT keypair directory into the verifier container"
                    ),
                );
            }
            if volumes.is_some_and(|items| {
                items.iter().any(|value| {
                    yaml_volume_source(value).is_some_and(|source| {
                        source == keypair_dir
                            || (source.starts_with(&format!("{keypair_dir}/"))
                                && source != public_key_source)
                    })
                })
            }) {
                warn(
                    warnings,
                    "compose",
                    format!(
                        "{rel} service `{service}` must not expose the JWT keypair directory or non-public key material at any container target"
                    ),
                );
            }
            if redis_dependency
                .is_none_or(|mapping| yaml_scalar(mapping, "condition") != Some("service_healthy"))
            {
                warn(
                    warnings,
                    "compose",
                    format!("{rel} service `{service}` must depend on healthy Redis"),
                );
            }
            let bypass_present =
                environment.is_some_and(|mapping| {
                    mapping.contains_key(YamlValue::String("EXAMPLE_AUTH_DISABLED".to_string()))
                }) || yaml_sequence(service_block, "environment").is_some_and(|items| {
                    items.iter().filter_map(YamlValue::as_str).any(|item| {
                        item == "EXAMPLE_AUTH_DISABLED"
                            || item.starts_with("EXAMPLE_AUTH_DISABLED=")
                    })
                });
            if bypass_present {
                warn(
                    warnings,
                    "compose",
                    format!("{rel} service `{service}` enables the development auth bypass"),
                );
            }
        }
    }
}

fn check_artifact_root_compose(rel: &str, services: &YamlMapping, warnings: &mut Vec<String>) {
    let init_name = "example-server-data-init";
    let smoke_name = "example-server-data-write-smoke";
    let init = yaml_mapping(services, init_name);
    let smoke = yaml_mapping(services, smoke_name);
    let server = yaml_mapping(services, "example-server");

    let init_valid = init.is_some_and(|service| {
        yaml_scalar(service, "restart") == Some("no")
            && yaml_scalar(service, "user").is_some_and(yaml_user_is_root)
            && yaml_service_has_volume(service, "shared_data_volume", "/data")
            && yaml_command_text(service).is_some_and(|command| {
                command.contains("/data/artifacts")
                    && (shell_invokes(&command, "mkdir")
                        || (shell_invokes(&command, "install") && command.contains("-d")))
                    && command_sets_artifact_owner(&command)
            })
    });
    if !init_valid {
        warn(
            warnings,
            "compose-artifacts",
            format!(
                "{rel}: `{init_name}` must be a root one-shot that mounts shared_data_volume at /data and prepares /data/artifacts for uid 1000 gid 0"
            ),
        );
    }

    let smoke_valid = smoke.is_some_and(|service| {
        yaml_scalar(service, "restart") == Some("no")
            && yaml_scalar(service, "user") == Some("1000:0")
            && yaml_service_has_volume(service, "shared_data_volume", "/data")
            && yaml_dependency_condition(service, init_name)
                == Some("service_completed_successfully")
            && yaml_command_text(service).is_some_and(|command| {
                command.contains("/data/artifacts")
                    && shell_invokes(&command, "touch")
                    && shell_invokes(&command, "rm")
            })
    });
    if !smoke_valid {
        warn(
            warnings,
            "compose-artifacts",
            format!(
                "{rel}: `{smoke_name}` must run once as 1000:0 after `{init_name}` and touch/remove a probe below /data/artifacts"
            ),
        );
    }

    if server.and_then(|service| yaml_dependency_condition(service, smoke_name))
        != Some("service_completed_successfully")
    {
        warn(
            warnings,
            "compose-artifacts",
            format!(
                "{rel}: `example-server` must wait for `{smoke_name}` with service_completed_successfully"
            ),
        );
    }
    if server.is_none_or(|service| !yaml_service_has_volume(service, "shared_data_volume", "/data"))
    {
        warn(
            warnings,
            "compose-artifacts",
            format!(
                "{rel}: `example-server` must mount shared_data_volume at the runtime path /data"
            ),
        );
    }
    if server.is_none_or(|service| {
        !yaml_command_has_exact_argument_pair(service, "--artifacts-root", "/data/artifacts")
    }) {
        warn(
            warnings,
            "compose-artifacts",
            format!(
                "{rel}: `example-server` command must pass the single adjacent argument pair `--artifacts-root /data/artifacts`"
            ),
        );
    }
}

fn yaml_user_is_root(user: &str) -> bool {
    matches!(user.split(':').next(), Some("0" | "root"))
}

fn yaml_service_has_volume(service: &YamlMapping, source: &str, target: &str) -> bool {
    yaml_sequence(service, "volumes").is_some_and(|volumes| {
        volumes.iter().any(|volume| {
            yaml_volume_source(volume) == Some(source) && yaml_volume_target(volume) == Some(target)
        })
    })
}

fn yaml_dependency_condition<'a>(service: &'a YamlMapping, dependency: &str) -> Option<&'a str> {
    yaml_mapping(service, "depends_on")
        .and_then(|dependencies| yaml_mapping(dependencies, dependency))
        .and_then(|dependency| yaml_scalar(dependency, "condition"))
}

fn yaml_command_text(service: &YamlMapping) -> Option<String> {
    let command = service.get(YamlValue::String("command".to_string()))?;
    if let Some(command) = command.as_str() {
        return Some(command.to_string());
    }
    command.as_sequence().and_then(|parts| {
        parts
            .iter()
            .map(YamlValue::as_str)
            .collect::<Option<Vec<_>>>()
            .map(|parts| parts.join("\n"))
    })
}

fn yaml_command_has_exact_argument_pair(service: &YamlMapping, flag: &str, value: &str) -> bool {
    let command = service
        .get(YamlValue::String("command".to_string()))
        .and_then(YamlValue::as_sequence);
    let Some(arguments) = command.and_then(|items| {
        items
            .iter()
            .map(YamlValue::as_str)
            .collect::<Option<Vec<_>>>()
    }) else {
        return false;
    };
    if arguments
        .iter()
        .any(|argument| argument.starts_with(&format!("{flag}=")))
    {
        return false;
    }
    let positions = arguments
        .iter()
        .enumerate()
        .filter_map(|(index, argument)| (*argument == flag).then_some(index))
        .collect::<Vec<_>>();
    positions.len() == 1 && arguments.get(positions[0] + 1) == Some(&value)
}

fn shell_invokes(command: &str, program: &str) -> bool {
    Regex::new(&format!(
        r"(?m)(?:^|[;&|]\s*|\n\s*)(?:/[^\s;|&]+/)?{}\s+",
        regex::escape(program)
    ))
    .expect("shell program invocation regex")
    .is_match(command)
}

fn command_sets_artifact_owner(command: &str) -> bool {
    let chown = shell_invokes(command, "chown")
        && Regex::new(r"(?m)\bchown\b[^\n;&|]*\b1000:0\b[^\n;&|]*/data/artifacts\b")
            .expect("artifact chown regex")
            .is_match(command);
    let install = shell_invokes(command, "install")
        && command.contains("-o 1000")
        && command.contains("-g 0")
        && command.contains("/data/artifacts");
    chown || install
}

fn yaml_mapping<'a>(mapping: &'a YamlMapping, key: &str) -> Option<&'a YamlMapping> {
    mapping
        .get(YamlValue::String(key.to_string()))
        .and_then(YamlValue::as_mapping)
}

fn yaml_sequence<'a>(mapping: &'a YamlMapping, key: &str) -> Option<&'a Vec<YamlValue>> {
    mapping
        .get(YamlValue::String(key.to_string()))
        .and_then(YamlValue::as_sequence)
}

fn yaml_scalar<'a>(mapping: &'a YamlMapping, key: &str) -> Option<&'a str> {
    mapping
        .get(YamlValue::String(key.to_string()))
        .and_then(YamlValue::as_str)
}

fn yaml_volume_target(value: &YamlValue) -> Option<&str> {
    if let Some(short) = value.as_str() {
        let without_mode = short.strip_suffix(":ro").unwrap_or(short);
        return without_mode.rsplit_once(':').map(|(_, target)| target);
    }
    value
        .as_mapping()
        .and_then(|mapping| yaml_scalar(mapping, "target"))
}

fn yaml_volume_source(value: &YamlValue) -> Option<&str> {
    if let Some(short) = value.as_str() {
        let without_mode = short.strip_suffix(":ro").unwrap_or(short);
        return without_mode.rsplit_once(':').map(|(source, _)| source);
    }
    value
        .as_mapping()
        .and_then(|mapping| yaml_scalar(mapping, "source"))
}

fn check_container_users(root: &Path, warnings: &mut Vec<String>) {
    for rel in [ALIGN_DOCKERFILE, PLATFORM_DOCKERFILE] {
        let Some(source) = read_required(root, rel, "container-user", warnings) else {
            continue;
        };
        let user = dockerfile_final_user(&source);
        if user.is_none_or(|user| !docker_user_is_non_root(user)) {
            warn(
                warnings,
                "container-user",
                format!("{rel}: final runtime stage must declare a verifiably non-root USER"),
            );
        }
    }
}

fn dockerfile_final_user(source: &str) -> Option<&str> {
    let mut final_stage_seen = false;
    let mut user = None;
    for raw_line in source.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let instruction = parts.next()?.to_ascii_uppercase();
        if instruction == "FROM" {
            final_stage_seen = true;
            user = None;
        } else if final_stage_seen && instruction == "USER" {
            user = parts.next();
        }
    }
    user
}

fn docker_user_is_non_root(user: &str) -> bool {
    let user = user.split(':').next().unwrap_or(user).trim();
    !user.is_empty() && user != "0" && !user.eq_ignore_ascii_case("root") && !user.starts_with('$')
}

fn check_align_routes(root: &Path, warnings: &mut Vec<String>) {
    let Some(source) = read_active_rust(root, ALIGN_API, "align-routes", warnings) else {
        return;
    };
    let Some(builder) = rust_fn(&source, "build_router") else {
        warn(
            warnings,
            "align-routes",
            format!("{ALIGN_API}: missing build_router"),
        );
        return;
    };
    let returned = rust_block_body(&builder)
        .and_then(last_top_level_expression)
        .unwrap_or_default();
    let Some(public_start) = returned.find("Router::new()") else {
        warn(
            warnings,
            "align-routes",
            format!("{ALIGN_API}: build_router must return the structural public router directly"),
        );
        return;
    };
    let public = &returned[public_start..];
    let route_re =
        Regex::new(r#"(?s)\.route\s*\(\s*"([^"]+)"\s*,\s*(get|post)\s*\("#).expect("route regex");
    let routes = route_re
        .captures_iter(public)
        .map(|capture| format!("{} {}", capture[2].to_ascii_uppercase(), &capture[1]))
        .collect::<Vec<_>>();
    let expected = vec!["GET /health", "GET /ready", "POST /match"];
    let route_count = Regex::new(r"\.route\s*\(")
        .expect("route call regex")
        .find_iter(public)
        .count();
    let merge_re =
        Regex::new(r"\.merge\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)").expect("merge regex");
    let merges = merge_re
        .captures_iter(public)
        .map(|capture| capture[1].to_string())
        .collect::<Vec<_>>();
    let merge_count = Regex::new(r"\.merge\s*\(")
        .expect("merge call regex")
        .find_iter(public)
        .count();
    let forbidden_composition =
        Regex::new(r"\.(?:nest|nest_service|route_service|fallback|fallback_service)\s*\(")
            .expect("public composition regex");
    if routes != expected
        || route_count != expected.len()
        || merge_count != 1
        || merges != ["protected"]
        || forbidden_composition.is_match(public)
    {
        warn(
            warnings,
            "align-routes",
            format!("{ALIGN_API}: public routes must be exactly {expected:?}; found {routes:?}"),
        );
    }
    let protected = let_binding_statement(&builder, "protected")
        .map(|statement| compact(statement).replace(",)", ")"))
        .unwrap_or_default();
    let exact_middleware = ".layer(axum::middleware::from_fn_with_state(auth.clone(),crate::auth::jwt_auth_middleware))";
    if !protected.ends_with(exact_middleware)
        || protected.matches("jwt_auth_middleware").count() != 1
        || let_binding_count(&builder, "protected") != 1
    {
        warn(
            warnings,
            "align-routes",
            format!("{ALIGN_API}: protected router must own stateful JWT middleware"),
        );
    }

    let match_handler = rust_fn(&source, "match_handler").unwrap_or_default();
    let compact_match = compact(&match_handler);
    let multipart_gate = compact_match.contains(".starts_with(\"multipart/\")")
        || compact_match.contains(".contains(\"multipart/form-data\")");
    let forbidden_dereference = [
        "application/x-www-form-urlencoded",
        "reqwest::get(",
        "tokio::fs::read(",
        "std::fs::read(",
        "file://",
    ]
    .iter()
    .any(|needle| source.contains(needle));
    if match_handler.is_empty()
        || !multipart_gate
        || !compact_match.contains("Multipart::from_request(req,&state).await")
        || !compact_match.contains("match_multipart(state,multipart).await")
        || !compact_match.contains("StatusCode::UNSUPPORTED_MEDIA_TYPE")
        || compact_match.matches("Multipart::from_request(").count() != 1
        || compact_match.matches("match_multipart(").count() != 1
        || word_count(&match_handler, "if") != 1
        || forbidden_dereference
    {
        warn(
            warnings,
            "align-match",
            format!(
                "{ALIGN_API}: public /match must accept multipart uploads only and must not dereference URLs or filesystem paths"
            ),
        );
    }
}

fn check_tenant_state_and_handlers(root: &Path, warnings: &mut Vec<String>) {
    let Some(lib) = read_active_rust(root, PLATFORM_LIB, "tenant-state", warnings) else {
        return;
    };
    let state = rust_named_block(&lib, "struct", "ServerState").unwrap_or_default();
    let compact_state = compact(&state);
    if !compact_state.contains("tenant_states:Arc<crate::tenant_state::TenantStateRegistry>") {
        warn(
            warnings,
            "tenant-state",
            format!("{PLATFORM_LIB}: ServerState must own TenantStateRegistry"),
        );
    }
    let global_field =
        Regex::new(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?(induction|induction_jobs|tbox_manager)\s*:")
            .expect("global state field regex");
    for capture in global_field.captures_iter(&state) {
        warn(
            warnings,
            "tenant-state",
            format!(
                "{PLATFORM_LIB}: ServerState must not own global `{}` mutable tenant state",
                &capture[1]
            ),
        );
    }

    let run = rust_fn(&lib, "run").unwrap_or_default();
    let protected = let_binding_statement(&run, "protected")
        .map(|statement| compact(statement).replace(",)", ")"))
        .unwrap_or_default();
    let exact_middleware = ".layer(axum::middleware::from_fn_with_state(self.auth.clone(),auth::jwt_auth_middleware)).with_state(state.clone())";
    if !protected.contains(exact_middleware)
        || protected.matches("jwt_auth_middleware").count() != 1
        || let_binding_count(&run, "protected") != 1
        || compact(&run).matches(".merge(protected)").count() != 1
    {
        warn(
            warnings,
            "handler-auth",
            format!(
                "{PLATFORM_LIB}: the protected router must attach JWT middleware before state and be merged exactly once"
            ),
        );
    }

    for handler in TENANT_STATE_HANDLERS {
        check_typed_handler(PLATFORM_LIB, &lib, handler, warnings);
        check_tenant_ownership_flow(PLATFORM_LIB, &lib, handler, warnings);
    }

    let Some(events) = read_active_rust(root, PLATFORM_EVENTS, "handler-auth", warnings) else {
        return;
    };
    for handler in ["post_induction_job", "get_induction_job"] {
        check_typed_handler(PLATFORM_EVENTS, &events, handler, warnings);
        check_event_tenant_ownership_flow(&events, handler, warnings);
    }

    let artifact_layout = rust_fn(&lib, "tenant_request_dir").unwrap_or_default();
    let compact_layout = compact(&artifact_layout);
    if !compact_layout.contains("validate_path_component(tenant_id)")
        || !compact_layout.contains("validate_path_component(request_id)")
        || !compact_layout.contains(".join(\"tenants\").join(tenant_id).join(request_id)")
    {
        warn(
            warnings,
            "artifacts",
            format!(
                "{PLATFORM_LIB}: artifact layout must be validated and rooted at tenants/{{tenant_id}}/{{request_id}}"
            ),
        );
    }
    let ingest = rust_fn(&lib, "ingest_document").unwrap_or_default();
    let retrieval = rust_fn(&lib, "get_ingest_artifact").unwrap_or_default();
    if !compact(&ingest)
        .contains("open_tenant_request_dir(root,&auth_context.tenant_id,&request_id,true)")
        || !compact(&retrieval)
            .contains("open_tenant_artifact(root,&auth_context.tenant_id,&id,filename)")
    {
        warn(
            warnings,
            "artifacts",
            format!(
                "{PLATFORM_LIB}: ingest and retrieval must use the tenant-scoped no-follow artifact open helpers"
            ),
        );
    }

    let (_, routed) = protected_router_handlers(&lib);
    for routed_handler in &routed {
        let Some((rel, handler_source, name)) =
            routed_handler_surface(root, &lib, &events, routed_handler)
        else {
            warn(
                warnings,
                "handler-auth",
                format!(
                    "{PLATFORM_LIB}: cannot resolve routed handler `{routed_handler}` to active Rust source"
                ),
            );
            continue;
        };
        if ["well_known_guide", "discovery_state"].contains(&name.as_str()) {
            continue;
        }
        let function = rust_fn(&handler_source, &name).unwrap_or_default();
        let compact_signature = compact(function.split('{').next().unwrap_or(&function));
        if compact_signature.contains("State<Arc<ServerState>>")
            || compact_signature.contains("State<Arc<crate::ServerState>>")
        {
            check_typed_handler(&rel, &handler_source, &name, warnings);
            let compact_function = compact(&function);
            if !routed_handler.contains("events::")
                && (compact_function.contains("tenant_runtime_state(")
                    || compact_function.contains("tenant_states.get_or_create("))
            {
                check_tenant_ownership_flow(&rel, &handler_source, &name, warnings);
            }
        }
        if function.contains("flight_ingest") {
            warn(
                warnings,
                "flight-boundary",
                format!("{rel}: routed handler `{routed_handler}` reads Flight ingest state"),
            );
        }
    }
    let flight_functions = rust_function_blocks(&lib)
        .into_iter()
        .chain(rust_function_blocks(&events))
        .filter(|(_, function)| function.contains("flight_ingest"))
        .collect::<Vec<_>>();
    let valid_flight_bootstrap = flight_functions.len() == 1
        && flight_functions[0].0 == "run"
        && flight_functions[0].1.matches("flight_ingest").count() == 4
        && compact(&flight_functions[0].1)
            .contains("flight_ingest:Arc::new(crate::flight_ingest::FlightIngestState")
        && compact(&flight_functions[0].1).contains("state.flight_ingest.clone()")
        && compact(&flight_functions[0].1)
            .contains("crate::flight_ingest::flight_service(flight_state)");
    if !valid_flight_bootstrap {
        let locations = flight_functions
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>();
        warn(
            warnings,
            "flight-boundary",
            format!(
                "{PLATFORM_LIB}: Flight state may only appear in the one internal data-plane bootstrap; found active functions {locations:?}"
            ),
        );
    }
}

fn check_transactional_induction(root: &Path, warnings: &mut Vec<String>) {
    let Some(lib) = read_active_rust(root, PLATFORM_LIB, "transactional-state", warnings) else {
        return;
    };
    for handler in ["run_induction", "import_ttl"] {
        let Some(function) = rust_fn(&lib, handler) else {
            warn(
                warnings,
                "transactional-state",
                format!("{PLATFORM_LIB}: missing transactional handler `{handler}`"),
            );
            continue;
        };
        if !transactional_induction_is_safe(handler, &function) {
            warn(
                warnings,
                "transactional-state",
                format!(
                    "{PLATFORM_LIB}: `{handler}` must parse and close over candidate state before committing tenant induction fields"
                ),
            );
        }
    }
}

fn transactional_induction_is_safe(handler: &str, function: &str) -> bool {
    let compact_function = compact(function);
    let Some(parse_at) = compact_function.find("parse_rdf_triples_as_core_facts(") else {
        return false;
    };
    let state_at = [
        compact_function.find("tenant_runtime_state(&state,&auth_context).await"),
        compact_function.find("state.tenant_states.get_or_create(&auth_context.tenant_id).await"),
    ]
    .into_iter()
    .flatten()
    .min();
    let Some(state_at) = state_at else {
        return false;
    };
    let Some(closure_at) = compact_function.find("run_core_join_closure_with_timeout(") else {
        return false;
    };
    let Some(export_at) = compact_function.find("core.export_owl2(") else {
        return false;
    };
    let assignment_re = Regex::new(r"\bst\.[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*=")
        .expect("tenant commit assignment regex");
    let assignments = assignment_re
        .find_iter(&compact_function)
        .map(|matched| matched.start())
        .collect::<Vec<_>>();
    let Some(first_commit) = assignments.first().copied() else {
        return false;
    };
    if !(parse_at < state_at
        && state_at < closure_at
        && closure_at < export_at
        && export_at < first_commit)
    {
        return false;
    }
    if !call_is_propagated(function, "parse_rdf_triples_as_core_facts", false)
        || !call_is_propagated(function, "run_core_join_closure_with_timeout", true)
        || !compact_function.contains("candidate_config=st.config.clone()")
        || !compact_function.contains("candidate_core=")
        || !compact_function.contains("ExampleCore::with_config(candidate_config.clone())")
        || !compact_function.contains("candidate_core.ingest(")
        || !compact_function.contains("std::mem::take(&mutcandidate_core.triples)")
        || compact_function[..first_commit].contains("st.core=")
    {
        return false;
    }
    let closure_calls = named_call_sites(function, "run_core_join_closure_with_timeout");
    if closure_calls.len() != 1
        || closure_calls[0].2.len() < 2
        || !compact(&closure_calls[0].2[0]).starts_with("candidate_config")
        || compact(&closure_calls[0].2[1]) != "facts"
    {
        return false;
    }

    let required_commits: &[&str] = if handler == "import_ttl" {
        &["st.config=", "st.owl2_config=", "st.core="]
    } else {
        &[
            "st.core=",
            "st.last_result=",
            "st.last_metrics=",
            "st.last_owl2=",
            "st.last_class_names=",
        ]
    };
    if required_commits.iter().any(|assignment| {
        compact_function
            .find(assignment)
            .is_none_or(|position| position <= export_at)
    }) {
        return false;
    }
    if handler == "import_ttl"
        && (!compact_function.contains("candidate_owl2_config=st.owl2_config.clone()")
            || !compact_function.contains("core.export_owl2(&candidate_owl2_config)"))
    {
        return false;
    }
    true
}

fn call_is_propagated(source: &str, name: &str, require_await: bool) -> bool {
    let calls = named_call_sites(source, name);
    calls.len() == 1
        && call_statement_tail(source, calls[0].1).is_some_and(|tail| {
            let tail = compact(tail);
            (!require_await || tail.contains(".await")) && tail.ends_with("?;")
        })
}

fn call_statement_tail(source: &str, call_close: usize) -> Option<&str> {
    let tail = source.get(call_close + 1..)?;
    let mut parentheses = 0usize;
    let mut brackets = 0usize;
    let mut braces = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, character) in tail.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '(' => parentheses += 1,
            ')' => parentheses = parentheses.saturating_sub(1),
            '[' => brackets += 1,
            ']' => brackets = brackets.saturating_sub(1),
            '{' => braces += 1,
            '}' => braces = braces.saturating_sub(1),
            ';' if parentheses == 0 && brackets == 0 && braces == 0 => {
                return tail.get(..=offset);
            }
            _ => {}
        }
    }
    None
}

fn routed_handler_surface(
    root: &Path,
    lib: &str,
    events: &str,
    routed_handler: &str,
) -> Option<(String, String, String)> {
    let trimmed = routed_handler
        .strip_prefix("crate::")
        .or_else(|| routed_handler.strip_prefix("self::"))
        .unwrap_or(routed_handler);
    let segments = trimmed.split("::").collect::<Vec<_>>();
    let name = (*segments.last()?).to_string();
    if segments.len() == 1 {
        return Some((PLATFORM_LIB.to_string(), lib.to_string(), name));
    }
    if segments[..segments.len() - 1] == ["events"] {
        return Some((PLATFORM_EVENTS.to_string(), events.to_string(), name));
    }

    let module = segments[..segments.len() - 1].join("/");
    let base = root.join("example-platform/example-server/src");
    for path in [
        base.join(format!("{module}.rs")),
        base.join(&module).join("mod.rs"),
    ] {
        if let Ok(source) = std::fs::read_to_string(&path) {
            let rel = path.strip_prefix(root).ok()?.to_string_lossy().to_string();
            return Some((rel, active_rust(&source), name));
        }
    }
    None
}

fn protected_router_handlers(source: &str) -> (String, Vec<String>) {
    let Some(run) = rust_fn(source, "run") else {
        return (String::new(), Vec::new());
    };
    (run, routed_handlers(source))
}

fn routed_handlers(source: &str) -> Vec<String> {
    let route_re = Regex::new(r"\.route\s*\(").expect("route binding regex");
    let handler_re = Regex::new(
        r"(?:axum::routing::)?(?:get|post|put|delete|patch|head|options|trace|any)\s*\(\s*([A-Za-z_][A-Za-z0-9_:]*)",
    )
    .expect("routed handler regex");
    route_re
        .find_iter(source)
        .filter_map(|route| {
            let open = route.end().checked_sub(1)?;
            let close = matching_parenthesis(source, open)?;
            Some(&source[open + 1..close])
        })
        .flat_map(|arguments| {
            handler_re
                .captures_iter(arguments)
                .map(|capture| capture[1].to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn check_typed_handler(rel: &str, source: &str, name: &str, warnings: &mut Vec<String>) {
    let Some(function) = rust_fn(source, name) else {
        warn(
            warnings,
            "handler-auth",
            format!("{rel}: missing tenant-state handler `{name}`"),
        );
        return;
    };
    let signature = function.split('{').next().unwrap_or(&function);
    let typed_auth = Regex::new(
        r"Extension\s*\([^)]*\)\s*:\s*(?:axum::extract::)?Extension\s*<\s*(?:crate::)?auth::AuthContext\s*>",
    )
    .expect("typed auth regex");
    if !typed_auth.is_match(signature) {
        warn(
            warnings,
            "handler-auth",
            format!("{rel}: stateful handler `{name}` must extract typed AuthContext"),
        );
    }
}

fn check_tenant_ownership_flow(rel: &str, source: &str, name: &str, warnings: &mut Vec<String>) {
    if ["ingest_db_schema", "merge_db_abox"].contains(&name) {
        return;
    }
    let function = rust_fn(source, name).unwrap_or_default();
    let body = compact(&function);
    let auth_scoped = if name == "get_ingest_artifact" {
        body.matches("open_tenant_artifact(").count() == 1
            && body.contains("open_tenant_artifact(root,&auth_context.tenant_id,&id,filename)")
    } else {
        let runtime_calls = body.matches("tenant_runtime_state(").count();
        let registry_calls = body.matches("tenant_states.get_or_create(").count();
        (runtime_calls == 1
            && registry_calls == 0
            && body.contains("tenant_runtime_state(&state,&auth_context).await"))
            || (runtime_calls == 0
                && registry_calls == 1
                && body
                    .contains("state.tenant_states.get_or_create(&auth_context.tenant_id).await"))
    };
    let request_tenant = body.contains("request.tenant_id") || body.contains("req.tenant_id");
    if (!auth_scoped || request_tenant) && !function.contains("flight_ingest") {
        let group = if name == "get_ingest_artifact" {
            "artifacts"
        } else {
            "handler-auth"
        };
        warn(
            warnings,
            group,
            format!(
                "{rel}: stateful handler `{name}` must derive tenant-owned state/paths from its extracted AuthContext"
            ),
        );
    }
}

fn check_event_tenant_ownership_flow(source: &str, name: &str, warnings: &mut Vec<String>) {
    let function = compact(&rust_fn(source, name).unwrap_or_default());
    let auth_scoped = function.matches("tenant_states.get_or_create(").count() == 1
        && (function.contains("state.tenant_states.get_or_create(&auth.tenant_id).await")
            || (function.contains("lettenant_id=auth.tenant_id.clone()")
                && function.contains("state.tenant_states.get_or_create(&tenant_id).await")))
        && !function.contains("request.tenant_id")
        && !function.contains("req.tenant_id");
    if !auth_scoped {
        warn(
            warnings,
            "handler-auth",
            format!(
                "{PLATFORM_EVENTS}: stateful handler `{name}` must key tenant state from extracted AuthContext"
            ),
        );
    }
}

fn check_database(root: &Path, warnings: &mut Vec<String>) {
    let Some(db) = read_active_rust(root, PLATFORM_DB, "database", warnings) else {
        return;
    };
    let source_request = rust_named_block(&db, "struct", "DbSourceRequest").unwrap_or_default();
    let redacted = rust_named_block(&db, "struct", "RedactedDbSource").unwrap_or_default();
    let schema_result = rust_named_block(&db, "struct", "SchemaIngestResult").unwrap_or_default();
    let merge_options = rust_named_block(&db, "struct", "MergeOptions").unwrap_or_default();
    let merge_result = rust_named_block(&db, "struct", "MergeResult").unwrap_or_default();
    let policy = rust_named_block(&db, "struct", "AdbcPolicy").unwrap_or_default();
    let policy_settings = rust_fn(&db, "from_settings").unwrap_or_default();
    let policy_resolve = rust_fn(&db, "resolve").unwrap_or_default();
    let format_validation = rust_fn(&db, "validate").unwrap_or_default();
    let driver_token = rust_fn(&db, "valid_driver_token").unwrap_or_default();
    let output_context = rust_fn(&db, "output_context").unwrap_or_default();
    let output_stem = rust_fn(&db, "table_output_stem").unwrap_or_default();
    let sanitize_error = rust_fn(&db, "sanitize_error").unwrap_or_default();
    let merge_blocking = rust_fn(&db, "merge_abox_blocking").unwrap_or_default();

    if !serde_deny_unknown(&db, "DbSourceRequest") || !serde_deny_unknown(&db, "MergeOptions") {
        warn(
            warnings,
            "database",
            format!(
                "{PLATFORM_DB}: DbSourceRequest and MergeOptions must deny unknown caller fields"
            ),
        );
    }
    for (label, block, forbidden) in [
        (
            "DbSourceRequest",
            source_request.as_str(),
            &["entrypoint", "driver_search_paths", "search_paths"][..],
        ),
        (
            "MergeOptions",
            merge_options.as_str(),
            &["output_dir", "output_path"][..],
        ),
        (
            "RedactedDbSource",
            redacted.as_str(),
            &["dsn", "username", "password", "entrypoint", "search_paths"][..],
        ),
        (
            "MergeResult",
            merge_result.as_str(),
            &["output_dir", "absolute_path", "paths"][..],
        ),
    ] {
        for field in forbidden {
            if rust_field(block, field) {
                warn(
                    warnings,
                    "database",
                    format!("{PLATFORM_DB}: {label} exposes forbidden field `{field}`"),
                );
            }
        }
    }
    if !compact(&schema_result).contains("source:RedactedDbSource") {
        warn(
            warnings,
            "database",
            format!("{PLATFORM_DB}: SchemaIngestResult must expose only RedactedDbSource"),
        );
    }
    let compact_policy = compact(&policy);
    if !compact_policy.contains("allowed_drivers:") || !compact_policy.contains("artifacts_root:") {
        warn(
            warnings,
            "database",
            format!(
                "{PLATFORM_DB}: AdbcPolicy must own the logical-driver allowlist and artifact root"
            ),
        );
    }
    let compact_settings = compact(&policy_settings);
    let compact_resolve = compact(&policy_resolve);
    let resolve_body = rust_block_body(&policy_resolve)
        .map(compact)
        .unwrap_or_default();
    let exact_resolve_body = compact(
        r#"
        if self.allowed_drivers.is_empty() {
            return Err(DbPolicyError::Disabled);
        }
        let dsn = source.dsn.trim();
        if dsn.is_empty()
            || !valid_driver_token(&source.driver)
            || !self.allowed_drivers.contains(&source.driver)
        {
            return Err(DbPolicyError::InvalidRequest);
        }
        let mut request = source.clone();
        request.dsn = dsn.to_string();
        Ok(ResolvedDbSource {
            redacted: RedactedDbSource::from(source),
            request,
        })
        "#,
    );
    if !compact_settings.contains("raw.is_empty()")
        || !compact_settings.contains("BTreeSet::new()")
        || !compact_settings.contains("artifacts_root")
        || !compact_settings.contains("InvalidConfiguration")
        || !compact_resolve.contains("allowed_drivers.is_empty()")
        || !compact_resolve.contains("DbPolicyError::Disabled")
        || !compact_resolve.contains("allowed_drivers.contains(&source.driver)")
        || word_count(&policy_resolve, "if") != 2
        || word_count(&policy_resolve, "return") != 2
        || compact_resolve.matches("Err(").count() != 2
        || compact_resolve.matches("Ok(").count() != 1
        || resolve_body != exact_resolve_body
    {
        warn(
            warnings,
            "database",
            format!(
                "{PLATFORM_DB}: AdbcPolicy must disable empty allowlists, reject unknown drivers, and require configured output for enabled ingestion"
            ),
        );
    }
    if !compact(&driver_token).contains("is_ascii_alphanumeric")
        || !compact(&driver_token).contains("(1..=128).contains")
        || !compact(&output_context).contains("validate_path_component(tenant_id)")
        || !compact(&output_context).contains("validate_path_component(operation_id)")
    {
        warn(
            warnings,
            "database",
            format!(
                "{PLATFORM_DB}: driver, tenant, and operation identifiers must be validated as single safe components"
            ),
        );
    }
    let format_literals = Regex::new(r#""([^"]+)""#)
        .expect("format literal regex")
        .captures_iter(&format_validation)
        .map(|capture| capture[1].to_string())
        .collect::<Vec<_>>();
    if !format_literals.iter().any(|value| value == "parquet")
        || !format_literals.iter().any(|value| value == "arrow_ipc")
        || format_literals
            .iter()
            .any(|value| value != "parquet" && value != "arrow_ipc")
        || !compact(&format_validation).contains("DbPolicyError::InvalidRequest")
    {
        warn(
            warnings,
            "database",
            format!(
                "{PLATFORM_DB}: merge formats must be validated to exactly parquet and arrow_ipc before output"
            ),
        );
    }

    let safe_flags = rust_fn(&db, "safe_adbc_load_flags").unwrap_or_default();
    let compact_flags = compact(&safe_flags);
    let load_function = rust_fn(&db, "ingest_schema_blocking").unwrap_or_default();
    let compact_load = compact(&load_function);
    let exact_safe_flags =
        "adbc_core::LOAD_FLAG_DEFAULT&!adbc_core::LOAD_FLAG_ALLOW_RELATIVE_PATHS";
    if rust_block_body(&safe_flags).map(compact).as_deref() != Some(exact_safe_flags)
        || !compact_flags.contains(exact_safe_flags)
        || !compact_load.contains("safe_adbc_load_flags()")
        || !compact_load.contains("request.driver.clone()")
        || compact_load.matches("None").count() < 2
    {
        warn(
            warnings,
            "database",
            format!(
                "{PLATFORM_DB}: executable ADBC loading must clear ALLOW_RELATIVE_PATHS and pass no entrypoint/search paths"
            ),
        );
    }

    let layout_re =
        Regex::new(r"(?s)open_tenant_db_operation_dir\s*\([^)]*tenant_id[^)]*operation_id[^)]*\)")
            .expect("DB layout regex");
    if !layout_re.is_match(&merge_blocking)
        || !compact(&merge_blocking).contains("formats:&ValidatedMergeFormats")
        || !compact(&merge_blocking).contains("table_output_stem(table)")
    {
        warn(
            warnings,
            "database",
            format!(
                "{PLATFORM_DB}: DB output must be rooted under the authenticated tenant operation directory"
            ),
        );
    }
    let compact_stem = compact(&output_stem);
    if !compact_stem.contains("String::with_capacity(64)")
        || !compact_stem.contains("to_ascii_lowercase")
        || !compact_stem.contains("slug.push_str(\"table\")")
        || !compact_stem.contains("table.catalog.as_deref()")
        || !compact_stem.contains("table.schema.as_deref()")
        || !compact_stem.contains("table.name.as_str()")
        || !compact_stem.contains("Sha256::digest")
        || !compact_stem.contains("digest[..8]")
        || !compact_stem.contains("format!(\"{slug}-{suffix}\")")
    {
        warn(
            warnings,
            "database",
            format!(
                "{PLATFORM_DB}: DB filenames must use the bounded lowercase slug plus 8-byte metadata hash"
            ),
        );
    }
    let compact_sanitize = compact(&sanitize_error);
    let exact_sanitize_body = compact(
        r#"
        match error {
            DbExecutorError::Failed(_) => "database driver operation failed".to_string(),
            DbExecutorError::ArtifactsUnavailable(_) => {
                "database artifact operation unavailable".to_string()
            }
        }
        "#,
    );
    if rust_block_body(&sanitize_error).map(compact).as_deref()
        != Some(exact_sanitize_body.as_str())
        || compact_sanitize.contains("error.to_string()")
    {
        warn(
            warnings,
            "database",
            format!(
                "{PLATFORM_DB}: executor logs must expose stable error classes and never driver-controlled text"
            ),
        );
    }

    let Some(lib) = read_active_rust(root, PLATFORM_LIB, "database", warnings) else {
        return;
    };
    let authorize = rust_fn(&lib, "authorize_db_operation").unwrap_or_default();
    let policy_errors = rust_fn(&lib, "map_db_policy_error").unwrap_or_default();
    let ingest = rust_fn(&lib, "ingest_db_schema").unwrap_or_default();
    let merge = rust_fn(&lib, "merge_db_abox").unwrap_or_default();
    let db_layout = rust_fn(&lib, "tenant_db_operation_dir").unwrap_or_default();
    let authorize_body = rust_block_body(&authorize).map(compact).unwrap_or_default();
    let exact_authorize = "[\"admin\",\"super_admin\",\"superadmin\"].iter().any(|role|auth_context.has_role(role))||(auth_context.has_role(\"service\")&&auth_context.has_permission(permission))";
    if authorize_body != exact_authorize
        || !compact(&ingest).contains("platform:db:ingest")
        || !compact(&merge).contains("platform:db:merge")
        || !compact(&merge).contains("output_context(&auth_context.tenant_id,&operation_id)")
    {
        warn(
            warnings,
            "database",
            format!(
                "{PLATFORM_LIB}: DB handlers must enforce exact admin/service permissions and tenant-owned output"
            ),
        );
    }
    let compact_layout = compact(&db_layout);
    let compact_ingest = compact(&ingest);
    let compact_merge = compact(&merge);
    let ingest_body = rust_block_body(&ingest).map(compact).unwrap_or_default();
    let merge_body = rust_block_body(&merge).map(compact).unwrap_or_default();
    let ingest_guard = compact(
        r#"
        if !authorize_db_operation(&auth_context, "platform:db:ingest") {
            return Err(db_error(axum::http::StatusCode::FORBIDDEN, "Forbidden"));
        }
        "#,
    );
    let merge_guard = compact(
        r#"
        if !authorize_db_operation(&auth_context, "platform:db:merge") {
            return Err(db_error(axum::http::StatusCode::FORBIDDEN, "Forbidden"));
        }
        "#,
    );
    let compact_errors = compact(&policy_errors);
    let format_at = compact_merge.find(".validate()");
    let output_at = compact_merge.find(".output_context(");
    let execute_at = compact_merge.find(".merge_abox(");
    if !compact_layout
        .contains(".join(\"tenants\").join(tenant_id).join(\"db\").join(operation_id)")
        || !compact_ingest.contains("Uuid::new_v4().simple().to_string()")
        || !compact_merge.contains("Uuid::new_v4().simple().to_string()")
        || !ingest_body.starts_with(&ingest_guard)
        || !merge_body.starts_with(&merge_guard)
        || ingest_body.matches("authorize_db_operation(").count() != 1
        || merge_body.matches("authorize_db_operation(").count() != 1
        || !matches!((format_at, output_at, execute_at), (Some(format), Some(output), Some(execute)) if format < output && output < execute)
    {
        warn(
            warnings,
            "database",
            format!(
                "{PLATFORM_LIB}: DB output must validate formats before writes and stay under tenants/{{tenant}}/db/{{uuid}}"
            ),
        );
    }
    if !serde_deny_unknown(&lib, "DbSchemaIngestRequest")
        || !serde_deny_unknown(&lib, "DbAboxMergeRequest")
        || !compact_errors.contains("StatusCode::BAD_REQUEST")
        || !compact_errors.contains("\"Invaliddatabaserequest\"")
        || !compact_errors.contains("StatusCode::SERVICE_UNAVAILABLE")
        || !compact_ingest.contains("StatusCode::FORBIDDEN")
        || !compact_ingest.contains("StatusCode::BAD_GATEWAY")
        || !compact_ingest.contains("source.sanitize_error(&error)")
        || !compact_ingest.contains("source=?redacted")
        || !compact_merge.contains("StatusCode::FORBIDDEN")
        || !compact_merge.contains("StatusCode::BAD_GATEWAY")
    {
        warn(
            warnings,
            "database",
            format!(
                "{PLATFORM_LIB}: DB HTTP boundary must keep strict DTOs, stable status/messages, and redacted executor failures"
            ),
        );
    }

    let Some(main) = read_active_rust(root, PLATFORM_MAIN, "database", warnings) else {
        return;
    };
    let policy_at = main.find("AdbcPolicy::from_env");
    let server_at = main
        .find("ExampleServer::new")
        .or_else(|| main.find("ExampleServer::with_flight_writer_policies"));
    let compact_main = compact(&main);
    let injects_auth_and_policy = compact_main
        .contains("ExampleServer::new(config,registry,auth,adbc_policy)")
        || compact_main.contains(
            "ExampleServer::with_flight_writer_policies(config,registry,auth,adbc_policy",
        );
    if !matches!((policy_at, server_at), (Some(policy), Some(server)) if policy < server)
        || !injects_auth_and_policy
    {
        warn(
            warnings,
            "database",
            format!(
                "{PLATFORM_MAIN}: AdbcPolicy must load before server construction and be injected with AuthConfig"
            ),
        );
    }
}

fn check_python_jobs(root: &Path, warnings: &mut Vec<String>) {
    let Some(raw) = read_required(root, PYTHON_TASKS, "python-jobs", warnings) else {
        return;
    };
    let source = strip_python_comments(&raw);
    let helper = python_fn(&source, "_platform_service_auth_headers").unwrap_or_default();
    let namespace_helper = python_fn(&source, "_platform_job_namespace").unwrap_or_default();
    let validator = python_fn(&source, "_validate_platform_namespace").unwrap_or_default();
    let compact_helper = compact(&helper);
    let validation_at = compact_helper.find("namespace=_validate_platform_namespace(namespace)");
    let token_at = compact_helper.find("token=create_access_token(");
    if !matches!((validation_at, token_at), (Some(validation), Some(token)) if validation < token)
        || compact_helper
            .matches("_validate_platform_namespace(namespace)")
            .count()
            != 1
        || !compact_helper.contains("role=\"service\"")
        || !compact_helper.contains("namespace=namespace")
        || !compact_helper.contains("extra={\"permissions\":list(permissions)}")
    {
        warn(
            warnings,
            "python-jobs",
            format!(
                "{PYTHON_TASKS}: platform service tokens must use canonical create_access_token authority and preserve tenant/permissions"
            ),
        );
    }
    let compact_namespace = compact(&namespace_helper);
    let compact_validator = compact(&validator);
    let compact_source = compact(&source);
    let namespace_guard_at = compact_namespace.find("ifnotisinstance(namespace,str):");
    let namespace_selection_at = compact_namespace
        .find("resolved=os.getenv(\"EXAMPLE_NAMESPACE\",\"\")ifnamespace==\"\"elsenamespace");
    let exact_pattern =
        r#"_PLATFORM_NAMESPACE_PATTERN=re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}\Z")"#;
    if !compact_source.contains(exact_pattern)
        || !compact_validator.contains(
            "ifnotisinstance(namespace,str)or_PLATFORM_NAMESPACE_PATTERN.fullmatch(namespace)isNone:",
        )
        || !compact_validator.contains("raiseValueError(")
        || !compact_validator.contains("returnnamespace")
        || compact_validator
            .matches("_PLATFORM_NAMESPACE_PATTERN.fullmatch(namespace)")
            .count()
            != 1
        || !matches!((namespace_guard_at, namespace_selection_at), (Some(guard), Some(selection)) if guard < selection)
        || !compact_namespace.contains("ifnotisinstance(namespace,str):raiseValueError(")
        || compact_namespace
            .matches("os.getenv(\"EXAMPLE_NAMESPACE\",\"\")")
            .count()
            != 1
        || compact_namespace.contains("namespaceoros.getenv(")
        || !compact_namespace.contains("return_validate_platform_namespace(resolved)")
        || compact_namespace.contains(".strip()")
        || namespace_helper.contains("\"default\"")
        || namespace_helper.contains("'default'")
    {
        warn(
            warnings,
            "python-jobs",
            format!(
                "{PYTHON_TASKS}: platform jobs must validate string tenant input and consult EXAMPLE_NAMESPACE only for an explicit empty string"
            ),
        );
    }
    for (constant, expected) in [
        (
            "_PLATFORM_INDUCTION_PERMISSIONS",
            "(\"platform:induction:write\",\"platform:induction:read\")",
        ),
        (
            "_PLATFORM_ALIGNMENT_PERMISSIONS",
            "(\"platform:alignment:write\",\"platform:alignment:read\")",
        ),
    ] {
        if !compact_source.contains(&format!("{constant}={expected}")) {
            warn(
                warnings,
                "python-jobs",
                format!("{PYTHON_TASKS}: `{constant}` must carry its exact two permissions"),
            );
        }
    }

    for (name, job_id, permissions) in PLATFORM_TASKS {
        let Some(function) = python_fn(&source, name) else {
            warn(
                warnings,
                "python-jobs",
                format!("{PYTHON_TASKS}: missing tenant-scoped task `{name}`"),
            );
            continue;
        };
        let function = compact(&function);
        let expected_job = format!("job_id={job_id}");
        let post_calls = python_request_calls(&function, "post")
            .into_iter()
            .filter(|(_, arguments)| is_platform_request(arguments))
            .collect::<Vec<_>>();
        let get_calls = python_request_calls(&function, "get")
            .into_iter()
            .filter(|(_, arguments)| is_platform_request(arguments))
            .collect::<Vec<_>>();
        let trigger_at = post_calls.first().map(|(start, _)| *start);
        let all_posts_authenticated = !post_calls.is_empty()
            && post_calls
                .iter()
                .all(|(_, arguments)| arguments.contains("headers=auth_headers"));
        let all_gets_authenticated = !get_calls.is_empty()
            && get_calls
                .iter()
                .all(|(_, arguments)| arguments.contains("headers=auth_headers"));
        let tail_at = function.find("stream_cursor=_platform_stream_tail_cursor(");
        if !function.contains("namespace")
            || !function.contains("stream_cursor")
            || !function.contains("namespace=_platform_job_namespace(namespace)")
            || !function.contains(&format!(
                "auth_headers=_platform_service_auth_headers(namespace,{permissions})"
            ))
            || !all_posts_authenticated
            || !all_gets_authenticated
            || !matches!((tail_at, trigger_at), (Some(tail), Some(trigger)) if tail < trigger)
            || !function.contains("xread_for_job(")
            || !function.contains("tenant_id=namespace")
            || !function.contains(&expected_job)
            || !function.contains("\"namespace\":namespace")
            || !function.contains("\"stream_cursor\":")
            || function.contains("\"default\"")
            || function.contains("'default'")
        {
            warn(
                warnings,
                "python-jobs",
                format!(
                    "{PYTHON_TASKS}: `{name}` must preserve tenant/cursor, authenticate trigger and fallback, and wait on tenant plus job"
                ),
            );
        }
    }
}

fn check_completion_events(root: &Path, warnings: &mut Vec<String>) {
    for (rel, service) in [(PLATFORM_EVENTS, "platform"), (ALIGN_JOBS, "align")] {
        let Some(source) = read_active_rust(root, rel, "events", warnings) else {
            continue;
        };
        let fields = rust_fn(&source, "job_event_fields").unwrap_or_default();
        let publish = rust_fn(&source, "publish_job_event").unwrap_or_default();
        let compact_fields = compact(&fields);
        let compact_publish = compact(&publish);
        if !compact_fields.contains("tenant_id:&str")
            || !compact_fields.contains("job_id:&str")
            || !compact_fields.contains("(\"tenant_id\",tenant_id.to_owned())")
            || !compact_fields.contains("(\"job_id\",job_id.to_owned())")
            || !compact_publish.contains("job_event_fields(tenant_id,job_id,status)")
        {
            warn(
                warnings,
                "events",
                format!(
                    "{rel}: {service} completion events must publish canonical tenant_id plus job_id"
                ),
            );
        }
        let caller = if rel == PLATFORM_EVENTS {
            rust_fn(&source, "post_induction_job").unwrap_or_default()
        } else {
            rust_fn(&source, "spawn_worker").unwrap_or_default()
        };
        let compact_caller = compact(&caller);
        if !compact_caller.contains("publish_job_event(&tenant_id,")
            || !compact_caller.contains("job_id")
        {
            warn(
                warnings,
                "events",
                format!(
                    "{rel}: {service} completion publisher must pass the owning tenant and completed job ID"
                ),
            );
        }
    }

    let Some(raw) = read_required(root, PYTHON_REDIS, "redis-wait", warnings) else {
        return;
    };
    let source = strip_python_comments(&raw);
    let waiter = python_fn(&source, "xread_for_job").unwrap_or_default();
    let compact_waiter = compact(&waiter);
    if !compact_waiter.contains("tenant_id:str")
        || !compact_waiter.contains("job_id:str")
        || !compact_waiter
            .contains("decoded.get(\"tenant_id\")==tenant_idanddecoded.get(\"job_id\")==job_id")
        || !compact_waiter.contains("client.xread({stream_key:cursor}")
        || !compact_waiter.contains("cursor=entry_id.decode()")
    {
        warn(
            warnings,
            "redis-wait",
            format!(
                "{PYTHON_REDIS}: xread_for_job must advance its cursor and match tenant_id plus job_id"
            ),
        );
    }
}

fn check_bearer_forwarding(root: &Path, warnings: &mut Vec<String>) {
    let Some(router) = read_active_rust(root, PLATFORM_ROUTER, "bearer-forwarding", warnings)
    else {
        return;
    };
    let inbound = rust_fn(&router, "inbound_authorization").unwrap_or_default();
    let apply = rust_fn(&router, "apply_authorization").unwrap_or_default();
    let compact_inbound = compact(&inbound);
    let compact_apply = compact(&apply);
    let exact_inbound_body = "headers.get(axum::http::header::AUTHORIZATION).cloned()";
    let exact_apply_body = compact(
        r#"
        match authorization {
            Some(value) => request.header(reqwest::header::AUTHORIZATION, value.as_bytes()),
            None => request,
        }
        "#,
    );
    if !compact_inbound.contains("Option<HeaderValue>")
        || rust_block_body(&inbound).map(compact).as_deref() != Some(exact_inbound_body)
        || !compact_apply.contains("authorization:Option<&HeaderValue>")
        || rust_block_body(&apply).map(compact).as_deref() != Some(exact_apply_body.as_str())
    {
        warn(
            warnings,
            "bearer-forwarding",
            format!(
                "{PLATFORM_ROUTER}: bearer forwarding must clone the inbound header and attach it request-locally"
            ),
        );
    }

    let logged_secret = Regex::new(
        r"(?s)(?:(?:tracing|log)::)?(?:trace|info|warn|error|debug|event|log)!\s*\([^;}]*(?:authorization|bearer)|(?:println|eprintln|dbg)!\s*\([^;}]*(?:authorization|bearer)",
    )
    .expect("bearer logging regex");
    if has_logging_macro(&inbound) || has_logging_macro(&apply) {
        warn(
            warnings,
            "bearer-forwarding",
            format!("{PLATFORM_ROUTER}: bearer helper must never log request credentials"),
        );
    }

    let Some(lib) = read_active_rust(root, PLATFORM_LIB, "bearer-forwarding", warnings) else {
        return;
    };
    let state = rust_named_block(&lib, "struct", "ServerState").unwrap_or_default();
    if has_persisted_bearer_field(&state) {
        warn(
            warnings,
            "bearer-forwarding",
            format!("{PLATFORM_LIB}: ServerState must not persist request bearer credentials"),
        );
    }
    for (name, record) in rust_struct_blocks(&lib) {
        if has_persisted_bearer_field(&record) {
            warn(
                warnings,
                "bearer-forwarding",
                format!(
                    "{PLATFORM_LIB}: struct `{name}` must not persist or serialize request bearer credentials"
                ),
            );
        }
    }

    let (_, routed) = protected_router_handlers(&lib);
    let mut proxy_handlers = PROXY_HANDLERS
        .iter()
        .map(|name| (*name).to_string())
        .collect::<BTreeSet<_>>();
    proxy_handlers.extend(
        routed
            .iter()
            .filter(|name| !name.contains("::"))
            .filter(|name| {
                rust_fn(&lib, name).is_some_and(|function| {
                    function
                        .split('{')
                        .next()
                        .unwrap_or(&function)
                        .contains("HeaderMap")
                        && (function.contains("http_client") || function.contains("reqwest"))
                })
            })
            .cloned(),
    );
    for handler in proxy_handlers {
        check_proxy_handler(PLATFORM_LIB, &lib, &handler, warnings, &logged_secret);
    }
    for routed_handler in routed.iter().filter(|name| {
        name.contains("::") && !name.starts_with("events::") && !name.starts_with("crate::events::")
    }) {
        let Some((rel, handler_source, name)) =
            routed_handler_surface(root, &lib, "", routed_handler)
        else {
            continue;
        };
        let function = rust_fn(&handler_source, &name).unwrap_or_default();
        let signature = function.split('{').next().unwrap_or(&function);
        if signature.contains("HeaderMap")
            && (function.contains("http_client") || function.contains("reqwest"))
        {
            check_proxy_handler(&rel, &handler_source, &name, warnings, &logged_secret);
        }
    }

    for rel in [PLATFORM_AUTH, ALIGN_AUTH] {
        let Some(auth) = read_active_rust(root, rel, "bearer-forwarding", warnings) else {
            continue;
        };
        let context = rust_named_block(&auth, "struct", "AuthContext").unwrap_or_default();
        if has_persisted_bearer_field(&context) {
            warn(
                warnings,
                "bearer-forwarding",
                format!("{rel}: AuthContext must not persist raw bearer credentials"),
            );
        }
    }
}

fn check_proxy_origins(root: &Path, warnings: &mut Vec<String>) {
    let Some(lib) = read_active_rust(root, PLATFORM_LIB, "proxy-origins", warnings) else {
        return;
    };
    let state = rust_named_block(&lib, "struct", "ServerState").unwrap_or_default();
    let compact_state = compact(&state);
    let run = rust_fn(&lib, "run").unwrap_or_default();
    let compact_run = compact(&run);
    let compact_lib = compact(&lib);
    let immutable_fields = compact_state.matches("align_url:String").count() == 1
        && compact_state.matches("ocr_url:String").count() == 1;
    let operator_initialized = compact_run
        .matches("align_url:self.config.align_url.clone()")
        .count()
        == 1
        && compact_run
            .matches("ocr_url:self.config.ocr_url.clone()")
            .count()
            == 1;
    let discovery_rebind = compact_run.contains("discovery::resolve_sidecars(")
        || compact_run.contains("resolved.align_url")
        || compact_run.contains("resolved.ocr_url")
        || compact_lib.contains("state.align_url.write(")
        || compact_lib.contains("state.ocr_url.write(")
        || compact_lib.contains("state.align_url=")
        || compact_lib.contains("state.ocr_url=");
    if !immutable_fields || !operator_initialized || discovery_rebind {
        warn(
            warnings,
            "proxy-origins",
            format!(
                "{PLATFORM_LIB}: bearer-bearing proxy origins must be immutable operator-configured Strings; discovery may be observational only"
            ),
        );
    }

    let logged_secret = Regex::new(
        r"(?s)(?:(?:tracing|log)::)?(?:trace|info|warn|error|debug|event|log)!\s*\([^;}]*(?:authorization|bearer)|(?:println|eprintln|dbg)!\s*\([^;}]*(?:authorization|bearer)",
    )
    .expect("proxy origin bearer logging regex");
    for (handler, field) in ALIGN_ORIGIN_HANDLERS
        .iter()
        .map(|handler| (*handler, "align_url"))
        .chain(
            OCR_ORIGIN_HANDLERS
                .iter()
                .map(|handler| (*handler, "ocr_url")),
        )
    {
        let Some(function) = rust_fn(&lib, handler) else {
            continue;
        };
        if !proxy_bearer_contract_holds(&lib, handler, &function, &logged_secret) {
            continue;
        }
        let compact_function = compact(&function);
        let expected = format!("state.{field}.clone()");
        if !compact_function.contains(&expected)
            || compact_function.contains("resolve_sidecars(")
            || compact_function.contains("state.discovery")
            || compact_function.contains("resolved.align_url")
            || compact_function.contains("resolved.ocr_url")
        {
            warn(
                warnings,
                "proxy-origins",
                format!(
                    "{PLATFORM_LIB}: proxy handler `{handler}` must derive its destination from immutable `ServerState.{field}`"
                ),
            );
        }
    }
}

fn proxy_bearer_contract_holds(
    source: &str,
    handler: &str,
    function: &str,
    logged_secret: &Regex,
) -> bool {
    let signature = function.split('{').next().unwrap_or(function);
    let compact_function = compact(function);
    signature.contains("HeaderMap")
        && compact_function
            .matches("letauthorization=crate::router::inbound_authorization(&headers);")
            .count()
            == 1
        && authorization_binding_count(function) == 1
        && proxy_sends_are_request_local(function)
        && !logged_secret.is_match(function)
        && !sensitive_bearer_identifiers(function)
            .iter()
            .any(|variable| logging_mentions(function, variable))
        && !instrument_captures_headers(source, handler)
}

fn check_proxy_handler(
    rel: &str,
    source: &str,
    handler: &str,
    warnings: &mut Vec<String>,
    logged_secret: &Regex,
) {
    let Some(function) = rust_fn(source, handler) else {
        warn(
            warnings,
            "bearer-forwarding",
            format!("{rel}: missing proxy-owning handler `{handler}`"),
        );
        return;
    };
    let signature = function.split('{').next().unwrap_or(&function);
    let compact_function = compact(&function);
    if !signature.contains("HeaderMap")
        || compact_function
            .matches("letauthorization=crate::router::inbound_authorization(&headers);")
            .count()
            != 1
        || authorization_binding_count(&function) != 1
        || !proxy_sends_are_request_local(&function)
    {
        warn(
            warnings,
            "bearer-forwarding",
            format!(
                "{rel}: proxy handler `{handler}` must forward the request-local Authorization header"
            ),
        );
    }
    if logged_secret.is_match(&function)
        || sensitive_bearer_identifiers(&function)
            .iter()
            .any(|variable| logging_mentions(&function, variable))
        || instrument_captures_headers(source, handler)
    {
        warn(
            warnings,
            "bearer-forwarding",
            format!("{rel}: proxy handler `{handler}` logs bearer material"),
        );
    }
}

fn has_persisted_bearer_field(block: &str) -> bool {
    [
        "authorization",
        "bearer",
        "bearer_token",
        "auth_header",
        "request_headers",
        "access_token",
        "token",
        "headers",
    ]
    .iter()
    .any(|field| rust_field(block, field))
        || Regex::new(
            r"(?m)(?:^|[\{,])\s*(?:pub(?:\([^)]*\))?\s+)?[A-Za-z_][A-Za-z0-9_]*\s*:\s*[^,\n}]*(?:HeaderValue|HeaderMap)",
        )
        .expect("persisted header type regex")
        .is_match(block)
}

fn has_logging_macro(source: &str) -> bool {
    Regex::new(
        r"(?m)(?:(?:tracing|log)::)?(?:trace|info|warn|error|debug|event|log)!\s*\(|(?:println|eprintln|dbg)!\s*\(",
    )
    .expect("logging macro regex")
    .is_match(source)
}

fn logging_mentions(source: &str, identifier: &str) -> bool {
    let logging_macro = Regex::new(
        r"(?m)(?:(?:tracing|log)::)?(?:trace|info|warn|error|debug|event|log)!\s*\(|(?:println|eprintln|dbg)!\s*\(",
    )
    .expect("credential logging dataflow regex");
    logging_macro.find_iter(source).any(|matched| {
        let Some(open) = matched.end().checked_sub(1) else {
            return false;
        };
        let Some(close) = matching_parenthesis(source, open) else {
            return false;
        };
        identifier_count(&source[open + 1..close], identifier) > 0
    })
}

fn authorization_binding_count(function: &str) -> usize {
    Regex::new(r"\blet\s+(?:mut\s+)?authorization(?:\s*:[^=;]+)?\s*=")
        .expect("authorization binding regex")
        .find_iter(function)
        .count()
}

fn instrument_captures_headers(source: &str, function: &str) -> bool {
    let attribute = Regex::new(&format!(
        r"(?s)#\s*\[\s*(?:tracing::)?instrument(?:\s*\(([^]]*)\))?\s*\]\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+{}\b",
        regex::escape(function)
    ))
    .expect("tracing instrument regex");
    let Some(capture) = attribute.captures(source) else {
        return false;
    };
    let arguments = capture.get(1).map_or("", |matched| matched.as_str());
    let arguments = compact(arguments);
    let explicit_header_field =
        named_call_sites(&arguments, "fields")
            .iter()
            .any(|(_, _, fields)| {
                fields.iter().any(|field| {
                    identifier_count(field, "headers") > 0
                        || identifier_count(field, "authorization") > 0
                })
            });
    if explicit_header_field {
        return true;
    }
    if arguments.contains("skip_all") {
        return false;
    }
    let skipped = Regex::new(r"skip\(([^)]*)\)")
        .expect("instrument skip regex")
        .captures(&arguments)
        .is_some_and(|skip| {
            skip[1]
                .split(',')
                .any(|identifier| ["headers", "authorization"].contains(&identifier))
        });
    !skipped
}

fn proxy_sends_are_request_local(function: &str) -> bool {
    if function.contains(".execute(") || function.contains("http_client.execute(") {
        return false;
    }
    let calls = named_call_sites(function, "apply_authorization");
    if calls.is_empty()
        || calls.iter().any(|(_, _, arguments)| {
            arguments.len() != 2 || compact(&arguments[1]) != "authorization.as_ref()"
        })
    {
        return false;
    }

    let send_sites = function.match_indices(".send()").collect::<Vec<_>>();
    if send_sites.is_empty() {
        return true;
    }
    if send_sites.len() != calls.len() {
        return false;
    }
    calls
        .iter()
        .zip(send_sites)
        .all(|((_, close, _), (send, _))| {
            *close < send && is_direct_builder_chain(&function[*close + 1..send])
        })
}

fn named_call_sites(source: &str, name: &str) -> Vec<(usize, usize, Vec<String>)> {
    let expression =
        Regex::new(&format!(r"\b{}\s*\(", regex::escape(name))).expect("named call regex");
    expression
        .find_iter(source)
        .filter_map(|matched| {
            let open = matched.end().checked_sub(1)?;
            let close = matching_parenthesis(source, open)?;
            Some((
                matched.start(),
                close,
                split_top_level_arguments(&source[open + 1..close]),
            ))
        })
        .collect()
}

fn is_direct_builder_chain(source: &str) -> bool {
    if source.trim().is_empty() {
        return true;
    }
    if !source.trim_start().starts_with('.') {
        return false;
    }
    let mut parentheses = 0usize;
    let mut brackets = 0usize;
    let mut braces = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for character in source.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '(' => parentheses += 1,
            ')' => parentheses = parentheses.saturating_sub(1),
            '[' => brackets += 1,
            ']' => brackets = brackets.saturating_sub(1),
            '{' => braces += 1,
            '}' => braces = braces.saturating_sub(1),
            ';' | ',' | '=' if parentheses == 0 && brackets == 0 && braces == 0 => return false,
            _ => {}
        }
    }
    parentheses == 0 && brackets == 0 && braces == 0
}

fn split_top_level_arguments(source: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut start = 0usize;
    let mut parentheses = 0usize;
    let mut brackets = 0usize;
    let mut braces = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, character) in source.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '(' => parentheses += 1,
            ')' => parentheses = parentheses.saturating_sub(1),
            '[' => brackets += 1,
            ']' => brackets = brackets.saturating_sub(1),
            '{' => braces += 1,
            '}' => braces = braces.saturating_sub(1),
            ',' if parentheses == 0 && brackets == 0 && braces == 0 => {
                arguments.push(source[start..index].trim().to_string());
                start = index + 1;
            }
            _ => {}
        }
    }
    if !source[start..].trim().is_empty() {
        arguments.push(source[start..].trim().to_string());
    }
    arguments
}

fn last_top_level_expression(source: &str) -> Option<&str> {
    let mut start = 0usize;
    let mut parentheses = 0usize;
    let mut brackets = 0usize;
    let mut braces = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, character) in source.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '(' => parentheses += 1,
            ')' => parentheses = parentheses.saturating_sub(1),
            '[' => brackets += 1,
            ']' => brackets = brackets.saturating_sub(1),
            '{' => braces += 1,
            '}' => braces = braces.saturating_sub(1),
            ';' if parentheses == 0 && brackets == 0 && braces == 0 => start = index + 1,
            _ => {}
        }
    }
    let expression = source[start..].trim();
    (!expression.is_empty()).then_some(expression)
}

fn let_binding_statement<'a>(source: &'a str, binding: &str) -> Option<&'a str> {
    let expression = Regex::new(&format!(r"\blet\s+{}\s*=", regex::escape(binding))).ok()?;
    let matched = expression.find(source)?;
    let tail = &source[matched.end()..];
    let mut parentheses = 0usize;
    let mut brackets = 0usize;
    let mut braces = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, character) in tail.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '(' => parentheses += 1,
            ')' => parentheses = parentheses.saturating_sub(1),
            '[' => brackets += 1,
            ']' => brackets = brackets.saturating_sub(1),
            '{' => braces += 1,
            '}' => braces = braces.saturating_sub(1),
            ';' if parentheses == 0 && brackets == 0 && braces == 0 => {
                return Some(&source[matched.start()..matched.end() + offset]);
            }
            _ => {}
        }
    }
    None
}

fn let_binding_count(source: &str, binding: &str) -> usize {
    Regex::new(&format!(r"\blet\s+{}\s*=", regex::escape(binding)))
        .expect("let binding count regex")
        .find_iter(source)
        .count()
}

fn sensitive_bearer_identifiers(function: &str) -> BTreeSet<String> {
    let mut sensitive = BTreeSet::new();
    let signature = function.split('{').next().unwrap_or(function);
    let typed_parameter =
        Regex::new(r"([A-Za-z_][A-Za-z0-9_]*)\s*:\s*[^,\n)]*(?:HeaderValue|HeaderMap)[^,\n)]*")
            .expect("header parameter regex");
    sensitive.extend(
        typed_parameter
            .captures_iter(signature)
            .map(|capture| capture[1].to_string()),
    );

    let assignment =
        Regex::new(r"(?s)let\s+([^=;]+)=\s*([^;]+);").expect("credential assignment regex");
    let binding_identifier =
        Regex::new(r"\b([a-z][A-Za-z0-9_]*)\b").expect("binding identifier regex");
    loop {
        let mut changed = false;
        for capture in assignment.captures_iter(function) {
            let bindings = binding_identifier
                .captures_iter(&capture[1])
                .map(|binding| binding[1].to_string())
                .filter(|binding| !["let", "mut", "ref"].contains(&binding.as_str()))
                .collect::<Vec<_>>();
            let rhs = &capture[2];
            let direct_extraction =
                rhs.contains("AUTHORIZATION") || rhs.contains("inbound_authorization");
            let references_sensitive = sensitive
                .iter()
                .any(|identifier| identifier_count(rhs, identifier) > 0);
            let declassified_response = rhs.contains(".send()") || rhs.contains("proxy_align(");
            let derived = direct_extraction || (references_sensitive && !declassified_response);
            if derived {
                for binding in bindings {
                    if sensitive.insert(binding) {
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    sensitive
}

fn compact(source: &str) -> String {
    source
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn identifier_count(source: &str, identifier: &str) -> usize {
    Regex::new(&format!(r"\b{}\b", regex::escape(identifier)))
        .expect("identifier count regex")
        .find_iter(source)
        .count()
}

fn word_count(source: &str, word: &str) -> usize {
    identifier_count(source, word)
}

fn rust_field(block: &str, field: &str) -> bool {
    Regex::new(&format!(
        r"(?m)(?:^|[{{,])\s*(?:pub(?:\([^)]*\))?\s+)?{}\s*:",
        regex::escape(field)
    ))
    .expect("field regex")
    .is_match(block)
}

fn serde_deny_unknown(source: &str, struct_name: &str) -> bool {
    Regex::new(&format!(
        r"(?s)#\s*\[\s*serde\s*\(\s*deny_unknown_fields\s*\)\s*\]\s*(?:pub\s+)?struct\s+{}\b",
        regex::escape(struct_name)
    ))
    .expect("serde deny regex")
    .is_match(source)
}

fn rust_fn(source: &str, name: &str) -> Option<String> {
    rust_named_block(source, "fn", name)
}

fn rust_block_body(block: &str) -> Option<&str> {
    let open = block.find('{')?;
    let close = matching_brace(block, open)?;
    Some(&block[open + 1..close])
}

fn rust_named_block(source: &str, keyword: &str, name: &str) -> Option<String> {
    let expression = Regex::new(&format!(
        r"\b{}\s+{}\b",
        regex::escape(keyword),
        regex::escape(name)
    ))
    .ok()?;
    let matched = expression.find(source)?;
    let open = source[matched.end()..].find('{')? + matched.end();
    let close = matching_brace(source, open)?;
    Some(source[matched.start()..=close].to_string())
}

fn rust_struct_blocks(source: &str) -> Vec<(String, String)> {
    let expression =
        Regex::new(r"\bstruct\s+([A-Za-z_][A-Za-z0-9_]*)\b").expect("struct block regex");
    expression
        .captures_iter(source)
        .filter_map(|capture| {
            let matched = capture.get(0)?;
            let open = source[matched.end()..].find('{')? + matched.end();
            let close = matching_brace(source, open)?;
            Some((
                capture[1].to_string(),
                source[matched.start()..=close].to_string(),
            ))
        })
        .collect()
}

fn rust_function_blocks(source: &str) -> Vec<(String, String)> {
    let expression =
        Regex::new(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\b").expect("function block regex");
    expression
        .captures_iter(source)
        .filter_map(|capture| {
            let matched = capture.get(0)?;
            let open = source[matched.end()..].find('{')? + matched.end();
            let close = matching_brace(source, open)?;
            Some((
                capture[1].to_string(),
                source[matched.start()..=close].to_string(),
            ))
        })
        .collect()
}

fn matching_brace(source: &str, open: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut index = open;
    let mut in_string = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
        } else if byte == b'{' {
            depth += 1;
        } else if byte == b'}' {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(index);
            }
        }
        index += 1;
    }
    None
}

fn matching_parenthesis(source: &str, open: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut index = open;
    let mut in_string = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
        } else if byte == b'(' {
            depth += 1;
        } else if byte == b')' {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(index);
            }
        }
        index += 1;
    }
    None
}

fn active_rust(source: &str) -> String {
    strip_cfg_test_items(&strip_rust_comments(source))
}

fn strip_rust_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    let mut block_depth = 0usize;
    let mut line_comment = false;
    let mut in_string = false;
    let mut escaped = false;

    while index < bytes.len() {
        let byte = bytes[index];
        let next = bytes.get(index + 1).copied();
        if line_comment {
            if byte == b'\n' {
                line_comment = false;
                output.push(byte);
            } else {
                output.push(b' ');
            }
            index += 1;
            continue;
        }
        if block_depth > 0 {
            if byte == b'/' && next == Some(b'*') {
                block_depth += 1;
                output.extend_from_slice(b"  ");
                index += 2;
            } else if byte == b'*' && next == Some(b'/') {
                block_depth -= 1;
                output.extend_from_slice(b"  ");
                index += 2;
            } else {
                output.push(if byte == b'\n' { b'\n' } else { b' ' });
                index += 1;
            }
            continue;
        }
        if in_string {
            output.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
            output.push(byte);
            index += 1;
        } else if byte == b'/' && next == Some(b'/') {
            line_comment = true;
            output.extend_from_slice(b"  ");
            index += 2;
        } else if byte == b'/' && next == Some(b'*') {
            block_depth = 1;
            output.extend_from_slice(b"  ");
            index += 2;
        } else {
            output.push(byte);
            index += 1;
        }
    }
    String::from_utf8(output).unwrap_or_default()
}

fn strip_cfg_test_items(source: &str) -> String {
    let attribute =
        Regex::new(r"(?s)#\s*\[\s*cfg\s*\(([^]]*)\)\s*\]").expect("cfg attribute regex");
    let mut output = source.to_string();

    loop {
        let target = attribute
            .captures_iter(&output)
            .find(|capture| cfg_is_test_only(&capture[1]))
            .and_then(|capture| {
                capture
                    .get(0)
                    .map(|matched| (matched.start(), matched.end()))
            });
        let Some((start, attribute_end)) = target else {
            break;
        };
        let tail = &output[attribute_end..];
        let brace = tail.find('{').map(|offset| attribute_end + offset);
        let semicolon = tail.find(';').map(|offset| attribute_end + offset);
        let end = match (brace, semicolon) {
            (Some(open), Some(semi)) if semi < open => semi,
            (Some(open), _) => matching_brace(&output, open).unwrap_or(output.len() - 1),
            (None, Some(semi)) => semi,
            (None, None) => output.len() - 1,
        };
        let replacement = output[start..=end]
            .chars()
            .map(|character| if character == '\n' { '\n' } else { ' ' })
            .collect::<String>();
        output.replace_range(start..=end, &replacement);
    }
    output
}

fn cfg_is_test_only(expression: &str) -> bool {
    let expression = compact(expression);
    expression == "test"
        || (expression.starts_with("all(")
            && expression.ends_with(')')
            && expression.contains("test")
            && !expression.contains("not(test)")
            && !expression.contains("any("))
}

fn strip_python_comments(source: &str) -> String {
    source
        .lines()
        .map(strip_python_line_comment)
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_python_line_comment(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut quote = None;
    let mut escaped = false;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == delimiter {
                quote = None;
            }
        } else if byte == b'\'' || byte == b'"' {
            quote = Some(byte);
        } else if byte == b'#' {
            return line[..index].to_string();
        }
    }
    line.to_string()
}

fn python_fn(source: &str, name: &str) -> Option<String> {
    let marker = format!("def {name}(");
    let start = source
        .match_indices(&marker)
        .find(|(index, _)| *index == 0 || source.as_bytes().get(index - 1) == Some(&b'\n'))?
        .0;
    let tail = &source[start..];
    let mut end = tail.len();
    let mut offset = 0usize;
    for line in tail.lines() {
        if offset > 0 && (line.starts_with("def ") || line.starts_with('@')) {
            end = offset;
            break;
        }
        offset += line.len() + 1;
    }
    Some(tail[..end].to_string())
}

fn python_request_calls(source: &str, method: &str) -> Vec<(usize, String)> {
    let expression = Regex::new(&format!(r"\brequests\.{}\s*\(", regex::escape(method)))
        .expect("Python requests call regex");
    expression
        .find_iter(source)
        .filter_map(|matched| {
            let open = matched.end().checked_sub(1)?;
            let close = matching_parenthesis(source, open)?;
            Some((matched.start(), source[open + 1..close].to_string()))
        })
        .collect()
}

fn is_platform_request(arguments: &str) -> bool {
    [
        "SERVER_URL",
        "ALIGN_URL",
        "platform_url",
        "platform_job_url",
    ]
    .iter()
    .any(|target| arguments.contains(target))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_runtime_trust_boundary_active_rust_ignores_comments_and_cfg_test_items() {
        let source = r#"
            struct ServerState { tenant_states: TenantStateRegistry }
            // struct ServerState { induction: Global }
            /* fn handler() { state.flight_ingest.read(); } */
            #[cfg(test)]
            mod tests { struct ServerState { tbox_manager: Global } }
            #[cfg(not(test))]
            fn production_only() { active_boundary(); }
            #[cfg(any(test, feature = "runtime"))]
            fn runtime_or_test() { active_boundary(); }
        "#;
        let active = active_rust(source);
        assert!(active.contains("tenant_states"));
        assert!(!active.contains("induction"));
        assert!(!active.contains("flight_ingest"));
        assert!(!active.contains("tbox_manager"));
        assert!(active.contains("production_only"));
        assert!(active.contains("runtime_or_test"));
    }
}
