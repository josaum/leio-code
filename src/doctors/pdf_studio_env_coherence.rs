use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{QueryEnvelope, RepoIndex};

pub struct PdfStudioEnvCoherenceDoctor;

impl Doctor for PdfStudioEnvCoherenceDoctor {
    fn name(&self) -> &'static str {
        "pdf-studio-env-coherence"
    }

    fn description(&self) -> &'static str {
        "Checks that PDF_STUDIO_* environment variables are set correctly in example-api/.env to prevent runtime failures in rendering and report generation."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        let started = Instant::now();
        let mut io_warnings = Vec::new();
        let mut warnings = Vec::new();
        let evidence = Vec::new();

        // Local dev uses example-api/.env (gitignored). CI and fresh clones rely on
        // the committed template, Dockerfile ENV defaults, and the deploy profile
        // fallback chain in deploy/defaults.env instead. The deploy file is the
        // one that materialized the 2026-08-17 incident: an unset studio key
        // silently falls back to OPENAI_API_KEY against a Gemini endpoint.
        let source_paths = [
            root.join("example-api/.env"),
            root.join("example-api/.env.example"),
            root.join("example-api/Dockerfile"),
            root.join("deploy/defaults.env"),
        ];

        let mut has_render_bin = false;
        let mut has_report_bin = false;
        let mut has_render_root = false;
        let mut sources_read = 0usize;
        // LLM provider coherence: the exact class of drift that took production
        // authoring down (empty PDF_STUDIO_LLM_API_KEY -> compose silently falls
        // back to OPENAI_API_KEY -> an sk- key authenticating a Gemini endpoint
        // 400/404s every completion). Values are never echoed, only their shape.
        let mut llm_base_url: Option<String> = None;
        let mut llm_key_shape: Option<&'static str> = None;

        for source_path in &source_paths {
            if source_path.ends_with(".env") && !source_path.exists() {
                continue;
            }
            let Some(content) = read_text(source_path, &mut io_warnings) else {
                continue;
            };
            sources_read += 1;
            for line in content.lines() {
                let trimmed = line.trim();
                if line_has_pdf_studio_var(trimmed, "PDF_STUDIO_RENDER_BIN") {
                    has_render_bin = true;
                } else if line_has_pdf_studio_var(trimmed, "PDF_STUDIO_REPORT_BIN") {
                    has_report_bin = true;
                } else if line_has_pdf_studio_var(trimmed, "PDF_STUDIO_RENDER_ROOT") {
                    has_render_root = true;
                } else if let Some(value) = line_value(trimmed, "PDF_STUDIO_LLM_BASE_URL") {
                    llm_base_url = Some(value.to_string());
                } else if let Some(value) = line_value(trimmed, "PDF_STUDIO_LLM_API_KEY") {
                    llm_key_shape = Some(key_shape(value));
                }
            }
        }

        if sources_read == 0 {
            warnings.push(
                "no PDF Studio env source found (expected example-api/.env, .env.example, or Dockerfile)"
                    .to_string(),
            );
        }
        warnings.extend(io_warnings);

        if !has_render_bin {
            warnings
                .push("PDF_STUDIO_RENDER_BIN is missing from example-api env sources".to_string());
        }
        if !has_report_bin {
            warnings
                .push("PDF_STUDIO_REPORT_BIN is missing from example-api env sources".to_string());
        }
        if !has_render_root {
            warnings
                .push("PDF_STUDIO_RENDER_ROOT is missing from example-api env sources".to_string());
        }

        // Provider/key coherence rules. Placeholders (template defaults such as
        // `${...}`, `<...>`, `changeme`) are skipped — only concrete drift is a
        // warning, so a fresh clone stays green.
        if let Some(shape) = llm_key_shape {
            if shape == "empty" {
                warnings.push(
                    "PDF_STUDIO_LLM_API_KEY is set but empty: compose falls back to OPENAI_API_KEY, \
                     which cannot authenticate non-OpenAI endpoints (production incident 2026-08-17)"
                        .to_string(),
                );
            }
            if let Some(base) = &llm_base_url {
                let gemini = base.contains("generativelanguage.googleapis.com");
                let openai = base.contains("api.openai.com");
                if gemini && shape == "sk" {
                    warnings.push(
                        "PDF_STUDIO_LLM_BASE_URL targets Gemini but the API key has the OpenAI sk- shape \
                         — every authoring completion will 400/404"
                            .to_string(),
                    );
                }
                if openai && shape == "google" {
                    warnings.push(
                        "PDF_STUDIO_LLM_BASE_URL targets api.openai.com but the API key has the Google AIza shape"
                            .to_string(),
                    );
                }
            }
        }

        QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_pdf_studio_env_coherence"),
            kind: "doctor".to_string(),
            summary: if warnings.is_empty() {
                "PDF_STUDIO_* environment variables are correctly configured.".to_string()
            } else {
                format!(
                    "Found {} warnings in PDF_STUDIO_* env config.",
                    warnings.len()
                )
            },
            confidence: if warnings.is_empty() { 0.98 } else { 0.8 },
            entities: vec![json!({
                "env_sources_checked": source_paths
                    .iter()
                    .map(|path| path.to_string_lossy().to_string())
                    .collect::<Vec<_>>(),
                "sources_read": sources_read,
                "has_render_bin": has_render_bin,
                "has_report_bin": has_report_bin,
                "has_render_root": has_render_root,
                "llm_base_url": llm_base_url,
                "llm_key_shape": llm_key_shape,
            })],
            evidence,
            warnings,
            meta: Some(json!({
                "doctor": "pdf-studio-env-coherence",
            })),
            timing_ms: started.elapsed().as_millis(),
        }
    }
}

