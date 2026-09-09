//! CLI entry point for the `leio-code` binary.
//!
//! Thin wrapper over the library crate: parses `clap` subcommands, loads or
//! builds the [`RepoIndex`](leio_code::model::RepoIndex), dispatches to the
//! relevant `find_*` / `explain_*` / `query_*` / `export_*` / `doctor_*` /
//! `search_*` function in the library, and prints the resulting
//! [`QueryEnvelope`](leio_code::model::QueryEnvelope) as either pretty text or
//! `--json` (the MCP wrapper relies on the JSON shape).
//!
//! No business logic lives here — every subcommand is a one-call delegation to
//! the library. Keep it that way so the MCP and Apps-SDK surfaces stay in sync.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;
use clap::{Parser, Subcommand, ValueEnum};
use leio_code::audit::{AuditFormat, render_audit};
use leio_code::baseline_allowlist;
use leio_code::capabilities::{workspace_capabilities, workspace_capabilities_from_facets};
use leio_code::code_graph::{default_code_graph_output_dir, export_code_graph};
use leio_code::config::repo_profile;
use leio_code::context::build_context_bundle;
use leio_code::diagnostics::{self, DiagFormat, RunMeta};
use leio_code::doctors::{run_all_doctors, run_baseline_doctors, run_ci_doctors, run_doctor};
use leio_code::export::{
    default_arrow_nodes_output_dir, default_formal_context_output_dir,
    default_hypergraph_output_dir, export_arrow_nodes, export_formal_context,
    export_formal_context_stream, export_hypergraph,
};
use leio_code::graph_query::{
    GraphDirection, query_call_graph, query_callsites_of, query_dead_code, query_importers_of,
    query_imports_in, query_resolved_importers_of, query_resolved_imports_in, query_symbols_in,
};
use leio_code::indexer::{
    build_or_update_index, default_index_path, load_fresh_index_summary, load_or_build_index,
};
use leio_code::init::{init_envelope, render_init_text, run_init};
use leio_code::model::QueryEnvelope;
use leio_code::nav::{NavAction, run_nav_page};
use leio_code::query::{
    explain_binary, explain_cartridge, explain_deploy_target, explain_env_var, explain_redis_key,
    explain_route, find_api_routes, find_binaries, find_binary_callers, find_cartridges,
    find_deploy_targets, find_docker_services, find_env_vars, find_redis_keys, find_route_callers,
    find_routes, find_subprocess_callers, find_symbols,
};
use serde_json::json;

/// The full kind catalog, derived from the CLI value enums and the doctor
/// registry. `capabilities --catalog` prints it so the MCP and Apps SDK
/// surfaces build their zod schemas from the binary itself — the JS mirror
/// arrays are gone, and kind drift becomes structurally impossible.
fn static_catalog() -> serde_json::Value {
    fn variant_names<T: clap::ValueEnum>() -> Vec<String> {
        T::value_variants()
            .iter()
            .filter_map(|variant| variant.to_possible_value())
            .map(|value| value.get_name().to_string())
            .collect()
    }
    let mut doctor_kinds: Vec<String> = [DoctorKind::All, DoctorKind::Baseline, DoctorKind::Ci]
        .iter()
        .filter_map(|kind| kind.to_possible_value())
        .map(|value| value.get_name().to_string())
        .collect();
    doctor_kinds.extend(
        leio_code::doctors::doctor_names_for_profile(leio_code::config::PROFILE_EXAMPLE)
            .into_iter()
            .map(str::to_string),
    );
    serde_json::json!({
        "find_kinds": variant_names::<FindKind>(),
        "explain_kinds": variant_names::<ExplainKind>(),
        "graph_kinds": leio_code::capabilities::GRAPH_KINDS,
        "export_kinds": variant_names::<ExportKind>(),
        "knowledge_kinds": variant_names::<KnowledgeKind>(),
        "doctor_kinds": doctor_kinds,
    })
}

#[derive(Debug, Parser)]
#[command(name = "leio-code")]
#[command(version = env!("LEIO_BUILD_VERSION"))]
#[command(about = "Agent-first code intelligence for arbitrary codebases")]
#[command(
    long_about = "Indexes one repo_root at a time. Find/graph/doctor stay local. \
Knowledge compile writes a wiki plus formal.nq; knowledge explain answers \
only through SPARQL (or refuses). Nav walks the lattice and headings. \
Every command appends a JSON-LD PROV event. Concurrent agents share locked \
sidecars; set LEIO_SESSION / --session to isolate nav."
)]
struct Cli {
    #[arg(long, global = true)]
    json: bool,

    #[arg(long, global = true, default_value = ".")]
    repo: PathBuf,

    #[arg(long, global = true)]
    index_path: Option<PathBuf>,

    /// Isolate nav (and other per-agent sidecars). Also reads `LEIO_SESSION`.
    #[arg(long, global = true)]
    session: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Index,
    /// Prepare a local conversation review; use --json for the full evidence packet.
    /// Does not index the directory, call models, or write conversation text to events.
    #[command(
        after_help = "Examples:\n  leio-code --repo /path/to/chats conversation --source chat.zip --date-order dmy\n  leio-code --repo /path/to/chats --json conversation --source messages.json --target reply"
    )]
    Conversation {
        /// UTF-8 WhatsApp TXT/ZIP or normalized JSON within --repo; repeat for up to 8 files
        #[arg(long, required = true, num_args = 1)]
        source: Vec<PathBuf>,
        /// WhatsApp numeric date convention (two-digit years mean 2000–2099)
        #[arg(long, default_value = "dmy", value_parser = ["dmy", "mdy"])]
        date_order: String,
        /// Exact exported account label, never a verified physical author
        #[arg(long)]
        account: Option<String>,
        /// Source message ID (or imported JSON id); default: latest matching message
        #[arg(long)]
        target: Option<String>,
        /// Messages per context section, bounded to 1–20
        #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u8).range(1..=20))]
        limit: u8,
    },
    /// One-shot onboarding: write a starter `.leio-code/config.toml` (kept
    /// if present unless `--force`), build the index, and print detected
    /// facets, capabilities, and next steps. The index always lands at the
    /// default `.leio-code/index.json` under the repo root.
    Init {
        /// Overwrite an existing `.leio-code/config.toml` with the starter template
        #[arg(long)]
        force: bool,
    },
    /// Quick workspace snapshot: profile, facets, and capability surface
    Status {
        /// Run a fast baseline doctor pass after the snapshot; exit non-zero if any warnings
        #[arg(long)]
        strict: bool,
        /// With `--strict`, only fail on warnings that do not match a line in this file (substring)
        #[arg(long, value_name = "FILE")]
        baseline_allowlist: Option<PathBuf>,
        /// With `--strict`, enable allowlist filtering (defaults to `.leio-code/baseline-allowlist.txt`)
        #[arg(long)]
        baseline_additive: bool,
    },
    /// Run the verification contract for the current repository profile
    Verify,
    /// Composite pre-deploy audit: status snapshot + every doctor + capabilities,
    /// rolled up into a single markdown or JSON report.
    Audit {
        /// Output format for the report
        #[arg(long, value_enum, default_value_t = AuditFormatArg::Markdown)]
        format: AuditFormatArg,
        /// Exit non-zero if any doctor reports a warning
        #[arg(long)]
        strict: bool,
        /// Write the report to this file (defaults to stdout)
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
    },
    /// Print the static kind catalog (CLI enums + doctor registry) without
    /// touching any index. The MCP / Apps SDK surfaces consume this at
    /// startup to build their schemas from the binary itself.
    Capabilities {
        #[arg(long)]
        catalog: bool,
    },
    /// Build an agent-ready context bundle for a task
    Context {
        /// Natural language task, symbol, path, env var, Redis key, or feature area
        task: String,
        /// Maximum number of ranked files/entities to include
        #[arg(long, default_value = "8")]
        limit: usize,
        /// Emit the exhaustive bundle: uncapped per-file entity lists and the
        /// full workspace capabilities block in meta. The default bundle is a
        /// diet (top-3 per-file entities plus counts).
        #[arg(long)]
        full: bool,
    },
    Find {
        /// Entity kind (symbol, env-var, redis-key, route, etc.) or search needle (defaults to symbol).
        #[arg(value_name = "KIND_OR_NEEDLE")]
        first: String,
        /// Needle when kind is explicitly specified as the first argument.
        #[arg(value_name = "NEEDLE")]
        second: Option<String>,
        /// Filter HTTP routes by method (e.g. `--method POST`). Only honored
        /// by `find route`; ignored for other kinds.
        #[arg(long, value_name = "METHOD")]
        method: Option<String>,
        /// Output format. `text` (default) is human-readable; `json` is the
        /// existing JSON envelope; `jsonld` adds @context/@id/@type for
        /// filtering and SPARQL ingest.
        #[arg(long, value_enum)]
        format: Option<OutputFormatArg>,
        /// Optional jq-style filter applied to the rendered output. Supports a
        /// small subset — see `leio_code::jsonld` for the grammar. Implies
        /// `--format=jsonld` unless `--format` is set explicitly.
        #[arg(long, value_name = "EXPR")]
        r#where: Option<String>,
    },
    Explain {
        #[arg(value_enum)]
        kind: Option<ExplainKind>,
        needle: Option<String>,
        /// Reveal raw values for secret-keyed variables (off by default).
        /// Affects `explain env-var` and `explain deploy-target`. When stdout
        /// is not a TTY, this flag is refused unless paired with
        /// `--i-know-what-i-am-doing` to prevent accidentally piping raw
        /// secrets into log collectors or CI captures.
        #[arg(long)]
        show_secrets: bool,
        /// Acknowledge the risk of `--show-secrets` outside a TTY context.
        /// Required when stdout is redirected to a file or pipe.
        #[arg(long)]
        i_know_what_i_am_doing: bool,
        /// Output format. `text` (default) is human-readable; `json` is the
        /// existing JSON envelope; `jsonld` adds @context/@id/@type for
        /// filtering and SPARQL ingest.
        #[arg(long, value_enum)]
        format: Option<OutputFormatArg>,
        /// Optional jq-style filter applied to the rendered output. Supports a
        /// small subset — see `leio_code::jsonld` for the grammar. Implies
        /// `--format=jsonld` unless `--format` is set explicitly.
        #[arg(long, value_name = "EXPR")]
        r#where: Option<String>,
        /// Read a stream of JSON-LD entities from stdin and re-run explain on
        /// each one. Closes the `find → jq → explain` loop. Accepts a whole
        /// envelope, a JSON array, or a stream of concatenated entity
        /// objects. When set, positional `<kind>` and `<needle>` args are
        /// ignored. `--where` is rejected (filter upstream with jq).
        #[arg(long)]
        stdin: bool,
    },
    Export {
        #[arg(value_enum)]
        kind: ExportKind,
        #[arg(long)]
        output_dir: Option<PathBuf>,
        /// Which graph projection to export. Only meaningful for
        /// `formal-context`; rejected for other kinds. Default: `file`.
        #[arg(long, value_enum, default_value_t = FormalContextObjectKindArg::File)]
        object_kind: FormalContextObjectKindArg,
        /// Output format for `formal-context`. `bundle` (default) keeps the
        /// existing sidecar-JSONL contract under
        /// `.leio-code/exports/formal-context-v1/`; `json` / `arrow` are the
        /// streamed shapes described in `docs/fca-induction-design.md`.
        #[arg(long, value_enum, default_value_t = FormalContextFormatArg::Bundle)]
        format: FormalContextFormatArg,
        /// Destination for the streamed `formal-context` output. Optional
        /// for `--format=json` (defaults to stdout); required for
        /// `--format=arrow`. Rejected when `--format=bundle` (use
        /// `--output-dir`).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    Doctor {
        #[arg(value_enum)]
        kind: DoctorKind,
        /// Output format. `text` (default) is human-readable; `json` and `sarif`
        /// are machine formats stamped with index version + commit SHA so reports
        /// are reproducible. SARIF 2.1.0 is consumable by GitHub code scanning.
        #[arg(long, value_enum, default_value_t = DiagFormatArg::Text)]
        format: DiagFormatArg,
        /// Print the spec citation, conceptual fix, and matching violation list
        /// for a single rule. Use `--explain list` to see every documented
        /// rule id. Ignores `--format`; output is always pretty text.
        #[arg(long, value_name = "RULE_ID")]
        explain: Option<String>,
        /// Emit a template unified-diff patch for the named rule when a
        /// mechanical fix is available (suggest_confidence = High). For rules
        /// with Medium confidence, explains why no diff is available. For Low-
        /// confidence rules, same. Never writes to disk — diff is stdout only.
        /// Exit 0 on success, 1 on unknown rule.
        #[arg(long, value_name = "RULE_ID")]
        suggest: Option<String>,
    },
    /// Query the local compiled wiki, or SPARQL-ground the formal graph.
    ///
    /// `compile` writes the Arrow wiki and `formal.nq`. `explain` answers
    /// only through SPARQL (or refuses). `sparql` is raw. `status` includes
    /// formal-graph health. Adaptive/text are lexical, not a proof.
    Knowledge {
        #[arg(value_enum)]
        kind: KnowledgeKind,
        needle: Option<String>,
        #[arg(long, default_value = "8")]
        limit: usize,
    },
    Graph {
        #[arg(value_enum)]
        kind: GraphKind,
        /// Required for all graph kinds except `dead-code`, which scans the whole index.
        needle: Option<String>,
        /// Maximum number of callers a symbol may have and still be flagged as dead code (default 0).
        /// Useful for catching near-orphans (1-2 callers, all in tests).
        #[arg(long, default_value = "0")]
        threshold: usize,
    },
    /// Stateful code, heading and FCA cursor: goto / select / callers / parent / back.
    ///
    /// Position is persisted under `.leio-code/` (shared `nav-session.json`,
    /// or `sessions/nav-<id>.json` when `--session` / `LEIO_SESSION` is set).
    /// Walk commands list results; use `select --index <n>` to move.
    Nav {
        #[arg(value_enum)]
        kind: NavKind,
        /// Required for `goto`: symbol, returned graph symbol URN, file, heading or FCA concept.
        /// Optional query for `explain`.
        needle: Option<String>,
        /// Zero-based result index within the last displayed page, for `select`.
        #[arg(long)]
        index: Option<usize>,
        /// Results per page, clamped to 1–100. Keep the same limit when continuing.
        #[arg(long, default_value = "20")]
        limit: usize,
        /// Continue with the next_offset returned by the previous listing (same query and limit).
        #[arg(long, default_value = "0")]
        offset: usize,
    },
    /// Watch the repo and reindex incrementally on file changes.
    /// Sync the installation from the canonical checkout: fast-forward
    /// main, rebuild, reinstall the binaries, and refresh the harness
    /// plugin caches (Codex / Claude Code) through their own CLIs.
    Update {
        /// Skip refreshing harness plugin caches.
        #[arg(long)]
        no_plugins: bool,
        /// Rebuild even when the checkout is already current.
        #[arg(long)]
        force: bool,
    },
    Watch {
        /// Debounce window in ms before reindex fires.
        #[arg(long, default_value = "500")]
        debounce_ms: u64,
        /// Suppress per-reindex log lines on stderr.
        #[arg(long)]
        quiet: bool,
    },
    /// Pure-Arrow knowledge base: fuse repos/docs folders into one queryable
    /// collection (lexical + vector, no server).
    Kb {
        #[command(subcommand)]
        action: KbCommand,
    },
}

