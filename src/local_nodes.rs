//! Local FCA + node store over `.leio-code/exports/arrow-nodes-v1/`.
//!
//! Search mmaps `nodes.search` first and falls back to the Arrow IPC stream
//! when the sidecar is missing or unreadable. Ranking is lexical + FCA
//! relation overlap, plus cosine against `semantic_vec` / `code_vec` when
//! `LEIO_CODE_EMBED_URL` is set.
//!
//! The store is process-wide so MCP / repeated in-process calls skip rebuild.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime};

use anyhow::{Context, Result, bail};
use arrow_array::{Array, Float32Array, ListArray, StringArray};
use rayon::prelude::*;
use serde_json::json;

use crate::arrow_ipc::read_ipc_stream_path;
use crate::embed::embed_query;
use crate::export::{
    default_arrow_nodes_output_dir, default_arrow_nodes_rows_path, default_search_sidecar_path,
};
use crate::model::{EvidenceItem, QueryEnvelope};
use crate::node_search::{self, PackedIndex};

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "is", "it", "of", "on",
    "or", "the", "to", "with",
];

/// Cosine at or above this keeps a row even with no lexical overlap.
///
/// BGE-M3 in-domain neighbors for a multi-word task sit ~0.45–0.75;
/// 0.42 keeps related symbols without flooding exact-match rankings.
const COSINE_KEEP: f32 = 0.42;

/// Scale cosine `[-1, 1]` onto the same integer score ladder as lexical
/// hits (exact symbol = 100). 0.75 cosine ≈ +60, so a strong semantic
/// neighbor can outrank a weak path substring (+12) but not an exact
/// symbol match.
const COSINE_SCORE_SCALE: f32 = 80.0;

/// `find` only short-circuits to local Arrow when the top hit is at least a
/// path-substring match. Weaker neighbors fall through to the symbol index.
const LOCAL_FIND_MIN_SCORE: i64 = 70;

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
    let store = Arc::new(NodeStore { key, body });
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
    search_ranked_hits(repo_root, needle, limit, SearchRanking::Semantic, || {
        embed_query(repo_root, needle).ok()
    })
}

/// Deterministic lexical/FCA ranking for cursor navigation and continuation.
/// Encoder availability cannot change the order between navigation pages.
pub fn search_navigation_hits(
    repo_root: &Path,
    needle: &str,
    limit: usize,
) -> Result<Vec<NodeHit>> {
    search_ranked_hits(repo_root, needle, limit, SearchRanking::Lexical, || {
        embed_query(repo_root, needle).ok()
    })
}

#[derive(Clone, Copy)]
enum SearchRanking {
    Semantic,
    Lexical,
}

fn search_ranked_hits(
    repo_root: &Path,
    needle: &str,
    limit: usize,
    ranking: SearchRanking,
    encoder: impl FnOnce() -> Option<Vec<f32>>,
) -> Result<Vec<NodeHit>> {
    let path = nodes_path(repo_root);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let store = load_store(&path)?;
    let terms = query_terms(needle);
    let relation_hints = relation_hints(&terms, needle);
    let needle_lc = needle.to_ascii_lowercase();
    let query_vec = match ranking {
        SearchRanking::Semantic => encoder(),
        SearchRanking::Lexical => None,
    };
    let query_norm = query_vec.as_deref().map(l2_norm);
    let mut hits: Vec<NodeHit> = match &store.body {
        StoreBody::Packed(packed) => score_packed(
            packed,
            &needle_lc,
            &terms,
            &relation_hints,
            query_vec.as_deref(),
            query_norm,
        ),
        StoreBody::Arrow(batches) => batches
            .par_iter()
            .flat_map(|mapped| {
                score_batch(
                    mapped,
                    &needle_lc,
                    &terms,
                    &relation_hints,
                    query_vec.as_deref(),
                    query_norm,
                )
                .unwrap_or_default()
            })
            .collect(),
    };
    hits.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.symbol.cmp(&right.symbol))
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.snippet.cmp(&right.snippet))
            .then_with(|| left.matched_relations.cmp(&right.matched_relations))
    });
    let mut seen = std::collections::HashSet::new();
    hits.retain(|hit| seen.insert((hit.path.clone(), hit.symbol.clone())));
    hits.truncate(limit.max(1));
    Ok(hits)
}

