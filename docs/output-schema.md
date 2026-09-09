# LEIO Code — Output Schema

**Schema version: 1.0** — stable contract for the JSON, JSON-LD, and SARIF
shapes returned by every output-producing `leio-code` subcommand.

This document is the **canonical source** for downstream consumers (MCP
wrapper, Apps SDK, Codex plugin, scripts piping to `jq`, oxigraph SPARQL
ingest). Breaking changes follow semver and are gated behind a major
version bump.

MCP hosts wrap this envelope as `CallToolResult.structuredContent.envelope`
under the **2025-11-25** wire contract
([MCP-SPEC-2025-11-25.md](MCP-SPEC-2025-11-25.md)). Tool execution failures
set `isError: true`; they do not replace this CLI schema.

The contract version is emitted at runtime as the `schema_version` field —
the first key of every `QueryEnvelope` and of every diagnostic-format root
document. Downstream consumers can branch on it before parsing the rest of
the payload.

---

## 1. The `QueryEnvelope` (`--json` / `--format=json`)

Every `find`, `explain`, `graph`, `audit`, and `doctor` subcommand returns
a JSON object with this shape:

```jsonc
{
  "schema_version": "1.0",                // contract version; see §7
  "query_id": "find_env-1700000000000",   // <prefix>-<unix_ms_nanos>; the
                                          // prefix is stable, the suffix
                                          // is monotonic per process
  "kind": "find",                         // enum — see §1.1
  "summary": "env var `DATABASE_URL`...", // one-line human summary
  "confidence": 0.9,                      // float in [0.0, 1.0]; ≥0.9
                                          // is "high confidence"
  "entities": [ /* §2 */ ],
  "evidence": [ /* §3 */ ],
  "warnings": [ "string", ... ],          // empty when clean
  "meta": { /* optional, subcommand-specific */ },
  "timing_ms": 7
}
```

### 1.1. `kind` enum

| Value | Emitted by |
|---|---|
| `find` | `leio-code find <kind> <needle>` |
| `explain` | `leio-code explain <kind> <needle>` |
| `graph` | `leio-code graph <kind> [<needle>]` |
| `audit` | `leio-code audit` |
| `doctor` | `leio-code doctor <kind>` |
| `knowledge` | `leio-code knowledge <kind> [<needle>]` |
| `context` | `leio-code context <task>` |

The `kind` is the broad subcommand family. The specific entity kind for
filtering (`env_var`, `redis_key`, `deploy_target`, etc.) is on each
`entities[]` element — see §2.

### 1.2. `confidence` semantics

| Range | Meaning |
|---|---|
| `0.95`–`1.0` | Stable contract result; safe to gate CI on. |
| `0.85`–`0.94` | Stable result; minor edge cases possible. |
| `0.5`–`0.84` | Heuristic. Treat as "for review, not for gating." |
| `0.0`–`0.49` | Low confidence. Show to a human; do not act on. |

The current exit-code mapping in `src/diagnostics.rs` treats `≥ 0.9 ∧
warnings.is_empty()` as `exit 0` (clean).

---

## 2. Entity shapes

`entities[]` is an **array of JSON objects** whose shape depends on the
subcommand. Every entity carries enough fields to be filterable via
`--where '.entities[] | select(.field == "value")'` (see §6).

In JSON-LD output (§5), each entity also carries an `@type` discriminator
that matches the table below.

### 2.1. `Symbol` (`find_symbol`)

```jsonc
{
  "@type": "Symbol",                    // JSON-LD only
  "name": "foo",
  "kind": "function",                   // function|class|struct|enum|trait|...
  "language": "rust",                   // rust|python|javascript|typescript|csharp|razor|go|c|cpp|bash|java|kotlin|html|css|swift|sql|...
  "path": "src/foo.rs",
  "line": 42,
  "qual_name": "Foo::method"            // optional; None for top-level
}
```

### 2.2. `EnvVar` (`find_env_var`, `explain_env_var`)

```jsonc
{
  "@type": "EnvVar",
  "name": "DATABASE_URL",
  "is_secret": false,                   // explain only; from is_secret_key heuristic
  "code_uses": [                        // every code site that references this var
    { "path": "src/db.rs", "line": 12,
      "access": "read", "language": "rust" }
  ],
  "profiles": ["production", "staging"],
  "secret_sets": ["prod_credentials"],
  "value_bindings": [                   // explain only; every source that declares this var
    {
      "state": "set",                   // set|empty|unset
      "display": "postgres://…",        // raw value, or "[redacted, N chars, sha256:<hex>]"
      "redacted": false,
      "source": {
        "kind": "env_file",             // env_file|deploy_profile|secret_set
        "path": ".env.local",
        "precedence": 0                 // lower wins
      }
    }
  ],
  "effective": {                        // explain only; the winning binding (or {state: "unset"})
    "state": "set", "display": "postgres://…", "redacted": false,
    "source": { "kind": "env_file", "path": ".env.local", "precedence": 0 }
  }
}
```

**Precedence ladder** (used by `value_bindings[].source.precedence`):

| Source | Precedence |
|---|---|
| `.env.local` | 0 |
| `.env.development.local`, `.env.production.local` | 1 |
| `.env.development`, `.env.production` | 2 |
| `.env` | 3 |
| `.env.example`, `.env.sample` | 4 |
| `deploy/profiles/*.env` | 10 |
| `deploy/secret-sets/*` | 20 |

