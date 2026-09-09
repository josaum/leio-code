# LEIO Code technical guide

[Product overview](../README.md) · [Install](install-stdio.md)

## First run

Start with an explicit repository root.

```bash
leio-code init --repo /path/to/your-repo
leio-code status --repo /path/to/your-repo
leio-code find symbol MyType --repo /path/to/your-repo
leio-code context "fix the payment validation flow" --repo /path/to/your-repo
leio-code graph callers-of validate_amount --repo /path/to/your-repo
leio-code doctor all --repo /path/to/your-repo
```

`init` writes a commented `.leio-code/config.toml`, builds the index, and
prints the profile it detected. Default profile is `generic`: symbols, env
vars, Redis keys, API routes, docker services, binaries, spawn/HTTP edges.

Pass `--repo` on every command (or `cd` there and use `--repo .`).

## Which command

| You want | Use |
| --- | --- |
| Exact ownership of a symbol, env var, Redis key, route | `find` |
| A ranked working set for a natural-language task | `context` |
| What a name *means* in this tree (bindings, callers, secrets redacted) | `explain` |
| Contract / wiring drift | `doctor` |
| Callers, callees, imports, dead code | `graph` |
| Walk files, headings, lattice; pin an IRI | `nav` (`explain` = SPARQL proof) |
| Local wiki + formal graph | `knowledge compile\|status\|explain\|sparql` |
| Formal context, code graph, Arrow nodes, hypergraph | `export` |
| Snapshot: profile, facets, packages, session | `status` / `capabilities` |

Start an agent task with `leio_code_context(task, repo_root)`: it returns ranked
files, provider/binary identity, index coverage limitations and exact-file next
calls. Use `status` for health, `capabilities` for supported kinds, `find` for a
name, `graph` for topology, and `doctor` for drift. A known exact file can go
directly to graph; repeating the startup sequence before each edit is unnecessary.

Agent routing: [skills/leio-code/SKILL.md](../skills/leio-code/SKILL.md).
Positioning: [docs/POSITIONING.md](../docs/POSITIONING.md).

## MCP

For local development, install only the `leio-code` stdio connection. The
`leio_code` HTTP Apps SDK is a separate integration surface, not an additional
local prerequisite.

