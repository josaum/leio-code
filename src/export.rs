//! Derived-artifact exports written under `.leio-code/exports/`.
//!
//! Three sister exports, each manifest-versioned for downstream consumers:
//! - [`export_formal_context`] — FCA objects/attributes/incidences (`v1`).
//!   Downstream: in-process `fca-fast-core` writes `lattice.json` + `induced.ttl`.
//! - [`export_arrow_nodes`] — Arrow IPC `RecordBatch` payload of LEIO node
//!   rows for the local node store (`v1`, deterministic SimHash vectors).
//! - [`export_hypergraph`] — JSON hypergraph derived from the same formal
//!   context, with `semantic_tooling` provenance.
//!
//! Code-graph export lives next door in [`crate::code_graph`] (separate
//! version cadence, separate query-cache contract). All exports write a
//! sidecar `manifest.json` so consumers can detect schema or revision drift
//! without re-parsing the body.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::fca::{self, ConceptMembership, MembershipOptions};
use anyhow::{Context, Result};
use arrow_array::{
    Array, BinaryArray, Float32Array, Int64Array, ListArray, RecordBatch, StringArray,
};
use arrow_ipc::writer::StreamWriter;
use serde::Serialize;
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::model::{
    AccessKind, DeclaredVar, DeployTargetRecord, EvidenceItem, QueryEnvelope, RepoIndex,
    SourceLanguage,
};
use crate::node_rows::{
    api_route_logical_id, build_leio_row_batch, docker_service_logical_id, visit_node_entities,
};
use crate::query::{collect_api_routes, collect_docker_services};

const FORMAL_CONTEXT_VERSION: u32 = 1;
const ARROW_NODES_VERSION: u32 = 1;
const ARROW_NODES_BATCH_SIZE: usize = 200;

#[derive(Debug, Clone, Serialize)]
struct FormalContextManifest {
    version: u32,
    repo_root: String,
    indexed_at: String,
    exported_at: String,
    objects_path: String,
    attributes_path: String,
    incidences_path: String,
    object_count: usize,
    attribute_count: usize,
    incidence_count: usize,
    object_kinds: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize)]
struct ArrowNodesManifest {
    version: u32,
    repo_root: String,
    indexed_at: String,
    exported_at: String,
    rows_path: String,
    row_count: usize,
    batch_count: usize,
    kind_counts: BTreeMap<String, usize>,
    /// FCA tag summary written next to `nodes.arrow` so readiness does not
    /// require re-parsing the row file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fca: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
struct FormalContextObject {
    id: String,
    kind: String,
    label: String,
    path: Option<String>,
    line: Option<usize>,
    language: Option<String>,
    metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
struct FormalContextAttribute {
    id: String,
    label: String,
    category: String,
}

#[derive(Debug, Clone, Serialize)]
struct FormalContextIncidence {
    object_id: String,
    attribute_id: String,
}

#[derive(Debug, Clone)]
struct AggregatedEnvVar {
    roots: BTreeSet<String>,
    languages: BTreeSet<String>,
    accesses: BTreeSet<String>,
}

#[derive(Debug, Clone)]
struct AggregatedRedisKey {
    roots: BTreeSet<String>,
    languages: BTreeSet<String>,
    accesses: BTreeSet<String>,
}

#[derive(Debug, Default)]
struct ContextBuilder {
    objects: BTreeMap<String, FormalContextObject>,
    attributes: BTreeMap<String, FormalContextAttribute>,
    incidences: BTreeSet<(String, String)>,
}

impl ContextBuilder {
    fn add_object(&mut self, object: FormalContextObject) {
        self.objects.entry(object.id.clone()).or_insert(object);
    }

    fn add_attribute(&mut self, category: &str, label: impl Into<String>) -> String {
        let label = label.into();
        let id = format!("attr:{label}");
        self.attributes
            .entry(id.clone())
            .or_insert_with(|| FormalContextAttribute {
                id: id.clone(),
                label,
                category: category.to_string(),
            });
        id
    }

    fn add_incidence(&mut self, object_id: &str, category: &str, label: impl Into<String>) {
        let attribute_id = self.add_attribute(category, label);
        self.incidences
            .insert((object_id.to_string(), attribute_id));
    }
}

pub fn default_formal_context_output_dir(root: &Path) -> PathBuf {
    root.join(".leio-code")
        .join("exports")
        .join("formal-context-v1")
}

pub fn export_formal_context(
    index: &RepoIndex,
    root: &Path,
    output_dir: &Path,
) -> Result<QueryEnvelope> {
    let started = Instant::now();
    let mut context = build_formal_context(index);
    add_wiki_sections(&mut context, root);

    fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    let objects_path = output_dir.join("objects.jsonl");
    let attributes_path = output_dir.join("attributes.jsonl");
    let incidences_path = output_dir.join("incidences.jsonl");
    let manifest_path = output_dir.join("manifest.json");

    write_jsonl(&objects_path, context.objects.values())?;
    write_jsonl(&attributes_path, context.attributes.values())?;

    let incidences: Vec<FormalContextIncidence> = context
        .incidences
        .iter()
        .map(|(object_id, attribute_id)| FormalContextIncidence {
            object_id: object_id.clone(),
            attribute_id: attribute_id.clone(),
        })
        .collect();
    write_jsonl(&incidences_path, incidences.iter())?;

    let mut object_kinds = BTreeMap::new();
    for object in context.objects.values() {
        *object_kinds.entry(object.kind.clone()).or_insert(0usize) += 1;
    }

    let manifest = FormalContextManifest {
        version: FORMAL_CONTEXT_VERSION,
        repo_root: index.root.clone(),
        indexed_at: index.indexed_at.clone(),
        exported_at: OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .context("failed to format export timestamp")?,
        objects_path: relative_to(output_dir, &objects_path),
        attributes_path: relative_to(output_dir, &attributes_path),
        incidences_path: relative_to(output_dir, &incidences_path),
        object_count: context.objects.len(),
        attribute_count: context.attributes.len(),
        incidence_count: context.incidences.len(),
        object_kinds,
    };
    let manifest_raw =
        serde_json::to_string_pretty(&manifest).context("failed to serialize manifest")?;
    fs::write(&manifest_path, manifest_raw)
        .with_context(|| format!("failed to write {}", manifest_path.display()))?;

    // Lattice artifacts are an optional enhancement beside the formal
    // context itself: without a resolvable fca wheel the export still lands
    // and nav keeps its index + Arrow fallbacks. Oversized contexts still
    // fail loudly inside `try_induce_from_pairs`.
    let mut lattice_warnings: Vec<String> = Vec::new();
    let lattice = if fca::wheel_available() {
        let mut artifact =
            crate::lattice::try_induce_from_pairs(&structural_incidence_pairs_for_fca(&context))?;
        crate::lattice::attach_source_provenance(&mut artifact, index, root);
        crate::lattice::attach_heading_functor(&mut artifact, root);
        Some(artifact)
    } else {
        lattice_warnings.push(
            "lattice induction skipped: no fca wheel resolved; set LEIO_FCA_WHEEL / LEIO_FCA_FIND_LINKS or commit artifacts/wheels/"
                .to_string(),
        );
        None
    };
    let lattice_paths = lattice
        .as_ref()
        .map(|artifact| crate::lattice::write_artifacts(output_dir, artifact))
        .transpose()?;
    let (lattice_path, owl_path) = match lattice_paths {
        Some(paths) => (paths.0.display().to_string(), paths.1.display().to_string()),
        None => (String::new(), String::new()),
    };

    Ok(QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: format!(
            "export-{}",
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ),
        kind: "export".to_string(),
        summary: format!(
            "exported formal context with {} objects, {} attributes, {} incidences -> {}",
            manifest.object_count,
            manifest.attribute_count,
            manifest.incidence_count,
            output_dir.display()
        ),
        confidence: 0.97,
        entities: vec![json!({
            "version": FORMAL_CONTEXT_VERSION,
            "repo_root": index.root,
            "output_dir": output_dir.display().to_string(),
            "manifest": manifest_path.display().to_string(),
            "objects": manifest.object_count,
            "attributes": manifest.attribute_count,
            "incidences": manifest.incidence_count,
            "object_kinds": manifest.object_kinds,
            "lattice_built": lattice.is_some(),
            "lattice_concepts": lattice.as_ref().map_or(0, |row| row.concepts.len()),
            "lattice_morphisms": lattice
                .as_ref()
                .map_or(0, |row| row.category.morphisms.len()),
            "functor_coherence": lattice.as_ref().map_or(0.0, |row| row.functor.coherence),
            "heading_objects": lattice.as_ref().map_or(0, |row| row.heading_objects.len()),
            "heading_morphisms": lattice
                .as_ref()
                .map_or(0, |row| row.heading_category.morphisms.len()),
            "heading_functor": lattice
                .as_ref()
                .map_or("", |row| row.heading_functor.name.as_str()),
            "heading_functor_coherence": lattice
                .as_ref()
                .map_or(0.0, |row| row.heading_functor.coherence),
            "heading_functor_preserved": lattice
                .as_ref()
                .map_or(0, |row| row.heading_functor.preserved),
            "heading_functor_total": lattice
                .as_ref()
                .map_or(0, |row| row.heading_functor.total),
            "lattice": lattice_path,
            "owl": owl_path,
        })],
        evidence: vec![
            EvidenceItem {
                kind: "artifact".to_string(),
                path: manifest_path.display().to_string(),
                line: None,
                detail: "formal context manifest".to_string(),
            },
            EvidenceItem {
                kind: "artifact".to_string(),
                path: objects_path.display().to_string(),
                line: None,
                detail: "formal context objects".to_string(),
            },
            EvidenceItem {
                kind: "artifact".to_string(),
                path: attributes_path.display().to_string(),
                line: None,
                detail: "formal context attributes".to_string(),
            },
            EvidenceItem {
                kind: "artifact".to_string(),
                path: incidences_path.display().to_string(),
                line: None,
                detail: "formal context incidences".to_string(),
            },
        ]
        .into_iter()
        .chain(lattice.as_ref().map(|row| EvidenceItem {
            kind: "artifact".to_string(),
            path: lattice_path.clone(),
            line: None,
            detail: format!(
                "concept lattice ({} concepts, identity {:.3}, heading functor {:.3})",
                row.concepts.len(),
                row.functor.coherence,
                row.heading_functor.coherence
            ),
        }))
        .chain(lattice.as_ref().map(|_| EvidenceItem {
            kind: "artifact".to_string(),
            path: owl_path.clone(),
            line: None,
            detail: "induced OWL TBox (cover as rdfs:subClassOf)".to_string(),
        }))
        .collect(),
        warnings: {
            let mut warnings = if output_dir == default_formal_context_output_dir(root) {
                Vec::new()
            } else {
                vec!["non-default output directory used".to_string()]
            };
            warnings.extend(lattice_warnings);
            warnings
        },
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    })
}

pub fn default_hypergraph_output_dir(root: &Path) -> PathBuf {
    root.join(".leio-code")
        .join("exports")
        .join("hypergraph-from-formal-context-v1")
}

/// Incidence hypergraph bundle from the same `(G, M, I)` as [`export_formal_context`]:
/// attribute-centric and object-centric hyperedges plus pairwise incidences (`schema`:
/// `example.formal_context_hypergraph.v1`). Includes `fca_fast` wheel structural lattice stats
/// (same structural pairs as node-row FCA enrichment).
pub fn export_hypergraph(
    index: &RepoIndex,
    root: &Path,
    output_dir: &Path,
) -> Result<QueryEnvelope> {
    let started = Instant::now();
    let context = build_formal_context(index);

    fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    let structural_pairs = structural_incidence_pairs_for_fca(&context);
    let fca_stats = fca_structural_lattice_stats(&structural_pairs);

    let mut by_attr: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut by_obj: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut pairwise: Vec<Vec<String>> = Vec::with_capacity(context.incidences.len());

    for (object_id, attribute_id) in &context.incidences {
        by_attr
            .entry(attribute_id.clone())
            .or_default()
            .insert(object_id.clone());
        by_obj
            .entry(object_id.clone())
            .or_default()
            .insert(attribute_id.clone());
        pairwise.push(vec![object_id.clone(), attribute_id.clone()]);
    }

    let hyperedges_by_attribute: BTreeMap<String, Vec<String>> = by_attr
        .into_iter()
        .map(|(k, v)| (k, v.into_iter().collect()))
        .collect();

    let hyperedges_by_object: BTreeMap<String, Vec<String>> = by_obj
        .into_iter()
        .map(|(k, v)| (k, v.into_iter().collect()))
        .collect();

    let mut vertices: Vec<Value> =
        Vec::with_capacity(context.objects.len() + context.attributes.len());
    for obj in context.objects.values() {
        vertices.push(json!({
            "id": obj.id,
            "vertex_kind": "object",
            "kind": obj.kind,
            "label": obj.label,
        }));
    }
    for attr in context.attributes.values() {
        vertices.push(json!({
            "id": attr.id,
            "vertex_kind": "attribute",
            "category": attr.category,
            "label": attr.label,
        }));
    }

    let vertices_union = context.objects.len() + context.attributes.len();

    let semantic_tooling = json!({
        "leio_indexer": "tree-sitter (Rust, Python, JavaScript, TypeScript, TSX) plus git-aware traversal; same incidence builder as `export formal-context`.",
        "fca_fast": fca_stats,
        "example_platform": "Runtime document→ontology induction (FCA join-closure + Crepe) lives in example-platform; LEIO uses vendored `fca-fast-core` on repo structural pairs for tooling-compatible lattice summaries.",
        "example_align": "Ontology alignment is provided by the example-align service (HTTP/Arrow Flight); not invoked by this export — call separately for merge / alignment jobs.",
        "office_parsers_rs": "Binary office and email parsing is implemented in office-parsers-rs; example-api installs prebuilt PyO3 wheels from the workspace `artifacts/wheels/manylinux/` wheelhouse. LEIO indexes those crates as source text here, not parsed document IR.",
        "workspace_wheelhouse": "Cross-compiled PyO3 wheels live under `<workspace>/artifacts/wheels/manylinux/` (maturin); API Docker `COPY artifacts/wheels/manylinux/` and `pip install` them. Optional Python-side FCA (`fca_fast` from office-parsers-rs/fca-fast-py) uses the same wheels — see `make leio-code-build-fca-wheel` and leio-code README.",
    });

    let out = json!({
        "schema": "example.formal_context_hypergraph.v1",
        "source": {
            "pipeline": "leio-code export hypergraph",
            "repo_root": index.root,
            "indexed_at": index.indexed_at,
            "note": "Built from a single `build_formal_context` pass — identical incidence set to `export formal-context` on the same index.",
        },
        "counts": {
            "objects": context.objects.len(),
            "attributes": context.attributes.len(),
            "incidences": context.incidences.len(),
            "vertices_union": vertices_union,
        },
        "views": {
            "attribute_centric_hyperedges": hyperedges_by_attribute,
            "object_centric_hyperedges": hyperedges_by_object,
            "pairwise_incidences": pairwise,
        },
        "vertices": vertices,
        "semantic_tooling": semantic_tooling,
    });

    let hypergraph_path = output_dir.join("hypergraph.json");
    let pretty = serde_json::to_string_pretty(&out).context("hypergraph json")?;
    fs::write(&hypergraph_path, pretty)
        .with_context(|| format!("failed to write {}", hypergraph_path.display()))?;

    Ok(QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: format!(
            "export-hypergraph-{}",
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ),
        kind: "export".to_string(),
        summary: format!(
            "exported hypergraph ({} objects, {} attributes, {} incidences) -> {}",
            context.objects.len(),
            context.attributes.len(),
            context.incidences.len(),
            hypergraph_path.display()
        ),
        confidence: 0.96,
        entities: vec![json!({
            "schema": "example.formal_context_hypergraph.v1",
            "repo_root": index.root,
            "output_dir": output_dir.display().to_string(),
            "hypergraph": hypergraph_path.display().to_string(),
            "objects": context.objects.len(),
            "attributes": context.attributes.len(),
            "incidences": context.incidences.len(),
        })],
        evidence: vec![EvidenceItem {
            kind: "artifact".to_string(),
            path: hypergraph_path.display().to_string(),
            line: None,
            detail: "incidence hypergraph JSON (formal context)".to_string(),
        }],
        warnings: if output_dir == default_hypergraph_output_dir(root) {
            Vec::new()
        } else {
            vec!["non-default output directory used".to_string()]
        },
        meta: Some(json!({
            "fca_fast": fca_stats.clone(),
        })),
        timing_ms: started.elapsed().as_millis(),
    })
}

