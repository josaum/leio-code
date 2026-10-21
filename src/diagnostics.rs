//! Machine-readable diagnostic rendering for the `doctor` subcommand.
//!
//! # Exit-code contract
//!
//! The `doctor` and `verify` paths converge on a stable exit-code table so that
//! CI and IDE integrations can branch on numeric codes without parsing output:
//!
//! | Code  | Meaning                                                                  |
//! |-------|--------------------------------------------------------------------------|
//! | `0`   | Envelope has no warnings (clean run).                                    |
//! | `1`   | Envelope has at least one warning. Same behavior across text/json/sarif. |
//! | `2`   | Doctor configuration error (e.g. unknown DoctorKind — clap rejects most  |
//! |       | of these earlier, so reaching `2` from this module is rare).             |
//! | `64+` | Reserved for internal tool errors (panics, IO failures, anyhow chains).  |
//!
//! Confidence is reported in the envelope but does *not* gate the exit code in
//! the current contract — keeping warning-only matches the existing
//! `print_and_maybe_fail` behavior for `Text` mode, so all three formats agree.
//!
//! # SARIF 2.1.0
//!
//! `--format=sarif` emits a SARIF 2.1.0 document with `runs[0].tool.driver`
//! identifying `leio-code`, one `result` per diagnostic, severity mapped to
//! `level` (`error` / `warning` / `note`), and an optional
//! `locations[0].physicalLocation` when the diagnostic carries a path.
//! Reproducibility metadata (index version + repo commit SHA + tool version)
//! lives on `runs[0].invocations[0].properties` so downstream consumers can
//! diff reports across runs.

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::model::QueryEnvelope;

/// Output format selector for `doctor --format=…`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagFormat {
    /// Pretty human-readable text (existing behavior).
    Text,
    /// Flat JSON with a small, stable shape — no SARIF nesting.
    Json,
    /// SARIF 2.1.0 (https://docs.oasis-open.org/sarif/sarif/v2.1.0/).
    Sarif,
}

/// Severity level for a single diagnostic. Maps 1:1 to SARIF `level`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Note,
}

impl Severity {
    /// SARIF `level` string (`"error" | "warning" | "note"`).
    pub fn as_sarif_level(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        }
    }
}

/// A single diagnostic in the format-agnostic intermediate representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub rule_id: String,
    pub severity: Severity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub message: String,
}

/// Reproducibility metadata stamped on every machine-readable report.
#[derive(Debug, Clone, Serialize)]
pub struct RunMeta {
    pub index_version: u32,
    pub commit_sha: String,
    pub tool_version: &'static str,
}

/// Render an entire `QueryEnvelope` in the requested format.
///
/// Evidence items become `Note` diagnostics; warnings become `Warning`
/// diagnostics with no path attached (warnings in `QueryEnvelope` are free-form
/// strings and don't carry structured locations).
pub fn render(envelope: &QueryEnvelope, format: DiagFormat, meta: &RunMeta) -> String {
    if matches!(format, DiagFormat::Text) {
        return render_text(envelope);
    }
    let diags = envelope_to_diagnostics(envelope);
    render_diagnostics(&diags, format, meta)
}

