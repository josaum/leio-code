# Changelog

All notable changes to LEIO Code. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow the crate version in `Cargo.toml`.

## [Unreleased]

### Added
- **RDF 1.2 support** across the knowledge stack. Oxigraph is upgraded from
  0.4 to 0.5.10 with the `rdf-12` feature, so formal.nq ingestion, dumping,
  and SPARQL now understand RDF 1.2: `<<(...)>>` triple terms
  (`rdf:reifies` reification), directional language literals
  (`"..."@he--rtl`, `rdf:dirLangString`), and SPARQL 1.2 triple-term
  patterns. The formal knowledge graph gains statement-level provenance:
  heading-alignment and functor-coherence triples are reified with
  `rdf:reifies` and annotated `prov:wasDerivedFrom` back to the source file
  / lattice artifact, queryable directly with
  `?r rdf:reifies <<( ?s ?p ?o )>> ; prov:wasDerivedFrom ?src`
  (`FormalStats.provenance` counts the pairs). SPARQL execution migrates to
  the oxigraph 0.5 `SparqlEvaluator` API via a `SparqlQueryExt` trait, the
  deprecated `model::Subject` alias is replaced by `NamedOrBlankNode`, and
  the toolchain pin follows current Rust `stable`. Spec research briefs
  (RDF 1.2 concepts/N-Triples/N-Quads/Turtle, JSON-LD 1.2 + JSON-LD-star,
  yaml-ld/cbor-ld/streaming/bp) are reflected in the design: the JSON-LD
  emitter stays exact 1.1 (directional literals map to `rdf:dirLangString`
  only when JSON-LD 1.2 lands), and yaml-ld/cbor-ld/streaming are watched
  not adopted.

### Added
- **Pure-Arrow knowledge bases (`kb`)**: fuse any repos/docs folders (git
  not required) into one queryable collection — Milvus-style scalar+vector
  array-of-structs in plain Arrow IPC, no server. New CLI family
  `leio-code kb add|remove|list|build|query|drop` and MCP tool
  `leio_code_kb`. Build chunks with the wiki article walk, content-hashes
  chunks (unchanged chunks keep embeddings across rebuilds), embeds new
  chunks via the configured OpenAI-compatible endpoint (`LEIO_CODE_EMBED_URL`),
  and degrades to lexical-only scoring when no endpoint is reachable —
  upgrading in place when one appears. Query is hybrid: lexical coverage +
  cosine over the `embedding` list column, top-k with source filters.
- **Live OpenRouter work-shape table** for the harness. New
  `leio-harness models refresh|show` fetches OpenRouter's public `/models`
  list (no API key), filters to text-only-output models with >= 100k context
  (skipping `:free`, `preview`, and zero-priced loss leaders), and ranks
  volume (cheapest), quality (priciest), and verifier (nearest median) into
  `~/.config/leio-harness/openrouter-models.json` — valid one week. Lane
  `workShape` resolution consults the cache first and falls back to the
  built-in 2026-08-17 snapshot; refresh is operator-invoked only and never
  blocks lane execution. First live refresh resolved mistral-nemo / glm-5 /
  o1-pro for volume / verifier / quality.
- **JSON Schema service descriptor** for the MCP surface:
  `schema/leio-code-mcp-service-descriptor.json` is generated from a live
  `tools/list` (`mcp/generate-descriptor.mjs`) — one document describing all
  16 tools as methods (`type`, `params`, `returns`) plus their full MCP
  `inputSchema`/`outputSchema` contracts. `mcp/service-descriptor.test.js`
  regenerates and fails on drift; `make verify` runs it.
- JSON-LD API Best Practices conformance note on the emitter (explicit
  `@id`/`@type`, node-object references, directional-value pass-through).