**Redaction.** When `is_secret_key(name)` matches (`*_SECRET|*_KEY|*_TOKEN|
*_PASSWORD|*_PWD|*_PRIVATE_KEY|*_CREDENTIAL[S]` plus the bare names
`SECRET|PASSWORD|TOKEN|API_KEY`), `display` is `[redacted, N chars,
sha256:<8 hex>]` and `redacted` is `true`. Passing `--show-secrets`
flips this; the flag is refused outside a TTY without
`--i-know-what-i-am-doing`.

### 2.3. `RedisKey` (`find_redis_key`, `explain_redis_key`)

```jsonc
{
  "@type": "RedisKey",
  "key": "route:exact:onboarding",
  "uses": [
    { "path": "example-gateway/src/...", "line": 55,
      "access": "read", "language": "rust" }
  ]
}
```

### 2.4. `DeployTarget` (`find_deploy_target`, `explain_deploy_target`)

```jsonc
{
  "@type": "DeployTarget",
  "name": "backend",
  "path": "deploy/targets/backend.toml",
  "profile": "production",              // backend_profile from the target
  "secret_set": "prod_credentials",
  "deploy_class": "stateful",
  "topology": "kubernetes",
  "cartridges": ["vigoros", "health_audit"],
  "required_integrations": ["pacto", "jaipay"],
  "health_checks": [ "..." ],
  "smoke_suite": "deploy/smoke/backend.sh",
  "rollback_command": "deploy/rollback/backend.sh",
  "promotion_policy": "manual",
  // NEW (PR follow-up to P1 #5): values for every var declared by
  // backend_profile + secret_set, resolved via the same value-binding
  // pipeline as EnvVar. Schema for each entry matches §2.2's
  // value_bindings[] item.
  "var_bindings": {
    "DATABASE_URL": {
      "is_secret": false,
      "bindings": [ /* same shape as EnvVar.value_bindings[] */ ],
      "effective": { /* same as EnvVar.effective */ }
    },
    "API_KEY": { "is_secret": true, "bindings": [...], "effective": {...} }
  }
}
```

### 2.5. `SubprocessCall` (`find_subprocess_caller`)

```jsonc
{
  "@type": "SubprocessCall",
  "binary": "leio-code",
  "callers": [
    { "path": "scripts/run.py", "line": 12, "language": "python" }
  ]
}
```

### 2.6. `Cartridge`, `ApiRoute`, `DockerService`

See `src/query.rs` for the full enum of `entity_type_for()` (in
`src/jsonld.rs`) — these all follow the same evidence + meta pattern
but expose different fields. Document additions in this section when
new entity kinds land.

### 2.7. `Binary` (`find_binary`, `explain_binary`)

Cross-language binary node from `index.cross_language.binaries` (Cargo
`[[bin]]`, npm `bin`, pyproject `[project.scripts]`).

```jsonc
{
  "@type": "Binary",
  "name": "leio-code",
  "path": "Cargo.toml",                 // declaring manifest, repo-relative
  "source": "CargoExplicit"             // CargoExplicit|CargoImplicit|CargoBin|NpmBin|PyprojectScript
}
```

`explain_binary` returns a mixed-shape entity array. Each entity carries an
internal `@kind` discriminator (`Binary`, `SpawnCallSite`, `UnresolvedEdge`)
so consumers branch inside the array rather than on the envelope-level
`@type`. The envelope `@type` is `ExplainResult` and the `@type` of every
entity is `Binary` (uniformly inherited from the envelope's `query_id`
prefix).

The unresolved-edge filter is **best-effort**: `UnresolvedEdge` carries no
binary-name field, so we match on `raw_snippet.contains(name)`. `meta`
carries `unresolved_filter: "best-effort substring on raw_snippet"` to
make this explicit.

### 2.8. `Route` (`find_route`, `explain_route`)

Cross-language HTTP route from `index.cross_language.routes`
(Flask, FastAPI, Express, axum).

```jsonc
{
  "@type": "Route",
  "route": "/api/users",
  "method": "GET",                      // uppercase HTTP verb, or "*"
  "framework": "flask",                 // flask|fastapi|express|axum
  "path": "server.py",                  // declaring source file, repo-relative
  "line": 12,
  "language": "python"
}
```

`find route` accepts `--method <VERB>` to filter by HTTP method
(case-insensitive). `explain_route` returns the same mixed-array shape as
`explain_binary` with per-entity `@kind` in `{Route, HttpCallSite}`.

### 2.9. `SpawnCallSite` (`find_callers <binary-name>`)

Resolved subprocess spawn caller. Emitted when the `find callers` dispatcher
sees a non-`/`-prefixed needle.

```jsonc
{
  "@type": "SpawnCallSite",
  "caller_path": "scripts/run.py",
  "caller_line": 8,
  "caller_language": "python",
  "callee_name": "leio-code",
  "callee_path": "Cargo.toml",          // declaring manifest of the resolved binary
  "confidence": 95
}
```

### 2.10. `HttpCallSite` (`find_callers <route-path>`)

Resolved client→route caller. Emitted when the `find callers` dispatcher
sees a `/`-prefixed needle.

