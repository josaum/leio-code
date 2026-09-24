// The runtime lives in the `leio_harness` library so Rust hosts can embed
// it; this binary is the clap front end over those modules.
#[cfg(feature = "codeview")]
use leio_harness::codeview;
use leio_harness::{
    acp, agents, arrow_events, bus, bus_client, embed, gate, gepa, improvement, integrate, lease,
    merge, model, orchestrator, shm, sigreg, tui, worktree,
};

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use lease::LeaseStore;
use model::{AgentLease, AgentSessionSpec, RunSpec};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "leio-harness",
    version,
    about = "LEIO-Harness — Rust/Arrow harness runtime"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Stage, serve, verify and roll back immutable local static-site releases.
    Delivery {
        #[arg(long)]
        dir: PathBuf,
        #[arg(long, value_parser = ["prepare", "deploy", "rollback", "inspect", "verify", "serve"])]
        action: String,
        #[arg(long)]
        source: Option<PathBuf>,
        #[arg(long)]
        revision: Option<String>,
        #[arg(long)]
        approval: Option<String>,
        #[arg(long, default_value = "127.0.0.1:8788")]
        address: std::net::SocketAddr,
    },
    /// Durable intake, approval, execution and artifact reporting.
    Workflow {
        #[arg(long)]
        dir: PathBuf,
        #[arg(long, value_parser = ["init", "show", "revise", "evidence", "confirm", "approve", "execute", "reconcile", "notification-failed"])]
        action: String,
        #[arg(long)]
        input: Option<PathBuf>,
        #[arg(long)]
        approval: Option<String>,
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long)]
        deploy_enabled: bool,
    },
    Run {
        #[arg(long)]
        spec: PathBuf,
        /// Bus endpoint; defaults to an automatically managed durable local bus.
        #[arg(long, conflicts_with = "no_bus")]
        bus: Option<String>,
        /// Explicitly run offline without bus participation.
        #[arg(long)]
        no_bus: bool,
    },
    /// Supervised bidirectional ACP proxy: bridges JSON-RPC frames between
    /// this process's stdio and an agent server (e.g. `grok agent stdio`).
    Agent {
        #[arg(long)]
        spec: PathBuf,
    },
    Arrow {
        #[command(subcommand)]
        command: ArrowCommand,
    },
    Lease {
        #[arg(long)]
        store: PathBuf,
        #[command(subcommand)]
        command: LeaseCommand,
    },
    Worktree {
        #[command(subcommand)]
        command: WorktreeCommand,
    },
    /// Fail-closed merge of a lane branch into a target checkout. The
    /// improvement gate evaluates the integrated tree, never isolated lanes.
    Merge {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        worktree: PathBuf,
        #[arg(long)]
        branch: String,
        #[arg(long = "into")]
        target_ref: String,
    },
    /// Integrate lane branches into an integration branch and run an objective
    /// on the integrated tree (never isolated lanes).
    Integrate {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long = "worktree-root")]
        worktree_root: PathBuf,
        #[arg(long)]
        target: String,
        #[arg(long = "branch")]
        branches: Vec<String>,
        #[arg(long = "objective-arg")]
        objective_argv: Vec<String>,
        #[arg(long = "output-dir")]
        output_dir: Option<PathBuf>,
        #[arg(long = "timeout-ms", default_value_t = 300_000)]
        timeout_ms: u64,
    },
    /// Improvement gate: baseline vs integrated objective, promote only on
    /// Improved.
    Gate {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long = "worktree-root")]
        worktree_root: PathBuf,
        #[arg(long)]
        target: String,
        #[arg(long)]
        baseline: String,
        #[arg(long = "branch")]
        branches: Vec<String>,
        #[arg(long = "objective-arg")]
        objective_argv: Vec<String>,
        #[arg(long = "output-dir")]
        output_dir: PathBuf,
        #[arg(long = "timeout-ms", default_value_t = 300_000)]
        timeout_ms: u64,
        #[arg(long = "isotropy-threshold", default_value_t = 2.0)]
        isotropy_threshold: f32,
        #[arg(long = "promote", default_value_t = false)]
        promote: bool,
    },
    Bus {
        #[command(subcommand)]
        command: BusCommand,
    },
    /// Shared memory zero-copy inter-agent coordination ring buffer.
    Shm {
        #[command(subcommand)]
        command: ShmCommand,
    },
    #[cfg(feature = "codeview")]
    Codeview {
        #[command(subcommand)]
        command: CodeviewCommand,
    },
    Gepa {
        #[command(subcommand)]
        command: GepaCommand,
    },
    Sigreg {
        #[command(subcommand)]
        command: SigregCommand,
    },
    /// Baseline-vs-collab improvement gate (merges only when collab wins).
    Compare {
        #[arg(long)]
        baseline: String,
        #[arg(long)]
        collab: String,
        #[arg(long, default_value_t = 2.0)]
        isotropy_threshold: f32,
    },
    /// Embed text into a vector via the configured embeddings endpoint.
    Embed {
        #[arg(long)]
        text: String,
    },
    /// OpenRouter work-shape model table (live refresh / inspection).
    Models {
        #[command(subcommand)]
        action: ModelsCommand,
    },
    Day {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long)]
        bus: Option<String>,
        #[arg(long, conflicts_with = "bus")]
        no_bus: bool,
        /// Live ANSI progress view while lanes run.
        #[arg(long, default_value_t = false)]
        watch: bool,
    },
}

