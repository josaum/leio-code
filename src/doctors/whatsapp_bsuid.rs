//! WhatsApp Business-scoped user ID (BSUID) contract doctors.
//!
//! Meta's username/BSUID rollout makes the customer phone number optional on
//! webhook payloads. These doctors keep the repo from drifting back to
//! phone-only identity assumptions in the WhatsApp hot path and CRM mirrors.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct WhatsAppBsuidWebhookDoctor;
pub struct WhatsAppBsuidCrmDoctor;
pub struct WhatsAppDisplayNameOnlyDoctor;

impl Doctor for WhatsAppBsuidWebhookDoctor {
    fn name(&self) -> &'static str {
        "whatsapp-bsuid-webhook"
    }

    fn description(&self) -> &'static str {
        "Checks that WhatsApp webhook/ingest/status paths preserve BSUID, username, parent BSUID, and phone-optional identity fields across Python and Rust."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_whatsapp_bsuid_webhook(root)
    }
}

impl Doctor for WhatsAppBsuidCrmDoctor {
    fn name(&self) -> &'static str {
        "whatsapp-bsuid-crm"
    }

    fn description(&self) -> &'static str {
        "Checks that Chatwoot/CRM mirrors keep customer identity separate from customer_phone and do not synthesize phone_number values from BSUIDs."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_whatsapp_bsuid_crm(root)
    }
}

impl Doctor for WhatsAppDisplayNameOnlyDoctor {
    fn name(&self) -> &'static str {
        "whatsapp-display-name-only"
    }

    fn description(&self) -> &'static str {
        "Checks that Meta Embedded Signup can discover and persist a technical sender when the browser emits a WABA without phone_number_id."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_whatsapp_display_name_only(root)
    }
}

struct Check<'a> {
    path: &'a str,
    label: &'a str,
    needles: &'a [&'a str],
    forbidden: &'a [&'a str],
}

pub fn doctor_whatsapp_bsuid_webhook(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let checks = [
        Check {
            path: "example-api/example/integrations/whatsapp/routers/webhook.py",
            label: "Python webhook extracts BSUID identity and status recipient fallback",
            needles: &[
                "def _extract_message_identity",
                "def _extract_status_identity",
                "def _status_recipient_identifier",
                "message.get(\"from_user_id\")",
                "message.get(\"from_parent_user_id\")",
                "identity.get(\"from_user_id\")",
                "identity.get(\"recipient_user_id\")",
                "identity[\"customer_identity\"]",
                "\"paid_messaging_account_id\"",
                "\"request_contact_info\"",
                "stats[\"status_updates\"] += _emit_status_updates",
            ],
            forbidden: &[],
        },
        Check {
            path: "example-api/example/integrations/whatsapp/events.py",
            label: "Canonical inbound event models phone-optional customer identity",
            needles: &[
                "user_phone: str  # customer WhatsApp identifier; may be phone, BSUID, or parent BSUID",
                "customer_identity: str | None = None",
                "customer_phone: str | None = None",
                "identity: dict[str, Any] | None = None",
            ],
            forbidden: &["user_phone: str  # customer phone"],
        },
        Check {
            path: "example-gateway/src/whatsapp/identity.rs",
            label: "Rust shared WhatsApp identity extractor preserves BSUID fields",
            needles: &[
                "pub fn business_scoped_identity",
                "(\"from_user_id\", \"from_user_id\")",
                "(\"from_parent_user_id\", \"from_parent_user_id\")",
                "(\"recipient_user_id\", \"recipient_user_id\")",
                "(\"recipient_parent_user_id\", \"recipient_parent_user_id\")",
                "(\"paid_messaging_account_id\", \"paid_messaging_account_id\")",
                "(\"wa_id\", \"contact_wa_id\")",
                "contact_username",
                "extracts_business_scoped_identity_from_messages_and_contacts",
                "extracts_business_scoped_identity_from_status_receipts",
            ],
            forbidden: &[],
        },
        Check {
            path: "example-gateway/src/channels/whatsapp.rs",
            label: "Rust channel normalization accepts from_user_id/from_parent_user_id fallback",
            needles: &[
                "business_scoped_identity",
                "&[\"from\", \"from_user_id\", \"from_parent_user_id\"]",
                "metadata.insert(\"identity\".to_string(), identity)",
                "request_contact_info",
                "test_normalize_whatsapp_webhook_preserves_business_scoped_identity",
            ],
            forbidden: &[],
        },
        Check {
            path: "example-gateway/src/ingest/whatsapp/mod.rs",
            label: "Rust ingest preserves BSUID identity into agent events",
            needles: &[
                "business_scoped_identity",
                "&[\"from\", \"from_user_id\", \"from_parent_user_id\"]",
                "obj.insert(\"identity\".to_string(), identity_value)",
                "obj.insert(\"_identity\".to_string(), identity_value)",
                "normalized_inbound_preserves_business_scoped_identity_without_phone_from",
            ],
            forbidden: &[],
        },
        Check {
            path: "example-gateway/src/ingest/whatsapp/status.rs",
            label: "Rust status ingest falls back to recipient_user_id and emits identity",
            needles: &[
                "fn status_recipient_identifier",
                "\"recipient_user_id\"",
                "\"recipient_parent_user_id\"",
                "business_scoped_identity(value, Some(status))",
                "obj.insert(\"identity\".to_string(), identity_value)",
                "status_recipient_identifier_falls_back_to_business_scoped_ids",
            ],
            forbidden: &[],
        },
        Check {
            path: "example-api/example/tests/api/test_whatsapp_changelog_2026.py",
            label: "Python regression coverage for Meta 2026 BSUID changelog",
            needles: &[
                "test_webhook_promotes_business_scoped_identity_fields_to_agent_event",
                "test_status_only_webhook_emits_business_scoped_delivery_identity_without_dispatch",
                "\"from_user_id\": \"bsuid-from-1\"",
                "\"recipient_user_id\": \"bsuid-recipient-1\"",
                "\"customer_identity\": \"bsuid-from-1\"",
                "\"request_contact_info\"",
            ],
            forbidden: &[],
        },
    ];

    run_checks(
        root,
        started,
        "doctor_whatsapp_bsuid_webhook",
        "whatsapp-bsuid-webhook",
        "checked WhatsApp BSUID webhook/ingest contracts",
        &checks,
    )
}

