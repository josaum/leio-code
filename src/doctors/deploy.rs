use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

use regex::Regex;
use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::deploy_support::{
    command_references_existing_path, extract_rollback_target, extract_smoke_target,
    readiness_lineage_matches,
};
use crate::model::{
    DeclaredVar, EvidenceItem, ProfileRecord, QueryEnvelope, RepoIndex, SecretSetRecord,
};

pub struct DeployDoctor;

impl Doctor for DeployDoctor {
    fn name(&self) -> &'static str {
        "deploy"
    }

    fn description(&self) -> &'static str {
        "Validates deploy target/profile/secret/smoke/rollback integrity."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_deploy(index, root)
    }
}

pub fn doctor_deploy(index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let deploy_script_path = root.join("deploy/scripts/deploy.sh");
    let pull_restart_path = root.join("deploy/scripts/pull-and-restart.sh");
    let rollback_path = root.join("deploy/scripts/rollback.sh");
    let common_path = root.join("deploy/lib/common.sh");
    let deploy_platform_path = root.join("deploy/scripts/deploy-platform.sh");
    let deploy_cloud_path = root.join("deploy/scripts/deploy-cloud.sh");
    let deploy_build_remote_path = root.join("deploy/scripts/deploy-build-remote.sh");
    let deploy_vigoros_path = root.join("deploy/scripts/deploy-vigoros.sh");
    let build_images_path = root.join("deploy/scripts/build-images.sh");
    let build_all_path = root.join("deploy/scripts/build-all.sh");
    let watch_path = root.join("deploy/scripts/watch.sh");
    let bootstrap_path = root.join("deploy/bootstrap.sh");
    let defaults_env_path = root.join("deploy/defaults.env");
    let health_audit_profile_path = root.join("deploy/profiles/health_audit.env");
    let secrets_template_path = root.join("deploy/secrets.env.example");
    let api_main_path = root.join("example-api/example/main.py");
    let main_compose_path = root.join("example-api/docker-compose.yml");
    let health_audit_compose_path = root.join("example-api/docker-compose.health-audit.yml");
    let optimized_compose_path = root.join("example-api/docker-compose.yml.optimized");
    let ui_dev_compose_path = root.join("example-api/docker-compose.ui-dev.yml");
    let remote_compose_path = root.join("example-api/docker-compose.remote.yml");
    let api_docker_path = root.join("example-api/Dockerfile");
    let gateway_docker_path = root.join("example-gateway/Dockerfile");
    let align_docker_path = root.join("example-align/Dockerfile");

    let deploy_script = read_text(&deploy_script_path, &mut warnings);
    let pull_restart_script = read_text(&pull_restart_path, &mut warnings);
    let rollback_script = read_text(&rollback_path, &mut warnings);
    let common_script = read_text(&common_path, &mut warnings);
    let deploy_platform_script = read_text(&deploy_platform_path, &mut warnings);
    let deploy_cloud_script = read_optional_text(&deploy_cloud_path, &mut warnings);
    let deploy_build_remote_script = read_text(&deploy_build_remote_path, &mut warnings);
    let deploy_vigoros_script = read_text(&deploy_vigoros_path, &mut warnings);
    let build_images_script = read_text(&build_images_path, &mut warnings);
    let build_all_script = read_text(&build_all_path, &mut warnings);
    let watch_script = read_text(&watch_path, &mut warnings);
    let bootstrap_script = read_text(&bootstrap_path, &mut warnings);
    let defaults_env = read_text(&defaults_env_path, &mut warnings);
    let health_audit_profile = read_text(&health_audit_profile_path, &mut warnings);
    let secrets_template = read_text(&secrets_template_path, &mut warnings);
    let api_main = read_text(&api_main_path, &mut warnings);
    let main_compose = read_text(&main_compose_path, &mut warnings);
    let health_audit_compose = read_text(&health_audit_compose_path, &mut warnings);
    let optimized_compose = read_optional_text(&optimized_compose_path, &mut warnings);
    let ui_dev_compose = read_optional_text(&ui_dev_compose_path, &mut warnings);
    let remote_compose = read_text(&remote_compose_path, &mut warnings);
    let api_docker = read_text(&api_docker_path, &mut warnings);
    let gateway_docker = read_text(&gateway_docker_path, &mut warnings);
    let align_docker = read_text(&align_docker_path, &mut warnings);
    for contract in [
        main_compose_path.clone(),
        root.join("example-api/docker-compose.minimal.yml"),
    ] {
        validate_redis_memory_contract(&contract, &mut warnings, &mut evidence);
    }

    let mut allowed_profile_keys = defaults_env
        .as_deref()
        .map(parse_env_keys)
        .unwrap_or_default();
    allowed_profile_keys.extend(
        [
            "EXAMPLE_ACTIVE_CARTRIDGES",
            "EXAMPLE_ROUTER_PROFILE",
            "EXAMPLE_CELERY_PROFILE",
            "COMPOSE_PROJECT_NAME",
            "COMPOSE_PROFILES",
            "EXAMPLE_SKIP_HOTPATH_SYNC_STARTUP",
            "HEALTH_AUDIT_AUTH_TENANT_SLUG",
            "HEALTH_AUDIT_AUTH_TENANT_NAME",
            "HEALTH_AUDIT_AUTH_ADMIN_EMAILS",
            // Consumed by the customer-ops routing observer; it is a
            // profile-scoped path, not a secret value.
            "PACTO_WEBHOOK_OBSERVED_PATH",
            // Consumed by deploy/lib/common.sh (deploy-time contract sync gate),
            // not by any compose file, so the compose-reference scan can't see it.
            "HEALTH_AUDIT_SYNC_CONTRACTS_ON_DEPLOY",
        ]
        .into_iter()
        .map(str::to_string),
    );
    for compose_src in [
        root.join("example-api/docker-compose.yml"),
        root.join("example-api/docker-compose.minimal.yml"),
        // Both SISFRON topologies declare their connected/air-gapped operational
        // keys (provider, offline/airgap mode, egress) as `${VAR:-default}`
        // substitutions in their own compose. Scan both so those profile keys are
        // recognized as canonical, not flagged as free-floating drift.
        root.join("deploy/docker/docker-compose.sisfron-airgap.yml"),
        root.join("deploy/docker/docker-compose.sisfron-local.yml"),
        // The health-audit overlay declares its ollama sizing/sync keys
        // (`HEALTH_AUDIT_OLLAMA_*`, `HEALTH_AUDIT_SYNC_CONTRACTS_ON_DEPLOY`) the
        // same way; they moved from defaults.env into the health_audit/sentinel
        // profiles during the 2026-06 target-separation fix.
        root.join("example-api/docker-compose.health-audit.yml"),
    ]
    .into_iter()
    .filter_map(|path| read_optional_text(&path, &mut warnings))
    {
        allowed_profile_keys.extend(parse_env_reference_keys(&compose_src));
    }
    let frontend_runtime = deploy_script
        .as_deref()
        .map(parse_frontend_runtime_mappings)
        .unwrap_or_default();
    let global_secret_keys = secrets_template
        .as_deref()
        .map(parse_env_keys)
        .unwrap_or_default();
    let compose_image_vars = main_compose
        .as_deref()
        .map(parse_env_reference_keys)
        .unwrap_or_default()
        .into_iter()
        .filter(|key| is_deploy_image_key(key))
        .collect::<BTreeSet<_>>();
    let materialized_runtime_vars = common_script
        .as_deref()
        .map(parse_runtime_materialized_env_keys)
        .unwrap_or_default();
    let missing_runtime_image_vars = compose_image_vars
        .difference(&materialized_runtime_vars)
        .cloned()
        .collect::<Vec<_>>();

    let profiles_by_name: HashMap<&str, &ProfileRecord> = index
        .profiles
        .iter()
        .map(|record| (record.name.as_str(), record))
        .collect();
    let secrets_by_name: HashMap<&str, &SecretSetRecord> = index
        .secret_sets
        .iter()
        .map(|record| (record.name.as_str(), record))
        .collect();

    let deploy_requires_secret_bundle = deploy_script.as_deref().is_some_and(|src| {
        src.contains("secret_set=\"$(target_manifest_value \"$TARGET\" \"secret_set\")\"")
            && src.contains(
                "err \"No local secret bundle found for secret set '$secret_set' (expected .env or .env.local)\"",
            )
    });
    let common_exposes_target_runtime_helpers = common_script.as_deref().is_some_and(|src| {
        src.contains("rewrite_local_url_for_host() {")
            && src.contains("run_target_health_checks() {")
            && src.contains("run_target_smoke_suite() {")
            && src.contains("require_legacy_deploy_opt_in() {")
    });
    let common_supports_secret_overrides = common_script
        .as_deref()
        .is_some_and(common_script_supports_secret_overrides);
    let common_supports_pacto_credentials_preflight = common_script
        .as_deref()
        .is_some_and(common_script_supports_pacto_credentials_preflight);
    let common_supports_infobip_credentials_preflight = common_script
        .as_deref()
        .is_some_and(common_script_supports_infobip_credentials_preflight);
    let common_supports_flight_broker_preflight = common_script
        .as_deref()
        .is_some_and(common_script_supports_flight_broker_preflight);
    let common_supports_flight_tenant_hmac_preflight = common_script
        .as_deref()
        .is_some_and(common_script_supports_flight_tenant_hmac_preflight);
    let deploy_reuses_common_target_helpers = deploy_script.as_deref().is_some_and(|src| {
        src.contains("source \"$(dirname \"$0\")/../lib/common.sh\"")
            && !src.contains("run_target_health_checks() {")
            && !src.contains("run_target_smoke_suite() {")
            && src.contains("report_runtime_state_contract \"$TARGET\" \"$active_cartridges\"")
            && src.contains("sync_target_runtime_inputs \"$TARGET\" \"$active_cartridges\"")
            && src.contains("sync_to_vm \"$DEFAULTS_ENV\" \"/opt/example/\"")
            && src.contains("sync_to_vm \"$env_path\" \"/opt/example/\"")
            && src.contains(
                "materialize_vm_runtime_env \"$(basename \"$env_path\")\" \"$secret_remote_path\"",
            )
            && src.contains("run_target_health_checks \"$TARGET\" \"$vm_ip\"")
            && src.contains("run_target_smoke_suite \"$TARGET\" \"http://$vm_ip\"")
    });
    let deploy_syncs_secret_overrides = deploy_script
        .as_deref()
        .is_some_and(deploy_script_syncs_secret_overrides);
    let deploy_preflights_pacto_credentials = deploy_script
        .as_deref()
        .is_some_and(deploy_script_preflights_pacto_credentials);
    let deploy_preflights_infobip_credentials = deploy_script
        .as_deref()
        .is_some_and(deploy_script_preflights_infobip_credentials);
    let deploy_preflights_flight_broker = deploy_script
        .as_deref()
        .is_some_and(deploy_script_preflights_flight_broker);
    let deploy_preflights_flight_tenant_hmac = deploy_script
        .as_deref()
        .is_some_and(deploy_script_preflights_flight_tenant_hmac);
    let deploy_runs_manifest_smoke_suite = deploy_script
        .as_deref()
        .is_some_and(|src| src.contains("run_target_smoke_suite \"$TARGET\" \"http://$vm_ip\""));
    let pull_restart_reuses_manifest_runtime_contract =
        pull_restart_script.as_deref().is_some_and(|src| {
            src.contains("SECRET_SET=\"$(target_manifest_value \"$TARGET\" \"secret_set\")\"")
                && src.contains(
                    "SECRET_BUNDLE=\"$(resolve_secret_bundle_file \"$SECRET_SET\" || true)\"",
                )
                && src.contains("ENV_PATH=\"$(validate_env_file \"$ENV_FILE\")\"")
                && src.contains("report_runtime_state_contract \"$TARGET\" \"$ACTIVE_CARTRIDGES\"")
                && src.contains("sync_target_runtime_inputs \"$TARGET\" \"$ACTIVE_CARTRIDGES\"")
                && src.contains("sync_to_vm \"$DEFAULTS_ENV\" \"/opt/example/\"")
                && src.contains("sync_to_vm \"$ENV_PATH\" \"/opt/example/\"")
                && src.contains(
                    "materialize_vm_runtime_env \"$(basename \"$ENV_PATH\")\" \"$REMOTE_SECRET_BUNDLE\"",
                )
                && src.contains("if [[ -n \"$TARGET\" ]]; then")
                && src.contains("run_target_health_checks \"$TARGET\" \"$VM_IP\"")
                && src.contains("run_target_smoke_suite \"$TARGET\" \"http://$VM_IP\"")
        });
    let pull_restart_syncs_secret_overrides = pull_restart_script
        .as_deref()
        .is_some_and(pull_restart_script_syncs_secret_overrides);
    let pull_restart_preflights_pacto_credentials = pull_restart_script
        .as_deref()
        .is_some_and(pull_restart_script_preflights_pacto_credentials);
    let pull_restart_preflights_infobip_credentials = pull_restart_script
        .as_deref()
        .is_some_and(pull_restart_script_preflights_infobip_credentials);
    let pull_restart_preflights_flight_broker = pull_restart_script
        .as_deref()
        .is_some_and(pull_restart_script_preflights_flight_broker);
    let pull_restart_preflights_flight_tenant_hmac = pull_restart_script
        .as_deref()
        .is_some_and(pull_restart_script_preflights_flight_tenant_hmac);
    let ocr_sidecar_waits_for_redis = main_compose
        .as_deref()
        .and_then(|src| extract_compose_service_block(src, "ocr-sidecar"))
        .is_some_and(|block| {
            block.contains("depends_on:")
                && block.contains("redis:")
                && block.contains("condition: service_healthy")
        });
    let ocr_sidecar_has_http_watchdog = main_compose
        .as_deref()
        .and_then(|src| extract_compose_service_block(src, "ocr-sidecar"))
        .is_some_and(|block| {
            block.contains("GATEWAY_HTTP_WATCHDOG_ENABLED")
                && block.contains("GATEWAY_HTTP_WATCHDOG_MAX_FAILURES")
        });
    let common_supports_target_cartridge_runtime_inputs =
        common_script.as_deref().is_some_and(|src| {
            src.contains("target_runtime_uses_cartridge() {")
                && src.contains("target_manifest_has_value \"$target_name\" \"cartridges\" \"$cartridge\"")
                && src.contains("sync_ocr_model_bundle_to_vm() {")
                && src.contains(
                    "target_runtime_uses_cartridge \"$target_name\" \"$active_cartridges\" \"health_audit\"",
                )
                && src.contains(
                    "target_runtime_uses_cartridge \"$target_name\" \"$active_cartridges\" \"assurant\"",
                )
        });
    let common_guards_health_audit_gcp_path = common_script.as_deref().is_some_and(|src| {
        src.contains("guard_gcp_deploy_target_allows_cartridges() {")
            && src.contains("runtime_provider\"")
            && src.contains("health_audit is AWS-only")
            && src.contains("EXAMPLE_ALLOW_HEALTH_AUDIT_GCP")
    });
    let deploy_calls_health_audit_gcp_guard = deploy_script.as_deref().is_some_and(|src| {
        src.contains("guard_gcp_deploy_target_allows_cartridges \"$TARGET\" \"$active_cartridges\"")
    });
    let pull_restart_calls_health_audit_gcp_guard =
        pull_restart_script.as_deref().is_some_and(|src| {
            src.contains(
                "guard_gcp_deploy_target_allows_cartridges \"$TARGET\" \"$ACTIVE_CARTRIDGES\"",
            )
        });
    let health_audit_compose_propagates_ocr_auto_download = health_audit_compose
        .as_deref()
        .and_then(|src| extract_compose_service_block(src, "ocr-sidecar"))
        .is_some_and(|block| block.contains("OCR_AUTO_DOWNLOAD: ${OCR_AUTO_DOWNLOAD:-true}"));
    let api_main_disables_public_runtime_docs = api_main.as_deref().is_some_and(|src| {
        src.contains("def _runtime_docs_routes(")
            && src.contains("EXAMPLE_DISABLE_RUNTIME_DOCS")
            && (src.contains("FastAPI(lifespan=lifespan, **_runtime_docs_routes())")
                || (src.contains("_register_gated_docs(app)")
                    && src.contains("docs_url=None, redoc_url=None, openapi_url=None")))
    });
    let health_audit_runtime_docs_are_private = defaults_env.as_deref().is_some_and(|src| {
        src.contains("EXAMPLE_PUBLIC_RUNTIME_DOCS=${EXAMPLE_PUBLIC_RUNTIME_DOCS:-true}")
    }) && health_audit_profile
        .as_deref()
        .is_some_and(|src| src.contains("EXAMPLE_PUBLIC_RUNTIME_DOCS=false"))
        && health_audit_compose.as_deref().is_some_and(|src| {
            src.contains("EXAMPLE_PUBLIC_RUNTIME_DOCS: ${EXAMPLE_PUBLIC_RUNTIME_DOCS:-false}")
        })
        && api_main_disables_public_runtime_docs;
    let deploy_waits_gateway_in_default_path = deploy_script
        .as_deref()
        .is_some_and(|src| src.contains("wait_for_vm_health \"http://127.0.0.1:9382/health\" 120"));
    let pull_restart_waits_gateway_in_default_path = pull_restart_script
        .as_deref()
        .is_some_and(|src| src.contains("wait_for_vm_health \"http://127.0.0.1:9382/health\" 120"));
    let rollback_restores_previous_release_snapshot =
        rollback_script.as_deref().is_some_and(|src| {
            src.contains("[[ -n \"$TARGET\" ]] || err \"rollback.sh requires --target\"")
                && src.contains("SECRET_SET=\"$(target_manifest_value \"$TARGET\" \"secret_set\")\"")
                && src.contains("sync_to_vm \"$DEFAULTS_ENV\" \"/opt/example/\"")
                && src.contains("sync_to_vm \"$ENV_PATH\" \"/opt/example/\"")
                && rollback_script_syncs_secret_overrides(src)
                && rollback_script_scopes_compose_services(src)
                && src.contains(
                    "materialize_vm_runtime_env \"$(basename \"$ENV_PATH\")\" \"$REMOTE_SECRET_BUNDLE\" \"$REMOTE_SECRET_OVERRIDE_BUNDLE\"",
                )
                && src.contains("[ -f ./.deploy-state/release-previous.env ]")
                && src.contains("source ./.deploy-state/release-previous.env")
                && src.contains(
                    "cp ./.deploy-state/release-previous.env ./.deploy-state/release-current.env",
                )
                && src.contains("docker compose --env-file .env up -d --force-recreate")
                && src.contains("run_target_health_checks \"$TARGET\" \"$VM_IP\"")
                && src.contains("run_target_smoke_suite \"$TARGET\" \"http://$VM_IP\"")
        });
    let legacy_scripts_require_opt_in = [
        deploy_platform_script
            .as_deref()
            .is_some_and(|src| src.contains("require_legacy_deploy_opt_in \"deploy-platform.sh\"")),
        !deploy_cloud_path.exists()
            || deploy_cloud_script.as_deref().is_some_and(|src| {
                src.contains("require_legacy_deploy_opt_in \"deploy-cloud.sh\"")
            }),
        deploy_build_remote_script.as_deref().is_some_and(|src| {
            src.contains("require_legacy_deploy_opt_in \"deploy-build-remote.sh\"")
        }),
        deploy_vigoros_script
            .as_deref()
            .is_some_and(|src| src.contains("require_legacy_deploy_opt_in \"deploy-vigoros.sh\"")),
        watch_script
            .as_deref()
            .is_some_and(|src| src.contains("EXAMPLE_ALLOW_LEGACY_DEPLOY")),
    ]
    .into_iter()
    .all(|value| value);
    let bootstrap_uses_defaults_env = bootstrap_script.as_deref().is_some_and(|src| {
        src.contains("DEFAULTS_ENV=\"$ROOT/deploy/defaults.env\"")
            && src.contains(
                "docker compose --env-file \"$DEFAULTS_ENV\" --env-file \"$PROFILE_ENV\" build",
            )
            && src.contains(
                "docker compose --env-file \"$DEFAULTS_ENV\" --env-file \"$PROFILE_ENV\" pull --ignore-pull-failures",
            )
            && src.contains("docker compose --env-file \"$DEFAULTS_ENV\" --env-file \"$PROFILE_ENV\" up -d")
    });
    let build_all_multi_platform = common_script
        .as_deref()
        .is_some_and(declares_multi_platform_build)
        && build_all_script
            .as_deref()
            .is_some_and(build_all_preserves_multi_platform_publish);
    let build_images_multi_platform = build_images_script
        .as_deref()
        .is_some_and(declares_multi_platform_build)
        || (build_all_multi_platform
            && build_images_script
                .as_deref()
                .is_some_and(delegates_to_build_all));
    let remote_compose_platform_neutral = remote_compose
        .as_deref()
        .is_some_and(|src| !compose_forces_platform(src));
    let compose_avoids_dead_jaipay_public_host = [
        main_compose.as_deref(),
        optimized_compose.as_deref(),
        ui_dev_compose.as_deref(),
    ]
    .into_iter()
    .flatten()
    .all(compose_avoids_dead_jaipay_public_host);
    let api_docker_arch_aware = api_docker
        .as_deref()
        .is_some_and(dockerfile_selects_arch_specific_wheels);
    let gateway_docker_arch_aware = gateway_docker
        .as_deref()
        .is_some_and(dockerfile_selects_targetarch_onnxruntime);
    let align_docker_arch_aware = align_docker
        .as_deref()
        .is_some_and(dockerfile_selects_targetarch_onnxruntime);

    if let Some(src) = &deploy_script {
        if let Some(line) = find_line(src, "case \"$name\" in") {
            evidence.push(EvidenceItem {
                kind: "deploy_runtime".to_string(),
                path: deploy_script_path.display().to_string(),
                line: Some(line),
                detail: "frontend deploy runtime case table is the authority for name -> path"
                    .to_string(),
            });
        }
        for (needle, detail) in [
            (
                "secret_set=\"$(target_manifest_value \"$TARGET\" \"secret_set\")\"",
                "backend deploy resolves the manifest-declared secret_set before running",
            ),
            (
                "sync_target_runtime_inputs \"$TARGET\" \"$active_cartridges\"",
                "backend deploy syncs target runtime inputs from both target manifest cartridges and active profile cartridges",
            ),
            (
                "run_target_smoke_suite \"$TARGET\" \"http://$vm_ip\"",
                "backend deploy executes the manifest-declared smoke suite after health checks",
            ),
            (
                "secret_layers_have_plusoft_credentials_if_required \"$TARGET\" \"$secret_bundle\" \"$secret_override_bundle\"",
                "backend deploy validates Plusoft credentials across base and override secret layers",
            ),
            (
                "secret_layers_have_pacto_credentials_if_required \"$TARGET\" \"$secret_bundle\" \"$secret_override_bundle\"",
                "backend deploy validates outbound Pacto/Jai-Pay credentials across base and override secret layers",
            ),
            (
                "secret_layers_have_pacto_webhook_chaves_if_required \"$TARGET\" \"$secret_bundle\" \"$secret_override_bundle\"",
                "backend deploy separately validates inbound Pacto webhook chaves across base and override secret layers",
            ),
            (
                "secret_layers_have_infobip_credentials_if_required \"$TARGET\" \"$secret_bundle\" \"$secret_override_bundle\"",
                "backend deploy validates Infobip resource and dispatch credentials across base and override secret layers",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "deploy_runtime".to_string(),
                    path: deploy_script_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            }
        }
    }
    if let Some(src) = &common_script {
        for (needle, detail) in [
            (
                "run_target_health_checks() {",
                "deploy common library owns target health check execution",
            ),
            (
                "run_target_smoke_suite() {",
                "deploy common library owns target smoke suite execution",
            ),
            (
                "target_runtime_uses_cartridge() {",
                "deploy common library resolves target/cartridge runtime hooks from profile CSV and target manifest cartridges",
            ),
            (
                "guard_gcp_deploy_target_allows_cartridges() {",
                "deploy common library prevents AWS-only Health Audit targets from flowing through GCP VM scripts",
            ),
            (
                "sync_ocr_model_bundle_to_vm() {",
                "deploy common library syncs OCR runtime models for cartridge targets that need the gateway OCR sidecar",
            ),
            (
                "require_legacy_deploy_opt_in() {",
                "legacy deploy entrypoints require explicit opt-in before bypassing the authoritative path",
            ),
            (
                "resolve_secret_override_file() {",
                "deploy runtime supports ignored per-target secret override layers",
            ),
            (
                "sync_secret_material_to_vm() {",
                "deploy runtime isolates remote secret material from same-named tracked profiles",
            ),
            (
                "target_uses_assurant_ops_artifacts() {",
                "deploy runtime separates Sara customer support from Assurant Ops artifact hooks",
            ),
            (
                "secret_layers_have_plusoft_credentials_if_required() {",
                "deploy runtime fails fast when Plusoft targets lack API credentials",
            ),
            (
                "secret_layers_have_pacto_credentials_if_required() {",
                "deploy runtime fails fast when Pacto/Jai-Pay targets lack usable outbound credentials",
            ),
            (
                "secret_layers_have_pacto_webhook_chaves_if_required() {",
                "deploy runtime fails fast when Pacto targets lack a valid inbound alias-to-hex32 webhook map",
            ),
            (
                "secret_layers_have_infobip_credentials_if_required() {",
                "deploy runtime fails fast when Infobip targets lack resource or dispatch credentials",
            ),
            (
                "secret_layers_have_flight_broker_credentials_if_required() {",
                "deploy runtime fails fast when explicitly broker-enabled Flight targets lack mTLS credentials",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "deploy_runtime".to_string(),
                    path: common_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            }
        }
    }
    if let Some(src) = &pull_restart_script {
        for (needle, detail) in [
            (
                "SECRET_SET=\"$(target_manifest_value \"$TARGET\" \"secret_set\")\"",
                "pull-and-restart resolves target secret_set from the manifest",
            ),
            (
                "ENV_PATH=\"$(validate_env_file \"$ENV_FILE\")\"",
                "pull-and-restart validates the resolved backend profile before restart",
            ),
            (
                "sync_to_vm \"$DEFAULTS_ENV\" \"/opt/example/\"",
                "pull-and-restart syncs defaults.env instead of assuming stale remote env",
            ),
            (
                "SECRET_OVERRIDE_BUNDLE=\"$(resolve_secret_override_file \"$SECRET_SET\" || true)\"",
                "pull-and-restart resolves optional target secret override bundle",
            ),
            (
                "run_target_smoke_suite \"$TARGET\" \"http://$VM_IP\"",
                "pull-and-restart executes the manifest-declared smoke suite after restart",
            ),
            (
                "sync_target_runtime_inputs \"$TARGET\" \"$ACTIVE_CARTRIDGES\"",
                "pull-and-restart syncs target runtime inputs using the same target/cartridge relation as deploy.sh",
            ),
            (
                "wait_for_vm_health \"http://127.0.0.1:9382/health\" 120",
                "pull-and-restart verifies gateway health before declaring the default backend path healthy",
            ),
            (
                "secret_layers_have_pacto_credentials_if_required \"$TARGET\" \"$SECRET_BUNDLE\" \"$SECRET_OVERRIDE_BUNDLE\"",
                "pull-and-restart validates outbound Pacto/Jai-Pay credentials across base and override secret layers",
            ),
            (
                "secret_layers_have_pacto_webhook_chaves_if_required \"$TARGET\" \"$SECRET_BUNDLE\" \"$SECRET_OVERRIDE_BUNDLE\"",
                "pull-and-restart separately validates inbound Pacto webhook chaves across base and override secret layers",
            ),
            (
                "secret_layers_have_infobip_credentials_if_required \"$TARGET\" \"$SECRET_BUNDLE\" \"$SECRET_OVERRIDE_BUNDLE\"",
                "pull-and-restart validates Infobip resource and dispatch credentials across base and override secret layers",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "deploy_runtime".to_string(),
                    path: pull_restart_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            }
        }
    }
    if let Some(src) = &rollback_script {
        for (needle, detail) in [
            (
                "[ -f ./.deploy-state/release-previous.env ]",
                "rollback requires a previous release snapshot on the VM",
            ),
            (
                "source ./.deploy-state/release-previous.env",
                "rollback restores pinned image references from the previous release snapshot",
            ),
            (
                "cp ./.deploy-state/release-previous.env ./.deploy-state/release-current.env",
                "rollback promotes the restored snapshot back to the current release contract after success",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "deploy_runtime".to_string(),
                    path: rollback_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            }
        }
    }

    if !deploy_requires_secret_bundle {
        warnings.push(
            "deploy/scripts/deploy.sh does not fail fast when a target secret_set cannot be resolved to a concrete secret bundle"
                .to_string(),
        );
    }
    if !common_exposes_target_runtime_helpers {
        warnings.push(
            "deploy/lib/common.sh does not expose the shared target health/smoke/legacy-guard helpers used by the runtime deploy scripts"
                .to_string(),
        );
    }
    if !missing_runtime_image_vars.is_empty() {
        warnings.push(format!(
            "deploy/lib/common.sh does not propagate compose image vars into .env.runtime-overrides: {}",
            missing_runtime_image_vars.join(", ")
        ));
    }
    if !common_supports_secret_overrides {
        warnings.push(
            "deploy/lib/common.sh does not expose an ignored target secret override layer or Plusoft credential preflight"
                .to_string(),
        );
    }
    if !common_supports_pacto_credentials_preflight {
        warnings.push(
            "deploy/lib/common.sh does not expose a Pacto/Jai-Pay credential preflight".to_string(),
        );
    }
    if !common_supports_infobip_credentials_preflight {
        warnings.push(
            "deploy/lib/common.sh does not expose an Infobip resource/dispatch credential preflight"
                .to_string(),
        );
    }
    if !common_supports_flight_broker_preflight {
        warnings.push(
            "deploy/lib/common.sh does not expose an explicit-target Flight mTLS broker credential preflight"
                .to_string(),
        );
    }
    if !common_supports_flight_tenant_hmac_preflight {
        warnings.push(
            "deploy/lib/common.sh does not enforce a 32-byte Flight tenant HMAC secret across target secret layers"
                .to_string(),
        );
    }
    if !deploy_reuses_common_target_helpers {
        warnings.push(
            "deploy/scripts/deploy.sh does not reuse the shared target runtime contract from deploy/lib/common.sh"
                .to_string(),
        );
    }
    if !deploy_syncs_secret_overrides {
        warnings.push(
            "deploy/scripts/deploy.sh does not sync the target secret override layer before rendering runtime env"
                .to_string(),
        );
    }
    if !deploy_preflights_pacto_credentials {
        warnings.push(
            "deploy/scripts/deploy.sh does not fail fast when a target requires Pacto/Jai-Pay credentials"
                .to_string(),
        );
    }
    if !deploy_preflights_infobip_credentials {
        warnings.push(
            "deploy/scripts/deploy.sh does not fail fast when a target requires Infobip resource/dispatch credentials"
                .to_string(),
        );
    }
    if !deploy_preflights_flight_broker {
        warnings.push(
            "deploy/scripts/deploy.sh does not fail fast when an explicitly broker-enabled Flight target lacks mTLS credentials"
                .to_string(),
        );
    }
    if !deploy_preflights_flight_tenant_hmac {
        warnings.push(
            "deploy/scripts/deploy.sh does not fail fast when the Flight tenant HMAC secret is absent or short"
                .to_string(),
        );
    }
    if !deploy_runs_manifest_smoke_suite {
        warnings.push(
            "deploy/scripts/deploy.sh does not execute the manifest-declared smoke_suite literally"
                .to_string(),
        );
    }
    if !pull_restart_reuses_manifest_runtime_contract {
        warnings.push(
            "deploy/scripts/pull-and-restart.sh does not reuse the same manifest-driven env/secret/health/smoke contract as deploy/scripts/deploy.sh"
                .to_string(),
        );
    }
    if !pull_restart_syncs_secret_overrides {
        warnings.push(
            "deploy/scripts/pull-and-restart.sh does not sync the target secret override layer before rendering runtime env"
                .to_string(),
        );
    }
    if !pull_restart_preflights_pacto_credentials {
        warnings.push(
            "deploy/scripts/pull-and-restart.sh does not fail fast when a target requires Pacto/Jai-Pay credentials"
                .to_string(),
        );
    }
    if !pull_restart_preflights_infobip_credentials {
        warnings.push(
            "deploy/scripts/pull-and-restart.sh does not fail fast when a target requires Infobip resource/dispatch credentials"
                .to_string(),
        );
    }
    if !pull_restart_preflights_flight_broker {
        warnings.push(
            "deploy/scripts/pull-and-restart.sh does not fail fast when an explicitly broker-enabled Flight target lacks mTLS credentials"
                .to_string(),
        );
    }
    if !pull_restart_preflights_flight_tenant_hmac {
        warnings.push(
            "deploy/scripts/pull-and-restart.sh does not fail fast when the Flight tenant HMAC secret is absent or short"
                .to_string(),
        );
    }
    if !ocr_sidecar_waits_for_redis {
        warnings.push(
            "example-api/docker-compose.yml lets ocr-sidecar start before Redis is healthy; gateway can hang during Redis BusyLoading after rebuild"
                .to_string(),
        );
    }
    if !ocr_sidecar_has_http_watchdog {
        warnings.push(
            "example-api/docker-compose.yml does not enable the gateway HTTP watchdog; unhealthy-but-alive gateway containers will not self-restart"
                .to_string(),
        );
    }
    if !common_supports_target_cartridge_runtime_inputs {
        warnings.push(
            "deploy/lib/common.sh does not model target runtime inputs as a target/cartridge relation; cartridge-specific deploy hooks can drift when target manifests and profile env disagree"
                .to_string(),
        );
    }
    if !common_guards_health_audit_gcp_path {
        warnings.push(
            "deploy/lib/common.sh does not enforce the Health Audit AWS-only contract before GCP VM deploy scripts sync runtime state"
                .to_string(),
        );
    }
    if !deploy_calls_health_audit_gcp_guard {
        warnings.push(
            "deploy/scripts/deploy.sh does not call the Health Audit AWS-only guard before GCP VM sync/start"
                .to_string(),
        );
    }
    if !pull_restart_calls_health_audit_gcp_guard {
        warnings.push(
            "deploy/scripts/pull-and-restart.sh does not call the Health Audit AWS-only guard before GCP VM sync/restart"
                .to_string(),
        );
    }
    if !health_audit_compose_propagates_ocr_auto_download {
        warnings.push(
            "example-api/docker-compose.health-audit.yml does not propagate OCR_AUTO_DOWNLOAD into ocr-sidecar; the sidecar can crash on an empty host model mount"
                .to_string(),
        );
    }
    if !health_audit_runtime_docs_are_private {
        warnings.push(
            "Health Audit dedicated runtime does not disable public FastAPI /docs and /openapi.json through its profile/compose/app contract"
                .to_string(),
        );
    }
    if !deploy_waits_gateway_in_default_path {
        warnings.push(
            "deploy/scripts/deploy.sh default path does not wait for gateway /health before declaring the backend healthy"
                .to_string(),
        );
    }
    if !pull_restart_waits_gateway_in_default_path {
        warnings.push(
            "deploy/scripts/pull-and-restart.sh default path does not wait for gateway /health before declaring the backend healthy"
                .to_string(),
        );
    }
    if !rollback_restores_previous_release_snapshot {
        warnings.push(
            "deploy/scripts/rollback.sh does not restore the previous release snapshot with the same manifest-driven env/secret/health/smoke contract"
                .to_string(),
        );
    }
    if !legacy_scripts_require_opt_in {
        warnings.push(
            "legacy deploy entrypoints still bypass the authoritative deploy.sh/manifest path without requiring explicit EXAMPLE_ALLOW_LEGACY_DEPLOY opt-in"
                .to_string(),
        );
    }
    if !bootstrap_uses_defaults_env {
        warnings.push(
            "deploy/bootstrap.sh does not load deploy/defaults.env together with the selected profile during compose build/pull/up"
                .to_string(),
        );
    }
    if !build_images_multi_platform {
        warnings.push(
            "deploy/scripts/build-images.sh does not default Docker Build Cloud publishes to linux/amd64,linux/arm64"
                .to_string(),
        );
    }
    if !build_all_multi_platform {
        warnings.push(
            "deploy/scripts/build-all.sh + deploy/lib/common.sh do not preserve the multi-platform linux/amd64,linux/arm64 default for remote image publishes"
                .to_string(),
        );
    }
    if !remote_compose_platform_neutral {
        warnings.push(
            "example-api/docker-compose.remote.yml still forces a single linux/* platform instead of letting Compose resolve the published multi-arch manifest"
                .to_string(),
        );
    }
    if !compose_avoids_dead_jaipay_public_host {
        warnings.push(
            "compose-managed Jai-Pay public URLs still reference jaipay.getjai.com, which is not a resolvable production ingress hostname"
                .to_string(),
        );
    }
    if !api_docker_arch_aware {
        warnings.push(
            "example-api/Dockerfile does not guarantee TARGETARCH-compatible fast wheels via bundled or source builds"
                .to_string(),
        );
    }
    if !gateway_docker_arch_aware {
        warnings.push(
            "example-gateway/Dockerfile still hardcodes x64 ONNX Runtime assets instead of selecting the TARGETARCH runtime payload"
                .to_string(),
        );
    }
    if !align_docker_arch_aware {
        warnings.push(
            "example-align/Dockerfile still hardcodes x64 ONNX Runtime assets instead of selecting the TARGETARCH runtime payload"
                .to_string(),
        );
    }

    let stale_bootstrap_auth = bootstrap_script
        .as_deref()
        .is_some_and(|src| src.contains("/v2/auth/token"));
    if stale_bootstrap_auth {
        warnings.push(
            "deploy/bootstrap.sh still references /v2/auth/token even though the canonical auth surface is /v2/auth/login + /v2/auth/refresh".to_string(),
        );
        if let Some(src) = &bootstrap_script
            && let Some(line) = find_line(src, "/v2/auth/token")
        {
            evidence.push(EvidenceItem {
                kind: "deploy_bootstrap".to_string(),
                path: bootstrap_path.display().to_string(),
                line: Some(line),
                detail: "bootstrap flow still hits a stale auth token endpoint".to_string(),
            });
        }
    }
    if let Some(src) = &bootstrap_script {
        for (needle, detail) in [
            (
                "EXAMPLE_VLLM_AGENT_MAX_TOKENS=${EXAMPLE_VLLM_AGENT_MAX_TOKENS:-192}",
                "defaults.env documents the shared agent token budget contract",
            ),
            (
                "DEFAULTS_ENV=\"$ROOT/deploy/defaults.env\"",
                "bootstrap treats defaults.env as part of the runtime contract",
            ),
            (
                "docker compose --env-file \"$DEFAULTS_ENV\" --env-file \"$PROFILE_ENV\" up -d",
                "bootstrap starts compose with both defaults.env and the selected profile env",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "deploy_bootstrap".to_string(),
                    path: bootstrap_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            }
        }
    }
    if let Some(src) = &build_images_script
        && let Some(line) = find_line(
            src,
            "PLATFORM=\"${BUILD_PLATFORM:-linux/amd64,linux/arm64}\"",
        )
    {
        evidence.push(EvidenceItem {
            kind: "deploy_runtime".to_string(),
            path: build_images_path.display().to_string(),
            line: Some(line),
            detail: "build-images publishes a multi-platform Docker Hub manifest by default"
                .to_string(),
        });
    }
    if let Some(src) = &build_images_script
        && let Some(line) = find_line(src, "deploy/scripts/build-all.sh")
    {
        evidence.push(EvidenceItem {
            kind: "deploy_runtime".to_string(),
            path: build_images_path.display().to_string(),
            line: Some(line),
            detail:
                "build-images delegates publishes to build-all, which inherits the shared multi-platform default"
                    .to_string(),
        });
    }
    if let Some(src) = &common_script
        && let Some(line) = find_line(
            src,
            "PLATFORM=\"${BUILD_PLATFORM:-linux/amd64,linux/arm64}\"",
        )
    {
        evidence.push(EvidenceItem {
            kind: "deploy_runtime".to_string(),
            path: common_path.display().to_string(),
            line: Some(line),
            detail:
                "deploy common defaults the remote build platform set to linux/amd64,linux/arm64"
                    .to_string(),
        });
    }
    if let Some(src) = &remote_compose
        && let Some(line) = find_line(src, "pull_policy: always")
    {
        evidence.push(EvidenceItem {
            kind: "deploy_runtime".to_string(),
            path: remote_compose_path.display().to_string(),
            line: Some(line),
            detail: "remote compose consumes published images without pinning a single platform"
                .to_string(),
        });
    }
    if let Some(src) = &main_compose
        && let Some(line) = find_line(src, "  ocr-sidecar:")
    {
        evidence.push(EvidenceItem {
            kind: "deploy_runtime".to_string(),
            path: main_compose_path.display().to_string(),
            line: Some(line),
            detail: "gateway sidecar is a first-class compose service with explicit startup dependencies"
                .to_string(),
        });
    }
    if let Some(src) = &main_compose
        && let Some(line) = find_line(src, "GATEWAY_HTTP_WATCHDOG_ENABLED")
    {
        evidence.push(EvidenceItem {
            kind: "deploy_runtime".to_string(),
            path: main_compose_path.display().to_string(),
            line: Some(line),
            detail: "gateway sidecar enables an internal HTTP watchdog so Docker restart policy recovers unhealthy-but-alive hangs"
                .to_string(),
        });
    }
    if let Some(src) = &main_compose
        && let Some(line) = find_line(
            src,
            "NEXT_PUBLIC_APP_URL: ${NEXT_PUBLIC_APP_URL:-https://api.getjai.com}",
        )
    {
        evidence.push(EvidenceItem {
            kind: "deploy_runtime".to_string(),
            path: main_compose_path.display().to_string(),
            line: Some(line),
            detail: "Jai-Pay hosted payment links default to the reachable api.getjai.com ingress"
                .to_string(),
        });
    }
    if let Some(src) = &health_audit_compose
        && let Some(line) = find_line(src, "OCR_AUTO_DOWNLOAD: ${OCR_AUTO_DOWNLOAD:-true}")
    {
        evidence.push(EvidenceItem {
            kind: "deploy_runtime".to_string(),
            path: health_audit_compose_path.display().to_string(),
            line: Some(line),
            detail:
                "health-audit compose propagates OCR_AUTO_DOWNLOAD so ocr-sidecar can recover when the host model mount is empty"
                    .to_string(),
        });
    }
    for (src, path, needle, detail) in [
        (
            defaults_env.as_ref(),
            &defaults_env_path,
            "EXAMPLE_PUBLIC_RUNTIME_DOCS=${EXAMPLE_PUBLIC_RUNTIME_DOCS:-true}",
            "shared defaults keep runtime docs public unless a dedicated profile disables them",
        ),
        (
            health_audit_profile.as_ref(),
            &health_audit_profile_path,
            "EXAMPLE_PUBLIC_RUNTIME_DOCS=false",
            "health-audit profile disables FastAPI runtime docs on dedicated customer deploys",
        ),
        (
            health_audit_compose.as_ref(),
            &health_audit_compose_path,
            "EXAMPLE_PUBLIC_RUNTIME_DOCS: ${EXAMPLE_PUBLIC_RUNTIME_DOCS:-false}",
            "health-audit compose propagates the runtime-docs privacy flag into the API container",
        ),
        (
            api_main.as_ref(),
            &api_main_path,
            "_register_gated_docs(app)",
            "FastAPI app removes /docs, /redoc, and /openapi.json when runtime docs are disabled",
        ),
    ] {
        if let Some(src) = src
            && let Some(line) = find_line(src, needle)
        {
            evidence.push(EvidenceItem {
                kind: "deploy_runtime".to_string(),
                path: path.display().to_string(),
                line: Some(line),
                detail: detail.to_string(),
            });
        }
    }
    if let Some(src) = &deploy_script
        && let Some(line) = find_line(
            src,
            "wait_for_vm_health \"http://127.0.0.1:9382/health\" 120",
        )
    {
        evidence.push(EvidenceItem {
            kind: "deploy_runtime".to_string(),
            path: deploy_script_path.display().to_string(),
            line: Some(line),
            detail: "default deploy path waits for gateway health before API health".to_string(),
        });
    }
    if let Some(src) = &api_docker
        && let Some(line) = find_line(src, "maturin build --release --locked")
    {
        evidence.push(EvidenceItem {
                kind: "deploy_runtime".to_string(),
                path: api_docker_path.display().to_string(),
                line: Some(line),
                detail: "api image installs TARGETARCH-matched bundled wheels first and builds any missing fast wheels from office-parsers-rs before final verification"
                    .to_string(),
            });
    }
    if let Some(src) = &gateway_docker
        && let Some(line) = find_line(src, "case \"${TARGETARCH:-amd64}\" in")
    {
        evidence.push(EvidenceItem {
                kind: "deploy_runtime".to_string(),
                path: gateway_docker_path.display().to_string(),
                line: Some(line),
                detail: "gateway image resolves ONNX Runtime assets from TARGETARCH instead of hardcoding x64"
                    .to_string(),
            });
    }
    if let Some(src) = &align_docker
        && let Some(line) = find_line(src, "case \"${TARGETARCH:-amd64}\" in")
    {
        evidence.push(EvidenceItem {
            kind: "deploy_runtime".to_string(),
            path: align_docker_path.display().to_string(),
            line: Some(line),
            detail:
                "align image resolves ONNX Runtime assets from TARGETARCH instead of hardcoding x64"
                    .to_string(),
        });
    }

    for target in &index.deploy_targets {
        let target_src = read_optional_text(&root.join(&target.path), &mut Vec::new());
        let runtime_provider = target_src
            .as_deref()
            .and_then(|src| extract_toml_string_value(src, "runtime_provider"));
        let gcp_target_carries_health_audit = runtime_provider.as_deref() == Some("gcp")
            && target
                .cartridges
                .iter()
                .any(|cartridge| cartridge == "health_audit");
        let smoke_exists = target
            .smoke_suite
            .as_deref()
            .map(|command| command_references_existing_path(command, &root.join("deploy")))
            .unwrap_or(false);
        let smoke_target = target.smoke_suite.as_deref().and_then(extract_smoke_target);
        let rollback_exists = target
            .rollback_command
            .as_deref()
            .map(|command| command_references_existing_path(command, &root.join("deploy")))
            .unwrap_or(false);
        let rollback_target = target
            .rollback_command
            .as_deref()
            .and_then(extract_rollback_target);
        let expected_readiness_target = target
            .readiness_target
            .as_deref()
            .unwrap_or(target.name.as_str());
        let aws_health_audit_target =
            target.name == "health_audit" && runtime_provider.as_deref() == Some("aws");
        let rollback_uses_snapshot_script =
            target.rollback_command.as_deref().is_some_and(|command| {
                supported_snapshot_rollback(
                    command,
                    expected_readiness_target,
                    aws_health_audit_target,
                )
            });
        let expected_smoke_target = if target.name == "sentinel" {
            target.name.as_str()
        } else {
            expected_readiness_target
        };
        let readiness_target_record = index
            .deploy_targets
            .iter()
            .find(|item| item.name == expected_readiness_target);
        let readiness_target_exists = readiness_target_record.is_some();
        let readiness_lineage_is_consistent = readiness_target_record
            .map(|record| readiness_lineage_matches(target, record))
            .unwrap_or(false);

        let manifest_profile_exists = target.profile.as_ref().is_some_and(|profile| {
            profiles_by_name.contains_key(format!("{profile}.env").as_str())
        });
        let backend_profile_exists = target.backend_profile.as_ref().is_some_and(|profile| {
            profiles_by_name.contains_key(format!("{profile}.env").as_str())
        });
        let profile_matches_backend = match (&target.profile, &target.backend_profile) {
            (Some(left), Some(right)) => left == right,
            _ => true,
        };

        let secret_record = target.secret_set.as_ref().and_then(|name| {
            secrets_by_name
                .get(format!("{name}.env.example").as_str())
                .copied()
        });
        let secret_exists = secret_record.is_some();

        let frontend_runtime_match = target
            .frontend_project
            .as_ref()
            .filter(|name| !name.is_empty())
            .and_then(|name| frontend_runtime.get(name.as_str()));
        let frontend_mapping_matches = match (target.ui_path.as_deref(), frontend_runtime_match) {
            (Some(ui_path), Some((runtime_path, _runtime_project))) => ui_path == runtime_path,
            (None, None) => true,
            (_, Some(_)) => false,
            (_, None) => target.ui_role.as_deref() == Some("api_only"),
        };

        let profile_key_drift = target
            .backend_profile
            .as_ref()
            .and_then(|name| {
                profiles_by_name
                    .get(format!("{name}.env").as_str())
                    .copied()
            })
            .map(|profile| {
                non_canonical_profile_keys(profile, &allowed_profile_keys, &global_secret_keys)
            })
            .unwrap_or_default();

        let secret_alias_drift = secret_record
            .map(|record| unresolved_secret_keys(record, &global_secret_keys))
            .unwrap_or_default();
        let requires_pacto = target
            .required_integrations
            .iter()
            .any(|integration| integration == "pacto");
        let requires_jaipay = target
            .required_integrations
            .iter()
            .any(|integration| integration == "jaipay");
        let secret_declares_pacto_credentials = secret_record
            .map(|record| secret_set_declares_pacto_credentials(record, requires_jaipay))
            .unwrap_or(!requires_pacto && !requires_jaipay);
        let local_secret_bundle = target
            .secret_set
            .as_deref()
            .and_then(|name| resolve_local_secret_bundle_path(root, name));
        let local_secret_has_pacto_credentials = local_secret_bundle
            .as_deref()
            .map(|path| env_file_has_pacto_credentials(path, requires_jaipay))
            .unwrap_or(true);
        let invalid_health_checks = target
            .health_checks
            .iter()
            .filter(|value| !is_http_url(value))
            .cloned()
            .collect::<Vec<_>>();
        let customer_ops_has_align_readiness = target.name != "customer_ops_unified"
            || target
                .health_checks
                .iter()
                .any(|value| value == "http://localhost:8081/ready");

        if !smoke_exists {
            warnings.push(format!(
                "{}: smoke suite path is missing or unresolved",
                target.name
            ));
        }
        if target.smoke_suite.is_some() && smoke_target.as_deref() != Some(expected_smoke_target) {
            warnings.push(format!(
                "{}: smoke suite must target `{}` instead of {:?}",
                target.name, expected_smoke_target, smoke_target
            ));
        }
        if !rollback_exists {
            warnings.push(format!(
                "{}: rollback command path is missing or unresolved",
                target.name
            ));
        }
        if target.rollback_command.is_some() && !rollback_uses_snapshot_script {
            warnings.push(format!(
                "{}: rollback command must use the supported snapshot operator for its runtime provider",
                target.name
            ));
        }
        if target.rollback_command.is_some()
            && rollback_target.as_deref() != Some(expected_readiness_target)
        {
            warnings.push(format!(
                "{}: rollback command must target `{}` instead of {:?}",
                target.name, expected_readiness_target, rollback_target
            ));
        }
        if target.profile.is_some() && !manifest_profile_exists {
            warnings.push(format!("{}: manifest profile is missing", target.name));
        }
        if target.backend_profile.is_some() && !backend_profile_exists {
            warnings.push(format!("{}: backend profile is missing", target.name));
        }
        if !profile_matches_backend {
            warnings.push(format!(
                "{}: target manifest drifts between profile={:?} and backend_profile={:?}",
                target.name, target.profile, target.backend_profile
            ));
        }
        if target.secret_set.is_some() && !secret_exists {
            warnings.push(format!("{}: secret set is missing", target.name));
        }
        if target.readiness_target.is_some() && !readiness_target_exists {
            warnings.push(format!(
                "{}: readiness_target `{}` does not exist under deploy/targets",
                target.name, expected_readiness_target
            ));
        }
        if target.readiness_target.is_some()
            && readiness_target_exists
            && !readiness_lineage_is_consistent
        {
            warnings.push(format!(
                "{}: readiness_target `{}` must share backend_profile and secret_set lineage",
                target.name, expected_readiness_target
            ));
        }
        if !frontend_mapping_matches {
            warnings.push(format!(
                "{}: frontend_project/ui_path does not match deploy.sh runtime mapping",
                target.name
            ));
        }
        if !profile_key_drift.is_empty() {
            warnings.push(format!(
                "{}: backend profile contains non-canonical operational keys: {}",
                target.name,
                profile_key_drift.join(", ")
            ));
        }
        if !secret_alias_drift.is_empty() {
            warnings.push(format!(
                "{}: secret set contains vars not normalized against deploy/secrets.env.example: {}",
                target.name,
                secret_alias_drift.join(", ")
            ));
        }
        if (requires_pacto || requires_jaipay) && !secret_declares_pacto_credentials {
            warnings.push(format!(
                "{}: secret set contract must declare a usable Pacto credential source for required integrations {:?}",
                target.name, target.required_integrations
            ));
        }
        if (requires_pacto || requires_jaipay) && !local_secret_has_pacto_credentials {
            warnings.push(format!(
                "{}: local secret bundle exists but has no concrete Pacto credential source for required integrations {:?}",
                target.name, target.required_integrations
            ));
        }
        if !invalid_health_checks.is_empty() {
            warnings.push(format!(
                "{}: health_checks must be absolute http(s) URLs: {}",
                target.name,
                invalid_health_checks.join(", ")
            ));
        }
        if target.name == "health_audit" && runtime_provider.as_deref() != Some("aws") {
            warnings.push(
                "health_audit: target must declare runtime_provider=\"aws\" because Health Audit is no longer deployed on the GCP customer-ops VM"
                    .to_string(),
            );
        }
        if target.name == "customer_ops_unified" && runtime_provider.as_deref() != Some("gcp") {
            warnings.push(
                "customer_ops_unified: target must declare runtime_provider=\"gcp\" so it cannot be confused with the AWS-only Health Audit deploy"
                    .to_string(),
            );
        }
        if !customer_ops_has_align_readiness {
            warnings.push(
                "customer_ops_unified: health_checks must include http://localhost:8081/ready so deploy promotion cannot accept an alignment service without its required model"
                    .to_string(),
            );
        }
        if gcp_target_carries_health_audit {
            warnings.push(format!(
                "{}: runtime_provider=\"gcp\" targets must not include health_audit; Health Audit is AWS-only",
                target.name
            ));
        }

        entities.push(json!({
            "name": target.name,
            "profile": target.profile,
            "runtime_provider": runtime_provider,
            "backend_profile": target.backend_profile,
            "readiness_target": target.readiness_target,
            "readiness_target_exists": readiness_target_exists,
            "readiness_lineage_is_consistent": readiness_lineage_is_consistent,
            "manifest_profile_exists": manifest_profile_exists,
            "backend_profile_exists": backend_profile_exists,
            "profile_matches_backend": profile_matches_backend,
            "secret_exists": secret_exists,
            "frontend_project": target.frontend_project,
            "ui_path": target.ui_path,
            "frontend_mapping_matches": frontend_mapping_matches,
            "profile_key_drift": profile_key_drift,
            "secret_alias_drift": secret_alias_drift,
            "requires_pacto": requires_pacto,
            "requires_jaipay": requires_jaipay,
            "secret_declares_pacto_credentials": secret_declares_pacto_credentials,
            "local_secret_bundle": local_secret_bundle
                .as_ref()
                .map(|path| path.display().to_string()),
            "local_secret_has_pacto_credentials": local_secret_has_pacto_credentials,
            "invalid_health_checks": invalid_health_checks,
            "smoke_exists": smoke_exists,
            "smoke_target": smoke_target,
            "rollback_exists": rollback_exists,
            "rollback_target": rollback_target,
            "rollback_uses_snapshot_script": rollback_uses_snapshot_script,
            "health_checks": target.health_checks,
            "customer_ops_has_align_readiness": customer_ops_has_align_readiness,
            "gcp_target_carries_health_audit": gcp_target_carries_health_audit,
        }));
        evidence.push(EvidenceItem {
            kind: "deploy_target".to_string(),
            path: target.path.clone(),
            line: None,
            detail: format!("target {}", target.name),
        });
    }

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_deploy"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked {} deploy targets, found {} warnings",
            index.deploy_targets.len(),
            warnings.len()
        ),
        confidence: if warnings.is_empty() { 0.95 } else { 0.72 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn is_http_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

fn declares_multi_platform_build(src: &str) -> bool {
    src.contains("linux/amd64,linux/arm64")
}

fn build_all_preserves_multi_platform_publish(src: &str) -> bool {
    let service_platform_defaults_to_shared_default =
        src.contains("service_platform=\"${5:-$PLATFORM}\"");
    let push_path_fans_out_platforms = src.contains("platforms_from_csv \"$service_platform\"")
        && src.contains("--platform \"$platform\"")
        && src.contains("arch_tag \"$TAG\" \"$platform\"")
        && src.contains("create_manifest_list \"$final_ref\" \"${arch_refs[@]}\"")
        && src.contains("verify_manifest_platforms \"$final_ref\" \"${manifest_platforms[@]}\"");
    let local_path_uses_service_platform = src.contains("--platform \"$service_platform\"");

    src.contains("--platform \"$PLATFORM\"")
        || (service_platform_defaults_to_shared_default
            && push_path_fans_out_platforms
            && local_path_uses_service_platform)
}

fn delegates_to_build_all(src: &str) -> bool {
    src.contains("deploy/scripts/build-all.sh") && src.contains("\"${ARGS[@]}\"")
}

fn common_script_supports_secret_overrides(src: &str) -> bool {
    src.contains("resolve_secret_override_file() {")
        && src.contains("secret_layers_have_plusoft_credentials_if_required() {")
        && src.contains("remote_secret_material_path() {")
        && src.contains(".deploy-secrets/%s")
        && src.contains("sync_secret_material_to_vm() {")
        && src.contains("RUNTIME_SECRET_OVERRIDE_BUNDLE=$secret_override_basename")
}

fn common_script_supports_pacto_credentials_preflight(src: &str) -> bool {
    src.contains("secret_layers_have_pacto_credentials_if_required() {")
        && src.contains("target_requires_integration \"$target_name\" \"jaipay\"")
        && src.contains("PACTO_CREDENTIALS_JSON")
        && src.contains("PACTO_CREDENTIALS_FILE")
        && src.contains("PACTO_API_TOKEN")
        && src.contains("secret_layers_have_pacto_webhook_chaves_if_required() {")
        && src.contains("target_requires_integration \"$target_name\" \"pacto\"")
        && src.contains("PACTO_WEBHOOK_CHAVES")
        && src.contains("alias-to-hex32")
}

fn common_script_supports_infobip_credentials_preflight(src: &str) -> bool {
    src.contains("secret_layers_have_infobip_credentials_if_required() {")
        && src.contains("target_requires_integration \"$target_name\" \"infobip\"")
        && src.contains("INFOBIP_API_BASE_URL")
        && src.contains("INFOBIP_BASIC_AUTH_USERNAME")
        && src.contains("INFOBIP_BASIC_AUTH_PASSWORD")
        && src.contains("INFOBIP_SCENARIO_KEY")
}

fn common_script_supports_flight_broker_preflight(src: &str) -> bool {
    src.contains("target_uses_protected_flight_broker() {")
        && src.contains("target_requires_integration \"$target_name\" \"flight_broker\"")
        && src.contains("secret_layers_have_flight_broker_credentials_if_required() {")
        && src.contains("EXAMPLE_FLIGHT_AUTH_BROKER_URL")
        && src.contains("EXAMPLE_FLIGHT_AUTH_BROKER_CLIENT_CERT")
        && src.contains("EXAMPLE_FLIGHT_AUTH_BROKER_CLIENT_KEY")
        && src.contains("EXAMPLE_FLIGHT_AUTH_BROKER_CA")
        && src.contains(
            "EXAMPLE_FLIGHT_AUTH_BROKER_SECRETS_HOST_DIR='/opt/example/secrets/flight-broker'",
        )
        && src.contains("'EXAMPLE_FLIGHT_AUTH_BROKER_SECRETS_HOST_DIR'")
}

fn common_script_supports_flight_tenant_hmac_preflight(src: &str) -> bool {
    src.contains("secret_layers_have_flight_tenant_hmac() {")
        && src.contains("EXAMPLE_FLIGHT_TENANT_HMAC_SECRET")
        && src.contains("len(value.encode(\"utf-8\")) < 32")
}

fn deploy_script_syncs_secret_overrides(src: &str) -> bool {
    src.contains("secret_override_bundle=\"$(resolve_secret_override_file \"$secret_set\" || true)\"")
        && src.contains("secret_layers_have_plusoft_credentials_if_required \"$TARGET\" \"$secret_bundle\" \"$secret_override_bundle\"")
        && src.contains("secret_override_remote_path=\"$(remote_secret_material_path \"$secret_override_bundle\")\"")
        && src.contains("sync_secret_material_to_vm \"$secret_override_bundle\"")
        && src.contains("\"$secret_override_remote_path\"")
}

fn deploy_script_preflights_pacto_credentials(src: &str) -> bool {
    src.contains("secret_layers_have_pacto_credentials_if_required \"$TARGET\" \"$secret_bundle\" \"$secret_override_bundle\"")
        && src.contains("secret_layers_have_pacto_webhook_chaves_if_required \"$TARGET\" \"$secret_bundle\" \"$secret_override_bundle\"")
}

fn deploy_script_preflights_infobip_credentials(src: &str) -> bool {
    src.contains("secret_layers_have_infobip_credentials_if_required \"$TARGET\" \"$secret_bundle\" \"$secret_override_bundle\"")
}

fn deploy_script_preflights_flight_broker(src: &str) -> bool {
    src.contains("secret_layers_have_flight_broker_credentials_if_required \"$TARGET\" \"$secret_bundle\" \"$secret_override_bundle\"")
}

fn deploy_script_preflights_flight_tenant_hmac(src: &str) -> bool {
    src.contains(
        "secret_layers_have_flight_tenant_hmac \"$secret_bundle\" \"$secret_override_bundle\"",
    )
}

fn pull_restart_script_syncs_secret_overrides(src: &str) -> bool {
    src.contains("SECRET_OVERRIDE_BUNDLE=\"$(resolve_secret_override_file \"$SECRET_SET\" || true)\"")
        && src.contains("secret_layers_have_plusoft_credentials_if_required \"$TARGET\" \"$SECRET_BUNDLE\" \"$SECRET_OVERRIDE_BUNDLE\"")
        && src.contains("REMOTE_SECRET_OVERRIDE_BUNDLE=\"$(remote_secret_material_path \"$SECRET_OVERRIDE_BUNDLE\")\"")
        && src.contains("sync_secret_material_to_vm \"$SECRET_OVERRIDE_BUNDLE\"")
        && src.contains("\"$REMOTE_SECRET_OVERRIDE_BUNDLE\"")
}

fn rollback_script_syncs_secret_overrides(src: &str) -> bool {
    src.contains(
        "SECRET_OVERRIDE_BUNDLE=\"$(resolve_secret_override_file \"$SECRET_SET\" || true)\"",
    ) && src.contains(
        "REMOTE_SECRET_OVERRIDE_BUNDLE=\"$(remote_secret_material_path \"$SECRET_OVERRIDE_BUNDLE\")\"",
    ) && src.contains("sync_secret_material_to_vm \"$SECRET_OVERRIDE_BUNDLE\"")
        && src.contains("\"$REMOTE_SECRET_OVERRIDE_BUNDLE\"")
        && src.contains("secret_layers_have_plusoft_credentials_if_required \"$TARGET\" \"$SECRET_BUNDLE\" \"$SECRET_OVERRIDE_BUNDLE\"")
        && src.contains("secret_layers_have_infobip_credentials_if_required \"$TARGET\" \"$SECRET_BUNDLE\" \"$SECRET_OVERRIDE_BUNDLE\"")
        && src.contains("secret_layers_have_pacto_credentials_if_required \"$TARGET\" \"$SECRET_BUNDLE\" \"$SECRET_OVERRIDE_BUNDLE\"")
        && src.contains("secret_layers_have_pacto_webhook_chaves_if_required \"$TARGET\" \"$SECRET_BUNDLE\" \"$SECRET_OVERRIDE_BUNDLE\"")
        && src.contains("secret_layers_have_flight_tenant_hmac \"$SECRET_BUNDLE\" \"$SECRET_OVERRIDE_BUNDLE\"")
        && src.contains("secret_layers_have_flight_broker_credentials_if_required \"$TARGET\" \"$SECRET_BUNDLE\" \"$SECRET_OVERRIDE_BUNDLE\"")
}

fn rollback_script_scopes_compose_services(src: &str) -> bool {
    src.contains("target_manifest_values \"$TARGET\" \"compose_services\"")
        && src.contains("target_services_csv='$TARGET_COMPOSE_SERVICES_CSV'")
        && src.contains(
            "docker compose --env-file .env up -d --force-recreate --no-deps \\$target_services",
        )
}

fn pull_restart_script_preflights_pacto_credentials(src: &str) -> bool {
    src.contains("secret_layers_have_pacto_credentials_if_required \"$TARGET\" \"$SECRET_BUNDLE\" \"$SECRET_OVERRIDE_BUNDLE\"")
        && src.contains("secret_layers_have_pacto_webhook_chaves_if_required \"$TARGET\" \"$SECRET_BUNDLE\" \"$SECRET_OVERRIDE_BUNDLE\"")
}

fn pull_restart_script_preflights_infobip_credentials(src: &str) -> bool {
    src.contains("secret_layers_have_infobip_credentials_if_required \"$TARGET\" \"$SECRET_BUNDLE\" \"$SECRET_OVERRIDE_BUNDLE\"")
}

fn pull_restart_script_preflights_flight_broker(src: &str) -> bool {
    src.contains("secret_layers_have_flight_broker_credentials_if_required \"$TARGET\" \"$SECRET_BUNDLE\" \"$SECRET_OVERRIDE_BUNDLE\"")
}

fn pull_restart_script_preflights_flight_tenant_hmac(src: &str) -> bool {
    src.contains(
        "secret_layers_have_flight_tenant_hmac \"$SECRET_BUNDLE\" \"$SECRET_OVERRIDE_BUNDLE\"",
    )
}

fn supported_snapshot_rollback(
    command: &str,
    readiness_target: &str,
    aws_health_audit_target: bool,
) -> bool {
    let generic = command.contains("./scripts/rollback.sh --target")
        && extract_rollback_target(command).as_deref() == Some(readiness_target);
    let health_audit_aws = aws_health_audit_target
        && readiness_target == "health_audit"
        && command.contains("./scripts/health-audit-aws.sh rollback ${HEALTH_AUDIT_AWS_SSH_ALIAS}");
    if aws_health_audit_target {
        health_audit_aws
    } else {
        generic
    }
}

fn compose_forces_platform(src: &str) -> bool {
    let Ok(pattern) = Regex::new(r"(?m)^\s*platform:\s*linux/(amd64|arm64)\s*$") else {
        return false;
    };
    pattern.is_match(src)
}

fn compose_avoids_dead_jaipay_public_host(src: &str) -> bool {
    !src.contains("jaipay.getjai.com")
}

fn extract_toml_string_value(src: &str, key: &str) -> Option<String> {
    let prefix = format!("{key} = ");
    src.lines().find_map(|line| {
        let trimmed = line.trim();
        let raw = trimmed.strip_prefix(&prefix)?.trim();
        if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
            Some(raw.trim_matches('"').to_string())
        } else {
            None
        }
    })
}

