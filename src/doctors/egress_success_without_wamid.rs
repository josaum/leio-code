//! Egress success-without-wamid doctor.
//!
//! On 2026-05-04 `example-gateway/src/server/handlers/egress.rs` had `unwrap_or("unknown")`
//! over the Meta API response field `messages[].id`. The gateway logged "WhatsApp message
//! sent successfully" with `wamid=unknown` even when Meta returned 2xx but no message id
//! (24h window expired, opted-out recipient, marketing block). Customers never received
//! the message; the platform reported success.
//!
//! Policy: in any `*/handlers/egress*.rs` (or `*/whatsapp/dispatch*.rs`) file, the parsed
//! response body's `messages[].id` extraction must NOT use `unwrap_or("unknown")` /
//! `unwrap_or_default()` followed by a "successfully sent" log. The fail-loud pattern is
//! a `match` / `if let Some(id)` that returns `Err(...)` when the id is missing.
//!
//! Static lint, narrow scope. Conservative: bans the literal string token `unwrap_or("unknown")`
//! anywhere in files matching the egress dispatch surface, and bans `unwrap_or_default()`
//! on lines that also reference `wamid`, `wa_message_id`, `messages[].id`, or `message_id`.

use std::path::{Path, PathBuf};
use std::time::Instant;

use ignore::WalkBuilder;
use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct EgressSuccessWithoutWamidDoctor;

impl Doctor for EgressSuccessWithoutWamidDoctor {
    fn name(&self) -> &'static str {
        "egress-success-without-wamid"
    }

    fn description(&self) -> &'static str {
        "Bans `unwrap_or(\"unknown\")` and id-related `unwrap_or_default()` patterns in WhatsApp egress dispatch files; Meta-API response id must fail loud, not silently log success."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_egress_success_without_wamid(root)
    }
}

const SCAN_GLOB_DIRS: &[&str] = &[
    "example-gateway/src/server/handlers",
    "example-gateway/src/whatsapp",
    "example-gateway/src/channels",
];

const ID_NEEDLES: &[&str] = &[
    "wamid",
    "wa_message_id",
    "message_id",
    "messages[\".id\"]",
    "messages[].id",
];

pub fn doctor_egress_success_without_wamid(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();
    let mut scanned = 0usize;
    let mut hits = 0usize;

    for path in collect_targets(root) {
        let rel = match path.strip_prefix(root) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        if !rel.contains("egress") && !rel.contains("dispatch") && !rel.contains("messaging") {
            continue;
        }
        let mut io = Vec::new();
        let body = match read_text(&path, &mut io) {
            Some(b) => b,
            None => {
                warnings.extend(io);
                continue;
            }
        };
        scanned += 1;
        for (idx, line) in body.lines().enumerate() {
            let trim = line.trim_start();
            if trim.starts_with("//") {
                continue;
            }
            // Pattern 1: `unwrap_or("unknown")` on a line that ALSO references a
            // Meta-response field (resp_body / messages / id / wamid). The unscoped
            // form (e.g. header default fallback) is allowed.
            let line_touches_response = line.contains("resp_body")
                || line.contains("response_body")
                || line.contains("messages")
                || line.contains("wamid")
                || line.contains("wa_message_id")
                || line.contains("message_id");
            if line.contains("unwrap_or(\"unknown\")") && line_touches_response {
                hits += 1;
                warnings.push(format!(
                    "{}:{}: `unwrap_or(\"unknown\")` on a Meta-response field -- failed Meta sends will be silently logged as success",
                    rel,
                    idx + 1
                ));
                evidence.push(EvidenceItem {
                    kind: "egress_unwrap_or_unknown".to_string(),
                    path: rel.clone(),
                    line: Some(idx + 1),
                    detail: line.trim().to_string(),
                });
                continue;
            }
            // Pattern 2: `unwrap_or_default()` on a line that also references one of
            // the id-bearing token names. (This is the "soft" form of the bug.)
            if line.contains("unwrap_or_default()")
                && ID_NEEDLES.iter().any(|needle| line.contains(needle))
            {
                hits += 1;
                warnings.push(format!(
                    "{}:{}: `unwrap_or_default()` on a Meta-id field -- prefer `match`/`if let Some(id)` and return Err on missing id",
                    rel,
                    idx + 1
                ));
                evidence.push(EvidenceItem {
                    kind: "egress_id_unwrap_or_default".to_string(),
                    path: rel.clone(),
                    line: Some(idx + 1),
                    detail: line.trim().to_string(),
                });
            }
        }
    }

    entities.push(json!({
        "doctor": "egress-success-without-wamid",
        "scanned_files": scanned,
        "violations": hits,
    }));

    let summary = if warnings.is_empty() {
        format!(
            "scanned {} egress/dispatch source file(s); no silent-success patterns found",
            scanned
        )
    } else {
        format!(
            "found {} silent-success pattern(s) in egress/dispatch surface",
            hits
        )
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_egress_success_without_wamid"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.94 } else { 0.55 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn collect_targets(root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for dir in SCAN_GLOB_DIRS {
        let scan_root = root.join(dir);
        if !scan_root.is_dir() {
            continue;
        }
        let mut builder = WalkBuilder::new(&scan_root);
        builder.hidden(false);
        builder.git_ignore(true);
        builder.require_git(false);
        for dent in builder.build().flatten() {
            let p = dent.path();
            if p.is_file()
                && p.extension().and_then(|e| e.to_str()) == Some("rs")
                && !p.to_string_lossy().contains("/tests/")
            {
                paths.push(p.to_path_buf());
            }
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_repo(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "leio-code-egress-wamid-{label}-{}-{nanos}",
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

    #[test]
    fn unwrap_or_unknown_in_egress_is_flagged() {
        let root = temp_repo("unknown");
        write(
            &root,
            "example-gateway/src/server/handlers/egress.rs",
            r#"async fn dispatch() {
    let wa_message_id = resp_body.get("messages").and_then(|m| m.as_str()).unwrap_or("unknown");
    info!("sent successfully");
}
"#,
        );
        let env = doctor_egress_success_without_wamid(&root);
        assert_eq!(env.warnings.len(), 1, "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn match_pattern_is_silent() {
        let root = temp_repo("match");
        write(
            &root,
            "example-gateway/src/server/handlers/egress.rs",
            r#"async fn dispatch() {
    let wa_message_id = match wa_message_id_opt {
        Some(id) => id,
        None => return Err("no message id".into()),
    };
}
"#,
        );
        let env = doctor_egress_success_without_wamid(&root);
        assert!(env.warnings.is_empty(), "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unwrap_or_default_on_id_line_is_flagged() {
        let root = temp_repo("default");
        write(
            &root,
            "example-gateway/src/whatsapp/dispatch.rs",
            r#"fn x() {
    let wamid = resp.get("id").and_then(|v| v.as_str()).unwrap_or_default();
}
"#,
        );
        let env = doctor_egress_success_without_wamid(&root);
        assert_eq!(env.warnings.len(), 1, "{:?}", env.warnings);
        let _ = fs::remove_dir_all(root);
    }
}