pub fn default_arrow_nodes_output_dir(root: &Path) -> PathBuf {
    root.join(".leio-code")
        .join("exports")
        .join("arrow-nodes-v1")
}

pub fn default_arrow_nodes_rows_path(output_dir: &Path) -> PathBuf {
    output_dir.join("nodes.arrow")
}

/// Packed cosine/lexical sidecar next to `nodes.arrow`.
///
/// Built lazily on first search (or at export) so CLI processes mmap ~one
/// vector column instead of decoding the full IPC stream.
pub fn default_search_sidecar_path(output_dir: &Path) -> PathBuf {
    output_dir.join("nodes.search")
}

pub fn default_arrow_nodes_manifest_path(output_dir: &Path) -> PathBuf {
    output_dir.join("manifest.json")
}

pub fn export_arrow_nodes(
    index: &RepoIndex,
    root: &Path,
    output_dir: &Path,
) -> Result<QueryEnvelope> {
    let started = Instant::now();

    fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    let rows_path = default_arrow_nodes_rows_path(output_dir);
    let tmp_path = output_dir.join("nodes.arrow.tmp");
    let manifest_path = default_arrow_nodes_manifest_path(output_dir);
    let mut kind_counts = BTreeMap::new();
    let mut row_count = 0usize;
    let mut batch_count = 0usize;
    let mut file = Some(
        File::create(&tmp_path)
            .with_context(|| format!("failed to create {}", tmp_path.display()))?,
    );
    let mut writer: Option<StreamWriter<File>> = None;

    // Buffer every entity so the FCA enrichment pass below can see the full
    // (object, attribute) bipartite relation before any row hits the file.
    let mut entities: Vec<Value> = Vec::new();
    visit_node_entities(index, root, |entity| {
        if let Some(kind) = entity.get("kind").and_then(Value::as_str) {
            *kind_counts.entry(kind.to_string()).or_insert(0usize) += 1;
        }
        row_count += 1;
        entities.push(entity);
    });

    let fca_stats = enrich_entities_with_fca(index, &mut entities);

    // Fill the three float vectors with real canonical BGE-M3 embeddings of three
    // distinct text views (identifier / natural-language / ontology). One embed
    // call per view-batch amortizes the encoder round-trip. Best-effort: if the
    // encoder is unreachable the placeholder zero vectors survive and the rows
    // stay `embed_model: "none"`, so the vector-ANN arm degrades cleanly instead
    // of aborting the whole export.
    let mut embed_warnings: Vec<String> = Vec::new();
    let embedded_rows = match crate::embed::embed_node_entities(root, &mut entities) {
        Ok(count) => count,
        Err(err) => {
            embed_warnings.push(format!(
                "semantic embedding skipped ({err}); rows written with placeholder vectors (embed_model=none)"
            ));
            0
        }
    };

    let flush_pending = |pending: &mut Vec<Value>,
                         writer: &mut Option<StreamWriter<File>>,
                         file: &mut Option<File>,
                         batch_count: &mut usize|
     -> Result<()> {
        let batch = build_leio_row_batch(pending).map_err(anyhow::Error::msg)?;
        if writer.is_none() {
            let sink = file
                .take()
                .context("Arrow output file was not available for first batch")?;
            *writer = Some(
                StreamWriter::try_new(sink, &batch.schema())
                    .context("failed to create Arrow IPC stream writer")?,
            );
        }
        writer
            .as_mut()
            .context("Arrow IPC stream writer was not initialized")?
            .write(&batch)
            .context("failed to write LEIO node batch")?;
        pending.clear();
        *batch_count += 1;
        Ok(())
    };

    let mut pending: Vec<Value> = Vec::with_capacity(ARROW_NODES_BATCH_SIZE);
    for entity in entities {
        pending.push(entity);
        if pending.len() >= ARROW_NODES_BATCH_SIZE {
            flush_pending(&mut pending, &mut writer, &mut file, &mut batch_count)?;
        }
    }

    if row_count == 0 || !pending.is_empty() {
        flush_pending(&mut pending, &mut writer, &mut file, &mut batch_count)?;
    }

    writer
        .as_mut()
        .context("Arrow IPC stream writer was not initialized")?
        .finish()
        .context("failed to finish LEIO node Arrow IPC stream")?;
    drop(writer);
    fs::rename(&tmp_path, &rows_path).with_context(|| {
        format!(
            "failed to publish {} over {}",
            tmp_path.display(),
            rows_path.display()
        )
    })?;
    if let Err(err) = crate::node_search::rebuild_next_to_arrow(output_dir) {
        embed_warnings.push(format!("search sidecar rebuild skipped ({err})"));
    }

    let manifest = ArrowNodesManifest {
        version: ARROW_NODES_VERSION,
        repo_root: index.root.clone(),
        indexed_at: index.indexed_at.clone(),
        exported_at: OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .context("failed to format export timestamp")?,
        rows_path: relative_to(output_dir, &rows_path),
        row_count,
        batch_count,
        kind_counts,
        fca: Some(fca_stats.clone()),
    };
    let manifest_raw = serde_json::to_string_pretty(&manifest)
        .context("failed to serialize Arrow node manifest")?;
    fs::write(&manifest_path, manifest_raw)
        .with_context(|| format!("failed to write {}", manifest_path.display()))?;

    Ok(QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: format!(
            "export-arrow-nodes-{}",
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ),
        kind: "export".to_string(),
        summary: format!(
            "exported {} LEIO Arrow nodes in {} batches -> {}",
            manifest.row_count,
            manifest.batch_count,
            output_dir.display()
        ),
        confidence: 0.95,
        entities: vec![json!({
            "version": ARROW_NODES_VERSION,
            "repo_root": index.root,
            "output_dir": output_dir.display().to_string(),
            "manifest": manifest_path.display().to_string(),
            "rows_path": rows_path.display().to_string(),
            "row_count": manifest.row_count,
            "batch_count": manifest.batch_count,
            "kind_counts": manifest.kind_counts,
        })],
        evidence: vec![
            EvidenceItem {
                kind: "artifact".to_string(),
                path: manifest_path.display().to_string(),
                line: None,
                detail: "Arrow node export manifest".to_string(),
            },
            EvidenceItem {
                kind: "artifact".to_string(),
                path: rows_path.display().to_string(),
                line: None,
                detail: "Arrow node IPC stream".to_string(),
            },
        ],
        warnings: {
            let mut warnings = if output_dir == default_arrow_nodes_output_dir(root) {
                Vec::new()
            } else {
                vec!["non-default output directory used".to_string()]
            };
            warnings.extend(embed_warnings.iter().cloned());
            warnings
        },
        meta: Some(json!({
            "transport": "arrow_ipc",
            "fca": fca_stats,
            "embed": {
                "model": crate::embed::EMBED_MODEL,
                "dim": crate::embed::EMBED_DIM,
                "embedded_rows": embedded_rows,
                "total_rows": row_count,
                "encoder": "remote_bge_m3",
                "warnings": embed_warnings,
            },
        })),
        timing_ms: started.elapsed().as_millis(),
    })
}

/// Structural pairs for live lattice induction and freshness checks.
/// Preserve the explicit repository root when reading live wiki inputs.
pub(crate) fn structural_fca_pairs_at(index: &RepoIndex, root: &Path) -> Vec<(String, String)> {
    let mut context = build_formal_context(index);
    add_wiki_sections(&mut context, root);
    structural_incidence_pairs_for_fca(&context)
}

fn add_wiki_sections(builder: &mut ContextBuilder, root: &Path) {
    for section in crate::knowledge::wiki_section_incidences(root) {
        let object_id = section.object_id();
        let mut metadata = BTreeMap::new();
        metadata.insert("file".to_string(), section.source_path.clone());
        metadata.insert("heading".to_string(), section.heading_path.clone());
        builder.add_object(FormalContextObject {
            id: object_id.clone(),
            kind: "section".to_string(),
            label: section.title.clone(),
            path: Some(section.source_path.clone()),
            line: Some(section.line as usize),
            language: Some("markdown".to_string()),
            metadata,
        });
        builder.add_incidence(&object_id, "kind", "kind:section");
        builder.add_incidence(&object_id, "topic", format!("topic:{}", section.topic));
        // Every ancestor heading is an attribute so a child section's intent
        // contains its parent's tags (heading functor can preserve subClassOf).
        for segment in crate::knowledge::heading_segments(&section.heading_path) {
            builder.add_incidence(&object_id, "heading", format!("heading:{segment}"));
        }
        builder.add_incidence(
            &object_id,
            "file",
            format!("inFile:{}", section.source_path),
        );
        builder.add_incidence(
            &format!("file:{}", section.source_path),
            "has_section",
            format!("hasSection:{}", section.title),
        );
    }
}

/// Structural `(object_id, attribute_label)` pairs for lattice induction via the wheel.
/// Excludes `symbol:` objects so the lattice stays tractable (same rule as node-row enrichment).
fn structural_incidence_pairs_for_fca(context: &ContextBuilder) -> Vec<(String, String)> {
    let mut pairs = Vec::with_capacity(context.incidences.len());
    for (object_id, attribute_id) in &context.incidences {
        if object_id.starts_with("symbol:") {
            continue;
        }
        if let Some(attr) = context.attributes.get(attribute_id) {
            pairs.push((object_id.clone(), attr.label.clone()));
        }
    }
    pairs
}

