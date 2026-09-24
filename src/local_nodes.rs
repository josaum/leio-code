//! Local FCA + node store over `.leio-code/exports/arrow-nodes-v1/`.
//!
//! Search mmaps `nodes.search` first and falls back to the Arrow IPC stream
//! when the sidecar is missing or unreadable. Ranking first builds a lexical +
//! FCA shortlist, then reranks it with compatible stored vectors and bounded
//! on-demand candidate embeddings when `LEIO_CODE_EMBED_URL` is set.
//!
//! The store is process-wide so MCP / repeated in-process calls skip rebuild.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime};

use anyhow::{Context, Result, bail};
use arrow_array::{Array, Float32Array, ListArray, StringArray};
use rayon::prelude::*;
use serde_json::json;

use crate::arrow_ipc::read_ipc_stream_path;
use crate::embed::{candidate_embedding_text_fields, embed_query, embed_texts};
use crate::export::{
    default_arrow_nodes_output_dir, default_arrow_nodes_rows_path, default_search_sidecar_path,
};
use crate::model::{EvidenceItem, QueryEnvelope};
use crate::node_search::{self, PackedIndex};

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "is", "it", "of", "on",
    "or", "the", "to", "with",
];

/// Cosine at or above this receives the full semantic score contribution.
///
/// BGE-M3 in-domain neighbors for a multi-word task sit ~0.45–0.75;
/// 0.42 separates strong semantic neighbors from quarter-scale contributions.
const COSINE_KEEP: f32 = 0.42;

/// Scale cosine `[-1, 1]` onto the same integer score ladder as lexical
/// hits (exact symbol = 100). 0.75 cosine ≈ +60, so a strong semantic
/// neighbor can outrank a weak path substring (+12) but not an exact
/// symbol match.
const COSINE_SCORE_SCALE: f32 = 80.0;

/// `find` only short-circuits to local Arrow when the top hit is at least a
/// path-substring match. Weaker neighbors fall through to the symbol index.
const LOCAL_FIND_MIN_SCORE: i64 = 70;

/// Minimum semantic shortlist size for small requested result sets.
const MIN_SEMANTIC_CANDIDATES: usize = 32;
/// Maximum number of candidates sent through semantic reranking.
const MAX_SEMANTIC_CANDIDATES: usize = 256;
/// Candidate multiplier balancing recall against remote embedding cost.
const SEMANTIC_CANDIDATE_MULTIPLIER: usize = 8;

/// Upper bound on the inverse-document-frequency multiplier applied to per-term
/// lexical bonuses. Rare terms are boosted up to this factor so they outrank the
/// flood of common-term substring matches, but never above the exact/substring
/// signals (+70/+80/+100).
const TERM_WEIGHT_MAX: f32 = 4.5;

/// LEIO node Arrow column ordinals — must match `build_leio_row_batch`.
mod col {
    pub const PATH: usize = 4;
    pub const KIND: usize = 6;
    pub const SYMBOL: usize = 7;
    pub const RELATIONS: usize = 9;
    pub const SNIPPET: usize = 11;
    pub const CODE_VEC: usize = 14;
    pub const SEMANTIC_VEC: usize = 15;
}

/// One scored row from the local Arrow node file.
#[derive(Debug, Clone)]
pub struct NodeHit {
    pub path: String,
    pub kind: String,
    pub symbol: String,
    pub score: i64,
    pub matched_relations: Vec<String>,
    pub snippet: String,
    pub cosine: Option<f32>,
}

struct StoreKey {
    path: PathBuf,
    modified: SystemTime,
    len: u64,
}

struct MappedBatch {
    batch: arrow_array::RecordBatch,
    semantic_norms: Vec<f32>,
    code_norms: Vec<f32>,
}

enum StoreBody {
    Packed(PackedIndex),
    Arrow(Vec<MappedBatch>),
}

struct NodeStore {
    key: StoreKey,
    body: StoreBody,
    term_idf: HashMap<String, f32>,
}

static STORE: OnceLock<Mutex<Option<Arc<NodeStore>>>> = OnceLock::new();

/// True when the Arrow node export exists on disk.
pub fn available(repo_root: &Path) -> bool {
    nodes_path(repo_root).is_file()
}

fn nodes_path(repo_root: &Path) -> PathBuf {
    default_arrow_nodes_rows_path(&default_arrow_nodes_output_dir(repo_root))
}

fn store_lock() -> &'static Mutex<Option<Arc<NodeStore>>> {
    STORE.get_or_init(|| Mutex::new(None))
}

fn load_store(path: &Path) -> Result<Arc<NodeStore>> {
    let meta =
        std::fs::metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let key = StoreKey {
        path: path.to_path_buf(),
        modified,
        len: meta.len(),
    };
    if let Ok(guard) = store_lock().lock()
        && let Some(store) = guard.as_ref()
        && store.key.path == key.path
        && store.key.modified == key.modified
        && store.key.len == key.len
    {
        return Ok(Arc::clone(store));
    }
    let sidecar = default_search_sidecar_path(path.parent().unwrap_or(path));
    let body = match node_search::load_or_build(path, &sidecar) {
        Ok(packed) => StoreBody::Packed(packed),
        Err(_) => StoreBody::Arrow(
            read_ipc_stream_path(path)?
                .into_iter()
                .map(map_batch)
                .collect(),
        ),
    };
    let term_idf = compute_idf(&body);
    let store = Arc::new(NodeStore {
        key,
        body,
        term_idf,
    });
    if let Ok(mut guard) = store_lock().lock() {
        *guard = Some(Arc::clone(&store));
    }
    Ok(store)
}

fn map_batch(batch: arrow_array::RecordBatch) -> MappedBatch {
    if batch.num_columns() <= col::SEMANTIC_VEC {
        return MappedBatch {
            semantic_norms: Vec::new(),
            code_norms: Vec::new(),
            batch,
        };
    }
    let semantic = batch
        .column(col::SEMANTIC_VEC)
        .as_any()
        .downcast_ref::<ListArray>();
    let code = batch
        .column(col::CODE_VEC)
        .as_any()
        .downcast_ref::<ListArray>();
    MappedBatch {
        semantic_norms: list_norms(semantic),
        code_norms: list_norms(code),
        batch,
    }
}

fn list_norms(list: Option<&ListArray>) -> Vec<f32> {
    let Some(list) = list else {
        return Vec::new();
    };
    (0..list.len())
        .map(|row| row_vector_slice(list, row).map(l2_norm).unwrap_or(0.0))
        .collect()
}

/// Ranked hits for a natural-language or identifier needle.
pub fn search_hits(repo_root: &Path, needle: &str, limit: usize) -> Result<Vec<NodeHit>> {
    search_hits_detailed(repo_root, needle, limit).map(|result| result.hits)
}

pub(crate) fn search_hits_detailed(
    repo_root: &Path,
    needle: &str,
    limit: usize,
) -> Result<DetailedSearchResult> {
    search_ranked_hits_with(
        repo_root,
        needle,
        limit,
        SearchRanking::Semantic,
        || embed_query(repo_root, needle),
        |texts| embed_texts(repo_root, texts),
    )
}

/// Deterministic lexical/FCA ranking for cursor navigation and continuation.
/// Encoder availability cannot change the order between navigation pages.
pub fn search_navigation_hits(
    repo_root: &Path,
    needle: &str,
    limit: usize,
) -> Result<Vec<NodeHit>> {
    search_ranked_hits_with(
        repo_root,
        needle,
        limit,
        SearchRanking::Lexical,
        || embed_query(repo_root, needle),
        |texts| embed_texts(repo_root, texts),
    )
    .map(|ranked| ranked.hits)
}

#[derive(Clone, Copy)]
enum SearchRanking {
    Semantic,
    Lexical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SemanticSource {
    None,
    Precomputed,
    OnDemand,
}

impl SemanticSource {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Precomputed => "precomputed",
            Self::OnDemand => "on_demand",
        }
    }
}

