//! SPARQL-gated knowledge answers with a lattice reasoning trail.
//!
//! `knowledge explain` binds the needle through SPARQL (or refuses). The
//! compiled wiki and concept lattice are the trail: heading, cover parents,
//! and functor witnesses. They never substitute for a missing binding.
// Rust guideline compliant 2026-02-21

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use leio_knowledge_core::sparql::{SparqlOutcome, execute};
use oxigraph::sparql::QueryResults;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::knowledge_graph::{
    self, FormalStore, fold_text, looks_like_sparql, sparql_escape, term_text,
};
use crate::lattice::{self, FunctorWitness, LatticeArtifact};
use crate::model::{EvidenceItem, QueryEnvelope, SCHEMA_VERSION};

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "as", "at", "by", "com", "da", "das", "de", "do", "dos", "e", "em", "for",
    "from", "in", "na", "nas", "no", "nos", "o", "of", "on", "or", "os", "para", "the", "to", "um",
    "uma", "with",
];
const MAX_FACTS: usize = 48;
const MAX_SUBJECTS: usize = 8;
const MIN_AND_SCORE: f64 = 60.0;

/// Ground `needle` in the formal graph or refuse.
///
/// # Errors
///
/// Returns an error when the formal store cannot be built.
pub fn explain_knowledge(repo_root: &Path, needle: &str, limit: usize) -> Result<QueryEnvelope> {
    explain_knowledge_scoped(repo_root, needle, limit, None, None)
}

/// Ground `needle`, preferring IRIs cited by a wiki heading (`leio:cites`).
///
/// # Errors
///
/// Returns an error when the formal store cannot be built.
pub fn explain_knowledge_scoped(
    repo_root: &Path,
    needle: &str,
    limit: usize,
    section_id: Option<&str>,
    pinned_iri: Option<&str>,
) -> Result<QueryEnvelope> {
    let started = Instant::now();
    let limit = limit.clamp(1, MAX_SUBJECTS);
    let formal = knowledge_graph::load_formal_store(repo_root)?;
    let lattice = lattice::load_lattice(&lattice::lattice_path(repo_root))?.unwrap_or_default();

    if looks_like_sparql(needle) {
        return Ok(sparql_to_envelope(
            needle, &formal, &lattice, started, limit, false,
        ));
    }

    let tokens = query_tokens(needle);
    if tokens.is_empty() {
        return Ok(refuse(
            needle,
            "refused: no content tokens to ground",
            &formal,
            &lattice,
            started,
            Vec::new(),
        ));
    }

    if let Some(pin) = pinned_iri.filter(|iri| is_ontology_subject(iri))
        && let Some(subject) = describe_subject(&formal, &lattice, pin, &tokens)?
        && subject.score >= MIN_AND_SCORE
    {
        let mut envelope = proof_envelope(
            needle,
            vec![subject],
            &formal,
            &lattice,
            started,
            vec![format!("PIN <{pin}>")],
        );
        if let Some(meta) = envelope.meta.as_mut() {
            meta["scope"] = json!(section_id);
            meta["scoped"] = json!(true);
            meta["pinned"] = json!(pin);
        }
        return Ok(envelope);
    }

    let mut used_scope = false;
    let mut used_identity = false;
    let mut sparql = grounding_sparql(&tokens);
    let mut candidates = Vec::new();
    if let Some(section) = section_id.filter(|id| !id.is_empty()) {
        let scoped = cited_subjects_sparql(section);
        match collect_subjects(&formal, &scoped) {
            Ok(rows) if !rows.is_empty() => {
                used_scope = true;
                sparql = scoped;
                candidates = rows;
            }
            Ok(_) => {}
            Err(err) => {
                return Ok(refuse(
                    needle,
                    &format!("refused: SPARQL error ({err})"),
                    &formal,
                    &lattice,
                    started,
                    vec![scoped],
                ));
            }
        }
    }
    if candidates.is_empty() {
        let id_query = identity_sparql(&tokens);
        match collect_subjects(&formal, &id_query) {
            Ok(id_hits) if id_hits.len() == 1 => {
                used_identity = true;
                sparql = id_query;
                candidates = id_hits;
            }
            Ok(id_hits) if !id_hits.is_empty() => {
                let fact_query = grounding_sparql(&tokens);
                let fact_hits = collect_subjects(&formal, &fact_query).unwrap_or_default();
                let fact_set: BTreeSet<String> = fact_hits.into_iter().collect();
                let inter: Vec<String> = id_hits
                    .iter()
                    .filter(|iri| fact_set.contains(*iri))
                    .cloned()
                    .collect();
                used_identity = true;
                sparql = id_query;
                candidates = if inter.is_empty() { id_hits } else { inter };
            }
            Ok(_) => {}
            Err(err) => {
                return Ok(refuse(
                    needle,
                    &format!("refused: SPARQL error ({err})"),
                    &formal,
                    &lattice,
                    started,
                    vec![id_query],
                ));
            }
        }
    }
    if candidates.is_empty() {
        sparql = grounding_sparql(&tokens);
        candidates = match collect_subjects(&formal, &sparql) {
            Err(err) => {
                return Ok(refuse(
                    needle,
                    &format!("refused: SPARQL error ({err})"),
                    &formal,
                    &lattice,
                    started,
                    vec![sparql],
                ));
            }
            Ok(rows) => rows,
        };
    }
    if candidates.is_empty() {
        return Ok(refuse(
            needle,
            &format!("refused: no SPARQL binding for `{needle}`"),
            &formal,
            &lattice,
            started,
            vec![sparql],
        ));
    }

    let mut grounded = Vec::new();
    for iri in candidates {
        if let Some(subject) = describe_subject(&formal, &lattice, &iri, &tokens)? {
            grounded.push(subject);
        }
        if grounded.len() >= limit {
            break;
        }
    }
    grounded.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.iri.cmp(&right.iri))
    });
    grounded.retain(|row| row.score >= MIN_AND_SCORE);
    grounded.truncate(limit);

    if grounded.is_empty() && used_scope {
        used_scope = false;
        sparql = grounding_sparql(&tokens);
        candidates = collect_subjects(&formal, &sparql).unwrap_or_default();
        for iri in candidates {
            if let Some(subject) = describe_subject(&formal, &lattice, &iri, &tokens)? {
                grounded.push(subject);
            }
        }
        grounded.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.iri.cmp(&right.iri))
        });
        grounded.retain(|row| row.score >= MIN_AND_SCORE);
        grounded.truncate(limit);
    }
    if grounded.is_empty() {
        return Ok(refuse(
            needle,
            &format!("refused: SPARQL hits for `{needle}` lack token coverage"),
            &formal,
            &lattice,
            started,
            vec![sparql],
        ));
    }
    if !used_scope && is_ambiguous(&grounded) {
        return Ok(refuse(
            needle,
            &format!(
                "refused: ambiguous SPARQL binding for `{needle}` ({} subjects)",
                grounded.len()
            ),
            &formal,
            &lattice,
            started,
            vec![sparql],
        ));
    }

    let mut envelope = proof_envelope(needle, grounded, &formal, &lattice, started, vec![sparql]);
    if let Some(meta) = envelope.meta.as_mut() {
        meta["scope"] = json!(section_id);
        meta["scoped"] = json!(used_scope);
        meta["identity"] = json!(used_identity);
    }
    Ok(envelope)
}

