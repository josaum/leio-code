---
name: leio-trio
description: Protocol for running LEIO Code (code ground truth), Reference Provider (governed business truth) and the leio-harness agent swarm (lanes, worktrees, semantic bus, S-2PC consensus, improvement gate) as ONE evidence chain. Use it whenever a task touches two or more of - code structure (especially across repos), business facts/metrics/forecasts/decisions, parallel or multi-model agents - even when the user names none of the tools. Triggers include "plan a swarm/day", "brief the lanes", "fan out agents", "cross-repo refactor", "ground this in LEIO and Reference Provider", "which numbers are signed", "verify this synthesis end to end", "pre-merge/pre-deploy evidence", "trio", "cognitive stack", "truth stack", "provenance chain", and any report that must carry LEIO query ids, Reference Provider receipts and harness manifests. Single-repo lookups go to leio-code, a lone business question to reference-provider-ask, model sweeps to leio-bench, and authoring one day spec to leio-day - this skill is the interlock that wraps them.
---

# LEIO Trio — three sovereignties, one evidence chain

Three surfaces, each sovereign over exactly one question, each emitting exactly
one kind of receipt. The interplay is not "use all three tools". It is a
discipline: **no surface may answer another surface's question, and every
handoff carries the previous receipt.** That is what turns a swarm's output
into an auditable chain instead of a pile of confident prose.

| Surface | Sovereign over | Receipt it emits | Must never |
| --- | --- | --- | --- |
| **LEIO Code** (`leio_code_*` / `leio-code`) | what the code *is*: symbols, callers, env, routes, drift, induced invariants | `query_id` + one JSON-LD PROV line in `.leio-code/events/events.ndjson` per call | assert a business fact; be reused across repo roots |
| **Reference Provider** (`reference_provider_ask/explain/verify`) | what the business *knows*, governed: signed vs unsigned claims, snapshots, policies | `res:` receipt, `snap:` snapshot, `cl:` claim ids, `verify` event id | execute, write, sign host prose, be treated as prod truth when `runtime.profile` says demo |
| **LEIO Harness swarm** (`leio-harness`) | what got *done*: lanes, worktrees, leases, bus vectors, integration, improvement verdict | `DayReport`, `LaneOutcome` per lane, `manifest.json` (SHA-256 per artifact), lane branches | merge or promote on its own; verify on isolated lanes; report a lane status the runtime did not produce |

Canonical documents own each surface. This skill adds only the interlock;
when it disagrees with one of them, they win and the disagreement is drift to
report:

- LEIO Code routing: `skills/leio-code/SKILL.md` (the only routing document).
- Reference Provider contract: skills `reference-provider-ask`, `reference-provider-explain`, `reference-provider-verify`.
- Swarm authoring: `skills/leio-day/SKILL.md`, design in `docs/harness/AGENT-FABRIC.md`.

## Depth first — say the rung out loud

Rigid on *what* (the surfaces never change roles), flexible on *depth*. The
first sentence of your work names the rung; the rung decides how much of the
loop runs. Over-running the loop on a trivial task is a failure too: it costs
the user tokens and buries the answer in ceremony.

| Rung | Looks like | Runs | Skips | Report |
| --- | --- | --- | --- | --- |
| **1 · trivial** | one file, no business fact, one edit or one lookup | Ground on one repo (`status` + the one `find`/`graph` that answers), the edit, its test | preflight script, Reference Provider, lanes, day spec, the template | ≤ 15 lines: what, `file:line`, `query_id`s, what else references it |
| **2 · scoped** | one repo, several files, or any governed number | preflight, full Ground, Govern decision (Reference Provider only if a fact is in scope), Tier A or none, Verify (doctors + tests + receipts script), report | Tier B, integration | template with the unused blocks marked "not applicable — none ran" |
| **3 · cross-repo / multi-lane** | several repos, several writers, multi-model, a merge gate | everything below | nothing | full template, three receipt blocks, drift → retroaliment |

## The loop

### 0. Preflight — code answers deterministic questions

```bash
python3 <skill>/scripts/trio_preflight.py /abs/repo [/abs/repo2 ...] --tier a|b
```

Binaries and versions, index freshness per repo, dirty trees, code-view
freshness (`leio-harness codeview check`), which verbs the *installed* harness
actually exposes, model-table source, embedding config presence (names only),
reference-provider MCP entry presence. Blocking gates exit non-zero; Tier B also
blocks on dirty trees and stale views. Do not let a model "check" these by
reading files — the answers are binary and the script is never wrong about
them. Reference Provider *reachability* is host-checked (one `reference_provider_ask`).

