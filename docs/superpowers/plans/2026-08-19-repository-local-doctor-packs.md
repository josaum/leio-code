# Repository-Local Doctor Packs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let an installed LEIO Code binary discover and safely execute declarative doctors owned by the repository being inspected, then migrate `office-parsers-clippy-gate` from compiled LEIO code into Example.

**Architecture:** LEIO gains a versioned local-pack loader and bounded static evaluator under `src/doctors/local_packs.rs`. The existing Rust doctor registry remains authoritative for compiled doctors, while one resolver merges compiled and repository-local doctors for CLI, suites, capabilities, stdio MCP, and Apps SDK. Example owns only its TOML policy; no repository pack can execute processes, use the network, escape its authorized content source, or shadow a compiled doctor.

**Tech Stack:** Rust 2021, Clap, Serde, `toml`, `serde_json`, Git CLI/object data for content eligibility, Node.js ESM, Zod, MCP stdio, Apps SDK, Cargo integration tests, Node test runner.

**Spec:** `docs/superpowers/specs/2026-08-19-repository-local-doctor-packs-design.md`

## Global Constraints

- Work in two existing repositories without switching branches: LEIO at `/Users/josaum/projects/leio-code` on `main`, and Example at `/Users/josaum/projects/example-workspace` on `fix/office-parsers-remaining-holes`.
- Do not push, merge, rebase, install, or publish LEIO.
- Preserve all unrelated dirty and untracked state; every commit stages explicit paths only.
- Local packs live only at `<repo>/.leio-code/doctors/*.toml` and use schema version `1`.
- Reserve `all`, `baseline`, and `ci`; reject local/compiled name collisions.
- Local doctors join `all` only with `suites = ["all"]`; they never join fixed `baseline` or `ci` suites.
- Hosted Apps SDK reads an immutable authorized revision; CLI and same-user stdio use tracked, non-ignored working-tree files.
- Reject paths equal to or beneath `.git`, `.leio-code`, `profiles`, `secrets`, and `deploy/secrets` as assertion targets.
- Never execute shell, Make, Cargo, repository binaries, plugins, URLs, environment expansion, or user regexes from a local pack.
- Enforce the exact limits from the spec: 128 packs, 256 checks per pack, 1 MiB per manifest, 16 MiB per input, 8 MiB aggregate manifests, 1,024 checks, 256 MiB unique input, 20,000 entries, 4,096 workspace members, path depth 32, 4,096 list items, 64 KiB scalars, 4,096 findings, 8 MiB output, and a 10-second local-pack deadline.
- Unknown or invalid targeted doctor configuration exits `2`; an evaluated strict doctor with warnings retains exit `1` behavior.
- Keep `QueryEnvelope.schema_version = "1.0"`; represent local results as extensible `entities` with `entity_kind = "local_doctor_check"`.
- Preserve existing compiled-doctor output and warning behavior.
- Follow TDD: each task begins with a failing focused test, proves the failure, adds the smallest implementation, proves the focused surface, and commits.

## File Structure

### LEIO Code

- Create `src/doctors/local_packs.rs`: schema, discovery, trusted content access, budgets, assertion evaluation, typed entities, local-pack catalog.
- Modify `src/doctors/mod.rs`: local module export, unified resolver, targeted execution, `doctor all` aggregation, generic pack-integrity result.
- Modify `src/capabilities.rs`: merge valid local doctor names and bounded invalid-pack diagnostics; remove source-text coupling to JavaScript allowlists.
- Modify `src/main.rs`: replace `DoctorKind` with a bounded runtime string and map configuration errors to exit `2`.
- Modify `src/diagnostics.rs`: translate typed local entities into text/JSON/SARIF and suppress exact keyed duplicate warnings.
- Create `tests/local_doctor_packs.rs`: loader, schema, security, assertion, budget, suite, capability, and office-parser fixture coverage.
- Create `tests/local_doctor_pack_cli.rs`: subprocess CLI parsing, exit codes, targeted execution, and aggregate behavior.
- Modify `tests/diagnostics_format.rs`: typed local JSON/SARIF rendering and duplicate suppression.
- Modify `tests/schema_version.rs`: schema `1.0` compatibility for local entities.
- Modify `tests/cli_surface.rs`: preserve `all`, `baseline`, `ci`, explain/suggest, and argument-position contracts.
- Create `mcp/process-runner.js`: bounded cancellation-aware child-process execution for stdio MCP.
- Create `mcp/process-runner.test.js`: deadline, cancellation, output-cap, and process-tree behavior.
- Modify `mcp/index.js`: dynamic doctor slug schema and exact-repository capability validation.
- Create `mcp/local-doctor-packs.test.js`: stdio dynamic validation and repository isolation.
- Create `apps-sdk/process-runner.js`: bounded hosted runner preserving tenant-scoped environment.
- Create `apps-sdk/process-runner.test.js`: hosted process deadline, cancellation, output, and sanitization coverage.
- Modify `apps-sdk/server.js`: dynamic validation for both direct audit and Carlos Motta audit paths after auth and target resolution.
- Create `apps-sdk/local-doctor-packs.test.js`: auth ordering, target/revision isolation, both consumers, and structured errors.
- Delete `src/doctors/office_parsers_clippy_gate.rs` only in the final migration task.
- Modify `src/doctors/self_contract.rs` only where it asserts the removed static CLI/transport mirrors.

### Example

- Create `.leio-code/doctors/office-parsers-clippy-gate.toml`: the only Example implementation file.
- Do not modify Example's Makefile, Cargo workspace, CI script, operations-health script, or existing `.leio-code/config.toml`.

### Integration-test helper contracts

Define these helpers at the top of `tests/local_doctor_packs.rs`; later tasks extend their methods rather than inventing unrelated fixtures:

