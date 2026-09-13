//! Walks the repo with `ignore`, extracts symbols / env vars / Redis keys / deploy hints into
//! [`RepoIndex`](crate::model::RepoIndex). Each [`FileRecord`](crate::model::FileRecord) keeps
//! `modified_unix_ms` for context-bundle ranking (recency tie-break). Bump `INDEX_VERSION` when
//! the serialized index schema changes.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use regex::Regex;
use time::OffsetDateTime;
use tree_sitter::{Language, Node, Parser};

use crate::config::load_repo_config;
use crate::model::{
    AccessKind, CrossLanguageGraph, DeclaredVar, DeployTargetRecord, EnvFileRecord,
    EnvVarOccurrence, FileRecord, IndexSummary, K8sConfigMapRecord, ProfileRecord,
    RedisKeyOccurrence, RepoIndex, SecretSetRecord, SourceLanguage, SymbolKind, SymbolOccurrence,
};
use crate::search;

// Bumped 6 → 7 in P0 #2 Phases 1/1b/4: JS/Rust/Makefile/npm detectors land +
// `SourceLanguage` gained `Make` and `NpmScript` variants. Older indexes
// can't round-trip those variants.
// Bumped 7 → 8 in P0 #2 Phase 2: `CrossLanguageGraph` added to `RepoIndex`.
// Bumped 8 → 9 in P0 #2 Phase 5: routes + http_calls + resolved_http_edges.
// Bumped 9 → 10 in P0 #2 Phase 3: `UnresolvedEdge` added to `FileRecord` and
// aggregated on `CrossLanguageGraph.unresolved_edges`.
// Bumped 10 → 11 in P0 #2 Phase 6: `RouteRecord.path_params`,
// `HttpCallOccurrence.is_template`, and `ResolvedHttpEdge.match_kind`.
// All three fields are `#[serde(default)]`-additive, so older indexes still
// deserialize (existing edges become `MatchKind::Literal`), but the
// schema_version semantics in `docs/output-schema.md` §7 require the bump.
// Bumped 11 → 12 in P0 #2 Phase 8: light dataflow.
// - `HttpCallOccurrence.resolved_via_dataflow: bool` (additive)
// - `SubprocessCallOccurrence.resolved_via_dataflow: bool` (additive)
// - `MatchKind::DataflowLiteral` (80) and `MatchKind::DataflowTemplate` (75)
// - `UnresolvedReason::AmbiguousAssignment`
// All additive, so older indexes still deserialize, but the new bands and
// dataflow flags would silently drop on a v11 reader.
// Bumped 12 → 13: `SourceLanguage::Rdf` and `SymbolKind::Property` so
// `.ttl` / `.owl` / `.jsonld` ontologies round-trip as first-class records.
// Bumped 15 → 16: Java, Kotlin, HTML, CSS, and Swift source-language variants.
// Bumped 18 → 19: the Python `os.getenv` access pattern now matches the
// `os.getenv("VAR", default)` two-argument form (previously only the bare
// single-argument call matched). This is an extraction change, not a schema
// change, but unchanged files are served from the per-version cache without
// re-running `extract_env_vars`, so the version must bump to force a
// re-extract of cached Python files.
const INDEX_VERSION: u32 = 19;

/// Public accessor for the indexer's schema version. Used by report-rendering
/// callers (e.g. `diagnostics::RunMeta`) so they don't need a `pub` constant.
pub fn index_version() -> u32 {
    INDEX_VERSION
}

static ENV_ACCESS_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r#"process\.env\.([A-Z][A-Z0-9_]+)"#).expect("valid regex"),
        Regex::new(r#"process\.env\[\s*["']([A-Z][A-Z0-9_]+)["']\s*\]"#).expect("valid regex"),
        Regex::new(r#"std::env::(?:var|var_os|set_var)\(["']([A-Z][A-Z0-9_]+)["']\)"#)
            .expect("valid regex"),
        Regex::new(r#"env::(?:var|var_os|set_var)\(["']([A-Z][A-Z0-9_]+)["']\)"#)
            .expect("valid regex"),
        Regex::new(r#"os\.getenv\(\s*["']([A-Z][A-Z0-9_]+)["'](?:\s*,\s*[^)]*)?\)"#)
            .expect("valid regex"),
        Regex::new(r#"os\.environ(?:\.get)?\[\s*["']([A-Z][A-Z0-9_]+)["']\s*\]"#)
            .expect("valid regex"),
        Regex::new(r#"os\.environ\.get\(["']([A-Z][A-Z0-9_]+)["']\)"#).expect("valid regex"),
        Regex::new(r#"\$\{([A-Z][A-Z0-9_]+)(?::-[^}]*)?\}"#).expect("valid regex"),
    ]
});
static DECLARED_ENV_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^([A-Z][A-Z0-9_]+)\s*=\s*(.+)?$"#).expect("valid regex"));
static REDIS_LITERAL_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"["']([A-Za-z0-9_{}*-]+(?::[A-Za-z0-9_{}*.-]+)+)["']"#).expect("valid regex")
});
static DECLARED_VAR_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^([A-Z][A-Z0-9_]+)\s*=\s*(.*)$"#).expect("valid regex"));

pub fn default_index_path(root: &Path) -> PathBuf {
    root.join(".leio-code").join("index.json")
}

pub fn default_index_summary_path(root: &Path) -> PathBuf {
    root.join(".leio-code").join("index-summary.json")
}

pub fn build_index_summary(index: &RepoIndex) -> IndexSummary {
    IndexSummary {
        version: index.version,
        root: index.root.clone(),
        indexed_at: index.indexed_at.clone(),
        file_count: index.files.len(),
        workspace_facets: index.workspace_facets(),
        // Stamped by `stamp_index_file` after `index.json` is on disk; 0 here
        // means "not yet stamped" and reads as stale until the file stat is known.
        index_file_bytes: 0,
        index_file_modified_ms: 0,
        truncated: false,
    }
}

/// Stamp a freshly-built summary with the on-disk `index.json` stat so freshness
/// can be checked by exact stat-equality (deterministic — no mtime-ordering race
/// under coarse filesystem granularity). Call after `index.json` has been written.
pub fn stamp_index_file(mut summary: IndexSummary, index_path: &Path) -> IndexSummary {
    if let Ok(meta) = fs::metadata(index_path) {
        summary.index_file_bytes = meta.len();
        summary.index_file_modified_ms = system_time_to_unix_ms(meta.modified().ok());
    }
    summary
}

/// Load the sidecar summary when it is at least as fresh as `index.json`.
pub fn load_fresh_index_summary(root: &Path, index_path: &Path) -> Option<IndexSummary> {
    let summary_path = default_index_summary_path(root);
    let summary = load_index_summary(&summary_path).ok()?;
    if summary.version != INDEX_VERSION {
        return None;
    }
    // Freshness by exact stat-equality against the index the summary was stamped
    // from. Unlike `summary_mtime < index_mtime` ordering, equality of a recorded
    // value is immune to coarse filesystem mtime granularity (the source of the
    // `index_summary_roundtrip_and_freshness_gate` CI flake), and comparing
    // `size` catches same-tick content changes that mtime alone would miss —
    // mirroring the per-file reuse heuristic in `build_or_update_index`.
    let index_meta = fs::metadata(index_path).ok()?;
    if summary.index_file_bytes != index_meta.len()
        || summary.index_file_modified_ms != system_time_to_unix_ms(index_meta.modified().ok())
    {
        return None;
    }
    // An index with zero files is not a code view, however fresh its stamp.
    // It happens when language coverage misses everything in the tree, and a
    // caller that trusts it reports full coverage of nothing. Enforcing this
    // here rather than in one consumer keeps CLI, MCP, status and the harness
    // on the same answer — the harness used to special-case it alone, so the
    // other three still said "fresh".
    if summary.file_count == 0 {
        return None;
    }
    Some(summary)
}

pub fn load_index_summary(summary_path: &Path) -> Result<IndexSummary> {
    let raw = fs::read(summary_path)
        .with_context(|| format!("failed to read index summary {}", summary_path.display()))?;
    let summary: IndexSummary = serde_json::from_slice(&raw)
        .with_context(|| format!("failed to parse index summary {}", summary_path.display()))?;
    Ok(summary)
}

pub fn save_index_summary(summary_path: &Path, summary: &IndexSummary) -> Result<()> {
    if let Some(parent) = summary_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    crate::sidecar::write_atomic_json(summary_path, summary)
        .with_context(|| format!("failed to write {}", summary_path.display()))?;
    Ok(())
}

/// Default freshness window for `load_or_build_index` when `LEIO_INDEX_TTL_SECS` is unset.
///
/// Five seconds made every CLI invocation more than 5s apart pay a full
/// reindex (~1.2s on the example workspace, ~100ms on leio-code). Five
/// minutes keeps lookups cheap while still catching new files in a
/// reasonable window; `leio-code watch` handles continuous reindexing, and
/// `LEIO_INDEX_TTL_SECS=0` forces an always-rebuild policy.
const DEFAULT_INDEX_TTL_SECS: u64 = 300;

/// Hard cap on indexed files when `LEIO_MAX_INDEX_FILES` is unset.
///
/// 80_000 source files is already a huge monorepo; JSON + tree-sitter beyond
/// that blows RAM and MCP timeouts. Set `LEIO_MAX_INDEX_FILES=0` for no cap.
const DEFAULT_MAX_INDEX_FILES: usize = 80_000;

/// Pretty-print `index.json` only when the tree is this small.
///
/// Pretty JSON is for humans debugging a crate. Compact JSON cuts write time
/// and the torn-write window on a monorepo.
const PRETTY_INDEX_FILE_LIMIT: usize = 2_000;

/// Load the index without rebuilding when its mtime is within the freshness TTL.
///
/// Falls back to `build_or_update_index` if the index is missing, stale, or unreadable.
/// The TTL can be overridden via `LEIO_INDEX_TTL_SECS` (set to `0` to always rebuild).
/// Concurrent agents share an advisory lock so only one walk runs at a time.
pub fn load_or_build_index(root: &Path, index_path: &Path) -> Result<RepoIndex> {
    if let Some(index) = load_fresh_index(root, index_path) {
        return Ok(index);
    }
    let _lock = crate::sidecar::acquire_lock(root, "index")?;
    if let Some(index) = load_fresh_index(root, index_path) {
        return Ok(index);
    }
    build_or_update_index(root, index_path, false)
}

fn load_fresh_index(root: &Path, index_path: &Path) -> Option<RepoIndex> {
    let ttl = index_ttl_secs();
    if ttl == 0 {
        return None;
    }
    let metadata = fs::metadata(index_path).ok()?;
    let modified = metadata.modified().ok()?;
    let age = SystemTime::now().duration_since(modified).ok()?;
    if age.as_secs() > ttl {
        return None;
    }
    let index = load_index(index_path).ok()?;
    if index.version != INDEX_VERSION {
        return None;
    }
    if is_index_stale_against_disk(root, &index, modified) {
        return None;
    }
    Some(index)
}

/// Detects whether tracked files or git working tree have changed since `index.json` was written.
fn is_index_stale_against_disk(root: &Path, index: &RepoIndex, index_modified: SystemTime) -> bool {
    let git_index = root.join(".git").join("index");
    if let Ok(meta) = fs::metadata(&git_index)
        && let Ok(m) = meta.modified()
        && m > index_modified
    {
        return true;
    }

    // Check up to 256 indexed files for newer mtime or deletion
    for file in index.files.iter().take(256) {
        let path = root.join(&file.path);
        match fs::metadata(&path) {
            Ok(meta) => {
                if let Ok(m) = meta.modified()
                    && m > index_modified
                {
                    return true;
                }
            }
            Err(_) => {
                // File in index was removed from disk
                return true;
            }
        }
    }
    false
}

fn index_ttl_secs() -> u64 {
    std::env::var("LEIO_INDEX_TTL_SECS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or_else(|| {
            if std::env::var("LEIO_SESSION").is_ok() {
                5
            } else {
                DEFAULT_INDEX_TTL_SECS
            }
        })
}

