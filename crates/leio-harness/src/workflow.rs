//! Durable, same-user workflow controller. Evidence never grants execution authority.
use crate::{
    model::{RunSpec, RunStatus},
    process,
};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub request: String,
    pub constraints: Vec<String>,
    pub questions: Vec<String>,
    pub acceptance: Vec<String>,
    pub steps: Vec<Step>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub name: String,
    /// edit, check, build, deploy, verify, or rollback. Deployment is explicitly enabled.
    pub kind: String,
    pub argv: Vec<String>,
    pub timeout_ms: u64,
    /// Only idempotent steps may be retried after a known failure.
    pub retry_safe: bool,
    pub artifacts: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    #[serde(rename = "@context")]
    pub context: serde_json::Value,
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@type")]
    pub entity_type: String,
    pub version: u32,
    pub repo: PathBuf,
    pub revision: u64,
    pub plan: Plan,
    pub digest: String,
    pub confirmed: bool,
    pub approved: bool,
    pub deploy_enabled: bool,
    pub evidence: Vec<serde_json::Value>,
    pub completed: usize,
    pub attempts: u64,
    pub state: String,
    pub history: Vec<serde_json::Value>,
}
fn digest(id: &str, revision: u64, plan: &Plan) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&(
        id, revision, plan,
    ))?)))
}
fn validate(plan: &Plan) -> Result<()> {
    ensure!(
        !plan.request.trim().is_empty() && !plan.acceptance.is_empty(),
        "request and acceptance criteria required"
    );
    ensure!(!plan.steps.is_empty(), "plan needs executable steps");
    for step in &plan.steps {
        ensure!(
            !step.argv.is_empty() && !step.argv[0].is_empty() && step.timeout_ms > 0,
            "invalid step command or timeout"
        );
        ensure!(
            ["edit", "check", "build", "deploy", "verify", "rollback"]
                .contains(&step.kind.as_str()),
            "unknown step kind"
        );
        for artifact in &step.artifacts {
            ensure!(
                !Path::new(artifact).is_absolute()
                    && !Path::new(artifact)
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir)),
                "artifact must be repo-relative"
            );
        }
    }
    Ok(())
}
fn save(dir: &Path, state: &Workflow) -> Result<()> {
    let mut file = fs::File::create(dir.join("state.jsonld.tmp"))?;
    file.write_all(&serde_json::to_vec_pretty(state)?)?;
    file.sync_all()?;
    fs::rename(dir.join("state.jsonld.tmp"), dir.join("state.jsonld"))?;
    fs::File::open(dir)?.sync_all()?;
    Ok(())
}
fn event(state: &mut Workflow, kind: &str, detail: serde_json::Value) {
    state.history.push(serde_json::json!({"@id":format!("{}:event:{}",state.id,state.history.len()+1),"@type":"prov:Activity","at":chrono::Utc::now().to_rfc3339(), "revision":state.revision,"event":kind,"detail":detail}));
}

struct WorkflowLock(fs::File);
impl Drop for WorkflowLock {
    fn drop(&mut self) {
        // Release explicitly: concurrent process spawning can briefly inherit
        // this descriptor before exec, extending a close-only flock lifetime.
        let _ = FileExt::unlock(&self.0);
    }
}

#[cfg(test)]
mod lock_tests {
    use super::*;

    #[test]
    fn transition_unlocks_even_with_a_duplicated_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.try_lock_exclusive().unwrap();
        let inherited = file.try_clone().unwrap();
        drop(WorkflowLock(file));
        let next = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        next.try_lock_exclusive().unwrap();
        FileExt::unlock(&next).unwrap();
        drop(inherited);
    }
}
/// All transitions serialize under an exclusive file lock. A crashed step stays
/// uncertain and cannot be blindly replayed: its external effects need reconciliation.
pub fn apply(
    dir: &Path,
    action: &str,
    input: Option<&Path>,
    approval: Option<&str>,
    repo: Option<&Path>,
    deploy_enabled: bool,
) -> Result<Workflow> {
    apply_with_deployment_policy(dir, action, input, approval, repo, deploy_enabled, true)
}

