//! Incremental file-watcher driving `build_or_update_index` on debounced
//! changes. Backs the `leio-code watch` subcommand.
//!
//! The watcher must NOT react to its own writes under `.leio-code/`
//! (where the JSON index and Arrow IPC search sidecar live), otherwise the
//! index save retriggers the watcher in a tight loop.
//!
//! Two entry points:
//!
//! - [`run_watch`] — blocking CLI loop used by `main.rs`.
//! - [`WatcherSession`] — testable handle that drives the same debounce
//!   and filter pipeline through a synchronous `wait_for_next_reindex`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::indexer::build_or_update_index;

/// Tuning knobs for the watcher.
#[derive(Debug, Clone)]
pub struct WatchOptions {
    /// How long to coalesce events before firing a reindex.
    pub debounce_ms: u64,
    /// Suppress the per-reindex `[watch] ...` line on stderr.
    pub quiet: bool,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            debounce_ms: 500,
            quiet: false,
        }
    }
}

/// One observable reindex.
#[derive(Debug, Clone)]
pub struct ReindexEvent {
    /// Path that woke the debounce window (first surviving event in the batch).
    pub trigger_path: PathBuf,
    /// Number of files in the rebuilt index.
    pub files_indexed: usize,
    /// Wall-clock duration of the reindex call.
    pub elapsed_ms: u128,
}

/// Long-lived background session. Drop to stop watching.
pub struct WatcherSession {
    // Holds the OS watcher alive. Dropping it stops event delivery.
    _watcher: RecommendedWatcher,
    // Holds the debounce thread alive. The thread exits when `_shutdown_tx`
    // is dropped (i.e. when this session is dropped).
    _debounce_thread: Option<thread::JoinHandle<()>>,
    // Closed-on-drop sentinel that signals the debounce thread to exit.
    _shutdown_tx: Sender<()>,
    // Receives one `ReindexEvent` per fired reindex.
    reindex_rx: Receiver<ReindexEvent>,
    // Cumulative count of fired reindexes; bumped before pushing to `reindex_rx`.
    reindex_count: Arc<AtomicU64>,
}

impl WatcherSession {
    /// Start watching `root` recursively. Returns once the OS watcher and
    /// debounce thread are running.
    pub fn new(root: &Path, index_path: &Path, opts: WatchOptions) -> Result<Self> {
        // Canonicalize the root once so we can compare event paths reliably.
        // On macOS, tempdirs live under /var/folders/... which is a symlink
        // to /private/var/folders/..., and FSEvent reports the /private/...
        // form. Without this, our prefix filter for `.leio-code/` misses.
        let canonical_root = root
            .canonicalize()
            .with_context(|| format!("failed to canonicalize watch root {}", root.display()))?;
        let index_path = index_path.to_path_buf();
        let debounce = Duration::from_millis(opts.debounce_ms);

        // mpsc from the notify callback into the debounce thread.
        let (event_tx, event_rx) = mpsc::channel::<Event>();
        // mpsc out: debounce thread -> public API.
        let (reindex_tx, reindex_rx) = mpsc::channel::<ReindexEvent>();
        // Shutdown sentinel: when its sender drops, the debounce loop exits.
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();

        let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                // Best-effort: if the receiver is gone we are shutting down.
                let _ = event_tx.send(event);
            }
        })
        .context("failed to create filesystem watcher")?;
        watcher
            .watch(&canonical_root, RecursiveMode::Recursive)
            .with_context(|| format!("failed to start watching {}", canonical_root.display()))?;

        let reindex_count = Arc::new(AtomicU64::new(0));
        let count_for_thread = Arc::clone(&reindex_count);
        let root_for_thread = canonical_root.clone();
        let quiet = opts.quiet;

        let debounce_thread = thread::Builder::new()
            .name("leio-watch-debounce".to_string())
            .spawn(move || {
                debounce_loop(
                    root_for_thread,
                    index_path,
                    debounce,
                    quiet,
                    event_rx,
                    shutdown_rx,
                    reindex_tx,
                    count_for_thread,
                );
            })
            .context("failed to spawn debounce thread")?;

        Ok(Self {
            _watcher: watcher,
            _debounce_thread: Some(debounce_thread),
            _shutdown_tx: shutdown_tx,
            reindex_rx,
            reindex_count,
        })
    }

    /// Block up to `timeout` waiting for the next debounced reindex.
    /// Returns `None` if no reindex fires within the window.
    pub fn wait_for_next_reindex(&self, timeout: Duration) -> Option<ReindexEvent> {
        self.reindex_rx.recv_timeout(timeout).ok()
    }

    /// Cumulative reindexes since session start.
    pub fn reindex_count(&self) -> u64 {
        self.reindex_count.load(Ordering::SeqCst)
    }
}

/// CLI entry point: watches `root`, reindexes on changes, prints to stderr.
/// Loops until the process is killed (Ctrl-C / SIGINT).
pub fn run_watch(root: &Path, index_path: &Path, opts: WatchOptions) -> Result<()> {
    let quiet = opts.quiet;
    let session = WatcherSession::new(root, index_path, opts)?;
    if !quiet {
        eprintln!("[watch] watching {} (Ctrl-C to stop)", root.display());
    }
    loop {
        if let Some(event) = session.wait_for_next_reindex(Duration::from_secs(60)) {
            let _ = crate::jsonld::record_event(root, &watch_envelope(root, &event));
            if !quiet {
                eprintln!(
                    "[watch] reindexed {} files in {} ms (trigger: {})",
                    event.files_indexed,
                    event.elapsed_ms,
                    event.trigger_path.display()
                );
            }
        }
    }
}

