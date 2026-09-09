//! Persist the concept lattice as a category and an OWL TBox.
//!
//! Cover edges are `subClassOf` morphisms. Two functors are checked:
//! the identity of the cover category onto itself, and the wiki heading
//! stack (`Child --subClassOf--> Parent` in one file) into the lattice.
//! OWL is written as Turtle so owl-fast / example-align can parse it later;
//! this crate does not vendor owl-fast-core.
// Rust guideline compliant 2026-02-21

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::fca::{self, InducedLattice, MembershipOptions};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::category::{Alignment, LatticeCategory, Morphism, evaluate_functor};
use crate::model::RepoIndex;

/// Artifact schema written next to `formal-context-v1`.
/// v3 adds the wiki heading category and its functor into the lattice.
const LATTICE_SCHEMA: &str = "leio.concept_lattice.v3";

/// One concept in the persisted lattice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatticeConcept {
    pub id: String,
    pub label: String,
    pub family: String,
    pub intent: Vec<String>,
    pub extent_size: usize,
    pub parents: Vec<String>,
    pub children: Vec<String>,
}

/// On-disk lattice + functor witnesses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatticeArtifact {
    pub schema: String,
    pub category: LatticeCategory,
    pub concepts: Vec<LatticeConcept>,
    pub memberships: BTreeMap<String, StoredMembership>,
    pub functor: FunctorWitness,
    #[serde(default)]
    pub heading_category: LatticeCategory,
    #[serde(default)]
    pub heading_objects: Vec<HeadingObject>,
    #[serde(default)]
    pub heading_alignment: Vec<Alignment>,
    #[serde(default)]
    pub heading_functor: FunctorWitness,
    /// Older artifacts remain usable, but cannot claim verified freshness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<LatticeProvenance>,
}

/// Inputs used for this lattice, separate from when its index was refreshed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatticeProvenance {
    pub fingerprint: String,
    pub generated_at: String,
    pub pair_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_version: Option<u32>,
}

const FINGERPRINT_PREFIX: &str = "sha256:structural-incidence-v1:";

/// Hash canonical structural pairs in one pass, without lattice enumeration.
/// The formal-context builder emits unique, sorted pairs from its BTreeSet.
/// Length prefixes keep arbitrary path/attribute text unambiguous.
fn incidence_fingerprint(pairs: &[(String, String)]) -> String {
    let mut hash = Sha256::new();
    hash.update(FINGERPRINT_PREFIX.as_bytes());
    for (object, attribute) in pairs {
        for value in [object, attribute] {
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value.as_bytes());
        }
    }
    format!("{FINGERPRINT_PREFIX}{:x}", hash.finalize())
}

fn provenance_for_pairs(pairs: &[(String, String)]) -> LatticeProvenance {
    LatticeProvenance {
        fingerprint: incidence_fingerprint(pairs),
        generated_at: time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .expect("UTC timestamp is RFC3339 representable"),
        pair_count: pairs.len(),
        source_root: None,
        index_root: None,
        indexed_at: None,
        index_version: None,
    }
}

fn normalized_root(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Attach repository identity only after inducing from that index's pairs.
pub(crate) fn attach_source_provenance(
    artifact: &mut LatticeArtifact,
    index: &RepoIndex,
    repo_root: &Path,
) {
    if let Some(provenance) = artifact.provenance.as_mut() {
        provenance.source_root = Some(normalized_root(repo_root));
        provenance.index_root = Some(normalized_root(Path::new(&index.root)));
        provenance.indexed_at = Some(index.indexed_at.clone());
        provenance.index_version = Some(index.version);
    }
}

/// Report lattice readiness without inducing, modifying, or deleting artifacts.
/// Freshness covers indexed structural incidences plus the live wiki headings;
/// a timestamp-only reindex does not invalidate an unchanged lattice.
pub fn lattice_readiness(index: &RepoIndex, repo_root: &Path) -> Value {
    let path = lattice_path(repo_root);
    let mut result = json!({
        "state": "missing",
        "artifact_path": path,
        "reason": "No persisted concept lattice; export formal-context to build it.",
        "rebuild_required": true,
        "current_indexed_at": index.indexed_at,
        "current_index_version": index.version,
    });
    let set_state = |result: &mut Value, state: &str, reason: String| {
        result["state"] = json!(state);
        result["reason"] = json!(reason);
        result["rebuild_required"] = json!(state != "current");
    };
    let current_root = normalized_root(repo_root);
    if normalized_root(Path::new(&index.root)) != current_root {
        set_state(&mut result, "invalid", "Index belongs to another repository; use an index for this repo_root before rebuilding.".into());
        result["rebuild_required"] = json!(false);
        return result;
    }
    let raw = match fs::read(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return result,
        Err(error) => {
            set_state(
                &mut result,
                "invalid",
                format!("Cannot read lattice artifact: {error}"),
            );
            return result;
        }
    };
    let artifact: LatticeArtifact = match serde_json::from_slice(&raw) {
        Ok(artifact) => artifact,
        Err(error) => {
            set_state(
                &mut result,
                "invalid",
                format!("Cannot parse lattice artifact: {error}"),
            );
            return result;
        }
    };
    if artifact.schema != LATTICE_SCHEMA {
        set_state(
            &mut result,
            "invalid",
            format!("Unsupported lattice schema: {}", artifact.schema),
        );
        return result;
    }
    let Some(provenance) = artifact.provenance else {
        set_state(
            &mut result,
            "unverified",
            "Legacy lattice has no input fingerprint; rebuild explicitly to verify freshness."
                .into(),
        );
        return result;
    };
    result["generated_at"] = json!(provenance.generated_at);
    result["indexed_at"] = json!(provenance.indexed_at);
    result["index_version"] = json!(provenance.index_version);
    result["source_root"] = json!(provenance.source_root);
    result["fingerprint"] = json!(provenance.fingerprint);
    result["pair_count"] = json!(provenance.pair_count);
    if !provenance.fingerprint.starts_with(FINGERPRINT_PREFIX) {
        set_state(
            &mut result,
            "unverified",
            "Lattice fingerprint algorithm is not recognized; rebuild to verify freshness.".into(),
        );
        return result;
    }
    let (Some(source_root), Some(index_root)) = (&provenance.source_root, &provenance.index_root)
    else {
        set_state(
            &mut result,
            "unverified",
            "Lattice inputs have no repository identity; rebuild to verify freshness.".into(),
        );
        return result;
    };
    if normalized_root(Path::new(source_root)) != current_root
        || normalized_root(Path::new(index_root)) != current_root
    {
        set_state(
            &mut result,
            "stale",
            "Lattice was generated for another repository; rebuild from this repository's index."
                .into(),
        );
        return result;
    }
    let pairs = crate::export::structural_fca_pairs_at(index, repo_root);
    let fingerprint = incidence_fingerprint(&pairs);
    result["current_fingerprint"] = json!(fingerprint);
    if fingerprint != provenance.fingerprint || pairs.len() != provenance.pair_count {
        set_state(&mut result, "stale", "Indexed structural incidences or wiki headings changed since lattice generation; rebuild explicitly.".into());
    } else {
        set_state(
            &mut result,
            "current",
            "Lattice matches the indexed structural incidences and current wiki headings.".into(),
        );
    }
    result
}

/// Wiki heading as an object in the heading category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadingObject {
    pub id: String,
    pub path: String,
    pub title: String,
    pub heading_path: String,
    pub line: u32,
}

