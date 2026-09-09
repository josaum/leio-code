//! Structural graph queries over the local code graph.
//!
//! Sister module to [`crate::code_graph`]: where `code_graph` *writes* the
//! N-Quads graph + manifest + structural query-cache, this module *reads* them
//! to answer agent-facing questions:
//!
//! - `callers-of` / `callees-of` / `callsites-of` — symbol-level call topology
//! - `symbols-in` — symbol inventory for a file
//! - `imports-in` / `importers-of` — import statements and reverse lookup by
//!   raw statement, module specifier, imported name, or file path (query cache)
//! - `resolved-imports-in` / `resolved-importers-of` — canonical, file-keyed
//!   import edges with unique-symbol resolution
//! - `dead-code` — symbols with zero callers (size-bounded heuristic)
//!
//! Hot path reads the JSON `query-cache.json` directly. When the cache is
//! missing or stale (see [`code_graph_refresh_reason`]) the module falls back
//! to loading the N-Quads into an in-memory Oxigraph store and answering via
//! SPARQL. Cache-first keeps the agent loop fast. The Oxigraph fallback for
//! `importers-of` matches exact `importsPath` only; specifier, imported name,
//! and file path lookup need the query cache.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::knowledge_graph::SparqlQueryExt;
use anyhow::{Context, Result};
use oxigraph::io::RdfFormat;
use oxigraph::model::Term;
use oxigraph::sparql::QueryResults;
use oxigraph::store::Store;
use serde_json::json;

use crate::code_graph::{
    CachedGraphCallsite, CachedGraphImport, CachedGraphImporter, CachedGraphNeighbor,
    CachedGraphSymbol, CodeGraphQueryCache, QUERY_CACHE_VERSION, code_graph_refresh_reason,
    default_code_graph_cache_path, default_code_graph_manifest_path, default_code_graph_output_dir,
    export_code_graph,
};
use crate::doctors::utils::query_id;
use crate::import_lookup::{
    collapse_import_match_kinds, import_detail_hit_kind, import_query_aliases,
    needle_looks_like_path,
};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

const PROV_NS: &str = "http://www.w3.org/ns/prov#";

/// RDF vocabulary prefix used by Oxigraph SPARQL fallbacks.
///
/// Honors `LEIO_CODE_RDF_NAMESPACE`. Config `[rdf] namespace` is applied at
/// export time via [`crate::config::code_rdf_namespace`]; SPARQL must match
/// the exported graph, so the env-or-default resolver is used here.
fn active_code_ns() -> String {
    crate::config::code_rdf_namespace_from_env()
}

#[derive(Debug, Clone, Copy)]
pub enum GraphDirection {
    CallersOf,
    CalleesOf,
}

impl GraphDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CallersOf => "callers-of",
            Self::CalleesOf => "callees-of",
        }
    }
}

#[derive(Debug, Clone)]
struct ResolvedSymbol {
    iri: String,
    name: String,
    qual_name: String,
    kind: String,
    path: String,
}

#[derive(Debug, Clone)]
struct GraphNeighbor {
    iri: String,
    name: String,
    qual_name: String,
    kind: String,
    path: String,
    line: Option<usize>,
}

#[derive(Debug, Clone)]
struct ResolvedFile {
    iri: String,
    path: String,
    language: String,
}

#[derive(Debug, Clone)]
struct GraphCallsite {
    owner_iri: String,
    owner_kind: String,
    owner_name: String,
    owner_qual_name: String,
    path: String,
    line: Option<usize>,
    expr: String,
}

#[derive(Debug, Clone)]
struct GraphImporter {
    raw_import: String,
    file_iri: String,
    path: String,
    language: String,
    line: Option<usize>,
}

#[derive(Debug, Clone)]
struct GraphImportDetail {
    raw: String,
    syntax_kind: String,
    module_specifiers: Vec<String>,
    imported_names: Vec<String>,
    line: Option<usize>,
    resolution_kind: String,
    candidate_paths: Vec<String>,
    candidate_symbols: Vec<GraphNeighbor>,
}

fn is_callable_kind(kind: &str) -> bool {
    matches!(kind, "function" | "method" | "class" | "interface")
}

pub fn query_call_graph(
    index: &RepoIndex,
    root: &Path,
    direction: GraphDirection,
    needle: &str,
) -> Result<QueryEnvelope> {
    let started = Instant::now();
    let (graph_path, query_cache_path, warnings) = ensure_graph_artifacts(index, root)?;
    let cache = load_or_refresh_graph_cache(index, root, &query_cache_path)?;
    let candidates = if let Some(cache) = &cache {
        resolve_candidates_from_cache(cache, needle)
    } else {
        let store = load_graph_store(&graph_path)?;
        resolve_candidates(&store, needle)?
    };
    if candidates.is_empty() {
        return Ok(QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("graph"),
            kind: "graph".to_string(),
            summary: format!("symbol `{}` not found in current code graph", needle),
            confidence: 0.0,
            entities: Vec::new(),
            evidence: graph_artifact_evidence(&graph_path, &query_cache_path),
            warnings,
            meta: None,
            timing_ms: started.elapsed().as_millis(),
        });
    }

    if candidates.len() > 1 {
        return Ok(QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("graph"),
            kind: "graph".to_string(),
            summary: format!(
                "symbol `{}` is ambiguous in current code graph ({} candidates)",
                needle,
                candidates.len()
            ),
            confidence: 0.2,
            entities: candidates
                .iter()
                .map(|candidate| {
                    json!({
                        "symbol": candidate.iri,
                        "name": candidate.name,
                        "qual_name": candidate.qual_name,
                        "kind": candidate.kind,
                        "path": candidate.path,
                    })
                })
                .collect(),
            evidence: graph_artifact_evidence(&graph_path, &query_cache_path),
            warnings,
            meta: None,
            timing_ms: started.elapsed().as_millis(),
        });
    }

    let symbol = &candidates[0];
    let neighbors = if let Some(cache) = &cache {
        fetch_neighbors_from_cache(cache, direction, &symbol.iri)
    } else {
        let store = load_graph_store(&graph_path)?;
        fetch_neighbors(&store, direction, &symbol.iri)?
    };
    let mut warnings = warnings;
    if neighbors.is_empty() && !is_callable_kind(&symbol.kind) {
        warnings.push(format!(
            "`{}` is a {}; the call graph only tracks function/method invocations. \
Use `find symbol {}` for type usages, or query an associated function (e.g. `{}::new`).",
            symbol.qual_name, symbol.kind, symbol.name, symbol.name,
        ));
    }
    let summary = match direction {
        GraphDirection::CallersOf => format!(
            "symbol `{}` has {} callers in current code graph",
            symbol.qual_name,
            neighbors.len()
        ),
        GraphDirection::CalleesOf => format!(
            "symbol `{}` has {} callees in current code graph",
            symbol.qual_name,
            neighbors.len()
        ),
    };

    let mut entities = vec![json!({
        "query": needle,
        "direction": direction.as_str(),
        "resolved_symbol": {
            "symbol": symbol.iri,
            "name": symbol.name,
            "qual_name": symbol.qual_name,
            "kind": symbol.kind,
            "path": symbol.path,
        },
    })];
    entities.extend(neighbors.iter().map(|neighbor| {
        json!({
            "symbol": neighbor.iri,
            "name": neighbor.name,
            "qual_name": neighbor.qual_name,
            "kind": neighbor.kind,
            "path": neighbor.path,
            "line": neighbor.line,
        })
    }));

    let mut evidence = graph_artifact_evidence(&graph_path, &query_cache_path);
    evidence.extend(neighbors.iter().map(|neighbor| EvidenceItem {
        kind: "graph_neighbor".to_string(),
        path: neighbor.path.clone(),
        line: neighbor.line,
        detail: format!("{} {}", neighbor.kind, neighbor.qual_name),
    }));

    Ok(QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("graph"),
        kind: "graph".to_string(),
        summary,
        confidence: if neighbors.is_empty() { 0.75 } else { 0.95 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    })
}