```rust
struct TestRepo {
    temp: tempfile::TempDir,
}

impl TestRepo {
    fn new() -> Self;
    fn path(&self) -> &Path;
    fn write(&self, relative: &str, body: &str);
    fn write_bytes(&self, relative: &str, body: &[u8]);
    fn git_add(&self, relative: &str);
    fn git_commit(&self, message: &str);
    fn write_pack(&self, name: &str, body: String);
    fn request(&self, mode: LocalContentMode) -> LocalPackRequest;
}

fn minimal_pack(name: &str) -> String;
fn compiled_names() -> BTreeSet<String>;

struct LocalDoctorFixture {
    repo: TestRepo,
    doctor_name: String,
    checks: Vec<String>,
}

impl LocalDoctorFixture {
    fn new(name: &str) -> Self;
    fn add_path_exists(&mut self, id: &str, path: &str, severity: Severity);
    fn add_file_contains(&mut self, id: &str, path: &str, literal: &str, severity: Severity);
    fn write_untracked(&self, path: &str, body: &str);
    fn run(&mut self) -> QueryEnvelope;
    fn run_check(&mut self, id: &str) -> LocalCheckView;
}

struct LocalCheckView(serde_json::Value);

impl LocalCheckView {
    fn passed(&self) -> bool;
    fn severity(&self) -> &str;
    fn reason_code(&self) -> &str;
}
```

`TestRepo::new()` initializes Git with deterministic local identity. `write_pack()` writes and stages the pack manifest so the manifest is repository content. Content-input tests explicitly stage or leave files untracked according to the behavior under test.

---

### Task 1: Strict local-pack schema and deterministic discovery

**Files:**
- Create: `src/doctors/local_packs.rs`
- Modify: `src/doctors/mod.rs`
- Create: `tests/local_doctor_packs.rs`

**Interfaces:**
- Consumes: compiled names from `doctor_names()` in `src/doctors/mod.rs`.
- Produces: `LocalPackRequest`, `LocalPackBudgets`, `LocalDoctorCatalog`, `LocalDoctorPack`, `LocalPackDiagnostic`, and `discover_local_doctor_packs()` for later evaluator and resolver tasks.

- [ ] **Step 1: Write failing schema and discovery tests**

Add tests that construct temporary Git repositories and assert: absent directory is empty; valid TOML loads; discovery is sorted; nested/hidden/non-TOML files are ignored; unknown fields, duplicate IDs, invalid slugs, filename/name mismatch, reserved names, compiled collisions, symlinked manifests/directories, special files, and every schema boundary produce bounded diagnostics.

```rust
#[test]
fn discovers_valid_packs_in_filename_order() {
    let repo = TestRepo::new();
    repo.write_pack("z-last", minimal_pack("z-last"));
    repo.write_pack("a-first", minimal_pack("a-first"));
    let request = repo.request(LocalContentMode::WorkingTreeTracked);
    let catalog = discover_local_doctor_packs(&request, &compiled_names());
    assert_eq!(catalog.packs.keys().cloned().collect::<Vec<_>>(), ["a-first", "z-last"]);
    assert!(catalog.diagnostics.is_empty());
}

#[test]
fn rejects_reserved_and_compiled_names() {
    let repo = TestRepo::new();
    repo.write_pack("all", minimal_pack("all"));
    repo.write_pack("self-contract", minimal_pack("self-contract"));
    let catalog = discover_local_doctor_packs(
        &repo.request(LocalContentMode::WorkingTreeTracked),
        &compiled_names(),
    );
    assert!(catalog.packs.is_empty());
    assert_eq!(catalog.diagnostics.len(), 2);
}
```

- [ ] **Step 2: Run the new integration target and confirm RED**

Run:

```bash
cd /Users/josaum/projects/leio-code
cargo test --test local_doctor_packs -- --nocapture
```

Expected: compilation fails because `local_packs` and its public types do not exist.

- [ ] **Step 3: Implement typed schema, limits, diagnostics, and no-follow manifest discovery**

Define the concrete public boundary:

```rust
pub const LOCAL_DOCTOR_DIR: &str = ".leio-code/doctors";
pub const LOCAL_CHECK_ENTITY_KIND: &str = "local_doctor_check";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalContentMode { RevisionTracked, WorkingTreeTracked }

#[derive(Debug, Clone)]
pub struct LocalPackBudgets {
    pub max_pack_files: usize,
    pub max_checks_per_pack: usize,
    pub max_manifest_bytes: u64,
    pub max_input_bytes_per_file: u64,
    pub max_manifest_input_bytes: u64,
    pub max_checks_per_request: usize,
    pub max_unique_input_bytes: u64,
    pub max_directory_entries: usize,
    pub max_workspace_members: usize,
    pub max_path_depth: usize,
    pub max_list_items: usize,
    pub max_scalar_bytes: usize,
    pub max_findings: usize,
    pub max_encoded_output_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct LocalPackRequest {
    pub root: PathBuf,
    pub content_mode: LocalContentMode,
    pub deadline: Instant,
    pub budgets: LocalPackBudgets,
}

pub fn discover_local_doctor_packs(
    request: &LocalPackRequest,
    compiled_names: &BTreeSet<String>,
) -> LocalDoctorCatalog;
```

Use `#[serde(deny_unknown_fields)]` on the manifest and every tagged assertion variant. Validate names with ASCII byte checks rather than regex. Open the doctor directory and manifests with one component-aware no-follow helper; reject symlinks and non-regular files before parsing. Sanitize diagnostics to code, relative path, field, line, column, and bounded message without source excerpts.

- [ ] **Step 4: Run discovery tests and the compiled doctor library tests**

```bash
cargo test --test local_doctor_packs -- --nocapture
cargo test doctors:: --lib
```

Expected: all new discovery tests pass; existing compiled doctors remain green.

- [ ] **Step 5: Commit Task 1**

```bash
git add -- src/doctors/local_packs.rs src/doctors/mod.rs tests/local_doctor_packs.rs
git commit -m "feat(doctors): add declarative local pack discovery"
```

### Task 2: Trusted content access and simple assertions

**Files:**
- Modify: `src/doctors/local_packs.rs`
- Modify: `tests/local_doctor_packs.rs`

**Interfaces:**
- Consumes: `LocalPackRequest` and parsed assertion variants from Task 1.
- Produces: `LocalPackRequestContext`, `LocalCheckResult`, `run_local_doctor_pack()`, and the `path-exists`, literal-file, and TOML-array evaluators.

- [ ] **Step 1: Write failing trusted-input and simple-assertion tests**

