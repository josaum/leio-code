# Repository-Local Doctor Packs Design

**Date:** 2026-08-19

**Status:** Approved in chat; awaiting review of this written specification

## Summary

LEIO Code currently compiles repository-specific doctors into the LEIO binary. A repository policy change therefore requires coordinated edits to LEIO's Rust registry, CLI enumeration, MCP schema, and Apps SDK schema, even when the policy applies only to one target repository.

This design introduces safe, declarative doctor packs stored in the repository they validate:

```text
<repository>/.leio-code/doctors/<doctor-name>.toml
```

LEIO remains the execution engine. The target repository owns the policy specification, severity decisions, paths, and tests. The first migration moves `office-parsers-clippy-gate` from compiled LEIO code into `/Users/josaum/projects/example-workspace/.leio-code/doctors/office-parsers-clippy-gate.toml`. It preserves existing severity classifications and introduces one approved tightening that the current Example workspace already satisfies: the strict Clippy fragments must occur inside the `office-parsers-clippy` target, and that target must include `--all-targets`.

Version 1 is intentionally bounded. It performs static, repository-local checks only. It does not execute shell commands, invoke build tools, access the network, expand environment variables, or load native plugins.

## Goals

1. Allow a repository to create and maintain its own LEIO doctors without recompiling LEIO.
2. Preserve the existing targeted CLI shape:

   ```bash
   leio-code doctor office-parsers-clippy-gate --repo /path/to/repository
   ```

3. Expose repository-local doctor names through runtime capabilities, MCP, and the Apps SDK.
4. Preserve structured `QueryEnvelope` output, evidence provenance, severities, strict-mode behavior, and machine-format diagnostics.
5. Keep `all`, `baseline`, and `ci` behavior explicit and deterministic.
6. Migrate `office-parsers-clippy-gate` with equivalent severity behavior, the explicit target-scoping and `--all-targets` tightening, and focused regression coverage.
7. Treat repository content as untrusted input and prevent doctor packs from becoming an execution or path-traversal mechanism.

## Non-Goals

Version 1 will not:

- Execute Make, Cargo, shell scripts, binaries, or arbitrary repository code.
- Support JavaScript, Python, Rust, WASM, or native doctor plugins.
- Fetch remote files or resolve URLs.
- Expand environment variables in paths or expected values.
- Provide user-defined regular expressions.
- Automatically add local doctors to the fixed `baseline` or `ci` suites.
- Migrate every existing Example-specific compiled doctor.
- Modify Example's Makefile, `scripts/verify-ci.sh`, or operations-health wiring.
- Install or publish a new LEIO binary.

## Current Problem

The Rust doctor library already dispatches a single doctor by string name, but the public CLI parses that name through a compile-time `DoctorKind` enum. Unknown repository-local names are rejected before library dispatch.

Doctor registration is also mirrored across multiple surfaces:

- The Rust registry defines compiled doctors and profile applicability.
- The CLI has a second exhaustive enum-to-name mapping.
- The stdio MCP server has a hard-coded doctor-kind array.
- The Apps SDK has a separate hard-coded doctor-kind array.

Capabilities already derive compiled doctor names dynamically from the Rust registry. The new design extends that runtime authority to include validated local packs and removes broad duplicate transport allowlists.

## Alternatives Considered

### Repository-local declarative packs

This is the selected approach. It keeps policy beside the source it validates, requires no repository-specific LEIO rebuild, and provides a narrow security boundary.

Its cost is a small assertion language that LEIO must version and maintain. The language is deliberately limited to operations required by the first real migration.

### Repository-local executable scripts

LEIO could discover scripts and execute them as doctors. This offers maximum flexibility but turns ordinary repository inspection into arbitrary process execution. Scripts could read credentials, mutate files, use the network, or behave differently across platforms. Correct sandboxing is outside the scope of this feature.

### Compiled Rust or WASM plugins

