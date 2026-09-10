//! Optional BGE-M3 embedding of LEIO node rows for the textual regime.
//!
//! Produces real semantic embeddings via a remote GPU encoder when
//! `LEIO_CODE_EMBED_URL` is set (direct TEI `/embed`, Gemini
//! `generativelanguage.googleapis.com`, or LiteLLM OpenAI `/v1/embeddings`,
//! 1024-d). Ingest may also read `[embed] url`. Query embedding uses env only.
//!
//! Three distinct text *views* are embedded per node so the vector arms are
//! meaningful instead of three identical probes:
//! - `code_vec`     ← identifier/structural view: `"{symbol} {kind}"` + path tail
//! - `semantic_vec` ← natural-language view: `text_snippet` (the previously
//!   wasted rich field), falling back to the structural view when empty
//! - `ontology_vec` ← ontology/context view: `"{kind} {relations} {path} {lang}"`
//!
//! All three views are embedded in *batched* calls (one embed request per view,
//! not per node) to amortize round-trips.
//!
//! Graceful degradation: if the encoder is unreachable or returns an error, the
//! node rows keep their placeholder vectors and a warning is surfaced — ingest
//! is never aborted, and the query path falls through to its cheaper stages.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use ureq::Agent;

use crate::config::{embed_model, embed_url};

/// Canonical encoder model id stamped on embedded rows.
pub(crate) const EMBED_MODEL: &str = "BAAI/bge-m3";

/// Dense vector dimension of [`EMBED_MODEL`].
pub(crate) const EMBED_DIM: usize = 1024;

/// Maximum characters of any single view text sent to the encoder.
///
/// BGE-M3 truncates internally; we cap here to keep request payloads bounded.
const MAX_VIEW_CHARS: usize = 2000;

/// TEI `--max-client-batch-size` default we deploy (64). Smaller than a full
/// view batch so a large export is split instead of 413'd.
const REMOTE_EMBED_BATCH: usize = 64;

/// GPU TEI / LiteLLM can take tens of seconds on a 64×2k-char batch.
///
/// Too low and export falls back to zeros on a healthy encoder.
const REMOTE_EMBED_TIMEOUT: Duration = Duration::from_secs(120);

/// Build the identifier/structural text view for a node entity.
///
/// `"{symbol} {target} {kind} {path-tail}"` — the load-bearing identifier
/// tokens. When the symbol is empty (files, cartridges with no label), the
/// `target` and a short path tail keep the view non-degenerate.
fn build_code_view(entity: &Value) -> String {
    let symbol = str_field(entity, "symbol");
    let kind = str_field(entity, "kind");
    let target = str_field(entity, "target");
    let path = str_field(entity, "path");
    let path_tail = path.rsplit('/').next().unwrap_or(path);
    let mut parts: Vec<&str> = Vec::new();
    for part in [symbol, target, kind, path_tail] {
        if !part.is_empty() {
            parts.push(part);
        }
    }
    cap(parts.join(" "))
}

/// Builds snippet-first candidate text for semantic embedding.
///
/// Empty snippets fall back to nonempty symbol, kind, and path-tail fields.
/// The result is capped at [`MAX_VIEW_CHARS`] characters.
pub(crate) fn candidate_embedding_text(entity: &Value) -> String {
    candidate_embedding_text_fields(
        str_field(entity, "text_snippet"),
        str_field(entity, "symbol"),
        str_field(entity, "kind"),
        str_field(entity, "path"),
    )
}

/// Builds candidate text directly from node fields.
pub(crate) fn candidate_embedding_text_fields(
    snippet: &str,
    symbol: &str,
    kind: &str,
    path: &str,
) -> String {
    let snippet = snippet.trim();
    if !snippet.is_empty() {
        return cap(snippet.to_string());
    }

    let symbol = symbol.trim();
    let kind = kind.trim();
    let path = path.trim();
    let path_tail = path.rsplit('/').next().unwrap_or(path);
    cap([symbol, kind, path_tail]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" "))
}

/// Build the ontology/context view: `"{kind} {relations…} {path} {lang}"`.
fn build_ontology_view(entity: &Value) -> String {
    let kind = str_field(entity, "kind");
    let path = str_field(entity, "path");
    let lang = str_field(entity, "lang");
    let relations = entity
        .get("relations")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    let mut text = String::new();
    for part in [kind, relations.as_str(), path, lang] {
        if !part.is_empty() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(part);
        }
    }
    cap(text)
}

