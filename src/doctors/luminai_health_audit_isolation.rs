//! Luminai is a separate product regime.  This doctor deliberately examines
//! only its declared surfaces and deploy wiring; Health Audit is never loaded
//! as an implementation dependency.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde_json::json;
use sha2::{Digest, Sha256};

use super::Doctor;
use super::utils::query_id;
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct LuminaiHealthAuditIsolationDoctor;

impl Doctor for LuminaiHealthAuditIsolationDoctor {
    fn name(&self) -> &'static str {
        "luminai-health-audit-isolation"
    }
    fn description(&self) -> &'static str {
        "Rejects Health Audit and unrelated-product coupling from Luminai surfaces."
    }
    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_luminai_health_audit_isolation(index, root)
    }
}

const HA_MARKERS: &[(&str, &str)] = &[
    ("cartridges.health_audit", "luminai_ha_forbidden_import"),
    ("HEALTH_AUDIT_", "luminai_ha_forbidden_env"),
    ("HA_", "luminai_ha_forbidden_env"),
    ("ha:", "luminai_ha_forbidden_redis"),
    ("hospital_audit", "luminai_ha_operational_identity"),
    ("health-audit-", "luminai_ha_operational_identity"),
    ("example-health-audit-", "luminai_ha_operational_identity"),
    ("health_audit", "luminai_ha_operational_identity"),
];
const UNRELATED_MARKERS: &[(&str, &str)] = &[
    ("cartridges.pacto", "luminai_unrelated_product_import"),
    ("cartridges.jaipay", "luminai_unrelated_product_import"),
    ("cartridges.fitness", "luminai_unrelated_product_import"),
    ("pacto:", "luminai_unrelated_product_identity"),
    ("jaipay:", "luminai_unrelated_product_identity"),
    ("fitness:", "luminai_unrelated_product_identity"),
    ("pacto", "luminai_unrelated_product_identity"),
    ("jaipay", "luminai_unrelated_product_identity"),
    ("fitness", "luminai_unrelated_product_identity"),
];
const FROZEN_PREFIXES: &[&str] = &[
    "cartridges/health_audit/",
    "health-audit-console/",
    "deploy/targets/health_audit",
    "deploy/profiles/health_audit",
    "deploy/secret-sets/hospital_audit",
    "example-api/docker-compose.health-audit",
    "deploy/health-audit",
];
const LUMINAI_WORKFLOW_PREFIX: &str = "example-gateway/assets/workflows/luminai_revenue_cycle/";

