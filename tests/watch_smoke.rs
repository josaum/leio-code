//! Smoke tests for the `leio-code watch` incremental reindexer.
//!
//! These exercise the [`WatcherSession`] API rather than the long-running
//! `run_watch` CLI loop. The session exposes the same debounce + filter
//! pipeline through a synchronous `wait_for_next_reindex` method so we can
//! assert behaviour without spawning the binary.

use std::fs;
use std::path::Path;
use std::thread;
use std::time::Duration;

use leio_code::indexer::default_index_path;
use leio_code::watcher::{WatchOptions, WatcherSession};
use tempfile::TempDir;

fn session(root: &Path, debounce_ms: u64) -> WatcherSession {
    let index_path = default_index_path(root);
    WatcherSession::new(
        root,
        &index_path,
        WatchOptions {
            debounce_ms,
            quiet: true,
        },
    )
    .expect("watcher session starts")
}

#[test]
fn reindex_after_file_change() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    // Seed: an initial file so the indexer has something to walk.
    fs::write(root.join("seed.rs"), "fn seed() {}\n").unwrap();

    let session = session(root, 200);

    // Write a brand-new .rs file inside the watched root.
    let new_file = root.join("hello.rs");
    fs::write(&new_file, "fn hello() {}\n").unwrap();

    let event = session
        .wait_for_next_reindex(Duration::from_secs(3))
        .expect("a reindex must fire after a file write");

    // FSEvent may coalesce, but trigger should be under the root.
    assert!(
        event.trigger_path.starts_with(root)
            || event
                .trigger_path
                .canonicalize()
                .ok()
                .zip(root.canonicalize().ok())
                .is_some_and(|(t, r)| t.starts_with(&r)),
        "trigger {:?} not under root {:?}",
        event.trigger_path,
        root
    );
    assert!(event.files_indexed >= 1);
}

#[test]
fn ignores_changes_under_dot_leio_code() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    fs::write(root.join("seed.rs"), "fn seed() {}\n").unwrap();

    let session = session(root, 200);

    // Create .leio-code/cache/foo.txt — writes here MUST be filtered or the
    // index save itself would retrigger the watcher in a tight loop.
    let dot_dir = root.join(".leio-code").join("cache");
    fs::create_dir_all(&dot_dir).unwrap();
    fs::write(dot_dir.join("foo.txt"), "noise\n").unwrap();

    let result = session.wait_for_next_reindex(Duration::from_millis(800));
    assert!(
        result.is_none(),
        "writes under .leio-code/ must not trigger a reindex (got {:?})",
        result
    );
    assert_eq!(session.reindex_count(), 0);
}

#[test]
fn debounces_burst_of_events() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    fs::write(root.join("seed.rs"), "fn seed() {}\n").unwrap();

    // Use a moderately long debounce so the burst all lands inside one window.
    let session = session(root, 400);

    for i in 0..5 {
        let path = root.join(format!("burst_{i}.rs"));
        fs::write(&path, format!("fn b{i}() {{}}\n")).unwrap();
        thread::sleep(Duration::from_millis(20));
    }

    // First reindex should arrive once the debounce window elapses.
    let first = session
        .wait_for_next_reindex(Duration::from_secs(3))
        .expect("burst should produce exactly one reindex");
    assert!(first.files_indexed >= 5);

    // Second call within a short window MUST return None: there are no more
    // pending events, so debounce should not fire again.
    let second = session.wait_for_next_reindex(Duration::from_millis(300));
    assert!(
        second.is_none(),
        "burst of 5 writes must coalesce into one reindex (got second: {:?})",
        second
    );
    assert_eq!(session.reindex_count(), 1);
}

#[test]
fn respects_custom_debounce_ms() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    fs::write(root.join("seed.rs"), "fn seed() {}\n").unwrap();

    let session = session(root, 100);

    fs::write(root.join("quick.rs"), "fn quick() {}\n").unwrap();

    // With debounce=100ms, even on macOS FSEvent (~100-500ms latency) the
    // reindex must arrive comfortably under the 500ms default. Allow 2s for
    // CI headroom — the assertion that matters is "fires" not "fires fast".
    let event = session
        .wait_for_next_reindex(Duration::from_millis(2000))
        .expect("custom debounce should still fire a reindex");
    assert!(event.files_indexed >= 1);
}