#[derive(Debug)]
pub(crate) struct DetailedSearchResult {
    pub(crate) hits: Vec<NodeHit>,
    pub(crate) semantic_source: SemanticSource,
}

struct CandidateFields<'a> {
    snippet: &'a str,
    symbol: &'a str,
    kind: &'a str,
    path: &'a str,
}

struct Candidate<'a> {
    hit: NodeHit,
    fields: CandidateFields<'a>,
    stored_vectors: [Option<(&'a [f32], f32)>; 2],
}

fn semantic_candidate_limit(requested_limit: usize) -> usize {
    requested_limit
        .saturating_mul(SEMANTIC_CANDIDATE_MULTIPLIER)
        .clamp(MIN_SEMANTIC_CANDIDATES, MAX_SEMANTIC_CANDIDATES)
}

#[cfg(test)]
fn search_ranked_hits(
    repo_root: &Path,
    needle: &str,
    limit: usize,
    ranking: SearchRanking,
    encoder: impl FnOnce() -> Option<Vec<f32>>,
) -> Result<Vec<NodeHit>> {
    search_ranked_hits_with(
        repo_root,
        needle,
        limit,
        ranking,
        || encoder().ok_or_else(|| "query embedding unavailable".to_string()),
        |texts| embed_texts(repo_root, texts),
    )
    .map(|ranked| ranked.hits)
}

fn search_ranked_hits_with(
    repo_root: &Path,
    needle: &str,
    limit: usize,
    ranking: SearchRanking,
    query_encoder: impl FnOnce() -> std::result::Result<Vec<f32>, String>,
    candidate_encoder: impl FnOnce(&[String]) -> std::result::Result<Vec<Vec<f32>>, String>,
) -> Result<DetailedSearchResult> {
    let path = nodes_path(repo_root);
    if !path.is_file() {
        return Ok(DetailedSearchResult {
            hits: Vec::new(),
            semantic_source: SemanticSource::None,
        });
    }
    let store = load_store(&path)?;
    let terms = query_terms(needle);
    let relation_hints = relation_hints(&terms, needle);
    let needle_lc = needle.to_ascii_lowercase();
    let candidates = match &store.body {
        StoreBody::Packed(packed) => {
            score_packed(packed, &needle_lc, &terms, &relation_hints, &store.term_idf)
        }
        StoreBody::Arrow(batches) => batches
            .par_iter()
            .flat_map(|mapped| {
                score_batch(mapped, &needle_lc, &terms, &relation_hints, &store.term_idf)
                    .unwrap_or_default()
            })
            .collect(),
    };
    rerank_candidates(candidates, limit, ranking, query_encoder, candidate_encoder)
}

fn rerank_candidates(
    candidates: Vec<Candidate<'_>>,
    limit: usize,
    ranking: SearchRanking,
    query_encoder: impl FnOnce() -> std::result::Result<Vec<f32>, String>,
    candidate_encoder: impl FnOnce(&[String]) -> std::result::Result<Vec<Vec<f32>>, String>,
) -> Result<DetailedSearchResult> {
    rerank_candidates_with_text(
        candidates,
        limit,
        ranking,
        query_encoder,
        candidate_encoder,
        |fields| {
            candidate_embedding_text_fields(fields.snippet, fields.symbol, fields.kind, fields.path)
        },
    )
}

fn rerank_candidates_with_text(
    mut candidates: Vec<Candidate<'_>>,
    limit: usize,
    ranking: SearchRanking,
    query_encoder: impl FnOnce() -> std::result::Result<Vec<f32>, String>,
    candidate_encoder: impl FnOnce(&[String]) -> std::result::Result<Vec<Vec<f32>>, String>,
    text_builder: impl Fn(&CandidateFields<'_>) -> String,
) -> Result<DetailedSearchResult> {
    sort_candidates(&mut candidates);
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|candidate| {
        seen.insert((candidate.hit.path.clone(), candidate.hit.symbol.clone()))
    });

    if matches!(ranking, SearchRanking::Lexical) {
        candidates.truncate(limit.max(1));
        return Ok(DetailedSearchResult {
            hits: candidates
                .into_iter()
                .map(|candidate| candidate.hit)
                .collect(),
            semantic_source: SemanticSource::None,
        });
    }
    if candidates.is_empty() {
        return Ok(DetailedSearchResult {
            hits: Vec::new(),
            semantic_source: SemanticSource::None,
        });
    }

    let query = match query_encoder() {
        Ok(query) => query,
        Err(_) => {
            candidates.truncate(limit.max(1));
            return Ok(DetailedSearchResult {
                hits: candidates
                    .into_iter()
                    .map(|candidate| candidate.hit)
                    .collect(),
                semantic_source: SemanticSource::None,
            });
        }
    };
    candidates.truncate(semantic_candidate_limit(limit));
    let query_norm = l2_norm(&query);
    let mut used_precomputed = false;
    let mut missing = Vec::new();
    for (index, candidate) in candidates.iter_mut().enumerate() {
        let compatible = candidate
            .stored_vectors
            .iter()
            .flatten()
            .find(|(vector, norm)| vector.len() == query.len() && *norm > f32::EPSILON);
        if let Some((vector, norm)) = compatible {
            let cosine = cosine_pre_normed(&query, query_norm, vector, *norm);
            apply_cosine(&mut candidate.hit, cosine);
            used_precomputed = true;
        } else {
            missing.push(index);
        }
    }

    let mut semantic_source = if used_precomputed {
        SemanticSource::Precomputed
    } else {
        SemanticSource::None
    };
    if !missing.is_empty() {
        let texts = missing
            .iter()
            .map(|index| text_builder(&candidates[*index].fields))
            .collect::<Vec<_>>();
        if let Ok(vectors) = candidate_encoder(&texts)
            && vectors.len() == missing.len()
            && vectors.iter().all(|vector| vector.len() == query.len())
        {
            for (index, vector) in missing.into_iter().zip(vectors) {
                let norm = l2_norm(&vector);
                let cosine = cosine_pre_normed(&query, query_norm, &vector, norm);
                apply_cosine(&mut candidates[index].hit, cosine);
            }
            semantic_source = SemanticSource::OnDemand;
        }
    }

    sort_candidates(&mut candidates);
    candidates.truncate(limit.max(1));
    Ok(DetailedSearchResult {
        hits: candidates
            .into_iter()
            .map(|candidate| candidate.hit)
            .collect(),
        semantic_source,
    })
}

fn sort_candidates(candidates: &mut [Candidate<'_>]) {
    candidates.sort_by(|left, right| compare_hits(&left.hit, &right.hit));
}

fn compare_hits(left: &NodeHit, right: &NodeHit) -> std::cmp::Ordering {
    right
        .score
        .cmp(&left.score)
        .then_with(|| left.path.cmp(&right.path))
        .then_with(|| left.symbol.cmp(&right.symbol))
        .then_with(|| left.kind.cmp(&right.kind))
        .then_with(|| left.snippet.cmp(&right.snippet))
        .then_with(|| left.matched_relations.cmp(&right.matched_relations))
}

fn apply_cosine(hit: &mut NodeHit, cosine: f32) {
    hit.cosine = Some(cosine);
    if cosine >= COSINE_KEEP {
        hit.score += (cosine * COSINE_SCORE_SCALE).round() as i64;
    } else if hit.score > 0 {
        hit.score += (cosine * (COSINE_SCORE_SCALE / 4.0)).round() as i64;
    }
}

/// Adaptive-shaped envelope over the local Arrow file. `None` if the file is absent.
pub fn search_adaptive(repo_root: &Path, needle: &str, limit: usize) -> Option<QueryEnvelope> {
    if !available(repo_root) {
        return None;
    }
    let started = Instant::now();
    let ranked = match search_ranked_hits_with(
        repo_root,
        needle,
        limit,
        SearchRanking::Semantic,
        || embed_query(repo_root, needle),
        |texts| embed_texts(repo_root, texts),
    ) {
        Ok(ranked) => ranked,
        Err(err) => {
            return Some(QueryEnvelope {
                schema_version: crate::model::SCHEMA_VERSION.to_string(),
                query_id: format!(
                    "arrow-adaptive-{}",
                    time::OffsetDateTime::now_utc().unix_timestamp_nanos()
                ),
                kind: "arrow".to_string(),
                summary: format!("local Arrow adaptive search failed: {err}"),
                confidence: 0.0,
                entities: Vec::new(),
                evidence: Vec::new(),
                warnings: vec![err.to_string()],
                meta: Some(json!({"transport": "arrow_ipc", "store": "local_nodes"})),
                timing_ms: started.elapsed().as_millis(),
            });
        }
    };
    Some(hits_to_envelope(
        needle,
        ranked.hits,
        ranked.semantic_source,
        started,
    ))
}

/// Map Arrow hits into a `find` envelope (symbol-shaped).
pub fn search_as_find(repo_root: &Path, needle: &str, limit: usize) -> Option<QueryEnvelope> {
    if !available(repo_root) {
        return None;
    }
    let started = Instant::now();
    let hits = search_hits(repo_root, needle, limit).ok()?;
    if hits
        .first()
        .is_none_or(|hit| hit.score < LOCAL_FIND_MIN_SCORE)
    {
        return None;
    }
    Some(QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: format!(
            "find-arrow-{}",
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ),
        kind: "find".to_string(),
        summary: format!(
            "found {} local Arrow node matches for `{needle}`",
            hits.len()
        ),
        confidence: 0.9,
        entities: hits
            .iter()
            .map(|hit| {
                json!({
                    "name": hit.symbol,
                    "kind": hit.kind,
                    "path": hit.path,
                    "score": hit.score,
                    "matched_relations": hit.matched_relations,
                })
            })
            .collect(),
        evidence: hits
            .iter()
            .map(|hit| EvidenceItem {
                kind: hit.kind.clone(),
                path: hit.path.clone(),
                line: None,
                detail: format!("arrow score={} {}", hit.score, hit.symbol),
            })
            .collect(),
        warnings: Vec::new(),
        meta: Some(json!({"source": "arrow-nodes", "transport": "arrow_ipc"})),
        timing_ms: started.elapsed().as_millis(),
    })
}

fn hits_to_envelope(
    needle: &str,
    hits: Vec<NodeHit>,
    semantic_source: SemanticSource,
    started: Instant,
) -> QueryEnvelope {
    let (strategy, semantic_source_name) = match semantic_source {
        SemanticSource::None => ("arrow_ipc_local", "none"),
        SemanticSource::Precomputed => ("arrow_ipc_local+precomputed_cosine", "precomputed"),
        SemanticSource::OnDemand => ("arrow_ipc_local+on_demand_bge_m3", "on_demand"),
    };
    let entities = hits
        .iter()
        .enumerate()
        .map(|(rank, hit)| {
            json!({
                "node_id": format!("{}:{}", hit.path, hit.symbol),
                "path": hit.path,
                "kind": hit.kind,
                "symbol": hit.symbol,
                "text_snippet": hit.snippet,
                "relations": hit.matched_relations,
                "_search": {
                    "rank": rank + 1,
                    "score": hit.score,
                    "cosine": hit.cosine,
                    "matched_relations": hit.matched_relations,
                    "store": "arrow_ipc",
                }
            })
        })
        .collect::<Vec<_>>();
    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: format!(
            "arrow-adaptive-{}",
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ),
        kind: "arrow".to_string(),
        summary: format!(
            "local Arrow adaptive search for `{needle}` ({} hits)",
            entities.len()
        ),
        confidence: if entities.is_empty() { 0.4 } else { 0.93 },
        evidence: hits
            .iter()
            .map(|hit| EvidenceItem {
                kind: "arrow_node".to_string(),
                path: hit.path.clone(),
                line: None,
                detail: format!("score={} {}", hit.score, hit.symbol),
            })
            .collect(),
        entities,
        warnings: vec![format!("search_strategy: {strategy}")],
        meta: Some(json!({
            "transport": "arrow_ipc",
            "store": "local_nodes",
            "path": "exports/arrow-nodes-v1/nodes.arrow",
            "cosine": semantic_source != SemanticSource::None,
            "semantic_source": semantic_source_name,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn score_packed<'a>(
    packed: &'a PackedIndex,
    needle_lc: &str,
    terms: &[String],
    relation_hints: &[String],
    term_idf: &HashMap<String, f32>,
) -> Vec<Candidate<'a>> {
    (0..packed.n())
        .into_par_iter()
        .filter_map(|row| {
            let path = packed.path(row);
            let kind = packed.kind(row);
            let symbol = packed.symbol(row);
            let snippet = packed.snippet(row);
            let mut score = local_score(path, symbol, snippet, needle_lc, terms, term_idf);
            let mut matched = Vec::new();
            let relations = packed.relations(row);
            for hint in relation_hints {
                if ascii_eq_ignore_case(relations, hint)
                    || ascii_contains_ignore_case(relations, hint)
                {
                    score += 18;
                    matched.push(hint.clone());
                }
            }
            if score <= 0 {
                return None;
            }
            Some(Candidate {
                hit: NodeHit {
                    path: path.to_string(),
                    kind: kind.to_string(),
                    symbol: symbol.to_string(),
                    score,
                    matched_relations: matched,
                    snippet: snippet.chars().take(240).collect(),
                    cosine: None,
                },
                fields: CandidateFields {
                    snippet,
                    symbol,
                    kind,
                    path,
                },
                stored_vectors: [
                    packed.vector(row).map(|vector| (vector, packed.norm(row))),
                    None,
                ],
            })
        })
        .collect()
}

fn score_batch<'a>(
    mapped: &'a MappedBatch,
    needle_lc: &str,
    terms: &[String],
    relation_hints: &[String],
    term_idf: &HashMap<String, f32>,
) -> Result<Vec<Candidate<'a>>> {
    let batch = &mapped.batch;
    if batch.num_columns() <= col::SEMANTIC_VEC {
        bail!(
            "nodes.arrow has {} columns; need at least {}",
            batch.num_columns(),
            col::SEMANTIC_VEC + 1
        );
    }
    let paths = utf8_col(batch, col::PATH, "path")?;
    let kinds = utf8_col(batch, col::KIND, "kind")?;
    let symbols = utf8_col(batch, col::SYMBOL, "symbol")?;
    let relations = batch
        .column(col::RELATIONS)
        .as_any()
        .downcast_ref::<ListArray>()
        .context("nodes.arrow column 9 is not List relations")?;
    let snippets = utf8_col(batch, col::SNIPPET, "text_snippet")?;
    let code_vecs = batch
        .column(col::CODE_VEC)
        .as_any()
        .downcast_ref::<ListArray>();
    let semantic_vecs = batch
        .column(col::SEMANTIC_VEC)
        .as_any()
        .downcast_ref::<ListArray>();

    let mut candidates = Vec::new();
    for row in 0..batch.num_rows() {
        let path = paths.value(row);
        let kind = kinds.value(row);
        let symbol = symbols.value(row);
        let snippet = snippets.value(row);
        let mut score = local_score(path, symbol, snippet, needle_lc, terms, term_idf);
        let mut matched = Vec::new();
        for hint in relation_hints {
            if relation_matches(relations, row, hint) {
                score += 18;
                matched.push(hint.clone());
            }
        }
        if score <= 0 {
            continue;
        }
        let semantic = mapped
            .semantic_norms
            .get(row)
            .copied()
            .filter(|norm| *norm > f32::EPSILON)
            .and_then(|norm| row_vector_slice(semantic_vecs?, row).map(|vector| (vector, norm)));
        let code = mapped
            .code_norms
            .get(row)
            .copied()
            .filter(|norm| *norm > f32::EPSILON)
            .and_then(|norm| row_vector_slice(code_vecs?, row).map(|vector| (vector, norm)));
        candidates.push(Candidate {
            hit: NodeHit {
                path: path.to_string(),
                kind: kind.to_string(),
                symbol: symbol.to_string(),
                score,
                matched_relations: matched,
                snippet: snippet.chars().take(240).collect(),
                cosine: None,
            },
            fields: CandidateFields {
                snippet,
                symbol,
                kind,
                path,
            },
            stored_vectors: [semantic, code],
        });
    }
    Ok(candidates)
}

fn local_score(
    path: &str,
    symbol: &str,
    snippet: &str,
    needle_lc: &str,
    terms: &[String],
    term_idf: &HashMap<String, f32>,
) -> i64 {
    let mut score = 0;
    if !needle_lc.is_empty() && ascii_eq_ignore_case(symbol, needle_lc) {
        score += 100;
    } else if !needle_lc.is_empty() && ascii_contains_ignore_case(symbol, needle_lc) {
        score += 80;
    }
    if !needle_lc.is_empty() && ascii_contains_ignore_case(path, needle_lc) {
        score += 70;
    }
    for term in terms {
        let weight = term_weight(term_idf.get(term).copied());
        if ascii_contains_ignore_case(path, term) {
            score += (12.0 * weight).round() as i64;
        }
        if ascii_contains_ignore_case(symbol, term) {
            score += (14.0 * weight).round() as i64;
        }
        if ascii_contains_ignore_case(snippet, term) {
            score += (8.0 * weight).round() as i64;
        }
    }
    score
}

/// IDF multiplier for a term, clamped so rare terms are boosted without
/// outranking the exact/substring signals. Unseen terms default to 1.0 (no
/// boost) because a substring-only match has no reliable document frequency.
fn term_weight(idf: Option<f32>) -> f32 {
    idf.unwrap_or(1.0).clamp(1.0, TERM_WEIGHT_MAX)
}

/// Smooth inverse document frequency over every node's path + symbol + snippet
/// tokens. Computed once per store load so rare terms can outrank the flood of
/// common-term substring matches.
fn compute_idf(body: &StoreBody) -> HashMap<String, f32> {
    let mut df: HashMap<String, u32> = HashMap::new();
    let total = match body {
        StoreBody::Packed(packed) => {
            for row in 0..packed.n() {
                count_doc_terms(
                    &mut df,
                    packed.path(row),
                    packed.symbol(row),
                    packed.snippet(row),
                );
            }
            packed.n()
        }
        StoreBody::Arrow(batches) => {
            for mapped in batches {
                let Ok(paths) = utf8_col(&mapped.batch, col::PATH, "path") else {
                    continue;
                };
                let Ok(symbols) = utf8_col(&mapped.batch, col::SYMBOL, "symbol") else {
                    continue;
                };
                let Ok(snippets) = utf8_col(&mapped.batch, col::SNIPPET, "text_snippet") else {
                    continue;
                };
                for row in 0..mapped.batch.num_rows() {
                    count_doc_terms(
                        &mut df,
                        paths.value(row),
                        symbols.value(row),
                        snippets.value(row),
                    );
                }
            }
            batches.iter().map(|mapped| mapped.batch.num_rows()).sum()
        }
    };
    if total == 0 {
        return HashMap::new();
    }
    let n = total as f32;
    df.into_iter()
        .map(|(term, count)| {
            let idf = ((n + 1.0) / (count as f32 + 1.0)).ln() + 1.0;
            (term, idf)
        })
        .collect()
}

fn count_doc_terms(df: &mut HashMap<String, u32>, path: &str, symbol: &str, snippet: &str) {
    let mut seen: HashSet<String> = HashSet::new();
    for text in [path, symbol, snippet] {
        for term in tokenize(text) {
            if seen.insert(term.clone()) {
                *df.entry(term).or_insert(0) += 1;
            }
        }
    }
}

fn utf8_col<'a>(
    batch: &'a arrow_array::RecordBatch,
    index: usize,
    name: &str,
) -> Result<&'a StringArray> {
    batch
        .column(index)
        .as_any()
        .downcast_ref::<StringArray>()
        .with_context(|| format!("nodes.arrow column {index} ({name}) is not Utf8"))
}

