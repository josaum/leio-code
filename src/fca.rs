//! Formal Concept Analysis (FCA) concept lattice induction and implication mining.
//!
//! leio-code provides a native, pure Rust in-process concept lattice induction
//! engine using the In-Close algorithm over compact bitset matrices. It computes
//! closed concept intents/extents, cover edges (Hasse diagram links), and
//! topological lattice depths without external runtime dependencies.
//!
//! For environments that specifically require the external PyO3 `fca_fast` parser
//! wheel via `uv run`, an optional Python bridge is retained and selectable via
//! `LEIO_FCA_FORCE_WHEEL=1` or `LEIO_FCA_PREFER_WHEEL=1`.
//!
//! Features:
//! - [`native_induct`] — pure Rust concept lattice induction;
//! - [`induct`] — inducts lattice via native engine (or wheel if requested);
//! - [`derive_memberships`] — per-object concept memberships (primary =
//!   most specific concept) used for row tagging and `lattice.json`;
//! - [`mine_single_premise`] — exception-free implication mining, which is
//!   pure `(object, attribute)` pair algebra and needs no lattice at all.
//!
//! Wheel resolution order (when wheel engine is enabled):
//! 1. `LEIO_FCA_WHEEL` — path or URL to a specific `fca_fast` wheel.
//! 2. `LEIO_FCA_FIND_LINKS` — directory or index passed to `--find-links`.
//! 3. Committed wheels under `artifacts/wheels/<platform>/` in this checkout
//!    (`macos-arm64`, `manylinux`), following the same artifact convention
//!    the rest of the workspace enforces for pre-built parser wheels.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

/// One concept as reported by the induction engine: node index, label, extent
/// (object names), intent (attribute labels), and both sizes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WheelConcept {
    pub node_id: usize,
    pub label: String,
    #[allow(dead_code)]
    pub source: String,
    #[serde(default)]
    pub extent: Vec<String>,
    #[serde(default)]
    pub intent: Vec<String>,
    #[serde(default)]
    pub extent_size: usize,
    #[serde(default)]
    pub intent_size: usize,
}

/// One cover edge: `source` (more specific concept) → `target` (more
/// general concept), by node index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WheelLink {
    pub source: usize,
    pub target: usize,
}

/// The reduced lattice result consumed by downstream exports and navigation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InducedLattice {
    #[serde(default)]
    pub input_count: usize,
    #[serde(default)]
    pub object_count: usize,
    #[serde(default)]
    pub attribute_count: usize,
    #[serde(default)]
    pub concept_count: usize,
    #[serde(default)]
    pub edge_count: usize,
    #[serde(default)]
    pub depth: usize,
    pub concepts: Vec<WheelConcept>,
    #[serde(default)]
    pub links: Vec<WheelLink>,
}

/// Why an induction could not run.
///
/// `WheelUnavailable` means the environment has no usable wheel (not
/// resolved, or `uv` is missing) — callers treat it as a graceful skip, not a
/// broken contract. `Failed` means a resolved wheel ran and errored.
#[derive(Debug)]
pub enum InductError {
    WheelUnavailable(String),
    Failed(String),
}

impl std::fmt::Display for InductError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InductError::WheelUnavailable(detail) => {
                write!(
                    f,
                    "fca wheel unavailable ({detail}); set LEIO_FCA_WHEEL or \
                     LEIO_FCA_FIND_LINKS, or commit wheels under artifacts/wheels/"
                )
            }
            InductError::Failed(detail) => write!(f, "fca wheel induction failed: {detail}"),
        }
    }
}

/// True when a concept lattice induction engine is available.
///
/// Because leio-code ships a high-performance in-process Rust induction engine,
/// lattice induction is always available without requiring an external Python wheel.
pub(crate) fn wheel_available() -> bool {
    true
}

/// True when an external Python wheel can be resolved.
#[allow(dead_code)]
pub(crate) fn python_wheel_resolved() -> bool {
    find_links_spec().is_some()
}

/// Resolve the `--find-links` spec for the wheel, if any.
fn find_links_spec() -> Option<String> {
    if let Ok(wheel) = std::env::var("LEIO_FCA_WHEEL") {
        let trimmed = wheel.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    if let Ok(links) = std::env::var("LEIO_FCA_FIND_LINKS") {
        let trimmed = links.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    default_committed_wheel()
}

/// Wheel filename suffix matching the current CPU architecture, so a
/// directory holding several platform wheels resolves to the right one.
fn arch_wheel_suffix() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64.whl"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64.whl"
    } else {
        ""
    }
}

/// Committed pre-built wheel for the current platform, if present.
fn default_committed_wheel() -> Option<String> {
    let platform_dir = if cfg!(target_os = "macos") {
        "macos-arm64"
    } else {
        "manylinux"
    };
    let arch_suffix = arch_wheel_suffix();
    let candidates = [
        PathBuf::from("artifacts").join("wheels").join(platform_dir),
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("artifacts")
            .join("wheels")
            .join(platform_dir),
    ];
    for dir in candidates {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut wheels: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension().is_some_and(|ext| ext == "whl")
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name.starts_with("fca_fast-")
                                && (arch_suffix.is_empty() || name.ends_with(arch_suffix))
                        })
            })
            .collect();
        wheels.sort();
        if let Some(wheel) = wheels.first() {
            return Some(wheel.display().to_string());
        }
    }
    None
}

