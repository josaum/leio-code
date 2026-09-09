//! SHA-256 artifact manifest for day runs — every lane's logs, Arrow event
//! stream, and result file hashed and byte-counted for release evidence.
use anyhow::{Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactEntry {
    pub path: String,
    pub producer: String,
    pub sha256: String,
    pub byte_size: u64,
}

#[derive(Debug, Serialize)]
pub struct DayManifest {
    pub version: u32,
    pub goal: String,
    pub generated_at_ms: u64,
    pub artifacts: Vec<ArtifactEntry>,
}

pub fn hash_file(path: &Path) -> Result<(String, u64)> {
    let data = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let mut digest = Sha256::new();
    digest.update(&data);
    Ok((hex::encode(digest.finalize()), data.len() as u64))
}

pub fn artifact_entry(path: &Path, producer: &str) -> Result<ArtifactEntry> {
    let (sha256, byte_size) = hash_file(path)?;
    Ok(ArtifactEntry {
        path: path.display().to_string(),
        producer: producer.to_owned(),
        sha256,
        byte_size,
    })
}

pub fn collect_run_artifacts(output_paths: &[String], producer: &str) -> Vec<ArtifactEntry> {
    let mut entries: Vec<ArtifactEntry> = output_paths
        .iter()
        .filter_map(|path| artifact_entry(Path::new(path), producer).ok())
        .collect();
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    entries
}

pub fn write_day_manifest(output_dir: &Path, manifest: &DayManifest) -> Result<std::path::PathBuf> {
    std::fs::create_dir_all(output_dir)?;
    let path = output_dir.join("manifest.json");
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(
        &temporary,
        format!("{}\n", serde_json::to_string_pretty(manifest)?),
    )?;
    std::fs::rename(temporary, &path)?;
    Ok(path)
}
