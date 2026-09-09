//! Generic import-boundary doctor.
//!
//! Invariant: configured architectural boundaries hold. A rule says "files
//! under `from_prefix` must not import anything under `deny_prefixes`"; any
//! import edge crossing that line is drift, regardless of language.
//!
//! ```toml
//! [[doctors.import_boundary.rules]]
//! name = "core-isolated"
//! from_prefix = "core/"
//! deny_prefixes = ["verticals/", "apps/"]
//! ```
//!
//! Import data comes from the code-graph query cache (the same artifact the
//! `graph resolved-imports-in` CLI path reads), regenerated on demand when
//! missing. Resolved candidate paths are checked first; when an import did
//! not resolve, relative specifiers (`./`, `../`) are normalized against the
//! importing file's directory and other specifiers are matched textually.
//! Dynamic imports the indexer cannot see are out of scope — the doctor
//! reports what the graph knows, nothing more.
//!
//! With zero configured rules the doctor is a no-op (zero warnings, no graph
//! load), which makes it safe to register under every profile.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::orphan_files::load_or_refresh_cache;
use super::utils::query_id;
use crate::code_graph::CachedGraphImport;
use crate::config::{ImportBoundaryRule, load_repo_config};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct ImportBoundaryDoctor;

impl Doctor for ImportBoundaryDoctor {
    fn name(&self) -> &'static str {
        "import-boundary"
    }

    fn description(&self) -> &'static str {
        "Enforces configured import boundaries ([[doctors.import_boundary.rules]]): files under a rule's from_prefix must not import paths under its deny_prefixes. No-ops with zero configured rules."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_import_boundary(index, root)
    }
}

