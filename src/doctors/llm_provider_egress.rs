//! Bans direct LLM provider URLs in workspace runtime source.
//!
//! This is the workspace-wide structural gate for every runtime surface.
//!
//! Policy:
//! - LLM egress should terminate in the canonical Example/`gateway` egress
//!   path. Direct provider URLs in app/cartridge runtime source bypass GEPA,
//!   the three-gate egress validator, and the audit trail.
//! - The `forge.manus.im` proxy and `*.manus.{space,im}` family are banned
//!   outright (PR #50 + PR #53 finished migrating off them).
//! - `openrouter.ai` is banned outside canonical reviewed egress points.
//! - Per CLAUDE.md ("Outbound messaging terminates in Rust"), the Rust
//!   gateway IS the legitimate LLM-egress point. The four canonical
//!   gateway egress files (`example-gateway/src/cognitive/engines/remote.rs`,
//!   `example-gateway/src/config/gateway.rs`, `example-gateway/src/llm_config.rs`,
//!   `example-gateway/src/whatsapp/ai_assist.rs`) are allowlisted to host the
//!   provider URLs. We DO NOT allowlist the whole `example-gateway/` tree;
//!   only those four files. Any new gateway egress file must be reviewed and
//!   added explicitly.
//!
//! Test files, `.env*` examples, markdown/docs, and vendored builds are
//! skipped. Traversal is `.gitignore`-aware via the `ignore` crate, so
//! gitignored backup directories (e.g. future `api_backup/`) are filtered
//! automatically. The allowlist is in-source (not config-file driven) so
//! it cannot drift silently.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Instant;

use ignore::WalkBuilder;
use regex::Regex;
use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct LlmProviderEgressDoctor;

impl Doctor for LlmProviderEgressDoctor {
    fn name(&self) -> &'static str {
        "llm-provider-egress"
    }

    fn description(&self) -> &'static str {
        "Bans direct LLM provider URLs (OpenAI, Anthropic, Google, OpenRouter, forge/manus) outside reviewed canonical egress points. Honors .gitignore via the `ignore` crate."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_llm_provider_egress(root)
    }
}

struct BannedPattern {
    name: &'static str,
    regex: Regex,
}

