# LEIO-Harness Agent Fabric

Goal: orchestrate *different agents* — Codex, Gemini, Claude (Claude Code),
z.ai, DeepSeek, OpenRouter, Kimi, Grok — across different branches, worktrees,
and even repositories, with **active machine-to-machine communication that
humans never need to read**, while the human spends the day in a single
terminal.

## Layers

1. **LEIO Code global code view (mandatory per agent).** Every agent lane must
   run `leio-code index` on its repo/worktree before starting and keep it fresh
   (`status` / `index` heartbeats). Cross-file discovery goes through
   `leio-code find/graph` (callers, callees, resolved imports) and FCA exports
   (`export formal-context`) — never ad-hoc grep. Each lane publishes its index
   hash + facet summary to the bus topic `code-view/<repo>` so the orchestrator
   can detect stale or divergent code views across agents.

2. **Semantic bus (Arrow Flight `do_exchange`).** Agents do not send text to
   each other. They publish embedding vectors of their state/intent
   (`leio-code/crates/leio-harness/src/bus.rs`):
   - Inbound `FlightData` with embedding `RecordBatch` (schema
     `embedding_schema()`) → appended, ack via `app_metadata`.
   - `{"type":"match","vector":[...],"topic":"...","top_k":k}` in
     `app_metadata` → cosine top-k hits (agent, run, topic, score).
   - Vectors travel as raw little-endian `f32` bytes inside Arrow buffers:
     zero-copy writes (`Buffer::from_vec` moves the allocation), zero-copy
     reads (mmap for IPC files, Arrow buffers for Flight frames).

   Topics are namespaced: `code-view/<repo>`, `intent/<task>`,
   `conflict/<branch>`, `merge-ready/<lane>`.

3. **Execution runtime (Rust).** `run` (process-group spawn, wall/idle
   timeouts, SIGTERM→SIGKILL, bounded logs, Arrow IPC event stream),
   `worktree create/retire` (fail-closed dirty checks, branch-safe),
   `lease` (atomic JSON registry, interprocess file lock, heartbeat/expiry).

4. **Agent adapters (CCR).** Model traffic goes through the router's protocol
   capabilities (`openai_responses`, `openai_chat_completions`,
   `anthropic_messages`, `gemini_generate_content`); interactive CLIs through
   profile launch plans. Each adapter maps: goal → agent invocation; agent
   state → embedding (via the configured embedding model); bus hits → agent
   context. Agents never see each other's raw text — only routed vectors
   translated by the orchestrator.

5. **Single terminal.** One `ccr harness day <goal>` command: decompose into
   lanes (repo × branch × worktree), acquire leases, start agents, stream a
   compact progress view (lease state, bus activity counters, build/deploy
   gates). The human sees decisions and diffs, never agent chatter.

## Current status

- [x] Arrow Flight `do_exchange` semantic bus with cosine match (tested:
      codex/kimi/grok publish + match round-trip).
- [x] Zero-copy Arrow IPC event capture for process runs.
- [x] Atomic lease store and safe worktree executor.
- [x] `BusClient` (publish / match over `do_exchange`) and CLI:
      `bus serve|publish|match|selftest`.
- [x] Embedding adapter (`embed.rs`): OpenAI-compatible `/embeddings` with
      timeouts, retries, dimension checks (`LEIO_HARNESS_EMBED_*`).
- [x] `leio-code` is the same workspace; `codeview publish` indexes
      **in-process** and publishes to `code-view/<repo>` on the bus.
- [x] GEPA vector evolution (`gepa.rs`, dependency-free): `slerp`,
      trust-region mutation, `ParetoFrontier`, semantic anchor, deterministic
      SplitMix64 RNG. CLI: `gepa merge|mutate|frontier|anchor`.
- [x] `leio-harness day` orchestrator: lanes with worktrees, atomic leases,
      supervised runs (serial or `--parallel`), result vectors published to
      `result/<lane>` on the bus.
- [x] Merge-readiness arbitration: `{"type":"merge_gate","lane":...,
      "goal_vector":...,"threshold":...}` bus op — GEPA anchor penalty against
      the goal embedding over the lane's latest intent/result state.
- [x] Agent adapters (`agents.rs`): declarative argv templates for
      codex/claude/gemini CLIs plus custom templates (`{task}`/`{model}`
      placeholders); no permission-bypass flags injected.
- [x] LEIO staleness gate: `codeview check` (leio-code's own stat-equality
      freshness) and `requireFreshCodeView` day gate.
- [x] TUI progress view for `day` (`--watch`): hand-rolled ANSI renderer,
      deterministic pure-frame core, per-lane live status glyphs.
- [x] Semantic lane state: task+status+stdout-tail embedded via the
      configured embeddings endpoint into `intent/<agent>` and
      `result/<agent>` (one-hot fallback without a model), feeding the
      `merge_gate` with real semantics.
- [x] SHA-256 day artifact manifest (`manifest.json` per run: logs, Arrow
      event streams, results — hash + byte size per entry).