/// Python bridge: read pairs as JSON on stdin, run
/// `fca_fast.induct_from_pairs`, and emit the reduced lattice as JSON on
/// stdout (concepts + cover links + counts).
const BRIDGE_SCRIPT: &str = r#"
import json, sys
import fca_fast

pairs = json.load(sys.stdin)
result = fca_fast.induct_from_pairs([tuple(p) for p in pairs])
graph = json.loads(result["graph_json"])
out = {key: result[key] for key in (
    "input_count", "object_count", "attribute_count", "concept_count",
    "edge_count", "depth",
)}
out["concepts"] = result["concepts"]
out["links"] = graph.get("links", [])
json.dump(out, sys.stdout)
"#;

/// Compact bitset representation for fast set algebra over formal contexts.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BitSet {
    words: Vec<u64>,
    len: usize,
}

impl BitSet {
    fn new(len: usize) -> Self {
        let num_words = len.div_ceil(64);
        Self {
            words: vec![0u64; num_words],
            len,
        }
    }

    fn full(len: usize) -> Self {
        let num_words = len.div_ceil(64);
        let mut words = vec![!0u64; num_words];
        if !len.is_multiple_of(64) && num_words > 0 {
            words[num_words - 1] = (1u64 << (len % 64)) - 1;
        }
        Self { words, len }
    }

    #[inline]
    fn set(&mut self, bit: usize) {
        if bit < self.len {
            self.words[bit / 64] |= 1u64 << (bit % 64);
        }
    }

    #[inline]
    fn contains(&self, bit: usize) -> bool {
        if bit < self.len {
            (self.words[bit / 64] & (1u64 << (bit % 64))) != 0
        } else {
            false
        }
    }

    #[inline]
    fn count_ones(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.words.iter().all(|&w| w == 0)
    }

    #[inline]
    fn is_subset(&self, other: &BitSet) -> bool {
        self.words
            .iter()
            .zip(&other.words)
            .all(|(&a, &b)| (a & !b) == 0)
    }

    #[inline]
    fn intersect(&self, other: &BitSet) -> BitSet {
        let words = self
            .words
            .iter()
            .zip(&other.words)
            .map(|(&a, &b)| a & b)
            .collect();
        BitSet {
            words,
            len: self.len,
        }
    }

    fn ones(&self) -> Vec<usize> {
        let mut res = Vec::with_capacity(self.count_ones());
        for (w_idx, &word) in self.words.iter().enumerate() {
            let mut w = word;
            while w != 0 {
                let t = w.trailing_zeros() as usize;
                let bit = w_idx * 64 + t;
                if bit < self.len {
                    res.push(bit);
                }
                w &= w - 1;
            }
        }
        res
    }
}

