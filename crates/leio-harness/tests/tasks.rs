use serde_json::{Value, json};
use std::{
    path::Path,
    process::Command,
    time::{Duration, Instant},
};
use tempfile::TempDir;
fn fixture() -> TempDir {
    let t = TempDir::new().unwrap();
    std::fs::write(t.path().join(".gitignore"), ".leio-code/\n").unwrap();
    std::fs::write(t.path().join("code.txt"), "before\n").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Harness Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "baseline",
        ],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(t.path())
                .status()
                .unwrap()
                .success()
        );
    }
    t
}
fn cli(repo: &Path, args: &[&str]) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_leio-harness"))
        .args(["task"])
        .args(args)
        .arg("--repo")
        .arg(repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
fn submit(repo: &Path, worker: Value, verify: Value, timeout: u64) -> Value {
    cli(
        repo,
        &[
            "submit",
            "--task",
            "fixture task; do not commit",
            "--worker",
            "fixture",
            "--argv",
            &worker.to_string(),
            "--verify-argv",
            &verify.to_string(),
            "--timeout-ms",
            &timeout.to_string(),
        ],
    )
}
fn wait(repo: &Path, id: &str) -> Value {
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        let v = cli(repo, &["status", "--run-id", id]);
        if matches!(
            v["state"].as_str(),
            Some("succeeded" | "failed" | "cancelled" | "blocked")
        ) {
            return v;
        }
        assert!(Instant::now() < until, "timed out: {v}");
        std::thread::sleep(Duration::from_millis(30));
    }
}
#[test]
fn task_change_verification_activity_and_event_pagination() {
    let repo = fixture();
    let s = submit(
        repo.path(),
        json!([
            "sh",
            "-c",
            "printf 'working\\n'; sleep .2; printf 'after\\n' > code.txt"
        ]),
        json!(["sh", "-c", "test \"$(cat code.txt)\" = after"]),
        3000,
    );
    let id = s["runId"].as_str().unwrap();
    let running = cli(repo.path(), &["status", "--run-id", id]);
    assert_eq!(running["state"], "running");
    assert_eq!(running["supervisorAlive"], true);
    assert!(running["workerPid"].is_number());
    let r = wait(repo.path(), id);
    assert_eq!(r["state"], "succeeded");
    assert_eq!(r["verificationResult"]["status"], "passed");
    assert_eq!(r["supervisorAlive"], false);
    assert!(
        std::fs::read_to_string(r["diffPath"].as_str().unwrap())
            .unwrap()
            .contains("+after")
    );
    let e = cli(repo.path(), &["events", "--run-id", id, "--limit", "2"]);
    assert_eq!(e["events"].as_array().unwrap().len(), 2);
    assert_eq!(e["hasMore"], true);
    let e2 = cli(
        repo.path(),
        &[
            "events",
            "--run-id",
            id,
            "--after",
            &e["nextAfter"].to_string(),
        ],
    );
    assert!(
        e2["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["kind"] == "output")
    );
    let list = cli(repo.path(), &["list"]);
    assert_eq!(list["runs"].as_array().unwrap().len(), 1);
}
#[test]
fn task_failures_spawn_timeout_and_verification_are_terminal() {
    for (worker, verify, timeout) in [
        (
            json!([
                "sh",
                "-c",
                "echo 'HTTP 401 provider authentication rejected' >&2; exit 1"
            ]),
            json!(["true"]),
            3000,
        ),
        (
            json!(["/nonexistent/harness-worker"]),
            json!(["true"]),
            3000,
        ),
        (json!(["sleep", "30"]), json!(["true"]), 100),
        (json!(["true"]), json!(["false"]), 3000),
    ] {
        let repo = fixture();
        let auth_failure = worker.to_string().contains("HTTP 401");
        let s = submit(repo.path(), worker, verify, timeout);
        let r = wait(repo.path(), s["runId"].as_str().unwrap());
        assert_eq!(r["state"], "failed");
        assert!(r["error"].is_string());
        if auth_failure {
            assert!(r["error"].as_str().unwrap().contains("HTTP 401"));
        }
        assert_eq!(r["supervisorAlive"], false);
    }
}
#[test]
fn cancel_terminates_worker_descendants_and_blocks_checkout_sharing() {
    let repo = fixture();
    let s = submit(
        repo.path(),
        json!(["sh", "-c", "sleep 30 & echo $!; wait"]),
        json!(["true"]),
        60000,
    );
    let id = s["runId"].as_str().unwrap();
    let other = submit(repo.path(), json!(["true"]), json!(["true"]), 3000);
    assert_eq!(
        wait(repo.path(), other["runId"].as_str().unwrap())["state"],
        "blocked"
    );
    cli(repo.path(), &["cancel", "--run-id", id]);
    let r = wait(repo.path(), id);
    assert_eq!(r["state"], "cancelled");
    let pid = std::fs::read_to_string(r["workerResult"]["stdoutPath"].as_str().unwrap()).unwrap();
    if let Ok(pid) = pid.trim().parse::<i32>() {
        // A killed child can briefly remain a zombie until the OS reaps it.
        let out = Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let stat = String::from_utf8_lossy(&out.stdout);
        assert!(
            stat.trim().is_empty() || stat.trim().starts_with('Z'),
            "descendant alive: {stat}"
        );
    }
}
#[test]
fn lost_supervision_is_never_reported_running_and_cannot_signal_reused_pid() {
    let repo = fixture();
    let s = submit(repo.path(), json!(["sleep", "30"]), json!(["true"]), 60000);
    let id = s["runId"].as_str().unwrap();
    let owner = s["supervisorPid"].as_u64().unwrap();
    assert!(
        Command::new("kill")
            .args(["-KILL", &owner.to_string()])
            .status()
            .unwrap()
            .success()
    );
    std::thread::sleep(Duration::from_millis(150));
    let r = cli(repo.path(), &["status", "--run-id", id]);
    assert_eq!(r["state"], "failed");
    assert_eq!(r["phase"], "interrupted");
    assert_eq!(r["supervisorAlive"], false);
    assert!(
        r["error"]
            .as_str()
            .unwrap()
            .contains("termination is unconfirmed")
    );
    // The inspection/cancel path never signals a persisted PID. Clean up the
    // test's known owned group explicitly rather than leaving a sleeping orphan.
    if let Some(pid) = r["workerPid"].as_i64() {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}
#[test]
fn dirty_checkout_is_rejected_before_any_worker_spawn() {
    let repo = fixture();
    std::fs::write(repo.path().join("code.txt"), "dirty").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_leio-harness"))
        .args(["task", "submit", "--repo"])
        .arg(repo.path())
        .args([
            "--task",
            "bad",
            "--worker",
            "fixture",
            "--argv",
            "[\"true\"]",
            "--verify-argv",
            "[\"true\"]",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!repo.path().join(".leio-code/harness/runs").exists());
}

#[test]
fn new_files_are_archived_and_sidecars_need_no_gitignore() {
    let repo = fixture();
    std::fs::write(repo.path().join(".gitignore"), "\n").unwrap();
    assert!(
        Command::new("git")
            .args(["add", ".gitignore"])
            .current_dir(repo.path())
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args([
                "-c",
                "user.name=Harness Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "no sidecar ignore"
            ])
            .current_dir(repo.path())
            .status()
            .unwrap()
            .success()
    );
    let s = submit(
        repo.path(),
        json!(["sh", "-c", "printf 'new code\\n' > 'new file.txt'"]),
        json!(["test", "-f", "new file.txt"]),
        3000,
    );
    let r = wait(repo.path(), s["runId"].as_str().unwrap());
    assert_eq!(r["state"], "succeeded");
    assert_eq!(r["changedPaths"], json!(["new file.txt"]));
    let artifacts = r["untrackedArtifacts"].as_array().unwrap();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(
        std::fs::read_to_string(artifacts[0].as_str().unwrap()).unwrap(),
        "new code\n"
    );
}

#[test]
fn task_rejects_subdirectory_before_spawn_or_record_creation() {
    let repo = fixture();
    let nested = repo.path().join("module");
    std::fs::create_dir(&nested).unwrap();
    let active = submit(repo.path(), json!(["sleep", "30"]), json!(["true"]), 60_000);
    let id = active["runId"].as_str().unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_leio-harness"))
        .args([
            "task",
            "submit",
            "--task",
            "must not run",
            "--worker",
            "fixture",
            "--argv",
            "[\"sh\",\"-c\",\"touch unexpected-worker\"]",
            "--verify-argv",
            "[\"true\"]",
            "--repo",
        ])
        .arg(&nested)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    cli(repo.path(), &["cancel", "--run-id", id]);
    assert_eq!(wait(repo.path(), id)["state"], "cancelled");
    assert!(!result.status.success());
    assert!(stderr.contains("Git checkout root"), "{stderr}");
    assert!(!nested.join("unexpected-worker").exists());
    assert!(!nested.join(".leio-code").exists());
}