pub fn doctor_whatsapp_bsuid_crm(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let checks = [
        Check {
            path: "example-api/example/integrations/chatwoot_sync.py",
            label: "Chatwoot separates customer identity from real phone number",
            needles: &[
                "def _looks_like_phone_identifier",
                "def _event_customer_identity",
                "def _event_customer_phone",
                "return f\"whatsapp-bsuid:{_identity_digest(identifier)}\"",
                "return f\"wa-bsuid:{_identity_digest(identifier)}\"",
                "contact_identifier = customer_phone or customer_identity",
                "phone_number=customer_phone",
                "if actual_phone:",
                "\"whatsapp_user_identifier\"",
                "\"missing_session_or_customer_identity\"",
            ],
            forbidden: &[
                "\"phone_number\": phone",
                "f\"+{phone}\"",
                "\"whatsapp_phone\": phone",
            ],
        },
        Check {
            path: "example-api/example/agents/tasks.py",
            label: "Plusoft retained history only emits phone1 from explicit customer_phone",
            needles: &[
                "normalized_event.get(\"customer_phone\")",
                "normalized_event.get(\"contact_wa_id\")",
                "user_phone_digits = \"\".join(ch for ch in customer_phone if ch.isdigit())",
                "user_data[\"phone1\"] = user_phone_digits",
            ],
            forbidden: &["user_phone_digits = \"\".join(ch for ch in str(user_phone or \"\")"],
        },
        Check {
            path: "example-api/example/tests/api/test_ops_console_tenant_normalization.py",
            label: "Chatwoot tests cover BSUID contacts without fake phone_number",
            needles: &[
                "test_chatwoot_contact_uses_bsuid_identifier_without_fake_phone",
                "test_chatwoot_sync_uses_bsuid_identifier_without_fake_phone",
                "assert \"phone_number\" not in posted[\"payload\"]",
                "assert \"phone_number\" not in contact_payload",
                "wa-bsuid:",
                "whatsapp-bsuid:",
                "\"whatsapp_user_identifier\": \"BR.12345678901234567890\"",
            ],
            forbidden: &[],
        },
    ];

    run_checks(
        root,
        started,
        "doctor_whatsapp_bsuid_crm",
        "whatsapp-bsuid-crm",
        "checked WhatsApp BSUID CRM/contact contracts",
        &checks,
    )
}

