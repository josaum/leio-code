//! Git checkout identity for provenance across repos, worktrees, and branches.
//!
//! LEIO is routinely pointed at a package inside a monorepo, a linked
//! worktree on another branch, or a second clone of the same origin. Events
//! therefore record both the inspected `--repo` path and the git checkout
//! that owns it: common dir (shared object store), worktree root, branch,
//! HEAD, and a stable `repo_id` derived from `origin` when present.
// Rust guideline compliant 2026-02-21

use std::path::Path;
use std::process::Command;

use serde::Serialize;
use sha2::{Digest, Sha256};

/// One inspected tree plus the git checkout that contains it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Checkout {
    /// Path LEIO was asked to inspect (`--repo` / `repo_root`).
    pub inspect: String,
    /// `git rev-parse --show-toplevel`. Equals `inspect` when git is absent.
    pub worktree: String,
    /// `git rev-parse --git-common-dir` (shared by linked worktrees).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub common_dir: Option<String>,
    /// `git rev-parse --git-dir` (per-worktree git dir).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_dir: Option<String>,
    /// `git rev-parse --abbrev-ref HEAD`. `HEAD` when detached.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Full commit SHA.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// Normalized `remote.origin.url` (`github.com/owner/repo`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Stable join key: origin slug, else hash of the common dir / worktree.
    pub repo_id: String,
}

impl Checkout {
    /// URN for the logical repository (shared across worktrees and clones).
    pub fn repo_iri(&self) -> String {
        format!("urn:leio-code:repo:{}", self.repo_id)
    }

    /// URN for this working tree (path + HEAD).
    pub fn worktree_iri(&self) -> String {
        format!(
            "urn:leio-code:worktree:{}:{}",
            short_id(&self.worktree),
            self.head.as_deref().unwrap_or("unborn")
        )
    }

    /// File name stem when journals are collected under `LEIO_EVENTS_DIR`.
    pub fn journal_stem(&self) -> String {
        format!(
            "{}-{}",
            short_id(&self.worktree),
            slug(self.branch.as_deref().unwrap_or("HEAD"))
        )
    }

    /// Path of `full` relative to the worktree root, if it lives there.
    pub fn git_path(&self, full: &str) -> Option<String> {
        Path::new(full)
            .strip_prefix(&self.worktree)
            .ok()
            .map(|rel| rel.to_string_lossy().replace('\\', "/"))
            .filter(|rel| !rel.is_empty())
    }
}

/// Discover the checkout that contains `inspect`.
///
/// Walks up via `git -C` so a package subdirectory still resolves to the
/// worktree. Missing git is not an error: the inspect path stands alone.
pub fn discover(inspect: &Path) -> Checkout {
    let inspect_s = inspect.display().to_string();
    let worktree = git(inspect, &["rev-parse", "--show-toplevel"])
        .map(|path| canonicalize_or(Path::new(&path)))
        .unwrap_or_else(|| inspect_s.clone());
    let common_dir = git(inspect, &["rev-parse", "--git-common-dir"]).map(|path| {
        let raw = Path::new(&path);
        // `git -C inspect` prints this relative to `inspect`, not to toplevel.
        if raw.is_absolute() {
            canonicalize_or(raw)
        } else {
            canonicalize_or(&inspect.join(raw))
        }
    });
    let git_dir = git(inspect, &["rev-parse", "--absolute-git-dir"]);
    let branch = git(inspect, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let head = git(inspect, &["rev-parse", "HEAD"]);
    let origin =
        git(inspect, &["config", "--get", "remote.origin.url"]).map(|url| normalize_origin(&url));
    let repo_id = origin
        .as_deref()
        .map(slug)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            format!(
                "local-{}",
                short_id(common_dir.as_deref().unwrap_or(worktree.as_str()))
            )
        });
    Checkout {
        inspect: inspect_s,
        worktree,
        common_dir,
        git_dir,
        branch,
        head,
        origin,
        repo_id,
    }
}

/// Strip credentials and `.git` so clones via SSH/HTTPS share one `repo_id`.
pub fn normalize_origin(raw: &str) -> String {
    let trimmed = raw.trim();
    let without_scheme = trimmed
        .strip_prefix("git@")
        .map(|rest| rest.replacen(':', "/", 1))
        .or_else(|| {
            trimmed
                .find("://")
                .map(|idx| trimmed[idx + 3..].to_string())
        })
        .unwrap_or_else(|| trimmed.to_string());
    let without_user = without_scheme
        .rsplit_once('@')
        .map(|(_, host)| host.to_string())
        .unwrap_or(without_scheme);
    without_user
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_ascii_lowercase()
}

fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

