//! Composite workspace audit super-command.
//!
//! `leio-code audit` rolls up several leio-code surfaces into one report so a
//! pre-deploy operator can spot drift in a single call:
//!
//! - Workspace snapshot (profile, file count, doctor count, facets)
//! - Verify (composite of every registered doctor for the active profile)
//! - Per-doctor highlights (any doctor that produced warnings gets a section)
//! - Capability surface (find / explain / graph / doctor / export kinds)
//! - Auto-generated next-step suggestions
//!
//! The roll-up is built from the *runtime* doctor registry — new doctors are
//! picked up automatically the moment they ship; nothing is hard-coded here.
//! Graph extras (e.g. `graph dead-code`) are best-effort: if the verb fails
//! at runtime the audit notes the skip and continues.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::capabilities::workspace_capabilities;
use crate::doctors::{run_all_doctors, run_doctor};
use crate::indexer::build_or_update_index;
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex, WorkspaceCapabilitySummary};

/// Cap on per-doctor evidence excerpts surfaced in the markdown report.
const EVIDENCE_PREVIEW_LIMIT: usize = 10;

/// Output format for the composite audit.
#[derive(Debug, Clone, Copy)]
pub enum AuditFormat {
    Markdown,
    Json,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditDoctorSummary {
    pub name: String,
    pub summary: String,
    pub warning_count: usize,
    pub evidence_count: usize,
    pub query_id: String,
    pub timing_ms: u128,
    /// Top warnings (capped at `EVIDENCE_PREVIEW_LIMIT`).
    pub warning_excerpts: Vec<String>,
    /// Top evidence excerpts (capped at `EVIDENCE_PREVIEW_LIMIT`).
    pub evidence_excerpts: Vec<EvidenceItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditExtraQuery {
    pub name: String,
    pub status: AuditExtraStatus,
    pub note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope: Option<QueryEnvelope>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditExtraStatus {
    Skipped,
    Ran,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditSummary {
    pub workspace_profile: String,
    pub indexed_at: String,
    pub generated_at: String,
    pub file_count: usize,
    pub deploy_target_count: usize,
    pub profile_count: usize,
    pub secret_set_count: usize,
    pub doctor_count: usize,
    pub failing_doctor_count: usize,
    pub warning_count: usize,
    pub passed: bool,
    pub timing_ms: u128,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditReport {
    pub summary: AuditSummary,
    pub verify: VerifyDigest,
    pub warnings: Vec<AuditDoctorSummary>,
    pub doctors: Vec<DoctorRow>,
    pub capabilities: WorkspaceCapabilitySummary,
    pub extras: Vec<AuditExtraQuery>,
    pub next_steps: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifyDigest {
    pub query_id: String,
    pub summary: String,
    pub warning_count: usize,
    pub timing_ms: u128,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorRow {
    pub name: String,
    pub warning_count: usize,
    pub evidence_count: usize,
    pub timing_ms: u128,
    pub passed: bool,
}

/// Build the composite audit report by running every registered doctor for the
/// current workspace profile, plus a small set of best-effort extras.
pub fn build_audit_report(repo: &Path, index_path: &Path) -> Result<AuditReport> {
    let started = Instant::now();
    let index = build_or_update_index(repo, index_path, false).with_context(|| {
        format!(
            "audit: failed to build or update index at {}",
            index_path.display()
        )
    })?;
    let capabilities = workspace_capabilities(&index, repo);

    let verify_envelope = run_all_doctors(&index, repo);
    let warnings = collect_doctor_highlights(&index, repo, &verify_envelope, &capabilities);
    let doctors = doctor_rows(&verify_envelope);

    let extras = run_audit_extras(&index, repo);

    let warning_count = verify_envelope.warnings.len();
    let failing_doctor_count = doctors.iter().filter(|row| !row.passed).count();
    let passed = warning_count == 0;

    let summary = AuditSummary {
        workspace_profile: capabilities.workspace_profile.clone(),
        indexed_at: index.indexed_at.clone(),
        generated_at: now_iso(),
        file_count: index.files.len(),
        deploy_target_count: index.deploy_targets.len(),
        profile_count: index.profiles.len(),
        secret_set_count: index.secret_sets.len(),
        doctor_count: doctors.len(),
        failing_doctor_count,
        warning_count,
        passed,
        timing_ms: started.elapsed().as_millis(),
    };

    let verify = VerifyDigest {
        query_id: verify_envelope.query_id.clone(),
        summary: verify_envelope.summary.clone(),
        warning_count,
        timing_ms: verify_envelope.timing_ms,
    };

    let next_steps = build_next_steps(&summary, &warnings, &capabilities);

    Ok(AuditReport {
        summary,
        verify,
        warnings,
        doctors,
        capabilities,
        extras,
        next_steps,
    })
}

/// Run the audit and render it to either markdown or JSON.
///
/// Returns the rendered string and the underlying report (so callers can decide
/// to fail on warnings without re-running anything).
pub fn render_audit(
    repo: &Path,
    index_path: &Path,
    format: AuditFormat,
) -> Result<(String, AuditReport)> {
    let report = build_audit_report(repo, index_path)?;
    let body = match format {
        AuditFormat::Markdown => render_markdown(&report),
        AuditFormat::Json => render_json(&report)?,
    };
    Ok((body, report))
}

/// Default index path delegate kept here so audit callers don't have to depend
/// on the indexer module directly.
pub fn default_audit_index_path(repo: &Path) -> PathBuf {
    crate::indexer::default_index_path(repo)
}

fn collect_doctor_highlights(
    index: &RepoIndex,
    root: &Path,
    verify_envelope: &QueryEnvelope,
    capabilities: &WorkspaceCapabilitySummary,
) -> Vec<AuditDoctorSummary> {
    // Pull per-doctor stats out of `run_all_doctors` so we know which ones
    // actually warned. Then re-run only the warning-producing doctors to grab
    // their individual evidence (the composite envelope mixes them all).
    let entities = &verify_envelope.entities;
    let mut highlights: Vec<AuditDoctorSummary> = Vec::new();

    for entity in entities {
        let Some(name) = entity.get("doctor").and_then(Value::as_str) else {
            continue;
        };
        let warning_count = entity
            .get("warning_count")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        if warning_count == 0 {
            continue;
        }

        // Re-run the named doctor so we get its evidence + warnings in
        // isolation. This keeps the composite report cheap (we only re-run
        // doctors that already failed) and the registry stays the source of
        // truth for what's available.
        let Some(envelope) = run_doctor(name, index, root) else {
            continue;
        };

        let warning_excerpts = envelope
            .warnings
            .iter()
            .take(EVIDENCE_PREVIEW_LIMIT)
            .cloned()
            .collect();
        let evidence_excerpts = envelope
            .evidence
            .iter()
            .take(EVIDENCE_PREVIEW_LIMIT)
            .cloned()
            .collect();

        highlights.push(AuditDoctorSummary {
            name: name.to_string(),
            summary: envelope.summary,
            warning_count: envelope.warnings.len(),
            evidence_count: envelope.evidence.len(),
            query_id: envelope.query_id,
            timing_ms: envelope.timing_ms,
            warning_excerpts,
            evidence_excerpts,
        });
    }

    let _ = capabilities; // capabilities are reported separately; available for future expansion
    highlights.sort_by(|a, b| {
        b.warning_count
            .cmp(&a.warning_count)
            .then_with(|| a.name.cmp(&b.name))
    });
    highlights
}

fn doctor_rows(verify_envelope: &QueryEnvelope) -> Vec<DoctorRow> {
    let mut rows: Vec<DoctorRow> = verify_envelope
        .entities
        .iter()
        .filter_map(|entity| {
            let name = entity.get("doctor").and_then(Value::as_str)?.to_string();
            let warning_count = entity
                .get("warning_count")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            let evidence_count = entity
                .get("evidence_count")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            let timing_ms = entity
                .get("timing_ms")
                .and_then(Value::as_u64)
                .map(u128::from)
                .unwrap_or(0);
            Some(DoctorRow {
                name,
                warning_count,
                evidence_count,
                timing_ms,
                passed: warning_count == 0,
            })
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

/// Best-effort extras. Each entry runs only if the underlying capability is
/// present at runtime; otherwise the report carries a "skipped" note so the
/// gap is visible without failing the whole audit.
fn run_audit_extras(index: &RepoIndex, root: &Path) -> Vec<AuditExtraQuery> {
    vec![run_dead_code_extra(index, root)]
}

fn run_dead_code_extra(index: &RepoIndex, root: &Path) -> AuditExtraQuery {
    match crate::graph_query::query_dead_code(index, root, 0) {
        Ok(envelope) => AuditExtraQuery {
            name: "graph.dead-code".to_string(),
            status: AuditExtraStatus::Ran,
            note: envelope.summary.clone(),
            envelope: Some(envelope),
        },
        Err(error) => AuditExtraQuery {
            name: "graph.dead-code".to_string(),
            status: AuditExtraStatus::Skipped,
            note: format!("graph dead-code failed: {error}"),
            envelope: None,
        },
    }
}

fn build_next_steps(
    summary: &AuditSummary,
    warnings: &[AuditDoctorSummary],
    capabilities: &WorkspaceCapabilitySummary,
) -> Vec<String> {
    let mut steps = Vec::new();
    if summary.passed {
        steps.push(format!(
            "audit clean for profile `{}` — keep `leio-code audit --strict` in pre-deploy",
            summary.workspace_profile
        ));
    } else {
        for highlight in warnings.iter().take(3) {
            steps.push(format!(
                "investigate `leio-code doctor {} --strict` ({} warnings)",
                highlight.name, highlight.warning_count
            ));
        }
        steps.push(
            "re-run `leio-code audit --format json --strict` for machine-readable triage"
                .to_string(),
        );
    }

    if capabilities.notes.is_empty() {
        steps.push("workspace facets fully populated; no follow-up notes".to_string());
    } else {
        for note in capabilities.notes.iter().take(2) {
            steps.push(format!("workspace note: {}", note));
        }
    }

    steps
}

fn render_markdown(report: &AuditReport) -> String {
    let mut out = String::new();
    let summary = &report.summary;
    out.push_str(&format!(
        "# Workspace Audit — {} @ {}\n\n",
        summary.workspace_profile, summary.generated_at
    ));

    // Summary
    out.push_str("## Summary\n\n");
    out.push_str(&format!(
        "- Result: {}\n",
        if summary.passed { "PASSED" } else { "WARNINGS" }
    ));
    out.push_str(&format!("- Profile: `{}`\n", summary.workspace_profile));
    out.push_str(&format!("- Indexed at: `{}`\n", summary.indexed_at));
    out.push_str(&format!("- Files indexed: {}\n", summary.file_count));
    out.push_str(&format!(
        "- Deploy targets: {} | profiles: {} | secret sets: {}\n",
        summary.deploy_target_count, summary.profile_count, summary.secret_set_count
    ));
    out.push_str(&format!(
        "- Doctors run: {} ({} failing)\n",
        summary.doctor_count, summary.failing_doctor_count
    ));
    out.push_str(&format!("- Warnings total: {}\n", summary.warning_count));
    out.push_str(&format!("- Audit timing: {} ms\n\n", summary.timing_ms));

    // Verify
    out.push_str("## Verify\n\n");
    out.push_str(&format!("- {}\n", report.verify.summary));
    out.push_str(&format!("- query_id: `{}`\n", report.verify.query_id));
    out.push_str(&format!(
        "- run_all_doctors timing: {} ms\n\n",
        report.verify.timing_ms
    ));

    if report.doctors.is_empty() {
        out.push_str("_No doctors registered for this profile._\n\n");
    } else {
        out.push_str("| Doctor | Status | Warnings | Evidence | Timing (ms) |\n");
        out.push_str("|---|---|---|---|---|\n");
        for row in &report.doctors {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                row.name,
                if row.passed { "ok" } else { "warn" },
                row.warning_count,
                row.evidence_count,
                row.timing_ms,
            ));
        }
        out.push('\n');
    }

    // Warnings
    out.push_str("## Warnings\n\n");
    if report.warnings.is_empty() {
        out.push_str("_No doctors emitted warnings._\n\n");
    } else {
        for highlight in &report.warnings {
            out.push_str(&format!(
                "### {} ({} warnings)\n\n",
                highlight.name, highlight.warning_count
            ));
            out.push_str(&format!("- {}\n", highlight.summary));
            if !highlight.warning_excerpts.is_empty() {
                out.push_str("\n**Top warnings**\n\n");
                for warning in &highlight.warning_excerpts {
                    out.push_str(&format!("- {}\n", warning));
                }
            }
            if !highlight.evidence_excerpts.is_empty() {
                out.push_str("\n**Evidence excerpts**\n\n");
                for item in &highlight.evidence_excerpts {
                    match item.line {
                        Some(line) => out.push_str(&format!(
                            "- `{}:{}` [{}] {}\n",
                            item.path, line, item.kind, item.detail
                        )),
                        None => out.push_str(&format!(
                            "- `{}` [{}] {}\n",
                            item.path, item.kind, item.detail
                        )),
                    }
                }
            }
            out.push('\n');
        }
    }

    // Capabilities (self-describing)
    out.push_str("## Capabilities\n\n");
    out.push_str(&format!(
        "- Find kinds: {}\n",
        format_kind_list(&report.capabilities.find_kinds)
    ));
    out.push_str(&format!(
        "- Explain kinds: {}\n",
        format_kind_list(&report.capabilities.explain_kinds)
    ));
    out.push_str(&format!(
        "- Graph kinds: {}\n",
        format_kind_list(&report.capabilities.graph_kinds)
    ));
    out.push_str(&format!(
        "- Export kinds: {}\n",
        format_kind_list(&report.capabilities.export_kinds)
    ));
    out.push_str(&format!(
        "- Doctor kinds: {} ({} total)\n",
        format_kind_list(&report.capabilities.doctor_kinds),
        report.capabilities.doctor_kinds.len()
    ));
    if !report.capabilities.notes.is_empty() {
        out.push_str("\n**Workspace notes**\n\n");
        for note in &report.capabilities.notes {
            out.push_str(&format!("- {}\n", note));
        }
    }
    out.push('\n');

    // Extras (skipped or ran)
    if !report.extras.is_empty() {
        out.push_str("## Extras\n\n");
        for extra in &report.extras {
            let label = match extra.status {
                AuditExtraStatus::Skipped => "skipped",
                AuditExtraStatus::Ran => "ran",
            };
            out.push_str(&format!("- `{}` ({}): {}\n", extra.name, label, extra.note));
        }
        out.push('\n');
    }

    // Next steps
    out.push_str("## Next steps\n\n");
    if report.next_steps.is_empty() {
        out.push_str("_No recommended actions._\n");
    } else {
        for step in &report.next_steps {
            out.push_str(&format!("- {}\n", step));
        }
    }

    out
}

fn render_json(report: &AuditReport) -> Result<String> {
    let value = json!({
        "summary": report.summary,
        "verify": report.verify,
        "warnings": report.warnings,
        "doctors": report.doctors,
        "capabilities": report.capabilities,
        "extras": report.extras,
        "next_steps": report.next_steps,
    });
    serde_json::to_string_pretty(&value).context("serialize audit report json")
}

fn format_kind_list(kinds: &[String]) -> String {
    if kinds.is_empty() {
        "_(none)_".to_string()
    } else {
        kinds
            .iter()
            .map(|kind| format!("`{}`", kind))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn now_iso() -> String {
    let now = OffsetDateTime::now_utc();
    let secs = now.unix_timestamp();
    let date = now.date();
    let time = now.time();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z (unix:{})",
        date.year(),
        u8::from(date.month()),
        date.day(),
        time.hour(),
        time.minute(),
        time.second(),
        secs,
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "leio-code-audit-{name}-{}-{}",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".leio-code")).expect("create temp repo");
        fs::write(
            root.join(".leio-code").join("config.toml"),
            "workspace_profile = \"generic\"\n",
        )
        .expect("write config");
        root
    }

    #[test]
    fn audit_markdown_contains_required_sections() {
        let root = temp_root("markdown-sections");
        let index_path = root.join(".leio-code").join("index.json");
        let (markdown, _) = render_audit(&root, &index_path, AuditFormat::Markdown).expect("audit");

        for section in [
            "# Workspace Audit",
            "## Summary",
            "## Verify",
            "## Warnings",
            "## Capabilities",
            "## Next steps",
        ] {
            assert!(
                markdown.contains(section),
                "missing section `{section}` in markdown: {markdown}"
            );
        }
        // Defensive: nothing should look like an unevaluated template literal.
        assert!(
            !markdown.contains("${"),
            "template literal leakage in markdown: {markdown}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn audit_json_exposes_top_level_keys() {
        let root = temp_root("json-keys");
        let index_path = root.join(".leio-code").join("index.json");
        let (payload, _) = render_audit(&root, &index_path, AuditFormat::Json).expect("audit json");

        let value: Value = serde_json::from_str(&payload).expect("audit json parses");
        for key in [
            "summary",
            "verify",
            "warnings",
            "doctors",
            "capabilities",
            "extras",
            "next_steps",
        ] {
            assert!(value.get(key).is_some(), "missing JSON key `{key}`");
        }
        assert!(value["summary"].get("workspace_profile").is_some());
        assert!(value["capabilities"].get("doctor_kinds").is_some());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn audit_report_picks_up_doctor_registry_dynamically() {
        // The audit must not hard-code doctor names. For a leio-code workspace
        // we should still see `self-contract` show up via the runtime registry.
        let root = std::env::temp_dir().join(format!(
            "leio-code-audit-registry-{}-{}",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".leio-code")).expect("create temp repo");
        fs::write(
            root.join(".leio-code").join("config.toml"),
            "workspace_profile = \"leio-code\"\n",
        )
        .expect("write config");

        let index_path = root.join(".leio-code").join("index.json");
        let report = build_audit_report(&root, &index_path).expect("audit report");

        assert_eq!(report.summary.workspace_profile, "leio-code");
        assert!(
            report
                .capabilities
                .doctor_kinds
                .iter()
                .any(|name| name == "self-contract"),
            "leio-code profile should expose self-contract doctor: {:?}",
            report.capabilities.doctor_kinds
        );
        let _ = fs::remove_dir_all(root);
    }
}
