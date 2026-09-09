use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};

/// Read text without emitting a missing-file warning. Used for alternate-path
/// probing where finding any one of N candidates is sufficient.
fn read_text_silent(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

/// Static cross-backend JWT compatibility check.
///
/// The console's `/api/auth/login` route tries Python first, then Gateway
/// (or the reverse, depending on AUTH_BACKEND_PREFERENCE). Whichever signs
/// the token must produce something the downstream service can verify, or
/// every protected request 401s. The recurring failure mode (documented in
/// memory/feedback_rust_owns_auth.md and feedback_jai_superadmin.md) is one
/// side using a different signing key or algorithm than the other.
///
/// This doctor verifies, statically:
///   1. The API defaults user tokens to ES256 while retaining bounded HS256
///      migration/service compatibility.
///   2. The gateway accepts both algorithms and normalizes role-only API
///      claims identically after either verification path.
///   3. The unified console's login route does not depend on backend-specific token
///      shapes that the other backend can't issue.
pub struct AuthJwtCompatDoctor;

impl Doctor for AuthJwtCompatDoctor {
    fn name(&self) -> &'static str {
        "auth-jwt-compat"
    }

    fn description(&self) -> &'static str {
        "Verifies ES256 user auth, HS256 service compatibility, and permission parity across API and Gateway."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_auth_jwt_compat(index, root)
    }
}

const SHARED_SECRET_ENV: &str = "JWT_SECRET";

/// Returns `(direct, via_helper)` indicating whether `AUTH_BACKEND_PREFERENCE`
/// is consulted directly by the console login route or indirectly through a
/// helper module the route imports (`authBackendPreferenceOrder`).
///
/// Either path keeps the env-driven cross-backend order intact; dropping both
/// hardcodes the ordering and is what the doctor must continue to flag.
fn cross_backend_preference_sources(route_src: &str, helper_src: Option<&str>) -> (bool, bool) {
    let direct = route_src.contains("AUTH_BACKEND_PREFERENCE");
    let via_helper = route_src.contains("authBackendPreferenceOrder")
        && helper_src.is_some_and(|src| src.contains("AUTH_BACKEND_PREFERENCE"));
    (direct, via_helper)
}

fn api_algorithm_contract(src: &str) -> (bool, bool) {
    let es256_default = src.contains("default=\"ES256\"") || src.contains("default='ES256'");
    let hs256_fallback = src.contains("JWT_LEGACY_HS256_ENABLED") && src.contains("HS256");
    (es256_default, hs256_fallback)
}

fn gateway_algorithm_contract(src: &str) -> (bool, bool, bool) {
    let accepts_hs256 = src.contains("Algorithm::HS256");
    let accepts_es256 =
        src.contains("Algorithm::ES256") && src.contains("EXAMPLE_JWT_PUBLIC_KEY_PATH");
    let normalizes_permissions = src.contains("normalize_api_permissions")
        && src
            .matches("Self::normalize_api_permissions(token_data.claims)")
            .count()
            >= 2
        && src.contains("superadmin");
    (accepts_hs256, accepts_es256, normalizes_permissions)
}

