use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use leio_code::doctors::{
    doctor_names, platform_runtime_trust_boundary::doctor_platform_runtime_trust_boundary,
};

static FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

const AUTH_FIXTURE: &str = r#"
enum AuthMode { Required(DecodingKey), DisabledForDevelopment }
struct AuthConfig { mode: AuthMode }
struct AuthContext { tenant_id: String, token_id: Option<String> }
struct ClaimsWire {
    sub: String,
    tenant_id: Option<String>,
    namespace: Option<String>,
    role: Option<String>,
    roles: Vec<String>,
    email: Option<String>,
    permissions: Vec<String>,
    jti: Option<String>,
}
impl AuthConfig {
    fn from_settings(disabled: Option<&str>, key_path: Option<&Path>) -> Result<Self, AuthConfigError> {
        if disabled == Some("true") { return Ok(Self::disabled_for_development()); }
        let path = key_path.ok_or(AuthConfigError::MissingPublicKeyPath)?;
        let pem = std::fs::read(path)?;
        Self::required_from_pem(&pem)
    }
    fn required_from_pem(pem: &[u8]) -> Result<Self, AuthConfigError> {
        let decoding_key = DecodingKey::from_ec_pem(pem)
            .map_err(|source| AuthConfigError::InvalidPublicKey { source })?;
        Ok(Self { mode: AuthMode::Required(decoding_key), })
    }
    fn authenticate(&self, headers: &HeaderMap) -> Result<AuthContext, AuthError> {
        let AuthMode::Required(key) = &self.mode else { return Ok(AuthContext::local_development()); };
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
    }
}
impl AuthContext {
    fn from_verified_claims(claims: ClaimsWire) -> Result<Self, AuthError> {
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
    }
}
fn normalize_tenant(tenant_id: Option<String>, namespace: Option<String>) -> Result<String, AuthError> {
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
}
async fn jwt_auth_middleware(
    State(auth): State<AuthConfig>,
    mut request: Request,
    next: Next,
) -> Response {
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
}
// Decoy only: if disabled.is_some() { allow_everything(); }
"#;

const COMPOSE_FIXTURE: &str = r#"
services:
  redis:
    image: redis
  example-server-data-init:
    image: busybox:1.36.1
    user: "0:0"
    restart: "no"
    command:
      - /bin/sh
      - -ec
      - |
        mkdir -p /data/artifacts
        chown 1000:0 /data/artifacts
        chmod 0770 /data/artifacts
    volumes:
      - shared_data_volume:/data
  example-server-data-write-smoke:
    image: busybox:1.36.1
    user: "1000:0"
    restart: "no"
    depends_on:
      example-server-data-init:
        condition: service_completed_successfully
    command:
      - /bin/sh
      - -ec
      - |
        probe=/data/artifacts/.example-server-write-smoke
        touch "$$probe"
        rm -f "$$probe"
    volumes:
      - shared_data_volume:/data
  example-align:
    environment:
      EXAMPLE_JWT_PUBLIC_KEY_PATH: /secrets/jwt/jwt_public.pem
      REDIS_SERVICE_HOST: redis
      REDIS_SERVICE_PORT: "6379"
      REDIS_PASSWORD: ${REDIS_PASSWORD:-}
    depends_on:
      redis:
        condition: service_healthy
    volumes:
      - ${JWT_KEYPAIR_DIR:-../deploy/secrets}/jwt_public.pem:/secrets/jwt/jwt_public.pem:ro
  example-server:
    command:
      - --http-addr
      - 0.0.0.0:8082
      - --artifacts-root
      - /data/artifacts
    environment:
      EXAMPLE_JWT_PUBLIC_KEY_PATH: /secrets/jwt/jwt_public.pem
      REDIS_SERVICE_HOST: redis
      REDIS_SERVICE_PORT: "6379"
      REDIS_PASSWORD: ${REDIS_PASSWORD:-}
    depends_on:
      redis:
        condition: service_healthy
      example-server-data-write-smoke:
        condition: service_completed_successfully
    volumes:
      - shared_data_volume:/data
      - ${JWT_KEYPAIR_DIR:-../deploy/secrets}/jwt_public.pem:/secrets/jwt/jwt_public.pem:ro
# EXAMPLE_AUTH_DISABLED: true
"#;

const ALIGN_DOCKERFILE_FIXTURE: &str = r#"
FROM rust:bookworm AS builder
RUN cargo build --release
FROM debian:bookworm-slim
RUN useradd --uid 1000 appuser
COPY --from=builder /app/example-align /usr/local/bin/example-align
USER appuser
ENTRYPOINT ["example-align"]
"#;

const PLATFORM_DOCKERFILE_FIXTURE: &str = r#"
FROM rust:bookworm AS builder
RUN cargo build --release
FROM debian:bookworm-slim
RUN useradd --uid 1000 appuser
COPY --from=builder /app/example-server /usr/local/bin/example-server
USER appuser
ENTRYPOINT ["example-server"]
"#;

const ALIGN_API_FIXTURE: &str = r#"
pub fn build_router(state: Arc<AppState>, jobs: Arc<JobsApiState>, auth: AuthConfig) -> Router {
    let protected = Router::new()
        .route("/align", post(align_handler))
        .fallback_service(static_service)
        .layer(axum::middleware::from_fn_with_state(auth.clone(), crate::auth::jwt_auth_middleware));
    Router::new()
        .route("/health", get(health_handler))
        .route("/ready", get(ready_handler))
        .route("/match", post(match_handler))
        .merge(protected)
}
async fn match_handler(
    State(state): State<Arc<AppState>>,
    req: Request,
) -> axum::response::Response {
    let content_type = req
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if content_type.contains("multipart/form-data") {
        match Multipart::from_request(req, &state).await {
            Ok(multipart) => match_multipart(state, multipart).await,
            Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
        }
    } else {
        (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "public /match accepts multipart/form-data uploads only",
        )
            .into_response()
    }
}
async fn match_multipart(state: Arc<AppState>, multipart: Multipart) -> axum::response::Response {
    align_to_rdf_response(&state, source_bytes, target_bytes).await
}
/* Router::new().route("/comment-only-admin", get(admin_handler)); */
#[cfg(test)]
mod tests {
    fn decoy() {
        Router::new().route("/test-only-admin", get(admin_handler));
        reqwest::get("http://127.0.0.1/private");
        tokio::fs::read("/tmp/private");
    }
}
"#;

