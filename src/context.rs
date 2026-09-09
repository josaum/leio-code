//! Context bundles rank repo artifacts for agent coding tasks.
//!
//! Human-facing reference: `leio-code/docs/CONTEXT_BUNDLE.md`.
//!
//! Ranking blends **hybrid sparse signals** aligned with repo-scale agent practice (SWE-bench–style
//! localization, hybrid lexical + structural cues, intent-conditioned routing):
//! - Lexical overlap on paths, symbols, env vars, and Redis keys with **smoothed IDF-style**
//!   weighting so rare discriminating terms outweigh ubiquitous path fragments.
//! - **Intent-conditioned path boosts** (tests/spec, deploy/CI, frontend, security-sensitive paths)
//!   when the task vocabulary matches—mirroring heuristic routing used in strong issue-localizers.
//! - **Identifier needles** (PascalCase / `SCREAMING_SNAKE`) extracted from the raw task for
//!   case-sensitive hits on paths and symbols—high precision for concrete edits.
//! - **Path-segment boost** when a task term equals a directory or file stem (one extension strip).
//! - **Reciprocal Rank Fusion (RRF)** across path / symbol / config layers (hybrid retrieval fusion).
//! - Optional **graph proximity** boosts when `.leio-code/exports/code-graph-v1/query-cache.json` is
//!   present—reweights neighbors of top seeds via import edges (no synchronous graph rebuild).
//! - Ties break on **newer `modified_unix_ms`**, then path order.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Instant;

use rayon::prelude::*;
use regex::Regex;
use serde_json::{Value, json};

use crate::capabilities::workspace_capabilities;
use crate::code_graph::CodeGraphQueryCache;
use crate::doctors::utils::query_id;
use crate::graph_query::try_load_graph_query_cache;
use crate::model::{
    DeployTargetRecord, EvidenceItem, FileRecord, QueryEnvelope, RedisKeyOccurrence, RepoIndex,
    SymbolOccurrence,
};

const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "can", "code", "do", "does", "for", "from",
    "go", "how", "i", "in", "into", "is", "it", "make", "of", "on", "or", "our", "the", "this",
    "to", "tool", "use", "we", "what", "when", "where", "with",
];

const AGENT_INSTRUCTION_FILENAMES: &[&str] = &[
    "AGENTS.md",
    "CLAUDE.md",
    "GEMINI.md",
    ".cursorrules",
    "codex.md",
];
const MEMORY_BANK_DIRS: &[&str] = &[
    ".leio-code/memory",
    ".leio-code/memories",
    ".codex/memory",
    "memory-bank",
    "docs/memory",
];
const MEMORY_BANK_FILES: &[&str] = &[
    ".leio-code/memory.md",
    ".codex/memory.md",
    "docs/agent-memory.md",
    "docs/agent-session-summary.md",
    "AGENT_MEMORY.md",
    "MEMORY.md",
];
const MAX_DISCOVERY_DOC_BYTES: u64 = 512 * 1024;

#[derive(Debug, Clone, Copy)]
struct IntentHints {
    wants_tests: bool,
    wants_deploy: bool,
    wants_frontend: bool,
    wants_security: bool,
}

#[derive(Debug, Clone, Copy)]
struct LayeredScore {
    path: i64,
    symbol: i64,
    config: i64,
}

#[derive(Debug, Clone)]
struct ScoredFile {
    path: String,
    language: String,
    score: i64,
    /// Indexer mtime (ms); used only for tie-breaking when scores match.
    modified_unix_ms: i128,
    reasons: BTreeSet<String>,
    symbols: Vec<Value>,
    env_vars: Vec<Value>,
    redis_keys: Vec<Value>,
}

#[derive(Debug, Clone)]
struct ScoredEntity {
    score: i64,
    value: Value,
    evidence: EvidenceItem,
}

struct ContextZoneInputs<'a> {
    agent_instructions: &'a [Value],
    memory_banks: &'a [Value],
    files: &'a [ScoredFile],
    symbols: &'a [ScoredEntity],
    env_vars: &'a [ScoredEntity],
    redis_keys: &'a [ScoredEntity],
    deploy_targets: &'a [ScoredEntity],
    graph_queries: &'a [Value],
    tests_to_run: &'a [Value],
    doctor_suggestions: &'a [Value],
    verification_anchors: &'a [Value],
    risk_notes: &'a [String],
}

