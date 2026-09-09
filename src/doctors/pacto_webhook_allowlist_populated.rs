//! Pacto webhook allowlist populated doctor.
//!
//! Catches the **exact production outage surfaced 2026-05-27**: PRs #214-#216
//! shipped the Pacto webhook hardening code + env-var plumbing (passthrough
//! in `docker-compose.yml`, blank templates in `deploy/secret-sets/*.env.example`)
//! but the actual chave list / empresa-name map was never populated in any
//! `.env.local` override file.
//!
//! Result: on the VM `PACTO_WEBHOOK_CHAVES` resolved to an empty string →
//! `resolve_alias_for_chave()` returned `None` → every Pacto webhook
//! rejected with `401 invalid chave` → Pacto retry-stormed our endpoint
//! and we lost per-unit attribution for hours.
//!
//! `leio-code explain env-var PACTO_WEBHOOK_CHAVES` already shows this
//! cleanly:
//!
//! ```text
//! "value_bindings":[{"state":"empty", "source":{"kind":"secret_set",
//! "name":"collections_platform.env.example"}}, ...],
//! "effective": {"state":"empty", ...}
//! ```
//!
//! This doctor codifies the rule for material deploy inputs:
//!   `PACTO_WEBHOOK_CHAVES` must have a non-empty value when an authoritative
//!   non-example deploy env file declares it. The temporary
//!   `PACTO_WEBHOOK_EMPRESA_NAME_FALLBACK` compatibility map is accepted only
//!   when the same deploy profile also enables source-locked learning, declares
//!   an IP allowlist, and provides a durable observation path. A fallback-only
//!   configuration still fails this doctor. Empty `.env.example` templates are
//!   documentation, not deploy material, and ignored `.env.local` bundles
//!   cannot be fixed by committing to `main`; deploy-time validation owns those.
//!
//! Scope (intentionally narrow):
//! - Two vars by name; no autodetection of "which vars are critical".
//! - Two file globs: `deploy/secret-sets/*.env*` and `deploy/secrets.env*`.
//! - One workspace-level rule (no per-target scoping yet).
//!
//! Not in scope:
//! - JSON syntax validation of the populated value (deferred — too easy to
//!   false-positive on legitimate one-off shapes).
//! - Other webhook surfaces (Liz, Plusoft, etc.) — different vars, different
//!   failure modes, can have their own doctor when they bite.

use std::path::Path;
use std::time::Instant;

use ignore::WalkBuilder;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde_json::json;

use super::Doctor;
use super::utils::query_id;
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct PactoWebhookAllowlistPopulatedDoctor;

impl Doctor for PactoWebhookAllowlistPopulatedDoctor {
    fn name(&self) -> &'static str {
        "pacto-webhook-allowlist-populated"
    }

    fn description(&self) -> &'static str {
        "PACTO_WEBHOOK_CHAVES must have a non-empty value in some deploy/secret-sets/*.env* file. Empty → 100% Pacto webhook rejection in prod (the bug surfaced 2026-05-27 after PR #224 deploy)."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_pacto_webhook_allowlist_populated(root)
    }
}

/// The authenticated env var the Pacto webhook validator reads. It must be
/// non-empty in some deploy file for the validator to accept any traffic.
const PACTO_AUTH_VARS: &[&str] = &["PACTO_WEBHOOK_CHAVES"];
const EMPRESA_MAP_VAR: &str = "PACTO_WEBHOOK_EMPRESA_NAME_FALLBACK";
const LEARNING_MODE_VAR: &str = "PACTO_WEBHOOK_CHAVE_LEARN_MODE";
const LEARNING_IP_VAR: &str = "PACTO_WEBHOOK_CHAVE_LEARN_IP_ALLOWLIST";
const OBSERVED_PATH_VAR: &str = "PACTO_WEBHOOK_OBSERVED_PATH";

/// Files we consider authoritative for "is this var populated for deploy".
/// We scan everything that looks like a deploy env file under `deploy/`.
const DEPLOY_SCAN_PREFIX: &str = "deploy/";