/// Render a pre-built diagnostic list in the requested format. Useful when the
/// caller has already collected diagnostics from a source other than a
/// `QueryEnvelope` (e.g. synthetic test cases, future per-line evidence).
///
/// `DiagFormat::Text` is accepted but produces only the warning summary — for
/// the full pretty-text rendering use [`render`] with an envelope.
pub fn render_diagnostics(diags: &[Diagnostic], format: DiagFormat, meta: &RunMeta) -> String {
    match format {
        DiagFormat::Text => diags
            .iter()
            .map(|d| {
                format!(
                    "[{}] {}: {}",
                    d.severity.as_sarif_level(),
                    d.rule_id,
                    d.message
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        DiagFormat::Json => render_json(diags, meta),
        DiagFormat::Sarif => render_sarif(diags, meta),
    }
}

/// True if the envelope should cause a non-zero exit under the doctor contract
/// (any warning -> exit 1). Confidence is intentionally *not* part of the gate
/// today; see the module-level contract table.
pub fn should_fail(envelope: &QueryEnvelope) -> bool {
    !envelope.warnings.is_empty()
}

fn envelope_to_diagnostics(envelope: &QueryEnvelope) -> Vec<Diagnostic> {
    let mut out = Vec::with_capacity(envelope.evidence.len() + envelope.warnings.len());
    for ev in &envelope.evidence {
        out.push(Diagnostic {
            rule_id: ev.kind.clone(),
            severity: Severity::Note,
            path: Some(ev.path.clone()),
            line: ev.line,
            message: ev.detail.clone(),
        });
    }
    for w in &envelope.warnings {
        out.push(Diagnostic {
            rule_id: envelope.kind.clone(),
            severity: Severity::Warning,
            path: None,
            line: None,
            message: w.clone(),
        });
    }
    out
}

fn render_text(envelope: &QueryEnvelope) -> String {
    let mut buf = String::new();
    buf.push_str(&envelope.summary);
    buf.push('\n');
    if !envelope.warnings.is_empty() {
        buf.push_str("warnings:\n");
        for w in &envelope.warnings {
            buf.push_str(&format!("- {}\n", w));
        }
    }
    if !envelope.evidence.is_empty() {
        buf.push_str("evidence:\n");
        for ev in &envelope.evidence {
            match ev.line {
                Some(line) => buf.push_str(&format!(
                    "- {}:{} [{}] {}\n",
                    ev.path, line, ev.kind, ev.detail
                )),
                None => buf.push_str(&format!("- {} [{}] {}\n", ev.path, ev.kind, ev.detail)),
            }
        }
    }
    buf
}

fn render_json(diags: &[Diagnostic], meta: &RunMeta) -> String {
    // `schema_version` first per `docs/output-schema.md` §7. Order is
    // preserved because the crate enables `serde_json/preserve_order`.
    let doc = json!({
        "schema_version": crate::model::SCHEMA_VERSION,
        "meta": {
            "index_version": meta.index_version,
            "commit_sha": meta.commit_sha,
            "tool_version": meta.tool_version,
        },
        "diagnostics": diags,
    });
    serde_json::to_string_pretty(&doc).expect("json render")
}

/// How mechanical is the auto-fix for a rule? Only `High` rules produce a
/// template diff from `--suggest`; `Medium` rules explain why no diff is
/// available; `Low` rules have no mechanical fix at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestConfidence {
    Low,
    Medium,
    High,
}

impl SuggestConfidence {
    pub fn as_str(self) -> &'static str {
        match self {
            SuggestConfidence::Low => "low",
            SuggestConfidence::Medium => "medium",
            SuggestConfidence::High => "high",
        }
    }
}

/// Static documentation for a single doctor rule, surfaced by
/// `doctor --explain <rule-id>` and `doctor --suggest <rule-id>`.
///
/// The presentation layer (`main.rs`) emits the description + citation +
/// fix_advice verbatim, then appends any matching `EvidenceItem`s from the
/// owning doctor's envelope so a reviewer can paste the result into a PR
/// comment. `--suggest` emits a template unified-diff when `suggest_fn` is
/// `Some` and `suggest_confidence` is `High`. It never writes to the working
/// tree.
#[derive(Debug, Clone)]
pub struct RuleDoc {
    /// Matches `EvidenceItem.kind` for the rule's violations.
    pub rule_id: &'static str,
    /// Optional `<doctor>/<rule>` or legacy aliases accepted by `rule_doc()` and `matches_evidence()`.
    pub aliases: &'static [&'static str],
    /// One-paragraph description of what this rule checks.
    pub description: &'static str,
    /// Conceptual fix advice. Markdown allowed; no actual code generation.
    pub fix_advice: &'static str,
    /// Spec citation: doctor module path or README anchor.
    pub citation: &'static str,
    /// Name of the doctor that emits this rule's evidence. Used by the CLI
    /// to run a single doctor when the user passes `doctor all --explain X`
    /// — saves running ~60 doctors when 1 suffices.
    pub doctor_name: &'static str,
    /// How mechanical is the auto-fix? Only `High` rules qualify for `--suggest`.
    pub suggest_confidence: SuggestConfidence,
    /// Returns a unified-diff patch string when the fix is mechanical, else None.
    /// Receives an `EvidenceItem` but for template (phase-1) suggesters the
    /// violation fields are not used — the diff is a fixed template. Never
    /// writes to disk.
    pub suggest_fn: Option<fn(&crate::model::EvidenceItem) -> Option<String>>,
}

