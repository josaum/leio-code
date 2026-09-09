//! Secret-set parity doctor.
//!
//! Catches a class of drift surfaced 2026-05-28: deploy `secret-sets/*.env.example`
//! templates declare keys that the matching persisted fill files never set.
//! `.env.local` / `.override.local` files are intentionally gitignored
//! operator material; they cannot be fixed by a commit to `main`, so this
//! doctor ignores them when auditing source-contract parity.
//!
//! Scope (narrow, by design):
//! - For each `deploy/secret-sets/<set>.env.example`, collect the declared
//!   keys (non-comment `KEY=` lines).
//! - Collect declared keys from every sibling `<set>.env*` file that is not
//!   ignored by the repo's source `.gitignore` — these are persisted fill paths.
//! - A key is "covered" if it appears as a declaration in at least one
//!   sibling file. A key declared only in `.example` is "spec-only" and warned.
//!
//! Empty values are fine — declaration alone counts as coverage. Filling
//! the value is a separate concern (operator/runtime), not a parity concern.
//!
//! Optional / template-only keys (relaxed 2026-05-29):
//! The `.example` files are deliberate SUPERSET TEMPLATES — they document
//! optional integration keys (Pacto webhook tuning, Chatwoot, Plusoft routing
//! flags, WhatsApp direct-fallback flags, EXAMPLE_FLIGHT / S3 / INFER infra)
//! that a given deployment may legitimately leave unset. Those must NOT be
//! flagged. A spec-only key is treated as optional/template-only when EITHER of
//! two EXPLICIT opt-outs applies (no silent disabling of the check):
//!   (a) the `.example` file marks it with a preceding `# leio:optional` line
//!       immediately above the `KEY=` line, OR an inline trailing `# optional`
//!       comment on the declaration itself; OR
//!   (b) the key name is in [`TEMPLATE_ONLY_KEYS`] — the curated allowlist of
//!       known template-only keys (seeded 2026-05-29 from the keys reported
//!       spec-only at relaxation time across every secret-set).
//! A genuinely-new spec-only key (not marked, not allowlisted) still warns —
//! that is the drift this doctor exists to catch.
//!
//! Not in scope:
//! - Whether the .local value is non-empty (operators may resolve at runtime).
//! - Cross-set parity (`customer_ops_unified.env.example` vs other sets).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Instant;

use ignore::WalkBuilder;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde_json::json;

use super::Doctor;
use super::utils::query_id;
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct SecretSetParityDoctor;

impl Doctor for SecretSetParityDoctor {
    fn name(&self) -> &'static str {
        "secret-set-parity"
    }

    fn description(&self) -> &'static str {
        "Every required key declared in deploy/secret-sets/<set>.env.example must also be declared in at least one sibling <set>.env* file (.local, .override.local, etc.). Optional/template-only keys (allowlisted or marked) are exempt. Catches spec-only keys that silently boot empty."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_secret_set_parity(root)
    }
}

const SECRET_SETS_DIR: &str = "deploy/secret-sets";

/// Curated allowlist of template-only keys. These are documented in `.example`
/// supersets but are optional for any given deployment, so leaving them unset
/// is intentional, not drift. Seeded 2026-05-29 from the full set of keys that
/// were reported spec-only at relaxation time. A NEW unlisted spec-only key
/// still warns.
const TEMPLATE_ONLY_KEYS: &[&str] = &[
    "CHATWOOT_API_TOKEN",
    "CHATWOOT_WEBHOOK_SECRET",
    "CHATWOOT_WEBHOOK_TOKEN",
    "INFOBIP_API_KEY",
    "INFOBIP_SCENARIO_KEY",
    "JAIPAY_DATABASE_URL",
    "MILVUS_URL",
    "EXAMPLE_FLIGHT_SECRET",
    "EXAMPLE_INFER_FLIGHT_URL",
    "EXAMPLE_INFER_HTTP_URL",
    "EXAMPLE_VLLM_BASE_URL",
    "PACTO_API_TOKEN",
    "PACTO_CREDENTIALS_JSON",
    "PACTO_WEBHOOK_CHAVES",
    "PACTO_WEBHOOK_OBSERVED_PATH",
    "PENTEST_SECRET",
    "PLUSOFT_API_BASE_URL",
    "PLUSOFT_HANDOVER_BASE_URL",
    "PLUSOFT_JAI_CAMPAIGN_CODES_BY_ENVIRONMENT",
    "PLUSOFT_RETAIN_ON_INACTIVE_CAMPAIGN",
    "PLUSOFT_RETAIN_ON_LOOKUP_NON_200",
    "PLUSOFT_ROUTING_DEFAULT_TO_JAI",
    "PRATIQUE_CHATWOOT_API_TOKEN",
    "S3_ACCESS_KEY_ID",
    "S3_BUCKET",
    "S3_SECRET_ACCESS_KEY",
    "VIGOROS_LEIO_SOURCE_IDS",
    "VIGOROS_SEARCH_COLLECTION",
    "WHATSAPP_APP_SECRET",
];

