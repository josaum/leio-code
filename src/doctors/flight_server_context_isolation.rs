use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

/// Guards Flight generator RPC telemetry against PyArrow resuming or closing
/// the generator from a different Python Context. ContextVar tokens may only
/// be reset in the Context that created them; violating that contract turns a
/// successful Flight action into a ValueError during generator cleanup.
pub struct FlightServerContextIsolationDoctor;

const METRICS_PY: &str = "example-api/example/flight/server_metrics.py";
const TEST_PY: &str = "example-api/example/tests/flight/test_server_metrics.py";

impl Doctor for FlightServerContextIsolationDoctor {
    fn name(&self) -> &'static str {
        "flight-server-context-isolation"
    }

    fn description(&self) -> &'static str {
        "Flight generator RPC metrics execute start, iteration, close, and \
         ContextVar cleanup inside one copied Context, with a cross-context \
         generator-close regression test."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_flight_server_context_isolation(root)
    }
}

pub fn doctor_flight_server_context_isolation(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();

    let metrics_path = root.join(METRICS_PY);
    let test_path = root.join(TEST_PY);
    let metrics = read_text(&metrics_path, &mut warnings).unwrap_or_default();
    let test = read_text(&test_path, &mut warnings).unwrap_or_default();

    for (needle, detail) in [
        ("ContextVar, copy_context", "imports copy_context"),
        ("rpc_context = copy_context()", "copies the caller Context"),
        (
            "rpc_context.run(start, args, kwargs)",
            "creates the ContextVar token inside the copied Context",
        ),
        (
            "rpc_context.run(next, iterator)",
            "resumes the Flight generator inside the copied Context",
        ),
        (
            "rpc_context.run(iterator.close)",
            "closes the Flight generator inside the copied Context",
        ),
        (
            "rpc_context.run(_CURRENT_BYTES.reset, token)",
            "resets the ContextVar token inside its originating Context",
        ),
    ] {
        if !metrics.contains(needle) {
            warnings.push(format!("{METRICS_PY} {detail}; missing `{needle}`"));
        }
    }

    for (needle, detail) in [
        (
            "test_generator_close_from_another_context_preserves_metrics_cleanup",
            "cross-context generator-close regression test",
        ),
        (
            "Context().run(results.close)",
            "different-Context close exercise",
        ),
    ] {
        if !test.contains(needle) {
            warnings.push(format!("{TEST_PY} is missing the {detail}: `{needle}`"));
        }
    }

    evidence.push(EvidenceItem {
        kind: "flight-server-context-isolation".to_string(),
        path: metrics_path.display().to_string(),
        line: None,
        detail: "Flight generator lifecycle is pinned to one copied Python Context".to_string(),
    });

    let summary = if warnings.is_empty() {
        "flight-server-context-isolation: generator lifecycle and ContextVar cleanup are context-safe".to_string()
    } else {
        format!(
            "flight-server-context-isolation: {} issue(s)",
            warnings.len()
        )
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor.flight-server-context-isolation"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.98 } else { 0.55 },
        entities: vec![json!({
            "metrics_file": METRICS_PY,
            "test_file": TEST_PY,
            "cross_context_close_test_present": test.contains(
                "test_generator_close_from_another_context_preserves_metrics_cleanup"
            ),
        })],
        evidence,
        warnings,
        meta: Some(json!({
            "metrics": METRICS_PY,
            "test": TEST_PY,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::doctor_flight_server_context_isolation;

    fn temp_repo(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "leio-flight-context-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("example-api/example/flight")).unwrap();
        fs::create_dir_all(root.join("example-api/example/tests/flight")).unwrap();
        root
    }

    fn write_good_repo(root: &Path) {
        fs::write(
            root.join("example-api/example/flight/server_metrics.py"),
            r#"
from contextvars import ContextVar, copy_context
rpc_context = copy_context()
totals, token, args, kwargs = rpc_context.run(start, args, kwargs)
item = rpc_context.run(next, iterator)
rpc_context.run(iterator.close)
rpc_context.run(_CURRENT_BYTES.reset, token)
"#,
        )
        .unwrap();
        fs::write(
            root.join("example-api/example/tests/flight/test_server_metrics.py"),
            r#"
def test_generator_close_from_another_context_preserves_metrics_cleanup():
    Context().run(results.close)
"#,
        )
        .unwrap();
    }

    #[test]
    fn context_safe_contract_passes() {
        let root = temp_repo("good");
        write_good_repo(&root);
        let result = doctor_flight_server_context_isolation(&root);
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn direct_cross_context_reset_is_flagged() {
        let root = temp_repo("bad");
        write_good_repo(&root);
        let path = root.join("example-api/example/flight/server_metrics.py");
        let body = fs::read_to_string(&path).unwrap().replace(
            "rpc_context.run(_CURRENT_BYTES.reset, token)",
            "_CURRENT_BYTES.reset(token)",
        );
        fs::write(path, body).unwrap();

        let result = doctor_flight_server_context_isolation(&root);
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("originating Context")),
            "{:?}",
            result.warnings
        );
        let _ = fs::remove_dir_all(root);
    }
}