Plugins would support complex logic but introduce build distribution, ABI or runtime compatibility, signing, sandboxing, and lifecycle concerns. They would also preserve the unwanted requirement that doctor policy be compiled before use.

## Architecture

The implementation has five focused units.

### 1. Pack discovery

The discovery layer scans only:

```text
<repository>/.leio-code/doctors/*.toml
```

Discovery is deterministic and sorted by filename. Files in nested directories, hidden temporary files, and non-TOML files are ignored.

The canonical repository root anchors discovery. LEIO opens `.leio-code/doctors` component by component without following symlinks. The doctor directory must be a real directory, and each manifest must be a regular file rather than a symlink, FIFO, socket, or device. The same no-follow rule applies while expanding Cargo workspace members; expansion never descends through a symlinked directory.

Each filename stem must match:

```text
[a-z0-9][a-z0-9-]{0,63}
```

The filename stem and the manifest's `name` field must match. The names `all`, `baseline`, and `ci` are reserved. A local name that collides with a compiled doctor is invalid; a local pack never shadows a compiled doctor.

Discovery returns both valid packs and explicit diagnostics for invalid pack files. Invalid files are not silently dropped.

### 2. Schema parser

Each doctor file uses a versioned TOML schema:

```toml
schema_version = 1
name = "example-contract"
description = "Validate an example repository contract."
suites = ["all"]

[[checks]]
id = "required-anchor"
kind = "file-contains"
path = "src/example.rs"
contains = "required_symbol"
severity = "warning"
```

Top-level fields:

| Field | Required | Meaning |
| --- | --- | --- |
| `schema_version` | yes | Must equal `1` in this implementation. |
| `name` | yes | Stable doctor slug; must match the filename. |
| `description` | yes | Human-readable doctor purpose. |
| `suites` | no | May contain only `all`; defaults to an empty list. |
| `checks` | yes | Non-empty ordered list of assertions. |

Each check has a unique slug-shaped `id`, a supported `kind`, assertion-specific fields, and a severity of `warning` or `info`. Unknown fields are rejected so misspellings do not weaken a contract silently. The Cargo feature-coverage assertion additionally has `integrity_severity = "warning"`; discovery read, traversal, and parse failures use that severity, while uncovered-package adoption findings use the ordinary check severity.

`warning` contributes to the envelope warning count and strict-mode failure. `info` appears in structured informational findings and never contributes to strict failure.

### 3. Safe evaluator

The evaluator receives a validated specification, an authorized repository target, and an explicit content-source mode. It produces the same `QueryEnvelope` family used by compiled doctors.

Hosted Apps SDK evaluation always reads the selected immutable repository revision. It never reads ignored, untracked, generated, neighboring-worktree, or deployment-local files from the server filesystem.

Local CLI and same-user stdio MCP evaluation use a documented `working-tree-tracked` mode so doctors can validate uncommitted source edits. Existing files whose contents are read must be regular files known to Git and not ignored. An existing directory tested by `path-exists` is eligible only when the selected revision contains that tree or the local Git index contains at least one tracked descendant. A nonexistent path may reach assertion evaluation and produce an ordinary missing-path finding when its normalized path and nearest existing ancestor satisfy the containment and deny rules; it is never opened or treated as readable content. A path equal to or beneath `.git`, `.leio-code`, `profiles`, `secrets`, or `deploy/secrets` is denied; the manifest currently being parsed is the sole `.leio-code` read exception. Ignored files, untracked files, and special files are ineligible as content inputs. A working-tree tracked file may contain local sensitive edits, so this mode is a same-user local trust surface and is never selected by the hosted Apps SDK. Version 1 has no pack-declared or transport-exposed override that broadens the eligible input set.

Every input path must be relative. Absolute paths and paths containing a parent traversal component are rejected during schema validation. Paths are opened through one component-aware, root-anchored, no-follow primitive and read through the validated handle rather than reopened by pathname. Containment comparisons operate on path components, never string prefixes. For a nonexistent target, the evaluator validates the nearest existing ancestor without following symlinks and then reports the target as missing. Symlink targets that escape or alias another tree are rejected, including symlinks swapped during validation.