- **Dual-era initialize echo** for the stdio MCP server: when a host
  requests the `2026-07-28` protocol version in the handshake, the SDK 1.30
  downgrade to `2025-11-25` made strict 2026-era hosts close the connection
  immediately after initialize (observed as repeated
  "sibling did not complete the exchange" + close-after-initialize cycles on
  a Claude Desktop Cowork pool after an app auto-update). The server now
  echoes the requested `2026-07-28` back — this server implements that
  era's surface (`server/discover`, `resultType`, per-request `_meta`) —
  while `2025-11-25` clients are unaffected. `scripts/mcp-capture.sh` adds
  a stdio MITM wire-capture wrapper for protocol forensics.

## [2.6.2] — 2026-09-08

### Added
- Local static-site delivery stages content-addressed build artifacts, serves
  them over loopback HTTP, verifies the running revision and every file, and
  supports verified rollback. Deployment authorization binds the target,
  action, artifact revision, and transition generation. A real local fixture
  exercises build A, deploy A, build B, deploy B, and rollback A; PyLD validates
  the persisted JSON-LD documents. This adapter does not claim production
  deployment coverage.
- Native Rust engineering workflows in `leio-harness` persist plans, input
  revisions, approvals, execution progress, failures, and artifact hashes as
  JSON-LD. Revision history preserves approval evidence and binds approval
  digests to the run and input revision; uncertain execution outcomes require
  reconciliation before retry.
- Knowledge references for JSON-LD 1.1, the RDF 1.2 Primer, and RDF 1.2
  Concepts, with source provenance and specification maturity recorded.
- Regression coverage for supported language extensions through discovery,
  indexing, and graph extraction, including C# top-level calls and Razor code
  blocks. C# and Razor remain input languages; workflow implementation is Rust.

### Fixed
- File discovery now admits supported source extensions, including C# and
  Razor, so available parsers receive the files during repository indexing.
- C# generic invocations retain the method as the graph callee rather than
  incorrectly selecting its type argument.
- CSS selector line counting uses one pass instead of repeatedly scanning
  source prefixes, avoiding quadratic indexing work on large stylesheets.
- Release coherence checks cover the Harness and knowledge-core crate
  manifests and lock entries, and both npm lockfile version locations.
- First-party plugin and extension metadata now agrees with the 2.6.2 Rust,
  MCP, and Apps SDK packages.

## [2.6.1] — 2026-09-01