pub fn doctor_whatsapp_display_name_only(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let checks = [
        Check {
            path: "example-api/example/integrations/whatsapp/routers/waba.py",
            label: "Control plane discovers exactly one Meta sender with the fresh business token",
            needles: &[
                "async def _discover_embedded_signup_sender",
                "create_whatsapp_client(",
                "token=access_token",
                "response = await discovery_client.phone_numbers.list",
                "if len(senders) != 1:",
                "Meta returned multiple sender identities; phone_number_id is required",
                "if not waba_id:",
                "Meta token exchange did not return a WABA id",
            ],
            forbidden: &[],
        },
        Check {
            path: "example-api/example/integrations/whatsapp/routers/waba.py",
            label: "Discovered sender becomes a paused tenant-owned channel and runtime projection",
            needles: &[
                "def _upsert_embedded_signup_sender",
                "mode=\"paused\"",
                "\"display_name_only\": display_name_only",
                "Meta sender identity is already assigned to tenant",
                "_sync_embedded_signup_projection(request.tenant_id, phone_number_id)",
            ],
            forbidden: &["mode=\"agent\""],
        },
        Check {
            path: "example-api/example/integrations/whatsapp/routers/waba.py",
            label: "Embedded Signup projects tenant credentials into the Rust campaign data plane",
            needles: &[
                "async def _sync_embedded_signup_gateway_credentials",
                "f\"{gateway_url}/api/whatsapp/credentials\"",
                "namespace=tenant_id",
                "\"gateway_credentials_stored\"] = True",
            ],
            forbidden: &[],
        },
        Check {
            path: "example-gateway/src/whatsapp/phone_numbers.rs",
            label: "Rust credential projection creates tenant parents atomically and uses supported Meta fields",
            needles: &[
                "fn persist_credentials_projection",
                "ensure_tenant_projection(&tx, &record.tenant_id)?;",
                "upsert_phone_number(&tx, record)?;",
                "authorize_credentials_tenant(&user, &input.tenant_id)",
                "fn phone_metadata_lookup_uses_supported_graph_fields",
                "fn credential_projection_is_tenant_scoped_and_permission_gated",
            ],
            forbidden: &["code_verification_status,whatsapp_business_account"],
        },
        Check {
            path: "example-gateway/src/ops_console/routes/campaigns.rs",
            label: "Campaign registry exposes outbound sender authority from the same runtime ownership projection used at create time",
            needles: &[
                "pub campaign_sender_registered: bool",
                "fn campaign_sender_is_registered_for_tenant",
                "find_phone_route_owner_async(state.clone(), &item.phone_number_id)",
                "campaign_sender_is_registered_for_tenant(item, owner.as_ref())",
                "find_phone_route_owner_async(state.clone(), &validated.phone_number_id)",
            ],
            forbidden: &[],
        },
        Check {
            path: "example-ops/src/app/api/gateway/resolve-target.ts",
            label: "Campaign WABA discovery stays on the Rust aggregate with the registry",
            needles: &[
                "pattern: /^ops_console\\/api\\/whatsapp-registry$/",
                "pattern: /^ops_console\\/api\\/wabas\\/[^/]+$/",
            ],
            forbidden: &["rewrite: (match) => `ops/api/wabas/${match[1]}`"],
        },
        Check {
            path: "example-ops/src/app/api/whatsapp/embedded-signup/route.ts",
            label: "Ops BFF accepts WABA-only browser events but requires the resolved sender before success",
            needles: &[
                "if (!wabaId)",
                "phone_number_id?: string;",
                "const resolvedPhoneNumberId = phoneNumberId ?? nonEmpty(exchange?.phone_number_id);",
                "!resolvedPhoneNumberId",
                "phoneNumberId: resolvedPhoneNumberId",
            ],
            forbidden: &["if (!wabaId || !phoneNumberId)"],
        },
        Check {
            path: "example-ops/src/components/tenants/onboarding/embedded-signup.tsx",
            label: "Ops accepts legacy and current Meta completion events without forcing phone-less registration",
            needles: &[
                "data?.event === 'FINISH' || data?.event === 'FINISH_ONLY_WABA'",
                "sessionInfoVersion: '3'",
            ],
            forbidden: &[
                "data?.event === 'FINISH')",
                "featureType: 'only_waba_sharing'",
            ],
        },
        Check {
            path: "example-ops/src/hooks/campaigns/useCampaignQueries.ts",
            label: "Campaign sender discovery intersects Meta phones with explicit outbound campaign authority",
            needles: &[
                "function getCampaignSenderEligibility",
                "entry.tenant_id === tenantId",
                "entry.waba_id === wabaId",
                "entry.phone_number_id === phoneNumberId",
                "entry.campaign_sender_registered === true",
                "const isOperationallyConnected = normalizedStatus === 'CONNECTED'",
                "const isCodeVerified = normalizedVerificationStatus === 'VERIFIED'",
                "if (hasMetaReadinessSignal && !isOperationallyConnected && !isCodeVerified)",
                "enabled: !!selectedTenant && !isLoadingRegistry",
            ],
            forbidden: &[],
        },
        Check {
            path: "example-ops/src/components/campaigns/wizard-steps/PhoneSelectionStep.tsx",
            label: "Campaign sender UI disables ineligible Meta phones and explains why",
            needles: &[
                "disabled={!phone.eligible}",
                "phone.ineligibleReason",
                "aria-disabled={!phone.eligible}",
            ],
            forbidden: &[],
        },
        Check {
            path: "example-ops/src/hooks/campaigns/useCampaignForm.ts",
            label: "Campaign submission gate rejects stale or manipulated sender selections",
            needles: &[
                "const selectedSender = data.selectedWaba?.phoneNumbers.find",
                "if (!selectedSender?.eligible)",
            ],
            forbidden: &[],
        },
        Check {
            path: "example-ops/src/hooks/campaigns/__tests__/useCampaignQueries.meta-wabas.test.tsx",
            label: "Campaign sender regression mirrors the C4 multi-phone route drift",
            needles: &[
                "only enables the C4 phone registered for the tenant and verified by Meta",
                "1319115564608417",
                "1264137613449382",
                "1199738853231458",
                "Linha não autorizada para campanhas neste tenant.",
            ],
            forbidden: &[],
        },
        Check {
            path: "example-api/example/tests/api/test_waba_display_name_only_signup.py",
            label: "Regression tests cover discovery, ambiguity, legacy sender, paused mode, and tenant theft",
            needles: &[
                "test_display_name_only_signup_discovers_meta_provided_sender",
                "test_display_name_only_signup_rejects_ambiguous_sender_discovery",
                "test_display_name_only_signup_rejects_missing_meta_sender",
                "test_explicit_sender_signup_does_not_run_discovery",
                "test_display_name_only_sender_is_persisted_paused_without_e164",
                "test_embedded_signup_cannot_steal_sender_from_another_tenant",
                "test_embedded_signup_syncs_credentials_to_rust_gateway",
            ],
            forbidden: &[],
        },
    ];

    run_checks(
        root,
        started,
        "doctor_whatsapp_display_name_only",
        "whatsapp-display-name-only",
        "checked WhatsApp display-name-only Embedded Signup contracts",
        &checks,
    )
}