fn files(root: &Path) -> Vec<PathBuf> {
    fn visit(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            if path.is_dir() {
                visit(&path, out);
            } else if path.is_file() {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    visit(root, &mut out);
    out
}

fn safe_repo_file(root: &Path, raw: &str) -> Option<PathBuf> {
    if !safe_path(raw) {
        return None;
    }
    let root = root.canonicalize().ok()?;
    let candidate = root.join(raw);
    let resolved = candidate.canonicalize().ok()?;
    (resolved.starts_with(&root) && resolved.is_file()).then_some(resolved)
}

fn target_compose_files(root: &Path, target_path: &Path) -> Vec<PathBuf> {
    let Ok(source) = std::fs::read_to_string(target_path) else {
        return Vec::new();
    };
    let Ok(value) = source.parse::<toml::Value>() else {
        return Vec::new();
    };
    ["compose_file"]
        .into_iter()
        .filter_map(|key| value.get(key).and_then(toml::Value::as_str))
        .filter_map(|path| safe_repo_file(root, path))
        .collect()
}

fn luminai_surfaces(root: &Path, index: &RepoIndex, all: &[PathBuf]) -> Vec<PathBuf> {
    let mut selected = Vec::new();
    for path in all {
        let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy();
        if (rel.starts_with("cartridges/luminai_revenue_cycle/")
            || rel.starts_with("luminai-console/")
            || rel.starts_with(LUMINAI_WORKFLOW_PREFIX))
            && !rel.contains("/tests/")
        {
            selected.push(path.clone());
        }
    }
    let mut pending: Vec<_> = index
        .deploy_targets
        .iter()
        .filter(|target| {
            target
                .cartridges
                .iter()
                .any(|cartridge| cartridge == "luminai_revenue_cycle")
        })
        .collect();
    let mut visited = HashSet::new();
    while let Some(target) = pending.pop() {
        if !visited.insert(target.name.as_str()) {
            continue;
        }
        let target_path = root.join(&target.path);
        selected.push(target_path.clone());
        selected.extend(target_compose_files(root, &target_path));
        for name in [&target.profile, &target.backend_profile] {
            if let Some(name) = name {
                selected.push(root.join(format!("deploy/profiles/{name}.env")));
            }
            if let Some(name) = name {
                selected.extend(
                    index
                        .profiles
                        .iter()
                        .filter(|profile| profile.name == *name)
                        .map(|profile| root.join(&profile.path)),
                );
            }
        }
        if let Some(name) = &target.secret_set {
            selected.push(root.join(format!("deploy/secret-sets/{name}.env.example")));
            selected.extend(
                index
                    .secret_sets
                    .iter()
                    .filter(|secret| secret.name == *name)
                    .map(|secret| root.join(&secret.path)),
            );
        }
        if let Some(next) = &target.readiness_target
            && let Some(next_target) = index
                .deploy_targets
                .iter()
                .find(|candidate| candidate.name == *next)
        {
            pending.push(next_target);
        }
    }
    if selected.is_empty() {
        return selected;
    }
    selected.retain(|path| path.is_file());
    selected.sort();
    selected.dedup();
    selected
}

fn push(evidence: &mut Vec<EvidenceItem>, kind: &str, path: &Path, text: &str, needle: &str) {
    evidence.push(EvidenceItem {
        kind: kind.to_string(),
        path: path.display().to_string(),
        line: text
            .lines()
            .position(|line| {
                line.to_ascii_uppercase()
                    .contains(&needle.to_ascii_uppercase())
            })
            .map(|line| line + 1),
        detail: format!("Luminai surface references forbidden identity `{needle}`"),
    });
}

fn changed_frozen_paths(root: &Path, base: &str) -> Result<Vec<String>, String> {
    let valid = Command::new("git")
        .args(["cat-file", "-e", &format!("{base}^{{commit}}")])
        .current_dir(root)
        .status()
        .map_err(|e| e.to_string())?
        .success();
    if !valid {
        return Err("LEIO_LUMINAI_CHANGE_BASE does not resolve to a commit".to_string());
    }
    let output = Command::new("git")
        .args(["diff", "--name-only", &format!("{base}..HEAD")])
        .current_dir(root)
        .output()
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|p| frozen(p))
        .map(str::to_owned)
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DirtyEntry {
    path: String,
    status: String,
    original_path: Option<String>,
    index_mode: Option<String>,
    index_blob_oid: Option<String>,
    worktree_kind: String,
    worktree_sha256: Option<String>,
}

fn oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn safe_path(value: &str) -> bool {
    let trimmed = value.strip_suffix('/').unwrap_or(value);
    !trimmed.is_empty()
        && !Path::new(value).is_absolute()
        && !trimmed
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
}
fn frozen(path: &str) -> bool {
    FROZEN_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

fn hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn tree_digest(path: &Path) -> Result<(String, Option<String>), String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if meta.file_type().is_symlink() {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            return Ok((
                "symlink".to_string(),
                Some(hex(std::fs::read_link(path)
                    .map_err(|e| e.to_string())?
                    .as_os_str()
                    .as_bytes())),
            ));
        }
        #[cfg(not(unix))]
        {
            return Err("symlink digest requires unix".to_string());
        }
    }
    if meta.is_file() {
        return Ok((
            "file".to_string(),
            Some(hex(&std::fs::read(path).map_err(|e| e.to_string())?)),
        ));
    }
    if !meta.is_dir() {
        return Ok(("missing".to_string(), None));
    }
    fn visit(
        dir: &Path,
        prefix: &Path,
        rows: &mut Vec<(Vec<u8>, u8, Vec<u8>)>,
    ) -> Result<(), String> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let rel = prefix.join(entry.file_name());
            #[cfg(unix)]
            use std::os::unix::ffi::OsStrExt;
            #[cfg(unix)]
            let key = rel.as_os_str().as_bytes().to_vec();
            #[cfg(not(unix))]
            let key = rel.to_string_lossy().as_bytes().to_vec();
            let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if meta.is_dir() {
                rows.push((key, b'D', Vec::new()));
                visit(&path, &rel, rows)?;
            } else if meta.file_type().is_symlink() {
                #[cfg(unix)]
                rows.push((
                    key,
                    b'L',
                    std::fs::read_link(&path)
                        .map_err(|e| e.to_string())?
                        .as_os_str()
                        .as_bytes()
                        .to_vec(),
                ));
            } else if meta.is_file() {
                rows.push((key, b'F', std::fs::read(&path).map_err(|e| e.to_string())?));
            } else {
                rows.push((key, b'O', Vec::new()));
            }
        }
        Ok(())
    }
    let mut rows = Vec::new();
    visit(path, Path::new(""), &mut rows)?;
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let mut stream = Vec::new();
    for (name, kind, payload) in rows {
        stream.extend_from_slice(&(name.len() as u64).to_be_bytes());
        stream.extend_from_slice(&name);
        stream.push(kind);
        stream.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        stream.extend_from_slice(&payload);
    }
    Ok(("directory".to_string(), Some(hex(&stream))))
}

