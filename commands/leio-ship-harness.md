---
description: Rebuild leio-code + leio-harness from this checkout, install globally, and pin the Hermes MCP (jai-ship harness)
argument-hint: "[--app] [--integrity]"
---

# LEIO ship — rebuild and pin the harness

Ship the LEIO stack from this checkout so every surface (CLI, harness bus on
:18815, Hermes profiles, plugin caches) runs the current build. Arguments, if
any: $ARGUMENTS

Procedure:

1. Confirm the tree is clean enough to build what you intend to ship
   (`git status`; untracked test artifacts are fine — committed-but-stale code
   is what you would be shipping).
2. Preferred path — the workbench installer (builds both crates, `make install`,
   installs `~/.grok/bin/leio-harness`, pins Hermes MCP via
   `~/.hermes/scripts/leio-integrity.py --fix`):

   ```bash
   bash ~/projects/leio-workbench/scripts/jai-ship harness
   ```

   Fallback without the workbench checkout:

   ```bash
   cargo build --release -p leio-code -p leio-harness && make install
   install -m 0755 target/release/leio-harness ~/.grok/bin/leio-harness
   ```

3. If the user passed `--app`: also run `jai-ship app` (swift test + install
   JAI Team.app). If `--integrity`: only `jai-ship integrity`.
4. Restart the bus so long-running surfaces pick up the new binary:

   ```bash
   launchctl kickstart -k gui/$(id -u)/ai.jquant.leio-bus
   ```

5. Verify and report:

   ```bash
   ~/.cargo/bin/leio-code --version && ~/.grok/bin/leio-harness --version
   lsof -nP -iTCP:18815 -sTCP:LISTEN
   ```

Report old → new versions per surface. If the bus did not come back on :18815,
say so plainly — do not claim the restart succeeded.
