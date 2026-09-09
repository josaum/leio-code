//! Bare cartridge Redis-key fallback doctor.
//!
//! `example/cartridges/registry.py:get_config()` historically falls back to a tenant-less
//! Redis key (`cartridge:{agent_id}`) when the tenant-scoped key is not present. Redis
//! keys with that shape are a cross-tenant lookup risk: any tenant whose namespace happens
//! to share an agent slug with another tenant could read the wrong config.
//!
//! On 2026-05-04 we removed the live bare keys from prod Redis but the *fallback path*
//! is still in code. This doctor flags the bare-key fallback construction so the next
//! refactor pass can delete it.
//!
//! Static check only — no Redis connection.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct BareCartridgeRedisKeyDoctor;

impl Doctor for BareCartridgeRedisKeyDoctor {
    fn name(&self) -> &'static str {
        "bare-cartridge-redis-key"
    }

    fn description(&self) -> &'static str {
        "Flags the tenant-less `cartridge:{agent_id}` Redis-key fallback in `example/cartridges/registry.py` as a cross-tenant lookup risk."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_bare_cartridge_redis_key(root)
    }
}

const REL_PATH: &str = "example-api/example/cartridges/registry.py";

pub fn doctor_bare_cartridge_redis_key(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let path = root.join(REL_PATH);
    let body = if path.is_file() {
        let mut io = Vec::new();
        match read_text(&path, &mut io) {
            Some(b) => b,
            None => {
                warnings.extend(io);
                return finalize(
                    started,
                    warnings,
                    entities,
                    evidence,
                    "could not read registry.py",
                );
            }
        }
    } else {
        return finalize(
            started,
            warnings,
            entities,
            evidence,
            "registry.py not found",
        );
    };

    // Look for the bare-key construction. The historical line is:
    //   keys.append(f"{prefix}:{agent_id.lower()}")
    // matched as: a `keys.append(...)` line whose f-string body has only ONE colon
    // before `{agent_id`. The tenant-scoped variant has TWO colons (prefix:tenant:agent).
    let mut hits = 0usize;
    for (idx, line) in body.lines().enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("keys.append(") {
            continue;
        }
        // Cheap fingerprint of the bare-key fallback: f-string with exactly one colon
        // between `{prefix}` and `{agent_id`.
        if line.contains("{prefix}:{agent_id") && !line.contains("{prefix}:{tenant_id}") {
            hits += 1;
            warnings.push(format!(
                "{}:{}: bare cartridge Redis key fallback `{{prefix}}:{{agent_id}}` is a cross-tenant lookup risk -- delete this fallback after confirming all tenant-scoped keys are populated",
                REL_PATH,
                idx + 1
            ));
            evidence.push(EvidenceItem {
                kind: "bare_cartridge_redis_key_fallback".to_string(),
                path: REL_PATH.to_string(),
                line: Some(idx + 1),
                detail: line.trim().to_string(),
            });
        }
    }

    entities.push(json!({
        "doctor": "bare-cartridge-redis-key",
        "registry_present": true,
        "fallback_hits": hits,
    }));

    let summary = if warnings.is_empty() {
        "no bare cartridge Redis-key fallback found in registry.py".to_string()
    } else {
        format!(
            "found {} bare cartridge Redis-key fallback line(s) in registry.py",
            hits
        )
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_bare_cartridge_redis_key"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.95 } else { 0.6 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn finalize(
    started: Instant,
    warnings: Vec<String>,
    entities: Vec<serde_json::Value>,
    evidence: Vec<EvidenceItem>,
    summary: &str,
) -> QueryEnvelope {
    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_bare_cartridge_redis_key"),
        kind: "doctor".to_string(),
        summary: summary.to_string(),
        confidence: 0.9,
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_repo(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-bare-redis-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn fallback_present_yields_warning() {
        let root = temp_repo("present");
        write(
            &root,
            REL_PATH,
            r#"def get_config(agent_id, *, tenant_id=None):
    keys = []
    if tenant_id:
        keys.append(f"{prefix}:{tenant_id}:{agent_id.lower()}")
    keys.append(f"{prefix}:{agent_id.lower()}")
"#,
        );
        let env = doctor_bare_cartridge_redis_key(&root);
        assert_eq!(env.warnings.len(), 1, "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn tenant_only_paths_are_silent() {
        let root = temp_repo("tenant-only");
        write(
            &root,
            REL_PATH,
            r#"def get_config(agent_id, *, tenant_id=None):
    keys = []
    if tenant_id:
        keys.append(f"{prefix}:{tenant_id}:{agent_id.lower()}")
"#,
        );
        let env = doctor_bare_cartridge_redis_key(&root);
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }
}
