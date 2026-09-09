//! Agent seed update-branch completeness doctor.
//!
//! Locks in the lesson learned from `cartridges/{liz_cobranca,pratique_cobranca}/seed.py`
//! on 2026-05-04: when the cartridge seed already has an existing agent row, the `else`
//! branch was calling `AgentCRUD.update(...)` *without* `provider=` or `model=`, so every
//! API startup re-ran the seed, reported success, but silently dropped the LLM provider
//! and model. The DB drifted to whatever was set at create time (often `gpt-4o-mini` from
//! a prior environment) and stuck.
//!
//! Policy: every `cartridges/*/seed.py` that calls `AgentCRUD.create(...)` AND
//! `AgentCRUD.update(...)` for the same agent must keep the two argument lists symmetric
//! with respect to `provider=` and `model=`. If `create(...)` passes `provider=`, the
//! corresponding `update(...)` must pass `provider=` too. Same rule for `model=`.
//!
//! The doctor is intentionally line-grep-based: it does not attempt to AST-match every
//! Python style. False positives are preferable to silent drift here.

use std::path::{Path, PathBuf};
use std::time::Instant;

use ignore::WalkBuilder;
use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct AgentSeedUpdateCompletenessDoctor;

impl Doctor for AgentSeedUpdateCompletenessDoctor {
    fn name(&self) -> &'static str {
        "agent-seed-update-completeness"
    }

    fn description(&self) -> &'static str {
        "Ensures cartridge seed.py update-branches keep `provider=` / `model=` symmetric with their create-branches, so existing-agent paths don't silently drop LLM fields."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_agent_seed_update_completeness(root)
    }
}

const FIELDS_TO_CHECK: &[&str] = &["provider=", "model="];

