//! `leio-code init` — one-shot onboarding from a fresh clone to a working index.
//!
//! Writes a commented starter `.leio-code/config.toml` (idempotent: an existing
//! config is kept unless `--force`), builds the index via the same
//! [`build_or_update_index`] entry point the `index` subcommand uses, and
//! reports detected facets + the capability surface so a stranger knows what
//! to run next. The index always lands at the default
//! `.leio-code/index.json` under the repo root.
//!
//! The core [`run_init`] is printing-free so tests can call it directly;
//! [`init_envelope`] and [`render_init_text`] shape the `--json` envelope and
//! the human summary for the CLI.
// Rust guideline compliant 2026-02-21

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::capabilities::workspace_capabilities;
use crate::indexer::{build_or_update_index, default_index_path};
use crate::model::{QueryEnvelope, SCHEMA_VERSION, WorkspaceCapabilitySummary};

/// Starter `.leio-code/config.toml` template written by [`run_init`].
///
/// Byte-stable contract: tests assert on the exact content, and `--force`
/// restores a hand-edited config to exactly these bytes. Changing it is a
/// user-visible behavior change — update `tests/init_cmd.rs` alongside.
pub const STARTER_CONFIG: &str = r##"# LEIO Code workspace configuration
# Docs: leio-code/docs/POSITIONING.md and README.md
version = 1
workspace_profile = "generic"

# Limit indexing to specific roots (default: whole repo)
# include_roots = ["src", "services"]
# exclude_dirs = ["vendor", "fixtures"]

# Local FCA/node search uses `.leio-code/exports/arrow-nodes-v1/nodes.arrow`
# — no server required.

# Optional GPU BGE-M3 encoder (TEI / LiteLLM / OpenAI-compatible).
# Env LEIO_CODE_EMBED_URL wins. Default model is BAAI/bge-m3 (1024-d).
# [embed]
# url = "http://tei-bge-m3.example:8080"
# model = "BAAI/bge-m3"

# RDF vocabulary for code-graph exports. Default is the published
# Example namespace. Env LEIO_CODE_RDF_NAMESPACE wins when set.
# [rdf]
# namespace = "https://example.com/code#"

# Generic contract doctors (opt-in)
# [doctors.orphan_files]
# surfaces = ["src/"]
#
# [doctors.env_contract]
# allow = ["MY_OPTIONAL_VAR", "FEATURE_*"]
#
# [[doctors.import_boundary.rules]]
# name = "core-isolated"
# from_prefix = "core/"
# deny_prefixes = ["apps/"]
"##;

/// Generalized `.mcp.json` server entry echoed by `init`.
///
/// Mirrors the canonical shape in `leio-code/.mcp.json`, which additionally
/// inlines a pnpm-or-`npm ci` bootstrap; this generalized form assumes the
/// wrapper deps were installed once via `pnpm install --prod` in `leio-code/mcp/`.
pub const MCP_WIRING_HINT: &str = r#"{
  "mcpServers": {
    "leio-code": {
      "command": "bash",
      "args": ["-lc", "exec node /path/to/leio-code/mcp/index.js"],
      "cwd": "."
    }
  }
}"#;

/// Suggested follow-up commands printed in the onboarding summary.
const NEXT_STEPS: &[&str] = &[
    "leio-code find symbol <Name>",
    "leio-code context \"your task\"",
    "leio-code doctor all",
    "leio-code graph callers-of <fn>",
];

/// What `init` did with `.leio-code/config.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigAction {
    /// No config existed; the starter template was written.
    Created,
    /// A config existed and `--force` replaced it with the starter template.
    Overwritten,
    /// A config existed and was left untouched (no `--force`).
    Kept,
}

impl ConfigAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Overwritten => "overwritten",
            Self::Kept => "kept",
        }
    }
}