#[derive(Subcommand)]
enum SigregCommand {
    /// Sliced Epps-Pulley isotropy score of a JSON array of {vector:[...]}.
    Isotropy {
        #[arg(long)]
        rows: PathBuf,
        #[arg(long, default_value_t = 64)]
        slices: usize,
        #[arg(long, default_value_t = 0)]
        seed: u64,
    },
}

#[derive(Subcommand)]
enum GepaCommand {
    /// Spherical merge of two intent vectors: {"vector":[...]}
    Merge {
        #[arg(long)]
        a: PathBuf,
        #[arg(long)]
        b: PathBuf,
        #[arg(long, default_value_t = 0.5)]
        t: f32,
    },
    /// Trust-region mutation of an intent vector.
    Mutate {
        #[arg(long)]
        parent: PathBuf,
        #[arg(long, default_value_t = 0.1)]
        strength: f32,
        #[arg(long, default_value_t = 5.0)]
        max_angle_deg: f32,
        #[arg(long, default_value_t = 0)]
        seed: u64,
    },
    /// Pareto frontier of JSON candidates: [{id, vector, scores, evaluations}]
    Frontier {
        #[arg(long)]
        candidates: PathBuf,
    },
    /// Pareto evolution cycle: pull bus candidates, evolve, republish.
    Cycle {
        #[arg(long)]
        bus: String,
        #[arg(long)]
        lane: String,
        #[arg(long)]
        goal_text: String,
        #[arg(long, default_value_t = 3)]
        generations: usize,
        #[arg(long, default_value_t = 0.1)]
        strength: f32,
        #[arg(long, default_value_t = 5.0)]
        max_angle_deg: f32,
        #[arg(long, default_value_t = 1.0)]
        anchor_beta: f32,
        #[arg(long, default_value_t = 0)]
        seed: u64,
    },
    /// Semantic-anchor drift penalty: R_adj = R - beta * (1 - cos).
    Anchor {
        #[arg(long)]
        vector: PathBuf,
        #[arg(long)]
        seed: PathBuf,
        #[arg(long, default_value_t = 0.1)]
        beta: f32,
    },
}

#[derive(Subcommand)]
enum BusCommand {
    /// Inspect protocol, runtime version, process identity and persistence.
    Health {
        #[arg(long)]
        bus: String,
    },
    /// Serve the Arrow Flight semantic bus (agents exchange embeddings).
    Serve {
        #[arg(long, default_value = "127.0.0.1:8815")]
        bind: String,
        /// Durable Arrow IPC snapshot (survives restarts).
        #[arg(long)]
        persist: Option<PathBuf>,
    },
    /// In-process do_exchange round-trip check (publish + semantic match).
    Selftest {
        #[arg(long, default_value_t = 18815)]
        port: u16,
    },
    /// Publish embedding rows from a JSON file: [{agent_id,run_id,topic,vector}]
    Publish {
        #[arg(long)]
        bus: String,
        #[arg(long)]
        rows: PathBuf,
    },
    /// Semantic match: {"vector":[...],"topic":...?,"top_k":...?}
    Match {
        #[arg(long)]
        bus: String,
        #[arg(long)]
        query: PathBuf,
    },
    /// Embed text over the bus (Flight do_exchange) — inference on the lab GPU.
    Embed {
        #[arg(long)]
        bus: String,
        #[arg(long)]
        text: String,
    },
}

