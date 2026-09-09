# Cross-Language Edges — Design Doc (P0 #2)

**Status: PROPOSAL.** Pre-implementation design. Awaiting review before
the first detector PR lands.

This document defines the **schema, detector taxonomy, resolution logic,
and CLI surface** for cross-language edges in `leio-code`. The narrow
Python `subprocess` slice (PR #145) shipped under this umbrella;
everything below extends that foundation.

The guiding rule: **never claim an edge we can't see.** Unresolved
references become a first-class "unresolved edge" record rather than
being silently dropped — this lets `doctor` surface "you spawn a binary
from this Python file but we can't tell which one" as a finding instead
of a black hole.

---

## 1. Motivation

A polyglot monorepo's hardest question — "which Rust binary does this
Python handler invoke?" — has no general answer today. Tree-sitter
grammars are loaded per-language; the cross-language linker is a single
narrow detector for Python `subprocess` calls (PR #145). To make
`find callers <symbol>` and `explain <binary>` actually useful as
agent-facing surfaces, we need edges that:

- Traverse process boundaries (subprocess, spawn).
- Traverse network boundaries (HTTP client → server route handler).
- Traverse build-tool boundaries (Make, npm scripts, xtask) → the
  binaries they ultimately invoke.
- Carry **provenance** (source file:line, language, idiom) so the
  edge is auditable.
- Carry **confidence** (literal vs. heuristic vs. unresolved) so
  consumers can gate on quality.

---

## 2. The graph model

Cross-language edges live on a new `CrossLanguageGraph` struct on
`RepoIndex`, alongside the existing `files`, `deploy_targets`,
`env_files`, etc. The graph is computed at index time from per-language
detectors and resolved before the index is written.

### 2.1. Node taxonomy

A node is a **target endpoint** that an edge can point at. There are
three kinds:

| Node kind | Identity | Source |
|---|---|---|
| `binary` | `name` (basename or absolute path as written) | Cargo: explicit `[[bin]]` blocks, implicit `src/main.rs` (Cargo's default-bin convention — most binaries in this workspace are declared this way), and `src/bin/*.rs` files (one binary per file). Scanned **per workspace member**, not just the workspace root, since multi-crate workspaces (e.g. `example-gateway`, `example-platform`, `example-align`, `office-parsers-rs`) each declare their own binaries. Also: `package.json bin`, `pyproject.toml [project.scripts]`, files in repo `bin/`, shebang-bearing files |
| `route` | `(method, path_template)` — e.g. `("GET", "/api/users/{id}")` | `@app.route("/...")` Flask, `app.{get,post,...}("/...")` Express, `axum::Router::route("/...", ...)` Rust, similar idioms |
| `script` | `(file, target_name)` — e.g. `("Makefile", "test")`, `("package.json", "build")` | Makefile targets, npm scripts, just/xtask recipes |

Every node carries `path` + `line` of its **declaration** (where the
binary/route/script is *defined*, not where it's called from). The
declaration may be in a manifest (Cargo.toml, package.json) — that's
intentional; the manifest is the contract.

### 2.2. Edge taxonomy

An edge is a **caller → callee** reference. Five edge kinds:

| Edge kind | From (caller) | To (callee node) | Detector |
|---|---|---|---|
| `subprocess_spawn` | source file:line | `binary` by name | Python subprocess (shipped), JS `child_process`, Rust `std::process::Command` |
| `http_call` | source file:line | `route` by `(method, path)` | Python `requests`/`httpx`, JS `fetch`/`axios`, Rust `reqwest`/`hyper`, Go `http.Client`, … |
| `script_invocation` | one `script` node | another `script` node OR `binary` | Makefile recipe lines, npm script `&&` chains |
| `manifest_declares` | `script` node | `binary` it ultimately runs | Cargo.toml `default-run`, npm `bin`, Python `[project.scripts]` |
| `unresolved` | source file:line | `UnresolvedTarget { kind, why }` | Any detector that sees a call but can't pin the target |

### 2.3. Edge confidence

Every resolved edge carries a `confidence: u8` (0–100):

| Confidence | Meaning |
|---|---|
| `95–100` | Literal-to-literal match. Static-string client → static-string declaration in the same repo. **Safe to gate on.** |
| `80–94` | Mostly-literal with one normalization step (e.g. `~/.cargo/bin/leio-code` resolves to a Cargo target via basename). |
| `60–79` | Heuristic match — e.g. URL `/api/users` matched a route template `/api/{resource}` by best-effort path-segment alignment. |
| `0–59` | Suspected — call site looks like a spawn/HTTP call but the target string is built dynamically and we found a *plausible* match. **Don't gate on these; surface to humans.** |

Unresolved edges have no confidence (they're not edges to any node).

### 2.4. Unresolved-edge shape

```rust
pub struct UnresolvedEdge {
    pub source_file: String,
    pub source_line: usize,
    pub edge_kind: EdgeKind,             // subprocess_spawn | http_call | …
    pub reason: UnresolvedReason,        // dynamic_first_arg | format_string | …
    pub raw_snippet: String,             // ~80 chars of the offending line
}

pub enum UnresolvedReason {
    DynamicFirstArg,       // `subprocess.run(cmd)` where cmd is a variable
    FormatString,          // `subprocess.run([f"bin-{x}", ...])`
    StringConcat,          // `requests.get(BASE_URL + "/path")`
    PathObject,            // `subprocess.run([Path("..."), ...])`
    ShellTrue,             // `subprocess.run("...", shell=True)`
    NonLocalTarget,        // matched binary name not declared anywhere in repo
    UnknownIdiom,          // we recognized the import but not this call shape
}
```

These show up in `find callers` output under a separate `unresolved` array
so consumers know the difference between "no callers" and "callers exist
but we couldn't pin them."

---

## 3. Detector taxonomy

Each detector is a function in `src/cross_language/<idiom>.rs` that takes
`(source: &str, path: &str, language: SourceLanguage)` and returns a
`Vec<DetectorOutput>` where:

```rust
pub enum DetectorOutput {
    Subprocess(SubprocessCallOccurrence),
    Http(HttpCallOccurrence),
    Script(ScriptInvocationOccurrence),
    Unresolved(UnresolvedEdge),
}
```

### 3.1. Subprocess spawn

| Language | Idioms detected | Status |
|---|---|---|
| Python | `subprocess.{run,Popen,check_output,check_call,call}([literal, …])` | **Shipped** (PR #145) |
| Python | `asyncio.create_subprocess_exec(literal, …)` | Follow-up — same shape, async wrapper |
| JavaScript / TypeScript | `child_process.{spawn,exec,execFile,fork}("literal", …)` | First new PR |
| Rust | `std::process::Command::new("literal")`, `tokio::process::Command::new("literal")` | First new PR |
| Go | `exec.Command("literal", …)` | Deferred |

**Resolution.** The literal binary name is matched against the `binary`
node set (Cargo `[[bin]]`, npm `bin`, Python `[project.scripts]`,
shebang scripts in `bin/`). On match: edge with confidence ≥ 95. On
no-match: emit `UnresolvedEdge { reason: NonLocalTarget }` — the binary
is real but lives outside the repo (system tools like `git`, `curl`).

### 3.2. HTTP calls → routes

This is the **hardest piece**. The design here:

**Calibration note for this monorepo specifically.** A quick survey of
`example-workspace` found Express (`leio-code/apps-sdk/server.js`) and
reqwest (`leio-code/vendor/example-client`) as the dominant idioms —
Python HTTP-server frameworks are barely used in the indexed surface,
and `ExampleRouter`/`MyRouter` patterns in `src/indexer.rs` are
dormant/test-only. Phase 5 should focus the v1 server idioms on
**Flask, FastAPI, Express, axum** (the common foursome) and the v1
client idioms on **requests, httpx, fetch, axios, reqwest**. Add other
frameworks (actix-web, hyper, aiohttp, …) as separate follow-up PRs
only when a real codebase needs them.

#### 3.2.1. Detect client calls with literal URLs

```rust
// Python
requests.get("/api/users")
httpx.post("https://service-a/v1/foo")
// JS
fetch("/api/users")
axios.get("/api/users")
// Rust
reqwest::get("/api/users")
client.get("/api/users")
```

Each captures `(method, url_string, file, line)`. Heuristic for method:
the function name (`get`/`post`/`put`/`delete`) or the `method:` kwarg
when present.

#### 3.2.2. Detect server route declarations

```python
# Flask
@app.route("/api/users", methods=["GET"])
@app.get("/api/users")  # Flask 2+
# FastAPI
@app.get("/api/users/{user_id}")
```

```javascript
// Express
app.get("/api/users", handler)
router.post("/api/users", handler)
```

```rust
// axum
Router::new().route("/api/users", get(list_users))
// actix-web
HttpServer::new(|| App::new().service(get_users))  // function-attribute form
```

#### 3.2.3. Match client URL → server route

**Static literal → static literal:** confidence 95+. Direct string compare.

**Static literal → template route:** confidence 80. We parse the route's
path template (Flask `<int:id>`, FastAPI `{user_id}`, axum `:id`) into a
regex, then test the client URL against it. If exactly one route
matches, attach.

**Multiple route matches** for the same URL: emit edges to all matched
routes with confidence reduced proportionally. Frame as "candidates" in
the entity output.

**Dynamic URL** (`f"/api/{id}"`, `BASE_URL + path`, etc.): emit
`UnresolvedEdge { reason: FormatString | StringConcat }`. Do NOT guess
which route was meant.

**Cross-host URLs** (`https://other-service/...`): match against routes
in the same repo when the host resolves to a known service name (e.g.
`EXAMPLE_GATEWAY_URL`-style env var → known service). Otherwise emit
unresolved with `reason: NonLocalTarget`.

**Confidence calibration is a non-goal for v1.** First implementation:
just literal-to-literal at confidence 95, template-with-one-match at
80, everything else unresolved. The fancier confidence math comes later
or never.

### 3.3. Script invocations

#### 3.3.1. Makefile recipes

Parse the Makefile (any recognized name: `Makefile`, `makefile`,
`GNUmakefile`). For each target, scan recipe lines (lines starting with
tab). Match recipe lines that look like `<word> [args...]` against
the binary node set. Match recipes that call other Make targets
(`$(MAKE) other-target`, `make other-target`).

**Out of scope for v1:** macros, conditionals, eval, recursive variable
expansion. If a recipe uses `$(VAR)` in the binary position, emit
unresolved.

#### 3.3.2. npm/package.json scripts

Parse `package.json::scripts`. Each script value is a shell command.
Tokenize on `&&`/`;`/`|` (best-effort — no real shell parser); for each
tokenized command, if the first word is a known binary or another
script (`npm run other`, `pnpm other`), record an edge.

#### 3.3.3. xtask / just / cargo aliases

`xtask` is a Rust binary like any other — already covered by §3.1.

`just`'s `justfile` is similar to Makefile recipes; parse if present.

Cargo aliases (`.cargo/config.toml::[alias]`) map a short name to a
`cargo` invocation — these are aliases, not edges.

---

## 4. Resolution algorithm

The detector pass produces raw `DetectorOutput`s. A separate
**resolution pass** turns them into edges in the `CrossLanguageGraph`:

```
phase 1 (detect): per-file scan → Vec<DetectorOutput>
phase 2 (collect): build the node set (binaries, routes, scripts)
                   from manifests + detected declarations
phase 3 (resolve): for each Subprocess/Http/Script DetectorOutput,
                   try to match against a node:
                     - exact literal: edge with confidence 95+
                     - template/heuristic: edge with confidence 60-94
                     - no match: UnresolvedEdge with classified reason
phase 4 (write): CrossLanguageGraph { nodes, edges, unresolved } is
                 attached to RepoIndex
```

The phase split is important: it lets the resolution layer evolve
without changing the detectors, and it lets `--explain` show resolution
provenance (`"matched route /api/users/{id} via template expansion"`).

---

## 5. CLI surface

### 5.1. `find callers <target>`

`target` is interpreted in this order:

1. A symbol name → existing symbol-graph query (unchanged).
2. A binary name → list every `subprocess_spawn` and `script_invocation`
   edge whose target binary matches.
3. A route path → list every `http_call` edge whose target route matches.

Output entity carries `@type: Callers` (JSON-LD), with sub-arrays
`subprocess_callers[]`, `http_callers[]`, `script_callers[]`, and
`unresolved[]`. The latter is **always present** when relevant — empty
array if no unresolved edges, omitted only when no detection ran.

### 5.2. `explain <binary>` / `explain <route>` / `explain <script>`

Add new ExplainKind variants:
- `binary` — for a binary node, show declaring manifest, callers (per
  edge kind), and any scripts that ultimately invoke it.
- `route` — for a route node, show the handler file:line and every
  client edge pointing at it (with confidence).
- `script` — for a Makefile target or npm script, show the chain of
  binaries it invokes.

### 5.3. `--format=text` rendering

Group by edge kind, then sort by confidence descending:

```
Callers of binary `leio-code` (4 resolved, 1 unresolved):
  subprocess_spawn (3):
    scripts/run_doctor.py:12     [confidence 100]
    ops-console/build.py:47      [confidence 100]
    cartridges/foo/setup.py:8    [confidence 100]
  script_invocation (1):
    Makefile:42                  via target `lint`  [confidence 95]
  unresolved (1):
    scripts/dynamic.py:18        reason: dynamic_first_arg
```

### 5.4. JSON-LD shape

Each detected edge becomes an entity:

```jsonc
{
  "@type": "SubprocessSpawn",
  "from": { "path": "scripts/run.py", "line": 12, "language": "python" },
  "to": { "@id": "urn:leio-code:binary:leio-code",
          "@type": "Binary",
          "name": "leio-code",
          "manifest": "leio-code/Cargo.toml" },
  "confidence": 100
}
```

Unresolved edges:

```jsonc
{
  "@type": "UnresolvedEdge",
  "from": { "path": "scripts/dynamic.py", "line": 18 },
  "edge_kind": "subprocess_spawn",
  "reason": "dynamic_first_arg",
  "raw_snippet": "subprocess.run(cmd)"
}
```

Schema additions versioned in `docs/output-schema.md` (P3 #8, PR #155).

---

## 6. Phased delivery

| Phase | Scope | PR target | Effort |
|---|---|---|---|
| **0 — Design** | This document | This PR | done with this PR |
| **1 — Subprocess parity** | JS `child_process` + Rust `std::process::Command` detectors. Reuse §3.1 design. Per-language tests. | 2 separate PRs | M each |
| **2 — Manifest binary nodes** | Build the `binary` node set from `Cargo.toml [[bin]]`, `package.json bin`, `pyproject.toml [project.scripts]`. Wire the resolution pass. | 1 PR | M |
| **3 — Unresolved-edge first-class** | Promote unresolved detections from silent skip → `UnresolvedEdge` records with classified `reason`. | 1 PR | S |
| **4 — Script invocations** | Makefile parser + npm scripts parser + manifest_declares edges. | 1 PR | L |
| **5 — HTTP routes (literal)** | Detect server routes (Flask/FastAPI/Express/axum) and client calls (`requests`/`fetch`/`reqwest`). Literal-only matching, no templates. | 1 PR | L |
| **6 — HTTP route templates** | Parse template syntaxes, match client URLs against templates with confidence math. | 1 PR | L |
| **7 — CLI polish** | `find callers <route>`, `explain <route>`, `--format=text` grouped rendering. | 1 PR | M |

**Effort scale.** S = ≤ 1 session, M = 1 focused session with TDD,
L = 1–2 sessions with design refinement during. Total: ~7–10 PRs over
several days of work.

Phase 1 can ship immediately (the design here is sufficient). Phases
4–6 each warrant a one-line design refinement PR-comment before
implementation.

---

## 7. Non-goals (explicit)

These are **out of scope** for the entire P0 #2 effort. Recording here
so we don't relitigate:

- **Whole-program data-flow analysis.** No tracking of variables across
  function boundaries. If the binary name is in a variable, it's
  unresolved.
- **Template-string inference.** No reasoning about which route
  `f"/api/{thing}"` could resolve to.
- **Cross-repo edges.** This is a single-repo tool. Imports from other
  repos in the monorepo are fine; calls into separately-versioned
  upstream services are unresolved.
- **Reverse-engineering compiled binaries.** Detectors run on source,
  not artifacts.
- **Auto-fixing route mismatches.** `doctor` may surface drift; humans
  fix it.
- **Real-time graph updates** beyond what `leio-code watch` already
  provides (P0 #1, shipped).

---

## 8. Open questions

These need resolution before the relevant phase implements them.
Tracked here so they don't get lost.

1. **Where in `model.rs` does `CrossLanguageGraph` live?** Top-level
   field on `RepoIndex` is simplest, but the graph is denormalized vs.
   the existing per-file storage. Decision: separate top-level field;
   the slight duplication is worth the queryability win. **Confirm
   in Phase 2 PR.**

2. **Index-version bump strategy.** Each phase adds fields. We've been
   bumping monotonically (5 → 6 in the multi-line dotenv PR). If we do
   it per-phase that's 5 bumps for P0 #2 alone. **Decision:** bump
   once per phase that changes the serialized schema, document each
   in `docs/output-schema.md` §7.

3. **Unresolved-edge volume.** A large polyglot repo could easily have
   thousands of unresolved subprocess/HTTP calls. **Decision:** group
   unresolved-edges by `reason` in the output; show top 20 per group
   with a count. Full list via `--all-unresolved`.

4. **Route-template syntax normalization.** Flask uses `<int:id>`,
   FastAPI uses `{id}`, axum uses `:id` then later `{id}`, Express
   uses `:id`. **Decision (proposal):** normalize all to FastAPI form
   `{name}` internally; show the original syntax in display when the
   edge is rendered. Confirm in Phase 6 PR.

5. **Confidence calibration.** The current ranges (95+ / 80-94 / 60-79
   / 0-59) are gut-feel. **Decision:** ship them as proposals;
   re-calibrate after seeing real distributions in `example-workspace`.

6. **Should `find callers <symbol>` integrate cross-language edges?**
   Today it walks the symbol-graph only. **Proposal:** keep them
   separate (`find callers <symbol>` for symbol graph; `find callers
   <binary>` / `<route>` for cross-language graph) — overlap is
   minimal in practice. Reviewers: object if you disagree.

---

## 9. Test strategy

Each phase ships with integration tests in `tests/cross_language_<phase>.rs`
that build synthetic repos via `tempfile::TempDir` and assert on the
graph produced. Pattern matches the existing
`tests/python_subprocess_edges.rs` (PR #145).

Per phase, at minimum:
- **Detect** what the spec says we detect.
- **Skip** what the spec says we skip (each skip case becomes a test
  asserting "0 edges, N unresolved with the expected reason").
- **Resolve** literal-to-literal matches to the right node.
- **Surface as unresolved** anything that should be.
- **Confidence** stays within the stated band.

The shared test scaffolding (`write_repo`, `build_index`) lives in
`tests/common.rs` once Phase 2 adds it; phases 3+ reuse.

---

## 10. Status

| Phase | Status |
|---|---|
| 0 | This PR |
| 1 | Ready to implement after this PR merges |
| 2–7 | Sequenced; design above is sufficient to brief implementer agents |
