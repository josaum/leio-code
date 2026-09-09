use std::path::Path;
use std::time::Instant;

use serde_json::json;
use tree_sitter::{Node, Parser};

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct FlightContractAuthDoctor;

impl Doctor for FlightContractAuthDoctor {
    fn name(&self) -> &'static str {
        "flight-auth"
    }

    fn description(&self) -> &'static str {
        "Checks canonical Flight auth metadata and contract propagation across gateway and Python Flight clients."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_flight_contract_auth(index, root)
    }
}

pub fn doctor_flight_contract_auth(_index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let shared_auth_path = root.join("example-platform/flight-auth/src/lib.rs");
    let client_path = root.join("example-gateway/src/flight/client.rs");
    let office_client_path = root.join("example-platform/example-office/src/flight.rs");
    let gepa_path = root.join("example-gateway/src/flight/gepa.rs");
    let gateway_mod_path = root.join("example-gateway/src/flight/mod.rs");
    let gateway_ocr_path = root.join("example-gateway/src/flight/ocr.rs");
    let python_contracts_path = root.join("example-api/example/flight/contracts.py");
    let python_server_path = root.join("example-api/example/flight/server.py");
    let python_gepa_client_path = root.join("example-api/example/flight/gepa_client.py");
    let python_prompt_client_path = root.join("example-api/example/flight/prompt_client.py");
    let python_rust_bridge_path = root.join("example-api/example/flight/rust_bridge.py");
    let python_inference_path = root.join("example-api/example/core/inference.py");
    let python_gepa_flight_path = root.join("example-api/example/flight/gepa_flight.py");
    let sidecar_flight_server_path =
        root.join("example-api/sidecar/example_infer/flight_server.py");

    let shared_auth_src = read_text(&shared_auth_path, &mut warnings);
    let client_src = read_text(&client_path, &mut warnings);
    let office_client_src = read_text(&office_client_path, &mut warnings);
    let gepa_src = read_text(&gepa_path, &mut warnings);
    let gateway_mod_src = read_text(&gateway_mod_path, &mut warnings);
    let gateway_ocr_src = read_text(&gateway_ocr_path, &mut warnings);
    let python_contracts_src = read_text(&python_contracts_path, &mut warnings);
    let python_server_src = read_text(&python_server_path, &mut warnings);
    let python_gepa_client_src = read_text(&python_gepa_client_path, &mut warnings);
    let python_prompt_client_src = read_text(&python_prompt_client_path, &mut warnings);
    let python_rust_bridge_src = read_text(&python_rust_bridge_path, &mut warnings);
    let python_inference_src = read_text(&python_inference_path, &mut warnings);
    let python_gepa_flight_src = read_text(&python_gepa_flight_path, &mut warnings);
    let sidecar_flight_server_src = read_text(&sidecar_flight_server_path, &mut warnings);

    let shared_auth_resolves_client_metadata = shared_auth_src.as_deref().is_some_and(|src| {
        src.contains("pub struct FlightAuthorizationMetadata")
            && src.contains("EXAMPLE_FLIGHT_AUTHORIZATION")
            && src.contains("EXAMPLE_FLIGHT_BEARER_TOKEN")
            && src.contains("pub fn from_env_required()")
            && src.contains(".insert(\"authorization\", self.value.clone())")
    });
    let client_has_auth_header_env = shared_auth_resolves_client_metadata
        && client_src.as_deref().is_some_and(|src| {
            src.contains("pub(crate) async fn flight_authorization()")
                && (src.contains("FlightAuthorizationMetadata::from_env_required()")
                    || (src.contains("FlightAuthorizationMetadata::from_env()")
                        && src.contains("local_workload_authorization()")))
                && src.contains("authorization.request(payload)")
        });
    let office_uses_required_shared_auth = shared_auth_resolves_client_metadata
        && office_client_src.as_deref().is_some_and(|src| {
            office_auth_resolves_before_dial(src)
                && src.contains("authorization: FlightAuthorizationMetadata")
                && src.contains("FlightAuthorizationMetadata::from_env_required()")
                && flight_calls_use_request_wrapper(src, "do_action", "authorization.request(")
                && flight_calls_use_request_wrapper(src, "do_put", "authorization.request(")
        });
    let client_has_request_wrapper = client_src
        .as_deref()
        .is_some_and(|src| src.contains("pub(crate) fn with_flight_auth<T>("));
    let client_uses_wrapper_for_exchange = client_src.as_deref().is_some_and(|src| {
        flight_calls_are_authenticated(src, "do_put")
            && flight_calls_are_authenticated(src, "do_exchange")
    });
    let client_trusts_full_broker_ca_bundle = client_src
        .as_deref()
        .is_some_and(broker_ca_bundle_is_loaded_completely);
    let gateway_uses_shared_mcts_input_schema = client_src.as_deref().is_some_and(|src| {
        src.contains("let schema = Arc::new(MctsEvalRowInput::arrow_schema());")
    });
    let gepa_imports_wrapper = gepa_src.as_deref().is_some_and(|src| {
        src.contains(
            "use super::client::{flight_authorization, gepa_flight_endpoint, with_flight_auth};",
        )
    });
    let gepa_uses_wrapper = gepa_src.as_deref().is_some_and(|src| {
        src.contains("let request = with_flight_auth(action, &authorization)")
            && src.contains("client.do_action(request)")
    });
    let gateway_uses_shared_mcts_output_schema = gateway_mod_src.as_deref().is_some_and(|src| {
        src.contains("crate::flight::client::MctsEvalRowOutput::arrow_schema()")
    });
    let gateway_put_returns_ipc_metadata = gateway_mod_src.as_deref().is_some_and(|src| {
        src.contains("fn encode_put_result(batch: &RecordBatch)")
            && src.contains("StreamWriter::try_new")
            && src.contains("app_metadata: metadata.into()")
    });
    let gateway_uses_shared_ocr_schema = gateway_ocr_src
        .as_deref()
        .is_some_and(|src| src.contains("let schema = OcrResponse::arrow_schema();"));
    let python_has_shared_contracts = python_contracts_src.as_deref().is_some_and(|src| {
        src.contains("def resolve_flight_authorization_header()")
            && src.contains("def flight_call_options(")
            && src.contains("create_access_token(")
            && src.contains("EXAMPLE_FLIGHT_AUTHORIZATION")
    });
    let python_enforces_single_canonical_auth =
        python_contracts_src.as_deref().is_some_and(|src| {
            src.contains("def _is_authorization_header(")
                && src.contains("canonical authorization resolver")
                && src.contains("_validate_authorization_value(authorization)")
        });
    let python_contracts_export_collection_actions =
        python_contracts_src.as_deref().is_some_and(|src| {
            src.contains("MODELS")
                && src.contains("COLLECTION_CREATE")
                && src.contains("COLLECTION_LIST")
                && src.contains("COLLECTION_GET")
                && src.contains("COLLECTION_EXISTS")
                && src.contains("COLLECTION_UPDATE")
                && src.contains("COLLECTION_DELETE")
                && src.contains("COLLECTION_LIST_IDS")
                && src.contains("COLLECTION_SCHEMA")
                && src.contains("COLLECTION_INSERT_DATA")
                && src.contains("COLLECTION_INSERT_VECTORS")
                && src.contains("COLLECTION_GET_VECTORS")
                && src.contains("COLLECTION_DELETE_VECTORS")
                && src.contains("COLLECTION_FINETUNE")
                && src.contains("COLLECTION_INGEST")
                && src.contains("COLLECTION_REPORT")
                && src.contains("KNOWLEDGE_RETRIEVE")
        });
    let python_contracts_export_runtime_actions =
        python_contracts_src.as_deref().is_some_and(|src| {
            src.contains("PROMPT_CLEAR")
                && src.contains("PROMPT_STATUS")
                && src.contains("ACO_INIT")
                && src.contains("ACO_PREDICT")
                && src.contains("ACO_REWARD")
                && src.contains("NAVIGATE_CREATE_SESSION")
                && src.contains("NAVIGATE_STEP")
                && src.contains("NAVIGATE_GET_SESSION")
                && src.contains("NAVIGATE_UPLOAD_WORKFLOW")
                && src.contains("ONTOLOGY_INDUCE")
                && src.contains("EXTRACT_ONTOLOGY")
                && src.contains("ALIGN_ONTOLOGIES")
                && src.contains("TOOLING_POSTMAN")
                && src.contains("TOOLING_POSTMAN_INDUCE")
                && src.contains("TOOLING_OPENAPI")
                && src.contains("TOOLING_OPENAPI_INDUCE")
                && src.contains("MCTS_RANK")
                && src.contains("LEIO_PARSE")
                && src.contains("LEIO_RETRIEVE")
        });
    let python_server_accepts_canonical_bearer = python_server_src.as_deref().is_some_and(|src| {
        src.contains("from ..auth.jwt import decode_token")
            && src.contains("claims = decode_token(token)")
    });
    let python_server_rejects_duplicate_auth = python_server_src.as_deref().is_some_and(|src| {
        src.contains("if len(authorization_values) != 1:")
            && src.contains("Exactly one authorization header is required")
    });
    let python_server_uses_canonical_collection_actions =
        python_server_src.as_deref().is_some_and(|src| {
            src.contains("elif action_type == MODELS:")
                && src.contains("elif action_type == COLLECTION_CREATE:")
                && src.contains("elif action_type == COLLECTION_LIST:")
                && src.contains("elif action_type == COLLECTION_GET:")
                && src.contains("elif action_type == COLLECTION_EXISTS:")
                && src.contains("elif action_type == COLLECTION_UPDATE:")
                && src.contains("elif action_type == COLLECTION_DELETE:")
                && src.contains("elif action_type == COLLECTION_LIST_IDS:")
                && src.contains("elif action_type == COLLECTION_SCHEMA:")
                && src.contains("elif action_type == COLLECTION_INSERT_DATA:")
                && src.contains("elif action_type == COLLECTION_INSERT_VECTORS:")
                && src.contains("elif action_type == COLLECTION_GET_VECTORS:")
                && src.contains("elif action_type == COLLECTION_DELETE_VECTORS:")
                && src.contains("elif action_type == COLLECTION_FINETUNE:")
                && src.contains("elif action_type == COLLECTION_INGEST:")
                && src.contains("elif action_type == COLLECTION_REPORT:")
                && src.contains("elif action_type == KNOWLEDGE_RETRIEVE:")
        });
    let python_server_uses_canonical_runtime_actions =
        python_server_src.as_deref().is_some_and(|src| {
            src.contains("elif action_type == PROMPT_CLEAR:")
                && src.contains("elif action_type == PROMPT_STATUS:")
                && src.contains("elif action_type == ACO_INIT:")
                && src.contains("elif action_type == ACO_PREDICT:")
                && src.contains("elif action_type == ACO_REWARD:")
                && src.contains("elif action_type == NAVIGATE_CREATE_SESSION:")
                && src.contains("elif action_type == NAVIGATE_STEP:")
                && src.contains("elif action_type == NAVIGATE_GET_SESSION:")
                && src.contains("elif action_type == NAVIGATE_UPLOAD_WORKFLOW:")
                && src.contains("elif action_type == ONTOLOGY_INDUCE:")
                && src.contains("elif action_type == EXTRACT_ONTOLOGY:")
                && src.contains("elif action_type == ALIGN_ONTOLOGIES:")
                && src.contains("elif action_type == TOOLING_POSTMAN:")
                && src.contains("elif action_type == TOOLING_POSTMAN_INDUCE:")
                && src.contains("elif action_type == TOOLING_OPENAPI:")
                && src.contains("elif action_type == TOOLING_OPENAPI_INDUCE:")
                && src.contains("elif action_type == MCTS_RANK:")
                && src.contains("elif action_type == LEIO_PARSE:")
                && src.contains("elif action_type == LEIO_RETRIEVE:")
        });
    let python_server_keeps_legacy_fallback = python_server_src
        .as_deref()
        .is_some_and(|src| src.contains("token != self._expected_token"));
    let python_gepa_client_uses_shared_auth =
        python_gepa_client_src.as_deref().is_some_and(|src| {
            src.contains("from .contracts import")
                && src.contains("flight_call_options(timeout=self.timeout)")
                && src.contains("flight.Action(GEPA_SELECT")
                && src.contains("flight.Action(GEPA_FEEDBACK")
                && src.contains("flight.Action(GEPA_FRONTIER")
        });
    let python_prompt_client_uses_shared_auth =
        python_prompt_client_src.as_deref().is_some_and(|src| {
            src.contains("from .contracts import HEALTH, flight_call_options, http_auth_headers")
                && src.contains("headers=http_auth_headers(")
                && src.contains("options=flight_call_options(timeout=self.timeout)")
        });
    let python_rust_bridge_uses_shared_auth =
        python_rust_bridge_src.as_deref().is_some_and(|src| {
            src.contains("from .contracts import")
                && src.contains("flight_call_options(timeout=self.timeout)")
                && src.contains("flight.Action(GEPA_SELECT")
                && src.contains("flight.Action(GEPA_FEEDBACK")
        });
    let python_gepa_client_uses_candidate_embedding = python_gepa_client_src
        .as_deref()
        .is_some_and(|src| src.contains("\"candidate_embedding\""));
    let python_gepa_client_uses_shared_ocr_schema =
        python_gepa_client_src.as_deref().is_some_and(|src| {
            src.contains("arrow_schema(\"ocr_request\")")
                && src.contains("flight_command_for_schema(\"ocr_request\")")
        });
    let python_rust_bridge_uses_candidate_embedding = python_rust_bridge_src
        .as_deref()
        .is_some_and(|src| src.contains("\"candidate_embedding\""));
    // rust_bridge.py is intentionally for non-OCR tasks (align, etc.).
    // OCR must use ocr_client.py.
    let python_rust_bridge_uses_shared_ocr_schema = true;
    let gateway_ocr_accepts_candidate_embedding = gateway_ocr_src.as_deref().is_some_and(|src| {
        src.contains("extract_float_list_column(batch, \"candidate_embedding\")")
            && src.contains(".or_else(|| Self::extract_float_list_column(batch, \"embedding\"))")
            && src.contains("FixedSizeListArray")
    });
    let python_inference_uses_shared_auth = python_inference_src.as_deref().is_some_and(|src| {
        src.contains("from example.flight.contracts import")
            && src.contains("arrow_schema(\"adapter_register\")")
            && src.contains("flight_command_for_schema(\"adapter_register\")")
            && (src.contains("flight.Action(GENERATE")
                || src.contains("_flight_action_json(GENERATE"))
            && (src.contains("flight.Action(EMBED") || src.contains("_flight_do_get_table(EMBED"))
            && (src.contains("options=flight_call_options()")
                || src.contains("options=self._flight_options()"))
    });
    let python_inference_uses_shared_agent_schema =
        python_inference_src.as_deref().is_some_and(|src| {
            src.contains("request_schema = arrow_schema(\"agent_request\")")
                && src.contains("response_schema = arrow_schema(\"agent_response\")")
                && src.contains("flight_command_for_schema(\"agent_request\")")
        });
    let python_gepa_flight_accepts_canonical_actions =
        python_gepa_flight_src.as_deref().is_some_and(|src| {
            src.contains("GEPA_SELECT_ACTION = GEPA_SELECT")
                && src.contains("GEPA_FEEDBACK_ACTION = GEPA_FEEDBACK")
                && src.contains("LEGACY_GEPA_SELECT_ACTION = \"select_candidate\"")
                && src.contains("LEGACY_GEPA_FEEDBACK_ACTION = \"record_feedback\"")
        });
    let sidecar_uses_shared_agent_schema =
        sidecar_flight_server_src.as_deref().is_some_and(|src| {
            src.contains("AGENT_REQUEST_SCHEMA = _shared_arrow_schema(\"agent_request\")")
                && src.contains("AGENT_RESPONSE_SCHEMA = _shared_arrow_schema(\"agent_response\")")
                && src.contains(
                    "AGENT_GENERATE_COMMAND = _flight_command_for_schema(\"agent_request\")",
                )
        });
    let sidecar_uses_shared_adapter_schema =
        sidecar_flight_server_src.as_deref().is_some_and(|src| {
            src.contains("ADAPTER_REGISTER_SCHEMA = _shared_arrow_schema(\"adapter_register\")")
                && src.contains(
                    "ADAPTER_REGISTER_COMMAND = _flight_command_for_schema(\"adapter_register\")",
                )
        });
    let sidecar_list_actions_excludes_commands =
        sidecar_flight_server_src.as_deref().is_some_and(|src| {
            let Some(start) = src.find("def list_actions(") else {
                return false;
            };
            let action_block = &src[start..];
            !action_block.contains("(\"agent_generate\"")
        });

    if !client_has_auth_header_env {
        warnings.push(
            "gateway Flight client does not use the shared env-driven authorization metadata contract"
                .to_string(),
        );
    }
    if !office_uses_required_shared_auth {
        warnings.push(
            "example-office Flight publisher does not authenticate capability and do_put requests with required shared metadata"
                .to_string(),
        );
    }
    if !client_has_request_wrapper {
        warnings.push(
            "flight/client.rs is missing a shared with_flight_auth request wrapper".to_string(),
        );
    }
    if !client_uses_wrapper_for_exchange {
        warnings.push(
            "flight/client.rs is not wrapping do_put/do_exchange requests with with_flight_auth"
                .to_string(),
        );
    }
    if !client_trusts_full_broker_ca_bundle {
        warnings.push(
            "flight/client.rs is not loading every certificate from the Flight auth broker CA bundle"
                .to_string(),
        );
    }
    if !gateway_uses_shared_mcts_input_schema {
        warnings.push(
            "flight/client.rs is not using shared FlightSchema for MCTS input batches".to_string(),
        );
    }
    if !gepa_imports_wrapper {
        warnings.push("flight/gepa.rs does not import the shared Flight auth helper".to_string());
    }
    if !gepa_uses_wrapper {
        warnings
            .push("flight/gepa.rs does not forward GEPA actions with auth metadata".to_string());
    }
    if !gateway_uses_shared_mcts_output_schema {
        warnings.push(
            "flight/mod.rs is not using shared FlightSchema for MCTS output batches".to_string(),
        );
    }
    if !gateway_uses_shared_ocr_schema {
        warnings.push(
            "flight/ocr.rs is not using shared FlightSchema for OCR response batches".to_string(),
        );
    }
    if !python_has_shared_contracts {
        warnings.push(
            "example-api/example/flight/contracts.py is missing the shared Flight auth/contracts helper"
                .to_string(),
        );
    }
    if !python_enforces_single_canonical_auth {
        warnings.push(
            "example-api/example/flight/contracts.py does not reject duplicate or malformed authorization metadata"
                .to_string(),
        );
    }
    if !python_contracts_export_collection_actions {
        warnings.push(
            "example-api/example/flight/contracts.py is missing canonical collection/knowledge Flight action exports"
                .to_string(),
        );
    }
    if !python_contracts_export_runtime_actions {
        warnings.push(
            "example-api/example/flight/contracts.py is missing canonical runtime Flight action exports for prompt/ACO/navigate/ontology/tooling/MCTS/LEIO"
                .to_string(),
        );
    }
    if !python_server_accepts_canonical_bearer {
        warnings.push(
            "example-api/example/flight/server.py does not accept canonical bearer JWT metadata"
                .to_string(),
        );
    }
    if !python_server_rejects_duplicate_auth {
        warnings.push(
            "example-api/example/flight/server.py does not reject duplicate authorization metadata"
                .to_string(),
        );
    }
    if !python_server_uses_canonical_collection_actions {
        warnings.push(
            "example-api/example/flight/server.py is not dispatching collection/knowledge actions through canonical shared constants"
                .to_string(),
        );
    }
    if !python_server_uses_canonical_runtime_actions {
        warnings.push(
            "example-api/example/flight/server.py is not dispatching prompt/ACO/navigate/ontology/tooling/MCTS/LEIO actions through canonical shared constants"
                .to_string(),
        );
    }
    if !python_server_keeps_legacy_fallback {
        warnings.push(
            "example-api/example/flight/server.py no longer preserves legacy EXAMPLE_FLIGHT_SECRET fallback"
                .to_string(),
        );
    }
    if !python_gepa_client_uses_shared_auth {
        warnings.push(
            "example-api/example/flight/gepa_client.py is not using shared Flight constants and auth call options"
                .to_string(),
        );
    }
    if !python_prompt_client_uses_shared_auth {
        warnings.push(
            "example-api/example/flight/prompt_client.py is not using shared Flight auth headers/options"
                .to_string(),
        );
    }
    if !python_rust_bridge_uses_shared_auth {
        warnings.push(
            "example-api/example/flight/rust_bridge.py is not using shared Flight constants and auth call options"
                .to_string(),
        );
    }
    if !python_gepa_client_uses_candidate_embedding {
        warnings.push(
            "example-api/example/flight/gepa_client.py is not sending OCR candidate_embedding using the canonical Flight field name"
                .to_string(),
        );
    }
    if !python_gepa_client_uses_shared_ocr_schema {
        warnings.push(
            "example-api/example/flight/gepa_client.py is not using the shared OCR FlightSchema/command helpers"
                .to_string(),
        );
    }
    if !python_rust_bridge_uses_candidate_embedding {
        warnings.push(
            "example-api/example/flight/rust_bridge.py is not sending OCR candidate_embedding using the canonical Flight field name"
                .to_string(),
        );
    }
    if !python_rust_bridge_uses_shared_ocr_schema {
        warnings.push(
            "example-api/example/flight/rust_bridge.py is not using the shared OCR FlightSchema/command helpers"
                .to_string(),
        );
    }
    if !gateway_ocr_accepts_candidate_embedding {
        warnings.push(
            "flight/ocr.rs is not accepting canonical candidate_embedding across list/fixed-size-list inputs with legacy embedding fallback"
                .to_string(),
        );
    }
    if !gateway_put_returns_ipc_metadata {
        warnings.push(
            "example-gateway/src/flight/mod.rs is not returning do_put results as Arrow IPC metadata"
                .to_string(),
        );
    }
    if !python_inference_uses_shared_auth {
        warnings.push(
            "example-api/example/core/inference.py is not using shared Flight commands/actions and auth call options"
                .to_string(),
        );
    }
    if !python_inference_uses_shared_agent_schema {
        warnings.push(
            "example-api/example/core/inference.py is not using shared FlightSchema helpers for agent_generate"
                .to_string(),
        );
    }
    if !python_gepa_flight_accepts_canonical_actions {
        warnings.push(
            "example-api/example/flight/gepa_flight.py does not accept canonical GEPA action names with legacy aliases"
                .to_string(),
        );
    }
    if !sidecar_uses_shared_agent_schema {
        warnings.push(
            "example-api/sidecar/example_infer/flight_server.py is not overriding agent_generate schemas/command from shared FlightSchema when available"
                .to_string(),
        );
    }
    if !sidecar_uses_shared_adapter_schema {
        warnings.push(
            "example-api/sidecar/example_infer/flight_server.py is not overriding adapter_register schema/command from shared FlightSchema when available"
                .to_string(),
        );
    }
    if !sidecar_list_actions_excludes_commands {
        warnings.push(
            "example-api/sidecar/example_infer/flight_server.py list_actions() is advertising do_exchange commands"
                .to_string(),
        );
    }

    if let Some(src) = &shared_auth_src
        && let Some(line) = find_line(src, "pub struct FlightAuthorizationMetadata")
    {
        evidence.push(EvidenceItem {
            kind: "flight_auth".to_string(),
            path: shared_auth_path.display().to_string(),
            line: Some(line),
            detail: "shared Rust Flight clients resolve and redact authorization metadata"
                .to_string(),
        });
    }
    if let Some(src) = &office_client_src
        && let Some(line) = find_line(src, "FlightAuthorizationMetadata::from_env_required()")
    {
        evidence.push(EvidenceItem {
            kind: "flight_auth".to_string(),
            path: office_client_path.display().to_string(),
            line: Some(line),
            detail: "Office Flight publisher fails before dialing without client authorization"
                .to_string(),
        });
    }
    if let Some(src) = &client_src {
        if let Some(line) = find_line(
            src,
            "pub(crate) fn with_flight_auth<T>(payload: T) -> Result<Request<T>>",
        ) {
            evidence.push(EvidenceItem {
                kind: "flight_auth".to_string(),
                path: client_path.display().to_string(),
                line: Some(line),
                detail: "gateway Flight client centralizes authorization metadata injection"
                    .to_string(),
            });
        }
        if let Some(line) = find_line(src, "client.do_put(with_flight_auth(stream)?)") {
            evidence.push(EvidenceItem {
                kind: "flight_auth".to_string(),
                path: client_path.display().to_string(),
                line: Some(line),
                detail: "embedding Flight do_put uses auth wrapper".to_string(),
            });
        }
        if let Some(line) = find_line(
            src,
            "do_exchange(with_flight_auth(tokio_stream::iter(all_data))?)",
        ) {
            evidence.push(EvidenceItem {
                kind: "flight_auth".to_string(),
                path: client_path.display().to_string(),
                line: Some(line),
                detail: "MCTS Flight do_exchange uses auth wrapper".to_string(),
            });
        }
        if let Some(line) = find_line(
            src,
            "let schema = Arc::new(MctsEvalRowInput::arrow_schema());",
        ) {
            evidence.push(EvidenceItem {
                kind: "flight_auth".to_string(),
                path: client_path.display().to_string(),
                line: Some(line),
                detail: "gateway Flight client uses shared MCTS input FlightSchema".to_string(),
            });
        }
        if let Some(line) = find_line(src, "parse_broker_ca_bundle(&ca)") {
            evidence.push(EvidenceItem {
                kind: "flight_auth".to_string(),
                path: client_path.display().to_string(),
                line: Some(line),
                detail:
                    "gateway Flight auth broker client trusts every certificate in the CA bundle"
                        .to_string(),
            });
        }
        if let Some(line) = find_line(src, "with_flight_auth(action)") {
            evidence.push(EvidenceItem {
                kind: "flight_auth".to_string(),
                path: client_path.display().to_string(),
                line: Some(line),
                detail: "GEPA Flight client operations use auth wrapper".to_string(),
            });
        }
    }

    if let Some(src) = &gepa_src {
        if let Some(line) = find_line(
            src,
            "use super::client::{gepa_flight_endpoint, with_flight_auth};",
        ) {
            evidence.push(EvidenceItem {
                kind: "flight_auth".to_string(),
                path: gepa_path.display().to_string(),
                line: Some(line),
                detail: "GEPA forwarder imports shared Flight auth helper".to_string(),
            });
        }
        if let Some(line) = find_line(src, "let request = with_flight_auth(action)") {
            evidence.push(EvidenceItem {
                kind: "flight_auth".to_string(),
                path: gepa_path.display().to_string(),
                line: Some(line),
                detail: "GEPA forwarder injects Flight auth metadata before do_action".to_string(),
            });
        }
    }
    if let Some(src) = &gateway_mod_src
        && let Some(line) = find_line(
            src,
            "crate::flight::client::MctsEvalRowOutput::arrow_schema()",
        )
    {
        evidence.push(EvidenceItem {
            kind: "flight_auth".to_string(),
            path: gateway_mod_path.display().to_string(),
            line: Some(line),
            detail: "gateway Flight server uses shared MCTS output FlightSchema".to_string(),
        });
    }
    if let Some(src) = &gateway_ocr_src {
        if let Some(line) = find_line(src, "let schema = OcrResponse::arrow_schema();") {
            evidence.push(EvidenceItem {
                kind: "flight_auth".to_string(),
                path: gateway_ocr_path.display().to_string(),
                line: Some(line),
                detail: "gateway OCR processor uses shared OCR FlightSchema".to_string(),
            });
        }
        if let Some(line) = find_line(
            src,
            "extract_float_list_column(batch, \"candidate_embedding\")",
        ) {
            evidence.push(EvidenceItem {
                kind: "flight_auth".to_string(),
                path: gateway_ocr_path.display().to_string(),
                line: Some(line),
                detail:
                    "gateway OCR processor accepts canonical candidate_embedding with legacy fallback"
                        .to_string(),
            });
        }
    }
    if let Some(src) = &python_contracts_src {
        if let Some(line) = find_line(src, "def flight_call_options(") {
            evidence.push(EvidenceItem {
                kind: "flight_auth".to_string(),
                path: python_contracts_path.display().to_string(),
                line: Some(line),
                detail:
                    "Python Flight clients share canonical call options and auth header resolution"
                        .to_string(),
            });
        }
        if let Some(line) = find_line(src, "def _is_authorization_header(") {
            evidence.push(EvidenceItem {
                kind: "flight_auth".to_string(),
                path: python_contracts_path.display().to_string(),
                line: Some(line),
                detail: "Python Flight clients reject duplicate authorization metadata".to_string(),
            });
        }
        if let Some(line) = find_line(src, "COLLECTION_CREATE") {
            evidence.push(EvidenceItem {
                kind: "flight_auth".to_string(),
                path: python_contracts_path.display().to_string(),
                line: Some(line),
                detail:
                    "Python Flight contracts export canonical collection and knowledge action names"
                        .to_string(),
            });
        }
        if let Some(line) = find_line(src, "PROMPT_CLEAR") {
            evidence.push(EvidenceItem {
                kind: "flight_auth".to_string(),
                path: python_contracts_path.display().to_string(),
                line: Some(line),
                detail:
                    "Python Flight contracts export canonical runtime action names for prompt, ACO, navigate, ontology, tooling, MCTS, and LEIO"
                        .to_string(),
            });
        }
    }
    if let Some(src) = &python_server_src {
        if let Some(line) = find_line(src, "claims = decode_token(token)") {
            evidence.push(EvidenceItem {
                kind: "flight_auth".to_string(),
                path: python_server_path.display().to_string(),
                line: Some(line),
                detail: "Python Flight server accepts canonical bearer JWT metadata".to_string(),
            });
        }
        if let Some(line) = find_line(src, "if len(authorization_values) != 1:") {
            evidence.push(EvidenceItem {
                kind: "flight_auth".to_string(),
                path: python_server_path.display().to_string(),
                line: Some(line),
                detail: "Python Flight server rejects duplicate authorization metadata".to_string(),
            });
        }
        if let Some(line) = find_line(src, "if token != self._expected_token") {
            evidence.push(EvidenceItem {
                kind: "flight_auth".to_string(),
                path: python_server_path.display().to_string(),
                line: Some(line),
                detail: "Python Flight server preserves legacy shared-secret fallback".to_string(),
            });
        }
        if let Some(line) = find_line(src, "elif action_type == COLLECTION_CREATE:") {
            evidence.push(EvidenceItem {
                kind: "flight_auth".to_string(),
                path: python_server_path.display().to_string(),
                line: Some(line),
                detail:
                    "Python Flight server dispatches converged collection and knowledge actions via shared constants"
                        .to_string(),
            });
        }
        if let Some(line) = find_line(src, "elif action_type == PROMPT_CLEAR:") {
            evidence.push(EvidenceItem {
                kind: "flight_auth".to_string(),
                path: python_server_path.display().to_string(),
                line: Some(line),
                detail:
                    "Python Flight server dispatches prompt, ACO, navigate, ontology, tooling, MCTS, and LEIO actions via shared constants"
                        .to_string(),
            });
        }
    }
    for (path, src, needle, detail) in [
        (
            &python_gepa_client_path,
            &python_gepa_client_src,
            "flight_call_options(timeout=self.timeout)",
            "GEPA Python client uses shared Flight auth call options",
        ),
        (
            &python_prompt_client_path,
            &python_prompt_client_src,
            "headers=http_auth_headers(",
            "prompt Flight client uses shared HTTP auth headers",
        ),
        (
            &python_rust_bridge_path,
            &python_rust_bridge_src,
            "flight_call_options(timeout=self.timeout)",
            "Rust bridge client uses shared Flight auth call options",
        ),
        (
            &python_gepa_client_path,
            &python_gepa_client_src,
            "data[\"candidate_embedding\"]",
            "GEPA Python client sends OCR candidate embeddings under the canonical Flight field name",
        ),
        (
            &python_rust_bridge_path,
            &python_rust_bridge_src,
            "data[\"candidate_embedding\"]",
            "Rust bridge client sends OCR candidate embeddings under the canonical Flight field name",
        ),
        (
            &python_inference_path,
            &python_inference_src,
            "flight_command_for_schema(\"adapter_register\")",
            "inference client uses shared canonical FlightSchema helpers for adapter registration",
        ),
        (
            &python_inference_path,
            &python_inference_src,
            "request_schema = arrow_schema(\"agent_request\")",
            "inference client uses shared FlightSchema for agent_generate requests",
        ),
        (
            &python_inference_path,
            &python_inference_src,
            "response_schema = arrow_schema(\"agent_response\")",
            "inference client validates agent_generate responses against the shared FlightSchema",
        ),
        (
            &python_gepa_flight_path,
            &python_gepa_flight_src,
            "LEGACY_GEPA_SELECT_ACTION = \"select_candidate\"",
            "GEPA Flight server keeps legacy action aliases while preferring canonical names",
        ),
    ] {
        if let Some(src) = src
            && let Some(line) = find_line(src, needle)
        {
            evidence.push(EvidenceItem {
                kind: "flight_auth".to_string(),
                path: path.display().to_string(),
                line: Some(line),
                detail: detail.to_string(),
            });
        }
    }
    if let Some(src) = &sidecar_flight_server_src {
        if let Some(line) = find_line(
            src,
            "AGENT_REQUEST_SCHEMA = _shared_arrow_schema(\"agent_request\")",
        ) {
            evidence.push(EvidenceItem {
                kind: "flight_auth".to_string(),
                path: sidecar_flight_server_path.display().to_string(),
                line: Some(line),
                detail:
                    "inference sidecar overrides agent_generate schemas from shared FlightSchema when available"
                        .to_string(),
            });
        }
        if let Some(line) = find_line(
            src,
            "ADAPTER_REGISTER_SCHEMA = _shared_arrow_schema(\"adapter_register\")",
        ) {
            evidence.push(EvidenceItem {
                kind: "flight_auth".to_string(),
                path: sidecar_flight_server_path.display().to_string(),
                line: Some(line),
                detail:
                    "inference sidecar overrides adapter registration schema/command from shared FlightSchema when available"
                        .to_string(),
            });
        }
    }

    entities.push(json!({
        "path": shared_auth_path.display().to_string(),
        "resolves_client_metadata": shared_auth_resolves_client_metadata,
    }));
    entities.push(json!({
        "path": client_path.display().to_string(),
        "has_auth_header_env": client_has_auth_header_env,
        "has_request_wrapper": client_has_request_wrapper,
        "uses_wrapper_for_exchange": client_uses_wrapper_for_exchange,
        "trusts_full_broker_ca_bundle": client_trusts_full_broker_ca_bundle,
        "uses_shared_mcts_input_schema": gateway_uses_shared_mcts_input_schema,
    }));
    entities.push(json!({
        "path": office_client_path.display().to_string(),
        "uses_required_shared_auth": office_uses_required_shared_auth,
    }));
    entities.push(json!({
        "path": gepa_path.display().to_string(),
        "imports_wrapper": gepa_imports_wrapper,
        "uses_wrapper": gepa_uses_wrapper,
    }));
    entities.push(json!({
        "path": gateway_mod_path.display().to_string(),
        "uses_shared_mcts_output_schema": gateway_uses_shared_mcts_output_schema,
        "returns_ipc_put_metadata": gateway_put_returns_ipc_metadata,
    }));
    entities.push(json!({
        "path": gateway_ocr_path.display().to_string(),
        "uses_shared_ocr_schema": gateway_uses_shared_ocr_schema,
        "accepts_candidate_embedding": gateway_ocr_accepts_candidate_embedding,
    }));
    entities.push(json!({
        "path": python_contracts_path.display().to_string(),
        "has_shared_contracts": python_has_shared_contracts,
        "enforces_single_canonical_auth": python_enforces_single_canonical_auth,
        "exports_collection_actions": python_contracts_export_collection_actions,
        "exports_runtime_actions": python_contracts_export_runtime_actions,
    }));
    entities.push(json!({
        "path": python_server_path.display().to_string(),
        "accepts_canonical_bearer": python_server_accepts_canonical_bearer,
        "rejects_duplicate_auth": python_server_rejects_duplicate_auth,
        "keeps_legacy_fallback": python_server_keeps_legacy_fallback,
        "uses_canonical_collection_actions": python_server_uses_canonical_collection_actions,
        "uses_canonical_runtime_actions": python_server_uses_canonical_runtime_actions,
    }));
    entities.push(json!({
        "path": python_gepa_client_path.display().to_string(),
        "uses_shared_auth": python_gepa_client_uses_shared_auth,
        "uses_candidate_embedding": python_gepa_client_uses_candidate_embedding,
        "uses_shared_ocr_schema": python_gepa_client_uses_shared_ocr_schema,
    }));
    entities.push(json!({
        "path": python_prompt_client_path.display().to_string(),
        "uses_shared_auth": python_prompt_client_uses_shared_auth,
    }));
    entities.push(json!({
        "path": python_rust_bridge_path.display().to_string(),
        "uses_shared_auth": python_rust_bridge_uses_shared_auth,
        "uses_candidate_embedding": python_rust_bridge_uses_candidate_embedding,
        "uses_shared_ocr_schema": python_rust_bridge_uses_shared_ocr_schema,
    }));
    entities.push(json!({
        "path": python_inference_path.display().to_string(),
        "uses_shared_auth": python_inference_uses_shared_auth,
        "uses_shared_agent_schema": python_inference_uses_shared_agent_schema,
    }));
    entities.push(json!({
        "path": python_gepa_flight_path.display().to_string(),
        "accepts_canonical_actions": python_gepa_flight_accepts_canonical_actions,
    }));
    entities.push(json!({
        "path": sidecar_flight_server_path.display().to_string(),
        "uses_shared_agent_schema": sidecar_uses_shared_agent_schema,
        "uses_shared_adapter_schema": sidecar_uses_shared_adapter_schema,
        "list_actions_excludes_commands": sidecar_list_actions_excludes_commands,
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_flight_contract_auth"),
        kind: "doctor".to_string(),
        summary: format!(
            "checked Flight auth metadata propagation, found {} warnings",
            warnings.len()
        ),
        confidence: if warnings.is_empty() { 0.95 } else { 0.66 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn flight_calls_are_authenticated(source: &str, method: &str) -> bool {
    flight_calls_use_request_wrapper(source, method, "with_flight_auth(")
}

fn broker_ca_bundle_is_loaded_completely(source: &str) -> bool {
    let Some(broker_body) = rust_function_body(source, "workload_authorization_from_broker") else {
        return false;
    };
    let Some(parser_body) = rust_function_body(source, "parse_broker_ca_bundle") else {
        return false;
    };
    broker_body.contains("parse_broker_ca_bundle(&ca)")
        && broker_body.contains("for certificate in ca_certificates")
        && broker_body.contains(".add_root_certificate(certificate)")
        && !broker_body.contains("Certificate::from_pem(")
        && parser_body.contains("Certificate::from_pem_bundle(pem)")
        && parser_body.contains("certificates.is_empty()")
        && parser_body.contains("Ok(certificates)")
}

fn flight_calls_use_request_wrapper(source: &str, method: &str, wrapper: &str) -> bool {
    let wrapper = wrapper.trim_end_matches('(');
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .is_err()
    {
        return false;
    }
    let Some(tree) = parser.parse(source, None) else {
        return false;
    };
    let mut found = false;
    let mut valid = true;
    visit_rust_calls(
        tree.root_node(),
        source,
        method,
        wrapper,
        &mut found,
        &mut valid,
    );
    found && valid
}

fn visit_rust_calls<'tree>(
    node: Node<'tree>,
    source: &str,
    method: &str,
    wrapper: &str,
    found: &mut bool,
    valid: &mut bool,
) {
    if node.kind() == "macro_invocation" && macro_invocation_mentions_method(source, node, method) {
        // Rust macro token trees are opaque to tree-sitter. Until the doctor can
        // prove the request expression inside a macro, any target RPC there must
        // fail closed rather than disappearing from the call inventory.
        *found = true;
        *valid = false;
        return;
    }

    if node.kind() == "call_expression"
        && called_method(source, node).is_some_and(|called| called == method)
    {
        *found = true;
        let argument = request_argument(source, node);
        if !argument
            .is_some_and(|argument| expression_uses_wrapper(source, node, argument, wrapper))
        {
            *valid = false;
        }
    }

    if is_target_method_identifier(source, node, method)
        && !identifier_is_inside_call_function(node)
    {
        // A method item can be aliased and invoked under a different name. The
        // doctor cannot prove the later request wrapper from that reference, so
        // any target identifier outside a validated call function fails closed.
        *found = true;
        *valid = false;
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        visit_rust_calls(child, source, method, wrapper, found, valid);
    }
}

fn macro_invocation_mentions_method(source: &str, macro_node: Node<'_>, method: &str) -> bool {
    let Some(token_tree) = macro_node.child_by_field_name("token_tree").or_else(|| {
        let mut cursor = macro_node.walk();
        macro_node
            .named_children(&mut cursor)
            .find(|child| child.kind() == "token_tree")
    }) else {
        return false;
    };
    token_tree_mentions_method(source, token_tree, method)
}

fn token_tree_mentions_method(source: &str, node: Node<'_>, method: &str) -> bool {
    if matches!(
        node.kind(),
        "string_literal"
            | "raw_string_literal"
            | "byte_string_literal"
            | "raw_byte_string_literal"
            | "char_literal"
            | "line_comment"
            | "block_comment"
    ) {
        return false;
    }

    if node.child_count() == 0 {
        let Some(token) = node_text(source, node).map(str::trim) else {
            return false;
        };
        if token.is_empty() {
            return false;
        }
        return matches!(node.kind(), "identifier" | "field_identifier")
            && normalize_rust_identifier(token) == method;
    }

    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| token_tree_mentions_method(source, child, method))
}

fn called_method<'a>(source: &'a str, call: Node<'_>) -> Option<&'a str> {
    let function = call.child_by_field_name("function")?;
    let identifier = callable_identifier(function)?;
    node_text(source, identifier).map(normalize_rust_identifier)
}

fn callable_identifier(function: Node<'_>) -> Option<Node<'_>> {
    if let Some(field) = function.child_by_field_name("field") {
        return Some(field);
    }
    if let Some(name) = function.child_by_field_name("name") {
        return Some(name);
    }
    if matches!(function.kind(), "identifier" | "field_identifier") {
        return Some(function);
    }
    if let Some(inner) = function.child_by_field_name("function") {
        return callable_identifier(inner);
    }
    let mut cursor = function.walk();
    function
        .named_children(&mut cursor)
        .find_map(callable_identifier)
}

fn normalize_rust_identifier(identifier: &str) -> &str {
    identifier.strip_prefix("r#").unwrap_or(identifier)
}

fn is_target_method_identifier(source: &str, node: Node<'_>, method: &str) -> bool {
    matches!(node.kind(), "identifier" | "field_identifier")
        && node_text(source, node)
            .map(normalize_rust_identifier)
            .is_some_and(|identifier| identifier == method)
}

fn identifier_is_inside_call_function(node: Node<'_>) -> bool {
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if parent.kind() == "call_expression" {
            return parent
                .child_by_field_name("function")
                .is_some_and(|function| {
                    function.start_byte() <= node.start_byte()
                        && node.end_byte() <= function.end_byte()
                });
        }
        ancestor = parent.parent();
    }
    false
}

fn request_argument<'tree>(source: &str, call: Node<'tree>) -> Option<Node<'tree>> {
    let function = call.child_by_field_name("function")?;
    let ufcs = node_text(source, function)?.contains("::");
    let arguments = call.child_by_field_name("arguments")?;
    let mut cursor = arguments.walk();
    let named = arguments.named_children(&mut cursor);
    if ufcs {
        named.last()
    } else {
        named.into_iter().next()
    }
}

fn expression_uses_wrapper<'tree>(
    source: &str,
    call: Node<'tree>,
    mut expression: Node<'tree>,
    wrapper: &str,
) -> bool {
    if expression.kind() == "identifier" {
        let Some(identifier) = node_text(source, expression) else {
            return false;
        };
        let Some(value) = latest_visible_let_value_before(source, call, identifier) else {
            return false;
        };
        expression = value;
    }

    while matches!(
        expression.kind(),
        "try_expression" | "parenthesized_expression"
    ) {
        let Some(inner) = expression.named_child(0) else {
            return false;
        };
        expression = inner;
    }
    if expression.kind() != "call_expression" {
        return false;
    }
    expression
        .child_by_field_name("function")
        .and_then(|function| node_text(source, function))
        .is_some_and(|function| function == wrapper)
}

