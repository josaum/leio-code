use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

/// Read text without emitting a missing-file warning.
fn read_text_silent(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn has_env_placeholder(source: &str, name: &str) -> bool {
    source.lines().any(|line| {
        line.trim()
            .strip_prefix(name)
            .is_some_and(|suffix| suffix.starts_with('='))
    })
}

fn quoted_literals(source: &str) -> Vec<String> {
    let mut literals = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    let mut current = String::new();
    for ch in source.chars() {
        match quote {
            Some(_) if escaped => {
                current.push(ch);
                escaped = false;
            }
            Some(_) if ch == '\\' => escaped = true,
            Some(active_quote) if ch == active_quote => {
                literals.push(std::mem::take(&mut current));
                quote = None;
            }
            Some(_) => current.push(ch),
            None if ch == '\'' || ch == '"' => quote = Some(ch),
            None => {}
        }
    }
    literals
}

fn health_audit_public_paths(source: &str) -> Option<Vec<String>> {
    let mapping = &source[source.find("_PUBLIC_CARTRIDGE_PATHS")?..];
    let key_offset = mapping
        .find("\"health_audit\"")
        .or_else(|| mapping.find("'health_audit'"))?;
    let after_key = &mapping[key_offset..];
    let call = &after_key[after_key.find("frozenset(")?..];
    let call = &call[..=call.find(')')?];
    Some(quoted_literals(call))
}

/// Health-audit auth single-store contract.
///
/// The Rust gateway is the single source of truth for health-audit users (the
/// `auth_users` DuckDB store): it seeds the `ha_*` roles + `ha:*` permissions
/// and provisions the admin at boot, and the console authenticates
/// gateway-first. The central cartridge mount injects auth before FastAPI
/// recompiles routes, `/status` is the only public health-audit route, and
/// required compose secrets stay in the canonical secret-set template. See
/// memory/feedback_rust_owns_auth.md.
pub struct HealthAuditAuthDoctor;

impl Doctor for HealthAuditAuthDoctor {
    fn name(&self) -> &'static str {
        "health-audit-auth"
    }

    fn description(&self) -> &'static str {
        "Verifies health-audit auth ownership, central route protection, public readiness, and deploy-secret parity."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_health_audit_auth(index, root)
    }
}

const HA_ROLES: [&str; 5] = [
    "ha_admin",
    "ha_auditor",
    "ha_rules",
    "ha_viewer",
    "ha_stamp",
];
const GATEWAY_SECURE_AUTH_STORE_REL: &str = "example-gateway/src/auth/secure_store.rs";
const PROFILE_REL: &str = "deploy/profiles/health_audit.env";
const SECRET_SET_REL: &str = "deploy/secret-sets/hospital_audit.env.example";
const COMPOSE_REL: &str = "example-api/docker-compose.health-audit.yml";
const CARTRIDGE_MOUNT_REL: &str = "example-api/example/cartridges/__init__.py";
const API_KEY_DEPENDENCY_REL: &str = "example-api/example/auth/dependencies.py";
const API_KEY_ROLES_REL: &str = "example-api/example/auth/roles.py";
const API_KEY_ROUTER_REL: &str = "example-api/example/routers/auth.py";
const HEALTH_AUDIT_ROUTER_REL: &str = "cartridges/health_audit/router.py";
const HEALTH_AUDIT_AUTHZ_REL: &str = "cartridges/health_audit/authz.py";
const HEALTH_AUDIT_RUNS_REL: &str = "cartridges/health_audit/routes/audit_runs.py";
const HEALTH_AUDIT_XML_QUEUE_REL: &str = "cartridges/health_audit/services/xml_glosa_queue.py";
const HEALTH_AUDIT_APPLIANCE_REL: &str = "cartridges/health_audit/appliance_app.py";
const HEALTH_AUDIT_APPLIANCE_COMPOSE_REL: &str = "deploy/stamp/docker-compose.appliance.yml";