The evaluator places these fields in every check result:

- Doctor name.
- Check identifier and kind.
- Severity.
- Pass or fail status.
- Repository-relative source path.
- Line number when a textual match supplies one.
- Bounded human-readable reason that never includes file contents, a matched line, or the expected literal.
- Assertion-specific structured metadata.

The evaluator never invokes a process or accesses a remote resource.

Each result is represented as a typed local-check entity in `QueryEnvelope.entities` with a stable `entity_kind = "local_doctor_check"`. Existing envelope schema version `1.0` remains valid because entities are already extensible objects. The entity carries the fields above; `QueryEnvelope.warnings` contains bounded summaries for failed warning-severity checks, and informational entities are listed in `meta.informational_findings`. Every local warning summary begins with the stable key `[local-doctor/<doctor>/<check>]`, matching the entity's rule identifier.

The diagnostic conversion layer is extended to recognize `local_doctor_check` entities. Text, `--json`, and SARIF rendering derive a stable rule identifier `local-doctor/<doctor>/<check>`, severity, repository-relative path, line, and sanitized message from the entity. When a typed entity is present, the converter suppresses exactly the warning string bearing that entity's stable key, so a failed check is rendered once rather than once as a warning and again as an evidence note. Existing compiled-doctor warnings do not use that reserved key grammar and remain unchanged. JSON and SARIF tests assert that check identity, severity, path, and line survive conversion.

### 4. Doctor resolver and suites

The CLI changes its doctor positional argument from the compile-time `DoctorKind` enum to a validated string.

Resolution is:

1. `all`, `baseline`, or `ci` invokes the existing suite implementation.
2. A compiled doctor name invokes the existing Rust doctor.
3. A valid repository-local doctor name invokes the declarative evaluator.
4. Any other name returns a deterministic unknown-doctor error.

A valid local doctor's assertions run when named directly. They participate in `doctor all` only when its specification contains `suites = ["all"]`.

Local doctors never participate automatically in `baseline` or `ci`. Those remain fixed compiled policy presets. This prevents a repository file from silently changing the meaning or performance envelope of LEIO's stable release gates.

`doctor all` evaluates applicable compiled doctors and opted-in valid local doctors. Its aggregator propagates every typed local-check entity into the aggregate envelope and retains the corresponding stable-keyed warning summary so strict warning counting and failing-doctor metadata remain correct. Machine-format conversion then performs the exact-key suppression described above; compiled-doctor warning aggregation is unchanged. `doctor all` also always runs one generic local-pack-integrity result over the doctor directory. Invalid manifests fail that integrity result but do not execute their assertions or masquerade as opted-in doctors. This makes pack corruption visible without claiming that a malformed targeted-only pack joined the `all` suite.

### 5. Capabilities and transports

Capabilities merge:

- Compiled doctors applicable to the detected repository profile.
- Valid repository-local doctor names.
- Reserved aggregate `all` where the current surface exposes it.

Capabilities also report invalid local-pack diagnostics in metadata, but do not advertise invalid names as runnable doctors.

The stdio MCP and Apps SDK retain stable tool names and stable input shapes. Their targeted doctor argument changes from a broad duplicated enum to a bounded non-empty string. After authentication and repository-target resolution, the transport validates the name against capabilities derived for that exact repository. Both Apps SDK consumers are covered: `audit_repository_contracts.kind` and `consult_carlos_motta_specialist.audit_kind` use post-authorization repository capability validation rather than the old shared static enum.

Validation must occur after target resolution. A client cannot supply trusted capability data, and one authenticated repository context must not reveal another checkout's doctor inventory.