pub fn build_context_bundle(
    index: &RepoIndex,
    root: &Path,
    task: &str,
    limit: usize,
    full: bool,
) -> QueryEnvelope {
    let started = Instant::now();
    let limit = limit.clamp(1, 40);
    let task_tokens = task_tokens(task);
    let capabilities = workspace_capabilities(index, root);
    let mut warnings = Vec::new();

    if task_tokens.is_empty() {
        warnings.push("context task did not contain enough searchable terms".to_string());
    }

    let task_lc = task.to_ascii_lowercase();
    let idf = sparse_token_idf(index, &task_tokens);
    let intents = classify_intents(&task_lc, &task_tokens);
    let identifier_needles = extract_identifier_needles(task);

    let graph_cache = try_load_graph_query_cache(root);
    let mut files = rank_files(
        index,
        &task_tokens,
        task,
        limit,
        &idf,
        intents,
        &identifier_needles,
        graph_cache.as_ref(),
    );
    if let Ok(hits) = crate::local_nodes::search_hits(root, task, limit.saturating_mul(3))
        && !hits.is_empty()
    {
        let hit_paths: HashSet<String> = hits.iter().map(|hit| hit.path.clone()).collect();
        for file in &mut files {
            if hit_paths.contains(&file.path) {
                file.score += 40;
                file.reasons.insert("local Arrow node/FCA hit".to_string());
            }
        }
        files.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| right.modified_unix_ms.cmp(&left.modified_unix_ms))
                .then_with(|| left.path.cmp(&right.path))
        });
    }
    files.truncate(limit);
    let symbols = rank_symbols(index, &task_tokens, limit, &idf);
    let env_vars = rank_env_vars(index, &task_tokens, limit, &idf);
    let redis_keys = rank_redis_keys(index, &task_tokens, limit, &idf);
    let deploy_targets = rank_deploy_targets(index, &task_tokens, limit, &idf);
    let tests_to_run = tests_to_run(&files, &task_tokens);
    let graph_queries = graph_queries(&files, &symbols, limit);
    let doctor_suggestions = doctor_suggestions(&capabilities.doctor_kinds, &task_tokens);
    let risk_notes = risk_notes(&files, &env_vars, &redis_keys, &deploy_targets);
    let agent_instructions = discover_agent_instructions(root, &files, limit);
    let memory_banks = discover_memory_banks(root, &task_tokens, limit);
    let verification_anchors = verification_anchors(root, &files, &task_tokens, limit);
    let context_zones = context_zones_with_detail(
        ContextZoneInputs {
            agent_instructions: &agent_instructions,
            memory_banks: &memory_banks,
            files: &files,
            symbols: &symbols,
            env_vars: &env_vars,
            redis_keys: &redis_keys,
            deploy_targets: &deploy_targets,
            graph_queries: &graph_queries,
            tests_to_run: &tests_to_run,
            doctor_suggestions: &doctor_suggestions,
            verification_anchors: &verification_anchors,
            risk_notes: &risk_notes,
        },
        full,
    );
    let execution_loop = execution_loop(&tests_to_run, &doctor_suggestions, &graph_queries);
    let selection_policy = selection_policy(limit, files.len(), intents);

    if files.is_empty() && symbols.is_empty() && env_vars.is_empty() && redis_keys.is_empty() {
        warnings.push("no indexed files or entities matched the task; try a more specific symbol, path, env var, Redis key, or API route".to_string());
    }

    let mut evidence = Vec::new();
    evidence.extend(files.iter().map(|file| EvidenceItem {
        kind: "context_file".to_string(),
        path: file.path.clone(),
        line: None,
        detail: format!(
            "score={} {}",
            file.score,
            file.reasons.iter().cloned().collect::<Vec<_>>().join("; ")
        ),
    }));
    if full {
        evidence.extend(
            symbols
                .iter()
                .take(limit)
                .map(|entity| entity.evidence.clone()),
        );
        evidence.extend(
            env_vars
                .iter()
                .take(limit)
                .map(|entity| entity.evidence.clone()),
        );
        evidence.extend(
            redis_keys
                .iter()
                .take(limit)
                .map(|entity| entity.evidence.clone()),
        );
    }
    evidence.extend(agent_instructions.iter().filter_map(|item| {
        Some(EvidenceItem {
            kind: "context_agent_instruction".to_string(),
            path: item.get("path")?.as_str()?.to_string(),
            line: None,
            detail: item.get("reason")?.as_str()?.to_string(),
        })
    }));
    evidence.extend(verification_anchors.iter().filter_map(|item| {
        Some(EvidenceItem {
            kind: "context_verification_anchor".to_string(),
            path: item.get("path")?.as_str()?.to_string(),
            line: item
                .get("line")
                .and_then(Value::as_u64)
                .and_then(|line| usize::try_from(line).ok()),
            detail: format!(
                "{} anchor",
                item.get("anchor").and_then(Value::as_str).unwrap_or("")
            ),
        })
    }));

    let workspace_profile = capabilities.workspace_profile.clone();
    // Default bundle is a diet: per-file entity lists are capped to the top
    // few plus counts, the duplicated instruction/memory keys stay only under
    // their zone-referenced names, and the full workspace capabilities block
    // stays out of meta. `--full` restores the exhaustive shape.
    let mut bundle = json!({
        "task": task,
        "tokens": task_tokens,
        "selection_policy": selection_policy,
        "retrieval_signals": retrieval_signals(
            intents,
            identifier_needles.len(),
            graph_cache.is_some(),
        ),
        "context_zones": context_zones,
        "instruction_sources": &agent_instructions,
        "memory_sources": &memory_banks,
        "files_to_read": files
            .iter()
            .map(|file| scored_file_json(file, full))
            .collect::<Vec<_>>(),
        "symbols": symbols.iter().map(|entity| entity.value.clone()).collect::<Vec<_>>(),
        "env_vars": env_vars.iter().map(|entity| entity.value.clone()).collect::<Vec<_>>(),
        "redis_keys": redis_keys.iter().map(|entity| entity.value.clone()).collect::<Vec<_>>(),
        "deploy_targets": deploy_targets.iter().map(|entity| entity.value.clone()).collect::<Vec<_>>(),
        "verification_anchors": verification_anchors,
        "graph_queries": graph_queries,
        "execution_loop": execution_loop,
        "tests_to_run": tests_to_run,
        "doctor_suggestions": doctor_suggestions,
        "risk_notes": risk_notes,
    });
    if full {
        // Exhaustive shape keeps the historical duplicate keys alongside
        // their zone-referenced names.
        bundle["agent_instructions"] = Value::Array(agent_instructions.clone());
        bundle["memory_banks"] = Value::Array(memory_banks.clone());
    }

    let file_count = files.len();
    let symbol_count = symbols.len();

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("context"),
        kind: "context".to_string(),
        summary: format!(
            "context bundle for `{task}`: {file_count} files, {symbol_count} symbols, {} tests, {} doctor suggestions, {} instruction docs, {} anchors",
            tests_to_run.len(),
            doctor_suggestions.len(),
            agent_instructions.len(),
            verification_anchors.len(),
        ),
        confidence: if file_count == 0 && symbol_count == 0 {
            0.45
        } else {
            0.86
        },
        entities: vec![bundle],
        evidence,
        warnings,
        meta: Some(json!({
            "limit": limit,
            "workspace_profile": workspace_profile,
            "workspace_capabilities": if full { Some(capabilities) } else { None },
            "next_tools": [
                "leio_code_context",
                "leio_code_find",
                "leio_code_graph",
                "leio_code_explain",
                "leio_code_doctor"
            ],
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn scored_file_json(file: &ScoredFile, full: bool) -> Value {
    let cap = |items: &[Value]| {
        if full {
            items.to_vec()
        } else {
            items.iter().take(3).cloned().collect::<Vec<_>>()
        }
    };
    if full {
        json!({
            "path": &file.path,
            "language": &file.language,
            "score": file.score,
            "modified_unix_ms": file.modified_unix_ms,
            "reasons": file.reasons.iter().cloned().collect::<Vec<_>>(),
            "symbols": cap(&file.symbols),
            "symbol_count": file.symbols.len(),
            "env_vars": cap(&file.env_vars),
            "env_var_count": file.env_vars.len(),
            "redis_keys": cap(&file.redis_keys),
            "redis_key_count": file.redis_keys.len(),
        })
    } else {
        json!({
            "path": &file.path,
            "language": &file.language,
            "score": file.score,
            "reasons": file.reasons.iter().take(2).cloned().collect::<Vec<_>>(),
            "symbols": cap(&file.symbols),
            "symbol_count": file.symbols.len(),
            "env_vars": cap(&file.env_vars),
            "env_var_count": file.env_vars.len(),
            "redis_keys": cap(&file.redis_keys),
            "redis_key_count": file.redis_keys.len(),
        })
    }
}

fn sparse_token_idf(index: &RepoIndex, task_tokens: &[String]) -> HashMap<String, f64> {
    let n = index.files.len().max(1) as f64;
    let mut weights = HashMap::new();
    for token in task_tokens {
        let tl = token.as_str();
        let df = index
            .files
            .iter()
            .filter(|file| {
                file.path.to_ascii_lowercase().contains(tl)
                    || file
                        .symbols
                        .iter()
                        .any(|symbol| symbol.name.to_ascii_lowercase().contains(tl))
            })
            .count() as f64;
        let idf = 1.0 + ((n + 1.0) / (df + 1.0)).ln();
        weights.insert(token.clone(), idf.min(3.5));
    }
    weights
}

fn classify_intents(task_lc: &str, tokens: &[String]) -> IntentHints {
    let set: HashSet<&str> = tokens.iter().map(|value| value.as_str()).collect();
    IntentHints {
        wants_tests: set.contains("test")
            || set.contains("tests")
            || set.contains("spec")
            || set.contains("jest")
            || set.contains("pytest")
            || set.contains("e2e")
            || task_lc.contains("unit test")
            || task_lc.contains("ci ")
            || task_lc.contains("playwright"),
        wants_deploy: set.contains("docker")
            || set.contains("deploy")
            || set.contains("kubernetes")
            || set.contains("k8s")
            || set.contains("compose")
            || set.contains("helm")
            || task_lc.contains("dockerfile")
            || task_lc.contains("workflow"),
        wants_frontend: set.contains("react")
            || set.contains("next")
            || set.contains("css")
            || set.contains("frontend")
            || set.contains("ui")
            || set.contains("tsx")
            || set.contains("jsx")
            || set.contains("tailwind"),
        wants_security: set.contains("auth")
            || set.contains("jwt")
            || set.contains("oauth")
            || set.contains("secret")
            || set.contains("password")
            || set.contains("token")
            || set.contains("vault"),
    }
}

fn intent_path_bonus(path: &str, hints: IntentHints) -> (i64, Vec<&'static str>) {
    let p = path.to_ascii_lowercase();
    let mut bonus = 0i64;
    let mut tags = Vec::new();

    if hints.wants_tests
        && (p.contains("/tests/")
            || p.contains("__tests__")
            || p.contains("/test/")
            || p.contains(".spec.")
            || p.contains(".test.")
            || p.ends_with("_test.rs")
            || p.ends_with("_test.py")
            || p.contains("/spec/")
            || p.contains("/e2e/"))
    {
        bonus += 18;
        tags.push("intent_route_tests_specs");
    }
    if hints.wants_deploy
        && (p.contains("dockerfile")
            || p.contains("/deploy/")
            || p.contains(".github/workflows")
            || p.contains("docker-compose")
            || p.contains("/kubernetes/")
            || p.contains("/helm/")
            || p.contains("/k8s/"))
    {
        bonus += 18;
        tags.push("intent_route_deploy_ci");
    }
    if hints.wants_frontend
        && (p.contains("/components/")
            || p.contains("/pages/")
            || p.contains("/app/")
            || p.ends_with(".tsx")
            || p.ends_with(".jsx")
            || p.contains("/styles/")
            || p.contains(".css"))
    {
        bonus += 14;
        tags.push("intent_route_frontend");
    }
    if hints.wants_security
        && (p.contains("/auth/")
            || p.contains("vault")
            || p.contains("/secrets/")
            || p.ends_with(".pem")
            || p.contains("/oauth"))
    {
        bonus += 14;
        tags.push("intent_route_security");
    }

    (bonus, tags)
}

static IDENTIFIER_PASCAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b[A-Z][a-z]+(?:[A-Z][a-z0-9]*)+\b").expect("valid identifier regex")
});
static IDENTIFIER_UPPER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[A-Z][A-Z0-9_]{3,}\b").expect("valid upper identifier regex"));

fn extract_identifier_needles(task: &str) -> Vec<String> {
    let mut out = Vec::new();
    for found in IDENTIFIER_PASCAL.find_iter(task) {
        out.push(found.as_str().to_string());
    }
    for found in IDENTIFIER_UPPER.find_iter(task) {
        let fragment = found.as_str();
        if fragment.contains('_') || fragment.len() >= 6 {
            out.push(fragment.to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}

fn identifier_needle_path_bonus(file: &FileRecord, needles: &[String]) -> i64 {
    if needles.is_empty() {
        return 0;
    }
    let path = file.path.as_str();
    let mut bonus = 0i64;
    for needle in needles {
        if path.contains(needle.as_str()) {
            bonus += 26;
        }
    }
    bonus.min(80)
}

fn identifier_needle_symbol_bonus(file: &FileRecord, needles: &[String]) -> i64 {
    if needles.is_empty() {
        return 0;
    }
    let mut bonus = 0i64;
    for needle in needles {
        if file
            .symbols
            .iter()
            .any(|symbol| symbol.name.contains(needle.as_str()))
        {
            bonus += 30;
        }
        if file
            .env_vars
            .iter()
            .any(|env| env.name.contains(needle.as_str()))
        {
            bonus += 22;
        }
    }
    bonus.min(120)
}

fn intent_route_labels(intents: IntentHints) -> Vec<&'static str> {
    let mut routes = Vec::new();
    if intents.wants_tests {
        routes.push("tests_specs_ci");
    }
    if intents.wants_deploy {
        routes.push("deploy_docker_ops");
    }
    if intents.wants_frontend {
        routes.push("frontend_ui");
    }
    if intents.wants_security {
        routes.push("security_auth_secrets");
    }
    routes
}

fn retrieval_signals(intents: IntentHints, needle_count: usize, graph_cache_loaded: bool) -> Value {
    json!({
        "sparse_idf_weighting": true,
        "intent_routes_considered": intent_route_labels(intents),
        "identifier_needles_extracted": needle_count,
        "graph_cache_loaded": graph_cache_loaded,
        "code_graph_refresh_hint": if graph_cache_loaded {
            json!(null)
        } else {
            json!("Run `leio-code export code-graph` from the repo root (after `leio-code index`) to populate `.leio-code/exports/code-graph-v1/query-cache.json` and unlock import + call-chain proximity boosts in context bundles.")
        },
        "reading_order": "Prefer files_to_read from the top down; truncate from the bottom if you must save tokens—later rows are weaker priors (mitigates lost-in-the-middle when consumers clip context).",
        "signal_stack": "lexical_sparse_idf · identifier_needles · intent_path_routes · path_segment_stem · rrf_path_symbol_config_layers · graph_import_proximity · graph_callchain_proximity_optional · mtime_tiebreak",
    })
}

/// Competition-style ranks: tied scores share the same rank (so identical layers → identical RRF).
fn assign_dense_ranks(scores: &[i64]) -> Vec<i64> {
    let n = scores.len();
    if n == 0 {
        return vec![];
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| scores[b].cmp(&scores[a]).then_with(|| a.cmp(&b)));

    let mut ranks = vec![0i64; n];
    let mut pos = 0;
    while pos < n {
        let value = scores[order[pos]];
        let rank = (pos + 1) as i64;
        let mut next = pos + 1;
        while next < n && scores[order[next]] == value {
            next += 1;
        }
        for k in pos..next {
            ranks[order[k]] = rank;
        }
        pos = next;
    }
    ranks
}

fn graph_neighbor_paths(cache: &CodeGraphQueryCache, seeds: &[String]) -> HashSet<String> {
    let mut out = HashSet::new();
    for seed in seeds {
        if let Some(imps) = cache.importers_by_target_path.get(seed.as_str()) {
            for imp in imps {
                out.insert(imp.path.clone());
            }
        }
        if let Some(iris) = cache.file_lookup_path.get(seed.as_str()) {
            for iri in iris {
                if let Some(details) = cache.file_import_details.get(iri) {
                    for detail in details {
                        for candidate in &detail.candidate_paths {
                            out.insert(candidate.clone());
                        }
                    }
                }
            }
        }
    }
    out
}

/// Paths of symbols that call—or are called from—definitions named in top-ranked files (call graph).
fn graph_symbol_edge_paths(
    cache: &CodeGraphQueryCache,
    top_files: &[&ScoredFile],
    mut symbol_budget: usize,
) -> HashSet<String> {
    let mut out = HashSet::new();
    const MAX_PATHS: usize = 256;
    for file in top_files {
        if symbol_budget == 0 || out.len() >= MAX_PATHS {
            break;
        }
        for sym in &file.symbols {
            if symbol_budget == 0 || out.len() >= MAX_PATHS {
                break;
            }
            let Some(name) = sym.get("name").and_then(Value::as_str) else {
                continue;
            };
            let Some(iris) = cache.symbol_lookup_name.get(name) else {
                continue;
            };
            symbol_budget -= 1;
            for iri in iris {
                if let Some(callers) = cache.callers_by_symbol.get(iri) {
                    for neighbor in callers {
                        out.insert(neighbor.path.clone());
                        if out.len() >= MAX_PATHS {
                            return out;
                        }
                    }
                }
                if let Some(callees) = cache.callees_by_symbol.get(iri) {
                    for neighbor in callees {
                        out.insert(neighbor.path.clone());
                        if out.len() >= MAX_PATHS {
                            return out;
                        }
                    }
                }
            }
        }
    }
    out
}

fn finalize_file_ranking(
    mut rows: Vec<(ScoredFile, LayeredScore)>,
    graph_cache: Option<&CodeGraphQueryCache>,
) -> Vec<ScoredFile> {
    let n = rows.len();
    if n == 0 {
        return vec![];
    }

    let path_v: Vec<i64> = rows.iter().map(|(_, layer)| layer.path).collect();
    let sym_v: Vec<i64> = rows.iter().map(|(_, layer)| layer.symbol).collect();
    let cfg_v: Vec<i64> = rows.iter().map(|(_, layer)| layer.config).collect();
    let rank_path = assign_dense_ranks(&path_v);
    let rank_sym = assign_dense_ranks(&sym_v);
    let rank_cfg = assign_dense_ranks(&cfg_v);

    const RRF_K: f64 = 60.0;
    const RRF_SCALE: f64 = 25_000.0;

    for i in 0..n {
        // A zero-score channel did not retrieve this file. Tied zero scores
        // must not manufacture rank-one evidence (especially config channels).
        let contribution = |score: i64, rank: i64| {
            if score > 0 {
                1.0 / (RRF_K + rank as f64)
            } else {
                0.0
            }
        };
        let rrf = contribution(path_v[i], rank_path[i])
            + contribution(sym_v[i], rank_sym[i])
            + contribution(cfg_v[i], rank_cfg[i]);
        let base = rows[i].1.path + rows[i].1.symbol + rows[i].1.config;
        rows[i].0.score = base + (rrf * RRF_SCALE) as i64;
    }

    rows.sort_by(|(left, _), (right, _)| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| right.modified_unix_ms.cmp(&left.modified_unix_ms))
            .then_with(|| left.path.cmp(&right.path))
    });

    if let Some(cache) = graph_cache {
        let seeds: Vec<String> = rows.iter().take(4).map(|(f, _)| f.path.clone()).collect();
        let top_refs: Vec<&ScoredFile> = rows.iter().take(4).map(|(f, _)| f).collect();
        let seed_set: HashSet<String> = seeds.iter().cloned().collect();
        let import_neighbors = graph_neighbor_paths(cache, &seeds);
        let chain_neighbors = graph_symbol_edge_paths(cache, &top_refs, 12);
        for (file, _) in rows.iter_mut() {
            if seed_set.contains(&file.path) {
                continue;
            }
            let from_import = import_neighbors.contains(&file.path);
            let from_call = chain_neighbors.contains(&file.path);
            if from_import && from_call {
                file.score += 22;
                file.reasons.insert(
                    "graph proximity: import edges + symbol caller/callee chain to top seeds"
                        .to_string(),
                );
            } else if from_import {
                file.score += 18;
                file.reasons.insert(
                    "graph proximity to top-ranked seeds (import edge in code-graph cache)"
                        .to_string(),
                );
            } else if from_call {
                file.score += 16;
                file.reasons.insert(
                    "graph proximity via matched symbols (callers/callees in code-graph cache)"
                        .to_string(),
                );
            }
        }
        rows.sort_by(|(left, _), (right, _)| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| right.modified_unix_ms.cmp(&left.modified_unix_ms))
                .then_with(|| left.path.cmp(&right.path))
        });
    }

    rows.into_iter().map(|(file, _)| file).collect()
}

