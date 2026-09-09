//! Formal knowledge graph for SPARQL-grounded answers.
//!
//! Loads repo RDF, the induced lattice OWL, and wiki citation triples into
//! an in-process Oxigraph store. RDF-star quoted triples are rewritten to
//! `rdf:Statement` reification so stock Turtle still parses. Wiki headings
//! whose labels match `rdfs:label` become `leio:cites` links into the lattice.
// Rust guideline compliant 2026-02-21

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufWriter, Cursor, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::model::{
    BlankNode, GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term, Triple,
};
use oxigraph::sparql::QueryResults;
use oxigraph::store::Store;
use regex::Regex;

use crate::jsonld::PROV_NS;
use crate::lattice::{self, LatticeArtifact};

// The store, its statistics and the query-text helpers live in
// `leio-knowledge-core` so container runtimes share them; re-exported here
// so every existing `knowledge_graph::` path keeps resolving.
use leio_knowledge_core::store::RDFS_NS;
pub use leio_knowledge_core::store::{
    FormalStats, FormalStore, KNOWLEDGE_NS, RDF_REIFIES, SparqlQueryExt, fold_text,
    looks_like_sparql, sparql_escape, term_text, with_standard_prefixes,
};

/// Skip oversized dumps; ontologies in this crate stay well under this.
const MAX_RDF_BYTES: u64 = 8 * 1024 * 1024;

/// Default formal N-Quads path next to the compiled wiki.
pub fn formal_nq_path(repo_root: &Path) -> PathBuf {
    crate::knowledge::default_knowledge_dir(repo_root).join("formal.nq")
}

/// Sidecar stats written next to `formal.nq`.
pub fn formal_stats_path(repo_root: &Path) -> PathBuf {
    crate::knowledge::default_knowledge_dir(repo_root).join("formal.json")
}

