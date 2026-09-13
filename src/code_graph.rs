//! Builds and writes the local RDF code graph from tree-sitter parses.
//!
//! Outputs under `.leio-code/exports/code-graph-v1/`:
//! - `graph.nq` — N-Quads with stable `symbol` URNs and revision-keyed
//!   `symbol-occurrence` URNs; captures `containsSymbol`, `importsPath`,
//!   `callsName`, and uniquely-resolved `calls` edges across Rust, Python,
//!   JavaScript, TypeScript, TSX, C#, Razor, Go, C, C++, Bash, Java, Kotlin,
//!   HTML, CSS, Swift, SQL, and RDF/OWL (`owl:imports`).
//! - `manifest.json` — versioned summary with revision, IRI, counts, and a
//!   source fingerprint so consumers can detect staleness cheaply.
//! - `query-cache.json` — flattened JSON view (`CodeGraphQueryCache`) keyed
//!   on revision: symbol lookup, file symbol inventory, call adjacency, raw
//!   and canonical import indexes. [`crate::graph_query`] reads this on the
//!   hot path and falls back to Oxigraph only when the cache is stale.
//!
//! Bump [`CODE_GRAPH_VERSION`] / [`QUERY_CACHE_VERSION`] when the schemas
//! change so stale caches force a rebuild.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tree_sitter::{Language, Node, Parser};

use crate::jsonc::parse_jsonc;
use crate::model::{
    AccessKind, EnvVarOccurrence, EvidenceItem, QueryEnvelope, RedisKeyOccurrence, RepoIndex,
    SourceLanguage, SymbolKind,
};

