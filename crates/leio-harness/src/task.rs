//! Explicit, durable coding tasks. No bus, embedding or ambient-agent dependency.
use crate::{
    model::{RunResult, RunSpec, RunStatus},
    process::{self, RunObserver},
};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[cfg(unix)]
use std::os::unix::{
    ffi::OsStrExt,
    fs::{DirBuilderExt, OpenOptionsExt},
    process::CommandExt,
};
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
#[cfg(unix)]
extern "C" fn interrupt(_: i32) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSpec {
    pub run_id: String,
    pub repo_root: String,
    pub task: String,
    pub worker: String,
    pub argv: Vec<String>,
    pub verify_argv: Vec<String>,
    pub constraints: Vec<String>,
    pub timeout_ms: u64,
    pub submitted_at_ms: u64,
    pub baseline_revision: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSnapshot {
    pub version: u32,
    pub run_id: String,
    pub repo_root: String,
    pub task: String,
    pub worker: String,
    pub state: String,
    pub phase: String,
    pub supervisor_pid: Option<u32>,
    pub worker_pid: Option<u32>,
    pub submitted_at_ms: u64,
    pub started_at_ms: Option<u64>,
    pub completed_at_ms: Option<u64>,
    pub last_activity_at_ms: u64,
    pub last_output_at_ms: Option<u64>,
    pub last_activity: String,
    pub error: Option<String>,
    pub sequence: u64,
    pub run_dir: String,
    pub worker_result: Option<RunResult>,
    pub verification_result: Option<RunResult>,
    pub changed_paths: Vec<String>,
    pub diff_path: Option<String>,
    pub diff_truncated: bool,
    pub untracked_artifacts: Vec<String>,
    pub untracked_truncated: bool,
}
fn terminal(state: &str) -> bool {
    matches!(state, "blocked" | "failed" | "cancelled" | "succeeded")
}
fn root(repo: &Path) -> Result<PathBuf> {
    ensure!(
        repo.is_absolute() && repo.is_dir(),
        "repo must be an absolute existing directory"
    );
    Ok(crate::process::clean_canonicalize(repo)?)
}
fn runs(repo: &Path) -> PathBuf {
    repo.join(".leio-code/harness/runs")
}
fn directory(repo: &Path, id: &str) -> Result<PathBuf> {
    ensure!(
        !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "invalid run_id"
    );
    let dir = runs(repo).join(id);
    ensure!(
        std::fs::symlink_metadata(&dir)?.file_type().is_dir(),
        "run directory must not be a symlink"
    );
    Ok(dir)
}
fn lock(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    Ok(options.open(path)?)
}
fn locked(path: &Path) -> Result<bool> {
    let file = lock(path)?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
        Err(e) => Err(e.into()),
    }
}
fn private_directory(path: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(path)?;
    ensure!(
        std::fs::symlink_metadata(path)?.file_type().is_dir(),
        "sidecar directory must not be a symlink"
    );
    Ok(())
}
fn read_snapshot(dir: &Path) -> Result<TaskSnapshot> {
    Ok(serde_json::from_slice(&std::fs::read(
        dir.join("status.json"),
    )?)?)
}

struct Tracker {
    dir: PathBuf,
    state: Mutex<TaskSnapshot>,
}
impl Tracker {
    fn record(
        &self,
        kind: &str,
        detail: &str,
        change: impl FnOnce(&mut TaskSnapshot),
    ) -> Result<()> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("task tracker poisoned"))?;
        change(&mut s);
        s.sequence += 1;
        s.last_activity_at_ms = now();
        s.last_activity = detail.to_owned();
        let event = json!({"seq":s.sequence,"timestampMs":s.last_activity_at_ms,"kind":kind,"phase":s.phase,"state":s.state,"workerPid":s.worker_pid,"detail":detail});
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(self.dir.join("events.ndjson"))?;
        serde_json::to_writer(&mut file, &event)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        process::write_json_atomic(&self.dir.join("status.json"), &*s)
    }
    fn finish(&self, state: &str, error: Option<String>) -> Result<()> {
        let detail = error.clone().unwrap_or_else(|| state.to_owned());
        self.record("completed", &detail, |s| {
            s.state = state.to_owned();
            s.phase = "completed".to_owned();
            s.worker_pid = None;
            s.completed_at_ms = Some(now());
            s.error = error;
        })
    }
}
impl RunObserver for Tracker {
    fn spawned(&self, pid: u32) -> Result<()> {
        self.record("spawned", "worker process spawned", |s| {
            s.worker_pid = Some(pid);
        })
    }
    fn output(&self, stream: &str, bytes: usize) -> Result<()> {
        self.record(
            "output",
            &format!("{stream}: {bytes} observed bytes; see phase log"),
            |s| {
                s.last_output_at_ms = Some(now());
            },
        )
    }
    fn canceled(&self) -> bool {
        INTERRUPTED.load(Ordering::SeqCst) || self.dir.join("cancel.request").exists()
    }
}

