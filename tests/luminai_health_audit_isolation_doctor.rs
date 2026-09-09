use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use leio_code::doctors::luminai_health_audit_isolation::doctor_luminai_health_audit_isolation;
use leio_code::doctors::{ci_doctor_names, doctor_names_for_profile};
use leio_code::indexer::build_or_update_index;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn write(root: &Path, relative: &str, body: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent");
    }
    fs::write(path, body).expect("write");
}

fn fixture(files: &[(&str, &str)]) -> TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    write(
        tmp.path(),
        ".leio-code/config.toml",
        "workspace_profile = \"example\"\n",
    );
    for (path, body) in files {
        write(tmp.path(), path, body);
    }
    tmp
}

fn result(root: &Path) -> leio_code::model::QueryEnvelope {
    let index_path = root.join(".leio-code/index.json");
    let index = build_or_update_index(root, &index_path, true).expect("index");
    doctor_luminai_health_audit_isolation(&index, root)
}

fn kinds(root: &Path) -> Vec<String> {
    result(root).evidence.into_iter().map(|e| e.kind).collect()
}
fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn command(root: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
}

fn init_dirty_repo() -> (TempDir, String, PathBuf) {
    let tmp = fixture(&[("cartridges/luminai_revenue_cycle/app.py", "pass\n")]);
    command(tmp.path(), &["init", "-q"]);
    command(tmp.path(), &["add", "."]);
    command(
        tmp.path(),
        &[
            "-c",
            "user.email=a@b.c",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "base",
        ],
    );
    let base = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(tmp.path())
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    let frozen = tmp.path().join("cartridges/health_audit/dirty.txt");
    write(
        tmp.path(),
        "cartridges/health_audit/dirty.txt",
        "baseline\n",
    );
    (tmp, base, frozen)
}

fn sha(bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(bytes);
    format!("{:x}", hash.finalize())
}

fn directory_sha(name: &str, bytes: &[u8]) -> String {
    let mut stream = Vec::new();
    stream.extend_from_slice(&(name.len() as u64).to_be_bytes());
    stream.extend_from_slice(name.as_bytes());
    stream.push(b'F');
    stream.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    stream.extend_from_slice(bytes);
    sha(&stream)
}

fn manifest(
    path: &Path,
    base: &str,
    frozen_rel: &str,
    status: &str,
    digest: Option<String>,
) -> PathBuf {
    let file = path.join("dirty-manifest.json");
    let body = serde_json::json!({"version":1,"base_commit":base,"entries":[{"path":frozen_rel,"status":status,"original_path":null,"index_mode":null,"index_blob_oid":null,"worktree_kind":if digest.is_none(){"missing"}else if frozen_rel.ends_with('/'){"directory"}else{"file"},"worktree_sha256":digest} ]});
    fs::write(&file, serde_json::to_vec(&body).unwrap()).unwrap();
    file
}

#[test]
fn absent_luminai_is_inactive() {
    let tmp = fixture(&[("README.md", "ordinary workspace\n")]);
    let envelope = result(tmp.path());
    assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
    assert_eq!(envelope.meta.unwrap()["reason"], "luminai_surfaces_absent");
}

#[test]
fn doctor_source_text_alone_does_not_activate_luminai() {
    let tmp = fixture(&[
        (
            "leio-code/src/doctors/luminai_health_audit_isolation.rs",
            "const X: &str = \"luminai_revenue_cycle\";\n",
        ),
        (
            "deploy/secret-sets/hospital_audit.env.example",
            "HEALTH_AUDIT_TOKEN=x\n",
        ),
    ]);
    assert_eq!(
        result(tmp.path()).meta.unwrap()["reason"],
        "luminai_surfaces_absent"
    );
}

#[test]
fn uppercase_unrelated_identities_are_rejected() {
    let tmp = fixture(&[(
        "cartridges/luminai_revenue_cycle/app.py",
        "PACTO_DATABASE_URL=x\nJAIPAY_QUEUE=x\nFITNESS_SCHEMA=x\n",
    )]);
    let envelope = result(tmp.path());
    assert!(
        envelope
            .evidence
            .iter()
            .any(|item| item.kind == "luminai_unrelated_product_identity" && item.line == Some(1))
    );
}

