//! Test route-mount integrity doctor.
//!
//! Catches tests that POST/GET a route path that no router mounted by the test
//! actually serves — the failure mode behind the `handoff_release` 404s, where
//! a test client hit a path whose route had moved or been removed.
//!
//! Scope (narrow, by design): the same WhatsApp / integration test files that
//! build a FastAPI test app via `include_router(...)`. For each such file we:
//!   (a) find which router module(s) it imports + mounts
//!       (`from <mod> import router`/`import <mod>` + `app.include_router(router)`);
//!   (b) collect literal request paths it hits (`client.post("/…")`,
//!       `client.get("/…")`);
//!   (c) verify each tested path is served by SOME `@router.<method>("<suffix>")`
//!       in a mounted router module, combined with that router's `prefix=`.
//!
//! Robustness choices (lean lenient to avoid false positives; the unit tests
//! prove it still fires on a genuinely-missing route):
//!   - A mounted router module that is a pure re-export
//!     (`sys.modules[__name__] = <alias>` + `import <real> as <alias>`) is
//!     followed to the real module file before collecting routes.
//!   - Route prefixes come from the router's own `APIRouter(prefix="…")`.
//!     A mount-level `include_router(router, prefix="…")` is NOT currently
//!     parsed (no curated test mounts with one); a test that did would need
//!     that handled to avoid a false positive. Documented limitation.
//!   - Path-param segments (`{id}`) match any single segment.
//!   - If a tested path matches ANY mounted route, it passes.
//!   - If the file mounts no resolvable router, or a tested path can't be
//!     reduced to a literal, SKIP it (no warn).

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct TestRouteMountIntegrityDoctor;

impl Doctor for TestRouteMountIntegrityDoctor {
    fn name(&self) -> &'static str {
        "test-route-mount-integrity"
    }

    fn description(&self) -> &'static str {
        "Catches tests in curated integration test files that hit a route path not served by any router the test mounts (the handoff_release 404 class of drift)."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_test_route_mount_integrity(root)
    }
}

const TEST_FILES: &[&str] = &[
    "example-api/example/tests/api/test_whatsapp_router.py",
    "example-api/example/tests/api/test_whatsapp_webhook_signature_fallback.py",
    "example-api/example/tests/api/test_egress_firewall.py",
    "example-api/example/tests/api/test_jaipay_webhook.py",
    "cartridges/plusoft/tests/test_plusoft_contracts.py",
];

/// A mounted route: full path = prefix + decorator suffix.
struct MountedRoute {
    method: String,
    full_path: String,
}

fn module_to_file(dotted: &str) -> Option<String> {
    let parts: Vec<&str> = dotted.split('.').collect();
    match *parts.first()? {
        "example" => Some(format!("example-api/{}", parts.join("/"))),
        "cartridges" => Some(parts.join("/")),
        _ => None,
    }
}

fn resolve_module_file(root: &Path, dotted: &str) -> Option<String> {
    let base = module_to_file(dotted)?;
    let py = format!("{base}.py");
    let init = format!("{base}/__init__.py");
    if root.join(&py).is_file() {
        Some(py)
    } else if root.join(&init).is_file() {
        Some(init)
    } else {
        None
    }
}

/// Collect `from <dotted> import router` / `from <dotted> import router as X`
/// and `import <dotted> as X` bindings used as routers. Returns the set of
/// dotted module paths whose `router` symbol is imported.
fn imported_router_modules(body: &str) -> Vec<String> {
    let mut modules = Vec::new();
    for line in body.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("from ") {
            // from <dotted> import router[ as X]
            if let Some(imp) = rest.find(" import ") {
                let dotted = rest[..imp].trim().to_string();
                let names = &rest[imp + " import ".len()..];
                if names
                    .split(&[',', ' ', '(', ')'][..])
                    .any(|tok| tok.trim() == "router")
                {
                    modules.push(dotted);
                }
            }
        } else if let Some(rest) = t.strip_prefix("import ") {
            // import <dotted> as X  (router referenced as X.router elsewhere)
            let rest = rest.trim();
            if let Some(as_idx) = rest.find(" as ") {
                let dotted = rest[..as_idx].trim().to_string();
                modules.push(dotted);
            }
        }
    }
    modules
}

/// Extract a quoted string value following `key=`. Returns the literal inside
/// the quotes for `prefix="…"` style args.
fn quoted_after(line: &str, key: &str) -> Option<String> {
    let idx = line.find(key)?;
    let after = &line[idx + key.len()..];
    let after = after.trim_start();
    let q = after.chars().next()?;
    if q != '"' && q != '\'' {
        return None;
    }
    let rest = &after[1..];
    let end = rest.find(q)?;
    Some(rest[..end].to_string())
}

