//! EVO (W12) cartridge credential-isolation + route-ordering doctor.
//!
//! EVO is a live gym-ERP integration (active in 7 deploy profiles incl.
//! `customer_ops_unified` and `platform`). Two contracts were hardened on
//! 2026-06-09 after they were found unguarded and untested:
//!
//!   1. **Credential isolation** (`cartridges/evo/config.py`): the per-request
//!      Basic-Auth override (`X-Evo-Api-Username` / `X-Evo-Api-Secret`) is an
//!      atomic pair. A half-supplied override must fail closed (HTTP 400) rather
//!      than being silently completed from the process-wide `EVO_API_*` defaults
//!      — mixing one unit's DNS with another unit's Secret Key reads/charges the
//!      wrong gym. The forbidden regression is the old independent
//!      `_REQUEST_*_OVERRIDE.get() or os.environ.get(...)` resolution.
//!
//!   2. **Route ordering** (`cartridges/evo/routes/members.py`): FastAPI matches
//!      in declaration order, so the parameterised `/members/{idMember}`
//!      (idMember: int) shadows the static `/members/basic|active|services`
//!      routes and 422s on int coercion. The static routes MUST be declared
//!      before the parametric catch-all.
//!
//! Needle/position-based (model: `pacto_drain_dependency.rs`).

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::source_scan::find_awaited_function_call_line;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct EvoCredentialIsolationDoctor;

impl Doctor for EvoCredentialIsolationDoctor {
    fn name(&self) -> &'static str {
        "evo-credential-isolation"
    }

    fn description(&self) -> &'static str {
        "Checks the EVO cartridge keeps per-request Basic-Auth overrides as an atomic pair (partial override fails closed, no cross-unit env mixing) and keeps static /members routes declared before /members/{idMember} so they are not shadowed."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_evo_credential_isolation(root)
    }
}

const CONFIG_PATH: &str = "cartridges/evo/config.py";
const MEMBERS_PATH: &str = "cartridges/evo/routes/members.py";
const PRODUCTION_CALLER_PATH: &str = "jai-pay/src/lib/evo.ts";

