//! `FormalStore::from_documents` must work without a repo on disk.
//!
//! The example this replaces read `../example-workspace/...`, so it only ran
//! when a sibling clone happened to exist. A test in this crate needs its own
//! bytes; the whole point of the constructor is that no repo is required.

use leio_code::knowledge_explain::sparql_with_store;
use leio_code::knowledge_graph::FormalStore;
use oxigraph::io::RdfFormat;

const TINY_GYM: &[u8] = include_bytes!("fixtures/tiny_gym.ttl");

#[test]
fn builds_a_store_from_bytes_and_answers_grounded() {
    let store = FormalStore::from_documents([("tiny_gym.ttl", RdfFormat::Turtle, TINY_GYM)])
        .expect("store from bytes");
    assert_eq!(store.stats.files_ok, 1);
    assert!(store.stats.files_failed.is_empty());
    assert!(store.stats.triples >= 4, "triples: {}", store.stats.triples);

    let envelope = sparql_with_store(
        &store,
        "PREFIX ex: <http://example.org/gym#> SELECT ?u ?o WHERE { ?u ex:opensAt ?o }",
        10,
    );
    assert_eq!(envelope.entities.len(), 2);
    let grounded = envelope
        .meta
        .as_ref()
        .and_then(|m| m.get("grounded"))
        .and_then(serde_json::Value::as_bool);
    assert_eq!(grounded, Some(true));
}

#[test]
fn a_malformed_document_does_not_blind_the_graph() {
    // One broken file must not cost the caller every other document.
    let store = FormalStore::from_documents([
        ("tiny_gym.ttl", RdfFormat::Turtle, TINY_GYM),
        (
            "broken.ttl",
            RdfFormat::Turtle,
            b"this is not turtle {{{".as_slice(),
        ),
    ])
    .expect("store from bytes");
    assert_eq!(store.stats.files_ok, 1);
    assert_eq!(store.stats.files_failed.len(), 1);
    assert!(store.stats.triples >= 4);
}

#[test]
fn jsonld_document_loads_alongside_turtle() {
    const JSONLD: &[u8] = br#"[{
  "@id": "http://example.org/gym#unitC",
  "http://www.w3.org/2000/01/rdf-schema#label": [{"@value": "Unit C"}],
  "http://example.org/gym#opensAt": [{"@value": "07:00"}]
}]"#;
    let store = FormalStore::from_documents([
        ("tiny_gym.ttl", RdfFormat::Turtle, TINY_GYM),
        (
            "unitC.jsonld",
            RdfFormat::JsonLd {
                profile: oxigraph::io::JsonLdProfileSet::empty(),
            },
            JSONLD,
        ),
    ])
    .expect("store from jsonld");
    assert_eq!(store.stats.files_ok, 2, "{:?}", store.stats.files_failed);
    let envelope = sparql_with_store(
        &store,
        "PREFIX ex: <http://example.org/gym#> SELECT ?u ?o WHERE { ?u ex:opensAt ?o }",
        10,
    );
    assert!(
        envelope.entities.len() >= 3,
        "expected turtle units plus JSON-LD unitC, got {}",
        envelope.entities.len()
    );
}
