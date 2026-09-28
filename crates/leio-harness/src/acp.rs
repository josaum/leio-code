//! Supervised bidirectional Agent Client Protocol (ACP) proxy.
//!
//! `leio-harness agent stdio --spec <file>` supervises an agent server process
//! (e.g. `grok agent stdio`) and transparently bridges JSON-RPC frames between
//! this process's stdin/stdout and the child. ACP stays byte-preserving on the
//! wire; every frame is additionally recorded (redacted) into an Arrow IPC
//! event stream, and an idle deadline protects the host from orphaned
//! sessions. The child is spawned as its own process group so timeouts and
//! fatal bridge errors terminate the whole tree.

use crate::arrow_events::{EventStreamWriter, StreamEvent};
use crate::model::{AgentSessionResult, AgentSessionSpec, RunStatus};
use crate::process::inherited_value;
use anyhow::{Context, Result, bail};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Absolute ceiling for a single ACP frame regardless of `max_frame_bytes`.
const HARD_FRAME_CAP: usize = 64 * 1024 * 1024;
/// Cap for stderr lines recorded into Arrow (diagnostics tail only).
const MAX_STDERR_EVENT_BYTES: usize = 4096;
/// Preflight command deadline.
const PREFLIGHT_TIMEOUT_MS: u64 = 30_000;

