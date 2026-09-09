//! Operator cross-tenant parity between the Python control plane and the Rust
//! gateway realtime plane.
//!
//! The ops console has two planes deciding what an operator sees: the Python
//! polling endpoints (`ops_console.py`) and the gateway realtime SSE stream
//! (`helpers/assignment.rs`). Cross-tenant authority must come from the same
//! explicit JWT permissions (`*` or `admin:full`) in both planes. Tenant and
//! role names are not authority; privileged roles are projected onto explicit
//! permissions when Python issues a token.
// Rust guideline compliant 2026-02-21

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct OperatorTenantParityDoctor;

impl Doctor for OperatorTenantParityDoctor {
    fn name(&self) -> &'static str {
        "operator-tenant-parity"
    }

    fn description(&self) -> &'static str {
        "Checks Python polling and Rust realtime SSE require the same explicit cross-tenant JWT permissions, without inferring authority from tenant or role names."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_operator_tenant_parity(index, root)
    }
}

fn python_function<'a>(src: &'a str, name: &str) -> Option<&'a str> {
    let signature = format!("def {name}(");
    let start = src.find(&signature)?;
    let rest = &src[start..];
    let end = rest[1..]
        .find("\ndef ")
        .map_or(rest.len(), |offset| offset + 1);
    Some(&rest[..end])
}

fn rust_function<'a>(src: &'a str, name: &str) -> Option<&'a str> {
    let signature = format!("fn {name}(");
    let start = src.find(&signature)?;
    let rest = &src[start..];
    let open = rest.find('{')?;
    let mut depth = 0usize;
    for (offset, ch) in rest[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(&rest[..open + offset + ch.len_utf8()]);
                }
            }
            _ => {}
        }
    }
    None
}

fn typescript_function<'a>(src: &'a str, name: &str) -> Option<&'a str> {
    let signature = format!("function {name}(");
    let start = src.find(&signature)?;
    let rest = &src[start..];
    let open = rest.find('{')?;
    let mut depth = 0usize;
    for (offset, ch) in rest[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(&rest[..open + offset + ch.len_utf8()]);
                }
            }
            _ => {}
        }
    }
    None
}