stdio MCP is the host surface (Claude, Cursor, Grok, Codex). Eighteen tools
including `leio_code_nav`. Both stdio and the Apps SDK HTTP server follow
**MCP 2026-07-28** dual-era with **2025-11-25** handshake
([spec](https://modelcontextprotocol.io/specification/2026-07-28),
[LEIO mapping](../docs/MCP-SPEC-2025-11-25.md)): `server/discover`, `resultType`,
`initialize.protocolVersion` (legacy hosts),
`serverInfo` (`title` / `version` / `description` / `websiteUrl` / `icons`),
`instructions`, `capabilities.tools`, tool `title` + `annotations` +
`outputSchema`, `execution.taskSupport: "forbidden"`, structuredContent +
text dual-write, and `isError: true` for tool execution failures.

The wrapper resolves the binary as
`LEIO_CODE_BIN` → `~/.cargo/bin` / PATH (skipping `target/{release,debug}`)
→ walk-up `target/` last.

Project checkout (cwd is this repo):

```json
{
  "mcpServers": {
    "leio-code": {
      "command": "bash",
      "args": ["./scripts/launch-stdio-mcp.sh"],
      "cwd": ".",
      "env": { "LEIO_CODE_BIN": "/Users/you/.cargo/bin/leio-code" }
    }
  }
}
```

Claude / Grok plugin install — the host cwd is usually **not** this repo. `.claude-plugin/plugin.json` launches:

```json
{
  "leio-code": {
    "command": "bash",
    "args": ["${CLAUDE_PLUGIN_ROOT}/scripts/launch-stdio-mcp.sh"]
  }
}
```

A cwd-relative `./scripts/launch-stdio-mcp.sh` from a plugin host fails the handshake (broken pipe). Do not run the hosted HTTP Apps SDK (`leio_code` on `:8181`) and this stdio server in the same session.

Or install this checkout as a Claude Code marketplace:

```bash
claude plugin marketplace add /absolute/path/to/leio-code
claude plugin install leio-code@leio-code
```

HTTP / ChatGPT Apps: [apps-sdk/README.md](../apps-sdk/README.md).
Codex local stdio: [installation guide](../docs/install-stdio.md). `codex/connect.sh` is the optional HTTP bootstrap. Gemini: [GEMINI.md](../GEMINI.md).

## What stays on disk

Everything under `<repo>/.leio-code/`:

| Path | Role |
| --- | --- |
| `index.json` | Symbol / env / Redis / route index |
| `search.duckdb` | Identifier-aware FTS sidecar |
| `exports/arrow-nodes-v1/nodes.arrow` | Local FCA / adaptive search (Arrow IPC) |
| `exports/arrow-nodes-v1/nodes.search` | Packed mmap sidecar (norms + vectors) |
| `exports/code-graph-v1/` | N-Quads + query cache |
| `exports/formal-context-v1/lattice.json` | Concept lattice + heading functor (v3) |
| `exports/formal-context-v1/induced.ttl` | OWL TBox (`rdfs:subClassOf` = cover) |
| `exports/knowledge-v1/formal.nq` | Formal graph (repo RDF + OWL + wiki cites) |
| `exports/knowledge-v1/formal.json` | Load stats for that graph |
| `nav-session.json` | Shared `nav` cursor (no `LEIO_SESSION`) |
| `sessions/nav-<id>.json` | Per-agent nav cursor |
| `events/events.ndjson` | JSON-LD PROV journal (full path + checkout) |

**No network by default.** Outbound traffic only if you set it:

- `LEIO_CODE_EMBED_URL` / `EMBEDDING_API_URL` — query embeddings (OpenAI-compatible, e.g. TEI). Inspected-repo `[embed] url` is **not** used on search.
- Apps SDK `repo_url` clone (host allowlist) and the env-gated Vigoros bridge.

Secrets in `explain` are redacted unless `--show-secrets` on a TTY.

## Knowledge wiki

Markdown in the repo compiles to a local Arrow wiki — same idea as
`nodes.search`, fully offline.

```bash
leio-code knowledge compile --repo .
leio-code knowledge status --repo .          # wiki + meta.formal
leio-code knowledge explain "hours Alpha" --repo . --json
leio-code knowledge sparql 'SELECT ?s ?h WHERE { ?s :hoursWeekday ?h }' --repo .
leio-code knowledge adaptive "knowledge wiki" --repo .
leio-code export formal-context --repo .
leio-code nav goto "LEIO Code > Knowledge wiki" --repo .
leio-code nav explain --repo .     # same SPARQL proof, pinned to current_iri
leio-code nav related --repo .     # heading parent/siblings + lattice family
leio-code nav align --repo .       # identity + wiki_heading_to_lattice
```

Store: `.leio-code/exports/knowledge-v1/articles.search` (+ `articles.arrow`).
First `text` / `adaptive` call compiles if the store is missing or stale
(source count + max mtime in the sidecar header). Markdown is split into
heading sections (`README.md#Knowledge wiki` is its own hit). Unchanged
files are reused from the previous sidecar. The sidecar carries a packed
inverted index: BM25 over posting candidates, phrase/title boosts, at
most two hits per file, then optional BGE-M3 cosine when
`LEIO_CODE_EMBED_URL` is set. Hits return a match-centered snippet plus
`heading_path` / `line`.

`compile` also writes `formal.nq`. `explain` binds the needle through SPARQL
(or returns `grounded: false`). Adaptive/text stay lexical. `status` reports
wiki counts plus formal triples, prefixes, and cache freshness.

## Concurrent agents

Several CLIs and MCP hosts may hit the same `repo_root` at once. Index, wiki,
`formal.nq`, and lattice rebuilds take `.leio-code/*.lock` and write with
temp+rename. Nav is **not** shared: set `--session` or `LEIO_SESSION` (also
inherited from `CLAUDE_SESSION_ID` / `GROK_SESSION_ID` / …) so each agent
gets `.leio-code/sessions/nav-<id>.json`.

In a monorepo, pin `--repo` to the package you are editing. `status` lists
Cargo/pnpm/npm `workspace_members`. `LEIO_MAX_INDEX_FILES` (default 80000)
caps a whole-tree walk.

`LEIO_INDEX_TTL_SECS` (default 300) is the freshness window before a lookup
reindexes; set `0` to always rebuild. On-demand concept-lattice induction is
bounded by `LEIO_LATTICE_MAX_ON_DEMAND_OBJECTS` (default 5000) and
`LEIO_LATTICE_MAX_ON_DEMAND_PAIRS` (default 100000); oversized contexts
report the explicit build command instead of hanging.

## Provenance events

Repository-analysis commands append one JSON-LD line to
`.leio-code/events/events.ndjson`. The `conversation` command is the deliberate
exception: it writes no LEIO event journal because its selected transcripts are
private source evidence. Repository events include:
absolute `fullPath`, `file://` `@id`, W3C PROV `used` / `wasGeneratedBy`,
and a `checkout` object (`repoId`, origin, worktree, branch, HEAD, git
common dir) so multi-repo / multi-worktree / multi-branch journals merge
without colliding. `--format jsonld` is the same document on stdout.
`LEIO_EVENTS_DIR` collects `{repoId}/{worktreeHash}-{branch}.ndjson`.
Disable with `LEIO_DISABLE_EVENTS=1`.

## Retrieval

Adaptive / multi-word `find` / `context` prefer the local Arrow export:

1. mmap `nodes.search` when the layout is valid (magic, version, alignment, no overlap)
2. else decode the Arrow IPC stream
3. optional cosine against BGE-M3 (1024-d) only when an **environment** embed URL is set
4. lexical fallback

Compound path ranking keeps stop words in adjacent pairs (`LEIO Code` →
`leio-code`).

Numbers: [docs/BENCHMARKS.md](../docs/BENCHMARKS.md). Context ranking:
[docs/CONTEXT_BUNDLE.md](../docs/CONTEXT_BUNDLE.md).

## Profiles

`generic` is enough for an arbitrary repo. Facets such as deploy targets,
cartridges, and large doctor suites are opt-in via `.leio-code/config.toml`.

This repository sets `workspace_profile = "leio-code"` and runs
`self-contract`, `slop`, `import-boundary`, `repo-hygiene`,
`codex-orchestration`, and `leio-release-coherence`. Ask `capabilities`
for the live list — do not hard-code doctor counts.

## Develop

```bash
cargo test --lib
node --test mcp/resolve-binary.test.js mcp/export-paths.test.js mcp/watch-state.test.js
leio-code doctor self-contract --repo .
```

`make verify` runs the focused crate + JS + start-local checks.
`make package-plugin` builds the Codex/plugin tarball via
`scripts/package_codex_plugin.py`.

## Harness

`leio-harness` lives in this repo (`crates/leio-harness`). It indexes
worktrees in-process through the `leio-code` library, then runs agent lanes
(leases, worktrees, Arrow Flight bus, GEPA). Lane branches integrate onto an
`integration/<id>` branch where the objective runs on the *integrated* tree —
never on isolated lanes — and an improvement gate fast-forwards into the
target only when the integrated outcome beats the baseline (fail-closed on
conflict, regression, or collapse). Design:
[docs/harness/AGENT-FABRIC.md](../docs/harness/AGENT-FABRIC.md).

```bash
leio-harness bus serve --bind 127.0.0.1:8815
leio-harness integrate --repo . --worktree-root wt --target main --branch agents/a --objective-arg ./objective.sh
leio-harness gate --repo . --worktree-root wt --target main --baseline main --branch agents/a --objective-arg ./objective.sh --output-dir runs --promote
leio-harness day --spec day.json
```


## Docs

| Doc | What |
| --- | --- |
| [docs/POSITIONING.md](../docs/POSITIONING.md) | Why this exists |
| [skills/leio-code/SKILL.md](../skills/leio-code/SKILL.md) | Agent first-pass and tool routing |
| [docs/AGENT-ROUTING.md](../docs/AGENT-ROUTING.md) | Pointer to the skill |
| [docs/CONTEXT_BUNDLE.md](../docs/CONTEXT_BUNDLE.md) | How `context` ranks files |
| [docs/BENCHMARKS.md](../docs/BENCHMARKS.md) | Retrieval latency |
| [docs/output-schema.md](../docs/output-schema.md) | JSON / JSON-LD / `--where` |
| [docs/RELEASE-CHECKLIST.md](../docs/RELEASE-CHECKLIST.md) | Cut a release |
| [apps-sdk/README.md](../apps-sdk/README.md) | HTTP MCP / ChatGPT Apps |