#[allow(clippy::too_many_arguments)]
fn rank_files(
    index: &RepoIndex,
    task_tokens: &[String],
    task: &str,
    limit: usize,
    idf: &HashMap<String, f64>,
    intents: IntentHints,
    identifier_needles: &[String],
    graph_cache: Option<&CodeGraphQueryCache>,
) -> Vec<ScoredFile> {
    let compounds = compound_path_tokens(task);
    let rows: Vec<_> = index
        .files
        .par_iter()
        .filter_map(|file| {
            score_file(
                file,
                task_tokens,
                &compounds,
                limit,
                idf,
                intents,
                identifier_needles,
            )
        })
        .collect();
    finalize_file_ranking(rows, graph_cache)
}

fn score_file(
    file: &FileRecord,
    task_tokens: &[String],
    compounds: &[String],
    limit: usize,
    idf: &HashMap<String, f64>,
    intents: IntentHints,
    identifier_needles: &[String],
) -> Option<(ScoredFile, LayeredScore)> {
    let mut path_layer = match_score_idf(task_tokens, &file.path, 12, idf);
    let mut reasons = BTreeSet::new();
    if path_layer > 0 {
        reasons.insert("path matches task terms (IDF-weighted)".to_string());
    }

    let seg_bonus = path_segment_match_bonus(task_tokens, compounds, &file.path);
    if seg_bonus > 0 {
        path_layer += seg_bonus;
        reasons.insert("task term matches a path segment (directory or file stem)".to_string());
    }

    let (intent_bonus, intent_tags) = intent_path_bonus(&file.path, intents);
    if intent_bonus > 0 {
        path_layer += intent_bonus;
        reasons.insert(format!(
            "intent-conditioned path boost ({})",
            intent_tags.join(", ")
        ));
    }

    let path_needles = identifier_needle_path_bonus(file, identifier_needles);
    let sym_needles = identifier_needle_symbol_bonus(file, identifier_needles);
    if path_needles > 0 || sym_needles > 0 {
        reasons.insert(
            "raw task identifiers (PascalCase / SCREAMING_SNAKE) match path, symbols, or env"
                .to_string(),
        );
    }
    path_layer += path_needles;

    let mut symbols = file
        .symbols
        .iter()
        .filter_map(|symbol| {
            let symbol_score = match_score_idf(task_tokens, &symbol.name, 14, idf)
                + match_score_idf(task_tokens, symbol.kind.as_str(), 4, idf);
            if symbol_score == 0 {
                return None;
            }
            Some((
                symbol_score,
                json!({
                    "name": &symbol.name,
                    "kind": symbol.kind.as_str(),
                    "line": symbol.line,
                }),
            ))
        })
        .collect::<Vec<_>>();
    symbols.sort_by_key(|right| std::cmp::Reverse(right.0));

    let mut symbol_layer = sym_needles;
    if let Some((symbol_score, _)) = symbols.first() {
        symbol_layer += *symbol_score;
        reasons.insert("defines matching symbols".to_string());
    }

    let mut env_vars = file
        .env_vars
        .iter()
        .filter_map(|env| {
            let env_score = match_score_idf(task_tokens, &env.name, 12, idf);
            if env_score == 0 {
                return None;
            }
            Some((
                env_score,
                json!({
                    "name": &env.name,
                    "access": env.access.as_str(),
                    "line": env.line,
                }),
            ))
        })
        .collect::<Vec<_>>();
    env_vars.sort_by_key(|right| std::cmp::Reverse(right.0));

    let mut redis_keys = file
        .redis_keys
        .iter()
        .filter_map(|key| {
            let key_score = match_score_idf(task_tokens, &key.key, 12, idf);
            if key_score == 0 {
                return None;
            }
            Some((
                key_score,
                json!({
                    "key": &key.key,
                    "access": key.access.as_str(),
                    "line": key.line,
                }),
            ))
        })
        .collect::<Vec<_>>();
    redis_keys.sort_by_key(|right| std::cmp::Reverse(right.0));

    let mut config_layer = 0i64;
    if let Some((env_score, _)) = env_vars.first() {
        config_layer += *env_score;
        reasons.insert("touches matching env vars".to_string());
    }
    if let Some((redis_score, _)) = redis_keys.first() {
        config_layer += *redis_score;
        reasons.insert("touches matching Redis keys".to_string());
    }

    let score = path_layer + symbol_layer + config_layer;
    if score == 0 {
        return None;
    }

    let layers = LayeredScore {
        path: path_layer,
        symbol: symbol_layer,
        config: config_layer,
    };

    Some((
        ScoredFile {
            path: file.path.clone(),
            language: file.language.as_str().to_string(),
            score,
            modified_unix_ms: file.modified_unix_ms,
            reasons,
            symbols: symbols
                .into_iter()
                .take(limit.min(5))
                .map(|(_, value)| value)
                .collect(),
            env_vars: env_vars
                .into_iter()
                .take(limit.min(5))
                .map(|(_, value)| value)
                .collect(),
            redis_keys: redis_keys
                .into_iter()
                .take(limit.min(5))
                .map(|(_, value)| value)
                .collect(),
        },
        layers,
    ))
}

