//! JSON-LD rendering for query envelopes plus a tiny jq-subset `--where`
//! filter, and the append-only PROV event journal.
//!
//! # `@context`
//!
//! [`CONTEXT`] is the vocabulary IRI prefix used by the rest of the Example
//! stack for leio-code emissions. The embedded context maps terms to that
//! namespace without a remote document fetch. A vocabulary IRI is not itself
//! a remote context document.
//!
//! # Envelope shape
//!
//! [`render_envelope_as_jsonld`] takes the existing [`QueryEnvelope`] and
//! returns a `serde_json::Value` whose root object carries:
//!
//! - `@context`: an embedded JSON-LD 1.1 term map using [`CONTEXT`].
//! - `@id`: `urn:leio-code:query:{query_id}` — stable per envelope.
//! - `@type`: `FindResult` for `kind = "find"`, `ExplainResult` for
//!   `kind = "explain"`, otherwise `{Capitalized}Result`.
//! - All existing envelope fields (`summary`, `confidence`, `evidence`,
//!   `entities`, `warnings`, `meta`, `timing_ms`) are passed through.
//! - Each entry in `entities` is augmented with an `@type` field derived from
//!   the envelope's `query_id` prefix (e.g. `find_env` → `EnvVar`,
//!   `explain_deploy_target` → `DeployTarget`).
//! - When a repo root is bound, every path is expanded to an absolute
//!   `fullPath` and a `file://` `@id`. The activity lists those files under
//!   W3C PROV `used` / `wasGeneratedBy` / `wasAssociatedWith` so a later
//!   SPARQL ingest can reconstruct the full provenance chain.
//!
//! # `--where` jq subset
//!
//! [`apply_where_filter`] supports exactly two filter shapes, both anchored on
//! `.entities[] | select(<predicate>)`:
//!
//! 1. **Equality / inequality on a field path against a JSON literal:**
//!    ```text
//!    .entities[] | select(<field-path> == <json-literal>)
//!    .entities[] | select(<field-path> != <json-literal>)
//!    ```
//!    where `<field-path>` is `.ident(.ident|[index])*` and `<json-literal>`
//!    is any value `serde_json` accepts (string, number, bool, null, array,
//!    object).
//!
//! 2. **Substring containment on a string field:**
//!    ```text
//!    .entities[] | select(<field-path> | contains(<string-literal>))
//!    ```
//!    The literal must be a JSON string; the field's value is also coerced to
//!    string. (This is the minimum useful subset — full jq `contains` accepts
//!    arrays/objects but those round-trips need a real jq.)
//!
//! Anything else — pipelines with `map`, comparators like `>`, multiple
//! selects, etc. — returns `Err(anyhow!("unsupported …"))`. The error message
//! always names what was expected so callers can tell users to pipe through
//! real `jq` for richer filtering.
//!
//! The returned value is the same envelope object with `entities` replaced by
//! the filtered list. All envelope-level fields (`@context`, `@id`, `@type`,
//! `summary`, etc.) are preserved so downstream jq / SPARQL pipelines keep
//! working.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value, json};

use crate::model::{EvidenceItem, QueryEnvelope};

/// Vocabulary IRI prefix for leio-code JSON-LD output.
///
/// This is a namespace identifier, not a remote context document. The
/// embedded context maps terms to it without attempting to fetch this URL.
pub const CONTEXT: &str = "https://ontology.getjai.com/leio-code/v1#";

/// Offline context: opaque metadata stays lossless as rdf:JSON while normal
/// entity/evidence fields remain queryable predicates. Full-IRI coercions
/// preserve the existing JSON strings used by journal consumers.
pub fn embedded_context() -> Value {
    serde_json::from_str::<Value>(include_str!("../mcp/contracts/context.jsonld"))
        .expect("bundled JSON-LD context is valid")["@context"]
        .clone()
}

/// W3C PROV namespace. Events cite these IRIs so the document stays valid
/// JSON-LD without rewriting [`CONTEXT`].
pub const PROV_NS: &str = "http://www.w3.org/ns/prov#";

/// Conformance note — JSON-LD API Best Practices (WG Note, advisory). The
/// emitter uses an offline context, node references and native JSON values.
/// Directional values pass through unchanged. Oxigraph's rdf-12 feature
/// ingests them as rdf:dirLangString; this does not establish complete JSON-LD
/// 1.2 processor conformance (the current parser rejects @version 1.2).
const PROV_USED: &str = "http://www.w3.org/ns/prov#used";
const PROV_GENERATED: &str = "http://www.w3.org/ns/prov#wasGeneratedBy";
const PROV_ASSOCIATED: &str = "http://www.w3.org/ns/prov#wasAssociatedWith";
const PROV_ATTRIBUTED: &str = "http://www.w3.org/ns/prov#wasAttributedTo";
const PROV_ENDED: &str = "http://www.w3.org/ns/prov#endedAtTime";
const PROV_ENTITY: &str = "http://www.w3.org/ns/prov#Entity";
const PROV_ACTIVITY: &str = "http://www.w3.org/ns/prov#Activity";
const PROV_AGENT: &str = "http://www.w3.org/ns/prov#SoftwareAgent";