/// Primary membership persisted for nav/align.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMembership {
    pub concept_id: String,
    pub label: String,
    pub family: String,
}

/// Identity-functor check of the cover category into the OWL TBox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctorWitness {
    pub name: String,
    pub coherence: f32,
    pub preserved: usize,
    pub total: usize,
}

impl Default for FunctorWitness {
    fn default() -> Self {
        Self {
            name: String::new(),
            coherence: 1.0,
            preserved: 0,
            total: 0,
        }
    }
}

/// Default lattice.json path under the formal-context export dir.
pub fn lattice_path(repo_root: &Path) -> PathBuf {
    crate::export::default_formal_context_output_dir(repo_root).join("lattice.json")
}

/// Default induced OWL Turtle path.
pub fn owl_path(repo_root: &Path) -> PathBuf {
    crate::export::default_formal_context_output_dir(repo_root).join("induced.ttl")
}

/// Induce the structural lattice from incidence pairs via the pre-built
/// `fca_fast` parser wheel (`crate::fca`).
///
/// The exponential concept enumeration runs inside the wheel; leio-code only
/// reshapes the wheel's concepts + cover links into the persisted artifact.
pub fn induce_from_pairs(pairs: &[(String, String)]) -> Result<LatticeArtifact> {
    let mut artifact = if pairs.is_empty() {
        empty_artifact()
    } else {
        let induced: InducedLattice = fca::induct(pairs).map_err(anyhow::Error::msg)?;
        build_artifact(induced)?
    };
    artifact.provenance = Some(provenance_for_pairs(pairs));
    Ok(artifact)
}

/// Build the persisted artifact from an induced wheel lattice.
fn build_artifact(induced: InducedLattice) -> Result<LatticeArtifact> {
    let opts = MembershipOptions::default();
    let total_objects = induced.object_count;
    let by_node: BTreeMap<usize, &fca::WheelConcept> = induced
        .concepts
        .iter()
        .map(|concept| (concept.node_id, concept))
        .collect();

    let mut concepts_by_id: BTreeMap<String, LatticeConcept> = BTreeMap::new();
    for concept in &induced.concepts {
        if opts.skip_universal_top && concept.extent_size == total_objects {
            continue;
        }
        if opts.skip_universal_bottom && concept.extent_size == 0 {
            continue;
        }
        let intent = concept.intent.clone();
        let id = fca::concept_id(&concept.label, concept.node_id);
        concepts_by_id.insert(
            id.clone(),
            LatticeConcept {
                id,
                label: concept.label.clone(),
                family: fca::derive_family(&intent),
                intent,
                extent_size: concept.extent_size,
                parents: Vec::new(),
                children: Vec::new(),
            },
        );
    }

    let mut morphisms = Vec::new();
    for link in &induced.links {
        let (Some(child), Some(parent)) = (by_node.get(&link.source), by_node.get(&link.target))
        else {
            continue;
        };
        let child_id = fca::concept_id(&child.label, child.node_id);
        let parent_id = fca::concept_id(&parent.label, parent.node_id);
        let Some((specific, general)) = orient_cover(&concepts_by_id, &child_id, &parent_id) else {
            continue;
        };
        if let Some(node) = concepts_by_id.get_mut(&specific) {
            node.parents.push(general.clone());
        }
        if let Some(node) = concepts_by_id.get_mut(&general) {
            node.children.push(specific.clone());
        }
        morphisms.push(Morphism {
            source: specific,
            target: general,
            label: "subClassOf".to_string(),
        });
    }

    for concept in concepts_by_id.values_mut() {
        concept.parents.sort();
        concept.parents.dedup();
        concept.children.sort();
        concept.children.dedup();
    }
    let ranks: BTreeMap<String, (usize, usize)> = concepts_by_id
        .iter()
        .map(|(id, concept)| (id.clone(), (concept.intent.len(), concept.extent_size)))
        .collect();
    for concept in concepts_by_id.values_mut() {
        sort_ids_by_rank(&ranks, &mut concept.parents, true);
        sort_ids_by_rank(&ranks, &mut concept.children, false);
    }

    let objects: Vec<String> = concepts_by_id.keys().cloned().collect();
    let category = LatticeCategory { objects, morphisms };
    let identity: Vec<Alignment> = category
        .objects
        .iter()
        .map(|id| Alignment {
            source_uri: id.clone(),
            target_uri: id.clone(),
        })
        .collect();
    let stats = evaluate_functor(&category, &category, &identity);

    let stored = fca::derive_memberships(&induced, opts)
        .into_iter()
        .map(|(object, membership)| {
            (
                object,
                StoredMembership {
                    concept_id: membership.primary_concept_id,
                    label: membership.primary_concept_label,
                    family: membership.primary_concept_family,
                },
            )
        })
        .collect();

    Ok(LatticeArtifact {
        provenance: None,
        schema: LATTICE_SCHEMA.to_string(),
        category,
        concepts: concepts_by_id.into_values().collect(),
        memberships: stored,
        functor: FunctorWitness {
            name: "identity_cover_to_owl".to_string(),
            coherence: stats.coherence,
            preserved: stats.preserved_count,
            total: stats.total_count,
        },
        heading_category: LatticeCategory::default(),
        heading_objects: Vec::new(),
        heading_alignment: Vec::new(),
        heading_functor: FunctorWitness {
            name: "wiki_heading_to_lattice".to_string(),
            coherence: 1.0,
            preserved: 0,
            total: 0,
        },
    })
}

