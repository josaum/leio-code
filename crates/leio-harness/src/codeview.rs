//! Mandatory per-agent global code view: indexes the lane's repo/worktree
//! **in-process** through the vendored `leio-code` library (no subprocess,
//! no CLI dependency) and publishes the index fingerprint to the bus so the
//! orchestrator always knows which code view each agent is looking at.
use crate::bus::{EmbeddingRow, new_embedding_row};
use crate::bus_client::BusClient;
use crate::embed::EmbedClient;
use anyhow::{Context, Result};
use leio_code::indexer::{
    build_index_summary, build_or_update_index, default_index_path, load_fresh_index_summary,
};
use std::path::Path;
use std::time::Duration;

#[derive(Debug, serde::Serialize)]
pub struct CodeViewStaleness {
    pub repo: String,
    pub fresh: bool,
    pub file_count: Option<u64>,
    pub indexed_at: Option<String>,
    pub reason: String,
}

/// Staleness gate for a lane's code view: fresh only when leio-code's own
/// stat-equality check says the stamped summary matches the current index.
pub fn check_code_view(repo: &Path) -> CodeViewStaleness {
    let index_path = default_index_path(repo);
    match load_fresh_index_summary(repo, &index_path) {
        Some(summary) => CodeViewStaleness {
            repo: repo.display().to_string(),
            fresh: true,
            file_count: Some(summary.file_count as u64),
            indexed_at: Some(summary.indexed_at),
            reason: "index summary matches current index".to_owned(),
        },
        None => CodeViewStaleness {
            repo: repo.display().to_string(),
            fresh: false,
            file_count: None,
            indexed_at: None,
            reason: "missing or stale leio-code index; run codeview publish first".to_owned(),
        },
    }
}

#[derive(Debug, serde::Serialize)]
pub struct CodeViewPublication {
    pub repo: String,
    pub topic: String,
    pub last_seq: u64,
    pub summary: String,
    pub file_count: u64,
}

/// Index the repo in-process via leio-code, embed the index summary, and
/// publish to `code-view/<repo-name>` on the bus.
pub async fn publish_code_view(
    repo: &Path,
    bus_addr: &str,
    agent_id: &str,
    run_id: &str,
    embed: Option<&EmbedClient>,
) -> Result<CodeViewPublication> {
    let root = repo.to_path_buf();
    let (file_count, indexed_at) = tokio::task::spawn_blocking(move || {
        let index_path = default_index_path(&root);
        let index = build_or_update_index(&root, &index_path, true)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let summary = build_index_summary(&index);
        Ok::<(u64, String), anyhow::Error>((summary.file_count as u64, summary.indexed_at.clone()))
    })
    .await
    .context("leio-code indexer task panicked")??;

    let summary = format!(
        "repo={} files={} indexed_at={}",
        repo.display(),
        file_count,
        indexed_at
    );
    let vector = match embed {
        Some(embed) => match embed.embed(std::slice::from_ref(&summary)).await {
            Ok(vectors) => vectors
                .into_iter()
                .next()
                .context("no embedding returned")?,
            Err(error) => {
                eprintln!(
                    "warning: code-view embedding unavailable ({error}); using fingerprint vector"
                );
                fingerprint_vector(&summary)
            }
        },
        None => fingerprint_vector(&summary),
    };
    let repo_name = repo
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_owned());
    let topic = format!("code-view/{repo_name}");
    let mut bus = BusClient::connect_with_retry(bus_addr, 10, Duration::from_millis(100)).await?;
    let row: EmbeddingRow = new_embedding_row(agent_id, run_id, &topic, vector);
    let last_seq = bus.publish(vec![row]).await?;
    Ok(CodeViewPublication {
        repo: repo.display().to_string(),
        topic,
        last_seq,
        summary,
        file_count,
    })
}

/// Deterministic vector from a string (sha256 expanded to 8 f32 dims, then
/// normalized). Not semantic, but stable and dimension-consistent.
pub fn fingerprint_vector(text: &str) -> Vec<f32> {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(text.as_bytes());
    let mut out: Vec<f32> = digest[..16]
        .chunks_exact(2)
        .map(|pair| (i16::from_le_bytes([pair[0], pair[1]]) as f32) / i16::MAX as f32)
        .collect();
    normalize_f32(&mut out);
    out
}

fn normalize_f32(vector: &mut [f32]) {
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in vector.iter_mut() {
            *value /= norm;
        }
    }
}