Cover tracked file success, tracked directory eligibility, safe missing path, revision-backed content, ignored/untracked rejection, exact denied prefixes, invalid UTF-8, special files, symlink swaps, file-size limits, `file-contains`, `file-contains-all`, `file-not-contains`, line provenance, TOML dotted arrays, and unique-input caching.

```rust
#[test]
fn path_exists_reports_safe_missing_path_as_a_finding() {
    let mut fixture = LocalDoctorFixture::new("required-path");
    fixture.add_path_exists("required-source", "src/required.rs", Severity::Warning);
    let envelope = fixture.run();
    assert_eq!(envelope.warnings.len(), 1);
    assert_eq!(envelope.entities[0]["path"], "src/required.rs");
}

#[test]
fn working_tree_mode_rejects_untracked_content_reads() {
    let mut fixture = LocalDoctorFixture::new("tracked-only");
    fixture.write_untracked(".env", "TOKEN=secret");
    fixture.add_file_contains("secret-probe", ".env", "TOKEN=", Severity::Warning);
    let envelope = fixture.run();
    assert_eq!(envelope.entities[0]["reason_code"], "ineligible_input");
    assert!(!envelope.to_string().contains("TOKEN"));
}
```

- [ ] **Step 2: Run focused tests and confirm RED**

```bash
cargo test --test local_doctor_packs path_exists -- --nocapture
cargo test --test local_doctor_packs working_tree -- --nocapture
cargo test --test local_doctor_packs simple_assertions -- --nocapture
```

Expected: tests fail because evaluation and trusted content access are not implemented.

- [ ] **Step 3: Implement one validated input abstraction and simple evaluators**

Add these internal boundaries and route every content read through them:

```rust
struct LocalPackRequestContext {
    request: LocalPackRequest,
    counters: RequestCounters,
    content_cache: BTreeMap<ContentIdentity, Arc<[u8]>>,
}

struct ValidatedInput {
    relative_path: PathBuf,
    bytes: Arc<[u8]>,
}

impl LocalPackRequestContext {
    fn read_tracked_regular_file(&mut self, relative: &Path) -> Result<ValidatedInput, CheckFailure>;
    fn check_path_exists(&mut self, relative: &Path) -> Result<bool, CheckFailure>;
}

pub fn run_local_doctor_pack(
    pack: &LocalDoctorPack,
    context: &mut LocalPackRequestContext,
) -> QueryEnvelope;
```

Working-tree reads must consult Git tracking/ignore state before opening. Revision mode must read the selected revision's blobs/tree rather than the mutable hosted filesystem. Produce local entities with doctor, check, kind, severity, passed, relative path, optional line, bounded reason, and safe metadata. Warning strings use `[local-doctor/<doctor>/<check>]` exactly; informational findings go to `meta.informational_findings`.

- [ ] **Step 4: Run all Task 2 tests**

```bash
cargo test --test local_doctor_packs -- --nocapture
```

Expected: trusted-input and simple assertion tests pass; no output contains source content, matched literals, or absolute paths.

- [ ] **Step 5: Commit Task 2**

```bash
git add -- src/doctors/local_packs.rs tests/local_doctor_packs.rs
git commit -m "feat(doctors): evaluate safe local file assertions"
```

### Task 3: Make target, Cargo feature coverage, and aggregate budgets

**Files:**
- Modify: `src/doctors/local_packs.rs`
- Modify: `tests/local_doctor_packs.rs`

**Interfaces:**
- Consumes: validated inputs and request counters from Task 2.
- Produces: `make-target-contains`, `cargo-feature-packages-covered-by-make-target`, `integrity_severity`, deadline/cancellation checks, and deterministic budget exhaustion.

- [ ] **Step 1: Write failing Make, Cargo, and aggregate-budget tests**

Add these Task 3 test helpers beside the shared fixture types:

```rust
fn make_target_fixture(makefile: &str) -> LocalDoctorFixture;
fn cargo_feature_fixture(member_manifest: &str) -> LocalDoctorFixture;
```

```rust
#[test]
fn make_fragments_in_another_target_do_not_satisfy_the_check() {
    let fixture = make_target_fixture(
        "wrong:\n\tcargo clippy --workspace --all-targets -- -D warnings\n\noffice-parsers-clippy:\n\t@true\n",
    );
    let result = fixture.run_check("workspace-clippy-target");
    assert!(!result.passed());
}

#[test]
fn cargo_parse_failures_use_integrity_severity() {
    let fixture = cargo_feature_fixture("not = [valid toml");
    let result = fixture.run_check("arrow-feature-packages-covered");
    assert_eq!(result.severity(), "warning");
    assert_eq!(result.reason_code(), "cargo_manifest_parse");
}
```

Also test no-follow glob traversal, escaping members, feature selection, missing `-p <package>` and feature anchors, 20,000/20,001 directory entries, 4,096/4,097 members, depth 32/33, 1,024/1,025 checks, 256 MiB unique input, 4,096/4,097 findings, encoded-output limit, deadline, and one-error abort behavior.

- [ ] **Step 2: Run focused tests and confirm RED**

```bash
cargo test --test local_doctor_packs make_target -- --nocapture
cargo test --test local_doctor_packs cargo_feature -- --nocapture
cargo test --test local_doctor_packs aggregate_budget -- --nocapture
```

Expected: tests fail because Make/Cargo evaluators and aggregate budgeting do not exist.

- [ ] **Step 3: Implement bounded line scanning, Cargo TOML discovery, and shared request charging**

Use these internal helpers:

```rust
fn make_target_recipe<'a>(source: &'a str, target: &str) -> Result<Vec<(usize, &'a str)>, CheckFailure>;
fn discover_feature_packages(
    context: &mut LocalPackRequestContext,
    workspace_manifest: &Path,
    feature: &str,
) -> Result<Vec<FeaturePackage>, CheckFailure>;
fn charge_deadline_and_budget(
    context: &mut LocalPackRequestContext,
    operation: BudgetOperation,
) -> Result<(), CheckFailure>;
```