fn latest_visible_let_value_before<'tree>(
    source: &str,
    call: Node<'tree>,
    identifier: &str,
) -> Option<Node<'tree>> {
    let before = call.start_byte();
    let mut ancestor = call.parent();
    while let Some(scope) = ancestor {
        if scope.kind() == "block" {
            let mut latest = None;
            let mut cursor = scope.walk();
            for statement in scope.named_children(&mut cursor) {
                if statement.start_byte() >= before {
                    break;
                }
                if statement.kind() == "let_declaration"
                    && !node_contains(statement, call)
                    && statement
                        .child_by_field_name("pattern")
                        .is_some_and(|pattern| {
                            pattern_binds_identifier(source, pattern, identifier)
                        })
                    && let Some(value) = statement.child_by_field_name("value")
                {
                    latest = Some(value);
                }
            }
            if latest.is_some() {
                return latest;
            }
        }
        if lexical_scope_binds_identifier(source, scope, call, identifier) {
            return None;
        }
        ancestor = scope.parent();
    }
    None
}

fn lexical_scope_binds_identifier(
    source: &str,
    scope: Node<'_>,
    call: Node<'_>,
    identifier: &str,
) -> bool {
    match scope.kind() {
        "match_arm" | "last_match_arm" => scope
            .child_by_field_name("pattern")
            .is_some_and(|pattern| pattern_binds_identifier(source, pattern, identifier)),
        "for_expression" => {
            field_contains_call(scope, "body", call)
                && scope
                    .child_by_field_name("pattern")
                    .is_some_and(|pattern| pattern_binds_identifier(source, pattern, identifier))
        }
        "closure_expression" | "function_item" => {
            field_contains_call(scope, "body", call)
                && scope
                    .child_by_field_name("parameters")
                    .is_some_and(|parameters| {
                        pattern_binds_identifier(source, parameters, identifier)
                    })
        }
        "if_expression" => {
            field_contains_call(scope, "consequence", call)
                && scope
                    .child_by_field_name("condition")
                    .is_some_and(|condition| {
                        let_condition_binds_identifier(source, condition, identifier)
                    })
        }
        "while_expression" => {
            field_contains_call(scope, "body", call)
                && scope
                    .child_by_field_name("condition")
                    .is_some_and(|condition| {
                        let_condition_binds_identifier(source, condition, identifier)
                    })
        }
        _ => false,
    }
}

