use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct TenantOverrideContractDoctor;

impl Doctor for TenantOverrideContractDoctor {
    fn name(&self) -> &'static str {
        "tenant-override-contract"
    }

    fn description(&self) -> &'static str {
        "Checks that tenant switching rejects raw headers and accepts only an app-bound, short-lived signed proof across Rust, Python, and Next.js."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_tenant_override_contract(index, root)
    }
}

pub fn doctor_tenant_override_contract(_index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    // ---- Source files ----
    let gw_middleware_path = root.join("example-gateway/src/auth/middleware.rs");
    let gw_principal_path = root.join("example-gateway/src/auth/principal.rs");
    let py_ops_path = root.join("example-api/example/routers/ops_console.py");
    let py_app_scope_path = root.join("example-api/example/core/app_scope.py");
    let py_runtime_agents_path = root.join("example-api/example/ops/runtime_agents.py");
    let py_onboarding_path = root.join("example-api/example/routers/admin.py");
    let nx_proxy_path = root.join("example-ops/src/app/api/gateway/[...path]/route.ts");
    let nx_auth_forwarding_path = root.join("example-ops/src/app/api/gateway/auth-forwarding.ts");
    let nx_tenant_headers_path =
        root.join("example-ops/src/app/api/gateway/tenant-context-headers.ts");
    let nx_me_path = root.join("example-ops/src/app/api/auth/me/route.ts");
    let nx_header_path = root.join("example-ops/src/components/layout/header.tsx");
    let nx_override_helper_path = root.join("example-ops/src/app/api/auth/tenant-override.ts");
    let nx_override_route_path = root.join("example-ops/src/app/api/auth/tenant-override/route.ts");
    let nx_app_scope_path = root.join("example-ops/src/lib/tenants/use-app-scope.ts");

    let gw_middleware_src = read_text(&gw_middleware_path, &mut warnings);
    let gw_principal_src = read_text(&gw_principal_path, &mut warnings);
    let py_ops_src = read_text(&py_ops_path, &mut warnings);
    let py_app_scope_src = read_text(&py_app_scope_path, &mut warnings);
    let py_runtime_agents_src = read_text(&py_runtime_agents_path, &mut warnings);
    let py_onboarding_src = read_text(&py_onboarding_path, &mut warnings);
    let nx_proxy_src = read_text(&nx_proxy_path, &mut warnings);
    let nx_auth_forwarding_src = read_text(&nx_auth_forwarding_path, &mut warnings);
    let nx_tenant_headers_src = read_text(&nx_tenant_headers_path, &mut warnings);
    let nx_me_src = read_text(&nx_me_path, &mut warnings);
    let nx_header_src = read_text(&nx_header_path, &mut warnings);
    let nx_override_helper_src = read_text(&nx_override_helper_path, &mut warnings);
    let nx_override_route_src = read_text(&nx_override_route_path, &mut warnings);
    let nx_app_scope_src = read_text(&nx_app_scope_path, &mut warnings);

    // ---- Invariant checks ----

    // 1. Raw selection headers are rejected by the gateway before a handler
    // can mistake them for authority.
    let gw_rejects_raw_override = gw_middleware_src.as_deref().is_some_and(|src| {
        src.contains("contains_key(\"x-tenant-override\")")
            && src.contains("return Err(AuthError::Forbidden)")
            && src.contains("StatusCode::FORBIDDEN")
    });

    // 2. Gateway selection is a HS256 proof bound to subject, server app,
    // auth epoch, expiry, and a live Redis application scope.
    let gw_verifies_bound_proof = gw_principal_src.as_deref().is_some_and(|src| {
        src.contains("x-tenant-override-proof")
            && src.contains("Algorithm::HS256")
            && src.contains("claims.sub != subject")
            && src.contains("claims.app != app_name")
            && src.contains("claims.auth_epoch != auth_epoch")
            && src.contains("projected_app_scope(state, &app_name)")
            && src.contains("scope.contains(&requested_tenant)")
    });

    // 3. The Python ops boundary rejects raw headers rather than accepting a
    // legacy wildcard/admin selector.
    let py_ops_rejects_raw_override = py_ops_src.as_deref().is_some_and(|src| {
        let gcu_block = function_block(src, "async def get_current_user(").unwrap_or_default();
        gcu_block.contains("request.headers.get(\"x-tenant-override\")")
            && gcu_block.contains("Raw tenant override headers are forbidden")
    });

    // 5. Python ops_console get_current_user accepts Request parameter
    let py_ops_gcu_has_request = py_ops_src.as_deref().is_some_and(|src| {
        let gcu_block = function_block(src, "async def get_current_user(").unwrap_or_default();
        gcu_block.contains("request: Request")
    });

    // 5. Python verifies the same proof claims and resolves against its live
    // app scope before applying any override (and audits the selection).
    let py_ops_verifies_bound_proof = py_ops_src.as_deref().is_some_and(|src| {
        let gcu_block = function_block(src, "async def get_current_user(").unwrap_or_default();
        gcu_block.contains("x-tenant-override-proof")
            && gcu_block.contains("_verify_tenant_override_proof")
            && gcu_block.contains("_resolve_principal_app_scope")
            && gcu_block.contains("_publish_tenant_override_audit")
    });

    // 6. Cross-tenant authority is explicit permissions, never role or tenant
    // spelling.
    let cross_tenant_uses_explicit_permissions = py_ops_src.as_deref().is_some_and(|src| {
        let helper_src =
            function_block(src, "def _has_cross_tenant_visibility").unwrap_or_default();
        helper_src.contains("permissions")
            && helper_src.contains("\"*\"")
            && helper_src.contains("\"admin:full\"")
            && !helper_src.contains("super_admin")
    });

    // 7. BFF strips every client-supplied tenant/app context header and may
    // repopulate only the signed proof recovered server-side.
    let nx_proxy_forwards_only_verified_proof = nx_proxy_src
        .as_deref()
        .is_some_and(|src| src.contains("injectTenantOverride(headers, request)"))
        && nx_auth_forwarding_src.as_deref().is_some_and(|src| {
            src.contains("stripClientTenantContextHeaders(headers)")
                && src.contains("resolveVerifiedAppTenantOverride(request.cookies)")
                && src.contains("headers.set('x-tenant-override-proof', override.proof)")
                && !src.contains("headers.set('x-tenant-override'")
        })
        && nx_tenant_headers_src.as_deref().is_some_and(|src| {
            src.contains("'x-tenant-override'")
                && src.contains("'x-tenant-override-proof'")
                && src.contains("'x-example-app'")
                && src.contains("stripClientTenantContextHeaders")
        });

    // 8. /me reports an effective tenant only after re-verifying the proof.
    let nx_me_applies_verified_override = nx_me_src.as_deref().is_some_and(|src| {
        src.contains("resolveVerifiedAppTenantOverride")
            && src.contains("profileWithEffectiveScope")
            && src.contains("override?.tenant")
            && src.contains("override?.authorizedTenants")
    });

    // 9. Header delegates switching to the server-backed hook and never
    // mints the HttpOnly proof in client JavaScript.
    let nx_header_uses_server_minted_switcher = nx_header_src.as_deref().is_some_and(|src| {
        src.contains("handleSwitchTenant")
            && src.contains("useTenantSwitch()")
            && src.contains("if (await switchTenant(id))")
            && !src.contains("document.cookie")
    });

    // 10. Server route scopes first, then mints/clears an HttpOnly proof.
    let nx_override_route_mints_scoped_proof =
        nx_override_route_src.as_deref().is_some_and(|src| {
            src.contains("verifyToken(accessToken)")
                && src.contains("isSuperAdminPayload")
                && src.contains("fetchEffectiveAppScope(backendToken)")
                && src.contains("signAppTenantOverride(accessToken, payload, tenantId)")
                && src.contains("TENANT_OVERRIDE_COOKIE")
                && src.contains("HttpOnly")
                && src.contains("TENANT_OVERRIDE_MAX_AGE_SECONDS")
        });

    // 11. The shared verifier enforces HS256 and binds the proof to server
    // app identity, principal subject, auth epoch, expiry, and live scope.
    let nx_override_helper_verifies_bound_proof =
        nx_override_helper_src.as_deref().is_some_and(|src| {
            src.contains("jwtVerify(proof, overrideSecret()")
                && src.contains("algorithms: [\"HS256\"]")
                && src.contains("requiredClaims: [\"sub\", \"app\", \"tenant\", \"iat\", \"exp\", \"auth_epoch\"]")
                && src.contains("payload.app !== configuredAppName()")
                && src.contains("payload.sub !== principal.userId")
                && src.contains("payload.auth_epoch !== authEpoch")
                && src.contains("fetchEffectiveAppScope(backendAccessToken, proof)")
                && src.contains("verifyToken(accessToken)")
                && src.contains("isSuperAdminPayload(principal)")
        });

    // 13. GymOps tenant membership is resolved from the same Redis projection
    // in Python and Rust.  This deliberately proves a structural boundary,
    // rather than a compiled list of tenant names.
    let py_app_scope_projects_runtime_membership = py_app_scope_src.as_deref().is_some_and(|src| {
        src.contains("def app_scope_key(app_name: str) -> str:")
            && src.contains("config:app:{normalize_app_name(app_name)}:tenants")
            && src.contains("def project_app_scope(")
            && src.contains("def resolve_effective_app_scope(")
    });
    let gw_app_scope_reads_runtime_membership = gw_principal_src.as_deref().is_some_and(|src| {
        src.contains("async fn projected_app_scope(")
            && src.contains("config:app:{app_name}:tenants")
            && src.contains(".smembers(&key)")
            && src.contains("normalize_projected_tenants(tenants)")
    });
    let runtime_agents_are_tenant_scoped = py_runtime_agents_src.as_deref().is_some_and(|src| {
        function_block(src, "def collect_runtime_agents(").is_some_and(|body| {
            body.contains("allowed_tenants")
                && body.contains("_indexed_agent_names(redis_client, tenant)")
                && !body.contains("phone_route:*")
                && !body.contains("whatsapp:conversation:*")
                && !body.contains("scan_iter(")
        })
    });
    let runtime_agents_route_uses_effective_scope = py_ops_src.as_deref().is_some_and(|src| {
        function_block(src, "async def list_runtime_agents(").is_some_and(|body| {
            body.contains("app_scope = user.get(\"app_scope\")")
                && body.contains("app_scope.authorized_tenants")
                && body.contains("collect_runtime_agents_async(")
        })
    });
    let onboarding_projects_app_membership = py_onboarding_src.as_deref().is_some_and(|src| {
        src.contains("AppRegistryCRUD.add_namespace(app_name, tenant_id)")
            && src.contains("project_app_scope(redis_client, app.name)")
    });
    let ui_uses_server_confirmed_scope = nx_app_scope_src.as_deref().is_some_and(|src| {
        src.contains("queryFn: () => api.getAppScope()")
            && src.contains("runtimeAgentsQueryKey")
            && src.contains("tenant:${scope.effectiveTenant}")
            && !src.contains("NEXT_PUBLIC_GYM_OPS_TENANT_IDS")
            && !src.contains("GYM_OPS_TENANT_IDS")
    });
    let gymops_scope_is_runtime_projected = py_app_scope_projects_runtime_membership
        && gw_app_scope_reads_runtime_membership
        && runtime_agents_are_tenant_scoped
        && runtime_agents_route_uses_effective_scope
        && onboarding_projects_app_membership
        && ui_uses_server_confirmed_scope;

    // ---- Push invariants ----
    push_invariant(
        &mut entities,
        &mut evidence,
        gw_rejects_raw_override,
        "gw_rejects_raw_override",
        "Rust gateway rejects raw x-tenant-override before tenant selection",
        "example-gateway/src/auth/middleware.rs",
        &gw_middleware_src,
        "x-tenant-override",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        gw_verifies_bound_proof,
        "gw_verifies_bound_proof",
        "Rust gateway verifies app-bound HS256 proof and intersects live Redis scope",
        "example-gateway/src/auth/principal.rs",
        &gw_principal_src,
        "x-tenant-override",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        py_ops_rejects_raw_override,
        "py_ops_rejects_raw_override",
        "Python ops console rejects raw x-tenant-override",
        "example-api/example/routers/ops_console.py",
        &py_ops_src,
        "x-tenant-override",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        py_ops_gcu_has_request,
        "py_ops_gcu_has_request",
        "Python ops_console get_current_user accepts Request parameter",
        "example-api/example/routers/ops_console.py",
        &py_ops_src,
        "request: Request",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        py_ops_verifies_bound_proof,
        "py_ops_verifies_bound_proof",
        "Python ops console verifies a bound proof, live app scope, and audit event",
        "example-api/example/routers/ops_console.py",
        &py_ops_src,
        "x-tenant-override-proof",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        cross_tenant_uses_explicit_permissions,
        "cross_tenant_uses_explicit_permissions",
        "Python cross-tenant authority uses explicit permissions rather than roles or tenant names",
        "example-api/example/routers/ops_console.py",
        &py_ops_src,
        "def _has_cross_tenant_visibility",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        nx_proxy_forwards_only_verified_proof,
        "nx_proxy_forwards_only_verified_proof",
        "Next.js BFF strips client tenant/app context and forwards only a server-verified proof",
        "example-ops/src/app/api/gateway/auth-forwarding.ts",
        &nx_auth_forwarding_src,
        "stripClientTenantContextHeaders(headers)",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        nx_me_applies_verified_override,
        "nx_me_applies_verified_override",
        "Next.js /api/auth/me applies only a re-verified app tenant proof",
        "example-ops/src/app/api/auth/me/route.ts",
        &nx_me_src,
        "tenant_override",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        nx_header_uses_server_minted_switcher,
        "nx_header_uses_server_minted_switcher",
        "Header delegates tenant switching to the server-backed hook and never mints a proof client-side",
        "example-ops/src/components/layout/header.tsx",
        &nx_header_src,
        "handleSwitchTenant",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        nx_override_route_mints_scoped_proof,
        "nx_override_route_mints_scoped_proof",
        "Next.js tenant-override route verifies authority, live app scope, and mints bounded HttpOnly proof",
        "example-ops/src/app/api/auth/tenant-override/route.ts",
        &nx_override_route_src,
        "tenant_override",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        nx_override_helper_verifies_bound_proof,
        "nx_override_helper_verifies_bound_proof",
        "Next.js tenant-override helper verifies HS256 subject/app/epoch/expiry proof and live app scope",
        "example-ops/src/app/api/auth/tenant-override.ts",
        &nx_override_helper_src,
        "jwtVerify(proof, overrideSecret()",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        gymops_scope_is_runtime_projected,
        "gymops_scope_is_runtime_projected",
        "GymOps membership is app_registry -> Redis projected, runtime-agent reads are effective-scope bounded, onboarding republishs membership, and the UI has no public tenant list",
        "example-api/example/core/app_scope.py",
        &py_app_scope_src,
        "def project_app_scope",
    );

    // ---- Warnings ----
    if !gw_rejects_raw_override {
        warnings.push(
            "Rust gateway must reject raw x-tenant-override before tenant selection".to_string(),
        );
    }
    if !gw_verifies_bound_proof {
        warnings.push(
            "Rust gateway tenant override must require an HS256 proof bound to subject, server app, auth epoch, expiry, and live app scope".to_string(),
        );
    }
    if !py_ops_rejects_raw_override {
        warnings.push("Python ops console must reject raw x-tenant-override headers".to_string());
    }
    if !py_ops_gcu_has_request {
        warnings.push(
            "Python ops_console get_current_user does not accept Request parameter".to_string(),
        );
    }
    if !py_ops_verifies_bound_proof {
        warnings.push(
            "Python ops console must verify a signed override proof, resolve live app scope, and publish the audit event".to_string(),
        );
    }
    if !cross_tenant_uses_explicit_permissions {
        warnings.push(
            "Python cross-tenant authority must use explicit permissions rather than roles or tenant names"
                .to_string(),
        );
    }
    if !nx_proxy_forwards_only_verified_proof {
        warnings.push(
            "Next.js BFF must strip client tenant/app headers and forward only a server-verified override proof"
                .to_string(),
        );
    }
    if !nx_me_applies_verified_override {
        warnings.push(
            "Next.js /api/auth/me must derive effective tenant only from a re-verified override proof".to_string(),
        );
    }
    if !nx_header_uses_server_minted_switcher {
        warnings
            .push("Header component is missing the server-minted tenant switcher flow".to_string());
    }
    if !nx_override_route_mints_scoped_proof {
        warnings.push(
            "Next.js tenant-override route must verify authority and live app scope before minting a bounded HttpOnly proof"
                .to_string(),
        );
    }
    if !nx_override_helper_verifies_bound_proof {
        warnings.push(
            "Next.js tenant-override helper must verify the HS256 app/subject/epoch/expiry proof and live scope before proxy forwarding"
                .to_string(),
        );
    }
    if !gymops_scope_is_runtime_projected {
        warnings.push(
            "GymOps tenant scope must use config:app:{app}:tenants across Python and Rust; runtime-agent collection must stay tenant-scoped, onboarding must republish membership, and the UI must not publish a compiled tenant list"
                .to_string(),
        );
    }

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_tenant_override_contract"),
        kind: "doctor".to_string(),
        summary: format!(
            "tenant override contract: {} warnings across {} invariants",
            warnings.len(),
            entities.len()
        ),
        confidence: if warnings.is_empty() { 0.98 } else { 0.65 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "doctor": "tenant-override-contract",
            "header": "x-tenant-override",
            "cookie": "tenant_override",
            "layers": ["rust-gateway", "python-api", "nextjs-proxy", "nextjs-ui"],
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn function_block<'a>(src: &'a str, signature: &str) -> Option<&'a str> {
    let start = src.find(signature)?;
    let remainder = &src[start..];
    let end = remainder
        .find("\ndef ")
        .or_else(|| remainder.find("\nasync def "))
        .or_else(|| {
            remainder.find(
                "\n# =============================================================================",
            )
        })
        .unwrap_or(remainder.len());
    Some(&remainder[..end])
}