Unknown, reserved misuse, invalid targeted packs, and profile-inapplicable names produce a normal tool error with structured `{ ok: false }` content. CLI unknown-name and invalid-pack configuration errors exit with code `2`, preserving LEIO's diagnostic contract; a successfully evaluated doctor containing warning findings exits with code `1` under the existing strict behavior. The doctor name continues to be passed to the CLI as one argument in an argument array; no shell command concatenation is allowed.

## Version 1 Assertion Language

### `path-exists`

Requires a file or directory at a repository-relative path.

Required fields: `path`.

This assertion does not read file contents. Existing file inputs must still be tracked regular files; existing directory inputs must be represented by the selected revision tree or a tracked descendant in the local index. A safe nonexistent path evaluates as absent and emits the check's declared finding rather than a loader error.

### `file-contains`

Requires one literal string in a UTF-8 text file.

Required fields: `path`, `contains`.

### `file-contains-all`

Requires every listed literal string in a UTF-8 text file.

Required fields: `path`, `contains` as a non-empty string list.

### `file-not-contains`

Requires a literal string to be absent from a UTF-8 text file.

Required fields: `path`, `contains`.

### `make-target-contains`

Parses a named Make target's recipe extent and requires literal fragments within that target rather than anywhere in the Makefile.

Required fields: `path`, `target`, `contains` as a non-empty string list.

The parser supports ordinary target declarations and tab-indented recipes. It does not attempt to evaluate Make conditionals, includes, variable expansion, or generated targets. If the target cannot be located statically, the check fails with an actionable reason.

### `toml-array-contains`

Parses a TOML document, resolves a dotted key, requires the resolved value to be an array, and checks for one literal string member.

Required fields: `path`, `key`, `value`.

### `cargo-feature-packages-covered-by-make-target`

Performs bounded runtime discovery needed by the office-parser migration.

Required fields:

- `workspace`: repository-relative Cargo workspace manifest.
- `feature`: feature name to discover.
- `makefile`: repository-relative Makefile.
- `target`: Make target that should cover discovered packages.

This assertion also requires `integrity_severity`. It applies to failures that prevent trustworthy discovery, including workspace or member-manifest read errors, traversal rejection, TOML parse errors, missing package names, and budget exhaustion. The ordinary `severity` applies only to successfully discovered packages that are not covered by the Make target and to optional target-adoption anchors.

The evaluator:

1. Parses the workspace manifest.
2. Expands only workspace member patterns beneath the workspace manifest's directory.
3. Rejects members resolving outside the repository root.
4. Parses member `Cargo.toml` files.
5. Selects packages declaring the named feature.
6. Parses the named Make target.
7. Reports each selected package whose target recipe lacks a corresponding `-p <package>` and feature-selection anchor.

The evaluator records bounded discovered package names, repository-relative manifests, and uncovered packages as structured metadata. It does not run Cargo metadata or Cargo itself.

The Example migration declares `integrity_severity = "warning"` and `severity = "info"`. This preserves the current strict treatment of discovery integrity failures while keeping incomplete Arrow-feature adoption informational.

## Office-Parsers Migration

The new repository-owned specification will be:

```text
/Users/josaum/projects/example-workspace/.leio-code/doctors/office-parsers-clippy-gate.toml
```

It preserves existing strict warning classifications and intentionally tightens the location and completeness of the Clippy recipe. The current Example target already satisfies the tightened contract. Strict warnings are emitted for:

1. Missing `office-parsers-clippy` Make target.
2. Missing `cargo clippy --workspace` in that target.
3. Missing `--all-targets` or `-D warnings` in that target.
4. Missing `vendor/oar-ocr` from `office-parsers-rs/Cargo.toml` workspace exclusions.
5. Missing `make office-parsers-clippy` from `scripts/verify-ci.sh`.
6. Missing the office-parser Clippy check from `scripts/ops/office-parsers-health.sh`.

