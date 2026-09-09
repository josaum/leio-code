# Trio report — <goal>

**Rung:** <1 trivial | 2 scoped | 3 cross-repo/multi-lane> · **Tier:** <none | A in-session | B harness day> · **Date:** <YYYY-MM-DD> · **Repo(s):** `/abs/repo @ <sha>`

## 0. Answer in three lines

- **<Governed (signed) | Interpretation (unsigned) | Not known>:** <the answer the user asked for, with its id>
- **<caption>:** <the second thing they need to know>
- **Decision boundary:** <what this authorises — usually "nothing by itself">

## 1. LEIO Code — code ground truth

| Repo root | Index age | Doctor baseline (Ground) | Doctors after change/integration | Delta |
| --- | --- | --- | --- | --- |
| /abs/repo | <age> | <N/M with warnings> | <N/M> | <none | +warning: doctor → file> |

Query ids cited: `<query_id>`, `<query_id>`, … (every one resolves in `<repo>/.leio-code/events/events.ndjson`; `trio_receipts.py` checked)
Code-view fingerprint: `<from codeview check>` (fresh: yes/no)
Key evidence: `<file:line>` — <what it proves> · blind spots hit: <none | literal found by rg after `find` returned 0 (`query_id`)>

## 2. Reference Provider — governed business truth

> Runtime: `<runtime.profile>` · production_ready: `<bool>` → **<demo evidence | production evidence>**
> (or: "No governed fact in scope — Reference Provider not consulted." and delete the table)

| Question | Route | Match | Claim id | Value | Status | as_of | Source |
| --- | --- | --- | --- | --- | --- | --- | --- |
| <q> | `reference:q_…` | exact / semantic-validated | `cl:…` | <v> | signed / unsigned / unknown | <date> | <feed> |

Receipt: `res:…` · Snapshot: `snap:…` · Verify: `<verified | rejected>` (event `<id>`, `host_prose_signed: false`)
Not governed (no-signed-route): <questions, if any> — reasoned in §4 as unsigned.

## 3. LEIO Harness — execution truth

Tier B: DayReport passed <n> / failed <n> · manifest `<outputDir>/manifest.json` (<k> artifacts, SHA-256 verified) · integration `integration/<id>` merged=<bool> conflict=<none|…> · verdict **<Improved | NoImprovement | Regressed | Collapsed>** → target <untouched | promoted> · bus <addr> · embeddings <model | one-hot fallback>

| Lane | Shape | Runtime status | Critic state | Reason | Ids resolved | Side-effect diff | Branch | Duration |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| <agentId> | explorer | ok / failed (LaneOutcome) | Committed / Aborted | — / <typed> | <n/n> | clean / <finding> | <branch> | <ms> |

Tier A: "Runtime status" is the returned block's status; critic columns are yours.
Tier none: **"No lanes ran; no DayReport, manifest, branches or verdict exist and none are claimed."**

## 4. Synthesis

<Every sentence carries a caption. Bullets, one claim each:>
- **Governed (signed):** …
- **Interpretation (unsigned):** …
- **Not known:** …
- **Unsigned host reasoning:** …

## 5. Decision and boundaries

What this report authorises: **nothing by itself.** Sends, promotions, merges into the target and deploys need a fresh human message naming the exact action.
Premise check: <the user's premise held | was refuted: <what was actually there>, scope adjusted to …>
Open uncertainties: …

## 6. Drift → retroaliment

| Drift found | Where | Made durable as |
| --- | --- | --- |
| <expected vs observed> | <file:line / query_id / receipt> | doctor `<name>` / test `<path>` / golden task / baseline entry / **proposed, not filed: <why>** |

## 7. Receipts index

- LEIO: `<query_id>` … (repo, events file)
- Reference Provider: receipt `res:…`, snapshot `snap:…`, claims `cl:…`, verify event `…`
- Harness: `manifest.json` path + count, lane branches, `DayReport` path — or "none"
- Code: `<repo> @ <sha>`; cited `file:line`s

## 8. Side effects disclosed

`trio_sideeffects.py diff` verdict: <clean | findings>. LEIO appended PROV lines under `.leio-code/` (expected). Reference Provider persisted gateway events server-side (expected). Anything else: <list, or "none">.