pub fn submit(
    repo: &Path,
    task: String,
    worker: String,
    argv: Vec<String>,
    verify_argv: Vec<String>,
    constraints: Vec<String>,
    timeout_ms: u64,
) -> Result<Value> {
    let repo = root(repo)?;
    ensure!(
        !task.trim().is_empty() && !worker.trim().is_empty(),
        "task and worker are required"
    );
    ensure!(
        task.len() <= 16_384
            && worker.len() <= 256
            && constraints.len() <= 64
            && constraints.iter().all(|s| s.len() <= 4096),
        "task metadata exceeds input bounds"
    );
    for a in [&argv, &verify_argv] {
        ensure!(
            !a.is_empty() && !a[0].trim().is_empty(),
            "worker and verification argv must be nonempty"
        );
    }
    ensure!(
        timeout_ms > 0 && timeout_ms <= 86_400_000,
        "timeout must be between 1ms and 24 hours"
    );
    // A subdirectory has the same Git checkout but a different sidecar/lock.
    // Reject it before creating a record or spawning a worker, so ownership and
    // change capture always cover the complete checkout. Existing records remain
    // inspectable through their original paths.
    let top = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&repo)
        .output()
        .context("resolve Git checkout root")?;
    ensure!(top.status.success(), "task requires a Git repository");
    #[cfg(unix)]
    let checkout = {
        let top_bytes = top.stdout.strip_suffix(b"\n").unwrap_or(&top.stdout);
        crate::process::clean_canonicalize(Path::new(std::ffi::OsStr::from_bytes(top_bytes)))?
    };
    #[cfg(not(unix))]
    let checkout =
        crate::process::clean_canonicalize(Path::new(std::str::from_utf8(&top.stdout)?.trim()))?;
    ensure!(
        repo == checkout,
        "task repo must be the Git checkout root ({}); constrain module scope in the task instead of submitting a subdirectory",
        checkout.display()
    );
    let git = Command::new("git")
        .args(["status", "--porcelain", "--", ".", ":(exclude).leio-code"])
        .current_dir(&repo)
        .output()
        .context("inspect repository baseline")?;
    ensure!(git.status.success(), "task requires a Git repository");
    ensure!(
        git.stdout.is_empty(),
        "task requires a clean checkout; use an explicit clean worktree"
    );
    let baseline = Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(&repo)
        .output()?;
    ensure!(
        baseline.status.success(),
        "task requires a committed Git baseline"
    );
    private_directory(&repo.join(".leio-code"))?;
    private_directory(&repo.join(".leio-code/harness"))?;
    private_directory(&runs(&repo))?;
    let id = process::unique_id();
    let dir = runs(&repo).join(&id);
    #[cfg(unix)]
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    #[cfg(not(unix))]
    std::fs::DirBuilder::new().create(&dir)?;
    let spec = TaskSpec {
        run_id: id.clone(),
        repo_root: repo.display().to_string(),
        task: task.clone(),
        worker: worker.clone(),
        argv,
        verify_argv,
        constraints,
        timeout_ms,
        submitted_at_ms: now(),
        baseline_revision: String::from_utf8(baseline.stdout)?.trim().to_owned(),
    };
    process::write_json_atomic(&dir.join("spec.json"), &spec)?;
    let tracker = Tracker {
        dir: dir.clone(),
        state: Mutex::new(TaskSnapshot {
            version: 1,
            run_id: id.clone(),
            repo_root: spec.repo_root.clone(),
            task,
            worker,
            state: "queued".to_owned(),
            phase: "queued".to_owned(),
            supervisor_pid: None,
            worker_pid: None,
            submitted_at_ms: spec.submitted_at_ms,
            started_at_ms: None,
            completed_at_ms: None,
            last_activity_at_ms: now(),
            last_output_at_ms: None,
            last_activity: "submitted".to_owned(),
            error: None,
            sequence: 0,
            run_dir: dir.display().to_string(),
            worker_result: None,
            verification_result: None,
            changed_paths: vec![],
            diff_path: None,
            diff_truncated: false,
            untracked_artifacts: vec![],
            untracked_truncated: false,
        }),
    };
    tracker.record("submitted", "explicit task submitted", |_| {})?;
    let mut log_options = OpenOptions::new();
    log_options.create_new(true).write(true);
    #[cfg(unix)]
    log_options.mode(0o600);
    let log = log_options.open(dir.join("supervisor.log"))?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["task", "execute", "--repo"])
        .arg(&repo)
        .args(["--run-id", &id])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    // Detach from the calling coding host's process group. Only the task
    // supervisor owns worker cancellation, never the MCP request lifetime.
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            nix::unistd::setsid().map_err(std::io::Error::from)?;
            Ok(())
        });
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    match command.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => {
            tracker.finish("failed", Some(format!("supervisor spawn failed: {e}")))?;
        }
    }
    // Small startup grace gives callers ownership or a persisted startup error.
    for _ in 0..40 {
        let s = read_snapshot(&dir)?;
        if terminal(&s.state) || (s.state == "running" && s.worker_pid.is_some()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    inspect(&repo, &id)
}

fn run_spec(spec: &TaskSpec, phase: &str, argv: Vec<String>, dir: &Path) -> RunSpec {
    RunSpec {
        run_id: phase.to_owned(),
        argv,
        cwd: spec.repo_root.clone(),
        output_dir: dir.display().to_string(),
        timeout_ms: spec.timeout_ms,
        kill_grace_ms: 200,
        max_output_bytes: 4 * 1024 * 1024,
        env_allowlist: vec![],
        env: [
            ("LEIO_TASK".to_owned(), spec.task.clone()),
            (
                "LEIO_TASK_CONSTRAINTS".to_owned(),
                serde_json::to_string(&spec.constraints).unwrap_or_default(),
            ),
            (
                "LEIO_TASK_SPEC".to_owned(),
                dir.join("spec.json").display().to_string(),
            ),
        ]
        .into_iter()
        .collect(),
        required_approval_token: None,
        approval_token: None,
    }
}
fn outcome(result: &RunResult) -> (&'static str, Option<String>) {
    match result.status {
        RunStatus::Passed => ("succeeded", None),
        RunStatus::Canceled => ("cancelled", result.error.clone()),
        _ => (
            "failed",
            Some(result.error.clone().unwrap_or_else(|| {
                let mut stderr = String::new();
                if let Ok(mut file) = File::open(&result.stderr_path) {
                    let size = file.metadata().map(|m| m.len()).unwrap_or_default();
                    let _ = file.seek(SeekFrom::Start(size.saturating_sub(8192)));
                    let _ = file.read_to_string(&mut stderr);
                }
                if stderr.contains("HTTP 401") || stderr.contains("401 Unauthorized") {
                    return "authentication rejected (HTTP 401); inspect stderr.log".to_owned();
                }
                format!(
                    "process {:?}; exit code {:?}; inspect stderr.log",
                    result.status, result.exit_code
                )
            })),
        ),
    }
}

pub fn execute(repo: &Path, id: &str) -> Result<()> {
    let repo = root(repo)?;
    let dir = directory(&repo, id)?;
    let owner = lock(&dir.join("owner.lock"))?;
    owner
        .try_lock_exclusive()
        .context("task already supervised")?;
    let s = read_snapshot(&dir)?;
    ensure!(s.state == "queued", "task may only execute once");
    let spec: TaskSpec = serde_json::from_slice(&std::fs::read(dir.join("spec.json"))?)?;
    ensure!(
        spec.repo_root == repo.display().to_string() && spec.run_id == id,
        "task repository identity mismatch"
    );
    let tracker = Arc::new(Tracker {
        dir: dir.clone(),
        state: Mutex::new(s),
    });
    INTERRUPTED.store(false, Ordering::SeqCst);
    #[cfg(unix)]
    {
        let signal = nix::sys::signal::SigAction::new(
            nix::sys::signal::SigHandler::Handler(interrupt),
            nix::sys::signal::SaFlags::empty(),
            nix::sys::signal::SigSet::empty(),
        );
        unsafe {
            nix::sys::signal::sigaction(nix::sys::signal::Signal::SIGTERM, &signal)?;
            nix::sys::signal::sigaction(nix::sys::signal::Signal::SIGINT, &signal)?;
        }
    }
    if tracker.canceled() {
        return tracker.finish("cancelled", Some("cancelled before dispatch".to_owned()));
    }
    let repo_lock = lock(&repo.join(".leio-code/harness/repository.lock"))?;
    if let Err(e) = repo_lock.try_lock_exclusive() {
        return tracker.finish(
            "blocked",
            Some(format!("repository already owned by another task: {e}")),
        );
    }
    let baseline = Command::new("git")
        .args(["status", "--porcelain", "--", ".", ":(exclude).leio-code"])
        .current_dir(&repo)
        .output()?;
    if !baseline.status.success() || !baseline.stdout.is_empty() {
        return tracker.finish(
            "blocked",
            Some("checkout changed between submission and dispatch".to_owned()),
        );
    }
    tracker.record("dispatched", "supervisor owns task and repository", |s| {
        s.state = "running".to_owned();
        s.phase = "worker".to_owned();
        s.supervisor_pid = Some(std::process::id());
        s.started_at_ms = Some(now());
    })?;
    let work = (|| -> Result<(String, Option<String>)> {
        let result = process::run_observed(
            run_spec(&spec, "worker", spec.argv.clone(), &dir),
            Some(tracker.clone()),
        )?;
        let (state, error) = outcome(&result);
        tracker.record("worker_exited", "worker process exited", |s| {
            s.worker_pid = None;
            s.worker_result = Some(result);
        })?;
        if state != "succeeded" {
            return Ok((state.to_owned(), error));
        }
        if tracker.canceled() {
            return Ok((
                "cancelled".to_owned(),
                Some("cancelled before verification".to_owned()),
            ));
        }
        tracker.record(
            "verification_started",
            "running required verification command",
            |s| {
                s.phase = "verification".to_owned();
            },
        )?;
        let result = process::run_observed(
            run_spec(&spec, "verification", spec.verify_argv.clone(), &dir),
            Some(tracker.clone()),
        )?;
        let (state, error) = outcome(&result);
        tracker.record("verification_exited", "verification process exited", |s| {
            s.worker_pid = None;
            s.verification_result = Some(result);
        })?;
        Ok((state.to_owned(), error))
    })();
    let (mut state, mut error) = match work {
        Ok(v) => v,
        Err(e) => ("failed".to_owned(), Some(format!("execution error: {e:#}"))),
    };
    tracker.record("collecting", "collecting Git change artifacts", |s| {
        s.phase = "artifacts".to_owned();
        s.worker_pid = None;
    })?;
    if let Err(e) = collect(&spec, &dir, &tracker) {
        state = "failed".to_owned();
        error = Some(format!(
            "artifact collection failed: {e:#}; prior failure: {error:?}"
        ));
    }
    tracker.finish(&state, error)
}

fn collect(spec: &TaskSpec, dir: &Path, tracker: &Tracker) -> Result<()> {
    let mut diff_spec = run_spec(
        spec,
        "diff",
        vec![
            "git".to_owned(),
            "diff".to_owned(),
            "--no-ext-diff".to_owned(),
            "--no-textconv".to_owned(),
            "--binary".to_owned(),
            spec.baseline_revision.clone(),
            "--".to_owned(),
            ".".to_owned(),
            ":(exclude).leio-code".to_owned(),
        ],
        dir,
    );
    diff_spec.timeout_ms = 10_000;
    let result = process::run(diff_spec)?;
    ensure!(result.status == RunStatus::Passed, "Git diff failed");
    let patch = dir.join("changes.patch");
    std::fs::copy(&result.stdout_path, &patch)?;
    let mut status_spec = run_spec(
        spec,
        "git-status",
        vec![
            "git".to_owned(),
            "status".to_owned(),
            "--porcelain=v1".to_owned(),
            "-z".to_owned(),
            "--untracked-files=all".to_owned(),
            "--".to_owned(),
            ".".to_owned(),
            ":(exclude).leio-code".to_owned(),
        ],
        dir,
    );
    status_spec.timeout_ms = 10_000;
    let status = process::run(status_spec)?;
    ensure!(
        status.status == RunStatus::Passed && !status.stdout_truncated,
        "Git status failed or exceeded output limit"
    );
    // -z preserves filenames exactly, including spaces. Rename/copy source
    // paths occupy a second record; expose both paths without status prefixes.
    let bytes = std::fs::read(&status.stdout_path)?;
    let mut changed = Vec::new();
    let mut records = bytes.split(|b| *b == 0).filter(|b| !b.is_empty());
    for_record(&mut records, &mut changed)?;
    let mut names_spec = run_spec(
        spec,
        "changed-names",
        vec![
            "git".to_owned(),
            "diff".to_owned(),
            "--name-only".to_owned(),
            "-z".to_owned(),
            spec.baseline_revision.clone(),
            "--".to_owned(),
            ".".to_owned(),
            ":(exclude).leio-code".to_owned(),
        ],
        dir,
    );
    names_spec.timeout_ms = 10_000;
    let names = process::run(names_spec)?;
    ensure!(
        names.status == RunStatus::Passed && !names.stdout_truncated,
        "changed-path capture failed"
    );
    changed.extend(
        std::fs::read(&names.stdout_path)?
            .split(|b| *b == 0)
            .filter(|b| !b.is_empty())
            .map(|b| String::from_utf8_lossy(b).into_owned()),
    );
    changed.sort();
    changed.dedup();
    let repo = Path::new(&spec.repo_root);
    let mut archived = Vec::new();
    let mut truncated = false;
    let mut budget = 16 * 1024 * 1024_u64;
    for record in bytes.split(|b| *b == 0).filter(|b| b.starts_with(b"?? ")) {
        let name = std::str::from_utf8(&record[3..]).context("untracked path is not UTF-8")?;
        let relative = Path::new(name);
        ensure!(
            relative
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
            "unsafe untracked path"
        );
        let source = repo.join(relative);
        let metadata = std::fs::symlink_metadata(&source)?;
        if !metadata.is_file() || !crate::process::clean_canonicalize(&source)?.starts_with(repo) {
            truncated = true;
            continue;
        }
        if archived.len() >= 64 || budget == 0 {
            truncated = true;
            continue;
        }
        let target = dir.join("untracked").join(relative);
        private_directory(target.parent().context("untracked parent missing")?)?;
        let mut input_options = OpenOptions::new();
        input_options.read(true);
        #[cfg(unix)]
        input_options.custom_flags(nix::libc::O_NOFOLLOW);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            input_options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let mut input = input_options
            .open(source)?
            .take(budget.min(4 * 1024 * 1024));
        let mut output_options = OpenOptions::new();
        output_options.write(true).create_new(true);
        #[cfg(unix)]
        output_options.mode(0o600);
        let mut output = output_options.open(&target)?;
        let size = std::io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        budget -= size;
        truncated |= metadata.len() > size;
        archived.push(target.display().to_string());
    }
    tracker.record(
        "artifacts",
        "diff and changed-path records collected",
        |s| {
            s.changed_paths = changed;
            s.diff_path = Some(patch.display().to_string());
            s.diff_truncated = result.stdout_truncated;
            s.untracked_artifacts = archived;
            s.untracked_truncated = truncated;
        },
    )
}

fn for_record<'a>(
    records: &mut impl Iterator<Item = &'a [u8]>,
    changed: &mut Vec<String>,
) -> Result<()> {
    while let Some(record) = records.next() {
        ensure!(record.len() >= 4, "invalid Git porcelain record");
        changed.push(String::from_utf8_lossy(&record[3..]).into_owned());
        if record[..2].iter().any(|b| *b == b'R' || *b == b'C') {
            let source = records.next().context("missing rename source")?;
            changed.push(String::from_utf8_lossy(source).into_owned());
        }
    }
    changed.sort();
    changed.dedup();
    Ok(())
}

