# FCA induction from `CrossLanguageGraph`

**Status:** Phase A shipped in leio-code (`export formal-context`, streamed
json/arrow, lattice.json v3 + induced.ttl, `nav parent|child|peer|align`,
wiki heading category functor `wiki_heading_to_lattice`).
Phase B/C (example-platform ingest + audit loop) remain cross-repo.

**Goal.** Feed leio-code's `CrossLanguageGraph` edges into `example-platform`'s `FormalContext` so the FCA concept lattice can surface implicit cartridge boundaries, shared infrastructure clusters, and orphan files — automatically, from operational structure rather than human-assigned tags.

**Non-goal.** Reimplementing FCA inside leio-code. The lattice algebra runs
in the pre-built `fca_fast` parser wheel (see `src/fca.rs` and
`artifacts/wheels/`); example-platform ingest remains Phase B.

**Bounded on-demand induction.** Full concept enumeration (Ganter
next-closure) is exponential in the worst case, so on-demand induction only
runs for small contexts (`LEIO_LATTICE_MAX_ON_DEMAND_OBJECTS`, default 5000;
`LEIO_LATTICE_MAX_ON_DEMAND_PAIRS`, default 100000). `nav goto|related` never
induce on the hot path — they use a persisted `lattice.json` when present and
otherwise fall back to index + Arrow hits. Lattice verbs and
`export formal-context` fail with an actionable error on oversized contexts
instead of blocking.

**Heading functor (shipped).** Wiki heading stacks form a category whose
generating morphisms are `Child --subClassOf--> Parent` inside one file.
`section:path#line` objects align to their primary lattice concept. The
alignment is a functor when nested headings remain more specific in the
cover graph. Witness: `lattice.json` field `heading_functor`
(`wiki_heading_to_lattice`). `nav goto` / `nav related` walk that category.

---

## The hypothesis

`CrossLanguageGraph` already knows which files touch which routes, env vars, binaries, redis keys, cartridges, deploy targets. That's an object-attribute incidence relation in everything but name. If we feed it to FCA, the resulting concept lattice should reveal:

- **Implicit cartridges**: clusters of files sharing the same env-var / route / binary footprint that don't share a cartridge label. Likely candidates for a new cartridge — or evidence of cross-cartridge leakage.
- **Shared infrastructure**: features (env vars, redis keys) that appear in many concepts at the top of the lattice. These are the load-bearing primitives the rest of the system depends on.
- **Orphan files**: objects that appear only in singleton concepts. Either dead code or genuinely isolated utilities.
- **Cartridge drift**: files declared in cartridge X but whose feature set is closer to cartridge Y's concept.

None of these require new judgment — they fall out of the lattice algebra `example-platform/example-core` already implements.

---

## The contract

leio-code emits a `FormalContext` instance via a new subcommand:

```bash
leio-code export formal-context --format=json > context.json
leio-code export formal-context --format=arrow > context.arrow      # zero-copy ingest path
```

The output schema:

```json
{
  "schema_version": "1.0",
  "object_kind": "file",
  "objects": [
    "example-gateway/src/app.rs",
    "example-api/example/core/auth.py",
    ...
  ],
  "attributes": [
    "env:REDIS_URL",
    "env:DATABASE_URL",
    "route:/api/users",
    "binary:leio-code",
    "redis:session:*",
    "cartridge:health_audit",
    "deploy_target:backend",
    ...
  ],
  "incidence": {
    "example-gateway/src/app.rs": [
      "env:REDIS_URL",
      "env:DATABASE_URL",
      "route:/health",
      "cartridge:gateway_core"
    ],
    ...
  },
  "provenance": {
    "<obj>|<attr>": {
      "source_path": "...",
      "source_line": 42,
      "edge_kind": "env_var_read"
    }
  }
}
```

The attribute namespace is **prefixed** (`env:`, `route:`, `binary:`, `redis:`, `cartridge:`, `deploy_target:`) so FCA users can filter to subset projections. example-platform can then call:

