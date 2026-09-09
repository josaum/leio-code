# LEIO Code

Follow [skills/leio-code/SKILL.md](skills/leio-code/SKILL.md). That file is the only agent routing document.

CLI-only gaps: [docs/MCP-SURFACE-GAP.md](docs/MCP-SURFACE-GAP.md).
Wire contract: [docs/MCP-SPEC-2025-11-25.md](docs/MCP-SPEC-2025-11-25.md).

Call MCP `leio_code_*` immediately. A failed call is the only reason to use the CLI. Live kind/doctor counts come from `capabilities`.

Narrow prompt pack (task-shaped, not a second routing map):

- `prompts/deploy-debug.md`
- `prompts/drift-hunt.md`
- `prompts/graph-investigation.md`

Repository assumptions:

- repo root is the active repository under analysis
- `Cargo.toml` is the crate manifest
- honor `LEIO_CODE_BIN`; do not drive `target/debug/leio-code` when an install exists