pub fn serve(spec: AgentSessionSpec) -> Result<AgentSessionResult> {
    validate(&spec)?;
    if let Some(required) = &spec.required_approval_token
        && spec.approval_token.as_ref() != Some(required)
    {
        bail!("approval token mismatch; agent session was not spawned");
    }
    if spec.preflight_leio {
        run_preflight(&spec)?;
    }
    let started_at_ms = now_ms();
    let started = Instant::now();
    let output_dir = PathBuf::from(&spec.output_dir).join(&spec.session_id);
    std::fs::create_dir_all(&output_dir)?;
    let events_arrow_path = output_dir.join("events.arrow");
    let frame_cap = spec.max_frame_bytes.min(HARD_FRAME_CAP);

    let mut command = Command::new(&spec.agent_argv[0]);
    command
        .args(&spec.agent_argv[1..])
        .current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    // Identity, credentials, and session continuity live under HOME; do not
    // inherit the full parent env (isolation), but these are required for
    // agent CLIs to reach their login and session state.
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
        "GROK_API_KEY",
        "XAI_API_KEY",
        "GROK_SESSION_ID",
        "LEIO_SESSION",
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
    let mut child = command
        .spawn()
        .with_context(|| format!("spawn {}", spec.agent_argv[0]))?;
    let agent_stdin = child.stdin.take().context("missing agent stdin")?;
    let agent_stdout = child.stdout.take().context("missing agent stdout")?;
    let agent_stderr = child.stderr.take().context("missing agent stderr")?;

    let (tx, rx) = channel::<StreamEvent>();
    let writer_path = events_arrow_path.clone();
    let writer = thread::spawn(move || arrow_writer(&writer_path, rx));
    let seq = Arc::new(AtomicU64::new(1));
    let last_activity_ms = Arc::new(AtomicU64::new(now_ms()));
    let fatal = Arc::new(AtomicBool::new(false));

    // Parent (ACP client) -> agent. Detached on purpose: the session may end
    // (timeout/kill) while the parent keeps its stdin open, so this thread is
    // allowed to outlive the session and exits when the parent closes stdin or
    // the process exits. Bridge errors set `fatal` so the main loop tears the
    // child down.
    let forward_tx = tx.clone();
    let forward_seq = seq.clone();
    let forward_activity = last_activity_ms.clone();
    let forward_fatal = fatal.clone();
    let mut agent_stdin = agent_stdin;
    let _stdin_thread = thread::spawn(move || -> Result<()> {
        let result = (|| -> Result<()> {
            let mut parent_stdin = BufReader::new(std::io::stdin());
            let mut line = Vec::new();
            loop {
                line.clear();
                let read = read_line_bounded(&mut parent_stdin, &mut line, frame_cap)
                    .context("read parent stdin")?;
                if read == 0 {
                    break;
                }
                record_frame(&forward_tx, &forward_seq, "acp_request", &line)?;
                forward_activity.store(now_ms(), Ordering::Relaxed);
                agent_stdin.write_all(&line)?;
                agent_stdin.flush()?;
            }
            agent_stdin.flush()?;
            Ok(())
        })();
        if result.is_err() {
            forward_fatal.store(true, Ordering::Relaxed);
        }
        result
    });

    // Agent -> parent (ACP client). The child's stdout is the ACP channel, so
    // frames are forwarded byte-for-byte and never captured.
    let response_tx = tx.clone();
    let response_seq = seq.clone();
    let response_activity = last_activity_ms.clone();
    let response_fatal = fatal.clone();
    let stdout_thread = thread::spawn(move || -> Result<()> {
        let result = (|| -> Result<()> {
            let mut parent_stdout = std::io::stdout();
            let mut reader = BufReader::new(agent_stdout);
            let mut line = Vec::new();
            loop {
                line.clear();
                let read = read_line_bounded(&mut reader, &mut line, frame_cap)
                    .context("read agent stdout")?;
                if read == 0 {
                    break;
                }
                record_frame(&response_tx, &response_seq, "acp_response", &line)?;
                response_activity.store(now_ms(), Ordering::Relaxed);
                parent_stdout.write_all(&line)?;
                parent_stdout.flush()?;
            }
            Ok(())
        })();
        if result.is_err() {
            response_fatal.store(true, Ordering::Relaxed);
        }
        result
    });

    // Agent stderr -> our stderr, with a bounded event record.
    let stderr_tx = tx.clone();
    let stderr_seq = seq.clone();
    let stderr_fatal = fatal.clone();
    let stderr_thread = thread::spawn(move || -> Result<()> {
        let result = (|| -> Result<()> {
            let mut reader = BufReader::new(agent_stderr);
            let mut line = Vec::new();
            loop {
                line.clear();
                let read = read_line_bounded(&mut reader, &mut line, frame_cap)
                    .context("read agent stderr")?;
                if read == 0 {
                    break;
                }
                eprint!("{}", String::from_utf8_lossy(&line));
                let truncated: Vec<u8> =
                    line.iter().take(MAX_STDERR_EVENT_BYTES).copied().collect();
                record_frame(&stderr_tx, &stderr_seq, "acp_agent_stderr", &truncated)?;
            }
            Ok(())
        })();
        if result.is_err() {
            stderr_fatal.store(true, Ordering::Relaxed);
        }
        result
    });
    drop(tx);

    let idle_timeout = Duration::from_millis(spec.idle_timeout_ms);
    let (status, exit_code, error) = loop {
        if fatal.load(Ordering::Relaxed) {
            terminate_group(child.id(), 2_000)?;
            let exit = child.wait()?;
            break (
                RunStatus::InfraError,
                exit.code(),
                Some("ACP bridge failed; see stderr for the underlying error".to_owned()),
            );
        }
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
        if spec.idle_timeout_ms > 0 {
            let idle_ms = now_ms().saturating_sub(last_activity_ms.load(Ordering::Relaxed));
            if idle_ms as u128 >= idle_timeout.as_millis() {
                terminate_group(child.id(), 2_000)?;
                let exit = child.wait()?;
                break (
                    RunStatus::TimedOut,
                    exit.code(),
                    Some(format!("no ACP traffic for {}ms", spec.idle_timeout_ms)),
                );
            }
        }
        thread::sleep(Duration::from_millis(20));
    };

    // The child pipes close on exit/kill, so the stdout/stderr bridges reach
    // EOF. The parent-stdin bridge is deliberately not joined.
    if let Err(error) = stdout_thread
        .join()
        .map_err(|_| anyhow::anyhow!("stdout bridge panicked"))?
    {
        eprintln!("[acp] stdout bridge: {error:#}");
    }
    if let Err(error) = stderr_thread
        .join()
        .map_err(|_| anyhow::anyhow!("stderr bridge panicked"))?
    {
        eprintln!("[acp] stderr bridge: {error:#}");
    }
    if let Err(error) = writer
        .join()
        .map_err(|_| anyhow::anyhow!("arrow writer panicked"))?
    {
        eprintln!("[acp] arrow writer: {error:#}");
    }

    let completed_at_ms = now_ms();
    let result = AgentSessionResult {
        version: 1,
        session_id: spec.session_id.clone(),
        status,
        exit_code,
        started_at_ms,
        completed_at_ms,
        duration_ms: started.elapsed().as_millis() as u64,
        events_arrow_path: events_arrow_path.display().to_string(),
        error,
    };
    write_json_atomic(&output_dir.join("result.json"), &result)?;
    eprintln!(
        "[acp] session {} finished: status={} exit={:?} duration_ms={} events={}",
        result.session_id,
        serde_json::to_string(&result.status)?,
        result.exit_code,
        result.duration_ms,
        result.events_arrow_path,
    );
    Ok(result)
}