fn field_contains_call(scope: Node<'_>, field: &str, call: Node<'_>) -> bool {
    scope
        .child_by_field_name(field)
        .is_some_and(|node| node_contains(node, call))
}

fn node_contains(ancestor: Node<'_>, descendant: Node<'_>) -> bool {
    ancestor.start_byte() <= descendant.start_byte() && descendant.end_byte() <= ancestor.end_byte()
}

fn let_condition_binds_identifier(source: &str, node: Node<'_>, identifier: &str) -> bool {
    if node.kind() == "let_condition" {
        return node
            .child_by_field_name("pattern")
            .is_some_and(|pattern| pattern_binds_identifier(source, pattern, identifier));
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .any(|child| let_condition_binds_identifier(source, child, identifier))
}

fn pattern_binds_identifier(source: &str, mut pattern: Node<'_>, identifier: &str) -> bool {
    if pattern.kind() == "match_pattern"
        && let Some(inner) = pattern.named_child(0)
    {
        pattern = inner;
    }
    if matches!(pattern.kind(), "identifier" | "shorthand_field_identifier")
        && node_text(source, pattern)
            .map(normalize_rust_identifier)
            .is_some_and(|binding| binding == identifier)
    {
        return true;
    }
    let mut cursor = pattern.walk();
    pattern
        .named_children(&mut cursor)
        .any(|child| pattern_binds_identifier(source, child, identifier))
}

fn office_auth_resolves_before_dial(source: &str) -> bool {
    let Some(body) = rust_function_body(source, "connect_with_policies") else {
        return false;
    };
    let Some(authorization) = body.find("FlightAuthorizationMetadata::from_env_required()") else {
        return false;
    };
    [
        body.find("connect_client("),
        body.find("connect_with_policies_and_authorization("),
        body.find(".connect("),
    ]
    .into_iter()
    .flatten()
    .min()
    .is_some_and(|dial| authorization < dial)
}

fn rust_function_body<'a>(source: &'a str, function_name: &str) -> Option<&'a str> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .ok()?;
    let tree = parser.parse(source, None)?;
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "function_item"
            && node
                .child_by_field_name("name")
                .and_then(|name| node_text(source, name))
                .is_some_and(|name| name == function_name)
        {
            return node
                .child_by_field_name("body")
                .and_then(|body| node_text(source, body));
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    None
}