/// Cap on `prov:used` file entities per event.
///
/// A `find` over a monorepo can cite thousands of paths. 64 files is enough
/// to reconstruct the walk without bloating the NDJSON journal.
const MAX_USED_PATHS: usize = 64;

thread_local! {
    static BOUND_REPO: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Bind the current repo so later envelopes get absolute `file://` paths.
pub fn bind_repo(root: &Path) {
    BOUND_REPO.with(|slot| {
        *slot.borrow_mut() = Some(root.to_path_buf());
    });
}

/// Repo last passed to [`bind_repo`] on this thread.
pub fn bound_repo() -> Option<PathBuf> {
    BOUND_REPO.with(|slot| slot.borrow().clone())
}

/// Append-only JSON-LD event journal for this checkout.
///
/// Default: `{repo}/.leio-code/events/events.ndjson` (one file per worktree).
/// `LEIO_EVENTS_DIR` collects many repos/worktrees as
/// `{dir}/{repo_id}/{worktreeHash}-{branch}.ndjson`.
pub fn events_path(repo_root: &Path) -> PathBuf {
    events_path_for(repo_root, &crate::checkout::discover(repo_root))
}

fn events_path_for(repo_root: &Path, checkout: &crate::checkout::Checkout) -> PathBuf {
    if let Some(dir) = std::env::var("LEIO_EVENTS_DIR")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return shared_events_path(Path::new(&dir), checkout);
    }
    repo_root
        .join(".leio-code")
        .join("events")
        .join("events.ndjson")
}

/// Journal path when many repos/worktrees share one events directory.
pub fn shared_events_path(events_dir: &Path, checkout: &crate::checkout::Checkout) -> PathBuf {
    events_dir
        .join(&checkout.repo_id)
        .join(format!("{}.ndjson", checkout.journal_stem()))
}

/// Map an entity-kind tag (derived from the envelope's `query_id`) to a
/// JSON-LD `@type` label for individual entities.
fn entity_type_for(query_id_prefix: &str) -> &'static str {
    match query_id_prefix {
        "find_symbol" => "Symbol",
        "find_env" | "explain_env" => "EnvVar",
        "find_redis" | "explain_redis" => "RedisKey",
        "find_deploy_target" | "explain_deploy_target" => "DeployTarget",
        "find_cartridge" | "explain_cartridge" => "Cartridge",
        "find_api_route" => "ApiRoute",
        "find_docker_service" => "DockerService",
        "find_subprocess" => "SubprocessCall",
        // Phase 7 (P0 #2) cross-language polish: binary/route registry +
        // dispatched caller queries. `explain_binary` / `explain_route`
        // produce mixed-shape entity arrays (declaration, callers, unresolved
        // edges) so consumers should branch on the per-entity `@kind` field
        // emitted by the query layer, not the envelope-level `@type`.
        "find_binary" | "explain_binary" => "Binary",
        "find_route" | "explain_route" => "Route",
        "find_callers_binary" => "SpawnCallSite",
        "find_callers_route" => "HttpCallSite",
        // FCA induction Phase A: streamed formal-context export.
        // The envelope `kind` is `"export"`, so the root document type is
        // `ExportResult` (handled by `envelope_type_for`). Per-entity rows
        // carry `@type: "FormalContext"`.
        "export_formal_context_stream" => "FormalContext",
        "knowledge_explain" => "KnowledgeSubject",
        "knowledge_sparql" => "SparqlBinding",
        "knowledge_status" | "knowledge_compile" => "WikiStore",
        "nav" => "NavNode",
        "index" => "IndexSnapshot",
        "status" => "WorkspaceStatus",
        "watch" => "WatchTrigger",
        _ => "Entity",
    }
}

/// Strip the trailing `-{unix_nanos}` from a [`crate::doctors::utils::query_id`]
/// to recover the static prefix (e.g. `find_env` from `find_env-1700000000000`).
fn query_id_prefix(query_id: &str) -> &str {
    match query_id.rsplit_once('-') {
        Some((prefix, suffix)) if suffix.chars().all(|c| c.is_ascii_digit()) => prefix,
        _ => query_id,
    }
}

fn envelope_type_for(kind: &str) -> String {
    match kind {
        "find" => "FindResult".to_string(),
        "explain" => "ExplainResult".to_string(),
        other => {
            let mut chars = other.chars();
            let head = chars.next().map(|c| c.to_ascii_uppercase()).into_iter();
            let rest = chars;
            format!(
                "{}{}Result",
                head.collect::<String>(),
                rest.collect::<String>()
            )
        }
    }
}

/// Render a [`QueryEnvelope`] as a JSON-LD document. See module-level docs for
/// the exact shape.
pub fn render_envelope_as_jsonld(envelope: &QueryEnvelope) -> Value {
    render_envelope_as_jsonld_in(envelope, bound_repo().as_deref())
}