/// Pure Rust concept lattice induction using In-Close over bitset incidences.
pub fn native_induct(pairs: &[(String, String)]) -> InducedLattice {
    if pairs.is_empty() {
        return InducedLattice {
            input_count: 0,
            object_count: 0,
            attribute_count: 0,
            concept_count: 0,
            edge_count: 0,
            depth: 0,
            concepts: Vec::new(),
            links: Vec::new(),
        };
    }

    // 1. Collect unique sorted objects and attributes
    let mut object_set = BTreeSet::new();
    let mut attribute_set = BTreeSet::new();
    for (obj, attr) in pairs {
        if !obj.is_empty() && !attr.is_empty() {
            object_set.insert(obj.as_str());
            attribute_set.insert(attr.as_str());
        }
    }

    let objects: Vec<&str> = object_set.into_iter().collect();
    let attributes: Vec<&str> = attribute_set.into_iter().collect();
    let num_objects = objects.len();
    let num_attributes = attributes.len();

    if num_objects == 0 || num_attributes == 0 {
        return InducedLattice {
            input_count: pairs.len(),
            object_count: num_objects,
            attribute_count: num_attributes,
            concept_count: 0,
            edge_count: 0,
            depth: 0,
            concepts: Vec::new(),
            links: Vec::new(),
        };
    }

    let obj_map: HashMap<&str, usize> = objects
        .iter()
        .enumerate()
        .map(|(i, &obj)| (obj, i))
        .collect();
    let attr_map: HashMap<&str, usize> = attributes
        .iter()
        .enumerate()
        .map(|(j, &attr)| (attr, j))
        .collect();

    // 2. Build incidence bitsets:
    let mut attr_extents = vec![BitSet::new(num_objects); num_attributes];
    for (obj, attr) in pairs {
        if let (Some(&o_idx), Some(&a_idx)) =
            (obj_map.get(obj.as_str()), attr_map.get(attr.as_str()))
        {
            attr_extents[a_idx].set(o_idx);
        }
    }

    // 3. In-Close algorithm to generate all concepts
    // Root concept: extent is all objects G. Intent is intersection of all objects' intents.
    let top_extent = BitSet::full(num_objects);
    let mut top_intent = BitSet::new(num_attributes);
    for (j, extent) in attr_extents.iter().enumerate() {
        if extent.count_ones() == num_objects {
            top_intent.set(j);
        }
    }

    let mut raw_concepts: Vec<(BitSet, BitSet)> = Vec::new();
    raw_concepts.push((top_extent.clone(), top_intent.clone()));

    fn in_close_recursive(
        extent: BitSet,
        intent: BitSet,
        start_attr: usize,
        num_attributes: usize,
        attr_extents: &[BitSet],
        raw_concepts: &mut Vec<(BitSet, BitSet)>,
    ) {
        for j in start_attr..num_attributes {
            if intent.contains(j) {
                continue;
            }

            let candidate_extent = extent.intersect(&attr_extents[j]);
            if candidate_extent.is_empty() {
                continue;
            }

            if candidate_extent.count_ones() == extent.count_ones() {
                continue;
            }

            // Canonical test: is candidate_extent already covered by some attribute r < j not in intent?
            let mut is_canonic = true;
            for (r, r_extent) in attr_extents.iter().enumerate().take(j) {
                if !intent.contains(r) && candidate_extent.is_subset(r_extent) {
                    is_canonic = false;
                    break;
                }
            }

            if is_canonic {
                let mut child_intent = intent.clone();
                child_intent.set(j);
                for (r, r_extent) in attr_extents.iter().enumerate().skip(j + 1) {
                    if candidate_extent.is_subset(r_extent) {
                        child_intent.set(r);
                    }
                }

                raw_concepts.push((candidate_extent.clone(), child_intent.clone()));
                in_close_recursive(
                    candidate_extent,
                    child_intent,
                    j + 1,
                    num_attributes,
                    attr_extents,
                    raw_concepts,
                );
            }
        }
    }

    in_close_recursive(
        top_extent,
        top_intent,
        0,
        num_attributes,
        &attr_extents,
        &mut raw_concepts,
    );

    // Ensure bottom concept (empty extent, all attributes) is present if no concept has all attributes
    let has_bottom = raw_concepts
        .iter()
        .any(|(_, int)| int.count_ones() == num_attributes);
    if !has_bottom {
        let bottom_extent = BitSet::new(num_objects);
        let bottom_intent = BitSet::full(num_attributes);
        raw_concepts.push((bottom_extent, bottom_intent));
    }

    // 4. Sort concepts topologically:
    // Top-to-bottom: descending extent_size, then ascending intent_size, then lexicographical order.
    raw_concepts.sort_by(|(ext_a, int_a), (ext_b, int_b)| {
        let size_a = ext_a.count_ones();
        let size_b = ext_b.count_ones();
        size_b
            .cmp(&size_a)
            .then_with(|| int_a.count_ones().cmp(&int_b.count_ones()))
            .then_with(|| {
                let a_ones = ext_a.ones();
                let b_ones = ext_b.ones();
                a_ones.cmp(&b_ones)
            })
    });

    raw_concepts.dedup_by(|(ext_a, int_a), (ext_b, int_b)| {
        ext_a.count_ones() == ext_b.count_ones()
            && ext_a.words == ext_b.words
            && int_a.words == int_b.words
    });

    // 5. Construct cover graph (Hasse diagram links)
    // Child (source) -> Parent (target)
    let num_concepts = raw_concepts.len();
    let mut links = Vec::new();
    let mut parents_by_child: Vec<Vec<usize>> = vec![Vec::new(); num_concepts];

    for child_idx in 0..num_concepts {
        let (child_ext, _) = &raw_concepts[child_idx];
        let child_size = child_ext.count_ones();

        let mut candidates: Vec<usize> = (0..num_concepts)
            .filter(|&parent_idx| {
                if parent_idx == child_idx {
                    return false;
                }
                let (parent_ext, _) = &raw_concepts[parent_idx];
                let parent_size = parent_ext.count_ones();
                if parent_size <= child_size && child_size > 0 {
                    return false;
                }
                child_ext.is_subset(parent_ext)
            })
            .collect();

        candidates.sort_by(|&a, &b| {
            let size_a = raw_concepts[a].0.count_ones();
            let size_b = raw_concepts[b].0.count_ones();
            size_a.cmp(&size_b)
        });

        let mut direct_parents: Vec<usize> = Vec::new();
        for candidate in candidates {
            let (cand_ext, _) = &raw_concepts[candidate];
            let mut is_redundant = false;
            for &p in &direct_parents {
                let (p_ext, _) = &raw_concepts[p];
                if p_ext.is_subset(cand_ext) {
                    is_redundant = true;
                    break;
                }
            }
            if !is_redundant {
                direct_parents.push(candidate);
                links.push(WheelLink {
                    source: child_idx,
                    target: candidate,
                });
            }
        }
        parents_by_child[child_idx] = direct_parents;
    }

    // 6. Compute depth via topological longest path
    let mut depth_by_node = vec![1usize; num_concepts];
    let mut max_depth = if num_concepts == 0 { 0 } else { 1 };

    for child_idx in 0..num_concepts {
        let mut d = 1;
        for &p in &parents_by_child[child_idx] {
            d = d.max(depth_by_node[p] + 1);
        }
        depth_by_node[child_idx] = d;
        max_depth = max_depth.max(d);
    }

    // 7. Format WheelConcept structures
    let mut concepts = Vec::with_capacity(num_concepts);
    for (node_id, (ext, int)) in raw_concepts.iter().enumerate() {
        let ext_objs: Vec<String> = ext
            .ones()
            .into_iter()
            .map(|idx| objects[idx].to_string())
            .collect();
        let int_attrs: Vec<String> = int
            .ones()
            .into_iter()
            .map(|idx| attributes[idx].to_string())
            .collect();

        let extent_size = ext_objs.len();
        let intent_size = int_attrs.len();

        let (label, source) = if int_attrs.is_empty() {
            (
                format!("concept_{node_id}"),
                "most_discriminating".to_string(),
            )
        } else {
            let mut parent_attrs = BTreeSet::new();
            for &p in &parents_by_child[node_id] {
                for attr_idx in raw_concepts[p].1.ones() {
                    parent_attrs.insert(attributes[attr_idx]);
                }
            }

            let new_attrs: Vec<&str> = int_attrs
                .iter()
                .filter(|a| !parent_attrs.contains(a.as_str()))
                .map(|s| s.as_str())
                .collect();

            if new_attrs.len() == 1 {
                (format_attr(new_attrs[0]), "most_discriminating".to_string())
            } else if new_attrs.is_empty() {
                let label = int_attrs
                    .iter()
                    .map(|a| format_attr(a))
                    .collect::<Vec<_>>()
                    .join(" + ");
                (label, "conjunction".to_string())
            } else {
                let label = new_attrs
                    .iter()
                    .map(|a| format_attr(a))
                    .collect::<Vec<_>>()
                    .join(" + ");
                (label, "conjunction".to_string())
            }
        };

        concepts.push(WheelConcept {
            node_id,
            label,
            source,
            extent: ext_objs,
            intent: int_attrs,
            extent_size,
            intent_size,
        });
    }

    let edge_count = links.len();

    InducedLattice {
        input_count: pairs.len(),
        object_count: num_objects,
        attribute_count: num_attributes,
        concept_count: num_concepts,
        edge_count,
        depth: max_depth,
        concepts,
        links,
    }
}