/// Adaptive-shaped envelope over the local Arrow file. `None` if the file is absent.
pub fn search_adaptive(repo_root: &Path, needle: &str, limit: usize) -> Option<QueryEnvelope> {
    if !available(repo_root) {
        return None;
    }
    let started = Instant::now();
    let hits = match search_hits(repo_root, needle, limit) {
        Ok(hits) => hits,
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
    Some(hits_to_envelope(needle, hits, started))
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

fn hits_to_envelope(needle: &str, hits: Vec<NodeHit>, started: Instant) -> QueryEnvelope {
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
        warnings: vec![if hits.iter().any(|hit| hit.cosine.is_some()) {
            "search_strategy: arrow_ipc_local+bge_m3_cosine".to_string()
        } else {
            "search_strategy: arrow_ipc_local".to_string()
        }],
        meta: Some(json!({
            "transport": "arrow_ipc",
            "store": "local_nodes",
            "path": "exports/arrow-nodes-v1/nodes.arrow",
            "cosine": hits.iter().any(|hit| hit.cosine.is_some()),
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn score_packed(
    packed: &PackedIndex,
    needle_lc: &str,
    terms: &[String],
    relation_hints: &[String],
    query_vec: Option<&[f32]>,
    query_norm: Option<f32>,
) -> Vec<NodeHit> {
    (0..packed.n())
        .into_par_iter()
        .filter_map(|row| {
            let path = packed.path(row);
            let kind = packed.kind(row);
            let symbol = packed.symbol(row);
            let snippet = packed.snippet(row);
            let mut score = 0i64;
            if !needle_lc.is_empty() && ascii_eq_ignore_case(symbol, needle_lc) {
                score += 100;
            } else if !needle_lc.is_empty() && ascii_contains_ignore_case(symbol, needle_lc) {
                score += 80;
            }
            if !needle_lc.is_empty() && ascii_contains_ignore_case(path, needle_lc) {
                score += 70;
            }
            for term in terms {
                if ascii_contains_ignore_case(path, term) {
                    score += 12;
                }
                if ascii_contains_ignore_case(symbol, term) {
                    score += 14;
                }
                if ascii_contains_ignore_case(snippet, term) {
                    score += 8;
                }
            }
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
            let cosine = query_vec.and_then(|query| {
                let qn = query_norm.unwrap_or(0.0);
                packed
                    .vector(row)
                    .map(|row_vec| cosine_pre_normed(query, qn, row_vec, packed.norm(row)))
            });
            if let Some(value) = cosine {
                if value >= COSINE_KEEP {
                    score += (value * COSINE_SCORE_SCALE).round() as i64;
                } else if score > 0 {
                    score += (value * (COSINE_SCORE_SCALE / 4.0)).round() as i64;
                }
            }
            if score <= 0 {
                return None;
            }
            Some(NodeHit {
                path: path.to_string(),
                kind: kind.to_string(),
                symbol: symbol.to_string(),
                score,
                matched_relations: matched,
                snippet: snippet.chars().take(240).collect(),
                cosine,
            })
        })
        .collect()
}

fn score_batch(
    mapped: &MappedBatch,
    needle_lc: &str,
    terms: &[String],
    relation_hints: &[String],
    query_vec: Option<&[f32]>,
    query_norm: Option<f32>,
) -> Result<Vec<NodeHit>> {
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

    let mut hits = Vec::new();
    for row in 0..batch.num_rows() {
        let path = paths.value(row);
        let kind = kinds.value(row);
        let symbol = symbols.value(row);
        let snippet = snippets.value(row);

        let mut score = 0i64;
        if !needle_lc.is_empty() && ascii_eq_ignore_case(symbol, needle_lc) {
            score += 100;
        } else if !needle_lc.is_empty() && ascii_contains_ignore_case(symbol, needle_lc) {
            score += 80;
        }
        if !needle_lc.is_empty() && ascii_contains_ignore_case(path, needle_lc) {
            score += 70;
        }
        for term in terms {
            if ascii_contains_ignore_case(path, term) {
                score += 12;
            }
            if ascii_contains_ignore_case(symbol, term) {
                score += 14;
            }
            if ascii_contains_ignore_case(snippet, term) {
                score += 8;
            }
        }
        let mut matched = Vec::new();
        for hint in relation_hints {
            if relation_matches(relations, row, hint) {
                score += 18;
                matched.push(hint.clone());
            }
        }
        let cosine = query_vec.and_then(|query| {
            let qn = query_norm.unwrap_or(0.0);
            mapped
                .semantic_norms
                .get(row)
                .copied()
                .filter(|norm| *norm > f32::EPSILON)
                .and_then(|norm| {
                    row_vector_slice(semantic_vecs?, row)
                        .map(|row_vec| cosine_pre_normed(query, qn, row_vec, norm))
                })
                .or_else(|| {
                    mapped
                        .code_norms
                        .get(row)
                        .copied()
                        .filter(|norm| *norm > f32::EPSILON)
                        .and_then(|norm| {
                            row_vector_slice(code_vecs?, row)
                                .map(|row_vec| cosine_pre_normed(query, qn, row_vec, norm))
                        })
                })
        });
        if let Some(value) = cosine {
            if value >= COSINE_KEEP {
                score += (value * COSINE_SCORE_SCALE).round() as i64;
            } else if score > 0 {
                score += (value * (COSINE_SCORE_SCALE / 4.0)).round() as i64;
            }
        }
        if score <= 0 {
            continue;
        }
        hits.push(NodeHit {
            path: path.to_string(),
            kind: kind.to_string(),
            symbol: symbol.to_string(),
            score,
            matched_relations: matched,
            snippet: snippet.chars().take(240).collect(),
            cosine,
        });
    }
    Ok(hits)
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
    let mut terms = Vec::new();
    for raw in needle.split(|ch: char| !ch.is_ascii_alphanumeric()) {
        let slug = raw.to_ascii_lowercase();
        if slug.len() < 2 || STOPWORDS.contains(&slug.as_str()) {
            continue;
        }
        if !terms.iter().any(|seen| seen == &slug) {
            terms.push(slug);
        }
    }
    terms
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
    fn score_packed_applies_cosine_to_semantic_neighbor() {
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
        let query = [0.6_f32, 0.8, 0.0, 0.0];
        let hits = score_packed(
            &packed,
            "unrelated-needle",
            &[],
            &[],
            Some(&query),
            Some(l2_norm(&query)),
        );
        assert_eq!(hits.len(), 1);
        assert!(hits[0].cosine.unwrap() > 0.99);
        assert!(hits[0].score > 0);
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
        assert!(score_batch(&mapped, "x", &[], &[], None, None).is_err());
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