/// Render JSON-LD with paths resolved against `repo_root`.
pub fn render_envelope_as_jsonld_in(envelope: &QueryEnvelope, repo_root: Option<&Path>) -> Value {
    let checkout = repo_root.map(crate::checkout::discover);
    let entity_type = entity_type_for(query_id_prefix(&envelope.query_id));
    let activity_id = match checkout.as_ref() {
        Some(checkout) => format!(
            "urn:leio-code:query:{}:{}",
            checkout.repo_id, envelope.query_id
        ),
        None => format!("urn:leio-code:query:{}", envelope.query_id),
    };
    let mut used: Vec<Value> = Vec::new();
    let mut seen = BTreeSet::new();

    let entities: Vec<Value> = envelope
        .entities
        .iter()
        .map(|entity| {
            let mut obj = match entity {
                Value::Object(map) => map.clone(),
                _ => {
                    let mut m = Map::new();
                    m.insert("value".to_string(), entity.clone());
                    m
                }
            };
            obj.insert("@type".to_string(), json!(entity_type));
            if let Some(rel) = entity_rel_path(entity)
                && let Some(full) = resolve_full_path(repo_root, &rel)
            {
                remember_used(&mut used, &mut seen, &rel, &full, checkout.as_ref());
                stamp_path(&mut obj, &rel, &full, checkout.as_ref(), &activity_id);
            }
            Value::Object(obj)
        })
        .collect();

    let evidence: Vec<Value> = envelope
        .evidence
        .iter()
        .map(|item| {
            render_evidence(
                item,
                repo_root,
                &activity_id,
                checkout.as_ref(),
                &mut used,
                &mut seen,
            )
        })
        .collect();

    let mut root = Map::new();
    // `schema_version` first so it serializes as the first JSON key. Matches
    // the field order on `QueryEnvelope` and the contract in
    // `docs/output-schema.md` §7. Relies on `serde_json::Map` preserving
    // insertion order when serialized via `to_string` / `to_string_pretty`.
    root.insert(
        "schema_version".to_string(),
        json!(crate::model::SCHEMA_VERSION),
    );
    root.insert("@context".to_string(), embedded_context());
    root.insert("@id".to_string(), json!(activity_id));
    root.insert(
        "@type".to_string(),
        json!(envelope_type_for(&envelope.kind)),
    );
    root.insert(
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type".to_string(),
        json!({"@id": PROV_ACTIVITY}),
    );
    root.insert("query_id".to_string(), json!(envelope.query_id));
    root.insert("kind".to_string(), json!(envelope.kind));
    root.insert("summary".to_string(), json!(envelope.summary));
    root.insert("confidence".to_string(), json!(envelope.confidence));
    root.insert("entities".to_string(), Value::Array(entities));
    root.insert("evidence".to_string(), Value::Array(evidence));
    root.insert("warnings".to_string(), json!(envelope.warnings));
    if let Some(meta) = &envelope.meta {
        root.insert("meta".to_string(), meta.clone());
    }
    root.insert("timing_ms".to_string(), json!(envelope.timing_ms));
    if let Some(repo) = repo_root {
        root.insert("repoRoot".to_string(), json!(repo.display().to_string()));
    }
    if let Some(checkout) = checkout.as_ref() {
        root.insert(
            "checkout".to_string(),
            serde_json::to_value(checkout).unwrap_or(Value::Null),
        );
        root.insert(PROV_ATTRIBUTED.to_string(), repo_node(checkout));
        root.insert("worktree".to_string(), worktree_node(checkout));
    }
    root.insert(PROV_ENDED.to_string(), json!(rfc3339_now()));
    root.insert(PROV_ASSOCIATED.to_string(), agent_node());
    if !used.is_empty() {
        root.insert(PROV_USED.to_string(), Value::Array(used));
    }

    Value::Object(root)
}

/// Persist one JSON-LD event line under `.leio-code/events/events.ndjson`.
///
/// # Errors
///
/// Returns an error when the events directory cannot be created or the
/// append fails. Callers treat this as best-effort.
pub fn record_event(repo_root: &Path, envelope: &QueryEnvelope) -> Result<()> {
    if events_disabled() {
        return Ok(());
    }
    let checkout = crate::checkout::discover(repo_root);
    let path = events_path_for(repo_root, &checkout);
    let lock_root = path.parent().unwrap_or(repo_root);
    let _lock = crate::sidecar::acquire_lock(lock_root, "events")?;
    let doc = render_envelope_as_jsonld_in(envelope, Some(repo_root));
    let line = serde_json::to_vec(&doc).context("serialize event jsonld")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    file.write_all(&line)
        .and_then(|()| file.write_all(b"\n"))
        .with_context(|| format!("append {}", path.display()))?;
    Ok(())
}

/// Record against the thread-bound repo. No-ops when unbound or disabled.
pub fn record_bound_event(envelope: &QueryEnvelope) {
    if let Some(repo) = bound_repo() {
        let _ = record_event(&repo, envelope);
    }
}