impl RuleDoc {
    /// True when `ev.kind` matches this rule's primary `rule_id` or one of its `aliases`.
    pub fn matches_evidence(&self, ev: &crate::model::EvidenceItem) -> bool {
        ev.kind == self.rule_id || self.aliases.contains(&ev.kind.as_str())
    }
}

/// Look up a single rule's static documentation by `rule_id` (or alias). Returns
/// `None` for unknown ids — the CLI maps that to a non-zero exit + a "try
/// `--explain list`" hint.
pub fn rule_doc(rule_id: &str) -> Option<&'static RuleDoc> {
    RULE_DOCS
        .iter()
        .find(|d| d.rule_id == rule_id || d.aliases.contains(&rule_id))
}

/// All registered rule docs, in stable declaration order. Used by
/// `doctor --explain list` to print every supported rule_id.
pub fn all_rule_docs() -> &'static [RuleDoc] {
    RULE_DOCS
}

/// Suggester for `redis_no_prefix`: emits a template unified-diff showing
/// how to add a `REDIS_KEY_PREFIX` declaration to the target's manifest.
/// This is a phase-1 template — the diff is fixed text, not derived from
/// the violation's path.
fn suggest_redis_no_prefix(_ev: &crate::model::EvidenceItem) -> Option<String> {
    Some(
        "--- a/deploy/<your-target>.env\n\
         +++ b/deploy/<your-target>.env\n\
         @@ -1,3 +1,4 @@\n\
          # Runtime environment for this deploy target.\n\
          # Add service-specific env vars below.\n\
         +REDIS_KEY_PREFIX=<tenant-or-cartridge-slug>\n\
          \n\
         # Route the key through the canonical tenant/cartridge builder\n\
         # (e.g. the repository Redis key module) using this prefix value.\n\
         # Never concatenate tenant_id into a key by hand.\n"
            .to_string(),
    )
}

fn suggest_env_undeclared_read(_ev: &crate::model::EvidenceItem) -> Option<String> {
    Some(
        "--- a/.env.example\n\
         +++ b/.env.example\n\
         @@ -1,2 +1,3 @@\n\
          # Documented runtime configuration variables\n\
         +<ENV_VAR_NAME>=<default-or-empty>\n\
         --- a/.leio-code/config.toml\n\
         +++ b/.leio-code/config.toml\n\
         @@ -1,2 +1,5 @@\n\
         +# Or exempt intentional ambient process variables:\n\
         +[doctors.env_contract]\n\
         +allow = [\"<ENV_VAR_NAME>\"]\n"
            .to_string(),
    )
}

fn suggest_tracked_noise(_ev: &crate::model::EvidenceItem) -> Option<String> {
    Some(
        "--- a/.gitignore\n\
         +++ b/.gitignore\n\
         @@ -1,2 +1,4 @@\n\
          # Local build and editor artifacts\n\
         +.DS_Store\n\
         +*.pyc\n\
         # Then untrack without deleting local state: git rm --cached <path>\n"
            .to_string(),
    )
}

fn suggest_rust_pin_drift(_ev: &crate::model::EvidenceItem) -> Option<String> {
    Some(
        "--- a/rust-toolchain.toml\n\
         +++ b/rust-toolchain.toml\n\
         @@ -1,2 +1,2 @@\n\
          [toolchain]\n\
         -channel = \"<drifted-version>\"\n\
         +channel = \"<canonical-version>\"\n"
            .to_string(),
    )
}