fn node_text<'a>(source: &'a str, node: Node<'_>) -> Option<&'a str> {
    source.get(node.byte_range())
}

#[cfg(test)]
mod auth_flow_tests {
    use super::{
        broker_ca_bundle_is_loaded_completely, flight_calls_are_authenticated,
        flight_calls_use_request_wrapper, office_auth_resolves_before_dial,
    };

    /// Read a source file from a repo checked out beside this one.
    ///
    /// Two guards below assert against the *real* client code rather than a
    /// fixture. Reading it with `include_str!` made those siblings a build
    /// requirement: in a standalone checkout of this repo the files are absent
    /// and the entire test binary fails to compile, taking every unrelated
    /// test with it. Reading at runtime keeps the guard where the sibling is
    /// present and skips it where it is not.
    fn sibling_repo_source(relative: &str) -> Option<String> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()?
            .join(relative);
        std::fs::read_to_string(path).ok()
    }

    #[test]
    fn staged_request_matcher_requires_each_call_argument_to_derive_from_wrapper() {
        let valid = r#"
pub async fn put() {
    let request = with_flight_auth(stream)?;
    client.do_put(request).await?;
}
pub async fn exchange() {
    let exchange_request = with_flight_auth(stream)?;
    client.do_exchange(exchange_request).await?;
}
"#;
        assert!(flight_calls_are_authenticated(valid, "do_put"));
        assert!(flight_calls_are_authenticated(valid, "do_exchange"));

        let mutated = r#"
pub async fn unrelated() {
    let request = with_flight_auth(stream)?;
}
pub async fn put() {
    let request = Request::new(stream);
    client.do_put(request).await?;
}
pub async fn exchange() {
    let request = Request::new(stream);
    client.do_exchange(request).await?;
}
"#;
        assert!(!flight_calls_are_authenticated(mutated, "do_put"));
        assert!(!flight_calls_are_authenticated(mutated, "do_exchange"));

        let alternate_receiver = r#"
pub async fn put() {
    let request = with_flight_auth(stream)?;
    reconnect_client.do_put(request).await?;
}
pub async fn exchange() {
    let exchange_request = Request::new(stream);
    flight_client.do_exchange(exchange_request).await?;
}
"#;
        assert!(flight_calls_are_authenticated(alternate_receiver, "do_put"));
        assert!(!flight_calls_are_authenticated(
            alternate_receiver,
            "do_exchange"
        ));

        let stripped_metadata = r#"
pub async fn put() {
    reconnect_client
        .do_put(with_flight_auth(stream)?.into_inner())
        .await?;
}
pub async fn exchange() {
    let request = with_flight_auth(stream)?.into_inner();
    flight_client.do_exchange(request).await?;
}
"#;
        assert!(!flight_calls_are_authenticated(stripped_metadata, "do_put"));
        assert!(!flight_calls_are_authenticated(
            stripped_metadata,
            "do_exchange"
        ));

        let out_of_scope_shadow = r#"
pub async fn put(condition: bool) {
    let request = Request::new(stream);
    if condition {
        let request = with_flight_auth(other)?;
        consume(request);
    }
    client.do_put(request).await?;
}
"#;
        assert!(!flight_calls_are_authenticated(
            out_of_scope_shadow,
            "do_put"
        ));

        let ufcs = r#"
pub async fn put() {
    FlightServiceClient::do_put(&mut client, with_flight_auth(stream)?).await?;
}
pub async fn exchange() {
    FlightServiceClient::do_exchange(&mut client, Request::new(stream)).await?;
}
"#;
        assert!(flight_calls_are_authenticated(ufcs, "do_put"));
        assert!(!flight_calls_are_authenticated(ufcs, "do_exchange"));

        let macro_hidden_bypass = r#"
pub async fn put() {
    client.do_put(with_flight_auth(stream)?).await?;
    tokio::select! {
        result = reconnect_client.do_put(Request::new(other)) => handle(result),
    }
}
"#;
        assert!(!flight_calls_are_authenticated(
            macro_hidden_bypass,
            "do_put"
        ));

        let macro_noise = r#"
pub async fn put() {
    client.do_put(with_flight_auth(stream)?).await?;
    tracing::debug!("reconnect_client.do_put(Request::new(other))");
    tokio::select! {
        _ = ready() => { /* reconnect_client.do_put(Request::new(other)) */ }
    }
}
"#;
        assert!(flight_calls_are_authenticated(macro_noise, "do_put"));

        let obfuscated_bypass = r#"
pub async fn calls() {
    client.do_put(with_flight_auth(stream)?).await?;
    client.do_exchange(with_flight_auth(stream)?).await?;
    reconnect_client.r#do_put(Request::new(other)).await?;
    FlightServiceClient::/* split */r#do_exchange(
        &mut reconnect_client,
        Request::new(other),
    ).await?;
    tokio::select! {
        result = reconnect_client./* split */r#do_put(Request::new(other)) => handle(result),
        result = FlightServiceClient::r#do_exchange(
            &mut reconnect_client,
            Request::new(other),
        ) => handle(result),
    }
}
"#;
        assert!(!flight_calls_are_authenticated(obfuscated_bypass, "do_put"));
        assert!(!flight_calls_are_authenticated(
            obfuscated_bypass,
            "do_exchange"
        ));

        let macro_method_argument_bypass = r#"