Parse only ordinary target declarations and tab-indented recipes. Expand Cargo member patterns without descending symlink directories. Use `integrity_severity` for traversal/read/parse/name/budget failures and ordinary `severity` only for successfully discovered adoption gaps. Do not invoke Make, Cargo, a shell, or another process.

- [ ] **Step 4: Run the complete evaluator target**

```bash
cargo test --test local_doctor_packs -- --nocapture
```

Expected: all discovery, safety, simple, Make, Cargo, and budget tests pass.

- [ ] **Step 5: Commit Task 3**

```bash
git add -- src/doctors/local_packs.rs tests/local_doctor_packs.rs
git commit -m "feat(doctors): add bounded make and cargo local checks"
```

### Task 4: Unified resolver, suites, capabilities, and CLI exit contract

**Files:**
- Modify: `src/doctors/mod.rs`
- Modify: `src/capabilities.rs`
- Modify: `src/main.rs`
- Modify: `tests/cli_surface.rs`
- Modify: `tests/local_doctor_packs.rs`
- Create: `tests/local_doctor_pack_cli.rs`

**Interfaces:**
- Consumes: `LocalDoctorCatalog` and evaluator from Tasks 1–3.
- Produces: `DoctorResolution`, `DoctorResolveError`, `DoctorContentSourceArg`, local-aware `run_doctor`, `run_all_doctors`, capabilities, and runtime string CLI parsing.

- [ ] **Step 1: Write failing resolver, suite, capability, and CLI tests**

Add these local helpers in the relevant integration test module:

```rust
fn opted_in_pack() -> String;
fn targeted_only_pack() -> String;
fn malformed_pack() -> String;
fn run_all_fixture(packs: &[String]) -> QueryEnvelope;
fn has_doctor(envelope: &QueryEnvelope, name: &str) -> bool;
fn count_integrity_results(envelope: &QueryEnvelope) -> usize;
```

```rust
#[test]
fn unknown_local_doctor_exits_as_configuration_error() {
    let fixture = TestRepo::new();
    Command::cargo_bin("leio-code")
        .unwrap()
        .arg("--repo")
        .arg(fixture.path())
        .args(["doctor", "does-not-exist"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("doctor `does-not-exist` is not available"));
}

#[test]
fn all_runs_only_opted_in_local_assertions_and_pack_integrity() {
    let envelope = run_all_fixture(&[opted_in_pack(), targeted_only_pack(), malformed_pack()]);
    assert!(has_doctor(&envelope, "opted-in"));
    assert!(!has_doctor(&envelope, "targeted-only"));
    assert_eq!(count_integrity_results(&envelope), 1);
}
```

Pin `baseline` and `ci` counts/names to their existing compiled sets; verify valid local names appear in capabilities, invalid packs appear only in bounded metadata, and `--explain`/`--suggest` on local doctors exit `2` because local rule documentation is not part of version 1.

Also test the trusted content selector: default CLI and stdio behavior is `working-tree-tracked`; `--doctor-content-source revision-tracked` reads Git `HEAD` tree/blob content and ignores mutable working-tree changes. The selector may narrow access but has no value that broadens beyond the two approved modes.

- [ ] **Step 2: Run resolver and CLI tests and confirm RED**

```bash
cargo test --test local_doctor_pack_cli -- --nocapture
cargo test --test local_doctor_packs resolver -- --nocapture
cargo test --test cli_surface doctor -- --nocapture
```

Expected: arbitrary local names still fail Clap parsing and capabilities omit local packs.

- [ ] **Step 3: Replace the static CLI enum with the unified runtime resolver**

Add:

```rust
pub enum DoctorResolution<'a> {
    AggregateAll,
    Baseline,
    Ci,
    Compiled(&'a dyn Doctor),
    Local(&'a LocalDoctorPack),
}

pub enum DoctorResolveError {
    Unknown { name: String },
    InvalidLocalPack { name: String, diagnostic: LocalPackDiagnostic },
    ReservedName { name: String },
}

pub fn resolve_doctor<'a>(
    name: &str,
    catalog: &'a LocalDoctorCatalog,
) -> Result<DoctorResolution<'a>, DoctorResolveError>;
```

Change `Command::Doctor { kind: DoctorKind }` to `Command::Doctor { kind: String }`, delete `DoctorKind` and its exhaustive mapping, validate the bounded slug after parsing, and map `DoctorResolveError` to exit `2`. Compute one catalog per command/root/content mode. Extend `run_all_doctors` sequentially with opted-in local packs and one pack-integrity result; propagate local entities and stable-keyed warnings. Do not change `BASELINE_DOCTOR_NAMES`, `CI_EXTRA_DOCTOR_NAMES`, or `run_named_doctors_parallel`.

Add a global trusted content selector used by doctor discovery in capabilities, doctor, context, init, status, and audit:

```rust
#[derive(clap::ValueEnum, Clone, Copy, Debug, Default)]
enum DoctorContentSourceArg {
    #[default]
    WorkingTreeTracked,
    RevisionTracked,
}

impl From<DoctorContentSourceArg> for LocalContentMode {
    fn from(value: DoctorContentSourceArg) -> Self {
        match value {
            DoctorContentSourceArg::WorkingTreeTracked => Self::WorkingTreeTracked,
            DoctorContentSourceArg::RevisionTracked => Self::RevisionTracked,
        }
    }
}
```

Expose it as `--doctor-content-source` with exactly those two values. Apps SDK always passes `revision-tracked` for both capabilities and doctor execution against its immutable checkout. Stdio does not pass it and therefore remains same-user working-tree mode.

Extend capability construction to accept the selected root/catalog, merge sorted valid local names, and include sanitized invalid-pack diagnostics in metadata. Update status/context/init/audit callers to pass coherent root/revision data.

- [ ] **Step 4: Run focused Rust surfaces**

```bash
cargo test --test local_doctor_pack_cli -- --nocapture
cargo test --test local_doctor_packs -- --nocapture
cargo test --test cli_surface -- --nocapture
cargo test capabilities::tests --lib
```

Expected: local names parse and resolve; exit codes are correct; `all`/capabilities are local-aware; baseline and CI remain unchanged.

- [ ] **Step 5: Commit Task 4**

