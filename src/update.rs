//! `leio-code update` — sync the installation from the canonical checkout.
//!
//! One command instead of the per-harness dance:
//! 1. locate the leio-code checkout (`LEIO_CODE_ROOT`, else the compile-time
//!    source path when it is still a valid checkout),
//! 2. refuse to touch a dirty tree, `git pull --ff-only origin main`,
//! 3. `cargo build --release` and install `leio-code` + `leio-harness` into
//!    `DEST_DIR` (default `~/.cargo/bin`), atomically,
//! 4. refresh the harness plugin caches (Codex, Claude Code) through their
//!    own CLIs when present — `--no-plugins` skips this.
//!
//! Every harness surfaces the same artifacts: MCP launches resolve the
//! binary from `~/.cargo/bin`, and the plugin caches re-snapshot the
//! checkout, so this one pass updates everything.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::model::{EvidenceItem, QueryEnvelope};

/// A completed update step, reported as an entity.
struct Step {
    name: &'static str,
    detail: String,
}

/// True when `path` looks like the leio-code source checkout.
fn is_checkout(path: &Path) -> bool {
    if !path.join(".git").exists() {
        return false;
    }
    let Ok(manifest) = std::fs::read_to_string(path.join("Cargo.toml")) else {
        return false;
    };
    manifest.contains("name = \"leio-code\"")
}

/// Resolve the leio-code checkout (`LEIO_CODE_ROOT`, else the compile-time
/// source path). Shared with the self-contract freshness check.
pub fn locate_checkout() -> Result<PathBuf> {
    if let Ok(root) = std::env::var("LEIO_CODE_ROOT") {
        let root = PathBuf::from(root.trim());
        if is_checkout(&root) {
            return Ok(root);
        }
        bail!(
            "LEIO_CODE_ROOT={} is not a leio-code checkout",
            root.display()
        );
    }
    let baked = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if is_checkout(&baked) {
        return Ok(baked);
    }
    bail!(
        "could not locate the leio-code checkout; set LEIO_CODE_ROOT to its \
         path (a clone with .git and Cargo.toml)"
    );
}

fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .with_context(|| "spawn git")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// Verify the routing artifacts actually installed, even for same-version refreshes.
fn verify_instruction_snapshot(checkout: &Path, installed: &Path) -> Result<()> {
    for relative in [
        "skills/leio-code/SKILL.md",
        "hooks/session-context.sh",
        "mcp/index.js",
    ] {
        let expected = std::fs::read(checkout.join(relative))
            .with_context(|| format!("read canonical {relative}"))?;
        let actual = std::fs::read(installed.join(relative))
            .with_context(|| format!("read installed {relative}"))?;
        if actual != expected {
            bail!("installed {relative} differs from the checkout; refresh the plugin cache");
        }
    }
    Ok(())
}

