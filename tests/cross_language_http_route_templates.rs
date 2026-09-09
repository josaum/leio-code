//! Integration tests for HTTP route template matching (P0 #2 Phase 6).
//!
//! Each test stages a tempdir, runs `build_or_update_index`, and asserts on
//! `index.cross_language.routes` (the `path_params` field) and
//! `index.cross_language.resolved_http_edges` (`match_kind` + `confidence`).
//! Mirrors the pattern from `tests/cross_language_http_routes.rs`.

use std::fs;
use std::path::Path;

use leio_code::indexer::{build_or_update_index, default_index_path};
use leio_code::model::MatchKind;
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
// Path-parameter extraction across frameworks
// ---------------------------------------------------------------------------

#[test]
fn flask_path_param_extracted_with_typed_form() {
    // `<int:id>` is Flask's typed-parameter syntax — the `int` part is a
    // converter, not part of the parameter name.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "@app.route(\"/users/<int:id>\")\ndef get_user(id): return id\n",
    );
    let index = build(tmp.path());
    let routes: Vec<_> = index
        .cross_language
        .routes
        .iter()
        .filter(|r| r.framework == "flask")
        .collect();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].route, "/users/<int:id>");
    assert_eq!(routes[0].path_params, vec!["id".to_string()]);
}

#[test]
fn fastapi_path_param_extracted_and_resolved_to_record() {
    // Phase 5 dropped these on purpose to prevent false literal-matches;
    // Phase 6 keeps them and exposes `path_params`. This test pins that the
    // FastAPI templated route round-trips into the index.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "from fastapi import FastAPI\napp = FastAPI()\n@app.get(\"/users/{id}\")\ndef get_user(id: int): return id\n",
    );
    let index = build(tmp.path());
    let routes: Vec<_> = index
        .cross_language
        .routes
        .iter()
        .filter(|r| r.framework == "fastapi")
        .collect();
    assert_eq!(routes.len(), 1, "fastapi templated route must be recorded");
    assert_eq!(routes[0].route, "/users/{id}");
    assert_eq!(routes[0].path_params, vec!["id".to_string()]);
}

#[test]
fn express_path_param_extracted() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.js",
        "const app = express();\napp.get(\"/users/:id\", (req, res) => res.send(req.params.id));\n",
    );
    let index = build(tmp.path());
    let routes: Vec<_> = index
        .cross_language
        .routes
        .iter()
        .filter(|r| r.framework == "express")
        .collect();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].route, "/users/:id");
    assert_eq!(routes[0].path_params, vec!["id".to_string()]);
}

#[test]
fn axum_path_param_extracted() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "src/server.rs",
        "use axum::{Router, routing::get};\nasync fn show() {}\nfn app() -> Router { Router::new().route(\"/items/{slug}\", get(show)) }\n",
    );
    let index = build(tmp.path());
    let routes: Vec<_> = index
        .cross_language
        .routes
        .iter()
        .filter(|r| r.framework == "axum")
        .collect();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].route, "/items/{slug}");
    assert_eq!(routes[0].path_params, vec!["slug".to_string()]);
}

// ---------------------------------------------------------------------------
// Tiered resolution
// ---------------------------------------------------------------------------

#[test]
fn literal_client_to_flask_template_route_yields_template_match() {
    // `requests.get("/users/42")` + Flask `/users/<int:id>` — segment
    // unification gives confidence 70, `MatchKind::Template`.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "@app.route(\"/users/<int:id>\")\ndef u(id): return id\n",
    );
    write(
        tmp.path(),
        "client.py",
        "import requests\nrequests.get(\"http://localhost/users/42\")\n",
    );
    let index = build(tmp.path());
    let edges = &index.cross_language.resolved_http_edges;
    assert_eq!(
        edges.len(),
        1,
        "expected one templated edge; got: {:#?}",
        edges
    );
    assert_eq!(edges[0].confidence, 70);
    assert_eq!(edges[0].match_kind, MatchKind::Template);
    assert_eq!(edges[0].route_path, "/users/<int:id>");
}