fn format_attr(attr: &str) -> String {
    attr.replace(':', " ")
}

/// Induct concept lattice via the external pre-built `fca_fast` Python wheel.
pub fn wheel_induct(pairs: &[(String, String)]) -> Result<InducedLattice, InductError> {
    if pairs.is_empty() {
        return Ok(InducedLattice {
            input_count: 0,
            object_count: 0,
            attribute_count: 0,
            concept_count: 0,
            edge_count: 0,
            depth: 0,
            concepts: Vec::new(),
            links: Vec::new(),
        });
    }
    let spec = find_links_spec()
        .ok_or_else(|| InductError::WheelUnavailable("no fca_fast wheel resolved".to_string()))?;

    // A direct wheel file installs with `--with <path>`; directories and
    // index URLs resolve the package by name through `--find-links`.
    let direct_wheel = spec.ends_with(".whl");
    let mut args: Vec<&str> = vec!["run", "--no-project", "--quiet"];
    if direct_wheel {
        args.extend(["--with", spec.as_str()]);
    } else {
        args.extend(["--with", "fca-fast", "--find-links", spec.as_str()]);
    }
    args.extend(["python3", "-c", BRIDGE_SCRIPT]);

    let mut child = Command::new("uv")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            InductError::WheelUnavailable(format!("could not spawn uv ({err}); is uv on PATH?"))
        })?;

    let payload = serde_json::to_vec(
        &pairs
            .iter()
            .map(|(object, attribute)| (object, attribute))
            .collect::<Vec<_>>(),
    )
    .map_err(|err| InductError::Failed(format!("encode pairs: {err}")))?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(&payload)
            .map_err(|err| InductError::Failed(format!("write pairs to wheel: {err}")))?;
    }
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .map_err(|err| InductError::Failed(format!("wait for wheel: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(InductError::Failed(format!(
            "exit {}: {}",
            output.status,
            stderr.trim().chars().take(400).collect::<String>()
        )));
    }
    serde_json::from_slice::<InducedLattice>(&output.stdout)
        .map_err(|err| InductError::Failed(format!("decode wheel output: {err}")))
}

/// Induct the concept lattice for `(object, attribute)` pairs.
///
/// Uses the high-performance native Rust engine by default, or defers to the
/// external PyO3 wheel if `LEIO_FCA_FORCE_WHEEL` or `LEIO_FCA_PREFER_WHEEL` is set.
pub fn induct(pairs: &[(String, String)]) -> Result<InducedLattice, InductError> {
    if pairs.is_empty() {
        return Ok(InducedLattice {
            input_count: 0,
            object_count: 0,
            attribute_count: 0,
            concept_count: 0,
            edge_count: 0,
            depth: 0,
            concepts: Vec::new(),
            links: Vec::new(),
        });
    }

    if std::env::var("LEIO_FCA_FORCE_WHEEL").is_ok()
        || (std::env::var("LEIO_FCA_PREFER_WHEEL").is_ok() && find_links_spec().is_some())
    {
        return wheel_induct(pairs);
    }

    Ok(native_induct(pairs))
}