fn str_field<'a>(entity: &'a Value, key: &str) -> &'a str {
    entity.get(key).and_then(Value::as_str).unwrap_or("")
}

fn cap(mut text: String) -> String {
    if text.chars().count() > MAX_VIEW_CHARS {
        text = text.chars().take(MAX_VIEW_CHARS).collect();
    }
    text
}

/// Embed all three text views for `entities` and overwrite their float vectors.
///
/// Overwrites `code_vec` / `semantic_vec` / `ontology_vec` in place via the
/// configured remote encoder and stamps `embed_model` / `embed_dim` on every
/// embedded row. Returns the number of rows whose vectors were replaced.
///
/// # Errors
/// Returns `Err(message)` when no encoder is configured, the encoder could not
/// be reached, or the response count did not match the input. On `Err`, callers
/// must keep the placeholder vectors and surface a warning rather than abort —
/// embeddings are best-effort.
pub(crate) fn embed_node_entities(
    repo_root: &Path,
    entities: &mut [Value],
) -> Result<usize, String> {
    if entities.is_empty() {
        return Ok(0);
    }

    let base = embed_url(repo_root)
        .ok_or_else(|| "no [embed] url / LEIO_CODE_EMBED_URL configured".to_string())?;
    let model = embed_model(repo_root);

    let code_views: Vec<String> = entities.iter().map(build_code_view).collect();
    let semantic_views: Vec<String> = entities.iter().map(candidate_embedding_text).collect();
    let ontology_views: Vec<String> = entities.iter().map(build_ontology_view).collect();

    let code_vecs = embed_texts_remote(&base, &model, &code_views)?;
    let semantic_vecs = embed_texts_remote(&base, &model, &semantic_views)?;
    let ontology_vecs = embed_texts_remote(&base, &model, &ontology_views)?;

    if code_vecs.len() != entities.len()
        || semantic_vecs.len() != entities.len()
        || ontology_vecs.len() != entities.len()
    {
        return Err(format!(
            "embed response count mismatch: {} entities but code={} semantic={} ontology={}",
            entities.len(),
            code_vecs.len(),
            semantic_vecs.len(),
            ontology_vecs.len(),
        ));
    }

    let mut embedded = 0usize;
    for (index, entity) in entities.iter_mut().enumerate() {
        let Some(object) = entity.as_object_mut() else {
            continue;
        };
        object.insert("code_vec".to_string(), float_json(&code_vecs[index]));
        object.insert(
            "semantic_vec".to_string(),
            float_json(&semantic_vecs[index]),
        );
        object.insert(
            "ontology_vec".to_string(),
            float_json(&ontology_vecs[index]),
        );
        object.insert("embed_model".to_string(), json!(EMBED_MODEL));
        object.insert("embed_dim".to_string(), json!(EMBED_DIM));
        embedded += 1;
    }

    Ok(embedded)
}

/// Embed one query string on the operator-configured GPU encoder.
///
/// Used by local Arrow search for cosine ranking. The host comes only from
/// `LEIO_CODE_EMBED_URL` / `EMBEDDING_API_URL` — never the inspected repo's
/// `[embed] url`. Returns `Err` when no env URL is set or the encoder call
/// fails — callers keep lexical ranking in that case.
pub(crate) fn embed_query(repo_root: &Path, text: &str) -> Result<Vec<f32>, String> {
    let mut vectors = embed_texts(repo_root, &[text.to_string()])?;
    vectors
        .pop()
        .ok_or_else(|| "remote query embed returned no vector".to_string())
}

/// Embed many texts on the operator-configured GPU encoder.
pub(crate) fn embed_texts(repo_root: &Path, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
    let Some(base) = crate::config::embed_query_url() else {
        return Err("no LEIO_CODE_EMBED_URL / EMBEDDING_API_URL configured".to_string());
    };
    let model = embed_model(repo_root);
    embed_texts_remote(&base, &model, texts)
}

