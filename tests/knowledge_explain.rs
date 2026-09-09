//! Formal SPARQL explain is gated: bind or refuse.
// Rust guideline compliant 2026-02-21

use std::fs;

use leio_code::knowledge_explain::{explain_knowledge, sparql_knowledge};
use leio_code::knowledge_graph;
use tempfile::tempdir;

fn fixture_repo() -> tempfile::TempDir {
    let dir = tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("ont")).unwrap();
    fs::create_dir_all(dir.path().join("docs")).unwrap();
    fs::write(
        dir.path().join("ont/unit.ttl"),
        r#"@prefix : <http://example.org/kb#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .

:Alpha a :Unit ;
    rdfs:label "Alpha" ;
    :hoursWeekday "05:30-23:00" ;
    :city "Beta City" .
"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("docs/alpha.md"),
        "# Alpha\n\nIRI: http://example.org/kb#Alpha\n\nHours 05:30-23:00.\n",
    )
    .unwrap();
    dir
}

#[test]
fn explain_grounds_hours_and_cites_iri() {
    let repo = fixture_repo();
    leio_code::knowledge::compile_knowledge(repo.path()).unwrap();
    knowledge_graph::compile_formal_graph(repo.path()).unwrap();
    let envelope = explain_knowledge(repo.path(), "hours Alpha", 4).unwrap();
    assert!(
        envelope.summary.contains("grounded"),
        "{}",
        envelope.summary
    );
    assert_eq!(envelope.meta.as_ref().unwrap()["grounded"], true);
    let iri = envelope.entities[0]["iri"].as_str().unwrap();
    assert_eq!(iri, "http://example.org/kb#Alpha");
    let blob = serde_json::to_string(&envelope.entities).unwrap();
    assert!(blob.contains("05:30-23:00"), "{blob}");
    assert!(
        envelope
            .evidence
            .iter()
            .any(|item| item.kind == "iri" && item.detail.contains("Alpha")),
        "{:?}",
        envelope.evidence
    );
}

#[test]
fn explain_refuses_ungrounded_needle() {
    let repo = fixture_repo();
    let envelope = explain_knowledge(repo.path(), "qwerty zxcvbnm", 4).unwrap();
    assert!(
        envelope.summary.starts_with("refused"),
        "{}",
        envelope.summary
    );
    assert_eq!(envelope.confidence, 0.0);
    assert!(envelope.entities.is_empty());
    assert!(envelope.warnings.iter().any(|row| row == "ungrounded"));
}

#[test]
fn sparql_select_returns_bindings() {
    let repo = fixture_repo();
    let envelope = sparql_knowledge(
        repo.path(),
        r#"PREFIX : <http://example.org/kb#>
SELECT ?s ?hours WHERE { ?s :hoursWeekday ?hours }"#,
        8,
    )
    .unwrap();
    assert!(
        envelope.summary.contains("solution"),
        "{}",
        envelope.summary
    );
    assert_eq!(envelope.meta.as_ref().unwrap()["grounded"], true);
}
