//! Embedding adapter: OpenAI-compatible `/embeddings` HTTP client used to turn
//! agent state / code-view summaries into bus vectors. No shortcuts: explicit
//! timeouts, retries with backoff, dimension consistency checks.
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct EmbedClient {
    endpoint: String,
    model: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingItem>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingItem {
    embedding: Vec<f32>,
    index: usize,
}

impl EmbedClient {
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Result<Self> {
        let base_url = base_url.into();
        let endpoint = format!("{}/embeddings", base_url.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self {
            endpoint,
            model: model.into(),
            api_key,
            client,
        })
    }

    pub fn from_env() -> Result<Self> {
        // Env vars win; a config file (~/.config/leio-harness/env) supplies
        // defaults so the harness works without exporting every run.
        let values = default_config_values();
        let base_url = std::env::var("LEIO_HARNESS_EMBED_URL")
            .ok()
            .or_else(|| values.get("LEIO_HARNESS_EMBED_URL").cloned())
            .context("LEIO_HARNESS_EMBED_URL is not set")?;
        let model = std::env::var("LEIO_HARNESS_EMBED_MODEL")
            .ok()
            .or_else(|| values.get("LEIO_HARNESS_EMBED_MODEL").cloned())
            .context("LEIO_HARNESS_EMBED_MODEL is not set")?;
        let api_key = std::env::var("LEIO_HARNESS_EMBED_KEY")
            .ok()
            .or_else(|| values.get("LEIO_HARNESS_EMBED_KEY").cloned());
        Self::new(base_url, model, api_key)
    }
}

fn default_config_values() -> std::collections::HashMap<String, String> {
    let file = std::env::var("LEIO_HARNESS_CONFIG")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|home| std::path::Path::new(&home).join(".config/leio-harness/env"))
        });
    file.and_then(|path| read_env_file(&path).ok())
        .unwrap_or_default()
}

pub fn read_env_file(path: &std::path::Path) -> Result<std::collections::HashMap<String, String>> {
    let contents = std::fs::read_to_string(path)?;
    let mut values = std::collections::HashMap::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            values.insert(key.trim().to_owned(), value.trim().to_owned());
        }
    }
    Ok(values)
}

impl EmbedClient {
    pub async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            bail!("embed requires at least one input");
        }
        let body = serde_json::json!({
            "model": self.model,
            "input": texts,
        });
        let mut last_error: Option<anyhow::Error> = None;
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(250 * attempt as u64)).await;
            }
            match self.try_embed(&body).await {
                Ok(vectors) => return Ok(vectors),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("embedding request failed")))
    }

    async fn try_embed(&self, body: &serde_json::Value) -> Result<Vec<Vec<f32>>> {
        let mut request = self.client.post(&self.endpoint).json(body);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        let response = request.send().await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!(
                "embedding endpoint returned {status}: {}",
                &text[..text.len().min(300)]
            );
        }
        let parsed: EmbeddingsResponse = response.json().await?;
        let mut items = parsed.data;
        items.sort_by_key(|item| item.index);
        let vectors: Vec<Vec<f32>> = items.into_iter().map(|item| item.embedding).collect();
        let expected = vectors.first().map(Vec::len).unwrap_or(0);
        if expected == 0 || vectors.iter().any(|vector| vector.len() != expected) {
            bail!("embedding response has inconsistent dimensions");
        }
        Ok(vectors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn spawn_mock(body: String, status_line: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buf = vec![0u8; 8192];
                let _ = stream.read(&mut buf);
                let body = body.clone();
                let response = format!(
                    "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        port
    }

    #[tokio::test]
    async fn parses_embeddings_and_preserves_order() {
        let body =
            r#"{"data":[{"index":1,"embedding":[0.1,0.2]},{"index":0,"embedding":[0.3,0.4]}]}"#
                .to_owned();
        let port = spawn_mock(body, "HTTP/1.1 200 OK");
        let client = EmbedClient::new(format!("http://127.0.0.1:{port}/v1"), "m", None).unwrap();
        let vectors = client
            .embed(&["a".to_owned(), "b".to_owned()])
            .await
            .unwrap();
        assert_eq!(vectors, vec![vec![0.3, 0.4], vec![0.1, 0.2]]);
    }

    #[tokio::test]
    async fn rejects_inconsistent_dimensions() {
        let body = r#"{"data":[{"index":0,"embedding":[0.1,0.2]},{"index":1,"embedding":[0.3]}]}"#
            .to_owned();
        let port = spawn_mock(body, "HTTP/1.1 200 OK");
        let client = EmbedClient::new(format!("http://127.0.0.1:{port}/v1"), "m", None).unwrap();
        assert!(
            client
                .embed(&["a".to_owned(), "b".to_owned()])
                .await
                .is_err()
        );
    }

    #[test]
    fn env_file_parses_key_values_and_skips_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("env");
        std::fs::write(
            &path,
            "LEIO_HARNESS_EMBED_URL=http://x/v1\n# comment\nLEIO_HARNESS_EMBED_MODEL=bge-m3\n\n",
        )
        .unwrap();
        let values = read_env_file(&path).unwrap();
        assert_eq!(values["LEIO_HARNESS_EMBED_URL"], "http://x/v1");
        assert_eq!(values["LEIO_HARNESS_EMBED_MODEL"], "bge-m3");
        assert!(!values.contains_key("# comment"));
    }

    #[tokio::test]
    async fn non_success_returns_error_after_retries() {
        let port = spawn_mock(
            "{\"error\":\"no\"}".to_owned(),
            "HTTP/1.1 500 Internal Server Error",
        );
        let client = EmbedClient::new(format!("http://127.0.0.1:{port}/v1"), "m", None).unwrap();
        assert!(client.embed(&["a".to_owned()]).await.is_err());
    }
}
