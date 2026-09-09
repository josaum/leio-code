# Light dataflow — one-hop URL and binary-name resolution

**Status:** Design doc. No code yet. Q3 candidate.

**Goal.** Bridge the gap between literal-only resolution (confidence 95) and
`UnresolvedEdge` for the obvious "I built a string in this function then used
it as a URL or binary name" case.

**Non-goal.** Whole-program data-flow analysis. Cross-function tracing.
Symbolic execution. SMT solving. Anything that requires building a CFG.

---

## The pattern we want to catch

```python
# Today: UnresolvedEdge { reason: TemplateOrConcat }
def fetch_users(base, uid):
    url = f"{base}/users/{uid}"
    return requests.get(url)
```

```python
# Today: UnresolvedEdge { reason: NonLiteralFirstArg }
def fetch_users():
    base = "http://localhost:8080"
    path = "/users"
    url = base + path
    return requests.get(url)
```

```javascript
// Today: UnresolvedEdge { reason: TemplateOrConcat }
function getUser(id) {
  const url = `/users/${id}`;
  return fetch(url);
}
```

```rust
// Today: UnresolvedEdge { reason: NonLiteralFirstArg }
fn fetch_user(id: u64) -> Result<...> {
    let url = format!("/users/{}", id);
    reqwest::get(&url)
}
```

After this PR: all four examples emit a `ResolvedHttpEdge` to `/users/{id}`
or `/users/{}` template route, at confidence 75 (between template-match 70
and normalized-literal 85).

---

## The boundary

**In scope (one hop, same function body):**

1. **Variable assignment from a literal** — `path = "/foo"` followed later by
   `requests.get(path)`. Trace `path` back to its declaration and substitute
   if it's a literal.

2. **f-string / template literal / format! template** — `f"{base}/users/{id}"`,
   `` `${base}/users/${id}` ``, `format!("{}/users/{}", base, id)`. Treat
   the placeholder positions as `{}` for template matching purposes; the
   non-placeholder text becomes a template-route-compatible path.

3. **Single concatenation** — `base + "/users"` where `base` is a
   literal-assigned variable in scope. Same rules as (1).

**Out of scope (always emits UnresolvedEdge):**

- Multi-hop: the URL is passed through a function call before use
- Cross-function: the URL is returned from a helper
- Conditionals: `url = a if x else b`
- Loops: `for path in paths: requests.get(path)`
- Class attributes, module-level constants imported from another file
- Anything involving `.format(**kwargs)` with non-literal kwargs

If the resolution is *ambiguous* (variable could be one of two literals),
emit a single `UnresolvedEdge { reason: AmbiguousAssignment }` rather than
multiple low-confidence resolved edges.

---

## Algorithm sketch

For each `UnresolvedEdge` currently emitted by `detect_*_http_calls` with
`reason ∈ { NonLiteralFirstArg, TemplateOrConcat }`:

1. Identify the **enclosing function body** in the source file. This is
   done by walking backwards from `source_line` until a function/method
   declaration is hit (Python `def`, JS `function`/arrow assigned to const,
   Rust `fn`).

2. Within that body, collect a **literal-binding table**: every assignment
   of the form `<name> = <string-literal>` or `<name> = <f-string-with-only-literals>`
   that appears *before* the call site.

3. **Substitute** the URL expression:
   - If the URL is a bare name, look it up in the table.
   - If the URL is a template, substitute each `${name}` / `{name}` /
     `{}` placeholder with its bound literal value if possible.
   - If any substitution fails (name not in table, or table value is itself
     non-literal), emit `UnresolvedEdge` with the original reason.

4. After substitution, if the result is a **template path** (contains `{}`
   or `{name}`), it matches templated routes via Phase 6's algorithm at
   confidence 75. If the result is a **fully literal path**, it matches
   literal routes at confidence 80 (between 85 normalized and 70 template).

5. Same algorithm applies to subprocess spawn calls: substitute binary
   names from in-function literal assignments. Cap at one hop.

---

## Confidence band

New band: **75 — Dataflow-resolved template** (between Normalized 85 and
Template 70). Distinct band so downstream consumers can filter — "show me
only edges resolved by static dataflow, not by template alone."

| Confidence | Match kind                     | Description                              |
|-----------:|--------------------------------|------------------------------------------|
| 95         | Literal                        | Both sides literal, exact match          |
| 85         | Normalized                     | Same after trailing-slash normalization  |
| 80         | DataflowLiteral *(new)*        | Resolved via in-function literal binding |
| 75         | DataflowTemplate *(new)*       | Substituted template, matched as template |
| 70         | Template                       | Direct template match, no dataflow       |
| 60         | TemplateMethodless             | Template match, method unknown           |

Two new `MatchKind` enum variants: `DataflowLiteral`, `DataflowTemplate`.

---

## File changes

