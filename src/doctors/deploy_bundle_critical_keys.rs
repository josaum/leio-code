//! Deploy-bundle critical-keys doctor.
//!
//! Catches the **empty-bundle-key clobber class** that has now caused three
//! production incidents on the `customer_ops_unified` target:
//!
//! - 2026-05-27: `PACTO_WEBHOOK_CHAVES` empty across the deploy chain →
//!   100% Pacto webhook rejection (covered by
//!   `pacto-webhook-allowlist-populated` for committed files).
//! - 2026-06-10: a deploy rsynced the LOCAL secret bundle (chaves empty)
//!   over the on-VM secrets layer → same outage, different vector.
//! - 2026-06-11: the bundle was regenerated with `INFOBIP_API_BASE_URL` and
//!   `INFOBIP_SCENARIO_KEY` empty; the next deploy blanked them in prod →
//!   every `sara_assurant` WhatsApp outbound failed at gateway dispatch
//!   ("Missing Infobip configuration") while inbound kept flowing.
//!
//! The common mechanism: `deploy/secret-sets/<target>.env.local` is the
//! deploy-material source on the operator machine and is rsynced over the
//! VM's secret layer on every deploy. An empty critical key in that file is
//! a scheduled outage, not a local detail.
//!
//! This doctor is the deploy-machine preflight the committed-file doctors
//! cannot be: `secret-set-parity` audits declarations only (values out of
//! scope) and `pacto-webhook-allowlist-populated` skips gitignored bundles
//! by design. Here the gitignored local bundle is exactly the audit target.
//!
//! Rules (narrow, incident-driven):
//! 1. When the bundle file exists on this machine, every key in
//!    [`CRITICAL_KEYS`] must be declared with a non-empty value. Missing and
//!    empty are the same failure: the render leaves the var blank in prod.
//! 2. Any declared value that looks like JSON (`{`/`[`, bare or
//!    double-quoted) must be single-quote wrapped. The render bash-`source`s
//!    bundles, so unquoted JSON loses its inner double quotes and parses as
//!    garbage downstream (the `PACTO_WEBHOOK_CHAVES` mangle trap).
//! 3. Every other material local secret bundle whose template declares
//!    `PACTO_WEBHOOK_CHAVES` must carry a non-empty alias-to-hex32 map. Outbound
//!    Pacto API tokens are rejected at this inbound boundary.
//! 4. When the primary bundle file is absent (CI, non-deploy machines) the doctor
//!    skips cleanly — there is nothing material to audit.
//!
//! Secrecy: warnings and evidence carry key names, files, and line numbers
//! only. Values are never echoed into the report.

use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::query_id;
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct DeployBundleCriticalKeysDoctor;

impl Doctor for DeployBundleCriticalKeysDoctor {
    fn name(&self) -> &'static str {
        "deploy-bundle-critical-keys"
    }

    fn description(&self) -> &'static str {
        "Deploy-machine preflight for local secret bundles: every critical key in customer_ops_unified must be non-empty; every Pacto-enabled bundle must carry an alias-to-hex32 webhook map rather than outbound API tokens; JSON values must be single-quote wrapped. Skips when the gitignored primary bundle is absent."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_deploy_bundle_critical_keys(root)
    }
}

/// The audited bundle, relative to the workspace root.
///
/// `customer_ops_unified` is the only target whose bundle has caused
/// outages so far; add `(path, keys)` pairs here when another bundle bites.
const BUNDLE_PATH: &str = "deploy/secret-sets/customer_ops_unified.env.local";