pub fn doctor_operator_tenant_parity(_index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let py_ops_path = root.join("example-api/example/routers/ops_console.py");
    let py_jwt_path = root.join("example-api/example/auth/jwt.py");
    let gw_assign_path = root.join("example-gateway/src/ops_console/routes/helpers/assignment.rs");
    let gw_sse_path = root.join("example-gateway/src/ops_console/routes/sse_routes.rs");
    let gw_auth_mod_path = root.join("example-gateway/src/auth/mod.rs");
    let gw_middleware_path = root.join("example-gateway/src/auth/middleware.rs");
    let gw_principal_path = root.join("example-gateway/src/auth/principal.rs");
    let gw_secure_store_path = root.join("example-gateway/src/auth/secure_store.rs");
    let gw_ops_auth_path = root.join("example-gateway/src/ops_console/routes/auth.rs");
    let ui_authority_path = root.join("example-ops/src/lib/tenants/superadmin.ts");
    let ui_superadmin_guard_path = root.join("example-ops/src/lib/tenants/superadmin-guard.ts");
    let ui_tenant_override_path = root.join("example-ops/src/app/api/auth/tenant-override.ts");
    let ui_business_pulse_path = root.join("example-ops/src/app/api/business-pulse/route.ts");
    let ui_fitness_guard_path = root.join("example-ops/src/lib/tenants/fitness-guard.ts");
    let ui_revops_proxy_path = root.join("example-ops/src/app/api/revops/[...path]/route.ts");
    let ui_revops_gate_path = root.join("example-ops/src/lib/revops/tenant-gate.ts");

    let py_ops_src = read_text(&py_ops_path, &mut warnings);
    let py_jwt_src = read_text(&py_jwt_path, &mut warnings);
    let gw_assign_src = read_text(&gw_assign_path, &mut warnings);
    let gw_sse_src = read_text(&gw_sse_path, &mut warnings);
    let gw_auth_mod_src = read_text(&gw_auth_mod_path, &mut warnings);
    let gw_middleware_src = read_text(&gw_middleware_path, &mut warnings);
    let gw_principal_src = read_text(&gw_principal_path, &mut warnings);
    let gw_secure_store_src = read_text(&gw_secure_store_path, &mut warnings);
    let gw_ops_auth_src = read_text(&gw_ops_auth_path, &mut warnings);
    let ui_authority_src = read_text(&ui_authority_path, &mut warnings);
    let ui_superadmin_guard_src = read_text(&ui_superadmin_guard_path, &mut warnings);
    let ui_tenant_override_src = read_text(&ui_tenant_override_path, &mut warnings);
    let ui_business_pulse_src = read_text(&ui_business_pulse_path, &mut warnings);
    let ui_fitness_guard_src = read_text(&ui_fitness_guard_path, &mut warnings);
    let ui_revops_proxy_src = read_text(&ui_revops_proxy_path, &mut warnings);
    let ui_revops_gate_src = read_text(&ui_revops_gate_path, &mut warnings);

    let py_operator_fn = py_ops_src
        .as_deref()
        .and_then(|src| python_function(src, "_has_cross_tenant_visibility"));
    let py_explicit_authority = py_operator_fn.is_some_and(|body| {
        body.contains("permissions") && body.contains("\"*\"") && body.contains("\"admin:full\"")
    });
    let py_no_named_authority = py_operator_fn.is_some_and(|body| {
        !body.contains("extract_tenant_id")
            && !body.contains("user.get(\"tenant_id\"")
            && !body.contains("user.get(\"namespace\"")
            && !body.contains("user.get(\"role\"")
            && !body.contains("user.get(\"roles\"")
            && !body.contains("_OPERATOR_TENANTS")
            && !body.contains("OPS_OPERATOR_TENANTS")
            && !body.contains("\"jai\"")
            && !body.contains("\"getjai\"")
    }) && py_ops_src.as_deref().is_some_and(|src| {
        !src.contains("_OPERATOR_TENANTS") && !src.contains("OPS_OPERATOR_TENANTS")
    });
    let role_projects_authority = py_jwt_src.as_deref().is_some_and(|src| {
        python_function(src, "_permissions_for_role").is_some_and(|body| {
            body.contains("super_admin") && body.contains("superadmin") && body.contains("[\"*\"]")
        })
    });

    let gw_operator_fn = gw_assign_src
        .as_deref()
        .and_then(|src| rust_function(src, "is_cross_tenant_operator"));
    let gw_explicit_authority = gw_operator_fn.is_some_and(|body| {
        body.contains("principal.permissions")
            && body.contains("\"*\"")
            && body.contains("\"admin:full\"")
    });
    let gw_no_named_authority = gw_operator_fn.is_some_and(|body| {
        !body.contains("principal.tenant_id")
            && !body.contains("principal.roles")
            && !body.contains("operator_tenants")
            && !body.contains("OPS_OPERATOR_TENANTS")
            && !body.contains("\"jai\"")
            && !body.contains("\"getjai\"")
    });
    let sse_applies = gw_sse_src
        .as_deref()
        .is_some_and(|s| s.contains("is_cross_tenant_operator"));
    let py_wildcard_boundary = py_ops_src.as_deref().is_some_and(|src| {
        python_function(src, "get_current_user").is_some_and(|body| {
            body.contains("_has_cross_tenant_visibility")
                && body.contains("tenant_id == \"*\"")
                && body.contains("not can_override_tenant")
                && body.contains("HTTP_403_FORBIDDEN")
                && !body.contains("is_super =")
        }) && python_function(src, "require_ops_role")
            .is_some_and(|body| !body.contains("tenant_id == \"*\""))
    });
    let gw_wildcard_boundary = gw_auth_mod_src.as_deref().is_some_and(|src| {
        rust_function(src, "has_cross_tenant_permission").is_some_and(|body| {
            body.contains("permissions")
                && body.contains("\"*\"")
                && body.contains("\"admin:full\"")
        })
    }) && gw_middleware_src.as_deref().is_some_and(|src| {
        src.matches("!has_cross_tenant_permission").count() >= 2
            && src.contains("return Err(AuthError::Unauthorized)")
            && src.contains("return next.run(request).await")
    }) && gw_principal_src.as_deref().is_some_and(|src| {
        src.contains("!can_override_tenant") && src.contains("return Err(AuthError::Unauthorized)")
    });
    let gw_issuer_projects_authority = gw_secure_store_src.as_deref().is_some_and(|src| {
        rust_function(src, "permissions_for_roles").is_some_and(|body| {
            body.contains("super_admin") && body.contains("permissions.push(\"*\"")
        })
    }) && gw_ops_auth_src.as_deref().is_some_and(|src| {
        rust_function(src, "get_permissions_for_roles").is_some_and(|body| {
            body.contains("super_admin") && body.contains("result.insert(\"*\"")
        })
    });
    let ui_authority_fn = ui_authority_src
        .as_deref()
        .and_then(|src| typescript_function(src, "hasCrossTenantAuthority"));
    let ui_explicit_authority = ui_authority_fn.is_some_and(|body| {
        body.contains("permissions")
            && body.contains("normalized.has(\"*\")")
            && body.contains("normalized.has(\"admin:full\")")
            && !body.contains("tenantId")
            && !body.contains("roles")
            && !body.contains("\"jai\"")
            && !body.contains("\"getjai\"")
    });
    let ui_global_gates = ui_superadmin_guard_src
        .as_deref()
        .is_some_and(|src| src.contains("hasCrossTenantAuthority(payload.permissions)"))
        && ui_tenant_override_src
            .as_deref()
            .is_some_and(|src| src.contains("hasCrossTenantAuthority(payload.permissions)"))
        && ui_business_pulse_src.as_deref().is_some_and(|src| {
            src.contains("hasCrossTenantAuthority") && src.contains("payload.permissions")
        });
    let ui_scoped_data_gates = ui_fitness_guard_src
        .as_deref()
        .is_some_and(|src| src.contains("canAccessFitnessTenant(tenant, payload?.permissions)"))
        && ui_revops_proxy_src
            .as_deref()
            .is_some_and(|src| src.contains("denyNonFitnessViewer(request)"))
        && ui_revops_gate_src.as_deref().is_some_and(|src| {
            src.contains("REVOPS_TENANTS.has(scope.tenantId)")
                && src.contains("hasCrossTenantAuthority(scope.permissions)")
                && !src.contains("\"jai\"")
                && !src.contains("\"getjai\"")
        });

    // ---- Push invariants ----
    push_invariant(
        &mut entities,
        &mut evidence,
        py_explicit_authority,
        "py_explicit_authority",
        "Python polling requires explicit * or admin:full permission",
        "example-api/example/routers/ops_console.py",
        &py_ops_src,
        "def _has_cross_tenant_visibility",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        py_no_named_authority,
        "py_no_named_authority",
        "Python polling does not infer cross-tenant authority from tenant or role names",
        "example-api/example/routers/ops_console.py",
        &py_ops_src,
        "def _has_cross_tenant_visibility",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        role_projects_authority,
        "role_projects_explicit_authority",
        "Python JWT issuance projects super_admin roles onto explicit wildcard authority",
        "example-api/example/auth/jwt.py",
        &py_jwt_src,
        "def _permissions_for_role",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        gw_explicit_authority,
        "gw_explicit_authority",
        "Gateway realtime requires explicit * or admin:full permission",
        "example-gateway/src/ops_console/routes/helpers/assignment.rs",
        &gw_assign_src,
        "fn is_cross_tenant_operator",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        gw_no_named_authority,
        "gw_no_named_authority",
        "Gateway realtime does not infer cross-tenant authority from tenant or role names",
        "example-gateway/src/ops_console/routes/helpers/assignment.rs",
        &gw_assign_src,
        "fn is_cross_tenant_operator",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        sse_applies,
        "sse_applies_operator_check",
        "Gateway SSE handler grants cross-tenant visibility via is_cross_tenant_operator",
        "example-gateway/src/ops_console/routes/sse_routes.rs",
        &gw_sse_src,
        "is_cross_tenant_operator",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        py_wildcard_boundary,
        "py_wildcard_boundary",
        "Python rejects wildcard-bound JWTs without explicit cross-tenant permission and never grants ops access from tenant name alone",
        "example-api/example/routers/ops_console.py",
        &py_ops_src,
        "Wildcard tenant requires explicit cross-tenant permission",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        gw_wildcard_boundary,
        "gw_wildcard_boundary",
        "Gateway cookie and bearer boundaries reject wildcard-bound JWTs without explicit cross-tenant permission",
        "example-gateway/src/auth/middleware.rs",
        &gw_middleware_src,
        "Rejecting wildcard tenant without explicit cross-tenant permission",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        gw_issuer_projects_authority,
        "gw_issuer_projects_explicit_authority",
        "Gateway native super_admin issuance projects explicit wildcard permission authority",
        "example-gateway/src/auth/secure_store.rs",
        &gw_secure_store_src,
        "permissions.push(\"*\"",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        ui_explicit_authority,
        "ui_explicit_authority",
        "Example Ops uses a client-safe permission-only cross-tenant authority helper",
        "example-ops/src/lib/tenants/superadmin.ts",
        &ui_authority_src,
        "function hasCrossTenantAuthority",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        ui_global_gates,
        "ui_global_gates",
        "Example Ops global monitor, tenant override, and business pulse gates use explicit permissions",
        "example-ops/src/lib/tenants/superadmin-guard.ts",
        &ui_superadmin_guard_src,
        "hasCrossTenantAuthority(payload.permissions)",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        ui_scoped_data_gates,
        "ui_scoped_data_gates",
        "Example Ops fitness and RevOps data gates require ownership or explicit cross-tenant permission",
        "example-ops/src/lib/tenants/fitness-guard.ts",
        &ui_fitness_guard_src,
        "canAccessFitnessTenant(tenant, payload?.permissions)",
    );

    // ---- Warnings ----
    if !py_explicit_authority {
        warnings.push("Python ops_console must require explicit * or admin:full permission for cross-tenant visibility".to_string());
    }
    if !py_no_named_authority {
        warnings.push("Python ops_console infers cross-tenant authority from tenant names or role names; only explicit permissions may grant it".to_string());
    }
    if !role_projects_authority {
        warnings.push("Python JWT issuance must project super_admin roles onto explicit wildcard permission authority".to_string());
    }
    if !gw_explicit_authority {
        warnings.push(
            "Gateway is_cross_tenant_operator must require explicit * or admin:full permission"
                .to_string(),
        );
    }
    if !gw_no_named_authority {
        warnings.push("Gateway is_cross_tenant_operator infers authority from tenant or role names; only explicit permissions may grant it".to_string());
    }
    if !sse_applies {
        warnings.push(
            "Gateway SSE handler no longer applies is_cross_tenant_operator; privileged operators will only see their bound tenant's chats in realtime"
                .to_string(),
        );
    }
    if !py_wildcard_boundary {
        warnings.push("Python auth boundary must reject wildcard-bound JWTs without * or admin:full and require_ops_role must not infer authority from tenant_id".to_string());
    }
    if !gw_wildcard_boundary {
        warnings.push("Gateway cookie, optional-cookie, and bearer boundaries must reject wildcard-bound JWTs without * or admin:full".to_string());
    }
    if !gw_issuer_projects_authority {
        warnings.push("Gateway native super_admin issuance must project explicit wildcard permission authority".to_string());
    }
    if !ui_explicit_authority {
        warnings.push(
            "Example Ops cross-tenant helper must use only explicit * or admin:full permissions"
                .to_string(),
        );
    }
    if !ui_global_gates {
        warnings.push("Example Ops global monitor, tenant override, and business pulse gates must use the explicit permission helper".to_string());
    }
    if !ui_scoped_data_gates {
        warnings.push("Example Ops fitness and RevOps proxies/pages must enforce data ownership or explicit cross-tenant permission".to_string());
    }

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_operator_tenant_parity"),
        kind: "doctor".to_string(),
        summary: format!(
            "operator tenant parity: {} warnings across {} invariants",
            warnings.len(),
            entities.len()
        ),
        confidence: if warnings.is_empty() { 0.98 } else { 0.65 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "doctor": "operator-tenant-parity",
            "planes": ["python-ops-console", "rust-gateway-auth-and-sse", "nextjs-bff-and-ui"],
            "explicit_permissions": ["*", "admin:full"],
            "role_projection": "super_admin -> *",
        })),
        timing_ms: started.elapsed().as_millis(),
    }
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
            kind: "operator_tenant_parity".to_string(),
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

    fn write_fixture(root: &Path, relative: &str, body: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("fixture parent")).expect("create fixture parent");
        fs::write(path, body).expect("write fixture");
    }

    fn write_explicit_authority_fixtures(root: &Path) {
        write_fixture(
            root,
            "example-api/example/routers/ops_console.py",
            r#"
def _has_cross_tenant_visibility(user):
    permissions = {str(p).strip().lower() for p in user.get("permissions", []) if p}
    return "*" in permissions or "admin:full" in permissions

def get_current_user(request, authorization):
    tenant_id = "*"
    can_override_tenant = _has_cross_tenant_visibility({"permissions": permissions})
    if tenant_id == "*" and not can_override_tenant:
        raise HTTPException(status_code=status.HTTP_403_FORBIDDEN, detail="Wildcard tenant requires explicit cross-tenant permission")

def require_ops_role(user):
    permissions = set(user.get("permissions", []))
    if "*" in permissions or "admin:full" in permissions:
        return user
"#,
        );
        write_fixture(
            root,
            "example-api/example/auth/jwt.py",
            r#"
def _permissions_for_role(role):
    normalized = role.strip().lower().replace("-", "_")
    if normalized in {"super_admin", "superadmin"}:
        return _permissions_for_role("admin") + ["*"]
    return ["config:read"]
"#,
        );
        write_fixture(
            root,
            "example-gateway/src/ops_console/routes/helpers/assignment.rs",
            r#"
pub fn is_cross_tenant_operator(principal: &TenantPrincipal) -> bool {
    principal.permissions.iter().any(|p| {
        let p = p.trim().to_ascii_lowercase();
        p == "*" || p == "admin:full"
    })
}
"#,
        );
        write_fixture(
            root,
            "example-gateway/src/ops_console/routes/sse_routes.rs",
            "let is_operator = super::helpers::assignment::is_cross_tenant_operator(&principal);",
        );
        write_fixture(
            root,
            "example-gateway/src/auth/mod.rs",
            r#"
pub(crate) fn has_cross_tenant_permission(permissions: &[String]) -> bool {
    permissions.iter().any(|permission| permission == "*" || permission == "admin:full")
}
"#,
        );
        write_fixture(
            root,
            "example-gateway/src/auth/middleware.rs",
            r#"
if user.tenant_id == "*" && !has_cross_tenant_permission(&user.permissions) {
    return Err(AuthError::Unauthorized);
}
if user.tenant_id == "*" && !has_cross_tenant_permission(&user.permissions) {
    return next.run(request).await;
}
"#,
        );
        write_fixture(
            root,
            "example-gateway/src/auth/principal.rs",
            r#"
let can_override_tenant = has_cross_tenant_permission(&claims.permissions);
if tenant_id == "*" && !can_override_tenant {
    return Err(AuthError::Unauthorized);
}
"#,
        );
        write_fixture(
            root,
            "example-gateway/src/auth/secure_store.rs",
            r#"
fn permissions_for_roles(roles: &[String]) -> Vec<String> {
    let mut permissions = Vec::new();
    if roles.iter().any(|role| role == "super_admin") {
        permissions.push("*".to_string());
    }
    permissions
}
"#,
        );
        write_fixture(
            root,
            "example-gateway/src/ops_console/routes/auth.rs",
            r#"
fn get_permissions_for_roles(roles: &[String]) -> Vec<String> {
    let mut result = std::collections::HashSet::new();
    if roles.iter().any(|role| role == "super_admin") {
        result.insert("*".to_string());
    }
    result.into_iter().collect()
}
"#,
        );
        write_fixture(
            root,
            "example-ops/src/lib/tenants/superadmin.ts",
            r#"
export function hasCrossTenantAuthority(permissions: readonly string[]): boolean {
  const normalized = new Set(permissions);
  return normalized.has("*") || normalized.has("admin:full");
}
"#,
        );
        write_fixture(
            root,
            "example-ops/src/lib/tenants/superadmin-guard.ts",
            "return hasCrossTenantAuthority(payload.permissions);",
        );
        write_fixture(
            root,
            "example-ops/src/app/api/auth/tenant-override.ts",
            "return hasCrossTenantAuthority(payload.permissions);",
        );
        write_fixture(
            root,
            "example-ops/src/app/api/business-pulse/route.ts",
            "const isSuperAdmin = hasCrossTenantAuthority(payload.permissions);",
        );
        write_fixture(
            root,
            "example-ops/src/lib/tenants/fitness-guard.ts",
            "if (!canAccessFitnessTenant(tenant, payload?.permissions)) return denied;",
        );
        write_fixture(
            root,
            "example-ops/src/app/api/revops/[...path]/route.ts",
            "const denied = await denyNonFitnessViewer(request);",
        );
        write_fixture(
            root,
            "example-ops/src/lib/revops/tenant-gate.ts",
            r#"
const REVOPS_TENANTS = new Set(["fitness_exclusive"]);
return REVOPS_TENANTS.has(scope.tenantId) || hasCrossTenantAuthority(scope.permissions);
"#,
        );
    }

    #[test]
    fn passes_for_explicit_permission_and_role_projection_contract() {
        let tmp = TempDir::new().expect("tempdir");
        write_explicit_authority_fixtures(tmp.path());

        let envelope = doctor_operator_tenant_parity(&empty_index(), tmp.path());

        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
        assert_eq!(envelope.entities.len(), 12);
    }

    #[test]
    fn flags_python_tenant_name_as_authority() {
        let tmp = TempDir::new().expect("tempdir");
        write_explicit_authority_fixtures(tmp.path());
        write_fixture(
            tmp.path(),
            "example-api/example/routers/ops_console.py",
            r#"
_OPERATOR_TENANTS = {"jai", "getjai", "*"}

def _has_cross_tenant_visibility(user):
    permissions = set(user.get("permissions", []))
    auth_tenant = user.get("tenant_id")
    return auth_tenant in _OPERATOR_TENANTS or "*" in permissions or "admin:full" in permissions
"#,
        );

        let envelope = doctor_operator_tenant_parity(&empty_index(), tmp.path());

        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("tenant name")),
            "{:?}",
            envelope.warnings
        );
    }

    #[test]
    fn flags_gateway_tenant_or_role_inference() {
        let tmp = TempDir::new().expect("tempdir");
        write_explicit_authority_fixtures(tmp.path());
        write_fixture(
            tmp.path(),
            "example-gateway/src/ops_console/routes/helpers/assignment.rs",
            r#"
pub fn is_cross_tenant_operator(principal: &TenantPrincipal) -> bool {
    principal.tenant_id == "getjai"
        || principal.roles.iter().any(|role| role == "super_admin")
        || principal.permissions.iter().any(|p| p == "*" || p == "admin:full")
}
"#,
        );

        let envelope = doctor_operator_tenant_parity(&empty_index(), tmp.path());

        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("tenant or role names")),
            "{:?}",
            envelope.warnings
        );
    }

    #[test]
    fn flags_ui_tenant_name_as_authority() {
        let tmp = TempDir::new().expect("tempdir");
        write_explicit_authority_fixtures(tmp.path());
        write_fixture(
            tmp.path(),
            "example-ops/src/lib/tenants/superadmin.ts",
            r#"
export function hasCrossTenantAuthority(permissions: readonly string[], tenantId: string): boolean {
  const normalized = new Set(permissions);
  return tenantId === "getjai" || normalized.has("*") || normalized.has("admin:full");
}
"#,
        );

        let envelope = doctor_operator_tenant_parity(&empty_index(), tmp.path());

        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("Example Ops cross-tenant helper")),
            "{:?}",
            envelope.warnings
        );
    }
}