pub fn doctor_evo_credential_isolation(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut checks_passed = 0usize;
    let checks_total = 3usize;

    // --- Check 1: credential isolation in config.py -----------------------
    let mut io_warnings = Vec::new();
    match read_text(&root.join(CONFIG_PATH), &mut io_warnings) {
        None => {
            warnings.push(format!(
                "credential isolation: {CONFIG_PATH} missing or unreadable"
            ));
            warnings.extend(io_warnings);
            evidence.push(EvidenceItem {
                kind: "evo_credential_isolation_missing_file".to_string(),
                path: CONFIG_PATH.to_string(),
                line: None,
                detail: "config.py with _build_basic_auth not found".to_string(),
            });
        }
        Some(body) => {
            // Required anchors proving the atomic-pair guard is in place.
            let required = [
                ("def _build_basic_auth", "credential builder"),
                (
                    "bool(override_username) != bool(override_secret)",
                    "atomic-pair guard",
                ),
                ("HTTP_400_BAD_REQUEST", "fail-closed status"),
            ];
            // Forbidden anchor: the old independent override||env resolution that
            // allowed cross-unit credential mixing.
            let forbidden = [
                "_REQUEST_USERNAME_OVERRIDE.get() or os.environ",
                "_REQUEST_SECRET_OVERRIDE.get() or os.environ",
            ];

            let missing: Vec<&str> = required
                .iter()
                .filter(|(needle, _)| !body.contains(needle))
                .map(|(needle, _)| *needle)
                .collect();
            let leaked: Vec<&str> = forbidden
                .iter()
                .filter(|needle| body.contains(**needle))
                .copied()
                .collect();

            if missing.is_empty() && leaked.is_empty() {
                checks_passed += 1;
            } else {
                if !missing.is_empty() {
                    warnings.push(format!(
                        "credential isolation drift in {CONFIG_PATH}: missing {}",
                        missing.join(", ")
                    ));
                    evidence.push(EvidenceItem {
                        kind: "evo_credential_isolation_missing_anchor".to_string(),
                        path: CONFIG_PATH.to_string(),
                        line: None,
                        detail: format!("missing: {}", missing.join(", ")),
                    });
                }
                if !leaked.is_empty() {
                    warnings.push(format!(
                        "credential mixing regression in {CONFIG_PATH}: forbidden {}",
                        leaked.join(", ")
                    ));
                    evidence.push(EvidenceItem {
                        kind: "evo_credential_isolation_forbidden_anchor".to_string(),
                        path: CONFIG_PATH.to_string(),
                        line: None,
                        detail: format!("forbidden: {}", leaked.join(", ")),
                    });
                }
            }
        }
    }

    // --- Check 3: production broker caller --------------------------------
    // Unit isolation is meaningless if the resolver is referenced only by its
    // tests.  The production EVO request path must load the unit binding and
    // resolve a short-lived credential before calling the provider.
    let mut io_warnings = Vec::new();
    match read_text(&root.join(PRODUCTION_CALLER_PATH), &mut io_warnings) {
        None => {
            warnings.push(format!(
                "production EVO caller: {PRODUCTION_CALLER_PATH} missing or unreadable"
            ));
            warnings.extend(io_warnings);
            evidence.push(EvidenceItem {
                kind: "evo_credential_isolation_missing_production_caller".to_string(),
                path: PRODUCTION_CALLER_PATH.to_string(),
                line: None,
                detail: "expected non-test EVO request path to resolve the scoped credential"
                    .to_string(),
            });
        }
        Some(body) => {
            let required = ["loadEvoProviderAccount(", "expectedBranchId:"];
            let missing = required
                .iter()
                .filter(|needle| !body.contains(**needle))
                .copied()
                .collect::<Vec<_>>();
            let has_resolver_call =
                find_awaited_function_call_line(&body, "resolveEvoCredential").is_some();
            if missing.is_empty() && has_resolver_call {
                checks_passed += 1;
            } else {
                let mut missing = missing;
                if !has_resolver_call {
                    missing.push("resolveEvoCredential(");
                }
                warnings.push(format!(
                    "production EVO caller drift in {PRODUCTION_CALLER_PATH}: missing {}",
                    missing.join(", ")
                ));
                evidence.push(EvidenceItem {
                    kind: "evo_credential_isolation_missing_production_caller".to_string(),
                    path: PRODUCTION_CALLER_PATH.to_string(),
                    line: None,
                    detail: format!("missing: {}", missing.join(", ")),
                });
            }
        }
    }

    // --- Check 2: static-before-parametric member route ordering ----------
    let mut io_warnings = Vec::new();
    match read_text(&root.join(MEMBERS_PATH), &mut io_warnings) {
        None => {
            warnings.push(format!(
                "route ordering: {MEMBERS_PATH} missing or unreadable"
            ));
            warnings.extend(io_warnings);
            evidence.push(EvidenceItem {
                kind: "evo_route_order_missing_file".to_string(),
                path: MEMBERS_PATH.to_string(),
                line: None,
                detail: "members.py not found".to_string(),
            });
        }
        Some(body) => {
            let param = body.find("@router.get(\"/members/{idMember}\")");
            let statics = [
                (
                    "/members/basic",
                    body.find("@router.get(\"/members/basic\")"),
                ),
                (
                    "/members/active",
                    body.find("@router.get(\"/members/active\")"),
                ),
                (
                    "/members/services",
                    body.find("@router.get(\"/members/services\")"),
                ),
            ];

            let mut ordering_ok = true;
            match param {
                None => {
                    ordering_ok = false;
                    warnings.push(format!(
                        "route ordering: {MEMBERS_PATH} no longer declares GET /members/{{idMember}}"
                    ));
                    evidence.push(EvidenceItem {
                        kind: "evo_route_order_missing_anchor".to_string(),
                        path: MEMBERS_PATH.to_string(),
                        line: None,
                        detail: "GET /members/{idMember} not found".to_string(),
                    });
                }
                Some(param_idx) => {
                    for (label, pos) in statics {
                        match pos {
                            None => {
                                ordering_ok = false;
                                warnings.push(format!(
                                    "route ordering: {MEMBERS_PATH} missing static GET {label}"
                                ));
                                evidence.push(EvidenceItem {
                                    kind: "evo_route_order_missing_anchor".to_string(),
                                    path: MEMBERS_PATH.to_string(),
                                    line: None,
                                    detail: format!("static route {label} not found"),
                                });
                            }
                            Some(static_idx) if static_idx > param_idx => {
                                ordering_ok = false;
                                warnings.push(format!(
                                    "route shadowing regression in {MEMBERS_PATH}: static GET {label} declared after /members/{{idMember}} — it will 422 on int coercion"
                                ));
                                evidence.push(EvidenceItem {
                                    kind: "evo_route_order_shadowed".to_string(),
                                    path: MEMBERS_PATH.to_string(),
                                    line: None,
                                    detail: format!("{label} shadowed by /members/{{idMember}}"),
                                });
                            }
                            Some(_) => {}
                        }
                    }
                }
            }

            if ordering_ok {
                checks_passed += 1;
            }
        }
    }

    let entities = vec![json!({
        "doctor": "evo-credential-isolation",
        "checks_passed": checks_passed,
        "checks_total": checks_total,
        "tenant": "evo",
        "credential_contract": "per-request Basic-Auth override is an atomic pair; partial override fails closed with HTTP 400; no cross-unit override/env mixing",
        "route_contract": "static /members/basic|active|services must be declared before /members/{idMember}",
        "production_caller_contract": "jai-pay/src/lib/evo.ts must resolve a credential from the scoped unit binding",
        "source": "EVO credential mixing + member-route shadowing hardened 2026-06-09",
    })];

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_evo_credential_isolation"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            "EVO credential isolation intact: overrides are atomic, member routes are ordered, and JAI Pay has a scoped production caller".to_string()
        } else {
            format!(
                "EVO credential isolation drift: {} warning(s)",
                warnings.len()
            )
        },
        confidence: if warnings.is_empty() { 0.95 } else { 0.6 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "tenant": "evo",
            "integration": "w12",
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "leio_evo_credential_isolation_{}_{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write(root: &Path, relative: &str, body: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    const VALID_CONFIG: &str = r#"
def _build_basic_auth():
    override_username = _REQUEST_USERNAME_OVERRIDE.get()
    override_secret = _REQUEST_SECRET_OVERRIDE.get()
    if bool(override_username) != bool(override_secret):
        raise HTTPException(status_code=status.HTTP_400_BAD_REQUEST, detail="...")
    if override_username and override_secret:
        username, secret = override_username, override_secret
    else:
        username = os.environ.get("EVO_API_USERNAME", "")
        secret = os.environ.get("EVO_API_SECRET", "")
    return httpx.BasicAuth(username=username, password=secret)
"#;

    // Static routes declared before the parametric catch-all.
    const VALID_MEMBERS: &str = r#"
@router.get("/members")
async def list_members(): ...

@router.get("/members/basic")
async def get_member_basic(): ...

@router.get("/members/active")
async def list_active_members(): ...

@router.get("/members/services")
async def list_member_services(): ...

@router.get("/members/{idMember}")
async def get_member(idMember: int): ...
"#;

    fn write_valid(root: &Path) {
        write(root, CONFIG_PATH, VALID_CONFIG);
        write(root, MEMBERS_PATH, VALID_MEMBERS);
        write(
            root,
            PRODUCTION_CALLER_PATH,
            "const account = await loadEvoProviderAccount(gymId);\nconst credential = await resolveEvoCredential({ expectedBranchId: account.branchId });",
        );
    }

    #[test]
    fn accepts_valid_contract() {
        let root = temp_root("valid");
        write_valid(&root);
        let envelope = doctor_evo_credential_isolation(&root);
        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
    }

    #[test]
    fn flags_credential_mixing_regression() {
        let root = temp_root("mixing");
        write_valid(&root);
        // Old, forbidden independent override||env resolution.
        write(
            &root,
            CONFIG_PATH,
            "def _build_basic_auth():\n    username = _REQUEST_USERNAME_OVERRIDE.get() or os.environ.get(\"EVO_API_USERNAME\", \"\")\n    secret = _REQUEST_SECRET_OVERRIDE.get() or os.environ.get(\"EVO_API_SECRET\", \"\")\n",
        );
        let envelope = doctor_evo_credential_isolation(&root);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("credential mixing regression")),
            "{:?}",
            envelope.warnings
        );
    }

    #[test]
    fn flags_route_shadowing_regression() {
        let root = temp_root("shadow");
        write_valid(&root);
        // Parametric route declared BEFORE the static ones (the original bug).
        write(
            &root,
            MEMBERS_PATH,
            "@router.get(\"/members/{idMember}\")\nasync def get_member(idMember: int): ...\n\n@router.get(\"/members/basic\")\nasync def get_member_basic(): ...\n\n@router.get(\"/members/active\")\nasync def list_active_members(): ...\n\n@router.get(\"/members/services\")\nasync def list_member_services(): ...\n",
        );
        let envelope = doctor_evo_credential_isolation(&root);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("route shadowing regression")),
            "{:?}",
            envelope.warnings
        );
    }

    #[test]
    fn flags_test_only_credential_resolution() {
        let root = temp_root("test_only_caller");
        write(root.as_path(), CONFIG_PATH, VALID_CONFIG);
        write(root.as_path(), MEMBERS_PATH, VALID_MEMBERS);
        write(
            root.as_path(),
            "jai-pay/src/lib/evo.test.ts",
            "resolveEvoCredential({ expectedBranchId: 4 });",
        );
        let envelope = doctor_evo_credential_isolation(&root);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("production EVO caller")),
            "{:?}",
            envelope.warnings
        );
    }

    #[test]
    fn flags_an_import_only_production_credential_resolver() {
        let root = temp_root("import_only_caller");
        write(root.as_path(), CONFIG_PATH, VALID_CONFIG);
        write(root.as_path(), MEMBERS_PATH, VALID_MEMBERS);
        write(
            root.as_path(),
            PRODUCTION_CALLER_PATH,
            "import { resolveEvoCredential } from './evo-credential-broker';\nconst account = await loadEvoProviderAccount(gymId);\nconst expectedBranchId = account.branchId;",
        );
        let envelope = doctor_evo_credential_isolation(&root);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("production EVO caller")),
            "{:?}",
            envelope.warnings
        );
    }

    #[test]
    fn flags_commented_or_locally_declared_production_resolver() {
        for (name, reference) in [
            (
                "commented_caller",
                "// const credential = await resolveEvoCredential({ expectedBranchId: account.branchId });",
            ),
            (
                "local_declaration",
                "async function resolveEvoCredential(input: unknown) { return input; }",
            ),
        ] {
            let root = temp_root(name);
            write(root.as_path(), CONFIG_PATH, VALID_CONFIG);
            write(root.as_path(), MEMBERS_PATH, VALID_MEMBERS);
            write(
                root.as_path(),
                PRODUCTION_CALLER_PATH,
                &format!(
                    "const account = await loadEvoProviderAccount(gymId);\nconst input = {{ expectedBranchId: account.branchId }};\n{reference}"
                ),
            );
            let envelope = doctor_evo_credential_isolation(&root);
            assert!(
                envelope
                    .warnings
                    .iter()
                    .any(|warning| warning.contains("production EVO caller")),
                "{:?}",
                envelope.warnings
            );
        }
    }
}
