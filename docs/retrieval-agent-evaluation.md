# Agent retrieval evaluation

Seven labeled development tasks in `benchmarks/retrieval-agent-tasks.json` exercise
smoke testing, ranking, binary resolution, startup instructions, updates, workflow
persistence and rollback. Labels identify useful implementation files and are not
exhaustive relevance judgments. The suite is small and used during development;
these measurements are not held-out evidence of general retrieval quality.

Run against an explicit binary and repository:

```sh
python3 scripts/evaluate_retrieval.py --binary ~/.cargo/bin/leio-code \
  --repo . --tasks benchmarks/retrieval-agent-tasks.json --output /tmp/retrieval.json
```

The before/after runs on the same checkout produced:

| Metric | Before | After |
| --- | ---: | ---: |
| Mean reciprocal rank | 0.440 | 0.493 |
| Relevant first result | 2/7 | 2/7 |
| Relevant result in top three | 4/7 | 4/7 |

Rank fusion now contributes only from scoring channels with a positive match.
Previously, absent channels could add a rank bonus. The startup hook moved from
outside the top five to second. Docker smoke fell from third to fourth and update
from fourth to fifth. Local delivery remains outside the top five. These are
known gaps, not solved by the aggregate improvement. Per-task paths and binary
identities are recorded in `benchmarks/retrieval-agent-results.json`.

Both existing LEIO repository golden tasks also passed. This evaluation does not
measure call-edge accuracy, runtime behavior, latency or F22 retrieval quality.

# Instruction distribution

Portable packaging and installation include identical payloads, including hooks,
build metadata, schemas and the knowledge core. The session hook emits the
canonical `skills/leio-code/SKILL.md`, including from an unrelated working directory.

`leio-code update` refreshes Codex through `plugin add --json`, avoiding an uninstall
gap, and compares the installed skill, session hook and MCP entry point with the
checkout. Missing or stale files produce a warning. Already-open sessions retain
their loaded instructions until reopened; refreshing the cache cannot rewrite
instructions already injected into a conversation.