/// Read persisted `formal.json` without opening Oxigraph.
pub fn read_formal_stats(repo_root: &Path) -> Option<FormalStats> {
    let raw = fs::read(formal_stats_path(repo_root)).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// Whether `formal.nq` is at least as new as the RDF and wiki sources.
pub fn formal_cache_fresh(repo_root: &Path, stats: &FormalStats) -> bool {
    let cached = formal_nq_path(repo_root);
    if !cached.is_file() {
        return false;
    }
    let sources = source_mtime(repo_root);
    file_mtime(&cached) >= sources && (stats.source_mtime == 0 || stats.source_mtime >= sources)
}

/// Disk health of the formal graph for `knowledge status`.
pub fn formal_graph_status(repo_root: &Path) -> serde_json::Value {
    let nq_present = formal_nq_path(repo_root).is_file();
    match read_formal_stats(repo_root) {
        Some(stats) => {
            let fresh = formal_cache_fresh(repo_root, &stats) && nq_present;
            serde_json::json!({
                "present": nq_present,
                "fresh": fresh,
                "nq": "formal.nq",
                "files_ok": stats.files_ok,
                "files_failed": stats.files_failed.len(),
                "triples": stats.triples,
                "citations": stats.citations,
                "alignments": stats.alignments,
                "prefixes": stats.prefixes,
                "source_mtime": stats.source_mtime,
                "cache": stats.cache,
            })
        }
        None => serde_json::json!({
            "present": nq_present,
            "fresh": false,
            "nq": "formal.nq",
        }),
    }
}

/// Build the formal store from repo RDF, induced OWL, and wiki/lattice links.
///
/// # Errors
///
/// Returns an error only when the Oxigraph store cannot be created or written.
pub fn compile_formal_graph(repo_root: &Path) -> Result<FormalStats> {
    let _lock = crate::sidecar::acquire_lock(repo_root, "formal")?;
    let loaded = build_formal_store(repo_root)?;
    persist_formal_store(repo_root, &loaded)?;
    Ok(loaded.stats)
}

/// Load a live formal store from repo RDF, induced OWL, and wiki links.
///
/// Reuses `formal.nq` when present so `explain` does not rebuild the store.
///
/// # Errors
///
/// Returns an error when the Oxigraph store cannot be created.
pub fn load_formal_store(repo_root: &Path) -> Result<FormalStore> {
    if let Some(hit) = try_load_cached_formal(repo_root) {
        return Ok(hit);
    }
    let _lock = crate::sidecar::acquire_lock(repo_root, "formal")?;
    if let Some(hit) = try_load_cached_formal(repo_root) {
        return Ok(hit);
    }
    let cached = formal_nq_path(repo_root);
    let mut loaded = build_formal_store(repo_root)?;
    loaded.stats.cache = if cached.is_file() {
        "rebuilt".to_string()
    } else {
        "fresh".to_string()
    };
    let _ = persist_formal_store(repo_root, &loaded);
    Ok(loaded)
}

fn try_load_cached_formal(repo_root: &Path) -> Option<FormalStore> {
    let cached = formal_nq_path(repo_root);
    if !cached.is_file() {
        return None;
    }
    let sources = source_mtime(repo_root);
    let recorded = fs::read(formal_stats_path(repo_root))
        .ok()
        .and_then(|raw| serde_json::from_slice::<FormalStats>(&raw).ok());
    let cache_fresh = file_mtime(&cached) >= sources
        && recorded
            .as_ref()
            .is_none_or(|stats| stats.source_mtime == 0 || stats.source_mtime >= sources);
    if !cache_fresh {
        return None;
    }
    let store = load_cached_nq(&cached).ok()?;
    let mut stats = recorded.unwrap_or_default();
    stats.cache = "hit".to_string();
    Some(FormalStore::from_parts(store, stats))
}

fn persist_formal_store(repo_root: &Path, loaded: &FormalStore) -> Result<()> {
    let dir = crate::knowledge::default_knowledge_dir(repo_root);
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let nq = formal_nq_path(repo_root);
    let tmp = crate::sidecar::temp_path(&nq);
    {
        let mut file = BufWriter::new(
            File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?,
        );
        loaded
            .store()
            .dump_to_writer(RdfFormat::NQuads, &mut file)
            .context("dump formal.nq")?;
        file.flush().context("flush formal.nq")?;
    }
    crate::sidecar::rename_replace(&tmp, &nq)?;
    crate::sidecar::write_atomic_json(&formal_stats_path(repo_root), &loaded.stats)
        .with_context(|| format!("write {}", formal_stats_path(repo_root).display()))?;
    Ok(())
}

fn build_formal_store(repo_root: &Path) -> Result<FormalStore> {
    let store = Store::new().context("create Oxigraph knowledge store")?;
    let mut stats = FormalStats::default();
    let graph_wiki = GraphName::DefaultGraph;
    let graph_lattice = GraphName::DefaultGraph;

    for path in discover_rdf_files(repo_root) {
        match load_rdf_file(&store, repo_root, &path) {
            Ok((count, prefixes)) => {
                stats.files_ok += 1;
                stats.triples = stats.triples.saturating_add(count);
                merge_prefixes(&mut stats.prefixes, prefixes);
            }
            Err(err) => stats
                .files_failed
                .push((rel_path(repo_root, &path), err.to_string())),
        }
    }

    let owl = lattice::owl_path(repo_root);
    if owl.is_file() {
        match load_rdf_file(&store, repo_root, &owl) {
            Ok((count, prefixes)) => {
                stats.files_ok += 1;
                stats.triples = stats.triples.saturating_add(count);
                merge_prefixes(&mut stats.prefixes, prefixes);
            }
            Err(err) => stats
                .files_failed
                .push((rel_path(repo_root, &owl), err.to_string())),
        }
    }

    let lattice = lattice::load_lattice(&lattice::lattice_path(repo_root))?.unwrap_or_default();
    stats.citations = insert_wiki_citations(&store, repo_root, &graph_wiki)?;
    let (aligned, alignment_provenance) =
        insert_heading_alignment(&store, repo_root, &lattice, &graph_lattice)?;
    stats.alignments = aligned;
    stats.provenance = alignment_provenance;
    stats.provenance += insert_functor_witnesses(&store, repo_root, &lattice, &graph_lattice)?;
    stats.alignments =
        stats
            .alignments
            .saturating_add(align_labels_to_headings(&store, &lattice, &graph_wiki)?);
    stats.source_mtime = source_mtime(repo_root);

    Ok(FormalStore::from_parts(store, stats))
}

fn discover_rdf_files(repo_root: &Path) -> Vec<PathBuf> {
    let mut builder = WalkBuilder::new(repo_root);
    builder.hidden(true);
    builder.git_ignore(true);
    builder.git_global(true);
    builder.git_exclude(true);
    builder.filter_entry(|entry| {
        let Some(name) = entry.file_name().to_str() else {
            return true;
        };
        if entry.path().is_dir() {
            return !matches!(
                name,
                "target"
                    | "node_modules"
                    | ".git"
                    | ".leio-code"
                    | "dist"
                    | "vendor"
                    | ".venv"
                    | "__pycache__"
                    | "fixtures"
            );
        }
        true
    });
    let mut files = Vec::new();
    for dent in builder.build().flatten() {
        let path = dent.path();
        if !dent.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        if !is_rdf_file(path) {
            continue;
        }
        if path.file_name().and_then(|name| name.to_str()) == Some("formal.nq") {
            continue;
        }
        if dent
            .metadata()
            .ok()
            .is_some_and(|meta| meta.len() > MAX_RDF_BYTES)
        {
            continue;
        }
        files.push(path.to_path_buf());
    }
    files.sort();
    files
}

fn is_rdf_file(path: &Path) -> bool {
    crate::ontology::is_rdf_path(path)
}

fn rdf_format(path: &Path, source: &str) -> RdfFormat {
    crate::ontology::rdf_format_for(path, source)
}

fn load_cached_nq(path: &Path) -> Result<Store> {
    let store = Store::new().context("create Oxigraph knowledge store")?;
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    store
        .load_from_reader(RdfFormat::NQuads, file)
        .with_context(|| format!("parse {}", path.display()))?;
    Ok(store)
}

fn load_rdf_file(
    store: &Store,
    repo_root: &Path,
    path: &Path,
) -> Result<(usize, BTreeMap<String, String>)> {
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let prefixes = extract_prefixes(&raw);
    let format = rdf_format(path, &raw);
    // Turtle-family only: RDF-star rewrite and missing-prefix injection would
    // corrupt JSON-LD and RDF/XML.
    let prepared = if crate::ontology::format_needs_turtle_prepare(format) {
        prepare_turtle(&raw)
    } else {
        raw
    };
    let rel = rel_path(repo_root, path);
    // Default graph so `SELECT { ?s ?p ?o }` sees ontology facts without GRAPH.
    let parser = RdfParser::from_format(format);
    let before = store.len().unwrap_or(0);
    store
        .load_from_reader(parser, Cursor::new(prepared.into_bytes()))
        .with_context(|| format!("parse RDF {rel}"))?;
    let after = store.len().unwrap_or(before);
    Ok((after.saturating_sub(before), prefixes))
}

fn extract_prefixes(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for cap in PREFIX_DECL.captures_iter(text) {
        let name = cap.get(1).map(|mat| mat.as_str()).unwrap_or("");
        let iri = cap.get(2).map(|mat| mat.as_str()).unwrap_or("");
        if iri.is_empty() {
            continue;
        }
        out.entry(name.to_string())
            .or_insert_with(|| iri.to_string());
    }
    out
}

fn merge_prefixes(into: &mut BTreeMap<String, String>, extra: BTreeMap<String, String>) {
    for (name, iri) in extra {
        into.entry(name).or_insert(iri);
    }
}

fn source_mtime(repo_root: &Path) -> u64 {
    let mut newest = 0u64;
    for path in discover_rdf_files(repo_root) {
        newest = newest.max(file_mtime(&path));
    }
    newest = newest.max(file_mtime(&lattice::owl_path(repo_root)));
    newest = newest.max(file_mtime(&lattice::lattice_path(repo_root)));
    newest = newest.max(file_mtime(
        &crate::knowledge::default_knowledge_dir(repo_root).join("articles.search"),
    ));
    newest
}

fn file_mtime(path: &Path) -> u64 {
    path.metadata()
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn prepare_turtle(text: &str) -> String {
    let mut out = String::new();
    if text.contains("<<") && !text.contains("@prefix rdf:") && !text.contains("PREFIX rdf:") {
        out.push_str("@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n");
    }
    if (text.contains("owl:") || text.contains("owl."))
        && !text.contains("@prefix owl:")
        && !text.contains("PREFIX owl:")
    {
        out.push_str("@prefix owl: <http://www.w3.org/2002/07/owl#> .\n");
    }
    let rewritten = RDF_STAR.replace_all(text, |caps: &regex::Captures<'_>| {
        format!(
            "[] a rdf:Statement ; rdf:subject {} ; rdf:predicate {} ; rdf:object {} ;",
            &caps[1], &caps[2], &caps[3]
        )
    });
    out.push_str(&rewritten);
    out
}

static PREFIX_DECL: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"(?i)(?:@prefix|PREFIX)\s+([A-Za-z_][\w-]*)?:\s*<([^>]+)>")
        .expect("prefix decl regex")
});

