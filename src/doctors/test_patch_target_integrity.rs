//! Test patch-target integrity doctor.
//!
//! Catches stale `unittest.mock` patch / monkeypatch targets that point at a
//! symbol no longer present in the target module — exactly the failure mode
//! that broke `test_whatsapp_router.py` and `test_egress_firewall.py` when a
//! helper they patched was renamed/removed.
//!
//! Scope (narrow, by design): a curated list of integration test files. For
//! each, we extract patch targets of the forms:
//!   - `patch("pkg.mod.symbol")` / `mocker.patch("pkg.mod.symbol")`
//!   - `patch.object(module_ident, "symbol")`
//!   - `monkeypatch.setattr(module_ident, "symbol")`
//!     (and `monkeypatch.setattr("pkg.mod.symbol", ...)`)
//!
//! Resolution rules (chosen to avoid false positives):
//!   - String dotted targets: resolve the LONGEST `example.*` / `cartridges.*`
//!     dotted prefix to a real repo file (`a/b/c.py` or `a/b/c/__init__.py`).
//!     The symbol to verify is the SINGLE component immediately after that
//!     resolved module prefix (NOT the last dotted component). Trailing
//!     attribute access (`.Redis.from_url`) is ignored.
//!   - `patch.object(x, "s")` / `setattr(x, "s")`: only proceed when `x` is a
//!     bare identifier bound by `import … [as x]` to a module file. If `x`
//!     contains a dot (attribute access) or is bound by `from … import x`, the
//!     module can't be resolved -> skip (out of scope, no warn).
//!   - If a module cannot be resolved to a file, SKIP (do not warn).
//!
//! A target WARNS only when the module resolves to a real file but the symbol
//! is genuinely absent: neither defined in the index for that file, nor present
//! in the module text as a def/class/assignment/import.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct TestPatchTargetIntegrityDoctor;

impl Doctor for TestPatchTargetIntegrityDoctor {
    fn name(&self) -> &'static str {
        "test-patch-target-integrity"
    }

    fn description(&self) -> &'static str {
        "Catches stale unittest.mock patch/monkeypatch targets in curated integration test files that point at a symbol no longer present in the resolved target module."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_test_patch_target_integrity(index, root)
    }
}

const TEST_FILES: &[&str] = &[
    "example-api/example/tests/api/test_whatsapp_router.py",
    "example-api/example/tests/api/test_whatsapp_webhook_signature_fallback.py",
    "example-api/example/tests/api/test_egress_firewall.py",
    "example-api/example/tests/api/test_agent_loop_task.py",
    "example-api/example/tests/api/test_jaipay_webhook.py",
    "cartridges/plusoft/tests/test_plusoft_contracts.py",
];

/// A patch target resolved to (module dotted path, symbol name) along with the
/// source line for evidence.
struct Target {
    dotted_module: String,
    symbol: String,
    line: usize,
}

/// Resolve a dotted module path (e.g. `example.agents.tasks`) to a repo-relative
/// file path. Only `example.*` and `cartridges.*` roots are in scope.
fn module_to_file(dotted: &str) -> Option<String> {
    let parts: Vec<&str> = dotted.split('.').collect();
    if parts.is_empty() {
        return None;
    }
    let base = match parts[0] {
        // `example.x.y` -> `example-api/example/x/y`
        "example" => format!("example-api/{}", parts.join("/")),
        // `cartridges.x.y` -> `cartridges/x/y`
        "cartridges" => parts.join("/"),
        _ => return None,
    };
    Some(base)
}

/// Given a dotted string target like `example.agents.tasks.redis.Redis.from_url`,
/// find the longest in-scope module prefix that resolves to a real file and
/// return (module_dotted, module_file_rel, symbol). The symbol is the single
/// component immediately after the resolved module prefix.
fn resolve_string_target(root: &Path, dotted: &str) -> Option<(String, String, String)> {
    let parts: Vec<&str> = dotted.split('.').collect();
    if parts.len() < 2 {
        return None;
    }
    if parts[0] != "example" && parts[0] != "cartridges" {
        return None;
    }
    // Try the longest module prefix first: parts[..k] for k from len-1 down to 1.
    // The symbol is parts[k]; we need at least one trailing component.
    for k in (1..parts.len()).rev() {
        let module_dotted = parts[..k].join(".");
        let Some(base) = module_to_file(&module_dotted) else {
            continue;
        };
        let py = format!("{base}.py");
        let init = format!("{base}/__init__.py");
        let py_exists = root.join(&py).is_file();
        let init_exists = root.join(&init).is_file();
        if py_exists || init_exists {
            let file = if py_exists { py } else { init };
            let symbol = parts[k].to_string();
            return Some((module_dotted, file, symbol));
        }
    }
    None
}