fn events_disabled() -> bool {
    std::env::var("LEIO_DISABLE_EVENTS")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

fn render_evidence(
    item: &EvidenceItem,
    repo_root: Option<&Path>,
    activity_id: &str,
    checkout: Option<&crate::checkout::Checkout>,
    used: &mut Vec<Value>,
    seen: &mut BTreeSet<String>,
) -> Value {
    let mut obj = json!({
        "@type": "Evidence",
        "kind": item.kind,
        "path": item.path,
        "line": item.line,
        "detail": item.detail,
    });
    if let Some(full) = resolve_full_path(repo_root, &item.path) {
        remember_used(used, seen, &item.path, &full, checkout);
        if let Some(map) = obj.as_object_mut() {
            stamp_path(map, &item.path, &full, checkout, activity_id);
            map.insert("@id".to_string(), json!(file_iri_line(&full, item.line)));
        }
    }
    obj
}

fn entity_rel_path(entity: &Value) -> Option<String> {
    for key in ["path", "source_path", "file"] {
        if let Some(path) = entity
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            return Some(path.to_string());
        }
    }
    None
}

fn resolve_full_path(repo_root: Option<&Path>, rel: &str) -> Option<String> {
    if rel.is_empty() {
        return None;
    }
    let path = Path::new(rel);
    if path.is_absolute() {
        return Some(path.display().to_string());
    }
    let root = repo_root?;
    let joined = root.join(rel);
    Some(
        joined
            .canonicalize()
            .unwrap_or(joined)
            .display()
            .to_string(),
    )
}

fn remember_used(
    used: &mut Vec<Value>,
    seen: &mut BTreeSet<String>,
    rel: &str,
    full: &str,
    checkout: Option<&crate::checkout::Checkout>,
) {
    if used.len() >= MAX_USED_PATHS || !seen.insert(full.to_string()) {
        return;
    }
    let mut node = json!({
        "@id": file_iri(full),
        "@type": [PROV_ENTITY, "File"],
        "path": rel,
        "fullPath": full,
    });
    if let Some(checkout) = checkout {
        node["repo"] = json!(checkout.repo_iri());
        node["worktree"] = json!(checkout.worktree);
        node["branch"] = json!(checkout.branch);
        node["head"] = json!(checkout.head);
        if let Some(git_path) = checkout.git_path(full) {
            node["gitPath"] = json!(git_path);
        }
    }
    used.push(node);
}

fn stamp_path(
    obj: &mut Map<String, Value>,
    rel: &str,
    full: &str,
    checkout: Option<&crate::checkout::Checkout>,
    activity_id: &str,
) {
    obj.insert("fullPath".to_string(), json!(full));
    obj.insert("@id".to_string(), json!(file_iri(full)));
    obj.insert(PROV_GENERATED.to_string(), json!(activity_id));
    if let Some(checkout) = checkout {
        obj.insert("repo".to_string(), json!(checkout.repo_iri()));
        obj.insert("worktree".to_string(), json!(checkout.worktree));
        obj.insert("branch".to_string(), json!(checkout.branch));
        obj.insert("head".to_string(), json!(checkout.head));
        if let Some(git_path) = checkout.git_path(full) {
            obj.insert("gitPath".to_string(), json!(git_path));
        }
    }
    let _ = rel;
}

fn repo_node(checkout: &crate::checkout::Checkout) -> Value {
    json!({
        "@id": checkout.repo_iri(),
        "@type": [PROV_ENTITY, "Repository"],
        "repoId": checkout.repo_id,
        "origin": checkout.origin,
        "commonDir": checkout.common_dir,
    })
}

fn worktree_node(checkout: &crate::checkout::Checkout) -> Value {
    json!({
        "@id": checkout.worktree_iri(),
        "@type": [PROV_ENTITY, "Worktree"],
        "worktree": checkout.worktree,
        "inspect": checkout.inspect,
        "branch": checkout.branch,
        "head": checkout.head,
        "gitDir": checkout.git_dir,
        "repo": checkout.repo_iri(),
    })
}

fn file_iri(full_path: &str) -> String {
    // Encode bytes, including fragment/query delimiters and literal percent.
    // Do not reinterpret a Unix filename's backslash as a path separator.
    let normalized = if cfg!(windows) {
        full_path.replace('\\', "/")
    } else {
        full_path.to_string()
    };
    let mut path = String::new();
    for byte in normalized.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~:".contains(&byte) {
            path.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(&mut path, "%{byte:02X}").expect("write to string");
        }
    }
    if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    }
}

fn file_iri_line(full_path: &str, line: Option<usize>) -> String {
    match line {
        Some(line) => format!("{}#L{line}", file_iri(full_path)),
        None => file_iri(full_path),
    }
}

fn agent_node() -> Value {
    let slug = crate::sidecar::session_slug();
    json!({
        "@id": format!("urn:leio-code:agent:{}", slug.as_deref().unwrap_or("local")),
        "@type": PROV_AGENT,
        "session": slug,
        "binary": "leio-code",
        "version": env!("CARGO_PKG_VERSION"),
    })
}

fn rfc3339_now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

// --------------------------------------------------------------------------
// `--where` filter
// --------------------------------------------------------------------------

