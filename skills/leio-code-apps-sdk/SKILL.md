---
name: leio-code-apps-sdk
description: This skill should be used when the host is "ChatGPT Developer Mode" or https://leio-code.getjai.com/mcp — "select_repository_target", "repo_url", "search_repository_memory", OAuth, DCR, or Apps SDK health. Do not use for local stdio MCP or leio_code_* tools (use leio-code).
---

# LEIO Code — Apps SDK (ChatGPT)

Never call `leio_code_*` on this host. Tools are `repository_*` / `inspect_*` / `prepare_*` / `audit_*`.

## Tool names (Apps SDK surface)

ChatGPT exposes **repository_* prefixed tools**, not `leio_code_*`:

| Apps SDK tool | Stdio MCP analogue |
| --- | --- |
| `guide_repository_tools` | `leio_code_guide` |
| `inspect_repository_capabilities` | `leio_code_capabilities` |
| `inspect_repository_status` | `leio_code_status` |
| `prepare_repository_context` | `leio_code_context` |
| `search_repository` | `leio_code_find` |
| `search_repository_memory` | (Apps semantic memory; not a stdio twin) |
| `explain_repository` | `leio_code_explain` |
| `graph_repository` | `leio_code_graph` |
| `audit_repository_contracts` | `leio_code_doctor` |
| `audit_repository_rollup` | `leio_code_audit` |
| `select_repository_target` | — (session only) |
| `clear_repository_target` | — (session only) |

Full routing: [../leio-code/SKILL.md](../leio-code/SKILL.md).
MCP 2025-11-25 contract: [docs/MCP-SPEC-2025-11-25.md](../../docs/MCP-SPEC-2025-11-25.md).

## Recommended ChatGPT workflow

1. `guide_repository_tools` — if unsure which tool to call.
2. `inspect_repository_status` — index health before anything else.
3. `inspect_repository_capabilities` — what this repo actually models.
4. `select_repository_target` — when analyzing a **different** public repo
   (`repo_url` + optional `git_ref`).
5. `prepare_repository_context` — before multi-file edits.
6. `search_repository` — entity lookup (symbol, env-var, api-route, …).
7. `graph_repository` — call/import topology (**not** search).
8. `audit_repository_rollup` with `strict: true` — pre-deploy gate.

## Health / OAuth (hosted)

- Issuer `https://auth.getjai.com/realms/leio-code`, audience `leio-code-apps-sdk`.
- DCR + `repo.read`. Health: Apps SDK `/health` (`legal.configured` when listing).
- Details: [apps-sdk/SUBMISSION.md](../../apps-sdk/SUBMISSION.md).

## Repository selection

- **Local loopback** (`http://127.0.0.1:8181/mcp`): omit `repo_url` to use
  this checkout. `repo_root` is accepted on 127.0.0.1 / localhost / ::1.
  Hosted non-loopback still rejects absolute server paths — use `repo_url`
  or `select_repository_target`.
- **Server default:** tools run against the deployment checkout when no
  `repo_url` is passed.
- **Public Git:** pass `repo_url` (HTTPS or `owner/repo` shorthand) on any
  tool, or call `select_repository_target` once per session.
- **Private GitHub:** connect GitHub in the widget, pick an installed repo,
  then `select_repository_target`.

## Local wiki / nav / lattice

Those surfaces are stdio MCP + CLI first (`leio_code_knowledge`,
`leio_code_nav`, `export formal-context` → `lattice.json` v3). Apps SDK
does not yet expose heading-functor nav. For wiki/lattice walks, use the
local plugin or shell `leio-code`.

## What Apps SDK does NOT expose

CLI-only or stdio-first surfaces (local wiki compile, nav/lattice walk,
`watch`, streamed FCA Arrow): see
[docs/MCP-SURFACE-GAP.md](../../docs/MCP-SURFACE-GAP.md).

## Submission / operator docs

- [apps-sdk/SUBMISSION.md](../../apps-sdk/SUBMISSION.md)
- [apps-sdk/chatgpt-app-submission.json](../../apps-sdk/chatgpt-app-submission.json)
