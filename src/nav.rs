//! Stateful node cursor persisted under `.leio-code/nav-session.json`
//! (or `.leio-code/sessions/nav-<id>.json` when `LEIO_SESSION` is set).
//!
//! Walks Arrow nodes, the code graph, and the concept lattice (cover
//! morphisms) without re-stating the current symbol on every call.
//! History/forward stacks survive process exits.
// Rust guideline compliant 2026-02-21

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::graph_query::{GraphDirection, query_call_graph, query_symbols_in};
use crate::local_nodes::{self, NodeHit};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};
use crate::query::find_symbols;

/// Cap on persisted history/forward entries.
///
/// 50 hops is enough for an agent walk without growing the session file
/// without bound. Older hops fall off the front.
const MAX_HISTORY: usize = 50;
const MAX_PAGE_SIZE: usize = 100;
const MEMBER_SAMPLE_LIMIT: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavAction {
    Here,
    Goto,
    Select,
    Callers,
    Callees,
    Neighbors,
    Related,
    Parent,
    Child,
    Peer,
    Align,
    Back,
    Forward,
    Reset,
    Explain,
}

impl NavAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Here => "here",
            Self::Goto => "goto",
            Self::Select => "select",
            Self::Callers => "callers",
            Self::Callees => "callees",
            Self::Neighbors => "neighbors",
            Self::Related => "related",
            Self::Parent => "parent",
            Self::Child => "child",
            Self::Peer => "peer",
            Self::Align => "align",
            Self::Back => "back",
            Self::Forward => "forward",
            Self::Reset => "reset",
            Self::Explain => "explain",
        }
    }
}

/// One node the cursor can sit on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NavNode {
    pub path: String,
    pub symbol: String,
    pub kind: String,
    /// Source line when supplied by the index, graph, or heading artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Stable graph identity, retained even when display names are ambiguous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_symbol: Option<String>,
    /// Canonical heading identity; titles can repeat across files and sections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
}

impl NavNode {
    fn same_identity(&self, other: &Self) -> bool {
        if self.kind == "fca_concept" && other.kind == "fca_concept" {
            return self.symbol == other.symbol;
        }
        if let (Some(left), Some(right)) = (&self.graph_symbol, &other.graph_symbol) {
            return left == right;
        }
        if let (Some(left), Some(right)) = (&self.section, &other.section) {
            return left == right;
        }
        self.path == other.path && self.symbol == other.symbol && self.kind == other.kind
    }

    fn retain_identity_from(&mut self, other: &Self) {
        if self.line.is_none() {
            self.line = other.line;
        }
        if self.graph_symbol.is_none() {
            self.graph_symbol.clone_from(&other.graph_symbol);
        }
        if self.section.is_none() {
            self.section.clone_from(&other.section);
        }
    }
}

/// On-disk navigation session for one repo.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NavSession {
    pub current: Option<NavNode>,
    #[serde(default)]
    pub history: Vec<NavNode>,
    #[serde(default)]
    pub future: Vec<NavNode>,
    #[serde(default)]
    pub last_results: Vec<NavNode>,
    /// Page metadata belongs to the listing that produced last_results.
    #[serde(default)]
    pub result_page: Option<serde_json::Value>,
    /// Investigation mode survives selection of source nodes from lattice results.
    #[serde(default)]
    pub navigation_mode: Option<String>,
    #[serde(default)]
    pub last_action: String,
    /// Current FCA concept id when the cursor sits on the lattice.
    #[serde(default)]
    pub current_concept: Option<String>,
    /// Current wiki heading object (`section:path#line`) when sitting on outline.
    #[serde(default)]
    pub current_section: Option<String>,
    /// Last grounded ontology IRI from `nav explain`.
    #[serde(default)]
    pub current_iri: Option<String>,
}

/// Absolute path of the persisted session file for this agent.
pub fn session_path(repo_root: &Path) -> PathBuf {
    crate::sidecar::nav_session_path(repo_root)
}

/// Load a session, or an empty one when the file is missing.
pub fn load_session(repo_root: &Path) -> Result<NavSession> {
    let path = session_path(repo_root);
    if !path.is_file() {
        return Ok(NavSession::default());
    }
    let raw =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
}

/// Whether `needle` is an HTTP(S) IRI, not a code symbol.
pub fn looks_like_iri(needle: &str) -> bool {
    let trimmed = needle.trim();
    trimmed.starts_with("http://") || trimmed.starts_with("https://")
}

/// Pin the cursor to an ontology IRI without loading the code index.
///
/// Used for knowledge-graph walks. Symbol/lattice search is skipped so a
/// `nav goto <iri>` stays off the SPARQL hot path.
///
/// # Errors
///
/// Returns an error when the session file cannot be read or written.
pub fn pin_iri(repo_root: &Path, iri: &str) -> Result<QueryEnvelope> {
    let started = Instant::now();
    let iri = iri.trim();
    if !looks_like_iri(iri) {
        bail!("pin_iri requires an http(s) IRI");
    }
    let mut session = load_session(repo_root)?;
    session.current_iri = Some(iri.to_string());
    session.navigation_mode = None;
    clear_results(&mut session);
    session.last_action = "goto".to_string();
    save_session(repo_root, &session)?;
    Ok(session_envelope(&session, Vec::new(), started, repo_root))
}