```jsonc
{
  "@type": "HttpCallSite",
  "caller_path": "src/client.ts",
  "caller_line": 23,
  "caller_language": "typescript",
  "route_path": "/api/users",
  "route_method": "GET",
  "route_source_path": "server.py",     // declaring source file of the resolved route
  "confidence": 95
}
```

---

## 3. `evidence[]`

Each `EvidenceItem` is a stable shape:

```jsonc
{
  "kind": "<rule_id_or_concept>",       // e.g. "env_var", "redis_key", "subprocess_call",
                                        //      "smoke_suite_missing", etc.
  "path": "relative/path.rs",
  "line": 42,                           // null for file-level / target-level findings
  "detail": "human-readable one-liner"
}
```

`evidence[]` is **the contract for doctor violations**: every warning has at
least one matching evidence item with `kind == <rule_id>`. The SARIF/JSON
diagnostic formats (§4) project these into machine-readable form.

---

## 4. Diagnostic formats (`doctor --format=…`)

`doctor` emits SARIF 2.1.0 or a simpler JSON shape on demand. These are
*alternatives* to the envelope output above — they are not nested inside
the envelope, they replace it.

### 4.1. `--format=sarif`

```jsonc
{
  "schema_version": "1.0",             // leio-code output contract; see §7
  "$schema": "https://docs.oasis-open.org/sarif/sarif/v2.1.0/cos02/schemas/sarif-schema-2.1.0.json",
  "version": "2.1.0",                  // SARIF format version (unrelated)
  "runs": [
    {
      "tool": {
        "driver": {
          "name": "leio-code",
          "version": "1.2.0"           // env!("CARGO_PKG_VERSION")
        }
      },
      "invocations": [
        {
          "executionSuccessful": true,
          "properties": {
            "schema_version": "1.0",   // mirror of root-level field
            "index_version": 6,        // from leio_code::indexer::index_version()
            "commit_sha": "<short-sha-or-unknown>"
          }
        }
      ],
      "results": [
        {
          "ruleId": "redis_key_hygiene",
          "level": "warning",          // error|warning|note
          "locations": [
            {
              "physicalLocation": {
                "artifactLocation": { "uri": "src/foo.rs" },
                "region": { "startLine": 42 }
              }
            }
          ],
          "message": { "text": "<detail from EvidenceItem>" }
        }
      ]
    }
  ]
}
```

`locations[]` is **omitted** when the evidence carries no path (e.g. an
envelope-level warning). The SARIF schema accepts this.

### 4.2. `--format=json`

A simpler shape for users who don't want SARIF:

```jsonc
{
  "schema_version": "1.0",
  "meta": {
    "index_version": 6,
    "commit_sha": "<short-sha-or-unknown>",
    "tool_version": "1.2.0"
  },
  "diagnostics": [
    {
      "rule_id": "redis_key_hygiene",
      "severity": "warning",
      "path": "src/foo.rs",
      "line": 42,
      "message": "<detail>"
    }
  ]
}
```

### 4.3. Exit codes

```
0   — clean: no warnings AND confidence ≥ 0.9
1   — violations: at least one warning in envelope.warnings
2   — config error: invalid input (e.g. unknown rule_id with --explain)
64+ — internal tool error (unexpected panic, IO failure, etc.)
```

The exit-code contract is honored by **every doctor subcommand** in every
format mode.

---

## 5. JSON-LD (`find` / `explain` with `--format=jsonld`)

The JSON-LD output is the envelope of §1 + §2 wrapped with:

```jsonc
{
  "schema_version": "1.0",                                      // first key; see §7
  "@context": "https://ontology.getjai.com/leio-code/v1#",                       // stable IRI; does NOT need to resolve
  "@id":      "urn:leio-code:query:<repoId>:find_env-1700000000000",
  "@type":    "FindResult",                                     // §5.1
  // ...all the QueryEnvelope fields from §1...
  "entities": [
    {
      "@type": "EnvVar",
      "fullPath": "/abs/path/.env",
      "@id": "file:///abs/path/.env",
      "gitPath": ".env",
      "branch": "main",
      "head": "<sha>",
      "repo": "urn:leio-code:repo:<repoId>"
    }
  ],
  "checkout": { "repoId": "...", "origin": "...", "worktree": "...", "branch": "...", "head": "..." },
  "http://www.w3.org/ns/prov#used": [ { "@id": "file://...", "fullPath": "..." } ]
}
```

### 5.1. Top-level `@type` mapping

| Envelope `kind` | `@type` |
|---|---|
| `find` | `FindResult` |
| `explain` | `ExplainResult` |
| `graph` | `GraphResult` |
| `audit` | `AuditResult` |
| `doctor` | `DoctorResult` |
| other | `<Kind>Result` (title-case + suffix) |

### 5.2. Entity `@type` mapping

See `entity_type_for()` in `src/jsonld.rs` for the canonical list. Major
entries:

| Subcommand prefix | `@type` |
|---|---|
| `find_symbol` | `Symbol` |
| `find_env`, `explain_env` | `EnvVar` |
| `find_redis`, `explain_redis` | `RedisKey` |
| `find_deploy_target`, `explain_deploy_target` | `DeployTarget` |
| `find_cartridge`, `explain_cartridge` | `Cartridge` |
| `find_api_route` | `ApiRoute` |
| `find_docker_service` | `DockerService` |
| `find_subprocess` | `SubprocessCall` |
| `find_binary`, `explain_binary` | `Binary` |
| `find_route`, `explain_route` | `Route` |
| `find_callers_binary` | `SpawnCallSite` |
| `find_callers_route` | `HttpCallSite` |
| `knowledge_explain` | `KnowledgeSubject` |
| `knowledge_sparql` | `SparqlBinding` |
| `nav` | `NavNode` |
| (anything else) | `Entity` |

Repository-analysis commands also append this JSON-LD document as one NDJSON
line to `.leio-code/events/events.ndjson` (or `LEIO_EVENTS_DIR`). The
`conversation` command deliberately writes no LEIO event journal. Repository
event paths are absolute. `checkout` identifies repo / worktree / branch / HEAD
so journals from clones and worktrees can be merged.

### 5.3. Why JSON-LD?

The annotations let consumers:

- **`jq`-filter cleanly** without re-parsing entity shape per subcommand:
  `.entities[] | select(.@type == "EnvVar")`
- **Ingest into oxigraph** via the JSON-LD → RDF pipeline for SPARQL
  queries spanning multiple `leio-code` invocations.
- **Pipe back through `explain --stdin`** — the entity's `@type` and the
  field-name table in §2 (`name` / `key` / `route`) are the contract
  `leio-code explain --stdin` consumes to dispatch each entity to the
  matching `explain_*`. Output is a JSON array of envelopes (one per input
  entity); unrecognized `@type` or missing field is skipped with a stderr
  warning.
- **Mix with other Example outputs** that share the same vocabulary IRI
  prefix (vocabulary alignment is a follow-up — currently
  `leio-code/ns/v1#` is its own namespace).

---

## 6. The `--where` filter

A tiny jq subset implemented natively (no jq binary dep). Supported:

```
.entities[] | select(<path> == <json-literal>)
.entities[] | select(<path> != <json-literal>)
.entities[] | select(<path> | contains(<string-literal>))
```

`<path>` grammar: `.ident(.ident|[uint])*`. Numbers, strings, booleans,
null, arrays, and objects are valid `<json-literal>` values
(parsed via `serde_json`).

Anything outside this grammar returns `Err("unsupported filter: ...")`.
For richer queries, pipe the JSON-LD output to real `jq` or feed
oxigraph SPARQL.

`--where` implies `--format=jsonld` unless `--format` is set explicitly.
Combining `--where` with `--format=text` is rejected at CLI parse time.

---

## 7. Stability and versioning policy

| Change type | Policy |
|---|---|
| Adding a new field to an entity | Minor version bump. Consumers should ignore unknown fields. |
| Adding a new `@type` (entity kind) | Minor version bump. Consumers branch on `@type` and gracefully skip unknowns. |
| Adding a new envelope `kind` | Minor version bump. |
| Removing or renaming a field | **Major version bump.** Plan: deprecation note in this doc for at least one minor cycle before removal. |
| Changing a field's type | **Major version bump.** |
| Changing the SARIF / JSON-LD top-level shape | **Major version bump.** |
| Adding a new exit code in the documented range (≥ 64) | Minor version bump. |
| Changing the meaning of exit codes 0/1/2 | **Major version bump.** |

The `schema_version` field is emitted as the first key of every envelope
and every diagnostic-format root document (JSON, JSON-LD, SARIF). It is
pinned to `"1.0"` and only changes when this contract changes — it is not
tied to the crate's `Cargo.toml` version. For SARIF specifically,
`schema_version` is *additional* to the SARIF format's own
`"version": "2.1.0"` field, and is also mirrored under
`runs[0].invocations[0].properties.schema_version` for tools that walk
SARIF strictly via the spec's reproducibility-properties slot.

---

## 8. `leio-code audit` — composite report

The audit super-command rolls up status + every registered doctor +
capabilities + best-effort extras into a single document. Unlike every
other output-producing subcommand, **audit does not emit a `QueryEnvelope`** —
the renderer outputs either Markdown (default) or a flat JSON object. The
§1.1 `kind` enum lists `audit` for completeness, but it labels the audit
*family*, not an envelope shape.

**When it produces output:** always. Default is stdout; `--out FILE` writes
to disk instead.

### 8.1. `--format=markdown` (default)

Render order (see `render_markdown` in `src/audit.rs`):

```markdown
# Workspace Audit — <profile> @ <iso-utc>

## Summary
- Result: PASSED | WARNINGS
- Profile: `<workspace_profile>`
- Indexed at: `<iso-utc>`
- Files indexed: <n>
- Deploy targets: <n> | profiles: <n> | secret sets: <n>
- Doctors run: <n> (<m> failing)
- Warnings total: <n>
- Audit timing: <ms> ms

## Verify
- <verify summary text>
- query_id: `verify-<unix_ns>`
- run_all_doctors timing: <ms> ms

| Doctor | Status | Warnings | Evidence | Timing (ms) |
|---|---|---|---|---|
| <name> | ok|warn | <n> | <n> | <n> |
| ... |

## Warnings
### <doctor-name> (<n> warnings)
- <doctor summary>

**Top warnings**
- <warning string>

**Evidence excerpts**
- `<path>:<line>` [<kind>] <detail>

## Capabilities
- Find kinds: `<a>`, `<b>`, ...
- Explain kinds: ...
- Graph kinds: ...
- Export kinds: ...
- Doctor kinds: ... (<n> total)

**Workspace notes**     ← only if non-empty
- <note>

## Extras                ← only when extras are present
- `<name>` (skipped|ran): <note>

## Next steps
- <suggestion>
```

