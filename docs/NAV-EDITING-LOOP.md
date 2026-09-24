# Navigation as an editing loop

Local stdio MCP and the CLI can locate, inspect, follow and assess indexed impact
without a separate source-file read. Hosted Apps SDK remains stateless graph only.

1. Call `leio_code_context` with an absolute `repo_root` and the concrete task.
   Ranked file candidates are suggestions, not proof of the cause. When an indexed
   definition is available, `next_calls` includes a directly executable nav open.
2. Execute that action, or a candidate's `open` action. Exact symbol URNs come
   from graph evidence; `definition:path#line` anchors come from indexed source
   locations. Never invent a URN. Keep one explicit `session` per agent and root.
3. Inspect `envelope.entities[role=current].source`: signature, bounded text,
   current definition line, file SHA-256, local freshness and index metadata match.
   The default is 40 lines, with a 6,000-byte text bound. `source_offset` and
   `source_lines` retrieve subsequent windows without moving the cursor.
4. Call nav `callees`, `callers` or `neighbors` with `follow: true`. A unique edge
   target is selected and inspected in the same call. Multiple targets remain a
   listing with exact open actions. An optional exact `needle` anchors the edge.
5. Use callers and concrete callsites to assess impact, then run the relevant tests.
   Source and graph observations do not establish runtime behavior or test coverage.

`full: true` returns the complete diagnostic packet. Default context, graph and nav
responses omit duplicate renderings and verbose lattice metadata during graph
walks. Graph listings retain bounded rows and disclose truncation. FCA navigation
retains its own lattice evidence. Returned actions are suggestions, never executed
automatically.

## Freshness and limits

Opening or revisiting a definition reparses only its selected UTF-8 source file
(up to 2 MiB), resolves the definition locally and relocates moved lines. A changed
or missing definition invalidates an existing candidate page; selecting an old
page fails without moving the cursor. The source hash describes the observed file
snapshot. Indexed edges still require index refresh after structural changes.
Source windows may include adjacent definitions and are not full function ASTs.
Paths resolving outside the repository are refused. Ambiguous changed definitions
return `source.state=unavailable` rather than a guessed excerpt.

## Evaluation

Measure concrete tasks: calls to reach the right implementation, response bytes
(and tokens only with an identified tokenizer), required external source reads,
correct edge targets, freshness after edits, and time to a verified fix. Count a
nav invocation as an action, not as success. Compare the same task and source
identity; disclose fixture tests separately from real repository observations.
