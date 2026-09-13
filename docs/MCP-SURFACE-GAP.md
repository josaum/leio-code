# LEIO Code — MCP Surface Gap

Explicit map of **CLI capabilities that agents must shell out for** (or that
lack a 1:1 MCP tool). Cross-link from [skills/leio-code/SKILL.md](../skills/leio-code/SKILL.md).

Verified against `leio-code --help` and the stdio MCP registration in
`mcp/index.js` / Apps SDK tools in `apps-sdk/server.js` (2026-08-13).

When MCP is available, prefer it for the overlapping surface (warm index). Use
CLI only for the rows below, CI exit-code gates, or when MCP transport is down.

Wire-level contract (initialize, tools, `isError` vs protocol errors):
[MCP-SPEC-2025-11-25.md](MCP-SPEC-2025-11-25.md).

---

## stdio MCP tools (present)

| MCP tool | CLI analogue |
| --- | --- |
| `leio_code_guide` | *(no CLI — use the skill + `capabilities`)* |
| `leio_code_status` | `leio-code status` |
| `leio_code_capabilities` | `leio-code capabilities` |
| `leio_code_context` | `leio-code context` |
| `leio_code_conversation` | `leio-code conversation --source <file>` (local files only) |
| `leio_code_index` | `leio-code index` |
| `leio_code_find` | `leio-code find` |
| `leio_code_explain` | `leio-code explain` |
| `leio_code_doctor` | `leio-code doctor` |
| `leio_code_graph` | `leio-code graph` |
| `leio_code_export` | `leio-code export` (bundle / envelope-friendly paths) |
| `leio_code_audit` | `leio-code audit` / `audit --strict` (composite rollup) |
| `leio_code_kb_bootstrap` | knowledge bootstrap path (see MCP help) |
| `leio_code_knowledge` | `leio-code knowledge` — `compile` (wiki + `formal.nq`), `explain` (SPARQL-gated or refuse), `sparql`, `adaptive`/`text`/`status` (local Arrow store) |
| `leio_code_nav` | `leio-code nav` — heading/lattice walk + `align` + `explain` (same proof as `knowledge explain`) |
| `leio_code_init` | `leio-code init` |
| `leio_code_verify` | `leio-code verify` |
| `leio_code_watch` | `leio-code watch` start/stop/status (not a stream) |

The Apps SDK **remaps** this surface rather than copying it. The contract lives in
[`apps-sdk/tool-name-map.js`](../apps-sdk/tool-name-map.js) and is asserted against both committed
surfaces by [`apps-sdk/surface-parity.test.js`](../apps-sdk/surface-parity.test.js):

| stdio MCP tool | hosted Apps SDK tool |
| --- | --- |
| `leio_code_guide` | `guide_repository_tools` |
| `leio_code_capabilities` | `inspect_repository_capabilities` |
| `leio_code_status` | `inspect_repository_status` |
| `leio_code_context` | `prepare_repository_context` |
| `leio_code_find` | `search_repository` |
| `leio_code_explain` | `explain_repository` |
| `leio_code_doctor` | `audit_repository_contracts` |
| `leio_code_audit` | `audit_repository_rollup` |
| `leio_code_graph` | **`graph_repository`** (real graph — not find) |

Hosted-only tools (no stdio counterpart): `search_repository_memory`,
`select_repository_target`, `clear_repository_target`.

Local-only tools (no hosted counterpart — they need files, a cursor or artifacts on the
caller's machine): `leio_code_conversation`, `leio_code_export` (writes or streams
artifacts), `leio_code_index`, `leio_code_init`, `leio_code_kb`, `leio_code_knowledge`,
`leio_code_nav`, `leio_code_verify`, `leio_code_watch`.

Remapped tools keep the stdio parameter set and its enum values. The hosted surface drops
only `index_path` and `timeout_ms` (the hosting server owns the checkout, the index and the
execution budget) and adds `repo_url` / `git_ref`. Both the parameter contract and the
enum equality are asserted against a live `tools/list` by
[`apps-sdk/submission-contract.test.js`](../apps-sdk/submission-contract.test.js).

Domain tool `consult_carlos_motta_specialist` registers only when `LEIO_VIGOROS_MCP_URL` is set.

---

## CLI-only (shell out)

