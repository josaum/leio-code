use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use regex::Regex;
use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct FrontendReadinessDoctor;

impl Doctor for FrontendReadinessDoctor {
    fn name(&self) -> &'static str {
        "frontend-readiness"
    }

    fn description(&self) -> &'static str {
        "Checks that the single Example customer-operations product UI (example-ops) stays build-honest, keeps intentional control/data-plane routing, and remains ready for cookie-backed Python auth flows without invalid Next route exports. Rejects resurrection of deprecated ops-console or jai-autopilot product trees and legacy production UI hosts."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_frontend_readiness(root)
    }
}

pub fn doctor_frontend_readiness(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    if !legacy_ops_console_absent(root) {
        warnings.push(
            "ops-console: deprecated second customer-ops UI tree still exists; example-ops is the single canonical UI"
                .to_string(),
        );
        evidence.push(EvidenceItem {
            kind: "frontend_readiness".to_string(),
            path: "ops-console".to_string(),
            line: None,
            detail: "deprecated duplicate customer-ops UI tree is present".to_string(),
        });
    }

    if !legacy_jai_autopilot_absent(root) {
        warnings.push(
            "jai-autopilot: deprecated separate product tree still exists; its customer-service, RevOps, collections, WhatsApp, and agent capabilities belong to the unified Example product"
                .to_string(),
        );
        evidence.push(EvidenceItem {
            kind: "frontend_readiness".to_string(),
            path: "jai-autopilot".to_string(),
            line: None,
            detail: "deprecated duplicate Autopilot product tree is present".to_string(),
        });
    }

    if !ops_api_docs_use_canonical_production_host(root) {
        warnings.push(
            "production UI host: example-ops API documentation must use app.getjai.com and must not restore bpo.getjai.com or gym-ops.vercel.app"
                .to_string(),
        );
        evidence.push(EvidenceItem {
            kind: "frontend_readiness".to_string(),
            path: "example-ops/docs/API.md".to_string(),
            line: None,
            detail: "the unified customer-ops product has exactly one production UI host"
                .to_string(),
        });
    }

    if !campaign_authority_is_consolidated(root) {
        warnings.push(
            "campaign authority: lifecycle routing, Rust pause/resume handlers, or Python tombstones have drifted; do not restore Redis/Python campaign stores"
                .to_string(),
        );
        evidence.push(EvidenceItem {
            kind: "frontend_readiness".to_string(),
            path: "example-ops/src/app/api/gateway/resolve-target.ts".to_string(),
            line: None,
            detail: "campaign lifecycle must resolve to the single tenant-scoped Rust aggregate"
                .to_string(),
        });
    }

    for app in [FrontendApp::new("example-ops")] {
        let package_path = root.join(app.name).join("package.json");
        let next_config_path = root.join(app.name).join("next.config.ts");
        let tsconfig_path = root.join(app.name).join("tsconfig.json");
        let jest_config_path = root.join(app.name).join("jest.config.cjs");
        let vendored_trpc_example_proxy_path = root
            .join(app.name)
            .join("vendor/jai-trpc/src/server/exampleProxy.ts");
        let flow_test_path = root
            .join(app.name)
            .join("src/lib/api/__tests__/flows.api.test.ts");
        let api_path = root.join(app.name).join("src/lib/api.ts");
        let api_core_path = root.join(app.name).join("src/lib/api/core.ts");
        let constants_path = root.join(app.name).join("src/lib/constants.ts");
        let whatsapp_path = root.join(app.name).join("src/lib/api/whatsapp.ts");
        let whatsapp_client_path = root.join(app.name).join("src/lib/api/WhatsAppClient.ts");
        let system_hud_path = root
            .join(app.name)
            .join("src/components/dashboard/nano/system-hud.tsx");
        let active_executions_path = root
            .join(app.name)
            .join("src/components/dashboard/nano/active-executions.tsx");
        let agent_issues_path = root
            .join(app.name)
            .join("src/components/dashboard/nano/agent-issues.tsx");
        let sse_provider_path = root
            .join(app.name)
            .join("src/providers/realtime/sse-provider.tsx");
        let gateway_route_path = root
            .join(app.name)
            .join("src/app/api/gateway/[...path]/route.ts");
        let gateway_resolve_target_path = root
            .join(app.name)
            .join("src/app/api/gateway/resolve-target.ts");
        let fitness_route_path = root
            .join(app.name)
            .join("src/app/api/fitness/[...path]/route.ts");
        let crm_route_path = root
            .join(app.name)
            .join("src/app/api/crm/[...path]/route.ts");
        let auth_shared_path = root.join(app.name).join("src/app/api/auth/shared.ts");
        let auth_login_route_path = root.join(app.name).join("src/app/api/auth/login/route.ts");
        let auth_refresh_route_path = root
            .join(app.name)
            .join("src/app/api/auth/refresh/route.ts");
        let auth_constants_path = root.join(app.name).join("src/lib/auth/constants.ts");
        let auth_normalization_test_path = root
            .join(app.name)
            .join("src/app/api/auth/__tests__/python-auth-normalization.test.ts");
        let auth_backend_preference_test_path = root
            .join(app.name)
            .join("src/app/api/auth/__tests__/backend-preference.test.ts");
        let gateway_resolve_target_test_path = root
            .join(app.name)
            .join("src/app/api/gateway/__tests__/resolve-target.test.ts");
        let auth_forwarding_test_path = root
            .join(app.name)
            .join("src/app/api/gateway/__tests__/auth-forwarding.test.ts");
        let auth_forwarding_helper_path = root
            .join(app.name)
            .join("src/app/api/gateway/auth-forwarding.ts");
        let sse_auth_lib_test_path = root
            .join(app.name)
            .join("src/lib/__tests__/sse-auth.test.ts");
        let sse_auth_hook_test_path = root
            .join(app.name)
            .join("src/hooks/infrastructure/__tests__/use-sse.auth.test.tsx");
        let deploy_python_api_script_path = root
            .join(app.name)
            .join("scripts/deploy_vercel_python_api.sh");
        let dockerfile_path = root.join(app.name).join("Dockerfile");
        let optimized_dockerfile_path = root.join(app.name).join("Dockerfile.optimized");
        let api_root = root.join(app.name).join("src/app/api");

        let package_src = read_text(&package_path, &mut warnings);
        let next_config_src = read_text(&next_config_path, &mut warnings);
        let tsconfig_src = read_text(&tsconfig_path, &mut warnings);
        let jest_config_src = read_text(&jest_config_path, &mut warnings);
        let vendored_trpc_example_proxy_src =
            read_text(&vendored_trpc_example_proxy_path, &mut warnings);
        let flow_test_src = read_text(&flow_test_path, &mut warnings);
        let api_src = read_text(&api_path, &mut warnings);
        let api_core_src = read_text(&api_core_path, &mut warnings);
        let constants_src = read_optional_text(&constants_path, &mut warnings);
        let whatsapp_src = read_text(&whatsapp_path, &mut warnings);
        let whatsapp_client_src = read_text(&whatsapp_client_path, &mut warnings);
        let system_hud_src = read_text(&system_hud_path, &mut warnings);
        let active_executions_src = read_text(&active_executions_path, &mut warnings);
        let agent_issues_src = read_text(&agent_issues_path, &mut warnings);
        let sse_provider_src = read_text(&sse_provider_path, &mut warnings);
        let gateway_route_src = read_text(&gateway_route_path, &mut warnings);
        let gateway_resolve_target_src =
            read_optional_text(&gateway_resolve_target_path, &mut warnings);
        let fitness_route_src = read_optional_text(&fitness_route_path, &mut warnings);
        let crm_route_src = read_optional_text(&crm_route_path, &mut warnings);
        let auth_shared_src = read_text(&auth_shared_path, &mut warnings);
        let auth_login_route_src = read_text(&auth_login_route_path, &mut warnings);
        let auth_refresh_route_src = read_optional_text(&auth_refresh_route_path, &mut warnings);
        let auth_constants_src = read_text(&auth_constants_path, &mut warnings);
        let auth_normalization_test_src = read_text(&auth_normalization_test_path, &mut warnings);
        let auth_backend_preference_test_src =
            read_optional_text(&auth_backend_preference_test_path, &mut warnings);
        let gateway_resolve_target_test_src =
            read_optional_text(&gateway_resolve_target_test_path, &mut warnings);
        let auth_forwarding_test_src =
            read_optional_text(&auth_forwarding_test_path, &mut warnings);
        let auth_forwarding_helper_src =
            read_optional_text(&auth_forwarding_helper_path, &mut warnings);
        let sse_auth_lib_test_src = read_text(&sse_auth_lib_test_path, &mut warnings);
        let sse_auth_hook_test_src = read_optional_text(&sse_auth_hook_test_path, &mut warnings);
        let deploy_python_api_script_src =
            read_optional_text(&deploy_python_api_script_path, &mut warnings);
        let dockerfile_src = read_optional_text(&dockerfile_path, &mut warnings);
        let optimized_dockerfile_src =
            read_optional_text(&optimized_dockerfile_path, &mut warnings);

        let package_json = package_src
            .as_deref()
            .and_then(|src| serde_json::from_str::<serde_json::Value>(src).ok());
        let scripts = package_json
            .as_ref()
            .and_then(|value| value.get("scripts"))
            .and_then(|value| value.as_object());
        let build_script = scripts
            .and_then(|scripts| scripts.get("build"))
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let typecheck_script = scripts
            .and_then(|scripts| scripts.get("typecheck"))
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let test_prod_script = scripts
            .and_then(|scripts| scripts.get("test:prod"))
            .and_then(|value| value.as_str())
            .unwrap_or("");

        let build_is_webpack = build_script == "next build --webpack";
        let has_honest_typecheck_script = typecheck_script.contains("next typegen")
            && typecheck_script.contains("tsc --noEmit")
            && typecheck_script.contains("tsconfig.tsbuildinfo");
        let has_test_prod = !test_prod_script.is_empty() && test_prod_script.contains("jest");
        let next_config_pins_app_root = next_config_src.as_deref().is_some_and(|src| {
            src.contains("const APP_ROOT = path.dirname(fileURLToPath(import.meta.url));")
                && src.contains("root: APP_ROOT")
        });
        let next_config_avoids_process_cwd = next_config_src.as_deref().is_some_and(|src| {
            !src.contains("root: process.cwd()") && !src.contains("root: process.cwd(),")
        });
        let next_config_does_not_ignore_type_errors = next_config_src
            .as_deref()
            .is_some_and(|src| !src.contains("ignoreBuildErrors: true"));
        let next_config_has_no_legacy_static_export = next_config_src
            .as_deref()
            .is_some_and(next_config_has_no_legacy_static_export);
        let deploy_script_has_no_legacy_static_export = deploy_python_api_script_src
            .as_deref()
            .map(deploy_script_has_no_legacy_static_export)
            .unwrap_or(true);
        let vendored_trpc_example_proxy_exists = vendored_trpc_example_proxy_path.exists();
        let tsconfig_uses_vendored_trpc = tsconfig_src.as_deref().is_some_and(|src| {
            src.contains("\"@jai/trpc/*\"")
                && src.contains("./vendor/jai-trpc/src/*")
                && !src.contains("../packages/trpc/src/*")
        });
        let tsconfig_uses_vendored_contracts = tsconfig_src.as_deref().is_some_and(|src| {
            src.contains("\"@contracts/generated/*\"")
                && src.contains("./vendor/example-ops-contracts/generated/*")
                && src.contains("\"@contracts/analytics/*\"")
                && src.contains("./vendor/example-ops-contracts/analytics/*")
                && !src.contains("../packages/example-ops-contracts/generated/*")
                && !src.contains("../packages/example-ops-contracts/analytics/*")
        });
        let jest_uses_vendored_trpc = jest_config_src.as_deref().is_some_and(|src| {
            src.contains("^@jai/trpc/(.*)$")
                && src.contains("<rootDir>/vendor/jai-trpc/src/$1")
                && !src.contains("<rootDir>/../packages/trpc/src/$1")
        });
        let jest_uses_vendored_contracts = jest_config_src.as_deref().is_some_and(|src| {
            src.contains("^@contracts/generated/(.*)$")
                && src.contains("<rootDir>/vendor/example-ops-contracts/generated/$1")
                && src.contains("^@contracts/analytics/(.*)$")
                && src.contains("<rootDir>/vendor/example-ops-contracts/analytics/$1")
        });
        let flow_tests_use_gateway_contract = flow_test_src.as_deref().is_some_and(|src| {
            src.contains("/api/gateway/api/flows/flow-1?tenant_id=tenant-1&include_json=true")
                && src.contains("/api/gateway/api/flows/flow-1/assets?tenant_id=tenant-1")
                && src.contains("/api/gateway/api/flows/validate")
        });
        let api_is_gateway_first_for_campaigns = api_src.as_deref().is_some_and(|src| {
            src.contains("GATEWAY_PROXY_BASE")
                && src.contains("/ops_console/api/campaigns")
                && src.contains("/ops_console/api/templates")
        });
        let api_uses_canonical_dashboard_runtime_surface =
            api_src.as_deref().is_some_and(|src| {
                src.contains("const CANONICAL_OPS_CONSOLE_API_BASE = \"/ops_console/api\";")
                    && src.contains("getActivity(limit?: number)")
                    && src.contains("getSystemHealth(): Promise<SystemHealth>")
                    && src.contains("getAgentConfigs(")
                    && src.contains("getLatestGlobalMessages(limit: number = 6)")
                    && src.contains("getExecutionEvents(executionId: string)")
                    && src.contains("${CANONICAL_OPS_CONSOLE_API_BASE}/activity")
                    && src.contains("${CANONICAL_OPS_CONSOLE_API_BASE}/health")
                    && src.contains("${CANONICAL_OPS_CONSOLE_API_BASE}/agent-configs")
                    && src.contains("${CANONICAL_OPS_CONSOLE_API_BASE}/conversations/latest?limit=${limit}")
                    && src.contains("/api/executions/${encodeURIComponent(executionId)}/events")
                    && !src.contains("version: \"python-proxy\"")
                    && !src.contains("${OPS_API_BASE}/runtime-agent-executions/${encodeURIComponent(executionId)}/events")
                    && !src.contains("${OPS_API_BASE}/conversations/latest?limit=${limit}")
            });
        let whatsapp_uses_ops_console_contract = whatsapp_src.as_deref().is_some_and(|src| {
            (src.contains("const OPS_CONSOLE_API_BASE = '/ops_console/api';")
                || src.contains("import { OPS_API_BASE } from '@/lib/constants';"))
                && src.contains("getWhatsAppRegistry")
                && src.contains("listTenants")
                && src.contains("/whatsapp-registry")
        });
        let whatsapp_list_tenants_is_registry_backed = whatsapp_src.as_deref().is_some_and(|src| {
            src.contains("const tenantsById = new Map<string, Tenant>();")
                && src.contains("getWhatsAppRegistry()")
                && (src.contains("for (const tenant of fallback.tenants")
                    || src.contains("if (tenantsById.size === 0)"))
                && src.contains("for (const entry of registry)")
                && src.contains("/tenants")
        });
        let whatsapp_client_is_gateway_first = whatsapp_client_src.as_deref().is_some_and(|src| {
            src.contains("const defaultBaseUrl = isServer")
                && src.contains(": '/api/gateway';")
                && src.contains("window.location.origin + this.config.baseUrl")
        });
        let dashboard_system_hud_handles_missing_telemetry =
            system_hud_src.as_deref().is_some_and(|src| {
                src.contains("SEM TELEMETRIA")
                    && src.contains("state={health?.duckdb_ready}")
                    && src.contains("state={health?.vault_ready}")
                    && src.contains("state={health?.llm_ready}")
            });
        let dashboard_agent_panels_use_api_helper = active_executions_src
            .as_deref()
            .is_some_and(|src| src.contains("api.getAgentExecutions({ limit: 20 })"))
            && agent_issues_src
                .as_deref()
                .is_some_and(|src| src.contains("api.getAgentExecutions({ limit: 100 })"));
        let api_does_not_stub_agent_executions_in_browser =
            api_src.as_deref().is_some_and(|src| {
                !src.contains("if (USE_PYTHON_API && typeof window !== \"undefined\") {\n      return {\n        executions: [],")
                    && !src.contains("if (USE_PYTHON_API && typeof window !== \"undefined\") {\r\n      return {\r\n        executions: [],")
            });
        let sse_provider_is_not_disabled_by_python_flag = sse_provider_src
            .as_deref()
            .is_some_and(|src| !src.contains("!USE_PYTHON_API"));
        let gateway_route_supports_whatsapp_templates =
            gateway_route_src.as_deref().is_some_and(|src| {
                src.contains("pattern: /^api\\/whatsapp\\/waba\\/([^/]+)\\/templates$/")
                    && src.contains("pattern: /^api\\/whatsapp\\/embedded_signup\\/exchange$/")
            });
        let gateway_route_supports_python_flow_transforms =
            gateway_route_src.as_deref().is_some_and(|src| {
                src.contains("pattern: /^api\\/flows$/") && src.contains("path: 'whatsapp/flows'")
            });
        let gateway_route_keeps_flows_on_rust_gateway = app.name == "example-ops"
            && gateway_resolve_target_test_src
                .as_deref()
                .is_some_and(|src| {
                    src.contains(
                        "WhatsApp Flow CRUD is owned by the Rust gateway canonical /api/flows handlers.",
                    ) && src.contains("[\"api/flows\", \"POST\"]")
                        && src.contains("[\"api/flows/validate\", \"POST\"]")
                });
        let gateway_route_supports_whatsapp_and_flows = gateway_route_supports_whatsapp_templates
            && (gateway_route_supports_python_flow_transforms
                || gateway_route_keeps_flows_on_rust_gateway);
        let auth_shared_has_python_backend_helpers =
            auth_shared_src.as_deref().is_some_and(|src| {
                src.contains("export type AuthBackend = \"gateway\" | \"python\";")
                    && src.contains("export function isPythonApiEnabled(): boolean {")
                    && src.contains("export function authBackendPreferenceOrder(): AuthBackend[] {")
            });
        let auth_constants_cover_admin_and_monitoring =
            auth_constants_src.as_deref().is_some_and(|src| {
                src.contains("ANALYTICS_READ: 'analytics:read'")
                    && src.contains("ADMIN_SETTINGS: 'admin:settings'")
                    && src.contains("PERMISSIONS.ANALYTICS_READ")
                    && src.contains("PERMISSIONS.ADMIN_SETTINGS")
            });
        let auth_constants_define_upstream_access_cookie = auth_constants_src
            .as_deref()
            .is_some_and(auth_constants_define_upstream_access_cookie);
        let auth_login_route_reissues_gateway_compatible_browser_token =
            auth_login_route_src.as_deref().is_some_and(|src| {
                (src.contains("import { generateToken } from \"@/lib/auth\";")
                    || src.contains("import { generateToken, PERMISSIONS } from \"@/lib/auth\";"))
                    && (src.contains("import { PERMISSIONS } from \"@/lib/auth/constants\";")
                        || src.contains(
                            "import { COOKIE_NAMES, PERMISSIONS } from \"@/lib/auth/constants\";",
                        )
                        || src
                            .contains("import { generateToken, PERMISSIONS } from \"@/lib/auth\";"))
                    && src.contains("authBackendPreferenceOrder,")
                    && src.contains("} from \"../shared\";")
                    && src.contains("const USE_PYTHON_API = isPythonApiEnabled();")
                    && src.contains("const BROWSER_SESSION_TTL_SECONDS = 24 * 60 * 60;")
                    && src
                        .matches("const browserToken = await generateToken({")
                        .count()
                        >= 2
                    && src.contains("const authResult = mapRustAuthPayload(data);")
                    && src.contains("ttlSeconds: BROWSER_SESSION_TTL_SECONDS")
                    && (src.contains("permissions: Object.values(PERMISSIONS)")
                        || src.contains("permissions: authResult.permissions"))
            });
        let api_core_includes_browser_credentials = api_core_src
            .as_deref()
            .is_some_and(api_core_includes_browser_credentials);
        let gateway_proxy_prefers_upstream_access_cookie = gateway_route_src
            .as_deref()
            .is_some_and(proxy_route_prefers_upstream_access_cookie)
            || auth_forwarding_helper_src
                .as_deref()
                .is_some_and(proxy_route_prefers_upstream_access_cookie);
        let fitness_proxy_prefers_upstream_access_cookie = fitness_route_src
            .as_deref()
            .map(proxy_route_prefers_upstream_access_cookie)
            .unwrap_or(true);
        let crm_proxy_prefers_upstream_access_cookie = crm_route_src
            .as_deref()
            .is_some_and(proxy_route_prefers_upstream_access_cookie);
        let auth_refresh_normalizes_token_cookie_targets = auth_refresh_route_src
            .as_deref()
            .is_some_and(refresh_route_normalizes_token_cookie_targets);
        let auth_forwarding_test_covers_upstream_cookie_bridge = auth_forwarding_test_src
            .as_deref()
            .is_some_and(auth_forwarding_tests_cover_upstream_cookie_bridge);
        let gym_control_plane_defaults_to_python = app.name != "example-ops"
            || [
                next_config_src.as_deref(),
                constants_src.as_deref(),
                gateway_resolve_target_src.as_deref(),
            ]
            .into_iter()
            .flatten()
            .all(uses_explicit_false_python_default);
        let gym_auth_prefers_python_by_default = app.name != "example-ops"
            || (auth_shared_src.as_deref().is_some_and(|src| {
                src.contains("return [\"python\", \"gateway\"];")
                    && src.contains("AUTH_BACKEND_PREFERENCE")
                    && src.contains("explicitPreference === \"gateway\"")
            }) && auth_backend_preference_test_src.as_deref().is_some_and(|src| {
                src.contains("prefers python auth by default")
                    && src.contains("expect(shared.authBackendPreferenceOrder()).toEqual([\"python\", \"gateway\"]);")
                    && src.contains("allows explicit gateway auth override")
            }));
        let gym_routes_control_plane_to_python_by_default = app.name != "example-ops"
            || gateway_resolve_target_test_src
                .as_deref()
                .is_some_and(|src| {
                    src.contains("defaults ops-console control-plane routes to the python API")
                        && src.contains("expect(routeModule.USE_PYTHON_API).toBe(true);")
                        && src.contains("resolveTarget(\"ops/api/tenants\", \"GET\")")
                        && src.contains("resolveTarget(\"ops_console/api/campaigns\", \"GET\")")
                });
        let gym_deploys_prebuilt_from_local_workspace = app.name != "example-ops"
            || deploy_python_api_script_src.as_deref().is_some_and(|src| {
                src.contains("AUTH_BACKEND_PREFERENCE \"python\"")
                    && src.contains("vercel --cwd \"$ROOT_DIR\" build --prod")
                    && src.contains("vercel --cwd \"$ROOT_DIR\" deploy --prebuilt --prod --yes")
            });
        let gym_docker_defaults_to_python = app.name != "example-ops"
            || [
                dockerfile_src.as_deref(),
                optimized_dockerfile_src.as_deref(),
            ]
            .into_iter()
            .flatten()
            .all(|src| src.contains("ARG USE_PYTHON_API=true"));
        let auth_python_normalization_is_tested =
            auth_normalization_test_src.as_deref().is_some_and(|src| {
                src.contains("maps admin role to admin settings and analytics")
                    && (src.contains("PERMISSIONS.ANALYTICS_VIEW")
                        || src.contains("\"analytics:read\""))
                    && src.contains("admin:settings")
                    && src.contains("maps superadmin aliases to super_admin with full permissions")
            });
        let sse_auth_is_covered = sse_auth_lib_test_src.as_deref().is_some_and(|src| {
            src.contains("forces the SSE token flow for same-origin connections when requested")
        }) || sse_auth_hook_test_src
            .as_deref()
            .is_some_and(|src| src.contains("forces the SSE token flow for same-origin URLs"));

        let invalid_route_exports = collect_invalid_route_exports(&api_root, &mut warnings);
        let invalid_route_export_count = invalid_route_exports.len();

        if !build_is_webpack {
            warnings.push(format!(
                "{}: package.json build script is not pinned to `next build --webpack`",
                app.name
            ));
        }
        if !has_honest_typecheck_script {
            warnings.push(format!(
                "{}: package.json is missing an honest `typecheck` script (`next typegen && tsc --noEmit` with stale cache reset)",
                app.name
            ));
        }
        if !has_test_prod {
            warnings.push(format!(
                "{}: package.json is missing a production Jest smoke script (`test:prod`)",
                app.name
            ));
        }
        if !next_config_pins_app_root {
            warnings.push(format!(
                "{}: next.config.ts does not pin `turbopack.root` to APP_ROOT",
                app.name
            ));
        }
        if !next_config_avoids_process_cwd {
            warnings.push(format!(
                "{}: next.config.ts still relies on `process.cwd()` for Turbopack root inference",
                app.name
            ));
        }
        if !next_config_does_not_ignore_type_errors {
            warnings.push(format!(
                "{}: next.config.ts still masks TypeScript build failures via `ignoreBuildErrors`",
                app.name
            ));
        }
        if !next_config_has_no_legacy_static_export {
            warnings.push(format!(
                "{}: next.config.ts still exposes legacy static export `/ops/ui` wiring",
                app.name
            ));
        }
        if !deploy_script_has_no_legacy_static_export {
            warnings.push(format!(
                "{}: Vercel deploy script still carries legacy OPS_CONSOLE_STATIC_EXPORT flags",
                app.name
            ));
        }
        if !vendored_trpc_example_proxy_exists {
            warnings.push(format!(
                "{}: vendored jai-trpc is incomplete; missing `vendor/jai-trpc/src/server/exampleProxy.ts`",
                app.name
            ));
        }
        if !tsconfig_uses_vendored_trpc {
            warnings.push(format!(
                "{}: tsconfig.json still points `@jai/trpc/*` outside the app bundle instead of the vendored copy",
                app.name
            ));
        }
        if !tsconfig_uses_vendored_contracts {
            warnings.push(format!(
                "{}: tsconfig.json still points `@contracts/*` outside the app bundle instead of vendored generated contracts",
                app.name
            ));
        }
        if !jest_uses_vendored_trpc {
            warnings.push(format!(
                "{}: jest.config.cjs still resolves `@jai/trpc/*` outside the app bundle instead of the vendored copy",
                app.name
            ));
        }
        if !jest_uses_vendored_contracts {
            warnings.push(format!(
                "{}: jest.config.cjs is missing vendored mappings for `@contracts/generated/*` and `@contracts/analytics/*`",
                app.name
            ));
        }
        if !flow_tests_use_gateway_contract {
            warnings.push(format!(
                "{}: flows API tests are not pinned to the gateway proxy contract",
                app.name
            ));
        }
        if !api_is_gateway_first_for_campaigns {
            warnings.push(format!(
                "{}: lib/api.ts is not clearly gateway-first for campaigns/templates/registry paths",
                app.name
            ));
        }
        if !api_uses_canonical_dashboard_runtime_surface {
            warnings.push(format!(
                "{}: lib/api.ts still lets dashboard runtime surfaces drift away from the canonical `/ops_console/api` path",
                app.name
            ));
        }
        if !whatsapp_uses_ops_console_contract {
            warnings.push(format!(
                "{}: lib/api/whatsapp.ts is not pinned to the canonical `/ops_console/api` discovery surface",
                app.name
            ));
        }
        if !whatsapp_list_tenants_is_registry_backed {
            warnings.push(format!(
                "{}: lib/api/whatsapp.ts does not build tenants from WhatsApp registry first with `/tenants` fallback",
                app.name
            ));
        }
        if !whatsapp_client_is_gateway_first {
            warnings.push(format!(
                "{}: WhatsAppClient is not pinned to `/api/gateway` in browser mode",
                app.name
            ));
        }
        if !dashboard_system_hud_handles_missing_telemetry {
            warnings.push(format!(
                "{}: dashboard SystemHUD still treats missing telemetry as hard-red instead of `SEM TELEMETRIA`",
                app.name
            ));
        }
        if !dashboard_agent_panels_use_api_helper {
            warnings.push(format!(
                "{}: dashboard agent panels are not using the shared `api.getAgentExecutions(...)` helper",
                app.name
            ));
        }
        if !api_does_not_stub_agent_executions_in_browser {
            warnings.push(format!(
                "{}: lib/api.ts still stubs agent executions to empty in browser mode",
                app.name
            ));
        }
        if !sse_provider_is_not_disabled_by_python_flag {
            warnings.push(format!(
                "{}: realtime SSE provider is still disabled by `USE_PYTHON_API` instead of relying on the gateway path",
                app.name
            ));
        }
        if !gateway_route_supports_whatsapp_and_flows {
            warnings.push(format!(
                "{}: gateway proxy route is missing the expected WhatsApp/flows compatibility transforms",
                app.name
            ));
        }
        if !auth_shared_has_python_backend_helpers {
            warnings.push(format!(
                "{}: app/api/auth/shared.ts is missing the shared Python-backend auth helpers (`AuthBackend`, `isPythonApiEnabled`, `authBackendPreferenceOrder`)",
                app.name
            ));
        }
        if !auth_constants_cover_admin_and_monitoring {
            warnings.push(format!(
                "{}: lib/auth/constants.ts is missing `ANALYTICS_READ` / `ADMIN_SETTINGS` parity or does not grant them to manager/admin roles",
                app.name
            ));
        }
        if !auth_constants_define_upstream_access_cookie {
            warnings.push(format!(
                "{}: lib/auth/constants.ts is missing the canonical `upstream_access_token` cookie name for Python bearer bridging",
                app.name
            ));
        }
        if !auth_login_route_reissues_gateway_compatible_browser_token {
            warnings.push(format!(
                "{}: app/api/auth/login/route.ts does not clearly reissue a gateway-compatible browser token for both Python and gateway auth flows",
                app.name
            ));
        }
        if !api_core_includes_browser_credentials {
            warnings.push(format!(
                "{}: lib/api/core.ts does not include browser cookies on fetches, so local auth-backed dashboard reads can drift",
                app.name
            ));
        }
        if !gateway_proxy_prefers_upstream_access_cookie {
            warnings.push(format!(
                "{}: app/api/gateway/[...path]/route.ts does not prefer `upstream_access_token` over stale Authorization headers for Python-backed browser sessions",
                app.name
            ));
        }
        if !fitness_proxy_prefers_upstream_access_cookie {
            warnings.push(format!(
                "{}: app/api/fitness/[...path]/route.ts does not prefer `upstream_access_token` over stale Authorization headers for Python-backed browser sessions",
                app.name
            ));
        }
        if !crm_proxy_prefers_upstream_access_cookie {
            warnings.push(format!(
                "{}: app/api/crm/[...path]/route.ts does not prefer `upstream_access_token` over stale Authorization headers for Python-backed browser sessions",
                app.name
            ));
        }
        if !auth_refresh_normalizes_token_cookie_targets {
            warnings.push(format!(
                "{}: app/api/auth/refresh/route.ts does not normalize `tokenCookie` for browser versus upstream Python token rotation",
                app.name
            ));
        }
        if !auth_forwarding_test_covers_upstream_cookie_bridge {
            warnings.push(format!(
                "{}: gateway auth forwarding lacks focused regression coverage for `upstream_access_token` and `tokenCookie` refresh behavior",
                app.name
            ));
        }
        if !auth_python_normalization_is_tested {
            warnings.push(format!(
                "{}: Python auth normalization is not covered by the expected admin/superadmin parity test",
                app.name
            ));
        }
        if !sse_auth_is_covered {
            warnings.push(format!(
                "{}: SSE auth flow is not covered by the expected same-origin token test",
                app.name
            ));
        }
        if !gym_control_plane_defaults_to_python {
            warnings.push(
                "example-ops: control-plane backend flags no longer default to Python unless explicitly set false".to_string(),
            );
        }
        if !gym_auth_prefers_python_by_default {
            warnings.push(
                "example-ops: auth backend preference no longer defaults to Python with gateway as an explicit/legacy fallback".to_string(),
            );
        }
        if !gym_routes_control_plane_to_python_by_default {
            warnings.push(
                "example-ops: route resolver tests no longer prove tenants/campaigns control-plane routes default to Python".to_string(),
            );
        }
        if !gym_deploys_prebuilt_from_local_workspace {
            warnings.push(
                "example-ops: Vercel deploy script no longer builds locally and deploys prebuilt output, risking workspace package drift".to_string(),
            );
        }
        if !gym_docker_defaults_to_python {
            warnings.push(
                "example-ops: Docker builds no longer default USE_PYTHON_API to true".to_string(),
            );
        }
        for violation in &invalid_route_exports {
            warnings.push(format!(
                "{}: {} exports `{}` from a Next route module, which breaks Next route typing",
                app.name,
                violation.path.display(),
                violation.export_name
            ));
        }

        for (path, src, needle, detail) in [
            (
                &package_path,
                package_src.as_ref(),
                "\"build\": \"next build --webpack\"",
                "frontend build script is pinned to webpack instead of the currently broken turbopack path",
            ),
            (
                &package_path,
                package_src.as_ref(),
                "\"typecheck\": \"rm -f tsconfig.tsbuildinfo && next typegen && tsc --noEmit --pretty false\"",
                "frontend typecheck script resets stale Next typegen cache before running TypeScript",
            ),
            (
                &next_config_path,
                next_config_src.as_ref(),
                "const APP_ROOT = path.dirname(fileURLToPath(import.meta.url));",
                "frontend Turbopack root is pinned to the app directory instead of process.cwd()",
            ),
            (
                &tsconfig_path,
                tsconfig_src.as_ref(),
                "./vendor/jai-trpc/src/*",
                "tsconfig keeps `@jai/trpc/*` inside the app via vendored sources",
            ),
            (
                &vendored_trpc_example_proxy_path,
                vendored_trpc_example_proxy_src.as_ref(),
                "handleExampleProxy",
                "vendored jai-trpc includes the server-side example proxy used by route handlers",
            ),
            (
                &tsconfig_path,
                tsconfig_src.as_ref(),
                "./vendor/example-ops-contracts/generated/*",
                "tsconfig keeps generated contracts inside the app via vendored sources",
            ),
            (
                &flow_test_path,
                flow_test_src.as_ref(),
                "/api/gateway/api/flows/flow-1?tenant_id=tenant-1&include_json=true",
                "flow API tests assert the canonical gateway proxy contract",
            ),
            (
                &api_path,
                api_src.as_ref(),
                "/ops_console/api/campaigns",
                "campaign CRUD goes through the canonical ops_console gateway surface",
            ),
            (
                &api_path,
                api_src.as_ref(),
                "const CANONICAL_OPS_CONSOLE_API_BASE = \"/ops_console/api\";",
                "dashboard runtime surfaces are pinned to the canonical ops_console API base",
            ),
            (
                &whatsapp_path,
                whatsapp_src.as_ref(),
                "const OPS_CONSOLE_API_BASE = '/ops_console/api';",
                "WhatsApp discovery stays on the canonical ops_console API surface",
            ),
            (
                &whatsapp_path,
                whatsapp_src.as_ref(),
                "const tenantsById = new Map<string, Tenant>();",
                "tenant discovery is built from the WhatsApp registry before falling back to `/tenants`",
            ),
            (
                &whatsapp_client_path,
                whatsapp_client_src.as_ref(),
                ": '/api/gateway';",
                "browser WhatsApp client defaults to the Next gateway proxy",
            ),
            (
                &system_hud_path,
                system_hud_src.as_ref(),
                "SEM TELEMETRIA",
                "dashboard system HUD distinguishes missing telemetry from hard failures",
            ),
            (
                &active_executions_path,
                active_executions_src.as_ref(),
                "api.getAgentExecutions({ limit: 20 })",
                "dashboard active executions use the shared agent-execution helper",
            ),
            (
                &agent_issues_path,
                agent_issues_src.as_ref(),
                "api.getAgentExecutions({ limit: 100 })",
                "dashboard agent-issues analysis uses the shared agent-execution helper",
            ),
            (
                &sse_provider_path,
                sse_provider_src.as_ref(),
                "const enabled = canConnect && !isPaused;",
                "dashboard SSE provider stays gateway-driven even when Python control-plane mode is enabled",
            ),
            (
                &gateway_route_path,
                gateway_route_src.as_ref(),
                "pattern: /^api\\/flows$/",
                "gateway proxy rewrites legacy flow operations onto canonical WhatsApp endpoints",
            ),
            (
                &auth_shared_path,
                auth_shared_src.as_ref(),
                "export function authBackendPreferenceOrder(): AuthBackend[] {",
                "auth shared helper keeps gateway/python backend preference logic canonical",
            ),
            (
                &auth_constants_path,
                auth_constants_src.as_ref(),
                "ANALYTICS_READ: 'analytics:read'",
                "auth constants expose the monitoring permission expected by config/monitoring pages",
            ),
            (
                &auth_constants_path,
                auth_constants_src.as_ref(),
                "UPSTREAM_ACCESS_TOKEN: 'upstream_access_token'",
                "auth constants expose the upstream Python bearer cookie used by browser-facing proxies",
            ),
            (
                &auth_login_route_path,
                auth_login_route_src.as_ref(),
                "const browserToken = await generateToken({",
                "Python and gateway auth flows reissue browser tokens compatible with gateway hot paths",
            ),
            (
                &auth_login_route_path,
                auth_login_route_src.as_ref(),
                "permissions: Object.values(PERMISSIONS)",
                "mock/superadmin login uses the full permission set instead of a wildcard placeholder",
            ),
            (
                &api_core_path,
                api_core_src.as_ref(),
                "credentials: \"include\"",
                "core API client includes browser cookies when calling app-local proxies",
            ),
            (
                if auth_forwarding_helper_src
                    .as_deref()
                    .is_some_and(proxy_route_prefers_upstream_access_cookie)
                {
                    &auth_forwarding_helper_path
                } else {
                    &gateway_route_path
                },
                if auth_forwarding_helper_src
                    .as_deref()
                    .is_some_and(proxy_route_prefers_upstream_access_cookie)
                {
                    auth_forwarding_helper_src.as_ref()
                } else {
                    gateway_route_src.as_ref()
                },
                "request.cookies.get(UPSTREAM_ACCESS_TOKEN_COOKIE)?.value ??",
                "gateway proxy prefers the upstream Python bearer cookie before falling back to the browser session token",
            ),
            (
                &auth_refresh_route_path,
                auth_refresh_route_src.as_ref(),
                "tokenCookie: UPSTREAM_ACCESS_TOKEN_COOKIE",
                "auth refresh normalizes Python refreshes into the upstream bearer cookie instead of clobbering the browser session cookie",
            ),
            (
                &auth_normalization_test_path,
                auth_normalization_test_src.as_ref(),
                "maps superadmin aliases to super_admin with full permissions",
                "Python auth normalization parity is covered by a focused test",
            ),
            (
                &auth_forwarding_test_path,
                auth_forwarding_test_src.as_ref(),
                "prefers the upstream python token when present",
                "gateway auth forwarding is covered by a focused upstream cookie regression test",
            ),
            (
                &jest_config_path,
                jest_config_src.as_ref(),
                "<rootDir>/vendor/jai-trpc/src/$1",
                "Jest resolves `@jai/trpc/*` against the vendored app copy",
            ),
            (
                &jest_config_path,
                jest_config_src.as_ref(),
                "<rootDir>/vendor/example-ops-contracts/generated/$1",
                "Jest resolves generated contracts against the vendored app copy",
            ),
        ] {
            if let Some(src) = src
                && let Some(line) = find_line(src, needle)
            {
                evidence.push(EvidenceItem {
                    kind: "frontend_readiness".to_string(),
                    path: path.display().to_string(),
                    line: Some(line),
                    detail: format!("{} ({})", detail, app.name),
                });
            }
        }

        for (path, src, needle, detail) in [
            (
                &sse_auth_lib_test_path,
                sse_auth_lib_test_src.as_ref(),
                "forces the SSE token flow for same-origin connections when requested",
                "SSE auth behavior is covered by the shared lib-level token-flow test",
            ),
            (
                &sse_auth_hook_test_path,
                sse_auth_hook_test_src.as_ref(),
                "forces the SSE token flow for same-origin URLs",
                "SSE auth behavior is covered by the hook-level same-origin token-flow test",
            ),
        ] {
            if let Some(src) = src
                && let Some(line) = find_line(src, needle)
            {
                evidence.push(EvidenceItem {
                    kind: "frontend_readiness".to_string(),
                    path: path.display().to_string(),
                    line: Some(line),
                    detail: format!("{} ({})", detail, app.name),
                });
            }
        }

        if next_config_has_no_legacy_static_export && let Some(src) = next_config_src.as_ref() {
            evidence.push(EvidenceItem {
                kind: "frontend_readiness".to_string(),
                path: next_config_path.display().to_string(),
                line: find_line(src, "Keep server routes enabled by default"),
                detail: format!(
                    "Next config has no legacy static export or `/ops/ui` basePath ({})",
                    app.name
                ),
            });
        }
        if deploy_script_has_no_legacy_static_export
            && let Some(src) = deploy_python_api_script_src.as_ref()
        {
            evidence.push(EvidenceItem {
                kind: "frontend_readiness".to_string(),
                path: deploy_python_api_script_path.display().to_string(),
                line: find_line(src, "vercel --cwd \"$ROOT_DIR\" build --prod"),
                detail: format!(
                    "Vercel deploy script no longer sets legacy static export flags ({})",
                    app.name
                ),
            });
        }

        for violation in invalid_route_exports {
            evidence.push(EvidenceItem {
                kind: "frontend_readiness".to_string(),
                path: violation.path.display().to_string(),
                line: Some(violation.line),
                detail: format!(
                    "invalid route export `{}` is not allowed from Next route modules",
                    violation.export_name
                ),
            });
        }

        let mut entity = json!({
            "app": app.name,
            "build_script": build_script,
            "typecheck_script": typecheck_script,
            "test_prod_script": test_prod_script,
            "build_is_webpack": build_is_webpack,
            "has_honest_typecheck_script": has_honest_typecheck_script,
            "has_test_prod": has_test_prod,
            "next_config_pins_app_root": next_config_pins_app_root,
            "next_config_avoids_process_cwd": next_config_avoids_process_cwd,
            "next_config_does_not_ignore_type_errors": next_config_does_not_ignore_type_errors,
            "next_config_has_no_legacy_static_export": next_config_has_no_legacy_static_export,
            "deploy_script_has_no_legacy_static_export": deploy_script_has_no_legacy_static_export,
            "vendored_trpc_example_proxy_exists": vendored_trpc_example_proxy_exists,
            "tsconfig_uses_vendored_trpc": tsconfig_uses_vendored_trpc,
            "tsconfig_uses_vendored_contracts": tsconfig_uses_vendored_contracts,
            "jest_uses_vendored_trpc": jest_uses_vendored_trpc,
            "jest_uses_vendored_contracts": jest_uses_vendored_contracts,
            "flow_tests_use_gateway_contract": flow_tests_use_gateway_contract,
            "api_is_gateway_first_for_campaigns": api_is_gateway_first_for_campaigns,
            "api_uses_canonical_dashboard_runtime_surface": api_uses_canonical_dashboard_runtime_surface,
            "whatsapp_uses_ops_console_contract": whatsapp_uses_ops_console_contract,
            "whatsapp_list_tenants_is_registry_backed": whatsapp_list_tenants_is_registry_backed,
            "whatsapp_client_is_gateway_first": whatsapp_client_is_gateway_first,
            "dashboard_system_hud_handles_missing_telemetry": dashboard_system_hud_handles_missing_telemetry,
            "dashboard_agent_panels_use_api_helper": dashboard_agent_panels_use_api_helper,
            "api_does_not_stub_agent_executions_in_browser": api_does_not_stub_agent_executions_in_browser,
            "sse_provider_is_not_disabled_by_python_flag": sse_provider_is_not_disabled_by_python_flag,
            "gateway_route_supports_whatsapp_and_flows": gateway_route_supports_whatsapp_and_flows,
        });
        let auth_entity = json!({
            "auth_shared_has_python_backend_helpers": auth_shared_has_python_backend_helpers,
            "auth_constants_cover_admin_and_monitoring": auth_constants_cover_admin_and_monitoring,
            "auth_constants_define_upstream_access_cookie": auth_constants_define_upstream_access_cookie,
            "auth_login_route_reissues_gateway_compatible_browser_token": auth_login_route_reissues_gateway_compatible_browser_token,
            "api_core_includes_browser_credentials": api_core_includes_browser_credentials,
            "gateway_proxy_prefers_upstream_access_cookie": gateway_proxy_prefers_upstream_access_cookie,
            "fitness_proxy_prefers_upstream_access_cookie": fitness_proxy_prefers_upstream_access_cookie,
            "crm_proxy_prefers_upstream_access_cookie": crm_proxy_prefers_upstream_access_cookie,
            "auth_refresh_normalizes_token_cookie_targets": auth_refresh_normalizes_token_cookie_targets,
            "auth_forwarding_test_covers_upstream_cookie_bridge": auth_forwarding_test_covers_upstream_cookie_bridge,
            "gym_control_plane_defaults_to_python": gym_control_plane_defaults_to_python,
            "gym_auth_prefers_python_by_default": gym_auth_prefers_python_by_default,
            "gym_routes_control_plane_to_python_by_default": gym_routes_control_plane_to_python_by_default,
            "gym_deploys_prebuilt_from_local_workspace": gym_deploys_prebuilt_from_local_workspace,
            "gym_docker_defaults_to_python": gym_docker_defaults_to_python,
            "auth_python_normalization_is_tested": auth_python_normalization_is_tested,
            "sse_auth_is_covered": sse_auth_is_covered,
            "invalid_route_export_count": invalid_route_export_count,
        });
        if let (Some(entity), Some(auth_entity)) = (entity.as_object_mut(), auth_entity.as_object())
        {
            entity.extend(auth_entity.clone());
        }
        entities.push(entity);
    }

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_frontend_readiness"),
        kind: "frontend_readiness".to_string(),
        summary: format!(
            "checked single-product, single-UI build-honest gateway-first readiness for example-ops, found {} warnings",
            warnings.len()
        ),
        confidence: if warnings.is_empty() { 0.98 } else { 0.71 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn legacy_ops_console_absent(root: &Path) -> bool {
    !root.join("ops-console/package.json").is_file()
}

fn campaign_authority_is_consolidated(root: &Path) -> bool {
    let read = |path: &str| fs::read_to_string(root.join(path)).unwrap_or_default();
    let resolver = read("example-ops/src/app/api/gateway/resolve-target.ts");
    let gateway_routes = read("example-gateway/src/ops_console/routes/mod.rs");
    let v2_campaigns = read("example-api/example/routers/campaigns.py");
    let ops_campaigns = read("example-api/example/routers/ops_console.py");
    let python_campaign_store = read("example-api/example/core/duckdb.py");
    let python_campaign_types = read("example-api/example/types/v2.py");
    let campaign_docs = read("docs/agents/campanhas.md");
    let openapi_json = read("example-api/docs/openapi.json");
    let openapi_yaml = read("example-api/docs/openapi.yaml");

    resolver.contains("RUST_DATA_PLANE_RULES")
        && (resolver.contains(r"ops_console\/api\/campaigns")
            || resolver.contains("ops_console/api/campaigns"))
        && resolver.contains("pause|resume")
        && resolver.contains("recovery-dispatcher")
        && gateway_routes.contains("pause_campaign_api")
        && gateway_routes.contains("resume_campaign_api")
        && gateway_routes.contains("reprocess_recovery_audience_api")
        && v2_campaigns.contains("retired_campaign_authority")
        && v2_campaigns.contains("HTTP_410_GONE")
        && v2_campaigns.contains("include_in_schema=False")
        && !v2_campaigns.contains("CampaignCRUD")
        && ops_campaigns.contains("retired_python_campaign_authority")
        && ops_campaigns.contains("HTTP_410_GONE")
        && ops_campaigns.contains("include_in_schema=False")
        && !ops_campaigns.contains("def _campaign_key(")
        && !ops_campaigns.contains("ops:campaign:*")
        && !ops_campaigns.contains("@router.get(\"/api/campaigns\")")
        && !ops_campaigns.contains("@router.post(\"/api/campaigns\")")
        && !ops_campaigns.contains("@router.patch(\"/api/campaigns/{campaign_id}\")")
        && campaign_docs.contains("410 Gone")
        && campaign_docs.contains("/ops_console/api/campaigns")
        && campaign_docs.contains("gateway Rust")
        && !python_campaign_store.contains("class CampaignCRUD")
        && !python_campaign_store.contains("CREATE TABLE IF NOT EXISTS campaigns")
        && !python_campaign_types.contains("class CampaignCreateRequest")
        && !root
            .join("example-api/scripts/whatsapp_campaign.py")
            .is_file()
        && !openapi_json.contains("\"/v2/campaigns")
        && !openapi_json.contains("\"/ops/api/campaigns")
        && !openapi_yaml.contains("/v2/campaigns:")
        && !openapi_yaml.contains("/ops/api/campaigns:")
}

fn legacy_jai_autopilot_absent(root: &Path) -> bool {
    !root.join("jai-autopilot/package.json").is_file()
}

fn ops_api_docs_use_canonical_production_host(root: &Path) -> bool {
    let docs = fs::read_to_string(root.join("example-ops/docs/API.md")).unwrap_or_default();
    docs.contains("**Production**: `https://app.getjai.com/api`")
        && !docs.contains("bpo.getjai.com")
        && !docs.contains("gym-ops.vercel.app")
}

struct FrontendApp {
    name: &'static str,
}

impl FrontendApp {
    const fn new(name: &'static str) -> Self {
        Self { name }
    }
}

fn read_optional_text(path: &Path, warnings: &mut Vec<String>) -> Option<String> {
    if path.exists() {
        read_text(path, warnings)
    } else {
        None
    }
}

fn api_core_includes_browser_credentials(src: &str) -> bool {
    src.contains("credentials: \"include\"") || src.contains("credentials: 'include'")
}

fn auth_constants_define_upstream_access_cookie(src: &str) -> bool {
    src.contains("UPSTREAM_ACCESS_TOKEN: 'upstream_access_token'")
        || src.contains("UPSTREAM_ACCESS_TOKEN: \"upstream_access_token\"")
}

fn proxy_route_prefers_upstream_access_cookie(src: &str) -> bool {
    let local_cookie_precedence = src
        .contains("request.cookies.get(UPSTREAM_ACCESS_TOKEN_COOKIE)?.value ??")
        && src.contains("request.cookies.get(ACCESS_TOKEN_COOKIE)?.value")
        && (src.contains("headers.set('authorization', `Bearer ${token}`)")
            || src.contains("headers.set(\"authorization\", `Bearer ${token}`)"));
    let shared_helper_precedence = src.contains("injectAuthFromCookie(headers, request, {")
        && (src.contains("preferLocalAccessToken: !USE_PYTHON_API")
            || src.contains("preferLocalAccessToken: false"));

    local_cookie_precedence || shared_helper_precedence
}

fn refresh_route_normalizes_token_cookie_targets(src: &str) -> bool {
    src.contains("tokenCookie: UPSTREAM_ACCESS_TOKEN_COOKIE")
        && src.contains("tokenCookie: ACCESS_TOKEN_COOKIE")
        && src.contains(
            "appendCookie(response, request, UPSTREAM_ACCESS_TOKEN_COOKIE, accessToken, expiresIn);",
        )
        && src.contains(
            "appendCookie(response, request, UPSTREAM_ACCESS_TOKEN_COOKIE, token, 3600);",
        )
        && src.contains(
            "request,\n      ACCESS_TOKEN_COOKIE,\n      browserToken,\n      BROWSER_SESSION_TTL_SECONDS,",
        )
}

fn auth_forwarding_tests_cover_upstream_cookie_bridge(src: &str) -> bool {
    src.contains("prefers the upstream python token when present")
        && src.contains("tokenCookie: \"upstream_access_token\"")
        && src.contains("Bearer python-token")
}

fn uses_explicit_false_python_default(src: &str) -> bool {
    src.contains("const isExplicitFalse =")
        && src.contains("!isExplicitFalse(process.env.USE_PYTHON_API)")
        && src.contains("!isExplicitFalse(process.env.NEXT_PUBLIC_USE_PYTHON_API)")
}

fn next_config_has_no_legacy_static_export(src: &str) -> bool {
    !src.contains("OPS_CONSOLE_STATIC_EXPORT")
        && !src.contains("NEXT_PUBLIC_OPS_CONSOLE_STATIC_EXPORT")
        && !src.contains("basePath: '/ops/ui'")
        && !src.contains("basePath: \"/ops/ui\"")
        && !src.contains("output: 'export'")
        && !src.contains("output: \"export\"")
}

fn deploy_script_has_no_legacy_static_export(src: &str) -> bool {
    !src.contains("OPS_CONSOLE_STATIC_EXPORT")
        && !src.contains("NEXT_PUBLIC_OPS_CONSOLE_STATIC_EXPORT")
}

struct RouteExportViolation {
    path: PathBuf,
    line: usize,
    export_name: String,
}

fn collect_invalid_route_exports(
    root: &Path,
    warnings: &mut Vec<String>,
) -> Vec<RouteExportViolation> {
    let mut files = Vec::new();
    collect_route_files(root, &mut files, warnings);
    let mut violations = Vec::new();
    let function_re =
        Regex::new(r"^\s*export\s+(?:async\s+)?function\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let value_re =
        Regex::new(r"^\s*export\s+(?:const|let|var)\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let type_re =
        Regex::new(r"^\s*export\s+(?:type|interface|class|enum)\s+([A-Za-z_][A-Za-z0-9_]*)")
            .unwrap();
    let named_re = Regex::new(r"^\s*export\s*\{([^}]+)\}").unwrap();
    let allowed: BTreeSet<&str> = [
        "GET",
        "POST",
        "PUT",
        "PATCH",
        "DELETE",
        "HEAD",
        "OPTIONS",
        "runtime",
        "dynamic",
        "dynamicParams",
        "revalidate",
        "fetchCache",
        "preferredRegion",
        "maxDuration",
        "config",
    ]
    .into_iter()
    .collect();

    for file in files {
        let src = match fs::read_to_string(&file) {
            Ok(src) => src,
            Err(err) => {
                warnings.push(format!("failed to read {}: {}", file.display(), err));
                continue;
            }
        };

        for (index, line) in src.lines().enumerate() {
            let trimmed = line.trim();
            if !trimmed.starts_with("export ") {
                continue;
            }
            for captures in [
                function_re.captures(trimmed),
                value_re.captures(trimmed),
                type_re.captures(trimmed),
            ]
            .into_iter()
            .flatten()
            {
                let name = captures.get(1).unwrap().as_str();
                if !allowed.contains(name) {
                    violations.push(RouteExportViolation {
                        path: file.clone(),
                        line: index + 1,
                        export_name: name.to_string(),
                    });
                }
            }

            if let Some(captures) = named_re.captures(trimmed) {
                let names = captures
                    .get(1)
                    .map(|match_| match_.as_str())
                    .unwrap_or("")
                    .split(',')
                    .filter_map(|segment| {
                        let raw = segment.trim();
                        if raw.is_empty() {
                            return None;
                        }
                        Some(raw.split_whitespace().next().unwrap_or(raw))
                    });
                for name in names {
                    if !allowed.contains(name) {
                        violations.push(RouteExportViolation {
                            path: file.clone(),
                            line: index + 1,
                            export_name: name.to_string(),
                        });
                    }
                }
            }
        }
    }

    violations
}

fn collect_route_files(root: &Path, files: &mut Vec<PathBuf>, warnings: &mut Vec<String>) {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) => {
            warnings.push(format!("failed to read {}: {}", root.display(), err));
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                warnings.push(format!("failed to iterate {}: {}", root.display(), err));
                continue;
            }
        };
        let path = entry.path();
        if path.is_dir() {
            collect_route_files(&path, files, warnings);
        } else if path.file_name().and_then(|name| name.to_str()) == Some("route.ts") {
            files.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        api_core_includes_browser_credentials, auth_constants_define_upstream_access_cookie,
        auth_forwarding_tests_cover_upstream_cookie_bridge, campaign_authority_is_consolidated,
        deploy_script_has_no_legacy_static_export, legacy_jai_autopilot_absent,
        legacy_ops_console_absent, next_config_has_no_legacy_static_export,
        ops_api_docs_use_canonical_production_host, proxy_route_prefers_upstream_access_cookie,
        refresh_route_normalizes_token_cookie_targets,
    };
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn detects_browser_credentials_in_core_client() {
        assert!(api_core_includes_browser_credentials(
            "fetch(url, { credentials: \"include\" })"
        ));
        assert!(!api_core_includes_browser_credentials(
            "fetch(url, { credentials: \"same-origin\" })"
        ));
    }

    #[test]
    fn detects_upstream_access_cookie_contract() {
        assert!(auth_constants_define_upstream_access_cookie(
            "UPSTREAM_ACCESS_TOKEN: 'upstream_access_token'"
        ));
        assert!(!auth_constants_define_upstream_access_cookie(
            "ACCESS_TOKEN: 'access_token'"
        ));
    }

    #[test]
    fn proxy_route_requires_upstream_cookie_preference() {
        let good = r#"
const token =
  request.cookies.get(UPSTREAM_ACCESS_TOKEN_COOKIE)?.value ??
  request.cookies.get(ACCESS_TOKEN_COOKIE)?.value;
headers.set('authorization', `Bearer ${token}`);
"#;
        let bad = r#"
if (headers.has('authorization')) return;
const token = request.cookies.get(AUTH_COOKIE)?.value;
"#;
        let explicit_shared_helper = r#"
injectAuthFromCookie(headers, request, {
  preferLocalAccessToken: false,
  allowLocalAccessTokenFallback: true,
});
"#;

        assert!(proxy_route_prefers_upstream_access_cookie(good));
        assert!(proxy_route_prefers_upstream_access_cookie(
            explicit_shared_helper
        ));
        assert!(!proxy_route_prefers_upstream_access_cookie(bad));
    }

    #[test]
    fn refresh_route_requires_token_cookie_normalization() {
        let good = r#"
tokenCookie: UPSTREAM_ACCESS_TOKEN_COOKIE
tokenCookie: ACCESS_TOKEN_COOKIE
appendCookie(response, request, UPSTREAM_ACCESS_TOKEN_COOKIE, accessToken, expiresIn);
appendCookie(response, request, UPSTREAM_ACCESS_TOKEN_COOKIE, token, 3600);
appendCookie(
      response,
      request,
      ACCESS_TOKEN_COOKIE,
      browserToken,
      BROWSER_SESSION_TTL_SECONDS,
);
"#;
        let clobbers_browser_session = r#"
tokenCookie: UPSTREAM_ACCESS_TOKEN_COOKIE
tokenCookie: ACCESS_TOKEN_COOKIE
appendCookie(response, request, ACCESS_TOKEN_COOKIE, accessToken, expiresIn);
appendCookie(response, request, ACCESS_TOKEN_COOKIE, token, 3600);
appendCookie(
      response,
      request,
      ACCESS_TOKEN_COOKIE,
      browserToken,
      BROWSER_SESSION_TTL_SECONDS,
);
"#;

        assert!(refresh_route_normalizes_token_cookie_targets(good));
        assert!(!refresh_route_normalizes_token_cookie_targets(
            clobbers_browser_session
        ));
    }

    #[test]
    fn auth_forwarding_tests_require_upstream_cookie_regression_coverage() {
        let good = r#"
it("prefers the upstream python token when present", async () => {});
tokenCookie: "upstream_access_token"
expect(forwardedHeaders.get("Authorization")).toBe("Bearer python-token");
"#;
        let bad = r#"
it("refreshes the token", async () => {});
tokenCookie: "access_token"
"#;

        assert!(auth_forwarding_tests_cover_upstream_cookie_bridge(good));
        assert!(!auth_forwarding_tests_cover_upstream_cookie_bridge(bad));
    }

    #[test]
    fn rejects_legacy_ops_ui_static_export_wiring() {
        let clean_next_config = r#"
const nextConfig = {
  ...(!IS_VERCEL && {
    output: 'standalone' as const,
  }),
  async rewrites() {
    return [{ source: '/ops/:path*', destination: `${PYTHON_API_URL}/ops/:path*` }];
  },
};
"#;
        let legacy_next_config = r#"
const ENABLE_STATIC_EXPORT =
  process.env.OPS_CONSOLE_STATIC_EXPORT === 'true' ||
  process.env.NEXT_PUBLIC_OPS_CONSOLE_STATIC_EXPORT === 'true';
const nextConfig = {
  ...(ENABLE_STATIC_EXPORT && {
    output: 'export' as const,
    basePath: '/ops/ui' as const,
  }),
};
"#;
        let clean_deploy_script = r#"
NEXT_PUBLIC_OPS_API_PATH="/ops" \
WHATSAPP_WEBHOOK_BACKEND=python \
vercel --cwd "$ROOT_DIR" build --prod
"#;
        let legacy_deploy_script = r#"
set_vercel_env "$target" OPS_CONSOLE_STATIC_EXPORT "false"
NEXT_PUBLIC_OPS_CONSOLE_STATIC_EXPORT=false \
vercel --cwd "$ROOT_DIR" build --prod
"#;

        assert!(next_config_has_no_legacy_static_export(clean_next_config));
        assert!(!next_config_has_no_legacy_static_export(legacy_next_config));
        assert!(deploy_script_has_no_legacy_static_export(
            clean_deploy_script
        ));
        assert!(!deploy_script_has_no_legacy_static_export(
            legacy_deploy_script
        ));
    }

    #[test]
    fn rejects_a_second_ops_ui_tree() {
        let dir = unique_tempdir("single-ops-ui");

        assert!(legacy_ops_console_absent(&dir));

        write_file(&dir.join("ops-console/package.json"), "{}");
        assert!(!legacy_ops_console_absent(&dir));

        cleanup(&dir);
    }

    #[test]
    fn rejects_a_separate_autopilot_product() {
        let dir = unique_tempdir("single-autopilot-ui");

        assert!(legacy_jai_autopilot_absent(&dir));

        write_file(&dir.join("jai-autopilot/package.json"), "{}");
        assert!(!legacy_jai_autopilot_absent(&dir));

        cleanup(&dir);
    }

    #[test]
    fn rejects_legacy_production_ui_hosts_in_ops_api_docs() {
        let dir = unique_tempdir("canonical-production-host");
        let docs = dir.join("example-ops/docs/API.md");

        write_file(&docs, "- **Production**: `https://app.getjai.com/api`\n");
        assert!(ops_api_docs_use_canonical_production_host(&dir));

        write_file(&docs, "- **Production**: `https://bpo.getjai.com/api`\n");
        assert!(!ops_api_docs_use_canonical_production_host(&dir));

        cleanup(&dir);
    }

    #[test]
    fn campaign_authority_requires_rust_routing_and_python_tombstones() {
        let dir = unique_tempdir("campaign-authority");
        write_file(
            &dir.join("example-ops/src/app/api/gateway/resolve-target.ts"),
            "RUST_DATA_PLANE_RULES ops_console/api/campaigns pause|resume recovery-dispatcher",
        );
        write_file(
            &dir.join("example-gateway/src/ops_console/routes/mod.rs"),
            "pause_campaign_api resume_campaign_api reprocess_recovery_audience_api",
        );
        write_file(
            &dir.join("example-api/example/routers/campaigns.py"),
            "retired_campaign_authority HTTP_410_GONE Rust gateway include_in_schema=False",
        );
        write_file(
            &dir.join("example-api/example/routers/ops_console.py"),
            "retired_python_campaign_authority HTTP_410_GONE include_in_schema=False",
        );
        write_file(
            &dir.join("docs/agents/campanhas.md"),
            "410 Gone /ops_console/api/campaigns gateway Rust",
        );

        assert!(campaign_authority_is_consolidated(&dir));

        write_file(
            &dir.join("example-api/example/routers/campaigns.py"),
            "CampaignCRUD.create CampaignCRUD.update",
        );
        assert!(!campaign_authority_is_consolidated(&dir));

        write_file(
            &dir.join("example-api/example/routers/campaigns.py"),
            "retired_campaign_authority HTTP_410_GONE Rust gateway include_in_schema=False",
        );
        write_file(
            &dir.join("docs/agents/campanhas.md"),
            "POST /v2/campaigns is the active campaign API",
        );
        assert!(!campaign_authority_is_consolidated(&dir));

        write_file(
            &dir.join("docs/agents/campanhas.md"),
            "410 Gone /ops_console/api/campaigns gateway Rust",
        );
        write_file(
            &dir.join("example-api/docs/openapi.json"),
            r#"{"paths":{"/v2/campaigns":{}}}"#,
        );
        assert!(!campaign_authority_is_consolidated(&dir));

        cleanup(&dir);
    }

    fn unique_tempdir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-frontend-readiness-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(path, contents).expect("write fixture file");
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }
}