/// Execute a raw SPARQL query against the formal graph.
///
/// # Errors
///
/// Returns an error when the formal store cannot be built.
pub fn sparql_knowledge(repo_root: &Path, query: &str, limit: usize) -> Result<QueryEnvelope> {
    let started = Instant::now();
    let formal = knowledge_graph::load_formal_store(repo_root)?;
    let lattice = lattice::load_lattice(&lattice::lattice_path(repo_root))?.unwrap_or_default();
    let mut envelope = sparql_to_envelope(query, &formal, &lattice, started, limit, true);
    envelope.kind = "knowledge_sparql".to_string();
    Ok(envelope)
}

/// SPARQL against a store the caller already holds, with the same envelope.
///
/// `sparql_knowledge` derives both the store and the lattice from `repo_root`.
/// A caller that embedded the ontology — in a container image, say — has the
/// graph but no repo, and the lattice trail is empty by construction there.
/// Returning the same [`QueryEnvelope`] keeps `grounded` meaning exactly what
/// it means elsewhere: the answer came from bindings, or it refuses.
#[must_use]
pub fn sparql_with_store(store: &FormalStore, query: &str, limit: usize) -> QueryEnvelope {
    let started = Instant::now();
    let limit = limit.clamp(1, MAX_SUBJECTS);
    sparql_to_envelope(
        query,
        store,
        &LatticeArtifact::default(),
        started,
        limit,
        false,
    )
}

/// One JSON-line SPARQL request for [`exec_sparql`].
#[derive(Debug, Deserialize)]
struct ExecRequest {
    query: String,
    #[serde(default)]
    limit: Option<usize>,
}

