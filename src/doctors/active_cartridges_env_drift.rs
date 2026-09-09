//! Active-cartridges env-drift doctor.
//!
//! Twice on 2026-05-04 the prod VM's `EXAMPLE_ACTIVE_CARTRIDGES` reverted from the full
//! 13-cartridge list to a 3-cartridge minimal list (likely a deploy script regenerating
//! `.env`). The doctor verifies the canonical workspace source — `deploy/profiles/platform.env`
//! — lists at least every cartridge that the code expects to be active:
//!
//!   - cartridges with a startup hook in `example-api/example/main.py`
//!     (anything imported as `from cartridges.<cart>.seed import seed_*_agent`)
//!
//! Missing cartridges produce a warning. Extra cartridges are fine — those just don't
//! have agent-seeding hooks but may still be active. The legacy `deploy-platform.sh`
//! default must also stay aligned because exported shell vars override compose env
//! files on the production VM.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use regex::Regex;
use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct ActiveCartridgesEnvDriftDoctor;

impl Doctor for ActiveCartridgesEnvDriftDoctor {
    fn name(&self) -> &'static str {
        "active-cartridges-env-drift"
    }

    fn description(&self) -> &'static str {
        "Verifies `deploy/profiles/platform.env` `EXAMPLE_ACTIVE_CARTRIDGES` is a superset of cartridges that have startup hooks in `example-api/example/main.py`."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_active_cartridges_env_drift(root)
    }
}

const PROFILE_REL: &str = "deploy/profiles/platform.env";
const MAIN_PY_REL: &str = "example-api/example/main.py";
const LEGACY_DEPLOY_REL: &str = "deploy/scripts/deploy-platform.sh";

pub fn doctor_active_cartridges_env_drift(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let profile_path = root.join(PROFILE_REL);
    let main_path = root.join(MAIN_PY_REL);
    let legacy_deploy_path = root.join(LEGACY_DEPLOY_REL);
    if !profile_path.is_file() || !main_path.is_file() {
        entities.push(json!({
            "doctor": "active-cartridges-env-drift",
            "skipped": true,
            "reason": "platform.env or main.py missing",
        }));
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_active_cartridges_env_drift"),
            kind: "doctor".to_string(),
            summary: "platform.env or main.py missing; skipped".to_string(),
            confidence: 0.9,
            entities,
            evidence,
            warnings,
            meta: None,
            timing_ms: started.elapsed().as_millis(),
        };
    }
    let mut io = Vec::new();
    let profile_body = read_text(&profile_path, &mut io).unwrap_or_default();
    let main_body = read_text(&main_path, &mut io).unwrap_or_default();
    let legacy_deploy_body = if legacy_deploy_path.is_file() {
        read_text(&legacy_deploy_path, &mut io).unwrap_or_default()
    } else {
        String::new()
    };
    warnings.extend(io);

    // 1. Extract the active list from platform.env.
    let active = extract_active_cartridges(&profile_body);
    // 2. Find expected cartridges via main.py imports.
    let import_re =
        Regex::new(r"from cartridges\.([a-zA-Z0-9_]+)\.seed import seed_").expect("valid regex");
    let mut expected: BTreeSet<String> = BTreeSet::new();
    for caps in import_re.captures_iter(&main_body) {
        if let Some(name) = caps.get(1) {
            expected.insert(name.as_str().to_string());
        }
    }

    let mut missing: Vec<String> = expected
        .iter()
        .filter(|c| !active.contains(*c))
        .cloned()
        .collect();
    missing.sort();

    for cart in &missing {
        warnings.push(format!(
            "{}: cartridge `{}` has a startup hook in {} but is NOT in EXAMPLE_ACTIVE_CARTRIDGES",
            PROFILE_REL, cart, MAIN_PY_REL
        ));
        evidence.push(EvidenceItem {
            kind: "active_cartridges_missing".to_string(),
            path: PROFILE_REL.to_string(),
            line: None,
            detail: format!(
                "expected cartridge `{}` based on main.py import; not present in EXAMPLE_ACTIVE_CARTRIDGES",
                cart
            ),
        });
    }

    let legacy_default = extract_legacy_deploy_default_cartridges(&legacy_deploy_body);
    let mut legacy_missing: Vec<String> = active
        .iter()
        .filter(|c| !legacy_default.is_empty() && !legacy_default.contains(*c))
        .cloned()
        .collect();
    legacy_missing.sort();

    for cart in &legacy_missing {
        warnings.push(format!(
            "{}: legacy ACTIVE_CARTRIDGES default omits `{}` from {} and can override the profile on deploy",
            LEGACY_DEPLOY_REL, cart, PROFILE_REL
        ));
        evidence.push(EvidenceItem {
            kind: "active_cartridges_legacy_default_missing".to_string(),
            path: LEGACY_DEPLOY_REL.to_string(),
            line: None,
            detail: format!(
                "legacy ACTIVE_CARTRIDGES default omits `{}` from platform profile",
                cart
            ),
        });
    }

    entities.push(json!({
        "doctor": "active-cartridges-env-drift",
        "active_cartridges": active.iter().collect::<Vec<_>>(),
        "expected_from_main": expected.iter().collect::<Vec<_>>(),
        "missing": missing,
        "legacy_default_cartridges": legacy_default.iter().collect::<Vec<_>>(),
        "legacy_default_missing": legacy_missing,
    }));

    let summary = if warnings.is_empty() {
        format!(
            "platform.env active list ({} entries) covers all {} cartridges with main.py startup hooks",
            active.len(),
            expected.len()
        )
    } else {
        format!(
            "platform.env active list is missing {} cartridge(s) referenced by main.py startup hooks",
            missing.len()
        )
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_active_cartridges_env_drift"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.95 } else { 0.55 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn extract_active_cartridges(env_body: &str) -> BTreeSet<String> {
    for line in env_body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("EXAMPLE_ACTIVE_CARTRIDGES=") else {
            continue;
        };
        // Strip surrounding quotes if any.
        let rest = rest.trim().trim_matches('"').trim_matches('\'');
        return rest
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    BTreeSet::new()
}