pub fn doctor_import_boundary(index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let rules: Vec<ImportBoundaryRule> = load_repo_config(root)
        .and_then(|config| config.doctors)
        .and_then(|doctors| doctors.import_boundary)
        .and_then(|import_boundary| import_boundary.rules)
        .unwrap_or_default();

    if rules.is_empty() {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_import_boundary"),
            kind: "doctor".to_string(),
            summary: "import-boundary: no import-boundary rules configured — doctor is inactive (add [[doctors.import_boundary.rules]] to .leio-code/config.toml)".to_string(),
            confidence: 0.95,
            entities,
            evidence,
            warnings,
            meta: Some(json!({"reason": "no_rules_configured"})),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    // Validate rules first; a malformed rule only exists when someone wrote
    // config, so surfacing it as a warning is fail-loud, not noise.
    let mut active_rules: Vec<(String, &ImportBoundaryRule)> = Vec::new();
    for (idx, rule) in rules.iter().enumerate() {
        let label = if rule.name.trim().is_empty() {
            format!("rule-{}", idx + 1)
        } else {
            rule.name.clone()
        };
        if rule.from_prefix.is_empty() || rule.deny_prefixes.is_empty() {
            warnings.push(format!(
                "import-boundary rule `{label}` is malformed: both from_prefix and deny_prefixes are required"
            ));
            continue;
        }
        active_rules.push((label, rule));
    }

    let cache = match load_or_refresh_cache(index, root) {
        Ok(cache) => cache,
        Err(error) => {
            warnings.push(format!("failed to load code graph query cache: {error}"));
            return QueryEnvelope {
                schema_version: crate::model::SCHEMA_VERSION.to_string(),
                query_id: query_id("doctor_import_boundary"),
                kind: "doctor".to_string(),
                summary: "could not check import boundaries: graph cache unavailable".to_string(),
                confidence: 0.0,
                entities,
                evidence,
                warnings,
                meta: None,
                timing_ms: started.elapsed().as_millis(),
            };
        }
    };

    let mut files_checked = 0usize;
    let mut violation_count = 0usize;

    for file in cache.files.values() {
        let matching_rules: Vec<&(String, &ImportBoundaryRule)> = active_rules
            .iter()
            .filter(|(_, rule)| file.path.starts_with(rule.from_prefix.as_str()))
            .collect();
        if matching_rules.is_empty() {
            continue;
        }
        files_checked += 1;

        let Some(imports) = cache.file_import_details.get(&file.iri) else {
            continue;
        };

        for import in imports {
            for (label, rule) in &matching_rules {
                let Some((target, denied_prefix)) =
                    violation_target(&file.path, import, &rule.deny_prefixes)
                else {
                    continue;
                };
                violation_count += 1;
                let line = import.line.unwrap_or(0);
                warnings.push(format!(
                    "rule `{label}`: {}:{line} imports `{}` which lands in denied prefix `{denied_prefix}` (resolved target: {target})",
                    file.path, import.raw
                ));
                entities.push(json!({
                    "doctor": "import-boundary",
                    "rule": label,
                    "file": file.path,
                    "import": import.raw,
                    "target": target,
                    "denied_prefix": denied_prefix,
                    "line": import.line,
                }));
                evidence.push(EvidenceItem {
                    kind: "import_boundary_violation".to_string(),
                    path: file.path.clone(),
                    line: import.line,
                    detail: format!(
                        "rule `{label}`: import `{}` → `{target}` crosses into `{denied_prefix}`",
                        import.raw
                    ),
                });
            }
        }
    }

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_import_boundary"),
        kind: "doctor".to_string(),
        summary: format!(
            "import-boundary: {} rule(s) active, {} file(s) checked, {} violation(s)",
            active_rules.len(),
            files_checked,
            violation_count
        ),
        confidence: if warnings.is_empty() { 0.9 } else { 0.75 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "rules_configured": rules.len(),
            "rules_active": active_rules.len(),
            "files_checked": files_checked,
            "violation_count": violation_count,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// Returns `(target_path, denied_prefix)` when `import` crosses into one of
/// `deny_prefixes`, else `None`.
///
/// Resolved candidate paths win: when the graph resolved the import, only the
/// candidates are checked. For unresolved imports, relative specifiers are
/// normalized against the importing file's directory and other specifiers are
/// matched textually against the denied prefixes.
fn violation_target(
    importer_path: &str,
    import: &CachedGraphImport,
    deny_prefixes: &[String],
) -> Option<(String, String)> {
    for candidate in &import.candidate_paths {
        for prefix in deny_prefixes {
            if candidate.starts_with(prefix.as_str()) {
                return Some((candidate.clone(), prefix.clone()));
            }
        }
    }
    if !import.candidate_paths.is_empty() {
        // The graph resolved this import and no candidate is denied; trust
        // the resolution over raw-text heuristics.
        return None;
    }

    let base_dir = importer_path
        .rsplit_once('/')
        .map(|(dir, _)| dir)
        .unwrap_or("");
    let specifiers: Vec<&str> = if import.module_specifiers.is_empty() {
        vec![import.raw.as_str()]
    } else {
        import
            .module_specifiers
            .iter()
            .map(String::as_str)
            .collect()
    };

    for specifier in specifiers {
        let normalized = if specifier.starts_with("./") || specifier.starts_with("../") {
            normalize_relative(base_dir, specifier)
        } else {
            Some(specifier.trim_start_matches('/').to_string())
        };
        let Some(normalized) = normalized else {
            continue;
        };
        for prefix in deny_prefixes {
            if normalized.starts_with(prefix.as_str()) {
                return Some((normalized, prefix.clone()));
            }
        }
    }
    None
}

/// Normalizes a `./`/`../` specifier against a repo-relative base directory.
/// Returns `None` when the specifier escapes above the repo root.
fn normalize_relative(base_dir: &str, specifier: &str) -> Option<String> {
    let mut segments: Vec<&str> = if base_dir.is_empty() {
        Vec::new()
    } else {
        base_dir.split('/').collect()
    };
    for part in specifier.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            other => segments.push(other),
        }
    }
    Some(segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Why: relative-import normalization is the fallback that catches
    // violations when graph resolution misses; off-by-one `..` handling would
    // silently let boundary crossings through.
    #[test]
    fn normalize_relative_resolves_parent_segments() {
        assert_eq!(
            normalize_relative("core/svc", "../verticals/api").as_deref(),
            Some("core/verticals/api")
        );
        assert_eq!(
            normalize_relative("core", "../verticals/api").as_deref(),
            Some("verticals/api")
        );
        assert_eq!(
            normalize_relative("core", "./local/api").as_deref(),
            Some("core/local/api")
        );
        // Escaping above the repo root is unresolvable, not a match.
        assert_eq!(normalize_relative("", "../outside"), None);
    }
}