fn fca_structural_lattice_stats(pairs: &[(String, String)]) -> Value {
    if pairs.is_empty() {
        return json!({
            "skipped_reason": "no structural pairs after excluding symbol:* objects",
            "input_pairs": 0,
            "distinct_primary_concepts": 0,
            "membership_objects": 0,
        });
    }
    let memberships = match fca::induct(pairs) {
        Ok(lattice) => fca::derive_memberships(&lattice, MembershipOptions::default()),
        Err(err) => {
            return json!({
                "skipped_reason": format!("fca wheel unavailable: {err}"),
                "input_pairs": pairs.len(),
                "distinct_primary_concepts": 0,
                "membership_objects": 0,
            });
        }
    };
    let mut concepts = HashSet::new();
    for m in memberships.values() {
        concepts.insert(m.primary_concept_id.clone());
    }
    json!({
        "wheel": "fca_fast",
        "role": "structural lattice over non-symbol objects — same pairs as node-row tagging",
        "input_pairs": pairs.len(),
        "membership_objects": memberships.len(),
        "distinct_primary_concepts": concepts.len(),
    })
}

/// In-place FCA enrichment for a buffered set of node entities.
///
/// Builds the rich formal context over the index (the same one
/// `export formal-context` emits — files, symbols, env vars, redis keys, api
/// routes, docker services, cartridges, profiles, secret sets, plus their
/// derived `topic:` / `family:` attributes), induces the concept lattice via
/// the pre-built `fca_fast` wheel, then appends `fcaConcept:` / `fcaFamily:` / `fcaIntent:`
/// / `fcaParent:` tags to each row's `relations` keyed by
/// `metadata.logical_id`.
///
/// Set `LEIO_FCA_ENRICH=0` to skip enrichment (e.g. for benchmarking).
/// Returns a structured summary for the export envelope's `meta.fca`.
fn enrich_entities_with_fca(index: &RepoIndex, entities: &mut [Value]) -> Value {
    let mut stats = json!({
        "enabled": true,
        "rows_tagged": 0,
        "tags_added": 0,
        "concepts": 0,
        "input_pairs": 0,
        "skipped_reason": Value::Null,
    });

    if std::env::var("LEIO_FCA_ENRICH").as_deref() == Ok("0") {
        stats["enabled"] = json!(false);
        stats["skipped_reason"] = json!("LEIO_FCA_ENRICH=0");
        return stats;
    }

    if entities.is_empty() {
        stats["skipped_reason"] = json!("no entities");
        return stats;
    }

    let context = build_formal_context(index);

    // Restrict the FCA induction to *structural* objects — files, cartridges,
    // api_routes, deploy_targets, profiles, secret_sets, docker_services,
    // env_vars, redis_keys. Individual symbols are excluded because they
    // (a) account for ~88% of the formal context (~75k of ~85k objects),
    // (b) inherit their semantic content from the parent file, and
    // (c) blow up the concept lattice from O(seconds) to O(minutes) without
    //     adding meaningful new partitions.
    //
    // Symbols still receive FCA tags below: each symbol inherits its parent
    // file's membership, which is exactly the partition we'd want.
    let pairs = structural_incidence_pairs_for_fca(&context);

    stats["input_pairs"] = json!(pairs.len());

    if pairs.is_empty() {
        stats["skipped_reason"] = json!("no incidences");
        return stats;
    }

    let memberships = match fca::induct(&pairs) {
        Ok(lattice) => fca::derive_memberships(&lattice, MembershipOptions::default()),
        Err(err) => {
            stats["skipped_reason"] = json!(format!("fca wheel unavailable: {err}"));
            return stats;
        }
    };

    let mut rows_tagged = 0usize;
    let mut tags_added = 0usize;
    let mut concept_ids = std::collections::HashSet::new();

    for entity in entities.iter_mut() {
        let Some(logical_id) = logical_id_of(entity) else {
            continue;
        };
        let membership = memberships.get(&logical_id).or_else(|| {
            // Symbol / env_var / redis_key entities aren't first-class objects
            // in the bounded FCA induction above. Inherit the parent file's
            // membership when the entity carries a path field.
            entity
                .get("path")
                .and_then(Value::as_str)
                .filter(|path| !path.is_empty())
                .and_then(|path| memberships.get(&format!("file:{path}")))
        });
        let Some(membership) = membership else {
            continue;
        };
        let added = append_fca_tags(entity, membership, &mut concept_ids);
        if added > 0 {
            rows_tagged += 1;
            tags_added += added;
        }
    }

    stats["rows_tagged"] = json!(rows_tagged);
    stats["tags_added"] = json!(tags_added);
    stats["concepts"] = json!(concept_ids.len());
    stats
}

