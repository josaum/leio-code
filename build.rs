//! Bake the source revision into the binary so staleness is detectable.
//!
//! `--version` and the self-contract freshness check compare this against the
//! checkout HEAD at *runtime*, so the stamp must name the commit the binary
//! was actually built from.
//!
//! Keeping that true requires a `rerun-if-changed` for every file that holds
//! the commit. With only `build.rs` declared, Cargo reuses the previous run's
//! values whenever `build.rs` itself is unchanged — so any commit that does
//! not touch this file produces a binary stamped with an earlier commit, and
//! the freshness check compares against a value that was never current. That
//! is the opposite of the intended signal: the stamp looked stale-proof while
//! being stale almost always.
//!
//! The cost is that moving HEAD re-runs this script, and switching branches
//! therefore relinks on the next build. An accurate stamp is not available
//! more cheaply: the commit cannot be re-read without re-running the reader.
//!
//! One limit remains, deliberately. `LEIO_BUILD_DIRTY` reflects the working
//! tree at the last run of this script, and no `rerun-if-changed` can track
//! "any tracked file differs from HEAD". Editing a source file flips the real
//! dirty state without re-running this script, so a `clean` stamp means
//! "clean when last stamped", not "clean now". The commit is the load-bearing
//! half; treat the dirty flag as a hint.

use std::env;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    emit_commit_rerun_triggers(&manifest);
    let commit = git_output(&manifest, &["rev-parse", "HEAD"])
        .map(|head| head[..12.min(head.len())].to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let dirty =
        git_output(&manifest, &["status", "--porcelain"]).is_some_and(|status| !status.is_empty());
    println!("cargo:rustc-env=LEIO_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=LEIO_BUILD_DIRTY={}", u8::from(dirty));
    let pkg_version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    println!(
        "cargo:rustc-env=LEIO_BUILD_VERSION={pkg_version} ({commit}{})",
        if dirty { ", dirty" } else { ", clean" }
    );
}

/// Declare the files whose contents decide what `git rev-parse HEAD` returns.
///
/// Paths come from `git rev-parse --git-path`, so linked worktrees and
/// submodules (where `.git` is a file, not a directory) resolve correctly
/// rather than being hardcoded to `.git/…`. Relative paths resolve against the
/// manifest directory, which is both this command's working directory and
/// where Cargo resolves a relative `rerun-if-changed`.
///
/// Outside a git checkout every lookup returns `None` and nothing is emitted,
/// leaving the stamp at `unknown` and the build unaffected.
fn emit_commit_rerun_triggers(manifest: &str) {
    // HEAD: the commit directly when detached, otherwise the ref name.
    if let Some(head) = git_output(manifest, &["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }

    // The branch ref HEAD names, which moves on every commit. `--quiet` keeps
    // a detached HEAD silent instead of erroring; there is no ref to watch.
    if let Some(reference) = git_output(manifest, &["symbolic-ref", "--quiet", "HEAD"])
        && let Some(path) = git_output(manifest, &["rev-parse", "--git-path", &reference])
    {
        println!("cargo:rerun-if-changed={path}");
    }

    // A packed ref has no loose file of its own, so watch the pack as well.
    // Cargo tolerates a declared path that does not exist.
    if let Some(packed) = git_output(manifest, &["rev-parse", "--git-path", "packed-refs"]) {
        println!("cargo:rerun-if-changed={packed}");
    }
}

fn git_output(dir: &str, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