fn validate(spec: &AgentSessionSpec) -> Result<()> {
    if spec.session_id.trim().is_empty() {
        bail!("session_id is required");
    }
    if spec.agent_argv.is_empty() || spec.agent_argv[0].trim().is_empty() {
        bail!("agent_argv must be non-empty");
    }
    if spec.max_frame_bytes == 0 {
        bail!("max_frame_bytes must be positive");
    }
    if !Path::new(&spec.cwd).is_dir() {
        bail!("cwd is not a directory: {}", spec.cwd);
    }
    Ok(())
}

fn run_preflight(spec: &AgentSessionSpec) -> Result<()> {
    let argv = if !spec.preflight_cmd.is_empty() {
        spec.preflight_cmd.clone()
    } else {
        vec![
            "leio-code".to_owned(),
            "status".to_owned(),
            "--json".to_owned(),
            "--repo".to_owned(),
            spec.cwd.clone(),
        ]
    };
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]).current_dir(&spec.cwd);
    let status = run_with_timeout(&mut command, PREFLIGHT_TIMEOUT_MS)?;
    if !status.success() {
        bail!(
            "LEIO preflight failed ({status}); leio-code status must pass before starting the agent session"
        );
    }
    Ok(())
}

fn run_with_timeout(command: &mut Command, timeout_ms: u64) -> Result<std::process::ExitStatus> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().context("spawn preflight")?;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            terminate_group(child.id(), 500)?;
            bail!("preflight timed out after {timeout_ms}ms");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Read one line (including `\n`) from a buffered reader, bounded by `cap`.
/// Returns the number of bytes read; zero means EOF.
fn read_line_bounded<R: BufRead>(reader: &mut R, out: &mut Vec<u8>, cap: usize) -> Result<usize> {
    let mut limited = reader.by_ref().take((cap + 1) as u64);
    let read = limited.read_until(b'\n', out)?;
    if out.len() > cap {
        bail!("ACP frame exceeds {cap} bytes");
    }
    Ok(read)
}

/// Record a redacted frame: method/id plus byte length, never raw content.
fn record_frame(
    tx: &Sender<StreamEvent>,
    seq: &Arc<AtomicU64>,
    kind: &str,
    line: &[u8],
) -> Result<()> {
    let mut value = serde_json::Map::new();
    value.insert("frame_len".to_owned(), serde_json::json!(line.len()));
    if let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(line) {
        if let Some(object) = parsed.as_object() {
            if let Some(method) = object.get("method").and_then(|v| v.as_str()) {
                value.insert("method".to_owned(), serde_json::json!(method));
            }
            if let Some(id) = object.get("id") {
                value.insert("id".to_owned(), id.clone());
            }
        }
    } else {
        value.insert("parse_error".to_owned(), serde_json::json!(true));
    }
    let payload = serde_json::to_vec(&value)?;
    tx.send(StreamEvent {
        seq: seq.fetch_add(1, Ordering::Relaxed),
        timestamp_ms: now_ms() as i64,
        kind: kind.to_owned(),
        stream: "acp".to_owned(),
        payload,
    })?;
    Ok(())
}