/// Allowlist: `(file_path_substring, allowed_pattern_name)`.
///
/// `file_path_substring` is matched against the repo-relative POSIX path
/// (substring/`ends_with`-style match, NOT a glob). `allowed_pattern_name`
/// must equal one of the `BANNED_PATTERNS[*].name` constants below.
///
const ALLOWLIST: &[(&str, &str)] = &[
    // Per CLAUDE.md: Rust gateway is the canonical LLM-egress point.
    (
        "example-gateway/src/cognitive/engines/remote.rs",
        "api.openai.com",
    ),
    // Per CLAUDE.md: Rust gateway is the canonical LLM-egress point.
    (
        "example-gateway/src/cognitive/engines/remote.rs",
        "api.anthropic.com",
    ),
    // Per CLAUDE.md: Rust gateway is the canonical LLM-egress point.
    (
        "example-gateway/src/cognitive/engines/remote.rs",
        "generativelanguage.googleapis.com",
    ),
    // Per CLAUDE.md: Rust gateway is the canonical LLM-egress point.
    (
        "example-gateway/src/cognitive/engines/remote.rs",
        "openrouter.ai",
    ),
    // Per CLAUDE.md: Rust gateway is the canonical LLM-egress point.
    ("example-gateway/src/config/gateway.rs", "api.openai.com"),
    // Per CLAUDE.md: Rust gateway is the canonical LLM-egress point.
    ("example-gateway/src/config/gateway.rs", "api.anthropic.com"),
    // Per CLAUDE.md: Rust gateway is the canonical LLM-egress point.
    (
        "example-gateway/src/config/gateway.rs",
        "generativelanguage.googleapis.com",
    ),
    // Per CLAUDE.md: Rust gateway is the canonical LLM-egress point.
    ("example-gateway/src/config/gateway.rs", "openrouter.ai"),
    // Per CLAUDE.md: Rust gateway is the canonical LLM-egress point.
    ("example-gateway/src/llm_config.rs", "api.openai.com"),
    // Per CLAUDE.md: Rust gateway is the canonical LLM-egress point.
    ("example-gateway/src/llm_config.rs", "api.anthropic.com"),
    // Per CLAUDE.md: Rust gateway is the canonical LLM-egress point.
    (
        "example-gateway/src/llm_config.rs",
        "generativelanguage.googleapis.com",
    ),
    // Per CLAUDE.md: Rust gateway is the canonical LLM-egress point.
    ("example-gateway/src/llm_config.rs", "openrouter.ai"),
    // Per CLAUDE.md: Rust gateway is the canonical LLM-egress point.
    (
        "example-gateway/src/whatsapp/ai_assist.rs",
        "api.openai.com",
    ),
    // Per CLAUDE.md: Rust gateway is the canonical LLM-egress point.
    (
        "example-gateway/src/whatsapp/ai_assist.rs",
        "api.anthropic.com",
    ),
    // Per CLAUDE.md: Rust gateway is the canonical LLM-egress point.
    (
        "example-gateway/src/whatsapp/ai_assist.rs",
        "generativelanguage.googleapis.com",
    ),
    // Per CLAUDE.md: Rust gateway is the canonical LLM-egress point.
    ("example-gateway/src/whatsapp/ai_assist.rs", "openrouter.ai"),
    // Doctor self-reference: this file defines the banned patterns, test fixtures, and
    // embedded snippet strings that contain the provider URLs by necessity.
    (
        "leio-code/src/doctors/llm_provider_egress.rs",
        "api.openai.com",
    ),
    (
        "leio-code/src/doctors/llm_provider_egress.rs",
        "api.anthropic.com",
    ),
    (
        "leio-code/src/doctors/llm_provider_egress.rs",
        "generativelanguage.googleapis.com",
    ),
    (
        "leio-code/src/doctors/llm_provider_egress.rs",
        "openrouter.ai",
    ),
    (
        "leio-code/src/doctors/llm_provider_egress.rs",
        "*.manus.space|*.manus.im",
    ),
    (
        "leio-code/src/doctors/llm_provider_egress.rs",
        "forge.manus.im",
    ),
    // manus_residue.rs self-reference: its test fixtures and pattern constants
    // reference forge.manus.im / manus.space / manus.im by design.
    (
        "leio-code/src/doctors/manus_residue.rs",
        "*.manus.space|*.manus.im",
    ),
    ("leio-code/src/doctors/manus_residue.rs", "forge.manus.im"),
    // plusoft_handover_payload_contract.rs embeds Sara OpenAI deployment needles
    // from deploy/profiles/customer_ops_unified.env by design.
    (
        "leio-code/src/doctors/plusoft_handover_payload_contract.rs",
        "api.openai.com",
    ),
    // example-api Python provider modules mirror the gateway's role: they ARE the
    // Python-side LLM-egress implementations and legitimately host provider URL config.
    (
        "example-api/example/agents/providers/anthropic_provider.py",
        "api.anthropic.com",
    ),
    (
        "example-api/example/agents/llm_provider.py",
        "api.openai.com",
    ),
    (
        "example-api/example/agents/llm_provider.py",
        "generativelanguage.googleapis.com",
    ),
    (
        "example-api/example/agents/conversation_classifier.py",
        "api.openai.com",
    ),
    (
        "example-api/example/agents/conversation_classifier.py",
        "generativelanguage.googleapis.com",
    ),
    // Deliberate offline / admin scripts — not part of the production request path.
    // sisfron/offline.py runs connectivity probes against provider endpoints.
    ("cartridges/sisfron/offline.py", "api.anthropic.com"),
    ("cartridges/sisfron/offline.py", "api.openai.com"),
    // One-off dimension generation script; not invoked at runtime.
    (
        "cartridges/assurant/scripts/generate_dimensions.py",
        "generativelanguage.googleapis.com",
    ),
    // Dev simulation script in example-gateway/scripts/; not part of the server.
    (
        "example-gateway/scripts/simulate_persona.py",
        "generativelanguage.googleapis.com",
    ),
    // vigoros-mcp TypeScript pipeline — pending migration to Example/Leio endpoints.
    // Tracked: Vigoros should use Leio endpoints for generation (MCTS + retrieval).
    ("vigoros-mcp/src/pipeline/retrieval.ts", "api.openai.com"),
    ("vigoros-mcp/src/pipeline/trace-store.ts", "api.openai.com"),
    ("vigoros-mcp/src/pipeline/research.ts", "api.anthropic.com"),
    ("vigoros-mcp/src/pipeline/generation.ts", "api.openai.com"),
    (
        "vigoros-mcp/src/pipeline/generation.ts",
        "api.anthropic.com",
    ),
    (
        "vigoros-mcp/src/pipeline/_llm_defaults.ts",
        "api.openai.com",
    ),
    (
        "vigoros-mcp/src/pipeline/_llm_defaults.ts",
        "api.anthropic.com",
    ),
    // sisfron-console ai-planning — pending Example routing.
    ("sisfron-console/src/lib/ai-planning.ts", "api.openai.com"),
    ("sisfron-console/src/lib/_llm_defaults.ts", "api.openai.com"),
    // Centralized defaults files are the authoritative mirrors for provider URLs.
    ("cartridges/_llm_defaults.py", "api.openai.com"),
    ("cartridges/_llm_defaults.py", "api.anthropic.com"),
    (
        "cartridges/_llm_defaults.py",
        "generativelanguage.googleapis.com",
    ),
    (
        "example-api/example/agents/_llm_defaults.py",
        "api.openai.com",
    ),
    (
        "example-api/example/agents/_llm_defaults.py",
        "api.anthropic.com",
    ),
    (
        "example-api/example/agents/_llm_defaults.py",
        "generativelanguage.googleapis.com",
    ),
    // Administrative and report generation scripts.
    ("scripts/llm_timeline_report.py", "api.openai.com"),
    ("scripts/llm_investigative_report.py", "api.openai.com"),
    ("scripts/llm_public_crossref_report.py", "api.openai.com"),
    ("scripts/llm_analyze_fca_outputs.py", "api.openai.com"),
    // cartridges/vigoros/generation.py — multi-provider direct calls, pending Example
    // migration. vigoros/config.py embed_base_url and EXAMPLE_VLLM_BASE_URL fallbacks
    // are removed in this same commit; the generation.py hardcoded call URLs remain
    // while the full generation routing migration is designed.
    ("cartridges/vigoros/generation.py", "api.anthropic.com"),
    ("cartridges/vigoros/generation.py", "api.openai.com"),
    (
        "cartridges/vigoros/generation.py",
        "generativelanguage.googleapis.com",
    ),
];

