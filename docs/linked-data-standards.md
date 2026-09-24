# Linked-data contracts and specification targets

Checked 2026-09-24. Conformance is scoped to exercised producer features; LEIO
does not claim to implement a general JSON-LD processor or the full W3C test suite.

| Area | Official source | Target and verification |
|---|---|---|
| RDF 1.2 | [Concepts, 2026-04-07 Candidate Recommendation](https://www.w3.org/TR/2026/CR-rdf12-concepts-20260407/) | Absolute IRIs, RDF term identity, typed literals and directional language strings. Oxigraph parses the actual emitted document offline. |
| SPARQL 1.2 | [Query, 2026-09-21 Working Draft](https://www.w3.org/TR/2026/WD-sparql12-query-20260921/) | `STRLANGDIR`, `LANGDIR`, `sameTerm`, and ordinary evidence queries. Draft support is not full language certification. |
| JSON-LD 1.2 | [Official API editor draft](https://w3c.github.io/json-ld-api/) | Tracking target. Draft recommends `@version: 1.2`; oxjsonld 0.2.6 rejects that value. Full 1.2 processing is **not supported**. Do not silently rewrite a 1.2 input to 1.1. |
| JSON-LD syntax | [1.1 Recommendation](https://www.w3.org/TR/json-ld11/) | Working compatibility profile uses `@version: 1.1`, an embedded context, IRI coercion and JSON literals. RDF 1.2 direction handling uses Oxigraph's `rdf-12` extension; ordinary 1.1 processors may need their own direction option. |
| JSON Schema | [Draft 2020-12 Core](https://json-schema.org/draft/2020-12/json-schema-core), [Validation](https://json-schema.org/draft/2020-12/json-schema-validation) | Explicit `$schema`, absolute `$id`, required producer fields, bounded scalar types. This schema validates the LEIO profile, not arbitrary JSON-LD semantics. |
| Well-known discovery | [RFC 8615](https://www.rfc-editor.org/rfc/rfc8615), [RFC 6415 Appendix A](https://datatracker.ietf.org/doc/html/rfc6415#appendix-A), [IANA registry](https://www.iana.org/assignments/well-known-uris/) | Registered `/.well-known/host-meta.json`, JRD `links`, `application/json`; no invented well-known suffix. |

## Consumption

The hosted server exposes these static, public contract documents:

- `GET /.well-known/host-meta.json`: JRD links to the query-result schema and context.
- `GET /schemas/query-result-jsonld-v1.schema.json`: `application/schema+json`.
- `GET /contexts/leio-code-v1.jsonld`: `application/ld+json`.

URLs come from the configured public origin, never request Host or forwarded
headers. Both HTTP and HTTPS entrypoints for the same deployment must expose
the same host metadata. Configure `LEIO_APPS_SDK_PUBLIC_URL` for public hosting.
These routes contain no repository records. MCP JSON-RPC responses retain their
own protocol schema; the query-result schema describes the CLI JSON-LD document,
not an entire MCP response. The context is embedded in each CLI result/journal
line, so discovery is optional for offline consumers.

The namespace `https://ontology.getjai.com/leio-code/v1#` identifies vocabulary
terms; it is not advertised as a retrievable context document. Metadata and
checkout objects are RDF JSON literals. Entity/evidence fields become graph
predicates; ordinary JSON arrays there are RDF sets unless explicitly represented
as lists. No claim is made that arbitrary JSON entity shapes round-trip byte for
byte. Graph evidence is unsigned engineering evidence, not attestation or proof
of runtime execution. FCA memberships express shared indexed attributes.

## Compatibility

The context changes from an invalid remote-context reference to a term map.
The erroneous boolean property named `prov:Activity` is replaced by an RDF type
statement. Historical journal entries remain historical; consumers must not
fetch their old namespace URL as if it were a working context. This change needs
explicit release notes and downstream compatibility review before publication.

Tests cover emitted data without substituting a test-only context, reserved
filename characters, line citations, IRI-valued provenance, date datatypes,
lossless metadata, directional literals, schema negative cases, and live HTTP
discovery. Passing these tests establishes only the listed feature coverage.
