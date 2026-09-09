//! Provenance events across branches, worktrees, and repos.
//!
//! These fixtures shell out to `git`. If `git` is missing the tests return
//! without failing so the suite still runs on stripped CI images.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use leio_code::checkout::{self, Checkout};
use leio_code::jsonld::{
    events_path, record_event, render_envelope_as_jsonld_in, shared_events_path,
};
use leio_code::model::{EvidenceItem, QueryEnvelope, SCHEMA_VERSION};
use serde_json::json;
use tempfile::TempDir;

#[test]
fn two_clones_same_origin_share_repo_id_not_worktree() {
    let Some(a) = git_repo_with_origin("git@github.com:acme/shared.git") else {
        return;
    };
    let Some(b) = git_repo_with_origin("https://github.com/acme/shared.git") else {
        return;
    };
    let left = checkout::discover(a.path());
    let right = checkout::discover(b.path());
    assert_eq!(left.repo_id, right.repo_id);
    assert_eq!(left.origin.as_deref(), Some("github.com/acme/shared"));
    assert_ne!(left.worktree, right.worktree);
}

#[test]
fn two_repos_keep_separate_event_journals() {
    let Some(alpha) = git_repo_with_origin("git@github.com:acme/alpha.git") else {
        return;
    };
    let Some(beta) = git_repo_with_origin("git@github.com:acme/beta.git") else {
        return;
    };
    write_dot_env(alpha.path());
    write_dot_env(beta.path());
    record_event(alpha.path(), &envelope("alpha")).unwrap();
    record_event(beta.path(), &envelope("beta")).unwrap();

    let alpha_doc = first_event(alpha.path());
    let beta_doc = first_event(beta.path());
    assert_ne!(
        alpha_doc["checkout"]["repoId"],
        beta_doc["checkout"]["repoId"]
    );
    assert_eq!(
        alpha_doc["checkout"]["origin"].as_str(),
        Some("github.com/acme/alpha")
    );
    assert_eq!(
        beta_doc["checkout"]["origin"].as_str(),
        Some("github.com/acme/beta")
    );
    assert_ne!(events_path(alpha.path()), events_path(beta.path()));
}

#[test]
fn branch_switch_keeps_repo_and_changes_head() {
    let Some(repo) = git_repo_with_origin("git@github.com:acme/branchy.git") else {
        return;
    };
    write_dot_env(repo.path());
    record_event(repo.path(), &envelope("on-main")).unwrap();
    let on_main = checkout::discover(repo.path());
    assert_eq!(on_main.branch.as_deref(), Some("main"));

    git_ok(repo.path(), &["checkout", "-b", "feature"]);
    fs::write(repo.path().join("feature.txt"), "feat\n").unwrap();
    git_ok(repo.path(), &["add", "feature.txt"]);
    git_ok(repo.path(), &["commit", "-m", "feature"]);
    record_event(repo.path(), &envelope("on-feature")).unwrap();
    let on_feature = checkout::discover(repo.path());

    assert_eq!(on_feature.branch.as_deref(), Some("feature"));
    assert_eq!(on_feature.repo_id, on_main.repo_id);
    assert_eq!(on_feature.worktree, on_main.worktree);
    assert_ne!(on_feature.head, on_main.head);

    let raw = fs::read_to_string(events_path(repo.path())).unwrap();
    let lines: Vec<&str> = raw.lines().filter(|line| !line.is_empty()).collect();
    assert_eq!(lines.len(), 2);
    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(first["checkout"]["branch"].as_str(), Some("main"));
    assert_eq!(second["checkout"]["branch"].as_str(), Some("feature"));
    assert_ne!(first["checkout"]["head"], second["checkout"]["head"]);
}

