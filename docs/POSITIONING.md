# LEIO Code — Positioning, ICPs, Killer Use Cases, Feature Matrix

Updated: 2026-07-09
Status: strategy baseline (grounded in codebase audit + competitive research, mid-2026)

---

## 1. Positioning

**Primary statement:**

> LEIO Code gives coding agents a complete, queryable map of your monorepo —
> symbols, call graphs, API routes, env vars, Redis keys, and deploy topology —
> from one local Rust binary, with nothing ever leaving your machine.

**Secondary (doctor-led, for the platform-engineering buyer):**

> ArchUnit for the agent era: LEIO Code turns architecture and deployment
> contracts into executable "doctors," so drift gets caught by an audit
> command — not by a production outage.

**Tactical (anti-grep, for the agent operator):**

> Stop letting agents grep. LEIO Code answers "where is it, who calls it, and
> is it still wired correctly" in one MCP query — across every language,
> config file, and compose profile in your repo.

### Why this is defensible (white space, mid-2026)

1. **Infra-aware code intelligence is unclaimed.** Every competitor indexes
   *code symbols only*. Serena (LSP-over-MCP), codebase-memory-mcp, GitNexus,
   CodeGraph, Aider's repo-map — all stop at functions/classes/imports. None
   index env vars, Redis keys, docker services, deploy targets, or API routes
   into the same graph as the call graph.
2. **Cross-artifact contract drift has no agent-consumable competitor.**
   Fitness-function tools exist (ArchUnit, ts-arch, dependency-cruiser, Tach,
   cargo-deny) but are single-language, import-rules-only, and not
   MCP-exposed. "AI architecture drift" became a named problem in 2026;
   nothing checks "does the deploy topology match the compose files."
3. **The local-first enterprise tier is abandoned.** Sourcegraph closed its
   source (2024) and went enterprise-only ($59/user/mo); Cursor computes
   embeddings in its cloud (documented org-level privacy complaints +
   embedding-inversion risk); Meta Glean is free but datacenter-grade Haskell
   infrastructure. A single Rust binary with local JSON/DuckDB persistence
   sits in the abandoned middle.
4. **Commoditization threat:** free "local code-graph MCP" servers are
   multiplying. Do **not** lead with the symbol graph alone — lead with the
   infra graph + doctors + audit gate.

### Privacy contract (load-bearing claim)

- All persistent state lives under `<repo>/.leio-code/` (JSON index, DuckDB
  FTS sidecar, RDF code graph, query cache, exports).
- The binary makes **no network calls by default**. Outbound traffic happens
  only when explicitly configured: the optional Vigoros bridge (Apps SDK,
  env-gated), and the
  Apps SDK `repo_url` clone path (host-allowlisted).
- Embeddings, when used, are deterministic local SimHash projections or go to
  a service **you** operate. No third-party cloud is ever implied.

---

## 2. ICPs (ideal customer profiles)

**ICP-1 — Platform/staff engineer in a polyglot monorepo (50k LOC–5M LOC).**
Rust/Python/TS mixed; tens of services; compose/k8s profiles; many env vars.
Pain: coding agents grep-thrash and burn context; cloud code-indexing is
banned or scary (fintech, health, gov, EU). Buys: local index + MCP +
cross-language graph. Success metric: agent answers "who calls this / where
is this env bound" in one tool call.

**ICP-2 — AI-forward dev team operating coding agents daily** (Claude Code,
Cursor, Codex, Gemini CLI). Pain: agents re-derive repo structure every
session, token burn, and AI-generated architecture drift sneaking past
review. Buys: `context` bundles + doctors as guardrails + SARIF in CI.
Success metric: measurable token/context reduction and drift caught pre-merge.

**ICP-3 — Ops-heavy tech lead** (many deploy targets, secret sets, env
profiles; "the .env outage" is a lived memory). Pain: config drift between
code, compose files, and deploy scripts is invisible until production. Buys:
`doctor` + `audit --strict` as a pre-deploy gate; `explain env-var` with
value provenance and redaction. Success metric: zero "missing env var"
deploy failures.