Caps: warning and evidence excerpts are limited to 10 per doctor
(`EVIDENCE_PREVIEW_LIMIT` in `src/audit.rs`). Doctor list is the *runtime*
registry filtered by `workspace_profile` — nothing is hard-coded.

### 8.2. `--format=json`

Flat object (see `render_json` in `src/audit.rs`):

```jsonc
{
  "summary": {
    "workspace_profile": "leio-code",
    "indexed_at": "2026-05-21T12:00:00Z",
    "generated_at": "2026-05-21T12:00:01Z (unix:1747828801)",
    "file_count": 11029,
    "deploy_target_count": 4,
    "profile_count": 3,
    "secret_set_count": 2,
    "doctor_count": 112,
    "failing_doctor_count": 0,
    "warning_count": 0,
    "passed": true,
    "timing_ms": 3421
  },
  "verify": {
    "query_id": "verify-1700000000000",
    "summary": "ran 112 doctor(s); 0 warning(s)",
    "warning_count": 0,
    "timing_ms": 3210
  },
  "warnings": [                    // empty array when passed
    {
      "name": "redis-key-hygiene",
      "summary": "...",
      "warning_count": 3,
      "evidence_count": 5,
      "query_id": "doctor-...",
      "timing_ms": 12,
      "warning_excerpts": ["..."],
      "evidence_excerpts": [ /* EvidenceItem; see §3 */ ]
    }
  ],
  "doctors": [
    { "name": "deploy", "warning_count": 0, "evidence_count": 0,
      "timing_ms": 4, "passed": true }
    /* ... one per doctor ... */
  ],
  "capabilities": {
    "workspace_profile": "leio-code",
    "find_kinds": ["symbol", "env-var", "redis-key", "..."],
    "explain_kinds": ["..."],
    "graph_kinds": ["..."],
    "export_kinds": ["..."],
    "doctor_kinds": ["..."],
    "notes": ["..."]
  },
  "extras": [
    { "name": "graph.dead-code", "status": "ran",
      "note": "found N dead-code candidates …",
      "envelope": { "kind": "graph", "summary": "…" } }
  ],
  "next_steps": ["audit clean for profile `leio-code` — ..."]
}
```

**Field semantics:**

| Field | Type | Description |
|---|---|---|
| `summary.passed` | bool | `failing_doctor_count == 0` |
| `summary.workspace_profile` | string | from `.leio-code/config.toml` |
| `verify` | object | digest of the composite `verify` envelope (run-all-doctors) |
| `warnings[]` | array | one row per doctor that produced ≥1 warning; capped at 10 excerpts each |
| `doctors[]` | array | one row per doctor that ran (alphabetical by name) |
| `capabilities` | object | mirrors `leio-code capabilities` JSON |
| `extras[]` | array | best-effort follow-ups (e.g. graph dead-code); `status: "ran"` when `query_dead_code` succeeds, `skipped` only if it errors |
| `next_steps[]` | array | recommendation lines (synthesized from `warnings` + `capabilities.notes`) |

### 8.3. `--strict` exit semantics

`--strict` is a separate contract from §4.3 doctor exit codes:

```text
0   — audit ran (always, when --strict is not set, even with warnings)
0   — --strict set AND summary.passed == true
1   — --strict set AND summary.passed == false (any doctor warning)
```

The non-zero path goes through `anyhow::bail!` (see `Command::Audit` in
`src/main.rs`), so the stderr message is human-readable, not JSON. Strict
mode does *not* alter the rendered body; the report is written first, then
the process exits.

**Example:**

```bash
$ leio-code audit --strict --format markdown --out /tmp/audit.md
# stderr: audit: wrote /tmp/audit.md (4821 bytes)
# stdout: (empty)
# exit:   0  (or 1 if --strict and any doctor warned)
```

**Stability:** stable. The audit super-command is the canonical
pre-deploy gate (see `CLAUDE.md` §"LEIO Code patterns"). Adding a new
section is backward-compatible; renaming or removing one of the top-level
keys (`summary`, `verify`, `warnings`, `doctors`, `capabilities`, `extras`,
`next_steps`) is a §7-style major change.

---

## 9. `leio-code export <kind>` — file outputs

Each `export` subcommand writes one or more files under
`.leio-code/exports/<kind>-v<version>/` and returns a `QueryEnvelope`
(`kind: "export"`) describing the artifacts. Two contracts per export:

- **Envelope contract** — the standard §1 shape; stable.
- **File contract** — versioned via a sidecar `manifest.json` (where
  applicable). A version bump is a breaking change for downstream
  consumers of the on-disk format.

`--output-dir` overrides the default location and adds a
`"non-default output directory used"` warning to the envelope so
downstream automation can detect non-default writes.

### 9.1. `export formal-context`

**Default output dir:** `.leio-code/exports/formal-context-v1/`

**Files written:**