const SERVER_PREFIX: &str = r#"
pub(crate) struct ServerState {
    flight_ingest: Arc<crate::flight_ingest::FlightIngestState>,
    align_url: String,
    ocr_url: String,
    discovery: discovery::DiscoveryHandle,
    adbc_policy: db::AdbcPolicy,
    db_executor: Arc<dyn db::DbExecutor>,
    pub(crate) tenant_states: Arc<crate::tenant_state::TenantStateRegistry>,
}
#[derive(Serialize)]
struct AuditRecord { request_id: String }
fn tenant_request_dir(root: &Path, tenant_id: &str, request_id: &str) -> Result<PathBuf, Error> {
    validate_path_component(tenant_id)?;
    validate_path_component(request_id)?;
    Ok(root.join("tenants").join(tenant_id).join(request_id))
}
fn tenant_db_operation_dir(root: &Path, tenant_id: &str, operation_id: &str) -> Result<PathBuf, Error> {
    validate_path_component(tenant_id)?;
    validate_path_component(operation_id)?;
    Ok(root.join("tenants").join(tenant_id).join("db").join(operation_id))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DbSchemaIngestRequest { source: db::DbSourceRequest }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DbAboxMergeRequest { tables: Vec<Table>, options: Option<db::MergeOptions> }
fn db_error(status: StatusCode, message: &'static str) -> DbHttpError { todo!() }
fn map_db_policy_error(error: db::DbPolicyError) -> DbHttpError {
    match error {
        db::DbPolicyError::InvalidRequest => db_error(StatusCode::BAD_REQUEST, "Invalid database request"),
        _ => db_error(StatusCode::SERVICE_UNAVAILABLE, "Database ingestion unavailable"),
    }
}
fn authorize_db_operation(auth_context: &auth::AuthContext, permission: &str) -> bool {
    ["admin", "super_admin", "superadmin"].iter().any(|role| auth_context.has_role(role))
        || (auth_context.has_role("service") && auth_context.has_permission(permission))
}
async fn ingest_db_schema(
    State(state): State<Arc<ServerState>>,
    Extension(auth_context): Extension<auth::AuthContext>,
) {
    if !authorize_db_operation(&auth_context, "platform:db:ingest") {
        return Err(db_error(axum::http::StatusCode::FORBIDDEN, "Forbidden"));
    }
    let source = state.adbc_policy.resolve(&request.source).map_err(map_db_policy_error)?;
    let redacted = source.redacted().clone();
    let operation_id = Uuid::new_v4().simple().to_string();
    let sanitized_error = source.sanitize_error(&error);
    tracing::error!(source = ?redacted, error = %sanitized_error);
    db_error(StatusCode::BAD_GATEWAY, "Database operation failed");
}
async fn merge_db_abox(
    State(state): State<Arc<ServerState>>,
    Extension(auth_context): Extension<auth::AuthContext>,
) {
    if !authorize_db_operation(&auth_context, "platform:db:merge") {
        return Err(db_error(axum::http::StatusCode::FORBIDDEN, "Forbidden"));
    }
    let formats = request.options.unwrap_or_default().validate().map_err(map_db_policy_error)?;
    let operation_id = Uuid::new_v4().simple().to_string();
    let output = state.adbc_policy.output_context(&auth_context.tenant_id, &operation_id)?;
    state.db_executor.merge_abox(request.tables, formats, output).await;
    db_error(StatusCode::BAD_GATEWAY, "Database operation failed");
}
async fn run(self) {
    let state = ServerState {
        flight_ingest: Arc::new(crate::flight_ingest::FlightIngestState {}),
        align_url: self.config.align_url.clone(),
        ocr_url: self.config.ocr_url.clone(),
        discovery: discovery::start_discovery(self.config.http_addr),
    };
    let flight_state = state.flight_ingest.clone();
    let _service = crate::flight_ingest::flight_service(flight_state);
    let protected = Router::new()
        .route("/api/induction/run", post(run_induction))
        .route("/api/induction/job", post(events::post_induction_job))
        .layer(axum::middleware::from_fn_with_state(
            self.auth.clone(),
            auth::jwt_auth_middleware,
        ))
        .with_state(state.clone());
    let app = public.merge(protected);
}
async fn discovery_state(State(state): State<Arc<ServerState>>) {
    let diagnostics = discovery::resolve_sidecars_with_diagnostics(
        &state.http_client,
        &state.discovery,
    )
    .await;
    observe(diagnostics);
}
"#;

const DB_FIXTURE: &str = r#"
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DbSourceRequest {
    pub name: String,
    pub dialect: String,
    pub driver: String,
    pub dsn: String,
    pub username: Option<String>,
    pub password: Option<String>,
}
#[derive(Serialize)]
pub struct RedactedDbSource {
    pub name: String,
    pub dialect: String,
    pub driver: String,
}
#[derive(Serialize)]
pub struct SchemaIngestResult { pub source: RedactedDbSource }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeOptions { pub formats: Vec<String> }
#[derive(Serialize)]
pub struct MergeResult {
    pub operation_id: String,
    pub parquet_files: Vec<String>,
    pub arrow_ipc_files: Vec<String>,
}
pub struct AdbcPolicy {
    allowed_drivers: Arc<BTreeSet<String>>,
    artifacts_root: Option<PathBuf>,
}
impl AdbcPolicy {
    fn from_settings(allowed_drivers: Option<&str>, artifacts_root: Option<PathBuf>) -> Result<Self, DbPolicyError> {
        let raw = allowed_drivers.unwrap_or_default().trim();
        if raw.is_empty() {
            return Ok(Self { allowed_drivers: Arc::new(BTreeSet::new()), artifacts_root });
        }
        let Some(root) = artifacts_root else { return Err(DbPolicyError::InvalidConfiguration); };
        Ok(Self { allowed_drivers: Arc::new(raw.split(',').map(str::to_owned).collect()), artifacts_root: Some(root) })
    }
    fn resolve(&self, source: &DbSourceRequest) -> Result<ResolvedDbSource, DbPolicyError> {
        if self.allowed_drivers.is_empty() { return Err(DbPolicyError::Disabled); }
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
    }
}
impl MergeOptions {
    fn validate(&self) -> Result<ValidatedMergeFormats, DbPolicyError> {
        for format in &self.formats {
            match format.as_str() {
                "parquet" | "arrow_ipc" => {}
                _ => return Err(DbPolicyError::InvalidRequest),
            }
        }
        Ok(ValidatedMergeFormats::new())
    }
}
fn safe_adbc_load_flags() -> u32 {
    adbc_core::LOAD_FLAG_DEFAULT & !adbc_core::LOAD_FLAG_ALLOW_RELATIVE_PATHS
}
fn ingest_schema_blocking(request: &ResolvedDbSource) {
    ManagedDriver::load_from_name(
        request.driver.clone(),
        None,
        AdbcVersion::V110,
        safe_adbc_load_flags(),
        None,
    );
}
fn output_context(root: &Path, tenant_id: &str, operation_id: &str) {
    validate_path_component(tenant_id);
    validate_path_component(operation_id);
    open_tenant_db_operation_dir(root, tenant_id, operation_id, true);
}
fn valid_driver_token(driver: &str) -> bool {
    let bytes = driver.as_bytes();
    (1..=128).contains(&bytes.len()) && bytes.iter().all(u8::is_ascii_alphanumeric)
}
fn sanitize_error(&self, error: &DbExecutorError) -> String {
    match error {
        DbExecutorError::Failed(_) => "database driver operation failed".to_string(),
        DbExecutorError::ArtifactsUnavailable(_) => {
            "database artifact operation unavailable".to_string()
        }
    }
}
fn table_output_stem(table: &TableSchema) -> String {
    let mut slug = String::with_capacity(64);
    slug.push(table.name.as_bytes()[0].to_ascii_lowercase() as char);
    if slug.is_empty() { slug.push_str("table"); }
    let metadata = serde_json::to_vec(&(table.catalog.as_deref(), table.schema.as_deref(), table.name.as_str())).unwrap();
    let digest = Sha256::digest(metadata);
    let suffix = digest[..8].iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    format!("{slug}-{suffix}")
}
fn merge_abox_blocking(tables: &[TableSchema], formats: &ValidatedMergeFormats, output: &DbOutputContext) {
    let operation_dir = open_tenant_db_operation_dir(&output.root, &output.tenant_id, &output.operation_id, true);
    for table in tables { let name = table_output_stem(table); create_artifact_in_request(&operation_dir, &name); }
}
#[cfg(test)]
mod tests {
    const BAD: &str = "entrypoint driver_search_paths output_dir LOAD_FLAG_ALLOW_RELATIVE_PATHS";
}
"#;

const MAIN_FIXTURE: &str = r#"
async fn main() -> Result<()> {
    let auth = AuthConfig::from_env()?;
    let adbc_policy = AdbcPolicy::from_env(config.artifacts_root.clone())?;
    let server = ExampleServer::new(config, registry, auth, adbc_policy);
    server.run().await
}
"#;

const EVENTS_FIXTURE: &str = r#"
fn job_event_fields(tenant_id: &str, job_id: &str, status: &str) -> [(&'static str, String); 4] {
    [
        ("job_id", job_id.to_owned()),
        ("tenant_id", tenant_id.to_owned()),
        ("service", "platform".to_owned()),
        ("status", status.to_owned()),
    ]
}
async fn publish_job_event(tenant_id: &str, job_id: &str, status: &str) {
    let fields = job_event_fields(tenant_id, job_id, status);
}
pub(crate) async fn post_induction_job(
    State(state): State<Arc<ServerState>>,
    Extension(auth): Extension<auth::AuthContext>,
) { let tenant_id = auth.tenant_id.clone(); let tenant_state = state.tenant_states.get_or_create(&tenant_id).await; publish_job_event(&tenant_id, &job_id, "succeeded").await; }
pub(crate) async fn get_induction_job(
    State(state): State<Arc<ServerState>>,
    Extension(auth): Extension<auth::AuthContext>,
) { state.tenant_states.get_or_create(&auth.tenant_id).await; }
"#;

const ALIGN_EVENTS_FIXTURE: &str = r#"
fn job_event_fields(tenant_id: &str, job_id: &str, status: &str) -> [(&'static str, String); 4] {
    [
        ("job_id", job_id.to_owned()),
        ("tenant_id", tenant_id.to_owned()),
        ("service", "align".to_owned()),
        ("status", status.to_owned()),
    ]
}
async fn publish_job_event(tenant_id: &str, job_id: &str, status: &str) {
    for (field, value) in job_event_fields(tenant_id, job_id, status) { command.arg(field).arg(value); }
}
fn spawn_worker() { publish_job_event(&tenant_id, &job_id, stream_status).await; }
"#;

const REDIS_FIXTURE: &str = r#"
def xread_for_job(
    stream_key: str,
    *,
    tenant_id: str,
    job_id: str,
    timeout_ms: int = 30_000,
    last_id: str = "$",
):
    cursor = last_id
    raw = client.xread({stream_key: cursor}, count=100, block=750)
    for entry_id, fields in entries:
        cursor = entry_id.decode() if isinstance(entry_id, bytes) else str(entry_id)
        decoded = decode(fields)
        if decoded.get("tenant_id") == tenant_id and decoded.get("job_id") == job_id:
            return decoded
"#;

const ROUTER_FIXTURE: &str = r#"
pub(crate) fn inbound_authorization(headers: &HeaderMap) -> Option<HeaderValue> {
    headers.get(axum::http::header::AUTHORIZATION).cloned()
}
pub(crate) fn apply_authorization(
    request: reqwest::RequestBuilder,
    authorization: Option<&HeaderValue>,
) -> reqwest::RequestBuilder {
    match authorization {
        Some(value) => request.header(reqwest::header::AUTHORIZATION, value.as_bytes()),
        None => request,
    }
}
"#;

const TENANT_HANDLERS: &[&str] = &[
    "get_ingest_artifact",
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
    "get_navigator_node",
    "import_tbox_from_turtle",
    "list_tboxes",
    "get_tbox_graph",
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

const PROXY_TUNE_FIXTURE: &str = "async fn proxy_tune(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>, headers: HeaderMap) { let base = state.align_url.clone(); let url = base; let authorization = crate::router::inbound_authorization(&headers); let _request = crate::router::apply_authorization(state.http_client.post(url), authorization.as_ref()); }";

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

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let id = FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "leio-platform-runtime-trust-boundary-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".leio-code")).expect("create fixture root");
        let fixture = Self { root };
        fixture.write(
            ".leio-code/config.toml",
            "workspace_profile = \"example\"\n",
        );
        fixture.write("example-platform/example-server/src/auth.rs", AUTH_FIXTURE);
        fixture.write("example-align/src/auth.rs", AUTH_FIXTURE);
        fixture.write("example-api/docker-compose.yml", COMPOSE_FIXTURE);
        fixture.write(
            "example-api/docker-compose.health-audit.yml",
            COMPOSE_FIXTURE,
        );
        fixture.write("example-align/Dockerfile", ALIGN_DOCKERFILE_FIXTURE);
        fixture.write(
            "example-platform/Dockerfile.server",
            PLATFORM_DOCKERFILE_FIXTURE,
        );
        fixture.write("example-align/src/api.rs", ALIGN_API_FIXTURE);
        fixture.write("example-platform/example-server/src/db.rs", DB_FIXTURE);
        fixture.write("example-platform/example-server/src/main.rs", MAIN_FIXTURE);
        fixture.write(
            "example-platform/example-server/src/events.rs",
            EVENTS_FIXTURE,
        );
        fixture.write("example-align/src/job_queue.rs", ALIGN_EVENTS_FIXTURE);
        fixture.write("example-api/example/core/redis_client.py", REDIS_FIXTURE);
        fixture.write(
            "example-platform/example-server/src/router.rs",
            ROUTER_FIXTURE,
        );
        fixture.write(
            "example-platform/example-server/src/lib.rs",
            &server_fixture(),
        );
        fixture.write(
            "example-api/example/agents/tasks.py",
            &python_tasks_fixture(),
        );
        fixture
    }

    fn write(&self, rel: &str, body: &str) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().expect("fixture parent")).expect("create fixture parent");
        fs::write(path, body).expect("write fixture");
    }

    fn replace_once(&self, rel: &str, from: &str, to: &str) {
        let path = self.root.join(rel);
        let body = fs::read_to_string(&path).expect("read fixture for mutation");
        assert!(
            body.contains(from),
            "mutation anchor `{from}` missing from {rel}"
        );
        fs::write(path, body.replacen(from, to, 1)).expect("write mutated fixture");
    }

    fn warnings(&self) -> Vec<String> {
        doctor_platform_runtime_trust_boundary(&self.root).warnings
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn server_fixture() -> String {
    let mut source = SERVER_PREFIX.to_string();
    for name in TENANT_HANDLERS {
        if *name == "get_ingest_artifact" {
            source.push_str(
                "async fn get_ingest_artifact(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>) { open_tenant_artifact(root, &auth_context.tenant_id, &id, filename); }\n",
            );
        } else if *name == "run_induction" {
            source.push_str(
                r#"
async fn run_induction(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>) {
    let mut triples = body.triples;
    if let Some(ttl) = body.rdf_ttl {
        let parsed = parse_rdf_triples_as_core_facts(&ttl).map_err(bad_request)?;
        triples.extend(parsed);
    }
    if triples.is_empty() { return Err(bad_request("triples required")); }
    let tenant_state = state.tenant_states.get_or_create(&auth_context.tenant_id).await;
    let mut st = tenant_state.induction.write().await;
    let candidate_config = st.config.clone();
    let mut candidate_core = ExampleCore::with_config(candidate_config.clone());
    for (s, p, o) in &triples { candidate_core.ingest(s.clone(), p.clone(), o.clone()); }
    let facts = std::mem::take(&mut candidate_core.triples);
    let (core, result) = run_core_join_closure_with_timeout(candidate_config, facts, body.axioms, timeout)
        .await
        .map_err(internal_error)?;
    let metrics = core.compute_metrics(&result);
    let (owl2, class_names) = core.export_owl2(&st.owl2_config).unwrap_or_default();
    st.core = core;
    st.last_result = Some(result);
    st.last_metrics = Some(metrics);
    st.last_owl2 = Some(owl2);
    st.last_class_names = class_names;
}
"#,
            );
        } else if *name == "import_ttl" {
            source.push_str(
                r#"
async fn import_ttl(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>) {
    let all_triples = parse_rdf_triples_as_core_facts(&body.ttl).map_err(bad_request)?;
    if all_triples.is_empty() { return Err(bad_request("triples required")); }
    let tenant_state = state.tenant_states.get_or_create(&auth_context.tenant_id).await;
    let mut st = tenant_state.induction.write().await;
    let mut candidate_config = st.config.clone();
    let mut candidate_owl2_config = st.owl2_config.clone();
    if let Some(iri) = body.base_iri {
        candidate_config.base_iri = iri.clone();
        candidate_owl2_config.base_iri = iri;
    }
    let mut candidate_core = ExampleCore::with_config(candidate_config.clone());
    for (s, p, o) in &all_triples { candidate_core.ingest(s.clone(), p.clone(), o.clone()); }
    let facts = std::mem::take(&mut candidate_core.triples);
    let (core, result) = run_core_join_closure_with_timeout(candidate_config.clone(), facts, axioms, timeout)
        .await
        .map_err(internal_error)?;
    let metrics = core.compute_metrics(&result);
    let (owl2, class_names) = core.export_owl2(&candidate_owl2_config).unwrap_or_default();
    st.config = candidate_config;
    st.owl2_config = candidate_owl2_config;
    st.core = core;
    st.last_result = Some(result);
    st.last_metrics = Some(metrics);
    st.last_owl2 = Some(owl2);
    st.last_class_names = class_names;
}
"#,
            );
        } else {
            source.push_str(&format!(
                "async fn {name}(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>) {{ let _ = state.tenant_states.get_or_create(&auth_context.tenant_id).await; }}\n"
            ));
        }
    }
    for name in PROXY_HANDLERS {
        let artifact_open = if *name == "ingest_document" {
            " open_tenant_request_dir(root, &auth_context.tenant_id, &request_id, true);"
        } else {
            ""
        };
        let tenant_scope = if ["ingest_document", "navigator_query"].contains(name) {
            " let _tenant_state = state.tenant_states.get_or_create(&auth_context.tenant_id).await;"
        } else {
            ""
        };
        let origin = if [
            "ingest_document",
            "proxy_ocr_health",
            "proxy_ocr_passthrough",
        ]
        .contains(name)
        {
            " let base = state.ocr_url.clone(); let url = base;"
        } else {
            " let base = state.align_url.clone(); let url = base;"
        };
        source.push_str(&format!(
            "async fn {name}(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>, headers: HeaderMap) {{{artifact_open}{tenant_scope}{origin} let authorization = crate::router::inbound_authorization(&headers); let _request = crate::router::apply_authorization(state.http_client.post(url), authorization.as_ref()); }}\n"
        ));
    }
    source.push_str(
        r#"
#[cfg(test)]
mod tests {
    struct ServerState { induction: Global, induction_jobs: Global, tbox_manager: Global }
    async fn run_induction(State(state): State<Arc<ServerState>>) { let _ = state.flight_ingest; }
}
// state.flight_ingest must never be read by a REST handler.
"#,
    );
    source
}

fn python_tasks_fixture() -> String {
    let mut source = r#"
import re
from example.auth.jwt import create_access_token
_PLATFORM_INDUCTION_PERMISSIONS = ("platform:induction:write", "platform:induction:read")
_PLATFORM_ALIGNMENT_PERMISSIONS = ("platform:alignment:write", "platform:alignment:read")
_PLATFORM_NAMESPACE_PATTERN = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}\Z")
def _validate_platform_namespace(namespace: str) -> str:
    if not isinstance(namespace, str) or _PLATFORM_NAMESPACE_PATTERN.fullmatch(namespace) is None:
        raise ValueError("invalid platform namespace")
    return namespace
def _platform_service_auth_headers(namespace: str, permissions: tuple[str, ...]):
    namespace = _validate_platform_namespace(namespace)
    token = create_access_token(
        user_id="agent-task-runner",
        email="agent-task-runner@internal.example",
        role="service",
        namespace=namespace,
        extra={"permissions": list(permissions)},
    )
    return {"Authorization": f"Bearer {token}"}
def _platform_job_namespace(namespace: str):
    if not isinstance(namespace, str):
        raise ValueError("platform namespace must be a string")
    resolved = os.getenv("EXAMPLE_NAMESPACE", "") if namespace == "" else namespace
    return _validate_platform_namespace(resolved)
# Decoy: namespace = namespace or "default"
"#
    .to_string();
    for (name, job_id, permissions) in PLATFORM_TASKS {
        source.push_str(&format!(
            r#"
def {name}(self, {job_id}: str | None = None, namespace: str = "", stream_cursor: str | None = None):
    namespace = _platform_job_namespace(namespace)
    auth_headers = _platform_service_auth_headers(namespace, {permissions})
    stream_cursor = _platform_stream_tail_cursor(_PLATFORM_COMPLETION_STREAM)
    response = requests.post(platform_url, headers=auth_headers)
    retry_kwargs = {{"namespace": namespace, "stream_cursor": stream_cursor}}
    fallback = requests.get(platform_job_url, headers=auth_headers)
    entry = xread_for_job(stream, tenant_id=namespace, job_id={job_id}, last_id=stream_cursor or "0-0")
    return fallback, entry
"#
        ));
    }
    source
}

#[test]
fn platform_runtime_trust_boundary_doctor_is_registered_before_artifact_reuse() {
    let names = doctor_names();
    let position = names
        .iter()
        .position(|name| *name == "platform-runtime-trust-boundary")
        .expect("doctor must be registered");
    assert_eq!(names.last(), Some(&"artifact-reuse"));
    assert_eq!(position + 1, names.len() - 1);
}

#[test]
fn platform_runtime_trust_boundary_positive_fixture_has_zero_warnings() {
    let fixture = Fixture::new();
    assert_eq!(fixture.warnings(), Vec::<String>::new());
}

#[test]
fn platform_runtime_trust_boundary_does_not_accept_compose_extension_decoys() {
    let fixture = Fixture::new();
    fixture.write(
        "example-api/docker-compose.yml",
        r#"
x-template:
  example-align:
    environment:
      EXAMPLE_JWT_PUBLIC_KEY_PATH: /secrets/jwt/jwt_public.pem
      REDIS_SERVICE_HOST: redis
      REDIS_SERVICE_PORT: "6379"
      REDIS_PASSWORD: ${REDIS_PASSWORD:-}
    depends_on:
      redis:
        condition: service_healthy
    volumes:
      - ${JWT_KEYPAIR_DIR:-../deploy/secrets}/jwt_public.pem:/secrets/jwt/jwt_public.pem:ro
services:
  redis:
    image: redis
  example-server-data-init:
    image: busybox:1.36.1
    user: "0:0"
    restart: "no"
    command: ["/bin/sh", "-ec", "mkdir -p /data/artifacts && chown 1000:0 /data/artifacts"]
    volumes:
      - shared_data_volume:/data
  example-server-data-write-smoke:
    image: busybox:1.36.1
    user: "1000:0"
    restart: "no"
    depends_on:
      example-server-data-init:
        condition: service_completed_successfully
    command: ["/bin/sh", "-ec", "touch /data/artifacts/.smoke && rm -f /data/artifacts/.smoke"]
    volumes:
      - shared_data_volume:/data
  example-align:
    environment:
      EXAMPLE_JWT_PUBLIC_KEY_PATH: /wrong/key.pem
      REDIS_SERVICE_HOST: untrusted-redis
      REDIS_SERVICE_PORT: "6379"
      REDIS_PASSWORD: ${REDIS_PASSWORD:-}
      EXAMPLE_AUTH_DISABLED: true
    depends_on:
      redis:
        condition: service_started
    volumes:
      - /tmp:/secrets/jwt
  example-server:
    command:
      - --http-addr
      - 0.0.0.0:8082
      - --artifacts-root
      - /data/artifacts
    environment:
      EXAMPLE_JWT_PUBLIC_KEY_PATH: /secrets/jwt/jwt_public.pem
      REDIS_SERVICE_HOST: redis
      REDIS_SERVICE_PORT: "6379"
      REDIS_PASSWORD: ${REDIS_PASSWORD:-}
    depends_on:
      redis:
        condition: service_healthy
      example-server-data-write-smoke:
        condition: service_completed_successfully
    volumes:
      - shared_data_volume:/data
      - ${JWT_KEYPAIR_DIR:-../deploy/secrets}/jwt_public.pem:/secrets/jwt/jwt_public.pem:ro
"#,
    );
    let warnings = fixture.warnings();
    assert!(
        warnings.iter().any(|warning| warning.contains("[compose]")),
        "a valid extension block must not mask the real service: {warnings:#?}"
    );
    assert!(
        warnings.iter().all(|warning| warning.contains("[compose]")),
        "compose decoy fixture emitted collateral groups: {warnings:#?}"
    );
}

#[test]
fn platform_runtime_trust_boundary_resolves_module_qualified_handlers() {
    let fixture = Fixture::new();
    fixture.write(
        "example-platform/example-server/src/admin.rs",
        r#"
async fn handler(State(state): State<Arc<ServerState>>) {
    state.tenant_states.get_or_create(&request.tenant_id).await;
    state.flight_ingest.core.read().await;
}
"#,
    );
    fixture.replace_once(
        "example-platform/example-server/src/lib.rs",
        ".route(\"/api/induction/run\", post(run_induction))",
        ".route(\"/api/induction/run\", post(run_induction))\n        .route(\"/admin\", any(admin::handler))",
    );
    let warnings = fixture.warnings();
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("[handler-auth]")),
        "module-qualified handler without AuthContext must warn: {warnings:#?}"
    );
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("[flight-boundary]")),
        "module-qualified REST Flight read must warn: {warnings:#?}"
    );
    assert!(
        warnings.iter().all(|warning| {
            warning.contains("[handler-auth]") || warning.contains("[flight-boundary]")
        }),
        "module-qualified handler fixture emitted collateral groups: {warnings:#?}"
    );
}

