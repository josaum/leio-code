use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct CompositionResolverDoctor;

impl Doctor for CompositionResolverDoctor {
    fn name(&self) -> &'static str {
        "composition-resolver"
    }

    fn description(&self) -> &'static str {
        "Checks that shared protected-shell composition stays server-owned and routes provider/auth wiring through ops-core instead of drifting back to app-local gates."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_composition_resolver(root)
    }
}

struct FrontendCompositionTarget {
    name: &'static str,
    composition_path: &'static str,
    layout_path: &'static str,
    providers_path: &'static str,
    shell_path: &'static str,
    auth_provider_path: &'static str,
    auth_server_path: &'static str,
    use_auth_path: &'static str,
}

const TARGETS: &[FrontendCompositionTarget] = &[FrontendCompositionTarget {
    name: "example-ops",
    composition_path: "example-ops/src/lib/app-composition.ts",
    layout_path: "example-ops/src/app/(ops)/layout.tsx",
    providers_path: "example-ops/src/app/(ops)/ops-providers.tsx",
    shell_path: "example-ops/src/components/layout/app-shell.tsx",
    auth_provider_path: "example-ops/src/providers/auth/auth-provider.tsx",
    auth_server_path: "example-ops/src/lib/auth-server.ts",
    use_auth_path: "example-ops/src/hooks/auth/use-auth.ts",
}];