fn extract_compose_service_block(src: &str, service_name: &str) -> Option<String> {
    let marker = format!("  {service_name}:");
    let mut in_block = false;
    let mut lines = Vec::new();
    for line in src.lines() {
        if line == marker {
            in_block = true;
            lines.push(line);
            continue;
        }
        if in_block
            && line.starts_with("  ")
            && !line.starts_with("    ")
            && !line.trim().is_empty()
        {
            break;
        }
        if in_block {
            lines.push(line);
        }
    }
    in_block.then(|| lines.join("\n"))
}

fn dockerfile_selects_arch_specific_wheels(src: &str) -> bool {
    src.contains("ARG TARGETARCH")
        && (src.contains("case \"${TARGETARCH:-amd64}\" in")
            || src.contains("case \"${TARGETARCH}\" in"))
        && src.contains("/build/wheels/*x86_64*.whl")
        && src.contains("/build/wheels/*aarch64*.whl")
        && src.contains("maturin build --release --locked")
        && (src.contains("/src/workspace/office-parsers-rs/${crate}/Cargo.toml")
            || src.contains("/build/office-parsers-rs/${crate}/Cargo.toml"))
        && src.contains("Missing required fast wheels after bundled install/source build fallback")
}

fn dockerfile_selects_targetarch_onnxruntime(src: &str) -> bool {
    src.contains("ARG TARGETARCH")
        && src.contains("case \"${TARGETARCH:-amd64}\" in")
        && src.contains("ort_arch=\"x64\"")
        && src.contains("ort_arch=\"aarch64\"")
        && src.contains("onnxruntime-linux-${ort_arch}-${ORT_VERSION}.tgz")
}