- [x] Durable bus: `bus serve --persist bus.arrow` snapshots embedding rows
      atomically in Arrow IPC, reloaded on restart (seq preserved).
- [x] Real embeddings validated end-to-end: LiteLLM `bge-m3` (1024d) on the
      lab GPU infra (`llm.tail21cbc4.ts.net:4000`), configured via
      `~/.config/leio-harness/env` (env vars override).
- [x] Improvement gate (`improvement.rs`): a merge only proceeds when the
      collaborative outcome provably beats a single-agent baseline — per-metric
      direction (maximize/minimize), SIGReg collapse veto, SHA-256 evidence,
      and a non-zero exit on no-improvement/regression/collapse.
- [x] GEPA×SIGReg evolution loop: `gepa cycle` runs trust-region generations
      with the SIGReg isotropy score steering exploration (widen strength on
      collapse) and a bi-objective Pareto frontier (goal alignment ×
      population diversity), with a per-generation trajectory.
- [x] SIGReg isotropy detector (`sigreg.rs`): sliced Epps-Pulley score
      over the bus embedding population (LeJEPA, arXiv:2511.08544), wired into
      `gepa cycle` to flag directional collapse. `sigreg isotropy` CLI.
- [x] `gepa cycle` CLI: pull bus candidates, run trust-region evolution,
      maintain a Pareto frontier (reward = 1 - anchor penalty vs goal), and
      republish offspring. Demo: goal=parent text keeps penalty ~0.004–0.012.: `{"type":"evolve","lane":...,
      "goal_vector":...,"strength":...,"max_angle_deg":...,"seed":...}`
      mutates the lane's latest intent within the trust region, checks
      semantic-anchor drift, and publishes the offspring.
- [x] Integration phase (`integrate.rs`): stack lane branches into an
      `integration/<id>` branch and run the objective on the *integrated* tree —
      never on isolated lanes. Fail-closed: a conflict aborts the whole
      integration and cleans up the throwaway worktree/branch.
- [x] Improvement gate (`gate.rs`): capture a baseline snapshot on
      `baseline_ref`, integrate the lanes, evaluate `evaluate_improvement`, and
      fast-forward `integration/<id>` into `target_ref` **only** when the
      verdict is `Improved` (and `--promote` is set). Non-improvement,
      regression, collapse, and conflict all leave the target untouched.

## Commands

```bash
leio-harness bus serve --bind 127.0.0.1:8815     # semantic bus daemon
leio-harness bus selftest                        # publish + match round-trip
leio-harness run --spec run.json                 # supervised process run
leio-harness agent stdio --spec agent-session.json  # supervised ACP proxy (e.g. grok agent stdio)
leio-harness lease --store leases.json acquire --lease lease.json
leio-harness worktree create --repo . --root wt --path wt/a --branch agents/a
leio-harness arrow inspect events.arrow          # mmap zero-copy IPC inspect
leio-harness integrate --repo . --worktree-root wt --target main --branch agents/a --objective-arg ./objective.sh
leio-harness gate --repo . --worktree-root wt --target main --baseline main --branch agents/a --objective-arg ./objective.sh --output-dir runs --promote
```

The `gate` objective contract: the command prints one `OutcomeSnapshot` JSON
object to stdout — `{"metrics":{...},"minimize":[...],"isotropy":...}` — and the
harness parses it (tolerating surrounding log lines). It is run once on the
baseline ref and once on the integrated tree; promotion happens only when the
collaborative result is strictly better on at least one metric and no metric
regresses (SIGReg collapse veto included).

## Opt-in OpenRouter model routing

Built-in templates (`codex`, `claude`, `gemini`, `kimi`, `grok`) do **not**
pass `{model}`. The CLI keeps its own default. OpenRouter
[rankings](https://openrouter.ai/rankings) measure token adoption, not quality
— do not auto-pick the weekly #1 for every lane.

To pin a permaslug, add a custom template that contains `{model}` and set
either `model` or `workShape` on the lane. Example:
[`day-openrouter.example.json`](./day-openrouter.example.json).

| workShape | aliases | OpenRouter slug | Why |
| --- | --- | --- | --- |
| `explorer` | `terra-low` | `deepseek/deepseek-v4-flash-0731` | volume / cheap discovery |
| `worker` | `terra-medium` | `deepseek/deepseek-v4-flash-0731` | volume implementation |
| `verifier` | `luna-medium` | `openai/gpt-5.6-luna` | latency-sensitive checks |
| `reviewer` | `sol-high` | `anthropic/claude-opus-5` | quality review |
| `security` | `sol-xhigh` | `anthropic/claude-opus-5` | auth / egress / secrets |

Coordinator (`grok`, no `workShape`) stays on the Grok CLI default.
`sol-medium` is rejected so it cannot silently inherit Flash. An explicit
`model` always wins over `workShape`. The CLI must already resolve the slug
(CCR / OpenRouter base URL); the harness only substitutes the string.

Source: OpenRouter (openrouter.ai/rankings), as of usage through 2026-08-17.
Licensed under CC BY 4.0.
