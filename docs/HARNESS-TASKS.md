# Observable coding tasks

The local `leio-harness task` interface runs explicit, caller-supplied commands.
It does not wake named agents, choose a provider, send messages, merge branches,
or need the semantic bus, embeddings, WAHA or Hermes. Existing `run`, `day`, ACP
and gate interfaces remain separate. Use `task` when a coding host needs a
durable run ID and inspectable progress instead of an ambient team.

## Contract (version 1)

Every action pins an absolute `--repo`. Records live at
`<repo>/.leio-code/harness/runs/<run-id>/` (private to the local user).

| CLI action | Required inputs | Output / effect |
| --- | --- | --- |
| `submit` | `--task`, `--worker`, `--argv` JSON string array, `--verify-argv` JSON string array | Creates a run and starts a detached supervisor; returns immediately with its run ID and record paths. |
| `list` | repository | Most recent 20 runs, including actual supervision state. |
| `status` | `--run-id` | Task, worker label, phase, supervisor/worker PID, elapsed time, last observed output, failure reason and artifact paths. |
| `events` | `--run-id`, optional `--after`, `--limit` | Bounded sequence-numbered lifecycle/output-reference events, with a continuation cursor. |
| `result` | `--run-id` | Status plus worker and verification results, changed paths, diff, logs and unresolved failure. May be called before completion. |
| `cancel` | `--run-id` | Requests cancellation; only the owning supervisor signals its worker group. Status acknowledges completion asynchronously. |

`leio_code_task` exposes those six actions through local stdio MCP with the same
repository and run IDs, plus the agent-first additions below. Explicit-command
`submit` requires `argv`, `verify_argv`, `task` and `worker`; the Codex preset
can omit `argv`. No shell interpolation is performed by the harness. Commands are
explicitly authorized execution, not read-only queries. Do not put secrets in
arguments, task text or constraints: the local records preserve them.

States are `queued`, `running`, `blocked`, `failed`, `cancelled`, `succeeded`.
Phases distinguish worker execution, verification and completion. A held
supervision lock establishes ownership; a heartbeat, bus socket, timestamp or
stored PID alone does not establish running work. Lost supervision produces a
failed/interrupted view, never a false running state. Cancellation before
dispatch prevents spawn. Timeouts and spawn errors become terminal results.

Only one task may execute per canonical repository directory at a time. Another
submission becomes blocked rather than sharing the checkout. Use explicit Git
worktrees for independent concurrent tasks. Submission requires a clean Git
checkout so `changes.patch` and `changedPaths` describe this task. The worker
receives `LEIO_TASK`, `LEIO_TASK_CONSTRAINTS` and `LEIO_TASK_SPEC` in its
environment; the caller must use a command that accepts or reads the task.
Constraints are recorded instructions, not an OS sandbox. Harness sidecar files
are excluded from the clean-checkout check and change capture even when the
repository does not yet ignore them.

Verification is mandatory and runs only after a successful worker exit. A
successful run means both commands exited zero; it does not independently
certify arbitrary model prose or business claims. Output events mean actual
stdout/stderr bytes, not inferred reasoning or parsed tool invocations. Silence
is displayed with its last output time; it is not treated as progress.

`changes.patch` compares against the submitted Git revision, including tracked
changes committed by an explicitly supplied worker. `changedPaths` includes new
files; their contents are archived under `untracked/` (64 regular files, 4 MiB
per file, 16 MiB total). Symlinks and oversized archives are reported with
`untrackedTruncated`; oversized diffs with `diffTruncated`. Inspect the checkout
when either flag is true. No commit or promotion is performed by the harness.

Each stage has a configurable timeout (default 10 minutes) and captures at most
4 MiB per stream. The status/result interfaces return paths, not whole logs.
The event journal records bounded metadata and references the output files.

## Example

```sh
leio-harness task submit --repo /absolute/clean/worktree \
  --task 'Fix the parser regression; do not commit' --worker codex \
  --argv '["codex","exec","--json","Fix the parser regression; do not commit"]' \
  --verify-argv '["cargo","test","-p","parser"]'
leio-harness task list --repo /absolute/clean/worktree
leio-harness task status --repo /absolute/clean/worktree --run-id <returned-id>
leio-harness task events --repo /absolute/clean/worktree --run-id <returned-id> --after 0
leio-harness task result --repo /absolute/clean/worktree --run-id <returned-id>
```

## Retaining the harness

