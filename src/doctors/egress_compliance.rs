use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex, SourceLanguage};

pub struct EgressComplianceDoctor;

impl Doctor for EgressComplianceDoctor {
    fn name(&self) -> &'static str {
        "egress-compliance"
    }

    fn description(&self) -> &'static str {
        "Checks that control-plane routers and Rust campaign workers do not bypass canonical egress."
    }

    fn run(&self, index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_egress_compliance(index, root)
    }
}

const DIRECT_EGRESS_PATTERNS: &[(&str, &str)] = &[
    ("graph.facebook.com", "direct WhatsApp API call"),
    ("api.twilio.com", "direct Twilio API call"),
    ("api.sendgrid.com", "direct SendGrid API call"),
    ("api.mailgun.net", "direct Mailgun API call"),
    ("import smtplib", "direct SMTP usage"),
    ("from smtplib import", "direct SMTP usage"),
    ("smtplib.smtp", "direct SMTP usage"),
    ("smtp.send_message", "direct SMTP usage"),
    ("smtp.sendmail", "direct SMTP usage"),
];

const EGRESS_ALLOWLIST_PATHS: &[&str] = &[
    "example-gateway/src/",
    "example-api/example/core/messaging/",
    "example-api/example/core/egress/",
    "example-api/example/egress/",
];

