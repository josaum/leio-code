use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

/// Continuous audit of the glosa-reason contract across the data→reasoner→defense
/// pipeline. Three drifts would each silently drop or mis-route glosas:
///   1. the gateway `GlosaResponse` reason fields (Rust) and the cartridge
///      `_REASON_MAP` keys (Python) must be the same set;
///   2. every `GlosaReasonCode` (except OTHER) must map to a TISS Tabela 38 code
///      in `TABELA38_BY_REASON`;
///   3. every mapped Tabela 38 code must actually exist in the Tabela 38 registry
///      (`validation-rules.ttl`).
pub struct GlosaContractDoctor;

const GATEWAY_GLOSA_RS: &str = "example-gateway/src/reasoning/glosa.rs";
const CLIENT_PY: &str = "cartridges/health_audit/glosa_client.py";
const FINANCIAL_PY: &str = "cartridges/health_audit/domain/financial.py";
const TABELA38_TTL: &str = "cartridges/health_audit/ontologies/validation-rules.ttl";

impl Doctor for GlosaContractDoctor {
    fn name(&self) -> &'static str {
        "glosa-contract"
    }

    fn description(&self) -> &'static str {
        "Glosa reasons stay in sync across the gateway GlosaResponse, the client \
         _REASON_MAP, and the TISS Tabela 38 motivoGlosa codes."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_glosa_contract(root)
    }
}

fn between_quotes(s: &str) -> Option<String> {
    let a = s.find('"')?;
    let rest = &s[a + 1..];
    let b = rest.find('"')?;
    Some(rest[..b].to_string())
}

/// `pub <name>: Vec<String>` fields inside `pub struct GlosaResponse { ... }`.
fn gateway_response_fields(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut in_struct = false;
    for line in src.lines() {
        let t = line.trim();
        if t.starts_with("pub struct GlosaResponse") {
            in_struct = true;
            continue;
        }
        if in_struct {
            if t.starts_with('}') {
                break;
            }
            if let Some(rest) = t.strip_prefix("pub ")
                && t.contains("Vec<String>")
                && let Some(colon) = rest.find(':')
            {
                let name = rest[..colon].trim();
                if !name.is_empty() {
                    out.insert(name.to_string());
                }
            }
        }
    }
    out
}

/// Quoted keys of the Python `_REASON_MAP` mapping (lines `"key": GlosaReasonCode.X`).
fn client_reason_keys(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut in_map = false;
    for line in src.lines() {
        let t = line.trim();
        if t.starts_with("_REASON_MAP") {
            in_map = true;
            continue;
        }
        if in_map {
            if t.starts_with('}') {
                break;
            }
            if t.contains("GlosaReasonCode.")
                && let Some(key) = between_quotes(t)
            {
                out.insert(key);
            }
        }
    }
    out
}

/// `(member, code)` pairs of `TABELA38_BY_REASON` (lines `GlosaReasonCode.X: "1234"`).
fn tabela38_mapping(src: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut in_map = false;
    for line in src.lines() {
        let t = line.trim();
        if t.starts_with("TABELA38_BY_REASON") {
            in_map = true;
            continue;
        }
        if in_map {
            if t.starts_with('}') {
                break;
            }
            if let Some(rest) = t.strip_prefix("GlosaReasonCode.")
                && let Some(colon) = rest.find(':')
            {
                let member = rest[..colon].trim().to_string();
                if let Some(code) = between_quotes(&rest[colon..]) {
                    out.push((member, code));
                }
            }
        }
    }
    out
}

/// Upper-case members of `class GlosaReasonCode(str, Enum)`.
fn reason_code_members(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut in_enum = false;
    for line in src.lines() {
        let t = line.trim();
        if t.starts_with("class GlosaReasonCode(") {
            in_enum = true;
            continue;
        }
        if in_enum {
            if t.starts_with("class ") {
                break;
            }
            if let Some(eq) = t.find(" = \"") {
                let name = t[..eq].trim();
                if !name.is_empty() && name.chars().all(|c| c.is_ascii_uppercase() || c == '_') {
                    out.insert(name.to_string());
                }
            }
        }
    }
    out
}

/// Tabela 38 codes declared in the TTL (`rule:ruleCode "XXXX"`).
fn tabela38_registry_codes(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in src.lines() {
        if let Some(idx) = line.find("rule:ruleCode")
            && let Some(code) = between_quotes(&line[idx..])
        {
            out.insert(code);
        }
    }
    out
}

