//! Doctor: `gateway-ocr-pipeline`
//!
//! Asserts that the workspace runs **one OCR pipeline brain**
//! (`example-ocr-models`) and that every consumer (`oar-ocr-cli`,
//! `example-gateway`) routes through it for:
//!
//! - profile / tuning (`OcrProfile`, `tuning::OcrTuning`)
//! - hardware detection / recommended threads (CoreML → CUDA → CPU)
//! - recogniser ↔ dictionary pairing (`resolve_recognizer_pair`,
//!   `co_resolve_recognizer_and_dict`)
//! - preprocessing assets (orientation / textline / UVDoc rectification)
//! - explicit EP feature wiring per OS (`coreml` on macOS, `cuda` on Linux)
//!
//! Each missing assertion is the regression-shape we already paid for once
//! ("OCR accuracy is subpar"). Catching it at audit time means we don't
//! pay for it again.
//!
//! Production OCR Flight now terminates in the Rust gateway's `ocr:process`
//! endpoint (:9485). This doctor asserts that the Python OCR Flight *client*
//! (`ocr_client.py`) defaults to the gateway — not the retired Python Flight
//! server on :8815 — while the Python API's own local OCR fallback still runs
//! through the canonical `example.ocr.client` rather than calling engine
//! runners directly.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct GatewayOcrPipelineDoctor;

impl Doctor for GatewayOcrPipelineDoctor {
    fn name(&self) -> &'static str {
        "gateway-ocr-pipeline"
    }

    fn description(&self) -> &'static str {
        "Checks that the workspace runs one OCR pipeline brain (example-ocr-models) \
         and that the CLI / gateway consume it for tuning, hardware detection, \
         rec\u{2194}dict pairing, and preprocessing wiring."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        run(root)
    }
}