/// POST texts to direct TEI `/embed` or LiteLLM OpenAI `/v1/embeddings`.
///
/// Requests are chunked to [`REMOTE_EMBED_BATCH`] so the lab TEI deployment's
/// `--max-client-batch-size 64` contract is respected.
fn embed_texts_remote(base: &str, model: &str, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let agent = Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_recv_body(Some(REMOTE_EMBED_TIMEOUT))
            .build(),
    );
    let endpoint = embedding_endpoint(base);
    let mut out = Vec::with_capacity(texts.len());
    for chunk in texts.chunks(REMOTE_EMBED_BATCH) {
        let vectors = match &endpoint {
            EmbeddingEndpoint::OpenAi(url) => embed_openai_chunk(&agent, url, model, chunk)?,
            EmbeddingEndpoint::Tei(url) => embed_tei_chunk(&agent, url, chunk)?,
            EmbeddingEndpoint::Gemini(url) => embed_gemini_chunk(&agent, url, chunk)?,
        };
        out.extend(vectors);
    }
    if out.len() != texts.len() {
        return Err(format!(
            "remote embed count mismatch: {} texts, {} vectors",
            texts.len(),
            out.len()
        ));
    }
    Ok(out)
}

#[derive(Debug, PartialEq, Eq)]
enum EmbeddingEndpoint {
    OpenAi(String),
    Tei(String),
    Gemini(String),
}

/// Host of Google's Generative Language API embedding endpoint.
const GEMINI_EMBED_HOST: &str = "generativelanguage.googleapis.com";

/// Model id sent for Gemini embedding requests. `outputDimensionality: 1024`
/// keeps vectors on the same contract as BGE-M3 so cosine scoring stays valid.
const GEMINI_EMBED_MODEL: &str = "models/gemini-embedding-2";

fn embedding_endpoint(base: &str) -> EmbeddingEndpoint {
    let normalized = base.trim_end_matches('/');
    if normalized.contains(GEMINI_EMBED_HOST) {
        let trimmed = normalized
            .strip_suffix(":embedContent")
            .unwrap_or(normalized);
        EmbeddingEndpoint::Gemini(format!("{trimmed}:batchEmbedContents"))
    } else if normalized.ends_with("/embed") {
        EmbeddingEndpoint::Tei(normalized.to_string())
    } else {
        EmbeddingEndpoint::OpenAi(openai_embeddings_url(normalized))
    }
}

fn openai_embeddings_url(base: &str) -> String {
    if base.ends_with("/v1/embeddings") {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}/embeddings")
    } else {
        format!("{base}/v1/embeddings")
    }
}

fn embed_openai_chunk(
    agent: &Agent,
    url: &str,
    model: &str,
    texts: &[String],
) -> Result<Vec<Vec<f32>>, String> {
    let body = json!({
        "model": model,
        "input": texts,
    });
    let mut request = agent.post(url).header("Content-Type", "application/json");
    if let Ok(key) = std::env::var("LEIO_CODE_EMBED_API_KEY") {
        let key = key.trim();
        if !key.is_empty() {
            request = request.header("Authorization", format!("Bearer {key}"));
        }
    }
    let mut response = request
        .send_json(&body)
        .map_err(|err| format!("remote embed {url}: {err}"))?;
    let payload: Value = response
        .body_mut()
        .read_json()
        .map_err(|err| format!("remote embed {url} decode: {err}"))?;
    parse_openai_embeddings(&payload, texts.len())
}

fn embed_tei_chunk(agent: &Agent, url: &str, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
    let body = json!({"inputs": texts});
    let mut request = agent.post(url).header("Content-Type", "application/json");
    if let Ok(key) = std::env::var("LEIO_CODE_EMBED_API_KEY") {
        let key = key.trim();
        if !key.is_empty() {
            request = request.header("Authorization", format!("Bearer {key}"));
        }
    }
    let mut response = request
        .send_json(&body)
        .map_err(|err| format!("remote TEI embed {url}: {err}"))?;
    let payload: Value = response
        .body_mut()
        .read_json()
        .map_err(|err| format!("remote TEI embed {url} decode: {err}"))?;
    parse_tei_embeddings(&payload, texts.len())
}

/// Build the Gemini `batchEmbedContents` request body for one chunk.
fn gemini_batch_request(texts: &[String]) -> Value {
    json!({
        "requests": texts
            .iter()
            .map(|text| {
                json!({
                    "model": GEMINI_EMBED_MODEL,
                    "content": {"parts": [{"text": text}]},
                    "outputDimensionality": EMBED_DIM,
                })
            })
            .collect::<Vec<_>>(),
    })
}