pub fn doctor_glosa_contract(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings: Vec<String> = Vec::new();
    let mut evidence: Vec<EvidenceItem> = Vec::new();

    let gateway = root.join(GATEWAY_GLOSA_RS);
    let client = root.join(CLIENT_PY);
    let financial = root.join(FINANCIAL_PY);
    let ttl = root.join(TABELA38_TTL);

    if !gateway.exists() || !client.exists() || !financial.exists() {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor.glosa-contract"),
            kind: "doctor".to_string(),
            summary: "glosa-contract: pipeline sources not all present; skipping".to_string(),
            confidence: 0.5,
            entities: vec![],
            evidence,
            warnings,
            meta: Some(json!({"skipped": true})),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    let gateway_src = read_text(&gateway, &mut warnings).unwrap_or_default();
    let client_src = read_text(&client, &mut warnings).unwrap_or_default();
    let financial_src = read_text(&financial, &mut warnings).unwrap_or_default();

    let response_fields = gateway_response_fields(&gateway_src);
    let reason_keys = client_reason_keys(&client_src);
    let mapping = tabela38_mapping(&financial_src);
    let members = reason_code_members(&financial_src);

    evidence.push(EvidenceItem {
        kind: "glosa-contract".to_string(),
        path: gateway.display().to_string(),
        line: None,
        detail: format!(
            "{} GlosaResponse fields, {} client reason keys, {} Tabela38 mappings, {} reason codes",
            response_fields.len(),
            reason_keys.len(),
            mapping.len(),
            members.len()
        ),
    });

    // 1. Gateway response fields == client reason map keys.
    if response_fields != reason_keys {
        let gateway_only: Vec<_> = response_fields.difference(&reason_keys).cloned().collect();
        let client_only: Vec<_> = reason_keys.difference(&response_fields).cloned().collect();
        warnings.push(format!(
            "glosa reason drift: GlosaResponse vs _REASON_MAP; gateway-only={gateway_only:?}, client-only={client_only:?}"
        ));
    }

    // 2. Every reason code except OTHER maps to a Tabela 38 code.
    let mapped_members: BTreeSet<String> = mapping.iter().map(|(m, _)| m.clone()).collect();
    for member in &members {
        if member == "OTHER" {
            continue;
        }
        if !mapped_members.contains(member) {
            warnings.push(format!(
                "GlosaReasonCode.{member} has no TABELA38_BY_REASON motivoGlosa code"
            ));
        }
    }

    // 3. Every mapped Tabela 38 code exists in the registry (TTL).
    if ttl.exists() {
        let ttl_src = read_text(&ttl, &mut warnings).unwrap_or_default();
        let registry = tabela38_registry_codes(&ttl_src);
        if registry.is_empty() {
            warnings.push(format!(
                "Tabela 38 registry parsed 0 codes from {}",
                ttl.display()
            ));
        } else {
            for (member, code) in &mapping {
                if !registry.contains(code) {
                    warnings.push(format!(
                        "GlosaReasonCode.{member} -> {code} is not a valid Tabela 38 code"
                    ));
                }
            }
        }
    } else {
        warnings.push(format!("Tabela 38 TTL not found at {}", ttl.display()));
    }

    let summary = if warnings.is_empty() {
        format!(
            "glosa-contract: {} reasons consistent across gateway, client, and Tabela 38",
            reason_keys.len()
        )
    } else {
        format!("glosa-contract: {} contract issue(s)", warnings.len())
    };
    let confidence = if warnings.is_empty() {
        0.98_f32
    } else {
        0.6_f32
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor.glosa-contract"),
        kind: "doctor".to_string(),
        summary,
        confidence,
        entities: vec![json!({
            "gateway_response_fields": response_fields.iter().collect::<Vec<_>>(),
            "client_reason_keys": reason_keys.iter().collect::<Vec<_>>(),
            "tabela38_mappings": mapping.len(),
            "reason_codes": members.iter().collect::<Vec<_>>(),
        })],
        evidence,
        warnings,
        meta: Some(json!({
            "gateway": GATEWAY_GLOSA_RS,
            "client": CLIENT_PY,
            "financial": FINANCIAL_PY,
            "tabela38_ttl": TABELA38_TTL,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}