static RDF_STAR: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(
        r"<<\s*((?:<[^>]+>|(?:[A-Za-z_][\w-]*)?:[\w-]+))\s+((?:<[^>]+>|(?:[A-Za-z_][\w-]*)?:[\w-]+))\s+((?:<[^>]+>|(?:[A-Za-z_][\w-]*)?:[\w-]+))\s*>>",
    )
    .expect("rdf-star rewrite regex")
});

fn insert_wiki_citations(store: &Store, repo_root: &Path, graph: &GraphName) -> Result<usize> {
    let mut builder = WalkBuilder::new(repo_root);
    builder.hidden(true);
    builder.git_ignore(true);
    builder.filter_entry(|entry| {
        let Some(name) = entry.file_name().to_str() else {
            return true;
        };
        if entry.path().is_dir() {
            return !matches!(
                name,
                "target" | "node_modules" | ".git" | ".leio-code" | "dist" | "vendor" | ".venv"
            );
        }
        true
    });
    let cites = named(format!("{KNOWLEDGE_NS}cites"))?;
    let stated = named(format!("{KNOWLEDGE_NS}statedIn"))?;
    let heading_pred = named(format!("{KNOWLEDGE_NS}heading"))?;
    let mut count = 0usize;
    for dent in builder.build().flatten() {
        let path = dent.path();
        if !dent.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(ext.as_str(), "md" | "mdx" | "txt") {
            continue;
        }
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        let rel = rel_path(repo_root, path);
        let heading = first_heading(&text);
        for (line, iri) in extract_iris(&text) {
            let Ok(section) = section_node(&rel, line) else {
                continue;
            };
            let Ok(object) = named(&iri) else {
                continue;
            };
            store.insert(&Quad::new(
                section.clone(),
                cites.clone(),
                object.clone(),
                graph.clone(),
            ))?;
            store.insert(&Quad::new(
                object,
                stated.clone(),
                section.clone(),
                graph.clone(),
            ))?;
            if let Some(title) = heading.as_deref() {
                store.insert(&Quad::new(
                    section,
                    heading_pred.clone(),
                    Literal::new_simple_literal(title),
                    graph.clone(),
                ))?;
            }
            count += 1;
        }
    }
    Ok(count)
}

