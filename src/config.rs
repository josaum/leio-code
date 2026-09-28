//! Loads `.leio-code/config.toml` and resolves workspace settings.
//!
//! Settings surfaced today (env vars always win when set non-empty):
//! - `workspace_profile` → `LEIO_CODE_WORKSPACE_PROFILE`
//!   ([`PROFILE_GENERIC`], [`PROFILE_LEIO_CODE`], [`PROFILE_EXAMPLE`]). Drives
//!   the doctor registry and capability surface.
//! - `rdf.namespace` → `LEIO_CODE_RDF_NAMESPACE` (defaults to
//!   [`DEFAULT_CODE_RDF_NAMESPACE`]). Used by code-graph N-Quads export.
//! - Platform layout overrides ([`PLATFORM_API_ROOT_ENV`],
//!   [`PLATFORM_PACKAGE_ENV`], [`PLATFORM_OPS_ROOT_ENV`],
//!   [`PLATFORM_CONTRACTS_ROOT_ENV`], [`PLATFORM_CARTRIDGE_ENV`]) add one
//!   private checkout beside the neutral defaults. Unset means only those
//!   defaults are recognized.
//! - `embed.url` / `embed.model` → `LEIO_CODE_EMBED_URL` /
//!   `LEIO_CODE_EMBED_MODEL` (optional BGE-M3 encoder). Direct TEI URLs end in
//!   `/embed`; LiteLLM/OpenAI-compatible bases receive `/v1/embeddings`.
//!
//! [`repo_namespace`] resolves a stable repo-name slug for cross-graph URN
//! generation; falls back to the working-dir basename.

use std::fs;
use std::path::Path;

use serde::Deserialize;

pub const PROFILE_GENERIC: &str = "generic";
pub const PROFILE_LEIO_CODE: &str = "leio-code";
pub const PROFILE_EXAMPLE: &str = "example";
/// Published RDF vocabulary prefix for code-graph terms.
///
/// Stable published IRI so existing N-Quads and SPARQL keep resolving when
/// this default is left alone. Changing it rewrites newly exported graphs.
/// Set [`CODE_RDF_NAMESPACE_ENV`] to keep a previously exported vocabulary host.
/// Overrides must still end in `#` or `/` after normalization.
pub const DEFAULT_CODE_RDF_NAMESPACE: &str = "https://example.local/leio/code#";
/// Env var that overrides [`DEFAULT_CODE_RDF_NAMESPACE`] / `[rdf] namespace`.
pub const CODE_RDF_NAMESPACE_ENV: &str = "LEIO_CODE_RDF_NAMESPACE";
/// OpenAI-compatible or direct TEI URL for BGE-M3 (`[embed] url`).
pub const EMBED_URL_ENV: &str = "LEIO_CODE_EMBED_URL";
/// Encoder model id. Defaults to `BAAI/bge-m3`.
pub const EMBED_MODEL_ENV: &str = "LEIO_CODE_EMBED_MODEL";
/// Workspace-standard dense model. Must stay 1024-d BGE-M3.
pub const DEFAULT_EMBED_MODEL: &str = "BAAI/bge-m3";