/// Keys that must be non-empty in the bundle for prod to survive a deploy.
///
/// Every entry traces to a real failure mode:
/// - `JWT_SECRET`: the only `:?`-required compose var; blank kills compose up.
/// - `PACTO_WEBHOOK_CHAVES`: empty → 100% Pacto webhook rejection
///   (2026-05-27, 2026-06-10).
/// - `INFOBIP_*`: blank base_url/scenario_key → every `sara_assurant`
///   outbound fails at gateway dispatch (2026-06-11); basic-auth pair is the
///   resolved auth header for the same dispatch path.
/// - `PLUSOFT_API_*` + `PLUSOFT_HANDOVER_BASE_URL`: campaign lookup and
///   handover fail silently (fail-open retain) without them.
/// - `META_ACCESS_TOKEN`: Meta egress for liz/pratique lines; prod gateway
///   falls back to this env var because the vault secret is absent.
/// - `EXAMPLE_FLIGHT_TENANT_HMAC_SECRET`: missing or shorter than 32 bytes
///   prevents the API's generated service JWT from binding to an opaque tenant,
///   causing the authenticated Flight watchdog to recycle Gunicorn workers.
const CRITICAL_KEYS: &[&str] = &[
    "JWT_SECRET",
    "EXAMPLE_FLIGHT_TENANT_HMAC_SECRET",
    "PACTO_WEBHOOK_CHAVES",
    "INFOBIP_API_BASE_URL",
    "INFOBIP_SCENARIO_KEY",
    "INFOBIP_BASIC_AUTH_USERNAME",
    "INFOBIP_BASIC_AUTH_PASSWORD",
    "PLUSOFT_API_BASE_URL",
    "PLUSOFT_API_USERNAME",
    "PLUSOFT_API_PASSWORD",
    "PLUSOFT_HANDOVER_BASE_URL",
    "META_ACCESS_TOKEN",
];

