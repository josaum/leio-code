# Install LEIO Code with one agent prompt

Paste this into a local coding agent with terminal access (for example, Codex
or Claude Code). A browser-only chat cannot install software on your computer.

> Install the official LEIO Code local stdio MCP from
> https://github.com/josaum/leio-code by following docs/install-stdio.md.
> Inspect the installation script, check prerequisites, build the locked source,
> and verify the actual MCP handshake, 18 tools, context and session navigation.
> Register only the leio-code stdio server in this host, preserving other MCP
> entries. Use absolute paths and the installer output. Do not enable the HTTP
> integration. Tell me whether a host reconnect is required and report the
> installed revision and verification results.

## Agent installation procedure

1. Check Git, a C/C++ build toolchain, current stable Rust (`cargo` via rustup),
   and Node.js 22+ with npm. macOS and Linux are the source-install targets;
   other platforms are not covered by this procedure. Install missing tools
   only under the host's normal authorization policy. No Rust toolchain is
   silently downloaded by this installer.
2. Clone `https://github.com/josaum/leio-code.git` into a persistent user-owned
   directory, such as `~/.local/share/leio-code/source`. Record the resolved
   commit. If that directory already exists, verify its origin and cleanliness;
   preserve local work and never reset it. An intentional update uses a clean
   checkout and a fast-forward-only pull.
3. Read `scripts/install-stdio.sh` and `scripts/verify-stdio.mjs`. Run
   `bash scripts/install-stdio.sh` from the checkout. It builds from Cargo.lock,
   installs the CLI under `~/.local/share/leio-code/bin`, installs locked Node
   dependencies without lifecycle scripts, and verifies the MCP in a disposable
   synthetic repository. It prints the host configuration as JSON on stdout.
   Builds require network access to public dependency registries and can take
   several minutes. Rust does not need to be compiled on every MCP start.
4. Register the printed **absolute** Node executable, MCP entrypoint and
   `LEIO_CODE_BIN` environment variable in the current host. Use the host's
   supported configuration mechanism, first inspecting an existing entry named
   `leio-code`. Preserve other servers and avoid duplicate registrations. The
   installer does not modify host configuration. Codex uses `mcp_servers` in
   TOML; Claude-compatible JSON uses the printed `mcpServers` shape.
5. Reconnect the host's MCP session when required. Then call
   `leio_code_context(task, repo_root)` against the user's chosen absolute
   repository and inspect provider identity and limitations. Every navigation
   call uses that root and an explicit session. Use `kind`, not `action`:
   inventory the exact file with `graph symbols-in`, copy a returned stable
   symbol URN into `goto`, then use `callees` or `neighbors` → inspect candidates
   → `select(index)` → `here`. A file containing multiple definitions may not
   uniquely identify a navigation target.
   Confirm returned evidence before claiming the host connection works.

The installation keeps its source checkout because the MCP wrapper and Node
dependencies live there. Re-running the script rebuilds the CLI in the same
prefix. To uninstall, remove only this host's `leio-code` registration, then
remove the dedicated installation directory after preserving any local edits.

## License

LEIO Code is available under **MIT OR Apache-2.0**, at your option. See
[LICENSE](../LICENSE), [LICENSE-MIT](../LICENSE-MIT),
[LICENSE-APACHE](../LICENSE-APACHE), and [third-party notices](../THIRD_PARTY.md).