| CLI command | Why MCP is missing / incomplete | Agent action |
| --- | --- | --- |
| `leio-code audit` / `audit --strict` | **stdio MCP:** `leio_code_audit` (pass `strict=true`). **Apps SDK:** `audit_repository_rollup`. Do not confuse with `leio_code_doctor` / `audit_repository_contracts` (doctor-only). | Prefer MCP audit tools; shell still fine for CI `--out` files |
| `leio-code watch` | Long-running; not a streaming subscription. stdio MCP `leio_code_watch` is start/stop/status via pid file | MCP control, or shell |
| `export formal-context --format=arrow` / streamed `--format=json` | stdio MCP `leio_code_export` now accepts `format` / `out` / `object_kind`. Large Arrow streams still write a file | Prefer MCP; shell when piping |
| `doctor --format=sarif` / `--suggest` / `--explain <rule>` | Advanced doctor UX may be incomplete on MCP | Prefer CLI when you need SARIF, rule explain, or suggest diffs |
| `find` / `explain` with `--format=jsonld` / `--where` | JSON-LD filter loop is CLI-oriented | Shell when chaining `find → jq → explain --stdin` |
| Make / FCA induction (`leio-code-induct-formal-context`, wheel build) | Outside binary MCP surface | `make leio-code-…` |
| Reference-docs KB (`make reference-docs-query`) | Separate corpora, not LEIO Code MCP | Make targets — not a LEIO Code surface |

---

## Partial / naming caveats

| Topic | Note |
| --- | --- |
| Conversation files | Local stdio + CLI only. Hosted Apps SDK exposes guide topic `conversation` but has no local-file conversation tool. Reference's local CLI bridge can consume the typed packet. |
| Doctor vs audit | `leio_code_doctor` / Apps `audit_repository_contracts` = CLI `doctor`. Composite rollup = `leio_code_audit` / Apps `audit_repository_rollup` = CLI `audit [--strict]`. |
| Graph `dead-code` | Supported on CLI, stdio `leio_code_graph` (needle optional), Apps `graph_repository`, and `audit` extras. |
| Cross-language find | CLI `find binary|route|callers|subprocess-caller` may outpace MCP kind enums — check tool schema or shell out. |
| `leio_code_guide` | stdio MCP + Apps `guide_repository_tools` (shared `mcp/guide.js`). Routing home: [skills/leio-code/SKILL.md](../skills/leio-code/SKILL.md). |

## Agent follow-ups and health

Guide responses recommend tools for the requested topic through `next_tools`
and the action palette. For example, the graph guide starts with `leio_code_graph`
on stdio and `graph_repository` on the hosted surface. Recommendations use the
current profile's capabilities and names available on that transport. Local-only
conversation and knowledge topics have no hosted next tool; the guide explains
their local workflow. Recommendations are suggestions and do not execute calls.

On stdio, `leio_code_context`, `leio_code_graph`, and `leio_code_nav` return up to
four `structuredContent.next_calls`.
Each has `tool`, `arguments`, and `reason`; arguments include the same absolute
`repo_root` and optional `index_path` used for the query. Graph recommendations
reuse returned symbol URNs to avoid duplicate-name ambiguity. Navigation calls
preserve the cursor session and use returned result indices for selection.
Choose a relevant call and pass its arguments directly. The existing envelopes
remain complete. Use guide topic `navigation` for the graph-to-FCA workflow;
the hosted guide explains it but exposes no local cursor tool. Concurrent agents
pass a distinct `session` on every `leio_code_nav` call (CLI `--session`, with
`LEIO_SESSION` as the fallback). FCA relationships reflect indexed attributes,
not runtime coupling; use graph callsites and imports for structural evidence.

Navigation supports bounded sequential pages with `limit` (1–100) and `offset`.
Read `envelope.meta.result_page`; use only its returned `next_offset` with the
same query, index, cursor session and limit. Result indices are zero-based within
the displayed page. Concept rows expose readable `concept_details` and bounded
member/attribute samples; source rows include lines when known. Lattice readiness
is in `envelope.meta.lattice`, including explicit stale/unverified states and a
rebuild requirement. Readiness and page metadata are advertised in the stdio nav
output schema. Arrow fallback in nav uses deterministic lexical and FCA ranking
so encoder availability cannot reorder successive pages.

Quick status reports `doctor_summary.scope = "baseline"` with a status of
`passed`, `warnings`, `failed`, or `unavailable`, counts and an exit code.
`doctor_count` excludes skipped suites; `skipped_count` reports them separately. Failed
or unavailable execution sets `isError`; allowlisted warnings remain visible.
Quick status does not establish a full audit result. Doctor presets `baseline`,
`ci`, and `all` are advertised by the CLI catalog and accepted by MCP. For
targeted checks, use the profile's kinds from capabilities. Guide examples are
filtered for that profile while keeping core lookup and context guidance.

---

## Rule of thumb

1. Overlap exists → **MCP first** (warm index).
2. Row in the CLI-only table → **shell out** without apology.
3. Unsure → `leio_code_guide` or [skills/leio-code/SKILL.md](../skills/leio-code/SKILL.md).
