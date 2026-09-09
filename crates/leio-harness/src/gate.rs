//! Improvement gate: capture a baseline snapshot, integrate the lanes, evaluate
//! improvement, and promote the integration branch into the target only when
//! the collaborative result provably beats the baseline.
//!
//! The objective command prints an `OutcomeSnapshot` JSON object to stdout. It
//! is run once on `baseline_ref` (single-agent baseline) and once on the
//! integrated tree. `evaluate_improvement` decides the verdict; promotion
//! (fast-forward of `integration/<id>` into `target_ref`) happens only on
//! `Improved` and only when `promote` is set.
use crate::improvement::{self, ImprovementVerdict, OutcomeSnapshot};
use crate::integrate::{self, IntegrationReport, ObjectiveSpec, PromotionResult};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GateSpec {
    pub repo: String,
    pub worktree_root: String,
    pub target_ref: String,
    pub baseline_ref: String,
    pub branches: Vec<String>,
    pub objective: ObjectiveSpec,
    #[serde(default = "default_isotropy_threshold")]
    pub isotropy_threshold: f32,
    #[serde(default)]
    pub promote: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GateReport {
    pub verdict: ImprovementVerdict,
    pub baseline: OutcomeSnapshot,
    pub collab: Option<OutcomeSnapshot>,
    pub improvement: Option<improvement::ImprovementReport>,
    pub integration: IntegrationReport,
    pub promotion: Option<PromotionResult>,
    pub promoted: bool,
}

pub fn run_gate(spec: &GateSpec) -> Result<GateReport> {
    validate(spec)?;
    let repo = Path::new(&spec.repo);
    let worktree_root = Path::new(&spec.worktree_root);

    let baseline =
        integrate::run_objective_on_ref(repo, worktree_root, &spec.baseline_ref, &spec.objective)?
            .snapshot
            .context("baseline objective produced no outcome snapshot")?;

    let integration = integrate::integrate_lanes(&integrate::IntegrateSpec {
        repo: spec.repo.clone(),
        worktree_root: spec.worktree_root.clone(),
        target_ref: spec.target_ref.clone(),
        branches: spec.branches.clone(),
        objective: Some(spec.objective.clone()),
    })?;

    let (verdict, collab, improvement_report, promotion, promoted) = if !integration.merged {
        // Fail-closed: a conflict means there is no integrated tree to
        // evaluate, so nothing can be promoted.
        (ImprovementVerdict::NoImprovement, None, None, None, false)
    } else {
        let collab = integration
            .objective
            .as_ref()
            .and_then(|objective| objective.snapshot.clone())
            .context("integration objective produced no outcome snapshot")?;
        let report =
            improvement::evaluate_improvement(&baseline, &collab, spec.isotropy_threshold)?;
        let improved = report.verdict == ImprovementVerdict::Improved;
        let promotion = if improved && spec.promote {
            Some(integrate::promote_integration(
                repo,
                repo,
                &integration.integration_branch,
                &spec.target_ref,
            )?)
        } else {
            None
        };
        let promoted = promotion
            .as_ref()
            .map(|promotion| promotion.promoted)
            .unwrap_or(false);
        (
            report.verdict,
            Some(collab),
            Some(report),
            promotion,
            promoted,
        )
    };

    // The integration worktree/branch is throwaway: after the gate decision it
    // is discarded (a promoted branch is already fast-forwarded into target).
    integrate::discard_worktree(
        repo,
        Path::new(&integration.integration_worktree),
        &integration.integration_branch,
    );

    Ok(GateReport {
        verdict,
        baseline,
        collab,
        improvement: improvement_report,
        integration,
        promotion,
        promoted,
    })
}

fn validate(spec: &GateSpec) -> Result<()> {
    if spec.target_ref.trim().is_empty() {
        bail!("target_ref is required");
    }
    if spec.baseline_ref.trim().is_empty() {
        bail!("baseline_ref is required");
    }
    if spec.branches.is_empty() {
        bail!("at least one branch is required");
    }
    if spec.objective.argv.is_empty() || spec.objective.argv[0].trim().is_empty() {
        bail!("objective argv is required");
    }
    if spec.objective.timeout_ms == 0 {
        bail!("objective timeout_ms must be positive");
    }
    if !Path::new(&spec.repo).is_dir() {
        bail!("repo is not a directory: {}", spec.repo);
    }
    std::fs::create_dir_all(&spec.worktree_root)?;
    Ok(())
}

fn default_isotropy_threshold() -> f32 {
    2.0
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

    /// Objective that reports `quality: 1.0` when the integration delivered
    /// `improve.txt`, else `quality: 0.5`.
    fn improving_objective() -> ObjectiveSpec {
        ObjectiveSpec {
            argv: vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "if test -f improve.txt; then printf '{\"metrics\":{\"quality\":1.0}}'; else printf '{\"metrics\":{\"quality\":0.5}}'; fi"
                    .to_owned(),
            ],
            output_dir: String::new(), // set per-test
            timeout_ms: 10_000,
        }
    }

    #[test]
    fn gate_promotes_when_integration_improves() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_fixture(dir.path());
        let wt_root = dir.path().join("wt");
        std::fs::create_dir_all(&wt_root).unwrap();
        lane_file(&repo, &wt_root, "lane/a", "improve.txt", "yes\n");
        let mut objective = improving_objective();
        objective.output_dir = dir.path().join("out").display().to_string();
        let report = run_gate(&GateSpec {
            repo: repo.display().to_string(),
            worktree_root: wt_root.display().to_string(),
            target_ref: "main".to_owned(),
            baseline_ref: "main".to_owned(),
            branches: vec!["lane/a".to_owned()],
            objective,
            isotropy_threshold: 2.0,
            promote: true,
        })
        .unwrap();
        assert_eq!(report.verdict, ImprovementVerdict::Improved);
        assert!(report.promoted);
        assert!(report.promotion.as_ref().unwrap().promoted);
        // main was fast-forwarded to include the lane's file.
        assert_eq!(
            std::fs::read_to_string(repo.join("improve.txt")).unwrap(),
            "yes\n"
        );
    }

    #[test]
    fn gate_does_not_promote_without_improvement() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_fixture(dir.path());
        let wt_root = dir.path().join("wt");
        std::fs::create_dir_all(&wt_root).unwrap();
        lane_file(&repo, &wt_root, "lane/a", "other.txt", "x\n");
        let mut objective = improving_objective();
        objective.output_dir = dir.path().join("out").display().to_string();
        let report = run_gate(&GateSpec {
            repo: repo.display().to_string(),
            worktree_root: wt_root.display().to_string(),
            target_ref: "main".to_owned(),
            baseline_ref: "main".to_owned(),
            branches: vec!["lane/a".to_owned()],
            objective,
            isotropy_threshold: 2.0,
            promote: true,
        })
        .unwrap();
        assert_eq!(report.verdict, ImprovementVerdict::NoImprovement);
        assert!(!report.promoted);
        assert!(report.promotion.is_none());
        assert!(!repo.join("other.txt").exists());
    }

    #[test]
    fn gate_is_fail_closed_on_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_fixture(dir.path());
        let wt_root = dir.path().join("wt");
        std::fs::create_dir_all(&wt_root).unwrap();
        lane_file(&repo, &wt_root, "lane/a", "README", "a\n");
        lane_file(&repo, &wt_root, "lane/b", "README", "b\n");
        let mut objective = improving_objective();
        objective.output_dir = dir.path().join("out").display().to_string();
        let report = run_gate(&GateSpec {
            repo: repo.display().to_string(),
            worktree_root: wt_root.display().to_string(),
            target_ref: "main".to_owned(),
            baseline_ref: "main".to_owned(),
            branches: vec!["lane/a".to_owned(), "lane/b".to_owned()],
            objective,
            isotropy_threshold: 2.0,
            promote: true,
        })
        .unwrap();
        assert!(!report.integration.merged);
        assert_eq!(report.verdict, ImprovementVerdict::NoImprovement);
        assert!(!report.promoted);
        assert_eq!(
            std::fs::read_to_string(repo.join("README")).unwrap(),
            "base\n"
        );
    }
}