/// RDF 1.2 statement-level provenance: reify `triple` and derive the reifier
/// from `evidence_iri`.
///
/// Emits two triples in the default graph — `<reifier> rdf:reifies
/// <<( s p o )>>` and `<reifier> prov:wasDerivedFrom <evidence>` — so SPARQL
/// consumers can query statement lineage directly:
/// `?r rdf:reifies <<( ?s ?p ?o )>> ; prov:wasDerivedFrom ?src`.
///
/// # Errors
///
/// Returns an error when Oxigraph rejects a vocabulary IRI or the insert.
fn insert_statement_provenance(
    store: &Store,
    subject: NamedOrBlankNode,
    predicate: &NamedNode,
    object: Term,
    evidence_iri: &str,
) -> Result<()> {
    let triple_term = Term::Triple(Box::new(Triple::new(subject, predicate.clone(), object)));
    let reifier = BlankNode::default();
    store.insert(&Quad::new(
        reifier.clone(),
        NamedNode::new(RDF_REIFIES).context("parse rdf:reifies IRI")?,
        triple_term,
        GraphName::DefaultGraph,
    ))?;
    store.insert(&Quad::new(
        reifier,
        NamedNode::new(format!("{PROV_NS}wasDerivedFrom"))
            .context("parse prov:wasDerivedFrom IRI")?,
        NamedNode::new(evidence_iri)
            .with_context(|| format!("parse evidence IRI {evidence_iri}"))?,
        GraphName::DefaultGraph,
    ))?;
    Ok(())
}

