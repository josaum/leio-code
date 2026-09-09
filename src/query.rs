//! `find` and `explain` queries over an indexed [`RepoIndex`].
//!
//! This is the "operational meaning" surface — direct ownership lookups for the
//! coding agent. Every function returns a [`QueryEnvelope`] with evidence and a
//! confidence rating; no I/O outside reading the index plus on-demand reads of
//! evidence files (deploy YAMLs, cartridge manifests, etc.).
//!
//! Function families:
//! - `find_symbols`, `find_env_vars`, `find_redis_keys`, `find_deploy_targets`,
//!   `find_cartridges`, `find_api_routes`, `find_docker_services` — ranked lists
//!   of concrete occurrences.
//! - `explain_env_var`, `explain_redis_key`, `explain_deploy_target`,
//!   `explain_cartridge` — synthesized cross-file reads with provenance.
//!
//! API-route ranking (token IDF, family heuristics, mounted-vs-profile-activated
//! priority) is intentionally rich because the coding agent uses route lookups
//! as a primary navigation surface; see the inline test suite for the contract.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::time::Instant;

use crate::deploy_support::{
    command_references_existing_path, extract_rollback_target, extract_smoke_target,
};
use crate::doctors::utils::query_id;
use regex::Regex;
use serde_json::json;
use serde_yaml::{Mapping as YamlMapping, Value as YamlValue};
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use crate::model::{
    DeployTargetRecord, EvidenceItem, QueryEnvelope, RedisKeyOccurrence, RepoIndex,
    SymbolOccurrence,
};
use crate::search::{self, SearchIndex};

/// Best-effort sidecar handle for the given index. Returns `None` when the
/// Arrow-IPC file is missing, version-mismatched, older than the JSON index, or
/// fails to open — callers fall back to the linear-scan path.
fn try_open_sidecar(index: &RepoIndex) -> Option<SearchIndex> {
    if std::env::var("LEIO_DISABLE_SEARCH_SIDECAR")
        .ok()
        .as_deref()
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
    {
        return None;
    }
    let root = Path::new(&index.root);
    let path = search::default_search_db_path(root);

    // Drop the sidecar when it's older than the JSON index — that means the
    // user re-indexed with the sidecar disabled and we'd otherwise return
    // results from before the latest edits.
    let json_path = crate::indexer::default_index_path(root);
    if let (Ok(json_meta), Ok(side_meta)) = (fs::metadata(&json_path), fs::metadata(&path))
        && let (Ok(json_mtime), Ok(side_mtime)) = (json_meta.modified(), side_meta.modified())
        && json_mtime > side_mtime
    {
        return None;
    }

    SearchIndex::open_if_fresh(&path).ok().flatten()
}

pub fn find_symbols(index: &RepoIndex, needle: &str) -> QueryEnvelope {
    let started = Instant::now();
    let multiword = needle.split_whitespace().count() > 1 || needle.contains("fca");
    // Local Arrow wins only when search_as_find has a path-or-better top hit.
    if multiword
        && let Some(envelope) =
            crate::local_nodes::search_as_find(Path::new(&index.root), needle, 20)
    {
        return envelope;
    }

    if let Some(sidecar) = try_open_sidecar(index)
        && let Ok(hits) = sidecar.search_symbols(needle, None)
    {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("find_symbol"),
            kind: "find".to_string(),
            summary: format!("found {} symbol matches for `{}`", hits.len(), needle),
            confidence: confidence(hits.len()),
            entities: hits
                .iter()
                .map(|hit| {
                    json!({
                        "name": hit.name,
                        "qual_name": hit.qual_name,
                        "kind": hit.kind,
                        "path": hit.path,
                        "line": hit.line,
                        "language": hit.language,
                        "score": hit.score,
                    })
                })
                .collect(),
            evidence: hits
                .iter()
                .map(|hit| EvidenceItem {
                    kind: "symbol".to_string(),
                    path: hit.path.clone(),
                    line: Some(hit.line),
                    detail: format!("{} {}", hit.kind, hit.name),
                })
                .collect(),
            warnings: Vec::new(),
            meta: Some(json!({ "source": "search-arrow" })),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    // ASCII case-folding mirrors the sidecar (`search::tokenize`, the Arrow
    // columnar scan) so the two paths agree byte-for-byte on what a
    // needle matches. Identifiers in the supported languages are
    // overwhelmingly ASCII; ASCII folding is also faster than the Unicode
    // table walk.
    let query = needle.to_ascii_lowercase();
    let matches: Vec<&SymbolOccurrence> = index
        .all_symbols()
        .filter(|item| {
            item.name.to_ascii_lowercase().contains(&query)
                || item
                    .qual_name
                    .as_deref()
                    .is_some_and(|qual| qual.to_ascii_lowercase().contains(&query))
        })
        .collect();

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("find_symbol"),
        kind: "find".to_string(),
        summary: format!("found {} symbol matches for `{}`", matches.len(), needle),
        confidence: confidence(matches.len()),
        entities: matches
            .iter()
            .map(|item| {
                json!({
                    "name": item.name,
                    "qual_name": item.qual_name,
                    "kind": item.kind.as_str(),
                    "path": item.path,
                    "line": item.line,
                    "language": item.language.as_str(),
                })
            })
            .collect(),
        evidence: matches
            .iter()
            .map(|item| EvidenceItem {
                kind: "symbol".to_string(),
                path: item.path.clone(),
                line: Some(item.line),
                detail: format!("{} {}", item.kind.as_str(), item.name),
            })
            .collect(),
        warnings: Vec::new(),
        meta: Some(json!({ "source": "linear-scan" })),
        timing_ms: started.elapsed().as_millis(),
    }
}

pub fn find_env_vars(index: &RepoIndex, needle: &str) -> QueryEnvelope {
    let started = Instant::now();

    if let Some(sidecar) = try_open_sidecar(index)
        && let Ok(hits) = sidecar.search_env_vars(needle, None)
    {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("find_env"),
            kind: "find".to_string(),
            summary: format!("found {} env var matches for `{}`", hits.len(), needle),
            confidence: confidence(hits.len()),
            entities: hits
                .iter()
                .map(|hit| {
                    json!({
                        "name": hit.name,
                        "access": hit.access,
                        "path": hit.path,
                        "line": hit.line,
                        "language": hit.language,
                        "score": hit.score,
                    })
                })
                .collect(),
            evidence: hits
                .iter()
                .map(|hit| EvidenceItem {
                    kind: "env_var".to_string(),
                    path: hit.path.clone(),
                    line: Some(hit.line),
                    detail: format!("{} {}", hit.access, hit.name),
                })
                .collect(),
            warnings: Vec::new(),
            meta: Some(json!({ "source": "search-arrow" })),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    let query = needle.to_ascii_lowercase();
    let matches: Vec<_> = index
        .all_env_vars()
        .filter(|item| item.name.to_ascii_lowercase().contains(&query))
        .collect();

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("find_env"),
        kind: "find".to_string(),
        summary: format!("found {} env var matches for `{}`", matches.len(), needle),
        confidence: confidence(matches.len()),
        entities: matches
            .iter()
            .map(|item| {
                json!({
                    "name": item.name,
                    "access": item.access.as_str(),
                    "path": item.path,
                    "line": item.line,
                    "language": item.language.as_str(),
                })
            })
            .collect(),
        evidence: matches
            .iter()
            .map(|item| EvidenceItem {
                kind: "env_var".to_string(),
                path: item.path.clone(),
                line: Some(item.line),
                detail: format!("{} {}", item.access.as_str(), item.name),
            })
            .collect(),
        warnings: Vec::new(),
        meta: Some(json!({ "source": "linear-scan" })),
        timing_ms: started.elapsed().as_millis(),
    }
}

