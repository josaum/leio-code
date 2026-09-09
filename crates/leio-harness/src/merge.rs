//! Fail-closed merge executor for cross-branch/cross-worktree integration.
//!
//! The improvement gate evaluates the *integrated* result, never the isolated
//! lanes. Integration therefore merges each lane branch into a target checkout
//! that is already on `target_ref` — it never performs a `checkout`, because a
//! ref that is checked out in another worktree (e.g. `main` in the main
//! checkout) cannot be checked out again. Dirty or conflicting trees are
//! refused, and no destructive reset is performed.
//!
//! Two variants serve two stages of the gate:
//! - [`merge_branch`] leaves the merge staged (`--no-commit`) so a single
//!   objective command can run against the merged tree before committing.
//! - [`merge_branch_committing`] commits each merge (`--no-ff`), letting the
//!   integration phase stack multiple lane merges into one integration branch.
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::path::Path;
use std::process::{Command, Output};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeResult {
    pub merged: bool,
    pub conflict: bool,
    pub source_branch: String,
    pub target_ref: String,
    pub worktree: String,
    pub files_changed: Option<String>,
    pub error: Option<String>,
}

/// Merge `source_branch` into `target_ref`, using `worktree` as the target
/// checkout (its current branch must already be `target_ref`). Leaves the merge
/// staged. Returns `conflict=true` without mutating the index on failure.
pub fn merge_branch(
    repo_root: &Path,
    worktree: &Path,
    source_branch: &str,
    target_ref: &str,
) -> Result<MergeResult> {
    let worktree = validate_merge_target(repo_root, worktree, source_branch, target_ref)?;
    let merge = Command::new("git")
        .arg("-C")
        .arg(&worktree)
        .args(["merge", "--no-ff", "--no-commit", source_branch])
        .output()?;
    if !merge.status.success() {
        let combined = combined_output(&merge);
        let conflict = combined.contains("CONFLICT");
        // Abort the in-progress merge so the worktree is never left half-merged.
        let _ = git(&worktree, &["merge", "--abort"]);
        return Ok(failed_result(
            &worktree,
            source_branch,
            target_ref,
            conflict,
            &combined,
        ));
    }
    let files = git(&worktree, &["diff", "--name-only", "--cached", "HEAD"])?;
    Ok(success_result(&worktree, source_branch, target_ref, &files))
}

/// Merge `source_branch` into `target_ref` and commit the result with `--no-ff`.
/// Each call becomes a real merge commit, so the integration phase can stack
/// many lane merges on one integration branch.
pub fn merge_branch_committing(
    repo_root: &Path,
    worktree: &Path,
    source_branch: &str,
    target_ref: &str,
) -> Result<MergeResult> {
    let worktree = validate_merge_target(repo_root, worktree, source_branch, target_ref)?;
    let merge = Command::new("git")
        .arg("-C")
        .arg(&worktree)
        .args(["merge", "--no-ff", source_branch])
        .output()?;
    if !merge.status.success() {
        let combined = combined_output(&merge);
        let conflict = combined.contains("CONFLICT");
        let _ = git(&worktree, &["merge", "--abort"]);
        return Ok(failed_result(
            &worktree,
            source_branch,
            target_ref,
            conflict,
            &combined,
        ));
    }
    let files = git(&worktree, &["diff", "--name-only", "HEAD~1", "HEAD"])?;
    Ok(success_result(&worktree, source_branch, target_ref, &files))
}