The user's constraints outrank the script: if they said "no harness
commands", run `--no-harness` (LEIO and git checks only) and report the
harness-backed gates as unchecked rather than quietly running `codeview
check` or `models show` anyway.

### 1. Ground — LEIO establishes the shared code view

Per **absolute** `repo_root`, in this order: `status` → `capabilities` →
`context "<task>"`, then at most one follow-up per open question (`find`,
`explain`, `graph`, `doctor`). Capture, because later phases cite them:

- the `query_id` of each call — the PROV anchor for that evidence;
- the doctor line from `status` (the baseline Verify compares against);
- the code-view fingerprint from `leio-harness codeview check --repo <abs>`.

Two repos means two loops; never one index against another tree. Every
concurrent agent on a shared tree gets its own `LEIO_SESSION`; a harness lane
in its own worktree already has private sidecars but starts *un-indexed*, so
its first action is `leio-code index --repo .`.

LEIO has known blind spots (route-id literals, module-level dicts, dict-keyed
dispatch — `references/ground-leio.md` §7). The rule is still LEIO first: when
`find` returns nothing for a literal, run `rg` for the literal *and cite both*
— the `query_id` that proved the miss and the `file:line` that found it.

**Why:** a swarm can only agree about code it sees identically. A stale or
divergent view does not look like staleness to the lanes — it looks like a
factual disagreement, and they will argue about it in your tokens.

### 2. Govern — Reference Provider decides what is a fact

Ask one question first, out loud: *does this task consume or produce a
business fact, metric, forecast, threshold or decision?* If no, write "no
governed facts in scope" and skip Reference Provider. If yes, one `reference_provider_ask` per
question, `view: "compact"`, and read from structured content:

`route`, `matched_route.match`, `governance`, `answer_summary.status`,
`evidence.primary[].as_of`, `verify_claims`, `receipt_id`, `snapshot_id`,
`runtime.profile`, `runtime.production_ready`.

Classify every fact you will carry forward as **signed**, **unsigned**,
**unknown**, or **no-signed-route** (= not governed, not prohibited — reason
on, labelled unsigned). Hold `receipt_id` + `snapshot_id` + `verify_claims`
unchanged; lane briefs and the report cite them, lanes never re-ask.

Runtime rule: if `runtime.profile` is not a production profile or
`production_ready` is false, every value is **demo evidence**. It can ground a
protocol exercise; it can never steer a real decision, and the Reference Provider block of
the report opens with that sentence.

**Why:** the model navigates governed knowledge; it does not invent it. A
signed route does not make the whole answer signed — inspect
`answer_summary.status` (compact) or `answer_claim_status` (audit) and each
claim. Reference Provider is read-only; nothing it returns authorises a write, a send, or a
promotion.

### 3. Decompose — work-shapes are the swarm's vocabulary

Pick the tier (table below), then lanes. The shapes are shared by harness
`workShape`, host role agents, and in-session subagents, so use them as names,
not decoration:

| Shape | Does | Writes tracked source? | Aborts a proposal on |
| --- | --- | --- | --- |
| `explorer` | maps with LEIO evidence, blast radius, ownership | no | — |
| `worker` | smallest change satisfying the explorer map + tests | yes (own worktree only) | — |
| `verifier` | focused tests, builds, doctors on the assigned diff | no | `INVARIANT_VIOLATION`, `REGRESSION_DETECTED` |
| `reviewer` | correctness, test gaps, provenance of claims | no | `HALLUCINATION_DETECTED`, `LOW_CONFIDENCE` |
| `security` | auth, tenant, egress, secrets in the diff | no | `INVARIANT_VIOLATION` |
| coordinator | integrates lane results; the only lane that may propose a merge | no | `TIMEOUT`, conflict |

Minimum useful shape: explorer → worker → verifier. Add reviewer and security
whenever the diff touches auth, tenant, egress, money, or a signed metric.
Keep at most four *active* lanes per repository — a design limit from the
multi-repo orchestration spec, not (yet) a runtime error; a serial day has one
active lane. More is lease contention and a worse code view, not more speed.

