//! WhatsApp audio/media hardening doctor.
//!
//! This doctor was tightened after the 2026-05-05 GCP OOM loop where
//! `example-worker-pratique-1` repeatedly died with `exitCode=137` while
//! HuggingFace/Xet (`hf-xet-*`) loaded Whisper inside the Celery worker. The
//! canonical evidence path for that incident was:
//!
//!   - `sudo dmesg -T | egrep -i "oom|Killed process|hf-xet|celery"`
//!   - `sudo journalctl -u docker --since ... | egrep "<cgroup-id>|worker-pratique|exitCode=137"`
//!   - `docker exec example-worker-pratique-1 sed -n '560,830p' .../tasks.py`
//!   - `docker compose config worker-pratique | egrep "WHATSAPP_AUDIO|WHATSAPP_MEDIA|HF_HUB_DISABLE_XET|mem_limit"`
//!
//! The repo-side invariant is now stricter than the original experimental
//! cartridge scan:
//!
//!   - Meta media downloads in `example-api/example/agents/tasks.py` must be
//!     streaming, byte-capped, timeout-bound, and fail-open.
//!   - Whisper/transformers ASR must be gated by
//!     `WHATSAPP_AUDIO_TRANSCRIPTION_ENABLED` and isolated in a subprocess with
//!     wallclock timeout + kill/terminate fallback.
//!   - `example-api/docker-compose.yml` must default in-process WhatsApp audio
//!     transcription off for constrained Celery workers, cap media bytes, and
//!     disable HF Xet unless a dedicated STT sidecar is introduced.

use std::path::Path;
use std::time::Instant;

use ignore::WalkBuilder;
use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct AudioHandlerTimeoutDoctor;

impl Doctor for AudioHandlerTimeoutDoctor {
    fn name(&self) -> &'static str {
        "audio-handler-timeout"
    }

    fn description(&self) -> &'static str {
        "Checks WhatsApp media/audio handling for byte caps, explicit timeouts, subprocess-isolated STT, and safe docker-compose defaults after the hf-xet/Whisper OOM incident."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_audio_handler_timeout(root)
    }
}