#[derive(Subcommand)]
enum ShmCommand {
    /// In-process round-trip check over shared memory ring buffer.
    Selftest {
        #[arg(long, default_value_t = 64)]
        capacity: usize,
    },
    /// Latency & throughput benchmark for shared memory SPSC push and pop.
    Bench {
        #[arg(long, default_value_t = 10_000)]
        iterations: usize,
        #[arg(long, default_value_t = 1536)]
        dimensions: usize,
    },
    /// Latency benchmark for Semantic Two-Phase Commit (S-2PC) consensus loop.
    ConsensusBench {
        #[arg(long, default_value_t = 10_000)]
        iterations: usize,
    },
}

#[derive(Subcommand)]
enum ModelsCommand {
    /// Fetch OpenRouter's public /models list and rank work-shape picks into
    /// ~/.config/leio-harness/openrouter-models.json (valid for one week).
    Refresh {
        /// Override the API base (defaults to https://openrouter.ai/api/v1).
        #[arg(long)]
        base_url: Option<String>,
    },
    /// Print the resolved work-shape slugs (cached when fresh, builtin
    /// snapshot otherwise).
    Show,
}

#[cfg(feature = "codeview")]
#[derive(Subcommand)]
enum CodeviewCommand {
    /// Index a repo in-process via leio-code and publish the fingerprint.
    Publish {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        bus: String,
        #[arg(long)]
        agent_id: String,
        #[arg(long)]
        run_id: String,
    },
    /// Staleness gate: fresh only when the stamped summary matches the index.
    Check {
        #[arg(long)]
        repo: PathBuf,
    },
}

#[derive(Subcommand)]
enum ArrowCommand {
    Inspect { path: PathBuf },
}

#[derive(Subcommand)]
enum LeaseCommand {
    List,
    Acquire {
        #[arg(long)]
        lease: PathBuf,
    },
    Heartbeat {
        #[arg(long)]
        run_id: String,
    },
    Release {
        #[arg(long)]
        run_id: String,
    },
    Expire {
        #[arg(long)]
        stale_after_ms: i64,
    },
}

#[derive(Subcommand)]
enum WorktreeCommand {
    Create {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        branch: String,
        #[arg(long)]
        base_ref: Option<String>,
    },
    Retire {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        path: PathBuf,
    },
}