/// Neutral API directory recognized for route mounting and Python modules.
///
/// Not a real product tree. [`PLATFORM_API_ROOT_ENV`] adds another root when
/// a checkout uses a different directory name.
pub const DEFAULT_PLATFORM_API_ROOT: &str = "example-api";
/// Neutral Python package under [`DEFAULT_PLATFORM_API_ROOT`].
pub const DEFAULT_PLATFORM_PACKAGE: &str = "example";
/// Neutral frontend app directory for import roots and topic tagging.
pub const DEFAULT_PLATFORM_OPS_ROOT: &str = "example-ops";
/// Neutral package root for `@contracts/*` import fallbacks.
pub const DEFAULT_PLATFORM_CONTRACTS_ROOT: &str = "packages/example-ops-contracts";
/// Neutral cartridge-activation variable read from the process and indexed profiles.
pub const DEFAULT_CARTRIDGE_ACTIVATION_VAR: &str = "EXAMPLE_ACTIVE_CARTRIDGES";
/// Extra API directory beside [`DEFAULT_PLATFORM_API_ROOT`].
pub const PLATFORM_API_ROOT_ENV: &str = "LEIO_CODE_PLATFORM_API_ROOT";
/// Extra Python package name beside [`DEFAULT_PLATFORM_PACKAGE`].
pub const PLATFORM_PACKAGE_ENV: &str = "LEIO_CODE_PLATFORM_PACKAGE";
/// Extra ops-app directory beside [`DEFAULT_PLATFORM_OPS_ROOT`].
pub const PLATFORM_OPS_ROOT_ENV: &str = "LEIO_CODE_PLATFORM_OPS_ROOT";
/// Extra contracts package root beside [`DEFAULT_PLATFORM_CONTRACTS_ROOT`].
pub const PLATFORM_CONTRACTS_ROOT_ENV: &str = "LEIO_CODE_PLATFORM_CONTRACTS_ROOT";
/// Extra cartridge-activation variable name beside [`DEFAULT_CARTRIDGE_ACTIVATION_VAR`].
///
/// The value is the variable's name, not its contents. Indexed profile files
/// and the process environment are both scanned for that name.
pub const PLATFORM_CARTRIDGE_ENV: &str = "LEIO_CODE_PLATFORM_CARTRIDGE_ENV";
/// Absolute prefix for machine-local markdown links. Read by `scripts/check_doc_links.py`.
pub const LINK_PREFIX_ENV: &str = "LEIO_CODE_LINK_PREFIX";

/// Whether live-infrastructure probes (Supabase reachability) should
/// self-skip instead of emitting warnings.
///
/// A *reachability* probe failing in an environment that simply has no
/// provisioned service (CI runners, air-gapped boxes) is an environment fact, not architectural
/// drift — so it must not gate `audit --strict`. Set `LEIO_SKIP_LIVE_PROBES=1`
/// (or `true`) in such environments; the live-probe doctors then return a
/// "skipped" envelope with zero warnings while every file-local invariant
/// doctor still runs and still gates strict mode.
pub fn skip_live_probes() -> bool {
    skip_live_probes_from(std::env::var("LEIO_SKIP_LIVE_PROBES").ok().as_deref())
}