#[test]
fn platform_runtime_trust_boundary_applies_compose_yaml_merges() {
    let fixture = Fixture::new();
    fixture.replace_once(
        "example-api/docker-compose.yml",
        "services:",
        "x-bypass: &bypass\n  EXAMPLE_AUTH_DISABLED: \"true\"\nservices:",
    );
    fixture.replace_once(
        "example-api/docker-compose.yml",
        "  example-align:\n    environment:",
        "  example-align:\n    environment:\n      <<: *bypass",
    );
    let warnings = fixture.warnings();
    assert!(
        warnings.iter().any(|warning| warning.contains("[compose]")),
        "inherited auth bypass must warn: {warnings:#?}"
    );
    assert!(
        warnings.iter().all(|warning| warning.contains("[compose]")),
        "compose merge fixture emitted collateral groups: {warnings:#?}"
    );
}

#[test]
fn platform_runtime_trust_boundary_checks_the_returned_align_router() {
    let fixture = Fixture::new();
    fixture.write(
        "example-align/src/api.rs",
        r#"
pub fn build_router(state: Arc<AppState>, jobs: Arc<JobsApiState>, auth: AuthConfig) -> Router {
    let protected = Router::new()
        .route("/align", post(align_handler))
        .layer(axum::middleware::from_fn_with_state(auth.clone(), jwt_auth_middleware));
    let public = Router::new()
        .route("/health", get(health_handler))
        .route("/ready", get(ready_handler))
        .route("/match", post(match_handler))
        .route("/admin", get(admin_handler))
        .merge(protected.clone());
    let _decoy = Router::new()
        .route("/health", get(health_handler))
        .route("/ready", get(ready_handler))
        .route("/match", post(match_handler))
        .merge(protected);
    public
}
"#,
    );
    let warnings = fixture.warnings();
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("[align-routes]")),
        "returned unsafe router must warn despite a later compliant decoy: {warnings:#?}"
    );
    assert!(
        warnings
            .iter()
            .all(|warning| warning.contains("[align-routes]")),
        "align return fixture emitted collateral groups: {warnings:#?}"
    );
}