fn rank_symbols(
    index: &RepoIndex,
    task_tokens: &[String],
    limit: usize,
    idf: &HashMap<String, f64>,
) -> Vec<ScoredEntity> {
    let mut entities = index
        .all_symbols()
        .filter_map(|symbol| score_symbol(symbol, task_tokens, idf))
        .collect::<Vec<_>>();
    sort_entities(&mut entities);
    entities.truncate(limit);
    entities
}

fn score_symbol(
    symbol: &SymbolOccurrence,
    task_tokens: &[String],
    idf: &HashMap<String, f64>,
) -> Option<ScoredEntity> {
    let score = match_score_idf(task_tokens, &symbol.name, 16, idf)
        + match_score_idf(task_tokens, symbol.kind.as_str(), 4, idf)
        + match_score_idf(task_tokens, &symbol.path, 5, idf);
    if score == 0 {
        return None;
    }
    Some(ScoredEntity {
        score,
        value: json!({
            "name": &symbol.name,
            "kind": symbol.kind.as_str(),
            "path": &symbol.path,
            "line": symbol.line,
            "language": symbol.language.as_str(),
            "score": score,
        }),
        evidence: EvidenceItem {
            kind: "context_symbol".to_string(),
            path: symbol.path.clone(),
            line: Some(symbol.line),
            detail: format!("score={score} {} {}", symbol.kind.as_str(), symbol.name),
        },
    })
}

fn rank_env_vars(
    index: &RepoIndex,
    task_tokens: &[String],
    limit: usize,
    idf: &HashMap<String, f64>,
) -> Vec<ScoredEntity> {
    let mut entities = index
        .all_env_vars()
        .filter_map(|env| {
            let score = match_score_idf(task_tokens, &env.name, 14, idf)
                + match_score_idf(task_tokens, env.access.as_str(), 3, idf)
                + match_score_idf(task_tokens, &env.path, 4, idf);
            if score == 0 {
                return None;
            }
            Some(ScoredEntity {
                score,
                value: json!({
                    "name": &env.name,
                    "access": env.access.as_str(),
                    "path": &env.path,
                    "line": env.line,
                    "language": env.language.as_str(),
                    "score": score,
                }),
                evidence: EvidenceItem {
                    kind: "context_env_var".to_string(),
                    path: env.path.clone(),
                    line: Some(env.line),
                    detail: format!("score={score} {} {}", env.access.as_str(), env.name),
                },
            })
        })
        .collect::<Vec<_>>();
    sort_entities(&mut entities);
    entities.truncate(limit);
    entities
}

fn rank_redis_keys(
    index: &RepoIndex,
    task_tokens: &[String],
    limit: usize,
    idf: &HashMap<String, f64>,
) -> Vec<ScoredEntity> {
    let mut entities = index
        .all_redis_keys()
        .filter_map(|key| score_redis_key(key, task_tokens, idf))
        .collect::<Vec<_>>();
    sort_entities(&mut entities);
    entities.truncate(limit);
    entities
}

fn score_redis_key(
    key: &RedisKeyOccurrence,
    task_tokens: &[String],
    idf: &HashMap<String, f64>,
) -> Option<ScoredEntity> {
    let score = match_score_idf(task_tokens, &key.key, 14, idf)
        + match_score_idf(task_tokens, key.access.as_str(), 3, idf)
        + match_score_idf(task_tokens, &key.path, 4, idf);
    if score == 0 {
        return None;
    }
    Some(ScoredEntity {
        score,
        value: json!({
            "key": &key.key,
            "access": key.access.as_str(),
            "path": &key.path,
            "line": key.line,
            "language": key.language.as_str(),
            "score": score,
        }),
        evidence: EvidenceItem {
            kind: "context_redis_key".to_string(),
            path: key.path.clone(),
            line: Some(key.line),
            detail: format!("score={score} {} {}", key.access.as_str(), key.key),
        },
    })
}

fn rank_deploy_targets(
    index: &RepoIndex,
    task_tokens: &[String],
    limit: usize,
    idf: &HashMap<String, f64>,
) -> Vec<ScoredEntity> {
    let mut entities = index
        .deploy_targets
        .iter()
        .filter_map(|target| score_deploy_target(target, task_tokens, idf))
        .collect::<Vec<_>>();
    sort_entities(&mut entities);
    entities.truncate(limit);
    entities
}

fn score_deploy_target(
    target: &DeployTargetRecord,
    task_tokens: &[String],
    idf: &HashMap<String, f64>,
) -> Option<ScoredEntity> {
    let mut score = match_score_idf(task_tokens, &target.name, 16, idf)
        + match_score_idf(task_tokens, &target.path, 5, idf)
        + target
            .backend_profile
            .as_deref()
            .map(|value| match_score_idf(task_tokens, value, 8, idf))
            .unwrap_or(0)
        + target
            .frontend_project
            .as_deref()
            .map(|value| match_score_idf(task_tokens, value, 8, idf))
            .unwrap_or(0);
    for cartridge in &target.cartridges {
        score += match_score_idf(task_tokens, cartridge, 6, idf);
    }
    if score == 0 {
        return None;
    }
    Some(ScoredEntity {
        score,
        value: json!({
            "name": &target.name,
            "path": &target.path,
            "backend_profile": &target.backend_profile,
            "frontend_project": &target.frontend_project,
            "cartridges": &target.cartridges,
            "score": score,
        }),
        evidence: EvidenceItem {
            kind: "context_deploy_target".to_string(),
            path: target.path.clone(),
            line: None,
            detail: format!("score={score} deploy target {}", target.name),
        },
    })
}

fn graph_queries(files: &[ScoredFile], symbols: &[ScoredEntity], limit: usize) -> Vec<Value> {
    let mut queries = Vec::new();
    let mut seen = HashSet::new();

    for symbol in symbols.iter().take(limit.min(4)) {
        let Some(name) = symbol.value.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(kind) = symbol.value.get("kind").and_then(Value::as_str) else {
            continue;
        };
        if matches!(kind, "function" | "method" | "class" | "interface")
            && seen.insert(format!("callers:{name}"))
        {
            queries.push(json!({
                "tool": "leio_code_graph",
                "kind": "callers-of",
                "needle": name,
                "reason": "inspect impact before editing matched callable",
            }));
            queries.push(json!({
                "tool": "leio_code_graph",
                "kind": "callsites-of",
                "needle": name,
                "reason": "inspect concrete callsites before editing matched callable",
            }));
        }
    }

    for file in files.iter().take(limit.min(4)) {
        if seen.insert(format!("symbols:{}", file.path)) {
            queries.push(json!({
                "tool": "leio_code_graph",
                "kind": "symbols-in",
                "needle": &file.path,
                "reason": "inventory local symbols before editing a ranked file",
            }));
        }
        if seen.insert(format!("imports:{}", file.path)) {
            queries.push(json!({
                "tool": "leio_code_graph",
                "kind": "resolved-imports-in",
                "needle": &file.path,
                "reason": "inspect dependency edges around a ranked file",
            }));
        }
    }

    queries.truncate(limit.max(4));
    queries
}