pub fn doctor_auth_jwt_compat(_index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();

    // Anchor files. Missing paths are warnings, not hard failures, so the
    // doctor stays useful across repo reorganizations.
    let api_jwt_paths = [
        "example-api/example/auth/jwt.py",
        "example-api/example/core/auth/jwt.py",
        "example-api/example/auth/__init__.py",
    ];
    let gateway_auth_paths = [
        "example-gateway/src/auth.rs",
        "example-gateway/src/auth/mod.rs",
        "example-gateway/src/auth/jwt.rs",
    ];
    let console_login_path = root.join("example-ops/src/app/api/auth/login/route.ts");

    // example-api: ES256 user tokens plus bounded HS256 migration/service support.
    let mut api_secret_evidence = false;
    let mut api_es256_evidence = false;
    let mut api_hs256_fallback_evidence = false;
    for rel in api_jwt_paths {
        let p = root.join(rel);
        if let Some(src) = read_text_silent(&p) {
            if src.contains(SHARED_SECRET_ENV) {
                api_secret_evidence = true;
                if let Some(line) = find_line(&src, SHARED_SECRET_ENV) {
                    evidence.push(EvidenceItem {
                        kind: "api-jwt-secret".to_string(),
                        path: p.display().to_string(),
                        line: Some(line),
                        detail: format!("example-api reads {SHARED_SECRET_ENV}"),
                    });
                }
            }
            let (es256_default, hs256_fallback) = api_algorithm_contract(&src);
            if es256_default {
                api_es256_evidence = true;
                if let Some(line) = find_line(&src, "ES256") {
                    evidence.push(EvidenceItem {
                        kind: "api-jwt-alg".to_string(),
                        path: p.display().to_string(),
                        line: Some(line),
                        detail: "example-api defaults user tokens to ES256".to_string(),
                    });
                }
            }
            if hs256_fallback {
                api_hs256_fallback_evidence = true;
                if let Some(line) = find_line(&src, "JWT_LEGACY_HS256_ENABLED") {
                    evidence.push(EvidenceItem {
                        kind: "api-jwt-legacy-alg".to_string(),
                        path: p.display().to_string(),
                        line: Some(line),
                        detail: "example-api retains bounded HS256 compatibility".to_string(),
                    });
                }
            }
        }
    }
    if !api_secret_evidence {
        warnings.push(format!(
            "example-api auth path does not appear to read {SHARED_SECRET_ENV}; Python and Gateway will mint incompatible JWTs"
        ));
    }
    if !api_es256_evidence {
        warnings.push("example-api auth path no longer defaults user tokens to ES256".to_string());
    }
    if !api_hs256_fallback_evidence {
        warnings.push(
            "example-api auth path lost bounded HS256 migration/service compatibility".to_string(),
        );
    }

    // example-gateway: verify both algorithms and normalize authorization claims.
    let mut gw_secret_evidence = false;
    let mut gw_hs256_evidence = false;
    let mut gw_es256_evidence = false;
    let mut gw_permission_evidence = false;
    for rel in gateway_auth_paths {
        let p = root.join(rel);
        if let Some(src) = read_text_silent(&p) {
            if src.contains(SHARED_SECRET_ENV) {
                gw_secret_evidence = true;
                if let Some(line) = find_line(&src, SHARED_SECRET_ENV) {
                    evidence.push(EvidenceItem {
                        kind: "gateway-jwt-secret".to_string(),
                        path: p.display().to_string(),
                        line: Some(line),
                        detail: format!("example-gateway reads {SHARED_SECRET_ENV}"),
                    });
                }
            }
            let (accepts_hs256, accepts_es256, normalizes_permissions) =
                gateway_algorithm_contract(&src);
            if accepts_hs256 {
                gw_hs256_evidence = true;
                if let Some(line) = find_line(&src, "HS256").or_else(|| find_line(&src, "Hs256")) {
                    evidence.push(EvidenceItem {
                        kind: "gateway-jwt-alg".to_string(),
                        path: p.display().to_string(),
                        line: Some(line),
                        detail: "example-gateway accepts HS256".to_string(),
                    });
                }
            }
            if accepts_es256 {
                gw_es256_evidence = true;
                evidence.push(EvidenceItem {
                    kind: "gateway-jwt-user-alg".to_string(),
                    path: p.display().to_string(),
                    line: find_line(&src, "EXAMPLE_JWT_PUBLIC_KEY_PATH"),
                    detail: "example-gateway accepts API-issued ES256 tokens".to_string(),
                });
            }
            if normalizes_permissions {
                gw_permission_evidence = true;
                evidence.push(EvidenceItem {
                    kind: "gateway-jwt-permissions".to_string(),
                    path: p.display().to_string(),
                    line: find_line(&src, "normalize_api_permissions"),
                    detail:
                        "role-only API claims use one permission projection after either algorithm"
                            .to_string(),
                });
            }
        }
    }
    if !gw_secret_evidence {
        warnings.push(format!(
            "example-gateway auth path does not appear to read {SHARED_SECRET_ENV}; Python-signed JWTs will fail verification"
        ));
    }
    if !gw_hs256_evidence {
        warnings.push(
            "example-gateway auth path does not accept HS256; Python-signed JWTs will fail verification".to_string(),
        );
    }
    if !gw_es256_evidence {
        warnings.push("example-gateway auth path does not accept API-issued ES256 tokens via EXAMPLE_JWT_PUBLIC_KEY_PATH".to_string());
    }
    if !gw_permission_evidence {
        warnings.push("example-gateway does not normalize API role claims after both HS256 and ES256 verification paths".to_string());
    }

    // Console login route: should not require a backend-specific field that
    // the other backend can't supply (e.g. depending on RustAuthResponse.token
    // for both paths). The check is conservative: we just verify both
    // mapPythonAuthPayload and mapRustAuthPayload exist, so a future PR that
    // removes one without removing its caller surfaces here.
    //
    // AUTH_BACKEND_PREFERENCE may be consulted directly in the route, or via a
    // helper module (e.g. `lib/authBackend.ts` exporting
    // `authBackendPreferenceOrder`). Either form keeps the env contract intact;
    // dropping both hardcodes the cross-backend order.
    let auth_backend_helper_path = root.join("example-ops/src/app/api/auth/shared.ts");
    if let Some(src) = read_text(&console_login_path, &mut warnings) {
        let has_python_mapper = src.contains("mapPythonAuthPayload");
        let has_rust_mapper = src.contains("mapRustAuthPayload");
        let helper_src = read_text_silent(&auth_backend_helper_path);
        let (has_backend_preference_direct, helper_reads_preference) =
            cross_backend_preference_sources(&src, helper_src.as_deref());
        let has_backend_preference = has_backend_preference_direct || helper_reads_preference;
        if !has_python_mapper || !has_rust_mapper {
            warnings.push(
                "example-ops login route is missing one of mapPythonAuthPayload/mapRustAuthPayload; cross-backend support is broken".to_string(),
            );
        }
        if !has_backend_preference {
            warnings.push(
                "example-ops login route no longer consults AUTH_BACKEND_PREFERENCE; cross-backend ordering is hardcoded".to_string(),
            );
        }
        evidence.push(EvidenceItem {
            kind: "console-login".to_string(),
            path: console_login_path.display().to_string(),
            line: None,
            detail: format!(
                "python_mapper={has_python_mapper}, rust_mapper={has_rust_mapper}, preference_env={has_backend_preference} (direct={has_backend_preference_direct}, helper={helper_reads_preference})"
            ),
        });
    }

    let summary = if warnings.is_empty() {
        "auth-jwt-compat: ES256 users + HS256 services agree; Gateway permission projection is algorithm-independent"
            .to_string()
    } else {
        format!(
            "auth-jwt-compat: {} drift indicator(s) — cross-backend auth at risk",
            warnings.len()
        )
    };

    let confidence = if warnings.is_empty() {
        0.95_f32
    } else {
        0.65_f32
    };
    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor.auth-jwt-compat"),
        kind: "doctor".to_string(),
        summary,
        confidence,
        entities: vec![],
        evidence,
        warnings,
        meta: Some(json!({
            "shared_secret_env": SHARED_SECRET_ENV,
            "user_algorithm": "ES256",
            "service_algorithm": "HS256",
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        api_algorithm_contract, cross_backend_preference_sources, gateway_algorithm_contract,
    };

    #[test]
    fn detects_current_api_algorithm_contract() {
        let src = r#"JWT_ALGORITHM = environment("JWT_ALGORITHM", default="ES256")
            JWT_LEGACY_HS256_ENABLED = environment("JWT_LEGACY_HS256_ENABLED", default="true")
            algorithms=["HS256"]"#;
        assert_eq!(api_algorithm_contract(src), (true, true));
    }

    #[test]
    fn detects_gateway_dual_algorithm_permission_projection() {
        let src = r#"Algorithm::HS256 Algorithm::ES256 EXAMPLE_JWT_PUBLIC_KEY_PATH
            Self::normalize_api_permissions(token_data.claims)
            Self::normalize_api_permissions(token_data.claims)
            fn normalize_api_permissions() { let role = "superadmin"; }"#;
        assert_eq!(gateway_algorithm_contract(src), (true, true, true));
    }

    #[test]
    fn rejects_permission_projection_on_only_one_algorithm_path() {
        let src = r#"Algorithm::HS256 Algorithm::ES256 EXAMPLE_JWT_PUBLIC_KEY_PATH
            Self::normalize_api_permissions(token_data.claims)
            fn normalize_api_permissions() { let role = "superadmin"; }"#;
        assert_eq!(gateway_algorithm_contract(src), (true, true, false));
    }

    #[test]
    fn direct_env_reference_counts_as_consulted() {
        let route = "const order = process.env.AUTH_BACKEND_PREFERENCE;";
        let (direct, via_helper) = cross_backend_preference_sources(route, None);
        assert!(direct, "direct env reference must be detected");
        assert!(!via_helper);
    }

    #[test]
    fn helper_indirection_counts_when_helper_reads_env() {
        let route = "import { authBackendPreferenceOrder } from '@/lib/authBackend';";
        let helper = "return process.env.AUTH_BACKEND_PREFERENCE?.trim();";
        let (direct, via_helper) = cross_backend_preference_sources(route, Some(helper));
        assert!(!direct);
        assert!(via_helper, "helper indirection must be detected");
    }

    #[test]
    fn helper_indirection_without_helper_reading_env_does_not_count() {
        let route = "import { authBackendPreferenceOrder } from '@/lib/authBackend';";
        let helper = "return ['gateway', 'python'];"; // hardcoded
        let (direct, via_helper) = cross_backend_preference_sources(route, Some(helper));
        assert!(!direct);
        assert!(
            !via_helper,
            "helper that does not read the env must NOT count as consulted"
        );
    }

    #[test]
    fn hardcoded_order_with_no_helper_use_is_flagged() {
        let route = "const order = ['gateway', 'python'];";
        let (direct, via_helper) =
            cross_backend_preference_sources(route, Some("// unused helper"));
        assert!(!direct);
        assert!(!via_helper);
    }
}
