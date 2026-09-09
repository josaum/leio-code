//! Formal knowledge core: Oxigraph SPARQL with anchored-or-refuse envelopes.
//!
//! This is the shared mechanism behind LEIO Code's `knowledge explain` and
//! behind any runtime that ships an ontology without a repository — a
//! container image, a multi-tenant brain substrate. Build a [`FormalStore`]
//! from RDF bytes with [`FormalStore::from_documents`], run SPARQL with
//! [`sparql_with_store`], and read a [`QueryEnvelope`] whose `meta.grounded`
//! is `true` only when the answer came from bindings. No bindings means a
//! refusal, never a guess.
//!
//! Repository walking, the concept lattice trail, and on-disk caching are
//! deliberately absent; `leio-code` layers those on for repo-backed stores.
//!
//! # Examples
//!
//! ```
//! use leio_knowledge_core::{FormalStore, RdfFormat, sparql_with_store};
//!
//! let ttl = b"@prefix ex: <http://example.org/> . ex:a ex:opensAt \"06:00\" .";
//! let store = FormalStore::from_documents([("a.ttl", RdfFormat::Turtle, ttl.as_slice())])?;
//! let envelope = sparql_with_store(
//!     &store,
//!     "PREFIX ex: <http://example.org/> SELECT ?o WHERE { ex:a ex:opensAt ?o }",
//!     10,
//! );
//! assert_eq!(envelope.entities.len(), 1);
//! assert_eq!(envelope.meta.as_ref().and_then(|m| m["grounded"].as_bool()), Some(true));
//! # Ok::<(), anyhow::Error>(())
//! ```
// Rust guideline compliant 2026-02-21

pub mod envelope;
pub mod sparql;
pub mod store;

#[doc(inline)]
pub use envelope::{EvidenceItem, QueryEnvelope, SCHEMA_VERSION};
#[doc(inline)]
pub use sparql::{SparqlOutcome, SparqlTable, execute, execute_table, refusal, sparql_with_store};
#[doc(inline)]
pub use store::{FormalStats, FormalStore, RdfFormat, SparqlQueryExt};