/// Write the session atomically enough for a single-writer CLI.
pub fn save_session(repo_root: &Path, session: &NavSession) -> Result<()> {
    let path = session_path(repo_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    crate::sidecar::write_atomic_json(&path, session)
        .with_context(|| format!("failed to write {}", path.display()))
}

/// Run one navigation verb against the persisted session (first page).
pub fn run_nav(
    index: &RepoIndex,
    repo_root: &Path,
    action: NavAction,
    needle: Option<&str>,
    select: Option<usize>,
    limit: usize,
) -> Result<QueryEnvelope> {
    run_nav_page(index, repo_root, action, needle, select, limit, 0)
}

/// Continue a bounded listing using its returned next_offset, query and limit.
/// Selection indices are always local to the displayed page.
pub fn run_nav_page(
    index: &RepoIndex,
    repo_root: &Path,
    action: NavAction,
    needle: Option<&str>,
    select: Option<usize>,
    limit: usize,
    offset: usize,
) -> Result<QueryEnvelope> {
    let started = Instant::now();
    let limit = limit.clamp(1, MAX_PAGE_SIZE);
    let scan_limit = offset
        .checked_add(limit)
        .and_then(|end| end.checked_add(1))
        .ok_or_else(|| anyhow::anyhow!("nav offset is too large"))?;
    let mut session = load_session(repo_root)?;
    let needle = needle.map(str::trim);
    validate_continuation(&session, index, repo_root, action, needle, limit, offset)?;
    let original_current = session.current.clone();
    let mut warnings = Vec::new();
    let mut complete_results = true;
    let listing = matches!(
        action,
        NavAction::Goto
            | NavAction::Callers
            | NavAction::Callees
            | NavAction::Neighbors
            | NavAction::Related
            | NavAction::Parent
            | NavAction::Child
            | NavAction::Peer
            | NavAction::Align
    );

    match action {
        NavAction::Here => {}
        NavAction::Reset => session = NavSession::default(),
        NavAction::Explain => {
            let hint = needle
                .map(str::to_string)
                .or_else(|| session.current.as_ref().map(|node| node.symbol.clone()))
                .or_else(|| session.current_section.clone());
            let Some(hint) = hint else {
                bail!("nav explain needs a current heading or a needle");
            };
            let mut envelope = crate::knowledge_explain::explain_knowledge_scoped(
                repo_root,
                &hint,
                limit,
                session.current_section.as_deref(),
                session.current_iri.as_deref(),
            )?;
            if envelope
                .meta
                .as_ref()
                .and_then(|meta| meta.get("grounded"))
                .and_then(serde_json::Value::as_bool)
                == Some(true)
            {
                if let Some(section) = envelope
                    .entities
                    .iter()
                    .find_map(|row| row.get("section").and_then(serde_json::Value::as_str))
                    && let Ok(Some(lattice)) = crate::lattice::try_load_lattice(repo_root)
                    && let Some(heading) = crate::lattice::heading_for(&lattice, section)
                {
                    sit_on_heading(&mut session, &lattice, heading);
                }
                if let Some(iri) = envelope
                    .entities
                    .first()
                    .and_then(|row| row.get("iri").and_then(serde_json::Value::as_str))
                {
                    session.current_iri = Some(iri.to_string());
                }
            }
            clear_results(&mut session);
            session.last_action = "explain".to_string();
            save_session(repo_root, &session)?;
            attach_readiness(&mut envelope, index, repo_root);
            return Ok(envelope);
        }
        NavAction::Goto => {
            let needle = require_needle(action, needle)?;
            if looks_like_iri(needle) {
                session.current_iri = Some(needle.to_string());
                clear_results(&mut session);
            } else if let Some(file) = exact_indexed_file(index, repo_root, needle)? {
                session.last_results = vec![file.clone()];
                if offset == 0 {
                    attach_concept(&mut session, repo_root, &file);
                    push_current(&mut session, file);
                }
            } else if needle.starts_with("urn:") {
                let envelope =
                    query_call_graph(index, repo_root, GraphDirection::CalleesOf, needle)?;
                let chosen = envelope.entities.iter()
                    .find_map(|entity| entity.get("resolved_symbol").and_then(node_from_graph_entity))
                    .ok_or_else(|| anyhow::anyhow!("graph symbol `{needle}` not found; run `leio-code graph symbols-in <path>` for current symbol URNs"))?;
                let chosen = hydrate_graph_source_line(index, repo_root, chosen)?;
                session.last_results = vec![chosen.clone()];
                if offset == 0 {
                    attach_concept(&mut session, repo_root, &chosen);
                    push_current(&mut session, chosen);
                }
            } else if let Ok(Some(lattice)) = crate::lattice::try_load_lattice(repo_root)
                && let Some(concept) = crate::lattice::concept_for(&lattice, needle)
                && (concept.id == needle || concept.label.eq_ignore_ascii_case(needle))
            {
                let node = node_from_concept(concept.clone());
                session.last_results = vec![node.clone()];
                if offset == 0 {
                    attach_concept(&mut session, repo_root, &node);
                    push_current(&mut session, node);
                }
            } else if let Ok(Some(lattice)) = crate::lattice::try_load_lattice(repo_root)
                && looks_like_heading(needle)
                && let Some(heading) = crate::lattice::heading_for(&lattice, needle)
            {
                sit_on_heading(&mut session, &lattice, heading);
            } else {
                let index_hits = nodes_from_find(index, needle, usize::MAX);
                // Index hits precede Arrow. Fetch enough Arrow rows to survive
                // deduplication against every index hit while keeping a stable prefix.
                let arrow_limit = scan_limit.saturating_add(index_hits.len());
                let arrow_hits =
                    local_nodes::search_navigation_hits(repo_root, needle, arrow_limit)?;
                complete_results = arrow_hits.len() < arrow_limit;
                session.last_results = merge_goto_hits(
                    needle,
                    index_hits,
                    arrow_hits.iter().map(node_from_hit).collect(),
                    usize::MAX,
                );
                if session.last_results.is_empty() {
                    if let Ok(Some(lattice)) = crate::lattice::try_load_lattice(repo_root)
                        && let Some(heading) = crate::lattice::heading_for(&lattice, needle)
                    {
                        sit_on_heading(&mut session, &lattice, heading);
                    } else {
                        warnings.push(format!(
                            "no index, Arrow, lattice, or heading matches for `{needle}`"
                        ));
                    }
                } else if offset == 0
                    && let Some(first) = session.last_results.first().cloned()
                {
                    attach_concept(&mut session, repo_root, &first);
                    push_current(&mut session, first);
                }
            }
        }
        NavAction::Select => {
            let slot = select.ok_or_else(|| anyhow::anyhow!("nav select requires --index"))?;
            let Some(chosen) = session.last_results.get(slot).cloned() else {
                bail!(
                    "nav select {slot} out of range ({} results)",
                    session.last_results.len()
                );
            };
            push_current(&mut session, chosen);
            if let Some(current) = session.current.clone() {
                attach_concept(&mut session, repo_root, &current);
            }
            clear_results(&mut session);
        }
        NavAction::Callers | NavAction::Callees | NavAction::Neighbors => {
            let current = require_current(&session)?;
            let needle = graph_needle(index, repo_root, &current)?;
            let directions: &[GraphDirection] = match action {
                NavAction::Callers => &[GraphDirection::CallersOf],
                NavAction::Callees => &[GraphDirection::CalleesOf],
                _ => &[GraphDirection::CallersOf, GraphDirection::CalleesOf],
            };
            let mut results = Vec::new();
            for direction in directions {
                let envelope = query_call_graph(index, repo_root, *direction, &needle)?;
                require_graph_resolution(&envelope)?;
                warnings.extend(envelope.warnings.iter().cloned());
                results.extend(nodes_from_graph(&envelope));
            }
            session.last_results = dedupe_nodes(results.into_iter());
        }
        NavAction::Related => {
            let current = require_current(&session)?;
            let mut related = Vec::new();
            if let Ok(Some(lattice)) = crate::lattice::try_load_lattice(repo_root) {
                // Related merges overlapping categories. Collect their finite
                // stored sets before paging so adding a later page cannot reorder
                // an earlier group or hide rows removed by deduplication.
                if let Some(section_id) = resolve_section(&session, &lattice, &current) {
                    session.current_section = Some(section_id.clone());
                    for direction in [
                        crate::lattice::LatticeWalk::Parent,
                        crate::lattice::LatticeWalk::Child,
                        crate::lattice::LatticeWalk::Peer,
                    ] {
                        related.extend(
                            crate::lattice::heading_walk(
                                &lattice,
                                &section_id,
                                direction,
                                lattice.heading_objects.len(),
                            )
                            .into_iter()
                            .map(node_from_heading),
                        );
                    }
                    related.extend(
                        lattice
                            .heading_alignment
                            .iter()
                            .filter(|row| row.source_uri == section_id)
                            .filter_map(|row| {
                                lattice
                                    .concepts
                                    .iter()
                                    .find(|concept| concept.id == row.target_uri)
                            })
                            .cloned()
                            .map(node_from_concept),
                    );
                }
                if let Some(concept_id) = session.current_concept.clone() {
                    related.extend(
                        crate::lattice::family_peers(&lattice, &concept_id, lattice.concepts.len())
                            .into_iter()
                            .map(node_from_concept),
                    );
                    related.extend(
                        lattice
                            .heading_alignment
                            .iter()
                            .filter(|row| {
                                row.target_uri == concept_id
                                    && session.current_section.as_deref()
                                        != Some(row.source_uri.as_str())
                            })
                            .filter_map(|row| {
                                lattice
                                    .heading_objects
                                    .iter()
                                    .find(|object| object.id == row.source_uri)
                            })
                            .cloned()
                            .map(node_from_heading),
                    );
                }
            }
            related.retain(|node| !node.same_identity(&current));
            if related.is_empty() {
                let hits = local_nodes::search_navigation_hits(
                    repo_root,
                    &current.symbol,
                    scan_limit.saturating_add(1),
                )?;
                complete_results = hits.len() < scan_limit.saturating_add(1);
                session.last_results = dedupe_nodes(
                    hits.iter()
                        .map(node_from_hit)
                        .filter(|node| !node.same_identity(&current)),
                );
            } else {
                session.last_results = dedupe_nodes(related.into_iter());
            }
        }
        NavAction::Parent | NavAction::Child | NavAction::Peer => {
            let lattice = crate::lattice::ensure_lattice(index, repo_root)?;
            let concept_id = resolve_concept(&session, &lattice)?;
            session.current_concept = Some(concept_id.clone());
            let direction = match action {
                NavAction::Parent => crate::lattice::LatticeWalk::Parent,
                NavAction::Child => crate::lattice::LatticeWalk::Child,
                _ => crate::lattice::LatticeWalk::Peer,
            };
            let results = crate::lattice::walk(&lattice, &concept_id, direction, scan_limit);
            complete_results = results.len() < scan_limit;
            session.last_results = results.into_iter().map(node_from_concept).collect();
            if session.last_results.is_empty() {
                warnings.push(format!(
                    "no {} morphisms from `{concept_id}`",
                    action.as_str()
                ));
            }
        }
        NavAction::Align => {
            let lattice = crate::lattice::ensure_lattice(index, repo_root)?;
            for functor in [&lattice.functor, &lattice.heading_functor] {
                warnings.push(format!(
                    "functor {} coherence={:.3} ({}/{})",
                    functor.name, functor.coherence, functor.preserved, functor.total
                ));
            }
            if let Ok(concept_id) = resolve_concept(&session, &lattice) {
                session.current_concept = Some(concept_id.clone());
                let results = crate::lattice::family_peers(&lattice, &concept_id, scan_limit);
                complete_results = results.len() < scan_limit;
                session.last_results = results.into_iter().map(node_from_concept).collect();
            } else {
                session.last_results = lattice
                    .concepts
                    .into_iter()
                    .map(node_from_concept)
                    .collect();
            }
        }
        NavAction::Back | NavAction::Forward => {
            let next = if action == NavAction::Back {
                let previous = session
                    .history
                    .pop()
                    .ok_or_else(|| anyhow::anyhow!("nav history is empty"))?;
                if let Some(current) = session.current.take() {
                    session.future.push(current);
                    trim_stack(&mut session.future);
                }
                previous
            } else {
                let next = session
                    .future
                    .pop()
                    .ok_or_else(|| anyhow::anyhow!("nav forward stack is empty"))?;
                if let Some(current) = session.current.take() {
                    session.history.push(current);
                    trim_stack(&mut session.history);
                }
                next
            };
            attach_concept(&mut session, repo_root, &next);
            session.current = Some(next);
            clear_results(&mut session);
        }
    }

    if listing && !needle.is_some_and(looks_like_iri) {
        if offset > 0 && action == NavAction::Goto {
            session.current = original_current;
        }
        let available = session.last_results.len();
        let total = complete_results.then_some(available);
        let has_more = available > offset + limit;
        session.last_results = session
            .last_results
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect();
        session.result_page = Some(json!({
            "offset": offset, "limit": limit, "returned": session.last_results.len(), "total": total,
            "has_more": has_more, "next_offset": has_more.then_some(offset + limit),
            "query": {"kind": action.as_str(), "needle": if action == NavAction::Goto { needle } else { None },
                "source": session.current, "indexed_at": index.indexed_at, "artifacts": navigation_artifacts(repo_root)},
        }));
    }
    update_navigation_mode(&mut session, action);
    session.last_action = action.as_str().to_string();
    save_session(repo_root, &session)?;
    let mut envelope = session_envelope(&session, warnings, started, repo_root);
    attach_readiness(&mut envelope, index, repo_root);
    Ok(envelope)
}

fn update_navigation_mode(session: &mut NavSession, action: NavAction) {
    let mode = match action {
        NavAction::Callers | NavAction::Callees | NavAction::Neighbors => Some("graph"),
        NavAction::Parent | NavAction::Child | NavAction::Peer | NavAction::Align => {
            Some("lattice")
        }
        NavAction::Goto | NavAction::Select | NavAction::Back | NavAction::Forward => {
            match session.current.as_ref().map(|node| node.kind.as_str()) {
                Some("fca_concept") => Some("lattice"),
                Some("wiki_section") => Some("heading"),
                Some(_) if action == NavAction::Goto || session.navigation_mode.is_none() => {
                    Some("graph")
                }
                _ => None,
            }
        }
        _ => None,
    };
    if let Some(mode) = mode {
        session.navigation_mode = Some(mode.into());
    }
}

fn clear_results(session: &mut NavSession) {
    session.last_results.clear();
    session.result_page = None;
}

fn validate_continuation(
    session: &NavSession,
    index: &RepoIndex,
    repo_root: &Path,
    action: NavAction,
    needle: Option<&str>,
    limit: usize,
    offset: usize,
) -> Result<()> {
    if offset == 0 {
        return Ok(());
    }
    let Some(page) = &session.result_page else {
        bail!("nav --offset requires an active listing; start at offset 0");
    };
    if page["next_offset"].as_u64() != u64::try_from(offset).ok()
        || page["limit"].as_u64() != Some(limit as u64)
        || page["query"]["kind"].as_str() != Some(action.as_str())
        || page["query"]["needle"].as_str()
            != if action == NavAction::Goto {
                needle
            } else {
                None
            }
        || page["query"]["indexed_at"].as_str() != Some(index.indexed_at.as_str())
        || page["query"]["source"] != serde_json::to_value(&session.current)?
        || page["query"]["artifacts"] != navigation_artifacts(repo_root)
    {
        bail!(
            "nav continuation changed: use the returned next_offset with the same query, limit, cursor and index, or restart at offset 0"
        );
    }
    Ok(())
}

fn navigation_artifacts(repo_root: &Path) -> serde_json::Value {
    fn stamp(path: &Path) -> serde_json::Value {
        let Ok(meta) = fs::metadata(path) else {
            return serde_json::Value::Null;
        };
        json!({"bytes": meta.len(), "modified_ns": meta.modified().ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok()).map(|time| time.as_nanos().to_string())})
    }
    json!({"lattice": stamp(&crate::lattice::lattice_path(repo_root)),
        "arrow": stamp(&crate::export::default_arrow_nodes_rows_path(&crate::export::default_arrow_nodes_output_dir(repo_root))),
        "search_sidecar": stamp(&crate::export::default_search_sidecar_path(&crate::export::default_arrow_nodes_output_dir(repo_root)))})
}

