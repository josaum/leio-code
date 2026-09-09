//! Recipient-limbo doctor.
//!
//! This catches two production drift classes that can silently strand people:
//! cartridge routers mounted outside their manifest `/v2/...` prefix, and reset
//! paths that stop clearing recipient-scoped pause / human-queue route blockers.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct RecipientLimboDoctor;

impl Doctor for RecipientLimboDoctor {
    fn name(&self) -> &'static str {
        "recipient-limbo"
    }

    fn description(&self) -> &'static str {
        "Checks cartridge `/v2` route prefixes and reset-path coverage for recipient pause / human-queue blockers."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_recipient_limbo(root)
    }
}

const INTERNAL_REL: &str = "example-api/example/routers/v2/internal.py";
const AGENT_TASKS_REL: &str = "example-api/example/agents/tasks.py";
const INTERNAL_TEST_REL: &str = "example-api/example/tests/api/test_internal_router.py";
const AGENT_TASKS_TEST_REL: &str = "example-api/example/tests/api/test_agent_loop_task.py";

pub fn doctor_recipient_limbo(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    check_cartridge_route_prefixes(root, &mut warnings, &mut evidence, &mut entities);
    check_reset_contracts(root, &mut warnings, &mut evidence, &mut entities);

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_recipient_limbo"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked recipient delivery safety contracts and cartridge v2 route prefixes, found {} warnings",
            warnings.len()
        ),
        confidence: if warnings.is_empty() { 0.95 } else { 0.6 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn check_cartridge_route_prefixes(
    root: &Path,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
    entities: &mut Vec<serde_json::Value>,
) {
    let cartridges_dir = root.join("cartridges");
    let mut scanned = 0usize;
    let mut route_prefixes = BTreeSet::new();

    let entries = match fs::read_dir(&cartridges_dir) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!(
                "cannot scan cartridges directory {}: {error}",
                cartridges_dir.display()
            ));
            return;
        }
    };

    for entry in entries.flatten() {
        let cartridge_dir = entry.path();
        if !cartridge_dir.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let toml_path = cartridge_dir.join("cartridge.toml");
        let router_path = cartridge_dir.join("router.py");
        if !toml_path.is_file() || !router_path.is_file() {
            continue;
        }

        let Some(route_prefix) = read_route_prefix(&toml_path, warnings) else {
            continue;
        };
        scanned += 1;
        route_prefixes.insert(route_prefix.clone());

        let router_body = read_text(&router_path, warnings).unwrap_or_default();
        if !route_prefix.starts_with("/v2/") {
            warnings.push(format!(
                "cartridges/{name}/cartridge.toml route_prefix `{route_prefix}` is not mounted under /v2"
            ));
        }
        if !router_body.contains(&format!("\"{route_prefix}\""))
            && !router_body.contains(&format!("'{route_prefix}'"))
        {
            warnings.push(format!(
                "cartridges/{name}/router.py does not expose manifest route_prefix `{route_prefix}`"
            ));
        }

        for stale_prefix in bare_prefixes_for(&route_prefix) {
            if router_body.contains(&format!("\"{stale_prefix}\""))
                || router_body.contains(&format!("'{stale_prefix}'"))
            {
                warnings.push(format!(
                    "cartridges/{name}/router.py still contains stale non-v2 prefix `{stale_prefix}` for `{route_prefix}`"
                ));
            }
        }

        if let Some(line) = find_line(&router_body, &route_prefix) {
            evidence.push(EvidenceItem {
                kind: "cartridge_v2_route_prefix".to_string(),
                path: rel_path(root, &router_path),
                line: Some(line),
                detail: format!("router exposes manifest route_prefix `{route_prefix}`"),
            });
        }
    }

    entities.push(json!({
        "doctor": "recipient-limbo",
        "surface": "cartridge_route_prefixes",
        "cartridges_scanned": scanned,
        "route_prefixes": route_prefixes,
    }));
}

