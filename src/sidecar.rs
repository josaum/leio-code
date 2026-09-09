//! Atomic sidecar writes and advisory locks for concurrent agents.
//!
//! Several agents and scripts may hit the same `repo_root` at once. Shared
//! artifacts (`index.json`, wiki, formal graph, lattice) are written with
//! temp+rename and rebuilt under a pid lock. Nav cursors are isolated by
//! `LEIO_SESSION` (or a detected host session id) so two agents do not
//! overwrite each other's `goto`.
// Rust guideline compliant 2026-02-21

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::json;

/// How long a waiter polls for a sibling rebuild.
///
/// 180s covers a full monorepo index on a laptop. A shorter wait makes the
/// second agent time out and start a duplicate walk. Override with
/// `LEIO_LOCK_WAIT_SECS`.
const DEFAULT_LOCK_WAIT_SECS: u64 = 180;

/// A lock older than this whose pid is gone is stolen.
///
/// 600s is longer than a large-tree index so we do not steal a live walk.
/// Override with `LEIO_LOCK_STALE_SECS`.
const DEFAULT_LOCK_STALE_SECS: u64 = 600;

/// First poll interval while waiting on a sibling lock.
///
/// 50ms is short enough that a finished rebuild is seen immediately, and
/// long enough to avoid a busy loop. Backs off to [`LOCK_POLL_MAX`].
const LOCK_POLL_MIN: Duration = Duration::from_millis(50);

/// Upper bound on lock-wait sleep.
///
/// 250ms keeps wait latency low without waking every tens of milliseconds
/// for a rebuild that may take minutes.
const LOCK_POLL_MAX: Duration = Duration::from_millis(250);

/// Cap workspace-member listing on status.
///
/// Status envelopes stay small; agents pin `--repo` from the first 64
/// packages and read Cargo/pnpm files for the rest.
const MAX_WORKSPACE_MEMBERS: usize = 64;

static SESSION_PIN: OnceLock<Mutex<Option<String>>> = OnceLock::new();
static HELD_LOCKS: OnceLock<Mutex<HashMap<PathBuf, u32>>> = OnceLock::new();

fn session_pin() -> &'static Mutex<Option<String>> {
    SESSION_PIN.get_or_init(|| Mutex::new(None))
}

fn held_locks() -> &'static Mutex<HashMap<PathBuf, u32>> {
    HELD_LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Pin the nav session slug for this process (`--session` / tests).
pub fn pin_session(slug: &str) {
    let cleaned = sanitize_session_slug(slug);
    if cleaned.is_empty() {
        return;
    }
    if let Ok(mut slot) = session_pin().lock() {
        *slot = Some(cleaned);
    }
}

/// Slug used to isolate nav (and future) per-agent state.
pub fn session_slug() -> Option<String> {
    if let Ok(slot) = session_pin().lock()
        && let Some(pinned) = slot.as_deref().filter(|value| !value.is_empty())
    {
        return Some(pinned.to_string());
    }
    if let Some(path) = env_nonempty("LEIO_NAV_SESSION") {
        let cleaned = sanitize_session_slug(&path_stem(&path));
        if !cleaned.is_empty() {
            return Some(cleaned);
        }
    }
    const KEYS: &[&str] = &[
        "LEIO_SESSION",
        "CLAUDE_SESSION_ID",
        "CLAUDE_CODE_SESSION",
        "CURSOR_SESSION_ID",
        "GROK_SESSION_ID",
        "GROK_CONVERSATION_ID",
        "CODEX_SESSION_ID",
        "TERM_SESSION_ID",
    ];
    for key in KEYS {
        if let Some(raw) = env_nonempty(key) {
            let cleaned = sanitize_session_slug(&raw);
            if !cleaned.is_empty() {
                return Some(cleaned);
            }
        }
    }
    None
}

/// On-disk nav session path for this agent.
pub fn nav_session_path(repo_root: &Path) -> PathBuf {
    if let Some(explicit) = env_nonempty("LEIO_NAV_SESSION") {
        let path = PathBuf::from(explicit);
        if path.is_absolute() {
            return path;
        }
        return repo_root.join(path);
    }
    match session_slug() {
        Some(slug) => repo_root
            .join(".leio-code")
            .join("sessions")
            .join(format!("nav-{slug}.json")),
        None => repo_root.join(".leio-code").join("nav-session.json"),
    }
}

/// Session identity for status / nav envelopes.
pub fn session_report(repo_root: &Path) -> serde_json::Value {
    let slug = session_slug();
    let path = nav_session_path(repo_root);
    json!({
        "id": slug,
        "isolated": slug.is_some(),
        "nav": rel_or_name(repo_root, &path),
    })
}

