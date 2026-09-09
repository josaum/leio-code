//! Slop gate for deck / report repositories.
//!
//! Direct port of `ppt-novo/scripts/slop-gate.mjs`. Activates only when a
//! `spine.json` file is present at the repository root, so registering the
//! doctor under `PROFILE_GENERIC` does not light up false positives in
//! non-deck repos ("blast radius zero" — see
//! `ppt-novo/docs/leio-code-doctor-integration.md`).
//!
//! Five rules:
//!   D1 placeholder-url   — `via.placeholder`, `placehold.it/co`, `example.com/`,
//!                          `lorem ipsum`, `unsplash.com/random`, bare `placeholder`.
//!   D2 banned-phrases    — EN + PT-BR corporate slop list.
//!   D3 action-titles     — every `spine.story[].title` must contain a digit/
//!                          currency, a verb token, or be ≥5 words.
//!   D4 source-required   — every `spine.story[]` carries non-empty `sources[]`.
//!   D5 density (WARN)    — `>220` word tokens in a node → evidence only, no
//!                          warning, matches the JS gate's exit-code semantics.

use std::path::Path;
use std::time::Instant;

use regex::Regex;
use serde_json::{Value, json};

use super::Doctor;
use super::utils::query_id;
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

const PLACEHOLDER_PATTERNS: &[&str] = &[
    r"(?i)via\.placeholder\.com",
    r"(?i)placehold\.it",
    r"(?i)placehold\.co",
    r"(?i)example\.com/",
    r"(?i)\blorem\s+ipsum\b",
    r"(?i)unsplash\.com/random",
    r"(?i)\bplaceholder\b",
];

const BANNED_PATTERNS: &[&str] = &[
    // EN
    r"(?i)\bleverage\b",
    r"(?i)\brobust\b",
    r"(?i)\bsynerg(y|ies|istic)\b",
    r"(?i)\bbest[- ]in[- ]class\b",
    r"(?i)\bcutting[- ]edge\b",
    r"(?i)\bseamless(ly)?\b",
    r"(?i)\bin today'?s fast[- ]paced\b",
    r"(?i)\bworld[- ]class\b",
    r"(?i)\bgame[- ]chang(er|ing)\b",
    r"(?i)\bunlock\s+(value|potential|growth)\b",
    r"(?i)\bend[- ]to[- ]end\b",
    // PT-BR
    r"(?i)\balavancar\b",
    r"(?i)\bsinergias?\b",
    r"(?i)\brobusto\b",
    r"(?i)\bponta[- ]a[- ]ponta\b",
    r"(?i)\bsoluç(ão|ões)\s+completa",
    r"(?i)\bde\s+ponta\b",
    r"(?i)\bestado da arte\b",
];

const VERB_TOKENS: &[&str] = &[
    // PT
    "vende",
    "consolida",
    "marca",
    "reflete",
    "aponta",
    "aquece",
    "reconfigura",
    "expande",
    "adquire",
    "indicam",
    "aliena",
    "libera",
    "fortalece",
    "demonstra",
    "pode",
    "tem",
    "é",
    "foi",
    "será",
    "cresce",
    "queda",
    "sobe",
    "cai",
    "solidifica",
    "analisar",
    "integrar",
    "extrair",
    "realiza",
    "rotaciona",
    "cumprem",
    // EN
    "grew",
    "grows",
    "launches",
    "ships",
    "wins",
    "loses",
    "drops",
    "beats",
    "closes",
    "announces",
    "expands",
    "acquires",
    "signs",
    "cuts",
    "raises",
    "reduces",
    "adds",
    "reveals",
    "outperforms",
    "underperforms",
    "enables",
    "triggers",
    "fuels",
];

const DIGIT_OR_CURRENCY: &str = r"(\d|R\$|US\$|\$|€|%)";

/// File extensions to scan in addition to spine.json.
const SCANNED_EXTENSIONS: &[&str] = &["tsx", "ts", "jsx", "js", "mjs"];