/// Tunables for membership derivation. Defaults mirror the original
/// membership rows contract used for row tagging.
#[derive(Debug, Clone, Copy)]
pub struct MembershipOptions {
    /// Maximum number of intent tokens kept on the primary concept.
    pub primary_intent_limit: usize,
    /// Maximum number of additional concepts (excluding the primary) per
    /// object.
    pub additional_limit: usize,
    /// Maximum number of parents recorded per additional concept.
    pub parents_per_concept: usize,
    /// Skip the universal top concept (extent = all objects).
    pub skip_universal_top: bool,
    /// Skip the empty bottom concept (extent = empty).
    pub skip_universal_bottom: bool,
}

impl Default for MembershipOptions {
    fn default() -> Self {
        Self {
            primary_intent_limit: 32,
            additional_limit: 4,
            parents_per_concept: 4,
            skip_universal_top: true,
            skip_universal_bottom: true,
        }
    }
}

/// One additional (non-primary) concept an object participates in.
#[derive(Debug, Clone)]
pub struct AdditionalConcept {
    pub concept_id: String,
    pub concept_family: String,
    pub parent_concept_ids: Vec<String>,
}

/// All concept memberships for a single object.
#[derive(Debug, Clone)]
pub struct ConceptMembership {
    pub primary_concept_id: String,
    pub primary_concept_label: String,
    pub primary_concept_family: String,
    pub primary_intent_tokens: Vec<String>,
    pub additional: Vec<AdditionalConcept>,
}

/// Derive per-object concept memberships from an induced lattice.
///
/// Objects appear as their original string names in each concept's extent.
/// The primary membership is the most specific participating concept (largest
/// intent, then smallest extent, then deterministic by concept id).
pub fn derive_memberships(
    lattice: &InducedLattice,
    opts: MembershipOptions,
) -> HashMap<String, ConceptMembership> {
    if lattice.concepts.is_empty() {
        return HashMap::new();
    }
    let total_objects = lattice.object_count;

    // concept id + family per retained node, plus parent ids from the cover.
    struct Descriptor {
        concept_id: String,
        concept_label: String,
        family: String,
        intent_tokens: Vec<String>,
        intent_size: usize,
        extent_size: usize,
        parent_ids: Vec<String>,
    }

    let mut descriptors: HashMap<usize, Descriptor> =
        HashMap::with_capacity(lattice.concepts.len());
    for concept in &lattice.concepts {
        if opts.skip_universal_top && concept.extent_size == total_objects {
            continue;
        }
        if opts.skip_universal_bottom && concept.extent_size == 0 {
            continue;
        }
        descriptors.insert(
            concept.node_id,
            Descriptor {
                concept_id: concept_id(&concept.label, concept.node_id),
                concept_label: concept.label.clone(),
                family: derive_family(&concept.intent),
                intent_tokens: concept.intent.clone(),
                intent_size: concept.intent_size,
                extent_size: concept.extent_size,
                parent_ids: Vec::new(),
            },
        );
    }

    let mut parents_by_node: HashMap<usize, Vec<usize>> = HashMap::new();
    for link in &lattice.links {
        parents_by_node
            .entry(link.source)
            .or_default()
            .push(link.target);
    }
    for (node, parents) in parents_by_node {
        let parent_ids: Vec<String> = parents
            .into_iter()
            .filter_map(|parent| {
                descriptors
                    .get(&parent)
                    .map(|descriptor| descriptor.concept_id.clone())
            })
            .take(opts.parents_per_concept)
            .collect();
        if let Some(descriptor) = descriptors.get_mut(&node) {
            descriptor.parent_ids = parent_ids;
        }
    }

    // Invert: every object in a retained concept's extent gets an entry.
    struct Entry {
        intent_size: usize,
        extent_size: usize,
        concept_id: String,
        concept_label: String,
        family: String,
        intent_tokens: Vec<String>,
        parent_ids: Vec<String>,
    }

    let mut by_object: HashMap<String, Vec<Entry>> = HashMap::new();
    for concept in &lattice.concepts {
        let Some(descriptor) = descriptors.get(&concept.node_id) else {
            continue;
        };
        for object in &concept.extent {
            by_object.entry(object.clone()).or_default().push(Entry {
                intent_size: descriptor.intent_size,
                extent_size: descriptor.extent_size,
                concept_id: descriptor.concept_id.clone(),
                concept_label: descriptor.concept_label.clone(),
                family: descriptor.family.clone(),
                intent_tokens: descriptor.intent_tokens.clone(),
                parent_ids: descriptor.parent_ids.clone(),
            });
        }
    }

    let mut out = HashMap::with_capacity(by_object.len());
    for (object, mut entries) in by_object {
        if entries.is_empty() {
            continue;
        }
        // Most specific first: larger intent_size, then smaller extent_size,
        // then deterministic by concept_id.
        entries.sort_by(|a, b| {
            b.intent_size
                .cmp(&a.intent_size)
                .then(a.extent_size.cmp(&b.extent_size))
                .then(a.concept_id.cmp(&b.concept_id))
        });

        let primary = entries.remove(0);
        let additional = entries
            .into_iter()
            .take(opts.additional_limit)
            .map(|entry| AdditionalConcept {
                concept_id: entry.concept_id,
                concept_family: entry.family,
                parent_concept_ids: entry.parent_ids,
            })
            .collect();
        let primary_intent_tokens = primary
            .intent_tokens
            .into_iter()
            .take(opts.primary_intent_limit)
            .collect();

        out.insert(
            object,
            ConceptMembership {
                primary_concept_id: primary.concept_id,
                primary_concept_label: primary.concept_label,
                primary_concept_family: primary.family,
                primary_intent_tokens,
                additional,
            },
        );
    }

    out
}