fn tests_to_run(files: &[ScoredFile], task_tokens: &[String]) -> Vec<Value> {
    let mut commands = BTreeSet::new();

    for file in files {
        let path = file.path.as_str();
        if path.ends_with(".rs") || path.starts_with("src/") {
            commands.insert((
                "cargo test".to_string(),
                "Rust source or crate metadata is in the ranked context".to_string(),
            ));
        }
        if path.starts_with("mcp/") {
            commands.insert((
                "cd mcp && npm run check && npm test".to_string(),
                "MCP wrapper files are in the ranked context".to_string(),
            ));
        }
        if path.starts_with("apps-sdk/") {
            commands.insert((
                "cd apps-sdk && npm run check && npm test".to_string(),
                "Apps SDK files are in the ranked context".to_string(),
            ));
        }
        if path.contains("Dockerfile")
            || path.contains("smoke-docker")
            || task_tokens
                .iter()
                .any(|token| token == "docker" || token == "smoke")
        {
            commands.insert((
                "cd apps-sdk && npm run docker:smoke".to_string(),
                "Docker or smoke-test surface is in scope".to_string(),
            ));
        }
        if path.starts_with("scripts/") || path.starts_with("tests/") || path.ends_with(".py") {
            commands.insert((
                "python3 -m unittest discover -s tests".to_string(),
                "Python scripts or tests are in the ranked context".to_string(),
            ));
        }
        if path.starts_with("packages/trpc/") {
            commands.insert((
                "pnpm -C packages/trpc typecheck".to_string(),
                "Shared tRPC package surface is in the ranked context".to_string(),
            ));
        }
        if path.starts_with("example-ops/") {
            commands.insert((
                "pnpm --dir example-ops typecheck && pnpm --dir example-ops test".to_string(),
                "Unified Example Ops frontend surface is in the ranked context".to_string(),
            ));
        }
    }

    if task_tokens
        .iter()
        .any(|token| matches!(token.as_str(), "verify" | "contract" | "doctor" | "plugin"))
    {
        commands.insert((
            "cargo run -- verify".to_string(),
            "Task mentions verification, contract, doctor, or plugin behavior".to_string(),
        ));
    }

    commands
        .into_iter()
        .map(|(command, reason)| json!({ "command": command, "reason": reason }))
        .collect()
}

fn doctor_suggestions(available: &[String], task_tokens: &[String]) -> Vec<Value> {
    let rules: &[(&[&str], &[&str])] = &[
        (
            &["auth", "jwt", "oauth", "token", "login"],
            &["auth-brokering", "flight-auth", "frontend-readiness"],
        ),
        (
            &["deploy", "docker", "fly", "prod", "release"],
            &["deploy", "prod-surface-hygiene", "self-contract"],
        ),
        (
            &["redis", "session", "lease", "state"],
            &["redis-key-hygiene", "session-hot-state"],
        ),
        (
            &["event", "stream", "semantic"],
            &["event-durability", "event-envelope", "semantic-wiring"],
        ),
        (
            &["frontend", "ui", "next", "react"],
            &["frontend-engine-client", "frontend-readiness"],
        ),
        (
            &[
                "leio", "mcp", "plugin", "package", "contract", "verify", "apps", "sdk",
            ],
            &["self-contract"],
        ),
    ];

    let available = available.iter().map(String::as_str).collect::<HashSet<_>>();
    let tokens = task_tokens
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut suggestions = Vec::new();
    let mut seen = HashSet::new();

    for (triggers, doctors) in rules {
        if !triggers.iter().any(|trigger| tokens.contains(trigger)) {
            continue;
        }
        for doctor in *doctors {
            if available.contains(doctor) && seen.insert(*doctor) {
                suggestions.push(json!({
                    "tool": "leio_code_doctor",
                    "kind": doctor,
                    "reason": format!("task matched doctor trigger(s): {}", triggers.join(", ")),
                }));
            }
        }
    }

    if !available.is_empty() && suggestions.is_empty() {
        suggestions.push(json!({
            "tool": "leio_code_doctor",
            "kind": "all",
            "reason": "profile exposes doctors; run the registered suite before a broad edit",
        }));
    }

    suggestions
}

fn risk_notes(
    files: &[ScoredFile],
    env_vars: &[ScoredEntity],
    redis_keys: &[ScoredEntity],
    deploy_targets: &[ScoredEntity],
) -> Vec<String> {
    let mut notes = BTreeSet::new();

    for file in files {
        let path = file.path.as_str();
        if path.contains("Cargo.toml")
            || path.contains("Cargo.lock")
            || path.contains("package.json")
            || path.contains("package-lock.json")
        {
            notes.insert("Manifest or lockfile changes can affect packaging, Docker builds, and plugin distribution.".to_string());
        }
        if path.contains("Dockerfile")
            || path.contains("entrypoint")
            || path.contains("smoke-docker")
        {
            notes.insert("Container/runtime changes should be verified with the Docker smoke path, not only unit tests.".to_string());
        }
        if path.starts_with("mcp/") || path.starts_with("apps-sdk/") {
            notes.insert("MCP and Apps SDK surfaces need tool-name and structuredContent parity after edits.".to_string());
        }
        if path.starts_with("src/doctors/") {
            notes.insert("Doctor changes should include focused regression tests plus self-contract coverage when they affect exposed surfaces.".to_string());
        }
    }

    if !env_vars.is_empty() {
        notes.insert("Env-var changes can affect deploy profiles and runtime auth; explain the variable before editing.".to_string());
    }
    if !redis_keys.is_empty() {
        notes.insert("Redis key changes can alter hot state/session contracts; inspect readers and writers together.".to_string());
    }
    if !deploy_targets.is_empty() {
        notes.insert("Deploy-target changes should keep health, smoke, rollback, profile, and secret-set lineage aligned.".to_string());
    }

    notes.into_iter().collect()
}

fn discover_agent_instructions(root: &Path, files: &[ScoredFile], limit: usize) -> Vec<Value> {
    let mut candidates = BTreeSet::new();

    for filename in AGENT_INSTRUCTION_FILENAMES {
        candidates.insert(PathBuf::from(filename));
    }
    candidates.insert(PathBuf::from(".github/copilot-instructions.md"));

    for dir in ranked_file_scope_dirs(files) {
        for filename in AGENT_INSTRUCTION_FILENAMES {
            candidates.insert(dir.join(filename));
        }
    }

    let mut discovered = candidates
        .into_iter()
        .filter_map(|relative_path| {
            let full_path = root.join(&relative_path);
            if !is_small_existing_file(&full_path) {
                return None;
            }
            let path = normalize_relative_path(&relative_path);
            Some(json!({
                "path": path,
                "kind": instruction_kind(&relative_path),
                "scope": instruction_scope(&relative_path),
                "line": 1,
                "trust_level": "repo_guidance_not_system",
                "reason": "agent instruction file discovered for the ranked context scope",
            }))
        })
        .collect::<Vec<_>>();

    discovered.sort_by(|left, right| {
        let left_path = left.get("path").and_then(Value::as_str).unwrap_or("");
        let right_path = right.get("path").and_then(Value::as_str).unwrap_or("");
        path_depth(left_path)
            .cmp(&path_depth(right_path))
            .then_with(|| left_path.cmp(right_path))
    });
    discovered.truncate(limit.clamp(6, 20));
    discovered
}

fn ranked_file_scope_dirs(files: &[ScoredFile]) -> BTreeSet<PathBuf> {
    let mut dirs = BTreeSet::new();

    for file in files {
        let Some(parent) = Path::new(&file.path).parent() else {
            continue;
        };
        let mut current = PathBuf::new();
        for component in parent.components().take(4) {
            current.push(component.as_os_str());
            dirs.insert(current.clone());
        }
    }

    dirs
}

fn discover_memory_banks(root: &Path, task_tokens: &[String], limit: usize) -> Vec<Value> {
    let mut candidates = BTreeSet::new();

    for filename in MEMORY_BANK_FILES {
        candidates.insert(PathBuf::from(filename));
    }

    for dirname in MEMORY_BANK_DIRS {
        let dir = root.join(dirname);
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if is_memory_doc_path(&path) {
                candidates.insert(PathBuf::from(dirname).join(entry.file_name()));
            }
        }
    }

    let mut discovered = candidates
        .into_iter()
        .filter_map(|relative_path| {
            let full_path = root.join(&relative_path);
            if !is_small_existing_file(&full_path) {
                return None;
            }
            let (line, matched_terms) = memory_match(&full_path, task_tokens);
            Some(json!({
                "path": normalize_relative_path(&relative_path),
                "kind": memory_kind(&relative_path),
                "scope": instruction_scope(&relative_path),
                "line": line,
                "matched_terms": matched_terms,
                "reason": "persistent agent memory source; read only the relevant lines before planning",
            }))
        })
        .collect::<Vec<_>>();

    discovered.sort_by(|left, right| {
        let left_terms = left
            .get("matched_terms")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        let right_terms = right
            .get("matched_terms")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        let left_path = left.get("path").and_then(Value::as_str).unwrap_or("");
        let right_path = right.get("path").and_then(Value::as_str).unwrap_or("");
        right_terms
            .cmp(&left_terms)
            .then_with(|| path_depth(left_path).cmp(&path_depth(right_path)))
            .then_with(|| left_path.cmp(right_path))
    });
    discovered.truncate(limit.clamp(4, 12));
    discovered
}

fn verification_anchors(
    root: &Path,
    files: &[ScoredFile],
    task_tokens: &[String],
    limit: usize,
) -> Vec<Value> {
    let anchor_re = Regex::new(r"#([A-Za-z][A-Za-z0-9_-]{2,})").expect("valid anchor regex");
    let mut anchors = Vec::new();
    let mut seen = HashSet::new();

    for file in files.iter().take(limit.clamp(6, 16)) {
        let full_path = root.join(&file.path);
        if !is_small_existing_file(&full_path) {
            continue;
        }
        let Ok(src) = fs::read_to_string(&full_path) else {
            continue;
        };
        for (idx, line) in src.lines().enumerate() {
            if !looks_like_comment_line(line) {
                continue;
            }
            for capture in anchor_re.captures_iter(line) {
                let Some(raw) = capture.get(1).map(|value| value.as_str()) else {
                    continue;
                };
                if looks_like_color_literal(raw) || !anchor_matches_task(raw, line, task_tokens) {
                    continue;
                }
                let anchor = format!("#{raw}");
                let key = format!("{}:{anchor}", file.path);
                if !seen.insert(key) {
                    continue;
                }
                anchors.push(json!({
                    "anchor": anchor,
                    "kind": "comment_anchor",
                    "path": file.path,
                    "line": idx + 1,
                    "source_field": "verification_anchors",
                    "reason": "task terms matched an explicit verification anchor in a ranked file",
                }));
                if anchors.len() >= limit.clamp(4, 16) {
                    return anchors;
                }
            }
        }
    }

    anchors
}

