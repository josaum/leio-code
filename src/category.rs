//! Category-of-concepts used by lattice export and `nav`.
//!
//! Matches the example-align surface: objects, labeled morphisms, and a
//! structure-preserving functor check (`evaluate_functor`). The concept
//! lattice is the category; cover edges are `subClassOf` morphisms
//! (child → parent, more specific → more general). Wiki heading stacks
//! are a second category mapped into the lattice by the same check.

use std::collections::{HashMap, HashSet};

/// Labeled arrow in a category.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Morphism {
    pub source: String,
    pub target: String,
    pub label: String,
}

/// One object mapping of an alignment-as-functor.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Alignment {
    pub source_uri: String,
    pub target_uri: String,
}

/// Preservation counts for a candidate functor.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctorStats {
    pub coherence: f32,
    pub preserved_count: usize,
    pub total_count: usize,
}

/// Finite category presented by objects and generating morphisms.
pub trait Category {
    fn objects(&self) -> Vec<String>;
    fn morphisms(&self) -> Vec<Morphism>;
    fn has_direct_morphism(&self, source: &str, target: &str, label: &str) -> bool;
    fn reaches(&self, source: &str, target: &str, label: &str) -> bool;
}

/// Concept lattice as a category (cover graph).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LatticeCategory {
    pub objects: Vec<String>,
    pub morphisms: Vec<Morphism>,
}

impl Category for LatticeCategory {
    fn objects(&self) -> Vec<String> {
        self.objects.clone()
    }

    fn morphisms(&self) -> Vec<Morphism> {
        self.morphisms.clone()
    }

    fn has_direct_morphism(&self, source: &str, target: &str, label: &str) -> bool {
        self.morphisms.iter().any(|morphism| {
            morphism.source == source && morphism.target == target && morphism.label == label
        })
    }

    fn reaches(&self, source: &str, target: &str, label: &str) -> bool {
        if source == target {
            return true;
        }
        let mut seen = HashSet::new();
        let mut stack = vec![source.to_string()];
        while let Some(current) = stack.pop() {
            if !seen.insert(current.clone()) {
                continue;
            }
            for morphism in &self.morphisms {
                if morphism.source != current || morphism.label != label {
                    continue;
                }
                if morphism.target == target {
                    return true;
                }
                stack.push(morphism.target.clone());
            }
        }
        false
    }
}

/// Check whether `alignment` preserves generating morphisms (fast-align functor).
///
/// A morphism `s --label--> t` is preserved when `F(s)` reaches `F(t)` along
/// the same label, or along `subClassOf` for non-hierarchy labels.
pub fn evaluate_functor<C: Category>(
    source: &C,
    target: &C,
    alignment: &[Alignment],
) -> FunctorStats {
    let map: HashMap<String, String> = alignment
        .iter()
        .map(|row| (row.source_uri.clone(), row.target_uri.clone()))
        .collect();
    let mut preserved = 0usize;
    let mut total = 0usize;
    for morphism in source.morphisms() {
        let Some(src_prime) = map.get(&morphism.source) else {
            continue;
        };
        let Some(tgt_prime) = map.get(&morphism.target) else {
            continue;
        };
        total += 1;
        let exact = target.reaches(src_prime, tgt_prime, &morphism.label);
        let generic =
            morphism.label != "subClassOf" && target.reaches(src_prime, tgt_prime, "subClassOf");
        if exact || generic {
            preserved += 1;
        }
    }
    FunctorStats {
        coherence: if total == 0 {
            1.0
        } else {
            preserved as f32 / total as f32
        },
        preserved_count: preserved,
        total_count: total,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_functor_on_cover_is_coherent() {
        let category = LatticeCategory {
            objects: vec!["a".into(), "b".into()],
            morphisms: vec![Morphism {
                source: "a".into(),
                target: "b".into(),
                label: "subClassOf".into(),
            }],
        };
        let alignment = vec![
            Alignment {
                source_uri: "a".into(),
                target_uri: "a".into(),
            },
            Alignment {
                source_uri: "b".into(),
                target_uri: "b".into(),
            },
        ];
        let stats = evaluate_functor(&category, &category, &alignment);
        assert_eq!(stats.total_count, 1);
        assert_eq!(stats.preserved_count, 1);
        assert!((stats.coherence - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn broken_alignment_drops_coherence() {
        let source = LatticeCategory {
            objects: vec!["a".into(), "b".into()],
            morphisms: vec![Morphism {
                source: "a".into(),
                target: "b".into(),
                label: "subClassOf".into(),
            }],
        };
        let target = LatticeCategory {
            objects: vec!["x".into(), "y".into()],
            morphisms: vec![],
        };
        let alignment = vec![
            Alignment {
                source_uri: "a".into(),
                target_uri: "x".into(),
            },
            Alignment {
                source_uri: "b".into(),
                target_uri: "y".into(),
            },
        ];
        let stats = evaluate_functor(&source, &target, &alignment);
        assert_eq!(stats.preserved_count, 0);
        assert_eq!(stats.coherence, 0.0);
    }
}
