//! Generic doctor execution. Repository policy is discovered from local packs.
use crate::config::repo_profile;
use crate::model::{QueryEnvelope, RepoIndex};
use serde_json::json;
use std::path::Path;
use std::time::Instant;
pub mod env_contract;
pub mod import_boundary;
pub mod local_packs;
pub mod native;
pub mod orphan_files;
pub mod redis_key_hygiene;
pub mod repo_hygiene;
pub mod rust_toolchain_pin_coherence;
pub mod skill_contract;
pub mod slop_gate;
pub mod utils;
pub mod vendored_crate_provenance;
pub mod version_manifest;
pub trait Doctor {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope;
}
fn registry() -> Vec<Box<dyn Doctor>> {
    vec![
        Box::new(skill_contract::SkillContractDoctor),
        Box::new(orphan_files::OrphanFilesDoctor),
        Box::new(redis_key_hygiene::RedisKeyHygieneDoctor),
        Box::new(rust_toolchain_pin_coherence::RustToolchainPinCoherenceDoctor),
        Box::new(slop_gate::SlopGateDoctor),
        Box::new(env_contract::EnvContractDoctor),
        Box::new(import_boundary::ImportBoundaryDoctor),
        Box::new(repo_hygiene::RepoHygieneDoctor),
        Box::new(vendored_crate_provenance::VendoredCrateProvenanceDoctor),
    ]
}
pub fn doctor_names() -> Vec<&'static str> {
    registry().iter().map(|d| d.name()).collect()
}
pub fn doctor_names_for_profile(_profile: &str) -> Vec<&'static str> {
    doctor_names()
}
pub fn doctor_names_for_root(root: &Path) -> Vec<String> {
    let mut names: Vec<_> = doctor_names().into_iter().map(str::to_owned).collect();
    match native::discover(root) {
        Ok(Some(pack)) => names.extend(pack.doctors.iter().map(|d| d.name.clone())),
        Ok(None) => {}
        Err(_) => names.push("native-pack-configuration".into()),
    }
    names.extend(local_catalog(root).1.packs.into_keys());
    names
}
pub fn run_doctor(name: &str, index: &RepoIndex, root: &Path) -> Option<QueryEnvelope> {
    if let Some(doctor) = registry().into_iter().find(|d| d.name() == name) {
        return Some(doctor.run(index, root));
    }
    native::run_one(name, index, root).or_else(|| {
        local_rows(root, name, false)
            .into_iter()
            .next()
            .map(|(_, e)| e)
    })
}
pub const BASELINE_DOCTOR_NAMES: &[&str] = &["repo-hygiene"];
pub const CI_EXTRA_DOCTOR_NAMES: &[&str] = &["skill-contract"];
pub fn ci_doctor_names() -> Vec<&'static str> {
    BASELINE_DOCTOR_NAMES
        .iter()
        .chain(CI_EXTRA_DOCTOR_NAMES)
        .copied()
        .collect()
}
pub fn run_all_doctors(index: &RepoIndex, root: &Path) -> QueryEnvelope {
    run_suite(index, root, "all")
}
pub fn run_baseline_doctors(index: &RepoIndex, root: &Path) -> QueryEnvelope {
    run_suite(index, root, "baseline")
}
pub fn run_ci_doctors(index: &RepoIndex, root: &Path) -> QueryEnvelope {
    run_suite(index, root, "ci")
}
fn run_suite(index: &RepoIndex, root: &Path, suite: &str) -> QueryEnvelope {
    let started = Instant::now();
    let mut rows = Vec::new();
    let disabled = crate::config::load_repo_config(root)
        .and_then(|c| c.doctors)
        .map(|d| d.disabled)
        .unwrap_or_default();
    for doctor in registry() {
        if disabled.iter().any(|name| name == doctor.name()) {
            continue;
        }
        if suite == "all"
            || BASELINE_DOCTOR_NAMES.contains(&doctor.name())
            || (suite == "ci" && CI_EXTRA_DOCTOR_NAMES.contains(&doctor.name()))
        {
            rows.push((doctor.name().to_owned(), doctor.run(index, root)));
        }
    }
    rows.extend(native::run_suite(suite, index, root));
    rows.extend(local_rows(root, suite, true));
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut entities = Vec::new();
    let mut failing = 0;
    for (name, envelope) in rows {
        if !envelope.warnings.is_empty() {
            failing += 1;
            evidence.extend(envelope.evidence.clone());
        }
        warnings.extend(envelope.warnings.iter().map(|w| format!("[{name}] {w}")));
        entities.push(json!({"doctor":name,"summary":envelope.summary,"warning_count":envelope.warnings.len(),"evidence_count":envelope.evidence.len(),"query_id":envelope.query_id,"timing_ms":envelope.timing_ms}));
    }
    let count = entities.len();
    let n = warnings.len();
    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.into(),
        query_id: utils::query_id("doctor_suite"),
        kind: format!("doctor_{suite}"),
        summary: format!(
            "{suite}: ran {count} doctors, {n} warnings across {failing} failing doctors"
        ),
        confidence: if n == 0 { 0.98 } else { 0.68 },
        entities,
        evidence,
        warnings,
        meta: Some(
            json!({"workspace_profile":repo_profile(root),"doctor_count":count,"failing_doctors":failing,"warning_count":n}),
        ),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn local_catalog(
    root: &Path,
) -> (
    local_packs::LocalPackRequest,
    local_packs::LocalDoctorCatalog,
) {
    let request = local_packs::LocalPackRequest {
        root: root.to_path_buf(),
        content_mode: local_packs::LocalContentMode::WorkingTreeTracked,
        deadline: Instant::now() + std::time::Duration::from_secs(15),
        budgets: Default::default(),
    };
    let mut names: std::collections::BTreeSet<String> =
        doctor_names().into_iter().map(str::to_owned).collect();
    if let Ok(Some(pack)) = native::discover(root) {
        names.extend(pack.doctors.into_iter().map(|d| d.name));
    }
    let catalog = local_packs::discover_local_doctor_packs(&request, &names);
    (request, catalog)
}
fn local_rows(root: &Path, selected: &str, suite: bool) -> Vec<(String, QueryEnvelope)> {
    let (request, catalog) = local_catalog(root);
    let mut context = local_packs::LocalPackRequestContext::new(request);
    let mut rows = Vec::new();
    for (name, pack) in catalog.packs {
        if (suite && (selected == "all" || pack.suites.iter().any(|s| s == selected)))
            || (!suite && name == selected)
        {
            rows.push((
                name,
                local_packs::run_local_doctor_pack(&pack, &mut context),
            ));
        }
    }
    for d in catalog.diagnostics {
        rows.push((
            "local-pack-configuration".into(),
            QueryEnvelope {
                schema_version: crate::model::SCHEMA_VERSION.into(),
                query_id: utils::query_id("local_pack_error"),
                kind: "doctor".into(),
                summary: "local doctor pack configuration failed".into(),
                confidence: 0.0,
                entities: vec![],
                evidence: vec![],
                warnings: vec![format!("{}: {}: {}", d.relative_path, d.code, d.message)],
                meta: None,
                timing_ms: 0,
            },
        ));
    }
    rows
}
