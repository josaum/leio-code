//! In-memory formal store: Oxigraph SPARQL over RDF documents, no repo required.
//!
//! [`FormalStore`] wraps an Oxigraph [`Store`] plus the [`FormalStats`] that
//! describe how it was assembled. [`FormalStore::from_documents`] builds one
//! from bytes already in memory — an ontology embedded in a container image,
//! a graph fetched over the wire — so a caller never needs a repository on
//! disk. Query text is normalised by [`with_standard_prefixes`] before it
//! reaches the evaluator.
// Rust guideline compliant 2026-02-21

use std::collections::BTreeMap;
use std::fmt;
use std::io::Cursor;

use anyhow::{Context, Result};
use oxigraph::io::RdfParser;
use oxigraph::model::{GraphName, Term};
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

pub use oxigraph::io::RdfFormat;

/// Knowledge vocabulary used for citation and lattice trail triples.
pub const KNOWLEDGE_NS: &str = "https://example.local/leio/knowledge#";
/// RDF Schema namespace.
pub const RDFS_NS: &str = "http://www.w3.org/2000/01/rdf-schema#";
/// RDF 1.2 reification predicate: `<reifier> rdf:reifies <<( s p o )>>`.
pub const RDF_REIFIES: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies";

/// Longest query prefix echoed in an error message.
///
/// Long enough to identify the failing query, short enough to keep the error
/// on one line. Matches the historical `leio-code` limit so messages do not
/// change across the crate split.
const ERROR_QUERY_PREVIEW_CHARS: usize = 120;

/// SPARQL execution on a store, uniform across the oxigraph 0.5 evaluator.
pub trait SparqlQueryExt {
    /// Parse and execute `query` against this store.
    ///
    /// # Errors
    ///
    /// Returns an error when Oxigraph rejects the query text or evaluation
    /// fails.
    fn sparql_query(&self, query: &str) -> Result<QueryResults<'_>>;
}

impl SparqlQueryExt for Store {
    fn sparql_query(&self, query: &str) -> Result<QueryResults<'_>> {
        Ok(SparqlEvaluator::new()
            .parse_query(query)
            .with_context(|| format!("SPARQL failed: {}", first_line(query)))?
            .on_store(self)
            .execute()?)
    }
}

/// In-memory formal store plus load statistics.
pub struct FormalStore {
    store: Store,
    /// How the store was assembled.
    pub stats: FormalStats,
}

impl fmt::Debug for FormalStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FormalStore")
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

/// Counts and skipped files from a formal-graph load.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FormalStats {
    pub files_ok: usize,
    pub files_failed: Vec<(String, String)>,
    pub triples: usize,
    pub citations: usize,
    pub alignments: usize,
    /// RDF 1.2 statement-level provenance pairs (`rdf:reifies` reifier +
    /// `prov:wasDerivedFrom` evidence).
    #[serde(default)]
    pub provenance: usize,
    /// Prefix name (empty string = `:`) → namespace IRI discovered in Turtle.
    #[serde(default)]
    pub prefixes: BTreeMap<String, String>,
    /// Newest source mtime (unix seconds) baked into this cache.
    #[serde(default)]
    pub source_mtime: u64,
    /// `hit`, `rebuilt`, or empty when compiled fresh.
    #[serde(default)]
    pub cache: String,
}

impl FormalStore {
    /// Wrap an already-populated Oxigraph store with its load statistics.
    ///
    /// Loaders that assemble the store themselves — a repository walk with
    /// citations and lattice alignment, for instance — use this to hand the
    /// finished graph over without re-parsing it.
    #[must_use]
    pub fn from_parts(store: Store, stats: FormalStats) -> Self {
        Self { store, stats }
    }

    /// The underlying Oxigraph store, for dumps and bulk loads.
    #[must_use]
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Run SPARQL against the loaded formal graph.
    ///
    /// Standard prefixes and the prefixes discovered at load time are
    /// injected when the query omits them.
    ///
    /// # Errors
    ///
    /// Returns an error when Oxigraph rejects the query text.
    pub fn query(&self, sparql: &str) -> Result<QueryResults<'_>> {
        let prepared = with_standard_prefixes(sparql, &self.stats.prefixes);
        self.store
            .sparql_query(&prepared)
            .with_context(|| format!("SPARQL failed: {}", first_line(sparql)))
    }

    /// Build a store from RDF documents already in memory, without a repo scan.
    ///
    /// A repository-backed loader needs a root because it walks the tree and
    /// caches beside the compiled wiki. Callers that already hold the RDF —
    /// an embedded ontology in a container image, a graph fetched over the
    /// wire — have no repo to point at, and materialising a fake one just to
    /// satisfy the signature is worse than admitting the store can come from
    /// bytes.
    ///
    /// Each entry is `(name, format, bytes)`; `name` only labels parse
    /// failures, which are collected into `stats.files_failed` instead of
    /// aborting the load: one malformed document should not blind the whole
    /// graph.
    ///
    /// # Examples
    ///
    /// ```
    /// use leio_knowledge_core::{FormalStore, RdfFormat};
    ///
    /// let ttl = b"@prefix ex: <http://example.org/> . ex:a ex:b ex:c .";
    /// let store = FormalStore::from_documents([("a.ttl", RdfFormat::Turtle, ttl.as_slice())])?;
    /// assert_eq!(store.stats.files_ok, 1);
    /// assert_eq!(store.stats.triples, 1);
    /// # Ok::<(), anyhow::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error only when the underlying store cannot be created.
    pub fn from_documents<'a, I>(documents: I) -> Result<Self>
    where
        I: IntoIterator<Item = (&'a str, RdfFormat, &'a [u8])>,
    {
        let store = Store::new().context("create Oxigraph knowledge store")?;
        let mut stats = FormalStats::default();
        for (name, format, bytes) in documents {
            let parser = RdfParser::from_format(format).with_default_graph(GraphName::DefaultGraph);
            match store.load_from_reader(parser, Cursor::new(bytes)) {
                Ok(()) => stats.files_ok += 1,
                Err(err) => stats
                    .files_failed
                    .push((name.to_string(), first_line(&err.to_string()))),
            }
        }
        stats.triples = store.len().unwrap_or(0);
        stats.cache = "in_memory".to_string();
        Ok(Self { store, stats })
    }
}

