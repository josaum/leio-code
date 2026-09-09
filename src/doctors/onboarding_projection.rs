use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct OnboardingProjectionDoctor;

impl Doctor for OnboardingProjectionDoctor {
    fn name(&self) -> &'static str {
        "onboarding-projection"
    }

    fn description(&self) -> &'static str {
        "Checks the live_tenants -> whatsapp_phone_numbers/agent_configurations projection path and whether dashboards have a safe fallback when the read-model is empty."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_onboarding_projection(root)
    }
}

pub fn doctor_onboarding_projection(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let main_path = root.join("example-gateway/src/main.rs");
    let live_tenants_path = root.join("example-gateway/src/config/live_tenants.rs");
    let gym_neural_grid_path =
        root.join("example-ops/src/components/dashboard/nano/neural-grid.tsx");
    let gym_mini_stats_path =
        root.join("example-ops/src/components/dashboard/nano/mini-stats-bar.tsx");

    let main_src = read_text(&main_path, &mut warnings);
    let live_tenants_src = read_text(&live_tenants_path, &mut warnings);
    let gym_neural_grid_src = read_text(&gym_neural_grid_path, &mut warnings);
    let gym_mini_stats_src = read_text(&gym_mini_stats_path, &mut warnings);

    let gateway_runs_live_tenant_seed = main_src.as_deref().is_some_and(|src| {
        src.contains("LIVE_TENANTS_PATH")
            && src.contains("seed_live_tenants_from_json")
            && src.contains("startup.seed_live_tenants")
    });
    let live_tenants_seeds_phone_numbers = live_tenants_src.as_deref().is_some_and(|src| {
        src.contains("INSERT INTO whatsapp_phone_numbers")
            && src.contains("ON CONFLICT(phone_number_id) DO UPDATE")
    });
    let live_tenants_seeds_agent_configs = live_tenants_src.as_deref().is_some_and(|src| {
        src.contains("INSERT INTO agent_configurations")
            && src.contains("ON CONFLICT(phone_number_id) DO UPDATE")
    });
    let live_tenants_cleans_stale_rows = live_tenants_src.as_deref().is_some_and(|src| {
        src.contains("delete_not_in(db, \"agent_configurations\", \"phone_number_id\", &phone_ids)")
            && src.contains(
                "delete_not_in(db, \"whatsapp_phone_numbers\", \"phone_number_id\", &phone_ids)",
            )
    });

    let gym_dashboard_blind_if_configs_empty = dashboard_blind_if_configs_empty(
        gym_neural_grid_src.as_deref(),
        gym_mini_stats_src.as_deref(),
    );

    if !gateway_runs_live_tenant_seed {
        warnings.push(
            "gateway startup no longer clearly runs the live_tenants bootstrap path".to_string(),
        );
    }
    if !live_tenants_seeds_phone_numbers {
        warnings.push(
            "live_tenants.rs no longer clearly projects rows into whatsapp_phone_numbers"
                .to_string(),
        );
    }
    if !live_tenants_seeds_agent_configs {
        warnings.push(
            "live_tenants.rs no longer clearly projects rows into agent_configurations".to_string(),
        );
    }
    if !live_tenants_cleans_stale_rows {
        warnings
            .push("live_tenants.rs no longer cleans stale phone/agent projection rows".to_string());
    }
    if gym_dashboard_blind_if_configs_empty {
        warnings.push(
            "example-ops dashboard still renders agent cards/counts directly from agent-configs and goes blind when the read-model is empty"
                .to_string(),
        );
    }

    for (path, src, needle, detail, kind) in [
        (
            &main_path,
            main_src.as_ref(),
            "seed_live_tenants_from_json(&db_conn, &live_tenants_path)",
            "gateway startup invokes live tenant projection seed",
            "onboarding_projection",
        ),
        (
            &live_tenants_path,
            live_tenants_src.as_ref(),
            "INSERT INTO whatsapp_phone_numbers",
            "live_tenants projects canonical phone-number rows",
            "onboarding_projection",
        ),
        (
            &live_tenants_path,
            live_tenants_src.as_ref(),
            "INSERT INTO agent_configurations",
            "live_tenants projects canonical agent configuration rows",
            "onboarding_projection",
        ),
        (
            &gym_neural_grid_path,
            gym_neural_grid_src.as_ref(),
            "const agents = configsData?.agent_configs || [];",
            "example-ops agent grid is keyed off agent-configs",
            "dashboard_dependency",
        ),
        (
            &gym_mini_stats_path,
            gym_mini_stats_src.as_ref(),
            "const activeAgents = agentConfigs?.agent_configs?.filter((a: any) => a.is_enabled).length || 0;",
            "example-ops active-agent count is keyed off agent-configs",
            "dashboard_dependency",
        ),
    ] {
        if let Some(src) = src
            && let Some(line) = find_line(src, needle)
        {
            evidence.push(EvidenceItem {
                kind: kind.to_string(),
                path: path.display().to_string(),
                line: Some(line),
                detail: detail.to_string(),
            });
        }
    }

    entities.push(json!({
        "path": main_path.display().to_string(),
        "runs_live_tenant_seed": gateway_runs_live_tenant_seed,
    }));
    entities.push(json!({
        "path": live_tenants_path.display().to_string(),
        "seeds_phone_numbers": live_tenants_seeds_phone_numbers,
        "seeds_agent_configurations": live_tenants_seeds_agent_configs,
        "cleans_stale_rows": live_tenants_cleans_stale_rows,
    }));
    entities.push(json!({
        "path": gym_neural_grid_path.display().to_string(),
        "dashboard_blind_if_configs_empty": gym_dashboard_blind_if_configs_empty,
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_onboarding_projection"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked live_tenants onboarding projection and dashboard dependency; found {} warnings",
            warnings.len()
        ),
        confidence: if warnings.is_empty() { 0.98 } else { 0.71 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn dashboard_blind_if_configs_empty(
    neural_grid_src: Option<&str>,
    mini_stats_src: Option<&str>,
) -> bool {
    let grid_depends_on_configs = neural_grid_src.is_some_and(|src| {
        src.contains("queryFn: () => api.getAgentConfigs()")
            && src.contains("const agents = configsData?.agent_configs || [];")
            && src.contains("Nenhum agente configurado")
            && !src.contains("const agents = configsData?.agent_configs?.length ?")
    });
    let mini_stats_depends_on_configs = mini_stats_src.is_some_and(|src| {
        src.contains("queryFn: () => api.getAgentConfigs()")
            && src.contains(
                "const activeAgents = agentConfigs?.agent_configs?.filter((a: any) => a.is_enabled).length || 0;",
            )
    });

    grid_depends_on_configs && mini_stats_depends_on_configs
}