/// Write lattice.json and induced.ttl under `dir`.
pub fn write_artifacts(dir: &Path, artifact: &LatticeArtifact) -> Result<(PathBuf, PathBuf)> {
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let lattice = dir.join("lattice.json");
    let owl = dir.join("induced.ttl");
    crate::sidecar::write_atomic(
        &lattice,
        &serde_json::to_vec_pretty(artifact).context("serialize lattice")?,
    )
    .with_context(|| format!("write {}", lattice.display()))?;
    crate::sidecar::write_atomic(&owl, render_owl(artifact).as_bytes())
        .with_context(|| format!("write {}", owl.display()))?;
    Ok((lattice, owl))
}

/// Load a previously written lattice, if present.
pub fn load_lattice(path: &Path) -> Result<Option<LatticeArtifact>> {
    if !path.is_file() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let artifact: LatticeArtifact =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    if artifact.schema != LATTICE_SCHEMA {
        return Ok(None);
    }
    Ok(Some(artifact))
}

/// Load a previously written lattice, if present, without inducing one.
///
/// Nav uses this for opportunistic lattice features so an unbuilt lattice
/// never blocks symbol/file navigation with a full FCA induction.
pub fn try_load_lattice(repo_root: &Path) -> Result<Option<LatticeArtifact>> {
    load_lattice(&lattice_path(repo_root))
}

/// Default maximum objects for *on-demand* lattice induction.
///
/// Full concept enumeration (Ganter next-closure) is exponential in the
/// worst case; on large workspaces it runs for minutes without producing a
/// result. On-demand induction is therefore reserved for small contexts;
/// large ones must be built explicitly (or sliced) so the failure is
/// actionable instead of a hang.
const DEFAULT_ON_DEMAND_MAX_OBJECTS: usize = 5_000;
const DEFAULT_ON_DEMAND_MAX_PAIRS: usize = 100_000;