fn extract_legacy_deploy_default_cartridges(script_body: &str) -> BTreeSet<String> {
    let re = Regex::new(r#"ACTIVE_CARTRIDGES="\$\{EXAMPLE_ACTIVE_CARTRIDGES:-([^}]*)\}""#)
        .expect("valid regex");
    let Some(caps) = re.captures(script_body) else {
        return BTreeSet::new();
    };
    let raw = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
    if raw.trim().starts_with('$') {
        return BTreeSet::new();
    }
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
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
            "leio-code-active-cart-{label}-{}-{nanos}",
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

    #[test]
    fn missing_cartridge_is_flagged() {
        let root = temp_repo("missing");
        write(
            &root,
            PROFILE_REL,
            "EXAMPLE_ACTIVE_CARTRIDGES=liz_cobranca\n",
        );
        write(
            &root,
            MAIN_PY_REL,
            "from cartridges.liz_cobranca.seed import seed_liz_agent\n\
             from cartridges.pratique_cobranca.seed import seed_pratique_agent\n",
        );
        let env = doctor_active_cartridges_env_drift(&root);
        assert_eq!(env.warnings.len(), 1, "{:?}", env.warnings);
        assert!(env.warnings[0].contains("pratique_cobranca"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn complete_list_is_silent() {
        let root = temp_repo("complete");
        write(
            &root,
            PROFILE_REL,
            "EXAMPLE_ACTIVE_CARTRIDGES=liz_cobranca,pratique_cobranca,extra\n",
        );
        write(
            &root,
            LEGACY_DEPLOY_REL,
            "ACTIVE_CARTRIDGES=\"${EXAMPLE_ACTIVE_CARTRIDGES:-liz_cobranca,pratique_cobranca,extra}\"\n",
        );
        write(
            &root,
            MAIN_PY_REL,
            "from cartridges.liz_cobranca.seed import seed_liz_agent\n\
             from cartridges.pratique_cobranca.seed import seed_pratique_agent\n",
        );
        let env = doctor_active_cartridges_env_drift(&root);
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_legacy_deploy_default_is_flagged() {
        let root = temp_repo("legacy");
        write(
            &root,
            PROFILE_REL,
            "EXAMPLE_ACTIVE_CARTRIDGES=assurant,insurance_agent,plusoft\n",
        );
        write(
            &root,
            LEGACY_DEPLOY_REL,
            "ACTIVE_CARTRIDGES=\"${EXAMPLE_ACTIVE_CARTRIDGES:-assurant}\"\n",
        );
        write(&root, MAIN_PY_REL, "");

        let env = doctor_active_cartridges_env_drift(&root);

        assert_eq!(env.warnings.len(), 2, "{:?}", env.warnings);
        assert!(
            env.warnings
                .iter()
                .any(|warning| warning.contains("insurance_agent"))
        );
        assert!(
            env.warnings
                .iter()
                .any(|warning| warning.contains("plusoft"))
        );
        let _ = fs::remove_dir_all(root);
    }
}