/// If `module_file` is a pure re-export alias (`sys.modules[__name__] = X` plus
/// `import <real> as X`), resolve and return the real module file. Else None.
fn follow_reexport(root: &Path, body: &str) -> Option<String> {
    let mut alias: Option<String> = None;
    for line in body.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("sys.modules[__name__] = ") {
            alias = Some(rest.trim().to_string());
        }
    }
    let alias = alias?;
    for line in body.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("import ")
            && let Some(as_idx) = rest.find(" as ")
        {
            let dotted = rest[..as_idx].trim();
            let bound = rest[as_idx + 4..].trim();
            if bound == alias {
                return resolve_module_file(root, dotted);
            }
        }
        if let Some(rest) = t.strip_prefix("from ") {
            // from pkg import mod as alias
            if let Some(imp) = rest.find(" import ") {
                let pkg = rest[..imp].trim();
                let names = &rest[imp + " import ".len()..];
                for part in names.split(',') {
                    let part = part.trim();
                    if let Some(as_idx) = part.find(" as ") {
                        let modname = part[..as_idx].trim();
                        let bound = part[as_idx + 4..].trim();
                        if bound == alias {
                            return resolve_module_file(root, &format!("{pkg}.{modname}"));
                        }
                    }
                }
            }
        }
    }
    None
}

/// Gather mounted routes from a router-module file body, given its prefix.
fn collect_routes_from_module(
    root: &Path,
    module_file: &str,
    mount_prefix: &str,
) -> Vec<MountedRoute> {
    let mut io = Vec::new();
    let Some(mut body) = read_text(&root.join(module_file), &mut io) else {
        return Vec::new();
    };
    // Follow a pure re-export alias to the real module.
    if let Some(real) = follow_reexport(root, &body)
        && let Some(real_body) = read_text(&root.join(&real), &mut io)
    {
        body = real_body;
    }

    // Router-internal prefix from `APIRouter(prefix="…")`.
    let mut router_prefix = String::new();
    for line in body.lines() {
        if line.contains("APIRouter(")
            && let Some(p) = quoted_after(line, "prefix=")
        {
            router_prefix = p;
            break;
        }
    }
    let full_prefix = format!("{mount_prefix}{router_prefix}");

    let mut routes = Vec::new();
    for line in body.lines() {
        let t = line.trim_start();
        for method in ["get", "post", "put", "delete", "patch"] {
            let deco = format!("@router.{method}(");
            if let Some(pos) = t.find(&deco) {
                let after = &t[pos + deco.len()..];
                let q = after.chars().next();
                if q == Some('"') || q == Some('\'') {
                    let qc = q.unwrap();
                    if let Some(end) = after[1..].find(qc) {
                        let suffix = &after[1..1 + end];
                        routes.push(MountedRoute {
                            method: method.to_string(),
                            full_path: format!("{full_prefix}{suffix}"),
                        });
                    }
                }
            }
        }
    }
    routes
}

/// Tested request: method + literal path.
struct TestedPath {
    method: String,
    path: String,
    line: usize,
}

/// Collect `client.post("/…")` / `client.get("/…")` literal-path requests.
/// Only literal string paths (no f-strings that interpolate the leading
/// segments) are collected; f-strings are skipped as non-literal.
fn collect_tested_paths(body: &str) -> Vec<TestedPath> {
    let mut paths = Vec::new();
    for (idx, line) in body.lines().enumerate() {
        for method in ["get", "post", "put", "delete", "patch"] {
            let needle = format!("client.{method}(");
            if let Some(pos) = line.find(&needle) {
                let after = &line[pos + needle.len()..];
                let after = after.trim_start();
                let q = after.chars().next();
                if q == Some('"') || q == Some('\'') {
                    let qc = q.unwrap();
                    if let Some(end) = after[1..].find(qc) {
                        let p = &after[1..1 + end];
                        if p.starts_with('/') {
                            paths.push(TestedPath {
                                method: method.to_string(),
                                path: p.to_string(),
                                line: idx + 1,
                            });
                        }
                    }
                }
            }
        }
    }
    paths
}

/// Match a tested path against a mounted route path, treating `{param}`
/// decorator segments as single-segment wildcards.
fn path_matches(tested: &str, route: &str) -> bool {
    let t: Vec<&str> = tested.trim_matches('/').split('/').collect();
    let r: Vec<&str> = route.trim_matches('/').split('/').collect();
    if t.len() != r.len() {
        return false;
    }
    t.iter().zip(r.iter()).all(|(ts, rs)| {
        if rs.starts_with('{') && rs.ends_with('}') {
            true // path param wildcard
        } else {
            ts == rs
        }
    })
}

