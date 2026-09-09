use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::query_id;
use crate::code_graph::{
    CodeGraphQueryCache, default_code_graph_cache_path, default_code_graph_output_dir,
    export_code_graph,
};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex, SourceLanguage, SymbolKind};

pub struct OrphanFilesDoctor;

impl Doctor for OrphanFilesDoctor {
    fn name(&self) -> &'static str {
        "orphan-files"
    }

    fn description(&self) -> &'static str {
        "Flags production-surface source files that no other indexed file imports, catching modules that are migrated, copied, or rewritten without any real caller."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_orphan_files(index, root)
    }
}

/// Default production surfaces scanned under the Example profile when no
/// `[doctors.orphan_files] surfaces` config is present. Each entry is
/// `(path_prefix, label)`. The list is intentionally tight: catching real
/// drift in the hot paths is more valuable than wide coverage that produces
/// noisy warnings.
const SURFACE_PREFIXES: &[(&str, &str)] = &[
    ("example-api/example/", "example-api"),
    ("example-gateway/src/", "example-gateway"),
];

/// File suffixes that disqualify a path from the scan (tests, examples, benches,
/// generated artifacts).
const EXCLUDE_SUFFIXES: &[&str] = &[
    "/__init__.py",
    "/conftest.py",
    "/_test.py",
    "_test.py",
    "_test.rs",
    ".test.ts",
    ".test.tsx",
    ".spec.ts",
    ".spec.tsx",
    "/main.rs",
    "/lib.rs",
    "/mod.rs",
    "/build.rs",
];

/// Path segments that disqualify a file from being considered orphan (tests,
/// examples, benches, generated outputs, vendored code).
const EXCLUDE_SEGMENTS: &[&str] = &[
    "/tests/",
    "/test/",
    "/__tests__/",
    "/examples/",
    "/example/",
    "/benches/",
    "/bench/",
    "/fixtures/",
    "/migrations/",
    "/generated/",
    "/vendor/",
    "/node_modules/",
    "/target/",
    "/.next/",
    "/dist/",
    "/build/",
];

/// Filenames that are framework-managed entry points (Next/Vite/Tauri/Python).
/// A file matching one of these names is never treated as orphan even when no
/// other file imports it — the framework wires it via convention.
const ENTRYPOINT_FILE_NAMES: &[&str] = &[
    "main.py",
    "main.rs",
    "lib.rs",
    "mod.rs",
    "build.rs",
    "index.ts",
    "index.tsx",
    "index.js",
    "index.jsx",
    "_app.tsx",
    "_app.ts",
    "_document.tsx",
    "_error.tsx",
    "middleware.ts",
    "middleware.tsx",
    "instrumentation.ts",
    "page.tsx",
    "page.ts",
    "layout.tsx",
    "layout.ts",
    "loading.tsx",
    "loading.ts",
    "error.tsx",
    "error.ts",
    "not-found.tsx",
    "not-found.ts",
    "route.ts",
    "route.tsx",
    "vite.config.ts",
    "vite.config.js",
    "next.config.ts",
    "next.config.js",
    "next.config.mjs",
    "tsup.config.ts",
    "tailwind.config.ts",
    "tailwind.config.js",
    "postcss.config.js",
    "postcss.config.mjs",
    "playwright.config.ts",
    "vitest.config.ts",
    "jest.config.cjs",
    "jest.config.js",
    "rollup.config.ts",
    "tsconfig.ts",
    "celery_app.py",
    "celeryconfig.py",
    "asgi.py",
    "wsgi.py",
    "settings.py",
    "urls.py",
    "manage.py",
    "app.py",
    "__main__.py",
];

/// Path segments that indicate a "framework-conventional" route file (Next.js
/// app/, pages/, api/). Files under these segments are always wired by the
/// framework regardless of imports.
const ENTRYPOINT_PATH_SEGMENTS: &[&str] = &["/app/", "/pages/", "/api/", "/routes/", "/bin/"];

