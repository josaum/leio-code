# Swarm — leio-harness inside the trio

Authoring a day is `skills/leio-day/SKILL.md`; the design is
`docs/harness/AGENT-FABRIC.md`; the live verbs are `leio-harness --help`.
This file adds the trio interlock: what a lane must carry in, what the runtime
hands back as receipts, and how every lane result moves through S-2PC.

## 1. Day spec fields (camelCase, from the runtime structs)

```text
goal, repo, worktreeRoot, outputDir           absolute paths
timeoutMs, parallel                           default serial
requireFreshCodeView                          true in a trio run
agentTemplates { name: { argv[], env{} } }    {task} and {model} placeholders
lanes[] { agentId, task, argv[]?, agent?, model?, workShape?, timeoutMs? }
integrate? { targetRef, objective? { argv[], outputDir, timeoutMs } }
```

Explicit `argv` wins over `agent`. Built-in templates (`codex exec {task}`,
`claude -p {task}`, `gemini -p {task}`, `kimi -p {task}`, `grok -p {task}`)
never consume `{model}`; a lane with `workShape` or `model` needs a custom
template such as `claude-model` / `codex-model` that does. Work shapes:
`explorer`, `worker` → volume pick; `verifier` → latency pick; `reviewer`,
`security` → quality pick (`leio-harness models show` prints the resolved
slugs and their source; `builtin` means a stale snapshot — refresh first).

Validate before running: `python3 scripts/validate_day_spec.py day.json`.

## 2. What the runtime hands back (cite these, never paraphrase status)

```text
DayReport      { goal, outcomes[], passed, failed, leaseStore, integration? }
LaneOutcome    { agentId, runId, branch, worktree, status, durationMs, error?, artifacts[] }
manifest.json  { version, goal, generatedAtMs, artifacts[{ path, producer, sha256, byteSize }] }   at <outputDir>/manifest.json
IntegrationReport { integrationBranch, integrationWorktree, targetRef, merged, conflict?, merges[], filesChanged[], objective?, cleanup }
ImprovementVerdict  Improved | NoImprovement | Regressed | Collapsed
```

A lane's status is `LaneOutcome.status`. A lane that printed "done" but has
`status != ok` is not done. A report that names lane outcomes the runtime
did not emit is fabricated status — the failure mode the harness doctor
family guards against.

## 3. Bus and embeddings

Topics: `code-view/<repo>`, `intent/<agent>`, `result/<agent>`,
`conflict/<branch>`, `merge-ready/<lane>`. Ops over `app_metadata`: `match`
(cosine top-k), `merge_gate` (GEPA anchor penalty of a lane's latest state
against the goal vector), `evolve`. Default bind `127.0.0.1:8815`; `--persist
bus.arrow` survives restarts. Embeddings come from
`~/.config/leio-harness/env` (`LEIO_HARNESS_EMBED_URL`, `_MODEL`, `_KEY`);
without them lanes fall back to one-hot vectors and the merge gate loses its
semantics — the day still runs, say so in the report.

## 4. Gates the harness enforces (do not bypass)

| Gate | Where | Fails when |
| --- | --- | --- |
| dirty tree | `worktree create`, `day` | source checkout has uncommitted changes (a worktree from HEAD would silently drop them) |
| fresh code view | `requireFreshCodeView` | any lane's `codeview check` is stale |
| merge readiness | bus `merge_gate` | anchor penalty above threshold |
| integration | `integrate` | any lane branch conflicts — whole integration aborts, throwaway worktree retired |
| improvement | `gate` / `compare` | verdict not `Improved` (no metric better, any metric worse, or SIGReg collapse) — target untouched |

Objective contract: the command prints one `OutcomeSnapshot` JSON object to
stdout — `{"metrics":{...},"minimize":[...],"isotropy":...}` — surrounding
log lines tolerated. It runs once on the baseline ref and once on the
integrated tree. Promotion (`--promote`) fast-forwards the target only on
`Improved`. Start from `assets/objective.template.sh` (tests passed ↑, LEIO
doctor warnings ↓; every metric deterministic for a tree — no timestamps,
network or model calls) and measure the baseline once before the day so the
report can show what "Improved" was measured against.

Side effects: take `scripts/trio_sideeffects.py snapshot <repo> --out state.json`
from the supervising session before the day; `diff --against state.json`
afterwards proves that only lane branches and worktrees appeared and the source
checkout stayed untouched (the harness must never stage, stash or reset it).

## 5. S-2PC — the epistemic lifecycle of every lane result

`Free → Tentative → Reflecting → Committed | Aborted`

| Transition | Who | Trio check that justifies it |
| --- | --- | --- |
| `Tentative` | the producing lane, on return | — (a claim, not a fact) |
| `Reflecting` | a verifier/critic acquiring the proposal | it is reading, not accepting |
| `Committed` | the critic, after all applicable checks pass | LEIO doctors + tests green on the integrated tree; Reference Provider `verify` = `verified`; improvement verdict `Improved` when a merge is proposed |
| `Aborted / INVARIANT_VIOLATION` | critic | new doctor warning, failing test, boundary crossed |
| `Aborted / REGRESSION_DETECTED` | critic or gate | verdict `Regressed` / `NoImprovement`, or a metric worse than baseline |
| `Aborted / HALLUCINATION_DETECTED` | critic | `reference_provider_verify` rejected, or a claim with no `query_id` / `file:line` |
| `Aborted / LOW_CONFIDENCE` | critic | evidence present but the lane's own uncertainty notes make it unusable |
| `Aborted / DIMENSION_MISMATCH` | runtime | vector or contract shape mismatch on the bus |
| `Aborted / TIMEOUT` | runtime | lane exceeded `timeoutMs` |

Nothing skips `Reflecting`; a `Committed` result is immutable — a change is a
new proposal. Proposal intents at the fabric level are `PROPOSE_FACT`,
`PROPOSE_TOOL_CALL`, `EMBEDDING_UPDATE`, `GOAL_MUTATION`, `MERGE_REQUEST`;
only a coordinator emits `MERGE_REQUEST`, and only the gate acts on it.

## 6. Tier A — in-session fan-out

Same vocabulary, lighter runtime: launch the lanes as parallel `Agent` calls
in one message. Each brief includes `LEIO_SESSION=<lane-id>`, the repo root,
the code-view fingerprint, the Reference Provider receipts it may cite and the five-part
return format from `lane-briefs.md`. Read-only shapes get read-only tools.
You are the critic: move each returned result through `Reflecting` yourself
and record the typed outcome in the report. There is no worktree, so Tier A
is for reads and at most one writing lane.

## 7. Limits and version honesty

- Keep at most four *active* lanes per repository. A serial day
  (`parallel: false`) has one active lane, so six serial lanes are fine; the
  validator errors only on a parallel day above the limit, because it is a
  design limit, not (yet) a runtime error.
- The installed binary may lag the checkout: shared-memory consensus
  subcommands and the UDS bus transport exist at source HEAD; confirm any verb
  with `leio-harness --help` before promising it, and rebuild with the
  documented ship path when you need it.
- `leio_code_orchestrate_*` task tools, barriers and federated graphs are a
  design document, not a live MCP surface.
- Secrets never enter specs, briefs, manifests or reports; provider URLs
  never enter workspace runtime source (an egress doctor bans it).
