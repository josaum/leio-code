# Lane briefs

Every lane — harness `task` string or in-session `Agent` prompt — has the same
five parts. The shape is the same so a reviewer can read six briefs in a
minute and so a lane never has to guess what "done" means.

```text
CONTEXT
  repo_root:        /abs/repo        (Tier B: your worktree cwd — `leio-code index --repo .` first if .leio-code/index.json is absent)
  session:          LEIO_SESSION=<lane-id>   (Tier A, shared tree; a Tier B worktree already has its own sidecars)
  code view:        <fingerprint from `leio-harness codeview check`> (refuse to work on another)
  LEIO evidence:    query_ids you may rely on: <ids>
  Reference Provider evidence:   receipt res:… snapshot snap:… claims: cl:… value=… status=… as_of=…   (or "none in scope")
  runtime:          <reference runtime.profile>  → demo evidence | production
TASK
  <one paragraph, one outcome>
SUCCESS CRITERIA
  <2–4 checkable statements>
MUST NOT
  <the boundaries of this shape, plus task-specific ones>
RETURN
  status: <what you produced>   (the runtime decides ok/failed — do not self-declare)
  evidence: file:line list + query_ids you added
  reference: receipts cited unchanged, or "none"
  uncertainties: <what you could not verify>
  proposal: <the S-2PC intent: PROPOSE_FACT | PROPOSE_TOOL_CALL | MERGE_REQUEST (coordinator only)>
```

Lanes that cannot run tools get the evidence pasted into CONTEXT (the
`file:line` excerpts and the Reference Provider values), never an instruction to look it
up.

**Self-contained means self-contained.** The lane's `task` string (harness) or
`Agent` prompt (Tier A) carries the repo root literal, its own session id and
its own must-nots. Pointing a lane at "read section 3 of briefs.md at
/some/path" fails the moment the path is on another machine or the file moves,
and it hides the lane's scope from whoever reads the day spec.

## Tier A wrapper (in-session `Agent` fan-out)

Every Tier A lane prompt is the wrapper below followed by the brief. Launch
all lanes in one message; keep the session ids distinct; you are the critic.

```text
You are lane <lane-id> (<shape>) in a LEIO trio run. Work read-only unless your brief says worker.
- Every leio-code call passes LEIO_SESSION=<lane-id> (env) or --session <lane-id> (CLI); MCP calls pin repo_root=/abs/repo.
- LEIO first; `rg` only for an exact literal or a known blind spot, and then cite both the query_id and the file:line.
- Do not ask Reference Provider; cite only the receipts in CONTEXT, unchanged, or return "fact missing: <question>".
- Do not call the advisor tool, do not spawn agents, do not write outside <allowed output path, if any>.
- Return ONLY the RETURN block from the brief: status, evidence (file:line + query_ids you added), reference receipts cited, uncertainties, proposal intent.
  The critic will resolve your query_ids in .leio-code/events/events.ndjson — an id that does not resolve aborts your proposal as HALLUCINATION_DETECTED.
<brief>
```

Critic pass after the returns land (record it in the report's harness block):

| Lane | Returned status | Ids resolved | file:line checked | Side-effect diff | State | Reason |
| --- | --- | --- | --- | --- | --- | --- |

## explorer

```text
TASK      Map <feature/symbol/contract> across <repo(s)> with LEIO: status → capabilities → context,
          then graph callers-of / importers-of for every public symbol in scope. Establish blast
          radius, ownership, tests that anchor the area, and doctors that would notice drift.
SUCCESS   every claim has file:line + query_id; blast radius names files and consumers;
          tests_to_run and doctor_suggestions from context are listed; open questions are explicit.
MUST NOT  edit files; run builds longer than the scoped tests; grep before LEIO; infer a business
          fact — return "fact missing: <question>" instead.
RETURN    the five-part block; proposal = PROPOSE_FACT
```

## worker

```text
TASK      Implement the smallest change that satisfies the explorer map: <link to explorer return>.
          Add or update the smallest test that proves the behaviour first. Work only in your
          worktree/branch.
SUCCESS   scoped tests green; no new doctor warnings from the suggested doctors; diff touches only
          files inside the explorer's blast radius or explains why not.
MUST NOT  merge, push, or touch the target branch; change a governed metric's computation without
          the Reference Provider receipt for its current value in CONTEXT; add dependencies; bypass a gate.
RETURN    diff summary, tests run with pass/fail, doctors run; proposal = PROPOSE_TOOL_CALL
```

## verifier

```text
TASK      Verify the worker's diff on the assigned worktree: confirm the actual diff first, then
          run the tests_to_run and doctor_suggestions from Ground, plus anything the risk_notes
          demand. Report exact commands and results.
SUCCESS   every command listed with exit status; new-vs-baseline doctor delta stated; failing
          case named with file:line and likely owner.
MUST NOT  repair failures; edit tracked source; declare Committed — you move the proposal to
          Reflecting and recommend Committed or Aborted with a typed reason.
RETURN    verdict recommendation: Committed | Aborted/<INVARIANT_VIOLATION|REGRESSION_DETECTED|LOW_CONFIDENCE>
```

## reviewer

```text
TASK      Review the diff and the prose for correctness, test gaps, and provenance: does every
          claim in the worker/explorer returns have a query_id or file:line; are Reference Provider values
          quoted with status, as_of and claim id unchanged; is anything asserted that no
          surface produced.
SUCCESS   each finding cites the line it concerns; each unsupported claim is named; the review
          distinguishes "wrong" from "unproven".
MUST NOT  edit; re-ask Reference Provider; accept a value that differs from the receipt in CONTEXT by any
          amount.
RETURN    verdict recommendation: Committed | Aborted/<HALLUCINATION_DETECTED|LOW_CONFIDENCE>
```

## security

```text
TASK      Audit the diff for auth, tenant isolation, egress, secrets and provider boundaries.
          Use LEIO explain on every env var and Redis key the diff touches; run the egress /
          auth / secret doctors the profile exposes.
SUCCESS   each boundary either cleared with evidence or flagged with file:line; no secret or
          provider URL introduced into runtime source or artifacts.
MUST NOT  edit; approve a send, campaign or promotion — those need a fresh human message.
RETURN    verdict recommendation: Committed | Aborted/INVARIANT_VIOLATION
```

## coordinator

```text
TASK      Integrate lane results: collect LaneOutcomes from the DayReport, run the integrated
          tree through `integrate` and the objective, read the improvement verdict, assemble the
          trio report from assets/trio-report.template.md.
SUCCESS   report has all three receipt blocks; every lane's final S-2PC state and typed reason
          is recorded from runtime status + critic recommendation; drift → retroaliment section
          names a doctor, test or baseline for each real drift.
MUST NOT  merge into the target (only `gate --promote` on Improved does); pick a model by token
          volume; summarise a lane from its prose; omit a failed lane.
RETURN    the report path; proposal = MERGE_REQUEST only when the verdict is Improved
```
