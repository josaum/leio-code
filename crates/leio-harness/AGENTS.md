# leio-harness — agent contract

**Read this before editing.** Every Hermes session also injects `~/.hermes/SHIP.md`.

Crate: `~/projects/leio-code/crates/leio-harness`  
Binary: `leio-harness` (`bus`, `codeview`, `day`, worktrees).  
Hermes plugin (memory tools): `~/.hermes/plugins/leio-harness`.

This is the team runtime. Do not treat example-workspace as the source.

## Ship

```bash
jai-ship harness
```

That rebuilds `leio-code` + `leio-harness` release, installs `~/.cargo/bin`, copies `~/.grok/bin/leio-harness`, and runs `leio-integrity.py --fix` so every Hermes MCP has `LEIO_CODE_BIN`.

Tests before ship: `cargo test -p leio-harness` from `~/projects/leio-code`.

## Widget

````markdown
```widget
{"title":"Install LEIO harness","actions":[{"label":"Ship harness","argv":["jai-ship","harness"]}]}
```
````

## Don't

- Don't point MCP at a leftover checkout. This tree's stdio server is `mcp/`.
- Don't `cargo install` a stale debug binary over `~/.cargo/bin/leio-code`.
- Don't merge from a dirty main; worktrees `agents/<id>/<run>`.
