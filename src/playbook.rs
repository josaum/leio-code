//! ACE-style evolving context playbook and doctor warning triage counters.
//!
//! Persists deterministic structural signals across sessions at
//! `.leio-code/playbook.json`:
//! - **Co-edit frequency** (`co_edits`): pairs of files modified together in
//!   commits (`git log --name-only`), capped at [`MAX_PLAYBOOK_ENTRIES`].
//! - **Symbol / env / Redis query frequency** (`query_counts`): hot needles
//!   queried via `find` / `explain`.
//! - **Session outcome signals** (`file_outcomes`): `helpfulness` and
//!   `harmfulness` counters per file.
//! - **Doctor warning triage counters** (`doctor_triage`, `doctor_warnings`):
//!   `fired_count`, `resolved_count`, and `ignored_count` (persisting across
//!   3+ commits) to surface noisy vs. high-signal doctors (`doctor triage`).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::doctors::utils::query_id;
use crate::model::{EvidenceItem, QueryEnvelope, SCHEMA_VERSION};

pub const PLAYBOOK_VERSION: u32 = 1;
pub const MAX_PLAYBOOK_ENTRIES: usize = 10_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playbook {
    pub version: u32,
    #[serde(default)]
    pub last_git_head: Option<String>,
    #[serde(default)]
    pub co_edits: BTreeMap<String, BTreeMap<String, u32>>,
    #[serde(default)]
    pub query_counts: BTreeMap<String, u32>,
    #[serde(default)]
    pub file_outcomes: BTreeMap<String, FileOutcomeCounter>,
    #[serde(default)]
    pub doctor_triage: BTreeMap<String, DoctorTriageEntry>,
    #[serde(default)]
    pub doctor_warnings: BTreeMap<String, WarningTriageEntry>,
}

