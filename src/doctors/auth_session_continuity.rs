//! Auth session-continuity doctor.
//!
//! Drift observed 2026-06-01: Example Ops could log an operator out mid-flow when
//! the upstream Python token expired, even though the browser JWT was still
//! valid. The durable contract is: login must retain the upstream refresh
//! credential, refresh must use that credential and project newly issued
//! authority into the browser session, then fall back to local renewal. Shared
//! UI auth should only force logout on real auth failures (401/403), not
//! transient refresh outages.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct AuthSessionContinuityDoctor;

impl Doctor for AuthSessionContinuityDoctor {
    fn name(&self) -> &'static str {
        "auth-session-continuity"
    }

    fn description(&self) -> &'static str {
        "Checks that UI refresh paths do not force logout when only an upstream token expires."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_auth_session_continuity(root)
    }
}

fn compact(source: &str) -> String {
    source.chars().filter(|c| !c.is_whitespace()).collect()
}

fn evidence_for(path: &Path, kind: &str, source: &str, needle: &str, detail: &str) -> EvidenceItem {
    EvidenceItem {
        kind: kind.to_string(),
        path: path.display().to_string(),
        line: find_line(source, needle),
        detail: detail.to_string(),
    }
}

pub fn doctor_auth_session_continuity(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut entities = Vec::new();

    let gym_refresh_path = root.join("example-ops/src/app/api/auth/refresh/route.ts");
    let mut example_checks_passed = 0usize;
    if let Some(source) = read_text(&gym_refresh_path, &mut warnings) {
        let packed = compact(&source);

        if source.contains("isMockFallbackEnabled") {
            warnings.push(
                "example-ops auth refresh still gates local browser-session renewal behind isMockFallbackEnabled; production upstream expiry can force logout".to_string(),
            );
        } else {
            example_checks_passed += 1;
            evidence.push(evidence_for(
                &gym_refresh_path,
                "auth_session_continuity_no_mock_gate",
                &source,
                "attemptLocalSessionRefresh",
                "local browser-session refresh is not mock-gated",
            ));
        }

        if packed.contains("attempts.push(()=>attemptLocalSessionRefresh(request,accessToken));") {
            example_checks_passed += 1;
            evidence.push(evidence_for(
                &gym_refresh_path,
                "auth_session_continuity_local_fallback",
                &source,
                "attemptLocalSessionRefresh",
                "refresh attempts include local browser JWT renewal after upstream attempts",
            ));
        } else {
            warnings.push(
                "example-ops auth refresh does not append attemptLocalSessionRefresh(request, accessToken); upstream refresh failures can still terminate active browser sessions".to_string(),
            );
        }

        if source.contains("verifyToken(accessToken)") && source.contains("generateToken({") {
            example_checks_passed += 1;
            evidence.push(evidence_for(
                &gym_refresh_path,
                "auth_session_continuity_local_rotation",
                &source,
                "verifyToken(accessToken)",
                "local refresh verifies the browser JWT and reissues a fresh token",
            ));
        } else {
            warnings.push(
                "example-ops local refresh no longer verifies the browser JWT and reissues a token"
                    .to_string(),
            );
        }

        if packed.contains("attemptPythonRefresh(request,refreshToken)")
            && packed.contains("Authorization:`Bearer${refreshToken}`")
            && !packed.contains("attemptPythonRefresh(request,upstreamAccessToken)")
        {
            example_checks_passed += 1;
            evidence.push(evidence_for(
                &gym_refresh_path,
                "auth_session_continuity_python_refresh_credential",
                &source,
                "attemptPythonRefresh(request, refreshToken)",
                "Python refresh uses the refresh credential instead of the expired access token",
            ));
        } else {
            warnings.push(
                "example-ops Python refresh does not use the Python refresh credential; role changes cannot rotate into the active session"
                    .to_string(),
            );
        }

        if source.contains("normalizeAuthUser")
            && source.contains("readString(payload.role)")
            && source.contains("readString(payload.namespace)")
        {
            example_checks_passed += 1;
            evidence.push(evidence_for(
                &gym_refresh_path,
                "auth_session_continuity_python_authority_rotation",
                &source,
                "normalizeAuthUser",
                "refreshed Python role and namespace are projected into the browser session",
            ));
        } else {
            warnings.push(
                "example-ops Python refresh does not project the refreshed role and namespace into the browser session"
                    .to_string(),
            );
        }
    }

    let login_path = root.join("example-ops/src/app/api/auth/login/route.ts");
    let mut login_checks_passed = 0usize;
    if let Some(source) = read_text(&login_path, &mut warnings) {
        let packed = compact(&source);
        if packed.contains("refresh_token:string")
            && packed.contains("refreshToken:login.refresh_token")
        {
            login_checks_passed += 1;
            evidence.push(evidence_for(
                &login_path,
                "auth_session_continuity_python_login_refresh_cookie",
                &source,
                "refreshToken: login.refresh_token",
                "Python login response retains the refresh token for browser session rotation",
            ));
        } else {
            warnings.push(
                "example-ops Python login refresh token is not retained; the active browser session cannot rotate authoritative role changes"
                    .to_string(),
            );
        }
    }

    let health_audit_me_path = root.join("health-audit-console/app/api/auth/me/route.ts");
    let mut health_audit_checks_passed = 0usize;
    if let Some(source) = read_text(&health_audit_me_path, &mut warnings) {
        let packed = compact(&source);
        if packed.contains("accessToken:typeofdata.token===\"string\"")
            && packed.contains("setAccessTokenCookie(request,response,attempt.accessToken)")
            && source.contains("Set-Cookie")
        {
            health_audit_checks_passed += 1;
            evidence.push(evidence_for(
                &health_audit_me_path,
                "auth_session_continuity_health_audit_gateway_authority_rotation",
                &source,
                "setAccessTokenCookie(request, response, attempt.accessToken)",
                "Health Audit persists the fresh gateway token returned with the current profile",
            ));
        } else {
            warnings.push(
                "health-audit-console auth profile discards the fresh gateway token; role and permission changes leave the API proxy on stale authority"
                    .to_string(),
            );
        }
    }

    let ops_core_path = root.join("packages/ops-core/src/auth/use-auth.tsx");
    let mut ops_core_checks_passed = 0usize;
    if let Some(source) = read_text(&ops_core_path, &mut warnings) {
        let packed = compact(&source);
        if packed.contains("if(!response.ok){if(response.status===401||response.status===403){") {
            ops_core_checks_passed += 1;
            evidence.push(evidence_for(
                &ops_core_path,
                "auth_session_continuity_401_403_only",
                &source,
                "response.status === 401",
                "shared UI auth only forces logout on 401/403 refresh failures",
            ));
        } else {
            warnings.push(
                "ops-core refreshToken appears to log out on non-auth refresh failures; transient backend errors can interrupt active UI sessions".to_string(),
            );
        }
    }

    let regression_path =
        root.join("example-ops/src/app/api/auth/__tests__/refresh.session-continuity.test.ts");
    let mut regression_checks_passed = 0usize;
    if let Some(source) = read_text(&regression_path, &mut warnings) {
        let required = [
            "keeps the browser session alive when the upstream python token expires",
            "AUTH_BACKEND_PREFERENCE",
            "expired-python-token",
            "browser-token",
            "verifyToken",
            "rotated-browser-token",
            "uses the Python refresh credential instead of the expired access token",
            "projects refreshed Python authority into the browser session",
            "python-refresh-token",
            "admin:full",
        ];
        let missing = required
            .iter()
            .filter(|needle| !source.contains(**needle))
            .copied()
            .collect::<Vec<_>>();
        if missing.is_empty() {
            regression_checks_passed += 1;
            evidence.push(EvidenceItem {
                kind: "auth_session_continuity_regression_test".to_string(),
                path: regression_path.display().to_string(),
                line: None,
                detail: "Example Ops covers upstream-token expiry without browser logout"
                    .to_string(),
            });
        } else {
            warnings.push(format!(
                "example-ops auth refresh continuity regression test is missing required anchors: {}",
                missing.join(", ")
            ));
        }
    }

    entities.push(json!({
        "doctor": "auth-session-continuity",
        "example_ops_checks_passed": example_checks_passed,
        "example_ops_checks_total": 5,
        "login_checks_passed": login_checks_passed,
        "login_checks_total": 1,
        "health_audit_checks_passed": health_audit_checks_passed,
        "health_audit_checks_total": 1,
        "ops_core_checks_passed": ops_core_checks_passed,
        "ops_core_checks_total": 1,
        "regression_checks_passed": regression_checks_passed,
        "regression_checks_total": 1,
        "contract": "upstream refresh credentials and authority must rotate without interrupting an active browser session",
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_auth_session_continuity"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            "auth-session-continuity: UI refresh keeps browser sessions alive across upstream token expiry".to_string()
        } else {
            format!(
                "auth-session-continuity: {} drift indicator(s) can force mid-session logout",
                warnings.len()
            )
        },
        confidence: if warnings.is_empty() { 0.94 } else { 0.66 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "primary_ui": "example-ops",
            "health_audit_ui": "health-audit-console",
            "shared_ui_auth": "packages/ops-core",
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "leio_auth_session_continuity_{}_{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_file(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn write_good_fixtures(root: &Path) {
        write_file(
            root,
            "example-ops/src/app/api/auth/refresh/route.ts",
            r#"
async function attemptLocalSessionRefresh(request: NextRequest, accessToken: string | undefined) {
  const payload = await verifyToken(accessToken);
  const token = await generateToken({
    userId: payload.userId,
    tenantId: payload.tenantId,
    email: payload.email,
    roles: payload.roles,
    permissions: payload.permissions,
  });
  return { response: token };
}
const attempts = authBackendPreferenceOrder().map((backend) => backend === "python"
  ? () => attemptPythonRefresh(request, refreshToken)
  : () => attemptGatewayRefresh(request, refreshToken));
attempts.push(() => attemptLocalSessionRefresh(request, accessToken));
async function attemptPythonRefresh(request: NextRequest, refreshToken: string | undefined) {
  const headers = { Authorization: `Bearer ${refreshToken}` };
  return headers;
}
const refreshedRole = readString(payload.role);
const refreshedTenantId = readString(payload.namespace);
const refreshedUser = normalizeAuthUser({
  fallbackRole: refreshedRole,
  fallbackTenantId: refreshedTenantId,
});
"#,
        );
        write_file(
            root,
            "example-ops/src/app/api/auth/login/route.ts",
            r#"
interface PythonLoginResponse {
  access_token: string;
  refresh_token: string;
}
function mapPythonAuthPayload(login: PythonLoginResponse) {
  return {
    token: login.access_token,
    refreshToken: login.refresh_token,
  };
}
"#,
        );
        write_file(
            root,
            "packages/ops-core/src/auth/use-auth.tsx",
            r#"
if (!response.ok) {
  if (response.status === 401 || response.status === 403) {
    invalidateRealtimeCache?.();
    persistUser(storageKey, null);
    router.replace(loginRoute);
  }
}
"#,
        );
        write_file(
            root,
            "health-audit-console/app/api/auth/me/route.ts",
            r#"
async function fetchGatewayMe() {
  const data = await res.json();
  return {
    accessToken:
      typeof data.token === "string" && data.token.length > 0
        ? data.token
        : undefined,
    profile: { id: data.id },
  };
}
function setAccessTokenCookie() {
  response.headers.append("Set-Cookie", cookie);
}
if (attempt.accessToken) {
  setAccessTokenCookie(request, response, attempt.accessToken);
}
"#,
        );
        write_file(
            root,
            "example-ops/src/app/api/auth/__tests__/refresh.session-continuity.test.ts",
            r#"
it("keeps the browser session alive when the upstream python token expires", async () => {
  process.env.AUTH_BACKEND_PREFERENCE = "python";
  const upstream = "expired-python-token";
  const browser = "browser-token";
  const verifyToken = jest.fn();
  const rotated = "rotated-browser-token";
});
it("uses the Python refresh credential instead of the expired access token", () => {
  const refresh = "python-refresh-token";
});
it("projects refreshed Python authority into the browser session", () => {
  const permission = "admin:full";
});
"#,
        );
    }

    #[test]
    fn passes_when_refresh_contract_is_intact() {
        let root = temp_root("ok");
        write_good_fixtures(&root);

        let envelope = doctor_auth_session_continuity(&root);

        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
        assert_eq!(envelope.evidence.len(), 9);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn flags_mock_gated_local_refresh() {
        let root = temp_root("mock_gate");
        write_good_fixtures(&root);
        write_file(
            &root,
            "example-ops/src/app/api/auth/refresh/route.ts",
            r#"
import { isMockFallbackEnabled } from "../shared";
async function attemptLocalSessionRefresh(request: NextRequest, accessToken: string | undefined) {
  if (!isMockFallbackEnabled()) {
    return { error: "local:disabled" };
  }
  const payload = await verifyToken(accessToken);
  const token = await generateToken({ userId: payload.userId });
  return { response: token };
}
const attempts = [];
attempts.push(() => attemptLocalSessionRefresh(request, accessToken));
"#,
        );

        let envelope = doctor_auth_session_continuity(&root);

        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("isMockFallbackEnabled")),
            "{:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn flags_eager_logout_on_transient_refresh_failure() {
        let root = temp_root("eager_logout");
        write_good_fixtures(&root);
        write_file(
            &root,
            "packages/ops-core/src/auth/use-auth.tsx",
            r#"
if (!response.ok) {
  invalidateRealtimeCache?.();
  persistUser(storageKey, null);
  setState({ user: null, isAuthenticated: false });
  router.replace(loginRoute);
}
"#,
        );

        let envelope = doctor_auth_session_continuity(&root);

        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("non-auth refresh failures")),
            "{:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn flags_python_refresh_that_reuses_the_access_token() {
        let root = temp_root("python_access_token");
        write_good_fixtures(&root);
        write_file(
            &root,
            "example-ops/src/app/api/auth/refresh/route.ts",
            r#"
async function attemptLocalSessionRefresh(request: NextRequest, accessToken: string | undefined) {
  const payload = await verifyToken(accessToken);
  const token = await generateToken({ userId: payload.userId });
  return { response: token };
}
async function attemptPythonRefresh(request: NextRequest, upstreamAccessToken: string | undefined) {
  const headers = { Authorization: `Bearer ${upstreamAccessToken}` };
  return headers;
}
const attempts = authBackendPreferenceOrder().map((backend) => backend === "python"
  ? () => attemptPythonRefresh(request, upstreamAccessToken)
  : () => attemptGatewayRefresh(request, refreshToken));
attempts.push(() => attemptLocalSessionRefresh(request, accessToken));
"#,
        );

        let envelope = doctor_auth_session_continuity(&root);

        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("Python refresh credential")),
            "{:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn flags_python_login_that_discards_the_refresh_token() {
        let root = temp_root("python_login_refresh_token");
        write_good_fixtures(&root);
        write_file(
            &root,
            "example-ops/src/app/api/auth/login/route.ts",
            r#"
interface PythonLoginResponse {
  access_token: string;
  token_type: string;
}
function mapPythonAuthPayload(login: PythonLoginResponse) {
  return { token: login.access_token };
}
"#,
        );

        let envelope = doctor_auth_session_continuity(&root);

        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("Python login refresh token")),
            "{:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn flags_health_audit_profile_that_discards_the_fresh_gateway_token() {
        let root = temp_root("health_audit_gateway_token");
        write_good_fixtures(&root);
        write_file(
            &root,
            "health-audit-console/app/api/auth/me/route.ts",
            r#"
async function fetchGatewayMe(token: string) {
  const res = await fetch("/api/auth/me", {
    headers: { Authorization: `Bearer ${token}` },
  });
  const data = await res.json();
  return { profile: { id: data.id, permissions: data.permissions } };
}
export async function GET() {
  const attempt = await fetchGatewayMe("stale-token");
  return NextResponse.json(attempt.profile);
}
"#,
        );

        let envelope = doctor_auth_session_continuity(&root);

        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("fresh gateway token")),
            "{:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(root);
    }
}