const CODE_GRAPH_VERSION: u32 = 4;
/// Query-cache schema version. Bump when cache fields or importer index keys
/// change so stale `query-cache.json` files force a rebuild.
///
/// v8 stores `importers_by_raw` as exact raw statements only. Specifier, imported
/// name, and file path resolve at query time from `file_import_details` /
/// `importers_by_target_path`. Older caches stuffed last-segment aliases into
/// `importers_by_raw` and collided on short names (`json`, `os`, `auth`).
pub(crate) const QUERY_CACHE_VERSION: u32 = 9;
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const PROV_SPECIALIZATION_OF: &str = "http://www.w3.org/ns/prov#specializationOf";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodeGraphManifest {
    version: u32,
    repo_root: String,
    indexed_at: String,
    exported_at: String,
    revision: String,
    graph_iri: String,
    graph_path: String,
    query_cache_path: String,
    parsed_file_count: usize,
    symbol_count: usize,
    occurrence_count: usize,
    import_edge_count: usize,
    call_edge_count: usize,
    resolved_call_edge_count: usize,
    language_counts: BTreeMap<String, usize>,
    parse_warning_count: usize,
    #[serde(default)]
    source_fingerprint: String,
    /// Vocabulary prefix written into `graph.nq` term IRIs.
    #[serde(default)]
    code_namespace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeGraphQueryCache {
    pub version: u32,
    #[serde(default)]
    pub query_cache_version: u32,
    pub revision: String,
    pub graph_iri: String,
    pub symbols: BTreeMap<String, CachedGraphSymbol>,
    pub symbol_lookup_name: BTreeMap<String, Vec<String>>,
    pub symbol_lookup_qual_name: BTreeMap<String, Vec<String>>,
    pub files: BTreeMap<String, CachedGraphFile>,
    pub file_lookup_path: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub file_imports: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub file_import_details: BTreeMap<String, Vec<CachedGraphImport>>,
    #[serde(default)]
    pub importers_by_raw: BTreeMap<String, Vec<CachedGraphImporter>>,
    #[serde(default)]
    pub importers_by_target_path: BTreeMap<String, Vec<CachedGraphImporter>>,
    pub callers_by_symbol: BTreeMap<String, Vec<CachedGraphNeighbor>>,
    pub callees_by_symbol: BTreeMap<String, Vec<CachedGraphNeighbor>>,
    pub callsites_by_symbol: BTreeMap<String, Vec<CachedGraphCallsite>>,
    pub file_symbols: BTreeMap<String, Vec<CachedGraphNeighbor>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedGraphSymbol {
    pub iri: String,
    pub name: String,
    pub qual_name: String,
    pub kind: String,
    pub path: String,
    pub line: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedGraphFile {
    pub iri: String,
    pub path: String,
    pub language: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedGraphImport {
    pub raw: String,
    pub syntax_kind: String,
    pub module_specifiers: Vec<String>,
    #[serde(default)]
    pub imported_names: Vec<String>,
    pub line: Option<usize>,
    #[serde(default)]
    pub resolution_kind: String,
    #[serde(default)]
    pub candidate_paths: Vec<String>,
    #[serde(default)]
    pub candidate_symbols: Vec<CachedGraphNeighbor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedGraphImporter {
    pub raw_import: String,
    pub file_iri: String,
    pub path: String,
    pub language: String,
    pub line: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedGraphNeighbor {
    pub iri: String,
    pub name: String,
    pub qual_name: String,
    pub kind: String,
    pub path: String,
    pub line: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedGraphCallsite {
    pub owner_iri: String,
    pub owner_kind: String,
    pub owner_name: String,
    pub owner_qual_name: String,
    pub path: String,
    pub line: Option<usize>,
    pub expr: String,
}

#[derive(Debug, Clone)]
struct ParsedFileGraph {
    path: String,
    language: SourceLanguage,
    file_iri: String,
    definitions: Vec<Definition>,
    imports: Vec<ImportEdge>,
    calls: Vec<CallEdge>,
}

#[derive(Debug, Clone)]
struct Definition {
    stable_iri: String,
    occurrence_iri: String,
    name: String,
    qual_name: String,
    kind: SymbolKind,
    start_line: usize,
    end_line: usize,
}

#[derive(Debug, Clone)]
struct ImportEdge {
    owner_iri: String,
    raw: String,
    syntax_kind: String,
    module_specifiers: Vec<String>,
    imported_names: Vec<String>,
    line: Option<usize>,
}

#[derive(Debug, Clone)]
struct CallEdge {
    owner_iri: String,
    callee_name: String,
    callee_expr: String,
}

#[derive(Debug, Clone)]
struct GraphBuildResult {
    nquads: String,
    query_cache: CodeGraphQueryCache,
    code_namespace: String,
    parsed_file_count: usize,
    symbol_count: usize,
    occurrence_count: usize,
    import_edge_count: usize,
    call_edge_count: usize,
    resolved_call_edge_count: usize,
    language_counts: BTreeMap<String, usize>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct ImportResolution {
    kind: String,
    candidate_paths: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct ImportResolverContext {
    indexed_paths: BTreeSet<String>,
    rust_crate_roots: BTreeMap<String, String>,
    /// tsconfig-derived path aliases keyed by repo-relative tsconfig directory
    /// (`""` for a root-level tsconfig). Each list is sorted longest
    /// `alias_prefix` first so most-specific patterns win.
    ts_alias_tables: BTreeMap<String, Vec<TsPathAlias>>,
}

/// One normalized `compilerOptions.paths` mapping from a `tsconfig.json`.
///
/// v1 scope: exact patterns (no `*`) and single-trailing-`*` patterns only.
/// Patterns with a leading/inner `*` or multiple stars are ignored, as are
/// wildcard targets that do not themselves end in a single trailing `*`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TsPathAlias {
    /// Specifier prefix (`@/` for pattern `@/*`), or the full specifier for
    /// exact patterns.
    alias_prefix: String,
    /// True when the tsconfig pattern ended in a trailing `*`.
    wildcard: bool,
    /// Repo-relative target prefixes (wildcard) or full target paths (exact),
    /// kept in tsconfig declaration order; each is tried and all hits are
    /// collected.
    targets: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CargoManifestToml {
    package: Option<CargoPackageToml>,
    lib: Option<CargoLibToml>,
}

#[derive(Debug, Deserialize)]
struct CargoPackageToml {
    name: String,
}

#[derive(Debug, Deserialize)]
struct CargoLibToml {
    name: Option<String>,
    path: Option<String>,
}

pub fn default_code_graph_output_dir(root: &Path) -> PathBuf {
    root.join(".leio-code")
        .join("exports")
        .join("code-graph-v1")
}

pub fn default_code_graph_manifest_path(output_dir: &Path) -> PathBuf {
    output_dir.join("manifest.json")
}

pub fn default_code_graph_cache_path(output_dir: &Path) -> PathBuf {
    output_dir.join("query-cache.json")
}

pub fn code_graph_source_fingerprint(index: &RepoIndex) -> String {
    let mut supported_files = index
        .files
        .iter()
        .filter(|file| supports_code_graph(file.language))
        .collect::<Vec<_>>();
    supported_files.sort_by(|left, right| left.path.cmp(&right.path));

    let mut hasher = Sha256::new();
    for file in supported_files {
        hasher.update(file.path.as_bytes());
        hasher.update([0]);
        hasher.update(file.language.as_str().as_bytes());
        hasher.update([0]);
        hasher.update(file.bytes.to_string().as_bytes());
        hasher.update([0]);
        hasher.update(file.modified_unix_ms.to_string().as_bytes());
        hasher.update([0xff]);
    }
    format!("{:x}", hasher.finalize())
}

pub fn code_graph_refresh_reason(index: &RepoIndex, output_dir: &Path) -> Option<String> {
    let manifest_path = default_code_graph_manifest_path(output_dir);
    let raw = fs::read_to_string(&manifest_path).ok()?;
    let manifest: CodeGraphManifest = match serde_json::from_str(&raw) {
        Ok(manifest) => manifest,
        Err(error) => {
            return Some(format!(
                "code graph manifest unreadable at {}: {}",
                manifest_path.display(),
                error
            ));
        }
    };

    if manifest.version != CODE_GRAPH_VERSION {
        return Some(format!(
            "code graph manifest version mismatch (expected {}, found {})",
            CODE_GRAPH_VERSION, manifest.version
        ));
    }

    if manifest.repo_root != index.root {
        return Some(format!(
            "code graph manifest repo root mismatch (expected {}, found {})",
            index.root, manifest.repo_root
        ));
    }

    let expected_fingerprint = code_graph_source_fingerprint(index);
    if manifest.source_fingerprint != expected_fingerprint {
        return Some("code graph manifest is stale for the current indexed source set".to_string());
    }

    None
}

pub fn export_code_graph(
    index: &RepoIndex,
    root: &Path,
    output_dir: &Path,
) -> Result<QueryEnvelope> {
    let started = Instant::now();
    let revision = detect_git_revision(root).unwrap_or_else(|| "workspace".to_string());
    let repo_slug = slug_component(
        root.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("repo"),
    );
    let graph_iri = format!(
        "urn:leio:graph:code:{repo_slug}:{}",
        slug_component(&revision)
    );
    let build = build_graph(index, root, &repo_slug, &revision, &graph_iri)?;

    fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    let graph_path = output_dir.join("graph.nq");
    let manifest_path = default_code_graph_manifest_path(output_dir);
    let query_cache_path = default_code_graph_cache_path(output_dir);
    fs::write(&graph_path, build.nquads.as_bytes())
        .with_context(|| format!("failed to write {}", graph_path.display()))?;
    let query_cache_raw = serde_json::to_string_pretty(&build.query_cache)
        .context("failed to serialize code graph query cache")?;
    fs::write(&query_cache_path, query_cache_raw)
        .with_context(|| format!("failed to write {}", query_cache_path.display()))?;

    let manifest = CodeGraphManifest {
        version: CODE_GRAPH_VERSION,
        repo_root: index.root.clone(),
        indexed_at: index.indexed_at.clone(),
        exported_at: OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .context("failed to format export timestamp")?,
        revision: revision.clone(),
        graph_iri: graph_iri.clone(),
        graph_path: relative_to(output_dir, &graph_path),
        query_cache_path: relative_to(output_dir, &query_cache_path),
        parsed_file_count: build.parsed_file_count,
        symbol_count: build.symbol_count,
        occurrence_count: build.occurrence_count,
        import_edge_count: build.import_edge_count,
        call_edge_count: build.call_edge_count,
        resolved_call_edge_count: build.resolved_call_edge_count,
        language_counts: build.language_counts.clone(),
        parse_warning_count: build.warnings.len(),
        source_fingerprint: code_graph_source_fingerprint(index),
        code_namespace: build.code_namespace.clone(),
    };
    let manifest_raw = serde_json::to_string_pretty(&manifest)
        .context("failed to serialize code graph manifest")?;
    fs::write(&manifest_path, manifest_raw)
        .with_context(|| format!("failed to write {}", manifest_path.display()))?;

    Ok(QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: format!(
            "code-graph-{}",
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ),
        kind: "export".to_string(),
        summary: format!(
            "exported code graph with {} files, {} symbols, {} calls ({} resolved) -> {}",
            manifest.parsed_file_count,
            manifest.symbol_count,
            manifest.call_edge_count,
            manifest.resolved_call_edge_count,
            output_dir.display()
        ),
        confidence: 0.93,
        entities: vec![json!({
            "version": CODE_GRAPH_VERSION,
            "repo_root": index.root,
            "revision": revision,
            "graph_iri": graph_iri,
            "output_dir": output_dir.display().to_string(),
            "manifest": manifest_path.display().to_string(),
            "graph_path": graph_path.display().to_string(),
            "query_cache_path": query_cache_path.display().to_string(),
            "parsed_files": manifest.parsed_file_count,
            "symbols": manifest.symbol_count,
            "occurrences": manifest.occurrence_count,
            "imports": manifest.import_edge_count,
            "calls": manifest.call_edge_count,
            "resolved_calls": manifest.resolved_call_edge_count,
            "language_counts": manifest.language_counts,
        })],
        evidence: vec![
            EvidenceItem {
                kind: "artifact".to_string(),
                path: manifest_path.display().to_string(),
                line: None,
                detail: "code graph manifest".to_string(),
            },
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
        ],
        warnings: build.warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    })
}

fn build_graph(
    index: &RepoIndex,
    root: &Path,
    repo_slug: &str,
    revision: &str,
    graph_iri: &str,
) -> Result<GraphBuildResult> {
    let resolver = build_import_resolver_context(index, root);
    let mut parsed_files = Vec::new();
    let mut warnings = Vec::new();
    let mut language_counts = BTreeMap::new();

    for file in &index.files {
        if !supports_code_graph(file.language) {
            continue;
        }

        let source_path = root.join(&file.path);
        let source = match fs::read_to_string(&source_path) {
            Ok(source) => source,
            Err(error) => {
                warnings.push(format!(
                    "failed to read {} for code graph export: {}",
                    file.path, error
                ));
                continue;
            }
        };

        match parse_file_graph(&file.path, file.language, &source, repo_slug, revision) {
            Ok(parsed) => {
                *language_counts
                    .entry(file.language.as_str().to_string())
                    .or_insert(0usize) += 1;
                parsed_files.push(parsed);
            }
            Err(error) => warnings.push(format!("failed to parse {}: {}", file.path, error)),
        }
    }

    let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
    let mut owner_occurrence_to_symbol = HashMap::new();
    let mut symbols = BTreeMap::new();
    let mut symbol_lookup_name = BTreeMap::new();
    let mut symbol_lookup_qual_name = BTreeMap::new();
    let mut files = BTreeMap::new();
    let mut file_lookup_path = BTreeMap::new();
    let mut file_imports = BTreeMap::new();
    let mut file_import_details = BTreeMap::new();
    let mut importers_by_raw = BTreeMap::new();
    let mut importers_by_target_path = BTreeMap::new();
    let mut file_symbols = BTreeMap::new();
    for file in &parsed_files {
        files.insert(
            file.file_iri.clone(),
            CachedGraphFile {
                iri: file.file_iri.clone(),
                path: file.path.clone(),
                language: file.language.as_str().to_string(),
            },
        );
        file_lookup_path
            .entry(file.path.clone())
            .or_insert_with(Vec::new)
            .push(file.file_iri.clone());
        for definition in &file.definitions {
            owner_occurrence_to_symbol.insert(
                definition.occurrence_iri.clone(),
                definition.stable_iri.clone(),
            );
            symbols.insert(
                definition.stable_iri.clone(),
                CachedGraphSymbol {
                    iri: definition.stable_iri.clone(),
                    name: definition.name.clone(),
                    qual_name: definition.qual_name.clone(),
                    kind: definition.kind.as_str().to_string(),
                    path: file.path.clone(),
                    line: Some(definition.start_line),
                },
            );
            symbol_lookup_name
                .entry(definition.name.clone())
                .or_insert_with(Vec::new)
                .push(definition.stable_iri.clone());
            symbol_lookup_qual_name
                .entry(definition.qual_name.clone())
                .or_insert_with(Vec::new)
                .push(definition.stable_iri.clone());
            push_unique_neighbor(
                &mut file_symbols,
                &file.file_iri,
                CachedGraphNeighbor {
                    iri: definition.stable_iri.clone(),
                    name: definition.name.clone(),
                    qual_name: definition.qual_name.clone(),
                    kind: definition.kind.as_str().to_string(),
                    path: file.path.clone(),
                    line: Some(definition.start_line),
                },
            );
            if is_callable_kind(definition.kind) {
                by_name
                    .entry(definition.name.clone())
                    .or_default()
                    .push(definition.stable_iri.clone());
            }
        }
    }

    // Resolved once per build call; every emitted term reuses this string.
    let code_ns = crate::config::code_rdf_namespace(root);
    let mut quads = String::new();
    let mut symbol_count = 0usize;
    let mut occurrence_count = 0usize;
    let mut import_edge_count = 0usize;
    let mut call_edge_count = 0usize;
    let mut resolved_call_edge_count = 0usize;
    let mut callers_by_symbol = BTreeMap::new();
    let mut callees_by_symbol = BTreeMap::new();
    let mut callsites_by_symbol = BTreeMap::new();

    // Env/redis occurrences live on `index.files[]`, but this loop iterates
    // `parsed_files` (which carry only definitions/imports/calls). Precompute a
    // path -> occurrences join once so the per-file emission is a cheap lookup.
    // Routes are not on `RepoIndexFile`; they are aggregated separately, so we
    // group them by their declaring file path the same way.
    let env_by_path: BTreeMap<&str, &[EnvVarOccurrence]> = index
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.env_vars.as_slice()))
        .collect();
    let redis_by_path: BTreeMap<&str, &[RedisKeyOccurrence]> = index
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.redis_keys.as_slice()))
        .collect();
    let mut routes_by_path: BTreeMap<String, Vec<crate::query::AggregatedApiRoute>> =
        BTreeMap::new();
    for route in crate::query::collect_api_routes(index) {
        routes_by_path
            .entry(route.file_path.clone())
            .or_default()
            .push(route);
    }

    for file in &parsed_files {
        write_iri_quad(
            &mut quads,
            &file.file_iri,
            RDF_TYPE,
            &format!("{code_ns}File"),
            graph_iri,
        );
        write_literal_quad(
            &mut quads,
            &file.file_iri,
            &format!("{code_ns}path"),
            &file.path,
            graph_iri,
        );
        write_literal_quad(
            &mut quads,
            &file.file_iri,
            &format!("{code_ns}language"),
            file.language.as_str(),
            graph_iri,
        );

        // Env/redis/route incidences for this file. These make the induced
        // invariants SPARQL-native: an access-typed literal predicate for
        // direct querying, plus a first-class attribute node so the incidence
        // is itself an entity (parallel to a formal-context attribute).
        //
        // DOCUMENTED SUBSET DIVERGENCE (not a bug): graph.nq only covers
        // `supports_code_graph()` languages, while `export::build_formal_context`
        // covers every indexed file. So these triples are a SUBSET of the full
        // incidence set. The induced-invariants doctor mines from this same
        // graph-covered set to keep its mining domain equal to its validation
        // domain (no spurious subset drift).
        if let Some(occurrences) = env_by_path.get(file.path.as_str()) {
            for env in *occurrences {
                let attr_iri = mint_env_attr_iri(env.access, &env.name);
                write_literal_quad(
                    &mut quads,
                    &file.file_iri,
                    &format!("{code_ns}{}", env_access_predicate(env.access)),
                    &env.name,
                    graph_iri,
                );
                write_iri_quad(
                    &mut quads,
                    &file.file_iri,
                    &format!("{code_ns}usesEnvAttr"),
                    &attr_iri,
                    graph_iri,
                );
                write_iri_quad(
                    &mut quads,
                    &attr_iri,
                    RDF_TYPE,
                    &format!("{code_ns}EnvAttr"),
                    graph_iri,
                );
                write_literal_quad(
                    &mut quads,
                    &attr_iri,
                    &format!("{code_ns}envName"),
                    &env.name,
                    graph_iri,
                );
                write_literal_quad(
                    &mut quads,
                    &attr_iri,
                    &format!("{code_ns}access"),
                    env.access.as_str(),
                    graph_iri,
                );
            }
        }
        if let Some(occurrences) = redis_by_path.get(file.path.as_str()) {
            for redis in *occurrences {
                let attr_iri = mint_redis_attr_iri(redis.access, &redis.key);
                write_literal_quad(
                    &mut quads,
                    &file.file_iri,
                    &format!("{code_ns}{}", redis_access_predicate(redis.access)),
                    &redis.key,
                    graph_iri,
                );
                write_iri_quad(
                    &mut quads,
                    &file.file_iri,
                    &format!("{code_ns}usesRedisAttr"),
                    &attr_iri,
                    graph_iri,
                );
                write_iri_quad(
                    &mut quads,
                    &attr_iri,
                    RDF_TYPE,
                    &format!("{code_ns}RedisAttr"),
                    graph_iri,
                );
                write_literal_quad(
                    &mut quads,
                    &attr_iri,
                    &format!("{code_ns}redisKey"),
                    &redis.key,
                    graph_iri,
                );
                write_literal_quad(
                    &mut quads,
                    &attr_iri,
                    &format!("{code_ns}access"),
                    redis.access.as_str(),
                    graph_iri,
                );
            }
        }
        if let Some(routes) = routes_by_path.get(&file.path) {
            for route in routes {
                let attr_iri = mint_route_attr_iri(&route.full_path);
                write_iri_quad(
                    &mut quads,
                    &file.file_iri,
                    &format!("{code_ns}declaresRoute"),
                    &attr_iri,
                    graph_iri,
                );
                write_iri_quad(
                    &mut quads,
                    &attr_iri,
                    RDF_TYPE,
                    &format!("{code_ns}RouteAttr"),
                    graph_iri,
                );
                write_literal_quad(
                    &mut quads,
                    &attr_iri,
                    &format!("{code_ns}routePath"),
                    &route.full_path,
                    graph_iri,
                );
                write_literal_quad(
                    &mut quads,
                    &attr_iri,
                    &format!("{code_ns}routeFamily"),
                    &route.route_family,
                    graph_iri,
                );
                write_literal_quad(
                    &mut quads,
                    &attr_iri,
                    &format!("{code_ns}mountStatus"),
                    &route.mount_status,
                    graph_iri,
                );
            }
        }

        for definition in &file.definitions {
            symbol_count += 1;
            occurrence_count += 1;

            write_iri_quad(
                &mut quads,
                &definition.stable_iri,
                RDF_TYPE,
                &format!("{code_ns}Symbol"),
                graph_iri,
            );
            write_literal_quad(
                &mut quads,
                &definition.stable_iri,
                &format!("{code_ns}name"),
                &definition.name,
                graph_iri,
            );
            write_literal_quad(
                &mut quads,
                &definition.stable_iri,
                &format!("{code_ns}qualName"),
                &definition.qual_name,
                graph_iri,
            );
            write_literal_quad(
                &mut quads,
                &definition.stable_iri,
                &format!("{code_ns}symbolKind"),
                definition.kind.as_str(),
                graph_iri,
            );
            write_literal_quad(
                &mut quads,
                &definition.stable_iri,
                &format!("{code_ns}language"),
                file.language.as_str(),
                graph_iri,
            );
            write_iri_quad(
                &mut quads,
                &file.file_iri,
                &format!("{code_ns}containsSymbol"),
                &definition.stable_iri,
                graph_iri,
            );

            write_iri_quad(
                &mut quads,
                &definition.occurrence_iri,
                RDF_TYPE,
                &format!("{code_ns}SymbolOccurrence"),
                graph_iri,
            );
            write_iri_quad(
                &mut quads,
                &definition.occurrence_iri,
                PROV_SPECIALIZATION_OF,
                &definition.stable_iri,
                graph_iri,
            );
            write_iri_quad(
                &mut quads,
                &definition.stable_iri,
                &format!("{code_ns}hasOccurrence"),
                &definition.occurrence_iri,
                graph_iri,
            );
            write_iri_quad(
                &mut quads,
                &definition.occurrence_iri,
                &format!("{code_ns}definedIn"),
                &file.file_iri,
                graph_iri,
            );
            write_literal_quad(
                &mut quads,
                &definition.occurrence_iri,
                &format!("{code_ns}revision"),
                revision,
                graph_iri,
            );
            write_literal_quad(
                &mut quads,
                &definition.occurrence_iri,
                &format!("{code_ns}startLine"),
                &definition.start_line.to_string(),
                graph_iri,
            );
            write_literal_quad(
                &mut quads,
                &definition.occurrence_iri,
                &format!("{code_ns}endLine"),
                &definition.end_line.to_string(),
                graph_iri,
            );
        }

        for import in &file.imports {
            let resolution = resolve_import_paths(
                &resolver,
                &file.path,
                file.language,
                &import.module_specifiers,
            );
            let candidate_symbols = resolve_import_symbols(
                &file_lookup_path,
                &file_symbols,
                &resolution.candidate_paths,
                &import.imported_names,
            );
            import_edge_count += 1;
            write_literal_quad(
                &mut quads,
                &import.owner_iri,
                &format!("{code_ns}importsPath"),
                &import.raw,
                graph_iri,
            );
            push_unique_string(&mut file_imports, &file.file_iri, import.raw.clone());
            push_unique_import(
                &mut file_import_details,
                &file.file_iri,
                CachedGraphImport {
                    raw: import.raw.clone(),
                    syntax_kind: import.syntax_kind.clone(),
                    module_specifiers: import.module_specifiers.clone(),
                    imported_names: import.imported_names.clone(),
                    line: import.line,
                    resolution_kind: resolution.kind.clone(),
                    candidate_paths: resolution.candidate_paths.clone(),
                    candidate_symbols,
                },
            );
            let importer = CachedGraphImporter {
                raw_import: import.raw.clone(),
                file_iri: file.file_iri.clone(),
                path: file.path.clone(),
                language: file.language.as_str().to_string(),
                line: import.line,
            };
            push_unique_importer(&mut importers_by_raw, &import.raw, importer.clone());
            // Populate importers for both "resolved" (1 candidate) and
            // "ambiguous" (>1 candidate) resolutions. The "ambiguous" case
            // arises from `from pkg import sub` where both `pkg/__init__.py`
            // and `pkg/sub.py` are valid targets — Python actually imports
            // both, so both files should be credited with an importer edge.
            if matches!(resolution.kind.as_str(), "resolved" | "ambiguous") {
                for candidate_path in &resolution.candidate_paths {
                    push_unique_importer(
                        &mut importers_by_target_path,
                        candidate_path,
                        importer.clone(),
                    );
                }
            }
        }

        for call in &file.calls {
            call_edge_count += 1;
            write_literal_quad(
                &mut quads,
                &call.owner_iri,
                &format!("{code_ns}callsName"),
                &call.callee_name,
                graph_iri,
            );
            write_literal_quad(
                &mut quads,
                &call.owner_iri,
                &format!("{code_ns}callsExpr"),
                &call.callee_expr,
                graph_iri,
            );
            if let Some(targets) = by_name.get(&call.callee_name)
                && targets.len() == 1
            {
                resolved_call_edge_count += 1;
                write_iri_quad(
                    &mut quads,
                    &call.owner_iri,
                    &format!("{code_ns}calls"),
                    &targets[0],
                    graph_iri,
                );

                let target_iri = &targets[0];
                if let Some(target_symbol) = symbols.get(target_iri) {
                    if let Some(owner_symbol_iri) = owner_occurrence_to_symbol.get(&call.owner_iri)
                    {
                        if let Some(owner_symbol) = symbols.get(owner_symbol_iri) {
                            push_unique_neighbor(
                                &mut callers_by_symbol,
                                target_iri,
                                CachedGraphNeighbor {
                                    iri: owner_symbol.iri.clone(),
                                    name: owner_symbol.name.clone(),
                                    qual_name: owner_symbol.qual_name.clone(),
                                    kind: owner_symbol.kind.clone(),
                                    path: owner_symbol.path.clone(),
                                    line: owner_symbol.line,
                                },
                            );
                            push_unique_neighbor(
                                &mut callees_by_symbol,
                                owner_symbol_iri,
                                CachedGraphNeighbor {
                                    iri: target_symbol.iri.clone(),
                                    name: target_symbol.name.clone(),
                                    qual_name: target_symbol.qual_name.clone(),
                                    kind: target_symbol.kind.clone(),
                                    path: target_symbol.path.clone(),
                                    line: target_symbol.line,
                                },
                            );
                            push_unique_callsite(
                                &mut callsites_by_symbol,
                                target_iri,
                                CachedGraphCallsite {
                                    owner_iri: call.owner_iri.clone(),
                                    owner_kind: owner_symbol.kind.clone(),
                                    owner_name: owner_symbol.name.clone(),
                                    owner_qual_name: owner_symbol.qual_name.clone(),
                                    path: owner_symbol.path.clone(),
                                    line: owner_symbol.line,
                                    expr: call.callee_expr.clone(),
                                },
                            );
                        }
                    } else if let Some(owner_file) = files.get(&call.owner_iri) {
                        push_unique_neighbor(
                            &mut callers_by_symbol,
                            target_iri,
                            CachedGraphNeighbor {
                                iri: owner_file.iri.clone(),
                                name: owner_file.path.clone(),
                                qual_name: owner_file.path.clone(),
                                kind: "file".to_string(),
                                path: owner_file.path.clone(),
                                line: None,
                            },
                        );
                        push_unique_callsite(
                            &mut callsites_by_symbol,
                            target_iri,
                            CachedGraphCallsite {
                                owner_iri: owner_file.iri.clone(),
                                owner_kind: "file".to_string(),
                                owner_name: owner_file.path.clone(),
                                owner_qual_name: owner_file.path.clone(),
                                path: owner_file.path.clone(),
                                line: None,
                                expr: call.callee_expr.clone(),
                            },
                        );
                    }
                }
            }
        }
    }

    Ok(GraphBuildResult {
        nquads: quads,
        code_namespace: code_ns,
        query_cache: CodeGraphQueryCache {
            version: CODE_GRAPH_VERSION,
            query_cache_version: QUERY_CACHE_VERSION,
            revision: revision.to_string(),
            graph_iri: graph_iri.to_string(),
            symbols,
            symbol_lookup_name,
            symbol_lookup_qual_name,
            files,
            file_lookup_path,
            file_imports,
            file_import_details,
            importers_by_raw,
            importers_by_target_path,
            callers_by_symbol,
            callees_by_symbol,
            callsites_by_symbol,
            file_symbols,
        },
        parsed_file_count: parsed_files.len(),
        symbol_count,
        occurrence_count,
        import_edge_count,
        call_edge_count,
        resolved_call_edge_count,
        language_counts,
        warnings,
    })
}

fn push_unique_neighbor(
    map: &mut BTreeMap<String, Vec<CachedGraphNeighbor>>,
    key: &str,
    neighbor: CachedGraphNeighbor,
) {
    let bucket = map.entry(key.to_string()).or_default();
    if bucket.iter().any(|existing| {
        existing.iri == neighbor.iri
            && existing.path == neighbor.path
            && existing.line == neighbor.line
            && existing.kind == neighbor.kind
    }) {
        return;
    }
    bucket.push(neighbor);
}

fn push_unique_callsite(
    map: &mut BTreeMap<String, Vec<CachedGraphCallsite>>,
    key: &str,
    callsite: CachedGraphCallsite,
) {
    let bucket = map.entry(key.to_string()).or_default();
    if bucket.iter().any(|existing| {
        existing.owner_iri == callsite.owner_iri
            && existing.path == callsite.path
            && existing.line == callsite.line
            && existing.expr == callsite.expr
    }) {
        return;
    }
    bucket.push(callsite);
}

fn push_unique_string(map: &mut BTreeMap<String, Vec<String>>, key: &str, value: String) {
    let bucket = map.entry(key.to_string()).or_default();
    if bucket.iter().any(|existing| existing == &value) {
        return;
    }
    bucket.push(value);
}

fn push_unique_import(
    map: &mut BTreeMap<String, Vec<CachedGraphImport>>,
    key: &str,
    value: CachedGraphImport,
) {
    let bucket = map.entry(key.to_string()).or_default();
    if bucket.iter().any(|existing| existing.raw == value.raw) {
        return;
    }
    bucket.push(value);
}

fn push_unique_importer(
    map: &mut BTreeMap<String, Vec<CachedGraphImporter>>,
    key: &str,
    value: CachedGraphImporter,
) {
    let bucket = map.entry(key.to_string()).or_default();
    if bucket.iter().any(|existing| {
        existing.file_iri == value.file_iri
            && existing.raw_import == value.raw_import
            && existing.line == value.line
    }) {
        return;
    }
    bucket.push(value);
}

fn parse_file_graph(
    path: &str,
    language: SourceLanguage,
    source: &str,
    repo_slug: &str,
    revision: &str,
) -> Result<ParsedFileGraph> {
    if language == SourceLanguage::Rdf {
        return Ok(parse_rdf_file_graph(path, source, repo_slug, revision));
    }
    if language == SourceLanguage::Sql {
        return Ok(parse_sql_file_graph(path, source, repo_slug, revision));
    }
    if language == SourceLanguage::Kotlin {
        return Ok(parse_kotlin_file_graph(path, source, repo_slug, revision));
    }
    let ts_language = ts_language(language)
        .with_context(|| format!("language {} is not supported", language.as_str()))?;

    let mut parser = Parser::new();
    parser.set_language(&ts_language).with_context(|| {
        format!(
            "failed to set tree-sitter language for {}",
            language.as_str()
        )
    })?;
    let parser_source = crate::parser_support::parser_source(language, source);
    let tree = parser
        .parse(parser_source.as_ref(), None)
        .with_context(|| format!("tree-sitter returned no parse tree for {}", path))?;

    let file_iri = mint_file_iri(repo_slug, path);
    let mut definitions = Vec::new();
    let mut imports = Vec::new();
    let mut calls = Vec::new();
    let mut scopes = Vec::new();
    let mut owners = Vec::new();
    let root_owner = file_iri.clone();

    walk_file_graph(
        tree.root_node(),
        parser_source.as_ref(),
        language,
        path,
        repo_slug,
        revision,
        &root_owner,
        &mut scopes,
        &mut owners,
        &mut definitions,
        &mut imports,
        &mut calls,
    );

    Ok(ParsedFileGraph {
        path: path.to_string(),
        language,
        file_iri,
        definitions,
        imports,
        calls,
    })
}

fn parse_sql_file_graph(
    path: &str,
    source: &str,
    repo_slug: &str,
    revision: &str,
) -> ParsedFileGraph {
    let file_iri = mint_file_iri(repo_slug, path);
    let pattern = Regex::new(r"(?i)\bcreate\s+(table|view|procedure|function)\s+(?:if\s+not\s+exists\s+)?([A-Za-z_][A-Za-z0-9_]*)").expect("sql graph declarations");
    let definitions = pattern
        .captures_iter(source)
        .filter_map(|capture| {
            let name = capture.get(2)?.as_str().to_string();
            let line = source[..capture.get(0)?.start()]
                .bytes()
                .filter(|b| *b == b'\n')
                .count()
                + 1;
            let stable_iri = mint_symbol_iri(
                repo_slug,
                path,
                SourceLanguage::Sql,
                SymbolKind::Module,
                &name,
            );
            Some(Definition {
                occurrence_iri: mint_occurrence_iri(&stable_iri, revision, line, line),
                stable_iri,
                name: name.clone(),
                qual_name: name,
                kind: SymbolKind::Module,
                start_line: line,
                end_line: line,
            })
        })
        .collect();
    ParsedFileGraph {
        path: path.to_string(),
        language: SourceLanguage::Sql,
        file_iri,
        definitions,
        imports: Vec::new(),
        calls: Vec::new(),
    }
}

fn parse_kotlin_file_graph(
    path: &str,
    source: &str,
    repo_slug: &str,
    revision: &str,
) -> ParsedFileGraph {
    let file_iri = mint_file_iri(repo_slug, path);
    let pattern = Regex::new(r"(?m)^\s*(?:public\s+|private\s+|internal\s+|open\s+|data\s+|sealed\s+|abstract\s+)*(fun|class|object|interface)\s+([A-Za-z_][A-Za-z0-9_]*)").expect("kotlin graph declarations");
    let definitions = pattern
        .captures_iter(source)
        .filter_map(|capture| {
            let name = capture.get(2)?.as_str().to_string();
            let kind = match capture.get(1)?.as_str() {
                "fun" => SymbolKind::Function,
                "interface" => SymbolKind::Interface,
                _ => SymbolKind::Class,
            };
            let line = source[..capture.get(0)?.start()]
                .bytes()
                .filter(|b| *b == b'\n')
                .count()
                + 1;
            let stable_iri = mint_symbol_iri(repo_slug, path, SourceLanguage::Kotlin, kind, &name);
            Some(Definition {
                occurrence_iri: mint_occurrence_iri(&stable_iri, revision, line, line),
                stable_iri,
                name: name.clone(),
                qual_name: name,
                kind,
                start_line: line,
                end_line: line,
            })
        })
        .collect();
    ParsedFileGraph {
        path: path.to_string(),
        language: SourceLanguage::Kotlin,
        file_iri,
        definitions,
        imports: Vec::new(),
        calls: Vec::new(),
    }
}

fn parse_rdf_file_graph(
    path: &str,
    source: &str,
    repo_slug: &str,
    revision: &str,
) -> ParsedFileGraph {
    let extracted = crate::ontology::extract_ontology(path, source);
    let file_iri = mint_file_iri(repo_slug, path);
    let language = SourceLanguage::Rdf;
    let mut definitions = Vec::new();
    for symbol in extracted.symbols {
        let qual_name = symbol
            .qual_name
            .clone()
            .unwrap_or_else(|| symbol.name.clone());
        let stable_iri = mint_symbol_iri(repo_slug, path, language, symbol.kind, &qual_name);
        let occurrence_iri = mint_occurrence_iri(&stable_iri, revision, symbol.line, symbol.line);
        definitions.push(Definition {
            stable_iri,
            occurrence_iri,
            name: symbol.name,
            qual_name,
            kind: symbol.kind,
            start_line: symbol.line,
            end_line: symbol.line,
        });
    }
    let imports = extracted
        .imports
        .into_iter()
        .map(|imp| ImportEdge {
            owner_iri: file_iri.clone(),
            raw: format!("owl:imports <{}>", imp.iri),
            syntax_kind: "owl_imports".to_string(),
            module_specifiers: vec![imp.iri],
            imported_names: Vec::new(),
            line: Some(imp.line),
        })
        .collect();
    ParsedFileGraph {
        path: path.to_string(),
        language,
        file_iri,
        definitions,
        imports,
        calls: Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_file_graph(
    node: Node<'_>,
    source: &str,
    language: SourceLanguage,
    path: &str,
    repo_slug: &str,
    revision: &str,
    root_owner: &str,
    scopes: &mut Vec<String>,
    owners: &mut Vec<String>,
    definitions: &mut Vec<Definition>,
    imports: &mut Vec<ImportEdge>,
    calls: &mut Vec<CallEdge>,
) {
    if let Some((import_raw, syntax_kind, module_specifiers, imported_names, line)) =
        import_text(language, node, source)
    {
        imports.push(ImportEdge {
            owner_iri: root_owner.to_string(),
            raw: import_raw,
            syntax_kind,
            module_specifiers,
            imported_names,
            line,
        });
    }

    if let Some((import_raw, syntax_kind, module_specifiers, line)) =
        string_literal_import(language, node, source)
    {
        imports.push(ImportEdge {
            owner_iri: root_owner.to_string(),
            raw: import_raw,
            syntax_kind,
            module_specifiers,
            imported_names: Vec::new(),
            line,
        });
    }

    if let Some(call) = call_from_node(language, node, source) {
        let owner_iri = owners
            .last()
            .cloned()
            .unwrap_or_else(|| root_owner.to_string());
        calls.push(CallEdge {
            owner_iri,
            callee_name: call.0,
            callee_expr: call.1,
        });
    }

    if let Some((kind, name)) = definition_from_node(language, node, source) {
        let qual_name = if scopes.is_empty() {
            name.clone()
        } else {
            format!("{}::{}", scopes.join("::"), name)
        };
        let stable_iri = mint_symbol_iri(repo_slug, path, language, kind, &qual_name);
        let occurrence_iri = mint_occurrence_iri(
            &stable_iri,
            revision,
            node.start_position().row + 1,
            node.end_position().row + 1,
        );

        definitions.push(Definition {
            stable_iri: stable_iri.clone(),
            occurrence_iri,
            name: name.clone(),
            qual_name,
            kind,
            start_line: node.start_position().row + 1,
            end_line: node.end_position().row + 1,
        });

        scopes.push(name);
        owners.push(
            definitions
                .last()
                .map(|definition| definition.occurrence_iri.clone())
                .unwrap_or_else(|| root_owner.to_string()),
        );
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            walk_file_graph(
                child,
                source,
                language,
                path,
                repo_slug,
                revision,
                root_owner,
                scopes,
                owners,
                definitions,
                imports,
                calls,
            );
        }
        owners.pop();
        scopes.pop();
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_file_graph(
            child,
            source,
            language,
            path,
            repo_slug,
            revision,
            root_owner,
            scopes,
            owners,
            definitions,
            imports,
            calls,
        );
    }
}

fn definition_from_node(
    language: SourceLanguage,
    node: Node<'_>,
    source: &str,
) -> Option<(SymbolKind, String)> {
    let kind = symbol_kind(language, node.kind())?;
    let name_node = node.child_by_field_name("name").or_else(|| {
        if matches!(language, SourceLanguage::C | SourceLanguage::Cpp)
            && node.kind() == "function_definition"
        {
            let declarator = node.child_by_field_name("declarator")?;
            declarator
                .child_by_field_name("declarator")
                .or_else(|| declarator.child_by_field_name("name"))
        } else {
            None
        }
    })?;
    let name = name_node
        .utf8_text(source.as_bytes())
        .ok()?
        .trim()
        .to_string();
    if name.is_empty() {
        return None;
    }
    Some((kind, name))
}

type ImportText = (String, String, Vec<String>, Vec<String>, Option<usize>);

fn import_text(language: SourceLanguage, node: Node<'_>, source: &str) -> Option<ImportText> {
    let kind = node.kind();
    let is_import = match language {
        // Also capture `mod_item` so that `pub mod foo;` declarations in
        // mod.rs create import edges to the sibling file — otherwise every
        // Rust submodule looks orphaned to the graph.
        SourceLanguage::Rust => kind == "use_declaration" || kind == "mod_item",
        SourceLanguage::Python => matches!(kind, "import_statement" | "import_from_statement"),
        // Re-export barrels (`export { X } from "./x"`, `export * from "./x"`)
        // are imports for graph purposes: the barrel depends on the target
        // file. Without this edge every module reachable only through an
        // `index.ts` barrel looks orphaned, and dead-code reports its live
        // symbols as unused. Only `export ... from` counts — a local
        // `export function f()` has no source and imports nothing.
        SourceLanguage::JavaScript | SourceLanguage::TypeScript | SourceLanguage::Tsx => {
            kind == "import_statement"
                || (kind == "export_statement" && node.child_by_field_name("source").is_some())
        }
        SourceLanguage::CSharp | SourceLanguage::Razor => matches!(
            kind,
            "using_directive" | "using_static_directive" | "global_using_directive"
        ),
        SourceLanguage::Go => kind == "import_declaration",
        SourceLanguage::C | SourceLanguage::Cpp => kind == "preproc_include",
        SourceLanguage::Bash => false,
        SourceLanguage::Java => kind == "import_declaration",
        SourceLanguage::Kotlin => kind == "import_header",
        SourceLanguage::Swift => kind == "import_declaration",
        _ => false,
    };
    if !is_import {
        return None;
    }
    let raw = node.utf8_text(source.as_bytes()).ok()?.trim().to_string();
    if raw.is_empty() {
        return None;
    }
    Some((
        raw.clone(),
        kind.to_string(),
        import_module_specifiers(language, &raw),
        import_imported_names(language, &raw),
        Some(node.start_position().row + 1),
    ))
}

/// Module-spec-like string literals tucked inside dict/tuple values, JS/TS dynamic
/// `import("...")` calls, or any other non-statement context. We treat them as
/// implicit imports so files referenced exclusively through registries
/// (`{"agent_run_router": (".agent_run", "router")}`), or through dynamic
/// imports, do not look orphaned to downstream graph queries.
type StringImport = (String, String, Vec<String>, Option<usize>);

fn string_literal_import(
    language: SourceLanguage,
    node: Node<'_>,
    source: &str,
) -> Option<StringImport> {
    let kind = node.kind();
    let is_string = match language {
        SourceLanguage::Python => kind == "string",
        SourceLanguage::JavaScript | SourceLanguage::TypeScript | SourceLanguage::Tsx => {
            matches!(kind, "string" | "template_string")
        }
        _ => return None,
    };
    if !is_string {
        return None;
    }
    // Skip strings that are children of a real import/from/use statement —
    // the regular `import_text` path already records them. Without this guard
    // a normal `import { x } from "@/lib/auth"` would be double-counted.
    if string_is_inside_import_statement(language, node) {
        return None;
    }
    let raw_text = node.utf8_text(source.as_bytes()).ok()?;
    let inner = unwrap_string_literal(raw_text)?;
    let inner = inner.trim();
    if inner.is_empty() {
        return None;
    }
    let module_spec = match language {
        SourceLanguage::Python => python_module_spec_from_string(inner)?,
        SourceLanguage::JavaScript | SourceLanguage::TypeScript | SourceLanguage::Tsx => {
            ts_module_spec_from_string(inner)?
        }
        _ => return None,
    };
    Some((
        format!("string-import \"{}\"", module_spec),
        "string_literal_import".to_string(),
        vec![module_spec],
        Some(node.start_position().row + 1),
    ))
}

/// Walk up a tree-sitter node's ancestry, returning true when one of the
/// enclosing nodes is a real import-style statement (a Python `import` /
/// `from ... import`, a TS/JS `import_statement`, or a Rust `use_declaration`).
/// We use this to ensure normal import strings are not double-counted as
/// "string-literal imports" when the synthetic-import scan runs.
fn string_is_inside_import_statement(language: SourceLanguage, node: Node<'_>) -> bool {
    let mut cursor = node.parent();
    while let Some(parent) = cursor {
        match language {
            SourceLanguage::Python => {
                if matches!(parent.kind(), "import_statement" | "import_from_statement") {
                    return true;
                }
            }
            SourceLanguage::JavaScript | SourceLanguage::TypeScript | SourceLanguage::Tsx => {
                if matches!(parent.kind(), "import_statement" | "export_statement") {
                    return true;
                }
            }
            SourceLanguage::Rust if parent.kind() == "use_declaration" => {
                return true;
            }
            _ => {}
        }
        cursor = parent.parent();
    }
    false
}

/// Strip enclosing single, double, or backtick quotes (and any Python string
/// prefix like `r`, `b`, `rb`, `f`) from a tree-sitter string node. Returns
/// `None` if the literal is not a single-line, single-segment string.
fn unwrap_string_literal(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    // Strip a small set of Python string prefixes.
    let after_prefix = trimmed
        .trim_start_matches("rb")
        .trim_start_matches("Rb")
        .trim_start_matches("rB")
        .trim_start_matches("RB")
        .trim_start_matches("br")
        .trim_start_matches("Br")
        .trim_start_matches("bR")
        .trim_start_matches("BR")
        .trim_start_matches('r')
        .trim_start_matches('R')
        .trim_start_matches('b')
        .trim_start_matches('B')
        .trim_start_matches('u')
        .trim_start_matches('U');
    let bytes = after_prefix.as_bytes();
    if bytes.len() < 2 {
        return None;
    }
    // Reject triple-quoted strings to avoid false positives on docstrings.
    if after_prefix.starts_with("\"\"\"") || after_prefix.starts_with("'''") {
        return None;
    }
    let opening = bytes[0];
    if opening != b'"' && opening != b'\'' && opening != b'`' {
        return None;
    }
    let closing = bytes[bytes.len() - 1];
    if closing != opening {
        return None;
    }
    Some(&after_prefix[1..after_prefix.len() - 1])
}

/// Returns the module spec if `inner` looks like a Python relative module path
/// (`.module`, `..pkg.sub`). Absolute and non-module strings are intentionally
/// rejected — capturing every absolute string would create too many false
/// positives. Registries that target absolute modules (like a Celery beat task
/// list) typically ALSO have a real `from ... import ...` somewhere, so the
/// graph will still resolve them.
fn python_module_spec_from_string(inner: &str) -> Option<String> {
    if !inner.starts_with('.') {
        return None;
    }
    if !looks_like_python_module_spec(inner) {
        return None;
    }
    Some(inner.to_string())
}

pub(crate) fn looks_like_python_module_spec(spec: &str) -> bool {
    // Allowed shape: a leading run of `.`, followed by zero or more `name(.name)*`
    // segments using identifier-safe characters. Strings that contain spaces,
    // slashes, file extensions, or non-identifier punctuation are rejected.
    let mut chars = spec.chars().peekable();
    while let Some('.') = chars.peek() {
        chars.next();
    }
    let rest: String = chars.collect();
    if rest.is_empty() {
        // Bare `.` / `..` — refuse, no useful target.
        return false;
    }
    for segment in rest.split('.') {
        if segment.is_empty() {
            return false;
        }
        let mut ch = segment.chars();
        let first = match ch.next() {
            Some(c) => c,
            None => return false,
        };
        if !(first.is_ascii_alphabetic() || first == '_') {
            return false;
        }
        for c in ch {
            if !(c.is_ascii_alphanumeric() || c == '_') {
                return false;
            }
        }
    }
    true
}

/// Returns the path-like spec if `inner` looks like a TypeScript / JavaScript
/// module path that the existing TS resolver knows how to handle (relative `./`,
/// `../`, or an `@/` alias). Bare specifiers like `react` are rejected to keep
/// noise low.
fn ts_module_spec_from_string(inner: &str) -> Option<String> {
    if inner.starts_with("./")
        || inner.starts_with("../")
        || inner.starts_with("@/")
        || inner.starts_with("@jai/")
        || inner.starts_with("@contracts/")
    {
        // Allow paths but reject anything with whitespace / illegal characters.
        if inner.chars().any(|ch| ch.is_whitespace()) {
            return None;
        }
        return Some(inner.to_string());
    }
    None
}

fn import_module_specifiers(language: SourceLanguage, raw: &str) -> Vec<String> {
    match language {
        SourceLanguage::Rust => rust_import_module_specifiers(raw),
        SourceLanguage::Python => python_import_module_specifiers(raw),
        SourceLanguage::JavaScript | SourceLanguage::TypeScript | SourceLanguage::Tsx => {
            js_import_module_specifiers(raw)
        }
        SourceLanguage::CSharp | SourceLanguage::Razor => csharp_import_module_specifiers(raw),
        SourceLanguage::Go => go_import_module_specifiers(raw),
        SourceLanguage::C | SourceLanguage::Cpp => c_import_module_specifiers(raw),
        SourceLanguage::Java | SourceLanguage::Kotlin | SourceLanguage::Swift => {
            vec![
                raw.trim()
                    .trim_end_matches(';')
                    .trim_start_matches("import ")
                    .trim()
                    .to_string(),
            ]
        }
        _ => Vec::new(),
    }
}

fn rust_import_module_specifiers(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();

    // Handle `mod foo;` / `pub mod foo;` / `pub(crate) mod foo;` declarations.
    // A bare `mod` item makes the sibling file part of the module tree; return
    // `self::foo` so the Rust resolver maps it to the actual file on disk.
    let mod_rest = trimmed
        .strip_prefix("pub(crate) mod ")
        .or_else(|| trimmed.strip_prefix("pub(super) mod "))
        .or_else(|| trimmed.strip_prefix("pub mod "))
        .or_else(|| trimmed.strip_prefix("mod "));
    if let Some(rest) = mod_rest {
        let name = rest.trim_end_matches(';').trim();
        if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return vec![format!("self::{name}")];
        }
        return Vec::new();
    }

    if !trimmed.starts_with("use ") {
        return Vec::new();
    }
    let mut module = trimmed
        .trim_start_matches("use ")
        .trim_end_matches(';')
        .trim()
        .to_string();
    if let Some((prefix, _)) = module.split_once(" as ") {
        module = prefix.trim().to_string();
    }
    if module.is_empty() {
        return Vec::new();
    }
    if let Some((prefix, suffix)) = module.split_once("::{") {
        let prefix = prefix.trim();
        let inner = suffix.trim_end_matches('}').trim();
        let mut out = Vec::new();
        for item in inner.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let item = item.split(" as ").next().unwrap_or(item).trim();
            if item == "self" {
                out.push(prefix.to_string());
            } else {
                out.push(format!("{prefix}::{item}"));
            }
        }
        if !out.is_empty() {
            out.sort();
            out.dedup();
            return out;
        }
    }
    vec![module]
}

fn python_import_module_specifiers(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("from ") {
        if let Some((module, names_part)) = rest.split_once(" import ") {
            let module = module.trim();
            if module.is_empty() {
                return Vec::new();
            }
            // Primary specifier: the module path itself.
            let mut out = vec![module.to_string()];
            // Also emit `module.name` for each imported name that looks like a
            // bare identifier — Python treats `from pkg import sub` as a
            // submodule import when `pkg/sub.py` exists. Without this, files
            // reachable only via `from pkg import sub` look orphaned.
            let names_part = names_part
                .trim()
                .trim_start_matches('(')
                .trim_end_matches(')');
            for name in names_part.split(',') {
                let name = name.trim().split(" as ").next().unwrap_or("").trim();
                if !name.is_empty()
                    && name != "*"
                    && name.chars().all(|c| c.is_alphanumeric() || c == '_')
                {
                    out.push(format!("{module}.{name}"));
                }
            }
            return out;
        }
        return Vec::new();
    }
    if let Some(rest) = trimmed.strip_prefix("import ") {
        return rest
            .split(',')
            .filter_map(|part| {
                let module = part.trim().split(" as ").next()?.trim();
                if module.is_empty() {
                    None
                } else {
                    Some(module.to_string())
                }
            })
            .collect();
    }
    Vec::new()
}

fn js_import_module_specifiers(raw: &str) -> Vec<String> {
    extract_quoted_module_specifier(raw).into_iter().collect()
}

fn csharp_import_module_specifiers(raw: &str) -> Vec<String> {
    let trimmed = raw
        .trim()
        .trim_end_matches(';')
        .trim_start_matches("global ")
        .trim_start_matches("using ")
        .trim_start_matches("static ")
        .trim();
    let module = trimmed
        .split_once('=')
        .map_or(trimmed, |(_, rhs)| rhs)
        .trim();
    if module.is_empty() {
        Vec::new()
    } else {
        vec![module.to_string()]
    }
}

fn go_import_module_specifiers(raw: &str) -> Vec<String> {
    raw.lines()
        .flat_map(|line| line.split('"').nth(1))
        .map(ToOwned::to_owned)
        .collect()
}

fn c_import_module_specifiers(raw: &str) -> Vec<String> {
    raw.split(['"', '<'])
        .nth(1)
        .and_then(|value| value.split(['"', '>']).next())
        .filter(|value| !value.is_empty())
        .map(|value| vec![value.to_string()])
        .unwrap_or_default()
}

fn extract_quoted_module_specifier(raw: &str) -> Option<String> {
    for quote in ['"', '\'', '`'] {
        let mut parts = raw.split(quote);
        let Some(_before) = parts.next() else {
            continue;
        };
        let Some(candidate) = parts.next() else {
            continue;
        };
        let candidate = candidate.trim();
        if !candidate.is_empty() {
            return Some(candidate.to_string());
        }
    }
    None
}

fn import_imported_names(language: SourceLanguage, raw: &str) -> Vec<String> {
    match language {
        SourceLanguage::Rust => rust_imported_names(raw),
        SourceLanguage::Python => python_imported_names(raw),
        SourceLanguage::JavaScript | SourceLanguage::TypeScript | SourceLanguage::Tsx => {
            js_imported_names(raw)
        }
        SourceLanguage::CSharp | SourceLanguage::Razor => csharp_imported_names(raw),
        SourceLanguage::Go => go_import_module_specifiers(raw)
            .into_iter()
            .filter_map(|module| module.rsplit('/').next().map(ToOwned::to_owned))
            .collect(),
        SourceLanguage::C | SourceLanguage::Cpp => c_import_module_specifiers(raw),
        SourceLanguage::Java | SourceLanguage::Kotlin | SourceLanguage::Swift => {
            import_module_specifiers(language, raw)
                .into_iter()
                .filter_map(|module| module.rsplit('.').next().map(ToOwned::to_owned))
                .collect()
        }
        _ => Vec::new(),
    }
}

fn csharp_imported_names(raw: &str) -> Vec<String> {
    csharp_import_module_specifiers(raw)
        .into_iter()
        .filter_map(|module| module.rsplit('.').next().map(ToOwned::to_owned))
        .collect()
}

fn rust_imported_names(raw: &str) -> Vec<String> {
    let trimmed = raw
        .trim()
        .trim_start_matches("pub ")
        .trim_start_matches("use ")
        .trim_end_matches(';')
        .trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if let Some((_, suffix)) = trimmed.split_once("::{") {
        let inner = suffix.trim_end_matches('}').trim();
        let mut out = Vec::new();
        for item in inner.split(',') {
            let item = item.trim();
            if item.is_empty() || item == "*" {
                continue;
            }
            let item = item.split(" as ").next().unwrap_or(item).trim();
            if item == "self" {
                continue;
            }
            out.push(item.to_string());
        }
        out.sort();
        out.dedup();
        return out;
    }
    trimmed
        .split(" as ")
        .next()
        .unwrap_or(trimmed)
        .rsplit("::")
        .next()
        .filter(|item| !item.is_empty() && *item != "*")
        .map(|item| vec![item.to_string()])
        .unwrap_or_default()
}

fn python_imported_names(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    let Some(rest) = trimmed.strip_prefix("from ") else {
        return Vec::new();
    };
    let Some((_, imported)) = rest.split_once(" import ") else {
        return Vec::new();
    };
    imported
        .split(',')
        .filter_map(|part| {
            let name = part.trim().split(" as ").next()?.trim();
            if name.is_empty() || name == "*" {
                None
            } else {
                Some(name.to_string())
            }
        })
        .collect()
}

fn js_imported_names(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    let Some((head, _)) = trimmed.split_once(" from ") else {
        return Vec::new();
    };
    // `export { X } from "./x"` re-exports X: for edge purposes X is a name
    // this file pulls in, exactly like an import.
    let head = head
        .trim_start_matches("import")
        .trim_start_matches("export")
        .trim();
    let head = head.strip_prefix("type ").unwrap_or(head).trim();
    let mut out = Vec::new();

    if let Some((prefix, suffix)) = head.split_once('{') {
        let default_part = prefix.trim().trim_end_matches(',').trim();
        if !default_part.is_empty() {
            let default_name = default_part
                .strip_prefix("type ")
                .unwrap_or(default_part)
                .trim();
            if !default_name.is_empty()
                && default_name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
            {
                out.push(default_name.to_string());
            }
        }
        let inner = suffix.trim_end_matches('}').trim();
        for part in inner.split(',') {
            let item = part.trim().trim_start_matches("type ").trim();
            if let Some(name) = item.split(" as ").next() {
                let name = name.trim();
                if !name.is_empty() {
                    out.push(name.to_string());
                }
            }
        }
    } else if let Some((_, alias)) = head.split_once("* as ") {
        let alias = alias.trim();
        if !alias.is_empty() {
            out.push(alias.to_string());
        }
    } else {
        let name = head.trim();
        if !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        {
            out.push(name.to_string());
        }
    }

    out.sort();
    out.dedup();
    out
}

fn build_import_resolver_context(index: &RepoIndex, root: &Path) -> ImportResolverContext {
    let indexed_paths = index
        .files
        .iter()
        .map(|file| normalize_repo_path(Path::new(&file.path)))
        .collect::<BTreeSet<_>>();
    let mut rust_crate_roots = BTreeMap::new();

    for file in &index.files {
        if file.path.ends_with("Cargo.toml") {
            let manifest_path = root.join(&file.path);
            let raw = match fs::read_to_string(&manifest_path) {
                Ok(raw) => raw,
                Err(_) => continue,
            };
            let manifest = match toml::from_str::<CargoManifestToml>(&raw) {
                Ok(manifest) => manifest,
                Err(_) => continue,
            };
            let Some(package) = manifest.package else {
                continue;
            };
            let base_dir = Path::new(&file.path)
                .parent()
                .unwrap_or_else(|| Path::new(""));
            let lib_path = manifest
                .lib
                .as_ref()
                .and_then(|lib| lib.path.as_deref())
                .unwrap_or("src/lib.rs");
            let src_root = base_dir.join(
                Path::new(lib_path)
                    .parent()
                    .unwrap_or_else(|| Path::new("src")),
            );
            let normalized_src_root = normalize_repo_path(&src_root);
            if normalized_src_root.is_empty() {
                continue;
            }
            rust_crate_roots.insert(package.name.replace('-', "_"), normalized_src_root.clone());
            if let Some(lib_name) = manifest.lib.and_then(|lib| lib.name) {
                rust_crate_roots.insert(lib_name.replace('-', "_"), normalized_src_root);
            }
        }
    }

    let mut ts_alias_tables = BTreeMap::new();
    for file in &index.files {
        // v1 scope: only files named exactly `tsconfig.json` define alias
        // tables; `tsconfig.*.json` variants only participate as one-level
        // `extends` targets resolved below.
        let is_tsconfig = Path::new(&file.path)
            .file_name()
            .and_then(|name| name.to_str())
            == Some("tsconfig.json");
        if !is_tsconfig {
            continue;
        }
        let tsconfig_dir = normalize_repo_path(
            Path::new(&file.path)
                .parent()
                .unwrap_or_else(|| Path::new("")),
        );
        let Some(aliases) = load_tsconfig_aliases(root, &file.path, &indexed_paths) else {
            continue;
        };
        if aliases.is_empty() {
            continue;
        }
        ts_alias_tables.insert(tsconfig_dir, aliases);
    }

    ImportResolverContext {
        indexed_paths,
        rust_crate_roots,
        ts_alias_tables,
    }
}

/// Loads and normalizes the `compilerOptions.paths` aliases of one tsconfig.
///
/// Supports ONE level of `extends` (relative targets resolved against the
/// tsconfig directory, `.json` appended when absent; the target must be an
/// indexed file). Child `baseUrl`/`paths` win per-key over the extended base.
/// All merged mappings resolve relative to the CHILD tsconfig directory
/// joined with the effective `baseUrl` (the tsconfig directory itself when
/// `baseUrl` is absent). Deeper extends chains, package-name extends targets,
/// and `tsconfig.*.json` alias owners are out of scope. Missing or
/// unparseable files yield `None` silently — this runs on the export hot
/// path, and the legacy hardcoded fallbacks still apply downstream.
fn load_tsconfig_aliases(
    root: &Path,
    tsconfig_path: &str,
    indexed_paths: &BTreeSet<String>,
) -> Option<Vec<TsPathAlias>> {
    let raw = fs::read_to_string(root.join(tsconfig_path)).ok()?;
    let child = parse_jsonc(&raw).ok()?;
    let tsconfig_dir = normalize_repo_path(
        Path::new(tsconfig_path)
            .parent()
            .unwrap_or_else(|| Path::new("")),
    );

    let parent = child
        .get("extends")
        .and_then(|value| value.as_str())
        .and_then(|target| resolve_tsconfig_extends_target(&tsconfig_dir, target, indexed_paths))
        .and_then(|resolved| fs::read_to_string(root.join(&resolved)).ok())
        .and_then(|raw| parse_jsonc(&raw).ok());

    let base_url = compiler_option_str(&child, "baseUrl").or_else(|| {
        parent
            .as_ref()
            .and_then(|p| compiler_option_str(p, "baseUrl"))
    });

    let mut merged_paths: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if let Some(parent) = &parent {
        collect_compiler_paths(parent, &mut merged_paths);
    }
    collect_compiler_paths(&child, &mut merged_paths);
    if merged_paths.is_empty() {
        return None;
    }

    let base_dir = match base_url.as_deref() {
        Some(base_url) => normalize_repo_path(&Path::new(&tsconfig_dir).join(base_url)),
        None => tsconfig_dir,
    };
    let mut aliases = merged_paths
        .iter()
        .filter_map(|(pattern, targets)| normalize_ts_path_mapping(pattern, targets, &base_dir))
        .collect::<Vec<_>>();
    // Longest prefix first so `@app/foo/*` outranks `@app/*`; lexicographic
    // tie-break keeps the table deterministic.
    aliases.sort_by(|left, right| {
        right
            .alias_prefix
            .len()
            .cmp(&left.alias_prefix.len())
            .then_with(|| left.alias_prefix.cmp(&right.alias_prefix))
    });
    Some(aliases)
}

/// Resolves a relative `extends` target to a repo-relative indexed path.
///
/// Returns `None` for package-name targets (no leading `.`) and for targets
/// that are not part of the index even after appending `.json`.
fn resolve_tsconfig_extends_target(
    tsconfig_dir: &str,
    target: &str,
    indexed_paths: &BTreeSet<String>,
) -> Option<String> {
    if !target.starts_with('.') {
        return None;
    }
    let resolved = normalize_repo_path(&Path::new(tsconfig_dir).join(target));
    if indexed_paths.contains(&resolved) {
        return Some(resolved);
    }
    let with_extension = format!("{resolved}.json");
    if indexed_paths.contains(&with_extension) {
        return Some(with_extension);
    }
    None
}

fn compiler_option_str(config: &serde_json::Value, key: &str) -> Option<String> {
    config
        .get("compilerOptions")
        .and_then(|options| options.get(key))
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

/// Overlays `compilerOptions.paths` entries onto `out` (later callers win per-key).
fn collect_compiler_paths(config: &serde_json::Value, out: &mut BTreeMap<String, Vec<String>>) {
    let Some(paths) = config
        .get("compilerOptions")
        .and_then(|options| options.get("paths"))
        .and_then(|value| value.as_object())
    else {
        return;
    };
    for (pattern, targets) in paths {
        let Some(targets) = targets.as_array() else {
            continue;
        };
        let targets = targets
            .iter()
            .filter_map(|target| target.as_str())
            .map(str::to_string)
            .collect::<Vec<_>>();
        if !targets.is_empty() {
            out.insert(pattern.clone(), targets);
        }
    }
}

/// Normalizes one `paths` mapping into a [`TsPathAlias`], or `None` when the
/// pattern shape is unsupported (see [`TsPathAlias`] for the v1 scope).
fn normalize_ts_path_mapping(
    pattern: &str,
    targets: &[String],
    base_dir: &str,
) -> Option<TsPathAlias> {
    let alias_prefix = match pattern.matches('*').count() {
        0 => {
            if pattern.is_empty() {
                return None;
            }
            pattern.to_string()
        }
        1 => pattern.strip_suffix('*')?.to_string(),
        _ => return None,
    };
    let wildcard = pattern.ends_with('*');
    let resolved_targets = targets
        .iter()
        .filter_map(|target| {
            let stars = target.matches('*').count();
            if wildcard {
                if stars == 1 {
                    target
                        .strip_suffix('*')
                        .map(|prefix| join_alias_target(base_dir, prefix))
                } else {
                    None
                }
            } else if stars == 0 {
                Some(join_alias_target(base_dir, target))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    if resolved_targets.is_empty() {
        return None;
    }
    Some(TsPathAlias {
        alias_prefix,
        wildcard,
        targets: resolved_targets,
    })
}

/// Joins a tsconfig-relative target onto the effective base directory.
///
/// The result intentionally stays un-normalized (it may contain `./` or
/// `../` segments and a trailing `/` for wildcard prefixes);
/// [`expand_ts_candidate_paths`] normalizes the final joined candidate.
fn join_alias_target(base_dir: &str, target: &str) -> String {
    if base_dir.is_empty() {
        target.to_string()
    } else {
        format!("{base_dir}/{target}")
    }
}

fn resolve_import_paths(
    context: &ImportResolverContext,
    owner_path: &str,
    language: SourceLanguage,
    module_specifiers: &[String],
) -> ImportResolution {
    let mut candidate_paths = BTreeSet::new();
    for specifier in module_specifiers {
        for candidate in resolve_import_specifier(context, owner_path, language, specifier) {
            candidate_paths.insert(candidate);
        }
    }
    let candidate_paths = candidate_paths.into_iter().collect::<Vec<_>>();
    let kind = match candidate_paths.len() {
        0 => "unresolved",
        1 => "resolved",
        _ => "ambiguous",
    };
    ImportResolution {
        kind: kind.to_string(),
        candidate_paths,
    }
}

fn resolve_import_symbols(
    file_lookup_path: &BTreeMap<String, Vec<String>>,
    file_symbols: &BTreeMap<String, Vec<CachedGraphNeighbor>>,
    candidate_paths: &[String],
    imported_names: &[String],
) -> Vec<CachedGraphNeighbor> {
    if candidate_paths.len() != 1 || imported_names.is_empty() {
        return Vec::new();
    }
    let Some(target_file_iris) = file_lookup_path.get(&candidate_paths[0]) else {
        return Vec::new();
    };
    let mut resolved = Vec::new();
    for imported_name in imported_names {
        let mut matches = Vec::new();
        for target_file_iri in target_file_iris {
            if let Some(symbols) = file_symbols.get(target_file_iri) {
                matches.extend(
                    symbols
                        .iter()
                        .filter(|symbol| symbol.name == *imported_name)
                        .cloned(),
                );
            }
        }
        if matches.len() == 1 {
            resolved.push(matches.remove(0));
        }
    }
    resolved.sort_by(|left, right| {
        left.qual_name
            .cmp(&right.qual_name)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.line.cmp(&right.line))
    });
    resolved.dedup_by(|left, right| left.iri == right.iri);
    resolved
}

fn resolve_import_specifier(
    context: &ImportResolverContext,
    owner_path: &str,
    language: SourceLanguage,
    specifier: &str,
) -> Vec<String> {
    match language {
        SourceLanguage::JavaScript | SourceLanguage::TypeScript | SourceLanguage::Tsx => {
            resolve_ts_import_specifier(context, owner_path, specifier)
        }
        SourceLanguage::Python => resolve_python_import_specifier(context, owner_path, specifier),
        SourceLanguage::Rust => resolve_rust_import_specifier(context, owner_path, specifier),
        SourceLanguage::Rdf => resolve_rdf_import_specifier(context, owner_path, specifier),
        SourceLanguage::C | SourceLanguage::Cpp => {
            resolve_c_include_specifier(context, owner_path, specifier)
        }
        SourceLanguage::Go => resolve_go_import_specifier(context, specifier),
        _ => Vec::new(),
    }
}

fn resolve_c_include_specifier(
    context: &ImportResolverContext,
    owner_path: &str,
    specifier: &str,
) -> Vec<String> {
    let Some(relative) = resolve_relative_path(owner_path, specifier) else {
        return Vec::new();
    };
    if context.indexed_paths.contains(&relative) {
        vec![relative]
    } else {
        Vec::new()
    }
}

fn resolve_go_import_specifier(context: &ImportResolverContext, specifier: &str) -> Vec<String> {
    let package = specifier.rsplit('/').next().unwrap_or(specifier);
    context
        .indexed_paths
        .iter()
        .filter(|path| path.ends_with(".go"))
        .filter(|path| {
            Path::new(path)
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == package)
        })
        .cloned()
        .collect()
}

fn resolve_rdf_import_specifier(
    context: &ImportResolverContext,
    owner_path: &str,
    specifier: &str,
) -> Vec<String> {
    let specifier = specifier.trim();
    if specifier.is_empty() {
        return Vec::new();
    }
    if specifier.starts_with("./") || specifier.starts_with("../") {
        return resolve_relative_path(owner_path, specifier)
            .into_iter()
            .filter(|path| context.indexed_paths.contains(path))
            .collect();
    }
    if crate::ontology::is_rdf_path(Path::new(specifier))
        && context.indexed_paths.contains(specifier)
    {
        return vec![specifier.to_string()];
    }
    let iri = specifier.trim_end_matches(['#', '/']);
    let Some(segment) = iri.rsplit(['/', '#']).next() else {
        return Vec::new();
    };
    if segment.is_empty() {
        return Vec::new();
    }
    context
        .indexed_paths
        .iter()
        .filter(|path| {
            let name = path.rsplit('/').next().unwrap_or(path.as_str());
            if !crate::ontology::is_rdf_path(Path::new(name)) {
                return false;
            }
            let stem = name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name);
            stem.eq_ignore_ascii_case(segment) || name.eq_ignore_ascii_case(segment)
        })
        .cloned()
        .collect()
}

fn resolve_ts_import_specifier(
    context: &ImportResolverContext,
    owner_path: &str,
    specifier: &str,
) -> Vec<String> {
    let specifier = specifier.trim();
    if specifier.is_empty() {
        return Vec::new();
    }
    if specifier.starts_with("./") || specifier.starts_with("../") {
        return resolve_relative_path(owner_path, specifier)
            .into_iter()
            .flat_map(|base| expand_ts_candidate_paths(context, &base))
            .collect();
    }
    // tsconfig-derived aliases first: works for any TypeScript monorepo.
    let alias_candidates = resolve_ts_alias_candidates(context, owner_path, specifier);
    if !alias_candidates.is_empty() {
        return alias_candidates;
    }
    // Legacy hardcoded fallbacks (Example workspace shapes), kept so repos
    // whose tsconfigs are missing from the index or unparseable still resolve.
    let base = if let Some(rest) = specifier.strip_prefix("@jai/trpc/") {
        Some(format!("packages/trpc/src/{rest}"))
    } else if let Some(rest) = specifier.strip_prefix("@contracts/generated/") {
        Some(format!("packages/example-ops-contracts/generated/{rest}"))
    } else if let Some(rest) = specifier.strip_prefix("@contracts/analytics/") {
        Some(format!("packages/example-ops-contracts/analytics/{rest}"))
    } else if let Some(rest) = specifier.strip_prefix("@/") {
        ts_app_src_root(owner_path).map(|root| format!("{root}/{rest}"))
    } else {
        None
    };
    base.into_iter()
        .flat_map(|base| expand_ts_candidate_paths(context, &base))
        .collect()
}

/// Resolves a non-relative TS specifier through tsconfig alias tables.
///
/// Walks ancestor directories of `owner_path` nearest-first; the first
/// tsconfig table whose aliases yield at least one indexed candidate wins.
/// Tables that match but expand to nothing are skipped so an outer monorepo
/// tsconfig can still answer. An empty result tells the caller to fall back
/// to the legacy hardcoded rules.
fn resolve_ts_alias_candidates(
    context: &ImportResolverContext,
    owner_path: &str,
    specifier: &str,
) -> Vec<String> {
    if context.ts_alias_tables.is_empty() {
        return Vec::new();
    }
    let mut dir = Path::new(owner_path).parent();
    while let Some(current) = dir {
        let key = normalize_repo_path(current);
        if let Some(aliases) = context.ts_alias_tables.get(&key) {
            let candidates = resolve_with_alias_table(context, aliases, specifier);
            if !candidates.is_empty() {
                return candidates;
            }
        }
        if key.is_empty() {
            break;
        }
        dir = current.parent();
    }
    Vec::new()
}

/// Applies one alias table (sorted longest `alias_prefix` first) to a specifier.
///
/// The first matching alias that expands to at least one indexed candidate
/// wins (most-specific pattern semantics); within that alias every target is
/// tried in tsconfig order and all hits are collected.
fn resolve_with_alias_table(
    context: &ImportResolverContext,
    aliases: &[TsPathAlias],
    specifier: &str,
) -> Vec<String> {
    for alias in aliases {
        let rest = if alias.wildcard {
            specifier.strip_prefix(&alias.alias_prefix)
        } else if specifier == alias.alias_prefix {
            Some("")
        } else {
            None
        };
        let Some(rest) = rest else {
            continue;
        };
        let mut candidates = BTreeSet::new();
        for target in &alias.targets {
            let base = if alias.wildcard {
                format!("{target}{rest}")
            } else {
                target.clone()
            };
            for hit in expand_ts_candidate_paths(context, &base) {
                candidates.insert(hit);
            }
        }
        if !candidates.is_empty() {
            return candidates.into_iter().collect();
        }
    }
    Vec::new()
}

fn ts_app_src_root(owner_path: &str) -> Option<&'static str> {
    if owner_path.starts_with("example-ops/") {
        Some("example-ops/src")
    } else if owner_path.starts_with("jai-pay/") {
        Some("jai-pay/src")
    } else if owner_path.starts_with("health-audit-console/") {
        Some("health-audit-console")
    } else if owner_path.starts_with("vigoros/app/") {
        Some("vigoros/app/src")
    } else {
        None
    }
}

fn expand_ts_candidate_paths(context: &ImportResolverContext, base: &str) -> Vec<String> {
    let mut candidates = BTreeSet::new();
    let base = normalize_repo_path(Path::new(base));
    if context.indexed_paths.contains(&base) {
        candidates.insert(base.clone());
    }
    let looks_like_file = Path::new(&base)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some();
    if looks_like_file {
        if context.indexed_paths.contains(&base) {
            return vec![base];
        }
        return Vec::new();
    }
    for extension in [".ts", ".tsx", ".js", ".jsx", ".mts", ".cts", ".d.ts"] {
        let path = format!("{base}{extension}");
        if context.indexed_paths.contains(&path) {
            candidates.insert(path);
        }
    }
    for index_file in [
        "index.ts",
        "index.tsx",
        "index.js",
        "index.jsx",
        "index.mts",
        "index.cts",
        "index.d.ts",
    ] {
        let path = normalize_repo_path(&Path::new(&base).join(index_file));
        if context.indexed_paths.contains(&path) {
            candidates.insert(path);
        }
    }
    candidates.into_iter().collect()
}

fn resolve_python_import_specifier(
    context: &ImportResolverContext,
    owner_path: &str,
    specifier: &str,
) -> Vec<String> {
    let specifier = specifier.trim();
    if specifier.is_empty() {
        return Vec::new();
    }
    let base = if specifier.starts_with('.') {
        resolve_python_relative_module(owner_path, specifier)
    } else {
        resolve_python_absolute_module(specifier)
    };
    base.into_iter()
        .flat_map(|base| expand_python_candidate_paths(context, &base))
        .collect()
}

fn resolve_python_relative_module(owner_path: &str, specifier: &str) -> Option<String> {
    let dots = specifier.chars().take_while(|ch| *ch == '.').count();
    let remainder = specifier[dots..].trim();
    let mut base_dir = Path::new(owner_path).parent()?.to_path_buf();
    for _ in 1..dots {
        base_dir = base_dir.parent()?.to_path_buf();
    }
    let base = if remainder.is_empty() {
        base_dir
    } else {
        base_dir.join(remainder.replace('.', "/"))
    };
    Some(normalize_repo_path(&base))
}

fn resolve_python_absolute_module(specifier: &str) -> Option<String> {
    for (prefix, root) in [
        ("example", "example-api/example"),
        ("cartridges", "cartridges"),
        ("event_jepa_cube", "jcube/event_jepa_cube"),
    ] {
        if specifier == prefix {
            return Some(root.to_string());
        }
        if let Some(rest) = specifier.strip_prefix(&format!("{prefix}.")) {
            return Some(format!("{root}/{}", rest.replace('.', "/")));
        }
    }
    None
}

fn expand_python_candidate_paths(context: &ImportResolverContext, base: &str) -> Vec<String> {
    let mut candidates = BTreeSet::new();
    let base = normalize_repo_path(Path::new(base));
    let module_file = format!("{base}.py");
    if context.indexed_paths.contains(&module_file) {
        candidates.insert(module_file);
    }
    let package_init = normalize_repo_path(&Path::new(&base).join("__init__.py"));
    if context.indexed_paths.contains(&package_init) {
        candidates.insert(package_init);
    }
    candidates.into_iter().collect()
}

fn resolve_rust_import_specifier(
    context: &ImportResolverContext,
    owner_path: &str,
    specifier: &str,
) -> Vec<String> {
    let specifier = specifier.trim();
    if specifier.is_empty() {
        return Vec::new();
    }
    let segments = specifier
        .split("::")
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.is_empty() {
        return Vec::new();
    }

    let mut base_roots = Vec::new();
    let mut start_index = 0usize;
    match segments[0] {
        "crate" => {
            if let Some(crate_src_root) = rust_src_root_for_file(owner_path) {
                base_roots.push(crate_src_root);
                start_index = 1;
            }
        }
        "self" => {
            if let Some(current_module_dir) = current_module_dir(owner_path) {
                base_roots.push(current_module_dir);
                start_index = 1;
            }
        }
        "super" => {
            if let Some(current_module_dir) = current_module_dir(owner_path) {
                let mut level = 0usize;
                while start_index < segments.len() && segments[start_index] == "super" {
                    level += 1;
                    start_index += 1;
                }
                if let Some(parent_dir) = ascend_relative_path(&current_module_dir, level) {
                    base_roots.push(parent_dir);
                }
            }
        }
        first => {
            if let Some(crate_root) = context.rust_crate_roots.get(first) {
                base_roots.push(crate_root.clone());
                start_index = 1;
            }
        }
    }
    if base_roots.is_empty() {
        return Vec::new();
    }

    let tail_segments = segments[start_index..]
        .iter()
        .map(|segment| segment.to_string())
        .collect::<Vec<_>>();
    let tail_variants = rust_tail_variants(&tail_segments);
    let mut candidates = BTreeSet::new();
    for base_root in base_roots {
        for tail in &tail_variants {
            for candidate in expand_rust_candidate_paths(context, &base_root, tail) {
                candidates.insert(candidate);
            }
        }
    }
    candidates.into_iter().collect()
}

fn rust_tail_variants(tail_segments: &[String]) -> Vec<Vec<String>> {
    if tail_segments.is_empty() {
        return Vec::new();
    }
    let mut variants = vec![tail_segments.to_vec()];
    if tail_segments.len() >= 2 {
        variants.push(tail_segments[..tail_segments.len() - 1].to_vec());
    }
    variants.sort();
    variants.dedup();
    variants
}

fn expand_rust_candidate_paths(
    context: &ImportResolverContext,
    base_root: &str,
    tail_segments: &[String],
) -> Vec<String> {
    if tail_segments.is_empty() {
        return Vec::new();
    }
    let module_base =
        normalize_repo_path(&Path::new(base_root).join(PathBuf::from_iter(tail_segments)));
    let mut candidates = BTreeSet::new();
    let file_path = format!("{module_base}.rs");
    if context.indexed_paths.contains(&file_path) {
        candidates.insert(file_path);
    }
    let mod_path = normalize_repo_path(&Path::new(&module_base).join("mod.rs"));
    if context.indexed_paths.contains(&mod_path) {
        candidates.insert(mod_path);
    }
    candidates.into_iter().collect()
}

fn rust_src_root_for_file(owner_path: &str) -> Option<String> {
    let marker = "/src/";
    let index = owner_path.find(marker)?;
    Some(owner_path[..index + marker.len() - 1].to_string())
}

fn current_module_dir(owner_path: &str) -> Option<String> {
    let path = Path::new(owner_path);
    let parent = path.parent()?;
    Some(normalize_repo_path(parent))
}

fn ascend_relative_path(path: &str, levels: usize) -> Option<String> {
    let mut cursor = Path::new(path).to_path_buf();
    for _ in 0..levels {
        cursor = cursor.parent()?.to_path_buf();
    }
    Some(normalize_repo_path(&cursor))
}

fn resolve_relative_path(owner_path: &str, specifier: &str) -> Option<String> {
    let parent = Path::new(owner_path).parent()?;
    Some(normalize_repo_path(&parent.join(specifier)))
}

fn normalize_repo_path(path: &Path) -> String {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    normalized.to_string_lossy().replace('\\', "/")
}

fn call_from_node(
    language: SourceLanguage,
    node: Node<'_>,
    source: &str,
) -> Option<(String, String)> {
    let call_kind = match language {
        SourceLanguage::Rust => "call_expression",
        SourceLanguage::Python => "call",
        SourceLanguage::JavaScript | SourceLanguage::TypeScript | SourceLanguage::Tsx => {
            "call_expression"
        }
        SourceLanguage::CSharp | SourceLanguage::Razor => "invocation_expression",
        SourceLanguage::Go => "call_expression",
        SourceLanguage::C | SourceLanguage::Cpp => "call_expression",
        SourceLanguage::Bash => "command",
        SourceLanguage::Java => "method_invocation",
        SourceLanguage::Kotlin | SourceLanguage::Swift => "call_expression",
        _ => return None,
    };
    if node.kind() != call_kind {
        return None;
    }
    let callee_node = node
        .child_by_field_name("function")
        .or_else(|| node.child_by_field_name("callee"))?;
    let callee_expr = callee_node
        .utf8_text(source.as_bytes())
        .ok()?
        .trim()
        .to_string();
    let callee_name = reference_name(callee_node, source)?;
    Some((callee_name, callee_expr))
}

fn reference_name(node: Node<'_>, source: &str) -> Option<String> {
    match node.kind() {
        // Generic arguments are types, not the invoked method's name.
        "generic_name" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .find(|child| child.kind() == "identifier")
                .and_then(|child| reference_name(child, source))
        }
        "identifier"
        | "type_identifier"
        | "field_identifier"
        | "property_identifier"
        | "shorthand_property_identifier_pattern" => {
            let value = node.utf8_text(source.as_bytes()).ok()?.trim().to_string();
            if value.is_empty() { None } else { Some(value) }
        }
        _ => {
            for field in ["name", "property", "attribute", "field"] {
                if let Some(child) = node.child_by_field_name(field)
                    && let Some(value) = reference_name(child, source)
                {
                    return Some(value);
                }
            }
            let mut cursor = node.walk();
            let named_children: Vec<Node<'_>> = node
                .children(&mut cursor)
                .filter(|child| child.is_named())
                .collect();
            for child in named_children.into_iter().rev() {
                if let Some(value) = reference_name(child, source) {
                    return Some(value);
                }
            }
            let raw = node.utf8_text(source.as_bytes()).ok()?.trim().to_string();
            let fallback = raw
                .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
                .rfind(|part| !part.is_empty())
                .map(ToOwned::to_owned)?;
            if fallback.is_empty() {
                None
            } else {
                Some(fallback)
            }
        }
    }
}

fn supports_code_graph(language: SourceLanguage) -> bool {
    matches!(
        language,
        SourceLanguage::Rust
            | SourceLanguage::Python
            | SourceLanguage::JavaScript
            | SourceLanguage::TypeScript
            | SourceLanguage::Tsx
            | SourceLanguage::CSharp
            | SourceLanguage::Razor
            | SourceLanguage::Go
            | SourceLanguage::C
            | SourceLanguage::Cpp
            | SourceLanguage::Bash
            | SourceLanguage::Java
            | SourceLanguage::Kotlin
            | SourceLanguage::Html
            | SourceLanguage::Css
            | SourceLanguage::Swift
            | SourceLanguage::Sql
            | SourceLanguage::Rdf
    )
}

fn ts_language(language: SourceLanguage) -> Option<Language> {
    match language {
        SourceLanguage::Rust => Some(tree_sitter_rust::LANGUAGE.into()),
        SourceLanguage::Python => Some(tree_sitter_python::LANGUAGE.into()),
        SourceLanguage::JavaScript => Some(tree_sitter_javascript::LANGUAGE.into()),
        SourceLanguage::TypeScript => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        SourceLanguage::Tsx => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        SourceLanguage::CSharp => Some(tree_sitter_c_sharp::LANGUAGE.into()),
        SourceLanguage::Razor => Some(tree_sitter_c_sharp::LANGUAGE.into()),
        SourceLanguage::Go => Some(tree_sitter_go::LANGUAGE.into()),
        SourceLanguage::C => Some(tree_sitter_c::LANGUAGE.into()),
        SourceLanguage::Cpp => Some(tree_sitter_cpp::LANGUAGE.into()),
        SourceLanguage::Bash => Some(tree_sitter_bash::LANGUAGE.into()),
        SourceLanguage::Java => Some(tree_sitter_java::LANGUAGE.into()),
        SourceLanguage::Html => Some(tree_sitter_html::LANGUAGE.into()),
        SourceLanguage::Css => Some(tree_sitter_css::LANGUAGE.into()),
        SourceLanguage::Swift => Some(tree_sitter_swift::LANGUAGE.into()),
        _ => None,
    }
}

fn symbol_kind(language: SourceLanguage, node_kind: &str) -> Option<SymbolKind> {
    match language {
        SourceLanguage::Rust => match node_kind {
            "function_item" => Some(SymbolKind::Function),
            "struct_item" => Some(SymbolKind::Struct),
            "enum_item" => Some(SymbolKind::Enum),
            "trait_item" => Some(SymbolKind::Trait),
            "mod_item" => Some(SymbolKind::Module),
            "const_item" | "static_item" => Some(SymbolKind::Constant),
            "type_item" => Some(SymbolKind::TypeAlias),
            _ => None,
        },
        SourceLanguage::Python => match node_kind {
            "function_definition" => Some(SymbolKind::Function),
            "class_definition" => Some(SymbolKind::Class),
            _ => None,
        },
        SourceLanguage::JavaScript | SourceLanguage::TypeScript | SourceLanguage::Tsx => {
            match node_kind {
                "function_declaration" => Some(SymbolKind::Function),
                "class_declaration" => Some(SymbolKind::Class),
                "method_definition" => Some(SymbolKind::Method),
                "interface_declaration" => Some(SymbolKind::Interface),
                "type_alias_declaration" => Some(SymbolKind::TypeAlias),
                "enum_declaration" => Some(SymbolKind::Enum),
                _ => None,
            }
        }
        SourceLanguage::CSharp | SourceLanguage::Razor => match node_kind {
            "method_declaration" | "constructor_declaration" | "local_function_statement" => {
                Some(SymbolKind::Method)
            }
            "class_declaration" => Some(SymbolKind::Class),
            "struct_declaration" => Some(SymbolKind::Struct),
            "interface_declaration" => Some(SymbolKind::Interface),
            "enum_declaration" => Some(SymbolKind::Enum),
            "record_declaration" => Some(SymbolKind::Struct),
            "delegate_declaration" => Some(SymbolKind::TypeAlias),
            "property_declaration" | "event_declaration" => Some(SymbolKind::Property),
            "namespace_declaration" => Some(SymbolKind::Module),
            _ => None,
        },
        SourceLanguage::Go => match node_kind {
            "function_declaration" => Some(SymbolKind::Function),
            "method_declaration" => Some(SymbolKind::Method),
            "type_declaration" | "type_spec" => Some(SymbolKind::TypeAlias),
            _ => None,
        },
        SourceLanguage::C | SourceLanguage::Cpp => match node_kind {
            "function_definition" => Some(SymbolKind::Function),
            "preproc_def" => Some(SymbolKind::Constant),
            "struct_specifier" => Some(SymbolKind::Struct),
            "class_specifier" => Some(SymbolKind::Class),
            "enum_specifier" => Some(SymbolKind::Enum),
            "type_definition" => Some(SymbolKind::TypeAlias),
            _ => None,
        },
        SourceLanguage::Bash => match node_kind {
            "function_definition" => Some(SymbolKind::Function),
            _ => None,
        },
        SourceLanguage::Java => match node_kind {
            "method_declaration" | "constructor_declaration" => Some(SymbolKind::Method),
            "class_declaration" => Some(SymbolKind::Class),
            "interface_declaration" => Some(SymbolKind::Interface),
            "enum_declaration" => Some(SymbolKind::Enum),
            _ => None,
        },
        SourceLanguage::Kotlin => match node_kind {
            "function_declaration" => Some(SymbolKind::Function),
            "class_declaration" | "object_declaration" => Some(SymbolKind::Class),
            "property_declaration" => Some(SymbolKind::Property),
            _ => None,
        },
        SourceLanguage::Html => match node_kind {
            "element" => Some(SymbolKind::Class),
            _ => None,
        },
        SourceLanguage::Css => match node_kind {
            "class_selector" | "id_selector" => Some(SymbolKind::Class),
            "declaration" => Some(SymbolKind::Property),
            _ => None,
        },
        SourceLanguage::Swift => match node_kind {
            "function_declaration" => Some(SymbolKind::Function),
            "class_declaration" => Some(SymbolKind::Class),
            "struct_declaration" => Some(SymbolKind::Struct),
            "enum_declaration" => Some(SymbolKind::Enum),
            _ => None,
        },
        _ => None,
    }
}

fn is_callable_kind(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Class
    )
}

fn detect_git_revision(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("rev-parse")
        .arg("--verify")
        .arg("HEAD")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

fn mint_file_iri(repo_slug: &str, path: &str) -> String {
    let path_slug = slug_component(path);
    let suffix = short_hash(format!("file:{repo_slug}:{path}").as_bytes(), 10);
    format!("urn:leio:code:file:{repo_slug}:{path_slug}:{suffix}")
}

fn mint_symbol_iri(
    repo_slug: &str,
    path: &str,
    language: SourceLanguage,
    kind: SymbolKind,
    qual_name: &str,
) -> String {
    let suffix = short_hash(
        format!(
            "symbol:{repo_slug}:{path}:{}:{}:{qual_name}",
            language.as_str(),
            kind.as_str()
        )
        .as_bytes(),
        12,
    );
    format!(
        "urn:leio:code:symbol:{repo_slug}:{}:{}:{}:{suffix}",
        slug_component(path),
        language.as_str(),
        slug_component(qual_name),
    )
}

fn mint_occurrence_iri(
    stable_iri: &str,
    revision: &str,
    start_line: usize,
    end_line: usize,
) -> String {
    format!(
        "{stable_iri}@{}:{}:{}",
        slug_component(revision),
        start_line,
        end_line
    )
}

/// Access-typed env predicate local name (without namespace), 1:1 with the
/// four `AccessKind` arms used by `export::env_attribute_label`.
///
/// The access distinction is load-bearing: `readsEnv:X` and `writesEnv:X` are
/// distinct formal-context attributes, so they must stay distinct predicates
/// here too. Collapsing them would erode the FCA access identity.
fn env_access_predicate(access: AccessKind) -> &'static str {
    match access {
        AccessKind::Read => "readsEnv",
        AccessKind::Write => "writesEnv",
        AccessKind::Declared => "declaresEnv",
        AccessKind::Unknown => "mentionsEnv",
    }
}

/// Access-typed redis predicate local name, mirror of [`env_access_predicate`].
fn redis_access_predicate(access: AccessKind) -> &'static str {
    match access {
        AccessKind::Read => "readsRedis",
        AccessKind::Write => "writesRedis",
        AccessKind::Declared => "declaresRedis",
        AccessKind::Unknown => "mentionsRedis",
    }
}

