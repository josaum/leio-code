//! Worker mem-budget doctor.
//!
//! On 2026-05-04 `example-api/docker-compose.yml` had `worker-pratique` at `mem_limit: 768m`
//! while `worker-liz` was at `1536m`. The cartridge `pratique_cobranca` imports `rdflib`
//! through `agent.py` and `ontology_loader.py` (heavy graph dep) — under that memory
//! ceiling, any non-trivial poison message could trigger an OOM kill that masquerades as
//! a stuck task. The fix was to bring the two parity-class workers to the same budget.
//!
//! Heuristic: if a celery worker's `command:` line in `example-api/docker-compose.yml`
//! references a cartridge whose `tools.py`, `agent.py`, or `ontology_loader.py` imports
//! a known-heavy dependency (`rdflib`, `torch`, `whisper`, `librosa`, `numpy`,
//! `scipy`, `transformers`), then that worker's `mem_limit` should be at least 1024m.
//!
//! False positives are acceptable; the doctor errs on the side of "tell the operator
//! to look at it before deploy."

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use ignore::WalkBuilder;
use regex::Regex;
use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct WorkerMemBudgetDoctor;

impl Doctor for WorkerMemBudgetDoctor {
    fn name(&self) -> &'static str {
        "worker-mem-budget"
    }

    fn description(&self) -> &'static str {
        "Flags celery worker services in example-api/docker-compose.yml whose cartridge imports a heavy dependency (rdflib/torch/etc.) but whose mem_limit is below the safe 1024m threshold."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_worker_mem_budget(root)
    }
}

const HEAVY_IMPORTS: &[&str] = &[
    "import rdflib",
    "from rdflib",
    "import torch",
    "from torch",
    "import whisper",
    "import librosa",
    "from librosa",
    "import transformers",
    "from transformers",
    "import scipy",
    "from scipy",
];

const SAFE_MIN_MEM_MB: u64 = 1024;

