use leio_harness::delivery;
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::TcpStream,
    path::Path,
};

fn stage(source: &Path, target: &Path, content: &str) -> Value {
    fs::create_dir_all(source.join("assets")).unwrap();
    fs::write(source.join("index.html"), content).unwrap();
    fs::write(source.join("assets/style.css"), "body{color:blue}").unwrap();
    fs::write(source.join("assets/app.js"), "console.log('ready');").unwrap();
    delivery::prepare(source, target).unwrap()
}
fn deploy(target: &Path, proposal: &Value, server: &delivery::DeliveryServer) -> Value {
    delivery::deploy(
        target,
        proposal["revision"].as_str().unwrap(),
        proposal["authorization_digest"].as_str().unwrap(),
        server.address(),
    )
    .unwrap_or_else(|error| panic!("{error:#}; server diagnostics: {:?}", server.diagnostics()))
}

#[test]
fn real_http_deploy_two_revisions_then_verified_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("deployment");
    let source = dir.path().join("dist");
    let server = delivery::start_server(&target, "127.0.0.1:0".parse().unwrap()).unwrap();
    let a = stage(&source, &target, "<h1>A</h1>");
    let receipt = deploy(&target, &a, &server);
    assert_eq!(receipt["files_verified"], 3);
    for (path, mime, body) in [
        ("/assets/style.css", "text/css", "body{color:blue}"),
        ("/assets/app.js", "text/javascript", "console.log('ready');"),
    ] {
        let mut stream = TcpStream::connect(server.address()).unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.contains(&format!("Content-Type: {mime}")));
        assert!(response.ends_with(body));
    }
    let b = stage(&source, &target, "<h1>B</h1>");
    deploy(&target, &b, &server);
    let state = delivery::inspect(&target).unwrap();
    assert_eq!(state["state"]["previous"], a["revision"]);
    assert_eq!(
        delivery::verify(&target, server.address()).unwrap()["revision"],
        b["revision"]
    );
    let restored = delivery::rollback(
        &target,
        state["rollback_authorization_digest"].as_str().unwrap(),
        server.address(),
    )
    .unwrap();
    assert_eq!(restored["revision"], a["revision"]);
    assert!(
        delivery::deploy(
            &target,
            b["revision"].as_str().unwrap(),
            b["authorization_digest"].as_str().unwrap(),
            server.address()
        )
        .is_err(),
        "rollback must not revive an old approval from the previous A revision"
    );
    assert_eq!(
        delivery::verify(&target, server.address()).unwrap()["revision"],
        a["revision"]
    );
    let saved: Value =
        serde_json::from_slice(&fs::read(target.join("state.jsonld")).unwrap()).unwrap();
    assert_eq!(saved["@context"]["details"]["@type"], "@json");
    assert_eq!(saved["details"]["history"].as_array().unwrap().len(), 3);
}

#[test]
fn runtime_accepts_request_bytes_arriving_after_connection() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("deployment");
    let proposal = stage(&dir.path().join("dist"), &target, "A");
    let server = delivery::start_server(&target, "127.0.0.1:0".parse().unwrap()).unwrap();
    deploy(&target, &proposal, &server);
    let mut stream = TcpStream::connect(server.address()).unwrap();
    // The listener accepts before the request arrives. On macOS an accepted
    // socket inherits O_NONBLOCK unless the server explicitly clears it.
    std::thread::sleep(std::time::Duration::from_millis(100));
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200 "),
        "{response:?}; server diagnostics: {:?}",
        server.diagnostics()
    );
    assert!(response.ends_with('A'));
}

#[test]
fn authorization_tamper_and_cross_target_runtime_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("deployment");
    let source = dir.path().join("dist");
    let server = delivery::start_server(&target, "127.0.0.1:0".parse().unwrap()).unwrap();
    let a = stage(&source, &target, "A");
    let id = a["revision"].as_str().unwrap();
    let approval = a["authorization_digest"].as_str().unwrap();
    assert!(delivery::deploy(&target, id, "not-authorized", server.address()).is_err());
    assert!(delivery::inspect(&target).unwrap()["state"]["active"].is_null());
    let wrong =
        delivery::start_server(&dir.path().join("other"), "127.0.0.1:0".parse().unwrap()).unwrap();
    assert!(delivery::deploy(&target, id, approval, wrong.address()).is_err());
    assert!(delivery::inspect(&target).unwrap()["state"]["active"].is_null());
    fs::write(
        target.join("releases").join(id).join("files/index.html"),
        "tampered",
    )
    .unwrap();
    assert!(delivery::deploy(&target, id, approval, server.address()).is_err());
    fs::write(
        target.join("releases").join(id).join("files/index.html"),
        "A",
    )
    .unwrap();
    deploy(&target, &a, &server);
    assert!(
        delivery::deploy(&target, id, approval, server.address()).is_err(),
        "stale authorization must not replay"
    );
    fs::write(
        target.join("releases").join(id).join("files/index.html"),
        "tampered",
    )
    .unwrap();
    assert!(delivery::verify(&target, server.address()).is_err());
    assert!(delivery::deploy(&target, "../escape", approval, server.address()).is_err());
}