/// A single-premise exception-free implication `premise => consequent`.
#[derive(Debug, Clone, PartialEq)]
pub struct Implication {
    pub premise: String,
    pub consequent: String,
    pub support: u32,
    pub frequency: f64,
    pub confidence: f64,
    pub stability: f64,
}

/// Largest extent size for which stability enumerates every non-empty subset
/// exactly (`2^12 - 1 = 4095` subsets); above it, deterministic sampling.
const EXACT_STABILITY_LIMIT: usize = 12;
/// Deterministic subset-sample count above the exact cutoff.
const STABILITY_SAMPLES: u32 = 256;

/// Mine single-premise exception-free implications from raw pairs.
///
/// For each attribute `a`, every attribute shared by all objects carrying
/// `a` is entailed without exception. Pure pair algebra — no lattice.
pub fn mine_single_premise(pairs: &[(String, String)]) -> Vec<Implication> {
    let mut objects_by_attribute: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut attributes_by_object: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (object, attribute) in pairs {
        objects_by_attribute
            .entry(attribute.as_str())
            .or_default()
            .insert(object.as_str());
        attributes_by_object
            .entry(object.as_str())
            .or_default()
            .insert(attribute.as_str());
    }
    let total_objects = attributes_by_object.len() as f64;

    let extent_of = |attribute: &str| -> BTreeSet<&str> {
        objects_by_attribute
            .get(attribute)
            .cloned()
            .unwrap_or_default()
    };

    let mut out = Vec::new();
    for (premise, extent) in &objects_by_attribute {
        let support = extent.len();
        if support == 0 {
            continue;
        }
        // Closure of the extent = attributes shared by every object carrying
        // the premise. Each such attribute `b` satisfies extent(a) ⊆
        // extent(b), i.e. `a => b` holds without exception.
        let mut closure_iter = extent.iter();
        let Some(first) = closure_iter.next() else {
            continue;
        };
        let mut closure: BTreeSet<&str> = attributes_by_object
            .get(*first)
            .cloned()
            .unwrap_or_default();
        for object in closure_iter {
            if let Some(attrs) = attributes_by_object.get(*object) {
                closure.retain(|attribute| attrs.contains(attribute));
            } else {
                closure.clear();
            }
            if closure.is_empty() {
                break;
            }
        }

        for consequent in &closure {
            if consequent == premise {
                continue;
            }
            let stability = extent_stability(&extent_of(consequent), extent);
            out.push(Implication {
                premise: (*premise).to_string(),
                consequent: (*consequent).to_string(),
                support: support as u32,
                frequency: if total_objects > 0.0 {
                    support as f64 / total_objects
                } else {
                    0.0
                },
                confidence: 1.0,
                stability,
            });
        }
    }

    out.sort_by(|left, right| {
        right
            .support
            .cmp(&left.support)
            .then_with(|| left.premise.cmp(&right.premise))
            .then_with(|| left.consequent.cmp(&right.consequent))
    });
    out
}

/// Sampled fraction of `premise_extent` subsets whose closure still contains
/// `consequent_extent` — a subset's closure contains the consequent iff every
/// member carries it, which reduces to membership in the consequent extent.
fn extent_stability(consequent_extent: &BTreeSet<&str>, premise_extent: &BTreeSet<&str>) -> f64 {
    let objects: Vec<&str> = premise_extent.iter().copied().collect();
    let k = objects.len();
    if k == 0 {
        return 0.0;
    }

    let subset_holds = |members: &[&str]| -> bool {
        members
            .iter()
            .all(|object| consequent_extent.contains(object))
    };

    if k <= EXACT_STABILITY_LIMIT {
        let mut held = 0u64;
        let mut total = 0u64;
        for mask in 1u32..(1u32 << k) {
            let members: Vec<&str> = (0..k)
                .filter(|bit| mask & (1u32 << bit) != 0)
                .map(|bit| objects[bit])
                .collect();
            total += 1;
            if subset_holds(&members) {
                held += 1;
            }
        }
        return held as f64 / total as f64;
    }

    // Deterministic Monte-Carlo: fixed seed so the draw is reproducible.
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut held = 0u64;
    for _ in 0..STABILITY_SAMPLES {
        let mut members: Vec<&str> = Vec::new();
        for &object in &objects {
            if splitmix64(&mut state) & 1 == 1 {
                members.push(object);
            }
        }
        if members.is_empty() {
            let index = (splitmix64(&mut state) as usize) % k;
            members.push(objects[index]);
        }
        if subset_holds(&members) {
            held += 1;
        }
    }
    held as f64 / f64::from(STABILITY_SAMPLES)
}