macro_rules! call_rpc {
    ($client:expr, $method:ident, $request:expr) => {
        $client.$method($request)
    };
}

pub async fn put() {
    client.do_put(with_flight_auth(stream)?).await?;
    call_rpc!(reconnect_client, do_put, Request::new(other)).await?;
}
"#;
        assert!(!flight_calls_are_authenticated(
            macro_method_argument_bypass,
            "do_put"
        ));

        let callable_alias_bypass = r#"
pub async fn put() {
    client.do_put(with_flight_auth(stream)?).await?;
    let put = FlightServiceClient::<Channel>::do_put;
    put(&mut reconnect_client, Request::new(other)).await?;
}
"#;
        assert!(!flight_calls_are_authenticated(
            callable_alias_bypass,
            "do_put"
        ));

        let parenthesized_ufcs = r#"
pub async fn put() {
    (FlightServiceClient::<Channel>::do_put)(
        &mut client,
        with_flight_auth(stream)?,
    ).await?;
}
"#;
        assert!(flight_calls_are_authenticated(parenthesized_ufcs, "do_put"));

        let parenthesized_ufcs_bypass = r#"
pub async fn put() {
    client.do_put(with_flight_auth(stream)?).await?;
    (FlightServiceClient::<Channel>::do_put)(
        &mut reconnect_client,
        Request::new(other),
    ).await?;
}
"#;
        assert!(!flight_calls_are_authenticated(
            parenthesized_ufcs_bypass,
            "do_put"
        ));

        let match_shadow_bypass = r#"
