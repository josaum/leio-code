//! Optional, read-only format observations for an expanded Cargo coverage request.
//! Use the formatter declared by the checkout only when that toolchain is installed.
use super::super::{bounded_source, read_manifest};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

fn bounded_output(command: &mut Command, deadline: Instant) -> Option<(ExitStatus, String)> {
    if Instant::now() >= deadline {
        return None;
    }
    let mut out = tempfile::tempfile().ok()?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(out.try_clone().ok()?)
        .stderr(out.try_clone().ok()?)
        .spawn()
        .ok()?;
    const MAX_OUTPUT: u64 = 128 * 1024;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if out.metadata().ok()?.len() > MAX_OUTPUT {
                    return None;
                }
                out.rewind().ok()?;
                let mut data = String::new();
                out.take(MAX_OUTPUT + 1).read_to_string(&mut data).ok()?;
                return Some((status, data));
            }
            Ok(None)
                if Instant::now() < deadline
                    && out.metadata().is_ok_and(|m| m.len() <= MAX_OUTPUT) =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn edition(root: &Path, package: &Path) -> Option<String> {
    let manifest = read_manifest(root, package)?;
    let value = manifest.get("package")?.get("edition");
    let edition = match value {
        None => "2015".to_owned(),
        Some(v) if v.get("workspace").and_then(toml::Value::as_bool) == Some(true) => {
            package.ancestors().find_map(|dir| {
                read_manifest(root, dir)?
                    .get("workspace")?
                    .get("package")?
                    .get("edition")?
                    .as_str()
                    .map(str::to_owned)
            })?
        }
        Some(v) => v.as_str()?.to_owned(),
    };
    matches!(edition.as_str(), "2015" | "2018" | "2021" | "2024").then_some(edition)
}

pub(super) fn drift(
    root: &Path,
    sources: &BTreeMap<&Path, Vec<&str>>,
) -> Option<Vec<(String, String)>> {
    let config: toml::Value = bounded_source(root, Path::new("rust-toolchain.toml"))?
        .parse()
        .ok()?;
    let toolchain = config.get("toolchain")?;
    if !toolchain
        .get("components")?
        .as_array()?
        .iter()
        .any(|s| s.as_str() == Some("rustfmt"))
    {
        return None;
    }
    let pin = toolchain.get("channel")?.as_str()?;
    // Only exact installed releases. Neither a custom toolchain nor a repo-local
    // executable should run as a side effect of inspecting an arbitrary checkout.
    let mut parts = pin.split('.');
    if parts.clone().count() != 3
        || !parts.all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let rustup = std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|d| d.is_absolute())
        .filter_map(|d| d.canonicalize().ok())
        .filter(|d| !d.starts_with(root))
        .filter_map(|d| {
            d.join(format!("rustup{}", std::env::consts::EXE_SUFFIX))
                .canonicalize()
                .ok()
        })
        .find(|p| p.is_file() && !p.starts_with(root))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let (status, path) = bounded_output(
        Command::new(rustup)
            .current_dir(std::env::temp_dir())
            .env("RUSTUP_AUTO_INSTALL", "0")
            .args(["which", "--toolchain", pin, "rustfmt"]),
        deadline,
    )?;
    if !status.success() {
        return None;
    }
    let formatter = Path::new(path.trim());
    if !formatter.is_absolute() {
        return None;
    }
    let formatter = formatter.canonicalize().ok()?;
    if formatter.starts_with(root) || !formatter.is_file() {
        return None;
    }
    let mut findings = BTreeSet::new();
    for (&package, files) in sources {
        let edition = edition(root, package)?;
        // Keep the command line bounded on Windows too. Skip children because
        // only the already-scoped, live indexed inputs belong to this probe.
        for chunk in files.chunks(32) {
            let mut cmd = Command::new(&formatter);
            cmd.current_dir(root)
                .args([
                    "--check",
                    "--files-with-diff",
                    "--edition",
                    &edition,
                    "--config",
                    "skip_children=true",
                ])
                .args(chunk.iter().map(|p| Path::new(".").join(p)));
            let (status, stdout) = bounded_output(&mut cmd, deadline)?;
            if status.success() && stdout.trim().is_empty() {
                continue;
            }
            if status.code() != Some(1) || stdout.trim().is_empty() {
                return None;
            }
            findings.extend(reported_files(root, chunk, &stdout)?);
        }
    }
    Some(
        findings
            .into_iter()
            .map(|path| {
                (
                    path,
                    format!("pinned rustfmt {pin} --check reports formatting drift in this file"),
                )
            })
            .collect(),
    )
}

fn reported_files(root: &Path, submitted: &[&str], stdout: &str) -> Option<Vec<String>> {
    // A parse error or a path outside the submitted set invalidates the group.
    stdout
        .lines()
        .map(|line| {
            let path = root.join(line).canonicalize().ok()?;
            let relative = path.strip_prefix(root).ok()?.to_str()?.replace('\\', "/");
            submitted.contains(&relative.as_str()).then_some(relative)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_submitted_formatter_diagnostics_are_evidence() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for name in ["model.rs", "private.rs"] {
            std::fs::write(root.join(name), "").unwrap();
        }
        let input = ["model.rs"];
        let absolute = root.join("model.rs");
        assert_eq!(
            reported_files(&root, &input, absolute.to_str().unwrap()),
            Some(vec!["model.rs".into()])
        );
        assert!(reported_files(&root, &input, "model.rs\nprivate.rs").is_none());
        assert!(reported_files(&root, &input, "model.rs\nerror: syntax").is_none());
    }

    #[test]
    fn absent_or_unpinned_formatter_keeps_source_only_evidence() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let files = BTreeMap::new();
        assert!(drift(&root, &files).is_none());
        for manifest in [
            "[toolchain]\nchannel='stable'\ncomponents=['rustfmt']",
            "[toolchain]\nchannel='1.75.0'\ncomponents=['clippy']",
        ] {
            std::fs::write(root.join("rust-toolchain.toml"), manifest).unwrap();
            assert!(drift(&root, &files).is_none());
        }
    }

    #[cfg(unix)]
    #[test]
    fn slow_formatter_is_killed_and_reaped() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "exec sleep 20"]);
        let start = Instant::now();
        assert!(bounded_output(&mut cmd, start + Duration::from_millis(50)).is_none());
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