pub fn doctor_pacto_webhook_allowlist_populated(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    // For each var, find every declaration line across deploy/ env files.
    // A "populated" hit = `VAR=<non-empty>`. An "empty" hit = `VAR=` with
    // nothing after the `=` (modulo whitespace).
    //
    // We record per-file state so the report can pinpoint which deploy
    // target is missing the value.
    let mut populated_anywhere: std::collections::BTreeMap<String, Vec<(String, usize)>> =
        std::collections::BTreeMap::new();
    let mut declared_empty: std::collections::BTreeMap<String, Vec<(String, usize)>> =
        std::collections::BTreeMap::new();
    let mut empresa_map_declarations: Vec<(String, usize)> = Vec::new();
    let mut learning_by_file: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, String>,
    > = std::collections::BTreeMap::new();

    let deploy_root = root.join(DEPLOY_SCAN_PREFIX);
    let gitignore = build_gitignore(root);
    if !deploy_root.exists() {
        // Workspaces without deploy/ are skipped — doctor doesn't apply.
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_pacto_webhook_allowlist_populated"),
            kind: "doctor".to_string(),
            summary: "pacto-webhook-allowlist-populated: deploy/ not present — doctor skipped"
                .to_string(),
            confidence: 0.95,
            entities,
            evidence,
            warnings,
            meta: Some(json!({"reason":"no_deploy_dir"})),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    let walker = WalkBuilder::new(&deploy_root)
        .hidden(false)
        .git_ignore(false) // also scan .local override files (gitignored)
        .build();

    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        // Match `.env`, `.env.example`, `.env.local`, `secrets.env`,
        // `customer_ops_unified.env.local`, etc.
        if !name.contains(".env") {
            continue;
        }
        if name.ends_with(".env.example") || is_ignored_by_source_gitignore(&gitignore, path) {
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        let contents = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        for (idx, line) in contents.lines().enumerate() {
            let trimmed = line.trim_start();
            // Skip comments and continuation lines.
            if trimmed.starts_with('#') || !trimmed.contains('=') {
                continue;
            }
            if let Some((key, value)) = trimmed.split_once('=') {
                let key = key.trim();
                let value = value.trim();
                if key == EMPRESA_MAP_VAR {
                    empresa_map_declarations.push((rel.clone(), idx + 1));
                }
                if [
                    EMPRESA_MAP_VAR,
                    LEARNING_MODE_VAR,
                    LEARNING_IP_VAR,
                    OBSERVED_PATH_VAR,
                ]
                .contains(&key)
                    && !value.is_empty()
                {
                    learning_by_file
                        .entry(rel.clone())
                        .or_default()
                        .insert(key.to_string(), value.to_string());
                }
            }
            for var in PACTO_AUTH_VARS {
                let prefix = format!("{var}=");
                if let Some(rest) = trimmed.strip_prefix(&prefix) {
                    let value = rest.trim();
                    if value.is_empty() {
                        declared_empty
                            .entry((*var).to_string())
                            .or_default()
                            .push((rel.clone(), idx + 1));
                    } else {
                        populated_anywhere
                            .entry((*var).to_string())
                            .or_default()
                            .push((rel.clone(), idx + 1));
                    }
                    break;
                }
            }
        }
    }

    // Rule: the authenticated key map must have a non-empty value
    // somewhere in a material deploy file. No material declaration means
    // source has no commit-persistable allowlist to audit here.
    let declares_any_material_auth_var = PACTO_AUTH_VARS
        .iter()
        .any(|var| populated_anywhere.contains_key(*var) || declared_empty.contains_key(*var));
    let has_any_population = PACTO_AUTH_VARS
        .iter()
        .any(|var| populated_anywhere.contains_key(*var));
    let has_controlled_learning_profile = learning_by_file.values().any(|values| {
        values
            .get(LEARNING_MODE_VAR)
            .is_some_and(|value| value == "accept_and_record")
            && [EMPRESA_MAP_VAR, LEARNING_IP_VAR, OBSERVED_PATH_VAR]
                .iter()
                .all(|key| values.get(*key).is_some_and(|value| !value.is_empty()))
    });

    if (declares_any_material_auth_var || !empresa_map_declarations.is_empty())
        && !has_any_population
        && !has_controlled_learning_profile
    {
        let empty_summary: Vec<String> = PACTO_AUTH_VARS
            .iter()
            .map(|var| {
                let count = declared_empty.get(*var).map(|v| v.len()).unwrap_or(0);
                format!("{var}: empty in {count} file(s)")
            })
            .collect();
        warnings.push(format!(
            "PACTO_WEBHOOK_CHAVES is empty across the deploy chain ({}). Next deploy will reject 100% of Pacto webhooks with `401 invalid chave`.",
            empty_summary.join(", ")
        ));
        for var in PACTO_AUTH_VARS {
            if let Some(empties) = declared_empty.get(*var) {
                for (file, line) in empties {
                    entities.push(json!({
                        "doctor": "pacto-webhook-allowlist-populated",
                        "var": var,
                        "file": file,
                        "line": line,
                        "state": "empty",
                    }));
                    evidence.push(EvidenceItem {
                        kind: "empty_pacto_var".to_string(),
                        path: file.clone(),
                        line: Some(*line),
                        detail: format!("`{var}=` declared with empty value"),
                    });
                }
            }
        }
    }

    let summary = if warnings.is_empty() {
        let populations: Vec<String> = populated_anywhere
            .iter()
            .map(|(var, hits)| format!("{var}: populated in {} file(s)", hits.len()))
            .collect();
        if populations.is_empty() && has_controlled_learning_profile {
            "pacto-webhook-allowlist-populated: source-locked accept-and-record compatibility profile configured; deploy-time secret validation still owns the promoted key map".to_string()
        } else if populations.is_empty() {
            "pacto-webhook-allowlist-populated: no persisted material Pacto allowlist declarations; deploy-time secret validation owns ignored local bundles".to_string()
        } else {
            format!(
                "pacto-webhook-allowlist-populated: at least one Pacto auth var is non-empty ({})",
                populations.join(", ")
            )
        }
    } else {
        format!(
            "pacto-webhook-allowlist-populated: {} warning(s) — Pacto webhook traffic will be rejected",
            warnings.len()
        )
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_pacto_webhook_allowlist_populated"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.95 } else { 0.88 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "material_declarations_present": declares_any_material_auth_var,
            "controlled_learning_profile": has_controlled_learning_profile,
            "populated": populated_anywhere.keys().collect::<Vec<_>>(),
            "empty_files": declared_empty.iter().map(|(var, hits)| {
                json!({"var": var, "files": hits.iter().map(|(f,_)| f).collect::<Vec<_>>()})
            }).collect::<Vec<_>>(),
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn build_gitignore(root: &Path) -> Option<Gitignore> {
    let mut builder = GitignoreBuilder::new(root);
    let gitignore = root.join(".gitignore");
    if gitignore.is_file() {
        let _ = builder.add(&gitignore);
    }
    builder.build().ok()
}

fn is_ignored_by_source_gitignore(gitignore: &Option<Gitignore>, path: &Path) -> bool {
    gitignore
        .as_ref()
        .is_some_and(|matcher| matcher.matched(path, path.is_dir()).is_ignore())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_tempdir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-pacto-pop-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(path, contents).expect("write fixture");
    }

    #[test]
    fn flags_when_authenticated_key_map_empty_across_all_files() {
        let dir = unique_tempdir("both-empty");
        write_file(
            &dir.join("deploy/secret-sets/customer_ops_unified.env.example"),
            "PACTO_WEBHOOK_CHAVES=\n",
        );
        write_file(
            &dir.join("deploy/secret-sets/customer_ops_unified.env"),
            "PACTO_WEBHOOK_CHAVES=\n",
        );
        let envelope = doctor_pacto_webhook_allowlist_populated(&dir);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("PACTO_WEBHOOK_CHAVES is empty")),
            "expected empty-key-map warning, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flags_when_only_unguarded_name_fallback_is_populated() {
        let dir = unique_tempdir("populated");
        write_file(
            &dir.join("deploy/secret-sets/customer_ops_unified.env.example"),
            "PACTO_WEBHOOK_CHAVES=\n",
        );
        write_file(
            &dir.join("deploy/secret-sets/customer_ops_unified.env.local"),
            "PACTO_WEBHOOK_EMPRESA_NAME_FALLBACK={\"a\":\"unit\"}\n",
        );
        let envelope = doctor_pacto_webhook_allowlist_populated(&dir);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("PACTO_WEBHOOK_CHAVES is empty")),
            "expected warning when only retired fallback is populated, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn accepts_source_locked_learning_profile_without_committed_secret_map() {
        let dir = unique_tempdir("controlled-learning");
        write_file(
            &dir.join("deploy/profiles/gyms_customer_service.env"),
            "PACTO_WEBHOOK_CHAVE_LEARN_MODE=accept_and_record\n\
             PACTO_WEBHOOK_CHAVE_LEARN_IP_ALLOWLIST=137.131.194.177/32\n\
             PACTO_WEBHOOK_OBSERVED_PATH=/data/pacto_observed_chaves.json\n\
             PACTO_WEBHOOK_EMPRESA_NAME_FALLBACK='{\"EXCLUSIVE UNID. PREMIUM - CE\":\"premium\"}'\n",
        );
        let envelope = doctor_pacto_webhook_allowlist_populated(&dir);
        assert!(
            envelope.warnings.is_empty(),
            "expected controlled learning profile to pass, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn does_not_flag_when_authenticated_key_map_is_populated() {
        let dir = unique_tempdir("authenticated");
        write_file(
            &dir.join("deploy/secret-sets/customer_ops_unified.env"),
            "PACTO_WEBHOOK_CHAVES={\"unit\":\"secret\"}\n",
        );
        let envelope = doctor_pacto_webhook_allowlist_populated(&dir);
        assert!(
            envelope.warnings.is_empty(),
            "expected no warning for authenticated key map, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignores_comments_and_other_vars() {
        let dir = unique_tempdir("comments");
        write_file(
            &dir.join("deploy/secret-sets/customer_ops_unified.env.example"),
            "PACTO_WEBHOOK_CHAVES=\n",
        );
        write_file(
            &dir.join("deploy/secret-sets/customer_ops_unified.env"),
            "# PACTO_WEBHOOK_CHAVES=foo (this is a comment, not a declaration)\n\
             OTHER_VAR=bar\n\
             PACTO_WEBHOOK_CHAVES=\n",
        );
        let envelope = doctor_pacto_webhook_allowlist_populated(&dir);
        // Should flag: comment isn't a real declaration; both real lines are empty.
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("PACTO_WEBHOOK_CHAVES is empty")),
            "expected warning, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn example_only_empty_template_is_not_material_drift() {
        let dir = unique_tempdir("example-only");
        write_file(
            &dir.join("deploy/secret-sets/customer_ops_unified.env.example"),
            "PACTO_WEBHOOK_CHAVES=\n",
        );
        let envelope = doctor_pacto_webhook_allowlist_populated(&dir);
        assert!(
            envelope.warnings.is_empty(),
            "expected no warning for template-only declarations, got: {:?}",
            envelope.warnings
        );
        assert!(
            envelope
                .summary
                .contains("no persisted material Pacto allowlist declarations"),
            "expected template-only summary, got: {}",
            envelope.summary
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_when_no_deploy_dir() {
        let dir = unique_tempdir("no-deploy");
        let envelope = doctor_pacto_webhook_allowlist_populated(&dir);
        assert!(envelope.warnings.is_empty());
        assert!(envelope.summary.contains("skipped"));
        let _ = fs::remove_dir_all(&dir);
    }
}