fn logical_id_of(entity: &Value) -> Option<String> {
    entity
        .pointer("/metadata/logical_id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Append FCA tags to `entity.relations`. Returns the number of tags added.
fn append_fca_tags(
    entity: &mut Value,
    membership: &ConceptMembership,
    concept_ids: &mut std::collections::HashSet<String>,
) -> usize {
    let Some(obj) = entity.as_object_mut() else {
        return 0;
    };
    let relations = obj
        .entry("relations".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(arr) = relations.as_array_mut() else {
        return 0;
    };

    let mut existing: std::collections::HashSet<String> = arr
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();

    let mut added = 0usize;
    let push_tag =
        |tag: String, arr: &mut Vec<Value>, existing: &mut std::collections::HashSet<String>| {
            if existing.insert(tag.clone()) {
                arr.push(Value::String(tag));
                return true;
            }
            false
        };

    // Primary concept: fcaConcept + fcaFamily + fcaIntent for top intent tokens.
    if push_tag(
        format!("fcaConcept:{}", membership.primary_concept_id),
        arr,
        &mut existing,
    ) {
        added += 1;
    }
    concept_ids.insert(membership.primary_concept_id.clone());

    if push_tag(
        format!("fcaFamily:{}", membership.primary_concept_family),
        arr,
        &mut existing,
    ) {
        added += 1;
    }

    for token in &membership.primary_intent_tokens {
        let slug = slug_token(token);
        if slug.is_empty() {
            continue;
        }
        if push_tag(format!("fcaIntent:{slug}"), arr, &mut existing) {
            added += 1;
        }
    }

    // Additional concepts: fcaConcept, fcaFamily, fcaParent links.
    for extra in &membership.additional {
        if push_tag(
            format!("fcaConcept:{}", extra.concept_id),
            arr,
            &mut existing,
        ) {
            added += 1;
        }
        concept_ids.insert(extra.concept_id.clone());
        if push_tag(
            format!("fcaFamily:{}", extra.concept_family),
            arr,
            &mut existing,
        ) {
            added += 1;
        }
        for parent in &extra.parent_concept_ids {
            if push_tag(format!("fcaParent:{parent}"), arr, &mut existing) {
                added += 1;
            }
        }
    }

    added
}

/// ASCII-lowercase, alphanumeric-only slug. Matches the slug shape used by
/// the relation hint generator in `derive_search_intent`, so query-time
/// `fcaIntent:<ngram>` hints actually align with row-time tags.
fn slug_token(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut last_sep = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_sep = false;
        } else if !last_sep {
            out.push('_');
            last_sep = true;
        }
    }
    out.trim_matches('_').to_string()
}

pub fn read_arrow_node_batches(output_dir: &Path) -> Result<Vec<RecordBatch>> {
    let rows_path = default_arrow_nodes_rows_path(output_dir);
    crate::arrow_ipc::read_ipc_stream_path(&rows_path)
        .with_context(|| format!("failed to read {}", rows_path.display()))
}

pub fn record_batch_node_ids(batch: &RecordBatch) -> Result<Vec<String>> {
    let node_ids = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .context("LEIO node batch column 0 was not Utf8 node_id")?;
    Ok((0..node_ids.len())
        .map(|row| node_ids.value(row).to_string())
        .collect())
}

pub fn record_batch_to_node_entities(batch: &RecordBatch) -> Result<Vec<Value>> {
    let node_ids = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .context("LEIO node batch column 0 was not Utf8 node_id")?;
    let tenant_ids = batch
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .context("LEIO node batch column 1 was not Utf8 tenant_id")?;
    let repos = batch
        .column(2)
        .as_any()
        .downcast_ref::<StringArray>()
        .context("LEIO node batch column 2 was not Utf8 repo")?;
    let revs = batch
        .column(3)
        .as_any()
        .downcast_ref::<StringArray>()
        .context("LEIO node batch column 3 was not Utf8 rev")?;
    let paths = batch
        .column(4)
        .as_any()
        .downcast_ref::<StringArray>()
        .context("LEIO node batch column 4 was not Utf8 path")?;
    let langs = batch
        .column(5)
        .as_any()
        .downcast_ref::<StringArray>()
        .context("LEIO node batch column 5 was not Utf8 lang")?;
    let kinds = batch
        .column(6)
        .as_any()
        .downcast_ref::<StringArray>()
        .context("LEIO node batch column 6 was not Utf8 kind")?;
    let symbols = batch
        .column(7)
        .as_any()
        .downcast_ref::<StringArray>()
        .context("LEIO node batch column 7 was not Utf8 symbol")?;
    let targets = batch
        .column(8)
        .as_any()
        .downcast_ref::<StringArray>()
        .context("LEIO node batch column 8 was not Utf8 target")?;
    let relations = batch
        .column(9)
        .as_any()
        .downcast_ref::<ListArray>()
        .context("LEIO node batch column 9 was not List<Utf8> relations")?;
    let metadata = batch
        .column(10)
        .as_any()
        .downcast_ref::<StringArray>()
        .context("LEIO node batch column 10 was not Utf8 metadata")?;
    let snippets = batch
        .column(11)
        .as_any()
        .downcast_ref::<StringArray>()
        .context("LEIO node batch column 11 was not Utf8 text_snippet")?;
    let embed_models = batch
        .column(12)
        .as_any()
        .downcast_ref::<StringArray>()
        .context("LEIO node batch column 12 was not Utf8 embed_model")?;
    let embed_dims = batch
        .column(13)
        .as_any()
        .downcast_ref::<Int64Array>()
        .context("LEIO node batch column 13 was not Int64 embed_dim")?;
    let code_vecs = batch
        .column(14)
        .as_any()
        .downcast_ref::<ListArray>()
        .context("LEIO node batch column 14 was not List<Float32> code_vec")?;
    let semantic_vecs = batch
        .column(15)
        .as_any()
        .downcast_ref::<ListArray>()
        .context("LEIO node batch column 15 was not List<Float32> semantic_vec")?;
    let ontology_vecs = batch
        .column(16)
        .as_any()
        .downcast_ref::<ListArray>()
        .context("LEIO node batch column 16 was not List<Float32> ontology_vec")?;
    let execution_vecs = batch
        .column(17)
        .as_any()
        .downcast_ref::<BinaryArray>()
        .context("LEIO node batch column 17 was not Binary execution_vec_bin")?;

    let mut entities = Vec::with_capacity(batch.num_rows());
    for row in 0..batch.num_rows() {
        let metadata_value = if metadata.is_null(row) {
            Value::Null
        } else {
            serde_json::from_str::<Value>(metadata.value(row))
                .with_context(|| format!("failed to parse metadata JSON at row {row}"))?
        };
        entities.push(json!({
            "node_id": node_ids.value(row),
            "tenant_id": tenant_ids.value(row),
            "repo": repos.value(row),
            "rev": revs.value(row),
            "path": paths.value(row),
            "lang": langs.value(row),
            "kind": kinds.value(row),
            "symbol": symbols.value(row),
            "target": targets.value(row),
            "relations": string_list_value(relations, row)?,
            "metadata": metadata_value,
            "text_snippet": snippets.value(row),
            "embed_model": embed_models.value(row),
            "embed_dim": embed_dims.value(row),
            "code_vec": float_list_value(code_vecs, row)?,
            "semantic_vec": float_list_value(semantic_vecs, row)?,
            "ontology_vec": float_list_value(ontology_vecs, row)?,
            "execution_vec_bin": execution_vecs
                .value(row)
                .iter()
                .copied()
                .map(|value| json!(value))
                .collect::<Vec<_>>(),
        }));
    }
    Ok(entities)
}

fn build_formal_context(index: &RepoIndex) -> ContextBuilder {
    let mut builder = ContextBuilder::default();
    let mut aggregated_envs: BTreeMap<String, AggregatedEnvVar> = BTreeMap::new();
    let mut aggregated_redis: BTreeMap<String, AggregatedRedisKey> = BTreeMap::new();
    let api_routes = collect_api_routes(index);
    let docker_services = collect_docker_services(index);

    for file in &index.files {
        let object_id = format!("file:{}", file.path);
        let language = file.language.as_str().to_string();
        let root = root_component(&file.path).unwrap_or("root").to_string();
        let extension = extension_for_path(&file.path).unwrap_or("none").to_string();
        let mut metadata = BTreeMap::new();
        metadata.insert("root".to_string(), root.clone());
        metadata.insert("extension".to_string(), extension.clone());
        metadata.insert("bytes".to_string(), file.bytes.to_string());

        builder.add_object(FormalContextObject {
            id: object_id.clone(),
            kind: "file".to_string(),
            label: file.path.clone(),
            path: Some(file.path.clone()),
            line: None,
            language: Some(language.clone()),
            metadata,
        });

        builder.add_incidence(&object_id, "kind", "kind:file");
        builder.add_incidence(&object_id, "language", format!("lang:{language}"));
        builder.add_incidence(&object_id, "root", format!("root:{root}"));
        builder.add_incidence(&object_id, "extension", format!("ext:{extension}"));

        for topic in topics_for_text(&file.path) {
            builder.add_incidence(&object_id, "topic", format!("topic:{topic}"));
        }
        for family in families_for_text(&file.path) {
            builder.add_incidence(&object_id, "family", format!("family:{family}"));
        }

        let mut symbol_kinds = BTreeSet::new();
        for symbol in &file.symbols {
            symbol_kinds.insert(symbol.kind.as_str().to_string());
        }
        for symbol_kind in symbol_kinds {
            builder.add_incidence(
                &object_id,
                "declares_symbol_kind",
                format!("declaresSymbolKind:{symbol_kind}"),
            );
        }

        for env_var in &file.env_vars {
            builder.add_incidence(
                &object_id,
                "env_access",
                env_attribute_label(env_var.access, &env_var.name),
            );

            let aggregate = aggregated_envs
                .entry(env_var.name.clone())
                .or_insert_with(|| AggregatedEnvVar {
                    roots: BTreeSet::new(),
                    languages: BTreeSet::new(),
                    accesses: BTreeSet::new(),
                });
            aggregate.roots.insert(root.clone());
            aggregate.languages.insert(language.clone());
            aggregate
                .accesses
                .insert(env_var.access.as_str().to_string());
        }

        for redis_key in &file.redis_keys {
            builder.add_incidence(
                &object_id,
                "redis_access",
                redis_attribute_label(redis_key.access, &redis_key.key),
            );

            let aggregate = aggregated_redis
                .entry(redis_key.key.clone())
                .or_insert_with(|| AggregatedRedisKey {
                    roots: BTreeSet::new(),
                    languages: BTreeSet::new(),
                    accesses: BTreeSet::new(),
                });
            aggregate.roots.insert(root.clone());
            aggregate.languages.insert(language.clone());
            aggregate
                .accesses
                .insert(redis_key.access.as_str().to_string());
        }
    }

    for symbol in index.all_symbols() {
        let object_id = format!(
            "symbol:{}:{}:{}:{}",
            symbol.path,
            symbol.line,
            symbol.kind.as_str(),
            symbol.name
        );
        let language = symbol.language.as_str().to_string();
        let root = root_component(&symbol.path).unwrap_or("root").to_string();
        let mut metadata = BTreeMap::new();
        metadata.insert("root".to_string(), root.clone());
        metadata.insert("symbol_kind".to_string(), symbol.kind.as_str().to_string());

        builder.add_object(FormalContextObject {
            id: object_id.clone(),
            kind: "symbol".to_string(),
            label: symbol.name.clone(),
            path: Some(symbol.path.clone()),
            line: Some(symbol.line),
            language: Some(language.clone()),
            metadata,
        });

        builder.add_incidence(&object_id, "kind", "kind:symbol");
        builder.add_incidence(
            &object_id,
            "symbol_kind",
            format!("symbolKind:{}", symbol.kind.as_str()),
        );
        builder.add_incidence(&object_id, "language", format!("lang:{language}"));
        builder.add_incidence(&object_id, "root", format!("root:{root}"));

        for topic in topics_for_text(&format!("{} {}", symbol.path, symbol.name)) {
            builder.add_incidence(&object_id, "topic", format!("topic:{topic}"));
        }
        for family in families_for_text(&format!("{} {}", symbol.path, symbol.name)) {
            builder.add_incidence(&object_id, "family", format!("family:{family}"));
        }
    }

    for target in &index.deploy_targets {
        let object_id = format!("deploy_target:{}", target.name);
        let mut metadata = BTreeMap::new();
        metadata.insert("manifest".to_string(), target.path.clone());
        if let Some(profile) = &target.backend_profile {
            metadata.insert("backend_profile".to_string(), profile.clone());
        }
        if let Some(secret_set) = &target.secret_set {
            metadata.insert("secret_set".to_string(), secret_set.clone());
        }

        builder.add_object(FormalContextObject {
            id: object_id.clone(),
            kind: "deploy_target".to_string(),
            label: target.name.clone(),
            path: Some(target.path.clone()),
            line: None,
            language: Some(SourceLanguage::Toml.as_str().to_string()),
            metadata,
        });

        add_deploy_target_attributes(&mut builder, &object_id, target);
    }

    let mut cartridge_names = index.cartridge_names().into_iter().collect::<Vec<_>>();
    cartridge_names.sort();
    for cartridge in cartridge_names {
        let object_id = format!("cartridge:{cartridge}");
        let prefix = format!("cartridges/{cartridge}/");
        let source_file_count = index
            .files
            .iter()
            .filter(|file| file.path.starts_with(&prefix))
            .count();
        let deploy_targets = index
            .deploy_targets
            .iter()
            .filter(|target| target.cartridges.iter().any(|value| value == &cartridge))
            .map(|target| target.name.clone())
            .collect::<Vec<_>>();
        let required_integrations = index
            .deploy_targets
            .iter()
            .filter(|target| target.cartridges.iter().any(|value| value == &cartridge))
            .flat_map(|target| target.required_integrations.iter().cloned())
            .collect::<BTreeSet<_>>();
        let mut metadata = BTreeMap::new();
        metadata.insert(
            "source_file_count".to_string(),
            source_file_count.to_string(),
        );
        metadata.insert(
            "deploy_target_count".to_string(),
            deploy_targets.len().to_string(),
        );

        builder.add_object(FormalContextObject {
            id: object_id.clone(),
            kind: "cartridge".to_string(),
            label: cartridge.clone(),
            path: Some(format!("cartridges/{cartridge}")),
            line: None,
            language: Some(SourceLanguage::Text.as_str().to_string()),
            metadata,
        });

        builder.add_incidence(&object_id, "kind", "kind:cartridge");
        builder.add_incidence(&object_id, "root", "root:cartridges");
        builder.add_incidence(
            &object_id,
            "source_files",
            format!("sourceFiles:{source_file_count}"),
        );
        for target in deploy_targets {
            builder.add_incidence(
                &object_id,
                "deploy_target",
                format!("deployTarget:{target}"),
            );
        }
        for integration in required_integrations {
            builder.add_incidence(
                &object_id,
                "integration",
                format!("dependsOn:{integration}"),
            );
        }
        for topic in topics_for_text(&cartridge) {
            builder.add_incidence(&object_id, "topic", format!("topic:{topic}"));
        }
        for family in families_for_text(&cartridge) {
            builder.add_incidence(&object_id, "family", format!("family:{family}"));
        }
    }

    for route in &api_routes {
        let object_id = api_route_logical_id(route);
        let mut metadata = BTreeMap::new();
        metadata.insert("mount_status".to_string(), route.mount_status.clone());
        metadata.insert("auth_policy".to_string(), route.auth_policy.clone());
        metadata.insert("route_role".to_string(), route.route_role.clone());
        metadata.insert("route_family".to_string(), route.route_family.clone());

        builder.add_object(FormalContextObject {
            id: object_id.clone(),
            kind: "api_route".to_string(),
            label: route.full_path.clone(),
            path: Some(route.file_path.clone()),
            line: Some(route.line),
            language: Some(route.language.clone()),
            metadata,
        });

        builder.add_incidence(&object_id, "kind", "kind:api_route");
        builder.add_incidence(
            &object_id,
            "route_family",
            format!("routeFamily:{}", route.route_family),
        );
        builder.add_incidence(
            &object_id,
            "mount_status",
            format!("mountStatus:{}", route.mount_status),
        );
        builder.add_incidence(
            &object_id,
            "auth_policy",
            format!("authPolicy:{}", route.auth_policy),
        );
        builder.add_incidence(
            &object_id,
            "route_role",
            format!("routeRole:{}", route.route_role),
        );
        for method in &route.methods {
            builder.add_incidence(&object_id, "method", format!("method:{method}"));
        }
        if let Some(cartridge) = route
            .file_path
            .strip_prefix("cartridges/")
            .and_then(|path| {
                let mut segments = path.split('/');
                segments.next().filter(|value| !value.is_empty())
            })
        {
            builder.add_incidence(&object_id, "cartridge", format!("cartridge:{cartridge}"));
        }
        for topic in topics_for_text(&format!(
            "{} {} {} {}",
            route.full_path, route.file_path, route.auth_policy, route.route_family
        )) {
            builder.add_incidence(&object_id, "topic", format!("topic:{topic}"));
        }
    }

    for service in &docker_services {
        let object_id = docker_service_logical_id(service);
        let mut metadata = BTreeMap::new();
        if let Some(image) = &service.image {
            metadata.insert("image".to_string(), image.clone());
        }
        if let Some(build_context) = &service.build_context {
            metadata.insert("build_context".to_string(), build_context.clone());
        }
        metadata.insert(
            "profile_count".to_string(),
            service.profiles.len().to_string(),
        );

        builder.add_object(FormalContextObject {
            id: object_id.clone(),
            kind: "docker_service".to_string(),
            label: service.name.clone(),
            path: Some(service.file_path.clone()),
            line: None,
            language: Some(SourceLanguage::Yaml.as_str().to_string()),
            metadata,
        });

        builder.add_incidence(&object_id, "kind", "kind:docker_service");
        builder.add_incidence(&object_id, "root", "root:docker_compose");
        if let Some(image) = &service.image {
            builder.add_incidence(&object_id, "image", format!("image:{image}"));
        }
        if let Some(build_context) = &service.build_context {
            builder.add_incidence(
                &object_id,
                "build_context",
                format!("buildContext:{build_context}"),
            );
        }
        for profile in &service.profiles {
            builder.add_incidence(&object_id, "profile", format!("profile:{profile}"));
        }
        for topic in topics_for_text(&format!(
            "{} {} {:?} {:?}",
            service.name, service.file_path, service.image, service.build_context
        )) {
            builder.add_incidence(&object_id, "topic", format!("topic:{topic}"));
        }
    }

    for profile in &index.profiles {
        let object_id = format!("profile:{}", profile.name);
        let mut metadata = BTreeMap::new();
        metadata.insert("path".to_string(), profile.path.clone());
        metadata.insert("var_count".to_string(), profile.vars.len().to_string());

        builder.add_object(FormalContextObject {
            id: object_id.clone(),
            kind: "profile".to_string(),
            label: profile.name.clone(),
            path: Some(profile.path.clone()),
            line: None,
            language: Some(SourceLanguage::Env.as_str().to_string()),
            metadata,
        });

        builder.add_incidence(&object_id, "kind", "kind:profile");
        builder.add_incidence(&object_id, "root", "root:deploy_profiles");
        builder.add_incidence(
            &object_id,
            "profile",
            format!("profileName:{}", profile.name),
        );
        add_declared_var_attributes(&mut builder, &object_id, &profile.vars, true);
        for topic in topics_for_text(&collect_declared_var_text(profile)) {
            builder.add_incidence(&object_id, "topic", format!("topic:{topic}"));
        }
    }

    for secret_set in &index.secret_sets {
        let object_id = format!("secret_set:{}", secret_set.name);
        let mut metadata = BTreeMap::new();
        metadata.insert("path".to_string(), secret_set.path.clone());
        metadata.insert("var_count".to_string(), secret_set.vars.len().to_string());

        builder.add_object(FormalContextObject {
            id: object_id.clone(),
            kind: "secret_set".to_string(),
            label: secret_set.name.clone(),
            path: Some(secret_set.path.clone()),
            line: None,
            language: Some(SourceLanguage::Env.as_str().to_string()),
            metadata,
        });

        builder.add_incidence(&object_id, "kind", "kind:secret_set");
        builder.add_incidence(&object_id, "root", "root:deploy_secret_sets");
        builder.add_incidence(
            &object_id,
            "secret_set",
            format!("secretSetName:{}", secret_set.name),
        );
        add_declared_var_attributes(&mut builder, &object_id, &secret_set.vars, false);
        for topic in topics_for_text(&collect_declared_var_text(secret_set)) {
            builder.add_incidence(&object_id, "topic", format!("topic:{topic}"));
        }
    }

    for (env_name, aggregate) in aggregated_envs {
        let object_id = format!("env_var:{env_name}");
        let mut metadata = BTreeMap::new();
        metadata.insert("root_count".to_string(), aggregate.roots.len().to_string());
        metadata.insert(
            "language_count".to_string(),
            aggregate.languages.len().to_string(),
        );

        builder.add_object(FormalContextObject {
            id: object_id.clone(),
            kind: "env_var".to_string(),
            label: env_name.clone(),
            path: None,
            line: None,
            language: None,
            metadata,
        });

        builder.add_incidence(&object_id, "kind", "kind:env_var");
        for access in aggregate.accesses {
            builder.add_incidence(&object_id, "access", format!("envSeenAs:{access}"));
        }
        for root in aggregate.roots {
            builder.add_incidence(&object_id, "root", format!("usedInRoot:{root}"));
        }
        for language in aggregate.languages {
            builder.add_incidence(&object_id, "language", format!("usedInLang:{language}"));
        }
        for topic in topics_for_text(&env_name) {
            builder.add_incidence(&object_id, "topic", format!("topic:{topic}"));
        }
    }

    for (redis_key, aggregate) in aggregated_redis {
        let object_id = format!("redis_key:{redis_key}");
        let mut metadata = BTreeMap::new();
        metadata.insert("root_count".to_string(), aggregate.roots.len().to_string());
        metadata.insert(
            "language_count".to_string(),
            aggregate.languages.len().to_string(),
        );

        builder.add_object(FormalContextObject {
            id: object_id.clone(),
            kind: "redis_key".to_string(),
            label: redis_key.clone(),
            path: None,
            line: None,
            language: None,
            metadata,
        });

        builder.add_incidence(&object_id, "kind", "kind:redis_key");
        for access in aggregate.accesses {
            builder.add_incidence(&object_id, "access", format!("redisSeenAs:{access}"));
        }
        for root in aggregate.roots {
            builder.add_incidence(&object_id, "root", format!("usedInRoot:{root}"));
        }
        for language in aggregate.languages {
            builder.add_incidence(&object_id, "language", format!("usedInLang:{language}"));
        }
        for topic in topics_for_text(&redis_key) {
            builder.add_incidence(&object_id, "topic", format!("topic:{topic}"));
        }
        for family in families_for_text(&redis_key) {
            builder.add_incidence(&object_id, "family", format!("family:{family}"));
        }
    }

    builder
}

fn add_deploy_target_attributes(
    builder: &mut ContextBuilder,
    object_id: &str,
    target: &DeployTargetRecord,
) {
    builder.add_incidence(object_id, "kind", "kind:deploy_target");
    builder.add_incidence(object_id, "target", format!("targetName:{}", target.name));

    if let Some(deploy_class) = &target.deploy_class {
        builder.add_incidence(
            object_id,
            "deploy_class",
            format!("deployClass:{deploy_class}"),
        );
    }
    if let Some(topology) = &target.topology {
        builder.add_incidence(object_id, "topology", format!("topology:{topology}"));
    }
    if let Some(ui_role) = &target.ui_role {
        builder.add_incidence(object_id, "ui_role", format!("uiRole:{ui_role}"));
    }
    if let Some(frontend_project) = &target.frontend_project {
        builder.add_incidence(
            object_id,
            "frontend_project",
            format!("frontendProject:{frontend_project}"),
        );
    }
    if let Some(backend_profile) = &target.backend_profile {
        builder.add_incidence(
            object_id,
            "backend_profile",
            format!("backendProfile:{backend_profile}"),
        );
    }
    if let Some(secret_set) = &target.secret_set {
        builder.add_incidence(object_id, "secret_set", format!("secretSet:{secret_set}"));
    }

    builder.add_incidence(
        object_id,
        "health_checks",
        if target.health_checks.is_empty() {
            "healthChecks:missing"
        } else {
            "healthChecks:configured"
        },
    );
    builder.add_incidence(
        object_id,
        "smoke",
        if target.smoke_suite.is_some() {
            "smoke:configured"
        } else {
            "smoke:missing"
        },
    );
    builder.add_incidence(
        object_id,
        "rollback",
        if target.rollback_command.is_some() {
            "rollback:configured"
        } else {
            "rollback:missing"
        },
    );

    for cartridge in &target.cartridges {
        builder.add_incidence(object_id, "cartridge", format!("cartridge:{cartridge}"));
    }
    for integration in &target.required_integrations {
        builder.add_incidence(object_id, "integration", format!("dependsOn:{integration}"));
        for topic in topics_for_text(integration) {
            builder.add_incidence(object_id, "topic", format!("topic:{topic}"));
        }
    }
    for topic in topics_for_text(&format!(
        "{} {} {} {:?} {:?}",
        target.name,
        target.path,
        target.topology.clone().unwrap_or_default(),
        target.backend_profile,
        target.secret_set
    )) {
        builder.add_incidence(object_id, "topic", format!("topic:{topic}"));
    }
    for family in families_for_text(&target.name) {
        builder.add_incidence(object_id, "family", format!("family:{family}"));
    }
}

fn add_declared_var_attributes(
    builder: &mut ContextBuilder,
    object_id: &str,
    vars: &[DeclaredVar],
    as_env: bool,
) {
    for var in vars {
        let label = if as_env {
            format!("declaresEnv:{}", var.name)
        } else {
            format!("declaresSecret:{}", var.name)
        };
        builder.add_incidence(object_id, "declared_var", label);
        for topic in topics_for_text(&var.name) {
            builder.add_incidence(object_id, "topic", format!("topic:{topic}"));
        }
    }
}

fn collect_declared_var_text<T>(record: &T) -> String
where
    T: DeclaredVars,
{
    record
        .declared_vars()
        .iter()
        .map(|item| item.name.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

trait DeclaredVars {
    fn declared_vars(&self) -> &[DeclaredVar];
}

impl DeclaredVars for crate::model::ProfileRecord {
    fn declared_vars(&self) -> &[DeclaredVar] {
        &self.vars
    }
}

impl DeclaredVars for crate::model::SecretSetRecord {
    fn declared_vars(&self) -> &[DeclaredVar] {
        &self.vars
    }
}

fn env_attribute_label(access: AccessKind, name: &str) -> String {
    match access {
        AccessKind::Read => format!("readsEnv:{name}"),
        AccessKind::Write => format!("writesEnv:{name}"),
        AccessKind::Declared => format!("declaresEnv:{name}"),
        AccessKind::Unknown => format!("mentionsEnv:{name}"),
    }
}

fn redis_attribute_label(access: AccessKind, key: &str) -> String {
    match access {
        AccessKind::Read => format!("readsRedis:{key}"),
        AccessKind::Write => format!("writesRedis:{key}"),
        AccessKind::Declared => format!("declaresRedis:{key}"),
        AccessKind::Unknown => format!("mentionsRedis:{key}"),
    }
}

fn root_component(path: &str) -> Option<&str> {
    path.split('/').next()
}

fn extension_for_path(path: &str) -> Option<&str> {
    path.rsplit('.').next().filter(|value| *value != path)
}

fn topics_for_text(text: &str) -> BTreeSet<String> {
    let value = text.to_ascii_lowercase();
    let mut topics: BTreeSet<String> = BTreeSet::new();
    let checks = [
        ("auth", &["auth", "jwt", "token", "openid"][..]),
        ("session", &["session", "conversation", "handoff"][..]),
        ("route", &["route", "routing", "webhook"][..]),
        (
            "deploy",
            &["deploy", "compose", "rollback", "bootstrap", "vercel"][..],
        ),
        ("flight", &["flight", "arrow", "ipc"][..]),
        ("redis", &["redis", "stream", "lease", "pubsub"][..]),
        (
            "ontology",
            &["ontology", "ttl", "rdf", "json-ld", "owl", "graph"][..],
        ),
        ("trace", &["trace", "event", "sse", "prov"][..]),
        ("gepa", &["gepa", "bandit", "mcts"][..]),
        ("jcube", &["jcube", "jepa", "twin"][..]),
        (
            "autopilot",
            &["autopilot", "ops-console", "example-ops"][..],
        ),
        ("hospital", &["health", "hospital", "glosa", "sentinel"][..]),
        (
            "collections",
            &[
                "cobranca",
                "collection",
                "pacto",
                "plusoft",
                "jai pay",
                "jai_pay",
            ][..],
        ),
        ("s1000d", &["s1000d", "brex", "csdb"][..]),
    ];

    for (topic, needles) in checks {
        if needles.iter().any(|needle| value.contains(needle)) {
            topics.insert(topic.to_string());
        }
    }

    // Content-derived tokens: tokenize the identifier text (snake_case +
    // CamelCase + non-alphanumeric splits) and emit each meaningful token as
    // its own topic. This is what bridges natural-language queries like
    // "egress validation" to files whose path/symbols actually contain those
    // words, instead of relying on the curated vocabulary above to know about
    // every domain term.
    for token in extract_topic_tokens(text) {
        topics.insert(token);
    }

    topics
}

/// Yield bounded-length, lowercase, alphanumeric tokens from a free-form
/// identifier string. Splits on non-alphanumerics, snake_case, kebab-case, and
/// CamelCase boundaries. Filters short tokens, common stopwords, and
/// structural noise (file extensions, directory names like `src`/`tests`).
fn extract_topic_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut prev_lower = false;

    let flush = |buf: &mut String, out: &mut Vec<String>| {
        if buf.is_empty() {
            return;
        }
        let token = std::mem::take(buf);
        if accept_topic_token(&token) {
            out.push(token);
        }
    };

    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            // Split on lowercase→uppercase transitions to handle CamelCase.
            if ch.is_ascii_uppercase() && prev_lower {
                flush(&mut buf, &mut out);
            }
            buf.push(ch.to_ascii_lowercase());
            prev_lower = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        } else {
            flush(&mut buf, &mut out);
            prev_lower = false;
        }
    }
    flush(&mut buf, &mut out);

    // Dedupe while preserving order, capping to keep formal-context size bounded.
    let mut seen = std::collections::HashSet::new();
    out.retain(|token| seen.insert(token.clone()));
    out.truncate(12);
    out
}

