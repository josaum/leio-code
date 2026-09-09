//! Generic env-contract doctor.
//!
//! Invariant: an env var that code READS but that is declared NOWHERE in the
//! repo is a deploy-time landmine — the code compiles, the tests pass, and
//! the first environment without that var fails at runtime.
//!
//! Declaration sources (all read from the index, no live probes):
//! - root dotenv files (`.env`, `.env.example`, …) → `index.env_files`
//! - deploy profiles → `index.profiles`
//! - secret sets → `index.secret_sets`
//! - Kubernetes ConfigMap `data:` keys → `index.k8s_configmaps`
//! - inline `Declared`-access occurrences (TOML/YAML/dotenv lines)
//!
//! Noise gating, in order:
//! 1. If the repo has ZERO declaration sources the doctor is inactive: it
//!    cannot distinguish "undeclared" from "this repo declares env elsewhere",
//!    so it emits zero warnings and says why in the summary.
//! 2. Well-known platform/runtime vars ([`PLATFORM_VARS`]) never warn.
//! 3. Vars the code itself writes (`set_var`-style) are runtime-provided by
//!    the repo's own code, not deploy configuration, so they never warn.
//! 4. An optional config allowlist (`[doctors.env_contract] allow = [...]`)
//!    suppresses intentionally undeclared vars; entries are exact names or
//!    `*`-suffixed prefix globs.
//!
//! Registered for `PROFILE_GENERIC` only. The Example workspace declares many
//! vars in deploy-time environments outside the repo, so running this doctor
//! there would report environment facts, not drift.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::query_id;
use crate::config::load_repo_config;
use crate::model::{AccessKind, EvidenceItem, QueryEnvelope, RepoIndex};

pub struct EnvContractDoctor;

impl Doctor for EnvContractDoctor {
    fn name(&self) -> &'static str {
        "env-contract"
    }

    fn description(&self) -> &'static str {
        "Flags env vars that code reads but that are declared nowhere in the repo (.env* files, profiles, secret sets, k8s ConfigMaps, inline declarations). Inactive when the repo has zero declaration sources."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_env_contract(index, root)
    }
}

/// Vars provided by the OS, the shell, CI runners, or language runtimes
/// rather than by repo configuration. Reading them without an in-repo
/// declaration is normal, so they are never flagged. The list is intentionally
/// small; repo-specific exemptions belong in `[doctors.env_contract] allow`.
const PLATFORM_VARS: &[&str] = &[
    // POSIX session basics.
    "HOME",
    "PATH",
    "PWD",
    "USER",
    "USERNAME",
    "LOGNAME",
    "SHELL",
    "TERM",
    "HOSTNAME",
    "LANG",
    "LC_ALL",
    "TZ",
    "TMPDIR",
    "TMP",
    "TEMP",
    // CI / runtime conventions.
    "CI",
    "GITHUB_ACTIONS",
    "NODE_ENV",
    "PORT",
    "HOST",
    "DEBUG",
    // Rust runtime knobs.
    "RUST_LOG",
    "RUST_BACKTRACE",
];

/// True when `name` matches an allowlist entry — exact match or, for entries
/// ending in `*`, a prefix match on the part before the `*`.
fn allow_matches(name: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| {
        if let Some(prefix) = pattern.strip_suffix('*') {
            name.starts_with(prefix)
        } else {
            name == pattern
        }
    })
}