/// Diet mode compacts every zone item to a `{kind, source_field, path, line}`
/// reference: the pointed-to section already carries the full payload, and
/// the zone's job is ordering + purpose, not a second copy.
fn context_zones_with_detail(inputs: ContextZoneInputs<'_>, full: bool) -> Vec<Value> {
    vec![
        json!({
            "name": "instructions",
            "purpose": "Repo guidance the agent should inspect before planning edits",
            "items": inputs.agent_instructions.iter().take(if full { 8 } else { 4 }).map(|item| context_zone_ref(item, "instruction_source", "instruction_sources", full)).collect::<Vec<_>>(),
        }),
        json!({
            "name": "memory",
            "purpose": "Persistent project or session memory to consult without dumping chat history",
            "items": inputs.memory_banks.iter().take(if full { 6 } else { 4 }).map(|item| context_zone_ref(item, "memory_source", "memory_sources", full)).collect::<Vec<_>>(),
        }),
        json!({
            "name": "anchors",
            "purpose": "Task-specific verification anchors found in comments",
            "items": inputs.verification_anchors.iter().take(if full { 8 } else { 4 }).map(|item| context_zone_ref(item, "verification_anchor", "verification_anchors", full)).collect::<Vec<_>>(),
        }),
        json!({
            "name": "ranked_files",
            "purpose": "Primary bounded working set: hybrid lexical (IDF-weighted), identifier needles, intent routes, segment stems, symbols/env/redis/deploy cues—read top-first",
            "items": inputs.files.iter().take(12).map(|file| if full {
                json!({
                    "kind": "ranked_file",
                    "source_field": "files_to_read",
                    "path": file.path,
                    "score": file.score,
                    "reason": file.reasons.iter().cloned().collect::<Vec<_>>().join("; "),
                })
            } else {
                json!({
                    "kind": "ranked_file",
                    "source_field": "files_to_read",
                    "path": file.path,
                })
            }).collect::<Vec<_>>(),
            "related_entity_counts": {
                "symbols": inputs.symbols.len(),
                "env_vars": inputs.env_vars.len(),
                "redis_keys": inputs.redis_keys.len(),
                "deploy_targets": inputs.deploy_targets.len(),
            },
        }),
        json!({
            "name": "graph_followups",
            "purpose": "Structural queries to run before editing high-impact files or symbols",
            "items": inputs.graph_queries.iter().take(if full { 8 } else { 4 }).map(|item| context_zone_ref(item, "graph_query", "graph_queries", full)).collect::<Vec<_>>(),
        }),
        json!({
            "name": "verification",
            "purpose": "Suggested tests and doctors for the self-correction loop",
            "items": inputs.tests_to_run.iter().take(if full { 8 } else { 4 }).map(|item| context_zone_ref(item, "test_command", "tests_to_run", full))
                .chain(inputs.doctor_suggestions.iter().take(if full { 8 } else { 4 }).map(|item| context_zone_ref(item, "doctor_suggestion", "doctor_suggestions", full)))
                .collect::<Vec<_>>(),
        }),
        json!({
            "name": "risks",
            "purpose": "Context-specific hazards to account for in the plan and final review",
            "items": inputs.risk_notes.iter().take(8).map(|note| json!({
                "kind": "risk_note",
                "source_field": "risk_notes",
                "reason": note,
            })).collect::<Vec<_>>(),
        }),
    ]
}

fn context_zone_ref(item: &Value, kind: &str, source_field: &str, full: bool) -> Value {
    if full {
        json!({
            "kind": kind,
            "source_field": source_field,
            "path": item.get("path").cloned(),
            "line": item.get("line").cloned(),
            "tool": item.get("tool").cloned(),
            "command": item.get("command").cloned(),
            "anchor": item.get("anchor").cloned(),
            "reason": item.get("reason").cloned(),
        })
    } else {
        // Diet ref: the source section carries tool/command/reason; the
        // zone only needs to say where to look.
        json!({
            "kind": kind,
            "source_field": source_field,
            "path": item.get("path").cloned(),
            "line": item.get("line").cloned(),
        })
    }
}

fn execution_loop(
    tests_to_run: &[Value],
    doctor_suggestions: &[Value],
    graph_queries: &[Value],
) -> Vec<Value> {
    let mut loop_steps = vec![
        json!({
            "phase": "plan",
            "action": "Read instruction, memory, anchor, and ranked-file zones; keep the edit plan scoped to the selected working set.",
        }),
        json!({
            "phase": "inspect",
            "action": "Run graph follow-up queries before changing matched callable or import-heavy files.",
            "available_followups": graph_queries.len(),
        }),
        json!({
            "phase": "write_tests",
            "action": "Add or update the smallest test that proves the intended behavior before broad implementation.",
        }),
        json!({
            "phase": "implement",
            "action": "Make the minimal code and policy changes needed to satisfy the plan and tests.",
        }),
        json!({
            "phase": "verify",
            "action": "Run the suggested commands and iterate on failures until the scoped checks pass.",
            "suggested_commands": tests_to_run,
        }),
    ];

    if !doctor_suggestions.is_empty() {
        loop_steps.push(json!({
            "phase": "policy_check",
            "action": "Run the suggested LEIO doctors after implementation to catch profile-specific contract drift.",
            "suggested_doctors": doctor_suggestions,
        }));
    }

    loop_steps
}

fn selection_policy(limit: usize, selected_files: usize, intents: IntentHints) -> Value {
    json!({
        "strategy": "bounded_ranked_context",
        "reranker": "hybrid_sparse_idf_identifier_intent_segment_rrf_path_symbol_config_graph_optional_mtime",
        "max_files": limit,
        "selected_files": selected_files,
        "intent_routes_considered": intent_route_labels(intents),
        "noise_control": "Prefer the top ranked files and structural follow-ups instead of dumping the full repository.",
    })
}

fn instruction_kind(path: &Path) -> &'static str {
    match path.file_name().and_then(|value| value.to_str()) {
        Some(".cursorrules") => "cursor_rules",
        Some("codex.md") => "codex_instructions",
        Some("AGENTS.md") => "agent_instructions",
        Some("CLAUDE.md") => "claude_instructions",
        Some("GEMINI.md") => "gemini_instructions",
        Some("copilot-instructions.md") => "copilot_instructions",
        _ => "agent_instructions",
    }
}

fn memory_kind(path: &Path) -> &'static str {
    let normalized = normalize_relative_path(path);
    if normalized.contains("session") {
        "session_summary"
    } else {
        "memory_bank"
    }
}

fn instruction_scope(path: &Path) -> &'static str {
    if path
        .parent()
        .is_none_or(|parent| parent.as_os_str().is_empty())
        || normalize_relative_path(path).starts_with(".github/")
    {
        "repo"
    } else {
        "workspace_area"
    }
}

fn memory_match(path: &Path, task_tokens: &[String]) -> (usize, Vec<String>) {
    if task_tokens.is_empty() {
        return (1, Vec::new());
    }

    let Ok(src) = fs::read_to_string(path) else {
        return (1, Vec::new());
    };
    for (idx, line) in src.lines().enumerate() {
        let line_tokens = tokenize(line).into_iter().collect::<HashSet<_>>();
        let mut matched = task_tokens
            .iter()
            .filter(|token| line_tokens.contains(*token))
            .cloned()
            .collect::<Vec<_>>();
        matched.sort();
        matched.dedup();
        if !matched.is_empty() {
            return (idx + 1, matched);
        }
    }

    (1, Vec::new())
}

fn is_memory_doc_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("md" | "txt" | "json" | "jsonl")
    )
}

fn is_small_existing_file(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() <= MAX_DISCOVERY_DOC_BYTES)
}

fn looks_like_comment_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
        || trimmed.starts_with("<!--")
        || trimmed.starts_with("--")
}

