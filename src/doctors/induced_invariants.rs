// Rust guideline compliant 2026-02-21
//! Induced-invariants doctor: enforces a curated baseline of structural
//! implications and warns on any current violation.
//!
//! The doctor builds a formal context over the same env/redis incidences that
//! `code_graph` writes into `graph.nq` — file objects carrying access-typed
//! `readsEnv:X` / `writesRedis:Y` attributes, restricted to the languages the
//! code graph covers (Rust, Python, JavaScript, TypeScript, TSX). It mines
//! single-premise exception-free implications via the in-repo pair algebra in
//! [`crate::fca`] and promotes
//! the ones that clear a conservative support + stability gate — but those
//! promoted rules are surfaced as *candidates* (info only), never as a gate.
//!
//! Enforcement keys on a **human-curated baseline** committed at
//! `leio-code/baselines/induced-invariants.json`. FCA proposes; humans promote.
//! The doctor validates the current code's extents against each baseline
//! invariant `a => b`: every file that carries `a` must also carry `b`. Any
//! file in `extent(a) \ extent(b)` is a violation and is reported with a
//! populated [`EvidenceItem`] naming the offending file.
//!
//! Sourcing the enforced set from the committed baseline — rather than from
//! mining the current context — breaks the tautology that made drift
//! undetectable: a rule a human pinned can now fail when the live code drifts
//! away from it, instead of being silently re-derived every run.
//!
//! When the baseline file is absent the doctor skips cleanly (info, not a
//! warning). When `LEIO_INDUCED_INVARIANTS_REFRESH=1` is set, the doctor
//! re-mines the current candidates and overwrites the baseline file (a
//! bootstrap/curation action) without enforcing on that run; the human then
//! reviews the git diff and prunes to the accepted subset before committing.
//!
//! Scope: invariants are over files and their env/redis attributes only —
//! never individual symbols. Every reported rule states this scope.

use std::path::Path;
use std::time::Instant;

use serde::Deserialize;
use serde_json::json;

use super::Doctor;
use super::utils::query_id;
use crate::fca::{self, Implication};
use crate::model::{AccessKind, EvidenceItem, QueryEnvelope, RepoIndex, SourceLanguage};

/// Minimum premise support for a rule to be promoted to a standing invariant.
///
/// Four is the documented floor: it filters out 2-file coincidences (e.g. the
/// app-sdk pairs that happen to share one or two files) while still admitting
/// the real clusters (the Keycloak `KEYCLOAK_BASE_URL ⟺ KEYCLOAK_REALM` pair
/// sits at support 4). Lowering this would manufacture spurious invariants and
/// erode operator trust in the doctor.
const SUPPORT_MIN: u32 = 4;

/// Minimum sampled extent-stability for promotion.
///
/// A real cohesive cluster keeps closing back to the consequent as objects are
/// removed (stability near 1.0); a coincidence collapses. 0.60 is the
/// conservative threshold from the plan — high enough to reject coincidences,
/// low enough not to drop genuine clusters.
const STABILITY_MIN: f64 = 0.60;

/// Repo-relative path to the committed, human-curated baseline.
///
/// Git-tracked (NOT under the gitignored `.leio-code/`) so it travels with the
/// repository and is the single source of enforced invariants. Resolved against
/// the workspace root threaded through [`Doctor::run`]. Changing this path
/// silently disables enforcement on clean clones, so it is pinned here.
const BASELINE_REL_PATH: &str = "leio-code/baselines/induced-invariants.json";

/// Environment flag that switches the doctor into baseline-refresh mode.
///
/// When set to `1`, the doctor re-mines the current candidates and overwrites
/// the baseline file instead of enforcing it. A refresh is a curation/bootstrap
/// action, never a passing gate; the human reviews the resulting git diff and
/// prunes to the accepted subset before committing. Following the established
/// `LEIO_*` doctor-flag precedent (the `Doctor` trait signature is fixed).
const REFRESH_ENV: &str = "LEIO_INDUCED_INVARIANTS_REFRESH";

