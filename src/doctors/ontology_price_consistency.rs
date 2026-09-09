//! Ontology plan price consistency doctor.
//!
//! Production drift found 2026-08-16: the central A-Box
//! `cartridges/pratique/ontologies/pratique.ttl` and the per-unit files under
//! `cartridges/pratique/ontologies/units/` both asserted `gr:hasPrice` for the
//! same `http://www.pratiquefitness.com.br/plans/...` IRIs with different
//! values (38 conflicts; unit files are authoritative). The sales agent quoted
//! nondeterministic prices depending on which graph the reasoner surfaced
//! first. This doctor fails whenever any plan IRI carries two or more
//! distinct price values across (or within) the Pratique ontology files.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use regex::Regex;
use serde_json::json;

use super::Doctor;
use super::utils::{query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct OntologyPriceConsistencyDoctor;

impl Doctor for OntologyPriceConsistencyDoctor {
    fn name(&self) -> &'static str {
        "ontology-price-consistency"
    }

    fn description(&self) -> &'static str {
        "Checks that every Pratique plan IRI asserts a single gr:hasPrice value across the ontology files."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_ontology_price_consistency(root)
    }
}

const ONTOLOGY_DIR: &str = "cartridges/pratique/ontologies";

struct PriceAssertion {
    value: f64,
    raw: String,
    file: String,
    line: usize,
}

/// Recursively collect `*.ttl` files under `dir`, sorted for determinism.
fn collect_ttl_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries = entries.flatten().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_ttl_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "ttl") {
            out.push(path);
        }
    }
}

/// Extract `gr:hasPrice` assertions from subject blocks whose IRI lives under
/// `.../plans/`. Blocks start at an unindented `<IRI>` subject line; property
/// lines are indented, so the block runs until the next unindented subject.
fn extract_plan_prices(body: &str, rel_path: &str) -> Vec<(String, PriceAssertion)> {
    let subject_re = Regex::new(r"^<([^>]+)>").expect("subject regex compiles");
    let price_re =
        Regex::new(r"gr:hasPrice\s+(-?[0-9]+(?:\.[0-9]+)?)").expect("price regex compiles");

    let mut out = Vec::new();
    let mut current_plan_iri: Option<String> = None;
    for (index, line) in body.lines().enumerate() {
        // A new subject block starts at any non-empty, non-indented line.
        if !line.is_empty() && !line.starts_with(char::is_whitespace) {
            current_plan_iri = subject_re
                .captures(line)
                .map(|caps| caps[1].to_string())
                .filter(|iri| iri.contains("/plans/"));
        }
        let Some(iri) = &current_plan_iri else {
            continue;
        };
        for caps in price_re.captures_iter(line) {
            let raw = caps[1].to_string();
            let Ok(value) = raw.parse::<f64>() else {
                continue;
            };
            out.push((
                iri.clone(),
                PriceAssertion {
                    value,
                    raw,
                    file: rel_path.to_string(),
                    line: index + 1,
                },
            ));
        }
    }
    out
}