#[test]
fn target_compose_file_is_exactly_selected_and_unsafe_or_unreferenced_files_are_ignored() {
    let tmp = fixture(&[
        (
            "deploy/targets/hospital_rcm.toml",
            "cartridges = [\"luminai_revenue_cycle\"]\ncompose_file = \"deploy/compose/luminai.yml\"\n",
        ),
        (
            "deploy/compose/luminai.yml",
            "services:\n  api:\n    image: health-audit-api\n",
        ),
        (
            "deploy/compose/unreferenced.yml",
            "HEALTH_AUDIT_UNREFERENCED=x\n",
        ),
    ]);
    let fixture_name = tmp.path().file_name().unwrap().to_string_lossy();
    let outside_name = format!("{fixture_name}-outside-forbidden.yml");
    let outside = tmp.path().parent().unwrap().join(&outside_name);
    fs::write(&outside, "HEALTH_AUDIT_OUTSIDE=x\n").expect("write traversal target");
    write(
        tmp.path(),
        "deploy/targets/unsafe_luminai.toml",
        &format!(
            "cartridges = [\"luminai_revenue_cycle\"]\ncompose_file = \"../{outside_name}\"\n"
        ),
    );
    let envelope = result(tmp.path());
    assert!(
        envelope
            .evidence
            .iter()
            .any(|item| item.kind == "luminai_ha_operational_identity"
                && item.path.ends_with("deploy/compose/luminai.yml"))
    );
    assert!(
        !envelope
            .evidence
            .iter()
            .any(|item| item.path.ends_with("unreferenced.yml"))
    );
    assert!(
        !envelope
            .evidence
            .iter()
            .any(|item| item.path == outside.display().to_string())
    );
    fs::remove_file(outside).expect("remove traversal target");
}

#[cfg(unix)]
#[test]
fn activated_compose_symlink_escaping_root_is_not_selected() {
    use std::os::unix::fs::symlink;

    let tmp = fixture(&[(
        "deploy/targets/hospital_rcm.toml",
        "cartridges = [\"luminai_revenue_cycle\"]\ncompose_file = \"deploy/compose/escape.yml\"\n",
    )]);
    let outside = tempfile::NamedTempFile::new().expect("outside forbidden file");
    fs::write(outside.path(), "HEALTH_AUDIT_ESCAPED=x\n").expect("write outside forbidden file");
    let link = tmp.path().join("deploy/compose/escape.yml");
    fs::create_dir_all(link.parent().expect("link parent")).expect("create link parent");
    symlink(outside.path(), &link).expect("create escaping symlink");

    let envelope = result(tmp.path());
    assert!(
        !envelope
            .evidence
            .iter()
            .any(|item| item.path.ends_with("deploy/compose/escape.yml")
                || item.path == outside.path().display().to_string()),
        "escaping symlink must not be selected: {:?}",
        envelope.evidence
    );
}

#[test]
fn clean_luminai_owns_its_database_redis_and_image_identities() {
    let tmp = fixture(&[(
        "cartridges/luminai_revenue_cycle/config.py",
        "LUMINAI_DATABASE_URL = 'postgres://luminai'\nREDIS_KEY = 'luminai:claims'\nIMAGE = 'luminai-api'\n",
    )]);
    assert!(result(tmp.path()).warnings.is_empty());
}

#[test]
fn health_audit_import_env_redis_and_operational_identity_have_stable_kinds() {
    let tmp = fixture(&[(
        "cartridges/luminai_revenue_cycle/bad.py",
        "import cartridges.health_audit\nHEALTH_AUDIT_URL = 'x'\nkey = 'ha:queue'\nimage = 'jquant/example-health-audit-api'\n",
    )]);
    let found = kinds(tmp.path());
    for expected in [
        "luminai_ha_forbidden_import",
        "luminai_ha_forbidden_env",
        "luminai_ha_forbidden_redis",
        "luminai_ha_operational_identity",
    ] {
        assert!(
            found.iter().any(|kind| kind == expected),
            "missing {expected}: {found:?}"
        );
    }
}

#[test]
fn luminai_workflow_asset_is_a_product_surface_but_shared_runtime_is_not() {
    let tmp = fixture(&[
        (
            "example-gateway/assets/workflows/luminai_revenue_cycle/luminai-revenue-cycle-v1.yaml",
            "tool: cartridges.health_audit\nruntime_env: HEALTH_AUDIT_URL\nservice: health-audit-runtime\n",
        ),
        (
            "example-gateway/src/workflow/shared_runtime.rs",
            "import cartridges.health_audit\n",
        ),
        (
            "cartridges/luminai_revenue_cycle/tests/test_isolation.py",
            "assert 'health_audit' not in runtime_dependencies\n",
        ),
    ]);

    let envelope = result(tmp.path());
    for expected in [
        "luminai_ha_forbidden_import",
        "luminai_ha_forbidden_env",
        "luminai_ha_operational_identity",
    ] {
        assert!(
            envelope.evidence.iter().any(|item| item.kind == expected
                && item.path.ends_with("luminai-revenue-cycle-v1.yaml")),
            "missing {expected}: {:?}",
            envelope.evidence
        );
    }
    assert!(
        !envelope
            .evidence
            .iter()
            .any(|item| item.path.ends_with("shared_runtime.rs")
                || item.path.ends_with("test_isolation.py")),
        "generic shared runtime and test files must stay outside product scanning: {:?}",
        envelope.evidence
    );
}