pub fn query_callsites_of(index: &RepoIndex, root: &Path, needle: &str) -> Result<QueryEnvelope> {
    let started = Instant::now();
    let (graph_path, query_cache_path, warnings) = ensure_graph_artifacts(index, root)?;
    let cache = load_or_refresh_graph_cache(index, root, &query_cache_path)?;
    let candidates = if let Some(cache) = &cache {
        resolve_candidates_from_cache(cache, needle)
    } else {
        let store = load_graph_store(&graph_path)?;
        resolve_candidates(&store, needle)?
    };
    if candidates.is_empty() {
        return Ok(QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("graph"),
            kind: "graph".to_string(),
            summary: format!("symbol `{}` not found in current code graph", needle),
            confidence: 0.0,
            entities: Vec::new(),
            evidence: graph_artifact_evidence(&graph_path, &query_cache_path),
            warnings,
            meta: None,
            timing_ms: started.elapsed().as_millis(),
        });
    }

    if candidates.len() > 1 {
        return Ok(QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("graph"),
            kind: "graph".to_string(),
            summary: format!(
                "symbol `{}` is ambiguous in current code graph ({} candidates)",
                needle,
                candidates.len()
            ),
            confidence: 0.2,
            entities: candidates
                .iter()
                .map(|candidate| {
                    json!({
                        "symbol": candidate.iri,
                        "name": candidate.name,
                        "qual_name": candidate.qual_name,
                        "kind": candidate.kind,
                        "path": candidate.path,
                    })
                })
                .collect(),
            evidence: graph_artifact_evidence(&graph_path, &query_cache_path),
            warnings,
            meta: None,
            timing_ms: started.elapsed().as_millis(),
        });
    }

    let symbol = &candidates[0];
    let callsites = if let Some(cache) = &cache {
        fetch_callsites_from_cache(cache, &symbol.iri)
    } else {
        let store = load_graph_store(&graph_path)?;
        fetch_callsites(&store, &symbol.iri)?
    };
    let mut entities = vec![json!({
        "query": needle,
        "kind": "callsites-of",
        "resolved_symbol": {
            "symbol": symbol.iri,
            "name": symbol.name,
            "qual_name": symbol.qual_name,
            "kind": symbol.kind,
            "path": symbol.path,
        },
    })];
    entities.extend(callsites.iter().map(|callsite| {
        json!({
            "owner": callsite.owner_iri,
            "owner_kind": callsite.owner_kind,
            "owner_name": callsite.owner_name,
            "owner_qual_name": callsite.owner_qual_name,
            "path": callsite.path,
            "line": callsite.line,
            "expr": callsite.expr,
        })
    }));

    let mut evidence = graph_artifact_evidence(&graph_path, &query_cache_path);
    evidence.extend(callsites.iter().map(|callsite| EvidenceItem {
        kind: "graph_callsite".to_string(),
        path: callsite.path.clone(),
        line: callsite.line,
        detail: if callsite.owner_qual_name.is_empty() {
            format!("callsite {}", callsite.expr)
        } else {
            format!("{} calls via {}", callsite.owner_qual_name, callsite.expr)
        },
    }));

    Ok(QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("graph"),
        kind: "graph".to_string(),
        summary: format!(
            "symbol `{}` has {} callsites in current code graph",
            symbol.qual_name,
            callsites.len()
        ),
        confidence: if callsites.is_empty() { 0.75 } else { 0.95 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    })
}

pub fn query_symbols_in(index: &RepoIndex, root: &Path, needle: &str) -> Result<QueryEnvelope> {
    let started = Instant::now();
    let (graph_path, query_cache_path, warnings) = ensure_graph_artifacts(index, root)?;
    let cache = load_or_refresh_graph_cache(index, root, &query_cache_path)?;
    let files = if let Some(cache) = &cache {
        resolve_files_from_cache(cache, needle)
    } else {
        let store = load_graph_store(&graph_path)?;
        resolve_files(&store, needle)?
    };
    if files.is_empty() {
        return Ok(QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("graph"),
            kind: "graph".to_string(),
            summary: format!("file `{}` not found in current code graph", needle),
            confidence: 0.0,
            entities: Vec::new(),
            evidence: graph_artifact_evidence(&graph_path, &query_cache_path),
            warnings,
            meta: None,
            timing_ms: started.elapsed().as_millis(),
        });
    }

    if files.len() > 1 {
        return Ok(QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("graph"),
            kind: "graph".to_string(),
            summary: format!(
                "file `{}` is ambiguous in current code graph ({} candidates)",
                needle,
                files.len()
            ),
            confidence: 0.2,
            entities: files
                .iter()
                .map(|file| {
                    json!({
                        "file": file.iri,
                        "path": file.path,
                        "language": file.language,
                    })
                })
                .collect(),
            evidence: graph_artifact_evidence(&graph_path, &query_cache_path),
            warnings,
            meta: None,
            timing_ms: started.elapsed().as_millis(),
        });
    }

    let file = &files[0];
    let symbols = if let Some(cache) = &cache {
        fetch_symbols_in_file_from_cache(cache, &file.iri)
    } else {
        let store = load_graph_store(&graph_path)?;
        fetch_symbols_in_file(&store, &file.iri)?
    };
    let mut entities = vec![json!({
        "query": needle,
        "kind": "symbols-in",
        "resolved_file": {
            "file": file.iri,
            "path": file.path,
            "language": file.language,
        },
    })];
    entities.extend(symbols.iter().map(|symbol| {
        json!({
            "symbol": symbol.iri,
            "name": symbol.name,
            "qual_name": symbol.qual_name,
            "kind": symbol.kind,
            "path": symbol.path,
            "line": symbol.line,
        })
    }));

    let mut evidence = graph_artifact_evidence(&graph_path, &query_cache_path);
    evidence.extend(symbols.iter().map(|symbol| EvidenceItem {
        kind: "graph_symbol".to_string(),
        path: symbol.path.clone(),
        line: symbol.line,
        detail: format!("{} {}", symbol.kind, symbol.qual_name),
    }));

    Ok(QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("graph"),
        kind: "graph".to_string(),
        summary: format!(
            "file `{}` defines {} symbols in current code graph",
            file.path,
            symbols.len()
        ),
        confidence: if symbols.is_empty() { 0.75 } else { 0.95 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    })
}

pub fn query_imports_in(index: &RepoIndex, root: &Path, needle: &str) -> Result<QueryEnvelope> {
    query_imports_in_internal(index, root, needle, false)
}

pub fn query_resolved_imports_in(
    index: &RepoIndex,
    root: &Path,
    needle: &str,
) -> Result<QueryEnvelope> {
    query_imports_in_internal(index, root, needle, true)
}

fn query_imports_in_internal(
    index: &RepoIndex,
    root: &Path,
    needle: &str,
    resolved: bool,
) -> Result<QueryEnvelope> {
    let started = Instant::now();
    let (graph_path, query_cache_path, warnings) = ensure_graph_artifacts(index, root)?;
    let cache = load_or_refresh_graph_cache(index, root, &query_cache_path)?;
    let files = if let Some(cache) = &cache {
        resolve_files_from_cache(cache, needle)
    } else {
        let store = load_graph_store(&graph_path)?;
        resolve_files(&store, needle)?
    };
    if files.is_empty() {
        return Ok(QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("graph"),
            kind: "graph".to_string(),
            summary: format!("file `{}` not found in current code graph", needle),
            confidence: 0.0,
            entities: Vec::new(),
            evidence: graph_artifact_evidence(&graph_path, &query_cache_path),
            warnings,
            meta: None,
            timing_ms: started.elapsed().as_millis(),
        });
    }

    if files.len() > 1 {
        return Ok(QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("graph"),
            kind: "graph".to_string(),
            summary: format!(
                "file `{}` is ambiguous in current code graph ({} candidates)",
                needle,
                files.len()
            ),
            confidence: 0.2,
            entities: files
                .iter()
                .map(|file| {
                    json!({
                        "file": file.iri,
                        "path": file.path,
                        "language": file.language,
                    })
                })
                .collect(),
            evidence: graph_artifact_evidence(&graph_path, &query_cache_path),
            warnings,
            meta: None,
            timing_ms: started.elapsed().as_millis(),
        });
    }

    let file = &files[0];
    let imports = if let Some(cache) = &cache {
        fetch_imports_in_file_from_cache(cache, &file.iri)
    } else {
        let store = load_graph_store(&graph_path)?;
        fetch_imports_in_file(&store, &file.iri)?
    };

    let mut entities = vec![json!({
        "query": needle,
        "kind": if resolved { "resolved-imports-in" } else { "imports-in" },
        "resolved_file": {
            "file": file.iri,
            "path": file.path,
            "language": file.language,
        },
    })];
    entities.extend(imports.iter().map(|raw_import| {
        if resolved {
            json!({
                "path": file.path,
                "import": raw_import.raw,
                "syntax_kind": raw_import.syntax_kind,
                "module_specifiers": raw_import.module_specifiers,
                "imported_names": raw_import.imported_names,
                "line": raw_import.line,
                "resolution_kind": raw_import.resolution_kind,
                "candidate_paths": raw_import.candidate_paths,
                "candidate_symbols": raw_import.candidate_symbols.iter().map(|symbol| json!({
                    "symbol": symbol.iri,
                    "name": symbol.name,
                    "qual_name": symbol.qual_name,
                    "kind": symbol.kind,
                    "path": symbol.path,
                    "line": symbol.line,
                })).collect::<Vec<_>>(),
            })
        } else {
            json!({
                "path": file.path,
                "import": raw_import.raw,
                "syntax_kind": raw_import.syntax_kind,
                "module_specifiers": raw_import.module_specifiers,
                "imported_names": raw_import.imported_names,
                "line": raw_import.line,
            })
        }
    }));

    let mut evidence = graph_artifact_evidence(&graph_path, &query_cache_path);
    evidence.extend(imports.iter().map(|raw_import| EvidenceItem {
        kind: "graph_import".to_string(),
        path: file.path.clone(),
        line: raw_import.line,
        detail: format!("imports {}", raw_import.raw),
    }));

    Ok(QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("graph"),
        kind: "graph".to_string(),
        summary: format!(
            "file `{}` declares {} {}imports in current code graph",
            file.path,
            imports.len(),
            if resolved { "resolved " } else { "" }
        ),
        confidence: if imports.is_empty() { 0.75 } else { 0.95 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    })
}

pub fn query_importers_of(index: &RepoIndex, root: &Path, needle: &str) -> Result<QueryEnvelope> {
    query_importers_of_internal(index, root, needle, false)
}

