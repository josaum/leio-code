---
name: leio-day
description: Orchestrate multi-model agent days — decompose a goal into lanes, run them through leio-harness (worktrees, leases, Arrow semantic bus), route each lane to the right OpenRouter model via work-shapes, and ground every lane in the LEIO code view plus Reference Provider governed knowledge. Triggers on "agent day", "run lanes", "multi-model", "orchestrate agents", "harness day", "which model should do this".
---

# LEIO Day — multi-model orchestration

The combined stack: **leio-code** is the shared code view (mandatory per lane),
**leio-harness** supervises agent lanes in isolated worktrees with leases and
an Arrow Flight semantic bus, and the **live OpenRouter model table** routes
each lane's work-shape to the right model. Reference Provider contributes governed
knowledge when the domain is business evidence.

## Choose the surface (30 seconds)

| You want | Use |
| --- | --- |
| Compare models on a task set | `jai-bench` / `/leio-bench` (the benchmark sweep) |
| Execute real work across lanes | a **day** (`leio-harness day --spec ...`) |
| Refresh which models the work-shapes resolve to | `leio-harness models refresh` |
| See the current model routing | `leio-harness models show` |
| Ship the whole stack after changes | `jai-ship harness` / `/leio-ship-harness` |

## Quick start

```bash
# 0. Models: refresh the live table (public API, no key, valid one week)
leio-harness models refresh && leio-harness models show

# 1. Author a day spec (JSON): goal, repo, lanes with work-shapes
#    Start from the example: docs/harness/day-openrouter.example.json

# 2. Run it (add --watch for the live lane TUI)
leio-harness day --spec day.json

# 3. Read the report (printed as JSON; artifacts + manifest in outputDir)
```

## Day spec anatomy

```json
{
  "goal": "...",
  "repo": "/abs/repo",
  "worktreeRoot": "/tmp/leio-wt",
  "outputDir": "/tmp/leio-runs",
  "timeoutMs": 600000,
  "parallel": false,
  "requireFreshCodeView": true,
  "agentTemplates": {
    "codex-model": {"argv": ["codex", "exec", "--model", "{model}", "{task}"]}
  },
  "lanes": [
    {"agentId": "explorer", "agent": "codex-model", "workShape": "explorer", "task": "..."}
  ]
}
```

- `workShape` routes `{model}` through the live table: explorer/worker →
  volume pick, verifier → latency pick, reviewer/security → quality pick.
  Explicit lane `model` overrides everything.
- Built-in templates (codex/claude/gemini/kimi/grok) do NOT consume `{model}`
  — they keep their CLI defaults. Use a `*-model` custom template or `--argv`
  when the lane must route through OpenRouter.
- Custom template env values expand `{task}`/`{model}` too (use them to pass
  lane context through the environment).
- OpenRouter-direct lanes (no agent CLI): the benchmark runner
  (`scripts/benchmark_models.py`, `--agent openrouter-direct`) ships a
  python-built chat-completions transport with `response_format` enforcement
  and retries — reuse it as the template pattern.

## Grounding mandates

1. **LEIO code view is mandatory per lane.** `requireFreshCodeView: true`
   fails the day when the view is stale; every lane publishes its fingerprint
   to `code-view/<repo>` on the bus. Cross-file discovery goes through
   `leio-code find/graph` — never grep.
2. **Reference Provider when the domain is business evidence.** Query
   `reference_provider_ask` (read-only, governed) and reflect claim statuses exactly;
   refusals are answers too — never invent numbers for unsigned metrics.
3. Agent lanes that cannot run tools need their grounding **embedded in the
   prompt** (see the openrouter-direct pattern in
   `scripts/benchmark_models.py`).

## Gates and failure modes

- **Dirty repo gate**: the harness refuses worktree creation on a dirty
  source tree — commit or stash first. This is by design; do not bypass.
- **Merge readiness**: lanes publish `result/<lane>` vectors; the merge gate
  (`gate`/`improvement`) only proceeds when the GEPA anchor penalty says the
  work moved toward the goal.
- **Model table staleness**: `models show` reports the source
  (`openrouter-api` vs `builtin`); a week-old cache silently reverts to the
  built-in snapshot — refresh before important days.
- **Cost**: lanes spend real model tokens. Name the models, check the lane
  count, and dry-run spec generation before executing.

## Workbench integration (JAI Team)

`scripts/jai-day --spec <file> [--watch]` wraps the day runner;
`jai-ship harness` rebuilds + installs the stack and re-pins Hermes MCP;
`jai-bench --execute` runs the model sweep. Emit a `widget` block in chat for
one-click runs.

### Default bus and failure behavior

`run` and `day` enable bus participation by default. The CLI uses `--bus`, then
`LEIO_HARNESS_BUS` from the environment or harness env file, then a managed,
durable private Unix socket. Use `--no-bus` only for an explicitly offline run.
Children receive the endpoint, agent ID and run ID. A lost result delivery makes
the run `infra_error`; inspect `bus-delivery.json` and the final report.
`leio-harness bus health --bus ENDPOINT` verifies protocol and runtime identity.
Shared-memory ABI v2 peers must not share segments with ABI v1 peers.