/// Build or refresh the JSON index. Honors `LEIO_REUSE_INDEX=1` (or `true`): if an on-disk
/// index exists with matching `root` and `version`, returns it without walking the tree
/// (for CI cache restore). Otherwise reuses unchanged file records by mtime+size.
///
/// When `quiet` is true, skips stderr progress lines (used by `watch --quiet`).
pub fn build_or_update_index(root: &Path, index_path: &Path, quiet: bool) -> Result<RepoIndex> {
    let _lock = crate::sidecar::acquire_lock(root, "index")?;
    if std::env::var("LEIO_REUSE_INDEX")
        .ok()
        .as_deref()
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        && let Ok(index) = load_index(index_path)
        && index.version == INDEX_VERSION
    {
        let root_str = root.display().to_string();
        if index.root == root_str {
            return Ok(index);
        }
    }

    let started = Instant::now();
    let previous = load_index(index_path)
        .ok()
        .filter(|index| index.version == INDEX_VERSION);
    let mut previous_files = HashMap::new();
    if let Some(index) = &previous {
        for file in &index.files {
            previous_files.insert(file.path.clone(), file.clone());
        }
    }

    let mut files = Vec::new();
    let mut routes: Vec<crate::model::RouteRecord> = Vec::new();
    let discovered = discover_source_files(root)?;
    if discovered.truncated && !quiet {
        eprintln!(
            "index truncated at {} files (set LEIO_MAX_INDEX_FILES or pin --repo to a package)",
            discovered.paths.len()
        );
    }
    if !quiet {
        for warning in &discovered.unreachable_include_roots {
            eprintln!("{warning}");
        }
    }
    for path in discovered.paths {
        let rel = relative_path(root, &path);
        let metadata =
            fs::metadata(&path).with_context(|| format!("failed to stat {}", path.display()))?;
        if should_skip_large_file(&path, metadata.len()) {
            continue;
        }
        let modified_unix_ms = system_time_to_unix_ms(metadata.modified().ok());
        let bytes = metadata.len() as usize;

        if let Some(existing) = previous_files.get(&rel)
            && existing.modified_unix_ms == modified_unix_ms
            && existing.bytes == bytes
        {
            // Re-extract routes from cached files; route declarations are not
            // stored per-FileRecord, only in the graph, so we need to rebuild
            // them on every index. Reading the file from disk is the simplest
            // path that avoids carrying routes inside FileRecord.
            if matches!(
                existing.language,
                SourceLanguage::Python
                    | SourceLanguage::JavaScript
                    | SourceLanguage::TypeScript
                    | SourceLanguage::Tsx
                    | SourceLanguage::CSharp
                    | SourceLanguage::Razor
                    | SourceLanguage::Rust
            ) && let Ok(content) = fs::read_to_string(&path)
            {
                routes.extend(extract_routes_for_file(&rel, existing.language, &content));
            }
            files.push(existing.clone());
            continue;
        }

        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let language = detect_language(&path);
        let symbols = extract_symbols(&rel, language, &content);
        let env_vars = extract_env_vars(&rel, language, &content);
        let redis_keys = extract_redis_keys(&rel, language, &content);
        let detector_out = match language {
            SourceLanguage::Python => {
                crate::cross_language::detect_python_subprocess_calls(&content, &rel)
            }
            SourceLanguage::JavaScript
            | SourceLanguage::TypeScript
            | SourceLanguage::Tsx
            | SourceLanguage::CSharp
            | SourceLanguage::Razor => {
                crate::cross_language::detect_js_subprocess_calls(&content, &rel, language)
            }
            SourceLanguage::Rust => {
                crate::cross_language::detect_rust_process_commands(&content, &rel)
            }
            SourceLanguage::Make => {
                crate::cross_language::detect_makefile_invocations(&rel, &content)
            }
            SourceLanguage::NpmScript => {
                crate::cross_language::detect_npm_script_invocations(&rel, &content)
            }
            _ => crate::cross_language::DetectorOutput::default(),
        };

        let http_out = match language {
            SourceLanguage::Python => {
                crate::cross_language::detect_python_http_calls(&content, &rel)
            }
            SourceLanguage::JavaScript
            | SourceLanguage::TypeScript
            | SourceLanguage::Tsx
            | SourceLanguage::CSharp
            | SourceLanguage::Razor => {
                crate::cross_language::detect_js_http_calls(&content, &rel, language)
            }
            SourceLanguage::Rust => crate::cross_language::detect_rust_http_calls(&content, &rel),
            _ => crate::cross_language::HttpDetectorOutput::default(),
        };

        routes.extend(extract_routes_for_file(&rel, language, &content));

        // Phase 8: HTTP detectors may emit unresolved edges (light dataflow
        // failure cases). Merge them with the subprocess unresolved stream.
        let mut combined_unresolved = detector_out.unresolved;
        combined_unresolved.extend(http_out.unresolved);

        files.push(FileRecord {
            path: rel,
            language,
            bytes,
            modified_unix_ms,
            symbols,
            env_vars,
            redis_keys,
            subprocess_calls: detector_out.spawns,
            http_calls: http_out.http_calls,
            unresolved_edges: combined_unresolved,
        });
    }

    let deploy_targets = parse_deploy_targets(root)?;
    let profiles = parse_env_records(&root.join("deploy/profiles"))?;
    let secret_sets = parse_env_records_secrets(&root.join("deploy/secret-sets"))?;
    let env_files = parse_root_env_files(root)?;

    let binaries = crate::cross_language::collect_binary_nodes(root);
    let resolved_spawns = crate::cross_language::resolve_spawn_edges(&files, &binaries);
    let routes = crate::cross_language::mount_cross_file_axum_nests(root, &files, routes);
    let resolved_http_edges = crate::cross_language::resolve_http_edges(&files, &routes);
    let unresolved_edges: Vec<_> = files
        .iter()
        .flat_map(|f| f.unresolved_edges.iter().cloned())
        .collect();
    let cross_language = CrossLanguageGraph {
        binaries,
        resolved_spawns,
        routes,
        resolved_http_edges,
        unresolved_edges,
    };

    // Collect Kubernetes ConfigMaps from YAML files already read during the walk.
    let mut k8s_configmaps = Vec::new();
    for path in discover_source_files(root)?.paths {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        if !matches!(ext, "yaml" | "yml") {
            continue;
        }
        let rel = relative_path(root, &path);
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        if let Some(record) = detect_k8s_configmap(&rel, &content) {
            k8s_configmaps.push(record);
        }
    }

    let index = RepoIndex {
        version: INDEX_VERSION,
        root: root.display().to_string(),
        indexed_at: OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .context("failed to format timestamp")?,
        files,
        deploy_targets,
        profiles,
        secret_sets,
        env_files,
        cross_language,
        k8s_configmaps,
    };

    save_index_with_truncation(index_path, &index, discovered.truncated)?;
    if !quiet {
        eprintln!(
            "indexed {} files in {} ms -> {}",
            index.files.len(),
            started.elapsed().as_millis(),
            index_path.display()
        );
    }

    // Best-effort: rebuild the Arrow IPC search sidecar alongside the JSON
    // index. Failures here never abort indexing — query.rs falls back to linear
    // scan when the sidecar is absent or stale.
    if !std::env::var("LEIO_DISABLE_SEARCH_SIDECAR")
        .ok()
        .as_deref()
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        && let Some(stats) = search::rebuild_sidecar(root, &index)
        && !quiet
    {
        eprintln!(
            "search sidecar: {} symbols / {} env vars / {} redis keys in {} ms",
            stats.symbols, stats.env_vars, stats.redis_keys, stats.elapsed_ms,
        );
    }

    Ok(index)
}

pub fn load_index(index_path: &Path) -> Result<RepoIndex> {
    let raw = fs::read(index_path)
        .with_context(|| format!("failed to read index {}", index_path.display()))?;
    let index: RepoIndex = serde_json::from_slice(&raw)
        .with_context(|| format!("failed to parse index {}", index_path.display()))?;
    Ok(index)
}

pub fn save_index(index_path: &Path, index: &RepoIndex) -> Result<()> {
    save_index_with_truncation(index_path, index, false)
}

fn save_index_with_truncation(index_path: &Path, index: &RepoIndex, truncated: bool) -> Result<()> {
    if let Some(parent) = index_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let raw = if index.files.len() > PRETTY_INDEX_FILE_LIMIT {
        serde_json::to_vec(index).context("failed to serialize index")?
    } else {
        serde_json::to_string_pretty(index)
            .context("failed to serialize index")?
            .into_bytes()
    };
    crate::sidecar::write_atomic(index_path, &raw)
        .with_context(|| format!("failed to write {}", index_path.display()))?;
    let summary_path = default_index_summary_path(Path::new(&index.root));
    let mut summary = stamp_index_file(build_index_summary(index), index_path);
    summary.truncated = truncated;
    save_index_summary(&summary_path, &summary)?;
    Ok(())
}

struct DiscoveredSources {
    paths: Vec<PathBuf>,
    truncated: bool,
    /// Configured `include_roots` entries that the walk can never reach.
    unreachable_include_roots: Vec<String>,
}