fn accept_topic_token(token: &str) -> bool {
    if token.len() < 4 || token.len() > 32 {
        return false;
    }
    if token.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    !TOPIC_TOKEN_STOPWORDS.contains(&token)
}

/// Tokens that surface from path/identifier extraction but carry no semantic
/// signal: file extensions, directory conventions, common verbs, structural
/// noise. Keeping these out prevents the formal context from filling up with
/// `topic:test` / `topic:from` / `topic:json` rows that match every query.
const TOPIC_TOKEN_STOPWORDS: &[&str] = &[
    "args",
    "async",
    "await",
    "build",
    "case",
    "class",
    "code",
    "config",
    "data",
    "def",
    "deps",
    "docs",
    "else",
    "enum",
    "fail",
    "false",
    "file",
    "fn",
    "from",
    "func",
    "function",
    "get",
    "global",
    "head",
    "impl",
    "import",
    "info",
    "init",
    "into",
    "json",
    "kind",
    "lib",
    "line",
    "list",
    "main",
    "make",
    "map",
    "method",
    "mod",
    "name",
    "node",
    "none",
    "null",
    "obj",
    "ok",
    "opt",
    "out",
    "pass",
    "path",
    "pkg",
    "pub",
    "rust",
    "self",
    "set",
    "shared",
    "size",
    "src",
    "static",
    "stop",
    "struct",
    "sub",
    "super",
    "test",
    "tests",
    "text",
    "this",
    "tmp",
    "true",
    "tsx",
    "type",
    "unit",
    "unwrap",
    "util",
    "utils",
    "value",
    "vec",
    "with",
    "yaml",
    "yml",
    "node_modules",
    "tests",
    // Single-purpose programming nouns that match every codebase.
    "client",
    "server",
    "request",
    "response",
    "result",
    "error",
    "ctx",
    "context",
    "handler",
    "router",
    "router",
    "module",
];

fn families_for_text(text: &str) -> BTreeSet<&'static str> {
    let value = text.to_ascii_lowercase();
    let mut families = BTreeSet::new();
    if ["sara", "liz", "pratique", "assurant"]
        .iter()
        .any(|needle| value.contains(needle))
    {
        families.insert("customer_ops");
    }
    if ["health", "hospital", "jcube", "glosa", "sentinel"]
        .iter()
        .any(|needle| value.contains(needle))
    {
        families.insert("hospital_audit");
    }
    if value.contains("vigoros") {
        families.insert("vigoros");
    }
    if ["s1000d", "extractor"]
        .iter()
        .any(|needle| value.contains(needle))
    {
        families.insert("document_intelligence");
    }
    if ["autopilot", "ops-console", "example-ops"]
        .iter()
        .any(|needle| value.contains(needle))
    {
        families.insert("ops_surface");
    }
    families
}

fn string_list_value(list: &ListArray, row: usize) -> Result<Vec<String>> {
    let values = list.value(row);
    let values = values
        .as_any()
        .downcast_ref::<StringArray>()
        .context("LEIO node list value was not Utf8")?;
    Ok((0..values.len())
        .map(|index| values.value(index).to_string())
        .collect())
}

fn float_list_value(list: &ListArray, row: usize) -> Result<Vec<f64>> {
    let values = list.value(row);
    let values = values
        .as_any()
        .downcast_ref::<Float32Array>()
        .context("LEIO node vector value was not Float32")?;
    Ok((0..values.len())
        .map(|index| f64::from(values.value(index)))
        .collect())
}

fn relative_to(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|value| value.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

fn write_jsonl<'a, T, I>(path: &Path, items: I) -> Result<()>
where
    T: Serialize + 'a,
    I: IntoIterator<Item = &'a T>,
{
    let file =
        File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    for item in items {
        serde_json::to_writer(&mut writer, item)
            .with_context(|| format!("failed to serialize line for {}", path.display()))?;
        writer
            .write_all(b"\n")
            .with_context(|| format!("failed to write newline for {}", path.display()))?;
    }
    writer
        .flush()
        .with_context(|| format!("failed to flush {}", path.display()))?;
    Ok(())
}

// ----------------------------------------------------------------------------
// FCA induction Phase A — provenanced stream output.
//
// Sibling to the legacy [`export_formal_context`] (sidecar JSONL bundle). This
// path emits the design-doc-shaped `FormalContext` document — a single JSON or
// Arrow IPC stream over stdout / `--out` — with every (object, attribute) pair
// carrying source-line provenance. Six projections are supported; the
// `file`/`cartridge`/`env_var`/`deploy_target` shapes are re-derived from the
// same `RepoIndex` fields the legacy builder reads, while `binary` and `route`
// read directly from `index.cross_language.{binaries,resolved_spawns,routes}`
// since those aren't part of the legacy bundle's attribute namespace.
//
// The legacy bundle path is unchanged. Existing consumers
// (`export_hypergraph`, `enrich_entities_with_fca`) keep their schema and
// sidecar files. See `docs/fca-induction-design.md` for the contract.

/// Schema version for the streamed FormalContext document. Bump when the
/// JSON / Arrow shape changes in a way consumers must branch on.
const FORMAL_CONTEXT_STREAM_SCHEMA_VERSION: &str = "1.0";

/// Which graph projection to materialise. See `docs/fca-induction-design.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormalContextObjectKind {
    File,
    Cartridge,
    Binary,
    Route,
    EnvVar,
    DeployTarget,
}