static BANNED_PATTERNS: LazyLock<Vec<BannedPattern>> = LazyLock::new(|| {
    let raw: &[(&str, &str)] = &[
        ("openrouter.ai", r"https?://openrouter\.ai"),
        ("api.openai.com", r"https?://api\.openai\.com"),
        ("api.anthropic.com", r"https?://api\.anthropic\.com"),
        (
            "generativelanguage.googleapis.com",
            r"https?://generativelanguage\.googleapis\.com",
        ),
        (
            "*.manus.space|*.manus.im",
            r"https?://[A-Za-z0-9_-]+\.manus\.(?:space|im)",
        ),
        ("forge.manus.im", r"forge\.manus\.im"),
    ];
    raw.iter()
        .map(|(name, pattern)| BannedPattern {
            name,
            regex: Regex::new(pattern).expect("valid regex"),
        })
        .collect()
});

/// Source-file extensions scanned for banned LLM provider URLs.
///
/// Mirrors the workspace's runtime-source surface for application code:
/// Rust, Python, TypeScript/TSX, JavaScript. Markdown, env, lockfiles, and
/// other doc/config formats are explicitly excluded by `should_scan_file`.
const SCAN_EXTENSIONS: &[&str] = &["rs", "py", "ts", "tsx", "js", "jsx", "mjs", "cjs"];

/// Directory segments that are skipped entirely during traversal, in
/// addition to whatever `.gitignore` already excludes. Belt-and-suspenders
/// for vendored / build outputs and explicit "backup" trees that may be
/// committed to the repo but should not be policy-checked.
const SKIPPED_DIR_SEGMENTS: &[&str] = &[
    "node_modules",
    "dist",
    ".next",
    "target",
    ".leio-code",
    ".claude",
    ".turbo",
    "build",
    "coverage",
    ".venv",
    "venv",
    "__pycache__",
    "vendor",
    "vendored",
    "wheels",
    // Backup/parking trees in frontend apps (`example-ops/src/api_backup/`,
    // `ops-console/src/api_backup/`). These hold legacy / archived route
    // copies that are not part of the active runtime surface.
    "api_backup",
];

