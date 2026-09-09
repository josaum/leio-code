use std::fs;
use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::query_id;
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct FrontendCongruenceDoctor;

impl Doctor for FrontendCongruenceDoctor {
    fn name(&self) -> &'static str {
        "frontend-engine-client"
    }

    fn description(&self) -> &'static str {
        "Guards the single Example Ops frontend, shared engine transport/views/contracts, and authenticated parity surfaces."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_frontend_engine_client(index, root)
    }
}

pub fn doctor_frontend_engine_client(_index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let gym_package = root.join("example-ops/package.json");
    let shared_transport = root.join("packages/trpc/src/client/example.ts");
    let trpc_route = root.join("example-ops/src/app/api/trpc/[trpc]/route.ts");
    let trpc_src = read_optional(&trpc_route).unwrap_or_default();

    let legacy_ops_absent = !root.join("ops-console/package.json").is_file();
    let legacy_autopilot_absent = !root.join("jai-autopilot/package.json").is_file();
    let local_engine_bff = trpc_src.contains("fetchRequestHandler")
        && trpc_src.contains("createEngineRuntimeRouter")
        && trpc_src.contains("createEngineCollectionsRouter")
        && trpc_src.contains("createRecoveryPlaybooksRouter")
        && !trpc_src.contains("AUTOPILOT_API_URL")
        && !trpc_src.contains("handleTrpcProxy");

    if !gym_package.is_file() {
        warnings.push("example-ops: canonical Example Ops UI package is missing".to_string());
    }
    if !shared_transport.is_file() {
        warnings.push("packages/trpc: shared Example transport is missing".to_string());
    }
    if !local_engine_bff {
        warnings.push(
            "example-ops: /api/trpc local engine BFF must host the shared engine routers locally without an Autopilot upstream"
                .to_string(),
        );
    }
    if !legacy_ops_absent {
        warnings.push("ops-console: retired duplicate UI package exists".to_string());
    }
    if !legacy_autopilot_absent {
        warnings.push("jai-autopilot: retired duplicate product package exists".to_string());
    }

    let mut shared_engine_status = Vec::new();
    check_shared_engine_sources(root, &mut shared_engine_status, &mut warnings);
    check_generated_contracts(root, &mut entities, &mut evidence, &mut warnings);
    check_optional_health_audit(root, &mut entities, &mut evidence, &mut warnings);
    check_optional_vigoros(root, &mut entities, &mut evidence, &mut warnings);
    check_critical_script_paths(root, &mut entities, &mut evidence, &mut warnings);
    check_runtime_boundary(root, &mut evidence, &mut warnings);

    entities.push(json!({
        "canonical_ops_ui": "example-ops",
        "production_url": "https://app.getjai.com",
        "shared_transport_present": shared_transport.is_file(),
        "local_engine_bff": local_engine_bff,
        "legacy_ops_console_absent": legacy_ops_absent,
        "legacy_jai_autopilot_absent": legacy_autopilot_absent,
        "shared_engine_sources": shared_engine_status,
    }));
    evidence.push(EvidenceItem {
        kind: "frontend_engine_client".to_string(),
        path: trpc_route.display().to_string(),
        line: None,
        detail: "canonical authenticated engine BFF for the single Example Ops UI".to_string(),
    });

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_frontend_engine_client"),
        kind: "frontend_engine_client".to_string(),
        summary: format!(
            "checked unified Example Ops engine client and parity surfaces; found {} warnings",
            warnings.len()
        ),
        confidence: if warnings.is_empty() { 0.99 } else { 0.71 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn read_optional(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

/// Source texts the runtime-boundary markers are read from.
///
/// Named fields rather than positional `&str` arguments: every one of these is
/// the same type, so a transposed pair still compiles and silently changes
/// which file each contract is asserted against.
#[derive(Default)]
struct BoundarySources<'a> {
    resolver: &'a str,
    route: &'a str,
    gateway_routes: &'a str,
    gateway_chat: &'a str,
    gateway_conversations: &'a str,
    monitoring_client: &'a str,
    ingress: &'a str,
    firewall: &'a str,
    whatsapp_tool: &'a str,
}

fn runtime_boundary_markers(sources: &BoundarySources<'_>) -> (bool, bool, bool, bool) {
    let BoundarySources {
        resolver,
        route,
        gateway_routes,
        gateway_chat,
        gateway_conversations,
        monitoring_client,
        ingress,
        firewall,
        whatsapp_tool,
    } = *sources;
    let runtime_routes = resolver.contains("RUST_DATA_PLANE_RULES")
        && resolver.contains("ops_console\\/api\\/conversations")
        && resolver.contains("analytics\\/realtime")
        && resolver.contains("ops_console/api/takeover/history")
        && resolver.contains("ops_console/api/takeover/status/")
        && resolver.contains("ops_console/api/takeover/permission")
        && gateway_routes.contains("/api/takeover/history")
        && gateway_routes.contains("/api/takeover/status/:session_id")
        && gateway_routes.contains("/api/takeover/permission")
        && gateway_routes.contains("/api/conversations/events")
        && gateway_routes.contains("/api/conversations/:session_id/events");
    let rust_request_contracts = resolver.contains("transformRustDataPlaneRequest")
        && resolver.contains("supervisor_id")
        && resolver.contains("reason_code")
        && resolver.contains("handback_handler_id")
        && resolver.contains("analyst_id")
        && resolver.contains("resolution")
        && route.contains("transformRustDataPlaneRequest")
        && route.contains("rewriteRustDataPlaneSearchParams")
        && gateway_chat.contains("Json(form): Json<SendTemplateForm>")
        && gateway_conversations.contains("TenantPrincipal")
        && gateway_conversations.contains("principal_can_read_conversation")
        && gateway_conversations.contains("enforced_conversation_tenant")
        && gateway_conversations
            .matches("principal: TenantPrincipal")
            .count()
            >= 7
        && monitoring_client.contains("ReleaseTakeoverParams")
        && monitoring_client.contains("analystId: string")
        && monitoring_client.contains("analyst_id: params.analystId");
    let rust_ingress = ingress.contains("RUST_WEBHOOK_TARGET")
        && ingress.contains("/ingest")
        && !ingress.contains("PYTHON_WEBHOOK_TARGET");
    let rust_egress = firewall.contains("/api/communications/outbound/intent")
        && firewall.contains(r#""permissions": ["egress:submit"]"#)
        && whatsapp_tool.contains("handoff_to_system2")
        && !firewall.contains("WHATSAPP_DIRECT_FALLBACK")
        && !whatsapp_tool.contains("WHATSAPP_DIRECT_FALLBACK")
        && !whatsapp_tool.contains("SARA_DIRECT_WHATSAPP_FALLBACK");
    (
        runtime_routes,
        rust_request_contracts,
        rust_ingress,
        rust_egress,
    )
}

/// Gateway source texts checked for gated runtime surfaces. Named fields for
/// the same reason as [`BoundarySources`]: all eleven are `&str`.
#[derive(Default)]
struct GatewaySources<'a> {
    main: &'a str,
    whatsapp_messages: &'a str,
    whatsapp_channel: &'a str,
    egress_handler: &'a str,
    jwt_middleware: &'a str,
    chatwoot_ingest: &'a str,
    sse_routes: &'a str,
    conversation_routes: &'a str,
    conversation_data_access: &'a str,
    resolver: &'a str,
    whatsapp_client: &'a str,
}

fn gateway_exposes_only_gated_runtime_surfaces(sources: &GatewaySources<'_>) -> bool {
    let GatewaySources {
        main,
        whatsapp_messages,
        whatsapp_channel,
        egress_handler,
        jwt_middleware,
        chatwoot_ingest,
        sse_routes,
        conversation_routes,
        conversation_data_access,
        resolver,
        whatsapp_client,
    } = *sources;
    main.contains("/api/communications/outbound/intent")
        && main.contains(".nest(\"/ops_console\"")
        && main.contains("/api/runtime/events")
        && main.contains("get(ops_console::routes::sse_handler)")
        && !main.contains("api::runtime_events::runtime_events_sse_handler")
        && !main.contains("/api/whatsapp/phone_numbers/:phone_id/messages/")
        && !whatsapp_messages.contains("let tenant_id = \"__system__\"")
        && !whatsapp_channel.contains("\"__system__\"")
        && egress_handler.contains("principal: TenantPrincipal")
        && egress_handler.contains("principal_can_submit_egress_for_tenant")
        && egress_handler.contains("principal_can_submit_egress")
        && egress_handler.contains("principal_can_review_egress")
        && egress_handler.contains("phone_route_belongs_to_tenant")
        && egress_handler.contains("principal_is_internal_campaign_worker")
        && egress_handler.contains("submit_internal_outbound_intent")
        && egress_handler.contains("generate_access_token")
        && egress_handler.contains(".bearer_auth(service_token)")
        && egress_handler.contains("internal_http_base_url")
        && egress_handler.contains("/api/communications/outbound/intent")
        && !jwt_middleware.contains("GATEWAY_INTERNAL_TRUST_LOOPBACK")
        && !jwt_middleware.contains("is_internal_trusted_loopback")
        && chatwoot_ingest.contains("submit_internal_outbound_intent")
        && !chatwoot_ingest.contains(".header(\"x-tenant-id\"")
        && sse_routes.contains("TenantPrincipal")
        && sse_routes.contains("RuntimeEventScope")
        && sse_routes.contains("principal_tenant")
        && sse_routes.contains("query_conversation_tenant")
        && sse_routes.contains("let Some(tenant)")
        && conversation_routes.contains("query_conversation_tenant")
        && conversation_data_access.contains("pub async fn query_conversation_tenant")
        && conversation_data_access.contains("missing session or database error must fail closed")
        && !resolver.contains("messages\\/text")
        && !resolver.contains("messages\\/template")
        && !whatsapp_client.contains("class MessagingApi")
}

fn check_runtime_boundary(
    root: &Path,
    evidence: &mut Vec<EvidenceItem>,
    warnings: &mut Vec<String>,
) {
    if !root.join("example-ops/package.json").is_file() {
        return;
    }

    let resolver_path = root.join("example-ops/src/app/api/gateway/resolve-target.ts");
    let route_path = root.join("example-ops/src/app/api/gateway/[...path]/route.ts");
    let gateway_routes_path = root.join("example-gateway/src/ops_console/routes/mod.rs");
    let gateway_chat_path = root.join("example-gateway/src/ops_console/routes/chat.rs");
    let gateway_conversations_path =
        root.join("example-gateway/src/ops_console/routes/conversations.rs");
    let monitoring_client_path = root.join("example-ops/src/lib/api/monitoring.ts");
    let gateway_main_path = root.join("example-gateway/src/main.rs");
    let whatsapp_messages_path = root.join("example-gateway/src/whatsapp/messages.rs");
    let whatsapp_channel_path = root.join("example-gateway/src/channels/whatsapp.rs");
    let egress_handler_path = root.join("example-gateway/src/server/handlers/egress.rs");
    let jwt_middleware_path = root.join("example-gateway/src/server/jwt_middleware.rs");
    let chatwoot_ingest_path = root.join("example-gateway/src/ingest/chatwoot/mod.rs");
    let gateway_sse_routes_path = root.join("example-gateway/src/ops_console/routes/sse_routes.rs");
    let conversation_data_access_path =
        root.join("example-gateway/src/ops_console/routes/helpers/data_access.rs");
    let whatsapp_client_path = root.join("example-ops/src/lib/api/WhatsAppClient.ts");
    let ingress_path = root.join("example-ops/src/app/ingest/route.ts");
    let firewall_path = root.join("example-api/example/egress/firewall.py");
    let tool_path = root.join("example-api/example/integrations/whatsapp/tool.py");
    if !resolver_path.is_file()
        && !ingress_path.is_file()
        && !firewall_path.is_file()
        && !tool_path.is_file()
    {
        return;
    }
    let resolver = read_optional(&resolver_path).unwrap_or_default();
    let route = read_optional(&route_path).unwrap_or_default();
    let gateway_routes = read_optional(&gateway_routes_path).unwrap_or_default();
    let gateway_chat = read_optional(&gateway_chat_path).unwrap_or_default();
    let gateway_conversations = read_optional(&gateway_conversations_path).unwrap_or_default();
    let monitoring_client = read_optional(&monitoring_client_path).unwrap_or_default();
    let gateway_main = read_optional(&gateway_main_path).unwrap_or_default();
    let whatsapp_messages = read_optional(&whatsapp_messages_path).unwrap_or_default();
    let whatsapp_channel = read_optional(&whatsapp_channel_path).unwrap_or_default();
    let egress_handler = read_optional(&egress_handler_path).unwrap_or_default();
    let jwt_middleware = read_optional(&jwt_middleware_path).unwrap_or_default();
    let chatwoot_ingest = read_optional(&chatwoot_ingest_path).unwrap_or_default();
    let gateway_sse_routes = read_optional(&gateway_sse_routes_path).unwrap_or_default();
    let conversation_data_access =
        read_optional(&conversation_data_access_path).unwrap_or_default();
    let whatsapp_client = read_optional(&whatsapp_client_path).unwrap_or_default();
    let ingress = read_optional(&ingress_path).unwrap_or_default();
    let firewall = read_optional(&firewall_path).unwrap_or_default();
    let whatsapp_tool = read_optional(&tool_path).unwrap_or_default();
    let (runtime_routes, rust_request_contracts, rust_ingress, rust_egress) =
        runtime_boundary_markers(&BoundarySources {
            resolver: &resolver,
            route: &route,
            gateway_routes: &gateway_routes,
            gateway_chat: &gateway_chat,
            gateway_conversations: &gateway_conversations,
            monitoring_client: &monitoring_client,
            ingress: &ingress,
            firewall: &firewall,
            whatsapp_tool: &whatsapp_tool,
        });
    let gated_gateway_surfaces = gateway_exposes_only_gated_runtime_surfaces(&GatewaySources {
        main: &gateway_main,
        whatsapp_messages: &whatsapp_messages,
        whatsapp_channel: &whatsapp_channel,
        egress_handler: &egress_handler,
        jwt_middleware: &jwt_middleware,
        chatwoot_ingest: &chatwoot_ingest,
        sse_routes: &gateway_sse_routes,
        conversation_routes: &gateway_conversations,
        conversation_data_access: &conversation_data_access,
        resolver: &resolver,
        whatsapp_client: &whatsapp_client,
    });

    if !runtime_routes {
        warnings.push(
            "example-ops: live conversations/realtime are not pinned to the Rust data plane"
                .to_string(),
        );
    }
    if !rust_request_contracts {
        warnings.push(
            "example-ops: Rust takeover/release/close/template request contracts are incomplete or bypassed"
                .to_string(),
        );
    }
    if !rust_ingress {
        warnings.push(
            "example-ops: WhatsApp webhook ingress can drift away from Rust /ingest".to_string(),
        );
    }
    if !rust_egress {
        warnings.push(
            "example-api: Python direct-WhatsApp fallback bypasses the Rust three-gate egress path"
                .to_string(),
        );
    }
    if !gated_gateway_surfaces {
        warnings.push(
            "example-gateway: public runtime SSE or raw Meta message routes bypass the authenticated tenant-scoped Ops/egress surfaces"
                .to_string(),
        );
    }

    evidence.push(EvidenceItem {
        kind: "frontend_runtime_boundary".to_string(),
        path: resolver_path.display().to_string(),
        line: None,
        detail: format!(
            "runtime_routes={runtime_routes}, rust_request_contracts={rust_request_contracts}, rust_ingress={rust_ingress}, rust_egress={rust_egress}, gated_gateway_surfaces={gated_gateway_surfaces}"
        ),
    });
}

fn check_shared_engine_sources(
    root: &Path,
    status: &mut Vec<serde_json::Value>,
    warnings: &mut Vec<String>,
) {
    // Keep tiny doctor fixtures useful: the detailed matrix is enabled once
    // packages/trpc is materialized as a real workspace package. The top-level
    // single-UI/BFF guards above still run for a minimal checkout.
    if !root.join("packages/trpc/package.json").is_file() {
        return;
    }
    let shared_sources: &[(&str, &[&str], &str)] = &[
        (
            "packages/trpc/src/client/example.ts",
            &["export class ExampleClientBase"],
            "shared Example client base",
        ),
        (
            "packages/trpc/src/client/example-kb.ts",
            &[
                "export async function listExampleCollections(",
                "export async function knowledgeRetrieve(",
                "export async function ragSearch(",
            ],
            "shared knowledge client",
        ),
        (
            "packages/trpc/src/server/exampleTransport.ts",
            &[
                "export interface ExampleServerTransportOptions",
                "export function makeExampleHeaders(",
                "export async function requestExampleJson<",
            ],
            "shared server transport",
        ),
    ];

    for (relative, markers, label) in shared_sources {
        let path = root.join(relative);
        let ok = read_optional(&path)
            .is_some_and(|src| markers.iter().all(|marker| src.contains(marker)));
        if !ok {
            warnings.push(format!("packages/trpc is missing {label}"));
        }
        status.push(json!({ "path": relative, "present": ok }));
    }

    let client_specs: &[(&str, &[&str])] = &[
        (
            "engineSearchTrpc.ts",
            &["createEngineSearchTrpcClient(", "engineSearch.search"],
        ),
        (
            "engineCollectionsTrpc.ts",
            &[
                "createEngineCollectionsTrpcClient(",
                "engineCollections.list",
            ],
        ),
        (
            "engineBanditsTrpc.ts",
            &["createEngineBanditsTrpcClient(", "engineBandits.list"],
        ),
        (
            "engineGepaTrpc.ts",
            &["createEngineGepaTrpcClient(", "engineGepa.list"],
        ),
        (
            "engineDiagnosticsTrpc.ts",
            &[
                "createEngineDiagnosticsTrpcClient(",
                "engineDiagnostics.flowScore",
            ],
        ),
        (
            "engineSpeechTrpc.ts",
            &["createEngineSpeechTrpcClient(", "engineSpeech.transcribe"],
        ),
        (
            "engineEmbeddingsTrpc.ts",
            &[
                "createEngineEmbeddingsTrpcClient(",
                "engineEmbeddings.embed",
            ],
        ),
        (
            "engineGenerateTrpc.ts",
            &["createEngineGenerateTrpcClient(", "engineGenerate.generate"],
        ),
        (
            "engineIngestTrpc.ts",
            &[
                "createEngineIngestTrpcClient(",
                "engineIngest.ingestDocuments",
            ],
        ),
        (
            "engineSystem2Trpc.ts",
            &["createEngineSystem2TrpcClient(", "engineSystem2.route"],
        ),
        (
            "engineRuntimeTrpc.ts",
            &["createEngineRuntimeTrpcClient(", "engineRuntime.models"],
        ),
        (
            "engineAdminTrpc.ts",
            &["createEngineAdminTrpcClient(", "engineAdmin.namespaces"],
        ),
        (
            "engineKnowledgeTrpc.ts",
            &["createEngineKnowledgeTrpcClient(", "knowledgeRetrieve(data"],
        ),
    ];
    for (file, markers) in client_specs {
        let relative = format!("packages/trpc/src/client/{file}");
        let ok = source_contains_all(&root.join(&relative), markers);
        if !ok {
            warnings.push(format!(
                "packages/trpc is missing shared engine client {file}"
            ));
        }
        status.push(json!({ "path": relative, "present": ok }));
    }

    let server_specs: &[(&str, &[&str])] = &[
        (
            "engineSearchRouter.ts",
            &["createEngineSearchRouter(", "protectedProcedure"],
        ),
        (
            "engineCollectionsRouter.ts",
            &["createEngineCollectionsRouter(", "protectedProcedure"],
        ),
        (
            "engineBanditsRouter.ts",
            &["createEngineBanditsRouter(", "protectedProcedure"],
        ),
        (
            "engineGepaRouter.ts",
            &["createEngineGepaRouter(", "protectedProcedure"],
        ),
        (
            "engineDiagnosticsRouter.ts",
            &["createEngineDiagnosticsRouter(", "protectedProcedure"],
        ),
        (
            "engineSpeechRouter.ts",
            &["createEngineSpeechRouter(", "protectedProcedure"],
        ),
        (
            "engineEmbeddingsRouter.ts",
            &["createEngineEmbeddingsRouter(", "protectedProcedure"],
        ),
        (
            "engineGenerateRouter.ts",
            &["createEngineGenerateRouter(", "protectedProcedure"],
        ),
        (
            "engineIngestRouter.ts",
            &["createEngineIngestRouter(", "protectedProcedure"],
        ),
        (
            "engineSystem2Router.ts",
            &["createEngineSystem2Router(", "protectedProcedure"],
        ),
        (
            "engineRuntimeRouter.ts",
            &["createEngineRuntimeRouter(", "protectedProcedure"],
        ),
        (
            "engineAdminRouter.ts",
            &["createEngineAdminRouter(", "protectedProcedure"],
        ),
    ];
    for (file, markers) in server_specs {
        let relative = format!("packages/trpc/src/server/{file}");
        let ok = source_contains_all(&root.join(&relative), markers);
        if !ok {
            warnings.push(format!(
                "packages/trpc is missing shared engine router {file}"
            ));
        }
        status.push(json!({ "path": relative, "present": ok }));
    }

    let view_specs: &[(&str, &str, &str)] = &[
        ("search", "SearchPage.tsx", "EngineSearchPage"),
        (
            "collections",
            "CollectionsPage.tsx",
            "EngineCollectionsPage",
        ),
        ("bandits", "BanditsPage.tsx", "EngineBanditsPage"),
        ("gepa", "GepaPage.tsx", "EngineGepaPage"),
        (
            "diagnostics",
            "DiagnosticsPage.tsx",
            "EngineDiagnosticsPage",
        ),
        ("speech", "SpeechPage.tsx", "EngineSpeechPage"),
        ("embeddings", "EmbeddingsPage.tsx", "EngineEmbeddingsPage"),
        ("generate", "GeneratePage.tsx", "EngineGeneratePage"),
        ("system2", "System2Page.tsx", "EngineSystem2Page"),
        ("models", "ModelsPage.tsx", "EngineModelsPage"),
        ("tasks", "TasksPage.tsx", "EngineTasksPage"),
        ("admin", "AdminPage.tsx", "EngineAdminPage"),
    ];
    for (route, view_file, export_name) in view_specs {
        let view_relative = format!("packages/trpc/src/views/engine/{view_file}");
        let route_relative = format!("example-ops/src/components/engine/views/{route}/page.tsx");
        let view_module = view_file.strip_suffix(".tsx").unwrap_or(view_file);
        let view_ok = source_contains_all(
            &root.join(&view_relative),
            &[&format!("export function {export_name}()")],
        );
        let route_ok = source_contains_all(
            &root.join(&route_relative),
            &[&format!("@jai/trpc/views/engine/{view_module}"), "export {"],
        );
        if !view_ok {
            warnings.push(format!(
                "packages/trpc is missing shared engine view {view_file}"
            ));
        }
        if !route_ok {
            warnings.push(format!(
                "example-ops is missing shared engine view route {route}"
            ));
        }
        status.push(json!({
            "path": view_relative,
            "present": view_ok,
            "gym_route": route_relative,
            "gym_route_present": route_ok,
        }));
    }
}

fn check_generated_contracts(
    root: &Path,
    entities: &mut Vec<serde_json::Value>,
    evidence: &mut Vec<EvidenceItem>,
    warnings: &mut Vec<String>,
) {
    let source_dir = root.join("packages/example-ops-contracts/generated");
    let vendor_dir = root.join("example-ops/vendor/example-ops-contracts/generated");
    let tsconfig = root.join("example-ops/tsconfig.json");
    let jest_config = root.join("example-ops/jest.config.cjs");

    // Minimal unit fixtures do not materialize the contracts package. In a
    // real workspace, however, the package and its vendored copy are both
    // required because the browser bundle must not reach outside example-ops.
    if !source_dir.exists() && !vendor_dir.exists() && !tsconfig.exists() {
        return;
    }

    let required = [
        "AgentConfiguration.ts",
        "QueueCase.ts",
        "whatsapp/Flow.ts",
        "whatsapp/FlowScreen.ts",
    ];
    let mut source_ok = true;
    let mut vendor_ok = true;
    for relative in required {
        source_ok &= source_dir.join(relative).is_file();
        vendor_ok &= vendor_dir.join(relative).is_file();
    }
    if !source_ok {
        warnings
            .push("generated contracts: canonical generated bindings are incomplete".to_string());
    }
    if !vendor_ok {
        warnings.push("example-ops: vendored generated contracts are incomplete".to_string());
    }

    let tsconfig_src = read_optional(&tsconfig).unwrap_or_default();
    let tsconfig_ok = tsconfig_src.contains("\"@contracts/generated/*\"")
        && tsconfig_src.contains("./vendor/example-ops-contracts/generated/*")
        && tsconfig_src.contains("\"@contracts/analytics/*\"")
        && !tsconfig_src.contains("../packages/example-ops-contracts/generated/*");
    if !tsconfig_ok {
        warnings.push(
            "example-ops: tsconfig must resolve generated contracts from its vendored copy"
                .to_string(),
        );
    }
    let jest_ok = if jest_config.is_file() {
        let src = read_optional(&jest_config).unwrap_or_default();
        src.contains("@contracts/generated/(.*)")
            && src.contains("vendor/example-ops-contracts/generated/$1")
            && src.contains("@contracts/analytics/(.*)")
    } else {
        true
    };
    if !jest_ok {
        warnings
            .push("example-ops: Jest does not resolve vendored generated contracts".to_string());
    }

    entities.push(json!({
        "path": source_dir.display().to_string(),
        "canonical_generated_contracts": source_ok,
        "vendored_generated_contracts": vendor_ok,
        "gym_tsconfig_contract_alias": tsconfig_ok,
        "gym_jest_contract_alias": jest_ok,
    }));
    evidence.push(EvidenceItem {
        kind: "generated_contracts".to_string(),
        path: vendor_dir.display().to_string(),
        line: None,
        detail: "example-ops resolves Rust-generated wire contracts from its vendored bundle"
            .to_string(),
    });
}

fn check_optional_health_audit(
    root: &Path,
    entities: &mut Vec<serde_json::Value>,
    evidence: &mut Vec<EvidenceItem>,
    warnings: &mut Vec<String>,
) {
    let app_dir = root.join("health-audit-console");
    if !app_dir.is_dir() {
        return;
    }

    let proxy = app_dir.join("app/api/example/[...path]/route.ts");
    let proxy_ok = source_contains_all(
        &proxy,
        &[
            "handleExampleProxy",
            "handleExampleProxy(request, context, NextResponse)",
        ],
    );
    if !proxy_ok {
        warnings.push(
            "health-audit-console: Example proxy bypasses the shared proxy handler".to_string(),
        );
    }

    let auth = app_dir.join("lib/auth.ts");
    let auth_src = read_optional(&auth).unwrap_or_default();
    let auth_ok = auth_src.contains("@jai/trpc/client/browserSession")
        && auth_src.contains("COOKIE_SESSION_TOKEN")
        && auth_src.contains("fetchAuthResponse('/api/auth/me')")
        && auth_src.contains("fetchAuthResponse('/api/auth/logout'")
        && auth_src.contains("fetchAuthResponse('/api/auth/refresh'")
        && !auth_src.contains("browserTokenAuth")
        && !auth_src.contains("sentinel_auth_token")
        && !auth_src.contains("sentinel_auth_refresh_token");
    if !auth_ok {
        warnings.push(
            "health-audit-console: auth must use the shared same-origin cookie session".to_string(),
        );
    }

    let auth_route_specs: &[(&str, &[&str])] = &[
        (
            "login",
            &["export async function POST", "Set-Cookie", "/v2/auth/login"],
        ),
        (
            "logout",
            &[
                "export async function POST",
                "access_token=; Path=/; HttpOnly",
            ],
        ),
        (
            "me",
            &[
                "export async function GET",
                "request.cookies.get(\"access_token\")",
            ],
        ),
        (
            "refresh",
            &[
                "export async function POST",
                "request.cookies.get(\"refresh_token\")",
            ],
        ),
    ];
    let auth_routes_ok = auth_route_specs.iter().all(|(name, markers)| {
        let path = app_dir.join(format!("app/api/auth/{name}/route.ts"));
        let ok = source_contains_all(&path, markers);
        if !ok {
            warnings.push(format!(
                "health-audit-console: /api/auth/{name} cookie route is incomplete"
            ));
        }
        ok
    });

    let api_base = app_dir.join("lib/apiBase.ts");
    let api_base_ok = source_contains_all(&api_base, &["WEB_PROXY_API_BASE = '/api/example'"]);
    if !api_base_ok {
        warnings.push(
            "health-audit-console: API base must default to the same-origin Example proxy"
                .to_string(),
        );
    }
    let transport = app_dir.join("lib/healthAuditApi.ts");
    let transport_ok = source_contains_all(
        &transport,
        &[
            "export async function healthAuditFetch(",
            "export async function healthAuditJson",
            "authenticatedFetch(",
        ],
    );
    if !transport_ok {
        warnings.push("health-audit-console: health-audit transport helper is missing".to_string());
    }

    let feature_clients = [
        "app/admissions/client.ts",
        "app/contracts/client.ts",
        "app/tiss/client.ts",
        "app/knowledge/client.ts",
    ];
    let feature_clients_ok = feature_clients.iter().all(|relative| {
        let ok = read_optional(&app_dir.join(relative))
            .is_some_and(|src| src.contains("healthAuditJson") || src.contains("healthAuditFetch"));
        if !ok {
            warnings.push(format!(
                "health-audit-console: feature client {relative} bypasses shared transport"
            ));
        }
        ok
    });

    let rules_ok = match (
        read_optional(&app_dir.join("app/rules/page.tsx")),
        read_optional(&app_dir.join("hooks/useRegistryRules.ts")),
        read_optional(&app_dir.join("app/rules/client.ts")),
    ) {
        (Some(page), Some(hook), Some(client)) => {
            health_audit_rules_uses_shared_transport(&page, &hook, &client)
        }
        _ => false,
    };
    if !rules_ok {
        warnings.push("health-audit-console: rules surface bypasses shared transport".to_string());
    }

    entities.push(json!({
        "path": app_dir.display().to_string(),
        "proxy_uses_shared_handler": proxy_ok,
        "cookie_auth": auth_ok,
        "auth_routes": auth_routes_ok,
        "api_base_uses_proxy": api_base_ok,
        "shared_transport": transport_ok,
        "feature_clients_use_shared_transport": feature_clients_ok,
        "rules_use_shared_transport": rules_ok,
    }));
    evidence.push(EvidenceItem {
        kind: "health_audit_parity".to_string(),
        path: proxy.display().to_string(),
        line: None,
        detail: "Health Audit remains a separate domain UI but shares the authenticated Example transport".to_string(),
    });
}

fn check_optional_vigoros(
    root: &Path,
    entities: &mut Vec<serde_json::Value>,
    evidence: &mut Vec<EvidenceItem>,
    warnings: &mut Vec<String>,
) {
    let app_dir = root.join("vigoros/app");
    if !app_dir.is_dir() {
        return;
    }
    let api = app_dir.join("src/services/exampleApi.ts");
    let operator = app_dir.join("src/lib/vigorosOperator.ts");
    let store = app_dir.join("src/store/index.ts");
    let tsconfig = app_dir.join("tsconfig.app.json");

    let api_src = read_optional(&api).unwrap_or_default();
    let api_ok = vigoros_auth_uses_shared_transport(&api_src)
        && api_src.contains("createBrowserCookieSessionClient")
        && api_src.contains("fetchExampleApiResponse")
        && api_src.contains("resolveExampleApiBaseUrl");
    if !api_ok {
        warnings.push(
            "vigoros: Example API/auth surface bypasses shared browser transport".to_string(),
        );
    }
    let operator_ok = source_contains_all(
        &operator,
        &[
            "from '@/services/exampleApi'",
            "fetchExampleApiResponse(",
            "resolveExampleApiBaseUrl()",
        ],
    ) && !read_optional(&operator)
        .unwrap_or_default()
        .contains("makeBearerAuthHeaders(");
    if !operator_ok {
        warnings.push("vigoros: operator duplicates auth/base-fetch transport".to_string());
    }
    let store_src = read_optional(&store).unwrap_or_default();
    let store_ok = store_src.contains("clearBrowserStorageString")
        && store_src.contains("clearStoredAuthSession")
        && store_src.contains("logoutAuthSession")
        && !store_src.contains("window.localStorage.removeItem('vigoros-storage')");
    if !store_ok {
        warnings.push("vigoros: store bypasses shared auth-session cleanup".to_string());
    }
    let tsconfig_ok = source_contains_all(&tsconfig, &["\"@jai/trpc/*\""]);
    if !tsconfig_ok {
        warnings.push("vigoros: tsconfig is missing the shared @jai/trpc alias".to_string());
    }

    entities.push(json!({
        "path": api.display().to_string(),
        "uses_shared_browser_auth": api_ok,
        "operator_uses_shared_transport": operator_ok,
        "store_uses_shared_auth_session": store_ok,
        "has_trpc_alias": tsconfig_ok,
    }));
    evidence.push(EvidenceItem {
        kind: "vigoros_parity".to_string(),
        path: api.display().to_string(),
        line: None,
        detail: "VIGOROS remains a domain UI and must share the platform auth/transport contract"
            .to_string(),
    });
}

fn check_critical_script_paths(
    root: &Path,
    entities: &mut Vec<serde_json::Value>,
    evidence: &mut Vec<EvidenceItem>,
    warnings: &mut Vec<String>,
) {
    // These scripts are allowed to mention the compatibility route `/ops_console`
    // and `OPS_CONSOLE_ALLOW_WHATSAPP_SEND`. What is forbidden is a physical
    // dependency on the retired `ops-console` tree or its Modal UI deployment.
    let scripts = [
        "example-api/start-chatbot.sh",
        "example-api/stop-chatbot.sh",
        "scripts/compose_smoke.sh",
        "example-gateway/scripts/setup_modal_secrets.py",
        "example-gateway/modal_utilities.py",
        "example-gateway/scripts/ngrok-api-demo.sh",
        "example-gateway/scripts/ngrok-deploy.sh",
        "example-gateway/scripts/ngrok-quick.sh",
        "example-gateway/ngrok.yml",
    ];
    let forbidden = [
        ("ops-console", "retired ops-console filesystem path"),
        (
            "consoleservice-serve.modal.run",
            "retired Modal UI deployment endpoint",
        ),
        ("ops-console-env", "retired Modal UI secret"),
        ("modal_deploy.py", "retired Modal UI deployment script"),
    ];
    let mut checked = Vec::new();
    for relative in scripts {
        let path = root.join(relative);
        let Some(src) = read_optional(&path) else {
            continue;
        };
        checked.push(relative);
        for (needle, description) in forbidden {
            if src.contains(needle) {
                warnings.push(format!(
                    "{relative}: contains {description}; use example-ops/Vercel instead"
                ));
            }
        }
    }
    if checked.is_empty() {
        return;
    }
    entities.push(json!({
        "critical_scripts": checked,
        "forbidden_physical_ui_paths": forbidden.iter().map(|(needle, _)| *needle).collect::<Vec<_>>(),
        "compatibility_tokens_allowed": ["/ops_console", "OPS_CONSOLE_ALLOW_WHATSAPP_SEND"],
    }));
    evidence.push(EvidenceItem {
        kind: "frontend_script_paths".to_string(),
        path: root.join("scripts/compose_smoke.sh").display().to_string(),
        line: None,
        detail: "critical local/Modal/ngrok scripts must not resurrect the retired physical Ops Console UI".to_string(),
    });
}

fn source_contains_all(path: &Path, markers: &[&str]) -> bool {
    read_optional(path).is_some_and(|src| markers.iter().all(|marker| src.contains(marker)))
}

fn health_audit_rules_uses_shared_transport(page: &str, hook: &str, client: &str) -> bool {
    let page_consumes_hook =
        page.contains("useRegistryRules") && page.contains("from '@/hooks/useRegistryRules'");
    let hook_routes_through_client =
        hook.contains("from '@/app/rules/client'") && hook.contains("listRegistryRules");
    let client_uses_shared_transport =
        client.contains("from '../../lib/healthAuditApi'") && client.contains("healthAuditJson");
    page_consumes_hook && hook_routes_through_client && client_uses_shared_transport
}

fn vigoros_auth_uses_shared_transport(src: &str) -> bool {
    src.contains("createBrowserCookieSessionClient")
        && src.contains("@jai/trpc/client/browserSession")
        && src.contains("fetchExampleApiResponse")
        && src.contains("resolveExampleApiBaseUrl")
        && !src.contains("AUTH_ACCESS_TOKEN_STORAGE_KEY")
        && !src.contains("AUTH_REFRESH_TOKEN_STORAGE_KEY")
        && !src.contains("localStorage.setItem('access_token'")
}

#[cfg(test)]
mod tests {
    use super::{
        BoundarySources, GatewaySources, check_critical_script_paths,
        doctor_frontend_engine_client, gateway_exposes_only_gated_runtime_surfaces,
        health_audit_rules_uses_shared_transport, runtime_boundary_markers,
        vigoros_auth_uses_shared_transport,
    };
    use crate::model::RepoIndex;
    use std::fs;
    use std::path::Path;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        fs::write(path, contents).expect("write fixture");
    }

    fn empty_index(root: &Path) -> RepoIndex {
        RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "test".to_string(),
            files: Vec::new(),
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        }
    }

    #[test]
    fn accepts_single_ui_with_local_engine_bff() {
        let root =
            std::env::temp_dir().join(format!("leio-frontend-congruence-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        write(&root.join("example-ops/package.json"), "{}");
        write(
            &root.join("packages/trpc/src/client/example.ts"),
            "export {};",
        );
        write(
            &root.join("example-ops/src/app/api/trpc/[trpc]/route.ts"),
            "fetchRequestHandler createEngineRuntimeRouter createEngineCollectionsRouter createRecoveryPlaybooksRouter",
        );
        let result = doctor_frontend_engine_client(&empty_index(&root), &root);
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn accepts_explicit_rust_runtime_ingress_and_egress_boundary() {
        assert_eq!(
            runtime_boundary_markers(&BoundarySources {
                resolver: "RUST_DATA_PLANE_RULES transformRustDataPlaneRequest ops_console/api/takeover/history ops_console/api/takeover/status/ ops_console/api/takeover/permission supervisor_id reason_code handback_handler_id analyst_id resolution ops_console\\/api\\/conversations analytics\\/realtime",
                route: "transformRustDataPlaneRequest rewriteRustDataPlaneSearchParams",
                gateway_routes: "/api/takeover/history /api/takeover/status/:session_id /api/takeover/permission /api/conversations/events /api/conversations/:session_id/events",
                gateway_chat: "Json(form): Json<SendTemplateForm>",
                gateway_conversations: "TenantPrincipal principal: TenantPrincipal principal: TenantPrincipal principal: TenantPrincipal principal: TenantPrincipal principal: TenantPrincipal principal: TenantPrincipal principal: TenantPrincipal principal_can_read_conversation enforced_conversation_tenant conversations_api messages_api latest_messages_api session_api session_context_api",
                monitoring_client: "ReleaseTakeoverParams analystId: string analyst_id: params.analystId",
                ingress: "const RUST_WEBHOOK_TARGET = `${GATEWAY_URL}/ingest`;",
                firewall: r#"gateway_url = '/api/communications/outbound/intent'; "permissions": ["egress:submit"]"#,
                whatsapp_tool: "handoff_to_system2(intent)",
            }),
            (true, true, true, true),
        );
    }

    #[test]
    fn rejects_python_direct_whatsapp_fallback() {
        assert_eq!(
            runtime_boundary_markers(&BoundarySources {
                resolver: "RUST_DATA_PLANE_RULES transformRustDataPlaneRequest ops_console/api/takeover/history ops_console/api/takeover/status/ ops_console/api/takeover/permission supervisor_id reason_code handback_handler_id analyst_id resolution ops_console\\/api\\/conversations analytics\\/realtime",
                route: "transformRustDataPlaneRequest rewriteRustDataPlaneSearchParams",
                gateway_routes: "/api/takeover/history /api/takeover/status/:session_id /api/takeover/permission /api/conversations/events /api/conversations/:session_id/events",
                gateway_chat: "Json(form): Json<SendTemplateForm>",
                gateway_conversations: "TenantPrincipal principal: TenantPrincipal principal: TenantPrincipal principal: TenantPrincipal principal: TenantPrincipal principal: TenantPrincipal principal: TenantPrincipal principal: TenantPrincipal principal_can_read_conversation enforced_conversation_tenant conversations_api messages_api latest_messages_api session_api session_context_api",
                monitoring_client: "ReleaseTakeoverParams analystId: string analyst_id: params.analystId",
                ingress: "const RUST_WEBHOOK_TARGET = `${GATEWAY_URL}/ingest`;",
                firewall: "'/api/communications/outbound/intent' WHATSAPP_DIRECT_FALLBACK",
                whatsapp_tool: "handoff_to_system2(intent)",
            }),
            (true, true, true, false),
        );
    }

    #[test]
    fn rejects_route_markers_without_real_rust_request_contracts() {
        assert_eq!(
            runtime_boundary_markers(&BoundarySources {
                resolver: "RUST_DATA_PLANE_RULES ops_console/api/takeover/history ops_console\\/api\\/conversations analytics\\/realtime",
                route: "legacy python body transform only",
                gateway_routes: "/api/takeover/history",
                gateway_chat: "Form(form): Form<SendTemplateForm>",
                gateway_conversations: "unscoped conversation stream",
                monitoring_client: "ReleaseTakeoverParams without analyst identity",
                ingress: "const RUST_WEBHOOK_TARGET = `${GATEWAY_URL}/ingest`;",
                firewall: r#"gateway_url = '/api/communications/outbound/intent'; "permissions": ["egress:submit"]"#,
                whatsapp_tool: "handoff_to_system2(intent)",
            }),
            (false, false, true, true),
        );
    }

    #[test]
    fn rejects_public_runtime_streams_and_raw_meta_message_routes() {
        assert!(gateway_exposes_only_gated_runtime_surfaces(
            &GatewaySources {
                main: r#".route("/api/communications/outbound/intent", post(handle_outbound_intent))
                .route("/api/runtime/events", get(ops_console::routes::sse_handler))
                .nest("/ops_console", ops_routes(state.clone()))"#,
                whatsapp_messages: "pub async fn build_message_payload() {}",
                whatsapp_channel: "WHATSAPP_CHANNEL_TENANT",
                egress_handler: "principal: TenantPrincipal principal_can_submit_egress_for_tenant principal_can_submit_egress principal_can_review_egress phone_route_belongs_to_tenant principal_is_internal_campaign_worker submit_internal_outbound_intent generate_access_token .bearer_auth(service_token) internal_http_base_url /api/communications/outbound/intent",
                jwt_middleware: "source headers never bypass authentication",
                chatwoot_ingest: "submit_internal_outbound_intent",
                sse_routes: "TenantPrincipal RuntimeEventScope principal_tenant query_conversation_tenant let Some(tenant)",
                conversation_routes: "query_conversation_tenant",
                conversation_data_access: "pub async fn query_conversation_tenant missing session or database error must fail closed",
                resolver: "RUST_DATA_PLANE_RULES",
                whatsapp_client: "class WhatsAppClient {}",
            }
        ));
        assert!(!gateway_exposes_only_gated_runtime_surfaces(
            &GatewaySources {
                main: r#".route("/api/runtime/events", get(runtime_events_sse_handler))
                .route("/api/whatsapp/phone_numbers/:phone_id/messages/text", post(send_text_handler))"#,
                whatsapp_messages: "let tenant_id = \"__system__\";",
                whatsapp_channel: "\"__system__\"",
                egress_handler: "handle_outbound_intent(body)",
                jwt_middleware: "GATEWAY_INTERNAL_TRUST_LOOPBACK is_internal_trusted_loopback",
                chatwoot_ingest: ".header(\"x-tenant-id\", tenant_id)",
                sse_routes: "bus.subscribe()",
                conversation_routes: "query_conversation_detail",
                conversation_data_access: "default tenant fallback",
                resolver: "messages\\/text",
                whatsapp_client: "class MessagingApi {}",
            }
        ));
    }

    #[test]
    fn rejects_autopilot_upstream_in_local_bff() {
        let root = std::env::temp_dir().join(format!(
            "leio-frontend-congruence-autopilot-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        write(&root.join("example-ops/package.json"), "{}");
        write(
            &root.join("packages/trpc/src/client/example.ts"),
            "export class ExampleClientBase {}",
        );
        write(&root.join("packages/trpc/package.json"), "{}");
        write(
            &root.join("example-ops/src/app/api/trpc/[trpc]/route.ts"),
            "fetchRequestHandler createEngineRuntimeRouter createEngineCollectionsRouter createRecoveryPlaybooksRouter AUTOPILOT_API_URL",
        );
        let result = doctor_frontend_engine_client(&empty_index(&root), &root);
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("local engine BFF"))
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_missing_shared_engine_view() {
        let root = std::env::temp_dir().join(format!(
            "leio-frontend-congruence-engine-view-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        write(&root.join("example-ops/package.json"), "{}");
        write(
            &root.join("packages/trpc/src/client/example.ts"),
            "export class ExampleClientBase {}",
        );
        write(&root.join("packages/trpc/package.json"), "{}");
        write(
            &root.join("example-ops/src/app/api/trpc/[trpc]/route.ts"),
            "fetchRequestHandler createEngineRuntimeRouter createEngineCollectionsRouter createRecoveryPlaybooksRouter",
        );
        write(
            &root.join("packages/trpc/src/views/engine/SearchPage.tsx"),
            "export function EngineSearchPage() {}",
        );
        write(
            &root.join("example-ops/src/components/engine/views/search/page.tsx"),
            "export { EngineSearchPage as default } from '@jai/trpc/views/engine/SearchPage';",
        );
        let result = doctor_frontend_engine_client(&empty_index(&root), &root);
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("shared engine view"))
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_health_audit_rules_transport_bypass() {
        assert!(!health_audit_rules_uses_shared_transport(
            "useRegistryRules from '@/hooks/useRegistryRules'",
            "fetch('/v2/health-audit/registry/rules')",
            "from '../../lib/healthAuditApi' healthAuditJson"
        ));
    }

    #[test]
    fn accepts_removed_health_audit_orphan_clients() {
        let root = std::env::temp_dir().join(format!(
            "leio-frontend-congruence-health-audit-orphans-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("health-audit-console")).expect("create fixture");

        let result = doctor_frontend_engine_client(&empty_index(&root), &root);
        assert!(
            result
                .warnings
                .iter()
                .all(|warning| !warning.contains("app/tenants/client.ts")
                    && !warning.contains("app/glosa/client.ts")),
            "{:?}",
            result.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_vigoros_direct_auth_transport() {
        assert!(!vigoros_auth_uses_shared_transport(
            "fetch('/v2/auth/me'); localStorage.setItem('access_token', token);"
        ));
        assert!(vigoros_auth_uses_shared_transport(
            "createBrowserCookieSessionClient from '@jai/trpc/client/browserSession' fetchExampleApiResponse resolveExampleApiBaseUrl"
        ));
    }

    #[test]
    fn rejects_retired_ops_console_script_path_but_allows_compatibility_tokens() {
        let root = std::env::temp_dir().join(format!(
            "leio-frontend-congruence-script-path-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        write(
            &root.join("scripts/compose_smoke.sh"),
            "OPS_CONSOLE_ALLOW_WHATSAPP_SEND=1 /ops_console/health cp -r $ROOT/ops-console $ROOT/example-gateway/ops-console",
        );
        let mut entities = Vec::new();
        let mut evidence = Vec::new();
        let mut warnings = Vec::new();
        check_critical_script_paths(&root, &mut entities, &mut evidence, &mut warnings);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("ops-console"))
        );
        assert!(warnings.iter().all(|warning| {
            !warning.contains("OPS_CONSOLE_ALLOW_WHATSAPP_SEND")
                && !warning.contains("/ops_console")
        }));
        let _ = fs::remove_dir_all(root);
    }
}
