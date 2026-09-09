# LEIO Code — Roadmap

Living document. Items are listed in priority order. Each item has a clear
"done when" criterion so progress is measurable.

The guiding principle: **invest in what makes leio-code unique — binding
code ↔ config ↔ targets ↔ secrets — before adding features other tools
already do well.**

---

## P0 — Tighten the unique value

### 1. Incremental indexing (`leio-code watch`)

**Problem.** `bootstrap-kb` is the only path to a fresh graph. The cold cost
punishes the inner debug loop and discourages running `doctor` continuously.

**Done when.**
- `leio-code watch` runs as a long-lived process, watches the repo root,
  debounces file events, and updates the DuckDB + oxigraph index in place.
- A file edit shows up in `find` / `explain` output within 2s of save on a
  workspace the size of `example-workspace/`.
- `--watch` is also accepted by `doctor` so CI-style checks can run on every
  change.

**Non-goals.** Full LSP server. Real-time UI. Watching across multiple repos.

---

### 2. Cross-language edges

**Status: phased delivery. See [docs/cross-language-edges-design.md](docs/cross-language-edges-design.md) for the spec.**

**Problem.** A polyglot monorepo's hardest question — "which Rust binary does
this Python handler invoke?" — is the killer feature for leio-code. Today,
`find callers` likely stops at the language boundary (tree-sitter grammars
are loaded per-language but the cross-language linker is unclear).

**Done when.**
- `find callers <symbol>` traverses at least these three edges:
  - Python `subprocess.run([...])` / `asyncio.create_subprocess_exec` →
    Rust/Node binary by name.
  - HTTP client call (`requests`, `httpx`, `reqwest`, `fetch`) with literal
    URL → server route handler (when the route is statically resolvable).
  - Shell `xtask`, `Makefile`, or `package.json` script → the target binary.
- `explain <binary>` lists upstream callers across all languages, not just
  the language the binary is written in.
- Documented limits: dynamic dispatch and string-built URLs are reported as
  "unresolved edges" instead of being silently dropped.

**Phased delivery** (each phase honors the spec in the design doc):