/// Mints a stable first-class IRI for an access-typed env attribute node.
///
/// `access` plus `name` uniquely identify the attribute, so no hash is needed.
fn mint_env_attr_iri(access: AccessKind, name: &str) -> String {
    format!(
        "urn:leio:code:env:{}:{}",
        access.as_str(),
        slug_component(name)
    )
}

/// Mints a stable first-class IRI for an access-typed redis attribute node.
fn mint_redis_attr_iri(access: AccessKind, key: &str) -> String {
    format!(
        "urn:leio:code:redis:{}:{}",
        access.as_str(),
        slug_component(key)
    )
}

/// Mints a stable first-class IRI for a route attribute node.
fn mint_route_attr_iri(full_path: &str) -> String {
    format!("urn:leio:code:route:{}", slug_component(full_path))
}

fn slug_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches('_').to_string()
}

fn short_hash(bytes: &[u8], len: usize) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    hex[..len.min(hex.len())].to_string()
}

fn write_iri_quad(buf: &mut String, subject: &str, predicate: &str, object: &str, graph: &str) {
    buf.push_str(&format!(
        "<{}> <{}> <{}> <{}> .\n",
        escape_iri(subject),
        escape_iri(predicate),
        escape_iri(object),
        escape_iri(graph)
    ));
}

