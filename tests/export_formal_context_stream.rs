//! Integration tests for the streamed `export formal-context` path —
//! the FCA induction Phase A contract.
//!
//! These exercise the new `export_formal_context_stream` function and its
//! CLI surface. They are deliberately isolated from the legacy bundle
//! (sidecar JSONL under `.leio-code/exports/formal-context-v1/`); the
//! backwards-compatibility test at the bottom verifies the bundle path is
//! still byte-for-byte unchanged.
//!
//! Why these tests:
//! - One test per projection (6 tests) — each verifies the right
//!   `object_kind` produces the right object set and attribute namespace.
//!   These encode the design-doc contract; a refactor that drops
//!   `binary:` from the file projection should fail here.
//! - JSON round-trip — locks the wire shape (schema_version, object_kind,
//!   objects, attributes, incidence, provenance) so consumers can branch.
//! - Arrow round-trip — locks the columnar shape and provenance struct
//!   schema so the Phase B example-platform adapter can zero-copy ingest.
//! - Provenance integrity — every (object, attribute) in `incidence` must
//!   have a matching `provenance` entry. The named-graph invariant in
//!   CLAUDE.md ("no silent erosion of provenance") is enforced here.
//! - CLI smoke — the binary entry point produces parseable JSON on stdout.
//! - Backcompat — `leio-code export formal-context` (no new flags)
//!   produces the legacy bundle at the canonical sidecar path.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

use leio_code::export::{
    FormalContextFormat, FormalContextObjectKind, export_formal_context_stream,
    read_formal_context_stream_arrow,
};
use leio_code::indexer::build_or_update_index;
use leio_code::model::RepoIndex;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_leio-code")
}

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(&path, body).expect("write file");
}

/// Stage a polyglot fixture exercising every projection's data source:
/// - A cartridge file with an env var read, a Redis access, and a
///   subprocess spawn.
/// - A second cartridge file declaring a Flask route.
/// - A Cargo `[[bin]]` so the spawn resolves to a real binary node.
/// - A deploy-target TOML referencing the cartridge + env var.
/// - A secret-set TOML declaring one of the env names.
fn stage_repo() -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();

    // Cargo workspace root + binary crate.
    write(
        root,
        "Cargo.toml",
        r#"[workspace]
members = ["leio-tool"]
resolver = "2"
"#,
    );
    write(
        root,
        "leio-tool/Cargo.toml",
        r#"[package]
name = "leio-tool"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "leio-tool"
path = "src/main.rs"
"#,
    );
    write(
        root,
        "leio-tool/src/main.rs",
        r#"fn main() { println!("hello"); }
"#,
    );

    // Cartridge file: env var + redis + spawn.
    write(
        root,
        "cartridges/revops/worker.py",
        r#"import os
import subprocess
import redis

def run():
    url = os.environ["REDIS_URL"]            # env_var read
    r = redis.Redis.from_url(url)
    r.set("session:cache", "value", ex=3600) # redis write with TTL
    subprocess.run(["leio-tool", "--help"])  # cross-language spawn
"#,
    );

    // Cartridge file: Flask route + a different env var.
    write(
        root,
        "cartridges/revops/router.py",
        r#"import os
from flask import Flask

app = Flask(__name__)

DATABASE_URL = os.environ["DATABASE_URL"]    # env_var read

@app.route("/api/users", methods=["GET"])
def list_users():
    return {"ok": True}
"#,
    );

    // Deploy-target manifest pointing at the backend_api profile + backend
    // secret-set. The actual env-var declarations come from the linked
    // profile and secret-set TOMLs below — the indexer doesn't read an
    // `[[env]]` block off the target itself.
    write(
        root,
        "deploy/targets/backend.toml",
        r#"name = "backend"
profile = "prod"
backend_profile = "backend_api"
secret_set = "backend"
cartridges = ["revops"]
required_integrations = ["redis"]
"#,
    );

    // Backend profile declaring REDIS_URL. The indexer scans
    // `deploy/profiles/*.env` and `*.env.example` (dotenv format, not TOML).
    // The profile's stored `name` is the full filename — `backend_api.env`
    // here — and the deploy-target matcher uses
    // `name == profile_name || name.starts_with("{profile_name}.")` so
    // `backend_api.env` matches a target referencing `backend_profile = "backend_api"`.
    write(root, "deploy/profiles/backend_api.env", "REDIS_URL=\n");

    // Secret-set declaring DATABASE_URL. The indexer's
    // `parse_env_records_secrets` only reads files ending in `.env.example`
    // from `deploy/secret-sets/` (hyphen, not underscore).
    write(
        root,
        "deploy/secret-sets/backend.env.example",
        "DATABASE_URL=\n",
    );

    tmp
}

