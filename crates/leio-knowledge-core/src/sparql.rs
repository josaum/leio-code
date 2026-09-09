//! Execute SPARQL against a [`FormalStore`] and build anchored-or-refuse envelopes.
//!
//! [`execute`] runs one query and reports what came back without deciding
//! what an empty result means. [`sparql_with_store`] applies the knowledge
//! contract on top: bindings ground the answer, no bindings refuse it. There
//! is no lattice trail here — `leio-code` layers that on for repo-backed
//! stores and reuses [`execute`] so both paths read the graph identically.
// Rust guideline compliant 2026-02-21

use std::collections::BTreeMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use oxigraph::sparql::QueryResults;
use serde_json::{Value, json};

use crate::envelope::{QueryEnvelope, SCHEMA_VERSION};
use crate::store::{FormalStore, term_text};

/// Envelope `kind` for SPARQL answers; shared with `leio-code knowledge explain`.
pub const SPARQL_ENVELOPE_KIND: &str = "knowledge_explain";

/// Confidence stamped on a grounded answer.
///
/// Bindings came straight from the graph, so the only residual doubt is the
/// caller's query shape; kept below 1.0 so downstream ranking can still
/// prefer exact-identity hits.
const CONFIDENCE_GROUNDED: f32 = 0.9;
/// Confidence stamped on `ASK = false`: a real answer, but a negative one.
const CONFIDENCE_ASK_FALSE: f32 = 0.4;
/// Confidence stamped on a refusal.
const CONFIDENCE_REFUSED: f32 = 0.0;
/// Skipped-document warnings surfaced per envelope.
///
/// Enough to see that a load is unhealthy without turning every answer into
/// a load report; the full list stays in [`FormalStore::stats`].
const MAX_SKIP_WARNINGS: usize = 8;

/// Result of one SPARQL execution, before the grounding contract is applied.
#[derive(Debug, Clone, PartialEq)]
pub enum SparqlOutcome {
    /// `SELECT` solutions, one JSON object per row (variable → display text).
    Solutions(Vec<Value>),
    /// `ASK` result.
    Boolean(bool),
    /// `CONSTRUCT` / `DESCRIBE` triples as `{s, p, o}` objects.
    Graph(Vec<Value>),
    /// Oxigraph rejected or failed the query; the payload is the cause.
    Error(String),
}

/// Run `query` against `formal`, returning at most `limit` rows.
///
/// `limit` is clamped to at least 1. Rows are display text via
/// [`term_text`], so literals lose their datatype and IRIs keep their full
/// form — the same rendering `leio-code` uses for its answers.
#[must_use]
pub fn execute(formal: &FormalStore, query: &str, limit: usize) -> SparqlOutcome {
    let limit = limit.max(1);
    match formal.query(query) {
        Ok(QueryResults::Solutions(solutions)) => {
            let mut rows = Vec::new();
            for solution in solutions {
                let Ok(solution) = solution else {
                    continue;
                };
                let mut row = BTreeMap::new();
                for (name, term) in solution.iter() {
                    row.insert(name.as_str().to_string(), json!(term_text(Some(term))));
                }
                rows.push(json!(row));
                if rows.len() >= limit {
                    break;
                }
            }
            SparqlOutcome::Solutions(rows)
        }
        Ok(QueryResults::Boolean(flag)) => SparqlOutcome::Boolean(flag),
        Ok(QueryResults::Graph(triples)) => {
            let mut rows = Vec::new();
            for triple in triples {
                let Ok(triple) = triple else {
                    continue;
                };
                rows.push(json!({
                    "s": triple.subject.to_string(),
                    "p": triple.predicate.to_string(),
                    "o": triple.object.to_string(),
                }));
                if rows.len() >= limit {
                    break;
                }
            }
            SparqlOutcome::Graph(rows)
        }
        Err(err) => SparqlOutcome::Error(err.to_string()),
    }
}

/// Column-major result of one SPARQL execution, for typed (Arrow) consumers.
///
/// Where [`execute`] renders rows as JSON objects, this keeps the query's
/// projection: `variables` is the ordered `SELECT` list and `columns[i]`
/// holds one entry per row for `variables[i]`, `None` where the row left the
/// variable unbound. A consumer can turn each column into a nullable string
/// array without walking JSON.
#[derive(Debug, Clone, PartialEq)]
pub enum SparqlTable {
    /// `SELECT` projection, one column per variable in query order.
    Solutions {
        variables: Vec<String>,
        columns: Vec<Vec<Option<String>>>,
        rows: usize,
    },
    /// `ASK` result.
    Boolean(bool),
    /// `CONSTRUCT` / `DESCRIBE` triples as three parallel columns.
    Graph {
        subjects: Vec<String>,
        predicates: Vec<String>,
        objects: Vec<String>,
    },
    /// Oxigraph rejected or failed the query; the payload is the cause.
    Error(String),
}

