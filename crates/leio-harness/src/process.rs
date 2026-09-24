use crate::arrow_events::{EventStreamWriter, StreamEvent};
use crate::model::{RunResult, RunSpec, RunStatus};
use anyhow::{Context, Result, bail};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(crate) fn unique_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{nanos:x}-{:x}-{:x}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

pub fn run(spec: RunSpec) -> Result<RunResult> {
    validate_spec(&spec)?;
    if let Some(required) = &spec.required_approval_token
        && spec.approval_token.as_ref() != Some(required)
    {
        bail!("approval token mismatch; process was not spawned");
    }
    let output_dir = PathBuf::from(&spec.output_dir).join(&spec.run_id);
    std::fs::create_dir_all(&spec.output_dir)?;
    std::fs::create_dir(&output_dir)
        .context("run directory already exists or cannot be created; use a unique run_id")?;
    let stdout_path = output_dir.join("stdout.log");
    let stderr_path = output_dir.join("stderr.log");
    let arrow_path = output_dir.join("events.arrow");
    let result_path = output_dir.join("result.json");
    let started_at_ms = now_ms();
    let started = Instant::now();
    let mut command = Command::new(&spec.argv[0]);
    command
        .args(&spec.argv[1..])
        .current_dir(&spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    // Identity + credential files live under HOME; USER is required by Claude
    // Code login. Do not inherit the full parent env (isolation), but without
    // these the agent CLIs fail before they can do work.
    for name in [
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "TMPDIR",
        "LANG",
        "LC_ALL",
        "TERM",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_RUNTIME_DIR",
        "SSH_AUTH_SOCK",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CONFIG_DIR",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "GROK_API_KEY",
        "XAI_API_KEY",
        "DEEPSEEK_API_KEY",
        "DEEPSEEK_APIKEY",
    ] {
        if let Some(value) = inherited_value(name) {
            command.env(name, value);
        }
    }
    let allowed = spec.env_allowlist.iter().collect::<BTreeSet<_>>();
    for (name, value) in &spec.env {
        if allowed.is_empty() || allowed.contains(name) {
            command.env(name, value);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    // Establish logs before any external effects can occur.
    let stdout_log = File::create(&stdout_path)?;
    let stderr_log = File::create(&stderr_path)?;
    let mut child = OwnedChild(
        command
            .spawn()
            .with_context(|| format!("spawn {}", spec.argv[0]))?,
    );
    let stdout = child.stdout.take().context("missing stdout")?;
    let stderr = child.stderr.take().context("missing stderr")?;
    let seq = Arc::new(AtomicU64::new(1));
    // Channel moves chunk ownership from readers to the Arrow writer; payloads
    // are never copied between threads.
    let (tx, rx) = sync_channel::<StreamEvent>(32);
    let arrow_writer_path = arrow_path.clone();
    let writer = thread::spawn(move || arrow_writer(&arrow_writer_path, rx));
    let finished = Arc::new(AtomicBool::new(false));
    let stdout_truncated = Arc::new(AtomicBool::new(false));
    let stderr_truncated = Arc::new(AtomicBool::new(false));
    let stdout_reader = read_stream(
        stdout,
        "stdout",
        spec.max_output_bytes,
        stdout_log,
        tx.clone(),
        seq.clone(),
        StreamFlags {
            truncated: stdout_truncated.clone(),
            finished: finished.clone(),
        },
    );
    let stderr_reader = read_stream(
        stderr,
        "stderr",
        spec.max_output_bytes,
        stderr_log,
        tx,
        seq,
        StreamFlags {
            truncated: stderr_truncated.clone(),
            finished: finished.clone(),
        },
    );
    let deadline = started + Duration::from_millis(spec.timeout_ms);
    let (status, exit_code, error) = loop {
        if let Some(exit) = child.try_wait()? {
            break (
                if exit.success() {
                    RunStatus::Passed
                } else {
                    RunStatus::Failed
                },
                exit.code(),
                None,
            );
        }
        if Instant::now() >= deadline {
            terminate_group(child.id(), spec.kill_grace_ms)?;
            let exit = child.wait()?;
            break (
                RunStatus::TimedOut,
                exit.code(),
                Some(format!("process exceeded {}ms timeout", spec.timeout_ms)),
            );
        }
        thread::sleep(Duration::from_millis(20));
    };
    // Descendants can retain stdout/stderr after the leader exits. Terminate
    // the owned process group before joining pipe readers, or a successful
    // shell can hang this supervisor forever.
    let _ = killpg(Pid::from_raw(child.id() as i32), Signal::SIGKILL);
    finished.store(true, Ordering::Release);
    stdout_reader
        .join()
        .map_err(|_| anyhow::anyhow!("stdout reader panicked"))??;
    stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("stderr reader panicked"))??;
    writer
        .join()
        .map_err(|_| anyhow::anyhow!("arrow writer panicked"))??;
    let stdout_truncated = stdout_truncated.load(Ordering::Relaxed);
    let stderr_truncated = stderr_truncated.load(Ordering::Relaxed);
    let completed_at_ms = now_ms();
    let result = RunResult {
        version: 1,
        run_id: spec.run_id,
        status,
        exit_code,
        started_at_ms,
        completed_at_ms,
        duration_ms: started.elapsed().as_millis() as u64,
        stdout_path: stdout_path.display().to_string(),
        stderr_path: stderr_path.display().to_string(),
        events_arrow_path: arrow_path.display().to_string(),
        stdout_truncated,
        stderr_truncated,
        error,
    };
    write_json_atomic(&result_path, &result)?;
    Ok(result)
}

struct OwnedChild(std::process::Child);
impl std::ops::Deref for OwnedChild {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for OwnedChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = killpg(Pid::from_raw(self.0.id() as i32), Signal::SIGKILL);
        let _ = self.0.wait();
    }
}

type StreamReaderHandle = thread::JoinHandle<Result<()>>;
struct StreamFlags {
    truncated: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
}

fn read_stream<R: Read + AsRawFd + Send + 'static>(
    mut reader: R,
    stream: &'static str,
    max_bytes: usize,
    mut log: File,
    tx: SyncSender<StreamEvent>,
    seq: Arc<AtomicU64>,
    flags: StreamFlags,
) -> StreamReaderHandle {
    thread::spawn(move || {
        let StreamFlags {
            truncated: truncated_flag,
            finished,
        } = flags;
        let fd = reader.as_raw_fd();
        // SAFETY: fd is borrowed from the owned pipe reader, valid for this
        // thread's lifetime. fcntl only changes the descriptor's status flags.
        unsafe {
            let flags = nix::libc::fcntl(fd, nix::libc::F_GETFL);
            if flags < 0
                || nix::libc::fcntl(fd, nix::libc::F_SETFL, flags | nix::libc::O_NONBLOCK) < 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        let mut captured = 0_usize;
        let mut drain_chunks = 0;
        loop {
            if finished.load(Ordering::Acquire) {
                drain_chunks += 1;
                if drain_chunks > 32 {
                    break;
                }
            }
            let mut buffer = vec![0_u8; 64 * 1024];
            let read = match reader.read(&mut buffer) {
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if finished.load(Ordering::Acquire) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            if read == 0 {
                break;
            }
            buffer.truncate(read);
            let remaining = max_bytes.saturating_sub(captured);
            if remaining > 0 {
                let kept = buffer.len().min(remaining);
                log.write_all(&buffer[..kept])?;
                captured += kept;
            }
            if read > remaining {
                truncated_flag.store(true, Ordering::Relaxed);
            }
            buffer.truncate(read.min(remaining));
            if buffer.is_empty() {
                continue;
            }
            // Ownership of the chunk moves into the channel; the Arrow writer
            // moves it into an IPC batch without copying.
            if tx
                .send(StreamEvent {
                    seq: seq.fetch_add(1, Ordering::Relaxed),
                    timestamp_ms: now_ms() as i64,
                    kind: "process_output".to_owned(),
                    stream: stream.to_owned(),
                    payload: buffer,
                })
                .is_err()
            {
                break;
            }
        }
        log.sync_all()?;
        Ok(())
    })
}

fn arrow_writer(path: &Path, rx: Receiver<StreamEvent>) -> Result<()> {
    let mut writer = EventStreamWriter::create(path)?;
    for event in rx {
        writer.write_event(event)?;
    }
    writer.finish()
}

fn validate_spec(spec: &RunSpec) -> Result<()> {
    if spec.run_id.trim().is_empty() || spec.argv.is_empty() || spec.argv[0].trim().is_empty() {
        bail!("run_id and argv are required");
    }
    if Path::new(&spec.run_id).components().count() != 1
        || !matches!(
            Path::new(&spec.run_id).components().next(),
            Some(std::path::Component::Normal(_))
        )
        || spec.run_id.contains('\\')
    {
        bail!("run_id must be a single safe path component");
    }
    if spec.timeout_ms == 0 || spec.kill_grace_ms == 0 || spec.max_output_bytes == 0 {
        bail!("timeouts and output bound must be positive");
    }
    if !Path::new(&spec.cwd).is_dir() {
        bail!("cwd is not a directory: {}", spec.cwd);
    }
    Ok(())
}

fn terminate_group(pid: u32, grace_ms: u64) -> Result<()> {
    #[cfg(unix)]
    {
        let pgid = Pid::from_raw(pid as i32);
        let _ = killpg(pgid, Signal::SIGTERM);
        thread::sleep(Duration::from_millis(grace_ms));
        let _ = killpg(pgid, Signal::SIGKILL);
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        thread::sleep(Duration::from_millis(grace_ms));
    }
    Ok(())
}

pub(crate) fn write_json_atomic(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut file = File::create(&temporary)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    std::fs::rename(temporary, path)?;
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub(crate) fn inherited_value(name: &str) -> Option<String> {
    if let Ok(value) = std::env::var(name)
        && !value.is_empty()
    {
        return Some(value);
    }
    crate::embed::read_env_file(&harness_env_path()?)
        .ok()?
        .get(name)
        .cloned()
        .filter(|value| !value.is_empty())
}

pub(crate) fn harness_env_path() -> Option<std::path::PathBuf> {
    std::env::var("LEIO_HARNESS_CONFIG")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|home| std::path::Path::new(&home).join(".config/leio-harness/env"))
        })
}

