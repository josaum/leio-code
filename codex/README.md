# leio-code ↔ Codex integration

This directory is the canonical seam between **OpenAI Codex** (and any other coding agent that supports MCP-over-HTTP) and **leio-code**'s warm-index surface.

## What's here

| File | Purpose |
|---|---|
| [`AGENTS.md`](AGENTS.md) | Auto-discovered by Codex at session start. The canonical routing reference. |
| [`connect.sh`](connect.sh) | **Canonical** one-command bootstrap: wraps `apps-sdk/scripts/start-local.sh`, prints (or `--append`s) the Codex config snippet. |
| [`codex-config.example.toml`](codex-config.example.toml) | Sample `~/.codex/config.toml` blocks for all three integration paths (local / remote / stdio). |

## Quick start (90 seconds, local workspace)

```bash
# 1. Build leio-code (one-time, releases live in ~/.cargo or via Docker)
cd /Users/josaum/projects/example-workspace/leio-code
cargo build --release --bin leio-code

# 2. Start the apps-sdk MCP-over-HTTP server (idempotent)
bash codex/connect.sh --append

# That prints + appends a [mcp_servers.leio_code] block to your
# ~/.codex/config.toml.  Restart your Codex session and the leio-code
# tools appear in the tool list.
```

## Why this exists

`leio-code` already has three surfaces:

| Surface | Transport | When to use |
|---|---|---|
| CLI (`leio-code ...`) | subprocess + stdout JSON | universal fallback; cold start per call |
| stdio MCP (`leio-code/mcp/index.js`) | stdio | Claude Code; one warm process per Claude session |
| HTTP MCP (`leio-code/apps-sdk/server.js`) | HTTP / streamable HTTP | **Codex, ChatGPT Apps, remote agents, CI** |

Until this directory existed, Codex users were paying the CLI cold-start tax (5–15s per query against the `example-workspace` index) on every call. The HTTP MCP server has been ready the whole time — it just wasn't discoverable.

`AGENTS.md` is the bridge: Codex auto-loads it at session start the same way Claude Code loads `CLAUDE.md`, and from there the agent finds `connect.sh` and gets a warm endpoint in one command.

## State of integration

| Capability | Status |
|---|---|
| MCP tool registration (`leio_code_find`, `_explain`, `_graph`, `_doctor`, `_status`) | shipped (in `apps-sdk/server.js`) |
| HTTP transport with session resumption | shipped (`StreamableHTTPServerTransport`) |
| `/health` probe | shipped |
| Codex-discoverable docs | this PR |
| Idempotent local launcher | this PR |
| Auto-append to `~/.codex/config.toml` | this PR |
| `--watch` mode streamed over MCP | not yet — use CLI for active watch |
| Federated multi-repo MCP (one server, N indexes) | not yet — Q3 candidate |

## Beyond Codex

Anything that speaks MCP-over-HTTP can use the same setup:

- **ChatGPT Apps SDK** — the original target of `apps-sdk/`. The `prepare_repository_context` and widget tools are tuned for it.
- **Claude Code (remote)** — when Claude Code runs in a sandbox without direct stdio access to leio-code/mcp.
- **CI agents** — e.g. a GitHub Actions runner that runs leio-code audits against PR branches, with the apps-sdk server pre-warmed in a sidecar container.
- **Custom agents** — anything built on the OpenAI Responses API's `mcp` tool type, or the Anthropic API's remote MCP support, can point at this endpoint.

## See also

- [`leio-code/apps-sdk/README.md`](../apps-sdk/README.md) — apps-sdk-specific contract (auth, OAuth, widget surface)
- [`leio-code/mcp/index.js`](../mcp/index.js) — stdio MCP source; tool registrations mirror the apps-sdk
- [`leio-code/docs/output-schema.md`](../docs/output-schema.md) — every envelope and diagnostic shape
- workspace-root [`AGENTS.md`](../../AGENTS.md) — universal agent guidance (this directory points the Codex-specific path)