/// Run `query` against `formal` and keep the projection as columns.
///
/// `limit` is clamped to at least 1 and caps the rows (or triples). Term
/// rendering matches [`execute`] and [`term_text`].
#[must_use]
pub fn execute_table(formal: &FormalStore, query: &str, limit: usize) -> SparqlTable {
    let limit = limit.max(1);
    match formal.query(query) {
        Ok(QueryResults::Solutions(solutions)) => {
            let variables: Vec<String> = solutions
                .variables()
                .iter()
                .map(|v| v.as_str().to_string())
                .collect();
            let mut columns: Vec<Vec<Option<String>>> = vec![Vec::new(); variables.len()];
            let mut rows = 0usize;
            for solution in solutions {
                let Ok(solution) = solution else {
                    continue;
                };
                for (index, column) in columns.iter_mut().enumerate() {
                    column.push(solution.get(index).map(|term| term_text(Some(term))));
                }
                rows += 1;
                if rows >= limit {
                    break;
                }
            }
            SparqlTable::Solutions {
                variables,
                columns,
                rows,
            }
        }
        Ok(QueryResults::Boolean(flag)) => SparqlTable::Boolean(flag),
        Ok(QueryResults::Graph(triples)) => {
            let mut subjects = Vec::new();
            let mut predicates = Vec::new();
            let mut objects = Vec::new();
            for triple in triples {
                let Ok(triple) = triple else {
                    continue;
                };
                subjects.push(triple.subject.to_string());
                predicates.push(triple.predicate.to_string());
                objects.push(triple.object.to_string());
                if subjects.len() >= limit {
                    break;
                }
            }
            SparqlTable::Graph {
                subjects,
                predicates,
                objects,
            }
        }
        Err(err) => SparqlTable::Error(err.to_string()),
    }
}

/// Answer `query` from `formal` under the anchored-or-refuse contract.
///
/// The envelope's `meta.grounded` is `true` only when the graph produced
/// bindings (or `ASK` returned `true`). An empty result, an empty graph, or a
/// SPARQL error is a refusal: confidence 0, an `ungrounded` warning, and a
/// one-step `refuse` reasoning trail. `limit` caps the rows returned and is
/// clamped to at least 1.
///
/// # Examples
///
/// ```
/// use leio_knowledge_core::{FormalStore, RdfFormat, sparql_with_store};
///
/// let store = FormalStore::from_documents(std::iter::empty::<(&str, RdfFormat, &[u8])>())?;
/// let envelope = sparql_with_store(&store, "SELECT ?s WHERE { ?s ?p ?o }", 5);
/// assert!(envelope.entities.is_empty());
/// assert!(envelope.warnings.iter().any(|w| w == "ungrounded"));
/// # Ok::<(), anyhow::Error>(())
/// ```
#[must_use]
pub fn sparql_with_store(formal: &FormalStore, query: &str, limit: usize) -> QueryEnvelope {
    let started = Instant::now();
    let sparql = vec![query.to_string()];
    match execute(formal, query, limit) {
        SparqlOutcome::Solutions(rows) => {
            if rows.is_empty() {
                return refuse(
                    query,
                    "refused: SPARQL returned no solutions",
                    formal,
                    started,
                    sparql,
                );
            }
            let summary = format!("SPARQL returned {} solution(s)", rows.len());
            let mut envelope = base_envelope(
                &summary,
                CONFIDENCE_GROUNDED,
                rows,
                formal,
                started,
                sparql,
                true,
            );
            if let Some(meta) = envelope.meta.as_mut() {
                meta["mode"] = json!("sparql");
            }
            envelope
        }
        SparqlOutcome::Boolean(flag) => base_envelope(
            &format!("SPARQL ASK = {flag}"),
            if flag {
                CONFIDENCE_GROUNDED
            } else {
                CONFIDENCE_ASK_FALSE
            },
            vec![json!({ "boolean": flag })],
            formal,
            started,
            sparql,
            flag,
        ),
        SparqlOutcome::Graph(rows) => {
            if rows.is_empty() {
                return refuse(
                    query,
                    "refused: SPARQL graph was empty",
                    formal,
                    started,
                    sparql,
                );
            }
            let summary = format!("SPARQL graph returned {} triple(s)", rows.len());
            base_envelope(
                &summary,
                CONFIDENCE_GROUNDED,
                rows,
                formal,
                started,
                sparql,
                true,
            )
        }
        SparqlOutcome::Error(err) => refuse(
            query,
            &format!("refused: SPARQL error ({err})"),
            formal,
            started,
            sparql,
        ),
    }
}

/// Build a refusal envelope for `needle` without running any query.
///
/// For callers that must decline before touching the graph — the question
/// is not SPARQL and the runtime has no lexical grounding, say — while still
/// returning the same envelope shape as a refused query: confidence 0, an
/// `ungrounded` warning, and a one-step `refuse` reasoning trail that carries
/// `summary`.
#[must_use]
pub fn refusal(formal: &FormalStore, needle: &str, summary: &str) -> QueryEnvelope {
    refuse(needle, summary, formal, Instant::now(), Vec::new())
}