fn embed_gemini_chunk(agent: &Agent, url: &str, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
    let body = gemini_batch_request(texts);
    let mut request = agent.post(url).header("Content-Type", "application/json");
    let key = std::env::var("LEIO_CODE_EMBED_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "gemini embed requires LEIO_CODE_EMBED_API_KEY".to_string())?;
    request = request.header("x-goog-api-key", key);
    let mut response = request
        .send_json(&body)
        .map_err(|err| format!("remote Gemini embed {url}: {err}"))?;
    let payload: Value = response
        .body_mut()
        .read_json()
        .map_err(|err| format!("remote Gemini embed {url} decode: {err}"))?;
    parse_gemini_embeddings(&payload, texts.len())
}

fn parse_openai_embeddings(payload: &Value, expected: usize) -> Result<Vec<Vec<f32>>, String> {
    let data = payload
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            format!(
                "remote embed missing data[]: {}",
                payload
                    .get("error")
                    .cloned()
                    .unwrap_or_else(|| payload.clone())
            )
        })?;
    if data.len() != expected {
        return Err(format!(
            "remote embed count mismatch: {expected} texts, {} vectors",
            data.len()
        ));
    }
    let mut ordered: Vec<Option<Vec<f32>>> = vec![None; expected];
    for item in data {
        let index =
            item.get("index")
                .and_then(Value::as_u64)
                .ok_or_else(|| "remote embed item missing index".to_string())? as usize;
        if index >= expected {
            return Err(format!(
                "remote embed index {index} out of range {expected}"
            ));
        }
        let embedding = item
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("remote embed index {index} missing embedding"))?;
        if embedding.len() != EMBED_DIM {
            return Err(format!(
                "remote embed index {index} dim {} (expected {EMBED_DIM})",
                embedding.len()
            ));
        }
        let mut values = Vec::with_capacity(EMBED_DIM);
        for number in embedding {
            let Some(value) = number.as_f64() else {
                return Err(format!("remote embed index {index} non-float component"));
            };
            values.push(value as f32);
        }
        ordered[index] = Some(values);
    }
    ordered
        .into_iter()
        .enumerate()
        .map(|(index, slot)| slot.ok_or_else(|| format!("missing remote embedding {index}")))
        .collect()
}

fn parse_tei_embeddings(payload: &Value, expected: usize) -> Result<Vec<Vec<f32>>, String> {
    let rows = payload
        .as_array()
        .ok_or_else(|| format!("remote TEI embed expected vector[]: {payload}"))?;
    if rows.len() != expected {
        return Err(format!(
            "remote TEI embed count mismatch: {expected} texts, {} vectors",
            rows.len()
        ));
    }
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let values = row
                .as_array()
                .ok_or_else(|| format!("remote TEI embed index {index} is not a vector"))?;
            if values.len() != EMBED_DIM {
                return Err(format!(
                    "remote TEI embed index {index} dim {} (expected {EMBED_DIM})",
                    values.len()
                ));
            }
            values
                .iter()
                .map(|number| {
                    number.as_f64().map(|value| value as f32).ok_or_else(|| {
                        format!("remote TEI embed index {index} non-float component")
                    })
                })
                .collect()
        })
        .collect()
}

/// Parse a Gemini `batchEmbedContents` response. Embeddings come back in
/// request order under `embeddings[].values`.
fn parse_gemini_embeddings(payload: &Value, expected: usize) -> Result<Vec<Vec<f32>>, String> {
    let rows = payload
        .get("embeddings")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            format!(
                "remote Gemini embed missing embeddings[]: {}",
                payload
                    .get("error")
                    .cloned()
                    .unwrap_or_else(|| payload.clone())
            )
        })?;
    if rows.len() != expected {
        return Err(format!(
            "remote Gemini embed count mismatch: {expected} texts, {} vectors",
            rows.len()
        ));
    }
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let values = row
                .get("values")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("remote Gemini embed index {index} missing values"))?;
            if values.len() != EMBED_DIM {
                return Err(format!(
                    "remote Gemini embed index {index} dim {} (expected {EMBED_DIM})",
                    values.len()
                ));
            }
            values
                .iter()
                .map(|number| {
                    number.as_f64().map(|value| value as f32).ok_or_else(|| {
                        format!("remote Gemini embed index {index} non-float component")
                    })
                })
                .collect()
        })
        .collect()
}

