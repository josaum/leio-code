use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{
    DeployTargetRecord, EvidenceItem, ProfileRecord, QueryEnvelope, RepoIndex, SecretSetRecord,
};

const REQUIRED_PROFILE_VARS: &[&str] = &[
    "VIGOROS_LLM_PROVIDER",
    "VIGOROS_LLM_MODEL",
    "VIGOROS_LLM_FALLBACK_PROVIDER",
    "VIGOROS_LLM_FALLBACK_MODEL",
    "VIGOROS_SWARM_PROVIDER",
    "VIGOROS_SWARM_MODEL",
    "VIGOROS_SWARM_EMBEDDING_MODE",
    "VIGOROS_SWARM_FALLBACK_PROVIDER",
    "VIGOROS_SWARM_FALLBACK_MODEL",
    "EXAMPLE_AGENT_MODEL",
    "VIGOROS_MILVUS_COLLECTION",
    "VIGOROS_LEIO_GLOBAL_SEARCH",
    "VIGOROS_API_AOS_SEARCH",
    "VIGOROS_LEIO_COLLECTION",
    "VIGOROS_LEIO_ARTICLE_COLLECTION",
    "VIGOROS_LEIO_TENANT_ID",
    "VIGOROS_LEIO_SOURCE_IDS",
    "EXAMPLE_KNOWLEDGE_BULK_IMPORT_ENABLED",
    "EXAMPLE_KNOWLEDGE_BULK_IMPORT_THRESHOLD",
    "EXAMPLE_KNOWLEDGE_BULK_COMMIT_EVERY",
    "MILVUS_BULK_IMPORT_ENABLED",
    "MILVUS_BULK_IMPORT_REMOTE_PATH",
];

const REQUIRED_SECRET_VARS: &[&str] = &[
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "META_ACCESS_TOKEN",
    "WHATSAPP_API_TOKEN",
    "WHATSAPP_ACCESS_TOKEN",
    "EXAMPLE_VLLM_BASE_URL",
    "EXAMPLE_INFER_HTTP_URL",
    "EXAMPLE_INFER_FLIGHT_URL",
    "MILVUS_URL",
    "MINIO_ENDPOINT",
    "MINIO_ACCESS_KEY",
    "MINIO_SECRET_KEY",
    "MINIO_BUCKET",
    "MINIO_SECURE",
];

pub struct VigorosSwarmDoctor;

impl Doctor for VigorosSwarmDoctor {
    fn name(&self) -> &'static str {
        "vigoros-swarm"
    }

    fn description(&self) -> &'static str {
        "Checks that VIGOROS swarm is wired for commercial text-mode operation with optional vLLM/Flight escalation and explicit deploy contract vars."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_vigoros_swarm(index, root)
    }
}

