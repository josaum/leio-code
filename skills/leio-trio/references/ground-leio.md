# Ground — LEIO Code inside the trio

Canonical routing lives in `skills/leio-code/SKILL.md`. Read it for verbs,
tool families and the per-repo loop; this file only adds what changes when
LEIO feeds a swarm and a report.

## 1. One session per concurrent agent

Every lane or subagent that touches the same tree sets its own session:

```bash
LEIO_SESSION=<lane-id> leio-code --repo /abs/repo nav goto <symbol>
leio-code --repo /abs/repo --session <lane-id> context "<task>"
```

The MCP server reads `LEIO_SESSION` from the environment. Nav cursors land at
`.leio-code/sessions/nav-<id>.json`; the index, wiki, formal graph and lattice
locks stay shared. Two lanes without distinct sessions overwrite each other's
cursor and then reason from the wrong "here".

This is a **Tier A** concern: in-session subagents share one checkout. A
**Tier B** lane runs in its own worktree, so its `.leio-code/` sidecars are
already private — but a fresh worktree has no index. The harness does not
inject a session or an index; the lane's first action is
`leio-code index --repo .` (then `status`) unless `.leio-code/index.json` is
present. Put that line in every Tier B brief.

## 2. The code-view fingerprint is the swarm's shared state

```bash
leio-harness codeview check --repo /abs/repo     # fresh only when the stamped summary matches the index
leio-code index --repo /abs/repo                 # when stale, then check again
leio-harness codeview publish --repo /abs/repo --bus <addr> --agent-id <lane> --run-id <run>
```

`publish` indexes in-process and puts the fingerprint on the bus topic
`code-view/<repo>`; a day with `requireFreshCodeView: true` fails fast when any
lane would start from a stale view. Record the fingerprint in Ground and put it
in every lane brief so a lane can refuse to work on a different view.

## 3. Cite `query_id`, not vibes

Every call returns a `query_id` (for example `context-1788477534952084000`) and
appends one JSON-LD PROV line to `.leio-code/events/events.ndjson` whose `@id`
is `urn:leio-code:query:<repo-slug>:<query_id>`. That id is the receipt:

- lane briefs list the `query_id`s whose evidence the lane may rely on;
- lanes return the `query_id`s they added;
- the report's LEIO block lists all of them.

Do not set `LEIO_DISABLE_EVENTS=1` during a trio run — it removes the receipt.

## 4. Capture the verification baseline before lanes start

From `status` keep the doctor line ("N/M with warnings"). From `context` keep
`tests_to_run`, `doctor_suggestions`, `graph_queries` and `risk_notes` — they
are the verifier lane's anchors and the reviewer lane's checklist. After
integration, run the same doctors on the integrated tree; the delta against
this baseline is the LEIO half of Verify. A warning that already existed is
context; a new one is `INVARIANT_VIOLATION`.

## 5. Cross-repo is one loop per root

`status → capabilities → context` once per absolute root, never one tree's
index against another. Federated multi-repository task graphs and
`leio_code_orchestrate_*` tools are a design document at the time of writing;
confirm with `capabilities` before promising them. Until then you write the
cross-repo comparison yourself and cite both sets of `query_id`s.

## 6. Pitfalls that have cost sessions

- **Stale binary.** `doctor self-contract` warns "installed binary was built
  from X but the checkout is at Y". Doctor counts, warning counts and verbs
  then disagree with the source. Say which binary ran before drawing
  conclusions; never drive `target/debug/leio-code` when an install exists.
- **Wrong root.** Indexing a workspace member from the workspace root (or the
  reverse) returns plausible, wrong evidence. `status` lists
  `workspace_members`; pin `repo_root` to the tree being edited.
- **Lexical is not proof.** `knowledge adaptive`/`text` are lexical; `graph`
  is structural, not semantic. A fact for the report comes from `knowledge
  explain` (SPARQL-grounded or refused) or from `find`/`graph` with
  `file:line`.
- **Grep before LEIO.** Substring search misses re-exports, glob imports and
  macro-generated call sites. Run `graph callers-of` and `importers-of` before
  touching a public symbol; `rg` afterwards only for an exact literal.

## 7. Known blind spots (observed in trio runs; re-check with `capabilities`)

These are places where LEIO returned nothing or too little for something that
exists. The rule does not change — LEIO first — but when you hit one, run
`rg` for the literal and cite **both**: the `query_id` that proved the miss
and the `file:line` that found it. That pair is also the retroaliment entry.

| Blind spot | Symptom | What to do |
| --- | --- | --- |
| Route ids and other IRI-like literals (`reference:q_roas_lookup`) | `find symbol` → 0 matches although the literal sits in a TTL/JSON/Python file | `rg -n '<literal>'`; the route is data, not a symbol — say so in the report |
| Module-level dict assignments (`RESOLVERS = {...}`) | `find symbol` → 0 | `rg -n '^<NAME>\s*='`, then read the dict |
| Dict-keyed dispatch (`RESOLVERS` indexed by `route.resolves`) | `graph callers-of <fn>` lists other callers but not the live dispatch site | read the dict and the indexing site; the graph has call edges, not reference edges |
| Doctor count in `status` vs `capabilities` | `status` says "all N green" while `capabilities.doctor_kinds` lists M | cite `capabilities`; never quote the count from `status` |
| `status` returns a snapshot, not a query envelope | no `query_id` to cite for it | cite `capabilities`/`context` ids instead |

If a blind spot costs a lane real time, it belongs in `retroaliment.md` §2 as a
golden context task or a LEIO issue — not only in the report.