fn main() {
    if let Err(error) = execute(Cli::parse()) {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

fn execute(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Delivery {
            dir,
            action,
            source,
            revision,
            approval,
            address,
        } => {
            use leio_harness::delivery;
            let result = match action.as_str() {
                "prepare" => {
                    delivery::prepare(source.as_deref().context("--source required")?, &dir)?
                }
                "deploy" => delivery::deploy(
                    &dir,
                    revision.as_deref().context("--revision required")?,
                    approval.as_deref().context("--approval required")?,
                    address,
                )?,
                "rollback" => delivery::rollback(
                    &dir,
                    approval.as_deref().context("--approval required")?,
                    address,
                )?,
                "inspect" => delivery::inspect(&dir)?,
                "verify" => delivery::verify(&dir, address)?,
                "serve" => {
                    delivery::serve(&dir, address)?;
                    return Ok(());
                }
                _ => unreachable!("clap validates delivery action"),
            };
            print_json(&result)?;
        }
        Command::Workflow {
            dir,
            action,
            input,
            approval,
            repo,
            deploy_enabled,
        } => {
            let result = leio_harness::workflow::apply(
                &dir,
                &action,
                input.as_deref(),
                approval.as_deref(),
                repo.as_deref(),
                deploy_enabled,
            )?;
            print_json(&result)?;
            if action == "execute" && matches!(result.state.as_str(), "failed" | "running") {
                anyhow::bail!(
                    "workflow step failed or needs reconciliation; inspect persisted history and process logs"
                );
            }
        }
        Command::Run { spec, bus, no_bus } => {
            let spec: RunSpec = serde_json::from_slice(
                &std::fs::read(&spec).with_context(|| format!("read {}", spec.display()))?,
            )?;
            let runtime = tokio::runtime::Runtime::new()?;
            let result = runtime.block_on(async {
                let addr = leio_harness::managed_bus::ensure(bus, no_bus).await?;
                leio_harness::managed_bus::run(spec, addr.as_deref()).await
            })?;
            print_json(&result)?;
            anyhow::ensure!(
                result.status == model::RunStatus::Passed,
                "run did not pass; see result.json"
            );
        }
        Command::Agent { spec } => {
            let spec: AgentSessionSpec = serde_json::from_slice(
                &std::fs::read(&spec).with_context(|| format!("read {}", spec.display()))?,
            )?;
            let result = acp::serve(spec)?;
            if result.status != model::RunStatus::Passed {
                std::process::exit(1);
            }
        }
        Command::Arrow { command } => match command {
            ArrowCommand::Inspect { path } => print_json(&arrow_events::inspect_mmap(&path)?)?,
        },
        Command::Lease { store, command } => {
            let store = LeaseStore::new(store);
            match command {
                LeaseCommand::List => print_json(&store.list()?)?,
                LeaseCommand::Acquire { lease } => {
                    let lease: AgentLease = serde_json::from_slice(&std::fs::read(&lease)?)?;
                    print_json(&store.acquire(lease)?)?;
                }
                LeaseCommand::Heartbeat { run_id } => {
                    print_json(&store.heartbeat(&run_id, Utc::now())?)?
                }
                LeaseCommand::Release { run_id } => {
                    print_json(&store.release(&run_id, Utc::now())?)?
                }
                LeaseCommand::Expire { stale_after_ms } => {
                    print_json(&store.expire_stale(Utc::now(), stale_after_ms)?)?
                }
            }
        }
        Command::Worktree { command } => match command {
            WorktreeCommand::Create {
                repo,
                root,
                path,
                branch,
                base_ref,
            } => print_json(&worktree::create(
                &repo,
                &root,
                &path,
                &branch,
                base_ref.as_deref(),
            )?)?,
            WorktreeCommand::Retire { repo, root, path } => {
                print_json(&worktree::retire(&repo, &root, &path)?)?
            }
        },
        Command::Merge {
            repo,
            worktree,
            branch,
            target_ref,
        } => print_json(&merge::merge_branch(
            &repo,
            &worktree,
            &branch,
            &target_ref,
        )?)?,
        Command::Integrate {
            repo,
            worktree_root,
            target,
            branches,
            objective_argv,
            output_dir,
            timeout_ms,
        } => {
            let objective = if objective_argv.is_empty() {
                None
            } else {
                Some(integrate::ObjectiveSpec {
                    argv: objective_argv,
                    output_dir: output_dir
                        .unwrap_or_else(|| worktree_root.join("objective"))
                        .display()
                        .to_string(),
                    timeout_ms,
                })
            };
            let report = integrate::integrate_lanes(&integrate::IntegrateSpec {
                repo: repo.display().to_string(),
                worktree_root: worktree_root.display().to_string(),
                target_ref: target,
                branches,
                objective,
            })?;
            print_json(&report)?;
        }
        Command::Gate {
            repo,
            worktree_root,
            target,
            baseline,
            branches,
            objective_argv,
            output_dir,
            timeout_ms,
            isotropy_threshold,
            promote,
        } => {
            let report = gate::run_gate(&gate::GateSpec {
                repo: repo.display().to_string(),
                worktree_root: worktree_root.display().to_string(),
                target_ref: target,
                baseline_ref: baseline,
                branches,
                objective: integrate::ObjectiveSpec {
                    argv: objective_argv,
                    output_dir: output_dir.display().to_string(),
                    timeout_ms,
                },
                isotropy_threshold,
                promote,
            })?;
            print_json(&report)?;
            if report.verdict != improvement::ImprovementVerdict::Improved {
                std::process::exit(1);
            }
        }
        Command::Bus { command } => match command {
            BusCommand::Health { bus: addr } => {
                let runtime = tokio::runtime::Runtime::new()?;
                let health = runtime.block_on(async {
                    bus_client::BusClient::connect(&addr).await?.health().await
                })?;
                print_json(&health)?;
            }
            BusCommand::Serve { bind, persist } => {
                let runtime = tokio::runtime::Runtime::new()?;
                runtime.block_on(bus::serve_auto(&bind, persist))?;
            }
            BusCommand::Selftest { port } => {
                let runtime = tokio::runtime::Runtime::new()?;
                let result = runtime.block_on(bus::selftest(port))?;
                print_json(&result)?;
            }
            BusCommand::Publish { bus: addr, rows } => {
                let rows: Vec<bus::EmbeddingRow> = serde_json::from_slice(&std::fs::read(&rows)?)?;
                let runtime = tokio::runtime::Runtime::new()?;
                let last = runtime.block_on(async {
                    bus_client::BusClient::connect_with_retry(
                        &addr,
                        10,
                        std::time::Duration::from_millis(100),
                    )
                    .await?
                    .publish(rows)
                    .await
                })?;
                print_json(&serde_json::json!({ "last_seq": last }))?;
            }
            BusCommand::Embed { bus: addr, text } => {
                let runtime = tokio::runtime::Runtime::new()?;
                let vector = runtime.block_on(async {
                    let mut client = bus_client::BusClient::connect_with_retry(
                        &addr,
                        10,
                        std::time::Duration::from_millis(100),
                    )
                    .await?;
                    client
                        .embed(vec![text])
                        .await?
                        .into_iter()
                        .next()
                        .context("no embedding returned")
                })?;
                print_json(&vector)?;
            }
            BusCommand::Match { bus: addr, query } => {
                let query: bus::MatchRequest = serde_json::from_slice(&std::fs::read(&query)?)?;
                let runtime = tokio::runtime::Runtime::new()?;
                let hits = runtime.block_on(async {
                    bus_client::BusClient::connect_with_retry(
                        &addr,
                        10,
                        std::time::Duration::from_millis(100),
                    )
                    .await?
                    .match_query(query)
                    .await
                })?;
                print_json(&hits)?;
            }
        },
        Command::Shm { command } => match command {
            ShmCommand::Selftest { capacity } => {
                let result = shm::selftest(capacity)?;
                print_json(&result)?;
            }
            ShmCommand::Bench {
                iterations,
                dimensions,
            } => {
                let result = shm::bench(iterations, dimensions)?;
                print_json(&result)?;
            }
            ShmCommand::ConsensusBench { iterations } => {
                let result = shm::consensus_bench(iterations)?;
                print_json(&result)?;
            }
        },
        Command::Models { action } => match action {
            ModelsCommand::Refresh { base_url } => {
                let runtime = tokio::runtime::Runtime::new()?;
                let cache = agents::models_cache_path()
                    .context("HOME is not set; cannot locate the models cache")?;
                let table =
                    runtime.block_on(agents::refresh_models_table(&cache, base_url.as_deref()))?;
                print_json(&table)?;
            }
            ModelsCommand::Show => {
                let table = agents::cached_table();
                print_json(&serde_json::json!({
                    "source": table.as_ref().map(|t| t.source.as_str()).unwrap_or("builtin"),
                    "volume": agents::openrouter_slug(
                        crate::agents::WorkShape::Explorer, table.as_ref()),
                    "verifier": agents::openrouter_slug(
                        crate::agents::WorkShape::Verifier, table.as_ref()),
                    "quality": agents::openrouter_slug(
                        crate::agents::WorkShape::Reviewer, table.as_ref()),
                }))?;
            }
        },
        Command::Embed { text } => {
            let runtime = tokio::runtime::Runtime::new()?;
            let vectors = runtime.block_on(async {
                let embed = embed::EmbedClient::from_env()?;
                embed.embed(std::slice::from_ref(&text)).await
            })?;
            print_json(
                &vectors
                    .into_iter()
                    .next()
                    .context("no embedding returned")?,
            )?;
        }
        Command::Day {
            spec,
            bus: addr,
            no_bus,
            watch,
        } => {
            let spec: orchestrator::DaySpec = serde_json::from_slice(&std::fs::read(&spec)?)?;
            let runtime = tokio::runtime::Runtime::new()?;
            let addr = runtime.block_on(leio_harness::managed_bus::ensure(addr, no_bus))?;
            let report = if watch {
                let (tx, rx) = std::sync::mpsc::channel::<orchestrator::DayEvent>();
                let goal = spec.goal.clone();
                let lanes: Vec<String> = spec
                    .lanes
                    .iter()
                    .map(|lane| lane.agent_id.clone())
                    .collect();
                let render = std::thread::spawn(move || {
                    let mut frames: std::collections::BTreeMap<String, tui::LaneFrame> = lanes
                        .iter()
                        .map(|agent| {
                            (
                                agent.clone(),
                                tui::LaneFrame {
                                    agent_id: agent.clone(),
                                    branch: "…".to_owned(),
                                    visual: tui::LaneVisual::Queued,
                                    elapsed_ms: 0,
                                    note: None,
                                },
                            )
                        })
                        .collect();
                    let mut starts: std::collections::BTreeMap<String, std::time::Instant> =
                        std::collections::BTreeMap::new();
                    let mut view = tui::TerminalView::new();
                    loop {
                        let mut disconnected = false;
                        loop {
                            let event = match rx.try_recv() {
                                Ok(event) => event,
                                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                    disconnected = true;
                                    break;
                                }
                            };
                            match event {
                                orchestrator::DayEvent::LaneStarted {
                                    agent_id,
                                    run_id,
                                    branch,
                                } => {
                                    starts.insert(run_id, std::time::Instant::now());
                                    if let Some(frame) = frames.get_mut(&agent_id) {
                                        frame.branch = branch;
                                        frame.visual = tui::LaneVisual::Running;
                                    }
                                }
                                orchestrator::DayEvent::LaneFinished {
                                    run_id,
                                    status,
                                    duration_ms,
                                    error,
                                } => {
                                    if let Some(start) = starts.remove(&run_id) {
                                        let _ = start;
                                    }
                                    for frame in frames.values_mut() {
                                        if frame.visual == tui::LaneVisual::Running
                                            && run_id.starts_with(&frame.agent_id)
                                        {
                                            frame.visual = match status.as_str() {
                                                "passed" => tui::LaneVisual::Passed,
                                                "failed" => tui::LaneVisual::Failed,
                                                "timed_out" => tui::LaneVisual::TimedOut,
                                                "canceled" => tui::LaneVisual::Canceled,
                                                _ => tui::LaneVisual::InfraError,
                                            };
                                            frame.elapsed_ms = duration_ms;
                                            frame.note = error.clone();
                                        }
                                    }
                                }
                            }
                        }
                        let rows: Vec<tui::LaneFrame> = frames.values().cloned().collect();
                        let all_done = rows.iter().all(|frame| {
                            !matches!(
                                frame.visual,
                                tui::LaneVisual::Queued | tui::LaneVisual::Running
                            )
                        });
                        if all_done || disconnected {
                            view.finish(&goal, &rows);
                            break;
                        }
                        view.draw(&goal, &rows);
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                });
                let result = runtime.block_on(orchestrator::run_day_with_events(
                    &spec,
                    addr.as_deref(),
                    Some(tx),
                ));
                let _ = render.join();
                result?
            } else {
                runtime.block_on(orchestrator::run_day(&spec, addr.as_deref()))?
            };
            print_json(&report)?;
            anyhow::ensure!(report.failed == 0, "day has failed lanes; inspect report");
        }
        Command::Compare {
            baseline,
            collab,
            isotropy_threshold,
        } => {
            let baseline = improvement::parse_snapshot(&baseline)?;
            let collab = improvement::parse_snapshot(&collab)?;
            let report = improvement::evaluate_improvement(&baseline, &collab, isotropy_threshold)?;
            print_json(&report)?;
            if report.verdict != improvement::ImprovementVerdict::Improved {
                std::process::exit(1);
            }
        }
        Command::Sigreg { command } => match command {
            SigregCommand::Isotropy { rows, slices, seed } => {
                let raw: Vec<serde_json::Value> = serde_json::from_slice(&std::fs::read(&rows)?)?;
                let vectors: Vec<Vec<f32>> = raw
                    .iter()
                    .filter_map(|row| {
                        row["vector"].as_array().map(|v| {
                            v.iter()
                                .filter_map(|x| x.as_f64().map(|x| x as f32))
                                .collect()
                        })
                    })
                    .collect();
                let score = sigreg::sphere_isotropy_score(&vectors, slices, seed);
                print_json(&serde_json::json!({
                    "score": score,
                    "threshold": sigreg::DEFAULT_ISOTROPY_THRESHOLD,
                    "isotropic": score <= sigreg::DEFAULT_ISOTROPY_THRESHOLD,
                    "population": vectors.len(),
                }))?;
            }
        },
        Command::Gepa { command } => {
            match command {
                GepaCommand::Merge { a, b, t } => {
                    let a: serde_json::Value = serde_json::from_slice(&std::fs::read(&a)?)?;
                    let b: serde_json::Value = serde_json::from_slice(&std::fs::read(&b)?)?;
                    let va = json_vector(&a)?;
                    let vb = json_vector(&b)?;
                    print_json(&serde_json::json!({ "vector": gepa::slerp(&va, &vb, t) }))?;
                }
                GepaCommand::Mutate {
                    parent,
                    strength,
                    max_angle_deg,
                    seed,
                } => {
                    let parent: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(&parent)?)?;
                    let vector = json_vector(&parent)?;
                    let mut rng = gepa::SplitMix64::new(seed);
                    print_json(&serde_json::json!({
                        "vector": gepa::trust_region_mutate(&vector, strength, max_angle_deg, &mut rng)
                    }))?;
                }
                GepaCommand::Cycle {
                    bus: addr,
                    lane,
                    goal_text,
                    generations,
                    strength,
                    max_angle_deg,
                    anchor_beta,
                    seed,
                } => {
                    let runtime = tokio::runtime::Runtime::new()?;
                    let report = runtime.block_on(async {
                    let mut client = bus_client::BusClient::connect_with_retry(
                        &addr,
                        10,
                        std::time::Duration::from_millis(100),
                    )
                    .await?;
                    let goal = client.embed(vec![goal_text]).await?.remove(0);
                    let mut offsprings = Vec::new();
                    let mut effective_strength = strength;
                    let mut trajectory = Vec::new();
                    for generation in 0..generations {
                        let intent_topic = format!("intent/{lane}");
                        let result_topic = format!("result/{lane}");
                        let mut rows = client.list(Some(&intent_topic)).await?;
                        rows.extend(client.list(Some(&result_topic)).await?);
                        rows.sort_by_key(|row| row.seq);
                        rows.dedup_by_key(|row| row.seq);
                        let Some(parent_seq) =
                            select_ranked_parent(&rows, &goal, anchor_beta).map(|row| row.seq)
                        else {
                            anyhow::bail!("lane has no candidates to evolve");
                        };
                        let result = client
                            .evolve_from(
                                &lane,
                                parent_seq,
                                goal.clone(),
                                bus_client::EvolveOptions {
                                    strength: effective_strength,
                                    max_angle_deg,
                                    anchor_beta,
                                    seed: seed.wrapping_add(generation as u64),
                                },
                            )
                            .await?;
                        let server_parent_seq = result["parent_seq"]
                            .as_u64()
                            .context("bus evolve response missing parent_seq")?;
                        anyhow::ensure!(
                            server_parent_seq == parent_seq,
                            "bus evolved parent_seq {server_parent_seq}, expected ranked parent_seq {parent_seq}"
                        );
                        let offspring_seq = result["offspring_seq"]
                            .as_u64()
                            .context("bus evolve response missing offspring_seq")?;
                        let offspring_penalty = result["anchor_penalty"].as_f64().unwrap_or(f64::NAN);
                        offsprings.push(serde_json::json!({
                            "generation": generation,
                            "parent_seq": server_parent_seq,
                            "offspring_seq": offspring_seq,
                            "anchor_penalty": offspring_penalty,
                            "strength": effective_strength,
                        }));
                        // SIGReg collapse guard: recompute population isotropy
                        // after this generation and widen exploration if the
                        // population is directionally collapsed.
                        let refreshed = {
                            let mut r = client.list(Some(&intent_topic)).await?;
                            r.extend(client.list(Some(&result_topic)).await?);
                            r
                        };
                        let vectors: Vec<Vec<f32>> =
                            refreshed.iter().map(|row| row.vector.clone()).collect();
                        let isotropy = sigreg::sphere_isotropy_score(&vectors, 64, seed.wrapping_add(generation as u64));
                        let collapsed = isotropy > sigreg::DEFAULT_ISOTROPY_THRESHOLD;
                        if collapsed {
                            effective_strength = (effective_strength * 1.5).min(1.0);
                        }
                        trajectory.push(serde_json::json!({
                            "generation": generation,
                            "isotropy_score": isotropy,
                            "collapsed": collapsed,
                            "effective_strength": effective_strength,
                        }));
                    }
                    let final_rows = {
                        let mut r = client.list(Some(&format!("intent/{lane}"))).await?;
                        r.extend(client.list(Some(&format!("result/{lane}"))).await?);
                        r
                    };
                    // Multi-objective frontier: goal alignment × population
                    // diversity (1 - mean cosine similarity to peers).
                    let mut frontier = gepa::ParetoFrontier::default();
                    for row in &final_rows {
                        let alignment = 1.0 - gepa::anchor_penalty(&row.vector, &goal, anchor_beta);
                        let diversity = 1.0 - mean_abs_cosine(&row.vector, &final_rows);
                        frontier.add(gepa::Candidate {
                            id: format!("{}@{}", row.agent_id, row.seq),
                            vector: row.vector.clone(),
                            scores: [
                                ("alignment".to_owned(), alignment),
                                ("diversity".to_owned(), diversity),
                            ]
                            .into_iter()
                            .collect(),
                            evaluations: 0,
                        });
                    }
                    let vectors: Vec<Vec<f32>> =
                        final_rows.iter().map(|row| row.vector.clone()).collect();
                    let isotropy = sigreg::sphere_isotropy_score(&vectors, 64, seed);
                    Ok::<_, anyhow::Error>(serde_json::json!({
                        "lane": lane,
                        "generations": generations,
                        "offsprings": offsprings,
                        "trajectory": trajectory,
                        "frontier": frontier.candidates().iter().map(|c| &c.id).collect::<Vec<_>>(),
                        "best": frontier.select_best_aggregate().map(|c| &c.id),
                        "sigreg": {
                            "isotropy_score": isotropy,
                            "threshold": sigreg::DEFAULT_ISOTROPY_THRESHOLD,
                            "isotropic": isotropy <= sigreg::DEFAULT_ISOTROPY_THRESHOLD,
                        },
                    }))
                })?;
                    print_json(&report)?;
                }
                GepaCommand::Frontier { candidates } => {
                    let candidates: Vec<gepa::Candidate> =
                        serde_json::from_slice(&std::fs::read(&candidates)?)?;
                    let mut frontier = gepa::ParetoFrontier::default();
                    let mut accepted = Vec::new();
                    for candidate in candidates {
                        if frontier.add(candidate.clone()) {
                            accepted.push(candidate.id);
                        }
                    }
                    print_json(&serde_json::json!({
                        "frontier": frontier.candidates().iter().map(|c| &c.id).collect::<Vec<_>>(),
                        "accepted": accepted,
                        "least_explored": frontier.select_least_explored().map(|c| &c.id),
                        "best_aggregate": frontier.select_best_aggregate().map(|c| &c.id),
                    }))?;
                }
                GepaCommand::Anchor { vector, seed, beta } => {
                    let vector: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(&vector)?)?;
                    let seed: serde_json::Value = serde_json::from_slice(&std::fs::read(&seed)?)?;
                    let penalty =
                        gepa::anchor_penalty(&json_vector(&vector)?, &json_vector(&seed)?, beta);
                    print_json(&serde_json::json!({ "anchor_penalty": penalty }))?;
                }
            }
        }
        #[cfg(feature = "codeview")]
        Command::Codeview { command } => match command {
            CodeviewCommand::Publish {
                repo,
                bus: addr,
                agent_id,
                run_id,
            } => {
                let runtime = tokio::runtime::Runtime::new()?;
                let publication = runtime.block_on(async {
                    let embed = embed::EmbedClient::from_env().ok();
                    codeview::publish_code_view(&repo, &addr, &agent_id, &run_id, embed.as_ref())
                        .await
                })?;
                print_json(&publication)?;
            }
            CodeviewCommand::Check { repo } => {
                print_json(&codeview::check_code_view(&repo))?;
            }
        },
    }
    Ok(())
}