fn parse_frontend_runtime_mappings(script: &str) -> HashMap<String, (String, String)> {
    let mut out = HashMap::new();
    let Ok(pattern) =
        Regex::new(r#"(?m)^\s*([a-zA-Z0-9._-]+)\)\s+path="([^"]+)";\s+project="([^"]+)"\s+;;"#)
    else {
        return out;
    };

    for capture in pattern.captures_iter(script) {
        let name = capture.get(1).map(|m| m.as_str().to_string());
        let path = capture.get(2).map(|m| m.as_str().to_string());
        let project = capture.get(3).map(|m| m.as_str().to_string());
        if let (Some(name), Some(path), Some(project)) = (name, path, project) {
            out.insert(name, (path, project));
        }
    }

    out
}

fn parse_env_reference_keys(raw: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let Ok(pattern) = Regex::new(r"\$\{([A-Z][A-Z0-9_]+)") else {
        return keys;
    };

    for capture in pattern.captures_iter(raw) {
        if let Some(name) = capture.get(1).map(|m| m.as_str().to_string()) {
            keys.insert(name);
        }
    }

    keys
}

fn is_deploy_image_key(key: &str) -> bool {
    key.ends_with("_IMAGE")
}

fn parse_runtime_materialized_env_keys(raw: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();

    if let Ok(export_pattern) = Regex::new(r"(?m)^\s*export\s+([A-Z][A-Z0-9_]+)=") {
        for capture in export_pattern.captures_iter(raw) {
            if let Some(name) = capture.get(1).map(|m| m.as_str().to_string()) {
                keys.insert(name);
            }
        }
    }

    if let Ok(python_key_pattern) = Regex::new(r"'([A-Z][A-Z0-9_]+)',") {
        for capture in python_key_pattern.captures_iter(raw) {
            if let Some(name) = capture.get(1).map(|m| m.as_str().to_string()) {
                keys.insert(name);
            }
        }
    }

    keys
}

fn read_optional_text(path: &Path, warnings: &mut Vec<String>) -> Option<String> {
    if path.exists() {
        read_text(path, warnings)
    } else {
        None
    }
}

fn parse_env_keys(raw: &str) -> BTreeSet<String> {
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || !trimmed.contains('=') {
                return None;
            }
            trimmed
                .split_once('=')
                .map(|(name, _)| name.trim().to_string())
        })
        .collect()
}