fn on_demand_limit(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

/// Induce the structural lattice from incidence pairs, bounded.
///
/// Refuses contexts above [`DEFAULT_ON_DEMAND_MAX_OBJECTS`] /
/// [`DEFAULT_ON_DEMAND_MAX_PAIRS`] (overridable via
/// `LEIO_LATTICE_MAX_ON_DEMAND_OBJECTS` / `LEIO_LATTICE_MAX_ON_DEMAND_PAIRS`)
/// instead of blocking for minutes on full concept enumeration.
pub fn try_induce_from_pairs(pairs: &[(String, String)]) -> Result<LatticeArtifact> {
    if pairs.is_empty() {
        return induce_from_pairs(pairs);
    }
    let max_objects = on_demand_limit(
        "LEIO_LATTICE_MAX_ON_DEMAND_OBJECTS",
        DEFAULT_ON_DEMAND_MAX_OBJECTS,
    );
    let max_pairs = on_demand_limit(
        "LEIO_LATTICE_MAX_ON_DEMAND_PAIRS",
        DEFAULT_ON_DEMAND_MAX_PAIRS,
    );
    let objects = pairs
        .iter()
        .map(|(object, _)| object.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    if objects > max_objects || pairs.len() > max_pairs {
        anyhow::bail!(
            "concept lattice induction skipped: formal context has {objects} objects / {} incidences \
             (on-demand limits: {max_objects} objects / {max_pairs} incidences). Build the lattice \
             explicitly with `leio-code --repo <root> export formal-context` for tractable contexts; \
             this export obeys the same limits. To allow a larger induction, explicitly raise \
             LEIO_LATTICE_MAX_ON_DEMAND_OBJECTS / LEIO_LATTICE_MAX_ON_DEMAND_PAIRS. \
             `leio-code export arrow-nodes` provides bounded navigation rows but does not rebuild lattice.json.",
            pairs.len()
        );
    }
    eprintln!(
        "inducing concept lattice from {} pairs across {objects} objects...",
        pairs.len()
    );
    induce_from_pairs(pairs)
}

/// Load lattice.json or induce from the live index and persist it.
///
/// Induction is bounded by [`try_induce_from_pairs`]; oversized contexts
/// fail with an actionable error instead of hanging on full concept
/// enumeration.
pub fn ensure_lattice(index: &RepoIndex, repo_root: &Path) -> Result<LatticeArtifact> {
    if let Some(existing) = try_load_lattice(repo_root)? {
        return Ok(existing);
    }
    let _lock = crate::sidecar::acquire_lock(repo_root, "lattice")?;
    if let Some(existing) = try_load_lattice(repo_root)? {
        return Ok(existing);
    }
    let pairs = crate::export::structural_fca_pairs_at(index, repo_root);
    let mut artifact = try_induce_from_pairs(&pairs)?;
    attach_source_provenance(&mut artifact, index, repo_root);
    attach_heading_functor(&mut artifact, repo_root);
    let dir = crate::export::default_formal_context_output_dir(repo_root);
    write_artifacts(&dir, &artifact)?;
    Ok(artifact)
}

/// Build the heading category from the local wiki and score it as a functor.
pub fn attach_heading_functor(artifact: &mut LatticeArtifact, repo_root: &Path) {
    let sections = crate::knowledge::wiki_section_incidences(repo_root);
    apply_heading_functor(artifact, &sections);
}

/// Align wiki heading stacks to primary concepts and evaluate the functor.
pub(crate) fn apply_heading_functor(
    artifact: &mut LatticeArtifact,
    sections: &[crate::knowledge::WikiSectionInc],
) {
    let heading_objects: Vec<HeadingObject> = sections
        .iter()
        .map(|section| HeadingObject {
            id: section.object_id(),
            path: section.source_path.clone(),
            title: section.title.clone(),
            heading_path: section.heading_path.clone(),
            line: section.line,
        })
        .collect();
    let heading_category = heading_category_from(&heading_objects);
    let heading_alignment: Vec<Alignment> = heading_objects
        .iter()
        .filter_map(|object| {
            let membership = artifact.memberships.get(&object.id)?;
            Some(Alignment {
                source_uri: object.id.clone(),
                target_uri: membership.concept_id.clone(),
            })
        })
        .collect();
    let stats = evaluate_functor(&heading_category, &artifact.category, &heading_alignment);
    artifact.heading_category = heading_category;
    artifact.heading_objects = heading_objects;
    artifact.heading_alignment = heading_alignment;
    artifact.heading_functor = FunctorWitness {
        name: "wiki_heading_to_lattice".to_string(),
        coherence: stats.coherence,
        preserved: stats.preserved_count,
        total: stats.total_count,
    };
}

/// Concept for a code path (`file:{path}`) or an explicit concept id.
pub fn concept_for<'a>(artifact: &'a LatticeArtifact, needle: &str) -> Option<&'a LatticeConcept> {
    let trimmed = needle.trim();
    if let Some(concept) = artifact.concepts.iter().find(|row| row.id == trimmed) {
        return Some(concept);
    }
    if let Some(concept) = artifact
        .concepts
        .iter()
        .find(|row| row.label.eq_ignore_ascii_case(trimmed))
    {
        return Some(concept);
    }
    if let Some(membership) = artifact.memberships.get(trimmed) {
        return artifact
            .concepts
            .iter()
            .find(|row| row.id == membership.concept_id);
    }
    if let Some(section_key) = section_membership_key(trimmed)
        && let Some(membership) = artifact.memberships.get(&section_key)
    {
        return artifact
            .concepts
            .iter()
            .find(|row| row.id == membership.concept_id);
    }
    if let Some(heading) = heading_for(artifact, trimmed)
        && let Some(membership) = artifact.memberships.get(&heading.id)
    {
        return artifact
            .concepts
            .iter()
            .find(|row| row.id == membership.concept_id);
    }
    let file_key = if trimmed.starts_with("file:") {
        trimmed.to_string()
    } else {
        format!("file:{trimmed}")
    };
    let membership = artifact.memberships.get(&file_key)?;
    artifact
        .concepts
        .iter()
        .find(|row| row.id == membership.concept_id)
}

/// Wiki heading object for a section id, `path#line`, or heading path.
pub fn heading_for<'a>(artifact: &'a LatticeArtifact, needle: &str) -> Option<&'a HeadingObject> {
    let trimmed = needle.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(object) = artifact
        .heading_objects
        .iter()
        .find(|row| row.id == trimmed)
    {
        return Some(object);
    }
    if let Some(key) = section_membership_key(trimmed)
        && let Some(object) = artifact.heading_objects.iter().find(|row| row.id == key)
    {
        return Some(object);
    }
    if let Some(object) = artifact
        .heading_objects
        .iter()
        .find(|row| row.heading_path.eq_ignore_ascii_case(trimmed))
    {
        return Some(object);
    }
    artifact
        .heading_objects
        .iter()
        .find(|row| row.title.eq_ignore_ascii_case(trimmed))
}

/// Neighbors in the heading category (document outline).
pub fn heading_walk(
    artifact: &LatticeArtifact,
    section_id: &str,
    direction: LatticeWalk,
    limit: usize,
) -> Vec<HeadingObject> {
    let mut ids = BTreeSet::new();
    match direction {
        LatticeWalk::Parent => {
            for morphism in &artifact.heading_category.morphisms {
                if morphism.source == section_id && morphism.label == "subClassOf" {
                    ids.insert(morphism.target.clone());
                }
            }
        }
        LatticeWalk::Child => {
            for morphism in &artifact.heading_category.morphisms {
                if morphism.target == section_id && morphism.label == "subClassOf" {
                    ids.insert(morphism.source.clone());
                }
            }
        }
        LatticeWalk::Peer => {
            let parents: Vec<String> = artifact
                .heading_category
                .morphisms
                .iter()
                .filter(|morphism| morphism.source == section_id && morphism.label == "subClassOf")
                .map(|morphism| morphism.target.clone())
                .collect();
            for parent in parents {
                for morphism in &artifact.heading_category.morphisms {
                    if morphism.target == parent
                        && morphism.source != section_id
                        && morphism.label == "subClassOf"
                    {
                        ids.insert(morphism.source.clone());
                    }
                }
            }
        }
    }
    ids.into_iter()
        .filter_map(|id| {
            artifact
                .heading_objects
                .iter()
                .find(|row| row.id == id)
                .cloned()
        })
        .take(limit.max(1))
        .collect()
}