#[allow(clippy::too_many_arguments)]
fn push_invariant(
    entities: &mut Vec<serde_json::Value>,
    evidence: &mut Vec<EvidenceItem>,
    passed: bool,
    name: &str,
    detail: &str,
    path: &str,
    src: &Option<String>,
    needle: &str,
) {
    entities.push(json!({
        "name": name,
        "passed": passed,
        "detail": detail,
    }));

    if passed {
        evidence.push(EvidenceItem {
            kind: "tenant_override_contract".to_string(),
            path: path.to_string(),
            line: src
                .as_deref()
                .and_then(|content| find_line(content, needle)),
            detail: detail.to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn empty_index() -> RepoIndex {
        RepoIndex {
            version: 0,
            root: String::new(),
            indexed_at: String::new(),
            files: Vec::new(),
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        }
    }

    fn write(root: &Path, relative: &str, content: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("fixture parent")).expect("fixture directory");
        fs::write(path, content).expect("fixture source");
    }

    #[test]
    fn reports_the_config_backed_gymops_scope_boundary() {
        let fixture = TempDir::new().expect("fixture");
        let root = fixture.path();

        write(
            root,
            "example-api/example/core/app_scope.py",
            r#"
def app_scope_key(app_name: str) -> str:
    return f"config:app:{normalize_app_name(app_name)}:tenants"
def project_app_scope(redis_client, app_name):
    redis_client.sadd(app_scope_key(app_name), "tenant")
def resolve_effective_app_scope(redis_client, *, app_name, home_tenant, permissions, requested_tenant):
    return load_projected_app_scope(redis_client, app_name)
"#,
        );
        write(
            root,
            "example-gateway/src/auth/principal.rs",
            r#"
pub(crate) async fn projected_app_scope() {
    let key = format!("config:app:{app_name}:tenants");
    let tenants: Vec<String> = redis.smembers(&key).await?;
    normalize_projected_tenants(tenants)
}
"#,
        );
        write(
            root,
            "example-api/example/ops/runtime_agents.py",
            r#"
def collect_runtime_agents(redis_client, allowed_tenants):
    for tenant in allowed_tenants:
        _indexed_agent_names(redis_client, tenant)
    return []
"#,
        );
        write(
            root,
            "example-api/example/routers/ops_console.py",
            r#"
async def list_runtime_agents(request, tenant=None, user=None):
    app_scope = user.get("app_scope")
    allowed_tenants = app_scope.authorized_tenants
    return await collect_runtime_agents_async(redis_client, allowed_tenants)
"#,
        );
        write(
            root,
            "example-api/example/routers/admin.py",
            r#"
async def create_onboarding():
    app = AppRegistryCRUD.add_namespace(app_name, tenant_id)
    project_app_scope(redis_client, app.name)
"#,
        );
        write(
            root,
            "example-ops/src/lib/tenants/use-app-scope.ts",
            r#"
export function runtimeAgentsQueryKey(scope) {
  const dimension = scope.effectiveTenant ? `tenant:${scope.effectiveTenant}` : `aggregate:${scope.application}`;
  return ["runtime-agents", dimension];
}
export function useAppScope() { return { queryFn: () => api.getAppScope() }; }
"#,
        );

        let result = doctor_tenant_override_contract(&empty_index(), root);
        let invariant = result.entities.iter().find(|entity| {
            entity.get("name").and_then(|value| value.as_str())
                == Some("gymops_scope_is_runtime_projected")
        });

        assert_eq!(
            invariant
                .and_then(|entity| entity.get("passed"))
                .and_then(|value| value.as_bool()),
            Some(true),
            "the doctor must prove the GymOps boundary uses the app projection rather than a compiled/public tenant list"
        );
    }
}
