//! End-to-end coverage of every `leio-code` CLI family against one fixture.
//!
//! Each test shells out to `CARGO_BIN_EXE_leio-code` and asserts the JSON
//! envelope (or on-disk artifact) for that verb. Kind *counts* are not
//! hard-coded — capabilities are read from the live envelope.
// Rust guideline compliant 2026-02-21

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

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

fn stage_repo() -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    unsafe {
        std::env::set_var("LEIO_DISABLE_SEARCH_SIDECAR", "1");
    }

    write(
        root,
        "Cargo.toml",
        "[package]\nname = \"surface-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[[bin]]\nname = \"surface-tool\"\npath = \"src/main.rs\"\n",
    );
    write(
        root,
        "src/main.rs",
        r#"fn main() {
    encode_payload();
}

fn encode_payload() {
    let _ = std::env::var("SURFACE_TOKEN");
}

fn unused_orphan() {}
"#,
    );
    write(
        root,
        "app.py",
        r#"import os
import redis

TOKEN = os.getenv("SURFACE_TOKEN")
r = redis.Redis()
r.get("session:surface-fixture")
"#,
    );
    write(
        root,
        "server.py",
        r#"from flask import Flask
app = Flask(__name__)

@app.route("/api/surface", methods=["GET"])
def surface():
    return "ok"
"#,
    );
    write(
        root,
        "docker-compose.yml",
        "services:\n  surface-api:\n    image: alpine\n    ports:\n      - \"8080:8080\"\n",
    );
    write(
        root,
        "README.md",
        "# Surface\n\n## Hours\n\nThe surface unit is open 05:30-23:00.\n",
    );
    write(
        root,
        "docs/alpha.md",
        "# Alpha\n\nIRI: http://example.org/kb#Alpha\n\nHours 05:30-23:00.\n",
    );
    write(
        root,
        "ont/unit.ttl",
        r#"@prefix : <http://example.org/kb#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .

:Alpha a :Unit ;
    rdfs:label "Alpha" ;
    :hoursWeekday "05:30-23:00" .
"#,
    );

    let git = Command::new("git")
        .args(["init", "-q"])
        .current_dir(root)
        .status()
        .expect("git init");
    assert!(git.success(), "git init must succeed");

    tmp
}