fn discover_source_files(root: &Path) -> Result<DiscoveredSources> {
    let config = load_repo_config(root).unwrap_or_default();
    let filter_config = config.clone();
    let include_roots = config
        .include_roots
        .as_ref()
        .filter(|items| !items.is_empty())
        .map(|items| items.iter().cloned().collect::<HashSet<_>>());
    let unreachable_include_roots =
        unreachable_include_roots(root, config.include_roots.as_deref());
    let mut builder = WalkBuilder::new(root);
    builder.hidden(false);
    builder.git_ignore(true);
    builder.git_global(true);
    builder.git_exclude(true);
    let walk_threads = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(4)
        .clamp(1, 8);
    builder.threads(walk_threads);
    builder.filter_entry(move |entry| {
        let Some(name) = entry.file_name().to_str() else {
            return true;
        };
        if entry.path().is_dir() {
            if is_excluded_dir(name, &filter_config) {
                return false;
            }
            // Agent harnesses nest linked git worktrees inside the repo
            // (`.claude/worktrees/<id>/`, and leio-harness lanes when their
            // root is in-tree). Each is a full second checkout of the same
            // files: indexing them duplicates every symbol per worktree, so
            // dead-code and graph queries report N phantom copies of every
            // real finding. Git itself does not descend into them.
            return entry.depth() == 0 || !is_linked_git_worktree(entry.path());
        }
        true
    });

    let cap = max_index_files();
    let mut paths = Vec::new();
    let mut truncated = false;
    for dent in builder.build() {
        let dent = dent?;
        let path = dent.path();
        if !path.is_file() {
            continue;
        }
        if is_deprioritized_by_include_roots(root, path, include_roots.as_ref()) {
            continue;
        }
        if should_index_file(path) {
            if cap > 0 && paths.len() >= cap {
                truncated = true;
                break;
            }
            paths.push(path.to_path_buf());
        }
    }
    Ok(DiscoveredSources {
        paths,
        truncated,
        unreachable_include_roots,
    })
}

/// `include_roots` entries the walk cannot reach, so a stale one stops being a
/// silent no-op. The walker does not follow symlinks (following them would
/// duplicate every symbol reachable by two paths), so a symlinked entry
/// contributes nothing while still reading as configured coverage.
fn unreachable_include_roots(root: &Path, include_roots: Option<&[String]>) -> Vec<String> {
    let Some(include_roots) = include_roots else {
        return Vec::new();
    };
    let mut unreachable = Vec::new();
    for entry in include_roots {
        let path = root.join(entry);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => unreachable.push(format!(
                "include_roots entry `{entry}` is a symlink; the walk does not follow symlinks, so nothing under it is indexed. Index that tree on its own root instead"
            )),
            Ok(metadata) if !metadata.is_dir() => unreachable.push(format!(
                "include_roots entry `{entry}` is not a directory; nothing under it is indexed"
            )),
            Ok(_) => {}
            Err(_) => unreachable.push(format!(
                "include_roots entry `{entry}` does not exist; nothing under it is indexed"
            )),
        }
    }
    unreachable
}

fn max_index_files() -> usize {
    match std::env::var("LEIO_MAX_INDEX_FILES") {
        Ok(raw) => raw.trim().parse().unwrap_or(DEFAULT_MAX_INDEX_FILES),
        Err(_) => DEFAULT_MAX_INDEX_FILES,
    }
}

fn should_index_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    if matches!(
        name,
        ".env"
            | ".env.example"
            | ".env.local"
            | "Dockerfile"
            | "Makefile"
            | "makefile"
            | "GNUmakefile"
    ) {
        return true;
    }

    detect_language(path) != SourceLanguage::Text
        || path.extension().and_then(|value| value.to_str()) == Some("fam")
}

fn should_skip_large_file(path: &Path, size: u64) -> bool {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    match extension {
        "json" => size > 512 * 1024,
        "yaml" | "yml" | "toml" => size > 256 * 1024,
        // Same cap as `knowledge_graph::MAX_RDF_BYTES` so a repo ontology that
        // the formal store loads is not silently dropped from `find symbol`.
        "ttl" | "owl" | "jsonld" | "rdf" | "nt" | "nq" | "trig" | "n3" => size > 8 * 1024 * 1024,
        _ => size > 2 * 1024 * 1024,
    }
}

fn is_deprioritized_by_include_roots(
    root: &Path,
    path: &Path,
    include_roots: Option<&HashSet<String>>,
) -> bool {
    let Some(include_roots) = include_roots else {
        return false;
    };
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let mut components = relative.components();
    let Some(first) = components.next() else {
        return false;
    };
    let Some(first) = first.as_os_str().to_str() else {
        return false;
    };
    if relative.parent().is_none() {
        return false;
    }
    !include_roots.contains(first)
}

fn is_excluded_dir(name: &str, config: &crate::config::LeioConfig) -> bool {
    let configured = config
        .exclude_dirs
        .as_ref()
        .filter(|items| !items.is_empty())
        .map(|items| items.iter().cloned().collect::<HashSet<_>>());
    let excluded = configured.unwrap_or_else(default_exclude_dirs);
    excluded.contains(name)
}

/// True when `dir` is a *linked git worktree* root: its `.git` is a file
/// pointing at `<main-repo>/.git/worktrees/<name>`. Submodules also use a
/// `.git` file, but point at `.git/modules/<name>` — those hold different code
/// and stay indexed, so this check is deliberately narrower than "has a .git".
fn is_linked_git_worktree(dir: &Path) -> bool {
    let git_path = dir.join(".git");
    if !git_path.is_file() {
        return false;
    }
    let Ok(raw) = fs::read_to_string(&git_path) else {
        return false;
    };
    let Some(gitdir) = raw.trim().strip_prefix("gitdir:") else {
        return false;
    };
    let gitdir = gitdir.trim().replace('\\', "/");
    gitdir.contains("/.git/worktrees/")
}

fn default_exclude_dirs() -> HashSet<String> {
    [
        ".git",
        ".leio-code",
        "node_modules",
        "target",
        ".next",
        ".nuxt",
        "dist",
        "build",
        ".turbo",
        ".pnpm-store",
        ".yarn",
        ".venv",
        "venv",
        "__pycache__",
        "coverage",
        "third_party",
        ".gradle",
        "Pods",
        "backup",
        "downloads",
        "output",
        "site",
        "temp_ocr",
        "wheels",
        "models",
        "tmp",
        "projects",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn detect_language(path: &Path) -> SourceLanguage {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if matches!(name, ".env" | ".env.example" | ".env.local") {
        return SourceLanguage::Env;
    }
    if name == "Dockerfile" {
        return SourceLanguage::Text;
    }
    if matches!(name, "Makefile" | "makefile" | "GNUmakefile") {
        return SourceLanguage::Make;
    }
    if name == "package.json" {
        return SourceLanguage::NpmScript;
    }
    match path.extension().and_then(|value| value.to_str()) {
        Some("rs") => SourceLanguage::Rust,
        Some("py") => SourceLanguage::Python,
        Some("ts") => SourceLanguage::TypeScript,
        Some("tsx") => SourceLanguage::Tsx,
        Some("cs" | "csx") => SourceLanguage::CSharp,
        Some("razor" | "cshtml") => SourceLanguage::Razor,
        Some("go") => SourceLanguage::Go,
        Some("c") => SourceLanguage::C,
        Some("h") => SourceLanguage::C,
        Some("cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx") => SourceLanguage::Cpp,
        Some("sh" | "bash") => SourceLanguage::Bash,
        Some("java") => SourceLanguage::Java,
        Some("kt" | "kts") => SourceLanguage::Kotlin,
        Some("html" | "htm") => SourceLanguage::Html,
        Some("css") => SourceLanguage::Css,
        Some("swift") => SourceLanguage::Swift,
        Some("js" | "mjs") => SourceLanguage::JavaScript,
        Some("json") => SourceLanguage::Json,
        Some("toml") => SourceLanguage::Toml,
        Some("yaml" | "yml") => SourceLanguage::Yaml,
        Some("sql") => SourceLanguage::Sql,
        Some("env") => SourceLanguage::Env,
        Some("ttl" | "owl" | "jsonld" | "rdf" | "nt" | "nq" | "trig" | "n3") => SourceLanguage::Rdf,
        _ => SourceLanguage::Text,
    }
}

/// Swift UI/app sources (JAI Team). tree-sitter-swift is not a workspace
/// crate; regex is enough for `find symbol` / empty-index integrity.
fn extract_swift_symbols(path: &str, source: &str) -> Vec<SymbolOccurrence> {
    static DECL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?m)^[ \t]*(?:(?:public|private|internal|fileprivate|open|package)\s+)*(?:(?:static|class|final|indirect|override)\s+)*(func|struct|enum|class|actor|protocol|extension)\s+([A-Za-z_][A-Za-z0-9_]*)",
        )
        .expect("swift decl")
    });
    let mut out = Vec::new();
    for cap in DECL.captures_iter(source) {
        let kind = match cap.get(1).map(|m| m.as_str()).unwrap_or("") {
            "func" => SymbolKind::Function,
            "struct" => SymbolKind::Struct,
            "enum" => SymbolKind::Enum,
            "class" | "actor" => SymbolKind::Class,
            "protocol" => SymbolKind::Interface,
            "extension" => SymbolKind::TypeAlias,
            _ => continue,
        };
        let name = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let line = source[..cap.get(0).map(|m| m.start()).unwrap_or(0)]
            .bytes()
            .filter(|b| *b == b'\n')
            .count()
            + 1;
        out.push(SymbolOccurrence {
            name: name.to_string(),
            kind,
            path: path.to_string(),
            line,
            language: SourceLanguage::Swift,
            qual_name: None,
        });
    }
    out
}

fn extract_symbols(path: &str, language: SourceLanguage, source: &str) -> Vec<SymbolOccurrence> {
    if language == SourceLanguage::Rdf || crate::ontology::is_rdf_path(Path::new(path)) {
        return crate::ontology::extract_ontology(path, source).symbols;
    }
    if language == SourceLanguage::Kotlin {
        return extract_kotlin_symbols(path, source);
    }
    if language == SourceLanguage::Sql {
        return extract_sql_symbols(path, source);
    }
    if language == SourceLanguage::Html {
        return extract_html_symbols(path, source);
    }
    if language == SourceLanguage::Css {
        return extract_css_symbols(path, source);
    }
    if path
        .rsplit('.')
        .next()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("swift"))
    {
        return extract_swift_symbols(path, source);
    }
    let Some(ts_language) = ts_language(language) else {
        return Vec::new();
    };

    let mut parser = Parser::new();
    if parser.set_language(&ts_language).is_err() {
        return Vec::new();
    }
    let parser_source = crate::parser_support::parser_source(language, source);
    let Some(tree) = parser.parse(parser_source.as_ref(), None) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    walk_symbols(
        tree.root_node(),
        parser_source.as_ref(),
        path,
        language,
        None,
        &mut out,
    );
    dedupe_symbols(out)
}