#[test]
fn worktrees_write_isolated_journals_with_shared_repo() {
    let Some(sandbox) = git_sandbox() else {
        return;
    };
    let primary = sandbox.path().join("repo");
    let linked = sandbox.path().join("wt");
    if !add_worktree(&primary, &linked, "other") {
        return;
    }
    write_dot_env(&primary);
    write_dot_env(&linked);
    record_event(&primary, &envelope("primary")).unwrap();
    record_event(&linked, &envelope("linked")).unwrap();

    let a = first_event(&primary);
    let b = first_event(&linked);
    assert_eq!(a["checkout"]["repoId"], b["checkout"]["repoId"]);
    assert_eq!(a["checkout"]["commonDir"], b["checkout"]["commonDir"]);
    assert_ne!(a["checkout"]["worktree"], b["checkout"]["worktree"]);
    assert_ne!(a["worktree"]["@id"], b["worktree"]["@id"]);
    assert_ne!(events_path(&primary), events_path(&linked));
    assert!(events_path(&primary).is_file());
    assert!(events_path(&linked).is_file());
}

#[test]
fn package_inspect_stamps_git_path_from_toplevel() {
    let Some(repo) = git_repo_with_origin("git@github.com:acme/mono.git") else {
        return;
    };
    let pkg = repo.path().join("pkg");
    fs::create_dir_all(pkg.join("src")).unwrap();
    fs::write(pkg.join("src/lib.rs"), "pub fn x() {}\n").unwrap();
    let env = QueryEnvelope {
        schema_version: SCHEMA_VERSION.to_string(),
        query_id: "find_symbol-1".to_string(),
        kind: "find".to_string(),
        summary: "pkg".to_string(),
        confidence: 1.0,
        entities: vec![json!({"name": "x", "path": "src/lib.rs"})],
        evidence: vec![EvidenceItem {
            kind: "symbol".to_string(),
            path: "src/lib.rs".to_string(),
            line: Some(1),
            detail: "x".to_string(),
        }],
        warnings: vec![],
        meta: None,
        timing_ms: 0,
    };
    let doc = render_envelope_as_jsonld_in(&env, Some(&pkg));
    assert_eq!(
        doc["entities"][0]["gitPath"].as_str(),
        Some("pkg/src/lib.rs")
    );
    assert_eq!(
        doc["evidence"][0]["gitPath"].as_str(),
        Some("pkg/src/lib.rs")
    );
    assert_eq!(
        doc["checkout"]["inspect"].as_str().unwrap(),
        pkg.display().to_string()
    );
    assert_eq!(
        doc["checkout"]["worktree"].as_str().unwrap(),
        repo.path().canonicalize().unwrap().display().to_string()
    );
    assert!(
        doc["@id"]
            .as_str()
            .unwrap()
            .contains(doc["checkout"]["repoId"].as_str().unwrap())
    );
}

#[test]
fn shared_events_dir_splits_by_repo_and_worktree() {
    let Some(alpha) = git_repo_with_origin("git@github.com:acme/alpha.git") else {
        return;
    };
    let Some(beta) = git_repo_with_origin("git@github.com:acme/beta.git") else {
        return;
    };
    let sink = TempDir::new().unwrap();
    let a = checkout::discover(alpha.path());
    let b = checkout::discover(beta.path());
    let pa = shared_events_path(sink.path(), &a);
    let pb = shared_events_path(sink.path(), &b);
    assert!(pa.starts_with(sink.path().join(&a.repo_id)));
    assert!(pb.starts_with(sink.path().join(&b.repo_id)));
    assert_ne!(pa, pb);
    assert!(pa.to_string_lossy().ends_with(".ndjson"));
}

#[test]
fn journal_stems_differ_across_clones_on_the_same_branch() {
    let Some(alpha) = git_repo_with_origin("git@github.com:acme/shared.git") else {
        return;
    };
    let Some(beta) = git_repo_with_origin("git@github.com:acme/shared.git") else {
        return;
    };
    let a = checkout::discover(alpha.path());
    let b = checkout::discover(beta.path());
    assert_eq!(a.branch.as_deref(), Some("main"));
    assert_eq!(a.branch, b.branch);
    assert_ne!(a.journal_stem(), b.journal_stem());
}

