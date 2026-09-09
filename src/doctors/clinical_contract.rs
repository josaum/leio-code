use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

/// Continuous audit of the clinical-extraction contract across crates/languages.
/// Two drifts would break extractor↔cartridge↔align interchange:
///   1. `CodeSystem` must be the same set in example-align (`omop_crosswalk.rs`),
///      the extraction contract (`clinical.rs`), and the cartridge Python mirror
///      (`clinical_extraction.py`) — a crosswalk produced with a vocab the
///      cartridge can't name (or vice-versa) is a silent integration break.
///   2. `OmopDomain` must match across `clinical.rs`, the Python mirror, and the
///      OMOP class names in `omop-cdm.ttl` (DAT-001).
pub struct ClinicalContractDoctor;

const ALIGN_RS: &str = "example-align/src/omop_crosswalk.rs";
const CLINICAL_RS: &str = "example-extraction-contracts/src/clinical.rs";
const CLINICAL_PY: &str = "cartridges/health_audit/clinical_extraction.py";
const OMOP_TTL: &str = "cartridges/health_audit/ontologies/omop-cdm.ttl";

impl Doctor for ClinicalContractDoctor {
    fn name(&self) -> &'static str {
        "clinical-contract"
    }

    fn description(&self) -> &'static str {
        "Clinical CodeSystem stays in sync across example-align, the extraction \
         contract, and the cartridge; OmopDomain matches clinical.rs + omop-cdm.ttl."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_clinical_contract(root)
    }
}