| File | Format | Shape |
|---|---|---|
| `objects.jsonl` | JSONL | one `FormalContextObject` per line: `{id, kind, label, path?, line?, language?, metadata{}}` |
| `attributes.jsonl` | JSONL | one `FormalContextAttribute` per line: `{id, label, category}` |
| `incidences.jsonl` | JSONL | one `FormalContextIncidence` per line: `{object_id, attribute_id}` |
| `manifest.json` | JSON | `FormalContextManifest` — `version: 1`, indexed/exported timestamps, file paths, counts, `object_kinds{kind: count}` histogram |

This is the input to `fca-fast` for lattice induction. See
[`docs/fca-induction-design.md`](fca-induction-design.md) for the
incidence-builder contract and the design rationale.

**Envelope (excerpt):**

```jsonc
{
  "schema_version": "1.0",
  "kind": "export",
  "query_id": "export-<unix_ns>",
  "summary": "exported formal context with N objects, M attributes, K incidences -> <dir>",
  "confidence": 0.97,
  "entities": [{
    "version": 1,
    "repo_root": "<root>",
    "output_dir": "<dir>",
    "manifest": "<manifest.json path>",
    "objects": N, "attributes": M, "incidences": K,
    "object_kinds": { "symbol": 12345, "env_var": 234 }
  }],
  "evidence": [ /* 4 EvidenceItem rows, one per artifact */ ],
  "warnings": [],
  "timing_ms": 412
}
```

### 9.2. `export code-graph`

**Default output dir:** `.leio-code/exports/code-graph-v1/`

**Files written:**

| File | Format | Shape |
|---|---|---|
| `graph.nq` | N-Quads | RDF graph: stable `symbol` URNs, revision-keyed `symbol-occurrence` URNs; predicates include `rdf:type`, `prov:specializationOf`, `containsSymbol`, `importsPath`, `callsName`, and resolved `calls` edges. Default vocabulary `https://example.local/leio/code#`; override with `LEIO_CODE_RDF_NAMESPACE` or `[rdf] namespace`. |
| `query-cache.json` | JSON | flat `CodeGraphQueryCache` for `crate::graph_query` hot-path lookup (see `src/code_graph.rs`): `symbols{}`, `files{}`, `file_imports{}`, `callers_by_symbol{}`, `callees_by_symbol{}`, `callsites_by_symbol{}`, `file_symbols{}`, plus name/qual-name reverse indexes. Keyed on `revision`. |
| `manifest.json` | JSON | `version: 4`, `query_cache_version: 6`, `repo_root`, `indexed_at`, `exported_at`, `revision`, `graph_iri` (e.g. `urn:leio:graph:code:<repo>:<revision>`), file/symbol/occurrence/import/call counts, `language_counts{}`, `parse_warning_count`, `source_fingerprint` (sha256 over file path+mtime+language tuples), `code_namespace`. |

Languages parsed: Rust, Python, JavaScript, TypeScript, TSX. Files in
other languages are skipped.

**Envelope (excerpt):**

```jsonc
{
  "schema_version": "1.0",
  "kind": "export",
  "query_id": "code-graph-<unix_ns>",
  "summary": "exported code graph with N files, M symbols, K calls (R resolved) -> <dir>",
  "confidence": 0.93,
  "entities": [{
    "version": 3,
    "repo_root": "<root>",
    "revision": "<git-sha-or-workspace>",
    "graph_iri": "urn:leio:graph:code:<slug>:<rev>",
    "output_dir": "<dir>",
    "manifest": "<...>",
    "graph_path": "<...>/graph.nq",
    "query_cache_path": "<...>/query-cache.json",
    "parsed_files": N, "symbols": M, "occurrences": "...", "imports": "...",
    "calls": K, "resolved_calls": R,
    "language_counts": { "rust": 1234, "python": 567 }
  }],
  "warnings": [ /* per-file parse warnings, if any */ ]
}
```

### 9.3. `export arrow-nodes`

**Default output dir:** `.leio-code/exports/arrow-nodes-v1/`

**Files written:**

| File | Format | Shape |
|---|---|---|
| `nodes.arrow` | Arrow IPC stream | `RecordBatch`es of LEIO node rows (batch size 200). Schema comes from `build_leio_row_batch` in `src/node_rows.rs`; deterministic SimHash vectors. |
| `manifest.json` | JSON | `ArrowNodesManifest` — `version: 1`, indexed/exported timestamps, `rows_path`, `row_count`, `batch_count`, `kind_counts{kind: count}`, optional `fca` (rows_tagged / concepts). This Arrow file is the zero-copy FCA+vector store for local search. |

The exporter runs an in-place FCA enrichment pass before writing — each
row's `relations` array is tagged with `fcaConcept:`, `fcaFamily:`,
`fcaIntent:`, and `fcaParent:` markers keyed by `metadata.logical_id`.
Set `LEIO_FCA_ENRICH=0` to skip enrichment (e.g. for benchmarking); the
envelope `meta.fca.enabled` reflects whether enrichment ran.

**Envelope `meta`:**

```jsonc
{
  "transport": "arrow_ipc",
  "fca": {
    "enabled": true,
    "rows_tagged": "<n>",
    "tags_added": "<n>",
    "concepts": "<n>",
    "input_pairs": "<n>",
    "skipped_reason": null
  }
}
```

### 9.4. `export hypergraph`

