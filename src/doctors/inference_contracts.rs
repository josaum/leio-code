use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct InferenceContractsDoctor;

impl Doctor for InferenceContractsDoctor {
    fn name(&self) -> &'static str {
        "inference-contracts"
    }

    fn description(&self) -> &'static str {
        "Checks inference contracts that are easy to break silently, including GLiNER word-level ONNX preprocessing and Flight tensor hot paths."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_inference_contracts(index, root)
    }
}

pub fn doctor_inference_contracts(_index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let ort_path = root.join("office-parsers-rs/gliner-core/src/ort_inference.rs");
    let py_path = root.join("office-parsers-rs/gliner-fast-py/src/lib.rs");
    let infer_client_path = root.join("example-api/example/core/inference.py");
    let infer_sidecar_path = root.join("example-api/sidecar/example_infer/flight_server.py");
    let infer_vllm_path = root.join("example-api/sidecar/example_infer/backends/vllm_backend.py");
    let infer_torch_path = root.join("example-api/sidecar/example_infer/backends/torch_backend.py");
    let infer_mlx_path = root.join("example-api/sidecar/example_infer/backends/mlx_backend.py");
    let infer_openai_path = root.join("example-api/sidecar/example_infer/openai_compat.py");

    let ort_src = read_text(&ort_path, &mut warnings);
    let py_src = read_text(&py_path, &mut warnings);
    let infer_client_src = read_text(&infer_client_path, &mut warnings);
    let infer_sidecar_src = read_text(&infer_sidecar_path, &mut warnings);
    let infer_vllm_src = read_text(&infer_vllm_path, &mut warnings);
    let infer_torch_src = read_text(&infer_torch_path, &mut warnings);
    let infer_mlx_src = read_text(&infer_mlx_path, &mut warnings);
    let infer_openai_src = read_text(&infer_openai_path, &mut warnings);

    if let Some(src) = ort_src.as_deref() {
        for (needle, detail) in [
            (
                r#"Regex::new(r"\w+(?:[-_]\w+)*|\S")"#,
                "GLiNER text splitting matches the upstream whitespace regex contract",
            ),
            (
                "fn split_text_words(text: &str, splitter_type: Option<&str>)",
                "GLiNER text is split into words before tokenizer subword encoding",
            ),
            (
                "fn build_word_mask_from_word_ids(",
                "GLiNER words_mask is reconstructed from tokenizer word_ids instead of token offsets",
            ),
            (
                "input_words.push(self.ent_token.clone());",
                "GLiNER prompt uses explicit ENT markers before text words",
            ),
            (
                "input_words.push(self.sep_token.clone());",
                "GLiNER prompt inserts SEP before the text payload",
            ),
            (
                ".encode(input_words, true)",
                "GLiNER prompt and text are encoded as one pretokenized word sequence",
            ),
            (
                "encoding.get_word_ids()",
                "GLiNER uses tokenizer word_ids to derive word-space masking",
            ),
            (
                "fn whitespace_splitter_matches_upstream_contract()",
                "GLiNER whitespace splitter has regression coverage",
            ),
            (
                "fn word_mask_skips_prompt_and_keeps_first_subword_only()",
                "GLiNER word_ids masking has regression coverage",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "code".to_string(),
                    path: ort_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            } else {
                warnings.push(format!(
                    "GLiNER inference contract missing invariant in {}: {needle}",
                    ort_path.display()
                ));
            }
        }

        for needle in [
            "fn build_token_level_word_mapping(",
            "special_ids = [self.class_token_id, self.sep_token_id, self.pad_token_id]",
        ] {
            if let Some(line) = find_line(src, needle) {
                warnings.push(format!(
                    "GLiNER token-level preprocessing fallback resurfaced in {}:{} via `{needle}`",
                    ort_path.display(),
                    line
                ));
            }
        }
    }

    if let Some(src) = py_src.as_deref() {
        for (needle, detail) in [
            (
                "predict_entities(&text, &label_refs, threshold)",
                "Python bindings delegate entity extraction directly to the shared Rust inference core",
            ),
            (
                "fn extract_entities_json(",
                "one-shot Python helper still routes through the same Rust inference core",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "code".to_string(),
                    path: py_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            } else {
                warnings.push(format!(
                    "GLiNER Python binding invariant missing in {}: {needle}",
                    py_path.display()
                ));
            }
        }
    }

    if let Some(src) = infer_client_src.as_deref() {
        for (needle, detail) in [
            (
                "from example.flight.tensors import (",
                "Inference client imports the shared Arrow tensor helpers",
            ),
            (
                "\"adapter_embedding_fp16\": adapter_values,",
                "Inference client sends inline adapters through the canonical fp16 Flight field",
            ),
            (
                "_flight_do_get_table(EMBED, payload)",
                "Inference client resolves embed() through Arrow do_get instead of JSON actions",
            ),
            (
                "\"last_hidden_state_fp32\"",
                "Inference client consumes the canonical last-hidden-state response field",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "code".to_string(),
                    path: infer_client_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            } else {
                warnings.push(format!(
                    "Flight tensor invariant missing in {}: {needle}",
                    infer_client_path.display()
                ));
            }
        }

        for needle in [
            "adapter_embedding.flatten().tolist()",
            "response_table.column(\"last_hidden_state\")[row_idx].as_py()",
            "raw = self._flight_action_json(EMBED, payload)",
            "batch.to_pylist()",
            "response_table.column(\"text\").to_pylist()",
        ] {
            if let Some(line) = find_line(src, needle) {
                warnings.push(format!(
                    "legacy Flight tensor anti-pattern resurfaced in {}:{} via `{needle}`",
                    infer_client_path.display(),
                    line
                ));
            }
        }
    }

    if let Some(src) = infer_sidecar_src.as_deref() {
        for (needle, detail) in [
            (
                "EMBED_SCHEMA = _shared_arrow_schema(\"embed_response\")",
                "Sidecar consumes the shared embed_response schema",
            ),
            (
                "request_table.column(\"adapter_embedding_fp16\")",
                "Sidecar decodes inline adapters from the canonical fp16 Flight field",
            ),
            (
                "tensor_row_to_numpy(table.column(\"embedding_fp16\"), dtype=np.float16)",
                "Sidecar reconstructs registered adapters from Arrow buffers without Python list materialisation",
            ),
            (
                "\"embeddings_fp32\": tensor_list_array_from_numpy(",
                "Sidecar emits flattened embedding batches through the canonical Arrow tensor field",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "code".to_string(),
                    path: infer_sidecar_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            } else {
                warnings.push(format!(
                    "Flight tensor invariant missing in {}: {needle}",
                    infer_sidecar_path.display()
                ));
            }
        }

        for needle in [
            "table.column(\"embedding\").to_pylist()",
            "request_table.column(\"adapter_embedding\")[0].as_py()",
            "vectors = [[float(x) for x in row] for row in result.embeddings.tolist()]",
            "def _action_embed(",
        ] {
            if let Some(line) = find_line(src, needle) {
                warnings.push(format!(
                    "legacy Flight tensor anti-pattern resurfaced in {}:{} via `{needle}`",
                    infer_sidecar_path.display(),
                    line
                ));
            }
        }
    }

    if let Some(src) = infer_vllm_src.as_deref() {
        for (needle, detail) in [
            (
                "def _extract_last_hidden_from_token_ids(",
                "vLLM backend centralises last-token hidden-state extraction for shared generate paths",
            ),
            (
                "\"last_hidden_state\": last_hidden,",
                "vLLM streaming generation surfaces the final last-hidden-state on the terminal chunk",
            ),
            (
                "def _manual_adapter_rows(",
                "vLLM manual embedding path reuses the canonical adapter buffer before unavoidable promotion",
            ),
            (
                "def _normalize_embedding_vector(",
                "vLLM embedding normalization is centralised in one helper across native and manual paths",
            ),
            (
                "def _embedding_output_buffer(",
                "vLLM preallocates embedding output matrices instead of stacking Python lists",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "code".to_string(),
                    path: infer_vllm_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            } else {
                warnings.push(format!(
                    "Flight tensor invariant missing in {}: {needle}",
                    infer_vllm_path.display()
                ));
            }
        }

        for needle in [
            "np.asarray(adapter.embedding, dtype=np.float32)",
            "rows[0].astype(np.float32)",
            "np.array(final.outputs[0].embedding, dtype=np.float32)",
            "np.stack(all_embeddings)",
        ] {
            if let Some(line) = find_line(src, needle) {
                warnings.push(format!(
                    "legacy vLLM embedding copy anti-pattern resurfaced in {}:{} via `{needle}`",
                    infer_vllm_path.display(),
                    line
                ));
            }
        }
    }

    if let Some(src) = infer_torch_src.as_deref() {
        for (needle, detail) in [
            (
                "def _adapter_tensor_for_inputs(",
                "Torch backend centralises adapter numpy->torch bridging in one helper",
            ),
            (
                "return torch.from_numpy(adapter.embedding).to(",
                "Torch backend reuses the canonical fp16 adapter buffer before the unavoidable device cast",
            ),
            (
                "def _embedding_output_buffer(",
                "Torch backend preallocates embedding output matrices instead of stacking Python lists",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "code".to_string(),
                    path: infer_torch_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            } else {
                warnings.push(format!(
                    "Torch adapter hot-path invariant missing in {}: {needle}",
                    infer_torch_path.display()
                ));
            }
        }

        for needle in [
            "np.ascontiguousarray(adapter.embedding)",
            "adapter.embedding.astype(np.float32)",
            "np.stack(all_embeddings)",
        ] {
            if let Some(line) = find_line(src, needle) {
                warnings.push(format!(
                    "legacy Torch adapter copy anti-pattern resurfaced in {}:{} via `{needle}`",
                    infer_torch_path.display(),
                    line
                ));
            }
        }
    }

    if let Some(src) = infer_mlx_src.as_deref() {
        for (needle, detail) in [
            (
                "def _adapter_embeds_for_inputs(",
                "MLX backend centralises adapter numpy->mlx bridging in one helper",
            ),
            (
                "def _normalize_embedding_vector(",
                "MLX embedding normalization is centralised in one helper",
            ),
            (
                "def _embedding_output_buffer(",
                "MLX backend preallocates embedding output matrices instead of stacking Python lists",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "code".to_string(),
                    path: infer_mlx_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            } else {
                warnings.push(format!(
                    "MLX embedding invariant missing in {}: {needle}",
                    infer_mlx_path.display()
                ));
            }
        }

        for needle in [
            "np.array(emb, dtype=np.float32)",
            "np.stack(all_embeddings)",
        ] {
            if let Some(line) = find_line(src, needle) {
                warnings.push(format!(
                    "legacy MLX embedding copy anti-pattern resurfaced in {}:{} via `{needle}`",
                    infer_mlx_path.display(),
                    line
                ));
            }
        }
    }

    if let Some(src) = infer_openai_src.as_deref() {
        for (needle, detail) in [
            (
                "def _json_float_vector(",
                "OpenAI-compatible HTTP edge centralises vector-to-JSON tensor materialisation in one helper",
            ),
            (
                "def _json_float_matrix_rows(",
                "OpenAI-compatible HTTP edge centralises batched embedding JSON materialisation in one helper",
            ),
            (
                "def _response_output_text_content_item(",
                "Responses output_text items are centralised in one shared builder",
            ),
            (
                "def _response_message_output_item(",
                "Responses message items are centralised in one shared builder",
            ),
            (
                "def _response_function_call_output_item(",
                "Responses function_call items are centralised in one shared builder",
            ),
            (
                "last_hidden_state=result.last_hidden_state,",
                "Non-streaming Responses API passes last-hidden-state through the shared HTTP edge envelope",
            ),
            (
                "embedding_rows = _json_float_matrix_rows(result.embeddings)",
                "Embeddings API materialises the batch once at the HTTP JSON edge before shaping response rows",
            ),
            (
                "def _build_response_envelope(",
                "Responses API uses one shared envelope builder for both streaming and non-streaming HTTP paths",
            ),
            (
                "usage=_responses_usage(completion_tokens, last_hidden_state)",
                "Streaming and non-streaming Responses API paths share the same usage/tensor serialization contract",
            ),
            (
                "def _chat_completion_chunk_payload(",
                "Chat Completions streaming uses one shared chunk payload builder at the HTTP edge",
            ),
            (
                "def _text_completion_chunk_payload(",
                "Text Completions streaming uses one shared chunk payload builder at the HTTP edge",
            ),
            (
                "def _response_created_event_payload(",
                "Responses streaming uses one shared builder for response.created at the HTTP edge",
            ),
            (
                "def _response_output_text_delta_event_payload(",
                "Responses streaming uses one shared builder for response.output_text.delta at the HTTP edge",
            ),
            (
                "def _response_output_text_done_event_payload(",
                "Responses streaming uses one shared builder for response.output_text.done at the HTTP edge",
            ),
            (
                "def _response_completed_event_payload(",
                "Responses streaming uses one shared builder for response.completed at the HTTP edge",
            ),
        ] {
            if let Some(line) = find_line(src, needle) {
                evidence.push(EvidenceItem {
                    kind: "code".to_string(),
                    path: infer_openai_path.display().to_string(),
                    line: Some(line),
                    detail: detail.to_string(),
                });
            } else {
                warnings.push(format!(
                    "HTTP tensor edge invariant missing in {}: {needle}",
                    infer_openai_path.display()
                ));
            }
        }

        for needle in [
            "result.last_hidden_state.tolist()",
            "result.embeddings[i].tolist()",
            "completion_tokens += 1",
        ] {
            if let Some(line) = find_line(src, needle) {
                warnings.push(format!(
                    "legacy HTTP tensor edge anti-pattern resurfaced in {}:{} via `{needle}`",
                    infer_openai_path.display(),
                    line
                ));
            }
        }

        for object_name in ["chat.completion.chunk", "text_completion"] {
            let needle = format!("\"object\": \"{object_name}\"");
            let count = src.matches(&needle).count();
            if count > 1 {
                warnings.push(format!(
                    "HTTP streaming chunk payload drift resurfaced in {}: `{needle}` appears {count} times instead of once in a shared helper",
                    infer_openai_path.display()
                ));
            }
        }

        for event_name in [
            "response.created",
            "response.output_text.delta",
            "response.output_text.done",
            "response.completed",
        ] {
            let needle = format!("\"type\": \"{event_name}\"");
            let count = src.matches(&needle).count();
            if count > 1 {
                warnings.push(format!(
                    "HTTP responses SSE payload drift resurfaced in {}: `{needle}` appears {count} times instead of once in a shared helper",
                    infer_openai_path.display()
                ));
            }
        }

        for response_item in ["function_call", "message", "output_text"] {
            let needle = format!("\"type\": \"{response_item}\"");
            let count = src.matches(&needle).count();
            if count > 1 {
                warnings.push(format!(
                    "HTTP responses output item drift resurfaced in {}: `{needle}` appears {count} times instead of once in a shared helper",
                    infer_openai_path.display()
                ));
            }
        }
    }

    entities.push(json!({
        "doctor": "inference-contracts",
        "surface": "inference-runtime",
        "checked_files": [
            ort_path.display().to_string(),
            py_path.display().to_string(),
            infer_client_path.display().to_string(),
            infer_sidecar_path.display().to_string(),
            infer_vllm_path.display().to_string(),
            infer_torch_path.display().to_string(),
            infer_mlx_path.display().to_string(),
            infer_openai_path.display().to_string(),
        ],
        "warning_count": warnings.len(),
        "evidence_count": evidence.len(),
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_inference_contracts"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            "inference contracts look intact for GLiNER word-level ONNX wiring and Flight tensor hot paths"
                .to_string()
        } else {
            format!(
                "inference contracts have {} warning(s) across GLiNER ONNX wiring or Flight tensor hot paths",
                warnings.len()
            )
        },
        confidence: if warnings.is_empty() { 0.97 } else { 0.68 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "surfaces": ["gliner-onnx", "flight-tensors", "http-tensors"],
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}