/// Keep a session id filesystem-safe and short.
pub fn sanitize_session_slug(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if out.len() >= 64 {
            break;
        }
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// Write `bytes` via a same-directory temp file, then rename over `path`.
///
/// # Errors
///
/// Returns an error when the parent cannot be created, the temp file cannot
/// be written, or the rename fails. The temp file is removed on failure.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("sidecar path has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let tmp = temp_path(path);
    if let Err(err) = fs::write(&tmp, bytes) {
        let _ = fs::remove_file(&tmp);
        return Err(err).with_context(|| format!("write {}", tmp.display()));
    }
    rename_replace(&tmp, path)
}

/// Serialize `value` as pretty JSON and write it atomically.
///
/// # Errors
///
/// Returns an error when serialization or the atomic write fails.
pub fn write_atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let raw = serde_json::to_vec_pretty(value).context("serialize sidecar json")?;
    write_atomic(path, &raw)
}

/// Hidden temp sibling used for atomic replace.
pub fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "sidecar".to_string());
    match path.parent() {
        Some(parent) => parent.join(format!(".{name}.tmp-{}", std::process::id())),
        None => PathBuf::from(format!(".{name}.tmp-{}", std::process::id())),
    }
}

/// Rename `tmp` onto `dest`, removing `dest` first on Windows.
///
/// # Errors
///
/// Returns an error when rename fails. The temp file is removed on failure.
pub fn rename_replace(tmp: &Path, dest: &Path) -> Result<()> {
    #[cfg(windows)]
    if dest.exists() {
        let _ = fs::remove_file(dest);
    }
    if let Err(err) = fs::rename(tmp, dest) {
        let _ = fs::remove_file(tmp);
        return Err(err).with_context(|| format!("rename into {}", dest.display()));
    }
    Ok(())
}

/// Held advisory lock. Unlinks the lock file on the last in-process drop.
#[derive(Debug)]
pub struct SidecarLock {
    path: PathBuf,
    release: bool,
}