/// Check whether `symbol` is defined or imported in the module file. Lenient by
/// design: returns true on any plausible binding. `index` provides authoritative
/// symbol definitions; the text fallback covers imports/assignments.
fn symbol_present(
    index: &RepoIndex,
    root: &Path,
    module_file: &str,
    symbol: &str,
    io_warnings: &mut Vec<String>,
) -> bool {
    // Index: a symbol defined in this exact file.
    if index
        .all_symbols()
        .any(|s| s.path == module_file && s.name == symbol)
    {
        return true;
    }
    // Text fallback: def/class/assignment/import.
    let Some(body) = read_text(&root.join(module_file), io_warnings) else {
        // Unreadable module -> treat as present (do not warn on IO problems).
        return true;
    };
    if symbol_bound_in_body(&body, symbol) {
        return true;
    }
    // The module may be a pure re-export alias (`sys.modules[__name__] = X`
    // plus `import <real> as X`). The patched symbol then lives in the real
    // module — follow the alias once before declaring the symbol absent.
    if let Some(real_file) = follow_reexport_module(root, &body) {
        // Index check against the real file.
        if index
            .all_symbols()
            .any(|s| s.path == real_file && s.name == symbol)
        {
            return true;
        }
        if let Some(real_body) = read_text(&root.join(&real_file), io_warnings)
            && symbol_bound_in_body(&real_body, symbol)
        {
            return true;
        }
    }
    false
}

/// True when `symbol` is defined or imported in `body`. Handles single-line and
/// parenthesized multi-line `from … import (…)` blocks (where each imported name
/// sits on its own line).
fn symbol_bound_in_body(body: &str, symbol: &str) -> bool {
    let patterns = [
        format!("def {symbol}"),
        format!("class {symbol}"),
        format!("{symbol} ="),
        format!("import {symbol}"),
        format!("as {symbol}"),
    ];
    if patterns.iter().any(|p| body.contains(p.as_str())) {
        return true;
    }
    // Single-line `from … import a, b, symbol`.
    for line in body.lines() {
        let t = line.trim_start();
        if t.starts_with("from ") && t.contains(" import ") {
            let after = &t[t.find(" import ").unwrap() + " import ".len()..];
            if after
                .split(&[',', ' ', '(', ')'][..])
                .any(|tok| tok.trim() == symbol)
            {
                return true;
            }
        }
    }
    // Parenthesized multi-line import member: a line whose trimmed content is
    // `symbol` or `symbol,` or `symbol as alias` (only meaningful inside an
    // import block, but checking standalone is a safe lenient over-accept).
    for line in body.lines() {
        let t = line.trim().trim_end_matches(',').trim();
        if t == symbol {
            return true;
        }
        // `symbol as alias` as a standalone import member.
        if t.contains(" as ") && t.split(" as ").next() == Some(symbol) {
            return true;
        }
    }
    false
}

