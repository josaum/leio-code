use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{AccessKind, EvidenceItem, QueryEnvelope, RepoIndex, SourceLanguage};

pub struct RedisKeyHygieneDoctor;

impl Doctor for RedisKeyHygieneDoctor {
    fn name(&self) -> &'static str {
        "redis-key-hygiene"
    }

    fn description(&self) -> &'static str {
        "Checks Redis key patterns: TTL presence, namespace prefixes, and key pattern collisions across modules."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_redis_key_hygiene(index, root)
    }
}

pub fn doctor_redis_key_hygiene(index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let mut no_ttl_count = 0usize;
    let mut no_prefix_count = 0usize;

    // Track key patterns by module to detect collisions
    let mut key_patterns_by_module: HashMap<String, Vec<(String, String)>> = HashMap::new();

    for file in &index.files {
        if !matches!(
            file.language,
            SourceLanguage::Python | SourceLanguage::Rust | SourceLanguage::TypeScript
        ) {
            continue;
        }

        let module = file.path.split('/').next().unwrap_or("unknown").to_string();

        // Check for SET commands without TTL
        let write_keys: Vec<_> = file
            .redis_keys
            .iter()
            .filter(|k| k.access == AccessKind::Write)
            .collect();

        if write_keys.is_empty() {
            continue;
        }

        let content = match read_text(&root.join(&file.path), &mut Vec::new()) {
            Some(c) => c,
            None => continue,
        };

        for key_occ in &write_keys {
            // Record key pattern for collision detection
            let pattern = extract_key_prefix(&key_occ.key);
            key_patterns_by_module
                .entry(module.clone())
                .or_default()
                .push((pattern.clone(), key_occ.key.clone()));

            // Check namespace prefix (should have at least one colon separator)
            if !key_occ.key.contains(':') {
                no_prefix_count += 1;
                warnings.push(format!(
                    "Redis key `{}` at {}:{} has no namespace prefix (missing `:` separator)",
                    key_occ.key, key_occ.path, key_occ.line
                ));
                evidence.push(EvidenceItem {
                    kind: "redis_no_prefix".to_string(),
                    path: key_occ.path.clone(),
                    line: Some(key_occ.line),
                    detail: format!("key `{}` has no namespace prefix", key_occ.key),
                });
            }
        }

        // Scan for SET without EX/PX/EXAT around the key's line
        for key_occ in &write_keys {
            // Look at the line and surrounding context for TTL
            let lines: Vec<&str> = content.lines().collect();
            let line_idx = key_occ.line.saturating_sub(1);
            let context_start = line_idx.saturating_sub(2);
            let context_end = (line_idx + 3).min(lines.len());
            let context_window = &lines[context_start..context_end];
            let context_text = context_window.join(" ").to_ascii_lowercase();

            let is_set = context_text.contains("set(")
                || context_text.contains(".set(")
                || context_text.contains("hset")
                || context_text.contains("lpush")
                || context_text.contains("rpush");

            if is_set {
                let has_ttl = context_text.contains("ex=")
                    || context_text.contains("px=")
                    || context_text.contains("exat=")
                    || context_text.contains("expire")
                    || context_text.contains("pexpire")
                    || context_text.contains("setex")
                    || context_text.contains("psetex")
                    || context_text.contains("ttl")
                    || context_text.contains("timeout");

                if !has_ttl {
                    no_ttl_count += 1;
                    warnings.push(format!(
                        "Redis SET for key `{}` at {}:{} has no apparent TTL (no EX/PX/EXAT/expire nearby)",
                        key_occ.key, key_occ.path, key_occ.line
                    ));
                    evidence.push(EvidenceItem {
                        kind: "redis_no_ttl".to_string(),
                        path: key_occ.path.clone(),
                        line: Some(key_occ.line),
                        detail: format!("SET `{}` without TTL", key_occ.key),
                    });
                }
            }
        }
    }

    // Detect key pattern collisions across modules
    let mut all_patterns: HashMap<String, Vec<String>> = HashMap::new();
    for (module, patterns) in &key_patterns_by_module {
        for (prefix, _full_key) in patterns {
            all_patterns
                .entry(prefix.clone())
                .or_default()
                .push(module.clone());
        }
    }
    let mut collision_count = 0usize;
    for (prefix, modules) in &all_patterns {
        let mut unique_modules: Vec<String> = modules.clone();
        unique_modules.sort();
        unique_modules.dedup();
        if unique_modules.len() > 1 {
            collision_count += 1;
            warnings.push(format!(
                "Redis key prefix `{}` used across multiple modules: {}",
                prefix,
                unique_modules.join(", ")
            ));
        }
    }

    entities.push(json!({
        "no_ttl_count": no_ttl_count,
        "no_prefix_count": no_prefix_count,
        "collision_count": collision_count,
        "modules_scanned": key_patterns_by_module.keys().collect::<Vec<_>>(),
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_redis_key_hygiene"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked Redis key hygiene: {} without TTL, {} without prefix, {} cross-module collisions",
            no_ttl_count, no_prefix_count, collision_count
        ),
        confidence: if warnings.is_empty() { 0.9 } else { 0.65 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn extract_key_prefix(key: &str) -> String {
    // Extract the prefix before the first dynamic segment (contains {})
    if let Some(idx) = key.find('{') {
        key[..idx].trim_end_matches(':').to_string()
    } else if let Some(idx) = key.find(':') {
        key[..idx].to_string()
    } else {
        key.to_string()
    }
}
