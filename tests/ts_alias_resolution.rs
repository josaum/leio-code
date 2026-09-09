//! Integration tests for tsconfig-driven TypeScript import-alias resolution.
//!
//! Each test stages a synthetic tempdir monorepo, runs the real indexer, and
//! asserts `resolved-imports-in` candidate paths through the same library
//! entry points the CLI uses. Pattern mirrors
//! `tests/cross_language_unresolved_edges.rs`.

use std::fs;
use std::path::Path;

use leio_code::graph_query::query_resolved_imports_in;
use leio_code::indexer::{build_or_update_index, default_index_path};
use leio_code::model::QueryEnvelope;
use tempfile::TempDir;

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(&path, body).expect("write file");
}

fn build(root: &Path) -> leio_code::model::RepoIndex {
    // Disable DuckDB sidecar — we only need the JSON index.
    // SAFETY: test suite is single-threaded by default.
    unsafe { std::env::set_var("LEIO_DISABLE_SEARCH_SIDECAR", "1") };
    let index_path = default_index_path(root);
    build_or_update_index(root, &index_path, true).expect("build index")
}

/// Returns the candidate paths of the resolved import whose specifiers
/// include `specifier`, or an empty vec when no such import was found.
fn candidate_paths_for(envelope: &QueryEnvelope, specifier: &str) -> Vec<String> {
    envelope
        .entities
        .iter()
        .find_map(|entity| {
            let specs = entity.get("module_specifiers")?.as_array()?;
            if !specs.iter().any(|s| s.as_str() == Some(specifier)) {
                return None;
            }
            let paths = entity.get("candidate_paths")?.as_array()?;
            Some(
                paths
                    .iter()
                    .filter_map(|p| p.as_str().map(str::to_string))
                    .collect::<Vec<_>>(),
            )
        })
        .unwrap_or_default()
}

fn resolution_kind_for(envelope: &QueryEnvelope, specifier: &str) -> Option<String> {
    envelope.entities.iter().find_map(|entity| {
        let specs = entity.get("module_specifiers")?.as_array()?;
        if !specs.iter().any(|s| s.as_str() == Some(specifier)) {
            return None;
        }
        entity.get("resolution_kind")?.as_str().map(str::to_string)
    })
}

// WHY: tsconfig `paths`/`baseUrl` must drive alias resolution in ANY
// TypeScript monorepo — no hardcoded Example app table involved — and the
// parser must tolerate JSONC comments because real tsconfigs carry them.
#[test]
fn tsconfig_paths_resolve_aliases_in_any_monorepo() {
    let tmp = TempDir::new().expect("create tempdir");
    write(
        tmp.path(),
        "web/tsconfig.json",
        r#"{
  // JSONC comment: tolerant parsing is part of the contract.
  "compilerOptions": {
    "baseUrl": ".",
    "paths": {
      "@/*": ["./src/*"],
      "@lib/*": ["../shared/lib/*"]
    }
  }
}
"#,
    );
    write(
        tmp.path(),
        "web/src/components/button.ts",
        "export const x = 1;\n",
    );
    write(tmp.path(), "shared/lib/util.ts", "export const y = 2;\n");
    write(
        tmp.path(),
        "web/src/page.ts",
        "import { x } from \"@/components/button\";\nimport { y } from \"@lib/util\";\nexport const page = [x, y];\n",
    );

    let index = build(tmp.path());
    let envelope = query_resolved_imports_in(&index, tmp.path(), "web/src/page.ts")
        .expect("resolved-imports-in should work");

    assert_eq!(
        candidate_paths_for(&envelope, "@/components/button"),
        vec!["web/src/components/button.ts".to_string()],
        "in-app `@/*` alias should resolve via tsconfig paths: {}",
        envelope.summary
    );
    assert_eq!(
        candidate_paths_for(&envelope, "@lib/util"),
        vec!["shared/lib/util.ts".to_string()],
        "cross-package `@lib/*` alias should resolve outside the tsconfig dir: {}",
        envelope.summary
    );
    assert_eq!(
        resolution_kind_for(&envelope, "@/components/button").as_deref(),
        Some("resolved")
    );
    assert_eq!(
        resolution_kind_for(&envelope, "@lib/util").as_deref(),
        Some("resolved")
    );
}

