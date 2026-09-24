//! `day` orchestrator: one goal, many lanes. Each lane gets a worktree, an
//! atomic lease, a supervised process run, and bus publication of its outcome
//! — the human sees one ordered report, never agent chatter.
use crate::agents::{AgentTemplate, cached_table, resolve_lane_invocation, resolve_model};
use crate::bus_client::BusClient;
#[cfg(feature = "codeview")]
use crate::codeview::check_code_view;
use crate::embed::EmbedClient;
use crate::lease::LeaseStore;
use crate::manifest::{DayManifest, collect_run_artifacts, write_day_manifest};
use crate::model::{AgentLease, LeaseState, LockScope, RunResult, RunSpec, RunStatus};
use crate::receipts::{ReceiptsReport, check_text};
use crate::{process, worktree};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::mpsc::Sender;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DayLane {
    pub agent_id: String,
    pub task: String,
    /// Explicit argv wins. When omitted, `agent` resolves a CLI template.
    #[serde(default)]
    pub argv: Vec<String>,
    /// Agent template name: codex | claude | gemini | custom key.
    #[serde(default)]
    pub agent: Option<String>,
    /// Model placeholder for templates containing {model}.
    #[serde(default)]
    pub model: Option<String>,
    /// Work-shape key. Fills `model` from the OpenRouter table when `model`
    /// is omitted. Built-in CLIs ignore this because they have no `{model}`.
    #[serde(default)]
    pub work_shape: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegratePhaseSpec {
    pub target_ref: String,
    pub objective: Option<crate::integrate::ObjectiveSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaySpec {
    pub goal: String,
    pub repo: String,
    pub worktree_root: String,
    pub output_dir: String,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub parallel: bool,
    /// Fail fast unless every lane starts from a fresh leio-code index.
    #[serde(default)]
    pub require_fresh_code_view: bool,
    /// A lane whose own output cites a LEIO query id that never resolves in
    /// its worktree's `.leio-code/events/events.ndjson`, or a malformed
    /// Reference Provider id, is reported as `failed` regardless of process exit
    /// status. The check runs before the worktree is retired (its PROV
    /// sidecar does not survive retirement) and never calls out to a network
    /// service — Reference Provider ids are checked for shape only.
    #[serde(default)]
    pub require_receipts_check: bool,
    /// Custom agent templates, merged over the built-ins.
    #[serde(default)]
    pub agent_templates: std::collections::BTreeMap<String, AgentTemplate>,
    pub lanes: Vec<DayLane>,
    /// Optional integration phase: after lanes finish, stack their branches
    /// into an integration branch and run an objective on the integrated tree.
    #[serde(default)]
    pub integrate: Option<IntegratePhaseSpec>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneOutcome {
    pub agent_id: String,
    pub run_id: String,
    pub branch: String,
    pub worktree: String,
    pub status: String,
    pub duration_ms: u64,
    pub error: Option<String>,
    #[serde(default)]
    pub artifacts: Vec<crate::manifest::ArtifactEntry>,
    /// Present only when `DaySpec.require_receipts_check` is set and the
    /// process itself passed. `None` means the check did not run, not that
    /// it passed — check `status` for the final verdict either way.
    #[serde(default)]
    pub receipts: Option<crate::receipts::ReceiptsReport>,
}

#[derive(Debug, Clone)]
pub enum DayEvent {
    LaneStarted {
        agent_id: String,
        run_id: String,
        branch: String,
    },
    LaneFinished {
        run_id: String,
        status: String,
        duration_ms: u64,
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayReport {
    pub goal: String,
    pub outcomes: Vec<LaneOutcome>,
    pub passed: usize,
    pub failed: usize,
    pub lease_store: String,
    #[serde(default)]
    pub integration: Option<crate::integrate::IntegrationReport>,
}

#[derive(Debug)]
struct CheckedRun {
    run: RunResult,
    receipts: Option<ReceiptsReport>,
}

/// Execute a day spec: lanes in isolated worktrees with leases, supervised
/// runs, results published to `result/<lane>` on the bus when available.
pub async fn run_day(spec: &DaySpec, bus_addr: Option<&str>) -> Result<DayReport> {
    run_day_with_events(spec, bus_addr, None).await
}

pub async fn run_day_with_events(
    spec: &DaySpec,
    bus_addr: Option<&str>,
    events: Option<Sender<DayEvent>>,
) -> Result<DayReport> {
    validate_day(spec)?;
    let repo = Path::new(&spec.repo);
    let worktree_root = Path::new(&spec.worktree_root);
    let lease_path = Path::new(&spec.output_dir).join("leases.json");
    let lease_store = LeaseStore::new(&lease_path);
    let mut bus = match bus_addr {
        Some(addr) => Some(
            BusClient::connect_with_retry(addr, 5, std::time::Duration::from_millis(100)).await?,
        ),
        None => None,
    };
    // Global code view is mandatory: publish the repo fingerprint to the bus
    // before any lane starts. A required fresh code view also requires delivery.
    if let Some(addr) = bus_addr {
        #[cfg(feature = "codeview")]
        {
            let embed = EmbedClient::from_env().ok();
            if let Err(error) = crate::codeview::publish_code_view(
                repo,
                addr,
                "orchestrator",
                &format!("day-{}", std::process::id()),
                embed.as_ref(),
            )
            .await
            {
                if spec.require_fresh_code_view {
                    return Err(error).context("required code-view publication failed");
                }
                eprintln!("warning: code-view publish failed: {error}");
            }
        }
        #[cfg(not(feature = "codeview"))]
        {
            let _ = (repo, addr);
            eprintln!("warning: code-view publish skipped: built without the `codeview` feature");
        }
    }
    if spec.parallel {
        run_parallel(spec, repo, worktree_root, &lease_store, bus_addr, events).await
    } else {
        let mut outcomes = Vec::new();
        for lane in &spec.lanes {
            outcomes.push(
                run_lane(
                    spec,
                    lane,
                    repo,
                    worktree_root,
                    &lease_store,
                    &mut bus,
                    events.clone(),
                )
                .await,
            );
        }
        Ok(report(spec, outcomes, &lease_store))
    }
}

async fn run_parallel(
    spec: &DaySpec,
    repo: &Path,
    worktree_root: &Path,
    lease_store: &LeaseStore,
    bus_addr: Option<&str>,
    events: Option<Sender<DayEvent>>,
) -> Result<DayReport> {
    let mut clients = Vec::new();
    // Preflight every transport before spawning any lane.
    for _ in &spec.lanes {
        clients.push(match bus_addr {
            Some(addr) => Some(
                BusClient::connect_with_retry(addr, 5, std::time::Duration::from_millis(100))
                    .await?,
            ),
            None => None,
        });
    }
    let mut handles = Vec::new();
    for (lane, mut lane_bus) in spec.lanes.iter().zip(clients) {
        let spec = spec.clone();
        let lane = lane.clone();
        let repo = repo.to_path_buf();
        let root = worktree_root.to_path_buf();
        let store = LeaseStore::new(lease_store.path().to_path_buf());
        let lane_events = events.clone();
        handles.push(tokio::spawn(async move {
            run_lane(
                &spec,
                &lane,
                &repo,
                &root,
                &store,
                &mut lane_bus,
                lane_events,
            )
            .await
        }));
    }
    let mut outcomes = Vec::new();
    for handle in handles {
        outcomes.push(handle.await.context("lane task panicked")?);
    }
    Ok(report(spec, outcomes, lease_store))
}

async fn run_lane(
    spec: &DaySpec,
    lane: &DayLane,
    repo: &Path,
    worktree_root: &Path,
    lease_store: &LeaseStore,
    bus: &mut Option<BusClient>,
    events: Option<Sender<DayEvent>>,
) -> LaneOutcome {
    let run_id = format!("{}-{}", lane.agent_id, short_id());
    let branch = format!("agents/{}/{}", sanitize(&lane.agent_id), sanitize(&run_id));
    let worktree_path = worktree_root.join(sanitize(&run_id));
    let base = LaneOutcome {
        agent_id: lane.agent_id.clone(),
        run_id: run_id.clone(),
        branch: branch.clone(),
        worktree: worktree_path.display().to_string(),
        status: "infra_error".to_owned(),
        duration_ms: 0,
        error: None,
        artifacts: Vec::new(),
        receipts: None,
    };
    let lease = AgentLease {
        agent_id: lane.agent_id.clone(),
        run_id: run_id.clone(),
        worktree_path: worktree_path.display().to_string(),
        branch: branch.clone(),
        owner: "leio-harness".to_owned(),
        heartbeat: Utc::now().to_rfc3339(),
        state: LeaseState::Active,
        lock_scopes: vec![LockScope::Worktree, LockScope::Branch],
        deployment_target: None,
        build_concurrency_group: None,
        scope_keys: Vec::new(),
    };
    if let Err(error) = lease_store.acquire(lease) {
        return LaneOutcome {
            error: Some(error.to_string()),
            ..base
        };
    }
    if let Some(tx) = &events {
        let _ = tx.send(DayEvent::LaneStarted {
            agent_id: lane.agent_id.clone(),
            run_id: run_id.clone(),
            branch: branch.clone(),
        });
    }
    let outcome = async {
        if let Some(client) = bus.as_mut() {
            client
                .publish(vec![crate::bus::new_embedding_row(
                    &lane.agent_id,
                    &run_id,
                    &format!("intent/{}", sanitize(&lane.agent_id)),
                    status_vector("running"),
                )])
                .await
                .context("publish lane intent before execution")?;
        }
        worktree::create(repo, worktree_root, &worktree_path, &branch, None)?;
        let model = resolve_model(
            lane.model.as_deref(),
            lane.work_shape.as_deref(),
            cached_table().as_ref(),
        )?;
        let (argv, mut env) = resolve_lane_invocation(
            (!lane.argv.is_empty()).then_some(lane.argv.as_slice()),
            lane.agent.as_deref(),
            &lane.task,
            model.as_deref(),
            &spec.agent_templates,
        )?;
        if let Some(client) = bus.as_ref() {
            env.insert("LEIO_HARNESS_BUS".to_owned(), client.address().to_owned());
            env.insert("LEIO_HARNESS_AGENT_ID".to_owned(), lane.agent_id.clone());
            env.insert("LEIO_HARNESS_RUN_ID".to_owned(), run_id.clone());
        }
        let run_spec = RunSpec {
            run_id: run_id.clone(),
            argv,
            cwd: worktree_path.display().to_string(),
            output_dir: spec.output_dir.clone(),
            timeout_ms: lane.timeout_ms.unwrap_or(spec.timeout_ms),
            kill_grace_ms: 2_000,
            max_output_bytes: 4 * 1024 * 1024,
            env_allowlist: env.keys().cloned().collect(),
            env,
            required_approval_token: None,
            approval_token: None,
        };
        let run = tokio::task::spawn_blocking(move || process::run(run_spec))
            .await
            .context("process supervisor panicked")??;
        // The lane-local PROV sidecar is removed when the worktree retires,
        // so receipt resolution must happen before commit/retirement. Preserve
        // the existing commit behavior even when the receipt verdict fails:
        // the lane branch remains auditable, but it cannot count as passed or
        // enter the integration phase.
        let receipts = if spec.require_receipts_check && run.status == RunStatus::Passed {
            let stdout = std::fs::read(&run.stdout_path).with_context(|| {
                format!("read lane stdout for receipt check: {}", run.stdout_path)
            })?;
            Some(check_text(
                &String::from_utf8_lossy(&stdout),
                &[worktree_path.as_path()],
            ))
        } else {
            None
        };
        let commit_msg = format!(
            "lane({}): {}",
            lane.agent_id,
            lane.task.lines().next().unwrap_or("lane changes")
        );
        worktree::commit_lane(&worktree_path, &commit_msg)?;
        worktree::retire(repo, worktree_root, &worktree_path)?;
        Ok::<_, anyhow::Error>(CheckedRun { run, receipts })
    }
    .await;
    let _ = lease_store.release(&run_id, Utc::now());
    let final_outcome = match outcome {
        Ok(mut checked) => {
            let (mut status, mut error) = effective_result(&checked.run, checked.receipts.as_ref());
            let directory = Path::new(&checked.run.stdout_path)
                .parent()
                .expect("run log parent")
                .to_path_buf();
            if let Some(client) = bus {
                let delivery = publish_result(client, lane, &run_id, status, &checked.run).await;
                let receipt = match delivery {
                    Ok(seq) => {
                        serde_json::json!({"status":"acknowledged", "lastSeq":seq,"address":client.address()})
                    }
                    Err(delivery) => {
                        status = "infra_error";
                        error = Some(format!(
                            "bus result delivery failed: {delivery:#}; process status: {:?}; prior error: {error:?}",
                            checked.run.status
                        ));
                        checked.run.status = RunStatus::InfraError;
                        checked.run.error = error.clone();
                        serde_json::json!({"status":"failed", "error":error,"address":client.address()})
                    }
                };
                if let Err(e) =
                    process::write_json_atomic(&directory.join("bus-delivery.json"), &receipt)
                        .and_then(|()| {
                            process::write_json_atomic(&directory.join("result.json"), &checked.run)
                        })
                {
                    status = "infra_error";
                    error = Some(format!(
                        "failed to persist bus delivery receipt: {e:#}; previous error: {error:?}"
                    ));
                }
            }
            let artifacts = collect_run_artifacts(
                &[
                    checked.run.stdout_path.clone(),
                    checked.run.stderr_path.clone(),
                    checked.run.events_arrow_path.clone(),
                    directory.join("result.json").display().to_string(),
                    directory.join("bus-delivery.json").display().to_string(),
                ],
                &format!("lane:{}", lane.agent_id),
            );
            LaneOutcome {
                status: status.to_owned(),
                duration_ms: checked.run.duration_ms,
                error,
                artifacts,
                receipts: checked.receipts,
                ..base
            }
        }
        Err(error) => LaneOutcome {
            error: Some(error.to_string()),
            ..base
        },
    };
    if let Some(tx) = &events {
        let _ = tx.send(DayEvent::LaneFinished {
            run_id,
            status: final_outcome.status.clone(),
            duration_ms: final_outcome.duration_ms,
            error: final_outcome.error.clone(),
        });
    }
    final_outcome
}

fn effective_result(
    run: &RunResult,
    receipts: Option<&ReceiptsReport>,
) -> (&'static str, Option<String>) {
    if run.status == RunStatus::Passed
        && let Some(report) = receipts
        && !report.ok
    {
        let mut reasons = Vec::new();
        if !report.unresolved_query_ids.is_empty() {
            reasons.push(format!(
                "unresolved LEIO query ids: {}",
                report.unresolved_query_ids.join(", ")
            ));
        }
        if !report.malformed_reference_ids.is_empty() {
            reasons.push(format!(
                "malformed Reference Provider ids: {}",
                report.malformed_reference_ids.join(", ")
            ));
        }
        let detail = if reasons.is_empty() {
            "unknown receipt validation failure".to_owned()
        } else {
            reasons.join("; ")
        };
        return ("failed", Some(format!("receipt check failed: {detail}")));
    }
    let status = match run.status {
        RunStatus::Passed => "passed",
        RunStatus::Failed => "failed",
        RunStatus::TimedOut => "timed_out",
        RunStatus::Canceled => "canceled",
        RunStatus::InfraError => "infra_error",
    };
    (status, run.error.clone())
}

/// Publish lane state: semantic embedding of task+status+stdout tail when an
/// embeddings endpoint is configured (LEIO_HARNESS_EMBED_*); otherwise the
/// deterministic one-hot status vector.
async fn publish_result(
    bus: &mut BusClient,
    lane: &DayLane,
    run_id: &str,
    status: &str,
    run: &crate::model::RunResult,
) -> Result<u64> {
    let vector = match semantic_state_vector(lane, status, run).await {
        Ok(vector) => vector,
        Err(_) => status_vector(status),
    };
    let result_row = crate::bus::new_embedding_row(
        &lane.agent_id,
        run_id,
        &format!("result/{}", sanitize(&lane.agent_id)),
        vector,
    );
    bus.publish(vec![result_row]).await
}

async fn semantic_state_vector(
    lane: &DayLane,
    status: &str,
    run: &crate::model::RunResult,
) -> Result<Vec<f32>> {
    let embed = EmbedClient::from_env()?;
    let stdout_tail = std::fs::read(&run.stdout_path)
        .ok()
        .map(|bytes| {
            let start = bytes.len().saturating_sub(2048);
            String::from_utf8_lossy(&bytes[start..]).into_owned()
        })
        .unwrap_or_default();
    let summary = format!(
        "agent={} task={} status={} exit={:?} tail={}",
        lane.agent_id, lane.task, status, run.exit_code, stdout_tail
    );
    let vectors = embed.embed(std::slice::from_ref(&summary)).await?;
    vectors.into_iter().next().context("no embedding returned")
}

/// Deterministic status encoding (no embedding model required on this path):
/// one-hot-ish vector over [pass, fail, timeout, error].
fn status_vector(status: &str) -> Vec<f32> {
    match status {
        "passed" => vec![1.0, 0.0, 0.0, 0.0],
        "failed" => vec![0.0, 1.0, 0.0, 0.0],
        "timed_out" => vec![0.0, 0.0, 1.0, 0.0],
        _ => vec![0.0, 0.0, 0.0, 1.0],
    }
}

fn report(spec: &DaySpec, outcomes: Vec<LaneOutcome>, store: &LeaseStore) -> DayReport {
    let passed = outcomes.iter().filter(|o| o.status == "passed").count();
    let failed = outcomes.len() - passed;
    let mut artifacts: Vec<_> = outcomes
        .iter()
        .flat_map(|outcome| outcome.artifacts.clone())
        .collect();
    artifacts.sort_by(|left, right| left.path.cmp(&right.path));
    let manifest = DayManifest {
        version: 1,
        goal: spec.goal.clone(),
        generated_at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        artifacts,
    };
    if let Err(error) = write_day_manifest(Path::new(&spec.output_dir), &manifest) {
        eprintln!("warning: failed to write day manifest: {error}");
    }
    let integration = run_integration_phase(spec, &outcomes);
    DayReport {
        goal: spec.goal.clone(),
        outcomes,
        passed,
        failed,
        lease_store: store.path().display().to_string(),
        integration,
    }
}

/// Optional integration phase: merge the branches of every *passed* lane into
/// one integration branch and run the objective on the integrated tree. A
/// conflict surfaces as `merged:false` on the report (fail-closed), never as a
/// hard error.
fn run_integration_phase(
    spec: &DaySpec,
    outcomes: &[LaneOutcome],
) -> Option<crate::integrate::IntegrationReport> {
    let phase = spec.integrate.as_ref()?;
    let branches: Vec<String> = outcomes
        .iter()
        .filter(|outcome| outcome.status == "passed")
        .map(|outcome| outcome.branch.clone())
        .collect();
    if branches.is_empty() {
        return None;
    }
    match crate::integrate::integrate_lanes(&crate::integrate::IntegrateSpec {
        repo: spec.repo.clone(),
        worktree_root: spec.worktree_root.clone(),
        target_ref: phase.target_ref.clone(),
        branches,
        objective: phase.objective.clone(),
    }) {
        Ok(report) => Some(report),
        Err(error) => {
            eprintln!("warning: integration phase failed: {error:#}");
            None
        }
    }
}

fn validate_day(spec: &DaySpec) -> Result<()> {
    if spec.goal.trim().is_empty() {
        bail!("day goal is required");
    }
    if spec.lanes.is_empty() {
        bail!("at least one lane is required");
    }
    if !Path::new(&spec.repo).is_dir() {
        bail!("repo is not a directory: {}", spec.repo);
    }
    let mut ids = std::collections::BTreeSet::new();
    for lane in &spec.lanes {
        anyhow::ensure!(
            lane.agent_id.len() <= 64
                && sanitize(&lane.agent_id) == lane.agent_id
                && !lane.agent_id.contains("..")
                && ids.insert(lane.agent_id.clone()),
            "lane IDs must be unique safe names up to 64 bytes"
        );
        if lane.agent_id.trim().is_empty() {
            bail!("lane requires agent_id");
        }
        let model = resolve_model(
            lane.model.as_deref(),
            lane.work_shape.as_deref(),
            cached_table().as_ref(),
        )
        .map_err(|error| anyhow::anyhow!("lane {}: {error}", lane.agent_id))?;
        resolve_lane_invocation(
            (!lane.argv.is_empty()).then_some(lane.argv.as_slice()),
            lane.agent.as_deref(),
            &lane.task,
            model.as_deref(),
            &spec.agent_templates,
        )
        .map_err(|error| anyhow::anyhow!("lane {}: {error}", lane.agent_id))?;
    }
    if spec.require_fresh_code_view {
        #[cfg(feature = "codeview")]
        {
            let check = check_code_view(Path::new(&spec.repo));
            if !check.fresh {
                bail!("code view gate refused day start: {}", check.reason);
            }
        }
        #[cfg(not(feature = "codeview"))]
        bail!("requireFreshCodeView needs the `codeview` feature; this build has none");
    }
    std::fs::create_dir_all(&spec.worktree_root)?;
    std::fs::create_dir_all(&spec.output_dir)?;
    Ok(())
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-._".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn short_id() -> String {
    crate::process::unique_id()
}

fn default_timeout_ms() -> u64 {
    300_000
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn gitc(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn repo_fixture(root: &Path) -> std::path::PathBuf {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        gitc(&repo, &["init", "-b", "main"]);
        gitc(&repo, &["config", "user.email", "test@example.com"]);
        gitc(&repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
        gitc(&repo, &["add", "README.md"]);
        gitc(&repo, &["commit", "-m", "fixture"]);
        repo
    }

    fn receipt_day_spec(root: &Path, repo: &Path, shell: &str) -> DaySpec {
        DaySpec {
            goal: "verify lane receipts".to_owned(),
            repo: repo.display().to_string(),
            worktree_root: root.join("worktrees").display().to_string(),
            output_dir: root.join("output").display().to_string(),
            timeout_ms: 10_000,
            parallel: false,
            require_fresh_code_view: false,
            require_receipts_check: true,
            agent_templates: Default::default(),
            lanes: vec![DayLane {
                agent_id: "receipt-lane".to_owned(),
                task: "emit evidence".to_owned(),
                argv: vec!["sh".to_owned(), "-c".to_owned(), shell.to_owned()],
                agent: None,
                model: None,
                work_shape: None,
                timeout_ms: None,
            }],
            integrate: None,
        }
    }

    #[test]
    fn day_lane_work_shape_deserializes_and_resolves() {
        let lane: DayLane = serde_json::from_str(
            r#"{
                "agentId": "explorer",
                "task": "map callers",
                "agent": "codex-model",
                "workShape": "terra-low"
            }"#,
        )
        .unwrap();
        // Hermetic: resolve against the built-in snapshot, not the machine's
        // ~/.config cache (a live refresh there would flake this test).
        let model = resolve_model(lane.model.as_deref(), lane.work_shape.as_deref(), None).unwrap();
        assert_eq!(model.as_deref(), Some("deepseek/deepseek-v4-flash-0731"));
    }

    #[test]
    fn openrouter_example_day_spec_deserializes() {
        let spec: DaySpec = serde_json::from_str(include_str!(
            "../../../docs/harness/day-openrouter.example.json"
        ))
        .unwrap();
        assert_eq!(spec.lanes.len(), 6);
        assert!(spec.agent_templates.contains_key("codex-model"));
        assert!(spec.agent_templates.contains_key("claude-model"));
        let coordinator = spec
            .lanes
            .iter()
            .find(|lane| lane.agent_id == "coordinator")
            .expect("coordinator lane");
        assert_eq!(coordinator.agent.as_deref(), Some("grok"));
        assert!(coordinator.work_shape.is_none());
        assert!(coordinator.model.is_none());
    }

    #[tokio::test]
    async fn receipt_gate_keeps_a_resolved_lane_passed() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_fixture(dir.path());
        let query_id = "context-1788477534952084000";
        let shell = format!(
            "mkdir -p .leio-code/events && printf '%s\\n' '{{\"query_id\":\"{query_id}\"}}' > .leio-code/events/events.ndjson && printf '%s\\n' 'Evidence: {query_id}'"
        );
        let report = run_day(&receipt_day_spec(dir.path(), &repo, &shell), None)
            .await
            .unwrap();

        assert_eq!(report.passed, 1);
        assert_eq!(report.failed, 0);
        assert_eq!(report.outcomes[0].status, "passed");
        assert!(report.outcomes[0].receipts.as_ref().unwrap().ok);
        assert!(!Path::new(&report.outcomes[0].worktree).exists());
    }

    #[tokio::test]
    async fn receipt_gate_fails_a_lane_with_a_fabricated_query_id() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_fixture(dir.path());
        let query_id = "find_symbol-1788478894304497000";
        let shell = format!("printf '%s\\n' 'Evidence: {query_id}'");
        let report = run_day(&receipt_day_spec(dir.path(), &repo, &shell), None)
            .await
            .unwrap();

        assert_eq!(report.passed, 0);
        assert_eq!(report.failed, 1);
        assert_eq!(report.outcomes[0].status, "failed");
        let receipts = report.outcomes[0].receipts.as_ref().unwrap();
        assert_eq!(receipts.unresolved_query_ids, vec![query_id]);
        assert!(!receipts.ok);
        assert!(
            report.outcomes[0]
                .error
                .as_deref()
                .unwrap()
                .contains("receipt check failed")
        );
        assert!(!Path::new(&report.outcomes[0].worktree).exists());
    }
}
