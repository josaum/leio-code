//! Integration phase: stack lane branches into one integration branch and run
//! the objective on the *integrated* tree — never on isolated lanes.
//!
//! The integration worktree is created from `target_ref` on a fresh
//! `integration/<id>` branch. Lane branches are merged commit-by-commit
//! ([`crate::merge::merge_branch_committing`]). Any conflict aborts the whole
//! integration (fail-closed) and cleans up the integration branch/worktree;
//! no destructive reset is ever applied to the target checkout.
use crate::merge::{self, MergeResult};
use crate::model::{RunResult, RunSpec, RunStatus};
use crate::{process, worktree};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrateSpec {
    pub repo: String,
    pub worktree_root: String,
    pub target_ref: String,
    pub branches: Vec<String>,
    pub objective: Option<ObjectiveSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectiveSpec {
    pub argv: Vec<String>,
    pub output_dir: String,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationReport {
    pub integration_branch: String,
    pub integration_worktree: String,
    pub target_ref: String,
    pub merged: bool,
    pub conflict: Option<String>,
    pub merges: Vec<MergeResult>,
    pub files_changed: Vec<String>,
    pub objective: Option<ObjectiveOutcome>,
    /// `kept` on success (coordinator promotes or discards); `retired` on
    /// conflict (cleanup already happened).
    pub cleanup: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectiveOutcome {
    pub status: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub error: Option<String>,
    /// Metric snapshot parsed from the objective's stdout (when it emitted a
    /// valid `OutcomeSnapshot` JSON object).
    pub snapshot: Option<crate::improvement::OutcomeSnapshot>,
}

pub fn integrate_lanes(spec: &IntegrateSpec) -> Result<IntegrationReport> {
    validate(spec)?;
    let repo = Path::new(&spec.repo);
    let worktree_root = Path::new(&spec.worktree_root);
    let id = short_id();
    // The branch name keeps the `integration/` namespace; the worktree
    // directory is flat so its parent always exists for canonicalization.
    let branch = format!("integration/{id}");
    let integrate_path = worktree_root.join(format!("integration-{id}"));

    worktree::create(
        repo,
        worktree_root,
        &integrate_path,
        &branch,
        Some(&spec.target_ref),
    )?;

    let mut merges = Vec::new();
    let mut files_changed: Vec<String> = Vec::new();
    let mut conflict: Option<String> = None;

    for source in &spec.branches {
        match merge::merge_branch_committing(repo, &integrate_path, source, &branch) {
            Ok(result) => {
                if let Some(files) = &result.files_changed {
                    for file in files.lines().map(str::trim).filter(|f| !f.is_empty()) {
                        if !files_changed.iter().any(|f| f == file) {
                            files_changed.push(file.to_owned());
                        }
                    }
                }
                let failed = !result.merged;
                let source = result.source_branch.clone();
                merges.push(result);
                if failed {
                    conflict = Some(source);
                    break;
                }
            }
            Err(error) => {
                // Pre-merge validation failure (dirty/unsafe ref); the merge was
                // not attempted. Clean up and surface the error.
                discard_worktree(repo, &integrate_path, &branch);
                return Err(error);
            }
        }
    }

    if let Some(conflicted_branch) = &conflict {
        discard_worktree(repo, &integrate_path, &branch);
        return Ok(IntegrationReport {
            integration_branch: branch,
            integration_worktree: integrate_path.display().to_string(),
            target_ref: spec.target_ref.clone(),
            merged: false,
            conflict: Some(conflicted_branch.clone()),
            merges,
            files_changed,
            objective: None,
            cleanup: "retired".to_owned(),
        });
    }

    let objective = match &spec.objective {
        Some(objective) => Some(run_objective(&integrate_path, objective)?),
        None => None,
    };
    files_changed.sort();

    Ok(IntegrationReport {
        integration_branch: branch,
        integration_worktree: integrate_path.display().to_string(),
        target_ref: spec.target_ref.clone(),
        merged: true,
        conflict: None,
        merges,
        files_changed,
        objective,
        cleanup: "kept".to_owned(),
    })
}

fn run_objective(worktree: &Path, objective: &ObjectiveSpec) -> Result<ObjectiveOutcome> {
    let run: RunResult = process::run(RunSpec {
        run_id: format!("objective-{}", short_id()),
        argv: objective.argv.clone(),
        cwd: worktree.display().to_string(),
        output_dir: objective.output_dir.clone(),
        timeout_ms: objective.timeout_ms,
        kill_grace_ms: 2_000,
        max_output_bytes: 4 * 1024 * 1024,
        env_allowlist: Vec::new(),
        env: Default::default(),
        required_approval_token: None,
        approval_token: None,
    })?;
    let snapshot = snapshot_from_stdout_path(&run.stdout_path);
    Ok(ObjectiveOutcome {
        status: status_name(&run.status).to_owned(),
        exit_code: run.exit_code,
        duration_ms: run.duration_ms,
        error: run.error,
        snapshot,
    })
}

fn snapshot_from_stdout_path(path: &str) -> Option<crate::improvement::OutcomeSnapshot> {
    let bytes = std::fs::read(path).ok()?;
    crate::improvement::parse_snapshot_from_stdout(&String::from_utf8_lossy(&bytes)).ok()
}

fn status_name(status: &RunStatus) -> &'static str {
    match status {
        RunStatus::Passed => "passed",
        RunStatus::Failed => "failed",
        RunStatus::TimedOut => "timed_out",
        RunStatus::Canceled => "canceled",
        RunStatus::InfraError => "infra_error",
    }
}

/// Force-remove a throwaway worktree (ours — auto-generated under
/// `worktree_root`) and delete its branch. Best-effort: a build/test objective
/// may have dirtied the worktree, so `--force` is intentional here and never
/// touches user work. `git worktree remove` itself validates the path is a
/// registered worktree, so a stray path is a harmless no-op.
pub fn discard_worktree(repo_root: &Path, worktree_path: &Path, branch: &str) {
    let _ = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args([
            "worktree",
            "remove",
            "--force",
            worktree_path.to_str().unwrap_or_default(),
        ])
        .status();
    let _ = git(repo_root, &["branch", "-D", branch]);
}

/// Run the objective in a throwaway worktree checked out from `ref_name`, and
/// return its outcome (including the parsed metric snapshot). The worktree and
/// temporary branch are force-removed afterwards — they are ours, not user work.
pub fn run_objective_on_ref(
    repo_root: &Path,
    worktree_root: &Path,
    ref_name: &str,
    objective: &ObjectiveSpec,
) -> Result<ObjectiveOutcome> {
    let id = short_id();
    let branch = format!("baseline/{id}");
    let path = worktree_root.join(format!("baseline-{id}"));
    worktree::create(repo_root, worktree_root, &path, &branch, Some(ref_name))?;
    let result = run_objective(&path, objective);
    discard_worktree(repo_root, &path, &branch);
    result
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromotionResult {
    pub promoted: bool,
    pub target_ref: String,
    pub branch: String,
    pub new_head: Option<String>,
    pub error: Option<String>,
}

/// Promote an integration branch into `target_ref` via a fast-forward merge in
/// the target checkout. Fails closed (promoted=false, no reset) when the target
/// has diverged and cannot fast-forward.
pub fn promote_integration(
    repo_root: &Path,
    worktree: &Path,
    integration_branch: &str,
    target_ref: &str,
) -> Result<PromotionResult> {
    let worktree =
        crate::merge::validate_merge_target(repo_root, worktree, integration_branch, target_ref)?;
    let out = Command::new("git")
        .arg("-C")
        .arg(&worktree)
        .args(["merge", "--ff-only", integration_branch])
        .output()?;
    if out.status.success() {
        let new_head = git(&worktree, &["rev-parse", "HEAD"])?.trim().to_owned();
        Ok(PromotionResult {
            promoted: true,
            target_ref: target_ref.to_owned(),
            branch: integration_branch.to_owned(),
            new_head: Some(new_head),
            error: None,
        })
    } else {
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(PromotionResult {
            promoted: false,
            target_ref: target_ref.to_owned(),
            branch: integration_branch.to_owned(),
            new_head: None,
            error: Some(combined.trim().to_owned()),
        })
    }
}

fn validate(spec: &IntegrateSpec) -> Result<()> {
    if spec.target_ref.trim().is_empty() {
        bail!("target_ref is required");
    }
    if spec.branches.is_empty() {
        bail!("at least one branch is required");
    }
    if !Path::new(&spec.repo).is_dir() {
        bail!("repo is not a directory: {}", spec.repo);
    }
    for branch in &spec.branches {
        if branch.trim().is_empty() {
            bail!("branch must not be empty");
        }
    }
    if let Some(objective) = &spec.objective {
        if objective.argv.is_empty() || objective.argv[0].trim().is_empty() {
            bail!("objective argv is required");
        }
        if objective.timeout_ms == 0 {
            bail!("objective timeout_ms must be positive");
        }
    }
    std::fs::create_dir_all(&spec.worktree_root)?;
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

fn short_id() -> String {
    crate::process::unique_id()
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

    fn repo_fixture(dir: &Path) -> std::path::PathBuf {
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        gitc(&repo, &["init", "-b", "main"]);
        gitc(&repo, &["config", "user.email", "t@t"]);
        gitc(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("README"), "base\n").unwrap();
        gitc(&repo, &["add", "README"]);
        gitc(&repo, &["commit", "-m", "base"]);
        repo
    }

    fn lane_file(repo: &Path, wt_root: &Path, branch: &str, filename: &str, content: &str) {
        let worktree = wt_root.join(branch.replace('/', "_"));
        gitc(
            repo,
            &["worktree", "add", "-b", branch, worktree.to_str().unwrap()],
        );
        std::fs::write(worktree.join(filename), content).unwrap();
        gitc(&worktree, &["add", filename]);
        gitc(&worktree, &["commit", "-m", branch]);
    }

    #[test]
    fn integrate_merges_lanes_and_runs_objective() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_fixture(dir.path());
        let wt_root = dir.path().join("wt");
        std::fs::create_dir_all(&wt_root).unwrap();
        lane_file(&repo, &wt_root, "lane/a", "a.txt", "a\n");
        lane_file(&repo, &wt_root, "lane/b", "b.txt", "b\n");
        let report = integrate_lanes(&IntegrateSpec {
            repo: repo.display().to_string(),
            worktree_root: wt_root.display().to_string(),
            target_ref: "main".to_owned(),
            branches: vec!["lane/a".to_owned(), "lane/b".to_owned()],
            objective: Some(ObjectiveSpec {
                argv: vec![
                    "sh".to_owned(),
                    "-c".to_owned(),
                    "test -f a.txt -a -f b.txt".to_owned(),
                ],
                output_dir: dir.path().join("out").display().to_string(),
                timeout_ms: 10_000,
            }),
        })
        .unwrap();
        assert!(report.merged);
        assert!(report.conflict.is_none());
        assert_eq!(report.objective.as_ref().unwrap().status, "passed");
        assert!(report.files_changed.iter().any(|f| f == "a.txt"));
        assert!(report.files_changed.iter().any(|f| f == "b.txt"));
        assert_eq!(report.cleanup, "kept");
    }

    #[test]
    fn integrate_conflict_is_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_fixture(dir.path());
        let wt_root = dir.path().join("wt");
        std::fs::create_dir_all(&wt_root).unwrap();
        lane_file(&repo, &wt_root, "lane/a", "README", "a\n");
        lane_file(&repo, &wt_root, "lane/b", "README", "b\n");
        let report = integrate_lanes(&IntegrateSpec {
            repo: repo.display().to_string(),
            worktree_root: wt_root.display().to_string(),
            target_ref: "main".to_owned(),
            branches: vec!["lane/a".to_owned(), "lane/b".to_owned()],
            objective: None,
        })
        .unwrap();
        assert!(!report.merged);
        assert_eq!(report.conflict.as_deref(), Some("lane/b"));
        assert_eq!(report.cleanup, "retired");
        // integration branch removed; main untouched.
        let branches = git(&repo, &["branch", "--list"]).unwrap();
        assert!(!branches.contains("integration/"));
        assert_eq!(
            std::fs::read_to_string(repo.join("README")).unwrap(),
            "base\n"
        );
    }
}