/// Answer many SPARQL queries against one loaded formal store.
///
/// Each stdin line is `{"query":"...","limit":N}`. Each stdout line is a
/// [`QueryEnvelope`]. Empty SELECT/CONSTRUCT results are valid zero-entity
/// answers, not explain-style refusals. Tenant logic does not belong here.
///
/// # Errors
///
/// Returns an error when the formal store cannot be built or stdin/stdout fails.
pub fn exec_sparql(repo_root: &Path, default_limit: usize) -> Result<()> {
    use std::io::{self, BufRead, Write};

    let formal = knowledge_graph::load_formal_store(repo_root)?;
    let lattice = lattice::load_lattice(&lattice::lattice_path(repo_root))?.unwrap_or_default();
    let default_limit = default_limit.max(1);
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let started = Instant::now();
        let req: ExecRequest = serde_json::from_str(&line)?;
        let limit = req.limit.unwrap_or(default_limit).max(1);
        let mut envelope = sparql_to_envelope(&req.query, &formal, &lattice, started, limit, true);
        envelope.kind = "knowledge_sparql".to_string();
        writeln!(stdout, "{}", serde_json::to_string(&envelope)?)?;
        stdout.flush()?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct GroundedSubject {
    iri: String,
    label: String,
    score: f64,
    exact_identity: bool,
    facts: Vec<(String, String)>,
    heading_path: Option<String>,
    section: Option<String>,
    concept: Option<String>,
    parents: Vec<String>,
    sources: Vec<String>,
}

fn query_tokens(needle: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut tokens = Vec::new();
    for raw in needle.split(|ch: char| !ch.is_alphanumeric()) {
        if raw.len() < 2 {
            continue;
        }
        let folded = fold_text(raw);
        if STOPWORDS.contains(&folded.as_str()) {
            continue;
        }
        if seen.insert(folded.clone()) {
            tokens.push(folded);
        }
    }
    tokens
}

fn cited_subjects_sparql(section_id: &str) -> String {
    let escaped = sparql_escape(section_id);
    let path = section_id.strip_prefix("section:").unwrap_or(section_id);
    let path = path.rsplit_once('#').map(|(head, _)| head).unwrap_or(path);
    let path_esc = sparql_escape(path);
    format!(
        r#"
PREFIX leio: <{}>
SELECT DISTINCT ?s WHERE {{
  {{
    ?sec leio:section "{escaped}" .
    ?sec leio:cites ?s .
  }} UNION {{
    ?sec leio:cites ?s .
    FILTER(CONTAINS(STR(?sec), "{path_esc}"))
  }}
}}
LIMIT 16
"#,
        knowledge_graph::KNOWLEDGE_NS
    )
}

fn identity_sparql(tokens: &[String]) -> String {
    let filters = tokens
        .iter()
        .map(|token| {
            let escaped = sparql_escape(token);
            format!(
                "CONTAINS(LCASE(CONCAT(STR(?s), \" \", COALESCE(STR(?label), \"\"), \" \", COALESCE(STR(?heading), \"\"))), \"{escaped}\")"
            )
        })
        .collect::<Vec<_>>()
        .join(" &&\n    ");
    format!(
        r#"
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX leio: <{}>
SELECT DISTINCT ?s WHERE {{
  {{
    ?s rdfs:label ?label .
  }} UNION {{
    ?sec leio:cites ?s .
    ?sec leio:heading ?heading .
  }}
  FILTER(
    {filters}
  )
}}
LIMIT 40
"#,
        knowledge_graph::KNOWLEDGE_NS
    )
}

fn is_ontology_subject(iri: &str) -> bool {
    (iri.starts_with("http://") || iri.starts_with("https://") || iri.starts_with("urn:"))
        && !iri.starts_with("https://example.local/leio/")
}

fn grounding_sparql(tokens: &[String]) -> String {
    let mut body = String::from("SELECT DISTINCT ?s WHERE {\n");
    for (idx, token) in tokens.iter().enumerate() {
        let escaped = sparql_escape(token);
        body.push_str(&format!(
            "  ?s ?p{idx} ?o{idx} .\n  FILTER(\n    CONTAINS(LCASE(STR(?s)), \"{escaped}\") ||\n    CONTAINS(LCASE(STR(?p{idx})), \"{escaped}\") ||\n    CONTAINS(LCASE(STR(?o{idx})), \"{escaped}\")\n  )\n"
        ));
    }
    body.push_str("}\nLIMIT 80\n");
    body
}

fn collect_subjects(formal: &FormalStore, sparql: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    if let QueryResults::Solutions(solutions) = formal.query(sparql)? {
        for solution in solutions {
            let solution = solution?;
            let iri = term_text(solution.get("s"));
            if (iri.starts_with("http") || iri.starts_with("urn:")) && seen.insert(iri.clone()) {
                out.push(iri);
            }
        }
    }
    Ok(out)
}

fn describe_subject(
    formal: &FormalStore,
    lattice: &LatticeArtifact,
    iri: &str,
    tokens: &[String],
) -> Result<Option<GroundedSubject>> {
    let escaped = sparql_escape(iri);
    let sparql = format!(
        r#"
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
SELECT ?p ?o WHERE {{
  {{ <{escaped}> ?p ?o }}
  UNION {{
    ?st a rdf:Statement ;
        rdf:subject <{escaped}> ;
        rdf:predicate ?p ;
        rdf:object ?o .
  }}
}}
LIMIT {MAX_FACTS}
"#
    );
    let mut facts = Vec::new();
    let mut label = String::new();
    if let QueryResults::Solutions(solutions) = formal.query(&sparql)? {
        for solution in solutions {
            let solution = solution?;
            let predicate = term_text(solution.get("p"));
            let object = term_text(solution.get("o"));
            if predicate.ends_with("label") && label.is_empty() {
                label = object.clone();
            }
            facts.push((predicate, object));
        }
    }
    if facts.is_empty() {
        return Ok(None);
    }
    if label.is_empty() {
        label = local_name(iri).to_string();
    }

    let types: Vec<String> = facts
        .iter()
        .filter(|(pred, _)| {
            pred.ends_with("#type")
                || pred.ends_with("/type")
                || pred.ends_with("22-rdf-syntax-ns#type")
        })
        .map(|(_, obj)| obj.clone())
        .collect();
    let trail = lattice_trail(formal, lattice, iri, &label)?;
    let heading = trail.0.clone().unwrap_or_default();
    let identity = fold_text(&format!(
        "{label} {heading} {} {}",
        local_name(iri),
        types
            .iter()
            .map(|ty| local_name(ty).to_string())
            .collect::<Vec<_>>()
            .join(" ")
    ));
    let fact_blob = fold_text(
        &facts
            .iter()
            .map(|(pred, obj)| format!("{} {obj}", local_name(pred)))
            .collect::<Vec<_>>()
            .join(" "),
    );
    let identity_hits = tokens
        .iter()
        .filter(|token| identity.contains(token.as_str()))
        .count();
    let fact_hits = tokens
        .iter()
        .filter(|token| fact_blob.contains(token.as_str()) || identity.contains(token.as_str()))
        .count();
    if fact_hits == 0 {
        return Ok(None);
    }
    let exact_label = fold_text(&label) == tokens.join(" ")
        || tokens
            .iter()
            .any(|token| fold_text(&label) == *token || fold_text(local_name(iri)) == *token);
    let exact_heading = !heading.is_empty()
        && (fold_text(&heading) == tokens.join(" ")
            || tokens.iter().any(|token| {
                fold_text(&heading) == *token
                    || heading.split(" > ").any(|part| fold_text(part) == *token)
            }));
    let exact_identity = exact_label || exact_heading;
    let score = if exact_identity {
        100.0
    } else if identity_hits == tokens.len() {
        88.0
    } else if identity_hits > 0 {
        55.0 + 12.0 * identity_hits as f64
    } else if fact_hits == tokens.len() {
        70.0
    } else {
        30.0 * (fact_hits as f64 / tokens.len() as f64)
    };

    let sources = stated_in_sources(formal, iri)?;
    Ok(Some(GroundedSubject {
        iri: iri.to_string(),
        label,
        score,
        exact_identity,
        facts,
        heading_path: trail.0,
        section: trail.1,
        concept: trail.2,
        parents: trail.3,
        sources,
    }))
}

fn stated_in_sources(formal: &FormalStore, iri: &str) -> Result<Vec<String>> {
    let escaped = sparql_escape(iri);
    let sparql = format!(
        r#"
PREFIX leio: <{}>
SELECT DISTINCT ?sid ?sec WHERE {{
  <{escaped}> leio:statedIn ?sec .
  OPTIONAL {{ ?sec leio:section ?sid }}
}}
LIMIT 8
"#,
        knowledge_graph::KNOWLEDGE_NS
    );
    let mut out = Vec::new();
    if let QueryResults::Solutions(solutions) = formal.query(&sparql)? {
        for solution in solutions {
            let solution = solution?;
            let sid = term_text(solution.get("sid"));
            let sec = term_text(solution.get("sec"));
            if !sid.is_empty() {
                out.push(sid);
            } else if !sec.is_empty() {
                out.push(sec);
            }
        }
    }
    Ok(out)
}

fn is_ambiguous(grounded: &[GroundedSubject]) -> bool {
    if grounded.len() < 2 {
        return false;
    }
    if grounded[0].exact_identity && grounded[0].score - grounded[1].score >= 8.0 {
        return false;
    }
    grounded[0].score - grounded[1].score < 8.0
}

/// The lattice reasoning trail for one subject: `(heading_path, section,
/// concept, parents)` — mirrors the fields of the same name on the answer.
type LatticeTrail = (Option<String>, Option<String>, Option<String>, Vec<String>);

fn lattice_trail(
    formal: &FormalStore,
    lattice: &LatticeArtifact,
    iri: &str,
    label: &str,
) -> Result<LatticeTrail> {
    let escaped = sparql_escape(iri);
    let cites = format!(
        r#"
PREFIX leio: <{}>
SELECT ?section ?heading ?concept ?sid WHERE {{
  ?section leio:cites <{escaped}> .
  OPTIONAL {{ ?section leio:heading ?heading }}
  OPTIONAL {{ ?section leio:concept ?concept }}
  OPTIONAL {{ ?section leio:section ?sid }}
}}
LIMIT 8
"#,
        knowledge_graph::KNOWLEDGE_NS
    );
    let mut heading = None;
    let mut section = None;
    let mut concept = None;
    if let QueryResults::Solutions(solutions) = formal.query(&cites)? {
        for solution in solutions {
            let solution = solution?;
            if heading.is_none() {
                let value = term_text(solution.get("heading"));
                if !value.is_empty() {
                    heading = Some(value);
                }
            }
            if section.is_none() {
                let value = term_text(solution.get("sid"));
                if !value.is_empty() {
                    section = Some(value);
                }
            }
            if concept.is_none() {
                let value = term_text(solution.get("concept"));
                if !value.is_empty() {
                    concept = Some(value);
                }
            }
        }
    }
    if section.is_none()
        && let Some(object) = lattice::heading_for(lattice, label)
    {
        heading = Some(object.heading_path.clone());
        section = Some(object.id.clone());
        if let Some(membership) = lattice.memberships.get(&object.id) {
            concept = Some(membership.concept_id.clone());
        }
    }
    if concept.is_none()
        && let Some(id) = section.as_deref()
        && let Some(membership) = lattice.memberships.get(id)
    {
        concept = Some(membership.concept_id.clone());
    }
    let mut parents = Vec::new();
    if let Some(id) = concept.as_deref()
        && let Some(node) = lattice.concepts.iter().find(|row| row.id == id)
    {
        parents = node.parents.clone();
        if heading.is_none() {
            heading = Some(node.label.clone());
        }
    }
    Ok((heading, section, concept, parents))
}

fn sparql_to_envelope(
    query: &str,
    formal: &FormalStore,
    lattice: &LatticeArtifact,
    started: Instant,
    limit: usize,
    empty_ok: bool,
) -> QueryEnvelope {
    match execute(formal, query, limit) {
        SparqlOutcome::Solutions(rows) => {
            if rows.is_empty() && !empty_ok {
                return refuse(
                    query,
                    "refused: SPARQL returned no solutions",
                    formal,
                    lattice,
                    started,
                    vec![query.to_string()],
                );
            }
            let mut envelope = base_envelope(
                "knowledge_explain",
                &format!("SPARQL returned {} solution(s)", rows.len()),
                0.9,
                rows,
                formal,
                lattice,
                started,
                vec![query.to_string()],
                true,
            );
            if let Some(meta) = envelope.meta.as_mut() {
                meta["mode"] = json!("sparql");
            }
            envelope
        }
        SparqlOutcome::Boolean(flag) => base_envelope(
            "knowledge_explain",
            &format!("SPARQL ASK = {flag}"),
            if flag { 0.9 } else { 0.4 },
            vec![json!({ "boolean": flag })],
            formal,
            lattice,
            started,
            vec![query.to_string()],
            flag,
        ),
        SparqlOutcome::Graph(rows) => {
            if rows.is_empty() && !empty_ok {
                return refuse(
                    query,
                    "refused: SPARQL graph was empty",
                    formal,
                    lattice,
                    started,
                    vec![query.to_string()],
                );
            }
            base_envelope(
                "knowledge_explain",
                &format!("SPARQL graph returned {} triple(s)", rows.len()),
                0.9,
                rows,
                formal,
                lattice,
                started,
                vec![query.to_string()],
                true,
            )
        }
        SparqlOutcome::Error(err) => refuse(
            query,
            &format!("refused: SPARQL error ({err})"),
            formal,
            lattice,
            started,
            vec![query.to_string()],
        ),
    }
}

fn proof_envelope(
    needle: &str,
    subjects: Vec<GroundedSubject>,
    formal: &FormalStore,
    lattice: &LatticeArtifact,
    started: Instant,
    sparql: Vec<String>,
) -> QueryEnvelope {
    let summary = subjects
        .first()
        .map(|row| {
            let fact = row
                .facts
                .iter()
                .filter(|(pred, _)| fact_rank(pred) < 9)
                .min_by_key(|(pred, _)| fact_rank(pred))
                .map(|(pred, obj)| format!("{} = {obj}", local_name(pred)));
            match fact {
                Some(detail) => format!("grounded `{}`: {} ({detail})", row.label, row.iri),
                None => format!("grounded `{}`: {}", row.label, row.iri),
            }
        })
        .unwrap_or_else(|| format!("grounded `{needle}`"));
    let entities = subjects
        .iter()
        .map(|row| {
            json!({
                "iri": row.iri,
                "label": row.label,
                "score": row.score,
                "facts": row.facts.iter().map(|(p, o)| json!({
                    "predicate": p,
                    "predicate_local": local_name(p),
                    "object": o,
                })).collect::<Vec<_>>(),
                "heading_path": row.heading_path,
                "section": row.section,
                "concept": row.concept,
                "parents": row.parents,
                "sources": row.sources,
            })
        })
        .collect::<Vec<_>>();
    let mut envelope = base_envelope(
        "knowledge_explain",
        &summary,
        0.92,
        entities,
        formal,
        lattice,
        started,
        sparql,
        true,
    );
    envelope.evidence = subjects
        .iter()
        .flat_map(|row| {
            let mut items = vec![EvidenceItem {
                kind: "iri".to_string(),
                path: row.section.clone().unwrap_or_else(|| row.iri.clone()),
                line: None,
                detail: format!("{} <{}>", row.label, row.iri),
            }];
            if let Some(heading) = &row.heading_path {
                items.push(EvidenceItem {
                    kind: "heading".to_string(),
                    path: row.section.clone().unwrap_or_default(),
                    line: None,
                    detail: heading.clone(),
                });
            }
            if let Some(concept) = &row.concept {
                items.push(EvidenceItem {
                    kind: "concept".to_string(),
                    path: concept.clone(),
                    line: None,
                    detail: format!("parents={}", row.parents.join(",")),
                });
            }
            items
        })
        .collect();
    if let Some(meta) = envelope.meta.as_mut() {
        meta["reasoning"] = json!(reasoning_steps(&subjects, lattice));
        meta["needle"] = json!(needle);
    }
    envelope
}

fn reasoning_steps(subjects: &[GroundedSubject], lattice: &LatticeArtifact) -> Vec<Value> {
    let mut steps = vec![json!({
        "step": 1,
        "kind": "sparql",
        "detail": "bind subjects whose triples cover every query token",
    })];
    let mut n = 2u32;
    if let Some(row) = subjects.first() {
        steps.push(json!({
            "step": n,
            "kind": "binding",
            "iri": row.iri,
            "label": row.label,
        }));
        n += 1;
        for (pred, obj) in row.facts.iter().take(6) {
            steps.push(json!({
                "step": n,
                "kind": "fact",
                "predicate": local_name(pred),
                "object": obj,
            }));
            n += 1;
        }
        if let Some(heading) = &row.heading_path {
            steps.push(json!({
                "step": n,
                "kind": "heading",
                "heading_path": heading,
                "section": row.section,
            }));
            n += 1;
        }
        if let Some(concept) = &row.concept {
            steps.push(json!({
                "step": n,
                "kind": "lattice",
                "concept": concept,
                "parents": row.parents,
            }));
            n += 1;
        }
    }
    for witness in functor_values(lattice) {
        steps.push(json!({
            "step": n,
            "kind": "functor",
            "name": witness.name,
            "coherence": witness.coherence,
            "preserved": witness.preserved,
            "total": witness.total,
        }));
        n += 1;
    }
    let _ = n;
    steps
}

fn refuse(
    needle: &str,
    summary: &str,
    formal: &FormalStore,
    lattice: &LatticeArtifact,
    started: Instant,
    sparql: Vec<String>,
) -> QueryEnvelope {
    let mut envelope = base_envelope(
        "knowledge_explain",
        summary,
        0.0,
        Vec::new(),
        formal,
        lattice,
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

// Nine arguments, but eight distinct types: this is the flat constructor for
// `QueryEnvelope`'s own fields, so bundling them into a parameter struct would
// just be a second spelling of the envelope itself. Transposition is caught by
// the type checker rather than by naming.
#[allow(clippy::too_many_arguments)]
fn base_envelope(
    kind: &str,
    summary: &str,
    confidence: f32,
    entities: Vec<Value>,
    formal: &FormalStore,
    lattice: &LatticeArtifact,
    started: Instant,
    sparql: Vec<String>,
    grounded: bool,
) -> QueryEnvelope {
    QueryEnvelope {
        schema_version: SCHEMA_VERSION.to_string(),
        query_id: format!(
            "{kind}-{}",
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ),
        kind: kind.to_string(),
        summary: summary.to_string(),
        confidence,
        entities,
        evidence: Vec::new(),
        warnings: formal
            .stats
            .files_failed
            .iter()
            .take(8)
            .map(|(path, err)| format!("rdf skip {path}: {err}"))
            .collect(),
        meta: Some(json!({
            "grounded": grounded,
            "transport": "sparql",
            "sparql": sparql,
            "prefixes": formal.stats.prefixes,
            "graphs": {
                "files_ok": formal.stats.files_ok,
                "files_failed": formal.stats.files_failed.len(),
                "triples": formal.stats.triples,
                "citations": formal.stats.citations,
                "alignments": formal.stats.alignments,
                "cache": formal.stats.cache,
                "source_mtime": formal.stats.source_mtime,
                "prefixes": formal.stats.prefixes,
            },
            "functors": functor_values(lattice),
            "knowledge_base": { "mode": "formal_sparql" },
        })),
        timing_ms: started.elapsed().as_millis(),
    }
}

fn functor_values(lattice: &LatticeArtifact) -> Vec<FunctorWitness> {
    [&lattice.functor, &lattice.heading_functor]
        .into_iter()
        .filter(|row| !row.name.is_empty())
        .cloned()
        .collect()
}

fn fact_rank(pred: &str) -> u8 {
    let name = local_name(pred).to_ascii_lowercase();
    if name.contains("ocioso") || name.starts_with("wellhub") {
        return 9;
    }
    if name.contains("segundasexta") || name.contains("hoursweekday") {
        0
    } else if name.contains("horariosabado") || name.contains("horariodomingo") {
        1
    } else if name.starts_with("horario") || name.contains("hours") {
        2
    } else if name.contains("preco") || name.contains("price") || name.contains("valor") {
        3
    } else if name.contains("endereco") {
        4
    } else {
        9
    }
}

fn local_name(iri: &str) -> &str {
    iri.rsplit(['#', '/', ':'])
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or(iri)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
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
        crate::knowledge::compile_knowledge(repo.path()).unwrap();
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
    fn sparql_empty_select_is_zero_solutions() {
        let repo = fixture_repo();
        crate::knowledge::compile_knowledge(repo.path()).unwrap();
        knowledge_graph::compile_formal_graph(repo.path()).unwrap();
        let envelope = sparql_knowledge(
            repo.path(),
            "SELECT ?s WHERE { ?s <http://example.org/kb#missing> ?x }",
            8,
        )
        .unwrap();
        assert!(
            envelope.summary.contains("0 solution"),
            "{}",
            envelope.summary
        );
        assert!(envelope.entities.is_empty());
        assert_eq!(envelope.meta.as_ref().unwrap()["grounded"], true);
        assert!(!envelope.warnings.iter().any(|row| row == "ungrounded"));
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

    #[test]
    fn sparql_injects_default_colon_prefix() {
        let repo = fixture_repo();
        crate::knowledge::compile_knowledge(repo.path()).unwrap();
        knowledge_graph::compile_formal_graph(repo.path()).unwrap();
        let envelope = sparql_knowledge(
            repo.path(),
            "SELECT ?s ?hours WHERE { ?s :hoursWeekday ?hours }",
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

    #[test]
    fn explain_refuses_ambiguous_shared_city() {
        let repo = fixture_repo();
        fs::write(
            repo.path().join("ont/unit.ttl"),
            r#"@prefix : <http://example.org/kb#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .

:Alpha a :Unit ;
    rdfs:label "Alpha" ;
    :hoursWeekday "05:30-23:00" ;
    :city "Beta City" .

:Gamma a :Unit ;
    rdfs:label "Gamma" ;
    :hoursWeekday "06:00-22:00" ;
    :city "Beta City" .
"#,
        )
        .unwrap();
        crate::knowledge::compile_knowledge(repo.path()).unwrap();
        knowledge_graph::compile_formal_graph(repo.path()).unwrap();
        let city = explain_knowledge(repo.path(), "city", 4).unwrap();
        assert!(
            city.summary.contains("ambiguous") || city.summary.starts_with("refused"),
            "{}",
            city.summary
        );
        assert_eq!(city.meta.as_ref().unwrap()["grounded"], false);
        let alpha = explain_knowledge(repo.path(), "hours Alpha", 4).unwrap();
        assert_eq!(alpha.meta.as_ref().unwrap()["grounded"], true);
        assert_eq!(
            alpha.entities[0]["iri"].as_str().unwrap(),
            "http://example.org/kb#Alpha"
        );
    }

    #[test]
    fn scoped_explain_uses_heading_cites() {
        let repo = fixture_repo();
        fs::write(
            repo.path().join("ont/unit.ttl"),
            r#"@prefix : <http://example.org/kb#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .

:Alpha a :Unit ;
    rdfs:label "Alpha" ;
    :hoursWeekday "05:30-23:00" ;
    :city "Beta City" .

:Gamma a :Unit ;
    rdfs:label "Gamma" ;
    :hoursWeekday "06:00-22:00" ;
    :city "Beta City" .
"#,
        )
        .unwrap();
        crate::knowledge::compile_knowledge(repo.path()).unwrap();
        let scoped = explain_knowledge_scoped(
            repo.path(),
            "hours",
            4,
            Some("section:docs/alpha.md#1"),
            None,
        )
        .unwrap();
        assert_eq!(scoped.meta.as_ref().unwrap()["grounded"], true);
        assert_eq!(scoped.meta.as_ref().unwrap()["scoped"], true);
        assert_eq!(
            scoped.entities[0]["iri"].as_str().unwrap(),
            "http://example.org/kb#Alpha"
        );
    }

    #[test]
    fn identity_query_grounds_label_only() {
        let repo = fixture_repo();
        crate::knowledge::compile_knowledge(repo.path()).unwrap();
        let envelope = explain_knowledge(repo.path(), "Alpha", 4).unwrap();
        assert_eq!(envelope.meta.as_ref().unwrap()["grounded"], true);
        assert_eq!(envelope.meta.as_ref().unwrap()["identity"], true);
        assert_eq!(
            envelope.entities[0]["iri"].as_str().unwrap(),
            "http://example.org/kb#Alpha"
        );
        assert!(
            envelope.entities[0]["sources"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty()),
            "{}",
            envelope.entities[0]
        );
    }

    #[test]
    fn compile_knowledge_writes_formal_nq() {
        let repo = fixture_repo();
        crate::knowledge::compile_knowledge(repo.path()).unwrap();
        assert!(knowledge_graph::formal_nq_path(repo.path()).is_file());
    }

    #[test]
    fn status_and_explain_surface_formal_prefixes() {
        let repo = fixture_repo();
        crate::knowledge::compile_knowledge(repo.path()).unwrap();
        let status = crate::knowledge::status_local(repo.path()).unwrap();
        let formal = &status.meta.as_ref().unwrap()["formal"];
        assert!(formal["present"].as_bool().unwrap());
        assert!(formal["fresh"].as_bool().unwrap());
        assert!(formal["triples"].as_u64().unwrap() > 0);
        assert_eq!(
            formal["prefixes"][""].as_str().unwrap(),
            "http://example.org/kb#"
        );
        assert!(status.entities[0]["formal_triples"].as_u64().unwrap() > 0);
        let envelope = explain_knowledge(repo.path(), "hours Alpha", 4).unwrap();
        let prefixes = &envelope.meta.as_ref().unwrap()["prefixes"];
        assert_eq!(prefixes[""].as_str().unwrap(), "http://example.org/kb#");
        assert_eq!(
            envelope.meta.as_ref().unwrap()["graphs"]["prefixes"][""]
                .as_str()
                .unwrap(),
            "http://example.org/kb#"
        );
        let sparql = sparql_knowledge(
            repo.path(),
            "SELECT ?s ?hours WHERE { ?s :hoursWeekday ?hours }",
            8,
        )
        .unwrap();
        assert_eq!(
            sparql.meta.as_ref().unwrap()["prefixes"][""]
                .as_str()
                .unwrap(),
            "http://example.org/kb#"
        );
    }
}