fn to_snake(variant: &str) -> String {
    let mut out = String::new();
    for (i, ch) in variant.chars().enumerate() {
        if ch.is_ascii_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

fn to_screaming(variant: &str) -> String {
    to_snake(variant).to_ascii_uppercase()
}

fn between_quotes(s: &str) -> Option<String> {
    let a = s.find('"')?;
    let rest = &s[a + 1..];
    let b = rest.find('"')?;
    Some(rest[..b].to_string())
}

/// CamelCase variant identifiers of a Rust `enum <name> { ... }`.
fn rust_enum_variants(src: &str, name: &str) -> Vec<String> {
    let needle = format!("enum {name} {{");
    let mut out = Vec::new();
    let mut in_enum = false;
    for line in src.lines() {
        let t = line.trim();
        if !in_enum {
            if line.contains(&needle) {
                in_enum = true;
            }
            continue;
        }
        if t.starts_with('}') {
            break;
        }
        let ident = t.trim_end_matches(',');
        if !ident.is_empty()
            && ident.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && ident.chars().all(|c| c.is_ascii_alphanumeric())
        {
            out.push(ident.to_string());
        }
    }
    out
}

/// String values of a Python `class <name>(...): NAME = "value"` enum.
fn python_enum_values(src: &str, class_name: &str) -> BTreeSet<String> {
    let needle = format!("class {class_name}(");
    let mut out = BTreeSet::new();
    let mut in_cls = false;
    for line in src.lines() {
        let t = line.trim();
        if !in_cls {
            if t.starts_with(&needle) {
                in_cls = true;
            }
            continue;
        }
        if t.starts_with("class ") {
            break;
        }
        if t.contains(" = \"")
            && let Some(v) = between_quotes(t)
        {
            out.insert(v);
        }
    }
    out
}

/// OMOP class local names declared in the TTL (`:Name rdf:type owl:Class`).
fn ttl_omop_classes(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in src.lines() {
        let t = line.trim();
        if t.contains("owl:Class") && t.starts_with(':') {
            let token = &t[1..];
            let name: String = token
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            if !name.is_empty() {
                out.insert(name);
            }
        }
    }
    out
}

pub fn doctor_clinical_contract(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings: Vec<String> = Vec::new();
    let mut evidence: Vec<EvidenceItem> = Vec::new();

    let align = root.join(ALIGN_RS);
    let clinical_rs = root.join(CLINICAL_RS);
    let clinical_py = root.join(CLINICAL_PY);
    let ttl = root.join(OMOP_TTL);

    if !align.exists() || !clinical_rs.exists() || !clinical_py.exists() {
        return QueryEnvelope {
            schema_version: crate::model::SCHEMA_VERSION.to_string(),
            query_id: query_id("doctor.clinical-contract"),
            kind: "doctor".to_string(),
            summary: "clinical-contract: sources not all present; skipping".to_string(),
            confidence: 0.5,
            entities: vec![],
            evidence,
            warnings,
            meta: Some(json!({"skipped": true})),
            timing_ms: started.elapsed().as_millis(),
        };
    }

    let align_src = read_text(&align, &mut warnings).unwrap_or_default();
    let clinical_rs_src = read_text(&clinical_rs, &mut warnings).unwrap_or_default();
    let clinical_py_src = read_text(&clinical_py, &mut warnings).unwrap_or_default();

    // ---- 1. CodeSystem tri-source parity (serde snake_case) ----
    let align_cs: BTreeSet<String> = rust_enum_variants(&align_src, "CodeSystem")
        .iter()
        .map(|v| to_snake(v))
        .collect();
    let rs_cs: BTreeSet<String> = rust_enum_variants(&clinical_rs_src, "CodeSystem")
        .iter()
        .map(|v| to_snake(v))
        .collect();
    let py_cs = python_enum_values(&clinical_py_src, "CodeSystem");

    evidence.push(EvidenceItem {
        kind: "clinical-contract".to_string(),
        path: align.display().to_string(),
        line: None,
        detail: format!(
            "CodeSystem: align={} clinical.rs={} python={}",
            align_cs.len(),
            rs_cs.len(),
            py_cs.len()
        ),
    });

    if !(align_cs == rs_cs && rs_cs == py_cs) {
        warnings.push(format!(
            "CodeSystem drift: align={align_cs:?}, clinical.rs={rs_cs:?}, python={py_cs:?}"
        ));
    }

    // ---- 2. OmopDomain parity (clinical.rs SCREAMING == python == TTL classes) ----
    let rs_dom: BTreeSet<String> = rust_enum_variants(&clinical_rs_src, "OmopDomain")
        .iter()
        .map(|v| to_screaming(v))
        .collect();
    let py_dom = python_enum_values(&clinical_py_src, "OmopDomain");

    if rs_dom != py_dom {
        warnings.push(format!(
            "OmopDomain drift (clinical.rs vs python): rs={rs_dom:?}, python={py_dom:?}"
        ));
    }
    if ttl.exists() {
        let ttl_dom: BTreeSet<String> =
            ttl_omop_classes(&read_text(&ttl, &mut warnings).unwrap_or_default())
                .iter()
                .map(|v| to_screaming(v))
                .collect();
        if !ttl_dom.is_empty() && ttl_dom != py_dom {
            warnings.push(format!(
                "OmopDomain drift (omop-cdm.ttl vs python): ttl={ttl_dom:?}, python={py_dom:?}"
            ));
        }
    }

    let summary = if warnings.is_empty() {
        format!(
            "clinical-contract: CodeSystem ({}) + OmopDomain ({}) consistent across crates",
            py_cs.len(),
            py_dom.len()
        )
    } else {
        format!("clinical-contract: {} contract issue(s)", warnings.len())
    };
    let confidence = if warnings.is_empty() {
        0.98_f32
    } else {
        0.6_f32
    };

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor.clinical-contract"),
        kind: "doctor".to_string(),
        summary,
        confidence,
        entities: vec![json!({
            "code_system": py_cs.iter().collect::<Vec<_>>(),
            "omop_domain": py_dom.iter().collect::<Vec<_>>(),
        })],
        evidence,
        warnings,
        meta: Some(json!({
            "align": ALIGN_RS,
            "clinical_rs": CLINICAL_RS,
            "clinical_py": CLINICAL_PY,
            "omop_ttl": OMOP_TTL,
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}