/// Symbols flagged as "dead": exported, with at most `threshold` callers AND zero importers
/// for the file that defines them. Skips well-known framework entry-point patterns so that
/// `main`, `default` exports, Next.js routes, lifecycle hooks, and test functions are not
/// mis-flagged as orphans.
pub fn query_dead_code(index: &RepoIndex, root: &Path, threshold: usize) -> Result<QueryEnvelope> {
    let started = Instant::now();
    let (graph_path, query_cache_path, warnings) = ensure_graph_artifacts(index, root)?;
    let cache = match load_or_refresh_graph_cache(index, root, &query_cache_path)? {
        Some(cache) => cache,
        None => {
            return Ok(QueryEnvelope {
                schema_version: crate::model::SCHEMA_VERSION.to_string(),
                query_id: query_id("graph"),
                kind: "graph".to_string(),
                summary: "code graph query cache unavailable; cannot enumerate dead code"
                    .to_string(),
                confidence: 0.0,
                entities: Vec::new(),
                evidence: graph_artifact_evidence(&graph_path, &query_cache_path),
                warnings,
                meta: None,
                timing_ms: started.elapsed().as_millis(),
            });
        }
    };

    let mut findings: Vec<DeadCodeFinding> = Vec::new();
    let mut skipped = 0usize;
    let mut symbols: Vec<&CachedGraphSymbol> = cache.symbols.values().collect();
    symbols.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line.unwrap_or(0).cmp(&right.line.unwrap_or(0)))
            .then_with(|| left.qual_name.cmp(&right.qual_name))
    });

    for symbol in symbols {
        if !is_callable_kind(&symbol.kind) {
            // Only function/method/class/interface participate in the call graph;
            // other kinds (constants, modules, type aliases) cannot meaningfully
            // be classified as "dead callers" with the current edges.
            continue;
        }
        if !looks_exported(&symbol.name, &symbol.qual_name) {
            skipped += 1;
            continue;
        }
        if matches_entrypoint_pattern(symbol, &cache) {
            skipped += 1;
            continue;
        }

        let caller_count = cache
            .callers_by_symbol
            .get(&symbol.iri)
            .map(|callers| callers.len())
            .unwrap_or(0);
        if caller_count > threshold {
            continue;
        }

        let importer_count = file_importer_count(&cache, &symbol.path);
        if importer_count > 0 {
            continue;
        }

        let language = cache
            .file_lookup_path
            .get(&symbol.path)
            .and_then(|iris| iris.first())
            .and_then(|iri| cache.files.get(iri))
            .map(|file| file.language.clone())
            .unwrap_or_default();

        let reason = if caller_count == 0 {
            format!(
                "0 callers and 0 importers for `{}` ({})",
                symbol.path, language
            )
        } else {
            format!(
                "{} callers (<= threshold {}) and 0 importers for `{}` ({})",
                caller_count, threshold, symbol.path, language
            )
        };

        findings.push(DeadCodeFinding {
            symbol: symbol.clone(),
            language,
            caller_count,
            importer_count,
            reason,
        });
    }

    let mut entities = vec![json!({
        "query": "dead-code",
        "kind": "dead-code",
        "threshold": threshold,
        "candidate_symbols": cache.symbols.len(),
        "skipped_entrypoint_or_private": skipped,
        "findings": findings.len(),
    })];
    entities.extend(findings.iter().map(|finding| {
        json!({
            "symbol_name": finding.symbol.name,
            "qual_name": finding.symbol.qual_name,
            "kind": finding.symbol.kind,
            "file": finding.symbol.path,
            "line": finding.symbol.line,
            "language": finding.language,
            "caller_count": finding.caller_count,
            "importer_count": finding.importer_count,
            "reason": finding.reason,
        })
    }));

    let mut evidence = graph_artifact_evidence(&graph_path, &query_cache_path);
    evidence.extend(findings.iter().map(|finding| EvidenceItem {
        kind: "graph_dead_code".to_string(),
        path: finding.symbol.path.clone(),
        line: finding.symbol.line,
        detail: format!("{} {}", finding.symbol.kind, finding.symbol.qual_name),
    }));

    Ok(QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("graph"),
        kind: "graph".to_string(),
        summary: format!(
            "found {} dead-code candidates (callers <= {}, importers = 0); {} symbols inspected, {} skipped as entrypoints/private",
            findings.len(),
            threshold,
            cache.symbols.len(),
            skipped,
        ),
        confidence: if findings.is_empty() { 0.95 } else { 0.85 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "threshold": threshold,
            "candidate_symbols": cache.symbols.len(),
            "skipped_entrypoint_or_private": skipped,
            "finding_count": findings.len(),
        })),
        timing_ms: started.elapsed().as_millis(),
    })
}

#[derive(Debug, Clone)]
struct DeadCodeFinding {
    symbol: CachedGraphSymbol,
    language: String,
    caller_count: usize,
    importer_count: usize,
    reason: String,
}

/// Returns true when a symbol name is exposed beyond the file that defines it.
/// We approximate "exported" with a conservative public-name heuristic: leading
/// underscores (Python `_helper`, Rust `_unused`) are treated as private, while
/// top-level names matching well-known main/default/test patterns are handled
/// separately by `matches_entrypoint_pattern`.
fn looks_exported(name: &str, qual_name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    if name.starts_with('_') {
        return false;
    }
    // Symbols nested deeper than two levels (e.g. closures or inner classes
    // inside private helpers) are not robustly trackable as call edges.
    if qual_name.matches("::").count() > 2 {
        return false;
    }
    true
}

/// Names treated as framework-managed entry points. The list is conservative on
/// purpose: false negatives (we miss a true orphan) are recoverable; false
/// positives (we flag a healthy entry point) erode trust in the query.
const ENTRYPOINT_NAME_PATTERNS: &[&str] = &[
    "main",
    "default",
    "App",
    "Layout",
    "Page",
    "Document",
    "Error",
    "NotFound",
    "RootLayout",
    "RootPage",
    "Loading",
    "Template",
    "Head",
    "Middleware",
    "Component",
    "GET",
    "POST",
    "PUT",
    "PATCH",
    "DELETE",
    "OPTIONS",
    "HEAD",
    "describe",
    "it",
    "test",
    "before",
    "after",
    "beforeAll",
    "afterAll",
    "beforeEach",
    "afterEach",
    "setUp",
    "tearDown",
    "setup",
    "teardown",
    "lambda_handler",
    "handler",
    "main_async",
];

fn matches_entrypoint_pattern(symbol: &CachedGraphSymbol, _cache: &CodeGraphQueryCache) -> bool {
    if ENTRYPOINT_NAME_PATTERNS
        .iter()
        .any(|candidate| symbol.name == *candidate)
    {
        return true;
    }
    // Next.js / Vite / Jest conventional file paths surface their default
    // exports through framework wiring, not import edges. A symbol whose file
    // matches these patterns is treated as framework-managed regardless of
    // the symbol name.
    let path = symbol.path.as_str();
    if path.contains("/pages/")
        || path.contains("/app/")
        || path.contains("/api/")
        || path.contains("/routes/")
        || path.contains("/tests/")
        || path.contains("/test/")
        || path.contains("/__tests__/")
        || path.contains("/benches/")
        || path.contains("/examples/")
        || path.ends_with("/middleware.ts")
        || path.ends_with("/middleware.tsx")
        || path.ends_with("/instrumentation.ts")
        || path.ends_with(".test.ts")
        || path.ends_with(".test.tsx")
        || path.ends_with(".test.js")
        || path.ends_with(".spec.ts")
        || path.ends_with(".spec.tsx")
        || path.ends_with(".spec.js")
        || path.ends_with("conftest.py")
        || path.ends_with("/__init__.py")
    {
        return true;
    }
    // pytest-style test functions (`def test_*`) and Rust `#[test]` functions
    // (`fn test_*`) are framework-managed and not called from non-test code.
    if symbol.name.starts_with("test_") {
        return true;
    }
    false
}

fn file_importer_count(cache: &CodeGraphQueryCache, path: &str) -> usize {
    cache
        .importers_by_target_path
        .get(path)
        .map(|importers| importers.len())
        .unwrap_or(0)
}

pub fn query_resolved_importers_of(
    index: &RepoIndex,
    root: &Path,
    needle: &str,
) -> Result<QueryEnvelope> {
    query_importers_of_internal(index, root, needle, true)
}

fn query_importers_of_internal(
    index: &RepoIndex,
    root: &Path,
    needle: &str,
    resolved: bool,
) -> Result<QueryEnvelope> {
    let started = Instant::now();
    let (graph_path, query_cache_path, mut warnings) = ensure_graph_artifacts(index, root)?;
    let cache = load_or_refresh_graph_cache(index, root, &query_cache_path)?;
    let mut match_kind = if resolved {
        "canonical-file"
    } else {
        "import-lookup"
    };
    let importers = if resolved {
        if let Some(cache) = &cache {
            resolve_files_from_cache(cache, needle)
                .into_iter()
                .flat_map(|file| fetch_importers_for_target_path_from_cache(cache, &file.path))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        }
    } else if let Some(cache) = &cache {
        let (hits, kind) = fetch_importers_from_cache(cache, needle);
        match_kind = kind;
        hits
    } else {
        warnings.push(
            "Oxigraph fallback matches exact importsPath only; specifier, imported name, and file path lookup need the query cache"
                .to_string(),
        );
        match_kind = "raw-import";
        let store = load_graph_store(&graph_path)?;
        fetch_importers(&store, needle)?
    };

    let mut entities = vec![json!({
        "query": needle,
        "kind": if resolved { "resolved-importers-of" } else { "importers-of" },
        "match_kind": match_kind,
    })];
    entities.extend(importers.iter().map(|importer| {
        json!({
            "import": importer.raw_import,
            "file": importer.file_iri,
            "path": importer.path,
            "language": importer.language,
            "line": importer.line,
        })
    }));

    let mut evidence = graph_artifact_evidence(&graph_path, &query_cache_path);
    evidence.extend(importers.iter().map(|importer| EvidenceItem {
        kind: "graph_importer".to_string(),
        path: importer.path.clone(),
        line: importer.line,
        detail: format!("imports {}", importer.raw_import),
    }));

    let summary = if resolved {
        format!(
            "file `{}` has {} canonical importers in current code graph",
            needle,
            importers.len()
        )
    } else {
        format!(
            "import `{}` has {} importers in current code graph",
            needle,
            importers.len()
        )
    };

    Ok(QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("graph"),
        kind: "graph".to_string(),
        summary,
        confidence: if importers.is_empty() { 0.75 } else { 0.9 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    })
}