| Phase | Scope | Status |
|---|---|---|
| 0 | Design doc + ROADMAP marker | shipped (PR #159) |
| 1 / 1b / 4 | JS `child_process`, Rust `std::process::Command`, Makefile + npm script detectors; `SourceLanguage::Make`/`NpmScript`; INDEX_VERSION → 7 | **shipped (this PR)** |
| 2 | `binary` node set (Cargo `[[bin]]` + implicit `src/main.rs` + `src/bin/*.rs`, npm `bin`, pyproject `[project.scripts]`) + resolution pass | sequenced after 1/1b |
| 3 | First-class `UnresolvedEdge` records (classified `reason`, grouped output) | sequenced after 2 |
| 5 | HTTP routes (literal-only) — Flask/FastAPI/Express/axum + requests/httpx/fetch/axios/reqwest | sequenced after 2 |
| 6 | HTTP route templates + confidence math | sequenced after 5 |
| 7 | CLI polish: `find callers <route>/<binary>`, `explain <route>/<binary>`, grouped text rendering | **shipped (this PR)** |

**Already shipped (PR #145):** Python `subprocess.{run,Popen,check_output,check_call,call}([literal, …])` narrow slice — line-anchored regex, literal first arg only. The phases above extend this.

**Non-goals.** Whole-program data-flow analysis. Inferring routes from
template strings. Cross-repo edges. Reverse-engineering compiled binaries.
Auto-fixing route mismatches.

---

### 3. Stable diagnostics — exit codes + SARIF

**Problem.** `doctor` is described as "a fantastic pre-flight CI check," but
CI gating requires stable exit codes and machine-readable output. Today the
contract is implicit.

**Done when.**
- Documented exit-code table: `0` clean, `1` violations, `2` config error,
  `64+` reserved for tool errors. Every doctor sub-command honors it.
- `doctor --format=sarif` emits SARIF 2.1.0 with one `result` per violation,
  including `ruleId`, `level`, `locations[].physicalLocation`, and
  `message.text`. Validated against the SARIF schema in CI.
- `doctor --format=json` for users who don't want SARIF; same field
  semantics, simpler shape.
- The `.leio-code/` directory records the index version + repo commit SHA so
  a report from two weeks ago is reproducible.

**Non-goals.** GitHub Code Scanning integration (downstream of SARIF
support, not a precondition).

---

## P1 — Make the structure useful from the outside

### 4. JSON-LD output for `find` and `explain`

**Problem.** Users want filters ("show targets that use `pacto` but not
`jaipay`"). Inventing a query DSL inside leio-code duplicates capability the
broader Example stack already provides via oxigraph + SPARQL and via the
Crepe/Datalog reasoner in `example-platform`.

**Done when.**
- [x] `find <kind> --format=jsonld` and `explain <id> --format=jsonld` emit
  valid JSON-LD with a published `@context`
  (`https://ontology.getjai.com/leio-code/v1#`, defined in
  `src/jsonld.rs::CONTEXT`). The envelope carries `@context`,
  `@id` (`urn:leio-code:query:<query_id>`), and `@type` (`FindResult` /
  `ExplainResult`); each entity is augmented with an `@type` derived from
  the envelope's `query_id` (e.g. `EnvVar`, `DeployTarget`).
- [x] The output round-trips through `jq` (raw JSON shape preserved; tested
  in `tests/jsonld_output.rs`) and is structurally compatible with
  oxigraph JSON-LD ingest. Live SPARQL ingest is not yet documented end-
  to-end — see "Still to do".
- [x] A tiny `--where '<jq-expression>'` shortcut exists for the 80% case.
  Supported grammar (see `src/jsonld.rs` module docs):
  - `.entities[] | select(<path> == <json-literal>)`
  - `.entities[] | select(<path> != <json-literal>)`
  - `.entities[] | select(<path> | contains(<string-literal>))`
    (substring on string fields; not full jq `contains`).
  Anything else returns a clear "unsupported filter" error so callers can
  pipe through real `jq` for richer queries.
- [x] README documents the recipe section: `leio-code find … |
  jq … | leio-code explain --stdin` (see "`explain --stdin` — pipe entities
  back through explain" in README.md).

**Still to do.**
- ~~README documentation pass: add a "JSON-LD output & filtering" section
  with the supported `--where` grammar, the `@context` IRI, and an
  end-to-end oxigraph SPARQL ingest example.~~ — **shipped (PR #175).**
  New top-level "JSON-LD output & filtering" section in README before
  "Cross-language queries" documents the envelope shape, the three-form
  `--where` grammar, and three end-to-end recipes (`find → jq → explain`,
  oxigraph SPARQL ingest with honest caveats, SARIF CI gate).
- ~~A `leio-code explain --stdin` helper~~ — **shipped.** Consumes a
  whole envelope / JSON array / concatenated entity stream from stdin,
  dispatches each entity by `@type` to the matching `explain_*`, skips
  unknown / malformed entities with a stderr warning, and emits one
  envelope per entity (`--format=text|json|jsonld`). See README §
  "`explain --stdin` — pipe entities back through explain".
- Richer `contains`: only string substring is supported today; jq's full
  `contains([{…}])` array/object form is deferred until a real use case
  appears (test in `where_with_contains` documents the narrower form).
- ~~Vocabulary alignment~~ — **shipped.** Aligned `CONTEXT` from
  `https://leio.code/ns/v1#` to `https://ontology.getjai.com/leio-code/v1#`
  to match the canonical Example ontology IRI base (used by
  `cartridges/sisfron/ontologies/*.ttl` and the wider stack). Pre-v1.0
  stabilization — no consumer pinned against the earlier value yet.

**Non-goals.** A custom query language. A GraphQL endpoint. Subscriptions.

---

### 5. `explain` shows values, with redaction and provenance

**Status: env-var slice SHIPPED.** Tracked follow-ups below.

**Problem.** Today `explain` shows the shape of a target / secret set / env
var binding, but not whether a value is set, where it comes from, or what
its current value is. Users fall back to grepping `.env*` files.

**Shipped — `explain env-var`:**
- Index version bumped to 4; root `.env*` files (`.env.local`, `.env`,
  `.env.example`, `.env.production`, …) are now indexed with precedence.
- `explain env-var <NAME>` returns a `value_bindings` array and an
  `effective` field on the entity JSON, with per-binding `state`
  (`set`/`unset`/`empty`), `display`, `redacted`, and `source` (`env_file`,
  `deploy_profile`, `secret_set`) including the path + precedence.
- Secret-keyed names (`*_SECRET|*_KEY|*_TOKEN|*_PASSWORD|*_PWD|*_PRIVATE_KEY|
  *_CREDENTIAL[S]`) are redacted by default to `[redacted, N chars]`.
- New `--show-secrets` CLI flag reveals raw values.
- Integration test suite at `tests/explain_values.rs` (7 cases) covers the
  contract end-to-end via `build_or_update_index` → `explain_env_var`.

**Also shipped (this branch, follow-up commits):**
- `explain deploy-target <name>` now surfaces a `var_bindings` object on the
  entity. For every env var declared by the target's `backend_profile` and
  `secret_set`, the response carries `is_secret`, the full ordered
  `bindings` list (env_file / deploy_profile / secret_set provenance), and
  an `effective` field pointing at the highest-precedence source. Redacted
  by default; `--show-secrets` (already CLI-plumbed and TTY-guarded) reveals
  raw values. Closes the "show me targets that use FOO but not BAR" query
  shape — each target now carries its resolved variable values.
- sha256 fingerprint in redacted display: `[redacted, N chars, sha256:<8hex>]`.
  Lets callers spot when two redacted slots hold the same value (e.g.
  `.env.local` and `.env` got out of sync) without leaking the value.
- `--show-secrets` is refused outside a TTY unless paired with
  `--i-know-what-i-am-doing`. Prevents accidentally piping raw secrets
  into log collectors or CI captures. The policy is a pure function
  (`check_show_secrets_guard`) so the test suite covers all 4 truth-table
  cells.
- Multi-line dotenv values. Parser is now state-machine driven and
  consumes lines until the matching closing quote when an opening `"` or
  `'` is left dangling on a `KEY=` line. The full body is stored in a new
  `DeclaredVar::raw_value` field (existing `value_preview` keeps its
  48-char single-line behavior for backwards compat). Non-secret display
  collapses to `<first line> […N more lines]`; secret redaction's
  `N chars` count and sha256 fingerprint now cover the full multi-line
  content. Index version bumped to 6. **Honest limits:** escape sequences
  (`\"`, `\n`), nested quotes, and backtick-quoted values are not
  supported. Unterminated quotes are dropped (safer-skip) rather than
  consuming the rest of the file.

**Still to do (follow-up PRs):**
- Shell-environment and k8s-configmap provenance variants are reserved on
  `BindingSource` but not populated yet. Add when the indexer learns to
  read them (or when a runtime-bound resolver is added).

**Non-goals.** Editing values. Pulling from remote secret stores. History.

---

## P1.5 — Evolving context and self-improvement

Inspired by ACE (Agentic Context Engineering, Zhang et al. 2025,
arXiv:2510.04618). Core insight: contexts that accumulate and refine
across sessions outperform static one-shot bundles by +10-17%. The
techniques below are structural — no LLM-in-the-loop for curation.

### 5a. Evolving playbook for `leio_code_context`

**Problem.** `leio_code_context` builds a ranked context bundle per query
from the static index. Every session starts from zero — no memory of which
files are commonly co-edited, which doctors fire most, or which symbols
are most queried. ACE shows that persisting and refining this knowledge
across sessions compounds accuracy.

**Done when.**
- A `.leio-code/playbook.json` file persists across sessions, accumulating:
  - **Co-edit frequency**: pairs of files edited in the same commit
    (extracted from `git log --name-only`). Used to boost related files
    when one is in the context.
  - **Doctor hit counters**: per-doctor `{fired, resolved, ignored}`.
    Doctors with high `ignored/fired` ratio are demoted in context
    ranking; doctors with high `resolved/fired` are promoted.
  - **Symbol query frequency**: which symbols/env-vars/Redis-keys are
    queried most. Hot symbols get priority in context bundles.
  - **Session outcome signals**: when a session ends with a successful
    commit (no reverted files), the context items that were active during
    that session get a `helpfulness += 1` bump. Reverted edits get
    `harmfulness += 1`.
- `leio_code_context` uses the playbook to re-rank its output: files
  co-edited with the current target get a boost proportional to
  co-edit frequency; hot symbols float to the top.
- The playbook is updated incrementally (delta merge, not full rewrite)
  and capped at 10K entries with LRU eviction.
- `leio-code playbook stats` prints a summary (top co-edits, hottest
  symbols, noisiest doctors).

**Non-goals.** LLM-based curation (the Curator role from ACE). The
playbook is maintained by deterministic counters and git history mining,
not by prompting a model. No network calls.

---

### 5c. Doctor warning triage counters

**Problem.** With 68 doctors and strict mode ON in CI, false positives
block PRs. Today the `doctor-warning-ledger` is a static script that
snapshots warnings. It doesn't track whether warnings are being acted on
or ignored over time. ACE's helpfulness/harmfulness counters solve this.

**Done when.**
- Each doctor warning in `.leio-code/playbook.json` carries:
  - `fired_count`: how many times this specific warning was emitted
  - `resolved_count`: how many times the warning disappeared in the
    next commit after being emitted (developer fixed it)
  - `ignored_count`: how many times the warning persisted across 3+
    commits (developer did not act on it)
- `leio-code doctor triage` prints a ranked table:
  - Doctors sorted by `ignored / fired` ratio (noisiest first)
  - Doctors with `ignored/fired > 0.8` are flagged as candidates for
    rule refinement or demotion
  - Doctors with `resolved/fired > 0.9` are validated as high-signal
- The triage data feeds back into the playbook (item 5a) so noisy
  doctors are demoted in context ranking automatically.

**Non-goals.** Auto-disabling doctors. The triage is informational;
humans decide whether to fix the doctor or adjust the rule.

---

## P2 — Improve the doctor surface without overreach

### 6. `doctor --explain <rule>` and `doctor --suggest`

**Problem.** Doctors detect issues but don't say *how* to fix them. Full
autofix on semantic rules (contract drift, route projection) is dangerous —
the precision threshold required for autofix-on-CI is far above what these
rules can offer.

**Shipped.**
- `doctor --explain <rule-id>` prints, for each violation: the rule's
  citation in the spec, the offending file:line, and a conceptual fix
  description. Static text per rule — no code generation. `--explain list`
  prints every documented rule id with a one-line summary. The rule
  registry lives in `src/diagnostics.rs` (`RuleDoc` / `rule_doc` /
  `all_rule_docs`) and currently documents seven rules across the
  redis-key-hygiene, semantic-wiring, model-cache-contract,
  route-projection, tenant-identity, and deploy doctors.

**Still to do.**
- `doctor --suggest <rule-id>` emits a unified diff to stdout as a *proposal*
  the user can inspect, edit, and apply with `git apply -`. Never writes to
  the working tree. Deferred to a follow-up PR.
- Each rule declares its `suggest_confidence`: `low | medium | high`. Only
  rules where the fix is mechanical (e.g., adding a missing env var
  declaration to a manifest) qualify for `--suggest`. Deferred with
  `--suggest`.

**Non-goals.** `--fix` that writes to disk. Auto-applied PRs. AI-generated
patches.

---

## P3 — Documentation and surface tidying

### 7. Document or hide the FCA / RDF surface — SHIPPED (documented)

The four `make leio-code-*-formal-context*` / `make leio-code-build-fca-wheel`
targets are user-facing power-user features (referenced from
`docs/leio-code-formal-context.md`, `docs/jcube-modal-runbook.md`,
`docs/contributing/leio-code.md`). The README now has a dedicated "FCA / RDF — Concept analysis
and graph export" section right after "Start Here" with a table for
each target (what it produces, when to use it) and a paragraph on how
the artifacts compose with `doctor` / `explain`. The existing
Apple-Silicon recipe got a stable heading so the new section can link
to it. No targets were renamed or hidden.

**Original framing kept for reference.**

**Problem.** `Makefile` exposes `leio-code-export-formal-context`,
`leio-code-induct-formal-context`, `leio-code-export-fca-memberships`, and
`leio-code-build-fca-wheel`. The README's "Start Here" never mentions them.
Either they're load-bearing power-user features or they're internal
plumbing — pick one.

**Done when.**
- If user-facing: README has a section explaining the FCA/RDF surface,
  including when to use `export-formal-context` vs. `induct-formal-context`,
  what the membership matrix is for, and how it composes with `doctor` and
  `explain`.
- If internal: rename to `leio-code-internal-*` or move into a `dev/`
  Makefile so they don't surface in `make help`.

---

### 8. Stable, documented output schema for `find` and `explain`

**Status: SHIPPED.**

**Problem.** Scripting against the CLI today is a moving target. A
contract is what turns leio-code from "useful tool" into "platform
primitive."

**Shipped:**
- `docs/output-schema.md` is the canonical contract for QueryEnvelope,
  every entity kind (Symbol/EnvVar/RedisKey/DeployTarget/SubprocessCall/
  Cartridge/ApiRoute/DockerService), the SARIF + JSON diagnostic
  formats, the JSON-LD wrapping with `@context`/`@id`/`@type`, the
  `--where` filter grammar, the exit-code table, and the
  add-vs-remove-vs-rename versioning policy.
- `schema_version: "1.0"` is emitted as the first key of every
  `QueryEnvelope` and of every diagnostic-format root document (JSON,
  JSON-LD, SARIF). Pinned to `"1.0"` until a breaking change ships; the
  value is decoupled from the crate's `Cargo.toml` version. Tested in
  `tests/schema_version.rs`.

**Still to do (follow-up PRs):**
- ~~Document `audit` markdown output, `export <kind>` file outputs,
  `knowledge` result shapes, and `context` task-bundle JSON
  as their contracts stabilize.~~ **shipped (this PR).** All four
  surfaces are now pinned in `docs/output-schema.md` as §8 (audit),
  §9 (export), §10 (knowledge), §11 (context). Stability is
  called out per surface: audit + export envelopes + context bundle
  keys are stable; export file payloads are versioned via
  `manifest.json` (formal-context-v1, code-graph-v3, arrow-nodes-v1,
  hypergraph schema `example.formal_context_hypergraph.v1`);
  knowledge `metadata` / `relations` / search `_search.stage`
  strings are unstable-but-aspires-to-stable.

---

## Out of scope (intentionally)

These come up in conversations and we keep saying no, on purpose. Recorded
here so we don't relitigate.

- **Custom query language inside leio-code.** Use JSON-LD + jq + SPARQL.
- **Auto-applied fixes for semantic doctor rules.** Precision is too low;
  failure cost is too high.
- **An interactive TUI.** The CLI + JSON-LD output is the contract; UIs
  belong upstream (`ops-console`, `health-audit-console`).
- **Indexing remote repos.** Single-repo tool. Multi-repo composition is a
  job for the wider platform.

---

## How to use this file

- Items move from "P*" sections into a `## Done` section when shipped, with
  the commit SHA that closed them.
- New ideas land in P3 or "Out of scope" and get promoted only with
  evidence (a real use case, a real cost of not doing it).
- "Done when" criteria are the contract — if the criterion is fuzzy, the
  item isn't ready to start.
