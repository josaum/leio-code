use crate::arrow_events::{EventStreamWriter, StreamEvent};
use crate::model::{RunResult, RunSpec, RunStatus};
use anyhow::{Context, Result, bail};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub fn run(spec: RunSpec) -> Result<RunResult> {
    validate_spec(&spec)?;
    if let Some(required) = &spec.required_approval_token
        && spec.approval_token.as_ref() != Some(required)
    {
        bail!("approval token mismatch; process was not spawned");
    }
    let output_dir = PathBuf::from(&spec.output_dir).join(&spec.run_id);
    std::fs::create_dir_all(&output_dir)?;
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
    let mut child = command
        .spawn()
        .with_context(|| format!("spawn {}", spec.argv[0]))?;
    let stdout = child.stdout.take().context("missing stdout")?;
    let stderr = child.stderr.take().context("missing stderr")?;
    let seq = Arc::new(AtomicU64::new(1));
    // Channel moves chunk ownership from readers to the Arrow writer; payloads
    // are never copied between threads.
    let (tx, rx) = channel::<StreamEvent>();
    let arrow_writer_path = arrow_path.clone();
    let writer = thread::spawn(move || arrow_writer(&arrow_writer_path, rx));
    let stdout_truncated = Arc::new(AtomicBool::new(false));
    let stderr_truncated = Arc::new(AtomicBool::new(false));
    let stdout_reader = read_stream(
        stdout,
        "stdout",
        spec.max_output_bytes,
        stdout_log,
        tx.clone(),
        seq.clone(),
        stdout_truncated.clone(),
    );
    let stderr_reader = read_stream(
        stderr,
        "stderr",
        spec.max_output_bytes,
        stderr_log,
        tx,
        seq,
        stderr_truncated.clone(),
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

type StreamReaderHandle = thread::JoinHandle<Result<()>>;

fn read_stream<R: Read + Send + 'static>(
    mut reader: R,
    stream: &'static str,
    max_bytes: usize,
    mut log: File,
    tx: Sender<StreamEvent>,
    seq: Arc<AtomicU64>,
    truncated_flag: Arc<AtomicBool>,
) -> StreamReaderHandle {
    thread::spawn(move || {
        let mut captured = 0_usize;
        loop {
            let mut buffer = vec![0_u8; 64 * 1024];
            let read = reader.read(&mut buffer)?;
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

fn write_json_atomic(path: &Path, value: &impl serde::Serialize) -> Result<()> {
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
