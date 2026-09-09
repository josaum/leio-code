//! Trait-based registry of profile-aware contract checks ("doctors").
//!
//! Each module in this directory implements one doctor — a contract over the
//! current repo (file paths, needle strings, parsed structures) that returns
//! a [`QueryEnvelope`] with `warnings` for any drift. The CLI surfaces them
//! individually (`doctor <name>`) and composed via [`crate::audit`].
//!
//! Routing:
//! - [`Doctor`] is the trait every doctor implements.
//! - Registration declares which profiles a doctor applies to. The
//!   [`crate::config::PROFILE_GENERIC`] profile registers only the
//!   workspace-agnostic generic pack (slop, redis-key-hygiene,
//!   rust-toolchain-pin-coherence, env-contract, import-boundary,
//!   orphan-files) — each of which no-ops cleanly when its inputs are
//!   absent; profile-specific checks light up under [`PROFILE_LEIO_CODE`]
//!   (self-contract) and [`PROFILE_EXAMPLE`] (the full operational catalog).
//! - `run_all_doctors` / `run_baseline_doctors` / `run_ci_doctors` execute
//!   the relevant subset in parallel via rayon and merge envelopes.
//!
//! Adding a new doctor: implement `Doctor`, register it under the right
//! profile(s), and (per `tests/test_doctor_warning_ledger.py`) ensure each
//! warning family the doctor can emit is accounted for in the ledger.

use std::path::Path;
use std::time::Instant;

use rayon::prelude::*;
use serde_json::json;

use crate::config::{PROFILE_EXAMPLE, PROFILE_GENERIC, PROFILE_LEIO_CODE, repo_profile};
use crate::model::{QueryEnvelope, RepoIndex};

pub mod active_cartridges_env_drift;
pub mod agent_seed_update_completeness;
pub mod align_dockerfile_path_dep_coherence;
pub mod artifact_reuse;
pub mod assurant_ops_production;
pub mod assurant_seed_wiring;
pub mod audio_handler_timeout;
pub mod auth_brokering;
pub mod auth_jwt_compat;
pub mod auth_session_continuity;
pub mod bare_cartridge_redis_key;
pub mod c4gym_evo_integration;
pub mod cartridge_boundary;
pub mod chatwoot_pratique_sync;
pub mod clinical_contract;
pub mod composition_resolver;
pub mod conversation_identity;
pub mod deploy;
pub mod deploy_bundle_critical_keys;
pub mod duckdb_contract;
pub mod egress_compliance;
pub mod egress_success_without_wamid;
pub mod env_contract;
pub mod event_durability;
pub mod event_envelope;
pub mod evo_credential_isolation;
pub mod fast_wheelhouse_contract;
pub mod fitness_member_unit_resolver;
pub mod flight_channel_reuse;
pub mod flight_contract_auth;
pub mod flight_secret_propagation;
pub mod flight_server_context_isolation;
pub mod flight_server_zero_copy;
pub mod frontend_congruence;
pub mod frontend_readiness;
pub mod gateway_oar_boundary;
pub mod gateway_ocr_pipeline;
pub mod generate_async_flight;
pub mod gliner_shared_surface;
pub mod glosa_contract;
pub mod grpc_message_size;
pub mod health_audit_ans_xsd;
pub mod health_audit_audit_trail_durability;
pub mod health_audit_auth;
pub mod health_audit_contract_airgap;
pub mod health_audit_embedding;
pub mod health_audit_engine_health;
pub mod health_audit_evidence_class;
pub mod health_audit_redis_primacy;
pub mod health_audit_router_size;
pub mod health_audit_sentinel;
pub mod health_audit_worker_runtime;
pub mod import_boundary;
pub mod induced_invariants;
pub mod inference_contracts;
pub mod jaipay_pacto_gcp_webhook;
pub mod jaipay_supabase_session_pooler;
pub mod kb_collection_exists;
pub mod layout_contract_platform;
pub mod layout_fast_spectral_contract;
pub mod leio_release_coherence;
pub mod lgpd_outbound_filter;
pub mod liz_jaipay_pacto_lookup_contract;
pub mod llm_provider_egress;
pub mod local_packs;
pub mod luminai_health_audit_isolation;
pub mod manus_residue;
pub mod modal_app_naming_coherence;