pub fn find_redis_keys(index: &RepoIndex, needle: &str) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let matcher = RedisKeyMatcher::compile(needle, &mut warnings);

    // Sidecar only handles the substring path — glob/regex matching stays on
    // the iterator path so the existing `*` / `?` semantics keep working.
    if matches!(matcher, RedisKeyMatcher::Substring(_))
        && let Some(sidecar) = try_open_sidecar(index)
        && let Ok(hits) = sidecar.search_redis_keys(needle, None)
    {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("find_redis"),
            kind: "find".to_string(),
            summary: format!("found {} redis key matches for `{}`", hits.len(), needle),
            confidence: confidence(hits.len()),
            entities: hits
                .iter()
                .map(|hit| {
                    json!({
                        "key": hit.key,
                        "access": hit.access,
                        "path": hit.path,
                        "line": hit.line,
                        "language": hit.language,
                        "score": hit.score,
                    })
                })
                .collect(),
            evidence: hits
                .iter()
                .map(|hit| EvidenceItem {
                    kind: "redis_key".to_string(),
                    path: hit.path.clone(),
                    line: Some(hit.line),
                    detail: format!("{} {}", hit.access, hit.key),
                })
                .collect(),
            warnings,
            meta: Some(json!({
                "source": "search-arrow",
                "match_mode": matcher.mode_label(),
            })),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    let matches: Vec<&RedisKeyOccurrence> = index
        .all_redis_keys()
        .filter(|item| matcher.matches(&item.key))
        .collect();

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("find_redis"),
        kind: "find".to_string(),
        summary: format!("found {} redis key matches for `{}`", matches.len(), needle),
        confidence: confidence(matches.len()),
        entities: matches
            .iter()
            .map(|item| {
                json!({
                    "key": item.key,
                    "access": item.access.as_str(),
                    "path": item.path,
                    "line": item.line,
                    "language": item.language.as_str(),
                })
            })
            .collect(),
        evidence: matches
            .iter()
            .map(|item| EvidenceItem {
                kind: "redis_key".to_string(),
                path: item.path.clone(),
                line: Some(item.line),
                detail: format!("{} {}", item.access.as_str(), item.key),
            })
            .collect(),
        warnings,
        meta: Some(json!({
            "source": "linear-scan",
            "match_mode": matcher.mode_label(),
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// Find Python files that spawn `binary` via a `subprocess.<fn>([literal, ...])` call.
///
/// Match is case-sensitive and exact on the literal first-argument string —
/// detection skipped the call entirely if the binary couldn't be read as a
/// plain string literal, so partial matches here would be misleading.
pub fn find_subprocess_callers(index: &RepoIndex, binary: &str) -> QueryEnvelope {
    let started = Instant::now();
    let matches: Vec<_> = index
        .all_subprocess_calls()
        .filter(|call| call.binary == binary)
        .collect();

    // Phase 3 (P0 #2): surface repo-wide unresolved spawn count alongside
    // resolved matches. The summary mentions both; the meta carries the full
    // list (callers can filter via `--where` on JSON-LD output) and a grouped
    // breakdown by reason for the text renderer.
    let unresolved = &index.cross_language.unresolved_edges;
    let unresolved_count = unresolved.len();
    let unresolved_breakdown = group_unresolved_by_reason(unresolved);

    let summary = if unresolved_count == 0 {
        format!(
            "found {} subprocess caller(s) for `{}`",
            matches.len(),
            binary
        )
    } else {
        format!(
            "found {} subprocess caller(s) for `{}`; {} unresolved spawn edge(s) repo-wide ({})",
            matches.len(),
            binary,
            unresolved_count,
            format_unresolved_breakdown(&unresolved_breakdown),
        )
    };

    let mut meta_obj = serde_json::Map::new();
    meta_obj.insert("source".to_string(), json!("linear-scan"));
    meta_obj.insert("unresolved_count".to_string(), json!(unresolved_count));
    meta_obj.insert(
        "unresolved_by_reason".to_string(),
        json!(unresolved_breakdown),
    );
    meta_obj.insert(
        "unresolved_edges".to_string(),
        serde_json::to_value(unresolved).unwrap_or(json!([])),
    );

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("find_subprocess"),
        kind: "find".to_string(),
        summary,
        confidence: confidence(matches.len()),
        entities: matches
            .iter()
            .map(|call| {
                json!({
                    "binary": call.binary,
                    "path": call.path,
                    "line": call.line,
                    "language": call.language.as_str(),
                })
            })
            .collect(),
        evidence: matches
            .iter()
            .map(|call| EvidenceItem {
                kind: "subprocess_call".to_string(),
                path: call.path.clone(),
                line: Some(call.line),
                detail: format!("subprocess(...{}...)", call.binary),
            })
            .collect(),
        warnings: Vec::new(),
        meta: Some(serde_json::Value::Object(meta_obj)),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// Group unresolved edges by their `UnresolvedReason` for the grouped count
/// summary. The key is the reason's variant name; stable ordering by variant
/// name (BTreeMap) keeps the output reproducible.
fn group_unresolved_by_reason(
    edges: &[crate::model::UnresolvedEdge],
) -> std::collections::BTreeMap<String, usize> {
    let mut out: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for edge in edges {
        let key = reason_variant_name(&edge.reason);
        *out.entry(key).or_insert(0) += 1;
    }
    out
}

fn reason_variant_name(reason: &crate::model::UnresolvedReason) -> String {
    use crate::model::UnresolvedReason::*;
    match reason {
        NonLiteralFirstArg => "NonLiteralFirstArg".to_string(),
        TemplateOrConcat => "TemplateOrConcat".to_string(),
        ShellInvocation => "ShellInvocation".to_string(),
        MakefileVariable => "MakefileVariable".to_string(),
        NpmVariable => "NpmVariable".to_string(),
        AmbiguousCommandImport => "AmbiguousCommandImport".to_string(),
        AmbiguousAssignment => "AmbiguousAssignment".to_string(),
        Other(tag) => format!("Other({tag})"),
    }
}

fn format_unresolved_breakdown(counts: &std::collections::BTreeMap<String, usize>) -> String {
    counts
        .iter()
        .map(|(reason, n)| format!("{n} {reason}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// List all binary nodes in the index, optionally filtered by name substring.
///
/// Phase 7 (P0 #2): exposes the binary registry built in Phase 2 so users can
/// answer "what binaries does this repo declare?" without reading the index
/// JSON directly. `filter` is a case-insensitive substring match on
/// `BinaryNode::name`; pass an empty string for "no filter".
pub fn find_binaries(index: &crate::model::RepoIndex, filter: &str) -> QueryEnvelope {
    let started = Instant::now();
    let needle = filter.to_ascii_lowercase();
    let matches: Vec<&crate::model::BinaryNode> = index
        .cross_language
        .binaries
        .iter()
        .filter(|b| needle.is_empty() || b.name.to_ascii_lowercase().contains(&needle))
        .collect();
    let summary = if filter.is_empty() {
        format!("found {} binary node(s)", matches.len())
    } else {
        format!(
            "found {} binary node(s) matching `{}`",
            matches.len(),
            filter
        )
    };
    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("find_binary"),
        kind: "find".to_string(),
        summary,
        confidence: confidence(matches.len()),
        entities: matches
            .iter()
            .map(|b| {
                json!({
                    "name": b.name,
                    "path": b.path,
                    "source": b.source,
                })
            })
            .collect(),
        evidence: matches
            .iter()
            .map(|b| EvidenceItem {
                kind: "binary_node".to_string(),
                path: b.path.clone(),
                line: None,
                detail: format!("binary `{}` from {:?}", b.name, b.source),
            })
            .collect(),
        warnings: Vec::new(),
        meta: Some(json!({
            "source": "cross_language.binaries",
            "filter": filter,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// List all HTTP routes in the index, optionally filtered by route substring
/// and HTTP method.
///
/// Phase 7 (P0 #2): exposes the route registry built in Phase 5 so users can
/// answer "what HTTP routes does this repo declare?" without reading the index
/// JSON directly. `filter` is a case-insensitive substring match on
/// `RouteRecord::route`; pass empty string for "no filter". `method`, when
/// set, is matched case-insensitively against `RouteRecord::method`.
pub fn find_routes(
    index: &crate::model::RepoIndex,
    filter: &str,
    method: Option<&str>,
) -> QueryEnvelope {
    let started = Instant::now();
    let needle = filter.to_ascii_lowercase();
    let method_upper = method.map(|m| m.to_ascii_uppercase());
    let matches: Vec<&crate::model::RouteRecord> = index
        .cross_language
        .routes
        .iter()
        .filter(|r| needle.is_empty() || r.route.to_ascii_lowercase().contains(&needle))
        .filter(|r| match &method_upper {
            Some(m) => r.method.eq_ignore_ascii_case(m),
            None => true,
        })
        .collect();
    let summary = match (filter.is_empty(), &method_upper) {
        (true, None) => format!("found {} route(s)", matches.len()),
        (false, None) => format!("found {} route(s) matching `{}`", matches.len(), filter),
        (true, Some(m)) => format!("found {} {} route(s)", matches.len(), m),
        (false, Some(m)) => format!(
            "found {} {} route(s) matching `{}`",
            matches.len(),
            m,
            filter
        ),
    };
    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("find_route"),
        kind: "find".to_string(),
        summary,
        confidence: confidence(matches.len()),
        entities: matches
            .iter()
            .map(|r| {
                json!({
                    "route": r.route,
                    "method": r.method,
                    "framework": r.framework,
                    "path": r.path,
                    "line": r.line,
                    "language": r.language.as_str(),
                })
            })
            .collect(),
        evidence: matches
            .iter()
            .map(|r| EvidenceItem {
                kind: "route_declaration".to_string(),
                path: r.path.clone(),
                line: Some(r.line),
                detail: format!("{} {} ({})", r.method, r.route, r.framework),
            })
            .collect(),
        warnings: Vec::new(),
        meta: Some(json!({
            "source": "cross_language.routes",
            "filter": filter,
            "method": method,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// List HTTP callers of a route — i.e. resolved client→route edges whose
/// `route_path` equals `route`. Phase 7 companion to [`find_subprocess_callers`].
pub fn find_route_callers(index: &crate::model::RepoIndex, route: &str) -> QueryEnvelope {
    let started = Instant::now();
    let matches: Vec<&crate::model::ResolvedHttpEdge> = index
        .cross_language
        .resolved_http_edges
        .iter()
        .filter(|edge| edge.route_path == route)
        .collect();
    let summary = format!(
        "found {} HTTP caller(s) for route `{}`",
        matches.len(),
        route
    );
    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("find_callers_route"),
        kind: "find".to_string(),
        summary,
        confidence: confidence(matches.len()),
        entities: matches
            .iter()
            .map(|edge| {
                json!({
                    "caller_path": edge.caller_path,
                    "caller_line": edge.caller_line,
                    "caller_language": edge.caller_language.as_str(),
                    "route_path": edge.route_path,
                    "route_method": edge.route_method,
                    "route_source_path": edge.route_source_path,
                    "confidence": edge.confidence,
                })
            })
            .collect(),
        evidence: matches
            .iter()
            .map(|edge| EvidenceItem {
                kind: "http_call".to_string(),
                path: edge.caller_path.clone(),
                line: Some(edge.caller_line),
                detail: format!("{} {}", edge.route_method, edge.route_path),
            })
            .collect(),
        warnings: Vec::new(),
        meta: Some(json!({
            "source": "cross_language.resolved_http_edges",
            "route": route,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// Binary-mode wrapper around [`find_subprocess_callers`] with a distinct
/// `query_id` prefix so JSON-LD `@type` mapping discriminates spawn vs. http
/// callers without inspecting entity content. Behavior is identical to
/// `find_subprocess_callers` aside from the prefix.
pub fn find_binary_callers(index: &crate::model::RepoIndex, binary: &str) -> QueryEnvelope {
    let mut envelope = find_subprocess_callers(index, binary);
    envelope.query_id = query_id("find_callers_binary");
    envelope
}

/// Explain a binary: where it's declared + every resolved caller + a
/// best-effort filter of unresolved edges whose `raw_snippet` mentions the
/// binary name. The unresolved filter is heuristic — `UnresolvedEdge` doesn't
/// carry a binary-name field — and is surfaced as such in `meta`.
pub fn explain_binary(index: &crate::model::RepoIndex, name: &str) -> QueryEnvelope {
    let started = Instant::now();
    let binary = index
        .cross_language
        .binaries
        .iter()
        .find(|b| b.name == name);
    let callers: Vec<&crate::model::ResolvedSpawnEdge> = index
        .cross_language
        .resolved_spawns
        .iter()
        .filter(|edge| edge.callee_name == name)
        .collect();
    // Heuristic: surface unresolved edges whose raw_snippet mentions the binary
    // name. UnresolvedEdge has no callee-name field (the whole point of
    // "unresolved" is that we couldn't pin one down), so this is best-effort.
    let unresolved: Vec<&crate::model::UnresolvedEdge> = index
        .cross_language
        .unresolved_edges
        .iter()
        .filter(|edge| edge.raw_snippet.contains(name))
        .collect();
    let unresolved_breakdown = {
        let owned: Vec<crate::model::UnresolvedEdge> =
            unresolved.iter().map(|e| (*e).clone()).collect();
        group_unresolved_by_reason(&owned)
    };

    let summary = match binary {
        Some(b) => format!(
            "binary `{}` at {} ({:?}); {} resolved caller(s)",
            b.name,
            b.path,
            b.source,
            callers.len()
        ),
        None => format!(
            "binary `{}` not declared in this repo; {} resolved caller(s)",
            name,
            callers.len()
        ),
    };

    let mut entities: Vec<serde_json::Value> = Vec::new();
    if let Some(b) = binary {
        entities.push(json!({
            "@kind": "Binary",
            "name": b.name,
            "path": b.path,
            "source": b.source,
        }));
    }
    for edge in &callers {
        entities.push(json!({
            "@kind": "SpawnCallSite",
            "caller_path": edge.caller_path,
            "caller_line": edge.caller_line,
            "caller_language": edge.caller_language.as_str(),
            "callee_name": edge.callee_name,
            "callee_path": edge.callee_path,
            "confidence": edge.confidence,
        }));
    }
    for edge in &unresolved {
        entities.push(json!({
            "@kind": "UnresolvedEdge",
            "source_path": edge.source_path,
            "source_line": edge.source_line,
            "source_language": edge.source_language.as_str(),
            "edge_kind": edge.edge_kind,
            "reason": reason_variant_name(&edge.reason),
            "raw_snippet": edge.raw_snippet,
        }));
    }

    let mut evidence: Vec<EvidenceItem> = Vec::new();
    if let Some(b) = binary {
        evidence.push(EvidenceItem {
            kind: "binary_declaration".to_string(),
            path: b.path.clone(),
            line: None,
            detail: format!("binary `{}` ({:?})", b.name, b.source),
        });
    }
    for edge in &callers {
        evidence.push(EvidenceItem {
            kind: "spawn_caller".to_string(),
            path: edge.caller_path.clone(),
            line: Some(edge.caller_line),
            detail: format!("spawns `{}`", edge.callee_name),
        });
    }

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("explain_binary"),
        kind: "explain".to_string(),
        summary,
        confidence: if binary.is_some() { 0.95 } else { 0.5 },
        entities,
        evidence,
        warnings: Vec::new(),
        meta: Some(json!({
            "source": "cross_language",
            "resolved_caller_count": callers.len(),
            "unresolved_match_count": unresolved.len(),
            "unresolved_by_reason": unresolved_breakdown,
            "unresolved_filter": "best-effort substring on raw_snippet",
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// Explain a route: where it's declared + every resolved HTTP caller.
pub fn explain_route(index: &crate::model::RepoIndex, route: &str) -> QueryEnvelope {
    let started = Instant::now();
    let declarations: Vec<&crate::model::RouteRecord> = index
        .cross_language
        .routes
        .iter()
        .filter(|r| r.route == route)
        .collect();
    let callers: Vec<&crate::model::ResolvedHttpEdge> = index
        .cross_language
        .resolved_http_edges
        .iter()
        .filter(|edge| edge.route_path == route)
        .collect();

    let summary = format!(
        "route `{}`: {} declaration(s); {} resolved caller(s)",
        route,
        declarations.len(),
        callers.len()
    );

    let mut entities: Vec<serde_json::Value> = Vec::new();
    for decl in &declarations {
        entities.push(json!({
            "@kind": "Route",
            "route": decl.route,
            "method": decl.method,
            "framework": decl.framework,
            "handler": decl.handler,
            "auth_hint": decl.auth_hint,
            "path": decl.path,
            "line": decl.line,
            "language": decl.language.as_str(),
        }));
    }
    for edge in &callers {
        entities.push(json!({
            "@kind": "HttpCallSite",
            "caller_path": edge.caller_path,
            "caller_line": edge.caller_line,
            "caller_language": edge.caller_language.as_str(),
            "route_path": edge.route_path,
            "route_method": edge.route_method,
            "route_source_path": edge.route_source_path,
            "confidence": edge.confidence,
        }));
    }

    let mut evidence: Vec<EvidenceItem> = Vec::new();
    for decl in &declarations {
        evidence.push(EvidenceItem {
            kind: "route_declaration".to_string(),
            path: decl.path.clone(),
            line: Some(decl.line),
            detail: format!("{} {} ({})", decl.method, decl.route, decl.framework),
        });
    }
    for edge in &callers {
        evidence.push(EvidenceItem {
            kind: "http_call".to_string(),
            path: edge.caller_path.clone(),
            line: Some(edge.caller_line),
            detail: format!("{} {}", edge.route_method, edge.route_path),
        });
    }

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("explain_route"),
        kind: "explain".to_string(),
        summary,
        confidence: if declarations.is_empty() { 0.5 } else { 0.95 },
        entities,
        evidence,
        warnings: Vec::new(),
        meta: Some(json!({
            "source": "cross_language",
            "declaration_count": declarations.len(),
            "resolved_caller_count": callers.len(),
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// Matcher for `find redis-key`. Switches between substring and glob/regex modes
/// based on the needle's syntax: `*` / `?` enable glob mode; a literal substring
/// otherwise (case-insensitive). Templated literals like `session:{id}` are
/// matched against indexed forms by treating the indexed `{...}` segments as
/// glob wildcards too — that way `find redis-key "session:abc"` won't match a
/// literal `session:{id}`, but a glob `session:*` will.
enum RedisKeyMatcher {
    Substring(String),
    Glob(Regex),
}

impl RedisKeyMatcher {
    fn compile(needle: &str, warnings: &mut Vec<String>) -> Self {
        if needle.contains('*') || needle.contains('?') {
            let pattern = glob_to_regex(needle);
            match Regex::new(&pattern) {
                Ok(re) => Self::Glob(re),
                Err(err) => {
                    warnings.push(format!(
                        "redis-key glob `{needle}` failed to compile ({err}); falling back to substring"
                    ));
                    Self::Substring(needle.to_ascii_lowercase())
                }
            }
        } else {
            Self::Substring(needle.to_ascii_lowercase())
        }
    }

    fn matches(&self, key: &str) -> bool {
        match self {
            Self::Substring(needle) => key.to_ascii_lowercase().contains(needle),
            Self::Glob(re) => re.is_match(key),
        }
    }

    fn mode_label(&self) -> &'static str {
        match self {
            Self::Substring(_) => "substring",
            Self::Glob(_) => "glob",
        }
    }
}

fn glob_to_regex(glob: &str) -> String {
    let mut out = String::from("(?i)^");
    let chars = glob.chars().peekable();
    for ch in chars {
        match ch {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            '.' | '+' | '(' | ')' | '|' | '^' | '$' | '\\' | '{' | '}' | '[' | ']' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out.push('$');
    out
}

pub fn find_deploy_targets(index: &RepoIndex, needle: &str) -> QueryEnvelope {
    let started = Instant::now();
    let facets = index.workspace_facets();
    if !facets.has_deploy_topology {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("find_deploy_target"),
            kind: "find".to_string(),
            summary: "repository does not model deploy targets".to_string(),
            confidence: 0.96,
            entities: Vec::new(),
            evidence: Vec::new(),
            warnings: Vec::new(),
            meta: Some(json!({
                "facet": "deploy_targets",
                "facet_available": false,
                "workspace_facets": facets,
            })),
            timing_ms: started.elapsed().as_millis(),
        };
    }
    let query = needle.to_ascii_lowercase();
    let matches: Vec<&DeployTargetRecord> = index
        .deploy_targets
        .iter()
        .filter(|item| item.name.to_ascii_lowercase().contains(&query))
        .collect();

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("find_deploy_target"),
        kind: "find".to_string(),
        summary: format!(
            "found {} deploy target matches for `{}`",
            matches.len(),
            needle
        ),
        confidence: confidence(matches.len()),
        entities: matches
            .iter()
            .map(|item| {
                json!({
                    "name": item.name,
                    "backend_profile": item.backend_profile,
                    "secret_set": item.secret_set,
                    "topology": item.topology,
                    "frontend_project": item.frontend_project,
                })
            })
            .collect(),
        evidence: matches
            .iter()
            .map(|item| EvidenceItem {
                kind: "deploy_target".to_string(),
                path: item.path.clone(),
                line: None,
                detail: format!("target {}", item.name),
            })
            .collect(),
        warnings: Vec::new(),
        meta: Some(json!({
            "facet": "deploy_targets",
            "facet_available": true,
            "workspace_facets": facets,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

pub fn explain_deploy_target(
    index: &RepoIndex,
    name: &str,
    root: &Path,
    opts: crate::value_resolution::ValueResolutionOpts,
) -> QueryEnvelope {
    let started = Instant::now();
    let facets = index.workspace_facets();
    if !facets.has_deploy_topology {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("explain_deploy_target"),
            kind: "explain".to_string(),
            summary: "repository does not model deploy targets".to_string(),
            confidence: 0.96,
            entities: Vec::new(),
            evidence: Vec::new(),
            warnings: Vec::new(),
            meta: Some(json!({
                "facet": "deploy_targets",
                "facet_available": false,
                "workspace_facets": facets,
            })),
            timing_ms: started.elapsed().as_millis(),
        };
    }
    let target = index.deploy_targets.iter().find(|item| item.name == name);
    let Some(target) = target else {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("explain_deploy_target"),
            kind: "explain".to_string(),
            summary: format!("deploy target `{}` not found", name),
            confidence: 0.0,
            entities: Vec::new(),
            evidence: Vec::new(),
            warnings: vec!["target not indexed".to_string()],
            meta: Some(json!({
                "facet": "deploy_targets",
                "facet_available": true,
                "workspace_facets": facets,
            })),
            timing_ms: started.elapsed().as_millis(),
        };
    };

    let profile_name = target
        .backend_profile
        .as_deref()
        .map(|value| format!("{value}.env"));
    let profile = profile_name
        .as_deref()
        .and_then(|needle| index.profiles.iter().find(|item| item.name == needle));
    let secret_name = target
        .secret_set
        .as_deref()
        .map(|value| format!("{value}.env.example"));
    let secret_set = secret_name
        .as_deref()
        .and_then(|needle| index.secret_sets.iter().find(|item| item.name == needle));

    let smoke_exists = target
        .smoke_suite
        .as_deref()
        .map(|command| command_references_existing_path(command, &root.join("deploy")))
        .unwrap_or(false);
    let smoke_target = target.smoke_suite.as_deref().and_then(extract_smoke_target);
    let rollback_exists = target
        .rollback_command
        .as_deref()
        .map(|command| command_references_existing_path(command, &root.join("deploy")))
        .unwrap_or(false);
    let rollback_target = target
        .rollback_command
        .as_deref()
        .and_then(extract_rollback_target);
    let expected_readiness_target = target
        .readiness_target
        .as_deref()
        .unwrap_or(target.name.as_str());
    let expected_smoke_target = if target.name == "sentinel" {
        target.name.as_str()
    } else {
        expected_readiness_target
    };
    let readiness_target_exists = index
        .deploy_targets
        .iter()
        .any(|item| item.name == expected_readiness_target);

    let mut warnings = Vec::new();
    if target.smoke_suite.is_some() && !smoke_exists {
        warnings.push("smoke suite command references a missing script or path".to_string());
    }
    if target.smoke_suite.is_some() && smoke_target.as_deref() != Some(expected_smoke_target) {
        warnings.push(format!(
            "smoke suite target must resolve to `{expected_smoke_target}`"
        ));
    }
    if target.rollback_command.is_some() && !rollback_exists {
        warnings.push("rollback command references a missing script or path".to_string());
    }
    if target.rollback_command.is_some()
        && rollback_target.as_deref() != Some(expected_readiness_target)
    {
        warnings.push(format!(
            "rollback command target must resolve to `{expected_readiness_target}`"
        ));
    }
    if target.backend_profile.is_some() && profile.is_none() {
        warnings.push("backend profile not found under deploy/profiles".to_string());
    }
    if target.secret_set.is_some() && secret_set.is_none() {
        warnings.push("secret set not found under deploy/secret-sets".to_string());
    }
    if target.readiness_target.is_some() && !readiness_target_exists {
        warnings.push("readiness target does not exist under deploy/targets".to_string());
    }

    // Resolve effective env-var values for every var the target declares via
    // its backend profile or secret set. Mirrors what `explain env-var` does,
    // but scoped to this target. Empty object when neither side declares any.
    let declared_vars: BTreeSet<&str> = profile
        .iter()
        .flat_map(|p| p.vars.iter())
        .chain(secret_set.iter().flat_map(|s| s.vars.iter()))
        .map(|v| v.name.as_str())
        .collect();
    let mut var_bindings: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for var_name in declared_vars {
        let bindings = crate::value_resolution::resolve_value_bindings(var_name, index, &opts);
        let is_secret = crate::value_resolution::is_secret_key(var_name);
        let effective = bindings
            .first()
            .map(|b| {
                json!({
                    "state": b.state.as_str(),
                    "display": b.display,
                    "redacted": b.redacted,
                    "source": b.source,
                })
            })
            .unwrap_or_else(|| json!({ "state": "unset", "display": "", "redacted": false }));
        let bindings_json: Vec<serde_json::Value> = bindings
            .iter()
            .map(|b| {
                json!({
                    "state": b.state.as_str(),
                    "display": b.display,
                    "redacted": b.redacted,
                    "source": b.source,
                })
            })
            .collect();
        var_bindings.insert(
            var_name.to_string(),
            json!({
                "is_secret": is_secret,
                "bindings": bindings_json,
                "effective": effective,
            }),
        );
    }

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("explain_deploy_target"),
        kind: "explain".to_string(),
        summary: format!(
            "target `{}` -> profile {:?}, secret_set {:?}, {} health checks",
            target.name,
            target.backend_profile,
            target.secret_set,
            target.health_checks.len()
        ),
        confidence: 0.92,
        entities: vec![json!({
            "name": target.name,
            "path": target.path,
            "deploy_class": target.deploy_class,
            "topology": target.topology,
            "ui_role": target.ui_role,
            "ui_path": target.ui_path,
            "frontend_project": target.frontend_project,
            "backend_profile": target.backend_profile,
            "readiness_target": target.readiness_target,
            "readiness_target_exists": readiness_target_exists,
            "secret_set": target.secret_set,
            "cartridges": target.cartridges,
            "required_integrations": target.required_integrations,
            "health_checks": target.health_checks,
            "smoke_suite": target.smoke_suite,
            "smoke_exists": smoke_exists,
            "smoke_target": smoke_target,
            "rollback_command": target.rollback_command,
            "rollback_exists": rollback_exists,
            "rollback_target": rollback_target,
            "profile_vars": profile.map(|item| item.vars.iter().map(|var| var.name.clone()).collect::<Vec<_>>()).unwrap_or_default(),
            "secret_vars": secret_set.map(|item| item.vars.iter().map(|var| var.name.clone()).collect::<Vec<_>>()).unwrap_or_default(),
            "var_bindings": var_bindings,
        })],
        evidence: vec![
            EvidenceItem {
                kind: "deploy_target".to_string(),
                path: target.path.clone(),
                line: None,
                detail: format!("manifest {}", target.name),
            },
            EvidenceItem {
                kind: "backend_profile".to_string(),
                path: profile.map(|item| item.path.clone()).unwrap_or_default(),
                line: None,
                detail: target.backend_profile.clone().unwrap_or_default(),
            },
            EvidenceItem {
                kind: "secret_set".to_string(),
                path: secret_set.map(|item| item.path.clone()).unwrap_or_default(),
                line: None,
                detail: target.secret_set.clone().unwrap_or_default(),
            },
        ],
        warnings,
        meta: Some(json!({
            "facet": "deploy_targets",
            "facet_available": true,
            "workspace_facets": facets,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

pub fn explain_env_var(
    index: &RepoIndex,
    name: &str,
    opts: crate::value_resolution::ValueResolutionOpts,
) -> QueryEnvelope {
    let started = Instant::now();
    let uses: Vec<_> = index
        .all_env_vars()
        .filter(|item| item.name == name)
        .collect();
    let profiles: Vec<_> = index
        .profiles
        .iter()
        .filter(|profile| profile.vars.iter().any(|var| var.name == name))
        .collect();
    let secret_sets: Vec<_> = index
        .secret_sets
        .iter()
        .filter(|secret| secret.vars.iter().any(|var| var.name == name))
        .collect();
    let value_bindings = crate::value_resolution::resolve_value_bindings(name, index, &opts);
    let is_secret = crate::value_resolution::is_secret_key(name);
    let effective = value_bindings
        .first()
        .map(|b| {
            json!({
                "state": b.state.as_str(),
                "display": b.display,
                "redacted": b.redacted,
                "source": b.source,
            })
        })
        .unwrap_or_else(|| json!({ "state": "unset", "display": "", "redacted": false }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("explain_env"),
        kind: "explain".to_string(),
        summary: format!(
            "env var `{}` -> {} code uses, {} profiles, {} secret sets, {} value bindings",
            name,
            uses.len(),
            profiles.len(),
            secret_sets.len(),
            value_bindings.len()
        ),
        confidence: 0.9,
        entities: vec![json!({
            "name": name,
            "is_secret": is_secret,
            "code_uses": uses.iter().map(|item| json!({
                "path": item.path,
                "line": item.line,
                "access": item.access.as_str(),
                "language": item.language.as_str(),
            })).collect::<Vec<_>>(),
            "profiles": profiles.iter().map(|item| item.name.clone()).collect::<Vec<_>>(),
            "secret_sets": secret_sets.iter().map(|item| item.name.clone()).collect::<Vec<_>>(),
            "value_bindings": value_bindings.iter().map(|b| json!({
                "state": b.state.as_str(),
                "display": b.display,
                "redacted": b.redacted,
                "source": b.source,
            })).collect::<Vec<_>>(),
            "effective": effective,
        })],
        evidence: uses
            .iter()
            .map(|item| EvidenceItem {
                kind: "env_var".to_string(),
                path: item.path.clone(),
                line: Some(item.line),
                detail: format!("{} {}", item.access.as_str(), item.name),
            })
            .chain(profiles.iter().map(|item| EvidenceItem {
                kind: "profile".to_string(),
                path: item.path.clone(),
                line: None,
                detail: format!("declares {}", name),
            }))
            .chain(secret_sets.iter().map(|item| EvidenceItem {
                kind: "secret_set".to_string(),
                path: item.path.clone(),
                line: None,
                detail: format!("declares {}", name),
            }))
            .collect(),
        warnings: Vec::new(),
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

pub fn explain_redis_key(index: &RepoIndex, key: &str) -> QueryEnvelope {
    let started = Instant::now();
    let matches: Vec<_> = index
        .all_redis_keys()
        .filter(|item| item.key == key)
        .collect();

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("explain_redis"),
        kind: "explain".to_string(),
        summary: format!("redis key `{}` -> {} indexed uses", key, matches.len()),
        confidence: confidence(matches.len()),
        entities: vec![json!({
            "key": key,
            "uses": matches.iter().map(|item| json!({
                "path": item.path,
                "line": item.line,
                "access": item.access.as_str(),
                "language": item.language.as_str(),
            })).collect::<Vec<_>>(),
        })],
        evidence: matches
            .iter()
            .map(|item| EvidenceItem {
                kind: "redis_key".to_string(),
                path: item.path.clone(),
                line: Some(item.line),
                detail: format!("{} {}", item.access.as_str(), item.key),
            })
            .collect(),
        warnings: Vec::new(),
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

pub fn find_cartridges(index: &RepoIndex, needle: &str) -> QueryEnvelope {
    let started = Instant::now();
    let facets = index.workspace_facets();
    if !facets.has_cartridges {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("find_cartridge"),
            kind: "find".to_string(),
            summary: "repository does not model cartridges".to_string(),
            confidence: 0.96,
            entities: Vec::new(),
            evidence: Vec::new(),
            warnings: Vec::new(),
            meta: Some(json!({
                "facet": "cartridges",
                "facet_available": false,
                "workspace_facets": facets,
            })),
            timing_ms: started.elapsed().as_millis(),
        };
    }
    let query = needle.to_ascii_lowercase();
    let activation_profiles = cartridge_activation_profiles(index);

    let mut cartridge_names: Vec<String> = index.cartridge_names().into_iter().collect();
    cartridge_names.sort();
    cartridge_names.dedup();

    let matches: Vec<&str> = cartridge_names
        .iter()
        .filter(|c| c.to_ascii_lowercase().contains(&query))
        .map(|c| c.as_str())
        .collect();

    // For each matched cartridge, find which deploy targets load it
    let entities: Vec<_> = matches
        .iter()
        .map(|cart| {
            let targets: Vec<&str> = index
                .deploy_targets
                .iter()
                .filter(|t| t.cartridges.iter().any(|c| c == cart))
                .map(|t| t.name.as_str())
                .collect();
            let source_file_count = index
                .files
                .iter()
                .filter(|file| file.path.starts_with(&format!("cartridges/{cart}/")))
                .count();
            let activation_profiles = activation_profiles.get(*cart).cloned().unwrap_or_default();
            json!({
                "cartridge": cart,
                "directory": format!("cartridges/{}", cart),
                "deploy_targets": targets,
                "deploy_target_count": targets.len(),
                "activation_profiles": activation_profiles,
                "source_file_count": source_file_count,
                "deployment_state": if targets.is_empty() { "undeployed" } else { "deployed" },
            })
        })
        .collect();

    // Also find cartridge source files
    let source_evidence: Vec<EvidenceItem> = matches
        .iter()
        .flat_map(|cart| {
            let cart_name = cart.to_string();
            let prefix = format!("cartridges/{}", cart);
            index
                .files
                .iter()
                .filter(move |f| f.path.starts_with(&prefix))
                .take(5) // cap at 5 files per cartridge
                .map(move |f| EvidenceItem {
                    kind: "cartridge_file".to_string(),
                    path: f.path.clone(),
                    line: None,
                    detail: format!("part of cartridge {}", cart_name),
                })
        })
        .collect();

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("find_cartridge"),
        kind: "find".to_string(),
        summary: format!(
            "found {} cartridge matches for `{}`, deployed across {} targets",
            matches.len(),
            needle,
            entities
                .iter()
                .map(|e| e["deploy_target_count"].as_u64().unwrap_or(0))
                .sum::<u64>()
        ),
        confidence: confidence(matches.len()),
        entities,
        evidence: source_evidence,
        warnings: Vec::new(),
        meta: Some(json!({
            "facet": "cartridges",
            "facet_available": true,
            "workspace_facets": facets,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// Candidates for routes the INDEXER already extracted (axum `.route(...)`,
/// Express `app.get(...)`) but the Python re-extraction pass above never
/// sees — without this, `find api-route` is blind to every non-Python route
/// even though they sit in `cross_language.routes`.
/// Python records are skipped: the pass above re-parses them with richer
/// mount/auth/role metadata.
fn push_indexed_route_candidates(
    index: &RepoIndex,
    query: Option<&str>,
    route_candidates: &mut Vec<ApiRouteCandidate>,
    seen: &mut HashSet<String>,
) {
    for record in &index.cross_language.routes {
        if record.language == crate::model::SourceLanguage::Python {
            continue;
        }
        if let Some(query) = query {
            let probe = PythonRouteMatch {
                router_name: record.framework.clone(),
                router_tags: Vec::new(),
                method: record.method.clone(),
                full_path: record.route.clone(),
                handler: String::new(),
                line: record.line,
                include_in_schema: true,
                auth_dependency: None,
            };
            if !route_matches_query(&query.to_ascii_lowercase(), &probe) {
                continue;
            }
        }
        let dedupe_key = format!(
            "indexed_route:{}:{}:{}:{}",
            record.path, record.line, record.method, record.route
        );
        if !seen.insert(dedupe_key) {
            continue;
        }
        route_candidates.push(ApiRouteCandidate {
            full_path: record.route.clone(),
            method: record.method.clone(),
            handler: record.handler.clone().unwrap_or_default(),
            mounted: true,
            mount_status: "standalone".to_string(),
            activation_profiles: None,
            public: None,
            auth_policy: record
                .auth_hint
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            auth_source: record
                .auth_hint
                .as_deref()
                .map(|hint| format!("indexer:{hint}"))
                .unwrap_or_default(),
            route_role: "canonical".to_string(),
            canonical_path: None,
            route_family: record.framework.clone(),
            route_family_label: record.framework.clone(),
            file_path: record.path.clone(),
            line: record.line,
            language: record.language.as_str().to_string(),
        });
    }
}

pub fn find_api_routes(index: &RepoIndex, needle: &str) -> QueryEnvelope {
    let started = Instant::now();
    let query = needle.to_ascii_lowercase();
    let example_registry = example_router_registry(index);
    let core_auth_policy = load_core_auth_policy(index);
    let active_env_cartridges = active_cartridges_from_env();
    let profile_cartridges = cartridge_activation_profiles(index);

    let mut matches: Vec<serde_json::Value> = Vec::new();
    let mut evidence: Vec<EvidenceItem> = Vec::new();
    let mut route_candidates: Vec<ApiRouteCandidate> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let summary: String;

    for file in &index.files {
        let path_lower = file.path.to_ascii_lowercase();
        let looks_like_route_file = path_lower.contains("router")
            || path_lower.contains("route")
            || path_lower.contains("handler")
            || path_lower.contains("/api/");
        if file.language != crate::model::SourceLanguage::Python || !looks_like_route_file {
            continue;
        }

        let file_path = Path::new(&index.root).join(&file.path);
        let Ok(source) = fs::read_to_string(&file_path) else {
            continue;
        };
        let documented_roles = documented_route_roles(&source);
        let extracted_routes = extract_python_routes(&source);
        let route_roles: Vec<(PythonRouteMatch, String)> = extracted_routes
            .into_iter()
            .map(|route| {
                let route_role = infer_route_role(&route, &documented_roles);
                (route, route_role)
            })
            .collect();
        let canonical_paths_by_handler: HashMap<String, String> = route_roles
            .iter()
            .filter(|(_, route_role)| route_role.as_str() == "canonical")
            .filter(|(route, _)| !route.handler.is_empty())
            .map(|(route, _)| (route.handler.clone(), route.full_path.clone()))
            .collect();

        for (route, route_role) in route_roles {
            let route_query_hit = route_matches_query(&query, &route);
            if !route_query_hit {
                continue;
            }

            let dedupe_key = format!(
                "python_route:{}:{}:{}:{}",
                file.path, route.line, route.method, route.full_path
            );
            if seen.insert(dedupe_key) {
                let (mounted, mount_status, activation_profiles) = route_mount_metadata(
                    &file.path,
                    &example_registry.file_to_router,
                    &example_registry.mounted_files,
                    &active_env_cartridges,
                    &profile_cartridges,
                );
                let (public, auth_policy, auth_source) = route_auth_metadata(
                    index,
                    &file.path,
                    &route.full_path,
                    route.auth_dependency.as_deref(),
                    &example_registry.file_to_router,
                    &core_auth_policy,
                );
                let canonical_path = if route_role == "compatibility_alias" {
                    canonical_paths_by_handler.get(&route.handler).cloned()
                } else {
                    None
                };
                let (route_family, route_family_label) = infer_route_family(&route, &file.path);
                route_candidates.push(ApiRouteCandidate {
                    full_path: route.full_path,
                    method: route.method,
                    handler: route.handler,
                    mounted,
                    mount_status,
                    activation_profiles,
                    public,
                    auth_policy,
                    auth_source,
                    route_role,
                    canonical_path,
                    route_family,
                    route_family_label,
                    file_path: file.path.clone(),
                    line: route.line,
                    language: file.language.as_str().to_string(),
                });
            }
        }
    }

    push_indexed_route_candidates(index, Some(&query), &mut route_candidates, &mut seen);

    if !route_candidates.is_empty() {
        let mut grouped = aggregate_route_candidates(route_candidates);
        grouped.sort_by(|left, right| compare_aggregated_routes(&query, left, right));
        let total_route_matches = grouped.len();
        let emit_family_summaries = should_emit_route_family_summaries(&query, &grouped);
        let family_summaries = if emit_family_summaries {
            summarize_route_families(&grouped)
        } else {
            Vec::new()
        };
        let compacted_routes = compact_route_entities(&query, &grouped);
        let omitted_route_count = total_route_matches.saturating_sub(compacted_routes.len());

        if omitted_route_count > 0 {
            summary = format!(
                "found {} API route matches for `{}` (showing {} routes + {} family summaries, omitted {} lower-priority routes)",
                total_route_matches,
                needle,
                compacted_routes.len(),
                family_summaries.len(),
                omitted_route_count
            );
        } else {
            summary = format!(
                "found {} API route matches for `{}`",
                total_route_matches, needle
            );
        }
        for family_summary in family_summaries {
            matches.push(family_summary);
        }
        for route in compacted_routes {
            let methods = route.methods.join("|");
            let profile_suffix = route
                .activation_profiles
                .as_ref()
                .map(|profiles| format!(" via {}", profiles.join(", ")))
                .unwrap_or_default();
            let auth_suffix = match route.auth_policy.as_str() {
                "unknown" => String::new(),
                _ => format!(", {} via {}", route.auth_policy, route.auth_source),
            };
            let route_role_suffix = match route.route_role.as_str() {
                "canonical" => ", canonical".to_string(),
                "compatibility_alias" => route
                    .canonical_path
                    .as_ref()
                    .map(|path| format!(", compatibility_alias -> {path}"))
                    .unwrap_or_else(|| ", compatibility_alias".to_string()),
                _ => String::new(),
            };

            let mut entity = json!({
                "name": route.full_path,
                "kind": "route",
                "methods": route.methods,
                "handlers": route.handlers,
                "mounted": route.mounted,
                "mount_status": route.mount_status,
                "activation_profiles": route.activation_profiles,
                "public": route.public,
                "auth_policy": route.auth_policy,
                "auth_source": route.auth_source,
                "route_role": route.route_role,
                "canonical_path": route.canonical_path,
                "route_family": route.route_family,
                "route_family_label": route.route_family_label,
                "path": route.file_path,
                "line": route.line,
                "language": route.language,
            });
            if let Some(method) = entity
                .get("methods")
                .and_then(|value| value.as_array())
                .filter(|methods| methods.len() == 1)
                .and_then(|methods| methods.first())
                .and_then(|value| value.as_str())
                .map(str::to_string)
            {
                entity["method"] = json!(method);
            }
            if let Some(handler) = entity
                .get("handlers")
                .and_then(|value| value.as_array())
                .filter(|handlers| handlers.len() == 1)
                .and_then(|handlers| handlers.first())
                .and_then(|value| value.as_str())
                .map(str::to_string)
            {
                entity["handler"] = json!(handler);
            }
            matches.push(entity);
            evidence.push(EvidenceItem {
                kind: "api_route".to_string(),
                path: route.file_path.clone(),
                line: Some(route.line),
                detail: format!(
                    "{} {} [{}{}{}{}]",
                    methods,
                    route.full_path,
                    route.mount_status,
                    profile_suffix,
                    auth_suffix,
                    route_role_suffix
                ),
            });
        }
    } else {
        for file in &index.files {
            if !is_routeish_file_path(&file.path) || is_low_signal_route_path(&file.path) {
                continue;
            }

            for sym in &file.symbols {
                let name_lower = sym.name.to_ascii_lowercase();
                if name_lower.contains(&query)
                    && (name_lower.contains("route")
                        || name_lower.contains("router")
                        || name_lower.contains("handler")
                        || name_lower.contains("endpoint")
                        || name_lower.starts_with("/v2/")
                        || name_lower.starts_with("/api/")
                        || name_lower.contains("webhook"))
                {
                    let dedupe_key = format!(
                        "symbol:{}:{}:{}:{}",
                        sym.path,
                        sym.line,
                        sym.kind.as_str(),
                        sym.name
                    );
                    if seen.insert(dedupe_key) {
                        matches.push(json!({
                            "name": sym.name,
                            "kind": sym.kind.as_str(),
                            "path": sym.path,
                            "line": sym.line,
                            "language": sym.language.as_str(),
                        }));
                        evidence.push(EvidenceItem {
                            kind: "api_route".to_string(),
                            path: sym.path.clone(),
                            line: Some(sym.line),
                            detail: format!("{} {}", sym.kind.as_str(), sym.name),
                        });
                    }
                }
            }
        }

        for file in &index.files {
            let path_lower = file.path.to_ascii_lowercase();
            if path_lower.contains(&query)
                && is_routeish_file_path(&file.path)
                && !is_low_signal_route_path(&file.path)
            {
                evidence.push(EvidenceItem {
                    kind: "route_file".to_string(),
                    path: file.path.clone(),
                    line: None,
                    detail: "route/handler file".to_string(),
                });
            }
        }

        summary = format!("found {} API route matches for `{}`", matches.len(), needle);
    }

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("find_api_route"),
        kind: "find".to_string(),
        summary,
        confidence: confidence(matches.len()),
        entities: matches,
        evidence,
        warnings: Vec::new(),
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

#[derive(Debug)]
struct PythonRouteMatch {
    router_name: String,
    router_tags: Vec<String>,
    method: String,
    full_path: String,
    handler: String,
    line: usize,
    include_in_schema: bool,
    auth_dependency: Option<String>,
}

#[derive(Debug)]
struct ApiRouteCandidate {
    full_path: String,
    method: String,
    handler: String,
    mounted: bool,
    mount_status: String,
    activation_profiles: Option<Vec<String>>,
    public: Option<bool>,
    auth_policy: String,
    auth_source: String,
    route_role: String,
    canonical_path: Option<String>,
    route_family: String,
    route_family_label: String,
    file_path: String,
    line: usize,
    language: String,
}

#[derive(Debug, Clone)]
pub(crate) struct AggregatedApiRoute {
    pub(crate) full_path: String,
    pub(crate) methods: Vec<String>,
    pub(crate) handlers: Vec<String>,
    pub(crate) mounted: bool,
    pub(crate) mount_status: String,
    pub(crate) activation_profiles: Option<Vec<String>>,
    pub(crate) public: Option<bool>,
    pub(crate) auth_policy: String,
    pub(crate) auth_source: String,
    pub(crate) route_role: String,
    pub(crate) canonical_path: Option<String>,
    pub(crate) route_family: String,
    pub(crate) route_family_label: String,
    pub(crate) file_path: String,
    pub(crate) line: usize,
    pub(crate) language: String,
}

#[derive(Debug, Clone)]
pub(crate) struct DockerServiceCandidate {
    pub(crate) name: String,
    pub(crate) file_path: String,
    pub(crate) image: Option<String>,
    pub(crate) build_context: Option<String>,
    pub(crate) profiles: Vec<String>,
}

#[derive(Debug, Default)]
struct ExampleRouterRegistry {
    router_to_file: HashMap<String, String>,
    file_to_router: HashMap<String, String>,
    mounted_files: HashSet<String>,
}

#[derive(Debug, Default)]
struct CoreAuthPolicy {
    exempt_routers: HashSet<String>,
    exempt_suffixes: Vec<String>,
}

fn extract_python_routes(source: &str) -> Vec<PythonRouteMatch> {
    let router_assign_re =
        Regex::new(r#"(?m)^(\w+)\s*=\s*APIRouter\("#).expect("valid router regex");
    let decorator_start_re =
        Regex::new(r#"^\s*@(\w+)\.(get|post|put|delete|patch|options|head)\("#)
            .expect("valid decorator start regex");
    let route_path_re = Regex::new(r#""([^"]*)""#).expect("valid route path regex");
    let handler_re = Regex::new(r#"^\s*(?:async\s+def|def)\s+([A-Za-z_][A-Za-z0-9_]*)"#)
        .expect("valid handler regex");
    let include_in_schema_false_re =
        Regex::new(r#"include_in_schema\s*=\s*False"#).expect("valid include_in_schema regex");
    let prefix_re = Regex::new(r#"prefix\s*=\s*"([^"]*)""#).expect("valid prefix regex");
    let tags_re = Regex::new(r#"(?s)tags\s*=\s*\[(?P<body>.*?)\]"#).expect("valid tags regex");
    let string_re = Regex::new(r#"'([^']+)'|"([^"]+)""#).expect("valid string regex");

    let mut router_specs: HashMap<String, (String, Vec<String>)> = HashMap::new();
    for caps in router_assign_re.captures_iter(source) {
        let Some(name) = caps.get(1).map(|m| m.as_str().to_string()) else {
            continue;
        };
        let Some(full_match) = caps.get(0) else {
            continue;
        };
        let open_paren_idx = full_match.end().saturating_sub(1);
        let Some(body) = extract_balanced_call_body(source, open_paren_idx) else {
            continue;
        };
        let prefix = prefix_re
            .captures(&body)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        let tags = tags_re
            .captures(&body)
            .and_then(|caps| caps.name("body").map(|m| m.as_str().to_string()))
            .map(|body| extract_python_string_literals(&body, &string_re))
            .unwrap_or_default();
        router_specs.insert(name, (prefix, tags));
    }

    let mut prefixes: HashMap<String, String> = HashMap::new();
    let mut router_tags: HashMap<String, Vec<String>> = HashMap::new();
    for (name, (prefix, tags)) in router_specs {
        prefixes.insert(name.clone(), prefix);
        router_tags.insert(name, tags);
    }

    let mut routes = Vec::new();
    let lines: Vec<&str> = source.lines().collect();
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let Some(start_caps) = decorator_start_re.captures(line) else {
            i += 1;
            continue;
        };

        let router_name = start_caps.get(1).map(|m| m.as_str()).unwrap_or_default();
        let method = start_caps
            .get(2)
            .map(|m| m.as_str().to_ascii_uppercase())
            .unwrap_or_default();
        let start_line = i + 1;
        let mut decorator_block = String::from(line);
        let mut paren_balance =
            line.matches('(').count() as isize - line.matches(')').count() as isize;

        while paren_balance > 0 && i + 1 < lines.len() {
            i += 1;
            decorator_block.push('\n');
            decorator_block.push_str(lines[i]);
            paren_balance += lines[i].matches('(').count() as isize;
            paren_balance -= lines[i].matches(')').count() as isize;
        }

        let route_path = route_path_re
            .captures(&decorator_block)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str())
            .unwrap_or_default()
            .to_string();

        let mut j = i + 1;
        let mut handler = String::new();
        let mut handler_signature = String::new();
        while j < lines.len() {
            if let Some(handler_caps) = handler_re.captures(lines[j]) {
                handler = handler_caps
                    .get(1)
                    .map(|m| m.as_str())
                    .unwrap_or_default()
                    .to_string();
                handler_signature = extract_python_signature(&lines, j);
                break;
            }
            if lines[j].trim_start().starts_with('@') {
                j += 1;
                continue;
            }
            j += 1;
        }

        let prefix = prefixes.get(router_name).cloned().unwrap_or_default();
        let full_path = if route_path.is_empty() {
            prefix
        } else if prefix.is_empty() {
            route_path
        } else {
            format!("{}{}", prefix, route_path)
        };

        routes.push(PythonRouteMatch {
            router_name: router_name.to_string(),
            router_tags: router_tags.get(router_name).cloned().unwrap_or_default(),
            method,
            full_path,
            handler,
            line: start_line,
            include_in_schema: !include_in_schema_false_re.is_match(&decorator_block),
            auth_dependency: explicit_fastapi_auth_dependency(&decorator_block, &handler_signature),
        });

        i += 1;
    }

    routes
}

fn extract_python_signature(lines: &[&str], start_idx: usize) -> String {
    let mut signature = String::new();
    let mut paren_balance = 0isize;

    for line in lines.iter().skip(start_idx) {
        if !signature.is_empty() {
            signature.push('\n');
        }
        signature.push_str(line);
        paren_balance += line.matches('(').count() as isize;
        paren_balance -= line.matches(')').count() as isize;

        if paren_balance <= 0 && line.contains(':') {
            break;
        }
    }

    signature
}

fn explicit_fastapi_auth_dependency(
    decorator_block: &str,
    handler_signature: &str,
) -> Option<String> {
    let protected_dependency_names = [
        "get_current_user",
        "require_admin",
        "require_pdf_studio_user",
        "require_tenant",
        "require_tenant_user",
    ];
    let body = format!("{decorator_block}\n{handler_signature}");

    protected_dependency_names
        .into_iter()
        .find(|name| {
            body.contains(&format!("Depends({name}")) || body.contains(&format!("Depends( {name}"))
        })
        .map(str::to_string)
}

fn extract_balanced_call_body(source: &str, open_paren_idx: usize) -> Option<String> {
    let bytes = source.as_bytes();
    if bytes.get(open_paren_idx).copied()? != b'(' {
        return None;
    }

    let mut depth = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut in_comment = false;
    let mut body_start: Option<usize> = None;

    for (idx, byte) in bytes.iter().enumerate().skip(open_paren_idx) {
        let ch = *byte;

        if in_comment {
            if ch == b'\n' {
                in_comment = false;
            }
            continue;
        }

        if in_single {
            if escaped {
                escaped = false;
            } else if ch == b'\\' {
                escaped = true;
            } else if ch == b'\'' {
                in_single = false;
            }
            continue;
        }

        if in_double {
            if escaped {
                escaped = false;
            } else if ch == b'\\' {
                escaped = true;
            } else if ch == b'"' {
                in_double = false;
            }
            continue;
        }

        match ch {
            b'#' => in_comment = true,
            b'\'' => in_single = true,
            b'"' => in_double = true,
            b'(' => {
                depth += 1;
                if depth == 1 {
                    body_start = Some(idx + 1);
                }
            }
            b')' => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                if depth == 0 {
                    let start = body_start?;
                    return source.get(start..idx).map(str::to_string);
                }
            }
            _ => {}
        }
    }

    None
}

fn normalize_route_family_label(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut prev_sep = false;
    for ch in value.nfd().filter(|ch| !is_combining_mark(*ch)) {
        if push_ascii_folded_family_char(&mut out, ch) {
            prev_sep = false;
        } else if !prev_sep {
            out.push('_');
            prev_sep = true;
        }
    }
    out.trim_matches('_').to_string()
}

fn push_ascii_folded_family_char(out: &mut String, ch: char) -> bool {
    if ch.is_ascii_alphanumeric() {
        out.push(ch.to_ascii_lowercase());
        return true;
    }

    let replacement = match ch {
        'ß' => "ss",
        'Æ' | 'æ' => "ae",
        'Œ' | 'œ' => "oe",
        'Ø' | 'ø' => "o",
        'Ł' | 'ł' => "l",
        'Ð' | 'ð' => "d",
        'Þ' | 'þ' => "th",
        _ => return false,
    };

    out.push_str(replacement);
    true
}

fn display_route_family_label(value: &str) -> String {
    value.replace('_', "-")
}

fn family_label_from_v2_prefix(path: &str) -> Option<(String, String)> {
    let mut segments = path.trim_start_matches('/').split('/');
    let first = segments.next()?;
    if first != "v2" {
        return None;
    }
    let second = segments.next()?;
    if second.is_empty() {
        return None;
    }
    let family = normalize_route_family_label(second);
    if family.is_empty() {
        return None;
    }
    Some((family, second.replace('_', "-")))
}

fn infer_route_family(route: &PythonRouteMatch, file_path: &str) -> (String, String) {
    if let Some(cartridge) = cartridge_name_from_path(file_path) {
        return (
            normalize_route_family_label(cartridge),
            display_route_family_label(cartridge),
        );
    }

    if let Some(tag) = route.router_tags.first() {
        return (normalize_route_family_label(tag), tag.clone());
    }

    if route.full_path.starts_with("/ops/api/") {
        return ("ops_api".to_string(), "ops-api".to_string());
    }

    if let Some(family) = family_label_from_v2_prefix(&route.full_path) {
        return family;
    }

    let label = Path::new(file_path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("route");
    (
        normalize_route_family_label(label),
        display_route_family_label(label),
    )
}

fn normalize_route_template(path: &str) -> String {
    let param_re = Regex::new(r#"\{[^}/]+\}"#).expect("valid route param regex");
    param_re.replace_all(path, "{}").to_string()
}

fn documented_route_roles(source: &str) -> HashMap<String, String> {
    let method_path_re = Regex::new(r#"^\s*(GET|POST|PUT|DELETE|PATCH|OPTIONS|HEAD)\s+(\S+)"#)
        .expect("valid documented route regex");
    let mut roles = HashMap::new();
    let mut current_role: Option<&str> = None;

    for raw_line in source.lines() {
        let line = raw_line.trim();
        let line_lower = line.to_ascii_lowercase();
        if line_lower.starts_with("canonical endpoints:") {
            current_role = Some("canonical");
            continue;
        }
        if line_lower.starts_with("legacy aliases")
            || line_lower.starts_with("compatibility aliases")
        {
            current_role = Some("compatibility_alias");
            continue;
        }
        if line.ends_with(':') && !line_lower.starts_with("http") {
            current_role = None;
        }

        let Some(role) = current_role else {
            continue;
        };
        let Some(caps) = method_path_re.captures(line) else {
            continue;
        };
        let Some(path) = caps.get(2).map(|m| m.as_str()) else {
            continue;
        };
        roles.insert(normalize_route_template(path), role.to_string());
    }

    roles
}

pub(crate) fn collect_api_routes(index: &RepoIndex) -> Vec<AggregatedApiRoute> {
    let example_registry = example_router_registry(index);
    let core_auth_policy = load_core_auth_policy(index);
    let active_env_cartridges = active_cartridges_from_env();
    let profile_cartridges = cartridge_activation_profiles(index);

    let mut route_candidates: Vec<ApiRouteCandidate> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for file in &index.files {
        let path_lower = file.path.to_ascii_lowercase();
        let looks_like_route_file = path_lower.contains("router")
            || path_lower.contains("route")
            || path_lower.contains("handler")
            || path_lower.contains("/api/");
        if file.language != crate::model::SourceLanguage::Python || !looks_like_route_file {
            continue;
        }

        let file_path = Path::new(&index.root).join(&file.path);
        let Ok(source) = fs::read_to_string(&file_path) else {
            continue;
        };
        let documented_roles = documented_route_roles(&source);
        let extracted_routes = extract_python_routes(&source);
        let route_roles: Vec<(PythonRouteMatch, String)> = extracted_routes
            .into_iter()
            .map(|route| {
                let route_role = infer_route_role(&route, &documented_roles);
                (route, route_role)
            })
            .collect();
        let canonical_paths_by_handler: HashMap<String, String> = route_roles
            .iter()
            .filter(|(_, route_role)| route_role.as_str() == "canonical")
            .filter(|(route, _)| !route.handler.is_empty())
            .map(|(route, _)| (route.handler.clone(), route.full_path.clone()))
            .collect();

        for (route, route_role) in route_roles {
            let dedupe_key = format!(
                "python_route:{}:{}:{}:{}",
                file.path, route.line, route.method, route.full_path
            );
            if seen.insert(dedupe_key) {
                let (mounted, mount_status, activation_profiles) = route_mount_metadata(
                    &file.path,
                    &example_registry.file_to_router,
                    &example_registry.mounted_files,
                    &active_env_cartridges,
                    &profile_cartridges,
                );
                let (public, auth_policy, auth_source) = route_auth_metadata(
                    index,
                    &file.path,
                    &route.full_path,
                    route.auth_dependency.as_deref(),
                    &example_registry.file_to_router,
                    &core_auth_policy,
                );
                let canonical_path = if route_role == "compatibility_alias" {
                    canonical_paths_by_handler.get(&route.handler).cloned()
                } else {
                    None
                };
                let (route_family, route_family_label) = infer_route_family(&route, &file.path);
                route_candidates.push(ApiRouteCandidate {
                    full_path: route.full_path,
                    method: route.method,
                    handler: route.handler,
                    mounted,
                    mount_status,
                    activation_profiles,
                    public,
                    auth_policy,
                    auth_source,
                    route_role,
                    canonical_path,
                    route_family,
                    route_family_label,
                    file_path: file.path.clone(),
                    line: route.line,
                    language: file.language.as_str().to_string(),
                });
            }
        }
    }

    push_indexed_route_candidates(index, None, &mut route_candidates, &mut seen);

    let mut grouped = aggregate_route_candidates(route_candidates);
    grouped.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.full_path.cmp(&right.full_path))
    });
    grouped
}

fn infer_route_role(
    route: &PythonRouteMatch,
    documented_roles: &HashMap<String, String>,
) -> String {
    let normalized_path = normalize_route_template(&route.full_path);
    if let Some(role) = documented_roles.get(&normalized_path) {
        return role.clone();
    }

    let router_name = route.router_name.to_ascii_lowercase();
    if router_name.contains("legacy") || !route.include_in_schema {
        return "compatibility_alias".to_string();
    }
    if router_name.contains("canonical") {
        return "canonical".to_string();
    }
    "primary".to_string()
}

fn aggregate_route_candidates(routes: Vec<ApiRouteCandidate>) -> Vec<AggregatedApiRoute> {
    let mut grouped: Vec<AggregatedApiRoute> = Vec::new();
    let mut index_by_key: HashMap<String, usize> = HashMap::new();

    for route in routes {
        let profiles_key = route
            .activation_profiles
            .as_ref()
            .map(|profiles| profiles.join(","))
            .unwrap_or_default();
        let key = format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            route.full_path,
            route.file_path,
            route.mounted,
            route.mount_status,
            route
                .public
                .map(|value| value.to_string())
                .unwrap_or_default(),
            route.auth_policy,
            route.auth_source,
            route.route_role,
            route.canonical_path.clone().unwrap_or_default(),
            route.route_family,
            profiles_key
        );

        if let Some(index) = index_by_key.get(&key).copied() {
            let entry = &mut grouped[index];
            if !entry.methods.iter().any(|method| method == &route.method) {
                entry.methods.push(route.method);
            }
            if !route.handler.is_empty()
                && !entry
                    .handlers
                    .iter()
                    .any(|handler| handler == &route.handler)
            {
                entry.handlers.push(route.handler);
            }
            entry.line = entry.line.min(route.line);
            continue;
        }

        let methods = vec![route.method];
        let handlers = if route.handler.is_empty() {
            Vec::new()
        } else {
            vec![route.handler]
        };
        grouped.push(AggregatedApiRoute {
            full_path: route.full_path,
            methods,
            handlers,
            mounted: route.mounted,
            mount_status: route.mount_status,
            activation_profiles: route.activation_profiles,
            public: route.public,
            auth_policy: route.auth_policy,
            auth_source: route.auth_source,
            route_role: route.route_role,
            canonical_path: route.canonical_path,
            route_family: route.route_family,
            route_family_label: route.route_family_label,
            file_path: route.file_path,
            line: route.line,
            language: route.language,
        });
        index_by_key.insert(key, grouped.len() - 1);
    }

    grouped
}

fn should_emit_route_family_summaries(query: &str, routes: &[AggregatedApiRoute]) -> bool {
    if query.len() < 5 || routes.len() < 6 {
        return false;
    }

    let family_count = routes
        .iter()
        .map(|route| route.route_family.as_str())
        .collect::<HashSet<_>>()
        .len();
    family_count >= 2
}

fn should_compact_route_entities(query: &str, routes: &[AggregatedApiRoute]) -> bool {
    if query.contains('/') || routes.len() < 40 {
        return false;
    }

    let family_count = routes
        .iter()
        .map(|route| route.route_family.as_str())
        .collect::<HashSet<_>>()
        .len();
    let largest_family_size = route_family_sizes(routes).into_values().max().unwrap_or(0);

    family_count >= 2 || largest_family_size >= 12
}

fn route_family_sizes(routes: &[AggregatedApiRoute]) -> HashMap<String, usize> {
    let mut sizes = HashMap::new();
    for route in routes {
        *sizes.entry(route.route_family.clone()).or_insert(0) += 1;
    }
    sizes
}

fn compact_route_entities<'a>(
    query: &str,
    routes: &'a [AggregatedApiRoute],
) -> Vec<&'a AggregatedApiRoute> {
    if !should_compact_route_entities(query, routes) {
        return routes.iter().collect();
    }

    const MAX_FAMILIES: usize = 4;
    const MAX_ROUTES_PER_PRIMARY_FAMILY: usize = 8;
    const MAX_ROUTES_PER_FAMILY: usize = 6;
    const MAX_TOTAL_ROUTES: usize = 24;

    let mut family_order = Vec::new();
    let mut by_family: HashMap<&str, Vec<&AggregatedApiRoute>> = HashMap::new();
    for route in routes {
        let family = route.route_family.as_str();
        if !by_family.contains_key(family) {
            family_order.push(family);
        }
        by_family.entry(family).or_default().push(route);
    }

    let mut selected = Vec::new();
    for (index, family) in family_order.into_iter().take(MAX_FAMILIES).enumerate() {
        let Some(family_routes) = by_family.get(family) else {
            continue;
        };
        let per_family_limit = if index == 0 {
            MAX_ROUTES_PER_PRIMARY_FAMILY
        } else {
            MAX_ROUTES_PER_FAMILY
        };
        let remaining_slots = MAX_TOTAL_ROUTES.saturating_sub(selected.len());
        if remaining_slots == 0 {
            break;
        }
        let keep_count = family_routes
            .len()
            .min(per_family_limit)
            .min(remaining_slots);
        selected.extend(family_routes.iter().take(keep_count).copied());
    }

    if selected.is_empty() {
        return routes.iter().take(MAX_TOTAL_ROUTES).collect();
    }

    selected
}

fn summarize_route_families(routes: &[AggregatedApiRoute]) -> Vec<serde_json::Value> {
    let mut by_family: HashMap<String, Vec<&AggregatedApiRoute>> = HashMap::new();
    let mut family_labels: HashMap<String, String> = HashMap::new();

    for route in routes {
        by_family
            .entry(route.route_family.clone())
            .or_default()
            .push(route);
        family_labels
            .entry(route.route_family.clone())
            .or_insert_with(|| route.route_family_label.clone());
    }

    let mut families: Vec<(String, Vec<&AggregatedApiRoute>)> = by_family.into_iter().collect();
    families.sort_by(|(left_family, left_routes), (right_family, right_routes)| {
        compare_route_families(left_routes, right_routes)
            .then_with(|| left_family.cmp(right_family))
    });

    families
        .into_iter()
        .map(|(family, family_routes)| {
            let label = family_labels
                .get(&family)
                .cloned()
                .unwrap_or_else(|| family.clone());
            let route_count = family_routes.len();
            let canonical_count = family_routes
                .iter()
                .filter(|route| route.route_role == "canonical")
                .count();
            let compatibility_alias_count = family_routes
                .iter()
                .filter(|route| route.route_role == "compatibility_alias")
                .count();
            let primary_count = family_routes
                .iter()
                .filter(|route| route.route_role == "primary")
                .count();
            let sample_paths: Vec<String> = family_routes
                .iter()
                .take(3)
                .map(|route| route.full_path.clone())
                .collect();
            let mounted_route_count = family_routes.iter().filter(|route| route.mounted).count();

            json!({
                "name": label,
                "kind": "route_family",
                "route_family": family,
                "route_family_label": label,
                "route_count": route_count,
                "mounted_route_count": mounted_route_count,
                "canonical_count": canonical_count,
                "compatibility_alias_count": compatibility_alias_count,
                "primary_count": primary_count,
                "sample_paths": sample_paths,
            })
        })
        .collect()
}

fn compare_route_families(
    left_routes: &[&AggregatedApiRoute],
    right_routes: &[&AggregatedApiRoute],
) -> Ordering {
    route_family_rank(left_routes)
        .cmp(&route_family_rank(right_routes))
        .then_with(|| {
            family_best_route_rank(left_routes).cmp(&family_best_route_rank(right_routes))
        })
        .then_with(|| right_routes.len().cmp(&left_routes.len()))
}

fn family_best_route_rank(routes: &[&AggregatedApiRoute]) -> (u8, u8, u8, usize, String) {
    routes
        .iter()
        .map(|route| {
            (
                route_mount_rank(route),
                route_scope_rank(route),
                route_role_rank(route),
                route.full_path.len(),
                route.full_path.clone(),
            )
        })
        .min()
        .unwrap_or((9, 9, 9, usize::MAX, String::new()))
}

fn route_family_rank(routes: &[&AggregatedApiRoute]) -> (u8, u8, u8, u8, u8) {
    let canonical_count = routes
        .iter()
        .filter(|route| route.route_role == "canonical")
        .count();
    let has_v2_non_ops = routes
        .iter()
        .any(|route| route.full_path.starts_with("/v2/") && !route.full_path.starts_with("/ops/"));
    let has_ops_path = routes
        .iter()
        .any(|route| route.full_path.starts_with("/ops/"));
    let all_ops_path = routes
        .iter()
        .all(|route| route.full_path.starts_with("/ops/"));
    let analytics_count = routes
        .iter()
        .filter(|route| route.full_path.contains("/analytics/"))
        .count();
    let has_profile_activated = routes
        .iter()
        .any(|route| route.mount_status == "profile_activated");
    let public_count = routes
        .iter()
        .filter(|route| route.public == Some(true))
        .count();

    let family_kind_rank = if canonical_count > 0 {
        0
    } else if has_v2_non_ops && !has_profile_activated {
        1
    } else if has_v2_non_ops && has_profile_activated {
        2
    } else if !all_ops_path {
        3
    } else {
        4
    };
    let ops_penalty = if has_ops_path { 1 } else { 0 };
    let analytics_penalty = if analytics_count > 0 { 1 } else { 0 };
    let public_rank = if public_count > 0 { 0 } else { 1 };
    let scope_rank = if all_ops_path {
        2
    } else if has_profile_activated {
        1
    } else {
        0
    };

    (
        family_kind_rank,
        ops_penalty,
        analytics_penalty,
        public_rank,
        scope_rank,
    )
}

fn compare_aggregated_routes(
    query: &str,
    left: &AggregatedApiRoute,
    right: &AggregatedApiRoute,
) -> Ordering {
    route_query_rank(query, left)
        .cmp(&route_query_rank(query, right))
        .then_with(|| route_mount_rank(left).cmp(&route_mount_rank(right)))
        .then_with(|| route_scope_rank(left).cmp(&route_scope_rank(right)))
        .then_with(|| route_role_rank(left).cmp(&route_role_rank(right)))
        .then_with(|| left.full_path.len().cmp(&right.full_path.len()))
        .then_with(|| left.full_path.cmp(&right.full_path))
        .then_with(|| left.file_path.cmp(&right.file_path))
        .then_with(|| left.line.cmp(&right.line))
}

fn route_matches_query(query: &str, route: &PythonRouteMatch) -> bool {
    if query.contains('/') {
        return route.full_path.to_ascii_lowercase().contains(query)
            || route.method.to_ascii_lowercase().contains(query)
            || route.handler.to_ascii_lowercase().contains(query);
    }

    // Hyphenated segments ("phone-lines") tokenize into ["phone", "lines"],
    // so a hyphenated needle can never equal a split token. Only multi-token
    // needles get the substring branch: a single-token query like "ask" must
    // stay pure token matching so it does not match "tasks".
    if route_search_tokens(query).len() > 1 && route.full_path.to_ascii_lowercase().contains(query)
    {
        return true;
    }

    let path_tokens = route_search_tokens(&route.full_path);
    let handler_tokens = route_search_tokens(&route.handler);
    let method_tokens = route_search_tokens(&route.method);

    path_tokens
        .iter()
        .chain(handler_tokens.iter())
        .chain(method_tokens.iter())
        .any(|token| token == query || token.starts_with(query))
}

fn route_query_rank(query: &str, route: &AggregatedApiRoute) -> u8 {
    let path = route.full_path.to_ascii_lowercase();
    if path == query {
        return 0;
    }
    if query.contains('/') && path.starts_with(query) {
        return 1;
    }
    let path_tokens = route_search_tokens(&route.full_path);
    let handler_tokens = route
        .handlers
        .iter()
        .flat_map(|handler| route_search_tokens(handler));
    let method_tokens = route
        .methods
        .iter()
        .flat_map(|method| route_search_tokens(method));
    let tokens: Vec<String> = path_tokens
        .into_iter()
        .chain(handler_tokens)
        .chain(method_tokens)
        .collect();
    if tokens.iter().any(|token| token == query) {
        return 1;
    }
    if tokens.iter().any(|token| token.starts_with(query)) {
        return 2;
    }
    if path.starts_with(query) {
        return 3;
    }
    if path.contains(query) {
        return 4;
    }
    5
}

fn route_search_tokens(value: &str) -> Vec<String> {
    value
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

fn route_mount_rank(route: &AggregatedApiRoute) -> u8 {
    match route.mount_status.as_str() {
        "mounted" => 0,
        "mounted_dynamic_env" => 1,
        "profile_activated" => 2,
        "unmounted" => 3,
        "unmounted_dynamic" => 4,
        "unknown" => 5,
        _ => 6,
    }
}

fn route_scope_rank(route: &AggregatedApiRoute) -> u8 {
    if route.file_path.starts_with("example-api/") {
        return 0;
    }
    if route.file_path.starts_with("cartridges/") {
        return 1;
    }
    2
}

fn route_role_rank(route: &AggregatedApiRoute) -> u8 {
    match route.route_role.as_str() {
        "primary" | "canonical" => 0,
        "compatibility_alias" => 1,
        _ => 2,
    }
}

fn example_router_registry(index: &RepoIndex) -> ExampleRouterRegistry {
    let registry_rel = "example-api/example/routers/__init__.py";
    let registry_path = Path::new(&index.root).join(registry_rel);
    let Ok(source) = fs::read_to_string(registry_path) else {
        return ExampleRouterRegistry::default();
    };

    let spec_re = Regex::new(r#""([A-Za-z0-9_]+)":\s*\("([^.][^"]*|(?:\.[^"]+))",\s*"[^"]+"\)"#)
        .expect("valid router spec regex");
    let mounted_re = Regex::new(r#"\("([A-Za-z0-9_]+)",\s*(?:None|"[^"]*")\)"#)
        .expect("valid mounted router regex");

    let mut registry = ExampleRouterRegistry::default();
    for caps in spec_re.captures_iter(&source) {
        let router_name = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
        let module_name = caps.get(2).map(|m| m.as_str()).unwrap_or_default();
        if let Some(path) = router_module_to_file(module_name) {
            registry
                .file_to_router
                .insert(path.clone(), router_name.to_string());
            registry
                .router_to_file
                .insert(router_name.to_string(), path);
        }
    }

    let full_entries_start = source.find("_FULL_ROUTER_ENTRIES").unwrap_or(0);
    let mounted_source = &source[full_entries_start..];
    for caps in mounted_re.captures_iter(mounted_source) {
        let router_name = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
        if let Some(path) = registry.router_to_file.get(router_name) {
            registry.mounted_files.insert(path.clone());
        }
    }

    expand_included_router_files(Path::new(&index.root), &mut registry);

    registry
}

fn active_cartridges_from_env() -> HashSet<String> {
    std::env::var("EXAMPLE_ACTIVE_CARTRIDGES")
        .ok()
        .map(|value| parse_cartridge_list(&value).into_iter().collect())
        .unwrap_or_default()
}

fn cartridge_activation_profiles(index: &RepoIndex) -> HashMap<String, Vec<String>> {
    let mut profiles_by_cartridge: HashMap<String, Vec<String>> = HashMap::new();
    for profile in &index.profiles {
        let Some(var) = profile
            .vars
            .iter()
            .find(|var| var.name == "EXAMPLE_ACTIVE_CARTRIDGES")
        else {
            continue;
        };
        let Some(value) = &var.value_preview else {
            continue;
        };
        for cartridge in parse_cartridge_list(value) {
            profiles_by_cartridge
                .entry(cartridge)
                .or_default()
                .push(profile.name.clone());
        }
    }
    profiles_by_cartridge
}

fn parse_cartridge_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

fn cartridge_name_from_path(path: &str) -> Option<&str> {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() >= 3 && parts[0] == "cartridges" && parts[2] == "router.py" {
        return parts.get(1).copied();
    }
    None
}

fn is_routeish_file_path(path: &str) -> bool {
    let path_lower = path.to_ascii_lowercase();
    path_lower.contains("router")
        || path_lower.contains("route")
        || path_lower.contains("handler")
        || path_lower.contains("/api/")
}

fn is_low_signal_route_path(path: &str) -> bool {
    let path_lower = path.to_ascii_lowercase();
    path_lower.contains("/tests/")
        || path_lower.contains("/__tests__/")
        || path_lower.contains("/vendor/")
        || path_lower.contains("/generated/")
        || path_lower.contains("/api_backup/")
        || path_lower.ends_with(".test.ts")
        || path_lower.ends_with(".test.tsx")
        || path_lower.ends_with(".test.py")
}

fn route_mount_metadata(
    file_path: &str,
    file_to_router: &HashMap<String, String>,
    mounted_example_files: &HashSet<String>,
    active_env_cartridges: &HashSet<String>,
    profile_cartridges: &HashMap<String, Vec<String>>,
) -> (bool, String, Option<Vec<String>>) {
    if file_to_router.contains_key(file_path) {
        let mounted = mounted_example_files.contains(file_path);
        let status = if mounted { "mounted" } else { "unmounted" };
        return (mounted, status.to_string(), None);
    }

    if file_path.starts_with("example-api/example/routers/") {
        return (false, "unmounted".to_string(), None);
    }

    if let Some(cartridge) = cartridge_name_from_path(file_path) {
        if active_env_cartridges.contains(cartridge) {
            return (true, "mounted_dynamic_env".to_string(), None);
        }
        if let Some(profiles) = profile_cartridges.get(cartridge)
            && !profiles.is_empty()
        {
            return (
                false,
                "profile_activated".to_string(),
                Some(profiles.clone()),
            );
        }
        return (false, "unmounted_dynamic".to_string(), None);
    }

    (false, "unknown".to_string(), None)
}

fn expand_included_router_files(root: &Path, registry: &mut ExampleRouterRegistry) {
    let import_re = Regex::new(
        r#"(?m)^\s*from\s+([A-Za-z0-9_\.]+|\.+[A-Za-z0-9_\.]*)\s+import\s+router(?:\s+as\s+([A-Za-z_][A-Za-z0-9_]*))?"#,
    )
    .expect("valid include import regex");
    let include_re =
        Regex::new(r#"(?m)^\s*\w+\.include_router\((\w+)\)"#).expect("valid include router regex");

    let mut queue: Vec<String> = registry.mounted_files.iter().cloned().collect();
    let mut visited: HashSet<String> = HashSet::new();

    while let Some(file_path) = queue.pop() {
        if !visited.insert(file_path.clone()) {
            continue;
        }

        let source_path = root.join(&file_path);
        let Ok(source) = fs::read_to_string(source_path) else {
            continue;
        };

        let mut alias_to_path: HashMap<String, String> = HashMap::new();
        for caps in import_re.captures_iter(&source) {
            let module_name = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
            let alias = caps
                .get(2)
                .map(|m| m.as_str())
                .unwrap_or("router")
                .to_string();
            if let Some(imported_path) = router_module_to_file(module_name) {
                alias_to_path.insert(alias, imported_path);
            }
        }

        for caps in include_re.captures_iter(&source) {
            let alias = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
            let Some(included_path) = alias_to_path.get(alias).cloned() else {
                continue;
            };
            registry
                .file_to_router
                .entry(included_path.clone())
                .or_insert_with(|| format!("included::{file_path}"));
            if registry.mounted_files.insert(included_path.clone()) {
                queue.push(included_path);
            }
        }
    }
}

fn load_core_auth_policy(index: &RepoIndex) -> CoreAuthPolicy {
    let main_path = Path::new(&index.root).join("example-api/example/main.py");
    let Ok(source) = fs::read_to_string(main_path) else {
        return CoreAuthPolicy::default();
    };

    let string_re = Regex::new(r#"'([^']+)'|"([^"]+)""#).expect("valid string regex");
    let router_block_re = Regex::new(r#"(?s)_AUTH_EXEMPT_ROUTERS\s*=\s*\{(?P<body>.*?)\}"#)
        .expect("valid auth exempt router regex");
    let suffix_block_re = Regex::new(r#"(?s)_AUTH_EXEMPT_PATH_SUFFIXES\s*=\s*\((?P<body>.*?)\)"#)
        .expect("valid auth exempt suffix regex");

    let exempt_routers = router_block_re
        .captures(&source)
        .and_then(|caps| caps.name("body").map(|m| m.as_str().to_string()))
        .map(|body| {
            extract_python_string_literals(&body, &string_re)
                .into_iter()
                .collect()
        })
        .unwrap_or_default();

    let exempt_suffixes = suffix_block_re
        .captures(&source)
        .and_then(|caps| caps.name("body").map(|m| m.as_str().to_string()))
        .map(|body| extract_python_string_literals(&body, &string_re))
        .unwrap_or_default();

    CoreAuthPolicy {
        exempt_routers,
        exempt_suffixes,
    }
}

fn extract_python_string_literals(source: &str, string_re: &Regex) -> Vec<String> {
    string_re
        .captures_iter(source)
        .filter_map(|caps| caps.get(1).or_else(|| caps.get(2)))
        .map(|m| m.as_str().to_string())
        .collect()
}

fn route_auth_metadata(
    index: &RepoIndex,
    file_path: &str,
    route_path: &str,
    explicit_auth_dependency: Option<&str>,
    file_to_router: &HashMap<String, String>,
    core_auth_policy: &CoreAuthPolicy,
) -> (Option<bool>, String, String) {
    if file_to_router.contains_key(file_path) {
        if let Some(suffix) = core_auth_policy.exempt_suffixes.iter().find(|suffix| {
            route_path
                .trim_end_matches('/')
                .ends_with(suffix.trim_end_matches('/'))
        }) {
            return (
                Some(true),
                "public".to_string(),
                format!("core_exempt_suffix:{suffix}"),
            );
        }

        if let Some(dep) = explicit_auth_dependency {
            return (
                Some(false),
                "protected".to_string(),
                format!("route_dependency:{dep}"),
            );
        }

        if let Some(router_name) = file_to_router.get(file_path)
            && core_auth_policy.exempt_routers.contains(router_name)
        {
            return (
                Some(true),
                "public".to_string(),
                format!("core_exempt_router:{router_name}"),
            );
        }

        return (
            Some(false),
            "protected".to_string(),
            "core_auth_injection:get_current_user".to_string(),
        );
    }

    if file_path.starts_with("example-api/example/routers/") {
        if let Some(suffix) = core_auth_policy.exempt_suffixes.iter().find(|suffix| {
            route_path
                .trim_end_matches('/')
                .ends_with(suffix.trim_end_matches('/'))
        }) {
            return (
                Some(true),
                "public".to_string(),
                format!("core_exempt_suffix_if_mounted:{suffix}"),
            );
        }

        if let Some(dep) = explicit_auth_dependency {
            return (
                Some(false),
                "protected".to_string(),
                format!("route_dependency_if_mounted:{dep}"),
            );
        }

        return (
            Some(false),
            "protected".to_string(),
            "core_auth_injection_if_mounted:get_current_user".to_string(),
        );
    }

    if let Some(cartridge) = cartridge_name_from_path(file_path) {
        let public_paths = cartridge_public_paths(index, cartridge);
        if let Some(public_path) = public_paths.iter().find(|public_path| {
            route_path
                .trim_end_matches('/')
                .ends_with(public_path.trim_end_matches('/'))
        }) {
            return (
                Some(true),
                "public".to_string(),
                format!("cartridge_public_path:{public_path}"),
            );
        }

        return (
            Some(false),
            "protected".to_string(),
            "cartridge_auth_injection:get_current_user".to_string(),
        );
    }

    (None, "unknown".to_string(), "unknown".to_string())
}

fn cartridge_public_paths(index: &RepoIndex, cartridge: &str) -> Vec<String> {
    let mut public_paths = vec![
        "/health".to_string(),
        "/healthz".to_string(),
        "/flows/data".to_string(),
    ];

    for path in loader_declared_cartridge_public_paths(index, cartridge) {
        append_public_path(&mut public_paths, path);
    }

    let manifest_path = Path::new(&index.root)
        .join("cartridges")
        .join(cartridge)
        .join("cartridge.toml");
    let Ok(raw) = fs::read_to_string(manifest_path) else {
        return public_paths;
    };
    let Ok(value) = toml::from_str::<toml::Value>(&raw) else {
        return public_paths;
    };
    let extra_paths = value
        .get("auth")
        .and_then(toml::Value::as_table)
        .and_then(|auth| auth.get("public_paths"))
        .and_then(toml::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for path in extra_paths {
        append_public_path(&mut public_paths, path);
    }
    public_paths
}

fn append_public_path(public_paths: &mut Vec<String>, path: String) {
    if !public_paths.contains(&path) {
        public_paths.push(path);
    }
}

fn loader_declared_cartridge_public_paths(index: &RepoIndex, cartridge: &str) -> Vec<String> {
    let loader_path = Path::new(&index.root)
        .join("example-api")
        .join("example")
        .join("cartridges")
        .join("__init__.py");
    let Ok(source) = fs::read_to_string(loader_path) else {
        return Vec::new();
    };
    let string_re =
        Regex::new(r#""([^"]+)"|'([^']+)'"#).expect("valid python string literal regex");
    let escaped = regex::escape(cartridge);
    let frozenset_re = Regex::new(&format!(
        r#"(?s)["']{escaped}["']\s*:\s*frozenset\s*\(\s*\{{(?P<body>.*?)\}}\s*\)"#
    ))
    .expect("valid cartridge frozenset public path regex");
    if let Some(paths) = frozenset_re
        .captures(&source)
        .and_then(|caps| caps.name("body").map(|body| body.as_str().to_string()))
        .map(|body| extract_python_string_literals(&body, &string_re))
    {
        return paths;
    }

    let set_re = Regex::new(&format!(
        r#"(?s)["']{escaped}["']\s*:\s*\{{(?P<body>.*?)\}}"#
    ))
    .expect("valid cartridge set public path regex");
    set_re
        .captures(&source)
        .and_then(|caps| caps.name("body").map(|body| body.as_str().to_string()))
        .map(|body| extract_python_string_literals(&body, &string_re))
        .unwrap_or_default()
}

fn router_module_to_file(module_name: &str) -> Option<String> {
    if module_name.starts_with('.') {
        let level = module_name.chars().take_while(|c| *c == '.').count();
        let rel = module_name[level..].trim_matches('.');
        let mut package_parts = vec!["example".to_string(), "routers".to_string()];
        let parent_hops = level.saturating_sub(1).min(package_parts.len());
        package_parts.truncate(package_parts.len().saturating_sub(parent_hops));
        if !rel.is_empty() {
            package_parts.extend(
                rel.split('.')
                    .filter(|part| !part.is_empty())
                    .map(str::to_string),
            );
        }
        if package_parts.is_empty() {
            return None;
        }
        return Some(format!("example-api/{}.py", package_parts.join("/")));
    }
    if let Some(rel) = module_name.strip_prefix("example.") {
        let rel = rel.replace('.', "/");
        return Some(format!("example-api/example/{}.py", rel));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        explain_cartridge, explain_deploy_target, find_api_routes, find_cartridges,
        find_deploy_targets,
    };
    use crate::model::{FileRecord, RepoIndex, SourceLanguage};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEMP_REPO_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn empty_index(root: &Path) -> RepoIndex {
        RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: Vec::new(),
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        }
    }

    #[test]
    fn find_deploy_targets_reports_unmodeled_facet() {
        let root = temp_repo_root();
        let result = find_deploy_targets(&empty_index(&root), "backend");
        assert_eq!(result.summary, "repository does not model deploy targets");
        assert_eq!(
            result
                .meta
                .as_ref()
                .and_then(|meta| meta.get("facet_available"))
                .and_then(|value| value.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn explain_deploy_target_reports_unmodeled_facet() {
        let root = temp_repo_root();
        let result = explain_deploy_target(
            &empty_index(&root),
            "backend",
            &root,
            crate::value_resolution::ValueResolutionOpts::default(),
        );
        assert_eq!(result.summary, "repository does not model deploy targets");
        assert_eq!(
            result
                .meta
                .as_ref()
                .and_then(|meta| meta.get("facet"))
                .and_then(|value| value.as_str()),
            Some("deploy_targets")
        );
    }

    #[test]
    fn find_cartridges_reports_unmodeled_facet() {
        let root = temp_repo_root();
        let result = find_cartridges(&empty_index(&root), "customer");
        assert_eq!(result.summary, "repository does not model cartridges");
        assert_eq!(
            result
                .meta
                .as_ref()
                .and_then(|meta| meta.get("facet_available"))
                .and_then(|value| value.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn explain_cartridge_reports_unmodeled_facet() {
        let root = temp_repo_root();
        let result = explain_cartridge(&empty_index(&root), "customer");
        assert_eq!(result.summary, "repository does not model cartridges");
        assert_eq!(
            result
                .meta
                .as_ref()
                .and_then(|meta| meta.get("facet"))
                .and_then(|value| value.as_str()),
            Some("cartridges")
        );
    }

    #[test]
    fn find_cartridges_discovers_undeployed_source_cartridge() {
        let root = temp_repo_root();
        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![
                FileRecord {
                    path: "cartridges/revops/router.py".to_string(),
                    language: SourceLanguage::Python,
                    bytes: 0,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "cartridges/revops/types.py".to_string(),
                    language: SourceLanguage::Python,
                    bytes: 0,
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

        let result = find_cartridges(&index, "revops");
        assert_eq!(
            result.entities[0]
                .get("cartridge")
                .and_then(|value| value.as_str()),
            Some("revops")
        );
        assert_eq!(
            result.entities[0]
                .get("deploy_target_count")
                .and_then(|value| value.as_u64()),
            Some(0)
        );
        assert_eq!(
            result.entities[0]
                .get("deployment_state")
                .and_then(|value| value.as_str()),
            Some("undeployed")
        );
    }

    #[test]
    fn explain_cartridge_reports_source_only_cartridge() {
        let root = temp_repo_root();
        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![FileRecord {
                path: "cartridges/revops/router.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 0,
                modified_unix_ms: 0,
                symbols: Vec::new(),
                env_vars: Vec::new(),
                redis_keys: Vec::new(),
                subprocess_calls: Vec::new(),
                http_calls: Vec::new(),
                unresolved_edges: Vec::new(),
            }],
            deploy_targets: Vec::new(),
            profiles: vec![crate::model::ProfileRecord {
                name: "revops.env".to_string(),
                path: "deploy/profiles/revops.env".to_string(),
                vars: vec![crate::model::DeclaredVar {
                    name: "EXAMPLE_ACTIVE_CARTRIDGES".to_string(),
                    value_preview: Some("revops".to_string()),
                    raw_value: None,
                }],
            }],
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let result = explain_cartridge(&index, "revops");
        assert!(result.summary.contains("0 deploy targets"));
        assert_eq!(
            result.entities[0]
                .get("activation_profiles")
                .and_then(|value| value.as_array())
                .map(|items| items.len()),
            Some(1)
        );
        assert_eq!(
            result.warnings,
            vec!["cartridge not referenced by any deploy target".to_string()]
        );
    }

    fn temp_repo_root() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let counter = TEMP_REPO_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "leio-code-query-test-{}-{nonce}-{counter}",
            std::process::id()
        ))
    }

    #[test]
    fn find_api_routes_extracts_python_decorator_paths() {
        let root = temp_repo_root();
        let router_path = root.join("example-api/example/routers");
        fs::create_dir_all(&router_path).expect("create router path");
        let file_path = router_path.join("agent_run.py");
        fs::write(
            &file_path,
            r#"
from fastapi import APIRouter

router = APIRouter(prefix="/v2/agent", tags=["agent-run"])

@router.post("/run/swarm")
async def agent_run_swarm():
    return {"ok": True}
"#,
        )
        .expect("write test router");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![FileRecord {
                path: "example-api/example/routers/agent_run.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 0,
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

        let result = find_api_routes(&index, "swarm");
        let route_names: Vec<String> = result
            .entities
            .iter()
            .filter_map(|entity| entity.get("name").and_then(|value| value.as_str()))
            .map(str::to_string)
            .collect();

        assert!(route_names.iter().any(|name| name == "/v2/agent/run/swarm"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_extracts_multiline_python_decorators() {
        let root = temp_repo_root();
        let router_path = root.join("example-api/example/routers");
        fs::create_dir_all(&router_path).expect("create router path");
        let file_path = router_path.join("workflow_sessions.py");
        fs::write(
            &file_path,
            r#"
from fastapi import APIRouter, status

router = APIRouter(prefix="/v2/workflows", tags=["workflow-sessions"])

@router.post(
    "/{workflow_id}/sessions",
    status_code=status.HTTP_201_CREATED,
)
async def create_workflow_session():
    return {"ok": True}
"#,
        )
        .expect("write multiline test router");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![FileRecord {
                path: "example-api/example/routers/workflow_sessions.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 0,
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

        let result = find_api_routes(&index, "sessions");
        let route_names: Vec<String> = result
            .entities
            .iter()
            .filter_map(|entity| entity.get("name").and_then(|value| value.as_str()))
            .map(str::to_string)
            .collect();

        assert!(
            route_names
                .iter()
                .any(|name| name == "/v2/workflows/{workflow_id}/sessions")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_matches_hyphenated_needles_on_prefixed_routers() {
        let root = temp_repo_root();
        let router_path = root.join("example-api/example/routers");
        fs::create_dir_all(&router_path).expect("create router path");
        fs::write(
            router_path.join("phone_lines.py"),
            r#"
from fastapi import APIRouter

router = APIRouter(prefix="/v2/phone-lines", tags=["phone-lines"])

@router.post("", dependencies=[])
async def create_phone_line():
    ...

@router.get("/{phone_id}")
async def get_phone_line(phone_id: str):
    ...
"#,
        )
        .expect("write test router");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![FileRecord {
                path: "example-api/example/routers/phone_lines.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 0,
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

        // Hyphenated needle with no slash: tokenization splits both sides,
        // the substring branch must carry the match (the review's exact miss).
        let result = find_api_routes(&index, "phone-lines");
        let names: Vec<String> = result
            .entities
            .iter()
            .filter_map(|entity| entity.get("name").and_then(|value| value.as_str()))
            .map(str::to_string)
            .collect();
        assert!(
            names.iter().any(|name| name == "/v2/phone-lines"),
            "names={names:?}"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_matches_indexed_axum_routes() {
        let root = temp_repo_root();
        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![FileRecord {
                path: "example-gateway/src/main.rs".to_string(),
                language: SourceLanguage::Rust,
                bytes: 0,
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
            cross_language: crate::model::CrossLanguageGraph {
                routes: vec![crate::model::RouteRecord {
                    handler: Some("chatwoot_webhook".to_string()),
                    auth_hint: Some("protected (route_layer)".to_string()),
                    path: "example-gateway/src/main.rs".to_string(),
                    line: 3286,
                    method: "POST".to_string(),
                    route: "/webhooks/chatwoot".to_string(),
                    framework: "axum".to_string(),
                    language: SourceLanguage::Rust,
                    path_params: Vec::new(),
                }],
                ..Default::default()
            },
            k8s_configmaps: Vec::new(),
        };

        // Path-bearing query (the review's exact miss).
        let result = find_api_routes(&index, "webhooks/chatwoot");
        let names: Vec<String> = result
            .entities
            .iter()
            .filter_map(|entity| entity.get("name").and_then(|value| value.as_str()))
            .map(str::to_string)
            .collect();
        assert!(
            names.iter().any(|name| name == "/webhooks/chatwoot"),
            "names={names:?}"
        );

        // Token query should also reach it.
        let result = find_api_routes(&index, "chatwoot");
        assert!(
            result
                .entities
                .iter()
                .filter_map(|entity| entity.get("name").and_then(|value| value.as_str()))
                .any(|name| name == "/webhooks/chatwoot"),
            "token query must match indexed routes"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_marks_mounted_example_api_routes() {
        let root = temp_repo_root();
        let router_path = root.join("example-api/example/routers");
        fs::create_dir_all(&router_path).expect("create router path");
        fs::write(
            root.join("example-api/example/main.py"),
            r#"
_AUTH_EXEMPT_ROUTERS = {"auth_router"}
_AUTH_EXEMPT_PATH_SUFFIXES = ("/health", "/healthz")
"#,
        )
        .expect("write main auth policy");
        fs::write(
            router_path.join("__init__.py"),
            r#"
_ROUTER_SPECS = {
    "workflow_sessions_router": (".workflow_sessions", "router"),
}

_FULL_ROUTER_ENTRIES = (
    ("workflow_sessions_router", None),
)
"#,
        )
        .expect("write registry");
        fs::write(
            router_path.join("workflow_sessions.py"),
            r#"
from fastapi import APIRouter

router = APIRouter(prefix="/v2/workflows", tags=["workflow-sessions"])

@router.post(
    "/{workflow_id}/sessions",
)
async def create_workflow_session():
    return {"ok": True}
"#,
        )
        .expect("write workflow sessions router");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![FileRecord {
                path: "example-api/example/routers/workflow_sessions.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 0,
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

        let result = find_api_routes(&index, "sessions");
        let mounted_flags: Vec<bool> = result
            .entities
            .iter()
            .filter(|entity| {
                entity.get("name").and_then(|value| value.as_str())
                    == Some("/v2/workflows/{workflow_id}/sessions")
            })
            .filter_map(|entity| entity.get("mounted").and_then(|value| value.as_bool()))
            .collect();
        let auth_policies: Vec<String> = result
            .entities
            .iter()
            .filter(|entity| {
                entity.get("name").and_then(|value| value.as_str())
                    == Some("/v2/workflows/{workflow_id}/sessions")
            })
            .filter_map(|entity| {
                entity
                    .get("auth_policy")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .collect();

        assert_eq!(mounted_flags, vec![true]);
        assert_eq!(auth_policies, vec!["protected".to_string()]);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_marks_cartridge_routes_as_profile_activated() {
        let root = temp_repo_root();
        let cartridge_path = root.join("cartridges/vigoros");
        fs::create_dir_all(&cartridge_path).expect("create cartridge path");
        fs::write(
            cartridge_path.join("router.py"),
            r#"
from fastapi import APIRouter

router = APIRouter(prefix="/v2/vigoros", tags=["vigoros"])

@router.post("/ask")
async def ask():
    return {"ok": True}
"#,
        )
        .expect("write cartridge router");
        fs::write(
            cartridge_path.join("cartridge.toml"),
            r#"
[auth]
public_paths = ["/ask"]
"#,
        )
        .expect("write cartridge manifest");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![FileRecord {
                path: "cartridges/vigoros/router.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 0,
                modified_unix_ms: 0,
                symbols: Vec::new(),
                env_vars: Vec::new(),
                redis_keys: Vec::new(),
                subprocess_calls: Vec::new(),
                http_calls: Vec::new(),
                unresolved_edges: Vec::new(),
            }],
            deploy_targets: Vec::new(),
            profiles: vec![crate::model::ProfileRecord {
                name: "vigoros.env".to_string(),
                path: "deploy/profiles/vigoros.env".to_string(),
                vars: vec![crate::model::DeclaredVar {
                    name: "EXAMPLE_ACTIVE_CARTRIDGES".to_string(),
                    value_preview: Some("vigoros,health_audit".to_string()),
                    raw_value: None,
                }],
            }],
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let result = find_api_routes(&index, "vigoros");
        let statuses: Vec<String> = result
            .entities
            .iter()
            .filter(|entity| {
                entity.get("name").and_then(|value| value.as_str()) == Some("/v2/vigoros/ask")
            })
            .filter_map(|entity| {
                entity
                    .get("mount_status")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .collect();
        let auth_policies: Vec<String> = result
            .entities
            .iter()
            .filter(|entity| {
                entity.get("name").and_then(|value| value.as_str()) == Some("/v2/vigoros/ask")
            })
            .filter_map(|entity| {
                entity
                    .get("auth_policy")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .collect();
        let public_flags: Vec<bool> = result
            .entities
            .iter()
            .filter(|entity| {
                entity.get("name").and_then(|value| value.as_str()) == Some("/v2/vigoros/ask")
            })
            .filter_map(|entity| entity.get("public").and_then(|value| value.as_bool()))
            .collect();

        assert_eq!(statuses, vec!["profile_activated".to_string()]);
        assert_eq!(auth_policies, vec!["public".to_string()]);
        assert_eq!(public_flags, vec![true]);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_marks_loader_declared_cartridge_public_paths() {
        let root = temp_repo_root();
        let cartridge_path = root.join("cartridges/health_audit");
        let loader_path = root.join("example-api/example/cartridges");
        fs::create_dir_all(&cartridge_path).expect("create cartridge path");
        fs::create_dir_all(&loader_path).expect("create cartridge loader path");
        fs::write(
            cartridge_path.join("router.py"),
            r#"
from fastapi import APIRouter

router = APIRouter(prefix="/v2/health-audit", tags=["health-audit"])

@router.get("/status")
async def status():
    return {"ok": True}

@router.get("/contracts")
async def contracts():
    return {"ok": True}
"#,
        )
        .expect("write health audit router");
        fs::write(
            loader_path.join("__init__.py"),
            r#"
_PUBLIC_CARTRIDGE_PATHS = {
    "health_audit": frozenset({"/v2/health-audit/status", "/status"}),
}
"#,
        )
        .expect("write cartridge loader auth overrides");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![FileRecord {
                path: "cartridges/health_audit/router.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 0,
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

        let result = find_api_routes(&index, "/v2/health-audit");
        let status_auth_policies: Vec<String> = result
            .entities
            .iter()
            .filter(|entity| {
                entity.get("name").and_then(|value| value.as_str())
                    == Some("/v2/health-audit/status")
            })
            .filter_map(|entity| {
                entity
                    .get("auth_policy")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .collect();
        let contracts_auth_policies: Vec<String> = result
            .entities
            .iter()
            .filter(|entity| {
                entity.get("name").and_then(|value| value.as_str())
                    == Some("/v2/health-audit/contracts")
            })
            .filter_map(|entity| {
                entity
                    .get("auth_policy")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .collect();

        assert_eq!(status_auth_policies, vec!["public".to_string()]);
        assert_eq!(contracts_auth_policies, vec!["protected".to_string()]);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_marks_core_public_routes_from_exempt_router() {
        let root = temp_repo_root();
        let router_path = root.join("example-api/example/routers");
        fs::create_dir_all(&router_path).expect("create router path");
        fs::write(
            root.join("example-api/example/main.py"),
            r#"
_AUTH_EXEMPT_ROUTERS = {"auth_router", "system_router"}
_AUTH_EXEMPT_PATH_SUFFIXES = ("/health", "/healthz", "/webhook")
"#,
        )
        .expect("write main auth policy");
        fs::write(
            router_path.join("__init__.py"),
            r#"
_ROUTER_SPECS = {
    "auth_router": (".auth", "router"),
}

_FULL_ROUTER_ENTRIES = (
    ("auth_router", None),
)
"#,
        )
        .expect("write registry");
        fs::write(
            router_path.join("auth.py"),
            r#"
from fastapi import APIRouter

router = APIRouter(prefix="/v2/auth", tags=["auth"])

@router.post("/login")
async def login():
    return {"ok": True}
"#,
        )
        .expect("write auth router");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![FileRecord {
                path: "example-api/example/routers/auth.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 0,
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

        let result = find_api_routes(&index, "login");
        let auth_policies: Vec<String> = result
            .entities
            .iter()
            .filter(|entity| {
                entity.get("name").and_then(|value| value.as_str()) == Some("/v2/auth/login")
            })
            .filter_map(|entity| {
                entity
                    .get("auth_policy")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .collect();
        let auth_sources: Vec<String> = result
            .entities
            .iter()
            .filter(|entity| {
                entity.get("name").and_then(|value| value.as_str()) == Some("/v2/auth/login")
            })
            .filter_map(|entity| {
                entity
                    .get("auth_source")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .collect();

        assert_eq!(auth_policies, vec!["public".to_string()]);
        assert_eq!(
            auth_sources,
            vec!["core_exempt_router:auth_router".to_string()]
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_respects_explicit_auth_dependency_inside_exempt_router() {
        let root = temp_repo_root();
        let router_path = root.join("example-api/example/routers");
        fs::create_dir_all(&router_path).expect("create router path");
        fs::write(
            root.join("example-api/example/main.py"),
            r#"
_AUTH_EXEMPT_ROUTERS = {"pdf_studio_router"}
_AUTH_EXEMPT_PATH_SUFFIXES = ("/health", "/healthz", "/webhook")
"#,
        )
        .expect("write main auth policy");
        fs::write(
            router_path.join("__init__.py"),
            r#"
_ROUTER_SPECS = {
    "pdf_studio_router": (".pdf_studio", "router"),
}

_FULL_ROUTER_ENTRIES = (
    ("pdf_studio_router", None),
)
"#,
        )
        .expect("write registry");
        fs::write(
            router_path.join("pdf_studio.py"),
            r#"
from fastapi import APIRouter, Depends

router = APIRouter(prefix="/v2/pdf-studio", tags=["PDF Studio"])

def require_pdf_studio_user():
    return {"tenant_id": "tenant_1"}

@router.get("/masters")
async def list_masters(_user = Depends(require_pdf_studio_user)):
    return []

@router.get("/render/health")
async def render_health():
    return {"ok": True}
"#,
        )
        .expect("write pdf studio router");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![FileRecord {
                path: "example-api/example/routers/pdf_studio.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 0,
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

        let result = find_api_routes(&index, "/v2/pdf-studio");
        let route_meta: Vec<(String, String, String)> = result
            .entities
            .iter()
            .filter_map(|entity| {
                Some((
                    entity.get("name")?.as_str()?.to_string(),
                    entity.get("auth_policy")?.as_str()?.to_string(),
                    entity.get("auth_source")?.as_str()?.to_string(),
                ))
            })
            .collect();

        assert!(
            route_meta.contains(&(
                "/v2/pdf-studio/masters".to_string(),
                "protected".to_string(),
                "route_dependency:require_pdf_studio_user".to_string()
            )),
            "route_meta={route_meta:?}"
        );
        assert!(route_meta.contains(&(
            "/v2/pdf-studio/render/health".to_string(),
            "public".to_string(),
            "core_exempt_suffix:/health".to_string()
        )));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_resolves_nested_relative_router_modules() {
        let root = temp_repo_root();
        let registry_path = root.join("example-api/example/routers");
        let whatsapp_path = root.join("example-api/example/integrations/whatsapp/routers");
        fs::create_dir_all(&registry_path).expect("create registry path");
        fs::create_dir_all(&whatsapp_path).expect("create whatsapp router path");
        fs::write(
            root.join("example-api/example/main.py"),
            r#"
_AUTH_EXEMPT_ROUTERS = {"auth_router", "system_router", "events_router"}
_AUTH_EXEMPT_PATH_SUFFIXES = ("/health", "/healthz", "/webhook", "/flows/data")
"#,
        )
        .expect("write main auth policy");
        fs::write(
            registry_path.join("__init__.py"),
            r#"
_ROUTER_SPECS = {
    "whatsapp_router": ("..integrations.whatsapp.routers.webhook", "router"),
}

_FULL_ROUTER_ENTRIES = (
    ("whatsapp_router", None),
)
"#,
        )
        .expect("write registry");
        fs::write(
            whatsapp_path.join("webhook.py"),
            r#"
from fastapi import APIRouter

router = APIRouter(prefix="/v2/whatsapp", tags=["whatsapp"])

@router.get("/webhook")
async def verify_webhook():
    return {"ok": True}
"#,
        )
        .expect("write whatsapp webhook router");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![FileRecord {
                path: "example-api/example/integrations/whatsapp/routers/webhook.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 0,
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

        let result = find_api_routes(&index, "webhook");
        let mounted_flags: Vec<bool> = result
            .entities
            .iter()
            .filter(|entity| {
                entity.get("name").and_then(|value| value.as_str()) == Some("/v2/whatsapp/webhook")
            })
            .filter_map(|entity| entity.get("mounted").and_then(|value| value.as_bool()))
            .collect();
        let auth_policies: Vec<String> = result
            .entities
            .iter()
            .filter(|entity| {
                entity.get("name").and_then(|value| value.as_str()) == Some("/v2/whatsapp/webhook")
            })
            .filter_map(|entity| {
                entity
                    .get("auth_policy")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .collect();

        assert_eq!(mounted_flags, vec![true]);
        assert_eq!(auth_policies, vec!["public".to_string()]);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_marks_included_core_subrouters_as_mounted() {
        let root = temp_repo_root();
        let registry_path = root.join("example-api/example/routers");
        let whatsapp_path = root.join("example-api/example/integrations/whatsapp/routers");
        fs::create_dir_all(&registry_path).expect("create registry path");
        fs::create_dir_all(&whatsapp_path).expect("create whatsapp router path");
        fs::write(
            root.join("example-api/example/main.py"),
            r#"
_AUTH_EXEMPT_ROUTERS = {"auth_router", "system_router", "events_router"}
_AUTH_EXEMPT_PATH_SUFFIXES = ("/health", "/healthz", "/webhook", "/flows/data")
"#,
        )
        .expect("write main auth policy");
        fs::write(
            registry_path.join("__init__.py"),
            r#"
_ROUTER_SPECS = {
    "whatsapp_full_router": ("..integrations.whatsapp.routers.full", "router"),
}

_FULL_ROUTER_ENTRIES = (
    ("whatsapp_full_router", "/v2"),
)
"#,
        )
        .expect("write registry");
        fs::write(
            whatsapp_path.join("full.py"),
            r#"
from fastapi import APIRouter
from example.integrations.whatsapp.routers.waba import router as waba_router

router = APIRouter(prefix="/whatsapp", tags=["whatsapp"])
router.include_router(waba_router)
"#,
        )
        .expect("write whatsapp full router");
        fs::write(
            whatsapp_path.join("waba.py"),
            r#"
from fastapi import APIRouter

router = APIRouter()

@router.get("/meta/wabas")
async def list_meta_wabas():
    return {"ok": True}
"#,
        )
        .expect("write whatsapp waba router");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![FileRecord {
                path: "example-api/example/integrations/whatsapp/routers/waba.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 0,
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

        let result = find_api_routes(&index, "meta");
        let mounted_flags: Vec<bool> = result
            .entities
            .iter()
            .filter(|entity| {
                entity.get("name").and_then(|value| value.as_str()) == Some("/meta/wabas")
            })
            .filter_map(|entity| entity.get("mounted").and_then(|value| value.as_bool()))
            .collect();
        let auth_policies: Vec<String> = result
            .entities
            .iter()
            .filter(|entity| {
                entity.get("name").and_then(|value| value.as_str()) == Some("/meta/wabas")
            })
            .filter_map(|entity| {
                entity
                    .get("auth_policy")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .collect();

        assert_eq!(mounted_flags, vec![true]);
        assert_eq!(auth_policies, vec!["protected".to_string()]);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_marks_unregistered_core_router_files_as_unmounted() {
        let root = temp_repo_root();
        let router_path = root.join("example-api/example/routers");
        fs::create_dir_all(&router_path).expect("create router path");
        fs::write(
            root.join("example-api/example/main.py"),
            r#"
_AUTH_EXEMPT_ROUTERS = {"auth_router", "system_router"}
_AUTH_EXEMPT_PATH_SUFFIXES = ("/health", "/healthz", "/webhook")
"#,
        )
        .expect("write main auth policy");
        fs::write(
            router_path.join("__init__.py"),
            r#"
_ROUTER_SPECS = {
    "auth_router": (".auth", "router"),
}

_FULL_ROUTER_ENTRIES = (
    ("auth_router", None),
)
"#,
        )
        .expect("write registry");
        fs::write(
            router_path.join("navigate.py"),
            r#"
from fastapi import APIRouter

router = APIRouter(prefix="/v2/navigate", tags=["navigate"])

@router.get("/sessions")
async def list_sessions():
    return {"ok": True}
"#,
        )
        .expect("write legacy navigate router");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![FileRecord {
                path: "example-api/example/routers/navigate.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 0,
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

        let result = find_api_routes(&index, "navigate");
        let statuses: Vec<String> = result
            .entities
            .iter()
            .filter(|entity| {
                entity.get("name").and_then(|value| value.as_str()) == Some("/v2/navigate/sessions")
            })
            .filter_map(|entity| {
                entity
                    .get("mount_status")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .collect();
        let auth_sources: Vec<String> = result
            .entities
            .iter()
            .filter(|entity| {
                entity.get("name").and_then(|value| value.as_str()) == Some("/v2/navigate/sessions")
            })
            .filter_map(|entity| {
                entity
                    .get("auth_source")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .collect();

        assert_eq!(statuses, vec!["unmounted".to_string()]);
        assert_eq!(
            auth_sources,
            vec!["core_auth_injection_if_mounted:get_current_user".to_string()]
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_prefers_concrete_routes_over_route_adjacent_test_symbols() {
        let root = temp_repo_root();
        let router_path = root.join("example-api/example/routers");
        let test_path = root.join("example-ops/src/app/api/auth/__tests__");
        fs::create_dir_all(&router_path).expect("create router path");
        fs::create_dir_all(&test_path).expect("create test path");
        fs::write(
            root.join("example-api/example/main.py"),
            r#"
_AUTH_EXEMPT_ROUTERS = {"auth_router"}
_AUTH_EXEMPT_PATH_SUFFIXES = ("/health", "/healthz", "/webhook")
"#,
        )
        .expect("write main auth policy");
        fs::write(
            router_path.join("__init__.py"),
            r#"
_ROUTER_SPECS = {
    "auth_router": (".auth", "router"),
}

_FULL_ROUTER_ENTRIES = (
    ("auth_router", None),
)
"#,
        )
        .expect("write registry");
        fs::write(
            router_path.join("auth.py"),
            r#"
from fastapi import APIRouter

router = APIRouter(prefix="/v2/auth", tags=["auth"])

@router.post("/login")
async def login():
    return {"ok": True}
"#,
        )
        .expect("write auth router");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![
                FileRecord {
                    path: "example-api/example/routers/auth.py".to_string(),
                    language: SourceLanguage::Python,
                    bytes: 0,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "example-ops/src/app/api/auth/__tests__/backend-preference.test.ts"
                        .to_string(),
                    language: SourceLanguage::TypeScript,
                    bytes: 0,
                    modified_unix_ms: 0,
                    symbols: vec![crate::model::SymbolOccurrence {
                        name: "loginRoute".to_string(),
                        kind: crate::model::SymbolKind::Variable,
                        path: "example-ops/src/app/api/auth/__tests__/backend-preference.test.ts"
                            .to_string(),
                        line: 40,
                        language: SourceLanguage::TypeScript,
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

        let result = find_api_routes(&index, "login");
        assert_eq!(result.entities.len(), 1);
        assert_eq!(
            result.entities[0]
                .get("name")
                .and_then(|value| value.as_str()),
            Some("/v2/auth/login")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_groups_same_path_across_methods() {
        let root = temp_repo_root();
        let router_path = root.join("example-api/example/routers");
        fs::create_dir_all(&router_path).expect("create router path");
        fs::write(
            root.join("example-api/example/main.py"),
            r#"
_AUTH_EXEMPT_ROUTERS = {"events_router"}
_AUTH_EXEMPT_PATH_SUFFIXES = ("/health", "/healthz", "/webhook")
"#,
        )
        .expect("write main auth policy");
        fs::write(
            router_path.join("__init__.py"),
            r#"
_ROUTER_SPECS = {
    "events_router": (".events", "router"),
}

_FULL_ROUTER_ENTRIES = (
    ("events_router", None),
)
"#,
        )
        .expect("write registry");
        fs::write(
            router_path.join("events.py"),
            r#"
from fastapi import APIRouter

router = APIRouter(prefix="/v2/events", tags=["events"])

@router.get("/webhook")
async def verify_webhook():
    return {"ok": True}

@router.post("/webhook")
async def receive_webhook():
    return {"ok": True}
"#,
        )
        .expect("write events router");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![FileRecord {
                path: "example-api/example/routers/events.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 0,
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

        let result = find_api_routes(&index, "webhook");
        assert_eq!(result.entities.len(), 1);
        let methods: Vec<String> = result.entities[0]
            .get("methods")
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect();
        let handlers: Vec<String> = result.entities[0]
            .get("handlers")
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect();

        assert_eq!(methods, vec!["GET".to_string(), "POST".to_string()]);
        assert_eq!(
            handlers,
            vec!["verify_webhook".to_string(), "receive_webhook".to_string()]
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_ranks_mounted_core_routes_ahead_of_profile_activated_cartridges() {
        let root = temp_repo_root();
        let router_path = root.join("example-api/example/routers");
        let cartridge_path = root.join("cartridges/evo");
        fs::create_dir_all(&router_path).expect("create router path");
        fs::create_dir_all(&cartridge_path).expect("create cartridge path");
        fs::write(
            root.join("example-api/example/main.py"),
            r#"
_AUTH_EXEMPT_ROUTERS = {"auth_router", "system_router", "events_router"}
_AUTH_EXEMPT_PATH_SUFFIXES = ("/health", "/healthz", "/webhook")
"#,
        )
        .expect("write main auth policy");
        fs::write(
            router_path.join("__init__.py"),
            r#"
_ROUTER_SPECS = {
    "events_router": (".events", "router"),
}

_FULL_ROUTER_ENTRIES = (
    ("events_router", None),
)
"#,
        )
        .expect("write registry");
        fs::write(
            router_path.join("events.py"),
            r#"
from fastapi import APIRouter

router = APIRouter(prefix="/v2/whatsapp", tags=["events"])

@router.get("/webhook")
async def verify_webhook():
    return {"ok": True}

@router.post("/webhook")
async def receive_webhook():
    return {"ok": True}
"#,
        )
        .expect("write core webhook router");
        fs::write(
            cartridge_path.join("router.py"),
            r#"
from fastapi import APIRouter

router = APIRouter(prefix="/v2/evo", tags=["evo"])

@router.get("/webhooks")
async def list_webhooks():
    return {"ok": True}
"#,
        )
        .expect("write cartridge router");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![
                FileRecord {
                    path: "example-api/example/routers/events.py".to_string(),
                    language: SourceLanguage::Python,
                    bytes: 0,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "cartridges/evo/router.py".to_string(),
                    language: SourceLanguage::Python,
                    bytes: 0,
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
            profiles: vec![crate::model::ProfileRecord {
                name: "liz_cobranca.env".to_string(),
                path: "deploy/profiles/liz_cobranca.env".to_string(),
                vars: vec![crate::model::DeclaredVar {
                    name: "EXAMPLE_ACTIVE_CARTRIDGES".to_string(),
                    value_preview: Some("evo".to_string()),
                    raw_value: None,
                }],
            }],
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let result = find_api_routes(&index, "webhook");
        assert_eq!(
            result.entities[0]
                .get("name")
                .and_then(|value| value.as_str()),
            Some("/v2/whatsapp/webhook")
        );
        assert_eq!(
            result.entities[1]
                .get("name")
                .and_then(|value| value.as_str()),
            Some("/v2/evo/webhooks")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_marks_compatibility_aliases_with_canonical_paths() {
        let root = temp_repo_root();
        let router_path = root.join("example-api/example/routers");
        fs::create_dir_all(&router_path).expect("create router path");
        fs::write(
            root.join("example-api/example/main.py"),
            r#"
_AUTH_EXEMPT_ROUTERS = {"auth_router"}
_AUTH_EXEMPT_PATH_SUFFIXES = ("/health", "/healthz", "/webhook")
"#,
        )
        .expect("write main auth policy");
        fs::write(
            router_path.join("__init__.py"),
            r#"
_ROUTER_SPECS = {
    "conversations_leio_router": (".conversations_leio", "router"),
}

_FULL_ROUTER_ENTRIES = (
    ("conversations_leio_router", None),
)
"#,
        )
        .expect("write registry");
        fs::write(
            router_path.join("conversations_leio.py"),
            r#"
"""Canonical endpoints:
    POST   /v2/leio/conversations/{id}/ask

Legacy aliases kept for compatibility where they do not collide with the
conversation lifecycle router:
    POST   /v2/conversations/{id}/ask
"""

from fastapi import APIRouter

canonical_router = APIRouter(prefix="/v2/leio/conversations", tags=["leio-conversations"])
legacy_router = APIRouter(prefix="/v2/conversations", tags=["leio-conversations"])
router = APIRouter()

@canonical_router.post("/{conversation_id}/ask")
@legacy_router.post("/{conversation_id}/ask", include_in_schema=False)
async def ask_conversation():
    return {"ok": True}

router.include_router(canonical_router)
router.include_router(legacy_router)
"#,
        )
        .expect("write compatibility router");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![FileRecord {
                path: "example-api/example/routers/conversations_leio.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 0,
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

        let result = find_api_routes(&index, "ask");
        let canonical = result
            .entities
            .iter()
            .find(|entity| {
                entity.get("name").and_then(|value| value.as_str())
                    == Some("/v2/leio/conversations/{conversation_id}/ask")
            })
            .expect("canonical route should exist");
        let compatibility_alias = result
            .entities
            .iter()
            .find(|entity| {
                entity.get("name").and_then(|value| value.as_str())
                    == Some("/v2/conversations/{conversation_id}/ask")
            })
            .expect("compatibility alias should exist");

        assert_eq!(
            canonical.get("route_role").and_then(|value| value.as_str()),
            Some("canonical")
        );
        assert_eq!(
            compatibility_alias
                .get("route_role")
                .and_then(|value| value.as_str()),
            Some("compatibility_alias")
        );
        assert_eq!(
            compatibility_alias
                .get("canonical_path")
                .and_then(|value| value.as_str()),
            Some("/v2/leio/conversations/{conversation_id}/ask")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_ranks_canonical_routes_ahead_of_compatibility_aliases() {
        let root = temp_repo_root();
        let router_path = root.join("example-api/example/routers");
        fs::create_dir_all(&router_path).expect("create router path");
        fs::write(
            root.join("example-api/example/main.py"),
            r#"
_AUTH_EXEMPT_ROUTERS = {"auth_router"}
_AUTH_EXEMPT_PATH_SUFFIXES = ("/health", "/healthz", "/webhook")
"#,
        )
        .expect("write main auth policy");
        fs::write(
            router_path.join("__init__.py"),
            r#"
_ROUTER_SPECS = {
    "conversations_leio_router": (".conversations_leio", "router"),
}

_FULL_ROUTER_ENTRIES = (
    ("conversations_leio_router", None),
)
"#,
        )
        .expect("write registry");
        fs::write(
            router_path.join("conversations_leio.py"),
            r#"
"""Canonical endpoints:
    POST   /v2/leio/conversations/{id}/ask

Legacy aliases kept for compatibility where they do not collide with the
conversation lifecycle router:
    POST   /v2/conversations/{id}/ask
"""

from fastapi import APIRouter

canonical_router = APIRouter(prefix="/v2/leio/conversations", tags=["leio-conversations"])
legacy_router = APIRouter(prefix="/v2/conversations", tags=["leio-conversations"])
router = APIRouter()

@canonical_router.post("/{conversation_id}/ask")
@legacy_router.post("/{conversation_id}/ask", include_in_schema=False)
async def ask_conversation():
    return {"ok": True}

router.include_router(canonical_router)
router.include_router(legacy_router)
"#,
        )
        .expect("write compatibility router");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![FileRecord {
                path: "example-api/example/routers/conversations_leio.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 0,
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

        let result = find_api_routes(&index, "ask");
        assert_eq!(
            result.entities[0]
                .get("name")
                .and_then(|value| value.as_str()),
            Some("/v2/leio/conversations/{conversation_id}/ask")
        );
        assert_eq!(
            result.entities[1]
                .get("name")
                .and_then(|value| value.as_str()),
            Some("/v2/conversations/{conversation_id}/ask")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_uses_token_matching_for_short_queries() {
        let root = temp_repo_root();
        let agents_path = root.join("example-api/example/agents");
        let router_path = root.join("example-api/example/routers");
        fs::create_dir_all(&agents_path).expect("create agents path");
        fs::create_dir_all(&router_path).expect("create router path");
        fs::write(
            root.join("example-api/example/main.py"),
            r#"
_AUTH_EXEMPT_ROUTERS = {"auth_router"}
_AUTH_EXEMPT_PATH_SUFFIXES = ("/health", "/healthz", "/webhook")
"#,
        )
        .expect("write main auth policy");
        fs::write(
            router_path.join("__init__.py"),
            r#"
_ROUTER_SPECS = {
    "conversations_leio_router": (".conversations_leio", "router"),
}

_FULL_ROUTER_ENTRIES = (
    ("conversations_leio_router", None),
)
"#,
        )
        .expect("write registry");
        fs::write(
            agents_path.join("router.py"),
            r#"
from fastapi import APIRouter

router = APIRouter(prefix="/v2/tasks", tags=["tasks"])

@router.get("")
async def list_tasks():
    return {"ok": True}
"#,
        )
        .expect("write tasks router");
        fs::write(
            router_path.join("conversations_leio.py"),
            r#"
"""Canonical endpoints:
    POST   /v2/leio/conversations/{id}/ask
"""

from fastapi import APIRouter

router = APIRouter(prefix="/v2/leio/conversations", tags=["leio-conversations"])

@router.post("/{conversation_id}/ask")
async def ask_conversation():
    return {"ok": True}
"#,
        )
        .expect("write ask router");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![
                FileRecord {
                    path: "example-api/example/agents/router.py".to_string(),
                    language: SourceLanguage::Python,
                    bytes: 0,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "example-api/example/routers/conversations_leio.py".to_string(),
                    language: SourceLanguage::Python,
                    bytes: 0,
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

        let result = find_api_routes(&index, "ask");
        let names: Vec<&str> = result
            .entities
            .iter()
            .filter_map(|entity| entity.get("name").and_then(|value| value.as_str()))
            .collect();

        assert_eq!(names, vec!["/v2/leio/conversations/{conversation_id}/ask"]);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_emits_family_summaries_for_broad_queries() {
        let root = temp_repo_root();
        let router_path = root.join("example-api/example/routers");
        fs::create_dir_all(&router_path).expect("create router path");
        fs::write(
            root.join("example-api/example/main.py"),
            r#"
_AUTH_EXEMPT_ROUTERS = {"auth_router"}
_AUTH_EXEMPT_PATH_SUFFIXES = ("/health", "/healthz", "/webhook")
"#,
        )
        .expect("write main auth policy");
        fs::write(
            router_path.join("__init__.py"),
            r#"
_ROUTER_SPECS = {
    "conversations_router": (".conversations", "router"),
    "conversations_leio_router": (".conversations_leio", "router"),
}

_FULL_ROUTER_ENTRIES = (
    ("conversations_router", None),
    ("conversations_leio_router", None),
)
"#,
        )
        .expect("write registry");
        fs::write(
            router_path.join("conversations.py"),
            r#"
from fastapi import APIRouter

router = APIRouter(prefix="/v2/conversations", tags=["conversations"])

@router.get("")
async def list_conversations():
    return {"ok": True}

@router.get("/{conv_id}")
async def get_conversation():
    return {"ok": True}

@router.post("/{conv_id}/handoff")
async def handoff_conversation():
    return {"ok": True}
"#,
        )
        .expect("write lifecycle router");
        fs::write(
            router_path.join("conversations_leio.py"),
            r#"
"""Canonical endpoints:
    POST   /v2/leio/conversations
    POST   /v2/leio/conversations/{id}/ask
    GET    /v2/leio/conversations/{id}

Legacy aliases kept for compatibility where they do not collide with the
conversation lifecycle router:
    POST   /v2/conversations
    POST   /v2/conversations/{id}/ask
    GET    /v2/conversations/{id}/history
"""

from fastapi import APIRouter

canonical_router = APIRouter(prefix="/v2/leio/conversations", tags=["leio-conversations"])
legacy_router = APIRouter(prefix="/v2/conversations", tags=["leio-conversations"])
router = APIRouter()

@canonical_router.post("")
@legacy_router.post("", include_in_schema=False)
async def create_conversation():
    return {"ok": True}

@canonical_router.post("/{conversation_id}/ask")
@legacy_router.post("/{conversation_id}/ask", include_in_schema=False)
async def ask_conversation():
    return {"ok": True}

@canonical_router.get("/{conversation_id}")
@legacy_router.get("/{conversation_id}/history", include_in_schema=False)
async def get_conversation():
    return {"ok": True}

router.include_router(canonical_router)
router.include_router(legacy_router)
"#,
        )
        .expect("write leio router");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![
                FileRecord {
                    path: "example-api/example/routers/conversations.py".to_string(),
                    language: SourceLanguage::Python,
                    bytes: 0,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "example-api/example/routers/conversations_leio.py".to_string(),
                    language: SourceLanguage::Python,
                    bytes: 0,
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

        let result = find_api_routes(&index, "conversations");
        assert_eq!(
            result.entities[0]
                .get("kind")
                .and_then(|value| value.as_str()),
            Some("route_family")
        );
        assert_eq!(
            result.entities[1]
                .get("kind")
                .and_then(|value| value.as_str()),
            Some("route_family")
        );
        let family_labels: Vec<&str> = result
            .entities
            .iter()
            .filter(|entity| {
                entity.get("kind").and_then(|value| value.as_str()) == Some("route_family")
            })
            .filter_map(|entity| {
                entity
                    .get("route_family_label")
                    .and_then(|value| value.as_str())
            })
            .collect();

        assert!(family_labels.contains(&"conversations"));
        assert!(family_labels.contains(&"leio-conversations"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_ranks_canonical_family_ahead_of_ops_family() {
        let root = temp_repo_root();
        let router_path = root.join("example-api/example/routers");
        fs::create_dir_all(&router_path).expect("create router path");
        fs::write(
            root.join("example-api/example/main.py"),
            r#"
_AUTH_EXEMPT_ROUTERS = {"auth_router"}
_AUTH_EXEMPT_PATH_SUFFIXES = ("/health", "/healthz", "/webhook")
"#,
        )
        .expect("write main auth policy");
        fs::write(
            router_path.join("__init__.py"),
            r#"
_ROUTER_SPECS = {
    "conversations_router": (".conversations", "router"),
    "conversations_leio_router": (".conversations_leio", "router"),
    "ops_console_router": (".ops_console", "router"),
}

_FULL_ROUTER_ENTRIES = (
    ("conversations_router", None),
    ("conversations_leio_router", None),
    ("ops_console_router", None),
)
"#,
        )
        .expect("write registry");
        fs::write(
            router_path.join("conversations.py"),
            r#"
from fastapi import APIRouter

router = APIRouter(prefix="/v2/conversations", tags=["conversations"])

@router.get("")
async def list_conversations():
    return {"ok": True}
"#,
        )
        .expect("write conversations router");
        fs::write(
            router_path.join("conversations_leio.py"),
            r#"
"""Canonical endpoints:
    POST   /v2/leio/conversations
    GET    /v2/leio/conversations/{id}

Legacy aliases kept for compatibility where they do not collide with the
conversation lifecycle router:
    POST   /v2/conversations
    GET    /v2/conversations/{id}/history
"""

from fastapi import APIRouter

canonical_router = APIRouter(prefix="/v2/leio/conversations", tags=["leio-conversations"])
legacy_router = APIRouter(prefix="/v2/conversations", tags=["leio-conversations"])
router = APIRouter()

@canonical_router.post("")
@legacy_router.post("", include_in_schema=False)
async def create_conversation():
    return {"ok": True}

@canonical_router.get("/{conversation_id}")
@legacy_router.get("/{conversation_id}/history", include_in_schema=False)
async def get_conversation():
    return {"ok": True}

router.include_router(canonical_router)
router.include_router(legacy_router)
"#,
        )
        .expect("write leio router");
        fs::write(
            router_path.join("ops_console.py"),
            r#"
from fastapi import APIRouter

router = APIRouter(prefix="/ops", tags=["ops-console"])

@router.get("/api/conversations")
async def list_conversations():
    return {"ok": True}
"#,
        )
        .expect("write ops router");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![
                FileRecord {
                    path: "example-api/example/routers/conversations.py".to_string(),
                    language: SourceLanguage::Python,
                    bytes: 0,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "example-api/example/routers/conversations_leio.py".to_string(),
                    language: SourceLanguage::Python,
                    bytes: 0,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "example-api/example/routers/ops_console.py".to_string(),
                    language: SourceLanguage::Python,
                    bytes: 0,
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

        let result = find_api_routes(&index, "conversations");
        let family_labels: Vec<&str> = result
            .entities
            .iter()
            .filter(|entity| {
                entity.get("kind").and_then(|value| value.as_str()) == Some("route_family")
            })
            .filter_map(|entity| {
                entity
                    .get("route_family_label")
                    .and_then(|value| value.as_str())
            })
            .collect();

        assert_eq!(family_labels[0], "leio-conversations");
        assert_eq!(family_labels[1], "conversations");
        assert_eq!(family_labels[2], "ops-console");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_ranks_public_webhook_family_ahead_of_ops_and_profile_families() {
        let root = temp_repo_root();
        let router_path = root.join("example-api/example/routers");
        let whatsapp_path = root.join("example-api/example/integrations/whatsapp/routers");
        let cartridge_path = root.join("cartridges/evo");
        fs::create_dir_all(&router_path).expect("create router path");
        fs::create_dir_all(&whatsapp_path).expect("create whatsapp path");
        fs::create_dir_all(&cartridge_path).expect("create cartridge path");
        fs::write(
            root.join("example-api/example/main.py"),
            r#"
_AUTH_EXEMPT_ROUTERS = {"auth_router"}
_AUTH_EXEMPT_PATH_SUFFIXES = ("/health", "/healthz", "/webhook")
"#,
        )
        .expect("write main auth policy");
        fs::write(
            router_path.join("__init__.py"),
            r#"
_ROUTER_SPECS = {
    "whatsapp_webhook_router": ("..integrations.whatsapp.routers.webhook", "router"),
    "ops_console_router": (".ops_console", "router"),
}

_FULL_ROUTER_ENTRIES = (
    ("whatsapp_webhook_router", None),
    ("ops_console_router", None),
)
"#,
        )
        .expect("write registry");
        fs::write(
            whatsapp_path.join("webhook.py"),
            r#"
from fastapi import APIRouter

router = APIRouter(prefix="/v2/whatsapp", tags=["whatsapp-webhook"])

@router.get("/webhook")
async def verify_webhook():
    return {"ok": True}

@router.post("/webhook")
async def receive_webhook():
    return {"ok": True}

@router.get("/webhook/debug")
async def webhook_debug():
    return {"ok": True}

@router.get("/webhook/status")
async def webhook_status():
    return {"ok": True}
"#,
        )
        .expect("write whatsapp webhook router");
        fs::write(
            router_path.join("ops_console.py"),
            r#"
from fastapi import APIRouter

router = APIRouter(prefix="/ops", tags=["ops-console"])

@router.get("/api/ingest/webhooks")
async def list_ingest_webhooks():
    return {"ok": True}

@router.get("/api/ingest/webhooks/{webhook_id}")
async def get_ingest_webhook():
    return {"ok": True}

@router.post("/api/ingest/webhooks/{webhook_id}/retry")
async def retry_ingest_webhook():
    return {"ok": True}
"#,
        )
        .expect("write ops router");
        fs::write(
            cartridge_path.join("router.py"),
            r#"
from fastapi import APIRouter

router = APIRouter(prefix="/v2/evo")

@router.get("/webhooks")
async def list_webhooks():
    return {"ok": True}

@router.post("/webhooks")
async def create_webhook():
    return {"ok": True}

@router.delete("/webhooks")
async def delete_webhook():
    return {"ok": True}
"#,
        )
        .expect("write cartridge router");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![
                FileRecord {
                    path: "example-api/example/integrations/whatsapp/routers/webhook.py"
                        .to_string(),
                    language: SourceLanguage::Python,
                    bytes: 0,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "example-api/example/routers/ops_console.py".to_string(),
                    language: SourceLanguage::Python,
                    bytes: 0,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "cartridges/evo/router.py".to_string(),
                    language: SourceLanguage::Python,
                    bytes: 0,
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
            profiles: vec![crate::model::ProfileRecord {
                name: "liz_cobranca.env".to_string(),
                path: "deploy/profiles/liz_cobranca.env".to_string(),
                vars: vec![crate::model::DeclaredVar {
                    name: "EXAMPLE_ACTIVE_CARTRIDGES".to_string(),
                    value_preview: Some("evo".to_string()),
                    raw_value: None,
                }],
            }],
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let result = find_api_routes(&index, "webhook");
        let family_labels: Vec<&str> = result
            .entities
            .iter()
            .filter(|entity| {
                entity.get("kind").and_then(|value| value.as_str()) == Some("route_family")
            })
            .filter_map(|entity| {
                entity
                    .get("route_family_label")
                    .and_then(|value| value.as_str())
            })
            .collect();

        assert_eq!(family_labels[0], "whatsapp-webhook");
        assert_eq!(family_labels[1], "evo");
        assert!(family_labels.contains(&"ops-console"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_uses_cartridge_identity_for_tagless_router_families() {
        let root = temp_repo_root();
        let cartridge_path = root.join("cartridges/evo");
        fs::create_dir_all(&cartridge_path).expect("create cartridge path");
        fs::write(
            cartridge_path.join("router.py"),
            r#"
from fastapi import APIRouter

router = APIRouter(
    prefix="/v2/evo",
    dependencies=[],
)

@router.get("/webhooks")
async def list_webhooks():
    return {"ok": True}
"#,
        )
        .expect("write cartridge router");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![FileRecord {
                path: "cartridges/evo/router.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 0,
                modified_unix_ms: 0,
                symbols: Vec::new(),
                env_vars: Vec::new(),
                redis_keys: Vec::new(),
                subprocess_calls: Vec::new(),
                http_calls: Vec::new(),
                unresolved_edges: Vec::new(),
            }],
            deploy_targets: Vec::new(),
            profiles: vec![crate::model::ProfileRecord {
                name: "liz_cobranca.env".to_string(),
                path: "deploy/profiles/liz_cobranca.env".to_string(),
                vars: vec![crate::model::DeclaredVar {
                    name: "EXAMPLE_ACTIVE_CARTRIDGES".to_string(),
                    value_preview: Some("evo".to_string()),
                    raw_value: None,
                }],
            }],
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let result = find_api_routes(&index, "webhook");
        assert_eq!(
            result
                .entities
                .iter()
                .find(|entity| entity.get("kind").and_then(|value| value.as_str()) == Some("route"))
                .and_then(|entity| entity.get("route_family_label"))
                .and_then(|value| value.as_str()),
            Some("evo")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_extracts_nested_apirouter_prefix_and_tags() {
        let root = temp_repo_root();
        let router_path = root.join("example-api/example/routers");
        fs::create_dir_all(&router_path).expect("create router path");
        fs::write(
            root.join("example-api/example/main.py"),
            r#"
_AUTH_EXEMPT_ROUTERS = {"auth_router"}
_AUTH_EXEMPT_PATH_SUFFIXES = ("/health", "/healthz", "/webhook")
"#,
        )
        .expect("write main auth policy");
        fs::write(
            router_path.join("__init__.py"),
            r#"
_ROUTER_SPECS = {
    "admin_router": (".admin", "router"),
}

_FULL_ROUTER_ENTRIES = (
    ("admin_router", None),
)
"#,
        )
        .expect("write registry");
        fs::write(
            router_path.join("admin.py"),
            r#"
from fastapi import APIRouter, Depends

def require_admin():
    return True

router = APIRouter(
    prefix="/v2/internal-admin",
    tags=["ops-admin"],
    dependencies=[Depends(require_admin)],
)

@router.get("/users")
async def list_users():
    return {"ok": True}
"#,
        )
        .expect("write nested admin router");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![FileRecord {
                path: "example-api/example/routers/admin.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 0,
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

        let result = find_api_routes(&index, "users");
        assert_eq!(
            result.entities[0]
                .get("name")
                .and_then(|value| value.as_str()),
            Some("/v2/internal-admin/users")
        );
        assert_eq!(
            result.entities[0]
                .get("route_family_label")
                .and_then(|value| value.as_str()),
            Some("ops-admin")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_normalizes_unicode_family_labels() {
        let root = temp_repo_root();
        let router_path = root.join("example-api/example/routers");
        fs::create_dir_all(&router_path).expect("create router path");
        fs::write(
            root.join("example-api/example/main.py"),
            r#"
_AUTH_EXEMPT_ROUTERS = {"auth_router"}
_AUTH_EXEMPT_PATH_SUFFIXES = ("/health", "/healthz", "/webhook")
"#,
        )
        .expect("write main auth policy");
        fs::write(
            router_path.join("__init__.py"),
            r#"
_ROUTER_SPECS = {
    "pacto_cobranca_router": (".pacto_cobranca", "router"),
}

_FULL_ROUTER_ENTRIES = (
    ("pacto_cobranca_router", None),
)
"#,
        )
        .expect("write registry");
        fs::write(
            router_path.join("pacto_cobranca.py"),
            r#"
from fastapi import APIRouter

router = APIRouter(
    prefix="/v2/pacto-cobranca",
    tags=["Pacto Cobrança"],
)

@router.post("/cobrar")
async def enviar_cobranca():
    return {"ok": True}
"#,
        )
        .expect("write pacto router");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![FileRecord {
                path: "example-api/example/routers/pacto_cobranca.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 0,
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

        let result = find_api_routes(&index, "pacto");
        assert_eq!(
            result.entities[0]
                .get("route_family")
                .and_then(|value| value.as_str()),
            Some("pacto_cobranca")
        );
        assert_eq!(
            result.entities[0]
                .get("route_family_label")
                .and_then(|value| value.as_str()),
            Some("Pacto Cobrança")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_api_routes_compacts_large_broad_family_queries() {
        let root = temp_repo_root();
        let router_path = root.join("example-api/example/routers");
        let cartridge_path = root.join("cartridges/pacto");
        fs::create_dir_all(&router_path).expect("create router path");
        fs::create_dir_all(&cartridge_path).expect("create cartridge path");
        fs::write(
            root.join("example-api/example/main.py"),
            r#"
_AUTH_EXEMPT_ROUTERS = {"auth_router"}
_AUTH_EXEMPT_PATH_SUFFIXES = ("/health", "/healthz", "/webhook")
"#,
        )
        .expect("write main auth policy");
        fs::write(
            router_path.join("__init__.py"),
            r#"
_ROUTER_SPECS = {
    "pacto_cobranca_router": (".pacto_cobranca", "router"),
}

_FULL_ROUTER_ENTRIES = (
    ("pacto_cobranca_router", None),
)
"#,
        )
        .expect("write registry");

        let mut core_router = String::from(
            r#"
from fastapi import APIRouter

router = APIRouter(prefix="/v2/pacto-cobranca", tags=["Pacto Cobrança"])
"#,
        );
        for idx in 0..18 {
            core_router.push_str(&format!(
                r#"

@router.get("/core-{idx}")
async def core_{idx}():
    return {{"ok": True}}
"#,
            ));
        }
        fs::write(router_path.join("pacto_cobranca.py"), core_router).expect("write core router");

        let mut cartridge_router = String::from(
            r#"
from fastapi import APIRouter

router = APIRouter(prefix="/v2/pacto", tags=["pacto"])
"#,
        );
        for idx in 0..36 {
            cartridge_router.push_str(&format!(
                r#"

@router.get("/route-{idx}")
async def route_{idx}():
    return {{"ok": True}}
"#,
            ));
        }
        fs::write(cartridge_path.join("router.py"), cartridge_router)
            .expect("write cartridge router");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![
                FileRecord {
                    path: "example-api/example/routers/pacto_cobranca.py".to_string(),
                    language: SourceLanguage::Python,
                    bytes: 0,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "cartridges/pacto/router.py".to_string(),
                    language: SourceLanguage::Python,
                    bytes: 0,
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
            profiles: vec![crate::model::ProfileRecord {
                name: "pacto.env".to_string(),
                path: "deploy/profiles/pacto.env".to_string(),
                vars: vec![crate::model::DeclaredVar {
                    name: "EXAMPLE_ACTIVE_CARTRIDGES".to_string(),
                    value_preview: Some("pacto".to_string()),
                    raw_value: None,
                }],
            }],
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let result = find_api_routes(&index, "pacto");
        let route_entities: Vec<_> = result
            .entities
            .iter()
            .filter(|entity| entity.get("kind").and_then(|value| value.as_str()) == Some("route"))
            .collect();
        let family_entities: Vec<_> = result
            .entities
            .iter()
            .filter(|entity| {
                entity.get("kind").and_then(|value| value.as_str()) == Some("route_family")
            })
            .collect();

        assert!(result.summary.contains("showing"));
        assert_eq!(
            result.entities[0]
                .get("kind")
                .and_then(|value| value.as_str()),
            Some("route_family")
        );
        assert!(!family_entities.is_empty());
        assert!(route_entities.len() < 54);
        assert!(route_entities.len() <= 14);
        assert!(route_entities.iter().any(|entity| {
            entity.get("route_family").and_then(|value| value.as_str()) == Some("pacto_cobranca")
        }));
        assert!(route_entities.iter().any(|entity| {
            entity.get("route_family").and_then(|value| value.as_str()) == Some("pacto")
        }));

        let _ = fs::remove_dir_all(root);
    }
}

pub fn find_docker_services(index: &RepoIndex, needle: &str) -> QueryEnvelope {
    let started = Instant::now();
    let query = needle.to_ascii_lowercase();
    let services = collect_docker_services(index);
    let generic_query = query.contains("docker")
        || query.contains("compose")
        || query.contains("service")
        || query.contains("container");

    let mut matches: Vec<serde_json::Value> = Vec::new();
    let mut evidence: Vec<EvidenceItem> = Vec::new();

    for service in services {
        let mut haystacks = vec![
            service.name.to_ascii_lowercase(),
            service.file_path.to_ascii_lowercase(),
        ];
        if let Some(image) = service.image.as_ref() {
            haystacks.push(image.to_ascii_lowercase());
        }
        if let Some(build_context) = service.build_context.as_ref() {
            haystacks.push(build_context.to_ascii_lowercase());
        }
        haystacks.extend(
            service
                .profiles
                .iter()
                .map(|profile| profile.to_ascii_lowercase()),
        );

        if generic_query || haystacks.iter().any(|value| value.contains(&query)) {
            matches.push(json!({
                "name": service.name,
                "kind": "docker_service",
                "path": service.file_path,
                "image": service.image,
                "build_context": service.build_context,
                "profiles": service.profiles,
            }));
            evidence.push(EvidenceItem {
                kind: "docker_service".to_string(),
                path: service.file_path.clone(),
                line: None,
                detail: format!(
                    "service {}{}{}",
                    service.name,
                    service
                        .image
                        .as_ref()
                        .map(|image| format!(" image={image}"))
                        .unwrap_or_default(),
                    service
                        .build_context
                        .as_ref()
                        .map(|context| format!(" build={context}"))
                        .unwrap_or_default(),
                ),
            });
        }
    }

    // Keep lightweight fallback evidence for Dockerfiles or service-related symbols.
    for file in index.files.iter().filter(|f| {
        let name = f.path.to_ascii_lowercase();
        name.contains("docker-compose")
            || name.contains("compose.yaml")
            || name.contains("compose.yml")
    }) {
        let file_lower = file.path.to_ascii_lowercase();
        if generic_query || file_lower.contains(&query) {
            evidence.push(EvidenceItem {
                kind: "docker_file".to_string(),
                path: file.path.clone(),
                line: None,
                detail: "Docker Compose file".to_string(),
            });
        }
    }

    for file in &index.files {
        for sym in &file.symbols {
            let name_lower = sym.name.to_ascii_lowercase();
            if name_lower.contains(&query)
                && (name_lower.contains("service")
                    || name_lower.contains("container")
                    || name_lower.contains("sidecar")
                    || name_lower.contains("worker"))
            {
                matches.push(json!({
                    "name": sym.name,
                    "kind": sym.kind.as_str(),
                    "path": sym.path,
                    "line": sym.line,
                    "language": sym.language.as_str(),
                }));
                evidence.push(EvidenceItem {
                    kind: "docker_symbol".to_string(),
                    path: sym.path.clone(),
                    line: Some(sym.line),
                    detail: format!("{} {}", sym.kind.as_str(), sym.name),
                });
            }
        }
    }

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("find_docker_service"),
        kind: "find".to_string(),
        summary: format!(
            "found {} Docker service matches for `{}`",
            matches.len(),
            needle
        ),
        confidence: confidence(matches.len()),
        entities: matches,
        evidence,
        warnings: Vec::new(),
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn is_docker_compose_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with("docker-compose.yml")
        || lower.ends_with("docker-compose.yaml")
        || lower.ends_with("compose.yml")
        || lower.ends_with("compose.yaml")
}

fn yaml_mapping_value<'a>(mapping: &'a YamlMapping, key: &str) -> Option<&'a YamlValue> {
    mapping.iter().find_map(|(candidate, value)| {
        candidate
            .as_str()
            .filter(|candidate_key| *candidate_key == key)
            .map(|_| value)
    })
}

fn yaml_string_values(value: &YamlValue) -> Vec<String> {
    match value {
        YamlValue::String(value) => vec![value.clone()],
        YamlValue::Sequence(items) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

fn yaml_build_context(value: &YamlValue) -> Option<String> {
    match value {
        YamlValue::String(value) => Some(value.clone()),
        YamlValue::Mapping(mapping) => yaml_mapping_value(mapping, "context")
            .and_then(|context| context.as_str())
            .map(str::to_string),
        _ => None,
    }
}

pub(crate) fn collect_docker_services(index: &RepoIndex) -> Vec<DockerServiceCandidate> {
    let mut services = Vec::new();

    for file in index
        .files
        .iter()
        .filter(|file| is_docker_compose_path(&file.path))
    {
        let file_path = Path::new(&index.root).join(&file.path);
        let Ok(source) = fs::read_to_string(&file_path) else {
            continue;
        };
        let Ok(doc) = serde_yaml::from_str::<YamlValue>(&source) else {
            continue;
        };
        let Some(root) = doc.as_mapping() else {
            continue;
        };
        let Some(services_node) = yaml_mapping_value(root, "services") else {
            continue;
        };
        let Some(services_mapping) = services_node.as_mapping() else {
            continue;
        };

        for (service_name, service_value) in services_mapping {
            let Some(name) = service_name.as_str() else {
                continue;
            };
            let service_mapping = service_value.as_mapping();
            let image = service_mapping
                .and_then(|mapping| yaml_mapping_value(mapping, "image"))
                .and_then(|value| value.as_str())
                .map(str::to_string);
            let build_context = service_mapping
                .and_then(|mapping| yaml_mapping_value(mapping, "build"))
                .and_then(yaml_build_context);
            let mut profiles = service_mapping
                .and_then(|mapping| yaml_mapping_value(mapping, "profiles"))
                .map(yaml_string_values)
                .unwrap_or_default();
            profiles.sort();
            profiles.dedup();

            services.push(DockerServiceCandidate {
                name: name.to_string(),
                file_path: file.path.clone(),
                image,
                build_context,
                profiles,
            });
        }
    }

    services.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.name.cmp(&right.name))
    });
    services
}

pub fn explain_cartridge(index: &RepoIndex, name: &str) -> QueryEnvelope {
    let started = Instant::now();
    let facets = index.workspace_facets();
    if !facets.has_cartridges {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("explain_cartridge"),
            kind: "explain".to_string(),
            summary: "repository does not model cartridges".to_string(),
            confidence: 0.96,
            entities: Vec::new(),
            evidence: Vec::new(),
            warnings: Vec::new(),
            meta: Some(json!({
                "facet": "cartridges",
                "facet_available": false,
                "workspace_facets": facets,
            })),
            timing_ms: started.elapsed().as_millis(),
        };
    }
    let query = name.to_ascii_lowercase();
    let activation_profiles = cartridge_activation_profiles(index);

    // Find deploy targets that load this cartridge
    let targets: Vec<_> = index
        .deploy_targets
        .iter()
        .filter(|t| t.cartridges.iter().any(|c| c.to_ascii_lowercase() == query))
        .collect();

    // Collect integrations and health checks across all targets
    let mut all_integrations: Vec<String> = targets
        .iter()
        .flat_map(|t| t.required_integrations.iter().cloned())
        .collect();
    all_integrations.sort();
    all_integrations.dedup();

    let mut all_health_checks: Vec<String> = targets
        .iter()
        .flat_map(|t| t.health_checks.iter().cloned())
        .collect();
    all_health_checks.sort();
    all_health_checks.dedup();

    // Find cartridge source files
    let prefix = format!("cartridges/{}", name);
    let source_files: Vec<_> = index
        .files
        .iter()
        .filter(|f| f.path.starts_with(&prefix))
        .collect();

    if targets.is_empty() && source_files.is_empty() {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("explain_cartridge"),
            kind: "explain".to_string(),
            summary: format!("cartridge `{}` not found in indexed sources", name),
            confidence: 0.0,
            entities: Vec::new(),
            evidence: Vec::new(),
            warnings: vec!["cartridge not present under cartridges/*".to_string()],
            meta: Some(json!({
                "facet": "cartridges",
                "facet_available": true,
                "workspace_facets": facets,
            })),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    // Find cartridge symbols
    let symbols: Vec<_> = source_files
        .iter()
        .flat_map(|f| f.symbols.iter())
        .take(20)
        .collect();
    let profile_matches = activation_profiles
        .get(name)
        .cloned()
        .or_else(|| activation_profiles.get(&query).cloned())
        .unwrap_or_default();
    let warnings = if targets.is_empty() {
        vec!["cartridge not referenced by any deploy target".to_string()]
    } else {
        Vec::new()
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("explain_cartridge"),
        kind: "explain".to_string(),
        summary: format!(
            "cartridge `{}` -> {} deploy targets, {} source files, {} integrations",
            name,
            targets.len(),
            source_files.len(),
            all_integrations.len()
        ),
        confidence: 0.92,
        entities: vec![json!({
            "cartridge": name,
            "directory": prefix,
            "source_file_count": source_files.len(),
            "symbol_count": symbols.len(),
            "deploy_targets": targets.iter().map(|t| json!({
                "name": t.name,
                "topology": t.topology,
                "frontend_project": t.frontend_project,
                "backend_profile": t.backend_profile,
            })).collect::<Vec<_>>(),
            "activation_profiles": profile_matches,
            "required_integrations": all_integrations,
            "health_checks": all_health_checks,
            "key_symbols": symbols.iter().map(|s| json!({
                "name": s.name,
                "kind": s.kind.as_str(),
                "path": s.path,
                "line": s.line,
            })).collect::<Vec<_>>(),
        })],
        evidence: targets
            .iter()
            .map(|t| EvidenceItem {
                kind: "deploy_target".to_string(),
                path: t.path.clone(),
                line: None,
                detail: format!("loads cartridge {}", name),
            })
            .chain(source_files.iter().take(5).map(|f| EvidenceItem {
                kind: "cartridge_file".to_string(),
                path: f.path.clone(),
                line: None,
                detail: format!("source file in {}", name),
            }))
            .collect(),
        warnings,
        meta: Some(json!({
            "facet": "cartridges",
            "facet_available": true,
            "workspace_facets": facets,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn confidence(count: usize) -> f32 {
    if count == 0 {
        0.0
    } else if count == 1 {
        0.95
    } else {
        0.82
    }
}

#[cfg(test)]
mod docker_tests {
    use super::{collect_docker_services, find_docker_services};
    use crate::model::{FileRecord, RepoIndex, SourceLanguage};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEMP_REPO_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_repo_root() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let counter = TEMP_REPO_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "leio-code-docker-tests-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp root");
        path
    }

    #[test]
    fn collect_docker_services_reads_compose_services() {
        let root = temp_repo_root();
        let compose_dir = root.join("stack");
        fs::create_dir_all(&compose_dir).expect("create compose dir");
        fs::write(
            compose_dir.join("docker-compose.yml"),
            r#"
services:
  api:
    image: example-api:latest
    profiles: ["core", "ops"]
  worker:
    build:
      context: ../example-api
    profiles:
      - jobs
"#,
        )
        .expect("write compose file");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![FileRecord {
                path: "stack/docker-compose.yml".to_string(),
                language: SourceLanguage::Yaml,
                bytes: 0,
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

        let services = collect_docker_services(&index);
        assert_eq!(services.len(), 2);
        assert_eq!(services[0].name, "api");
        assert_eq!(services[0].image.as_deref(), Some("example-api:latest"));
        assert_eq!(
            services[0].profiles,
            vec!["core".to_string(), "ops".to_string()]
        );
        assert_eq!(services[1].name, "worker");
        assert_eq!(services[1].build_context.as_deref(), Some("../example-api"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_docker_services_matches_service_metadata() {
        let root = temp_repo_root();
        let compose_dir = root.join("stack");
        fs::create_dir_all(&compose_dir).expect("create compose dir");
        fs::write(
            compose_dir.join("compose.yaml"),
            r#"
services:
  api:
    image: ghcr.io/example/api:latest
    profiles: ["core"]
  worker:
    build: ./worker
    profiles: ["jobs"]
"#,
        )
        .expect("write compose file");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "now".to_string(),
            files: vec![FileRecord {
                path: "stack/compose.yaml".to_string(),
                language: SourceLanguage::Yaml,
                bytes: 0,
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

        let image_result = find_docker_services(&index, "ghcr.io/example/api");
        assert_eq!(image_result.entities.len(), 1);
        assert_eq!(
            image_result.entities[0]
                .get("name")
                .and_then(|value| value.as_str()),
            Some("api")
        );

        let profile_result = find_docker_services(&index, "jobs");
        assert_eq!(profile_result.entities.len(), 1);
        assert_eq!(
            profile_result.entities[0]
                .get("name")
                .and_then(|value| value.as_str()),
            Some("worker")
        );

        let _ = fs::remove_dir_all(root);
    }
}