fn envelope(label: &str) -> QueryEnvelope {
    QueryEnvelope {
        schema_version: SCHEMA_VERSION.to_string(),
        query_id: format!("find_env-{label}"),
        kind: "find".to_string(),
        summary: label.to_string(),
        confidence: 1.0,
        entities: vec![json!({"name": "FOO", "path": ".env"})],
        evidence: vec![EvidenceItem {
            kind: "env_var".to_string(),
            path: ".env".to_string(),
            line: Some(1),
            detail: "FOO".to_string(),
        }],
        warnings: vec![],
        meta: None,
        timing_ms: 0,
    }
}

fn first_event(repo: &Path) -> serde_json::Value {
    let raw = fs::read_to_string(events_path(repo)).expect("events journal");
    let line = raw.lines().next().expect("at least one event");
    serde_json::from_str(line).expect("jsonld line")
}

fn write_dot_env(root: &Path) {
    fs::write(root.join(".env"), "FOO=1\n").unwrap();
}

fn git_repo_with_origin(origin: &str) -> Option<TempDir> {
    let dir = init_git_repo()?;
    git_ok(dir.path(), &["remote", "add", "origin", origin]);
    Some(dir)
}

fn git_sandbox() -> Option<TempDir> {
    let root = TempDir::new().ok()?;
    let repo = root.path().join("repo");
    fs::create_dir(&repo).ok()?;
    if !init_git_at(&repo) {
        return None;
    }
    Some(root)
}

fn init_git_repo() -> Option<TempDir> {
    let dir = TempDir::new().ok()?;
    if !init_git_at(dir.path()) {
        return None;
    }
    Some(dir)
}

fn init_git_at(path: &Path) -> bool {
    // Positive form on purpose: the doubly-negated version reads badly and
    // clippy 0.1.96 rejects it as nonminimal_bool while 0.1.97 accepts it, so
    // the gate passed here and failed on a machine one release behind.
    // Short-circuiting is unchanged — `git_ok` runs commands, so the fallback
    // must still only run when `init -b main` fails.
    let initialized = git_ok(path, &["init", "-b", "main"])
        || (git_ok(path, &["init"]) && git_ok(path, &["checkout", "-b", "main"]));
    if !initialized {
        return false;
    }
    git_ok(path, &["config", "user.email", "leio@example.test"]);
    git_ok(path, &["config", "user.name", "leio"]);
    fs::create_dir_all(path.join("pkg")).ok();
    if fs::write(path.join("README.md"), "hi\n").is_err() {
        return false;
    }
    git_ok(path, &["add", "README.md"]) && git_ok(path, &["commit", "-m", "init"])
}

fn add_worktree(repo: &Path, dest: &Path, branch: &str) -> bool {
    let _ = git_ok(repo, &["branch", branch]);
    Command::new("git")
        .args(["worktree", "add", dest.to_str().unwrap(), branch])
        .current_dir(repo)
        .status()
        .ok()
        .is_some_and(|s| s.success())
}

fn git_ok(cwd: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .ok()
        .is_some_and(|s| s.success())
}

#[test]
fn shared_events_path_uses_repo_id_and_journal_stem() {
    let checkout = Checkout {
        inspect: "/tmp/inspect".into(),
        worktree: "/tmp/wt-a".into(),
        common_dir: Some("/tmp/wt-a/.git".into()),
        git_dir: Some("/tmp/wt-a/.git".into()),
        branch: Some("feature/x".into()),
        head: Some("abc".into()),
        origin: Some("github.com/acme/app".into()),
        repo_id: "github-com-acme-app".into(),
    };
    let path = shared_events_path(Path::new("/var/leio/events"), &checkout);
    assert_eq!(
        path,
        PathBuf::from("/var/leio/events")
            .join("github-com-acme-app")
            .join(format!("{}.ndjson", checkout.journal_stem()))
    );
    assert!(checkout.journal_stem().contains("feature-x"));
}
