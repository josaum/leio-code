use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct SisfronOodaRuntimeDoctor;

impl Doctor for SisfronOodaRuntimeDoctor {
    fn name(&self) -> &'static str {
        "sisfron-ooda-runtime"
    }

    fn description(&self) -> &'static str {
        "Checks that SISFRON OODA runtime emits domain events, persists PITCIC/snapshots/metrics, archives resolved cycles, and exposes CV plus SSE contracts."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_sisfron_ooda_runtime(root)
    }
}

pub fn doctor_sisfron_ooda_runtime(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let engine_path = root.join("cartridges/sisfron/engine.py");
    let router_path = root.join("cartridges/sisfron/router.py");
    let types_path = root.join("cartridges/sisfron/types.py");
    let monitoring_path = root.join("cartridges/sisfron/monitoring.py");
    let runtime_tests_path = root.join("cartridges/sisfron/tests/test_engine_runtime.py");
    let cv_tests_path = root.join("cartridges/sisfron/tests/test_router_cv.py");
    let youraies_3dgs_path = root.join("youraies/src/youraies/camera/gaussian_splatting.py");
    let youraies_streaming_path = root.join("youraies/src/youraies/streaming/app.py");
    let youraies_streaming_tests_path = root.join("youraies/tests/test_streaming_app.py");
    let youraies_3dgs_tests_path = root.join("youraies/tests/test_gaussian_splatting.py");

    let engine_src = read_text(&engine_path, &mut warnings);
    let router_src = read_text(&router_path, &mut warnings);
    let types_src = read_text(&types_path, &mut warnings);
    let monitoring_src = read_text(&monitoring_path, &mut warnings);
    let runtime_tests_src = read_text(&runtime_tests_path, &mut warnings);
    let cv_tests_src = read_text(&cv_tests_path, &mut warnings);
    let youraies_3dgs_src = read_text(&youraies_3dgs_path, &mut warnings);
    let youraies_streaming_src = read_text(&youraies_streaming_path, &mut warnings);
    let youraies_streaming_tests_src = read_text(&youraies_streaming_tests_path, &mut warnings);
    let youraies_3dgs_tests_src = read_text(&youraies_3dgs_tests_path, &mut warnings);

    let pitcic_contract = engine_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "def _run_pitcic(",
                "pitcic_product = self._run_pitcic(cycle_id, cycle_data,",
                "contact_observations",
                "self.r.set(self._key(cycle_id, \"pitcic\"), pitcic_product.model_dump_json())",
                "result[\"pitcic\"] = json.loads(raw_pitcic)",
            ],
        )
    });
    let event_log_contract = engine_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "def _publish_event(",
                "serialized = json.dumps(payload, default=str)",
                "self.r.rpush(self._key(\"events\"), serialized)",
                "def list_events(",
                "\"CycleCreated\"",
                "\"ObservationAdded\"",
                "\"OptionsGenerated\"",
                "\"ActionAuthorized\"",
                "\"ActionDispatched\"",
                "\"FeedbackReceived\"",
                "\"CycleResolved\"",
            ],
        )
    });
    let snapshot_archive_contract = engine_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "def _create_snapshot(self, cycle_id: str, phase: str) -> CycleSnapshot:",
                "named_graph_uri=f\"urn:sisfron:cycle:{cycle_id}:snapshot:{phase}:{snapshot_id}\"",
                "self._create_snapshot(cycle_id, OODAPhase.OBSERVE.value)",
                "self._create_snapshot(cycle_id, new_phase.value)",
                "def _archive_to_consumption(",
                "self.r.set(self._key(\"lake\", \"consumption\", cycle_id), record.model_dump_json())",
            ],
        )
    });
    let metrics_contract = engine_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "def _record_resolution_metrics(",
                "self.r.hset(self._key(\"metrics\", cycle_id), mapping=metrics)",
                "self._key(\"metrics\", \"resolved\")",
                "def _record_external_call(",
                "self.r.rpush(self._key(\"llm\", \"calls\"), json.dumps(record))",
            ],
        )
    });
    let dispatch_feedback_contract = engine_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "def _record_feedback_arc(",
                "self.r.rpush(self._key(cycle_id, \"feedback\"), feedback.model_dump_json())",
                "def _enqueue_dispatch(",
                "self.r.rpush(self._key(\"dispatch\", channel), json.dumps(envelope))",
                "result[\"feedback_arcs\"] = [json.loads(item) for item in raw_feedback]",
                "\"feedback_count\": feedback_count,",
            ],
        )
    });
    let monitoring_alignment_contract = monitoring_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &["metrics:{cycle_id}", "metrics:resolved", "llm:calls"],
        )
    });
    let sse_contract = router_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "async def event_stream_pubsub():",
                "pubsub_channel = _engine_key(\"events:pubsub\")",
                "async def event_stream_poll():",
                "cursor = _get_engine().list_events(domain=domain, sector_id=sector_id)[1]",
                "events_batch, cursor = _get_engine().list_events(",
                "media_type=\"text/event-stream\"",
            ],
        )
    });
    let cv_contract = router_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "class CvImageRequest(BaseModel):",
                "class CvChangeRequest(BaseModel):",
                "def _decode_base64_image(image_base64: str):",
                "def _serialize_cv_result(payload: dict[str, Any]) -> dict[str, Any]:",
                "@_auth_router.post(\"/cv/segment\")",
                "@_auth_router.post(\"/cv/detect\")",
                "@_auth_router.post(\"/cv/changes\")",
                "image = _decode_base64_image(req.image_base64)",
                "before = _decode_base64_image(req.before_image_base64)",
                "after = _decode_base64_image(req.after_image_base64)",
            ],
        )
    });
    let runtime_regression_tests = runtime_tests_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "test_orient_persists_pitcic_and_emits_events",
                "test_act_enqueues_dispatch_and_feedback_arc",
                "test_resolve_archives_consumption_and_metrics",
            ],
        )
    });
    let cv_regression_tests = cv_tests_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "test_cv_segment_returns_serializable_payload",
                "test_cv_detect_returns_detection_payload",
                "test_cv_changes_returns_change_payload",
            ],
        )
    });
    let youraies_3dgs_bridge_contract = youraies_streaming_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "@app.get(\"/api/fusion/3dgs/status\")",
                "@app.get(\"/api/fusion/3dgs/latest\")",
                "@app.post(\"/api/fusion/3dgs/dispatch\")",
                "@app.get(\"/api/fusion/3dgs/artifacts/latest/manifest\")",
                "@app.get(\"/api/fusion/3dgs/artifacts/latest/images/{camera_id}\")",
                "def _require_auth(",
                "_require_auth(authorization, key, _fusion_source_id())",
                "local_paths_redacted",
                "payload[\"manifest_uri\"] = _public_manifest_uri()",
                "view[\"image_uri\"] = _public_image_uri(str(camera_id))",
                "expose_local_paths=True",
            ],
        )
    });
    let youraies_3dgs_bridge_tests = youraies_streaming_tests_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "test_latest_3dgs_manifest_returns_sisfron_observation",
                "test_3dgs_bridge_requires_auth_when_stream_auth_enabled",
                "test_3dgs_artifact_routes_return_portable_manifest_and_image",
            ],
        )
    });
    let youraies_3dgs_georeg_track_contract = youraies_3dgs_src.as_deref().is_some_and(|src| {
        source_contains_all(
            src,
            &[
                "self.scene_track_id = _scene_track_id(config.rig_id, views)",
                "\"georegistration\": _build_georegistration(self.config)",
                "\"calibration_health\": calibration_health",
                "_read_calibration_metrics(",
                "\"sync\": {",
                "\"reference_timestamp\": _iso_timestamp(reference_timestamp_s)",
                "\"views\": _build_sync_records(samples, self.views, reference_timestamp_s)",
                "\"track\": {",
                "\"scene_track_id\": self.scene_track_id",
                "\"continuity\": \"continuous\" if sequence > 0 else \"initialized\"",
                "def _build_georegistration(",
                "def _build_sync_records(",
                "def _build_calibration_health(",
                "backend_metrics",
                "\"refinement\": {",
                "def _scene_track_id(",
            ],
        )
    });
    let sisfron_fused_scene_track_contract = types_src
        .as_deref()
        .zip(engine_src.as_deref())
        .is_some_and(|(types, engine)| {
            source_contains_all(
                types,
                &[
                    "scene_track_id: str | None = None",
                    "persistent_scene_track_ids: list[str]",
                    "continuous_scene_update_count: int",
                    "temporal_alignment_score: float | None",
                    "georegistration_confidence: float | None",
                    "calibration_score: float | None",
                    "calibration_healthy: bool",
                ],
            ) && source_contains_all(
                engine,
                &[
                    "scene_track_counts: Counter[str] = Counter()",
                    "continuous_scene_update_count += 1",
                    "persistent_scene_track_ids = sorted(",
                    "scene:track:{track_id}",
                    "scene:persistent_3dgs_track",
                    "scene:georegistered",
                    "scene:calibration:healthy",
                    "calibration_healthy = calibration_score is None or calibration_score >= 0.6",
                ],
            )
        });
    let fused_scene_track_tests = runtime_tests_src
        .as_deref()
        .zip(youraies_3dgs_tests_src.as_deref())
        .is_some_and(|(runtime_tests, youraies_tests)| {
            source_contains_all(
                runtime_tests,
                &[
                    "test_orient_uses_persistent_3dgs_track_continuity",
                    "test_orient_blocks_scene_corroboration_when_calibration_is_degraded",
                ],
            ) && source_contains_all(
                youraies_tests,
                &[
                    "second_manifest[\"track\"][\"continuity\"] == \"continuous\"",
                    "manifest[\"georegistration\"][\"coordinate_frame\"]",
                    "manifest[\"calibration_health\"][\"status\"]",
                    "test_external_backend_metrics_refine_calibration_health",
                    "observation[\"payload\"][\"scene_track_id\"]",
                    "observation[\"payload\"][\"quality\"][\"calibration_score\"]",
                ],
            )
        });

    if !pitcic_contract {
        warnings.push(
            "engine.py no longer persists the PITCIC product into the SISFRON cycle runtime or surfaces it through cycle reads"
                .to_string(),
        );
    }
    if !event_log_contract {
        warnings.push(
            "engine.py no longer maintains the dedicated SISFRON event log with the expected OODA runtime domain events"
                .to_string(),
        );
    }
    if !snapshot_archive_contract {
        warnings.push(
            "engine.py no longer snapshots OODA transitions or archives resolved cycles into the SISFRON consumption zone"
                .to_string(),
        );
    }
    if !metrics_contract {
        warnings.push(
            "engine.py no longer records SISFRON cycle metrics and external-call telemetry under the monitoring contracts"
                .to_string(),
        );
    }
    if !dispatch_feedback_contract {
        warnings.push(
            "engine.py no longer retains dispatch queue and feedback-arc runtime state for SISFRON authorized actions"
                .to_string(),
        );
    }
    if !monitoring_alignment_contract {
        warnings.push(
            "monitoring.py no longer documents the metrics and llm-call keys that the SISFRON runtime is expected to populate"
                .to_string(),
        );
    }
    if !sse_contract {
        warnings.push(
            "router.py no longer serves SISFRON SSE from the dedicated event log contract"
                .to_string(),
        );
    }
    if !cv_contract {
        warnings.push(
            "router.py no longer exposes the JSON/base64 CV handlers for segmentation, detection, and change detection"
                .to_string(),
        );
    }
    if !runtime_regression_tests {
        warnings.push(
            "test_engine_runtime.py is missing the SISFRON runtime regressions for PITCIC, dispatch feedback, and resolve archive metrics"
                .to_string(),
        );
    }
    if !cv_regression_tests {
        warnings.push(
            "test_router_cv.py is missing the SISFRON CV transport regressions for segment, detect, and change endpoints"
                .to_string(),
        );
    }
    if !youraies_3dgs_bridge_contract {
        warnings.push(
            "youraies streaming app no longer exposes the authenticated, redacted 3DGS fusion bridge required by SISFRON Orient"
                .to_string(),
        );
    }
    if !youraies_3dgs_bridge_tests {
        warnings.push(
            "test_streaming_app.py is missing YourAIes 3DGS bridge regressions for auth, portable artifact manifests, and SISFRON observations"
                .to_string(),
        );
    }
    if !youraies_3dgs_georeg_track_contract {
        warnings.push(
            "gaussian_splatting.py no longer emits georegistration, per-camera sync telemetry, and persistent scene-track continuity in 3DGS manifests"
                .to_string(),
        );
    }
    if !sisfron_fused_scene_track_contract {
        warnings.push(
            "SISFRON fused-scene runtime no longer carries persistent 3DGS scene-track and georegistration evidence into Orient"
                .to_string(),
        );
    }
    if !fused_scene_track_tests {
        warnings.push(
            "SISFRON/YourAIes tests no longer cover georegistered 3DGS scene-track continuity"
                .to_string(),
        );
    }

    for (path, src, needle, detail) in [
        (
            &engine_path,
            engine_src.as_ref(),
            "def _run_pitcic(",
            "engine owns the PITCIC runtime integration hook",
        ),
        (
            &engine_path,
            engine_src.as_ref(),
            "self.r.set(self._key(cycle_id, \"pitcic\"), pitcic_product.model_dump_json())",
            "PITCIC products are persisted into cycle runtime state",
        ),
        (
            &engine_path,
            engine_src.as_ref(),
            "def _publish_event(",
            "runtime writes dedicated SISFRON domain events",
        ),
        (
            &engine_path,
            engine_src.as_ref(),
            "def _create_snapshot(self, cycle_id: str, phase: str) -> CycleSnapshot:",
            "runtime snapshots OODA transitions with deterministic named graph URIs",
        ),
        (
            &engine_path,
            engine_src.as_ref(),
            "self.r.set(self._key(\"lake\", \"consumption\", cycle_id), record.model_dump_json())",
            "resolved cycles are archived to the consumption zone",
        ),
        (
            &engine_path,
            engine_src.as_ref(),
            "self.r.hset(self._key(\"metrics\", cycle_id), mapping=metrics)",
            "runtime records cycle metrics for monitoring",
        ),
        (
            &engine_path,
            engine_src.as_ref(),
            "self.r.rpush(self._key(\"llm\", \"calls\"), json.dumps(record))",
            "runtime records external-call telemetry for monitoring",
        ),
        (
            &engine_path,
            engine_src.as_ref(),
            "self.r.rpush(self._key(\"dispatch\", channel), json.dumps(envelope))",
            "authorized actions are queued into dispatch channels",
        ),
        (
            &router_path,
            router_src.as_ref(),
            "cursor = _get_engine().list_events(domain=domain, sector_id=sector_id)[1]",
            "SSE stream consumes the dedicated engine event log",
        ),
        (
            &router_path,
            router_src.as_ref(),
            "@_auth_router.post(\"/cv/segment\")",
            "CV segmentation route is live",
        ),
        (
            &router_path,
            router_src.as_ref(),
            "@_auth_router.post(\"/cv/changes\")",
            "CV change-detection route is live",
        ),
        (
            &runtime_tests_path,
            runtime_tests_src.as_ref(),
            "test_orient_persists_pitcic_and_emits_events",
            "runtime regressions cover PITCIC and event emission",
        ),
        (
            &cv_tests_path,
            cv_tests_src.as_ref(),
            "test_cv_segment_returns_serializable_payload",
            "CV route regressions cover JSON/base64 transport",
        ),
        (
            &youraies_streaming_path,
            youraies_streaming_src.as_ref(),
            "@app.get(\"/api/fusion/3dgs/latest\")",
            "YourAIes exposes the latest fused 3DGS scene for SISFRON Orient",
        ),
        (
            &youraies_streaming_path,
            youraies_streaming_src.as_ref(),
            "_require_auth(authorization, key, _fusion_source_id())",
            "YourAIes 3DGS fusion endpoints share stream-auth enforcement",
        ),
        (
            &youraies_streaming_path,
            youraies_streaming_src.as_ref(),
            "payload[\"manifest_uri\"] = _public_manifest_uri()",
            "SISFRON observations point at portable artifact manifests",
        ),
        (
            &youraies_streaming_tests_path,
            youraies_streaming_tests_src.as_ref(),
            "test_3dgs_bridge_requires_auth_when_stream_auth_enabled",
            "YourAIes 3DGS bridge regressions cover authenticated access",
        ),
        (
            &youraies_3dgs_path,
            youraies_3dgs_src.as_ref(),
            "\"georegistration\": _build_georegistration(self.config)",
            "3DGS keyframe manifests carry rig georegistration metadata",
        ),
        (
            &youraies_3dgs_path,
            youraies_3dgs_src.as_ref(),
            "\"calibration_health\": calibration_health",
            "3DGS keyframe manifests carry calibration health for trust gating",
        ),
        (
            &youraies_3dgs_path,
            youraies_3dgs_src.as_ref(),
            "_read_calibration_metrics(",
            "3DGS keyframe writer ingests external backend calibration residuals",
        ),
        (
            &youraies_3dgs_path,
            youraies_3dgs_src.as_ref(),
            "\"continuity\": \"continuous\" if sequence > 0 else \"initialized\"",
            "3DGS keyframe manifests carry persistent scene-track continuity",
        ),
        (
            &types_path,
            types_src.as_ref(),
            "persistent_scene_track_ids: list[str]",
            "SISFRON scene context models persistent 3DGS scene tracks",
        ),
        (
            &engine_path,
            engine_src.as_ref(),
            "scene:persistent_3dgs_track",
            "Orient emits persistent 3DGS scene-track evidence patterns",
        ),
        (
            &engine_path,
            engine_src.as_ref(),
            "scene:calibration:healthy",
            "Orient emits calibration-health evidence patterns for trusted 3DGS scenes",
        ),
        (
            &runtime_tests_path,
            runtime_tests_src.as_ref(),
            "test_orient_uses_persistent_3dgs_track_continuity",
            "runtime regressions cover persistent 3DGS scene-track continuity",
        ),
    ] {
        push_evidence(&mut evidence, path, src, needle, detail);
    }

    entities.push(json!({
        "path": engine_path.display().to_string(),
        "pitcic_contract": pitcic_contract,
        "event_log_contract": event_log_contract,
        "snapshot_archive_contract": snapshot_archive_contract,
        "metrics_contract": metrics_contract,
        "dispatch_feedback_contract": dispatch_feedback_contract,
    }));
    entities.push(json!({
        "path": monitoring_path.display().to_string(),
        "monitoring_alignment_contract": monitoring_alignment_contract,
    }));
    entities.push(json!({
        "path": router_path.display().to_string(),
        "sse_contract": sse_contract,
        "cv_contract": cv_contract,
    }));
    entities.push(json!({
        "path": types_path.display().to_string(),
        "sisfron_fused_scene_track_contract": sisfron_fused_scene_track_contract,
    }));
    entities.push(json!({
        "path": runtime_tests_path.display().to_string(),
        "runtime_regression_tests": runtime_regression_tests,
    }));
    entities.push(json!({
        "path": cv_tests_path.display().to_string(),
        "cv_regression_tests": cv_regression_tests,
    }));
    entities.push(json!({
        "path": youraies_streaming_path.display().to_string(),
        "youraies_3dgs_bridge_contract": youraies_3dgs_bridge_contract,
    }));
    entities.push(json!({
        "path": youraies_3dgs_path.display().to_string(),
        "youraies_3dgs_georeg_track_contract": youraies_3dgs_georeg_track_contract,
    }));
    entities.push(json!({
        "path": youraies_streaming_tests_path.display().to_string(),
        "youraies_3dgs_bridge_tests": youraies_3dgs_bridge_tests,
        "fused_scene_track_tests": fused_scene_track_tests,
    }));

    let summary = if warnings.is_empty() {
        "SISFRON OODA runtime invariants hold across PITCIC integration, event streaming, snapshots, archive/metrics, dispatch feedback, CV handlers, YourAIes 3DGS fusion bridge, georegistered scene-track continuity, calibration health gating, and regression coverage".to_string()
    } else {
        format!(
            "SISFRON OODA runtime doctor found {} warnings across engine, router, monitoring, YourAIes bridge, georegistered scene tracks, calibration health, and regression coverage",
            warnings.len()
        )
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_sisfron_ooda_runtime"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.98 } else { 0.66 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "doctor": "sisfron-ooda-runtime",
            "contracts": {
                "pitcic_contract": pitcic_contract,
                "event_log_contract": event_log_contract,
                "snapshot_archive_contract": snapshot_archive_contract,
                "metrics_contract": metrics_contract,
                "dispatch_feedback_contract": dispatch_feedback_contract,
                "monitoring_alignment_contract": monitoring_alignment_contract,
                "sse_contract": sse_contract,
                "cv_contract": cv_contract,
                "runtime_regression_tests": runtime_regression_tests,
                "cv_regression_tests": cv_regression_tests,
                "youraies_3dgs_bridge_contract": youraies_3dgs_bridge_contract,
                "youraies_3dgs_bridge_tests": youraies_3dgs_bridge_tests,
                "youraies_3dgs_georeg_track_contract": youraies_3dgs_georeg_track_contract,
                "sisfron_fused_scene_track_contract": sisfron_fused_scene_track_contract,
                "fused_scene_track_tests": fused_scene_track_tests,
            }
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn source_contains_all(src: &str, needles: &[&str]) -> bool {
    needles.iter().all(|needle| src.contains(needle))
}

fn push_evidence(
    evidence: &mut Vec<EvidenceItem>,
    path: &Path,
    src: Option<&String>,
    needle: &str,
    detail: &str,
) {
    if let Some(src) = src
        && let Some(line) = find_line(src, needle)
    {
        evidence.push(EvidenceItem {
            kind: "sisfron_ooda_runtime".to_string(),
            path: path.display().to_string(),
            line: Some(line),
            detail: detail.to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::doctor_sisfron_ooda_runtime;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "leio-code-sisfron-ooda-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("cartridges/sisfron/tests")).expect("create tests dir");
        root
    }

    fn write_fixture(root: &Path, relative: &str, content: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, content).expect("write fixture");
    }

    #[test]
    fn detects_happy_path_sisfron_ooda_runtime_contract() {
        let root = temp_root("happy");
        write_fixture(
            &root,
            "cartridges/sisfron/engine.py",
            r#"
def orient():
    contact_observations = observations
    pitcic_product = self._run_pitcic(cycle_id, cycle_data, contact_observations)
    self.r.set(self._key(cycle_id, "pitcic"), pitcic_product.model_dump_json())
def get_cycle():
    result["pitcic"] = json.loads(raw_pitcic)
    result["feedback_arcs"] = [json.loads(item) for item in raw_feedback]
    payload = {"feedback_count": feedback_count,}
def _run_pitcic(
):
    pass
def _publish_event(
):
    serialized = json.dumps(payload, default=str)
    self.r.rpush(self._key("events"), serialized)
    "CycleCreated"
    "ObservationAdded"
    "OptionsGenerated"
    "ActionAuthorized"
    "ActionDispatched"
    "FeedbackReceived"
    "CycleResolved"
def list_events(
):
    pass
def _create_snapshot(self, cycle_id: str, phase: str) -> CycleSnapshot:
    named_graph_uri=f"urn:sisfron:cycle:{cycle_id}:snapshot:{phase}:{snapshot_id}"
def create():
    self._create_snapshot(cycle_id, OODAPhase.OBSERVE.value)
def transition():
    self._create_snapshot(cycle_id, new_phase.value)
def _archive_to_consumption(self, cycle_id: str, *, duration_ms: int, tempo_met: bool) -> None:
    self.r.set(self._key("lake", "consumption", cycle_id), record.model_dump_json())
def _record_resolution_metrics(
):
    self.r.hset(self._key("metrics", cycle_id), mapping=metrics)
    self._key("metrics", "resolved")
def _record_external_call(
):
    self.r.rpush(self._key("llm", "calls"), json.dumps(record))
def _record_feedback_arc(self, cycle_id: str, *, action_id: str, result: str) -> FeedbackArc:
    self.r.rpush(self._key(cycle_id, "feedback"), feedback.model_dump_json())
def _enqueue_dispatch(self, cycle_id: str, action: AuthorizedAction) -> None:
    self.r.rpush(self._key("dispatch", channel), json.dumps(envelope))
def _build_scene_context():
    scene_track_counts: Counter[str] = Counter()
    continuous_scene_update_count += 1
    persistent_scene_track_ids = sorted([])
    calibration_healthy = calibration_score is None or calibration_score >= 0.6
def _scene_context_patterns():
    f"scene:track:{track_id}"
    "scene:persistent_3dgs_track"
    "scene:georegistered"
    "scene:calibration:healthy"
"#,
        );
        write_fixture(
            &root,
            "cartridges/sisfron/types.py",
            r#"
class FusedScenePayload(BaseModel):
    scene_track_id: str | None = None
    temporal_alignment_score: float | None = None
    georegistration_confidence: float | None = None
    calibration_score: float | None = None
    calibration_healthy: bool = True
class SceneContext(BaseModel):
    persistent_scene_track_ids: list[str] = Field(default_factory=list)
    continuous_scene_update_count: int = Field(0, ge=0)
    temporal_alignment_score: float | None = Field(None, ge=0.0, le=1.0)
    georegistration_confidence: float | None = Field(None, ge=0.0, le=1.0)
    calibration_score: float | None = Field(None, ge=0.0, le=1.0)
    calibration_healthy: bool = True
"#,
        );
        write_fixture(
            &root,
            "cartridges/sisfron/router.py",
            r#"
"""module"""
async def events():
    async def event_stream_pubsub():
        pubsub_channel = _engine_key("events:pubsub")
    async def event_stream_poll():
        pass
    cursor = _get_engine().list_events(domain=domain, sector_id=sector_id)[1]
    events_batch, cursor = _get_engine().list_events(
        since_index=cursor,
        domain=domain,
        sector_id=sector_id,
    )
    return StreamingResponse(event_stream(), media_type="text/event-stream")
class CvImageRequest(BaseModel):
    pass
class CvChangeRequest(BaseModel):
    pass
def _decode_base64_image(image_base64: str):
    pass
def _serialize_cv_result(payload: dict[str, Any]) -> dict[str, Any]:
    pass
@_auth_router.post("/cv/segment")
async def cv_segment():
    image = _decode_base64_image(req.image_base64)
@_auth_router.post("/cv/detect")
async def cv_detect():
    image = _decode_base64_image(req.image_base64)
@_auth_router.post("/cv/changes")
async def cv_changes():
    before = _decode_base64_image(req.before_image_base64)
    after = _decode_base64_image(req.after_image_base64)
"#,
        );
        write_fixture(
            &root,
            "cartridges/sisfron/monitoring.py",
            "metrics:{cycle_id}\nmetrics:resolved\nllm:calls\n",
        );
        write_fixture(
            &root,
            "cartridges/sisfron/tests/test_engine_runtime.py",
            r#"
def test_orient_persists_pitcic_and_emits_events():
    pass
def test_act_enqueues_dispatch_and_feedback_arc():
    pass
def test_resolve_archives_consumption_and_metrics():
    pass
def test_orient_uses_persistent_3dgs_track_continuity():
    pass
def test_orient_blocks_scene_corroboration_when_calibration_is_degraded():
    pass
"#,
        );
        write_fixture(
            &root,
            "cartridges/sisfron/tests/test_router_cv.py",
            r#"
def test_cv_segment_returns_serializable_payload():
    pass
def test_cv_detect_returns_detection_payload():
    pass
def test_cv_changes_returns_change_payload():
    pass
"#,
        );
        write_fixture(
            &root,
            "youraies/src/youraies/camera/gaussian_splatting.py",
            r#"
def __init__():
    self.scene_track_id = _scene_track_id(config.rig_id, views)
def _build_manifest():
    _read_calibration_metrics()
    manifest = {
        "georegistration": _build_georegistration(self.config),
        "calibration_health": calibration_health,
        "sync": {
            "reference_timestamp": _iso_timestamp(reference_timestamp_s),
            "views": _build_sync_records(samples, self.views, reference_timestamp_s),
        },
        "track": {
            "scene_track_id": self.scene_track_id,
            "continuity": "continuous" if sequence > 0 else "initialized",
        },
    }
def _build_georegistration():
    pass
def _build_sync_records():
    pass
def _build_calibration_health():
    backend_metrics
    "refinement": {}
def _read_calibration_metrics():
    pass
def _scene_track_id():
    pass
"#,
        );
        write_fixture(
            &root,
            "youraies/src/youraies/streaming/app.py",
            r#"
def _require_auth(
):
    pass
def _public_manifest_uri():
    pass
def _public_image_uri(camera_id):
    pass
def _public_manifest():
    view["image_uri"] = _public_image_uri(str(camera_id))
def _public_observation():
    payload["manifest_uri"] = _public_manifest_uri()
def _read_latest_3dgs_manifest():
    local_paths_redacted
@app.get("/api/fusion/3dgs/status")
async def fusion_3dgs_status():
    _require_auth(authorization, key, _fusion_source_id())
@app.get("/api/fusion/3dgs/latest")
async def latest_3dgs_manifest():
    _require_auth(authorization, key, _fusion_source_id())
@app.get("/api/fusion/3dgs/artifacts/latest/manifest")
async def latest_3dgs_manifest_artifact():
    _require_auth(authorization, key, _fusion_source_id())
@app.get("/api/fusion/3dgs/artifacts/latest/images/{camera_id}")
async def latest_3dgs_image_artifact():
    _require_auth(authorization, key, _fusion_source_id())
@app.post("/api/fusion/3dgs/dispatch")
async def dispatch_latest_3dgs_manifest():
    _require_auth(authorization, key, _fusion_source_id())
    latest = _read_latest_3dgs_manifest(config, expose_local_paths=True)
"#,
        );
        write_fixture(
            &root,
            "youraies/tests/test_streaming_app.py",
            r#"
def test_latest_3dgs_manifest_returns_sisfron_observation():
    pass
def test_3dgs_bridge_requires_auth_when_stream_auth_enabled():
    pass
def test_3dgs_artifact_routes_return_portable_manifest_and_image():
    pass
"#,
        );
        write_fixture(
            &root,
            "youraies/tests/test_gaussian_splatting.py",
            r#"
def test_keyframe_writer_exports_calibrated_bundle():
    assert second_manifest["track"]["continuity"] == "continuous"
    assert manifest["georegistration"]["coordinate_frame"]
    assert manifest["calibration_health"]["status"]
def test_external_backend_metrics_refine_calibration_health():
    pass
def test_splat_manifest_to_sisfron_observation():
    assert observation["payload"]["scene_track_id"]
    assert observation["payload"]["quality"]["calibration_score"]
"#,
        );

        let envelope = doctor_sisfron_ooda_runtime(&root);
        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn warns_when_event_log_contract_disappears() {
        let root = temp_root("warn");
        write_fixture(
            &root,
            "cartridges/sisfron/engine.py",
            "def _run_pitcic(\n):\n    pass\nself.r.set(self._key(cycle_id, \"pitcic\"), pitcic_product.model_dump_json())\nresult[\"pitcic\"] = json.loads(raw_pitcic)\ndef _record_resolution_metrics(\n):\n    self.r.hset(self._key(\"metrics\", cycle_id), mapping=metrics)\n    self._key(\"metrics\", \"resolved\")\ndef _record_external_call(\n):\n    self.r.rpush(self._key(\"llm\", \"calls\"), json.dumps(record))\ndef _record_feedback_arc(self, cycle_id: str, *, action_id: str, result: str) -> FeedbackArc:\n    self.r.rpush(self._key(cycle_id, \"feedback\"), feedback.model_dump_json())\ndef _enqueue_dispatch(self, cycle_id: str, action: AuthorizedAction) -> None:\n    self.r.rpush(self._key(\"dispatch\", channel), json.dumps(envelope))\ndef _create_snapshot(self, cycle_id: str, phase: str) -> CycleSnapshot:\n    named_graph_uri=f\"urn:sisfron:cycle:{cycle_id}:snapshot:{phase}:{snapshot_id}\"\nself._create_snapshot(cycle_id, OODAPhase.OBSERVE.value)\nself._create_snapshot(cycle_id, new_phase.value)\ndef _archive_to_consumption(self, cycle_id: str, *, duration_ms: int, tempo_met: bool) -> None:\n    self.r.set(self._key(\"lake\", \"consumption\", cycle_id), record.model_dump_json())\n",
        );
        write_fixture(
            &root,
            "cartridges/sisfron/router.py",
            "\"\"\"SSE stream backed by the dedicated SISFRON event log.\"\"\"\ncursor = _get_engine().list_events(domain=domain, sector_id=sector_id)[1]\nevents_batch, cursor = _get_engine().list_events(\nmedia_type=\"text/event-stream\"\nclass CvImageRequest(BaseModel):\nclass CvChangeRequest(BaseModel):\ndef _decode_base64_image(image_base64: str):\ndef _serialize_cv_result(payload: dict[str, Any]) -> dict[str, Any]:\n@_auth_router.post(\"/cv/segment\")\n@_auth_router.post(\"/cv/detect\")\n@_auth_router.post(\"/cv/changes\")\nimage = _decode_base64_image(req.image_base64)\nbefore = _decode_base64_image(req.before_image_base64)\nafter = _decode_base64_image(req.after_image_base64)\n",
        );
        write_fixture(
            &root,
            "cartridges/sisfron/monitoring.py",
            "metrics:{cycle_id}\nmetrics:resolved\nllm:calls\n",
        );
        write_fixture(
            &root,
            "cartridges/sisfron/tests/test_engine_runtime.py",
            "def test_orient_persists_pitcic_and_emits_events():\n    pass\ndef test_act_enqueues_dispatch_and_feedback_arc():\n    pass\ndef test_resolve_archives_consumption_and_metrics():\n    pass\n",
        );
        write_fixture(
            &root,
            "cartridges/sisfron/tests/test_router_cv.py",
            "def test_cv_segment_returns_serializable_payload():\n    pass\ndef test_cv_detect_returns_detection_payload():\n    pass\ndef test_cv_changes_returns_change_payload():\n    pass\n",
        );

        let envelope = doctor_sisfron_ooda_runtime(&root);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("dedicated SISFRON event log")),
            "{:?}",
            envelope.warnings
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn warns_when_youraies_3dgs_bridge_contract_disappears() {
        let root = temp_root("youraies-bridge-warn");

        let envelope = doctor_sisfron_ooda_runtime(&root);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("3DGS fusion bridge")),
            "{:?}",
            envelope.warnings
        );

        let _ = fs::remove_dir_all(root);
    }
}