### Fixed
- The stdio MCP server no longer dies at startup when the `leio-code` binary
  cannot be resolved (scrubbed `HOME`/`PATH`, missing Cargo install, stale
  binary without `capabilities --catalog`). The kind catalog used to load via
  `execFileSync` at module scope: any resolution or spawn failure — including
  the `cargo run` fallback with no `cargo` on `PATH` (`spawnSync cargo
  ENOENT`) — killed the process before the transport started, which clients
  observe as an instant close on their `server/discover` / `initialize`
  probe ("Version negotiation failed: the connection closed during the
  server/discover probe" in shared-pool hosts). The catalog now loads
  lazily, gated behind the first `tools/list` / `tools/call`
  (`installLazyToolAccess` in `mcp-spec-2025-11-25.js`): tools register
  pre-connect with permissive string kind schemas, and once
  `capabilities --catalog` answers (bounded by a 30 s timeout,
  `LEIO_CODE_CATALOG_TIMEOUT_MS`) the six kind-bearing tools upgrade their
  input schemas to catalog enums in place via the SDK's registered-tool
  `update()`. A failed catalog keeps the permissive schemas, sets
  `kind_catalog_warning` on tool results, and appends the failure to
  `~/.leio-code/mcp-startup-errors.log` (new startup diagnostics log;
  `uncaughtException` / `unhandledRejection` / connect failures are logged
  there too). Net effect: every probe pattern a host uses — modern
  `server/discover` pre-initialize, legacy handshake, tools/list without
  initialize, probe-then-abort — is answered in ~100–200 ms regardless of
  binary state, and degraded environments serve free-form kinds validated by
  the CLI instead of a dead server.
- `scripts/launch-stdio-mcp.sh` passes `--ignore-workspace` to pnpm: with a
  `pnpm-workspace.yaml` above the checkout (e.g. a home-level workspace),
  pnpm walked up to the workspace root, reported "Already up to date", and
  left the plugin cache's `mcp/node_modules` empty — every launch died with
  `ERR_MODULE_NOT_FOUND: @modelcontextprotocol/sdk`.
- `find api-route` no longer loses to grep. Two independent defects: the
  query path only re-parsed Python route files, so every axum/Express route
  already sitting in `cross_language.routes` was invisible — indexed
  non-Python routes now feed `find`/`collect` candidates; and the FastAPI
  indexer extractor ignored `APIRouter(prefix=...)`, dropping empty-path
  decorators (`@router.post("")` is the collection route) and storing
  unprefixed parameterized paths. Hyphenated needles (`phone-lines`) also
  never matched because tokenization split both sides — multi-token needles
  now get a substring branch. Verified against the workspace that motivated
  the report: `find api-route webhooks/chatwoot` 0 → 2 hits (axum),
  `find api-route phone-lines` 0 → 10 hits (FastAPI, prefix-joined, with
  handler and auth metadata).

### Changed
- Cross-file axum nest resolution disambiguates generic callee names
  (`api::extract::routes` vs `api::config::routes`) by module path before
  falling back to unique bare-name declarations, follows `.merge(fn())`
  chains transitively (cycle-safe BFS) so routers merged inside a mounted
  router receive the prefix, and mounts root routes to the bare prefix
  (`/api/extract` instead of `/api/extract/`). Verified on the gateway:
  the extract family indexes as `/api/extract`, `/api/extract/rdf`, ...
- Cross-file axum nests mount at index build: `.nest("/prefix",
  module::router_fn())` resolves the callee against the indexed Rust
  symbols and prefixes every route from the declaring file, composing with
  inline nests (verified: `/ops_console/api/ingest/webhooks`). Files whose
  router is only merged inside the callee keep no prefix — a documented
  approximation pending a real call graph. `explain route` now surfaces
  handler and auth metadata on route declarations.
- Inline axum `.nest("/prefix", Router::new()...)` mounts resolve to their
  full path (recursively for nested nests): the ops-console ingest family
  indexes as `/api/ingest/*` instead of bare `/webhooks`, `/stats`. When a
  Rust file scopes its builder chain with an auth `route_layer`
  (`middleware::from_fn*` + jwt/auth), routes declared before the layer
  index as `protected (route_layer)` and after it as
  `public (post route_layer)`, surfaced in `find api-route` auth metadata
  (absent hint stays `unknown`; cross-file `.nest(module::routes())`
  mounts are a documented limitation).
- Indexed routes now carry their handler where the declaration names one:
  axum `.route("/x", post(handler))` and Express `app.get("/x", handler)`
  capture the handler into the RouteRecord (serde-defaulted, so older
  indexes stay loadable), and `find api-route` surfaces it in
  `handlers`/`handler` — the review's axum results no longer come back
  with empty handler metadata.
- Kind surfaces derive from the binary. `capabilities --catalog` prints
  the full static catalog (CLI value enums + the doctor registry + the
  `all` meta-kind), and both JS surfaces (stdio MCP, Apps SDK) build
  their zod kind schemas from it at startup instead of hand-maintained
  mirror arrays — the five-copy drift class is gone. The MCP and Apps SDK
  now also advertise the kinds the CLI already supported (binary/route
  explain, subprocess-caller/binary/route find, knowledge exec). The
  self-contract doctor and the capabilities contract test now assert
  derivation (no literal kind arrays may reappear).
- Context/envelope diet. Default `context` bundles are 56–60% smaller:
  zone items became `{kind, source_field, path, line}` references instead
  of second copies of the payload, per-file `symbols`/`env_vars`/
  `redis_keys` cap at 3 with `*_count` totals, envelope `evidence` keeps
  file/instruction/anchor items only, and the duplicated
  `agent_instructions`/`memory_banks` keys plus the full
  `workspace_capabilities` block stay behind `context --full`. `--json`
  output is now compact for every command. The MCP surface stopped
  embedding the raw stdout (a full re-serialized copy of the envelope)
  and the duplicated `envelope_meta` in successful results; stderr is
  capped to a 2 KB tail.

### Added
- Binary freshness self-check. `--version` now prints the exact source
  commit the binary was built from (`2.6.0 (19d1aa0c6a77, clean)`), baked by
  `build.rs`, and the `self-contract` doctor compares that commit against the
  checkout HEAD — an installed binary serving an older surface warns with the
  remediation `leio-code update`. Dev builds running from inside the checkout
  never warn (drift mid-edit is the normal developer state).
- `leio-code update`: one-command install refresh. Locates the checkout
  (`LEIO_CODE_ROOT` or the compile-time source path), refuses a dirty tree,
  fast-forwards `main`, rebuilds, reinstalls `leio-code` + `leio-harness` into
  `DEST_DIR` (default `~/.cargo/bin`), and refreshes the Codex / Claude Code
  plugin caches through their own CLIs (`--no-plugins` skips; `--force`
  rebuilds even when current).

### Removed
- The entire Milvus integration. `src/milvus/` (REST client, Flight sync,
  adaptive/hybrid retrieval, KB search), the `milvus` CLI family,
  `explain milvus`, the `milvus-collection-drift` / `milvus-reachability` /
  `knowledge-bootstrap` doctors, the pymilvus ingest/sync/eval scripts, the
  tenant-scoped collection env plumbing, and every docs/skills/mirror
  reference are gone. The local node store is Arrow-only:
  `export arrow-nodes` writes `.leio-code/exports/arrow-nodes-v1/nodes.arrow`
  (formerly `milvus-nodes`), and `find` keeps its lexical + FCA + optional
  cosine ranking over that file with no server. `knowledge` answers from the
  local compiled wiki only; the MCP `leio_code_milvus` and
  `leio_code_kb_bootstrap` tools were removed and `search_repository_memory`
  now runs read-only `leio-code find symbol`.
- Vendored example crates. `vendor/fca-fast-core` and the `example-client`
  path dependency are deleted; leio-code no longer vendors or path-depends on
  any example-workspace crate. FCA concept-lattice induction now runs in the
  pre-built `fca_fast` parser wheel (PyO3, committed for macos-arm64 and
  manylinux aarch64/x86_64 under `artifacts/wheels/`, invoked hermetically via
  `uv run --no-project`; override with `LEIO_FCA_WHEEL` /
  `LEIO_FCA_FIND_LINKS`). Membership derivation and single-premise implication
  mining moved into `src/fca.rs` as leio-code code; `export formal-context`
  skips the optional lattice artifacts with a warning when no wheel resolves,
  and nav keeps its index + Arrow fallbacks.

### Fixed
- `skill-contract` no longer warns on non-skills repositories: a repo with
  neither `skills/registry.toml` nor tracked `SKILL.md` manifests now
  deactivates the doctor cleanly (info, never a `--strict` failure), instead
  of emitting an unsilenceable `registry-missing-or-unreadable` warning.
  Repos that do track skill manifests without a registry still get the
  finding.
- Local Apps SDK HTTP (`start-local.sh` on `:8181`) now indexes this
  checkout instead of the parent of `leio-code/` (`apps-sdk/../..` was
  `~/projects`, so `inspect_repository_status` with no args walked every
  sibling tree and died on binary files). Loopback binds accept `repo_root`
  without `LEIO_APPS_SDK_ALLOW_SERVER_REPO_ROOT`; hosted non-loopback still
  rejects it. An unmarked `LEIO_CODE_REPO_ROOT` (no `.git` / `Cargo.toml` /
  `package.json`) falls back to the leio-code tree next to `server.js`.
- `nav goto` no longer hangs when `lattice.json` is missing. Nav loads the
  lattice opportunistically (`try_load_lattice`) and falls back to index +
  Arrow hits, so symbol/file navigation works on any repo; lattice verbs
  (`parent` / `child` / `peer` / `align`) and `export formal-context` fail
  fast with an actionable error instead of running unbounded FCA concept
  enumeration (the example workspace context is 36,505 objects / 287,170
  incidences — full Ganter next-closure never finishes there).
- CLI lookups no longer reindex on almost every invocation: the
  `LEIO_INDEX_TTL_SECS` default is 300s (was 5s), so a typical `find` on the
  example workspace went from ~1.2s (full reindex) to ~130ms (search only).
- Claude/Grok plugin stdio no longer depends on the host cwd. Plugin
  `mcpServers` launches `${CLAUDE_PLUGIN_ROOT}/scripts/launch-stdio-mcp.sh`
  instead of pointing at the project-relative `.mcp.json`, so initialize
  works when the host spawns off the plugin root (the previous relative
  path died with a broken pipe). `self-contract` now checks that map.

### Changed
- On-demand lattice induction is bounded by
  `LEIO_LATTICE_MAX_ON_DEMAND_OBJECTS` (default 5000) and
  `LEIO_LATTICE_MAX_ON_DEMAND_PAIRS` (default 100000). Oversized contexts
  return instructions to build the lattice explicitly (`leio-code export
  formal-context` on a tractable repo) or use the bounded structural route
  (`leio-code export milvus-nodes`).
- MCP surfaces are **dual-era**: **2026-07-28** plus **2025-11-25**
  handshake (`docs/MCP-SPEC-2025-11-25.md`). `server/discover` answers
  before initialize; results carry `resultType: complete` and
  `io.modelcontextprotocol/serverInfo`; `tools/list` includes `ttlMs` /
  `cacheScope`. Legacy hosts keep using `initialize`. Tasks stay
  forbidden. The TypeScript SDK (1.30) is still handshake-based.

## [2.6.0] — 2026-08-13

### Added
- Formal knowledge graph: `knowledge compile` writes `formal.nq` (repo RDF
  + induced OWL + wiki citations; RDF-star → `rdf:Statement`). `knowledge
  explain` answers only through SPARQL or refuses. `knowledge sparql` runs
  a raw query. `nav explain` uses the same proof from the cursor. Identity
  SPARQL (label/heading/IRI) outranks loose facts; ambiguous subjects are
  refused. Prefixes `rdf`/`rdfs`/`owl`/`leio` and discovered `:` are
  injected. Stale `formal.nq` rebuilds when sources are newer. Scoped
  explain prefers `leio:cites` from the current heading. Provenance lists
  `leio:statedIn`. `nav` pins `current_iri`. `knowledge status` reports
  `meta.formal` (triples, prefixes, freshness).
- Concurrent agents: atomic sidecar writes and `.leio-code/*.lock` around
  index / wiki / formal / lattice rebuilds. Nav sessions isolate on
  `LEIO_SESSION` / `--session` (`.leio-code/sessions/nav-<id>.json`).
  Monorepo walks skip `.leio-code`, cap at `LEIO_MAX_INDEX_FILES`, and
  list Cargo/pnpm/npm `workspace_members` on `status`.
- JSON-LD PROV events: every command appends `.leio-code/events/events.ndjson`
  with absolute `fullPath`, `file://` `@id`, and `prov:used`. Checkout
  identity (`repoId`, origin, worktree, branch, HEAD, git common dir)
  keeps multi-repo / multi-worktree / multi-branch journals joinable.
  `LEIO_EVENTS_DIR` collects `{repoId}/{worktreeHash}-{branch}.ndjson`.
- Claude/Codex plugin: SessionStart hook, `scripts/launch-stdio-mcp.sh`
  (pnpm, npm ci fallback), `.claude-plugin/marketplace.json`, doctor
  agent reads live `capabilities`.

## [2.5.0] — 2026-08-13

### Added
- Wiki heading stacks are a category (`Child --subClassOf--> Parent` in one
  file). `lattice.json` v3 stores that category, the `section:path#line` →
  primary-concept alignment, and a real functor witness
  (`wiki_heading_to_lattice`). Heading-stack attributes (`heading:Root`,
  `heading:Child`, …) make nested sections more specific in the lattice.
  `nav goto` sits on a heading (`section:…`, `path#line`, or `Root > Child`);
  `nav related` walks heading parent/child/peer plus lattice family;
  `nav align` reports both identity and heading functors.
- Lattice `nav parent`/`child` keep only strictly more-general / more-specific
  cover neighbors (intent/extent). Wiki sections join the formal context as
  `section:path#line` objects.
- Concept lattice as a category: `export formal-context` writes `lattice.json`
  (cover morphisms = `subClassOf`) and `induced.ttl` (OWL TBox). Identity
  alignment is checked as a functor (fast-align coherence). `nav parent|child|peer|align`
  walks those morphisms and persists `current_concept` on the nav session.
- Local knowledge wiki: `leio-code knowledge compile` writes
  `.leio-code/exports/knowledge-v1/`. `knowledge adaptive|text|status`
  read that store first (Milvus only if it is empty or explicitly forced).
  Ranking is IDF-weighted with phrase/title boosts, YAML titles, match
  snippets, changelog/license/fixture skips, and a header identity so
  the store rebuilds when sources change. v4 stores heading sections
  plus a packed inverted index; search is BM25 on posting candidates,
  collapses sibling sections, and optionally reranks with BGE-M3 when
  `LEIO_CODE_EMBED_URL` is set. Unchanged files are reused.
- Remote BGE-M3 encoder: `[embed] url` / `LEIO_CODE_EMBED_URL` supports direct
  TEI `/embed` and OpenAI-compatible `/v1/embeddings`. Fallback remains
  Example `/v2/embed`.
- Local Arrow adaptive/find uses bounded query-time BGE-M3 reranking: compatible
  stored `semantic_vec` / `code_vec` values are the fast path, while zero or
  incompatible candidates are embedded on demand without persisting request-local
  vectors; lexical/FCA ranking remains the fallback.
- Stateful node navigation: `leio-code nav` / MCP `leio_code_nav`
  (here/goto/select/callers/callees/neighbors/related/back/forward/reset)
  persists `.leio-code/nav-session.json`.

## [2.4.0] — 2026-08-13

### Added
- Local Arrow adaptive search: `milvus adaptive` / multi-word `find` /
  `context` read `.leio-code/exports/milvus-nodes-v1/nodes.arrow` first
  (FCA tags + text). No Milvus process.
- `fca_ready` is local first: Arrow `nodes.arrow` / formal-context export.
  Milvus is an optional ANN/KB sink, not the FCA store.
- `[rdf] namespace` in `.leio-code/config.toml` (env `LEIO_CODE_RDF_NAMESPACE`
  still wins). Default vocabulary is unchanged.
- `leio-code audit` extras now run `graph dead-code` instead of skipping it.
- stdio MCP: `leio_code_milvus`, `leio_code_init`, `leio_code_verify`,
  `leio_code_watch` (start/stop/status). `leio_code_export` accepts
  `format` / `object_kind` / `out`. `leio_code_graph` needle is optional for
  `dead-code`.
- Homebrew formula at `dist/homebrew/leio-code.rb`. Binary release matrix
  includes macOS x86_64.
- Retrieval micro-benchmark: `scripts/benchmark_retrieval.py` and
  `docs/BENCHMARKS.md`.

### Fixed
- Linux `watch` story documented against notify 8 (inotify is a target dep,
  not a missing feature).
- `c4gym_evo_integration` tree-sitter helpers now name lifetimes so rustc
  1.97 accepts the crate.

### Removed
- Milvus Lite (`milvus serve|stop|provision`, `[milvus] mode = "lite"`,
  `scripts/milvus_lite_gateway.py`). Local store is Arrow `nodes.arrow`.

## [2.3.0] — 2026-07-12

### Added
- LEIO-native Codex orchestration with five explicit model/effort/sandbox roles
  and bounded lifecycle hooks.
- Generic `codex-orchestration` and `leio-release-coherence` doctors across
  CLI, capabilities, MCP, Apps SDK, audits, and registry-derived exports.
- Apps SDK: `guide_repository_tools`, `audit_repository_rollup`; Carlos Motta
  tool registers only when `LEIO_VIGOROS_MCP_URL` is set.
- stdio MCP: `leio_code_audit` composite; shared `mcp/guide.js`.
- Docs: `docs/RELEASE-CHECKLIST.md`; POSITIONING/README honesty for generic
  repos + install path (`make leio-code-install-global`).

### Fixed
- First-party LEIO release identity now advances coherently instead of
  remaining at `2.2.0` after the post-release doctor, Flight, knowledge, and
  routing work.
- Apps SDK graph remap and release-surface guidance previously accumulated
  under Unreleased. `leio_code_graph` now maps to `graph_repository` (real
  `leio-code graph`), not `search_repository` / find.

## [2.2.0] — 2026-07-01

### Fixed
- **Main CI health gate** — `pdf-studio-env-coherence` accepts `.env.example` and
  Dockerfile defaults when gitignored `.env` is absent; tessellation FFI contract
  needles restored; `llm-provider-egress` allowlists Plusoft doctor fixtures;
  Sara OpenAI runtime regression test added; `owl_fast` wheel indexed in
  `artifacts/manifest.json`.

## [2.1.0] — 2026-06-10

### Added
- **Generic doctor pack** — the doctor system now delivers value on any repo,
  not just the Example workspace:
  - `env-contract` (generic profile): flags env vars read in code but declared
    nowhere in the repo (.env* files, profiles, secret sets, k8s ConfigMaps,
    inline declarations). Inactive when the repo has zero declaration sources;
    `[doctors.env_contract] allow` supports exact and `*`-suffix exemptions.
  - `import-boundary` (all profiles): ArchUnit-style, cross-language import
    boundaries declared as `[[doctors.import_boundary.rules]]`
    (`name` / `from_prefix` / `deny_prefixes`). No-ops with zero rules.
  - `orphan-files` is now registered for the generic profile with
    config-driven surfaces (`[doctors.orphan_files] surfaces`); Example
    defaults unchanged.
  - `redis-key-hygiene` and `rust-toolchain-pin-coherence` promoted to the
    generic profile (both no-op cleanly when their inputs are absent).
- **`leio-code init`** — one-command onboarding: scaffolds a commented
  `.leio-code/config.toml` (idempotent; `--force` rewrites), builds the index,
  and prints detected facets, capabilities, next-step commands, and an MCP
  wiring hint. Supports `--json`.
- **tsconfig-based TS import aliases** — `compilerOptions.paths` + `baseUrl`
  (JSONC-tolerant, one level of `extends`) now drive TypeScript import
  resolution in the code graph; the legacy hardcoded workspace aliases remain
  only as a fallback when tsconfig produces no candidates.
- `LEIO_CODE_RDF_NAMESPACE` overrides the RDF namespace used by the code-graph
  export (default unchanged).
- Shared JSONC parsing module (`src/jsonc.rs`) extracted from
  `typescript-config-hygiene`.
- **Milvus Lite mode** — single-bundle local vector backend: `[milvus]
  mode = "lite"` plus `leio-code milvus serve|stop|provision`. The embedded
  `scripts/milvus_lite_gateway.py` supervises a `milvus-lite` server and a
  REST v2 shim for the six endpoints the Rust client uses (client
  unchanged); all state lives under `.leio-code/milvus/`. Trace /
  binary-vector search remains standalone-only. See
  `docs/milvus-lite-mode.md`.
- `docs/POSITIONING.md`: market positioning, ICPs, killer use cases, feature
  matrix, SOTA scorecard, prioritized backlog.
- README: Install & Quick Start and Local persistence & privacy sections.

### Fixed
- `mcp/index.js` / `apps-sdk/server.js` doctor lists were missing
  `assurant-seed-wiring` (drift introduced by PR #299), breaking the
  JS/Rust doctor-surface parity test.

## [2.0.0] — 2026-06-08

Crate + plugin release as packaged under `releases/leio-code-plugin-v2.0.0.*`.
Highlights since 1.1.0: context bundles with ordered agent workflow zones,
cross-language route/binary queries, doctor `--explain`, SARIF output,
knowledge-base surfaces, watch mode, Codex/Gemini/Apps SDK packaging.

## [1.1.0] — 2026-05-01

First packaged plugin release (`releases/leio-code-plugin-v1.1.0.*`).
