//! Assurant seed-wiring doctor.
//!
//! The Assurant cartridge cannot answer a single routing/normalization request
//! without its raw operational data: the routing Parquet catalog
//! (service centers, manufacturers, risk-type→linha, ZIP coverage), the
//! committed ontology (TBox/SHACL/JSON-LD context), and the Sara runtime
//! state. That data is materialized at container boot by
//! `example-api/scripts/seed_assurant_ops.py`, which `docker-compose.yml`
//! invokes **only** when `EXAMPLE_ACTIVE_CARTRIDGES` contains `assurant`.
//!
//! This doctor verifies that the "active ⇒ seeded" path stays intact so the
//! cartridge never boots active-but-starved:
//!
//!   1. the seed script exists;
//!   2. `docker-compose.yml` runs `seed_assurant_ops.py`, gated on `,assurant,`;
//!   3. the git-tracked raw-data source the seed/ingest depends on — the
//!      ontology TBox — is present (the routing Parquet is intentionally
//!      externalized/gitignored and synced at seed time, so it is NOT required
//!      to be committed);
//!   4. no deploy profile activates `assurant` while disabling the boot seed
//!      (`ASSURANT_SEED_ON_BOOT=false`), which would leave the data unseeded.
//!
//! Missing wiring is a warning, not a hard error: like the rest of the
//! Assurant data plane, seeding is fail-open, but a regression here is exactly
//! how the cartridge silently loses its raw data.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct AssurantSeedWiringDoctor;

impl Doctor for AssurantSeedWiringDoctor {
    fn name(&self) -> &'static str {
        "assurant-seed-wiring"
    }

    fn description(&self) -> &'static str {
        "Verifies the Assurant cartridge's raw-data seed (Parquet catalog + ontology) is wired to run on boot whenever the cartridge is active."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_assurant_seed_wiring(root)
    }
}

const CARTRIDGE_REL: &str = "cartridges/assurant";
const SEED_SCRIPT_REL: &str = "example-api/scripts/seed_assurant_ops.py";
const COMPOSE_REL: &str = "example-api/docker-compose.yml";
const ONTOLOGY_TBOX_REL: &str = "cartridges/assurant/ontologies/assurant-service-center-tbox.ttl";
const STAGE_SCRIPT_REL: &str = "deploy/scripts/stage-assurant-gpu-artifacts.sh";
const PROFILES_DIR_REL: &str = "deploy/profiles";
const SERVICE_CENTER_PROFILE: &str = "deploy/profiles/assurant_service_center_ops.env";
const CARTRIDGE: &str = "assurant";