**Default output dir:** `.leio-code/exports/hypergraph-from-formal-context-v1/`

**Files written:**

| File | Format | Shape |
|---|---|---|
| `hypergraph.json` | JSON | single document with `schema: "example.formal_context_hypergraph.v1"`. Contents: `source{}`, `counts{}`, `views{attribute_centric_hyperedges, object_centric_hyperedges, pairwise_incidences}`, `vertices[]` (each with `vertex_kind: "object"\|"attribute"`), `semantic_tooling{}` provenance pointers. **No sidecar manifest** — the schema field inside the file is the version pin. |

Built from the same `build_formal_context` pass as §9.1, so the
incidence set is identical on a given index. `meta.fca_fast` carries
structural-lattice stats (input pairs, distinct primary concepts,
membership object count) over the non-`symbol:` subset.

**Stability summary for `export`:**

- Envelope shape: stable (matches §1).
- File schemas: each version is stable for its lifetime; a manifest
  `version` bump is a breaking change for downstream consumers and
  should be coordinated with the `fca_fast` wheel and oxigraph ingest.
- Non-default `--output-dir` always emits the
  `"non-default output directory used"` warning so CI gates can detect it.

---

## 10. `leio-code knowledge` — search results

The family emits a `QueryEnvelope` whose `kind` depends on the
subcommand (the §1.1 table lists it under the broad `knowledge`
family — the actual envelope `kind` values are below).

### 10.1. `knowledge` subcommand kinds

| CLI | Envelope `kind` | What it returns |
|---|---|---|
| `knowledge adaptive <q>` | `knowledge_search` | 4-step cascade over compiled markdown/wiki: title → topic → text → hybrid vector; strategy that fired is in `meta.search.strategy` and as a `search_strategy: <name>` warning prefix |
| `knowledge text <q>` | `knowledge_search` | lexical search across title/body/topic/source_path |
| `knowledge status` | `status` | scoped collection health + per-topic / per-article breakdown |

### 10.2. Entity shape — knowledge rows (`KNOWLEDGE_OUTPUT_FIELDS`)

```jsonc
{
  "id": "<chunk-id>",
  "kind": "wiki_chunk",
  "title": "...",
  "body": "<markdown excerpt>",
  "metadata": { /* opaque */ },
  "source_segments": ["..."],
  "topic": "deploy",
  "source_path": "docs/...",
  "_search": {
    "stage": "title|topic|text|vector|article_max_sim",
    "score": 0.93
  }
}
```

### 10.3. Common warning prefixes

The family adds structured warnings that consumers can pattern-match
on:

| Prefix | Meaning |
|---|---|
| `search_strategy: <name>` | which stage of the cascade produced the result set (knowledge adaptive only) |
| `relation_hint_unused: ...` | text query carried an `fcaFamily:` / `fcaConcept:` token that didn't narrow results |

**Stability for knowledge:**

- Envelope `kind` values (`knowledge_search`, `status`) — stable.
- `KNOWLEDGE_OUTPUT_FIELDS` projections — stable; individual fields are
  append-only.
- `entities[].metadata` and `entities[].relations` contents —
  **unstable-but-aspires-to-stable**; consumers should treat unknown
  `relations:` prefixes as opaque rather than failing.
- `_search.stage` / `_search.score` — present only when ranking ran;
  stage values are open-ended (new strategies add new labels).
- `meta.search` and the `search_strategy:` warning prefix — stable
  enough to gate on, but new prefixes may appear (treat the set as open).

---

## 11. `leio-code context` — task bundle

Builds a ranked, bounded working set for a natural-language task. The
ranking pipeline, intent routes, IDF weighting, and optional graph
proximity boosts are documented in
[`docs/CONTEXT_BUNDLE.md`](CONTEXT_BUNDLE.md) — this section pins the
*output shape*, not the algorithm.

**Diet default vs `--full`.** The default bundle is compact: zone items
are `{kind, source_field, path, line}` references (the pointed-to section
carries the payload), per-file `symbols`/`env_vars`/`redis_keys` are
capped at 3 with `*_count` totals alongside, envelope `evidence` keeps
file/instruction/anchor items only, and `meta` omits the workspace
capabilities block. `context --full` restores the exhaustive shape
(uncapped per-file lists, full zone items, duplicated legacy keys,
capabilities in meta). Measured effect: ~56–60% smaller default bundles.
`--json` output is compact (single line) for all commands — machines
re-parse it; the human renderer is the default text mode.

**When it produces output:** always. Single-entity `QueryEnvelope` with
`kind: "context"`; the bundle lives in `entities[0]`.

### 11.1. Envelope

```jsonc
{
  "schema_version": "1.0",
  "query_id": "context-<unix_ns>",
  "kind": "context",
  "summary": "context bundle for `<task>`: F files, S symbols, T tests, D doctor suggestions, I instruction docs, A anchors",
  "confidence": 0.86,
  "entities": [ /* §11.2 — exactly one entry */ ],
  "evidence": [ /* per-file, per-symbol, per-env, per-redis, per-instruction, per-anchor */ ],
  "warnings": [],
  "meta": {
    "limit": 8,
    "workspace_profile": "leio-code",
    "workspace_capabilities": { /* mirrors `leio-code capabilities` */ },
    "next_tools": [
      "leio_code_context", "leio_code_find", "leio_code_graph",
      "leio_code_explain", "leio_code_doctor"
    ]
  },
  "timing_ms": 142
}
```

