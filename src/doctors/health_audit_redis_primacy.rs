//! Health-audit Redis-primacy doctor.
//!
//! Workspace invariant #4 — "Redis is the operational source of truth" —
//! requires audit-run state and the review queue to live in Redis, not
//! exclusively in on-disk JSON. Workstream C of the health-audit gaps
//! remediation introduces `cartridges/health_audit/redis_state.py` and
//! wires it into `cartridges/health_audit/routes/audit_runs.py`.
//!
//! This doctor flags any drift in that contract:
//!
//! 1. `redis_state.py` defines the Redis read, queue, and projection helpers.
//! 2. `router.py` projects central writes through the atomic Redis helper.
//! 3. `routes/audit_runs.py` reads and lists through the Redis helpers.
//!
//! Static check only — no Redis connection.

use std::path::Path;
use std::time::Instant;

use serde_json::json;
use tree_sitter::{Node, Parser, Tree};

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct HealthAuditRedisPrimacyDoctor;

impl Doctor for HealthAuditRedisPrimacyDoctor {
    fn name(&self) -> &'static str {
        "health-audit-redis-primacy"
    }

    fn description(&self) -> &'static str {
        "Verifies that the health_audit cartridge keeps run state and the review queue in Redis (workspace invariant #4) via redis_state.py and routes/audit_runs.py wiring."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_health_audit_redis_primacy(root)
    }
}

const REDIS_STATE_REL: &str = "cartridges/health_audit/redis_state.py";
const ROUTE_REL: &str = "cartridges/health_audit/routes/audit_runs.py";

const REQUIRED_EXPORTS: &[&str] = &[
    "lease_run",
    "set_run_state",
    "get_run_state",
    "enqueue_review",
    "dequeue_review",
    "list_review_queue",
    "project_run_state",
];

const ROUTER_REL: &str = "cartridges/health_audit/router.py";

pub fn doctor_health_audit_redis_primacy(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    // ---- 1. redis_state.py present and complete --------------------------
    let state_path = root.join(REDIS_STATE_REL);
    let state_body = if state_path.is_file() {
        let mut io = Vec::new();
        match read_text(&state_path, &mut io) {
            Some(b) => Some(b),
            None => {
                warnings.extend(io);
                None
            }
        }
    } else {
        warnings.push(format!(
            "{REDIS_STATE_REL}: missing — Redis primary for health_audit runs is not wired (workspace invariant #4)"
        ));
        None
    };

    let mut missing_exports: Vec<&str> = Vec::new();
    if let Some(body) = state_body.as_ref() {
        match PythonModule::parse(body) {
            Some(module) => {
                for export in REQUIRED_EXPORTS {
                    if !module.has_top_level_function(export) {
                        missing_exports.push(export);
                    }
                }
            }
            None => {
                warnings.push(format!(
                    "{REDIS_STATE_REL}: could not parse Python syntax for Redis helper verification"
                ));
                missing_exports.extend(REQUIRED_EXPORTS);
            }
        }
        if !missing_exports.is_empty() {
            for export in &missing_exports {
                warnings.push(format!(
                    "{REDIS_STATE_REL}: missing required export `{export}`"
                ));
            }
        }
        evidence.push(EvidenceItem {
            kind: "redis_state_module".to_string(),
            path: REDIS_STATE_REL.to_string(),
            line: None,
            detail: format!(
                "found {}/{} required helpers",
                REQUIRED_EXPORTS.len() - missing_exports.len(),
                REQUIRED_EXPORTS.len()
            ),
        });
    }

    // ---- 2. router.py projects central writes through Redis --------------
    let router_path = root.join(ROUTER_REL);
    if router_path.is_file() {
        let mut io = Vec::new();
        if let Some(body) = read_text(&router_path, &mut io) {
            if let Some(module) = PythonModule::parse(&body) {
                require_function_call(
                    &module,
                    "_write_audit_run_unlocked",
                    "_project_audit_run_to_redis",
                    ROUTER_REL,
                    &mut warnings,
                );
                require_function_call(
                    &module,
                    "_project_audit_run_to_redis",
                    "project_run_state",
                    ROUTER_REL,
                    &mut warnings,
                );
            } else {
                warnings.push(format!(
                    "{ROUTER_REL}: could not parse Python syntax for Redis projection verification"
                ));
            }
            evidence.push(EvidenceItem {
                kind: "audit_run_redis_projection".to_string(),
                path: ROUTER_REL.to_string(),
                line: None,
                detail: "checked central write and projection helper call chain".to_string(),
            });
        } else {
            warnings.extend(io);
        }
    } else {
        warnings.push(format!(
            "{ROUTER_REL}: missing — central audit-run writes cannot project to Redis"
        ));
    }

    // ---- 3. routes/audit_runs.py reads and lists through Redis -----------
    let route_path = root.join(ROUTE_REL);
    if route_path.is_file() {
        let mut io = Vec::new();
        if let Some(body) = read_text(&route_path, &mut io) {
            if let Some(module) = PythonModule::parse(&body) {
                require_function_call(
                    &module,
                    "get_audit_run",
                    "_read_current_audit_run",
                    ROUTE_REL,
                    &mut warnings,
                );
                require_function_call(
                    &module,
                    "_read_current_audit_run",
                    "get_run_state",
                    ROUTE_REL,
                    &mut warnings,
                );
                require_function_call(
                    &module,
                    "list_audit_review_queue",
                    "list_review_queue",
                    ROUTE_REL,
                    &mut warnings,
                );
            } else {
                warnings.push(format!(
                    "{ROUTE_REL}: could not parse Python syntax for Redis read verification"
                ));
            }
            evidence.push(EvidenceItem {
                kind: "audit_runs_route".to_string(),
                path: ROUTE_REL.to_string(),
                line: None,
                detail: "checked read and review-queue handler call chains".to_string(),
            });
        } else {
            warnings.extend(io);
        }
    } else {
        warnings.push(format!(
            "{ROUTE_REL}: missing — health_audit audit-run route not present in this repo snapshot"
        ));
    }

    entities.push(json!({
        "doctor": "health-audit-redis-primacy",
        "redis_state_present": state_body.is_some(),
        "missing_exports": missing_exports,
    }));

    let summary = if warnings.is_empty() {
        "health_audit cartridge keeps audit runs and the review queue in Redis (invariant #4 satisfied)".to_string()
    } else {
        format!(
            "health_audit Redis-primacy drift: {} warning(s)",
            warnings.len()
        )
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_health_audit_redis_primacy"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.95 } else { 0.6 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn require_function_call(
    module: &PythonModule<'_>,
    function: &str,
    callee: &str,
    path: &str,
    warnings: &mut Vec<String>,
) {
    if !module.function_calls(function, callee) {
        warnings.push(format!(
            "{path}: `{function}` does not call `{callee}(...)` in its function body — Redis-primary chain is incomplete"
        ));
    }
}

struct PythonModule<'source> {
    source: &'source str,
    tree: Tree,
}

