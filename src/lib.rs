//! LEIO Code library crate — agent-first code intelligence for arbitrary repositories.
//!
//! The crate is structured as a small set of cooperating layers; see each module
//! header for details and the workspace [`README.md`](../README.md) for the
//! end-user surface.
//!
//! - **Indexing**: [`indexer`] walks the repo into a [`model::RepoIndex`].
//! - **Onboarding**: [`init`] scaffolds `.leio-code/config.toml` and builds the
//!   first index for `leio-code init`.
//! - **Ownership queries**: [`query`] answers `find` / `explain` over the index.
//! - **Graph queries**: [`graph_query`] answers `callers-of` / `callees-of` /
//!   `imports-in` / `dead-code` from the local code-graph cache (or Oxigraph fallback).
//! - **Code graph export**: [`code_graph`] emits N-Quads + manifest + a structural
//!   query-cache from tree-sitter parses.
//! - **Local wiki store**: [`knowledge`] compiles markdown into
//!   `exports/knowledge-v1/` and serves `knowledge` queries fully offline.
//! - **Formal knowledge**: [`knowledge_graph`] loads repo RDF + induced OWL
//!   (Turtle, RDF/XML, JSON-LD); [`knowledge_explain`] answers only with SPARQL
//!   bindings and a lattice trail. [`ontology`] extracts OWL/RDFS classes and
//!   properties into the symbol index.
//! - **Local node store**: [`local_nodes`] scans the FCA-tagged
//!   `exports/arrow-nodes-v1/nodes.arrow` file (zero-copy, fully local).
//! - **Node navigation**: [`nav`] keeps a stateful cursor (goto / callers /
//!   related / lattice parent / heading related) in `.leio-code/nav-session.json`.
//! - **Concept lattice**: [`lattice`] persists cover morphisms, OWL, and the
//!   wiki-heading functor into the concept category.
//! - **Search sidecar**: [`search`] mirrors the index into an Arrow IPC
//!   (file-format) store at `.leio-code/search.arrow` for ranked `find` lookups.
//!   Reads take no file lock. Best-effort; [`query`] falls back to linear scan
//!   when the sidecar is missing or stale.
//! - **Concurrent sidecars**: [`sidecar`] atomically replaces JSON/N-Quads
//!   and serializes rebuilds with `.leio-code/*.lock` so several agents can
//!   share one tree. Nav sessions are isolated by `LEIO_SESSION`.
//! - **Checkout identity**: [`checkout`] names the git worktree, branch, HEAD,
//!   and origin so JSON-LD events stay distinct across clones and worktrees.
//! - **Other exports**: [`export`] emits FCA formal context, node-row Arrow IPC,
//!   and hypergraph artifacts under `.leio-code/exports/`.
//! - **Context bundles**: [`context`] ranks files/symbols for an agent task.
//! - **Doctors**: [`doctors`] is a profile-aware contract registry; [`audit`] is
//!   the composite pre-deploy roll-up; [`baseline_allowlist`] filters known-noise
//!   warnings under `--strict`.
//! - **Capabilities**: [`capabilities`] reports which find/explain/graph/doctor/export
//!   verbs are meaningful for the current workspace profile.
//! - **Config**: [`config`] resolves `.leio-code/config.toml` and env overrides.

/// Locating the multi-repo workspace this crate historically shipped inside.
///
/// A few guards assert against the *real* sibling repos (`example-api`,
/// `example-gateway`, `deploy`, …) rather than fixtures — they catch drift in
/// code this crate does not own. This crate also ships standalone, where those
/// repos are simply absent: the guards then have nothing to assert and must
/// skip, not fail. Without this, a standalone checkout cannot get a green test
/// run, so real regressions hide among permanent environmental failures.
#[cfg(test)]
pub(crate) mod test_workspace {
    use std::path::{Path, PathBuf};

    /// The workspace root (this repo's parent) when `required` exists inside
    /// it; `None` in a standalone checkout. `required` is relative to the root,
    /// e.g. `"example-api"` or `".leio-code/baseline-allowlist.txt"`.
    pub(crate) fn workspace_root_with(required: &str) -> Option<PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent()?;
        root.join(required).exists().then(|| root.to_path_buf())
    }
}

pub mod arrow_ipc;
pub mod audit;
pub mod baseline_allowlist;
pub mod capabilities;
pub mod category;
pub mod checkout;
pub mod code_graph;
pub mod config;
pub mod context;
pub mod conversation;
pub mod cross_language;
pub mod deploy_support;
pub mod diagnostics;
pub mod doctors;
pub mod embed;
pub mod explain_stdin;
pub mod export;
pub mod fca;
pub mod graph_query;
pub mod import_lookup;
pub mod indexer;
pub mod init;
pub mod jsonc;
pub mod jsonld;
pub mod kb;
pub mod knowledge;
pub mod knowledge_explain;
pub mod knowledge_graph;
pub mod lattice;
pub mod local_nodes;
pub mod model;
pub mod nav;
pub mod node_rows;
pub mod node_search;
pub mod ontology;
pub mod parser_support;
pub mod query;
pub mod search;
pub mod sidecar;
pub mod update;
pub mod value_resolution;
pub mod watcher;