/// Shared pre-merge validation: canonicalizes both paths, verifies the worktree
/// is linked to the repo, sanitizes refs, refuses a dirty tree, and confirms
/// the current branch is `target_ref`. Returns the canonicalized worktree.
pub(crate) fn validate_merge_target(
    repo_root: &Path,
    worktree: &Path,
    source_branch: &str,
    target_ref: &str,
) -> Result<std::path::PathBuf> {
    let repo_root = canonical_repo_root(repo_root)?;
    let worktree = std::fs::canonicalize(worktree)
        .with_context(|| format!("canonicalize {}", worktree.display()))?;
    assert_linked(&repo_root, &worktree)?;
    validate_ref(source_branch)?;
    validate_ref(target_ref)?;

    let status = git(
        &worktree,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    if status.lines().any(|line| !is_tool_cache_entry(line)) {
        bail!("worktree is dirty; refusing merge");
    }

    let current = git(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_owned();
    if current != target_ref {
        bail!("worktree is on '{current}', expected '{target_ref}'");
    }
    Ok(worktree)
}

fn success_result(
    worktree: &Path,
    source_branch: &str,
    target_ref: &str,
    files: &str,
) -> MergeResult {
    let files_changed = files.trim().to_owned();
    MergeResult {
        merged: true,
        conflict: false,
        source_branch: source_branch.to_owned(),
        target_ref: target_ref.to_owned(),
        worktree: worktree.display().to_string(),
        files_changed: if files_changed.is_empty() {
            None
        } else {
            Some(files_changed)
        },
        error: None,
    }
}

fn failed_result(
    worktree: &Path,
    source_branch: &str,
    target_ref: &str,
    conflict: bool,
    combined: &str,
) -> MergeResult {
    MergeResult {
        merged: false,
        conflict,
        source_branch: source_branch.to_owned(),
        target_ref: target_ref.to_owned(),
        worktree: worktree.display().to_string(),
        files_changed: None,
        error: Some(combined.trim().to_owned()),
    }
}

/// Git writes merge conflict markers to stdout, diagnostics to stderr; a
/// fail-closed executor must look at both before declaring a conflict.
fn combined_output(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("{stdout}\n{stderr}")
}

fn canonical_repo_root(path: &Path) -> Result<std::path::PathBuf> {
    let expected =
        std::fs::canonicalize(path).with_context(|| format!("canonicalize {}", path.display()))?;
    let actual = git(&expected, &["rev-parse", "--show-toplevel"])?;
    let actual = std::fs::canonicalize(actual.trim())?;
    if expected != actual {
        bail!("repository root mismatch");
    }
    Ok(expected)
}

/// Verify `worktree` is a git checkout linked to `repo_root` (or the main
/// checkout itself). Prevents merging into an arbitrary directory.
fn assert_linked(repo_root: &Path, worktree: &Path) -> Result<()> {
    let top = git(worktree, &["rev-parse", "--show-toplevel"])?;
    if std::fs::canonicalize(top.trim())? != worktree {
        bail!("worktree is not a git checkout root");
    }
    let common_raw = git(worktree, &["rev-parse", "--git-common-dir"])?
        .trim()
        .to_owned();
    let common = if Path::new(&common_raw).is_absolute() {
        std::fs::canonicalize(&common_raw)?
    } else {
        std::fs::canonicalize(worktree.join(&common_raw))?
    };
    let repo_git = std::fs::canonicalize(repo_root.join(".git"))?;
    if common != repo_git {
        bail!("worktree is not linked to repository root");
    }
    Ok(())
}

fn validate_ref(value: &str) -> Result<()> {
    if value.is_empty()
        || value.contains("..")
        || value
            .chars()
            .any(|c| c.is_whitespace() || "~^:?*[\\".contains(c))
    {
        bail!("unsafe git ref: {value}");
    }
    Ok(())
}

fn is_tool_cache_entry(line: &str) -> bool {
    let path_part = line.trim_start_matches('?').trim();
    path_part == ".leio-code" || path_part.starts_with(".leio-code/")
}

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn gitc(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        gitc(&repo, &["init", "-b", "main"]);
        gitc(&repo, &["config", "user.email", "t@t"]);
        gitc(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("README"), "base\n").unwrap();
        gitc(&repo, &["add", "README"]);
        gitc(&repo, &["commit", "-m", "base"]);
        (dir, repo)
    }

    /// Create a linked lane worktree on `branch`, write `content` to
    /// `filename`, and commit it. Returns the worktree path.
    fn lane_file(
        repo: &Path,
        wt_root: &Path,
        branch: &str,
        filename: &str,
        content: &str,
    ) -> std::path::PathBuf {
        let worktree = wt_root.join(branch.replace('/', "_"));
        gitc(
            repo,
            &["worktree", "add", "-b", branch, worktree.to_str().unwrap()],
        );
        std::fs::write(worktree.join(filename), content).unwrap();
        gitc(&worktree, &["add", filename]);
        gitc(&worktree, &["commit", "-m", branch]);
        worktree
    }

    #[test]
    fn clean_merge_reports_files_changed() {
        let (_d, repo) = fixture();
        let wt_root = _d.path().join("wt");
        std::fs::create_dir_all(&wt_root).unwrap();
        let _lane = lane_file(&repo, &wt_root, "feature/x", "README", "base\nfeature\n");
        let result = merge_branch(&repo, &repo, "feature/x", "main").unwrap();
        assert!(result.merged);
        assert!(!result.conflict);
        assert!(
            result
                .files_changed
                .as_deref()
                .unwrap_or("")
                .contains("README")
        );
    }

    #[test]
    fn conflict_refuses_and_leaves_tree_intact() {
        let (_d, repo) = fixture();
        let wt_root = _d.path().join("wt");
        std::fs::create_dir_all(&wt_root).unwrap();
        let _lane = lane_file(&repo, &wt_root, "feature/x", "README", "feature\n");
        std::fs::write(repo.join("README"), "main\n").unwrap();
        gitc(&repo, &["add", "README"]);
        gitc(&repo, &["commit", "-m", "main"]);
        let result = merge_branch(&repo, &repo, "feature/x", "main").unwrap();
        assert!(!result.merged);
        assert!(result.conflict);
        let status = git(&repo, &["status", "--porcelain=v1"]).unwrap();
        assert!(!status.contains("UU"));
    }

    #[test]
    fn committing_merge_stacks_commits() {
        let (_d, repo) = fixture();
        let wt_root = _d.path().join("wt");
        std::fs::create_dir_all(&wt_root).unwrap();
        let _a = lane_file(&repo, &wt_root, "lane/a", "a.txt", "a\n");
        let _b = lane_file(&repo, &wt_root, "lane/b", "b.txt", "b\n");
        let first = merge_branch_committing(&repo, &repo, "lane/a", "main").unwrap();
        assert!(first.merged);
        let second = merge_branch_committing(&repo, &repo, "lane/b", "main").unwrap();
        assert!(second.merged);
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "a\n");
        assert_eq!(std::fs::read_to_string(repo.join("b.txt")).unwrap(), "b\n");
    }
}