fn exact_indexed_file(
    index: &RepoIndex,
    repo_root: &Path,
    needle: &str,
) -> Result<Option<NavNode>> {
    if looks_like_iri(needle) || needle.starts_with("urn:") || looks_like_heading(needle) {
        return Ok(None);
    }
    let candidate = needle.strip_prefix("file:").unwrap_or(needle);
    let path = Path::new(candidate);
    let explicit = needle.starts_with("file:")
        || candidate.starts_with("./")
        || path.is_absolute()
        || candidate.contains('/')
        || candidate.contains('\\')
        || path.extension().is_some();
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir | Component::Prefix(_)))
    {
        bail!("nav goto file paths must stay inside the repository without parent traversal");
    }
    let root = fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    let relative = if path.is_absolute() {
        path.strip_prefix(repo_root)
            .or_else(|_| path.strip_prefix(&root))
            .map_err(|_| anyhow::anyhow!("nav goto file path is outside the repository"))?
            .to_path_buf()
    } else {
        path.components()
            .filter(|part| !matches!(part, Component::CurDir))
            .collect::<PathBuf>()
    };
    if let Ok(canonical) = fs::canonicalize(root.join(&relative))
        && !canonical.starts_with(&root)
    {
        bail!("nav goto file path resolves outside the repository");
    }
    let relative = relative.to_string_lossy().replace('\\', "/");
    if let Some(file) = index.files.iter().find(|file| file.path == relative) {
        return Ok(Some(NavNode {
            path: file.path.clone(),
            symbol: file.path.clone(),
            kind: "file".into(),
            line: None,
            graph_symbol: None,
            section: None,
        }));
    }
    let exact_symbol = index
        .files
        .iter()
        .flat_map(|file| &file.symbols)
        .any(|symbol| symbol.name == needle || symbol.qual_name.as_deref() == Some(needle));
    if explicit && !exact_symbol {
        bail!(
            "file `{relative}` is not indexed; refresh the repository index or choose an indexed path"
        );
    }
    Ok(None)
}

fn attach_readiness(envelope: &mut QueryEnvelope, index: &RepoIndex, repo_root: &Path) {
    let readiness = crate::lattice::lattice_readiness(index, repo_root);
    if let Some(state @ ("stale" | "unverified" | "invalid")) = readiness["state"].as_str() {
        let reason = readiness["reason"]
            .as_str()
            .unwrap_or("artifact freshness could not be verified");
        let rebuild = if readiness["rebuild_required"].as_bool() == Some(true) {
            "; rebuild with `leio-code export formal-context`"
        } else {
            ""
        };
        envelope
            .warnings
            .push(format!("lattice {state}: {reason}{rebuild}"));
    }
    let meta = envelope.meta.get_or_insert_with(|| json!({}));
    meta["lattice"] = readiness;
}

fn require_needle(action: NavAction, needle: Option<&str>) -> Result<&str> {
    needle
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("nav {} requires a needle", action.as_str()))
}

fn require_current(session: &NavSession) -> Result<NavNode> {
    session
        .current
        .clone()
        .ok_or_else(|| anyhow::anyhow!("no current node; run `leio-code nav goto <symbol>` first"))
}

fn push_current(session: &mut NavSession, next: NavNode) {
    if let Some(current) = session.current.as_mut()
        && current.same_identity(&next)
    {
        // A repeated authoritative lookup may retain its URN while source lines
        // move after reindexing. Keep the old line only for line-less legacy rows.
        if next.line.is_some() {
            current.line = next.line;
        }
        current.retain_identity_from(&next);
        return;
    }
    if let Some(current) = session.current.take() {
        session.history.push(current);
        trim_stack(&mut session.history);
    }
    session.future.clear();
    session.current = Some(next);
}

fn trim_stack(stack: &mut Vec<NavNode>) {
    if stack.len() > MAX_HISTORY {
        let excess = stack.len() - MAX_HISTORY;
        stack.drain(0..excess);
    }
}

fn nodes_from_find(index: &RepoIndex, needle: &str, limit: usize) -> Vec<NavNode> {
    find_symbols(index, needle)
        .entities
        .iter()
        .filter_map(|entity| {
            let symbol = entity
                .get("qual_name")
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .or_else(|| entity.get("name").and_then(|value| value.as_str()))?;
            let path = entity.get("path").and_then(|value| value.as_str())?;
            let kind = entity
                .get("kind")
                .and_then(|value| value.as_str())
                .unwrap_or("symbol");
            Some(NavNode {
                path: path.to_string(),
                symbol: symbol.to_string(),
                kind: kind.to_string(),
                line: entity
                    .get("line")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|line| u32::try_from(line).ok()),
                graph_symbol: None,
                section: None,
            })
        })
        .take(limit)
        .collect()
}

/// Exact index hits first, then other index hits, then Arrow cosine/lexical.
///
/// Arrow can outrank an exact symbol when the export is stale or cosine
/// prefers a similarly named type (`embed_query` → `EmbedRequest`).
fn merge_goto_hits(
    needle: &str,
    index_hits: Vec<NavNode>,
    arrow_hits: Vec<NavNode>,
    limit: usize,
) -> Vec<NavNode> {
    let needle_lc = needle.to_ascii_lowercase();
    let (exact, rest): (Vec<NavNode>, Vec<NavNode>) = index_hits
        .into_iter()
        .partition(|node| node.symbol.eq_ignore_ascii_case(&needle_lc));
    dedupe_nodes(exact.into_iter().chain(rest).chain(arrow_hits))
        .into_iter()
        .take(limit)
        .collect()
}

