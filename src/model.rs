//! Serializable data shapes shared across the crate.
//!
//! The two contracts that matter most to downstream consumers:
//! - [`RepoIndex`] — the on-disk index serialized to
//!   `.leio-code/index.json` by [`crate::indexer`]. Versioned by
//!   `INDEX_VERSION` in [`crate::indexer`]; bump there when this struct's
//!   serialized shape changes.
//! - [`QueryEnvelope`] — the uniform response shape returned by every
//!   `find` / `explain` / `graph` / `doctor` function. The CLI's `--json`
//!   mode and the MCP/Apps-SDK wrappers all key off this contract.
//!
//! Records (`FileRecord`, `SymbolOccurrence`, `EnvVarOccurrence`, etc.) are
//! plain data — invariants and aggregations live on `RepoIndex` impls below.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

/// Output contract version and envelope types, owned by `leio-knowledge-core`.
///
/// They live in the split-out crate so a runtime that only needs SPARQL
/// grounding reads the same contract without depending on this crate;
/// every `crate::model::*` path keeps resolving through these re-exports.
pub use leio_knowledge_core::envelope::{EvidenceItem, QueryEnvelope, SCHEMA_VERSION};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoIndex {
    pub version: u32,
    pub root: String,
    pub indexed_at: String,
    pub files: Vec<FileRecord>,
    pub deploy_targets: Vec<DeployTargetRecord>,
    pub profiles: Vec<ProfileRecord>,
    pub secret_sets: Vec<SecretSetRecord>,
    /// Root-level dotenv files (`.env`, `.env.local`, …) parsed for value
    /// resolution. Added in index version 4. `#[serde(default)]` keeps older
    /// on-disk indices loadable.
    #[serde(default)]
    pub env_files: Vec<EnvFileRecord>,
    /// Binary node registry and resolved cross-language spawn edges. Added in
    /// index version 8. `#[serde(default)]` keeps older on-disk indexes
    /// loadable.
    #[serde(default)]
    pub cross_language: CrossLanguageGraph,
    /// Kubernetes ConfigMap resources found in the repo's YAML files.
    /// `#[serde(default)]` keeps older on-disk indices loadable without a
    /// version bump.
    #[serde(default)]
    pub k8s_configmaps: Vec<K8sConfigMapRecord>,
}

/// A Kubernetes ConfigMap detected from a YAML file.
///
/// Parsed with simple string scanning — not a full YAML parser. Only
/// single-document files with top-level `data:` key-value pairs (plain
/// strings, no anchors, no multi-line block scalars) are supported.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct K8sConfigMapRecord {
    /// Repo-relative path of the YAML file.
    pub path: String,
    /// Value of `metadata.name`.
    pub map_name: String,
    /// Value of `metadata.namespace`, if present.
    pub namespace: Option<String>,
    /// Key-value pairs from the `data:` section.
    pub entries: BTreeMap<String, String>,
}

/// A root-level dotenv-style file. Populated by `indexer::parse_root_env_files`
/// and consumed by `value_resolution::resolve_value_bindings`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvFileRecord {
    /// Repo-relative path, e.g. `.env`, `.env.local`.
    pub path: String,
    /// Resolution precedence — lower wins. See `value_resolution` module docs.
    pub precedence: u8,
    pub vars: Vec<DeclaredVar>,
}

impl RepoIndex {
    pub fn all_symbols(&self) -> impl Iterator<Item = &SymbolOccurrence> {
        self.files.iter().flat_map(|file| file.symbols.iter())
    }

    pub fn all_env_vars(&self) -> impl Iterator<Item = &EnvVarOccurrence> {
        self.files.iter().flat_map(|file| file.env_vars.iter())
    }

    pub fn all_redis_keys(&self) -> impl Iterator<Item = &RedisKeyOccurrence> {
        self.files.iter().flat_map(|file| file.redis_keys.iter())
    }

