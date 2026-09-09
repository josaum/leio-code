//! Binary node discovery — scans manifests to build the registry of binaries
//! that can be targets of cross-language spawn edges.
//!
//! Supported manifest types:
//! - `Cargo.toml` `[[bin]]` sections → [`BinaryNodeSource::CargoExplicit`]
//! - `Cargo.toml` implicit binary: `[package] name` + `src/main.rs` present
//!   → [`BinaryNodeSource::CargoImplicit`]
//! - `src/bin/<name>.rs` files → [`BinaryNodeSource::CargoBin`]
//! - `package.json` `"bin"` field → [`BinaryNodeSource::NpmBin`]
//! - `pyproject.toml` `[project.scripts]` → [`BinaryNodeSource::PyprojectScript`]
//!
//! Any IO error on an individual file is silently skipped; the scanner never
//! panics.

use std::path::Path;

use ignore::WalkBuilder;

use crate::model::{BinaryNode, BinaryNodeSource};

/// Walk `root` and collect all binary nodes from supported manifests.
pub fn collect_binary_nodes(root: &Path) -> Vec<BinaryNode> {
    let mut out = Vec::new();

    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            // Skip known heavy directories — same list as indexer.
            if entry.path().is_dir() {
                return !matches!(
                    name.as_ref(),
                    ".git"
                        | "node_modules"
                        | "target"
                        | ".next"
                        | "dist"
                        | "build"
                        | ".turbo"
                        | ".venv"
                        | "venv"
                        | "__pycache__"
                        | "backup"
                        | "downloads"
                        | "output"
                        | "site"
                        | "temp_ocr"
                        | "wheels"
                        | "models"
                        | "tmp"
                        | "projects"
                );
            }
            true
        })
        .build();

    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        match name {
            "Cargo.toml" => collect_cargo_nodes(root, path, &mut out),
            "package.json" => collect_npm_nodes(root, path, &mut out),
            "pyproject.toml" => collect_pyproject_nodes(root, path, &mut out),
            _ => {
                // Check for src/bin/<name>.rs pattern.
                if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    try_collect_cargo_bin(root, path, &mut out);
                }
            }
        }
    }

    out
}

/// Relative path from `root` to `path`, always using forward slashes.
fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Scan a `Cargo.toml` for explicit `[[bin]]` entries and implicit binaries
/// (when `src/main.rs` exists alongside the manifest).
fn collect_cargo_nodes(root: &Path, cargo_toml: &Path, out: &mut Vec<BinaryNode>) {
    let raw = match std::fs::read_to_string(cargo_toml) {
        Ok(raw) => raw,
        Err(_) => return,
    };
    let value: toml::Value = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return,
    };

    let manifest_rel = rel(root, cargo_toml);

    // [[bin]] entries — CargoExplicit.
    if let Some(bins) = value.get("bin").and_then(toml::Value::as_array) {
        for bin in bins {
            if let Some(name) = bin
                .get("name")
                .and_then(toml::Value::as_str)
                .filter(|s| !s.is_empty())
            {
                out.push(BinaryNode {
                    name: name.to_string(),
                    path: manifest_rel.clone(),
                    source: BinaryNodeSource::CargoExplicit,
                });
            }
        }
    }

    // Implicit binary from [package] name + src/main.rs presence — CargoImplicit.
    if let Some(pkg_name) = value
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(toml::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        let dir = cargo_toml.parent().unwrap_or(root);
        let main_rs = dir.join("src").join("main.rs");
        if main_rs.is_file() {
            out.push(BinaryNode {
                name: pkg_name.to_string(),
                path: rel(root, &main_rs),
                source: BinaryNodeSource::CargoImplicit,
            });
        }
    }
}

/// Check if `path` is `src/bin/<name>.rs` relative to any ancestor `Cargo.toml`
/// and emit a `CargoBin` node.
fn try_collect_cargo_bin(root: &Path, path: &Path, out: &mut Vec<BinaryNode>) {
    // The file must be named `src/bin/<stem>.rs` — verify the two-level parent.
    let parent = match path.parent() {
        Some(p) => p,
        None => return,
    };
    if parent.file_name().and_then(|n| n.to_str()) != Some("bin") {
        return;
    }
    let src_dir = match parent.parent() {
        Some(p) => p,
        None => return,
    };
    if src_dir.file_name().and_then(|n| n.to_str()) != Some("src") {
        return;
    }

    let stem = match path.file_stem().and_then(|s| s.to_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return,
    };

    out.push(BinaryNode {
        name: stem.to_string(),
        path: rel(root, path),
        source: BinaryNodeSource::CargoBin,
    });
}

