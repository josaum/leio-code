//! Bake the source revision into the binary so staleness is detectable.
//!
//! `--version` and the self-contract freshness check compare this against the
//! checkout HEAD at *runtime*; the build script deliberately emits no
//! `rerun-if` triggers for `.git` so switching branches never forces a
//! rebuild — a binary that was not rebuilt keeps reporting the commit it was
//! actually built from, which is exactly the signal the check needs.

use std::env;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
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