fn ensure_graph_artifacts(
    index: &RepoIndex,
    root: &Path,
) -> Result<(PathBuf, PathBuf, Vec<String>)> {
    let output_dir = default_code_graph_output_dir(root);
    let graph_path = output_dir.join("graph.nq");
    let manifest_path = default_code_graph_manifest_path(&output_dir);
    let query_cache_path = default_code_graph_cache_path(&output_dir);
    let mut warnings = Vec::new();

    let refresh_reason =
        if !graph_path.exists() || !query_cache_path.exists() || !manifest_path.exists() {
            Some("code graph artifacts missing".to_string())
        } else {
            code_graph_refresh_reason(index, &output_dir)
        };

    if let Some(reason) = refresh_reason {
        export_code_graph(index, root, &output_dir)?;
        warnings.push(format!(
            "code graph artifacts refreshed; generated fresh export ({reason})"
        ));
    }

    Ok((graph_path, query_cache_path, warnings))
}

fn load_graph_store(graph_path: &Path) -> Result<Store> {
    let file = File::open(graph_path)
        .with_context(|| format!("failed to open {}", graph_path.display()))?;
    let reader = BufReader::new(file);
    let store = Store::new().context("failed to create Oxigraph store")?;
    store
        .load_from_reader(RdfFormat::NQuads, reader)
        .with_context(|| format!("failed to load N-Quads from {}", graph_path.display()))?;
    Ok(store)
}

fn load_graph_cache(cache_path: &Path) -> Result<CodeGraphQueryCache> {
    let raw = std::fs::read_to_string(cache_path)
        .with_context(|| format!("failed to read {}", cache_path.display()))?;
    let cache: CodeGraphQueryCache = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", cache_path.display()))?;
    if cache.query_cache_version != QUERY_CACHE_VERSION {
        anyhow::bail!(
            "query cache schema mismatch: expected {}, found {}",
            QUERY_CACHE_VERSION,
            cache.query_cache_version
        );
    }
    Ok(cache)
}

/// Load an existing code-graph query cache from disk **without** rebuilding exports.
/// Returns `None` when missing, unreadable, or schema version mismatch (fail-open for callers).
pub fn try_load_graph_query_cache(root: &Path) -> Option<CodeGraphQueryCache> {
    let path = default_code_graph_cache_path(&default_code_graph_output_dir(root));
    load_graph_cache(&path).ok()
}

fn load_or_refresh_graph_cache(
    index: &RepoIndex,
    root: &Path,
    query_cache_path: &Path,
) -> Result<Option<CodeGraphQueryCache>> {
    match load_graph_cache(query_cache_path) {
        Ok(cache) => Ok(Some(cache)),
        Err(_) => {
            let output_dir = default_code_graph_output_dir(root);
            export_code_graph(index, root, &output_dir)?;
            Ok(load_graph_cache(query_cache_path).ok())
        }
    }
}

fn resolve_candidates(store: &Store, needle: &str) -> Result<Vec<ResolvedSymbol>> {
    if needle.starts_with("urn:") {
        return resolve_by_iri(store, needle);
    }
    let ns = active_code_ns();
    let quoted = sparql_string(needle);
    let query = format!(
        r#"
PREFIX code: <{ns}>
PREFIX prov: <{PROV_NS}>
SELECT DISTINCT ?symbol ?name ?qualName ?kind ?path WHERE {{
  GRAPH ?g {{
    ?symbol a code:Symbol ;
            code:name ?name ;
            code:qualName ?qualName ;
            code:symbolKind ?kind .
    FILTER(?name = "{quoted}" || ?qualName = "{quoted}")
    OPTIONAL {{
      ?occ prov:specializationOf ?symbol ;
           code:definedIn ?file .
      ?file code:path ?path .
    }}
  }}
}}
ORDER BY ?qualName ?path
"#
    );
    collect_symbols(
        store
            .sparql_query(&query)
            .context("failed to query candidate symbols")?,
    )
}

fn resolve_candidates_from_cache(cache: &CodeGraphQueryCache, needle: &str) -> Vec<ResolvedSymbol> {
    if needle.starts_with("urn:") {
        return cache
            .symbols
            .get(needle)
            .map(|symbol| vec![symbol_from_cache(symbol)])
            .unwrap_or_default();
    }

    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for iri in cache
        .symbol_lookup_name
        .get(needle)
        .into_iter()
        .flatten()
        .chain(
            cache
                .symbol_lookup_qual_name
                .get(needle)
                .into_iter()
                .flatten(),
        )
    {
        if !seen.insert(iri.clone()) {
            continue;
        }
        if let Some(symbol) = cache.symbols.get(iri) {
            out.push(symbol_from_cache(symbol));
        }
    }
    out.sort_by(|left, right| {
        left.qual_name
            .cmp(&right.qual_name)
            .then_with(|| left.path.cmp(&right.path))
    });
    out
}

fn resolve_by_iri(store: &Store, iri: &str) -> Result<Vec<ResolvedSymbol>> {
    let ns = active_code_ns();
    let query = format!(
        r#"
PREFIX code: <{ns}>
PREFIX prov: <{PROV_NS}>
SELECT DISTINCT ?symbol ?name ?qualName ?kind ?path WHERE {{
  GRAPH ?g {{
    VALUES ?symbol {{ <{iri}> }}
    ?symbol a code:Symbol ;
            code:name ?name ;
            code:qualName ?qualName ;
            code:symbolKind ?kind .
    OPTIONAL {{
      ?occ prov:specializationOf ?symbol ;
           code:definedIn ?file .
      ?file code:path ?path .
    }}
  }}
}}
"#
    );
    collect_symbols(
        store
            .sparql_query(&query)
            .context("failed to resolve symbol by iri")?,
    )
}

fn collect_symbols(results: QueryResults) -> Result<Vec<ResolvedSymbol>> {
    let mut out = Vec::new();
    if let QueryResults::Solutions(solutions) = results {
        let mut seen = BTreeSet::new();
        for solution in solutions {
            let solution = solution.context("failed to read SPARQL solution")?;
            let Some(Term::NamedNode(symbol)) = solution.get("symbol") else {
                continue;
            };
            if !seen.insert(symbol.as_str().to_string()) {
                continue;
            }
            out.push(ResolvedSymbol {
                iri: symbol.as_str().to_string(),
                name: term_value(solution.get("name")),
                qual_name: term_value(solution.get("qualName")),
                kind: term_value(solution.get("kind")),
                path: term_value(solution.get("path")),
            });
        }
    }
    Ok(out)
}

fn resolve_files(store: &Store, needle: &str) -> Result<Vec<ResolvedFile>> {
    let ns = active_code_ns();
    let quoted = sparql_string(needle);
    let query = format!(
        r#"
PREFIX code: <{ns}>
SELECT DISTINCT ?file ?path ?language WHERE {{
  GRAPH ?g {{
    ?file a code:File ;
          code:path ?path ;
          code:language ?language .
    FILTER(?path = "{quoted}" || STRENDS(?path, "{quoted}"))
  }}
}}
ORDER BY ?path
"#
    );

    let mut out = Vec::new();
    if let QueryResults::Solutions(solutions) = store
        .sparql_query(&query)
        .context("failed to resolve file candidates")?
    {
        let mut seen = BTreeSet::new();
        for solution in solutions {
            let solution = solution.context("failed to read file solution")?;
            let Some(Term::NamedNode(file)) = solution.get("file") else {
                continue;
            };
            if !seen.insert(file.as_str().to_string()) {
                continue;
            }
            out.push(ResolvedFile {
                iri: file.as_str().to_string(),
                path: term_value(solution.get("path")),
                language: term_value(solution.get("language")),
            });
        }
    }
    Ok(out)
}

fn resolve_files_from_cache(cache: &CodeGraphQueryCache, needle: &str) -> Vec<ResolvedFile> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for (path, file_iris) in &cache.file_lookup_path {
        if path != needle && !path.ends_with(needle) {
            continue;
        }
        for iri in file_iris {
            if !seen.insert(iri.clone()) {
                continue;
            }
            if let Some(file) = cache.files.get(iri) {
                out.push(ResolvedFile {
                    iri: file.iri.clone(),
                    path: file.path.clone(),
                    language: file.language.clone(),
                });
            }
        }
    }
    out.sort_by(|left, right| left.path.cmp(&right.path));
    out
}