fn run_cli(repo: &Path, args: &[&str]) -> (String, String, i32) {
    let mut cmd = Command::new(bin());
    cmd.arg("--json").arg("--repo").arg(repo);
    for arg in args {
        cmd.arg(arg);
    }
    let out = cmd.output().expect("run leio-code");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

fn envelope(repo: &Path, args: &[&str]) -> Value {
    envelope_allowing(repo, args, &[0])
}

fn envelope_allowing(repo: &Path, args: &[&str], allowed_codes: &[i32]) -> Value {
    let (stdout, stderr, code) = run_cli(repo, args);
    assert!(
        allowed_codes.contains(&code),
        "args={args:?} exit={code} allowed={allowed_codes:?} stderr={stderr} stdout={stdout}"
    );
    serde_json::from_str(&stdout).unwrap_or_else(|err| {
        panic!("JSON parse failed for {args:?}: {err}; stdout={stdout}; stderr={stderr}")
    })
}

fn query_envelope(value: Value) -> Value {
    value.get("envelope").cloned().unwrap_or(value)
}

fn kind_of(value: &Value) -> &str {
    value["kind"].as_str().unwrap_or("")
}

fn assert_kind_family(value: &Value, family: &str) {
    let kind = kind_of(value);
    let query_id = value["query_id"].as_str().unwrap_or("");
    assert!(
        kind == family
            || kind.starts_with(family)
            || kind.contains(family)
            || query_id.contains(family),
        "expected kind family `{family}`, got kind={kind} query_id={query_id}"
    );
}

#[test]
fn index_writes_index_json() {
    let tmp = stage_repo();
    let env = envelope(tmp.path(), &["index"]);
    assert!(tmp.path().join(".leio-code/index.json").exists());
    assert!(
        env["summary"].as_str().unwrap_or("").contains("indexed"),
        "{}",
        env["summary"]
    );
}

#[test]
fn status_and_capabilities_agree_on_profile() {
    let tmp = stage_repo();
    let status_raw = envelope(tmp.path(), &["status"]);
    let status = query_envelope(status_raw.clone());
    let caps = envelope(tmp.path(), &["capabilities"]);
    let status_profile = status_raw["ci"]["profile"]
        .as_str()
        .or_else(|| status["entities"][0]["workspace_profile"].as_str())
        .or_else(|| status["meta"]["workspace_profile"].as_str())
        .expect("status profile");
    let caps_profile = caps["meta"]["workspace_profile"]
        .as_str()
        .or_else(|| caps["meta"]["workspace_capabilities"]["workspace_profile"].as_str())
        .or_else(|| caps["entities"][0]["workspace_profile"].as_str())
        .expect("capabilities profile");
    assert_eq!(status_profile, caps_profile);
    let find_kinds = caps["meta"]["workspace_capabilities"]["find_kinds"]
        .as_array()
        .or_else(|| caps["entities"][0]["find_kinds"].as_array())
        .expect("find_kinds");
    assert!(
        find_kinds.iter().any(|kind| kind == "symbol"),
        "{find_kinds:?}"
    );
}

#[test]
fn context_ranks_the_task_files() {
    let tmp = stage_repo();
    let env = envelope(tmp.path(), &["context", "encode_payload SURFACE_TOKEN"]);
    assert_eq!(env["kind"], "context");
    let files = env["entities"][0]["files_to_read"]
        .as_array()
        .or_else(|| env["meta"]["files_to_read"].as_array())
        .cloned()
        .unwrap_or_default();
    let blob = env.to_string();
    assert!(
        blob.contains("main.rs") || blob.contains("encode_payload") || !files.is_empty(),
        "{blob}"
    );
}

#[test]
fn find_symbol_env_route_redis_docker_and_binary() {
    let tmp = stage_repo();
    let symbol = envelope(tmp.path(), &["find", "symbol", "encode_payload"]);
    assert!(
        symbol["entities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["name"] == "encode_payload"),
        "{symbol}"
    );

    let env_var = envelope(tmp.path(), &["find", "env-var", "SURFACE_TOKEN"]);
    assert!(
        env_var["entities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["name"] == "SURFACE_TOKEN" || row["key"] == "SURFACE_TOKEN"),
        "{env_var}"
    );

    let route = envelope(tmp.path(), &["find", "api-route", "/api/surface"]);
    let route_blob = route.to_string();
    assert!(
        route_blob.contains("/api/surface") || route_blob.contains("surface"),
        "{route_blob}"
    );

    let redis = envelope(
        tmp.path(),
        &["find", "redis-key", "session:surface-fixture"],
    );
    assert!(
        redis["entities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["key"] == "session:surface-fixture"
                || row["name"] == "session:surface-fixture"),
        "{redis}"
    );

    let docker = envelope(tmp.path(), &["find", "docker-service", "surface-api"]);
    let docker_blob = docker.to_string();
    assert!(
        docker_blob.contains("surface-api") || !docker["entities"].as_array().unwrap().is_empty(),
        "{docker_blob}"
    );

    let binary = envelope(tmp.path(), &["find", "binary", "surface-tool"]);
    assert!(
        binary["entities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["name"] == "surface-tool"),
        "{binary}"
    );
}

#[test]
fn explain_env_var() {
    let tmp = stage_repo();
    let env_var = envelope(tmp.path(), &["explain", "env-var", "SURFACE_TOKEN"]);
    assert_eq!(env_var["kind"], "explain");
    let blob = env_var.to_string();
    assert!(blob.contains("SURFACE_TOKEN"), "{blob}");
}

#[test]
fn graph_symbols_callers_callees_callsites_imports_and_dead_code() {
    let tmp = stage_repo();
    let symbols = envelope(tmp.path(), &["graph", "symbols-in", "src/main.rs"]);
    assert!(
        symbols["entities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["name"] == "encode_payload" || row["symbol"] == "encode_payload"),
        "{symbols}"
    );

    let callers = envelope(tmp.path(), &["graph", "callers-of", "encode_payload"]);
    assert!(
        !callers["entities"].as_array().unwrap().is_empty()
            || callers["summary"].as_str().unwrap_or("").contains("caller"),
        "{callers}"
    );

    let callees = envelope(tmp.path(), &["graph", "callees-of", "main"]);
    assert_eq!(callees["kind"], "graph");

    let callsites = envelope(tmp.path(), &["graph", "callsites-of", "encode_payload"]);
    assert_eq!(callsites["kind"], "graph");

    let imports = envelope(tmp.path(), &["graph", "imports-in", "app.py"]);
    assert_eq!(imports["kind"], "graph");

    let importers = envelope(tmp.path(), &["graph", "importers-of", "app.py"]);
    assert_eq!(importers["kind"], "graph");

    let resolved_imports = envelope(tmp.path(), &["graph", "resolved-imports-in", "app.py"]);
    assert_eq!(resolved_imports["kind"], "graph");

    let resolved_importers = envelope(tmp.path(), &["graph", "resolved-importers-of", "app.py"]);
    assert_eq!(resolved_importers["kind"], "graph");

    let dead = envelope(tmp.path(), &["graph", "dead-code"]);
    assert_eq!(dead["kind"], "graph");
}

#[test]
fn doctor_baseline_ci_and_audit_json() {
    let tmp = stage_repo();
    let baseline = envelope_allowing(tmp.path(), &["doctor", "baseline"], &[0, 1]);
    assert_kind_family(&baseline, "doctor");

    let ci = envelope_allowing(tmp.path(), &["doctor", "ci"], &[0, 1]);
    assert_kind_family(&ci, "doctor");

    let all = envelope_allowing(tmp.path(), &["doctor", "all"], &[0, 1]);
    assert_kind_family(&all, "doctor");

    let audit = envelope_allowing(tmp.path(), &["audit", "--format", "json"], &[0, 1]);
    assert!(
        audit.get("kind").is_some()
            || audit.get("summary").is_some()
            || audit.get("doctors").is_some()
            || audit.get("report").is_some(),
        "{audit}"
    );
}

#[test]
fn verify_returns_an_envelope() {
    let tmp = stage_repo();
    let (stdout, stderr, code) = run_cli(tmp.path(), &["verify"]);
    assert!(
        code == 0 || code == 1,
        "verify should exit 0 or 1, got {code}; stderr={stderr}"
    );
    let env: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|_| panic!("verify stdout must be JSON; stdout={stdout}; stderr={stderr}"));
    assert!(
        env.get("summary").is_some() || env.get("kind").is_some(),
        "{env}"
    );
}

#[test]
fn export_formal_context_code_graph_arrow_nodes_and_hypergraph() {
    let tmp = stage_repo();
    let _ = envelope(tmp.path(), &["index"]);

    let formal = envelope(tmp.path(), &["export", "formal-context"]);
    assert!(
        tmp.path()
            .join(".leio-code/exports/formal-context-v1")
            .exists()
            || formal.to_string().contains("formal"),
        "{formal}"
    );

    let graph = envelope(tmp.path(), &["export", "code-graph"]);
    assert!(
        tmp.path().join(".leio-code/exports/code-graph-v1").exists()
            || graph.to_string().contains("graph"),
        "{graph}"
    );

    let nodes = envelope(tmp.path(), &["export", "arrow-nodes"]);
    assert!(
        tmp.path()
            .join(".leio-code/exports/arrow-nodes-v1")
            .exists()
            || nodes.to_string().contains("node"),
        "{nodes}"
    );

    let hyper = envelope(tmp.path(), &["export", "hypergraph"]);
    assert!(
        tmp.path().join(".leio-code/exports/hypergraph-v1").exists()
            || hyper.to_string().contains("hyper"),
        "{hyper}"
    );
}

#[test]
fn knowledge_compile_status_text_adaptive_explain_and_sparql() {
    let tmp = stage_repo();
    let compiled = envelope(tmp.path(), &["knowledge", "compile"]);
    assert!(
        tmp.path().join(".leio-code/exports/knowledge-v1").exists()
            || compiled["summary"]
                .as_str()
                .unwrap_or("")
                .contains("compile"),
        "{compiled}"
    );

    let status = envelope(tmp.path(), &["knowledge", "status"]);
    assert!(
        kind_of(&status).contains("knowledge")
            || kind_of(&status) == "status"
            || status["query_id"]
                .as_str()
                .is_some_and(|id| id.contains("knowledge")),
        "{status}"
    );

    let text = envelope(tmp.path(), &["knowledge", "text", "Alpha"]);
    assert_kind_family(&text, "knowledge");

    let adaptive = envelope(tmp.path(), &["knowledge", "adaptive", "Alpha"]);
    assert_kind_family(&adaptive, "knowledge");

    let explained = envelope(tmp.path(), &["knowledge", "explain", "hours Alpha"]);
    let grounded = explained["meta"]["grounded"].as_bool();
    assert!(
        grounded == Some(true)
            || explained["summary"]
                .as_str()
                .unwrap_or("")
                .starts_with("refused")
            || explained["warnings"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row == "ungrounded")),
        "{explained}"
    );

    let sparql = envelope(
        tmp.path(),
        &[
            "knowledge",
            "sparql",
            "SELECT ?s WHERE { ?s ?p ?o } LIMIT 4",
        ],
    );
    assert_kind_family(&sparql, "knowledge");
}

#[test]
fn nav_goto_here_and_session_isolation() {
    let tmp = stage_repo();
    let goto = envelope(tmp.path(), &["nav", "goto", "Alpha"]);
    assert_eq!(goto["kind"], "nav");

    let here = envelope(tmp.path(), &["nav", "here"]);
    assert_eq!(here["kind"], "nav");

    let (stdout, stderr, code) = Command::new(bin())
        .args([
            "--json",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--session",
            "surface-a",
            "nav",
            "goto",
            "Hours",
        ])
        .output()
        .map(|out| {
            (
                String::from_utf8_lossy(&out.stdout).to_string(),
                String::from_utf8_lossy(&out.stderr).to_string(),
                out.status.code().unwrap_or(-1),
            )
        })
        .expect("nav --session");
    assert_eq!(code, 0, "{stderr} {stdout}");
    assert!(
        tmp.path().join(".leio-code/sessions").exists() || stdout.contains("nav"),
        "{stdout}"
    );
}

#[test]
fn arrow_node_export_feeds_local_find() {
    let tmp = stage_repo();
    let exported = envelope(tmp.path(), &["export", "arrow-nodes"]);
    assert!(exported.to_string().contains("node"), "{exported}");

    // The exported store backs local ranked `find` lookups with no server.
    let found = envelope(tmp.path(), &["find", "symbol", "encode_payload"]);
    assert!(
        found["kind"] == "find" || found.to_string().contains("encode_payload"),
        "{found}"
    );
}

#[test]
fn version_reports_built_commit() {
    let tmp = TempDir::new().expect("tempdir");
    let (stdout, stderr, code) = run_cli(tmp.path(), &["--version"]);
    assert_eq!(code, 0, "stderr={stderr}");
    // "leio-code 2.6.0 (19d1aa0c6a77, clean|dirty)" — the commit suffix is
    // what makes a stale install visible at a glance.
    assert!(
        stdout
            .trim()
            .matches(|c: char| c.is_ascii_hexdigit())
            .count()
            >= 12,
        "stdout={stdout}"
    );
    assert!(
        stdout.contains(", clean") || stdout.contains(", dirty"),
        "stdout={stdout}"
    );
}

#[test]
fn init_on_fresh_tree_is_json() {
    let tmp = TempDir::new().expect("tempdir");
    write(tmp.path(), "hello.rs", "fn hello() {}\n");
    let env = envelope(tmp.path(), &["init"]);
    assert!(tmp.path().join(".leio-code/config.toml").exists());
    assert!(tmp.path().join(".leio-code/index.json").exists());
    assert!(tmp.path().join(".leio-code/.gitignore").exists());
    assert!(
        env["summary"].as_str().unwrap_or("").contains("init")
            || env["meta"]["config_action"].is_string()
            || env["entities"][0]["config_action"].is_string(),
        "{env}"
    );
}

#[test]
fn init_updates_root_gitignore_and_writes_internal_gitignore() {
    let tmp = TempDir::new().expect("tempdir");
    write(tmp.path(), ".git/config", "");
    write(tmp.path(), ".gitignore", "target/\n");
    write(tmp.path(), "hello.rs", "fn hello() {}\n");
    let _env = envelope(tmp.path(), &["init"]);
    assert!(tmp.path().join(".leio-code/.gitignore").exists());
    let root_gitignore = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
    assert!(root_gitignore.contains(".leio-code/"));
}

#[test]
fn find_jsonld_where_and_doctor_sarif() {
    let tmp = stage_repo();
    let (stdout, stderr, code) = Command::new(bin())
        .args([
            "--repo",
            tmp.path().to_str().unwrap(),
            "find",
            "symbol",
            "encode_payload",
            "--format",
            "jsonld",
        ])
        .output()
        .map(|out| {
            (
                String::from_utf8_lossy(&out.stdout).to_string(),
                String::from_utf8_lossy(&out.stderr).to_string(),
                out.status.code().unwrap_or(-1),
            )
        })
        .expect("find jsonld");
    assert_eq!(code, 0, "{stderr}");
    let doc: Value = serde_json::from_str(&stdout).expect("jsonld");
    assert!(
        doc.get("@context").is_some() || doc.to_string().contains("@id"),
        "{doc}"
    );

    let (sarif_out, sarif_err, sarif_code) = Command::new(bin())
        .args([
            "--repo",
            tmp.path().to_str().unwrap(),
            "doctor",
            "baseline",
            "--format",
            "sarif",
        ])
        .output()
        .map(|out| {
            (
                String::from_utf8_lossy(&out.stdout).to_string(),
                String::from_utf8_lossy(&out.stderr).to_string(),
                out.status.code().unwrap_or(-1),
            )
        })
        .expect("doctor sarif");
    assert_eq!(sarif_code, 0, "{sarif_err}");
    let sarif: Value = serde_json::from_str(&sarif_out).expect("sarif json");
    assert_eq!(sarif["version"], "2.1.0");
}

#[test]
fn find_infers_symbol_kind_when_omitted() {
    let tmp = stage_repo();
    // Run find with inferred kind: `find encode_payload` instead of `find symbol encode_payload`
    let (stdout, stderr, code) = Command::new(bin())
        .args([
            "--repo",
            tmp.path().to_str().unwrap(),
            "--json",
            "find",
            "encode_payload",
        ])
        .output()
        .map(|out| {
            (
                String::from_utf8_lossy(&out.stdout).to_string(),
                String::from_utf8_lossy(&out.stderr).to_string(),
                out.status.code().unwrap_or(-1),
            )
        })
        .expect("find with inferred symbol kind");

    assert_eq!(code, 0, "{stderr}");
    let envelope: Value = serde_json::from_str(&stdout).expect("envelope json");
    assert_eq!(envelope["kind"], "find");
    assert_eq!(envelope["confidence"], 0.95);
    let entities = envelope["entities"].as_array().expect("entities array");
    assert!(!entities.is_empty(), "must find encode_payload symbol");
    assert_eq!(entities[0]["name"], "encode_payload");
}

#[test]
fn upward_repo_discovery_from_subdirectory() {
    let tmp = stage_repo();
    let deep_dir = tmp.path().join("src").join("nested").join("deep");
    fs::create_dir_all(&deep_dir).expect("create deep dir");

    // Run status from deep directory WITHOUT --repo
    let (stdout, stderr, code) = Command::new(bin())
        .args(["--json", "status"])
        .current_dir(&deep_dir)
        .output()
        .map(|out| {
            (
                String::from_utf8_lossy(&out.stdout).to_string(),
                String::from_utf8_lossy(&out.stderr).to_string(),
                out.status.code().unwrap_or(-1),
            )
        })
        .expect("status from subdirectory");

    assert_eq!(code, 0, "{stderr}");
    let raw: Value = serde_json::from_str(&stdout).expect("status json");
    let envelope = query_envelope(raw);
    let entities = envelope["entities"].as_array().expect("entities");
    let file_count = entities[0]["file_count"].as_u64().unwrap_or(0);
    // Should index the whole repo, not just 0 or 1 files in the subfolder
    assert!(
        file_count >= 5,
        "must index enclosing repo files, got {file_count}"
    );

    // Must NOT create an orphan .leio-code in the deep subdirectory
    assert!(
        !deep_dir.join(".leio-code").exists(),
        "must not create .leio-code in subfolder"
    );
}

#[test]
fn working_tree_dirt_auto_refreshes_fresh_index() {
    let tmp = stage_repo();
    let repo_path = tmp.path();

    // 1. Initial query builds index
    let (_, _, code1) = Command::new(bin())
        .args([
            "--repo",
            repo_path.to_str().unwrap(),
            "find",
            "encode_payload",
        ])
        .output()
        .map(|out| ("", "", out.status.code().unwrap_or(-1)))
        .expect("initial find");
    assert_eq!(code1, 0);

    // 2. Modify working tree by appending a new symbol
    let main_path = repo_path.join("src").join("main.rs");
    let current_content = fs::read_to_string(&main_path).expect("read main.rs");
    let updated_content = format!("{current_content}\npub fn newly_added_agent_symbol() {{}}\n");
    // Ensure mtime advances
    std::thread::sleep(std::time::Duration::from_millis(15));
    fs::write(&main_path, updated_content).expect("write updated main.rs");

    // 3. Query the newly added symbol immediately without running `leio-code index`
    let (stdout, stderr, code2) = Command::new(bin())
        .args([
            "--repo",
            repo_path.to_str().unwrap(),
            "--json",
            "find",
            "newly_added_agent_symbol",
        ])
        .output()
        .map(|out| {
            (
                String::from_utf8_lossy(&out.stdout).to_string(),
                String::from_utf8_lossy(&out.stderr).to_string(),
                out.status.code().unwrap_or(-1),
            )
        })
        .expect("find newly added symbol");

    assert_eq!(code2, 0, "{stderr}");
    let envelope: Value = serde_json::from_str(&stdout).expect("find json");
    let entities = envelope["entities"].as_array().expect("entities");
    assert!(
        !entities.is_empty(),
        "stale index bug: newly added symbol must be found automatically"
    );
    assert_eq!(entities[0]["name"], "newly_added_agent_symbol");
}
#[test]
fn help_documents_conversation_provenance_exception() {
    let output = Command::new(bin())
        .arg("--help")
        .output()
        .expect("run help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(
            "Repository commands append a JSON-LD PROV event; conversation deliberately does not."
        ),
        "help must state the conversation journaling exception: {stdout}"
    );
    assert!(
        !stdout.contains("Every command appends a JSON-LD PROV event"),
        "help must not overstate event coverage: {stdout}"
    );
}

#[test]
fn kind_catalog_exposes_doctor_presets() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_leio-code"))
        .args(["--json", "capabilities", "--catalog"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let catalog = &envelope["entities"][0];
    for preset in ["all", "baseline", "ci"] {
        assert!(
            catalog["doctor_kinds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == preset),
            "missing {preset}"
        );
    }
    assert!(
        catalog["find_kinds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "text"),
        "missing text in find_kinds"
    );
    assert!(
        catalog["explain_kinds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "symbol"),
        "missing symbol in explain_kinds"
    );
}

#[test]
fn explain_symbol_find_text_and_context_diff_work_end_to_end() {
    let tmp = stage_repo();
    let _ = envelope(tmp.path(), &["index"]);

    let explained = envelope(tmp.path(), &["explain", "symbol", "encode_payload"]);
    assert_eq!(explained["kind"], "explain");
    let sym = &explained["entities"][0];
    assert_eq!(sym["name"], "encode_payload");
    assert!(
        sym["source_excerpt"]
            .as_str()
            .unwrap_or("")
            .contains("encode_payload"),
        "{explained}"
    );
    assert!(sym["callers"].is_array(), "{explained}");

    let text_hits = envelope(tmp.path(), &["find", "text", "encode_payload"]);
    assert_eq!(text_hits["kind"], "find");
    assert!(
        !text_hits["entities"].as_array().unwrap().is_empty(),
        "{text_hits}"
    );
    assert!(
        text_hits["entities"][0]["enclosing_symbol"].is_string()
            || text_hits["entities"][0]["snippet"].is_string(),
        "{text_hits}"
    );

    let diff_ctx = envelope(tmp.path(), &["context", "--diff"]);
    assert_eq!(diff_ctx["kind"], "context");
    assert!(
        diff_ctx["entities"][0]["diff_summary"].is_object(),
        "{diff_ctx}"
    );
}

#[test]
fn context_follows_changed_contracts_across_local_packages() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    for (path, body) in [
        (".gitignore", "/.leio-code/\n"),
        (
            "Cargo.toml",
            "[workspace]\nmembers=['viewer','island']\n[workspace.dependencies]\nkit={package='orchard',path='.'}\n[package]\nname='orchard'\nversion='0.1.0'\nedition='2021'\n[lib]\nname='orchard_lib'\n",
        ),
        (
            "viewer/Cargo.toml",
            "[package]\nname='viewer'\nversion='0.1.0'\nedition='2021'\n[dependencies]\nkit.workspace=true\n",
        ),
        (
            "island/Cargo.toml",
            "[package]\nname='island'\nversion='0.1.0'\nedition='2021'\n",
        ),
        ("src/lib.rs", "pub mod report;\npub mod compass;\n"),
        (
            "src/compass.rs",
            "pub fn bearing() -> usize { 4 }\npub fn inclination() -> usize { 7 }\n",
        ),
        (
            "viewer/src/main.rs",
            "mod bridge;\nfn main(){bridge::north();bridge::south();}\n",
        ),
        (
            "viewer/src/bridge.rs",
            "use kit::compass::{bearing, inclination};\npub fn north() { bearing(); }\npub fn south(){inclination();}\n",
        ),
        (
            "viewer/tests/runs.rs",
            r#"#[test] fn invokes(){let _ = env!("CARGO_BIN_EXE_viewer");}"#,
        ),
        (
            "tests/checks.rs",
            r#"#[test] fn angle(){assert_eq!(orchard_lib::compass::inclination(),7);}"#,
        ),
        (
            "tests/palette.rs",
            r#"#[test] fn palette(){assert_eq!(orchard_lib::report::catalog()["flavor"], "indigo");}"#,
        ),
        (
            "viewer/tests/shade.rs",
            r#"use kit::report::{catalog, SHADE as TONE}; #[test] fn shade(){assert_eq!(catalog().get("flavor").copied(), Some(TONE));}"#,
        ),
        (
            "tests/repaired.rs",
            r#"use orchard_lib::report::catalog; const SHADE: &str="violet"; #[test] fn repaired(){assert_eq!(catalog()["flavor"], SHADE, "indigo");}"#,
        ),
        (
            "tests/other_import.rs",
            r#"mod misc {pub const SHADE:&str="violet";} use misc::SHADE; #[test] fn other(){assert_eq!(orchard_lib::report::catalog()["flavor"], SHADE);}"#,
        ),
        (
            "tests/future.rs",
            r#"#[test] fn future(){assert_eq!(orchard_lib::report::catalog()["count"], "five");}"#,
        ),
        ("island/src/lib.rs", ""),
        (
            "island/tests/mirror.rs",
            r#"#[test] fn mirror(){ let x=std::collections::BTreeMap::from([("flavor","indigo")]); assert_eq!(x["flavor"], "indigo");}"#,
        ),
    ] {
        write(root, path, body);
    }
    let original = r#"pub const SHADE: &str = "indigo";
pub fn catalog() -> std::collections::BTreeMap<&'static str,&'static str> {
    std::collections::BTreeMap::from([("flavor", SHADE), ("count", "five")])
}
#[test] fn shape() { assert_eq!(catalog()["flavor"], SHADE); assert_eq!(catalog()["count"], "five"); }"#;
    write(root, "src/report.rs", original);
    git(&["add", "."]);
    git(&["commit", "-qm", "initial orchard"]);
    let changed = original
        .replace(r#"("flavor", SHADE)"#, r#"("flavor","violet")"#)
        .replace(r#"["flavor"], SHADE"#, r#"["flavor"], "violet""#);
    write(root, "src/report.rs", &changed);
    git(&["commit", "-qam", "switch palette"]);
    write(root, "notes.txt", "unrelated work");
    git(&["add", "notes.txt"]);
    git(&["commit", "-qm", "record notes"]);
    write(root, "src/unrelated.rs", "pub fn other() -> bool { true }");
    git(&["add", "src/unrelated.rs"]);
    git(&["commit", "-qm", "add unrelated helper"]);
    write(
        root,
        "src/report.rs",
        &changed.replace("\"five\"", "\"nine\""),
    );
    git(&["commit", "-qam", "future change"]);
    git(&["checkout", "-q", "--detach", "HEAD^"]);
    envelope(root, &["index"]);
    let query = |limit: &str| {
        envelope(
            root,
            &[
                "context",
                "Expand test coverage. Update `viewer/src/main.rs`.",
                "--full",
                "--limit",
                limit,
            ],
        )
    };
    let small = query("4");
    let paths: Vec<_> = small["entities"][0]["files_to_read"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        [
            "viewer/src/main.rs",
            "tests/palette.rs",
            "viewer/tests/shade.rs",
            "viewer/src/bridge.rs"
        ]
    );
    for limit in ["1", "2"] {
        let limited = query(limit);
        let files = limited["entities"][0]["files_to_read"].as_array().unwrap();
        assert_eq!(files.len(), limit.parse::<usize>().unwrap());
        assert_eq!(files[0]["path"], "viewer/src/main.rs");
    }
    let ablated = Command::new(bin())
        .args(["--json", "--repo"])
        .arg(root)
        .args([
            "context",
            "Expand test coverage. Update `viewer/src/main.rs`.",
            "--full",
            "--limit",
            "40",
        ])
        .env("LEIO_CONTEXT_ABLATION", "no_playbook")
        .output()
        .unwrap();
    assert!(ablated.status.success());
    assert!(
        !String::from_utf8(ablated.stdout)
            .unwrap()
            .contains("test still asserts")
    );
    let medium = query("5");
    assert!(
        medium["entities"][0]["files_to_read"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == "src/compass.rs"),
        "an observed coverage gap should keep the final slot ahead of a generic binary fallback"
    );
    let wide = query("40");
    let files = wide["entities"][0]["files_to_read"].as_array().unwrap();
    let reasons = |needle: &str| {
        files
            .iter()
            .filter(|f| {
                f["reasons"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|r| r.as_str().unwrap().contains(needle))
            })
            .map(|f| f["path"].as_str().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        reasons("test still asserts"),
        ["tests/palette.rs", "viewer/tests/shade.rs"]
    );
    assert!(reasons("local dependency").contains(&"viewer/src/bridge.rs"));
    let gap = files
        .iter()
        .find(|f| f["path"] == "src/compass.rs")
        .unwrap()["reasons"]
        .to_string();
    assert!(
        gap.contains("bearing") && !gap.contains("inclination"),
        "{gap}"
    );
    for path in ["src/lib.rs", "src/compass.rs"] {
        let original = fs::read_to_string(root.join(path)).unwrap();
        write(root, path, &(original + "\n"));
    }
    let diff = envelope(root, &["context", "--diff", "--limit", "2"]);
    let actual: std::collections::BTreeSet<_> = diff["entities"][0]["files_to_read"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["path"].as_str())
        .collect();
    assert_eq!(
        actual,
        std::collections::BTreeSet::from(["src/lib.rs", "src/compass.rs"])
    );
}
