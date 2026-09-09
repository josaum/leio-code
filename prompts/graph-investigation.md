# Graph Investigation Prompt

Start with LEIO `status`, `capabilities`, and `context` for the absolute
`repo_root`. Keep that root and any index pinned throughout the investigation.

1. Choose the narrowest graph query that matches the question:
   - `callers-of`
   - `callees-of`
   - `callsites-of`
   - `symbols-in`
   - `imports-in`
   - `importers-of`
   - `resolved-imports-in`
   - `resolved-importers-of`
2. Resolve ambiguous symbols with `symbols-in` on the known file. Copy the chosen
   row's stable `symbol` URN into later graph needles or local `nav goto`; do not
   silently choose among same-named symbols.
3. For a multi-step local walk, set one explicit MCP nav `session` per agent
   (`--session` on CLI; `LEIO_SESSION` fallback). Callers/callees/neighbors list
   call adjacency. Inspect `role=result` rows, then `select` a returned zero-based
   index before walking further. Use `here`, `back`, and `forward` to track the cursor.
4. Use FCA only when shared indexed attributes help narrow the investigation:
   `parent` is more general, `child` more specific, and `peer` shares a parent.
   Prepare missing lattice artifacts with an intentional `export formal-context`.
   Ordinary graph queries need no formal-context export. FCA cover relations,
   heading links and retrieval similarity are not proof of runtime coupling;
   return to `callsites-of` and source evidence before making an impact claim.
5. Return:
   - answer
   - evidence paths and lines
   - unresolved ambiguity
   - FCA or retrieval leads separately from call/import evidence
   - a relevant bounded `next_calls` entry (`tool`, `arguments`, `reason`) only if needed
6. Do not substitute fuzzy grep for structural evidence. Hosted Apps SDK exposes
   stateless graph queries; use local stdio/CLI for nav. Guide topic `navigation`
   provides the session and selection workflow.