/// Neighbors along one morphism label.
pub fn walk(
    artifact: &LatticeArtifact,
    concept_id: &str,
    direction: LatticeWalk,
    limit: usize,
) -> Vec<LatticeConcept> {
    let Some(concept) = artifact.concepts.iter().find(|row| row.id == concept_id) else {
        return Vec::new();
    };
    let ids: &[String] = match direction {
        LatticeWalk::Parent => &concept.parents,
        LatticeWalk::Child => &concept.children,
        LatticeWalk::Peer => {
            return peers(artifact, concept, limit);
        }
    };
    let mut neighbors = ids
        .iter()
        .filter_map(|id| artifact.concepts.iter().find(|row| &row.id == id))
        .filter(|neighbor| match direction {
            LatticeWalk::Parent => is_more_general(neighbor, concept),
            LatticeWalk::Child => is_more_general(concept, neighbor),
            LatticeWalk::Peer => true,
        })
        .cloned()
        .collect::<Vec<_>>();
    neighbors.sort_by(|left, right| match direction {
        LatticeWalk::Parent => left
            .intent
            .len()
            .cmp(&right.intent.len())
            .then(right.extent_size.cmp(&left.extent_size)),
        LatticeWalk::Child => right
            .intent
            .len()
            .cmp(&left.intent.len())
            .then(left.extent_size.cmp(&right.extent_size)),
        LatticeWalk::Peer => left.id.cmp(&right.id),
    });
    neighbors.truncate(limit.max(1));
    neighbors
}

/// Same-family concepts that are not the current one.
pub fn family_peers(
    artifact: &LatticeArtifact,
    concept_id: &str,
    limit: usize,
) -> Vec<LatticeConcept> {
    let Some(concept) = artifact.concepts.iter().find(|row| row.id == concept_id) else {
        return Vec::new();
    };
    artifact
        .concepts
        .iter()
        .filter(|row| row.id != concept.id && row.family == concept.family)
        .take(limit.max(1))
        .cloned()
        .collect()
}

/// Lattice walk direction (cover morphisms).
#[derive(Debug, Clone, Copy)]
pub enum LatticeWalk {
    Parent,
    Child,
    Peer,
}

fn peers(
    artifact: &LatticeArtifact,
    concept: &LatticeConcept,
    limit: usize,
) -> Vec<LatticeConcept> {
    let mut ids = BTreeSet::new();
    for parent in &concept.parents {
        if let Some(parent_concept) = artifact.concepts.iter().find(|row| &row.id == parent) {
            for child in &parent_concept.children {
                if child != &concept.id {
                    ids.insert(child.clone());
                }
            }
        }
    }
    ids.into_iter()
        .filter_map(|id| artifact.concepts.iter().find(|row| row.id == id).cloned())
        .take(limit.max(1))
        .collect()
}

fn is_more_general(parent: &LatticeConcept, child: &LatticeConcept) -> bool {
    parent.intent.len() <= child.intent.len()
        && parent.extent_size >= child.extent_size
        && (parent.intent.len() < child.intent.len() || parent.extent_size > child.extent_size)
}

fn orient_cover(
    concepts: &BTreeMap<String, LatticeConcept>,
    left: &str,
    right: &str,
) -> Option<(String, String)> {
    let child = concepts.get(left)?;
    let parent = concepts.get(right)?;
    if is_more_general(parent, child) {
        Some((left.to_string(), right.to_string()))
    } else if is_more_general(child, parent) {
        Some((right.to_string(), left.to_string()))
    } else {
        None
    }
}

fn sort_ids_by_rank(
    ranks: &BTreeMap<String, (usize, usize)>,
    ids: &mut [String],
    general_first: bool,
) {
    ids.sort_by(|left, right| {
        let (li, le) = ranks.get(left).copied().unwrap_or((0, 0));
        let (ri, re) = ranks.get(right).copied().unwrap_or((0, 0));
        if general_first {
            li.cmp(&ri).then(re.cmp(&le)).then(left.cmp(right))
        } else {
            ri.cmp(&li).then(le.cmp(&re)).then(left.cmp(right))
        }
    });
}

impl Default for LatticeArtifact {
    fn default() -> Self {
        empty_artifact()
    }
}

fn empty_artifact() -> LatticeArtifact {
    LatticeArtifact {
        provenance: None,
        schema: LATTICE_SCHEMA.to_string(),
        category: LatticeCategory::default(),
        concepts: Vec::new(),
        memberships: BTreeMap::new(),
        functor: FunctorWitness {
            name: "identity_cover_to_owl".to_string(),
            coherence: 1.0,
            preserved: 0,
            total: 0,
        },
        heading_category: LatticeCategory::default(),
        heading_objects: Vec::new(),
        heading_alignment: Vec::new(),
        heading_functor: FunctorWitness {
            name: "wiki_heading_to_lattice".to_string(),
            coherence: 1.0,
            preserved: 0,
            total: 0,
        },
    }
}