fn regex_symbols(
    path: &str,
    source: &str,
    language: SourceLanguage,
    pattern: &Regex,
) -> Vec<SymbolOccurrence> {
    pattern
        .captures_iter(source)
        .filter_map(|cap| {
            let kind = match cap.get(1)?.as_str() {
                "fun" | "function" => SymbolKind::Function,
                "class" | "object" => SymbolKind::Class,
                "interface" => SymbolKind::Interface,
                "table" | "view" | "procedure" => SymbolKind::Module,
                _ => SymbolKind::Variable,
            };
            let name = cap.get(2)?.as_str();
            let line = source[..cap.get(0)?.start()]
                .bytes()
                .filter(|b| *b == b'\n')
                .count()
                + 1;
            Some(SymbolOccurrence {
                name: name.to_string(),
                kind,
                path: path.to_string(),
                line,
                language,
                qual_name: None,
            })
        })
        .collect()
}

fn extract_kotlin_symbols(path: &str, source: &str) -> Vec<SymbolOccurrence> {
    static KOTLIN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?m)^\s*(?:public\s+|private\s+|internal\s+|open\s+|data\s+|sealed\s+|abstract\s+)*(fun|class|object|interface)\s+([A-Za-z_][A-Za-z0-9_]*)").expect("kotlin declarations")
    });
    regex_symbols(path, source, SourceLanguage::Kotlin, &KOTLIN)
}

fn extract_sql_symbols(path: &str, source: &str) -> Vec<SymbolOccurrence> {
    static SQL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\bcreate\s+(table|view|procedure|function)\s+(?:if\s+not\s+exists\s+)?([A-Za-z_][A-Za-z0-9_]*)").expect("sql declarations")
    });
    regex_symbols(path, source, SourceLanguage::Sql, &SQL)
}

fn extract_html_symbols(path: &str, source: &str) -> Vec<SymbolOccurrence> {
    static TAG: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"<([A-Za-z][A-Za-z0-9:-]*)").expect("html tags"));
    TAG.captures_iter(source)
        .filter_map(|cap| {
            let name = cap.get(1)?.as_str();
            let line = source[..cap.get(0)?.start()]
                .bytes()
                .filter(|b| *b == b'\n')
                .count()
                + 1;
            Some(SymbolOccurrence {
                name: name.to_string(),
                kind: SymbolKind::Class,
                path: path.to_string(),
                line,
                language: SourceLanguage::Html,
                qual_name: None,
            })
        })
        .collect()
}

fn extract_css_symbols(path: &str, source: &str) -> Vec<SymbolOccurrence> {
    static SELECTOR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?m)(?:^|\})\s*([.#][A-Za-z_][A-Za-z0-9_-]*)").expect("css selectors")
    });
    let mut scanned_until = 0;
    let mut line = 1;
    SELECTOR
        .captures_iter(source)
        .filter_map(|cap| {
            let selector = cap.get(1)?;
            // Matches arrive in source order: scan each byte at most once.
            // Use the selector, not the preceding brace/whitespace in the match.
            line += source[scanned_until..selector.start()]
                .bytes()
                .filter(|b| *b == b'\n')
                .count();
            scanned_until = selector.start();
            Some(SymbolOccurrence {
                name: selector.as_str().to_string(),
                kind: SymbolKind::Class,
                path: path.to_string(),
                line,
                language: SourceLanguage::Css,
                qual_name: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod css_line_tests {
    use super::extract_css_symbols;

    #[test]
    fn css_selector_lines_follow_multiline_and_minified_rules() {
        let source = "/* café */\r\n\r\n  .first {\r\n color: red;\r\n}\r\n\r\n  #second {}.third {}\r\n.fourth {}";
        let symbols = extract_css_symbols("styles.css", source);
        let locations: Vec<_> = symbols
            .iter()
            .map(|symbol| (symbol.name.as_str(), symbol.line))
            .collect();
        assert_eq!(
            locations,
            [(".first", 3), ("#second", 7), (".third", 7), (".fourth", 8)]
        );
    }

    #[test]
    fn css_selector_lines_handle_many_minified_rules() {
        let source = (0..10_000)
            .map(|index| format!(".selector_{index}{{color:red}}"))
            .collect::<String>();
        let symbols = extract_css_symbols("vendor.min.css", &source);
        assert_eq!(symbols.len(), 10_000);
        assert!(symbols.iter().all(|symbol| symbol.line == 1));
        assert_eq!(symbols[0].name, ".selector_0");
        assert_eq!(symbols[9_999].name, ".selector_9999");
    }
}

fn walk_symbols(
    node: Node<'_>,
    source: &str,
    path: &str,
    language: SourceLanguage,
    enclosing_type: Option<&str>,
    out: &mut Vec<SymbolOccurrence>,
) {
    if let Some(kind) = symbol_kind(language, node.kind())
        && let Some(name_node) = symbol_name_node(language, node)
        && let Ok(name) = name_node.utf8_text(source.as_bytes())
    {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            let (effective_kind, qual_name) =
                resolve_method_scope(language, kind, enclosing_type, trimmed);
            out.push(SymbolOccurrence {
                name: trimmed.to_string(),
                kind: effective_kind,
                path: path.to_string(),
                line: node.start_position().row + 1,
                language,
                qual_name,
            });
        }
    }

    // Function-like nodes open a fresh local scope — anything nested inside
    // their body (helper `fn`, inner `def`, closure-local helpers) belongs
    // to that body, not to the enclosing impl / class. Reset before
    // recursing so a Rust `impl Foo { fn bar() { fn helper() {} } }` gives
    // `helper` no qual_name.
    //
    // Otherwise `scope_type_name` returns a fresh type-bearing scope (impl /
    // class / trait); when it doesn't, descendants inherit the current one.
    let scoped = scope_type_name(language, node, source);
    let next_scope: Option<&str> = if opens_local_scope(language, node.kind()) {
        None
    } else {
        scoped.as_deref().or(enclosing_type)
    };

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_symbols(child, source, path, language, next_scope, out);
    }
}

fn symbol_name_node<'a>(language: SourceLanguage, node: Node<'a>) -> Option<Node<'a>> {
    if let Some(name) = node.child_by_field_name("name") {
        return Some(name);
    }
    if matches!(language, SourceLanguage::C | SourceLanguage::Cpp)
        && node.kind() == "function_definition"
    {
        let declarator = node.child_by_field_name("declarator")?;
        return declarator
            .child_by_field_name("declarator")
            .or_else(|| declarator.child_by_field_name("name"));
    }
    None
}

/// Function-like node kinds that should isolate their bodies from the
/// outer impl / class scope. Nested fn / def / lambda / closure inside
/// such a node is a local helper, not a method of the outer type.
fn opens_local_scope(language: SourceLanguage, node_kind: &str) -> bool {
    match language {
        SourceLanguage::Rust => matches!(
            node_kind,
            "function_item" | "function_signature_item" | "closure_expression"
        ),
        SourceLanguage::Python => {
            matches!(node_kind, "function_definition" | "lambda")
        }
        SourceLanguage::JavaScript
        | SourceLanguage::TypeScript
        | SourceLanguage::Tsx
        | SourceLanguage::CSharp
        | SourceLanguage::Razor
        | SourceLanguage::Go
        | SourceLanguage::C
        | SourceLanguage::Cpp
        | SourceLanguage::Bash => matches!(
            node_kind,
            "function_declaration"
                | "method_definition"
                | "function_expression"
                | "arrow_function"
                | "generator_function_declaration"
                | "generator_function"
                | "method_declaration"
                | "constructor_declaration"
                | "local_function_statement"
                | "lambda_expression"
        ),
        _ => false,
    }
}

/// Returns the enclosing-type name when `node` opens a method-bearing scope.
fn scope_type_name(language: SourceLanguage, node: Node<'_>, source: &str) -> Option<String> {
    let bytes = source.as_bytes();
    match (language, node.kind()) {
        (SourceLanguage::Rust, "impl_item") => {
            // `impl Foo` and `impl Trait for Foo` both expose Foo on the
            // `type` field — the trait, when present, sits on `trait`.
            node.child_by_field_name("type")
                .and_then(|child| child.utf8_text(bytes).ok())
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty())
        }
        (SourceLanguage::Rust, "trait_item") => node
            .child_by_field_name("name")
            .and_then(|child| child.utf8_text(bytes).ok())
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty()),
        (SourceLanguage::CSharp | SourceLanguage::Razor, kind)
            if matches!(
                kind,
                "class_declaration"
                    | "struct_declaration"
                    | "interface_declaration"
                    | "record_declaration"
            ) =>
        {
            node.child_by_field_name("name")
                .and_then(|child| child.utf8_text(bytes).ok())
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty())
        }
        (SourceLanguage::Python, "class_definition") => node
            .child_by_field_name("name")
            .and_then(|child| child.utf8_text(bytes).ok())
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty()),
        (
            SourceLanguage::JavaScript
            | SourceLanguage::TypeScript
            | SourceLanguage::Tsx
            | SourceLanguage::CSharp
            | SourceLanguage::Razor,
            "class_declaration",
        ) => node
            .child_by_field_name("name")
            .and_then(|child| child.utf8_text(bytes).ok())
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty()),
        _ => None,
    }
}

/// Promote `Function` symbols to `Method` and attach a qualified `Type::name`
/// (Rust) or `Type.name` (others) when they sit inside an enclosing impl /
/// trait / class scope. `method_definition` symbols are already classified as
/// `Method` but still need the qualifier; pre-existing non-function symbols
/// pass through unchanged.
fn resolve_method_scope(
    language: SourceLanguage,
    kind: SymbolKind,
    enclosing_type: Option<&str>,
    name: &str,
) -> (SymbolKind, Option<String>) {
    let Some(ty) = enclosing_type else {
        return (kind, None);
    };

    let effective_kind = match kind {
        SymbolKind::Function => SymbolKind::Method,
        other => other,
    };

    if !matches!(effective_kind, SymbolKind::Method) {
        // Nested types inside a scope keep their kind but don't earn a
        // qualifier — `Outer::Inner` ambiguity isn't worth the complexity
        // until the call graph asks for it.
        return (effective_kind, None);
    }

    let separator = match language {
        SourceLanguage::Rust => "::",
        _ => ".",
    };
    (effective_kind, Some(format!("{ty}{separator}{name}")))
}