pub fn inspect(repo: &Path, id: &str) -> Result<Value> {
    let repo = root(repo)?;
    let dir = directory(&repo, id)?;
    let mut s = read_snapshot(&dir)?;
    let alive = locked(&dir.join("owner.lock"))?;
    if !terminal(&s.state)
        && !alive
        && (s.state != "queued" || now().saturating_sub(s.submitted_at_ms) > 10_000)
    {
        s.state = "failed".to_owned();
        s.phase = "interrupted".to_owned();
        s.error=Some("supervision was lost; worker termination is unconfirmed; inspect logs before resubmitting".to_owned());
    }
    let elapsed = s
        .completed_at_ms
        .unwrap_or_else(now)
        .saturating_sub(s.started_at_ms.unwrap_or(s.submitted_at_ms));
    let mut v = serde_json::to_value(&s)?;
    v["supervisorAlive"] = json!(alive && !terminal(&s.state));
    v["elapsedMs"] = json!(elapsed);
    v["silenceMs"] = json!(
        now().saturating_sub(
            s.last_output_at_ms
                .or(s.started_at_ms)
                .unwrap_or(s.submitted_at_ms)
        )
    );
    v["eventsPath"] = json!(dir.join("events.ndjson"));
    v["specPath"] = json!(dir.join("spec.json"));
    v["supervisorLogPath"] = json!(dir.join("supervisor.log"));
    Ok(v)
}
pub fn list(repo: &Path) -> Result<Value> {
    let repo = root(repo)?;
    let path = runs(&repo);
    if !path.exists() {
        return Ok(json!({"repoRoot":repo,"runs":[]}));
    }
    let mut rows = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        rows.push(inspect(&repo, &id)?);
    }
    rows.sort_by_key(|v| std::cmp::Reverse(v["submittedAtMs"].as_u64().unwrap_or_default()));
    rows.truncate(20);
    Ok(json!({"repoRoot":repo,"runs":rows}))
}
pub fn events(repo: &Path, id: &str, after: u64, limit: usize) -> Result<Value> {
    ensure!((1..=100).contains(&limit), "event limit must be 1..100");
    let repo = root(repo)?;
    let dir = directory(&repo, id)?;
    let mut rows = Vec::new();
    let mut more = false;
    for line in BufReader::new(File::open(dir.join("events.ndjson"))?).lines() {
        let line = line?;
        let event: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => break,
        }; // concurrent final append may be incomplete
        if event["seq"].as_u64().unwrap_or_default() <= after {
            continue;
        }
        if rows.len() == limit {
            more = true;
            break;
        }
        rows.push(event);
    }
    let next = rows.last().and_then(|v| v["seq"].as_u64()).unwrap_or(after);
    Ok(json!({"runId":id,"events":rows,"nextAfter":next,"hasMore":more}))
}
pub fn cancel(repo: &Path, id: &str) -> Result<Value> {
    let repo = root(repo)?;
    let dir = directory(&repo, id)?;
    let current = inspect(&repo, id)?;
    if !terminal(current["state"].as_str().unwrap_or("")) {
        let mut options = OpenOptions::new();
        options.create(true).write(true).truncate(true);
        #[cfg(unix)]
        options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let mut file = options.open(dir.join("cancel.request"))?;
        writeln!(file, "{}", now())?;
        file.sync_all()?;
    }
    let mut v = inspect(&repo, id)?;
    v["cancellationRequested"] = json!(dir.join("cancel.request").exists());
    Ok(v)
}
