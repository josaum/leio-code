use serde_json::json;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::tempdir;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_leio-harness")
}

/// Poll until the bus actually serves Flight (not just accepts TCP). A raw
/// TCP-accept races with Flight handler registration under parallel load,
/// which made the first publish flaky.
fn wait_bus_ready(bus_addr: &str, query: &std::path::Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let out = Command::new(bin())
            .args([
                "bus",
                "match",
                "--bus",
                bus_addr,
                "--query",
                query.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        if out.status.success() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "bus server did not become ready: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn spawn_mock_embedding_server(vector: Vec<f32>) -> (String, thread::JoinHandle<bool>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let response_body = serde_json::to_string(&json!({
        "data": [{"index": 0, "embedding": vector}]
    }))
    .unwrap();
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut request = [0_u8; 8192];
                    let _ = stream.read(&mut request);
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        response_body.len(),
                        response_body
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                    return true;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return false;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => return false,
            }
        }
    });
    (format!("http://127.0.0.1:{port}/v1"), handle)
}

#[test]
fn run_writes_arrow_ipc_and_result() {
    let root = tempdir().unwrap();
    let spec = root.path().join("spec.json");
    fs::write(
        &spec,
        serde_json::to_vec(&json!({
            "runId": "run-a",
            "argv": ["/bin/echo", "arrow-ok"],
            "cwd": root.path(),
            "outputDir": root.path().join("runs"),
            "timeoutMs": 5000,
            "envAllowlist": [],
            "env": {}
        }))
        .unwrap(),
    )
    .unwrap();
    let output = Command::new(bin())
        .args(["run", "--spec", spec.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "passed");
    let arrow = result["eventsArrowPath"].as_str().unwrap();
    let inspect = Command::new(bin())
        .args(["arrow", "inspect", arrow])
        .output()
        .unwrap();
    assert!(inspect.status.success());
    let summary: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert!(summary["rows"].as_u64().unwrap() >= 1);
    assert!(summary["payload_bytes"].as_u64().unwrap() >= 8);
}

#[test]
fn approval_mismatch_never_spawns() {
    let root = tempdir().unwrap();
    let marker = root.path().join("spawned");
    let spec = root.path().join("spec.json");
    fs::write(
        &spec,
        serde_json::to_vec(&json!({
            "runId": "deploy-a",
            "argv": ["/usr/bin/touch", marker],
            "cwd": root.path(),
            "outputDir": root.path().join("runs"),
            "timeoutMs": 5000,
            "requiredApprovalToken": "deploy:prod:abc",
            "approvalToken": "wrong"
        }))
        .unwrap(),
    )
    .unwrap();
    let output = Command::new(bin())
        .args(["run", "--spec", spec.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!marker.exists());
}

#[test]
fn leases_conflict_and_release_deterministically() {
    let root = tempdir().unwrap();
    let store = root.path().join("leases.json");
    let lease_a = root.path().join("a.json");
    let lease_b = root.path().join("b.json");
    let base = json!({
        "agentId": "agent-a",
        "runId": "run-a",
        "worktreePath": "/tmp/jqr/a",
        "branch": "agents/a",
        "owner": "test",
        "heartbeat": "2026-08-13T00:00:00Z",
        "state": "active",
        "lockScopes": ["worktree", "branch"]
    });
    fs::write(&lease_a, serde_json::to_vec(&base).unwrap()).unwrap();
    let mut conflicting = base;
    conflicting["agentId"] = json!("agent-b");
    conflicting["runId"] = json!("run-b");
    fs::write(&lease_b, serde_json::to_vec(&conflicting).unwrap()).unwrap();
    let first = Command::new(bin())
        .args([
            "lease",
            "--store",
            store.to_str().unwrap(),
            "acquire",
            "--lease",
            lease_a.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(first.status.success());
    let second = Command::new(bin())
        .args([
            "lease",
            "--store",
            store.to_str().unwrap(),
            "acquire",
            "--lease",
            lease_b.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!second.status.success());
    let released = Command::new(bin())
        .args([
            "lease",
            "--store",
            store.to_str().unwrap(),
            "release",
            "--run-id",
            "run-a",
        ])
        .output()
        .unwrap();
    assert!(released.status.success());
}

#[test]
fn bus_selftest_publishes_and_matches_over_do_exchange() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let output = Command::new(bin())
        .args(["bus", "selftest", "--port", &port.to_string()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["ack"]["last_seq"], 4);
    assert_eq!(result["merge_gate"]["verdict"], "pass");
    assert!(result["evolve"]["anchor_penalty"].is_number());
    assert!(
        result["evolve"]["offspring_seq"].as_u64().unwrap()
            > result["evolve"]["parent_seq"].as_u64().unwrap()
    );
    let hits = result["matches"]["hits"].as_array().unwrap();
    assert_eq!(hits[0]["agent_id"], "codex");
    assert_eq!(hits[1]["agent_id"], "kimi");
}

#[test]
fn bus_state_survives_restart_with_persist() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let bus_addr = format!("http://127.0.0.1:{port}");
    let root = tempdir().unwrap();
    let snap = root.path().join("bus.arrow");
    let rows = root.path().join("rows.json");
    fs::write(
        &rows,
        serde_json::to_vec(&json!([{"agent_id":"codex","run_id":"r1","topic":"intent/codex","vector":[1.0,0.0,0.0,0.0]}])).unwrap(),
    ).unwrap();
    let query = root.path().join("query.json");
    fs::write(
        &query,
        serde_json::to_vec(&json!({"vector":[1.0,0.0,0.0,0.0],"topic":"intent/codex"})).unwrap(),
    )
    .unwrap();
    for round in 0..2 {
        let mut server = Command::new(bin())
            .args([
                "bus",
                "serve",
                "--bind",
                &format!("127.0.0.1:{port}"),
                "--persist",
                snap.to_str().unwrap(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        wait_bus_ready(&bus_addr, &query);
        if round == 0 {
            let published = Command::new(bin())
                .args([
                    "bus",
                    "publish",
                    "--bus",
                    &bus_addr,
                    "--rows",
                    rows.to_str().unwrap(),
                ])
                .output()
                .unwrap();
            assert!(published.status.success());
        }
        let matched = Command::new(bin())
            .args([
                "bus",
                "match",
                "--bus",
                &bus_addr,
                "--query",
                query.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(matched.status.success());
        let hits: serde_json::Value = serde_json::from_slice(&matched.stdout).unwrap();
        assert_eq!(hits[0]["agent_id"], "codex");
        assert_eq!(hits[0]["seq"], 1);
        let _ = server.kill();
        let _ = server.wait();
    }
}

#[test]
fn bus_publish_and_match_roundtrip_over_cli() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let bus_addr = format!("http://127.0.0.1:{port}");
    let root = tempdir().unwrap();
    let rows = root.path().join("rows.json");
    fs::write(
        &rows,
        serde_json::to_vec(&json!([{
            "agent_id": "claude",
            "run_id": "run-x",
            "topic": "intent/task-1",
            "timestamp_ms": 0,
            "vector": [0.0, 1.0, 0.0]
        }]))
        .unwrap(),
    )
    .unwrap();
    let query = root.path().join("query.json");
    fs::write(
        &query,
        serde_json::to_vec(&json!({"vector": [0.0, 1.0, 0.0], "topic": "intent/task-1"})).unwrap(),
    )
    .unwrap();
    let mut server = Command::new(bin())
        .args(["bus", "serve", "--bind", &format!("127.0.0.1:{port}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    wait_bus_ready(&bus_addr, &query);
    let published = Command::new(bin())
        .args([
            "bus",
            "publish",
            "--bus",
            &bus_addr,
            "--rows",
            rows.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        published.status.success(),
        "{}",
        String::from_utf8_lossy(&published.stderr)
    );
    let matched = Command::new(bin())
        .args([
            "bus",
            "match",
            "--bus",
            &bus_addr,
            "--query",
            query.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        matched.status.success(),
        "{}",
        String::from_utf8_lossy(&matched.stderr)
    );
    let hits: serde_json::Value = serde_json::from_slice(&matched.stdout).unwrap();
    assert_eq!(hits[0]["agent_id"], "claude");
    let _ = server.kill();
    let _ = server.wait();
}

#[test]
fn gepa_cycle_uses_ranked_result_parent_end_to_end() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let bus_addr = format!("http://127.0.0.1:{port}");
    let root = tempdir().unwrap();
    let snapshot = root.path().join("bus.arrow");
    let rows = root.path().join("rows.json");
    fs::write(
        &rows,
        serde_json::to_vec(&json!([
            {
                "agent_id": "intent-agent",
                "run_id": "intent-run",
                "topic": "intent/cycle",
                "vector": [-1.0, 0.0]
            },
            {
                "agent_id": "result-agent",
                "run_id": "result-run",
                "topic": "result/cycle",
                "vector": [1.0, 0.0]
            }
        ]))
        .unwrap(),
    )
    .unwrap();
    let query = root.path().join("query.json");
    fs::write(
        &query,
        serde_json::to_vec(&json!({"vector": [1.0, 0.0], "topic": "result/cycle"})).unwrap(),
    )
    .unwrap();
    let (embed_url, embed_server) = spawn_mock_embedding_server(vec![1.0, 0.0]);
    let mut server = Command::new(bin())
        .args([
            "bus",
            "serve",
            "--bind",
            &format!("127.0.0.1:{port}"),
            "--persist",
            snapshot.to_str().unwrap(),
        ])
        .env("LEIO_HARNESS_EMBED_URL", embed_url)
        .env("LEIO_HARNESS_EMBED_MODEL", "mock")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_bus_ready(&bus_addr, &query);

    let published = Command::new(bin())
        .args([
            "bus",
            "publish",
            "--bus",
            &bus_addr,
            "--rows",
            rows.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let cycle = Command::new(bin())
        .args([
            "gepa",
            "cycle",
            "--bus",
            &bus_addr,
            "--lane",
            "cycle",
            "--goal-text",
            "goal",
            "--generations",
            "1",
            "--strength",
            "0",
            "--anchor-beta",
            "2",
            "--seed",
            "7",
        ])
        .output()
        .unwrap();
    let _ = server.kill();
    let _ = server.wait();
    let embed_called = embed_server.join().unwrap();

    assert!(
        published.status.success(),
        "{}",
        String::from_utf8_lossy(&published.stderr)
    );
    assert!(
        cycle.status.success(),
        "{}",
        String::from_utf8_lossy(&cycle.stderr)
    );
    assert!(embed_called, "cycle never called the mock embedding server");
    let report: serde_json::Value = serde_json::from_slice(&cycle.stdout).unwrap();
    assert_eq!(report["generations"], 1);
    assert_eq!(report["offsprings"][0]["parent_seq"], 2);
    assert_eq!(report["offsprings"][0]["offspring_seq"], 3);
}

#[test]
fn bus_publish_and_match_roundtrip_over_uds() {
    let root = tempdir().unwrap();
    let sock = root.path().join("bus.sock");
    let bus_addr = format!("unix://{}", sock.display());
    let rows = root.path().join("rows.json");
    fs::write(
        &rows,
        serde_json::to_vec(&json!([{
            "agent_id": "gemini",
            "run_id": "run-uds-1",
            "topic": "intent/task-uds",
            "timestamp_ms": 0,
            "vector": [1.0, 0.0, 0.0]
        }]))
        .unwrap(),
    )
    .unwrap();
    let query = root.path().join("query.json");
    fs::write(
        &query,
        serde_json::to_vec(&json!({"vector": [1.0, 0.0, 0.0], "topic": "intent/task-uds"}))
            .unwrap(),
    )
    .unwrap();
    let mut server = Command::new(bin())
        .args(["bus", "serve", "--bind", sock.to_str().unwrap()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    wait_bus_ready(&bus_addr, &query);
    let published = Command::new(bin())
        .args([
            "bus",
            "publish",
            "--bus",
            &bus_addr,
            "--rows",
            rows.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        published.status.success(),
        "{}",
        String::from_utf8_lossy(&published.stderr)
    );
    let matched = Command::new(bin())
        .args([
            "bus",
            "match",
            "--bus",
            &bus_addr,
            "--query",
            query.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        matched.status.success(),
        "{}",
        String::from_utf8_lossy(&matched.stderr)
    );
    let hits: serde_json::Value = serde_json::from_slice(&matched.stdout).unwrap();
    assert_eq!(hits[0]["agent_id"], "gemini");
    assert_eq!(hits[0]["seq"], 1);
    let _ = server.kill();
    let _ = server.wait();
}

#[test]
fn worktree_retire_refuses_dirty_and_rejects_escape() {
    let root = tempdir().unwrap();
    let repo = root.path().join("repo");
    let wt = root.path().join("wt");
    let worktree = wt.join("lane");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&wt).unwrap();
    exec_git(&repo, &["init", "-b", "main"]);
    exec_git(&repo, &["config", "user.email", "t@t"]);
    exec_git(&repo, &["config", "user.name", "t"]);
    fs::write(repo.join("README"), "x\n").unwrap();
    exec_git(&repo, &["add", "README"]);
    exec_git(&repo, &["commit", "-m", "init"]);

    let bin = bin();
    // create
    let out = Command::new(bin)
        .args([
            "worktree",
            "create",
            "--repo",
            repo.to_str().unwrap(),
            "--root",
            wt.to_str().unwrap(),
            "--path",
            worktree.to_str().unwrap(),
            "--branch",
            "agents/lane",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // dirty retire refused
    fs::write(worktree.join("dirty"), "y\n").unwrap();
    let out = Command::new(bin)
        .args([
            "worktree",
            "retire",
            "--repo",
            repo.to_str().unwrap(),
            "--root",
            wt.to_str().unwrap(),
            "--path",
            worktree.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("dirty"));
    // escape rejected
    let out = Command::new(bin)
        .args([
            "worktree",
            "create",
            "--repo",
            repo.to_str().unwrap(),
            "--root",
            wt.to_str().unwrap(),
            "--path",
            root.path().join("outside").to_str().unwrap(),
            "--branch",
            "agents/x",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn agent_proxy_forwards_acp_frames_and_records_arrow() {
    let root = tempdir().unwrap();
    let spec = root.path().join("agent-spec.json");
    fs::write(
        &spec,
        serde_json::to_vec(&json!({
            "sessionId": "sess-forward",
            "cwd": root.path(),
            "outputDir": root.path().join("runs"),
            "agentArgv": ["/bin/sh", "-c", "while IFS= read -r l; do printf '%s\\n' \"$l\" | sed 's/ping/pong/g'; done"],
            "idleTimeoutMs": 8000,
            "preflightLeio": false
        }))
        .unwrap(),
    )
    .unwrap();
    let mut child = Command::new(bin())
        .args(["agent", "--spec", spec.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let rx = spawn_line_reader(child.stdout.take().unwrap());
    stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n")
        .unwrap();
    stdin.flush().unwrap();
    let line = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("harness did not echo the ACP frame");
    assert!(line.contains("\"pong\""), "unexpected frame: {line}");
    drop(stdin);
    let status = wait_exit(&mut child, Duration::from_secs(15)).expect("harness did not exit");
    assert!(status.success(), "harness exited with {status}");
    let result: serde_json::Value = serde_json::from_slice(
        &fs::read(root.path().join("runs/sess-forward/result.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(result["status"], "passed");
    let inspect = Command::new(bin())
        .args([
            "arrow",
            "inspect",
            result["eventsArrowPath"].as_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(inspect.status.success());
    let summary: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    let kinds: Vec<&str> = summary["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|value| value.as_str())
        .collect();
    assert!(kinds.contains(&"acp_request"));
    assert!(kinds.contains(&"acp_response"));
}

#[test]
fn agent_proxy_idle_timeout_terminates_child() {
    let root = tempdir().unwrap();
    let spec = root.path().join("agent-spec.json");
    fs::write(
        &spec,
        serde_json::to_vec(&json!({
            "sessionId": "sess-idle",
            "cwd": root.path(),
            "outputDir": root.path().join("runs"),
            "agentArgv": ["/bin/sh", "-c", "sleep 30"],
            "idleTimeoutMs": 400,
            "preflightLeio": false
        }))
        .unwrap(),
    )
    .unwrap();
    let mut child = Command::new(bin())
        .args(["agent", "--spec", spec.to_str().unwrap()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let status = wait_exit(&mut child, Duration::from_secs(15)).expect("harness did not exit");
    assert!(!status.success());
    let result: serde_json::Value =
        serde_json::from_slice(&fs::read(root.path().join("runs/sess-idle/result.json")).unwrap())
            .unwrap();
    assert_eq!(result["status"], "timed_out");
    assert!(result["error"].as_str().unwrap().contains("no ACP traffic"));
}

#[test]
fn agent_proxy_approval_mismatch_never_spawns() {
    let root = tempdir().unwrap();
    let marker = root.path().join("spawned");
    let spec = root.path().join("agent-spec.json");
    fs::write(
        &spec,
        serde_json::to_vec(&json!({
            "sessionId": "sess-approval",
            "cwd": root.path(),
            "outputDir": root.path().join("runs"),
            "agentArgv": ["/usr/bin/touch", marker],
            "idleTimeoutMs": 8000,
            "preflightLeio": false,
            "requiredApprovalToken": "deploy:prod:abc",
            "approvalToken": "wrong"
        }))
        .unwrap(),
    )
    .unwrap();
    let output = Command::new(bin())
        .args(["agent", "--spec", spec.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!marker.exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("approval token mismatch"));
}

fn spawn_line_reader<R: Read + Send + 'static>(reader: R) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx.send(line.trim().to_owned()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

fn wait_exit(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn process_timeout_nonexit_and_truncation() {
    let bin = bin();
    let root = tempdir().unwrap();
    let spec = root.path().join("s.json");
    // non-zero exit
    fs::write(&spec, serde_json::to_vec(&json!({"runId":"fail","argv":["/bin/sh","-c","exit 3"],"cwd":root.path(),"outputDir":root.path().join("runs"),"timeoutMs":10000})).unwrap()).unwrap();
    let out = Command::new(bin)
        .args(["run", "--spec", spec.to_str().unwrap()])
        .output()
        .unwrap();
    let r: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(r["status"], "failed");
    assert_eq!(r["exitCode"], 3);
    // timeout
    fs::write(&spec, serde_json::to_vec(&json!({"runId":"hang","argv":["/bin/sleep","30"],"cwd":root.path(),"outputDir":root.path().join("runs"),"timeoutMs":500,"killGraceMs":500})).unwrap()).unwrap();
    let out = Command::new(bin)
        .args(["run", "--spec", spec.to_str().unwrap()])
        .output()
        .unwrap();
    let r: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(r["status"], "timed_out");
    // truncation
    fs::write(&spec, serde_json::to_vec(&json!({"runId":"big","argv":["/bin/sh","-c","head -c 100000 /dev/zero | tr '\\0' 'a'"],"cwd":root.path(),"outputDir":root.path().join("runs"),"timeoutMs":10000,"maxOutputBytes":1024})).unwrap()).unwrap();
    let out = Command::new(bin)
        .args(["run", "--spec", spec.to_str().unwrap()])
        .output()
        .unwrap();
    let r: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(r["status"], "passed");
    assert_eq!(r["stdoutTruncated"], true);
}

fn exec_git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