#[test]
fn bare_fetch_to_fastapi_template_route_yields_methodless_template_match() {
    // `fetch("/users/42")` carries method `*` — even though the FastAPI
    // route declares GET, we downgrade to `TemplateMethodless` (60).
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "@app.get(\"/users/{id}\")\ndef u(): return 1\n",
    );
    write(tmp.path(), "client.ts", "fetch(\"/users/42\")\n");
    let index = build(tmp.path());
    let edges = &index.cross_language.resolved_http_edges;
    assert_eq!(
        edges.len(),
        1,
        "expected one templated edge; got: {:#?}",
        edges
    );
    assert_eq!(edges[0].confidence, 60);
    assert_eq!(edges[0].match_kind, MatchKind::TemplateMethodless);
}

#[test]
fn template_client_to_express_template_route_yields_template_match() {
    // Backtick-template client `fetch(\`/users/${id}\`)` against Express
    // template `/users/:id` — both templated, segments align. Method on
    // bare `fetch` is `*`, so we still downgrade to `TemplateMethodless`.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.js",
        "const app = express();\napp.get(\"/users/:id\", h);\n",
    );
    write(tmp.path(), "client.ts", "fetch(`/users/${id}`)\n");
    let index = build(tmp.path());
    let edges = &index.cross_language.resolved_http_edges;
    assert_eq!(
        edges.len(),
        1,
        "expected one templated edge; got: {:#?}",
        edges
    );
    // Method-aware variant of the same scenario via axios.
    assert!(matches!(
        edges[0].match_kind,
        MatchKind::Template | MatchKind::TemplateMethodless
    ));
}

#[test]
fn axios_template_client_to_express_template_route_keeps_method() {
    // With `axios.get`, the method is known — template match at 70 with
    // `MatchKind::Template`.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.js",
        "const app = express();\napp.get(\"/users/:id\", h);\n",
    );
    write(tmp.path(), "client.ts", "axios.get(`/users/${id}`)\n");
    let index = build(tmp.path());
    let edges = &index.cross_language.resolved_http_edges;
    assert_eq!(edges.len(), 1, "got: {:#?}", edges);
    assert_eq!(edges[0].confidence, 70);
    assert_eq!(edges[0].match_kind, MatchKind::Template);
}

#[test]
fn normalized_trailing_slash_match_yields_85() {
    // Client `/users/` vs route `/users` — same path after trailing-slash
    // normalization. Tier 2.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "@app.route(\"/users\")\ndef u(): return 1\n",
    );
    write(
        tmp.path(),
        "client.py",
        "import requests\nrequests.get(\"/users/\")\n",
    );
    let index = build(tmp.path());
    let edges = &index.cross_language.resolved_http_edges;
    assert_eq!(edges.len(), 1, "got: {:#?}", edges);
    assert_eq!(edges[0].confidence, 85);
    assert_eq!(edges[0].match_kind, MatchKind::Normalized);
}

#[test]
fn mismatched_segment_count_yields_no_edge() {
    // Route `/users/{id}` cannot unify with URL `/users/42/extra` —
    // different segment counts.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "@app.get(\"/users/{id}\")\ndef u(): return 1\n",
    );
    write(
        tmp.path(),
        "client.py",
        "import requests\nrequests.get(\"/users/42/extra\")\n",
    );
    let index = build(tmp.path());
    assert!(
        index.cross_language.resolved_http_edges.is_empty(),
        "segment count mismatch must not resolve"
    );
}

#[test]
fn mismatched_literal_segment_yields_no_edge() {
    // Route `/users/{id}` cannot match URL `/admins/42` — the literal
    // first segment differs.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "server.py",
        "@app.get(\"/users/{id}\")\ndef u(): return 1\n",
    );
    write(
        tmp.path(),
        "client.py",
        "import requests\nrequests.get(\"/admins/42\")\n",
    );
    let index = build(tmp.path());
    assert!(
        index.cross_language.resolved_http_edges.is_empty(),
        "literal segment mismatch must not resolve"
    );
}