#[test]
fn unrelated_product_references_cannot_be_allowlisted() {
    let tmp = fixture(&[
        (
            ".leio-code/config.toml",
            "workspace_profile = \"example\"\n[doctors.luminai_health_audit_isolation]\nallow = [\"pacto\", \"jaipay\"]\n",
        ),
        (
            "cartridges/luminai_revenue_cycle/bad.py",
            "import cartridges.pacto\nurl = 'jaipay://db'\nkey = 'fitness:member'\n",
        ),
    ]);
    let found = kinds(tmp.path());
    assert!(
        found
            .iter()
            .any(|kind| kind == "luminai_unrelated_product_import")
    );
}

#[test]
fn semantic_target_traversal_checks_profile_secret_compose_and_transitive_target() {
    let tmp = fixture(&[
        (
            "deploy/targets/hospital_rcm.toml",
            "cartridges = [\"luminai_revenue_cycle\"]\nprofile = \"luminai\"\nsecret_set = \"luminai\"\ncompose_service = \"luminai\"\ninherits = [\"shared\"]\n",
        ),
        ("deploy/profiles/luminai.env", "HEALTH_AUDIT_TOKEN=x\n"),
        (
            "deploy/secret-sets/luminai.env.example",
            "DATABASE_URL=postgres://hospital_audit\n",
        ),
        (
            "deploy/docker-compose.luminai.yml",
            "services:\n  luminai:\n    image: health-audit-api\n",
        ),
        ("deploy/targets/shared.toml", "cartridges = [\"pacto\"]\n"),
    ]);
    let found = kinds(tmp.path());
    assert!(found.iter().any(|kind| kind == "luminai_ha_forbidden_env"));
    assert!(
        found
            .iter()
            .any(|kind| kind == "luminai_ha_operational_identity")
    );
}

#[test]
fn explicit_base_rejects_frozen_committed_path_and_ci_missing_base_fails_closed() {
    let _lock = env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = fixture(&[("cartridges/luminai_revenue_cycle/app.py", "pass\n")]);
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(tmp.path())
        .status()
        .unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(tmp.path())
        .status()
        .unwrap();
    Command::new("git")
        .args([
            "-c",
            "user.email=a@b.c",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "base",
        ])
        .current_dir(tmp.path())
        .status()
        .unwrap();
    let base = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    write(tmp.path(), "cartridges/health_audit/frozen.py", "changed\n");
    Command::new("git")
        .args(["add", "."])
        .current_dir(tmp.path())
        .status()
        .unwrap();
    Command::new("git")
        .args([
            "-c",
            "user.email=a@b.c",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "bad",
        ])
        .current_dir(tmp.path())
        .status()
        .unwrap();
    unsafe {
        std::env::set_var(
            "LEIO_LUMINAI_CHANGE_BASE",
            String::from_utf8(base.stdout).unwrap().trim(),
        );
    }
    assert!(
        kinds(tmp.path())
            .iter()
            .any(|kind| kind == "luminai_ha_frozen_path")
    );
    unsafe {
        std::env::remove_var("LEIO_LUMINAI_CHANGE_BASE");
    }
}

#[test]
fn dirty_manifest_exempts_only_exact_unchanged_frozen_record() {
    let _lock = env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (tmp, base, frozen) = init_dirty_repo();
    let path = manifest(
        tmp.path(),
        &base,
        "cartridges/health_audit/",
        "??",
        Some(directory_sha("dirty.txt", b"baseline\n")),
    );
    unsafe {
        std::env::set_var("LEIO_LUMINAI_CHANGE_BASE", &base);
        std::env::set_var("LEIO_LUMINAI_PREEXISTING_DIRTY_MANIFEST", &path);
    }
    assert!(
        !kinds(tmp.path())
            .iter()
            .any(|kind| kind == "luminai_ha_frozen_path")
    );
    fs::write(&frozen, "changed\n").unwrap();
    let changed = result(tmp.path());
    assert!(
        changed
            .evidence
            .iter()
            .any(|item| item.kind == "luminai_ha_frozen_path"),
        "{:?} {:?}",
        changed.warnings,
        changed.evidence
    );
    unsafe {
        std::env::remove_var("LEIO_LUMINAI_CHANGE_BASE");
        std::env::remove_var("LEIO_LUMINAI_PREEXISTING_DIRTY_MANIFEST");
    }
}