fn relation_matches(list: &ListArray, row: usize, hint: &str) -> bool {
    if row >= list.len() || list.is_null(row) {
        return false;
    }
    let start = list
        .value_offsets()
        .get(row)
        .copied()
        .and_then(|off| usize::try_from(off).ok())
        .unwrap_or(0);
    let len = usize::try_from(list.value_length(row)).unwrap_or(0);
    let Some(end) = start.checked_add(len) else {
        return false;
    };
    let Some(values) = list.values().as_any().downcast_ref::<StringArray>() else {
        return false;
    };
    (start..end).any(|index| {
        if index >= values.len() || values.is_null(index) {
            return false;
        }
        let rel = values.value(index);
        ascii_eq_ignore_case(rel, hint) || ascii_contains_ignore_case(rel, hint)
    })
}

fn row_vector_slice(list: &ListArray, row: usize) -> Option<&[f32]> {
    if list.is_null(row) {
        return None;
    }
    let start = list.value_offsets()[row] as usize;
    let len = list.value_length(row) as usize;
    if len == 0 {
        return None;
    }
    let values = list.values().as_any().downcast_ref::<Float32Array>()?;
    let rel = start.checked_sub(values.offset())?;
    let slice = values.values().get(rel..rel + len)?;
    slice
        .iter()
        .any(|value| value.abs() > f32::EPSILON)
        .then_some(slice)
}