fn float_json(values: &[f32]) -> Value {
    Value::Array(
        values
            .iter()
            .map(|value| json!(f64::from(*value)))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn code_view_prefers_symbol_then_kind() {
        let entity = json!({
            "symbol": "build_node_entity",
            "kind": "symbol",
            "target": "",
            "path": "src/node_rows.rs",
            "text_snippet": "kind=symbol label=build_node_entity",
        });
        let view = build_code_view(&entity);
        assert!(view.contains("build_node_entity"));
        assert!(view.contains("symbol"));
        assert!(view.contains("node_rows.rs"));
    }

    #[test]
    fn semantic_view_uses_snippet_then_falls_back() {
        let with_snippet = json!({
            "symbol": "foo",
            "kind": "symbol",
            "path": "a/b.rs",
            "text_snippet": "natural language description of foo",
        });
        assert_eq!(
            candidate_embedding_text(&with_snippet),
            "natural language description of foo"
        );

        let empty_snippet = json!({
            "symbol": "foo",
            "kind": "symbol",
            "target": "",
            "path": "a/b.rs",
            "text_snippet": "",
        });
        // Falls back to the structural view rather than an empty string.
        assert_eq!(
            candidate_embedding_text(&empty_snippet),
            build_code_view(&empty_snippet)
        );
        assert!(!candidate_embedding_text(&empty_snippet).is_empty());
    }

    #[test]
    fn ontology_view_joins_kind_relations_path_lang() {
        let entity = json!({
            "kind": "deploy_target",
            "path": "deploy/targets/x.toml",
            "lang": "toml",
            "relations": ["kind:deploy_target", "cartridge:revops"],
            "symbol": "x",
        });
        let view = build_ontology_view(&entity);
        assert!(view.starts_with("deploy_target"));
        assert!(view.contains("cartridge:revops"));
        assert!(view.contains("deploy/targets/x.toml"));
        assert!(view.ends_with("toml"));
    }

    #[test]
    fn empty_entities_short_circuit() {
        let mut entities: Vec<Value> = Vec::new();
        assert_eq!(
            embed_node_entities(std::path::Path::new("/tmp"), &mut entities),
            Ok(0)
        );
    }

    #[test]
    fn openai_embeddings_url_appends_v1_path() {
        assert_eq!(
            openai_embeddings_url("http://tei.example:8080"),
            "http://tei.example:8080/v1/embeddings"
        );
        assert_eq!(
            openai_embeddings_url("http://llm.example:4000/v1"),
            "http://llm.example:4000/v1/embeddings"
        );
        assert_eq!(
            openai_embeddings_url("http://tei.example:8080/v1/embeddings"),
            "http://tei.example:8080/v1/embeddings"
        );
    }

    #[test]
    fn embedding_endpoint_detects_direct_tei_path() {
        assert_eq!(
            embedding_endpoint("http://tei.example:8080/embed"),
            EmbeddingEndpoint::Tei("http://tei.example:8080/embed".to_string())
        );
        assert_eq!(
            embedding_endpoint("http://llm.example:4000"),
            EmbeddingEndpoint::OpenAi("http://llm.example:4000/v1/embeddings".to_string())
        );
    }

    #[test]
    fn parse_openai_embeddings_orders_by_index() {
        let payload = json!({
            "data": [
                {"index": 1, "embedding": vec![0.0; EMBED_DIM]},
                {"index": 0, "embedding": vec![1.0; EMBED_DIM]},
            ]
        });
        let vectors = parse_openai_embeddings(&payload, 2).expect("parse");
        assert_eq!(vectors.len(), 2);
        assert!((vectors[0][0] - 1.0).abs() < f32::EPSILON);
        assert!(vectors[1][0].abs() < f32::EPSILON);
    }

    #[test]
    fn parse_tei_embeddings_preserves_input_order() {
        let payload = json!([vec![0.0; EMBED_DIM], vec![1.0; EMBED_DIM]]);
        let vectors = parse_tei_embeddings(&payload, 2).expect("parse");
        assert_eq!(vectors.len(), 2);
        assert!(vectors[0][0].abs() < f32::EPSILON);
        assert!((vectors[1][0] - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn parse_tei_embeddings_rejects_wrong_count() {
        let payload = json!([vec![0.0; EMBED_DIM]]);
        let error = parse_tei_embeddings(&payload, 2).expect_err("count mismatch");
        assert!(error.contains("count mismatch"));
    }

    #[test]
    fn parse_tei_embeddings_rejects_wrong_dimension() {
        let payload = json!([vec![0.0; EMBED_DIM - 1]]);
        let error = parse_tei_embeddings(&payload, 1).expect_err("dimension mismatch");
        assert!(error.contains("dim 1023"));
    }

    #[test]
    fn parse_tei_embeddings_rejects_non_numeric_components() {
        let payload = json!([vec![json!("not-a-number"); EMBED_DIM]]);
        let error = parse_tei_embeddings(&payload, 1).expect_err("numeric component");
        assert!(error.contains("non-float component"));
    }

    #[test]
    fn candidate_embedding_text_prefers_nonempty_snippet() {
        let entity = json!({
            "symbol": "build_node",
            "kind": "symbol",
            "path": "src/node.rs",
            "text_snippet": "  natural language snippet  ",
        });
        assert_eq!(
            candidate_embedding_text(&entity),
            "natural language snippet"
        );
    }

    #[test]
    fn candidate_embedding_text_falls_back_to_nonempty_structural_fields() {
        let entity = json!({
            "symbol": "build_node",
            "kind": "symbol",
            "path": "src/node.rs",
            "text_snippet": "",
            "target": "ignored-target",
        });
        assert_eq!(
            candidate_embedding_text(&entity),
            "build_node symbol node.rs"
        );
    }

    #[test]
    fn candidate_embedding_text_truncates_by_characters() {
        let entity = json!({
            "text_snippet": "é".repeat(MAX_VIEW_CHARS + 1),
        });
        let text = candidate_embedding_text(&entity);
        assert_eq!(text.chars().count(), MAX_VIEW_CHARS);
        assert_eq!(text, "é".repeat(MAX_VIEW_CHARS));
    }

    #[test]
    fn parse_openai_embeddings_rejects_wrong_count() {
        let payload = json!({
            "data": [
                {"index": 0, "embedding": vec![0.0; EMBED_DIM]},
                {"index": 1, "embedding": vec![1.0; EMBED_DIM]},
                {"index": 1, "embedding": vec![1.0; EMBED_DIM]},
            ]
        });
        let error = parse_openai_embeddings(&payload, 2).expect_err("count mismatch");
        assert!(error.contains("count mismatch"));
    }

    #[test]
    fn embedding_endpoint_detects_gemini_path() {
        assert_eq!(
            embedding_endpoint("https://generativelanguage.googleapis.com/v1beta/models/gemini-embedding-2"),
            EmbeddingEndpoint::Gemini(
                "https://generativelanguage.googleapis.com/v1beta/models/gemini-embedding-2:batchEmbedContents"
                    .to_string()
            )
        );
        assert_eq!(
            embedding_endpoint("https://generativelanguage.googleapis.com/v1beta/models/gemini-embedding-2:embedContent"),
            EmbeddingEndpoint::Gemini(
                "https://generativelanguage.googleapis.com/v1beta/models/gemini-embedding-2:batchEmbedContents"
                    .to_string()
            )
        );
    }

    #[test]
    fn parse_gemini_embeddings_preserves_input_order() {
        let payload = json!({
            "embeddings": [
                {"values": vec![0.0; EMBED_DIM]},
                {"values": vec![1.0; EMBED_DIM]},
            ]
        });
        let vectors = parse_gemini_embeddings(&payload, 2).expect("parse");
        assert_eq!(vectors.len(), 2);
        assert!(vectors[0][0].abs() < f32::EPSILON);
        assert!((vectors[1][0] - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn parse_gemini_embeddings_rejects_wrong_count() {
        let payload = json!({"embeddings": [{"values": vec![0.0; EMBED_DIM]}]});
        let error = parse_gemini_embeddings(&payload, 2).expect_err("count mismatch");
        assert!(error.contains("count mismatch"));
    }

    #[test]
    fn parse_gemini_embeddings_rejects_wrong_dimension() {
        let payload = json!({"embeddings": [{"values": vec![0.0; EMBED_DIM - 1]}]});
        let error = parse_gemini_embeddings(&payload, 1).expect_err("dimension mismatch");
        assert!(error.contains("dim 1023"));
    }

    #[test]
    fn gemini_batch_request_wraps_texts_with_model_and_dimension() {
        let texts = vec!["alpha".to_string(), "beta".to_string()];
        let body = gemini_batch_request(&texts);
        assert_eq!(body["requests"].as_array().unwrap().len(), 2);
        assert_eq!(body["requests"][0]["model"], "models/gemini-embedding-2");
        assert_eq!(body["requests"][0]["content"]["parts"][0]["text"], "alpha");
        assert_eq!(body["requests"][0]["outputDimensionality"], EMBED_DIM);
    }
}