/// Convention-based "Protocol-implementation" entrypoints. These are
/// (parent_dir_segment, class_suffix) pairs: a file matching the parent_dir
/// pattern and whose top-level class names end with class_suffix is treated as
/// an entrypoint. The pattern catches the typical `runtime_checkable Protocol`
/// implementation layout — `*/channels/<adapter>.py` defining `XChannelAdapter`,
/// `*/providers/<provider>.py` defining `XProvider`, etc. — without requiring a
/// full type-system traversal.
const PROTOCOL_IMPL_PATTERNS: &[(&str, &str)] = &[
    ("/channels/", "ChannelAdapter"),
    ("/channels/", "Adapter"),
    ("/providers/", "Provider"),
    ("/handover/", "HandoverTarget"),
    ("/handover/", "Handover"),
    ("/strategies/", "Strategy"),
];

/// Files intentionally retained as dynamically loaded extension points,
/// component-library inventory, or optional tool adapters. The import graph
/// cannot see these runtime/configuration edges, so keep the allowlist exact:
/// adding a new entry requires naming the concrete file that is not supposed to
/// be import-wired.
const DYNAMIC_ENTRYPOINT_FILES: &[&str] = &[
    "example-api/example/agents/flight_model.py",
    "example-api/example/agents/prompt_prefix.py",
    "example-api/example/agents/session.py",
    "example-api/example/agents/tools/builtin/bandit.py",
    "example-api/example/agents/tools/builtin/classify.py",
    "example-api/example/agents/tools/builtin/embed.py",
    "example-api/example/agents/tools/builtin/extract.py",
    "example-api/example/agents/tools/builtin/knowledge.py",
    "example-api/example/agents/tools/integration.py",
    "example-api/example/agents/tools/loader.py",
    "example-api/example/clients/vllm_client.py",
    "example-api/example/core/vector/types.py",
    "example-api/example/flight/gepa_flight.py",
    "example-api/example/integrations/whatsapp/agent_dispatch.py",
    "example-api/example/integrations/whatsapp/events.py",
    "example-api/example/routers/navigate.py",
    "example-api/example/types/adapters.py",
    "example-api/example/types/responses.py",
    // Staged Fractal-Vault auth store for the Chat-SOTA Rust-auth phase: module-wired
    // (`auth::secure_store`) and compiled, but intentionally unconsumed until the
    // auth overhaul lands. Re-evaluate when src/auth grows its storage backend.
    "example-gateway/src/auth/secure_store.rs",
    "example-gateway/src/integrations/tools/kb_session.rs",
    "example-gateway/src/integrations/tools/physics.rs",
    "example-gateway/src/integrations/tools/whatsapp_business.rs",
    "example-gateway/src/storage/duckdb_resilience.rs",
];