/// Directory names to skip when walking the tree.
const SKIPPED_DIRS: &[&str] = &["node_modules", "target", ".leio-code", ".git", "scripts"];

/// Repo-relative paths explicitly retained as "legacy / before" references.
/// Mirrors `LEGACY_SKIPS` in `ppt-novo/scripts/slop-gate.mjs`.
const LEGACY_SKIPS: &[&str] = &["Presentation.tsx"];

pub struct SlopGateDoctor;

impl Doctor for SlopGateDoctor {
    fn name(&self) -> &'static str {
        "slop"
    }

    fn description(&self) -> &'static str {
        "Slop gate for deck/report repos with a spine.json. Rejects placeholder \
         URLs, banned corporate phrases (EN+PT-BR), titles without verbs/digits, \
         story nodes missing sources[], and over-dense nodes (WARN-only). \
         Activates only when spine.json is present."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        let started = Instant::now();
        let spine_path = root.join("spine.json");

        if !spine_path.exists() {
            return QueryEnvelope {
                schema_version: crate::model::SCHEMA_VERSION.to_string(),
                query_id: query_id("doctor_slop"),
                kind: "doctor".to_string(),
                summary: "slop: no spine.json at repo root — skipped".to_string(),
                confidence: 0.98,
                entities: vec![],
                evidence: vec![],
                warnings: vec![],
                meta: Some(json!({ "activated": false })),
                timing_ms: started.elapsed().as_millis(),
            };
        }

        let placeholder_res: Vec<Regex> = PLACEHOLDER_PATTERNS
            .iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect();
        let banned_res: Vec<Regex> = BANNED_PATTERNS
            .iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect();

        let mut evidence: Vec<EvidenceItem> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();

        // D1 + D2: regex scan over spine.json text + repo source files.
        let mut scan_targets: Vec<(String, String)> = Vec::new();

        let spine_raw = match std::fs::read_to_string(&spine_path) {
            Ok(s) => s,
            Err(err) => {
                warnings.push(format!("failed to read spine.json: {err}"));
                return finalize(started, evidence, warnings);
            }
        };
        scan_targets.push(("spine.json".to_string(), spine_raw.clone()));

        collect_source_files(root, root, &mut scan_targets);

        for (rel_path, text) in &scan_targets {
            for (line_idx, line) in text.lines().enumerate() {
                for re in &placeholder_res {
                    if re.is_match(line) {
                        evidence.push(EvidenceItem {
                            kind: "placeholder-url".to_string(),
                            path: rel_path.clone(),
                            line: Some(line_idx + 1),
                            detail: format!(
                                "placeholder pattern `{}` — `{}`",
                                re.as_str(),
                                truncate(line.trim(), 160)
                            ),
                        });
                        warnings.push(format!(
                            "{}:{} placeholder URL/string",
                            rel_path,
                            line_idx + 1
                        ));
                    }
                }
                for re in &banned_res {
                    if let Some(m) = re.find(line) {
                        evidence.push(EvidenceItem {
                            kind: "banned-phrases".to_string(),
                            path: rel_path.clone(),
                            line: Some(line_idx + 1),
                            detail: format!(
                                "banned phrase \"{}\" — `{}`",
                                m.as_str(),
                                truncate(line.trim(), 160)
                            ),
                        });
                        warnings.push(format!(
                            "{}:{} banned phrase \"{}\"",
                            rel_path,
                            line_idx + 1,
                            m.as_str()
                        ));
                    }
                }
            }
        }

        // D3, D4, D5: spine-structured rules.
        let spine: Value = match serde_json::from_str(&spine_raw) {
            Ok(v) => v,
            Err(err) => {
                warnings.push(format!("failed to parse spine.json: {err}"));
                return finalize(started, evidence, warnings);
            }
        };

        let story = spine.get("story").and_then(|s| s.as_array());
        let node_count = story.map(|s| s.len()).unwrap_or(0);

        let digit_re = Regex::new(DIGIT_OR_CURRENCY).expect("digit re");
        let verb_res: Vec<Regex> = VERB_TOKENS
            .iter()
            .filter_map(|v| Regex::new(&format!(r"(?i)\b{}\b", regex::escape(v))).ok())
            .collect();

        if let Some(nodes) = story {
            for node in nodes {
                let id = node
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<unknown>");
                let where_ = format!("spine.json#{id}");

                // D3 action-titles
                let title = node.get("title").and_then(|v| v.as_str()).unwrap_or("");
                if !is_action_title(title, &digit_re, &verb_res) {
                    evidence.push(EvidenceItem {
                        kind: "action-titles".to_string(),
                        path: "spine.json".to_string(),
                        line: None,
                        detail: format!(
                            "node `{id}` title is not an assertion (no verb, no digit, <5 words): \"{}\"",
                            title
                        ),
                    });
                    warnings.push(format!("{where_} title not an assertion"));
                }

                // D4 source-required
                let sources_ok = node
                    .get("sources")
                    .and_then(|v| v.as_array())
                    .map(|a| !a.is_empty())
                    .unwrap_or(false);
                if !sources_ok {
                    evidence.push(EvidenceItem {
                        kind: "source-required".to_string(),
                        path: "spine.json".to_string(),
                        line: None,
                        detail: format!("node `{id}` has missing or empty sources[]"),
                    });
                    warnings.push(format!("{where_} sources[] missing or empty"));
                }

                // D5 density (WARN-only — evidence, no warning)
                let flat = node.to_string();
                let word_count = flat
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .filter(|w| !w.is_empty())
                    .count();
                if word_count > 220 {
                    evidence.push(EvidenceItem {
                        kind: "density".to_string(),
                        path: "spine.json".to_string(),
                        line: None,
                        detail: format!(
                            "node `{id}` carries {word_count} word tokens (soft cap 220)"
                        ),
                    });
                    // Intentionally no warnings.push() — density is WARN-level.
                }
            }
        } else {
            warnings.push("spine.json missing `story[]` array".to_string());
        }

        QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_slop"),
            kind: "doctor".to_string(),
            summary: format!(
                "slop: {} fail / {} evidence items across {} spine nodes",
                warnings.len(),
                evidence.len(),
                node_count
            ),
            confidence: if warnings.is_empty() { 0.98 } else { 0.68 },
            entities: vec![json!({
                "spine_path": "spine.json",
                "spine_node_count": node_count,
                "files_scanned": scan_targets.len(),
            })],
            evidence,
            warnings,
            meta: Some(json!({ "activated": true, "spine_node_count": node_count })),
            timing_ms: started.elapsed().as_millis(),
        }
    }
}