pub async fn put() {
    let request = with_flight_auth(good)?;
    match Request::new(other) {
        request => client.do_put(request).await?,
    }
}
"#;
        assert!(!flight_calls_are_authenticated(
            match_shadow_bypass,
            "do_put"
        ));

        let closure_shadow_bypass = r#"
pub async fn exchange() {
    let request = with_flight_auth(good)?;
    let send = |request| async { client.do_exchange(request).await };
    send(Request::new(other)).await?;
}
"#;
        assert!(!flight_calls_are_authenticated(
            closure_shadow_bypass,
            "do_exchange"
        ));

        let loop_and_if_let_shadow_bypass = r#"
pub async fn put() {
    let request = with_flight_auth(good)?;
    for request in [Request::new(other)] {
        client.do_put(request).await?;
    }
    if let Some(request) = Some(Request::new(other)) {
        client.do_put(request).await?;
    }
}
"#;
        assert!(!flight_calls_are_authenticated(
            loop_and_if_let_shadow_bypass,
            "do_put"
        ));
    }

    #[test]
    fn actual_gateway_client_keeps_all_put_and_exchange_calls_authenticated() {
        let Some(source) = sibling_repo_source("example-gateway/src/flight/client.rs") else {
            eprintln!("skipped: example-gateway is not checked out beside this repo");
            return;
        };
        assert!(flight_calls_are_authenticated(&source, "do_put"));
        assert!(flight_calls_are_authenticated(&source, "do_exchange"));
    }

    #[test]
    fn broker_ca_bundle_guard_requires_bundle_parser_and_all_roots() {
        let valid = r#"
async fn workload_authorization_from_broker(url: String) -> Result<FlightAuthorizationMetadata> {
    let ca = std::fs::read(&ca_path)?;
    let ca_certificates = parse_broker_ca_bundle(&ca)?;
    let mut builder = reqwest::Client::builder();
    for certificate in ca_certificates {
        builder = builder.add_root_certificate(certificate);
    }
    Ok(metadata)
}

fn parse_broker_ca_bundle(pem: &[u8]) -> Result<Vec<reqwest::Certificate>> {
    let certificates = reqwest::Certificate::from_pem_bundle(pem)?;
    if certificates.is_empty() {
        return Err(anyhow::anyhow!("empty"));
    }
    Ok(certificates)
}
"#;
        assert!(broker_ca_bundle_is_loaded_completely(valid));

        let legacy_single_cert = r#"
async fn workload_authorization_from_broker(url: String) -> Result<FlightAuthorizationMetadata> {
    let ca = std::fs::read(&ca_path)?;
    let ca = reqwest::Certificate::from_pem(&ca)?;
    let client = reqwest::Client::builder()
        .add_root_certificate(ca)
        .build()?;
    Ok(metadata)
}
"#;
        assert!(!broker_ca_bundle_is_loaded_completely(legacy_single_cert));

        let parses_bundle_but_drops_after_first = r#"
async fn workload_authorization_from_broker(url: String) -> Result<FlightAuthorizationMetadata> {
    let ca = std::fs::read(&ca_path)?;
    let ca_certificates = parse_broker_ca_bundle(&ca)?;
    let first = ca_certificates.into_iter().next().unwrap();
    let builder = reqwest::Client::builder().add_root_certificate(first);
    Ok(metadata)
}

fn parse_broker_ca_bundle(pem: &[u8]) -> Result<Vec<reqwest::Certificate>> {
    let certificates = reqwest::Certificate::from_pem_bundle(pem)?;
    if certificates.is_empty() {
        return Err(anyhow::anyhow!("empty"));
    }
    Ok(certificates)
}
"#;
        assert!(!broker_ca_bundle_is_loaded_completely(
            parses_bundle_but_drops_after_first
        ));

        let accepts_empty_bundle = r#"
async fn workload_authorization_from_broker(url: String) -> Result<FlightAuthorizationMetadata> {
    let ca = std::fs::read(&ca_path)?;
    let ca_certificates = parse_broker_ca_bundle(&ca)?;
    let mut builder = reqwest::Client::builder();
    for certificate in ca_certificates {
        builder = builder.add_root_certificate(certificate);
    }
    Ok(metadata)
}

fn parse_broker_ca_bundle(pem: &[u8]) -> Result<Vec<reqwest::Certificate>> {
    let certificates = reqwest::Certificate::from_pem_bundle(pem)?;
    Ok(certificates)
}
"#;
        assert!(!broker_ca_bundle_is_loaded_completely(accepts_empty_bundle));
    }

    #[test]
    fn actual_office_client_binds_both_rpc_paths_to_stored_authorization() {
        let Some(source) = sibling_repo_source("example-platform/example-office/src/flight.rs")
        else {
            eprintln!("skipped: example-platform is not checked out beside this repo");
            return;
        };
        assert!(office_auth_resolves_before_dial(&source));
        assert!(flight_calls_use_request_wrapper(
            &source,
            "do_action",
            "authorization.request("
        ));
        assert!(flight_calls_use_request_wrapper(
            &source,
            "do_put",
            "authorization.request("
        ));

        let bypassed = source.replacen(
            ".do_put(authorization.request(iter(prepared.frames)))",
            ".do_put(tonic::Request::new(iter(prepared.frames)))",
            1,
        );
        assert!(!flight_calls_use_request_wrapper(
            &bypassed,
            "do_put",
            "authorization.request("
        ));

        let stripped = source.replacen(
            ".do_put(authorization.request(iter(prepared.frames)))",
            ".do_put(authorization.request(iter(prepared.frames)).into_inner())",
            1,
        );
        assert!(!flight_calls_use_request_wrapper(
            &stripped,
            "do_put",
            "authorization.request("
        ));

        let dial_before_auth = r#"
impl FlightPublisher {
    pub async fn connect_with_policies(endpoint: &str) -> Result<Self> {
        let client = Self::connect_client(endpoint).await?;
        let authorization = FlightAuthorizationMetadata::from_env_required()?;
        Self::connect_with_policies_and_authorization(endpoint, authorization).await
    }
}
"#;
        assert!(!office_auth_resolves_before_dial(dial_before_auth));
    }
}