fn heading_category_from(objects: &[HeadingObject]) -> LatticeCategory {
    let mut by_file: BTreeMap<&str, Vec<&HeadingObject>> = BTreeMap::new();
    for object in objects {
        by_file
            .entry(object.path.as_str())
            .or_default()
            .push(object);
    }
    let mut morphisms = Vec::new();
    for rows in by_file.values() {
        for child in rows {
            let Some(parent_path) = parent_heading_path(&child.heading_path) else {
                continue;
            };
            let parent = rows
                .iter()
                .filter(|row| {
                    row.heading_path == parent_path && row.line <= child.line && row.id != child.id
                })
                .max_by_key(|row| row.line);
            let Some(parent) = parent else {
                continue;
            };
            morphisms.push(Morphism {
                source: child.id.clone(),
                target: parent.id.clone(),
                label: "subClassOf".to_string(),
            });
        }
    }
    LatticeCategory {
        objects: objects.iter().map(|object| object.id.clone()).collect(),
        morphisms,
    }
}

fn parent_heading_path(path: &str) -> Option<String> {
    let segments = crate::knowledge::heading_segments(path);
    if segments.len() < 2 {
        return None;
    }
    Some(segments[..segments.len() - 1].join(" > "))
}

fn section_membership_key(needle: &str) -> Option<String> {
    if needle.starts_with("section:") {
        return Some(needle.to_string());
    }
    let (path, line) = needle.rsplit_once('#')?;
    if path.is_empty() || line.is_empty() {
        return None;
    }
    if !path.contains('/') && !path.contains('.') {
        return None;
    }
    Some(format!("section:{path}#{line}"))
}

fn render_owl(artifact: &LatticeArtifact) -> String {
    let mut out = String::from(
        "@prefix owl: <http://www.w3.org/2002/07/owl#> .\n\
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
         @prefix leio: <urn:leio:concept/> .\n\n",
    );
    for concept in &artifact.concepts {
        let iri = turtle_local(&concept.id);
        out.push_str(&format!("leio:{iri} a owl:Class ;\n"));
        out.push_str(&format!(
            "  rdfs:label {} ;\n",
            turtle_string(&concept.label)
        ));
        if concept.parents.is_empty() {
            out.push_str("  rdfs:subClassOf owl:Thing .\n\n");
        } else {
            for (i, parent) in concept.parents.iter().enumerate() {
                let end = if i + 1 == concept.parents.len() {
                    " .\n\n"
                } else {
                    " ;\n"
                };
                out.push_str(&format!(
                    "  rdfs:subClassOf leio:{}{end}",
                    turtle_local(parent)
                ));
            }
        }
    }
    out
}

