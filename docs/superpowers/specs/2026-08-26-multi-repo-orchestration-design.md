# LEIO Multi-Repository Orchestration Design

Date: 2026-08-26
Status: Approved in design review; awaiting written-spec review

## Summary

LEIO Code will expose a local, agent-facing orchestration workflow for tasks that span one or more repositories. LEIO Code remains the intelligence and control plane. The `leio-harness` workspace crate remains a separate execution runtime. LEIO Workbench remains an optional visual client and does not own orchestration semantics.

For a multi-repository task, LEIO builds an immutable code graph for each pinned repository revision, federates those named graphs without losing provenance, and derives deterministic cross-repository contract edges. Harness runs isolated worker lanes in repository-scoped Git worktrees. Workers share a versioned task graph and may coordinate through explicit barriers. Barrier coordination is opt-in in the task specification and structurally enforced by Harness once enabled.

OpenRouter selects the concrete worker model by default through `openrouter/auto`. LEIO assigns the lane's repository, role, objective, graph scope, and coordination policy; OpenRouter assigns the LLM; Harness launches and supervises the worker. An explicit model pin overrides automatic routing.

## Product boundary

The shipped product is LEIO Code with two process boundaries:

- `leio-code`: repository indexing, graph export and query, cross-repository federation, contract inference, local stdio MCP tools, and task control requests.
- `leio-harness`: worker launch, process supervision, repository leases, Git worktrees and branches, task state, barrier enforcement, graph refresh coordination, and result manifests.

LEIO Workbench consumes Harness as a UI client. It may display tasks, workers, barriers, graphs, and results later, but the first implementation does not require Workbench changes. The separate `/Users/josaum/projects/harness` repository is not part of this design.

Local execution tools are exposed only by the stdio MCP server. The hosted ChatGPT Apps SDK surface does not receive local process, filesystem, Git, or OpenRouter execution authority.

## Goals

- Let a coding agent start and supervise bounded multi-agent work in arbitrary local repositories.
- Support one task containing multiple repositories.
- Give every worker a consistent shared graph of the task's repositories and proposed changes.
- Derive useful cross-repository contract edges without collapsing repository or revision identity.
- Make graph refreshes deterministic and atomic at explicit coordination barriers.
- Isolate all writes in repository-scoped worktrees and branches.
- Allow at most four active worker lanes per canonical codebase, with no machine-wide aggregate ceiling in v1.
- Use OpenRouter Auto Router for default model selection while preserving explicit pins.
- Return branches, diffs, tests, graph versions, model receipts, and diagnostics as structured evidence.

## Non-goals

- Automatic Git integration, merge, push, or promotion.
- A continuously mutating graph that changes after every commit.
- Prompt-only barrier coordination.
- A new coding-agent loop implemented inside LEIO Code.
- A general distributed scheduler or a machine-wide concurrency budget.
- Hosted execution through the Apps SDK.
- New schema-language parsers in v1. Federation initially links deterministic contracts already represented by LEIO's indexes and code graph.
- Replacing the existing single-repository `status`, `capabilities`, `context`, `find`, `explain`, or `graph` workflows.

## User workflow

The parent coding agent starts a task through the local MCP server with absolute repository roots, lane ownership, and optional barrier coordination. A representative request is:

```json
{
  "goal": "Update the client and service to the new checkout contract",
  "repositories": [
    {
      "repoRoot": "/absolute/path/client",
      "baseRevision": "HEAD"
    },
    {
      "repoRoot": "/absolute/path/service",
      "baseRevision": "HEAD"
    }
  ],
  "lanes": [
    {
      "laneId": "client-worker",
      "repoRoot": "/absolute/path/client",
      "role": "worker",
      "task": "Update the checkout request and its tests"
    },
    {
      "laneId": "service-worker",
      "repoRoot": "/absolute/path/service",
      "role": "worker",
      "task": "Update the checkout endpoint and its tests"
    },
    {
      "laneId": "contract-reviewer",
      "repoRoot": "/absolute/path/service",
      "role": "reviewer",
      "task": "Verify the client and service contracts agree"
    }
  ],
  "coordination": {
    "mode": "barrier",
    "requiredLanes": [
      "client-worker",
      "service-worker",
      "contract-reviewer"
    ]
  }
}
```