fn non_canonical_profile_keys(
    profile: &ProfileRecord,
    allowed_profile_keys: &BTreeSet<String>,
    global_secret_keys: &BTreeSet<String>,
) -> Vec<String> {
    profile
        .vars
        .iter()
        .filter(|var| !profile_key_is_allowed(var, allowed_profile_keys, global_secret_keys))
        .map(|var| var.name.clone())
        .collect()
}

fn profile_key_is_allowed(
    var: &DeclaredVar,
    allowed_profile_keys: &BTreeSet<String>,
    global_secret_keys: &BTreeSet<String>,
) -> bool {
    allowed_profile_keys.contains(var.name.as_str())
        || (global_secret_keys.contains(var.name.as_str()) && var.value_preview.is_none())
}

fn unresolved_secret_keys(
    secret_set: &SecretSetRecord,
    global_secret_keys: &BTreeSet<String>,
) -> Vec<String> {
    secret_set
        .vars
        .iter()
        .map(|var| normalize_secret_key(&var.name))
        .zip(secret_set.vars.iter().map(|var| var.name.clone()))
        .filter_map(|(canonical, original)| {
            if global_secret_keys.contains(canonical) {
                None
            } else {
                Some(original)
            }
        })
        .collect()
}

fn secret_set_declares_pacto_credentials(
    secret_set: &SecretSetRecord,
    _requires_jaipay: bool,
) -> bool {
    let declared: BTreeSet<&str> = secret_set
        .vars
        .iter()
        .map(|var| normalize_secret_key(&var.name))
        .collect();

    declared.contains("PACTO_CREDENTIALS_JSON")
        || declared.contains("PACTO_API_TOKEN")
        || declared.contains("PACTO_CREDENTIALS_FILE")
}