pub fn doctor_worker_mem_budget(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    // 1. Determine which cartridges import a heavy dep.
    let heavy_carts = scan_heavy_cartridges(root);

    // 2. Parse docker-compose.yml: collect (service_name, command_line, mem_limit_mb).
    let compose_path = root.join("example-api/docker-compose.yml");
    if !compose_path.is_file() {
        return finalize(
            started,
            warnings,
            entities,
            evidence,
            "example-api/docker-compose.yml not found; skipped",
        );
    }
    let mut io_warnings = Vec::new();
    let body = match read_text(&compose_path, &mut io_warnings) {
        Some(b) => b,
        None => {
            warnings.extend(io_warnings);
            return finalize(
                started,
                warnings,
                entities,
                evidence,
                "could not read docker-compose.yml",
            );
        }
    };

    let services = parse_worker_services(&body);

    let mut violation_count = 0usize;
    for svc in &services {
        // Find which cartridge(s) this worker references (via the `command:` line, which
        // contains queue names like `agent_pratique_pratique_cobranca_queue`).
        let mut matched_carts = Vec::new();
        for cart in &heavy_carts {
            if svc.command.contains(cart.as_str()) {
                matched_carts.push(cart.clone());
            }
        }
        // If the worker's name itself ends with a substring matching a heavy cartridge
        // (e.g. `worker-pratique` -> `pratique_cobranca`), include that too.
        if matched_carts.is_empty() {
            let worker_suffix = svc.name.trim_start_matches("worker-");
            for cart in &heavy_carts {
                if cart.starts_with(worker_suffix) || worker_suffix.starts_with(cart.as_str()) {
                    matched_carts.push(cart.clone());
                }
            }
        }
        if matched_carts.is_empty() {
            continue;
        }
        let Some(mem_mb) = svc.mem_limit_mb else {
            continue;
        };
        if mem_mb >= SAFE_MIN_MEM_MB {
            continue;
        }
        violation_count += 1;
        warnings.push(format!(
            "example-api/docker-compose.yml:{}: worker `{}` has mem_limit={}m (< {}m) but references heavy-import cartridge(s): {}",
            svc.mem_limit_line.unwrap_or(svc.name_line),
            svc.name,
            mem_mb,
            SAFE_MIN_MEM_MB,
            matched_carts.join(", ")
        ));
        evidence.push(EvidenceItem {
            kind: "worker_mem_budget_low".to_string(),
            path: "example-api/docker-compose.yml".to_string(),
            line: svc.mem_limit_line.or(Some(svc.name_line)),
            detail: format!(
                "worker `{}` mem_limit={}m, heavy cartridges: {}",
                svc.name,
                mem_mb,
                matched_carts.join(", ")
            ),
        });
    }

    entities.push(json!({
        "doctor": "worker-mem-budget",
        "heavy_cartridges": heavy_carts,
        "worker_services_scanned": services.len(),
        "violations": violation_count,
        "safe_min_mem_mb": SAFE_MIN_MEM_MB,
    }));

    let summary = if warnings.is_empty() {
        format!(
            "all {} worker service(s) tied to heavy-import cartridges meet the {}m floor",
            services.len(),
            SAFE_MIN_MEM_MB
        )
    } else {
        format!(
            "{} worker service(s) under the {}m mem_limit floor for heavy-import cartridges",
            violation_count, SAFE_MIN_MEM_MB
        )
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_worker_mem_budget"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.92 } else { 0.55 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn finalize(
    started: Instant,
    warnings: Vec<String>,
    entities: Vec<serde_json::Value>,
    evidence: Vec<EvidenceItem>,
    summary: &str,
) -> QueryEnvelope {
    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_worker_mem_budget"),
        kind: "doctor".to_string(),
        summary: summary.to_string(),
        confidence: 0.9,
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn scan_heavy_cartridges(root: &Path) -> Vec<String> {
    let cartridges_root = root.join("cartridges");
    if !cartridges_root.is_dir() {
        return Vec::new();
    }
    let mut found: BTreeMap<String, ()> = BTreeMap::new();
    let mut builder = WalkBuilder::new(&cartridges_root);
    builder.hidden(false);
    builder.git_ignore(true);
    builder.require_git(false);
    builder.max_depth(Some(3));
    let interesting_files = ["tools.py", "agent.py", "ontology_loader.py"];
    for dent in builder.build().flatten() {
        let path = dent.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !interesting_files.contains(&name) {
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let cart_name = match rel_str.split('/').nth(1) {
            Some(c) => c.to_string(),
            None => continue,
        };
        let mut io = Vec::new();
        if let Some(body) = read_text(path, &mut io)
            && HEAVY_IMPORTS.iter().any(|pat| body.contains(pat))
        {
            found.insert(cart_name, ());
        }
    }
    found.into_keys().collect()
}

#[derive(Debug, Default)]
struct WorkerService {
    name: String,
    name_line: usize,
    command: String,
    mem_limit_mb: Option<u64>,
    mem_limit_line: Option<usize>,
}

fn parse_worker_services(body: &str) -> Vec<WorkerService> {
    // Match top-level service entries that start with two-space indent: `^  worker-NAME:`
    let header_re = Regex::new(r"(?m)^  (worker-[a-zA-Z0-9_-]+):\s*$").expect("valid regex");
    // Within a block (until next top-level service header), grab `command:` and `mem_limit:`.
    let command_re = Regex::new(r"(?m)^    command:\s*(.+)$").expect("valid regex");
    let mem_re = Regex::new(r"(?m)^    mem_limit:\s*(\d+)([gGmM])\s*$").expect("valid regex");

    // Collect (line_no_in_lines, name) pairs.
    let mut headers: Vec<(usize, String)> = Vec::new();
    for caps in header_re.captures_iter(body) {
        let mat = caps.get(0).expect("capture present");
        let line = body[..mat.start()].chars().filter(|c| *c == '\n').count() + 1;
        headers.push((line, caps.get(1).unwrap().as_str().to_string()));
    }

    let lines: Vec<&str> = body.lines().collect();
    let mut services = Vec::new();
    for (i, (line_no, name)) in headers.iter().enumerate() {
        let next_line = headers
            .get(i + 1)
            .map(|(n, _)| *n)
            .unwrap_or(lines.len() + 1);
        let block_start = *line_no - 1;
        let block_end = next_line - 1;
        let block = lines[block_start..block_end.min(lines.len())].join("\n");

        let command = command_re
            .captures(&block)
            .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
            .unwrap_or_default();

        let mut mem_limit_mb: Option<u64> = None;
        let mut mem_limit_line: Option<usize> = None;
        if let Some(caps) = mem_re.captures(&block) {
            let n: u64 = caps.get(1).unwrap().as_str().parse().unwrap_or(0);
            let unit = caps.get(2).unwrap().as_str();
            mem_limit_mb = Some(match unit {
                "g" | "G" => n * 1024,
                _ => n,
            });
            // Compute the absolute line for the match.
            if let Some(m) = caps.get(0) {
                let local_line = block[..m.start()].chars().filter(|c| *c == '\n').count() + 1;
                mem_limit_line = Some(block_start + local_line);
            }
        }

        services.push(WorkerService {
            name: name.clone(),
            name_line: *line_no,
            command,
            mem_limit_mb,
            mem_limit_line,
        });
    }
    services
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_repo(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-worker-mem-{label}-{}-{nanos}",
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
    fn worker_with_heavy_cart_and_low_mem_is_flagged() {
        let root = temp_repo("low");
        write(
            &root,
            "cartridges/foo/agent.py",
            "from rdflib import Graph\n",
        );
        write(
            &root,
            "example-api/docker-compose.yml",
            r#"services:
  worker-foo:
    command: celery -Q agent_foo_queue
    mem_limit: 768m
"#,
        );
        let env = doctor_worker_mem_budget(&root);
        assert_eq!(env.warnings.len(), 1, "{:?}", env.warnings);
        assert!(env.warnings[0].contains("worker-foo"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn worker_with_heavy_cart_and_safe_mem_is_silent() {
        let root = temp_repo("safe");
        write(
            &root,
            "cartridges/foo/agent.py",
            "from rdflib import Graph\n",
        );
        write(
            &root,
            "example-api/docker-compose.yml",
            r#"services:
  worker-foo:
    command: celery -Q agent_foo_queue
    mem_limit: 1536m
"#,
        );
        let env = doctor_worker_mem_budget(&root);
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn worker_without_heavy_cart_is_silent_even_at_low_mem() {
        let root = temp_repo("light");
        write(&root, "cartridges/lite/agent.py", "x = 1\n");
        write(
            &root,
            "example-api/docker-compose.yml",
            r#"services:
  worker-lite:
    command: celery -Q agent_lite_queue
    mem_limit: 512m
"#,
        );
        let env = doctor_worker_mem_budget(&root);
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }
}