impl<'source> PythonModule<'source> {
    fn parse(source: &'source str) -> Option<Self> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .ok()?;
        let tree = parser.parse(source, None)?;
        if tree.root_node().has_error() {
            return None;
        }
        Some(Self { source, tree })
    }

    fn has_top_level_function(&self, function: &str) -> bool {
        self.top_level_function(function).is_some()
    }

    fn function_calls(&self, function: &str, callee: &str) -> bool {
        let Some(function_node) = self.top_level_function(function) else {
            return false;
        };
        function_node
            .child_by_field_name("body")
            .is_some_and(|body| self.node_calls(body, callee))
    }

    fn top_level_function(&self, function: &str) -> Option<Node<'_>> {
        let root = self.tree.root_node();
        let mut cursor = root.walk();
        let mut found = None;
        for child in root.named_children(&mut cursor) {
            let candidate = match child.kind() {
                "function_definition" => Some(child),
                "decorated_definition" => child
                    .child_by_field_name("definition")
                    .filter(|definition| definition.kind() == "function_definition"),
                _ => None,
            };
            let Some(candidate) = candidate else {
                continue;
            };
            let Some(name) = candidate.child_by_field_name("name") else {
                continue;
            };
            if name
                .utf8_text(self.source.as_bytes())
                .is_ok_and(|value| value == function)
            {
                found = Some(candidate);
            }
        }
        found
    }

    fn node_calls(&self, node: Node<'_>, callee: &str) -> bool {
        if node.kind() == "call" && self.call_matches(node, callee) {
            return true;
        }
        if matches!(
            node.kind(),
            "function_definition" | "class_definition" | "decorated_definition" | "lambda"
        ) {
            return false;
        }
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .any(|child| self.node_calls(child, callee))
    }

    fn call_matches(&self, call: Node<'_>, callee: &str) -> bool {
        let Some(function) = call.child_by_field_name("function") else {
            return false;
        };
        let target = match function.kind() {
            "identifier" => Some(function),
            "attribute" => function.child_by_field_name("attribute"),
            _ => None,
        };
        target
            .and_then(|node| node.utf8_text(self.source.as_bytes()).ok())
            .is_some_and(|name| name == callee)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_repo(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-ha-redis-primacy-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn imports_comments_and_unrelated_dummy_calls_do_not_satisfy_wiring() {
        let root = temp_repo("ok");
        write(
            &root,
            REDIS_STATE_REL,
            r#"from somewhere import get_run_state, list_review_queue, project_run_state

async def lease_run(): ...
async def set_run_state(): ...
async def get_run_state(): ...
async def enqueue_review(): ...
async def dequeue_review(): ...
async def list_review_queue(): ...
def project_run_state(): ...
"#,
        );
        write(
            &root,
            ROUTE_REL,
            r#"# get_audit_run calls _read_current_audit_run()
# _read_current_audit_run calls get_run_state()
# list_audit_review_queue calls list_review_queue()
def unrelated_dummy():
    _project_audit_run_to_redis({})
    project_run_state("tenant", "run", {})
    get_run_state("tenant", "run")
    list_review_queue("tenant")

def _write_audit_run_unlocked(payload):
    pass

def _project_audit_run_to_redis(payload):
    pass

async def get_audit_run(request, run_id):
    pass  # _read_current_audit_run("tenant", run_id)
    message = "_read_current_audit_run(tenant, run_id)"
    """get_audit_run must call _read_current_audit_run(tenant, run_id)."""

async def _read_current_audit_run(tenant_id, run_id):
    pass  # get_run_state(tenant_id, run_id)
    message = "get_run_state(tenant_id, run_id)"

async def list_audit_review_queue(request):
    pass  # list_review_queue("tenant")
    message = "list_review_queue(tenant)"
"#,
        );
        write(
            &root,
            ROUTER_REL,
            r#"def _write_audit_run_unlocked(payload):
    pass  # _project_audit_run_to_redis(payload)
    message = "_project_audit_run_to_redis(payload)"

def _project_audit_run_to_redis(payload):
    pass  # project_run_state("tenant", "run", payload)
    message = "project_run_state(tenant, run, payload)"
"#,
        );
        let env = doctor_health_audit_redis_primacy(&root);
        assert_all_guarded_edges_warn(&env);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn module_docstrings_with_fake_definitions_and_calls_do_not_satisfy_wiring() {
        let root = temp_repo("module-docstring-fakes");
        write(
            &root,
            REDIS_STATE_REL,
            r#"
"""async def lease_run(): ...
async def set_run_state(): ...
async def get_run_state(): ...
async def enqueue_review(): ...
async def dequeue_review(): ...
async def list_review_queue(): ...
def project_run_state(): ...
"""
"#,
        );
        write(
            &root,
            ROUTER_REL,
            r#"
"""def _write_audit_run_unlocked(payload):
    _project_audit_run_to_redis(payload)

def _project_audit_run_to_redis(payload):
    project_run_state("tenant", "run", payload)
"""
"#,
        );
        write(
            &root,
            ROUTE_REL,
            r#"
"""async def get_audit_run(request, run_id):
    return await _read_current_audit_run("tenant", run_id)

async def _read_current_audit_run(tenant_id, run_id):
    return await get_run_state(tenant_id, run_id)

async def list_audit_review_queue(request):
    return await list_review_queue("tenant")
"""
"#,
        );
        let env = doctor_health_audit_redis_primacy(&root);
        assert_all_guarded_edges_warn(&env);
        assert!(
            env.warnings
                .iter()
                .any(|warning| warning.contains("missing required export `get_run_state`")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn escaped_triple_quotes_do_not_expose_fake_module_definitions() {
        let root = temp_repo("escaped-module-docstring-fakes");
        write(
            &root,
            REDIS_STATE_REL,
            r#"
"""An escaped delimiter remains text: \"""
async def lease_run(): ...
async def set_run_state(): ...
async def get_run_state(): ...
async def enqueue_review(): ...
async def dequeue_review(): ...
async def list_review_queue(): ...
def project_run_state(): ...
"""
"#,
        );
        write(
            &root,
            ROUTER_REL,
            r#"
"""An escaped delimiter remains text: \"""
def _write_audit_run_unlocked(payload):
    _project_audit_run_to_redis(payload)

def _project_audit_run_to_redis(payload):
    project_run_state("tenant", "run", payload)
"""
"#,
        );
        write(
            &root,
            ROUTE_REL,
            r#"
"""An escaped delimiter remains text: \"""
async def get_audit_run(request, run_id):
    return await _read_current_audit_run("tenant", run_id)

async def _read_current_audit_run(tenant_id, run_id):
    return await get_run_state(tenant_id, run_id)

async def list_audit_review_queue(request):
    return await list_review_queue("tenant")
"""
"#,
        );
        let env = doctor_health_audit_redis_primacy(&root);
        assert_all_guarded_edges_warn(&env);
        assert!(
            env.warnings
                .iter()
                .any(|warning| warning.contains("missing required export `get_run_state`")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn longer_identifiers_do_not_satisfy_canonical_calls() {
        let root = temp_repo("longer-identifiers");
        write(&root, REDIS_STATE_REL, complete_state_module());
        write(
            &root,
            ROUTER_REL,
            r#"def _write_audit_run_unlocked(payload):
    not__project_audit_run_to_redis(payload)

def _project_audit_run_to_redis(payload):
    not_project_run_state("tenant", "run", payload)
"#,
        );
        write(
            &root,
            ROUTE_REL,
            r#"async def get_audit_run(request, run_id):
    return await fallback__read_current_audit_run("tenant", run_id)

async def _read_current_audit_run(tenant_id, run_id):
    return await fallback_get_run_state(tenant_id, run_id)

async def list_audit_review_queue(request):
    return await legacy_list_review_queue("tenant")
"#,
        );
        let env = doctor_health_audit_redis_primacy(&root);
        assert_all_guarded_edges_warn(&env);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unicode_prefixed_identifiers_do_not_satisfy_canonical_calls() {
        let root = temp_repo("unicode-identifiers");
        write(&root, REDIS_STATE_REL, complete_state_module());
        write(
            &root,
            ROUTER_REL,
            r#"def _write_audit_run_unlocked(payload):
    é_project_audit_run_to_redis(payload)

def _project_audit_run_to_redis(payload):
    éproject_run_state("tenant", "run", payload)
"#,
        );
        write(
            &root,
            ROUTE_REL,
            r#"async def get_audit_run(request, run_id):
    return await é_read_current_audit_run("tenant", run_id)

async def _read_current_audit_run(tenant_id, run_id):
    return await éget_run_state(tenant_id, run_id)

async def list_audit_review_queue(request):
    return await élist_review_queue("tenant")
"#,
        );
        let env = doctor_health_audit_redis_primacy(&root);
        assert_all_guarded_edges_warn(&env);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn nested_uninvoked_call_decoys_do_not_satisfy_wiring() {
        let root = temp_repo("nested-call-decoys");
        write(&root, REDIS_STATE_REL, complete_state_module());
        write(
            &root,
            ROUTER_REL,
            r#"def _write_audit_run_unlocked(payload):
    def decoy():
        _project_audit_run_to_redis(payload)

def _project_audit_run_to_redis(payload):
    class Decoy:
        project_run_state("tenant", "run", payload)
"#,
        );
        write(
            &root,
            ROUTE_REL,
            r#"async def get_audit_run(request, run_id):
    decoy = lambda: _read_current_audit_run("tenant", run_id)

async def _read_current_audit_run(tenant_id, run_id):
    def decoy():
        return get_run_state(tenant_id, run_id)

async def list_audit_review_queue(request):
    class Decoy:
        queued = list_review_queue("tenant")
"#,
        );
        let env = doctor_health_audit_redis_primacy(&root);
        assert_all_guarded_edges_warn(&env);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_redis_state_warns() {
        let root = temp_repo("missing-state");
        write(
            &root,
            ROUTE_REL,
            "set_run_state()\nenqueue_review()\ndequeue_review()\n",
        );
        let env = doctor_health_audit_redis_primacy(&root);
        assert!(!env.warnings.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_get_handler_chain_warns() {
        let root = temp_repo("missing-get-chain");
        write(
            &root,
            REDIS_STATE_REL,
            r#"async def lease_run(): ...
async def set_run_state(): ...
async def get_run_state(): ...
async def enqueue_review(): ...
async def dequeue_review(): ...
async def list_review_queue(): ...
def project_run_state(): ...
"#,
        );
        write(&root, ROUTER_REL, complete_router_module());
        write(
            &root,
            ROUTE_REL,
            r#"def _write_audit_run_unlocked(payload):
    _project_audit_run_to_redis(payload)

def _project_audit_run_to_redis(payload):
    project_run_state("tenant", "run", payload)

async def get_audit_run(request, run_id):
    pass

async def _read_current_audit_run(tenant_id, run_id):
    return await get_run_state(tenant_id, run_id)

async def list_audit_review_queue(request):
    return await list_review_queue("tenant")
"#,
        );
        let env = doctor_health_audit_redis_primacy(&root);
        assert!(
            env.warnings
                .iter()
                .any(|warning| warning.contains("get_audit_run")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_queue_handler_call_warns() {
        let root = temp_repo("missing-queue-call");
        write(&root, REDIS_STATE_REL, complete_state_module());
        write(&root, ROUTER_REL, complete_router_module());
        write(
            &root,
            ROUTE_REL,
            complete_route_module()
                .replace(
                    "    return await list_review_queue(tenant_id)\n",
                    "    return []\n",
                )
                .as_str(),
        );
        let env = doctor_health_audit_redis_primacy(&root);
        assert!(
            env.warnings
                .iter()
                .any(|warning| warning.contains("list_audit_review_queue")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_central_write_projection_chain_warns() {
        let root = temp_repo("missing-write-projection");
        write(&root, REDIS_STATE_REL, complete_state_module());
        write(
            &root,
            ROUTER_REL,
            complete_router_module()
                .replace(
                    "    _project_audit_run_to_redis(payload)\n",
                    "    persist(payload)\n",
                )
                .as_str(),
        );
        write(&root, ROUTE_REL, complete_route_module());
        let env = doctor_health_audit_redis_primacy(&root);
        assert!(
            env.warnings
                .iter()
                .any(|warning| warning.contains("_write_audit_run_unlocked")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_projection_to_project_run_state_warns() {
        let root = temp_repo("missing-project-run-state");
        write(&root, REDIS_STATE_REL, complete_state_module());
        write(
            &root,
            ROUTER_REL,
            complete_router_module()
                .replace(
                    "    project_run_state(\"tenant\", \"run\", payload)\n",
                    "    persist(payload)\n",
                )
                .as_str(),
        );
        write(&root, ROUTE_REL, complete_route_module());
        let env = doctor_health_audit_redis_primacy(&root);
        assert!(
            env.warnings
                .iter()
                .any(|warning| warning.contains("_project_audit_run_to_redis")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_read_current_to_get_run_state_warns() {
        let root = temp_repo("missing-get-run-state");
        write(&root, REDIS_STATE_REL, complete_state_module());
        write(&root, ROUTER_REL, complete_router_module());
        write(
            &root,
            ROUTE_REL,
            complete_route_module()
                .replace(
                    "    return await get_run_state(tenant_id, run_id)\n",
                    "    return None\n",
                )
                .as_str(),
        );
        let env = doctor_health_audit_redis_primacy(&root);
        assert!(
            env.warnings
                .iter()
                .any(|warning| warning.contains("_read_current_audit_run")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn full_real_wiring_passes() {
        let root = temp_repo("full-real-wiring");
        write(&root, REDIS_STATE_REL, complete_state_module());
        write(&root, ROUTER_REL, complete_router_module());
        write(&root, ROUTE_REL, complete_route_module());
        let env = doctor_health_audit_redis_primacy(&root);
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }

    fn complete_state_module() -> &'static str {
        r#"async def lease_run(): ...
async def set_run_state(): ...
async def get_run_state(): ...
async def enqueue_review(): ...
async def dequeue_review(): ...
async def list_review_queue(): ...
def project_run_state(): ...
"#
    }

    fn complete_route_module() -> &'static str {
        r#"async def get_audit_run(request, run_id):
    return await _read_current_audit_run("tenant", run_id)

async def _read_current_audit_run(tenant_id, run_id):
    return await get_run_state(tenant_id, run_id)

async def list_audit_review_queue(request):
    tenant_id = "tenant"
    return await list_review_queue(tenant_id)
"#
    }

    fn complete_router_module() -> &'static str {
        r#"def _write_audit_run_unlocked(payload):
    _project_audit_run_to_redis(payload)

def _project_audit_run_to_redis(payload):
    project_run_state("tenant", "run", payload)
"#
    }

    fn assert_all_guarded_edges_warn(env: &QueryEnvelope) {
        for function in [
            "_write_audit_run_unlocked",
            "_project_audit_run_to_redis",
            "get_audit_run",
            "_read_current_audit_run",
            "list_audit_review_queue",
        ] {
            assert!(
                env.warnings
                    .iter()
                    .any(|warning| warning.contains(function)),
                "missing warning for {function}: {:?}",
                env.warnings
            );
        }
    }
}