fn index_state(root: &Path, path: &str) -> Result<(Option<String>, Option<String>), String> {
    let output = Command::new("git")
        .args(["ls-files", "-s", "--", path])
        .current_dir(root)
        .output()
        .map_err(|e| e.to_string())?;
    let line =
        String::from_utf8(output.stdout).map_err(|_| "index path is not UTF-8".to_string())?;
    let Some(row) = line
        .lines()
        .find(|row| row.split_whitespace().nth(2) == Some("0"))
    else {
        return Ok((None, None));
    };
    let fields: Vec<_> = row.split_whitespace().collect();
    if fields.len() < 3 {
        return Err("malformed git index record".to_string());
    }
    Ok((Some(fields[0].to_string()), Some(fields[1].to_string())))
}

fn manifest_entry(value: &serde_json::Value) -> Result<DirtyEntry, String> {
    let obj = value
        .as_object()
        .ok_or("manifest entry must be an object")?;
    let expected = [
        "path",
        "status",
        "original_path",
        "index_mode",
        "index_blob_oid",
        "worktree_kind",
        "worktree_sha256",
    ];
    if obj.len() != expected.len() || expected.iter().any(|key| !obj.contains_key(*key)) {
        return Err("manifest entry has unknown or missing fields".to_string());
    }
    let string = |key: &str| {
        obj[key]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("{key} must be a string"))
    };
    let path = string("path")?;
    if !safe_path(&path) {
        return Err("manifest path is unsafe".to_string());
    }
    let status = string("status")?;
    if status.len() != 2
        || status == "!!"
        || (status != "??" && !status.bytes().all(|b| b == b' ' || b"MADRCUT".contains(&b)))
    {
        return Err("manifest status is invalid".to_string());
    }
    let original_path = if obj["original_path"].is_null() {
        None
    } else {
        Some(string("original_path")?)
    };
    if original_path.as_deref().is_some_and(|p| !safe_path(p))
        || (original_path.is_some() && !matches!(status.as_bytes()[0], b'R' | b'C'))
    {
        return Err("manifest original_path is invalid".to_string());
    }
    let index_mode = if obj["index_mode"].is_null() {
        None
    } else {
        Some(string("index_mode")?)
    };
    let index_blob_oid = if obj["index_blob_oid"].is_null() {
        None
    } else {
        Some(string("index_blob_oid")?)
    };
    if index_mode.is_some() != index_blob_oid.is_some()
        || index_mode
            .as_deref()
            .is_some_and(|m| m.len() != 6 || !m.bytes().all(|b| (b'0'..=b'7').contains(&b)))
        || index_blob_oid.as_deref().is_some_and(|id| !oid(id))
    {
        return Err("manifest index state is invalid".to_string());
    }
    let worktree_kind = string("worktree_kind")?;
    if !matches!(
        worktree_kind.as_str(),
        "file" | "directory" | "symlink" | "missing"
    ) {
        return Err("manifest worktree_kind is invalid".to_string());
    }
    let worktree_sha256 = if obj["worktree_sha256"].is_null() {
        None
    } else {
        Some(string("worktree_sha256")?)
    };
    if (worktree_kind == "missing") != worktree_sha256.is_none()
        || worktree_sha256.as_deref().is_some_and(|h| {
            h.len() != 64
                || !h
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    {
        return Err("manifest worktree_sha256 is invalid".to_string());
    }
    Ok(DirtyEntry {
        path,
        status,
        original_path,
        index_mode,
        index_blob_oid,
        worktree_kind,
        worktree_sha256,
    })
}

fn current_dirty(root: &Path) -> Result<Vec<DirtyEntry>, String> {
    let output = Command::new("git")
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=normal"])
        .current_dir(root)
        .output()
        .map_err(|e| e.to_string())?;
    let mut fields = output.stdout.split(|b| *b == 0);
    let mut entries = Vec::new();
    while let Some(row) = fields.next() {
        if row.is_empty() {
            continue;
        }
        if row.len() < 4 {
            return Err("malformed porcelain status".to_string());
        }
        let status = std::str::from_utf8(&row[..2])
            .map_err(|_| "non-UTF8 status".to_string())?
            .to_string();
        let path = std::str::from_utf8(&row[3..])
            .map_err(|_| "non-UTF8 porcelain path".to_string())?
            .to_string();
        let original_path = if matches!(status.as_bytes()[0], b'R' | b'C') {
            Some(
                std::str::from_utf8(fields.next().ok_or("missing rename source")?)
                    .map_err(|_| "non-UTF8 porcelain path".to_string())?
                    .to_string(),
            )
        } else {
            None
        };
        let (index_mode, index_blob_oid) = index_state(root, &path)?;
        let full = root.join(&path);
        let (worktree_kind, worktree_sha256) =
            if full.exists() || std::fs::symlink_metadata(&full).is_ok() {
                tree_digest(&full)?
            } else {
                ("missing".to_string(), None)
            };
        entries.push(DirtyEntry {
            path,
            status,
            original_path,
            index_mode,
            index_blob_oid,
            worktree_kind,
            worktree_sha256,
        });
    }
    Ok(entries)
}

fn validate_dirty_manifest(_root: &Path, base: &str) -> Result<Vec<DirtyEntry>, String> {
    let raw = std::env::var("LEIO_LUMINAI_PREEXISTING_DIRTY_MANIFEST")
        .map_err(|_| "missing dirty manifest".to_string())?;
    let path = Path::new(&raw);
    if !path.is_absolute()
        || std::fs::symlink_metadata(path)
            .map_err(|_| "dirty manifest is missing".to_string())?
            .file_type()
            .is_symlink()
        || !path.is_file()
    {
        return Err("dirty manifest must be an existing absolute regular file".to_string());
    }
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|_| "dirty manifest is not valid JSON".to_string())?;
    let obj = value
        .as_object()
        .ok_or("dirty manifest must be an object")?;
    if obj.len() != 3
        || !obj.contains_key("version")
        || !obj.contains_key("base_commit")
        || !obj.contains_key("entries")
        || obj["version"] != 1
        || obj["base_commit"].as_str() != Some(base)
        || !oid(base)
    {
        return Err("dirty manifest header is invalid".to_string());
    }
    let mut entries = Vec::new();
    for entry in obj["entries"]
        .as_array()
        .ok_or("dirty manifest entries must be an array")?
    {
        entries.push(manifest_entry(entry)?);
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    if entries.windows(2).any(|pair| pair[0].path == pair[1].path) {
        return Err("dirty manifest has duplicate paths".to_string());
    }
    Ok(entries)
}

pub fn doctor_luminai_health_audit_isolation(index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let all = files(root);
    let surfaces = luminai_surfaces(root, index, &all);
    if surfaces.is_empty() {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_luminai_health_audit_isolation"),
            kind: "doctor".to_string(),
            summary: "Luminai surfaces absent; isolation guard inactive".to_string(),
            confidence: 0.98,
            entities: Vec::new(),
            evidence: Vec::new(),
            warnings: Vec::new(),
            meta: Some(json!({"reason":"luminai_surfaces_absent"})),
            timing_ms: started.elapsed().as_millis(),
        };
    }
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    for path in surfaces {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let normalized = text.to_ascii_uppercase();
        for (needle, kind) in HA_MARKERS.iter().chain(UNRELATED_MARKERS.iter()) {
            if normalized.contains(&needle.to_ascii_uppercase()) {
                push(&mut evidence, kind, &path, &text, needle);
                warnings.push(format!("{kind}: {} references `{needle}`", path.display()));
            }
        }
    }
    match std::env::var("LEIO_LUMINAI_CHANGE_BASE") {
        Ok(base) if !base.trim().is_empty() => match changed_frozen_paths(root, &base) {
            Ok(paths) => {
                for path in paths {
                    evidence.push(EvidenceItem {
                        kind: "luminai_ha_frozen_path".to_string(),
                        path: path.clone(),
                        line: None,
                        detail:
                            "committed Luminai change set touches a frozen Health Audit surface"
                                .to_string(),
                    });
                    warnings.push(format!("luminai_ha_frozen_path: {path}"));
                }
            }
            Err(detail) => {
                evidence.push(EvidenceItem {
                    kind: "luminai_changeset_unconfigured".to_string(),
                    path: root.display().to_string(),
                    line: None,
                    detail: detail.clone(),
                });
                warnings.push(format!("luminai_changeset_unconfigured: {detail}"));
            }
        },
        _ if std::env::var("CI").ok().as_deref() == Some("true") => {
            evidence.push(EvidenceItem {
                kind: "luminai_changeset_unconfigured".to_string(),
                path: root.display().to_string(),
                line: None,
                detail: "CI requires LEIO_LUMINAI_CHANGE_BASE when Luminai is active".to_string(),
            });
            warnings.push(
                "luminai_changeset_unconfigured: CI requires LEIO_LUMINAI_CHANGE_BASE".to_string(),
            );
        }
        _ => {}
    }
    match std::env::var("LEIO_LUMINAI_PREEXISTING_DIRTY_MANIFEST") {
        Ok(_) if std::env::var("CI").ok().as_deref() == Some("true") => {
            evidence.push(EvidenceItem {
                kind: "luminai_changeset_unconfigured".to_string(),
                path: root.display().to_string(),
                line: None,
                detail: "CI must not use LEIO_LUMINAI_PREEXISTING_DIRTY_MANIFEST".to_string(),
            });
            warnings.push(
                "luminai_changeset_unconfigured: CI must not use a dirty manifest".to_string(),
            );
        }
        Ok(_) => match std::env::var("LEIO_LUMINAI_CHANGE_BASE")
            .ok()
            .filter(|base| !base.is_empty())
            .ok_or("dirty manifest requires LEIO_LUMINAI_CHANGE_BASE".to_string())
            .and_then(|base| validate_dirty_manifest(root, &base).map(|entries| (base, entries)))
        {
            Ok((_base, baseline)) => match current_dirty(root) {
                Ok(current) => {
                    let baseline_frozen: Vec<_> = baseline
                        .into_iter()
                        .filter(|entry| {
                            frozen(&entry.path)
                                || entry.original_path.as_deref().is_some_and(frozen)
                        })
                        .collect();
                    let current_frozen: Vec<_> = current
                        .into_iter()
                        .filter(|entry| {
                            frozen(&entry.path)
                                || entry.original_path.as_deref().is_some_and(frozen)
                        })
                        .collect();
                    for entry in &current_frozen {
                        if !baseline_frozen.iter().any(|baseline| baseline == entry) {
                            evidence.push(EvidenceItem { kind: "luminai_ha_frozen_path".to_string(), path: entry.path.clone(), line: None, detail: "frozen dirty state is new or differs from its exact baseline record".to_string() });
                            warnings.push(format!("luminai_ha_frozen_path: {}", entry.path));
                        }
                    }
                    for entry in &baseline_frozen {
                        if !current_frozen.iter().any(|current| current == entry) {
                            evidence.push(EvidenceItem { kind: "luminai_ha_frozen_path".to_string(), path: entry.path.clone(), line: None, detail: "baseline frozen dirty record is no longer byte/status/index-identical".to_string() });
                            warnings.push(format!("luminai_ha_frozen_path: {}", entry.path));
                        }
                    }
                }
                Err(detail) => {
                    evidence.push(EvidenceItem {
                        kind: "luminai_changeset_unconfigured".to_string(),
                        path: root.display().to_string(),
                        line: None,
                        detail: detail.clone(),
                    });
                    warnings.push(format!("luminai_changeset_unconfigured: {detail}"));
                }
            },
            Err(detail) => {
                evidence.push(EvidenceItem {
                    kind: "luminai_changeset_unconfigured".to_string(),
                    path: root.display().to_string(),
                    line: None,
                    detail: detail.clone(),
                });
                warnings.push(format!("luminai_changeset_unconfigured: {detail}"));
            }
        },
        Err(_) => match current_dirty(root) {
            Ok(current) => {
                for entry in current.into_iter().filter(|entry| {
                    frozen(&entry.path) || entry.original_path.as_deref().is_some_and(frozen)
                }) {
                    evidence.push(EvidenceItem {
                        kind: "luminai_ha_frozen_path".to_string(),
                        path: entry.path.clone(),
                        line: None,
                        detail: "frozen dirty state has no explicit baseline exemption".to_string(),
                    });
                    warnings.push(format!("luminai_ha_frozen_path: {}", entry.path));
                }
            }
            Err(detail) => {
                evidence.push(EvidenceItem {
                    kind: "luminai_changeset_unconfigured".to_string(),
                    path: root.display().to_string(),
                    line: None,
                    detail: detail.clone(),
                });
                warnings.push(format!("luminai_changeset_unconfigured: {detail}"));
            }
        },
    }
    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_luminai_health_audit_isolation"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked {} Luminai surface(s), found {} isolation violation(s)",
            all.len(),
            warnings.len()
        ),
        confidence: if warnings.is_empty() { 0.98 } else { 0.68 },
        entities: Vec::new(),
        evidence,
        warnings,
        meta: Some(json!({"luminai_surfaces": all.len()})),
        timing_ms: started.elapsed().as_millis(),
    }
}