impl Default for Playbook {
    fn default() -> Self {
        Self {
            version: PLAYBOOK_VERSION,
            last_git_head: None,
            co_edits: BTreeMap::new(),
            query_counts: BTreeMap::new(),
            file_outcomes: BTreeMap::new(),
            doctor_triage: BTreeMap::new(),
            doctor_warnings: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileOutcomeCounter {
    pub helpfulness: u32,
    pub harmfulness: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DoctorTriageEntry {
    pub doctor: String,
    pub fired_count: u32,
    pub resolved_count: u32,
    pub ignored_count: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WarningTriageEntry {
    pub doctor: String,
    pub warning: String,
    pub fired_count: u32,
    pub resolved_count: u32,
    pub ignored_count: u32,
    pub streak_commits: u32,
    pub last_seen_commit: Option<String>,
    pub active_in_last_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorTriageRow {
    pub doctor: String,
    pub fired_count: u32,
    pub resolved_count: u32,
    pub ignored_count: u32,
    pub ignored_ratio: f64,
    pub resolved_ratio: f64,
    pub status: &'static str,
}

pub fn default_playbook_path(root: &Path) -> PathBuf {
    root.join(".leio-code").join("playbook.json")
}

pub fn load_playbook(root: &Path) -> Option<Playbook> {
    let path = default_playbook_path(root);
    let raw = fs::read(&path).ok()?;
    let pb: Playbook = serde_json::from_slice(&raw).ok()?;
    (pb.version == PLAYBOOK_VERSION).then_some(pb)
}

pub fn load_or_default(root: &Path) -> Playbook {
    load_playbook(root).unwrap_or_default()
}

/// Load `.leio-code/playbook.json`, automatically syncing git co-edits when `.leio-code`
/// exists and `last_git_head` is missing or behind `HEAD`.
pub fn load_or_auto_sync(root: &Path) -> Option<Playbook> {
    if !root.join(".leio-code").is_dir() {
        return load_playbook(root);
    }
    let existing = load_playbook(root);
    let Some(head) = current_git_head(root) else {
        return existing;
    };
    if let Some(ref pb) = existing
        && pb.last_git_head.as_deref() == Some(head.as_str())
    {
        return existing;
    }
    let Ok(_lock) = crate::sidecar::acquire_lock(root, "playbook") else {
        return existing;
    };
    let mut pb = load_or_default(root);
    if pb.last_git_head.as_deref() != Some(head.as_str()) {
        sync_git_co_edits(root, &mut pb);
        let _ = save_playbook(root, &pb);
    }
    Some(pb)
}

pub fn save_playbook(root: &Path, playbook: &Playbook) -> Result<()> {
    let path = default_playbook_path(root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    crate::sidecar::write_atomic_json(&path, playbook)
        .with_context(|| format!("failed to write {}", path.display()))
}

fn current_git_head(root: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

/// Mine co-edit pairs from `git log --name-only` incrementally and cap entries.
pub fn sync_git_co_edits(root: &Path, playbook: &mut Playbook) -> usize {
    let Some(head) = current_git_head(root) else {
        return 0;
    };
    if playbook.last_git_head.as_deref() == Some(head.as_str()) {
        return 0;
    }

    let is_incremental_delta = playbook.last_git_head.is_some();
    let range_arg = playbook
        .last_git_head
        .as_ref()
        .map(|prev| format!("{prev}..{head}"));
    let mut cmd = Command::new("git");
    cmd.current_dir(root).args([
        "log",
        "-n",
        "100",
        "--name-only",
        "--pretty=format:COMMIT:%H:%s",
    ]);
    if let Some(range) = &range_arg {
        cmd.arg(range);
    }

    let output = match cmd.output() {
        Ok(out) if out.status.success() => out,
        _ => {
            // Fallback if previous HEAD was rebased away.
            let Ok(out) = Command::new("git")
                .current_dir(root)
                .args(["log", "-n", "50", "--name-only", "--pretty=format:COMMIT:%H:%s"])
                .output()
            else {
                return 0;
            };
            if !out.status.success() {
                return 0;
            }
            out
        }
    };

    let text = String::from_utf8_lossy(&output.stdout);
    let commits = parse_commits_with_subjects(&text);
    let mut added_pairs = 0usize;
    for commit in commits {
        let files = commit.files;
        let lower_subj = commit.subject.trim().to_ascii_lowercase();
        let is_revert = lower_subj.starts_with("revert ") || lower_subj.starts_with("revert:");
        if is_revert && files.len() <= 20 {
            for path in &files {
                let outcome = playbook.file_outcomes.entry(path.clone()).or_default();
                outcome.harmfulness = outcome.harmfulness.saturating_add(1);
            }
        } else if is_incremental_delta && !files.is_empty() && files.len() <= 20 {
            for path in &files {
                let outcome = playbook.file_outcomes.entry(path.clone()).or_default();
                outcome.helpfulness = outcome.helpfulness.saturating_add(1);
            }
        }

        // Ignore single-file commits and huge tree-wide sweeps (>20 files).
        if files.len() < 2 || files.len() > 20 {
            continue;
        }
        for i in 0..files.len() {
            for j in (i + 1)..files.len() {
                let a = &files[i];
                let b = &files[j];
                *playbook
                    .co_edits
                    .entry(a.clone())
                    .or_default()
                    .entry(b.clone())
                    .or_insert(0) += 1;
                *playbook
                    .co_edits
                    .entry(b.clone())
                    .or_default()
                    .entry(a.clone())
                    .or_insert(0) += 1;
                added_pairs += 1;
            }
        }
    }

    evict_co_edits(&mut playbook.co_edits, MAX_PLAYBOOK_ENTRIES);
    playbook.last_git_head = Some(head);
    added_pairs
}

#[derive(Debug, Clone)]
pub struct ParsedCommit {
    pub subject: String,
    pub files: Vec<String>,
}

pub fn parse_commits_with_subjects(log_text: &str) -> Vec<ParsedCommit> {
    let mut out = Vec::new();
    let mut current_subject = String::new();
    let mut current_files = Vec::new();
    let mut in_commit = false;
    for raw_line in log_text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("COMMIT:") {
            if in_commit && !current_files.is_empty() {
                current_files.sort();
                current_files.dedup();
                out.push(ParsedCommit {
                    subject: std::mem::take(&mut current_subject),
                    files: std::mem::take(&mut current_files),
                });
            } else {
                current_files.clear();
            }
            current_subject = rest
                .split_once(':')
                .map(|(_, s)| s.to_string())
                .unwrap_or_default();
            in_commit = true;
            continue;
        }
        current_files.push(line.to_string());
    }
    if in_commit && !current_files.is_empty() {
        current_files.sort();
        current_files.dedup();
        out.push(ParsedCommit {
            subject: current_subject,
            files: current_files,
        });
    }
    out
}

pub fn parse_commit_file_sets(log_text: &str) -> Vec<Vec<String>> {
    parse_commits_with_subjects(log_text)
        .into_iter()
        .map(|c| c.files)
        .collect()
}

fn evict_co_edits(
    co_edits: &mut BTreeMap<String, BTreeMap<String, u32>>,
    max_entries: usize,
) {
    let total: usize = co_edits.values().map(BTreeMap::len).sum();
    if total <= max_entries {
        return;
    }
    let mut flat: Vec<(u32, String, String)> = Vec::with_capacity(total);
    for (src, neighbors) in co_edits.iter() {
        for (dst, count) in neighbors {
            flat.push((*count, src.clone(), dst.clone()));
        }
    }
    flat.sort();
    let to_remove = total.saturating_sub(max_entries);
    for (_, src, dst) in flat.into_iter().take(to_remove) {
        if let Some(neighbors) = co_edits.get_mut(&src) {
            neighbors.remove(&dst);
            if neighbors.is_empty() {
                co_edits.remove(&src);
            }
        }
    }
}

fn evict_query_counts(counts: &mut BTreeMap<String, u32>, max_entries: usize) {
    if counts.len() <= max_entries {
        return;
    }
    let mut items: Vec<(u32, String)> = counts.iter().map(|(k, v)| (*v, k.clone())).collect();
    items.sort();
    let to_remove = counts.len().saturating_sub(max_entries);
    for (_, key) in items.into_iter().take(to_remove) {
        counts.remove(&key);
    }
}

/// Record a symbol / env-var / Redis-key lookup in `.leio-code/playbook.json` when `.leio-code` exists.
pub fn record_query_best_effort(root: &Path, needle: &str) {
    let trimmed = needle.trim();
    if trimmed.is_empty() || !root.join(".leio-code").is_dir() {
        return;
    }
    let Ok(_lock) = crate::sidecar::acquire_lock(root, "playbook") else {
        return;
    };
    let mut pb = load_or_default(root);
    *pb.query_counts.entry(trimmed.to_string()).or_insert(0) += 1;
    evict_query_counts(&mut pb.query_counts, MAX_PLAYBOOK_ENTRIES);
    let _ = save_playbook(root, &pb);
}

/// Record session outcome (`helpful = true` bumps `helpfulness`, `false` bumps `harmfulness`).
pub fn record_session_outcome(root: &Path, paths: &[String], helpful: bool) -> Result<()> {
    let _lock = crate::sidecar::acquire_lock(root, "playbook")?;
    let mut pb = load_or_default(root);
    for path in paths {
        let entry = pb.file_outcomes.entry(path.clone()).or_default();
        if helpful {
            entry.helpfulness = entry.helpfulness.saturating_add(1);
        } else {
            entry.harmfulness = entry.harmfulness.saturating_add(1);
        }
    }
    save_playbook(root, &pb)
}

/// Record explicit helpful and harmful file outcomes and return a `playbook_outcome` envelope.
pub fn record_session_outcomes_envelope(
    root: &Path,
    helpful: &[String],
    harmful: &[String],
) -> Result<QueryEnvelope> {
    let started = Instant::now();
    let _lock = crate::sidecar::acquire_lock(root, "playbook")?;
    let mut pb = load_or_default(root);
    let mut updated = Vec::new();
    for path in helpful {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            continue;
        }
        let entry = pb.file_outcomes.entry(trimmed.to_string()).or_default();
        entry.helpfulness = entry.helpfulness.saturating_add(1);
        updated.push(json!({
            "path": trimmed,
            "outcome": "helpful",
            "helpfulness": entry.helpfulness,
            "harmfulness": entry.harmfulness,
        }));
    }
    for path in harmful {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            continue;
        }
        let entry = pb.file_outcomes.entry(trimmed.to_string()).or_default();
        entry.harmfulness = entry.harmfulness.saturating_add(1);
        updated.push(json!({
            "path": trimmed,
            "outcome": "harmful",
            "helpfulness": entry.helpfulness,
            "harmfulness": entry.harmfulness,
        }));
    }
    save_playbook(root, &pb)?;
    Ok(QueryEnvelope {
        schema_version: SCHEMA_VERSION.to_string(),
        query_id: query_id("playbook_outcome"),
        kind: "playbook_outcome".to_string(),
        summary: format!(
            "playbook outcome: recorded {} helpful and {} harmful file signal(s)",
            helpful.len(),
            harmful.len()
        ),
        confidence: 0.98,
        entities: updated,
        evidence: Vec::new(),
        warnings: Vec::new(),
        meta: Some(json!({
            "playbook_path": default_playbook_path(root).display().to_string(),
            "helpful_count": helpful.len(),
            "harmful_count": harmful.len(),
        })),
        timing_ms: started.elapsed().as_millis(),
    })
}

/// Return `(priority_delta, triage_status)` for ranking doctor suggestions in `context`.
/// High-signal doctors (`resolved/fired > 0.9`) get `+10`; noisy doctors (`ignored/fired > 0.8`)
/// get `-10`.
pub fn doctor_triage_priority(pb: &Playbook, doctor_name: &str) -> (i32, Option<&'static str>) {
    let Some(d) = pb.doctor_triage.get(doctor_name) else {
        return (0, None);
    };
    if d.fired_count == 0 {
        return (0, None);
    }
    let denom = f64::from(d.fired_count);
    let ignored_ratio = (f64::from(d.ignored_count) / denom).clamp(0.0, 1.0);
    let resolved_ratio = (f64::from(d.resolved_count) / denom).clamp(0.0, 1.0);
    if ignored_ratio > 0.8 {
        (-10, Some("candidate_for_demotion"))
    } else if resolved_ratio > 0.9 {
        (10, Some("high_signal"))
    } else {
        (0, Some("normal"))
    }
}

/// Update doctor warning triage counters after a doctor or suite run.
pub fn record_doctor_observations_best_effort(
    root: &Path,
    rows: &[(String, QueryEnvelope)],
) {
    if !root.join(".leio-code").is_dir() {
        return;
    }
    let Ok(_lock) = crate::sidecar::acquire_lock(root, "playbook") else {
        return;
    };
    let head = current_git_head(root);
    let mut pb = load_or_default(root);
    record_doctor_observations(&mut pb, rows, head.as_deref());
    let _ = save_playbook(root, &pb);
}

pub fn record_doctor_observations(
    pb: &mut Playbook,
    rows: &[(String, QueryEnvelope)],
    commit_head: Option<&str>,
) {
    for (doctor_name, envelope) in rows {
        let current_set: std::collections::BTreeSet<String> =
            envelope.warnings.iter().cloned().collect();

        // Check previously active warnings for this doctor that disappeared.
        let mut newly_resolved = 0u32;
        for entry in pb
            .doctor_warnings
            .values_mut()
            .filter(|w| w.doctor == *doctor_name && w.active_in_last_run)
        {
            if !current_set.contains(&entry.warning) {
                entry.resolved_count = entry.resolved_count.saturating_add(1);
                entry.active_in_last_run = false;
                entry.streak_commits = 0;
                newly_resolved = newly_resolved.saturating_add(1);
            }
        }

        let mut newly_fired = 0u32;
        let mut newly_ignored = 0u32;
        for warning in &current_set {
            let key = format!("[{doctor_name}] {warning}");
            let entry = pb
                .doctor_warnings
                .entry(key)
                .or_insert_with(|| WarningTriageEntry {
                    doctor: doctor_name.clone(),
                    warning: warning.clone(),
                    ..Default::default()
                });
            entry.fired_count = entry.fired_count.saturating_add(1);
            newly_fired = newly_fired.saturating_add(1);
            entry.active_in_last_run = true;

            match (commit_head, entry.last_seen_commit.as_deref()) {
                (Some(cur), Some(prev)) if cur != prev => {
                    entry.streak_commits = entry.streak_commits.saturating_add(1);
                    entry.last_seen_commit = Some(cur.to_string());
                }
                (Some(cur), None) => {
                    entry.streak_commits = 1;
                    entry.last_seen_commit = Some(cur.to_string());
                }
                (None, _) => {
                    entry.streak_commits = entry.streak_commits.saturating_add(1);
                }
                _ => {}
            }
            if entry.streak_commits >= 3 {
                entry.ignored_count = entry.ignored_count.saturating_add(1);
                newly_ignored = newly_ignored.saturating_add(1);
            }
        }

        let doc_entry = pb
            .doctor_triage
            .entry(doctor_name.clone())
            .or_insert_with(|| DoctorTriageEntry {
                doctor: doctor_name.clone(),
                ..Default::default()
            });
        doc_entry.fired_count = doc_entry.fired_count.saturating_add(newly_fired);
        doc_entry.resolved_count = doc_entry.resolved_count.saturating_add(newly_resolved);
        doc_entry.ignored_count = doc_entry.ignored_count.saturating_add(newly_ignored);
    }
}

/// Compute ranked doctor triage rows (sorted by `ignored / fired` descending).
pub fn ranked_doctor_triage(pb: &Playbook) -> Vec<DoctorTriageRow> {
    let mut rows: Vec<DoctorTriageRow> = pb
        .doctor_triage
        .values()
        .filter(|d| d.fired_count > 0 || d.resolved_count > 0 || d.ignored_count > 0)
        .map(|d| {
            let denom = f64::from(d.fired_count.max(1));
            let ignored_ratio = (f64::from(d.ignored_count) / denom).clamp(0.0, 1.0);
            let resolved_ratio = (f64::from(d.resolved_count) / denom).clamp(0.0, 1.0);
            let status = if ignored_ratio > 0.8 {
                "candidate_for_demotion"
            } else if resolved_ratio > 0.9 {
                "high_signal"
            } else {
                "normal"
            };
            DoctorTriageRow {
                doctor: d.doctor.clone(),
                fired_count: d.fired_count,
                resolved_count: d.resolved_count,
                ignored_count: d.ignored_count,
                ignored_ratio,
                resolved_ratio,
                status,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.ignored_ratio
            .partial_cmp(&a.ignored_ratio)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.fired_count.cmp(&a.fired_count))
            .then_with(|| a.doctor.cmp(&b.doctor))
    });
    rows
}

/// Build the `leio-code doctor triage` envelope.
pub fn doctor_triage_envelope(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let pb = load_or_default(root);
    let rows = ranked_doctor_triage(&pb);
    let demote_count = rows
        .iter()
        .filter(|r| r.status == "candidate_for_demotion")
        .count();
    let high_signal_count = rows.iter().filter(|r| r.status == "high_signal").count();
    let evidence: Vec<EvidenceItem> = rows
        .iter()
        .map(|r| EvidenceItem {
            kind: "doctor_triage".to_string(),
            path: ".leio-code/playbook.json".to_string(),
            line: None,
            detail: format!(
                "{}: fired={} resolved={} ignored={} (ignored/fired={:.2}, resolved/fired={:.2}) [{}]",
                r.doctor,
                r.fired_count,
                r.resolved_count,
                r.ignored_count,
                r.ignored_ratio,
                r.resolved_ratio,
                r.status
            ),
        })
        .collect();
    QueryEnvelope {
        schema_version: SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_triage"),
        kind: "doctor_triage".to_string(),
        summary: format!(
            "doctor triage: {} tracked doctor(s), {} noisy (ignored/fired > 0.8), {} high-signal (resolved/fired > 0.9)",
            rows.len(),
            demote_count,
            high_signal_count
        ),
        confidence: 0.95,
        entities: rows
            .iter()
            .map(|r| serde_json::to_value(r).unwrap_or_else(|_| json!({})))
            .collect(),
        evidence,
        warnings: Vec::new(),
        meta: Some(json!({
            "playbook_path": default_playbook_path(root).display().to_string(),
            "tracked_doctors": rows.len(),
            "TrackedWarnings": pb.doctor_warnings.len(),
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// Build the `leio-code playbook stats` (or `sync`) envelope.
pub fn playbook_stats_envelope(root: &Path, sync_first: bool) -> Result<QueryEnvelope> {
    let started = Instant::now();
    let mut pb = load_or_default(root);
    let mut mined_pairs = 0usize;
    if sync_first {
        let _lock = crate::sidecar::acquire_lock(root, "playbook")?;
        pb = load_or_default(root);
        mined_pairs = sync_git_co_edits(root, &mut pb);
        save_playbook(root, &pb)?;
    }

    let mut top_co_edits: Vec<(u32, String, String)> = Vec::new();
    for (src, neighbors) in &pb.co_edits {
        for (dst, count) in neighbors {
            if src < dst {
                top_co_edits.push((*count, src.clone(), dst.clone()));
            }
        }
    }
    top_co_edits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)).then_with(|| a.2.cmp(&b.2)));
    let top_co_edits_json: Vec<Value> = top_co_edits
        .iter()
        .take(15)
        .map(|(count, a, b)| json!({ "file_a": a, "file_b": b, "co_edit_count": count }))
        .collect();

    let mut hot_symbols: Vec<(u32, String)> = pb
        .query_counts
        .iter()
        .map(|(k, v)| (*v, k.clone()))
        .collect();
    hot_symbols.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let hot_symbols_json: Vec<Value> = hot_symbols
        .iter()
        .take(15)
        .map(|(count, name)| json!({ "needle": name, "query_count": count }))
        .collect();

    let triage = ranked_doctor_triage(&pb);

    Ok(QueryEnvelope {
        schema_version: SCHEMA_VERSION.to_string(),
        query_id: query_id("playbook_stats"),
        kind: "playbook_stats".to_string(),
        summary: format!(
            "playbook stats: {} co-edit pairs, {} queried needles, {} triaged doctors{}",
            top_co_edits.len(),
            pb.query_counts.len(),
            triage.len(),
            if sync_first {
                format!(" (synced {mined_pairs} co-edit pair updates from git log)")
            } else {
                String::new()
            }
        ),
        confidence: 0.96,
        entities: vec![json!({
            "version": pb.version,
            "last_git_head": pb.last_git_head,
            "co_edit_pair_count": top_co_edits.len(),
            "top_co_edits": top_co_edits_json,
            "queried_needle_count": pb.query_counts.len(),
            "hottest_queries": hot_symbols_json,
            "file_outcome_count": pb.file_outcomes.len(),
            "doctor_triage": triage,
        })],
        evidence: top_co_edits
            .iter()
            .take(10)
            .map(|(count, a, b)| EvidenceItem {
                kind: "co_edit_pair".to_string(),
                path: a.clone(),
                line: None,
                detail: format!("co-edited with `{b}` in {count} commit(s)"),
            })
            .collect(),
        warnings: Vec::new(),
        meta: Some(json!({
            "playbook_path": default_playbook_path(root).display().to_string(),
            "synced": sync_first,
        })),
        timing_ms: started.elapsed().as_millis(),
    })
}

/// Co-edit and outcome boost for a candidate file given top seed paths.
pub fn file_playbook_boost(pb: &Playbook, file_path: &str, seed_paths: &[String]) -> (i64, Option<String>) {
    let mut co_edit_bonus = 0i64;
    let mut best_seed: Option<(&str, u32)> = None;
    for seed in seed_paths {
        if seed == file_path {
            continue;
        }
        if let Some(count) = pb.co_edits.get(seed).and_then(|m| m.get(file_path)).copied()
            && count > 0
        {
            let bonus = i64::from(count.min(6)) * 4;
            co_edit_bonus += bonus;
            if best_seed.is_none_or(|(_, best)| count > best) {
                best_seed = Some((seed.as_str(), count));
            }
        }
    }
    let outcome_delta = pb
        .file_outcomes
        .get(file_path)
        .map(|o| (i64::from(o.helpfulness) - i64::from(o.harmfulness)).clamp(-5, 5) * 3)
        .unwrap_or(0);
    let total = co_edit_bonus.min(24) + outcome_delta;
    let reason = best_seed.map(|(seed, count)| format!("playbook co-edit with `{seed}` ({count}x)"));
    (total, reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_git_log_and_evicts_at_cap() {
        let sample = "\
COMMIT:1111
src/a.rs
src/b.rs

COMMIT:2222
src/a.rs
src/b.rs
src/c.rs
";
        let sets = parse_commit_file_sets(sample);
        assert_eq!(sets.len(), 2);
        let mut pb = Playbook::default();
        for files in sets {
            for i in 0..files.len() {
                for j in (i + 1)..files.len() {
                    let a = &files[i];
                    let b = &files[j];
                    *pb.co_edits.entry(a.clone()).or_default().entry(b.clone()).or_insert(0) += 1;
                    *pb.co_edits.entry(b.clone()).or_default().entry(a.clone()).or_insert(0) += 1;
                }
            }
        }
        assert_eq!(pb.co_edits["src/a.rs"]["src/b.rs"], 2);
        assert_eq!(pb.co_edits["src/a.rs"]["src/c.rs"], 1);
        evict_co_edits(&mut pb.co_edits, 2);
        let total: usize = pb.co_edits.values().map(BTreeMap::len).sum();
        assert!(total <= 2);
    }

    #[test]
    fn doctor_triage_tracks_fired_resolved_and_ignored() {
        let mut pb = Playbook::default();
        let make_env = |warnings: Vec<&str>| QueryEnvelope {
            schema_version: SCHEMA_VERSION.to_string(),
            query_id: "q".to_string(),
            kind: "doctor".to_string(),
            summary: String::new(),
            confidence: 1.0,
            entities: Vec::new(),
            evidence: Vec::new(),
            warnings: warnings.into_iter().map(str::to_owned).collect(),
            meta: None,
            timing_ms: 0,
        };

        // 3 consecutive commits with the same warning on noisy-doc -> ignored_count increments.
        record_doctor_observations(
            &mut pb,
            &[("noisy-doc".to_string(), make_env(vec!["w1"]))],
            Some("c1"),
        );
        record_doctor_observations(
            &mut pb,
            &[("noisy-doc".to_string(), make_env(vec!["w1"]))],
            Some("c2"),
        );
        record_doctor_observations(
            &mut pb,
            &[("noisy-doc".to_string(), make_env(vec!["w1"]))],
            Some("c3"),
        );

        // High-signal doctor fires once on c1, then resolves on c2.
        record_doctor_observations(
            &mut pb,
            &[("clean-doc".to_string(), make_env(vec!["fix-me"]))],
            Some("c1"),
        );
        record_doctor_observations(
            &mut pb,
            &[("clean-doc".to_string(), make_env(vec![]))],
            Some("c2"),
        );

        let ranked = ranked_doctor_triage(&pb);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].doctor, "noisy-doc");
        assert_eq!(ranked[0].fired_count, 3);
        assert_eq!(ranked[0].ignored_count, 1);
        assert_eq!(ranked[1].doctor, "clean-doc");
        assert_eq!(ranked[1].fired_count, 1);
        assert_eq!(ranked[1].resolved_count, 1);
        assert_eq!(ranked[1].status, "high_signal");
        assert_eq!(
            doctor_triage_priority(&pb, "clean-doc"),
            (10, Some("high_signal"))
        );
    }

    #[test]
    fn parses_commit_subjects_for_revert_detection() {
        let sample = "\
COMMIT:aaaa:Revert \"bad change\"
src/bad.rs
src/other.rs

COMMIT:bbbb:feat: add clean helper
src/good.rs
";
        let commits = parse_commits_with_subjects(sample);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].subject, "Revert \"bad change\"");
        assert_eq!(commits[0].files, vec!["src/bad.rs", "src/other.rs"]);
        assert_eq!(commits[1].subject, "feat: add clean helper");
        assert_eq!(commits[1].files, vec!["src/good.rs"]);
    }
}