fn fetch_neighbors(
    store: &Store,
    direction: GraphDirection,
    symbol_iri: &str,
) -> Result<Vec<GraphNeighbor>> {
    let ns = active_code_ns();
    let query = match direction {
        GraphDirection::CallersOf => format!(
            r#"
PREFIX code: <{ns}>
PREFIX prov: <{PROV_NS}>
SELECT DISTINCT ?neighbor ?name ?qualName ?kind ?path ?line WHERE {{
  GRAPH ?g {{
    {{
      ?occ code:calls <{symbol_iri}> ;
           prov:specializationOf ?neighbor ;
           code:definedIn ?file .
      ?neighbor a code:Symbol ;
                code:name ?name ;
                code:qualName ?qualName ;
                code:symbolKind ?kind .
      ?file code:path ?path .
      OPTIONAL {{ ?occ code:startLine ?line . }}
    }}
    UNION
    {{
      ?neighbor a code:File ;
                code:calls <{symbol_iri}> ;
                code:path ?path .
      BIND(?path AS ?name)
      BIND(?path AS ?qualName)
      BIND("file" AS ?kind)
    }}
  }}
}}
ORDER BY ?path ?line ?qualName
"#
        ),
        GraphDirection::CalleesOf => format!(
            r#"
PREFIX code: <{ns}>
PREFIX prov: <{PROV_NS}>
SELECT DISTINCT ?neighbor ?name ?qualName ?kind ?path ?line WHERE {{
  GRAPH ?g {{
    ?sourceOcc prov:specializationOf <{symbol_iri}> ;
               code:calls ?neighbor .
    ?neighbor a code:Symbol ;
              code:name ?name ;
              code:qualName ?qualName ;
              code:symbolKind ?kind .
    OPTIONAL {{
      ?neighborOcc prov:specializationOf ?neighbor ;
                   code:definedIn ?file ;
                   code:startLine ?line .
      ?file code:path ?path .
    }}
  }}
}}
ORDER BY ?path ?line ?qualName
"#
        ),
    };

    let mut out = Vec::new();
    if let QueryResults::Solutions(solutions) = store
        .sparql_query(&query)
        .context("failed to query code graph neighbors")?
    {
        let mut seen = BTreeSet::new();
        for solution in solutions {
            let solution = solution.context("failed to read graph solution")?;
            let Some(Term::NamedNode(neighbor)) = solution.get("neighbor") else {
                continue;
            };
            if !seen.insert(neighbor.as_str().to_string()) {
                continue;
            }
            out.push(GraphNeighbor {
                iri: neighbor.as_str().to_string(),
                name: term_value(solution.get("name")),
                qual_name: term_value(solution.get("qualName")),
                kind: term_value(solution.get("kind")),
                path: term_value(solution.get("path")),
                line: term_value(solution.get("line")).parse::<usize>().ok(),
            });
        }
    }
    Ok(out)
}

fn fetch_neighbors_from_cache(
    cache: &CodeGraphQueryCache,
    direction: GraphDirection,
    symbol_iri: &str,
) -> Vec<GraphNeighbor> {
    let neighbors = match direction {
        GraphDirection::CallersOf => cache.callers_by_symbol.get(symbol_iri),
        GraphDirection::CalleesOf => cache.callees_by_symbol.get(symbol_iri),
    };
    neighbors
        .into_iter()
        .flatten()
        .map(neighbor_from_cache)
        .collect()
}

fn fetch_callsites(store: &Store, symbol_iri: &str) -> Result<Vec<GraphCallsite>> {
    let ns = active_code_ns();
    let query = format!(
        r#"
PREFIX code: <{ns}>
PREFIX prov: <{PROV_NS}>
SELECT DISTINCT ?owner ?ownerKind ?ownerName ?ownerQualName ?path ?line ?expr WHERE {{
  GRAPH ?g {{
    ?owner code:calls <{symbol_iri}> ;
           code:callsExpr ?expr .
    OPTIONAL {{ ?owner code:startLine ?line . }}
    OPTIONAL {{ ?owner code:path ?ownerPath . }}
    OPTIONAL {{
      ?owner code:definedIn ?file .
      ?file code:path ?definedPath .
    }}
    BIND(COALESCE(?definedPath, ?ownerPath, "") AS ?path)
    OPTIONAL {{
      ?owner prov:specializationOf ?symbol .
      ?symbol a code:Symbol ;
              code:name ?ownerName ;
              code:qualName ?ownerQualName ;
              code:symbolKind ?ownerKind .
    }}
  }}
}}
ORDER BY ?path ?line ?ownerQualName ?expr
"#
    );

    let mut out = Vec::new();
    if let QueryResults::Solutions(solutions) = store
        .sparql_query(&query)
        .context("failed to query graph callsites")?
    {
        let mut seen = BTreeSet::new();
        for solution in solutions {
            let solution = solution.context("failed to read graph callsite solution")?;
            let Some(Term::NamedNode(owner)) = solution.get("owner") else {
                continue;
            };
            let line = term_value(solution.get("line")).parse::<usize>().ok();
            let expr = term_value(solution.get("expr"));
            let path = term_value(solution.get("path"));
            let dedupe_key = format!(
                "{}|{}|{}|{}",
                owner.as_str(),
                path,
                line.unwrap_or_default(),
                expr
            );
            if !seen.insert(dedupe_key) {
                continue;
            }
            out.push(GraphCallsite {
                owner_iri: owner.as_str().to_string(),
                owner_kind: term_value(solution.get("ownerKind")),
                owner_name: term_value(solution.get("ownerName")),
                owner_qual_name: term_value(solution.get("ownerQualName")),
                path,
                line,
                expr,
            });
        }
    }
    Ok(out)
}

fn fetch_callsites_from_cache(cache: &CodeGraphQueryCache, symbol_iri: &str) -> Vec<GraphCallsite> {
    cache
        .callsites_by_symbol
        .get(symbol_iri)
        .into_iter()
        .flatten()
        .map(callsite_from_cache)
        .collect()
}

fn fetch_symbols_in_file(store: &Store, file_iri: &str) -> Result<Vec<GraphNeighbor>> {
    let ns = active_code_ns();
    let query = format!(
        r#"
PREFIX code: <{ns}>
PREFIX prov: <{PROV_NS}>
SELECT DISTINCT ?symbol ?name ?qualName ?kind ?path ?line WHERE {{
  GRAPH ?g {{
    <{file_iri}> code:containsSymbol ?symbol ;
                 code:path ?path .
    ?symbol a code:Symbol ;
            code:name ?name ;
            code:qualName ?qualName ;
            code:symbolKind ?kind .
    OPTIONAL {{
      ?occ prov:specializationOf ?symbol ;
           code:definedIn <{file_iri}> ;
           code:startLine ?line .
    }}
  }}
}}
ORDER BY ?line ?qualName
"#
    );

    let mut out = Vec::new();
    if let QueryResults::Solutions(solutions) = store
        .sparql_query(&query)
        .context("failed to query file symbols")?
    {
        let mut seen = BTreeSet::new();
        for solution in solutions {
            let solution = solution.context("failed to read file symbol solution")?;
            let Some(Term::NamedNode(symbol)) = solution.get("symbol") else {
                continue;
            };
            if !seen.insert(symbol.as_str().to_string()) {
                continue;
            }
            out.push(GraphNeighbor {
                iri: symbol.as_str().to_string(),
                name: term_value(solution.get("name")),
                qual_name: term_value(solution.get("qualName")),
                kind: term_value(solution.get("kind")),
                path: term_value(solution.get("path")),
                line: term_value(solution.get("line")).parse::<usize>().ok(),
            });
        }
    }
    Ok(out)
}

fn fetch_symbols_in_file_from_cache(
    cache: &CodeGraphQueryCache,
    file_iri: &str,
) -> Vec<GraphNeighbor> {
    cache
        .file_symbols
        .get(file_iri)
        .into_iter()
        .flatten()
        .map(neighbor_from_cache)
        .collect()
}

fn fetch_imports_in_file(store: &Store, file_iri: &str) -> Result<Vec<GraphImportDetail>> {
    let ns = active_code_ns();
    let query = format!(
        r#"
PREFIX code: <{ns}>
SELECT DISTINCT ?raw WHERE {{
  GRAPH ?g {{
    <{file_iri}> code:importsPath ?raw .
  }}
}}
ORDER BY ?raw
"#
    );

    let mut out = Vec::new();
    if let QueryResults::Solutions(solutions) = store
        .sparql_query(&query)
        .context("failed to query file imports")?
    {
        let mut seen = BTreeSet::new();
        for solution in solutions {
            let solution = solution.context("failed to read file import solution")?;
            let raw = term_value(solution.get("raw"));
            if raw.is_empty() || !seen.insert(raw.clone()) {
                continue;
            }
            out.push(GraphImportDetail {
                raw: raw.clone(),
                syntax_kind: String::new(),
                module_specifiers: Vec::new(),
                imported_names: Vec::new(),
                line: None,
                resolution_kind: "unresolved".to_string(),
                candidate_paths: Vec::new(),
                candidate_symbols: Vec::new(),
            });
        }
    }
    Ok(out)
}

fn fetch_imports_in_file_from_cache(
    cache: &CodeGraphQueryCache,
    file_iri: &str,
) -> Vec<GraphImportDetail> {
    cache
        .file_import_details
        .get(file_iri)
        .into_iter()
        .flatten()
        .map(import_detail_from_cache)
        .collect()
}

fn fetch_importers(store: &Store, needle: &str) -> Result<Vec<GraphImporter>> {
    let ns = active_code_ns();
    let quoted = sparql_string(needle);
    let query = format!(
        r#"
PREFIX code: <{ns}>
SELECT DISTINCT ?file ?path ?language ?raw WHERE {{
  GRAPH ?g {{
    ?file a code:File ;
          code:path ?path ;
          code:language ?language ;
          code:importsPath ?raw .
    FILTER(?raw = "{quoted}")
  }}
}}
ORDER BY ?path
"#
    );

    let mut out = Vec::new();
    if let QueryResults::Solutions(solutions) = store
        .sparql_query(&query)
        .context("failed to query importers")?
    {
        let mut seen = BTreeSet::new();
        for solution in solutions {
            let solution = solution.context("failed to read importer solution")?;
            let Some(Term::NamedNode(file)) = solution.get("file") else {
                continue;
            };
            let raw_import = term_value(solution.get("raw"));
            let path = term_value(solution.get("path"));
            let dedupe_key = format!("{}|{}", file.as_str(), raw_import);
            if !seen.insert(dedupe_key) {
                continue;
            }
            out.push(GraphImporter {
                raw_import,
                file_iri: file.as_str().to_string(),
                path,
                language: term_value(solution.get("language")),
                line: None,
            });
        }
    }
    Ok(out)
}