#[test]
fn platform_runtime_trust_boundary_checks_module_qualified_proxies() {
    let fixture = Fixture::new();
    fixture.write(
        "example-platform/example-server/src/proxy.rs",
        r#"
async fn handler(
    State(state): State<Arc<ServerState>>,
    Extension(auth_context): Extension<auth::AuthContext>,
    headers: HeaderMap,
) {
    state.http_client.post(url).send().await;
}
"#,
    );
    fixture.replace_once(
        "example-platform/example-server/src/lib.rs",
        ".route(\"/api/induction/run\", post(run_induction))",
        ".route(\"/api/induction/run\", post(run_induction))\n        .route(\"/proxy\", any(proxy::handler))",
    );
    let warnings = fixture.warnings();
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("[bearer-forwarding]")),
        "module-qualified proxy without bearer forwarding must warn: {warnings:#?}"
    );
    assert!(
        warnings
            .iter()
            .all(|warning| warning.contains("[bearer-forwarding]")),
        "module proxy fixture emitted collateral groups: {warnings:#?}"
    );
}

#[test]
fn platform_runtime_trust_boundary_reports_one_broken_invariant_at_a_time() {
    struct Case {
        name: &'static str,
        path: &'static str,
        from: &'static str,
        to: &'static str,
        warning_group: &'static str,
    }

    let cases = [
        Case {
            name: "platform auth bypass becomes permissive",
            path: "example-platform/example-server/src/auth.rs",
            from: "disabled == Some(\"true\")",
            to: "disabled.is_some()",
            warning_group: "[auth]",
        },
        Case {
            name: "platform auth retains exact check but adds permissive bypass",
            path: "example-platform/example-server/src/auth.rs",
            from: "if disabled == Some(\"true\") { return Ok(Self::disabled_for_development()); }",
            to: "if disabled.is_some() { return Ok(Self::disabled_for_development()); }\n        if disabled == Some(\"true\") { return Ok(Self::disabled_for_development()); }",
            warning_group: "[auth]",
        },
        Case {
            name: "platform auth retains exact check but adds helper bypass",
            path: "example-platform/example-server/src/auth.rs",
            from: "if disabled == Some(\"true\") { return Ok(Self::disabled_for_development()); }",
            to: "if helper_bypass(disabled) { return Ok(dev_auth()); }\n        if disabled == Some(\"true\") { return Ok(Self::disabled_for_development()); }",
            warning_group: "[auth]",
        },
        Case {
            name: "platform auth drops issuer validation",
            path: "example-platform/example-server/src/auth.rs",
            from: "validation.set_issuer(&[EXPECTED_ISSUER]);",
            to: "",
            warning_group: "[auth]",
        },
        Case {
            name: "platform key loader returns local development early",
            path: "example-platform/example-server/src/auth.rs",
            from: "fn required_from_pem(pem: &[u8]) -> Result<Self, AuthConfigError> {",
            to: "fn required_from_pem(pem: &[u8]) -> Result<Self, AuthConfigError> {\n        if allow_unsafe() { return Ok(Self::disabled_for_development()); }",
            warning_group: "[auth]",
        },
        Case {
            name: "platform authenticator returns local development early",
            path: "example-platform/example-server/src/auth.rs",
            from: "fn authenticate(&self, headers: &HeaderMap) -> Result<AuthContext, AuthError> {",
            to: "fn authenticate(&self, headers: &HeaderMap) -> Result<AuthContext, AuthError> {\n        if allow_unsafe() { return Ok(AuthContext::local_development()); }",
            warning_group: "[auth]",
        },
        Case {
            name: "platform JWT middleware injects local development context",
            path: "example-platform/example-server/src/auth.rs",
            from: "match auth.authenticate(request.headers()) {",
            to: "if allow_unsafe() { request.extensions_mut().insert(AuthContext::local_development()); return next.run(request).await; }\n    match auth.authenticate(request.headers()) {",
            warning_group: "[auth]",
        },
        Case {
            name: "platform auth accepts conflicting tenants",
            path: "example-platform/example-server/src/auth.rs",
            from: "(Some(_), Some(_)) => return Err(AuthError::InvalidTenant)",
            to: "(Some(canonical), Some(_)) => canonical",
            warning_group: "[auth]",
        },
        Case {
            name: "platform tenant normalizer adds permissive early return",
            path: "example-platform/example-server/src/auth.rs",
            from: "fn normalize_tenant(tenant_id: Option<String>, namespace: Option<String>) -> Result<String, AuthError> {",
            to: "fn normalize_tenant(tenant_id: Option<String>, namespace: Option<String>) -> Result<String, AuthError> {\n    if allow_unsafe() { return Ok(tenant_id.unwrap_or_default()); }",
            warning_group: "[auth]",
        },
        Case {
            name: "verified claims bypass canonical tenant normalization",
            path: "example-platform/example-server/src/auth.rs",
            from: "fn from_verified_claims(claims: ClaimsWire) -> Result<Self, AuthError> {",
            to: "fn from_verified_claims(claims: ClaimsWire) -> Result<Self, AuthError> {\n        if allow_unsafe() { return Ok(Self::local_development()); }",
            warning_group: "[auth]",
        },
        Case {
            name: "align loses legacy tenant claim",
            path: "example-align/src/auth.rs",
            from: "namespace: Option<String>",
            to: "legacy_tenant: Option<String>",
            warning_group: "[auth]",
        },
        Case {
            name: "artifact init service is missing",
            path: "example-api/docker-compose.yml",
            from: "  example-server-data-init:\n    image: busybox:1.36.1",
            to: "  example-server-data-init-disabled:\n    image: busybox:1.36.1",
            warning_group: "[compose-artifacts]",
        },
        Case {
            name: "artifact init is not root",
            path: "example-api/docker-compose.health-audit.yml",
            from: "    user: \"0:0\"",
            to: "    user: \"1000:0\"",
            warning_group: "[compose-artifacts]",
        },
        Case {
            name: "artifact init mounts the shared volume at the wrong target",
            path: "example-api/docker-compose.yml",
            from: "        chmod 0770 /data/artifacts\n    volumes:\n      - shared_data_volume:/data",
            to: "        chmod 0770 /data/artifacts\n    volumes:\n      - shared_data_volume:/cache",
            warning_group: "[compose-artifacts]",
        },
        Case {
            name: "artifact init stops assigning runtime ownership",
            path: "example-api/docker-compose.health-audit.yml",
            from: "        chown 1000:0 /data/artifacts",
            to: "        echo initialized /data/artifacts",
            warning_group: "[compose-artifacts]",
        },
        Case {
            name: "artifact write smoke uses the wrong uid",
            path: "example-api/docker-compose.yml",
            from: "    user: \"1000:0\"",
            to: "    user: \"1001:0\"",
            warning_group: "[compose-artifacts]",
        },
        Case {
            name: "artifact write smoke loses its actual write probe",
            path: "example-api/docker-compose.health-audit.yml",
            from: "        touch \"$$probe\"",
            to: "        test -w /data/artifacts",
            warning_group: "[compose-artifacts]",
        },
        Case {
            name: "artifact write smoke no longer removes its probe",
            path: "example-api/docker-compose.yml",
            from: "        rm -f \"$$probe\"",
            to: "        true # probe retained",
            warning_group: "[compose-artifacts]",
        },
        Case {
            name: "artifact write smoke does not wait for init completion",
            path: "example-api/docker-compose.health-audit.yml",
            from: "      example-server-data-init:\n        condition: service_completed_successfully",
            to: "      example-server-data-init:\n        condition: service_started",
            warning_group: "[compose-artifacts]",
        },
        Case {
            name: "server does not wait for artifact write smoke",
            path: "example-api/docker-compose.yml",
            from: "      example-server-data-write-smoke:\n        condition: service_completed_successfully\n    volumes:",
            to: "    volumes:",
            warning_group: "[compose-artifacts]",
        },
        Case {
            name: "server mounts shared data at the wrong target",
            path: "example-api/docker-compose.yml",
            from: "      example-server-data-write-smoke:\n        condition: service_completed_successfully\n    volumes:\n      - shared_data_volume:/data",
            to: "      example-server-data-write-smoke:\n        condition: service_completed_successfully\n    volumes:\n      - shared_data_volume:/srv/data",
            warning_group: "[compose-artifacts]",
        },
        Case {
            name: "server mounts the wrong data volume source",
            path: "example-api/docker-compose.health-audit.yml",
            from: "      example-server-data-write-smoke:\n        condition: service_completed_successfully\n    volumes:\n      - shared_data_volume:/data",
            to: "      example-server-data-write-smoke:\n        condition: service_completed_successfully\n    volumes:\n      - unrelated_data_volume:/data",
            warning_group: "[compose-artifacts]",
        },
        Case {
            name: "server passes the wrong artifact root despite a label decoy",
            path: "example-api/docker-compose.yml",
            from: "      - --artifacts-root\n      - /data/artifacts\n    environment:",
            to: "      - --artifacts-root\n      - /tmp/artifacts\n    labels:\n      artifact-root-decoy: \"--artifacts-root /data/artifacts\"\n    environment:",
            warning_group: "[compose-artifacts]",
        },
        Case {
            name: "compose loses jwt path",
            path: "example-api/docker-compose.yml",
            from: "EXAMPLE_JWT_PUBLIC_KEY_PATH: /secrets/jwt/jwt_public.pem",
            to: "EXAMPLE_JWT_PUBLIC_KEY_PATH: /wrong/key.pem",
            warning_group: "[compose]",
        },
        Case {
            name: "compose restores directory keypair mount",
            path: "example-api/docker-compose.yml",
            from: "${JWT_KEYPAIR_DIR:-../deploy/secrets}/jwt_public.pem:/secrets/jwt/jwt_public.pem:ro",
            to: "${JWT_KEYPAIR_DIR:-../deploy/secrets}:/secrets/jwt:ro",
            warning_group: "[compose]",
        },
        Case {
            name: "compose adds directory keypair mount beside public key",
            path: "example-api/docker-compose.health-audit.yml",
            from: "${JWT_KEYPAIR_DIR:-../deploy/secrets}/jwt_public.pem:/secrets/jwt/jwt_public.pem:ro",
            to: "${JWT_KEYPAIR_DIR:-../deploy/secrets}/jwt_public.pem:/secrets/jwt/jwt_public.pem:ro\n      - ${JWT_KEYPAIR_DIR:-../deploy/secrets}:/secrets/jwt:ro",
            warning_group: "[compose]",
        },
        Case {
            name: "compose smuggles keypair directory at alternate target",
            path: "example-api/docker-compose.yml",
            from: "${JWT_KEYPAIR_DIR:-../deploy/secrets}/jwt_public.pem:/secrets/jwt/jwt_public.pem:ro",
            to: "type: bind\n        source: ${JWT_KEYPAIR_DIR:-../deploy/secrets}\n        target: /keys\n        read_only: true",
            warning_group: "[compose]",
        },
        Case {
            name: "compose loses redis password",
            path: "example-api/docker-compose.health-audit.yml",
            from: "REDIS_PASSWORD: ${REDIS_PASSWORD:-}",
            to: "REDIS_PASSWORD: hard-coded",
            warning_group: "[compose]",
        },
        Case {
            name: "compose redis dependency is not healthy",
            path: "example-api/docker-compose.yml",
            from: "condition: service_healthy",
            to: "condition: service_started",
            warning_group: "[compose]",
        },
        Case {
            name: "compose enables bypass",
            path: "example-api/docker-compose.yml",
            from: "EXAMPLE_JWT_PUBLIC_KEY_PATH: /secrets/jwt/jwt_public.pem",
            to: "EXAMPLE_JWT_PUBLIC_KEY_PATH: /secrets/jwt/jwt_public.pem\n      EXAMPLE_AUTH_DISABLED: true",
            warning_group: "[compose]",
        },
        Case {
            name: "compose enables list-form bypass",
            path: "example-api/docker-compose.yml",
            from: "EXAMPLE_JWT_PUBLIC_KEY_PATH: /secrets/jwt/jwt_public.pem",
            to: "EXAMPLE_JWT_PUBLIC_KEY_PATH: /secrets/jwt/jwt_public.pem\n      - EXAMPLE_AUTH_DISABLED=true",
            warning_group: "[compose]",
        },
        Case {
            name: "compose imports bare list-form bypass",
            path: "example-api/docker-compose.yml",
            from: "EXAMPLE_JWT_PUBLIC_KEY_PATH: /secrets/jwt/jwt_public.pem",
            to: "EXAMPLE_JWT_PUBLIC_KEY_PATH: /secrets/jwt/jwt_public.pem\n      - EXAMPLE_AUTH_DISABLED",
            warning_group: "[compose]",
        },
        Case {
            name: "compose nests environment under a decoy mapping",
            path: "example-api/docker-compose.yml",
            from: "  example-align:\n    environment:",
            to: "  example-align:\n    metadata:\n      environment:",
            warning_group: "[compose]",
        },
        Case {
            name: "align container returns to root",
            path: "example-align/Dockerfile",
            from: "USER appuser",
            to: "USER root",
            warning_group: "[container-user]",
        },
        Case {
            name: "platform container returns to uid zero",
            path: "example-platform/Dockerfile.server",
            from: "USER appuser",
            to: "USER 0:0",
            warning_group: "[container-user]",
        },
        Case {
            name: "align adds public route",
            path: "example-align/src/api.rs",
            from: ".route(\"/match\", post(match_handler))",
            to: ".route(\"/match\", post(match_handler))\n        .route(\"/admin\", get(admin_handler))",
            warning_group: "[align-routes]",
        },
        Case {
            name: "align adds public delete route",
            path: "example-align/src/api.rs",
            from: ".route(\"/match\", post(match_handler))",
            to: ".route(\"/match\", post(match_handler))\n        .route(\"/admin\", delete(admin_handler))",
            warning_group: "[align-routes]",
        },
        Case {
            name: "align adds public fallback",
            path: "example-align/src/api.rs",
            from: ".merge(protected)",
            to: ".fallback(admin_handler)\n        .merge(protected)",
            warning_group: "[align-routes]",
        },
        Case {
            name: "align merges callable public admin router",
            path: "example-align/src/api.rs",
            from: ".merge(protected)",
            to: ".merge(protected).merge(admin_router())",
            warning_group: "[align-routes]",
        },
        Case {
            name: "align protected router drops JWT middleware layer",
            path: "example-align/src/api.rs",
            from: ".layer(axum::middleware::from_fn_with_state(auth.clone(), crate::auth::jwt_auth_middleware));",
            to: ";",
            warning_group: "[align-routes]",
        },
        Case {
            name: "align keeps unused JWT middleware decoy",
            path: "example-align/src/api.rs",
            from: ".layer(axum::middleware::from_fn_with_state(auth.clone(), crate::auth::jwt_auth_middleware));",
            to: ";\n    let _unused = axum::middleware::from_fn_with_state(auth.clone(), crate::auth::jwt_auth_middleware);",
            warning_group: "[align-routes]",
        },
        Case {
            name: "align shadows compliant protected router with unprotected router",
            path: "example-align/src/api.rs",
            from: ".layer(axum::middleware::from_fn_with_state(auth.clone(), crate::auth::jwt_auth_middleware));",
            to: ".layer(axum::middleware::from_fn_with_state(auth.clone(), crate::auth::jwt_auth_middleware));\n    let protected = Router::new().route(\"/admin\", get(admin_handler));",
            warning_group: "[align-routes]",
        },
        Case {
            name: "align public match accepts form encoded URLs",
            path: "example-align/src/api.rs",
            from: "if content_type.contains(\"multipart/form-data\") {",
            to: "if content_type.contains(\"multipart/form-data\") || content_type.contains(\"application/x-www-form-urlencoded\") {",
            warning_group: "[align-match]",
        },
        Case {
            name: "align public match restores URL and file dereference handler",
            path: "example-align/src/api.rs",
            from: "async fn match_multipart(state: Arc<AppState>, multipart: Multipart)",
            to: "async fn dereference_ontology(url: &str) { let remote = reqwest::get(url).await; let local = tokio::fs::read(url).await; }\nasync fn match_multipart(state: Arc<AppState>, multipart: Multipart)",
            warning_group: "[align-match]",
        },
        Case {
            name: "server loses tenant registry",
            path: "example-platform/example-server/src/lib.rs",
            from: "pub(crate) tenant_states: Arc<crate::tenant_state::TenantStateRegistry>,",
            to: "pub(crate) runtime_state: Arc<GlobalRuntimeState>,",
            warning_group: "[tenant-state]",
        },
        Case {
            name: "platform proxy origin becomes mutable",
            path: "example-platform/example-server/src/lib.rs",
            from: "align_url: String,",
            to: "align_url: Arc<tokio::sync::RwLock<String>> ,",
            warning_group: "[proxy-origins]",
        },
        Case {
            name: "platform proxy origin initializes from discovery",
            path: "example-platform/example-server/src/lib.rs",
            from: "align_url: self.config.align_url.clone(),",
            to: "align_url: resolved.align_url.unwrap_or_default(),",
            warning_group: "[proxy-origins]",
        },
        Case {
            name: "platform discovery writes credential proxy origin",
            path: "example-platform/example-server/src/lib.rs",
            from: "observe(diagnostics);",
            to: "*state.align_url.write().await = diagnostics.resolved.align_url.unwrap_or_default();",
            warning_group: "[proxy-origins]",
        },
        Case {
            name: "platform proxy sends bearer to discovered origin",
            path: "example-platform/example-server/src/lib.rs",
            from: "let base = state.align_url.clone(); let url = base;",
            to: "let base = resolved.align_url.unwrap_or_default(); let url = base;",
            warning_group: "[proxy-origins]",
        },
        Case {
            name: "server restores global induction",
            path: "example-platform/example-server/src/lib.rs",
            from: "adbc_policy: db::AdbcPolicy,",
            to: "adbc_policy: db::AdbcPolicy,\n    induction: RwLock<InductionState>,",
            warning_group: "[tenant-state]",
        },
        Case {
            name: "server restores global jobs",
            path: "example-platform/example-server/src/lib.rs",
            from: "adbc_policy: db::AdbcPolicy,",
            to: "adbc_policy: db::AdbcPolicy,\n    induction_jobs: RwLock<JobMap>,",
            warning_group: "[tenant-state]",
        },
        Case {
            name: "server restores global tbox",
            path: "example-platform/example-server/src/lib.rs",
            from: "adbc_policy: db::AdbcPolicy,",
            to: "adbc_policy: db::AdbcPolicy,\n    tbox_manager: TBoxManager,",
            warning_group: "[tenant-state]",
        },
        Case {
            name: "platform protected router drops JWT middleware layer",
            path: "example-platform/example-server/src/lib.rs",
            from: ".layer(axum::middleware::from_fn_with_state(\n            self.auth.clone(),\n            auth::jwt_auth_middleware,\n        ))\n        .with_state(state.clone())",
            to: ".with_state(state.clone())",
            warning_group: "[handler-auth]",
        },
        Case {
            name: "platform keeps unused JWT middleware decoy",
            path: "example-platform/example-server/src/lib.rs",
            from: ".layer(axum::middleware::from_fn_with_state(\n            self.auth.clone(),\n            auth::jwt_auth_middleware,\n        ))\n        .with_state(state.clone());",
            to: ".with_state(state.clone());\n    let _unused = axum::middleware::from_fn_with_state(self.auth.clone(), auth::jwt_auth_middleware);",
            warning_group: "[handler-auth]",
        },
        Case {
            name: "platform shadows compliant protected router with unprotected router",
            path: "example-platform/example-server/src/lib.rs",
            from: ".with_state(state.clone());\n    let app = public.merge(protected);",
            to: ".with_state(state.clone());\n    let protected = Router::new().route(\"/admin\", get(admin_handler)).with_state(state.clone());\n    let app = public.merge(protected);",
            warning_group: "[handler-auth]",
        },
        Case {
            name: "stateful handler loses typed auth",
            path: "example-platform/example-server/src/lib.rs",
            from: "async fn get_induction_config(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>)",
            to: "async fn get_induction_config(State(state): State<Arc<ServerState>>)",
            warning_group: "[handler-auth]",
        },
        Case {
            name: "stateful handler ignores auth tenant for mutable state",
            path: "example-platform/example-server/src/lib.rs",
            from: "async fn get_induction_config(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>) { let _ = state.tenant_states.get_or_create(&auth_context.tenant_id).await; }",
            to: "async fn get_induction_config(State(state): State<Arc<ServerState>>, Extension(_auth_context): Extension<auth::AuthContext>) { let _ = state.tenant_states.get_or_create(&request.tenant_id).await; }",
            warning_group: "[handler-auth]",
        },
        Case {
            name: "stateful handler retains auth decoy and mutates request tenant",
            path: "example-platform/example-server/src/lib.rs",
            from: "async fn get_induction_config(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>) { let _ = state.tenant_states.get_or_create(&auth_context.tenant_id).await; }",
            to: "async fn get_induction_config(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>) { let _decoy = state.tenant_states.get_or_create(&auth_context.tenant_id).await; let _actual = state.tenant_states.get_or_create(&request.tenant_id).await; }",
            warning_group: "[handler-auth]",
        },
        Case {
            name: "run induction mutates committed core before closure succeeds",
            path: "example-platform/example-server/src/lib.rs",
            from: "let facts = std::mem::take(&mut candidate_core.triples);",
            to: "st.core = candidate_core;\n    let facts = std::mem::take(&mut st.core.triples);",
            warning_group: "[transactional-state]",
        },
        Case {
            name: "import ttl parses after opening committed tenant state",
            path: "example-platform/example-server/src/lib.rs",
            from: "let all_triples = parse_rdf_triples_as_core_facts(&body.ttl).map_err(bad_request)?;\n    if all_triples.is_empty() { return Err(bad_request(\"triples required\")); }\n    let tenant_state = state.tenant_states.get_or_create(&auth_context.tenant_id).await;",
            to: "let tenant_state = state.tenant_states.get_or_create(&auth_context.tenant_id).await;\n    let all_triples = parse_rdf_triples_as_core_facts(&body.ttl).map_err(bad_request)?;\n    if all_triples.is_empty() { return Err(bad_request(\"triples required\")); }",
            warning_group: "[transactional-state]",
        },
        Case {
            name: "import ttl mutates committed config before closure succeeds",
            path: "example-platform/example-server/src/lib.rs",
            from: "let mut candidate_config = st.config.clone();",
            to: "st.config.base_iri = body.base_iri.clone().unwrap_or_default();\n    let mut candidate_config = st.config.clone();",
            warning_group: "[transactional-state]",
        },
        Case {
            name: "event handler ignores auth tenant for mutable state",
            path: "example-platform/example-server/src/events.rs",
            from: "state.tenant_states.get_or_create(&tenant_id).await",
            to: "state.tenant_states.get_or_create(&request.tenant_id).await",
            warning_group: "[handler-auth]",
        },
        Case {
            name: "event handler retains auth decoy and mutates request tenant",
            path: "example-platform/example-server/src/events.rs",
            from: "state.tenant_states.get_or_create(&tenant_id).await; publish_job_event",
            to: "state.tenant_states.get_or_create(&tenant_id).await; state.tenant_states.get_or_create(&request.tenant_id).await; publish_job_event",
            warning_group: "[handler-auth]",
        },
        Case {
            name: "new routed stateful handler lacks typed auth",
            path: "example-platform/example-server/src/lib.rs",
            from: "async fn run(self) {",
            to: "async fn new_stateful(State(state): State<Arc<ServerState>>) { state.tenant_states.read().await; }\nasync fn run(self) {\n    let extra = Router::new().route(\"/new-stateful\", any(new_stateful));",
            warning_group: "[handler-auth]",
        },
        Case {
            name: "artifact layout loses tenant",
            path: "example-platform/example-server/src/lib.rs",
            from: "root.join(\"tenants\").join(tenant_id).join(request_id)",
            to: "root.join(\"requests\").join(request_id)",
            warning_group: "[artifacts]",
        },
        Case {
            name: "ingest bypasses tenant artifact helper",
            path: "example-platform/example-server/src/lib.rs",
            from: "open_tenant_request_dir(root, &auth_context.tenant_id, &request_id, true);",
            to: "std::fs::create_dir_all(root.join(request_id));",
            warning_group: "[artifacts]",
        },
        Case {
            name: "retrieval bypasses tenant artifact helper",
            path: "example-platform/example-server/src/lib.rs",
            from: "open_tenant_artifact(root, &auth_context.tenant_id, &id, filename);",
            to: "std::fs::read_to_string(root.join(id).join(filename));",
            warning_group: "[artifacts]",
        },
        Case {
            name: "rest handler reads flight",
            path: "example-platform/example-server/src/lib.rs",
            from: "let _ = state.tenant_states.get_or_create(&auth_context.tenant_id).await;",
            to: "let _ = state.flight_ingest.core.read().await;",
            warning_group: "[flight-boundary]",
        },
        Case {
            name: "new inline REST handler reads flight",
            path: "example-platform/example-server/src/lib.rs",
            from: ".route(\"/api/induction/run\", post(run_induction))",
            to: ".route(\"/api/induction/run\", post(run_induction))\n        .route(\"/flight-debug\", get(|State(state): State<Arc<ServerState>>| async move { state.flight_ingest.core.read().await }))",
            warning_group: "[flight-boundary]",
        },
        Case {
            name: "separate any subrouter handler reads flight",
            path: "example-platform/example-server/src/lib.rs",
            from: "async fn run(self) {",
            to: "async fn flight_debug(State(state): State<Arc<ServerState>>, Extension(auth): Extension<auth::AuthContext>) { state.flight_ingest.core.read().await; }\nasync fn run(self) {\n    let extra = Router::new().route(\"/flight-debug\", any(flight_debug));",
            warning_group: "[flight-boundary]",
        },
        Case {
            name: "chained method router handler reads flight",
            path: "example-platform/example-server/src/lib.rs",
            from: "async fn run(self) {",
            to: "async fn flight_debug(State(state): State<Arc<ServerState>>, Extension(auth): Extension<auth::AuthContext>) { state.flight_ingest.core.read().await; }\nasync fn run(self) {\n    let extra = Router::new().route(\"/flight-debug\", get(run_induction).post(flight_debug));",
            warning_group: "[flight-boundary]",
        },
        Case {
            name: "nested subrouter handler reads flight",
            path: "example-platform/example-server/src/lib.rs",
            from: "async fn run(self) {",
            to: "async fn flight_debug(State(state): State<Arc<ServerState>>, Extension(auth): Extension<auth::AuthContext>) { state.flight_ingest.core.read().await; }\nasync fn run(self) {\n    let extra = Router::new().route(\"/flight-debug\", get(flight_debug));\n    let nested = Router::new().nest(\"/debug\", extra);",
            warning_group: "[flight-boundary]",
        },
        Case {
            name: "db dto becomes permissive",
            path: "example-platform/example-server/src/db.rs",
            from: "#[serde(deny_unknown_fields)]",
            to: "#[serde(default)]",
            warning_group: "[database]",
        },
        Case {
            name: "db dto exposes entrypoint",
            path: "example-platform/example-server/src/db.rs",
            from: "pub driver: String,",
            to: "pub driver: String,\n    pub entrypoint: Option<String>,",
            warning_group: "[database]",
        },
        Case {
            name: "db response stops redacting source",
            path: "example-platform/example-server/src/db.rs",
            from: "pub struct SchemaIngestResult { pub source: RedactedDbSource }",
            to: "pub struct SchemaIngestResult { pub source: DbSourceRequest }",
            warning_group: "[database]",
        },
        Case {
            name: "merge accepts output dir",
            path: "example-platform/example-server/src/db.rs",
            from: "pub struct MergeOptions { pub formats: Vec<String> }",
            to: "pub struct MergeOptions { pub formats: Vec<String>, pub output_dir: PathBuf }",
            warning_group: "[database]",
        },
        Case {
            name: "db policy loses allowlist",
            path: "example-platform/example-server/src/db.rs",
            from: "allowed_drivers: Arc<BTreeSet<String>>",
            to: "driver_name: String",
            warning_group: "[database]",
        },
        Case {
            name: "db policy no longer disables empty allowlist",
            path: "example-platform/example-server/src/db.rs",
            from: "if raw.is_empty()",
            to: "if false",
            warning_group: "[database]",
        },
        Case {
            name: "db policy adds an early permissive success",
            path: "example-platform/example-server/src/db.rs",
            from: "fn resolve(&self, source: &DbSourceRequest) -> Result<ResolvedDbSource, DbPolicyError> {",
            to: "fn resolve(&self, source: &DbSourceRequest) -> Result<ResolvedDbSource, DbPolicyError> {\n        if source.driver == \"*\" { return Ok(ResolvedDbSource::from(source)); }",
            warning_group: "[database]",
        },
        Case {
            name: "db policy wraps canonical resolution in a permissive match arm",
            path: "example-platform/example-server/src/db.rs",
            from: r#"fn resolve(&self, source: &DbSourceRequest) -> Result<ResolvedDbSource, DbPolicyError> {
        if self.allowed_drivers.is_empty() { return Err(DbPolicyError::Disabled); }
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
    }"#,
            to: r#"fn resolve(&self, source: &DbSourceRequest) -> Result<ResolvedDbSource, DbPolicyError> {
        match allow_unsafe() {
            false => {
                if self.allowed_drivers.is_empty() { return Err(DbPolicyError::Disabled); }
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
            }
            true => permissive_resolve(source),
        }
    }"#,
            warning_group: "[database]",
        },
        Case {
            name: "db formats accept csv",
            path: "example-platform/example-server/src/db.rs",
            from: "\"parquet\" | \"arrow_ipc\" => {}",
            to: "\"parquet\" | \"arrow_ipc\" | \"csv\" => {}",
            warning_group: "[database]",
        },
        Case {
            name: "db output stem loses metadata hash",
            path: "example-platform/example-server/src/db.rs",
            from: "let digest = Sha256::digest(metadata);",
            to: "let digest = [0_u8; 8];",
            warning_group: "[database]",
        },
        Case {
            name: "db errors stop redacting passwords",
            path: "example-platform/example-server/src/db.rs",
            from: "DbExecutorError::Failed(_) => \"database driver operation failed\".to_string(),",
            to: "DbExecutorError::Failed(_) => error.to_string(),",
            warning_group: "[database]",
        },
        Case {
            name: "db uses bare default load flags",
            path: "example-platform/example-server/src/db.rs",
            from: "safe_adbc_load_flags(),",
            to: "adbc_core::LOAD_FLAG_DEFAULT,",
            warning_group: "[database]",
        },
        Case {
            name: "db load flags retain a safe decoy but return unsafe default",
            path: "example-platform/example-server/src/db.rs",
            from: "adbc_core::LOAD_FLAG_DEFAULT & !adbc_core::LOAD_FLAG_ALLOW_RELATIVE_PATHS",
            to: "{ let _safe = adbc_core::LOAD_FLAG_DEFAULT & !adbc_core::LOAD_FLAG_ALLOW_RELATIVE_PATHS; adbc_core::LOAD_FLAG_DEFAULT }",
            warning_group: "[database]",
        },
        Case {
            name: "db ingest permission drifts",
            path: "example-platform/example-server/src/lib.rs",
            from: "platform:db:ingest",
            to: "platform:db:*",
            warning_group: "[database]",
        },
        Case {
            name: "db authorization adds an operator bypass",
            path: "example-platform/example-server/src/lib.rs",
            from: "fn authorize_db_operation(auth_context: &auth::AuthContext, permission: &str) -> bool {",
            to: "fn authorize_db_operation(auth_context: &auth::AuthContext, permission: &str) -> bool {\n    if auth_context.has_role(\"operator\") { return true; }",
            warning_group: "[database]",
        },
        Case {
            name: "db ingest ignores authorization behind a forbidden decoy",
            path: "example-platform/example-server/src/lib.rs",
            from: "if !authorize_db_operation(&auth_context, \"platform:db:ingest\") {\n        return Err(db_error(axum::http::StatusCode::FORBIDDEN, \"Forbidden\"));\n    }",
            to: "let _ignored = authorize_db_operation(&auth_context, \"platform:db:ingest\");\n    if false { return Err(db_error(axum::http::StatusCode::FORBIDDEN, \"Forbidden\")); }",
            warning_group: "[database]",
        },
        Case {
            name: "db output loses tenant",
            path: "example-platform/example-server/src/db.rs",
            from: "open_tenant_db_operation_dir(&output.root, &output.tenant_id, &output.operation_id, true)",
            to: "output.root.join(&output.operation_id)",
            warning_group: "[database]",
        },
        Case {
            name: "db formats are validated after output selection",
            path: "example-platform/example-server/src/lib.rs",
            from: "let formats = request.options.unwrap_or_default().validate().map_err(map_db_policy_error)?;\n    let operation_id = Uuid::new_v4().simple().to_string();\n    let output = state.adbc_policy.output_context(&auth_context.tenant_id, &operation_id)?;",
            to: "let operation_id = Uuid::new_v4().simple().to_string();\n    let output = state.adbc_policy.output_context(&auth_context.tenant_id, &operation_id)?;\n    let formats = request.options.unwrap_or_default().validate().map_err(map_db_policy_error)?;",
            warning_group: "[database]",
        },
        Case {
            name: "db invalid request maps to internal error",
            path: "example-platform/example-server/src/lib.rs",
            from: "StatusCode::BAD_REQUEST, \"Invalid database request\"",
            to: "StatusCode::INTERNAL_SERVER_ERROR, \"driver failed: secret\"",
            warning_group: "[database]",
        },
        Case {
            name: "python tokens bypass canonical authority",
            path: "example-api/example/agents/tasks.py",
            from: "token = create_access_token(",
            to: "token = jwt.encode(",
            warning_group: "[python-jobs]",
        },
        Case {
            name: "python tenant validation removed",
            path: "example-api/example/agents/tasks.py",
            from: "namespace = _validate_platform_namespace(namespace)",
            to: "namespace = namespace",
            warning_group: "[python-jobs]",
        },
        Case {
            name: "python tenant grammar accepts leading punctuation",
            path: "example-api/example/agents/tasks.py",
            from: "[A-Za-z0-9][A-Za-z0-9._-]{0,127}\\Z",
            to: "[A-Za-z0-9._-]{1,128}\\Z",
            warning_group: "[python-jobs]",
        },
        Case {
            name: "python trigger loses auth",
            path: "example-api/example/agents/tasks.py",
            from: "requests.post(platform_url, headers=auth_headers)",
            to: "requests.post(platform_url)",
            warning_group: "[python-jobs]",
        },
        Case {
            name: "python keeps authenticated trigger decoy plus unauthenticated trigger",
            path: "example-api/example/agents/tasks.py",
            from: "response = requests.post(platform_url, headers=auth_headers)",
            to: "response = requests.post(platform_url, headers=auth_headers)\n    leaked = requests.post(platform_url)",
            warning_group: "[python-jobs]",
        },
        Case {
            name: "python fallback loses auth",
            path: "example-api/example/agents/tasks.py",
            from: "requests.get(platform_job_url, headers=auth_headers)",
            to: "requests.get(platform_job_url)",
            warning_group: "[python-jobs]",
        },
        Case {
            name: "python inline comment fakes trigger auth",
            path: "example-api/example/agents/tasks.py",
            from: "response = requests.post(platform_url, headers=auth_headers)",
            to: "response = requests.post(platform_url)  # headers=auth_headers",
            warning_group: "[python-jobs]",
        },
        Case {
            name: "python trigger skips pre-tail cursor",
            path: "example-api/example/agents/tasks.py",
            from: "stream_cursor = _platform_stream_tail_cursor(_PLATFORM_COMPLETION_STREAM)",
            to: "stream_cursor = \"$\"",
            warning_group: "[python-jobs]",
        },
        Case {
            name: "python retry drops tenant and cursor",
            path: "example-api/example/agents/tasks.py",
            from: "retry_kwargs = {\"namespace\": namespace, \"stream_cursor\": stream_cursor}",
            to: "retry_kwargs = {}",
            warning_group: "[python-jobs]",
        },
        Case {
            name: "python restores default tenant",
            path: "example-api/example/agents/tasks.py",
            from: "resolved = os.getenv(\"EXAMPLE_NAMESPACE\", \"\") if namespace == \"\" else namespace",
            to: "resolved = (os.getenv(\"EXAMPLE_NAMESPACE\", \"\") or \"default\") if namespace == \"\" else namespace",
            warning_group: "[python-jobs]",
        },
        Case {
            name: "python job namespace restores truthiness fallback",
            path: "example-api/example/agents/tasks.py",
            from: "resolved = os.getenv(\"EXAMPLE_NAMESPACE\", \"\") if namespace == \"\" else namespace",
            to: "resolved = namespace or os.getenv(\"EXAMPLE_NAMESPACE\", \"\")",
            warning_group: "[python-jobs]",
        },
        Case {
            name: "python task overrides canonical tenant with default",
            path: "example-api/example/agents/tasks.py",
            from: "namespace = _platform_job_namespace(namespace)",
            to: "namespace = _platform_job_namespace(namespace)\n    namespace = namespace or \"default\"",
            warning_group: "[python-jobs]",
        },
        Case {
            name: "platform event loses tenant",
            path: "example-platform/example-server/src/events.rs",
            from: "(\"tenant_id\", tenant_id.to_owned())",
            to: "(\"namespace\", tenant_id.to_owned())",
            warning_group: "[events]",
        },
        Case {
            name: "platform publisher uses wrong tenant",
            path: "example-platform/example-server/src/events.rs",
            from: "publish_job_event(&tenant_id, &job_id, \"succeeded\")",
            to: "publish_job_event(&other_tenant, &job_id, \"succeeded\")",
            warning_group: "[events]",
        },
        Case {
            name: "align event loses tenant",
            path: "example-align/src/job_queue.rs",
            from: "(\"tenant_id\", tenant_id.to_owned())",
            to: "(\"namespace\", tenant_id.to_owned())",
            warning_group: "[events]",
        },
        Case {
            name: "align publisher uses wrong tenant",
            path: "example-align/src/job_queue.rs",
            from: "publish_job_event(&tenant_id, &job_id, stream_status)",
            to: "publish_job_event(&other_tenant, &job_id, stream_status)",
            warning_group: "[events]",
        },
        Case {
            name: "redis waiter matches only job",
            path: "example-api/example/core/redis_client.py",
            from: "decoded.get(\"tenant_id\") == tenant_id and decoded.get(\"job_id\") == job_id",
            to: "decoded.get(\"job_id\") == job_id",
            warning_group: "[redis-wait]",
        },
        Case {
            name: "redis waiter ignores advancing cursor",
            path: "example-api/example/core/redis_client.py",
            from: "client.xread({stream_key: cursor}",
            to: "client.xread({stream_key: \"$\"}",
            warning_group: "[redis-wait]",
        },
        Case {
            name: "bearer is persisted in state",
            path: "example-platform/example-server/src/lib.rs",
            from: "flight_ingest: Arc<crate::flight_ingest::FlightIngestState>,",
            to: "flight_ingest: Arc<crate::flight_ingest::FlightIngestState>,\n    authorization: HeaderValue,",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "bearer is persisted in auth context",
            path: "example-platform/example-server/src/auth.rs",
            from: "struct AuthContext { tenant_id: String, token_id: Option<String> }",
            to: "struct AuthContext { tenant_id: String, token_id: Option<String>, bearer: String }",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "bearer is serialized into a record",
            path: "example-platform/example-server/src/lib.rs",
            from: "struct AuditRecord { request_id: String }",
            to: "struct AuditRecord { request_id: String, authorization: String }",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "bearer hides in request headers field",
            path: "example-platform/example-server/src/lib.rs",
            from: "struct AuditRecord { request_id: String }",
            to: "struct AuditRecord { request_id: String, request_headers: HeaderMap }",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "bearer persists under arbitrary token field name",
            path: "example-platform/example-server/src/lib.rs",
            from: "struct AuditRecord { request_id: String }",
            to: "struct AuditRecord { request_id: String, token: HeaderValue }",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "bearer persists under arbitrary headers field name",
            path: "example-platform/example-server/src/lib.rs",
            from: "struct AuditRecord { request_id: String }",
            to: "struct AuditRecord { request_id: String, headers: HeaderMap }",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "bearer helper logs arbitrary credential local",
            path: "example-platform/example-server/src/router.rs",
            from: "headers.get(axum::http::header::AUTHORIZATION).cloned()",
            to: "{ let value = headers.get(axum::http::header::AUTHORIZATION).cloned(); dbg!(&value); value }",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "bearer helper retains canonical decoy but returns another header",
            path: "example-platform/example-server/src/router.rs",
            from: "headers.get(axum::http::header::AUTHORIZATION).cloned()",
            to: "{ let _canonical = headers.get(axum::http::header::AUTHORIZATION).cloned(); headers.get(\"X-Debug\").cloned() }",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "bearer apply helper adds permissive early return",
            path: "example-platform/example-server/src/router.rs",
            from: "match authorization {",
            to: "if allow_unsafe() { return request; }\n    match authorization {",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "proxy drops request local bearer",
            path: "example-platform/example-server/src/lib.rs",
            from: PROXY_TUNE_FIXTURE,
            to: "async fn proxy_tune(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>, headers: HeaderMap) { let _request = state.http_client.post(url); }",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "proxy logs bearer",
            path: "example-platform/example-server/src/lib.rs",
            from: "let authorization = crate::router::inbound_authorization(&headers);",
            to: "let authorization = crate::router::inbound_authorization(&headers); tracing::info!(authorization = ?authorization);",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "proxy logs arbitrary credential local",
            path: "example-platform/example-server/src/lib.rs",
            from: PROXY_TUNE_FIXTURE,
            to: "async fn proxy_tune(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>, headers: HeaderMap) { let value = crate::router::inbound_authorization(&headers); tracing::info!(credential = ?value); let _request = crate::router::apply_authorization(state.http_client.post(url), value.as_ref()); }",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "proxy logs raw header map",
            path: "example-platform/example-server/src/lib.rs",
            from: PROXY_TUNE_FIXTURE,
            to: "async fn proxy_tune(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>, headers: HeaderMap) { tracing::info!(credential_headers = ?headers); let authorization = crate::router::inbound_authorization(&headers); let _request = crate::router::apply_authorization(state.http_client.post(url), authorization.as_ref()); }",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "proxy logs aliased credential local",
            path: "example-platform/example-server/src/lib.rs",
            from: PROXY_TUNE_FIXTURE,
            to: "async fn proxy_tune(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>, headers: HeaderMap) { let authorization = crate::router::inbound_authorization(&headers); let value = authorization; tracing::info!(credential = ?value); let _request = crate::router::apply_authorization(state.http_client.post(url), value.as_ref()); }",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "proxy logs destructured credential local",
            path: "example-platform/example-server/src/lib.rs",
            from: PROXY_TUNE_FIXTURE,
            to: "async fn proxy_tune(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>, headers: HeaderMap) { let Some(value) = crate::router::inbound_authorization(&headers) else { return; }; tracing::info!(credential = ?value); let _request = crate::router::apply_authorization(state.http_client.post(url), Some(&value)); }",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "proxy logs tuple-destructured credential alias",
            path: "example-platform/example-server/src/lib.rs",
            from: PROXY_TUNE_FIXTURE,
            to: "async fn proxy_tune(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>, headers: HeaderMap) { let authorization = crate::router::inbound_authorization(&headers); let alias = authorization.as_ref(); let (credential, _) = (alias, ()); tracing::info!(credential = ?credential); let _request = crate::router::apply_authorization(state.http_client.post(url), authorization.as_ref()); }",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "proxy logs transformed bearer bytes",
            path: "example-platform/example-server/src/lib.rs",
            from: PROXY_TUNE_FIXTURE,
            to: "async fn proxy_tune(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>, headers: HeaderMap) { let authorization = crate::router::inbound_authorization(&headers); let bytes = authorization.as_ref().map(HeaderValue::as_bytes); tracing::event!(tracing::Level::INFO, ?bytes); let _request = crate::router::apply_authorization(state.http_client.post(url), authorization.as_ref()); }",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "proxy tracing event logs raw headers",
            path: "example-platform/example-server/src/lib.rs",
            from: PROXY_TUNE_FIXTURE,
            to: "async fn proxy_tune(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>, headers: HeaderMap) { tracing::event!(tracing::Level::INFO, ?headers); let authorization = crate::router::inbound_authorization(&headers); let _request = crate::router::apply_authorization(state.http_client.post(url), authorization.as_ref()); }",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "proxy log macro logs raw headers",
            path: "example-platform/example-server/src/lib.rs",
            from: PROXY_TUNE_FIXTURE,
            to: "async fn proxy_tune(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>, headers: HeaderMap) { log::log!(log::Level::Info, \"{:?}\", headers); let authorization = crate::router::inbound_authorization(&headers); let _request = crate::router::apply_authorization(state.http_client.post(url), authorization.as_ref()); }",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "proxy tracing instrument captures headers",
            path: "example-platform/example-server/src/lib.rs",
            from: "async fn proxy_tune(",
            to: "#[tracing::instrument]\nasync fn proxy_tune(",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "proxy instrument skips parameter but records explicit header field",
            path: "example-platform/example-server/src/lib.rs",
            from: "async fn proxy_tune(",
            to: "#[instrument(skip(headers), fields(credential = ?headers))]\nasync fn proxy_tune(",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "proxy imported instrument captures headers",
            path: "example-platform/example-server/src/lib.rs",
            from: "async fn proxy_tune(",
            to: "#[instrument]\nasync fn proxy_tune(",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "proxy discards inbound bearer and forwards none",
            path: "example-platform/example-server/src/lib.rs",
            from: PROXY_TUNE_FIXTURE,
            to: "async fn proxy_tune(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>, headers: HeaderMap) { let _ignored = crate::router::inbound_authorization(&headers); let authorization: Option<HeaderValue> = None; let _response = crate::router::apply_authorization(state.http_client.post(url), authorization.as_ref()).send(); }",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "proxy logs UFCS-transformed bearer bytes",
            path: "example-platform/example-server/src/lib.rs",
            from: PROXY_TUNE_FIXTURE,
            to: "async fn proxy_tune(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>, headers: HeaderMap) { let authorization = crate::router::inbound_authorization(&headers); let bytes = HeaderValue::as_bytes(authorization.as_ref().unwrap()); tracing::event!(tracing::Level::INFO, ?bytes); let _request = crate::router::apply_authorization(state.http_client.post(url), authorization.as_ref()); }",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "proxy keeps decoy apply but sends directly",
            path: "example-platform/example-server/src/lib.rs",
            from: PROXY_TUNE_FIXTURE,
            to: "async fn proxy_tune(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>, headers: HeaderMap) { let authorization = crate::router::inbound_authorization(&headers); let _decoy = crate::router::apply_authorization(state.http_client.post(url), authorization.as_ref()); let _response = state.http_client.post(url).send(); }",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "proxy hides decoy apply and direct send in one tuple expression",
            path: "example-platform/example-server/src/lib.rs",
            from: PROXY_TUNE_FIXTURE,
            to: "async fn proxy_tune(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>, headers: HeaderMap) { let authorization = crate::router::inbound_authorization(&headers); let _response = (crate::router::apply_authorization(state.http_client.post(url), authorization.as_ref()), state.http_client.post(url).send()).1; }",
            warning_group: "[bearer-forwarding]",
        },
        Case {
            name: "proxy keeps authorized builder decoy but executes unauthenticated request",
            path: "example-platform/example-server/src/lib.rs",
            from: PROXY_TUNE_FIXTURE,
            to: "async fn proxy_tune(State(state): State<Arc<ServerState>>, Extension(auth_context): Extension<auth::AuthContext>, headers: HeaderMap) { let authorization = crate::router::inbound_authorization(&headers); let _decoy = crate::router::apply_authorization(state.http_client.post(url), authorization.as_ref()); let _response = state.http_client.execute(unauthorized_request); }",
            warning_group: "[bearer-forwarding]",
        },
    ];

    for case in cases {
        let fixture = Fixture::new();
        fixture.replace_once(case.path, case.from, case.to);
        let warnings = fixture.warnings();
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains(case.warning_group)),
            "case `{}` expected warning group {}; got {warnings:#?}",
            case.name,
            case.warning_group
        );
        assert!(
            warnings
                .iter()
                .all(|warning| warning.contains(case.warning_group)),
            "case `{}` changed one invariant but emitted collateral groups: {warnings:#?}",
            case.name
        );
    }
}

#[test]
fn platform_runtime_trust_boundary_ignores_comment_and_cfg_test_decoys() {
    let fixture = Fixture::new();
    let warnings = fixture.warnings();
    assert!(warnings.is_empty(), "decoys must be ignored: {warnings:#?}");
}

#[test]
fn platform_runtime_trust_boundary_accepts_instrument_when_headers_are_skipped() {
    let fixture = Fixture::new();
    fixture.replace_once(
        "example-platform/example-server/src/lib.rs",
        "async fn proxy_tune(",
        "#[tracing::instrument(skip(headers))]\nasync fn proxy_tune(",
    );
    let warnings = fixture.warnings();
    assert!(
        warnings.is_empty(),
        "instrumentation that skips request headers should remain valid: {warnings:#?}"
    );
}

fn _assert_path(_: &Path) {}