pub fn doctor_ontology_price_consistency(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut entities = Vec::new();

    let ontology_root = root.join(ONTOLOGY_DIR);
    if !ontology_root.is_dir() {
        warnings.push(format!(
            "Pratique ontology directory missing or unreadable: {ONTOLOGY_DIR}"
        ));
        evidence.push(EvidenceItem {
            kind: "ontology_price_consistency_missing_dir".to_string(),
            path: ONTOLOGY_DIR.to_string(),
            line: None,
            detail: "central A-Box + per-unit plan price sources not found".to_string(),
        });
    }

    let mut io_warnings = Vec::new();
    let mut by_iri: BTreeMap<String, Vec<PriceAssertion>> = BTreeMap::new();
    let mut files_scanned = 0usize;
    if ontology_root.is_dir() {
        let mut ttl_files = Vec::new();
        collect_ttl_files(&ontology_root, &mut ttl_files);
        for path in ttl_files {
            let rel_path = path
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| path.to_string_lossy().to_string());
            let Some(body) = read_text(&path, &mut io_warnings) else {
                warnings.push(format!("failed to read ontology file: {rel_path}"));
                continue;
            };
            files_scanned += 1;
            for (iri, assertion) in extract_plan_prices(&body, &rel_path) {
                by_iri.entry(iri).or_default().push(assertion);
            }
        }
    }
    warnings.extend(io_warnings);

    let mut conflicting_iris = 0usize;
    for (iri, assertions) in &by_iri {
        let mut distinct: Vec<f64> = assertions.iter().map(|a| a.value).collect();
        distinct.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        distinct.dedup();
        if distinct.len() < 2 {
            continue;
        }
        conflicting_iris += 1;
        let rendered_values = distinct
            .iter()
            .map(|value| format!("{value:.2}"))
            .collect::<Vec<_>>()
            .join(", ");
        let rendered_files = assertions
            .iter()
            .map(|a| format!("{}:{} ({})", a.file, a.line, a.raw))
            .collect::<Vec<_>>()
            .join(", ");
        warnings.push(format!(
            "plan IRI {iri} has conflicting gr:hasPrice values [{rendered_values}] across: {rendered_files}"
        ));
        for assertion in assertions {
            evidence.push(EvidenceItem {
                kind: "ontology_price_consistency_conflict".to_string(),
                path: assertion.file.clone(),
                line: Some(assertion.line),
                detail: format!(
                    "{iri} gr:hasPrice {} (conflicting values: {rendered_values})",
                    assertion.raw
                ),
            });
        }
    }

    entities.push(json!({
        "doctor": "ontology-price-consistency",
        "ontology_dir": ONTOLOGY_DIR,
        "files_scanned": files_scanned,
        "plan_iris_with_prices": by_iri.len(),
        "conflicting_iris": conflicting_iris,
        "authoritative_source": "cartridges/pratique/ontologies/units/*.ttl (per-unit files)",
        "drift_incident": "2026-08-16: central pratique.ttl disagreed with units/*.ttl on 38 plan IRIs; sales agent quoted nondeterministic prices"
    }));

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_ontology_price_consistency"),
        kind: "doctor".to_string(),
        summary: if conflicting_iris == 0 && warnings.is_empty() {
            "Pratique plan prices are consistent across ontology files".to_string()
        } else if conflicting_iris == 0 {
            "Pratique ontology price scan incomplete: unreadable inputs".to_string()
        } else {
            format!(
                "Pratique plan price drift: {conflicting_iris} plan IRI(s) with conflicting gr:hasPrice values"
            )
        },
        confidence: if conflicting_iris == 0 && warnings.is_empty() {
            0.93
        } else {
            0.62
        },
        entities,
        evidence,
        warnings,
        meta: Some(json!({
            "tenant": "pratique",
            "cartridge": "pratique",
            "scope": "cartridges/pratique/ontologies/**/*.ttl",
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "leio_ontology_price_consistency_{}_{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("cartridges/pratique/ontologies/units")).unwrap();
        root
    }

    const CENTRAL: &str = r#"
<http://www.pratiquefitness.com.br/plans/combo-plus/abraao> a :AssinaturaCombo,
        owl:NamedIndividual ;
    rdfs:label "Plano Combo Plus - Abrahão Caram"@pt-br ;
    gr:hasPrice 219.90 ;
    :oferecidoPor <http://www.pratiquefitness.com.br/units/abraao> .

<http://www.pratiquefitness.com.br/plans/plus/abraao> a :AssinaturaPlus,
        owl:NamedIndividual ;
    gr:hasPrice 179.90 .
"#;

    const UNIT_MATCH: &str = r#"
<http://www.pratiquefitness.com.br/plans/combo-plus/abraao> a :AssinaturaCombo, owl:NamedIndividual;
    gr:hasPrice 219.90;
    :oferecidoPor <http://www.pratiquefitness.com.br/units/abraao>.
"#;

    const UNIT_CONFLICT: &str = r#"
<http://www.pratiquefitness.com.br/plans/combo-plus/abraao> a :AssinaturaCombo, owl:NamedIndividual;
    gr:hasPrice 199.90;
    :oferecidoPor <http://www.pratiquefitness.com.br/units/abraao>.
"#;

    #[test]
    fn flags_conflicting_prices_across_files() {
        let root = temp_root("conflict");
        std::fs::write(
            root.join("cartridges/pratique/ontologies/pratique.ttl"),
            CENTRAL,
        )
        .unwrap();
        std::fs::write(
            root.join("cartridges/pratique/ontologies/units/abraao.ttl"),
            UNIT_CONFLICT,
        )
        .unwrap();

        let envelope = doctor_ontology_price_consistency(&root);
        assert_eq!(envelope.confidence, 0.62);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("combo-plus/abraao")
                    && warning.contains("199.90")
                    && warning.contains("219.90")),
            "expected conflict warning, got {:?}",
            envelope.warnings
        );
        assert!(
            envelope
                .evidence
                .iter()
                .any(|item| item.kind == "ontology_price_consistency_conflict"
                    && item.path.ends_with("units/abraao.ttl"))
        );
        assert!(envelope.summary.contains("1 plan IRI(s)"));
    }

    #[test]
    fn passes_on_consistent_prices() {
        let root = temp_root("consistent");
        std::fs::write(
            root.join("cartridges/pratique/ontologies/pratique.ttl"),
            CENTRAL,
        )
        .unwrap();
        std::fs::write(
            root.join("cartridges/pratique/ontologies/units/abraao.ttl"),
            UNIT_MATCH,
        )
        .unwrap();

        let envelope = doctor_ontology_price_consistency(&root);
        assert!(
            envelope.warnings.is_empty(),
            "unexpected warnings: {:?}",
            envelope.warnings
        );
        assert_eq!(
            envelope.summary,
            "Pratique plan prices are consistent across ontology files"
        );
        assert_eq!(envelope.confidence, 0.93);
    }

    #[test]
    fn ignores_non_plan_price_blocks() {
        let root = temp_root("nonplan");
        std::fs::write(
            root.join("cartridges/pratique/ontologies/pratique.ttl"),
            format!(
                "{CENTRAL}\n:exameBioimpedancia a schema1:MedicalTest ;\n    gr:hasPrice 99.90 .\n"
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("cartridges/pratique/ontologies/units/abraao.ttl"),
            UNIT_MATCH,
        )
        .unwrap();

        let envelope = doctor_ontology_price_consistency(&root);
        assert!(
            envelope.warnings.is_empty(),
            "non-plan price blocks must not conflict: {:?}",
            envelope.warnings
        );
    }
}