/// Apply a tiny jq-subset filter to a JSON-LD envelope. See the module-level
/// docs for the supported grammar.
pub fn apply_where_filter(envelope: &Value, jq_expr: &str) -> Result<Value> {
    let pred = parse_where(jq_expr)?;

    let envelope_obj = envelope
        .as_object()
        .ok_or_else(|| anyhow!("envelope is not a JSON object"))?;
    let entities = envelope_obj
        .get("entities")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("envelope has no `entities` array to filter"))?;

    let filtered: Vec<Value> = entities
        .iter()
        .filter(|entity| pred.evaluate(entity))
        .cloned()
        .collect();

    let mut out = envelope_obj.clone();
    out.insert("entities".to_string(), Value::Array(filtered));
    Ok(Value::Object(out))
}

/// Compiled predicate from a parsed `--where` expression.
#[derive(Debug)]
enum Predicate {
    Equals(Vec<PathStep>, Value),
    NotEquals(Vec<PathStep>, Value),
    /// Substring containment: the value at `path`, coerced to string, must
    /// contain `needle` as a substring.
    ContainsStr(Vec<PathStep>, String),
}

#[derive(Debug)]
enum PathStep {
    Field(String),
    Index(usize),
}

impl Predicate {
    fn evaluate(&self, entity: &Value) -> bool {
        match self {
            Predicate::Equals(path, target) => {
                resolve(entity, path).map(|v| v == target).unwrap_or(false)
            }
            Predicate::NotEquals(path, target) => {
                resolve(entity, path).map(|v| v != target).unwrap_or(true)
            }
            Predicate::ContainsStr(path, needle) => resolve(entity, path)
                .and_then(|v| v.as_str().map(|s| s.contains(needle.as_str())))
                .unwrap_or(false),
        }
    }
}

fn resolve<'a>(value: &'a Value, path: &[PathStep]) -> Option<&'a Value> {
    let mut cursor = value;
    for step in path {
        cursor = match (cursor, step) {
            (Value::Object(map), PathStep::Field(name)) => map.get(name)?,
            (Value::Array(arr), PathStep::Index(i)) => arr.get(*i)?,
            _ => return None,
        };
    }
    Some(cursor)
}

/// Parse a `.entities[] | select(<predicate>)` expression. The accepted
/// grammar is intentionally tiny — see module docs.
fn parse_where(expr: &str) -> Result<Predicate> {
    let trimmed = expr.trim();
    let rest = trimmed
        .strip_prefix(".entities[]")
        .ok_or_else(|| anyhow!("unsupported filter: expected `.entities[] | select(...)`"))?
        .trim_start();
    let rest = rest
        .strip_prefix('|')
        .ok_or_else(|| anyhow!("unsupported filter: expected `|` after `.entities[]`"))?
        .trim_start();
    let rest = rest
        .strip_prefix("select(")
        .ok_or_else(|| anyhow!("unsupported filter: expected `select(...)`"))?;
    let inner = rest
        .strip_suffix(')')
        .ok_or_else(|| anyhow!("unsupported filter: missing closing `)` on `select(...)`"))?
        .trim();

    parse_predicate(inner)
}

/// Parse the body of `select(<predicate>)`.
fn parse_predicate(inner: &str) -> Result<Predicate> {
    // Try `<path> | contains(<json-literal>)` first because `==` and `!=`
    // never appear inside the `contains(...)` call.
    if let Some((lhs, rhs)) = split_top_level_pipe(inner) {
        let path = parse_path(lhs.trim())?;
        let rhs = rhs.trim();
        let rest = rhs.strip_prefix("contains(").ok_or_else(|| {
            anyhow!(
                "unsupported filter: only `contains(<literal>)` is supported on the right of `|`"
            )
        })?;
        let lit_src = rest
            .strip_suffix(')')
            .ok_or_else(|| anyhow!("unsupported filter: missing closing `)` on `contains(...)`"))?;
        let literal: Value = serde_json::from_str(lit_src.trim())
            .map_err(|e| anyhow!("unsupported filter: contains literal must be JSON: {e}"))?;
        let needle = literal.as_str().ok_or_else(|| {
            anyhow!("unsupported filter: contains(<literal>) only supports string literals")
        })?;
        return Ok(Predicate::ContainsStr(path, needle.to_string()));
    }

    // Otherwise expect `<path> (==|!=) <json-literal>`.
    if let Some((lhs, rhs)) = split_top_level(inner, "==") {
        let path = parse_path(lhs.trim())?;
        let literal: Value = serde_json::from_str(rhs.trim()).map_err(|e| {
            anyhow!("unsupported filter: right-hand side of `==` must be a JSON literal: {e}")
        })?;
        return Ok(Predicate::Equals(path, literal));
    }
    if let Some((lhs, rhs)) = split_top_level(inner, "!=") {
        let path = parse_path(lhs.trim())?;
        let literal: Value = serde_json::from_str(rhs.trim()).map_err(|e| {
            anyhow!("unsupported filter: right-hand side of `!=` must be a JSON literal: {e}")
        })?;
        return Ok(Predicate::NotEquals(path, literal));
    }

    bail!(
        "unsupported filter: expected `<path> == <literal>`, `<path> != <literal>`, or `<path> | contains(<string>)`"
    )
}

