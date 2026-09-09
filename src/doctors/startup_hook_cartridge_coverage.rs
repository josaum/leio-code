//! Startup hook cartridge-coverage doctor.
//!
//! On 2026-05-04 we discovered `example-api/example/main.py` had startup hooks for
//! `seed_sara_agent` and `seed_liz_agent` but `seed_pratique_agent` was missing entirely.
//! Result: production restarted, the cartridge appeared "active", but the agent row was
//! never created.
//!
//! Contract: every cartridge that exposes a top-level `def seed_<name>_agent():` in
//! `cartridges/<cart>/seed.py` must have a matching `from cartridges.<cart>.seed import
//! seed_<name>_agent` invocation in `example-api/example/main.py`.
//!
//! This is a structural correspondence; the doctor does not check ordering or env-gating
//! — only the *presence* of the import + call. False positives are preferable to silent
//! drift here.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use ignore::WalkBuilder;
use regex::Regex;
use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct StartupHookCartridgeCoverageDoctor;

impl Doctor for StartupHookCartridgeCoverageDoctor {
    fn name(&self) -> &'static str {
        "startup-hook-cartridge-coverage"
    }

    fn description(&self) -> &'static str {
        "Ensures every cartridge that defines a `seed_*_agent()` function in seed.py is invoked from `example-api/example/main.py` on startup."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_startup_hook_cartridge_coverage(root)
    }
}

pub fn doctor_startup_hook_cartridge_coverage(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let main_py_path = root.join("example-api/example/main.py");
    let main_py_body = if main_py_path.is_file() {
        let mut io_warnings = Vec::new();
        read_text(&main_py_path, &mut io_warnings).unwrap_or_default()
    } else {
        // If main.py isn't there we can't check coverage, but we don't fail the doctor —
        // simply emit an empty informational entity.
        entities.push(json!({
            "doctor": "startup-hook-cartridge-coverage",
            "main_py_present": false,
            "expected_seed_functions": Vec::<String>::new(),
            "missing_hooks": Vec::<String>::new(),
        }));
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_startup_hook_cartridge_coverage"),
            kind: "doctor".to_string(),
            summary: "example-api/example/main.py not found; skipped".to_string(),
            confidence: 0.9,
            entities,
            evidence,
            warnings,
            meta: None,
            timing_ms: started.elapsed().as_millis(),
        };
    };

    let def_re = Regex::new(r"(?m)^def (seed_[a-zA-Z0-9_]+_agent)\s*\(").expect("valid regex");

    let mut expected: BTreeSet<(String, String)> = BTreeSet::new(); // (cart, fn)
    for path in collect_seed_py_files(root) {
        let rel = match path.strip_prefix(root) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        let cart_name = match rel.split('/').nth(1) {
            Some(n) => n.to_string(),
            None => continue,
        };

        let mut io_warnings = Vec::new();
        let body = match read_text(&path, &mut io_warnings) {
            Some(b) => b,
            None => {
                warnings.extend(io_warnings);
                continue;
            }
        };
        for caps in def_re.captures_iter(&body) {
            if let Some(name) = caps.get(1) {
                expected.insert((cart_name.clone(), name.as_str().to_string()));
            }
        }
    }

    let mut missing: Vec<(String, String)> = Vec::new();
    for (cart, fn_name) in &expected {
        let import_start = format!("from cartridges.{}.seed import", cart);
        let call_marker = format!("{}(", fn_name);
        // Import may be single-line or a parenthesized multi-name import.
        let import_present = main_py_body
            .find(&import_start)
            .map(|idx| {
                let after = &main_py_body[idx + import_start.len()..];
                let span_end = if after.trim_start().starts_with('(') {
                    after.find(')').map(|i| i + 1).unwrap_or(after.len())
                } else {
                    after.find('\n').unwrap_or(after.len())
                };
                after[..span_end].contains(fn_name)
            })
            .unwrap_or(false);
        let call_present = main_py_body.contains(&call_marker);
        if !(import_present && call_present) {
            missing.push((cart.clone(), fn_name.clone()));
            warnings.push(format!(
                "example-api/example/main.py: missing startup hook for cartridge `{}` -- expected `{}` (import_present={}, call_present={})",
                cart, fn_name, import_present, call_present
            ));
            evidence.push(EvidenceItem {
                kind: "startup_hook_missing".to_string(),
                path: "example-api/example/main.py".to_string(),
                line: None,
                detail: format!(
                    "cartridge `{}` defines `{}` in seed.py but main.py does not import+call it",
                    cart, fn_name
                ),
            });
        }
    }

    entities.push(json!({
        "doctor": "startup-hook-cartridge-coverage",
        "main_py_present": true,
        "expected_seed_functions": expected
            .iter()
            .map(|(c, f)| format!("{c}::{f}"))
            .collect::<Vec<_>>(),
        "missing_hooks": missing
            .iter()
            .map(|(c, f)| format!("{c}::{f}"))
            .collect::<Vec<_>>(),
    }));

    let summary = if warnings.is_empty() {
        format!(
            "all {} cartridge `seed_*_agent` function(s) are wired into main.py",
            expected.len()
        )
    } else {
        format!(
            "{} cartridge `seed_*_agent` function(s) are not wired into main.py",
            missing.len()
        )
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_startup_hook_cartridge_coverage"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.95 } else { 0.55 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn collect_seed_py_files(root: &Path) -> Vec<std::path::PathBuf> {
    let cartridges_root = root.join("cartridges");
    if !cartridges_root.is_dir() {
        return Vec::new();
    }
    let mut builder = WalkBuilder::new(&cartridges_root);
    builder.hidden(false);
    builder.git_ignore(true);
    builder.require_git(false);
    builder.max_depth(Some(2));

    let mut paths = Vec::new();
    for dent in builder.build().flatten() {
        let path = dent.path();
        if path.is_file() && path.file_name().and_then(|n| n.to_str()) == Some("seed.py") {
            paths.push(path.to_path_buf());
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_repo(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-startup-hook-{label}-{}-{nanos}",
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
    fn complete_coverage_yields_no_warnings() {
        let root = temp_repo("complete");
        write(
            &root,
            "cartridges/liz_cobranca/seed.py",
            "def seed_liz_agent():\n    pass\n",
        );
        write(
            &root,
            "example-api/example/main.py",
            r#"
from cartridges.liz_cobranca.seed import seed_liz_agent
seed_liz_agent()
"#,
        );
        let env = doctor_startup_hook_cartridge_coverage(&root);
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_hook_is_flagged() {
        let root = temp_repo("missing");
        write(
            &root,
            "cartridges/pratique_cobranca/seed.py",
            "def seed_pratique_agent():\n    pass\n",
        );
        write(&root, "example-api/example/main.py", "# nothing\n");
        let env = doctor_startup_hook_cartridge_coverage(&root);
        assert_eq!(env.warnings.len(), 1, "{:?}", env.warnings);
        assert!(env.warnings[0].contains("pratique_cobranca"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn import_without_call_is_flagged() {
        let root = temp_repo("import-only");
        write(
            &root,
            "cartridges/foo/seed.py",
            "def seed_foo_agent():\n    pass\n",
        );
        write(
            &root,
            "example-api/example/main.py",
            "from cartridges.foo.seed import seed_foo_agent\n# never called\n",
        );
        let env = doctor_startup_hook_cartridge_coverage(&root);
        assert_eq!(env.warnings.len(), 1, "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }
}
