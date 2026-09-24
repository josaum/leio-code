//! CLI bus policy. Explicit endpoints fail closed; the default is a private,
//! durable Unix socket shared by local runs. A file lock serializes startup.
use crate::{
    bus::new_embedding_row,
    bus_client::BusClient,
    model::{RunResult, RunSpec, RunStatus},
    process,
};
use anyhow::{Context, Result, bail};
use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

pub fn directory() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("LEIO_HARNESS_BUS_DIR") {
        return Ok(PathBuf::from(path));
    }
    Ok(
        PathBuf::from(std::env::var_os("HOME").context("HOME missing; set LEIO_HARNESS_BUS_DIR")?)
            .join(".local/share/leio-harness/bus"),
    )
}

pub async fn ensure(explicit: Option<String>, disabled: bool) -> Result<Option<String>> {
    if disabled {
        return Ok(None);
    }
    if let Some(addr) = explicit.or_else(|| process::inherited_value("LEIO_HARNESS_BUS")) {
        let mut client = BusClient::connect(&addr)
            .await
            .context("configured bus unavailable")?;
        client
            .health()
            .await
            .context("configured endpoint is not a healthy bus")?;
        return Ok(Some(addr));
    }
    let dir = directory()?;
    anyhow::ensure!(
        dir.is_absolute() && dir.parent().is_some(),
        "bus directory must be an absolute private directory"
    );
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        let meta = std::fs::symlink_metadata(&dir)?;
        // SAFETY: geteuid has no arguments or memory preconditions.
        let uid = unsafe { nix::libc::geteuid() };
        anyhow::ensure!(
            meta.file_type().is_dir() && meta.uid() == uid && meta.mode() & 0o077 == 0,
            "bus directory must be owned by this user, mode 0700, and not a symlink: {}",
            dir.display()
        );
    }
    let addr = dir.join("bus.sock").to_string_lossy().into_owned();
    anyhow::ensure!(
        addr.len() < 100,
        "default bus socket path is too long; set LEIO_HARNESS_BUS_DIR to a shorter absolute path"
    );
    // try_lock + asynchronous delay never block a Tokio worker on another starter.
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("startup.lock"))?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match fs2::FileExt::try_lock_exclusive(&lock) {
            Ok(()) => break,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(50)).await
            }
            Err(e) => return Err(e).context("managed bus startup lock"),
        }
    }
    if let Ok(mut client) = BusClient::connect(&addr).await {
        client.health().await?;
        return Ok(Some(addr));
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("server.log"))?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["bus", "serve", "--bind", &addr, "--persist"])
        .arg(dir.join("bus.arrow"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // A daemon must not remain in an agent's timeout-killed process group.
        unsafe {
            command.pre_exec(|| {
                nix::unistd::setsid().map_err(std::io::Error::from)?;
                Ok(())
            });
        }
    }
    let mut child = command.spawn().context("start managed bus")?;
    for _ in 0..100 {
        if let Some(status) = child.try_wait()? {
            bail!(
                "managed bus exited {status}; inspect {}",
                dir.join("server.log").display()
            );
        }
        if let Ok(mut client) = BusClient::connect(&addr).await
            && client.health().await.is_ok()
        {
            std::fs::write(dir.join("server.pid"), child.id().to_string())?;
            // Reap while this parent lives; the OS adopts it when the CLI exits.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return Ok(Some(addr));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let _ = child.kill();
    let _ = child.wait();
    bail!(
        "managed bus startup timed out; inspect {}",
        dir.join("server.log").display()
    )
}

pub async fn run(mut spec: RunSpec, addr: Option<&str>) -> Result<RunResult> {
    let mut client = match addr {
        Some(addr) => Some(BusClient::connect(addr).await?),
        None => None,
    };
    let run_id = spec.run_id.clone();
    let agent_id = spec
        .env
        .get("LEIO_HARNESS_AGENT_ID")
        .cloned()
        .unwrap_or_else(|| run_id.clone());
    if let Some(client) = &mut client {
        for (key, value) in [
            ("LEIO_HARNESS_BUS", client.address()),
            ("LEIO_HARNESS_RUN_ID", &run_id),
            ("LEIO_HARNESS_AGENT_ID", &agent_id),
        ] {
            spec.env.insert(key.to_owned(), value.to_owned());
            if !spec.env_allowlist.is_empty() {
                spec.env_allowlist.push(key.to_owned());
            }
        }
        client
            .publish(vec![new_embedding_row(
                &agent_id,
                &run_id,
                &format!("intent/{agent_id}"),
                vec![0., 0., 0., 1.],
            )])
            .await
            .context("publish run intent")?;
    } else {
        // --no-bus must not leak an endpoint through an explicit spec.
        spec.env.remove("LEIO_HARNESS_BUS");
    }
    let mut result = tokio::task::spawn_blocking(move || process::run(spec))
        .await
        .context("process supervisor panicked")??;
    if let Some(client) = &mut client {
        let vector = match result.status {
            RunStatus::Passed => vec![1., 0., 0., 0.],
            RunStatus::Failed => vec![0., 1., 0., 0.],
            RunStatus::TimedOut => vec![0., 0., 1., 0.],
            _ => vec![0., 0., 0., 1.],
        };
        let delivery = client
            .publish(vec![new_embedding_row(
                &agent_id,
                &run_id,
                &format!("result/{agent_id}"),
                vector,
            )])
            .await;
        let receipt = match delivery {
            Ok(seq) => {
                serde_json::json!({"status":"acknowledged", "lastSeq":seq,"address":client.address()})
            }
            Err(error) => {
                let message = format!(
                    "bus result delivery failed: {error:#}; process status: {:?}; prior error: {:?}",
                    result.status, result.error
                );
                result.status = RunStatus::InfraError;
                result.error = Some(message.clone());
                serde_json::json!({"status":"failed", "error":message,"address":client.address()})
            }
        };
        let directory = std::path::Path::new(&result.stdout_path)
            .parent()
            .context("run directory missing")?;
        process::write_json_atomic(&directory.join("bus-delivery.json"), &receipt)?;
        process::write_json_atomic(&directory.join("result.json"), &result)?;
    }
    Ok(result)
}