```bash
git add -- src/doctors/mod.rs src/capabilities.rs src/main.rs tests/cli_surface.rs tests/local_doctor_packs.rs tests/local_doctor_pack_cli.rs
git commit -m "feat(doctors): resolve and aggregate repository-local packs"
```

### Task 5: Typed text, JSON, and SARIF diagnostics

**Files:**
- Modify: `src/diagnostics.rs`
- Modify: `tests/diagnostics_format.rs`
- Modify: `tests/schema_version.rs`
- Modify: `tests/local_doctor_pack_cli.rs`

**Interfaces:**
- Consumes: `local_doctor_check` entities and `[local-doctor/<doctor>/<check>]` warning keys.
- Produces: `local_check_diagnostic()`, exact-key suppression, and schema-compatible machine formats.

- [ ] **Step 1: Write failing targeted and aggregate rendering tests**

Add `fn local_warning_envelope(doctor: &str, check: &str, path: &str, line: usize) -> QueryEnvelope` to `tests/diagnostics_format.rs`; it must emit one typed entity and its exact stable-keyed warning.

```rust
#[test]
fn local_check_renders_once_with_location_in_sarif() {
    let envelope = local_warning_envelope("pack", "check", "Makefile", 12);
    let diagnostics = envelope_to_diagnostics(&envelope);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].rule_id, "local-doctor/pack/check");
    assert_eq!(diagnostics[0].path.as_deref(), Some("Makefile"));
    assert_eq!(diagnostics[0].line, Some(12));
}
```

Add aggregate `doctor all --format=json` and SARIF tests proving typed entities survive aggregation, matching keyed warnings render once, nonmatching compiled warnings remain, informational results do not fail, and no absolute path/content/literal/control sequence leaks.

- [ ] **Step 2: Run diagnostic tests and confirm RED**

```bash
cargo test --test diagnostics_format --test schema_version --test local_doctor_pack_cli -- --nocapture
```

Expected: existing converter emits generic duplicate warnings and evidence notes.

- [ ] **Step 3: Implement typed conversion before generic warnings**

```rust
fn local_check_diagnostic(entity: &serde_json::Value) -> Option<Diagnostic>;
fn local_warning_key(entity: &serde_json::Value) -> Option<String>;
fn sanitize_diagnostic_message(input: &str) -> String;
```

In `envelope_to_diagnostics`, collect typed local diagnostics and their exact keys first. Render existing evidence normally. Render only warning strings whose exact stable key is not represented by a typed entity. Preserve compiled warnings byte-for-byte. Escape ANSI/OSC/control/bidi characters and enforce diagnostic/output bounds before serialization.

- [ ] **Step 4: Run format and CLI tests**

```bash
cargo test --test diagnostics_format --test schema_version --test local_doctor_pack_cli -- --nocapture
```

Expected: text, JSON, and SARIF carry one correctly located local diagnostic; schema remains `1.0`.

- [ ] **Step 5: Commit Task 5**

```bash
git add -- src/diagnostics.rs tests/diagnostics_format.rs tests/schema_version.rs tests/local_doctor_pack_cli.rs
git commit -m "feat(diagnostics): render typed local doctor findings"
```

### Task 6: Stdio MCP dynamic doctor validation and bounded process control

**Files:**
- Create: `mcp/process-runner.js`
- Create: `mcp/process-runner.test.js`
- Modify: `mcp/index.js`
- Create: `mcp/local-doctor-packs.test.js`
- Modify: `src/capabilities.rs`

**Interfaces:**
- Consumes: runtime `workspace_capabilities.doctor_kinds` from Task 4.
- Produces: `runProcessBounded()`, `doctorKindField`, and `resolveDoctorKindForRepo()` for stdio handlers.

- [ ] **Step 1: Write failing runner and doctor-transport tests**

Test that a syntactically valid local slug is accepted only after resolving the exact root and reading its capabilities; repository B cannot use repository A's name; unknown/inapplicable names return `isError: true` with structured `{ ok: false }` without doctor spawn; aggregates remain allowed; argv contains separate `"doctor"` and name entries; static `DOCTOR_KINDS` is absent. Test stdout/stderr caps, timeout, AbortSignal, SIGTERM then SIGKILL process-tree cleanup, and bounded sanitized errors.

```javascript
test("validates a local doctor against the resolved repository", async () => {
  const result = await resolveDoctorKindForRepo("repo-owned", {
    repoRoot: repoA,
    readCapabilities: async (root) => ({ doctor_kinds: root === repoA ? ["repo-owned"] : [] }),
  });
  assert.equal(result.kind, "repo-owned");
});
```

- [ ] **Step 2: Run Node tests and confirm RED**

```bash
cd /Users/josaum/projects/leio-code
node --test mcp/process-runner.test.js mcp/local-doctor-packs.test.js
```

Expected: imports/helpers do not exist and the static enum still rejects dynamic names.

- [ ] **Step 3: Implement bounded runner and post-root capability validation**

In `mcp/process-runner.js`, implement `createBoundedProcessState({ child, maxStdoutBytes, maxStderrBytes })` to retain only the configured byte prefixes plus truncation flags, and `terminateProcessGroup(child, { code, killGraceMs })` to signal the detached group with `SIGTERM` and then `SIGKILL`. Both helpers are private; `runProcessBounded` is the exported test seam.

```javascript
export async function runProcessBounded(command, args, cwd, {
  timeoutMs,
  maxStdoutBytes,
  maxStderrBytes,
  killGraceMs,
  signal,
  env,
} = {}) {
  return await new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd,
      env,
      detached: process.platform !== "win32",
      stdio: ["ignore", "pipe", "pipe"],
    });
    const state = createBoundedProcessState({ child, maxStdoutBytes, maxStderrBytes });
    const terminate = (code) => terminateProcessGroup(child, { code, killGraceMs });
    const timer = setTimeout(() => terminate("deadline_exceeded"), timeoutMs);
    const onAbort = () => terminate("cancelled");
    signal?.addEventListener("abort", onAbort, { once: true });
    child.stdout.on("data", (chunk) => state.appendStdout(chunk, terminate));
    child.stderr.on("data", (chunk) => state.appendStderr(chunk, terminate));
    child.once("error", reject);
    child.once("close", (code, childSignal) => {
      clearTimeout(timer);
      signal?.removeEventListener("abort", onAbort);
      resolve(state.result(code, childSignal));
    });
  });
}

export const doctorKindField = z.string().trim().min(1).max(64)
  .regex(/^[a-z0-9][a-z0-9-]{0,63}$/);

async function resolveDoctorKindForRepo(kind, {
  repoRoot,
  indexPath,
  timeoutMs,
  signal,
  readCapabilities = readWorkspaceCapabilities,
}) {
  const capabilities = await readCapabilities(repoRoot, { indexPath, timeoutMs, signal });
  const allowed = new Set(["all", "baseline", "ci", ...capabilities.doctor_kinds]);
  if (!allowed.has(kind)) return unknownDoctorToolError(kind, capabilities.doctor_kinds);
  return { ok: true, kind };
}
```