#[test]
fn interrupted_promotion_resumes_original_authorized_revision() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("deployment");
    let source = dir.path().join("dist");
    let server = delivery::start_server(&target, "127.0.0.1:0".parse().unwrap()).unwrap();
    let a = stage(&source, &target, "A");
    deploy(&target, &a, &server);
    let b = stage(&source, &target, "B");
    // Model interruption immediately after the atomic pending promotion save.
    let path = target.join("state.jsonld");
    let mut saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    saved["details"]["pending"] = json!({"action":"deploy","revision":b["revision"],"authorization":b["authorization_digest"],"old_active":a["revision"],"old_previous":null});
    saved["details"]["active"] = b["revision"].clone();
    saved["details"]["previous"] = a["revision"].clone();
    fs::write(path, serde_json::to_vec_pretty(&saved).unwrap()).unwrap();
    assert!(
        delivery::deploy(
            &target,
            b["revision"].as_str().unwrap(),
            "wrong",
            server.address()
        )
        .is_err()
    );
    deploy(&target, &b, &server);
    let state = delivery::inspect(&target).unwrap();
    assert!(state["state"]["pending"].is_null());
    assert_eq!(state["state"]["previous"], a["revision"]);
    assert_eq!(state["state"]["history"].as_array().unwrap().len(), 2);
}

#[test]
fn failed_runtime_verification_restores_prior_active_release() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("deployment");
    let source = dir.path().join("dist");
    let server = delivery::start_server(&target, "127.0.0.1:0".parse().unwrap()).unwrap();
    let a = stage(&source, &target, "A");
    deploy(&target, &a, &server);
    let b = stage(&source, &target, "B");
    let wrong =
        delivery::start_server(&dir.path().join("other"), "127.0.0.1:0".parse().unwrap()).unwrap();
    assert!(
        delivery::deploy(
            &target,
            b["revision"].as_str().unwrap(),
            b["authorization_digest"].as_str().unwrap(),
            wrong.address()
        )
        .is_err()
    );
    let current = delivery::inspect(&target).unwrap();
    assert_eq!(current["state"]["active"], a["revision"]);
    assert!(current["state"]["previous"].is_null());
    assert!(current["state"]["pending"].is_null());
    assert_eq!(
        delivery::verify(&target, server.address()).unwrap()["revision"],
        a["revision"]
    );
    // Failed verification is a recorded attempt; retry can promote exactly B.
    deploy(&target, &b, &server);
    let same = delivery::prepare(&source, &target).unwrap();
    deploy(&target, &same, &server);
    assert_eq!(
        delivery::inspect(&target).unwrap()["state"]["previous"],
        a["revision"],
        "authorized same-release verification must preserve rollback target"
    );
}

#[cfg(unix)]
#[test]
fn symlinks_and_unsafe_artifact_names_are_rejected() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("deployment");
    let source = dir.path().join("dist");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("index.html"), "A").unwrap();
    symlink("index.html", source.join("alias.html")).unwrap();
    assert!(delivery::prepare(&source, &target).is_err());
    fs::remove_file(source.join("alias.html")).unwrap();
    fs::write(source.join("..\\escape"), "unsafe").unwrap();
    assert!(delivery::prepare(&source, &target).is_err());
    fs::remove_file(source.join("..\\escape")).unwrap();
    fs::create_dir(source.join("__leio")).unwrap();
    fs::write(source.join("__leio/revision"), "reserved").unwrap();
    assert!(delivery::prepare(&source, &target).is_err());
    fs::remove_dir_all(source.join("__leio")).unwrap();
    let linked = dir.path().join("linked");
    symlink(&target, &linked).unwrap();
    assert!(delivery::prepare(&source, &linked).is_err());
    assert!(delivery::start_server(&target, "0.0.0.0:0".parse().unwrap()).is_err());
}
