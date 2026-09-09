//! Behavioral integration tests for the generic doctor pack (POSITIONING.md P1 #1).
//!
//! These drive the full `build_or_update_index` → doctor pipeline on synthetic
//! tempdir repos — no mocks — because the pack's contract is behavioral:
//! every generic doctor must (a) no-op silently when its inputs are absent,
//! and (b) fire precise warnings when the configured invariant is violated.
//! A generic repo that opts into nothing must see `doctor all` stay green.

use std::fs;
use std::path::Path;

use leio_code::doctors::env_contract::doctor_env_contract;
use leio_code::doctors::import_boundary::doctor_import_boundary;
use leio_code::indexer::build_or_update_index;
use tempfile::TempDir;

/// Write a synthetic repo. Each tuple is `(relative_path, content)`.
fn write_repo(files: &[(&str, &str)]) -> TempDir {
    let tmp = tempfile::tempdir().expect("create tempdir");
    for (rel, content) in files {
        let full = tmp.path().join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(&full, content).expect("write file");
    }
    tmp
}

fn build(tmp: &Path) -> leio_code::model::RepoIndex {
    let index_path = tmp.join(".leio-code").join("index.json");
    fs::create_dir_all(index_path.parent().expect("index parent")).expect("mkdir .leio-code");
    build_or_update_index(tmp, &index_path, true).expect("build index")
}

// Why: with zero declaration sources the doctor cannot distinguish
// "undeclared" from "declared outside the repo"; firing would make the pack
// unusable on minimal repos, so silence is the contract.
#[test]
fn env_contract_inactive_without_declaration_sources() {
    let tmp = write_repo(&[(
        "app/main.py",
        "import os\nurl = os.environ[\"TOTALLY_UNDECLARED_URL\"]\n",
    )]);
    let index = build(tmp.path());
    let envelope = doctor_env_contract(&index, tmp.path());
    assert!(
        envelope.warnings.is_empty(),
        "expected no warnings without declaration sources; got {:?}",
        envelope.warnings
    );
    assert!(
        envelope.summary.to_lowercase().contains("declaration"),
        "summary should explain the inactive gate; got {}",
        envelope.summary
    );
}

// Why: this is the core invariant — a read with at least one declaration
// source present and no declaration anywhere is the .env-outage class of bug.
#[test]
fn env_contract_flags_undeclared_read_when_sources_exist() {
    let tmp = write_repo(&[
        (".env.example", "DATABASE_URL=postgres://localhost/dev\n"),
        (
            "app/main.py",
            "import os\na = os.environ[\"DATABASE_URL\"]\nb = os.environ[\"STRIPE_WEBHOOK_SECRET\"]\n",
        ),
    ]);
    let index = build(tmp.path());
    let envelope = doctor_env_contract(&index, tmp.path());
    assert_eq!(
        envelope.warnings.len(),
        1,
        "exactly the undeclared var should warn; got {:?}",
        envelope.warnings
    );
    assert!(
        envelope.warnings[0].contains("STRIPE_WEBHOOK_SECRET"),
        "warning should name the undeclared var; got {}",
        envelope.warnings[0]
    );
    assert!(
        !envelope
            .warnings
            .iter()
            .any(|warning| warning.contains("DATABASE_URL")),
        "declared var must not warn"
    );
}

// Why: the allowlist is the escape hatch for intentionally-external vars;
// if it stops suppressing, teams turn the doctor off entirely.
#[test]
fn env_contract_allowlist_suppresses_configured_vars() {
    let tmp = write_repo(&[
        (".env.example", "DATABASE_URL=postgres://localhost/dev\n"),
        (
            ".leio-code/config.toml",
            "workspace_profile = \"generic\"\n\n[doctors.env_contract]\nallow = [\"STRIPE_*\"]\n",
        ),
        (
            "app/main.py",
            "import os\nb = os.environ[\"STRIPE_WEBHOOK_SECRET\"]\n",
        ),
    ]);
    let index = build(tmp.path());
    let envelope = doctor_env_contract(&index, tmp.path());
    assert!(
        envelope.warnings.is_empty(),
        "allowlisted prefix must suppress; got {:?}",
        envelope.warnings
    );
}

// Why: zero rules must mean zero cost and zero noise — that is what makes
// registering the doctor under every profile safe.
#[test]
fn import_boundary_inactive_without_rules() {
    let tmp = write_repo(&[("core/a.ts", "import { x } from \"../verticals/b\";\n")]);
    let index = build(tmp.path());
    let envelope = doctor_import_boundary(&index, tmp.path());
    assert!(envelope.warnings.is_empty());
    assert!(
        envelope.summary.contains("no import-boundary rules"),
        "summary should say the doctor is inactive; got {}",
        envelope.summary
    );
}

// Why: the configured boundary is the product promise (ArchUnit-style,
// cross-language); a crossing edge must fire with file evidence.
#[test]
fn import_boundary_flags_configured_crossing() {
    let tmp = write_repo(&[
        (
            ".leio-code/config.toml",
            "workspace_profile = \"generic\"\n\n[[doctors.import_boundary.rules]]\nname = \"core-isolated\"\nfrom_prefix = \"core/\"\ndeny_prefixes = [\"verticals/\"]\n",
        ),
        (
            "core/a.ts",
            "import { x } from \"../verticals/b\";\nexport const y = x;\n",
        ),
        ("verticals/b.ts", "export const x = 1;\n"),
        // Inside-the-boundary import that must NOT warn.
        (
            "core/c.ts",
            "import { y } from \"./a\";\nexport const z = y;\n",
        ),
    ]);
    let index = build(tmp.path());
    let envelope = doctor_import_boundary(&index, tmp.path());
    assert_eq!(
        envelope.warnings.len(),
        1,
        "exactly the crossing import should warn; got {:?}",
        envelope.warnings
    );
    let warning = &envelope.warnings[0];
    assert!(
        warning.contains("core-isolated") || warning.contains("verticals/"),
        "warning should reference the rule or denied target; got {warning}"
    );
    assert!(
        !envelope
            .warnings
            .iter()
            .any(|warning| warning.contains("core/c.ts")),
        "within-boundary import must not warn"
    );
}