/// Static registry. Each entry is hand-written prose — no code generation
/// and no inference from doctor source. When a doctor's behavior changes the
/// matching entry here must be updated by hand.
static RULE_DOCS: &[RuleDoc] = &[
    RuleDoc {
        rule_id: "redis_no_prefix",
        aliases: &["redis-key-hygiene/no-prefix", "redis-key-hygiene/wildcard-keys"],
        description: "Flags Redis keys built without a tenant or cartridge prefix. \
             Operational state may be sharded by tenant; an unprefixed \
             key collides across tenants and silently leaks one tenant's \
             routes, sessions, or leases into another's lookups. The doctor \
             scans literal key strings and key-builder helpers across the \
             gateway and cartridges.",
        fix_advice: "Route the key through the canonical tenant- or cartridge-aware \
             builder (e.g. the helpers in `the repository Redis key module`). If \
             the key is genuinely global, register it on the allowlist in the \
             doctor instead of inlining a literal — that way the next reader \
             can tell intent apart from drift. Never concatenate `tenant_id` \
             into a key by hand.",
        citation: "src/doctors/redis_key_hygiene.rs",
        doctor_name: "redis-key-hygiene",
        suggest_confidence: SuggestConfidence::High,
        suggest_fn: Some(suggest_redis_no_prefix),
    },
    RuleDoc {
        rule_id: "redis_no_ttl",
        aliases: &["redis-key-hygiene/missing-ttl", "redis-key-hygiene/no-ttl"],
        description: "Flags `SET`/`HSET` call sites that write to Redis without setting \
             an explicit TTL on operational state. The repository treats Redis as the \
             hot store for routes, sessions, and leases — keys that live \
             forever accumulate as drift and starve memory budgets when a \
             cartridge churns its routing surface.",
        fix_advice: "Pass an explicit `EX`/`PX` or follow the write with `EXPIRE`, \
             matching the TTL the rest of that key family uses. If the value \
             is genuinely permanent (config, schema), promote it out of Redis \
             into the appropriate store and remove the write site — don't \
             silence the doctor with a comment.",
        citation: "src/doctors/redis_key_hygiene.rs",
        doctor_name: "redis-key-hygiene",
        suggest_confidence: SuggestConfidence::Medium,
        suggest_fn: None,
    },
    RuleDoc {
        rule_id: "env_undeclared_read",
        aliases: &["env-contract/undeclared-var", "env-contract/undeclared-read"],
        description: "Flags environment variables read in source code that are not declared \
             in any `.env*` file, `deploy/profiles/*`, `deploy/secret-sets/*`, Docker Compose \
             manifest, Dockerfile `ENV`/`ARG`, or Kubernetes ConfigMap.",
        fix_advice: "Declare the variable in `.env.example` (or the appropriate deploy profile / \
             manifest) so operators know it exists, or add it to `[doctors.env_contract] allow` \
             in `.leio-code/config.toml` if it is an ambient host variable.",
        citation: "src/doctors/env_contract.rs",
        doctor_name: "env-contract",
        suggest_confidence: SuggestConfidence::High,
        suggest_fn: Some(suggest_env_undeclared_read),
    },
    RuleDoc {
        rule_id: "import_boundary_violation",
        aliases: &["import-boundary/forbidden-import", "import-boundary/violation"],
        description: "Flags imports that cross a forbidden architectural boundary declared under \
             `[[doctors.import_boundary.rules]]` in `.leio-code/config.toml`.",
        fix_advice: "Move the shared type or helper into an allowed lower-layer module, invert \
             the dependency via a trait/interface, or adjust `[[doctors.import_boundary.rules]]` \
             if the boundary definition is outdated.",
        citation: "src/doctors/import_boundary.rs",
        doctor_name: "import-boundary",
        suggest_confidence: SuggestConfidence::Medium,
        suggest_fn: None,
    },
    RuleDoc {
        rule_id: "orphan_file",
        aliases: &["orphan-files/unreferenced-file", "orphan-files/orphan-file"],
        description: "Flags source files that have zero incoming resolved imports in the code graph \
             and are not recognized entrypoints, tests, binaries, or configured `[doctors.orphan_files]` \
             exemptions.",
        fix_advice: "Delete the file if it is dead code, wire it into the module tree / router \
             if it was meant to be active, or add its path to `[doctors.orphan_files] dynamic_entrypoints` \
             in `.leio-code/config.toml` if it is invoked externally.",
        citation: "src/doctors/orphan_files.rs",
        doctor_name: "orphan-files",
        suggest_confidence: SuggestConfidence::Medium,
        suggest_fn: None,
    },
    RuleDoc {
        rule_id: "conflict_leftover",
        aliases: &["repo-hygiene/conflict-leftover"],
        description: "Flags committed Git merge-conflict residue (`*.orig`, `*.rej`) or unresolved \
             conflict markers tracked in repository files.",
        fix_advice: "Delete the leftover `*.orig`/`*.rej` file or resolve the conflict and remove \
             all residue before committing.",
        citation: "src/doctors/repo_hygiene.rs",
        doctor_name: "repo-hygiene",
        suggest_confidence: SuggestConfidence::Medium,
        suggest_fn: None,
    },
    RuleDoc {
        rule_id: "tracked_noise",
        aliases: &[
            "repo-hygiene/tracked-noise",
            "repo-hygiene/debug-artifact",
            "repo-hygiene/secret-artifact",
        ],
        description: "Flags OS, editor, or build noise files (`.DS_Store`, `*.pyc`, `*.swp`, \
             coverage data, notebook checkpoints, or scratch artifacts) that are tracked by Git.",
        fix_advice: "Untrack the file with `git rm --cached <path>` and add its pattern to \
             `.gitignore` so it cannot be re-added.",
        citation: "src/doctors/repo_hygiene.rs",
        doctor_name: "repo-hygiene",
        suggest_confidence: SuggestConfidence::High,
        suggest_fn: Some(suggest_tracked_noise),
    },
    RuleDoc {
        rule_id: "duplicate_large_files",
        aliases: &["repo-hygiene/duplicate-large-files", "repo-hygiene/large-binary"],
        description: "Flags byte-identical large files (>5 MB) tracked at multiple paths in the repository.",
        fix_advice: "Consolidate the duplicate copies into a single canonical path (or generate \
             the mirror at release/build time instead of tracking duplicate blobs).",
        citation: "src/doctors/repo_hygiene.rs",
        doctor_name: "repo-hygiene",
        suggest_confidence: SuggestConfidence::Medium,
        suggest_fn: None,
    },
    RuleDoc {
        rule_id: "rust_pin_drift",
        aliases: &[
            "rust-toolchain-pin-coherence/version-mismatch",
            "rust-toolchain-pin-coherence/pin-drift",
        ],
        description: "Flags Rust toolchain version drift across `rust-toolchain.toml`, Dockerfiles \
             (`FROM rust:…`, `cargo-chef`), inline `channel = \"…\"` pins, and configured build scripts.",
        fix_advice: "Align every toolchain pin to the canonical `X.Y` minor declared in \
             `rust-toolchain.toml` so CI, container builds, and local checkouts compile with the \
             same Rust toolchain.",
        citation: "src/doctors/rust_toolchain_pin_coherence.rs",
        doctor_name: "rust-toolchain-pin-coherence",
        suggest_confidence: SuggestConfidence::High,
        suggest_fn: Some(suggest_rust_pin_drift),
    },
    RuleDoc {
        rule_id: "rust_channel_floating",
        aliases: &[
            "rust-toolchain-pin-coherence/floating-channel",
            "rust-toolchain-pin-coherence/channel-floating",
        ],
        description: "Flags `rust-toolchain.toml` when `channel` is set to a floating track \
             (`stable`, `beta`, or bare `nightly`) instead of an explicit `X.Y.Z` release.",
        fix_advice: "Replace the floating track in `rust-toolchain.toml` with an explicit \
             `channel = \"X.Y.Z\"` version so every host and container builds with the same compiler.",
        citation: "src/doctors/rust_toolchain_pin_coherence.rs",
        doctor_name: "rust-toolchain-pin-coherence",
        suggest_confidence: SuggestConfidence::High,
        suggest_fn: Some(suggest_rust_pin_drift),
    },
    RuleDoc {
        rule_id: "vendor_manifest_missing",
        aliases: &["vendored-crate-provenance/manifest-missing"],
        description: "Flags vendored crates under `vendor/` when `vendor/manifest.json` is missing \
             or unreadable.",
        fix_advice: "Create or restore `vendor/manifest.json` documenting the upstream repository, \
             path, commit SHA, policy (`pinned-exact` or `pinned-divergent`), and `content_sha256` \
             for every vendored crate.",
        citation: "src/doctors/vendored_crate_provenance.rs",
        doctor_name: "vendored-crate-provenance",
        suggest_confidence: SuggestConfidence::Medium,
        suggest_fn: None,
    },
    RuleDoc {
        rule_id: "vendor_provenance_violation",
        aliases: &["vendored-crate-provenance/provenance-violation"],
        description: "Flags vendored crates whose `Cargo.toml` version or `content_sha256` tree hash \
             violates the provenance record in `vendor/manifest.json`.",
        fix_advice: "Update `vendor/manifest.json` with the new upstream commit/version and \
             `content_sha256` (plus a `note` if `policy = \"pinned-divergent\"`), or revert \
             unauthorized local edits inside `vendor/<crate>`.",
        citation: "src/doctors/vendored_crate_provenance.rs",
        doctor_name: "vendored-crate-provenance",
        suggest_confidence: SuggestConfidence::Medium,
        suggest_fn: None,
    },
    RuleDoc {
        rule_id: "vendor_crate_unrecorded",
        aliases: &["vendored-crate-provenance/untracked-vendor"],
        description: "Flags directories under `vendor/` that contain a `Cargo.toml` but have no \
             corresponding entry in `vendor/manifest.json`.",
        fix_advice: "Add an entry for the vendored crate in `vendor/manifest.json` with its \
             upstream repo/path/commit and `content_sha256`, or remove the unrecorded directory \
             from `vendor/`.",
        citation: "src/doctors/vendored_crate_provenance.rs",
        doctor_name: "vendored-crate-provenance",
        suggest_confidence: SuggestConfidence::Medium,
        suggest_fn: None,
    },
    RuleDoc {
        rule_id: "skill_contract_violation",
        aliases: &["skill-contract/violation"],
        description: "Flags skill registry (`skills/registry.toml`) or `SKILL.md` contract violations: \
             unregistered skills, frontmatter schema/length errors, broken relative links or resources, \
             forbidden resource extensions (`.pyc`, `.zip`), or unallowlisted credential patterns.",
        fix_advice: "Register retained skills in `skills/registry.toml`, keep `SKILL.md` under 500 lines \
             with valid `name` + `description` frontmatter (40–1024 chars), fix broken relative links, \
             and remove forbidden artifacts or raw credentials.",
        citation: "src/doctors/skill_contract.rs",
        doctor_name: "skill-contract",
        suggest_confidence: SuggestConfidence::Medium,
        suggest_fn: None,
    },
    RuleDoc {
        rule_id: "placeholder-url",
        aliases: &["slop/placeholder-url"],
        description: "Flags placeholder URLs and dummy strings (`via.placeholder`, `placehold.it`, \
             `example.com/`, `lorem ipsum`, `unsplash.com/random`, bare `placeholder`) in `spine.json` \
             or deck source files.",
        fix_advice: "Replace placeholder URLs and dummy copy with real, verified asset paths and \
             grounded domain text.",
        citation: "src/doctors/slop_gate.rs",
        doctor_name: "slop",
        suggest_confidence: SuggestConfidence::Medium,
        suggest_fn: None,
    },
    RuleDoc {
        rule_id: "banned-phrases",
        aliases: &["slop/banned-phrases"],
        description: "Flags corporate filler and AI-slop clichés (EN + PT-BR, such as `leverage`, \
             `synergy`, `best-in-class`, `cutting-edge`, `seamless`, `game-changer`, `alavancar`, \
             `sinergias`, `estado da arte`) in `spine.json` or deck source files.",
        fix_advice: "Rewrite the sentence using concrete facts, numbers, and domain-specific nouns/verbs \
             instead of generic superlatives.",
        citation: "src/doctors/slop_gate.rs",
        doctor_name: "slop",
        suggest_confidence: SuggestConfidence::Medium,
        suggest_fn: None,
    },
    RuleDoc {
        rule_id: "action-titles",
        aliases: &["slop/action-titles"],
        description: "Flags `spine.json` `story[]` node titles that are topic labels rather than \
             assertions (missing a verb, digit/currency, and fewer than 5 words).",
        fix_advice: "Rewrite each `story[].title` as a governing takeaway assertion containing an \
             action verb or quantified metric.",
        citation: "src/doctors/slop_gate.rs",
        doctor_name: "slop",
        suggest_confidence: SuggestConfidence::Medium,
        suggest_fn: None,
    },
    RuleDoc {
        rule_id: "source-required",
        aliases: &["slop/source-required"],
        description: "Flags `spine.json` `story[]` nodes whose `sources[]` array is missing or empty.",
        fix_advice: "Populate `sources[]` on every `story[]` node with the backing document, table, \
             or query provenance.",
        citation: "src/doctors/slop_gate.rs",
        doctor_name: "slop",
        suggest_confidence: SuggestConfidence::Medium,
        suggest_fn: None,
    },
    RuleDoc {
        rule_id: "density",
        aliases: &["slop/density"],
        description: "Flags `spine.json` `story[]` nodes exceeding the 220-word soft density cap \
             (emitted as advisory evidence without failing the gate).",
        fix_advice: "Split dense story nodes across two slides or trim secondary prose so the slide \
             stays scannable.",
        citation: "src/doctors/slop_gate.rs",
        doctor_name: "slop",
        suggest_confidence: SuggestConfidence::Low,
        suggest_fn: None,
    },
];