fn line_has_pdf_studio_var(line: &str, key: &str) -> bool {
    let prefix = format!("{key}=");
    line.starts_with(&prefix) || line.starts_with(&format!("{key}:"))
}

/// Return the value of `KEY=value` / `KEY: value` if this line declares it.
fn line_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}=");
    if let Some(rest) = line.strip_prefix(&prefix) {
        return Some(rest.trim());
    }
    let colon = format!("{key}:");
    line.strip_prefix(&colon).map(|rest| rest.trim())
}

/// Classify a concrete key by shape only — the secret itself never leaves this fn.
fn key_shape(value: &str) -> &'static str {
    if value.is_empty()
        || value.starts_with("${")
        || value.starts_with('<')
        || value.eq_ignore_ascii_case("changeme")
    {
        return if value.is_empty() {
            "empty"
        } else {
            "placeholder"
        };
    }
    if value.starts_with("sk-") {
        "sk"
    } else if value.starts_with("AIza") {
        "google"
    } else {
        "other"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_index() -> RepoIndex {
        RepoIndex {
            version: 1,
            root: "/tmp/repo".to_string(),
            indexed_at: "2026-01-01T00:00:00Z".to_string(),
            files: Vec::new(),
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        }
    }

    fn run_doctor(root: &Path) -> (Vec<String>, serde_json::Value) {
        let index = empty_index();
        let envelope = PdfStudioEnvCoherenceDoctor.run(&index, root);
        let entities = envelope
            .entities
            .first()
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        (envelope.warnings, entities)
    }

    fn write_env(root: &Path, body: &str) {
        let api = root.join("example-api");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::write(api.join(".env"), body).unwrap();
        std::fs::write(api.join(".env.example"), "").unwrap();
        std::fs::write(api.join("Dockerfile"), "").unwrap();
        let deploy = root.join("deploy");
        std::fs::create_dir_all(&deploy).unwrap();
        std::fs::write(deploy.join("defaults.env"), "").unwrap();
    }

    const BASE_VARS: &str = "PDF_STUDIO_RENDER_BIN=render-deck\nPDF_STUDIO_REPORT_BIN=workspace-docs-report\nPDF_STUDIO_RENDER_ROOT=/opt/pdf-studio-root\n";

    #[test]
    fn coherent_gemini_env_is_clean() {
        let dir = tempfile::tempdir().unwrap();
        write_env(
            dir.path(),
            &format!(
                "{BASE_VARS}PDF_STUDIO_LLM_BASE_URL=https://generativelanguage.googleapis.com/v1beta/openai/\nPDF_STUDIO_LLM_API_KEY=AIzaSyFAKE-fake-fake\n"
            ),
        );
        let (warnings, entities) = run_doctor(dir.path());
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(entities["llm_key_shape"], "google");
    }

    #[test]
    fn sk_key_against_gemini_endpoint_is_flagged() {
        let dir = tempfile::tempdir().unwrap();
        write_env(
            dir.path(),
            &format!(
                "{BASE_VARS}PDF_STUDIO_LLM_BASE_URL=https://generativelanguage.googleapis.com/v1beta/openai/\nPDF_STUDIO_LLM_API_KEY=sk-proj-FAKEFAKE\n"
            ),
        );
        let (warnings, _) = run_doctor(dir.path());
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("Gemini") && w.contains("sk-")),
            "expected Gemini/sk- mismatch warning, got: {warnings:?}"
        );
    }

    #[test]
    fn empty_key_warns_about_silent_openai_fallback() {
        let dir = tempfile::tempdir().unwrap();
        write_env(
            dir.path(),
            &format!(
                "{BASE_VARS}PDF_STUDIO_LLM_BASE_URL=https://generativelanguage.googleapis.com/v1beta/openai/\nPDF_STUDIO_LLM_API_KEY=\n"
            ),
        );
        let (warnings, _) = run_doctor(dir.path());
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("falls back to OPENAI_API_KEY")),
            "expected silent-fallback warning, got: {warnings:?}"
        );
    }

    #[test]
    fn placeholder_template_stays_clean() {
        let dir = tempfile::tempdir().unwrap();
        write_env(
            dir.path(),
            &format!(
                "{BASE_VARS}PDF_STUDIO_LLM_BASE_URL=https://generativelanguage.googleapis.com/v1beta/openai/\nPDF_STUDIO_LLM_API_KEY=changeme\n"
            ),
        );
        let (warnings, entities) = run_doctor(dir.path());
        assert!(
            warnings.is_empty(),
            "placeholder must not warn: {warnings:?}"
        );
        assert_eq!(entities["llm_key_shape"], "placeholder");
    }

    #[test]
    fn google_key_against_openai_endpoint_is_flagged() {
        let dir = tempfile::tempdir().unwrap();
        write_env(
            dir.path(),
            &format!(
                "{BASE_VARS}PDF_STUDIO_LLM_BASE_URL=https://api.openai.com/v1\nPDF_STUDIO_LLM_API_KEY=AIzaSyFAKE-fake-fake\n"
            ),
        );
        let (warnings, _) = run_doctor(dir.path());
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("api.openai.com") && w.contains("AIza")),
            "expected OpenAI/AIza mismatch warning, got: {warnings:?}"
        );
    }
}
