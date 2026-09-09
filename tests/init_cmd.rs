//! Integration tests for `leio_code::init::run_init` — the printing-free core
//! of the `leio-code init` onboarding command. Tempdir-based, calling the
//! library function directly (same fixture pattern as `watch_smoke.rs`).
// Rust guideline compliant 2026-02-21

use std::fs;
use std::path::Path;

use leio_code::init::{ConfigAction, STARTER_CONFIG, run_init};
use tempfile::TempDir;

/// Seed a minimal polyglot repo: one Rust file and one Python file that
/// references an env var, so facet counts have something to detect.
fn seed_repo() -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    fs::write(
        tmp.path().join("main.rs"),
        "fn main() { run(); }\nfn run() {}\n",
    )
    .expect("write rust seed");
    fs::write(
        tmp.path().join("app.py"),
        "import os\nKEY = os.getenv(\"INIT_TEST_VAR\")\n",
    )
    .expect("write python seed");
    tmp
}

fn config_path(root: &Path) -> std::path::PathBuf {
    root.join(".leio-code").join("config.toml")
}

// WHY: a stranger's very first `init` must yield a generic config AND a
// working index in one shot — that is the whole self-service onboarding promise.
#[test]
fn fresh_repo_creates_config_and_index() {
    let tmp = seed_repo();
    let root = tmp.path();

    let report = run_init(root, false).expect("init succeeds on a fresh repo");

    let written = fs::read_to_string(config_path(root)).expect("config written");
    assert_eq!(
        written, STARTER_CONFIG,
        "starter config must be byte-stable"
    );
    assert!(written.contains("workspace_profile = \"generic\""));

    assert_eq!(report.config_action, ConfigAction::Created);
    assert!(
        root.join(".leio-code").join("index.json").exists(),
        "index.json must exist after init"
    );
    assert!(
        report.file_count >= 2,
        "both seeded source files must be indexed (got {})",
        report.file_count
    );
    assert!(report.languages.iter().any(|lang| lang == "rust"));
    assert!(report.languages.iter().any(|lang| lang == "python"));
    assert!(
        report.env_var_count >= 1,
        "os.getenv(\"INIT_TEST_VAR\") must be detected as an env var facet"
    );
    assert_eq!(report.capabilities.workspace_profile, "generic");
    assert!(
        !report.next_steps.is_empty(),
        "next steps must be populated"
    );
    assert!(
        report.mcp_hint.contains("mcpServers"),
        "MCP wiring hint must carry an .mcp.json server entry"
    );
}

// WHY: re-running init must never clobber the user's config — onboarding has
// to be safely re-runnable (idempotency invariant).
#[test]
fn second_run_without_force_keeps_config_bytes() {
    let tmp = seed_repo();
    let root = tmp.path();
    run_init(root, false).expect("first init succeeds");

    let before = fs::read(config_path(root)).expect("read config after first run");

    let report = run_init(root, false).expect("second init succeeds");
    let after = fs::read(config_path(root)).expect("read config after second run");

    assert_eq!(report.config_action, ConfigAction::Kept);
    assert_eq!(
        before, after,
        "config must be byte-identical when --force is not passed"
    );
}

// WHY: --force is the documented recovery path back to a known-good starter
// config after a bad hand edit.
#[test]
fn force_rewrites_manually_edited_config_to_template() {
    let tmp = seed_repo();
    let root = tmp.path();
    run_init(root, false).expect("first init succeeds");

    fs::write(
        config_path(root),
        "version = 1\nworkspace_profile = \"generic\"\n# hand edit\n",
    )
    .expect("manual config edit");

    let report = run_init(root, true).expect("forced init succeeds");

    assert_eq!(report.config_action, ConfigAction::Overwritten);
    let rewritten = fs::read_to_string(config_path(root)).expect("read rewritten config");
    assert_eq!(
        rewritten, STARTER_CONFIG,
        "--force must restore the exact starter template"
    );
}
