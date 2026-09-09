---
description: Author and run a multi-model agent day via leio-harness (lanes, worktrees, leases, OpenRouter work-shape routing)
argument-hint: "<goal text> [--repo path] [--parallel] [--models slug1 slug2 ...] [--dry-run]"
---

# LEIO day — orchestrate a multi-model agent day

Goal from the user (required): $ARGUMENTS

Procedure:

1. **Scope check.** Parse the goal, the target `--repo` (default: the current
   repository), and any `--models`. The repo must be clean — the harness
   refuses worktree creation on a dirty source tree; commit/stash or stop and
   say why.
2. **Models.** Run `leio-harness models show`. If the source is `builtin` or
   older than a week, run `leio-harness models refresh` first (public API, no
   key). Pick per-lane models by work-shape from the resolved table unless the
   user named models explicitly.
3. **Author the day spec** following `skills/leio-day/SKILL.md`: lanes
   decompose the goal (explorer → worker → verifier is the minimum useful
   shape; add reviewer/security for risky changes), agent templates consume
   `{model}` for OpenRouter routing, `requireFreshCodeView: true`,
   `outputDir` under `/tmp/leio-runs/<short-goal>`.
4. **Dry-run first**: write the spec, print the lane table (agent, work-shape,
   model, task summary), and estimate cost. Execute only when the user asked
   for execution; otherwise hand back the spec path and the exact
   `leio-harness day --spec ...` command.
5. **Execute** with `--bus 127.0.0.1:18815` when the bus is up. After the day,
   read the report: per-lane status/duration, passed/failed, and the artifact
   manifest. Summarize outcomes and surface any failed lanes with their error
   strings — never silently retry lanes.

Never bypass the dirty-repo or freshness gates. Never inject permission-
bypass flags into agent templates.
