//! Integration tests for the generic doctor pack: `env-contract`,
//! `import-boundary`, config-driven `orphan-files` surfaces, and the
//! generic-profile registry contract.
//!
//! Each test stages a synthetic tempdir repo, runs the real indexer, and
//! calls the doctor functions directly (profile routing is covered by the
//! registry tests). The pattern mirrors `tests/cli_cross_language_polish.rs`.

use std::fs;
use std::path::Path;

use leio_code::doctors::doctor_names_for_profile;
use leio_code::doctors::env_contract::doctor_env_contract;
use leio_code::doctors::import_boundary::doctor_import_boundary;
use leio_code::doctors::orphan_files::doctor_orphan_files;
use leio_code::indexer::{build_or_update_index, default_index_path};
use tempfile::TempDir;

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(&path, body).expect("write file");
}

fn build(root: &Path) -> leio_code::model::RepoIndex {
    // Disable DuckDB sidecar — we only need the JSON index.
    // SAFETY: the value is constant across every test in this process.
    unsafe { std::env::set_var("LEIO_DISABLE_SEARCH_SIDECAR", "1") };
    let index_path = default_index_path(root);
    build_or_update_index(root, &index_path, true).expect("build index")
}

// ---------------------------------------------------------------------------
// env-contract
// ---------------------------------------------------------------------------

// Why: without any declaration source the doctor cannot distinguish
// "undeclared" from "declared outside the repo", so it must stay silent
// instead of flooding arbitrary repos with false positives.
#[test]
fn env_contract_is_inactive_without_declaration_sources() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    write(
        root,
        "app.js",
        "const token = process.env.SVC_TOKEN_A;\nconsole.log(token);\n",
    );

    let index = build(root);
    let envelope = doctor_env_contract(&index, root);

    assert!(
        envelope.warnings.is_empty(),
        "zero declaration sources must mean zero warnings, got: {:?}",
        envelope.warnings
    );
    assert!(
        envelope.summary.contains("inactive"),
        "summary must explain the doctor is inactive, got: {}",
        envelope.summary
    );
}

// Why: the doctor's whole point — once the repo declares env vars somewhere,
// a read of an undeclared var is a deploy-time landmine and must warn,
// while declared vars must not.
#[test]
fn env_contract_flags_only_the_undeclared_read() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    write(root, ".env.example", "SVC_TOKEN_A=example\n");
    write(
        root,
        "app.js",
        "const a = process.env.SVC_TOKEN_A;\nconst b = process.env.SVC_TOKEN_B;\n",
    );

    let index = build(root);
    let envelope = doctor_env_contract(&index, root);

    assert_eq!(
        envelope.warnings.len(),
        1,
        "exactly one undeclared var expected, got: {:?}",
        envelope.warnings
    );
    assert!(
        envelope.warnings[0].contains("SVC_TOKEN_B"),
        "warning must name the undeclared var, got: {}",
        envelope.warnings[0]
    );
    assert!(
        envelope.warnings[0].contains("app.js"),
        "warning must say where the var is read, got: {}",
        envelope.warnings[0]
    );
    // file:line evidence for the read site.
    assert!(
        envelope
            .evidence
            .iter()
            .any(|item| item.path == "app.js" && item.line.is_some()),
        "expected file:line evidence for the undeclared read, got: {:?}",
        envelope.evidence
    );
    // The declared var must not be flagged.
    assert!(
        !envelope
            .warnings
            .iter()
            .any(|warning| warning.contains("SVC_TOKEN_A")),
        "declared var must not warn: {:?}",
        envelope.warnings
    );
}

