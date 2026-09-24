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

/// Look up a single rule's static documentation by `rule_id`. Returns `None`
/// for unknown ids — the CLI maps that to a non-zero exit + a "try
/// `--explain list`" hint.
pub fn rule_doc(rule_id: &str) -> Option<&'static RuleDoc> {
    RULE_DOCS.iter().find(|d| d.rule_id == rule_id)
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

/// Static registry. Each entry is hand-written prose — no code generation
/// and no inference from doctor source. When a doctor's behavior changes the
/// matching entry here must be updated by hand.
static RULE_DOCS: &[RuleDoc] = &[
    RuleDoc {
        rule_id: "redis_no_prefix",
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
