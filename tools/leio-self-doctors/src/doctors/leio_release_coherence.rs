//! Coherence gate for first-party LEIO Code release versions.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id};
use super::version_manifest::{VersionKind, VersionManifest, version_from_source, version_line};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

const RELEASE_SURFACES: &[VersionManifest<'static>] = &[
    VersionManifest {
        path: "crates/leio-harness/Cargo.toml",
        kind: VersionKind::CargoPackage,
        label: "Harness crate manifest",
    },
    VersionManifest {
        path: "crates/leio-knowledge-core/Cargo.toml",
        kind: VersionKind::CargoPackage,
        label: "Knowledge core crate manifest",
    },
    VersionManifest {
        path: "Cargo.lock",
        kind: VersionKind::CargoLockPackage("leio-harness"),
        label: "Harness lockfile package",
    },
    VersionManifest {
        path: "Cargo.lock",
        kind: VersionKind::CargoLockPackage("leio-knowledge-core"),
        label: "Knowledge core lockfile package",
    },
    VersionManifest {
        path: "mcp/package-lock.json",
        kind: VersionKind::JsonPackage,
        label: "MCP package lock top-level version",
    },
    VersionManifest {
        path: "apps-sdk/package-lock.json",
        kind: VersionKind::JsonPackage,
        label: "Apps SDK package lock top-level version",
    },
    VersionManifest {
        path: "Cargo.toml",
        kind: VersionKind::CargoPackage,
        label: "Rust crate manifest",
    },
    VersionManifest {
        path: "Cargo.lock",
        kind: VersionKind::CargoLockPackage("leio-code"),
        label: "Rust lockfile package",
    },
    VersionManifest {
        path: "mcp/package.json",
        kind: VersionKind::JsonPackage,
        label: "MCP package manifest",
    },
    VersionManifest {
        path: "mcp/package-lock.json",
        kind: VersionKind::NpmPackageLockRoot,
        label: "MCP package lock root",
    },
    VersionManifest {
        path: "apps-sdk/package.json",
        kind: VersionKind::JsonPackage,
        label: "Apps SDK package manifest",
    },
    VersionManifest {
        path: "apps-sdk/package-lock.json",
        kind: VersionKind::NpmPackageLockRoot,
        label: "Apps SDK package lock root",
    },
    VersionManifest {
        path: ".codex-plugin/plugin.json",
        kind: VersionKind::JsonPackage,
        label: "Codex plugin manifest",
    },
    VersionManifest {
        path: ".claude-plugin/plugin.json",
        kind: VersionKind::JsonPackage,
        label: "Claude plugin manifest",
    },
    VersionManifest {
        path: ".chatgpt-plugin/plugin.json",
        kind: VersionKind::JsonPackage,
        label: "ChatGPT plugin manifest",
    },
    VersionManifest {
        path: "gemini-extension.json",
        kind: VersionKind::JsonPackage,
        label: "Gemini extension manifest",
    },
    VersionManifest {
        path: "manifest.json",
        kind: VersionKind::JsonPackage,
        label: "desktop extension manifest",
    },
];

pub struct LeioReleaseCoherenceDoctor;

impl Doctor for LeioReleaseCoherenceDoctor {
    fn name(&self) -> &'static str {
        "leio-release-coherence"
    }

    fn description(&self) -> &'static str {
        "Checks exact first-party LEIO crate, npm, plugin, extension, product, lockfile, and changelog versions without touching independent dependency or wheel versions."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_leio_release_coherence(root)
    }
}