fn insert_heading_alignment(
    store: &Store,
    repo_root: &Path,
    lattice: &LatticeArtifact,
    graph: &GraphName,
) -> Result<(usize, usize)> {
    let heading_pred = named(format!("{KNOWLEDGE_NS}heading"))?;
    let concept_pred = named(format!("{KNOWLEDGE_NS}concept"))?;
    let section_pred = named(format!("{KNOWLEDGE_NS}section"))?;
    let label_pred = named(format!("{RDFS_NS}label"))?;
    let mut count = 0usize;
    let mut provenance = 0usize;
    for object in &lattice.heading_objects {
        let Ok(section) = section_node(&object.path, object.line) else {
            continue;
        };
        store.insert(&Quad::new(
            section.clone(),
            heading_pred.clone(),
            Literal::new_simple_literal(&object.heading_path),
            graph.clone(),
        ))?;
        store.insert(&Quad::new(
            section.clone(),
            section_pred.clone(),
            Literal::new_simple_literal(&object.id),
            graph.clone(),
        ))?;
        store.insert(&Quad::new(
            section.clone(),
            label_pred.clone(),
            Literal::new_simple_literal(&object.title),
            graph.clone(),
        ))?;
        insert_statement_provenance(
            store,
            section.clone().into(),
            &heading_pred,
            Literal::new_simple_literal(&object.heading_path).into(),
            &format!(
                "file://{}/{}#line={}",
                repo_root.display(),
                object.path,
                object.line
            ),
        )?;
        provenance += 1;
        if let Some(membership) = lattice.memberships.get(&object.id)
            && let Ok(concept) = concept_node(&membership.concept_id)
        {
            store.insert(&Quad::new(
                section,
                concept_pred.clone(),
                concept,
                graph.clone(),
            ))?;
        }
        count += 1;
    }
    Ok((count, provenance))
}

fn insert_functor_witnesses(
    store: &Store,
    repo_root: &Path,
    lattice: &LatticeArtifact,
    graph: &GraphName,
) -> Result<usize> {
    let coherence = named(format!("{KNOWLEDGE_NS}coherence"))?;
    let preserved = named(format!("{KNOWLEDGE_NS}preserved"))?;
    let total = named(format!("{KNOWLEDGE_NS}total"))?;
    let name_pred = named(format!("{KNOWLEDGE_NS}functorName"))?;
    let lattice_evidence = format!("file://{}", lattice::lattice_path(repo_root).display());
    let mut provenance = 0usize;
    for witness in [&lattice.functor, &lattice.heading_functor] {
        if witness.name.is_empty() {
            continue;
        }
        let iri = named(format!("urn:leio:functor/{}", witness.name))?;
        store.insert(&Quad::new(
            iri.clone(),
            name_pred.clone(),
            Literal::new_simple_literal(&witness.name),
            graph.clone(),
        ))?;
        store.insert(&Quad::new(
            iri.clone(),
            coherence.clone(),
            Literal::new_simple_literal(format!("{:.3}", witness.coherence)),
            graph.clone(),
        ))?;
        store.insert(&Quad::new(
            iri.clone(),
            preserved.clone(),
            Literal::new_simple_literal(witness.preserved.to_string()),
            graph.clone(),
        ))?;
        store.insert(&Quad::new(
            iri,
            total.clone(),
            Literal::new_simple_literal(witness.total.to_string()),
            graph.clone(),
        ))?;
        insert_statement_provenance(
            store,
            named(format!("urn:leio:functor/{}", witness.name)).map(NamedOrBlankNode::from)?,
            &coherence,
            Literal::new_simple_literal(format!("{:.3}", witness.coherence)).into(),
            &lattice_evidence,
        )?;
        provenance += 1;
    }
    Ok(provenance)
}

