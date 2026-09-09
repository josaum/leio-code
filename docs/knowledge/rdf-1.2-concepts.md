# RDF 1.2 Concepts and Abstract Data Model — W3C knowledge reference

Canonical source: https://www.w3.org/TR/rdf12-concepts/
Pinned version: https://www.w3.org/TR/2026/CR-rdf12-concepts-20260407/
Publisher: W3C RDF & SPARQL Working Group.
Publication status: Candidate Recommendation Snapshot, 2026-04-07; not a final Recommendation.

## Abstract data model

RDF graphs contain subject–predicate–object triples. Datasets organize a default graph and zero or more named graphs. The model distinguishes IRIs, blank nodes, literals and triple terms; concrete serialization formats implement this shared model.

## RDF 1.2 additions

Triple terms can occur as objects of other triples. RDF 1.2 also introduces directional language-tagged strings and mechanisms for announcing the RDF version used by data. Consult the pinned specification for restrictions, datatype definitions and conformance requirements.

## LEIO application decisions

Use this reference when reviewing graph identity, dataset scope and JSON-LD exports. Preserve repository and revision provenance. Keep serialization validation distinct from semantic correctness and implementation conformance. Adding a knowledge reference does not certify support for every RDF 1.2 feature.

## Related knowledge

- [RDF 1.2 Primer](rdf-1.2-primer.md)
- [JSON-LD 1.1](json-ld-1.1.md)

This is an original reference note, not a mirror of the specification. Follow the canonical source for publication updates and the pinned source for reproducible citations.