pub mod codex_orchestration;
pub mod compose_worker_mem_budget;
pub mod oaei_doc_consistency;
pub mod ocr_canonical_layout;
pub mod ocr_flight_wiring;
pub mod ocr_models_on_disk;
pub mod office_parsers_arrow_ipc;
pub mod office_parsers_clippy_gate;
pub mod office_parsers_doc_coverage;
pub mod office_parsers_node_parity;
pub mod onboarding_drift;
pub mod onboarding_projection;
pub mod ontology_price_consistency;
pub mod operator_tenant_parity;
pub mod orphan_files;
pub mod pacto_drain_dependency;
pub mod pacto_webhook_allowlist_populated;
pub mod parse_hybrid_fallback;
pub mod pdf_pipeline_divergence;
pub mod pdf_studio_env_coherence;
pub mod platform_runtime_trust_boundary;
pub mod plusoft_handover_payload_contract;
pub mod plusoft_routing_contract;
pub mod plusoft_transcript_fidelity;
pub mod pratique_cobranca_json_contract;
pub mod prod_surface_hygiene;
pub mod publishable_crate;
pub mod py_rust_boundary;
pub mod recipient_limbo;
pub mod redis_key_hygiene;
pub mod repo_hygiene;
pub mod revops_snapshot_schema_sync;
pub mod revops_tenant_gate;
pub mod route_projection;
pub mod rust_dependency_baseline;
pub mod rust_toolchain_pin_coherence;
pub mod sara_assurant_egress_contract;
pub mod script_path_existence;
pub mod secret_set_parity;
pub mod self_contract;
pub mod semantic_wiring;
pub mod server_dockerfile_context_coherence;
pub mod session_hot_state;
pub mod sisfron_ooda_runtime;
pub mod sisfron_simulation_durability;
pub mod skill_contract;
pub mod slop_gate;
mod source_scan;
pub mod startup_hook_cartridge_coverage;
pub mod supabase_project_liveness;
pub mod supabase_runtime_shape;
pub mod tenant_identity;
pub mod tenant_override_contract;
pub mod tessellation_contract;
pub mod test_patch_target_integrity;
pub mod test_route_mount_integrity;
pub mod typescript_config_hygiene;
pub mod utils;
pub mod vendored_crate_provenance;
pub mod version_manifest;
pub mod vigoros_swarm;
pub mod whatsapp_bsuid;
pub mod worker_mem_budget;

pub trait Doctor {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope;
}

struct RegisteredDoctor {
    profiles: &'static [&'static str],
    doctor: Box<dyn Doctor>,
}

impl RegisteredDoctor {
    fn supports_profile(&self, profile: &str) -> bool {
        self.profiles.contains(&profile)
    }
}

pub fn doctor_names() -> Vec<&'static str> {
    registry()
        .into_iter()
        .map(|registered| registered.doctor.name())
        .collect()
}

pub fn doctor_names_for_profile(profile: &str) -> Vec<&'static str> {
    registry()
        .into_iter()
        .filter(|registered| registered.supports_profile(profile))
        .map(|registered| registered.doctor.name())
        .collect()
}