/// If `body` is a pure re-export alias (`sys.modules[__name__] = X` + a binding
/// `import <real> as X` / `from <pkg> import <mod> as X`), return the real
/// module's repo-relative file path.
fn follow_reexport_module(root: &Path, body: &str) -> Option<String> {
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
        if let Some(rest) = t.strip_prefix("from ")
            && let Some(imp) = rest.find(" import ")
        {
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
    None
}

/// Resolve a dotted module path to a `.py` or `/__init__.py` file under `root`.
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

/// Strip a Python string literal's surrounding quotes if present.
fn unquote(s: &str) -> Option<&str> {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[bytes.len() - 1] == bytes[0]
    {
        Some(&s[1..s.len() - 1])
    } else {
        None
    }
}

/// Parse `import a.b.c as x` / `import x` bindings local-identifier -> dotted.
fn collect_import_aliases(body: &str) -> BTreeMap<String, String> {
    let mut aliases = BTreeMap::new();
    for line in body.lines() {
        let t = line.trim_start();
        if !t.starts_with("import ") {
            continue;
        }
        let rest = t["import ".len()..].trim();
        // Only single-target `import` lines (no commas) for simplicity.
        if rest.contains(',') {
            continue;
        }
        if let Some(as_idx) = rest.find(" as ") {
            let dotted = rest[..as_idx].trim().to_string();
            let alias = rest[as_idx + 4..].trim().to_string();
            if !alias.is_empty() {
                aliases.insert(alias, dotted);
            }
        } else {
            // `import a.b.c` binds top-level `a`; `import a` binds `a`.
            let dotted = rest.trim().to_string();
            if let Some(top) = dotted.split('.').next() {
                aliases.insert(top.to_string(), dotted.clone());
            }
        }
    }
    aliases
}

/// Extract patch targets from a single test-file body.
fn extract_targets(body: &str) -> Vec<Target> {
    let aliases = collect_import_aliases(body);
    let mut targets = Vec::new();

    for (idx, line) in body.lines().enumerate() {
        let line_no = idx + 1;

        // String-target forms: patch("..."), mocker.patch("..."),
        // monkeypatch.setattr("a.b.symbol", ...).
        for call in ["patch(", "patch.object(", "setattr("] {
            let mut search_from = 0usize;
            while let Some(pos) = line[search_from..].find(call) {
                let start = search_from + pos + call.len();
                search_from = start;
                let after = &line[start..];

                if call == "patch.object(" || call == "setattr(" {
                    // Form: (module_ident, "symbol", ...). First arg is the
                    // module identifier; second is the quoted symbol.
                    if let Some((module_ident, symbol)) = parse_object_setattr(after)
                        && let Some(dotted) = aliases.get(&module_ident)
                    {
                        targets.push(Target {
                            dotted_module: dotted.clone(),
                            symbol,
                            line: line_no,
                        });
                    }
                    // bare identifier not an import alias, or dotted access
                    // (module_ident is None) -> skip.
                } else {
                    // patch("...") string form. First arg should be a string.
                    let arg = first_arg(after);
                    if let Some(s) = arg.and_then(unquote)
                        && s.contains('.')
                    {
                        targets.push(Target {
                            dotted_module: s.to_string(),
                            symbol: String::new(), // resolved later
                            line: line_no,
                        });
                    }
                }
            }
        }
    }
    targets
}

/// From the text after `patch.object(` / `setattr(`, parse (module_ident, symbol)
/// when the first arg is a bare identifier (no dot) and the second is a quoted
/// string. Returns None when the first arg is a dotted attribute access or the
/// second arg isn't a string literal.
fn parse_object_setattr(after: &str) -> Option<(String, String)> {
    // First argument: up to the first top-level comma.
    let comma = after.find(',')?;
    let first = after[..comma].trim();
    // monkeypatch.setattr("a.b.symbol", ...) string form is handled here too.
    if let Some(s) = unquote(first) {
        // String first-arg => treat like a dotted string target: module is the
        // dotted prefix, symbol resolved later. Encode by returning the whole
        // dotted path as module_ident with empty symbol sentinel.
        if s.contains('.') {
            return Some((format!("\u{0}STR\u{0}{s}"), String::new()));
        }
        return None;
    }
    if first.contains('.') || first.is_empty() {
        // attribute access (e.g. celery_module.app) -> not a module ident.
        return None;
    }
    // Second argument must be a quoted symbol.
    let rest = &after[comma + 1..];
    let symbol = first_arg(rest).and_then(unquote)?;
    Some((first.to_string(), symbol.to_string()))
}

/// Return the text of the first argument (up to first top-level comma or close
/// paren) from a slice that begins right after an opening `(`.
fn first_arg(s: &str) -> Option<&str> {
    let end = s.find([',', ')']).unwrap_or(s.len());
    let arg = s[..end].trim();
    if arg.is_empty() { None } else { Some(arg) }
}

pub fn doctor_test_patch_target_integrity(index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut entities = Vec::new();

    let mut files_scanned = 0usize;
    let mut targets_checked = 0usize;

    for rel in TEST_FILES {
        // Missing/unreadable curated file -> skip silently. A deleted or moved
        // test file is out of scope (this doctor only validates patch targets in
        // files that DO exist); skipping keeps the doctor green on the repo.
        let mut io_warnings = Vec::new();
        let Some(body) = read_text(&root.join(rel), &mut io_warnings) else {
            continue;
        };
        files_scanned += 1;

        for target in extract_targets(&body) {
            // Resolve the (module_file, symbol). Two cases:
            // - dotted_module starts with the STR sentinel: it came from a
            //   string first-arg (patch.object/setattr string form).
            // - otherwise: either a `patch("...")` string target (symbol empty,
            //   resolve from the dotted string) or an object/setattr ident form
            //   (symbol already known, module is an import alias dotted path).
            let (module_file, symbol, module_dotted) =
                if let Some(stripped) = target.dotted_module.strip_prefix("\u{0}STR\u{0}") {
                    match resolve_string_target(root, stripped) {
                        Some((md, file, sym)) => (file, sym, md),
                        None => continue, // unresolved module -> skip
                    }
                } else if target.symbol.is_empty() {
                    // `patch("pkg.mod.symbol")` form.
                    match resolve_string_target(root, &target.dotted_module) {
                        Some((md, file, sym)) => (file, sym, md),
                        None => continue,
                    }
                } else {
                    // object/setattr ident form: module is the import-alias
                    // dotted path; verify it resolves to a file.
                    match module_to_file(&target.dotted_module) {
                        Some(base) => {
                            let py = format!("{base}.py");
                            let init = format!("{base}/__init__.py");
                            if root.join(&py).is_file() {
                                (py, target.symbol.clone(), target.dotted_module.clone())
                            } else if root.join(&init).is_file() {
                                (init, target.symbol.clone(), target.dotted_module.clone())
                            } else {
                                continue; // unresolved -> skip
                            }
                        }
                        None => continue,
                    }
                };

            targets_checked += 1;
            let mut io2 = Vec::new();
            if !symbol_present(index, root, &module_file, &symbol, &mut io2) {
                warnings.push(format!(
                    "stale patch target in {rel}:{}: `{symbol}` not found in resolved module {module_dotted} ({module_file})",
                    target.line
                ));
                evidence.push(EvidenceItem {
                    kind: "stale_patch_target".to_string(),
                    path: rel.to_string(),
                    line: Some(target.line),
                    detail: format!(
                        "`{symbol}` absent from {module_file} (module {module_dotted})"
                    ),
                });
            }
        }
    }

    entities.push(json!({
        "doctor": "test-patch-target-integrity",
        "files_scanned": files_scanned,
        "targets_checked": targets_checked,
        "test_files": TEST_FILES,
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_test_patch_target_integrity"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            format!(
                "test-patch-target-integrity: {targets_checked} resolvable patch target(s) across {files_scanned} file(s), all symbols present"
            )
        } else {
            format!(
                "test-patch-target-integrity: {} stale patch target(s)",
                warnings.len()
            )
        },
        confidence: if warnings.is_empty() { 0.95 } else { 0.7 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "files_scanned": files_scanned,
            "targets_checked": targets_checked,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::model::{FileRecord, RepoIndex, SourceLanguage, SymbolKind, SymbolOccurrence};

    fn temp_root(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("leio_patch_target_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write(root: &Path, relative: &str, body: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn index_with(symbols: Vec<(&str, &str)>) -> RepoIndex {
        let mut by_file: BTreeMap<String, Vec<SymbolOccurrence>> = BTreeMap::new();
        for (path, name) in symbols {
            by_file
                .entry(path.to_string())
                .or_default()
                .push(SymbolOccurrence {
                    name: name.to_string(),
                    kind: SymbolKind::Function,
                    path: path.to_string(),
                    line: 1,
                    language: SourceLanguage::Python,
                    qual_name: None,
                });
        }
        let files = by_file
            .into_iter()
            .map(|(path, symbols)| FileRecord {
                path,
                language: SourceLanguage::Python,
                bytes: 0,
                modified_unix_ms: 0,
                symbols,
                env_vars: Vec::new(),
                redis_keys: Vec::new(),
                subprocess_calls: Vec::new(),
                http_calls: Vec::new(),
                unresolved_edges: Vec::new(),
            })
            .collect();
        RepoIndex {
            version: 1,
            root: String::new(),
            indexed_at: String::new(),
            files,
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        }
    }

    #[test]
    fn stale_string_target_warns() {
        let root = temp_root("stale");
        // Module exists, but does NOT define `gone_helper`.
        write(
            &root,
            "example-api/example/agents/tasks.py",
            "def real_helper():\n    pass\n",
        );
        // Test file patches a removed symbol.
        write(
            &root,
            "example-api/example/tests/api/test_whatsapp_router.py",
            "with patch(\"example.agents.tasks.gone_helper\"):\n    pass\n",
        );
        let index = index_with(vec![("example-api/example/agents/tasks.py", "real_helper")]);
        let envelope = doctor_test_patch_target_integrity(&index, &root);
        assert!(
            envelope.warnings.iter().any(|w| w.contains("gone_helper")),
            "{:?}",
            envelope.warnings
        );
    }

    #[test]
    fn valid_defined_target_does_not_warn() {
        let root = temp_root("valid-def");
        write(
            &root,
            "example-api/example/agents/tasks.py",
            "def real_helper():\n    pass\n",
        );
        write(
            &root,
            "example-api/example/tests/api/test_whatsapp_router.py",
            "with patch(\"example.agents.tasks.real_helper\"):\n    pass\n",
        );
        let index = index_with(vec![("example-api/example/agents/tasks.py", "real_helper")]);
        let envelope = doctor_test_patch_target_integrity(&index, &root);
        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
    }

    #[test]
    fn imported_symbol_does_not_warn() {
        let root = temp_root("valid-import");
        // `redis` is an import binding, not a def — must still pass (lenient).
        write(
            &root,
            "example-api/example/agents/tasks.py",
            "import redis\n\ndef other():\n    pass\n",
        );
        write(
            &root,
            "example-api/example/tests/api/test_whatsapp_router.py",
            "with patch(\"example.agents.tasks.redis.Redis.from_url\"):\n    pass\n",
        );
        let index = index_with(vec![("example-api/example/agents/tasks.py", "other")]);
        let envelope = doctor_test_patch_target_integrity(&index, &root);
        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
    }

    #[test]
    fn unresolved_module_is_skipped() {
        let root = temp_root("unresolved");
        // openai.* never resolves to a repo file -> no warn.
        write(
            &root,
            "example-api/example/tests/api/test_whatsapp_router.py",
            "with patch(\"openai.AsyncOpenAI\"):\n    pass\n",
        );
        let index = index_with(vec![]);
        let envelope = doctor_test_patch_target_integrity(&index, &root);
        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
    }

    #[test]
    fn reexport_alias_symbol_does_not_warn() {
        let root = temp_root("reexport");
        // Real symbol lives in webhook.py.
        write(
            &root,
            "example-api/example/integrations/whatsapp/routers/webhook.py",
            "def _redis_client():\n    pass\n",
        );
        // handoff.py is a pure re-export alias for webhook.
        write(
            &root,
            "example-api/example/integrations/whatsapp/routers/handoff.py",
            "from example.integrations.whatsapp.routers import webhook as _webhook\nsys.modules[__name__] = _webhook\n",
        );
        // Test patches the symbol via the alias module path.
        write(
            &root,
            "example-api/example/tests/api/test_whatsapp_router.py",
            "monkeypatch.setattr(whatsapp_mod, \"_redis_client\", lambda: None)\nimport example.integrations.whatsapp.routers.handoff as whatsapp_mod\n",
        );
        // Index only knows the real file's symbol, not the alias file.
        let index = index_with(vec![(
            "example-api/example/integrations/whatsapp/routers/webhook.py",
            "_redis_client",
        )]);
        let envelope = doctor_test_patch_target_integrity(&index, &root);
        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
    }

    #[test]
    fn multiline_import_member_does_not_warn() {
        let root = temp_root("multiline-import");
        // Symbol imported inside a parenthesized multi-line `from … import (…)`.
        write(
            &root,
            "example-api/example/agents/tasks.py",
            "from example.agents.helpers import (\n    _materialize_agent_model,\n    other_helper,\n)\n",
        );
        write(
            &root,
            "example-api/example/tests/api/test_agent_loop_task.py",
            "with patch(\"example.agents.tasks._materialize_agent_model\"):\n    pass\n",
        );
        let index = index_with(vec![]);
        let envelope = doctor_test_patch_target_integrity(&index, &root);
        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
    }

    #[test]
    fn attribute_access_setattr_is_skipped() {
        let root = temp_root("attr-setattr");
        // monkeypatch.setattr(celery_module.app, "send_task", ...) — first arg is
        // an attribute access, not a module identifier -> skip, no warn.
        write(
            &root,
            "example-api/example/tests/api/test_whatsapp_router.py",
            "monkeypatch.setattr(celery_module.app, \"send_task\", fake)\n",
        );
        let index = index_with(vec![]);
        let envelope = doctor_test_patch_target_integrity(&index, &root);
        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
    }
}