The current compiled doctor searches some command fragments across the whole Makefile and does not require `--all-targets`. The migration deliberately scopes those fragments to `office-parsers-clippy` and requires `--all-targets`; fixtures pin this approved tightening. Where the compiled doctor groups command requirements into one logical check, the migrated specification may emit multiple check records while retaining warning severity and actionable evidence.

It preserves informational findings for:

1. A missing `office-parsers-clippy-features` target.
2. A feature target that lacks `cargo clippy`, `--features arrow`, or `-D warnings`.
3. An Arrow-enabled parser package omitted from the feature target.
4. Missing feature-target propagation to CI or operations health.

After fixture parity and real-repository verification pass, LEIO removes:

- `src/doctors/office_parsers_clippy_gate.rs`.
- Its module declaration.
- Its compiled registry registration.
- Its obsolete static CLI mapping.
- Its obsolete MCP and Apps SDK static-kind entries.

The existing LEIO commit that added runtime Arrow coverage remains in history. The migration is additive and supersedes that implementation in a later commit; history is not rewritten.

Other office-parser doctors remain compiled. Their migration is explicitly outside this first implementation.

## Error Handling

### Invalid pack during targeted execution

If the requested filename exists but cannot be parsed or validated, targeted execution returns a non-success tool result with a specific path and schema diagnostic. It must not degrade into a generic unknown-doctor message.

### Invalid pack during capabilities

Capabilities omit the invalid doctor from runnable names and include a bounded diagnostic in metadata and warnings.

### Invalid pack during `doctor all`

Every invalid manifest contributes to the single generic local-pack-integrity result that always runs during `doctor all`. Invalid manifests never execute assertions and are never counted as opted-in local doctors. This fail-closed integrity check is separate from suite membership.

### Missing input file

A check referencing a missing file emits a finding at its declared severity with the unresolved repository-relative path.

### Non-UTF-8 text

Text assertions against non-UTF-8 content emit a finding at the declared severity. V1 does not perform lossy decoding.

### Unknown schema version or assertion kind

The pack is invalid and is handled through the invalid-pack paths above.

## Security Model

Doctor packs are untrusted repository data.

The loader and evaluator enforce:

- Repository-relative paths only.
- Lexical rejection of parent traversal.
- Root-anchored, component-aware, no-follow file opening.
- Symlink and special-file rejection for doctor directories, manifests, assertion inputs, and workspace traversal.
- Immutable revision-backed inputs for hosted Apps SDK execution.
- Git-tracked, non-ignored regular-file inputs for local working-tree execution.
- Rejection of paths equal to or beneath `.git`, `.leio-code`, `profiles`, `secrets`, and `deploy/secrets` as assertion targets.
- A bounded filename and doctor-name grammar.
- At most 128 pack files per repository.
- At most 256 checks per pack.
- At most 1 MiB per doctor manifest.
- At most 16 MiB per text, TOML, Makefile, or Cargo manifest input read by a check.
- At most 8 MiB of aggregate manifest input per request.
- At most 1,024 evaluated local checks per request, including `doctor all`.
- At most 256 MiB of cumulative unique assertion input per request.
- At most 20,000 directory entries visited, 4,096 workspace members expanded, and path depth 32.
- At most 4,096 items in any schema list and 64 KiB in any scalar string.
- At most 4,096 emitted findings and 8 MiB of encoded local-doctor output per request.
- A 10-second local-pack evaluation deadline, checked during traversal and matching.
- No process creation.
- No network access.
- No environment expansion.
- No arbitrary regular expressions.
- No compiled-doctor shadowing.
- Authentication and repository authorization before Apps SDK discovery.

Discovery and evaluation share one request budget across every local pack. Input is cached by validated content identity: revision blob identity in hosted mode and canonical tracked path plus file metadata in local mode. Unique input bytes are charged once, while each match operation still consumes the check budget. Literal matching and parsing use bounded linear algorithms.

Exceeding a budget or receiving cancellation stops local-pack evaluation and emits one bounded deterministic error; it does not emit one error per skipped check. The MCP and Apps SDK wrappers additionally enforce the subprocess deadline, the encoded-output cap, client-cancellation termination, and per-request isolation so one repository cannot consume another repository's budget. Boundary values and the first rejected value receive focused tests.