pub fn doctor_audio_handler_timeout(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let agent_checks = check_agent_tasks(root, &mut warnings, &mut evidence);
    let compose_checks = check_compose_defaults(root, &mut warnings, &mut evidence);
    let customer_ops_checks = check_customer_ops_audio_runtime(root, &mut warnings, &mut evidence);
    let cartridge_checks = scan_legacy_cartridge_surfaces(root, &mut warnings, &mut evidence);

    entities.push(json!({
        "doctor": "audio-handler-timeout",
        "agent_tasks": agent_checks,
        "compose_defaults": compose_checks,
        "customer_ops_audio_runtime": customer_ops_checks,
        "legacy_cartridge_scan": cartridge_checks,
        "incident": "2026-05-05 worker-pratique hf-xet/Whisper cgroup OOM loop",
    }));

    let summary = if warnings.is_empty() {
        "WhatsApp audio/media path is hardened: byte-capped streaming download, subprocess-gated STT, safe compose defaults, and no legacy cartridge timeout hints".to_string()
    } else {
        format!(
            "{} WhatsApp audio/media hardening warning(s); review before API/worker deploy",
            warnings.len()
        )
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_audio_handler_timeout"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.94 } else { 0.62 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "canonical_remote_evidence": [
                "sudo dmesg -T | egrep -i 'oom|Killed process|hf-xet|celery'",
                "sudo journalctl -u docker --since <window> --until <window> | egrep '<cgroup-id>|worker-pratique|exitCode=137'",
                "docker exec example-worker-pratique-1 sed -n '560,830p' /home/appuser/example-api/example/agents/tasks.py",
                "docker compose config worker-pratique | egrep 'WHATSAPP_AUDIO|WHATSAPP_MEDIA|HF_HUB_DISABLE_XET|mem_limit'"
            ]
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn check_agent_tasks(
    root: &Path,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) -> serde_json::Value {
    let rel = "example-api/example/agents/tasks.py";
    let path = root.join(rel);
    let mut io = Vec::new();
    let Some(body) = read_text(&path, &mut io) else {
        warnings.extend(io);
        warnings.push(format!(
            "{rel}: missing; cannot verify WhatsApp audio/media hardening"
        ));
        evidence.push(EvidenceItem {
            kind: "audio_agent_tasks_missing".to_string(),
            path: rel.to_string(),
            line: None,
            detail: "agent task file not found".to_string(),
        });
        return json!({"path": rel, "present": false});
    };

    let checks = [
        (
            "media_byte_cap_env",
            "WHATSAPP_MEDIA_MAX_BYTES",
            "Meta media downloads must be capped by WHATSAPP_MEDIA_MAX_BYTES",
        ),
        (
            "media_byte_cap_function",
            "def _max_media_bytes()",
            "Meta media download cap helper is missing",
        ),
        (
            "streaming_download",
            "stream=True",
            "Meta media download must stream instead of buffering the whole payload",
        ),
        (
            "streaming_iter_content",
            "iter_content(chunk_size=65536)",
            "Meta media download must enforce the cap while streaming chunks",
        ),
        (
            "content_length_precheck",
            "Content-Length",
            "Meta media download should reject oversized Content-Length before reading",
        ),
        (
            "audio_transcription_env_gate",
            "WHATSAPP_AUDIO_TRANSCRIPTION_ENABLED",
            "Whisper/transformers ASR must be explicitly gateable by env",
        ),
        (
            "required_audio_contract",
            "audio_transcription_required",
            "Sara's always-on audio route must override accidental global disablement",
        ),
        (
            "audio_outcome_enricher",
            "def _enrich_audio(",
            "Audio enrichment must retain a per-message transcription outcome",
        ),
        (
            "audio_outcome_status",
            "audio_transcription_status",
            "Audio task results must expose transcription success or failure",
        ),
        (
            "subprocess_target",
            "def _whisper_subprocess_target",
            "Whisper ASR must run in an isolated subprocess target",
        ),
        (
            "spawn_context",
            "get_context(\"spawn\")",
            "Whisper ASR subprocess must use spawn context to isolate worker state",
        ),
        (
            "wallclock_join_timeout",
            "proc.join(timeout=timeout_secs)",
            "Whisper ASR subprocess must have a wallclock timeout",
        ),
        (
            "terminate_or_kill_timeout",
            "proc.terminate()",
            "Timed out ASR subprocess must be terminated",
        ),
        (
            "kill_fallback",
            "proc.kill()",
            "Timed out/crashed ASR subprocess must have a kill fallback",
        ),
    ];

    let mut passed = 0usize;
    let mut failed = 0usize;
    for (name, needle, message) in checks {
        if body.contains(needle) {
            passed += 1;
            continue;
        }
        failed += 1;
        let line = line_no(&body, needle);
        warnings.push(format!("{rel}: {message}"));
        evidence.push(EvidenceItem {
            kind: format!("audio_agent_tasks_missing_{name}"),
            path: rel.to_string(),
            line,
            detail: message.to_string(),
        });
    }

    // Regression guard for the exact unsafe production shape observed during the incident.
    if body.contains("return r2.content") {
        failed += 1;
        warnings.push(format!(
            "{rel}: unsafe full-buffer Meta media download (`return r2.content`) can OOM Celery workers"
        ));
        evidence.push(EvidenceItem {
            kind: "audio_agent_tasks_full_buffer_download".to_string(),
            path: rel.to_string(),
            line: line_no(&body, "return r2.content"),
            detail: "use stream=True + iter_content + WHATSAPP_MEDIA_MAX_BYTES cap".to_string(),
        });
    }

    // Regression guard for in-worker Whisper pipeline construction without the isolated target.
    if body.contains("hf_pipeline(\"automatic-speech-recognition\"")
        && !body.contains("def _whisper_subprocess_target")
    {
        failed += 1;
        warnings.push(format!(
            "{rel}: transformers ASR pipeline is constructed without subprocess isolation"
        ));
        evidence.push(EvidenceItem {
            kind: "audio_agent_tasks_in_worker_whisper".to_string(),
            path: rel.to_string(),
            line: line_no(&body, "automatic-speech-recognition"),
            detail: "construct Whisper pipeline only inside _whisper_subprocess_target".to_string(),
        });
    }

    json!({"path": rel, "present": true, "passed": passed, "failed": failed})
}

fn check_compose_defaults(
    root: &Path,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) -> serde_json::Value {
    let rel = "example-api/docker-compose.yml";
    let path = root.join(rel);
    let mut io = Vec::new();
    let Some(body) = read_text(&path, &mut io) else {
        warnings.extend(io);
        warnings.push(format!("{rel}: missing; cannot verify safe audio defaults"));
        evidence.push(EvidenceItem {
            kind: "audio_compose_missing".to_string(),
            path: rel.to_string(),
            line: None,
            detail: "docker-compose.yml not found".to_string(),
        });
        return json!({"path": rel, "present": false});
    };

    let checks = [
        (
            "transcription_disabled_default",
            "WHATSAPP_AUDIO_TRANSCRIPTION_ENABLED: ${WHATSAPP_AUDIO_TRANSCRIPTION_ENABLED:-false}",
            "Compose must default in-process WhatsApp audio transcription off for Celery workers",
        ),
        (
            "transcription_timeout_default",
            "WHATSAPP_AUDIO_TRANSCRIPTION_TIMEOUT_SECS: ${WHATSAPP_AUDIO_TRANSCRIPTION_TIMEOUT_SECS:-30}",
            "Compose must bound optional audio transcription wallclock time",
        ),
        (
            "openai_transcription_base_default",
            "WHATSAPP_AUDIO_TRANSCRIPTION_OPENAI_BASE_URL: ${WHATSAPP_AUDIO_TRANSCRIPTION_OPENAI_BASE_URL:-https://api.openai.com/v1}",
            "Compose must keep WhatsApp audio transcription on the real OpenAI API instead of inheriting Sara's OpenAI-compatible Gemini base URL",
        ),
        (
            "media_max_bytes_default",
            "WHATSAPP_MEDIA_MAX_BYTES: ${WHATSAPP_MEDIA_MAX_BYTES:-10485760}",
            "Compose must default WhatsApp media downloads to a conservative byte cap",
        ),
        (
            "hf_xet_disabled_default",
            "HF_HUB_DISABLE_XET: ${HF_HUB_DISABLE_XET:-1}",
            "Compose must disable HF Xet by default in API/Celery containers",
        ),
    ];

    let mut passed = 0usize;
    let mut failed = 0usize;
    for (name, needle, message) in checks {
        if body.contains(needle) {
            passed += 1;
            continue;
        }
        failed += 1;
        warnings.push(format!("{rel}: {message}"));
        evidence.push(EvidenceItem {
            kind: format!("audio_compose_missing_{name}"),
            path: rel.to_string(),
            line: line_no(&body, needle.split(':').next().unwrap_or(needle)),
            detail: message.to_string(),
        });
    }

    json!({"path": rel, "present": true, "passed": passed, "failed": failed})
}

fn check_customer_ops_audio_runtime(
    root: &Path,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) -> serde_json::Value {
    let profile_rel = "deploy/profiles/customer_ops_unified.env";
    let secret_rel = "deploy/secret-sets/customer_ops_unified.env.example";
    let compose_rel = "example-api/docker-compose.yml";

    let mut profile_io = Vec::new();
    let Some(profile) = read_text(&root.join(profile_rel), &mut profile_io) else {
        return json!({"profile": profile_rel, "present": false, "skipped": true});
    };
    warnings.extend(profile_io);

    let audio_enabled = profile
        .lines()
        .any(|line| line.trim() == "WHATSAPP_AUDIO_TRANSCRIPTION_ENABLED=true");
    if !audio_enabled {
        return json!({"profile": profile_rel, "present": true, "audio_enabled": false});
    }

    let mut secret_io = Vec::new();
    let secret = read_text(&root.join(secret_rel), &mut secret_io).unwrap_or_default();
    warnings.extend(secret_io);

    let mut compose_io = Vec::new();
    let compose = read_text(&root.join(compose_rel), &mut compose_io).unwrap_or_default();
    warnings.extend(compose_io);

    let checks = [
        (
            "openai_key_documented",
            secret.contains("OPENAI_API_KEY=")
                || secret.contains("WHATSAPP_AUDIO_TRANSCRIPTION_OPENAI_API_KEY="),
            secret_rel,
            "customer_ops_unified enables WhatsApp audio transcription but the secret bundle does not document OPENAI_API_KEY or WHATSAPP_AUDIO_TRANSCRIPTION_OPENAI_API_KEY",
            "OPENAI_API_KEY",
        ),
        (
            "openai_key_propagated",
            compose.contains("OPENAI_API_KEY:"),
            compose_rel,
            "Compose must propagate OPENAI_API_KEY into the Sara worker that runs WhatsApp audio transcription",
            "OPENAI_API_KEY:",
        ),
        (
            "openai_transcription_base_profile",
            profile
                .contains("WHATSAPP_AUDIO_TRANSCRIPTION_OPENAI_BASE_URL=https://api.openai.com/v1"),
            profile_rel,
            "customer_ops_unified must pin WhatsApp audio transcription to the real OpenAI API; Sara's Gemini OpenAI-compatible base returns 404 for /audio/transcriptions",
            "WHATSAPP_AUDIO_TRANSCRIPTION_OPENAI_BASE_URL",
        ),
        (
            "infobip_media_username_documented",
            secret.contains("INFOBIP_BASIC_AUTH_USERNAME="),
            secret_rel,
            "Infobip inbound media downloads need INFOBIP_BASIC_AUTH_USERNAME in the customer_ops_unified secret bundle",
            "INFOBIP_BASIC_AUTH_USERNAME",
        ),
        (
            "infobip_media_password_documented",
            secret.contains("INFOBIP_BASIC_AUTH_PASSWORD="),
            secret_rel,
            "Infobip inbound media downloads need INFOBIP_BASIC_AUTH_PASSWORD in the customer_ops_unified secret bundle",
            "INFOBIP_BASIC_AUTH_PASSWORD",
        ),
    ];

    let mut passed = 0usize;
    let mut failed = 0usize;
    for (name, ok, rel, message, needle) in checks {
        if ok {
            passed += 1;
            continue;
        }
        failed += 1;
        warnings.push(format!("{rel}: {message}"));
        let body = if rel == compose_rel {
            &compose
        } else if rel == profile_rel {
            &profile
        } else {
            &secret
        };
        evidence.push(EvidenceItem {
            kind: format!("audio_customer_ops_missing_{name}"),
            path: rel.to_string(),
            line: line_no(body, needle),
            detail: message.to_string(),
        });
    }

    json!({
        "profile": profile_rel,
        "secret_set": secret_rel,
        "compose": compose_rel,
        "present": true,
        "audio_enabled": true,
        "passed": passed,
        "failed": failed,
    })
}

fn scan_legacy_cartridge_surfaces(
    root: &Path,
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
) -> serde_json::Value {
    let cartridges_root = root.join("cartridges");
    if !cartridges_root.is_dir() {
        return json!({"scanned_files": 0, "hits": 0});
    }

    let mut builder = WalkBuilder::new(&cartridges_root);
    builder.hidden(false);
    builder.git_ignore(true);
    builder.require_git(false);
    builder.max_depth(Some(3));

    let mut scanned = 0usize;
    let mut hits = 0usize;
    for dent in builder.build().flatten() {
        let path = dent.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name != "tasks.py" && name != "agent.py" && name != "messaging.py" {
            continue;
        }
        let mut io = Vec::new();
        let Some(body) = read_text(path, &mut io) else {
            warnings.extend(io);
            continue;
        };
        let lower = body.to_ascii_lowercase();
        if !(lower.contains("audio")
            || lower.contains("voice")
            || lower.contains("media")
            || lower.contains("ogg")
            || lower.contains("whatsapp_media"))
        {
            continue;
        }
        scanned += 1;
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        let has_httpx_client =
            body.contains("httpx.AsyncClient(") || body.contains("httpx.Client(");
        let has_explicit_timeout = body.contains("timeout=");
        if has_httpx_client && !has_explicit_timeout {
            hits += 1;
            warnings.push(format!(
                "{rel_str}: constructs httpx client(s) without an explicit `timeout=` -- audio/media downloads can hang Celery workers"
            ));
            evidence.push(EvidenceItem {
                kind: "audio_handler_no_timeout".to_string(),
                path: rel_str.clone(),
                line: line_no(&body, "httpx."),
                detail: "audio/media dispatch surface uses httpx without explicit timeout"
                    .to_string(),
            });
        }

        let has_media_get = body.contains(".get(") && lower.contains("media");
        let has_try_except = body.contains("try:") && body.contains("except");
        if has_media_get && !has_try_except {
            hits += 1;
            warnings.push(format!(
                "{rel_str}: media GET without try/except -- download failures may surface as task crashes"
            ));
            evidence.push(EvidenceItem {
                kind: "audio_handler_no_try_except".to_string(),
                path: rel_str.clone(),
                line: line_no(&body, ".get("),
                detail: "audio/media dispatch surface lacks try/except around media .get(...)"
                    .to_string(),
            });
        }
    }

    json!({"scanned_files": scanned, "hits": hits})
}

fn line_no(body: &str, needle: &str) -> Option<usize> {
    body.find(needle)
        .map(|idx| body[..idx].chars().filter(|c| *c == '\n').count() + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_repo(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-audio-handler-{label}-{}-{nanos}",
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

    fn safe_tasks_py() -> &'static str {
        r#"def _max_media_bytes():
    return int(os.environ.get("WHATSAPP_MEDIA_MAX_BYTES", "26214400"))

def _download_meta_media_bytes(url):
    with requests.get(url, headers={}, timeout=30, stream=True) as r2:
        content_length = r2.headers.get("Content-Length")
        for chunk in r2.iter_content(chunk_size=65536):
            pass

def _whisper_subprocess_target(tmp_path, model_alias, queue):
    pipe = hf_pipeline("automatic-speech-recognition", model=model_alias, device="cpu")

def _run_whisper_on_bytes(raw, ext, media_id):
    if not _env_flag("WHATSAPP_AUDIO_TRANSCRIPTION_ENABLED", True):
        return None
    ctx = _mp.get_context("spawn")
    proc = ctx.Process(target=_whisper_subprocess_target)
    proc.join(timeout=timeout_secs)
    proc.terminate()
    proc.kill()

def _enrich_audio(event):
    required = event.get("audio_transcription_required")
    return {**event, "audio_transcription_status": "succeeded"}
"#
    }

    fn safe_compose() -> &'static str {
        r#"x-common-env: &common-env
  WHATSAPP_AUDIO_TRANSCRIPTION_ENABLED: ${WHATSAPP_AUDIO_TRANSCRIPTION_ENABLED:-false}
  WHATSAPP_AUDIO_TRANSCRIPTION_TIMEOUT_SECS: ${WHATSAPP_AUDIO_TRANSCRIPTION_TIMEOUT_SECS:-30}
  WHATSAPP_AUDIO_TRANSCRIPTION_OPENAI_BASE_URL: ${WHATSAPP_AUDIO_TRANSCRIPTION_OPENAI_BASE_URL:-https://api.openai.com/v1}
  WHATSAPP_MEDIA_MAX_BYTES: ${WHATSAPP_MEDIA_MAX_BYTES:-10485760}
  HF_HUB_DISABLE_XET: ${HF_HUB_DISABLE_XET:-1}
"#
    }

    fn safe_compose_with_openai() -> String {
        format!(
            "{}  OPENAI_API_KEY: ${{OPENAI_API_KEY:-}}\n",
            safe_compose()
        )
    }

    #[test]
    fn hardened_audio_path_is_silent() {
        let root = temp_repo("safe");
        write(
            &root,
            "example-api/example/agents/tasks.py",
            safe_tasks_py(),
        );
        write(&root, "example-api/docker-compose.yml", safe_compose());
        let env = doctor_audio_handler_timeout(&root);
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn old_in_worker_whisper_and_full_buffer_download_are_flagged() {
        let root = temp_repo("unsafe");
        write(
            &root,
            "example-api/example/agents/tasks.py",
            r#"def _download_meta_media_bytes(url):
    r2 = requests.get(url, headers={}, timeout=30)
    return r2.content

def _run_whisper_on_bytes(raw, ext, media_id):
    pipe = hf_pipeline("automatic-speech-recognition", model="openai/whisper-large-v3", device="cpu")
"#,
        );
        write(
            &root,
            "example-api/docker-compose.yml",
            "x-common-env: &common-env\n",
        );
        let env = doctor_audio_handler_timeout(&root);
        assert!(
            env.warnings.iter().any(|w| w.contains("full-buffer")),
            "{:?}",
            env.warnings
        );
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("subprocess isolation")),
            "{:?}",
            env.warnings
        );
        assert!(
            env.warnings.iter().any(|w| w.contains("transcription off")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_cartridge_missing_timeout_is_flagged() {
        let root = temp_repo("legacy-notimeout");
        write(
            &root,
            "example-api/example/agents/tasks.py",
            safe_tasks_py(),
        );
        write(&root, "example-api/docker-compose.yml", safe_compose());
        write(
            &root,
            "cartridges/foo/tasks.py",
            r#"import httpx
async def handle_audio(url):
    async with httpx.AsyncClient() as client:
        try:
            r = await client.get(url)
        except Exception:
            return None
"#,
        );
        let env = doctor_audio_handler_timeout(&root);
        assert!(!env.warnings.is_empty(), "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn customer_ops_audio_profile_requires_infobip_media_auth_names() {
        let root = temp_repo("customer-ops-audio-secret");
        write(
            &root,
            "example-api/example/agents/tasks.py",
            safe_tasks_py(),
        );
        write(
            &root,
            "example-api/docker-compose.yml",
            &safe_compose_with_openai(),
        );
        write(
            &root,
            "deploy/profiles/customer_ops_unified.env",
            "WHATSAPP_AUDIO_TRANSCRIPTION_ENABLED=true\nWHATSAPP_AUDIO_TRANSCRIPTION_OPENAI_BASE_URL=https://api.openai.com/v1\n",
        );
        write(
            &root,
            "deploy/secret-sets/customer_ops_unified.env.example",
            "OPENAI_API_KEY=\nINFOBIP_BASIC_AUTH_USERNAME=\n",
        );

        let env = doctor_audio_handler_timeout(&root);
        assert!(
            env.warnings
                .iter()
                .any(|w| w.contains("INFOBIP_BASIC_AUTH_PASSWORD")),
            "{:?}",
            env.warnings
        );
        let _ = fs::remove_dir_all(root);
    }
}
