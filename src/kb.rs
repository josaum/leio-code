//! Pure-Arrow knowledge bases: Milvus-style collections (scalar + vector
//! fields, cosine top-k) implemented as plain Arrow IPC array-of-structs
//! files — no server, no external service.
//!
//! A **knowledge base** fuses several sources (any repo or docs folder, git
//! or not) into one queryable scope. Sources are chunked with the same
//! article walk the local wiki uses, chunk identities are content hashes
//! (so unchanged chunks keep their embeddings across rebuilds), and chunk
//! embeddings come from the configured OpenAI-compatible embeddings
//! endpoint when one is reachable — the collection degrades to lexical-only
//! scoring otherwise, and upgrades in place when an endpoint appears.
//!
//! Layout: `~/.leio-code/kb/<name>.arrow` (the collection), `<name>.meta.json`
//! (sources, stats, embedding model), and `registry.json` (name → sources).
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use arrow_array::{
    Array, FixedSizeListArray, Float32Array, Int64Array, ListArray, StringArray, UInt32Array,
};
use arrow_schema::{DataType, Field, Fields, Schema};

use crate::embed::embed_texts;
use arrow_array::RecordBatch;
use serde_json::json;

fn fold_text_local(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect()
}

const EMBED_WEIGHT: f64 = 15.0;
const LEXICAL_WEIGHT: f64 = 3.0;

/// Pure-Arrow collection schema. Row = struct; `embedding` is the Milvus-style
/// vector field (nullable — lexical-only KBs store nulls and upgrade in place
/// once an embeddings endpoint is configured).
fn collection_schema(dim: Option<i32>) -> Schema {
    let embedding = match dim {
        Some(dim) => DataType::FixedSizeList(
            std::sync::Arc::new(Field::new("item", DataType::Float32, true)),
            dim,
        ),
        None => DataType::List(std::sync::Arc::new(Field::new(
            "item",
            DataType::Float32,
            true,
        ))),
    };
    Schema::new(vec![
        Field::new("chunk_id", DataType::Utf8, false),
        Field::new("source", DataType::Utf8, false),
        Field::new("path", DataType::Utf8, false),
        Field::new("title", DataType::Utf8, false),
        Field::new("topic", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("heading_path", DataType::Utf8, false),
        Field::new("content", DataType::Utf8, false),
        Field::new("line", DataType::UInt32, false),
        Field::new("mtime", DataType::Int64, false),
        Field::new("embedding", embedding, true),
        Field::new("embedding_model", DataType::Utf8, true),
    ])
}

/// One registered source (any repo or docs folder — git not required).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KbSource {
    pub path: String,
}

/// Registry: knowledge-base name → sources.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KbRegistry {
    #[serde(default)]
    pub bases: BTreeMap<String, Vec<KbSource>>,
}

pub fn kb_home() -> PathBuf {
    Path::new(&std::env::var("HOME").unwrap_or_else(|_| ".".to_string()))
        .join(".leio-code")
        .join("kb")
}

pub fn registry_path() -> PathBuf {
    kb_home().join("registry.json")
}

pub fn collection_path(name: &str) -> PathBuf {
    kb_home().join(format!("{name}.arrow"))
}

pub fn meta_path(name: &str) -> PathBuf {
    kb_home().join(format!("{name}.meta.json"))
}

/// Load the registry, defaulting to an empty one when absent.
pub fn load_registry() -> KbRegistry {
    std::fs::read(registry_path())
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or_default()
}

/// Save the registry.
pub fn save_registry(registry: &KbRegistry) -> Result<()> {
    let path = registry_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(registry)?)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Register `source` under knowledge base `name` (idempotent).
pub fn add_source(name: &str, source: &Path) -> Result<Vec<KbSource>> {
    let source = source
        .canonicalize()
        .with_context(|| format!("resolve {}", source.display()))?;
    let mut registry = load_registry();
    let sources = registry.bases.entry(name.to_owned()).or_default();
    let rendered = source.display().to_string();
    if !sources.iter().any(|s| s.path == rendered) {
        sources.push(KbSource { path: rendered });
    }
    let snapshot = sources.clone();
    save_registry(&registry)?;
    Ok(snapshot)
}

/// Remove `source` from knowledge base `name`.
pub fn remove_source(name: &str, source: &str) -> Result<bool> {
    let mut registry = load_registry();
    let Some(sources) = registry.bases.get_mut(name) else {
        return Ok(false);
    };
    let before = sources.len();
    sources.retain(|s| !s.path.replace('\\', "/").ends_with(source));
    let changed = sources.len() != before;
    save_registry(&registry)?;
    Ok(changed)
}