Repository-derived output is untrusted data. Names and identifiers use bounded ASCII grammars. Other scalars reject NUL and unsafe control or bidirectional characters; terminal rendering escapes ANSI and OSC controls. Parse diagnostics contain only a bounded error code, repository-relative path, field when known, line, and column. They never include raw source excerpts, absolute server paths, file contents, expected literals, or matched lines. MCP and Apps SDK place repository-derived values only in typed structured fields and never interpolate them into tool instructions or authoritative top-level prose.

## Data Flow

```text
CLI / MCP / Apps SDK request
        |
        v
resolve and authorize repository root
        |
        v
discover + validate .leio-code/doctors/*.toml
        |
        +---- invalid diagnostics ----> capabilities metadata / failing result
        |
        v
resolve reserved suite, compiled doctor, or local doctor
        |
        v
evaluate bounded repository-local assertions
        |
        v
standard QueryEnvelope with checks, evidence, warnings, and info findings
```

## Testing Strategy

Implementation follows test-driven development.

### Loader and schema unit tests

- Empty or absent doctor directory.
- Deterministic sorted discovery.
- TOML files only.
- Valid schema version.
- Unknown schema version.
- Unknown and misspelled fields.
- Empty checks.
- Duplicate check identifiers.
- Filename/name mismatch.
- Invalid slug.
- Reserved-name collision.
- Compiled-doctor collision.
- Absolute path and parent traversal rejection.
- Symlinked doctor directory, symlinked manifest, internal and external symlink targets, and special-file rejection.
- ANSI, OSC, bidirectional control, NUL, oversized scalar, and malformed-input diagnostic sanitization.
- Per-item and aggregate request-budget boundary and first-rejected-value behavior.

### Assertion evaluator tests

- Passing and failing cases for every assertion kind.
- Exact repository-relative evidence paths.
- Line provenance for literal matches.
- Missing file and invalid UTF-8 behavior.
- `path-exists` accepts a revision-backed or tracked-descendant directory and evaluates a safe nonexistent path as absent without opening it.
- Make target boundary parsing so text in another target cannot satisfy a check.
- TOML dotted-key traversal and type mismatch.
- Cargo workspace member discovery.
- Arrow feature selection.
- Missing per-package Make coverage.
- Workspace members that attempt to escape the repository.
- Workspace glob traversal never follows symlinks and respects entry, member, depth, byte, and deadline budgets.
- Discovery integrity failures use `integrity_severity`; uncovered packages use ordinary `severity`.
- Repeated reads use the validated cache and charge unique bytes once while charging each match.
- `.env`, `.git/config`, ignored, untracked, secret/profile, `.leio-code`, FIFO, socket, and device targets are rejected.
- A tracked source input remains readable in local mode; hosted evaluation reads the selected revision rather than neighboring or modified server-worktree content.

### CLI integration tests

- An arbitrary valid local doctor name parses and runs.
- `--repo` placement remains compatible.
- `--format`, `--explain`, and `--suggest` remain compatible.
- Text, JSON, and SARIF preserve the local check rule ID, severity, repository-relative path, and line without duplicate diagnostics.
- An opted-in local doctor preserves the same fields and exact-key duplicate suppression through `doctor all --format=json|sarif`, while compiled warning rendering remains unchanged.
- `all`, `baseline`, and `ci` preserve existing behavior.
- Unknown doctor exits `2` with a deterministic runtime configuration error.
- Invalid requested pack exits `2` with a schema-specific error.
- A successfully evaluated strict doctor with warnings retains exit `1` behavior.
- `doctor all` executes assertions only for local doctors opting into `all` and separately reports the generic pack-integrity result.

### Capabilities and transport tests