fn check_reset_contracts(
    root: &Path,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
    entities: &mut Vec<serde_json::Value>,
) {
    let contracts = [
        SourceContract {
            rel: INTERNAL_REL,
            label: "internal reset clears all agent sessions for a recipient when no agent is specified",
            needles: &["agent_session:memory:wa:{tenant}:*:*:{user_hash}"],
        },
        SourceContract {
            rel: INTERNAL_REL,
            label: "internal reset clears specific recipient pause keys",
            needles: &["agent_recipient_pause:{tenant}:{payload.agent_id}:{user_hash}"],
        },
        SourceContract {
            rel: INTERNAL_REL,
            label: "internal reset clears wildcard recipient pause keys",
            needles: &["agent_recipient_pause:{tenant}:*:{user_hash}"],
        },
        SourceContract {
            rel: INTERNAL_REL,
            label: "internal reset clears canonical phone_user_route overrides",
            needles: &[
                "scan_user_route_projections(",
                "delete_user_route_projection(phone_line_id, payload.user_phone.strip(), client=r)",
            ],
        },
        SourceContract {
            rel: AGENT_TASKS_REL,
            label: "agent turn reset includes canonical phone_user_route key",
            needles: &["user_route_key(business_phone_id, user_phone)"],
        },
        SourceContract {
            rel: AGENT_TASKS_REL,
            label: "agent turn reset includes specific recipient pause key",
            needles: &[
                "agent_recipient_pause_pattern(tenant_id, user_phone, agent_id=pause_agent_id)",
            ],
        },
        SourceContract {
            rel: AGENT_TASKS_REL,
            label: "agent turn reset includes wildcard recipient pause key",
            needles: &["agent_recipient_pause_pattern(tenant_id, user_phone)"],
        },
        SourceContract {
            rel: INTERNAL_TEST_REL,
            label: "internal reset has regression coverage for human_queue route overrides",
            needles: &[
                "test_reset_session_clears_only_matching_user_route_overrides",
                "phone_user_route:biz_a:{user_hash}",
                "\"agent_id\": \"human_queue\"",
            ],
        },
        SourceContract {
            rel: INTERNAL_TEST_REL,
            label: "internal reset has regression coverage for recipient pause",
            needles: &["agent_recipient_pause:tenant_a:agent_a:{user_hash}"],
        },
        SourceContract {
            rel: AGENT_TASKS_TEST_REL,
            label: "agent loop reset has regression coverage for recipient pause wildcard deletion",
            needles: &[
                "test_session_reset_clears_agent_recipient_pause_by_pattern",
                "agent_recipient_pause:assurant:sara_assurant:abc",
                "scan_iter.call_args.kwargs[\"match\"].startswith(\"agent_recipient_pause:assurant:*:\")",
            ],
        },
    ];

    let mut passed = 0usize;
    for contract in &contracts {
        let path = root.join(contract.rel);
        let Some(src) = read_text(&path, warnings) else {
            continue;
        };
        let missing: Vec<&str> = contract
            .needles
            .iter()
            .copied()
            .filter(|needle| !src.contains(needle))
            .collect();
        if missing.is_empty() {
            passed += 1;
            if let Some(line) = find_line(&src, contract.needles[0]) {
                evidence.push(EvidenceItem {
                    kind: "recipient_limbo_reset_contract".to_string(),
                    path: contract.rel.to_string(),
                    line: Some(line),
                    detail: contract.label.to_string(),
                });
            }
        } else {
            warnings.push(format!(
                "{} missing recipient-limbo invariant `{}` ({})",
                contract.rel,
                contract.label,
                missing.join(", ")
            ));
        }
    }

    entities.push(json!({
        "doctor": "recipient-limbo",
        "surface": "recipient_reset_contracts",
        "contracts_checked": contracts.len(),
        "contracts_passed": passed,
    }));
}