fn l2_norm(values: &[f32]) -> f32 {
    dot_f32(values, values).sqrt()
}

fn cosine_pre_normed(left: &[f32], left_norm: f32, right: &[f32], right_norm: f32) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    let denom = left_norm * right_norm;
    if denom <= f32::EPSILON {
        0.0
    } else {
        (dot_f32(left, right) / denom).clamp(-1.0, 1.0)
    }
}

#[cfg(test)]
fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    cosine_pre_normed(left, l2_norm(left), right, l2_norm(right))
}

/// Four-wide FMA so LLVM emits NEON/AVX without `unsafe`.
#[inline]
fn dot_f32(left: &[f32], right: &[f32]) -> f32 {
    let n = left.len().min(right.len());
    let mut index = 0;
    let mut acc0 = 0.0f32;
    let mut acc1 = 0.0f32;
    let mut acc2 = 0.0f32;
    let mut acc3 = 0.0f32;
    while index + 4 <= n {
        acc0 += left[index] * right[index];
        acc1 += left[index + 1] * right[index + 1];
        acc2 += left[index + 2] * right[index + 2];
        acc3 += left[index + 3] * right[index + 3];
        index += 4;
    }
    let mut tail = acc0 + acc1 + acc2 + acc3;
    while index < n {
        tail += left[index] * right[index];
        index += 1;
    }
    tail
}

fn ascii_eq_ignore_case(left: &str, right_lc: &str) -> bool {
    if left.len() != right_lc.len() {
        return false;
    }
    left.as_bytes()
        .iter()
        .zip(right_lc.as_bytes())
        .all(|(a, b)| a.to_ascii_lowercase() == *b)
}

fn ascii_contains_ignore_case(haystack: &str, needle_lc: &str) -> bool {
    if needle_lc.is_empty() {
        return true;
    }
    if !haystack.is_ascii() {
        return haystack.to_ascii_lowercase().contains(needle_lc);
    }
    let hay = haystack.as_bytes();
    let needle = needle_lc.as_bytes();
    if hay.len() < needle.len() {
        return false;
    }
    hay.windows(needle.len()).any(|window| {
        window
            .iter()
            .zip(needle)
            .all(|(a, b)| a.to_ascii_lowercase() == *b)
    })
}

fn query_terms(needle: &str) -> Vec<String> {
    tokenize(needle)
}