/// Pure truthiness check for the `LEIO_SKIP_LIVE_PROBES` value (split out so it
/// is unit-testable without mutating process env).
pub fn skip_live_probes_from(raw: Option<&str>) -> bool {
    matches!(
        raw.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

#[cfg(test)]
mod skip_live_probes_tests {
    use super::skip_live_probes_from;

    #[test]
    fn truthy_values_enable_skip() {
        for v in ["1", "true", "TRUE", "Yes", "on", " true "] {
            assert!(skip_live_probes_from(Some(v)), "{v:?} should enable skip");
        }
    }

    #[test]
    fn falsy_or_absent_values_do_not_skip() {
        for v in [None, Some(""), Some("0"), Some("false"), Some("no")] {
            assert!(!skip_live_probes_from(v), "{v:?} should not enable skip");
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LeioConfig {
    pub version: Option<u32>,
    pub primary_tool: Option<String>,
    pub default_mode: Option<String>,
    pub workspace_profile: Option<String>,
    pub include_roots: Option<Vec<String>>,
    pub exclude_dirs: Option<Vec<String>>,
    /// Optional `[embed]` table. Absent unless a remote encoder is configured.
    #[serde(default)]
    pub embed: Option<EmbedConfig>,
    /// Optional `[rdf]` table. Absent in most repos; defaults stay published.
    #[serde(default)]
    pub rdf: Option<RdfConfig>,
    /// Optional per-doctor configuration (`[doctors.*]` tables). Absent in
    /// most repos; every nested field is optional so older configs keep
    /// loading unchanged.
    #[serde(default)]
    pub doctors: Option<DoctorsConfig>,
}

/// RDF export vocabulary under `[rdf]` in `.leio-code/config.toml`.
///
/// ```toml
/// [rdf]
/// namespace = "https://example.com/code#"
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RdfConfig {
    /// Vocabulary prefix for code-graph term IRIs. Env override:
    /// [`CODE_RDF_NAMESPACE_ENV`].
    pub namespace: Option<String>,
}

/// Per-doctor configuration under `[doctors.*]` in `.leio-code/config.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DoctorsConfig {
    #[serde(default)]
    pub disabled: Vec<String>,
    /// Explicit repository-owned scripts with BUILD_TOOLCHAIN defaults.
    #[serde(default)]
    pub rust_build_scripts: Vec<String>,
    /// `[doctors.env_contract]` — see [`EnvContractConfig`].
    pub env_contract: Option<EnvContractConfig>,
    /// `[doctors.import_boundary]` — see [`ImportBoundaryConfig`].
    pub import_boundary: Option<ImportBoundaryConfig>,
    /// `[doctors.orphan_files]` — see [`OrphanFilesConfig`].
    pub orphan_files: Option<OrphanFilesConfig>,
}

/// Allowlist for the `env-contract` doctor.
///
/// ```toml
/// [doctors.env_contract]
/// allow = ["FOO_*", "BAR"]
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EnvContractConfig {
    /// Env var names exempt from the undeclared-read check. Each entry is an
    /// exact name (`"BAR"`) or a `*`-suffixed prefix glob (`"FOO_*"`).
    pub allow: Option<Vec<String>>,
}

/// Boundary rules for the `import-boundary` doctor.
///
/// ```toml
/// [[doctors.import_boundary.rules]]
/// name = "core-isolated"
/// from_prefix = "core/"
/// deny_prefixes = ["verticals/", "apps/"]
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ImportBoundaryConfig {
    pub rules: Option<Vec<ImportBoundaryRule>>,
}

/// One `[[doctors.import_boundary.rules]]` entry.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ImportBoundaryRule {
    /// Rule name echoed in warnings; defaults to a positional label.
    #[serde(default)]
    pub name: String,
    /// Repo-relative path prefix the rule governs (e.g. `"core/"`).
    #[serde(default)]
    pub from_prefix: String,
    /// Repo-relative path prefixes that files under `from_prefix` must not
    /// import (e.g. `["verticals/", "apps/"]`).
    #[serde(default)]
    pub deny_prefixes: Vec<String>,
}

/// Scan surfaces for the `orphan-files` doctor.
///
/// ```toml
/// [doctors.orphan_files]
/// surfaces = ["src/", "lib/"]
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
pub struct OrphanFilesConfig {
    /// Exact repository-owned dynamic entrypoint exceptions.
    #[serde(default)]
    pub dynamic_entrypoints: Vec<String>,
    /// Repo-relative path prefixes to scan. The warning label is the prefix
    /// with any trailing `/` trimmed.
    pub surfaces: Option<Vec<String>>,
}

/// Remote BGE-M3 encoder under `[embed]` in `.leio-code/config.toml`.
///
/// Direct TEI endpoints use the `/embed` suffix; LiteLLM/OpenAI-compatible
/// endpoints may use their base URL and receive `/v1/embeddings` automatically.
///
/// ```toml
/// [embed]
/// url = "http://tei-bge-m3.example:8080/embed"
/// model = "BAAI/bge-m3"
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmbedConfig {
    /// Direct TEI `/embed` or LiteLLM/OpenAI-compatible base. Env [`EMBED_URL_ENV`] wins.
    pub url: Option<String>,
    /// Model id posted to `/v1/embeddings`. Defaults to [`DEFAULT_EMBED_MODEL`].
    pub model: Option<String>,
}

pub fn load_repo_config(root: &Path) -> Option<LeioConfig> {
    let path = root.join(".leio-code").join("config.toml");
    let raw = fs::read_to_string(path).ok()?;
    toml::from_str(&raw).ok()
}

pub fn repo_profile(root: &Path) -> String {
    std::env::var("LEIO_CODE_WORKSPACE_PROFILE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            load_repo_config(root)
                .and_then(|config| config.workspace_profile)
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| PROFILE_GENERIC.to_string())
        .trim()
        .to_ascii_lowercase()
}

/// Env-only embed URL for the **query** path.
///
/// Never reads the inspected repo's `[embed] url`. An untrusted tree must not
/// choose the host that receives query text or `LEIO_CODE_EMBED_API_KEY`.
pub fn embed_query_url() -> Option<String> {
    std::env::var(EMBED_URL_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("EMBEDDING_API_URL")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .and_then(|raw| normalize_http_base(&raw))
}

/// Resolve the remote embedding base URL for **ingest/export**.
///
/// Precedence: [`EMBED_URL_ENV`], then `EMBEDDING_API_URL`, then `[embed] url`.
/// Empty values are treated as unset. Scheme is added when missing.
pub fn embed_url(root: &Path) -> Option<String> {
    embed_query_url().or_else(|| {
        load_repo_config(root)
            .and_then(|config| config.embed.and_then(|embed| embed.url))
            .filter(|value| !value.trim().is_empty())
            .and_then(|raw| normalize_http_base(&raw))
    })
}

/// Encoder model id (`LEIO_CODE_EMBED_MODEL` / `[embed] model` / BGE-M3).
pub fn embed_model(root: &Path) -> String {
    std::env::var(EMBED_MODEL_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            load_repo_config(root)
                .and_then(|config| config.embed.and_then(|embed| embed.model))
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| DEFAULT_EMBED_MODEL.to_string())
}

/// Adds `http://` when the value has no scheme. Empty → `None`.
pub fn normalize_http_base(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    Some(if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    })
}

/// Normalizes an optional RDF namespace override into a usable prefix.
///
/// Empty or whitespace-only overrides fall back to
/// [`DEFAULT_CODE_RDF_NAMESPACE`]. Overrides that do not already end in `#`
/// or `/` get a trailing `#` so term IRIs stay well-formed.
pub fn resolve_code_namespace(override_value: Option<&str>) -> String {
    let Some(raw) = override_value else {
        return DEFAULT_CODE_RDF_NAMESPACE.to_string();
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return DEFAULT_CODE_RDF_NAMESPACE.to_string();
    }
    if trimmed.ends_with('#') || trimmed.ends_with('/') {
        trimmed.to_string()
    } else {
        format!("{trimmed}#")
    }
}

/// Resolves the code-graph RDF namespace: non-empty env, then `[rdf] namespace`,
/// then [`DEFAULT_CODE_RDF_NAMESPACE`].
pub fn code_rdf_namespace(root: &Path) -> String {
    let from_env = std::env::var(CODE_RDF_NAMESPACE_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if from_env.is_some() {
        return resolve_code_namespace(from_env.as_deref());
    }
    let from_config = load_repo_config(root)
        .and_then(|config| config.rdf.and_then(|rdf| rdf.namespace))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    resolve_code_namespace(from_config.as_deref())
}

/// Env-only resolver for SPARQL fallbacks that do not carry a repo root.
pub fn code_rdf_namespace_from_env() -> String {
    resolve_code_namespace(
        std::env::var(CODE_RDF_NAMESPACE_ENV)
            .ok()
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty()),
    )
}

pub fn repo_namespace(root: &Path) -> String {
    std::env::var("LEIO_CODE_REPO_NAMESPACE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            root.file_name()
                .and_then(|value| value.to_str())
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("repo")
                .to_string()
        })
}

/// One optional platform tree: API package, ops app, and contracts package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformLayout {
    pub api_root: String,
    pub package: String,
    pub ops_root: String,
    pub contracts_root: String,
}

fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn trimmed_override(value: Option<&str>, default: &str) -> String {
    value
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .unwrap_or(default)
        .trim_matches('/')
        .to_string()
}

/// Neutral layout, plus one extra layout when any override is set.
///
/// The neutral layout stays first so fixtures that use the published directory
/// names still resolve when an operator has also configured a private checkout.
pub fn platform_layouts_from(
    api_root: Option<&str>,
    package: Option<&str>,
    ops_root: Option<&str>,
    contracts_root: Option<&str>,
) -> Vec<PlatformLayout> {
    let default = PlatformLayout {
        api_root: DEFAULT_PLATFORM_API_ROOT.to_string(),
        package: DEFAULT_PLATFORM_PACKAGE.to_string(),
        ops_root: DEFAULT_PLATFORM_OPS_ROOT.to_string(),
        contracts_root: DEFAULT_PLATFORM_CONTRACTS_ROOT.to_string(),
    };
    let mut layouts = vec![default.clone()];
    let any_override = [api_root, package, ops_root, contracts_root]
        .into_iter()
        .any(|value| value.is_some_and(|item| !item.trim().is_empty()));
    if !any_override {
        return layouts;
    }
    let extra = PlatformLayout {
        api_root: trimmed_override(api_root, DEFAULT_PLATFORM_API_ROOT),
        package: trimmed_override(package, DEFAULT_PLATFORM_PACKAGE),
        ops_root: trimmed_override(ops_root, DEFAULT_PLATFORM_OPS_ROOT),
        contracts_root: trimmed_override(contracts_root, DEFAULT_PLATFORM_CONTRACTS_ROOT),
    };
    if extra != default
        && !extra.api_root.is_empty()
        && !extra.package.is_empty()
        && !extra.ops_root.is_empty()
        && !extra.contracts_root.is_empty()
    {
        layouts.push(extra);
    }
    layouts
}

/// [`platform_layouts_from`] using the process environment.
pub fn platform_layouts() -> Vec<PlatformLayout> {
    platform_layouts_from(
        nonempty_env(PLATFORM_API_ROOT_ENV).as_deref(),
        nonempty_env(PLATFORM_PACKAGE_ENV).as_deref(),
        nonempty_env(PLATFORM_OPS_ROOT_ENV).as_deref(),
        nonempty_env(PLATFORM_CONTRACTS_ROOT_ENV).as_deref(),
    )
}

/// Cartridge-activation variable names, default first, then the env override.
pub fn cartridge_activation_var_names_from(extra: Option<&str>) -> Vec<String> {
    let mut names = vec![DEFAULT_CARTRIDGE_ACTIVATION_VAR.to_string()];
    if let Some(extra) = extra.map(str::trim).filter(|value| !value.is_empty())
        && !names.iter().any(|name| name == extra)
    {
        names.push(extra.to_string());
    }
    names
}

/// [`cartridge_activation_var_names_from`] using [`PLATFORM_CARTRIDGE_ENV`].
pub fn cartridge_activation_var_names() -> Vec<String> {
    cartridge_activation_var_names_from(nonempty_env(PLATFORM_CARTRIDGE_ENV).as_deref())
}

/// `path` equals `root` or lives under `root/`.
pub fn path_has_platform_prefix(path: &str, root: &str) -> bool {
    !root.is_empty() && (path == root || path.starts_with(&format!("{root}/")))
}

/// Whether indexed text names the ops surface, including a configured ops directory.
pub fn text_mentions_ops_surface(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    if value.contains("autopilot") || value.contains("ops-console") {
        return true;
    }
    platform_layouts().iter().any(|layout| {
        let ops = layout.ops_root.to_ascii_lowercase();
        !ops.is_empty() && value.contains(&ops)
    })
}

/// Parent of a code or knowledge namespace (`…/leio/code#` → `…/leio/`).
pub fn vocabulary_root(namespace: &str) -> String {
    let trimmed = namespace.trim().trim_end_matches(['#', '/']);
    match trimmed.rfind('/') {
        Some(idx) if idx > 0 => format!("{}/", &trimmed[..idx]),
        _ => String::new(),
    }
}

/// Published vocabulary prefix, plus the prefix implied by an env namespace.
pub fn leio_iri_prefixes_from(env_namespace: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |namespace: &str| {
        let prefix = vocabulary_root(namespace);
        if !prefix.is_empty() && !out.iter().any(|item| item == &prefix) {
            out.push(prefix);
        }
    };
    push(DEFAULT_CODE_RDF_NAMESPACE);
    if let Some(raw) = env_namespace
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        push(&resolve_code_namespace(Some(raw)));
    }
    out
}