fn finalize(started: Instant, evidence: Vec<EvidenceItem>, warnings: Vec<String>) -> QueryEnvelope {
    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_slop"),
        kind: "doctor".to_string(),
        summary: format!("slop: aborted with {} warnings", warnings.len()),
        confidence: 0.5,
        entities: vec![],
        evidence,
        warnings,
        meta: Some(json!({ "activated": true, "aborted": true })),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn is_action_title(title: &str, digit_re: &Regex, verb_res: &[Regex]) -> bool {
    if title.is_empty() {
        return false;
    }
    if digit_re.is_match(title) {
        return true;
    }
    if verb_res.iter().any(|re| re.is_match(title)) {
        return true;
    }
    let words = title.split_whitespace().count();
    words >= 5
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}…")
    }
}

fn collect_source_files(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            if SKIPPED_DIRS.contains(&name) {
                continue;
            }
            collect_source_files(root, &path, out);
        } else if path.is_file() {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !SCANNED_EXTENSIONS.contains(&ext) {
                continue;
            }
            let rel = match path.strip_prefix(root) {
                Ok(r) => r.to_string_lossy().to_string(),
                Err(_) => continue,
            };
            if LEGACY_SKIPS.contains(&rel.as_str()) {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                out.push((rel, text));
            }
        }
    }
}