pub fn run_doctor(name: &str, index: &RepoIndex, root: &Path) -> Option<QueryEnvelope> {
    let profile = repo_profile(root);
    registry()
        .into_iter()
        .find(|registered| registered.doctor.name() == name)
        .map(|registered| {
            if registered.supports_profile(&profile) {
                registered.doctor.run(index, root)
            } else {
                QueryEnvelope {
                    schema_version: crate::model::SCHEMA_VERSION.to_string(),
                    query_id: utils::query_id(&format!("doctor_{name}_unsupported")),
                    kind: "doctor".to_string(),
                    summary: format!(
                        "doctor `{name}` is not available for workspace profile `{profile}`"
                    ),
                    confidence: 0.95,
                    entities: vec![json!({
                        "doctor": name,
                        "workspace_profile": profile.clone(),
                        "supported_profiles": registered.profiles,
                    })],
                    evidence: Vec::new(),
                    warnings: Vec::new(),
                    meta: Some(json!({
                        "workspace_profile": profile,
                        "supported_profiles": registered.profiles,
                    })),
                    timing_ms: 0,
                }
            }
        })
}

pub fn run_all_doctors(index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let profile = repo_profile(root);
    let doctors = registry()
        .into_iter()
        .filter(|registered| registered.supports_profile(&profile))
        .collect::<Vec<_>>();

    if doctors.is_empty() {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: utils::query_id("doctor_all"),
            kind: "doctor".to_string(),
            summary: format!("no doctors are configured for workspace profile `{profile}`"),
            confidence: 0.95,
            entities: Vec::new(),
            evidence: Vec::new(),
            warnings: Vec::new(),
            meta: Some(json!({
                "workspace_profile": profile,
                "doctor_count": 0,
            })),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();
    let mut total_warnings = 0usize;
    let mut failing_doctors = 0usize;

    let has_py_rust_composite = doctors
        .iter()
        .any(|entry| entry.doctor.name() == "py-rust-boundary");

    for registered in doctors {
        let doctor = registered.doctor;
        let name = doctor.name();
        if name != "py-rust-boundary"
            && has_py_rust_composite
            && py_rust_boundary::PY_RUST_BOUNDARY_CHILD_DOCTORS.contains(&name)
        {
            entities.push(json!({
                "doctor": name,
                "skipped": true,
                "summary": "covered by py-rust-boundary composite",
            }));
            continue;
        }
        let envelope = doctor.run(index, root);
        let warning_count = envelope.warnings.len();

        if warning_count > 0 {
            failing_doctors += 1;
            evidence.extend(envelope.evidence.clone());
        }
        total_warnings += warning_count;

        warnings.extend(
            envelope
                .warnings
                .iter()
                .map(|warning| format!("[{name}] {warning}")),
        );
        entities.push(json!({
            "doctor": name,
            "summary": envelope.summary,
            "warning_count": warning_count,
            "evidence_count": envelope.evidence.len(),
            "query_id": envelope.query_id,
        }));
    }

    let doctor_count = entities.len();
    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: utils::query_id("doctor_all"),
        kind: "doctor".to_string(),
        summary: format!(
            "ran {} doctors for profile `{}`, found {} warnings across {} failing doctors",
            doctor_count, profile, total_warnings, failing_doctors
        ),
        confidence: if total_warnings == 0 { 0.98 } else { 0.68 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "workspace_profile": profile,
            "doctor_count": doctor_count,
            "failing_doctors": failing_doctors,
            "warning_count": total_warnings,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// Doctor names for `status --strict` and `leio-code doctor baseline` (file-local; no live probes).
pub const BASELINE_DOCTOR_NAMES: &[&str] = &[
    "deploy",
    "py-rust-boundary",
    "cartridge-boundary",
    "route-projection",
    "typescript-config-hygiene",
    "self-contract",
    // OCR consolidation invariants — fast (string-grep over 6 files) and a
    // hard prerequisite for accuracy across the workspace.  Any drift in
    // the rec↔dict pairing, pre-stage wiring, EP features, or shared
    // tuning surface fires here before it reaches a tenant.
    "gateway-ocr-pipeline",
    "ocr-models-on-disk",
    "ocr-canonical-layout",
    // Generic, file-local, fast: audits git-tracked files for committed noise.
    "repo-hygiene",
    // Portable, deterministic release and agent-orchestration contracts. Both
    // self-skip cleanly when their respective repository surfaces are absent.
    "codex-orchestration",
    "leio-release-coherence",
];

/// Extra doctors for `leio-code doctor ci` (still file-local).
pub const CI_EXTRA_DOCTOR_NAMES: &[&str] = &[
    "luminai-health-audit-isolation",
    "skill-contract",
    "semantic-wiring",
    "auth-session-continuity",
    // Cross-plane seam checks live in py-rust-boundary (baseline); keep CI
    // focused on file-local wiring not covered by the composite.
    "manus-residue",
    // Drift-pattern doctors added 2026-05-04 — pre-deploy relevant.
    "agent-seed-update-completeness",
    "startup-hook-cartridge-coverage",
    "assurant-seed-wiring",
    "assurant-ops-production",
    "parse-hybrid-fallback",
    "layout-fast-spectral-contract",
    "egress-success-without-wamid",
    "bare-cartridge-redis-key",
    "worker-mem-budget",
    "audio-handler-timeout",
    "chatwoot-pratique-sync",
    "kb-collection-exists",
    "evo-credential-isolation",
    "fitness-member-unit-resolver",
    "jaipay-pacto-gcp-webhook",
    "jaipay-supabase-session-pooler",
    "supabase-runtime-shape",
    "compose-worker-mem-budget",
    "health-audit-worker-runtime",
    "health-audit-audit-trail-durability",
    "supabase-project-liveness",
    "liz-jaipay-pacto-lookup-contract",
    "plusoft-routing-contract",
    "plusoft-handover-payload-contract",
    "plusoft-transcript-fidelity",
    "pratique-cobranca-json-contract",
    "recipient-limbo",
    "sara-assurant-egress-contract",
    "tenant-identity",
    "tenant-override-contract",
    "pdf-studio-env-coherence",
];

pub fn ci_doctor_names() -> Vec<&'static str> {
    let mut v: Vec<&str> = BASELINE_DOCTOR_NAMES.to_vec();
    for &n in CI_EXTRA_DOCTOR_NAMES {
        if !v.contains(&n) {
            v.push(n);
        }
    }
    v
}

fn run_named_doctors_parallel(
    names: &[&str],
    index: &RepoIndex,
    root: &Path,
    query_id_kind: &str,
    label: &str,
) -> QueryEnvelope {
    let started = Instant::now();
    let profile = repo_profile(root);

    struct Row {
        name: String,
        skipped: bool,
        summary: String,
        warning_count: usize,
        evidence_count: usize,
        query_id: String,
        warnings: Vec<String>,
        evidence: Vec<crate::model::EvidenceItem>,
        timing_ms: u128,
    }

    let mut rows: Vec<Row> = names
        .par_iter()
        .copied()
        .filter_map(|name| {
            let t0 = Instant::now();
            let envelope = run_doctor(name, index, root)?;
            let timing_ms = t0.elapsed().as_millis();
            let skipped = envelope.warnings.is_empty()
                && envelope
                    .summary
                    .contains("not available for workspace profile");
            Some(Row {
                name: name.to_string(),
                skipped,
                summary: envelope.summary,
                warning_count: envelope.warnings.len(),
                evidence_count: envelope.evidence.len(),
                query_id: envelope.query_id,
                warnings: envelope.warnings,
                evidence: envelope.evidence,
                timing_ms,
            })
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));

    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();
    let mut total_warnings = 0usize;
    let mut failing_doctors = 0usize;
    let mut ran_count = 0usize;
    let mut doctor_timings_ms = serde_json::Map::new();

    for row in rows {
        doctor_timings_ms.insert(row.name.clone(), json!(row.timing_ms));
        if row.skipped {
            entities.push(json!({
                "doctor": row.name,
                "skipped": true,
                "summary": row.summary,
                "timing_ms": row.timing_ms,
            }));
            continue;
        }
        ran_count += 1;
        if row.warning_count > 0 {
            failing_doctors += 1;
            evidence.extend(row.evidence.clone());
        }
        total_warnings += row.warning_count;
        warnings.extend(
            row.warnings
                .iter()
                .map(|warning| format!("[{}] {}", row.name, warning)),
        );
        entities.push(json!({
            "doctor": row.name,
            "summary": row.summary,
            "warning_count": row.warning_count,
            "evidence_count": row.evidence_count,
            "query_id": row.query_id,
            "timing_ms": row.timing_ms,
        }));
    }

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: utils::query_id(query_id_kind),
        kind: query_id_kind.to_string(),
        summary: format!(
            "{label}: ran {ran_count} doctors for profile `{profile}`, {total_warnings} warnings across {failing_doctors} failing doctors"
        ),
        confidence: if total_warnings == 0 { 0.98 } else { 0.68 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "workspace_profile": profile,
            "doctor_names": names,
            "doctor_count": ran_count,
            "failing_doctors": failing_doctors,
            "warning_count": total_warnings,
            "doctor_timings_ms": doctor_timings_ms,
            "parallel": true,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// Fast, file-local doctors suitable for `status --strict` and quick CI gates.
pub fn run_baseline_doctors(index: &RepoIndex, root: &Path) -> QueryEnvelope {
    run_named_doctors_parallel(
        BASELINE_DOCTOR_NAMES,
        index,
        root,
        "doctor_baseline",
        "baseline",
    )
}

/// Baseline plus a few high-value wiring doctors (still file-local).
pub fn run_ci_doctors(index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let names = ci_doctor_names();
    run_named_doctors_parallel(names.as_slice(), index, root, "doctor_ci", "ci")
}

fn registry() -> Vec<RegisteredDoctor> {
    vec![
        RegisteredDoctor {
            profiles: &[PROFILE_LEIO_CODE],
            doctor: Box::new(self_contract::SelfContractDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE, PROFILE_GENERIC],
            doctor: Box::new(skill_contract::SkillContractDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(deploy::DeployDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(pdf_studio_env_coherence::PdfStudioEnvCoherenceDoctor),
        },
        RegisteredDoctor {
            // Deploy-machine preflight: audits the gitignored local secret
            // bundle, so it only fires where that bundle exists.
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(deploy_bundle_critical_keys::DeployBundleCriticalKeysDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(duckdb_contract::DuckdbContractDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(semantic_wiring::SemanticWiringDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(tessellation_contract::TessellationContractDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(session_hot_state::SessionHotStateDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(sisfron_ooda_runtime::SisfronOodaRuntimeDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(sisfron_simulation_durability::SisfronSimulationDurabilityDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(onboarding_projection::OnboardingProjectionDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(onboarding_drift::OnboardingDriftDoctor),
        },
        RegisteredDoctor {
            // Generic-promoted: surfaces are config-driven; without config
            // the doctor no-ops on generic and keeps const defaults on
            // example, so promotion cannot fire new warnings anywhere.
            profiles: &[PROFILE_EXAMPLE, PROFILE_GENERIC],
            doctor: Box::new(orphan_files::OrphanFilesDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(
                pacto_webhook_allowlist_populated::PactoWebhookAllowlistPopulatedDoctor,
            ),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(prod_surface_hygiene::ProdSurfaceHygieneDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(oaei_doc_consistency::OaeiDocConsistencyDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(office_parsers_node_parity::OfficeParsersNodeParityDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(office_parsers_arrow_ipc::OfficeParsersArrowIpcDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(office_parsers_clippy_gate::OfficeParsersClippyGateDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(office_parsers_doc_coverage::OfficeParsersDocCoverageDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(fast_wheelhouse_contract::FastWheelhouseContractDoctor),
        },
        RegisteredDoctor {
            // Generic-promoted: scans index redis_keys only; with zero keys
            // it reports a clean "0/0/0" summary and no warnings.
            profiles: &[PROFILE_EXAMPLE, PROFILE_GENERIC],
            doctor: Box::new(redis_key_hygiene::RedisKeyHygieneDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(revops_tenant_gate::RevopsTenantGateDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(lgpd_outbound_filter::LgpdOutboundFilterDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(route_projection::RouteProjectionDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(script_path_existence::ScriptPathExistenceDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(secret_set_parity::SecretSetParityDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(auth_brokering::AuthBrokeringDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(health_audit_auth::HealthAuditAuthDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(cartridge_boundary::CartridgeBoundaryDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(composition_resolver::CompositionResolverDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(chatwoot_pratique_sync::ChatwootPratiqueSyncDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(egress_compliance::EgressComplianceDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(event_durability::EventDurabilityDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(event_envelope::EventEnvelopeDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(flight_contract_auth::FlightContractAuthDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(flight_secret_propagation::FlightRuntimeAuthDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(gateway_oar_boundary::GatewayOarBoundaryDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(gateway_ocr_pipeline::GatewayOcrPipelineDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(ocr_models_on_disk::OcrModelsOnDiskDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(ocr_canonical_layout::OcrCanonicalLayoutDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(gliner_shared_surface::GlinerSharedSurfaceDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(health_audit_ans_xsd::HealthAuditAnsXsdDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(health_audit_embedding::HealthAuditEmbeddingDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(health_audit_sentinel::HealthAuditSentinelDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(luminai_health_audit_isolation::LuminaiHealthAuditIsolationDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(health_audit_router_size::HealthAuditRouterSizeDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(health_audit_contract_airgap::HealthAuditContractAirgapDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(health_audit_worker_runtime::HealthAuditWorkerRuntimeDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(glosa_contract::GlosaContractDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(clinical_contract::ClinicalContractDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(generate_async_flight::GenerateAsyncFlightDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(flight_server_zero_copy::FlightServerZeroCopyDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(flight_server_context_isolation::FlightServerContextIsolationDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(health_audit_evidence_class::HealthAuditEvidenceClassDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(
                health_audit_audit_trail_durability::HealthAuditAuditTrailDurabilityDoctor,
            ),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(health_audit_engine_health::HealthAuditEngineHealthDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(flight_channel_reuse::FlightChannelReuseDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(grpc_message_size::GrpcMessageSizeDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(auth_jwt_compat::AuthJwtCompatDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(auth_session_continuity::AuthSessionContinuityDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(health_audit_redis_primacy::HealthAuditRedisPrimacyDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(inference_contracts::InferenceContractsDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(llm_provider_egress::LlmProviderEgressDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(manus_residue::ManusResidueDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(modal_app_naming_coherence::ModalAppNamingCoherenceDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(
                align_dockerfile_path_dep_coherence::AlignDockerfilePathDepCoherenceDoctor,
            ),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(
                server_dockerfile_context_coherence::ServerDockerfileContextCoherenceDoctor,
            ),
        },
        RegisteredDoctor {
            // Generic-promoted: self-skips cleanly when rust-toolchain.toml
            // is missing or pins a named channel (stable/nightly).
            profiles: &[PROFILE_EXAMPLE, PROFILE_GENERIC],
            doctor: Box::new(rust_toolchain_pin_coherence::RustToolchainPinCoherenceDoctor),
        },
        RegisteredDoctor {
            // Workspace-wide Arrow/ORT/DuckDB/Parquet ABI pin enforcement.
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(rust_dependency_baseline::RustDependencyBaselineDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(frontend_congruence::FrontendCongruenceDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(frontend_readiness::FrontendReadinessDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(conversation_identity::ConversationIdentityDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(vigoros_swarm::VigorosSwarmDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(typescript_config_hygiene::TypescriptConfigHygieneDoctor),
        },
        // ---- Drift-pattern doctors added 2026-05-04 (see commit message) ----
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(agent_seed_update_completeness::AgentSeedUpdateCompletenessDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(startup_hook_cartridge_coverage::StartupHookCartridgeCoverageDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(worker_mem_budget::WorkerMemBudgetDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(bare_cartridge_redis_key::BareCartridgeRedisKeyDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(egress_success_without_wamid::EgressSuccessWithoutWamidDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(active_cartridges_env_drift::ActiveCartridgesEnvDriftDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(assurant_seed_wiring::AssurantSeedWiringDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(assurant_ops_production::AssurantOpsProductionDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(ocr_flight_wiring::OcrFlightWiringDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(parse_hybrid_fallback::ParseHybridFallbackDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(layout_fast_spectral_contract::LayoutFastSpectralContractDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(layout_contract_platform::LayoutContractPlatformDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(pdf_pipeline_divergence::PdfPipelineDivergenceDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(audio_handler_timeout::AudioHandlerTimeoutDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(kb_collection_exists::KbCollectionExistsDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(fitness_member_unit_resolver::FitnessMemberUnitResolverDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(evo_credential_isolation::EvoCredentialIsolationDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(c4gym_evo_integration::C4GymEvoIntegrationDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(jaipay_pacto_gcp_webhook::JaipayPactoGcpWebhookDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(jaipay_supabase_session_pooler::JaipaySupabaseSessionPoolerDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(supabase_runtime_shape::SupabaseRuntimeShapeDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(compose_worker_mem_budget::ComposeWorkerMemBudgetDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(supabase_project_liveness::SupabaseProjectLivenessDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(liz_jaipay_pacto_lookup_contract::LizJaiPayPactoLookupContractDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(plusoft_routing_contract::PlusoftRoutingContractDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(
                plusoft_handover_payload_contract::PlusoftHandoverPayloadContractDoctor,
            ),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(plusoft_transcript_fidelity::PlusoftTranscriptFidelityDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(pacto_drain_dependency::PactoDrainDependencyDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(test_patch_target_integrity::TestPatchTargetIntegrityDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(test_route_mount_integrity::TestRouteMountIntegrityDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(pratique_cobranca_json_contract::PratiqueCobrancaJsonContractDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(ontology_price_consistency::OntologyPriceConsistencyDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(recipient_limbo::RecipientLimboDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(sara_assurant_egress_contract::SaraAssurantEgressContractDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(whatsapp_bsuid::WhatsAppBsuidWebhookDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(whatsapp_bsuid::WhatsAppBsuidCrmDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(whatsapp_bsuid::WhatsAppDisplayNameOnlyDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(revops_snapshot_schema_sync::RevopsSnapshotSchemaSyncDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(tenant_identity::TenantIdentityDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(tenant_override_contract::TenantOverrideContractDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(operator_tenant_parity::OperatorTenantParityDoctor),
        },
        // Slop gate for deck/report repos. Registered under every profile;
        // the doctor is inert unless `spine.json` is present at the repo root
        // (blast-radius-zero gate), so it's safe to advertise everywhere.
        RegisteredDoctor {
            profiles: &[PROFILE_GENERIC, PROFILE_LEIO_CODE, PROFILE_EXAMPLE],
            doctor: Box::new(slop_gate::SlopGateDoctor),
        },
        // ---- Generic doctor pack (2026-06) ----
        // env-contract is generic-only by design: the Example workspace
        // declares many vars in deploy-time environments outside the repo,
        // so registering it under PROFILE_EXAMPLE would flood `doctor all`
        // with environment facts (and break the CI green contract).
        RegisteredDoctor {
            profiles: &[PROFILE_GENERIC],
            doctor: Box::new(env_contract::EnvContractDoctor),
        },
        // import-boundary no-ops with zero configured rules (and never loads
        // the code graph in that case), so it is safe under every profile.
        RegisteredDoctor {
            profiles: &[PROFILE_GENERIC, PROFILE_LEIO_CODE, PROFILE_EXAMPLE],
            doctor: Box::new(import_boundary::ImportBoundaryDoctor),
        },
        // repo-hygiene is workspace-agnostic: it audits git-tracked files for
        // committed noise (conflict leftovers, ignore-worthy artifacts,
        // byte-identical large duplicates). Safe under every profile — it
        // self-skips when git is unavailable and never flags legitimate data.
        RegisteredDoctor {
            profiles: &[PROFILE_GENERIC, PROFILE_LEIO_CODE, PROFILE_EXAMPLE],
            doctor: Box::new(repo_hygiene::RepoHygieneDoctor),
        },
        // Portable Codex orchestration and LEIO release gates. Each is inert
        // until its own tracked contract surfaces exist, so they are safe to
        // advertise under every workspace profile.
        RegisteredDoctor {
            profiles: &[PROFILE_GENERIC, PROFILE_LEIO_CODE, PROFILE_EXAMPLE],
            doctor: Box::new(codex_orchestration::CodexOrchestrationDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_GENERIC, PROFILE_LEIO_CODE, PROFILE_EXAMPLE],
            doctor: Box::new(leio_release_coherence::LeioReleaseCoherenceDoctor),
        },
        // induced-invariants mines stable single-premise env/redis
        // co-occurrence rules from the graph-covered incidence set and warns on
        // current violations. Workspace-specific (Example) since the induced
        // invariants are facts about this repository's structure.
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(induced_invariants::InducedInvariantsDoctor),
        },
        // publishable-crate wires down the Example Sovereign Crate contract:
        // crates opt in via `[package.metadata.example] tier = "sovereign"` and
        // are then held to cargo-publish mechanics + discoverability metadata +
        // standalone quality (+ OSI license/CHANGELOG when public).
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(publishable_crate::PublishableCrateDoctor),
        },
        // Copy-vendoring has no contract of its own: this holds every crate
        // under vendor/ to a recorded upstream commit + content hash. Registered
        // for leio-code because that is where the first-party copies live, and
        // for example so the same rule applies if that tree ever vendors one.
        // Deliberately not PROFILE_GENERIC: an unrelated repo's `cargo vendor`
        // output is third-party and not ours to record.
        RegisteredDoctor {
            profiles: &[PROFILE_LEIO_CODE, PROFILE_EXAMPLE],
            doctor: Box::new(vendored_crate_provenance::VendoredCrateProvenanceDoctor),
        },
        // py-rust-boundary is the composite gate for Python↔Rust seam drift.
        // It runs child boundary doctors internally; register it before
        // artifact-reuse so the registry index stays aligned with MCP lists.
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(py_rust_boundary::PyRustBoundaryDoctor),
        },
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(platform_runtime_trust_boundary::PlatformRuntimeTrustBoundaryDoctor),
        },
        // artifact-reuse enforces the consolidated artifacts/ reorg: wheels
        // centralized under artifacts/wheels/manylinux/, consumers repointed off
        // the retired wheels/ root, phantom *-fast-node pins gone, and the
        // manifest in sync with disk. Workspace-specific (Example). Registered
        // LAST so the registry index stays aligned with the trailing entry in
        // mcp/index.js (DOCTOR_KINDS) and apps-sdk/server.js (doctorKinds).
        RegisteredDoctor {
            profiles: &[PROFILE_EXAMPLE],
            doctor: Box::new(artifact_reuse::ArtifactReuseDoctor),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ci_gate_includes_supabase_runtime_shape() {
        let names = ci_doctor_names();

        assert!(
            names.contains(&"supabase-runtime-shape"),
            "expected doctor ci to include supabase-runtime-shape; got {names:?}"
        );
    }
}