impl FormalContextObjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Cartridge => "cartridge",
            Self::Binary => "binary",
            Self::Route => "route",
            Self::EnvVar => "env_var",
            Self::DeployTarget => "deploy_target",
        }
    }
}

/// Output format selector for the formal-context export.
///
/// `Bundle` is the legacy sidecar-JSONL contract under
/// `.leio-code/exports/formal-context-v1/`. `Json` and `Arrow` are the new
/// design-doc shapes (single document to stdout / `--out`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormalContextFormat {
    Bundle,
    Json,
    Arrow,
}

impl FormalContextFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bundle => "bundle",
            Self::Json => "json",
            Self::Arrow => "arrow",
        }
    }
}

/// Per-incidence provenance carried through to the streamed output.
/// Mirrors the `edge_kind` enum documented in `docs/fca-induction-design.md`.
#[derive(Debug, Clone, Serialize)]
pub struct StreamProvenance {
    pub source_path: String,
    pub source_line: u32,
    pub edge_kind: String,
    pub confidence: u8,
}

/// One incidence triple plus its provenance.
#[derive(Debug, Clone)]
struct ProvenancedIncidence {
    object: String,
    attribute: String,
    prov: StreamProvenance,
}

/// In-memory representation of the document the streamed formal-context
/// export materialises. Kept private so the JSON / Arrow encoders are the
/// only public surface.
#[derive(Debug, Clone)]
struct StreamContext {
    object_kind: FormalContextObjectKind,
    /// Sorted unique objects.
    objects: Vec<String>,
    /// Sorted unique attributes.
    attributes: Vec<String>,
    /// `object -> [attribute, …]`, sorted-stable.
    incidence: BTreeMap<String, BTreeSet<String>>,
    /// `(object, attribute) -> provenance`. First-write-wins (the data
    /// sources are deterministic so duplicates carry the same provenance).
    provenance: BTreeMap<(String, String), StreamProvenance>,
}

impl StreamContext {
    fn new(object_kind: FormalContextObjectKind) -> Self {
        Self {
            object_kind,
            objects: Vec::new(),
            attributes: Vec::new(),
            incidence: BTreeMap::new(),
            provenance: BTreeMap::new(),
        }
    }

    fn from_pairs(object_kind: FormalContextObjectKind, pairs: Vec<ProvenancedIncidence>) -> Self {
        let mut ctx = Self::new(object_kind);
        let mut objects = BTreeSet::new();
        let mut attributes = BTreeSet::new();
        for ProvenancedIncidence {
            object,
            attribute,
            prov,
        } in pairs
        {
            objects.insert(object.clone());
            attributes.insert(attribute.clone());
            ctx.incidence
                .entry(object.clone())
                .or_default()
                .insert(attribute.clone());
            ctx.provenance.entry((object, attribute)).or_insert(prov);
        }
        ctx.objects = objects.into_iter().collect();
        ctx.attributes = attributes.into_iter().collect();
        ctx
    }
}

/// Public entry-point for the streamed formal-context export.
///
/// Reads from `index`, builds the requested projection, and writes the result
/// to either `stdout` (when `out` is `None`) or `out`. `Arrow` format always
/// requires `out`; bundle is not routed through this function (see
/// [`export_formal_context`] for the legacy sidecar path).
pub fn export_formal_context_stream(
    index: &RepoIndex,
    object_kind: FormalContextObjectKind,
    format: FormalContextFormat,
    out: Option<&Path>,
) -> Result<QueryEnvelope> {
    let started = Instant::now();

    if format == FormalContextFormat::Bundle {
        anyhow::bail!(
            "export_formal_context_stream does not handle bundle format; \
             use export_formal_context for the legacy sidecar path"
        );
    }
    if format == FormalContextFormat::Arrow && out.is_none() {
        anyhow::bail!("--format=arrow requires --out=<path>");
    }

    let pairs = collect_provenanced_pairs(index, object_kind);
    let context = StreamContext::from_pairs(object_kind, pairs);

    let incidence_count: usize = context.incidence.values().map(BTreeSet::len).sum();

    let write_destination = match (format, out) {
        (FormalContextFormat::Json, Some(path)) => {
            let bytes = encode_json(&context)?;
            fs::write(path, bytes)
                .with_context(|| format!("failed to write {}", path.display()))?;
            Some(path.display().to_string())
        }
        (FormalContextFormat::Json, None) => {
            let bytes = encode_json(&context)?;
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            handle
                .write_all(&bytes)
                .context("failed to write formal-context JSON to stdout")?;
            handle
                .write_all(b"\n")
                .context("failed to write trailing newline")?;
            None
        }
        (FormalContextFormat::Arrow, Some(path)) => {
            let file = File::create(path)
                .with_context(|| format!("failed to create {}", path.display()))?;
            encode_arrow(&context, file)?;
            Some(path.display().to_string())
        }
        (FormalContextFormat::Arrow, None) => unreachable!("guarded above"),
        (FormalContextFormat::Bundle, _) => unreachable!("guarded above"),
    };

    let summary = match &write_destination {
        Some(dest) => format!(
            "exported formal-context stream ({} projection, {} format, {} objects, {} attributes, {} incidences) -> {}",
            object_kind.as_str(),
            format.as_str(),
            context.objects.len(),
            context.attributes.len(),
            incidence_count,
            dest
        ),
        None => format!(
            "exported formal-context stream ({} projection, {} format, {} objects, {} attributes, {} incidences) -> stdout",
            object_kind.as_str(),
            format.as_str(),
            context.objects.len(),
            context.attributes.len(),
            incidence_count
        ),
    };

    let evidence = match &write_destination {
        Some(dest) => vec![EvidenceItem {
            kind: "artifact".to_string(),
            path: dest.clone(),
            line: None,
            detail: format!("formal-context stream ({})", format.as_str()),
        }],
        None => Vec::new(),
    };

    Ok(QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: format!(
            "export-formal-context-stream-{}",
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ),
        kind: "export".to_string(),
        summary,
        confidence: 0.97,
        entities: vec![json!({
            "schema_version": FORMAL_CONTEXT_STREAM_SCHEMA_VERSION,
            "object_kind": object_kind.as_str(),
            "format": format.as_str(),
            "objects": context.objects.len(),
            "attributes": context.attributes.len(),
            "incidences": incidence_count,
            "out": write_destination,
        })],
        evidence,
        warnings: Vec::new(),
        meta: None,
        timing_ms: started.elapsed().as_millis(),
    })
}

fn encode_json(context: &StreamContext) -> Result<Vec<u8>> {
    // Hand-build a serde_json::Value with field order matching the design
    // doc (schema_version → object_kind → objects → attributes → incidence →
    // provenance). `serde_json::Map` preserves insertion order.
    let mut incidence_map = serde_json::Map::new();
    for (obj, attrs) in &context.incidence {
        let arr: Vec<Value> = attrs.iter().map(|a| Value::String(a.clone())).collect();
        incidence_map.insert(obj.clone(), Value::Array(arr));
    }

    let mut provenance_map = serde_json::Map::new();
    for ((obj, attr), prov) in &context.provenance {
        let key = format!("{obj}|{attr}");
        provenance_map.insert(
            key,
            json!({
                "source_path": prov.source_path,
                "source_line": prov.source_line,
                "edge_kind": prov.edge_kind,
                "confidence": prov.confidence,
            }),
        );
    }

    let mut root = serde_json::Map::new();
    root.insert(
        "schema_version".to_string(),
        json!(FORMAL_CONTEXT_STREAM_SCHEMA_VERSION),
    );
    root.insert(
        "object_kind".to_string(),
        json!(context.object_kind.as_str()),
    );
    root.insert(
        "objects".to_string(),
        Value::Array(
            context
                .objects
                .iter()
                .map(|s| Value::String(s.clone()))
                .collect(),
        ),
    );
    root.insert(
        "attributes".to_string(),
        Value::Array(
            context
                .attributes
                .iter()
                .map(|s| Value::String(s.clone()))
                .collect(),
        ),
    );
    root.insert("incidence".to_string(), Value::Object(incidence_map));
    root.insert("provenance".to_string(), Value::Object(provenance_map));

    serde_json::to_vec_pretty(&Value::Object(root))
        .context("failed to serialize formal-context stream JSON")
}

fn encode_arrow<W: Write>(context: &StreamContext, sink: W) -> Result<()> {
    use arrow_array::builder::{StringBuilder, UInt8Builder, UInt32Builder};
    use arrow_array::{ArrayRef, StructArray};
    use arrow_schema::{DataType, Field, Schema};
    use std::sync::Arc;

    let total: usize = context.incidence.values().map(BTreeSet::len).sum();

    let mut object_b = StringBuilder::with_capacity(total, total * 16);
    let mut attribute_b = StringBuilder::with_capacity(total, total * 16);
    let mut src_path_b = StringBuilder::with_capacity(total, total * 32);
    let mut src_line_b = UInt32Builder::with_capacity(total);
    let mut edge_kind_b = StringBuilder::with_capacity(total, total * 16);
    let mut confidence_b = UInt8Builder::with_capacity(total);

    for (obj, attrs) in &context.incidence {
        for attr in attrs {
            object_b.append_value(obj);
            attribute_b.append_value(attr);
            // Every (obj, attr) in `incidence` has a `provenance` entry —
            // they're built from the same pair list.
            let prov = context
                .provenance
                .get(&(obj.clone(), attr.clone()))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "missing provenance for ({obj}, {attr}) — internal invariant broken"
                    )
                })?;
            src_path_b.append_value(&prov.source_path);
            src_line_b.append_value(prov.source_line);
            edge_kind_b.append_value(&prov.edge_kind);
            confidence_b.append_value(prov.confidence);
        }
    }

    let prov_fields = vec![
        Arc::new(Field::new("source_path", DataType::Utf8, false)),
        Arc::new(Field::new("source_line", DataType::UInt32, false)),
        Arc::new(Field::new("edge_kind", DataType::Utf8, false)),
        Arc::new(Field::new("confidence", DataType::UInt8, false)),
    ];
    let provenance_array = StructArray::new(
        prov_fields.clone().into(),
        vec![
            Arc::new(src_path_b.finish()) as ArrayRef,
            Arc::new(src_line_b.finish()) as ArrayRef,
            Arc::new(edge_kind_b.finish()) as ArrayRef,
            Arc::new(confidence_b.finish()) as ArrayRef,
        ],
        None,
    );

    let schema = Arc::new(Schema::new(vec![
        Field::new("object", DataType::Utf8, false),
        Field::new("attribute", DataType::Utf8, false),
        Field::new("provenance", DataType::Struct(prov_fields.into()), false),
    ]));

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(object_b.finish()) as ArrayRef,
            Arc::new(attribute_b.finish()) as ArrayRef,
            Arc::new(provenance_array) as ArrayRef,
        ],
    )
    .context("failed to build formal-context Arrow RecordBatch")?;

    let mut writer = StreamWriter::try_new(sink, schema.as_ref())
        .context("failed to create Arrow IPC stream writer")?;
    writer
        .write(&batch)
        .context("failed to write formal-context Arrow batch")?;
    writer
        .finish()
        .context("failed to finish formal-context Arrow IPC stream")?;
    Ok(())
}

/// Parallel walker to [`build_formal_context`]: emits `(object, attribute,
/// provenance)` triples for the requested projection. Separate from the legacy
/// builder so the existing sidecar contract and downstream consumers
/// (`export_hypergraph`, `enrich_entities_with_fca`) stay byte-for-byte
/// unchanged.
fn collect_provenanced_pairs(
    index: &RepoIndex,
    object_kind: FormalContextObjectKind,
) -> Vec<ProvenancedIncidence> {
    match object_kind {
        FormalContextObjectKind::File => pairs_for_file(index),
        FormalContextObjectKind::Cartridge => pairs_for_cartridge(index),
        FormalContextObjectKind::Binary => pairs_for_binary(index),
        FormalContextObjectKind::Route => pairs_for_route(index),
        FormalContextObjectKind::EnvVar => pairs_for_env_var(index),
        FormalContextObjectKind::DeployTarget => pairs_for_deploy_target(index),
    }
}