// Why: the config allowlist is the documented escape hatch for vars that are
// intentionally provided outside the repo; it must suppress the warning.
#[test]
fn env_contract_allowlist_suppresses_warnings() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    write(root, ".env.example", "SVC_TOKEN_A=example\n");
    write(
        root,
        ".leio-code/config.toml",
        "[doctors.env_contract]\nallow = [\"SVC_TOKEN_*\"]\n",
    );
    write(
        root,
        "app.js",
        "const a = process.env.SVC_TOKEN_A;\nconst b = process.env.SVC_TOKEN_B;\n",
    );

    let index = build(root);
    let envelope = doctor_env_contract(&index, root);

    assert!(
        envelope.warnings.is_empty(),
        "allowlisted vars must not warn, got: {:?}",
        envelope.warnings
    );
    assert!(
        envelope.summary.contains("scanned"),
        "doctor must still be active (declaration sources exist), got: {}",
        envelope.summary
    );
}

// ---------------------------------------------------------------------------
// import-boundary
// ---------------------------------------------------------------------------

// Why: with zero configured rules the doctor must be a safe no-op everywhere
// (this is what makes registering it under every profile harmless).
#[test]
fn import_boundary_is_inactive_without_rules() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    write(
        root,
        "core/service.ts",
        "import { v } from \"../verticals/billing/api\";\nexport const used = v;\n",
    );
    write(root, "verticals/billing/api.ts", "export const v = 1;\n");

    let index = build(root);
    let envelope = doctor_import_boundary(&index, root);

    assert!(
        envelope.warnings.is_empty(),
        "no rules means no warnings, got: {:?}",
        envelope.warnings
    );
    assert!(
        envelope
            .summary
            .contains("no import-boundary rules configured"),
        "summary must explain inactivity, got: {}",
        envelope.summary
    );
}

// Why: the core invariant — a file under from_prefix importing into a denied
// prefix is an architecture violation and must warn with rule + location.
#[test]
fn import_boundary_flags_relative_import_violation() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    write(
        root,
        ".leio-code/config.toml",
        "[[doctors.import_boundary.rules]]\n\
         name = \"core-isolated\"\n\
         from_prefix = \"core/\"\n\
         deny_prefixes = [\"verticals/\"]\n",
    );
    write(
        root,
        "core/service.ts",
        "import { v } from \"../verticals/billing/api\";\nexport const used = v;\n",
    );
    write(root, "verticals/billing/api.ts", "export const v = 1;\n");

    let index = build(root);
    let envelope = doctor_import_boundary(&index, root);

    assert_eq!(
        envelope.warnings.len(),
        1,
        "exactly one violation expected, got: {:?}",
        envelope.warnings
    );
    assert!(
        envelope.warnings[0].contains("core-isolated"),
        "warning must name the rule, got: {}",
        envelope.warnings[0]
    );
    assert!(
        envelope.warnings[0].contains("core/service.ts"),
        "warning must name the violating file, got: {}",
        envelope.warnings[0]
    );
    assert!(
        envelope
            .evidence
            .iter()
            .any(|item| item.path == "core/service.ts"),
        "expected evidence anchored on the violating file, got: {:?}",
        envelope.evidence
    );
}

// Why: rules must only constrain files under from_prefix — flagging imports
// from unrelated paths would make boundary rules unusable.
#[test]
fn import_boundary_ignores_files_outside_from_prefix() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    write(
        root,
        ".leio-code/config.toml",
        "[[doctors.import_boundary.rules]]\n\
         name = \"core-isolated\"\n\
         from_prefix = \"core/\"\n\
         deny_prefixes = [\"verticals/\"]\n",
    );
    // The violating import lives OUTSIDE from_prefix — must not warn.
    write(
        root,
        "tools/widget.ts",
        "import { v } from \"../verticals/billing/api\";\nexport const used = v;\n",
    );
    write(root, "verticals/billing/api.ts", "export const v = 1;\n");

    let index = build(root);
    let envelope = doctor_import_boundary(&index, root);

    assert!(
        envelope.warnings.is_empty(),
        "files outside from_prefix must be untouched, got: {:?}",
        envelope.warnings
    );
}

// ---------------------------------------------------------------------------
// orphan-files (config-driven surfaces)
// ---------------------------------------------------------------------------

