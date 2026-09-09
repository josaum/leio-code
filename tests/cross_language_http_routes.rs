//! Integration tests for HTTP route discovery + literal client-to-route
//! resolution (P0 #2 Phase 5).
//!
//! Each test stages a tempdir, runs `build_or_update_index`, and asserts what
//! ended up in `index.cross_language.routes` and
//! `index.cross_language.resolved_http_edges`. Mirrors the pattern from
//! `tests/cross_language_binary_nodes.rs`.

use std::fs;
use std::path::Path;

use leio_code::indexer::{build_or_update_index, default_index_path};
use tempfile::TempDir;

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(&path, body).expect("write file");
}

fn build(root: &Path) -> leio_code::model::RepoIndex {
    // SAFETY: tests run single-threaded by default in cargo's `test` profile.
    unsafe { std::env::set_var("LEIO_DISABLE_SEARCH_SIDECAR", "1") };
    let index_path = default_index_path(root);
    build_or_update_index(root, &index_path, true).expect("build index")
}

// ---------------------------------------------------------------------------
// Server-side route detection
// ---------------------------------------------------------------------------

#[test]
fn flask_route_detected_default_get() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "from flask import Flask\napp = Flask(__name__)\n@app.route(\"/users\")\ndef users(): return \"ok\"\n",
    );
    let index = build(tmp.path());
    let routes: Vec<_> = index
        .cross_language
        .routes
        .iter()
        .filter(|r| r.framework == "flask")
        .collect();
    assert_eq!(routes.len(), 1, "expected one flask route");
    assert_eq!(routes[0].route, "/users");
    assert_eq!(routes[0].method, "GET");
}

#[test]
fn flask_route_with_methods_post() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "@app.route(\"/users\", methods=[\"POST\"])\ndef u(): pass\n",
    );
    let index = build(tmp.path());
    let routes: Vec<_> = index
        .cross_language
        .routes
        .iter()
        .filter(|r| r.framework == "flask")
        .collect();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].method, "POST");
}

#[test]
fn fastapi_get_route_detected() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "from fastapi import FastAPI\napp = FastAPI()\n@app.get(\"/items\")\ndef items(): return []\n",
    );
    let index = build(tmp.path());
    let routes: Vec<_> = index
        .cross_language
        .routes
        .iter()
        .filter(|r| r.framework == "fastapi")
        .collect();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].route, "/items");
    assert_eq!(routes[0].method, "GET");
}

#[test]
fn express_post_route_detected() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.js",
        "const app = express();\napp.post(\"/api/login\", (req, res) => { res.send(\"ok\"); });\n",
    );
    let index = build(tmp.path());
    let routes: Vec<_> = index
        .cross_language
        .routes
        .iter()
        .filter(|r| r.framework == "express")
        .collect();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].route, "/api/login");
    assert_eq!(routes[0].method, "POST");
}

#[test]
fn axum_route_detected() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "src/server.rs",
        "use axum::{Router, routing::get};\nasync fn health() {}\nfn make_app() -> Router { Router::new().route(\"/health\", get(health)) }\n",
    );
    let index = build(tmp.path());
    let routes: Vec<_> = index
        .cross_language
        .routes
        .iter()
        .filter(|r| r.framework == "axum")
        .collect();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].route, "/health");
    assert_eq!(routes[0].method, "GET");
}

// ---------------------------------------------------------------------------
// Resolution: literal client → literal route
// ---------------------------------------------------------------------------

#[test]
fn python_requests_to_flask_route_resolves() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "@app.route(\"/users\")\ndef u(): pass\n",
    );
    write(
        tmp.path(),
        "client.py",
        "import requests\nrequests.get(\"http://localhost/users\")\n",
    );
    let index = build(tmp.path());
    assert_eq!(
        index.cross_language.resolved_http_edges.len(),
        1,
        "expected one resolved edge; got {:#?}",
        index.cross_language.resolved_http_edges
    );
    let edge = &index.cross_language.resolved_http_edges[0];
    assert_eq!(edge.route_path, "/users");
    assert_eq!(edge.route_method, "GET");
    assert_eq!(edge.caller_path, "client.py");
    assert_eq!(edge.route_source_path, "server.py");
    assert_eq!(edge.confidence, 95);
}

#[test]
fn fetch_to_fastapi_route_resolves() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "@app.get(\"/items\")\ndef items(): return []\n",
    );
    write(tmp.path(), "client.ts", "fetch(\"/items\")\n");
    let index = build(tmp.path());
    assert_eq!(
        index.cross_language.resolved_http_edges.len(),
        1,
        "expected one resolved edge for fetch->fastapi"
    );
    let edge = &index.cross_language.resolved_http_edges[0];
    assert_eq!(edge.route_path, "/items");
}

#[test]
fn reqwest_to_axum_route_resolves() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "src/server.rs",
        "use axum::{Router, routing::get};\nasync fn h() {}\nfn app() -> Router { Router::new().route(\"/health\", get(h)) }\n",
    );
    write(
        tmp.path(),
        "src/client.rs",
        "async fn ping() { let _ = reqwest::Client::new().get(\"/health\").send().await; }\n",
    );
    let index = build(tmp.path());
    assert_eq!(
        index.cross_language.resolved_http_edges.len(),
        1,
        "expected one resolved edge for reqwest->axum"
    );
    let edge = &index.cross_language.resolved_http_edges[0];
    assert_eq!(edge.route_path, "/health");
    assert_eq!(edge.route_method, "GET");
}

#[test]
fn path_only_fetch_resolves_without_scheme() {
    // `fetch("/foo")` has no scheme — make sure it still resolves to a route
    // declared by the server in the same repo.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "@app.get(\"/foo\")\ndef f(): pass\n",
    );
    write(tmp.path(), "client.js", "fetch(\"/foo\")\n");
    let index = build(tmp.path());
    assert_eq!(index.cross_language.resolved_http_edges.len(), 1);
}

#[test]
fn non_matching_path_does_not_resolve() {
    // The client targets `/missing`; the server only declares `/users`.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "@app.get(\"/users\")\ndef u(): pass\n",
    );
    write(tmp.path(), "client.js", "fetch(\"/missing\")\n");
    let index = build(tmp.path());
    assert!(
        index.cross_language.resolved_http_edges.is_empty(),
        "no route matches /missing — no edge should be emitted"
    );
}

#[test]
fn template_literal_url_is_not_resolved() {
    // Template literals are intentionally skipped in v1 — Phase 6 will turn
    // these into a path-parameter match, but for now they must not match any
    // literal route.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "@app.get(\"/users\")\ndef u(): pass\n",
    );
    write(tmp.path(), "client.ts", "fetch(`/users/${id}`)\n");
    let index = build(tmp.path());
    assert!(
        index.cross_language.resolved_http_edges.is_empty(),
        "template-literal URLs must not emit resolved edges"
    );
}