/// [`leio_iri_prefixes_from`] using [`CODE_RDF_NAMESPACE_ENV`].
pub fn leio_iri_prefixes() -> Vec<String> {
    leio_iri_prefixes_from(nonempty_env(CODE_RDF_NAMESPACE_ENV).as_deref())
}

/// True when `iri` is under a published or env-selected vocabulary prefix.
pub fn iri_is_local_vocabulary(iri: &str) -> bool {
    leio_iri_prefixes()
        .iter()
        .any(|prefix| iri.starts_with(prefix.as_str()))
}

/// Wiki section IRIs. An env namespace wins so emitted IRIs stay on that host.
pub fn wiki_iri_prefix_from(env_namespace: Option<&str>) -> String {
    let prefixes = leio_iri_prefixes_from(env_namespace);
    let root = prefixes
        .last()
        .cloned()
        .unwrap_or_else(|| vocabulary_root(DEFAULT_CODE_RDF_NAMESPACE));
    format!("{root}wiki/")
}

/// [`wiki_iri_prefix_from`] using [`CODE_RDF_NAMESPACE_ENV`].
pub fn wiki_iri_prefix() -> String {
    wiki_iri_prefix_from(nonempty_env(CODE_RDF_NAMESPACE_ENV).as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Env mutation in these tests is unsound under parallel test threads, so
    /// workspace-profile tests serialize on this lock and restore the prior value.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn restore_workspace_profile(prior: Option<String>) {
        // SAFETY: serialized test-only process environment mutation.
        unsafe {
            match prior {
                Some(value) => std::env::set_var("LEIO_CODE_WORKSPACE_PROFILE", value),
                None => std::env::remove_var("LEIO_CODE_WORKSPACE_PROFILE"),
            }
        }
    }

    #[test]
    fn repo_profile_defaults_to_generic() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prior = std::env::var("LEIO_CODE_WORKSPACE_PROFILE").ok();
        // SAFETY: serialized test-only process environment mutation.
        unsafe { std::env::remove_var("LEIO_CODE_WORKSPACE_PROFILE") };
        let root = PathBuf::from("/tmp/arbitrary-repo");
        assert_eq!(repo_profile(&root), PROFILE_GENERIC);
        restore_workspace_profile(prior);
    }

    #[test]
    fn repo_profile_returns_configured_name_unchanged() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prior = std::env::var("LEIO_CODE_WORKSPACE_PROFILE").ok();
        // SAFETY: serialized test-only process environment mutation.
        unsafe { std::env::remove_var("LEIO_CODE_WORKSPACE_PROFILE") };
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = dir.path().join(".leio-code");
        std::fs::create_dir_all(&cfg).expect("mkdir");
        std::fs::write(cfg.join("config.toml"), "workspace_profile = \"example\"\n")
            .expect("write");
        assert_eq!(repo_profile(dir.path()), PROFILE_EXAMPLE);
        std::fs::write(
            cfg.join("config.toml"),
            "workspace_profile = \"custom-pack\"\n",
        )
        .expect("write");
        assert_eq!(repo_profile(dir.path()), "custom-pack");
        restore_workspace_profile(prior);
    }

    #[test]
    fn platform_layouts_keep_neutral_default_and_optional_override() {
        let layouts = platform_layouts_from(None, None, None, None);
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].api_root, DEFAULT_PLATFORM_API_ROOT);
        let layouts = platform_layouts_from(Some("platform-api"), Some("platform_pkg"), None, None);
        assert_eq!(layouts.len(), 2);
        assert_eq!(layouts[1].api_root, "platform-api");
        assert_eq!(layouts[1].package, "platform_pkg");
        assert_eq!(layouts[1].ops_root, DEFAULT_PLATFORM_OPS_ROOT);
    }

    #[test]
    fn vocabulary_prefix_follows_namespace_override() {
        let prefixes = leio_iri_prefixes_from(Some("https://vocabulary.example/code"));
        assert!(
            prefixes
                .iter()
                .any(|prefix| prefix == "https://example.local/leio/")
        );
        assert!(
            prefixes
                .iter()
                .any(|prefix| prefix == "https://vocabulary.example/")
        );
        assert_eq!(
            wiki_iri_prefix_from(None),
            "https://example.local/leio/wiki/"
        );
        assert_eq!(
            wiki_iri_prefix_from(Some("https://vocabulary.example/code#")),
            "https://vocabulary.example/wiki/"
        );
    }

    #[test]
    fn cartridge_activation_names_keep_the_neutral_default() {
        let names = cartridge_activation_var_names_from(Some("PLATFORM_CARTRIDGES"));
        assert_eq!(names[0], DEFAULT_CARTRIDGE_ACTIVATION_VAR);
        assert_eq!(names[1], "PLATFORM_CARTRIDGES");
        assert_eq!(
            cartridge_activation_var_names_from(Some(DEFAULT_CARTRIDGE_ACTIVATION_VAR)),
            vec![DEFAULT_CARTRIDGE_ACTIVATION_VAR.to_string()]
        );
    }

    #[test]
    fn repo_namespace_defaults_to_root_basename() {
        let root = PathBuf::from("/tmp/arbitrary-repo");
        assert_eq!(repo_namespace(&root), "arbitrary-repo");
    }

    #[test]
    fn normalize_http_base_adds_scheme_and_strips_slash() {
        assert_eq!(
            normalize_http_base("tei.example:8080/").as_deref(),
            Some("http://tei.example:8080")
        );
        assert_eq!(
            normalize_http_base("https://tei.example:8080/").as_deref(),
            Some("https://tei.example:8080")
        );
        assert_eq!(normalize_http_base("  "), None);
    }

    #[test]
    fn embed_model_reads_config_when_env_unset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg_dir = dir.path().join(".leio-code");
        std::fs::create_dir_all(&cfg_dir).expect("mkdir");
        const REPO_URL: &str = "http://leio-test-repo-embed.invalid:8080";
        std::fs::write(
            cfg_dir.join("config.toml"),
            format!("version = 1\n[embed]\nmodel = \"BAAI/bge-m3\"\nurl = \"{REPO_URL}\"\n"),
        )
        .expect("write");
        assert_eq!(embed_model(dir.path()), "BAAI/bge-m3");
        let env_is_repo_url = [EMBED_URL_ENV, "EMBEDDING_API_URL"].iter().any(|name| {
            std::env::var(name)
                .ok()
                .and_then(|raw| normalize_http_base(&raw))
                .as_deref()
                == Some(REPO_URL)
        });
        if !env_is_repo_url {
            assert_ne!(
                embed_query_url().as_deref(),
                Some(REPO_URL),
                "query path must ignore inspected-repo [embed] url"
            );
        }
        if embed_query_url().is_none() {
            assert_eq!(embed_url(dir.path()).as_deref(), Some(REPO_URL));
        }
    }

    #[test]
    fn code_namespace_defaults_without_override() {
        assert_eq!(resolve_code_namespace(None), DEFAULT_CODE_RDF_NAMESPACE);
        assert_eq!(resolve_code_namespace(Some("")), DEFAULT_CODE_RDF_NAMESPACE);
        assert_eq!(
            resolve_code_namespace(Some("   ")),
            DEFAULT_CODE_RDF_NAMESPACE
        );
    }

    #[test]
    fn code_namespace_override_normalizes_trailing_separator() {
        assert_eq!(
            resolve_code_namespace(Some("https://acme.test/code")),
            "https://acme.test/code#"
        );
        assert_eq!(
            resolve_code_namespace(Some("https://acme.test/code#")),
            "https://acme.test/code#"
        );
        assert_eq!(
            resolve_code_namespace(Some("https://acme.test/code/")),
            "https://acme.test/code/"
        );
    }

    #[test]
    fn code_rdf_namespace_reads_config_when_env_unset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg_dir = dir.path().join(".leio-code");
        std::fs::create_dir_all(&cfg_dir).expect("mkdir");
        std::fs::write(
            cfg_dir.join("config.toml"),
            "[rdf]\nnamespace = \"https://acme.test/from-config\"\n",
        )
        .expect("write config");
        // This test assumes the process env does not set the override. If a
        // caller exported LEIO_CODE_RDF_NAMESPACE, env still wins by contract.
        if std::env::var(CODE_RDF_NAMESPACE_ENV)
            .ok()
            .as_deref()
            .is_none_or(|v| v.trim().is_empty())
        {
            assert_eq!(
                code_rdf_namespace(dir.path()),
                "https://acme.test/from-config#"
            );
        }
    }

    // Why: configs written before the `[doctors]` table existed must keep
    // deserializing — the doctor pack is strictly additive.
    #[test]
    fn config_without_doctors_table_parses_with_none() {
        let config: LeioConfig =
            toml::from_str("version = 1\nworkspace_profile = \"generic\"\n").expect("parse");
        assert!(config.doctors.is_none());
    }

    // Why: the env-contract allowlist is the documented escape hatch for
    // intentionally undeclared vars; its TOML shape is a public contract.
    #[test]
    fn doctors_env_contract_allow_parses() {
        let config: LeioConfig =
            toml::from_str("[doctors.env_contract]\nallow = [\"FOO_*\", \"BAR\"]\n")
                .expect("parse");
        let allow = config
            .doctors
            .and_then(|doctors| doctors.env_contract)
            .and_then(|env_contract| env_contract.allow)
            .expect("allow list");
        assert_eq!(allow, vec!["FOO_*".to_string(), "BAR".to_string()]);
    }

    // Why: import-boundary rules are the only way the doctor activates; the
    // exact table/field names here are mirrored in the config template.
    #[test]
    fn doctors_import_boundary_rules_parse() {
        let config: LeioConfig = toml::from_str(
            "[[doctors.import_boundary.rules]]\n\
             name = \"core-isolated\"\n\
             from_prefix = \"core/\"\n\
             deny_prefixes = [\"verticals/\", \"apps/\"]\n",
        )
        .expect("parse");
        let rules = config
            .doctors
            .and_then(|doctors| doctors.import_boundary)
            .and_then(|import_boundary| import_boundary.rules)
            .expect("rules");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, "core-isolated");
        assert_eq!(rules[0].from_prefix, "core/");
        assert_eq!(
            rules[0].deny_prefixes,
            vec!["verticals/".to_string(), "apps/".to_string()]
        );
    }

    // Why: a partially-written rule (missing fields) must deserialize to
    // defaults instead of failing the whole config load.
    #[test]
    fn doctors_import_boundary_rule_fields_default() {
        let config: LeioConfig =
            toml::from_str("[[doctors.import_boundary.rules]]\nname = \"only-name\"\n")
                .expect("parse");
        let rules = config
            .doctors
            .and_then(|doctors| doctors.import_boundary)
            .and_then(|import_boundary| import_boundary.rules)
            .expect("rules");
        assert_eq!(rules[0].name, "only-name");
        assert!(rules[0].from_prefix.is_empty());
        assert!(rules[0].deny_prefixes.is_empty());
    }

    // Why: orphan-files surfaces make the doctor portable to non-Example
    // repos; the `surfaces` key is the activation switch on generic profiles.
    #[test]
    fn doctors_orphan_files_surfaces_parse() {
        let config: LeioConfig =
            toml::from_str("[doctors.orphan_files]\nsurfaces = [\"src/\", \"lib/\"]\n")
                .expect("parse");
        let surfaces = config
            .doctors
            .and_then(|doctors| doctors.orphan_files)
            .and_then(|orphan_files| orphan_files.surfaces)
            .expect("surfaces");
        assert_eq!(surfaces, vec!["src/".to_string(), "lib/".to_string()]);
    }
}