fn looks_like_color_literal(value: &str) -> bool {
    matches!(value.len(), 3 | 6 | 8) && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn anchor_matches_task(anchor: &str, line: &str, task_tokens: &[String]) -> bool {
    if task_tokens.is_empty() {
        return true;
    }
    let anchor_tokens = tokenize(anchor);
    let line_tokens = tokenize(line);
    task_tokens.iter().any(|token| {
        anchor_tokens
            .iter()
            .any(|anchor_token| anchor_token == token)
            || line_tokens.iter().any(|line_token| line_token == token)
    })
}

fn normalize_relative_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn path_depth(path: &str) -> usize {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .count()
}

/// Adjacent raw tokens as hyphen/underscore path slugs.
///
/// Uses original order and keeps stop words so `LEIO Code` still becomes
/// `leio-code` even though `code` is dropped from IDF tokens.
fn compound_path_tokens(task: &str) -> Vec<String> {
    let ordered = tokenize_keep_stops(task);
    let mut out = Vec::new();
    for pair in ordered.windows(2) {
        if pair[0].len() < 3 || pair[1].len() < 3 {
            continue;
        }
        out.push(format!("{}-{}", pair[0], pair[1]));
        out.push(format!("{}_{}", pair[0], pair[1]));
    }
    out
}

/// Bonus when a task token (len ≥ 3) equals a path segment stem — e.g. task `context` boosts
/// `src/context/mod.rs` beyond incidental substring matches elsewhere.
fn path_segment_match_bonus(task_tokens: &[String], compounds: &[String], path: &str) -> i64 {
    let segments: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    let mut bonus = 0i64;
    for token in task_tokens {
        if token.len() < 3 {
            continue;
        }
        for seg in &segments {
            let stem = seg.rsplit_once('.').map(|(a, _)| a).unwrap_or(seg);
            if stem.eq_ignore_ascii_case(token.as_str()) {
                bonus += 8;
                break;
            }
        }
    }
    for compound in compounds {
        for seg in &segments {
            let stem = seg.rsplit_once('.').map(|(a, _)| a).unwrap_or(seg);
            if stem.eq_ignore_ascii_case(compound.as_str()) {
                bonus += 22;
                break;
            }
        }
    }
    bonus
}

fn sort_entities(entities: &mut [ScoredEntity]) {
    entities.sort_by(|left, right| {
        right.score.cmp(&left.score).then_with(|| {
            let left_path = left.value.get("path").and_then(Value::as_str).unwrap_or("");
            let right_path = right
                .value
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("");
            left_path.cmp(right_path)
        })
    });
}

fn match_score_idf(
    task_tokens: &[String],
    candidate: &str,
    exact_weight: i64,
    idf: &HashMap<String, f64>,
) -> i64 {
    let candidate_tokens = tokenize(candidate);
    if candidate_tokens.is_empty() || task_tokens.is_empty() {
        return 0;
    }

    let candidate_set = candidate_tokens
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let candidate_lower = candidate.to_ascii_lowercase();
    let mut score = 0i64;

    for token in task_tokens {
        let token_str = token.as_str();
        let mult = idf.get(token).copied().unwrap_or(1.0);
        let base = if candidate_set.contains(token_str) {
            exact_weight
        } else if candidate_tokens
            .iter()
            .any(|candidate_token| fuzzy_token_match(token_str, candidate_token))
        {
            exact_weight / 2
        } else if token_str.len() >= 4 && candidate_lower.contains(token_str) {
            exact_weight / 3
        } else {
            0
        };
        if base != 0 {
            score += (base as f64 * mult).round() as i64;
        }
    }

    score
}

fn fuzzy_token_match(left: &str, right: &str) -> bool {
    if left.len() < 3 || right.len() < 3 {
        return false;
    }
    left.starts_with(right) || right.starts_with(left)
}

fn task_tokens(task: &str) -> Vec<String> {
    let mut tokens = tokenize(task);
    tokens.sort();
    tokens.dedup();
    tokens
}

fn tokenize_keep_stops(value: &str) -> Vec<String> {
    let mut normalized = String::new();
    let mut previous_was_lower_or_digit = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            if ch.is_ascii_uppercase() && previous_was_lower_or_digit {
                normalized.push(' ');
            }
            normalized.push(ch.to_ascii_lowercase());
            previous_was_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        } else {
            normalized.push(' ');
            previous_was_lower_or_digit = false;
        }
    }

    normalized
        .split_whitespace()
        .filter(|token| token.len() >= 2)
        .map(str::to_string)
        .collect()
}