/// Parse a `.field(.field|[index])*` path. Returns the sequence of steps.
fn parse_path(src: &str) -> Result<Vec<PathStep>> {
    let mut chars = src.chars().peekable();
    if chars.next() != Some('.') {
        return Err(anyhow!("unsupported filter: path must start with `.`"));
    }
    let mut steps = Vec::new();
    let mut ident = String::new();
    let flush_ident = |ident: &mut String, steps: &mut Vec<PathStep>| {
        if !ident.is_empty() {
            steps.push(PathStep::Field(std::mem::take(ident)));
        }
    };
    while let Some(&c) = chars.peek() {
        match c {
            '.' => {
                chars.next();
                flush_ident(&mut ident, &mut steps);
            }
            '[' => {
                flush_ident(&mut ident, &mut steps);
                chars.next();
                let mut idx = String::new();
                while let Some(&c) = chars.peek() {
                    if c == ']' {
                        break;
                    }
                    idx.push(c);
                    chars.next();
                }
                if chars.next() != Some(']') {
                    return Err(anyhow!("unsupported filter: unterminated `[index]`"));
                }
                let n: usize = idx.trim().parse().map_err(|_| {
                    anyhow!("unsupported filter: array index must be a non-negative integer")
                })?;
                steps.push(PathStep::Index(n));
            }
            c if c.is_alphanumeric() || c == '_' => {
                ident.push(c);
                chars.next();
            }
            other => {
                return Err(anyhow!(
                    "unsupported filter: unexpected character `{other}` in path"
                ));
            }
        }
    }
    flush_ident(&mut ident, &mut steps);
    if steps.is_empty() {
        return Err(anyhow!("unsupported filter: empty path"));
    }
    Ok(steps)
}

/// Split `s` at the first top-level occurrence of `needle` (== or !=), ignoring
/// occurrences inside `( )`, `[ ]`, `{ }`, or string literals. Returns `None`
/// if not found.
fn split_top_level<'a>(s: &'a str, needle: &str) -> Option<(&'a str, &'a str)> {
    let bytes = s.as_bytes();
    let needle_bytes = needle.as_bytes();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            _ => {}
        }
        if depth == 0 && bytes[i..].starts_with(needle_bytes) {
            // Avoid matching `==` when looking for `!=` and vice versa: the
            // caller passes the exact two-char operator, and == != are
            // length-2 so this prefix check is unambiguous.
            return Some((&s[..i], &s[i + needle_bytes.len()..]));
        }
        i += 1;
    }
    None
}