fn node_from_hit(hit: &NodeHit) -> NavNode {
    NavNode {
        path: hit.path.clone(),
        symbol: hit.symbol.clone(),
        kind: hit.kind.clone(),
        line: None,
        graph_symbol: None,
        section: None,
    }
}

fn node_from_concept(concept: crate::lattice::LatticeConcept) -> NavNode {
    NavNode {
        path: String::new(),
        symbol: concept.id,
        kind: "fca_concept".to_string(),
        line: None,
        graph_symbol: None,
        section: None,
    }
}

fn node_from_heading(heading: crate::lattice::HeadingObject) -> NavNode {
    NavNode {
        path: heading.path,
        symbol: heading.heading_path,
        kind: "wiki_section".to_string(),
        line: Some(heading.line),
        graph_symbol: None,
        section: Some(heading.id),
    }
}

fn looks_like_heading(needle: &str) -> bool {
    needle.starts_with("section:") || needle.contains(" > ") || section_needle_has_line(needle)
}

fn section_needle_has_line(needle: &str) -> bool {
    needle.rsplit_once('#').is_some_and(|(path, line)| {
        !path.is_empty() && !line.is_empty() && (path.contains('/') || path.contains('.'))
    })
}

fn sit_on_heading(
    session: &mut NavSession,
    lattice: &crate::lattice::LatticeArtifact,
    heading: &crate::lattice::HeadingObject,
) {
    let node = node_from_heading(heading.clone());
    session.current_iri = None;
    session.current_section = Some(heading.id.clone());
    session.current_concept = lattice
        .memberships
        .get(&heading.id)
        .map(|membership| membership.concept_id.clone());
    session.last_results = vec![node.clone()];
    if let Some(concept_id) = session.current_concept.as_deref()
        && let Some(concept) = lattice.concepts.iter().find(|row| row.id == concept_id)
    {
        session
            .last_results
            .push(node_from_concept(concept.clone()));
    }
    push_current(session, node);
}

fn resolve_section(
    session: &NavSession,
    lattice: &crate::lattice::LatticeArtifact,
    current: &NavNode,
) -> Option<String> {
    if let Some(id) = session.current_section.as_deref()
        && lattice.heading_objects.iter().any(|row| row.id == id)
    {
        return Some(id.to_string());
    }
    if current.kind == "wiki_section" {
        return crate::lattice::heading_for(lattice, &current.symbol).map(|row| row.id.clone());
    }
    crate::lattice::heading_for(lattice, &current.path).map(|row| row.id.clone())
}

fn resolve_concept(
    session: &NavSession,
    lattice: &crate::lattice::LatticeArtifact,
) -> Result<String> {
    if let Some(id) = session.current_concept.as_deref()
        && lattice.concepts.iter().any(|row| row.id == id)
    {
        return Ok(id.to_string());
    }
    if let Some(current) = &session.current {
        if current.kind == "fca_concept" {
            return Ok(current.symbol.clone());
        }
        if let Some(concept) = crate::lattice::concept_for(lattice, &current.path) {
            return Ok(concept.id.clone());
        }
    }
    anyhow::bail!("nav lattice walk needs a current concept (goto a file or concept first)")
}

fn attach_concept(session: &mut NavSession, repo_root: &Path, node: &NavNode) {
    session.current_concept = None;
    session.current_section = None;
    session.current_iri = None;
    if node.kind == "fca_concept" {
        session.current_concept = Some(node.symbol.clone());
        return;
    }
    if let Ok(Some(lattice)) = crate::lattice::try_load_lattice(repo_root) {
        if node.kind == "wiki_section"
            && let Some(heading) = node
                .section
                .as_deref()
                .and_then(|id| crate::lattice::heading_for(&lattice, id))
                .or_else(|| {
                    lattice.heading_objects.iter().find(|heading| {
                        heading.path == node.path && heading.heading_path == node.symbol
                    })
                })
        {
            session.current_section = Some(heading.id.clone());
            if let Some(membership) = lattice.memberships.get(&heading.id) {
                session.current_concept = Some(membership.concept_id.clone());
            }
            return;
        }
        if let Some(concept) = crate::lattice::concept_for(&lattice, &node.path) {
            session.current_concept = Some(concept.id.clone());
        }
    }
}

fn nodes_from_graph(envelope: &QueryEnvelope) -> Vec<NavNode> {
    envelope
        .entities
        .iter()
        .filter(|entity| entity.get("query").is_none())
        .filter_map(node_from_graph_entity)
        .collect()
}

fn require_graph_resolution(envelope: &QueryEnvelope) -> Result<()> {
    if !envelope
        .entities
        .iter()
        .any(|row| row.get("resolved_symbol").is_some())
    {
        anyhow::bail!(
            "{}; run `leio-code graph symbols-in <path>` then `leio-code nav goto <symbol URN>`",
            envelope.summary
        );
    }
    Ok(())
}

fn node_from_graph_entity(entity: &serde_json::Value) -> Option<NavNode> {
    let symbol = entity
        .get("qual_name")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .or_else(|| entity.get("name").and_then(serde_json::Value::as_str))?;
    Some(NavNode {
        path: entity.get("path")?.as_str()?.to_string(),
        symbol: symbol.to_string(),
        kind: entity
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("symbol")
            .to_string(),
        line: entity
            .get("line")
            .and_then(serde_json::Value::as_u64)
            .and_then(|line| u32::try_from(line).ok()),
        graph_symbol: entity
            .get("symbol")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        section: None,
    })
}

/// Call-graph resolved_symbol packets omit source lines. Resolve that field
/// from the file's symbol inventory using the canonical URN, never a display name.
fn hydrate_graph_source_line(
    index: &RepoIndex,
    repo_root: &Path,
    mut node: NavNode,
) -> Result<NavNode> {
    if node.line.is_none()
        && let Some(iri) = node.graph_symbol.as_deref()
    {
        let inventory = query_symbols_in(index, repo_root, &node.path)?;
        node.line = inventory
            .entities
            .iter()
            .find(|entity| entity.get("symbol").and_then(serde_json::Value::as_str) == Some(iri))
            .and_then(node_from_graph_entity)
            .and_then(|resolved| resolved.line);
    }
    Ok(node)
}

/// Resolve legacy/index nodes within their selected file before walking.
/// Graph results already carry their identity, so subsequent hops avoid lookup.
fn graph_needle(index: &RepoIndex, repo_root: &Path, node: &NavNode) -> Result<String> {
    if let Some(iri) = &node.graph_symbol {
        return Ok(iri.clone());
    }
    let envelope = query_symbols_in(index, repo_root, &node.path)?;
    let matches: Vec<&serde_json::Value> = envelope
        .entities
        .iter()
        .filter(|entity| {
            entity.get("path").and_then(serde_json::Value::as_str) == Some(node.path.as_str())
                && ["name", "qual_name"].iter().any(|key| {
                    entity.get(key).and_then(serde_json::Value::as_str)
                        == Some(node.symbol.as_str())
                })
        })
        .collect();
    if matches.len() == 1
        && let Some(iri) = matches[0].get("symbol").and_then(serde_json::Value::as_str)
    {
        return Ok(iri.to_string());
    }
    anyhow::bail!(
        "cannot uniquely resolve `{}` in `{}`; run `leio-code graph symbols-in {}` then `leio-code nav goto <symbol URN>`",
        node.symbol,
        node.path,
        node.path
    )
}

fn dedupe_nodes(nodes: impl Iterator<Item = NavNode>) -> Vec<NavNode> {
    let mut out: Vec<NavNode> = Vec::new();
    for node in nodes {
        if let Some(seen) = out.iter_mut().find(|seen| seen.same_identity(&node)) {
            seen.retain_identity_from(&node);
        } else {
            out.push(node);
        }
    }
    out
}

