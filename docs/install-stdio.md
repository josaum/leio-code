# Install LEIO Code

Download the wheel for your platform from the [GitHub release](https://github.com/josaum/leio-code/releases/tag/leio-code-plugin-v2.7.0), or install the npm tarball directly.

```bash
npm install -g https://github.com/josaum/leio-code/releases/download/leio-code-plugin-v2.7.0/leio-code-2.7.0.tgz
```

The Python wheel and the Node package install `leio-code`, `leio-harness`, and
the stdio server `leio-mcp`. They register that server with harnesses already
on the machine. Any other MCP host launches `leio-mcp`.
Release binaries cover macOS arm64, macOS x64, Linux x64, and Windows x64.

## Windows x64 portable installation

Extract the full `leio-code-2.7.0-windows-amd64-portable.zip` into a user-writable folder.
It contains both LEIO executables, Node.js, MinGit, the MCP JavaScript dependencies,
and the LEIO skill. From PowerShell in that folder, run these commands.

```powershell
.\leio-code.cmd --version
.\leio-harness.cmd --version
.\leio-mcp.cmd --registration
```

`.\leio-mcp.cmd --registration` prints the absolute MCP command and arguments.
Run `.\leio-mcp.cmd --register` to register with auto-detected hosts for your
Windows user. Keep the extracted folder in place after registering. Core
repository queries and stdio start with the included files and Windows system
libraries. Optional model services and agent providers use their own credentials
and runtimes.

The steps below are for changing Leio itself. A browser-only chat cannot
install software on your computer.

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
   `bash scripts/install-stdio.sh` from the checkout. It builds `leio-code` and
   `leio-harness` from Cargo.lock, installs both under
   `~/.local/share/leio-code/bin`, installs locked Node dependencies without
   lifecycle scripts, and verifies the MCP in a disposable synthetic
   repository. It prints the host configuration as JSON on stdout. Builds
   require network access to public dependency registries and can take several
   minutes. Rust does not need to be compiled on every MCP start.
4. Register the printed **absolute** Node executable, MCP entrypoint,
   `LEIO_CODE_BIN`, and `LEIO_HARNESS_BIN` in the current host. Copy `command`,
   `args`, and `env` into that host's stdio server entry. The printed
   `mcpServers` object is the JSON shape used by hosts that store servers that
   way; a host with a different file copies the same three fields. Do not set
   `LEIO_CODE_REPO_ROOT`. Preserve other servers and avoid a second `leio-code`
   entry. The installer does not modify host configuration.
5. Reconnect the host's MCP session when required. Then call
   `leio_code_context(task, repo_root)` against the user's chosen absolute
   repository and inspect provider identity and limitations. A different
   codebase is a different absolute `repo_root`; the same server process can
   answer for each of them. Harness commands take `--repo` the same way.
   Every navigation call uses that root and an explicit session. Use `kind`, not `action`:
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