Anti-ICP: single-language small repos (an LSP suffices); teams wanting
cloud-managed everything; security scanning use cases (CodeQL's territory).

---

## 3. Killer use cases

1. **One-hop ownership queries.** "Where is `JWT_SECRET` read, where is it
   declared, which deploy target binds it, is it redacted?" — `find env-var`,
   `explain env-var` (value provenance, sha256 fingerprints, TTY-guarded
   secrets).
2. **Agent context bundles.** Natural-language task → ranked files, symbols,
   tests, risks, doctors (`context`). The agent reads 8 files instead of 80.
3. **Pre-deploy audit gate.** `leio-code audit --strict` / `doctor
   --format=sarif` → GitHub Code Scanning. One call, every registered
   contract, non-zero exit on drift.
4. **Refactor blast-radius.** `graph callers-of` / `importers-of` /
   `resolved-importers-of` before any rename or extraction — including
   re-exports and cross-file edges grep misses.
5. **Cross-language tracing.** "Which Python handler spawns this Rust
   binary? Which client hits this route?" — spawn edges, HTTP edges with
   confidence tiers, unresolved edges reported instead of dropped.
6. **Architecture contracts as code (doctors).** Boundary rules, env
   contracts, orphan surfaces — declared in `.leio-code/config.toml`,
   enforced by `doctor`, visible to agents via MCP.
7. **Local knowledge bases.** Compile a docs tree/wiki into a locally-hosted
   node store; adaptive retrieval without any cloud.

---

## 4. Killer feature matrix

### Implemented (ship-quality today)

| Feature | Evidence |
|---|---|
| Local persistence: JSON index v12 + DuckDB FTS sidecar + RDF graph + query cache, all under `.leio-code/` | `src/indexer.rs`, `src/search.rs`, `src/code_graph.rs` |
| Zero-config generic indexing (5 langs via tree-sitter; fresh repo indexes in ms) | empirical: 5-file repo → 3 ms; example 7,353 files → ~1.9 s |
| find: symbol / env-var / redis-key / api-route / docker-service / binary / route / callers | `src/query.rs` |
| explain: env-var (+value provenance, redaction, sha256), redis-key, binary, route | `src/query.rs`, ROADMAP §5 shipped |
| graph: callers/callees/callsites/symbols-in/imports/importers, resolved imports, dead-code | `src/graph_query.rs` |
| Cross-language edges: subprocess + HTTP + Make/npm scripts, 4-tier confidence, unresolved-edge honesty | `src/cross_language/` |
| Doctor runtime: trait registry with capability-derived suites, parallel (rayon), `--strict`, SARIF 2.1.0, exit-code table | `src/doctors/mod.rs`, `src/audit.rs` |
| `context` bundles: RRF fusion, graph proximity, golden-task benchmark | `src/context.rs`, `benchmarks/` |
| MCP (stdio + HTTP) with capability-aware tool routing; Claude/Codex/Gemini/ChatGPT integrations | `mcp/index.js`, `apps-sdk/server.js` |
| Stable output schema v1.0 + JSON-LD + `--where` filter | `docs/output-schema.md`, `src/jsonld.rs` |
| Watch-mode incremental reindex (debounced) | `src/watcher.rs` |
| Self-contract doctor (tool audits itself: manifests, packaging, MCP parity) | `tools/leio-self-doctors/src/doctors/self_contract.rs` |

### Partial (exists, but gated or coupled)

| Feature | Gap | Severity |
|---|---|---|
| **Doctors on generic repos** | The generic profile exposes registry-derived, self-gating suites for repository hygiene, environment/import contracts, Codex orchestration, and LEIO release coherence. Query the live list with `leio-code capabilities`; Example-only doctors remain profile-gated. | Closed for P1; deepen via config-declared rules |
| TS import resolution | `tsconfig.json` `compilerOptions.paths` + `baseUrl` drive resolution; legacy Example aliases are fallback-only | Closed for P1 |
| Onboarding | `leio-code init` scaffolds config, indexes, prints capabilities + MCP wiring | Closed for P1 |
| Orphan-files / route-projection / duckdb-contract doctors | `orphan-files` is config-driven on generic; Example path tables remain for Example-only suites | Medium |
| Knowledge base / semantic search | Local Arrow `nodes.arrow` + formal-context, fully offline | Medium |
| RDF namespace | `LEIO_CODE_RDF_NAMESPACE` + `[rdf] namespace` in `.leio-code/config.toml`; default remains `https://example.local/leio/code#` | Closed |
| Package distribution | Build-from-source + `make leio-code-install-global` + Docker Hub image; crates.io blocked by path deps — see `docs/RELEASE-CHECKLIST.md` | High for public adoption |

### Missing (not started)

| Feature | Why it matters |
|---|---|
| **Config-declared contract rules** (import boundaries, env contracts) so any repo gets doctor value without writing Rust | Converts the moat feature from "Example-only" to "self-service" |
| Package-manager distribution (crates.io blocked by vendored path deps; no brew tap; no CI release binaries) | The #1 adoption blocker |
| Evolving playbook (ACE-style co-edit/doctor/symbol counters) | ROADMAP P1.5; compounding context quality |
| `doctor --suggest` (proposal diffs) | ROADMAP P2 |
| Cross-platform watch story (notify pinned to macOS fsevent) | Linux/Windows self-service |
| Public benchmarks vs Serena/repo-map/grep | SOTA claims need receipts |

---

## 5. SOTA scorecard (category: local-first agent-native code intelligence)

| Dimension | Bar | Today |
|---|---|---|
| Breadth of indexed entities | symbols + infra entities + routes + deploy topology | **Leads category** |
| Contract drift detection | agent-consumable, cross-artifact | **Leads category** (but Example-gated) |
| Privacy / locality | zero network by default, local artifacts | **Meets** (needs public claim + doc) |
| Agent ergonomics | MCP-first, capability routing, stable envelopes | **Meets** |
| Self-service onboarding | one command from zero to answers | **Meets** (`init`); distribution still build/install/Docker (not crates.io) |
| Language breadth | 5 langs tree-sitter vs Serena's 30+ via LSP | **Behind** (mitigated by infra breadth) |
| Benchmarked retrieval quality | published numbers | **Missing** |

Verdict: the engine leads on infra-aware intelligence + doctors; P1 access
gaps (generic doctors, `init`, tsconfig paths) are closed. Remaining adoption
gap is distribution (crates.io blocked; use install-global / Docker / releases
checklist), not core capability.

---

## 6. Prioritized backlog

P1 (shipped — converts the moat to self-service):
1. **Generic doctor pack** — shipped (7 generic suites; see `capabilities`).
2. **tsconfig-paths alias inference** — shipped.
3. **`leio-code init`** — shipped.
4. **Hosted vector-store modes** — removed entirely. Local store is Arrow `nodes.arrow`.
5. **Configurable RDF namespace** — shipped (`LEIO_CODE_RDF_NAMESPACE` + `[rdf] namespace`).

P2 (next):
5. README reorg: Installation/Getting Started first; privacy contract
   section; integration links (Codex/Gemini) surfaced.
6. Distribution: GitHub Releases with CI-built binaries (macOS arm64/x86_64,
   Linux x86_64); brew formula under `dist/homebrew/`; investigate un-vendoring for crates.io.
7. Linux watch backend — notify 8 compiles inotify as a Linux target dep (not a feature). `watch_smoke` is the contract; CI runs it on ubuntu.
8. Published micro-benchmarks: tokens-to-answer vs grep on 3 golden tasks
   (`scripts/benchmark_retrieval.py`, `docs/BENCHMARKS.md`). Serena comparison stays optional.

P3 (roadmap, unchanged): evolving playbook (ROADMAP P1.5), doctor triage
counters, `doctor --suggest`, KB local fallback.

Out of scope (re-affirmed): custom query DSL, auto-fix on semantic rules,
TUI, multi-repo federation.
