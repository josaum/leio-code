//! SIGReg sliced Epps–Pulley isotropy detector, ported from example's
//! `evolution/isotropy.py` (Klindt et al. — SIGReg-trained encoders are
//! isotropic; GEPA selection concentrates the population). Runtime check on
//! the bus embedding population, dependency-free and deterministic.
use crate::gepa::SplitMix64;

/// Calibrated decision boundary: <= ~0.57 isotropic, >= ~2.7 collapsed.
pub const DEFAULT_ISOTROPY_THRESHOLD: f32 = 2.0;

/// Closed-form Epps–Pulley statistic of a 1-D sample against N(0,1).
pub fn epps_pulley_statistic(z: &[f32]) -> f32 {
    let n = z.len();
    if n < 3 {
        return 0.0;
    }
    let mut term_pair = 0.0_f64;
    for &zi in z {
        for &zj in z {
            let d = (zi - zj) as f64;
            term_pair += (-(d * d) / 2.0).exp();
        }
    }
    term_pair /= n as f64;
    let term_cross: f64 = z
        .iter()
        .map(|&value| {
            let value = value as f64;
            (2.0_f64).sqrt() * (-(value * value) / 4.0).exp()
        })
        .sum();
    (term_pair - term_cross + n as f64 / 3.0_f64.sqrt()) as f32
}

/// Mean sliced Epps–Pulley score of L2-normalized embeddings. Each slice
/// projects onto a random unit direction u and tests z = sqrt(d) * (X @ u)
/// against N(0,1).
pub fn sphere_isotropy_score(embeddings: &[Vec<f32>], n_slices: usize, seed: u64) -> f32 {
    let n = embeddings.len();
    if n < 3 {
        return 0.0;
    }
    let d = embeddings[0].len();
    if d == 0 {
        return 0.0;
    }
    // L2-normalize rows.
    let x: Vec<Vec<f32>> = embeddings
        .iter()
        .map(|row| {
            let norm = row.iter().map(|v| v * v).sum::<f32>().sqrt();
            if norm > 0.0 {
                row.iter().map(|v| v / norm).collect()
            } else {
                row.clone()
            }
        })
        .collect();
    let mut rng = SplitMix64::new(seed);
    let mut total = 0.0_f64;
    for _ in 0..n_slices {
        let mut u: Vec<f32> = (0..d).map(|_| rng.next_normal()).collect();
        let unorm = u.iter().map(|v| v * v).sum::<f32>().sqrt();
        if unorm > 0.0 {
            for value in &mut u {
                *value /= unorm;
            }
        }
        let z: Vec<f32> = x
            .iter()
            .map(|row| {
                let dot: f32 = row.iter().zip(&u).map(|(a, b)| a * b).sum();
                (d as f32).sqrt() * dot
            })
            .collect();
        total += epps_pulley_statistic(&z) as f64;
    }
    (total / n_slices as f64) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spread_population(n: usize, dim: usize, seed: u64) -> Vec<Vec<f32>> {
        let mut rng = SplitMix64::new(seed);
        (0..n)
            .map(|_| (0..dim).map(|_| rng.next_normal()).collect())
            .collect()
    }

    #[test]
    fn collapsed_population_scores_high() {
        // All rows identical → directional collapse.
        let collapsed: Vec<Vec<f32>> = (0..32).map(|_| vec![1.0, 0.0, 0.0, 0.0]).collect();
        let spread = spread_population(32, 4, 7);
        let collapsed_score = sphere_isotropy_score(&collapsed, 64, 0);
        let spread_score = sphere_isotropy_score(&spread, 64, 0);
        assert!(
            collapsed_score > spread_score + 1.0,
            "collapsed {collapsed_score} should clearly exceed spread {spread_score}"
        );
        assert!(collapsed_score > DEFAULT_ISOTROPY_THRESHOLD);
    }

    #[test]
    fn deterministic_per_seed() {
        let population = spread_population(16, 8, 3);
        assert_eq!(
            sphere_isotropy_score(&population, 32, 42),
            sphere_isotropy_score(&population, 32, 42)
        );
    }

    #[test]
    fn empty_and_tiny_populations_score_zero() {
        assert_eq!(sphere_isotropy_score(&[], 16, 0), 0.0);
        assert_eq!(sphere_isotropy_score(&[vec![1.0]], 16, 0), 0.0);
    }
}