`repoRoot` is always absolute. Each repository is canonicalized and pinned to a concrete commit before graph generation or worktree creation. The response returns a `taskId`, `taskGraphId`, graph version, pinned repositories, lane states, and the next valid control operations.

## Identities and isolation

### Repository identity

LEIO reuses checkout discovery rather than treating a path string as identity:

- `repoId`: normalized Git origin when present, otherwise the canonical repository path.
- `baseRevision`: resolved commit SHA used for the task baseline.
- `worktree`: canonical worktree path.
- `branch`: lane branch.
- `head`: lane commit SHA.

Two clones of one origin share a `repoId` but retain distinct worktree identities. Separate repositories never share indexes, locks, branches, or event journals.

### Task and graph identity

- `taskId`: globally unique local task identifier.
- `taskGraphId`: stable identifier for the task's federated graph family.
- `graphVersion`: monotonically increasing integer within that family.
- `laneId`: unique within a task.
- `attempt`: monotonically increasing retry number within a lane.

Every graph node and edge retains repository, revision, file, and line provenance. Proposal nodes additionally retain lane, branch, head, and attempt.

### Artifacts

Repository-local source artifacts stay under each repository's `.leio-code/`, including its index and `exports/code-graph-v1` files. Task-level federation artifacts belong to Harness and default to:

```text
~/.leio-harness/tasks/<taskId>/
```

The task specification may provide an absolute `outputDir`. Each graph version is stored in its own immutable directory. Harness atomically replaces a small current-version pointer only after the new version and manifest are complete.

## Components

### Local MCP adapter

The local stdio MCP server validates requests, applies tool annotations, starts or contacts Harness, and translates task manifests into structured MCP results. It must not hold a mutable current repository. Every repository-bearing request carries absolute roots, and every task-scoped query carries a `taskGraphId` and optional explicit graph version.

The v1 task surface is:

- `leio_code_orchestrate_start`: validate and start a task, returning promptly with identifiers and initial state.
- `leio_code_orchestrate_status`: return task, lane, barrier, repository, graph, and evidence state.
- `leio_code_orchestrate_control`: accept only `retry_lane` and `cancel_task` in v1.
- `leio_code_task_context`: return a ranked, provenance-preserving working set across the federated graph.
- `leio_code_task_graph`: query task-scoped symbols, calls, imports, and cross-repository contract edges.

The MCP server continues to advertise `execution.taskSupport: "forbidden"`. A Harness run is a LEIO domain task, not an MCP tasks-extension object. Tool execution failures use `isError: true` with readable structured diagnostics.

### Federation engine

LEIO Code produces or refreshes each repository's existing revision-keyed named N-Quads export and query cache. The federation engine loads those immutable sources into a task-local dataset without rewriting their repository IRIs.

The initial deterministic cross-repository edge families are:

- HTTP client method and normalized route to matching server route.
- Exact environment-variable declarations and accesses.
- Exact or structurally identical Redis/message key readers and writers.
- Known binary or subprocess invocations to matching executable declarations.
- Canonically resolved package/import references when the target repository is one of the task repositories.

Ambiguous, templated, or conflicting matches remain explicit unresolved candidates with reasons. Text similarity alone cannot create a contract edge. The task graph is an evidence surface, not a license to guess.

### Harness task coordinator

Harness owns the durable task state machine:

```text
starting -> indexing -> running -> waiting_at_barrier
                                -> refreshing_graph -> running
                                -> blocked
                                -> completed
                                -> cancelling -> cancelled
                                -> failed
```

Harness creates worktrees and branches from pinned revisions, acquires repository-scoped leases, launches workers with argv arrays, captures bounded output, records results, and retires worktrees without deleting branches or evidence.

### Worker and OpenRouter adapter

Harness does not implement a new coding-agent loop. It launches an OpenRouter-capable coding-agent adapter through the existing declarative agent-template boundary. The adapter contract must support:

- model route input (`openrouter/auto` by default or an explicit model pin),
- a stable lane session identifier,
- repository/worktree cwd,
- an environment allowlist,
- task graph identity and version,
- structured completion and route receipts.

The default model mapping is no longer a hard-coded dated `workShape -> slug` table. `workShape` and role may constrain an allowed-model policy, but OpenRouter Auto Router chooses the concrete model. An explicit non-empty `model` value wins for reproducibility or specialized work.

The route receipt records the requested route and the concrete model and provider reported by the adapter. If an adapter cannot transmit stable routing identity or report the selected route, it fails preflight instead of claiming compliant OpenRouter routing.

`OPENROUTER_API_KEY` comes only from the operator's host environment. It is passed through a narrow allowlist to the routing adapter and is never serialized into task specs, prompts, graph artifacts, MCP results, logs, or manifests.

## Federated graph lifecycle

### Baseline

At task start, LEIO resolves every `baseRevision`, refreshes the repository-local indexes, exports the per-repository named graphs, derives contract edges, and publishes graph version 1. Workers start only after every required baseline graph is valid.

### Proposal overlays

Multiple lanes may produce divergent branches in the same repository. LEIO must not present those branches as one integrated repository state. At a barrier, each successful lane commit becomes its own proposal overlay named by repository, lane, attempt, branch, and head.

A refreshed task graph contains:

```text
baseline repository graphs
+ lane proposal overlays
+ deterministic cross-repository contract edges
+ unresolved contract candidates and conflicts
```

Workers can distinguish baseline, proposal, and canonical integrated nodes in every query response.

### Canonical refresh

Only an explicit, separately authorized Git integration operation can nominate a new canonical repository head. After such an operation, a later graph refresh replaces the relevant proposal view with a graph exported from that integrated commit. This design does not add the integration operation itself to the v1 MCP surface.

## Barrier protocol

Barrier coordination is disabled unless the task specifies `coordination.mode = "barrier"`. Without it, lanes behave independently and no shared refresh is implied.

When enabled:

1. Every lane receives the current `taskGraphId`, `graphVersion`, and barrier identifier.
2. Lanes communicate through the existing Harness bus.
3. A lane finishes its phase by emitting `barrier_ready` with repository, branch, commit, tests, result summary, and graph-affecting paths.
4. Harness validates the commit and evidence before marking the lane ready.
5. Harness waits until all required lanes are ready.
6. LEIO builds proposal overlays for changed lane commits and derives cross-repository edges.
7. Harness atomically publishes the next graph version.
8. Harness emits `barrier_released` with the exact version and resumes the required lanes.

Agent messages may explain progress, but only structured Harness events change barrier state. A worker cannot release itself or another worker.

## Concurrency

The limit is four active lanes per canonical `repoId`, not four per task and not four for the whole machine.

- A task phase declaring more than four lanes for one repository is rejected before launch.
- Repository leases are atomic across simultaneous Harness tasks and processes.
- If another task already consumes capacity for a repository, a new start fails with structured `repo_capacity_exhausted` evidence rather than exceeding four or silently waiting.
- Different repositories may each run four lanes concurrently.
- There is no machine-wide aggregate limit or scheduler in v1.

## Dirty working trees

Harness never stages, commits, resets, stashes, cleans, or otherwise modifies the user's source checkout. A dirty repository blocks task startup by default because a worktree created from `HEAD` would silently exclude uncommitted work.

The caller may explicitly set `excludeUncommittedChanges: true` for that repository. Harness then pins the requested commit, reports the excluded dirty paths, and creates lane worktrees from the commit. The dirty source checkout remains untouched.

## Failure handling

### Startup and graph failures

- An invalid, duplicate, missing, or non-Git repository fails validation.
- A missing or ambiguous revision fails before worktree creation.
- Failure to build any required baseline graph prevents task start.
- Graph ambiguity becomes unresolved evidence; it does not fail the task unless the graph itself is invalid.

### Worker and OpenRouter failures