fn align_labels_to_headings(
    store: &Store,
    lattice: &LatticeArtifact,
    graph: &GraphName,
) -> Result<usize> {
    let labels = collect_labels(store)?;
    if labels.is_empty() || lattice.heading_objects.is_empty() {
        return Ok(0);
    }
    let cites = named(format!("{KNOWLEDGE_NS}cites"))?;
    let stated = named(format!("{KNOWLEDGE_NS}statedIn"))?;
    let mut by_fold: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (iri, label) in labels {
        by_fold.entry(fold_text(&label)).or_default().push(iri);
    }
    let mut count = 0usize;
    for heading in &lattice.heading_objects {
        // Only the page title (no `>` stack) aligns to a unique rdfs:label.
        if heading.heading_path.contains(" > ") {
            continue;
        }
        let keys = [fold_text(&heading.title)];
        for key in keys {
            if key.is_empty() || GENERIC_HEADINGS.contains(&key.as_str()) {
                continue;
            }
            let Some(iris) = by_fold.get(&key) else {
                continue;
            };
            if iris.len() != 1 {
                continue;
            }
            let Ok(section) = section_node(&heading.path, heading.line) else {
                continue;
            };
            for iri in iris {
                let Ok(object) = named(iri) else {
                    continue;
                };
                store.insert(&Quad::new(
                    section.clone(),
                    cites.clone(),
                    object.clone(),
                    graph.clone(),
                ))?;
                store.insert(&Quad::new(
                    object,
                    stated.clone(),
                    section.clone(),
                    graph.clone(),
                ))?;
                count += 1;
            }
        }
    }
    Ok(count)
}

fn collect_labels(store: &Store) -> Result<Vec<(String, String)>> {
    let sparql = format!(
        r#"
PREFIX rdfs: <{RDFS_NS}>
SELECT DISTINCT ?s ?label WHERE {{
  ?s rdfs:label ?label .
}}
"#
    );
    let mut out = Vec::new();
    if let QueryResults::Solutions(solutions) = store
        .sparql_query(&sparql)
        .context("label inventory SPARQL")?
    {
        for solution in solutions {
            let solution = solution.context("label solution")?;
            let iri = term_text(solution.get("s"));
            let label = term_text(solution.get("label"));
            if is_ontology_iri(&iri) {
                out.push((iri, label));
            }
        }
    }
    Ok(out)
}

fn extract_iris(text: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line_no = u32::try_from(idx + 1).unwrap_or(1);
        if let Some(rest) = line.split_once("IRI:") {
            let candidate = rest
                .1
                .trim()
                .trim_matches(|ch| ch == '<' || ch == '>' || ch == '`' || ch == '"')
                .trim_end_matches(['.', ',', ';', ')']);
            if looks_like_iri(candidate) {
                out.push((line_no, candidate.to_string()));
            }
        }
        for cap in IRI_FINDER.captures_iter(line) {
            let Some(iri) = cap.get(1).or_else(|| cap.get(2)).map(|mat| mat.as_str()) else {
                continue;
            };
            let iri = iri.trim_end_matches(['.', ',', ';', ')']);
            if looks_like_iri(iri) {
                out.push((line_no, iri.to_string()));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

static IRI_FINDER: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"<(https?://[^>\s]+|urn:[^>\s]+)>|(https?://[^\s)>\]]+|urn:[^\s)>\]]+)")
        .expect("iri finder")
});

const GENERIC_HEADINGS: &[&str] = &[
    "planos",
    "servicos",
    "serviços",
    "estrutura",
    "politicas",
    "políticas",
    "fatos na ontologia",
    "faq",
];

fn is_ontology_iri(iri: &str) -> bool {
    (iri.starts_with("http://") || iri.starts_with("https://") || iri.starts_with("urn:"))
        && !iri.starts_with("https://example.local/leio/")
        && !iri.starts_with("urn:leio:")
}

fn looks_like_iri(value: &str) -> bool {
    (value.starts_with("http://") || value.starts_with("https://") || value.starts_with("urn:"))
        && !value.contains(' ')
        && value.len() > 8
}