pub fn doctor_leio_release_coherence(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let Some(package_root) = locate_leio_root(root) else {
        return inactive_envelope(started);
    };

    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut entities = Vec::new();

    let authoritative_path = package_root.join("Cargo.toml");
    let authoritative_source = match std::fs::read_to_string(&authoritative_path) {
        Ok(source) => source,
        Err(error) => {
            release_warn(&mut warnings, format!("could not read Cargo.toml: {error}"));
            String::new()
        }
    };
    let authoritative = version_from_source(&authoritative_source, VersionKind::CargoPackage);
    if authoritative.is_none() {
        release_warn(
            &mut warnings,
            "could not parse authoritative package.version from Cargo.toml",
        );
    }

    for manifest in RELEASE_SURFACES {
        let display = display_path(root, &package_root, manifest.path);
        let path = package_root.join(manifest.path);
        let source = match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                release_warn(
                    &mut warnings,
                    format!(
                        "{} `{display}` is missing or unreadable: {error}",
                        manifest.label
                    ),
                );
                continue;
            }
        };
        let actual = version_from_source(&source, manifest.kind);
        match actual.as_deref() {
            Some(actual) => {
                evidence.push(EvidenceItem {
                    kind: "leio_release_version".to_string(),
                    path: display.clone(),
                    line: version_line(&source, manifest.kind),
                    detail: format!(
                        "{} declares {actual}; authoritative Cargo version is {}",
                        manifest.label,
                        authoritative.as_deref().unwrap_or("unknown")
                    ),
                });
                entities.push(json!({
                    "path": display,
                    "label": manifest.label,
                    "version": actual,
                    "authoritative_version": authoritative,
                }));
                if let Some(expected) = authoritative.as_deref()
                    && actual != expected
                {
                    release_warn(
                        &mut warnings,
                        format!(
                            "{} `{}` declares version {actual}, expected {expected}",
                            manifest.label,
                            display_path(root, &package_root, manifest.path)
                        ),
                    );
                }
            }
            None => release_warn(
                &mut warnings,
                format!(
                    "could not parse {} version from `{display}`",
                    manifest.label
                ),
            ),
        }
    }

    let changelog_display = display_path(root, &package_root, "CHANGELOG.md");
    match std::fs::read_to_string(package_root.join("CHANGELOG.md")) {
        Ok(changelog) => {
            if let Some(version) = authoritative.as_deref() {
                let heading = format!("## [{version}]");
                if let Some(line) = find_line(&changelog, &heading) {
                    evidence.push(EvidenceItem {
                        kind: "leio_release_changelog".to_string(),
                        path: changelog_display.clone(),
                        line: Some(line),
                        detail: format!("changelog contains release heading {heading}"),
                    });
                } else {
                    release_warn(
                        &mut warnings,
                        format!("`{changelog_display}` is missing release heading `{heading}`"),
                    );
                }
            }
        }
        Err(error) => release_warn(
            &mut warnings,
            format!("`{changelog_display}` is missing or unreadable: {error}"),
        ),
    }

    let warning_count = warnings.len();
    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_leio_release_coherence"),
        kind: "doctor".to_string(),
        summary: if warning_count == 0 {
            format!(
                "leio-release-coherence: all first-party release surfaces are aligned at {}",
                authoritative.as_deref().unwrap_or("unknown")
            )
        } else {
            format!("leio-release-coherence: found {warning_count} release warning(s)")
        },
        confidence: if warning_count == 0 { 0.99 } else { 0.75 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "activated": true,
            "package_root": display_path(root, &package_root, ""),
            "authoritative_version": authoritative,
            "release_surfaces": RELEASE_SURFACES.len(),
            "warning_count": warning_count,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn inactive_envelope(started: Instant) -> QueryEnvelope {
    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_leio_release_coherence"),
        kind: "doctor".to_string(),
        summary: "leio-release-coherence: no LEIO package manifests; inactive".to_string(),
        confidence: 0.98,
        entities: Vec::new(),
        evidence: Vec::new(),
        warnings: Vec::new(),
        meta: Some(json!({ "activated": false })),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn release_warn(warnings: &mut Vec<String>, message: impl AsRef<str>) {
    warnings.push(format!("[leio-release-coherence] {}", message.as_ref()));
}

fn locate_leio_root(root: &Path) -> Option<PathBuf> {
    for candidate in [root.to_path_buf(), root.join("leio-code")] {
        let Ok(source) = std::fs::read_to_string(candidate.join("Cargo.toml")) else {
            continue;
        };
        let package_name = toml::from_str::<toml::Value>(&source)
            .ok()
            .and_then(|value| {
                value
                    .get("package")?
                    .get("name")?
                    .as_str()
                    .map(str::to_owned)
            });
        if version_from_source(&source, VersionKind::CargoPackage).is_some()
            && package_name.as_deref() == Some("leio-code")
        {
            return Some(candidate);
        }
    }
    None
}

fn display_path(root: &Path, package_root: &Path, relative: &str) -> String {
    let path = package_root.join(relative);
    path.strip_prefix(root)
        .unwrap_or(&path)
        .to_string_lossy()
        .trim_end_matches('/')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "leio-release-coherence-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create fixture root");
        root
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create fixture parent");
        }
        fs::write(path, contents).expect("write fixture");
    }

    fn write_release(root: &Path, version: &str) {
        write(
            &root.join("Cargo.toml"),
            &format!("[package]\nname = \"leio-code\"\nversion = \"{version}\"\n"),
        );
        write(
            &root.join("Cargo.lock"),
            &format!(
                "[[package]]\nname = \"router\"\nversion = \"2.2.0\"\n\n[[package]]\nname = \"leio-code\"\nversion = \"{version}\"\n\n[[package]]\nname = \"leio-harness\"\nversion = \"{version}\"\n\n[[package]]\nname = \"leio-knowledge-core\"\nversion = \"{version}\"\n"
            ),
        );
        for package in ["leio-harness", "leio-knowledge-core"] {
            write(
                &root.join(format!("crates/{package}/Cargo.toml")),
                &format!("[package]\nname = \"{package}\"\nversion = \"{version}\"\n"),
            );
        }
        for package in ["mcp", "apps-sdk"] {
            write(
                &root.join(format!("{package}/package.json")),
                &format!("{{\"name\":\"@example/{package}\",\"version\":\"{version}\"}}"),
            );
            write(
                &root.join(format!("{package}/package-lock.json")),
                &format!(
                    "{{\"version\":\"{version}\",\"packages\":{{\"\":{{\"version\":\"{version}\"}},\"node_modules/router\":{{\"version\":\"2.2.0\"}}}}}}"
                ),
            );
        }
        for manifest in [
            ".codex-plugin/plugin.json",
            ".claude-plugin/plugin.json",
            ".chatgpt-plugin/plugin.json",
            "gemini-extension.json",
            "manifest.json",
        ] {
            write(
                &root.join(manifest),
                &format!("{{\"name\":\"leio-code\",\"version\":\"{version}\"}}"),
            );
        }
        write(
            &root.join("CHANGELOG.md"),
            &format!("# Changelog\n\n## [{version}] — 2026-07-12\n"),
        );
    }

    fn activated(envelope: &QueryEnvelope) -> bool {
        envelope
            .meta
            .as_ref()
            .and_then(|meta| meta.get("activated"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }

    #[test]
    fn no_leio_manifests_is_an_inactive_clean_skip() {
        let root = temp_root("absent");
        let envelope = doctor_leio_release_coherence(&root);
        assert!(envelope.warnings.is_empty());
        assert!(!activated(&envelope));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn matching_first_party_surfaces_and_changelog_are_clean() {
        let root = temp_root("clean");
        write_release(&root, "2.3.0");
        let envelope = doctor_leio_release_coherence(&root);
        assert!(activated(&envelope));
        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
        assert!(envelope.summary.contains("aligned"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn mismatched_plugin_and_missing_changelog_entry_warn() {
        let root = temp_root("drift");
        write_release(&root, "2.3.0");
        write(
            &root.join(".codex-plugin/plugin.json"),
            "{\"name\":\"leio-code\",\"version\":\"2.2.0\"}",
        );
        write(&root.join("CHANGELOG.md"), "# Changelog\n\n## [2.2.0]\n");

        let warnings = doctor_leio_release_coherence(&root).warnings.join("\n");
        assert!(warnings.contains(".codex-plugin/plugin.json"));
        assert!(warnings.contains("2.2.0"));
        assert!(warnings.contains("CHANGELOG.md"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn npm_manifest_drift_is_reported_for_each_provider() {
        for package in ["mcp", "apps-sdk"] {
            let root = temp_root(package);
            write_release(&root, "2.6.2");
            let path = format!("{package}/package.json");
            write(&root.join(&path), r#"{"version":"2.6.1"}"#);
            let envelope = doctor_leio_release_coherence(&root);
            assert_eq!(envelope.warnings.len(), 1, "{:?}", envelope.warnings);
            let warning = &envelope.warnings[0];
            assert!(warning.contains(&path));
            assert!(warning.contains("2.6.1, expected 2.6.2"));
            fs::remove_dir_all(root).ok();
        }
    }

    #[test]
    fn npm_lock_checks_top_level_and_root_package_independently() {
        for package in ["mcp", "apps-sdk"] {
            for (top, nested) in [("2.6.1", "2.6.2"), ("2.6.2", "2.6.1")] {
                let root = temp_root("npm-lock-drift");
                write_release(&root, "2.6.2");
                let path = format!("{package}/package-lock.json");
                write(
                    &root.join(&path),
                    &format!(r#"{{"version":"{top}","packages":{{"":{{"version":"{nested}"}}}}}}"#),
                );
                let envelope = doctor_leio_release_coherence(&root);
                assert_eq!(envelope.warnings.len(), 1, "{:?}", envelope.warnings);
                assert!(envelope.warnings[0].contains(&path));
                assert!(envelope.warnings[0].contains("2.6.1, expected 2.6.2"));
                fs::remove_dir_all(root).ok();
            }
        }
    }

    #[test]
    fn rust_member_manifest_and_lock_drift_are_reported_independently() {
        for package in ["leio-harness", "leio-knowledge-core"] {
            for manifest_drift in [true, false] {
                let root = temp_root("rust-member-drift");
                write_release(&root, "2.6.2");
                let path = if manifest_drift {
                    format!("crates/{package}/Cargo.toml")
                } else {
                    "Cargo.lock".to_string()
                };
                let original = fs::read_to_string(root.join(&path)).unwrap();
                let source = original.replace(
                    &format!("name = \"{package}\"\nversion = \"2.6.2\""),
                    &format!("name = \"{package}\"\nversion = \"2.6.1\""),
                );
                assert_ne!(source, original);
                write(&root.join(&path), &source);
                let envelope = doctor_leio_release_coherence(&root);
                assert_eq!(envelope.warnings.len(), 1, "{:?}", envelope.warnings);
                assert!(envelope.warnings[0].contains(&path));
                assert!(envelope.warnings[0].contains("2.6.1, expected 2.6.2"));
                fs::remove_dir_all(root).ok();
            }
        }
    }

    #[test]
    fn monorepo_root_discovers_nested_leio_code_package() {
        let root = temp_root("monorepo");
        write_release(&root.join("leio-code"), "2.3.0");
        let envelope = doctor_leio_release_coherence(&root);
        assert!(activated(&envelope));
        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
        fs::remove_dir_all(root).ok();
    }
}