pub fn doctor_test_route_mount_integrity(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut entities = Vec::new();

    let mut files_scanned = 0usize;
    let mut paths_checked = 0usize;

    for rel in TEST_FILES {
        // Missing/unreadable curated file -> skip silently. A deleted or moved
        // test file is out of scope for this doctor (it only validates routes
        // hit by tests that DO exist), and skipping keeps the doctor green.
        let mut io = Vec::new();
        let Some(body) = read_text(&root.join(rel), &mut io) else {
            continue;
        };

        // Must build an app via include_router to be in scope.
        if !body.contains("include_router(") {
            continue;
        }
        files_scanned += 1;

        // Mount-level prefixes from include_router(router, prefix="…") are rare;
        // default to empty. Collect router module candidates.
        let mut routes: Vec<MountedRoute> = Vec::new();
        for dotted in imported_router_modules(&body) {
            if let Some(module_file) = resolve_module_file(root, &dotted) {
                routes.extend(collect_routes_from_module(root, &module_file, ""));
            }
        }

        // If we couldn't resolve any mounted route, skip the file (out of scope).
        if routes.is_empty() {
            continue;
        }

        for tested in collect_tested_paths(&body) {
            paths_checked += 1;
            let served = routes
                .iter()
                .any(|r| r.method == tested.method && path_matches(&tested.path, &r.full_path));
            if !served {
                warnings.push(format!(
                    "{rel}:{}: tested {} {} has no matching mounted route",
                    tested.line,
                    tested.method.to_uppercase(),
                    tested.path
                ));
                evidence.push(EvidenceItem {
                    kind: "unmounted_tested_route".to_string(),
                    path: rel.to_string(),
                    line: Some(tested.line),
                    detail: format!(
                        "{} {} not served by any mounted router",
                        tested.method.to_uppercase(),
                        tested.path
                    ),
                });
            }
        }
    }

    entities.push(json!({
        "doctor": "test-route-mount-integrity",
        "files_scanned": files_scanned,
        "paths_checked": paths_checked,
        "test_files": TEST_FILES,
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_test_route_mount_integrity"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            format!(
                "test-route-mount-integrity: {paths_checked} tested path(s) across {files_scanned} file(s), all served by mounted routers"
            )
        } else {
            format!(
                "test-route-mount-integrity: {} tested path(s) with no matching mounted route",
                warnings.len()
            )
        },
        confidence: if warnings.is_empty() { 0.95 } else { 0.7 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "files_scanned": files_scanned,
            "paths_checked": paths_checked,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("leio_route_mount_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write(root: &Path, relative: &str, body: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn missing_route_warns() {
        let root = temp_root("missing-route");
        write(
            &root,
            "example-api/example/routers/conversations.py",
            "router = APIRouter(prefix=\"/v2/conversations\")\n@router.post(\"/{conv_id}/handoff\")\ndef h():\n    pass\n",
        );
        // Test mounts conversations router but hits /release which doesn't exist.
        write(
            &root,
            "example-api/example/tests/api/test_whatsapp_router.py",
            "from example.routers.conversations import router\napp.include_router(router)\nresp = client.post(\"/v2/conversations/conv_1/release\", json={})\n",
        );
        let envelope = doctor_test_route_mount_integrity(&root);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("/v2/conversations/conv_1/release")),
            "{:?}",
            envelope.warnings
        );
    }

    #[test]
    fn mounted_route_does_not_warn() {
        let root = temp_root("mounted-ok");
        write(
            &root,
            "example-api/example/routers/conversations.py",
            "router = APIRouter(prefix=\"/v2/conversations\")\n@router.post(\"/{conv_id}/release\")\ndef r():\n    pass\n",
        );
        write(
            &root,
            "example-api/example/tests/api/test_whatsapp_router.py",
            "from example.routers.conversations import router\napp.include_router(router)\nresp = client.post(\"/v2/conversations/conv_1/release\", json={})\n",
        );
        let envelope = doctor_test_route_mount_integrity(&root);
        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
    }

    #[test]
    fn follows_reexport_alias() {
        let root = temp_root("reexport");
        // Real router lives in webhook.py.
        write(
            &root,
            "example-api/example/integrations/whatsapp/routers/webhook.py",
            "router = APIRouter(prefix=\"/v2/whatsapp\")\n@router.post(\"/webhook\")\ndef w():\n    pass\n",
        );
        // handoff.py is a pure re-export alias.
        write(
            &root,
            "example-api/example/integrations/whatsapp/routers/handoff.py",
            "from example.integrations.whatsapp.routers import webhook as _webhook\nsys.modules[__name__] = _webhook\n",
        );
        write(
            &root,
            "example-api/example/tests/api/test_whatsapp_router.py",
            "from example.integrations.whatsapp.routers.handoff import router\napp.include_router(router)\nresp = client.post(\"/v2/whatsapp/webhook\", json={})\n",
        );
        let envelope = doctor_test_route_mount_integrity(&root);
        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
    }

    #[test]
    fn file_without_include_router_is_skipped() {
        let root = temp_root("no-mount");
        write(
            &root,
            "example-api/example/tests/api/test_egress_firewall.py",
            "resp = client.post(\"/anything\", json={})\n",
        );
        let envelope = doctor_test_route_mount_integrity(&root);
        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
    }
}