Resolve/canonicalize the root first, then read capabilities for that root, then validate and invoke. Remove `DOCTOR_KINDS` and its Zod enum. Replace the Rust source-text parity test in `src/capabilities.rs` with behavior assertions that the JS source uses bounded strings and runtime capabilities rather than duplicated names.

- [ ] **Step 4: Run stdio MCP checks**

```bash
node --test mcp/process-runner.test.js mcp/local-doctor-packs.test.js
cd mcp && npm run check && npm test
```

Expected: dynamic names, isolation, cancellation, deadline, output cap, and existing MCP contracts pass.

- [ ] **Step 5: Commit Task 6**

```bash
git add -- mcp/process-runner.js mcp/process-runner.test.js mcp/index.js mcp/local-doctor-packs.test.js src/capabilities.rs
git commit -m "feat(mcp): validate local doctors after repository resolution"
```

### Task 7: Apps SDK dynamic validation for both consumers

**Files:**
- Create: `apps-sdk/process-runner.js`
- Create: `apps-sdk/process-runner.test.js`
- Modify: `apps-sdk/server.js`
- Create: `apps-sdk/local-doctor-packs.test.js`
- Modify: `apps-sdk/runtime-contract.test.js`

**Interfaces:**
- Consumes: authenticated `repoTarget`, revision-scoped capabilities, and bounded process semantics.
- Produces: `DoctorRuntimeDependencies`, `resolveDoctorKindForTarget()` shared by `audit_repository_contracts` and `consult_carlos_motta_specialist`, and a directly testable bounded runner.

- [ ] **Step 1: Write failing Apps SDK auth, isolation, dual-consumer, and process tests**

Cover auth rejection before checkout/capabilities, immutable revision target before validation, repository/session A versus B, stale revision cache rejection, hosted non-default `repo_root` rejection, direct audit, Carlos with `include_audit=true`, Carlos skipping validation when false, one argv element per name, `isError: true` plus structured `{ ok: false }` for rejected names, bounded error payloads, timeout, cancellation, process-tree cleanup, output caps, and removal of `doctorKinds`.

```javascript
test("Carlos validates audit_kind only after authorized target resolution", async () => {
  const events = [];
  const runtime = orderedDoctorRuntime(events, { doctorKinds: ["repo-owned"] });
  const result = await buildCarlosMottaSpecialistResult(
    { include_audit: true, audit_kind: "repo-owned" },
    runtime,
  );
  assert.deepEqual(events, ["auth", "target", "capabilities", "doctor"]);
  assert.equal(result.specialist_review.audit.kind, "repo-owned");
});
```

Define `orderedDoctorRuntime(events, { doctorKinds })` in the test to return four async dependency functions—`ensureToolAccess`, `resolveRuntimeRepoTarget`, `readCapabilitiesForTarget`, and `invokeDoctor`—that append their own stable event name before returning deterministic fixtures.

- [ ] **Step 2: Run focused Apps tests and confirm RED**

```bash
cd /Users/josaum/projects/leio-code
node --test apps-sdk/process-runner.test.js apps-sdk/local-doctor-packs.test.js
```

Expected: dynamic helper/runner do not exist and both schemas still depend on `doctorKinds`.

- [ ] **Step 3: Implement hosted bounded runner and shared post-auth target validation**

Add an optional second `runtime` parameter to the internal `buildCarlosMottaSpecialistResult` function. Its production default is an immutable object containing the existing auth, target resolver, capabilities invocation, and doctor invocation functions; tests inject the same four-function interface. The public MCP/Apps tool schema and handler signature do not change.

```javascript
async function resolveDoctorKindForTarget(kind, {
  repoTarget,
  authInfo,
  sessionId,
  timeoutMs,
  signal,
}) {
  const capabilities = await invokeCapabilitiesForResolvedTarget(repoTarget, {
    authInfo, sessionId, timeoutMs, signal,
  });
  const allowed = new Set(["all", "baseline", "ci", ...capabilities.doctor_kinds]);
  if (!allowed.has(kind)) return unknownDoctorToolError(kind, capabilities.doctor_kinds);
  return { ok: true, kind, repoTarget };
}
```

Preserve the order `auth.ensureToolAccess → resolveRuntimeRepoTarget → capabilities for that immutable target → validation → doctor argv`. Use the same helper in direct audit and Carlos. Validate Carlos only when `include_audit` is true. Replace both Zod enums with the bounded slug string. Preserve tenant-scoped environment construction, but replace unbounded/raw runner output with sanitized capped output and deadline/cancellation process-tree termination.

Both hosted capabilities and doctor invocations must append `"--doctor-content-source", "revision-tracked"` as separate trusted argv entries after target resolution. No client input chooses or overrides this hosted value.

- [ ] **Step 4: Run Apps SDK checks**

```bash
node --test apps-sdk/process-runner.test.js apps-sdk/local-doctor-packs.test.js apps-sdk/runtime-contract.test.js
cd apps-sdk && npm run check && npm test
```

Expected: both doctor consumers, auth ordering, target isolation, process controls, and existing Apps contracts pass.

- [ ] **Step 5: Commit Task 7**

```bash
git add -- apps-sdk/process-runner.js apps-sdk/process-runner.test.js apps-sdk/server.js apps-sdk/local-doctor-packs.test.js apps-sdk/runtime-contract.test.js
git commit -m "feat(apps-sdk): authorize repository-local doctors dynamically"
```