/// Like [`split_top_level`] but for the `|` infix operator with whitespace
/// padding (`<path> | contains(...)`).
fn split_top_level_pipe(s: &str) -> Option<(&str, &str)> {
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'|' if depth == 0 => return Some((&s[..i], &s[i + 1..])),
            _ => {}
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EvidenceItem, QueryEnvelope};

    fn fake_envelope(kind: &str, query_prefix: &str) -> QueryEnvelope {
        QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: format!("{query_prefix}-1700000000000"),
            kind: kind.to_string(),
            summary: "fake".to_string(),
            confidence: 1.0,
            entities: vec![json!({"name": "FOO", "path": ".env"})],
            evidence: vec![EvidenceItem {
                kind: "env_var".to_string(),
                path: ".env".to_string(),
                line: Some(1),
                detail: "FOO".to_string(),
            }],
            warnings: vec![],
            meta: None,
            timing_ms: 0,
        }
    }

    #[test]
    fn envelope_type_for_known_kinds() {
        assert_eq!(envelope_type_for("find"), "FindResult");
        assert_eq!(envelope_type_for("explain"), "ExplainResult");
        assert_eq!(envelope_type_for("audit"), "AuditResult");
    }

    #[test]
    fn entity_type_resolution() {
        assert_eq!(entity_type_for("find_env"), "EnvVar");
        assert_eq!(entity_type_for("explain_env"), "EnvVar");
        assert_eq!(entity_type_for("find_subprocess"), "SubprocessCall");
        assert_eq!(entity_type_for("not_a_known_prefix"), "Entity");
    }

    #[test]
    fn query_id_prefix_strips_timestamp() {
        assert_eq!(query_id_prefix("find_env-1700000000000"), "find_env");
        assert_eq!(
            query_id_prefix("explain_deploy_target-42"),
            "explain_deploy_target"
        );
        assert_eq!(query_id_prefix("no_suffix"), "no_suffix");
    }

    #[test]
    fn render_adds_context_id_type() {
        let env = fake_envelope("find", "find_env");
        let doc = render_envelope_as_jsonld(&env);
        assert_eq!(doc["@context"], embedded_context());
        assert_eq!(doc["@type"], json!("FindResult"));
        assert!(
            doc["@id"]
                .as_str()
                .unwrap()
                .starts_with("urn:leio-code:query:")
        );
        assert_eq!(doc["entities"][0]["@type"], json!("EnvVar"));
    }

    #[test]
    fn render_in_repo_emits_absolute_file_iri_and_prov_used() {
        let dir = tempfile::tempdir().expect("tempdir");
        let env_path = dir.path().join(".env");
        std::fs::write(&env_path, "FOO=1\n").unwrap();
        let env = fake_envelope("find", "find_env");
        let doc = render_envelope_as_jsonld_in(&env, Some(dir.path()));
        let full = doc["entities"][0]["fullPath"].as_str().unwrap();
        assert!(
            Path::new(full).is_absolute(),
            "fullPath should be absolute: {full}"
        );
        assert!(
            full.ends_with(".env"),
            "fullPath should keep the file name: {full}"
        );
        assert!(
            doc["entities"][0]["@id"]
                .as_str()
                .unwrap()
                .starts_with("file://"),
            "{}",
            doc["entities"][0]
        );
        let used = doc[PROV_USED].as_array().expect("prov:used");
        assert_eq!(used.len(), 1);
        assert_eq!(used[0]["fullPath"], full);
        assert_eq!(doc["evidence"][0]["fullPath"], full);
        assert!(
            doc[PROV_ASSOCIATED]["@id"]
                .as_str()
                .unwrap()
                .starts_with("urn:leio-code:agent:")
        );
        assert!(
            doc["checkout"]["repoId"]
                .as_str()
                .unwrap()
                .starts_with("local-")
        );
        assert_eq!(
            doc["worktree"]["inspect"].as_str().unwrap(),
            dir.path().display().to_string()
        );
    }

    #[test]
    fn git_checkout_stamps_branch_head_and_git_path() {
        let Some(repo) = crate::checkout::init_git_fixture() else {
            return;
        };
        std::fs::write(repo.path().join(".env"), "FOO=1\n").unwrap();
        let env = fake_envelope("find", "find_env");
        let doc = render_envelope_as_jsonld_in(&env, Some(repo.path()));
        assert_eq!(doc["checkout"]["branch"].as_str(), Some("main"));
        assert_eq!(doc["entities"][0]["branch"].as_str(), Some("main"));
        assert_eq!(doc["entities"][0]["gitPath"].as_str(), Some(".env"));
        assert_eq!(doc["checkout"]["head"].as_str().unwrap().len(), 40);
        assert!(
            doc["@id"]
                .as_str()
                .unwrap()
                .contains(doc["checkout"]["repoId"].as_str().unwrap())
        );
    }

    #[test]
    fn record_event_appends_jsonld_ndjson() {
        let dir = tempfile::tempdir().expect("tempdir");
        let env = fake_envelope("find", "find_env");
        record_event(dir.path(), &env).unwrap();
        record_event(dir.path(), &env).unwrap();
        let raw = std::fs::read_to_string(events_path(dir.path())).unwrap();
        let lines: Vec<&str> = raw.lines().filter(|line| !line.is_empty()).collect();
        assert_eq!(lines.len(), 2);
        let doc: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(doc["@context"], embedded_context());
        assert_eq!(doc["@type"], "FindResult");
        assert!(doc["entities"][0]["fullPath"].as_str().is_some());
    }

    #[test]
    fn emitted_document_converts_offline_with_provenance_and_metadata() {
        use oxigraph::io::{JsonLdProfileSet, RdfFormat, RdfParser};
        use oxigraph::model::Term;
        let dir = tempfile::tempdir().unwrap();
        let name = "a #?% café.rs";
        std::fs::write(dir.path().join(name), "// evidence").unwrap();
        let mut env = fake_envelope("find", "find_symbol");
        env.entities = vec![json!({"name":"example", "path":name})];
        env.evidence[0].path = name.into();
        env.meta = Some(json!({"ordered":[2,1,2,null], "empty":[], "@private":"kept"}));
        let doc = render_envelope_as_jsonld_in(&env, Some(dir.path()));
        let bytes = serde_json::to_vec(&doc).unwrap();
        // No base IRI or remote context loader, and no patching the emitter output.
        let quads = RdfParser::from_format(RdfFormat::JsonLd {
            profile: JsonLdProfileSet::empty(),
        })
        .for_reader(bytes.as_slice())
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("actual emitted document must parse offline");
        let activity = doc["@id"].as_str().unwrap();
        let file = doc["entities"][0]["@id"].as_str().unwrap();
        assert!(file.ends_with("/a%20%23%3F%25%20caf%C3%A9.rs"), "{file}");
        let has_edge = |predicate: &str, object: &str| {
            quads.iter().any(|q| {
                q.predicate.as_str() == predicate
                    && matches!(&q.object,
                Term::NamedNode(node) if node.as_str() == object)
            })
        };
        assert!(has_edge(
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            PROV_ACTIVITY
        ));
        assert!(has_edge(PROV_GENERATED, activity));
        assert!(has_edge(PROV_USED, file));
        assert!(has_edge(
            &format!("{CONTEXT}evidence"),
            &format!("{file}#L1")
        ));
        let meta = quads
            .iter()
            .find(|q| q.predicate.as_str() == format!("{CONTEXT}meta"))
            .unwrap();
        let Term::Literal(literal) = &meta.object else {
            panic!("metadata is an RDF JSON literal")
        };
        assert_eq!(
            literal.datatype().as_str(),
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#JSON"
        );
        assert_eq!(
            serde_json::from_str::<Value>(literal.value()).unwrap(),
            env.meta.unwrap()
        );
        let ended = quads
            .iter()
            .find(|q| q.predicate.as_str() == PROV_ENDED)
            .unwrap();
        assert!(matches!(&ended.object, Term::Literal(value)
            if value.datatype().as_str() == "http://www.w3.org/2001/XMLSchema#dateTime"));
        let unbound = render_envelope_as_jsonld_in(&fake_envelope("find", "find_env"), None);
        assert!(
            unbound["entities"][0].get("@id").is_none(),
            "relative paths must not invent absolute file identities"
        );
    }

    #[test]
    fn filter_equals_string_literal() {
        let env = fake_envelope("find", "find_env");
        let doc = render_envelope_as_jsonld(&env);
        let out = apply_where_filter(&doc, r#".entities[] | select(.name == "FOO")"#).unwrap();
        assert_eq!(out["entities"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn filter_contains_substring() {
        let env = fake_envelope("find", "find_env");
        let doc = render_envelope_as_jsonld(&env);
        let out =
            apply_where_filter(&doc, r#".entities[] | select(.path | contains(".env"))"#).unwrap();
        assert_eq!(out["entities"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn unsupported_grammar_errors() {
        let env = fake_envelope("find", "find_env");
        let doc = render_envelope_as_jsonld(&env);
        let err = apply_where_filter(&doc, ".entities | map(.) | length > 3").unwrap_err();
        assert!(
            format!("{err}")
                .to_ascii_lowercase()
                .contains("unsupported")
        );
    }

    #[test]
    fn directional_value_object_passes_through_emitter() {
        let mut env = fake_envelope("find", "find_env");
        env.entities[0] = json!({
            "name": "welkom",
            "path": ".env",
            "title": {"@value": "\u{5e9}\u{5dc}\u{5d5}\u{5dd}", "@language": "he", "@direction": "rtl"},
        });
        let doc = render_envelope_as_jsonld(&env);
        // JSON-LD 1.1 value objects (and therefore JSON-LD 1.2 documents,
        // which the charter keeps backward compatible) must survive rendering
        // verbatim so the RDF 1.2 direction is never lost.
        assert_eq!(
            doc["entities"][0]["title"],
            json!({"@value": "\u{5e9}\u{5dc}\u{5d5}\u{5dd}", "@language": "he", "@direction": "rtl"})
        );
    }

    #[test]
    fn directional_value_object_ingests_as_dirlangstring() {
        use crate::knowledge_graph::SparqlQueryExt;
        use oxigraph::io::{JsonLdProfileSet, RdfFormat, RdfParser};
        use oxigraph::model::BaseDirection;
        use oxigraph::sparql::QueryResults;
        use oxigraph::store::Store;
        use std::io::Cursor;

        let mut env = fake_envelope("find", "find_env");
        env.entities[0] = json!({
            "name": "welkom",
            "path": ".env",
            "title": {"@value": "\u{5e9}\u{5dc}\u{5d5}\u{5dd}", "@language": "he", "@direction": "rtl"},
        });
        let doc = render_envelope_as_jsonld_in(&env, None);
        let doc_str = doc.to_string();

        let store = Store::new().expect("store");
        let parser = RdfParser::from_format(RdfFormat::JsonLd {
            profile: JsonLdProfileSet::empty(),
        })
        .with_base_iri("urn:leio:test")
        .expect("base iri");
        for quad in parser.for_reader(Cursor::new(doc_str.as_bytes())) {
            store.insert(&quad.expect("json-ld quad")).expect("insert");
        }

        // The literal stored as RDF 1.2 rdf:dirLangString with base direction.
        let mut directional = Vec::new();
        for quad in store.iter() {
            let quad = quad.expect("quad");
            if let oxigraph::model::Term::Literal(literal) = &quad.object
                && let Some(direction) = literal.direction()
            {
                assert_eq!(direction, BaseDirection::Rtl);
                assert_eq!(literal.language(), Some("he"));
                directional.push(literal.to_string());
            }
        }
        assert_eq!(directional.len(), 1, "{directional:?}");
        assert!(directional[0].contains("@he--rtl"), "{directional:?}");

        // W3C SPARQL 1.2 §17.4.2: direction is part of the literal term.
        let direction = store.sparql_query(
            r#"ASK { ?s ?p ?t FILTER(sameTerm(?t, STRLANGDIR("שלום", "he", "rtl")) && LANGDIR(?t) = "rtl") }"#,
        ).expect("SPARQL 1.2 directional functions");
        assert!(matches!(direction, QueryResults::Boolean(true)));

        // Language-only lookup also remains supported.
        let results = store
            .sparql_query("SELECT ?t WHERE { ?s ?p ?t FILTER(lang(?t) = \"he\") }")
            .expect("sparql");
        match results {
            QueryResults::Solutions(solutions) => assert_eq!(solutions.count(), 1),
            _other => panic!("expected solutions"),
        }
    }
}
