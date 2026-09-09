---
description: Model-swept LEIO golden-task benchmark via leio-harness day (dry-run by default, --execute spends model tokens)
argument-hint: "[--execute] [--models slug1 slug2 ...] [--agent codex-model|claude-model] [--repo path]"
---

# LEIO bench — model sweep over golden tasks

Run the model-sweep benchmark exactly as the `leio-bench` skill prescribes
(`skills/leio-bench/SKILL.md`). The user's arguments, if any:
$ARGUMENTS

Procedure:

1. Default model set when the user named none:
   `deepseek/deepseek-v4-flash-0731 openai/gpt-5.6-luna anthropic/claude-opus-5`
2. The leio-code repo **must be clean** — the harness refuses worktree creation
   on a dirty tree. Check `git status`; commit/stash or stop and say why.
3. Dry-run first (no `--execute`): `jai-bench $ARGUMENTS` (or
   `python3 scripts/benchmark_models.py $ARGUMENTS` outside the workbench).
   Show the lane count and the generated `day-spec.json` summary.
4. Execute only if the user asked for it (`--execute` in $ARGUMENTS, or an
   explicit "spend it" / "run it for real"). Announce the model list and lane
   count before running.
5. After the day completes, read `benchmarks/model-sweep/report.md` and report
   the per-model table: planned/graded/missing, json_valid, mean evidence
   ratio, median duration. Name the best model by evidence ratio among
   json_valid lanes.
6. Surface the JAI Team `widget` block from the runner output so the user can
   one-click re-run.

Never edit the golden fixture to make scores look better. Never retry failed
lanes silently — report them.