// Why: config surfaces are what make orphan-files portable — a generic repo
// that opts in must get orphan detection scoped to its own paths.
#[test]
fn orphan_files_config_surfaces_drive_scanning_on_generic_repo() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    write(
        root,
        ".leio-code/config.toml",
        "[doctors.orphan_files]\nsurfaces = [\"src/\"]\n",
    );
    write(root, "src/orphan_module.ts", "export const dead = 1;\n");
    write(root, "src/used.ts", "export const used = 1;\n");
    write(
        root,
        "src/index.ts",
        "import { used } from \"./used\";\nexport const boot = used;\n",
    );

    let index = build(root);
    let envelope = doctor_orphan_files(&index, root);

    assert!(
        envelope
            .warnings
            .iter()
            .any(|warning| warning.contains("src/orphan_module.ts")),
        "unimported file under a configured surface must be flagged, got: {:?}",
        envelope.warnings
    );
    assert!(
        !envelope
            .warnings
            .iter()
            .any(|warning| warning.contains("src/used.ts")),
        "imported file must not be flagged, got: {:?}",
        envelope.warnings
    );
}

// Why: without configured surfaces a generic repo must get a clean no-op —
// inheriting Example's hardcoded path list on foreign repos would be noise.
#[test]
fn orphan_files_is_inactive_on_generic_repo_without_surfaces() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    write(root, "src/orphan_module.ts", "export const dead = 1;\n");

    let index = build(root);
    let envelope = doctor_orphan_files(&index, root);

    assert!(
        envelope.warnings.is_empty(),
        "generic repo without surfaces must be a no-op, got: {:?}",
        envelope.warnings
    );
    assert!(
        envelope
            .summary
            .contains("no orphan-files surfaces configured"),
        "summary must explain inactivity, got: {}",
        envelope.summary
    );
}

// ---------------------------------------------------------------------------
// registry
// ---------------------------------------------------------------------------

// Why: the generic profile is the product surface for arbitrary repos; this
// pins exactly which doctors light up there so accidental registrations (or
// silent drops) are caught.
#[test]
fn generic_profile_exposes_the_generic_doctor_pack() {
    let mut names = doctor_names_for_profile("generic");
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "codex-orchestration",
            "env-contract",
            "import-boundary",
            "leio-release-coherence",
            "orphan-files",
            "redis-key-hygiene",
            "repo-hygiene",
            "rust-toolchain-pin-coherence",
            "skill-contract",
            "slop",
        ]
    );
}

// Why: env-contract on the Example workspace would flag deploy-time-provided
// vars and break the `doctor all` CI green contract; this pins the exclusion.
#[test]
fn example_profile_excludes_env_contract_but_gets_import_boundary() {
    let names = doctor_names_for_profile("example");
    assert!(
        !names.contains(&"env-contract"),
        "env-contract must NOT be registered for example, got: {names:?}"
    );
    assert!(
        names.contains(&"import-boundary"),
        "import-boundary (no-op without rules) should be available on example, got: {names:?}"
    );
}

// Why: artifact-reuse enforces the artifacts/ reorg, a fact about THIS
// workspace's layout — it must light up on example but never leak into the
// generic product surface (it would false-positive on arbitrary repos).
#[test]
fn py_rust_boundary_is_example_only() {
    assert!(
        doctor_names_for_profile("example").contains(&"py-rust-boundary"),
        "py-rust-boundary must be registered for the example profile"
    );
    assert!(
        !doctor_names_for_profile("generic").contains(&"py-rust-boundary"),
        "py-rust-boundary must NOT leak into the generic profile"
    );
}

#[test]
fn artifact_reuse_is_example_only() {
    assert!(
        doctor_names_for_profile("example").contains(&"artifact-reuse"),
        "artifact-reuse must be registered for the example profile"
    );
    assert!(
        !doctor_names_for_profile("generic").contains(&"artifact-reuse"),
        "artifact-reuse must NOT leak into the generic profile"
    );
}