A brief has the same five parts for every lane — context (repo_root, session
or index instruction, code-view fingerprint, the Reference Provider receipts it may cite),
task, success criteria, must-not, return format — and is **self-contained**:
the lane never needs to read a file to learn its own scope. Templates and the
Tier A wrapper: `references/lane-briefs.md`.

If Ground refutes the user's premise (the duplication is not where they said,
the repo is not separate, the fact is not governed), say so *before*
decomposing and re-scope the lanes to what is actually there. A swarm on a
false premise is the most expensive way to discover it.

### 4. Execute — Tier A or Tier B

**Tier A — in-session fan-out** (same repo, mostly reads, minutes, one model
family). Runbook:

1. `trio_sideeffects.py snapshot <repos> --out <run>/state.json` — taken by
   you, before any lane, so a later `diff` is not self-attestation.
2. Launch every lane as a parallel `Agent` call **in one message**, each
   prompt = the wrapper + the brief, each with a distinct `LEIO_SESSION`
   (`references/lane-briefs.md`, "Tier A wrapper"). Read-only shapes get
   read-only tools.
3. Each return lands as `Tentative`. You are the critic: `Reflecting` means
   you resolved its `query_id`s (`trio_receipts.py`), checked the `file:line`
   it cites, and ran `trio_sideeffects.py diff`. Then `Committed`, or
   `Aborted` with one typed reason. Record the state *from the return*, never
   from the lane's own summary of itself.

**Tier B — harness day** (writes across lanes, multi-model, isolation, a merge
gate, cross-CLI): author a spec from `assets/day-spec.template.json` and an
objective from `assets/objective.template.sh`, validate, dry-run, execute:

```bash
python3 <skill>/scripts/validate_day_spec.py day.json
leio-harness models show            # source must be openrouter-api, not builtin
leio-harness day --spec day.json --bus 127.0.0.1:18815 --watch
```

Inherited non-negotiables: clean source tree (the harness refuses worktrees
otherwise — commit what lanes must see, stash only the unrelated rest),
`requireFreshCodeView: true`, templates that consume `{model}` when a lane
sets `workShape` (built-in CLI templates ignore it), no permission-bypass
flags, no secrets in the spec, models and lane count named before spending.

**S-2PC lifecycle — every lane result in either tier.** `Tentative` on
arrival; a critic takes it to `Reflecting`, then `Committed` or `Aborted` with
one typed reason (`INVARIANT_VIOLATION`, `LOW_CONFIDENCE`,
`DIMENSION_MISMATCH`, `REGRESSION_DETECTED`, `HALLUCINATION_DETECTED`,
`TIMEOUT`). Nothing skips `Reflecting`; nothing `Committed` is edited — re-propose.

**Why:** lane status must come from the runtime (`LaneOutcome.status`,
`DayReport.passed/failed`) or, in Tier A, from the returned block — never be
inferred from prose. A "swarm" that reports per-role status while zero agents
ran is a provenance bug, and it has happened; a harness doctor family exists
to keep it from returning.

Delta and pitfalls: `references/swarm-harness.md`.

### 5. Verify — three checks on the integrated tree, two of them scripted

| Check | Surface | Passes when | Otherwise |
| --- | --- | --- | --- |
| Contract drift + tests | LEIO `audit` (`strict`) or the doctors `context` suggested, plus the suggested test commands | zero new warnings vs the Ground baseline | `Aborted / INVARIANT_VIOLATION`, name the file and doctor |
| Claim attribution | `reference_provider_verify` with `verify_claims`, `receipt_id`, `snapshot_id` copied **unchanged** | `verification_status: verified` | `Aborted / HALLUCINATION_DETECTED`, quote the finding, fix, verify again |
| Collaborative improvement | `leio-harness integrate` then `gate` (or `compare`) with an objective printing one `OutcomeSnapshot` | verdict `Improved` | `NoImprovement`, `Regressed`, `Collapsed` leave the target untouched — that is the point |

Then the deterministic pass over your own output:

```bash
python3 <skill>/scripts/trio_receipts.py report.md --repo /abs/repo [--repo ...] [--manifest .../manifest.json]
python3 <skill>/scripts/trio_sideeffects.py diff --against <run>/state.json
```

`trio_receipts.py` resolves every cited LEIO `query_id` against
`events.ndjson`, checks Reference Provider id shapes and receipt+snapshot co-presence,
counts caption coverage in the synthesis, verifies manifest SHA-256s, and
flags a lane table with no runtime artifact behind it. A non-zero exit is a
fabrication signal; fix the report, do not argue with the script.

