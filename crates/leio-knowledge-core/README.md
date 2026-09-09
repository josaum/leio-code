# leio-knowledge-core

In-memory formal knowledge store: Oxigraph SPARQL with anchored-or-refuse
envelopes.

This crate is the shared mechanism behind `leio-code knowledge explain` and
behind any runtime that ships an ontology without a repository — a container
image, a multi-tenant brain substrate. It deliberately excludes repository
walking, the concept lattice trail, and on-disk caching; `leio-code` layers
those on for repo-backed stores.

```rust
use leio_knowledge_core::{FormalStore, RdfFormat, sparql_with_store};

let ttl = b"@prefix ex: <http://example.org/> . ex:a ex:opensAt \"06:00\" .";
let store = FormalStore::from_documents([("a.ttl", RdfFormat::Turtle, ttl.as_slice())])?;
let envelope = sparql_with_store(
    &store,
    "PREFIX ex: <http://example.org/> SELECT ?o WHERE { ex:a ex:opensAt ?o }",
    10,
);
assert_eq!(envelope.meta.as_ref().and_then(|m| m["grounded"].as_bool()), Some(true));
```

The contract: `meta.grounded` is `true` only when the graph produced
bindings. No bindings, an empty graph, or a SPARQL error is a refusal —
confidence 0, an `ungrounded` warning, a one-step `refuse` reasoning trail.
A consumer never has to guess.