pub fn doctor_health_audit_auth(_index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();

    // 1. Gateway owns the role/permission definitions + admin provisioning.
    let seed_path = root.join("example-gateway/src/auth/seed.rs");
    match read_text_silent(&seed_path) {
        Some(src) => {
            let missing: Vec<&str> = HA_ROLES
                .iter()
                .copied()
                .filter(|r| !src.contains(*r))
                .collect();
            if !missing.is_empty() {
                warnings.push(format!(
                    "gateway auth seed is missing role(s) {missing:?}; the health-audit role set has drifted"
                ));
            }
            if !src.contains("provision_health_audit_admin") {
                warnings.push(
                    "gateway auth seed no longer exposes provision_health_audit_admin; single-store admin provisioning is gone".to_string(),
                );
            }
            if !src.contains("ha:") {
                warnings.push(
                    "gateway auth seed defines no ha:* permissions; the claim set has drifted".to_string(),
                );
            }
            let stamp_seed_anchors = [
                "'perm-ha-stamp-run',       'ha:stamp:run'",
                "('role-ha-stamp', 'perm-ha-stamp-run')",
                "permission_id <> 'perm-ha-stamp-run'",
            ];
            let missing_stamp_seed_anchors: Vec<&str> = stamp_seed_anchors
                .iter()
                .copied()
                .filter(|anchor| !src.contains(anchor))
                .collect();
            if missing_stamp_seed_anchors.is_empty() {
                evidence.push(EvidenceItem {
                    kind: "health-audit-stamp-seed-least-privilege".to_string(),
                    path: seed_path.display().to_string(),
                    line: find_line(&src, "perm-ha-stamp-run"),
                    detail: "gateway seed grants ha_stamp only ha:stamp:run and removes the legacy broad audit grant".to_string(),
                });
            } else {
                warnings.push(format!(
                    "gateway auth seed must converge ha_stamp to only ha:stamp:run and remove the legacy ha:audit:run grant; missing anchor(s) {missing_stamp_seed_anchors:?}"
                ));
            }
            if let Some(line) = find_line(&src, "apply_health_audit_auth_roles") {
                evidence.push(EvidenceItem {
                    kind: "gateway-auth-seed".to_string(),
                    path: seed_path.display().to_string(),
                    line: Some(line),
                    detail: "gateway defines ha_* roles + ha:* permissions + admin provisioning"
                        .to_string(),
                });
            }
        }
        None => warnings.push(
            "example-gateway/src/auth/seed.rs is missing; the gateway no longer owns the health-audit auth store".to_string(),
        ),
    }

    // 2. Gateway applies the role seed + provisions the admin at boot.
    let main_path = root.join("example-gateway/src/main.rs");
    if let Some(src) = read_text_silent(&main_path) {
        let applies_roles = src.contains("apply_health_audit_auth_roles");
        let provisions_admin = src.contains("provision_health_audit_admin");
        let auth_seed_fails_closed = src
            .contains("return Err(anyhow::anyhow!(\"Health-audit role seed failed: {e}\"))")
            && src.contains(
                "return Err(anyhow::anyhow!(\"Health-audit role seed task error: {e}\"))",
            );
        let hardcoded_superadmin_reset = src.contains("Reset password for JAI superadmin")
            || src.contains("bcrypt::hash(\"admin\"");
        if !applies_roles || !provisions_admin {
            warnings.push(format!(
                "gateway startup does not both seed roles and provision the admin (apply_roles={applies_roles}, provision_admin={provisions_admin})"
            ));
        }
        if !auth_seed_fails_closed {
            warnings.push(
                "gateway startup must fail closed when the Health Audit auth seed cannot converge legacy role grants".to_string(),
            );
        }
        if hardcoded_superadmin_reset {
            warnings.push(
                "gateway startup hard-resets JAI superadmin passwords; this overwrites live health-audit credentials on restart".to_string(),
            );
        } else {
            evidence.push(EvidenceItem {
                kind: "no-dev-superadmin-reset".to_string(),
                path: main_path.display().to_string(),
                line: find_line(&src, "Do not mutate seeded JAI superadmin passwords"),
                detail: "gateway startup does not hard-reset JAI superadmin passwords".to_string(),
            });
        }
        if let Some(line) = find_line(&src, "provision_health_audit_admin") {
            evidence.push(EvidenceItem {
                kind: "gateway-boot-seed".to_string(),
                path: main_path.display().to_string(),
                line: Some(line),
                detail: "gateway seeds roles + provisions ha_admin on boot".to_string(),
            });
        }
    } else {
        warnings.push(
            "example-gateway/src/main.rs is missing; cannot confirm boot-time provisioning"
                .to_string(),
        );
    }

    // 3. Console authenticates gateway-first (Python is opt-out only).
    let console_path = root.join("health-audit-console/lib/authBackend.ts");
    match read_text_silent(&console_path) {
        Some(src) => {
            let gateway_default = src.contains("return [\"gateway\", \"python\"]");
            let python_opt_out = src.contains("=== \"python\"");
            if !gateway_default || !python_opt_out {
                warnings.push(format!(
                    "health-audit-console no longer defaults to gateway-first auth (gateway_default={gateway_default}, python_opt_out={python_opt_out})"
                ));
            }
            evidence.push(EvidenceItem {
                kind: "console-auth-default".to_string(),
                path: console_path.display().to_string(),
                line: find_line(&src, "return [\"gateway\""),
                detail: format!(
                    "gateway_default={gateway_default}, python_opt_out={python_opt_out}"
                ),
            });
        }
        None => warnings.push(
            "health-audit-console/lib/authBackend.ts is missing; the console auth-order contract is gone".to_string(),
        ),
    }

    // 3b. Opaque integration keys must materialize the same tenant and
    // permission claims that Health Audit route dependencies consume.
    let api_key_dependency_path = root.join(API_KEY_DEPENDENCY_REL);
    if let Some(src) = read_text_silent(&api_key_dependency_path) {
        let required_anchors = [
            "roles=[api_key.role]",
            "permissions=permissions_for_role(api_key.role)",
            "tenant_id=namespace",
            "api_key_id=api_key.id",
        ];
        let missing: Vec<&str> = required_anchors
            .iter()
            .copied()
            .filter(|anchor| !src.contains(anchor))
            .collect();
        if missing.is_empty() {
            evidence.push(EvidenceItem {
                kind: "health-audit-opaque-api-key-claims".to_string(),
                path: api_key_dependency_path.display().to_string(),
                line: find_line(&src, "permissions=permissions_for_role(api_key.role)"),
                detail: "opaque API key materializes role, permissions, namespace, tenant_id, and stable key identity".to_string(),
            });
        } else {
            warnings.push(format!(
                "opaque API key resolution must materialize permissions, tenant_id, and api_key_id for Health Audit; missing anchor(s) {missing:?}"
            ));
        }
    } else {
        warnings.push(format!(
            "{API_KEY_DEPENDENCY_REL} is missing; opaque integration key claims cannot be verified"
        ));
    }

    let api_key_roles_path = root.join(API_KEY_ROLES_REL);
    match read_text_silent(&api_key_roles_path) {
        Some(src) => {
            let compact: String = src.chars().filter(|ch| !ch.is_whitespace()).collect();
            if compact.contains("\"ha_stamp\":(\"ha:stamp:run\",)") {
                evidence.push(EvidenceItem {
                    kind: "health-audit-stamp-role-least-privilege".to_string(),
                    path: api_key_roles_path.display().to_string(),
                    line: find_line(&src, "\"ha_stamp\""),
                    detail: "Python opaque-key role mapping grants ha_stamp exactly ha:stamp:run"
                        .to_string(),
                });
            } else {
                warnings.push(format!(
                    "{API_KEY_ROLES_REL}: ha_stamp must map exactly to ha:stamp:run"
                ));
            }
        }
        None => warnings.push(format!(
            "{API_KEY_ROLES_REL} is missing; ha_stamp permission projection cannot be verified"
        )),
    }

    match read_text_silent(&root.join(API_KEY_ROUTER_REL)) {
        Some(src) => {
            let guard_calls = src
                .matches("_require_api_key_management_permission(payload)")
                .count();
            if src.contains("\"ha:users:manage\"")
                && src.contains("\"manage:users\"")
                && guard_calls >= 3
            {
                evidence.push(EvidenceItem {
                    kind: "health-audit-api-key-management-gate".to_string(),
                    path: root.join(API_KEY_ROUTER_REL).display().to_string(),
                    line: find_line(&src, "def _require_api_key_management_permission"),
                    detail: "create, list, and revoke API-key lifecycle operations require explicit platform or Health Audit user-management authority".to_string(),
                });
            } else {
                warnings.push(format!(
                    "{API_KEY_ROUTER_REL}: create, list, and revoke API keys must all invoke the explicit management lifecycle guard (calls={guard_calls})"
                ));
            }
        }
        None => warnings.push(format!(
            "{API_KEY_ROUTER_REL} is missing; API-key lifecycle authorization cannot be verified"
        )),
    }

    // 4. The cartridge loader centrally injects get_current_user before
    // include_router compiles each APIRoute's dependant graph. Health Audit's
    // readiness aliases are the only public paths for that cartridge.
    let cartridge_mount_path = root.join(CARTRIDGE_MOUNT_REL);
    match read_text_silent(&cartridge_mount_path) {
        Some(src) => {
            let required_injection_anchors = [
                "def _inject_auth_dependency(",
                "from example.auth.dependencies import get_current_user",
                "auth_dep = Depends(get_current_user)",
                "route.dependencies = list(route.dependencies or []) + [auth_dep]",
            ];
            let missing_injection_anchors: Vec<&str> = required_injection_anchors
                .iter()
                .copied()
                .filter(|anchor| !src.contains(anchor))
                .collect();
            if missing_injection_anchors.is_empty() {
                evidence.push(EvidenceItem {
                    kind: "central-cartridge-auth-injection".to_string(),
                    path: cartridge_mount_path.display().to_string(),
                    line: find_line(&src, "auth_dep = Depends(get_current_user)"),
                    detail: "central _inject_auth_dependency appends Depends(get_current_user)"
                        .to_string(),
                });
            } else {
                warnings.push(format!(
                    "{CARTRIDGE_MOUNT_REL}: _inject_auth_dependency is missing Depends(get_current_user) wiring anchor(s) {missing_injection_anchors:?}"
                ));
            }

            let public_allowlist_guard =
                "if normalized_path in _PUBLIC_CARTRIDGE_PATHS.get(cartridge_name, frozenset()):";
            let auth_append = "route.dependencies = list(route.dependencies or []) + [auth_dep]";
            match (src.find(public_allowlist_guard), src.find(auth_append)) {
                (Some(guard_offset), Some(append_offset)) if guard_offset < append_offset => {
                    evidence.push(EvidenceItem {
                        kind: "central-auth-honors-public-allowlist".to_string(),
                        path: cartridge_mount_path.display().to_string(),
                        line: find_line(&src, public_allowlist_guard),
                        detail: "auth injection consults the cartridge public allowlist before appending get_current_user".to_string(),
                    });
                }
                _ => warnings.push(format!(
                    "{CARTRIDGE_MOUNT_REL}: auth injection must consult the public allowlist before appending Depends(get_current_user)"
                )),
            }

            let injection_call =
                src.find("_inject_auth_dependency(cartridge_router, manifest, cartridge)");
            let security_call = src.find("_apply_cartridge_security(cartridge_router, cartridge)");
            let include_call = src.find("router.include_router(cartridge_router)");
            match (injection_call, security_call, include_call) {
                (Some(injection_offset), Some(security_offset), Some(include_offset))
                    if injection_offset < security_offset && security_offset < include_offset =>
                {
                    evidence.push(EvidenceItem {
                        kind: "central-authz-before-router-compile".to_string(),
                        path: cartridge_mount_path.display().to_string(),
                        line: find_line(
                            &src,
                            "_apply_cartridge_security(cartridge_router, cartridge)",
                        ),
                        detail: "generic auth runs first, cartridge authorization second, and router inclusion compiles both dependencies last".to_string(),
                    });
                }
                _ => warnings.push(format!(
                    "{CARTRIDGE_MOUNT_REL}: _inject_auth_dependency must run before router.include_router, and _apply_cartridge_security must run after generic auth but before router.include_router so FastAPI compiles authentication and authorization"
                )),
            }

            let mut public_paths = health_audit_public_paths(&src).unwrap_or_default();
            public_paths.sort();
            let expected_public_paths =
                vec!["/status".to_string(), "/v2/health-audit/status".to_string()];
            if public_paths == expected_public_paths {
                evidence.push(EvidenceItem {
                    kind: "health-audit-public-status-only".to_string(),
                    path: cartridge_mount_path.display().to_string(),
                    line: find_line(&src, "\"health_audit\": frozenset"),
                    detail: "health_audit public allowlist exposes only /status aliases"
                        .to_string(),
                });
            } else {
                warnings.push(format!(
                    "{CARTRIDGE_MOUNT_REL}: health_audit public allowlist must expose only /status aliases; found {public_paths:?}"
                ));
            }
        }
        None => warnings.push(format!(
            "{CARTRIDGE_MOUNT_REL} is missing; cannot confirm central Health Audit auth injection"
        )),
    }

    // Health Audit owns a complete method+path policy table. The loader invokes
    // it before route compilation; the hook then rejects both uncovered routes
    // and stale policies during startup, binds the JWT tenant to request state,
    // and checks gateway-issued TokenPayload.permissions.
    let health_audit_authz_path = root.join(HEALTH_AUDIT_AUTHZ_REL);
    match read_text_silent(&health_audit_authz_path) {
        Some(src) => {
            let compact: String = src.chars().filter(|ch| !ch.is_whitespace()).collect();

            let explicit_policy_map = compact
                .contains("HEALTH_AUDIT_ROUTE_POLICIES:Final=_policy_map()");
            let expected_offset = compact.find("expected=set(HEALTH_AUDIT_ROUTE_POLICIES)");
            let actual_offset = compact.find("actual=set(mounted)");
            let comparison_offset = compact.find("ifactual!=expected:");
            let drift_evidence =
                compact.contains("unmapped_routes=") && compact.contains("stale_policies=");
            let attaches_permission_dependency =
                compact.contains("Depends(_permission_dependency(policy,key))");
            let fail_closed_policy = match (expected_offset, actual_offset, comparison_offset) {
                (Some(expected), Some(actual), Some(comparison)) => {
                    expected < comparison && actual < comparison
                }
                _ => false,
            };

            if explicit_policy_map
                && fail_closed_policy
                && drift_evidence
                && attaches_permission_dependency
            {
                evidence.push(EvidenceItem {
                    kind: "health-audit-route-policy-coverage".to_string(),
                    path: health_audit_authz_path.display().to_string(),
                    line: find_line(&src, "HEALTH_AUDIT_ROUTE_POLICIES"),
                    detail: "explicit method+path policies are attached and startup compares actual routes with expected policies in both directions".to_string(),
                });
            } else {
                warnings.push(format!(
                    "{HEALTH_AUDIT_AUTHZ_REL}: authorization policy drift must fail startup by comparing actual and expected route sets (explicit_map={explicit_policy_map}, equality_check={fail_closed_policy}, drift_evidence={drift_evidence}, dependency_attached={attaches_permission_dependency})"
                ));
            }

            let permission_anchors = [
                "def_permission_dependency(",
                "user:TokenPayload=Depends(get_current_user)",
                "granted=set(user.permissions)",
            ];
            let missing_permission_anchors: Vec<&str> = permission_anchors
                .iter()
                .copied()
                .filter(|anchor| !compact.contains(anchor))
                .collect();
            if missing_permission_anchors.is_empty() {
                evidence.push(EvidenceItem {
                    kind: "health-audit-token-permissions".to_string(),
                    path: health_audit_authz_path.display().to_string(),
                    line: find_line(&src, "granted = set(user.permissions)"),
                    detail: "route permission dependency reads gateway-issued TokenPayload.permissions".to_string(),
                });
            } else {
                warnings.push(format!(
                    "{HEALTH_AUDIT_AUTHZ_REL}: permission dependency must authorize from TokenPayload.permissions; missing anchor(s) {missing_permission_anchors:?}"
                ));
            }

            let tenant_rejection_anchors = [
                "wildcard_claim=tenant_id==\"*\"ornamespace==\"*\"",
                "ifwildcard_claimandnotcanonical:",
                "tenant_override==\"*\"",
                "request.headers.get(\"x-tenant-id\")",
                "requested_tenant==\"*\"",
                "requested_tenant!=canonical",
            ];
            let missing_tenant_rejection_anchors: Vec<&str> = tenant_rejection_anchors
                .iter()
                .copied()
                .filter(|anchor| !compact.contains(anchor))
                .collect();
            if missing_tenant_rejection_anchors.is_empty() {
                evidence.push(EvidenceItem {
                    kind: "health-audit-tenant-header-rejection".to_string(),
                    path: health_audit_authz_path.display().to_string(),
                    line: find_line(&src, "requested_tenant ="),
                    detail: "wildcard token use is constrained and x-tenant-id rejects missing, wildcard, or mismatched tenants".to_string(),
                });
            } else {
                warnings.push(format!(
                    "{HEALTH_AUDIT_AUTHZ_REL}: x-tenant-id must reject missing, wildcard, and mismatch values while wildcard claims require an explicit authorized concrete override; missing anchor(s) {missing_tenant_rejection_anchors:?}"
                ));
            }

            let tenant_state_binding = "request.state.health_audit_tenant_id=canonical";
            if compact.contains(tenant_state_binding) {
                evidence.push(EvidenceItem {
                    kind: "health-audit-request-tenant-binding".to_string(),
                    path: health_audit_authz_path.display().to_string(),
                    line: find_line(&src, "request.state.health_audit_tenant_id = canonical"),
                    detail: "validated tenant is bound to request.state.health_audit_tenant_id for downstream handlers".to_string(),
                });
            } else {
                warnings.push(format!(
                    "{HEALTH_AUDIT_AUTHZ_REL}: validated tenant must be bound to request.state.health_audit_tenant_id"
                ));
            }

            let public_policy_count = compact.matches("public=True").count();
            let exact_public_status = compact
                .contains("add(None,(\"GET\",\"/v2/health-audit/status\"),public=True)");
            if public_policy_count == 1 && exact_public_status {
                evidence.push(EvidenceItem {
                    kind: "health-audit-public-policy-status-only".to_string(),
                    path: health_audit_authz_path.display().to_string(),
                    line: find_line(&src, "public=True"),
                    detail: "authorization policy marks only exact GET /v2/health-audit/status as public".to_string(),
                });
            } else {
                warnings.push(format!(
                    "{HEALTH_AUDIT_AUTHZ_REL}: public policy must expose only exact GET /status; public_entries={public_policy_count}, exact_status={exact_public_status}"
                ));
            }
        }
        None => warnings.push(format!(
            "{HEALTH_AUDIT_AUTHZ_REL} is missing; Health Audit has no explicit route-permission and tenant-binding policy"
        )),
    }

    let authz_source = read_text_silent(&root.join(HEALTH_AUDIT_AUTHZ_REL)).unwrap_or_default();
    let authz_compact: String = authz_source
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect();
    let stamp_policy_anchors = [
        "add(HA_STAMP_RUN,(\"POST\",\"/v2/health-audit/stamp\"),(\"POST\",\"/v2/health-audit/audit/xml-glosa\"),)",
        "any_of=(HA_GLOSA_VIEW,HA_STAMP_RUN)",
        "request.state.health_audit_principal=user",
    ];
    let audit_runs_source = read_text_silent(&root.join(HEALTH_AUDIT_RUNS_REL)).unwrap_or_default();
    let queue_source = read_text_silent(&root.join(HEALTH_AUDIT_XML_QUEUE_REL)).unwrap_or_default();
    let router_source = read_text_silent(&root.join(HEALTH_AUDIT_ROUTER_REL)).unwrap_or_default();
    let ownership_anchors_present = stamp_policy_anchors
        .iter()
        .all(|anchor| authz_compact.contains(anchor))
        && audit_runs_source.contains("requester_api_key_id == principal.api_key_id")
        && audit_runs_source.contains("response.pop(\"_requester_api_key_id\", None)")
        && queue_source.contains("run_payload[\"_requester_api_key_id\"]")
        && queue_source.contains("requester_api_key_id: str | None = None")
        && queue_source.contains("components.append(f\"api-key:{canonical_requester}\"")
        && router_source.contains("\"_requester_api_key_id\"")
        && router_source.contains("requester_api_key_id=requester_api_key_id")
        && router_source.contains("persisted_requester_api_key_id != requester_api_key_id");
    if ownership_anchors_present {
        evidence.push(EvidenceItem {
            kind: "health-audit-stamp-owned-polling".to_string(),
            path: root.join(HEALTH_AUDIT_RUNS_REL).display().to_string(),
            line: find_line(&audit_runs_source, "requester_api_key_id == principal.api_key_id"),
            detail: "stamp credentials can poll only their persisted own run and the private owner field is not returned".to_string(),
        });
    } else {
        warnings.push(
            "Health Audit stamp polling must bind the opaque key principal, persist private run ownership, authorize only the exact owner, and conceal the owner field".to_string(),
        );
    }

    // The air-gapped appliance bypasses the central cartridge loader, so it
    // must apply the exact same fail-closed policy before FastAPI compiles the
    // mounted routes. Its single-tenant topology must also pin a concrete
    // tenant slug rather than accepting a wildcard runtime.
    let appliance_path = root.join(HEALTH_AUDIT_APPLIANCE_REL);
    let appliance_compose_path = root.join(HEALTH_AUDIT_APPLIANCE_COMPOSE_REL);
    if appliance_path.exists() || appliance_compose_path.exists() {
        match read_text_silent(&appliance_path) {
            Some(src) => {
                let import_offset = src.find("from .authz import configure_router_security");
                let security_offset =
                    src.find("configure_router_security(health_audit_router)");
                let include_offset = src.find("app.include_router(health_audit_router)");
                match (import_offset, security_offset, include_offset) {
                    (Some(_), Some(security), Some(include)) if security < include => {
                        evidence.push(EvidenceItem {
                            kind: "health-audit-appliance-authz-before-mount".to_string(),
                            path: appliance_path.display().to_string(),
                            line: find_line(
                                &src,
                                "configure_router_security(health_audit_router)",
                            ),
                            detail: "air-gapped appliance applies the Health Audit permission and tenant policy before router inclusion".to_string(),
                        });
                    }
                    _ => warnings.push(format!(
                        "{HEALTH_AUDIT_APPLIANCE_REL}: configure_router_security(health_audit_router) must run before app.include_router so the appliance cannot bypass permission and tenant gates"
                    )),
                }
            }
            None => warnings.push(format!(
                "{HEALTH_AUDIT_APPLIANCE_REL} is missing while the appliance topology exists; cannot confirm route security"
            )),
        }

        match read_text_silent(&appliance_compose_path) {
            Some(src) => {
                let required_tenant_guard = "HEALTH_AUDIT_AUTH_TENANT_SLUG: ${HEALTH_AUDIT_AUTH_TENANT_SLUG:?HEALTH_AUDIT_AUTH_TENANT_SLUG is required}";
                if src.contains(required_tenant_guard) {
                    evidence.push(EvidenceItem {
                        kind: "health-audit-appliance-required-tenant".to_string(),
                        path: appliance_compose_path.display().to_string(),
                        line: find_line(&src, required_tenant_guard),
                        detail: "air-gapped appliance fails fast unless a concrete tenant slug is configured".to_string(),
                    });
                } else {
                    warnings.push(format!(
                        "{HEALTH_AUDIT_APPLIANCE_COMPOSE_REL}: appliance must require HEALTH_AUDIT_AUTH_TENANT_SLUG with a fail-fast :? guard"
                    ));
                }
            }
            None => warnings.push(format!(
                "{HEALTH_AUDIT_APPLIANCE_COMPOSE_REL} is missing while the appliance entrypoint exists; cannot confirm tenant pinning"
            )),
        }
    }

    match read_text_silent(&root.join(GATEWAY_SECURE_AUTH_STORE_REL)) {
        Some(src) => {
            let anchors = [
                "\"perm-ha-stamp-run\"",
                "\"ha:stamp:run\"",
                "\"role-ha-stamp\"",
                "(\"role-ha-stamp\", \"perm-ha-stamp-run\")",
                "permission_id <> 'perm-ha-stamp-run'",
            ];
            let missing: Vec<&str> = anchors
                .iter()
                .copied()
                .filter(|anchor| !src.contains(anchor))
                .collect();
            if missing.is_empty() {
                evidence.push(EvidenceItem {
                    kind: "health-audit-secure-store-stamp-role".to_string(),
                    path: root
                        .join(GATEWAY_SECURE_AUTH_STORE_REL)
                        .display()
                        .to_string(),
                    line: find_line(&src, "perm-ha-stamp-run"),
                    detail: "encrypted auth store converges ha_stamp to the same narrow permission as the primary gateway seed".to_string(),
                });
            } else {
                warnings.push(format!(
                    "{GATEWAY_SECURE_AUTH_STORE_REL}: encrypted auth seed must converge ha_stamp to only ha:stamp:run; missing anchor(s) {missing:?}"
                ));
            }
        }
        None => warnings.push(format!(
            "{GATEWAY_SECURE_AUTH_STORE_REL} is missing; encrypted auth role parity cannot be verified"
        )),
    }

    // The unauthenticated status route is deliberately tiny. Dependency and
    // filesystem diagnostics belong on the authenticated /status/details
    // route so a public health probe cannot trigger expensive work.
    let health_audit_router_path = root.join(HEALTH_AUDIT_ROUTER_REL);
    match read_text_silent(&health_audit_router_path) {
        Some(src) => {
            let public_marker = "@router.get(\"/status\")";
            let details_marker = "@router.get(\"/status/details\")";
            match (src.find(public_marker), src.find(details_marker)) {
                (Some(public_offset), Some(details_offset)) if public_offset < details_offset => {
                    let public_status = &src[public_offset..details_offset];
                    let heavy_probe_anchors = [
                        "_get_ontology_dependency_state",
                        "get_tuss_repo",
                        "_ensure_directory",
                        "build_ocr_dependency_state",
                        "parsers_status",
                        "build_reference_pipeline_statuses",
                        "embedding_status",
                        "fast_stamp_status",
                        "build_cartridge_status",
                    ];
                    let heavy_probes: Vec<&str> = heavy_probe_anchors
                        .iter()
                        .copied()
                        .filter(|anchor| public_status.contains(anchor))
                        .collect();
                    let minimal_payload = [
                        "\"cartridge\"",
                        "\"health_audit\"",
                        "\"status\"",
                        "\"ok\"",
                        "\"ready\"",
                        "True",
                    ]
                    .iter()
                    .all(|anchor| public_status.contains(anchor));

                    if heavy_probes.is_empty() && minimal_payload {
                        evidence.push(EvidenceItem {
                            kind: "health-audit-public-status-lightweight".to_string(),
                            path: health_audit_router_path.display().to_string(),
                            line: find_line(&src, public_marker),
                            detail: "public /status returns a lightweight static readiness payload; detailed probes require auth".to_string(),
                        });
                    } else {
                        warnings.push(format!(
                            "{HEALTH_AUDIT_ROUTER_REL}: public /status must remain lightweight and static (minimal_payload={minimal_payload}, heavy_probes={heavy_probes:?}); move dependency diagnostics to authenticated /status/details"
                        ));
                    }
                }
                _ => warnings.push(format!(
                    "{HEALTH_AUDIT_ROUTER_REL}: public /status must be lightweight and precede the authenticated /status/details route"
                )),
            }
        }
        None => warnings.push(format!(
            "{HEALTH_AUDIT_ROUTER_REL} is missing; cannot confirm public /status is lightweight"
        )),
    }

    // 5. Deploy wires the provisioning env to the gateway service.
    let compose_path = root.join(COMPOSE_REL);
    if let Some(src) = read_text_silent(&compose_path) {
        let required_pg_password_guard =
            "POSTGRES_PASSWORD: ${HEALTH_AUDIT_PG_PASSWORD:?HEALTH_AUDIT_PG_PASSWORD is required}";
        if src.contains(required_pg_password_guard) {
            evidence.push(EvidenceItem {
                kind: "compose-required-postgres-password".to_string(),
                path: compose_path.display().to_string(),
                line: find_line(&src, required_pg_password_guard),
                detail: "compose fails fast when HEALTH_AUDIT_PG_PASSWORD is absent".to_string(),
            });
        } else {
            warnings.push(format!(
                "{COMPOSE_REL}: compose must require HEALTH_AUDIT_PG_PASSWORD with a fail-fast :? guard"
            ));
        }
        let required = [
            "HEALTH_AUDIT_AUTH_TENANT_SLUG",
            "HEALTH_AUDIT_AUTH_TENANT_NAME",
            "HEALTH_AUDIT_AUTH_ADMIN_EMAILS",
            "HEALTH_AUDIT_AUTH_ADMIN_PASSWORD",
        ];
        let missing: Vec<&str> = required
            .iter()
            .copied()
            .filter(|needle| !src.contains(needle))
            .collect();
        if missing.is_empty() {
            evidence.push(EvidenceItem {
                kind: "compose-provisioning-env".to_string(),
                path: compose_path.display().to_string(),
                line: find_line(&src, "HEALTH_AUDIT_AUTH_TENANT_SLUG"),
                detail: "gateway service receives HEALTH_AUDIT_AUTH_* provisioning env".to_string(),
            });
        } else {
            warnings.push(format!(
                "{COMPOSE_REL}: missing provisioning env {:?}; the gateway will not fully seed health-audit auth",
                missing
            ));
        }
    } else {
        warnings.push(format!(
            "{COMPOSE_REL} is missing; cannot confirm provisioning env wiring"
        ));
    }

    // 6. The profile/secret-set split must keep non-secret bootstrap data in
    // the tracked profile and the password in the gitignored secret-set.
    let profile_path = root.join(PROFILE_REL);
    match read_text_silent(&profile_path) {
        Some(src) => {
            let missing: Vec<&str> = [
                "HEALTH_AUDIT_AUTH_TENANT_SLUG=",
                "HEALTH_AUDIT_AUTH_TENANT_NAME=",
                "HEALTH_AUDIT_AUTH_ADMIN_EMAILS=",
            ]
            .iter()
            .copied()
            .filter(|needle| !src.contains(needle))
            .collect();
            if !missing.is_empty() {
                warnings.push(format!(
                    "{PROFILE_REL}: missing bootstrap profile field(s) {:?}; target renders will drift",
                    missing
                ));
            }
            if src.contains("HEALTH_AUDIT_AUTH_ADMIN_PASSWORD=") {
                warnings.push(format!(
                    "{PROFILE_REL}: tracked profile must not contain HEALTH_AUDIT_AUTH_ADMIN_PASSWORD; keep it in the gitignored secret set"
                ));
            }
            if let Some(line) = find_line(&src, "HEALTH_AUDIT_AUTH_TENANT_SLUG") {
                evidence.push(EvidenceItem {
                    kind: "profile-bootstrap-env".to_string(),
                    path: profile_path.display().to_string(),
                    line: Some(line),
                    detail: "tracked profile owns tenant slug/name/admin emails; password stays out-of-band".to_string(),
                });
            }
        }
        None => warnings.push(format!(
            "{PROFILE_REL} is missing; cannot confirm tracked bootstrap env"
        )),
    }

    let secret_set_path = root.join(SECRET_SET_REL);
    match read_text_silent(&secret_set_path) {
        Some(src) => {
            if !src.contains("HEALTH_AUDIT_AUTH_ADMIN_PASSWORD=") {
                warnings.push(format!(
                    "{SECRET_SET_REL}: missing HEALTH_AUDIT_AUTH_ADMIN_PASSWORD placeholder; operators have no canonical secret-set entry for auth bootstrap"
                ));
            } else if let Some(line) = find_line(&src, "HEALTH_AUDIT_AUTH_ADMIN_PASSWORD") {
                evidence.push(EvidenceItem {
                    kind: "secret-set-bootstrap-password".to_string(),
                    path: secret_set_path.display().to_string(),
                    line: Some(line),
                    detail: "gitignored secret-set template owns the auth bootstrap password"
                        .to_string(),
                });
            }
            if has_env_placeholder(&src, "HEALTH_AUDIT_PG_PASSWORD") {
                evidence.push(EvidenceItem {
                    kind: "secret-set-postgres-password".to_string(),
                    path: secret_set_path.display().to_string(),
                    line: find_line(&src, "HEALTH_AUDIT_PG_PASSWORD"),
                    detail: "canonical secret-set supplies the compose-required Health Audit Postgres password".to_string(),
                });
            } else {
                warnings.push(format!(
                    "{SECRET_SET_REL}: HEALTH_AUDIT_PG_PASSWORD is required by {COMPOSE_REL} but missing from the canonical secret-set"
                ));
            }
        }
        None => warnings.push(format!(
            "{SECRET_SET_REL} is missing; cannot confirm secret-set bootstrap contract"
        )),
    }

    let summary = if warnings.is_empty() {
        "health-audit-auth: single gateway-owned store, central route auth compiled, status public and lightweight, deploy secret contract complete".to_string()
    } else {
        format!(
            "health-audit-auth: {} drift indicator(s) — single-store auth contract at risk",
            warnings.len()
        )
    };
    let confidence = if warnings.is_empty() {
        0.95_f32
    } else {
        0.6_f32
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor.health-audit-auth"),
        kind: "doctor".to_string(),
        summary,
        confidence,
        entities: vec![],
        evidence,
        warnings,
        meta: Some(json!({
            "roles": HA_ROLES,
            "store": "example-gateway auth_users (DuckDB)",
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        API_KEY_DEPENDENCY_REL, API_KEY_ROLES_REL, API_KEY_ROUTER_REL, CARTRIDGE_MOUNT_REL,
        COMPOSE_REL, GATEWAY_SECURE_AUTH_STORE_REL, HEALTH_AUDIT_APPLIANCE_COMPOSE_REL,
        HEALTH_AUDIT_APPLIANCE_REL, HEALTH_AUDIT_AUTHZ_REL, HEALTH_AUDIT_ROUTER_REL,
        HEALTH_AUDIT_RUNS_REL, HEALTH_AUDIT_XML_QUEUE_REL, PROFILE_REL, SECRET_SET_REL,
        doctor_health_audit_auth,
    };
    use crate::model::RepoIndex;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_repo(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "leio-code-ha-auth-{name}-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create temp repo");
        root
    }

    fn empty_index() -> RepoIndex {
        RepoIndex {
            version: 1,
            root: "/tmp/repo".to_string(),
            indexed_at: "2026-01-01T00:00:00Z".to_string(),
            files: Vec::new(),
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        }
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let full = root.join(rel);
        fs::create_dir_all(full.parent().expect("parent")).expect("create parent");
        fs::write(full, body).expect("write file");
    }

    const OK_SEED: &str = r#"
fn apply_health_audit_auth_roles() {
    let _ = ["ha_admin", "ha_auditor", "ha_rules", "ha_viewer", "ha_stamp"];
    let _ = "ha:*";
    let _ = "'perm-ha-stamp-run',       'ha:stamp:run'";
    let _ = "('role-ha-stamp', 'perm-ha-stamp-run')";
    let _ = "permission_id <> 'perm-ha-stamp-run'";
}

fn provision_health_audit_admin() {}
"#;

    const OK_MAIN: &str = r#"
fn main() {
    match apply_health_audit_auth_roles() {
        Ok(()) => {}
        Err(e) => return Err(anyhow::anyhow!("Health-audit role seed failed: {e}")),
    }
    if task_failed {
        return Err(anyhow::anyhow!("Health-audit role seed task error: {e}"));
    }
    provision_health_audit_admin();
}
"#;

    const OK_SECURE_AUTH_STORE: &str = r#"
fn apply_health_audit_roles() {
    let _ = "perm-ha-stamp-run";
    let _ = "ha:stamp:run";
    let _ = "role-ha-stamp";
    let _ = ("role-ha-stamp", "perm-ha-stamp-run");
    let _ = "permission_id <> 'perm-ha-stamp-run'";
}
"#;

    const OK_CONSOLE: &str = r#"
export function resolveAuthBackendOrder(preference?: string) {
  if (preference?.trim().toLowerCase() === "python") {
    return ["python", "gateway"];
  }
  return ["gateway", "python"];
}
"#;

    const OK_COMPOSE: &str = r#"
services:
  postgres:
    environment:
      POSTGRES_PASSWORD: ${HEALTH_AUDIT_PG_PASSWORD:?HEALTH_AUDIT_PG_PASSWORD is required}
  ocr-sidecar:
    environment:
      HEALTH_AUDIT_AUTH_TENANT_SLUG: ${HEALTH_AUDIT_AUTH_TENANT_SLUG:-}
      HEALTH_AUDIT_AUTH_TENANT_NAME: ${HEALTH_AUDIT_AUTH_TENANT_NAME:-}
      HEALTH_AUDIT_AUTH_ADMIN_EMAILS: ${HEALTH_AUDIT_AUTH_ADMIN_EMAILS:-}
      HEALTH_AUDIT_AUTH_ADMIN_PASSWORD: ${HEALTH_AUDIT_AUTH_ADMIN_PASSWORD:-}
"#;

    const OK_PROFILE: &str = r#"
HEALTH_AUDIT_AUTH_TENANT_SLUG=pcpsaude
HEALTH_AUDIT_AUTH_TENANT_NAME=PCP Saúde
HEALTH_AUDIT_AUTH_ADMIN_EMAILS=admin@example.com
"#;

    const OK_SECRET_SET: &str = r#"
HEALTH_AUDIT_AUTH_ADMIN_PASSWORD=
HEALTH_AUDIT_PG_PASSWORD=
"#;

    const OK_CARTRIDGE_MOUNT: &str = r#"
_PUBLIC_CARTRIDGE_PATHS = {
    "health_audit": frozenset({"/v2/health-audit/status", "/status"}),
}

def _inject_auth_dependency(cartridge_router, manifest, cartridge_name):
    from example.auth.dependencies import get_current_user

    auth_dep = Depends(get_current_user)
    for route in cartridge_router.routes:
        normalized_path = route.path.rstrip("/")
        if normalized_path in _PUBLIC_CARTRIDGE_PATHS.get(cartridge_name, frozenset()):
            continue
        route.dependencies = list(route.dependencies or []) + [auth_dep]

def get_cartridge_router():
    _inject_auth_dependency(cartridge_router, manifest, cartridge)
    _apply_cartridge_security(cartridge_router, cartridge)
    router.include_router(cartridge_router)
"#;

    const OK_HEALTH_AUDIT_AUTHZ: &str = r#"
from fastapi import Depends
from example.auth.dependencies import TokenPayload, get_current_user

def _policy_map():
    policies = {}
    add(None, ("GET", "/v2/health-audit/status"), public=True)
    add("ha:rules:view", ("GET", "/v2/health-audit/rules"))
    add(
        HA_STAMP_RUN,
        ("POST", "/v2/health-audit/stamp"),
        ("POST", "/v2/health-audit/audit/xml-glosa"),
    )
    add(
        None,
        ("GET", "/v2/health-audit/audit/runs/{run_id}"),
        any_of=(HA_GLOSA_VIEW, HA_STAMP_RUN),
    )
    return policies

HEALTH_AUDIT_ROUTE_POLICIES: Final = _policy_map()

async def _bind_tenant(request, user: TokenPayload):
    tenant_id = user.tenant_id.strip()
    namespace = user.namespace.strip()
    wildcard_claim = tenant_id == "*" or namespace == "*"
    if wildcard_claim and not canonical:
        if "*" not in user.permissions or not tenant_override or tenant_override == "*":
            raise _forbidden("Explicit tenant override required for wildcard token")
    requested_tenant = (request.headers.get("x-tenant-id") or "").strip()
    if not requested_tenant or requested_tenant == "*" or requested_tenant != canonical:
        raise _forbidden("Tenant context does not match authenticated token")
    request.state.health_audit_tenant_id = canonical

def _permission_dependency(policy, key):
    async def require_health_audit_access(
        request,
        user: TokenPayload = Depends(get_current_user),
    ):
        await _bind_tenant(request, user)
        request.state.health_audit_principal = user
        granted = set(user.permissions)
        if "*" in granted:
            return

def configure_router_security(router):
    mounted = {}
    expected = set(HEALTH_AUDIT_ROUTE_POLICIES)
    actual = set(mounted)
    if actual != expected:
        missing = sorted(actual - expected)
        stale = sorted(expected - actual)
        raise RuntimeError(
            f"unmapped_routes={missing!r}; stale_policies={stale!r}"
        )
    for key, route in mounted.items():
        policy = HEALTH_AUDIT_ROUTE_POLICIES[key]
        route.dependencies = list(route.dependencies or []) + [
            Depends(_permission_dependency(policy, key))
        ]
"#;

    const OK_API_KEY_DEPENDENCIES: &str = r#"
async def _resolve_opaque_api_key(raw_key):
    return TokenPayload(
        sub=api_key.id,
        roles=[api_key.role],
        permissions=permissions_for_role(api_key.role),
        namespace=namespace,
        tenant_id=namespace,
        api_key_id=api_key.id,
        type="api_key",
    )
"#;

    const OK_API_KEY_ROLES: &str = r#"
HEALTH_AUDIT_ROLE_PERMISSIONS = {
    "ha_stamp": ("ha:stamp:run",),
}
"#;

    const OK_API_KEY_ROUTER: &str = r#"
def _require_api_key_management_permission(payload):
    management_permissions = {"*", "manage:users", "ha:users:manage"}
    if set(payload["permissions"]).isdisjoint(management_permissions):
        raise Forbidden()

def create_api_key_endpoint(payload):
    _require_api_key_management_permission(payload)

def list_api_keys(payload):
    _require_api_key_management_permission(payload)

def revoke_api_key(payload):
    _require_api_key_management_permission(payload)
"#;

    const OK_HEALTH_AUDIT_RUNS: &str = r#"
def get_audit_run(request, run_id):
    principal = health_audit_principal(request)
    requester_api_key_id = run_payload.get("_requester_api_key_id")
    if requester_api_key_id == principal.api_key_id:
        response = dict(run_payload)
        response.pop("_requester_api_key_id", None)
        return response
"#;

    const OK_HEALTH_AUDIT_XML_QUEUE: &str = r#"
def content_addressed_xml_glosa_run_id(requester_api_key_id: str | None = None):
    components.append(f"api-key:{canonical_requester}".encode())

def create_queued_xml_glosa_audit_run(requester_api_key_id: str | None = None):
    run_payload["_requester_api_key_id"] = requester_api_key_id
"#;

    const OK_HEALTH_AUDIT_ROUTER: &str = r#"
@router.get("/status")
async def cartridge_status():
    return {"cartridge": "health_audit", "status": "ok", "ready": True}

@router.get("/status/details")
async def cartridge_status_details():
    ontology_ready, ontology_error = await _get_ontology_dependency_state()
    return build_cartridge_status({})

def _accept_xml_glosa_audit_job(requester_api_key_id):
    content_addressed_xml_glosa_run_id(
        requester_api_key_id=requester_api_key_id,
    )
    persisted_requester_api_key_id = run_payload.get("_requester_api_key_id")
    if persisted_requester_api_key_id != requester_api_key_id:
        raise NotFound()
    for field in ("_requester_api_key_id",):
        preserve(field)
"#;

    #[test]
    fn passes_when_gateway_bootstrap_contract_is_complete() {
        let root = temp_repo("ok");
        write(&root, "example-gateway/src/auth/seed.rs", OK_SEED);
        write(&root, GATEWAY_SECURE_AUTH_STORE_REL, OK_SECURE_AUTH_STORE);
        write(&root, "example-gateway/src/main.rs", OK_MAIN);
        write(&root, "health-audit-console/lib/authBackend.ts", OK_CONSOLE);
        write(&root, COMPOSE_REL, OK_COMPOSE);
        write(&root, PROFILE_REL, OK_PROFILE);
        write(&root, SECRET_SET_REL, OK_SECRET_SET);
        write(&root, CARTRIDGE_MOUNT_REL, OK_CARTRIDGE_MOUNT);
        write(
            &root,
            "cartridges/health_audit/authz.py",
            OK_HEALTH_AUDIT_AUTHZ,
        );
        write(&root, API_KEY_DEPENDENCY_REL, OK_API_KEY_DEPENDENCIES);
        write(&root, API_KEY_ROLES_REL, OK_API_KEY_ROLES);
        write(&root, API_KEY_ROUTER_REL, OK_API_KEY_ROUTER);
        write(&root, HEALTH_AUDIT_RUNS_REL, OK_HEALTH_AUDIT_RUNS);
        write(&root, HEALTH_AUDIT_XML_QUEUE_REL, OK_HEALTH_AUDIT_XML_QUEUE);
        write(
            &root,
            "cartridges/health_audit/router.py",
            OK_HEALTH_AUDIT_ROUTER,
        );

        let result = doctor_health_audit_auth(&empty_index(), &root);

        assert!(
            result.warnings.is_empty(),
            "expected no warnings, got {:?}",
            result.warnings
        );
        assert!(result.summary.contains("single gateway-owned store"));
    }

    #[test]
    fn warns_when_stamp_role_regains_the_broad_audit_permission() {
        let root = temp_repo("stamp-role-broad");
        write(&root, "example-gateway/src/auth/seed.rs", OK_SEED);
        write(&root, "example-gateway/src/main.rs", OK_MAIN);
        write(&root, "health-audit-console/lib/authBackend.ts", OK_CONSOLE);
        write(&root, COMPOSE_REL, OK_COMPOSE);
        write(&root, PROFILE_REL, OK_PROFILE);
        write(&root, SECRET_SET_REL, OK_SECRET_SET);
        write(&root, CARTRIDGE_MOUNT_REL, OK_CARTRIDGE_MOUNT);
        write(&root, HEALTH_AUDIT_AUTHZ_REL, OK_HEALTH_AUDIT_AUTHZ);
        write(&root, API_KEY_DEPENDENCY_REL, OK_API_KEY_DEPENDENCIES);
        write(
            &root,
            API_KEY_ROLES_REL,
            &OK_API_KEY_ROLES.replace("ha:stamp:run", "ha:audit:run"),
        );
        write(&root, HEALTH_AUDIT_RUNS_REL, OK_HEALTH_AUDIT_RUNS);
        write(&root, HEALTH_AUDIT_XML_QUEUE_REL, OK_HEALTH_AUDIT_XML_QUEUE);
        write(&root, HEALTH_AUDIT_ROUTER_REL, OK_HEALTH_AUDIT_ROUTER);

        let result = doctor_health_audit_auth(&empty_index(), &root);

        assert!(
            result.warnings.iter().any(|warning| {
                warning.contains("ha_stamp") && warning.contains("ha:stamp:run")
            })
        );
    }

    #[test]
    fn warns_when_gateway_logs_auth_seed_failure_and_keeps_starting() {
        let root = temp_repo("auth-seed-fail-open");
        write(&root, "example-gateway/src/auth/seed.rs", OK_SEED);
        write(
            &root,
            "example-gateway/src/main.rs",
            &OK_MAIN
                .replace(
                    "return Err(anyhow::anyhow!(\"Health-audit role seed failed: {e}\"))",
                    "warn!(\"Health-audit role seed failed: {e}\")",
                )
                .replace(
                    "return Err(anyhow::anyhow!(\"Health-audit role seed task error: {e}\"))",
                    "warn!(\"Health-audit role seed task error: {e}\")",
                ),
        );
        write(&root, GATEWAY_SECURE_AUTH_STORE_REL, OK_SECURE_AUTH_STORE);
        write(&root, "health-audit-console/lib/authBackend.ts", OK_CONSOLE);
        write(&root, COMPOSE_REL, OK_COMPOSE);
        write(&root, PROFILE_REL, OK_PROFILE);
        write(&root, SECRET_SET_REL, OK_SECRET_SET);
        write(&root, CARTRIDGE_MOUNT_REL, OK_CARTRIDGE_MOUNT);
        write(&root, HEALTH_AUDIT_AUTHZ_REL, OK_HEALTH_AUDIT_AUTHZ);
        write(&root, API_KEY_DEPENDENCY_REL, OK_API_KEY_DEPENDENCIES);
        write(&root, API_KEY_ROLES_REL, OK_API_KEY_ROLES);
        write(&root, API_KEY_ROUTER_REL, OK_API_KEY_ROUTER);
        write(&root, HEALTH_AUDIT_RUNS_REL, OK_HEALTH_AUDIT_RUNS);
        write(&root, HEALTH_AUDIT_XML_QUEUE_REL, OK_HEALTH_AUDIT_XML_QUEUE);
        write(&root, HEALTH_AUDIT_ROUTER_REL, OK_HEALTH_AUDIT_ROUTER);

        let result = doctor_health_audit_auth(&empty_index(), &root);

        assert!(
            result.warnings.iter().any(|warning| {
                warning.contains("fail closed") && warning.contains("auth seed")
            })
        );
    }

    #[test]
    fn warns_when_stamp_polling_drops_exact_key_ownership() {
        let root = temp_repo("stamp-polling-unowned");
        write(&root, "example-gateway/src/auth/seed.rs", OK_SEED);
        write(&root, "example-gateway/src/main.rs", OK_MAIN);
        write(&root, "health-audit-console/lib/authBackend.ts", OK_CONSOLE);
        write(&root, COMPOSE_REL, OK_COMPOSE);
        write(&root, PROFILE_REL, OK_PROFILE);
        write(&root, SECRET_SET_REL, OK_SECRET_SET);
        write(&root, CARTRIDGE_MOUNT_REL, OK_CARTRIDGE_MOUNT);
        write(&root, HEALTH_AUDIT_AUTHZ_REL, OK_HEALTH_AUDIT_AUTHZ);
        write(&root, API_KEY_DEPENDENCY_REL, OK_API_KEY_DEPENDENCIES);
        write(&root, API_KEY_ROLES_REL, OK_API_KEY_ROLES);
        write(
            &root,
            HEALTH_AUDIT_RUNS_REL,
            &OK_HEALTH_AUDIT_RUNS.replace("requester_api_key_id == principal.api_key_id", "True"),
        );
        write(&root, HEALTH_AUDIT_XML_QUEUE_REL, OK_HEALTH_AUDIT_XML_QUEUE);
        write(&root, HEALTH_AUDIT_ROUTER_REL, OK_HEALTH_AUDIT_ROUTER);

        let result = doctor_health_audit_auth(&empty_index(), &root);

        assert!(result.warnings.iter().any(|warning| {
            warning.contains("stamp polling") && warning.contains("exact owner")
        }));
    }

    #[test]
    fn warns_when_cartridge_security_hook_is_not_between_auth_and_router_inclusion() {
        let root = temp_repo("late-cartridge-security");
        write(&root, "example-gateway/src/auth/seed.rs", OK_SEED);
        write(&root, "example-gateway/src/main.rs", OK_MAIN);
        write(&root, "health-audit-console/lib/authBackend.ts", OK_CONSOLE);
        write(&root, COMPOSE_REL, OK_COMPOSE);
        write(&root, PROFILE_REL, OK_PROFILE);
        write(&root, SECRET_SET_REL, OK_SECRET_SET);
        write(
            &root,
            CARTRIDGE_MOUNT_REL,
            &OK_CARTRIDGE_MOUNT.replace(
                "    _apply_cartridge_security(cartridge_router, cartridge)\n    router.include_router(cartridge_router)",
                "    router.include_router(cartridge_router)\n    _apply_cartridge_security(cartridge_router, cartridge)",
            ),
        );
        write(
            &root,
            "cartridges/health_audit/authz.py",
            OK_HEALTH_AUDIT_AUTHZ,
        );
        write(
            &root,
            "cartridges/health_audit/router.py",
            OK_HEALTH_AUDIT_ROUTER,
        );

        let result = doctor_health_audit_auth(&empty_index(), &root);

        assert!(result.warnings.iter().any(|warning| {
            warning.contains("_apply_cartridge_security")
                && warning.contains("before router.include_router")
        }));
    }

    #[test]
    fn warns_when_appliance_bypasses_health_audit_security() {
        let root = temp_repo("appliance-authz-bypass");
        write(
            &root,
            HEALTH_AUDIT_APPLIANCE_REL,
            "from .router import router as health_audit_router\napp.include_router(health_audit_router)\n",
        );
        write(
            &root,
            HEALTH_AUDIT_APPLIANCE_COMPOSE_REL,
            "services:\n  appliance:\n    environment:\n      HEALTH_AUDIT_AUTH_TENANT_SLUG: ${HEALTH_AUDIT_AUTH_TENANT_SLUG:-}\n",
        );

        let result = doctor_health_audit_auth(&empty_index(), &root);

        assert!(result.warnings.iter().any(|warning| {
            warning.contains("appliance_app.py")
                && warning.contains("configure_router_security")
                && warning.contains("before app.include_router")
        }));
        assert!(result.warnings.iter().any(|warning| {
            warning.contains("docker-compose.appliance.yml")
                && warning.contains("must require HEALTH_AUDIT_AUTH_TENANT_SLUG")
        }));
    }

    #[test]
    fn warns_when_health_audit_authz_policy_file_is_missing() {
        let root = temp_repo("missing-health-audit-authz");
        write(&root, "example-gateway/src/auth/seed.rs", OK_SEED);
        write(&root, "example-gateway/src/main.rs", OK_MAIN);
        write(&root, "health-audit-console/lib/authBackend.ts", OK_CONSOLE);
        write(&root, COMPOSE_REL, OK_COMPOSE);
        write(&root, PROFILE_REL, OK_PROFILE);
        write(&root, SECRET_SET_REL, OK_SECRET_SET);
        write(&root, CARTRIDGE_MOUNT_REL, OK_CARTRIDGE_MOUNT);
        write(
            &root,
            "cartridges/health_audit/router.py",
            OK_HEALTH_AUDIT_ROUTER,
        );

        let result = doctor_health_audit_auth(&empty_index(), &root);

        assert!(result.warnings.iter().any(|warning| {
            warning.contains("cartridges/health_audit/authz.py") && warning.contains("missing")
        }));
    }

    #[test]
    fn warns_when_health_audit_route_policy_does_not_fail_closed_on_drift() {
        let root = temp_repo("authz-policy-not-fail-closed");
        write(&root, "example-gateway/src/auth/seed.rs", OK_SEED);
        write(&root, "example-gateway/src/main.rs", OK_MAIN);
        write(&root, "health-audit-console/lib/authBackend.ts", OK_CONSOLE);
        write(&root, COMPOSE_REL, OK_COMPOSE);
        write(&root, PROFILE_REL, OK_PROFILE);
        write(&root, SECRET_SET_REL, OK_SECRET_SET);
        write(&root, CARTRIDGE_MOUNT_REL, OK_CARTRIDGE_MOUNT);
        write(
            &root,
            "cartridges/health_audit/authz.py",
            &OK_HEALTH_AUDIT_AUTHZ.replace("    if actual != expected:\n", ""),
        );
        write(
            &root,
            "cartridges/health_audit/router.py",
            OK_HEALTH_AUDIT_ROUTER,
        );

        let result = doctor_health_audit_auth(&empty_index(), &root);

        assert!(result.warnings.iter().any(|warning| {
            warning.contains("authorization policy drift")
                && warning.contains("actual")
                && warning.contains("expected")
        }));
    }

    #[test]
    fn warns_when_health_audit_permission_dependency_ignores_token_permissions() {
        let root = temp_repo("authz-ignores-token-permissions");
        write(&root, "example-gateway/src/auth/seed.rs", OK_SEED);
        write(&root, "example-gateway/src/main.rs", OK_MAIN);
        write(&root, "health-audit-console/lib/authBackend.ts", OK_CONSOLE);
        write(&root, COMPOSE_REL, OK_COMPOSE);
        write(&root, PROFILE_REL, OK_PROFILE);
        write(&root, SECRET_SET_REL, OK_SECRET_SET);
        write(&root, CARTRIDGE_MOUNT_REL, OK_CARTRIDGE_MOUNT);
        write(
            &root,
            "cartridges/health_audit/authz.py",
            &OK_HEALTH_AUDIT_AUTHZ.replace(
                "        granted = set(user.permissions)",
                "        granted = set()",
            ),
        );
        write(
            &root,
            "cartridges/health_audit/router.py",
            OK_HEALTH_AUDIT_ROUTER,
        );

        let result = doctor_health_audit_auth(&empty_index(), &root);

        assert!(result.warnings.iter().any(|warning| {
            warning.contains("TokenPayload.permissions")
                && warning.contains("permission dependency")
        }));
    }

    #[test]
    fn warns_when_opaque_api_key_does_not_materialize_stamp_scope_and_tenant() {
        let root = temp_repo("opaque-key-misses-stamp-scope");
        write(
            &root,
            "example-api/example/auth/dependencies.py",
            r#"
async def _resolve_opaque_api_key(raw_key):
    return TokenPayload(
        sub=api_key.id,
        role=api_key.role,
        namespace=namespace,
        type="api_key",
    )
"#,
        );
        write(
            &root,
            "example-api/example/auth/roles.py",
            r#"
API_KEY_ROLE_PERMISSIONS = {
    "ha_stamp": ("ha:audit:run",),
}
"#,
        );

        let result = doctor_health_audit_auth(&empty_index(), &root);

        assert!(result.warnings.iter().any(|warning| {
            warning.contains("opaque API key")
                && warning.contains("permissions")
                && warning.contains("tenant_id")
        }));
    }

    #[test]
    fn warns_when_health_audit_tenant_header_allows_wildcard_or_mismatch() {
        let root = temp_repo("authz-allows-tenant-header-drift");
        write(&root, "example-gateway/src/auth/seed.rs", OK_SEED);
        write(&root, "example-gateway/src/main.rs", OK_MAIN);
        write(&root, "health-audit-console/lib/authBackend.ts", OK_CONSOLE);
        write(&root, COMPOSE_REL, OK_COMPOSE);
        write(&root, PROFILE_REL, OK_PROFILE);
        write(&root, SECRET_SET_REL, OK_SECRET_SET);
        write(&root, CARTRIDGE_MOUNT_REL, OK_CARTRIDGE_MOUNT);
        write(
            &root,
            "cartridges/health_audit/authz.py",
            &OK_HEALTH_AUDIT_AUTHZ.replace(
                "    if not requested_tenant or requested_tenant == \"*\" or requested_tenant != canonical:",
                "    if not requested_tenant:",
            ),
        );
        write(
            &root,
            "cartridges/health_audit/router.py",
            OK_HEALTH_AUDIT_ROUTER,
        );

        let result = doctor_health_audit_auth(&empty_index(), &root);

        assert!(result.warnings.iter().any(|warning| {
            warning.contains("x-tenant-id")
                && warning.contains("wildcard")
                && warning.contains("mismatch")
        }));
    }

    #[test]
    fn warns_when_health_audit_validated_tenant_is_not_bound_to_request_state() {
        let root = temp_repo("authz-does-not-bind-tenant");
        write(&root, "example-gateway/src/auth/seed.rs", OK_SEED);
        write(&root, "example-gateway/src/main.rs", OK_MAIN);
        write(&root, "health-audit-console/lib/authBackend.ts", OK_CONSOLE);
        write(&root, COMPOSE_REL, OK_COMPOSE);
        write(&root, PROFILE_REL, OK_PROFILE);
        write(&root, SECRET_SET_REL, OK_SECRET_SET);
        write(&root, CARTRIDGE_MOUNT_REL, OK_CARTRIDGE_MOUNT);
        write(
            &root,
            "cartridges/health_audit/authz.py",
            &OK_HEALTH_AUDIT_AUTHZ
                .replace("    request.state.health_audit_tenant_id = canonical\n", ""),
        );
        write(
            &root,
            "cartridges/health_audit/router.py",
            OK_HEALTH_AUDIT_ROUTER,
        );

        let result = doctor_health_audit_auth(&empty_index(), &root);

        assert!(result.warnings.iter().any(|warning| {
            warning.contains("request.state.health_audit_tenant_id")
                && warning.contains("validated tenant")
        }));
    }

    #[test]
    fn warns_when_health_audit_authz_exposes_any_route_other_than_exact_status() {
        let root = temp_repo("authz-public-business-route");
        write(&root, "example-gateway/src/auth/seed.rs", OK_SEED);
        write(&root, "example-gateway/src/main.rs", OK_MAIN);
        write(&root, "health-audit-console/lib/authBackend.ts", OK_CONSOLE);
        write(&root, COMPOSE_REL, OK_COMPOSE);
        write(&root, PROFILE_REL, OK_PROFILE);
        write(&root, SECRET_SET_REL, OK_SECRET_SET);
        write(&root, CARTRIDGE_MOUNT_REL, OK_CARTRIDGE_MOUNT);
        write(
            &root,
            "cartridges/health_audit/authz.py",
            &OK_HEALTH_AUDIT_AUTHZ.replace(
                "add(None, (\"GET\", \"/v2/health-audit/status\"), public=True)",
                "add(None, (\"GET\", \"/v2/health-audit/contracts\"), public=True)",
            ),
        );
        write(
            &root,
            "cartridges/health_audit/router.py",
            OK_HEALTH_AUDIT_ROUTER,
        );

        let result = doctor_health_audit_auth(&empty_index(), &root);

        assert!(result.warnings.iter().any(|warning| {
            warning.contains("public policy") && warning.contains("exact GET /status")
        }));
    }

    #[test]
    fn warns_when_public_status_executes_detailed_dependency_probes() {
        let root = temp_repo("heavy-public-status");
        write(&root, "example-gateway/src/auth/seed.rs", OK_SEED);
        write(&root, "example-gateway/src/main.rs", OK_MAIN);
        write(&root, "health-audit-console/lib/authBackend.ts", OK_CONSOLE);
        write(&root, COMPOSE_REL, OK_COMPOSE);
        write(&root, PROFILE_REL, OK_PROFILE);
        write(&root, SECRET_SET_REL, OK_SECRET_SET);
        write(&root, CARTRIDGE_MOUNT_REL, OK_CARTRIDGE_MOUNT);
        write(
            &root,
            "cartridges/health_audit/router.py",
            &OK_HEALTH_AUDIT_ROUTER.replace(
                "return {\"cartridge\": \"health_audit\", \"status\": \"ok\", \"ready\": True}",
                "ontology_ready, ontology_error = await _get_ontology_dependency_state()\n    return build_cartridge_status({})",
            ),
        );

        let result = doctor_health_audit_auth(&empty_index(), &root);

        assert!(result.warnings.iter().any(|warning| {
            warning.contains("public /status") && warning.contains("lightweight")
        }));
    }

    #[test]
    fn warns_when_secret_set_password_placeholder_is_missing() {
        let root = temp_repo("missing-secret-set");
        write(&root, "example-gateway/src/auth/seed.rs", OK_SEED);
        write(&root, "example-gateway/src/main.rs", OK_MAIN);
        write(&root, "health-audit-console/lib/authBackend.ts", OK_CONSOLE);
        write(&root, COMPOSE_REL, OK_COMPOSE);
        write(&root, PROFILE_REL, OK_PROFILE);
        write(&root, SECRET_SET_REL, "JWT_SECRET=\n");

        let result = doctor_health_audit_auth(&empty_index(), &root);

        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("HEALTH_AUDIT_AUTH_ADMIN_PASSWORD placeholder"))
        );
    }

    #[test]
    fn warns_when_password_leaks_into_tracked_profile() {
        let root = temp_repo("password-in-profile");
        write(&root, "example-gateway/src/auth/seed.rs", OK_SEED);
        write(&root, "example-gateway/src/main.rs", OK_MAIN);
        write(&root, "health-audit-console/lib/authBackend.ts", OK_CONSOLE);
        write(&root, COMPOSE_REL, OK_COMPOSE);
        write(
            &root,
            PROFILE_REL,
            "HEALTH_AUDIT_AUTH_TENANT_SLUG=pcpsaude\nHEALTH_AUDIT_AUTH_TENANT_NAME=PCP Saúde\nHEALTH_AUDIT_AUTH_ADMIN_EMAILS=admin@example.com\nHEALTH_AUDIT_AUTH_ADMIN_PASSWORD=bad\n",
        );
        write(&root, SECRET_SET_REL, OK_SECRET_SET);

        let result = doctor_health_audit_auth(&empty_index(), &root);

        assert!(result.warnings.iter().any(|warning| {
            warning.contains("tracked profile must not contain HEALTH_AUDIT_AUTH_ADMIN_PASSWORD")
        }));
    }

    #[test]
    fn warns_when_central_cartridge_auth_injection_is_missing() {
        let root = temp_repo("missing-central-auth-injection");
        write(&root, "example-gateway/src/auth/seed.rs", OK_SEED);
        write(&root, "example-gateway/src/main.rs", OK_MAIN);
        write(&root, "health-audit-console/lib/authBackend.ts", OK_CONSOLE);
        write(&root, COMPOSE_REL, OK_COMPOSE);
        write(&root, PROFILE_REL, OK_PROFILE);
        write(&root, SECRET_SET_REL, OK_SECRET_SET);
        write(
            &root,
            CARTRIDGE_MOUNT_REL,
            r#"
_PUBLIC_CARTRIDGE_PATHS = {
    "health_audit": frozenset({"/v2/health-audit/status", "/status"}),
}

def get_cartridge_router():
    router.include_router(cartridge_router)
"#,
        );

        let result = doctor_health_audit_auth(&empty_index(), &root);

        assert!(result.warnings.iter().any(|warning| {
            warning.contains("_inject_auth_dependency")
                && warning.contains("Depends(get_current_user)")
        }));
    }

    #[test]
    fn warns_when_auth_injection_runs_after_router_inclusion() {
        let root = temp_repo("late-central-auth-injection");
        write(&root, "example-gateway/src/auth/seed.rs", OK_SEED);
        write(&root, "example-gateway/src/main.rs", OK_MAIN);
        write(&root, "health-audit-console/lib/authBackend.ts", OK_CONSOLE);
        write(&root, COMPOSE_REL, OK_COMPOSE);
        write(&root, PROFILE_REL, OK_PROFILE);
        write(&root, SECRET_SET_REL, OK_SECRET_SET);
        write(
            &root,
            CARTRIDGE_MOUNT_REL,
            r#"
_PUBLIC_CARTRIDGE_PATHS = {
    "health_audit": frozenset({"/v2/health-audit/status", "/status"}),
}

def _inject_auth_dependency(cartridge_router, manifest, cartridge_name):
    from example.auth.dependencies import get_current_user

    auth_dep = Depends(get_current_user)
    for route in cartridge_router.routes:
        route.dependencies = list(route.dependencies or []) + [auth_dep]

def get_cartridge_router():
    router.include_router(cartridge_router)
    _inject_auth_dependency(cartridge_router, manifest, cartridge)
"#,
        );

        let result = doctor_health_audit_auth(&empty_index(), &root);

        assert!(result.warnings.iter().any(|warning| {
            warning.contains("_inject_auth_dependency must run before router.include_router")
        }));
    }

    #[test]
    fn warns_when_health_audit_public_allowlist_exposes_business_route() {
        let root = temp_repo("public-business-route");
        write(&root, "example-gateway/src/auth/seed.rs", OK_SEED);
        write(&root, "example-gateway/src/main.rs", OK_MAIN);
        write(&root, "health-audit-console/lib/authBackend.ts", OK_CONSOLE);
        write(&root, COMPOSE_REL, OK_COMPOSE);
        write(&root, PROFILE_REL, OK_PROFILE);
        write(&root, SECRET_SET_REL, OK_SECRET_SET);
        write(
            &root,
            CARTRIDGE_MOUNT_REL,
            &OK_CARTRIDGE_MOUNT.replace(
                "\"/v2/health-audit/status\", \"/status\"",
                "\"/v2/health-audit/status\", \"/status\", \"/v2/health-audit/contracts\"",
            ),
        );

        let result = doctor_health_audit_auth(&empty_index(), &root);

        assert!(result.warnings.iter().any(|warning| {
            warning.contains("health_audit public allowlist") && warning.contains("only /status")
        }));
    }

    #[test]
    fn warns_when_auth_injection_ignores_public_allowlist() {
        let root = temp_repo("ignored-public-allowlist");
        write(&root, "example-gateway/src/auth/seed.rs", OK_SEED);
        write(&root, "example-gateway/src/main.rs", OK_MAIN);
        write(&root, "health-audit-console/lib/authBackend.ts", OK_CONSOLE);
        write(&root, COMPOSE_REL, OK_COMPOSE);
        write(&root, PROFILE_REL, OK_PROFILE);
        write(&root, SECRET_SET_REL, OK_SECRET_SET);
        write(
            &root,
            CARTRIDGE_MOUNT_REL,
            &OK_CARTRIDGE_MOUNT.replace(
                "        if normalized_path in _PUBLIC_CARTRIDGE_PATHS.get(cartridge_name, frozenset()):\n            continue\n",
                "",
            ),
        );

        let result = doctor_health_audit_auth(&empty_index(), &root);

        assert!(result.warnings.iter().any(|warning| {
            warning.contains("public allowlist") && warning.contains("auth injection")
        }));
    }

    #[test]
    fn warns_when_compose_required_pg_password_is_missing_from_secret_set() {
        let root = temp_repo("missing-pg-password");
        write(&root, "example-gateway/src/auth/seed.rs", OK_SEED);
        write(&root, "example-gateway/src/main.rs", OK_MAIN);
        write(&root, "health-audit-console/lib/authBackend.ts", OK_CONSOLE);
        write(&root, COMPOSE_REL, OK_COMPOSE);
        write(&root, PROFILE_REL, OK_PROFILE);
        write(&root, SECRET_SET_REL, "HEALTH_AUDIT_AUTH_ADMIN_PASSWORD=\n");
        write(&root, CARTRIDGE_MOUNT_REL, OK_CARTRIDGE_MOUNT);

        let result = doctor_health_audit_auth(&empty_index(), &root);

        assert!(result.warnings.iter().any(|warning| {
            warning.contains("HEALTH_AUDIT_PG_PASSWORD") && warning.contains("canonical secret-set")
        }));
    }

    #[test]
    fn warns_when_compose_does_not_require_pg_password() {
        let root = temp_repo("optional-pg-password");
        write(&root, "example-gateway/src/auth/seed.rs", OK_SEED);
        write(&root, "example-gateway/src/main.rs", OK_MAIN);
        write(&root, "health-audit-console/lib/authBackend.ts", OK_CONSOLE);
        write(
            &root,
            COMPOSE_REL,
            &OK_COMPOSE.replace(
                "${HEALTH_AUDIT_PG_PASSWORD:?HEALTH_AUDIT_PG_PASSWORD is required}",
                "${HEALTH_AUDIT_PG_PASSWORD:-postgres}",
            ),
        );
        write(&root, PROFILE_REL, OK_PROFILE);
        write(&root, SECRET_SET_REL, OK_SECRET_SET);
        write(&root, CARTRIDGE_MOUNT_REL, OK_CARTRIDGE_MOUNT);

        let result = doctor_health_audit_auth(&empty_index(), &root);

        assert!(result.warnings.iter().any(|warning| {
            warning.contains("compose") && warning.contains("must require HEALTH_AUDIT_PG_PASSWORD")
        }));
    }
}