fn canonicalize_or(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

fn slug(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if out.len() >= 80 {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
pub(crate) fn init_git_fixture() -> Option<tempfile::TempDir> {
    tests::init_git_repo()
}

fn short_id(raw: &str) -> String {
    let digest = Sha256::digest(raw.as_bytes());
    format!(
        "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    #[test]
    fn normalize_origin_joins_ssh_and_https() {
        assert_eq!(
            normalize_origin("git@github.com:josaum/leio-code.git"),
            "github.com/josaum/leio-code"
        );
        assert_eq!(
            normalize_origin("https://github.com/josaum/leio-code.git"),
            "github.com/josaum/leio-code"
        );
        assert_eq!(
            normalize_origin("https://user:token@github.com/josaum/leio-code"),
            "github.com/josaum/leio-code"
        );
        assert_eq!(
            normalize_origin("ssh://git@github.com/josaum/leio-code.git"),
            "github.com/josaum/leio-code"
        );
        assert_eq!(
            normalize_origin("https://github.com/josaum/leio-code.git/"),
            "github.com/josaum/leio-code"
        );
    }

    #[test]
    fn same_origin_clones_share_repo_id() {
        let Some(alpha) = init_git_repo() else {
            return;
        };
        let Some(beta) = init_git_repo() else {
            return;
        };
        git_ok(
            alpha.path(),
            &["remote", "add", "origin", "git@github.com:acme/widget.git"],
        );
        git_ok(
            beta.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/acme/widget.git",
            ],
        );
        let a = discover(alpha.path());
        let b = discover(beta.path());
        assert_eq!(a.origin.as_deref(), Some("github.com/acme/widget"));
        assert_eq!(a.repo_id, b.repo_id);
        assert_eq!(a.repo_iri(), b.repo_iri());
        assert_ne!(a.worktree, b.worktree);
        assert_ne!(a.worktree_iri(), b.worktree_iri());
        assert_ne!(a.journal_stem(), b.journal_stem());
    }

    #[test]
    fn different_origins_get_different_repo_ids() {
        let Some(alpha) = init_git_repo() else {
            return;
        };
        let Some(beta) = init_git_repo() else {
            return;
        };
        git_ok(
            alpha.path(),
            &["remote", "add", "origin", "git@github.com:acme/one.git"],
        );
        git_ok(
            beta.path(),
            &["remote", "add", "origin", "git@github.com:acme/two.git"],
        );
        assert_ne!(
            discover(alpha.path()).repo_id,
            discover(beta.path()).repo_id
        );
    }

    #[test]
    fn git_path_is_relative_to_worktree_not_inspect() {
        let Some(repo) = init_git_repo() else {
            return;
        };
        let full = repo.path().join("pkg").join("nested.rs");
        fs::write(&full, "fn x() {}\n").unwrap();
        let found = discover(&repo.path().join("pkg"));
        let rel = found
            .git_path(&full.canonicalize().unwrap().display().to_string())
            .expect("inside worktree");
        assert_eq!(rel, "pkg/nested.rs");
    }

    #[test]
    fn discover_without_git_uses_inspect_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let found = discover(dir.path());
        assert_eq!(found.worktree, dir.path().display().to_string());
        assert!(found.branch.is_none());
        assert!(found.repo_id.starts_with("local-"));
    }

    #[test]
    fn discover_reads_branch_and_shared_common_dir() {
        let Some(repo) = init_git_repo() else {
            return;
        };
        let main = discover(repo.path());
        assert_eq!(main.branch.as_deref(), Some("main"));
        assert!(main.head.as_deref().is_some_and(|h| h.len() == 40));
        git_ok(repo.path(), &["checkout", "-b", "feature"]);
        fs::write(repo.path().join("pkg/nested.txt"), "n\n").unwrap();
        let feature = discover(&repo.path().join("pkg"));
        assert_eq!(feature.branch.as_deref(), Some("feature"));
        assert_eq!(feature.worktree, main.worktree);
        assert_eq!(feature.common_dir, main.common_dir);
        assert_eq!(feature.head, main.head);
    }

    #[test]
    fn two_worktrees_share_repo_not_worktree() {
        let Some(repo) = init_git_repo() else {
            return;
        };
        let sibling = repo.path().parent().unwrap().join(format!(
            "{}-wt",
            repo.path().file_name().unwrap().to_string_lossy()
        ));
        git_ok(repo.path(), &["branch", "other"]);
        let status = Command::new("git")
            .args(["worktree", "add", sibling.to_str().unwrap(), "other"])
            .current_dir(repo.path())
            .status()
            .ok();
        if !status.is_some_and(|s| s.success()) {
            return;
        }
        let a = discover(repo.path());
        let b = discover(&sibling);
        assert_eq!(a.common_dir, b.common_dir);
        assert_eq!(a.repo_id, b.repo_id);
        assert_ne!(a.worktree, b.worktree);
        assert_ne!(a.worktree_iri(), b.worktree_iri());
        let _ = fs::remove_dir_all(&sibling);
    }

    pub(crate) fn init_git_repo() -> Option<tempfile::TempDir> {
        let dir = tempfile::tempdir().ok()?;
        if !git_ok(dir.path(), &["init", "-b", "main"]) {
            return None;
        }
        git_ok(dir.path(), &["config", "user.email", "leio@example.test"]);
        git_ok(dir.path(), &["config", "user.name", "leio"]);
        fs::create_dir_all(dir.path().join("pkg")).ok()?;
        fs::write(dir.path().join("README.md"), "hi\n").ok()?;
        git_ok(dir.path(), &["add", "README.md"]);
        if !git_ok(dir.path(), &["commit", "-m", "init"]) {
            return None;
        }
        Some(dir)
    }

    pub(crate) fn git_ok(cwd: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .ok()
            .is_some_and(|s| s.success())
    }
}
