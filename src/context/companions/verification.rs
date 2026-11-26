//! Checkout evidence for coverage and test maintenance in a Cargo package.
mod contracts;
mod coverage;

pub(super) use contracts::stale_expectations;
pub(super) use coverage::coverage_gaps;

use super::{RepoIndex, git, is_test_like_file_path, live_file, package};
use crate::model::SourceLanguage;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use tree_sitter::{Node, Parser};

const MAX_SOURCE_BYTES: usize = 256 * 1024;
const MAX_SCAN_BYTES: usize = 8 * 1024 * 1024;

#[derive(Default)]
pub(super) struct CargoScope {
    packages: BTreeSet<PathBuf>,
    // Package -> crate import name (including Cargo aliases) -> local package.
    dependencies: BTreeMap<PathBuf, BTreeMap<String, PathBuf>>,
}

impl CargoScope {
    pub(super) fn from_seeds(root: &Path, seeds: &[String]) -> Self {
        let mut scope = Self::default();
        let mut manifests = HashMap::new();
        for seed in seeds.iter().take(4) {
            let path = Path::new(seed);
            if path.extension().and_then(|s| s.to_str()) != Some("rs") {
                continue;
            }
            let dir = package(root, path, &mut manifests);
            if scope.dependencies.contains_key(dir) {
                continue;
            }
            let Some(manifest) = read_manifest(root, dir) else {
                continue;
            };
            if manifest.get("package").is_none() {
                continue;
            }
            scope.packages.insert(dir.to_owned());
            let deps = scope.dependencies.entry(dir.to_owned()).or_default();
            // Cargo resolves inherited dependency paths relative to the workspace manifest.
            let workspace_deps = std::cell::OnceCell::new();
            for table in ["dependencies", "dev-dependencies"] {
                for (name, dep) in manifest
                    .get(table)
                    .and_then(toml::Value::as_table)
                    .into_iter()
                    .flatten()
                {
                    let (base, dep) = if dep.get("workspace").and_then(toml::Value::as_bool)
                        == Some(true)
                    {
                        let workspace = workspace_deps.get_or_init(|| {
                            dir.ancestors().find_map(|owner| {
                                let manifest = read_manifest(root, owner)?;
                                let dependencies =
                                    manifest.get("workspace")?.get("dependencies")?.as_table()?;
                                Some((owner, dependencies.clone()))
                            })
                        });
                        let Some((owner, table)) = workspace else {
                            continue;
                        };
                        let Some(dependency) = table.get(name) else {
                            continue;
                        };
                        (*owner, dependency)
                    } else {
                        (dir, dep)
                    };
                    let Some(rel) = dep.get("path").and_then(toml::Value::as_str) else {
                        continue;
                    };
                    let Ok(target) = root.join(base).join(rel).canonicalize() else {
                        continue;
                    };
                    if let Ok(relative) = target.strip_prefix(root)
                        && live_file(root, &relative.join("Cargo.toml")).is_some()
                    {
                        scope.packages.insert(relative.to_owned());
                        deps.insert(name.replace('-', "_"), relative.to_owned());
                    }
                }
            }
        }
        scope
    }

    pub(super) fn bridge(&self, dir: &Path, source: &str) -> Option<&str> {
        let deps = self.dependencies.get(dir)?;
        if deps.is_empty() {
            return None;
        }
        let mut parser = rust_parser()?;
        let tree = parser.parse(source, None)?;
        let mut found = None;
        walk(tree.root_node(), &mut |node| {
            if matches!(
                node.kind(),
                "scoped_identifier" | "scoped_type_identifier" | "scoped_use_list"
            ) && let Some(lhs) = node.child_by_field_name("path")
                && let Some((name, _)) = deps.get_key_value(text(lhs, source))
            {
                found = Some(name.as_str());
            }
        });
        found
    }
}

fn read_manifest(root: &Path, dir: &Path) -> Option<toml::Value> {
    bounded_source(root, &dir.join("Cargo.toml"))?.parse().ok()
}

fn bounded_source(root: &Path, relative: &Path) -> Option<String> {
    let file = File::open(live_file(root, relative)?).ok()?;
    if file.metadata().ok()?.len() > MAX_SOURCE_BYTES as u64 {
        return None;
    }
    let mut source = String::new();
    file.take(MAX_SOURCE_BYTES as u64 + 1)
        .read_to_string(&mut source)
        .ok()?;
    (source.len() <= MAX_SOURCE_BYTES).then_some(source)
}

fn rust_parser() -> Option<Parser> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .ok()?;
    Some(parser)
}

fn text<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    node.utf8_text(source.as_bytes()).unwrap_or("")
}

// An explicit cursor avoids unbounded call stacks for nested source or macros.
fn walk<'t>(node: Node<'t>, visit: &mut impl FnMut(Node<'t>)) {
    let mut cursor = node.walk();
    loop {
        visit(cursor.node());
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}
