use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct RouteProjectionDoctor;

impl Doctor for RouteProjectionDoctor {
    fn name(&self) -> &'static str {
        "route-projection"
    }

    fn description(&self) -> &'static str {
        "Checks canonical route projections plus the mounted Python API surface invariants."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_route_projection(index, root)
    }
}

pub fn doctor_route_projection(_index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let hotpath_state_path = root.join("example-api/example/hotpath_state.py");
    let webhook_path = root.join("example-api/example/integrations/whatsapp/routers/webhook.py");
    let handoff_path = root.join("example-api/example/agents/tools/handoff.py");
    let ops_path = root.join("example-api/example/routers/ops.py");
    let ops_console_path = root.join("example-api/example/routers/ops_console.py");
    let internal_path = root.join("example-api/example/routers/v2/internal.py");
    let legacy_routing_path =
        root.join("example-api/example/integrations/whatsapp/whatsapp_routing.py");
    let router_registry_path = root.join("example-api/example/routers/__init__.py");
    let main_path = root.join("example-api/example/main.py");
    let navigate_path = root.join("example-api/example/routers/navigate.py");
    let workflow_sessions_path = root.join("example-api/example/routers/workflow_sessions.py");
    let retrieve_stream_path = root.join("example-api/example/routers/retrieve_stream.py");
    let conversations_leio_path = root.join("example-api/example/routers/conversations_leio.py");
    let registry_tests_path =
        root.join("example-api/example/tests/api/test_router_registry_consistency.py");
    let liz_seed_path = root.join("cartridges/liz_cobranca/seed.py");
    let pratique_seed_path = root.join("cartridges/pratique_cobranca/seed.py");
    let sara_seed_path = root.join("example-api/scripts/seed_sara_assurant.py");
    let sara_cartridge_seed_path = root.join("cartridges/insurance_agent/seed.py");
    let gateway_routing_path = root.join("example-gateway/src/routing.rs");

    let hotpath_state_src = read_text(&hotpath_state_path, &mut warnings);
    let webhook_src = read_text(&webhook_path, &mut warnings);
    let handoff_src = read_text(&handoff_path, &mut warnings);
    let ops_src = read_text(&ops_path, &mut warnings);
    let ops_console_src = read_text(&ops_console_path, &mut warnings);
    let internal_src = read_text(&internal_path, &mut warnings);
    let legacy_routing_src = read_text(&legacy_routing_path, &mut warnings);
    let router_registry_src = read_text(&router_registry_path, &mut warnings);
    let main_src = read_text(&main_path, &mut warnings);
    let navigate_src = read_text(&navigate_path, &mut warnings);
    let workflow_sessions_src = read_text(&workflow_sessions_path, &mut warnings);
    let retrieve_stream_src = read_text(&retrieve_stream_path, &mut warnings);
    let conversations_leio_src = read_text(&conversations_leio_path, &mut warnings);
    let registry_tests_src = read_text(&registry_tests_path, &mut warnings);
    let liz_seed_src = read_text(&liz_seed_path, &mut warnings);
    let pratique_seed_src = read_text(&pratique_seed_path, &mut warnings);
    let sara_seed_src = read_text(&sara_seed_path, &mut warnings);
    let sara_cartridge_seed_src = read_text(&sara_cartridge_seed_path, &mut warnings);
    let gateway_routing_src = read_text(&gateway_routing_path, &mut warnings);

    let hotpath_uses_prefix_helpers = hotpath_state_src.as_deref().is_some_and(|src| {
        src.contains("def _route_prefix() -> str:")
            && src.contains("def _route_key(phone_line_id: str) -> str:")
            && src.contains("return f\"{_route_prefix()}:{phone_line_id}\"")
    });
    let webhook_reads_canonical_projection = webhook_src.as_deref().is_some_and(|src| {
        src.contains("prefix = os.environ.get(\"WHATSAPP_ROUTE_PREFIX\", \"phone_route\")")
            && src.contains("global_raw = _redis_client().hgetall(f\"{prefix}:{phone_id}\")")
    });
    let webhook_honors_agent_recipient_pause = webhook_src.as_deref().is_some_and(|src| {
        src.contains("from example.hotpath_state import")
            && source_contains_all(
                src,
                &[
                    "agent_recipient_pause_key",
                    "get_agent_recipient_pause",
                    "get_agent_recipient_pause(",
                    "[WHATSAPP] Agent-recipient pair paused",
                ],
            )
    });
    let handoff_writes_agent_recipient_pause = handoff_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "from example.hotpath_state import set_agent_recipient_pause",
                "set_agent_recipient_pause(",
                "\"recipient_pause_applied\"",
                "\"route_override_applied\": False",
            ],
        )
    });
    let ops_reads_canonical_routes = ops_src.as_deref().is_some_and(|src| {
        src.contains("def _resolve_global_route(phone_id: str) -> dict[str, str] | None:")
            && src.contains("route_key = _user_route_key(business_phone_id, user_phone)")
    });
    let ops_console_uses_shared_route_projection_helpers =
        ops_console_src.as_deref().is_some_and(|src| {
            // Accept both single-line and multi-line `from ... import (...)` forms.
            let imports_helpers = src.contains("from example.hotpath_state import")
                && src.contains("delete_phone_route_projection")
                && src.contains("upsert_phone_route_projection");
            imports_helpers
                && src.contains("upsert_phone_route_projection(")
                && src.contains("projection_source=\"ops_console_route_api\"")
                && src.contains("delete_phone_route_projection(pattern, client=redis_client)")
                && src.contains("delete_phone_route_projection(phone_id, client=redis_client)")
        });
    let internal_uses_canonical_reset_helpers = internal_src.as_deref().is_some_and(|src| {
        // internal.py should import and use canonical helpers from hotpath_state
        // rather than defining private copies
        src.contains("from example.hotpath_state import")
            && src.contains("scan_user_route_projections")
            && src.contains("extract_phone_line_from_user_route_key")
            && src.contains("delete_user_route_projection")
    });
    let internal_still_uses_phone_digits_route_pattern = internal_src
        .as_deref()
        .is_some_and(|src| src.contains("phone_user_route:{phone_digits}"));
    let legacy_routing_guards_writes = legacy_routing_src.as_deref().is_some_and(|src| {
        src.contains("def _legacy_route_writes_enabled(self) -> bool:")
            && src.contains("WHATSAPP_ROUTING_ENABLE_LEGACY_WRITES")
            && src.contains("return self._reject_legacy_route_write(\"set_global_route\")")
            && src.contains("return self._reject_legacy_route_write(\"set_user_route\")")
            && src.contains("return self._reject_legacy_route_write(\"clear_user_route\")")
    });
    let registry_mounts_canonical_route_surfaces =
        router_registry_src.as_deref().is_some_and(|src| {
            registry_mounts_router(src, "flow_router", ".flow")
                && registry_mounts_router(src, "retrieve_stream_router", ".retrieve_stream")
                && registry_mounts_router(src, "workflow_sessions_router", ".workflow_sessions")
                && registry_mounts_router(src, "conversations_leio_router", ".conversations_leio")
        });
    let main_uses_registry_driven_mounts = main_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "routers as router_registry",
                "for router_name, prefix in router_registry.iter_router_entries(profile):",
                "router = router_registry.load_router(router_name)",
                "_inject_core_auth(app, router_name, start_index)",
            ],
        )
    });
    let navigate_is_explicitly_legacy = navigate_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "Legacy generic navigate router",
                "It is intentionally not mounted in the main API router registry.",
                "workflow_sessions.py",
                "/v2/workflows/{workflow_id}/sessions",
            ],
        )
    });
    let workflow_sessions_is_canonical_surface =
        workflow_sessions_src.as_deref().is_some_and(|src| {
            source_contains_all(
                src,
                &[
                    "Workflow sessions router",
                    "router = APIRouter(prefix=\"/v2/workflows\", tags=[\"workflow-sessions\"])",
                    "\"/{workflow_id}/sessions\"",
                    "\"/{workflow_id}/sessions/{session_id}/step\"",
                    "\"/{workflow_id}/sessions/{session_id}\"",
                ],
            )
        });
    let retrieve_stream_is_mounted_surface = retrieve_stream_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "router = APIRouter(prefix=\"/v2/retrieve\", tags=[\"retrieve\"])",
                "\"/{task_id}/stream\"",
                "Stream MCTS progress via SSE",
            ],
        )
    });
    let conversations_leio_declares_canonical_and_compat_aliases =
        conversations_leio_src.as_deref().is_some_and(|src| {
            source_contains_all(
                src,
                &[
                    "Canonical endpoints:",
                    "Legacy aliases kept for compatibility",
                    "canonical_router = APIRouter(prefix=\"/v2/leio/conversations\", tags=[\"leio-conversations\"])",
                    "legacy_router = APIRouter(prefix=\"/v2/conversations\", tags=[\"leio-conversations\"])",
                    "include_in_schema=False",
                ],
            )
        });
    let registry_has_regression_tests = registry_tests_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "test_build_main_router_matches_core_router_includes",
                "test_include_core_routers_does_not_mutate_source_router_dependencies",
                "build_main_router",
            ],
        )
    });
    let hotpath_preserves_human_confirmed_routes =
        hotpath_state_src.as_deref().is_some_and(|src| {
            source_contains_all(
                src,
                &[
                    "_HUMAN_CONFIRMED_PROJECTION_SOURCES",
                    "preserve_human_confirmed",
                    "projection_source not in _HUMAN_CONFIRMED_PROJECTION_SOURCES",
                    "[HOTPATH] Preserving human-confirmed phone route projection",
                ],
            )
        });
    let hotpath_separates_meta_tenant_whatsapp = hotpath_state_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "channel_provider = str(phone_line.channel_provider or \"meta\").strip().lower()",
                "if channel_provider != \"meta\":",
                "return",
                "\"channel_provider\": phone_line.channel_provider or \"meta\"",
                "channel_provider == \"meta\"",
                "if channel_provider == \"meta\" and str(phone_line.provider_account_id or \"\").strip():",
                "has_tenant_whatsapp",
            ],
        )
    });
    let liz_seed_matches_human_confirmed_meta = liz_seed_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "WABA_ID = \"1457602182254517\"",
                "\"id\": \"755985340928184\"",
                "\"display_phone\": \"+55 88 2018-1312\"",
                "\"verified_name\": \"Academia Fitness Exclusive Financeiro\"",
                "LIZ_ASSIGNED_PHONES = [\"755985340928184\"]",
                "projection_source=\"human_confirmed\"",
                "preserve_human_confirmed=False",
            ],
        )
    });
    let sara_seed_matches_infobip_broker =
        sara_seed_src.as_deref().is_some_and(|src| {
            source_contains_all(
                src,
                &[
                    "SARA_INFOBIP_PHONE_LINE_ID",
                    "5511987771687",
                    "SARA_INFOBIP_PHONE_DISPLAY",
                    "+55 11 98777-1687",
                    "SARA_INFOBIP_VERIFIED_NAME",
                    "Assurant Plusoft Infobip Broker",
                    "channel_provider=\"infobip\"",
                    "\"broker\": \"infobip\"",
                    "\"integration\": \"plusoft\"",
                    "\"route_owner\": \"sara_assurant\"",
                    "\"ingest_path\": \"/v2/plusoft/ingest\"",
                    "_ensure_sara_assignment(SARA_INFOBIP_PHONE_LINE_ID)",
                    "validate_seeded_runtime_state()",
                    "projection_source=\"human_confirmed\"",
                    "preserve_human_confirmed=False",
                ],
            )
        }) && sara_cartridge_seed_src.as_deref().is_some_and(|src| {
            source_contains_all(
                src,
                &[
                    "SARA_INFOBIP_PHONE_LINE_ID",
                    "5511987771687",
                    "SARA_INFOBIP_PHONE_DISPLAY",
                    "+55 11 98777-1687",
                    "SARA_INFOBIP_VERIFIED_NAME",
                    "Assurant Plusoft Infobip Broker",
                    "channel_provider=\"infobip\"",
                    "\"broker\": \"infobip\"",
                    "\"integration\": \"plusoft\"",
                    "\"route_owner\": \"sara_assurant\"",
                    "\"ingest_path\": \"/v2/plusoft/ingest\"",
                    "_ensure_sara_assignment(SARA_INFOBIP_PHONE_LINE_ID)",
                    "projection_source=\"human_confirmed\"",
                    "preserve_human_confirmed=False",
                ],
            )
        });
    let pratique_seed_matches_human_confirmed_meta =
        pratique_seed_src.as_deref().is_some_and(|src| {
            source_contains_all(
                src,
                &[
                    "WABA_ID = os.environ.get(\"PRATIQUE_WABA_ID\", \"494529723740389\")",
                    "\"id\": \"451129211414233\"",
                    "\"display_phone\": \"+55 31 9292-7320\"",
                    "\"verified_name\": \"Academia Pratique Fitness\"",
                    "PRATIQUE_ASSIGNED_PHONE = \"451129211414233\"",
                    "projection_source=\"human_confirmed\"",
                    "preserve_human_confirmed=False",
                ],
            )
        });
    let sara_seed_matches_human_confirmed_meta = sara_seed_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "SARA_PHONE_LINE_ID = \"108079528970614\"",
                "SARA_PHONE_DISPLAY = \"+55 11 5194-0431\"",
                "SARA_PHONE_VERIFIED_NAME = \"JAI Fit Demonstração de Atendimento\"",
                "SARA_WABA_ID = \"113089725128375\"",
                "projection_source=\"human_confirmed\"",
                "preserve_human_confirmed=False",
            ],
        )
    });
    let gateway_routes_list_reads_redis_projection =
        gateway_routing_src.as_deref().is_some_and(|src| {
            source_contains_all(
                src,
                &[
                    "pub async fn list_routes_handler",
                    "list_routes_from_redis(redis).await",
                    "fn route_info_from_redis_hash",
                    "async fn scan_redis_route_keys",
                    "\"source\": \"redis\"",
                ],
            )
        });
    let gateway_resolver_honors_redis_suppression =
        gateway_routing_src.as_deref().is_some_and(|src| {
            source_contains_all(
                src,
                &[
                    "enum RedisRouteLookup",
                    "RedisRouteLookup::Suppressed",
                    "mode == \"paused\"",
                    "Ok(RedisRouteLookup::Suppressed) => return None",
                    "Ok(RedisRouteLookup::Miss) => return None",
                ],
            )
        });
    let gateway_route_mutations_project_redis = gateway_routing_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "async fn upsert_route_in_redis",
                "async fn delete_route_from_redis",
                "\"projection_source\", \"gateway_route_api\"",
                "\"redis_projected\": redis_projected",
            ],
        )
    });
    let gateway_route_tests_cover_redis_projection =
        gateway_routing_src.as_deref().is_some_and(|src| {
            source_contains_all(
                src,
                &[
                    "redis_route_lookup_suppresses_paused_projection",
                    "route_info_from_redis_hash_preserves_paused_lines",
                    "parse_phone_pattern_from_route_identifier_accepts_redis_keys",
                ],
            )
        });

    if !hotpath_uses_prefix_helpers {
        warnings.push(
            "hotpath_state.py does not expose canonical route projection helpers".to_string(),
        );
    }
    if !webhook_reads_canonical_projection {
        warnings.push(
            "webhook.py is not reading phone_route through the canonical projection flow"
                .to_string(),
        );
    }
    if !webhook_honors_agent_recipient_pause {
        warnings.push("webhook.py is not honoring explicit agent-recipient pause keys".to_string());
    }
    if !handoff_writes_agent_recipient_pause {
        warnings.push(
            "handoff.py is not using explicit agent-recipient pause instead of user route overrides"
                .to_string(),
        );
    }
    if !ops_reads_canonical_routes {
        warnings.push(
            "ops.py is not resolving human overrides through canonical route helpers".to_string(),
        );
    }
    if !ops_console_uses_shared_route_projection_helpers {
        warnings.push(
            "ops_console.py is not routing phone_route writes/deletes through shared hotpath_state projection helpers"
                .to_string(),
        );
    }
    if !internal_uses_canonical_reset_helpers {
        warnings
            .push("internal.py reset path does not use canonical route reset helpers".to_string());
    }
    if internal_still_uses_phone_digits_route_pattern {
        warnings.push("internal.py still deletes phone_user_route via phone_digits instead of canonical business_phone_id:user_hash".to_string());
    }
    if !legacy_routing_guards_writes {
        warnings.push(
            "legacy whatsapp_routing.py route writers are not guarded behind WHATSAPP_ROUTING_ENABLE_LEGACY_WRITES"
                .to_string(),
        );
    }
    if !registry_mounts_canonical_route_surfaces {
        warnings.push(
            "routers/__init__.py is missing one of the canonical mounted route surfaces (flow, retrieve_stream, workflow_sessions, conversations_leio)"
                .to_string(),
        );
    }
    if !main_uses_registry_driven_mounts {
        warnings.push(
            "main.py is not mounting core routers through the shared router registry iteration flow"
                .to_string(),
        );
    }
    if !navigate_is_explicitly_legacy {
        warnings.push(
            "navigate.py no longer documents itself as the unmounted legacy surface superseded by workflow_sessions.py"
                .to_string(),
        );
    }
    if !workflow_sessions_is_canonical_surface {
        warnings.push(
            "workflow_sessions.py does not clearly expose the canonical workflow-bound navigation session surface"
                .to_string(),
        );
    }
    if !retrieve_stream_is_mounted_surface {
        warnings.push(
            "retrieve_stream.py is missing the canonical /v2/retrieve/{task_id}/stream SSE surface"
                .to_string(),
        );
    }
    if !conversations_leio_declares_canonical_and_compat_aliases {
        warnings.push(
            "conversations_leio.py does not clearly distinguish canonical LEIO routes from schema-hidden compatibility aliases"
                .to_string(),
        );
    }
    if !registry_has_regression_tests {
        warnings.push(
            "router registry consistency coverage is missing from test_router_registry_consistency.py"
                .to_string(),
        );
    }
    if !hotpath_preserves_human_confirmed_routes {
        warnings.push(
            "hotpath_state.py does not guard human-confirmed Redis phone routes from DuckDB projection overwrite"
                .to_string(),
        );
    }
    if !hotpath_separates_meta_tenant_whatsapp {
        warnings.push(
            "hotpath_state.py does not keep non-Meta broker routes out of tenant_whatsapp credential projections"
                .to_string(),
        );
    }
    if !liz_seed_matches_human_confirmed_meta {
        warnings.push(
            "liz_cobranca seed does not recreate the human-confirmed Fitness Exclusive routing contract"
                .to_string(),
        );
    }
    if !pratique_seed_matches_human_confirmed_meta {
        warnings.push(
            "pratique_cobranca seed does not recreate the human-confirmed Pratique Cobrança routing contract"
                .to_string(),
        );
    }
    if !sara_seed_matches_human_confirmed_meta {
        warnings.push(
            "seed_sara_assurant.py does not recreate the human-confirmed Sara routing contract"
                .to_string(),
        );
    }
    if !sara_seed_matches_infobip_broker {
        warnings.push(
            "Sara seed does not recreate the human-confirmed Plusoft/Infobip broker routing contract"
                .to_string(),
        );
    }
    if !gateway_routes_list_reads_redis_projection {
        warnings.push(
            "example-gateway /api/routes does not list Redis phone_route projections before DuckDB fallback"
                .to_string(),
        );
    }
    if !gateway_resolver_honors_redis_suppression {
        warnings.push(
            "example-gateway Redis route resolver does not treat paused/invalid Redis projections as authoritative suppressions"
                .to_string(),
        );
    }
    if !gateway_route_mutations_project_redis {
        warnings.push(
            "example-gateway route create/delete APIs do not write the Redis runtime projection"
                .to_string(),
        );
    }
    if !gateway_route_tests_cover_redis_projection {
        warnings.push(
            "example-gateway routing tests do not cover Redis projection listing and paused-route suppression"
                .to_string(),
        );
    }

    for (path, src, needle, detail) in [
        (
            &hotpath_state_path,
            hotpath_state_src.as_ref(),
            "def _route_key(phone_line_id: str) -> str:",
            "canonical global route key helper",
        ),
        (
            &webhook_path,
            webhook_src.as_ref(),
            "global_raw = _redis_client().hgetall(f\"{prefix}:{phone_id}\")",
            "webhook reads global phone route projection",
        ),
        (
            &webhook_path,
            webhook_src.as_ref(),
            "get_agent_recipient_pause,",
            "webhook checks explicit agent-recipient pause keys before dispatch",
        ),
        (
            &handoff_path,
            handoff_src.as_ref(),
            "def _user_route_key(business_phone_id: str, user_phone: str) -> str:",
            "handoff writes canonical user route override keys",
        ),
        (
            &ops_path,
            ops_src.as_ref(),
            "route_key = _user_route_key(business_phone_id, user_phone)",
            "ops resolves human override via canonical user route keys",
        ),
        (
            &ops_console_path,
            ops_console_src.as_ref(),
            "upsert_phone_route_projection(",
            "ops console route API delegates global route writes to the shared hotpath_state helper",
        ),
        (
            &internal_path,
            internal_src.as_ref(),
            "route_pattern = _user_route_pattern(user_hash, payload.phone_line_id)",
            "internal reset clears canonical user route override keys",
        ),
        (
            &legacy_routing_path,
            legacy_routing_src.as_ref(),
            "def _legacy_route_writes_enabled(self) -> bool:",
            "legacy route writers are guarded behind an explicit migration flag",
        ),
        (
            &router_registry_path,
            router_registry_src.as_ref(),
            "\"retrieve_stream_router\": (\".retrieve_stream\", \"router\")",
            "router registry includes the mounted retrieve_stream surface",
        ),
        (
            &router_registry_path,
            router_registry_src.as_ref(),
            "\"workflow_sessions_router\": (\".workflow_sessions\", \"router\")",
            "router registry includes the canonical workflow_sessions surface",
        ),
        (
            &main_path,
            main_src.as_ref(),
            "for router_name, prefix in router_registry.iter_router_entries(profile):",
            "main mounts core routers through the shared router registry",
        ),
        (
            &navigate_path,
            navigate_src.as_ref(),
            "It is intentionally not mounted in the main API router registry.",
            "navigate.py is explicitly documented as a legacy unmounted surface",
        ),
        (
            &workflow_sessions_path,
            workflow_sessions_src.as_ref(),
            "router = APIRouter(prefix=\"/v2/workflows\", tags=[\"workflow-sessions\"])",
            "workflow_sessions.py is the canonical mounted workflow session surface",
        ),
        (
            &retrieve_stream_path,
            retrieve_stream_src.as_ref(),
            "\"/{task_id}/stream\"",
            "retrieve_stream.py exposes the mounted MCTS SSE route",
        ),
        (
            &conversations_leio_path,
            conversations_leio_src.as_ref(),
            "Legacy aliases kept for compatibility",
            "conversations_leio.py documents canonical routes and compatibility aliases separately",
        ),
        (
            &registry_tests_path,
            registry_tests_src.as_ref(),
            "test_build_main_router_matches_core_router_includes",
            "router registry consistency has dedicated regression coverage",
        ),
        (
            &hotpath_state_path,
            hotpath_state_src.as_ref(),
            "_HUMAN_CONFIRMED_PROJECTION_SOURCES",
            "Redis human-confirmed phone routes are protected from generated DuckDB projection drift",
        ),
        (
            &hotpath_state_path,
            hotpath_state_src.as_ref(),
            "if channel_provider == \"meta\" and str(phone_line.provider_account_id or \"\").strip():",
            "only Meta lines are allowed to keep tenant_whatsapp credential projections canonical",
        ),
        (
            &liz_seed_path,
            liz_seed_src.as_ref(),
            "LIZ_ASSIGNED_PHONES = [\"755985340928184\"]",
            "Fitness Exclusive seed assigns only the human-confirmed financeiro phone to Liz",
        ),
        (
            &pratique_seed_path,
            pratique_seed_src.as_ref(),
            "\"display_phone\": \"+55 31 9292-7320\"",
            "Pratique Cobrança seed uses the human-confirmed Meta phone display",
        ),
        (
            &sara_seed_path,
            sara_seed_src.as_ref(),
            "SARA_PHONE_LINE_ID = \"108079528970614\"",
            "Sara seed uses the Meta-grounded JAI Assistant phone line",
        ),
        (
            &sara_seed_path,
            sara_seed_src.as_ref(),
            "SARA_INFOBIP_PHONE_LINE_ID = os.getenv(\"SARA_INFOBIP_PHONE_LINE_ID\", \"5511987771687\").strip()",
            "Sara deploy seed records the Plusoft/Infobip broker phone line as a non-Meta route",
        ),
        (
            &sara_cartridge_seed_path,
            sara_cartridge_seed_src.as_ref(),
            "SARA_INFOBIP_PHONE_LINE_ID = os.getenv(\"SARA_INFOBIP_PHONE_LINE_ID\", \"5511987771687\").strip()",
            "Sara cartridge startup seed records the Plusoft/Infobip broker phone line as a non-Meta route",
        ),
        (
            &gateway_routing_path,
            gateway_routing_src.as_ref(),
            "list_routes_from_redis(redis).await",
            "gateway /api/routes reads Redis phone_route projections before DuckDB fallback",
        ),
        (
            &gateway_routing_path,
            gateway_routing_src.as_ref(),
            "Ok(RedisRouteLookup::Suppressed) => return None",
            "gateway resolver treats paused/invalid Redis projections as authoritative suppressions",
        ),
        (
            &gateway_routing_path,
            gateway_routing_src.as_ref(),
            "Ok(RedisRouteLookup::Miss) => return None",
            "gateway resolver treats successful Redis misses as authoritative no-route results",
        ),
        (
            &gateway_routing_path,
            gateway_routing_src.as_ref(),
            "\"projection_source\", \"gateway_route_api\"",
            "gateway route API writes Redis runtime projections with explicit source",
        ),
        (
            &gateway_routing_path,
            gateway_routing_src.as_ref(),
            "route_info_from_redis_hash_preserves_paused_lines",
            "gateway routing tests cover paused Redis projections in /api/routes",
        ),
    ] {
        if let Some(src) = src
            && let Some(line) = find_line(src, needle)
        {
            evidence.push(EvidenceItem {
                kind: "route_projection".to_string(),
                path: path.display().to_string(),
                line: Some(line),
                detail: detail.to_string(),
            });
        }
    }

    entities.push(json!({
        "path": hotpath_state_path.display().to_string(),
        "uses_prefix_helpers": hotpath_uses_prefix_helpers,
    }));
    entities.push(json!({
        "path": webhook_path.display().to_string(),
        "reads_canonical_projection": webhook_reads_canonical_projection,
        "honors_agent_recipient_pause": webhook_honors_agent_recipient_pause,
    }));
    entities.push(json!({
        "path": handoff_path.display().to_string(),
        "writes_agent_recipient_pause": handoff_writes_agent_recipient_pause,
    }));
    entities.push(json!({
        "path": ops_path.display().to_string(),
        "reads_canonical_routes": ops_reads_canonical_routes,
    }));
    entities.push(json!({
        "path": ops_console_path.display().to_string(),
        "uses_shared_route_projection_helpers": ops_console_uses_shared_route_projection_helpers,
    }));
    entities.push(json!({
        "path": internal_path.display().to_string(),
        "uses_canonical_reset_helpers": internal_uses_canonical_reset_helpers,
        "uses_phone_digits_route_pattern": internal_still_uses_phone_digits_route_pattern,
    }));
    entities.push(json!({
        "path": legacy_routing_path.display().to_string(),
        "guards_legacy_writes": legacy_routing_guards_writes,
    }));
    entities.push(json!({
        "path": router_registry_path.display().to_string(),
        "mounts_canonical_route_surfaces": registry_mounts_canonical_route_surfaces,
    }));
    entities.push(json!({
        "path": main_path.display().to_string(),
        "uses_registry_driven_mounts": main_uses_registry_driven_mounts,
    }));
    entities.push(json!({
        "path": navigate_path.display().to_string(),
        "is_explicitly_legacy": navigate_is_explicitly_legacy,
    }));
    entities.push(json!({
        "path": workflow_sessions_path.display().to_string(),
        "is_canonical_surface": workflow_sessions_is_canonical_surface,
    }));
    entities.push(json!({
        "path": retrieve_stream_path.display().to_string(),
        "is_mounted_surface": retrieve_stream_is_mounted_surface,
    }));
    entities.push(json!({
        "path": conversations_leio_path.display().to_string(),
        "declares_canonical_and_compat_aliases": conversations_leio_declares_canonical_and_compat_aliases,
    }));
    entities.push(json!({
        "path": registry_tests_path.display().to_string(),
        "has_registry_regression_tests": registry_has_regression_tests,
    }));
    entities.push(json!({
        "path": hotpath_state_path.display().to_string(),
        "preserves_human_confirmed_routes": hotpath_preserves_human_confirmed_routes,
        "separates_meta_tenant_whatsapp": hotpath_separates_meta_tenant_whatsapp,
    }));
    entities.push(json!({
        "path": liz_seed_path.display().to_string(),
        "matches_human_confirmed_meta": liz_seed_matches_human_confirmed_meta,
    }));
    entities.push(json!({
        "path": pratique_seed_path.display().to_string(),
        "matches_human_confirmed_meta": pratique_seed_matches_human_confirmed_meta,
    }));
    entities.push(json!({
        "path": sara_seed_path.display().to_string(),
        "matches_human_confirmed_meta": sara_seed_matches_human_confirmed_meta,
        "matches_infobip_broker": sara_seed_matches_infobip_broker,
    }));
    entities.push(json!({
        "path": sara_cartridge_seed_path.display().to_string(),
        "matches_infobip_broker": sara_seed_matches_infobip_broker,
    }));
    entities.push(json!({
        "path": gateway_routing_path.display().to_string(),
        "routes_list_reads_redis_projection": gateway_routes_list_reads_redis_projection,
        "resolver_honors_redis_suppression": gateway_resolver_honors_redis_suppression,
        "route_mutations_project_redis": gateway_route_mutations_project_redis,
        "route_tests_cover_redis_projection": gateway_route_tests_cover_redis_projection,
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_route_projection"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked route projection wiring and mounted API surface invariants, found {} warnings",
            warnings.len()
        ),
        confidence: if warnings.is_empty() { 0.95 } else { 0.67 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn source_contains_all(src: &str, needles: &[&str]) -> bool {
    needles.iter().all(|needle| src.contains(needle))
}

fn registry_mounts_router(src: &str, router_name: &str, module_spec: &str) -> bool {
    src.contains(&format!(
        "\"{router_name}\": (\"{module_spec}\", \"router\")"
    )) && src.contains(&format!("(\"{router_name}\", None)"))
}

#[cfg(test)]
mod tests {
    use super::{registry_mounts_router, source_contains_all};

    #[test]
    fn registry_mounts_router_requires_spec_and_full_entry() {
        let src = r#"
_ROUTER_SPECS = {
    "workflow_sessions_router": (".workflow_sessions", "router"),
}

_FULL_ROUTER_ENTRIES = (
    ("workflow_sessions_router", None),
)
"#;

        assert!(registry_mounts_router(
            src,
            "workflow_sessions_router",
            ".workflow_sessions"
        ));
        assert!(!registry_mounts_router(
            src,
            "retrieve_stream_router",
            ".retrieve_stream"
        ));
    }

    #[test]
    fn source_contains_all_requires_every_invariant() {
        let src = "canonical legacy include_in_schema=False";
        assert!(source_contains_all(src, &["canonical", "legacy"]));
        assert!(!source_contains_all(src, &["canonical", "missing"]));
    }

    /// The registry-mount check must accept both the bare import form and the
    /// grouped form that pulls additional symbols from `.` on the same line.
    /// The doctor regression we just fixed was a substring that only matched
    /// the bare form, so main.py adding `__version__` to the same import
    /// silently flipped the check off.
    #[test]
    fn registry_import_token_matches_grouped_and_bare_imports() {
        let bare = "from . import routers as router_registry";
        let grouped = "from . import __version__, routers as router_registry";
        let renamed_unrelated = "from . import routers as other_registry";
        assert!(source_contains_all(bare, &["routers as router_registry"]));
        assert!(source_contains_all(
            grouped,
            &["routers as router_registry"]
        ));
        assert!(
            !source_contains_all(renamed_unrelated, &["routers as router_registry"]),
            "an import that aliases routers to a different name must still fail"
        );
    }
}