fn print_json(value: &serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[derive(Debug, Subcommand)]
enum KbCommand {
    /// Register a source (repo or docs folder) under a knowledge base.
    Add {
        name: String,
        #[arg(long)]
        source: PathBuf,
    },
    /// Remove a registered source.
    Remove { name: String, source: String },
    /// List registered knowledge bases and their sources.
    List,
    /// Build/refresh the collection (chunk, hash, embed new chunks).
    Build {
        name: String,
        /// Root used for embeddings config resolution (defaults to the
        /// first registered source).
        #[arg(long)]
        embed_repo: Option<PathBuf>,
    },
    /// Hybrid top-k query over the collection.
    Query {
        name: String,
        query: String,
        #[arg(long, default_value_t = 5)]
        top_k: usize,
        #[arg(long)]
        source: Option<String>,
    },
    /// Delete the collection and its registration.
    Drop { name: String },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum AuditFormatArg {
    Markdown,
    Json,
}

impl From<AuditFormatArg> for AuditFormat {
    fn from(value: AuditFormatArg) -> Self {
        match value {
            AuditFormatArg::Markdown => AuditFormat::Markdown,
            AuditFormatArg::Json => AuditFormat::Json,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum DiagFormatArg {
    Text,
    Json,
    Sarif,
}

impl From<DiagFormatArg> for DiagFormat {
    fn from(value: DiagFormatArg) -> Self {
        match value {
            DiagFormatArg::Text => DiagFormat::Text,
            DiagFormatArg::Json => DiagFormat::Json,
            DiagFormatArg::Sarif => DiagFormat::Sarif,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormatArg {
    /// Human-readable (default).
    Text,
    /// Existing pretty JSON envelope.
    Json,
    /// JSON-LD with `@context`/`@id`/`@type`; filter with jq or ingest via oxigraph.
    Jsonld,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum FindKind {
    Symbol,
    EnvVar,
    RedisKey,
    DeployTarget,
    Cartridge,
    ApiRoute,
    DockerService,
    SubprocessCaller,
    /// Cross-language binary node registry (Cargo `[[bin]]`, npm `bin`,
    /// pyproject `[project.scripts]`). Needle is an optional substring filter.
    Binary,
    /// Cross-language HTTP route registry (Flask, FastAPI, Express, axum).
    /// Needle is an optional substring filter; combine with `--method`.
    Route,
    /// Dispatched caller lookup: needle starting with `/` lists HTTP callers
    /// of that route; otherwise lists subprocess callers of that binary name.
    Callers,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ExplainKind {
    DeployTarget,
    EnvVar,
    RedisKey,
    Cartridge,
    /// Cross-language binary explanation: declaration + every resolved caller
    /// + best-effort unresolved-edge matches grouped by reason.
    Binary,
    /// Cross-language route explanation: every declaration + every resolved
    /// HTTP caller.
    Route,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ExportKind {
    FormalContext,
    CodeGraph,
    ArrowNodes,
    /// Incidence hypergraph JSON (`example.formal_context_hypergraph.v1`) from the same formal context as `formal-context`
    Hypergraph,
}

/// CLI mirror of [`leio_code::export::FormalContextObjectKind`]. Six
/// projections supported. Default `file`. Only meaningful for
/// `export formal-context` — rejected at dispatch for other kinds.
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum FormalContextObjectKindArg {
    File,
    Cartridge,
    Binary,
    Route,
    EnvVar,
    DeployTarget,
}

impl FormalContextObjectKindArg {
    fn to_export(self) -> leio_code::export::FormalContextObjectKind {
        use leio_code::export::FormalContextObjectKind as K;
        match self {
            Self::File => K::File,
            Self::Cartridge => K::Cartridge,
            Self::Binary => K::Binary,
            Self::Route => K::Route,
            Self::EnvVar => K::EnvVar,
            Self::DeployTarget => K::DeployTarget,
        }
    }
}

/// CLI mirror of [`leio_code::export::FormalContextFormat`].
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum FormalContextFormatArg {
    /// Legacy: sidecar JSONL files under `.leio-code/exports/formal-context-v1/`.
    Bundle,
    /// Streamed JSON document (design-doc shape) to stdout or `--out`.
    Json,
    /// Streamed Arrow IPC RecordBatch (one row per incidence) to `--out`.
    Arrow,
}

impl FormalContextFormatArg {
    fn to_export(self) -> leio_code::export::FormalContextFormat {
        use leio_code::export::FormalContextFormat as F;
        match self {
            Self::Bundle => F::Bundle,
            Self::Json => F::Json,
            Self::Arrow => F::Arrow,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum DoctorKind {
    All,
    /// Same checks as `status --strict` (fast parallel preset)
    Baseline,
    /// Baseline + semantic-wiring, auth-brokering, inference-contracts
    Ci,
    Deploy,
    DeployBundleCriticalKeys,
    DuckdbContract,
    SemanticWiring,
    TessellationContract,
    SessionHotState,
    SisfronOodaRuntime,
    SisfronSimulationDurability,
    OnboardingProjection,
    OnboardingDrift,
    OrphanFiles,
    PactoWebhookAllowlistPopulated,
    PublishableCrate,
    VendoredCrateProvenance,
    RevopsTenantGate,
    RouteProjection,
    ScriptPathExistence,
    SecretSetParity,
    AuthBrokering,
    CompositionResolver,
    EventDurability,
    EventEnvelope,
    FlightAuth,
    FlightRuntimeAuth,
    GatewayOarBoundary,
    GatewayOcrPipeline,
    OcrModelsOnDisk,
    OcrCanonicalLayout,
    GlinerSharedSurface,
    #[value(name = "health-audit-ans-xsd")]
    HealthAuditAnsXsd,
    HealthAuditAuth,
    HealthAuditEmbedding,
    HealthAuditSentinel,
    #[value(name = "luminai-health-audit-isolation")]
    LuminaiHealthAuditIsolation,
    HealthAuditRouterSize,
    HealthAuditContractAirgap,
    HealthAuditWorkerRuntime,
    GlosaContract,
    ClinicalContract,
    GenerateAsyncFlight,
    FlightServerZeroCopy,
    FlightServerContextIsolation,
    HealthAuditEvidenceClass,
    HealthAuditAuditTrailDurability,
    HealthAuditEngineHealth,
    FlightChannelReuse,
    GrpcMessageSize,
    AuthJwtCompat,
    AuthSessionContinuity,
    HealthAuditRedisPrimacy,
    InferenceContracts,
    LlmProviderEgress,
    ManusResidue,
    FrontendEngineClient,
    FrontendReadiness,
    ConversationIdentity,
    VigorosSwarm,
    CartridgeBoundary,
    EgressCompliance,
    RedisKeyHygiene,
    ModalAppNamingCoherence,
    AlignDockerfilePathDepCoherence,
    ServerDockerfileContextCoherence,
    RustToolchainPinCoherence,
    RustDependencyBaseline,
    ProdSurfaceHygiene,
    OaeiDocConsistency,
    OfficeParsersNodeParity,
    OfficeParsersArrowIpc,
    OfficeParsersClippyGate,
    OfficeParsersDocCoverage,
    FastWheelhouseContract,
    SelfContract,
    SkillContract,
    TypescriptConfigHygiene,
    // Drift-pattern doctors added 2026-05-04
    AgentSeedUpdateCompleteness,
    StartupHookCartridgeCoverage,
    WorkerMemBudget,
    ModelCacheContract,
    BareCartridgeRedisKey,
    EgressSuccessWithoutWamid,
    ActiveCartridgesEnvDrift,
    AssurantSeedWiring,
    #[value(name = "assurant-ops-production")]
    AssurantOpsProduction,
    OcrFlightWiring,
    ParseHybridFallback,
    LayoutFastSpectralContract,
    LayoutContractPlatform,
    PdfPipelineDivergence,
    #[value(name = "pdf-studio-env-coherence")]
    PdfStudioEnvCoherence,
    AudioHandlerTimeout,
    ChatwootPratiqueSync,
    KbCollectionExists,
    FitnessMemberUnitResolver,
    #[value(name = "lgpd-outbound-filter")]
    LgpdOutboundFilter,
    #[value(name = "evo-credential-isolation")]
    EvoCredentialIsolation,
    #[value(name = "c4gym-evo-integration")]
    C4GymEvoIntegration,
    #[value(name = "jaipay-pacto-gcp-webhook")]
    JaipayPactoGcpWebhook,
    #[value(name = "jaipay-supabase-session-pooler")]
    JaipaySupabaseSessionPooler,
    #[value(name = "supabase-runtime-shape")]
    SupabaseRuntimeShape,
    #[value(name = "compose-worker-mem-budget")]
    ComposeWorkerMemBudget,
    #[value(name = "supabase-project-liveness")]
    SupabaseProjectLiveness,
    #[value(name = "liz-jaipay-pacto-lookup-contract")]
    LizJaiPayPactoLookupContract,
    #[value(name = "plusoft-routing-contract")]
    PlusoftRoutingContract,
    #[value(name = "plusoft-handover-payload-contract")]
    PlusoftHandoverPayloadContract,
    #[value(name = "plusoft-transcript-fidelity")]
    PlusoftTranscriptFidelity,
    #[value(name = "pacto-drain-dependency")]
    PactoDrainDependency,
    #[value(name = "test-patch-target-integrity")]
    TestPatchTargetIntegrity,
    #[value(name = "test-route-mount-integrity")]
    TestRouteMountIntegrity,
    #[value(name = "pratique-cobranca-json-contract")]
    PratiqueCobrancaJsonContract,
    #[value(name = "ontology-price-consistency")]
    OntologyPriceConsistency,
    RecipientLimbo,
    #[value(name = "sara-assurant-egress-contract")]
    SaraAssurantEgressContract,
    #[value(name = "whatsapp-bsuid-webhook")]
    WhatsAppBsuidWebhook,
    #[value(name = "whatsapp-bsuid-crm")]
    WhatsAppBsuidCrm,
    #[value(name = "whatsapp-display-name-only")]
    WhatsAppDisplayNameOnly,
    #[value(name = "revops-snapshot-schema-sync")]
    RevopsSnapshotSchemaSync,
    TenantIdentity,
    TenantOverrideContract,
    /// Python↔gateway operator cross-tenant (OPS_OPERATOR_TENANTS) parity
    OperatorTenantParity,
    /// Slop gate for deck/report repos with a spine.json
    Slop,
    /// Generic: env vars read by code but declared nowhere in the repo
    EnvContract,
    /// Generic: config-driven import boundary rules
    ImportBoundary,
    /// Generic: committed repo noise (conflict leftovers, ignore-worthy files, large dup sets)
    #[value(name = "repo-hygiene")]
    RepoHygiene,
    /// Portable: bounded Codex multi-agent roles, hooks, and repository contract
    #[value(name = "codex-orchestration")]
    CodexOrchestration,
    /// Portable: first-party LEIO release surfaces and changelog stay version-aligned
    #[value(name = "leio-release-coherence")]
    LeioReleaseCoherence,
    /// Stable env/redis co-occurrence invariants induced from the code graph
    InducedInvariants,
    /// Authenticated, tenant-scoped Rust/Python/compose platform boundary
    #[value(name = "platform-runtime-trust-boundary")]
    PlatformRuntimeTrustBoundary,
    /// artifacts/ reorg: centralized wheels, repointed consumers, manifest sync
    ArtifactReuse,
    /// Composite Python control-plane ↔ Rust data-plane boundary gate
    #[value(name = "py-rust-boundary")]
    PyRustBoundary,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::ValueEnum;

    #[test]
    fn doctor_kind_exposes_supabase_runtime_shape() {
        let names: Vec<String> = DoctorKind::value_variants()
            .iter()
            .filter_map(|kind| kind.to_possible_value())
            .map(|value| value.get_name().to_string())
            .collect();

        assert!(
            names.iter().any(|name| name == "supabase-runtime-shape"),
            "expected DoctorKind to expose supabase-runtime-shape; got {names:?}"
        );
    }

    #[test]
    fn doctor_kind_exposes_platform_runtime_trust_boundary() {
        let names: Vec<String> = DoctorKind::value_variants()
            .iter()
            .filter_map(|kind| kind.to_possible_value())
            .map(|value| value.get_name().to_string())
            .collect();

        assert!(
            names
                .iter()
                .any(|name| name == "platform-runtime-trust-boundary"),
            "expected DoctorKind to expose platform-runtime-trust-boundary; got {names:?}"
        );
    }

    #[test]
    fn doctor_kind_exposes_whatsapp_display_name_only() {
        let names: Vec<String> = DoctorKind::value_variants()
            .iter()
            .filter_map(|kind| kind.to_possible_value())
            .map(|value| value.get_name().to_string())
            .collect();

        assert!(
            names
                .iter()
                .any(|name| name == "whatsapp-display-name-only"),
            "expected DoctorKind to expose whatsapp-display-name-only; got {names:?}"
        );
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum KnowledgeKind {
    /// Adaptive wiki-style retrieval: title -> topic -> lexical -> hybrid vector fallback
    Adaptive,
    /// Lexical markdown/wiki search over the scoped knowledge corpus
    Text,
    /// Scoped collection health and compiled wiki stats
    Status,
    /// Compile markdown/text in the repo into `.leio-code/exports/knowledge-v1/`
    Compile,
    /// SPARQL-grounded answer with lattice trail; refuses when unbound
    Explain,
    /// Raw SPARQL against the formal knowledge graph
    Sparql,
    /// JSON-line SPARQL over one loaded store (empty results are valid)
    Exec,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum NavKind {
    Here,
    Goto,
    Select,
    Callers,
    Callees,
    Neighbors,
    Related,
    Parent,
    Child,
    Peer,
    Align,
    Back,
    Forward,
    Reset,
    /// SPARQL-grounded proof from the current heading or a needle
    Explain,
}

impl From<NavKind> for NavAction {
    fn from(value: NavKind) -> Self {
        match value {
            NavKind::Here => Self::Here,
            NavKind::Goto => Self::Goto,
            NavKind::Select => Self::Select,
            NavKind::Callers => Self::Callers,
            NavKind::Callees => Self::Callees,
            NavKind::Neighbors => Self::Neighbors,
            NavKind::Related => Self::Related,
            NavKind::Parent => Self::Parent,
            NavKind::Child => Self::Child,
            NavKind::Peer => Self::Peer,
            NavKind::Align => Self::Align,
            NavKind::Back => Self::Back,
            NavKind::Forward => Self::Forward,
            NavKind::Reset => Self::Reset,
            NavKind::Explain => Self::Explain,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum GraphKind {
    CallersOf,
    CalleesOf,
    CallsitesOf,
    SymbolsIn,
    ImportsIn,
    /// Reverse import lookup by raw statement, module specifier, imported name, or file path.
    ImportersOf,
    ResolvedImportsIn,
    ResolvedImportersOf,
    /// Symbols with 0 callers AND files with 0 importers. Skips `main`, route
    /// handlers, default exports, lifecycle hooks, and tests.
    DeadCode,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(session) = cli.session.as_deref() {
        leio_code::sidecar::pin_session(session);
    }
    let repo = if matches!(&cli.command, Command::Conversation { .. }) {
        cli.repo.canonicalize()?
    } else {
        canonical_repo(&cli.repo)?
    };
    leio_code::jsonld::bind_repo(&repo);
    let index_path = cli
        .index_path
        .clone()
        .unwrap_or_else(|| default_index_path(&repo));

    match cli.command {
        Command::Conversation {
            source,
            date_order,
            account,
            target,
            limit,
        } => {
            let envelope = leio_code::conversation::prepare(
                &repo,
                &source,
                &date_order,
                account.as_deref(),
                target.as_deref(),
                limit as usize,
            )?;
            // This family deliberately bypasses the generic event journal:
            // transcript excerpts are private source data, not repository telemetry.
            if cli.json {
                println!("{}", serde_json::to_string(&envelope)?);
            } else {
                print!("{}", leio_code::conversation::render_text(&envelope)?);
            }
        }
        Command::Index => {
            let index = build_or_update_index(&repo, &index_path, false)?;
            let workspace_facets = index.workspace_facets();
            let capabilities = workspace_capabilities(&index, &repo);
            let workspace_profile = capabilities.workspace_profile.clone();
            let envelope = QueryEnvelope {
                schema_version: leio_code::model::SCHEMA_VERSION.to_string(),
                query_id: format!(
                    "index-{}",
                    time::OffsetDateTime::now_utc().unix_timestamp_nanos()
                ),
                kind: "index".to_string(),
                summary: format!(
                    "indexed {} files, {} deploy targets, {} profiles, {} secret sets",
                    index.files.len(),
                    index.deploy_targets.len(),
                    index.profiles.len(),
                    index.secret_sets.len()
                ),
                confidence: 0.98,
                entities: vec![serde_json::json!({
                    "root": index.root,
                    "indexed_at": index.indexed_at,
                    "file_count": index.files.len(),
                    "deploy_targets": index.deploy_targets.len(),
                    "profiles": index.profiles.len(),
                    "secret_sets": index.secret_sets.len(),
                    "index_path": index_path.display().to_string(),
                })],
                evidence: Vec::new(),
                warnings: Vec::new(),
                meta: Some(serde_json::json!({
                    "workspace_profile": workspace_profile,
                    "workspace_facets": workspace_facets,
                    "workspace_capabilities": capabilities,
                })),
                timing_ms: 0,
            };
            print_and_maybe_fail(&envelope, cli.json, false)?;
        }
        Command::Init { force } => {
            let started = std::time::Instant::now();
            match run_init(&repo, force) {
                Ok(report) => {
                    if cli.json {
                        let envelope = init_envelope(&report, started.elapsed().as_millis());
                        print_envelope(&envelope, true);
                    } else {
                        println!("{}", render_init_text(&report));
                    }
                }
                Err(err) => {
                    // Exit 2 per docs/output-schema.md §4.3 (config error):
                    // config-write or index-build failures during onboarding.
                    eprintln!("Error: leio-code init failed: {err:#}");
                    std::process::exit(2);
                }
            }
        }
        Command::Status {
            strict,
            baseline_allowlist,
            baseline_additive,
        } => {
            let started = std::time::Instant::now();
            let index = load_or_build_index(&repo, &index_path).with_context(|| {
                format!(
                    "failed to build or update index at {}",
                    index_path.display()
                )
            })?;
            let capabilities = workspace_capabilities(&index, &repo);
            let workspace_facets = index.workspace_facets();
            let workspace_profile = capabilities.workspace_profile.clone();
            let members = leio_code::sidecar::workspace_members(&repo);
            let session = leio_code::sidecar::session_report(&repo);
            let summary_file = leio_code::indexer::load_fresh_index_summary(&repo, &index_path);
            let truncated = summary_file.as_ref().is_some_and(|row| row.truncated);
            let member_note = if members.len() >= 2 {
                format!("; {} workspace packages", members.len())
            } else {
                String::new()
            };
            let status_summary = format!(
                "workspace `{}` indexed {} files{member_note}; {} deploy targets, {} profiles, {} secret sets; {} doctors available",
                workspace_profile,
                index.files.len(),
                index.deploy_targets.len(),
                index.profiles.len(),
                index.secret_sets.len(),
                capabilities.doctor_kinds.len()
            );

            let use_allowlist = strict && (baseline_additive || baseline_allowlist.is_some());
            let allowlist_path_buf = if use_allowlist {
                baseline_allowlist
                    .clone()
                    .unwrap_or_else(|| repo.join(".leio-code").join("baseline-allowlist.txt"))
            } else {
                PathBuf::new()
            };

            let mut envelope = QueryEnvelope {
                schema_version: leio_code::model::SCHEMA_VERSION.to_string(),
                query_id: format!(
                    "status-{}",
                    time::OffsetDateTime::now_utc().unix_timestamp_nanos()
                ),
                kind: "status".to_string(),
                summary: status_summary.clone(),
                confidence: 0.98,
                entities: vec![serde_json::json!({
                    "workspace_profile": workspace_profile,
                    "file_count": index.files.len(),
                    "deploy_targets": index.deploy_targets.len(),
                    "profiles": index.profiles.len(),
                    "secret_sets": index.secret_sets.len(),
                    "cartridges": workspace_facets.cartridges,
                    "find_kinds": capabilities.find_kinds,
                    "explain_kinds": capabilities.explain_kinds,
                    "doctor_kinds": capabilities.doctor_kinds,
                    "index_path": index_path.display().to_string(),
                    "workspace_members": members,
                    "session": session,
                    "index_truncated": truncated,
                    "strict": strict,
                    "baseline_additive": baseline_additive,
                })],
                evidence: Vec::new(),
                warnings: Vec::new(),
                meta: Some(serde_json::json!({
                    "workspace_profile": workspace_profile,
                    "workspace_facets": workspace_facets,
                    "workspace_capabilities": capabilities,
                    "workspace_members": members,
                    "session": session,
                    "index_truncated": truncated,
                    "next_commands": [
                        "leio-code status --strict",
                        "leio-code doctor baseline",
                        "leio-code doctor ci",
                        "leio-code verify",
                        "leio-code capabilities"
                    ],
                    "strict": strict,
                    "baseline_additive": baseline_additive,
                })),
                timing_ms: started.elapsed().as_millis(),
            };
            if truncated {
                envelope.warnings.push(
                    "index truncated at LEIO_MAX_INDEX_FILES; pin --repo to a package or raise the cap"
                        .to_string(),
                );
            }
            if members.len() >= 2 && index.files.len() > 2_000 {
                envelope.warnings.push(
                    "monorepo: pin --repo to a package per agent and set LEIO_SESSION so nav cursors do not collide"
                        .to_string(),
                );
            }

            let mut fail_on_warnings = false;
            let mut blocking_count = 0usize;
            let mut baseline_for_json: Option<QueryEnvelope> = None;

            if strict {
                let baseline = run_baseline_doctors(&index, &repo);
                let allowed = if use_allowlist {
                    baseline_allowlist::load_allowlist(&allowlist_path_buf).with_context(|| {
                        format!(
                            "failed to load baseline allowlist {}",
                            allowlist_path_buf.display()
                        )
                    })?
                } else {
                    Vec::new()
                };
                let blocking =
                    baseline_allowlist::filter_blocking_warnings(&baseline.warnings, &allowed);
                blocking_count = blocking.len();
                fail_on_warnings = !blocking.is_empty();

                envelope.summary = format!("{} | {}", status_summary, baseline.summary);
                envelope.warnings = baseline.warnings.clone();
                envelope.evidence = baseline.evidence.clone();
                envelope.timing_ms = started.elapsed().as_millis();
                let warning_count = envelope.warnings.len();
                let allowlisted_count = warning_count.saturating_sub(blocking_count);
                if let Some(serde_json::Value::Object(ref mut map)) = envelope.meta {
                    map.insert(
                        "baseline".to_string(),
                        serde_json::json!({
                            "summary": baseline.summary,
                            "meta": baseline.meta,
                            "query_id": baseline.query_id,
                            "entities": baseline.entities,
                            "warning_count": warning_count,
                            "timing_ms": baseline.timing_ms,
                        }),
                    );
                    if use_allowlist {
                        map.insert(
                            "baseline_allowlist".to_string(),
                            serde_json::json!({
                                "path": allowlist_path_buf.display().to_string(),
                                "patterns_loaded": allowed.len(),
                                "blocking_warning_count": blocking_count,
                                "allowlisted_warning_count": allowlisted_count,
                            }),
                        );
                    }
                }
                baseline_for_json = Some(baseline);
            } else {
                envelope.timing_ms = started.elapsed().as_millis();
            }

            if cli.json {
                let index_age_secs = index_file_age_secs(&index_path);
                let (baseline_ms, doctor_timings_ms) = baseline_for_json
                    .as_ref()
                    .map(|b| {
                        let timings = b
                            .meta
                            .as_ref()
                            .and_then(|m| m.get("doctor_timings_ms").cloned());
                        (Some(b.timing_ms), timings)
                    })
                    .unwrap_or((None, None));

                let ci = serde_json::json!({
                    "ok": !fail_on_warnings,
                    "warning_count": envelope.warnings.len(),
                    "blocking_warning_count": if strict { blocking_count } else { 0 },
                    "profile": workspace_profile,
                    "index_age_secs": index_age_secs,
                    "index_indexed_at": index.indexed_at,
                    "strict": strict,
                    "baseline_allowlist_enabled": use_allowlist,
                    "baseline_allowlist_path": use_allowlist.then(|| allowlist_path_buf.display().to_string()),
                    "timing_ms": {
                        "total": envelope.timing_ms,
                        "baseline": baseline_ms,
                    },
                    "doctor_timings_ms": doctor_timings_ms,
                });
                let wrapper = serde_json::json!({
                    "ci": ci,
                    "envelope": serde_json::to_value(&envelope)
                        .context("serialize status envelope")?,
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&wrapper).expect("status wrapper json")
                );
                if fail_on_warnings {
                    anyhow::bail!(
                        "status --strict: {} blocking baseline warning(s); see JSON .ci and envelope.warnings",
                        blocking_count
                    );
                }
            } else {
                print_and_maybe_fail(&envelope, false, fail_on_warnings)?;
            }
        }
        Command::Update { no_plugins, force } => {
            let envelope = leio_code::update::run_update(force, !no_plugins)
                .context("leio-code update failed")?;
            print_and_maybe_fail(&envelope, cli.json, false)?;
        }
        Command::Verify => {
            let started = std::time::Instant::now();
            let index = build_or_update_index(&repo, &index_path, false)?;
            let profile = repo_profile(&repo);
            let doctor_envelope = run_all_doctors(&index, &repo);
            let warning_count = doctor_envelope.warnings.len();
            let envelope = QueryEnvelope {
                schema_version: leio_code::model::SCHEMA_VERSION.to_string(),
                query_id: format!(
                    "verify-{}",
                    time::OffsetDateTime::now_utc().unix_timestamp_nanos()
                ),
                kind: "verify".to_string(),
                summary: if warning_count == 0 {
                    format!("verification passed for workspace profile `{profile}`")
                } else {
                    format!(
                        "verification failed for workspace profile `{profile}` with {warning_count} warning(s)"
                    )
                },
                confidence: if warning_count == 0 { 0.98 } else { 0.68 },
                entities: vec![serde_json::json!({
                    "check": "doctor_all",
                    "summary": doctor_envelope.summary.clone(),
                    "workspace_profile": profile.clone(),
                    "warning_count": warning_count,
                    "doctor_count": doctor_envelope
                        .meta
                        .as_ref()
                        .and_then(|meta| meta.get("doctor_count"))
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0),
                })],
                evidence: doctor_envelope.evidence,
                warnings: doctor_envelope.warnings,
                meta: Some(serde_json::json!({
                    "workspace_profile": profile,
                    "doctor_envelope": {
                        "query_id": doctor_envelope.query_id.clone(),
                        "summary": doctor_envelope.summary.clone(),
                        "meta": doctor_envelope.meta.clone(),
                    },
                    "index_path": index_path.display().to_string(),
                    "file_count": index.files.len(),
                })),
                timing_ms: started.elapsed().as_millis(),
            };
            print_and_maybe_fail(&envelope, cli.json, true)?;
        }
        Command::Audit {
            format,
            strict,
            out,
        } => {
            let audit_format: AuditFormat = format.into();
            let (body, report) = render_audit(&repo, &index_path, audit_format)?;
            if let Some(path) = out.as_ref() {
                std::fs::write(path, &body).with_context(|| {
                    format!("failed to write audit report to {}", path.display())
                })?;
                eprintln!("audit: wrote {} ({} bytes)", path.display(), body.len());
            } else {
                println!("{}", body);
            }
            if strict && !report.summary.passed {
                anyhow::bail!(
                    "audit --strict: {} warning(s) across {} doctor(s); profile `{}`",
                    report.summary.warning_count,
                    report.summary.failing_doctor_count,
                    report.summary.workspace_profile
                );
            }
        }
        Command::Capabilities { catalog } => {
            if catalog {
                let envelope = QueryEnvelope {
                    schema_version: leio_code::model::SCHEMA_VERSION.to_string(),
                    query_id: format!(
                        "capabilities-catalog-{}",
                        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
                    ),
                    kind: "capabilities".to_string(),
                    summary: "static kind catalog (CLI enums + doctor registry)".to_string(),
                    confidence: 1.0,
                    entities: vec![static_catalog()],
                    evidence: Vec::new(),
                    warnings: Vec::new(),
                    meta: None,
                    timing_ms: 0,
                };
                print_and_maybe_fail(&envelope, cli.json, false)?;
                return Ok(());
            }
            let capabilities = if let Some(summary) = load_fresh_index_summary(&repo, &index_path) {
                workspace_capabilities_from_facets(&summary.workspace_facets, &repo)
            } else {
                let index = load_or_build_index(&repo, &index_path).with_context(|| {
                    format!(
                        "failed to build or update index at {}",
                        index_path.display()
                    )
                })?;
                if load_fresh_index_summary(&repo, &index_path).is_none() {
                    let summary_path = leio_code::indexer::default_index_summary_path(&repo);
                    let _ = leio_code::indexer::save_index_summary(
                        &summary_path,
                        &leio_code::indexer::stamp_index_file(
                            leio_code::indexer::build_index_summary(&index),
                            &index_path,
                        ),
                    );
                }
                workspace_capabilities(&index, &repo)
            };
            let workspace_profile = capabilities.workspace_profile.clone();
            let doctor_summary = if capabilities.doctor_kinds.is_empty() {
                "no profile-specific doctor suites".to_string()
            } else {
                format!("{} doctor suites", capabilities.doctor_kinds.len())
            };
            let envelope = QueryEnvelope {
                schema_version: leio_code::model::SCHEMA_VERSION.to_string(),
                query_id: format!(
                    "capabilities-{}",
                    time::OffsetDateTime::now_utc().unix_timestamp_nanos()
                ),
                kind: "capabilities".to_string(),
                summary: format!(
                    "workspace profile `{}` supports {} find kinds, {} explain kinds, {doctor_summary}",
                    workspace_profile,
                    capabilities.find_kinds.len(),
                    capabilities.explain_kinds.len(),
                ),
                confidence: 0.98,
                entities: vec![serde_json::json!(capabilities.clone())],
                evidence: Vec::new(),
                warnings: Vec::new(),
                meta: Some(serde_json::json!({
                    "workspace_profile": workspace_profile,
                    "workspace_capabilities": capabilities,
                })),
                timing_ms: 0,
            };
            print_and_maybe_fail(&envelope, cli.json, false)?;
        }
        Command::Context { task, limit, full } => {
            let index = load_or_build_index(&repo, &index_path).with_context(|| {
                format!(
                    "failed to build or update index at {}",
                    index_path.display()
                )
            })?;
            let envelope = build_context_bundle(&index, &repo, &task, limit, full);
            print_and_maybe_fail(&envelope, cli.json, false)?;
        }
        Command::Find {
            first,
            second,
            method,
            format,
            r#where,
        } => {
            let (kind, needle) = match (clap::ValueEnum::from_str(&first, true), second) {
                (Ok(k), s) => (k, s),
                (Err(_), None) => (FindKind::Symbol, Some(first)),
                (Err(err), Some(_)) => {
                    anyhow::bail!("{err}");
                }
            };
            // Per-kind needle requirements. `binary` / `route` accept no
            // needle (return the full registry); all other kinds need one.
            let needle_required = !matches!(kind, FindKind::Binary | FindKind::Route);
            if needle_required && needle.is_none() {
                anyhow::bail!("find {:?} requires a needle (symbol/name/path)", kind);
            }
            if method.is_some() && !matches!(kind, FindKind::Route) {
                anyhow::bail!("--method is only valid with `find route`");
            }
            let needle_owned = needle.unwrap_or_default();
            let index = load_or_build_index(&repo, &index_path).with_context(|| {
                format!(
                    "failed to build or update index at {}",
                    index_path.display()
                )
            })?;
            let envelope = match kind {
                FindKind::Symbol => find_symbols(&index, &needle_owned),
                FindKind::EnvVar => find_env_vars(&index, &needle_owned),
                FindKind::RedisKey => find_redis_keys(&index, &needle_owned),
                FindKind::DeployTarget => find_deploy_targets(&index, &needle_owned),
                FindKind::Cartridge => find_cartridges(&index, &needle_owned),
                FindKind::ApiRoute => find_api_routes(&index, &needle_owned),
                FindKind::DockerService => find_docker_services(&index, &needle_owned),
                FindKind::SubprocessCaller => find_subprocess_callers(&index, &needle_owned),
                FindKind::Binary => find_binaries(&index, &needle_owned),
                FindKind::Route => find_routes(&index, &needle_owned, method.as_deref()),
                // Name-based dispatch: leading `/` → route caller lookup,
                // anything else → subprocess caller lookup. Distinct
                // query_id prefixes drive the JSON-LD @type mapping.
                FindKind::Callers => {
                    if needle_owned.starts_with('/') {
                        find_route_callers(&index, &needle_owned)
                    } else {
                        find_binary_callers(&index, &needle_owned)
                    }
                }
            };
            emit_envelope(&envelope, cli.json, format, r#where.as_deref())?;
        }
        Command::Explain {
            kind,
            needle,
            show_secrets,
            i_know_what_i_am_doing,
            format,
            r#where,
            stdin,
        } => {
            use std::io::IsTerminal;
            let is_tty = std::io::stdout().is_terminal();
            leio_code::value_resolution::check_show_secrets_guard(
                show_secrets,
                i_know_what_i_am_doing,
                is_tty,
            )
            .map_err(anyhow::Error::msg)?;
            if stdin {
                // --stdin mode: read JSON-LD entities, dispatch to existing
                // explain_* per entity, render the collected envelopes.
                leio_code::explain_stdin::validate_stdin_flags(r#where.as_deref())?;
                let index = load_or_build_index(&repo, &index_path).with_context(|| {
                    format!(
                        "failed to build or update index at {}",
                        index_path.display()
                    )
                })?;
                let entities = leio_code::explain_stdin::read_entities(std::io::stdin().lock())?;
                let opts = leio_code::value_resolution::ValueResolutionOpts { show_secrets };
                let mut envelopes: Vec<QueryEnvelope> = Vec::with_capacity(entities.len());
                let mut skipped = leio_code::explain_stdin::SkipSummary::default();
                for (idx, entity) in entities.iter().enumerate() {
                    match leio_code::explain_stdin::dispatch(entity, &index, &repo, opts.clone()) {
                        leio_code::explain_stdin::DispatchOutcome::Ok(env) => envelopes.push(*env),
                        leio_code::explain_stdin::DispatchOutcome::UnknownType(ty) => {
                            eprintln!("leio-code: skipped entity {idx}: unknown @type `{ty}`");
                            skipped.record_unknown(&ty);
                        }
                        leio_code::explain_stdin::DispatchOutcome::MissingField(reason) => {
                            eprintln!("leio-code: skipped entity {idx}: {reason}");
                            skipped.record_missing();
                        }
                    }
                }
                if let Some(summary) = skipped.one_line() {
                    eprintln!("leio-code: {summary}");
                }
                emit_envelopes(&envelopes, cli.json, format)?;
            } else {
                let kind = kind
                    .context("explain requires a <kind> positional argument (or pass --stdin)")?;
                let index = load_or_build_index(&repo, &index_path).with_context(|| {
                    format!(
                        "failed to build or update index at {}",
                        index_path.display()
                    )
                })?;
                let envelope = match kind {
                    ExplainKind::DeployTarget => explain_deploy_target(
                        &index,
                        needle
                            .as_deref()
                            .context("explain deploy-target requires a target name")?,
                        &repo,
                        leio_code::value_resolution::ValueResolutionOpts { show_secrets },
                    ),
                    ExplainKind::EnvVar => explain_env_var(
                        &index,
                        needle
                            .as_deref()
                            .context("explain env-var requires an env var name")?,
                        leio_code::value_resolution::ValueResolutionOpts { show_secrets },
                    ),
                    ExplainKind::RedisKey => explain_redis_key(
                        &index,
                        needle
                            .as_deref()
                            .context("explain redis-key requires a key")?,
                    ),
                    ExplainKind::Cartridge => explain_cartridge(
                        &index,
                        needle
                            .as_deref()
                            .context("explain cartridge requires a cartridge name")?,
                    ),
                    ExplainKind::Binary => explain_binary(
                        &index,
                        needle
                            .as_deref()
                            .context("explain binary requires a binary name")?,
                    ),
                    ExplainKind::Route => explain_route(
                        &index,
                        needle
                            .as_deref()
                            .context("explain route requires a route path")?,
                    ),
                };
                emit_envelope(&envelope, cli.json, format, r#where.as_deref())?;
            }
        }
        Command::Knowledge {
            kind,
            needle,
            limit,
        } => {
            let envelope = match kind {
                KnowledgeKind::Adaptive => leio_code::knowledge::search_knowledge_adaptive(
                    &repo,
                    needle
                        .as_deref()
                        .context("knowledge adaptive requires a needle")?,
                    limit,
                ),
                KnowledgeKind::Text => leio_code::knowledge::search_knowledge_text(
                    &repo,
                    needle
                        .as_deref()
                        .context("knowledge text requires a needle")?,
                    limit,
                ),
                KnowledgeKind::Status => leio_code::knowledge::knowledge_status(&repo),
                KnowledgeKind::Compile => leio_code::knowledge::compile_knowledge(&repo)?,
                KnowledgeKind::Explain => leio_code::knowledge_explain::explain_knowledge(
                    &repo,
                    needle
                        .as_deref()
                        .context("knowledge explain requires a needle")?,
                    limit,
                )?,
                KnowledgeKind::Sparql => leio_code::knowledge_explain::sparql_knowledge(
                    &repo,
                    needle
                        .as_deref()
                        .context("knowledge sparql requires a query")?,
                    limit,
                )?,
                KnowledgeKind::Exec => {
                    leio_code::knowledge_explain::exec_sparql(&repo, limit)?;
                    return Ok(());
                }
            };
            print_and_maybe_fail(&envelope, cli.json, false)?;
        }
        Command::Export {
            kind,
            output_dir,
            object_kind,
            format,
            out,
        } => {
            // Validate the new flags. They're only meaningful for
            // `formal-context`. Reject loud rather than ignoring silently so
            // misconfigured scripts surface immediately.
            if !matches!(kind, ExportKind::FormalContext) {
                if object_kind != FormalContextObjectKindArg::File {
                    anyhow::bail!("--object-kind is only valid with `export formal-context`");
                }
                if format != FormalContextFormatArg::Bundle {
                    anyhow::bail!("--format is only valid with `export formal-context`");
                }
                if out.is_some() {
                    anyhow::bail!("--out is only valid with `export formal-context`");
                }
            }
            let index = load_or_build_index(&repo, &index_path).with_context(|| {
                format!(
                    "failed to build or update index at {}",
                    index_path.display()
                )
            })?;
            let envelope = match kind {
                ExportKind::FormalContext => match format {
                    FormalContextFormatArg::Bundle => {
                        // Legacy contract: backwards-compatible. The new
                        // flags are inert unless one was actually set; if
                        // `--object-kind` is anything other than `file`
                        // when bundle is requested, that's a user error
                        // (the bundle path doesn't project by object_kind).
                        if object_kind != FormalContextObjectKindArg::File {
                            anyhow::bail!(
                                "--object-kind is only valid with --format=json or --format=arrow"
                            );
                        }
                        if out.is_some() {
                            anyhow::bail!(
                                "--out is only valid with --format=json or --format=arrow; \
                                 use --output-dir for the bundle layout"
                            );
                        }
                        let output_dir =
                            output_dir.unwrap_or_else(|| default_formal_context_output_dir(&repo));
                        export_formal_context(&index, &repo, &output_dir)?
                    }
                    FormalContextFormatArg::Json | FormalContextFormatArg::Arrow => {
                        if output_dir.is_some() {
                            anyhow::bail!(
                                "--output-dir is only valid with --format=bundle; \
                                 use --out for the streamed formats"
                            );
                        }
                        export_formal_context_stream(
                            &index,
                            object_kind.to_export(),
                            format.to_export(),
                            out.as_deref(),
                        )?
                    }
                },
                ExportKind::CodeGraph => {
                    let output_dir =
                        output_dir.unwrap_or_else(|| default_code_graph_output_dir(&repo));
                    export_code_graph(&index, &repo, &output_dir)?
                }
                ExportKind::ArrowNodes => {
                    let output_dir =
                        output_dir.unwrap_or_else(|| default_arrow_nodes_output_dir(&repo));
                    export_arrow_nodes(&index, &repo, &output_dir)?
                }
                ExportKind::Hypergraph => {
                    let output_dir =
                        output_dir.unwrap_or_else(|| default_hypergraph_output_dir(&repo));
                    export_hypergraph(&index, &repo, &output_dir)?
                }
            };
            // For `--format=json` to stdout, the stream itself already went
            // to stdout; `print_and_maybe_fail` would emit the envelope on
            // top of the payload. Suppress envelope printing in that case so
            // the only thing on stdout is parseable JSON.
            let suppress_envelope = matches!(kind, ExportKind::FormalContext)
                && format == FormalContextFormatArg::Json
                && out.is_none()
                && !cli.json;
            if !suppress_envelope {
                print_and_maybe_fail(&envelope, cli.json, false)?;
            }
        }
        Command::Doctor {
            kind,
            format,
            explain,
            suggest,
        } => {
            // Short-circuit `--suggest <rule-id>` before building the index:
            // the output is static text / template diff and needs no index.
            if let Some(rule_id) = suggest.as_deref() {
                match leio_code::diagnostics::rule_doc(rule_id) {
                    None => {
                        eprintln!(
                            "Error: no documented rule \"{rule_id}\". \
                             Run `leio-code doctor --explain list` for the full list."
                        );
                        std::process::exit(1);
                    }
                    Some(rule) => {
                        print_suggest_rule(rule);
                        return Ok(());
                    }
                }
            }
            // Short-circuit `--explain list` before building the index: it's
            // pure static text and we want it to work against any directory.
            if let Some(rule_id) = explain.as_deref() {
                if rule_id == "list" {
                    print_explain_list();
                    return Ok(());
                }
                // Reject unknown rule ids before doing any index work, so the
                // command stays fast and the exit code is stable (2).
                if leio_code::diagnostics::rule_doc(rule_id).is_none() {
                    eprintln!(
                        "Error: no documented rule \"{rule_id}\". \
                         Run `leio-code doctor --explain list` for the full list."
                    );
                    std::process::exit(2);
                }
            }
            let index = load_or_build_index(&repo, &index_path).with_context(|| {
                format!(
                    "failed to build or update index at {}",
                    index_path.display()
                )
            })?;
            // When `--explain <rule>` is set with `kind == All`, run only the
            // owning doctor — running ~60 doctors to filter for one rule's
            // evidence is wasteful and is the common shape ("which doctor
            // owns this id?" is exactly what the rule registry encodes).
            let explain_rule = explain
                .as_deref()
                .and_then(leio_code::diagnostics::rule_doc);
            let envelope = match (kind, explain_rule) {
                (DoctorKind::All, Some(rule)) => run_doctor(rule.doctor_name, &index, &repo)
                    .with_context(|| format!("doctor `{}` not registered", rule.doctor_name))?,
                (DoctorKind::All, None) => run_all_doctors(&index, &repo),
                (DoctorKind::Baseline, _) => run_baseline_doctors(&index, &repo),
                (DoctorKind::Ci, _) => run_ci_doctors(&index, &repo),
                _ => {
                    let doctor_name = match kind {
                        DoctorKind::All => unreachable!("handled above"),
                        DoctorKind::Baseline | DoctorKind::Ci => {
                            unreachable!("handled above")
                        }
                        DoctorKind::Deploy => "deploy",
                        DoctorKind::DeployBundleCriticalKeys => "deploy-bundle-critical-keys",
                        DoctorKind::DuckdbContract => "duckdb-contract",
                        DoctorKind::SemanticWiring => "semantic-wiring",
                        DoctorKind::TessellationContract => "tessellation-contract",
                        DoctorKind::SessionHotState => "session-hot-state",
                        DoctorKind::SisfronOodaRuntime => "sisfron-ooda-runtime",
                        DoctorKind::SisfronSimulationDurability => "sisfron-simulation-durability",
                        DoctorKind::OnboardingProjection => "onboarding-projection",
                        DoctorKind::OnboardingDrift => "onboarding-drift",
                        DoctorKind::OrphanFiles => "orphan-files",
                        DoctorKind::PactoWebhookAllowlistPopulated => {
                            "pacto-webhook-allowlist-populated"
                        }
                        DoctorKind::PublishableCrate => "publishable-crate",
                        DoctorKind::VendoredCrateProvenance => "vendored-crate-provenance",
                        DoctorKind::RevopsTenantGate => "revops-tenant-gate",
                        DoctorKind::RouteProjection => "route-projection",
                        DoctorKind::ScriptPathExistence => "script-path-existence",
                        DoctorKind::SecretSetParity => "secret-set-parity",
                        DoctorKind::AuthBrokering => "auth-brokering",
                        DoctorKind::CompositionResolver => "composition-resolver",
                        DoctorKind::EventDurability => "event-durability",
                        DoctorKind::EventEnvelope => "event-envelope",
                        DoctorKind::FlightAuth => "flight-auth",
                        DoctorKind::FlightRuntimeAuth => "flight-runtime-auth",
                        DoctorKind::GatewayOarBoundary => "gateway-oar-boundary",
                        DoctorKind::GatewayOcrPipeline => "gateway-ocr-pipeline",
                        DoctorKind::OcrModelsOnDisk => "ocr-models-on-disk",
                        DoctorKind::OcrCanonicalLayout => "ocr-canonical-layout",
                        DoctorKind::GlinerSharedSurface => "gliner-shared-surface",
                        DoctorKind::HealthAuditAnsXsd => "health-audit-ans-xsd",
                        DoctorKind::HealthAuditAuth => "health-audit-auth",
                        DoctorKind::HealthAuditEmbedding => "health-audit-embedding",
                        DoctorKind::HealthAuditSentinel => "health-audit-sentinel",
                        DoctorKind::LuminaiHealthAuditIsolation => "luminai-health-audit-isolation",
                        DoctorKind::HealthAuditRouterSize => "health-audit-router-size",
                        DoctorKind::HealthAuditContractAirgap => "health-audit-contract-airgap",
                        DoctorKind::HealthAuditWorkerRuntime => "health-audit-worker-runtime",
                        DoctorKind::GlosaContract => "glosa-contract",
                        DoctorKind::ClinicalContract => "clinical-contract",
                        DoctorKind::GenerateAsyncFlight => "generate-async-flight",
                        DoctorKind::FlightServerZeroCopy => "flight-server-zero-copy",
                        DoctorKind::FlightServerContextIsolation => {
                            "flight-server-context-isolation"
                        }
                        DoctorKind::HealthAuditEvidenceClass => "health-audit-evidence-class",
                        DoctorKind::HealthAuditAuditTrailDurability => {
                            "health-audit-audit-trail-durability"
                        }
                        DoctorKind::HealthAuditEngineHealth => "health-audit-engine-health",
                        DoctorKind::FlightChannelReuse => "flight-channel-reuse",
                        DoctorKind::GrpcMessageSize => "grpc-message-size",
                        DoctorKind::AuthJwtCompat => "auth-jwt-compat",
                        DoctorKind::AuthSessionContinuity => "auth-session-continuity",
                        DoctorKind::HealthAuditRedisPrimacy => "health-audit-redis-primacy",
                        DoctorKind::InferenceContracts => "inference-contracts",
                        DoctorKind::LlmProviderEgress => "llm-provider-egress",
                        DoctorKind::ManusResidue => "manus-residue",
                        DoctorKind::FrontendEngineClient => "frontend-engine-client",
                        DoctorKind::FrontendReadiness => "frontend-readiness",
                        DoctorKind::ConversationIdentity => "conversation-identity",
                        DoctorKind::VigorosSwarm => "vigoros-swarm",
                        DoctorKind::CartridgeBoundary => "cartridge-boundary",
                        DoctorKind::EgressCompliance => "egress-compliance",
                        DoctorKind::RedisKeyHygiene => "redis-key-hygiene",
                        DoctorKind::ModalAppNamingCoherence => "modal-app-naming-coherence",
                        DoctorKind::AlignDockerfilePathDepCoherence => {
                            "align-dockerfile-path-dep-coherence"
                        }
                        DoctorKind::ServerDockerfileContextCoherence => {
                            "server-dockerfile-context-coherence"
                        }
                        DoctorKind::RustToolchainPinCoherence => "rust-toolchain-pin-coherence",
                        DoctorKind::RustDependencyBaseline => "rust-dependency-baseline",
                        DoctorKind::ProdSurfaceHygiene => "prod-surface-hygiene",
                        DoctorKind::OaeiDocConsistency => "oaei-doc-consistency",
                        DoctorKind::OfficeParsersNodeParity => "office-parsers-node-parity",
                        DoctorKind::OfficeParsersArrowIpc => "office-parsers-arrow-ipc",
                        DoctorKind::OfficeParsersClippyGate => "office-parsers-clippy-gate",
                        DoctorKind::OfficeParsersDocCoverage => "office-parsers-doc-coverage",
                        DoctorKind::FastWheelhouseContract => "fast-wheelhouse-contract",
                        DoctorKind::SelfContract => "self-contract",
                        DoctorKind::SkillContract => "skill-contract",
                        DoctorKind::TypescriptConfigHygiene => "typescript-config-hygiene",
                        DoctorKind::AgentSeedUpdateCompleteness => "agent-seed-update-completeness",
                        DoctorKind::StartupHookCartridgeCoverage => {
                            "startup-hook-cartridge-coverage"
                        }
                        DoctorKind::WorkerMemBudget => "worker-mem-budget",
                        DoctorKind::ModelCacheContract => "model-cache-contract",
                        DoctorKind::BareCartridgeRedisKey => "bare-cartridge-redis-key",
                        DoctorKind::EgressSuccessWithoutWamid => "egress-success-without-wamid",
                        DoctorKind::ActiveCartridgesEnvDrift => "active-cartridges-env-drift",
                        DoctorKind::AssurantSeedWiring => "assurant-seed-wiring",
                        DoctorKind::AssurantOpsProduction => "assurant-ops-production",
                        DoctorKind::OcrFlightWiring => "ocr-flight-wiring",
                        DoctorKind::ParseHybridFallback => "parse-hybrid-fallback",
                        DoctorKind::LayoutFastSpectralContract => "layout-fast-spectral-contract",
                        DoctorKind::LayoutContractPlatform => "layout-contract-platform",
                        DoctorKind::PdfPipelineDivergence => "pdf-pipeline-divergence",
                        DoctorKind::PdfStudioEnvCoherence => "pdf-studio-env-coherence",
                        DoctorKind::AudioHandlerTimeout => "audio-handler-timeout",
                        DoctorKind::ChatwootPratiqueSync => "chatwoot-pratique-sync",
                        DoctorKind::KbCollectionExists => "kb-collection-exists",
                        DoctorKind::FitnessMemberUnitResolver => "fitness-member-unit-resolver",
                        DoctorKind::LgpdOutboundFilter => "lgpd-outbound-filter",
                        DoctorKind::EvoCredentialIsolation => "evo-credential-isolation",
                        DoctorKind::C4GymEvoIntegration => "c4gym-evo-integration",
                        DoctorKind::JaipayPactoGcpWebhook => "jaipay-pacto-gcp-webhook",
                        DoctorKind::JaipaySupabaseSessionPooler => "jaipay-supabase-session-pooler",
                        DoctorKind::SupabaseRuntimeShape => "supabase-runtime-shape",
                        DoctorKind::ComposeWorkerMemBudget => "compose-worker-mem-budget",
                        DoctorKind::SupabaseProjectLiveness => "supabase-project-liveness",
                        DoctorKind::LizJaiPayPactoLookupContract => {
                            "liz-jaipay-pacto-lookup-contract"
                        }
                        DoctorKind::PlusoftRoutingContract => "plusoft-routing-contract",
                        DoctorKind::PlusoftHandoverPayloadContract => {
                            "plusoft-handover-payload-contract"
                        }
                        DoctorKind::PlusoftTranscriptFidelity => "plusoft-transcript-fidelity",
                        DoctorKind::PactoDrainDependency => "pacto-drain-dependency",
                        DoctorKind::TestPatchTargetIntegrity => "test-patch-target-integrity",
                        DoctorKind::TestRouteMountIntegrity => "test-route-mount-integrity",
                        DoctorKind::PratiqueCobrancaJsonContract => {
                            "pratique-cobranca-json-contract"
                        }
                        DoctorKind::OntologyPriceConsistency => "ontology-price-consistency",
                        DoctorKind::RecipientLimbo => "recipient-limbo",
                        DoctorKind::SaraAssurantEgressContract => "sara-assurant-egress-contract",
                        DoctorKind::WhatsAppBsuidWebhook => "whatsapp-bsuid-webhook",
                        DoctorKind::WhatsAppBsuidCrm => "whatsapp-bsuid-crm",
                        DoctorKind::WhatsAppDisplayNameOnly => "whatsapp-display-name-only",
                        DoctorKind::RevopsSnapshotSchemaSync => "revops-snapshot-schema-sync",
                        DoctorKind::TenantIdentity => "tenant-identity",
                        DoctorKind::TenantOverrideContract => "tenant-override-contract",
                        DoctorKind::OperatorTenantParity => "operator-tenant-parity",
                        DoctorKind::Slop => "slop",
                        DoctorKind::EnvContract => "env-contract",
                        DoctorKind::ImportBoundary => "import-boundary",
                        DoctorKind::RepoHygiene => "repo-hygiene",
                        DoctorKind::CodexOrchestration => "codex-orchestration",
                        DoctorKind::LeioReleaseCoherence => "leio-release-coherence",
                        DoctorKind::InducedInvariants => "induced-invariants",
                        DoctorKind::PlatformRuntimeTrustBoundary => {
                            "platform-runtime-trust-boundary"
                        }
                        DoctorKind::ArtifactReuse => "artifact-reuse",
                        DoctorKind::PyRustBoundary => "py-rust-boundary",
                    };
                    run_doctor(doctor_name, &index, &repo)
                        .with_context(|| format!("doctor `{doctor_name}` not registered"))?
                }
            };
            if let Some(rule) = explain_rule {
                // --explain always emits the static block + filtered
                // violations. --format is intentionally ignored — the output
                // is shaped for pasting into a PR comment.
                print_explain_rule(rule, &envelope);
            } else {
                let diag_format: DiagFormat = format.into();
                // Aggregate suite runs (`all` / `baseline` / `ci`) honor the
                // workspace baseline allowlist (`.leio-code/baseline-allowlist.txt`)
                // — the same ledger the `status --strict` path consults.
                // Allowlisted warnings are still printed but do not fail the
                // gate, so environmental doctors that physically cannot pass in
                // a CI runner (e.g. a reachability probe with no provisioned
                // sidecar) stop blocking `make leio-code-doctor-all`,
                // while any new, un-ledgered warning still fails it. This
                // matches the advisory policy already adopted in the
                // leio-code-audit workflows. Individual `doctor <name>` runs
                // stay strict — if you ask for one doctor you want its verdict.
                let honor_allowlist = matches!(
                    kind,
                    DoctorKind::All | DoctorKind::Baseline | DoctorKind::Ci
                );
                let blocking_warnings: Vec<String> = if honor_allowlist {
                    let allowlist_path = repo.join(".leio-code").join("baseline-allowlist.txt");
                    let allowed =
                        baseline_allowlist::load_allowlist(&allowlist_path).unwrap_or_default();
                    baseline_allowlist::filter_blocking_warnings(&envelope.warnings, &allowed)
                } else {
                    envelope.warnings.clone()
                };
                let allowlisted = envelope
                    .warnings
                    .len()
                    .saturating_sub(blocking_warnings.len());
                match diag_format {
                    DiagFormat::Text => {
                        // Print the full envelope (every warning, including
                        // allowlisted ones, for transparency); fail only on
                        // blocking warnings.
                        print_and_maybe_fail(&envelope, cli.json, false)?;
                        if !blocking_warnings.is_empty() {
                            if allowlisted > 0 {
                                anyhow::bail!(
                                    "doctor reported {} blocking warning(s) ({} allowlisted); see output above",
                                    blocking_warnings.len(),
                                    allowlisted
                                );
                            }
                            anyhow::bail!(
                                "doctor reported {} warning(s); see output above",
                                blocking_warnings.len()
                            );
                        }
                    }
                    DiagFormat::Json | DiagFormat::Sarif => {
                        let run_meta = build_run_meta(&repo);
                        println!("{}", diagnostics::render(&envelope, diag_format, &run_meta));
                        if !blocking_warnings.is_empty() {
                            let fmt = match diag_format {
                                DiagFormat::Json => "json",
                                DiagFormat::Sarif => "sarif",
                                DiagFormat::Text => unreachable!(),
                            };
                            if allowlisted > 0 {
                                anyhow::bail!(
                                    "doctor reported {} blocking warning(s) ({} allowlisted); see {} output above",
                                    blocking_warnings.len(),
                                    allowlisted,
                                    fmt
                                );
                            }
                            anyhow::bail!(
                                "doctor reported {} warning(s); see {} output above",
                                blocking_warnings.len(),
                                fmt
                            );
                        }
                    }
                }
            }
        }
        Command::Graph {
            kind,
            needle,
            threshold,
        } => {
            let index = load_or_build_index(&repo, &index_path).with_context(|| {
                format!(
                    "failed to build or update index at {}",
                    index_path.display()
                )
            })?;
            let envelope = match kind {
                GraphKind::DeadCode => query_dead_code(&index, &repo, threshold)?,
                _ => {
                    let needle = needle.ok_or_else(|| {
                        anyhow::anyhow!(
                            "graph `{:?}` requires a needle (symbol or file path)",
                            kind
                        )
                    })?;
                    match kind {
                        GraphKind::CallersOf => {
                            query_call_graph(&index, &repo, GraphDirection::CallersOf, &needle)?
                        }
                        GraphKind::CalleesOf => {
                            query_call_graph(&index, &repo, GraphDirection::CalleesOf, &needle)?
                        }
                        GraphKind::CallsitesOf => query_callsites_of(&index, &repo, &needle)?,
                        GraphKind::SymbolsIn => query_symbols_in(&index, &repo, &needle)?,
                        GraphKind::ImportsIn => query_imports_in(&index, &repo, &needle)?,
                        GraphKind::ImportersOf => query_importers_of(&index, &repo, &needle)?,
                        GraphKind::ResolvedImportsIn => {
                            query_resolved_imports_in(&index, &repo, &needle)?
                        }
                        GraphKind::ResolvedImportersOf => {
                            query_resolved_importers_of(&index, &repo, &needle)?
                        }
                        GraphKind::DeadCode => unreachable!("handled above"),
                    }
                }
            };
            print_and_maybe_fail(&envelope, cli.json, false)?;
        }
        Command::Nav {
            kind,
            needle,
            index,
            limit,
            offset,
        } => {
            let action = NavAction::from(kind);
            let envelope = if action == NavAction::Goto
                && needle
                    .as_deref()
                    .is_some_and(leio_code::nav::looks_like_iri)
            {
                if offset != 0 {
                    anyhow::bail!("nav goto an IRI does not support --offset");
                }
                leio_code::nav::pin_iri(&repo, needle.as_deref().unwrap())?
            } else {
                let index_doc = load_or_build_index(&repo, &index_path).with_context(|| {
                    format!(
                        "failed to build or update index at {}",
                        index_path.display()
                    )
                })?;
                run_nav_page(
                    &index_doc,
                    &repo,
                    action,
                    needle.as_deref(),
                    index,
                    limit,
                    offset,
                )?
            };
            print_and_maybe_fail(&envelope, cli.json, false)?;
        }
        Command::Watch { debounce_ms, quiet } => {
            leio_code::watcher::run_watch(
                &repo,
                &index_path,
                leio_code::watcher::WatchOptions { debounce_ms, quiet },
            )?;
        }
        Command::Kb { action } => match action {
            KbCommand::Add { name, source } => {
                let sources = leio_code::kb::add_source(&name, &source)?;
                print_json(&json!({
                    "ok": true,
                    "kb": name,
                    "sources": sources.iter().map(|s| s.path.clone()).collect::<Vec<_>>(),
                }))?;
            }
            KbCommand::Remove { name, source } => {
                let removed = leio_code::kb::remove_source(&name, &source)?;
                print_json(&json!({ "ok": true, "removed": removed }))?;
            }
            KbCommand::List => {
                let registry = leio_code::kb::load_registry();
                print_json(&json!({
                    "bases": registry.bases.iter().map(|(name, sources)| {
                        json!({
                            "name": name,
                            "sources": sources.iter().map(|s| s.path.clone()).collect::<Vec<_>>(),
                        })
                    }).collect::<Vec<_>>(),
                }))?;
            }
            KbCommand::Build { name, embed_repo } => {
                let root = embed_repo
                    .or_else(|| {
                        leio_code::kb::load_registry()
                            .bases
                            .get(&name)
                            .and_then(|sources| {
                                sources.first().map(|s| PathBuf::from(s.path.clone()))
                            })
                    })
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                let stats = leio_code::kb::build_kb(&name, &root)?;
                print_json(&serde_json::json!({
                    "sources": stats.sources,
                    "rows": stats.rows,
                    "embedded": stats.embedded,
                    "lexical_only": stats.lexical_only,
                    "embedding_model": stats.embedding_model,
                    "built_at_ms": stats.built_at_ms,
                }))?;
            }
            KbCommand::Query {
                name,
                query,
                top_k,
                source,
            } => {
                let hits = leio_code::kb::query_kb(
                    &name,
                    &query,
                    top_k,
                    source.as_deref(),
                    &std::env::current_dir().unwrap_or_default(),
                )?;
                print_json(&json!({ "kb": name, "hits": hits }))?;
            }
            KbCommand::Drop { name } => {
                let arrow = leio_code::kb::collection_path(&name);
                let meta = leio_code::kb::meta_path(&name);
                let removed =
                    std::fs::remove_file(&arrow).is_ok() || std::fs::remove_file(&meta).is_ok();
                let mut registry = leio_code::kb::load_registry();
                registry.bases.remove(&name);
                leio_code::kb::save_registry(&registry)?;
                print_json(&json!({ "ok": true, "removed": removed }))?;
            }
        },
    }

    Ok(())
}

fn index_file_age_secs(index_path: &Path) -> Option<f64> {
    let meta = std::fs::metadata(index_path).ok()?;
    let modified = meta.modified().ok()?;
    std::time::SystemTime::now()
        .duration_since(modified)
        .ok()
        .map(|duration| duration.as_secs_f64())
}

fn canonical_repo(path: &Path) -> Result<PathBuf> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("failed to resolve repo path {}", path.display()))?;

    // When running from a subdirectory with default or current directory path,
    // walk up ancestor directories to find the enclosing repository root
    // containing .git or .leio-code (mirrors git and cargo workspace discovery).
    if path == Path::new(".")
        && !canonical.join(".git").exists()
        && !canonical.join(".leio-code").exists()
    {
        for ancestor in canonical.ancestors().skip(1) {
            if ancestor.join(".git").exists() || ancestor.join(".leio-code").exists() {
                return Ok(ancestor.to_path_buf());
            }
        }
    }

    Ok(canonical)
}

/// Build the reproducibility stamp for machine-readable diagnostics. Falls back
/// to `"unknown"` if `git rev-parse HEAD` fails (e.g. repo isn't a git repo or
/// `git` isn't on PATH) — never an error.
fn build_run_meta(repo: &Path) -> RunMeta {
    let commit_sha = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    RunMeta {
        index_version: leio_code::indexer::index_version(),
        commit_sha,
        tool_version: env!("CARGO_PKG_VERSION"),
    }
}

fn print_envelope(envelope: &QueryEnvelope, json_mode: bool) {
    leio_code::jsonld::record_bound_event(envelope);
    if json_mode {
        // Machine mode is compact: consumers re-parse (MCP, jq, agents), so
        // pretty-printing only inflated payload size by ~45%.
        println!(
            "{}",
            serde_json::to_string(envelope).expect("json serialization should succeed")
        );
        return;
    }

    println!("{}", envelope.summary);
    if !envelope.warnings.is_empty() {
        println!("warnings:");
        for warning in &envelope.warnings {
            println!("- {}", warning);
        }
    }
    if let Some(meta) = &envelope.meta {
        println!("meta:");
        println!(
            "{}",
            serde_json::to_string_pretty(meta).expect("json serialization should succeed")
        );
    }
    if !envelope.evidence.is_empty() {
        println!("evidence:");
        for item in &envelope.evidence {
            match item.line {
                Some(line) => println!("- {}:{} [{}] {}", item.path, line, item.kind, item.detail),
                None => println!("- {} [{}] {}", item.path, item.kind, item.detail),
            }
        }
    }
    if !envelope.entities.is_empty() {
        println!("entities:");
        for entity in &envelope.entities {
            println!("- {}", entity);
        }
    }
}

/// Dispatch for `find` / `explain`: resolves `--format` (text / json / jsonld)
/// with `--where` implying jsonld when no explicit format is given, then
/// prints the envelope or its JSON-LD rendering. The global `--json` flag is
/// preserved as a synonym for `--format=json` (otherwise the MCP wrapper
/// would break).
fn emit_envelope(
    envelope: &QueryEnvelope,
    global_json: bool,
    format: Option<OutputFormatArg>,
    where_expr: Option<&str>,
) -> Result<()> {
    let effective = match format {
        Some(explicit) => explicit,
        None if where_expr.is_some() => OutputFormatArg::Jsonld,
        None if global_json => OutputFormatArg::Json,
        None => OutputFormatArg::Text,
    };
    if where_expr.is_some() && !matches!(effective, OutputFormatArg::Jsonld) {
        anyhow::bail!(
            "--where applies to JSON-LD output only; use `--format=jsonld` or omit `--format` (jsonld is implied when `--where` is set)"
        );
    }
    match effective {
        OutputFormatArg::Text => print_envelope(envelope, false),
        OutputFormatArg::Json => print_envelope(envelope, true),
        OutputFormatArg::Jsonld => {
            leio_code::jsonld::record_bound_event(envelope);
            let mut doc = leio_code::jsonld::render_envelope_as_jsonld(envelope);
            if let Some(expr) = where_expr {
                doc = leio_code::jsonld::apply_where_filter(&doc, expr)
                    .with_context(|| format!("invalid --where expression: {expr}"))?;
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&doc).expect("json-ld serialization should succeed")
            );
        }
    }
    Ok(())
}

/// Render a collection of envelopes (produced by `explain --stdin`) to
/// stdout per `--format`.
///
/// Output shape:
/// - `text` (default): each envelope rendered through `print_envelope`,
///   separated by a divider line. Empty input prints nothing.
/// - `json`: JSON array of envelopes (pretty-printed). Empty input prints
///   `[]`.
/// - `jsonld`: JSON array of JSON-LD-rendered envelopes (pretty-printed).
///
/// `--where` is rejected upstream in `validate_stdin_flags`; this helper
/// does not accept a where_expr because per-envelope filtering is
/// out-of-scope for the stdin loop (users already filtered with jq).
fn emit_envelopes(
    envelopes: &[QueryEnvelope],
    global_json: bool,
    format: Option<OutputFormatArg>,
) -> Result<()> {
    let effective = match format {
        Some(explicit) => explicit,
        None if global_json => OutputFormatArg::Json,
        None => OutputFormatArg::Text,
    };
    match effective {
        OutputFormatArg::Text => {
            for (idx, envelope) in envelopes.iter().enumerate() {
                if idx > 0 {
                    println!("---");
                }
                print_envelope(envelope, false);
            }
        }
        OutputFormatArg::Json => {
            for envelope in envelopes {
                leio_code::jsonld::record_bound_event(envelope);
            }
            println!(
                "{}",
                serde_json::to_string_pretty(envelopes).expect("json serialization should succeed")
            );
        }
        OutputFormatArg::Jsonld => {
            for envelope in envelopes {
                leio_code::jsonld::record_bound_event(envelope);
            }
            let docs: Vec<serde_json::Value> = envelopes
                .iter()
                .map(leio_code::jsonld::render_envelope_as_jsonld)
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&docs).expect("json-ld serialization should succeed")
            );
        }
    }
    Ok(())
}

/// Print the list of every documented rule_id, one per line, with a short
/// first-sentence summary from the description. Used by
/// `doctor --explain list`.
fn print_explain_list() {
    println!(
        "Documented rules ({}):",
        leio_code::diagnostics::all_rule_docs().len()
    );
    for doc in leio_code::diagnostics::all_rule_docs() {
        // Print the rule_id and the first sentence of the description as a
        // one-line summary so the listing fits on one screen.
        let summary = doc
            .description
            .split_once(". ")
            .map(|(head, _)| head)
            .unwrap_or(doc.description);
        println!("  {:<32} {}", doc.rule_id, summary);
    }
    println!();
    println!(
        "Run `leio-code doctor all --explain <rule-id>` for the full citation and fix advice."
    );
}

/// Print the static description / citation / fix advice for a single rule,
/// followed by the violations from the supplied envelope whose
/// `EvidenceItem.kind` matches the rule_id. Output is plain text, suitable
/// for pasting into a PR comment.
fn print_explain_rule(rule: &leio_code::diagnostics::RuleDoc, envelope: &QueryEnvelope) {
    println!("=== Rule: {} ===", rule.rule_id);
    println!("{}", rule.description);
    println!();
    println!("Citation: {}", rule.citation);
    println!();
    println!("Conceptual fix:");
    println!("{}", rule.fix_advice);
    println!();
    let matches: Vec<_> = envelope
        .evidence
        .iter()
        .filter(|ev| ev.kind == rule.rule_id)
        .collect();
    println!("Violations ({}):", matches.len());
    for ev in matches {
        match ev.line {
            Some(line) => println!("  {}:{}  {}", ev.path, line, ev.detail),
            None => println!("  {}  {}", ev.path, ev.detail),
        }
    }
}

/// Emit the mechanical fix suggestion for a single rule.
///
/// - `SuggestConfidence::High` + `suggest_fn = Some(f)`: call `f` with a
///   dummy evidence item and print the resulting unified diff to stdout.
/// - `SuggestConfidence::Medium`: explain that no diff is available for this
///   rule at medium confidence (fix requires knowing runtime context).
/// - `SuggestConfidence::Low` (or High with `suggest_fn = None`): explain
///   that no mechanical fix is available.
///
/// Never writes to disk. Always exits 0 (the caller already validated the
/// rule id and exits 1 on unknown ids).
fn print_suggest_rule(rule: &leio_code::diagnostics::RuleDoc) {
    use leio_code::diagnostics::SuggestConfidence;
    use leio_code::model::EvidenceItem;

    println!(
        "=== Suggest: {} (confidence: {}) ===",
        rule.rule_id,
        rule.suggest_confidence.as_str()
    );
    println!();

    match (rule.suggest_confidence, rule.suggest_fn) {
        (SuggestConfidence::High, Some(f)) => {
            // Call the suggester with a dummy violation — phase-1 suggesters
            // return a fixed template independent of the violation fields.
            let dummy = EvidenceItem {
                kind: rule.rule_id.to_string(),
                path: String::new(),
                line: None,
                detail: String::new(),
            };
            match f(&dummy) {
                Some(diff) => {
                    println!("Template diff (apply manually — leio-code never writes to disk):");
                    println!();
                    println!("{}", diff);
                }
                None => {
                    println!(
                        "No mechanical fix available for rule `{}` (confidence: high, but \
                         suggest_fn returned None).",
                        rule.rule_id
                    );
                }
            }
        }
        (SuggestConfidence::Medium, _) => {
            println!(
                "No mechanical fix available for rule `{}` at medium confidence.",
                rule.rule_id
            );
            println!();
            println!(
                "A medium-confidence fix requires knowing runtime context (e.g. the \
                 specific key name or call-site structure) that cannot be determined \
                 from a static template. Use `--explain {}` for the conceptual fix \
                 advice, then apply it manually at the flagged call site.",
                rule.rule_id
            );
        }
        (SuggestConfidence::Low, _) | (SuggestConfidence::High, None) => {
            println!(
                "No mechanical fix available for rule `{}` (confidence: {}).",
                rule.rule_id,
                rule.suggest_confidence.as_str()
            );
            println!();
            println!(
                "Use `--explain {}` for the conceptual fix advice.",
                rule.rule_id
            );
        }
    }
}

fn print_and_maybe_fail(
    envelope: &QueryEnvelope,
    json_mode: bool,
    fail_on_warnings: bool,
) -> Result<()> {
    print_envelope(envelope, json_mode);
    if fail_on_warnings && !envelope.warnings.is_empty() {
        anyhow::bail!(
            "doctor reported {} warnings; see output above",
            envelope.warnings.len()
        );
    }
    Ok(())
}

#[cfg(test)]
mod c4gym_doctor_cli_tests {
    use super::*;

    #[test]
    fn parses_c4gym_evo_integration_doctor() {
        let cli =
            <Cli as clap::Parser>::try_parse_from(["leio-code", "doctor", "c4gym-evo-integration"])
                .expect("c4gym doctor must be selectable");
        assert!(matches!(
            cli.command,
            Command::Doctor {
                kind: DoctorKind::C4GymEvoIntegration,
                ..
            }
        ));
    }
}