fn watch_envelope(root: &Path, event: &ReindexEvent) -> crate::model::QueryEnvelope {
    let trigger = event.trigger_path.display().to_string();
    crate::model::QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: format!(
            "watch-{}",
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ),
        kind: "watch".to_string(),
        summary: format!("reindexed {} files after {}", event.files_indexed, trigger),
        confidence: 1.0,
        entities: vec![serde_json::json!({
            "path": trigger,
            "files_indexed": event.files_indexed,
        })],
        evidence: vec![crate::model::EvidenceItem {
            kind: "watch_trigger".to_string(),
            path: trigger,
            line: None,
            detail: format!("{} files", event.files_indexed),
        }],
        warnings: Vec::new(),
        meta: Some(serde_json::json!({
            "repo": root.display().to_string(),
            "elapsed_ms": event.elapsed_ms,
        })),
        timing_ms: event.elapsed_ms,
    }
}

/// Return true if `path` should be ignored (under `.leio-code/`, `target/`,
/// or `.git/`). Inputs may be absolute or already-relative.
fn is_ignored(root: &Path, path: &Path) -> bool {
    // Try to relativize. If `path` is not under `root` we still defensively
    // check the raw path, in case notify reported an absolute path that we
    // couldn't fully canonicalize (e.g. parent of a deleted file).
    let rel = path.strip_prefix(root).unwrap_or(path);
    let mut components = rel.components();
    match components.next() {
        Some(c) => {
            let s = c.as_os_str();
            s == ".leio-code" || s == "target" || s == ".git"
        }
        None => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn debounce_loop(
    root: PathBuf,
    index_path: PathBuf,
    debounce: Duration,
    quiet: bool,
    event_rx: Receiver<Event>,
    shutdown_rx: Receiver<()>,
    reindex_tx: Sender<ReindexEvent>,
    reindex_count: Arc<AtomicU64>,
) {
    loop {
        // Shutdown is signalled by the sender being dropped (channel
        // disconnected). A `try_recv` returning Disconnected means the
        // session was dropped; anything else means keep going.
        use std::sync::mpsc::TryRecvError;
        match shutdown_rx.try_recv() {
            Err(TryRecvError::Disconnected) => return,
            // `Ok(_)` cannot happen — we never send into shutdown_rx —
            // but treat it as "keep going" for completeness.
            Ok(_) | Err(TryRecvError::Empty) => {}
        }
        let first = match event_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(ev) => ev,
            Err(RecvTimeoutError::Timeout) => continue,
            // Sender dropped: session is being torn down.
            Err(RecvTimeoutError::Disconnected) => return,
        };

        let Some(trigger) = first_interesting_path(&root, &first) else {
            continue;
        };

        // Coalesce: drain any further events that arrive within `debounce`.
        let deadline = Instant::now() + debounce;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match event_rx.recv_timeout(remaining) {
                Ok(_) => {
                    // Ignore content — we already have a trigger and we'll
                    // reindex the whole tree anyway.
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }

        // Fire the reindex. Failures are logged but don't kill the watcher.
        let started = Instant::now();
        match build_or_update_index(&root, &index_path, quiet) {
            Ok(index) => {
                let elapsed_ms = started.elapsed().as_millis();
                reindex_count.fetch_add(1, Ordering::SeqCst);
                let event = ReindexEvent {
                    trigger_path: trigger,
                    files_indexed: index.files.len(),
                    elapsed_ms,
                };
                // Receiver may be gone if the session was just dropped.
                if reindex_tx.send(event).is_err() {
                    return;
                }
            }
            Err(err) => {
                if !quiet {
                    eprintln!("[watch] reindex failed: {err:#}");
                }
            }
        }
    }
}

/// Extract the first path from `event` that survives the ignore filter.
/// `notify` events can carry multiple paths (e.g. renames). We canonicalize
/// each (best-effort) so the macOS `/var` vs `/private/var` discrepancy
/// can't smuggle a `.leio-code/` write past the filter.
fn first_interesting_path(root: &Path, event: &Event) -> Option<PathBuf> {
    // Skip pure-metadata access events; they're noisy and never indicate a
    // tree change. Modify/Create/Remove are the ones we care about.
    if matches!(event.kind, EventKind::Access(_)) {
        return None;
    }
    for raw in &event.paths {
        let canonical = raw.canonicalize().unwrap_or_else(|_| raw.clone());
        if is_ignored(root, &canonical) {
            continue;
        }
        return Some(canonical);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignore_filter_catches_dot_leio_code() {
        let root = Path::new("/tmp/repo");
        assert!(is_ignored(
            root,
            Path::new("/tmp/repo/.leio-code/index.json")
        ));
        assert!(is_ignored(root, Path::new("/tmp/repo/target/debug/foo")));
        assert!(is_ignored(root, Path::new("/tmp/repo/.git/HEAD")));
        assert!(!is_ignored(root, Path::new("/tmp/repo/src/lib.rs")));
    }
}