/// Hosts may further restrict deployment. Evaluate that restriction under the
/// same lock as step selection, so concurrent progress cannot bypass the gate.
pub fn apply_with_deployment_policy(
    dir: &Path,
    action: &str,
    input: Option<&Path>,
    approval: Option<&str>,
    repo: Option<&Path>,
    deploy_enabled: bool,
    deployment_allowed: bool,
) -> Result<Workflow> {
    fs::create_dir_all(dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join("lock"))?;
    lock.try_lock_exclusive()
        .context("workflow is owned by another operation")?;
    let _lock = WorkflowLock(lock);
    if action == "init" {
        ensure!(
            !dir.join("state.jsonld").exists(),
            "workflow already exists"
        );
        let plan: Plan =
            serde_json::from_slice(&fs::read(input.context("--input plan required")?)?)?;
        validate(&plan)?;
        let root = repo.context("--repo required")?.canonicalize()?;
        ensure!(root.is_dir(), "repository must be a directory");
        let id = format!(
            "urn:leio:workflow:run:{}",
            hex::encode(Sha256::digest(
                dir.canonicalize()?.to_string_lossy().as_bytes()
            ))
        );
        let state = Workflow {
            context: serde_json::json!({"@version":1.1,"@vocab":"urn:leio:workflow:","steps":{"@container":"@list"},"argv":{"@container":"@list"},"detail":{"@type":"@json"},"prov":"http://www.w3.org/ns/prov#","history":{"@container":"@list"},"evidence":{"@type":"@json"}}),
            id: id.clone(),
            entity_type: "Workflow".into(),
            version: 1,
            repo: root,
            revision: 1,
            digest: digest(&id, 1, &plan)?,
            plan,
            confirmed: false,
            approved: false,
            deploy_enabled,
            evidence: vec![],
            completed: 0,
            attempts: 0,
            state: "draft".into(),
            history: vec![],
        };
        save(dir, &state)?;
        return Ok(state);
    }
    let mut state: Workflow = serde_json::from_slice(&fs::read(dir.join("state.jsonld"))?)?;
    if action == "show" {
        return Ok(state);
    }
    if action == "reconcile" {
        ensure!(
            state.state == "running",
            "only interrupted steps need reconciliation"
        );
        ensure!(
            approval == Some(state.digest.as_str()),
            "current approval required"
        );
        let note = fs::read_to_string(input.context("--input reconciliation note required")?)?;
        ensure!(
            !note.trim().is_empty(),
            "reconciliation must explain observed effects"
        );
        event(&mut state, "reconciled", serde_json::json!({"note":note}));
        state.state = "failed".into();
        state.approved = false;
        save(dir, &state)?;
        return Ok(state);
    }
    ensure!(
        state.state != "running",
        "interrupted step has unknown effects; inspect logs and reconcile before revising; automatic replay refused"
    );
    match action {
        "revise" => {
            let plan: Plan =
                serde_json::from_slice(&fs::read(input.context("--input required")?)?)?;
            validate(&plan)?;
            let previous = serde_json::json!({"plan":state.plan,"evidence":state.evidence,"digest":state.digest,"confirmed":state.confirmed,"approved":state.approved,"completed":state.completed,"state":state.state});
            event(&mut state, "superseded", previous);
            state.digest = digest(&state.id, state.revision + 1, &plan)?;
            state.plan = plan;
            state.revision += 1;
            state.confirmed = false;
            state.approved = false;
            state.completed = 0;
            state.evidence.clear();
            state.state = "draft".into();
        }
        "evidence" => {
            ensure!(!state.approved, "revise before replacing approved evidence");
            let value: serde_json::Value =
                serde_json::from_slice(&fs::read(input.context("--input evidence required")?)?)?;
            state.evidence.push(value);
        }
        "confirm" => {
            ensure!(
                approval == Some(state.digest.as_str()),
                "confirmation must name current plan digest"
            );
            ensure!(
                state.plan.questions.is_empty(),
                "resolve open questions before confirmation"
            );
            state.confirmed = true;
        }
        "approve" => {
            ensure!(
                state.confirmed && !state.evidence.is_empty(),
                "confirm requirements and attach LEIO evidence first"
            );
            ensure!(
                approval == Some(state.digest.as_str()),
                "approval must name current plan digest"
            );
            state.approved = true;
            if state.state != "failed" && state.state != "completed" {
                state.state = "approved".into();
            }
        }
        "execute" => {
            ensure!(
                state.approved && approval == Some(state.digest.as_str()),
                "explicit execution grant for current plan required"
            );
            ensure!(
                state.completed < state.plan.steps.len(),
                "workflow already completed"
            );
            let step = state.plan.steps[state.completed].clone();
            ensure!(
                !["deploy", "rollback"].contains(&step.kind.as_str())
                    || (state.deploy_enabled && deployment_allowed),
                "deployment disabled"
            );
            ensure!(
                state.state != "failed" || step.retry_safe,
                "step is not safe to retry; reconcile effects and revise"
            );
            state.attempts += 1;
            state.state = "running".into();
            event(&mut state, "started", serde_json::to_value(&step)?);
            save(dir, &state)?;
            let spec = RunSpec {
                run_id: format!("attempt-{}", state.attempts),
                argv: step.argv,
                cwd: state.repo.display().to_string(),
                output_dir: dir.canonicalize()?.display().to_string(),
                timeout_ms: step.timeout_ms,
                kill_grace_ms: 1000,
                max_output_bytes: 1024 * 1024,
                env_allowlist: vec![],
                env: Default::default(),
                required_approval_token: None,
                approval_token: None,
            };
            let result = process::run(spec);
            let mut passed = result.as_ref().is_ok_and(|r| r.status == RunStatus::Passed);
            let mut artifacts = Vec::new();
            // Failed commands can still produce useful partial artifacts.
            for name in step.artifacts {
                match state.repo.join(&name).canonicalize().and_then(|path| {
                        if !path.starts_with(&state.repo) { return Err(std::io::Error::other("artifact escapes repository")); }
                        fs::read(path)
                    }) {
                        Ok(bytes)=>artifacts.push(serde_json::json!({"path":name,"sha256":hex::encode(Sha256::digest(bytes))})),
                        Err(error)=>{passed=false; artifacts.push(serde_json::json!({"path":name,"error":error.to_string()}));}
                    }
            }
            event(
                &mut state,
                "result",
                serde_json::json!({"kind":step.kind,"passed":passed,"process":result.as_ref().ok(),"error":result.as_ref().err().map(|e|e.to_string()),"artifacts":artifacts}),
            );
            if passed {
                state.completed += 1;
                state.state = if state.completed == state.plan.steps.len() {
                    "completed"
                } else {
                    "approved"
                }
                .into();
            } else if result.is_err() {
                // Adapter errors may occur after spawn or after external effects.
                // Keep the uncertain state until an operator reconciles it.
                state.state = "running".into();
                state.approved = false;
            } else {
                state.state = "failed".into();
            }
        }
        "notification-failed" => {
            event(
                &mut state,
                "notification-failed",
                serde_json::json!({"detail":approval.context("--approval notification detail required")?}),
            );
        }
        _ => anyhow::bail!("unknown workflow action"),
    }
    event(&mut state, action, serde_json::Value::Null);
    save(dir, &state)?;
    Ok(state)
}