fn render_sarif(diags: &[Diagnostic], meta: &RunMeta) -> String {
    let results: Vec<serde_json::Value> = diags
        .iter()
        .map(|d| {
            let mut result = json!({
                "ruleId": d.rule_id,
                "level": d.severity.as_sarif_level(),
                "message": { "text": d.message },
            });
            if let Some(path) = &d.path {
                let mut phys = json!({
                    "artifactLocation": { "uri": path },
                });
                if let Some(line) = d.line {
                    phys["region"] = json!({ "startLine": line });
                }
                result["locations"] = json!([{ "physicalLocation": phys }]);
            }
            result
        })
        .collect();

    let rules: Vec<serde_json::Value> = RULE_DOCS
        .iter()
        .map(|r| {
            json!({
                "id": r.rule_id,
                "shortDescription": { "text": r.description },
                "help": { "text": r.fix_advice },
                "properties": {
                    "doctor": r.doctor_name,
                    "citation": r.citation,
                    "suggest_confidence": r.suggest_confidence.as_str(),
                    "aliases": r.aliases,
                }
            })
        })
        .collect();

    // `schema_version` is the leio-code output-contract version (see
    // `docs/output-schema.md` §7). It is *not* the SARIF format version,
    // which lives at top-level `"version": "2.1.0"`. Placed first at the
    // SARIF root so consumers can branch on the contract version before
    // parsing the SARIF body — SARIF tolerates additional top-level keys.
    // The same value is also mirrored in
    // `runs[0].invocations[0].properties.schema_version` for tools that
    // walk SARIF strictly via the spec's reproducibility-properties slot.
    let doc = json!({
        "schema_version": crate::model::SCHEMA_VERSION,
        "version": "2.1.0",
        "$schema": "https://docs.oasis-open.org/sarif/sarif/v2.1.0/cos02/schemas/sarif-schema-2.1.0.json",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "leio-code",
                    "version": meta.tool_version,
                    "rules": rules,
                }
            },
            "invocations": [{
                "executionSuccessful": true,
                "properties": {
                    "schema_version": crate::model::SCHEMA_VERSION,
                    "index_version": meta.index_version,
                    "commit_sha": meta.commit_sha,
                }
            }],
            "results": results,
        }]
    });
    serde_json::to_string_pretty(&doc).expect("sarif render")
}
