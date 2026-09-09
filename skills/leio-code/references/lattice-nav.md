# Graph, FCA and heading navigation

Use graph queries for indexed call/import evidence. Use Formal Concept Analysis
(FCA) to discover entities sharing indexed attributes and explore their cover
relations. FCA membership, alignment coherence and shared parents are not proof
of runtime coupling, execution, or semantic equivalence. Verify an impact claim
with callsites, source evidence and relevant execution.

Ordinary graph queries and call navigation do not require formal-context export.
When FCA artifacts are missing, prepare them intentionally with
`leio_code_export({"repo_root":"/abs/repo","kind":"formal-context"})` or
`leio-code --repo /abs/repo export formal-context` before a lattice walk.
When induction succeeds, this writes
`.leio-code/exports/formal-context-v1/lattice.json` (schema v3). Check the returned
`lattice_built` flag; backend availability and induction limits can prevent it.

- Cover morphisms are `subClassOf` (specific → general).
- `parent` lists more general concepts, `child` more specific concepts, and
  `peer` concepts with a shared parent. These are FCA relations, not call edges.
- Identity functor: `identity_cover_to_owl`.
- Wiki heading stacks are a second category: `Child --subClassOf--> Parent` in one file.
- Alignment of `section:path#line` onto primary concepts is `wiki_heading_to_lattice`.
- OWL TBox is `induced.ttl`.

For local stdio `leio_code_nav` / CLI `leio-code nav`:

- `goto` a stable graph symbol URN, symbol name, file, concept id,
  `section:path#line`, `path#line`, or `Root > Child`. To resolve duplicate names,
  query `graph symbols-in <file>` and use the selected row's `symbol` URN in both
  graph needles and nav goto. Copy the returned identifier; do not invent it.
- `callers` / `callees` / `neighbors` list indexed call adjacency.
- `related` lists heading and lattice relations, with retrieval candidates as a
  fallback. A related result does not establish a call edge.
- `parent` / `child` / `peer` list FCA cover relationships.
- Walks update `role=result` rows without moving the cursor. Inspect each row's
  path, symbol and zero-based `index`; `select` that index before the next walk.
  The result index is not the row's offset in the envelope entities array.
- `here` inspects the cursor; `back` / `forward` restore previous selections.
- `align` — both functor scores
- `explain` — SPARQL-grounded proof from the current heading (or a needle); lattice is the trail
- Pin absolute `repo_root`, any `index`, and the same explicit `session` on every
  MCP nav call. Concurrent agents use distinct session IDs. CLI uses `--session`,
  with `LEIO_SESSION` as the environment fallback.
- Session state lives in `.leio-code/nav-session.json`, or
  `.leio-code/sessions/nav-<id>.json` with isolation (`current_concept`,
  `current_section`, `current_iri`). The index/wiki/formal/lattice locks are shared.
- `here` surfaces the pinned IRI on the current entity and in the summary

Exact indexed file paths resolve before fuzzy retrieval: `src/auth.rs`,
`./src/auth.rs`, `file:src/auth.rs`, or an absolute path inside the same repository.
This does not require an Arrow export or lattice. Reindex newly added files first.
Source rows include `line` when known. A concept's `path` is null because its family
is a classification, not a source location.

Concept rows include `concept_details`: a readable label, family, `extent_size`,
bounded `intent` attributes with count/truncation indicators, and representative
member objects. Read the membership basis and sample indicators; these objects
are not a complete enumeration of the extent. Shared attributes remain separate
from call/import evidence.

`envelope.meta.lattice` reports `current`, `stale`, `missing`, `unverified`, or
`invalid`, with a reason and `rebuild_required`. The IRI-only fast path reports
`unchecked`. Current means that the selected index's structural incidences and
live wiki inputs match the artifact fingerprint; it does not replace reindexing
changed source. Older artifacts remain readable but unverified until a real
rebuild. Explicit `export formal-context` rebuilds subject to induction limits
and backend availability; exporting Arrow nodes does not refresh `lattice.json`.

`envelope.meta.result_page` reports `offset`, `limit`, `returned`, `total` (null
when not known), `has_more`, and `next_offset`. Follow the continuation in
`next_calls`, or repeat the same listing query and session with its returned
`next_offset` and unchanged `limit` (1–100). Result indices are local to each page.
`here` preserves a pending page; selection and history moves clear it. Changing
the cursor, index, navigation artifacts or page size invalidates that continuation.
Arrow fallback in paged navigation uses deterministic lexical and FCA ranking;
it does not request remote embeddings. Use context/adaptive retrieval for semantic
discovery, then move to an exact returned identity for a stable walk.

With a connected MCP SDK `client`, replace the repository, file and symbol name
below. This selects an exact graph identity, examines an FCA parent, and returns
to graph evidence. Prepare a missing FCA lattice first as described above.

```js
const repo_root = "/abs/repo";
const session = "agent-auth-review";
const call = async (name, args) => {
  const result = await client.callTool({ name, arguments: { repo_root, ...args } });
  if (result.isError) throw new Error(JSON.stringify(result.content));
  return result.structuredContent;
};
const graph = await call("leio_code_graph", {
  kind: "symbols-in", needle: "src/auth.rs",
});
const symbols = graph.envelope.entities.filter(
  (row) => row.qual_name === "validate_bearer" && row.symbol,
);
if (symbols.length !== 1) throw new Error("Choose one qualified symbol from the inventory");
const needle = symbols[0].symbol; // Stable URN returned by this repository.
await call("leio_code_nav", { session, kind: "goto", needle });
const parents = await call("leio_code_nav", { session, kind: "parent", limit: 5 });
const candidate = parents.envelope.entities.find((row) => row.role === "result");
// Inspect the candidate's symbol and path before choosing it.
if (candidate) {
  await call("leio_code_nav", { session, kind: "select", index: candidate.index });
  await call("leio_code_nav", { session, kind: "back" });
}
await call("leio_code_graph", { kind: "callsites-of", needle });
```

CLI equivalents keep the session before the subcommand. Here `select --index 0`
is valid only when `parent` returned a result with index 0:

```sh
leio-code --repo /abs/repo --session agent-auth-review nav goto '<returned symbol URN>'
leio-code --repo /abs/repo --session agent-auth-review nav parent --limit 5
leio-code --repo /abs/repo --session agent-auth-review nav select --index 0
leio-code --repo /abs/repo --session agent-auth-review nav back
```

Graph and nav may return bounded `structuredContent.next_calls`, each with
`tool`, `arguments` and `reason`. Inspect the suggestion before invoking it and
preserve its repository, optional index and session. Suggestions do not execute
automatically or turn a retrieved relationship into evidence of behavior.

`knowledge compile` also writes `.leio-code/exports/knowledge-v1/formal.nq` (repo RDF + induced OWL + `leio:cites`; RDF-star quoted triples become `rdf:Statement`). `knowledge explain` / `knowledge sparql` / `nav explain` read that graph. Unbound or ambiguous explain is `grounded: false`. `nav explain` first binds IRIs cited by `current_section`, then the pinned
`current_iri`, then identity SPARQL (label/heading), then loose facts.
Explain includes `rdf:Statement` facts and `sources` from `leio:statedIn`.

The hosted Apps SDK has no nav tool; use `graph_repository` for its stateless
graph surface. If the local MCP schema lacks `leio_code_nav`, use the same verb
on the CLI (`LEIO_CODE_BIN`). Guide topic `navigation` explains both surfaces.