fn json_vector(value: &serde_json::Value) -> Result<Vec<f32>> {
    let array = value["vector"]
        .as_array()
        .context("expected {\"vector\": [..]}")?;
    array
        .iter()
        .map(|item| {
            item.as_f64()
                .map(|v| v as f32)
                .context("vector entries must be numbers")
        })
        .collect()
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn select_ranked_parent<'a>(
    rows: &'a [bus::EmbeddingRow],
    goal: &[f32],
    anchor_beta: f32,
) -> Option<&'a bus::EmbeddingRow> {
    rows.iter()
        .filter(|row| row.vector.len() == goal.len())
        .max_by(|left, right| {
            let left_score = 1.0 - gepa::anchor_penalty(&left.vector, goal, anchor_beta);
            let right_score = 1.0 - gepa::anchor_penalty(&right.vector, goal, anchor_beta);
            left_score
                .total_cmp(&right_score)
                .then_with(|| left.seq.cmp(&right.seq))
        })
}

/// Mean absolute cosine similarity between `v` and every row (excluding a
/// zero-norm self match), as a collapse/diversity proxy in [0,1].
fn mean_abs_cosine(v: &[f32], rows: &[bus::EmbeddingRow]) -> f32 {
    let mut norm_v = v.to_vec();
    crate::gepa::normalize(&mut norm_v);
    let mut sum = 0.0_f32;
    let mut count = 0_u32;
    for row in rows {
        let mut other = row.vector.clone();
        crate::gepa::normalize(&mut other);
        let sim = crate::gepa::dot(&norm_v, &other).abs();
        sum += sim;
        count += 1;
    }
    if count == 0 { 0.0 } else { sum / count as f32 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(seq: u64, vector: Vec<f32>) -> bus::EmbeddingRow {
        bus::EmbeddingRow {
            seq,
            timestamp_ms: 0,
            agent_id: "agent".to_owned(),
            run_id: "run".to_owned(),
            topic: "intent/lane".to_owned(),
            vector,
        }
    }

    #[test]
    fn ranked_parent_uses_numeric_score_and_matching_dimension() {
        let rows = vec![
            row(1, vec![-1.0, 0.0]),
            row(2, vec![1.0, 0.0]),
            row(3, vec![1.0, 0.0, 0.0]),
        ];

        let selected = select_ranked_parent(&rows, &[1.0, 0.0], 2.0).unwrap();

        assert_eq!(selected.seq, 2);
    }
}
