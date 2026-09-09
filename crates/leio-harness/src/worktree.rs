use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Serialize)]
pub struct WorktreeResult {
    pub action: String,
    pub repo_root: String,
    pub worktree_path: String,
    pub branch: Option<String>,
}

pub fn create(
    repo_root: &Path,
    worktree_root: &Path,
    worktree_path: &Path,
    branch: &str,
    base_ref: Option<&str>,
) -> Result<WorktreeResult> {
    let repo_root = canonical_repo_root(repo_root)?;
    let worktree_root = normalize_absolute(worktree_root)?;
    let worktree_path = normalize_absolute(worktree_path)?;
    assert_within(&worktree_path, &worktree_root)?;
    if worktree_path.exists() {
        bail!("worktree path already exists: {}", worktree_path.display());
    }
    validate_branch(branch)?;
    let source_status = git(
        &repo_root,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    let dirty = source_status.lines().any(|line| !is_tool_cache_entry(line));
    if dirty {
        bail!("source repository is dirty; refusing worktree creation");
    }
    let branch_ref = format!("refs/heads/{branch}");
    let exists = git_status(
        &repo_root,
        &["show-ref", "--verify", "--quiet", &branch_ref],
    )?;
    let worktree_string = worktree_path.to_string_lossy().into_owned();
    if exists {
        git(&repo_root, &["worktree", "add", &worktree_string, branch])?;
    } else {
        git(
            &repo_root,
            &[
                "worktree",
                "add",
                "-b",
                branch,
                &worktree_string,
                base_ref.unwrap_or("HEAD"),
            ],
        )?;
    }
    Ok(WorktreeResult {
        action: "create".to_owned(),
        repo_root: repo_root.display().to_string(),
        worktree_path: worktree_path.display().to_string(),
        branch: Some(branch.to_owned()),
    })
}

/// Stage lane edits (except `.leio-code`) and commit if anything remains.
/// Returns true when a commit was created.
pub fn commit_lane(worktree_path: &Path, message: &str) -> Result<bool> {
    let worktree_path = if worktree_path.exists() {
        std::fs::canonicalize(worktree_path)?
    } else {
        bail!("worktree path does not exist: {}", worktree_path.display());
    };
    git(&worktree_path, &["add", "-A"])?;
    let _ = git(&worktree_path, &["reset", "-q", "--", ".leio-code"]);
    let status = git(
        &worktree_path,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    if !status.lines().any(|line| !is_tool_cache_entry(line)) {
        return Ok(false);
    }
    let msg = if message.trim().is_empty() {
        "leio-harness: lane changes".to_owned()
    } else {
        message.chars().take(72).collect()
    };
    git(
        &worktree_path,
        &[
            "-c",
            "user.name=leio-harness",
            "-c",
            "user.email=leio-harness@local",
            "commit",
            "-m",
            &msg,
        ],
    )?;
    Ok(true)
}

pub fn retire(
    repo_root: &Path,
    worktree_root: &Path,
    worktree_path: &Path,
) -> Result<WorktreeResult> {
    let repo_root = canonical_repo_root(repo_root)?;
    let worktree_root = normalize_absolute(worktree_root)?;
    let worktree_path = normalize_absolute(worktree_path)?;
    assert_within(&worktree_path, &worktree_root)?;
    let status = git(
        &worktree_path,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    if status.lines().any(|line| !is_tool_cache_entry(line)) {
        bail!("worktree is dirty; refusing retirement");
    }
    let worktree_string = worktree_path.to_string_lossy().into_owned();
    // `.leio-code/` is untracked tool cache: `worktree remove` still refuses
    // without --force even when our dirty check ignores it.
    git(
        &repo_root,
        &["worktree", "remove", "--force", &worktree_string],
    )?;
    Ok(WorktreeResult {
        action: "retire".to_owned(),
        repo_root: repo_root.display().to_string(),
        worktree_path: worktree_path.display().to_string(),
        branch: None,
    })
}

fn canonical_repo_root(path: &Path) -> Result<PathBuf> {
    let expected =
        std::fs::canonicalize(path).with_context(|| format!("canonicalize {}", path.display()))?;
    let actual = git(&expected, &["rev-parse", "--show-toplevel"])?;
    let actual = std::fs::canonicalize(actual.trim())?;
    if expected != actual {
        bail!(
            "repository root mismatch: expected {}, got {}",
            expected.display(),
            actual.display()
        );
    }
    Ok(expected)
}

fn normalize_absolute(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return Ok(std::fs::canonicalize(path)?);
    }
    let parent = path.parent().context("path has no parent")?;
    let parent = std::fs::canonicalize(parent)?;
    Ok(parent.join(path.file_name().context("path has no file name")?))
}

fn assert_within(child: &Path, root: &Path) -> Result<()> {
    if child == root || child.starts_with(root) {
        Ok(())
    } else {
        bail!("worktree path escapes configured root")
    }
}

fn validate_branch(branch: &str) -> Result<()> {
    if branch.is_empty()
        || branch.starts_with('/')
        || branch.ends_with('/')
        || branch.contains("..")
        || branch
            .chars()
            .any(|value| value.is_whitespace() || "~^:?*[\\".contains(value))
    {
        bail!("unsafe branch name: {branch}");
    }
    Ok(())
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

fn git_status(repo: &Path, args: &[&str]) -> Result<bool> {
    Ok(Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()?
        .success())
}

/// leio-code writes `.leio-code/` (index.json, search.arrow) into the repo, and
/// Claude Code nests its own git worktrees under `.claude/worktrees/`, which
/// surface as untracked entries in the parent. Those are tool caches, not user
/// work — ignore them in dirty checks. The rest of `.claude/` (settings, skills)
/// *is* user work and stays dirty-relevant.
fn is_tool_cache_entry(line: &str) -> bool {
    let path_part = line.trim_start_matches('?').trim();
    for prefix in [".leio-code", ".claude/worktrees"] {
        if path_part == prefix || path_part.starts_with(&format!("{prefix}/")) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_cache_is_not_dirty() {
        assert!(is_tool_cache_entry("?? .leio-code/"));
        assert!(is_tool_cache_entry("?? .leio-code/index.json"));
        assert!(!is_tool_cache_entry("?? .leio-code-2/"));
        assert!(!is_tool_cache_entry(" M src/main.rs"));
    }

    /// Claude Code nests worktrees under `.claude/worktrees/`; git reports each
    /// as one untracked entry in the parent. Ignoring them keeps a repo that
    /// hosts another agent's worktrees usable as a harness source.
    #[test]
    fn nested_agent_worktrees_are_not_dirty() {
        assert!(is_tool_cache_entry("?? .claude/worktrees/"));
        assert!(is_tool_cache_entry(
            "?? .claude/worktrees/wf_077e3050-b9f-1/"
        ));
        // The rest of `.claude/` is user work, not a cache.
        assert!(!is_tool_cache_entry("?? .claude/"));
        assert!(!is_tool_cache_entry("?? .claude/settings.json"));
        assert!(!is_tool_cache_entry("?? .claude-worktrees/"));
    }

    #[test]
    fn commit_lane_skips_clean_tree() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-qb", "main"]).unwrap();
        git(&repo, &["config", "user.email", "t@t"]).unwrap();
        git(&repo, &["config", "user.name", "t"]).unwrap();
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        git(&repo, &["add", "a.txt"]).unwrap();
        git(&repo, &["commit", "-m", "init"]).unwrap();
        assert!(!commit_lane(&repo, "noop").unwrap());
        std::fs::write(repo.join("b.txt"), "b").unwrap();
        assert!(commit_lane(&repo, "add b").unwrap());
    }
}
