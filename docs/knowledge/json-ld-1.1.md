# JSON-LD 1.1 — W3C knowledge reference

Canonical source: https://www.w3.org/TR/json-ld11/
Pinned Recommendation: https://www.w3.org/TR/2020/REC-json-ld11-20200716/
Publisher: W3C. Recommendation date: 2020-07-16.

## Syntax and identity

JSON-LD represents linked data using JSON. A context maps application terms to IRIs. Use `@id` for node identity and `@type` for node types. Use `@version: 1.1` in the context when depending on 1.1 features.

## Ordering and opaque payloads

Ordinary arrays do not express RDF ordering; ordered steps require `@list`. Use `@json` type coercion for opaque JSON values that must survive conversion without reinterpretation as nodes.

## LEIO application decisions

Workflow runs persist as `state.jsonld` with stable run and event identifiers. Plan steps and command arguments preserve order. Execution results are engineering evidence; JSON-LD serialization does not grant authority or prove an application deployment. These are LEIO design choices, not W3C requirements.

## Related normative material

- Processing algorithms: https://www.w3.org/TR/json-ld11-api/
- Framing: https://www.w3.org/TR/json-ld11-framing/

This is a short original reference note, not a mirror of the Recommendation. Follow the canonical source for conformance rules and examples.
