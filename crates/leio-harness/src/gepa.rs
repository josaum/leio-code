//! GEPA-style vector-native evolution for agent intents, ported from
//! example's `EmbeddingGEPA` (example-api/.../evolution/gepa.py) into
//! dependency-free f32 Rust. Used by the bus/orchestrator to merge and mutate
//! agent intent embeddings without ever leaving embedding space.
//!
//! - `slerp`: spherical interpolation for merging two agent intents.
//! - `trust_region_mutate`: exploration bounded to a max angle from the parent.
//! - `anchor_penalty`: semantic-anchor drift penalty (prevents mode collapse).
//! - `ParetoFrontier`: multi-objective dominance archive (reward, cost, latency…).
//! - Deterministic by construction: SplitMix64 + Box–Muller, no external RNG.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub fn normalize(vector: &mut [f32]) {
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in vector.iter_mut() {
            *value /= norm;
        }
    }
}

pub fn dot(left: &[f32], right: &[f32]) -> f32 {
    left.iter().zip(right).map(|(a, b)| a * b).sum()
}

/// Spherical linear interpolation between two vectors (normalized inside).
pub fn slerp(v0: &[f32], v1: &[f32], t: f32) -> Vec<f32> {
    assert_eq!(v0.len(), v1.len(), "slerp dimension mismatch");
    let mut a = v0.to_vec();
    let mut b = v1.to_vec();
    normalize(&mut a);
    normalize(&mut b);
    let dot = dot(&a, &b).clamp(-1.0, 1.0);
    let omega = dot.acos();
    let sin_omega = omega.sin();
    // sin(omega) ~ 0 for both parallel (omega ~ 0) and antipodal (omega ~ pi)
    // vectors; the spherical formula is ill-conditioned there. Fall back to
    // renormalized linear interpolation (still anchors t=0/1).
    let mut out = if sin_omega.abs() < 1e-6 {
        a.iter()
            .zip(&b)
            .map(|(x, y)| (1.0 - t) * x + t * y)
            .collect::<Vec<_>>()
    } else {
        a.iter()
            .zip(&b)
            .map(|(x, y)| {
                ((1.0 - t) * omega).sin() / sin_omega * x + (t * omega).sin() / sin_omega * y
            })
            .collect::<Vec<_>>()
    };
    normalize(&mut out);
    out
}

/// Deterministic RNG: SplitMix64 bit stream + Box–Muller for gaussians.
#[derive(Debug, Clone)]
pub struct SplitMix64 {
    state: u64,
    cached_normal: Option<f32>,
}

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self {
            state: seed,
            cached_normal: None,
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn next_f32_open01(&mut self) -> f32 {
        ((self.next_u64() >> 40) as f32 + 0.5) / 16_777_216.0
    }

    pub fn next_normal(&mut self) -> f32 {
        if let Some(cached) = self.cached_normal.take() {
            return cached;
        }
        let u1 = self.next_f32_open01();
        let u2 = self.next_f32_open01();
        let radius = (-2.0 * u1.ln()).sqrt();
        let angle = 2.0 * std::f32::consts::PI * u2;
        self.cached_normal = Some(radius * angle.sin());
        radius * angle.cos()
    }
}

/// Gradient-free mutation bounded to `max_angle_deg` from the parent vector:
/// sample gaussian noise, strip the radial component (tangent noise), scale by
/// `strength`, add, renormalize, then project back onto the sphere cap if the
/// result exceeds the trust angle.
pub fn trust_region_mutate(
    parent: &[f32],
    strength: f32,
    max_angle_deg: f32,
    rng: &mut SplitMix64,
) -> Vec<f32> {
    let mut base = parent.to_vec();
    normalize(&mut base);
    if base.iter().all(|value| value.abs() < 1e-6) {
        // Degenerate (zero) intent: no trust region is defined around it.
        return parent.to_vec();
    }
    let mut noise: Vec<f32> = base.iter().map(|_| rng.next_normal()).collect();
    let radial = dot(&noise, &base);
    for (n, b) in noise.iter_mut().zip(&base) {
        *n -= radial * b;
    }
    normalize(&mut noise);
    let mut child: Vec<f32> = base
        .iter()
        .zip(&noise)
        .map(|(b, n)| b + strength * n)
        .collect();
    normalize(&mut child);
    if max_angle_deg > 0.0 {
        let angle = dot(&base, &child).clamp(-1.0, 1.0).acos().to_degrees();
        if angle > max_angle_deg {
            let t = max_angle_deg / angle;
            child = slerp(&base, &child, t);
        }
    }
    child
}

