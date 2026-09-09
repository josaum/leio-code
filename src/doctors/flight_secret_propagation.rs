//! `flight-runtime-auth` doctor.
//!
//! Guards the production/runtime `ExistingAuth` matrix instead of perpetuating
//! the retired `EXAMPLE_FLIGHT_SECRET` bearer. It verifies receiver mode,
//! canonical claim constraints, signing-material exposure, static-bearer
//! absence, the gateway's dedicated platform URL, and the documented dynamic
//! credential boundary for Brains and Office.
//!
//! The implementation intentionally stays dependency-light (a small YAML
//! scope scanner plus exact source checks) so `leio-code doctor ci` remains
//! cheap and deterministic.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct FlightRuntimeAuthDoctor;

impl Doctor for FlightRuntimeAuthDoctor {
    fn name(&self) -> &'static str {
        "flight-runtime-auth"
    }

    fn description(&self) -> &'static str {
        "Verifies the ExistingAuth runtime matrix, public-only key exposure, dynamic credentials, and platform Flight URL truth."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_flight_runtime_auth(root)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimeAuthMode {
    Es256,
    ControlledHs256,
}

impl RuntimeAuthMode {
    fn label(self) -> &'static str {
        match self {
            Self::Es256 => "es256",
            Self::ControlledHs256 => "controlled-hs256",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlatformFlightTopology {
    Enabled,
    Disabled,
    NotApplicable,
}

impl PlatformFlightTopology {
    fn label(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
            Self::NotApplicable => "not-applicable",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct RuntimeReceiver {
    file: &'static str,
    service: &'static str,
    mode: RuntimeAuthMode,
    platform_topology: PlatformFlightTopology,
    rest_dev_bypass: bool,
}

const RECEIVERS: &[RuntimeReceiver] = &[
    RuntimeReceiver {
        file: "example-api/docker-compose.yml",
        service: "ocr-sidecar",
        mode: RuntimeAuthMode::Es256,
        platform_topology: PlatformFlightTopology::Enabled,
        rest_dev_bypass: false,
    },
    RuntimeReceiver {
        file: "example-api/docker-compose.yml",
        service: "example-server",
        mode: RuntimeAuthMode::Es256,
        platform_topology: PlatformFlightTopology::NotApplicable,
        rest_dev_bypass: false,
    },
    RuntimeReceiver {
        file: "example-api/docker-compose.health-audit.yml",
        service: "ocr-sidecar",
        mode: RuntimeAuthMode::ControlledHs256,
        platform_topology: PlatformFlightTopology::Enabled,
        rest_dev_bypass: false,
    },
    RuntimeReceiver {
        file: "example-api/docker-compose.health-audit.yml",
        service: "example-server",
        mode: RuntimeAuthMode::ControlledHs256,
        platform_topology: PlatformFlightTopology::NotApplicable,
        rest_dev_bypass: false,
    },
    RuntimeReceiver {
        file: "example-api/docker-compose.yml.optimized",
        service: "ocr-sidecar",
        mode: RuntimeAuthMode::ControlledHs256,
        platform_topology: PlatformFlightTopology::Enabled,
        rest_dev_bypass: false,
    },
    RuntimeReceiver {
        file: "example-api/docker-compose.yml.optimized",
        service: "example-server",
        mode: RuntimeAuthMode::ControlledHs256,
        platform_topology: PlatformFlightTopology::NotApplicable,
        rest_dev_bypass: true,
    },
    RuntimeReceiver {
        file: "deploy/docker/docker-compose.sisfron-local.yml",
        service: "example-gateway",
        mode: RuntimeAuthMode::ControlledHs256,
        platform_topology: PlatformFlightTopology::Disabled,
        rest_dev_bypass: false,
    },
    RuntimeReceiver {
        file: "deploy/docker/docker-compose.sisfron-airgap.yml",
        service: "example-gateway",
        mode: RuntimeAuthMode::ControlledHs256,
        platform_topology: PlatformFlightTopology::Disabled,
        rest_dev_bypass: false,
    },
    RuntimeReceiver {
        file: "deploy/stamp/docker-compose.appliance.yml",
        service: "ocr-sidecar",
        mode: RuntimeAuthMode::ControlledHs256,
        platform_topology: PlatformFlightTopology::Disabled,
        rest_dev_bypass: false,
    },
];

const STATIC_BEARER_KEYS: &[&str] = &[
    "EXAMPLE_FLIGHT_SECRET",
    "EXAMPLE_FLIGHT_AUTHORIZATION",
    "EXAMPLE_FLIGHT_BEARER_TOKEN",
];
const PLATFORM_URL_VALUE: &str = "${EXAMPLE_PLATFORM_FLIGHT_URL:-grpc://example-server:8815}";
const PLATFORM_DISABLED_VALUE: &str = "disabled";
const FLIGHT_BROKER_ENV: &[(&str, &str)] = &[
    (
        "EXAMPLE_FLIGHT_AUTH_BROKER_URL",
        "${EXAMPLE_FLIGHT_AUTH_BROKER_URL:-}",
    ),
    (
        "EXAMPLE_FLIGHT_AUTH_BROKER_CLIENT_CERT",
        "${EXAMPLE_FLIGHT_AUTH_BROKER_CLIENT_CERT:-/secrets/flight-broker/client.crt}",
    ),
    (
        "EXAMPLE_FLIGHT_AUTH_BROKER_CLIENT_KEY",
        "${EXAMPLE_FLIGHT_AUTH_BROKER_CLIENT_KEY:-/secrets/flight-broker/client.key}",
    ),
    (
        "EXAMPLE_FLIGHT_AUTH_BROKER_CA",
        "${EXAMPLE_FLIGHT_AUTH_BROKER_CA:-/secrets/flight-broker/ca.pem}",
    ),
];
const HEALTH_AUDIT_COMPOSE_FILE: &str = "example-api/docker-compose.health-audit.yml";
const HEALTH_AUDIT_ALIGN_LOCAL_MARKER: &str = "Health Audit deliberately omits EXAMPLE_FLIGHT_URL";

#[derive(Clone, Debug)]
struct AnchorScope {
    alias: Option<String>,
    body: String,
}

#[derive(Clone, Debug)]
struct ServiceEnvScope {
    service: String,
    line: usize,
    body: String,
    service_body: String,
    merged_aliases: Vec<String>,
}

pub fn doctor_flight_runtime_auth(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut violations = Vec::new();
    let mut io_warnings = Vec::new();
    let mut files = HashMap::<&'static str, String>::new();

    for receiver in RECEIVERS {
        if files.contains_key(receiver.file) {
            continue;
        }
        let path = root.join(receiver.file);
        match read_text(&path, &mut io_warnings) {
            Some(body) => {
                files.insert(receiver.file, body);
            }
            None => push_violation(
                receiver.file,
                None,
                "runtime manifest is missing or unreadable".to_owned(),
                &mut warnings,
                &mut evidence,
                &mut violations,
            ),
        }
    }

    if let Some(compose) = files.get(HEALTH_AUDIT_COMPOSE_FILE) {
        let scopes = extract_service_env_scopes(compose)
            .into_iter()
            .map(|scope| (scope.service.clone(), scope))
            .collect::<HashMap<_, _>>();
        for forbidden in ["EXAMPLE_FLIGHT_AUTH_BROKER_", "/secrets/flight-broker"] {
            if compose.contains(forbidden) {
                push_violation(
                    HEALTH_AUDIT_COMPOSE_FILE,
                    None,
                    format!(
                        "standalone Health Audit must not depend on external Flight broker material `{forbidden}`"
                    ),
                    &mut warnings,
                    &mut evidence,
                    &mut violations,
                );
            }
        }

        let align_service = "example-align";
        if let Some(scope) = scopes.get(align_service) {
            for issue in health_audit_local_align_issues(compose, scope) {
                push_violation(
                    HEALTH_AUDIT_COMPOSE_FILE,
                    Some(scope.line),
                    format!("service `{align_service}`: {issue}"),
                    &mut warnings,
                    &mut evidence,
                    &mut violations,
                );
            }
        } else {
            push_violation(
                HEALTH_AUDIT_COMPOSE_FILE,
                None,
                format!("service `{align_service}` has no environment scope"),
                &mut warnings,
                &mut evidence,
                &mut violations,
            );
        }
    }

    for receiver in RECEIVERS {
        let Some(compose) = files.get(receiver.file) else {
            continue;
        };
        let Some(scope) = extract_service_env_scopes(compose)
            .into_iter()
            .find(|scope| scope.service == receiver.service)
        else {
            push_violation(
                receiver.file,
                None,
                format!("service `{}` has no environment scope", receiver.service),
                &mut warnings,
                &mut evidence,
                &mut violations,
            );
            continue;
        };

        for issue in receiver_issues(compose, &scope, *receiver) {
            push_violation(
                receiver.file,
                Some(scope.line),
                format!("service `{}`: {issue}", receiver.service),
                &mut warnings,
                &mut evidence,
                &mut violations,
            );
        }
    }

    let source_checks = [
        (
            "example-gateway/src/flight/client.rs",
            SourceCheck::PlatformUrl,
        ),
        ("example-gateway/src/main.rs", SourceCheck::GatewayStartup),
        (
            "example-api/example/flight/contracts.py",
            SourceCheck::PythonRefresh,
        ),
        ("example-align/src/types.rs", SourceCheck::AlignFlightOptIn),
        (
            "example-align/src/category_aco.rs",
            SourceCheck::AlignLocalFallback,
        ),
    ];
    for (rel, check) in source_checks {
        let path = root.join(rel);
        let Some(source) = read_text(&path, &mut io_warnings) else {
            push_violation(
                rel,
                None,
                "runtime auth source is missing or unreadable".to_owned(),
                &mut warnings,
                &mut evidence,
                &mut violations,
            );
            continue;
        };
        for issue in check.issues(&source) {
            push_violation(
                rel,
                None,
                issue,
                &mut warnings,
                &mut evidence,
                &mut violations,
            );
        }
    }

    for (rel, needles) in [
        (
            "office-parsers-rs/README.md",
            &[
                "EXAMPLE_FLIGHT_ISSUER=example-api",
                "EXAMPLE_FLIGHT_AUDIENCE=example",
                "flight:invoke",
                "tenant_id",
            ][..],
        ),
        (
            "example-platform/README.md",
            &["rotating credential source", "flight:invoke", "tenant_id"][..],
        ),
        (
            "example-platform/example-brains-node/README.md",
            &["short-lived access JWT", "flight:invoke", "fail-closed"][..],
        ),
        (
            "deploy/BRAINS_DEPLOY.md",
            &[
                "EXAMPLE_FLIGHT_REQUIRE_AUTH=true",
                "EXAMPLE_FLIGHT_ALLOW_LEGACY_HS256=false",
                "production workload",
                "rotating runtime credential source",
            ][..],
        ),
        (
            "deploy/profiles/health_audit.env",
            &[
                "HEALTH_AUDIT_AUTH_TENANT_SLUG=pcpsaude",
                "EXAMPLE_FLIGHT_REJECT_DEV_SECRET=1",
                "Flight remains local to the customer stack",
            ][..],
        ),
        (
            "example-gateway/docs/FLIGHT_AUTH_BROKER.md",
            &[
                "`EXAMPLE_FLIGHT_LOCAL_SIGNER_ENABLED=true`",
                "five-minute HS256 access token",
                "MUST NOT receive `flight:tenant-override`",
                "`example-align` | local LinUCB | no Flight credential",
            ][..],
        ),
    ] {
        let path = root.join(rel);
        let Some(body) = read_text(&path, &mut io_warnings) else {
            push_violation(
                rel,
                None,
                "runtime auth documentation is missing or unreadable".to_owned(),
                &mut warnings,
                &mut evidence,
                &mut violations,
            );
            continue;
        };
        for needle in needles {
            if !body.contains(needle) {
                push_violation(
                    rel,
                    None,
                    format!("missing credential-boundary evidence `{needle}`"),
                    &mut warnings,
                    &mut evidence,
                    &mut violations,
                );
            }
        }
    }

    warnings.extend(io_warnings);
    let entities = vec![json!({
        "doctor": "flight-runtime-auth",
        "capability_auth_mode": "ExistingAuth",
        "canonical_issuer": "example-api",
        "canonical_audience": "example",
        "required_permission": "flight:invoke",
        "runtime_matrix": RECEIVERS.iter().map(|receiver| json!({
            "file": receiver.file,
            "service": receiver.service,
            "mode": receiver.mode.label(),
            "platform_topology": receiver.platform_topology.label(),
        })).collect::<Vec<_>>(),
        "health_audit_local_signer": {
            "service": "ocr-sidecar",
            "algorithm": "HS256",
            "ttl_seconds": 300,
            "permissions": ["flight:invoke"],
            "external_broker": false,
        },
        "health_audit_local_flight_clients": [{
            "service": "example-align",
            "mode": "local-linucb",
            "flight_credentials": false,
            "activation_key": "EXAMPLE_FLIGHT_URL",
        }],
        "violations": violations,
    })];

    let summary = if warnings.is_empty() {
        format!(
            "checked {} ExistingAuth receiver(s); runtime modes, credentials, mounts, and platform URL are coherent",
            RECEIVERS.len()
        )
    } else {
        format!("{} Flight runtime-auth drift item(s)", evidence.len())
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_flight_runtime_auth"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.98 } else { 0.65 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn push_violation(
    path: &str,
    line: Option<usize>,
    detail: String,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
    violations: &mut Vec<serde_json::Value>,
) {
    warnings.push(format!("{path}: {detail}"));
    evidence.push(EvidenceItem {
        kind: "flight_runtime_auth_drift".to_owned(),
        path: path.to_owned(),
        line,
        detail: detail.clone(),
    });
    violations.push(json!({"file": path, "line": line, "detail": detail}));
}

fn receiver_issues(
    compose: &str,
    scope: &ServiceEnvScope,
    receiver: RuntimeReceiver,
) -> Vec<String> {
    let env = resolved_service_env(compose, scope);

    let mut issues = Vec::new();
    require_value(&env, "EXAMPLE_FLIGHT_REQUIRE_AUTH", "true", &mut issues);
    require_value(&env, "EXAMPLE_FLIGHT_ISSUER", "example-api", &mut issues);
    require_value(&env, "EXAMPLE_FLIGHT_AUDIENCE", "example", &mut issues);

    for key in STATIC_BEARER_KEYS {
        if env.contains_key(*key) {
            issues.push(format!("retired/static bearer key `{key}` is present"));
        }
    }

    match receiver.mode {
        RuntimeAuthMode::Es256 => {
            require_value(
                &env,
                "EXAMPLE_FLIGHT_ALLOW_LEGACY_HS256",
                "false",
                &mut issues,
            );
            require_value(
                &env,
                "EXAMPLE_JWT_PUBLIC_KEY_PATH",
                "/secrets/jwt/jwt_public.pem",
                &mut issues,
            );
            if !scope
                .service_body
                .contains("/jwt_public.pem:/secrets/jwt/jwt_public.pem:ro")
            {
                issues.push("ES256 receiver lacks a public-key-only read-only mount".to_owned());
            }
            if scope.service_body.contains(":/secrets/jwt:ro") {
                issues.push(
                    "receiver mounts the whole JWT directory instead of only the public key"
                        .to_owned(),
                );
            }
        }
        RuntimeAuthMode::ControlledHs256 => {
            require_value(
                &env,
                "EXAMPLE_FLIGHT_ALLOW_LEGACY_HS256",
                "true",
                &mut issues,
            );
            if !env
                .get("JWT_SECRET")
                .is_some_and(|value| value.starts_with("${JWT_SECRET:?"))
            {
                issues.push(
                    "controlled HS256 requires fail-closed `${JWT_SECRET:?…}` signing material"
                        .to_owned(),
                );
            }
        }
    }

    match receiver.platform_topology {
        PlatformFlightTopology::Enabled => {
            require_value(
                &env,
                "EXAMPLE_PLATFORM_FLIGHT_URL",
                PLATFORM_URL_VALUE,
                &mut issues,
            );
            if receiver.file == HEALTH_AUDIT_COMPOSE_FILE {
                require_value(
                    &env,
                    "EXAMPLE_FLIGHT_LOCAL_SIGNER_ENABLED",
                    "true",
                    &mut issues,
                );
                if env
                    .keys()
                    .any(|key| key.starts_with("EXAMPLE_FLIGHT_AUTH_BROKER_"))
                    || scope.service_body.contains("/secrets/flight-broker")
                {
                    issues.push(
                        "standalone Health Audit publisher must use the local signer without broker material"
                            .to_owned(),
                    );
                }
            } else {
                for (key, expected) in FLIGHT_BROKER_ENV {
                    require_value(&env, key, expected, &mut issues);
                }
                if !scope.service_body.contains("*flight-broker-volume")
                    && !scope.service_body.contains("/secrets/flight-broker:ro")
                {
                    issues.push(
                        "protected Flight receiver lacks a read-only mTLS broker secret mount"
                            .to_owned(),
                    );
                }
            }
        }
        PlatformFlightTopology::Disabled => {
            if env.get("EXAMPLE_PLATFORM_FLIGHT_URL").map(String::as_str)
                != Some(PLATFORM_DISABLED_VALUE)
            {
                issues.push(
                    "platform export must be explicitly disabled with `EXAMPLE_PLATFORM_FLIGHT_URL: disabled`"
                        .to_owned(),
                );
            }
        }
        PlatformFlightTopology::NotApplicable => {
            if env.contains_key("EXAMPLE_PLATFORM_FLIGHT_URL") {
                issues.push(
                    "non-publisher receiver must not define `EXAMPLE_PLATFORM_FLIGHT_URL`"
                        .to_owned(),
                );
            }
        }
    }
    if receiver.rest_dev_bypass {
        require_value(&env, "EXAMPLE_AUTH_DISABLED", "true", &mut issues);
    }

    issues
}

fn resolved_service_env(compose: &str, scope: &ServiceEnvScope) -> HashMap<String, String> {
    let anchors = extract_anchor_scopes(compose)
        .into_iter()
        .filter_map(|anchor| anchor.alias.map(|alias| (alias, parse_env(&anchor.body))))
        .collect::<HashMap<_, _>>();
    let mut env = HashMap::new();
    for alias in &scope.merged_aliases {
        if let Some(entries) = anchors.get(alias) {
            env.extend(entries.clone());
        }
    }
    env.extend(parse_env(&scope.body));
    env
}

fn health_audit_local_align_issues(compose: &str, scope: &ServiceEnvScope) -> Vec<String> {
    let env = resolved_service_env(compose, scope);
    let mut issues = Vec::new();
    for key in [
        "EXAMPLE_FLIGHT_URL",
        "EXAMPLE_FLIGHT_SECRET",
        "EXAMPLE_FLIGHT_AUTHORIZATION",
        "EXAMPLE_FLIGHT_BEARER_TOKEN",
        "EXAMPLE_FLIGHT_AUTH_BROKER_URL",
        "EXAMPLE_FLIGHT_AUTH_BROKER_CLIENT_CERT",
        "EXAMPLE_FLIGHT_AUTH_BROKER_CLIENT_KEY",
        "EXAMPLE_FLIGHT_AUTH_BROKER_CA",
    ] {
        if env.contains_key(key) {
            issues.push(format!(
                "local LinUCB mode must not define Flight client key `{key}`"
            ));
        }
    }
    if scope.service_body.contains("/secrets/flight-broker") {
        issues.push("local LinUCB mode must not mount Flight broker material".to_owned());
    }
    if !scope.service_body.contains(HEALTH_AUDIT_ALIGN_LOCAL_MARKER) {
        issues.push(format!(
            "local LinUCB opt-out lacks canonical marker `{HEALTH_AUDIT_ALIGN_LOCAL_MARKER}`"
        ));
    }
    issues
}

fn require_value(
    env: &HashMap<String, String>,
    key: &str,
    expected: &str,
    issues: &mut Vec<String>,
) {
    if env.get(key).map(String::as_str) != Some(expected) {
        issues.push(format!("`{key}` must equal `{expected}`"));
    }
}

#[derive(Clone, Copy)]
enum SourceCheck {
    PlatformUrl,
    GatewayStartup,
    PythonRefresh,
    AlignFlightOptIn,
    AlignLocalFallback,
}

impl SourceCheck {
    fn issues(self, source: &str) -> Vec<String> {
        match self {
            Self::PlatformUrl => platform_url_source_issues(source),
            Self::GatewayStartup => {
                let mut issues = Vec::new();
                let verifier =
                    source.find("FlightRequestAuthVerifier::new(flight_auth_config.clone())");
                let required = source.find("FlightRequestAuthVerifier::require_enabled");
                let bind = source.find("tokio::net::TcpListener::bind(&http_addr)");
                if !matches!((verifier, required, bind), (Some(v), Some(r), Some(b)) if v <= r && r < b)
                {
                    issues.push(
                        "gateway does not validate required Flight auth before HTTP bind"
                            .to_owned(),
                    );
                }
                issues.extend(event_worker_startup_issues(source));
                issues
            }
            Self::PythonRefresh => {
                let mut issues = Vec::new();
                for needle in [
                    "_SERVICE_BEARER_LOCK",
                    "expires_at - _SERVICE_BEARER_REFRESH_SKEW_SECONDS",
                    "_mint_generated_service_bearer",
                    "\"permissions\": [\"flight:invoke\"]",
                ] {
                    if !source.contains(needle) {
                        issues.push(format!("Python service JWT refresh lacks `{needle}`"));
                    }
                }
                if source.contains("@lru_cache") {
                    issues
                        .push("Python service bearer still uses a permanent lru_cache".to_owned());
                }
                issues
            }
            Self::AlignFlightOptIn => {
                let needle = "bandit_flight_url: std::env::var(\"EXAMPLE_FLIGHT_URL\").ok()";
                (!source.contains(needle))
                    .then(|| format!("align Flight activation is not explicit through `{needle}`"))
                    .into_iter()
                    .collect()
            }
            Self::AlignLocalFallback => {
                let mut issues = Vec::new();
                for needle in [
                    "if let Some(ref url) = self.config.bandit_flight_url",
                    "BanditBackend::Local(RefCell::new(LinUCB::new",
                ] {
                    if !source.contains(needle) {
                        issues.push(format!("align local LinUCB fallback lacks `{needle}`"));
                    }
                }
                issues
            }
        }
    }
}

fn platform_url_source_issues(source: &str) -> Vec<String> {
    let mut issues = Vec::new();
    let writer = function_segment(
        source,
        "pub async fn stream_quads_to_platform",
        "// ── GEPA Flight Client",
    );
    if !writer.contains("endpoint: &str")
        || !writer.contains("let endpoint = normalize_grpc_endpoint(endpoint.to_owned());")
        || writer.contains("platform_flight_endpoint()")
        || writer.contains("gepa_flight_endpoint")
    {
        issues.push(
            "platform quad writer is not bound exclusively to its caller-provided dedicated endpoint"
                .to_owned(),
        );
    }
    let resolver = function_segment(
        source,
        "pub fn platform_flight_endpoint",
        "/// Select a GEPA candidate",
    );
    if !resolver.contains("std::env::var(\"EXAMPLE_PLATFORM_FLIGHT_URL\")") {
        issues.push(
            "platform endpoint resolver does not read exact EXAMPLE_PLATFORM_FLIGHT_URL".to_owned(),
        );
    }
    if !resolver.contains("-> Option<String>")
        || !resolver.contains("eq_ignore_ascii_case(\"disabled\")")
    {
        issues.push(
            "platform endpoint resolver does not fail closed for absent or explicitly disabled configuration"
                .to_owned(),
        );
    }
    if ["127.0.0.1", "localhost", "unwrap_or", ".or_else("]
        .iter()
        .any(|needle| resolver.contains(needle))
    {
        issues.push("platform endpoint resolver contains a localhost/default fallback".to_owned());
    }
    issues
}

fn event_worker_startup_issues(source: &str) -> Vec<String> {
    let segmented = function_segment(
        source,
        "// Event Flight background worker",
        "// Redis Streams drain worker",
    );
    let block = if segmented.is_empty() {
        source
    } else {
        segmented
    };
    let gate = block.find("if let Some(platform_endpoint)");
    let resolver = block.find("platform_flight_endpoint()");
    let channel = block.find("init_event_channel()");
    let spawn = block.find("spawn_event_flight_worker(");
    let ordered = matches!(
        (gate, resolver, channel, spawn),
        (Some(g), Some(r), Some(c), Some(s)) if g <= r && r < c && c < s
    );
    let endpoint_passed = spawn.is_some_and(|start| {
        let call = &block[start..];
        let end = call.find(");").unwrap_or(call.len());
        call[..end].contains("platform_endpoint")
    });

    if ordered && endpoint_passed {
        Vec::new()
    } else {
        vec![
            "Event Flight worker is not gated by a configured dedicated platform endpoint"
                .to_owned(),
        ]
    }
}

fn function_segment<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let Some(start_at) = source.find(start) else {
        return "";
    };
    let tail = &source[start_at..];
    let end_at = tail.find(end).unwrap_or(tail.len());
    &tail[..end_at]
}

fn parse_env(block: &str) -> HashMap<String, String> {
    block
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("<<:") {
                return None;
            }
            let (key, value) = trimmed.split_once(':')?;
            if !key.chars().all(|character| {
                character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
            }) {
                return None;
            }
            Some((
                key.to_owned(),
                value.trim().trim_matches(['\'', '"']).to_owned(),
            ))
        })
        .collect()
}

fn extract_anchor_scopes(compose: &str) -> Vec<AnchorScope> {
    let lines: Vec<&str> = compose.lines().collect();
    let mut scopes = Vec::new();
    let mut index = 0usize;

    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim();
        if indent_of(line) == 0
            && trimmed.starts_with("x-")
            && trimmed.contains(':')
            && trimmed.contains("-env")
        {
            let alias = parse_anchor_alias(trimmed);
            let start = index + 1;
            index += 1;
            while index < lines.len() {
                let current = lines[index];
                if !current.trim().is_empty()
                    && !current.trim_start().starts_with('#')
                    && indent_of(current) == 0
                {
                    break;
                }
                index += 1;
            }
            scopes.push(AnchorScope {
                alias,
                body: lines[start..index].join("\n"),
            });
            continue;
        }
        index += 1;
    }
    scopes
}

fn parse_anchor_alias(header: &str) -> Option<String> {
    let after = &header[header.find('&')? + 1..];
    let alias = after
        .chars()
        .take_while(|character| {
            character.is_ascii_alphanumeric() || *character == '-' || *character == '_'
        })
        .collect::<String>();
    (!alias.is_empty()).then_some(alias)
}

fn extract_service_env_scopes(compose: &str) -> Vec<ServiceEnvScope> {
    let lines: Vec<&str> = compose.lines().collect();
    let mut scopes = Vec::new();
    let Some(services_index) = lines.iter().position(|line| line.trim_end() == "services:") else {
        return scopes;
    };

    let mut index = services_index + 1;
    while index < lines.len() {
        let line = lines[index];
        let indent = indent_of(line);
        if !line.trim().is_empty() && !line.trim_start().starts_with('#') && indent == 0 {
            break;
        }
        if indent == 2 && line.trim_end().ends_with(':') && !line.trim_start().starts_with('-') {
            let service = line.trim().trim_end_matches(':').to_owned();
            let service_line = index + 1;
            index += 1;
            let mut service_lines = Vec::new();
            while index < lines.len() {
                let current = lines[index];
                if !current.trim().is_empty() && indent_of(current) <= 2 {
                    break;
                }
                service_lines.push(current);
                index += 1;
            }
            if let Some((body, env_offset, merged_aliases)) =
                extract_environment_block(&service_lines)
            {
                scopes.push(ServiceEnvScope {
                    service,
                    line: service_line + env_offset,
                    body,
                    service_body: service_lines.join("\n"),
                    merged_aliases,
                });
            }
            continue;
        }
        index += 1;
    }
    scopes
}

fn extract_environment_block(lines: &[&str]) -> Option<(String, usize, Vec<String>)> {
    let mut index = 0usize;
    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim_start();
        if trimmed == "environment:" || trimmed.starts_with("environment:") {
            let env_indent = indent_of(line);
            let env_offset = index + 1;
            let mut body = Vec::new();
            let mut aliases = parse_aliases_from_line(trimmed);
            index += 1;
            while index < lines.len() {
                let current = lines[index];
                if !current.trim().is_empty() && indent_of(current) <= env_indent {
                    break;
                }
                aliases.extend(parse_aliases_from_line(current.trim_start()));
                body.push(current);
                index += 1;
            }
            return Some((body.join("\n"), env_offset, aliases));
        }
        index += 1;
    }
    None
}