fn arrow_writer(path: &Path, rx: Receiver<StreamEvent>) -> Result<()> {
    let mut writer = EventStreamWriter::create(path)?;
    for event in rx {
        writer.write_event(event)?;
    }
    writer.finish()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::Cursor;
    use tempfile::TempDir;

    fn valid_spec(root: &Path) -> AgentSessionSpec {
        AgentSessionSpec {
            session_id: "session-test".to_owned(),
            cwd: root.display().to_string(),
            output_dir: root.join("runs").display().to_string(),
            agent_argv: vec!["/usr/bin/true".to_owned()],
            env_allowlist: Vec::new(),
            env: BTreeMap::new(),
            idle_timeout_ms: 1_000,
            max_frame_bytes: 1_024,
            preflight_leio: false,
            preflight_cmd: Vec::new(),
            required_approval_token: None,
            approval_token: None,
        }
    }

    #[test]
    fn validation_rejects_each_invalid_session_boundary() {
        let root = TempDir::new().expect("tempdir");
        let spec = valid_spec(root.path());
        validate(&spec).expect("valid spec");

        let mut missing_session = spec.clone();
        missing_session.session_id = "  ".to_owned();
        assert!(
            validate(&missing_session)
                .unwrap_err()
                .to_string()
                .contains("session_id is required")
        );

        let mut missing_agent = spec.clone();
        missing_agent.agent_argv.clear();
        assert!(
            validate(&missing_agent)
                .unwrap_err()
                .to_string()
                .contains("agent_argv must be non-empty")
        );

        let mut blank_agent = spec.clone();
        blank_agent.agent_argv = vec!["  ".to_owned()];
        assert!(validate(&blank_agent).is_err());

        let mut zero_frame_cap = spec.clone();
        zero_frame_cap.max_frame_bytes = 0;
        assert!(
            validate(&zero_frame_cap)
                .unwrap_err()
                .to_string()
                .contains("max_frame_bytes must be positive")
        );

        let mut missing_cwd = spec;
        missing_cwd.cwd = root.path().join("missing").display().to_string();
        assert!(
            validate(&missing_cwd)
                .unwrap_err()
                .to_string()
                .contains("cwd is not a directory")
        );
    }

    #[test]
    fn bounded_line_reader_accepts_the_limit_and_rejects_oversized_frames() {
        let mut exact = Cursor::new(b"1234\nnext\n".to_vec());
        let mut line = Vec::new();
        assert_eq!(read_line_bounded(&mut exact, &mut line, 5).unwrap(), 5);
        assert_eq!(line, b"1234\n");

        let mut oversized = Cursor::new(b"1234\n".to_vec());
        let error = read_line_bounded(&mut oversized, &mut Vec::new(), 4).unwrap_err();
        assert!(error.to_string().contains("ACP frame exceeds 4 bytes"));

        let mut empty = Cursor::new(Vec::<u8>::new());
        assert_eq!(
            read_line_bounded(&mut empty, &mut Vec::new(), 4).unwrap(),
            0
        );
    }

    #[test]
    fn frame_records_are_redacted_and_mark_parse_errors() {
        let (tx, rx) = channel();
        let seq = Arc::new(AtomicU64::new(7));
        let secret =
            br#"{"jsonrpc":"2.0","id":42,"method":"session/start","params":{"token":"secret"}}
"#;

        record_frame(&tx, &seq, "acp_request", secret).expect("record valid frame");
        record_frame(&tx, &seq, "acp_request", b"not-json\n").expect("record malformed frame");

        let valid = rx.recv().expect("valid event");
        assert_eq!(valid.seq, 7);
        assert_eq!(valid.kind, "acp_request");
        let payload: serde_json::Value =
            serde_json::from_slice(&valid.payload).expect("valid payload");
        assert_eq!(payload["method"], "session/start");
        assert_eq!(payload["id"], 42);
        assert_eq!(payload["frame_len"], secret.len());
        assert!(payload.get("params").is_none());
        assert!(!String::from_utf8_lossy(&valid.payload).contains("secret"));

        let malformed = rx.recv().expect("malformed event");
        assert_eq!(malformed.seq, 8);
        let payload: serde_json::Value =
            serde_json::from_slice(&malformed.payload).expect("malformed payload");
        assert_eq!(payload["parse_error"], true);
    }

    #[test]
    fn frame_recording_reports_a_closed_event_channel() {
        let (tx, rx) = channel();
        drop(rx);

        assert!(record_frame(&tx, &Arc::new(AtomicU64::new(1)), "request", b"{}\n").is_err());
    }

    #[test]
    fn custom_preflight_accepts_success_and_reports_failure() {
        let root = TempDir::new().expect("tempdir");
        let mut spec = valid_spec(root.path());
        spec.preflight_cmd = vec!["/bin/sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()];
        run_preflight(&spec).expect("successful preflight");

        spec.preflight_cmd[2] = "exit 9".to_owned();
        let error = run_preflight(&spec).unwrap_err();
        assert!(error.to_string().contains("LEIO preflight failed"));
    }

    #[test]
    fn preflight_timeout_terminates_the_entire_process_group() {
        let root = TempDir::new().expect("tempdir");
        let marker = root.path().join("orphaned-preflight");
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "sleep 0.2; touch \"$MARKER\""])
            .env("MARKER", &marker);

        let error = run_with_timeout(&mut command, 10).unwrap_err();

        assert!(error.to_string().contains("preflight timed out after 10ms"));
        assert!(
            !marker.exists(),
            "timed-out preflight escaped its process group"
        );
    }
}