The acceptance gate covers submission, live ownership/output visibility,
reviewable change and verification, provider-style nonzero failure, spawn
failure, cancellation of a process tree, timeout, interruption and isolation
from unavailable bus/WAHA services. Preserve the operational logs when retiring
the ambient scheduler. UI consumers must read this contract rather than infer
agent work from a listening bus or quarter-hour clock.

## Local runtime retirement (2026-09-30)

The `ai.jquant.cida-bumbo` LaunchAgent is disabled. Logs, profile databases and
the original plist are preserved. The bus and embedding LaunchAgent definitions
were moved from `~/Library/LaunchAgents/` to
`~/.leio-harness/manual-services/`, so they no longer start at login. Copies of
all three definitions are in `~/.leio-harness/retired/20260930-012830/`.
`~/.leio-harness/runtime-policy.json` records the change.

Known optional consumers are JAI Team's explicit `BusHealth.start`, Hermes
memory publication via `busio.py`, and `bus-talk` (which uses the embedding
service). Existing `day`, bus, GEPA and codeview workflows remain available.
They are not needed by `task`. This inventory covers the inspected local
consumers; it does not claim to enumerate every external client.

Use `leio-harness-services status|start|stop` for the preserved optional services.
Starting them explicitly registers them for the current login session; their
plists remain outside the login startup directory. This command never wakes
agents or sends messages. JAI Team's Start control can still start the bus alone.

## Agent-first MCP contract

For current uncommitted work, use `leio_code_task` with `action="review"`,
the source checkout root, and `sources`: an explicit list of relative regular
files. Optional `task`, `context`, `constraints`, `codex_binary` and `timeout_ms`
bound the review. No worktree path, worker argv or verification command is needed.
The action copies current file bytes, including tracked edits and new files,
preserves relative paths, records SHA-256 provenance, and makes a private Git
snapshot under `.leio-code/harness/reviews/<id>/snapshot`. The internal baseline
commit belongs only to that generated snapshot; the source checkout is neither
committed nor stashed. Source symlinks, traversal and Git/harness metadata are
rejected. Capture limits are 64 files, 4 MiB per file and 16 MiB total. Capture
is consistent per file and rejects concurrent writes observed while reading;
it is not an atomic repository-wide snapshot. Unselected files and dependencies
are not copied. Select relevant instructions and supporting source explicitly.

The review preset uses Codex's read-only sandbox with approval policy `never`.
It inventories configured MCP servers without starting them and disables each
server for this invocation. It also ignores exec rules and disables apps, plugins,
hooks and subagents to avoid giving external tools a write route outside the shell
sandbox. Existing Codex authentication and model/provider settings are retained.
Before submitting, it checks the effective inventory again and refuses to start
if any server remains enabled. Server names containing dots or special characters
are rejected because the CLI dotted-key override cannot isolate them reliably.
Findings go to stdout and are returned
through the bounded inline worker-output preview. Built-in verification checks
that selected snapshot hashes and working files remain unchanged; it does not
validate findings or check the original source after capture. You can continue
editing the original checkout while the review runs. Arbitrary worker/verification
commands are rejected for `review`; explicit editing remains the `submit` path.

`review_source` contains the source root, snapshot root, capture time, selected
paths and hashes. Follow returned `next_calls` against the snapshot root for
status, events, result and cancellation. `list` on the original source returns
`review_runs` receipts with ready-to-call live-status requests, in addition to its
ordinary tasks. Receipts are discovery records, not live status. The newest 64
review directories are inspected; `review_runs_truncated` marks a larger catalog.
Snapshots and receipts are retained for inspection without automatic cleanup.
Provenance receipts have a shared 128 KiB write/read bound that accommodates
the maximum selection of Unicode paths; oversized provenance is rejected before
worker submission. Git staging treats selected filenames as literal paths.
Successful captured reviews return `review.next_action="review_findings"`,
with an assurance statement specific to snapshot integrity and finding review.

Optional receipt failures do not invalidate durable task state or block lifecycle
controls. `review_warnings` includes the affected path and failure kind. If the
source receipt is missing or malformed, a validated catalog receipt can supply
provenance; otherwise status, events, result and cancellation still return the
core task with an explicit warning and no claimed provenance. Reads do not repair
receipt files. Structural validation prevents malformed provenance from breaking
the MCP response after an operation has already completed.