pub fn doctor_vigoros_swarm(index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let router_path = root.join("cartridges/vigoros/router.py");
    let generation_path = root.join("cartridges/vigoros/generation.py");
    let provider_path = root.join("example-api/example/agents/providers/vllm_provider.py");
    let caddy_path = root.join("deploy/caddy/vigoros.Caddyfile");
    let router_src = read_text(&router_path, &mut warnings);
    let generation_src = read_text(&generation_path, &mut warnings);
    let provider_src = read_text(&provider_path, &mut warnings);
    let caddy_src = read_text(&caddy_path, &mut warnings);
    // The production MCP runtime (apps-sdk → rest-runtime) projects swarm
    // roles to a crew panel; it must not invent per-role execution state.
    let runtime_path = root.join("vigoros-mcp/src/rest-runtime.ts");
    let runtime_src = read_text(&runtime_path, &mut warnings);

    let target = index
        .deploy_targets
        .iter()
        .find(|item| item.name == "vigoros");
    let profile = target
        .and_then(|item| resolve_profile(index, item))
        .or_else(|| index.profiles.iter().find(|item| item.name == "vigoros"));
    let secret_set = target
        .and_then(|item| resolve_secret_set(index, item))
        .or_else(|| index.secret_sets.iter().find(|item| item.name == "vigoros"));

    let profile_vars = profile_var_names(profile);
    let secret_vars = secret_var_names(secret_set);

    let uses_dynamic_swarm = router_src.as_deref().is_some_and(|src| {
        src.contains("from example.agents.swarm import run_dynamic_swarm")
            && (src.contains("output = await run_dynamic_swarm(")
                || src.contains("output = await _run_swarm(")
                || src.contains("return await run_dynamic_swarm("))
    });
    let embedding_mode_configurable = router_src
        .as_deref()
        .is_some_and(|src| src.contains("embedding_mode=embedding_mode"));
    let provider_configurable = router_src.as_deref().is_some_and(|src| {
        src.contains("_resolve_swarm_provider(config)") && src.contains("VIGOROS_SWARM_PROVIDER")
    });
    let text_mode_enabled = router_src
        .as_deref()
        .is_some_and(|src| src.contains("text_mode=True"));
    let uses_collection_contract = router_src
        .as_deref()
        .is_some_and(|src| src.contains("collection=config.milvus_collection"));
    let returns_dynamic_mode = router_src.as_deref().is_some_and(|src| {
        src.contains("\"dynamic_embedding_swarm\"") && src.contains("\"dynamic_text_swarm\"")
    });
    let swarm_has_fallback = router_src.as_deref().is_some_and(|src| {
        src.contains("config.llm_fallback_enabled")
            && src.contains("config.llm_fallback_provider")
            && src.contains("VIGOROS_SWARM_FALLBACK_PROVIDER")
            && src.contains("VIGOROS_SWARM_FALLBACK_MODEL")
            && src.contains("fallback_used = True")
    });
    let fallback_is_provider_agnostic = router_src.as_deref().is_some_and(|src| {
        src.contains("can_fallback = config.llm_fallback_enabled")
            && !src.contains("provider == \"vllm\"\n            and config.llm_fallback_enabled")
    });
    let batch_timeout_guard = generation_src.as_deref().is_some_and(|src| {
        src.contains("_vllm_request_timeout_seconds")
            && src.contains("asyncio.wait_for(")
            && src.contains("VIGOROS vLLM Flight request timed out")
    });
    let stream_timeout_guard = generation_src.as_deref().is_some_and(|src| {
        src.contains("_vllm_stream_first_chunk_timeout_seconds")
            && src.contains("asyncio.wait_for(")
            && src.contains("first Flight chunk")
    });
    let provider_timeout_guard = provider_src.as_deref().is_some_and(|src| {
        src.contains("DEFAULT_VLLM_FLIGHT_RPC_TIMEOUT_SEC")
            && src.contains("asyncio.wait_for(")
            && src.contains("Flight generation timed out")
    });
    let caddy_tbox_proxy_targets_mcp = caddy_src
        .as_deref()
        .is_some_and(caddy_tbox_proxy_targets_mcp);
    let runtime_reports_only_planned_roles = runtime_src
        .as_deref()
        .is_some_and(runtime_reports_only_planned_roles);

    let missing_profile_vars = missing_vars(&profile_vars, REQUIRED_PROFILE_VARS);
    let missing_secret_vars = missing_vars(&secret_vars, REQUIRED_SECRET_VARS);

    if let Some(src) = &router_src {
        for (needle, detail) in [
            (
                "@_auth_router.post(\"/ask/swarm\")",
                "VIGOROS exposes a dedicated swarm endpoint",
            ),
            (
                "return await run_dynamic_swarm(",
                "VIGOROS swarm is expected to use the dynamic swarm runner",
            ),
            (
                "_resolve_swarm_provider(config)",
                "VIGOROS swarm should resolve the configured commercial/vLLM provider explicitly",
            ),
            (
                "embedding_mode=embedding_mode",
                "VIGOROS swarm should make embedding-native context propagation configurable",
            ),
            (
                "text_mode=True",
                "VIGOROS swarm should keep text-mode inter-agent propagation active for commercial LLMs",
            ),
            (
                "config.llm_fallback_provider",
                "VIGOROS swarm should transparently fall back when vLLM is unavailable",
            ),
            (
                "VIGOROS_SWARM_FALLBACK_PROVIDER",
                "VIGOROS swarm should expose a dedicated fallback provider override",
            ),
            (
                "VIGOROS_SWARM_FALLBACK_MODEL",
                "VIGOROS swarm should expose a dedicated fallback model override",
            ),
            (
                "can_fallback = config.llm_fallback_enabled",
                "VIGOROS swarm fallback should work for commercial and vLLM providers",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "vigoros_swarm".to_string(),
                    path: router_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            }
        }
    }

    if let Some(src) = &generation_src {
        for (needle, detail) in [
            (
                "_vllm_request_timeout_seconds",
                "VIGOROS batch generation should cap vLLM Flight wait time before falling back",
            ),
            (
                "_vllm_stream_first_chunk_timeout_seconds",
                "VIGOROS stream generation should cap time-to-first-chunk before falling back",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "vigoros_swarm".to_string(),
                    path: generation_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            }
        }
    }

    if let Some(src) = &provider_src
        && let Some(line) = find_line(src, "DEFAULT_VLLM_FLIGHT_RPC_TIMEOUT_SEC")
    {
        evidence.push(EvidenceItem {
                kind: "vigoros_swarm".to_string(),
                path: provider_path.display().to_string(),
                line: Some(line),
                detail:
                    "The vLLM provider should bound Flight latency so swarm orchestration can fail over cleanly"
                        .to_string(),
            });
    }

    if let Some(src) = &caddy_src
        && let Some(line) = find_line(src, "handle /api/tbox/*")
    {
        evidence.push(EvidenceItem {
            kind: "caddy_route".to_string(),
            path: caddy_path.display().to_string(),
            line: Some(line),
            detail: "same-origin VIGOROS TBox requests should terminate on the authenticated Fly VIGOROS MCP surface"
                .to_string(),
        });
    }

    if let Some(src) = &runtime_src
        && let Some(line) = find_line(src, "agents_executed: false")
    {
        evidence.push(EvidenceItem {
            kind: "vigoros_swarm".to_string(),
            path: runtime_path.display().to_string(),
            line: Some(line),
            detail: "VIGOROS swarm roles are an operator plan: the runtime states that no agent executed instead of inventing per-role status"
                .to_string(),
        });
    }

    if let Some(target_record) = target {
        evidence.push(EvidenceItem {
            kind: "deploy_target".to_string(),
            path: target_record.path.clone(),
            line: None,
            detail:
                "deploy target `vigoros` is the authoritative deploy contract for this cartridge"
                    .to_string(),
        });
    }

    if let Some(profile_record) = profile {
        let profile_path = Path::new(&profile_record.path);
        if let Some(profile_src) = read_text(profile_path, &mut warnings) {
            for var in REQUIRED_PROFILE_VARS {
                if let Some(line) = find_line(&profile_src, &format!("{var}=")) {
                    evidence.push(EvidenceItem {
                        kind: "backend_profile".to_string(),
                        path: profile_record.path.clone(),
                        line: Some(line),
                        detail: format!("profile declares `{var}` for the VIGOROS swarm runtime"),
                    });
                }
            }
        }
    }

    if let Some(secret_record) = secret_set {
        let secret_path = Path::new(&secret_record.path);
        if let Some(secret_src) = read_text(secret_path, &mut warnings) {
            for var in REQUIRED_SECRET_VARS {
                if let Some(line) = find_line(&secret_src, &format!("{var}=")) {
                    evidence.push(EvidenceItem {
                        kind: "secret_set".to_string(),
                        path: secret_record.path.clone(),
                        line: Some(line),
                        detail: vigoros_secret_var_detail(var),
                    });
                }
            }
        }
    }

    if !uses_dynamic_swarm {
        warnings.push(
            "vigoros ask/swarm does not call `run_dynamic_swarm`; production will drift away from the configured commercial/text swarm behavior".to_string(),
        );
    }
    if !embedding_mode_configurable {
        warnings.push(
            "vigoros ask/swarm does not pass configurable `embedding_mode`; commercial/vLLM runtime selection can drift".to_string(),
        );
    }
    if !provider_configurable {
        warnings.push(
            "vigoros ask/swarm does not resolve `VIGOROS_SWARM_PROVIDER`; commercial text-mode cannot be made primary explicitly".to_string(),
        );
    }
    if !text_mode_enabled {
        warnings.push(
            "vigoros ask/swarm does not force `text_mode=True`; commercial swarms may lose inter-agent context propagation".to_string(),
        );
    }
    if !uses_collection_contract {
        warnings.push(
            "vigoros ask/swarm is not using `config.milvus_collection`; swarm retrieval may drift from the cartridge ontology collection".to_string(),
        );
    }
    if !returns_dynamic_mode {
        warnings.push(
            "vigoros ask/swarm no longer reports both `dynamic_embedding_swarm` and `dynamic_text_swarm`; frontend and ops tooling lose a stable execution marker".to_string(),
        );
    }
    if !swarm_has_fallback {
        warnings.push(
            "vigoros ask/swarm does not transparently fall back away from vLLM using the dedicated swarm fallback provider/model vars".to_string(),
        );
    }
    if !fallback_is_provider_agnostic {
        warnings.push(
            "vigoros ask/swarm fallback is still gated to vLLM only; commercial provider failures will not fail over cleanly".to_string(),
        );
    }
    if !batch_timeout_guard {
        warnings.push(
            "vigoros batch generation lacks an explicit vLLM Flight timeout; Modal brownouts may block fallback".to_string(),
        );
    }
    if !stream_timeout_guard {
        warnings.push(
            "vigoros stream generation lacks an explicit first-chunk timeout; users may wait indefinitely before fallback".to_string(),
        );
    }
    if !provider_timeout_guard {
        warnings.push(
            "the vLLM provider lacks an explicit Flight timeout; dynamic swarm orchestration may never trigger fallback on Modal brownouts".to_string(),
        );
    }
    if !caddy_tbox_proxy_targets_mcp {
        warnings.push(
            "vigoros Caddy /api/tbox/* route does not proxy to the authenticated Fly vigoros-mcp surface; same-origin TBox calls can 404 or bypass MCP auth".to_string(),
        );
    }
    if !runtime_reports_only_planned_roles {
        warnings.push(
            "vigoros-mcp rest-runtime reports per-role swarm agent status it never executed (regex/phase-divined `running`/`completed`, or `agents_spawned` counted from the role list), or lost the `agents_executed: false` marker; the crew panel would show fabricated provenance".to_string(),
        );
    }
    if !missing_profile_vars.is_empty() {
        warnings.push(format!(
            "deploy profile `vigoros` is missing explicit swarm runtime vars: {}",
            missing_profile_vars.join(", ")
        ));
    }
    if !missing_secret_vars.is_empty() {
        let gateway_missing: Vec<_> = missing_secret_vars
            .iter()
            .filter(|var| is_vigoros_gateway_egress_secret_var(var))
            .cloned()
            .collect();
        let flight_missing: Vec<_> = missing_secret_vars
            .iter()
            .filter(|var| !is_vigoros_gateway_egress_secret_var(var))
            .cloned()
            .collect();
        let mut parts = Vec::new();
        if !flight_missing.is_empty() {
            parts.push(format!(
                "Flight-backed infer runtime vars: {}",
                flight_missing.join(", ")
            ));
        }
        if !gateway_missing.is_empty() {
            parts.push(format!(
                "gateway agent egress/media token vars: {}",
                gateway_missing.join(", ")
            ));
        }
        warnings.push(format!(
            "secret set `vigoros` is missing required secret vars — {}",
            parts.join("; ")
        ));
    }

    entities.push(json!({
        "target": "vigoros",
        "uses_dynamic_swarm": uses_dynamic_swarm,
        "embedding_mode_configurable": embedding_mode_configurable,
        "provider_configurable": provider_configurable,
        "text_mode_enabled": text_mode_enabled,
        "uses_collection_contract": uses_collection_contract,
        "returns_dynamic_mode": returns_dynamic_mode,
        "swarm_has_fallback": swarm_has_fallback,
        "fallback_is_provider_agnostic": fallback_is_provider_agnostic,
        "batch_timeout_guard": batch_timeout_guard,
        "stream_timeout_guard": stream_timeout_guard,
        "provider_timeout_guard": provider_timeout_guard,
        "caddy_tbox_proxy_targets_mcp": caddy_tbox_proxy_targets_mcp,
        "profile": profile.map(|item| item.name.clone()),
        "secret_set": secret_set.map(|item| item.name.clone()),
        "missing_profile_vars": missing_profile_vars,
        "missing_secret_vars": missing_secret_vars,
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_vigoros_swarm"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            "vigoros swarm is commercial text-mode ready with optional vLLM/Flight escalation and explicit deploy vars".to_string()
        } else {
            format!("vigoros swarm contract has {} warning(s)", warnings.len())
        },
        confidence: if warnings.is_empty() { 0.96 } else { 0.71 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn resolve_profile<'a>(
    index: &'a RepoIndex,
    target: &DeployTargetRecord,
) -> Option<&'a ProfileRecord> {
    target
        .backend_profile
        .as_deref()
        .or(target.profile.as_deref())
        .and_then(|name| {
            index
                .profiles
                .iter()
                .find(|item| item.name == name || item.name == format!("{name}.env"))
        })
}

fn resolve_secret_set<'a>(
    index: &'a RepoIndex,
    target: &DeployTargetRecord,
) -> Option<&'a SecretSetRecord> {
    target.secret_set.as_deref().and_then(|name| {
        index.secret_sets.iter().find(|item| {
            item.name == name
                || item.name == format!("{name}.env")
                || item.name == format!("{name}.env.example")
        })
    })
}