pub fn doctor_agent_seed_update_completeness(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();
    let mut scanned_files = 0usize;
    let mut violation_count = 0usize;

    for path in collect_seed_py_files(root) {
        let rel = match path.strip_prefix(root) {
            Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };

        let mut io_warnings: Vec<String> = Vec::new();
        let content = match read_text(&path, &mut io_warnings) {
            Some(c) => c,
            None => {
                warnings.extend(io_warnings);
                continue;
            }
        };
        scanned_files += 1;

        if !content.contains("AgentCRUD.create(") || !content.contains("AgentCRUD.update(") {
            continue;
        }

        // Extract argument-list bodies for each create / update call. Naive: walk lines,
        // when we see `AgentCRUD.create(` (or `.update(`) collect lines until matching
        // depth of zero on parentheses. Good enough for the seed.py style we care about.
        let creates = extract_call_arg_blocks(&content, "AgentCRUD.create(");
        let updates = extract_call_arg_blocks(&content, "AgentCRUD.update(");

        if creates.is_empty() || updates.is_empty() {
            continue;
        }

        // We pair each create with each update in the same file; in practice cartridge
        // seed.py contains one create + one update per agent, so the simple cross-product
        // is fine. If a future seed has multiple agents we still raise on any create that
        // declares `provider=`/`model=` while *no* update in the file does — the operator
        // can split the file or refactor.
        for create_block in &creates {
            for field in FIELDS_TO_CHECK {
                let create_has_field = create_block.body.contains(field);
                if !create_has_field {
                    continue;
                }
                let any_update_has_field = updates
                    .iter()
                    .any(|update_block| update_block.body.contains(field));
                if any_update_has_field {
                    continue;
                }
                violation_count += 1;
                warnings.push(format!(
                    "{}:{}: AgentCRUD.create(...) sets `{}` but no AgentCRUD.update(...) in the same file does — existing-agent path will silently drop this field",
                    rel,
                    create_block.line,
                    field.trim_end_matches('='),
                ));
                evidence.push(EvidenceItem {
                    kind: "agent_seed_field_drift".to_string(),
                    path: rel.clone(),
                    line: Some(create_block.line),
                    detail: format!(
                        "field `{}` is set on AgentCRUD.create at line {} but not on any AgentCRUD.update in this file",
                        field.trim_end_matches('='),
                        create_block.line
                    ),
                });
            }
        }
    }

    entities.push(json!({
        "doctor": "agent-seed-update-completeness",
        "scanned_seed_files": scanned_files,
        "violations": violation_count,
        "fields_checked": FIELDS_TO_CHECK,
    }));

    let summary = if warnings.is_empty() {
        format!(
            "scanned {} cartridge seed.py file(s); create/update field parity holds",
            scanned_files
        )
    } else {
        format!(
            "found {} cartridge seed.py file(s) where AgentCRUD.update is missing fields set on AgentCRUD.create",
            violation_count
        )
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_agent_seed_update_completeness"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.95 } else { 0.6 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

#[derive(Debug)]
struct CallBlock {
    line: usize,
    body: String,
}

fn extract_call_arg_blocks(content: &str, marker: &str) -> Vec<CallBlock> {
    let mut blocks = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    for (idx, line) in lines.iter().enumerate() {
        let Some(start_col) = line.find(marker) else {
            continue;
        };
        // Begin parenthesis-balance walk from immediately after the opening `(`.
        let after = &line[start_col + marker.len()..];
        let mut depth = 1i64;
        let mut buf = String::new();
        // Process remainder of starting line.
        for ch in after.chars() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            buf.push(ch);
        }
        if depth > 0 {
            // Continue across lines until depth reaches zero.
            let mut k = idx + 1;
            while depth > 0 && k < lines.len() {
                let l = lines[k];
                buf.push('\n');
                for ch in l.chars() {
                    match ch {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    buf.push(ch);
                }
                k += 1;
            }
        }
        blocks.push(CallBlock {
            line: idx + 1,
            body: buf,
        });
    }
    blocks
}

fn collect_seed_py_files(root: &Path) -> Vec<PathBuf> {
    let cartridges_root = root.join("cartridges");
    if !cartridges_root.is_dir() {
        return Vec::new();
    }
    let mut builder = WalkBuilder::new(&cartridges_root);
    builder.hidden(false);
    builder.git_ignore(true);
    builder.git_global(true);
    builder.git_exclude(true);
    builder.require_git(false);
    builder.max_depth(Some(2));

    let mut paths = Vec::new();
    for dent in builder.build().flatten() {
        let path = dent.path();
        if !path.is_file() {
            continue;
        }
        if path.file_name().and_then(|n| n.to_str()) == Some("seed.py") {
            paths.push(path.to_path_buf());
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_repo(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-agent-seed-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn symmetric_create_update_yields_no_warnings() {
        let root = temp_repo("symmetric");
        write(
            &root,
            "cartridges/foo/seed.py",
            r#"
def seed_foo_agent():
    if agent is None:
        AgentCRUD.create(
            slug="foo",
            provider="openai",
            model="gpt-5.5",
        )
    else:
        AgentCRUD.update(
            agent.id,
            provider="openai",
            model="gpt-5.5",
        )
"#,
        );
        let env = doctor_agent_seed_update_completeness(&root);
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_provider_in_update_is_flagged() {
        let root = temp_repo("missing-provider");
        write(
            &root,
            "cartridges/bar/seed.py",
            r#"
def seed_bar_agent():
    if agent is None:
        AgentCRUD.create(
            slug="bar",
            provider="openai",
            model="gpt-5.5",
        )
    else:
        AgentCRUD.update(
            agent.id,
            instructions=BAR_INSTRUCTIONS,
        )
"#,
        );
        let env = doctor_agent_seed_update_completeness(&root);
        assert_eq!(env.warnings.len(), 2, "{:?}", env.warnings);
        assert!(env.warnings.iter().any(|w| w.contains("provider")));
        assert!(env.warnings.iter().any(|w| w.contains("model")));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn create_without_provider_is_not_flagged() {
        // If neither create nor update sets provider, the doctor stays silent —
        // we only enforce parity, not the choice itself.
        let root = temp_repo("no-provider");
        write(
            &root,
            "cartridges/baz/seed.py",
            r#"
def seed_baz_agent():
    if agent is None:
        AgentCRUD.create(slug="baz")
    else:
        AgentCRUD.update(agent.id)
"#,
        );
        let env = doctor_agent_seed_update_completeness(&root);
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }
}