pub fn doctor_orphan_files(index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    // Surface resolution: explicit `[doctors.orphan_files] surfaces` config
    // wins; without it the Example profile keeps its const defaults
    // (behavior unchanged) and every other profile is a no-op — generic
    // repos opt in instead of inheriting Example's path list.
    let configured_surfaces: Option<Vec<String>> = crate::config::load_repo_config(root)
        .and_then(|config| config.doctors)
        .and_then(|doctors| doctors.orphan_files)
        .and_then(|orphan_files| orphan_files.surfaces)
        .filter(|surfaces| !surfaces.is_empty());

    let surfaces: Vec<(String, String)> = match configured_surfaces {
        Some(surfaces) => surfaces
            .into_iter()
            .map(|prefix| {
                let label = prefix.trim_end_matches('/').to_string();
                (prefix, label)
            })
            .collect(),
        None if crate::config::repo_profile(root) == crate::config::PROFILE_EXAMPLE => {
            SURFACE_PREFIXES
                .iter()
                .map(|(prefix, label)| ((*prefix).to_string(), (*label).to_string()))
                .collect()
        }
        None => {
            return QueryEnvelope {
                schema_version: crate::model::SCHEMA_VERSION.to_string(),
                query_id: query_id("doctor_orphan_files"),
                kind: "doctor".to_string(),
                summary: "orphan-files: no orphan-files surfaces configured — doctor is inactive (set [doctors.orphan_files] surfaces in .leio-code/config.toml)".to_string(),
                confidence: 0.95,
                entities,
                evidence,
                warnings,
                meta: Some(json!({"reason": "no_surfaces_configured"})),
                timing_ms: started.elapsed().as_millis(),
            };
        }
    };

    let cache = match load_or_refresh_cache(index, root) {
        Ok(cache) => cache,
        Err(error) => {
            warnings.push(format!("failed to load code graph query cache: {error}"));
            return QueryEnvelope {
                schema_version: crate::model::SCHEMA_VERSION.to_string(),
                query_id: query_id("doctor_orphan_files"),
                kind: "doctor".to_string(),
                summary: "could not enumerate orphan files: graph cache unavailable".to_string(),
                confidence: 0.0,
                entities,
                evidence,
                warnings,
                meta: None,
                timing_ms: started.elapsed().as_millis(),
            };
        }
    };

    let mut surface_summary: Vec<(String, usize, usize)> = Vec::new();

    for (prefix, label) in &surfaces {
        let prefix_str = prefix.as_str();
        let mut surface_files: Vec<&str> = cache
            .files
            .values()
            .map(|file| file.path.as_str())
            .filter(|path| path.starts_with(prefix_str))
            .filter(|path| !is_excluded_path(&format!("/{}", &path[prefix_str.len()..])))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        surface_files.sort();

        let mut orphan_count = 0usize;

        for path in &surface_files {
            if is_entrypoint_file(path) {
                continue;
            }
            if is_dynamic_entrypoint_file(path) {
                continue;
            }
            if is_protocol_impl_entrypoint(index, path) {
                continue;
            }

            let importer_count = cache
                .importers_by_target_path
                .get(*path)
                .map(|importers| importers.len())
                .unwrap_or(0);
            if importer_count > 0 {
                continue;
            }

            // A file with only re-exports is sometimes legitimately uncalled but
            // exposes downstream API; the import-edge graph only tracks `import`
            // statements, so re-export-only files would still be flagged. We
            // accept this minor false-positive risk in exchange for catching the
            // 433-line drift case the doctor exists to prevent.

            orphan_count += 1;
            let abs_path = root.join(path);
            let detail = format!(
                "no importers in indexed code graph; delete or wire it in (surface: {label})"
            );
            warnings.push(format!(
                "{path} is orphan in {label}: 0 importers across the workspace; delete or wire it in"
            ));
            entities.push(json!({
                "doctor": "orphan-files",
                "surface": label,
                "file": path,
                "absolute_path": abs_path.display().to_string(),
                "importer_count": 0,
            }));
            evidence.push(EvidenceItem {
                kind: "orphan_file".to_string(),
                path: path.to_string(),
                line: None,
                detail,
            });
        }

        surface_summary.push((label.to_string(), surface_files.len(), orphan_count));
    }

    let summary = format!(
        "orphan-files: scanned {} surfaces, found {} orphan files",
        surfaces.len(),
        warnings.len()
    );

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_orphan_files"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.95 } else { 0.78 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "surfaces": surface_summary
                .iter()
                .map(|(label, total, orphans)| json!({
                    "surface": label,
                    "scanned_files": total,
                    "orphan_files": orphans,
                }))
                .collect::<Vec<_>>(),
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// Loads the code-graph query cache, exporting fresh artifacts when the
/// cache file is missing. Shared with the import-boundary doctor so both
/// read the same artifact the `graph` CLI queries use.
pub(crate) fn load_or_refresh_cache(
    index: &RepoIndex,
    root: &Path,
) -> anyhow::Result<CodeGraphQueryCache> {
    let output_dir = default_code_graph_output_dir(root);
    let cache_path = default_code_graph_cache_path(&output_dir);
    if !cache_path.exists() {
        export_code_graph(index, root, &output_dir)?;
    }
    let raw = std::fs::read_to_string(&cache_path)?;
    let cache: CodeGraphQueryCache = serde_json::from_str(&raw)?;
    Ok(cache)
}

fn is_excluded_path(path: &str) -> bool {
    if EXCLUDE_SEGMENTS
        .iter()
        .any(|segment| path.contains(segment))
    {
        return true;
    }
    if EXCLUDE_SUFFIXES.iter().any(|suffix| path.ends_with(suffix)) {
        return true;
    }
    false
}

fn is_entrypoint_file(path: &str) -> bool {
    let file_name = path.rsplit('/').next().unwrap_or(path);
    if ENTRYPOINT_FILE_NAMES.contains(&file_name) {
        return true;
    }
    if ENTRYPOINT_PATH_SEGMENTS
        .iter()
        .any(|segment| path.contains(segment))
    {
        return true;
    }
    false
}

fn is_dynamic_entrypoint_file(path: &str) -> bool {
    DYNAMIC_ENTRYPOINT_FILES.contains(&path)
}

/// Convention-based detection for `runtime_checkable Protocol` implementations.
/// We do not have full type analysis, so the heuristic is purely structural:
/// if the file lives in a directory that registers Protocol implementations
/// (`channels/`, `providers/`, `handover/`, `strategies/`) AND defines a
/// top-level class whose name matches the convention's suffix, treat it as a
/// Protocol-impl entrypoint. A factory or registry in the same package will
/// instantiate it dynamically; the import-edge graph cannot see that linkage.
fn is_protocol_impl_entrypoint(index: &RepoIndex, path: &str) -> bool {
    // Match Python and TypeScript implementation files only — Rust uses the
    // trait/impl machinery directly, with no analogous false-positive class.
    let language = match index.files.iter().find(|file| file.path == path) {
        Some(file) => file.language,
        None => return false,
    };
    if !matches!(
        language,
        SourceLanguage::Python
            | SourceLanguage::TypeScript
            | SourceLanguage::Tsx
            | SourceLanguage::JavaScript
    ) {
        return false;
    }

    let file = match index.files.iter().find(|file| file.path == path) {
        Some(file) => file,
        None => return false,
    };

    // The expensive matching only fires for files that already live under one of
    // the convention-bearing directories. This keeps the heuristic narrow and
    // makes it cheap to extend with new directory/suffix pairs.
    let mut matched_dir_with_suffix = None;
    for (segment, suffix) in PROTOCOL_IMPL_PATTERNS {
        if path.contains(segment) {
            matched_dir_with_suffix = Some((*segment, *suffix));
            break;
        }
    }
    let (_segment, suffix) = match matched_dir_with_suffix {
        Some(pair) => pair,
        None => return false,
    };

    file.symbols.iter().any(|symbol| {
        matches!(
            symbol.kind,
            SymbolKind::Class | SymbolKind::Interface | SymbolKind::Struct
        ) && symbol.name.ends_with(suffix)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FileRecord, SourceLanguage};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_tempdir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-orphan-files-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(path, contents).expect("write fixture");
    }

    /// The const SURFACE_PREFIXES only apply under the Example profile now
    /// that surfaces are config-driven; fixtures must opt in explicitly.
    fn write_example_profile(dir: &Path) {
        write_file(
            &dir.join(".leio-code/config.toml"),
            "workspace_profile = \"example\"\n",
        );
    }

    fn ts_record(path: &str, bytes: usize) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            language: SourceLanguage::TypeScript,
            bytes,
            modified_unix_ms: 0,
            symbols: Vec::new(),
            env_vars: Vec::new(),
            redis_keys: Vec::new(),
            subprocess_calls: Vec::new(),
            http_calls: Vec::new(),
            unresolved_edges: Vec::new(),
        }
    }

    fn rust_record(path: &str, bytes: usize) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            language: SourceLanguage::Rust,
            bytes,
            modified_unix_ms: 0,
            symbols: Vec::new(),
            env_vars: Vec::new(),
            redis_keys: Vec::new(),
            subprocess_calls: Vec::new(),
            http_calls: Vec::new(),
            unresolved_edges: Vec::new(),
        }
    }

    fn py_record(path: &str, bytes: usize) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            language: SourceLanguage::Python,
            bytes,
            modified_unix_ms: 0,
            symbols: Vec::new(),
            env_vars: Vec::new(),
            redis_keys: Vec::new(),
            subprocess_calls: Vec::new(),
            http_calls: Vec::new(),
            unresolved_edges: Vec::new(),
        }
    }

    fn py_record_with_class(path: &str, bytes: usize, class_name: &str, line: usize) -> FileRecord {
        use crate::model::{SymbolKind, SymbolOccurrence};
        FileRecord {
            path: path.to_string(),
            language: SourceLanguage::Python,
            bytes,
            modified_unix_ms: 0,
            symbols: vec![SymbolOccurrence {
                name: class_name.to_string(),
                kind: SymbolKind::Class,
                path: path.to_string(),
                line,
                language: SourceLanguage::Python,
                qual_name: None,
            }],
            env_vars: Vec::new(),
            redis_keys: Vec::new(),
            subprocess_calls: Vec::new(),
            http_calls: Vec::new(),
            unresolved_edges: Vec::new(),
        }
    }

    #[test]
    fn flags_orphan_file_in_gateway_core() {
        let dir = unique_tempdir("orphan-server");
        write_example_profile(&dir);
        write_file(
            &dir.join("example-gateway/src/_core/llm.ts"),
            "export function invokeLLM() { return 1; }\n",
        );
        write_file(
            &dir.join("example-gateway/src/_core/index.ts"),
            // index.ts is a recognized entry point; it should NOT be flagged.
            "export const placeholder = 1;\n",
        );
        write_file(
            &dir.join("example-gateway/src/wired.ts"),
            "import { invokeLLM as alias } from \"./_core/llm\";\nexport function bootstrap() { return alias(); }\n",
        );
        // Wait — for the orphan to be truly orphan, no file should import it.
        // Replace `wired.ts` so it doesn't import the orphan.
        write_file(
            &dir.join("example-gateway/src/wired.ts"),
            "export function bootstrap() { return 1; }\n",
        );

        let index = RepoIndex {
            version: 1,
            root: dir.display().to_string(),
            indexed_at: "2026-05-04T00:00:00Z".to_string(),
            files: vec![
                ts_record("example-gateway/src/_core/llm.ts", 50),
                ts_record("example-gateway/src/_core/index.ts", 30),
                ts_record("example-gateway/src/wired.ts", 50),
            ],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let envelope = doctor_orphan_files(&index, &dir);

        // The orphan file should be flagged.
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("example-gateway/src/_core/llm.ts") && w.contains("orphan")),
            "expected orphan llm.ts to be flagged, got warnings: {:?}",
            envelope.warnings
        );
        // index.ts should NOT be flagged (entry point).
        assert!(
            !envelope
                .warnings
                .iter()
                .any(|w| w.contains("_core/index.ts")),
            "index.ts is an entry point and must not be flagged: {:?}",
            envelope.warnings
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_warnings_when_all_files_are_imported() {
        let dir = unique_tempdir("orphan-clean");
        write_example_profile(&dir);
        write_file(
            &dir.join("example-gateway/src/_core/llm.ts"),
            "export function invokeLLM() { return 1; }\n",
        );
        write_file(
            &dir.join("example-gateway/src/index.ts"),
            "import { invokeLLM } from \"./_core/llm\";\nexport function boot() { return invokeLLM(); }\n",
        );

        let index = RepoIndex {
            version: 1,
            root: dir.display().to_string(),
            indexed_at: "2026-05-04T00:00:00Z".to_string(),
            files: vec![
                ts_record("example-gateway/src/_core/llm.ts", 50),
                ts_record("example-gateway/src/index.ts", 100),
            ],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let envelope = doctor_orphan_files(&index, &dir);

        assert!(
            envelope.warnings.is_empty(),
            "expected no orphan warnings, got: {:?}",
            envelope.warnings
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_excluded_test_and_vendor_paths() {
        let dir = unique_tempdir("orphan-excluded");
        write_example_profile(&dir);
        // A test file in the surface path, never imported. Must be skipped.
        write_file(
            &dir.join("example-gateway/src/foo.test.ts"),
            "export function describesomething() { return 1; }\n",
        );
        // A vendored file under server/. Must be skipped.
        write_file(
            &dir.join("example-gateway/src/vendor/ext.ts"),
            "export function vendored() { return 1; }\n",
        );

        let index = RepoIndex {
            version: 1,
            root: dir.display().to_string(),
            indexed_at: "2026-05-04T00:00:00Z".to_string(),
            files: vec![
                ts_record("example-gateway/src/foo.test.ts", 50),
                ts_record("example-gateway/src/vendor/ext.ts", 50),
            ],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let envelope = doctor_orphan_files(&index, &dir);

        assert!(
            envelope.warnings.is_empty(),
            "expected tests and vendored files to be skipped, got: {:?}",
            envelope.warnings
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// Fix 1 — dotted-string-import registry detection.
    ///
    /// `routers/__init__.py` references the agent router only via a tuple
    /// literal `(".admin_router", "router")`. Without string-literal scanning
    /// the doctor would emit a false-positive orphan warning for `admin.py`.
    #[test]
    fn skips_files_referenced_via_dotted_string_registry() {
        let dir = unique_tempdir("orphan-dotted-string");
        write_example_profile(&dir);
        write_file(
            &dir.join("example-api/example/routers/__init__.py"),
            "from importlib import import_module\n\
             ROUTERS = {\n    \"admin_router\": (\".admin\", \"router\"),\n    \"tasks_router\": (\"..agents.router\", \"router\"),\n}\n\
             def load(name):\n    spec, attr = ROUTERS[name]\n    return getattr(import_module(spec, __name__), attr)\n",
        );
        write_file(
            &dir.join("example-api/example/routers/admin.py"),
            "from fastapi import APIRouter\nrouter = APIRouter()\n",
        );
        write_file(
            &dir.join("example-api/example/agents/router.py"),
            "from fastapi import APIRouter\nrouter = APIRouter()\n",
        );

        let index = RepoIndex {
            version: 1,
            root: dir.display().to_string(),
            indexed_at: "2026-05-04T00:00:00Z".to_string(),
            files: vec![
                py_record("example-api/example/routers/__init__.py", 220),
                py_record("example-api/example/routers/admin.py", 60),
                py_record("example-api/example/agents/router.py", 60),
            ],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let envelope = doctor_orphan_files(&index, &dir);

        assert!(
            !envelope.warnings.iter().any(|w| w.contains("admin.py")),
            "admin.py is referenced via dotted-string registry; should not be flagged: {:?}",
            envelope.warnings
        );
        assert!(
            !envelope
                .warnings
                .iter()
                .any(|w| w.contains("agents/router.py")),
            "agents/router.py is referenced via dotted-string registry; should not be flagged: {:?}",
            envelope.warnings
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// Fix 2 — deferred imports inside function bodies.
    ///
    /// `agent_run.py` imports `example.agents.swarm` only when the route is
    /// actually called. The recursive AST walk should still detect the import
    /// edge so `swarm.py` is not flagged orphan.
    #[test]
    fn skips_files_referenced_via_deferred_imports() {
        let dir = unique_tempdir("orphan-deferred");
        write_example_profile(&dir);
        write_file(
            &dir.join("example-api/example/agents/swarm.py"),
            "def run_swarm():\n    return 1\n",
        );
        write_file(
            &dir.join("example-api/example/routers/agent_run.py"),
            "from fastapi import APIRouter\n\nrouter = APIRouter()\n\n\
             @router.get('/swarm')\nasync def swarm_endpoint():\n    \
             from example.agents.swarm import run_swarm\n    return run_swarm()\n",
        );

        let index = RepoIndex {
            version: 1,
            root: dir.display().to_string(),
            indexed_at: "2026-05-04T00:00:00Z".to_string(),
            files: vec![
                py_record("example-api/example/agents/swarm.py", 40),
                py_record("example-api/example/routers/agent_run.py", 200),
            ],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let envelope = doctor_orphan_files(&index, &dir);

        assert!(
            !envelope
                .warnings
                .iter()
                .any(|w| w.contains("agents/swarm.py")),
            "swarm.py is referenced via deferred import; should not be flagged: {:?}",
            envelope.warnings
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// Fix 3 — Protocol-implementation entrypoint heuristic.
    ///
    /// A class ending in `ChannelAdapter` inside a `*/channels/<name>.py` file
    /// is a Protocol implementation. Even when no other file imports the
    /// adapter directly, it is wired up at runtime by a registry / factory and
    /// must not be reported as orphan.
    #[test]
    fn skips_protocol_implementation_classes_in_channels_directory() {
        let dir = unique_tempdir("orphan-protocol-channels");
        write_example_profile(&dir);
        write_file(
            &dir.join("example-api/example/agents/channels/whatsapp.py"),
            "class WhatsAppChannelAdapter:\n    channel_name = 'whatsapp'\n    \
             def send(self, message):\n        return None\n",
        );
        write_file(
            &dir.join("example-api/example/agents/channels/__init__.py"),
            "from typing import Protocol, runtime_checkable\n\n\
             @runtime_checkable\nclass ChannelAdapter(Protocol):\n    \
             channel_name: str\n    def send(self, message): ...\n",
        );

        let index = RepoIndex {
            version: 1,
            root: dir.display().to_string(),
            indexed_at: "2026-05-04T00:00:00Z".to_string(),
            files: vec![
                py_record_with_class(
                    "example-api/example/agents/channels/whatsapp.py",
                    140,
                    "WhatsAppChannelAdapter",
                    1,
                ),
                py_record("example-api/example/agents/channels/__init__.py", 180),
            ],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let envelope = doctor_orphan_files(&index, &dir);

        assert!(
            !envelope
                .warnings
                .iter()
                .any(|w| w.contains("channels/whatsapp.py")),
            "WhatsAppChannelAdapter is a Protocol-impl entrypoint; should not be flagged: {:?}",
            envelope.warnings
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// Sanity check: a file under `*/channels/` whose class name does NOT match
    /// the convention is still flagged as orphan when nothing imports it. We
    /// do not want the heuristic to over-shadow the intent of the doctor.
    #[test]
    fn protocol_heuristic_does_not_silence_unrelated_classes() {
        let dir = unique_tempdir("orphan-protocol-unrelated");
        write_example_profile(&dir);
        write_file(
            &dir.join("example-api/example/agents/channels/notes.py"),
            "class WhatsAppNotes:\n    pass\n",
        );

        let index = RepoIndex {
            version: 1,
            root: dir.display().to_string(),
            indexed_at: "2026-05-04T00:00:00Z".to_string(),
            files: vec![py_record_with_class(
                "example-api/example/agents/channels/notes.py",
                40,
                "WhatsAppNotes",
                1,
            )],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let envelope = doctor_orphan_files(&index, &dir);

        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("channels/notes.py") && w.contains("orphan")),
            "WhatsAppNotes does not end with `Adapter` / `ChannelAdapter`; should still flag: {:?}",
            envelope.warnings
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// Fix 4 — Rust `mod X;` declarations create import edges.
    ///
    /// `agents/mod.rs` declares `pub mod executor;`. Without treating `mod_item`
    /// nodes as import statements, `executor.rs` would look orphaned even though
    /// `mod.rs` wires it into the module tree.
    #[test]
    fn skips_rust_submodule_declared_via_mod_item() {
        let dir = unique_tempdir("orphan-rust-mod-item");
        write_example_profile(&dir);
        write_file(
            &dir.join("example-gateway/src/agents/mod.rs"),
            "pub mod executor;\npub mod types;\n",
        );
        write_file(
            &dir.join("example-gateway/src/agents/executor.rs"),
            "pub fn execute() {}\n",
        );
        write_file(
            &dir.join("example-gateway/src/agents/types.rs"),
            "pub struct AgentId;\n",
        );

        let index = RepoIndex {
            version: 1,
            root: dir.display().to_string(),
            indexed_at: "2026-05-04T00:00:00Z".to_string(),
            files: vec![
                rust_record("example-gateway/src/agents/mod.rs", 40),
                rust_record("example-gateway/src/agents/executor.rs", 30),
                rust_record("example-gateway/src/agents/types.rs", 20),
            ],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let envelope = doctor_orphan_files(&index, &dir);

        assert!(
            !envelope
                .warnings
                .iter()
                .any(|w| w.contains("agents/executor.rs")),
            "executor.rs is declared via `pub mod executor;` and must not be flagged: {:?}",
            envelope.warnings
        );
        assert!(
            !envelope
                .warnings
                .iter()
                .any(|w| w.contains("agents/types.rs")),
            "types.rs is declared via `pub mod types;` and must not be flagged: {:?}",
            envelope.warnings
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