/// Advances a `splitmix64` state and returns the next pseudo-random value.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Stable concept id: `fca:<slug(label)>:<node>`.
pub(crate) fn concept_id(label: &str, node: usize) -> String {
    format!("fca:{}:{node}", slug(label))
}

/// Scans intent attribute strings for a meaningful prefix and returns its
/// slugged value.
pub(crate) fn derive_family(intent_tokens: &[String]) -> String {
    for prefix in ["family:", "topic:", "root:", "kind:"] {
        for token in intent_tokens {
            if let Some(rest) = token.strip_prefix(prefix) {
                let trimmed = rest.trim();
                if trimmed.is_empty() {
                    continue;
                }
                return slug(trimmed);
            }
        }
    }
    "concept".to_string()
}

/// Lowercase ASCII alphanum; runs of any other char collapse to a single
/// `-`. Always returns a non-empty string.
pub(crate) fn slug(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut last_dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "concept".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_lattice() -> InducedLattice {
        // Hand-built lattice equivalent of:
        //   (file:a, env:REDIS), (file:a, route:/x),
        //   (file:b, env:REDIS), (file:b, cartridge:revops),
        //   (file:c, route:/x)
        serde_json::from_value(serde_json::json!({
            "input_count": 5,
            "object_count": 3,
            "attribute_count": 3,
            "concept_count": 6,
            "edge_count": 7,
            "depth": 4,
            "concepts": [
                {"node_id": 0, "label": "concept_0", "source": "most_discriminating",
                 "extent": ["file:a", "file:b", "file:c"], "intent": [],
                 "extent_size": 3, "intent_size": 0},
                {"node_id": 1, "label": "route /x", "source": "most_discriminating",
                 "extent": ["file:a", "file:c"], "intent": ["route:/x"],
                 "extent_size": 2, "intent_size": 1},
                {"node_id": 2, "label": "env REDIS", "source": "most_discriminating",
                 "extent": ["file:a", "file:b"], "intent": ["env:REDIS"],
                 "extent_size": 2, "intent_size": 1},
                {"node_id": 3, "label": "cartridge revops", "source": "most_discriminating",
                 "extent": ["file:b"], "intent": ["env:REDIS", "cartridge:revops"],
                 "extent_size": 1, "intent_size": 2},
                {"node_id": 4, "label": "env REDIS + route /x", "source": "conjunction",
                 "extent": ["file:a"], "intent": ["env:REDIS", "route:/x"],
                 "extent_size": 1, "intent_size": 2},
                {"node_id": 5, "label": "cartridge revops", "source": "most_discriminating",
                 "extent": [], "intent": ["env:REDIS", "route:/x", "cartridge:revops"],
                 "extent_size": 0, "intent_size": 3}
            ],
            "links": [
                {"source": 1, "target": 0},
                {"source": 2, "target": 0},
                {"source": 3, "target": 2},
                {"source": 4, "target": 2},
                {"source": 4, "target": 1},
                {"source": 5, "target": 4},
                {"source": 5, "target": 3}
            ]
        }))
        .expect("fixture lattice")
    }

    #[test]
    fn slug_collapses_punctuation_and_lowercases() {
        assert_eq!(slug("Family:Customer Ops"), "family-customer-ops");
        assert_eq!(slug("---"), "concept");
        assert_eq!(slug(""), "concept");
    }

    #[test]
    fn family_uses_first_meaningful_prefix() {
        let tokens = vec![
            "kind:file".to_string(),
            "family:customer_ops".to_string(),
            "topic:audit".to_string(),
        ];
        assert_eq!(derive_family(&tokens), "customer-ops");
    }

    #[test]
    fn memberships_assign_most_specific_concept_as_primary() {
        let lattice = fixture_lattice();
        let memberships = derive_memberships(&lattice, MembershipOptions::default());
        assert_eq!(memberships.len(), 3, "one entry per non-bottom object");

        let a = &memberships["file:a"];
        // file:a lives in concepts 1, 2, 4; the most specific is 4
        // (intent_size 2, extent_size 1).
        assert_eq!(a.primary_concept_id, "fca:env-redis-route-x:4");
        assert_eq!(a.primary_concept_label, "env REDIS + route /x");
        assert!(a.primary_intent_tokens.contains(&"env:REDIS".to_string()));
        assert!(a.primary_intent_tokens.contains(&"route:/x".to_string()));

        let b = &memberships["file:b"];
        assert_eq!(b.primary_concept_id, "fca:cartridge-revops:3");

        let c = &memberships["file:c"];
        assert_eq!(c.primary_concept_id, "fca:route-x:1");

        // file:a additionally participates in concepts 1 and 2.
        assert!(
            a.additional
                .iter()
                .any(|extra| extra.concept_id == "fca:route-x:1")
        );
        assert!(
            a.additional
                .iter()
                .any(|extra| extra.concept_id == "fca:env-redis:2")
        );
    }

    #[test]
    fn mine_single_premise_finds_exception_free_rules() {
        let pairs = vec![
            ("file:a".to_string(), "kind:rust".to_string()),
            ("file:a".to_string(), "family:auth".to_string()),
            ("file:b".to_string(), "kind:rust".to_string()),
            ("file:b".to_string(), "family:audit".to_string()),
            ("file:c".to_string(), "kind:python".to_string()),
            ("file:c".to_string(), "family:audit".to_string()),
        ];
        let rules = mine_single_premise(&pairs);
        // kind:rust is carried by file:a and file:b; the only attribute they
        // share beyond it is nothing — so kind:rust entails no other rule.
        assert!(
            !rules
                .iter()
                .any(|rule| rule.premise == "kind:rust" && rule.consequent == "family:auth")
        );

        // A shared-by-all-pairs attribute entails every attribute.
        let pairs = vec![
            ("file:a".to_string(), "kind:rust".to_string()),
            ("file:a".to_string(), "root:repo".to_string()),
            ("file:b".to_string(), "kind:rust".to_string()),
            ("file:b".to_string(), "root:repo".to_string()),
        ];
        let rules = mine_single_premise(&pairs);
        assert!(
            rules
                .iter()
                .any(|rule| rule.premise == "kind:rust" && rule.consequent == "root:repo")
        );
        assert!(
            rules
                .iter()
                .any(|rule| rule.premise == "root:repo" && rule.consequent == "kind:rust")
        );
        let rule = rules
            .iter()
            .find(|rule| rule.premise == "kind:rust" && rule.consequent == "root:repo")
            .expect("rule");
        assert_eq!(rule.support, 2);
        assert_eq!(rule.confidence, 1.0);
        // With an extent of 2, every non-empty subset still carries the
        // consequent, so stability is exact 1.0.
        assert!((rule.stability - 1.0).abs() < f64::EPSILON);
    }

    /// Integration smoke: verifies induct() runs natively over a small context.
    #[test]
    fn induct_small_context_natively() {
        let pairs = vec![
            ("file:a".to_string(), "env:REDIS".to_string()),
            ("file:a".to_string(), "route:/x".to_string()),
            ("file:b".to_string(), "env:REDIS".to_string()),
            ("file:b".to_string(), "cartridge:revops".to_string()),
            ("file:c".to_string(), "route:/x".to_string()),
        ];
        let lattice = induct(&pairs).expect("native induction succeeds");
        assert_eq!(lattice.object_count, 3);
        assert_eq!(lattice.attribute_count, 3);
        assert_eq!(lattice.concept_count, 6);
        assert_eq!(lattice.edge_count, 7);
        assert_eq!(lattice.depth, 4);
        assert!(!lattice.links.is_empty(), "cover edges present");

        let memberships = derive_memberships(&lattice, MembershipOptions::default());
        assert_eq!(memberships.len(), 3);
        assert!(
            memberships["file:a"]
                .primary_concept_label
                .contains("REDIS")
        );
    }

    #[test]
    fn native_induct_verifies_lattice_invariants_and_cover() {
        let pairs = vec![
            ("file:a".to_string(), "env:REDIS".to_string()),
            ("file:a".to_string(), "route:/x".to_string()),
            ("file:b".to_string(), "env:REDIS".to_string()),
            ("file:b".to_string(), "cartridge:revops".to_string()),
            ("file:c".to_string(), "route:/x".to_string()),
        ];
        let lattice = native_induct(&pairs);

        // Verify top concept is present at node 0
        let top = &lattice.concepts[0];
        assert_eq!(top.node_id, 0);
        assert_eq!(top.extent_size, 3);
        assert_eq!(top.intent_size, 0);

        // Verify bottom concept is present at the final node
        let bottom = lattice.concepts.last().expect("bottom concept");
        assert_eq!(bottom.extent_size, 0);
        assert_eq!(bottom.intent_size, 3);

        // Verify cover graph is transitivity-free:
        // for every edge u -> v, extent(u) ⊂ extent(v) and no intermediate w exists
        let extents_by_node: HashMap<usize, BTreeSet<&str>> = lattice
            .concepts
            .iter()
            .map(|c| (c.node_id, c.extent.iter().map(|s| s.as_str()).collect()))
            .collect();

        for link in &lattice.links {
            let u_ext = &extents_by_node[&link.source];
            let v_ext = &extents_by_node[&link.target];
            assert!(
                u_ext.is_subset(v_ext),
                "link source must be a subset of target"
            );
            assert_ne!(
                u_ext, v_ext,
                "link source and target extents must be strictly distinct"
            );

            // No intermediate node w such that u_ext ⊂ w_ext ⊂ v_ext
            for (w_id, w_ext) in &extents_by_node {
                if *w_id == link.source || *w_id == link.target {
                    continue;
                }
                assert!(
                    !(u_ext.is_subset(w_ext)
                        && w_ext.is_subset(v_ext)
                        && u_ext != w_ext
                        && w_ext != v_ext),
                    "cover edge {link:?} violated by intermediate node {w_id}"
                );
            }
        }
    }

    #[test]
    fn native_induct_handles_empty_and_trivial_contexts() {
        let empty: Vec<(String, String)> = Vec::new();
        let lattice = native_induct(&empty);
        assert_eq!(lattice.concept_count, 0);

        let single = vec![("file:one".to_string(), "tag:test".to_string())];
        let lattice = native_induct(&single);
        assert_eq!(lattice.object_count, 1);
        assert_eq!(lattice.attribute_count, 1);
        assert_eq!(lattice.concept_count, 1);
    }
}