fn resolve_local_secret_bundle_path(root: &Path, secret_set_name: &str) -> Option<PathBuf> {
    let base = root.join("deploy/secret-sets");
    [
        base.join(format!("{secret_set_name}.env.local")),
        base.join(format!("{secret_set_name}.env")),
    ]
    .into_iter()
    .find(|path| path.exists())
}

fn env_file_has_pacto_credentials(path: &Path, _requires_jaipay: bool) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let material = parse_material_env_keys(&raw);

    material.contains("PACTO_CREDENTIALS_JSON")
        || material.contains("PACTO_API_TOKEN")
        || material.contains("PACTO_CREDENTIALS_FILE")
}

fn parse_material_env_keys(raw: &str) -> BTreeSet<String> {
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }

            let trimmed = trimmed.strip_prefix("export ").unwrap_or(trimmed).trim();
            let (key, value) = trimmed.split_once('=')?;
            let key = key.trim();
            if key.is_empty() {
                return None;
            }

            let value = value.trim().trim_matches('"').trim_matches('\'').trim();
            if value.is_empty() || value.starts_with("${") {
                return None;
            }

            Some(key.to_string())
        })
        .collect()
}

fn normalize_secret_key(name: &str) -> &str {
    match name {
        "WHATSAPP_ACCESS_TOKEN" => "WHATSAPP_API_TOKEN",
        "WHATSAPP_VERIFY_TOKEN" => "WHATSAPP_WEBHOOK_VERIFY_TOKEN",
        "WHATSAPP_APP_SECRET" => "META_APP_SECRET",
        "GEMINI_API_KEY" => "GOOGLE_API_KEY",
        _ => name,
    }
}