/// On-disk schema version for the baseline file.
const BASELINE_SCHEMA_VERSION: u32 = 1;

/// One curated invariant as stored in the baseline file.
///
/// Only `(premise, consequent)` is the enforced identity; `support`,
/// `stability`, and `confidence` are informational curation metadata.
#[derive(Debug, Clone, Deserialize)]
struct BaselineInvariant {
    premise: String,
    consequent: String,
    #[serde(default)]
    support: u32,
    #[serde(default)]
    stability: f64,
}

/// The committed baseline: schema version plus curated invariants.
///
/// `#[serde(default)]` on every field keeps the shape forward-compatible with
/// extra keys (e.g. `generated_at`, `note`) the curator may add.
#[derive(Debug, Clone, Deserialize)]
struct InvariantBaseline {
    #[serde(default)]
    schema_version: u32,
    #[serde(default)]
    invariants: Vec<BaselineInvariant>,
}

/// A promoted standing invariant `premise => consequent`.
#[derive(Debug, Clone)]
struct PromotedInvariant {
    premise: String,
    consequent: String,
    support: u32,
    stability: f64,
}

/// A file that violates a promoted invariant by carrying the premise without
/// the consequent.
#[derive(Debug, Clone)]
struct InvariantViolation {
    premise: String,
    consequent: String,
    file: String,
    line: usize,
}

pub struct InducedInvariantsDoctor;

impl Doctor for InducedInvariantsDoctor {
    fn name(&self) -> &'static str {
        "induced-invariants"
    }

    fn description(&self) -> &'static str {
        "Mines stable single-premise env/redis co-occurrence invariants and warns on any current violation."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_induced_invariants(index, root)
    }
}

/// Returns true for the languages the code graph (and thus `graph.nq`) covers.
///
/// Mirrors `code_graph::supports_code_graph` so the doctor's mining domain
/// equals the graph-covered incidence set written to `graph.nq`.
fn is_graph_covered(language: SourceLanguage) -> bool {
    matches!(
        language,
        SourceLanguage::Rust
            | SourceLanguage::Python
            | SourceLanguage::JavaScript
            | SourceLanguage::TypeScript
            | SourceLanguage::Tsx
    )
}

/// Access-typed env attribute label, identical to `export::env_attribute_label`.
fn env_label(access: AccessKind, name: &str) -> String {
    let prefix = match access {
        AccessKind::Read => "readsEnv",
        AccessKind::Write => "writesEnv",
        AccessKind::Declared => "declaresEnv",
        AccessKind::Unknown => "mentionsEnv",
    };
    format!("{prefix}:{name}")
}

/// Access-typed redis attribute label, identical to `export::redis_attribute_label`.
fn redis_label(access: AccessKind, key: &str) -> String {
    let prefix = match access {
        AccessKind::Read => "readsRedis",
        AccessKind::Write => "writesRedis",
        AccessKind::Declared => "declaresRedis",
        AccessKind::Unknown => "mentionsRedis",
    };
    format!("{prefix}:{key}")
}

/// Collects the `(file_path, attribute_label)` incidence pairs the doctor mines.
///
/// Restricted to graph-covered languages and to access-typed env/redis labels —
/// the exact incidence set `code_graph` writes to `graph.nq`. The first line at
/// which each attribute occurs in each file is recorded so violations can point
/// at a concrete `file:line`.
fn graph_incidence_pairs(index: &RepoIndex) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for file in &index.files {
        if !is_graph_covered(file.language) {
            continue;
        }
        for env in &file.env_vars {
            pairs.push((file.path.clone(), env_label(env.access, &env.name)));
        }
        for redis in &file.redis_keys {
            pairs.push((file.path.clone(), redis_label(redis.access, &redis.key)));
        }
    }
    pairs
}