/// Explicit per-line opt-out markers in `.example` files.
const MARKER_PRECEDING: &str = "# leio:optional";
const MARKER_INLINE: &str = "# optional";

/// Return the declaration key name (`KEY`) if `trimmed` is an env declaration
/// line of the form `KEY=...`, else `None`.
fn declaration_key(trimmed: &str) -> Option<String> {
    let eq = trimmed.find('=')?;
    let key = trimmed[..eq].trim().to_string();
    if !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        Some(key)
    } else {
        None
    }
}

/// Extract `KEY=...` declaration names from an env file body. Comments and
/// non-declaration lines are skipped.
fn declared_keys(contents: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for line in contents.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(key) = declaration_key(trimmed) {
            keys.insert(key);
        }
    }
    keys
}

/// Keys declared in an `.example` file that are marked optional via an explicit
/// per-line marker: a preceding `# leio:optional` line, or an inline trailing
/// `# optional` comment on the declaration itself.
fn marked_optional_keys(contents: &str) -> BTreeSet<String> {
    let mut optional = BTreeSet::new();
    let mut preceding_marker = false;
    for line in contents.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            preceding_marker = false;
            continue;
        }
        if trimmed.starts_with('#') {
            preceding_marker = trimmed.trim_end() == MARKER_PRECEDING;
            continue;
        }
        if let Some(key) = declaration_key(trimmed) {
            let inline_optional = trimmed
                .find('#')
                .is_some_and(|hash| trimmed[hash..].trim_end() == MARKER_INLINE);
            if preceding_marker || inline_optional {
                optional.insert(key);
            }
        }
        preceding_marker = false;
    }
    optional
}

/// A spec-only key is exempt from warning when it is allowlisted as
/// template-only or marked optional in its `.example` file.
fn is_optional_key(key: &str, marked_optional: &BTreeSet<String>) -> bool {
    TEMPLATE_ONLY_KEYS.contains(&key) || marked_optional.contains(key)
}

/// Resolve "set name" from a filename like `customer_ops_unified.env.example`,
/// `customer_ops_unified.env.local`, `customer_ops_unified.env.override.local`.
/// Returns the part before `.env`.
fn set_name_from_filename(name: &str) -> Option<&str> {
    name.find(".env").map(|i| &name[..i])
}