pub fn doctor_composition_resolver(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let composition_types_path = root.join("packages/ops-core/src/composition/types.ts");
    let shell_frame_path = root.join("packages/ops-core/src/shell/protected-shell-frame.tsx");
    let routing_policy_path = root.join("packages/ops-core/src/routing/policy.ts");

    let composition_types_src = read_text(&composition_types_path, &mut warnings);
    let shell_frame_src = read_text(&shell_frame_path, &mut warnings);
    let routing_policy_src = read_text(&routing_policy_path, &mut warnings);

    let shared_composition_contract_intact = composition_types_src.as_deref().is_some_and(|src| {
        src.contains("export interface OpsAppComposition")
            && src.contains("capabilityPacks: string[];")
            && src.contains("enabledRoutes: string[];")
            && src.contains("providerPolicy: ProviderRoutePolicy;")
    });
    let shared_shell_frame_intact = shell_frame_src.as_deref().is_some_and(|src| {
        src.contains("export function ProtectedShellFrame")
            && src.contains("isAuthenticated: boolean;")
            && src.contains("isLoading: boolean;")
            && src.contains("href=\"#main-content\"")
    });
    let shared_route_policy_intact = routing_policy_src.as_deref().is_some_and(|src| {
        src.contains("export interface ProviderRoutePolicy")
            && src.contains("realtimeRoutes: readonly PathMatcher[];")
            && src.contains("ingestRoutes: readonly PathMatcher[];")
            && src.contains("liveIndicatorRoutes: readonly PathMatcher[];")
            && src.contains("export function matchesPathPolicy")
    });

    if !shared_composition_contract_intact {
        warnings.push(
            "ops-core composition contract no longer clearly defines capability packs, enabled routes, and provider policy".to_string(),
        );
    }
    if !shared_shell_frame_intact {
        warnings.push(
            "ops-core protected shell frame no longer clearly owns authenticated/loading shell fallback behavior".to_string(),
        );
    }
    if !shared_route_policy_intact {
        warnings.push(
            "ops-core route policy helpers no longer clearly expose provider policy matchers for frontend shells".to_string(),
        );
    }

    for (path, src, needle, detail) in [
        (
            &composition_types_path,
            composition_types_src.as_ref(),
            "export interface OpsAppComposition",
            "ops-core publishes the shared app-composition contract",
        ),
        (
            &shell_frame_path,
            shell_frame_src.as_ref(),
            "export function ProtectedShellFrame",
            "ops-core publishes the shared protected shell frame",
        ),
        (
            &routing_policy_path,
            routing_policy_src.as_ref(),
            "export function matchesPathPolicy",
            "ops-core publishes the shared provider-route matcher",
        ),
    ] {
        push_evidence(&mut evidence, path, src, needle, detail, "shared_contract");
    }

    for target in TARGETS {
        let composition_path = root.join(target.composition_path);
        let layout_path = root.join(target.layout_path);
        let providers_path = root.join(target.providers_path);
        let shell_path = root.join(target.shell_path);
        let auth_provider_path = root.join(target.auth_provider_path);
        let auth_server_path = root.join(target.auth_server_path);
        let use_auth_path = root.join(target.use_auth_path);

        let composition_src = read_text(&composition_path, &mut warnings);
        let layout_src = read_text(&layout_path, &mut warnings);
        let providers_src = read_text(&providers_path, &mut warnings);
        let shell_src = read_text(&shell_path, &mut warnings);
        let auth_provider_src = read_text(&auth_provider_path, &mut warnings);
        let auth_server_src = read_text(&auth_server_path, &mut warnings);
        let use_auth_src = read_text(&use_auth_path, &mut warnings);

        let composition_is_shared = composition_src.as_deref().is_some_and(|src| {
            src.contains(
                "import type { OpsAppComposition } from \"@example/ops-core/composition\";",
            ) && src.contains("export const appComposition: OpsAppComposition = {")
                && src.contains("capabilityPacks:")
                && src.contains("enabledRoutes:")
                && src.contains("providerPolicy:")
                && src.contains("featureBindings:")
        });
        let layout_uses_server_auth_gate = layout_src.as_deref().is_some_and(|src| {
            src.contains("const initialAuth = await getServerAuthState();")
                && src.contains("if (!initialAuth.isAuthenticated)")
                && src.contains("redirect('/login');")
                && src.contains("<AuthProvider initialAuth={initialAuth}>")
                && src.contains("<OpsProviders>")
                && src.contains("<AppShell>{children}</AppShell>")
        });
        let providers_follow_composition_policy = providers_src.as_deref().is_some_and(|src| {
            src.contains("import { matchesPathPolicy } from '@example/ops-core/routing';")
                && src.contains("import { appComposition } from '@/lib/app-composition';")
                && src.contains("appComposition.providerPolicy.realtimeRoutes")
                && src.contains("appComposition.providerPolicy.ingestRoutes")
        });
        let shell_uses_shared_frame = shell_src.as_deref().is_some_and(|src| {
            src.contains("import { ProtectedShellFrame } from \"@example/ops-core/shell\";")
                && src.contains("const { isAuthenticated, isLoading } = useAuthContext();")
                && src.contains("<ProtectedShellFrame")
        });
        let auth_provider_uses_shared_factory = auth_provider_src.as_deref().is_some_and(|src| {
            src.contains("import { createOpsAuthProvider } from \"@example/ops-core/auth\";")
                && src.contains("registerAuthFailureHandler: onAuthFailure")
        });
        let auth_server_uses_shared_snapshot = auth_server_src.as_deref().is_some_and(|src| {
            src.contains(
                "import { buildServerAuthState, type AuthState } from '@example/ops-core/auth';",
            ) && src.contains("export async function getServerAuthState(): Promise<AuthState>")
                && src.contains("return buildServerAuthState(await getCurrentUser());")
        });
        let use_auth_uses_shared_hook = use_auth_src.as_deref().is_some_and(|src| {
            src.contains("import { useOpsAuth, type AuthState, type UseAuthReturn } from \"@example/ops-core/auth\";")
                && src.contains("return useOpsAuth({")
                && src.contains("invalidateRealtimeCache: invalidateSseTokenCache")
        });

        if !composition_is_shared {
            warnings.push(format!(
                "{} no longer clearly exports a shared OpsAppComposition contract",
                target.name
            ));
        }
        if !layout_uses_server_auth_gate {
            warnings.push(format!(
                "{} protected layout no longer clearly enforces server auth before shell render",
                target.name
            ));
        }
        if !providers_follow_composition_policy {
            warnings.push(format!(
                "{} ops providers no longer clearly derive realtime/ingest mounting from appComposition.providerPolicy",
                target.name
            ));
        }
        if !shell_uses_shared_frame {
            warnings.push(format!(
                "{} app shell no longer clearly uses the shared ProtectedShellFrame",
                target.name
            ));
        }
        if !auth_provider_uses_shared_factory {
            warnings.push(format!(
                "{} auth provider no longer clearly uses createOpsAuthProvider from ops-core",
                target.name
            ));
        }
        if !auth_server_uses_shared_snapshot {
            warnings.push(format!(
                "{} auth-server no longer clearly builds its snapshot through ops-core",
                target.name
            ));
        }
        if !use_auth_uses_shared_hook {
            warnings.push(format!(
                "{} use-auth hook no longer clearly delegates to useOpsAuth from ops-core",
                target.name
            ));
        }

        for (path, src, needle, detail, kind) in [
            (
                &composition_path,
                composition_src.as_ref(),
                "export const appComposition: OpsAppComposition = {",
                "frontend publishes its app-specific shared composition object",
                "composition",
            ),
            (
                &layout_path,
                layout_src.as_ref(),
                "const initialAuth = await getServerAuthState();",
                "protected layout snapshots auth on the server before rendering",
                "auth_guard",
            ),
            (
                &layout_path,
                layout_src.as_ref(),
                "<AuthProvider initialAuth={initialAuth}>",
                "protected layout seeds the shared auth provider with the server snapshot",
                "auth_guard",
            ),
            (
                &providers_path,
                providers_src.as_ref(),
                "appComposition.providerPolicy.realtimeRoutes",
                "ops providers derive realtime mounting from shared composition policy",
                "provider_policy",
            ),
            (
                &shell_path,
                shell_src.as_ref(),
                "import { ProtectedShellFrame } from \"@example/ops-core/shell\";",
                "app shell reuses the shared protected frame",
                "shell",
            ),
            (
                &auth_provider_path,
                auth_provider_src.as_ref(),
                "createOpsAuthProvider",
                "auth provider is built from the shared ops-core factory",
                "auth_bridge",
            ),
            (
                &auth_server_path,
                auth_server_src.as_ref(),
                "return buildServerAuthState(await getCurrentUser());",
                "server auth snapshot is delegated to ops-core",
                "auth_bridge",
            ),
            (
                &use_auth_path,
                use_auth_src.as_ref(),
                "return useOpsAuth({",
                "client auth hook is delegated to ops-core",
                "auth_bridge",
            ),
        ] {
            push_evidence(&mut evidence, path, src, needle, detail, kind);
        }

        entities.push(json!({
            "app": target.name,
            "composition_is_shared": composition_is_shared,
            "layout_uses_server_auth_gate": layout_uses_server_auth_gate,
            "providers_follow_composition_policy": providers_follow_composition_policy,
            "shell_uses_shared_frame": shell_uses_shared_frame,
            "auth_provider_uses_shared_factory": auth_provider_uses_shared_factory,
            "auth_server_uses_shared_snapshot": auth_server_uses_shared_snapshot,
            "use_auth_uses_shared_hook": use_auth_uses_shared_hook,
        }));
    }

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_composition_resolver"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            "shared protected-shell composition wiring is intact for the single Example Ops UI"
                .to_string()
        } else {
            format!(
                "composition resolver checks found {} warning(s) across shared shell, auth, or provider-policy wiring",
                warnings.len()
            )
        },
        confidence: if warnings.is_empty() { 0.97 } else { 0.73 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn push_evidence(
    evidence: &mut Vec<EvidenceItem>,
    path: &Path,
    src: Option<&String>,
    needle: &str,
    detail: &str,
    kind: &str,
) {
    if let Some(src) = src
        && let Some(line) = find_line(src, needle)
    {
        evidence.push(EvidenceItem {
            kind: kind.to_string(),
            path: path.display().to_string(),
            line: Some(line),
            detail: detail.to_string(),
        });
    }
}