Run LEIO and the objective on the **integrated** tree, never an isolated lane.
Run Reference Provider verify on the prose you are about to ship, not on your notes.

When the thing under verification is someone else's *account of a swarm*
(a write-up, a status message), check it against the same three rules a
real run would face: lane count vs the four-active-lanes design limit,
promotion only through the gate on `Improved` (never per-lane merges), and
status from a runtime artifact (`DayReport`, `manifest.json`, branches) —
and say which of the three it fails, with the artifact that is missing.

### 6. Emit — one report, three receipt blocks, one receipts index

Use `assets/trio-report.template.md`. Blocks: LEIO (query ids, doctor delta,
fingerprint), Reference Provider (receipt, snapshot, per-claim status, runtime line first),
Harness (`DayReport` summary, manifest path, lane table with runtime status
and critic state — or the honest sentence "none ran, none claimed"), then the
synthesis with every sentence captioned, the decision boundary, **drift →
retroaliment** (each real drift named with what makes it durable — recipe in
`references/retroaliment.md`), the receipts index, and side effects
disclosed. A decision with downstream consequences that is not in the report
did not happen.

## Choosing the tier

| Signal | Tier A (in-session `Agent`) | Tier B (`leio-harness day`) | No swarm |
| --- | --- | --- | --- |
| Writes | none, or one lane writes | several lanes write | one edit |
| Isolation needed | no | worktrees + leases + merge gate | no |
| Models | one family | per-shape routing, cross-CLI | — |
| Duration | minutes | tens of minutes to hours | seconds |
| Cost owner | you, in tokens | named models, named lane count | — |
| Evidence | `query_id`s + returned blocks + side-effect diff | `manifest.json` + branches + verdict | `query_id`s |

When unsure, start Tier A with an explorer and a verifier; promote to Tier B
only when the explorer's blast radius says several lanes must write.

## Hard boundaries

- **Grep after LEIO, not before.** Substring search misses re-exports, glob
  imports and generated call sites. `rg` is for an exact literal or a known
  LEIO blind spot — and then you cite both.
- **One index per repo root.** Cross-repo means one loop per root and a
  comparison you write yourself.
- **A signed route is not a signed answer.** Read the claim statuses;
  describe a mixed boundary as two sentences, not one.
- **Demo Reference Provider is demo.** `runtime.profile` decides, not the confidence of the
  number.
- **The objective runs on the integrated tree.** Isolated-lane green is not
  evidence of a mergeable whole.
- **Lanes propose; the gate promotes.** No lane, including the coordinator,
  merges into the target. `gate --promote` does, only on `Improved`.
- **Status comes from the runtime or the returned block.** Never summarise a
  lane as done, failed or reviewing from its prose.
- **Receipts resolve or they are fiction.** A `query_id` that is not in
  `events.ndjson`, a receipt without its snapshot, a manifest hash that does
  not match — each is a fabrication signal, not a formatting nit.
- **No hard-coded counts, no promised verbs.** Kinds and doctors come from
  `capabilities`; verbs from `--help` (preflight prints both). The installed
  binaries may lag the checkout; the multi-repo orchestration MCP tools are a
  design at the time of writing.
- **Secrets stay operator-side.** Never in day specs, briefs, manifests or
  reports. Provider URLs never enter workspace runtime source.
- **No sends, promotions or writes on Reference Provider's authority.** Human approval is a
  separate, fresh message.

## Files in this skill

| File | Read when |
| --- | --- |
| `references/ground-leio.md` | Ground with more than one agent or repo; §7 for LEIO's known blind spots |
| `references/govern-reference.md` | a fact, metric or decision enters the task |
| `references/swarm-harness.md` | choosing Tier B, or mapping lane results to S-2PC |
| `references/lane-briefs.md` | writing any lane brief; the Tier A `Agent` wrapper |
| `references/retroaliment.md` | a real drift was found and must become durable |
| `assets/day-spec.template.json` · `assets/objective.template.sh` | authoring a Tier B day |
| `assets/trio-report.template.md` | emitting a rung-2 or rung-3 report |
| `scripts/trio_preflight.py` | phase 0, rung 2 and up |
| `scripts/validate_day_spec.py` | before any `leio-harness day` |
| `scripts/trio_sideeffects.py` | before/after any lane or subagent |
| `scripts/trio_receipts.py` | on every report before it ships |