pub fn doctor_secret_set_parity(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let dir = root.join(SECRET_SETS_DIR);
    let gitignore = build_gitignore(root);
    if !dir.exists() {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_secret_set_parity"),
            kind: "doctor".to_string(),
            summary: "secret-set-parity: deploy/secret-sets/ not present — doctor skipped"
                .to_string(),
            confidence: 0.95,
            entities,
            evidence,
            warnings,
            meta: Some(json!({"reason": "no_secret_sets_dir"})),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    // Map: set_name -> (example_keys, marked_optional_keys, example_rel)
    let mut examples: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>, String)> =
        BTreeMap::new();
    let mut siblings: BTreeMap<String, Vec<(String, BTreeSet<String>)>> = BTreeMap::new();

    let walker = WalkBuilder::new(&dir)
        .hidden(false)
        .git_ignore(false) // .local files are gitignored but we still scan them
        .max_depth(Some(1))
        .build();

    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if !name.contains(".env") {
            continue;
        }
        let set = match set_name_from_filename(name) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let rel = match path.strip_prefix(root) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        let contents = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let keys = declared_keys(&contents);
        if name.ends_with(".env.example") {
            let marked = marked_optional_keys(&contents);
            examples.insert(set, (keys, marked, rel));
        } else if !is_ignored_by_source_gitignore(&gitignore, path) {
            siblings.entry(set).or_default().push((rel, keys));
        }
    }

    let mut sets_checked = 0usize;
    let mut sets_with_drift = 0usize;

    for (set, (example_keys, marked_optional, example_rel)) in &examples {
        sets_checked += 1;
        let Some(source_fill_files) = siblings.get(set) else {
            entities.push(json!({
                "doctor": "secret-set-parity",
                "set": set,
                "example_file": example_rel,
                "state": "template_only_no_persisted_fill",
            }));
            continue;
        };
        let covered: BTreeSet<String> = source_fill_files
            .iter()
            .flat_map(|(_, k)| k.iter().cloned())
            .collect();
        // A key is missing only if it is uncovered AND not exempt (allowlisted
        // template-only key or explicitly marked optional in the .example).
        let spec_only: Vec<&String> = example_keys
            .iter()
            .filter(|k| !covered.contains(*k) && !is_optional_key(k, marked_optional))
            .collect();
        if !spec_only.is_empty() {
            sets_with_drift += 1;
            warnings.push(format!(
                "secret-set `{set}` has {} key(s) declared in {example_rel} but not in any sibling .local/.override.local: {}",
                spec_only.len(),
                spec_only.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
            ));
            for key in &spec_only {
                entities.push(json!({
                    "doctor": "secret-set-parity",
                    "set": set,
                    "key": key,
                    "example_file": example_rel,
                    "state": "spec_only",
                }));
                evidence.push(EvidenceItem {
                    kind: "spec_only_secret_key".to_string(),
                    path: example_rel.clone(),
                    line: None,
                    detail: format!("`{key}` declared in .example but not in any sibling .local"),
                });
            }
        }
    }

    let summary = if warnings.is_empty() {
        format!(
            "secret-set-parity: {sets_checked} secret-set(s) checked, all required .example keys covered by sibling fill files (template-only/optional keys exempt)"
        )
    } else {
        format!(
            "secret-set-parity: {sets_with_drift}/{sets_checked} secret-set(s) have undocumented spec-only keys"
        )
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_secret_set_parity"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.95 } else { 0.88 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "sets_checked": sets_checked,
            "sets_with_drift": sets_with_drift,
            "template_only_allowlist_size": TEMPLATE_ONLY_KEYS.len(),
            "ignored_operator_local_files_excluded": true,
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
            "leio-code-secret-parity-{label}-{}-{nanos}",
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
    fn flags_spec_only_keys() {
        let dir = unique_tempdir("spec-only");
        write_file(
            &dir.join("deploy/secret-sets/foo.env.example"),
            "ALPHA=\nBETA=\nGAMMA=\n",
        );
        write_file(&dir.join("deploy/secret-sets/foo.env.local"), "ALPHA=x\n");
        let envelope = doctor_secret_set_parity(&dir);
        assert_eq!(envelope.warnings.len(), 1);
        let w = &envelope.warnings[0];
        assert!(w.contains("BETA"), "expected BETA in warning: {w}");
        assert!(w.contains("GAMMA"), "expected GAMMA in warning: {w}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn override_local_counts_as_coverage() {
        let dir = unique_tempdir("override");
        write_file(
            &dir.join("deploy/secret-sets/foo.env.example"),
            "ALPHA=\nBETA=\n",
        );
        write_file(&dir.join("deploy/secret-sets/foo.env.local"), "ALPHA=x\n");
        write_file(
            &dir.join("deploy/secret-sets/foo.env.override.local"),
            "BETA=y\n",
        );
        let envelope = doctor_secret_set_parity(&dir);
        assert!(
            envelope.warnings.is_empty(),
            "expected no warnings, got {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn comments_and_blank_lines_are_not_declarations() {
        let dir = unique_tempdir("comments");
        write_file(
            &dir.join("deploy/secret-sets/foo.env.example"),
            "# ALPHA=this-is-a-comment\nBETA=\n",
        );
        write_file(&dir.join("deploy/secret-sets/foo.env.local"), "BETA=y\n");
        let envelope = doctor_secret_set_parity(&dir);
        assert!(
            envelope.warnings.is_empty(),
            "expected no warnings, got {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_values_count_as_coverage() {
        let dir = unique_tempdir("empty-values");
        write_file(&dir.join("deploy/secret-sets/foo.env.example"), "ALPHA=\n");
        write_file(&dir.join("deploy/secret-sets/foo.env.local"), "ALPHA=\n");
        let envelope = doctor_secret_set_parity(&dir);
        assert!(envelope.warnings.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_when_no_dir() {
        let dir = unique_tempdir("no-dir");
        let envelope = doctor_secret_set_parity(&dir);
        assert!(envelope.warnings.is_empty());
        assert!(envelope.summary.contains("skipped"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn allowlisted_template_only_key_is_not_flagged() {
        let dir = unique_tempdir("allowlist");
        // PACTO_WEBHOOK_CHAVES is in TEMPLATE_ONLY_KEYS; UNDOCUMENTED_KEY is not.
        write_file(
            &dir.join("deploy/secret-sets/foo.env.example"),
            "ALPHA=\nPACTO_WEBHOOK_CHAVES=\nUNDOCUMENTED_KEY=\n",
        );
        write_file(&dir.join("deploy/secret-sets/foo.env.local"), "ALPHA=x\n");
        let envelope = doctor_secret_set_parity(&dir);
        // PACTO_WEBHOOK_CHAVES must be exempt; UNDOCUMENTED_KEY must still warn.
        assert_eq!(envelope.warnings.len(), 1, "{:?}", envelope.warnings);
        let w = &envelope.warnings[0];
        assert!(
            w.contains("UNDOCUMENTED_KEY"),
            "expected UNDOCUMENTED_KEY in warning: {w}"
        );
        assert!(
            !w.contains("PACTO_WEBHOOK_CHAVES"),
            "allowlisted key must not be flagged: {w}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn preceding_marker_exempts_key_but_unmarked_still_warns() {
        let dir = unique_tempdir("marker-preceding");
        write_file(
            &dir.join("deploy/secret-sets/foo.env.example"),
            "# leio:optional\nFEATURE_FLAG_X=\nFEATURE_FLAG_Y=\n",
        );
        // No sibling covers either key.
        write_file(&dir.join("deploy/secret-sets/foo.env.local"), "\n");
        let envelope = doctor_secret_set_parity(&dir);
        assert_eq!(envelope.warnings.len(), 1, "{:?}", envelope.warnings);
        let w = &envelope.warnings[0];
        assert!(
            w.contains("FEATURE_FLAG_Y"),
            "unmarked key must still warn: {w}"
        );
        assert!(
            !w.contains("FEATURE_FLAG_X"),
            "marked-optional key must not warn: {w}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn inline_optional_comment_exempts_key() {
        let dir = unique_tempdir("marker-inline");
        write_file(
            &dir.join("deploy/secret-sets/foo.env.example"),
            "FEATURE_FLAG_X=  # optional\nFEATURE_FLAG_Y=\n",
        );
        write_file(&dir.join("deploy/secret-sets/foo.env.local"), "\n");
        let envelope = doctor_secret_set_parity(&dir);
        assert_eq!(envelope.warnings.len(), 1, "{:?}", envelope.warnings);
        let w = &envelope.warnings[0];
        assert!(w.contains("FEATURE_FLAG_Y"), "unmarked key must warn: {w}");
        assert!(
            !w.contains("FEATURE_FLAG_X"),
            "inline-optional key must not warn: {w}"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