fn validate_redis_memory_contract(
    compose_path: &Path,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) {
    let Ok(raw) = std::fs::read_to_string(compose_path) else {
        warnings.push(format!(
            "{}: could not read compose file for Redis memory validation",
            compose_path.display()
        ));
        return;
    };

    let Ok(redis_block) = Regex::new(r"(?ms)^  redis:\n(?P<body>(?:    .*?\n)+)") else {
        return;
    };
    let Some(captures) = redis_block.captures(&raw) else {
        return;
    };
    let Some(body) = captures.name("body").map(|m| m.as_str()) else {
        return;
    };

    let Ok(mem_limit_pattern) = Regex::new(r"(?m)^\s{4}mem_limit:\s*([^\n#]+)") else {
        return;
    };
    let Ok(maxmemory_pattern) = Regex::new(r"(?m)^\s*-\s+--maxmemory\s*$\n^\s*-\s+([^\n#]+)")
    else {
        return;
    };

    let Some(mem_limit_raw) = mem_limit_pattern
        .captures(body)
        .and_then(|capture| capture.get(1))
        .map(|m| m.as_str().trim().trim_matches('"').trim_matches('\''))
    else {
        return;
    };
    let Some(maxmemory_raw) = maxmemory_pattern
        .captures(body)
        .and_then(|capture| capture.get(1))
        .map(|m| m.as_str().trim().trim_matches('"').trim_matches('\''))
    else {
        return;
    };

    let Some(mem_limit_bytes) = parse_size_bytes(mem_limit_raw) else {
        warnings.push(format!(
            "{}: could not parse redis mem_limit `{}`",
            compose_path.display(),
            mem_limit_raw
        ));
        return;
    };
    let Some(maxmemory_bytes) = parse_size_bytes(maxmemory_raw) else {
        warnings.push(format!(
            "{}: could not parse redis --maxmemory `{}`",
            compose_path.display(),
            maxmemory_raw
        ));
        return;
    };

    if let Some(line) = find_line(&raw, "  redis:") {
        evidence.push(EvidenceItem {
            kind: "deploy_runtime".to_string(),
            path: compose_path.display().to_string(),
            line: Some(line),
            detail: format!(
                "redis sizing contract mem_limit={} maxmemory={}",
                mem_limit_raw, maxmemory_raw
            ),
        });
    }

    if mem_limit_bytes <= maxmemory_bytes {
        warnings.push(format!(
            "{}: redis mem_limit `{}` must stay above redis --maxmemory `{}` or the container can OOM during AOF/RDB load",
            compose_path.display(),
            mem_limit_raw,
            maxmemory_raw
        ));
    }
}