#[test]
fn dirty_manifest_rejects_staged_removed_new_and_malformed_records() {
    let _lock = env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (tmp, base, frozen) = init_dirty_repo();
    let path = manifest(
        tmp.path(),
        &base,
        "cartridges/health_audit/",
        "??",
        Some(directory_sha("dirty.txt", b"baseline\n")),
    );
    unsafe {
        std::env::set_var("LEIO_LUMINAI_CHANGE_BASE", &base);
        std::env::set_var("LEIO_LUMINAI_PREEXISTING_DIRTY_MANIFEST", &path);
    }
    command(tmp.path(), &["add", "cartridges/health_audit/dirty.txt"]);
    assert!(
        kinds(tmp.path())
            .iter()
            .any(|kind| kind == "luminai_ha_frozen_path")
    );
    command(
        tmp.path(),
        &["reset", "--", "cartridges/health_audit/dirty.txt"],
    );
    fs::remove_file(&frozen).unwrap();
    assert!(
        kinds(tmp.path())
            .iter()
            .any(|kind| kind == "luminai_ha_frozen_path")
    );
    fs::write(&path, br#"{"version":1,"base_commit":"bad","entries":[{"path":"../bad","status":"??","original_path":null,"index_mode":null,"index_blob_oid":null,"worktree_kind":"missing","worktree_sha256":null},{"path":"../bad","status":"??","original_path":null,"index_mode":null,"index_blob_oid":null,"worktree_kind":"missing","worktree_sha256":null}]}"#).unwrap();
    assert!(
        kinds(tmp.path())
            .iter()
            .any(|kind| kind == "luminai_changeset_unconfigured")
    );
    unsafe {
        std::env::remove_var("LEIO_LUMINAI_CHANGE_BASE");
        std::env::remove_var("LEIO_LUMINAI_PREEXISTING_DIRTY_MANIFEST");
    }
}

#[test]
fn frozen_staged_and_unstaged_dirt_fail_without_manifest() {
    let _lock = env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (tmp, base, frozen) = init_dirty_repo();
    unsafe {
        std::env::set_var("LEIO_LUMINAI_CHANGE_BASE", &base);
    }
    assert!(
        kinds(tmp.path())
            .iter()
            .any(|kind| kind == "luminai_ha_frozen_path")
    );
    command(tmp.path(), &["add", "cartridges/health_audit/dirty.txt"]);
    assert!(
        kinds(tmp.path())
            .iter()
            .any(|kind| kind == "luminai_ha_frozen_path")
    );
    let _ = frozen;
    unsafe {
        std::env::remove_var("LEIO_LUMINAI_CHANGE_BASE");
    }
}

#[test]
fn ci_rejects_dirty_manifest_and_manifest_symlink() {
    let _lock = env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (tmp, base, _frozen) = init_dirty_repo();
    let path = manifest(
        tmp.path(),
        &base,
        "cartridges/health_audit/",
        "??",
        Some(directory_sha("dirty.txt", b"baseline\n")),
    );
    unsafe {
        std::env::set_var("LEIO_LUMINAI_CHANGE_BASE", &base);
        std::env::set_var("LEIO_LUMINAI_PREEXISTING_DIRTY_MANIFEST", &path);
        std::env::set_var("CI", "true");
    }
    assert!(
        kinds(tmp.path())
            .iter()
            .any(|kind| kind == "luminai_changeset_unconfigured")
    );
    unsafe {
        std::env::remove_var("CI");
        std::env::remove_var("LEIO_LUMINAI_CHANGE_BASE");
        std::env::remove_var("LEIO_LUMINAI_PREEXISTING_DIRTY_MANIFEST");
    }
}

#[test]
fn doctor_is_registered_for_profile_ci_cli_and_capabilities_catalog() {
    assert!(doctor_names_for_profile("example").contains(&"luminai-health-audit-isolation"));
    assert!(ci_doctor_names().contains(&"luminai-health-audit-isolation"));
    let output = Command::new(env!("CARGO_BIN_EXE_leio-code"))
        .args(["doctor", "--help"])
        .output()
        .expect("run doctor help");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("luminai-health-audit-isolation"));

    // Sovereign-repo adaptation: `mcp/index.js` and `apps-sdk/server.js`
    // derive their doctor-kind lists from `capabilities --catalog`, which is
    // itself profile-driven from this registry, so no mirror arrays exist to
    // assert against and the monorepo CI workflow files are gone. Assert the
    // catalog surface directly against a example-profile workspace instead.
    let tmp = fixture(&[]);
    let index = build_or_update_index(tmp.path(), &tmp.path().join(".leio-code/index.json"), true)
        .expect("index");
    let capabilities = leio_code::capabilities::workspace_capabilities(&index, tmp.path());
    assert!(
        capabilities
            .doctor_kinds
            .iter()
            .any(|kind| kind == "luminai-health-audit-isolation")
    );
}