Catalog publication is atomic and does not overwrite an existing receipt. If it
fails after submission, the response preserves the task, run ID and pinned
follow-up calls, with `review_catalog_record_failed`. If the harness submission
reply is lost or malformed, the response reports an unknown outcome and returns
the preserved snapshot plus a `list` follow-up; inspect durable records before
resubmitting. This error does not assert that a worker started or that it failed.

One unreadable catalog receipt does not hide healthy reviews. Discovery can
recover references from the same snapshot's confined run directories and marks
those entries `recovered=true`, with `review_catalog_incomplete=true` and warnings.
Recovery inspects up to eight run directories per snapshot and returns at most 64
references overall; truncation is explicit. Redirected catalog, receipt and run
paths are rejected. Recovered references are discovery hints: call status for
live ownership and outcome, and inspect verification before accepting findings.

The local MCP task tool adds an explicit `prepare` action and a `codex` submission
preset. `prepare` takes an absolute `worktree_path` that does not yet exist and an
optional `base_ref` (default HEAD), resolves a commit, and creates a detached Git
worktree. It never commits or copies local edits. A dirty source is rejected
unless `allow_dirty_source=true` explicitly acknowledges that those edits will
be excluded. The returned `repo_root` is the root for subsequent task calls.

For `submit`, use `worker="codex"`, `task`, and `verify_argv`; omit `argv` to use
the installed Codex executable with its existing login/model, ephemeral JSON
output and workspace-write sandbox. Optional `context` carries caller findings;
`constraints` carries scope requirements. Task/context remain literal argv text.
`codex_binary` may select an absolute executable. Explicit `argv` remains the
existing low-level route and cannot be combined with preset-only fields.
The preset records instructions to avoid commits, pushes, messages and further
delegation; recorded instructions are not an enforcement boundary.

`result` preserves the existing snapshot and adds `review`: task outcome,
verification evidence, changed files, bounded inline patch and worker/verification
stdout/stderr, truncation/missing-artifact flags, and a recommended next action.
`review_bytes` bounds each of the five previews (default 8192, maximum 32768).
Read artifacts only from the expected run directory, reject redirected paths,
and retain full paths for local follow-up. Logs and patches are untrusted worker
output. A succeeded command is evidence to review, not permission to merge.
All lifecycle responses include ready-to-call `next_calls` pinned to the returned
repository/run ID. Submission remains nonblocking; use status/events while running,
result at completion, and cancel when needed. No automatic polling or promotion.

Fresh stdio sessions discover this through initialize instructions and guide
`topic="tasks"`. Existing connected hosts must reconnect to refresh tool catalogs
and loaded code; restarting a worker does not refresh its parent host's tools.
The UI and hosted Apps SDK remain separate from this local execution contract.

### Agent-first acceptance (2026-09-30)

The fresh stdio SDK connection advertised the task tool, initialize instructions,
and tasks guide. The installed-transport check verified all 19 tools against the
canonical catalog. MCP tests passed 109 with one optional skip; seven surface
contract checks passed. Tests cover literal task/context handoff, dirty-source
rejection and explicit exclusion, source preservation, worktree paths with spaces,
inline review, verification evidence, cancellation, invalid parameter combinations,
UTF-8 byte bounds, and redirected artifact rejection.

A real installed Codex run used the preset without explicit argv or binary,
prepared a fresh detached worktree, fixed a byte-count parser, changed only
`size.py`, and passed the independent `python3 verify.py` command. Run
`18da049601431ed8-13d49-0` completed in 91 seconds. Its full review packet is at
`.leio-code/harness/acceptance-20260930/agent-preset/review-default-timeout.json`.
An earlier run with a caller-selected three-minute limit timed out after writing
its patch; its result correctly retained a failure, partial changes and no
verification result. Both receipts are preserved. No UI changes were needed.

### Review-driven corrections

New submissions must use the Git checkout root. A subdirectory shares the Git
checkout but previously received a separate ownership lock and narrower change
capture. Reject it before creating a run or spawning a worker; scope a module
through the task instructions. Existing records remain inspectable at their
original roots.

Inline review also includes `new_files`, each with its relative filename and a
confined artifact preview from the captured `untracked/` archive. Up to eight
files share an additional `review_bytes` budget. `new_files_incomplete` explicitly
reports omitted, truncated or unavailable contents; full archive paths remain in
the task snapshot. These are captured file contents, not mutable checkout reads.

`prepare` checks the destination after Git hooks have run. If a hook leaves it
dirty, return an error with `preparation.clean=false`, preserve its path and
contents, and offer no submit next-call. A clean result explicitly reports
`preparation.clean=true`. Codex discovery validates every installed candidate
before selecting it and continues past directories or non-executable files.