fn first_heading(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let trimmed = line.trim();
        trimmed
            .strip_prefix('#')
            .map(|rest| rest.trim_start_matches('#').trim().to_string())
            .filter(|title| !title.is_empty())
    })
}

fn section_node(path: &str, line: u32) -> Result<NamedNode> {
    let mut iri = String::from("https://example.local/leio/wiki/");
    for ch in path.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '-' | '_') {
            iri.push(ch);
        } else {
            iri.push_str(&format!("_{:x}_", u32::from(ch)));
        }
    }
    iri.push_str(&format!("#L{line}"));
    named(&iri)
}

fn concept_node(concept_id: &str) -> Result<NamedNode> {
    let local: String = concept_id
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    named(format!("urn:leio:concept/{local}"))
}

fn named(iri: impl AsRef<str>) -> Result<NamedNode> {
    NamedNode::new(iri.as_ref().to_string()).with_context(|| format!("IRI {}", iri.as_ref()))
}

fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
fn store_from_nq(nq: &str) -> Store {
    let store = Store::new().expect("store");
    store
        .load_from_reader(
            RdfParser::from_format(RdfFormat::NQuads),
            Cursor::new(nq.as_bytes().to_vec()),
        )
        .expect("parse N-Quads");
    store
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rdf_star_rewrites_to_reification() {
        let src = r#"<< <http://ex/s> :p <http://ex/o> >> :day "Mon" ."#;
        let out = prepare_turtle(src);
        assert!(out.contains("rdf:Statement"));
        assert!(out.contains("rdf:subject <http://ex/s>"));
        assert!(!out.contains("<<"));
    }

    #[test]
    fn extract_explicit_iri_lines() {
        let text = "# Alpha\n\nIRI: http://example.org/kb#Alpha\n";
        let found = extract_iris(text);
        assert_eq!(found[0].1, "http://example.org/kb#Alpha");
    }

    #[test]
    fn injects_default_colon_prefix() {
        let mut prefixes = BTreeMap::new();
        prefixes.insert("".into(), "http://example.org/kb#".into());
        let out = with_standard_prefixes("SELECT ?s WHERE { ?s :hoursWeekday ?h }", &prefixes);
        assert!(out.contains("PREFIX : <http://example.org/kb#>"));
        assert!(out.contains("PREFIX leio:"));
        assert!(out.contains("SELECT ?s WHERE"));
    }

    #[test]
    fn jsonld_and_rdfxml_owl_load_into_formal_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("ont")).expect("mkdir ont");
        fs::write(
            dir.path().join("ont/invoice.jsonld"),
            r#"[{
  "@id": "http://example.org/leio-ont#Invoice",
  "@type": "http://www.w3.org/2002/07/owl#Class"
}]
"#,
        )
        .expect("write jsonld");
        fs::write(
            dir.path().join("ont/policy.owl"),
            r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:owl="http://www.w3.org/2002/07/owl#">
  <owl:Class rdf:about="http://example.org/leio-ont#Policy"/>
</rdf:RDF>
"#,
        )
        .expect("write owl");
        let stats = compile_formal_graph(dir.path()).expect("compile");
        assert!(
            stats.files_failed.is_empty(),
            "parse failures: {:?}",
            stats.files_failed
        );
        assert!(stats.files_ok >= 2, "files_ok={}", stats.files_ok);
        let loaded = load_formal_store(dir.path()).expect("load");
        let results = loaded
            .query("SELECT ?s WHERE { ?s a owl:Class }")
            .expect("sparql");
        let mut iris = Vec::new();
        if let QueryResults::Solutions(solutions) = results {
            for solution in solutions {
                let solution = solution.expect("row");
                iris.push(term_text(solution.get("s")));
            }
        }
        assert!(iris.iter().any(|iri| iri.ends_with("Invoice")), "{iris:?}");
        assert!(iris.iter().any(|iri| iri.ends_with("Policy")), "{iris:?}");
    }

    #[test]
    fn rdf12_directional_literal_round_trips() {
        let nq = concat!(
            "<https://ex.test/doc#sec> <https://example.local/leio/knowledge#title> ",
            "\"\u{5e7}\u{5d4}\u{5d9}\u{5d9}\u{5dd}\"@he--rtl .\n"
        );
        let store = store_from_nq(nq);
        let results = store
            .sparql_query(
                "PREFIX k: <https://example.local/leio/knowledge#>
                 SELECT ?t WHERE { ?s k:title ?t }",
            )
            .expect("query");
        if let QueryResults::Solutions(solutions) = results {
            let mut titles = Vec::new();
            for solution in solutions {
                let row = solution.expect("row");
                titles.push(term_text(row.get("t")));
            }
            assert_eq!(titles.len(), 1, "{titles:?}");
        } else {
            panic!("expected solutions");
        }
        // The dump must preserve the RDF 1.2 directional form (@he--rtl).
        let mut dumped = Vec::new();
        store
            .dump_to_writer(RdfFormat::NQuads, &mut dumped)
            .expect("dump");
        let dumped = String::from_utf8(dumped).expect("utf8");
        assert!(dumped.contains("@he--rtl"), "dump lost direction: {dumped}");
    }

    #[test]
    fn rdf12_triple_term_provenance_round_trips() {
        let store = Store::new().expect("store");
        insert_statement_provenance(
            &store,
            NamedOrBlankNode::NamedNode(NamedNode::new("https://ex.test/doc#sec").expect("iri")),
            &NamedNode::new("https://example.local/leio/knowledge#heading").expect("predicate"),
            Literal::new_simple_literal("Architecture").into(),
            "file:///repo/README.md#line=3",
        )
        .expect("provenance");

        // SPARQL 1.2: triple-term pattern in the reifies position.
        let results = store
            .sparql_query(
                "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
                 PREFIX prov: <http://www.w3.org/ns/prov#>
                 SELECT ?s ?p ?o ?src WHERE {
                   ?r rdf:reifies <<( ?s ?p ?o )>> ;
                      prov:wasDerivedFrom ?src .
                 }",
            )
            .expect("reification query");
        let mut rows = 0;
        if let QueryResults::Solutions(solutions) = results {
            for solution in solutions {
                let row = solution.expect("row");
                assert_eq!(
                    term_text(row.get("p")),
                    "https://example.local/leio/knowledge#heading"
                );
                assert!(term_text(row.get("src")).starts_with("file:///repo/"));
                rows += 1;
            }
        }
        assert_eq!(rows, 1, "expected exactly one reified statement");

        // Dump round-trips as N-Quads 1.2 (triple terms are object-only here).
        let mut dumped = Vec::new();
        store
            .dump_to_writer(RdfFormat::NQuads, &mut dumped)
            .expect("dump");
        let dumped = String::from_utf8(dumped).expect("utf8");
        assert!(dumped.contains("22-rdf-syntax-ns#reifies"), "{dumped}");
        assert!(dumped.contains("<<("), "dump lost triple term: {dumped}");
        let reloaded = store_from_nq(&dumped);
        assert_eq!(
            reloaded.len().expect("reload len"),
            store.len().expect("store len"),
            "reload changed quad count"
        );
    }

    #[test]
    fn heading_alignment_emits_statement_provenance() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let artifact = LatticeArtifact {
            heading_objects: vec![crate::lattice::HeadingObject {
                id: "sec-intro".into(),
                path: "README.md".into(),
                title: "Intro".into(),
                heading_path: "Root > Intro".into(),
                line: 3,
            }],
            ..Default::default()
        };
        let store = Store::new().expect("store");
        let graph = GraphName::DefaultGraph;
        let (aligned, provenance) =
            insert_heading_alignment(&store, dir.path(), &artifact, &graph).expect("align");
        assert_eq!(aligned, 1);
        assert_eq!(provenance, 1);
        let results = store
            .sparql_query(
                "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
                 SELECT ?s WHERE { ?r rdf:reifies <<( ?s ?p ?o )>> }",
            )
            .expect("reification query");
        match results {
            QueryResults::Solutions(solutions) => assert_eq!(solutions.count(), 1),
            _other => panic!("expected solutions"),
        }
    }
}