impl Drop for SidecarLock {
    fn drop(&mut self) {
        if !self.release {
            return;
        }
        let last = if let Ok(mut held) = held_locks().lock() {
            let count = held.get_mut(&self.path).copied().unwrap_or(1);
            if count <= 1 {
                held.remove(&self.path);
                true
            } else {
                held.insert(self.path.clone(), count - 1);
                false
            }
        } else {
            true
        };
        if last {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Acquire `.leio-code/{name}.lock`, waiting for a sibling agent if needed.
///
/// Re-entrant in-process: nested acquires increment a counter and do not
/// unlock until the last guard drops.
///
/// # Errors
///
/// Returns an error when the lock directory cannot be created or the wait
/// budget is exhausted while another live process still holds the lock.
pub fn acquire_lock(repo_root: &Path, name: &str) -> Result<SidecarLock> {
    let safe = sanitize_session_slug(name);
    if safe.is_empty() {
        bail!("lock name is empty");
    }
    let dir = repo_root.join(".leio-code");
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join(format!("{safe}.lock"));

    if let Ok(mut held) = held_locks().lock()
        && let Some(count) = held.get_mut(&path)
    {
        *count = count.saturating_add(1);
        return Ok(SidecarLock {
            path,
            release: true,
        });
    }

    let wait_for = Duration::from_secs(lock_wait_secs());
    let stale_after = Duration::from_secs(lock_stale_secs());
    let deadline = Instant::now() + wait_for;
    let mut sleep_for = LOCK_POLL_MIN;

    loop {
        match try_create_lock(&path) {
            Ok(()) => {
                if let Ok(mut held) = held_locks().lock() {
                    held.insert(path.clone(), 1);
                }
                return Ok(SidecarLock {
                    path,
                    release: true,
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                if steal_stale_lock(&path, stale_after) {
                    continue;
                }
                if Instant::now() >= deadline {
                    bail!(
                        "timed out waiting {}s for {} (another agent is rebuilding)",
                        wait_for.as_secs(),
                        path.display()
                    );
                }
                thread::sleep(sleep_for);
                sleep_for = (sleep_for * 2).min(LOCK_POLL_MAX);
            }
            Err(err) => {
                return Err(err).with_context(|| format!("create lock {}", path.display()));
            }
        }
    }
}

/// Cargo / pnpm / npm workspace members under `root`.
pub fn workspace_members(root: &Path) -> Vec<WorkspaceMember> {
    let mut out = Vec::new();
    push_unique(&mut out, cargo_workspace_members(root));
    push_unique(&mut out, pnpm_workspace_members(root));
    push_unique(&mut out, npm_workspace_members(root));
    out.truncate(MAX_WORKSPACE_MEMBERS);
    out
}

/// One package directory inside a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceMember {
    pub path: String,
    pub kind: String,
}

fn cargo_workspace_members(root: &Path) -> Vec<WorkspaceMember> {
    let Ok(raw) = fs::read_to_string(root.join("Cargo.toml")) else {
        return Vec::new();
    };
    let Ok(value) = raw.parse::<toml::Value>() else {
        return Vec::new();
    };
    let Some(members) = value
        .get("workspace")
        .and_then(|table| table.get("members"))
        .and_then(toml::Value::as_array)
    else {
        return Vec::new();
    };
    let patterns: Vec<String> = members
        .iter()
        .filter_map(|row| row.as_str().map(str::to_string))
        .collect();
    expand_member_globs(root, &patterns, "cargo")
}

fn pnpm_workspace_members(root: &Path) -> Vec<WorkspaceMember> {
    let Ok(raw) = fs::read_to_string(root.join("pnpm-workspace.yaml")) else {
        return Vec::new();
    };
    expand_member_globs(root, &yaml_list_after(&raw, "packages:"), "pnpm")
}

fn npm_workspace_members(root: &Path) -> Vec<WorkspaceMember> {
    let Ok(raw) = fs::read_to_string(root.join("package.json")) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    let patterns = match value.get("workspaces") {
        Some(serde_json::Value::Array(rows)) => rows
            .iter()
            .filter_map(|row| row.as_str().map(str::to_string))
            .collect::<Vec<_>>(),
        Some(serde_json::Value::Object(map)) => map
            .get("packages")
            .and_then(serde_json::Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| row.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    expand_member_globs(root, &patterns, "npm")
}

fn expand_member_globs(root: &Path, patterns: &[String], kind: &str) -> Vec<WorkspaceMember> {
    let mut out = Vec::new();
    for pattern in patterns {
        let pattern = pattern.trim().trim_matches('\'').trim_matches('"');
        if pattern.is_empty() || pattern.starts_with('!') {
            continue;
        }
        if let Some(prefix) = pattern.strip_suffix("/**") {
            push_child_packages(root, prefix, kind, &mut out, 2);
            continue;
        }
        if let Some(prefix) = pattern.strip_suffix("/*") {
            push_child_packages(root, prefix, kind, &mut out, 1);
            continue;
        }
        if root.join(pattern).is_dir() {
            out.push(WorkspaceMember {
                path: pattern.trim_end_matches('/').to_string(),
                kind: kind.to_string(),
            });
        }
    }
    out
}

fn push_child_packages(
    root: &Path,
    prefix: &str,
    kind: &str,
    out: &mut Vec<WorkspaceMember>,
    depth: usize,
) {
    let dir = if prefix.is_empty() {
        root.to_path_buf()
    } else {
        root.join(prefix)
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        if is_package_dir(&path) {
            out.push(WorkspaceMember {
                path: rel,
                kind: kind.to_string(),
            });
        } else if depth > 1 {
            push_child_packages(root, &rel, kind, out, depth - 1);
        }
        if out.len() >= MAX_WORKSPACE_MEMBERS {
            return;
        }
    }
}

fn is_package_dir(path: &Path) -> bool {
    path.join("Cargo.toml").is_file()
        || path.join("package.json").is_file()
        || path.join("pyproject.toml").is_file()
}

fn yaml_list_after(raw: &str, header: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut take = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if trimmed == header {
            take = true;
            continue;
        }
        if take {
            if let Some(item) = trimmed.strip_prefix("- ") {
                out.push(item.trim().to_string());
            } else if !line.starts_with(' ') && !line.starts_with('\t') {
                break;
            }
        }
    }
    out
}

fn push_unique(into: &mut Vec<WorkspaceMember>, extra: Vec<WorkspaceMember>) {
    for member in extra {
        if !into.iter().any(|seen| seen.path == member.path) {
            into.push(member);
        }
    }
}

fn try_create_lock(path: &Path) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let body = format!("pid={}\nstarted={started}\n", std::process::id());
    file.write_all(body.as_bytes())?;
    Ok(())
}

fn steal_stale_lock(path: &Path, stale_after: Duration) -> bool {
    let Ok(raw) = fs::read_to_string(path) else {
        return false;
    };
    let pid = lock_field(&raw, "pid").and_then(|value| value.parse::<u32>().ok());
    let started = lock_field(&raw, "started").and_then(|value| value.parse::<u64>().ok());
    let age = started
        .and_then(|secs| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH + Duration::from_secs(secs))
                .ok()
        })
        .unwrap_or(Duration::ZERO);
    let dead = pid.is_some_and(|pid| !pid_alive(pid));
    if dead || age >= stale_after {
        let _ = fs::remove_file(path);
        return true;
    }
    false
}

fn lock_field<'a>(raw: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}=");
    raw.lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(str::trim)
}

fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    if pid == std::process::id() {
        return true;
    }
    #[cfg(unix)]
    {
        // Safety: signal 0 is a documented existence probe and does not deliver.
        let rc = unsafe { kill_bind::kill(pid as i32, 0) };
        rc == 0
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

#[cfg(unix)]
mod kill_bind {
    #[link(name = "c")]
    unsafe extern "C" {
        pub fn kill(pid: i32, sig: i32) -> i32;
    }
}

fn lock_wait_secs() -> u64 {
    env_u64("LEIO_LOCK_WAIT_SECS").unwrap_or(DEFAULT_LOCK_WAIT_SECS)
}

fn lock_stale_secs() -> u64 {
    env_u64("LEIO_LOCK_STALE_SECS").unwrap_or(DEFAULT_LOCK_STALE_SECS)
}

fn env_u64(key: &str) -> Option<u64> {
    env_nonempty(key)?.parse().ok()
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn path_stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .or_else(|| Path::new(path).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

fn rel_or_name(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|rel| rel.display().to_string())
        .unwrap_or_else(|_| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn sanitize_session_slug_strips_unsafe() {
        assert_eq!(
            sanitize_session_slug("claude:abc/def ghi"),
            "claude-abc-def-ghi"
        );
        assert_eq!(sanitize_session_slug("///"), "");
        assert!(sanitize_session_slug(&"x".repeat(80)).len() <= 64);
    }

    #[test]
    fn write_atomic_replaces_without_leftover_tmp() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("index.json");
        write_atomic(&path, br#"{"a":1}"#).unwrap();
        write_atomic(&path, br#"{"a":2}"#).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"a":2}"#);
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|row| row.ok())
            .filter(|row| {
                row.file_name()
                    .to_string_lossy()
                    .contains(".index.json.tmp-")
            })
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn lock_is_reentrant_and_releases() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = acquire_lock(dir.path(), "index").unwrap();
        let second = acquire_lock(dir.path(), "index").unwrap();
        let lock_path = dir.path().join(".leio-code").join("index.lock");
        assert!(lock_path.is_file());
        drop(second);
        assert!(lock_path.is_file());
        drop(first);
        assert!(!lock_path.is_file());
    }

    #[test]
    fn stale_lock_with_dead_pid_is_stolen() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join(".leio-code")).unwrap();
        let lock_path = dir.path().join(".leio-code").join("index.lock");
        fs::write(&lock_path, "pid=999999\nstarted=1\n").unwrap();
        let _guard = acquire_lock(dir.path(), "index").unwrap();
        let body = fs::read_to_string(&lock_path).unwrap();
        assert!(
            body.contains(&format!("pid={}", std::process::id())),
            "{body}"
        );
    }

    #[test]
    fn nav_session_path_uses_slug() {
        let dir = tempfile::tempdir().expect("tempdir");
        let isolated = nav_session_path_for(dir.path(), Some("agent-1"));
        assert!(
            isolated.ends_with("sessions/nav-agent-1.json"),
            "{}",
            isolated.display()
        );
        let shared = nav_session_path_for(dir.path(), None);
        assert!(shared.ends_with("nav-session.json"), "{}", shared.display());
    }

    #[test]
    fn workspace_members_from_cargo_and_pnpm() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("crates/alpha")).unwrap();
        fs::write(
            dir.path().join("crates/alpha/Cargo.toml"),
            "[package]\nname=\"alpha\"\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("apps/web")).unwrap();
        fs::write(
            dir.path().join("apps/web/package.json"),
            r#"{"name":"web"}"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("pnpm-workspace.yaml"),
            "packages:\n  - 'apps/*'\n",
        )
        .unwrap();
        let members = workspace_members(dir.path());
        let paths: Vec<&str> = members.iter().map(|row| row.path.as_str()).collect();
        assert!(paths.contains(&"crates/alpha"), "{paths:?}");
        assert!(paths.contains(&"apps/web"), "{paths:?}");
    }

    #[test]
    fn write_atomic_json_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("formal.json");
        write_atomic_json(&path, &json!({"triples": 3})).unwrap();
        let value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["triples"], 3);
    }

    fn nav_session_path_for(repo_root: &Path, slug: Option<&str>) -> PathBuf {
        match slug {
            Some(slug) => repo_root
                .join(".leio-code")
                .join("sessions")
                .join(format!("nav-{slug}.json")),
            None => repo_root.join(".leio-code").join("nav-session.json"),
        }
    }
}