/// Inject rdf/rdfs/owl/leio and discovered `:` prefixes when the query omits them.
#[must_use]
pub fn with_standard_prefixes(query: &str, prefixes: &BTreeMap<String, String>) -> String {
    let mut preamble = String::new();
    let mut add = |name: &str, iri: &str| {
        if has_prefix_decl(query, name) || iri.is_empty() {
            return;
        }
        if name.is_empty() {
            preamble.push_str(&format!("PREFIX : <{iri}>\n"));
        } else {
            preamble.push_str(&format!("PREFIX {name}: <{iri}>\n"));
        }
    };
    add("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#");
    add("rdfs", RDFS_NS);
    add("owl", "http://www.w3.org/2002/07/owl#");
    add("leio", KNOWLEDGE_NS);
    if let Some(iri) = prefixes.get("") {
        add("", iri);
    }
    for (name, iri) in prefixes {
        if name.is_empty() {
            continue;
        }
        add(name, iri);
    }
    if preamble.is_empty() {
        query.to_string()
    } else {
        preamble.push('\n');
        preamble.push_str(query);
        preamble
    }
}

fn has_prefix_decl(query: &str, name: &str) -> bool {
    let lower = query.to_ascii_lowercase();
    if name.is_empty() {
        lower.contains("prefix :") || lower.contains("@prefix :")
    } else {
        let needle = format!("prefix {name}:");
        let at = format!("@prefix {name}:");
        lower.contains(&needle) || lower.contains(&at)
    }
}

/// Whether `needle` looks like a SPARQL query rather than a lexical question.
#[must_use]
pub fn looks_like_sparql(needle: &str) -> bool {
    let trimmed = needle.trim_start();
    let head = trimmed
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    matches!(
        head.as_str(),
        "SELECT" | "ASK" | "CONSTRUCT" | "DESCRIBE" | "PREFIX" | "BASE"
    )
}

/// Escape a string for inclusion in a SPARQL quoted literal.
#[must_use]
pub fn sparql_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Case-fold and strip combining marks for label alignment.
#[must_use]
pub fn fold_text(value: &str) -> String {
    value
        .nfd()
        .filter(|ch| !unicode_normalization::char::is_combining_mark(*ch))
        .collect::<String>()
        .to_lowercase()
}

/// Display form of an Oxigraph term.
#[must_use]
pub fn term_text(term: Option<&Term>) -> String {
    match term {
        Some(Term::Literal(lit)) => lit.value().to_string(),
        Some(Term::NamedNode(node)) => node.as_str().to_string(),
        Some(Term::BlankNode(node)) => format!("_:{}", node.as_str()),
        Some(Term::Triple(triple)) => format!("{triple}"),
        None => String::new(),
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(text)
        .trim()
        .chars()
        .take(ERROR_QUERY_PREVIEW_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_prefixes_are_injected_once() {
        let mut prefixes = BTreeMap::new();
        prefixes.insert(String::new(), "http://example.org/".to_string());
        let out = with_standard_prefixes("SELECT ?s WHERE { ?s :p ?o }", &prefixes);
        assert!(out.starts_with("PREFIX rdf: <"));
        assert!(out.contains("PREFIX : <http://example.org/>"));
        assert_eq!(out.matches("PREFIX rdfs:").count(), 1);
    }

    #[test]
    fn declared_prefixes_are_left_alone() {
        let out = with_standard_prefixes("PREFIX rdf: <x> SELECT ?s WHERE {}", &BTreeMap::new());
        assert_eq!(out.matches("PREFIX rdf:").count(), 1);
    }

    #[test]
    fn sparql_detection_is_keyword_led() {
        assert!(looks_like_sparql("  select ?s where { ?s ?p ?o }"));
        assert!(looks_like_sparql("PREFIX ex: <x> ASK { }"));
        assert!(!looks_like_sparql("qual o horário de sábado?"));
    }

    #[test]
    fn fold_text_strips_marks_and_case() {
        assert_eq!(fold_text("Horário SÁBADO"), "horario sabado");
    }

    #[test]
    fn debug_never_dumps_the_graph() {
        let store = FormalStore::from_documents(std::iter::empty()).expect("empty store");
        let rendered = format!("{store:?}");
        assert!(rendered.starts_with("FormalStore"));
        assert!(rendered.contains("stats"));
    }
}