fn pairs_for_file(index: &RepoIndex) -> Vec<ProvenancedIncidence> {
    let mut out = Vec::new();
    for file in &index.files {
        // env: attributes — one per declared/read env var occurrence.
        for env in &file.env_vars {
            out.push(ProvenancedIncidence {
                object: file.path.clone(),
                attribute: format!("env:{}", env.name),
                prov: StreamProvenance {
                    source_path: env.path.clone(),
                    source_line: env.line as u32,
                    edge_kind: "env_var_access".to_string(),
                    confidence: 90,
                },
            });
        }
        // redis: attributes
        for key in &file.redis_keys {
            out.push(ProvenancedIncidence {
                object: file.path.clone(),
                attribute: format!("redis:{}", key.key),
                prov: StreamProvenance {
                    source_path: key.path.clone(),
                    source_line: key.line as u32,
                    edge_kind: "redis_access".to_string(),
                    confidence: 90,
                },
            });
        }
        // binary: — spawned by this file (resolved cross-language).
        for spawn in &index.cross_language.resolved_spawns {
            if spawn.caller_path == file.path {
                out.push(ProvenancedIncidence {
                    object: file.path.clone(),
                    attribute: format!("binary:{}", spawn.callee_name),
                    prov: StreamProvenance {
                        source_path: spawn.caller_path.clone(),
                        source_line: spawn.caller_line as u32,
                        edge_kind: "binary_spawn".to_string(),
                        confidence: spawn.confidence,
                    },
                });
            }
        }
        // route: — declared in this file (RouteRecord canonical).
        for route in &index.cross_language.routes {
            if route.path == file.path {
                out.push(ProvenancedIncidence {
                    object: file.path.clone(),
                    attribute: format!("route:{}", route.route),
                    prov: StreamProvenance {
                        source_path: route.path.clone(),
                        source_line: route.line as u32,
                        edge_kind: "route_declaration".to_string(),
                        confidence: 95,
                    },
                });
            }
        }
        // cartridge: — file lives under cartridges/<name>/…
        if let Some(cartridge) = cartridge_from_path(&file.path) {
            out.push(ProvenancedIncidence {
                object: file.path.clone(),
                attribute: format!("cartridge:{cartridge}"),
                prov: StreamProvenance {
                    source_path: file.path.clone(),
                    source_line: 1,
                    edge_kind: "cartridge_membership".to_string(),
                    confidence: 100,
                },
            });
        }
        // deploy_target: — file is the manifest of a deploy target.
        for target in &index.deploy_targets {
            if target.path == file.path {
                out.push(ProvenancedIncidence {
                    object: file.path.clone(),
                    attribute: format!("deploy_target:{}", target.name),
                    prov: StreamProvenance {
                        source_path: target.path.clone(),
                        source_line: 1,
                        edge_kind: "deploy_target_declaration".to_string(),
                        confidence: 100,
                    },
                });
            }
        }
    }
    out
}

fn pairs_for_cartridge(index: &RepoIndex) -> Vec<ProvenancedIncidence> {
    let mut out = Vec::new();
    for file in &index.files {
        let Some(cartridge) = cartridge_from_path(&file.path) else {
            continue;
        };
        for env in &file.env_vars {
            out.push(ProvenancedIncidence {
                object: cartridge.clone(),
                attribute: format!("env:{}", env.name),
                prov: StreamProvenance {
                    source_path: env.path.clone(),
                    source_line: env.line as u32,
                    edge_kind: "env_var_access".to_string(),
                    confidence: 90,
                },
            });
        }
        for key in &file.redis_keys {
            out.push(ProvenancedIncidence {
                object: cartridge.clone(),
                attribute: format!("redis:{}", key.key),
                prov: StreamProvenance {
                    source_path: key.path.clone(),
                    source_line: key.line as u32,
                    edge_kind: "redis_access".to_string(),
                    confidence: 90,
                },
            });
        }
        for spawn in &index.cross_language.resolved_spawns {
            if spawn.caller_path == file.path {
                out.push(ProvenancedIncidence {
                    object: cartridge.clone(),
                    attribute: format!("binary:{}", spawn.callee_name),
                    prov: StreamProvenance {
                        source_path: spawn.caller_path.clone(),
                        source_line: spawn.caller_line as u32,
                        edge_kind: "binary_spawn".to_string(),
                        confidence: spawn.confidence,
                    },
                });
            }
        }
        for route in &index.cross_language.routes {
            if route.path == file.path {
                out.push(ProvenancedIncidence {
                    object: cartridge.clone(),
                    attribute: format!("route:{}", route.route),
                    prov: StreamProvenance {
                        source_path: route.path.clone(),
                        source_line: route.line as u32,
                        edge_kind: "route_declaration".to_string(),
                        confidence: 95,
                    },
                });
            }
        }
    }
    out
}

fn pairs_for_binary(index: &RepoIndex) -> Vec<ProvenancedIncidence> {
    let mut out = Vec::new();
    // Declarations: emitted once per binary so isolated binaries still surface
    // as objects in the projection.
    for bin in &index.cross_language.binaries {
        out.push(ProvenancedIncidence {
            object: bin.name.clone(),
            attribute: format!("declared_in:{}", bin.path),
            prov: StreamProvenance {
                source_path: bin.path.clone(),
                source_line: 1,
                edge_kind: "binary_declaration".to_string(),
                confidence: 100,
            },
        });
    }
    // Callers: every resolved spawn edge adds two attributes (caller file +
    // caller language) to the callee binary.
    for spawn in &index.cross_language.resolved_spawns {
        out.push(ProvenancedIncidence {
            object: spawn.callee_name.clone(),
            attribute: format!("caller_file:{}", spawn.caller_path),
            prov: StreamProvenance {
                source_path: spawn.caller_path.clone(),
                source_line: spawn.caller_line as u32,
                edge_kind: "binary_spawn".to_string(),
                confidence: spawn.confidence,
            },
        });
        out.push(ProvenancedIncidence {
            object: spawn.callee_name.clone(),
            attribute: format!("caller_lang:{}", spawn.caller_language.as_str()),
            prov: StreamProvenance {
                source_path: spawn.caller_path.clone(),
                source_line: spawn.caller_line as u32,
                edge_kind: "binary_spawn".to_string(),
                confidence: spawn.confidence,
            },
        });
    }
    out
}

fn pairs_for_route(index: &RepoIndex) -> Vec<ProvenancedIncidence> {
    let mut out = Vec::new();
    for route in &index.cross_language.routes {
        // framework + method, anchored on the declaration line.
        out.push(ProvenancedIncidence {
            object: route.route.clone(),
            attribute: format!("framework:{}", route.framework),
            prov: StreamProvenance {
                source_path: route.path.clone(),
                source_line: route.line as u32,
                edge_kind: "route_declaration".to_string(),
                confidence: 95,
            },
        });
        out.push(ProvenancedIncidence {
            object: route.route.clone(),
            attribute: format!("method:{}", route.method),
            prov: StreamProvenance {
                source_path: route.path.clone(),
                source_line: route.line as u32,
                edge_kind: "route_declaration".to_string(),
                confidence: 95,
            },
        });
    }
    // caller_file: from resolved HTTP edges.
    for edge in &index.cross_language.resolved_http_edges {
        out.push(ProvenancedIncidence {
            object: edge.route_path.clone(),
            attribute: format!("caller_file:{}", edge.caller_path),
            prov: StreamProvenance {
                source_path: edge.caller_path.clone(),
                source_line: edge.caller_line as u32,
                edge_kind: "http_call".to_string(),
                confidence: edge.confidence,
            },
        });
    }
    out
}

fn pairs_for_env_var(index: &RepoIndex) -> Vec<ProvenancedIncidence> {
    let mut out = Vec::new();

    // file: + cartridge: — env vars observed in source files.
    for file in &index.files {
        for env in &file.env_vars {
            out.push(ProvenancedIncidence {
                object: env.name.clone(),
                attribute: format!("file:{}", env.path),
                prov: StreamProvenance {
                    source_path: env.path.clone(),
                    source_line: env.line as u32,
                    edge_kind: "env_var_access".to_string(),
                    confidence: 90,
                },
            });
            if let Some(cartridge) = cartridge_from_path(&env.path) {
                out.push(ProvenancedIncidence {
                    object: env.name.clone(),
                    attribute: format!("cartridge:{cartridge}"),
                    prov: StreamProvenance {
                        source_path: env.path.clone(),
                        source_line: env.line as u32,
                        edge_kind: "env_var_access".to_string(),
                        confidence: 90,
                    },
                });
            }
        }
    }

    // deploy_target: — env var declared in the backend_profile (or the
    // secret_set) of a deploy target. We can't go through `file.env_vars`
    // because deploy-target TOMLs don't go through the source-file env
    // extractor; the structural link is via the named profile / secret_set.
    for target in &index.deploy_targets {
        if let Some(profile_name) = &target.backend_profile
            && let Some(profile) = index.profiles.iter().find(|p| {
                p.name == *profile_name || p.name.starts_with(&format!("{profile_name}."))
            })
        {
            for var in &profile.vars {
                out.push(ProvenancedIncidence {
                    object: var.name.clone(),
                    attribute: format!("deploy_target:{}", target.name),
                    prov: StreamProvenance {
                        source_path: profile.path.clone(),
                        source_line: 1,
                        edge_kind: "deploy_target_declaration".to_string(),
                        confidence: 90,
                    },
                });
            }
        }
        if let Some(secret_name) = &target.secret_set
            && let Some(secret_set) = index
                .secret_sets
                .iter()
                .find(|s| s.name == *secret_name || s.name.starts_with(&format!("{secret_name}.")))
        {
            for var in &secret_set.vars {
                out.push(ProvenancedIncidence {
                    object: var.name.clone(),
                    attribute: format!("deploy_target:{}", target.name),
                    prov: StreamProvenance {
                        source_path: secret_set.path.clone(),
                        source_line: 1,
                        edge_kind: "secret_declaration".to_string(),
                        confidence: 90,
                    },
                });
            }
        }
    }

    // is_secret:true — env names declared in any secret set.
    for secret_set in &index.secret_sets {
        for var in &secret_set.vars {
            out.push(ProvenancedIncidence {
                object: var.name.clone(),
                attribute: "is_secret:true".to_string(),
                prov: StreamProvenance {
                    source_path: secret_set.path.clone(),
                    source_line: 1,
                    edge_kind: "secret_declaration".to_string(),
                    confidence: 100,
                },
            });
        }
    }

    out
}

fn pairs_for_deploy_target(index: &RepoIndex) -> Vec<ProvenancedIncidence> {
    let mut out = Vec::new();
    for target in &index.deploy_targets {
        if let Some(profile) = &target.backend_profile {
            out.push(ProvenancedIncidence {
                object: target.name.clone(),
                attribute: format!("profile:{profile}"),
                prov: StreamProvenance {
                    source_path: target.path.clone(),
                    source_line: 1,
                    edge_kind: "deploy_target_declaration".to_string(),
                    confidence: 100,
                },
            });
            // env: — env vars declared by the linked profile.
            if let Some(profile_rec) = index
                .profiles
                .iter()
                .find(|p| p.name == *profile || p.name.starts_with(&format!("{profile}.")))
            {
                for var in &profile_rec.vars {
                    out.push(ProvenancedIncidence {
                        object: target.name.clone(),
                        attribute: format!("env:{}", var.name),
                        prov: StreamProvenance {
                            source_path: profile_rec.path.clone(),
                            source_line: 1,
                            edge_kind: "deploy_target_declaration".to_string(),
                            confidence: 90,
                        },
                    });
                }
            }
        }
        if let Some(secret_set_name) = &target.secret_set
            && let Some(secret_rec) = index.secret_sets.iter().find(|s| {
                s.name == *secret_set_name || s.name.starts_with(&format!("{secret_set_name}."))
            })
        {
            for var in &secret_rec.vars {
                out.push(ProvenancedIncidence {
                    object: target.name.clone(),
                    attribute: format!("env:{}", var.name),
                    prov: StreamProvenance {
                        source_path: secret_rec.path.clone(),
                        source_line: 1,
                        edge_kind: "secret_declaration".to_string(),
                        confidence: 90,
                    },
                });
            }
        }
    }

    // binary: — every binary whose declaration file is inside any cartridge
    // declared by this deploy target. Use the cartridge membership as the
    // bridge from binary -> deploy_target.
    for target in &index.deploy_targets {
        for bin in &index.cross_language.binaries {
            let Some(cartridge) = cartridge_from_path(&bin.path) else {
                continue;
            };
            if !target.cartridges.iter().any(|c| c == &cartridge) {
                continue;
            }
            out.push(ProvenancedIncidence {
                object: target.name.clone(),
                attribute: format!("binary:{}", bin.name),
                prov: StreamProvenance {
                    source_path: bin.path.clone(),
                    source_line: 1,
                    edge_kind: "binary_declaration".to_string(),
                    confidence: 90,
                },
            });
        }
        for route in &index.cross_language.routes {
            let Some(cartridge) = cartridge_from_path(&route.path) else {
                continue;
            };
            if !target.cartridges.iter().any(|c| c == &cartridge) {
                continue;
            }
            out.push(ProvenancedIncidence {
                object: target.name.clone(),
                attribute: format!("route:{}", route.route),
                prov: StreamProvenance {
                    source_path: route.path.clone(),
                    source_line: route.line as u32,
                    edge_kind: "route_declaration".to_string(),
                    confidence: 90,
                },
            });
        }
    }
    out
}