/// Semantic anchor penalty: R_adj = R - beta * (1 - cos(x, x_seed)).
/// Keeps evolved intents semantically near the human's original goal.
pub fn anchor_penalty(vector: &[f32], seed_embedding: &[f32], beta: f32) -> f32 {
    let mut v = vector.to_vec();
    let mut s = seed_embedding.to_vec();
    normalize(&mut v);
    normalize(&mut s);
    beta * (1.0 - dot(&v, &s))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub vector: Vec<f32>,
    pub scores: BTreeMap<String, f32>,
    pub evaluations: u32,
}

impl Candidate {
    /// Multi-objective dominance: all objectives >= and at least one strictly >.
    pub fn dominates(&self, other: &Candidate) -> bool {
        let mut strictly_better = false;
        for (objective, value) in &self.scores {
            let other_value = other
                .scores
                .get(objective)
                .copied()
                .unwrap_or(f32::NEG_INFINITY);
            if *value < other_value {
                return false;
            }
            if *value > other_value {
                strictly_better = true;
            }
        }
        strictly_better
    }

    pub fn aggregate(&self) -> f32 {
        if self.scores.is_empty() {
            return f32::NEG_INFINITY;
        }
        self.scores.values().sum::<f32>() / self.scores.len() as f32
    }
}

#[derive(Debug, Default)]
pub struct ParetoFrontier {
    candidates: BTreeMap<String, Candidate>,
}

impl ParetoFrontier {
    pub fn add(&mut self, candidate: Candidate) -> bool {
        for existing in self.candidates.values() {
            if existing.dominates(&candidate) {
                return false;
            }
        }
        let dominated: Vec<String> = self
            .candidates
            .values()
            .filter(|existing| candidate.dominates(existing))
            .map(|existing| existing.id.clone())
            .collect();
        for id in dominated {
            self.candidates.remove(&id);
        }
        self.candidates.insert(candidate.id.clone(), candidate);
        true
    }

    pub fn candidates(&self) -> Vec<&Candidate> {
        self.candidates.values().collect()
    }

    pub fn select_least_explored(&self) -> Option<&Candidate> {
        self.candidates
            .values()
            .min_by_key(|candidate| candidate.evaluations)
    }