/// Structured result of [`run_init`]; serialized into the `--json` envelope.
#[derive(Debug, Clone, Serialize)]
pub struct InitReport {
    /// Absolute path of `.leio-code/config.toml`.
    pub config_path: String,
    pub config_action: ConfigAction,
    /// Absolute path of the index file that was built or refreshed.
    pub index_path: String,
    pub file_count: usize,
    /// Distinct source languages seen in the index, sorted.
    pub languages: Vec<String>,
    /// Distinct env var names referenced or declared across the repo.
    pub env_var_count: usize,
    /// Distinct Redis key patterns detected across the repo.
    pub redis_key_count: usize,
    /// HTTP routes declared by server frameworks (Flask, FastAPI, Express, axum).
    pub route_count: usize,
    pub capabilities: WorkspaceCapabilitySummary,
    pub next_steps: Vec<String>,
    /// Generalized `.mcp.json` server snippet ([`MCP_WIRING_HINT`]).
    pub mcp_hint: String,
}

/// Scaffold the starter config and build the index for `repo`.
///
/// Printing-free core of `leio-code init`: writes [`STARTER_CONFIG`] to
/// `.leio-code/config.toml` when missing (or unconditionally with `force`),
/// then builds/refreshes the index at the default path and summarizes the
/// detected facets and capability surface.
///
/// # Errors
///
/// Returns an error when the config directory or file cannot be written, or
/// when the index build fails (unreadable repo root, IO failure). The CLI
/// maps both to exit code 2 per `docs/output-schema.md` §4.3.
pub fn run_init(repo: &Path, force: bool) -> Result<InitReport> {
    let config_dir = repo.join(".leio-code");
    let config_path = config_dir.join("config.toml");
    let existed = config_path.exists();

    let config_action = if !existed || force {
        fs::create_dir_all(&config_dir)
            .with_context(|| format!("failed to create config dir {}", config_dir.display()))?;
        fs::write(&config_path, STARTER_CONFIG)
            .with_context(|| format!("failed to write starter config {}", config_path.display()))?;
        if existed {
            ConfigAction::Overwritten
        } else {
            ConfigAction::Created
        }
    } else {
        ConfigAction::Kept
    };

    // Ensure .leio-code/ has an internal .gitignore to ignore runtime sidecars/indexes
    let internal_gitignore = config_dir.join(".gitignore");
    if !internal_gitignore.exists() || force {
        let _ = fs::write(&internal_gitignore, "*\n!.gitignore\n!config.toml\n");
    }

    // Ensure root .gitignore ignores .leio-code/ if a git repository is detected
    let repo_gitignore = repo.join(".gitignore");
    if repo_gitignore.exists() {
        if let Ok(content) = fs::read_to_string(&repo_gitignore)
            && !content
                .lines()
                .any(|line| line.trim() == ".leio-code" || line.trim() == ".leio-code/")
        {
            let mut updated = content;
            if !updated.is_empty() && !updated.ends_with('\n') {
                updated.push('\n');
            }
            updated.push_str("\n# leio-code local index and sidecars\n.leio-code/\n");
            let _ = fs::write(&repo_gitignore, updated);
        }
    } else if repo.join(".git").exists() {
        let _ = fs::write(
            &repo_gitignore,
            "# leio-code local index and sidecars\n.leio-code/\n",
        );
    }

    let index_path = default_index_path(repo);
    let index = build_or_update_index(repo, &index_path, false).with_context(|| {
        format!(
            "failed to build or update index at {}",
            index_path.display()
        )
    })?;

    // Facets are derived from the in-memory index — distinct names, not raw
    // occurrence counts, so the numbers match what `find` can actually return.
    let languages = index
        .files
        .iter()
        .map(|file| file.language.as_str().to_string())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let env_var_count = index
        .all_env_vars()
        .map(|var| var.name.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let redis_key_count = index
        .all_redis_keys()
        .map(|key| key.key.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let route_count = index.cross_language.routes.len();
    let capabilities = workspace_capabilities(&index, repo);

    Ok(InitReport {
        config_path: config_path.display().to_string(),
        config_action,
        index_path: index_path.display().to_string(),
        file_count: index.files.len(),
        languages,
        env_var_count,
        redis_key_count,
        route_count,
        capabilities,
        next_steps: NEXT_STEPS.iter().map(|step| (*step).to_string()).collect(),
        mcp_hint: MCP_WIRING_HINT.to_string(),
    })
}

/// Wrap an [`InitReport`] in the standard [`QueryEnvelope`] for `--json`.
pub fn init_envelope(report: &InitReport, timing_ms: u128) -> QueryEnvelope {
    QueryEnvelope {
        schema_version: SCHEMA_VERSION.to_string(),
        query_id: format!(
            "init-{}",
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ),
        kind: "init".to_string(),
        summary: format!(
            "init: config {} at {}; indexed {} files into {}; workspace profile `{}`",
            report.config_action.as_str(),
            report.config_path,
            report.file_count,
            report.index_path,
            report.capabilities.workspace_profile
        ),
        confidence: 0.98,
        entities: vec![serde_json::json!(report)],
        evidence: Vec::new(),
        warnings: Vec::new(),
        meta: Some(serde_json::json!({
            "workspace_profile": report.capabilities.workspace_profile,
            "workspace_facets": report.capabilities.workspace_facets,
            "workspace_capabilities": report.capabilities,
            "next_commands": report.next_steps,
            "mcp_hint": report.mcp_hint,
        })),
        timing_ms,
    }
}

/// Render the human onboarding summary for an [`InitReport`].
pub fn render_init_text(report: &InitReport) -> String {
    let mut out = String::new();

    let config_line = match report.config_action {
        ConfigAction::Created => format!("config: wrote starter {}", report.config_path),
        ConfigAction::Overwritten => format!(
            "config: overwrote {} with the starter template",
            report.config_path
        ),
        ConfigAction::Kept => format!(
            "config: kept existing {} (pass --force to overwrite)",
            report.config_path
        ),
    };
    out.push_str(&config_line);
    out.push('\n');
    out.push_str(&format!(
        "index:  {} ({} files)\n",
        report.index_path, report.file_count
    ));

    out.push_str("\ndetected facets:\n");
    out.push_str(&format!(
        "  languages:   {}\n",
        if report.languages.is_empty() {
            "(none)".to_string()
        } else {
            report.languages.join(", ")
        }
    ));
    out.push_str(&format!(
        "  env vars:    {} distinct\n",
        report.env_var_count
    ));
    out.push_str(&format!(
        "  redis keys:  {} distinct\n",
        report.redis_key_count
    ));
    out.push_str(&format!("  http routes: {}\n", report.route_count));

    let caps = &report.capabilities;
    out.push_str(&format!(
        "\ncapabilities (profile `{}`):\n",
        caps.workspace_profile
    ));
    out.push_str(&format!("  find:    {}\n", caps.find_kinds.join(", ")));
    out.push_str(&format!("  explain: {}\n", caps.explain_kinds.join(", ")));
    out.push_str(&format!("  graph:   {}\n", caps.graph_kinds.join(", ")));
    out.push_str(&format!("  export:  {}\n", caps.export_kinds.join(", ")));
    out.push_str(&format!(
        "  doctors: {} suite(s) — run `leio-code doctor all`\n",
        caps.doctor_kinds.len()
    ));
    for note in &caps.notes {
        out.push_str(&format!("  note:    {note}\n"));
    }

    out.push_str("\nnext steps:\n");
    for step in &report.next_steps {
        out.push_str(&format!("  {step}\n"));
    }

    out.push_str(
        "\nMCP wiring — add to your project's .mcp.json (server lives in leio-code/mcp/; \
         run `pnpm install --prod` there once):\n",
    );
    out.push_str(&report.mcp_hint);

    out
}
