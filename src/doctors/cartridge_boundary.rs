use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex, SourceLanguage};

pub struct CartridgeBoundaryDoctor;

impl Doctor for CartridgeBoundaryDoctor {
    fn name(&self) -> &'static str {
        "cartridge-boundary"
    }

    fn description(&self) -> &'static str {
        "Checks cartridge isolation: no cross-cartridge imports and no core importing cartridge-specific logic."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_cartridge_boundary(index, root)
    }
}

pub fn doctor_cartridge_boundary(index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    // Collect all cartridge names from directory listing
    let cartridge_dir = root.join("cartridges");
    let mut cartridge_names: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&cartridge_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir()
                && let Some(name) = path.file_name().and_then(|n| n.to_str())
                && !name.starts_with('.')
                && name != "__pycache__"
            {
                cartridge_names.push(name.to_string());
            }
        }
    }
    cartridge_names.sort();

    let allowlist = load_cross_import_allowlist(root, &mut warnings);
    let mut cross_imports = 0usize;
    let mut allowed_cross_imports = 0usize;
    let mut core_imports = 0usize;
    let mut allowed_evidence: Vec<EvidenceItem> = Vec::new();

    // Scan Python files for cross-cartridge imports
    for file in &index.files {
        if file.language != SourceLanguage::Python {
            continue;
        }

        // Determine which cartridge this file belongs to (if any)
        let own_cartridge = cartridge_names
            .iter()
            .find(|c| file.path.starts_with(&format!("cartridges/{}/", c)));

        let content = match read_text(&root.join(&file.path), &mut Vec::new()) {
            Some(c) => c,
            None => continue,
        };

        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if !trimmed.starts_with("import ") && !trimmed.starts_with("from ") {
                continue;
            }

            for cartridge in &cartridge_names {
                let import_pattern = format!("cartridges.{}", cartridge);
                let import_pattern2 = format!("cartridges/{}", cartridge);

                if !trimmed.contains(&import_pattern) && !trimmed.contains(&import_pattern2) {
                    continue;
                }

                if let Some(own) = own_cartridge {
                    if *own != *cartridge {
                        if let Some(reason) = allowlist.get(&(own.clone(), cartridge.clone())) {
                            allowed_cross_imports += 1;
                            allowed_evidence.push(EvidenceItem {
                                kind: "allowed_cross_cartridge_import".to_string(),
                                path: file.path.clone(),
                                line: Some(idx + 1),
                                detail: format!(
                                    "cartridge `{}` imports `{}` (allowlisted: {})",
                                    own, cartridge, reason
                                ),
                            });
                        } else {
                            cross_imports += 1;
                            warnings.push(format!(
                                "cartridge `{}` imports from cartridge `{}` at {}:{}",
                                own,
                                cartridge,
                                file.path,
                                idx + 1
                            ));
                            evidence.push(EvidenceItem {
                                kind: "cross_cartridge_import".to_string(),
                                path: file.path.clone(),
                                line: Some(idx + 1),
                                detail: format!("cartridge `{}` imports `{}`", own, cartridge),
                            });
                        }
                    }
                } else {
                    // Non-cartridge code importing cartridge logic
                    let is_core = file.path.starts_with("example-api/example/core")
                        || file.path.starts_with("example-gateway/src/");
                    if is_core {
                        core_imports += 1;
                        warnings.push(format!(
                            "core code imports cartridge `{}` at {}:{}",
                            cartridge,
                            file.path,
                            idx + 1
                        ));
                        evidence.push(EvidenceItem {
                            kind: "core_cartridge_import".to_string(),
                            path: file.path.clone(),
                            line: Some(idx + 1),
                            detail: format!("core code imports cartridge `{}`", cartridge),
                        });
                    }
                }
            }
        }
    }

    entities.push(json!({
        "cartridges": cartridge_names,
        "cross_cartridge_imports": cross_imports,
        "allowed_cross_cartridge_imports": allowed_cross_imports,
        "core_cartridge_imports": core_imports,
        "allowlist_size": allowlist.len(),
    }));

    evidence.extend(allowed_evidence);

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_cartridge_boundary"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked {} cartridge boundaries, found {} cross-imports ({} allowlisted) and {} core-imports",
            cartridge_names.len(),
            cross_imports,
            allowed_cross_imports,
            core_imports
        ),
        confidence: if warnings.is_empty() { 0.95 } else { 0.7 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

/// Load `(from, to)` cartridge import pairs that are intentional.
///
/// Looks for `cartridges/cross-imports.allow.toml` first, then
/// `.leio-code/cross-imports.allow.toml`. Format:
///
/// ```toml
/// [[allow]]
/// from = "gohosptwin"
/// to = "health_audit"
/// reason = "shared TISS contract types"
/// ```
fn load_cross_import_allowlist(
    root: &Path,
    warnings: &mut Vec<String>,
) -> HashMap<(String, String), String> {
    let candidates = [
        root.join("cartridges/cross-imports.allow.toml"),
        root.join(".leio-code/cross-imports.allow.toml"),
    ];
    let path = match candidates.iter().find(|p| p.exists()) {
        Some(p) => p,
        None => return HashMap::new(),
    };

    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) => {
            warnings.push(format!(
                "failed to read cross-import allowlist at {}: {err}",
                path.display()
            ));
            return HashMap::new();
        }
    };

    let parsed: toml::Value = match toml::from_str(&raw) {
        Ok(value) => value,
        Err(err) => {
            warnings.push(format!(
                "failed to parse cross-import allowlist at {}: {err}",
                path.display()
            ));
            return HashMap::new();
        }
    };

    let mut allow = HashMap::new();
    if let Some(items) = parsed.get("allow").and_then(|v| v.as_array()) {
        for entry in items {
            let from = entry.get("from").and_then(|v| v.as_str());
            let to = entry.get("to").and_then(|v| v.as_str());
            let reason = entry.get("reason").and_then(|v| v.as_str()).map(str::trim);
            if let (Some(from), Some(to)) = (from, to) {
                if let Some(reason) = reason.filter(|value| !value.is_empty()) {
                    allow.insert((from.to_string(), to.to_string()), reason.to_string());
                } else {
                    warnings.push(format!(
                        "allowlist entry in {} for `{}` -> `{}` missing non-empty `reason`",
                        path.display(),
                        from,
                        to
                    ));
                }
            } else {
                warnings.push(format!(
                    "allowlist entry in {} missing `from` or `to`",
                    path.display()
                ));
            }
        }
    }
    allow
}