| File                                          | Change                                                     |
|-----------------------------------------------|------------------------------------------------------------|
| `src/cross_language/dataflow.rs` (new)        | `LiteralBindingTable`, `enclosing_function_range`, `substitute`. Per-language: Python, JS/TS, Rust. |
| `src/cross_language/python_http.rs`           | Call `dataflow::resolve_one_hop` before emitting unresolved |
| `src/cross_language/js_http.rs`               | Same                                                       |
| `src/cross_language/rust_http.rs`             | Same                                                       |
| `src/cross_language/python_subprocess.rs`     | Same, for binary name                                      |
| `src/cross_language/js_subprocess.rs`         | Same                                                       |
| `src/cross_language/rust_process.rs`          | Same                                                       |
| `src/model.rs`                                | `MatchKind::DataflowLiteral`, `MatchKind::DataflowTemplate` |
| `src/indexer.rs`                              | INDEX_VERSION bump (12)                                    |
| `tests/cross_language_dataflow.rs` (new)      | 12+ scenarios across the matrix                            |

---

## What this is NOT trying to be

- **Not a tree-sitter rewrite.** Regex-based, line-anchored, function-scoped.
  If the function spans 200 lines with conditionals, the algorithm gives up
  and emits unresolved. That's fine.
- **Not a CFG.** No basic blocks, no SSA, no phi nodes. The literal-binding
  table is a flat scan from function entry to the call site. If a variable
  is reassigned between declaration and use, take the *last* assignment
  before the call — but if that assignment is conditional, emit unresolved.
- **Not type-aware.** Doesn't know that `base` is a `str` vs. an `int`.
  The literal-binding table only tracks bindings where the RHS is *itself*
  a string-shaped literal (or an f-string with only literal interpolations).
- **Not a refactor of the existing detectors.** The dataflow pass is a
  *post-processor* that runs on `UnresolvedEdge`s after the existing
  detectors finish. If it succeeds, it replaces the unresolved with a
  resolved edge. If it fails, the unresolved stays.

---

## Why this matters

Today's `find callers /users/{id}` misses the vast majority of real call
sites because almost no production code calls `requests.get("/users/42")` —
it calls `requests.get(f"{base}/users/{uid}")`. The one-hop substitution
catches the 80% case (`base` is assigned a literal at function entry,
`uid` is a function param representing a runtime value). 

The escape hatch is the new confidence band: if a user wants to be strict,
they filter by `.confidence >= 85`. If they want to be lenient, they
include `.confidence >= 75`. The graph stays honest about the resolution
strategy.

---

## Open questions

1. **JS arrow-function bodies.** `const fetchUser = (id) => fetch(\`/users/${id}\`)`.
   The enclosing-function detection has to handle both `function` declarations
   and arrow assignments. Solvable; just more regex.

2. **Rust `let mut url = String::new(); url.push_str("/users/"); url.push_str(&id.to_string());`**
   This pattern is common but defeats the algorithm. Probably out of scope
   for v1 — emit unresolved. Document the limit.

3. **TypeScript template literals with type assertions.**
   `` fetch(`/users/${id as string}` as string) ``. Strip type assertions
   before substitution? Probably yes; document.

4. **Performance.** The enclosing-function detection requires re-reading
   the file to find function boundaries. Cache the boundaries per file
   alongside `FileRecord.symbols` (already-extracted function symbols carry
   line ranges). Reuse the existing symbol scan output.

5. **Cross-language?** A Python file that assigns `URL = "/api/foo"` to a
   module-level constant, then a Python function in the same file uses it.
   That's a *two-hop* (module → function) — out of scope for v1.

---

## Done when

- One-hop substitution implemented for Python, JS/TS, Rust
- Two new `MatchKind` variants populated
- New confidence band 75 documented in `output-schema.md`
- INDEX_VERSION bumped to 12
- 12+ integration tests cover the matrix (4 languages × 3 patterns + edge
  cases for ambiguity, conditional reassignment, out-of-scope multi-hop)
- README example shows the new resolution working
- ROADMAP marks this Q3 item shipped

---

## Risk

- **False positives.** If a literal binding gets reassigned conditionally
  and the algorithm picks the wrong one, the resolved edge points at the
  wrong route. The 75 confidence band signals "trust, but verify" — but
  consumers may treat it as authoritative.

  *Mitigation:* When the dataflow pass finds *any* conditional reassignment
  between declaration and use, emit `UnresolvedEdge { reason: AmbiguousAssignment }`
  instead of guessing.

- **Regex limits.** Function-boundary detection via regex is brittle for
  nested closures (especially in JS). The fallback is "emit unresolved" —
  but if the regex matches the *wrong* boundary, we get spurious resolution.

  *Mitigation:* Reuse the existing tree-sitter-grade symbol scan
  (`FileRecord.symbols` already has function ranges with confidence) rather
  than rolling new regex.

- **Index size growth.** Every previously-unresolved edge becomes potentially
  resolvable — if the algorithm fires often, the graph balloons. Real
  monorepos have 1000s of HTTP calls.

  *Mitigation:* Profile on `example-workspace/`. If the dataflow pass adds
  >20% to indexing time, gate it behind `--with-dataflow` until a faster
  implementation lands.