pub fn doctor_llm_provider_egress(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();
    let mut violations = 0usize;
    let mut scanned_files = 0usize;

    for path in collect_scan_targets(root) {
        let rel = match path.strip_prefix(root) {
            Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };

        if is_test_path(&rel) {
            continue;
        }
        if is_env_or_doc_path(&rel) {
            continue;
        }

        let mut io_warnings: Vec<String> = Vec::new();
        let content = match read_text(&path, &mut io_warnings) {
            Some(c) => c,
            None => continue,
        };
        scanned_files += 1;

        // Cheap pre-filter: skip files with no chance of matching any banned token.
        let lower = content.to_ascii_lowercase();
        if !lower.contains("openrouter.ai")
            && !lower.contains("api.openai.com")
            && !lower.contains("api.anthropic.com")
            && !lower.contains("generativelanguage.googleapis.com")
            && !lower.contains(".manus.space")
            && !lower.contains(".manus.im")
            && !lower.contains("forge.manus.im")
        {
            continue;
        }

        for (idx, line) in content.lines().enumerate() {
            for pattern in BANNED_PATTERNS.iter() {
                if !pattern.regex.is_match(line) {
                    continue;
                }
                if is_allowlisted(&rel, pattern.name) {
                    continue;
                }
                let matched_text = pattern
                    .regex
                    .find(line)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default();
                let snippet = surrounding_snippet(line);
                violations += 1;
                warnings.push(format!(
                    "{}: direct LLM provider URL `{}` ({}) at line {} -- {}",
                    rel,
                    matched_text,
                    pattern.name,
                    idx + 1,
                    snippet
                ));
                evidence.push(EvidenceItem {
                    kind: "llm_provider_egress_violation".to_string(),
                    path: rel.clone(),
                    line: Some(idx + 1),
                    detail: format!(
                        "matched_pattern={} matched_text={} snippet={}",
                        pattern.name, matched_text, snippet
                    ),
                });
            }
        }
    }

    entities.push(json!({
        "violations": violations,
        "scanned_files": scanned_files,
        "banned_patterns": BANNED_PATTERNS
            .iter()
            .map(|p| p.name)
            .collect::<Vec<_>>(),
        "allowlist_entries": ALLOWLIST.len(),
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_llm_provider_egress"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked LLM provider egress across {} runtime files, found {} violations",
            scanned_files, violations
        ),
        confidence: if warnings.is_empty() { 0.95 } else { 0.6 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

/// Walk `root` with `.gitignore`-aware traversal (`ignore::WalkBuilder`),
/// returning the source files that should be policy-checked.
fn collect_scan_targets(root: &Path) -> Vec<PathBuf> {
    let mut builder = WalkBuilder::new(root);
    builder.hidden(false);
    // Honor every layer of git ignore semantics:
    //   - `.gitignore` files at any depth
    //   - the global gitignore (e.g. `~/.config/git/ignore`)
    //   - `.git/info/exclude`
    builder.git_ignore(true);
    builder.git_global(true);
    builder.git_exclude(true);
    // By default the `ignore` crate only honors `.gitignore` when it can
    // see a parent `.git/` directory. The doctor intentionally relaxes
    // that rule so a `.gitignore` at `root` is respected even when the
    // workspace is being scanned from a checkout that doesn't expose a
    // `.git/` (e.g. shallow snapshots used by ops wrappers, and
    // tempdir-based fixtures in this module's tests).
    builder.require_git(false);
    builder.filter_entry(|entry| {
        let Some(name) = entry.file_name().to_str() else {
            return true;
        };
        if entry.path().is_dir() {
            return !is_skipped_dir_segment(name);
        }
        true
    });

    let mut paths = Vec::new();
    for dent in builder.build().flatten() {
        let path = dent.path();
        if !path.is_file() {
            continue;
        }
        if !should_scan_file(path) {
            continue;
        }
        paths.push(path.to_path_buf());
    }
    paths
}

fn should_scan_file(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
        return false;
    };
    SCAN_EXTENSIONS
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(extension))
}

fn is_skipped_dir_segment(name: &str) -> bool {
    SKIPPED_DIR_SEGMENTS.contains(&name)
}

fn is_test_path(path: &str) -> bool {
    path.contains("/tests/")
        || path.contains("/__tests__/")
        || path.starts_with("tests/")
        || path.starts_with("__tests__/")
        || path.ends_with(".test.ts")
        || path.ends_with(".test.tsx")
        || path.ends_with(".test.py")
        || path.ends_with(".spec.ts")
        || path.ends_with(".spec.tsx")
        || path.ends_with("_test.py")
        || path.ends_with("_tests.py")
        || path.ends_with("_test.rs")
        || path.ends_with("_tests.rs")
}

fn is_env_or_doc_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.starts_with(".env")
        || name.ends_with(".env")
        || name.ends_with(".env.example")
        || name.ends_with(".env.local")
    {
        return true;
    }
    if path.ends_with(".md") || path.contains("/docs/") || path.starts_with("docs/") {
        return true;
    }
    if name.starts_with("README") {
        return true;
    }
    false
}

fn is_allowlisted(file_path: &str, pattern_name: &str) -> bool {
    ALLOWLIST.iter().any(|(allow_path, allow_pattern)| {
        file_path.ends_with(allow_path) && *allow_pattern == pattern_name
    })
}

fn surrounding_snippet(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.len() > 200 {
        format!("{}…", &trimmed[..200])
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_repo(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "leio-code-llm-egress-{name}-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create temp repo");
        root
    }

    fn write(root: &Path, path: &str, body: &str) {
        let full_path = root.join(path);
        fs::create_dir_all(full_path.parent().expect("test file parent")).expect("create parent");
        fs::write(full_path, body).expect("write test file");
    }

    #[test]
    fn clean_tree_yields_no_warnings() {
        let root = temp_repo("clean");
        write(
            &root,
            "some-app/src/lib/api.ts",
            "export const BASE_URL = 'https://api.example.test';\n",
        );
        write(
            &root,
            "some-app/agent.py",
            "BASE_URL = \"https://api.example.test\"\n",
        );

        let result = doctor_llm_provider_egress(&root);
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn drift_in_runtime_ts_is_reported() {
        let root = temp_repo("drift-ts");
        let path = "some-app/src/lib/llm.ts";
        write(&root, path, "const URL = 'https://openrouter.ai/api/v1';\n");

        let result = doctor_llm_provider_egress(&root);
        assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
        assert!(
            result.warnings[0].contains("openrouter.ai"),
            "{:?}",
            result.warnings
        );
        assert_eq!(result.evidence.len(), 1);
        assert_eq!(result.evidence[0].path, path);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn allowlisted_canonical_gateway_callsites_are_silent() {
        let root = temp_repo("allowlist");
        let path = "example-gateway/src/cognitive/engines/remote.rs";
        write(
            &root,
            path,
            "const OPENAI_BASE_URL = \"https://api.openai.com/v1\";\n",
        );

        let result = doctor_llm_provider_egress(&root);
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn allowlist_does_not_cover_other_banned_patterns_in_same_file() {
        let root = temp_repo("allowlist-narrow");
        let path = "example-gateway/src/cognitive/engines/remote.rs";
        write(
            &root,
            path,
            "const FORGE = 'https://forge.manus.im/v1';\n\
             const OPENAI = 'https://api.openai.com/v1';\n",
        );

        let result = doctor_llm_provider_egress(&root);
        // api.openai.com is allowlisted for this file, but forge.manus.im
        // (and the manus.im subdomain match) is still banned.
        assert!(!result.warnings.is_empty(), "{:?}", result.warnings);
        assert!(
            result.warnings.iter().any(|w| w.contains("forge.manus.im")),
            "{:?}",
            result.warnings
        );
        assert!(
            result.warnings.iter().all(|w| !w.contains("openrouter.ai")),
            "{:?}",
            result.warnings
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_files_are_not_scanned() {
        let root = temp_repo("tests");
        let cases = [
            "some-app/__tests__/llm.test.ts",
            "some-app/src/foo.test.ts",
            "some-app/src/foo.spec.ts",
            "some-app/src/foo.test.tsx",
            "tests/integration/test_llm.py",
            "some-cartridge/llm_test.py",
            "some-cartridge/tests/test_llm.py",
            "some-rs/src/foo_test.rs",
        ];
        let body = "const x = 'https://openrouter.ai/api/v1';\n\
                    URL = \"https://api.openai.com/v1\"\n";
        for path in cases {
            write(&root, path, body);
        }

        let result = doctor_llm_provider_egress(&root);
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn markdown_and_env_files_are_skipped() {
        let root = temp_repo("docs");
        let cases = [
            (
                "some-app/README.md",
                "fetch('https://openrouter.ai/api/v1')",
            ),
            (
                "docs/llm-providers.md",
                "BASE = 'https://api.openai.com/v1'",
            ),
            (
                "some-app/.env.example",
                "OPENROUTER_BASE_URL=https://openrouter.ai/api/v1",
            ),
        ];
        for (path, body) in cases {
            write(&root, path, body);
        }

        let result = doctor_llm_provider_egress(&root);
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rust_runtime_drift_is_reported_with_python_drift() {
        let root = temp_repo("multi-lang");
        let rs_path = "some-app/src/llm.rs";
        let py_path = "some-cartridge/agent.py";
        write(
            &root,
            rs_path,
            "const URL: &str = \"https://api.anthropic.com/v1\";\n",
        );
        write(
            &root,
            py_path,
            "BASE = \"https://generativelanguage.googleapis.com/v1beta\"\n",
        );

        let result = doctor_llm_provider_egress(&root);
        assert_eq!(result.warnings.len(), 2, "{:?}", result.warnings);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn forge_manus_im_substring_is_caught_even_without_scheme() {
        let root = temp_repo("forge");
        let path = "some-app/src/legacy.ts";
        write(
            &root,
            path,
            "// legacy reference: forge.manus.im/v1/chat\n\
             const URL = '/proxy/forge.manus.im/v1';\n",
        );

        let result = doctor_llm_provider_egress(&root);
        // Two lines mention forge.manus.im → 2 warnings.
        assert_eq!(result.warnings.len(), 2, "{:?}", result.warnings);
        assert!(result.warnings.iter().all(|w| w.contains("forge.manus.im")));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn classification_helpers() {
        assert!(is_test_path("foo/__tests__/bar.ts"));
        assert!(is_test_path("foo/bar.test.ts"));
        assert!(is_test_path("foo/bar.spec.tsx"));
        assert!(is_test_path("tests/test_llm.py"));
        assert!(is_test_path("foo/llm_test.py"));
        assert!(is_test_path("foo/llm_test.rs"));
        assert!(!is_test_path("foo/llm.ts"));

        assert!(is_env_or_doc_path("README.md"));
        assert!(is_env_or_doc_path("foo/README.md"));
        assert!(is_env_or_doc_path("docs/foo.md"));
        assert!(is_env_or_doc_path("foo/.env.example"));
        assert!(is_env_or_doc_path("foo/.env"));
        assert!(!is_env_or_doc_path("foo/llm.ts"));

        assert!(is_skipped_dir_segment("node_modules"));
        assert!(is_skipped_dir_segment("dist"));
        assert!(is_skipped_dir_segment("api_backup"));
        assert!(!is_skipped_dir_segment("src"));

        assert!(!is_allowlisted(
            "some-app/server/provider.ts",
            "forge.manus.im"
        ));
        assert!(!is_allowlisted(
            "some-app/server/provider.ts",
            "openrouter.ai"
        ));
        // Gateway egress files are allowlisted for canonical providers.
        assert!(is_allowlisted(
            "example-gateway/src/cognitive/engines/remote.rs",
            "api.openai.com"
        ));
        assert!(is_allowlisted(
            "example-gateway/src/cognitive/engines/remote.rs",
            "api.anthropic.com"
        ));
        assert!(is_allowlisted(
            "example-gateway/src/cognitive/engines/remote.rs",
            "generativelanguage.googleapis.com"
        ));
        assert!(is_allowlisted(
            "example-gateway/src/cognitive/engines/remote.rs",
            "openrouter.ai"
        ));
        assert!(is_allowlisted(
            "example-gateway/src/config/gateway.rs",
            "api.openai.com"
        ));
        assert!(is_allowlisted(
            "example-gateway/src/llm_config.rs",
            "generativelanguage.googleapis.com"
        ));
        assert!(is_allowlisted(
            "example-gateway/src/whatsapp/ai_assist.rs",
            "api.anthropic.com"
        ));
        // But the gateway allowlist must NOT cover the manus residue patterns.
        assert!(!is_allowlisted(
            "example-gateway/src/cognitive/engines/remote.rs",
            "forge.manus.im"
        ));
        assert!(!is_allowlisted(
            "example-gateway/src/cognitive/engines/remote.rs",
            "*.manus.space|*.manus.im"
        ));
        // Dev simulation script is explicitly allowlisted by name (see ALLOWLIST comment
        // "Dev simulation script ... not part of the server.") — the allowlist is keyed on
        // exact file path, so this is still a deliberate, narrow entry.
        assert!(is_allowlisted(
            "example-gateway/scripts/simulate_persona.py",
            "generativelanguage.googleapis.com"
        ));
        // But the allowlist must NOT cover other gateway files (e.g. unknown scripts,
        // tests, or unallowlisted handler files) just because they live under the gateway.
        assert!(!is_allowlisted(
            "example-gateway/scripts/some_other_script.py",
            "generativelanguage.googleapis.com"
        ));
        assert!(!is_allowlisted(
            "example-gateway/src/whatsapp/handoff.rs",
            "api.openai.com"
        ));
    }

    #[test]
    fn gitignored_directory_is_skipped() {
        // Verifies that the doctor honors `.gitignore` semantics via the
        // `ignore` crate's `WalkBuilder`. A drift file inside a gitignored
        // directory must not produce warnings.
        let root = temp_repo("gitignore-dir");
        // Mark `secret_backup/` as gitignored at the repo root.
        write(&root, ".gitignore", "secret_backup/\n");
        write(
            &root,
            "secret_backup/src/llm.ts",
            "const URL = 'https://openrouter.ai/api/v1';\n",
        );
        // Sanity-check: a sibling, non-ignored file with the same pattern
        // should still trigger a warning, so the test validates that
        // gitignore is what makes the difference (not e.g. a typo).
        write(
            &root,
            "active/src/llm.ts",
            "const URL = 'https://openrouter.ai/api/v1';\n",
        );

        let result = doctor_llm_provider_egress(&root);
        assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
        assert!(
            result.warnings[0].contains("active/src/llm.ts"),
            "{:?}",
            result.warnings
        );
        assert!(
            !result.warnings.iter().any(|w| w.contains("secret_backup/")),
            "gitignored directory must not appear in warnings: {:?}",
            result.warnings
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn gitignored_single_file_is_skipped() {
        // Single-file gitignore entry (not a directory pattern) must also
        // be honored so that ad-hoc local backups don't surface as drift.
        let root = temp_repo("gitignore-file");
        write(&root, ".gitignore", "src/legacy_backup.ts\n");
        write(
            &root,
            "src/legacy_backup.ts",
            "const URL = 'https://api.openai.com/v1';\n",
        );
        write(
            &root,
            "src/active.ts",
            "const URL = 'https://api.openai.com/v1';\n",
        );

        let result = doctor_llm_provider_egress(&root);
        assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
        assert!(
            result.warnings[0].contains("src/active.ts"),
            "{:?}",
            result.warnings
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn api_backup_directory_is_skipped_even_when_not_gitignored() {
        // The `api_backup/` segment is a workspace convention for parked
        // legacy route copies in `example-ops/src/api_backup/` and
        // `ops-console/src/api_backup/`. It is skipped explicitly
        // (in addition to whatever `.gitignore` does) so the doctor doesn't
        // surface drift from inactive code.
        let root = temp_repo("api-backup");
        write(
            &root,
            "example-ops/src/api_backup/analyze-issues/route.ts",
            "const URL = 'https://api.anthropic.com/v1/messages';\n",
        );
        write(
            &root,
            "ops-console/src/api_backup/analyze-issues/route.ts",
            "const URL = 'https://api.anthropic.com/v1/messages';\n",
        );
        // Sibling active route still gets flagged.
        write(
            &root,
            "example-ops/src/app/active/route.ts",
            "const URL = 'https://api.anthropic.com/v1/messages';\n",
        );

        let result = doctor_llm_provider_egress(&root);
        assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
        assert!(
            result.warnings[0].contains("example-ops/src/app/active/route.ts"),
            "{:?}",
            result.warnings
        );
        assert!(
            !result.warnings.iter().any(|w| w.contains("api_backup")),
            "api_backup hits must be filtered: {:?}",
            result.warnings
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn gateway_canonical_egress_paths_produce_no_warnings() {
        // Per CLAUDE.md, the Rust gateway IS the legitimate LLM-egress
        // point. Provider URLs in the four canonical gateway files must
        // not be flagged.
        let root = temp_repo("gateway-allowlist");
        let bodies = [
            (
                "example-gateway/src/cognitive/engines/remote.rs",
                "let openai = \"https://api.openai.com/v1\";\n\
                 let anthropic = \"https://api.anthropic.com/v1\";\n\
                 let google = \"https://generativelanguage.googleapis.com/v1beta\";\n\
                 let openrouter = \"https://openrouter.ai/api/v1\";\n",
            ),
            (
                "example-gateway/src/config/gateway.rs",
                "let openai = \"https://api.openai.com/v1\";\n\
                 let anthropic = \"https://api.anthropic.com/v1\";\n\
                 let google = \"https://generativelanguage.googleapis.com/v1beta\";\n",
            ),
            (
                "example-gateway/src/llm_config.rs",
                "//! OpenAI: https://api.openai.com/v1\n\
                 //! Anthropic: https://api.anthropic.com/v1\n\
                 //! Google: https://generativelanguage.googleapis.com/v1beta\n",
            ),
            (
                "example-gateway/src/whatsapp/ai_assist.rs",
                "let google = \"https://generativelanguage.googleapis.com/v1beta\";\n",
            ),
        ];
        for (path, body) in bodies {
            write(&root, path, body);
        }

        let result = doctor_llm_provider_egress(&root);
        assert!(
            result.warnings.is_empty(),
            "gateway canonical egress files must produce no warnings: {:?}",
            result.warnings
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn gateway_allowlist_is_narrow_to_named_files() {
        // Unknown gateway source files (and unknown gateway scripts) must
        // remain policy-checked. Adding a new gateway file to the allowlist
        // should require deliberate review. Named dev scripts (e.g.
        // `simulate_persona.py`) are explicitly allowlisted; an unknown
        // sibling script under `example-gateway/scripts/` must still warn.
        let root = temp_repo("gateway-narrow");
        // Allowlisted: should NOT warn.
        write(
            &root,
            "example-gateway/src/cognitive/engines/remote.rs",
            "let url = \"https://api.openai.com/v1\";\n",
        );
        // NOT allowlisted: should warn.
        write(
            &root,
            "example-gateway/src/whatsapp/handoff.rs",
            "let url = \"https://api.openai.com/v1\";\n",
        );
        // Unknown gateway script: not in the ALLOWLIST, must warn.
        write(
            &root,
            "example-gateway/scripts/some_other_script.py",
            "URL = \"https://generativelanguage.googleapis.com/v1beta/models/x:generateContent\"\n",
        );

        let result = doctor_llm_provider_egress(&root);
        assert_eq!(result.warnings.len(), 2, "{:?}", result.warnings);
        assert!(
            result.warnings.iter().any(|w| w.contains("handoff.rs")),
            "{:?}",
            result.warnings
        );
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("some_other_script.py")),
            "{:?}",
            result.warnings
        );
        assert!(
            !result.warnings.iter().any(|w| w.contains("remote.rs")),
            "remote.rs must remain allowlisted: {:?}",
            result.warnings
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn gateway_allowlist_does_not_cover_manus_residue() {
        // Even in the canonical gateway egress files, a manus.* reference
        // should still be banned. The allowlist is provider-specific.
        let root = temp_repo("gateway-vs-manus");
        let path = "example-gateway/src/cognitive/engines/remote.rs";
        write(
            &root,
            path,
            "let openai = \"https://api.openai.com/v1\";\n\
             let legacy = \"https://forge.manus.im/v1\";\n",
        );

        let result = doctor_llm_provider_egress(&root);
        // openai allowlisted, manus is not.
        assert!(!result.warnings.is_empty(), "{:?}", result.warnings);
        assert!(
            result.warnings.iter().any(|w| w.contains("forge.manus.im")),
            "{:?}",
            result.warnings
        );
        assert!(
            result
                .warnings
                .iter()
                .all(|w| !w.contains("api.openai.com")),
            "{:?}",
            result.warnings
        );

        let _ = fs::remove_dir_all(root);
    }
}