fn run_checks(
    root: &Path,
    started: Instant,
    query_prefix: &str,
    doctor_name: &str,
    summary_prefix: &str,
    checks: &[Check<'_>],
) -> QueryEnvelope {
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut entities = Vec::new();
    let mut passed = 0usize;

    for check in checks {
        let full_path = root.join(check.path);
        let mut io_warnings = Vec::new();
        let Some(body) = read_text(&full_path, &mut io_warnings) else {
            warnings.push(format!(
                "{} missing or unreadable: {}",
                check.label, check.path
            ));
            warnings.extend(io_warnings);
            entities.push(json!({
                "doctor": doctor_name,
                "path": check.path,
                "label": check.label,
                "passed": false,
                "missing_file": true,
            }));
            continue;
        };

        let missing = check
            .needles
            .iter()
            .filter(|needle| !body.contains(**needle))
            .copied()
            .collect::<Vec<_>>();
        let forbidden_hits = check
            .forbidden
            .iter()
            .filter(|needle| body.contains(**needle))
            .copied()
            .collect::<Vec<_>>();

        if missing.is_empty() && forbidden_hits.is_empty() {
            passed += 1;
            if let Some(first_needle) = check.needles.first() {
                evidence.push(EvidenceItem {
                    kind: doctor_name.to_string(),
                    path: check.path.to_string(),
                    line: find_line(&body, first_needle),
                    detail: check.label.to_string(),
                });
            }
        }

        if !missing.is_empty() {
            warnings.push(format!(
                "{} drift in {}: missing {}",
                check.label,
                check.path,
                missing.join(", ")
            ));
        }
        if !forbidden_hits.is_empty() {
            warnings.push(format!(
                "{} drift in {}: forbidden {}",
                check.label,
                check.path,
                forbidden_hits.join(", ")
            ));
        }

        entities.push(json!({
            "doctor": doctor_name,
            "path": check.path,
            "label": check.label,
            "passed": missing.is_empty() && forbidden_hits.is_empty(),
            "required_anchors": check.needles.len(),
            "forbidden_anchors": check.forbidden.len(),
            "missing": missing,
            "forbidden_hits": forbidden_hits,
        }));
    }

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id(query_prefix),
        kind: "doctor".to_string(),
        summary: format!(
            "{summary_prefix}, found {} warnings ({passed}/{total} surfaces passed)",
            warnings.len(),
            total = checks.len()
        ),
        confidence: if warnings.is_empty() { 0.95 } else { 0.62 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "doctor": doctor_name,
            "checks_passed": passed,
            "checks_total": checks.len(),
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}