struct SourceContract {
    rel: &'static str,
    label: &'static str,
    needles: &'static [&'static str],
}

fn read_route_prefix(path: &Path, warnings: &mut Vec<String>) -> Option<String> {
    let body = read_text(path, warnings)?;
    let value = match body.parse::<toml::Value>() {
        Ok(value) => value,
        Err(error) => {
            warnings.push(format!("{} is not valid TOML: {error}", path.display()));
            return None;
        }
    };
    value
        .get("api")
        .and_then(|api| api.get("route_prefix"))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
}

fn bare_prefixes_for(route_prefix: &str) -> Vec<String> {
    let Some(rest) = route_prefix.strip_prefix("/v2/") else {
        return Vec::new();
    };
    vec![format!("/{rest}")]
}

fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_repo(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-recipient-limbo-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn write_minimal_reset_contracts(root: &Path) {
        write(
            root,
            INTERNAL_REL,
            r#"
patterns.append(f"agent_session:memory:wa:{tenant}:*:*:{user_hash}")
patterns.append(f"agent_recipient_pause:{tenant}:{payload.agent_id}:{user_hash}")
patterns.append(f"agent_recipient_pause:{tenant}:*:{user_hash}")
for key_str in scan_user_route_projections():
    delete_user_route_projection(phone_line_id, payload.user_phone.strip(), client=r)
"#,
        );
        write(
            root,
            AGENT_TASKS_REL,
            r#"
keys.append(user_route_key(business_phone_id, user_phone))
keys.append(agent_recipient_pause_pattern(tenant_id, user_phone, agent_id=pause_agent_id))
keys.append(agent_recipient_pause_pattern(tenant_id, user_phone))
"#,
        );
        write(
            root,
            INTERNAL_TEST_REL,
            r#"
def test_reset_session_clears_only_matching_user_route_overrides(): pass
route_key = f"phone_user_route:biz_a:{user_hash}"
fake_redis.hset(route_key, {"agent_id": "human_queue"})
fake_redis.expire(route_key, 3600)
fake_redis.hset(f"agent_recipient_pause:tenant_a:agent_a:{user_hash}", {})
"#,
        );
        write(
            root,
            AGENT_TASKS_TEST_REL,
            r#"
def test_session_reset_clears_agent_recipient_pause_by_pattern(): pass
keys = ["agent_recipient_pause:assurant:sara_assurant:abc"]
assert client.scan_iter.call_args.kwargs["match"].startswith("agent_recipient_pause:assurant:*:")
"#,
        );
    }

    #[test]
    fn stale_non_v2_router_prefix_is_flagged() {
        let root = temp_repo("stale-prefix");
        write_minimal_reset_contracts(&root);
        write(
            &root,
            "cartridges/liz_cobranca/cartridge.toml",
            r#"
[api]
route_prefix = "/v2/liz-cobranca"
"#,
        );
        write(
            &root,
            "cartridges/liz_cobranca/router.py",
            r#"router = APIRouter(prefix="/liz-cobranca", tags=["liz-cobranca"])"#,
        );

        let envelope = doctor_recipient_limbo(&root);

        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("does not expose manifest route_prefix")),
            "{:?}",
            envelope.warnings
        );
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("stale non-v2 prefix `/liz-cobranca`")),
            "{:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn complete_contracts_are_silent() {
        let root = temp_repo("complete");
        write_minimal_reset_contracts(&root);
        write(
            &root,
            "cartridges/pratique_cobranca/cartridge.toml",
            r#"
[api]
route_prefix = "/v2/pratique-cobranca"
"#,
        );
        write(
            &root,
            "cartridges/pratique_cobranca/router.py",
            r#"router = APIRouter(prefix="/v2/pratique-cobranca", tags=["pratique"])"#,
        );

        let envelope = doctor_recipient_limbo(&root);

        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
        let _ = fs::remove_dir_all(root);
    }
}
