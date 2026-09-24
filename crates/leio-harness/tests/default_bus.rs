//! Real subprocess tests: default activation, startup races, durable restart,
//! child endpoint propagation and delivery failure. No external model calls.
use leio_harness::bus_client::BusClient;
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Child, Command},
    time::{Duration, Instant},
};
const BIN: &str = env!("CARGO_BIN_EXE_leio-harness");

struct Fixture {
    dir: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        Self {
            dir: tempfile::Builder::new()
                .prefix("leio-bus-")
                .tempdir_in("/tmp")
                .unwrap(),
        }
    }
    fn root(&self) -> &Path {
        self.dir.path()
    }
    fn command(&self) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.env("LEIO_HARNESS_BUS_DIR", self.root().join("bus"))
            .env_remove("LEIO_HARNESS_BUS")
            .env("LEIO_HARNESS_CONFIG", self.root().join("empty.env"));
        fs::write(self.root().join("empty.env"), "").unwrap();
        cmd
    }
    fn start(&self, id: &str, script: &str, args: &[&str]) -> Child {
        let spec = self.root().join(format!("{id}.json"));
        fs::write(&spec, serde_json::to_vec(&json!({"runId":id,"argv":["/bin/sh","-c",script],"cwd":self.root(),"outputDir":self.root().join("runs"),"timeoutMs":5000,"killGraceMs":20})).unwrap()).unwrap();
        self.command()
            .args(["run", "--spec"])
            .arg(spec)
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    }
    fn stop_bus(&self) {
        if let Ok(pid) = fs::read_to_string(self.root().join("bus/server.pid")) {
            let pid: i32 = pid.parse().unwrap();
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGTERM,
            );
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop_bus();
    }
}
fn passed(child: Child) -> Value {
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[tokio::test]
async fn concurrent_default_starters_share_bus_and_recover_durable_rows() {
    let f = Fixture::new();
    let a = f.start("a", "printf '%s' \"$LEIO_HARNESS_BUS\"", &[]);
    let b = f.start("b", "printf '%s' \"$LEIO_HARNESS_BUS\"", &[]);
    let a = passed(a);
    let b = passed(b);
    let addr = fs::read_to_string(a["stdoutPath"].as_str().unwrap()).unwrap();
    assert_eq!(
        addr,
        fs::read_to_string(b["stdoutPath"].as_str().unwrap()).unwrap()
    );
    let mut client = BusClient::connect(&addr).await.unwrap();
    let before = client.list(None).await.unwrap();
    assert_eq!(before.len(), 4);
    f.stop_bus();
    drop(client);
    tokio::time::sleep(Duration::from_millis(200)).await;
    passed(f.start("c", "true", &[]));
    let after = BusClient::connect(&addr)
        .await
        .unwrap()
        .list(None)
        .await
        .unwrap();
    assert_eq!(after.len(), 6);
    assert_eq!(
        after.iter().map(|r| r.seq).collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5, 6]
    );
}

#[test]
fn offline_is_explicit_and_configured_failure_prevents_work() {
    let f = Fixture::new();
    passed(f.start("offline", "test -z \"$LEIO_HARNESS_BUS\"", &["--no-bus"]));
    assert!(!f.root().join("bus").exists());
    let missing = f.root().join("missing.sock");
    let output = f
        .start(
            "unavailable",
            "touch SHOULD_NOT_EXIST",
            &["--bus", missing.to_str().unwrap()],
        )
        .wait_with_output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!f.root().join("SHOULD_NOT_EXIST").exists());
}

#[test]
fn lost_bus_marks_run_infra_error_and_exits_nonzero() {
    let f = Fixture::new();
    let child = f.start("lost", "touch started; sleep 0.5; echo completed", &[]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f.root().join("started").exists() {
        assert!(Instant::now() < deadline, "run did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    f.stop_bus();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "infra_error");
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("bus result delivery failed")
    );
    let saved: Value =
        serde_json::from_slice(&fs::read(f.root().join("runs/lost/result.json")).unwrap()).unwrap();
    assert_eq!(saved, result);
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn day_parallel_defaults_deliver_results_and_bus_loss_fails_day() {
    let f = Fixture::new();
    let repo = f.root().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Harness test"]);
    git(&repo, &["config", "user.email", "test@example.invalid"]);
    fs::write(repo.join("README"), "fixture").unwrap();
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-qm", "fixture"]);
    let spec = f.root().join("day.json");
    let mut value = json!({"goal":"default-on lanes", "repo":repo,"worktreeRoot":f.root().join("worktrees"),"outputDir":f.root().join("day-runs"),"parallel":true,"timeoutMs":10000,"lanes":[
        {"agentId":"a","task":"write artifact","argv":["/bin/sh","-c","test -n \"$LEIO_HARNESS_BUS\" && echo a > a.txt"]},
        {"agentId":"b","task":"write artifact","argv":["/bin/sh","-c","test -n \"$LEIO_HARNESS_BUS\" && echo b > b.txt"]}
    ]});
    fs::write(&spec, serde_json::to_vec(&value).unwrap()).unwrap();
    let output = f
        .command()
        .args(["day", "--spec"])
        .arg(&spec)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["passed"], 2);
    let addr = f.root().join("bus/bus.sock");
    let mut bus = BusClient::connect(addr.to_str().unwrap()).await.unwrap();
    for outcome in report["outcomes"].as_array().unwrap() {
        let rows = bus
            .list(Some(&format!(
                "result/{}",
                outcome["agentId"].as_str().unwrap()
            )))
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].run_id, outcome["runId"].as_str().unwrap());
    }
    // The command itself kills the bus only after publication and child startup.
    let pid = fs::read_to_string(f.root().join("bus/server.pid")).unwrap();
    value["lanes"] = json!([{"agentId":"lost","task":"exercise delivery failure","argv":["/bin/sh","-c",format!("kill -TERM {pid}; sleep 0.1; echo artifact > lost.txt")]}]);
    fs::write(&spec, serde_json::to_vec(&value).unwrap()).unwrap();
    let output = f
        .command()
        .args(["day", "--watch", "--spec"])
        .arg(&spec)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("infra_error"), "{stdout}");
}