fn concept_details(
    lattice: Option<&crate::lattice::LatticeArtifact>,
    node: &NavNode,
    current_concept: Option<&str>,
) -> Option<serde_json::Value> {
    let lattice = lattice?;
    let concept = if node.kind == "fca_concept" {
        lattice.concepts.iter().find(|row| row.id == node.symbol)
    } else if let Some(id) = current_concept {
        lattice.concepts.iter().find(|row| row.id == id)
    } else {
        node.section
            .as_deref()
            .and_then(|id| crate::lattice::concept_for(lattice, id))
            .or_else(|| crate::lattice::concept_for(lattice, &node.path))
    }?;
    let primary: Vec<_> = lattice
        .memberships
        .iter()
        .filter(|(_, membership)| membership.concept_id == concept.id)
        .collect();
    let mut candidates = primary.clone();
    if candidates.len() < MEMBER_SAMPLE_LIMIT {
        let containing: std::collections::BTreeSet<_> = lattice
            .concepts
            .iter()
            .filter(|other| {
                other.id != concept.id
                    && concept
                        .intent
                        .iter()
                        .all(|attribute| other.intent.contains(attribute))
            })
            .map(|other| other.id.as_str())
            .collect();
        candidates.extend(
            lattice
                .memberships
                .iter()
                .filter(|(_, membership)| containing.contains(membership.concept_id.as_str())),
        );
    }
    candidates.sort_by(|(left_id, left), (right_id, right)| {
        (
            left.concept_id != concept.id,
            !(left_id.starts_with("file:") || left_id.starts_with("section:")),
            left_id,
        )
            .cmp(&(
                right.concept_id != concept.id,
                !(right_id.starts_with("file:") || right_id.starts_with("section:")),
                right_id,
            ))
    });
    let inherited = candidates
        .iter()
        .take(MEMBER_SAMPLE_LIMIT)
        .any(|(_, membership)| membership.concept_id != concept.id);
    let members: Vec<_> = candidates
        .iter()
        .take(MEMBER_SAMPLE_LIMIT)
        .map(|(id, membership)| {
            let mut member = json!({"object_id": id, "primary_concept_id": membership.concept_id,
            "inherited": membership.concept_id != concept.id});
            if let Some(path) = id.strip_prefix("file:") {
                member["path"] = json!(path);
            } else if let Some(heading) = lattice
                .heading_objects
                .iter()
                .find(|heading| heading.id == **id)
            {
                member["path"] = json!(heading.path);
                member["line"] = json!(heading.line);
            }
            member
        })
        .collect();
    Some(json!({
        "id": concept.id, "label": concept.label, "family": concept.family,
        "intent": concept.intent.iter().take(32).collect::<Vec<_>>(),
        "intent_count": concept.intent.len(), "intent_truncated": concept.intent.len() > 32,
        "extent_size": concept.extent_size, "member_sample_count": members.len(),
        "member_sample_limit": MEMBER_SAMPLE_LIMIT, "members": members,
        "primary_membership_count": primary.len(), "membership_basis": if inherited { "stored_primary_memberships_with_containing_intent" } else { "stored_primary_memberships" },
        "members_are_complete_extent": false,
    }))
}