fn write_literal_quad(buf: &mut String, subject: &str, predicate: &str, object: &str, graph: &str) {
    buf.push_str(&format!(
        "<{}> <{}> \"{}\" <{}> .\n",
        escape_iri(subject),
        escape_iri(predicate),
        escape_literal(object),
        escape_iri(graph)
    ));
}

fn escape_iri(value: &str) -> String {
    value
        .replace('>', "%3E")
        .replace('<', "%3C")
        .replace('"', "%22")
        .replace(' ', "_")
}

fn escape_literal(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn relative_to(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_and_occurrence_iris_are_distinct() {
        let stable = mint_symbol_iri(
            "repo",
            "src/lib.rs",
            SourceLanguage::Rust,
            SymbolKind::Function,
            "foo::bar",
        );
        let occurrence = mint_occurrence_iri(&stable, "rev1", 10, 20);
        assert!(stable.starts_with("urn:leio:code:symbol:repo:"));
        assert!(occurrence.contains("@rev1:10:20"));
        assert_ne!(stable, occurrence);
    }

    #[test]
    fn python_calls_resolve_to_local_definition_name() {
        let source = r#"
def helper():
    return 1

def caller():
    return helper()
"#;
        let parsed = parse_file_graph(
            "pkg/example.py",
            SourceLanguage::Python,
            source,
            "repo",
            "rev1",
        )
        .expect("python graph should parse");

        assert_eq!(parsed.definitions.len(), 2);
        assert_eq!(parsed.calls.len(), 1);
        assert_eq!(parsed.calls[0].callee_name, "helper");
    }

    #[test]
    fn csharp_graph_extracts_definitions_calls_and_using() {
        let source = "using Demo.Core; namespace Demo; public class Worker { public void Run() { Helper(); } private void Helper() {} }";
        let parsed = parse_file_graph("Worker.cs", SourceLanguage::CSharp, source, "repo", "rev1")
            .expect("C# graph should parse");

        assert!(parsed.definitions.iter().any(|d| d.name == "Worker"));
        assert!(parsed.definitions.iter().any(|d| d.name == "Run"));
        assert!(parsed.definitions.iter().any(|d| d.name == "Helper"));
        assert_eq!(parsed.imports.len(), 1);
        assert_eq!(parsed.imports[0].module_specifiers, vec!["Demo.Core"]);
        assert!(parsed.calls.iter().any(|c| c.callee_name == "Helper"));
    }

    #[test]
    fn csharp_top_level_program_graph_keeps_framework_calls() {
        let source = r#"
using F22.Client.Web.F22Dashboard.Components;
var builder = WebApplication.CreateBuilder(args);
builder.Services.AddScoped<HomeContentService>();
var app = builder.Build();
app.MapF22BlazorWeb<App>();
app.Run();
"#;
        let parsed = parse_file_graph(
            "src/Client/Web/F22.Client.Web.F22Dashboard/Program.cs",
            SourceLanguage::CSharp,
            source,
            "repo",
            "rev1",
        )
        .expect("top-level C# graph should parse");

        assert_eq!(parsed.imports[0].module_specifiers, vec!["F22.Client.Web.F22Dashboard.Components"]);
        assert!(parsed.definitions.is_empty(), "top-level statements have no named declarations");
        assert!(parsed.calls.iter().any(|call| call.callee_name == "CreateBuilder"));
        assert!(parsed
            .calls
            .iter()
            .any(|call| call.callee_name == "MapF22BlazorWeb"));
        assert!(parsed.calls.iter().any(|call| call.callee_name == "Run"));
        assert!(parsed.calls.iter().any(|call| call.callee_name == "AddScoped"));
        assert!(!parsed.calls.iter().any(|call| ["App", "HomeContentService"].contains(&call.callee_name.as_str())));
    }

    #[test]
    fn razor_graph_extracts_code_block_symbols_without_markup() {
        let source = r#"<h1>Hello</h1>
@code {
    public void Save() { Refresh(); }
    private void Refresh() {}
}
"#;
        let parsed = parse_file_graph("Pages/Index.razor", SourceLanguage::Razor, source, "repo", "rev1")
            .expect("Razor graph should parse");

        assert!(parsed.definitions.iter().any(|definition| definition.name == "Save"));
        assert!(parsed.definitions.iter().any(|definition| definition.name == "Refresh"));
        assert!(parsed.calls.iter().any(|call| call.callee_name == "Refresh"));
        assert!(!parsed.definitions.iter().any(|definition| definition.name == "Hello"));
    }

    #[test]
    fn go_c_cpp_and_bash_graphs_parse() {
        let go = parse_file_graph(
            "cmd/main.go",
            SourceLanguage::Go,
            "package main\nimport \"fmt\"\nfunc Run() { fmt.Println(1) }\n",
            "repo",
            "rev1",
        )
        .expect("Go graph should parse");
        assert!(go.definitions.iter().any(|d| d.name == "Run"));
        assert_eq!(go.imports[0].module_specifiers, vec!["fmt"]);

        let c = parse_file_graph(
            "src/main.c",
            SourceLanguage::C,
            "#include \"worker.h\"\nvoid run() { helper(); }\n",
            "repo",
            "rev1",
        )
        .expect("C graph should parse");
        assert_eq!(c.imports[0].module_specifiers, vec!["worker.h"]);
        assert!(c.definitions.iter().any(|d| d.name == "run"));
        assert!(c.calls.iter().any(|call| call.callee_name == "helper"));

        let cpp = parse_file_graph(
            "src/main.cpp",
            SourceLanguage::Cpp,
            "class Worker {}; void run() { helper(); }\n",
            "repo",
            "rev1",
        )
        .expect("C++ graph should parse");
        assert!(cpp.definitions.iter().any(|d| d.name == "Worker"));
        assert!(cpp.calls.iter().any(|call| call.callee_name == "helper"));

        let bash = parse_file_graph(
            "scripts/build.sh",
            SourceLanguage::Bash,
            "build() { echo building; }\nbuild\n",
            "repo",
            "rev1",
        )
        .expect("Bash graph should parse");
        assert!(bash.definitions.iter().any(|d| d.name == "build"));
    }

    #[test]
    fn sql_graph_extracts_schema_definitions() {
        let parsed = parse_file_graph(
            "db/schema.sql",
            SourceLanguage::Sql,
            "CREATE TABLE users (id INTEGER);\nCREATE VIEW active_users AS SELECT * FROM users;",
            "repo",
            "rev1",
        )
        .expect("SQL graph should parse");
        assert!(parsed.definitions.iter().any(|d| d.name == "users"));
        assert!(parsed.definitions.iter().any(|d| d.name == "active_users"));
    }

    #[test]
    fn python_imports_capture_module_specifiers() {
        let source = r#"
import os, sys as system
from pathlib import Path
"#;
        let parsed = parse_file_graph(
            "pkg/example.py",
            SourceLanguage::Python,
            source,
            "repo",
            "rev1",
        )
        .expect("python graph should parse");

        assert_eq!(parsed.imports.len(), 2);
        assert_eq!(parsed.imports[0].module_specifiers, vec!["os", "sys"]);
        // `from pathlib import Path` emits both the package specifier and the
        // potential submodule specifier `pathlib.Path`. The resolver will
        // discard `pathlib.Path` when no `pathlib/Path.py` exists on disk.
        assert_eq!(
            parsed.imports[1].module_specifiers,
            vec!["pathlib", "pathlib.Path"]
        );
    }

    #[test]
    fn rust_use_and_call_are_captured() {
        let source = r#"
use crate::dep::helper;

fn run() {
    helper();
}
"#;
        let parsed = parse_file_graph("src/lib.rs", SourceLanguage::Rust, source, "repo", "rev1")
            .expect("rust graph should parse");

        assert_eq!(parsed.imports.len(), 1);
        assert_eq!(
            parsed.imports[0].module_specifiers,
            vec!["crate::dep::helper"]
        );
        assert_eq!(parsed.calls.len(), 1);
        assert_eq!(parsed.calls[0].callee_name, "helper");
    }

    #[test]
    fn rust_resolver_maps_workspace_crate_imports_to_files() {
        let mut indexed_paths = BTreeSet::new();
        indexed_paths.insert("leio-code/src/code_graph.rs".to_string());
        let context = ImportResolverContext {
            indexed_paths,
            rust_crate_roots: BTreeMap::from([(
                "leio_code".to_string(),
                "leio-code/src".to_string(),
            )]),
            ts_alias_tables: BTreeMap::new(),
        };

        let resolved = resolve_rust_import_specifier(
            &context,
            "leio-code/src/main.rs",
            "leio_code::code_graph::export_code_graph",
        );

        assert_eq!(resolved, vec!["leio-code/src/code_graph.rs"]);
    }

    #[test]
    fn python_resolver_maps_workspace_and_relative_modules_to_files() {
        let mut indexed_paths = BTreeSet::new();
        indexed_paths.insert("example-api/example/auth/dependencies.py".to_string());
        indexed_paths.insert("cartridges/vigoros/router.py".to_string());
        let context = ImportResolverContext {
            indexed_paths,
            rust_crate_roots: BTreeMap::new(),
            ts_alias_tables: BTreeMap::new(),
        };

        let absolute = resolve_python_import_specifier(
            &context,
            "scripts/example.py",
            "example.auth.dependencies",
        );
        let relative =
            resolve_python_import_specifier(&context, "cartridges/vigoros/tasks.py", ".router");

        assert_eq!(absolute, vec!["example-api/example/auth/dependencies.py"]);
        assert_eq!(relative, vec!["cartridges/vigoros/router.py"]);
    }

    #[test]
    fn python_dotted_string_literals_are_captured_as_implicit_imports() {
        let source = r#"
ROUTERS = {
    "agents_router": (".agents", "router"),
    "tasks_router": ("..agents.router", "router"),
}
"#;
        let parsed = parse_file_graph(
            "example-api/example/routers/__init__.py",
            SourceLanguage::Python,
            source,
            "repo",
            "rev1",
        )
        .expect("python graph should parse");

        let specifiers: Vec<&String> = parsed
            .imports
            .iter()
            .flat_map(|edge| edge.module_specifiers.iter())
            .collect();
        assert!(
            specifiers.iter().any(|spec| spec.as_str() == ".agents"),
            "expected dotted-string \".agents\" to be captured: {:?}",
            specifiers,
        );
        assert!(
            specifiers
                .iter()
                .any(|spec| spec.as_str() == "..agents.router"),
            "expected dotted-string \"..agents.router\" to be captured: {:?}",
            specifiers,
        );
    }

    #[test]
    fn regular_python_import_strings_are_not_double_counted() {
        // The string `"os"` inside `import os` should NOT trigger a synthetic
        // string-literal import — the existing path already records it.
        let source = r#"
from pathlib import Path
import os
"#;
        let parsed = parse_file_graph(
            "scripts/example.py",
            SourceLanguage::Python,
            source,
            "repo",
            "rev1",
        )
        .expect("python graph should parse");

        // Two real imports, zero synthetic.
        assert_eq!(parsed.imports.len(), 2);
        assert!(
            parsed
                .imports
                .iter()
                .all(|edge| edge.syntax_kind != "string_literal_import"),
            "regular import strings should not be double-counted: {:?}",
            parsed.imports,
        );
    }

    #[test]
    fn python_string_module_spec_rejects_non_module_strings() {
        // String literals that look like file paths or arbitrary content must
        // not be promoted to imports.
        let source = r#"
LABEL = "not.a.module.spec.txt"
COMMENT = "fox.jumps.over.dog"
NOT_MODULE = "/etc/passwd"
ABSOLUTE = "example.auth"
RELATIVE = ".admin"
"#;
        let parsed = parse_file_graph(
            "scripts/example.py",
            SourceLanguage::Python,
            source,
            "repo",
            "rev1",
        )
        .expect("python graph should parse");

        let specifiers: Vec<&String> = parsed
            .imports
            .iter()
            .flat_map(|edge| edge.module_specifiers.iter())
            .collect();
        assert!(
            specifiers.iter().any(|spec| spec.as_str() == ".admin"),
            "relative dotted spec must be captured: {:?}",
            specifiers,
        );
        for forbidden in [
            "not.a.module.spec.txt",
            "fox.jumps.over.dog",
            "/etc/passwd",
            "example.auth",
        ] {
            assert!(
                specifiers.iter().all(|spec| spec.as_str() != forbidden),
                "string `{forbidden}` should not be captured as an import: {:?}",
                specifiers,
            );
        }
    }

    #[test]
    fn ts_dynamic_import_strings_are_captured() {
        let source = r#"
const mod = await import("./agents/router");
const named = import("../config/settings");
const aliased = import("@/lib/auth");
const bare = import("react");
"#;
        let parsed = parse_file_graph(
            "ops-console/src/lazy.ts",
            SourceLanguage::TypeScript,
            source,
            "repo",
            "rev1",
        )
        .expect("ts graph should parse");

        let specifiers: Vec<&String> = parsed
            .imports
            .iter()
            .flat_map(|edge| edge.module_specifiers.iter())
            .collect();
        assert!(
            specifiers
                .iter()
                .any(|spec| spec.as_str() == "./agents/router"),
            "expected dynamic import \"./agents/router\" to be captured: {:?}",
            specifiers,
        );
        assert!(
            specifiers
                .iter()
                .any(|spec| spec.as_str() == "../config/settings"),
            "expected dynamic import \"../config/settings\" to be captured: {:?}",
            specifiers,
        );
        assert!(
            specifiers.iter().any(|spec| spec.as_str() == "@/lib/auth"),
            "expected aliased dynamic import \"@/lib/auth\" to be captured: {:?}",
            specifiers,
        );
        assert!(
            specifiers.iter().all(|spec| spec.as_str() != "react"),
            "bare specifier `react` is not a path import; should not be captured: {:?}",
            specifiers,
        );
    }

    // WHY: the default namespace is a published contract — existing Example
    // exports and downstream SPARQL must keep resolving without the env var.
    #[test]
    fn code_namespace_defaults_without_override() {
        assert_eq!(
            crate::config::resolve_code_namespace(None),
            crate::config::DEFAULT_CODE_RDF_NAMESPACE
        );
        assert_eq!(
            crate::config::resolve_code_namespace(Some("")),
            crate::config::DEFAULT_CODE_RDF_NAMESPACE
        );
        assert_eq!(
            crate::config::resolve_code_namespace(Some("   ")),
            crate::config::DEFAULT_CODE_RDF_NAMESPACE
        );
    }

    // WHY: overrides must yield well-formed term IRIs whether or not the
    // caller remembered the trailing separator.
    #[test]
    fn code_namespace_override_normalizes_trailing_separator() {
        assert_eq!(
            crate::config::resolve_code_namespace(Some("https://acme.test/code")),
            "https://acme.test/code#"
        );
        assert_eq!(
            crate::config::resolve_code_namespace(Some("https://acme.test/code#")),
            "https://acme.test/code#"
        );
        assert_eq!(
            crate::config::resolve_code_namespace(Some("https://acme.test/code/")),
            "https://acme.test/code/"
        );
    }

    // WHY: exact (no-star) tsconfig paths map a full specifier to full target
    // paths — mis-shaping them as prefixes would corrupt resolution.
    #[test]
    fn ts_path_mapping_normalizes_exact_patterns() {
        let alias = normalize_ts_path_mapping("@config", &["./config/index.ts".to_string()], "web")
            .expect("exact mapping should normalize");
        assert_eq!(alias.alias_prefix, "@config");
        assert!(!alias.wildcard);
        assert_eq!(alias.targets, vec!["web/./config/index.ts".to_string()]);
    }

    // WHY: single-trailing-star patterns are the dominant tsconfig shape;
    // the prefix split must keep the separator so `@/x` maps under `src/`.
    #[test]
    fn ts_path_mapping_normalizes_trailing_star_patterns() {
        let alias = normalize_ts_path_mapping(
            "@/*",
            &["./src/*".to_string(), "../shared/*".to_string()],
            "web",
        )
        .expect("wildcard mapping should normalize");
        assert_eq!(alias.alias_prefix, "@/");
        assert!(alias.wildcard);
        assert_eq!(
            alias.targets,
            vec!["web/./src/".to_string(), "web/../shared/".to_string()]
        );
    }

    // WHY: unsupported pattern shapes must be ignored (not mis-resolved) so
    // exotic tsconfigs degrade to the legacy fallback instead of bad edges.
    #[test]
    fn ts_path_mapping_ignores_unsupported_patterns() {
        // Inner star.
        assert_eq!(
            normalize_ts_path_mapping("@app/*/internal", &["./src/*".to_string()], "web"),
            None
        );
        // Multiple stars.
        assert_eq!(
            normalize_ts_path_mapping("@app/*/*", &["./src/*".to_string()], "web"),
            None
        );
        // Wildcard pattern whose only target lacks a trailing star.
        assert_eq!(
            normalize_ts_path_mapping("@/*", &["./src/literal.ts".to_string()], "web"),
            None
        );
        // Exact pattern whose only target carries a star.
        assert_eq!(
            normalize_ts_path_mapping("@config", &["./src/*".to_string()], "web"),
            None
        );
    }

    // WHY: longest-prefix-first ordering is what lets `@app/inner/*` shadow
    // `@app/*`; regressing to declaration order would flip resolutions.
    #[test]
    fn alias_table_prefers_longest_matching_prefix() {
        let mut indexed_paths = BTreeSet::new();
        indexed_paths.insert("web/inner/widget.ts".to_string());
        indexed_paths.insert("web/src/app/inner/widget.ts".to_string());
        let context = ImportResolverContext {
            indexed_paths,
            rust_crate_roots: BTreeMap::new(),
            ts_alias_tables: BTreeMap::new(),
        };
        // Sorted longest alias_prefix first, as load_tsconfig_aliases produces.
        let aliases = vec![
            TsPathAlias {
                alias_prefix: "@app/inner/".to_string(),
                wildcard: true,
                targets: vec!["web/inner/".to_string()],
            },
            TsPathAlias {
                alias_prefix: "@app/".to_string(),
                wildcard: true,
                targets: vec!["web/src/app/".to_string()],
            },
        ];

        let resolved = resolve_with_alias_table(&context, &aliases, "@app/inner/widget");

        assert_eq!(resolved, vec!["web/inner/widget.ts".to_string()]);
    }

    // WHY: a tsconfig alias must outrank the legacy hardcoded `@/` table,
    // otherwise non-Example repos keep resolving through Example app roots.
    #[test]
    fn ts_resolver_prefers_tsconfig_aliases_over_hardcoded_rules() {
        let mut indexed_paths = BTreeSet::new();
        indexed_paths.insert("web/source/lib/auth.ts".to_string());
        let mut ts_alias_tables = BTreeMap::new();
        ts_alias_tables.insert(
            "web".to_string(),
            vec![TsPathAlias {
                alias_prefix: "@/".to_string(),
                wildcard: true,
                targets: vec!["web/./source/".to_string()],
            }],
        );
        let context = ImportResolverContext {
            indexed_paths,
            rust_crate_roots: BTreeMap::new(),
            ts_alias_tables,
        };

        let resolved = resolve_ts_import_specifier(&context, "web/app/page.ts", "@/lib/auth");

        assert_eq!(resolved, vec!["web/source/lib/auth.ts".to_string()]);
    }

    // WHY: when no tsconfig alias matches, the legacy hardcoded table must
    // still answer — this is the Example-workspace no-regression guarantee.
    #[test]
    fn ts_resolver_falls_back_to_hardcoded_rules_without_tsconfig_hits() {
        let mut indexed_paths = BTreeSet::new();
        indexed_paths.insert("packages/trpc/src/router.ts".to_string());
        let context = ImportResolverContext {
            indexed_paths,
            rust_crate_roots: BTreeMap::new(),
            ts_alias_tables: BTreeMap::new(),
        };

        let resolved =
            resolve_ts_import_specifier(&context, "example-ops/src/page.ts", "@jai/trpc/router");

        assert_eq!(resolved, vec!["packages/trpc/src/router.ts".to_string()]);
    }
}