fn parse_size_bytes(value: &str) -> Option<u64> {
    let trimmed = value.trim().trim_matches('"').trim_matches('\'');
    if trimmed.is_empty() {
        return None;
    }
    let normalized = if trimmed.starts_with("${") && trimmed.ends_with('}') {
        trimmed
            .trim_start_matches("${")
            .trim_end_matches('}')
            .split(":-")
            .nth(1)
            .map(str::trim)
            .unwrap_or("")
    } else {
        trimmed
    };
    if normalized.is_empty() {
        return None;
    }

    let split_at = normalized
        .find(|char: char| !char.is_ascii_digit() && char != '.')
        .unwrap_or(normalized.len());
    let (number_part, unit_part) = normalized.split_at(split_at);
    let number = number_part.parse::<f64>().ok()?;
    let multiplier = match unit_part.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1_f64,
        "k" | "kb" => 1024_f64,
        "m" | "mb" => 1024_f64.powi(2),
        "g" | "gb" => 1024_f64.powi(3),
        "t" | "tb" => 1024_f64.powi(4),
        _ => return None,
    };
    Some((number * multiplier) as u64)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{
        build_all_preserves_multi_platform_publish, common_script_supports_flight_broker_preflight,
        common_script_supports_flight_tenant_hmac_preflight,
        common_script_supports_infobip_credentials_preflight,
        common_script_supports_pacto_credentials_preflight,
        common_script_supports_secret_overrides, compose_avoids_dead_jaipay_public_host,
        compose_forces_platform, declares_multi_platform_build, delegates_to_build_all,
        deploy_script_preflights_flight_tenant_hmac, deploy_script_preflights_infobip_credentials,
        deploy_script_preflights_pacto_credentials, deploy_script_syncs_secret_overrides,
        dockerfile_selects_arch_specific_wheels, dockerfile_selects_targetarch_onnxruntime,
        non_canonical_profile_keys, normalize_secret_key, parse_env_reference_keys,
        parse_material_env_keys, parse_runtime_materialized_env_keys,
        pull_restart_script_preflights_flight_tenant_hmac,
        pull_restart_script_preflights_infobip_credentials,
        pull_restart_script_preflights_pacto_credentials,
        pull_restart_script_syncs_secret_overrides, rollback_script_scopes_compose_services,
        rollback_script_syncs_secret_overrides, secret_set_declares_pacto_credentials,
        supported_snapshot_rollback,
    };
    use crate::model::{DeclaredVar, ProfileRecord, SecretSetRecord};

    fn keys(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    fn declared(name: &str, value_preview: Option<&str>) -> DeclaredVar {
        DeclaredVar {
            name: name.to_string(),
            value_preview: value_preview.map(str::to_string),
            raw_value: None,
        }
    }

    #[test]
    fn detects_multi_platform_default() {
        assert!(declares_multi_platform_build(
            r#"PLATFORM="${BUILD_PLATFORM:-linux/amd64,linux/arm64}""#
        ));
        assert!(!declares_multi_platform_build(
            r#"PLATFORM="${BUILD_PLATFORM:-linux/amd64}""#
        ));
    }

    #[test]
    fn detects_build_all_per_arch_manifest_publish_path() {
        assert!(build_all_preserves_multi_platform_publish(
            r#"
local service_platform="${5:-$PLATFORM}"
build_cmd=(docker buildx build --builder "$BUILDER" --platform "$platform")
arch_ref="$(image_ref "$service" "$(arch_tag "$TAG" "$platform")")"
done < <(platforms_from_csv "$service_platform")
create_manifest_list "$final_ref" "${arch_refs[@]}"
verify_manifest_platforms "$final_ref" "${manifest_platforms[@]}"
build_cmd=(docker buildx build --builder "$BUILDER" --platform "$service_platform")
"#
        ));
        assert!(!build_all_preserves_multi_platform_publish(
            r#"
local service_platform="${5:-linux/amd64}"
build_cmd=(docker buildx build --builder "$BUILDER" --platform "$service_platform")
"#
        ));
    }

    #[test]
    fn detects_build_images_delegating_to_build_all() {
        assert!(delegates_to_build_all(
            r#"exec "$ROOT/deploy/scripts/build-all.sh" "${ARGS[@]}""#
        ));
        assert!(!delegates_to_build_all(
            r#"docker buildx build --push "$ROOT""#
        ));
    }

    #[test]
    fn detects_secret_override_runtime_support() {
        assert!(common_script_supports_secret_overrides(
            r#"
resolve_secret_override_file() {
  true
}
secret_layers_have_plusoft_credentials_if_required() {
  true
}
remote_secret_material_path() {
  printf '.deploy-secrets/%s\n' "$(basename "$source")"
}
sync_secret_material_to_vm() {
  sync_to_vm "$source" "$remote_dir/"
}
RUNTIME_SECRET_OVERRIDE_BUNDLE=$secret_override_basename
"#
        ));

        assert!(!common_script_supports_secret_overrides(
            r#"
resolve_secret_override_file() {
  true
}
secret_layers_have_plusoft_credentials_if_required() {
  true
}
RUNTIME_SECRET_OVERRIDE_BUNDLE=$secret_override_basename
"#
        ));
    }

    #[test]
    fn detects_deploy_script_secret_override_sync() {
        let good = r#"
secret_override_bundle="$(resolve_secret_override_file "$secret_set" || true)"
secret_layers_have_plusoft_credentials_if_required "$TARGET" "$secret_bundle" "$secret_override_bundle"
secret_override_remote_path="$(remote_secret_material_path "$secret_override_bundle")"
sync_secret_material_to_vm "$secret_override_bundle"
materialize_vm_runtime_env "$(basename "$env_path")" "$secret_remote_path" "$secret_override_remote_path"
"#;
        let bad = r#"
secret_override_bundle="$(resolve_secret_override_file "$secret_set" || true)"
secret_layers_have_plusoft_credentials_if_required "$TARGET" "$secret_bundle" "$secret_override_bundle"
sync_to_vm "$secret_override_bundle" "/opt/example/"
materialize_vm_runtime_env "$(basename "$env_path")" "$(basename "${secret_bundle:-}")" "$(basename "${secret_override_bundle:-}")"
"#;

        assert!(deploy_script_syncs_secret_overrides(good));
        assert!(!deploy_script_syncs_secret_overrides(bad));
    }

    #[test]
    fn detects_pull_restart_secret_override_sync() {
        let good = r#"
SECRET_OVERRIDE_BUNDLE="$(resolve_secret_override_file "$SECRET_SET" || true)"
secret_layers_have_plusoft_credentials_if_required "$TARGET" "$SECRET_BUNDLE" "$SECRET_OVERRIDE_BUNDLE"
REMOTE_SECRET_OVERRIDE_BUNDLE="$(remote_secret_material_path "$SECRET_OVERRIDE_BUNDLE")"
sync_secret_material_to_vm "$SECRET_OVERRIDE_BUNDLE"
materialize_vm_runtime_env "$(basename "$ENV_PATH")" "$REMOTE_SECRET_BUNDLE" "$REMOTE_SECRET_OVERRIDE_BUNDLE"
"#;
        let bad = r#"
SECRET_OVERRIDE_BUNDLE="$(resolve_secret_override_file "$SECRET_SET" || true)"
secret_layers_have_plusoft_credentials_if_required "$TARGET" "$SECRET_BUNDLE" "$SECRET_OVERRIDE_BUNDLE"
sync_to_vm "$SECRET_OVERRIDE_BUNDLE" "/opt/example/"
materialize_vm_runtime_env "$(basename "$ENV_PATH")" "$(basename "${SECRET_BUNDLE:-}")" "$(basename "${SECRET_OVERRIDE_BUNDLE:-}")"
"#;

        assert!(pull_restart_script_syncs_secret_overrides(good));
        assert!(!pull_restart_script_syncs_secret_overrides(bad));
    }

    #[test]
    fn detects_rollback_secret_override_sync() {
        let good = r#"
SECRET_OVERRIDE_BUNDLE="$(resolve_secret_override_file "$SECRET_SET" || true)"
REMOTE_SECRET_OVERRIDE_BUNDLE="$(remote_secret_material_path "$SECRET_OVERRIDE_BUNDLE")"
sync_secret_material_to_vm "$SECRET_OVERRIDE_BUNDLE"
secret_layers_have_plusoft_credentials_if_required "$TARGET" "$SECRET_BUNDLE" "$SECRET_OVERRIDE_BUNDLE"
secret_layers_have_infobip_credentials_if_required "$TARGET" "$SECRET_BUNDLE" "$SECRET_OVERRIDE_BUNDLE"
secret_layers_have_pacto_credentials_if_required "$TARGET" "$SECRET_BUNDLE" "$SECRET_OVERRIDE_BUNDLE"
secret_layers_have_pacto_webhook_chaves_if_required "$TARGET" "$SECRET_BUNDLE" "$SECRET_OVERRIDE_BUNDLE"
secret_layers_have_flight_tenant_hmac "$SECRET_BUNDLE" "$SECRET_OVERRIDE_BUNDLE"
secret_layers_have_flight_broker_credentials_if_required "$TARGET" "$SECRET_BUNDLE" "$SECRET_OVERRIDE_BUNDLE"
materialize_vm_runtime_env "$(basename "$ENV_PATH")" "$REMOTE_SECRET_BUNDLE" "$REMOTE_SECRET_OVERRIDE_BUNDLE"
"#;
        let bad = r#"
sync_secret_material_to_vm "$SECRET_BUNDLE"
materialize_vm_runtime_env "$(basename "$ENV_PATH")" "$REMOTE_SECRET_BUNDLE"
"#;

        assert!(rollback_script_syncs_secret_overrides(good));
        assert!(!rollback_script_syncs_secret_overrides(bad));
    }

    #[test]
    fn detects_rollback_target_service_scope() {
        let good = r#"
done < <(target_manifest_values "$TARGET" "compose_services")
target_services_csv='$TARGET_COMPOSE_SERVICES_CSV'
docker compose --env-file .env up -d --force-recreate --no-deps \$target_services
"#;
        let transitive_dependency_risk = r#"
done < <(target_manifest_values "$TARGET" "compose_services")
target_services_csv='$TARGET_COMPOSE_SERVICES_CSV'
docker compose --env-file .env up -d --force-recreate \$target_services
"#;
        let unscoped = "docker compose --env-file .env up -d --force-recreate";

        assert!(rollback_script_scopes_compose_services(good));
        assert!(!rollback_script_scopes_compose_services(
            transitive_dependency_risk
        ));
        assert!(!rollback_script_scopes_compose_services(unscoped));
    }

    #[test]
    fn detects_pacto_credentials_preflight() {
        let common = r#"
secret_layers_have_pacto_credentials_if_required() {
  target_requires_integration "$target_name" "jaipay"
  require_material_secret PACTO_CREDENTIALS_JSON
  require_material_secret PACTO_CREDENTIALS_FILE
  require_material_secret PACTO_API_TOKEN
}
secret_layers_have_pacto_webhook_chaves_if_required() {
  target_requires_integration "$target_name" "pacto"
  require_material_secret PACTO_WEBHOOK_CHAVES
  alias-to-hex32
}
"#;
        let deploy = r#"
secret_layers_have_pacto_credentials_if_required "$TARGET" "$secret_bundle" "$secret_override_bundle"
secret_layers_have_pacto_webhook_chaves_if_required "$TARGET" "$secret_bundle" "$secret_override_bundle"
"#;
        let pull = r#"
secret_layers_have_pacto_credentials_if_required "$TARGET" "$SECRET_BUNDLE" "$SECRET_OVERRIDE_BUNDLE"
secret_layers_have_pacto_webhook_chaves_if_required "$TARGET" "$SECRET_BUNDLE" "$SECRET_OVERRIDE_BUNDLE"
"#;

        assert!(common_script_supports_pacto_credentials_preflight(common));
        assert!(deploy_script_preflights_pacto_credentials(deploy));
        assert!(pull_restart_script_preflights_pacto_credentials(pull));
    }

    #[test]
    fn detects_infobip_credentials_preflight() {
        let common = r#"
secret_layers_have_infobip_credentials_if_required() {
  target_requires_integration "$target_name" "infobip"
  INFOBIP_API_BASE_URL
  INFOBIP_BASIC_AUTH_USERNAME
  INFOBIP_BASIC_AUTH_PASSWORD
  INFOBIP_SCENARIO_KEY
}
"#;
        let deploy = r#"
secret_layers_have_infobip_credentials_if_required "$TARGET" "$secret_bundle" "$secret_override_bundle"
"#;
        let pull = r#"
secret_layers_have_infobip_credentials_if_required "$TARGET" "$SECRET_BUNDLE" "$SECRET_OVERRIDE_BUNDLE"
"#;

        assert!(common_script_supports_infobip_credentials_preflight(common));
        assert!(deploy_script_preflights_infobip_credentials(deploy));
        assert!(pull_restart_script_preflights_infobip_credentials(pull));
    }

    #[test]
    fn flight_broker_preflight_requires_explicit_target_integration() {
        let explicit = r#"
target_uses_protected_flight_broker() {
  target_requires_integration "$target_name" "flight_broker"
}
secret_layers_have_flight_broker_credentials_if_required() {
  EXAMPLE_FLIGHT_AUTH_BROKER_URL
  EXAMPLE_FLIGHT_AUTH_BROKER_CLIENT_CERT
  EXAMPLE_FLIGHT_AUTH_BROKER_CLIENT_KEY
  EXAMPLE_FLIGHT_AUTH_BROKER_CA
}
export EXAMPLE_FLIGHT_AUTH_BROKER_SECRETS_HOST_DIR='/opt/example/secrets/flight-broker'
'EXAMPLE_FLIGHT_AUTH_BROKER_SECRETS_HOST_DIR'
"#;
        let compose_wide = explicit.replace(
            "target_requires_integration \"$target_name\" \"flight_broker\"",
            "case \"$compose_file\" in *) return 0 ;; esac",
        );

        assert!(common_script_supports_flight_broker_preflight(explicit));
        assert!(!common_script_supports_flight_broker_preflight(
            &compose_wide
        ));
    }

    #[test]
    fn detects_flight_tenant_hmac_preflight_across_deploy_paths() {
        let common = r#"
secret_layers_have_flight_tenant_hmac() {
  EXAMPLE_FLIGHT_TENANT_HMAC_SECRET
  len(value.encode("utf-8")) < 32
}
"#;
        let deploy = r#"
secret_layers_have_flight_tenant_hmac "$secret_bundle" "$secret_override_bundle"
"#;
        let pull = r#"
secret_layers_have_flight_tenant_hmac "$SECRET_BUNDLE" "$SECRET_OVERRIDE_BUNDLE"
"#;

        assert!(common_script_supports_flight_tenant_hmac_preflight(common));
        assert!(deploy_script_preflights_flight_tenant_hmac(deploy));
        assert!(pull_restart_script_preflights_flight_tenant_hmac(pull));
        assert!(!common_script_supports_flight_tenant_hmac_preflight(
            &common.replace(" < 32", " < 16")
        ));
    }

    #[test]
    fn accepts_provider_specific_health_audit_aws_rollback() {
        assert!(supported_snapshot_rollback(
            "./scripts/health-audit-aws.sh rollback ${HEALTH_AUDIT_AWS_SSH_ALIAS}",
            "health_audit",
            true,
        ));
        assert!(!supported_snapshot_rollback(
            "./scripts/rollback.sh --target health_audit",
            "health_audit",
            true,
        ));
        assert!(!supported_snapshot_rollback(
            "./scripts/health-audit-aws.sh rollback customer-host",
            "health_audit",
            true,
        ));
        assert!(supported_snapshot_rollback(
            "./scripts/rollback.sh --target health_audit",
            "health_audit",
            false,
        ));
    }

    #[test]
    fn detects_compose_platform_pins() {
        assert!(compose_forces_platform(
            "services:\n  api:\n    platform: linux/amd64\n"
        ));
        assert!(!compose_forces_platform(
            "services:\n  api:\n    pull_policy: always\n"
        ));
    }

    #[test]
    fn detects_dead_jaipay_public_hostname() {
        assert!(compose_avoids_dead_jaipay_public_host(
            "NEXT_PUBLIC_APP_URL: ${NEXT_PUBLIC_APP_URL:-https://api.getjai.com}"
        ));
        assert!(!compose_avoids_dead_jaipay_public_host(
            "NEXT_PUBLIC_APP_URL: https://jaipay.getjai.com"
        ));
    }

    #[test]
    fn detects_arch_aware_parser_wheel_install() {
        let good = r#"
ARG TARGETARCH
case "${TARGETARCH:-amd64}" in
  amd64) set -- /build/wheels/*x86_64*.whl ;;
  arm64) set -- /build/wheels/*aarch64*.whl ;;
esac
maturin build --release --locked --manifest-path "/src/workspace/office-parsers-rs/${crate}/Cargo.toml"
raise SystemExit("Missing required fast wheels after bundled install/source build fallback")
"#;
        let bad = r#"
RUN uv pip install --system --no-cache /build/wheels/*.whl
"#;

        assert!(dockerfile_selects_arch_specific_wheels(good));
        assert!(!dockerfile_selects_arch_specific_wheels(bad));
    }

    #[test]
    fn detects_arch_aware_parser_wheel_install_with_narrow_build_context() {
        let good = r#"
ARG TARGETARCH
export TARGETARCH="${TARGETARCH:-amd64}"
case "${TARGETARCH}" in
  amd64) set -- /build/wheels/*x86_64*.whl ;;
  arm64) set -- /build/wheels/*aarch64*.whl ;;
esac
maturin build --release --locked --manifest-path "/build/office-parsers-rs/${crate}/Cargo.toml"
raise SystemExit("Missing required fast wheels after bundled install/source build fallback")
"#;

        assert!(dockerfile_selects_arch_specific_wheels(good));
    }

    #[test]
    fn detects_arch_aware_onnxruntime_downloads() {
        let good = r#"
ARG TARGETARCH
case "${TARGETARCH:-amd64}" in
  amd64) ort_arch="x64" ;;
  arm64) ort_arch="aarch64" ;;
esac
curl -sL "https://example.invalid/onnxruntime-linux-${ort_arch}-${ORT_VERSION}.tgz"
"#;
        let bad = r#"
curl -sL "https://example.invalid/onnxruntime-linux-x64-${ORT_VERSION}.tgz"
"#;

        assert!(dockerfile_selects_targetarch_onnxruntime(good));
        assert!(!dockerfile_selects_targetarch_onnxruntime(bad));
    }

    #[test]
    fn extracts_env_reference_keys_from_compose_contracts() {
        let keys = parse_env_reference_keys(
            r#"
services:
  api:
    image: ${EXAMPLE_API_IMAGE:-jquant/example-api:latest}
    environment:
      REDIS_URL: redis://:${REDIS_PASSWORD:-}@redis:6379/0
      META_ACCESS_TOKEN: ${META_ACCESS_TOKEN:-${WHATSAPP_API_TOKEN:-}}
"#,
        );

        assert!(keys.contains("EXAMPLE_API_IMAGE"));
        assert!(keys.contains("REDIS_PASSWORD"));
        assert!(keys.contains("META_ACCESS_TOKEN"));
        assert!(keys.contains("WHATSAPP_API_TOKEN"));
    }

    #[test]
    fn extracts_runtime_materialized_image_keys() {
        let keys = parse_runtime_materialized_env_keys(
            r#"
export EXAMPLE_API_IMAGE='${EXAMPLE_API_IMAGE:-}'
export EXAMPLE_STUDIO_WEB_IMAGE='${EXAMPLE_STUDIO_WEB_IMAGE:-}'
for key in (
    'EXAMPLE_API_IMAGE',
    'EXAMPLE_STUDIO_WEB_IMAGE',
):
    pass
"#,
        );

        assert!(keys.contains("EXAMPLE_API_IMAGE"));
        assert!(keys.contains("EXAMPLE_STUDIO_WEB_IMAGE"));
    }

    #[test]
    fn normalizes_legacy_whatsapp_secret_alias() {
        assert_eq!(
            normalize_secret_key("WHATSAPP_APP_SECRET"),
            "META_APP_SECRET"
        );
        assert_eq!(
            normalize_secret_key("WHATSAPP_VERIFY_TOKEN"),
            "WHATSAPP_WEBHOOK_VERIFY_TOKEN"
        );
    }

    #[test]
    fn pacto_secret_contract_accepts_registry_file_for_jaipay() {
        let file_only = SecretSetRecord {
            name: "customer_ops_unified.env.example".to_string(),
            path: "deploy/secret-sets/customer_ops_unified.env.example".to_string(),
            vars: vec![declared("PACTO_CREDENTIALS_FILE", None)],
        };
        let json = SecretSetRecord {
            name: "customer_ops_unified.env.example".to_string(),
            path: "deploy/secret-sets/customer_ops_unified.env.example".to_string(),
            vars: vec![declared("PACTO_CREDENTIALS_JSON", None)],
        };

        assert!(secret_set_declares_pacto_credentials(&file_only, true));
        assert!(secret_set_declares_pacto_credentials(&file_only, false));
        assert!(secret_set_declares_pacto_credentials(&json, true));
    }

    #[test]
    fn material_env_keys_ignore_empty_and_unexpanded_values() {
        let raw = r#"
PACTO_API_TOKEN=
PACTO_CREDENTIALS_FILE=${PACTO_CREDENTIALS_FILE:-}
export PACTO_CREDENTIALS_JSON='{"unit":"ok"}'
"#;
        let material = parse_material_env_keys(raw);

        assert_eq!(material.len(), 1);
        assert!(material.contains("PACTO_CREDENTIALS_JSON"));
    }

    #[test]
    fn non_canonical_profile_keys_allows_empty_secret_disables() {
        let profile = ProfileRecord {
            name: "sisfron-airgap.env".to_string(),
            path: "deploy/profiles/sisfron-airgap.env".to_string(),
            vars: vec![
                declared("EXAMPLE_ACTIVE_CARTRIDGES", Some("sisfron")),
                declared("OPENAI_API_KEY", None),
                declared("ANTHROPIC_API_KEY", None),
                declared("WHATSAPP_APP_SECRET", None),
                declared("UNREGISTERED_RUNTIME_KEY", Some("value")),
            ],
        };

        let drift = non_canonical_profile_keys(
            &profile,
            &keys(&["EXAMPLE_ACTIVE_CARTRIDGES"]),
            &keys(&["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "WHATSAPP_APP_SECRET"]),
        );

        assert_eq!(drift, vec!["UNREGISTERED_RUNTIME_KEY"]);
    }

    #[test]
    fn non_canonical_profile_keys_rejects_secret_values_in_profiles() {
        let profile = ProfileRecord {
            name: "bad.env".to_string(),
            path: "deploy/profiles/bad.env".to_string(),
            vars: vec![declared("OPENAI_API_KEY", Some("sk-test"))],
        };

        let drift =
            non_canonical_profile_keys(&profile, &BTreeSet::new(), &keys(&["OPENAI_API_KEY"]));

        assert_eq!(drift, vec!["OPENAI_API_KEY"]);
    }

    #[test]
    fn customer_ops_runtime_capture_keys_are_canonical_profile_keys() {
        let profile = ProfileRecord {
            name: "customer_ops_unified.env".to_string(),
            path: "deploy/profiles/customer_ops_unified.env".to_string(),
            vars: vec![
                declared("VIGOROS_SWARM_SYNTHESIS_MODEL", Some("claude-sonnet-4-6")),
                declared("VIGOROS_SWARM_SYNTHESIS_MAX_TOKENS", Some("8192")),
                declared("VIGOROS_SWARM_WEB_SEARCH_ENABLED", Some("true")),
                declared("OPENAI_RESPONSES_MAX_OUTPUT_TOKENS", Some("4096")),
                declared("OPENAI_RESPONSES_REASONING_EFFORT", Some("low")),
                declared("OPENAI_RESPONSES_TEXT_VERBOSITY", Some("low")),
                declared(
                    "OPENAI_RESPONSES_WEB_SEARCH_TOOL",
                    Some("web_search_preview"),
                ),
                declared("EXAMPLE_SKIP_HOTPATH_SYNC_STARTUP", Some("true")),
            ],
        };

        let drift = non_canonical_profile_keys(
            &profile,
            &keys(&[
                "VIGOROS_SWARM_SYNTHESIS_MODEL",
                "VIGOROS_SWARM_SYNTHESIS_MAX_TOKENS",
                "VIGOROS_SWARM_WEB_SEARCH_ENABLED",
                "OPENAI_RESPONSES_MAX_OUTPUT_TOKENS",
                "OPENAI_RESPONSES_REASONING_EFFORT",
                "OPENAI_RESPONSES_TEXT_VERBOSITY",
                "OPENAI_RESPONSES_WEB_SEARCH_TOOL",
                "EXAMPLE_SKIP_HOTPATH_SYNC_STARTUP",
            ]),
            &BTreeSet::new(),
        );

        assert!(drift.is_empty(), "unexpected drift: {drift:?}");
    }

    #[test]
    fn sisfron_local_operational_keys_grounded_by_compose_substitution() {
        // The sisfron-local profile declares connected-topology operational keys
        // (LLM/embed provider, offline/airgap mode, egress) that the sisfron
        // cartridge config reads. They are canonical only because the
        // sisfron-local compose declares each as a `${VAR:-default}` substitution
        // (mirroring its already-grounded sibling keys), which the doctor now
        // scans. If the compose regresses those entries back to bare literals,
        // `parse_env_reference_keys` stops grounding them and this test fails —
        // catching the exact drift the doctor flagged.
        let compose_environment = r#"
  SISFRON_LLM_PROVIDER: ${SISFRON_LLM_PROVIDER:-vllm}
  SISFRON_LLM_MODEL: ${SISFRON_LLM_MODEL:-Qwen/Qwen3.5-27B}
  SISFRON_EMBED_PROVIDER: ${SISFRON_EMBED_PROVIDER:-vllm}
  SISFRON_OFFLINE_MODE: ${SISFRON_OFFLINE_MODE:-false}
  SISFRON_AIRGAP_MODE: ${SISFRON_AIRGAP_MODE:-false}
  ALLOW_EXTERNAL_REQUESTS: ${ALLOW_EXTERNAL_REQUESTS:-true}
"#;
        let allowed = parse_env_reference_keys(compose_environment);

        let profile = ProfileRecord {
            name: "sisfron-local.env".to_string(),
            path: "deploy/profiles/sisfron-local.env".to_string(),
            vars: vec![
                declared("SISFRON_LLM_PROVIDER", Some("vllm")),
                declared("SISFRON_EMBED_PROVIDER", Some("vllm")),
                declared("SISFRON_OFFLINE_MODE", Some("false")),
                declared("SISFRON_AIRGAP_MODE", Some("false")),
                declared("ALLOW_EXTERNAL_REQUESTS", Some("true")),
            ],
        };

        let drift = non_canonical_profile_keys(&profile, &allowed, &BTreeSet::new());

        assert!(drift.is_empty(), "unexpected drift: {drift:?}");
    }
}