### Real repository review and installed fixes (2026-09-30)

Run `18da04dfe87540d0-1808f-0` reviewed an exact eight-file snapshot of the current
implementation. Submission returned in 163 ms; the review completed in 470666 ms
and changed only `review.json`. Independent verification confirmed the captured
source hashes were unchanged. Its three demonstrated findings were the nested
checkout ownership/capture bug, dirty checkout-hook postcondition, and invalid
Codex discovery candidate. The new-file inline evidence gap was separately
reproduced. All four were corrected with behavioral regressions.

Validation: 163 Rust tests passed with serial execution, clippy passed, and the
installed harness MCP suite passed 112 tests with one optional skip. Seven
surface-contract checks and the 19-tool installed-transport check passed.
`jai-ship harness` installed the release; Cargo, Grok and release executable
hashes matched. An installed-binary check kept a root task running, rejected a
nested submission before record creation, and acknowledged cancellation of the
probe. Fresh MCP result retrieval returned all three review findings through the
new-file preview, with no direct report-file read needed by the consumer.

Receipts are under `.leio-code/harness/acceptance-20260930/real-review/`:
`result.json` preserves the pre-fix result, `result-after-fixes.json` demonstrates
inline report retrieval, and `installed-lock-fix.json` records the installed
checkout-root behavior and executable hash. That trial used manual snapshot
preparation; the subsequent `review` action automates selected-file capture for
current uncommitted edits. This trial supports bounded background reviews with
durable evidence; it is not a
measurement of productivity gains over an integrated host's native delegation.

### Automatic current-file review acceptance (2026-09-30)

Run `18da06b33d7d6470-53b2-0` used `action="review"` directly against four current
implementation files in the dirty source checkout. No manual worktree, snapshot,
worker argv or verifier setup was required. Real Codex completed in 177437 ms;
the built-in verifier passed, the snapshot had no changed files, and independent
before/after source hashes matched. Its two findings identified a Unicode receipt
size mismatch and Git pathspec interpretation of literal filenames. Filesystem
regressions reproduced both before the fixes and passed afterward.

The final installed-harness MCP suite passed 118 tests with one optional skip.
Syntax and diff checks passed; the generated service descriptor matched a fresh
tools/list response. The installed transport check verified 19 stdio tools,
CLI identity, task context and navigation cursor. A fresh client found the real
run through source-root `list`, then retrieved the result and inline findings
using the pinned snapshot root. Completed status reported no live supervisor.

Receipts are under `.leio-code/harness/acceptance-20260930/automatic-review/`:
`result.json` preserves the review, `result-after-fixes.json` demonstrates final
result consumption, `discovery-after-fixes.json` proves source-root discovery,
and `source-before.json` / `source-after.json` record source preservation at review
completion. Earlier authentication and configuration-override failures are also
retained; neither claimed success or ran verification. The selected-file trial
does not substitute for a full repository review or validate finding semantics.

### Receipt and submission recovery acceptance (2026-09-30)

Fault injection against the installed harness reproduced an existing failure:
blocking `record.json` caused a tool error after a worker had already started,
discarding the submission response. The corrected handler preserves the run and
follow-up calls with a publication warning. Installed-transport tests recover
that run through source-root discovery, corrupt its remaining source receipt,
and still read status/events, request cancellation and observe a cancelled state
with no live supervisor. Source content is preserved.

A second installed-transport test lets the real harness submit a worker, then
replaces the CLI reply with malformed data. The handler reports an unknown
outcome and returns a snapshot-root list call. Following it recovers exactly one
run, also discoverable from the source root; its worker and built-in verification
complete successfully. No retry or second submission occurs. Filesystem tests
cover healthy entries beside corrupt or missing receipts, validated provenance
fallback without disk repair, and rejection of redirected recovery directories.

Final MCP validation passed 122 tests with one optional skip; syntax and diff
checks passed. All 19 installed stdio tools and the generated descriptor were
verified. A fresh client recovered the earlier real Codex run
`18da06b33d7d6470-53b2-0` through the updated handler with its succeeded state,
passed verification, no changed files and no live supervisor. Evidence is under
`.leio-code/harness/acceptance-20260930/recovery/`, including test logs and fresh
discovery/result receipts. This recovery work used controlled workers and injected
transport/filesystem faults; it did not run another model review.