- Capabilities include valid local names only for the selected repository.
- Invalid packs appear as diagnostics but not runnable names.
- stdio MCP accepts a dynamically discovered doctor name.
- Apps SDK `audit_repository_contracts.kind` accepts the same name after repository authorization.
- Apps SDK `consult_carlos_motta_specialist.audit_kind` uses the same post-authorization dynamic validation.
- Unknown and profile-inapplicable names return structured tool errors.
- Unauthorized callers cannot probe local doctor inventory.
- Arbitrary hosted server roots remain rejected.
- Repository A cannot observe repository B through symlink aliases, stale capability caches, client-provided roots or index paths, or revision changes.
- Capabilities, status, context, init, and audit retain coherent local-doctor metadata where they consume workspace capabilities.
- MCP and Apps SDK enforce cancellation, deadline, output-size, and per-request budget behavior.
- Broad duplicated doctor-kind arrays are removed.
- Existing MCP transport and legacy-contract tests continue to pass.

### Office-parser migration fixtures

- Complete clean fixture.
- Missing Make target.
- Target without a real workspace Clippy command.
- Missing `--all-targets`.
- Missing `-D warnings`.
- Missing vendor exclusion.
- Missing CI invocation.
- Missing operations-health invocation.
- Missing optional feature target remains informational.
- Malformed optional target remains informational.
- A discovered Arrow-enabled crate omitted from feature coverage remains informational.
- Workspace/member read or parse failure remains a strict warning.
- Optional feature target absent from CI or operations health remains informational.

### Real-repository verification

The source-built LEIO binary will run against `/Users/josaum/projects/example-workspace` and prove:

- Targeted `office-parsers-clippy-gate` execution.
- Zero strict warnings on the current repository state.
- Informational Arrow-feature adoption findings remain informational.
- Runtime-derived Arrow package coverage metadata.
- Inclusion in `doctor all`.
- Presence in capabilities.
- Equivalent structured behavior through stdio MCP and Apps SDK tests.

The installed LEIO binary may lag the checked-out source. Verification and the final report will distinguish source-built results from installed-binary results. Installing or publishing the new binary requires separate explicit scope.

## Commit and Repository Strategy

No branch is switched in either repository.

LEIO remains:

```text
/Users/josaum/projects/leio-code
branch: main
```

Example remains:

```text
/Users/josaum/projects/example-workspace
branch: fix/office-parsers-remaining-holes
```

Implementation commits will be additive and repository-specific. The design document is committed first in LEIO. LEIO engine and transport changes are committed in dependency order. The Example local pack and its repository-owned fixture coverage are committed separately in Example. Existing unrelated dirty files and untracked artifacts are preserved. Nothing is pushed.

## Success Criteria

The feature is complete when all of the following are true:

1. A repository can add a valid `.leio-code/doctors/<name>.toml` without modifying or rebuilding LEIO.
2. The doctor runs through the unchanged `leio-code doctor <name> --repo <repo>` command shape.
3. Capabilities, stdio MCP, and Apps SDK discover and accept the local name for the authorized repository.
4. Local packs cannot execute processes, use the network, read ineligible local files, traverse or follow symlinks outside the authorized content source, shadow compiled doctors, exhaust an unbounded request budget, or inject unsafe terminal/agent output.
5. `doctor all` executes assertions only for explicitly opted-in valid local doctors, reports pack validity through one generic integrity result, and leaves `baseline` and `ci` unchanged.
6. `office-parsers-clippy-gate` is owned by Example and the compiled LEIO implementation is removed.
7. The migrated doctor preserves existing severity classifications and informational Arrow-adoption behavior, keeps discovery integrity failures strict, and enforces the approved target-scoping and `--all-targets` tightening on fixtures and the real repository.
8. Focused Rust, CLI, MCP, Apps SDK, self-contract, and real-repository verification passes, or any unrelated baseline blocker is reported with exact evidence.
9. Both repositories remain on their original branches, unrelated dirty state is preserved, and no push occurs.