fn refuse(
    needle: &str,
    summary: &str,
    formal: &FormalStore,
    started: Instant,
    sparql: Vec<String>,
) -> QueryEnvelope {
    let mut envelope = base_envelope(
        summary,
        CONFIDENCE_REFUSED,
        Vec::new(),
        formal,
        started,
        sparql,
        false,
    );
    envelope.warnings.push("ungrounded".to_string());
    if let Some(meta) = envelope.meta.as_mut() {
        meta["needle"] = json!(needle);
        meta["reasoning"] = json!([{
            "step": 1,
            "kind": "refuse",
            "detail": summary,
        }]);
    }
    envelope
}

fn base_envelope(
    summary: &str,
    confidence: f32,
    entities: Vec<Value>,
    formal: &FormalStore,
    started: Instant,
    sparql: Vec<String>,
    grounded: bool,
) -> QueryEnvelope {
    let stats = &formal.stats;
    QueryEnvelope {
        schema_version: SCHEMA_VERSION.to_string(),
        query_id: format!("{SPARQL_ENVELOPE_KIND}-{}", unix_nanos()),
        kind: SPARQL_ENVELOPE_KIND.to_string(),
        summary: summary.to_string(),
        confidence,
        entities,
        evidence: Vec::new(),
        warnings: stats
            .files_failed
            .iter()
            .take(MAX_SKIP_WARNINGS)
            .map(|(path, err)| format!("rdf skip {path}: {err}"))
            .collect(),
        meta: Some(json!({
            "grounded": grounded,
            "transport": "sparql",
            "sparql": sparql,
            "prefixes": stats.prefixes,
            "graphs": {
                "files_ok": stats.files_ok,
                "files_failed": stats.files_failed.len(),
                "triples": stats.triples,
                "citations": stats.citations,
                "alignments": stats.alignments,
                "cache": stats.cache,
                "source_mtime": stats.source_mtime,
                "prefixes": stats.prefixes,
            },
            "knowledge_base": { "mode": "formal_sparql" },
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::RdfFormat;

    const GYM: &[u8] = b"@prefix ex: <http://example.org/gym#> .
ex:unitA ex:opensAt \"06:00\" .
ex:unitB ex:opensAt \"07:00\" .
";

    fn store() -> FormalStore {
        FormalStore::from_documents([("gym.ttl", RdfFormat::Turtle, GYM)]).expect("store")
    }

    fn grounded(envelope: &QueryEnvelope) -> Option<bool> {
        envelope.meta.as_ref()?.get("grounded")?.as_bool()
    }

    #[test]
    fn select_rows_are_grounded_and_capped() {
        let outcome = execute(
            &store(),
            "PREFIX ex: <http://example.org/gym#> SELECT ?u WHERE { ?u ex:opensAt ?o }",
            1,
        );
        assert!(matches!(outcome, SparqlOutcome::Solutions(ref rows) if rows.len() == 1));
    }

    #[test]
    fn empty_select_refuses() {
        let envelope = sparql_with_store(
            &store(),
            "PREFIX ex: <http://example.org/gym#> SELECT ?u WHERE { ?u ex:closesAt ?o }",
            10,
        );
        assert_eq!(grounded(&envelope), Some(false));
        assert!(envelope.warnings.iter().any(|w| w == "ungrounded"));
        assert!(envelope.entities.is_empty());
        assert_eq!(envelope.confidence, CONFIDENCE_REFUSED);
    }

    #[test]
    fn ask_false_is_an_answer_not_a_refusal() {
        let envelope = sparql_with_store(
            &store(),
            "PREFIX ex: <http://example.org/gym#> ASK { ex:unitZ ex:opensAt ?o }",
            10,
        );
        assert_eq!(grounded(&envelope), Some(false));
        assert!(!envelope.warnings.iter().any(|w| w == "ungrounded"));
        assert_eq!(envelope.confidence, CONFIDENCE_ASK_FALSE);
    }

    #[test]
    fn broken_query_refuses_with_the_cause() {
        let envelope = sparql_with_store(&store(), "SELECT ?u WHERE { broken", 10);
        assert_eq!(grounded(&envelope), Some(false));
        assert!(envelope.summary.starts_with("refused: SPARQL error"));
    }

    #[test]
    fn table_keeps_projection_order_and_unbound_as_null() {
        let table = execute_table(
            &store(),
            "PREFIX ex: <http://example.org/gym#> SELECT ?o ?missing ?u WHERE { ?u ex:opensAt ?o } ORDER BY ?o",
            10,
        );
        let SparqlTable::Solutions {
            variables,
            columns,
            rows,
        } = table
        else {
            panic!("expected solutions");
        };
        assert_eq!(variables, vec!["o", "missing", "u"]);
        assert_eq!(rows, 2);
        assert_eq!(columns[0][0].as_deref(), Some("06:00"));
        assert_eq!(columns[1][0], None);
        assert!(columns[2][0].as_deref().unwrap().ends_with("#unitA"));
    }

    #[test]
    fn construct_returns_triples() {
        let outcome = execute(
            &store(),
            "PREFIX ex: <http://example.org/gym#> CONSTRUCT { ?u ex:opensAt ?o } WHERE { ?u ex:opensAt ?o }",
            10,
        );
        assert!(matches!(outcome, SparqlOutcome::Graph(ref rows) if rows.len() == 2));
    }
}