/// Scan a `package.json` for the `"bin"` field.
fn collect_npm_nodes(root: &Path, pkg_json: &Path, out: &mut Vec<BinaryNode>) {
    let raw = match std::fs::read_to_string(pkg_json) {
        Ok(raw) => raw,
        Err(_) => return,
    };
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return,
    };

    let manifest_rel = rel(root, pkg_json);

    match value.get("bin") {
        // "bin": { "cmd": "./index.js", ... } — each key is a binary name.
        Some(serde_json::Value::Object(map)) => {
            for key in map.keys() {
                if !key.is_empty() {
                    out.push(BinaryNode {
                        name: key.clone(),
                        path: manifest_rel.clone(),
                        source: BinaryNodeSource::NpmBin,
                    });
                }
            }
        }
        // "bin": "./index.js" — the package name is the binary name.
        Some(serde_json::Value::String(_)) => {
            if let Some(pkg_name) = value
                .get("name")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
            {
                out.push(BinaryNode {
                    name: pkg_name.to_string(),
                    path: manifest_rel,
                    source: BinaryNodeSource::NpmBin,
                });
            }
        }
        _ => {}
    }
}

/// Scan a `pyproject.toml` for `[project.scripts]` entries.
fn collect_pyproject_nodes(root: &Path, pyproject: &Path, out: &mut Vec<BinaryNode>) {
    let raw = match std::fs::read_to_string(pyproject) {
        Ok(raw) => raw,
        Err(_) => return,
    };
    let value: toml::Value = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return,
    };

    let manifest_rel = rel(root, pyproject);

    if let Some(scripts) = value
        .get("project")
        .and_then(|p| p.get("scripts"))
        .and_then(toml::Value::as_table)
    {
        for key in scripts.keys() {
            if !key.is_empty() {
                out.push(BinaryNode {
                    name: key.clone(),
                    path: manifest_rel.clone(),
                    source: BinaryNodeSource::PyprojectScript,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(root: &std::path::Path, rel: &str, body: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, body).expect("write");
    }

    #[test]
    fn cargo_explicit_bin_section() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "Cargo.toml",
            "[package]\nname = \"myapp\"\nversion = \"0.1.0\"\n\n[[bin]]\nname = \"mytool\"\npath = \"src/main.rs\"\n",
        );
        let bins = collect_binary_nodes(tmp.path());
        // Should find CargoExplicit "mytool" and CargoImplicit "myapp" (src/main.rs absent)
        let explicit: Vec<_> = bins
            .iter()
            .filter(|b| b.source == BinaryNodeSource::CargoExplicit)
            .collect();
        assert_eq!(explicit.len(), 1);
        assert_eq!(explicit[0].name, "mytool");
        assert_eq!(explicit[0].path, "Cargo.toml");
    }

    #[test]
    fn cargo_implicit_from_main_rs() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "Cargo.toml",
            "[package]\nname = \"myapp\"\nversion = \"0.1.0\"\n",
        );
        write(tmp.path(), "src/main.rs", "fn main() {}\n");

        let bins = collect_binary_nodes(tmp.path());
        let implicit: Vec<_> = bins
            .iter()
            .filter(|b| b.source == BinaryNodeSource::CargoImplicit)
            .collect();
        assert_eq!(implicit.len(), 1);
        assert_eq!(implicit[0].name, "myapp");
        assert_eq!(implicit[0].path, "src/main.rs");
    }

    #[test]
    fn cargo_bin_dir() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "src/bin/tool.rs", "fn main() {}\n");
        write(tmp.path(), "src/bin/other.rs", "fn main() {}\n");

        let bins = collect_binary_nodes(tmp.path());
        let cargo_bins: Vec<_> = bins
            .iter()
            .filter(|b| b.source == BinaryNodeSource::CargoBin)
            .collect();
        let mut names: Vec<_> = cargo_bins.iter().map(|b| b.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["other", "tool"]);
    }

    #[test]
    fn npm_bin_object() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "package.json",
            r#"{"name":"myapp","bin":{"my-cli":"./index.js"}}"#,
        );
        let bins = collect_binary_nodes(tmp.path());
        let npm: Vec<_> = bins
            .iter()
            .filter(|b| b.source == BinaryNodeSource::NpmBin)
            .collect();
        assert_eq!(npm.len(), 1);
        assert_eq!(npm[0].name, "my-cli");
        assert_eq!(npm[0].path, "package.json");
    }

    #[test]
    fn npm_bin_string() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "package.json",
            r#"{"name":"my-pkg","bin":"./index.js"}"#,
        );
        let bins = collect_binary_nodes(tmp.path());
        let npm: Vec<_> = bins
            .iter()
            .filter(|b| b.source == BinaryNodeSource::NpmBin)
            .collect();
        assert_eq!(npm.len(), 1);
        assert_eq!(npm[0].name, "my-pkg");
    }

    #[test]
    fn pyproject_scripts() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "pyproject.toml",
            "[project]\nname = \"mypkg\"\n\n[project.scripts]\nmytool = \"mypkg.cli:main\"\n",
        );
        let bins = collect_binary_nodes(tmp.path());
        let py: Vec<_> = bins
            .iter()
            .filter(|b| b.source == BinaryNodeSource::PyprojectScript)
            .collect();
        assert_eq!(py.len(), 1);
        assert_eq!(py[0].name, "mytool");
        assert_eq!(py[0].path, "pyproject.toml");
    }

    #[test]
    fn malformed_manifest_skipped() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "Cargo.toml", "this is not valid toml {{{{");
        let bins = collect_binary_nodes(tmp.path());
        assert!(bins.is_empty(), "malformed manifest must not panic or emit");
    }
}