fn session_envelope(
    session: &NavSession,
    warnings: Vec<String>,
    started: Instant,
    repo_root: &Path,
) -> QueryEnvelope {
    let current = session.current.clone();
    let lattice = crate::lattice::try_load_lattice(repo_root).ok().flatten();
    let pin = session
        .current_iri
        .as_deref()
        .map(|iri| format!(" · iri {iri}"))
        .unwrap_or_default();
    let summary = match &current {
        Some(node) => format!(
            "nav {} at `{}` ({}){pin} · {} results · history {}",
            session.last_action,
            node.symbol,
            if node.kind == "fca_concept" {
                "concept"
            } else {
                &node.path
            },
            session.last_results.len(),
            session.history.len()
        ),
        None => format!(
            "nav {}{pin} · no current node · {} results",
            session.last_action,
            session.last_results.len()
        ),
    };
    let mut entities = Vec::new();
    if let Some(node) = &current {
        entities.push(json!({
            "role": "current",
            "path": if node.kind == "fca_concept" { None } else { Some(&node.path) },
            "line": node.line,
            "concept_details": concept_details(lattice.as_ref(), node, session.current_concept.as_deref()),
            "symbol": node.symbol,
            "kind": node.kind,
            "graph_symbol": node.graph_symbol,
            "iri": session.current_iri,
            "section": session.current_section,
            "concept": session.current_concept,
        }));
    } else if let Some(iri) = &session.current_iri {
        entities.push(json!({
            "role": "current",
            "kind": "iri",
            "iri": iri,
        }));
    }
    entities.extend(session.last_results.iter().enumerate().map(|(rank, node)| {
        json!({
            "role": "result",
            "index": rank,
            "path": if node.kind == "fca_concept" { None } else { Some(&node.path) },
            "line": node.line,
            "concept_details": concept_details(lattice.as_ref(), node, None),
            "symbol": node.symbol,
            "kind": node.kind,
            "graph_symbol": node.graph_symbol,
            "section": node.section,
        })
    }));
    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: format!(
            "nav-{}",
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ),
        kind: "nav".to_string(),
        summary,
        confidence: if current.is_some() || session.current_iri.is_some() {
            0.9
        } else {
            0.4
        },
        evidence: dedupe_nodes(
            current
                .iter()
                .cloned()
                .chain(session.last_results.iter().cloned()),
        )
        .iter()
        .filter(|node| node.kind != "fca_concept" && !node.path.is_empty())
        .map(|node| EvidenceItem {
            kind: "nav_node".to_string(),
            path: node.path.clone(),
            line: node.line.map(|line| line as usize),
            detail: format!("{} {}", node.kind, node.symbol),
        })
        .collect(),
        entities,
        warnings,
        meta: Some(json!({
            "action": session.last_action,
            "history_len": session.history.len(),
            "future_len": session.future.len(),
            "current_concept": session.current_concept,
            "current_section": session.current_section,
            "current_iri": session.current_iri,
            "session": crate::sidecar::session_report(repo_root),
            "result_page": session.result_page,
            "navigation_mode": session.navigation_mode,
            "lattice": {"state": "unchecked", "artifact_path": crate::lattice::lattice_path(repo_root), "rebuild_required": false, "reason": "index freshness was not checked on this lightweight path"},
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indexed_fixture(root: &Path, sources: &[(&str, &str)]) -> RepoIndex {
        for (path, source) in sources {
            fs::write(root.join(path), source).expect("write fixture source");
        }
        crate::indexer::load_or_build_index(root, &crate::indexer::default_index_path(root))
            .expect("index fixture")
    }

    #[test]
    fn exact_file_goto_uses_only_index_and_normalizes_paths() {
        let dir = tempfile::tempdir().unwrap();
        let index = indexed_fixture(dir.path(), &[("plain.py", "def plain():\n    return 1\n")]);
        let absolute = dir.path().join("plain.py");
        for needle in [
            "plain.py",
            "./plain.py",
            "file:plain.py",
            absolute.to_str().unwrap(),
        ] {
            let envelope =
                run_nav(&index, dir.path(), NavAction::Goto, Some(needle), None, 10).unwrap();
            assert_eq!(envelope.entities[0]["path"], "plain.py");
            assert_eq!(envelope.entities[0]["kind"], "file");
            assert_eq!(
                envelope.meta.as_ref().unwrap()["lattice"]["state"],
                "missing"
            );
        }
        assert!(!crate::lattice::lattice_path(dir.path()).exists());
        assert!(!local_nodes::available(dir.path()));
        let mut wrong_index = index.clone();
        wrong_index.root = dir
            .path()
            .join("different-root")
            .to_string_lossy()
            .into_owned();
        let mut envelope =
            session_envelope(&NavSession::default(), vec![], Instant::now(), dir.path());
        attach_readiness(&mut envelope, &wrong_index, dir.path());
        assert_eq!(
            envelope.meta.as_ref().unwrap()["lattice"]["state"],
            "invalid"
        );
        assert_eq!(
            envelope.meta.as_ref().unwrap()["lattice"]["rebuild_required"],
            false
        );
        assert!(!envelope.warnings.is_empty());
        assert!(
            envelope
                .warnings
                .iter()
                .all(|warning| !warning.contains("export formal-context"))
        );
        fs::write(dir.path().join("unindexed.py"), "def hidden(): pass\n").unwrap();
        let before = fs::read(session_path(dir.path())).unwrap();
        for needle in [
            "../plain.py",
            "/tmp/outside.py",
            "file:unindexed.py",
            "missing.py",
        ] {
            assert!(
                run_nav(&index, dir.path(), NavAction::Goto, Some(needle), None, 10).is_err(),
                "{needle}"
            );
            assert_eq!(fs::read(session_path(dir.path())).unwrap(), before);
        }
    }

    #[test]
    fn navigation_retains_source_lines_and_dotted_symbol_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let index = indexed_fixture(
            dir.path(),
            &[(
                "source.py",
                "class Example:\n    def method(self):\n        pass\n",
            )],
        );
        let envelope = run_nav(
            &index,
            dir.path(),
            NavAction::Goto,
            Some("Example.method"),
            None,
            10,
        )
        .unwrap();
        assert_eq!(envelope.entities[0]["line"], 2);
        assert!(
            envelope
                .evidence
                .iter()
                .any(|item| item.path == "source.py" && item.line == Some(2))
        );
        let graph = query_symbols_in(&index, dir.path(), "source.py").unwrap();
        let row = graph
            .entities
            .iter()
            .find(|row| row["name"] == "method")
            .unwrap();
        let graph_node = node_from_graph_entity(row).unwrap();
        assert_eq!(graph_node.line, Some(2));
        run_nav(&index, dir.path(), NavAction::Reset, None, None, 10).unwrap();
        let goto = run_nav(
            &index,
            dir.path(),
            NavAction::Goto,
            row["symbol"].as_str(),
            None,
            10,
        )
        .unwrap();
        assert_eq!(goto.entities[0]["graph_symbol"], row["symbol"]);
        assert_eq!(goto.entities[0]["line"], 2);
        assert_eq!(goto.entities[1]["line"], 2);
        assert!(
            goto.evidence
                .iter()
                .any(|item| item.path == "source.py" && item.line == Some(2))
        );
        let here = run_nav(&index, dir.path(), NavAction::Here, None, None, 10).unwrap();
        assert_eq!(here.entities[0]["line"], 2);
        assert_eq!(
            load_session(dir.path()).unwrap().current.unwrap().line,
            Some(2)
        );
        let heading = node_from_heading(crate::lattice::HeadingObject {
            id: "section:docs.md#12".into(),
            path: "docs.md".into(),
            title: "Title".into(),
            heading_path: "Title".into(),
            line: 12,
        });
        assert_eq!(heading.line, Some(12));
    }

    #[test]
    fn repeating_graph_urn_after_reindex_refreshes_line_without_extra_history() {
        let dir = tempfile::tempdir().unwrap();
        let index = indexed_fixture(
            dir.path(),
            &[("source.py", "def target():\n    return 1\n")],
        );
        let inventory = query_symbols_in(&index, dir.path(), "source.py").unwrap();
        let urn = inventory
            .entities
            .iter()
            .find(|row| row["name"] == "target")
            .unwrap()["symbol"]
            .as_str()
            .unwrap();
        let before = run_nav(&index, dir.path(), NavAction::Goto, Some(urn), None, 10).unwrap();
        assert_eq!(before.entities[0]["line"], 1);
        let history = load_session(dir.path()).unwrap().history.len();
        fs::write(
            dir.path().join("source.py"),
            "\n\ndef target():\n    return 1\n",
        )
        .unwrap();
        let refreshed = crate::indexer::build_or_update_index(
            dir.path(),
            &crate::indexer::default_index_path(dir.path()),
            true,
        )
        .unwrap();
        let after = run_nav(&refreshed, dir.path(), NavAction::Goto, Some(urn), None, 10).unwrap();
        assert_eq!(after.entities[0]["graph_symbol"], urn);
        assert_eq!(after.entities[0]["line"], 3);
        assert_eq!(after.entities[1]["line"], 3);
        assert!(
            after
                .evidence
                .iter()
                .any(|item| item.path == "source.py" && item.line == Some(3))
        );
        assert!(
            !after
                .evidence
                .iter()
                .any(|item| item.path == "source.py" && item.line == Some(1))
        );
        let mut session = load_session(dir.path()).unwrap();
        assert_eq!(session.history.len(), history);
        // A legacy packet without a source line must not erase the refreshed line.
        let mut legacy = session.current.clone().unwrap();
        legacy.line = None;
        push_current(&mut session, legacy);
        assert_eq!(session.current.unwrap().line, Some(3));
        assert_eq!(session.history.len(), history);
    }

    #[test]
    fn concept_packets_describe_bounded_primary_samples_without_source_paths() {
        use crate::lattice::{LatticeArtifact, LatticeConcept, StoredMembership};
        let dir = tempfile::tempdir().unwrap();
        let mut lattice = LatticeArtifact::default();
        lattice.concepts.push(LatticeConcept {
            id: "concept-a".into(),
            label: "Rust source".into(),
            family: "language".into(),
            intent: (0..40).map(|n| format!("attribute:{n}")).collect(),
            extent_size: 25,
            parents: vec![],
            children: vec![],
        });
        for n in 0..8 {
            lattice.memberships.insert(
                format!("file:src/file{n}.rs"),
                StoredMembership {
                    concept_id: "concept-a".into(),
                    label: "Rust source".into(),
                    family: "language".into(),
                },
            );
        }
        crate::lattice::write_artifacts(
            crate::lattice::lattice_path(dir.path()).parent().unwrap(),
            &lattice,
        )
        .unwrap();
        let node = node_from_concept(lattice.concepts[0].clone());
        let session = NavSession {
            current: Some(node.clone()),
            current_concept: Some(node.symbol.clone()),
            last_results: vec![node],
            ..NavSession::default()
        };
        let envelope = session_envelope(&session, vec![], Instant::now(), dir.path());
        for entity in &envelope.entities {
            assert!(entity["path"].is_null());
            let details = &entity["concept_details"];
            assert_eq!(details["label"], "Rust source");
            assert_eq!(details["extent_size"], 25);
            assert_eq!(details["members"].as_array().unwrap().len(), 5);
            assert_eq!(details["member_sample_count"], 5);
            assert_eq!(details["primary_membership_count"], 8);
            assert_eq!(details["membership_basis"], "stored_primary_memberships");
            assert_eq!(details["members_are_complete_extent"], false);
            assert_eq!(details["intent"].as_array().unwrap().len(), 32);
            assert_eq!(details["intent_count"], 40);
            assert_eq!(details["intent_truncated"], true);
        }
        assert!(envelope.evidence.is_empty());
        lattice.concepts.push(LatticeConcept {
            id: "broad".into(),
            label: "Sources".into(),
            family: "language".into(),
            intent: vec!["attribute:0".into()],
            extent_size: 30,
            parents: vec![],
            children: vec!["concept-a".into()],
        });
        let broad = node_from_concept(lattice.concepts[1].clone());
        let details = concept_details(Some(&lattice), &broad, None).unwrap();
        assert_eq!(details["primary_membership_count"], 0);
        assert_eq!(details["member_sample_count"], 5);
        assert_eq!(
            details["membership_basis"],
            "stored_primary_memberships_with_containing_intent"
        );
        assert_eq!(details["members"][0]["inherited"], true);
        assert_eq!(details["members"][0]["primary_concept_id"], "concept-a");
    }

    #[test]
    fn graph_pages_keep_cursor_have_exact_totals_and_select_page_local_indices() {
        let dir = tempfile::tempdir().unwrap();
        let source = "def target():\n    return 1\ndef a():\n    return target()\ndef b():\n    return target()\ndef c():\n    return target()\ndef d():\n    return target()\ndef e():\n    return target()\n";
        let index = indexed_fixture(dir.path(), &[("walk.py", source)]);
        run_nav(&index, dir.path(), NavAction::Goto, Some("target"), None, 2).unwrap();
        let mut seen = Vec::new();
        for offset in [0, 2, 4] {
            let envelope = run_nav_page(
                &index,
                dir.path(),
                NavAction::Callers,
                None,
                None,
                2,
                offset,
            )
            .unwrap();
            assert_eq!(envelope.entities[0]["symbol"], "target");
            let page = &envelope.meta.as_ref().unwrap()["result_page"];
            assert_eq!(page["offset"], offset);
            assert_eq!(page["total"], 5);
            assert_eq!(page["has_more"], offset < 4);
            assert_eq!(envelope.entities[1]["index"], 0);
            seen.extend(
                envelope
                    .entities
                    .iter()
                    .skip(1)
                    .map(|row| row["graph_symbol"].as_str().unwrap().to_string()),
            );
            let readback = run_nav(&index, dir.path(), NavAction::Here, None, None, 20).unwrap();
            assert_eq!(readback.meta.as_ref().unwrap()["result_page"], *page);
            assert!(load_session(dir.path()).unwrap().last_results.len() <= 2);
        }
        let unique = seen.iter().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(seen.len(), 5);
        assert_eq!(unique.len(), 5);
        let last = seen.last().unwrap();
        let selected = run_nav(&index, dir.path(), NavAction::Select, None, Some(0), 20).unwrap();
        assert_eq!(selected.entities[0]["graph_symbol"], *last);
        assert_eq!(selected.entities.len(), 1);
        assert!(selected.meta.as_ref().unwrap()["result_page"].is_null());
        let backed = run_nav(&index, dir.path(), NavAction::Back, None, None, 20).unwrap();
        assert_eq!(backed.entities[0]["symbol"], "target");
        assert!(backed.meta.as_ref().unwrap()["result_page"].is_null());
    }

    #[test]
    fn goto_continuation_rejects_changed_context_without_mutating_session() {
        let dir = tempfile::tempdir().unwrap();
        let index = indexed_fixture(
            dir.path(),
            &[
                ("a.py", "def matching(): pass\n"),
                ("b.py", "def matching(): pass\n"),
                ("c.py", "def matching(): pass\n"),
            ],
        );
        let first = run_nav(
            &index,
            dir.path(),
            NavAction::Goto,
            Some("matching"),
            None,
            1,
        )
        .unwrap();
        let original = first.entities[0].clone();
        assert_eq!(
            first.meta.as_ref().unwrap()["result_page"]["next_offset"],
            1
        );
        let before = fs::read(session_path(dir.path())).unwrap();
        for (action, needle, limit, offset) in [
            (NavAction::Goto, Some("matching"), 1, usize::MAX),
            (NavAction::Goto, Some("matching"), 2, 1),
            (NavAction::Goto, Some("other"), 1, 1),
            (NavAction::Callers, None, 1, 1),
            (NavAction::Goto, Some("matching"), 1, 2),
        ] {
            assert!(run_nav_page(&index, dir.path(), action, needle, None, limit, offset).is_err());
            assert_eq!(fs::read(session_path(dir.path())).unwrap(), before);
        }
        let mut changed_index = index.clone();
        changed_index.indexed_at = "changed".into();
        assert!(
            run_nav_page(
                &changed_index,
                dir.path(),
                NavAction::Goto,
                Some("matching"),
                None,
                1,
                1
            )
            .is_err()
        );
        let second = run_nav_page(
            &index,
            dir.path(),
            NavAction::Goto,
            Some("matching"),
            None,
            1,
            1,
        )
        .unwrap();
        assert_eq!(second.entities[0], original);
        assert_ne!(second.entities[1]["path"], first.entities[1]["path"]);
    }

    #[test]
    fn selecting_source_results_retains_lattice_investigation_mode() {
        let mut session = NavSession::default();
        update_navigation_mode(&mut session, NavAction::Parent);
        session.current = Some(NavNode {
            path: "source.py".into(),
            symbol: "source".into(),
            kind: "function".into(),
            line: Some(1),
            graph_symbol: None,
            section: None,
        });
        update_navigation_mode(&mut session, NavAction::Select);
        assert_eq!(session.navigation_mode.as_deref(), Some("lattice"));
        update_navigation_mode(&mut session, NavAction::Callers);
        assert_eq!(session.navigation_mode.as_deref(), Some("graph"));
    }

    #[test]
    fn lattice_pages_report_unknown_total_until_complete() {
        use crate::lattice::{LatticeArtifact, LatticeConcept};
        let dir = tempfile::tempdir().unwrap();
        let index = indexed_fixture(dir.path(), &[("plain.py", "def plain(): pass\n")]);
        let mut lattice = LatticeArtifact::default();
        lattice.concepts.push(LatticeConcept {
            id: "root".into(),
            label: "Root".into(),
            family: "code".into(),
            intent: vec!["code".into()],
            extent_size: 10,
            parents: vec![],
            children: (0..5).map(|n| format!("child-{n}")).collect(),
        });
        for n in 0..5 {
            lattice.concepts.push(LatticeConcept {
                id: format!("child-{n}"),
                label: format!("Child {n}"),
                family: "code".into(),
                intent: vec!["code".into(), format!("child:{n}")],
                extent_size: 1,
                parents: vec!["root".into()],
                children: vec![],
            });
        }
        crate::lattice::write_artifacts(
            crate::lattice::lattice_path(dir.path()).parent().unwrap(),
            &lattice,
        )
        .unwrap();
        run_nav(&index, dir.path(), NavAction::Goto, Some("root"), None, 2).unwrap();
        let mut seen = Vec::new();
        for offset in [0, 2, 4] {
            let envelope =
                run_nav_page(&index, dir.path(), NavAction::Child, None, None, 2, offset).unwrap();
            let page = &envelope.meta.as_ref().unwrap()["result_page"];
            if offset < 4 {
                assert!(page["total"].is_null());
            } else {
                assert_eq!(page["total"], 5);
            }
            seen.extend(
                envelope
                    .entities
                    .iter()
                    .skip(1)
                    .map(|row| row["symbol"].as_str().unwrap().to_string()),
            );
        }
        assert_eq!(seen.len(), 5);
        assert_eq!(
            seen.iter().collect::<std::collections::BTreeSet<_>>().len(),
            5
        );
    }

    #[test]
    fn graph_urn_and_selection_keep_same_named_symbols_in_the_selected_file() {
        let dir = tempfile::tempdir().unwrap();
        let index = indexed_fixture(
            dir.path(),
            &[
                (
                    "a.py",
                    "def alpha():\n    return 1\n\ndef caller():\n    return alpha()\n",
                ),
                (
                    "b.py",
                    "def beta():\n    return 2\n\ndef caller():\n    return beta()\n",
                ),
            ],
        );
        let graph = query_symbols_in(&index, dir.path(), "b.py").unwrap();
        let helper = graph
            .entities
            .iter()
            .find(|row| row["name"] == "beta")
            .unwrap();
        let iri = helper["symbol"].as_str().unwrap();
        let goto = run_nav(&index, dir.path(), NavAction::Goto, Some(iri), None, 10).unwrap();
        assert_eq!(goto.entities[0]["graph_symbol"], iri);
        assert_eq!(goto.entities[0]["path"], "b.py");
        let leaf = run_nav(&index, dir.path(), NavAction::Callees, None, None, 10).unwrap();
        assert_eq!(leaf.entities.len(), 1, "a resolved leaf has no result rows");
        let callers = run_nav(&index, dir.path(), NavAction::Callers, None, None, 1).unwrap();
        assert_eq!(callers.entities.len(), 2);
        assert_eq!(callers.entities[1]["symbol"], "caller");
        assert_eq!(callers.entities[1]["path"], "b.py");
        assert!(
            callers.entities[1]["graph_symbol"]
                .as_str()
                .unwrap()
                .starts_with("urn:")
        );
        run_nav(&index, dir.path(), NavAction::Select, None, Some(0), 10).unwrap();
        let callees = run_nav(&index, dir.path(), NavAction::Callees, None, None, 10).unwrap();
        assert_eq!(callees.entities[1]["graph_symbol"], iri);
        assert_eq!(callees.entities[1]["path"], "b.py");

        // Existing symbol-name entry also resolves within the selected path.
        run_nav(
            &index,
            dir.path(),
            NavAction::Goto,
            Some("caller"),
            None,
            10,
        )
        .unwrap();
        let session = load_session(dir.path()).unwrap();
        let slot = session
            .last_results
            .iter()
            .position(|node| node.path == "b.py")
            .unwrap();
        run_nav(&index, dir.path(), NavAction::Select, None, Some(slot), 10).unwrap();
        let callees = run_nav(&index, dir.path(), NavAction::Callees, None, None, 10).unwrap();
        assert_eq!(callees.entities[1]["graph_symbol"], iri);
        assert_eq!(callees.entities[1]["path"], "b.py");

        let before = fs::read(session_path(dir.path())).unwrap();
        let missing = run_nav(
            &index,
            dir.path(),
            NavAction::Goto,
            Some("urn:missing:symbol"),
            None,
            10,
        );
        assert!(missing.unwrap_err().to_string().contains("symbols-in"));
        assert_eq!(fs::read(session_path(dir.path())).unwrap(), before);

        let mut stale = load_session(dir.path()).unwrap();
        stale.current.as_mut().unwrap().graph_symbol = Some("urn:missing:symbol".into());
        save_session(dir.path(), &stale).unwrap();
        let before = fs::read(session_path(dir.path())).unwrap();
        let missing = run_nav(&index, dir.path(), NavAction::Callers, None, None, 10);
        assert!(missing.unwrap_err().to_string().contains("not found"));
        assert_eq!(fs::read(session_path(dir.path())).unwrap(), before);
    }

    #[test]
    fn history_and_selection_rederive_fca_metadata_and_clear_stale_pins() {
        use crate::lattice::{HeadingObject, LatticeArtifact, LatticeConcept, StoredMembership};
        let dir = tempfile::tempdir().unwrap();
        let index = indexed_fixture(dir.path(), &[("plain.py", "def plain():\n    return 1\n")]);
        let mut lattice = LatticeArtifact::default();
        for (id, parent, count) in [("a", "pa", 1), ("b", "pb", 2), ("pa", "", 3), ("pb", "", 4)] {
            lattice.concepts.push(LatticeConcept {
                id: id.into(),
                label: id.into(),
                family: "test".into(),
                intent: if parent.is_empty() {
                    vec!["general".into()]
                } else {
                    vec!["general".into(), id.into()]
                },
                extent_size: count,
                parents: if parent.is_empty() {
                    vec![]
                } else {
                    vec![parent.into()]
                },
                children: vec![],
            });
        }
        for (path, concept) in [("a.md", "a"), ("b.md", "b")] {
            let id = format!("section:{path}#1");
            lattice.heading_objects.push(HeadingObject {
                id: id.clone(),
                path: path.into(),
                title: "Shared".into(),
                heading_path: "Shared".into(),
                line: 1,
            });
            lattice.memberships.insert(
                id,
                StoredMembership {
                    concept_id: concept.into(),
                    label: concept.into(),
                    family: "test".into(),
                },
            );
        }
        crate::lattice::write_artifacts(
            crate::lattice::lattice_path(dir.path()).parent().unwrap(),
            &lattice,
        )
        .unwrap();
        run_nav(
            &index,
            dir.path(),
            NavAction::Goto,
            Some("section:a.md#1"),
            None,
            10,
        )
        .unwrap();
        run_nav(
            &index,
            dir.path(),
            NavAction::Goto,
            Some("section:b.md#1"),
            None,
            10,
        )
        .unwrap();
        run_nav(&index, dir.path(), NavAction::Back, None, None, 10).unwrap();
        let session = load_session(dir.path()).unwrap();
        assert_eq!(session.current_section.as_deref(), Some("section:a.md#1"));
        assert_eq!(session.current_concept.as_deref(), Some("a"));
        let parents = run_nav(&index, dir.path(), NavAction::Parent, None, None, 10).unwrap();
        assert_eq!(parents.entities[1]["symbol"], "pa");
        run_nav(&index, dir.path(), NavAction::Forward, None, None, 10).unwrap();
        assert_eq!(
            load_session(dir.path()).unwrap().current_concept.as_deref(),
            Some("b")
        );

        let mut session = load_session(dir.path()).unwrap();
        session.current_iri = Some("http://example.org/stale".into());
        session.last_results = vec![node_from_heading(lattice.heading_objects[0].clone())];
        save_session(dir.path(), &session).unwrap();
        run_nav(&index, dir.path(), NavAction::Select, None, Some(0), 10).unwrap();
        let session = load_session(dir.path()).unwrap();
        assert_eq!(session.current_section.as_deref(), Some("section:a.md#1"));
        assert_eq!(session.current_concept.as_deref(), Some("a"));
        assert!(session.current_iri.is_none());

        run_nav(&index, dir.path(), NavAction::Goto, Some("plain"), None, 10).unwrap();
        let session = load_session(dir.path()).unwrap();
        assert_eq!(session.current.as_ref().unwrap().symbol, "plain");
        assert!(session.current_concept.is_none());
        assert!(session.current_section.is_none());
        assert!(session.current_iri.is_none());
    }

    #[test]
    fn old_nav_nodes_remain_readable_without_identity_fields() {
        let node: NavNode =
            serde_json::from_value(json!({"path":"a.py", "symbol":"helper", "kind":"function"}))
                .unwrap();
        assert!(node.graph_symbol.is_none());
        assert!(node.section.is_none());
        assert!(node.line.is_none());
        let mut legacy_concept = node.clone();
        legacy_concept.kind = "fca_concept".into();
        legacy_concept.path = "language".into();
        let mut current_concept = legacy_concept.clone();
        current_concept.path.clear();
        assert!(legacy_concept.same_identity(&current_concept));
    }

    #[test]
    fn mixed_node_representations_keep_identity_without_duplicate_history() {
        let legacy = NavNode {
            path: "a.py".into(),
            symbol: "caller".into(),
            kind: "function".into(),
            line: None,
            graph_symbol: None,
            section: None,
        };
        let mut graph = legacy.clone();
        graph.graph_symbol = Some("urn:leio:code:symbol:caller".into());
        assert!(legacy.same_identity(&graph));
        assert_eq!(
            dedupe_nodes([legacy.clone(), graph.clone()].into_iter()),
            vec![graph.clone()]
        );
        let mut session = NavSession {
            current: Some(legacy.clone()),
            ..NavSession::default()
        };
        push_current(&mut session, graph.clone());
        push_current(&mut session, legacy);
        assert!(session.history.is_empty());
        assert_eq!(session.current.as_ref(), Some(&graph));

        let mut other_graph = graph.clone();
        other_graph.graph_symbol = Some("urn:leio:code:symbol:other".into());
        assert_eq!(dedupe_nodes([graph, other_graph].into_iter()).len(), 2);

        let heading = NavNode {
            path: "a.md".into(),
            symbol: "Shared".into(),
            kind: "wiki_section".into(),
            line: None,
            graph_symbol: None,
            section: Some("section:a.md#1".into()),
        };
        let mut legacy_heading = heading.clone();
        legacy_heading.section = None;
        assert!(heading.same_identity(&legacy_heading));
        let mut repeated_heading = heading.clone();
        repeated_heading.section = Some("section:a.md#5".into());
        assert!(!heading.same_identity(&repeated_heading));
    }

    #[test]
    fn save_and_load_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let session = NavSession {
            current: Some(NavNode {
                path: "src/nav.rs".to_string(),
                symbol: "run_nav".to_string(),
                kind: "function".to_string(),
                line: None,
                graph_symbol: None,
                section: None,
            }),
            ..NavSession::default()
        };
        save_session(dir.path(), &session).expect("save");
        let loaded = load_session(dir.path()).expect("load");
        assert_eq!(loaded.current, session.current);
    }

    #[test]
    fn push_current_records_history_and_clears_future() {
        let mut session = NavSession::default();
        push_current(
            &mut session,
            NavNode {
                path: "a.rs".into(),
                symbol: "a".into(),
                kind: "function".into(),
                line: None,
                graph_symbol: None,
                section: None,
            },
        );
        session.future.push(NavNode {
            path: "c.rs".into(),
            symbol: "c".into(),
            kind: "function".into(),
            line: None,
            graph_symbol: None,
            section: None,
        });
        push_current(
            &mut session,
            NavNode {
                path: "b.rs".into(),
                symbol: "b".into(),
                kind: "function".into(),
                line: None,
                graph_symbol: None,
                section: None,
            },
        );
        assert_eq!(session.history.len(), 1);
        assert_eq!(session.history[0].symbol, "a");
        assert_eq!(session.current.as_ref().unwrap().symbol, "b");
        assert!(session.future.is_empty());
    }

    #[test]
    fn merge_goto_hits_prefers_exact_index_symbol() {
        let exact = NavNode {
            path: "src/embed.rs".into(),
            symbol: "embed_query".into(),
            kind: "function".into(),
            line: None,
            graph_symbol: None,
            section: None,
        };
        let cosine = NavNode {
            path: "vendor/example-client/src/types/embed.rs".into(),
            symbol: "EmbedRequest".into(),
            kind: "struct".into(),
            line: None,
            graph_symbol: None,
            section: None,
        };
        let merged = merge_goto_hits(
            "embed_query",
            vec![exact.clone()],
            vec![cosine.clone(), exact.clone()],
            8,
        );
        assert_eq!(merged[0], exact);
        assert_eq!(merged.iter().filter(|node| node == &&exact).count(), 1);
        assert!(merged.contains(&cosine));
    }

    #[test]
    fn looks_like_iri_accepts_http_only() {
        assert!(looks_like_iri("http://example.org/kb#Alpha"));
        assert!(looks_like_iri(" https://example.org/x "));
        assert!(!looks_like_iri("embed_query"));
        assert!(!looks_like_iri("src/nav.rs"));
    }

    #[test]
    fn pin_iri_writes_current_iri_without_index() {
        let dir = tempfile::tempdir().expect("tempdir");
        let envelope = pin_iri(dir.path(), "http://example.org/kb#Alpha").unwrap();
        assert!(envelope.summary.contains("Alpha") || envelope.kind.contains("nav"));
        let session = load_session(dir.path()).unwrap();
        assert_eq!(
            session.current_iri.as_deref(),
            Some("http://example.org/kb#Alpha")
        );
        assert_eq!(session.last_action, "goto");
    }

    #[test]
    fn looks_like_heading_accepts_section_and_outline() {
        assert!(looks_like_heading("section:docs/page.md#12"));
        assert!(looks_like_heading("docs/page.md#12"));
        assert!(looks_like_heading("Root > Child"));
        assert!(!looks_like_heading("src/nav.rs"));
        assert!(!looks_like_heading("embed_query"));
    }

    #[test]
    fn session_envelope_surfaces_current_iri() {
        let session = NavSession {
            last_action: "here".to_string(),
            current: Some(NavNode {
                path: "docs/alpha.md".to_string(),
                symbol: "Alpha".to_string(),
                kind: "wiki_section".to_string(),
                line: None,
                graph_symbol: None,
                section: None,
            }),
            current_section: Some("section:docs/alpha.md#1".to_string()),
            current_iri: Some("http://example.org/kb#Alpha".to_string()),
            current_concept: Some("c1".to_string()),
            ..NavSession::default()
        };
        let envelope = session_envelope(&session, Vec::new(), Instant::now(), Path::new("."));
        assert_eq!(
            envelope.entities[0]["iri"].as_str().unwrap(),
            "http://example.org/kb#Alpha"
        );
        assert_eq!(
            envelope.entities[0]["section"].as_str().unwrap(),
            "section:docs/alpha.md#1"
        );
        assert_eq!(envelope.entities[0]["concept"].as_str().unwrap(), "c1");
        assert_eq!(
            envelope.meta.as_ref().unwrap()["current_iri"]
                .as_str()
                .unwrap(),
            "http://example.org/kb#Alpha"
        );
        assert!(
            envelope.summary.contains("http://example.org/kb#Alpha"),
            "{}",
            envelope.summary
        );
    }

    #[test]
    fn trim_stack_keeps_newest_entries() {
        let mut stack = (0..(MAX_HISTORY + 5))
            .map(|index| NavNode {
                path: format!("{index}.rs"),
                symbol: index.to_string(),
                kind: "function".into(),
                line: None,
                graph_symbol: None,
                section: None,
            })
            .collect::<Vec<_>>();
        trim_stack(&mut stack);
        assert_eq!(stack.len(), MAX_HISTORY);
        assert_eq!(stack[0].symbol, "5");
    }
}