fn profile_var_names(profile: Option<&ProfileRecord>) -> BTreeSet<String> {
    profile
        .map(|item| item.vars.iter().map(|var| var.name.clone()).collect())
        .unwrap_or_default()
}

fn secret_var_names(secret_set: Option<&SecretSetRecord>) -> BTreeSet<String> {
    secret_set
        .map(|item| item.vars.iter().map(|var| var.name.clone()).collect())
        .unwrap_or_default()
}

fn is_vigoros_gateway_egress_secret_var(var: &str) -> bool {
    matches!(
        var,
        "META_ACCESS_TOKEN" | "WHATSAPP_API_TOKEN" | "WHATSAPP_ACCESS_TOKEN"
    )
}

fn vigoros_secret_var_detail(var: &str) -> String {
    if is_vigoros_gateway_egress_secret_var(var) {
        format!("secret set documents `{var}` for gateway agent egress/media helpers")
    } else {
        format!("secret set documents `{var}` for the Flight-backed infer runtime")
    }
}

fn missing_vars(declared: &BTreeSet<String>, required: &[&str]) -> Vec<String> {
    required
        .iter()
        .filter(|name| !declared.contains(**name))
        .map(|name| (*name).to_string())
        .collect()
}

fn caddy_tbox_proxy_targets_mcp(src: &str) -> bool {
    let Some(start) = src.find("handle /api/tbox/*") else {
        return false;
    };
    let block = &src[start..];
    let end = block
        .find("\n\thandle ")
        .or_else(|| block.find("\nhandle "))
        .unwrap_or(block.len());
    block[..end].contains("vigoros-mcp.fly.dev")
}

