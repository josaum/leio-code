//! example-server Dockerfile/context coherence doctor.
//!
//! `example-platform/Dockerfile.server` uses a minimized build context prepared
//! by `scripts/docker-context.sh`. Any first-party source named by a build
//! context `COPY` must be rsync'd into `$CTX/server/...`, otherwise BuildKit
//! fails before `cargo chef` can run.
//!
//! This guards the PR #247 smoke failure where `Dockerfile.server` copied
//! `example-ort-utils` in planner and builder stages, but the server context
//! script did not include the directory.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::query_id;
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

const DOCTOR_NAME: &str = "server-dockerfile-context-coherence";

pub struct ServerDockerfileContextCoherenceDoctor;

impl Doctor for ServerDockerfileContextCoherenceDoctor {
    fn name(&self) -> &'static str {
        DOCTOR_NAME
    }

    fn description(&self) -> &'static str {
        "Validates that every first-party build-context COPY source in example-platform/Dockerfile.server is rsync'd into scripts/docker-context.sh's server context."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_server_dockerfile_context_coherence(root)
    }
}

pub fn doctor_server_dockerfile_context_coherence(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    let dockerfile = root.join("example-platform/Dockerfile.server");
    let ctx_script = root.join("scripts/docker-context.sh");

    let Ok(dockerfile_text) = std::fs::read_to_string(&dockerfile) else {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor_server_dockerfile_context_coherence"),
            kind: "doctor".to_string(),
            summary: format!(
                "{DOCTOR_NAME}: example-platform/Dockerfile.server not found - doctor skipped"
            ),
            confidence: 0.95,
            entities,
            evidence,
            warnings,
            meta: Some(json!({"reason": "dockerfile_server_missing"})),
            timing_ms: started.elapsed().as_millis(),
        };
    };
    let ctx_text = std::fs::read_to_string(&ctx_script).unwrap_or_default();

    let sources = copied_build_context_sources(&dockerfile_text);
    for source in &sources {
        if !server_context_mentions_source(&ctx_text, source) {
            warnings.push(format!(
                "[{DOCTOR_NAME}] example-platform/Dockerfile.server copies `{source}`, but scripts/docker-context.sh does not rsync it into `$CTX/server/{source}`"
            ));
            entities.push(json!({
                "doctor": DOCTOR_NAME,
                "surface": "scripts/docker-context.sh",
                "copy_source": source,
                "rsync_count": 0,
            }));
            evidence.push(EvidenceItem {
                kind: "missing_server_context_rsync".to_string(),
                path: "scripts/docker-context.sh".to_string(),
                line: None,
                detail: format!(
                    "expected `rsync ... $ROOT/{source}/ $CTX/server/{source}/` for a Dockerfile.server COPY source"
                ),
            });
        }
    }

    let summary = format!(
        "{DOCTOR_NAME}: {} build-context COPY source(s), {} drift warning(s)",
        sources.len(),
        warnings.len()
    );

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_server_dockerfile_context_coherence"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.95 } else { 0.85 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "copy_sources": sources,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn copied_build_context_sources(dockerfile_text: &str) -> Vec<String> {
    let mut sources = Vec::new();
    for line in dockerfile_text.lines() {
        let mut rest = line.trim_start();
        if !rest.starts_with("COPY ") {
            continue;
        }
        rest = rest.trim_start_matches("COPY ").trim_start();
        if rest.starts_with('[') {
            // JSON-form COPY is not used by Dockerfile.server today. Leaving it
            // alone avoids a fragile partial parser.
            continue;
        }

        let mut skip_line = false;
        loop {
            let Some((token, tail)) = split_first_token(rest) else {
                skip_line = true;
                break;
            };
            if !token.starts_with("--") {
                rest = rest.trim_start();
                break;
            }
            if token.starts_with("--from=") {
                skip_line = true;
                break;
            }
            rest = tail.trim_start();
        }
        if skip_line {
            continue;
        }

        let Some((source, _tail)) = split_first_token(rest) else {
            continue;
        };
        if source.starts_with('/')
            || source == "."
            || source == ".."
            || source.starts_with('$')
            || source.contains('*')
        {
            continue;
        }
        let source = source.trim_end_matches('/').to_string();
        if !sources.contains(&source) {
            sources.push(source);
        }
    }
    sources.sort();
    sources
}

fn split_first_token(input: &str) -> Option<(&str, &str)> {
    let trimmed = input.trim_start();
    if trimmed.is_empty() {
        return None;
    }
    for (idx, ch) in trimmed.char_indices() {
        if ch.is_whitespace() {
            return Some((&trimmed[..idx], &trimmed[idx..]));
        }
    }
    Some((trimmed, ""))
}

fn server_context_mentions_source(ctx_text: &str, source: &str) -> bool {
    let needle_plain = format!("$CTX/server/{source}");
    let needle_braced = format!("${{CTX}}/server/{source}");
    ctx_text.contains(&needle_plain) || ctx_text.contains(&needle_braced)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_tempdir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-server-docker-context-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(path, contents).expect("write fixture");
    }

    #[test]
    fn flags_copy_source_missing_from_server_context() {
        let dir = unique_tempdir("missing-source");
        write_file(
            &dir.join("example-platform/Dockerfile.server"),
            "FROM rust AS planner\nCOPY example-platform ./example-platform\nCOPY example-ort-utils ./example-ort-utils\n",
        );
        write_file(
            &dir.join("scripts/docker-context.sh"),
            "rsync \"$ROOT/example-platform/\" \"$CTX/server/example-platform/\"\n",
        );

        let envelope = doctor_server_dockerfile_context_coherence(&dir);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("example-ort-utils") && w.contains("docker-context.sh")),
            "expected missing server context source to be flagged, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn does_not_flag_when_copy_sources_are_wired() {
        let dir = unique_tempdir("wired-ok");
        write_file(
            &dir.join("example-platform/Dockerfile.server"),
            "FROM rust AS planner\nCOPY example-platform ./example-platform\nCOPY example-ort-utils ./example-ort-utils\nCOPY example-api/sdks/example-client ./example-api/sdks/example-client\n",
        );
        write_file(
            &dir.join("scripts/docker-context.sh"),
            "rsync \"$ROOT/example-platform/\" \"$CTX/server/example-platform/\"\nrsync \"$ROOT/example-ort-utils/\" \"$CTX/server/example-ort-utils/\"\nrsync \"$ROOT/example-api/sdks/example-client/\" \"$CTX/server/example-api/sdks/example-client/\"\n",
        );

        let envelope = doctor_server_dockerfile_context_coherence(&dir);
        assert!(
            envelope.warnings.is_empty(),
            "fully wired copy sources should not warn, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignores_stage_copy_sources() {
        let dir = unique_tempdir("stage-copy");
        write_file(
            &dir.join("example-platform/Dockerfile.server"),
            "FROM rust AS builder\nCOPY --from=builder /tmp/example-server /usr/local/bin/example-server\n",
        );
        write_file(&dir.join("scripts/docker-context.sh"), "");

        let envelope = doctor_server_dockerfile_context_coherence(&dir);
        assert!(
            envelope.warnings.is_empty(),
            "stage-copy sources should not require context rsync, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