```rust
let ctx = FormalContext::from_jsonl(reader)?;        // or arrow
let lattice = ctx.compute_lattice(&Config::default());
```

---

## Multiple projections

The default `object_kind` is `file`. But the same underlying graph supports other shapes:

| `object_kind`    | Objects                  | Attributes                              | Question it answers                          |
|------------------|--------------------------|------------------------------------------|----------------------------------------------|
| `file`           | source files             | env vars, routes, binaries, redis keys   | "which files act as one functional unit"     |
| `cartridge`      | cartridge names          | env vars, routes, binaries, redis keys   | "which cartridges share infrastructure"      |
| `binary`         | binary names             | callers (files), env vars they use       | "which binaries serve which subsystems"      |
| `route`          | route paths              | callers (files), framework, method       | "which routes cluster by usage pattern"      |
| `env_var`        | env var names            | files that use them, cartridges          | "which env vars travel together"             |
| `deploy_target`  | deploy target names      | env vars they declare, binaries, routes  | "which deploy targets are functionally equivalent" |

Each projection is a different *concept lattice*. Same algebra, different inputs.

CLI shape: `leio-code export formal-context --object-kind=cartridge`.

---

## File touches

| File                                                      | Change                                          |
|-----------------------------------------------------------|-------------------------------------------------|
| `src/export.rs` (extend existing exporter)                | New `export_formal_context()` function          |
| `src/main.rs`                                             | New `Export { Kind::FormalContext { object_kind, format } }` clap variant |
| `src/jsonld.rs`                                           | New `@type: "FormalContext"` entity mapping     |
| `docs/output-schema.md`                                   | New §X for FormalContext shape                  |
| `tests/export_formal_context.rs` (new)                    | Per-projection tests                            |
| `example-platform/example-core/src/io.rs`                 | New `FormalContext::from_leio_jsonl()` adapter  |
| `example-platform/example-server/src/...`                 | New endpoint `POST /context/import?from=leio-code` |

**Note**: the platform-side adapter (`from_leio_jsonl`) is the cross-repo seam. It can be implemented independently after this design lands — leio-code's export is a stable JSON contract.

---

## Build sequence

1. **Phase A (leio-code side):**
   - Implement `export_formal_context()` reading from existing `RepoIndex`
   - Add the 6 projections (file, cartridge, binary, route, env_var, deploy_target)
   - Add `--format=json` and `--format=arrow` (Arrow path uses the columnar shape so example-platform can ingest zero-copy)
   - Add `provenance` map (every incidence triple carries a `source_path` + `source_line`)
   - Add `tests/export_formal_context.rs`
   
   **Independent of example-platform.** Ships as one PR.

2. **Phase B (example-platform side):**
   - Add `FormalContext::from_leio_jsonl()` and `from_leio_arrow()` constructors
   - Wire into `example-server` as `POST /context/import?from=leio-code`
   - Add `example-cli`: `example-cli ingest leio-code --input context.json`
   
   **Separate PR in the example-platform repo.**

3. **Phase C (loop closure):**
   - `leio-code audit` adds a `--with-fca-induction` flag that POSTs the
     formal context to example-platform's server, retrieves the lattice,
     and surfaces the top-N concepts as a doctor finding.
   - Optional. Doesn't change the contract; just adds an observability path.

---

## Provenance & the named-graph invariant

CLAUDE.md is explicit: "*No silent erosion of provenance. Named graphs, audit trails, and semantic event boundaries must survive transformations.*"

Every `(object, attribute)` pair in the exported context carries:

- `source_path` — the file:line where leio-code observed the relationship
- `source_line`
- `edge_kind` — `"env_var_read"`, `"route_declaration"`, `"binary_spawn"`, `"redis_access"`, `"cartridge_membership"`, `"deploy_target_declaration"`
- `confidence` — passed through from the underlying `ResolvedSpawnEdge` / `ResolvedHttpEdge` / `EnvVarOccurrence` / etc.

This means an FCA concept derived from N triples can be traced back to N specific file:line citations. The lattice never loses provenance through the FCA transform — it just aggregates it.