fn parse_aliases_from_line(trimmed: &str) -> Vec<String> {
    let mut aliases = Vec::new();
    let mut rest = trimmed;
    while let Some(star) = rest.find('*') {
        let after = &rest[star + 1..];
        let alias = after
            .chars()
            .take_while(|character| {
                character.is_ascii_alphanumeric() || *character == '-' || *character == '_'
            })
            .collect::<String>();
        if !alias.is_empty() {
            aliases.push(alias);
        }
        rest = after;
    }
    aliases
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receiver_from(compose: &str, mode: RuntimeAuthMode) -> (ServiceEnvScope, RuntimeReceiver) {
        let scope = extract_service_env_scopes(compose)
            .into_iter()
            .find(|scope| scope.service == "receiver")
            .unwrap();
        let receiver = RuntimeReceiver {
            file: "compose.yml",
            service: "receiver",
            mode,
            platform_topology: PlatformFlightTopology::Disabled,
            rest_dev_bypass: false,
        };
        (scope, receiver)
    }

    #[test]
    fn controlled_hs256_inherited_from_anchor_is_clean_without_static_bearer() {
        let compose = r#"x-runtime-env: &runtime-env
  JWT_SECRET: ${JWT_SECRET:?JWT_SECRET required}
  EXAMPLE_FLIGHT_REQUIRE_AUTH: "true"
  EXAMPLE_FLIGHT_ALLOW_LEGACY_HS256: "true"
  EXAMPLE_FLIGHT_ISSUER: example-api
  EXAMPLE_FLIGHT_AUDIENCE: example
  EXAMPLE_PLATFORM_FLIGHT_URL: disabled
services:
  receiver:
    environment:
      <<: *runtime-env
"#;
        let (scope, receiver) = receiver_from(compose, RuntimeAuthMode::ControlledHs256);
        assert!(receiver_issues(compose, &scope, receiver).is_empty());
    }

    #[test]
    fn missing_required_mode_and_retired_secret_are_both_flagged() {
        let compose = r#"services:
  receiver:
    environment:
      EXAMPLE_FLIGHT_ALLOW_LEGACY_HS256: "true"
      EXAMPLE_FLIGHT_ISSUER: example-api
      EXAMPLE_FLIGHT_AUDIENCE: example
      JWT_SECRET: ${JWT_SECRET:?JWT_SECRET required}
      EXAMPLE_FLIGHT_SECRET: static-token
"#;
        let (scope, receiver) = receiver_from(compose, RuntimeAuthMode::ControlledHs256);
        let issues = receiver_issues(compose, &scope, receiver);
        assert!(issues.iter().any(|issue| issue.contains("REQUIRE_AUTH")));
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("retired/static bearer"))
        );
    }

    #[test]
    fn es256_requires_public_file_mount_and_rejects_directory_mount() {
        let compose = r#"services:
  receiver:
    environment:
      EXAMPLE_FLIGHT_REQUIRE_AUTH: "true"
      EXAMPLE_FLIGHT_ALLOW_LEGACY_HS256: "false"
      EXAMPLE_FLIGHT_ISSUER: example-api
      EXAMPLE_FLIGHT_AUDIENCE: example
      EXAMPLE_JWT_PUBLIC_KEY_PATH: /secrets/jwt/jwt_public.pem
    volumes:
      - ./secrets:/secrets/jwt:ro
"#;
        let (scope, receiver) = receiver_from(compose, RuntimeAuthMode::Es256);
        let issues = receiver_issues(compose, &scope, receiver);
        assert!(issues.iter().any(|issue| issue.contains("public-key-only")));
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("whole JWT directory"))
        );
    }

    #[test]
    fn platform_url_truth_rejects_gepa_endpoint_reuse() {
        let source = r#"
pub async fn stream_quads_to_platform() {
    let endpoint = gepa_flight_endpoint();
}
// ── GEPA Flight Client
pub fn platform_flight_endpoint() -> String {
    std::env::var("EXAMPLE_PLATFORM_FLIGHT_URL").unwrap()
}
/// Select a GEPA candidate
"#;
        let issues = platform_url_source_issues(source);
        assert!(issues.iter().any(|issue| issue.contains("exclusively")));
    }

    #[test]
    fn disabled_platform_export_requires_explicit_sentinel() {
        let compose = r#"services:
  receiver:
    environment:
      EXAMPLE_FLIGHT_REQUIRE_AUTH: "true"
      EXAMPLE_FLIGHT_ALLOW_LEGACY_HS256: "true"
      EXAMPLE_FLIGHT_ISSUER: example-api
      EXAMPLE_FLIGHT_AUDIENCE: example
      JWT_SECRET: ${JWT_SECRET:?JWT_SECRET required}
"#;
        let (scope, receiver) = receiver_from(compose, RuntimeAuthMode::ControlledHs256);
        let issues = receiver_issues(compose, &scope, receiver);
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("explicitly disabled")),
            "missing platform topology marker must fail closed: {issues:#?}"
        );

        let explicitly_disabled = compose.replace(
            "      JWT_SECRET: ${JWT_SECRET:?JWT_SECRET required}\n",
            "      JWT_SECRET: ${JWT_SECRET:?JWT_SECRET required}\n      EXAMPLE_PLATFORM_FLIGHT_URL: disabled\n",
        );
        let (scope, receiver) =
            receiver_from(&explicitly_disabled, RuntimeAuthMode::ControlledHs256);
        assert!(
            receiver_issues(&explicitly_disabled, &scope, receiver).is_empty(),
            "the disabled sentinel must be accepted as an explicit topology decision"
        );
    }

    #[test]
    fn enabled_platform_export_requires_mtls_broker_contract() {
        let compose = r#"x-runtime-env: &runtime-env
  JWT_SECRET: ${JWT_SECRET:?JWT_SECRET required}
  EXAMPLE_FLIGHT_REQUIRE_AUTH: "true"
  EXAMPLE_FLIGHT_ALLOW_LEGACY_HS256: "true"
  EXAMPLE_FLIGHT_ISSUER: example-api
  EXAMPLE_FLIGHT_AUDIENCE: example
  EXAMPLE_PLATFORM_FLIGHT_URL: ${EXAMPLE_PLATFORM_FLIGHT_URL:-grpc://example-server:8815}
  EXAMPLE_FLIGHT_AUTH_BROKER_URL: ${EXAMPLE_FLIGHT_AUTH_BROKER_URL:-}
  EXAMPLE_FLIGHT_AUTH_BROKER_CLIENT_CERT: ${EXAMPLE_FLIGHT_AUTH_BROKER_CLIENT_CERT:-/secrets/flight-broker/client.crt}
  EXAMPLE_FLIGHT_AUTH_BROKER_CLIENT_KEY: ${EXAMPLE_FLIGHT_AUTH_BROKER_CLIENT_KEY:-/secrets/flight-broker/client.key}
  EXAMPLE_FLIGHT_AUTH_BROKER_CA: ${EXAMPLE_FLIGHT_AUTH_BROKER_CA:-/secrets/flight-broker/ca.pem}
services:
  receiver:
    environment:
      <<: *runtime-env
"#;
        let scope = extract_service_env_scopes(compose)
            .into_iter()
            .find(|scope| scope.service == "receiver")
            .unwrap();
        let receiver = RuntimeReceiver {
            file: "compose.yml",
            service: "receiver",
            mode: RuntimeAuthMode::ControlledHs256,
            platform_topology: PlatformFlightTopology::Enabled,
            rest_dev_bypass: false,
        };
        let issues = receiver_issues(compose, &scope, receiver);
        assert!(
            issues.iter().any(|issue| issue.contains("mTLS broker")),
            "missing broker wiring must fail closed: {issues:#?}"
        );

        let complete = compose.replace(
            "    environment:\n      <<: *runtime-env\n",
            "    environment:\n      <<: *runtime-env\n    volumes:\n      - /opt/flight-broker:/secrets/flight-broker:ro\n",
        );
        let scope = extract_service_env_scopes(&complete)
            .into_iter()
            .find(|scope| scope.service == "receiver")
            .unwrap();
        assert!(receiver_issues(&complete, &scope, receiver).is_empty());
    }

    #[test]
    fn health_audit_gateway_rejects_external_broker_material() {
        let compose = r#"x-runtime-env: &runtime-env
  JWT_SECRET: ${JWT_SECRET:?JWT_SECRET required}
  EXAMPLE_FLIGHT_REQUIRE_AUTH: "true"
  EXAMPLE_FLIGHT_ALLOW_LEGACY_HS256: "true"
  EXAMPLE_FLIGHT_ISSUER: example-api
  EXAMPLE_FLIGHT_AUDIENCE: example
  EXAMPLE_PLATFORM_FLIGHT_URL: ${EXAMPLE_PLATFORM_FLIGHT_URL:-grpc://example-server:8815}
  EXAMPLE_FLIGHT_AUTH_BROKER_URL: ${EXAMPLE_FLIGHT_AUTH_BROKER_URL:-}
  EXAMPLE_FLIGHT_AUTH_BROKER_CLIENT_CERT: ${EXAMPLE_FLIGHT_AUTH_BROKER_CLIENT_CERT:-/secrets/flight-broker/client.crt}
  EXAMPLE_FLIGHT_AUTH_BROKER_CLIENT_KEY: ${EXAMPLE_FLIGHT_AUTH_BROKER_CLIENT_KEY:-/secrets/flight-broker/client.key}
  EXAMPLE_FLIGHT_AUTH_BROKER_CA: ${EXAMPLE_FLIGHT_AUTH_BROKER_CA:-/secrets/flight-broker/ca.pem}
services:
  ocr-sidecar:
    environment:
      <<: *runtime-env
    volumes:
      - /opt/flight-broker:/secrets/flight-broker:ro
"#;
        let scope = extract_service_env_scopes(compose)
            .into_iter()
            .find(|scope| scope.service == "ocr-sidecar")
            .unwrap();
        let receiver = RuntimeReceiver {
            file: "example-api/docker-compose.health-audit.yml",
            service: "ocr-sidecar",
            mode: RuntimeAuthMode::ControlledHs256,
            platform_topology: PlatformFlightTopology::Enabled,
            rest_dev_bypass: false,
        };

        let issues = receiver_issues(compose, &scope, receiver);
        assert!(issues.iter().any(|issue| issue.contains("local signer")));
    }

    #[test]
    fn health_audit_gateway_accepts_local_rotating_signer() {
        let compose = r#"x-runtime-env: &runtime-env
  JWT_SECRET: ${JWT_SECRET:?JWT_SECRET required}
  EXAMPLE_FLIGHT_REQUIRE_AUTH: "true"
  EXAMPLE_FLIGHT_ALLOW_LEGACY_HS256: "true"
  EXAMPLE_FLIGHT_ISSUER: example-api
  EXAMPLE_FLIGHT_AUDIENCE: example
  EXAMPLE_PLATFORM_FLIGHT_URL: ${EXAMPLE_PLATFORM_FLIGHT_URL:-grpc://example-server:8815}
  EXAMPLE_FLIGHT_LOCAL_SIGNER_ENABLED: "true"
services:
  ocr-sidecar:
    environment:
      <<: *runtime-env
"#;
        let scope = extract_service_env_scopes(compose)
            .into_iter()
            .find(|scope| scope.service == "ocr-sidecar")
            .unwrap();
        let receiver = RuntimeReceiver {
            file: "example-api/docker-compose.health-audit.yml",
            service: "ocr-sidecar",
            mode: RuntimeAuthMode::ControlledHs256,
            platform_topology: PlatformFlightTopology::Enabled,
            rest_dev_bypass: false,
        };

        assert!(
            receiver_issues(compose, &scope, receiver).is_empty(),
            "the customer-local signer must remain valid"
        );
    }

    #[test]
    fn health_audit_align_local_contract_rejects_static_flight_client() {
        let compose = r#"services:
  example-align:
    environment:
      # Health Audit deliberately omits EXAMPLE_FLIGHT_URL in the safe form.
      EXAMPLE_FLIGHT_URL: grpc://api:8815
      EXAMPLE_FLIGHT_SECRET: static-secret
    volumes:
      - /opt/flight-broker/align:/secrets/flight-broker/align:ro
"#;
        let scope = extract_service_env_scopes(compose)
            .into_iter()
            .find(|scope| scope.service == "example-align")
            .unwrap();

        let issues = health_audit_local_align_issues(compose, &scope);
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("`EXAMPLE_FLIGHT_URL`"))
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("`EXAMPLE_FLIGHT_SECRET`"))
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("must not mount Flight broker material"))
        );
    }

    #[test]
    fn platform_url_truth_rejects_localhost_fallback() {
        let source = r#"
pub async fn stream_quads_to_platform(endpoint: &str) {
    let endpoint = normalize_grpc_endpoint(endpoint.to_owned());
}
// ── GEPA Flight Client
pub fn platform_flight_endpoint() -> Option<String> {
    std::env::var("EXAMPLE_PLATFORM_FLIGHT_URL")
        .ok()
        .map(normalize_grpc_endpoint)
        .or_else(|| Some("grpc://127.0.0.1:8815".to_owned()))
}
/// Select a GEPA candidate
"#;
        let issues = platform_url_source_issues(source);
        assert!(
            issues.iter().any(|issue| issue.contains("fallback")),
            "localhost fallback must not pass the production platform route: {issues:#?}"
        );
    }

    #[test]
    fn gateway_startup_requires_platform_endpoint_gate_around_event_worker() {
        let source = r#"
FlightRequestAuthVerifier::new(flight_auth_config.clone());
FlightRequestAuthVerifier::require_enabled(&flight_auth_config);
tokio::net::TcpListener::bind(&http_addr);

if let Some(rx) = one_file_gateway::events::init_event_channel() {
    one_file_gateway::events::worker::spawn_event_flight_worker(rx, endpoint);
}
"#;
        let issues = SourceCheck::GatewayStartup.issues(source);
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("Event Flight worker")),
            "an unconditional worker spawn must be rejected: {issues:#?}"
        );
    }

    #[test]
    fn workspace_existing_auth_runtime_matrix_is_clean() {
        // Reads the sibling repos' compose manifests and Flight clients; every
        // one is "missing or unreadable" in a standalone checkout.
        let Some(root) = crate::test_workspace::workspace_root_with("example-api") else {
            eprintln!("skipped: no workspace repos beside this repo");
            return;
        };
        let envelope = doctor_flight_runtime_auth(&root);
        assert!(envelope.warnings.is_empty(), "{:#?}", envelope.warnings);
    }

    #[test]
    fn workspace_doctor_reports_health_audit_align_as_local_only() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("leio-code lives under workspace root");
        let envelope = doctor_flight_runtime_auth(root);
        let clients = envelope.entities[0]
            .get("health_audit_local_flight_clients")
            .and_then(serde_json::Value::as_array)
            .expect("doctor must expose the disabled-client inventory");

        assert!(clients.iter().any(|client| {
            client.get("service").and_then(serde_json::Value::as_str) == Some("example-align")
                && client.get("mode").and_then(serde_json::Value::as_str) == Some("local-linucb")
                && client
                    .get("flight_credentials")
                    .and_then(serde_json::Value::as_bool)
                    == Some(false)
        }));
    }
}
