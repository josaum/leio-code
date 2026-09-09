---
name: leio-bench
description: Model-swept LEIO benchmark — run the golden context tasks across multiple OpenRouter-routed models through leio-harness day, and grade which model uses LEIO evidence best. Triggers on "model sweep", "benchmark models", "jai-bench", "golden tasks sweep", "compare models on leio". Dry-run by default; --execute spends model tokens.
---

# LEIO Bench — model-swept golden tasks

Sweep N OpenRouter models over the golden context tasks (`benchmarks/context-golden-tasks.json`)
through `leio-harness day`: one lane per (task × model), each lane an agent CLI
invoked with the model slug, grounded in the LEIO code view, graded on evidence
use.

## When to use

- "Which model uses LEIO best?" / "benchmark the models" / "sweep the golden tasks"
- Before/after a retrieval change: same sweep re-measures whether model behavior
  tracks the context change.

## How to run

```bash
# 1. Dry-run (default): writes benchmarks/model-sweep/day-spec.json — no spend
jai-bench                                    # workbench checkout
python3 scripts/benchmark_models.py          # leio-code checkout

# 2. Inspect day-spec.json (lanes = tasks × models), then execute:
jai-bench --execute --models \
  deepseek/deepseek-v4-flash-0731 openai/gpt-5.6-luna anthropic/claude-opus-5
```

Alternatives: `--agent claude-model`, or `--argv` for a custom OpenRouter-routed
CLI template (`{model}`/`{task}` placeholders). `--bus 127.0.0.1:18815` publishes
lane results to the harness bus.

## Grading (per lane, from the lane's stdout artifact)

| Metric | Meaning |
| --- | --- |
| `json_valid` | lane emitted the required verdict object `{"answer", "evidence_paths", "tests"}` |
| `evidence_ratio` | verdict `evidence_paths` hit ratio vs the task's `expected_paths_any` / `expected_instruction_paths_any` / `expected_memory_paths_any` |
| `tests_hit` | task's `expected_tests_any` named in verdict `tests`/`answer` |
| `duration_ms` | harness-measured lane wall clock |

Output: `benchmarks/model-sweep/report.{json,md}` + a JAI Team `widget` block
(one-click re-run). Per-model summary reports planned/graded/missing lanes
separately — `missing` lanes never reported an outcome.

## Requirements and gates

- The **repo must be clean** — the harness refuses worktree creation on a dirty
  source tree (fail-closed). Commit or stash first.
- Agent CLIs (codex/claude or the custom template's binary) must be installed
  and **authenticated against OpenRouter-routed providers**. The harness injects
  no keys; `OPENROUTER_API_KEY` stays operator-side.
- Semantic-bus embeddings need `LEIO_HARNESS_EMBED_URL/MODEL` (+
  `~/.config/leio-harness/env`); without them lanes fall back to one-hot vectors
  (merge gates lose semantics; sweep still runs).
- Lane slugs default to the harness work-shape table (`agents.rs`): explorer/
  worker → volume model, verifier → verifier model, reviewer/security → quality
  model. Explicit `--models` overrides.

## Do not

- Do not run `--execute` without naming the models and checking the spec.
- Do not commit `benchmarks/model-sweep/` outputs (gitignored).
- Do not add `openrouter.ai` URLs to workspace runtime source — the
  `llm_provider_egress` doctor bans that; OpenRouter lives at the harness/CLI
  layer only.