fn tokenize(value: &str) -> Vec<String> {
    tokenize_keep_stops(value)
        .into_iter()
        .filter(|token| !STOP_WORDS.contains(&token.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        AccessKind, FileRecord, RepoIndex, SourceLanguage, SymbolKind, SymbolOccurrence,
    };

    fn test_index() -> RepoIndex {
        RepoIndex {
            version: 1,
            root: "/tmp/repo".to_string(),
            indexed_at: "2026-01-01T00:00:00Z".to_string(),
            files: vec![
                FileRecord {
                    path: "src/doctors/self_contract.rs".to_string(),
                    language: SourceLanguage::Rust,
                    bytes: 1000,
                    modified_unix_ms: 0,
                    symbols: vec![SymbolOccurrence {
                        name: "SelfContractDoctor".to_string(),
                        kind: SymbolKind::Struct,
                        path: "src/doctors/self_contract.rs".to_string(),
                        line: 42,
                        language: SourceLanguage::Rust,
                        qual_name: None,
                    }],
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "apps-sdk/server.js".to_string(),
                    language: SourceLanguage::JavaScript,
                    bytes: 2000,
                    modified_unix_ms: 0,
                    symbols: vec![SymbolOccurrence {
                        name: "buildVigorosBridgeInfo".to_string(),
                        kind: SymbolKind::Function,
                        path: "apps-sdk/server.js".to_string(),
                        line: 12,
                        language: SourceLanguage::JavaScript,
                        qual_name: None,
                    }],
                    env_vars: vec![crate::model::EnvVarOccurrence {
                        name: "LEIO_VIGOROS_MCP_URL".to_string(),
                        access: AccessKind::Read,
                        path: "apps-sdk/server.js".to_string(),
                        line: 14,
                        language: SourceLanguage::JavaScript,
                    }],
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
            ],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        }
    }

    #[test]
    fn rank_fusion_does_not_reward_missing_channels() {
        let file = |path: &str| ScoredFile {
            path: path.into(),
            language: "rust".into(),
            score: 0,
            modified_unix_ms: 0,
            reasons: BTreeSet::new(),
            symbols: vec![],
            env_vars: vec![],
            redis_keys: vec![],
        };
        let ranked = finalize_file_ranking(
            vec![
                (
                    file("weak.rs"),
                    LayeredScore {
                        path: 1,
                        symbol: 0,
                        config: 0,
                    },
                ),
                (
                    file("matched.rs"),
                    LayeredScore {
                        path: 30,
                        symbol: 20,
                        config: 0,
                    },
                ),
            ],
            None,
        );
        assert_eq!(ranked[0].path, "matched.rs");
        // One real path match contributes one rank term, not three.
        assert!(ranked[1].score < 420);
        assert!(ranked[0].score > ranked[1].score * 2);
    }

    #[test]
    fn tokenize_splits_camel_case_and_ignores_stop_words() {
        assert_eq!(
            task_tokens("Fix the SelfContractDoctor packaging"),
            vec![
                "contract".to_string(),
                "doctor".to_string(),
                "fix".to_string(),
                "packaging".to_string(),
                "self".to_string(),
            ]
        );
    }

    #[test]
    fn context_bundle_ranks_files_and_suggests_followups() {
        let index = test_index();
        let env = build_context_bundle(
            &index,
            Path::new("/tmp/repo"),
            "fix Apps SDK VIGOROS bridge env and self contract packaging",
            5,
            false,
        );

        assert_eq!(env.kind, "context");
        assert!(env.summary.contains("context bundle"));
        assert!(env.warnings.is_empty());
        let bundle = &env.entities[0];
        let files = bundle
            .get("files_to_read")
            .and_then(Value::as_array)
            .expect("files");
        assert_eq!(
            files[0].get("path").and_then(Value::as_str),
            Some("apps-sdk/server.js")
        );
        assert!(
            bundle
                .get("tests_to_run")
                .and_then(Value::as_array)
                .expect("tests")
                .iter()
                .any(|item| item
                    .get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|command| command.contains("npm run check")))
        );
        let zone_names = bundle
            .get("context_zones")
            .and_then(Value::as_array)
            .expect("context zones")
            .iter()
            .filter_map(|item| item.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(
            zone_names,
            vec![
                "instructions",
                "memory",
                "anchors",
                "ranked_files",
                "graph_followups",
                "verification",
                "risks",
            ]
        );
    }

    #[test]
    fn context_bundle_discovers_instructions_memory_and_verification_anchors() {
        let root = std::env::temp_dir().join(format!(
            "leio-code-context-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join(".leio-code")).expect("memory dir");
        std::fs::create_dir_all(root.join("apps-sdk")).expect("apps dir");
        std::fs::write(
            root.join("AGENTS.md"),
            "Run focused tests before broad implementation.",
        )
        .expect("agents");
        std::fs::write(
            root.join(".leio-code/memory.md"),
            "Auth edge cases prefer canonical token verification.",
        )
        .expect("memory");
        std::fs::write(
            root.join("apps-sdk/server.js"),
            "// #auth-edge-case verifies canonical token handling\nfunction handler() {}\n",
        )
        .expect("source");

        let mut index = test_index();
        index.root = root.display().to_string();
        let env = build_context_bundle(
            &index,
            &root,
            "fix Apps SDK auth edge case canonical token handling",
            5,
            false,
        );
        let bundle = &env.entities[0];

        let instructions = bundle
            .get("instruction_sources")
            .and_then(Value::as_array)
            .expect("instructions");
        assert!(instructions.iter().any(|item| {
            item.get("path")
                .and_then(Value::as_str)
                .is_some_and(|path| path == "AGENTS.md")
        }));

        let memory = bundle
            .get("memory_sources")
            .and_then(Value::as_array)
            .expect("memory");
        assert!(memory.iter().any(|item| {
            item.get("path")
                .and_then(Value::as_str)
                .is_some_and(|path| path == ".leio-code/memory.md")
        }));

        let anchors = bundle
            .get("verification_anchors")
            .and_then(Value::as_array)
            .expect("anchors");
        assert!(anchors.iter().any(|item| {
            item.get("anchor")
                .and_then(Value::as_str)
                .is_some_and(|anchor| anchor == "#auth-edge-case")
        }));

        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn path_segment_match_bonus_matches_file_or_dir_stem() {
        let task = "refine context ranking logic";
        let tokens = task_tokens(task);
        let compounds = compound_path_tokens(task);
        assert!(path_segment_match_bonus(&tokens, &compounds, "src/context.rs") >= 8);
        assert_eq!(
            path_segment_match_bonus(&tokens, &compounds, "src/foo/bar.rs"),
            0
        );
    }

    #[test]
    fn path_segment_match_bonus_joins_adjacent_tokens_into_hyphen_stems() {
        let task = "fix unified Example Ops auth brokering";
        let tokens = task_tokens(task);
        let compounds = compound_path_tokens(task);
        let example = path_segment_match_bonus(
            &tokens,
            &compounds,
            "example-ops/src/app/api/auth/shared.ts",
        );
        let assurant = path_segment_match_bonus(
            &tokens,
            &compounds,
            "assurant-ops/src/app/api/auth/me/route.ts",
        );
        assert!(example >= 22, "example-ops stem should match Example+Ops");
        assert!(example > assurant);
    }

    #[test]
    fn path_segment_match_bonus_recovers_leio_code_after_token_sort() {
        let task = "harden LEIO Code context ranking";
        let tokens = task_tokens(task);
        let compounds = compound_path_tokens(task);
        assert!(
            tokens.iter().all(|token| token != "code"),
            "code stays a stop word for IDF"
        );
        assert!(
            compounds.iter().any(|c| c == "leio-code"),
            "compounds={compounds:?}"
        );
        assert_eq!(
            path_segment_match_bonus(&tokens, &compounds, "leio-code/src/foo.rs"),
            22
        );
        assert_eq!(
            path_segment_match_bonus(&tokens, &compounds, "src/code-graph.rs"),
            0
        );
        assert_eq!(
            path_segment_match_bonus(&tokens, &compounds, "src/unrelated/mod.rs"),
            0
        );
    }

    #[test]
    fn compound_path_tokens_table() {
        let leio = compound_path_tokens("LEIO Code");
        assert!(leio.contains(&"leio-code".to_string()));
        assert!(leio.contains(&"leio_code".to_string()));
        let mcp = compound_path_tokens("MCP Tool");
        assert!(mcp.contains(&"mcp-tool".to_string()));
        assert!(!compound_path_tokens("Code LEIO").contains(&"leio-code".to_string()));
        assert!(!compound_path_tokens("LEIO the Code").contains(&"leio-code".to_string()));
    }

    #[test]
    fn rank_files_breaks_score_ties_with_recency() {
        let index = RepoIndex {
            version: 1,
            root: "/tmp/repo".to_string(),
            indexed_at: "2026-01-01T00:00:00Z".to_string(),
            files: vec![
                FileRecord {
                    path: "z/old.rs".to_string(),
                    language: SourceLanguage::Rust,
                    bytes: 10,
                    modified_unix_ms: 100,
                    symbols: vec![SymbolOccurrence {
                        name: "marker".to_string(),
                        kind: SymbolKind::Function,
                        path: "z/old.rs".to_string(),
                        line: 1,
                        language: SourceLanguage::Rust,
                        qual_name: None,
                    }],
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "z/new.rs".to_string(),
                    language: SourceLanguage::Rust,
                    bytes: 10,
                    modified_unix_ms: 999_999,
                    symbols: vec![SymbolOccurrence {
                        name: "marker".to_string(),
                        kind: SymbolKind::Function,
                        path: "z/new.rs".to_string(),
                        line: 1,
                        language: SourceLanguage::Rust,
                        qual_name: None,
                    }],
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
            ],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };
        let tokens = task_tokens("marker");
        let idf = sparse_token_idf(&index, &tokens);
        let intents = classify_intents("marker", &tokens);
        let needles = extract_identifier_needles("marker");
        let ranked = rank_files(&index, &tokens, "marker", 10, &idf, intents, &needles, None);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].path, "z/new.rs");
        assert_eq!(ranked[1].path, "z/old.rs");
    }

    #[test]
    fn intent_route_prefers_tests_when_task_mentions_testing() {
        let index = RepoIndex {
            version: 1,
            root: "/tmp/repo".to_string(),
            indexed_at: "2026-01-01T00:00:00Z".to_string(),
            files: vec![
                FileRecord {
                    path: "src/helper.rs".to_string(),
                    language: SourceLanguage::Rust,
                    bytes: 10,
                    modified_unix_ms: 500,
                    symbols: vec![SymbolOccurrence {
                        name: "helper".to_string(),
                        kind: SymbolKind::Function,
                        path: "src/helper.rs".to_string(),
                        line: 1,
                        language: SourceLanguage::Rust,
                        qual_name: None,
                    }],
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "tests/helper.rs".to_string(),
                    language: SourceLanguage::Rust,
                    bytes: 10,
                    modified_unix_ms: 500,
                    symbols: vec![SymbolOccurrence {
                        name: "helper".to_string(),
                        kind: SymbolKind::Function,
                        path: "tests/helper.rs".to_string(),
                        line: 1,
                        language: SourceLanguage::Rust,
                        qual_name: None,
                    }],
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
            ],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };
        let task = "fix helper unit test regression";
        let tokens = task_tokens(task);
        let idf = sparse_token_idf(&index, &tokens);
        let intents = classify_intents(&task.to_ascii_lowercase(), &tokens);
        assert!(intents.wants_tests);
        let needles = extract_identifier_needles(task);
        let ranked = rank_files(&index, &tokens, task, 10, &idf, intents, &needles, None);
        assert_eq!(ranked[0].path, "tests/helper.rs");
    }

    #[test]
    fn identifier_needle_boosts_file_defining_that_symbol() {
        let index = RepoIndex {
            version: 1,
            root: "/tmp/repo".to_string(),
            indexed_at: "2026-01-01T00:00:00Z".to_string(),
            files: vec![
                FileRecord {
                    path: "src/other.rs".to_string(),
                    language: SourceLanguage::Rust,
                    bytes: 10,
                    modified_unix_ms: 100,
                    symbols: vec![SymbolOccurrence {
                        name: "UnrelatedFn".to_string(),
                        kind: SymbolKind::Function,
                        path: "src/other.rs".to_string(),
                        line: 1,
                        language: SourceLanguage::Rust,
                        qual_name: None,
                    }],
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "src/target.rs".to_string(),
                    language: SourceLanguage::Rust,
                    bytes: 10,
                    modified_unix_ms: 100,
                    symbols: vec![SymbolOccurrence {
                        name: "SelfContractDoctor".to_string(),
                        kind: SymbolKind::Struct,
                        path: "src/target.rs".to_string(),
                        line: 2,
                        language: SourceLanguage::Rust,
                        qual_name: None,
                    }],
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
            ],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };
        let task = "refactor SelfContractDoctor packaging";
        let tokens = task_tokens(task);
        let idf = sparse_token_idf(&index, &tokens);
        let intents = classify_intents(&task.to_ascii_lowercase(), &tokens);
        let needles = extract_identifier_needles(task);
        assert!(
            needles.iter().any(|n| n.contains("SelfContractDoctor")),
            "needles={needles:?}"
        );
        let ranked = rank_files(&index, &tokens, task, 10, &idf, intents, &needles, None);
        assert_eq!(ranked[0].path, "src/target.rs");
    }

    #[test]
    fn default_bundle_is_a_diet_and_full_restores_the_exhaustive_shape() {
        let index = test_index();
        let root = temp_context_root();
        let diet = build_context_bundle(
            &index,
            &root,
            "fix Apps SDK VIGOROS bridge env and self contract packaging",
            5,
            false,
        );
        let bundle = &diet.entities[0];

        // Duplicated legacy keys stay out of the default bundle.
        assert!(bundle.get("agent_instructions").is_none());
        assert!(bundle.get("memory_banks").is_none());
        // Meta carries no capabilities block in the diet.
        assert!(
            diet.meta
                .as_ref()
                .and_then(|meta| meta.get("workspace_capabilities"))
                .is_none_or(|value| value.is_null())
        );
        // Per-file entity lists are capped with counts alongside.
        for file in bundle
            .get("files_to_read")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
        {
            let symbols = file
                .get("symbols")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            assert!(symbols <= 3, "diet caps per-file symbols at 3");
            assert!(file.get("symbol_count").is_some());
        }
        // Zone items are compact references, not second copies.
        for zone in bundle
            .get("context_zones")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
        {
            for item in zone
                .get("items")
                .and_then(Value::as_array)
                .unwrap_or(&Vec::new())
            {
                assert!(
                    item.get("reason").is_none() || zone.get("name") == Some(&json!("risks")),
                    "diet zone refs drop the reason payload"
                );
            }
        }

        let exhaustive = build_context_bundle(
            &index,
            &root,
            "fix Apps SDK VIGOROS bridge env and self contract packaging",
            5,
            true,
        );
        let full_bundle = &exhaustive.entities[0];
        assert!(
            exhaustive
                .meta
                .as_ref()
                .and_then(|meta| meta.get("workspace_capabilities"))
                .is_some_and(|value| !value.is_null())
        );
        assert!(full_bundle.get("agent_instructions").is_some());

        let _ = fs::remove_dir_all(&root);
    }

    fn temp_context_root() -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("leio-context-diet-{nanos}"));
        fs::create_dir_all(&dir).expect("temp root");
        dir
    }

    #[test]
    fn bundle_surfaces_retrieval_signals() {
        let index = test_index();
        let env = build_context_bundle(
            &index,
            Path::new("/tmp/repo"),
            "deploy docker smoke test for JWT auth",
            5,
            false,
        );
        let bundle = &env.entities[0];
        let signals = bundle.get("retrieval_signals").expect("retrieval_signals");
        assert_eq!(signals.get("sparse_idf_weighting"), Some(&json!(true)));
        let routes = signals
            .get("intent_routes_considered")
            .and_then(Value::as_array)
            .expect("routes");
        assert!(!routes.is_empty());
        assert_eq!(signals.get("graph_cache_loaded"), Some(&json!(false)));
        assert!(
            signals
                .get("code_graph_refresh_hint")
                .and_then(Value::as_str)
                .is_some_and(|hint| hint.contains("export code-graph")),
            "expected actionable hint when graph cache is absent, got {:?}",
            signals.get("code_graph_refresh_hint")
        );
    }
}