pub fn doctor_assurant_seed_wiring(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();
    let mut io = Vec::new();

    // Skip cleanly when the cartridge is not present in this checkout.
    if !root.join(CARTRIDGE_REL).is_dir() {
        entities.push(json!({
            "doctor": "assurant-seed-wiring",
            "skipped": true,
            "reason": "cartridges/assurant not present",
        }));
        return envelope(
            "cartridges/assurant not present; skipped".to_string(),
            0.9,
            entities,
            evidence,
            warnings,
            started,
        );
    }

    // 1. Seed script must exist.
    let seed_present = root.join(SEED_SCRIPT_REL).is_file();
    let seed_body = read_text(&root.join(SEED_SCRIPT_REL), &mut io).unwrap_or_default();
    if !seed_present {
        warnings.push(format!(
            "{}: Assurant seed script is missing; an active cartridge has nothing to materialize its routing/ontology data",
            SEED_SCRIPT_REL
        ));
        evidence.push(EvidenceItem {
            kind: "assurant_seed_script_missing".to_string(),
            path: SEED_SCRIPT_REL.to_string(),
            line: None,
            detail: "expected the boot seed script for Assurant raw data".to_string(),
        });
    } else if !seed_body.contains("seed_prediction_models") {
        warnings.push(format!(
            "{}: seed script does not stage prediction models; claims inference may fall back to heuristics",
            SEED_SCRIPT_REL
        ));
        evidence.push(EvidenceItem {
            kind: "assurant_prediction_seed_missing".to_string(),
            path: SEED_SCRIPT_REL.to_string(),
            line: None,
            detail: "expected seed_assurant_ops.py to copy/upload prediction artifacts".to_string(),
        });
    }

    // 2. docker-compose must run the seed on boot, gated on the active cartridge.
    let compose_path = root.join(COMPOSE_REL);
    let compose_body = read_text(&compose_path, &mut io).unwrap_or_default();
    let runs_seed = compose_body.contains("seed_assurant_ops.py");
    let gates_on_assurant = compose_body.contains(",assurant,");
    if !runs_seed {
        warnings.push(format!(
            "{}: does not invoke `seed_assurant_ops.py`; Assurant raw data will not be seeded on boot",
            COMPOSE_REL
        ));
        evidence.push(EvidenceItem {
            kind: "assurant_seed_not_invoked".to_string(),
            path: COMPOSE_REL.to_string(),
            line: None,
            detail: "compose command should run `python scripts/seed_assurant_ops.py` when assurant is active".to_string(),
        });
    } else if !gates_on_assurant {
        // The seed runs but is not gated on the active-cartridge list. This is
        // not fatal (it would just run unconditionally) but it breaks the
        // documented "seed only when active" contract.
        warnings.push(format!(
            "{}: runs `seed_assurant_ops.py` but is not gated on `,assurant,` in EXAMPLE_ACTIVE_CARTRIDGES",
            COMPOSE_REL
        ));
        evidence.push(EvidenceItem {
            kind: "assurant_seed_ungated".to_string(),
            path: COMPOSE_REL.to_string(),
            line: find_line(&compose_body, "seed_assurant_ops.py"),
            detail: "expected the seed invocation to be gated on the active-cartridge list"
                .to_string(),
        });
    } else if let Some(line) = find_line(&compose_body, "seed_assurant_ops.py") {
        evidence.push(EvidenceItem {
            kind: "assurant_seed_invocation".to_string(),
            path: COMPOSE_REL.to_string(),
            line: Some(line),
            detail: "compose seeds Assurant raw data on boot, gated on the active cartridge"
                .to_string(),
        });
    }

    // 3. The git-tracked raw-data source (ontology TBox) must be present. The
    //    routing Parquet is intentionally externalized (gitignored, synced from
    //    object storage at seed time), so it is deliberately not required here.
    let ontology_present = root.join(ONTOLOGY_TBOX_REL).is_file();
    if !ontology_present {
        warnings.push(format!(
            "{}: Assurant ontology TBox is missing; the seed/ingest path has no committed knowledge source",
            ONTOLOGY_TBOX_REL
        ));
        evidence.push(EvidenceItem {
            kind: "assurant_ontology_source_missing".to_string(),
            path: ONTOLOGY_TBOX_REL.to_string(),
            line: None,
            detail: "expected the committed Assurant ontology TBox used by ingest".to_string(),
        });
    }

    let stage_body = read_text(&root.join(STAGE_SCRIPT_REL), &mut io).unwrap_or_default();
    if !stage_body.contains("assurant-models/prediction") {
        warnings.push(format!(
            "{}: does not stage prediction model artifacts to the VM seed dir",
            STAGE_SCRIPT_REL
        ));
        evidence.push(EvidenceItem {
            kind: "assurant_prediction_stage_missing".to_string(),
            path: STAGE_SCRIPT_REL.to_string(),
            line: None,
            detail: "expected stage-assurant-gpu-artifacts.sh to rsync prediction PKLs".to_string(),
        });
    }

    let sc_profile_body =
        read_text(&root.join(SERVICE_CENTER_PROFILE), &mut io).unwrap_or_default();
    if profile_activates_assurant(&sc_profile_body)
        && !profile_env_true(&sc_profile_body, "ASSURANT_SEED_PREDICTION_MODELS")
    {
        warnings.push(format!(
            "{}: activates assurant but does not enable ASSURANT_SEED_PREDICTION_MODELS",
            SERVICE_CENTER_PROFILE
        ));
        evidence.push(EvidenceItem {
            kind: "assurant_prediction_seed_disabled".to_string(),
            path: SERVICE_CENTER_PROFILE.to_string(),
            line: find_line(&sc_profile_body, "ASSURANT_SEED_PREDICTION_MODELS"),
            detail: "service-center ops profile should seed prediction models on boot".to_string(),
        });
    }

    // 4. No deploy profile may activate assurant while disabling the boot seed.
    let mut active_profiles: Vec<String> = Vec::new();
    let mut seed_disabled_profiles: Vec<String> = Vec::new();
    let profiles_dir = root.join(PROFILES_DIR_REL);
    if let Ok(entries) = std::fs::read_dir(&profiles_dir) {
        let mut paths: Vec<std::path::PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "env").unwrap_or(false))
            .collect();
        paths.sort();
        for path in paths {
            let body = read_text(&path, &mut io).unwrap_or_default();
            if !profile_activates_assurant(&body) {
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            active_profiles.push(rel.clone());
            if profile_disables_boot_seed(&body) {
                seed_disabled_profiles.push(rel.clone());
                warnings.push(format!(
                    "{}: activates `assurant` but sets ASSURANT_SEED_ON_BOOT=false; raw data will never be seeded for this profile",
                    rel
                ));
                evidence.push(EvidenceItem {
                    kind: "assurant_active_seed_disabled".to_string(),
                    path: rel,
                    line: find_line(&body, "ASSURANT_SEED_ON_BOOT"),
                    detail: "profile activates assurant but disables the boot seed".to_string(),
                });
            }
        }
    }

    entities.push(json!({
        "doctor": "assurant-seed-wiring",
        "seed_script_present": seed_present,
        "compose_runs_seed": runs_seed,
        "compose_gates_on_assurant": gates_on_assurant,
        "ontology_source_present": ontology_present,
        "active_profiles": active_profiles,
        "seed_disabled_profiles": seed_disabled_profiles,
    }));

    warnings.extend(io);

    let summary = if warnings.is_empty() {
        format!(
            "Assurant seed wiring intact: boot seed gated on active cartridge across {} profile(s)",
            active_profiles_count(&entities)
        )
    } else {
        format!(
            "Assurant seed wiring has {} issue(s); raw data may not be seeded when the cartridge is active",
            warnings.len()
        )
    };

    envelope(
        summary,
        if warnings.is_empty() { 0.95 } else { 0.55 },
        entities,
        evidence,
        warnings,
        started,
    )
}

