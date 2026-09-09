# Drift → retroaliment

A trio run that finds a real disagreement and only writes it in a report has
half-finished. "No silent erosion of provenance" also means no silent erosion
of the *tooling* that guards it: each drift becomes something that fires next
time without a human remembering.

## 1. Name the drift precisely

One line each: what was expected (with its source — a doctor, a contract, a
signed claim, a design doc) vs what was observed (with `file:line` or the
`query_id` / receipt that proved it). "LEIO find symbol returns 0 for
`q_roas_lookup` (`find_symbol-1788478894304497000`) although the literal
exists at `app/cartridge/ontology/reference.ttl:693`" is a drift. "LEIO seems
incomplete" is not.

## 2. Pick the durable form

| Drift is about | Make it durable as | Where |
| --- | --- | --- |
| a repo contract (env var pairs, route ↔ handler, Redis key ownership, egress) | a **doctor**, or a tightening of the doctor that should have caught it | leio-code `src/doctors/<name>.rs`, registered in `src/doctors/mod.rs`; every doctor ships a unit test that plants the violation and sees the warning |
| an induced invariant (FCA-mined implication that held and now does not) | a **baseline entry** the `induced-invariants` doctor validates against | leio-code `baselines/induced-invariants.json` (curated by a human; the miner proposes) |
| LEIO retrieval missing something a task needed (a literal, a dict-dispatch edge, a route id) | a **golden context task** whose `expected_paths_any` names the file LEIO should have ranked | leio-code `benchmarks/context-golden-tasks.json`, re-measured by `leio-bench` |
| behaviour of the code under change | a **regression test** next to the code, encoding *why* (the test must fail if the business rule changes) | the owning repo |
| an Reference Provider boundary (asked period beyond `as_of`, a value quoted without status, a demo profile mistaken for prod) | an **assertion in this skill's evals**, and a note in `govern-reference.md` if the rule was missing | `skills/leio-trio/evals/evals.json` |
| a lane reporting status the runtime did not produce | a **harness doctor** tightening (the family that guards fabricated swarm status) | leio-code `src/doctors/` |

## 3. File it where it can act

- A LEIO gap is a leio-code issue, not a note in the vertical's repo.
- A vertical contract is a doctor in leio-code *about* the vertical, plus a
  test in the vertical.
- Do not self-file a fix into a repo you were told to keep read-only; hand the
  drift line and the durable form to the user as the report's last table.

## 4. Close the loop in the report

The retroaliment table has three columns — drift, where, made durable as — and
the third column is either a path you created or the words "proposed, not
filed" with the reason. Never leave it blank: an empty cell is the drift
eroding silently.