/// Promotes mined implications that clear the support + stability gate.
///
/// Deterministic: input order from [`mine_single_premise`] is already stable,
/// and the gate is a pure filter, so the promoted set is reproducible.
fn promote(rules: &[Implication]) -> Vec<PromotedInvariant> {
    rules
        .iter()
        .filter(|rule| rule.support >= SUPPORT_MIN && rule.stability >= STABILITY_MIN)
        .map(|rule| PromotedInvariant {
            premise: rule.premise.clone(),
            consequent: rule.consequent.clone(),
            support: rule.support,
            stability: rule.stability,
        })
        .collect()
}

/// Validates promoted invariants against the live incidence pairs.
///
/// For each rule `a => b`, every object in `extent(a)` must also be in
/// `extent(b)`. Any object that carries the premise but not the consequent is a
/// violation. The offending object label is a repo-relative file path; the
/// occurrence line is not carried on the pairs, so evidence anchors at line 1
/// of the offending file (the path is the load-bearing locator).
fn validate(
    invariants: &[PromotedInvariant],
    pairs: &[(String, String)],
) -> Vec<InvariantViolation> {
    use std::collections::{BTreeMap, BTreeSet};

    let mut objects_by_attribute: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (object, attribute) in pairs {
        objects_by_attribute
            .entry(attribute.as_str())
            .or_default()
            .insert(object.as_str());
    }
    let extent_of = |attribute: &str| -> BTreeSet<&str> {
        objects_by_attribute
            .get(attribute)
            .cloned()
            .unwrap_or_default()
    };

    let mut violations = Vec::new();
    for invariant in invariants {
        let premise_extent = extent_of(&invariant.premise);
        let consequent_extent = extent_of(&invariant.consequent);
        for file in premise_extent.difference(&consequent_extent) {
            violations.push(InvariantViolation {
                premise: invariant.premise.clone(),
                consequent: invariant.consequent.clone(),
                file: (*file).to_string(),
                line: 1,
            });
        }
    }
    violations
}

/// Mines, promotes, and validates in one pass over `pairs`.
///
/// Returns the promoted invariants and any violations. Pure over its input so
/// tests can inject a mutated incidence set and assert the violation surfaces.
/// Retained as the candidate-mining oracle the unit tests exercise; the live
/// doctor sources its enforced set from the committed baseline, not from here.
#[cfg(test)]
fn analyze(pairs: &[(String, String)]) -> (Vec<PromotedInvariant>, Vec<InvariantViolation>) {
    let promoted = candidates(pairs);
    let violations = validate(&promoted, pairs);
    (promoted, violations)
}

/// Mines and promotes current candidates (info-only discovery; never gates).
fn candidates(pairs: &[(String, String)]) -> Vec<PromotedInvariant> {
    promote(&fca::mine_single_premise(pairs))
}

/// Maps curated baseline entries onto the enforced invariant set.
///
/// Carries the curation metadata (`support`, `stability`) through for evidence,
/// but enforcement keys only on `(premise, consequent)`.
fn baseline_to_promoted(baseline: &InvariantBaseline) -> Vec<PromotedInvariant> {
    baseline
        .invariants
        .iter()
        .map(|entry| PromotedInvariant {
            premise: entry.premise.clone(),
            consequent: entry.consequent.clone(),
            support: entry.support,
            stability: entry.stability,
        })
        .collect()
}

