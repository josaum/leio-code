use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct GatewayOarBoundaryDoctor;

impl Doctor for GatewayOarBoundaryDoctor {
    fn name(&self) -> &'static str {
        "gateway-oar-boundary"
    }

    fn description(&self) -> &'static str {
        "Checks that example-gateway keeps OCR-heavy `oar` code behind explicit feature and bootstrap boundaries."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_gateway_oar_boundary(root)
    }
}

pub fn doctor_gateway_oar_boundary(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let cargo_path = root.join("example-gateway/Cargo.toml");
    let lib_path = root.join("example-gateway/src/lib.rs");
    let main_path = root.join("example-gateway/src/main.rs");
    let runtime_path = root.join("example-gateway/src/ocr_runtime.rs");
    let app_path = root.join("example-gateway/src/app.rs");
    let collider_path = root.join("example-gateway/src/collider.rs");
    let pdf_types_path = root.join("example-gateway/src/pdf_types.rs");
    let ops_types_path = root.join("example-gateway/src/ops_console/types.rs");
    let flight_mod_path = root.join("example-gateway/src/flight/mod.rs");
    let flight_ocr_path = root.join("example-gateway/src/flight/ocr.rs");
    let extractor_runner_path = root.join("example-gateway/src/extractor_runner.rs");
    let cli_path = root.join("example-gateway/src/cli/mod.rs");
    let dockerfile_path = root.join("example-gateway/Dockerfile");
    let compose_path = root.join("example-api/docker-compose.yml");
    let start_gateway_path = root.join("example-gateway/start-gateway.sh");
    let start_chatbot_path = root.join("example-api/start-chatbot.sh");
    let defaults_env_path = root.join("deploy/defaults.env");
    let makefile_path = root.join("Makefile");
    let extractor_cargo_path = root.join("example-extractor/src-tauri/Cargo.toml");
    let extractor_build_path = root.join("example-extractor/src-tauri/build.rs");

    let cargo_src = read_text(&cargo_path, &mut warnings);
    let lib_src = read_text(&lib_path, &mut warnings);
    let main_src = read_text(&main_path, &mut warnings);
    let runtime_src = read_text(&runtime_path, &mut warnings);
    let app_src = read_text(&app_path, &mut warnings);
    let collider_src = read_text(&collider_path, &mut warnings);
    let pdf_types_src = read_text(&pdf_types_path, &mut warnings);
    let ops_types_src = read_text(&ops_types_path, &mut warnings);
    let flight_mod_src = read_text(&flight_mod_path, &mut warnings);
    let flight_ocr_src = read_text(&flight_ocr_path, &mut warnings);
    let extractor_runner_src = read_text(&extractor_runner_path, &mut warnings);
    let cli_src = read_text(&cli_path, &mut warnings);
    let dockerfile_src = read_text(&dockerfile_path, &mut warnings);
    let compose_src = read_text(&compose_path, &mut warnings);
    let start_gateway_src = read_text(&start_gateway_path, &mut warnings);
    let start_chatbot_src = read_text(&start_chatbot_path, &mut warnings);
    let defaults_env_src = read_text(&defaults_env_path, &mut warnings);
    let makefile_src = read_text(&makefile_path, &mut warnings);
    let extractor_cargo_src = read_text(&extractor_cargo_path, &mut warnings);
    let extractor_build_src = read_text(&extractor_build_path, &mut warnings);

    let cargo_requires_oar_bins = cargo_src.as_deref().is_some_and(|src| {
        src.contains("[[bin]]\nname = \"test_ocr\"")
            && src.contains("[[bin]]\nname = \"pdf2excel\"")
            && src.matches("required-features = [\"oar\"]").count() >= 2
    });
    let cargo_declares_oar_boundary = cargo_src.as_deref().is_some_and(|src| {
        src.contains("oar = [")
            && src.contains("vl = [")
            && src.contains("all = [\"oar\", \"vl\", \"hybrid-llm\"]")
    });
    let lib_gates_ocr_modules = lib_src.as_deref().is_some_and(|src| {
        src.contains("#[cfg(feature = \"oar\")]\npub mod ocr_config;")
            && src.contains("#[cfg(feature = \"oar\")]\npub mod ocr_pipeline;")
            && src.contains("#[cfg(feature = \"oar\")]\npub mod ocr_vis;")
            && src.contains("#[cfg(feature = \"oar\")]\npub mod pdf_render;")
            && src.contains("#[cfg(feature = \"oar\")]\npub mod pdf_handler;")
            && src.contains("pub mod pdf_types;")
    });
    let main_uses_boundary_module = main_src.as_deref().is_some_and(|src| {
        src.contains("mod ocr_runtime;")
            && src.contains("let ocr = ocr_runtime::bootstrap_ocr_runtime(&args)?;")
            && !src.contains("use one_file_gateway::{ocr_config, ocr_pipeline};")
            && !src.contains("ocr_config::")
            && !src.contains("ocr_pipeline::")
            && !src.contains("let ocr_det_target =")
            && !src.contains("let ocr_rec_target =")
            && !src.contains("let ocr_layout_target =")
            && !src.contains("let ocr_table_target =")
            && !src.contains("let ocr_keys_target =")
    });
    let main_uses_stable_collider = main_src.as_deref().is_some_and(|src| {
        src.contains("let ocr = ocr_runtime::bootstrap_ocr_runtime(")
            && (src.contains("collider: ocr_runtime::bootstrap_collider(&ocr)?,")
                || src.contains("let collider = ocr_runtime::bootstrap_collider(&ocr)?;"))
            && !src
                .contains("let ocr_pipeline: Arc<Mutex<Option<()>>> = Arc::new(Mutex::new(None));")
    });
    let runtime_owns_bootstrap = runtime_src.as_deref().is_some_and(|src| {
        src.contains("pub fn bootstrap_ocr_runtime(")
            && src.contains("pub fn bootstrap_collider(")
            && src.contains("pub fn attach_collider_ocr(")
            && src.contains("fn build_profile_pipeline(")
            && src.contains("fn build_cli_pipeline(")
            && src.contains("use one_file_gateway::{app::OcrRuntimeState, cli::Args, collider::LockedCollider};")
            && src.contains("ModelManager::ensure_model")
    });
    let app_wraps_ocr_runtime = app_src.as_deref().is_some_and(|src| {
        src.contains("pub ocr: OcrRuntimeState,")
            && src.contains("pub struct OcrRuntimeState {")
            && src.contains("self.ocr.ensure_initialized().await")
            && src.contains("self.ocr.is_ready()")
            && !src.contains("pub ocr_pipeline:")
            && !src.contains("pub ocr_pipeline_config:")
    });
    let collider_uses_stable_boundary = collider_src.as_deref().is_some_and(|src| {
        src.contains("    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {")
            && src.contains("ocr_pipeline: None,")
            && src.contains("pub fn attach_ocr_pipeline(")
            && !src.contains("pub fn new(\n        #[cfg(feature = \"oar\")] ocr_pipeline")
    });
    let ops_types_gate_ocr_exports = ops_types_src.as_deref().is_some_and(|src| {
        src.contains("#[cfg(feature = \"oar\")]\nuse crate::ocr_config::{")
            && src.contains("use crate::pdf_types::{")
            && !src.contains("use crate::pdf_handler::{")
            && src.contains("#[cfg(feature = \"oar\")]\n    {")
    });
    let pdf_types_hold_shared_contracts = pdf_types_src.as_deref().is_some_and(|src| {
        src.contains("pub struct PdfDocument {")
            && src.contains("pub struct PdfPage {")
            && src.contains("pub struct PdfTable {")
            && src.contains("pub struct ExtractedEntity {")
            && src.contains("pub struct PdfProcessingResult {")
            && src.contains(
                "#[ts(export, export_to = \"../../packages/example-ops-contracts/generated/\")]",
            )
    });
    let flight_gates_ocr_processor = flight_mod_src.as_deref().is_some_and(|src| {
        src.contains("#[cfg(feature = \"oar\")]\npub mod ocr;")
            && src.contains("#[cfg(feature = \"oar\")]\nuse ocr::OCRProcessor;")
            && src.contains(
                "ocr disabled in this gateway build; enable `oar` to process OCR Flight requests",
            )
    });
    let flight_ocr_uses_extractor_executor =
        flight_ocr_src.as_deref().is_some_and(|src| {
            src.contains("ExtractorContractRunner")
                && src.contains("ExtractorContractRunner::discover()")
                && src.contains("example-extractor")
                && !src.contains("Extracted text from image")
        }) && extractor_runner_src.as_deref().is_some_and(|src| {
            src.contains("EXAMPLE_EXTRACTOR_CONTRACT_COMMAND")
                && src.contains("EXAMPLE_EXTRACTOR_CONTRACT_BIN")
                && src.contains("contract_extract_json")
                && src.contains("PathBuf::from(\"/app/contract_extract_json\")")
                && src.contains("tokio::process::Command")
        });
    let cli_gates_ocr_args = cli_src.as_deref().is_some_and(|src| {
        src.contains(
            "#[cfg(feature = \"oar\")]\n    #[arg(long)]\n    pub ocr_det_model: Option<String>,",
        ) && src.contains(
            "#[cfg(feature = \"oar\")]\n    #[arg(long)]\n    pub ocr_rec_model: Option<String>,",
        ) && src.contains(
            "#[cfg(feature = \"oar\")]\n    #[arg(long, default_value = \"oar-structure\")]\n    pub ocr_pipeline: String,",
        ) && src.contains(
            "#[cfg(feature = \"oar\")]\n    #[arg(long, default_value = \"quality\")]\n    pub ocr_profile: Option<String>,",
        ) && src.contains(
            "#[cfg(feature = \"oar\")]\n    #[arg(long)]\n    pub vl_model_dir: Option<String>,",
        )
    });
    let extractor_contract_decouples_desktop = extractor_cargo_src.as_deref().is_some_and(|src| {
        src.contains("required-features = [\"desktop\"]")
            && src.contains("name = \"contract_extract_json\"")
            && src.contains("tauri = { version = \"2\", features = [], optional = true }")
            && src.contains("tauri-plugin-dialog = { version = \"2\", optional = true }")
            && src.contains("tauri-plugin-shell = { version = \"2\", optional = true }")
            && src.contains("desktop = [\"dep:tauri\", \"dep:tauri-plugin-dialog\", \"dep:tauri-plugin-shell\"]")
    });
    let extractor_build_skips_tauri_headless = extractor_build_src.as_deref().is_some_and(|src| {
        src.contains("CARGO_FEATURE_DESKTOP") && src.contains("tauri_build::build()")
    });
    let dockerfile_bakes_extractor_bin = dockerfile_src.as_deref().is_some_and(|src| {
        src.contains("cargo build --release --no-default-features --bin contract_extract_json")
            && (src.contains("COPY --from=extractor-builder /tmp/contract_extract_json /app/contract_extract_json")
                || src.contains("COPY --from=extractor-builder /app/example-extractor/src-tauri/target/release/contract_extract_json /app/contract_extract_json"))
            && src.contains("ENV EXAMPLE_EXTRACTOR_CONTRACT_BIN=/app/contract_extract_json")
    });
    let dockerfile_bakes_oar_ocr_bin = dockerfile_src.as_deref().is_some_and(|src| {
        src.contains("oar-ocr-cli/Cargo.toml")
            && src.contains("COPY --from=extractor-builder /tmp/oar-ocr-cli /app/oar-ocr-cli")
            && src.contains("ENV EXAMPLE_EXTRACTOR_OAR_OCR_BIN=/app/oar-ocr-cli")
            && src.contains("ENV EXAMPLE_OAR_OCR_BIN=/app/oar-ocr-cli")
    });
    let compose_preserves_extractor_default = compose_src.as_deref().is_some_and(|src| {
        src.contains("EXAMPLE_EXTRACTOR_CONTRACT_BIN: ${EXAMPLE_EXTRACTOR_CONTRACT_BIN:-/app/contract_extract_json}")
    });
    let compose_preserves_oar_ocr_default = compose_src.as_deref().is_some_and(|src| {
        src.contains(
            "EXAMPLE_EXTRACTOR_OAR_OCR_BIN: ${EXAMPLE_EXTRACTOR_OAR_OCR_BIN:-/app/oar-ocr-cli}",
        ) && src.contains("EXAMPLE_OAR_OCR_BIN: ${EXAMPLE_OAR_OCR_BIN:-/app/oar-ocr-cli}")
    });
    let start_gateway_knows_container_bin = start_gateway_src
        .as_deref()
        .is_some_and(|src| src.contains("\"/app/contract_extract_json\""));
    let start_gateway_knows_oar_ocr_bin = start_gateway_src
        .as_deref()
        .is_some_and(|src| src.contains("\"/app/oar-ocr-cli\""));
    let start_chatbot_knows_container_bin = start_chatbot_src
        .as_deref()
        .is_some_and(|src| src.contains("\"/app/contract_extract_json\""));
    let defaults_env_uses_container_bin = defaults_env_src.as_deref().is_some_and(|src| {
        src.contains("EXAMPLE_EXTRACTOR_CONTRACT_BIN=${EXAMPLE_EXTRACTOR_CONTRACT_BIN:-/app/contract_extract_json}")
    });
    let defaults_env_uses_oar_ocr_bin = defaults_env_src.as_deref().is_some_and(|src| {
        src.contains(
            "EXAMPLE_EXTRACTOR_OAR_OCR_BIN=${EXAMPLE_EXTRACTOR_OAR_OCR_BIN:-/app/oar-ocr-cli}",
        ) && src.contains("EXAMPLE_OAR_OCR_BIN=${EXAMPLE_OAR_OCR_BIN:-/app/oar-ocr-cli}")
    });
    let makefile_builds_headless_contract_bin = makefile_src.as_deref().is_some_and(|src| {
        src.contains("extractor-contract-bin: verify")
            && src.contains("cargo build --manifest-path \"$(EXTRACTOR_TAURI)/Cargo.toml\" --no-default-features --bin contract_extract_json")
    });
    if !cargo_requires_oar_bins {
        warnings.push(
            "example-gateway/Cargo.toml does not require `oar` explicitly for OCR-only bins"
                .to_string(),
        );
    }
    if !cargo_declares_oar_boundary {
        warnings.push(
            "example-gateway/Cargo.toml does not expose the expected explicit `oar`/`vl` feature boundary"
                .to_string(),
        );
    }
    if !lib_gates_ocr_modules {
        warnings.push(
            "example-gateway/src/lib.rs does not gate OCR-heavy modules behind `feature = \"oar\"`"
                .to_string(),
        );
    }
    if !main_uses_boundary_module {
        warnings.push(
            "example-gateway/src/main.rs still owns raw OCR bootstrap logic instead of delegating to `ocr_runtime`"
                .to_string(),
        );
    }
    if !main_uses_stable_collider {
        warnings.push(
            "example-gateway/src/main.rs still constructs the collider through an OCR-shaped constructor instead of attaching OCR after construction"
                .to_string(),
        );
    }
    if !runtime_owns_bootstrap {
        warnings.push(
            "example-gateway/src/ocr_runtime.rs does not own the expected OCR bootstrap boundary"
                .to_string(),
        );
    }
    if !app_wraps_ocr_runtime {
        warnings.push(
            "example-gateway/src/app.rs still exposes raw OCR pipeline/config types instead of an OcrRuntimeState wrapper"
                .to_string(),
        );
    }
    if !collider_uses_stable_boundary {
        warnings.push(
            "example-gateway/src/collider.rs still exposes an OCR-shaped constructor instead of a stable attach-after-construction boundary"
                .to_string(),
        );
    }
    if !ops_types_gate_ocr_exports {
        warnings.push(
            "example-gateway/src/ops_console/types.rs still leaks OCR exports outside `feature = \"oar\"` guards or depends on `pdf_handler` instead of shared PDF contracts"
                .to_string(),
        );
    }
    if !pdf_types_hold_shared_contracts {
        warnings.push(
            "example-gateway/src/pdf_types.rs does not hold the shared neutral PDF contracts expected by the gateway/UI boundary"
                .to_string(),
        );
    }
    if !flight_gates_ocr_processor {
        warnings.push(
            "example-gateway/src/flight/mod.rs still compiles or dispatches OCR Flight processing outside the explicit `oar` boundary"
                .to_string(),
        );
    }
    if !flight_ocr_uses_extractor_executor {
        warnings.push(
            "example-gateway/src/flight/ocr.rs is not delegating OCR Flight processing through the example-extractor boundary or still contains placeholder extraction output"
                .to_string(),
        );
    }
    if !cli_gates_ocr_args {
        warnings.push(
            "example-gateway/src/cli/mod.rs still exposes OCR-only CLI flags outside the `oar` feature gate"
                .to_string(),
        );
    }
    if !extractor_contract_decouples_desktop {
        warnings.push(
            "example-extractor/src-tauri/Cargo.toml still couples `contract_extract_json` to desktop-only Tauri dependencies"
                .to_string(),
        );
    }
    if !extractor_build_skips_tauri_headless {
        warnings.push(
            "example-extractor/src-tauri/build.rs still runs tauri-build for headless contract binary builds"
                .to_string(),
        );
    }
    if !dockerfile_bakes_extractor_bin {
        warnings.push(
            "example-gateway/Dockerfile does not fully bake the extractor-backed OCR binary into the gateway image"
                .to_string(),
        );
    }
    if !dockerfile_bakes_oar_ocr_bin {
        warnings.push(
            "example-gateway/Dockerfile does not bake /app/oar-ocr-cli, so the extractor's local Paddle/OAR OCR fallback can silently disappear in the runtime image"
                .to_string(),
        );
    }
    if !compose_preserves_extractor_default {
        warnings.push(
            "example-api/docker-compose.yml can still override the gateway image default and lose the canonical /app/contract_extract_json path"
                .to_string(),
        );
    }
    if !compose_preserves_oar_ocr_default {
        warnings.push(
            "example-api/docker-compose.yml does not preserve the canonical /app/oar-ocr-cli fallback path for extractor child processes"
                .to_string(),
        );
    }
    if !start_gateway_knows_container_bin {
        warnings.push(
            "example-gateway/start-gateway.sh does not know the canonical container extractor path /app/contract_extract_json"
                .to_string(),
        );
    }
    if !start_gateway_knows_oar_ocr_bin {
        warnings.push(
            "example-gateway/start-gateway.sh does not know the canonical local Paddle/OAR fallback path /app/oar-ocr-cli"
                .to_string(),
        );
    }
    if !start_chatbot_knows_container_bin {
        warnings.push(
            "example-api/start-chatbot.sh does not know the canonical container extractor path /app/contract_extract_json"
                .to_string(),
        );
    }
    if !defaults_env_uses_container_bin {
        warnings.push(
            "deploy/defaults.env does not declare /app/contract_extract_json as the canonical default for the extractor-backed OCR boundary"
                .to_string(),
        );
    }
    if !defaults_env_uses_oar_ocr_bin {
        warnings.push(
            "deploy/defaults.env does not declare /app/oar-ocr-cli as the canonical fallback OCR binary for the extractor"
                .to_string(),
        );
    }
    if !makefile_builds_headless_contract_bin {
        warnings.push(
            "Makefile still builds contract_extract_json without the headless no-default-features path"
                .to_string(),
        );
    }

    for (path, src, needle, detail) in [
        (
            &cargo_path,
            cargo_src.as_ref(),
            "required-features = [\"oar\"]",
            "OCR-only bins explicitly require the `oar` feature",
        ),
        (
            &lib_path,
            lib_src.as_ref(),
            "#[cfg(feature = \"oar\")]\npub mod ocr_config;",
            "gateway lib gates OCR config behind the `oar` feature",
        ),
        (
            &lib_path,
            lib_src.as_ref(),
            "pub mod pdf_types;",
            "gateway lib exposes neutral PDF contracts separately from the PDF handler implementation",
        ),
        (
            &lib_path,
            lib_src.as_ref(),
            "#[cfg(feature = \"oar\")]\npub mod pdf_handler;",
            "gateway lib only compiles the PDF handler implementation when `oar` is enabled",
        ),
        (
            &main_path,
            main_src.as_ref(),
            "collider: ocr_runtime::bootstrap_collider(&ocr)?,",
            "gateway main delegates collider bootstrap to the OCR boundary module instead of wiring OCR into the constructor path inline",
        ),
        (
            &runtime_path,
            runtime_src.as_ref(),
            "pub fn bootstrap_collider(",
            "dedicated OCR boundary module owns OCR bootstrap, stable collider construction, and attach logic",
        ),
        (
            &app_path,
            app_src.as_ref(),
            "pub ocr: OcrRuntimeState,",
            "AppState owns OCR through a wrapper instead of exposing raw OCR pipeline/config fields",
        ),
        (
            &collider_path,
            collider_src.as_ref(),
            "pub fn attach_ocr_pipeline(",
            "Collider attaches OCR after stable construction instead of shaping its constructor around `oar`",
        ),
        (
            &pdf_types_path,
            pdf_types_src.as_ref(),
            "pub struct PdfDocument {",
            "neutral PDF contracts live in pdf_types.rs instead of the heavier handler module",
        ),
        (
            &ops_types_path,
            ops_types_src.as_ref(),
            "use crate::pdf_types::{",
            "ops console TS exports depend on neutral PDF contracts and only expose OCR types when `oar` is enabled",
        ),
        (
            &flight_mod_path,
            flight_mod_src.as_ref(),
            "#[cfg(feature = \"oar\")]\npub mod ocr;",
            "Flight only compiles the OCR processor when `oar` is enabled and otherwise returns `unimplemented`",
        ),
        (
            &flight_ocr_path,
            flight_ocr_src.as_ref(),
            "ExtractorContractRunner::discover()",
            "OCR Flight delegates real extraction through example-extractor instead of returning synthetic placeholder text",
        ),
        (
            &extractor_runner_path,
            extractor_runner_src.as_ref(),
            "PathBuf::from(\"/app/contract_extract_json\")",
            "extractor runner owns the canonical example-extractor binary discovery path",
        ),
        (
            &cli_path,
            cli_src.as_ref(),
            "#[cfg(feature = \"oar\")]\n    #[arg(long)]\n    pub ocr_det_model: Option<String>,",
            "gateway CLI only exposes OCR/VL flags when `oar` is enabled",
        ),
        (
            &extractor_cargo_path,
            extractor_cargo_src.as_ref(),
            "required-features = [\"desktop\"]",
            "extractor desktop app is feature-gated so contract_extract_json can build headlessly without Tauri runtime deps",
        ),
        (
            &extractor_build_path,
            extractor_build_src.as_ref(),
            "CARGO_FEATURE_DESKTOP",
            "extractor build script skips tauri-build when only the headless contract binary is being built",
        ),
        (
            &dockerfile_path,
            dockerfile_src.as_ref(),
            "ENV EXAMPLE_EXTRACTOR_CONTRACT_BIN=/app/contract_extract_json",
            "gateway image bakes and advertises the extractor-backed OCR binary at the canonical container path",
        ),
        (
            &dockerfile_path,
            dockerfile_src.as_ref(),
            "ENV EXAMPLE_EXTRACTOR_OAR_OCR_BIN=/app/oar-ocr-cli",
            "gateway image bakes and advertises the extractor local Paddle/OAR OCR fallback binary",
        ),
        (
            &compose_path,
            compose_src.as_ref(),
            "EXAMPLE_EXTRACTOR_CONTRACT_BIN: ${EXAMPLE_EXTRACTOR_CONTRACT_BIN:-/app/contract_extract_json}",
            "compose keeps the canonical extractor binary path instead of clearing the image default",
        ),
        (
            &compose_path,
            compose_src.as_ref(),
            "EXAMPLE_EXTRACTOR_OAR_OCR_BIN: ${EXAMPLE_EXTRACTOR_OAR_OCR_BIN:-/app/oar-ocr-cli}",
            "compose keeps the canonical extractor local Paddle/OAR OCR fallback path",
        ),
        (
            &defaults_env_path,
            defaults_env_src.as_ref(),
            "EXAMPLE_EXTRACTOR_CONTRACT_BIN=${EXAMPLE_EXTRACTOR_CONTRACT_BIN:-/app/contract_extract_json}",
            "deploy defaults treat /app/contract_extract_json as the canonical extractor binary path for gateway images",
        ),
        (
            &defaults_env_path,
            defaults_env_src.as_ref(),
            "EXAMPLE_EXTRACTOR_OAR_OCR_BIN=${EXAMPLE_EXTRACTOR_OAR_OCR_BIN:-/app/oar-ocr-cli}",
            "deploy defaults treat /app/oar-ocr-cli as the canonical extractor local Paddle/OAR OCR fallback path",
        ),
        (
            &makefile_path,
            makefile_src.as_ref(),
            "cargo build --manifest-path \"$(EXTRACTOR_TAURI)/Cargo.toml\" --no-default-features --bin contract_extract_json",
            "workspace Makefile builds the extractor contract binary through the same headless path used by the gateway image",
        ),
        (
            &start_chatbot_path,
            start_chatbot_src.as_ref(),
            "\"/app/contract_extract_json\"",
            "python-side startup autodiscovery also knows the canonical container extractor path",
        ),
        (
            &start_gateway_path,
            start_gateway_src.as_ref(),
            "\"/app/contract_extract_json\"",
            "gateway startup autodiscovery knows the canonical container extractor path",
        ),
        (
            &start_gateway_path,
            start_gateway_src.as_ref(),
            "\"/app/oar-ocr-cli\"",
            "gateway startup autodiscovery knows the canonical local Paddle/OAR OCR fallback path",
        ),
        (
            &dockerfile_path,
            dockerfile_src.as_ref(),
            "RUN cargo build --release --no-default-features --bin contract_extract_json",
            "gateway image builds the extractor contract binary without desktop features or Tauri runtime baggage",
        ),
    ] {
        if let Some(src) = src
            && let Some(line) = find_line(src, needle)
        {
            evidence.push(EvidenceItem {
                kind: "gateway_oar_boundary".to_string(),
                path: path.display().to_string(),
                line: Some(line),
                detail: detail.to_string(),
            });
        }
    }

    entities.push(json!({
        "path": cargo_path.display().to_string(),
        "requires_oar_bins": cargo_requires_oar_bins,
        "declares_oar_boundary": cargo_declares_oar_boundary,
    }));
    entities.push(json!({
        "path": lib_path.display().to_string(),
        "gates_ocr_modules": lib_gates_ocr_modules,
    }));
    entities.push(json!({
        "path": main_path.display().to_string(),
        "uses_boundary_module": main_uses_boundary_module,
        "uses_stable_collider": main_uses_stable_collider,
    }));
    entities.push(json!({
        "path": runtime_path.display().to_string(),
        "owns_bootstrap": runtime_owns_bootstrap,
    }));
    entities.push(json!({
        "path": app_path.display().to_string(),
        "wraps_ocr_runtime": app_wraps_ocr_runtime,
    }));
    entities.push(json!({
        "path": collider_path.display().to_string(),
        "uses_stable_boundary": collider_uses_stable_boundary,
    }));
    entities.push(json!({
        "path": pdf_types_path.display().to_string(),
        "holds_shared_contracts": pdf_types_hold_shared_contracts,
    }));
    entities.push(json!({
        "path": ops_types_path.display().to_string(),
        "gates_ocr_exports": ops_types_gate_ocr_exports,
    }));
    entities.push(json!({
        "path": flight_mod_path.display().to_string(),
        "gates_ocr_processor": flight_gates_ocr_processor,
    }));
    entities.push(json!({
        "path": flight_ocr_path.display().to_string(),
        "uses_extractor_executor": flight_ocr_uses_extractor_executor,
    }));
    entities.push(json!({
        "path": cli_path.display().to_string(),
        "gates_ocr_args": cli_gates_ocr_args,
    }));
    entities.push(json!({
        "path": extractor_cargo_path.display().to_string(),
        "contract_decouples_desktop": extractor_contract_decouples_desktop,
    }));
    entities.push(json!({
        "path": extractor_build_path.display().to_string(),
        "build_skips_tauri_headless": extractor_build_skips_tauri_headless,
    }));
    entities.push(json!({
        "path": dockerfile_path.display().to_string(),
        "bakes_extractor_bin": dockerfile_bakes_extractor_bin,
        "bakes_oar_ocr_bin": dockerfile_bakes_oar_ocr_bin,
    }));
    entities.push(json!({
        "path": compose_path.display().to_string(),
        "preserves_extractor_default": compose_preserves_extractor_default,
        "preserves_oar_ocr_default": compose_preserves_oar_ocr_default,
    }));
    entities.push(json!({
        "path": defaults_env_path.display().to_string(),
        "uses_container_bin": defaults_env_uses_container_bin,
        "uses_oar_ocr_bin": defaults_env_uses_oar_ocr_bin,
    }));
    entities.push(json!({
        "path": makefile_path.display().to_string(),
        "builds_headless_contract_bin": makefile_builds_headless_contract_bin,
    }));
    entities.push(json!({
        "path": start_chatbot_path.display().to_string(),
        "knows_container_bin": start_chatbot_knows_container_bin,
    }));
    entities.push(json!({
        "path": start_gateway_path.display().to_string(),
        "knows_container_bin": start_gateway_knows_container_bin,
        "knows_oar_ocr_bin": start_gateway_knows_oar_ocr_bin,
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_gateway_oar_boundary"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked gateway `oar` boundary, found {} warnings",
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