### Task 8: Add the Example-owned office-parser policy

**Files:**
- Create in Example: `.leio-code/doctors/office-parsers-clippy-gate.toml`
- Modify in LEIO: `tests/local_doctor_packs.rs`

**Interfaces:**
- Consumes: the assertion schema/evaluator and approved migration behavior.
- Produces: the exact repository-owned policy and synthetic parity fixture. The compiled LEIO doctor remains active until Task 9.

- [ ] **Step 1: Write failing exact-policy fixture tests in LEIO**

Copy the exact intended TOML into a test constant or load the Example file only in the final cross-repository smoke, while synthetic `TempDir` tests materialize Makefile, Cargo workspace/members, verify script, and operations script. Cover all strict failures, target scoping, `--all-targets`, informational optional target/adoption gaps, and strict discovery integrity failures.

Define one fixture wrapper in `tests/local_doctor_packs.rs`:

```rust
struct OfficePolicyFixture {
    local: LocalDoctorFixture,
}

impl OfficePolicyFixture {
    fn complete() -> Self;
    fn without_feature_target() -> Self;
    fn with_makefile(makefile: &str) -> Self;
    fn with_member_manifest(manifest: &str) -> Self;
    fn run(mut self) -> QueryEnvelope;
}
```

`OfficePolicyFixture::run()` calls discovery with `compiled_names_after_office_migration()`, a test helper that returns the real compiled-name set minus only `office-parsers-clippy-gate`. This simulates the intended Task 9 registry state without weakening the production collision rule or deleting the compiled fallback before the Example file is committed.

```rust
#[test]
fn office_policy_keeps_optional_feature_adoption_informational() {
    let fixture = OfficePolicyFixture::without_feature_target();
    let envelope = fixture.run();
    assert!(envelope.warnings.is_empty());
    assert!(!envelope.meta["informational_findings"].as_array().unwrap().is_empty());
}
```

- [ ] **Step 2: Run the office-policy fixture and confirm RED**

```bash
cd /Users/josaum/projects/leio-code
cargo test --test local_doctor_packs office_policy -- --nocapture
```

Expected: fixture fails because the exact migration policy has not been added to the test or Example.

- [ ] **Step 3: Add the exact Example policy and finish fixture parity**

Create `/Users/josaum/projects/example-workspace/.leio-code/doctors/office-parsers-clippy-gate.toml` with ten checks:

```toml
schema_version = 1
name = "office-parsers-clippy-gate"
description = "Validate the office-parsers Rust Clippy gate and its CI and operations-health wiring."
suites = ["all"]

[[checks]]
id = "workspace-clippy-target"
kind = "make-target-contains"
path = "Makefile"
target = "office-parsers-clippy"
contains = ["cargo clippy --workspace"]
severity = "warning"

[[checks]]
id = "workspace-clippy-all-targets"
kind = "make-target-contains"
path = "Makefile"
target = "office-parsers-clippy"
contains = ["--all-targets"]
severity = "warning"

[[checks]]
id = "workspace-clippy-deny-warnings"
kind = "make-target-contains"
path = "Makefile"
target = "office-parsers-clippy"
contains = ["-D warnings"]
severity = "warning"

[[checks]]
id = "vendored-oar-ocr-excluded"
kind = "toml-array-contains"
path = "office-parsers-rs/Cargo.toml"
key = "workspace.exclude"
value = "vendor/oar-ocr"
severity = "warning"

[[checks]]
id = "verify-ci-runs-clippy"
kind = "file-contains"
path = "scripts/verify-ci.sh"
contains = "make office-parsers-clippy"
severity = "warning"

[[checks]]
id = "ops-health-runs-clippy"
kind = "file-contains"
path = "scripts/ops/office-parsers-health.sh"
contains = "run_check office_parsers_clippy make office-parsers-clippy"
severity = "warning"

[[checks]]
id = "arrow-feature-clippy-target"
kind = "make-target-contains"
path = "Makefile"
target = "office-parsers-clippy-features"
contains = ["cargo clippy", "--features arrow", "-D warnings"]
severity = "info"

[[checks]]
id = "arrow-feature-packages-covered"
kind = "cargo-feature-packages-covered-by-make-target"
workspace = "office-parsers-rs/Cargo.toml"
feature = "arrow"
makefile = "Makefile"
target = "office-parsers-clippy-features"
severity = "info"
integrity_severity = "warning"

[[checks]]
id = "verify-ci-runs-arrow-feature-clippy"
kind = "file-contains"
path = "scripts/verify-ci.sh"
contains = "make office-parsers-clippy-features"
severity = "info"

[[checks]]
id = "ops-health-runs-arrow-feature-clippy"
kind = "file-contains"
path = "scripts/ops/office-parsers-health.sh"
contains = "office_parsers_clippy_features"
severity = "info"
```

Do not modify any other Example file. The old compiled doctor still resolves this name until Task 9; the local collision is expected and safe during this intermediate commit.

- [ ] **Step 4: Run fixture parity and commit Example explicitly**

```bash
cd /Users/josaum/projects/leio-code
cargo test --test local_doctor_packs office_policy -- --nocapture

cd /Users/josaum/projects/example-workspace
git add -- .leio-code/doctors/office-parsers-clippy-gate.toml
git commit --only -m "feat(leio): own office parsers clippy doctor" -- .leio-code/doctors/office-parsers-clippy-gate.toml
```

Expected: fixture tests pass; Example commits exactly one new file and preserves unrelated WIP.

- [ ] **Step 5: Commit the LEIO parity fixture separately**

```bash
cd /Users/josaum/projects/leio-code
git add -- tests/local_doctor_packs.rs
git commit -m "test(doctors): pin office parser local policy"
```

### Task 9: Retire the compiled office doctor and prove the real local pack

**Files:**
- Delete: `src/doctors/office_parsers_clippy_gate.rs`
- Modify: `src/doctors/mod.rs`
- Modify: `src/main.rs` only if any obsolete static remnant survived Task 4
- Modify: `src/doctors/self_contract.rs`
- Modify: `tests/cli_surface.rs`
- Modify: `tests/local_doctor_pack_cli.rs`