pub fn doctor_deploy_bundle_critical_keys(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut entities = Vec::new();
    let mut evidence = Vec::new();

    audit_pacto_local_bundles(root, &mut warnings, &mut entities, &mut evidence);

    let bundle = root.join(BUNDLE_PATH);
    let contents = match std::fs::read_to_string(&bundle) {
        Ok(s) => s,
        Err(_) => {
            let summary = if warnings.is_empty() {
                format!(
                    "deploy-bundle-critical-keys: {BUNDLE_PATH} not present — doctor skipped (not a deploy machine)"
                )
            } else {
                format!(
                    "deploy-bundle-critical-keys: {} warning(s) in material Pacto secret bundles; {BUNDLE_PATH} not present",
                    warnings.len()
                )
            };
            return QueryEnvelope {
                schema_version: crate::model::SCHEMA_VERSION.to_string(),
                query_id: query_id("doctor_deploy_bundle_critical_keys"),
                kind: "doctor".to_string(),
                summary,
                confidence: if warnings.is_empty() { 0.95 } else { 0.9 },
                entities,
                evidence,
                warnings,
                meta: Some(json!({"reason": "bundle_absent", "bundle": BUNDLE_PATH})),
                timing_ms: started.elapsed().as_millis(),
            };
        }
    };

    // First pass: collect declaration state per key and check JSON quoting on
    // every declaration (not just critical keys — the mangle trap is generic).
    let mut declared_non_empty: Vec<&str> = Vec::new();
    let mut declared_empty: Vec<(&str, usize)> = Vec::new();
    let mut declared_invalid: Vec<(&str, usize, &str)> = Vec::new();
    let mut meta_app_secret: Option<(String, usize)> = None;
    let mut whatsapp_app_secret: Option<(String, usize)> = None;
    for (idx, line) in contents.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() || key.contains(char::is_whitespace) {
            continue;
        }
        let unquoted_value = value.trim_matches('"').trim_matches('\'').to_string();
        match key {
            "META_APP_SECRET" if !unquoted_value.is_empty() => {
                meta_app_secret = Some((unquoted_value.clone(), idx + 1));
            }
            "WHATSAPP_APP_SECRET" if !unquoted_value.is_empty() => {
                whatsapp_app_secret = Some((unquoted_value.clone(), idx + 1));
            }
            _ => {}
        }

        if value_is_unsafely_quoted_json(value) {
            warnings.push(format!(
                "{key} ({BUNDLE_PATH}:{line_no}): JSON value is not single-quote wrapped — the render bash-sources bundles and strips double quotes, so this value reaches prod mangled. Use {key}='{{...}}'.",
                line_no = idx + 1
            ));
            entities.push(json!({
                "doctor": "deploy-bundle-critical-keys",
                "key": key,
                "file": BUNDLE_PATH,
                "line": idx + 1,
                "state": "json_not_single_quoted",
            }));
            evidence.push(EvidenceItem {
                kind: "json_not_single_quoted".to_string(),
                path: BUNDLE_PATH.to_string(),
                line: Some(idx + 1),
                detail: format!("`{key}=` JSON value must be single-quote wrapped"),
            });
        }

        if let Some(canonical) = CRITICAL_KEYS.iter().find(|k| **k == key) {
            if value.is_empty() {
                declared_empty.push((*canonical, idx + 1));
            } else if key == "EXAMPLE_FLIGHT_TENANT_HMAC_SECRET" && unquoted_value.len() < 32 {
                declared_invalid.push((*canonical, idx + 1, "shorter than 32 bytes"));
            } else if key == "PACTO_WEBHOOK_CHAVES" && !pacto_webhook_map_is_valid(&unquoted_value)
            {
                declared_invalid.push((
                    *canonical,
                    idx + 1,
                    "not a non-empty alias-to-hex32 webhook map",
                ));
            } else {
                declared_non_empty.push(*canonical);
            }
        }
    }

    if let (Some((whatsapp_secret, whatsapp_line)), Some((meta_secret, meta_line))) =
        (&whatsapp_app_secret, &meta_app_secret)
        && whatsapp_secret != meta_secret
    {
        warnings.push(format!(
            "WHATSAPP_APP_SECRET ({BUNDLE_PATH}:{whatsapp_line}) differs from META_APP_SECRET ({BUNDLE_PATH}:{meta_line}) — the API verifies Meta webhooks with WHATSAPP_APP_SECRET first, so an old value rejects Liz/Pratique/Sara inbound messages."
        ));
        entities.push(json!({
            "doctor": "deploy-bundle-critical-keys",
            "key": "WHATSAPP_APP_SECRET",
            "file": BUNDLE_PATH,
            "line": whatsapp_line,
            "state": "meta_secret_mismatch",
        }));
        evidence.push(EvidenceItem {
            kind: "meta_webhook_secret_mismatch".to_string(),
            path: BUNDLE_PATH.to_string(),
            line: Some(*whatsapp_line),
            detail: "`WHATSAPP_APP_SECRET` must match `META_APP_SECRET` when both are declared"
                .to_string(),
        });
    }

    // Second pass: every critical key must be present and non-empty. A key
    // that is declared empty AND missing entirely fail identically in prod,
    // so both states warn with the same severity.
    for key in CRITICAL_KEYS {
        if declared_non_empty.contains(key) {
            continue;
        }
        let (state, line) =
            if let Some((_, line, state)) = declared_invalid.iter().find(|(k, _, _)| k == key) {
                (*state, Some(*line))
            } else {
                declared_empty
                    .iter()
                    .find(|(k, _)| k == key)
                    .map_or(("missing", None), |(_, line)| ("empty", Some(*line)))
            };
        warnings.push(format!(
            "{key} is {state} in {BUNDLE_PATH} — the next deploy rsyncs this bundle over the VM secret layer and invalidates critical production runtime configuration."
        ));
        entities.push(json!({
            "doctor": "deploy-bundle-critical-keys",
            "key": key,
            "file": BUNDLE_PATH,
            "line": line,
            "state": state,
        }));
        evidence.push(EvidenceItem {
            kind: format!("critical_key_{state}"),
            path: BUNDLE_PATH.to_string(),
            line,
            detail: format!("`{key}` must be non-empty in the deploy bundle"),
        });
    }

    let summary = if warnings.is_empty() {
        format!(
            "deploy-bundle-critical-keys: all {} critical keys populated in {BUNDLE_PATH}",
            CRITICAL_KEYS.len()
        )
    } else {
        format!(
            "deploy-bundle-critical-keys: {} warning(s) — next deploy will blank critical prod config",
            warnings.len()
        )
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_deploy_bundle_critical_keys"),
        kind: "doctor".to_string(),
        summary,
        confidence: if warnings.is_empty() { 0.95 } else { 0.9 },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "bundle": BUNDLE_PATH,
            "critical_keys": CRITICAL_KEYS,
            "populated": declared_non_empty,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

/// Audit all material non-primary secret bundles that declare a Pacto webhook
/// key map in their checked-in template. This runs even when the primary
/// customer-ops bundle is absent, because Academias deploys from its own
/// `collections_platform` secret set.
fn audit_pacto_local_bundles(
    root: &Path,
    warnings: &mut Vec<String>,
    entities: &mut Vec<serde_json::Value>,
    evidence: &mut Vec<EvidenceItem>,
) {
    let secret_set_dir = root.join("deploy/secret-sets");
    if let Ok(entries) = std::fs::read_dir(&secret_set_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if !name.ends_with(".env.local") || name == "customer_ops_unified.env.local" {
                continue;
            }
            let template_name = name.trim_end_matches(".local").to_string() + ".example";
            let template = secret_set_dir.join(template_name);
            let template_declares_pacto =
                std::fs::read_to_string(&template)
                    .ok()
                    .is_some_and(|value| {
                        value
                            .lines()
                            .any(|line| line.trim_start().starts_with("PACTO_WEBHOOK_CHAVES="))
                    });
            if !template_declares_pacto {
                continue;
            }
            let Ok(other_contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let declaration = other_contents.lines().enumerate().find_map(|(idx, line)| {
                let trimmed = line.trim_start();
                if trimmed.starts_with('#') {
                    return None;
                }
                trimmed
                    .strip_prefix("PACTO_WEBHOOK_CHAVES=")
                    .map(|value| (value.trim(), idx + 1))
            });
            let (state, line) = match declaration {
                None => ("missing", None),
                Some(("", line)) => ("empty", Some(line)),
                Some((value, line))
                    if !pacto_webhook_map_is_valid(value.trim_matches('"').trim_matches('\'')) =>
                {
                    ("not a non-empty alias-to-hex32 webhook map", Some(line))
                }
                Some(_) => continue,
            };
            warnings.push(format!(
                "PACTO_WEBHOOK_CHAVES is {state} in {rel} — outbound Pacto API credentials must never authenticate inbound webhooks."
            ));
            entities.push(json!({
                "doctor": "deploy-bundle-critical-keys",
                "key": "PACTO_WEBHOOK_CHAVES",
                "file": rel,
                "line": line,
                "state": state,
            }));
            evidence.push(EvidenceItem {
                kind: "pacto_webhook_key_boundary".to_string(),
                path: rel,
                line,
                detail: "`PACTO_WEBHOOK_CHAVES` must be a non-empty alias-to-hex32 webhook map"
                    .to_string(),
            });
        }
    }
}

/// Returns `true` when a declared value carries JSON that bash-`source`
/// would mangle: bare `{`/`[` or double-quoted JSON. Single-quote wrapped
/// values are safe and accepted.
fn value_is_unsafely_quoted_json(value: &str) -> bool {
    let bare_json = value.starts_with('{') || value.starts_with('[');
    let double_quoted_json = value.starts_with("\"{") || value.starts_with("\"[");
    bare_json || double_quoted_json
}

fn pacto_webhook_map_is_valid(value: &str) -> bool {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str(value) else {
        return false;
    };
    !map.is_empty()
        && map.iter().all(|(alias, chave)| {
            !alias.trim().is_empty()
                && chave.as_str().is_some_and(|token| {
                    token.len() == 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
        })
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
            "leio-code-bundle-crit-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }

    fn write_bundle(dir: &Path, contents: &str) {
        let path = dir.join(BUNDLE_PATH);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(path, contents).expect("write fixture");
    }

    fn fully_populated_bundle() -> String {
        let mut out = String::new();
        for key in CRITICAL_KEYS {
            if *key == "PACTO_WEBHOOK_CHAVES" {
                out.push_str(
                    "PACTO_WEBHOOK_CHAVES='{\"sao_jose\":\"0123456789abcdef0123456789abcdef\"}'\n",
                );
            } else if *key == "EXAMPLE_FLIGHT_TENANT_HMAC_SECRET" {
                out.push_str(
                    "EXAMPLE_FLIGHT_TENANT_HMAC_SECRET=0123456789abcdef0123456789abcdef\n",
                );
            } else {
                out.push_str(&format!("{key}=some-value\n"));
            }
        }
        out
    }

    #[test]
    fn skips_when_bundle_absent() {
        let dir = unique_tempdir("absent");
        let envelope = doctor_deploy_bundle_critical_keys(&dir);
        assert!(envelope.warnings.is_empty());
        assert!(envelope.summary.contains("skipped"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn passes_when_all_critical_keys_populated() {
        let dir = unique_tempdir("populated");
        write_bundle(&dir, &fully_populated_bundle());
        let envelope = doctor_deploy_bundle_critical_keys(&dir);
        assert!(
            envelope.warnings.is_empty(),
            "expected clean run, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flags_empty_critical_key() {
        let dir = unique_tempdir("empty-key");
        let bundle = fully_populated_bundle()
            .replace("INFOBIP_API_BASE_URL=some-value", "INFOBIP_API_BASE_URL=");
        write_bundle(&dir, &bundle);
        let envelope = doctor_deploy_bundle_critical_keys(&dir);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("INFOBIP_API_BASE_URL is empty")),
            "expected empty-key warning, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flags_missing_critical_key() {
        let dir = unique_tempdir("missing-key");
        let bundle: String = fully_populated_bundle()
            .lines()
            .filter(|l| !l.starts_with("INFOBIP_SCENARIO_KEY="))
            .map(|l| format!("{l}\n"))
            .collect();
        write_bundle(&dir, &bundle);
        let envelope = doctor_deploy_bundle_critical_keys(&dir);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("INFOBIP_SCENARIO_KEY is missing")),
            "expected missing-key warning, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flags_short_flight_tenant_hmac_without_echoing_it() {
        let dir = unique_tempdir("short-flight-hmac");
        let short_secret = "sensitive-short-value";
        let bundle = fully_populated_bundle().replace(
            "EXAMPLE_FLIGHT_TENANT_HMAC_SECRET=0123456789abcdef0123456789abcdef",
            &format!("EXAMPLE_FLIGHT_TENANT_HMAC_SECRET={short_secret}"),
        );
        write_bundle(&dir, &bundle);
        let envelope = doctor_deploy_bundle_critical_keys(&dir);
        let dump = format!("{:?} {:?}", envelope.warnings, envelope.evidence);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("EXAMPLE_FLIGHT_TENANT_HMAC_SECRET")
                    && w.contains("shorter than 32 bytes")),
            "expected short-secret warning, got: {:?}",
            envelope.warnings
        );
        assert!(
            !dump.contains(short_secret),
            "report must not echo secret values: {dump}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flags_bare_json_value() {
        let dir = unique_tempdir("bare-json");
        let bundle = fully_populated_bundle().replace(
            "PACTO_WEBHOOK_CHAVES='{\"sao_jose\":\"0123456789abcdef0123456789abcdef\"}'",
            "PACTO_WEBHOOK_CHAVES={\"sao_jose\":\"0123456789abcdef0123456789abcdef\"}",
        );
        write_bundle(&dir, &bundle);
        let envelope = doctor_deploy_bundle_critical_keys(&dir);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("not single-quote wrapped")),
            "expected quoting warning, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flags_pacto_api_token_masquerading_as_webhook_chave_without_echoing_it() {
        let dir = unique_tempdir("pacto-api-token");
        let api_token = "api-token-that-is-not-a-32-character-webhook-chave";
        let bundle =
            fully_populated_bundle().replace("0123456789abcdef0123456789abcdef", api_token);
        write_bundle(&dir, &bundle);
        let envelope = doctor_deploy_bundle_critical_keys(&dir);
        let dump = format!("{:?} {:?}", envelope.warnings, envelope.evidence);
        assert!(
            envelope.warnings.iter().any(|warning| {
                warning.contains("PACTO_WEBHOOK_CHAVES")
                    && warning.contains("alias-to-hex32 webhook map")
            }),
            "expected webhook-key shape warning, got: {:?}",
            envelope.warnings
        );
        assert!(
            !dump.contains(api_token),
            "report must not echo the Pacto credential: {dump}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flags_pacto_api_token_in_collections_platform_bundle() {
        let dir = unique_tempdir("collections-pacto-api-token");
        fs::create_dir_all(dir.join("deploy/secret-sets"))
            .expect("create collections secret-set directory");
        fs::write(
            dir.join("deploy/secret-sets/collections_platform.env.example"),
            "PACTO_WEBHOOK_CHAVES=\n",
        )
        .expect("write collections template");
        let collections_bundle = dir.join("deploy/secret-sets/collections_platform.env.local");
        fs::write(
            &collections_bundle,
            "PACTO_WEBHOOK_CHAVES='{\"salesianos\":\"outbound-api-token\"}'\n",
        )
        .expect("write collections bundle");

        let envelope = doctor_deploy_bundle_critical_keys(&dir);
        assert!(
            envelope.warnings.iter().any(|warning| {
                warning.contains("collections_platform.env.local")
                    && warning.contains("alias-to-hex32 webhook map")
            }),
            "expected collections webhook-key shape warning, got: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flags_meta_webhook_secret_mismatch() {
        let dir = unique_tempdir("meta-secret-mismatch");
        let bundle = fully_populated_bundle()
            + "META_APP_SECRET=current-meta-secret\n"
            + "WHATSAPP_APP_SECRET=old-whatsapp-secret\n";
        write_bundle(&dir, &bundle);
        let envelope = doctor_deploy_bundle_critical_keys(&dir);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|w| w.contains("WHATSAPP_APP_SECRET")
                    && w.contains("differs from META_APP_SECRET")),
            "expected meta secret mismatch warning, got: {:?}",
            envelope.warnings
        );
        let dump = format!("{:?} {:?}", envelope.warnings, envelope.evidence);
        assert!(
            !dump.contains("current-meta-secret") && !dump.contains("old-whatsapp-secret"),
            "report must not echo Meta webhook secret values: {dump}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn warnings_never_contain_values() {
        let dir = unique_tempdir("no-leak");
        let bundle = fully_populated_bundle().replace(
            "JWT_SECRET=some-value",
            "JWT_SECRET=super-secret-token-value",
        ) + "EXTRA_JSON={\"leak\":\"nope\"}\n";
        write_bundle(&dir, &bundle);
        let envelope = doctor_deploy_bundle_critical_keys(&dir);
        let dump = format!("{:?} {:?}", envelope.warnings, envelope.evidence);
        assert!(
            !dump.contains("super-secret-token-value") && !dump.contains("nope"),
            "report must not echo bundle values: {dump}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignores_comments() {
        let dir = unique_tempdir("comments");
        let bundle = format!("# INFOBIP_API_BASE_URL=\n{}", fully_populated_bundle());
        write_bundle(&dir, &bundle);
        let envelope = doctor_deploy_bundle_critical_keys(&dir);
        assert!(
            envelope.warnings.is_empty(),
            "comment lines must not count as declarations: {:?}",
            envelope.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