fn run(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    // Files we audit.
    let models_lib = root.join("office-parsers-rs/example-ocr-models/src/lib.rs");
    let cli_main = root.join("office-parsers-rs/oar-ocr-cli/src/main.rs");
    let cli_cargo = root.join("office-parsers-rs/oar-ocr-cli/Cargo.toml");
    let gw_config = root.join("example-gateway/src/ocr_config.rs");
    let gw_pipeline = root.join("example-gateway/src/ocr_pipeline.rs");
    let gw_runtime = root.join("example-gateway/src/ocr_runtime.rs");
    let py_ingest = root.join("example-api/example/routers/ingest.py");
    let py_ocr_router = root.join("example-api/example/routers/ocr.py");
    let py_flight_server = root.join("example-api/example/flight/server.py");
    let py_ocr_client = root.join("example-api/example/flight/ocr_client.py");
    let py_rust_bridge = root.join("example-api/example/flight/rust_bridge.py");
    let py_flight_init = root.join("example-api/example/flight/__init__.py");
    let api_compose = root.join("example-api/docker-compose.yml");

    let models_src = read_text(&models_lib, &mut warnings);
    let cli_src = read_text(&cli_main, &mut warnings);
    let cli_toml = read_text(&cli_cargo, &mut warnings);
    let cfg_src = read_text(&gw_config, &mut warnings);
    let pipe_src = read_text(&gw_pipeline, &mut warnings);
    let rt_src = read_text(&gw_runtime, &mut warnings);
    let py_ingest_src = read_text(&py_ingest, &mut warnings);
    let py_ocr_router_src = read_text(&py_ocr_router, &mut warnings);
    let py_flight_server_src = read_text(&py_flight_server, &mut warnings);
    let py_ocr_client_src = read_text(&py_ocr_client, &mut warnings);
    let py_rust_bridge_src = read_text(&py_rust_bridge, &mut warnings);
    let py_flight_init_src = read_text(&py_flight_init, &mut warnings);
    let api_compose_src = read_text(&api_compose, &mut warnings);

    // ── 1. example-ocr-models exposes the consolidated public surface ───
    let models_exposes_profile = contains_all(
        models_src.as_deref(),
        &["pub enum OcrProfile", "pub fn active_profile()"],
    );
    let models_exposes_tuning = contains_all(
        models_src.as_deref(),
        &["pub mod tuning", "pub struct OcrTuning"],
    );
    let models_exposes_hardware = contains_all(
        models_src.as_deref(),
        &[
            "pub fn detect_hardware()",
            "pub fn recommended_threads()",
            "pub struct HardwareCapabilities",
        ],
    );
    let models_exposes_rec_pairing = contains_all(
        models_src.as_deref(),
        &[
            "pub fn resolve_recognizer_pair()",
            "pub fn resolve_recognizer_pair_with(",
            "pub fn co_resolve_recognizer_and_dict(",
            "pub struct RecognizerPair",
            "pub enum RecognizerLanguage",
        ],
    );
    let models_exposes_preprocessing = contains_all(
        models_src.as_deref(),
        &[
            "pub struct PreprocessingAssets",
            "pub fn resolve_preprocessing_assets()",
            "pub fn resolve_preprocessing_assets_with(",
        ],
    );
    if !models_exposes_profile {
        warnings.push(
            "example-ocr-models does not expose the OcrProfile / active_profile() surface"
                .to_string(),
        );
    }
    if !models_exposes_tuning {
        warnings.push(
            "example-ocr-models is missing the shared `tuning` module (OcrTuning) — CLI / gateway will drift"
                .to_string(),
        );
    }
    if !models_exposes_hardware {
        warnings.push(
            "example-ocr-models is missing detect_hardware() / recommended_threads() — consumers will hard-code threads / EPs"
                .to_string(),
        );
    }
    if !models_exposes_rec_pairing {
        warnings.push(
            "example-ocr-models is missing the recogniser\u{2194}dictionary pairing surface (resolve_recognizer_pair / co_resolve_recognizer_and_dict)"
                .to_string(),
        );
    }
    if !models_exposes_preprocessing {
        warnings.push(
            "example-ocr-models is missing PreprocessingAssets / resolve_preprocessing_assets() — pre-stages will not auto-resolve"
                .to_string(),
        );
    }

    // ── 2. oar-ocr-cli consumes the shared brain and wires EPs ──────────
    let cli_uses_tuning = contains_all(
        cli_src.as_deref(),
        &[
            "tuning::OcrTuning",
            "OcrTuning::from_env()",
            "resolve_recognizer_pair_with",
        ],
    );
    let cli_wires_pre_stages = contains_all(
        cli_src.as_deref(),
        &[
            "with_document_image_orientation_classification",
            "with_text_line_orientation_classification",
            "with_document_image_rectification",
        ],
    );
    let cli_replaces_heuristic_gate =
        contains_any(
            cli_src.as_deref(),
            &["confidence_quality_score", "mean recognizer confidence"],
        ) && !contains_any(cli_src.as_deref(), &["fn extracted_text_quality_score"]);
    // The env var (`EXAMPLE_OCR_AUTO_INVERT`) is parsed in the shared
    // `tuning::OcrTuning` after the consolidation — the CLI just calls the
    // accessor.  Asserting on the *behaviour* here, not the string.
    let cli_handles_dark_pages = contains_all(
        cli_src.as_deref(),
        &[
            "fn is_dark_background",
            "composite_rgba_on_dominant",
            "auto_invert_dark_pages()",
        ],
    );
    let models_exposes_auto_invert_env =
        contains_all(models_src.as_deref(), &["EXAMPLE_OCR_AUTO_INVERT"]);
    let cli_runs_basic_first_then_falls_back = contains_all(
        cli_src.as_deref(),
        &[
            "match (requested_mode, structure)",
            "Ok(runs) if !runs.is_empty() => Ok(runs)",
        ],
    );

    let cli_cargo_macos_coreml = cli_toml.as_deref().is_some_and(|src| {
        src.contains("[target.'cfg(target_os = \"macos\")'.dependencies]")
            && src.contains("features = [\"coreml\", \"download-binaries\"]")
    });
    let cli_cargo_linux_cuda = cli_toml.as_deref().is_some_and(|src| {
        src.contains("[target.'cfg(target_os = \"linux\")'.dependencies]")
            && src.contains("features = [\"cuda\", \"download-binaries\"]")
    });

    if !cli_uses_tuning {
        warnings.push(
            "oar-ocr-cli/src/main.rs does not consume the shared example-ocr-models tuning surface"
                .to_string(),
        );
    }
    if !cli_wires_pre_stages {
        warnings.push(
            "oar-ocr-cli/src/main.rs does not wire all three pre-stages (orientation / textline / rectification) into the OAR builders"
                .to_string(),
        );
    }
    if !cli_replaces_heuristic_gate {
        warnings.push(
            "oar-ocr-cli/src/main.rs is still using the heuristic quality-score gate (extracted_text_quality_score) instead of mean recogniser confidence"
                .to_string(),
        );
    }
    if !cli_handles_dark_pages {
        warnings.push(
            "oar-ocr-cli/src/main.rs is not background-aware in load_rgb_image — dark-mode / photographed exports will be destroyed by the alpha blend"
                .to_string(),
        );
    }
    if !models_exposes_auto_invert_env {
        warnings.push(
            "example-ocr-models tuning module does not parse EXAMPLE_OCR_AUTO_INVERT — auto-invert toggle is unreachable from the workspace env schema"
                .to_string(),
        );
    }
    if !cli_runs_basic_first_then_falls_back {
        warnings.push(
            "oar-ocr-cli/src/main.rs still runs the basic + structure double-pass instead of structure-first + empty-fallback"
                .to_string(),
        );
    }
    if !cli_cargo_macos_coreml {
        warnings.push(
            "oar-ocr-cli/Cargo.toml does not enable the `coreml` EP feature on macOS targets"
                .to_string(),
        );
    }
    if !cli_cargo_linux_cuda {
        warnings.push(
            "oar-ocr-cli/Cargo.toml does not enable the `cuda` EP feature on Linux targets"
                .to_string(),
        );
    }

    // ── 3. example-gateway routes through example-ocr-models ────────────
    let cfg_uses_tuning = contains_all(
        cfg_src.as_deref(),
        &[
            "example_ocr_models::tuning::OcrTuning::from_env()",
            "example_ocr_models::recommended_threads()",
        ],
    );
    let cfg_uses_hw_detect = contains_all(
        cfg_src.as_deref(),
        &["example_ocr_models::detect_hardware()"],
    );
    let cfg_uses_co_resolve = contains_all(
        cfg_src.as_deref(),
        &["example_ocr_models::co_resolve_recognizer_and_dict("],
    );
    let cfg_passes_pre_stages = contains_all(
        cfg_src.as_deref(),
        &[
            "doc_orientation_model.as_deref()",
            "textline_orientation_model.as_deref()",
            "rectification_model.as_deref()",
        ],
    );
    let cfg_writes_full_det_config = contains_all(
        cfg_src.as_deref(),
        &[
            "tuning.det_unclip_ratio",
            "tuning.det_long_side",
            "tuning.det_max_side",
        ],
    );

    if !cfg_uses_tuning {
        warnings.push(
            "example-gateway/src/ocr_config.rs does not pull tuning + recommended_threads from example-ocr-models — it will drift from the CLI defaults"
                .to_string(),
        );
    }
    if !cfg_uses_hw_detect {
        warnings.push(
            "example-gateway/src/ocr_config.rs does not call example_ocr_models::detect_hardware() — Linux/NVIDIA hosts will silently fall back to CPU"
                .to_string(),
        );
    }
    if !cfg_uses_co_resolve {
        warnings.push(
            "example-gateway/src/ocr_config.rs does not co-resolve recogniser \u{2194} dictionary — TOML profile mistakes will silently corrupt OCR output"
                .to_string(),
        );
    }
    if !cfg_passes_pre_stages {
        warnings.push(
            "example-gateway/src/ocr_config.rs does not pass preprocessing pre-stage paths through to OarStructurePipeline::new"
                .to_string(),
        );
    }
    if !cfg_writes_full_det_config {
        warnings.push(
            "example-gateway/src/ocr_config.rs is not enriching TextDetectionConfig with the workspace tuning baseline (unclip_ratio / long_side / max_side)"
                .to_string(),
        );
    }

    let pipe_accepts_pre_stage_params = contains_all(
        pipe_src.as_deref(),
        &[
            "doc_orientation_model: Option<&str>",
            "textline_orientation_model: Option<&str>",
            "rectification_model: Option<&str>",
        ],
    );
    let pipe_wires_pre_stages = contains_all(
        pipe_src.as_deref(),
        &[
            "with_document_orientation",
            "with_text_line_orientation",
            "with_document_rectification",
        ],
    );
    let pipe_exposes_format_and_confidence = contains_all(
        pipe_src.as_deref(),
        &["pub fn format_structure_result(", "pub fn mean_confidence("],
    );

    if !pipe_accepts_pre_stage_params {
        warnings.push(
            "example-gateway/src/ocr_pipeline.rs OarStructurePipeline::new does not accept pre-stage model params"
                .to_string(),
        );
    }
    if !pipe_wires_pre_stages {
        warnings.push(
            "example-gateway/src/ocr_pipeline.rs does not wire pre-stages into the OARStructureBuilder — `quality` profile preprocessing is logged but never executed"
                .to_string(),
        );
    }
    if !pipe_exposes_format_and_confidence {
        warnings.push(
            "example-gateway/src/ocr_pipeline.rs does not expose `format_structure_result` / `mean_confidence` — callers cannot reuse the classical projection without code duplication"
                .to_string(),
        );
    }

    let rt_resolves_pre_stages = contains_all(
        rt_src.as_deref(),
        &["example_ocr_models::resolve_preprocessing_assets("],
    );
    let rt_co_resolves_dict = contains_all(
        rt_src.as_deref(),
        &["example_ocr_models::co_resolve_recognizer_and_dict("],
    );

    if !rt_resolves_pre_stages {
        warnings.push(
            "example-gateway/src/ocr_runtime.rs does not auto-resolve pre-stage assets for the CLI-args path"
                .to_string(),
        );
    }
    if !rt_co_resolves_dict {
        warnings.push(
            "example-gateway/src/ocr_runtime.rs CLI-args path does not co-resolve recogniser \u{2194} dictionary"
                .to_string(),
        );
    }

    // ── 5. Python API owns production OCR over gRPC ────────────────────
    let py_ingest_uses_ocr_client = contains_all(
        py_ingest_src.as_deref(),
        &["from example.flight.ocr_client import create_ocr_flight_client"],
    ) && !contains_any(
        py_ingest_src.as_deref(),
        &[
            "from example.flight.rust_bridge import create_rust_flight_client",
            "\"rust_ocr\"",
            "'rust_ocr'",
        ],
    );
    let py_flight_server_local_only = contains_all(
        py_flight_server_src.as_deref(),
        &[
            "def _handle_ocr(",
            "self._handle_local_ocr(table, writer)",
            "def _handle_local_ocr(",
            "from ..ocr.client import",
            "ocr_document as run_canonical_ocr_document",
            "run_canonical_ocr_document(",
        ],
    ) && !contains_any(
        py_flight_server_src.as_deref(),
        &[
            "EXAMPLE_OCR_FLIGHT_BACKEND",
            "_proxy_do_put_metadata",
            "_gateway_flight_location",
            "Failed to forward OCR request",
            "run_paddle_ocr_file",
            "run_paddle_vl_file",
            "run_vision_ocr_file",
        ],
    );
    let py_ocr_router_uses_canonical_client = contains_all(
        py_ocr_router_src.as_deref(),
        &[
            "from example.ocr.client import",
            "ocr_document as run_canonical_ocr_document",
            "await asyncio.to_thread(",
            "run_canonical_ocr_document,",
        ],
    ) && !contains_any(
        py_ocr_router_src.as_deref(),
        &[
            "run_paddle_ocr_file",
            "run_paddle_vl_file",
            "run_vision_ocr_file",
        ],
    );
    // The OCR Flight repoint is intentional and contract-proven: the Python
    // OCR client now defaults to the Rust gateway's `ocr:process` Flight
    // endpoint (:9485), matching core ingest. We assert the gateway-first
    // contract and flag the OPPOSITE drift — falling back to the retired
    // Python Flight server on :8815.
    let py_ocr_client_defaults_to_gateway_flight = contains_all(
        py_ocr_client_src.as_deref(),
        &[
            "class ExampleOcrFlightClient",
            "def create_ocr_flight_client(",
            "EXAMPLE_OCR_FLIGHT_URL",
            "EXAMPLE_GATEWAY_FLIGHT_URL",
            "grpc://localhost:9485",
        ],
    ) && !contains_any(
        py_ocr_client_src.as_deref(),
        &["grpc://localhost:8815", "grpc://api:8815"],
    );
    let py_rust_bridge_has_no_ocr = !contains_any(
        py_rust_bridge_src.as_deref(),
        &["class RustOCRResult", "def process_ocr(", "ocr_request"],
    );
    let py_flight_init_exports_ocr_client =
        contains_all(
            py_flight_init_src.as_deref(),
            &[
                "ExampleOcrFlightClient",
                "OcrFlightResult",
                "create_ocr_flight_client",
            ],
        ) && !contains_any(py_flight_init_src.as_deref(), &["RustOCRResult"]);
    let api_compose_points_ocr_to_python_flight = contains_all(
        api_compose_src.as_deref(),
        &["EXAMPLE_OCR_FLIGHT_URL", "grpc://api:8815"],
    );

    if !py_ingest_uses_ocr_client {
        warnings.push(
            "example-api/example/routers/ingest.py must use create_ocr_flight_client for PDF/image OCR and must not label OCR fallback as rust_ocr"
                .to_string(),
        );
    }
    if !py_flight_server_local_only {
        warnings.push(
            "example-api/example/flight/server.py must run local Python OCR through example.ocr.client.ocr_document and must not proxy OCR or call engine runners directly"
                .to_string(),
        );
    }
    if !py_ocr_router_uses_canonical_client {
        warnings.push(
            "example-api/example/routers/ocr.py must dispatch through example.ocr.client.ocr_document and must not call engine runners directly"
                .to_string(),
        );
    }
    if !py_ocr_client_defaults_to_gateway_flight {
        warnings.push(
            "example-api/example/flight/ocr_client.py must default its OCR Flight location to the Rust gateway (EXAMPLE_OCR_FLIGHT_URL / EXAMPLE_GATEWAY_FLIGHT_URL / grpc://localhost:9485) and must not fall back to the retired Python OCR Flight server on :8815"
                .to_string(),
        );
    }
    if !py_rust_bridge_has_no_ocr {
        warnings.push(
            "example-api/example/flight/rust_bridge.py still exposes OCR symbols; OCR should live behind ocr_client.py"
                .to_string(),
        );
    }
    if !py_flight_init_exports_ocr_client {
        warnings.push(
            "example-api/example/flight/__init__.py does not export the OCR Flight client cleanly, or still exports RustOCRResult"
                .to_string(),
        );
    }
    if !api_compose_points_ocr_to_python_flight {
        warnings.push(
            "example-api/docker-compose.yml does not set EXAMPLE_OCR_FLIGHT_URL to the Python API Flight endpoint"
                .to_string(),
        );
    }

    // ── Evidence: anchor every assertion to a (file, line) ──────────────
    for (path, src, needle, detail) in [
        (
            &models_lib,
            models_src.as_ref(),
            "pub mod tuning",
            "shared OCR tuning module (OcrTuning) lives in example-ocr-models",
        ),
        (
            &models_lib,
            models_src.as_ref(),
            "pub fn co_resolve_recognizer_and_dict(",
            "post-hoc rec\u{2194}dict pairing helper for callers that resolve assets independently",
        ),
        (
            &models_lib,
            models_src.as_ref(),
            "pub fn resolve_preprocessing_assets()",
            "auto-resolution of orientation / textline / rectification assets at the active profile",
        ),
        (
            &cli_main,
            cli_src.as_ref(),
            "tuning::OcrTuning::from_env()",
            "oar-ocr-cli builds its tuning bundle from the shared workspace surface",
        ),
        (
            &cli_main,
            cli_src.as_ref(),
            "with_document_image_orientation_classification",
            "oar-ocr-cli wires document orientation pre-stage",
        ),
        (
            &cli_main,
            cli_src.as_ref(),
            "fn confidence_quality_score(",
            "oar-ocr-cli quality gate uses recogniser confidence (not the heuristic letter/digit ratio)",
        ),
        (
            &cli_cargo,
            cli_toml.as_ref(),
            "features = [\"coreml\", \"download-binaries\"]",
            "oar-ocr-cli enables CoreML on macOS at compile time so MLProgram / ANE are available",
        ),
        (
            &cli_cargo,
            cli_toml.as_ref(),
            "features = [\"cuda\", \"download-binaries\"]",
            "oar-ocr-cli enables CUDA on Linux at compile time so NVIDIA hosts don't silently fall back to CPU",
        ),
        (
            &gw_config,
            cfg_src.as_ref(),
            "example_ocr_models::tuning::OcrTuning::from_env()",
            "gateway pulls TextDetectionConfig defaults from the shared OcrTuning",
        ),
        (
            &gw_config,
            cfg_src.as_ref(),
            "example_ocr_models::recommended_threads()",
            "gateway uses the hardware-aware thread recommendation",
        ),
        (
            &gw_config,
            cfg_src.as_ref(),
            "example_ocr_models::detect_hardware()",
            "gateway picks EP via shared hardware detection (CoreML / CUDA / CPU)",
        ),
        (
            &gw_config,
            cfg_src.as_ref(),
            "example_ocr_models::co_resolve_recognizer_and_dict(",
            "gateway profile path co-resolves recogniser \u{2194} dictionary so TOML mistakes can't silently corrupt OCR",
        ),
        (
            &gw_pipeline,
            pipe_src.as_ref(),
            "with_document_orientation",
            "OarStructurePipeline wires the document orientation pre-stage into the OARStructureBuilder",
        ),
        (
            &gw_pipeline,
            pipe_src.as_ref(),
            "with_document_rectification",
            "OarStructurePipeline wires the UVDoc rectification pre-stage",
        ),
        (
            &gw_runtime,
            rt_src.as_ref(),
            "example_ocr_models::resolve_preprocessing_assets(",
            "gateway CLI-args path auto-resolves pre-stage assets",
        ),
        (
            &gw_runtime,
            rt_src.as_ref(),
            "example_ocr_models::co_resolve_recognizer_and_dict(",
            "gateway CLI-args path co-resolves recogniser \u{2194} dictionary",
        ),
        (
            &gw_pipeline,
            pipe_src.as_ref(),
            "pub fn mean_confidence(",
            "mean_confidence is exposed so callers can reuse the classical projection without code duplication",
        ),
        (
            &py_ingest,
            py_ingest_src.as_ref(),
            "create_ocr_flight_client",
            "API ingest routes PDF/image OCR through the Python OCR Flight client",
        ),
        (
            &py_flight_server,
            py_flight_server_src.as_ref(),
            "run_canonical_ocr_document(",
            "Flight ocr:process dispatches through the canonical Python OCR client",
        ),
        (
            &py_ocr_router,
            py_ocr_router_src.as_ref(),
            "run_canonical_ocr_document,",
            "FastAPI /v2/ocr dispatches through the canonical Python OCR client",
        ),
        (
            &py_ocr_client,
            py_ocr_client_src.as_ref(),
            "EXAMPLE_OCR_FLIGHT_URL",
            "OCR Flight client resolves the Rust gateway ocr:process endpoint (:9485) first",
        ),
        (
            &py_rust_bridge,
            py_rust_bridge_src.as_ref(),
            "OCR is intentionally owned by the Python Flight server",
            "Rust bridge documents that OCR must use ocr_client.py",
        ),
        (
            &api_compose,
            api_compose_src.as_ref(),
            "EXAMPLE_OCR_FLIGHT_URL",
            "Compose points OCR Flight consumers at the Python API gRPC endpoint",
        ),
    ] {
        if let Some(src) = src
            && let Some(line) = find_line(src, needle)
        {
            evidence.push(EvidenceItem {
                kind: "gateway_ocr_pipeline".to_string(),
                path: path.display().to_string(),
                line: Some(line),
                detail: detail.to_string(),
            });
        }
    }

    entities.push(json!({
        "path": models_lib.display().to_string(),
        "exposes_profile": models_exposes_profile,
        "exposes_tuning": models_exposes_tuning,
        "exposes_hardware": models_exposes_hardware,
        "exposes_rec_pairing": models_exposes_rec_pairing,
        "exposes_preprocessing": models_exposes_preprocessing,
    }));
    entities.push(json!({
        "path": cli_main.display().to_string(),
        "uses_shared_tuning": cli_uses_tuning,
        "wires_pre_stages": cli_wires_pre_stages,
        "uses_confidence_gate": cli_replaces_heuristic_gate,
        "handles_dark_pages": cli_handles_dark_pages,
        "structure_first_with_fallback": cli_runs_basic_first_then_falls_back,
    }));
    entities.push(json!({
        "path": cli_cargo.display().to_string(),
        "macos_coreml_feature": cli_cargo_macos_coreml,
        "linux_cuda_feature": cli_cargo_linux_cuda,
    }));
    entities.push(json!({
        "path": gw_config.display().to_string(),
        "uses_shared_tuning": cfg_uses_tuning,
        "uses_hw_detect": cfg_uses_hw_detect,
        "uses_co_resolve": cfg_uses_co_resolve,
        "passes_pre_stages": cfg_passes_pre_stages,
        "writes_full_det_config": cfg_writes_full_det_config,
    }));
    entities.push(json!({
        "path": gw_pipeline.display().to_string(),
        "accepts_pre_stage_params": pipe_accepts_pre_stage_params,
        "wires_pre_stages": pipe_wires_pre_stages,
        "exposes_format_and_confidence": pipe_exposes_format_and_confidence,
    }));
    entities.push(json!({
        "path": gw_runtime.display().to_string(),
        "resolves_pre_stages": rt_resolves_pre_stages,
        "co_resolves_dict": rt_co_resolves_dict,
    }));
    entities.push(json!({
        "path": py_ingest.display().to_string(),
        "uses_python_ocr_client": py_ingest_uses_ocr_client,
    }));
    entities.push(json!({
        "path": py_flight_server.display().to_string(),
        "ocr_process_local_python_only": py_flight_server_local_only,
    }));
    entities.push(json!({
        "path": py_ocr_router.display().to_string(),
        "uses_canonical_ocr_client": py_ocr_router_uses_canonical_client,
    }));
    entities.push(json!({
        "path": py_ocr_client.display().to_string(),
        "defaults_to_gateway_flight": py_ocr_client_defaults_to_gateway_flight,
    }));
    entities.push(json!({
        "path": py_rust_bridge.display().to_string(),
        "has_no_ocr_symbols": py_rust_bridge_has_no_ocr,
    }));
    entities.push(json!({
        "path": py_flight_init.display().to_string(),
        "exports_ocr_client": py_flight_init_exports_ocr_client,
    }));
    entities.push(json!({
        "path": api_compose.display().to_string(),
        "points_ocr_to_python_flight": api_compose_points_ocr_to_python_flight,
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_gateway_ocr_pipeline"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked OCR pipeline consolidation across example-ocr-models / oar-ocr-cli / example-gateway, found {} warnings",
            warnings.len()
        ),
        confidence: if warnings.is_empty() { 0.95 } else { 0.69 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn contains_all(src: Option<&str>, needles: &[&str]) -> bool {
    src.is_some_and(|s| needles.iter().all(|n| s.contains(n)))
}

fn contains_any(src: Option<&str>, needles: &[&str]) -> bool {
    src.is_some_and(|s| needles.iter().any(|n| s.contains(n)))
}