In Crepe / oxigraph terms: every attribute incidence becomes an RDF triple in a named graph `leio-code:incidence@<commit-sha>`. Concepts derived from the lattice get their own named graph `leio-code:concept:<concept-id>@<commit-sha>` linking back to the source incidences.

---

## What this is NOT trying to do

- **Not replacing the existing FCA pipeline.** The algebra, lattice construction, concept naming, navigator — all in `example-platform/example-core`. leio-code is just a new input source.
- **Not introducing FCA logic inside leio-code.** The export is mechanical: `RepoIndex` → `FormalContext` JSON. No concept computation, no lattice math.
- **Not replacing cartridges.** Cartridges remain the only domain injection point per workspace invariant #3. FCA-derived clusters are *evidence*, not enforcement.
- **Not real-time.** Export is run on demand. The watcher doesn't re-export on every file change.

---

## Open questions

1. **Cartridges-as-objects vs. cartridges-as-attributes.** When `object_kind=file`, cartridge membership is an attribute (`cartridge:health_audit`). When `object_kind=cartridge`, cartridges are objects and their member files become attributes. Both projections are useful but they overlap conceptually. Document the difference, ship both.

2. **Granularity of env-var attributes.** Should `env:REDIS_URL` be one attribute, or should it split by access kind (`env_read:REDIS_URL`, `env_write:REDIS_URL`)? Defer the split until a real use case appears. Default to merged.

3. **Confidence in FCA.** FCA is binary — either the object has the attribute or it doesn't. But `ResolvedHttpEdge` has confidence bands 60–95. Option A: include all edges, treat all confidences as "has the attribute." Option B: threshold at 75+ (drop low-confidence). Option C: emit two contexts (`-strict.json` and `-lenient.json`).
   
   *Tentative answer*: Option A for v1. Confidence travels in provenance; FCA stays binary. Filtering by confidence happens in jq before ingest.

4. **Scale.** The example-workspace has ~10k files. The number of attributes (every env var × access kind × file) could exceed 10k. The lattice has worst-case 2^min(|O|,|A|) concepts, though in practice much smaller. Profile before claiming "production-ready."

5. **The example-platform side: does `FormalContext::from_jsonl` already exist?** Check before designing the adapter. If yes, the leio-code side just emits in that format. If no, this design adds one constructor.

6. **Arrow zero-copy ingest.** example-platform's pipeline uses Arrow `58.3.0`. The leio-code `--format=arrow` path should match. Two columns suffice: `object: string`, `attribute: string` with a row per incidence. Provenance becomes a third column (struct). zero-copy ingest into `FormalContext` is a real win — defer if `from_jsonl` already exists and is fast enough.

---

## Done when (Phase A only)

- `leio-code export formal-context --object-kind=<kind> --format=<json|arrow>` produces a valid context
- All 6 projections implemented
- Provenance carried through to the output
- `tests/export_formal_context.rs` covers each projection + provenance integrity
- `docs/output-schema.md` documents the FormalContext shape
- README example: end-to-end pipeline `leio-code export formal-context | example-cli ingest leio-code`
- ROADMAP marks Q3 #1 (FCA induction loop, leio-code side) shipped

Phases B and C ship separately in their own repos.

---

## Why this matters

The Example thesis (`CLAUDE.md`):

> The domain model is induced from operational traces.
> The graph constrains what is legal.
> The LLM navigates the graph; it does not invent the workflow.

The `CrossLanguageGraph` is one of the **structural priors**. Today it feeds the runtime (via `leio-code audit`, `--format=sarif`, the apps-sdk server). Tomorrow it also feeds the *cognition layer* — `example-platform`'s FCA pipeline turns the static graph into induced concepts that the workflow ontology can constrain against.

That's the loop closing: source code → structural graph → induced concept lattice → workflow constraints → runtime validation. leio-code is the first arrow; example-platform is the second.

The deck I just authored says exactly this on slide 16 ("The induction loop closes"). This design doc is the implementation contract for that promise.
