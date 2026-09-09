use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct TenantIdentityDoctor;

impl Doctor for TenantIdentityDoctor {
    fn name(&self) -> &'static str {
        "tenant-identity"
    }

    fn description(&self) -> &'static str {
        "Checks that tenant_id remains canonical while legacy tenant aliases stay isolated at API boundaries."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_tenant_identity(index, root)
    }
}

pub fn doctor_tenant_identity(_index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let tenant_identity_path = root.join("example-api/example/tenant_identity.py");
    let ops_console_path = root.join("example-api/example/routers/ops_console.py");
    let ops_console_tests_path =
        root.join("example-api/example/tests/api/test_ops_console_tenant_normalization.py");

    let tenant_identity_src = read_text(&tenant_identity_path, &mut warnings);
    let ops_console_src = read_text(&ops_console_path, &mut warnings);
    let ops_console_tests_src = read_text(&ops_console_tests_path, &mut warnings);

    let tenant_identity_contract = tenant_identity_src.as_deref().is_some_and(|src| {
        contains_all(
            src,
            &[
                "_TENANT_KEYS = (\"tenant_id\", \"tenant\", \"tenantId\", \"namespace_id\", \"namespace\", \"namespaceId\")",
                "def extract_tenant_id(",
                "def normalize_tenant_fields(",
                "data[\"tenant_id\"] = tenant_id",
                "data[\"tenant\"] = tenant_id",
            ],
        )
    });
    let ops_imports_boundary_helpers = ops_console_src.as_deref().is_some_and(|src| {
        src.contains(
            "from example.tenant_identity import extract_tenant_id, normalize_tenant_fields",
        )
    });
    let enforced_tenant_uses_normalizer = ops_console_src.as_deref().is_some_and(|src| {
        src.contains("auth_tenant = str(extract_tenant_id(user) or \"\").strip()")
    });
    let cross_tenant_visibility_is_explicit = ops_console_src.as_deref().is_some_and(|src| {
        let helper_src =
            function_block(src, "def _has_cross_tenant_visibility").unwrap_or_default();
        contains_all(
            helper_src,
            &[
                "def _has_cross_tenant_visibility(user: dict[str, Any]) -> bool:",
                "permissions = {str(p).strip().lower() for p in user.get(\"permissions\", []) if p}",
                "return \"*\" in permissions or \"admin:full\" in permissions",
            ],
        ) && !helper_src.contains("role in")
            && !helper_src.contains("auth_tenant in")
            && !helper_src.contains("any(p.startswith(\"admin:\") for p in permissions)")
    });
    let session_builder_canonicalizes_tenant = ops_console_src.as_deref().is_some_and(|src| {
        contains_all(
            src,
            &[
                "tenant_id = _tenant_id_or_default(decoded)",
                "tenant_id=tenant_id,",
                "tenant=tenant_id,",
            ],
        )
    });
    let compatibility_alias_emission_is_explicit = ops_console_src.as_deref().is_some_and(|src| {
        contains_all(
            src,
            &[
                "def _tenant_alias_payload(tenant_id: str) -> dict[str, str]:",
                "return {\"tenant_id\": normalized, \"tenant\": normalized}",
            ],
        )
    });
    let ops_tests_cover_legacy_aliases = ops_console_tests_src.as_deref().is_some_and(|src| {
        contains_all(
            src,
            &[
                "test_tenant_id_or_default_accepts_legacy_aliases",
                "{\"namespace_id\": \"fitness_exclusive\"}",
                "test_conversation_payload_builders_emit_both_tenant_fields",
                "test_collect_known_users_backfills_tenant_id_aliases",
                "test_plain_admin_remains_bound_to_own_tenant",
                "test_cross_tenant_visibility_requires_explicit_permission",
                "test_explicit_cross_tenant_permissions_match_gateway_contract",
            ],
        )
    });
    let ops_has_direct_namespace_get = ops_console_src
        .as_deref()
        .is_some_and(|src| src.contains(".get(\"namespace") || src.contains(".get('namespace"));
    let ops_has_internal_session_tenant_reads = ops_console_src
        .as_deref()
        .is_some_and(has_session_tenant_alias_read);
    let ops_has_direct_existing_tenant_alias_reads =
        ops_console_src.as_deref().is_some_and(|src| {
            src.contains("existing.get(\"tenant\")") || src.contains("existing.get('tenant')")
        });

    push_invariant(
        &mut entities,
        &mut evidence,
        tenant_identity_contract,
        "tenant_identity_contract",
        "tenant_identity.py defines tenant_id canonicalization and compatibility aliases",
        "example-api/example/tenant_identity.py",
        &tenant_identity_src,
        "_TENANT_KEYS",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        ops_imports_boundary_helpers,
        "ops_imports_boundary_helpers",
        "ops console uses the shared tenant identity boundary helpers",
        "example-api/example/routers/ops_console.py",
        &ops_console_src,
        "from example.tenant_identity import",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        enforced_tenant_uses_normalizer,
        "enforced_tenant_uses_normalizer",
        "ops auth tenant enforcement reads aliases only through extract_tenant_id",
        "example-api/example/routers/ops_console.py",
        &ops_console_src,
        "auth_tenant = str(extract_tenant_id(user)",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        cross_tenant_visibility_is_explicit,
        "cross_tenant_visibility_is_explicit",
        "roles and tenant names remain tenant-bound; only wildcard or admin:full permissions can cross tenants",
        "example-api/example/routers/ops_console.py",
        &ops_console_src,
        "def _has_cross_tenant_visibility",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        session_builder_canonicalizes_tenant,
        "session_builder_canonicalizes_tenant",
        "conversation sessions keep tenant_id and tenant compatibility in sync",
        "example-api/example/routers/ops_console.py",
        &ops_console_src,
        "tenant_id = _tenant_id_or_default(decoded)",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        compatibility_alias_emission_is_explicit,
        "compatibility_alias_emission_is_explicit",
        "tenant compatibility alias emission is centralized",
        "example-api/example/routers/ops_console.py",
        &ops_console_src,
        "def _tenant_alias_payload",
    );
    push_invariant(
        &mut entities,
        &mut evidence,
        ops_tests_cover_legacy_aliases,
        "ops_tests_cover_legacy_aliases",
        "tenant normalization tests cover legacy aliases and emitted compatibility fields",
        "example-api/example/tests/api/test_ops_console_tenant_normalization.py",
        &ops_console_tests_src,
        "test_tenant_id_or_default_accepts_legacy_aliases",
    );

    if !tenant_identity_contract {
        warnings.push(
            "tenant_identity.py no longer exposes the expected tenant_id normalization contract"
                .to_string(),
        );
    }
    if !ops_imports_boundary_helpers {
        warnings
            .push("ops_console.py does not import the shared tenant identity helpers".to_string());
    }
    if !enforced_tenant_uses_normalizer {
        warnings.push("ops tenant enforcement should use extract_tenant_id(user) instead of ad hoc alias probing".to_string());
    }
    if !cross_tenant_visibility_is_explicit {
        warnings.push("ops tenant enforcement must derive cross-tenant visibility only from explicit wildcard or admin:full permissions".to_string());
    }
    if !session_builder_canonicalizes_tenant {
        warnings.push("conversation session builder should canonicalize tenant fields through _tenant_id_or_default".to_string());
    }
    if !compatibility_alias_emission_is_explicit {
        warnings.push(
            "tenant compatibility alias emission should stay centralized in _tenant_alias_payload"
                .to_string(),
        );
    }
    if !ops_tests_cover_legacy_aliases {
        warnings.push("ops tenant normalization tests no longer cover legacy aliases and compatibility output".to_string());
    }
    if ops_has_direct_namespace_get {
        warnings.push("ops_console.py still has direct namespace alias reads; route through extract_tenant_id instead".to_string());
    }
    if ops_has_internal_session_tenant_reads {
        warnings.push("ops_console.py still reads SessionItem.tenant internally; use SessionItem.tenant_id after normalization".to_string());
    }
    if ops_has_direct_existing_tenant_alias_reads {
        warnings.push("ops_console.py still reads existing['tenant'] directly; route stored records through _tenant_id_or_default".to_string());
    }

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_tenant_identity"),
        kind: "doctor".to_string(),
        summary: format!(
            "tenant identity contract: {} warnings across {} invariants",
            warnings.len(),
            entities.len()
        ),
        confidence: if warnings.is_empty() { 0.98 } else { 0.65 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "doctor": "tenant-identity",
            "canonical_field": "tenant_id",
            "compatibility_aliases": ["tenant", "tenantId", "namespace_id", "namespace", "namespaceId"],
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn contains_all(src: &str, needles: &[&str]) -> bool {
    needles.iter().all(|needle| src.contains(needle))
}

fn function_block<'a>(src: &'a str, signature: &str) -> Option<&'a str> {
    let start = src.find(signature)?;
    let remainder = &src[start..];
    let end = remainder
        .find("\ndef ")
        .or_else(|| {
            remainder.find(
                "\n# =============================================================================",
            )
        })
        .unwrap_or(remainder.len());
    Some(&remainder[..end])
}

fn has_session_tenant_alias_read(src: &str) -> bool {
    src.lines()
        .any(|line| line.contains("session.tenant") && !line.contains("session.tenant_id"))
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
            kind: "tenant_identity".to_string(),
            path: path.to_string(),
            line: src
                .as_deref()
                .and_then(|content| find_line(content, needle)),
            detail: detail.to_string(),
        });
    }
}