fn fetch_importers_from_cache(
    cache: &CodeGraphQueryCache,
    needle: &str,
) -> (Vec<GraphImporter>, &'static str) {
    let needles = import_query_aliases(needle);
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut kinds = BTreeSet::new();

    for key in &needles {
        let before = out.len();
        append_cached_importers(
            &mut out,
            &mut seen,
            cache.importers_by_raw.get(key).into_iter().flatten(),
        );
        if out.len() > before {
            kinds.insert("raw-import");
        }
        if needle_looks_like_path(needle) {
            let before = out.len();
            append_cached_importers(
                &mut out,
                &mut seen,
                cache
                    .importers_by_target_path
                    .get(key)
                    .into_iter()
                    .flatten(),
            );
            if out.len() > before {
                kinds.insert("canonical-file");
            }
        }
    }

    for (file_iri, details) in &cache.file_import_details {
        let Some(file) = cache.files.get(file_iri) else {
            continue;
        };
        for detail in details {
            let candidate_paths = if needle_looks_like_path(needle) {
                detail.candidate_paths.as_slice()
            } else {
                &[]
            };
            let Some(kind) = import_detail_hit_kind(
                &detail.module_specifiers,
                &detail.imported_names,
                candidate_paths,
                &needles,
            ) else {
                continue;
            };
            let importer = CachedGraphImporter {
                raw_import: detail.raw.clone(),
                file_iri: file.iri.clone(),
                path: file.path.clone(),
                language: file.language.clone(),
                line: detail.line,
            };
            let before = out.len();
            append_cached_importers(&mut out, &mut seen, std::iter::once(&importer));
            if out.len() > before {
                kinds.insert(kind);
            }
        }
    }

    out.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.raw_import.cmp(&right.raw_import))
    });
    (out, collapse_import_match_kinds(&kinds))
}

fn append_cached_importers<'a>(
    out: &mut Vec<GraphImporter>,
    seen: &mut BTreeSet<String>,
    importers: impl IntoIterator<Item = &'a CachedGraphImporter>,
) {
    for importer in importers {
        let line = importer
            .line
            .map(|value| value.to_string())
            .unwrap_or_default();
        let dedupe = format!("{}|{}|{line}", importer.file_iri, importer.raw_import);
        if seen.insert(dedupe) {
            out.push(importer_from_cache(importer));
        }
    }
}

fn fetch_importers_for_target_path_from_cache(
    cache: &CodeGraphQueryCache,
    target_path: &str,
) -> Vec<GraphImporter> {
    cache
        .importers_by_target_path
        .get(target_path)
        .into_iter()
        .flatten()
        .map(importer_from_cache)
        .collect()
}

fn graph_artifact_evidence(graph_path: &Path, query_cache_path: &Path) -> Vec<EvidenceItem> {
    vec![
        EvidenceItem {
            kind: "artifact".to_string(),
            path: graph_path.display().to_string(),
            line: None,
            detail: "code graph N-Quads export".to_string(),
        },
        EvidenceItem {
            kind: "artifact".to_string(),
            path: query_cache_path.display().to_string(),
            line: None,
            detail: "code graph query cache".to_string(),
        },
    ]
}

fn symbol_from_cache(symbol: &CachedGraphSymbol) -> ResolvedSymbol {
    ResolvedSymbol {
        iri: symbol.iri.clone(),
        name: symbol.name.clone(),
        qual_name: symbol.qual_name.clone(),
        kind: symbol.kind.clone(),
        path: symbol.path.clone(),
    }
}

fn neighbor_from_cache(neighbor: &CachedGraphNeighbor) -> GraphNeighbor {
    GraphNeighbor {
        iri: neighbor.iri.clone(),
        name: neighbor.name.clone(),
        qual_name: neighbor.qual_name.clone(),
        kind: neighbor.kind.clone(),
        path: neighbor.path.clone(),
        line: neighbor.line,
    }
}

fn callsite_from_cache(callsite: &CachedGraphCallsite) -> GraphCallsite {
    GraphCallsite {
        owner_iri: callsite.owner_iri.clone(),
        owner_kind: callsite.owner_kind.clone(),
        owner_name: callsite.owner_name.clone(),
        owner_qual_name: callsite.owner_qual_name.clone(),
        path: callsite.path.clone(),
        line: callsite.line,
        expr: callsite.expr.clone(),
    }
}

fn import_detail_from_cache(import_detail: &CachedGraphImport) -> GraphImportDetail {
    GraphImportDetail {
        raw: import_detail.raw.clone(),
        syntax_kind: import_detail.syntax_kind.clone(),
        module_specifiers: import_detail.module_specifiers.clone(),
        imported_names: import_detail.imported_names.clone(),
        line: import_detail.line,
        resolution_kind: import_detail.resolution_kind.clone(),
        candidate_paths: import_detail.candidate_paths.clone(),
        candidate_symbols: import_detail
            .candidate_symbols
            .iter()
            .map(neighbor_from_cache)
            .collect(),
    }
}

fn importer_from_cache(importer: &CachedGraphImporter) -> GraphImporter {
    GraphImporter {
        raw_import: importer.raw_import.clone(),
        file_iri: importer.file_iri.clone(),
        path: importer.path.clone(),
        language: importer.language.clone(),
        line: importer.line,
    }
}