// WHY: one level of `extends` is in scope, and the child's `paths` must win
// per-key — flipping the merge order would silently re-point alias targets.
#[test]
fn child_tsconfig_extends_parent_and_child_paths_win() {
    let tmp = TempDir::new().expect("create tempdir");
    write(
        tmp.path(),
        "web/tsconfig.base.json",
        r#"{
  "compilerOptions": {
    "baseUrl": ".",
    "paths": {
      "@x/*": ["./parent-x/*"],
      "@inherited/*": ["./src/*"]
    }
  }
}
"#,
    );
    write(
        tmp.path(),
        "web/tsconfig.json",
        r#"{
  "extends": "./tsconfig.base.json",
  "compilerOptions": {
    "paths": {
      "@x/*": ["./child-x/*"]
    }
  }
}
"#,
    );
    // Both targets exist on disk so the assertion proves precedence, not
    // mere existence.
    write(tmp.path(), "web/parent-x/thing.ts", "export const a = 1;\n");
    write(tmp.path(), "web/child-x/thing.ts", "export const a = 2;\n");
    write(tmp.path(), "web/src/dep.ts", "export const b = 3;\n");
    write(
        tmp.path(),
        "web/src/page.ts",
        "import { a } from \"@x/thing\";\nimport { b } from \"@inherited/dep\";\nexport const page = [a, b];\n",
    );

    let index = build(tmp.path());
    let envelope = query_resolved_imports_in(&index, tmp.path(), "web/src/page.ts")
        .expect("resolved-imports-in should work");

    assert_eq!(
        candidate_paths_for(&envelope, "@x/thing"),
        vec!["web/child-x/thing.ts".to_string()],
        "child tsconfig `paths` must override the extended base per-key: {}",
        envelope.summary
    );
    assert_eq!(
        candidate_paths_for(&envelope, "@inherited/dep"),
        vec!["web/src/dep.ts".to_string()],
        "mappings only declared in the extended base must still apply: {}",
        envelope.summary
    );
}

// WHY: repos without any tsconfig must keep resolving the legacy hardcoded
// Example aliases — this is the no-regression guarantee for the workspace.
#[test]
fn legacy_hardcoded_aliases_still_resolve_without_tsconfig() {
    let tmp = TempDir::new().expect("create tempdir");
    write(
        tmp.path(),
        "packages/trpc/src/router.ts",
        "export const r = 1;\n",
    );
    write(
        tmp.path(),
        "example-ops/src/page.ts",
        "import { r } from \"@jai/trpc/router\";\nexport const page = r;\n",
    );

    let index = build(tmp.path());
    let envelope = query_resolved_imports_in(&index, tmp.path(), "example-ops/src/page.ts")
        .expect("resolved-imports-in should work");

    assert_eq!(
        candidate_paths_for(&envelope, "@jai/trpc/router"),
        vec!["packages/trpc/src/router.ts".to_string()],
        "legacy `@jai/trpc/` mapping must survive when no tsconfig exists: {}",
        envelope.summary
    );
    assert_eq!(
        resolution_kind_for(&envelope, "@jai/trpc/router").as_deref(),
        Some("resolved")
    );
}

#[test]
fn single_quoted_and_default_ts_imports_resolve_correctly() {
    let tmp = TempDir::new().expect("create tempdir");
    write(
        tmp.path(),
        "src/components/canvas.ts",
        "export class Canvas {}\n",
    );
    write(
        tmp.path(),
        "src/data/constants.ts",
        "export const FOO = 42;\n",
    );
    write(
        tmp.path(),
        "src/main.ts",
        "import { Canvas } from './components/canvas';\nimport { FOO } from './data/constants';\nexport const app = [Canvas, FOO];\n",
    );

    let index = build(tmp.path());
    let envelope = query_resolved_imports_in(&index, tmp.path(), "src/main.ts")
        .expect("resolved-imports-in should work");

    assert_eq!(
        candidate_paths_for(&envelope, "./components/canvas"),
        vec!["src/components/canvas.ts".to_string()],
        "single-quoted extensionless relative import must resolve to .ts file: {}",
        envelope.summary
    );
    assert_eq!(
        candidate_paths_for(&envelope, "./data/constants"),
        vec!["src/data/constants.ts".to_string()],
        "single-quoted extensionless relative import must resolve to .ts file: {}",
        envelope.summary
    );
    assert_eq!(
        resolution_kind_for(&envelope, "./components/canvas").as_deref(),
        Some("resolved")
    );
    assert_eq!(
        resolution_kind_for(&envelope, "./data/constants").as_deref(),
        Some("resolved")
    );
}
