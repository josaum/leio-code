# Durable engineering workflow

`leio-harness workflow` adopts F22's intake, confirmation, plan approval, incremental execution and durable progress contracts. Runs persist in JSON-LD, independently of the calling agent or terminal. This is a local, same-user interface: a plan digest is a review binding, not a security credential.

## Run a workflow

Create a plan JSON with `request`, `constraints`, `questions`, `acceptance`, and `steps`. Each step contains `name`, `kind`, `argv`, `timeout_ms`, `retry_safe`, and repo-relative `artifacts`. Commands use the existing supervised Harness process adapter (timeouts, bounded output and Arrow logs).

```sh
leio-harness workflow --dir /tmp/my-run --action init --repo /path/to/repo --input plan.json
leio-code --repo /path/to/repo --json context 'the requested change' > evidence.json
leio-harness workflow --dir /tmp/my-run --action evidence --input evidence.json
leio-harness workflow --dir /tmp/my-run --action show
# Review the plan and copy its digest into the following explicit transitions.
leio-harness workflow --dir /tmp/my-run --action confirm --approval DIGEST
leio-harness workflow --dir /tmp/my-run --action approve --approval DIGEST
leio-harness workflow --dir /tmp/my-run --action execute --approval DIGEST
```

Execute advances one step at a time. Successful steps retain their results when a later step fails; available declared artifacts are hashed even when a command fails. `revise --input revised-plan.json` archives the prior plan, evidence and approval state before clearing current confirmations, evidence and progress. New plan digests bind the run identity and input revision, so returning to an earlier plan does not restore its approval. Known failures may be retried only when the approved step declares `retry_safe`. Adapter errors conservatively retain an uncertain `running` state, even if they may have occurred before spawn. An interrupted or uncertain `running` state refuses replay; `reconcile --input operator-note.txt --approval DIGEST` records investigation and invalidates execution approval. Inspect child processes and effects before reconciling.

Deployment and rollback steps are disabled unless initialized with `--deploy-enabled`. A completed plan means its commands passed and declared artifacts were hashed; it does not itself establish a verified deployment. Include independent runtime revision verification and a tested rollback command in a delivery plan. A `notification-failed` record preserves the work outcome.

## Agent-driven intake

The calling agent is the interface. It turns the user's request into the plan JSON, gathers bounded LEIO evidence, and uses `workflow init` and `workflow evidence` to persist them. The user does not need to fill a form or write JSON. No browser or nested LLM process is required.

The agent presents the proposed requirements and commands for review, then records confirmation and execution authorization as separate transitions. Existing authorization from the conversation can be carried forward when it covers the concrete plan; evidence alone never grants authority. Reopen with `workflow --action show` to recover progress. Revised inputs must go through `revise` so stale approvals cannot apply.

## Verified local delivery

The `delivery` adapter publishes a static-site directory as an immutable, content-addressed release. It serves the active release on loopback, verifies the served artifact, and persists delivery evidence as JSON-LD. It is a concrete local delivery target; it does not provision a cloud service.

```sh
# Produce site/ using your real build command and focused checks first.
leio-harness delivery --dir /tmp/site-delivery --action prepare --source /path/to/repo/site
# In a second terminal, run the actual serving process.
leio-harness delivery --dir /tmp/site-delivery --action serve --address 127.0.0.1:8788
```

Review the prepared revision and authorization digest, then pass them explicitly:

```sh
leio-harness delivery --dir /tmp/site-delivery --action deploy --revision REVISION --approval DIGEST
leio-harness delivery --dir /tmp/site-delivery --action verify
leio-harness delivery --dir /tmp/site-delivery --action inspect
```

After deploying a second release, inspect the rollback authorization digest and use `--action rollback --approval DIGEST`. Authorization is bound to the target and current revision; an intervening deployment invalidates stale approval. A deployment is verified only when runtime content matches the expected artifact. Preserve the deployment directory across server restarts.

These commands can be explicit `build`, `deploy`, `verify` and `rollback` steps in a workflow. Copy reviewed digests into the approved plan rather than deriving approval from evidence automatically. Preparing a release and reviewing its digest are separate from promoting it.

## Provenance and limits

Behavioral source: F22 DashboardService's session transitions, write-up progress and notification failure contracts, reviewed in the local F22 snapshot. No F22 implementation code was copied. The workflow executes local argv, with an agent-driven CLI and static-site delivery adapter. Remote infrastructure provisioning and authenticated multi-user approvals are outside this implementation. Evidence attachment is host-supplied; it does not certify coverage or completeness. No F22 capability is retired by these changes.

See [JSON-LD 1.1 reference](../knowledge/json-ld-1.1.md).

## Verification

Run `cargo test --locked -p leio-harness --test workflow`. For independent JSON-LD expansion and RDF conversion, build the debug CLI and run `python scripts/verify_workflow_jsonld.py --binary target/debug/leio-harness` in an environment with PyLD installed. The smoke prints the retained temporary run path and RDF quad count.

`python scripts/verify_delivery_jsonld.py --binary target/debug/leio-harness` builds two fixture sites through the workflow CLI, verifies actual HTTP delivery, rolls back, and independently expands the retained workflow and artifact JSON-LD documents. Run `cargo test --locked -p leio-harness --test workflow --test delivery` for authority, persistence and delivery recovery regressions.