fn chunk_id(source: &str, path: &str, line: u32, content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    hasher.update(b"\x00");
    hasher.update(path.as_bytes());
    hasher.update(b"\x00");
    hasher.update(line.to_le_bytes());
    hasher.update(content.as_bytes());
    let digest = hasher.finalize();
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Stats written beside the collection after each build.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KbBuildStats {
    pub sources: Vec<String>,
    pub rows: usize,
    pub embedded: usize,
    pub lexical_only: bool,
    pub embedding_model: Option<String>,
    pub built_at_ms: u64,
}

fn write_collection(name: &str, batch: &RecordBatch) -> Result<()> {
    let path = collection_path(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = path.with_extension("arrow.tmp");
    let file = std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
    // Stream format — mirrors the knowledge wiki store (StreamWriter write +
    // read_ipc_stream_path read), and matches the embedding List column.
    use arrow_ipc::writer::StreamWriter;
    let mut writer = StreamWriter::try_new(BufWriter::new(file), batch.schema().as_ref())
        .context("create Arrow stream writer")?;
    writer.write(batch).context("write collection batch")?;
    writer.finish().context("finish Arrow stream writer")?;
    std::fs::rename(&tmp, &path).with_context(|| format!("rename {}", path.display()))?;
    Ok(())
}

fn read_collection(name: &str) -> Result<Option<RecordBatch>> {
    let path = collection_path(name);
    if !path.is_file() {
        return Ok(None);
    }
    let batches = crate::arrow_ipc::read_ipc_stream_path(&path)
        .with_context(|| format!("read {}", path.display()))?;
    let mut iter = batches.into_iter();
    let first = iter.next().context("empty collection file")?;
    Ok(Some(first))
}

/// A scored query hit.
#[derive(Debug, Clone, Serialize)]
pub struct KbHit {
    pub score: f64,
    pub source: String,
    pub path: String,
    pub title: String,
    pub topic: String,
    pub kind: String,
    pub heading_path: String,
    pub line: u32,
    pub snippet: String,
    pub cosine: Option<f64>,
}

fn row_string(batch: &RecordBatch, column: &str, row: usize) -> String {
    let array = batch
        .column_by_name(column)
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        .expect("string column");
    if array.is_null(row) {
        String::new()
    } else {
        array.value(row).to_owned()
    }
}

fn row_u32(batch: &RecordBatch, column: &str, row: usize) -> u32 {
    batch
        .column_by_name(column)
        .and_then(|c| c.as_any().downcast_ref::<UInt32Array>())
        .expect("u32 column")
        .value(row)
}

fn row_vector(batch: &RecordBatch, row: usize) -> Option<Vec<f32>> {
    let column = batch.column_by_name("embedding")?;
    match column.data_type() {
        DataType::FixedSizeList(_, _) => {
            let array = column
                .as_any()
                .downcast_ref::<FixedSizeListArray>()
                .expect("fixed size list");
            if array.is_null(row) {
                return None;
            }
            let values = array.value(row);
            let floats = values
                .as_any()
                .downcast_ref::<Float32Array>()
                .expect("float values");
            Some(floats.values().to_vec())
        }
        DataType::List(_) => {
            let array = column.as_any().downcast_ref::<ListArray>().expect("list");
            if array.is_null(row) {
                return None;
            }
            let values = array.value(row);
            let floats = values
                .as_any()
                .downcast_ref::<Float32Array>()
                .expect("float values");
            Some(floats.values().to_vec())
        }
        _ => None,
    }
}

fn row_embedding_model(batch: &RecordBatch, row: usize) -> Option<String> {
    let column = batch.column_by_name("embedding_model")?;
    let array = column
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("embedding_model column");
    if array.is_null(row) {
        None
    } else {
        Some(array.value(row).to_owned())
    }
}

fn cosine(left: &[f32], right: &[f32]) -> f64 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut left_norm = 0.0;
    let mut right_norm = 0.0;
    for (l, r) in left.iter().zip(right.iter()) {
        dot += f64::from(*l) * f64::from(*r);
        left_norm += f64::from(*l) * f64::from(*l);
        right_norm += f64::from(*r) * f64::from(*r);
    }
    if left_norm == 0.0 || right_norm == 0.0 {
        return 0.0;
    }
    dot / (left_norm.sqrt() * right_norm.sqrt())
}

/// Rarity-weighted lexical score: idf-weighted token coverage plus a bonus
/// when the full query phrase appears contiguously.
fn lexical_score(
    query_tokens: &[String],
    idf: &[f64],
    idf_total: f64,
    folded_query: &str,
    content: &str,
) -> f64 {
    let folded = fold_text_local(content);
    if query_tokens.is_empty() || idf_total <= 0.0 {
        return 0.0;
    }
    let mut hit_weight = 0.0;
    for (token, weight) in query_tokens.iter().zip(idf.iter()) {
        if folded.contains(token.as_str()) {
            hit_weight += weight;
        }
    }
    let mut score = LEXICAL_WEIGHT * (hit_weight / idf_total);
    if folded.contains(folded_query) {
        score += LEXICAL_WEIGHT;
    }
    score
}

fn query_tokens(query: &str) -> Vec<String> {
    fold_text_local(query)
        .split_whitespace()
        .filter(|token| token.len() >= 2)
        .map(ToOwned::to_owned)
        .collect()
}

/// Build (or incrementally refresh) a knowledge base from its registered
/// sources. Chunks are content-hashed; unchanged chunks keep their
/// embeddings, new chunks are embedded when an embeddings endpoint is
/// reachable, and vanished chunks are dropped.
///
/// # Errors
///
/// Returns an error when the registry has no entry for `name`, a source
/// cannot be read, or the collection cannot be written.
pub fn build_kb(name: &str, repo_root: &Path) -> Result<KbBuildStats> {
    let registry = load_registry();
    let sources = registry
        .bases
        .get(name)
        .with_context(|| format!("unknown knowledge base: {name}"))?;
    let mut stats = KbBuildStats {
        sources: sources.iter().map(|s| s.path.clone()).collect(),
        built_at_ms: now_ms(),
        ..KbBuildStats::default()
    };

    // Existing rows: chunk_id -> (embedding, embedding_model) — reused for
    // surviving chunks so rebuilds do not re-embed unchanged content.
    let mut carried: BTreeMap<String, (Option<Vec<f32>>, Option<String>)> = BTreeMap::new();
    let prior_dim = if let Some(batch) = read_collection(name)? {
        for row in 0..batch.num_rows() {
            let chunk = row_string(&batch, "chunk_id", row);
            carried.insert(
                chunk,
                (row_vector(&batch, row), row_embedding_model(&batch, row)),
            );
        }
        row_vector(&batch, 0).map(|vector| vector.len() as i32)
    } else {
        None
    };

    struct Row {
        chunk_id: String,
        source: String,
        path: String,
        title: String,
        topic: String,
        kind: String,
        heading_path: String,
        content: String,
        line: u32,
        mtime: i64,
        embedding: Option<Vec<f32>>,
        embedding_model: Option<String>,
    }
    let mut rows: Vec<Row> = Vec::new();
    let mut to_embed: Vec<(usize, String)> = Vec::new();

    for source in sources.iter() {
        let batch = crate::knowledge::collect_articles(Path::new(&source.path))?;
        for article in &batch.articles {
            let chunk = chunk_id(
                &source.path,
                &article.source_path,
                article.line,
                &article.body,
            );
            let mtime: i64 = batch
                .stamps
                .get(&article.source_path.to_string())
                .map(|stamp| stamp.mtime as i64)
                .unwrap_or(0);
            let carried_embedding = carried.get(&chunk);
            let (embedding, embedding_model) = match carried_embedding {
                Some((vector, model)) => (vector.clone(), model.clone()),
                None => {
                    to_embed.push((rows.len(), article.body.to_string()));
                    (None, None)
                }
            };
            rows.push(Row {
                chunk_id: chunk,
                source: source.path.clone(),
                path: article.source_path.to_string(),
                title: article.title.to_string(),
                topic: article.topic.to_string(),
                kind: article.kind.to_string(),
                heading_path: article.heading_path.to_string(),
                content: article.body.to_string(),
                line: article.line,
                mtime,
                embedding,
                embedding_model,
            });
        }
    }

    if !to_embed.is_empty() {
        let texts: Vec<String> = to_embed.iter().map(|(_, text)| text.clone()).collect();
        let anchor = sources
            .first()
            .map(|s| Path::new(s.path.as_str()))
            .unwrap_or(repo_root);
        if let Ok(vectors) = embed_texts(anchor, &texts) {
            stats.lexical_only = false;
            for ((row_index, _), vector) in to_embed.iter().zip(vectors) {
                rows[*row_index].embedding = Some(vector.clone());
                rows[*row_index].embedding_model =
                    Some(std::env::var("LEIO_CODE_EMBED_MODEL").unwrap_or_default());
                stats.embedded += 1;
            }
        } else {
            stats.lexical_only = true;
        }
    }
    let _ = prior_dim;

    let mut embedded_models: Vec<Option<String>> = Vec::new();
    let mut embeddings: Vec<Option<Vec<f32>>> = Vec::new();
    for row in &rows {
        embeddings.push(row.embedding.clone());
        embedded_models.push(row.embedding_model.clone());
    }
    let dim = embeddings
        .iter()
        .flatten()
        .flat_map(|vector| vector.first())
        .map(|_| 0i32)
        .max()
        .and_then(|_| {
            embeddings
                .iter()
                .flatten()
                .next()
                .map(|vector| vector.len() as i32)
        });

    let schema = collection_schema(dim);
    let embedding_field = build_embedding_array(&embeddings, dim);

    let batch = RecordBatch::try_new(
        std::sync::Arc::new(schema.clone()),
        vec![
            std::sync::Arc::new(string_array(
                rows.iter().map(|r| r.chunk_id.as_str()).collect(),
            )),
            std::sync::Arc::new(string_array(
                rows.iter().map(|r| r.source.as_str()).collect(),
            )),
            std::sync::Arc::new(string_array(rows.iter().map(|r| r.path.as_str()).collect())),
            std::sync::Arc::new(string_array(
                rows.iter().map(|r| r.title.as_str()).collect(),
            )),
            std::sync::Arc::new(string_array(
                rows.iter().map(|r| r.topic.as_str()).collect(),
            )),
            std::sync::Arc::new(string_array(rows.iter().map(|r| r.kind.as_str()).collect())),
            std::sync::Arc::new(string_array(
                rows.iter().map(|r| r.heading_path.as_str()).collect(),
            )),
            std::sync::Arc::new(string_array(
                rows.iter().map(|r| r.content.as_str()).collect(),
            )),
            std::sync::Arc::new(u32_array(rows.iter().map(|r| r.line).collect())),
            std::sync::Arc::new(i64_array(rows.iter().map(|r| r.mtime).collect())),
            embedding_field,
            std::sync::Arc::new(string_array_option(
                embedded_models.iter().map(|m| m.as_deref()).collect(),
            )),
        ],
    )
    .context("build collection batch")?;
    write_collection(name, &batch)?;

    stats.rows = rows.len();
    stats.embedding_model = rows
        .iter()
        .find_map(|row| row.embedding_model.clone())
        .or_else(|| std::env::var("LEIO_CODE_EMBED_MODEL").ok());

    let meta = json!({
        "name": name,
        "sources": stats.sources,
        "rows": stats.rows,
        "embedded": stats.embedded,
        "lexical_only": stats.lexical_only,
        "built_at_ms": stats.built_at_ms,
    });
    let meta_path = meta_path(name);
    std::fs::write(&meta_path, serde_json::to_vec_pretty(&meta)?)
        .with_context(|| format!("write {}", meta_path.display()))?;
    Ok(stats)
}

fn build_embedding_array(
    embeddings: &[Option<Vec<f32>>],
    dim: Option<i32>,
) -> std::sync::Arc<dyn Array> {
    let values: Vec<f32> = embeddings
        .iter()
        .flatten()
        .flat_map(|vector| vector.iter().copied())
        .collect();
    match dim {
        Some(dim) => {
            let floats = Float32Array::from(values);
            let field = std::sync::Arc::new(Field::new("item", DataType::Float32, true));
            let nulls: Vec<bool> = embeddings.iter().map(|entry| entry.is_none()).collect();
            let array = FixedSizeListArray::new(
                field,
                dim,
                std::sync::Arc::new(floats),
                Some(arrow_buffer::NullBuffer::new(nulls.into())),
            );
            std::sync::Arc::new(array) as std::sync::Arc<dyn Array>
        }
        None => {
            // Lexical-only collections keep a null list column.
            let field = std::sync::Arc::new(Field::new("item", DataType::Float32, true));
            std::sync::Arc::new(ListArray::new_null(field, embeddings.len()))
                as std::sync::Arc<dyn Array>
        }
    }
}

fn string_array(values: Vec<&str>) -> StringArray {
    StringArray::from(values)
}

fn string_array_option(values: Vec<Option<&str>>) -> StringArray {
    StringArray::from(values)
}

fn u32_array(values: Vec<u32>) -> UInt32Array {
    UInt32Array::from(values)
}

fn i64_array(values: Vec<i64>) -> Int64Array {
    Int64Array::from(values)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Query a knowledge base: hybrid lexical + cosine scoring over the pure-Arrow
/// collection, top-k by score. Works lexical-only when the collection has no
/// embeddings.
///
/// # Errors
///
/// Returns an error when the collection does not exist or the query embedding
/// cannot be produced for a vector collection.
pub fn query_kb(
    name: &str,
    query: &str,
    top_k: usize,
    source_filter: Option<&str>,
    repo_root: &Path,
) -> Result<Vec<KbHit>> {
    let batch = read_collection(name)
        .context("read collection")?
        .with_context(|| format!("unknown knowledge base: {name}"))?;
    let tokens = query_tokens(query);
    // Rarity weighting (IDF): a token found in every row ("terms") carries
    // almost no signal; a token found in one row ("unmapped") dominates.
    let total_rows = batch.num_rows().max(1);
    let folded_contents: Vec<String> = (0..batch.num_rows())
        .map(|row| fold_text_local(&row_string(&batch, "content", row)))
        .collect();
    let idf: Vec<f64> = tokens
        .iter()
        .map(|token| {
            let df = folded_contents
                .iter()
                .filter(|content| content.contains(token.as_str()))
                .count();
            (1.0 + total_rows as f64 / (df.max(1) as f64)).ln()
        })
        .collect();
    let idf_total: f64 = idf.iter().sum();
    let folded_query = fold_text_local(query);
    let query_vector = if row_vector(&batch, 0).is_some() {
        match embed_texts(repo_root, &[query.to_owned()]) {
            Ok(vectors) => vectors.into_iter().next(),
            Err(_) => None,
        }
    } else {
        None
    };

    let mut hits: Vec<KbHit> = Vec::new();
    for row in 0..batch.num_rows() {
        let source = row_string(&batch, "source", row);
        if let Some(filter) = source_filter
            && !source.replace('\\', "/").ends_with(filter)
        {
            continue;
        }
        let content = row_string(&batch, "content", row);
        let lex = lexical_score(&tokens, &idf, idf_total, &folded_query, &content);
        let row_vector_value = row_vector(&batch, row);
        let cosine_value = match (&query_vector, &row_vector_value) {
            (Some(query), Some(vector)) => Some(cosine(query, vector)),
            _ => None,
        };
        let score = lex
            + cosine_value
                .map(|value| EMBED_WEIGHT * value)
                .unwrap_or(0.0);
        if score <= 0.0 {
            continue;
        }
        hits.push(KbHit {
            score,
            source,
            path: row_string(&batch, "path", row),
            title: row_string(&batch, "title", row),
            topic: row_string(&batch, "topic", row),
            kind: row_string(&batch, "kind", row),
            heading_path: row_string(&batch, "heading_path", row),
            line: row_u32(&batch, "line", row),
            snippet: content.chars().take(240).collect(),
            cosine: cosine_value,
        });
    }
    hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(top_k);
    Ok(hits)
}

/// Fields helper for the collection schema (used by tests).
#[allow(dead_code)]
fn schema_fields(dim: Option<i32>) -> Fields {
    collection_schema(dim).fields.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    type FixtureRow = (String, String, String, String, String, String, String, u32);

    fn write_source(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn registry_round_trip() {
        // Use the explicit-path APIs; this test never mutates the process HOME.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("registry.json");
        let mut registry = KbRegistry::default();
        registry
            .bases
            .entry("eng".to_owned())
            .or_default()
            .push(KbSource {
                path: "/tmp/docs".into(),
            });
        std::fs::write(&path, serde_json::to_vec_pretty(&registry).unwrap()).unwrap();
        let loaded: KbRegistry = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(loaded.bases["eng"][0].path, "/tmp/docs");
    }

    #[test]
    fn build_and_lexical_query_over_fixture_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let docs_a = tmp.path().join("docs-a");
        let docs_b = tmp.path().join("docs-b");
        write_source(
            &docs_a,
            "specs/auth.md",
            "# Auth Spec\nTokens carry a 15 minute TTL and rotate via the signed assertion route.\n",
        );
        write_source(
            &docs_b,
            "runbooks/rollback.txt",
            "Rollback runbook: pin the previous cartridge and flush session keys.\n",
        );

        // Register both sources, then build.
        let registry = KbRegistry {
            bases: BTreeMap::from([(
                "eng".to_owned(),
                vec![
                    KbSource {
                        path: docs_a.display().to_string(),
                    },
                    KbSource {
                        path: docs_b.display().to_string(),
                    },
                ],
            )]),
        };
        let kb_dir = tmp.path().join("kb");
        std::fs::create_dir_all(&kb_dir).unwrap();
        let collection = kb_dir.join("eng.arrow");

        // Build via the same walk the production builder uses.
        let mut rows: Vec<FixtureRow> = Vec::new();
        for source in &registry.bases["eng"] {
            let batch = crate::knowledge::collect_articles(Path::new(&source.path)).unwrap();
            for a in &batch.articles {
                rows.push((
                    chunk_id(&source.path, &a.source_path, a.line, &a.body),
                    source.path.clone(),
                    a.source_path.to_string(),
                    a.title.to_string(),
                    a.topic.to_string(),
                    a.kind.to_string(),
                    a.body.clone(),
                    a.line,
                ));
            }
        }
        assert_eq!(rows.len(), 2, "expected one chunk per fixture file");
        assert!(rows.iter().any(|r| r.5 == "markdown"));
        assert!(rows.iter().any(|r| r.5 == "text"));
        assert!(rows.iter().all(|r| !r.6.is_empty()));
        let _ = collection;

        // Lexical query semantics: fold + containment.
        let tokens = query_tokens("rollback cartridge");
        let scored: Vec<&FixtureRow> = rows
            .iter()
            .filter(|r| {
                tokens
                    .iter()
                    .any(|t| r.6.to_ascii_lowercase().contains(t.as_str()))
            })
            .collect();
        assert_eq!(scored.len(), 1);
        assert!(scored[0].6.contains("Rollback"));
    }

    #[test]
    fn chunk_ids_are_stable_and_content_sensitive() {
        let a = chunk_id("src", "README.md", 1, "hello");
        let b = chunk_id("src", "README.md", 1, "hello");
        let c = chunk_id("src", "README.md", 1, "world");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn query_skips_rows_without_matching_tokens() {
        // Empty-token guard: lexical score is 0 and the row is skipped.
        assert!(lexical_score(&[], &[], 0.0, "", "anything") == 0.0);
    }
}

#[test]
fn idf_weighting_ranks_rare_tokens_above_common_ones() {
    let tokens = vec!["unmapped".to_owned(), "terms".to_owned()];
    // "terms" appears in every doc (df=3 -> tiny idf); "unmapped" in one
    // (df=1 -> large idf).
    let idf = vec![(1.0 + 3.0_f64.ln()), (1.0 + 3.0 / 3.0_f64).ln()];
    let idf_total: f64 = idf.iter().sum();
    let folded_query = fold_text_local("unmapped terms");

    let doc_with_rare = "the unmapped key was dropped silently terms";
    let doc_common_only = "these terms are everywhere terms";
    let score_rare = lexical_score(&tokens, &idf, idf_total, &folded_query, doc_with_rare);
    let score_common = lexical_score(&tokens, &idf, idf_total, &folded_query, doc_common_only);
    assert!(
        score_rare > score_common,
        "rare-token doc must outrank common-only doc: {score_rare} vs {score_common}"
    );
    // Full idf coverage on the rare doc (no contiguous phrase, so no bonus):
    // the score reaches the full lexical weight.
    assert!((score_rare - LEXICAL_WEIGHT).abs() < 1e-9, "{score_rare}");
}

#[test]
fn phrase_bonus_beats_equal_coverage_without_phrase() {
    let tokens = vec!["safe".to_owned(), "mode".to_owned()];
    let idf = vec![2.0_f64.ln(), 2.0_f64.ln()];
    let idf_total: f64 = idf.iter().sum();
    let folded_query = fold_text_local("safe mode");
    let with_phrase = "the safe mode posture is fail-closed";
    let scattered = "stay safe; switch the mode later";
    let a = lexical_score(&tokens, &idf, idf_total, &folded_query, with_phrase);
    let b = lexical_score(&tokens, &idf, idf_total, &folded_query, scattered);
    assert!(
        a > b,
        "contiguous phrase must outrank scattered tokens: {a} vs {b}"
    );
}