fn sparql_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn term_value(term: Option<&Term>) -> String {
    match term {
        Some(Term::Literal(lit)) => lit.value().to_string(),
        Some(Term::NamedNode(node)) => node.as_str().to_string(),
        Some(Term::BlankNode(node)) => format!("_:{}", node.as_str()),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FileRecord, RepoIndex, SourceLanguage};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_test_root(prefix: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should move forward")
            .as_nanos();
        std::env::temp_dir().join(format!("leio-code-{prefix}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn callers_of_query_resolves_named_graph_export() {
        let root = unique_test_root("graph-query");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".leio-code")).expect("create test root");
        fs::write(
            root.join("example.py"),
            "def helper():\n    return 1\n\ndef caller():\n    return helper()\n",
        )
        .expect("write source");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "2026-03-29T00:00:00Z".to_string(),
            files: vec![FileRecord {
                path: "example.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 64,
                modified_unix_ms: 0,
                symbols: Vec::new(),
                env_vars: Vec::new(),
                redis_keys: Vec::new(),
                subprocess_calls: Vec::new(),
                http_calls: Vec::new(),
                unresolved_edges: Vec::new(),
            }],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let envelope = query_call_graph(&index, &root, GraphDirection::CallersOf, "helper")
            .expect("query should succeed");

        assert!(envelope.summary.contains("has 1 callers"));
        assert!(
            envelope
                .entities
                .iter()
                .any(|entity| entity.to_string().contains("caller"))
        );
    }

    #[test]
    fn callsites_of_query_returns_call_expression() {
        let root = unique_test_root("graph-callsites");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".leio-code")).expect("create test root");
        fs::write(
            root.join("example.py"),
            "def helper():\n    return 1\n\ndef caller():\n    return helper()\n",
        )
        .expect("write source");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "2026-03-29T00:00:00Z".to_string(),
            files: vec![FileRecord {
                path: "example.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 64,
                modified_unix_ms: 0,
                symbols: Vec::new(),
                env_vars: Vec::new(),
                redis_keys: Vec::new(),
                subprocess_calls: Vec::new(),
                http_calls: Vec::new(),
                unresolved_edges: Vec::new(),
            }],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let envelope = query_callsites_of(&index, &root, "helper").expect("query should succeed");

        assert!(envelope.summary.contains("has 1 callsites"));
        assert!(
            envelope
                .entities
                .iter()
                .any(|entity| entity.to_string().contains("\"expr\":\"helper\""))
        );
    }

    #[test]
    fn symbols_in_query_returns_local_symbols() {
        let root = unique_test_root("graph-symbols-in");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".leio-code")).expect("create test root");
        fs::write(
            root.join("example.py"),
            "def helper():\n    return 1\n\nclass Example:\n    pass\n",
        )
        .expect("write source");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "2026-03-29T00:00:00Z".to_string(),
            files: vec![FileRecord {
                path: "example.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 64,
                modified_unix_ms: 0,
                symbols: Vec::new(),
                env_vars: Vec::new(),
                redis_keys: Vec::new(),
                subprocess_calls: Vec::new(),
                http_calls: Vec::new(),
                unresolved_edges: Vec::new(),
            }],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let envelope = query_symbols_in(&index, &root, "example.py").expect("query should work");

        assert!(envelope.summary.contains("defines 2 symbols"));
        assert!(
            envelope
                .entities
                .iter()
                .any(|entity| entity.to_string().contains("Example"))
        );
        assert!(
            envelope
                .entities
                .iter()
                .any(|entity| entity.to_string().contains("helper"))
        );
    }

    #[test]
    fn symbols_in_query_refreshes_stale_export_when_index_changes() {
        let root = unique_test_root("graph-symbols-refresh");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".leio-code")).expect("create test root");
        fs::write(root.join("old.py"), "def old_helper():\n    return 1\n").expect("write old");

        let stale_index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "2026-03-29T00:00:00Z".to_string(),
            files: vec![FileRecord {
                path: "old.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 31,
                modified_unix_ms: 1,
                symbols: Vec::new(),
                env_vars: Vec::new(),
                redis_keys: Vec::new(),
                subprocess_calls: Vec::new(),
                http_calls: Vec::new(),
                unresolved_edges: Vec::new(),
            }],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        export_code_graph(&stale_index, &root, &default_code_graph_output_dir(&root))
            .expect("export stale graph");

        fs::write(root.join("fresh.py"), "def fresh_helper():\n    return 2\n")
            .expect("write fresh");

        let fresh_index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "2026-03-30T00:00:00Z".to_string(),
            files: vec![
                FileRecord {
                    path: "old.py".to_string(),
                    language: SourceLanguage::Python,
                    bytes: 31,
                    modified_unix_ms: 1,
                    symbols: Vec::new(),
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "fresh.py".to_string(),
                    language: SourceLanguage::Python,
                    bytes: 33,
                    modified_unix_ms: 2,
                    symbols: Vec::new(),
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

        let envelope = query_symbols_in(&fresh_index, &root, "fresh.py")
            .expect("query should refresh stale graph");

        assert!(envelope.summary.contains("defines 1 symbols"));
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("refreshed"))
        );
        assert!(
            envelope
                .entities
                .iter()
                .any(|entity| entity.to_string().contains("fresh_helper"))
        );
    }

    #[test]
    fn callers_of_query_includes_file_scope_calls() {
        let root = unique_test_root("graph-file-scope-callers");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".leio-code")).expect("create test root");
        fs::write(
            root.join("example.py"),
            "def helper():\n    return 1\n\nhelper()\n",
        )
        .expect("write source");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "2026-03-29T00:00:00Z".to_string(),
            files: vec![FileRecord {
                path: "example.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 32,
                modified_unix_ms: 0,
                symbols: Vec::new(),
                env_vars: Vec::new(),
                redis_keys: Vec::new(),
                subprocess_calls: Vec::new(),
                http_calls: Vec::new(),
                unresolved_edges: Vec::new(),
            }],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let envelope = query_call_graph(&index, &root, GraphDirection::CallersOf, "helper")
            .expect("query should succeed");

        assert!(envelope.summary.contains("has 1 callers"));
        assert!(
            envelope
                .entities
                .iter()
                .any(|entity| entity.to_string().contains("\"kind\":\"file\""))
        );
    }

    #[test]
    fn imports_in_query_returns_raw_imports_for_file() {
        let root = unique_test_root("graph-imports-in");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".leio-code")).expect("create test root");
        fs::write(
            root.join("example.py"),
            "import os\nfrom pathlib import Path\n\ndef helper():\n    return os.getcwd()\n",
        )
        .expect("write source");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "2026-03-29T00:00:00Z".to_string(),
            files: vec![FileRecord {
                path: "example.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 96,
                modified_unix_ms: 0,
                symbols: Vec::new(),
                env_vars: Vec::new(),
                redis_keys: Vec::new(),
                subprocess_calls: Vec::new(),
                http_calls: Vec::new(),
                unresolved_edges: Vec::new(),
            }],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let envelope = query_imports_in(&index, &root, "example.py").expect("query should work");

        assert!(envelope.summary.contains("declares 2 imports"));
        assert!(
            envelope
                .entities
                .iter()
                .any(|entity| entity.to_string().contains("import os"))
        );
        assert!(
            envelope
                .entities
                .iter()
                .any(|entity| entity.to_string().contains("from pathlib import Path"))
        );
    }

    #[test]
    fn importers_of_query_returns_files_for_raw_import() {
        let root = unique_test_root("graph-importers-of");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".leio-code")).expect("create test root");
        fs::write(root.join("a.py"), "import os\n").expect("write a");
        fs::write(root.join("b.py"), "import os\n").expect("write b");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "2026-03-29T00:00:00Z".to_string(),
            files: vec![
                FileRecord {
                    path: "a.py".to_string(),
                    language: SourceLanguage::Python,
                    bytes: 10,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "b.py".to_string(),
                    language: SourceLanguage::Python,
                    bytes: 10,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
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

        let envelope = query_importers_of(&index, &root, "import os").expect("query should work");

        assert!(envelope.summary.contains("has 2 importers"));
        assert!(
            envelope
                .entities
                .iter()
                .any(|entity| entity.to_string().contains("\"path\":\"a.py\""))
        );
        assert!(
            envelope
                .entities
                .iter()
                .any(|entity| entity.to_string().contains("\"path\":\"b.py\""))
        );

        let by_specifier = query_importers_of(&index, &root, "os").expect("specifier lookup");
        assert!(
            by_specifier.summary.contains("has 2 importers"),
            "importers-of os (module specifier) must match `import os`, not only the raw statement: {}",
            by_specifier.summary
        );
        assert_eq!(
            by_specifier.entities[0]["match_kind"].as_str(),
            Some("specifier")
        );
    }

    #[test]
    fn importers_of_query_matches_python_module_name_and_file_path() {
        let root = unique_test_root("graph-importers-python-from");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("cartridges/c4gym")).expect("create module dir");
        fs::create_dir_all(root.join("example-api/example")).expect("create consumer dir");
        fs::write(
            root.join("cartridges/c4gym/seed_cobranca.py"),
            "def seed_c4_cobranca_agent():\n    return 1\n",
        )
        .expect("write module");
        fs::write(
            root.join("cartridges/c4gym/tests_atendimento.py"),
            "from cartridges.c4gym.seed_cobranca import seed_c4_cobranca_agent\n",
        )
        .expect("write test importer");
        fs::write(
            root.join("example-api/example/main.py"),
            "from cartridges.c4gym.seed_cobranca import seed_c4_cobranca_agent\n",
        )
        .expect("write api importer");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "2026-03-29T00:00:00Z".to_string(),
            files: vec![
                python_file_record("cartridges/c4gym/seed_cobranca.py"),
                python_file_record("cartridges/c4gym/tests_atendimento.py"),
                python_file_record("example-api/example/main.py"),
            ],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        for needle in [
            "cartridges.c4gym.seed_cobranca",
            "seed_c4_cobranca_agent",
            "cartridges/c4gym/seed_cobranca.py",
        ] {
            let envelope = query_importers_of(&index, &root, needle)
                .unwrap_or_else(|error| panic!("importers-of {needle}: {error}"));
            let paths = importer_paths(&envelope);
            assert!(
                envelope.summary.contains("has 2 importers"),
                "importers-of {needle} should find both Python consumers, got {}: {paths:?}",
                envelope.summary
            );
            assert!(
                paths
                    .iter()
                    .any(|path| path == "cartridges/c4gym/tests_atendimento.py"),
                "importers-of {needle} missing test consumer: {paths:?}"
            );
            assert!(
                paths
                    .iter()
                    .any(|path| path == "example-api/example/main.py"),
                "importers-of {needle} missing api consumer: {paths:?}"
            );
        }

        let stem = query_importers_of(&index, &root, "seed_cobranca")
            .expect("last-segment query should run");
        assert!(
            !stem.summary.contains("has 2 importers"),
            "last-segment `seed_cobranca` must not collide with the module path: {}",
            stem.summary
        );
    }

    fn python_file_record(path: &str) -> crate::model::FileRecord {
        crate::model::FileRecord {
            path: path.to_string(),
            language: SourceLanguage::Python,
            bytes: 64,
            modified_unix_ms: 0,
            symbols: Vec::new(),
            env_vars: Vec::new(),
            redis_keys: Vec::new(),
            subprocess_calls: Vec::new(),
            http_calls: Vec::new(),
            unresolved_edges: Vec::new(),
        }
    }

    fn importer_paths(envelope: &QueryEnvelope) -> Vec<String> {
        envelope
            .entities
            .iter()
            .filter_map(|entity| entity.get("path")?.as_str().map(str::to_string))
            .collect()
    }

    #[test]
    fn resolved_imports_in_query_exposes_canonical_candidate_paths_for_ts_aliases() {
        let root = unique_test_root("graph-imports-canonical-ts");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("example-ops/src/lib")).expect("create lib dir");
        fs::write(
            root.join("example-ops/src/page.ts"),
            "import { helper } from \"@/lib/auth\";\nexport const page = helper();\n",
        )
        .expect("write importer");
        fs::write(
            root.join("example-ops/src/lib/auth.ts"),
            "export function helper() { return 1; }\n",
        )
        .expect("write imported module");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "2026-03-29T00:00:00Z".to_string(),
            files: vec![
                FileRecord {
                    path: "example-ops/src/page.ts".to_string(),
                    language: SourceLanguage::TypeScript,
                    bytes: 68,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "example-ops/src/lib/auth.ts".to_string(),
                    language: SourceLanguage::TypeScript,
                    bytes: 25,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
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

        let envelope = query_resolved_imports_in(&index, &root, "example-ops/src/page.ts")
            .expect("query should work");

        assert!(envelope.summary.contains("declares 1 resolved imports"));
        assert!(envelope.entities.iter().any(|entity| {
            entity
                .to_string()
                .contains("\"resolution_kind\":\"resolved\"")
        }));
        assert!(
            envelope
                .entities
                .iter()
                .any(|entity| { entity.to_string().contains("example-ops/src/lib/auth.ts") })
        );
        // Order-insensitive: the original assertion looked at a specific JSON
        // key order inside `candidate_symbols[0]` which is no longer
        // guaranteed now that `serde_json` preserves insertion order (see
        // `Cargo.toml`'s `preserve_order` feature, added for `schema_version`
        // ordering). Branch on the parsed value instead of substring-matching.
        assert!(envelope.entities.iter().any(|entity| {
            entity
                .get("candidate_symbols")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|sym| sym.get("kind"))
                .and_then(|kind| kind.as_str())
                == Some("function")
        }));
    }

    #[test]
    fn resolved_importers_of_query_prefers_canonical_file_resolution_when_available() {
        let root = unique_test_root("graph-importers-canonical-ts");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("example-ops/src/lib")).expect("create lib dir");
        fs::write(
            root.join("example-ops/src/page.ts"),
            "import { helper } from \"@/lib/auth\";\nexport const page = helper();\n",
        )
        .expect("write importer");
        fs::write(
            root.join("example-ops/src/lib/auth.ts"),
            "export function helper() { return 1; }\n",
        )
        .expect("write imported module");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "2026-03-29T00:00:00Z".to_string(),
            files: vec![
                FileRecord {
                    path: "example-ops/src/page.ts".to_string(),
                    language: SourceLanguage::TypeScript,
                    bytes: 68,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "example-ops/src/lib/auth.ts".to_string(),
                    language: SourceLanguage::TypeScript,
                    bytes: 25,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
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

        let envelope = query_resolved_importers_of(&index, &root, "example-ops/src/lib/auth.ts")
            .expect("query should work");

        assert!(envelope.summary.contains("canonical importers"));
        assert!(envelope.entities.iter().any(|entity| {
            entity
                .to_string()
                .contains("\"match_kind\":\"canonical-file\"")
        }));
        assert!(envelope.entities.iter().any(|entity| {
            entity
                .to_string()
                .contains("\"import\":\"import { helper } from \\\"@/lib/auth\\\";\"")
        }));
    }

    #[test]
    fn dead_code_query_flags_orphan_exported_function() {
        let root = unique_test_root("graph-dead-code");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".leio-code")).expect("create test root");
        fs::create_dir_all(root.join("server")).expect("create server dir");
        // `orphan_helper` is exported (no leading underscore), defined in a file
        // that is not imported by anyone, and never called from anywhere.
        fs::write(
            root.join("server/orphan.ts"),
            "export function orphanHelper() { return 1; }\n",
        )
        .expect("write orphan");
        // `wired_helper` is exported and reachable: another file imports it.
        fs::write(
            root.join("server/wired.ts"),
            "export function wiredHelper() { return 2; }\n",
        )
        .expect("write wired");
        fs::write(
            root.join("server/index.ts"),
            "import { wiredHelper } from \"./wired\";\nexport function bootstrap() { return wiredHelper(); }\n",
        )
        .expect("write index");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "2026-05-04T00:00:00Z".to_string(),
            files: vec![
                FileRecord {
                    path: "server/orphan.ts".to_string(),
                    language: SourceLanguage::TypeScript,
                    bytes: 50,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "server/wired.ts".to_string(),
                    language: SourceLanguage::TypeScript,
                    bytes: 50,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "server/index.ts".to_string(),
                    language: SourceLanguage::TypeScript,
                    bytes: 100,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
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

        let envelope = query_dead_code(&index, &root, 0).expect("dead-code query should succeed");

        assert!(
            envelope
                .entities
                .iter()
                .any(|entity| entity.to_string().contains("orphanHelper")),
            "expected orphanHelper to be flagged: {:?}",
            envelope.entities
        );
        assert!(
            !envelope
                .entities
                .iter()
                .skip(1) // skip header
                .any(|entity| entity.to_string().contains("wiredHelper")),
            "wiredHelper has importer; should not be flagged: {:?}",
            envelope.entities
        );
    }

    /// A component reachable only through an `index.ts` re-export barrel is
    /// live code. Before `export ... from` produced import edges, every such
    /// module reported 0 importers and its symbols were flagged dead — the
    /// exact false positive seen on cosmic-mirror's `components/skeletons`.
    #[test]
    fn dead_code_query_follows_reexport_barrels() {
        let root = unique_test_root("graph-dead-code-barrel");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".leio-code")).expect("create test root");
        fs::create_dir_all(root.join("client/skeletons")).expect("create skeletons dir");
        fs::write(
            root.join("client/skeletons/CircleSkeleton.tsx"),
            "export function CircleSkeleton() { return null; }\n",
        )
        .expect("write skeleton");
        // The barrel re-exports it; nothing imports the .tsx file directly.
        fs::write(
            root.join("client/skeletons/index.ts"),
            "export { CircleSkeleton } from \"./CircleSkeleton\";\n",
        )
        .expect("write barrel");
        fs::write(
            root.join("client/Page.tsx"),
            "import { CircleSkeleton } from \"./skeletons\";\nexport function Page() { return CircleSkeleton(); }\n",
        )
        .expect("write page");

        let file = |path: &str, bytes: usize| FileRecord {
            path: path.to_string(),
            language: SourceLanguage::Tsx,
            bytes,
            modified_unix_ms: 0,
            symbols: Vec::new(),
            env_vars: Vec::new(),
            redis_keys: Vec::new(),
            subprocess_calls: Vec::new(),
            http_calls: Vec::new(),
            unresolved_edges: Vec::new(),
        };
        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "2026-05-04T00:00:00Z".to_string(),
            files: vec![
                file("client/skeletons/CircleSkeleton.tsx", 50),
                FileRecord {
                    language: SourceLanguage::TypeScript,
                    ..file("client/skeletons/index.ts", 60)
                },
                file("client/Page.tsx", 100),
            ],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let envelope = query_dead_code(&index, &root, 0).expect("dead-code query should succeed");

        assert!(
            !envelope
                .entities
                .iter()
                .skip(1) // skip header
                .any(|entity| entity.to_string().contains("CircleSkeleton")),
            "CircleSkeleton is imported via the barrel; should not be flagged: {:?}",
            envelope.entities
        );
    }

    #[test]
    fn dead_code_query_skips_well_known_entrypoint_names_and_paths() {
        let root = unique_test_root("graph-dead-code-entrypoint");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".leio-code")).expect("create test root");
        fs::create_dir_all(root.join("src")).expect("create src dir");
        fs::create_dir_all(root.join("app/dashboard")).expect("create app dir");
        fs::create_dir_all(root.join("server")).expect("create server dir");
        // `main` is an entry point and must not be flagged even with zero callers.
        fs::write(
            root.join("src/main.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .expect("write main");
        // A page route under `/app/` — Next.js framework convention.
        fs::write(
            root.join("app/dashboard/page.tsx"),
            "export default function Page() { return null; }\n",
        )
        .expect("write page");
        // `_private` should not even be considered exported.
        fs::write(
            root.join("server/private.ts"),
            "export function _private() { return 1; }\n",
        )
        .expect("write private");
        // Test functions in conventional locations.
        fs::write(
            root.join("server/foo.test.ts"),
            "export function describeSomething() { return 1; }\n",
        )
        .expect("write test");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "2026-05-04T00:00:00Z".to_string(),
            files: vec![
                FileRecord {
                    path: "src/main.rs".to_string(),
                    language: SourceLanguage::Rust,
                    bytes: 40,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "app/dashboard/page.tsx".to_string(),
                    language: SourceLanguage::Tsx,
                    bytes: 50,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "server/private.ts".to_string(),
                    language: SourceLanguage::TypeScript,
                    bytes: 40,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "server/foo.test.ts".to_string(),
                    language: SourceLanguage::TypeScript,
                    bytes: 60,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
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

        let envelope = query_dead_code(&index, &root, 0).expect("dead-code query should succeed");

        for forbidden in ["fn main", "Page", "_private", "describeSomething"] {
            assert!(
                !envelope
                    .entities
                    .iter()
                    .skip(1)
                    .any(|entity| entity.to_string().contains(forbidden)),
                "expected entry-point/private symbol `{forbidden}` to be skipped, got entities: {:?}",
                envelope.entities
            );
        }
    }

    #[test]
    fn rdf_ontology_symbols_and_owl_imports_are_in_the_graph() {
        let root = unique_test_root("graph-ontology");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("ont")).expect("create ont");
        fs::create_dir_all(root.join(".leio-code")).expect("create leio dir");
        fs::write(
            root.join("ont/core.ttl"),
            r#"@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix ex: <http://example.org/leio-ont#> .
<http://example.org/leio-ont> a owl:Ontology ;
    owl:imports <http://example.org/leio-ont/imported> .
ex:Claim a owl:Class .
"#,
        )
        .expect("write core");
        fs::write(
            root.join("ont/imported.ttl"),
            r#"@prefix owl: <http://www.w3.org/2002/07/owl#> .
<http://example.org/leio-ont/imported> a owl:Ontology .
"#,
        )
        .expect("write imported");

        let rdf_file = |path: &str| FileRecord {
            path: path.to_string(),
            language: SourceLanguage::Rdf,
            bytes: 64,
            modified_unix_ms: 0,
            symbols: Vec::new(),
            env_vars: Vec::new(),
            redis_keys: Vec::new(),
            subprocess_calls: Vec::new(),
            http_calls: Vec::new(),
            unresolved_edges: Vec::new(),
        };
        let index = RepoIndex {
            version: crate::indexer::index_version(),
            root: root.display().to_string(),
            indexed_at: "2026-08-20T00:00:00Z".to_string(),
            files: vec![rdf_file("ont/core.ttl"), rdf_file("ont/imported.ttl")],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let symbols = query_symbols_in(&index, &root, "ont/core.ttl").expect("symbols-in");
        assert!(
            symbols
                .entities
                .iter()
                .any(|entity| entity.to_string().contains("Claim")),
            "{:?}",
            symbols.entities
        );

        let imports = query_imports_in(&index, &root, "ont/core.ttl").expect("imports-in");
        assert!(
            imports
                .entities
                .iter()
                .any(|entity| entity.to_string().contains("owl:imports")),
            "{:?}",
            imports.entities
        );

        let importers = query_importers_of(&index, &root, "http://example.org/leio-ont/imported")
            .expect("importers-of");
        assert!(
            importers
                .entities
                .iter()
                .any(|entity| entity.to_string().contains("ont/core.ttl")),
            "{:?}",
            importers.entities
        );
        let _ = fs::remove_dir_all(&root);
    }
}