#[cfg(test)]
mod property_tests {
    use crate::model::RunSpec;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn run_spec_deserialize_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..=1024)) {
            if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                let _ = serde_json::from_value::<RunSpec>(json);
            }
        }

        #[test]
        fn run_spec_roundtrips_valid_specs(
            run_id in "[a-zA-Z0-9-]{1,32}",
            argv in prop::collection::vec("[ -~]{1,32}", 1..=8),
            timeout_ms in 1u64..600_000,
        ) {
            let spec = RunSpec {
                run_id,
                argv,
                cwd: "/tmp".to_owned(),
                output_dir: "/tmp/runs".to_owned(),
                timeout_ms,
                kill_grace_ms: 2_000,
                max_output_bytes: 1024,
                env_allowlist: vec![],
                env: Default::default(),
                required_approval_token: None,
                approval_token: None,
            };
            let json = serde_json::to_value(&spec).unwrap();
            let back: RunSpec = serde_json::from_value(json).unwrap();
            assert_eq!(back.run_id, spec.run_id);
            assert_eq!(back.argv, spec.argv);
            assert_eq!(back.timeout_ms, spec.timeout_ms);
        }
    }
}

#[cfg(test)]
mod hardening_tests {
    use super::*;
    fn spec(dir: &Path, script: &str) -> RunSpec {
        RunSpec {
            run_id: "test".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
            cwd: dir.display().to_string(),
            output_dir: dir.display().to_string(),
            timeout_ms: 2000,
            kill_grace_ms: 20,
            max_output_bytes: 1024,
            env: Default::default(),
            env_allowlist: vec![],
            required_approval_token: None,
            approval_token: None,
        }
    }
    #[test]
    fn output_bound_applies_to_logs_and_arrow() {
        let dir = tempfile::tempdir().unwrap();
        let result = run(spec(dir.path(), "head -c 1048576 /dev/zero")).unwrap();
        assert_eq!(result.status, RunStatus::Passed);
        assert!(result.stdout_truncated);
        assert_eq!(std::fs::metadata(result.stdout_path).unwrap().len(), 1024);
        assert!(std::fs::metadata(result.events_arrow_path).unwrap().len() < 16384);
    }
    #[test]
    fn inherited_pipe_does_not_hang_after_leader_exit() {
        let dir = tempfile::tempdir().unwrap();
        let start = Instant::now();
        let result = run(spec(dir.path(), "sleep 30 & echo done")).unwrap();
        assert_eq!(result.status, RunStatus::Passed);
        assert!(start.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn traversal_and_existing_run_directory_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        for id in ["../escape", "/tmp/escape", ".", "..", "a/b", "a\\b"] {
            let mut s = spec(dir.path(), "true");
            s.run_id = id.into();
            assert!(run(s).is_err(), "accepted {id}");
        }
        run(spec(dir.path(), "echo original")).unwrap();
        assert!(run(spec(dir.path(), "echo overwritten")).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("test/stdout.log")).unwrap(),
            "original\n"
        );
    }
}