    pub fn all_subprocess_calls(&self) -> impl Iterator<Item = &SubprocessCallOccurrence> {
        self.files
            .iter()
            .flat_map(|file| file.subprocess_calls.iter())
    }

    pub fn cartridge_names(&self) -> HashSet<String> {
        let mut cartridges = self
            .deploy_targets
            .iter()
            .flat_map(|target| target.cartridges.iter().cloned())
            .collect::<HashSet<_>>();

        for file in &self.files {
            let mut segments = file.path.split('/');
            let Some(root) = segments.next() else {
                continue;
            };
            if root != "cartridges" {
                continue;
            }
            let Some(name) = segments.next() else {
                continue;
            };
            if name.is_empty() || name.contains('.') {
                continue;
            }
            cartridges.insert(name.to_string());
        }

        cartridges
    }

    pub fn workspace_facets(&self) -> WorkspaceFacetSummary {
        let cartridges = self.cartridge_names();

        WorkspaceFacetSummary {
            deploy_targets: self.deploy_targets.len(),
            profiles: self.profiles.len(),
            secret_sets: self.secret_sets.len(),
            cartridges: cartridges.len(),
            has_deploy_topology: !self.deploy_targets.is_empty(),
            has_profile_envs: !self.profiles.is_empty(),
            has_secret_sets: !self.secret_sets.is_empty(),
            has_cartridges: !cartridges.is_empty(),
        }
    }
}