/// Serializes promoted candidates into the on-disk baseline JSON.
///
/// Deterministic key ordering (`serde_json` is built with `preserve_order`) so
/// the round-trip diff a curator reviews is stable. `generated_at` records the
/// re-mine time; `note` documents the human-curation contract.
///
/// # Errors
///
/// Returns the `serde_json` error if serialization fails.
fn serialize_baseline(candidates: &[PromotedInvariant]) -> Result<String, serde_json::Error> {
    let invariants: Vec<_> = candidates
        .iter()
        .map(|invariant| {
            json!({
                "premise": invariant.premise,
                "consequent": invariant.consequent,
                "support": invariant.support,
                "stability": invariant.stability,
                "confidence": 1.0,
            })
        })
        .collect();
    let doc = json!({
        "schema_version": BASELINE_SCHEMA_VERSION,
        "generated_at": time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
        "note": "Re-mined candidate set. Human-curated: review the git diff and \
    prune to the accepted subset before committing. FCA proposes; humans promote.",
        "invariants": invariants,
    });
    serde_json::to_string_pretty(&doc)
}

/// Renders an info envelope (no warnings) with the given summary and meta.
fn info_envelope(started: Instant, summary: String, meta: serde_json::Value) -> QueryEnvelope {
    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_induced_invariants"),
        kind: "doctor".to_string(),
        summary,
        confidence: 0.98,
        entities: vec![],
        evidence: vec![],
        warnings: vec![],
        meta: Some(meta),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// Runs the induced-invariants doctor over `index`.
pub fn doctor_induced_invariants(index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let pairs = graph_incidence_pairs(index);

    // Candidate discovery (info only): the live mining output. Never gates.
    let candidates = candidates(&pairs);
    let baseline_path = root.join(BASELINE_REL_PATH);
    let refresh = std::env::var(REFRESH_ENV)
        .map(|v| v == "1")
        .unwrap_or(false);

    // Refresh mode: re-mine and overwrite the baseline; do NOT enforce. Same
    // write path also bootstraps an absent baseline from current candidates.
    if refresh {
        return refresh_baseline(started, &baseline_path, &candidates);
    }

    // Absent baseline => clean skip (info, never a warning).
    if !baseline_path.exists() {
        return info_envelope(
            started,
            format!(
                "induced-invariants: no baseline at {} — skipped",
                baseline_path.display()
            ),
            json!({
                "activated": false,
                "candidate_count": candidates.len(),
                "baseline_path": baseline_path.display().to_string(),
            }),
        );
    }

    // Load + parse the curated baseline. Parse failure is a warning, not a panic.
    let raw = match std::fs::read_to_string(&baseline_path) {
        Ok(content) => content,
        Err(err) => {
            return finalize_warnings(
                started,
                vec![format!(
                    "failed to read induced-invariants baseline {}: {err}",
                    baseline_path.display()
                )],
                json!({ "activated": false }),
            );
        }
    };
    let baseline: InvariantBaseline = match serde_json::from_str(&raw) {
        Ok(parsed) => parsed,
        Err(err) => {
            return finalize_warnings(
                started,
                vec![format!(
                    "failed to parse induced-invariants baseline: {err}"
                )],
                json!({ "activated": false }),
            );
        }
    };

    // Enforce the curated baseline against the CURRENT context. This is the
    // exact validate() flow `catches_synthetic_violation` proves: a promoted
    // set produced from a different context (here, the committed baseline)
    // validated against the live one.
    let baseline_promoted = baseline_to_promoted(&baseline);
    let violations = validate(&baseline_promoted, &pairs);

    // Detect baseline premises whose label has vanished from the current
    // context (an access-reclassification / stale-baseline signal, not a gate).
    let present_labels: std::collections::HashSet<&str> =
        pairs.iter().map(|(_, attr)| attr.as_str()).collect();
    let stale_premises: Vec<String> = baseline_promoted
        .iter()
        .filter(|inv| !present_labels.contains(inv.premise.as_str()))
        .map(|inv| inv.premise.clone())
        .collect();

    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    for violation in &violations {
        warnings.push(format!(
            "file `{}` carries `{}` but not `{}`, violating the curated invariant `{} => {}` (scope: file env/redis attributes)",
            violation.file,
            violation.premise,
            violation.consequent,
            violation.premise,
            violation.consequent,
        ));
        evidence.push(EvidenceItem {
            kind: "induced-invariant-violation".to_string(),
            path: violation.file.clone(),
            line: Some(violation.line),
            detail: format!(
                "curated invariant `{} => {}` (single-premise, exception-free, file env/redis scope) is broken here",
                violation.premise, violation.consequent
            ),
        });
    }

    let candidate_json: Vec<_> = candidates
        .iter()
        .map(|invariant| {
            json!({
                "premise": invariant.premise,
                "consequent": invariant.consequent,
                "support": invariant.support,
                "stability": invariant.stability,
                "confidence": 1.0,
            })
        })
        .collect();
    let baseline_json: Vec<_> = baseline_promoted
        .iter()
        .map(|invariant| {
            json!({
                "premise": invariant.premise,
                "consequent": invariant.consequent,
                "support": invariant.support,
                "stability": invariant.stability,
            })
        })
        .collect();

    let entities = vec![json!({
        "baseline_invariant_count": baseline_promoted.len(),
        "candidate_count": candidates.len(),
        "violation_count": violations.len(),
        "stale_premise_count": stale_premises.len(),
        "support_min": SUPPORT_MIN,
        "stability_min": STABILITY_MIN,
        "scope": "file × {env-attr, redis-attr}, graph-covered languages",
        "enforced_invariants": baseline_json,
        "candidate_invariants": candidate_json,
    })];

    let meta = json!({
        "activated": true,
        "baseline_invariant_count": baseline_promoted.len(),
        "candidate_count": candidates.len(),
        "baseline_schema_version": baseline.schema_version,
        "stale_baseline_premises": stale_premises,
    });

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_induced_invariants"),
        kind: "doctor".to_string(),
        summary: format!(
            "enforced {} curated env/redis invariants ({} candidates discovered); found {} violations",
            baseline_promoted.len(),
            candidates.len(),
            violations.len()
        ),
        confidence: if warnings.is_empty() { 0.98 } else { 0.72 },
        entities,
        evidence,
        warnings,
        meta: Some(meta),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// Writes the re-mined candidate set to the baseline path; never enforces.
fn refresh_baseline(
    started: Instant,
    baseline_path: &Path,
    candidates: &[PromotedInvariant],
) -> QueryEnvelope {
    let mut warnings = Vec::new();
    let mut wrote = false;
    match serialize_baseline(candidates) {
        Ok(json_text) => {
            if let Some(parent) = baseline_path.parent()
                && let Err(err) = std::fs::create_dir_all(parent)
            {
                warnings.push(format!(
                    "failed to create baseline directory {}: {err}",
                    parent.display()
                ));
            }
            if warnings.is_empty() {
                match std::fs::write(baseline_path, json_text) {
                    Ok(()) => wrote = true,
                    Err(err) => warnings.push(format!(
                        "failed to write induced-invariants baseline {}: {err}",
                        baseline_path.display()
                    )),
                }
            }
        }
        Err(err) => warnings.push(format!(
            "failed to serialize induced-invariants baseline: {err}"
        )),
    }

    let meta = json!({
        "activated": false,
        "refreshed": wrote,
        "candidate_count": candidates.len(),
        "baseline_path": baseline_path.display().to_string(),
    });

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_induced_invariants"),
        kind: "doctor".to_string(),
        summary: if wrote {
            format!(
                "refreshed induced-invariants baseline with {} candidates (NOT enforced — review the git diff and prune before committing)",
                candidates.len()
            )
        } else {
            "induced-invariants refresh failed; baseline unchanged".to_string()
        },
        confidence: if warnings.is_empty() { 0.98 } else { 0.72 },
        entities: vec![],
        evidence: vec![],
        warnings,
        meta: Some(meta),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// Renders a warnings envelope (e.g. baseline read/parse failure).
fn finalize_warnings(
    started: Instant,
    warnings: Vec<String>,
    meta: serde_json::Value,
) -> QueryEnvelope {
    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_induced_invariants"),
        kind: "doctor".to_string(),
        summary: "induced-invariants: baseline could not be loaded".to_string(),
        confidence: 0.72,
        entities: vec![],
        evidence: vec![],
        warnings,
        meta: Some(meta),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keycloak_pairs() -> Vec<(String, String)> {
        let mut pairs = Vec::new();
        for file in ["a.mjs", "b.mjs", "c.mjs", "d.mjs"] {
            pairs.push((file.to_string(), "readsEnv:KEYCLOAK_BASE_URL".to_string()));
            pairs.push((file.to_string(), "readsEnv:KEYCLOAK_REALM".to_string()));
        }
        pairs
    }

    #[test]
    fn promotes_keycloak_pair_as_held_invariant() {
        let pairs = keycloak_pairs();
        let (promoted, violations) = analyze(&pairs);
        assert!(
            promoted.iter().any(|i| {
                (i.premise == "readsEnv:KEYCLOAK_BASE_URL"
                    && i.consequent == "readsEnv:KEYCLOAK_REALM")
                    || (i.premise == "readsEnv:KEYCLOAK_REALM"
                        && i.consequent == "readsEnv:KEYCLOAK_BASE_URL")
            }),
            "keycloak pair must be promoted: {promoted:?}"
        );
        assert!(
            violations.is_empty(),
            "clean context must hold: {violations:?}"
        );
    }

    #[test]
    fn does_not_promote_two_file_coincidence() {
        // Two files share an env pair: support 2, below SUPPORT_MIN.
        let mut pairs = Vec::new();
        for file in ["x.mjs", "y.mjs"] {
            pairs.push((file.to_string(), "readsEnv:APP_SDK_A".to_string()));
            pairs.push((file.to_string(), "readsEnv:APP_SDK_B".to_string()));
        }
        let (promoted, _) = analyze(&pairs);
        assert!(
            promoted.is_empty(),
            "2-file coincidence must not be promoted: {promoted:?}"
        );
    }

    #[test]
    fn catches_synthetic_violation() {
        // Start from the held keycloak cluster, then add a fifth file that
        // reads BASE_URL but NOT REALM. The promoted invariant (mined from the
        // first four) is now violated by the fifth file.
        let mut pairs = keycloak_pairs();
        pairs.push((
            "rogue.mjs".to_string(),
            "readsEnv:KEYCLOAK_BASE_URL".to_string(),
        ));

        // Mine the invariant from the clean cluster only...
        let clean = keycloak_pairs();
        let promoted = promote(&fca::mine_single_premise(&clean));
        // ...then validate against the mutated incidence set.
        let violations = validate(&promoted, &pairs);

        let rogue: Vec<_> = violations
            .iter()
            .filter(|v| {
                v.file == "rogue.mjs"
                    && v.premise == "readsEnv:KEYCLOAK_BASE_URL"
                    && v.consequent == "readsEnv:KEYCLOAK_REALM"
            })
            .collect();
        assert_eq!(
            rogue.len(),
            1,
            "exactly one violation expected: {violations:?}"
        );
        assert_eq!(rogue[0].line, 1);
    }

    #[test]
    fn analysis_is_deterministic() {
        let pairs = keycloak_pairs();
        let (first_inv, first_v) = analyze(&pairs);
        let (second_inv, second_v) = analyze(&pairs);
        let key = |inv: &[PromotedInvariant]| {
            inv.iter()
                .map(|i| (i.premise.clone(), i.consequent.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(key(&first_inv), key(&second_inv));
        assert_eq!(first_v.len(), second_v.len());
    }

    /// Loads the committed baseline file (`CARGO_MANIFEST_DIR` = `leio-code/`).
    fn committed_baseline() -> InvariantBaseline {
        // `BASELINE_REL_PATH` is workspace-root relative; `CARGO_MANIFEST_DIR`
        // already points at the `leio-code` crate, so strip the leading segment.
        let crate_rel = BASELINE_REL_PATH
            .strip_prefix("leio-code/")
            .unwrap_or(BASELINE_REL_PATH);
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(crate_rel);
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("baseline must be committed at {}: {e}", path.display()));
        serde_json::from_str(&raw).expect("committed baseline must parse")
    }

    #[test]
    fn committed_baseline_contains_keycloak_pair() {
        let baseline = committed_baseline();
        assert_eq!(baseline.schema_version, BASELINE_SCHEMA_VERSION);
        assert!(
            baseline.invariants.iter().any(|i| {
                i.premise == "readsEnv:KEYCLOAK_BASE_URL"
                    && i.consequent == "readsEnv:KEYCLOAK_REALM"
            }),
            "committed baseline must seed the Keycloak pair: {baseline:?}"
        );
    }

    #[test]
    fn enforces_committed_baseline_and_names_violating_live_file() {
        // Curated baseline is the enforced set. Build a CURRENT context that
        // holds every baseline invariant except one: a real, live-shaped file
        // path reads KEYCLOAK_BASE_URL but NOT KEYCLOAK_REALM.
        let baseline = committed_baseline();
        let promoted = baseline_to_promoted(&baseline);

        let mut pairs = Vec::new();
        // Four clean carriers so the held invariants still hold.
        for file in [
            "example-ops/src/lib/keycloak.ts",
            "jai-pay/src/lib/keycloak.ts",
            "health-audit-console/src/lib/keycloak.ts",
        ] {
            pairs.push((file.to_string(), "readsEnv:KEYCLOAK_BASE_URL".to_string()));
            pairs.push((file.to_string(), "readsEnv:KEYCLOAK_REALM".to_string()));
        }
        // The rogue live-shaped file: premise without the consequent.
        let rogue = "example-ops/src/app/api/auth/[...nextauth]/route.ts";
        pairs.push((rogue.to_string(), "readsEnv:KEYCLOAK_BASE_URL".to_string()));

        let violations = validate(&promoted, &pairs);

        let hit: Vec<_> = violations
            .iter()
            .filter(|v| {
                v.file == rogue
                    && v.premise == "readsEnv:KEYCLOAK_BASE_URL"
                    && v.consequent == "readsEnv:KEYCLOAK_REALM"
            })
            .collect();
        assert_eq!(
            hit.len(),
            1,
            "baseline enforcement must name the rogue file once: {violations:?}"
        );
        assert_eq!(hit[0].line, 1);

        // The warning surfaced by the doctor body must name the file path.
        let warning = format!(
            "file `{}` carries `{}` but not `{}`, violating the curated invariant `{} => {}` (scope: file env/redis attributes)",
            hit[0].file, hit[0].premise, hit[0].consequent, hit[0].premise, hit[0].consequent,
        );
        assert!(
            warning.contains(rogue),
            "warning must name the violating file: {warning}"
        );
    }

    #[test]
    fn baseline_round_trips_through_serialize() {
        // Serialize a candidate set and re-parse: keys survive deterministically.
        let candidates = vec![PromotedInvariant {
            premise: "readsEnv:PW_EMAIL".to_string(),
            consequent: "readsEnv:PW_PASSWORD".to_string(),
            support: 8,
            stability: 1.0,
        }];
        let json_text = serialize_baseline(&candidates).expect("serialize");
        let parsed: InvariantBaseline = serde_json::from_str(&json_text).expect("re-parse");
        assert_eq!(parsed.schema_version, BASELINE_SCHEMA_VERSION);
        assert_eq!(parsed.invariants.len(), 1);
        assert_eq!(parsed.invariants[0].premise, "readsEnv:PW_EMAIL");
        assert_eq!(parsed.invariants[0].consequent, "readsEnv:PW_PASSWORD");
    }
}