#[cfg(test)]
mod tests {
    use super::load_cross_import_allowlist;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_repo(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "leio-code-cartridge-{name}-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("cartridges")).expect("create temp cartridges dir");
        root
    }

    #[test]
    fn allowlist_preserves_reasons_for_evidence() {
        let root = temp_repo("reason");
        fs::write(
            root.join("cartridges/cross-imports.allow.toml"),
            r#"
[[allow]]
from = "revops"
to = "pacto"
reason = "provider adapter owns this dependency"
"#,
        )
        .expect("write allowlist");

        let mut warnings = Vec::new();
        let allowlist = load_cross_import_allowlist(&root, &mut warnings);

        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            allowlist.get(&("revops".to_string(), "pacto".to_string())),
            Some(&"provider adapter owns this dependency".to_string())
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn allowlist_requires_reason() {
        let root = temp_repo("missing-reason");
        fs::write(
            root.join("cartridges/cross-imports.allow.toml"),
            r#"
[[allow]]
from = "revops"
to = "pacto"
"#,
        )
        .expect("write allowlist");

        let mut warnings = Vec::new();
        let allowlist = load_cross_import_allowlist(&root, &mut warnings);

        assert!(allowlist.is_empty());
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("missing non-empty `reason`"))
        );

        let _ = fs::remove_dir_all(root);
    }
}