/// Lightweight snapshot written beside `index.json` for fast `capabilities` /
/// `status` routing without deserializing the full `files[]` payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexSummary {
    pub version: u32,
    pub root: String,
    pub indexed_at: String,
    pub file_count: usize,
    pub workspace_facets: WorkspaceFacetSummary,
    /// Size in bytes of the `index.json` this summary was stamped from. Freshness
    /// is decided by exact equality against the current `index.json` stat (not
    /// mtime ordering), so coarse filesystem mtime granularity can't make the
    /// check non-deterministic. Defaults to 0 for summaries written before this
    /// field existed — which compares unequal to any real index and forces a
    /// safe rebuild.
    #[serde(default)]
    pub index_file_bytes: u64,
    /// Modified-time (unix ms) of the `index.json` this summary was stamped from.
    /// Paired with `index_file_bytes` for the stat-equality freshness check.
    #[serde(default)]
    pub index_file_modified_ms: i128,
    /// True when the walk stopped at `LEIO_MAX_INDEX_FILES`.
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorkspaceFacetSummary {
    pub deploy_targets: usize,
    pub profiles: usize,
    pub secret_sets: usize,
    pub cartridges: usize,
    pub has_deploy_topology: bool,
    pub has_profile_envs: bool,
    pub has_secret_sets: bool,
    pub has_cartridges: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorkspaceCapabilitySummary {
    pub workspace_profile: String,
    pub workspace_facets: WorkspaceFacetSummary,
    pub find_kinds: Vec<String>,
    pub explain_kinds: Vec<String>,
    pub doctor_kinds: Vec<String>,
    pub graph_kinds: Vec<String>,
    pub export_kinds: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    pub path: String,
    pub language: SourceLanguage,
    pub bytes: usize,
    pub modified_unix_ms: i128,
    pub symbols: Vec<SymbolOccurrence>,
    pub env_vars: Vec<EnvVarOccurrence>,
    pub redis_keys: Vec<RedisKeyOccurrence>,
    #[serde(default)]
    pub subprocess_calls: Vec<SubprocessCallOccurrence>,
    /// HTTP client calls detected in this file. Added in Phase 5.
    #[serde(default)]
    pub http_calls: Vec<HttpCallOccurrence>,
    /// Per-file substrate for `CrossLanguageGraph::unresolved_edges`. Stored
    /// here so incremental rebuilds (which reuse cached `FileRecord`s for
    /// unchanged files) don't drop unresolved edges; the aggregated view on
    /// `CrossLanguageGraph` is rebuilt from these. `#[serde(default)]` keeps
    /// pre-version-10 indexes loadable.
    #[serde(default)]
    pub unresolved_edges: Vec<UnresolvedEdge>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SourceLanguage {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    CSharp,
    Razor,
    Go,
    C,
    Cpp,
    Bash,
    Java,
    Kotlin,
    Html,
    Css,
    Swift,
    Json,
    Toml,
    Yaml,
    Sql,
    Env,
    Text,
    /// Makefile-style recipes (`Makefile`, `makefile`, `GNUmakefile`). Used by
    /// `cross_language::makefile` to attribute script-invocation edges.
    Make,
    /// `package.json` script entries. Used by `cross_language::npm_scripts` to
    /// attribute script-invocation edges; the underlying file is still JSON.
    NpmScript,
    /// RDF / OWL documents (`.ttl`, `.owl`, `.jsonld`, `.rdf`, `.nt`, `.nq`,
    /// `.trig`, `.n3`). Schema terms are extracted by [`crate::ontology`].
    Rdf,
}

impl SourceLanguage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::CSharp => "csharp",
            Self::Razor => "razor",
            Self::Go => "go",
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::Bash => "bash",
            Self::Java => "java",
            Self::Kotlin => "kotlin",
            Self::Html => "html",
            Self::Css => "css",
            Self::Swift => "swift",
            Self::Json => "json",
            Self::Toml => "toml",
            Self::Yaml => "yaml",
            Self::Sql => "sql",
            Self::Env => "env",
            Self::Text => "text",
            Self::Make => "make",
            Self::NpmScript => "npm_script",
            Self::Rdf => "rdf",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SymbolOccurrence {
    pub name: String,
    pub kind: SymbolKind,
    pub path: String,
    pub line: usize,
    pub language: SourceLanguage,
    /// Fully-qualified path within the source file when the symbol is
    /// scoped by an enclosing `impl` / `class` / `trait`. Bare functions
    /// and top-level types leave this `None`. Format: `Type::method` for
    /// Rust, `Type.method` for Python / JS / TS / TSX. Always optional so
    /// older `index.json` snapshots remain readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qual_name: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Class,
    Struct,
    Enum,
    Trait,
    Interface,
    TypeAlias,
    Module,
    Constant,
    Method,
    Variable,
    /// RDF/OWL property (`owl:ObjectProperty`, `owl:DatatypeProperty`,
    /// `rdf:Property`, …).
    Property,
}

impl SymbolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Class => "class",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Interface => "interface",
            Self::TypeAlias => "type_alias",
            Self::Module => "module",
            Self::Constant => "constant",
            Self::Method => "method",
            Self::Variable => "variable",
            Self::Property => "property",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct EnvVarOccurrence {
    pub name: String,
    pub access: AccessKind,
    pub path: String,
    pub line: usize,
    pub language: SourceLanguage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SubprocessCallOccurrence {
    /// First positional argument when it's a string literal — typically the
    /// binary name or absolute path of the spawned process.
    pub binary: String,
    pub path: String,
    pub line: usize,
    pub language: SourceLanguage,
    /// True when this call's `binary` was reconstructed by the Phase 8
    /// light-dataflow pass (one-hop in-function literal-binding
    /// substitution). `resolve_spawn_edges` consults this to downgrade
    /// the confidence band from 95 to 80 (DataflowLiteral).
    /// `#[serde(default)]` keeps older indexes loadable.
    #[serde(default)]
    pub resolved_via_dataflow: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct RedisKeyOccurrence {
    pub key: String,
    pub access: AccessKind,
    pub path: String,
    pub line: usize,
    pub language: SourceLanguage,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AccessKind {
    Read,
    Write,
    Declared,
    Unknown,
}

impl AccessKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Declared => "declared",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployTargetRecord {
    pub name: String,
    pub path: String,
    pub profile: Option<String>,
    pub readiness_target: Option<String>,
    pub deploy_class: Option<String>,
    pub topology: Option<String>,
    pub ui_role: Option<String>,
    pub ui_path: Option<String>,
    pub frontend_project: Option<String>,
    pub backend_profile: Option<String>,
    pub secret_set: Option<String>,
    pub health_checks: Vec<String>,
    pub smoke_suite: Option<String>,
    pub rollback_command: Option<String>,
    pub cartridges: Vec<String>,
    pub required_integrations: Vec<String>,
    pub promotion_policy: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileRecord {
    pub name: String,
    pub path: String,
    pub vars: Vec<DeclaredVar>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretSetRecord {
    pub name: String,
    pub path: String,
    pub vars: Vec<DeclaredVar>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeclaredVar {
    pub name: String,
    pub value_preview: Option<String>,
    /// Full raw value as parsed from the source, preserving newlines for
    /// multi-line dotenv values. `#[serde(default)]` so older on-disk indexes
    /// (pre INDEX_VERSION=6) still deserialize.
    #[serde(default)]
    pub raw_value: Option<String>,
}

/// A binary that can be a target of a cross-language spawn edge.
///
/// `path` is the repo-relative path of the defining manifest or source file:
/// - `CargoExplicit` → the `Cargo.toml` that contains the `[[bin]]` section
/// - `CargoImplicit` → `src/main.rs` whose parent crate name is the binary
/// - `CargoBin` → `src/bin/<name>.rs`
/// - `NpmBin` → the `package.json` containing the `"bin"` field
/// - `PyprojectScript` → the `pyproject.toml` containing `[project.scripts]`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct BinaryNode {
    pub name: String,
    pub path: String,
    pub source: BinaryNodeSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum BinaryNodeSource {
    CargoExplicit,
    CargoImplicit,
    CargoBin,
    NpmBin,
    PyprojectScript,
}

/// A resolved cross-language edge: a subprocess spawn that references a known binary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ResolvedSpawnEdge {
    pub caller_path: String,
    pub caller_line: usize,
    pub caller_language: SourceLanguage,
    pub callee_name: String,
    /// Repo-relative path of the [`BinaryNode`] that was matched.
    pub callee_path: String,
    /// 0–100; literal name match = 95.
    pub confidence: u8,
}

/// An HTTP route declared by a server framework. Captured during indexing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct RouteRecord {
    /// Repo-relative path of the source file declaring the route.
    pub path: String,
    pub line: usize,
    /// HTTP method as uppercase string ("GET", "POST", "*" for catch-all).
    pub method: String,
    /// The literal route path as declared, e.g. "/api/users".
    pub route: String,
    /// Framework that declared the route — `"flask"`, `"fastapi"`,
    /// `"express"`, `"axum"`. Free-form to keep extensible.
    pub framework: String,
    pub language: SourceLanguage,
    /// Handler identifier where the declaration names one (axum
    /// `get(handler_fn)`, Express `app.get("/x", handler)`). FastAPI/Flask
    /// indexers resolve handlers at query time instead.
    #[serde(default)]
    pub handler: Option<String>,
    /// Auth posture inferred from the declaration site (axum
    /// `route_layer` scoping). Absent when the file declares no auth layer.
    #[serde(default)]
    pub auth_hint: Option<String>,
    /// Path parameters extracted from the route declaration. Added in
    /// Phase 6. Example: `/users/{id}/posts/{slug}` → `["id", "slug"]`.
    /// `#[serde(default)]` keeps pre-Phase-6 indexes loadable.
    #[serde(default)]
    pub path_params: Vec<String>,
}

/// An HTTP call site detected in source code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct HttpCallOccurrence {
    pub path: String,
    pub line: usize,
    pub method: String,
    /// The literal URL or path portion the call targeted. May be a full
    /// URL (`https://api.example.com/users`) or just a path (`/users`).
    pub url: String,
    pub language: SourceLanguage,
    /// Which client library was detected — `"requests"`, `"httpx"`,
    /// `"fetch"`, `"axios"`, `"reqwest"`.
    pub client: String,
    /// True if `url` contains template placeholders (`{...}`, `${...}`, `{}`).
    /// Added in Phase 6. `#[serde(default)]` keeps pre-Phase-6 indexes
    /// loadable (they only carried literal URLs).
    #[serde(default)]
    pub is_template: bool,
    /// True when this call's `url` was reconstructed by the Phase 8
    /// light-dataflow pass (one-hop in-function literal-binding
    /// substitution). The tier walk in `resolve_http_edges` consults this
    /// to downgrade Literal → DataflowLiteral (80) and Template →
    /// DataflowTemplate (75). `#[serde(default)]` keeps older indexes
    /// loadable.
    #[serde(default)]
    pub resolved_via_dataflow: bool,
}

/// How a [`ResolvedHttpEdge`] was matched. Drives confidence bands.
/// Phase 6 introduced four bands; Phase 8 adds two more for one-hop
/// in-function dataflow substitution.
/// - [`MatchKind::Literal`] (95) — exact string equality
/// - [`MatchKind::Normalized`] (85) — equal after trailing-slash /
///   double-slash normalization
/// - [`MatchKind::DataflowLiteral`] (80) — one-hop literal-binding
///   substitution produced a literal URL that then matched a literal
///   route. Added in Phase 8 (light dataflow).
/// - [`MatchKind::DataflowTemplate`] (75) — one-hop substitution
///   produced a template URL that matched a template route. Added in
///   Phase 8.
/// - [`MatchKind::Template`] (70) — segment-by-segment unification with
///   path parameters (no dataflow involved)
/// - [`MatchKind::TemplateMethodless`] (60) — template match where the
///   caller's HTTP method was unknown (`*`)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    #[default]
    Literal,
    Normalized,
    /// Phase 8: one-hop dataflow substitution produced a literal that
    /// matched a literal route.
    DataflowLiteral,
    /// Phase 8: one-hop dataflow substitution produced a template that
    /// matched a template route.
    DataflowTemplate,
    Template,
    TemplateMethodless,
}

/// A resolved client→route edge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ResolvedHttpEdge {
    pub caller_path: String,
    pub caller_line: usize,
    pub caller_language: SourceLanguage,
    pub route_path: String,
    pub route_method: String,
    pub route_source_path: String,
    pub confidence: u8,
    /// How the edge was resolved. Added in Phase 6. `#[serde(default)]`
    /// keeps pre-Phase-6 indexes loadable — they only carried literal
    /// matches, which is the default.
    #[serde(default)]
    pub match_kind: MatchKind,
}

/// Aggregates binary node discovery and resolved spawn edges for the whole repo.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CrossLanguageGraph {
    pub binaries: Vec<BinaryNode>,
    pub resolved_spawns: Vec<ResolvedSpawnEdge>,
    /// HTTP routes declared by server frameworks. Added in Phase 5.
    #[serde(default)]
    pub routes: Vec<RouteRecord>,
    /// Resolved client→route edges. Added in Phase 5.
    #[serde(default)]
    pub resolved_http_edges: Vec<ResolvedHttpEdge>,
    /// Cross-language edges the detectors noticed but could not resolve to a
    /// concrete target (dynamic first args, shell strings, Makefile variable
    /// expansions, …). Aggregated from `FileRecord::unresolved_edges`. Added
    /// in index version 10. `#[serde(default)]` keeps older indexes loadable.
    #[serde(default)]
    pub unresolved_edges: Vec<UnresolvedEdge>,
}

/// A cross-language edge candidate the detector noticed but could not
/// resolve to a concrete target. First-class record so the user can see
/// "we tried but couldn't pin this down" rather than silent drop.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct UnresolvedEdge {
    pub source_path: String,
    pub source_line: usize,
    pub source_language: SourceLanguage,
    /// Which detector emitted this — `"subprocess_spawn"`, `"http_call"`,
    /// `"script_invocation"`. Free-form string to keep the enum open.
    pub edge_kind: String,
    pub reason: UnresolvedReason,
    /// One-line trimmed snippet of the offending source. Truncated to 120
    /// chars to keep indexes compact.
    pub raw_snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum UnresolvedReason {
    /// First argument is not a string literal — variable, function call,
    /// `Path::new(...)`, etc.
    NonLiteralFirstArg,
    /// First argument is a template / f-string / concatenation
    /// (`f"bin-{x}"`, `` `bin-${x}` ``, `"a" + b`).
    TemplateOrConcat,
    /// Detected `shell=True` (Python) — the binary name is inside a shell
    /// string we won't parse.
    ShellInvocation,
    /// Make recipe with `$(VAR)` or `${VAR}` substitution.
    MakefileVariable,
    /// npm script value uses `${VAR}` / `$VAR`.
    NpmVariable,
    /// Bare `Command::new(...)` with no `use std/tokio::process::Command`
    /// in scope — could be a user-defined `Command` type.
    AmbiguousCommandImport,
    /// Phase 8: the light-dataflow pass found the variable feeding the
    /// URL or binary-name is reassigned on conditional branches between
    /// its declaration and the call site. Rather than guess which branch
    /// wins we surface the ambiguity.
    AmbiguousAssignment,
    /// Catch-all for cases we want to surface but don't have a sharper
    /// classification for yet.
    Other(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_facets_summarize_optional_topology() {
        let index = RepoIndex {
            version: 1,
            root: "/tmp/repo".to_string(),
            indexed_at: "2026-01-01T00:00:00Z".to_string(),
            files: Vec::new(),
            deploy_targets: vec![DeployTargetRecord {
                name: "backend_api".to_string(),
                path: "deploy/targets/backend_api.toml".to_string(),
                profile: None,
                readiness_target: None,
                deploy_class: None,
                topology: Some("service".to_string()),
                ui_role: None,
                ui_path: None,
                frontend_project: None,
                backend_profile: Some("backend_api".to_string()),
                secret_set: Some("backend_api".to_string()),
                health_checks: Vec::new(),
                smoke_suite: None,
                rollback_command: None,
                cartridges: vec!["customer_success".to_string(), "billing".to_string()],
                required_integrations: Vec::new(),
                promotion_policy: None,
            }],
            profiles: vec![ProfileRecord {
                name: "backend_api.env".to_string(),
                path: "deploy/profiles/backend_api.env".to_string(),
                vars: Vec::new(),
            }],
            secret_sets: vec![SecretSetRecord {
                name: "backend_api.env.example".to_string(),
                path: "deploy/secret-sets/backend_api.env.example".to_string(),
                vars: Vec::new(),
            }],
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let facets = index.workspace_facets();
        assert_eq!(facets.deploy_targets, 1);
        assert_eq!(facets.profiles, 1);
        assert_eq!(facets.secret_sets, 1);
        assert_eq!(facets.cartridges, 2);
        assert!(facets.has_deploy_topology);
        assert!(facets.has_profile_envs);
        assert!(facets.has_secret_sets);
        assert!(facets.has_cartridges);
    }

    #[test]
    fn workspace_facets_detect_cartridges_from_source_tree() {
        let index = RepoIndex {
            version: 1,
            root: "/tmp/repo".to_string(),
            indexed_at: "2026-01-01T00:00:00Z".to_string(),
            files: vec![
                FileRecord {
                    path: "cartridges/revops/router.py".to_string(),
                    language: SourceLanguage::Python,
                    bytes: 0,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "cartridges/revops/types.py".to_string(),
                    language: SourceLanguage::Python,
                    bytes: 0,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
            ],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let facets = index.workspace_facets();
        assert_eq!(facets.cartridges, 1);
        assert!(facets.has_cartridges);
        assert!(index.cartridge_names().contains("revops"));
    }
}