pub fn doctor_egress_compliance(index: &RepoIndex, root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();
    let mut violations = 0usize;

    for file in &index.files {
        if file.language != SourceLanguage::Python {
            continue;
        }
        if is_test_path(&file.path) {
            continue;
        }

        // Skip files that are part of the egress pipeline itself
        if EGRESS_ALLOWLIST_PATHS
            .iter()
            .any(|prefix| file.path.starts_with(prefix))
        {
            continue;
        }

        // Focus on router files and cartridge code
        let is_router = file.path.contains("router.py");
        let is_cartridge = file.path.starts_with("cartridges/");
        if !is_router && !is_cartridge {
            continue;
        }

        let content = match read_text(&root.join(&file.path), &mut Vec::new()) {
            Some(c) => c,
            None => continue,
        };

        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let lower = line.to_ascii_lowercase();

            for (pattern, description) in DIRECT_EGRESS_PATTERNS {
                if lower.contains(&pattern.to_ascii_lowercase()) {
                    violations += 1;
                    warnings.push(format!(
                        "{}: {} at line {} -- {}",
                        file.path,
                        description,
                        idx + 1,
                        trimmed
                    ));
                    evidence.push(EvidenceItem {
                        kind: "egress_violation".to_string(),
                        path: file.path.clone(),
                        line: Some(idx + 1),
                        detail: description.to_string(),
                    });
                }
            }
        }
    }

    // MCP message tools execute inside the Rust data plane, but they are still
    // callers of the canonical egress boundary.  Only protocol acknowledgements
    // (read receipts and typing indicators) may touch Meta directly; customer-
    // visible content must submit an EgressIntent so all three gates run.
    let rust_mcp_path = "example-gateway/src/integrations/tools/messaging/whatsapp.rs";
    if index.files.iter().any(|file| file.path == rust_mcp_path)
        && let Some(content) = read_text(&root.join(rust_mcp_path), &mut Vec::new())
    {
        let mut current_function = String::new();
        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed
                .strip_prefix("pub async fn ")
                .or_else(|| trimmed.strip_prefix("async fn "))
            {
                current_function = rest
                    .split(['(', '<'])
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_string();
            }

            let direct_provider_call =
                trimmed.contains("graph.facebook.com") || trimmed.contains(".bearer_auth(");
            let protocol_signal = matches!(
                current_function.as_str(),
                "mark_as_read" | "send_typing_indicator"
            );
            if direct_provider_call && !protocol_signal {
                violations += 1;
                warnings.push(format!(
                    "{rust_mcp_path}: MCP function `{current_function}` bypasses canonical egress at line {} -- {trimmed}",
                    idx + 1
                ));
                evidence.push(EvidenceItem {
                    kind: "rust_mcp_direct_provider_egress".to_string(),
                    path: rust_mcp_path.to_string(),
                    line: Some(idx + 1),
                    detail: format!(
                        "MCP function `{current_function}` must submit EgressIntent; only read/typing protocol signals may call Meta directly"
                    ),
                });
            }
        }

        if content.contains("is_customer_visible_whatsapp_tool") {
            let canonical_markers = [
                "access.submit(intent, \"mcp-tool\")",
                "ensure_agent_send_lease(ctx, &cmd).await?",
                "ToolOrigin::Agent",
                "ToolOrigin::HumanOperator",
                "ToolOrigin::AuthenticatedService",
            ];
            for marker in canonical_markers {
                if !content.contains(marker) {
                    violations += 1;
                    warnings.push(format!(
                        "{rust_mcp_path}: canonical MCP egress marker missing: {marker}"
                    ));
                    evidence.push(EvidenceItem {
                        kind: "rust_mcp_missing_canonical_egress".to_string(),
                        path: rust_mcp_path.to_string(),
                        line: None,
                        detail: format!("missing canonical egress marker: {marker}"),
                    });
                }
            }

            for function in [
                "send_message",
                "send_flow",
                "send_template",
                "send_media",
                "send_buttons",
                "send_list",
                "send_location",
                "send_contacts",
                "send_reaction",
                "send_cta_url",
                "send_cta_phone",
                "send_product",
                "send_product_list",
                "send_catalog_message",
                "send_order_details",
                "send_order_status",
            ] {
                let signature = format!("async fn {function}(");
                let Some(start) = content.find(&signature) else {
                    violations += 1;
                    warnings.push(format!(
                        "{rust_mcp_path}: registered content sender `{function}` is missing"
                    ));
                    continue;
                };
                let rest = &content[start..];
                let end = rest[signature.len()..]
                    .find("\n    async fn ")
                    .map(|offset| signature.len() + offset)
                    .or_else(|| rest.find("\n}\n\n#[async_trait]"))
                    .unwrap_or(rest.len());
                if !rest[..end].contains("submit_whatsapp_payload(") {
                    violations += 1;
                    warnings.push(format!(
                        "{rust_mcp_path}: `{function}` does not submit through canonical MCP egress"
                    ));
                    evidence.push(EvidenceItem {
                        kind: "rust_mcp_missing_canonical_egress".to_string(),
                        path: rust_mcp_path.to_string(),
                        line: None,
                        detail: format!(
                            "customer-visible sender `{function}` must call submit_whatsapp_payload"
                        ),
                    });
                }
            }
        }
    }

    // Campaign scheduling/recovery lives in Rust today, but it is still a
    // control-plane caller. It must submit EgressIntent through the canonical
    // authenticated handoff instead of reaching Meta directly.
    for file in &index.files {
        if file.language != SourceLanguage::Rust
            || file.path != "example-gateway/src/ops_console/routes/campaigns.rs"
            || is_test_path(&file.path)
        {
            continue;
        }

        let content = match read_text(&root.join(&file.path), &mut Vec::new()) {
            Some(content) => content,
            None => continue,
        };
        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("//") {
                continue;
            }
            let lower = trimmed.to_ascii_lowercase();
            let direct_messages_url = lower.contains("graph.facebook.com")
                && (lower.contains("/messages\"") || lower.contains("/messages'"));
            let direct_meta_helper = trimmed.contains("meta_post_json(");
            if direct_messages_url || direct_meta_helper {
                let description = if direct_messages_url {
                    "Rust campaign direct WhatsApp messages URL"
                } else {
                    "Rust campaign direct Meta POST helper"
                };
                violations += 1;
                warnings.push(format!(
                    "{}: {} at line {} -- {}",
                    file.path,
                    description,
                    idx + 1,
                    trimmed
                ));
                evidence.push(EvidenceItem {
                    kind: "egress_violation".to_string(),
                    path: file.path.clone(),
                    line: Some(idx + 1),
                    detail: description.to_string(),
                });
            }
        }

        let defines_campaign_send = content.contains("send_campaign_for_tenant")
            || content.contains("send_campaign_api")
            || content.contains("send_test_template_api");
        if defines_campaign_send
            && (!content.contains("EgressIntent")
                || !content.contains("submit_internal_outbound_intent")
                || !content.contains("campaign_delivery_claim")
                || !content.contains("record_campaign_delivery_egress_result")
                || !content.contains(r#""source": "campaign_worker""#)
                || !content.contains("let initial_status = \"draft\"")
                || !content.contains("draft_response: gate_content")
                || !content.contains("principal_can_access_campaign_tenant")
                || !content.contains("messages:send permission required")
                || !content.contains("cases:assign permission required")
                || !content.contains("Campaign status cannot be changed through generic PATCH")
                || !content.contains("status = 'draft'")
                || !content.contains("registry.retain")
                || !content.contains("internal_egress_outcome_is_unknown"))
        {
            violations += 1;
            warnings.push(format!(
                "{}: Rust campaign sender is missing canonical EgressIntent handoff",
                file.path
            ));
            evidence.push(EvidenceItem {
                kind: "rust_campaign_missing_canonical_handoff".to_string(),
                path: file.path.clone(),
                line: None,
                detail: "campaign senders must use the canonical intent handoff, explicit approval, pre-dispatch delivery claims, content projection, and provider-result reconciliation"
                    .to_string(),
            });
        }
    }

    // Python user tokens and Rust authorization must project the same role
    // grants; otherwise a correctly protected handler becomes unusable (or a
    // caller gains a different permission set depending on token issuer).
    for (path, required_markers) in [
        (
            "example-api/example/auth/jwt.py",
            vec![
                "def _permissions_for_role",
                "\"permissions\": _permissions_for_role(role)",
                "\"messages:send\"",
                "\"cases:assign\"",
            ],
        ),
        (
            "example-gateway/src/auth/mod.rs",
            vec![
                "fn normalize_api_permissions",
                "analyst_permissions",
                "has_role(\"supervisor\")",
                "\"messages:send\"",
                "\"cases:assign\"",
            ],
        ),
    ] {
        if !index.files.iter().any(|file| file.path == path) {
            continue;
        }
        let Some(content) = read_text(&root.join(path), &mut Vec::new()) else {
            continue;
        };
        let missing = required_markers
            .iter()
            .filter(|marker| !content.contains(**marker))
            .copied()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            violations += 1;
            warnings.push(format!(
                "{path}: role-only JWT permission projection is incomplete (missing {})",
                missing.join(", ")
            ));
            evidence.push(EvidenceItem {
                kind: "egress_role_permission_drift".to_string(),
                path: path.to_string(),
                line: None,
                detail: "Python-issued role tokens must receive the same message-send and campaign-approval grants enforced by Rust".to_string(),
            });
        }
    }

    entities.push(json!({
        "violations": violations,
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_egress_compliance"),
        kind: "doctor".to_string(),
        summary: format!("checked egress compliance, found {} violations", violations),
        confidence: if warnings.is_empty() { 0.92 } else { 0.6 },
        entities,
        evidence,
        warnings,
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    }
}

fn is_test_path(path: &str) -> bool {
    path.contains("/tests/")
        || path.ends_with("_test.py")
        || path.ends_with("_tests.py")
        || path.ends_with("_test.rs")
        || path.ends_with("_tests.rs")
}

#[cfg(test)]
mod tests {
    use super::{doctor_egress_compliance, is_test_path};
    use crate::model::{FileRecord, RepoIndex, SourceLanguage};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_repo(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "leio-code-egress-{name}-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create temp repo");
        root
    }

    fn file_record(path: &str) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            language: SourceLanguage::Python,
            bytes: 0,
            modified_unix_ms: 0,
            symbols: Vec::new(),
            env_vars: Vec::new(),
            redis_keys: Vec::new(),
            subprocess_calls: Vec::new(),
            http_calls: Vec::new(),
            unresolved_edges: Vec::new(),
        }
    }

    fn rust_file_record(path: &str) -> FileRecord {
        FileRecord {
            language: SourceLanguage::Rust,
            ..file_record(path)
        }
    }

    fn index_with(paths: &[&str]) -> RepoIndex {
        RepoIndex {
            version: 1,
            root: "/tmp/repo".to_string(),
            indexed_at: "2026-01-01T00:00:00Z".to_string(),
            files: paths.iter().map(|path| file_record(path)).collect(),
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        }
    }

    fn index_with_records(files: Vec<FileRecord>) -> RepoIndex {
        RepoIndex {
            version: 1,
            root: "/tmp/repo".to_string(),
            indexed_at: "2026-01-01T00:00:00Z".to_string(),
            files,
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        }
    }

    fn write(root: &Path, path: &str, body: &str) {
        let full_path = root.join(path);
        fs::create_dir_all(full_path.parent().expect("test file parent")).expect("create parent");
        fs::write(full_path, body).expect("write test file");
    }

    #[test]
    fn audited_egress_helpers_do_not_count_as_direct_sends() {
        let root = temp_repo("helpers");
        let path = "cartridges/insurance_agent/agent.py";
        write(
            &root,
            path,
            r#"
from example.agents.tools import build_whatsapp_send_tool
from example.egress.firewall import send_whatsapp_response

async def run():
    tool = build_whatsapp_send_tool()
    return await send_whatsapp_response(agent_name="agent", tenant_id="t", user_phone="1", message="ok")
"#,
        );

        let result = doctor_egress_compliance(&index_with(&[path]), &root);
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn direct_smtp_usage_in_cartridge_is_reported() {
        let root = temp_repo("smtp");
        let path = "cartridges/sisfron/alert_notifier.py";
        write(
            &root,
            path,
            r#"
import smtplib

def send(msg):
    smtp = smtplib.SMTP("smtp.example.test")
    smtp.send_message(msg)
"#,
        );

        let result = doctor_egress_compliance(&index_with(&[path]), &root);
        assert_eq!(result.warnings.len(), 3, "{:?}", result.warnings);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_paths_are_not_reported_as_live_egress() {
        assert!(is_test_path(
            "cartridges/sisfron/tests/test_alert_notifier.py"
        ));
        assert!(is_test_path("cartridges/sisfron/alert_notifier_test.py"));
        assert!(!is_test_path("cartridges/sisfron/alert_notifier.py"));
    }

    #[test]
    fn rust_campaign_direct_meta_sends_are_reported() {
        let root = temp_repo("rust-campaign-direct");
        let path = "example-gateway/src/ops_console/routes/campaigns.rs";
        write(
            &root,
            path,
            r#"
async fn send_campaign_for_tenant() {
    let url = "https://graph.facebook.com/v23.0/123/messages";
    meta_post_json(url, "token", &body).await;
}
"#,
        );

        let result =
            doctor_egress_compliance(&index_with_records(vec![rust_file_record(path)]), &root);
        assert_eq!(result.warnings.len(), 3, "{:?}", result.warnings);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rust_mcp_direct_meta_sends_are_reported() {
        let root = temp_repo("rust-mcp-direct");
        let path = "example-gateway/src/integrations/tools/messaging/whatsapp.rs";
        write(
            &root,
            path,
            r#"
async fn send_template(ctx: &ToolContext) {
    let token = resolve_access_token(ctx, &args).await?;
    let url = "https://graph.facebook.com/v23.0/123/messages";
    client.post(url).bearer_auth(token).json(&payload).send().await?;
}
"#,
        );

        let result =
            doctor_egress_compliance(&index_with_records(vec![rust_file_record(path)]), &root);
        assert!(
            result
                .evidence
                .iter()
                .any(|item| item.kind == "rust_mcp_direct_provider_egress"),
            "{:?}",
            result.warnings
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rust_campaign_internal_handoff_is_clean() {
        let root = temp_repo("rust-campaign-handoff");
        let path = "example-gateway/src/ops_console/routes/campaigns.rs";
        write(
            &root,
            path,
            r#"
async fn send_campaign_for_tenant() {
    let intent = EgressIntent { /* fields */ };
    let initial_status = "draft";
    let campaign_delivery_claim = true;
    let record_campaign_delivery_egress_result = true;
    let metadata = json!({"source": "campaign_worker"});
    let intent = EgressIntent { draft_response: gate_content };
    principal_can_access_campaign_tenant(&principal, "tenant");
    let permission = "messages:send permission required";
    let approval = "cases:assign permission required";
    let patch = "Campaign status cannot be changed through generic PATCH";
    let claim = "status = 'draft'";
    registry.retain(|item| true);
    internal_egress_outcome_is_unknown("error");
    submit_internal_outbound_intent(&state, intent, "campaign-worker").await;
}
"#,
        );

        let result =
            doctor_egress_compliance(&index_with_records(vec![rust_file_record(path)]), &root);
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);

        let _ = fs::remove_dir_all(root);
    }
}