fn symbol_kind(language: SourceLanguage, node_kind: &str) -> Option<SymbolKind> {
    match language {
        SourceLanguage::Rust => match node_kind {
            // `function_signature_item` covers bodyless declarations inside
            // `trait Foo { fn bar(&self); }`. They're classified Function
            // here and then promoted to Method by `resolve_method_scope`
            // when the walker is inside a trait/impl scope.
            "function_item" | "function_signature_item" => Some(SymbolKind::Function),
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
                "lexical_declaration" | "variable_declarator" => Some(SymbolKind::Variable),
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
            "type_spec" => Some(SymbolKind::TypeAlias),
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

fn dedupe_symbols(items: Vec<SymbolOccurrence>) -> Vec<SymbolOccurrence> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in items {
        if seen.insert(item.clone()) {
            out.push(item);
        }
    }
    out
}

fn extract_env_vars(path: &str, language: SourceLanguage, source: &str) -> Vec<EnvVarOccurrence> {
    let mut out = Vec::new();
    for (idx, line) in source.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = line.trim();
        if matches!(
            language,
            SourceLanguage::Env | SourceLanguage::Toml | SourceLanguage::Yaml
        ) && !trimmed.starts_with('#')
            && let Some(captures) = DECLARED_ENV_PATTERN.captures(trimmed)
        {
            let name = captures.get(1).map(|m| m.as_str()).unwrap_or_default();
            if !name.is_empty() {
                out.push(EnvVarOccurrence {
                    name: name.to_string(),
                    access: AccessKind::Declared,
                    path: path.to_string(),
                    line: line_no,
                    language,
                });
            }
        }

        for pattern in ENV_ACCESS_PATTERNS.iter() {
            for capture in pattern.captures_iter(line) {
                let Some(name) = capture.get(1).map(|value| value.as_str()) else {
                    continue;
                };
                let access = if line.contains("set_var") {
                    AccessKind::Write
                } else if let Some(eq) = line.find('=') {
                    let is_equality = line[eq..].starts_with("==")
                        || (eq > 0
                            && (line.as_bytes()[eq - 1] == b'!'
                                || line.as_bytes()[eq - 1] == b'<'
                                || line.as_bytes()[eq - 1] == b'>'));
                    if !is_equality && capture.get(0).unwrap().start() < eq {
                        AccessKind::Write
                    } else {
                        AccessKind::Read
                    }
                } else {
                    AccessKind::Read
                };
                out.push(EnvVarOccurrence {
                    name: name.to_string(),
                    access,
                    path: path.to_string(),
                    line: line_no,
                    language,
                });
            }
        }
    }
    dedupe_env_vars(out)
}

fn dedupe_env_vars(items: Vec<EnvVarOccurrence>) -> Vec<EnvVarOccurrence> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in items {
        if seen.insert(item.clone()) {
            out.push(item);
        }
    }
    out
}

fn extract_redis_keys(
    path: &str,
    language: SourceLanguage,
    source: &str,
) -> Vec<RedisKeyOccurrence> {
    let mut out = Vec::new();
    for (idx, line) in source.lines().enumerate() {
        let access = classify_redis_access(line);
        for capture in REDIS_LITERAL_PATTERN.captures_iter(line) {
            let Some(key) = capture.get(1).map(|value| value.as_str()) else {
                continue;
            };
            if likely_redis_key(key) {
                out.push(RedisKeyOccurrence {
                    key: key.to_string(),
                    access,
                    path: path.to_string(),
                    line: idx + 1,
                    language,
                });
            }
        }
    }
    dedupe_redis_keys(out)
}

fn classify_redis_access(line: &str) -> AccessKind {
    let lower = line.to_ascii_lowercase();
    if [
        "set(",
        ".set(",
        "hset(",
        ".hset(",
        "xadd(",
        ".xadd(",
        "lpush(",
        ".lpush(",
        "rpush(",
        ".rpush(",
        "publish(",
        ".publish(",
        "expire(",
        ".expire(",
        "delete(",
        ".delete(",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        AccessKind::Write
    } else if [
        "get(", ".get", "hget", "hgetall", "xread", "xrange", "lrange", "exists",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        AccessKind::Read
    } else {
        AccessKind::Unknown
    }
}

fn likely_redis_key(key: &str) -> bool {
    if is_common_permission_scope(key) {
        return false;
    }

    let prefixes = [
        "route:",
        "route:exact:",
        "phone_route:",
        "phone_user_route:",
        "session:",
        "messages:",
        "events:",
        "agent_session:",
        "wa_outbound:",
        "lease:",
        "workflow:",
        "memory:",
        "conversation:",
    ];
    prefixes.iter().any(|prefix| key.starts_with(prefix))
}

fn is_common_permission_scope(key: &str) -> bool {
    matches!(
        key,
        "messages:send" | "messages:view" | "messages:read" | "messages:write" | "messages:delete"
    )
}

fn dedupe_redis_keys(items: Vec<RedisKeyOccurrence>) -> Vec<RedisKeyOccurrence> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in items {
        if seen.insert(item.clone()) {
            out.push(item);
        }
    }
    out
}

fn parse_deploy_targets(root: &Path) -> Result<Vec<DeployTargetRecord>> {
    let target_dir = root.join("deploy/targets");
    if !target_dir.exists() {
        return Ok(Vec::new());
    }

    let mut items = Vec::new();
    for entry in fs::read_dir(&target_dir)
        .with_context(|| format!("failed to read {}", target_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("toml") {
            continue;
        }
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let value: toml::Value =
            toml::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))?;
        let rel = relative_path(root, &path);
        let name = value
            .get("target_name")
            .and_then(toml::Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| {
                path.file_stem()
                    .and_then(|value| value.to_str())
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_else(|| rel.clone());

        items.push(DeployTargetRecord {
            name,
            path: rel,
            profile: get_string(&value, "profile"),
            readiness_target: get_string(&value, "readiness_target"),
            deploy_class: get_string(&value, "deploy_class"),
            topology: get_string(&value, "topology"),
            ui_role: get_string(&value, "ui_role"),
            ui_path: get_string(&value, "ui_path"),
            frontend_project: get_string(&value, "frontend_project"),
            backend_profile: get_string(&value, "backend_profile"),
            secret_set: get_string(&value, "secret_set"),
            health_checks: get_array(&value, "health_checks"),
            smoke_suite: get_string(&value, "smoke_suite"),
            rollback_command: get_string(&value, "rollback_command"),
            cartridges: get_array(&value, "cartridges"),
            required_integrations: get_array(&value, "required_integrations"),
            promotion_policy: get_string(&value, "promotion_policy"),
        });
    }

    items.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(items)
}

fn parse_env_records(dir: &Path) -> Result<Vec<ProfileRecord>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut items = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !(name.ends_with(".env") || name.ends_with(".env.example")) {
            continue;
        }
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let vars = parse_declared_vars(&raw);
        items.push(ProfileRecord {
            name: name.to_string(),
            path: path.display().to_string(),
            vars,
        });
    }
    items.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(items)
}

fn parse_env_records_secrets(dir: &Path) -> Result<Vec<SecretSetRecord>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut items = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !name.ends_with(".env.example") {
            continue;
        }
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let vars = parse_declared_vars(&raw);
        items.push(SecretSetRecord {
            name: name.to_string(),
            path: path.display().to_string(),
            vars,
        });
    }
    items.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(items)
}

pub fn load_profiles_and_secret_sets(
    root: &Path,
) -> Result<(Vec<ProfileRecord>, Vec<SecretSetRecord>)> {
    let profiles = parse_env_records(&root.join("deploy/profiles"))?;
    let secrets = parse_env_records_secrets(&root.join("deploy/secret-sets"))?;
    Ok((profiles, secrets))
}

/// Scan the repo root for dotenv-style files (`.env`, `.env.local`, …).
///
/// Each file becomes an [`EnvFileRecord`] with a precedence ordered so lower
/// values win during resolution (`.env.local` overrides `.env`). Missing files
/// are silently skipped — most repos have a partial set.
///
/// The set of recognized filenames is intentionally narrow; ad-hoc names like
/// `.env.staging.preview` are ignored to keep the precedence table predictable.
pub fn parse_root_env_files(root: &Path) -> Result<Vec<EnvFileRecord>> {
    // (filename, precedence) — lower precedence wins.
    const KNOWN: &[(&str, u8)] = &[
        (".env.local", 0),
        (".env.development.local", 1),
        (".env.production.local", 1),
        (".env.development", 2),
        (".env.production", 2),
        (".env", 3),
        (".env.example", 4),
        (".env.sample", 4),
    ];
    let mut out = Vec::new();
    for (name, precedence) in KNOWN {
        let path = root.join(name);
        if !path.is_file() {
            continue;
        }
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(_) => continue,
        };
        let vars = parse_declared_vars(&raw);
        out.push(EnvFileRecord {
            path: (*name).to_string(),
            precedence: *precedence,
            vars,
        });
    }
    Ok(out)
}

/// Parse `KEY=VALUE` declarations from a dotenv-style file.
///
/// Multi-line quoted values are supported: when an opening `"` or `'` is found
/// without a matching closing quote on the same line, the parser accumulates
/// subsequent lines (preserving newlines verbatim) until the closing quote.
/// The surrounding quotes are stripped from the captured raw value.
///
/// **Unterminated quote policy (safer-skip):** if the opening quote is never
/// closed before EOF, the offending variable is dropped entirely rather than
/// consuming the rest of the file. This preserves any well-formed variables
/// that appear after the malformed one; the test
/// `unterminated_quote_does_not_swallow_following_vars` pins this contract.
///
/// **Not supported (deferred):** escape sequences inside quotes (`\"`, `\n`),
/// nested quotes, backtick-quoted values, and continuation of a single-quoted
/// value containing the other quote style as a literal.
fn parse_declared_vars(raw: &str) -> Vec<DeclaredVar> {
    let mut vars = Vec::new();
    let lines: Vec<&str> = raw.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            i += 1;
            continue;
        }
        let Some(captures) = DECLARED_VAR_PATTERN.captures(trimmed) else {
            i += 1;
            continue;
        };
        let name = captures
            .get(1)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        let value_part = captures.get(2).map(|m| m.as_str()).unwrap_or("");

        // Detect an opening quote without a same-line match. We only care about
        // the very first char (after the `=`). Whitespace before the opening
        // quote would be unusual; keep it strict.
        let opening_quote = value_part
            .chars()
            .next()
            .filter(|c| *c == '"' || *c == '\'');

        let raw_value: Option<String> = if let Some(quote) = opening_quote {
            let after_quote = &value_part[1..];
            if let Some(end_idx) = after_quote.find(quote) {
                // Closing quote on same line: take content between the quotes.
                Some(after_quote[..end_idx].to_string())
            } else {
                // Open-quoted: accumulate subsequent lines until the close.
                let mut acc = String::from(after_quote);
                let mut j = i + 1;
                let mut closed = false;
                while j < lines.len() {
                    acc.push('\n');
                    let next = lines[j];
                    if let Some(end_idx) = next.find(quote) {
                        acc.push_str(&next[..end_idx]);
                        closed = true;
                        i = j; // advance outer cursor past the closing line
                        break;
                    }
                    acc.push_str(next);
                    j += 1;
                }
                if !closed {
                    // Drop the var entirely; don't consume following lines.
                    i += 1;
                    continue;
                }
                Some(acc)
            }
        } else {
            let trimmed_val = value_part.trim();
            if trimmed_val.is_empty() {
                None
            } else {
                Some(trimmed_val.to_string())
            }
        };

        let value_preview = raw_value
            .as_deref()
            .map(|v| v.chars().take(48).collect::<String>());

        vars.push(DeclaredVar {
            name,
            value_preview,
            raw_value,
        });
        i += 1;
    }
    vars
}

