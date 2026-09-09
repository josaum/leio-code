use std::path::Path;
use std::time::Instant;

#[cfg(test)]
use std::fs;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct AuthBrokeringDoctor;

impl Doctor for AuthBrokeringDoctor {
    fn name(&self) -> &'static str {
        "auth-brokering"
    }

    fn description(&self) -> &'static str {
        "Guards the canonical Example auth boundary, browser session continuity, and tenant-scoped credential forwarding."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_auth_brokering(index, root)
    }
}

pub fn doctor_auth_brokering(_index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();

    let api_auth_path = root.join("example-api/example/routers/auth.py");
    let api_jwt_path = root.join("example-api/example/auth/jwt.py");
    let api_keys_path = root.join("example-api/example/auth/keys.py");
    let api_dependencies_path = root.join("example-api/example/auth/dependencies.py");
    let gym_auth_constants_path = root.join("example-ops/src/lib/auth/constants.ts");
    let gym_auth_shared_path = root.join("example-ops/src/app/api/auth/shared.ts");
    let gym_login_path = root.join("example-ops/src/app/api/auth/login/route.ts");
    let gym_refresh_path = root.join("example-ops/src/app/api/auth/refresh/route.ts");
    let forwarding_path = root.join("example-ops/src/app/api/gateway/auth-forwarding.ts");
    let forwarding_tests_path =
        root.join("example-ops/src/app/api/gateway/__tests__/auth-forwarding.test.ts");
    let refresh_tests_path =
        root.join("example-ops/src/app/api/auth/__tests__/refresh.session-continuity.test.ts");

    let mut read_warnings = Vec::new();
    let api_auth = read_text(&api_auth_path, &mut read_warnings);
    let api_jwt = read_text(&api_jwt_path, &mut read_warnings);
    let api_keys = read_text(&api_keys_path, &mut read_warnings);
    let api_dependencies = read_text(&api_dependencies_path, &mut read_warnings);
    let gym_auth_constants = read_text(&gym_auth_constants_path, &mut read_warnings);
    let gym_auth_shared = read_text(&gym_auth_shared_path, &mut read_warnings);
    let gym_login = read_text(&gym_login_path, &mut read_warnings);
    let gym_refresh = read_text(&gym_refresh_path, &mut read_warnings);
    let forwarding = read_text(&forwarding_path, &mut read_warnings);
    let forwarding_tests = read_text(&forwarding_tests_path, &mut read_warnings);
    let refresh_tests = read_text(&refresh_tests_path, &mut read_warnings);
    warnings.extend(read_warnings);

    let has_es256_jwks = api_jwt
        .as_deref()
        .zip(api_keys.as_deref())
        .is_some_and(|(jwt, keys)| has_es256_jwks_contract(jwt, keys));
    let has_refresh_session_cookies = api_auth
        .as_deref()
        .zip(gym_login.as_deref())
        .zip(gym_refresh.as_deref())
        .zip(forwarding.as_deref())
        .is_some_and(|(((api, login), refresh), forward)| {
            has_refresh_session_cookie_contract(api, login, refresh, forward)
        });
    let has_sso_materialization = api_auth
        .as_deref()
        .is_some_and(has_sso_materialization_contract);
    let has_workspace_credential_isolation = api_dependencies
        .as_deref()
        .zip(api_auth.as_deref())
        .zip(forwarding.as_deref())
        .is_some_and(|((dependencies, api), forward)| {
            has_workspace_credential_isolation(dependencies, api)
                && has_local_bff_contract(forward, gym_auth_shared.as_deref().unwrap_or_default())
        });
    let has_canonical_routes = api_auth
        .as_deref()
        .is_some_and(|api| has_canonical_auth_routes(api, root));
    let has_local_bff = forwarding
        .as_deref()
        .zip(gym_auth_shared.as_deref())
        .is_some_and(|(forward, shared)| has_local_bff_contract(forward, shared));
    let has_gym_issuer = gym_auth_constants
        .as_deref()
        .is_some_and(has_canonical_gym_issuer);
    let has_forwarding_regression_tests = forwarding_tests
        .as_deref()
        .is_some_and(has_forwarding_regression_test);
    let has_refresh_regression_tests = refresh_tests
        .as_deref()
        .is_some_and(has_refresh_regression_test);
    let legacy_product_auth_surfaces_absent = legacy_product_auth_surfaces_absent(root);

    if !has_es256_jwks {
        warnings.push(
            "example-api: JWT verification no longer prefers ES256/JWKS with bounded HS256 fallback"
                .to_string(),
        );
    }
    if !has_refresh_session_cookies {
        warnings.push(
            "auth boundary: refresh rotation, browser session cookies, or upstream token normalization is incomplete"
                .to_string(),
        );
    }
    if !has_sso_materialization {
        warnings.push(
            "example-api: OAuth/OIDC SSO callback does not link or materialize namespace-scoped users"
                .to_string(),
        );
    }
    if !has_workspace_credential_isolation {
        warnings.push(
            "auth boundary: workspace/API-key credentials are not proven tenant-scoped and BFF-isolated"
                .to_string(),
        );
    }
    if !has_canonical_routes {
        warnings.push(
            "auth boundary: canonical /v2/auth and example-ops /api/auth routes are incomplete"
                .to_string(),
        );
    }
    if !has_local_bff {
        warnings.push(
            "example-ops: local BFF does not preserve the upstream bearer cookie before local fallback"
                .to_string(),
        );
    }
    if !has_gym_issuer {
        warnings.push(
            "example-ops: JWT_CONFIG issuer/audience must remain example-api/example, not ops-console"
                .to_string(),
        );
    }
    if !has_forwarding_regression_tests {
        warnings.push(
            "example-ops: upstream/local auth forwarding regression coverage is missing"
                .to_string(),
        );
    }
    if !has_refresh_regression_tests {
        warnings.push(
            "example-ops: refresh/session continuity regression coverage is missing".to_string(),
        );
    }
    if !legacy_product_auth_surfaces_absent {
        warnings.push(
            "single UI guard: retired ops-console or jai-autopilot auth surface exists".to_string(),
        );
    }

    add_evidence(
        &mut evidence,
        &api_jwt_path,
        api_jwt.as_deref(),
        "JWT_ALGORITHM = environment(\"JWT_ALGORITHM\", default=\"ES256\")",
        "canonical API JWT algorithm defaults to ES256 with bounded migration fallback",
    );
    add_evidence(
        &mut evidence,
        &api_keys_path,
        api_keys.as_deref(),
        "def get_jwks",
        "canonical API publishes JWKS for downstream token verification",
    );
    add_evidence(
        &mut evidence,
        &api_auth_path,
        api_auth.as_deref(),
        "RefreshTokenCRUD.revoke(stored.token_id)",
        "refresh tokens rotate and revoke the prior session credential",
    );
    add_evidence(
        &mut evidence,
        &api_auth_path,
        api_auth.as_deref(),
        "@router.get(\"/oidc/{provider}/callback\")",
        "OIDC callback materializes/link accounts in the canonical control plane",
    );
    add_evidence(
        &mut evidence,
        &api_dependencies_path,
        api_dependencies.as_deref(),
        "api_key.namespace_id",
        "opaque API keys resolve through their owning namespace",
    );
    add_evidence(
        &mut evidence,
        &gym_auth_constants_path,
        gym_auth_constants.as_deref(),
        "ISSUER: 'example-api'",
        "example-ops JWT issuer remains aligned with Python/Rust verification",
    );
    add_evidence(
        &mut evidence,
        &gym_auth_shared_path,
        gym_auth_shared.as_deref(),
        "export type AuthBackend = \"gateway\" | \"python\";",
        "local BFF keeps the canonical backend preference contract",
    );
    add_evidence(
        &mut evidence,
        &forwarding_path,
        forwarding.as_deref(),
        "request.cookies.get(UPSTREAM_ACCESS_TOKEN_COOKIE)?.value",
        "gateway BFF prefers the upstream Python bearer cookie",
    );
    add_evidence(
        &mut evidence,
        &gym_refresh_path,
        gym_refresh.as_deref(),
        "tokenCookie: UPSTREAM_ACCESS_TOKEN_COOKIE",
        "Python refreshes update only the upstream browser cookie",
    );
    add_evidence(
        &mut evidence,
        &forwarding_tests_path,
        forwarding_tests.as_deref(),
        "prefers the upstream python token when present",
        "upstream/local forwarding precedence has regression coverage",
    );
    add_evidence(
        &mut evidence,
        &refresh_tests_path,
        refresh_tests.as_deref(),
        "keeps the browser session alive when the upstream python token expires",
        "refresh/session continuity has regression coverage",
    );

    let entities = vec![json!({
        "canonical_control_plane": "example-api",
        "canonical_operator_bff": "example-ops",
        "es256_jwks": has_es256_jwks,
        "refresh_session_cookies": has_refresh_session_cookies,
        "sso_materialization": has_sso_materialization,
        "workspace_credential_isolation": has_workspace_credential_isolation,
        "canonical_routes": has_canonical_routes,
        "local_bff": has_local_bff,
        "gym_jwt_issuer": has_gym_issuer,
        "forwarding_regression_tests": has_forwarding_regression_tests,
        "refresh_regression_tests": has_refresh_regression_tests,
        "legacy_product_auth_surfaces_absent": legacy_product_auth_surfaces_absent,
    })];

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_auth_brokering"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked canonical Example auth brokering, found {} warnings",
            warnings.len()
        ),
        confidence: if warnings.is_empty() { 0.98 } else { 0.7 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn add_evidence(
    evidence: &mut Vec<EvidenceItem>,
    path: &Path,
    source: Option<&str>,
    needle: &str,
    detail: &str,
) {
    if let Some(source) = source
        && let Some(line) = find_line(source, needle)
    {
        evidence.push(EvidenceItem {
            kind: "auth_brokering".to_string(),
            path: path.display().to_string(),
            line: Some(line),
            detail: detail.to_string(),
        });
    }
}

fn contains_all(source: &str, needles: &[&str]) -> bool {
    needles.iter().all(|needle| source.contains(needle))
}

fn has_es256_jwks_contract(jwt: &str, keys: &str) -> bool {
    contains_all(
        jwt,
        &[
            "JWT_ALGORITHM = environment(\"JWT_ALGORITHM\", default=\"ES256\")",
            "decode_opts = {\"algorithms\": [JWT_ALGORITHM]",
            "JWT_LEGACY_HS256_ENABLED",
            "algorithms=[\"HS256\"]",
        ],
    ) && contains_all(
        keys,
        &[
            "def get_jwks",
            "\"kty\": \"EC\"",
            "\"crv\": \"P-256\"",
            "\"alg\": \"ES256\"",
        ],
    )
}

fn has_refresh_session_cookie_contract(
    api: &str,
    login: &str,
    refresh: &str,
    forwarding: &str,
) -> bool {
    contains_all(
        api,
        &[
            "@router.post(\"/refresh\")",
            "RefreshTokenCRUD.revoke(stored.token_id)",
            "_make_cookie_response(request, token_response)",
            "_REFRESH_COOKIE_NAME",
            "@router.get(\"/me\"",
            "@router.post(\"/logout\"",
        ],
    ) && contains_all(
        login,
        &[
            "UPSTREAM_ACCESS_TOKEN_COOKIE",
            "REFRESH_TOKEN_COOKIE",
            "HttpOnly",
            "SameSite=Strict",
        ],
    ) && contains_all(
        refresh,
        &[
            "attemptPythonRefresh",
            "Authorization: `Bearer ${upstreamAccessToken}`",
            "tokenCookie: UPSTREAM_ACCESS_TOKEN_COOKIE",
            "appendExpiredAuthCookies",
            "attemptLocalSessionRefresh",
            "request.cookies.get(UPSTREAM_ACCESS_TOKEN_COOKIE)?.value ?? accessToken",
        ],
    ) && contains_all(
        forwarding,
        &[
            "attemptTokenRefresh",
            "refreshHeaders.set('Cookie', cookieHeader)",
            "refreshHeaders.set('Authorization', `Bearer ${accessToken}`)",
            "UPSTREAM_ACCESS_TOKEN_COOKIE",
            "REFRESH_TOKEN_COOKIE",
        ],
    )
}

fn has_sso_materialization_contract(api: &str) -> bool {
    contains_all(
        api,
        &[
            "@router.get(\"/oauth/{provider}/callback\")",
            "@router.get(\"/oidc/{provider}/callback\")",
            "_verify_oauth_state(state)",
            "OAuthAccountCRUD",
            "OAuthAccountCRUD.create(",
            "NamespaceCRUD.create",
            "MemberCRUD.create",
            "_make_cookie_response(request, token_response)",
        ],
    )
}

fn has_workspace_credential_isolation(dependencies: &str, api: &str) -> bool {
    contains_all(
        dependencies,
        &[
            "APIKeyCRUD.get_by_sha256",
            "api_key.namespace_id",
            "NamespaceCRUD.get_by_id",
            "tenant_id",
            "x-tenant-override",
        ],
    ) && contains_all(
        api,
        &[
            "key_record.namespace_id != namespace.id",
            "_resolve_token_identity",
            "_check_not_revoked",
        ],
    )
}

fn has_canonical_auth_routes(api: &str, root: &Path) -> bool {
    contains_all(
        api,
        &[
            "router = APIRouter(prefix=\"/v2/auth\"",
            "@router.post(\"/login\")",
            "@router.post(\"/refresh\")",
            "@router.get(\"/me\"",
            "@router.post(\"/logout\"",
            "@router.get(\"/.well-known/jwks.json\")",
        ],
    ) && [
        "example-ops/src/app/api/auth/login/route.ts",
        "example-ops/src/app/api/auth/refresh/route.ts",
        "example-ops/src/app/api/auth/me/route.ts",
        "example-ops/src/app/api/auth/logout/route.ts",
    ]
    .iter()
    .all(|path| root.join(path).is_file())
}

fn has_local_bff_contract(forwarding: &str, shared: &str) -> bool {
    contains_all(
        forwarding,
        &[
            "injectAuthFromCookie",
            "request.cookies.get(UPSTREAM_ACCESS_TOKEN_COOKIE)?.value",
            "request.cookies.get(ACCESS_TOKEN_COOKIE)?.value",
            "upstreamAccessToken ??",
            "headers.set('authorization', `Bearer ${token}`)",
            "attemptTokenRefresh",
        ],
    ) && contains_all(
        shared,
        &[
            "export type AuthBackend = \"gateway\" | \"python\";",
            "authBackendPreferenceOrder",
            "\"python\"",
            "\"gateway\"",
        ],
    )
}

fn has_canonical_gym_issuer(source: &str) -> bool {
    (source.contains("ISSUER: 'example-api'") || source.contains("ISSUER: \"example-api\""))
        && (source.contains("AUDIENCE: 'example'") || source.contains("AUDIENCE: \"example\""))
        && !source.contains("ISSUER: 'ops-console'")
        && !source.contains("ISSUER: \"ops-console\"")
}

fn has_forwarding_regression_test(source: &str) -> bool {
    contains_all(
        source,
        &[
            "prefers the upstream python token when present",
            "tokenCookie: \"upstream_access_token\"",
            "Bearer python-token",
        ],
    )
}

fn has_refresh_regression_test(source: &str) -> bool {
    contains_all(
        source,
        &[
            "keeps the browser session alive when the upstream python token expires",
            "verifyToken",
            "generateToken",
            "access_token=rotated-browser-token",
        ],
    )
}

fn legacy_product_auth_surfaces_absent(root: &Path) -> bool {
    !root.join("ops-console/package.json").is_file()
        && !root.join("jai-autopilot/package.json").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_hs256_only_jwt_contract() {
        assert!(!has_es256_jwks_contract(
            "JWT_ALGORITHM = environment(\"JWT_ALGORITHM\", default=\"HS256\")\nalgorithms=[JWT_ALGORITHM]",
            "def get_jwks(): return {\"keys\": []}",
        ));
    }

    #[test]
    fn rejects_refresh_without_rotation_and_cookie_normalization() {
        assert!(!has_refresh_session_cookie_contract(
            "@router.post(\"/refresh\")\n_make_cookie_response(request, token_response)",
            "REFRESH_TOKEN_COOKIE = COOKIE_NAMES.REFRESH_TOKEN",
            "attemptPythonRefresh(request, token)\nAuthorization: `Bearer ${token}`",
            "injectAuthFromCookie(headers, request)",
        ));
    }

    #[test]
    fn rejects_sso_without_linked_identity_materialization() {
        assert!(!has_sso_materialization_contract(
            "@router.get(\"/oidc/{provider}/callback\")\nreturn _make_cookie_response(request, token_response)",
        ));
    }

    #[test]
    fn rejects_unscoped_workspace_credentials() {
        assert!(!has_workspace_credential_isolation(
            "APIKeyCRUD.get_by_sha256(raw_key)\nreturn TokenPayload(sub=api_key.id)",
            "const token = request.cookies.get(ACCESS_TOKEN_COOKIE)?.value;",
        ));
    }

    #[test]
    fn rejects_missing_canonical_auth_routes() {
        assert!(!has_canonical_auth_routes(
            "router = APIRouter(prefix=\"/v2/auth\")\n@router.post(\"/login\")",
            Path::new("/tmp/missing-gym-auth-route"),
        ));
    }

    #[test]
    fn rejects_local_bff_that_overwrites_upstream_credentials() {
        assert!(!has_local_bff_contract(
            "export function injectAuthFromCookie(headers, request) { headers.set('authorization', `Bearer ${request.cookies.get(ACCESS_TOKEN_COOKIE)?.value}`); }",
            "export type AuthBackend = \"gateway\" | \"python\";",
        ));
    }

    #[test]
    fn rejects_legacy_ops_console_issuer() {
        assert!(!has_canonical_gym_issuer(
            "JWT_CONFIG = { ISSUER: 'ops-console', AUDIENCE: 'gateway-ops' }",
        ));
    }

    #[test]
    fn rejects_resurrection_of_retired_ui_surfaces() {
        let dir = std::env::temp_dir().join(format!(
            "leio-code-auth-brokering-ui-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("ops-console")).expect("create ops-console fixture");
        fs::write(dir.join("ops-console/package.json"), "{}").expect("write fixture");
        assert!(!legacy_product_auth_surfaces_absent(&dir));
        let _ = fs::remove_dir_all(&dir);
    }
}