- A transient provider or process-start error receives one bounded retry using the same stable lane routing identity.
- Test failures, invalid commits, malformed evidence, and policy failures are not retried automatically.
- An explicit model pin cannot silently change to another model.
- Exhausted retries mark the lane failed and block any barrier that requires it.

### Barrier and refresh failures

- A missing, timed-out, or failed required lane leaves the barrier blocked.
- Harness cannot silently remove a required lane or release a partial barrier.
- A failed graph refresh leaves the previous graph version current.
- `retry_lane` creates a new recorded attempt for only the selected failed lane.
- `cancel_task` terminates worker process groups and releases leases while preserving branches, commits, manifests, logs, and graph versions.

## Git authority

V1 may create worktrees, create lane branches, stage lane changes, and commit within those lane worktrees because those operations are the requested execution workflow. V1 may not automatically integrate, merge, rebase, push, delete branches, or promote results.

Task completion returns repository-scoped branches, commits, diffs, test results, graph findings, and unresolved integration risks. The parent coding agent or user decides whether to invoke a later, separately authorized integration workflow.

## MCP and security boundaries

- Orchestration tools exist only on the local stdio MCP surface.
- Tool schemas use absolute paths and reject unresolved traversal or symlink escapes.
- Tool annotations accurately mark read-only graph queries versus mutating start/control operations.
- Worker commands are argv arrays; task data is never interpolated into a shell command.
- Environment inheritance is deny-by-default with an explicit allowlist.
- Secrets are redacted before output and are never graph nodes.
- Timeouts, frame sizes, captured output, and result artifacts are bounded.
- Existing single-repository MCP tools and wire behavior remain backward-compatible.
- Workbench may consume task manifests later but cannot bypass Harness leases, barriers, or Git policy.

## Verification strategy

### Federation tests

- Matching HTTP client and server fixtures in separate repositories produce one cross-repository edge with correct repository, revision, file, and line provenance.
- Identical symbol names in different repositories do not collide.
- Ambiguous contracts stay unresolved.
- Proposal overlays remain distinguishable from baseline and canonical heads.
- A failed refresh leaves the previous graph version byte-for-byte current.

### Harness tests

- A barrier releases only after every required lane submits valid evidence.
- Failed, timed-out, or malformed lanes block the barrier.
- A retry replaces only the failed lane attempt.
- Cancellation terminates processes and leases while preserving recoverable evidence.
- More than four lanes in one repository phase is rejected.
- Atomic cross-process leases prevent concurrent tasks from exceeding four lanes for one repository.
- Separate repositories can each run four lanes concurrently.

### OpenRouter tests

- Default lanes request `openrouter/auto`.
- Each lane receives a stable unique routing session identifier.
- Explicit model pins override automatic routing.
- Requested route and selected model/provider receipts are recorded.
- API keys do not appear in specs, prompts, logs, manifests, graph artifacts, or MCP results.
- CI uses a fake OpenRouter endpoint and fake worker adapter; it makes no paid calls.

### MCP tests

- The five task tools have validated input/output schemas, correct annotations, structured content, and visible execution errors.
- The hosted Apps SDK tool catalog does not expose local orchestration tools.
- Existing single-repository tool contracts remain unchanged.

### End-to-end acceptance

An end-to-end fixture creates a client repository and a service repository with a discoverable HTTP contract. The task starts isolated worker lanes, enforces a barrier, publishes proposal overlays, exposes a deliberately introduced contract mismatch in the federated graph, and finishes with branch and test evidence without merging either repository.

## Implementation slices

The implementation plan should preserve one end-to-end workflow while landing it in dependency order:

1. Task identities, immutable federation artifacts, contract queries, and proposal-overlay tests.
2. Harness multi-repository task state, repository-wide four-lane leases, barrier protocol, and control operations.
3. Local stdio MCP task tools and explicit hosted-surface exclusion.
4. OpenRouter Auto Router worker policy, routing receipts, secret redaction, and the two-repository acceptance fixture.

Each slice must be test-first and keep the repository's existing dirty work separate from task-specific edits.