fn get_string(value: &toml::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(toml::Value::as_str)
        .map(ToOwned::to_owned)
}

fn get_array(value: &toml::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(toml::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(toml::Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
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

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

fn system_time_to_unix_ms(value: Option<SystemTime>) -> i128 {
    value
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i128)
        .unwrap_or_default()
}

pub fn refresh_auxiliary_records(index: &mut RepoIndex, root: &Path) -> Result<()> {
    index.deploy_targets = parse_deploy_targets(root)?;
    let (profiles, secret_sets) = load_profiles_and_secret_sets(root)?;
    index.profiles = profiles;
    index.secret_sets = secret_sets;
    let binaries = crate::cross_language::collect_binary_nodes(root);
    let resolved_spawns = crate::cross_language::resolve_spawn_edges(&index.files, &binaries);
    // Preserve already-discovered route declarations across the refresh —
    // routes are extracted during the per-file walk, not during this auxiliary
    // pass. Re-resolving the edges keeps the graph consistent with whatever
    // routes the previous build observed.
    let routes = index.cross_language.routes.clone();
    let resolved_http_edges = crate::cross_language::resolve_http_edges(&index.files, &routes);
    let unresolved_edges: Vec<_> = index
        .files
        .iter()
        .flat_map(|f| f.unresolved_edges.iter().cloned())
        .collect();
    index.cross_language = CrossLanguageGraph {
        binaries,
        resolved_spawns,
        routes,
        resolved_http_edges,
        unresolved_edges,
    };
    Ok(())
}

/// Run the per-language route detectors against a single file's content and
/// return the records to be added to the graph's `routes` list.
fn extract_routes_for_file(
    rel: &str,
    language: SourceLanguage,
    content: &str,
) -> Vec<crate::model::RouteRecord> {
    match language {
        SourceLanguage::Python => {
            let mut out = crate::cross_language::detect_flask_routes(content, rel);
            out.extend(crate::cross_language::detect_fastapi_routes(content, rel));
            out
        }
        SourceLanguage::JavaScript | SourceLanguage::TypeScript | SourceLanguage::Tsx => {
            crate::cross_language::detect_express_routes(content, rel, language)
        }
        SourceLanguage::Rust => crate::cross_language::detect_axum_routes(content, rel),
        _ => Vec::new(),
    }
}

/// Try to parse a YAML file as a Kubernetes ConfigMap.
///
/// Returns `Some(K8sConfigMapRecord)` when the file contains `kind: ConfigMap`
/// and a `data:` section. Uses simple line-by-line scanning — not a full YAML
/// parser. Limitations (by design, to avoid a new dependency):
///   - Only single-document files are supported (no `---` multi-doc).
///   - Only plain string `data:` values are extracted (`key: value` on one line).
///   - Block scalars (`|`, `>`) and anchors are ignored.
///   - `metadata.name` and `metadata.namespace` must appear as `name: <val>` /
///     `namespace: <val>` somewhere before or after `data:`, at two-space indent.
pub fn detect_k8s_configmap(path: &str, content: &str) -> Option<K8sConfigMapRecord> {
    use std::collections::BTreeMap;

    // Quick pre-filter: must mention both markers.
    if !content.contains("kind: ConfigMap") || !content.contains("data:") {
        return None;
    }

    let mut map_name: Option<String> = None;
    let mut namespace: Option<String> = None;
    let mut entries: BTreeMap<String, String> = BTreeMap::new();

    // State machine: are we inside the `data:` block?
    let mut in_data = false;

    for line in content.lines() {
        // Top-level `kind` confirmation (no leading spaces).
        if line.trim_start() == line && line.starts_with("kind:") {
            if line.trim_end() != "kind: ConfigMap" {
                // A different kind — bail out.
                return None;
            }
            continue;
        }

        // metadata fields at two-space indent.
        if let Some(rest) = line.strip_prefix("  name:") {
            if map_name.is_none() {
                map_name = Some(rest.trim().to_string());
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("  namespace:") {
            if namespace.is_none() {
                namespace = Some(rest.trim().to_string());
            }
            continue;
        }

        // `data:` section marker at top level.
        if line.trim_start() == line && line.trim_end() == "data:" {
            in_data = true;
            continue;
        }

        // Any other top-level key exits the data section.
        if in_data && line.trim_start() == line && !line.is_empty() && !line.starts_with('#') {
            in_data = false;
        }

        if in_data {
            // Two-space-indented `key: value` pairs.
            if let Some(rest) = line.strip_prefix("  ")
                && let Some(colon) = rest.find(':')
            {
                let key = rest[..colon].trim();
                let val = rest[colon + 1..].trim();
                // Skip block scalar indicators and empty values.
                if !key.is_empty()
                    && !val.starts_with('|')
                    && !val.starts_with('>')
                    && !val.starts_with('#')
                {
                    entries.insert(key.to_string(), val.to_string());
                }
            }
        }
    }

    let map_name = map_name?; // No name → not a well-formed ConfigMap.

    Some(K8sConfigMapRecord {
        path: path.to_string(),
        map_name,
        namespace,
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupe_symbols_uses_struct_identity() {
        let items = vec![
            SymbolOccurrence {
                name: "run".to_string(),
                kind: SymbolKind::Function,
                path: "src/lib.rs".to_string(),
                line: 10,
                language: SourceLanguage::Rust,
                qual_name: None,
            },
            SymbolOccurrence {
                name: "run".to_string(),
                kind: SymbolKind::Function,
                path: "src/lib.rs".to_string(),
                line: 10,
                language: SourceLanguage::Rust,
                qual_name: None,
            },
        ];

        let deduped = dedupe_symbols(items);
        assert_eq!(deduped.len(), 1);
    }

    #[test]
    fn rust_impl_methods_get_qualified_names_and_method_kind() {
        let source = r#"
pub struct ExampleRouter;

impl ExampleRouter {
    pub fn route(&self, key: &str) -> Option<String> {
        None
    }
}

pub fn bare_top_level() {}
"#;
        let syms = extract_symbols("src/router.rs", SourceLanguage::Rust, source);

        let route = syms
            .iter()
            .find(|s| s.name == "route")
            .expect("route extracted");
        assert_eq!(route.kind, SymbolKind::Method);
        assert_eq!(route.qual_name.as_deref(), Some("ExampleRouter::route"));

        let bare = syms
            .iter()
            .find(|s| s.name == "bare_top_level")
            .expect("bare fn extracted");
        assert_eq!(bare.kind, SymbolKind::Function);
        assert!(bare.qual_name.is_none());
    }

    #[test]
    fn rust_trait_methods_inherit_trait_name() {
        let source = r#"
pub trait Reasoner {
    fn reason(&self, query: &str) -> String;
}
"#;
        let syms = extract_symbols("src/reasoner.rs", SourceLanguage::Rust, source);
        let reason = syms
            .iter()
            .find(|s| s.name == "reason")
            .expect("trait method extracted");
        assert_eq!(reason.kind, SymbolKind::Method);
        assert_eq!(reason.qual_name.as_deref(), Some("Reasoner::reason"));
    }

    #[test]
    fn python_class_methods_use_dot_qualifier() {
        let source = r#"
class Pipeline:
    def run(self, payload):
        return payload

def standalone():
    return None
"#;
        let syms = extract_symbols("src/pipeline.py", SourceLanguage::Python, source);
        let run = syms
            .iter()
            .find(|s| s.name == "run")
            .expect("method extracted");
        assert_eq!(run.kind, SymbolKind::Method);
        assert_eq!(run.qual_name.as_deref(), Some("Pipeline.run"));

        let alone = syms
            .iter()
            .find(|s| s.name == "standalone")
            .expect("standalone fn extracted");
        assert_eq!(alone.kind, SymbolKind::Function);
        assert!(alone.qual_name.is_none());
    }

    #[test]
    fn nested_functions_do_not_inherit_enclosing_type_scope() {
        // A Rust local helper nested inside an impl method must not be
        // promoted to a method of the outer type.
        let rust_source = r#"
pub struct MyRouter;

impl MyRouter {
    pub fn route(&self, key: &str) -> Option<String> {
        fn helper(s: &str) -> usize { s.len() }
        Some(format!("{}", helper(key)))
    }
}
"#;
        let syms = extract_symbols("src/router.rs", SourceLanguage::Rust, rust_source);
        let route = syms.iter().find(|s| s.name == "route").expect("route");
        assert_eq!(route.kind, SymbolKind::Method);
        assert_eq!(route.qual_name.as_deref(), Some("MyRouter::route"));

        let helper = syms.iter().find(|s| s.name == "helper").expect("helper");
        assert_eq!(helper.kind, SymbolKind::Function);
        assert!(
            helper.qual_name.is_none(),
            "nested helper must not inherit MyRouter scope, got {:?}",
            helper.qual_name,
        );

        // Same shape for Python — nested `def` inside a method stays a
        // bare function.
        let py_source = r#"
class Pipeline:
    def run(self, payload):
        def helper(value):
            return value
        return helper(payload)
"#;
        let py_syms = extract_symbols("src/pipeline.py", SourceLanguage::Python, py_source);
        let run = py_syms.iter().find(|s| s.name == "run").expect("run");
        assert_eq!(run.kind, SymbolKind::Method);
        assert_eq!(run.qual_name.as_deref(), Some("Pipeline.run"));
        let py_helper = py_syms
            .iter()
            .find(|s| s.name == "helper")
            .expect("py helper");
        assert_eq!(py_helper.kind, SymbolKind::Function);
        assert!(py_helper.qual_name.is_none());
    }

    #[test]
    fn typescript_class_methods_use_dot_qualifier() {
        let source = r#"
export class Router {
    route(key: string): string | null {
        return null;
    }
}
"#;
        let syms = extract_symbols("src/router.ts", SourceLanguage::TypeScript, source);
        let route = syms
            .iter()
            .find(|s| s.name == "route")
            .expect("ts method extracted");
        assert_eq!(route.kind, SymbolKind::Method);
        assert_eq!(route.qual_name.as_deref(), Some("Router.route"));
    }

    #[test]
    fn csharp_and_razor_are_detected_and_indexed() {
        assert!(should_index_file(Path::new("Worker.cs")));
        assert!(should_index_file(Path::new("Worker.csx")));
        assert!(should_index_file(Path::new("Pages/Index.razor")));
        assert!(should_index_file(Path::new("Pages/Index.cshtml")));
        assert_eq!(
            detect_language(Path::new("Worker.cs")),
            SourceLanguage::CSharp
        );
        assert_eq!(
            detect_language(Path::new("Pages/Index.cshtml")),
            SourceLanguage::Razor
        );

        let csharp = "namespace Demo; public class Worker { public void Run() {} public string Name { get; set; } }";
        let symbols = extract_symbols("Worker.cs", SourceLanguage::CSharp, csharp);
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Worker" && s.kind == SymbolKind::Class)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Run" && s.kind == SymbolKind::Method)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Name" && s.kind == SymbolKind::Property)
        );

        let razor = "<h1>Hello</h1>\n@code {\n    public void Save() {}\n}\n";
        let symbols = extract_symbols("Pages/Index.cshtml", SourceLanguage::Razor, razor);
        let save = symbols
            .iter()
            .find(|s| s.name == "Save")
            .expect("Razor method");
        assert_eq!(save.kind, SymbolKind::Method);
        assert_eq!(save.line, 3);
        assert_eq!(save.language, SourceLanguage::Razor);
    }

    #[test]
    fn go_c_cpp_and_bash_are_detected_and_indexed() {
        assert_eq!(
            detect_language(Path::new("cmd/main.go")),
            SourceLanguage::Go
        );
        assert_eq!(detect_language(Path::new("src/main.c")), SourceLanguage::C);
        assert_eq!(
            detect_language(Path::new("src/main.cpp")),
            SourceLanguage::Cpp
        );
        assert_eq!(
            detect_language(Path::new("scripts/build.sh")),
            SourceLanguage::Bash
        );

        let go = "package main\nfunc Run() {}\ntype Worker struct {}\n";
        let symbols = extract_symbols("cmd/main.go", SourceLanguage::Go, go);
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Run" && s.kind == SymbolKind::Function)
        );
        assert!(symbols.iter().any(|s| s.name == "Worker"));

        let c = "typedef struct Worker { int ready; } Worker;\nvoid run(void) {}\n";
        let symbols = extract_symbols("src/main.c", SourceLanguage::C, c);
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "run" && s.kind == SymbolKind::Function)
        );

        let cpp = "class Worker {};\nvoid run() {}\n";
        let symbols = extract_symbols("src/main.cpp", SourceLanguage::Cpp, cpp);
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Worker" && s.kind == SymbolKind::Class)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "run" && s.kind == SymbolKind::Function)
        );

        let bash = "build() {\n  echo building\n}\n";
        let symbols = extract_symbols("scripts/build.sh", SourceLanguage::Bash, bash);
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "build" && s.kind == SymbolKind::Function)
        );
    }

    #[test]
    fn java_kotlin_html_css_sql_and_swift_are_indexed() {
        assert_eq!(
            detect_language(Path::new("src/App.java")),
            SourceLanguage::Java
        );
        assert_eq!(
            detect_language(Path::new("src/App.kt")),
            SourceLanguage::Kotlin
        );
        assert_eq!(
            detect_language(Path::new("web/index.html")),
            SourceLanguage::Html
        );
        assert_eq!(
            detect_language(Path::new("web/app.css")),
            SourceLanguage::Css
        );
        assert_eq!(
            detect_language(Path::new("db/schema.sql")),
            SourceLanguage::Sql
        );
        assert_eq!(
            detect_language(Path::new("App.swift")),
            SourceLanguage::Swift
        );

        let java = "public class App { public void run() {} }";
        assert!(
            extract_symbols("App.java", SourceLanguage::Java, java)
                .iter()
                .any(|s| s.name == "App" && s.kind == SymbolKind::Class)
        );

        let kotlin = "class App\nfun run() {}\n";
        assert!(
            extract_symbols("App.kt", SourceLanguage::Kotlin, kotlin)
                .iter()
                .any(|s| s.name == "run" && s.kind == SymbolKind::Function)
        );

        let html = "<main><h1>Hello</h1></main>";
        assert!(
            extract_symbols("index.html", SourceLanguage::Html, html)
                .iter()
                .any(|s| s.kind == SymbolKind::Class)
        );

        let css = ".card { color: red; }";
        assert!(
            extract_symbols("app.css", SourceLanguage::Css, css)
                .iter()
                .any(|s| s.kind == SymbolKind::Class)
        );

        let sql =
            "CREATE TABLE users (id INTEGER); CREATE VIEW active_users AS SELECT * FROM users;";
        let sql_symbols = extract_symbols("schema.sql", SourceLanguage::Sql, sql);
        assert!(sql_symbols.iter().any(|s| s.name == "users"));
        assert!(sql_symbols.iter().any(|s| s.name == "active_users"));

        let swift = "struct App {}\nfunc run() {}\n";
        assert!(
            extract_symbols("App.swift", SourceLanguage::Swift, swift)
                .iter()
                .any(|s| s.name == "run" && s.language == SourceLanguage::Swift)
        );
    }

    #[test]
    fn env_access_detects_write_vs_read() {
        let source = r#"
process.env["JWT_SECRET"] = rotate();
const token = process.env.JWT_SECRET;
if (process.env.JWT_SECRET === "x") { return; }
os.environ["DB_PASSWORD"] = "secret"
value = os.environ["DB_PASSWORD"]
"#;

        let envs = extract_env_vars("demo.ts", SourceLanguage::TypeScript, source);
        let access_by_line: HashMap<usize, AccessKind> = envs
            .into_iter()
            .map(|item| (item.line, item.access))
            .collect();

        assert_eq!(access_by_line.get(&2), Some(&AccessKind::Write));
        assert_eq!(access_by_line.get(&3), Some(&AccessKind::Read));
        assert_eq!(access_by_line.get(&4), Some(&AccessKind::Read));
        assert_eq!(access_by_line.get(&5), Some(&AccessKind::Write));
        assert_eq!(access_by_line.get(&6), Some(&AccessKind::Read));
    }

    #[test]
    fn python_getenv_captures_default_argument_form() {
        let source = r#"
flags = os.getenv("FLAG_ENABLED", "true").strip()
secret = os.getenv('API_SECRET')
value = os.getenv("EXAMPLE_ACTIVE_DOMAINS", "")
"#;

        let envs = extract_env_vars("demo.py", SourceLanguage::Python, source);
        let names: Vec<&str> = envs.iter().map(|item| item.name.as_str()).collect();

        assert_eq!(names, vec!["FLAG_ENABLED", "API_SECRET", "EXAMPLE_ACTIVE_DOMAINS"]);
        assert!(envs.iter().all(|item| item.access == AccessKind::Read));
    }

    #[test]
    fn redis_extraction_ignores_permission_labels() {
        let source = r#"
export const PERMISSIONS = {
  MESSAGES_SEND: 'messages:send',
  MESSAGES_DELETE: 'messages:delete',
};
await redis.hset('workflow:leio-indexer-fixture', mapping);
await redis.expire('workflow:leio-indexer-fixture', 3600);
"#;

        let keys = extract_redis_keys("demo.ts", SourceLanguage::TypeScript, source);
        let access_by_key: HashMap<String, AccessKind> = keys
            .into_iter()
            .map(|item| (item.key, item.access))
            .collect();

        assert!(!access_by_key.contains_key("messages:delete"));
        assert_eq!(
            access_by_key.get("workflow:leio-indexer-fixture"),
            Some(&AccessKind::Write)
        );
    }

    #[test]
    fn dockerfile_indexed_as_text_makefile_as_make() {
        let dockerfile = Path::new("/tmp/workspace/Dockerfile");
        let makefile = Path::new("/tmp/workspace/Makefile");
        let gnumakefile = Path::new("/tmp/workspace/GNUmakefile");
        let pkg = Path::new("/tmp/workspace/package.json");

        assert!(should_index_file(dockerfile));
        assert!(should_index_file(makefile));
        assert!(should_index_file(gnumakefile));
        assert_eq!(detect_language(dockerfile), SourceLanguage::Text);
        assert_eq!(detect_language(makefile), SourceLanguage::Make);
        assert_eq!(detect_language(gnumakefile), SourceLanguage::Make);
        assert_eq!(detect_language(pkg), SourceLanguage::NpmScript);
    }

    #[test]
    fn hypha_c_firmware_is_indexed_and_defines_are_symbols() {
        // Why: Flipper FAP lives in .c/.h; skipping those extensions hid Hypha.
        assert!(should_index_file(Path::new("firmware/hypha_go/hypha_go.c")));
        assert!(should_index_file(Path::new(
            "firmware/hypha_go/hypha_go_queue.h"
        )));
        let header = r#"
#define HYPHA_APPS_DATA "/ext/apps_data/hypha"
#define HYPHA_QUEUE_LINE_MAX 512
static bool hypha_queue_capture(Storage* storage) {
    return true;
}
int32_t hypha_go_app(void* p) {
    (void)p;
    return 0;
}
"#;
        let symbols = extract_symbols("firmware/hypha_go/hypha_go.c", SourceLanguage::C, header);
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"HYPHA_APPS_DATA"));
        assert!(names.contains(&"HYPHA_QUEUE_LINE_MAX"));
        assert!(names.contains(&"hypha_queue_capture"));
        assert!(names.contains(&"hypha_go_app"));
        assert!(!names.contains(&"if"));
    }

    #[test]
    fn swift_sources_are_indexed_and_decls_are_symbols() {
        // Why: JAI Team is Swift-only; skipping .swift made leio-workbench
        // report a fresh 0-file index and Jana/Cleito had nothing to navigate.
        assert!(should_index_file(Path::new(
            "Sources/LeioWorkbenchCore/HermesHistory.swift"
        )));
        assert_eq!(
            detect_language(Path::new("Sources/LeioWorkbenchCore/HermesHistory.swift")),
            SourceLanguage::Swift
        );
        let source = r#"
public enum HermesHistory {
    public static func latestPreview(profile: String) -> String { "" }
}
public struct WorkerRecord {}
"#;
        let symbols = extract_symbols(
            "Sources/LeioWorkbenchCore/HermesHistory.swift",
            SourceLanguage::Swift,
            source,
        );
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"HermesHistory"));
        assert!(names.contains(&"latestPreview"));
        assert!(names.contains(&"WorkerRecord"));
    }

    #[test]
    fn ontology_ttl_owl_jsonld_are_indexed_as_rdf_symbols() {
        // Why: without these extensions, Example ontologies were invisible to
        // `find symbol` even though the formal store already loaded Turtle.
        assert!(should_index_file(Path::new("ontologies/claim.ttl")));
        assert!(should_index_file(Path::new("ontologies/policy.owl")));
        assert!(should_index_file(Path::new("ontologies/invoice.jsonld")));
        assert_eq!(
            detect_language(Path::new("ontologies/claim.ttl")),
            SourceLanguage::Rdf
        );
        let turtle = r#"@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix ex: <http://example.org/leio-ont#> .
ex:Claim a owl:Class .
"#;
        let symbols = extract_symbols("ontologies/claim.ttl", SourceLanguage::Rdf, turtle);
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Claim" && s.kind == SymbolKind::Class),
            "{symbols:?}"
        );

        let root = std::env::temp_dir().join(format!(
            "leio-code-ontology-index-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("ont")).expect("mkdir ont");
        fs::write(root.join("ont/claim.ttl"), turtle).expect("write ttl");
        let index_path = default_index_path(&root);
        let index = build_or_update_index(&root, &index_path, true).expect("index ontologies");
        assert!(
            index
                .all_symbols()
                .any(|s| s.name == "Claim" && s.language == SourceLanguage::Rdf),
            "indexed files: {:?}",
            index.files.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// Linked worktrees nested in the repo (`.claude/worktrees/<id>/`) are a
    /// second checkout of the same files; indexing them reports every symbol
    /// once per worktree. Submodules use a `.git` file too but point at
    /// `.git/modules/` and hold different code, so they stay indexed.
    #[test]
    fn discovery_skips_nested_linked_worktrees_but_keeps_submodules() {
        let root = std::env::temp_dir().join(format!(
            "leio-code-nested-worktree-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".claude/worktrees/wf_1")).expect("mkdir worktree");
        fs::create_dir_all(root.join("vendor/submodule")).expect("mkdir submodule");
        fs::create_dir_all(root.join("src")).expect("mkdir src");

        fs::write(root.join("src/main.ts"), "export function real() {}\n").expect("write real");
        // Linked worktree: gitdir points into the main repo's .git/worktrees.
        fs::write(
            root.join(".claude/worktrees/wf_1/.git"),
            "gitdir: /repo/.git/worktrees/wf_1\n",
        )
        .expect("write worktree gitfile");
        fs::write(
            root.join(".claude/worktrees/wf_1/main.ts"),
            "export function duplicated() {}\n",
        )
        .expect("write worktree copy");
        // Submodule: gitdir points into .git/modules.
        fs::write(
            root.join("vendor/submodule/.git"),
            "gitdir: /repo/.git/modules/submodule\n",
        )
        .expect("write submodule gitfile");
        fs::write(
            root.join("vendor/submodule/lib.ts"),
            "export function vendored() {}\n",
        )
        .expect("write submodule file");

        assert!(is_linked_git_worktree(&root.join(".claude/worktrees/wf_1")));
        assert!(!is_linked_git_worktree(&root.join("vendor/submodule")));
        assert!(!is_linked_git_worktree(&root.join("src")));

        let discovered = discover_source_files(&root).expect("discover");
        let names = discovered
            .paths
            .iter()
            .map(|path| {
                path.strip_prefix(&root)
                    .unwrap_or(path)
                    .display()
                    .to_string()
            })
            .collect::<Vec<_>>();

        assert!(
            names.iter().any(|name| name.ends_with("src/main.ts")),
            "repo source must be indexed: {names:?}"
        );
        assert!(
            names.iter().any(|name| name.ends_with("submodule/lib.ts")),
            "submodule source must stay indexed: {names:?}"
        );
        assert!(
            !names.iter().any(|name| name.contains("worktrees/wf_1")),
            "nested linked worktree must not be indexed: {names:?}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_symlinked_include_root_is_reported_instead_of_silently_indexing_nothing() {
        // A workspace listed a frontend in include_roots, then moved the real
        // directory and left a symlink at the old path. The walk does not follow
        // symlinks, so the entry read as configured coverage while contributing
        // zero files, and nothing said so. The warning is the whole point: the
        // silence is what let the gap survive a migration.
        let root = std::env::temp_dir().join(format!(
            "leio-code-symlinked-include-root-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("real")).expect("mkdir real");
        fs::write(root.join("real/app.ts"), "export const a = 1;\n").expect("write ts");
        fs::create_dir_all(root.join(".leio-code")).expect("mkdir config");
        fs::write(
            root.join(".leio-code/config.toml"),
            "version = 1\ninclude_roots = [\"linked\"]\n",
        )
        .expect("write config");
        std::os::unix::fs::symlink(root.join("real"), root.join("linked")).expect("symlink");

        let discovered = discover_source_files(&root).expect("discover");

        assert!(
            discovered
                .unreachable_include_roots
                .iter()
                .any(|warning| warning.contains("`linked`") && warning.contains("symlink")),
            "a symlinked include_root must be reported: {:?}",
            discovered.unreachable_include_roots
        );
        assert!(
            !discovered
                .paths
                .iter()
                .any(|path| path.to_string_lossy().contains("linked/")),
            "the walk must not descend into the symlink"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_real_include_root_directory_is_not_reported() {
        // The warning must stay quiet for ordinary configuration, or it trains
        // readers to ignore it.
        let root = std::env::temp_dir().join(format!(
            "leio-code-real-include-root-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("real")).expect("mkdir real");
        fs::write(root.join("real/app.ts"), "export const a = 1;\n").expect("write ts");
        fs::create_dir_all(root.join(".leio-code")).expect("mkdir config");
        fs::write(
            root.join(".leio-code/config.toml"),
            "version = 1\ninclude_roots = [\"real\"]\n",
        )
        .expect("write config");

        let discovered = discover_source_files(&root).expect("discover");

        assert!(
            discovered.unreachable_include_roots.is_empty(),
            "a real directory must not warn: {:?}",
            discovered.unreachable_include_roots
        );
        assert!(
            discovered
                .paths
                .iter()
                .any(|path| path.ends_with("real/app.ts")),
            "the real include_root must still be indexed"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_include_root_is_reported() {
        let root = std::env::temp_dir().join(format!(
            "leio-code-missing-include-root-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".leio-code")).expect("mkdir config");
        fs::write(
            root.join(".leio-code/config.toml"),
            "version = 1\ninclude_roots = [\"gone\"]\n",
        )
        .expect("write config");

        let discovered = discover_source_files(&root).expect("discover");

        assert!(
            discovered
                .unreachable_include_roots
                .iter()
                .any(|warning| warning.contains("`gone`") && warning.contains("does not exist")),
            "a missing include_root must be reported: {:?}",
            discovered.unreachable_include_roots
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn discovery_includes_csharp_and_razor_sources() {
        let root = std::env::temp_dir().join(format!(
            "leio-code-csharp-razor-discovery-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("mkdir src");
        fs::write(
            root.join("src/Program.cs"),
            "var builder = WebApplication.CreateBuilder(args);\nvar app = builder.Build();\napp.Run();\n",
        )
        .expect("write csharp");
        fs::write(
            root.join("src/Index.razor"),
            "<h1>Hello</h1>\n@code { public void Save() {} }\n",
        )
        .expect("write razor");

        let discovered = discover_source_files(&root).expect("discover");
        assert!(
            discovered
                .paths
                .iter()
                .any(|path| path.ends_with("Program.cs"))
        );
        assert!(
            discovered
                .paths
                .iter()
                .any(|path| path.ends_with("Index.razor"))
        );

        let index_path = default_index_path(&root);
        let index = build_or_update_index(&root, &index_path, true).expect("index sources");
        assert!(index.files.iter().any(|file| {
            file.path.ends_with("Program.cs") && file.language == SourceLanguage::CSharp
        }));
        assert!(index.files.iter().any(|file| {
            file.path.ends_with("Index.razor") && file.language == SourceLanguage::Razor
        }));
        assert!(index.all_symbols().any(|symbol| symbol.name == "Save"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn index_summary_roundtrip_and_freshness_gate() {
        let root =
            std::env::temp_dir().join(format!("leio-code-index-summary-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".leio-code")).expect("mkdir");

        let index_path = default_index_path(&root);
        let summary_path = default_index_summary_path(&root);
        let index = RepoIndex {
            version: INDEX_VERSION,
            root: root.display().to_string(),
            indexed_at: "2026-06-19T00:00:00Z".to_string(),
            files: vec![FileRecord {
                path: "cartridges/demo/src/main.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 12,
                modified_unix_ms: 0,
                symbols: Vec::new(),
                env_vars: Vec::new(),
                redis_keys: Vec::new(),
                subprocess_calls: Vec::new(),
                http_calls: Vec::new(),
                unresolved_edges: Vec::new(),
            }],
            deploy_targets: vec![DeployTargetRecord {
                name: "api".to_string(),
                path: "deploy/api.toml".to_string(),
                profile: None,
                readiness_target: None,
                deploy_class: None,
                topology: None,
                ui_role: None,
                ui_path: None,
                frontend_project: None,
                backend_profile: None,
                secret_set: None,
                health_checks: Vec::new(),
                smoke_suite: None,
                rollback_command: None,
                cartridges: vec!["demo".to_string()],
                required_integrations: Vec::new(),
                promotion_policy: None,
            }],
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: CrossLanguageGraph::default(),
            k8s_configmaps: Vec::new(),
        };

        save_index(&index_path, &index).expect("save index");
        let summary = load_index_summary(&summary_path).expect("load summary");
        assert_eq!(summary.file_count, 1);
        assert!(summary.workspace_facets.has_cartridges);
        assert!(load_fresh_index_summary(&root, &index_path).is_some());

        // An index with zero files is not a code view, however fresh the
        // stamp: it means language coverage missed the whole tree, and a
        // caller that trusts it reports full coverage of nothing. This lived
        // in leio-harness alone, so CLI, MCP and status still called it fresh.
        let mut empty = index.clone();
        empty.files.clear();
        save_index(&index_path, &empty).expect("save empty index");
        assert_eq!(
            load_index_summary(&summary_path)
                .expect("load summary")
                .file_count,
            0
        );
        assert!(
            load_fresh_index_summary(&root, &index_path).is_none(),
            "empty index reported as a fresh code view"
        );

        // Rewrite the index with different content: its size no longer matches the
        // size stamped into the summary, so the stat-equality freshness check must
        // reject it. This is deterministic regardless of filesystem mtime
        // granularity — the bug the old `summary_mtime < index_mtime` ordering had.
        fs::write(&index_path, b"{\"version\":99,\"corrupted\":true}").expect("corrupt index");
        assert!(load_fresh_index_summary(&root, &index_path).is_none());

        let _ = fs::remove_dir_all(root);
    }
}