/// Extract the cartridge name from a repo-relative path, if any.
/// Mirrors the recognizer used in `add_cartridge_membership` inside the
/// legacy builder (`cartridges/<name>/...`).
fn cartridge_from_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix("cartridges/")?;
    let mut segments = rest.split('/');
    let name = segments.next()?;
    if name.is_empty() || name.contains('.') {
        return None;
    }
    Some(name.to_string())
}

/// Read back an Arrow-IPC formal-context stream into `(object, attribute,
/// provenance)` triples. Used by integration tests and downstream consumers
/// that prefer the columnar shape.
pub fn read_formal_context_stream_arrow(path: &Path) -> Result<Vec<ProvenancedIncidenceRead>> {
    use arrow_array::{StringArray, StructArray, UInt8Array, UInt32Array};

    let batches = crate::arrow_ipc::read_ipc_stream_path(path).with_context(|| {
        format!(
            "failed to open formal-context Arrow IPC stream {}",
            path.display()
        )
    })?;
    let mut out = Vec::new();
    for batch in batches {
        let objects = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .context("column 0 was not Utf8 object")?;
        let attributes = batch
            .column(1)
            .as_any()
            .downcast_ref::<StringArray>()
            .context("column 1 was not Utf8 attribute")?;
        let provenance = batch
            .column(2)
            .as_any()
            .downcast_ref::<StructArray>()
            .context("column 2 was not Struct<provenance>")?;
        let src_path = provenance
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .context("provenance.source_path was not Utf8")?;
        let src_line = provenance
            .column(1)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .context("provenance.source_line was not UInt32")?;
        let edge_kind = provenance
            .column(2)
            .as_any()
            .downcast_ref::<StringArray>()
            .context("provenance.edge_kind was not Utf8")?;
        let confidence = provenance
            .column(3)
            .as_any()
            .downcast_ref::<UInt8Array>()
            .context("provenance.confidence was not UInt8")?;
        for row in 0..batch.num_rows() {
            out.push(ProvenancedIncidenceRead {
                object: objects.value(row).to_string(),
                attribute: attributes.value(row).to_string(),
                source_path: src_path.value(row).to_string(),
                source_line: src_line.value(row),
                edge_kind: edge_kind.value(row).to_string(),
                confidence: confidence.value(row),
            });
        }
    }
    Ok(out)
}

/// Public mirror of [`ProvenancedIncidence`] for callers that read the Arrow
/// stream back (e.g. integration tests, the Phase B example-platform
/// adapter).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenancedIncidenceRead {
    pub object: String,
    pub attribute: String,
    pub source_path: String,
    pub source_line: u32,
    pub edge_kind: String,
    pub confidence: u8,
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use super::{
        build_formal_context, default_arrow_nodes_output_dir, env_attribute_label,
        export_arrow_nodes, export_hypergraph, read_arrow_node_batches, record_batch_node_ids,
        record_batch_to_node_entities, redis_attribute_label,
    };
    use crate::model::{
        AccessKind, DeployTargetRecord, EnvVarOccurrence, FileRecord, ProfileRecord,
        RedisKeyOccurrence, RepoIndex, SecretSetRecord, SourceLanguage, SymbolKind,
        SymbolOccurrence,
    };

    fn temp_repo_root() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("leio-code-export-tests-{nanos}"));
        fs::create_dir_all(&path).expect("create temp root");
        path
    }

    #[test]
    fn formal_context_captures_auth_session_deploy_traits() {
        let index = RepoIndex {
            version: 1,
            root: "/tmp/repo".to_string(),
            indexed_at: "2026-03-27T00:00:00Z".to_string(),
            files: vec![FileRecord {
                path: "example-gateway/src/auth/session.rs".to_string(),
                language: SourceLanguage::Rust,
                bytes: 128,
                modified_unix_ms: 0,
                symbols: vec![SymbolOccurrence {
                    name: "AuthSessionManager".to_string(),
                    kind: SymbolKind::Struct,
                    path: "example-gateway/src/auth/session.rs".to_string(),
                    line: 12,
                    language: SourceLanguage::Rust,
                    qual_name: None,
                }],
                env_vars: vec![EnvVarOccurrence {
                    name: "JWT_PUBLIC_KEY".to_string(),
                    access: AccessKind::Read,
                    path: "example-gateway/src/auth/session.rs".to_string(),
                    line: 3,
                    language: SourceLanguage::Rust,
                }],
                redis_keys: vec![RedisKeyOccurrence {
                    key: "session:{phone}:{sender}".to_string(),
                    access: AccessKind::Write,
                    path: "example-gateway/src/auth/session.rs".to_string(),
                    line: 9,
                    language: SourceLanguage::Rust,
                }],
                subprocess_calls: Vec::new(),
                http_calls: Vec::new(),
                unresolved_edges: Vec::new(),
            }],
            deploy_targets: vec![DeployTargetRecord {
                name: "health_audit".to_string(),
                path: "deploy/targets/health_audit.toml".to_string(),
                profile: Some("health_audit".to_string()),
                readiness_target: None,
                deploy_class: Some("shared_family_node".to_string()),
                topology: Some("standalone_solution".to_string()),
                ui_role: Some("autopilot".to_string()),
                ui_path: None,
                frontend_project: Some("health-audit-console".to_string()),
                backend_profile: Some("hospital_data".to_string()),
                secret_set: Some("hospital_data".to_string()),
                health_checks: vec!["http://localhost:8000/health".to_string()],
                smoke_suite: Some("./scripts/compose_smoke.sh health_audit".to_string()),
                rollback_command: Some(
                    "./scripts/pull-and-restart.sh --target health_audit".to_string(),
                ),
                cartridges: vec!["health_audit".to_string()],
                required_integrations: vec!["hospital_db".to_string()],
                promotion_policy: None,
            }],
            profiles: vec![ProfileRecord {
                name: "hospital_data.env".to_string(),
                path: "deploy/profiles/hospital_data.env".to_string(),
                vars: vec![crate::model::DeclaredVar {
                    name: "JWT_PUBLIC_KEY".to_string(),
                    value_preview: None,
                    raw_value: None,
                }],
            }],
            secret_sets: vec![SecretSetRecord {
                name: "hospital_data.env.example".to_string(),
                path: "deploy/secret-sets/hospital_data.env.example".to_string(),
                vars: vec![crate::model::DeclaredVar {
                    name: "DB_PASSWORD".to_string(),
                    value_preview: None,
                    raw_value: None,
                }],
            }],
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let context = build_formal_context(&index);

        assert!(
            context
                .objects
                .contains_key("file:example-gateway/src/auth/session.rs")
        );
        assert!(context.objects.contains_key("deploy_target:health_audit"));
        assert!(context.objects.contains_key("env_var:JWT_PUBLIC_KEY"));
        assert!(
            context
                .objects
                .contains_key("redis_key:session:{phone}:{sender}")
        );

        let env_attr = format!(
            "attr:{}",
            env_attribute_label(AccessKind::Read, "JWT_PUBLIC_KEY")
        );
        let redis_attr = format!(
            "attr:{}",
            redis_attribute_label(AccessKind::Write, "session:{phone}:{sender}")
        );
        assert!(context.attributes.contains_key(&env_attr));
        assert!(context.attributes.contains_key(&redis_attr));
        assert!(context.attributes.contains_key("attr:topic:auth"));
        assert!(context.attributes.contains_key("attr:topic:session"));
        assert!(
            context
                .attributes
                .contains_key("attr:family:hospital_audit")
        );
        assert!(context.incidences.contains(&(
            "deploy_target:health_audit".to_string(),
            "attr:dependsOn:hospital_db".to_string()
        )));
    }

    #[test]
    fn hypergraph_export_matches_formal_context_counts() {
        let index = RepoIndex {
            version: 1,
            root: "/tmp/repo".to_string(),
            indexed_at: "2026-03-27T00:00:00Z".to_string(),
            files: vec![FileRecord {
                path: "src/lib.rs".to_string(),
                language: SourceLanguage::Rust,
                bytes: 10,
                modified_unix_ms: 0,
                symbols: vec![],
                env_vars: vec![],
                redis_keys: vec![],
                subprocess_calls: Vec::new(),
                http_calls: Vec::new(),
                unresolved_edges: Vec::new(),
            }],
            deploy_targets: vec![],
            profiles: vec![],
            secret_sets: vec![],
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };
        let root = temp_repo_root();
        let out = root.join("hyper_out");
        export_hypergraph(&index, &root, &out).expect("hypergraph export");
        let raw = fs::read_to_string(out.join("hypergraph.json")).expect("read hypergraph.json");
        let v: serde_json::Value = serde_json::from_str(&raw).expect("parse hypergraph json");
        assert_eq!(v["schema"], json!("example.formal_context_hypergraph.v1"));
        assert!(v.get("views").is_some());
        assert!(v.pointer("/semantic_tooling/fca_fast").is_some());
        assert!(
            v.pointer("/semantic_tooling/workspace_wheelhouse")
                .is_some()
        );
        let ctx = build_formal_context(&index);
        assert_eq!(
            v["counts"]["objects"].as_u64().unwrap() as usize,
            ctx.objects.len()
        );
        assert_eq!(
            v["counts"]["incidences"].as_u64().unwrap() as usize,
            ctx.incidences.len()
        );
    }

    #[test]
    fn formal_context_captures_cartridge_route_and_docker_service_objects() {
        let root = temp_repo_root();
        fs::create_dir_all(root.join("cartridges/revops")).expect("create cartridge dir");
        fs::create_dir_all(root.join("infra")).expect("create infra dir");
        fs::write(
            root.join("cartridges/revops/router.py"),
            r#"
from fastapi import APIRouter

router = APIRouter(prefix="/v2/revops")

@router.get("/health")
async def revops_health():
    return {"ok": True}
"#,
        )
        .expect("write router");
        fs::write(
            root.join("infra/docker-compose.yml"),
            r#"
services:
  api:
    image: example-api:latest
    profiles: ["core"]
"#,
        )
        .expect("write compose");

        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "2026-04-05T00:00:00Z".to_string(),
            files: vec![
                FileRecord {
                    path: "cartridges/revops/router.py".to_string(),
                    language: SourceLanguage::Python,
                    bytes: 0,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
                FileRecord {
                    path: "infra/docker-compose.yml".to_string(),
                    language: SourceLanguage::Yaml,
                    bytes: 0,
                    modified_unix_ms: 0,
                    symbols: Vec::new(),
                    env_vars: Vec::new(),
                    redis_keys: Vec::new(),
                    subprocess_calls: Vec::new(),
                    http_calls: Vec::new(),
                    unresolved_edges: Vec::new(),
                },
            ],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let context = build_formal_context(&index);
        assert!(context.objects.contains_key("cartridge:revops"));
        assert!(
            context
                .objects
                .contains_key("api_route:cartridges/revops/router.py:6:primary:/v2/revops/health")
        );
        assert!(
            context
                .objects
                .contains_key("docker_service:infra/docker-compose.yml:api")
        );
        assert!(context.attributes.contains_key("attr:kind:cartridge"));
        assert!(context.attributes.contains_key("attr:kind:api_route"));
        assert!(context.attributes.contains_key("attr:kind:docker_service"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn arrow_node_export_round_trips_arrow_batches() {
        let root = temp_repo_root();
        let index = RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "2026-04-05T00:00:00Z".to_string(),
            files: vec![FileRecord {
                path: "cartridges/revops/router.py".to_string(),
                language: SourceLanguage::Python,
                bytes: 42,
                modified_unix_ms: 7,
                symbols: vec![SymbolOccurrence {
                    name: "revops_health".to_string(),
                    kind: SymbolKind::Function,
                    path: "cartridges/revops/router.py".to_string(),
                    line: 6,
                    language: SourceLanguage::Python,
                    qual_name: None,
                }],
                env_vars: Vec::new(),
                redis_keys: Vec::new(),
                subprocess_calls: Vec::new(),
                http_calls: Vec::new(),
                unresolved_edges: Vec::new(),
            }],
            deploy_targets: Vec::new(),
            profiles: Vec::new(),
            secret_sets: Vec::new(),
            env_files: Vec::new(),
            cross_language: Default::default(),
            k8s_configmaps: Vec::new(),
        };

        let output_dir = default_arrow_nodes_output_dir(&root);
        let envelope = export_arrow_nodes(&index, &root, &output_dir).expect("export nodes");
        assert!(envelope.summary.contains("exported"));

        let batches = read_arrow_node_batches(&output_dir).expect("read node batches");
        let ids = record_batch_node_ids(&batches[0]).expect("extract node ids");
        assert!(!ids.is_empty());

        let entities = record_batch_to_node_entities(&batches[0]).expect("decode node batch");
        assert!(
            entities.iter().any(|entity| {
                entity.get("kind").and_then(|value| value.as_str()) == Some("file")
            })
        );
        assert!(entities.iter().any(|entity| {
            entity.get("kind").and_then(|value| value.as_str()) == Some("symbol")
        }));

        let _ = fs::remove_dir_all(root);
    }
}