pub fn doctor_env_contract(index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    // --- Collect declared names across every declaration source. ---
    let mut declared: BTreeSet<&str> = BTreeSet::new();
    for env_file in &index.env_files {
        for var in &env_file.vars {
            declared.insert(var.name.as_str());
        }
    }
    for profile in &index.profiles {
        for var in &profile.vars {
            declared.insert(var.name.as_str());
        }
    }
    for secret_set in &index.secret_sets {
        for var in &secret_set.vars {
            declared.insert(var.name.as_str());
        }
    }
    for configmap in &index.k8s_configmaps {
        for key in configmap.entries.keys() {
            declared.insert(key.as_str());
        }
    }

    // Inline `Declared` occurrences (TOML/YAML/dotenv assignment lines) and
    // code writes. A var the code itself sets before reading is provided at
    // runtime by the repo's own code, so it is not a deploy-time landmine.
    let mut declared_occurrence_files: BTreeSet<&str> = BTreeSet::new();
    let mut written: BTreeSet<&str> = BTreeSet::new();
    for file in &index.files {
        for occurrence in &file.env_vars {
            match occurrence.access {
                AccessKind::Declared => {
                    declared.insert(occurrence.name.as_str());
                    declared_occurrence_files.insert(file.path.as_str());
                }
                AccessKind::Write => {
                    written.insert(occurrence.name.as_str());
                }
                AccessKind::Read | AccessKind::Unknown => {}
            }
        }
    }

    let env_file_count = index.env_files.len();
    let profile_count = index.profiles.len();
    let secret_set_count = index.secret_sets.len();
    let configmap_count = index.k8s_configmaps.len();
    let inline_declaration_file_count = declared_occurrence_files.len();
    let source_count = env_file_count
        + profile_count
        + secret_set_count
        + configmap_count
        + inline_declaration_file_count;

    let sources_meta = json!({
        "env_files": env_file_count,
        "profiles": profile_count,
        "secret_sets": secret_set_count,
        "k8s_configmaps": configmap_count,
        "files_with_inline_declarations": inline_declaration_file_count,
    });

    // Gate: with zero declaration sources the doctor cannot tell "undeclared"
    // apart from "declared outside the repo" — stay silent instead of noisy.
    if source_count == 0 {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_env_contract"),
            kind: "doctor".to_string(),
            summary: "env-contract: no env declaration sources found (.env* files, profiles, secret sets, k8s configmaps, inline declarations); env-contract is inactive".to_string(),
            confidence: 0.95,
            entities,
            evidence,
            warnings,
            meta: Some(json!({
                "reason": "no_declaration_sources",
                "declaration_sources": sources_meta,
            })),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    let allow_patterns: Vec<String> = load_repo_config(root)
        .and_then(|config| config.doctors)
        .and_then(|doctors| doctors.env_contract)
        .and_then(|env_contract| env_contract.allow)
        .unwrap_or_default();

    // --- Group undeclared reads by var name. ---
    let mut undeclared_reads: BTreeMap<&str, Vec<(&str, usize)>> = BTreeMap::new();
    for file in &index.files {
        for occurrence in &file.env_vars {
            if occurrence.access != AccessKind::Read {
                continue;
            }
            let name = occurrence.name.as_str();
            if declared.contains(name)
                || written.contains(name)
                || PLATFORM_VARS.contains(&name)
                || allow_matches(name, &allow_patterns)
            {
                continue;
            }
            undeclared_reads
                .entry(name)
                .or_default()
                .push((occurrence.path.as_str(), occurrence.line));
        }
    }

    for (name, sites) in &undeclared_reads {
        // Entries are only created on push, so `sites` is never empty.
        let Some(&(first_path, first_line)) = sites.first() else {
            continue;
        };
        let extra_sites = sites.len().saturating_sub(1);
        let extra_note = if extra_sites > 0 {
            format!(" (+{extra_sites} more read sites)")
        } else {
            String::new()
        };
        warnings.push(format!(
            "env var `{name}` is read at {first_path}:{first_line}{extra_note} but is declared in none of the scanned sources (env files: {env_file_count}, profiles: {profile_count}, secret sets: {secret_set_count}, k8s configmaps: {configmap_count}, files with inline declarations: {inline_declaration_file_count})"
        ));
        entities.push(json!({
            "doctor": "env-contract",
            "var": name,
            "read_site_count": sites.len(),
            "first_read": format!("{first_path}:{first_line}"),
        }));
        for (path, line) in sites {
            evidence.push(EvidenceItem {
                kind: "env_undeclared_read".to_string(),
                path: (*path).to_string(),
                line: Some(*line),
                detail: format!("`{name}` read here but declared nowhere in the repo"),
            });
        }
    }

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_env_contract"),
        kind: "doctor".to_string(),
        summary: format!(
            "env-contract: scanned {} declaration source(s), found {} undeclared env var read(s)",
            source_count,
            undeclared_reads.len()
        ),
        confidence: if warnings.is_empty() { 0.9 } else { 0.7 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "declaration_sources": sources_meta,
            "allow_patterns": allow_patterns,
            "undeclared_var_count": undeclared_reads.len(),
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Why: the allowlist contract is "exact name or `*`-suffix glob"; if the
    // matcher drifts, configured exemptions silently stop suppressing.
    #[test]
    fn allow_matches_exact_and_prefix_glob() {
        let patterns = vec!["FOO_*".to_string(), "BAR".to_string()];
        assert!(allow_matches("FOO_TOKEN", &patterns));
        assert!(allow_matches("FOO_", &patterns));
        assert!(allow_matches("BAR", &patterns));
        assert!(!allow_matches("BARISTA", &patterns));
        assert!(!allow_matches("BAZ", &patterns));
        assert!(!allow_matches("FOO", &patterns));
    }
}