/// Tokenize arbitrary text into lowercase terms, splitting on non-alphanumeric
/// boundaries and on camelCase / digit transitions so that `reprocessCandidates`,
/// `reprocess_candidates`, and `reprocess candidates` share the same tokens.
fn tokenize(text: &str) -> Vec<String> {
    let mut raw = Vec::new();
    for chunk in text.split(|ch: char| !ch.is_ascii_alphanumeric()) {
        push_identifier_parts(chunk, &mut raw);
    }
    let mut terms = Vec::new();
    for token in raw {
        let slug = token.to_ascii_lowercase();
        if slug.len() < 2 || STOPWORDS.contains(&slug.as_str()) {
            continue;
        }
        if !terms.iter().any(|seen| seen == &slug) {
            terms.push(slug);
        }
    }
    terms
}

/// Split one alphanumeric run into parts at camelCase and digit boundaries.
/// Snake_case and other separators are already handled by the caller's split.
fn push_identifier_parts(chunk: &str, out: &mut Vec<String>) {
    let bytes = chunk.as_bytes();
    let mut start = 0usize;
    for i in 1..bytes.len() {
        let prev = bytes[i - 1];
        let cur = bytes[i];
        let boundary = (prev.is_ascii_lowercase() && cur.is_ascii_uppercase())
            || (prev.is_ascii_digit() && cur.is_ascii_alphabetic())
            || (prev.is_ascii_alphabetic() && cur.is_ascii_digit());
        if boundary {
            out.push(chunk[start..i].to_string());
            start = i;
        }
    }
    if start < chunk.len() {
        out.push(chunk[start..].to_string());
    }
}