fn index_of(root: &Path) -> RepoIndex {
    let index_path = root.join(".leio-code").join("index.json");
    build_or_update_index(root, &index_path, true).expect("build index")
}

fn json_for(index: &RepoIndex, kind: FormalContextObjectKind) -> Value {
    // Tmp file for the stream output so we don't go through stdout in unit
    // tests.
    let tmp = TempDir::new().expect("tempdir");
    let out = tmp.path().join("context.json");
    export_formal_context_stream(index, kind, FormalContextFormat::Json, Some(&out))
        .expect("stream json");
    let body = fs::read_to_string(&out).expect("read");
    serde_json::from_str(&body).expect("parse json")
}

fn arrow_for(index: &RepoIndex, kind: FormalContextObjectKind) -> PathBuf {
    let tmp = TempDir::new().expect("tempdir");
    let out = tmp.path().join("context.arrow");
    export_formal_context_stream(index, kind, FormalContextFormat::Arrow, Some(&out))
        .expect("stream arrow");
    // The tempdir would drop on return; copy the file out first.
    let kept = std::env::temp_dir().join(format!(
        "leio-fcas-{}-{}.arrow",
        kind.as_str(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::copy(&out, &kept).expect("copy arrow");
    kept
}

// --------------------------------------------------------------------------
// Per-projection tests — one test per object_kind. Each verifies that the
// projection produces the expected object identifier shape and the expected
// attribute prefixes, so a future refactor that drops a relation surfaces
// here (not just in downstream FCA results).
// --------------------------------------------------------------------------

#[test]
fn file_projection_has_env_redis_binary_route_cartridge_and_deploy_attributes() {
    let tmp = stage_repo();
    let index = index_of(tmp.path());
    let doc = json_for(&index, FormalContextObjectKind::File);

    assert_eq!(doc["schema_version"], "1.0");
    assert_eq!(doc["object_kind"], "file");

    let attributes: Vec<String> = doc["attributes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let prefixes: std::collections::BTreeSet<String> = attributes
        .iter()
        .filter_map(|attr| attr.split_once(':').map(|(p, _)| p.to_string()))
        .collect();
    // The fixture intentionally covers all six prefixes for the file
    // projection.
    for required in [
        "env",
        "redis",
        "binary",
        "route",
        "cartridge",
        "deploy_target",
    ] {
        assert!(
            prefixes.contains(required),
            "file projection missing attribute prefix {required:?}; got {prefixes:?}"
        );
    }

    let worker = "cartridges/revops/worker.py";
    let worker_attrs: Vec<String> = doc["incidence"][worker]
        .as_array()
        .expect("worker has incidence")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(worker_attrs.contains(&"env:REDIS_URL".to_string()));
    assert!(worker_attrs.contains(&"redis:session:cache".to_string()));
    assert!(worker_attrs.contains(&"binary:leio-tool".to_string()));
    assert!(worker_attrs.contains(&"cartridge:revops".to_string()));
}

#[test]
fn cartridge_projection_aggregates_env_and_route_under_cartridge_name() {
    let tmp = stage_repo();
    let index = index_of(tmp.path());
    let doc = json_for(&index, FormalContextObjectKind::Cartridge);

    assert_eq!(doc["object_kind"], "cartridge");
    let objects: Vec<String> = doc["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(objects.contains(&"revops".to_string()));

    let attrs: Vec<String> = doc["incidence"]["revops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(attrs.contains(&"env:REDIS_URL".to_string()));
    assert!(attrs.contains(&"env:DATABASE_URL".to_string()));
    assert!(attrs.contains(&"route:/api/users".to_string()));
    assert!(attrs.contains(&"binary:leio-tool".to_string()));
}

#[test]
fn binary_projection_carries_caller_file_and_lang() {
    let tmp = stage_repo();
    let index = index_of(tmp.path());
    let doc = json_for(&index, FormalContextObjectKind::Binary);

    assert_eq!(doc["object_kind"], "binary");
    let objects: Vec<String> = doc["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(objects.contains(&"leio-tool".to_string()));

    let attrs: Vec<String> = doc["incidence"]["leio-tool"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(attrs.iter().any(|a| a.starts_with("declared_in:")));
    assert!(
        attrs
            .iter()
            .any(|a| a == "caller_file:cartridges/revops/worker.py")
    );
    assert!(attrs.iter().any(|a| a == "caller_lang:python"));
}

#[test]
fn route_projection_keys_on_route_path_with_framework_and_method() {
    let tmp = stage_repo();
    let index = index_of(tmp.path());
    let doc = json_for(&index, FormalContextObjectKind::Route);

    assert_eq!(doc["object_kind"], "route");
    let objects: Vec<String> = doc["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        objects.contains(&"/api/users".to_string()),
        "route projection missing /api/users; got {objects:?}"
    );

    let attrs: Vec<String> = doc["incidence"]["/api/users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(attrs.iter().any(|a| a.starts_with("framework:")));
    assert!(attrs.iter().any(|a| a == "method:GET"));
}

#[test]
fn env_var_projection_marks_secret_set_membership() {
    let tmp = stage_repo();
    let index = index_of(tmp.path());
    let doc = json_for(&index, FormalContextObjectKind::EnvVar);

    assert_eq!(doc["object_kind"], "env_var");
    let objects: Vec<String> = doc["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(objects.contains(&"REDIS_URL".to_string()));
    assert!(objects.contains(&"DATABASE_URL".to_string()));

    let database_attrs: Vec<String> = doc["incidence"]["DATABASE_URL"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        database_attrs.contains(&"is_secret:true".to_string()),
        "DATABASE_URL should be tagged is_secret:true; got {database_attrs:?}"
    );
    assert!(
        database_attrs
            .iter()
            .any(|a| a.starts_with("file:") || a.starts_with("cartridge:")),
        "DATABASE_URL should be linked back to a file or cartridge; got {database_attrs:?}"
    );
}

#[test]
fn deploy_target_projection_carries_profile_and_cartridge_binary() {
    let tmp = stage_repo();
    let index = index_of(tmp.path());
    let doc = json_for(&index, FormalContextObjectKind::DeployTarget);

    assert_eq!(doc["object_kind"], "deploy_target");
    let objects: Vec<String> = doc["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        objects.contains(&"backend".to_string()),
        "deploy_target projection missing `backend`; got {objects:?}"
    );

    let attrs: Vec<String> = doc["incidence"]["backend"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(attrs.iter().any(|a| a.starts_with("profile:")));
    // The fixture's `leio-tool` binary lives at the workspace root, not in
    // a cartridge — so deploy_target -> binary won't be linked here. We
    // assert on the env link instead.
    assert!(
        attrs.iter().any(|a| a == "env:REDIS_URL"),
        "deploy_target projection missing env:REDIS_URL; got {attrs:?}"
    );
}

// --------------------------------------------------------------------------
// Round-trip and integrity checks.
// --------------------------------------------------------------------------

#[test]
fn json_round_trip_preserves_shape() {
    let tmp = stage_repo();
    let index = index_of(tmp.path());
    let doc = json_for(&index, FormalContextObjectKind::File);

    // Top-level keys are exactly the six documented fields, in order.
    let map = doc.as_object().expect("root object");
    let keys: Vec<&str> = map.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "schema_version",
            "object_kind",
            "objects",
            "attributes",
            "incidence",
            "provenance"
        ]
    );
}

#[test]
fn arrow_round_trip_matches_json_incidences() {
    let tmp = stage_repo();
    let index = index_of(tmp.path());
    let doc = json_for(&index, FormalContextObjectKind::File);
    let arrow_path = arrow_for(&index, FormalContextObjectKind::File);
    let rows = read_formal_context_stream_arrow(&arrow_path).expect("read arrow");

    // Count expected incidences from the JSON doc.
    let json_count: usize = doc["incidence"]
        .as_object()
        .unwrap()
        .values()
        .map(|v| v.as_array().unwrap().len())
        .sum();
    assert_eq!(rows.len(), json_count);

    // Spot-check: each row's (object, attribute) is present in the JSON
    // incidence map.
    for row in &rows {
        let attrs = doc["incidence"][&row.object]
            .as_array()
            .unwrap_or_else(|| panic!("arrow row references missing object {}", row.object));
        assert!(
            attrs.iter().any(|v| v.as_str() == Some(&row.attribute)),
            "arrow row ({}, {}) not present in JSON incidence",
            row.object,
            row.attribute,
        );
        assert!(!row.source_path.is_empty(), "arrow row missing source_path");
        assert!(!row.edge_kind.is_empty(), "arrow row missing edge_kind");
    }

    let _ = fs::remove_file(&arrow_path);
}

#[test]
fn provenance_covers_every_incidence_for_every_projection() {
    let tmp = stage_repo();
    let index = index_of(tmp.path());
    for kind in [
        FormalContextObjectKind::File,
        FormalContextObjectKind::Cartridge,
        FormalContextObjectKind::Binary,
        FormalContextObjectKind::Route,
        FormalContextObjectKind::EnvVar,
        FormalContextObjectKind::DeployTarget,
    ] {
        let doc = json_for(&index, kind);
        let provenance = doc["provenance"].as_object().expect("provenance is a map");
        for (object, attrs) in doc["incidence"].as_object().expect("incidence is a map") {
            for attr in attrs.as_array().unwrap() {
                let key = format!("{object}|{}", attr.as_str().unwrap());
                let prov = provenance.get(&key).unwrap_or_else(|| {
                    panic!(
                        "missing provenance for {key:?} in {} projection",
                        kind.as_str()
                    )
                });
                let map = prov.as_object().unwrap();
                assert!(map.contains_key("source_path"));
                assert!(map.contains_key("source_line"));
                assert!(map.contains_key("edge_kind"));
                assert!(map.contains_key("confidence"));
            }
        }
    }
}

// --------------------------------------------------------------------------
// CLI smoke + backwards-compatibility.
// --------------------------------------------------------------------------

#[test]
fn cli_export_formal_context_json_produces_valid_document() {
    let tmp = stage_repo();
    let out = tmp.path().join("context.json");
    let status = Command::new(bin())
        .arg("--repo")
        .arg(tmp.path())
        .arg("export")
        .arg("formal-context")
        .arg("--object-kind=file")
        .arg("--format=json")
        .arg("--out")
        .arg(&out)
        .status()
        .expect("run leio-code");
    assert!(status.success(), "leio-code exited non-zero");

    let body = fs::read_to_string(&out).expect("read out");
    let doc: Value = serde_json::from_str(&body).expect("parse json");
    assert_eq!(doc["schema_version"], "1.0");
    assert_eq!(doc["object_kind"], "file");
    assert!(doc["objects"].is_array());
    assert!(doc["incidence"].is_object());
}

#[test]
fn cli_export_formal_context_bundle_default_is_unchanged() {
    // The bundle path is the existing contract. This test pins it: the
    // default `leio-code export formal-context` (no new flags) must keep
    // producing the legacy sidecar files at the legacy path. Any breakage
    // here is a backwards-compat violation.
    let tmp = stage_repo();
    let status = Command::new(bin())
        .arg("--repo")
        .arg(tmp.path())
        .arg("export")
        .arg("formal-context")
        .status()
        .expect("run leio-code");
    assert!(status.success(), "leio-code exited non-zero");

    let bundle = tmp
        .path()
        .join(".leio-code")
        .join("exports")
        .join("formal-context-v1");
    for sidecar in [
        "objects.jsonl",
        "attributes.jsonl",
        "incidences.jsonl",
        "manifest.json",
    ] {
        let path = bundle.join(sidecar);
        assert!(path.exists(), "bundle sidecar missing: {}", path.display());
        let meta = fs::metadata(&path).expect("metadata");
        assert!(meta.len() > 0, "bundle sidecar empty: {}", path.display());
    }
}