**Interfaces:**
- Consumes: committed Example policy and complete dynamic engine/transports.
- Produces: no compiled `office-parsers-clippy-gate`; the same public name resolves exclusively from Example.

- [ ] **Step 1: Write failing migration ownership tests**

In `tests/local_doctor_pack_cli.rs`, define `fn capabilities_for_repo(root: &Path) -> WorkspaceCapabilities` by invoking the source-built binary with `--json --repo <root> capabilities` and deserializing `envelope.entities[0]` into the existing capability type.

```rust
#[test]
fn office_clippy_gate_is_local_for_example_not_compiled() {
    assert!(!doctor_names().contains(&"office-parsers-clippy-gate"));
    let capabilities = capabilities_for_repo(Path::new("/Users/josaum/projects/example-workspace"));
    assert!(capabilities.doctor_kinds.contains(&"office-parsers-clippy-gate".to_string()));
}
```

Add self-contract assertions that no `DoctorKind`, `DOCTOR_KINDS`, `doctorKinds`, module declaration, or registry entry remains, while both transport tools retain dynamic runtime validation.

- [ ] **Step 2: Run migration tests and confirm RED**

```bash
cd /Users/josaum/projects/leio-code
cargo test --test local_doctor_pack_cli office_clippy -- --nocapture
cargo test self_contract --lib
```

Expected: compiled doctor/module/registration still exists.

- [ ] **Step 3: Delete the compiled implementation and obsolete mirrors**

Remove `src/doctors/office_parsers_clippy_gate.rs`, its module declaration, Example-profile registry entry, and old self-contract mirror assertions. Do not remove the other compiled office-parser doctors. Do not weaken collision rejection globally; the collision disappears because this one compiled registration is intentionally retired.

- [ ] **Step 4: Run focused migration and real-repository source-built proof**

```bash
cargo fmt --check
cargo test --test local_doctor_packs --test local_doctor_pack_cli --test diagnostics_format --test schema_version -- --nocapture
cargo test self_contract --lib

cargo run --quiet -- --repo /Users/josaum/projects/example-workspace capabilities
cargo run --quiet -- --repo /Users/josaum/projects/example-workspace doctor office-parsers-clippy-gate --format json
cargo run --quiet -- --repo /Users/josaum/projects/example-workspace doctor office-parsers-clippy-gate --format sarif
cargo run --quiet -- --repo /Users/josaum/projects/example-workspace doctor all --format json
```

Expected: capabilities lists the local doctor; targeted JSON/SARIF report zero warnings and informational missing feature-target adoption; `all` includes the opted-in doctor plus pack integrity; `baseline`/`ci` are unchanged.

- [ ] **Step 5: Commit compiled-doctor retirement**

```bash
git add -- src/doctors/mod.rs src/doctors/self_contract.rs tests/cli_surface.rs tests/local_doctor_pack_cli.rs
git add -u -- src/doctors/office_parsers_clippy_gate.rs src/main.rs
git commit -m "refactor(doctors): migrate office clippy gate to Example"
```

### Task 10: Final cross-surface verification and evidence

**Files:**
- Modify only if a focused verification exposes a defect in the files owned by Tasks 1–9.
- Do not create generated evidence or install artifacts in either repository.

**Interfaces:**
- Consumes: all prior task outputs.
- Produces: reproducible completion evidence and final branch/status proof.

- [ ] **Step 1: Run the complete focused Rust surface**

```bash
cd /Users/josaum/projects/leio-code
cargo fmt --check
cargo test --test local_doctor_packs
cargo test --test local_doctor_pack_cli
cargo test --test diagnostics_format --test schema_version
cargo test --test cli_surface
cargo test capabilities::tests --lib
cargo test doctors:: --lib
```

Expected: all focused Rust tests pass.

- [ ] **Step 2: Run stdio MCP and Apps SDK verification**

```bash
node --test mcp/process-runner.test.js mcp/local-doctor-packs.test.js
cd mcp && npm run check && npm test

cd /Users/josaum/projects/leio-code
node --test apps-sdk/process-runner.test.js apps-sdk/local-doctor-packs.test.js apps-sdk/runtime-contract.test.js
cd apps-sdk && npm run check && npm test
```

Expected: dynamic validation, auth/target ordering, repository isolation, cancellation, deadlines, output limits, and existing protocol tests pass.

- [ ] **Step 3: Run broader LEIO verification proportionate to the architecture change**

```bash
cd /Users/josaum/projects/leio-code
cargo test
python3 -m unittest discover -s tests
cargo run -- verify
```

Expected: all pass, or any unrelated committed baseline failure is recorded with exact command/output and confirmed outside the changed paths.

- [ ] **Step 4: Repeat the real Example doctor and suite proof**

```bash
cd /Users/josaum/projects/leio-code
cargo run --quiet -- --repo /Users/josaum/projects/example-workspace capabilities
cargo run --quiet -- --repo /Users/josaum/projects/example-workspace doctor office-parsers-clippy-gate --format json
cargo run --quiet -- --repo /Users/josaum/projects/example-workspace doctor office-parsers-clippy-gate --format sarif
cargo run --quiet -- --repo /Users/josaum/projects/example-workspace doctor all --format json
cargo run --quiet -- --repo /Users/josaum/projects/example-workspace doctor baseline --format json
cargo run --quiet -- --repo /Users/josaum/projects/example-workspace doctor ci --format json
```

Expected: local targeted/all behavior passes; optional Arrow adoption remains informational; fixed suites remain unchanged.

- [ ] **Step 5: Prove branch and dirty-state preservation without committing generated artifacts**

```bash
git -C /Users/josaum/projects/leio-code status --short --branch
git -C /Users/josaum/projects/leio-code log --oneline --decorate -12
git -C /Users/josaum/projects/example-workspace status --short --branch
git -C /Users/josaum/projects/example-workspace log --oneline --decorate -12
```

Expected: LEIO remains on `main`; Example remains on `fix/office-parsers-remaining-holes`; only pre-existing unrelated WIP/log artifacts remain; nothing is pushed or installed.