fn relation_hints(terms: &[String], needle: &str) -> Vec<String> {
    let mut hints = Vec::new();
    for raw in needle.split_whitespace() {
        let trimmed = raw.trim_matches(|ch: char| ch.is_ascii_punctuation());
        if trimmed.starts_with("fcaConcept:") || trimmed.starts_with("fcaFamily:") {
            hints.push(trimmed.to_ascii_lowercase());
        }
    }
    for term in terms {
        hints.push(format!("fcafamily:{term}"));
        hints.push(format!("fcaintent:{term}"));
        hints.push(format!("fcaintent:topic_{term}"));
        hints.push(format!("topic:{term}"));
    }
    hints
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_terms_drops_stopwords() {
        assert_eq!(
            query_terms("find the RDF namespace"),
            vec!["find", "rdf", "namespace"]
        );
    }

    #[test]
    fn tokenize_splits_camel_case_and_digits() {
        // "by" is a stopword and "2" is shorter than the minimum term length,
        // so both are dropped after the identifier split.
        assert_eq!(
            tokenize("selectReprocessCandidatesByColumn"),
            vec!["select", "reprocess", "candidates", "column"]
        );
        assert_eq!(
            tokenize("buildLeioRowBatch2"),
            vec!["build", "leio", "row", "batch"]
        );
    }

    #[test]
    fn tokenize_shares_tokens_across_identifier_styles() {
        let camel = tokenize("selectReprocessCandidates");
        let snake = tokenize("select_reprocess_candidates");
        let spaced = tokenize("select reprocess candidates");
        assert_eq!(camel, snake);
        assert_eq!(snake, spaced);
    }

    #[test]
    fn term_weight_boosts_rare_terms_only() {
        // Unseen terms get no boost (substring-only matches have no reliable DF).
        assert_eq!(term_weight(None), 1.0);
        // A common term (idf near 1) is unchanged.
        assert_eq!(term_weight(Some(1.0)), 1.0);
        // A rare term is boosted but clamped to TERM_WEIGHT_MAX.
        assert_eq!(term_weight(Some(3.0)), 3.0);
        assert_eq!(term_weight(Some(20.0)), TERM_WEIGHT_MAX);
    }

    #[test]
    fn local_score_weights_rare_terms_above_common_ones() {
        let idf = HashMap::from([("reprocess".to_string(), 4.5), ("column".to_string(), 1.0)]);
        let rare = local_score(
            "src/audit.rs",
            "select_reprocess_candidates",
            "",
            "reprocess column",
            &["reprocess".to_string(), "column".to_string()],
            &idf,
        );
        let common = local_score(
            "src/util.rs",
            "column_order",
            "",
            "reprocess column",
            &["column".to_string()],
            &idf,
        );
        assert!(rare > common);
    }

    #[test]
    fn relation_hints_include_fca_and_topics() {
        let terms = query_terms("rdf namespace");
        let hints = relation_hints(&terms, "rdf namespace");
        assert!(hints.iter().any(|hint| hint.contains("rdf")));
        assert!(hints.iter().any(|hint| hint.contains("topic_")));
    }

    #[test]
    fn search_hits_empty_without_file() {
        let hits = search_hits(Path::new("/tmp/no-such-leio-repo"), "rdf", 8).expect("ok");
        assert!(hits.is_empty());
    }

    #[test]
    fn detailed_search_reports_no_semantic_source_without_file() {
        let result = search_hits_detailed(Path::new("/tmp/no-such-leio-repo"), "rdf", 8)
            .expect("detailed search");
        assert!(result.hits.is_empty());
        assert_eq!(result.semantic_source, SemanticSource::None);
        assert_eq!(result.semantic_source.as_str(), "none");
    }

    #[test]
    fn available_false_without_export() {
        assert!(!available(Path::new("/tmp/no-such-leio-repo")));
    }

    #[test]
    fn cosine_similarity_identical_unit_vectors_is_one() {
        let vector = [0.6_f32, 0.8];
        assert!((cosine_similarity(&vector, &vector) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_orthogonal_is_zero() {
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn ascii_contains_does_not_allocate_for_ascii() {
        assert!(ascii_contains_ignore_case(
            "NamespaceResolver",
            "namespaceresolver"
        ));
        assert!(ascii_contains_ignore_case("src/Foo.rs", "foo"));
        assert!(!ascii_contains_ignore_case("src/Foo.rs", "bar"));
    }

    #[test]
    fn four_wide_dot_matches_scalar() {
        let left: Vec<f32> = (0..1024).map(|i| (i as f32) * 0.001).collect();
        let right: Vec<f32> = (0..1024).map(|i| 1.0 - (i as f32) * 0.0005).collect();
        let wide = dot_f32(&left, &right);
        let scalar: f32 = left.iter().zip(&right).map(|(a, b)| a * b).sum();
        assert!((wide - scalar).abs() < 1e-3);
    }

    #[test]
    fn cosine_pre_normed_rejects_mismatched_lengths() {
        assert_eq!(cosine_pre_normed(&[1.0, 0.0], 1.0, &[1.0], 1.0), 0.0);
    }

    #[test]
    fn score_packed_requires_positive_local_score_before_semantics() {
        use crate::node_rows::build_leio_row_batch;
        use arrow_ipc::writer::StreamWriter;
        use serde_json::json;
        use std::fs::File;

        let dir = tempfile::tempdir().expect("temp");
        let arrow_path = dir.path().join("nodes.arrow");
        let sidecar_path = dir.path().join("nodes.search");
        let entities = vec![json!({
            "node_id": "n1",
            "tenant_id": "t",
            "repo": "r",
            "rev": "1",
            "path": "src/nav.rs",
            "lang": "rust",
            "kind": "function",
            "symbol": "run_nav",
            "target": "",
            "relations": ["fcaFamily:nav"],
            "metadata": {},
            "text_snippet": "fn run_nav()",
            "embed_model": "BAAI/bge-m3",
            "embed_dim": 4,
            "code_vec": [0.0, 1.0, 0.0, 0.0],
            "semantic_vec": [0.6, 0.8, 0.0, 0.0],
            "ontology_vec": [0.0],
            "execution_vec_bin": [],
        })];
        let batch = build_leio_row_batch(&entities).expect("batch");
        {
            let file = File::create(&arrow_path).expect("create");
            let mut writer = StreamWriter::try_new(file, &batch.schema()).expect("writer");
            writer.write(&batch).expect("write");
            writer.finish().expect("finish");
        }
        let packed = node_search::load_or_build(&arrow_path, &sidecar_path).expect("sidecar");
        let hits = score_packed(
            &packed,
            "unrelated-needle",
            &[],
            &[],
            &std::collections::HashMap::new(),
        );
        assert!(hits.is_empty());
    }

    #[test]
    fn map_batch_and_score_batch_tolerate_short_schema() {
        use std::sync::Arc;

        use arrow_array::{ArrayRef, Int32Array, RecordBatch};
        use arrow_schema::{DataType, Field, Schema};

        let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int32, false)]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Int32Array::from(vec![1])) as ArrayRef],
        )
        .expect("batch");
        let mapped = map_batch(batch);
        assert!(mapped.semantic_norms.is_empty());
        assert!(mapped.code_norms.is_empty());
        assert!(score_batch(&mapped, "x", &[], &[], &std::collections::HashMap::new()).is_err());
    }

    fn write_export_repo(entities: &[serde_json::Value]) -> tempfile::TempDir {
        use crate::node_rows::build_leio_row_batch;
        use arrow_ipc::writer::StreamWriter;
        use std::fs::File;

        let dir = tempfile::tempdir().expect("temp");
        let export = default_arrow_nodes_output_dir(dir.path());
        std::fs::create_dir_all(&export).expect("export dir");
        let arrow_path = default_arrow_nodes_rows_path(&export);
        let batch = build_leio_row_batch(entities).expect("batch");
        let file = File::create(&arrow_path).expect("create");
        let mut writer = StreamWriter::try_new(file, &batch.schema()).expect("writer");
        writer.write(&batch).expect("write");
        writer.finish().expect("finish");
        dir
    }

    fn sample_nav_entity() -> serde_json::Value {
        serde_json::json!({
            "node_id": "n1",
            "tenant_id": "t",
            "repo": "r",
            "rev": "1",
            "path": "src/nav.rs",
            "lang": "rust",
            "kind": "function",
            "symbol": "run_nav",
            "target": "",
            "relations": ["fcaFamily:nav"],
            "metadata": {},
            "text_snippet": "fn run_nav()",
            "embed_model": "BAAI/bge-m3",
            "embed_dim": 4,
            "code_vec": [0.0, 0.0, 0.0, 0.0],
            "semantic_vec": [0.0, 0.0, 0.0, 0.0],
            "ontology_vec": [0.0],
            "execution_vec_bin": [],
        })
    }

    #[test]
    fn navigation_ranking_never_encodes_and_keeps_stable_deduplicated_pages() {
        let mut entities = Vec::new();
        for (rank, letter) in ["e", "c", "a", "d", "b"].iter().enumerate() {
            let mut entity = sample_nav_entity();
            entity["node_id"] = json!(format!("matching-{rank}"));
            entity["path"] = json!(format!("src/{letter}.rs"));
            entity["symbol"] = json!("matching");
            entity["text_snippet"] = json!("matching source");
            entity["semantic_vec"] = if rank % 2 == 0 {
                json!([1.0, 0.0, 0.0, 0.0])
            } else {
                json!([0.0, 1.0, 0.0, 0.0])
            };
            entities.push(entity);
        }
        // The weaker duplicate sorts below other distinct hits, so adjacent
        // deduplication would let it leak into a later page.
        let mut duplicate = entities[2].clone();
        duplicate["node_id"] = json!("duplicate-a");
        duplicate["text_snippet"] = json!("");
        entities.push(duplicate);
        let dir = write_export_repo(&entities);
        let all = search_ranked_hits(dir.path(), "matching", 99, SearchRanking::Lexical, || {
            panic!("navigation must not request an embedding")
        })
        .unwrap();
        let expected: Vec<_> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|letter| format!("src/{letter}.rs"))
            .collect();
        assert_eq!(
            all.iter().map(|hit| hit.path.clone()).collect::<Vec<_>>(),
            expected
        );
        assert!(all.iter().all(|hit| hit.cosine.is_none()));
        for limit in [1, 3, 5, 99] {
            let prefix = search_navigation_hits(dir.path(), "matching", limit).unwrap();
            assert_eq!(
                prefix
                    .iter()
                    .map(|hit| hit.path.clone())
                    .collect::<Vec<_>>(),
                expected.iter().take(limit).cloned().collect::<Vec<_>>()
            );
        }

        let index = crate::indexer::load_or_build_index(
            dir.path(),
            &crate::indexer::default_index_path(dir.path()),
        )
        .unwrap();
        let mut pages = Vec::new();
        for offset in [0, 2, 4] {
            let envelope = crate::nav::run_nav_page(
                &index,
                dir.path(),
                crate::nav::NavAction::Goto,
                Some("matching"),
                None,
                2,
                offset,
            )
            .unwrap();
            pages.extend(
                envelope
                    .entities
                    .iter()
                    .skip(1)
                    .map(|row| row["path"].as_str().unwrap().to_string()),
            );
        }
        assert_eq!(pages, expected);

        crate::nav::run_nav_page(
            &index,
            dir.path(),
            crate::nav::NavAction::Goto,
            Some("matching"),
            None,
            2,
            0,
        )
        .unwrap();
        let session_before = std::fs::read(crate::nav::session_path(dir.path())).unwrap();
        let sidecar = default_search_sidecar_path(&default_arrow_nodes_output_dir(dir.path()));
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(sidecar)
            .unwrap()
            .write_all(b"changed")
            .unwrap();
        let continuation = crate::nav::run_nav_page(
            &index,
            dir.path(),
            crate::nav::NavAction::Goto,
            Some("matching"),
            None,
            2,
            2,
        );
        assert!(
            continuation
                .unwrap_err()
                .to_string()
                .contains("continuation changed")
        );
        assert_eq!(
            std::fs::read(crate::nav::session_path(dir.path())).unwrap(),
            session_before
        );
    }

    #[test]
    fn semantic_search_with_zero_candidates_calls_neither_encoder() {
        use std::cell::Cell;

        let dir = write_export_repo(&[sample_nav_entity()]);
        let query_calls = Cell::new(0);
        let candidate_calls = Cell::new(0);
        let ranked = search_ranked_hits_with(
            dir.path(),
            "absent-term",
            8,
            SearchRanking::Semantic,
            || {
                query_calls.set(query_calls.get() + 1);
                Ok(vec![1.0, 0.0])
            },
            |_| {
                candidate_calls.set(candidate_calls.get() + 1);
                Ok(Vec::new())
            },
        )
        .expect("search");
        assert!(ranked.hits.is_empty());
        assert_eq!(query_calls.get(), 0);
        assert_eq!(candidate_calls.get(), 0);
    }

    #[test]
    fn lexical_search_calls_neither_encoder() {
        let dir = write_export_repo(&[sample_nav_entity()]);
        let ranked = search_ranked_hits_with(
            dir.path(),
            "run_nav",
            8,
            SearchRanking::Lexical,
            || panic!("lexical search must not embed the query"),
            |_| panic!("lexical search must not embed candidates"),
        )
        .expect("search");
        assert_eq!(ranked.hits.len(), 1);
        assert_eq!(ranked.semantic_source, SemanticSource::None);
    }

    #[test]
    fn semantic_candidate_cap_is_exact_and_applied_before_embedding() {
        use std::cell::Cell;

        assert_eq!(semantic_candidate_limit(0), 32);
        assert_eq!(semantic_candidate_limit(4), 32);
        assert_eq!(semantic_candidate_limit(5), 40);
        assert_eq!(semantic_candidate_limit(32), 256);
        assert_eq!(semantic_candidate_limit(usize::MAX), 256);

        let entities = (0..40)
            .map(|index| {
                let mut entity = sample_nav_entity();
                entity["node_id"] = json!(format!("n{index}"));
                entity["path"] = json!(format!("src/{index:02}.rs"));
                entity["symbol"] = json!(format!("matching_{index:02}"));
                entity["text_snippet"] = json!(format!("matching candidate {index:02}"));
                entity
            })
            .collect::<Vec<_>>();
        let dir = write_export_repo(&entities);
        let embedded_count = Cell::new(0);
        let ranked = search_ranked_hits_with(
            dir.path(),
            "matching",
            1,
            SearchRanking::Semantic,
            || Ok(vec![1.0, 0.0]),
            |texts| {
                embedded_count.set(texts.len());
                Ok(vec![vec![1.0, 0.0]; texts.len()])
            },
        )
        .expect("search");
        assert_eq!(embedded_count.get(), 32);
        assert_eq!(ranked.hits.len(), 1);
    }

    #[test]
    fn semantic_rerank_embeds_only_missing_or_incompatible_candidates() {
        use std::cell::Cell;

        let mut compatible = sample_nav_entity();
        compatible["path"] = json!("src/a.rs");
        compatible["symbol"] = json!("matching_a");
        compatible["semantic_vec"] = json!([1.0, 0.0]);
        let mut incompatible = sample_nav_entity();
        incompatible["path"] = json!("src/b.rs");
        incompatible["symbol"] = json!("matching_b");
        incompatible["semantic_vec"] = json!([1.0, 0.0, 0.0]);
        let mut missing = sample_nav_entity();
        missing["path"] = json!("src/c.rs");
        missing["symbol"] = json!("matching_c");
        missing["semantic_vec"] = json!([0.0, 0.0]);
        let dir = write_export_repo(&[compatible, incompatible, missing]);
        let embedded_count = Cell::new(0);
        let ranked = search_ranked_hits_with(
            dir.path(),
            "matching",
            8,
            SearchRanking::Semantic,
            || Ok(vec![1.0, 0.0]),
            |texts| {
                embedded_count.set(texts.len());
                Ok(vec![vec![0.0, 1.0]; texts.len()])
            },
        )
        .expect("search");
        assert_eq!(embedded_count.get(), 2);
        assert_eq!(ranked.semantic_source, SemanticSource::OnDemand);
    }

    #[test]
    fn all_compatible_candidates_skip_candidate_embedding() {
        let mut entity = sample_nav_entity();
        entity["symbol"] = json!("matching");
        entity["semantic_vec"] = json!([1.0, 0.0]);
        let dir = write_export_repo(&[entity]);
        let ranked = search_ranked_hits_with(
            dir.path(),
            "matching",
            8,
            SearchRanking::Semantic,
            || Ok(vec![1.0, 0.0]),
            |_| panic!("compatible precomputed vectors must skip candidate embedding"),
        )
        .expect("search");
        assert_eq!(ranked.semantic_source, SemanticSource::Precomputed);
        assert!(ranked.hits[0].cosine.is_some());
    }

    #[test]
    fn candidate_embedding_failure_preserves_lexical_and_precomputed_scores() {
        let mut precomputed = sample_nav_entity();
        precomputed["path"] = json!("src/a.rs");
        precomputed["symbol"] = json!("matching_a");
        precomputed["semantic_vec"] = json!([1.0, 0.0]);
        let mut missing = sample_nav_entity();
        missing["path"] = json!("src/b.rs");
        missing["symbol"] = json!("matching_b");
        missing["semantic_vec"] = json!([0.0, 0.0]);
        let dir = write_export_repo(&[precomputed, missing]);
        let lexical = search_ranked_hits_with(
            dir.path(),
            "matching",
            8,
            SearchRanking::Lexical,
            || unreachable!(),
            |_| unreachable!(),
        )
        .expect("lexical");
        let ranked = search_ranked_hits_with(
            dir.path(),
            "matching",
            8,
            SearchRanking::Semantic,
            || Ok(vec![1.0, 0.0]),
            |_| Err("candidate encoder unavailable".to_string()),
        )
        .expect("semantic");
        let lexical_missing = lexical
            .hits
            .iter()
            .find(|hit| hit.path == "src/b.rs")
            .expect("lexical missing");
        let ranked_missing = ranked
            .hits
            .iter()
            .find(|hit| hit.path == "src/b.rs")
            .expect("ranked missing");
        assert_eq!(ranked_missing.score, lexical_missing.score);
        assert!(ranked_missing.cosine.is_none());
        assert!(ranked.hits.iter().any(|hit| hit.cosine.is_some()));
        assert_eq!(ranked.semantic_source, SemanticSource::Precomputed);
    }

    #[test]
    fn invalid_candidate_embedding_shapes_preserve_lexical_scores() {
        let mut entity = sample_nav_entity();
        entity["symbol"] = json!("matching");
        entity["semantic_vec"] = json!([0.0, 0.0]);
        let dir = write_export_repo(&[entity]);
        let lexical = search_ranked_hits_with(
            dir.path(),
            "matching",
            8,
            SearchRanking::Lexical,
            || unreachable!(),
            |_| unreachable!(),
        )
        .expect("lexical");
        for vectors in [Vec::new(), vec![vec![1.0]]] {
            let ranked = search_ranked_hits_with(
                dir.path(),
                "matching",
                8,
                SearchRanking::Semantic,
                || Ok(vec![1.0, 0.0]),
                |_| Ok(vectors.clone()),
            )
            .expect("semantic");
            assert_eq!(ranked.hits[0].score, lexical.hits[0].score);
            assert!(ranked.hits[0].cosine.is_none());
            assert_eq!(ranked.semantic_source, SemanticSource::None);
        }
    }

    #[test]
    fn query_embedding_failure_returns_uncapped_lexical_ranking() {
        let entities = (0..300)
            .map(|index| {
                let mut entity = sample_nav_entity();
                entity["node_id"] = json!(format!("n{index}"));
                entity["path"] = json!(format!("src/{index:03}.rs"));
                entity["symbol"] = json!(format!("matching_{index:03}"));
                entity
            })
            .collect::<Vec<_>>();
        let dir = write_export_repo(&entities);
        let ranked = search_ranked_hits_with(
            dir.path(),
            "matching",
            300,
            SearchRanking::Semantic,
            || Err("query encoder unavailable".to_string()),
            |_| panic!("candidate encoder must not run after query failure"),
        )
        .expect("search");
        assert_eq!(ranked.hits.len(), 300);
        assert_eq!(ranked.semantic_source, SemanticSource::None);
    }

    #[test]
    fn candidate_text_is_built_only_for_capped_missing_semantic_candidates() {
        use std::cell::Cell;

        let entities = (0..40)
            .map(|index| {
                let mut entity = sample_nav_entity();
                entity["node_id"] = json!(format!("n{index}"));
                entity["path"] = json!(format!("src/{index:02}.rs"));
                entity["symbol"] = json!(format!("matching_{index:02}"));
                entity["text_snippet"] = json!(format!("matching candidate {index:02}"));
                entity
            })
            .collect::<Vec<_>>();
        let batch = crate::node_rows::build_leio_row_batch(&entities).expect("batch");
        let mapped = map_batch(batch);
        let terms = query_terms("matching");
        let candidates = score_batch(
            &mapped,
            "matching",
            &terms,
            &[],
            &std::collections::HashMap::new(),
        )
        .expect("score");
        let lexical_builds = Cell::new(0);
        let lexical = rerank_candidates_with_text(
            candidates,
            40,
            SearchRanking::Lexical,
            || panic!("lexical query encoder"),
            |_| panic!("lexical candidate encoder"),
            |fields| {
                lexical_builds.set(lexical_builds.get() + 1);
                candidate_embedding_text_fields(
                    fields.snippet,
                    fields.symbol,
                    fields.kind,
                    fields.path,
                )
            },
        )
        .expect("lexical");
        assert_eq!(lexical.hits.len(), 40);
        assert_eq!(lexical_builds.get(), 0);

        let candidates = score_batch(
            &mapped,
            "matching",
            &terms,
            &[],
            &std::collections::HashMap::new(),
        )
        .expect("score");
        let semantic_builds = Cell::new(0);
        rerank_candidates_with_text(
            candidates,
            1,
            SearchRanking::Semantic,
            || Ok(vec![1.0, 0.0]),
            |texts| Ok(vec![vec![1.0, 0.0]; texts.len()]),
            |fields| {
                semantic_builds.set(semantic_builds.get() + 1);
                candidate_embedding_text_fields(
                    fields.snippet,
                    fields.symbol,
                    fields.kind,
                    fields.path,
                )
            },
        )
        .expect("semantic");
        assert_eq!(semantic_builds.get(), 32);
    }

    #[test]
    fn arrow_score_batch_rerank_uses_stored_vectors_and_embeds_only_gaps() {
        let mut semantic = sample_nav_entity();
        semantic["path"] = json!("src/a.rs");
        semantic["symbol"] = json!("matching_semantic");
        semantic["text_snippet"] = json!("matching semantic preference");
        semantic["semantic_vec"] = json!([1.0, 0.0]);
        semantic["code_vec"] = json!([0.0, 1.0]);
        let mut code = sample_nav_entity();
        code["path"] = json!("src/b.rs");
        code["symbol"] = json!("matching_code");
        code["text_snippet"] = json!("matching code fallback");
        code["semantic_vec"] = json!([0.0, 0.0]);
        code["code_vec"] = json!([1.0, 0.0]);
        let mut incompatible = sample_nav_entity();
        incompatible["path"] = json!("src/c.rs");
        incompatible["symbol"] = json!("matching_incompatible");
        incompatible["text_snippet"] = json!("matching incompatible");
        incompatible["semantic_vec"] = json!([1.0, 0.0, 0.0]);
        incompatible["code_vec"] = json!([0.0, 1.0, 0.0]);
        let mut missing = sample_nav_entity();
        missing["path"] = json!("src/d.rs");
        missing["symbol"] = json!("matching_missing");
        missing["text_snippet"] = json!("matching missing");
        missing["semantic_vec"] = json!([0.0, 0.0]);
        missing["code_vec"] = json!([0.0, 0.0]);
        let batch =
            crate::node_rows::build_leio_row_batch(&[semantic, code, incompatible, missing])
                .expect("batch");
        let mapped = map_batch(batch);
        let terms = query_terms("matching");
        let candidates = score_batch(
            &mapped,
            "matching",
            &terms,
            &[],
            &std::collections::HashMap::new(),
        )
        .expect("score");
        let ranked = rerank_candidates(
            candidates,
            8,
            SearchRanking::Semantic,
            || Ok(vec![1.0, 0.0]),
            |texts| {
                assert_eq!(texts.len(), 2);
                assert!(texts.iter().any(|text| text.contains("incompatible")));
                assert!(texts.iter().any(|text| text.contains("missing")));
                Ok(vec![vec![1.0, 0.0]; texts.len()])
            },
        )
        .expect("rerank");
        for path in ["src/a.rs", "src/b.rs", "src/c.rs", "src/d.rs"] {
            let hit = ranked
                .hits
                .iter()
                .find(|hit| hit.path == path)
                .expect("hit");
            assert!(hit.cosine.is_some_and(|cosine| cosine > 0.99));
        }
        assert_eq!(ranked.semantic_source, SemanticSource::OnDemand);
    }

    #[test]
    fn arrow_candidate_failure_preserves_lexical_and_precomputed_cosine() {
        let mut precomputed = sample_nav_entity();
        precomputed["path"] = json!("src/a.rs");
        precomputed["symbol"] = json!("matching_precomputed");
        precomputed["semantic_vec"] = json!([1.0, 0.0]);
        let mut missing = sample_nav_entity();
        missing["path"] = json!("src/b.rs");
        missing["symbol"] = json!("matching_missing");
        missing["semantic_vec"] = json!([0.0, 0.0]);
        missing["code_vec"] = json!([0.0, 0.0]);
        let batch = crate::node_rows::build_leio_row_batch(&[precomputed, missing]).expect("batch");
        let mapped = map_batch(batch);
        let terms = query_terms("matching");
        let lexical = rerank_candidates(
            score_batch(
                &mapped,
                "matching",
                &terms,
                &[],
                &std::collections::HashMap::new(),
            )
            .expect("score"),
            8,
            SearchRanking::Lexical,
            || unreachable!(),
            |_| unreachable!(),
        )
        .expect("lexical");
        let ranked = rerank_candidates(
            score_batch(
                &mapped,
                "matching",
                &terms,
                &[],
                &std::collections::HashMap::new(),
            )
            .expect("score"),
            8,
            SearchRanking::Semantic,
            || Ok(vec![1.0, 0.0]),
            |_| Err("candidate encoder unavailable".to_string()),
        )
        .expect("semantic");
        let lexical_missing = lexical
            .hits
            .iter()
            .find(|hit| hit.path == "src/b.rs")
            .expect("lexical missing");
        let ranked_missing = ranked
            .hits
            .iter()
            .find(|hit| hit.path == "src/b.rs")
            .expect("ranked missing");
        assert_eq!(ranked_missing.score, lexical_missing.score);
        assert!(ranked_missing.cosine.is_none());
        assert!(
            ranked
                .hits
                .iter()
                .find(|hit| hit.path == "src/a.rs")
                .is_some_and(|hit| hit.cosine.is_some_and(|cosine| cosine > 0.99))
        );
        assert_eq!(ranked.semantic_source, SemanticSource::Precomputed);
    }

    #[test]
    fn envelope_metadata_distinguishes_semantic_sources() {
        for (source, source_name, strategy, cosine) in [
            (SemanticSource::None, "none", "arrow_ipc_local", false),
            (
                SemanticSource::Precomputed,
                "precomputed",
                "arrow_ipc_local+precomputed_cosine",
                true,
            ),
            (
                SemanticSource::OnDemand,
                "on_demand",
                "arrow_ipc_local+on_demand_bge_m3",
                true,
            ),
        ] {
            let envelope = hits_to_envelope("matching", Vec::new(), source, Instant::now());
            let meta = envelope.meta.as_ref().expect("meta");
            assert_eq!(meta["semantic_source"].as_str(), Some(source_name));
            assert_eq!(meta["cosine"].as_bool(), Some(cosine));
            assert_eq!(
                envelope.warnings,
                vec![format!("search_strategy: {strategy}")]
            );
        }
    }

    #[test]
    fn on_demand_cosine_reorders_positive_local_candidates() {
        let mut first = sample_nav_entity();
        first["path"] = json!("src/a.rs");
        first["symbol"] = json!("matching_a");
        first["text_snippet"] = json!("matching orthogonal");
        first["semantic_vec"] = json!([0.0, 0.0]);
        let mut second = sample_nav_entity();
        second["path"] = json!("src/b.rs");
        second["symbol"] = json!("matching_b");
        second["text_snippet"] = json!("matching aligned");
        second["semantic_vec"] = json!([0.0, 0.0]);
        let dir = write_export_repo(&[first, second]);
        let ranked = search_ranked_hits_with(
            dir.path(),
            "matching",
            8,
            SearchRanking::Semantic,
            || Ok(vec![1.0, 0.0]),
            |texts| {
                Ok(texts
                    .iter()
                    .map(|text| {
                        if text.contains("aligned") {
                            vec![1.0, 0.0]
                        } else {
                            vec![0.0, 1.0]
                        }
                    })
                    .collect())
            },
        )
        .expect("search");
        assert_eq!(ranked.hits[0].path, "src/b.rs");
        assert_eq!(ranked.semantic_source, SemanticSource::OnDemand);
    }

    #[test]
    fn search_as_find_requires_min_score() {
        let dir = write_export_repo(&[sample_nav_entity()]);
        let weak = search_as_find(dir.path(), "fn", 8);
        assert!(
            weak.is_none(),
            "snippet-only hit must fall through the find short-circuit"
        );
        let strong = search_as_find(dir.path(), "run_nav", 8).expect("path/symbol hit");
        let score = strong.entities[0]
            .get("score")
            .and_then(|value| value.as_i64())
            .expect("score");
        assert!(score >= LOCAL_FIND_MIN_SCORE, "score={score}");
    }
}
