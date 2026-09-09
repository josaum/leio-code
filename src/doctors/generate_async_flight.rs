use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

/// Guards the `/v2/generate` event-loop safety. `client.generate` is a
/// synchronous pyarrow Flight RPC; calling it directly from the `async def`
/// handler froze every concurrent request for the whole generation. The fix
/// offloads the blocking calls via `asyncio.to_thread`. Drift that would
/// re-freeze the loop:
///   1. a direct `client.generate(...)` call returns (the sync RPC runs on the
///      event loop again) instead of being passed to `asyncio.to_thread`;
///   2. `asyncio` / `asyncio.to_thread` disappear, or the regression test that
///      proves the loop stays responsive is deleted.
pub struct GenerateAsyncFlightDoctor;

const GENERATE_PY: &str = "example-api/example/routers/generate.py";
const TEST_PY: &str = "example-api/example/tests/generate/test_generate_async_offload.py";

impl Doctor for GenerateAsyncFlightDoctor {
    fn name(&self) -> &'static str {
        "generate-async-flight"
    }

    fn description(&self) -> &'static str {
        "/v2/generate offloads its synchronous Flight calls via asyncio.to_thread \
         so a generation can't block the FastAPI event loop."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_generate_async_flight(root)
    }
}

pub fn doctor_generate_async_flight(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings: Vec<String> = Vec::new();
    let mut evidence: Vec<EvidenceItem> = Vec::new();

    let generate = root.join(GENERATE_PY);
    let test = root.join(TEST_PY);

    if !generate.exists() {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor.generate-async-flight"),
            kind: "doctor".to_string(),
            summary: "generate-async-flight: generate.py not present; skipping".to_string(),
            confidence: 0.5,
            entities: vec![],
            evidence,
            warnings,
            meta: Some(json!({"skipped": true})),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    let src = read_text(&generate, &mut warnings).unwrap_or_default();

    // 1. The blocking sync generate must NOT be called directly; it must be
    //    passed to asyncio.to_thread (i.e. `client.generate,`), never invoked
    //    (`client.generate(`) on the event loop.
    let direct_calls = src.matches("client.generate(").count();
    if direct_calls > 0 {
        warnings.push(format!(
            "{GENERATE_PY} calls client.generate(...) directly ({direct_calls} site(s)) on the event loop — offload it via asyncio.to_thread(client.generate, ...)"
        ));
    }

    // 2. The offload primitive + its import must be present.
    if !src.contains("import asyncio") {
        warnings.push(format!("{GENERATE_PY} no longer imports asyncio"));
    }
    if !src.contains("asyncio.to_thread") {
        warnings.push(format!(
            "{GENERATE_PY} no longer uses asyncio.to_thread — sync Flight calls run on the event loop"
        ));
    }

    // 3. The event-loop-responsiveness regression test must not be deleted.
    if !test.exists() {
        warnings.push(format!(
            "{TEST_PY} is missing — event-loop non-blocking is no longer proven"
        ));
    }

    evidence.push(EvidenceItem {
        kind: "generate-async-flight".to_string(),
        path: generate.display().to_string(),
        line: None,
        detail: format!("{direct_calls} direct client.generate( call(s); offloaded via to_thread"),
    });

    let summary = if warnings.is_empty() {
        "generate-async-flight: /v2/generate offloads sync Flight calls off the event loop"
            .to_string()
    } else {
        format!("generate-async-flight: {} issue(s)", warnings.len())
    };
    let confidence = if warnings.is_empty() {
        0.97_f32
    } else {
        0.6_f32
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor.generate-async-flight"),
        kind: "doctor".to_string(),
        summary,
        confidence,
        entities: vec![json!({
            "direct_generate_calls": direct_calls,
            "uses_to_thread": src.contains("asyncio.to_thread"),
            "test_present": test.exists(),
        })],
        evidence,
        warnings,
        meta: Some(json!({
            "generate": GENERATE_PY,
            "test": TEST_PY,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}
