---
name: run-leio-code
description: Use when a repository task needs LEIO Code orientation, symbol search, call graphs, runtime topology, context bundles, or doctor checks before manual search.
---


# run-leio-code

Agent routing (when to use which subcommand / MCP vs CLI):
[`../../../skills/leio-code/SKILL.md`](../../../skills/leio-code/SKILL.md).
Prefer a fresh **release** binary (`~/.cargo/bin/leio-code` or workspace
`target/release/leio-code`); stale debug copies lie about doctors/kinds.

`leio-code` is the workspace's code-intelligence CLI (Rust, clap): it indexes a
repo and answers ownership/graph/contract questions via subcommands
(`status`, `find`, `explain`, `graph`, `doctor`, `audit`, `verify`,
`knowledge`, `context`, `watch`). It is driven from the shell — no GUI. The
agent path is the smoke driver, which builds and exercises the file-local
surface against a target repo.

**Paths below are relative to the `leio-code/` crate dir.** It lives inside the
`example-workspace` monorepo with a shared `CARGO_TARGET_DIR=./target`, so the
binary lands at the *workspace* `target/`, not under the crate.

## Prerequisites

- Rust toolchain (`cargo`) — builds with stable.
- `python3` — only for the driver's `--json` parse assertion.

No extra OS packages were needed to build/run the CLI on this machine.

## Build

```bash
cargo build --release   # ~2m cold, <1s warm; binary at <workspace-root>/target/release/leio-code
```

## Run (agent path) — the driver

```bash
# from the leio-code/ crate dir. Arg = repo to analyze (default: leio-code itself).
.Codex/skills/run-leio-code/smoke.sh .
```

It builds, then drives the CLI and asserts exit code + expected output for:
`status`, `verify`, `find symbol`, `graph callers-of`, `doctor repo-hygiene`,
`audit --format markdown`, and a `status --json` parse. Point a different repo at it by passing a path.

## Direct invocation (drive individual subcommands)

Uses `cargo run --release` so you always drive a fresh binary. Every line below
was run this session and exits 0:

```bash
cargo run --release -- status  --repo .                                   # snapshot: profile, facets, doctors
cargo run --release -- verify  --repo .                                   # self-contract: "ran N doctors, 0 warnings"
cargo run --release -- find  symbol encode_node_query_vectors --repo .    # "found 1 symbol matches"
cargo run --release -- graph callers-of visit_node_entities   --repo .    # "symbol ... has N callers"
cargo run --release -- doctor repo-hygiene --repo .                       # one doctor by name
cargo run --release -- audit  --repo . --format markdown                  # composite: "# Workspace Audit ..."
cargo run --release -- status --repo . --json 2>/dev/null                 # clean JSON on stdout (logs on stderr)
```

`knowledge` is fully local (no server) and is exercised by the smoke run
by the driver (file-local commands only).

## Gotchas (hit this session)

- **Stale binary is silent.** A leftover `target/release/leio-code` can be from
  an older checkout (shared `CARGO_TARGET_DIR`): a stale binary did not know the
  `repo-hygiene` doctor and suggested `prod-surface-hygiene` instead. Always
  rebuild (the driver uses `cargo run --release`); if invoking the binary
  directly, verify against recent code first.
- **Logs go to stderr, JSON to stdout.** Progress lines (`indexed 281 files…`,
  `search sidecar: …`) print to **stderr**. With `--json`, stdout is clean
  machine-readable — but only if you keep stderr out: `--json 2>/dev/null`.
  Mixing them yields `JSONDecodeError: Expecting value: line 1 column 1`.
- **Shared target dir.** The binary is at `<workspace-root>/target/release/`,
  not under the worktree/crate. Prefer `cargo run --release` over guessing the
  path.
- **First `graph` query is slower.** It exports the code-graph cache on first
  use (`code graph artifacts refreshed; generated fresh export`); later graph
  queries are fast.

## Troubleshooting

- `doctor <name>` suggests a *different* doctor (`a similar value exists: …`) →
  stale binary, rebuild (`cargo build --release`).
- `JSONDecodeError: Expecting value` parsing output → you piped stderr into the
  parser; add `2>/dev/null` and pass `--json`.
- `--repo` must point at a real directory; the index lands at
  `<repo>/.leio-code/index.json`.

## Test (sanity, not the main event)

```bash
cargo test            # full suite (lib + integration); discover current counts from the run
```