/// Run a harness CLI refresh step; failures become warnings, never aborts.
fn refresh_harness_plugins(
    checkout: &Path,
    steps: &mut Vec<Step>,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) {
    if let Some(codex) = on_path("codex") {
        // `add` refreshes the snapshot in place; avoid a remove/install gap
        // while existing sessions still hold hook paths into the cache.
        match Command::new(&codex)
            .args(["plugin", "add", "leio-code@leio-code", "--json"])
            .output()
        {
            Ok(output) if output.status.success() => {
                let verified = serde_json::from_slice::<serde_json::Value>(&output.stdout)
                    .context("parse Codex plugin install result")
                    .and_then(|result| {
                        let installed = result
                            .get("installedPath")
                            .and_then(|v| v.as_str())
                            .context("Codex did not report installedPath")?;
                        verify_instruction_snapshot(checkout, Path::new(installed))
                    });
                if let Err(error) = verified {
                    warnings.push(format!(
                        "codex plugin instruction verification failed: {error:#}"
                    ));
                } else {
                    steps.push(Step { name: "plugins_codex_verified",
                        detail: "installed skill, session hook and MCP entrypoint match the checkout; reopen sessions to reload instructions".to_string() });
                }
                steps.push(Step {
                    name: "plugins_codex",
                    detail: "codex plugin add completed; see instruction verification result"
                        .to_string(),
                });
                evidence.push(EvidenceItem {
                    kind: "plugin_refresh".to_string(),
                    path: codex.display().to_string(),
                    line: None,
                    detail: "codex plugin leio-code@leio-code reinstalled".to_string(),
                });
            }
            Ok(output) => warnings.push(format!(
                "codex plugin refresh failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )),
            Err(err) => warnings.push(format!("codex plugin refresh failed: {err}")),
        }
    }
    if let Some(claude) = on_path("claude") {
        let _ = Command::new(&claude)
            .args(["plugin", "uninstall", "leio-code@leio-code"])
            .output();
        let _ = Command::new(&claude)
            .args(["plugin", "marketplace", "update", "leio-code"])
            .output();
        match Command::new(&claude)
            .args(["plugin", "install", "leio-code@leio-code"])
            .output()
        {
            Ok(output) if output.status.success() => {
                steps.push(Step {
                    name: "plugins_claude",
                    detail: "claude code plugin cache re-snapshotted from the checkout".to_string(),
                });
                evidence.push(EvidenceItem {
                    kind: "plugin_refresh".to_string(),
                    path: claude.display().to_string(),
                    line: None,
                    detail: "claude plugin leio-code@leio-code reinstalled".to_string(),
                });
            }
            Ok(output) => warnings.push(format!(
                "claude plugin refresh failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )),
            Err(err) => warnings.push(format!("claude plugin refresh failed: {err}")),
        }
    }
}

/// Remove build/ and VCS fat that harness installers snapshot from the
/// checkout. A plugin cache only needs sources, `mcp/`, skills, and the
/// wheel `artifacts/`; `target/` and `.git/` are dead weight measured in
/// gigabytes inside the cache copies.
fn prune_cache_fat(cache_root: &Path) {
    fn recurse(dir: &Path, depth: u8) {
        if depth > 4 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !file_type.is_dir() {
                continue;
            }
            if name == "target" || name == ".git" {
                let _ = std::fs::remove_dir_all(&path);
            } else {
                recurse(&path, depth + 1);
            }
        }
    }
    recurse(cache_root, 0);
}

/// Cache roots the two harness CLIs snapshot plugins into.
fn plugin_cache_roots() -> [PathBuf; 2] {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/root"));
    [
        home.join(".codex/plugins/cache/leio-code"),
        home.join(".claude/plugins/cache/leio-code"),
    ]
}

/// Install `source` over `dest` atomically (write a sibling temp, rename).
fn install_binary(source: &Path, dest: &Path) -> Result<()> {
    let tmp = dest.with_extension("tmp-update");
    std::fs::copy(source, &tmp)
        .with_context(|| format!("copy {} -> {}", source.display(), tmp.display()))?;
    std::fs::rename(&tmp, dest).with_context(|| format!("publish {}", dest.display()))?;
    Ok(())
}

/// Run the whole update pass and return its envelope.
pub fn run_update(force: bool, refresh_plugins: bool) -> Result<QueryEnvelope> {
    let started = Instant::now();
    let mut steps: Vec<Step> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut evidence: Vec<EvidenceItem> = Vec::new();

    let checkout = locate_checkout()?;
    steps.push(Step {
        name: "checkout",
        detail: checkout.display().to_string(),
    });

    let dirty = git(&checkout, &["status", "--porcelain"])?;
    if !dirty.is_empty() {
        bail!(
            "checkout {} has uncommitted changes; commit or stash them before updating",
            checkout.display()
        );
    }
    let branch = git(&checkout, &["symbolic-ref", "--short", "HEAD"])?;
    if branch != "main" {
        bail!("checkout is on branch `{branch}`; `leio-code update` only fast-forwards `main`");
    }

    git(&checkout, &["fetch", "origin"])?;
    let head_before = git(&checkout, &["rev-parse", "HEAD"])?;
    let remote = git(&checkout, &["rev-parse", "origin/main"])?;
    let changed = head_before != remote;

    let mut built = false;
    if changed {
        let pulled = git(&checkout, &["pull", "--ff-only", "origin", "main"])?;
        steps.push(Step {
            name: "pull",
            detail: pulled,
        });
    } else if !force {
        steps.push(Step {
            name: "pull",
            detail: "already current on origin/main".to_string(),
        });
    }

    let head_after = git(&checkout, &["rev-parse", "HEAD"])?;
    if changed || force {
        // Docs-only pulls may leave cargo's cache fully fresh, which would
        // keep the binary's baked build label on the previous commit and the
        // freshness doctor nagging forever. Rewrite build.rs in place (a
        // no-op content change bumps its mtime) so the label always
        // refreshes on update builds.
        let build_script = checkout.join("build.rs");
        if let Ok(source) = std::fs::read_to_string(&build_script) {
            let _ = std::fs::write(&build_script, source);
        }

        let cargo = on_path("cargo")
            .or_else(|| Some(home_cargo_bin().join("cargo")))
            .context("cargo not found on PATH")?;
        let build = Command::new(&cargo)
            .current_dir(&checkout)
            .args([
                "build",
                "--release",
                "-p",
                "leio-code",
                "-p",
                "leio-harness",
            ])
            .output()
            .with_context(|| "spawn cargo build")?;
        if !build.status.success() {
            bail!(
                "release build failed: {}",
                String::from_utf8_lossy(&build.stderr)
                    .lines()
                    .rev()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or("(no output)")
            );
        }
        built = true;
        steps.push(Step {
            name: "build",
            detail: "cargo build --release -p leio-code -p leio-harness".to_string(),
        });

        let dest_dir = std::env::var("DEST_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home_cargo_bin());
        std::fs::create_dir_all(&dest_dir)?;
        let target_dir = std::env::var("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| checkout.join("target"));
        install_binary(
            &target_dir.join("release/leio-code"),
            &dest_dir.join("leio-code"),
        )?;
        install_binary(
            &target_dir.join("release/leio-harness"),
            &dest_dir.join("leio-harness"),
        )?;
        steps.push(Step {
            name: "install",
            detail: format!("{}", dest_dir.display()),
        });
        evidence.push(EvidenceItem {
            kind: "binary".to_string(),
            path: dest_dir.join("leio-code").display().to_string(),
            line: None,
            detail: "installed binary".to_string(),
        });
        evidence.push(EvidenceItem {
            kind: "binary".to_string(),
            path: dest_dir.join("leio-harness").display().to_string(),
            line: None,
            detail: "installed binary".to_string(),
        });
    }

    let version = if built {
        let dest_dir = std::env::var("DEST_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home_cargo_bin());
        Command::new(dest_dir.join("leio-code"))
            .arg("--version")
            .output()
            .ok()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .filter(|text| !text.is_empty())
    } else {
        None
    };

    if refresh_plugins {
        refresh_harness_plugins(&checkout, &mut steps, &mut warnings, &mut evidence);
        let mut pruned = Vec::new();
        for root in plugin_cache_roots() {
            if root.is_dir() {
                prune_cache_fat(&root);
                pruned.push(root.display().to_string());
            }
        }
        if !pruned.is_empty() {
            steps.push(Step {
                name: "prune_caches",
                detail: format!("removed target/ and .git/ from {}", pruned.join(", ")),
            });
        }
    }

    let summary = if built {
        format!(
            "updated {} -> {} ({}){}",
            &head_before[..12.min(head_before.len())],
            &head_after[..12.min(head_after.len())],
            version.as_deref().unwrap_or("version unknown"),
            if refresh_plugins {
                " + refreshed harness plugins"
            } else {
                ""
            }
        )
    } else {
        "checkout already current; nothing rebuilt".to_string()
    };

    Ok(QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: format!(
            "update-{}",
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ),
        kind: "update".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.99 } else { 0.8 },
        entities: steps
            .iter()
            .map(|step| json!({ "step": step.name, "detail": step.detail }))
            .collect(),
        evidence,
        warnings,
        meta: Some(json!({
            "checkout": checkout.display().to_string(),
            "head_before": head_before,
            "head_after": head_after,
            "built": built,
            "plugins_refreshed": refresh_plugins,
        })),
        timing_ms: started.elapsed().as_millis(),
    })
}

fn home_cargo_bin() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/root"))
        .join(".cargo/bin")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruction_snapshot_detects_same_version_staleness_and_missing_hooks() {
        let root = tempfile::tempdir().unwrap();
        let checkout = root.path().join("checkout");
        let installed = root.path().join("installed");
        for relative in [
            "skills/leio-code/SKILL.md",
            "hooks/session-context.sh",
            "mcp/index.js",
        ] {
            for base in [&checkout, &installed] {
                let file = base.join(relative);
                std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                std::fs::write(file, "current").unwrap();
            }
        }
        assert!(verify_instruction_snapshot(&checkout, &installed).is_ok());
        std::fs::write(installed.join("skills/leio-code/SKILL.md"), "old").unwrap();
        assert!(verify_instruction_snapshot(&checkout, &installed).is_err());
        std::fs::write(installed.join("skills/leio-code/SKILL.md"), "current").unwrap();
        std::fs::remove_file(installed.join("hooks/session-context.sh")).unwrap();
        assert!(verify_instruction_snapshot(&checkout, &installed).is_err());
    }

    #[test]
    fn checkout_detection_requires_git_and_manifest() {
        let dir = tempfile_dir();
        assert!(!is_checkout(&dir), "empty dir is not a checkout");

        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "name = \"other\"").unwrap();
        assert!(!is_checkout(&dir), "wrong crate is not a checkout");

        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"leio-code\"\n").unwrap();
        assert!(is_checkout(&dir), "git + leio-code manifest is a checkout");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tempfile_dir() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("leio-code-update-tests-{nanos}"))
    }
}
