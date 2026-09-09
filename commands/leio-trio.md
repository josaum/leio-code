---
description: Run a task through the LEIO trio protocol — LEIO Code ground truth, Reference Provider governed truth, leio-harness swarm — as one evidence chain
argument-hint: "<goal text> [--repo /abs/path ...] [--tier a|b] [--plan-only]"
---

# LEIO trio — ground, govern, decompose, execute, verify, emit

Goal from the user (required): $ARGUMENTS

Follow `skills/leio-trio/SKILL.md`. First sentence: name the **rung** (1
trivial / 2 scoped / 3 cross-repo or multi-lane) — rung 1 runs only Ground on
one repo and answers in ≤ 15 lines; the steps below are rung 2 and up.

1. **Preflight** — `python3 skills/leio-trio/scripts/trio_preflight.py <repo roots> --tier <a|b>`.
   Report blocking gates verbatim; stop on a blocker. Snapshot side effects:
   `scripts/trio_sideeffects.py snapshot <repo roots> --out <run>/state.json`.
2. **Ground** — per absolute repo root: `status → capabilities → context "<goal>"`.
   Keep the `query_id`s, the doctor baseline and the code-view fingerprint.
3. **Govern** — decide out loud whether a business fact, metric, forecast or
   decision is in scope. If yes, one `reference_provider_ask` per question; classify
   each fact signed / unsigned / unknown / no-signed-route; note
   `runtime.profile`.
4. **Decompose** — choose the tier from the table in the skill; write one
   five-part brief per lane from `references/lane-briefs.md`; at most four
   lanes per repository.
5. **Execute** — Tier A: parallel `Agent` calls with `LEIO_SESSION` per lane.
   Tier B: day spec from `assets/day-spec.template.json`, then
   `validate_day_spec.py`, `leio-harness models show`, dry-run, and only then
   `leio-harness day`. With `--plan-only`, stop after writing the spec and
   briefs and hand back the exact commands.
6. **Verify** — doctors + tests on the integrated tree; `reference_provider_verify`
   with `verify_claims` unchanged; improvement verdict must be `Improved`
   before any promotion. Then the scripted pass:
   `scripts/trio_receipts.py <report> --repo <roots>` (cited ids must resolve)
   and `scripts/trio_sideeffects.py diff --against <run>/state.json`.
7. **Emit** — fill `assets/trio-report.template.md`; every synthesis sentence
   captioned; drift → doctor / test / golden task / baseline per
   `references/retroaliment.md`; receipts index and side effects disclosed.

Never bypass the dirty-tree or fresh-code-view gates, never let a lane merge,
never summarise a lane's status from its prose, never treat demo-profile Reference Provider
values as production truth.