/// Whether the MCP runtime reports swarm roles honestly.
///
/// Honest means no per-role status synthesized from role names or phase
/// numbers, no `agents_spawned` counted from the role list, and an explicit
/// `agents_executed: false` marker next to `planned` roles. The regexes this
/// guards against once turned a single grounding pipeline into a crew panel
/// of "running" and "completed" agents that never existed.
fn runtime_reports_only_planned_roles(src: &str) -> bool {
    let fabricates = src.contains("function agentStatusForRole(")
        || src.contains("function statusProgress(")
        || src.contains("agents_spawned: Math.min(");
    let marks_plan = src.contains("agents_executed: false") && src.contains("status: \"planned\"");
    !fabricates && marks_plan
}

#[cfg(test)]
mod tests {
    use super::{caddy_tbox_proxy_targets_mcp, runtime_reports_only_planned_roles};

    #[test]
    fn swarm_roles_must_not_carry_fabricated_status() {
        let stale = "function agentStatusForRole(role) { return phase > 3 ? \"completed\" : \"queued\"; }\n\
                     agents_spawned: Math.min(profile.roles.length, profile.maxAgents),";
        assert!(!runtime_reports_only_planned_roles(stale));

        let unmarked = "return { role, status: \"planned\", progress: 0 };";
        assert!(!runtime_reports_only_planned_roles(unmarked));

        let honest = "agents_spawned: 0,\n    agents_executed: false,\n\
                      return { role, status: \"planned\", progress: 0 };";
        assert!(runtime_reports_only_planned_roles(honest));
    }

    #[test]
    fn tbox_route_must_target_mcp_not_control_plane() {
        let stale = r#"vigoros.getjai.com {
	handle /api/tbox/* {
		uri replace /api/tbox/ /v2/vigoros/tbox/ 1
		reverse_proxy https://api.getjai.com
	}

	handle /api/* {
		reverse_proxy https://vigoros-mcp.fly.dev
	}
}"#;
        assert!(!caddy_tbox_proxy_targets_mcp(stale));

        let canonical = r#"vigoros.getjai.com {
	handle /api/tbox/* {
		reverse_proxy https://vigoros-mcp.fly.dev
	}
}"#;
        assert!(caddy_tbox_proxy_targets_mcp(canonical));
    }
}