fn turtle_local(id: &str) -> String {
    id.chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

fn turtle_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn readiness_index(root: &Path) -> RepoIndex {
        RepoIndex {
            version: 1,
            root: root.display().to_string(),
            indexed_at: "2026-09-08T00:00:00Z".into(),
            files: vec![crate::model::FileRecord {
                path: "src/start.rs".into(),
                language: crate::model::SourceLanguage::Rust,
                bytes: 100,
                modified_unix_ms: 1,
                symbols: Vec::new(),
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
        }
    }

    fn write_readiness_fixture(index: &RepoIndex, root: &Path) {
        let pairs = crate::export::structural_fca_pairs_at(index, root);
        let mut artifact = LatticeArtifact {
            provenance: Some(provenance_for_pairs(&pairs)),
            ..Default::default()
        };
        attach_source_provenance(&mut artifact, index, root);
        write_artifacts(
            &crate::export::default_formal_context_output_dir(root),
            &artifact,
        )
        .expect("persist fixture");
    }

    #[test]
    fn readiness_ignores_timestamp_refresh_but_detects_structural_change() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let mut index = readiness_index(dir.path());
        let mut other_file = index.files[0].clone();
        other_file.path = "src/other.rs".into();
        index.files.push(other_file);
        write_readiness_fixture(&index, dir.path());
        assert_eq!(lattice_readiness(&index, dir.path())["state"], "current");

        index.indexed_at = "2026-09-08T01:00:00Z".into();
        index.files[0].bytes += 10;
        index.files[0].modified_unix_ms += 1;
        index.files.reverse();
        let current = lattice_readiness(&index, dir.path());
        assert_eq!(current["state"], "current");
        assert_eq!(current["rebuild_required"], false);
        assert_eq!(current["indexed_at"], "2026-09-08T00:00:00Z");
        assert_eq!(current["current_indexed_at"], index.indexed_at);

        index.files[0].language = crate::model::SourceLanguage::Python;
        let stale = lattice_readiness(&index, dir.path());
        assert_eq!(stale["state"], "stale");
        assert_eq!(stale["rebuild_required"], true);
        assert_ne!(stale["fingerprint"], stale["current_fingerprint"]);
    }

    #[test]
    fn readiness_detects_live_wiki_headings_without_index_changes() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let index = readiness_index(dir.path());
        fs::write(
            dir.path().join("README.md"),
            "# Navigation\n\nFirst topic.\n",
        )
        .expect("wiki source");
        let pairs = crate::export::structural_fca_pairs_at(&index, dir.path());
        assert!(
            pairs
                .iter()
                .any(|(object, _)| object.starts_with("section:"))
        );
        write_readiness_fixture(&index, dir.path());
        assert_eq!(lattice_readiness(&index, dir.path())["state"], "current");
        fs::write(dir.path().join("README.md"), "# Graph\n\nFirst topic.\n")
            .expect("changed wiki heading");
        assert_eq!(lattice_readiness(&index, dir.path())["state"], "stale");
    }

    #[test]
    fn readiness_distinguishes_missing_malformed_unsupported_and_legacy_artifacts() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let index = readiness_index(dir.path());
        let path = lattice_path(dir.path());
        let missing = lattice_readiness(&index, dir.path());
        assert_eq!(missing["state"], "missing");
        assert_eq!(missing["artifact_path"], path.display().to_string());
        assert!(!path.exists(), "readiness must never induce a lattice");
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, "{broken").expect("malformed fixture");
        assert_eq!(lattice_readiness(&index, dir.path())["state"], "invalid");
        assert_eq!(
            fs::read_to_string(&path).expect("preserved artifact"),
            "{broken"
        );

        let mut legacy = serde_json::to_value(LatticeArtifact::default()).expect("legacy value");
        assert!(legacy.get("provenance").is_none());
        fs::write(&path, serde_json::to_vec(&legacy).expect("legacy bytes")).expect("legacy");
        assert!(
            load_lattice(&path)
                .expect("backwards-compatible load")
                .is_some()
        );
        assert_eq!(lattice_readiness(&index, dir.path())["state"], "unverified");
        legacy["schema"] = json!("future.unknown");
        fs::write(&path, serde_json::to_vec(&legacy).expect("future bytes")).expect("future");
        assert_eq!(lattice_readiness(&index, dir.path())["state"], "invalid");
    }

    #[test]
    fn readiness_never_reuses_another_repositories_index_or_artifact() {
        let first = tempfile::tempdir().expect("first root");
        let second = tempfile::tempdir().expect("second root");
        let first_index = readiness_index(first.path());
        let second_index = readiness_index(second.path());
        write_readiness_fixture(&first_index, first.path());
        let mismatched_index = lattice_readiness(&second_index, first.path());
        assert_eq!(mismatched_index["state"], "invalid");
        assert_eq!(mismatched_index["rebuild_required"], false);
        fs::create_dir_all(lattice_path(second.path()).parent().expect("parent")).expect("mkdir");
        fs::copy(lattice_path(first.path()), lattice_path(second.path())).expect("copied artifact");
        assert_eq!(
            lattice_readiness(&second_index, second.path())["state"],
            "stale"
        );
        fs::write(second.path().join("README.md"), "# Separate wiki\n").expect("other wiki");
        assert!(
            !crate::export::structural_fca_pairs_at(&first_index, first.path())
                .iter()
                .any(|(object, _)| object.starts_with("section:"))
        );
    }

    #[test]
    fn structural_export_stamps_readiness_provenance() {
        if !crate::fca::wheel_available() {
            eprintln!("skipped: no fca wheel available");
            return;
        }
        let dir = tempfile::tempdir().expect("tmpdir");
        let index = readiness_index(dir.path());
        crate::export::export_formal_context(
            &index,
            dir.path(),
            &crate::export::default_formal_context_output_dir(dir.path()),
        )
        .expect("structural export");
        let readiness = lattice_readiness(&index, dir.path());
        assert_eq!(readiness["state"], "current");
        assert!(
            readiness["generated_at"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert_eq!(readiness["source_root"], normalized_root(dir.path()));
        assert!(readiness["pair_count"].as_u64().expect("pair count") > 0);
    }

    #[test]
    fn ensure_lattice_stamps_even_an_empty_induction_without_a_wheel() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let mut index = readiness_index(dir.path());
        index.files.clear();
        let artifact = ensure_lattice(&index, dir.path()).expect("empty lattice");
        assert_eq!(artifact.provenance.expect("provenance").pair_count, 0);
        assert_eq!(lattice_readiness(&index, dir.path())["state"], "current");
    }

    #[test]
    fn incidence_hash_delimits_pair_members() {
        assert_ne!(
            incidence_fingerprint(&[("ab".into(), "c".into())]),
            incidence_fingerprint(&[("a".into(), "bc".into())]),
        );
    }

    /// Lattice tests need the pre-built wheel; skip (not fail) without it.
    fn induce_or_skip(pairs: &[(String, String)]) -> Option<LatticeArtifact> {
        if !crate::fca::wheel_available() {
            eprintln!("skipped: no fca wheel available");
            return None;
        }
        Some(induce_from_pairs(pairs).expect("wheel induction"))
    }

    #[test]
    fn induce_builds_cover_and_coherent_functor() {
        let pairs = vec![
            ("file:a.rs".into(), "readsEnv:KEY".into()),
            ("file:a.rs".into(), "root:src".into()),
            ("file:b.rs".into(), "readsEnv:KEY".into()),
            ("file:b.rs".into(), "root:src".into()),
            ("file:c.rs".into(), "root:docs".into()),
        ];
        let Some(artifact) = induce_or_skip(&pairs) else {
            return;
        };
        assert!(!artifact.concepts.is_empty());
        assert!((artifact.functor.coherence - 1.0).abs() < f32::EPSILON);
        let owl = render_owl(&artifact);
        assert!(owl.contains("owl:Class"));
        assert!(artifact.memberships.contains_key("file:a.rs"));
    }

    #[test]
    fn try_induce_on_demand_limits_refuse_oversized_and_pass_small() {
        if !crate::fca::wheel_available() {
            eprintln!("skipped: no fca wheel available");
            return;
        }
        unsafe {
            std::env::set_var("LEIO_LATTICE_MAX_ON_DEMAND_OBJECTS", "3");
            std::env::set_var("LEIO_LATTICE_MAX_ON_DEMAND_PAIRS", "10");
        }
        let oversized = vec![
            ("file:a.rs".into(), "root:src".into()),
            ("file:b.rs".into(), "root:docs".into()),
            ("file:c.rs".into(), "root:docs".into()),
            ("file:d.rs".into(), "root:docs".into()),
        ];
        let result = try_induce_from_pairs(&oversized);
        let message = result
            .expect_err("oversized context refuses on-demand induction")
            .to_string();
        assert!(message.contains("concept lattice induction skipped"));
        assert!(message.contains("LEIO_LATTICE_MAX_ON_DEMAND_OBJECTS"));

        let small = vec![
            ("file:a.rs".into(), "readsEnv:KEY".into()),
            ("file:a.rs".into(), "root:src".into()),
            ("file:b.rs".into(), "readsEnv:KEY".into()),
            ("file:b.rs".into(), "root:src".into()),
            ("file:c.rs".into(), "root:docs".into()),
        ];
        let artifact = try_induce_from_pairs(&small).expect("context within limits induces");
        assert!(!artifact.concepts.is_empty());
        assert!(artifact.memberships.contains_key("file:a.rs"));

        unsafe {
            std::env::remove_var("LEIO_LATTICE_MAX_ON_DEMAND_OBJECTS");
            std::env::remove_var("LEIO_LATTICE_MAX_ON_DEMAND_PAIRS");
        }
    }

    #[test]
    fn walk_parents_are_strictly_more_general() {
        let pairs = vec![
            ("file:a.rs".into(), "readsEnv:KEY".into()),
            ("file:a.rs".into(), "root:src".into()),
            ("file:b.rs".into(), "readsEnv:KEY".into()),
            ("file:c.rs".into(), "root:docs".into()),
        ];
        let Some(artifact) = induce_or_skip(&pairs) else {
            return;
        };
        let start_id = artifact
            .memberships
            .get("file:a.rs")
            .expect("membership")
            .concept_id
            .clone();
        let start = artifact
            .concepts
            .iter()
            .find(|row| row.id == start_id)
            .expect("concept");
        let parents = walk(&artifact, &start_id, LatticeWalk::Parent, 16);
        for parent in &parents {
            assert!(
                is_more_general(parent, start),
                "parent {} intent={} ext={} vs start intent={} ext={}",
                parent.id,
                parent.intent.len(),
                parent.extent_size,
                start.intent.len(),
                start.extent_size
            );
        }
        let children = walk(&artifact, &start_id, LatticeWalk::Child, 16);
        for child in &children {
            assert!(
                is_more_general(start, child),
                "child {} is not more specific than {}",
                child.id,
                start.id
            );
        }
    }

    #[test]
    fn wiki_section_objects_get_memberships() {
        let pairs = vec![
            ("file:docs/knowledge.md".into(), "kind:file".into()),
            (
                "section:docs/knowledge.md#1".into(),
                "heading:Knowledge wiki".into(),
            ),
            ("section:docs/knowledge.md#1".into(), "kind:section".into()),
            (
                "file:docs/knowledge.md".into(),
                "hasSection:Knowledge wiki".into(),
            ),
        ];
        let Some(artifact) = induce_or_skip(&pairs) else {
            return;
        };
        assert!(
            artifact
                .memberships
                .contains_key("section:docs/knowledge.md#1")
        );
        assert!(artifact.memberships.contains_key("file:docs/knowledge.md"));
    }

    #[test]
    fn heading_functor_preserves_nested_outline() {
        let parent = crate::knowledge::WikiSectionInc {
            title: "Root".into(),
            topic: "docs".into(),
            source_path: "docs/page.md".into(),
            heading_path: "Root".into(),
            line: 1,
        };
        let child = crate::knowledge::WikiSectionInc {
            title: "Child".into(),
            topic: "docs".into(),
            source_path: "docs/page.md".into(),
            heading_path: "Root > Child".into(),
            line: 5,
        };
        let pairs = vec![
            (parent.object_id(), "kind:section".into()),
            (parent.object_id(), "heading:Root".into()),
            (parent.object_id(), "inFile:docs/page.md".into()),
            (child.object_id(), "kind:section".into()),
            (child.object_id(), "heading:Root".into()),
            (child.object_id(), "heading:Child".into()),
            (child.object_id(), "inFile:docs/page.md".into()),
            // A third object so the shared parent intent is not the universal top
            // (skipped by MembershipOptions::skip_universal_top).
            ("file:other.rs".into(), "kind:file".into()),
        ];
        let Some(mut artifact) = induce_or_skip(&pairs) else {
            return;
        };
        apply_heading_functor(&mut artifact, &[parent, child]);
        assert_eq!(artifact.heading_category.morphisms.len(), 1);
        assert_eq!(artifact.heading_category.morphisms[0].label, "subClassOf");
        assert_eq!(artifact.heading_functor.name, "wiki_heading_to_lattice");
        assert_eq!(artifact.heading_functor.total, 1);
        assert_eq!(artifact.heading_functor.preserved, 1);
        assert!((artifact.heading_functor.coherence - 1.0).abs() < f32::EPSILON);
        let start = heading_for(&artifact, "docs/page.md#5").expect("child heading");
        let parents = heading_walk(&artifact, &start.id, LatticeWalk::Parent, 8);
        assert_eq!(parents.len(), 1);
        assert_eq!(parents[0].heading_path, "Root");
    }

    #[test]
    fn heading_functor_drops_when_alignment_inverts_nesting() {
        let objects = vec![
            HeadingObject {
                id: "section:a.md#1".into(),
                path: "a.md".into(),
                title: "Root".into(),
                heading_path: "Root".into(),
                line: 1,
            },
            HeadingObject {
                id: "section:a.md#5".into(),
                path: "a.md".into(),
                title: "Child".into(),
                heading_path: "Root > Child".into(),
                line: 5,
            },
        ];
        let heading = heading_category_from(&objects);
        let lattice = LatticeCategory {
            objects: vec!["fca:child:1".into(), "fca:root:0".into()],
            morphisms: vec![Morphism {
                source: "fca:child:1".into(),
                target: "fca:root:0".into(),
                label: "subClassOf".into(),
            }],
        };
        let inverted = vec![
            Alignment {
                source_uri: "section:a.md#5".into(),
                target_uri: "fca:root:0".into(),
            },
            Alignment {
                source_uri: "section:a.md#1".into(),
                target_uri: "fca:child:1".into(),
            },
        ];
        let stats = evaluate_functor(&heading, &lattice, &inverted);
        assert_eq!(stats.total_count, 1);
        assert_eq!(stats.preserved_count, 0);
        assert_eq!(stats.coherence, 0.0);
    }
}