    pub fn select_best_aggregate(&self) -> Option<&Candidate> {
        self.candidates.values().max_by(|left, right| {
            left.aggregate()
                .partial_cmp(&right.aggregate())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(v: &[f32]) -> f32 {
        v.iter().map(|x| x * x).sum::<f32>().sqrt()
    }

    #[test]
    fn slerp_endpoints_and_unit_norm() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let mid = slerp(&a, &b, 0.5);
        assert!((norm(&mid) - 1.0).abs() < 1e-6);
        assert!((dot(&mid, &a) - dot(&mid, &b)).abs() < 1e-6);
        let start = slerp(&a, &b, 0.0);
        assert!((dot(&start, &a) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn trust_region_respects_max_angle() {
        let parent = vec![1.0, 0.0, 0.0, 0.0];
        let mut rng = SplitMix64::new(42);
        for _ in 0..64 {
            let child = trust_region_mutate(&parent, 0.5, 5.0, &mut rng);
            let angle = dot(&parent, &child).clamp(-1.0, 1.0).acos().to_degrees();
            assert!(angle <= 5.0 + 1e-3, "angle {angle} exceeded trust region");
            assert!((norm(&child) - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn mutation_is_deterministic_per_seed() {
        let parent = vec![0.5, 0.5, 0.5, 0.5];
        let mut first = SplitMix64::new(7);
        let mut second = SplitMix64::new(7);
        assert_eq!(
            trust_region_mutate(&parent, 0.2, 0.0, &mut first),
            trust_region_mutate(&parent, 0.2, 0.0, &mut second)
        );
    }

    #[test]
    fn anchor_penalty_rewards_alignment() {
        let seed = vec![1.0, 0.0];
        let mut same = vec![2.0, 0.0];
        normalize(&mut same);
        let opposite = vec![-1.0, 0.0];
        assert!(anchor_penalty(&same, &seed, 0.5) < 1e-6);
        assert!(anchor_penalty(&opposite, &seed, 0.5) > 0.9);
    }

    #[test]
    fn pareto_frontier_tracks_dominance() {
        let mut frontier = ParetoFrontier::default();
        let candidate = |id: &str, reward: f32, speed: f32| Candidate {
            id: id.to_owned(),
            vector: vec![1.0],
            scores: BTreeMap::from([("reward".to_owned(), reward), ("speed".to_owned(), speed)]),
            evaluations: 0,
        };
        assert!(frontier.add(candidate("a", 0.9, 0.1)));
        assert!(frontier.add(candidate("b", 0.8, 0.5))); // non-dominated tradeoff
        assert!(!frontier.add(candidate("c", 0.7, 0.2))); // dominated by b
        assert_eq!(frontier.candidates().len(), 2);
        assert!(frontier.add(candidate("d", 0.95, 0.6))); // dominates both
        assert_eq!(frontier.candidates().len(), 1);
        assert_eq!(frontier.select_best_aggregate().unwrap().id, "d");
    }
}

#[cfg(test)]
mod property_tests {
    use super::*;
    use proptest::prelude::*;

    fn any_vector(dim: std::ops::RangeInclusive<usize>) -> impl Strategy<Value = Vec<f32>> {
        prop::collection::vec(-10.0f32..10.0f32, dim)
    }

    proptest! {
        #[test]
        fn slerp_output_is_unit_and_endpoints_anchor(
            a in any_vector(1..=16),
            b in any_vector(1..=16),
            t in 0.0f32..=1.0f32,
        ) {
            let (a, b) = if a.len() == b.len() { (a, b) } else { (a.clone(), a.clone()) };
            let (mut na, mut nb) = (a.clone(), b.clone());
            normalize(&mut na);
            normalize(&mut nb);
            let out = slerp(&a, &b, t);
            prop_assert!((dot(&out, &out) - 1.0).abs() < 1e-4);
            if t == 0.0 { prop_assert!((dot(&out, &na) - 1.0).abs() < 1e-3); }
            if t == 1.0 { prop_assert!((dot(&out, &nb) - 1.0).abs() < 1e-3); }
        }

        #[test]
        fn mutation_stays_inside_trust_region(
            parent in any_vector(2..=32),
            strength in 0.01f32..1.0f32,
            max_angle in 0.1f32..60.0f32,
            seed in 0u64..u64::MAX,
        ) {
            prop_assume!(parent.iter().map(|v| v * v).sum::<f32>() > 1e-3);
            let mut rng = SplitMix64::new(seed);
            let child = trust_region_mutate(&parent, strength, max_angle, &mut rng);
            prop_assert_eq!(child.len(), parent.len());
            prop_assert!((dot(&child, &child) - 1.0).abs() < 1e-4);
            let mut normalized_parent = parent.clone();
            normalize(&mut normalized_parent);
            let angle = dot(&normalized_parent, &child).clamp(-1.0, 1.0).acos().to_degrees();
            prop_assert!(angle <= max_angle + 1e-2);
        }

        #[test]
        fn anchor_penalty_is_bounded(
            v in any_vector(1..=16),
            s in any_vector(1..=16),
            beta in 0.0f32..10.0f32,
        ) {
            let (v, s) = if v.len() == s.len() { (v, s) } else { (v.clone(), v.clone()) };
            let p = anchor_penalty(&v, &s, beta);
            prop_assert!(p >= -1e-4 && p <= 2.0 * beta + 1e-4);
        }
    }
}