fn active_profiles_count(entities: &[serde_json::Value]) -> usize {
    entities
        .first()
        .and_then(|e| e.get("active_profiles"))
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0)
}

/// True when the profile's `EXAMPLE_ACTIVE_CARTRIDGES` lists `assurant`.
fn profile_activates_assurant(body: &str) -> bool {
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("EXAMPLE_ACTIVE_CARTRIDGES=") else {
            continue;
        };
        let rest = rest.trim().trim_matches('"').trim_matches('\'');
        return rest.split(',').map(|s| s.trim()).any(|s| s == CARTRIDGE);
    }
    false
}

/// True when the profile sets `ASSURANT_SEED_ON_BOOT` to a falsey value.
fn profile_disables_boot_seed(body: &str) -> bool {
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("ASSURANT_SEED_ON_BOOT=") else {
            continue;
        };
        let value = rest
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_lowercase();
        return matches!(value.as_str(), "false" | "0" | "no" | "off");
    }
    false
}

/// True when the profile sets `NAME=true` (or other truthy boot-seed values).
fn profile_env_true(body: &str, name: &str) -> bool {
    let prefix = format!("{name}=");
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix(prefix.as_str()) else {
            continue;
        };
        let value = rest
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_lowercase();
        return matches!(value.as_str(), "true" | "1" | "yes" | "on");
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn envelope(
    summary: String,
    confidence: f32,
    entities: Vec<serde_json::Value>,
    evidence: Vec<EvidenceItem>,
    warnings: Vec<String>,
    started: Instant,
) -> QueryEnvelope {
    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_assurant_seed_wiring"),
        kind: "doctor".to_string(),
        summary,
        confidence,
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_repo(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-assurant-seed-{label}-{}-{nanos}",
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

    fn wire_intact(root: &Path) {
        fs::create_dir_all(root.join(CARTRIDGE_REL)).unwrap();
        write(
            root,
            SEED_SCRIPT_REL,
            "def seed_prediction_models(): pass\n",
        );
        write(root, ONTOLOGY_TBOX_REL, "@prefix ta: <> .\n");
        write(
            root,
            COMPOSE_REL,
            "services:\n  api:\n    command: /bin/sh -lc 'if printf \",%s,\" \"$${EXAMPLE_ACTIVE_CARTRIDGES:-}\" | grep -q \",assurant,\"; then python scripts/seed_assurant_ops.py; fi; exec gunicorn'\n",
        );
        write(
            root,
            "deploy/profiles/assurant.env",
            "EXAMPLE_ACTIVE_CARTRIDGES=assurant\nASSURANT_SEED_ON_BOOT=true\nASSURANT_SEED_PREDICTION_MODELS=true\n",
        );
        write(
            root,
            STAGE_SCRIPT_REL,
            "MODEL_SEED_DIR=/opt/example/seed/assurant-models/prediction\n",
        );
        write(
            root,
            SERVICE_CENTER_PROFILE,
            "EXAMPLE_ACTIVE_CARTRIDGES=assurant\nASSURANT_SEED_ON_BOOT=true\nASSURANT_SEED_PREDICTION_MODELS=true\n",
        );
    }

    #[test]
    fn intact_wiring_is_silent() {
        let root = temp_repo("intact");
        wire_intact(&root);
        let env = doctor_assurant_seed_wiring(&root);
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_seed_invocation_is_flagged() {
        let root = temp_repo("noseed");
        wire_intact(&root);
        // Compose no longer runs the seed.
        write(
            &root,
            COMPOSE_REL,
            "services:\n  api:\n    command: exec gunicorn\n",
        );
        let env = doctor_assurant_seed_wiring(&root);
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("seed_assurant_ops.py")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn active_profile_with_seed_disabled_is_flagged() {
        let root = temp_repo("disabled");
        wire_intact(&root);
        write(
            &root,
            "deploy/profiles/assurant_no_seed.env",
            "EXAMPLE_ACTIVE_CARTRIDGES=assurant,plusoft\nASSURANT_SEED_ON_BOOT=false\n",
        );
        let env = doctor_assurant_seed_wiring(&root);
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("ASSURANT_SEED_ON_BOOT=false")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_ontology_source_is_flagged() {
        let root = temp_repo("noont");
        wire_intact(&root);
        fs::remove_file(root.join(ONTOLOGY_TBOX_REL)).unwrap();
        let env = doctor_assurant_seed_wiring(&root);
        assert!(
            env.warnings.iter().any(|w| w.contains("ontology TBox")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn absent_cartridge_skips() {
        let root = temp_repo("absent");
        let env = doctor_assurant_seed_wiring(&root);
        assert!(env.warnings.is_empty());
        assert!(env.summary.contains("skipped"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn profile_with_assurant_substring_is_not_a_false_positive() {
        let root = temp_repo("substr");
        wire_intact(&root);
        // A cartridge whose name merely contains "assurant" must not count.
        write(
            &root,
            "deploy/profiles/other.env",
            "EXAMPLE_ACTIVE_CARTRIDGES=assurant_lookalike\nASSURANT_SEED_ON_BOOT=false\n",
        );
        let env = doctor_assurant_seed_wiring(&root);
        // assurant.env (intact) is the only real activation; the lookalike is ignored.
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }
}