`confidence` drops to `0.45` when no files and no symbols matched.

### 11.2. Bundle (`entities[0]`)

```jsonc
{
  "task": "<verbatim task string>",
  "tokens": ["<lowercased, stopword-filtered task terms>"],

  "selection_policy": {
    "strategy": "hybrid_sparse_idf",
    "reranker": "rrf_k60+graph_proximity",
    "limit": 8,
    "files_considered": 11029,
    "intent_routes_considered": ["tests", "deploy"]
  },

  "retrieval_signals": {
    "sparse_idf_weighting": true,
    "intent_routes_considered": ["tests", "deploy"],
    "identifier_needles_extracted": 3,
    "graph_cache_loaded": true,
    "code_graph_refresh_hint": null,
    "signal_stack": "idf+intent+identifiers+rrf+graph_proximity"
  },

  "context_zones": [
    { "zone": "instructions", "items": [/* refs */] },
    { "zone": "memory",       "items": [/* refs */] },
    { "zone": "anchors",      "items": [/* refs */] },
    { "zone": "files",        "items": [/* refs */] },
    { "zone": "graph",        "items": [/* refs */] },
    { "zone": "verification", "items": [/* refs */] },
    { "zone": "risks",        "items": [/* refs */] }
  ],

  "instruction_sources": [ /* same as agent_instructions */ ],
  "memory_sources":      [ /* same as memory_banks */ ],
  "agent_instructions":  [ { "path": "AGENTS.md", "reason": "..." } ],
  "memory_banks":        [ { "path": ".leio-code/memory.md", "reason": "..." } ],

  "files_to_read": [
    {
      "path": "src/foo.rs",
      "language": "rust",
      "score": 142,
      "modified_unix_ms": 1700000000000,
      "reasons": ["path:foo", "symbol:Foo", "intent_route:tests"],
      "symbols":   [ /* matched symbol JSONs */ ],
      "env_vars":  [ /* matched env-var JSONs */ ],
      "redis_keys":[ /* matched redis-key JSONs */ ]
    }
  ],
  "symbols":         [ /* ScoredEntity.value, see model.rs */ ],
  "env_vars":        [ /* ... */ ],
  "redis_keys":      [ /* ... */ ],
  "deploy_targets":  [ /* ... */ ],
  "verification_anchors": [
    { "path": "tests/foo.rs", "line": 42, "anchor": "fn test_foo" }
  ],

  "graph_queries": [
    { "verb": "callers-of", "needle": "Foo::method", "reason": "..." }
  ],
  "execution_loop": [
    "run leio-code doctor <kind>",
    "run cargo test <test>"
  ],
  "tests_to_run": [ { "path": "tests/foo.rs", "reason": "..." } ],
  "doctor_suggestions": [ { "kind": "redis-key-hygiene", "reason": "..." } ],
  "risk_notes": [ "touches deploy/profiles/*.env — review before merge" ]
}
```

### 11.3. Field semantics

| Field | Type | Description |
|---|---|---|
| `task` | string | verbatim CLI argument |
| `tokens` | string[] | task after camelCase split, stopword removal, lowercasing |
| `selection_policy` | object | how files were ranked + how many were considered |
| `retrieval_signals` | object | which signal classes fired; `code_graph_refresh_hint` is a string suggesting `cargo run -- export code-graph` when no cache is loaded |
| `context_zones[]` | array | reading order — top zones are strongest. Layout mitigates "lost in the middle" when the consumer clips |
| `files_to_read[]` | array | strongest-first; `reasons[]` are stable strings describing why each file made the cut |
| `verification_anchors[]` | array | test functions / spec headings near top-ranked files |
| `graph_queries[]` | array | follow-up `leio-code graph` commands the agent should consider |
| `execution_loop[]` | string[] | one-line commands forming the recommended edit→verify loop |
| `risk_notes[]` | string[] | secret/deploy/redis-key sensitivity warnings about the top files |

### 11.4. Example

```bash
$ leio-code context "fix the redis key for onboarding session expiry" --limit 5 --json
# emits the envelope above; entities[0].files_to_read leads with files
# matching `redis` + `onboarding` intent routes
```

**Stability:**

- Envelope shape — stable.
- Top-level bundle keys (`task`, `tokens`, `selection_policy`,
  `retrieval_signals`, `context_zones`, `files_to_read`, `symbols`,
  `env_vars`, `redis_keys`, `deploy_targets`, `verification_anchors`,
  `graph_queries`, `execution_loop`, `tests_to_run`,
  `doctor_suggestions`, `risk_notes`, `agent_instructions`,
  `memory_banks`, `instruction_sources`, `memory_sources`) — stable.
- `files_to_read[].reasons[]` — string set is **append-only**; new reason
  labels may appear as new intent routes ship.
- `selection_policy.reranker` / `retrieval_signals.signal_stack` — string
  values are **unstable-but-aspires-to-stable**; they encode the active
  ranker stack and change when ranking logic changes.
- `meta.next_tools[]` — stable hints for chained MCP/CLI flows.

See [`docs/CONTEXT_BUNDLE.md`](CONTEXT_BUNDLE.md) for the ranking pipeline
in detail.
