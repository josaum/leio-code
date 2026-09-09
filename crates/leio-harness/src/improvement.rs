//! Improvement gate: collaboration must provably beat a single-agent baseline
//! before a merge is allowed. Pure, deterministic, evidence-producing.
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutcomeSnapshot {
    /// Metrics. Direction defaults to higher-is-better; list a metric in
    /// `minimize` to make it lower-is-better (cost, latency, ...).
    pub metrics: BTreeMap<String, f64>,
    /// Metrics where a lower value is better.
    #[serde(default)]
    pub minimize: Vec<String>,
    /// SIGReg isotropy score of the collab population (lower = healthier).
    #[serde(default)]
    pub isotropy: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImprovementVerdict {
    Improved,
    NoImprovement,
    Regressed,
    Collapsed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImprovementReport {
    pub verdict: ImprovementVerdict,
    pub baseline: BTreeMap<String, f64>,
    pub collab: BTreeMap<String, f64>,
    pub deltas: BTreeMap<String, f64>,
    pub isotropy: Option<f32>,
    pub isotropy_threshold: f32,
    pub evidence_sha256: String,
    pub reasons: Vec<String>,
}

/// Epsilon below which metric differences count as "equal" (noise floor).
const DELTA_EPSILON: f64 = 1e-6;

/// Evaluate whether a collaborative outcome improves on a single-agent
/// baseline. `isotropy_threshold` is the SIGReg collapse boundary (2.0).
pub fn evaluate_improvement(
    baseline: &OutcomeSnapshot,
    collab: &OutcomeSnapshot,
    isotropy_threshold: f32,
) -> Result<ImprovementReport> {
    let mut reasons = Vec::new();
    let mut deltas = BTreeMap::new();
    let mut collab_output = BTreeMap::new();
    let mut baseline_output = BTreeMap::new();

    for metric in baseline.metrics.keys().chain(collab.metrics.keys()) {
        let b = baseline
            .metrics
            .get(metric)
            .copied()
            .unwrap_or(f64::NEG_INFINITY);
        let c = collab
            .metrics
            .get(metric)
            .copied()
            .unwrap_or(f64::NEG_INFINITY);
        baseline_output.insert(metric.clone(), b);
        collab_output.insert(metric.clone(), c);
        let minimize = collab.minimize.contains(metric) || baseline.minimize.contains(metric);
        // delta is positive when collab is BETTER than baseline, in either direction.
        deltas.insert(metric.clone(), if minimize { b - c } else { c - b });
    }

    // Collapse gate first: an isotropic-Gaussian-violating population cannot
    // claim a trustworthy improvement (SIGReg guarantee void).
    let mut verdict = ImprovementVerdict::Improved;
    if let Some(isotropy) = collab.isotropy
        && isotropy > isotropy_threshold
    {
        verdict = ImprovementVerdict::Collapsed;
        reasons.push(format!(
            "collab isotropy {isotropy:.3} exceeds threshold {isotropy_threshold:.3}"
        ));
    }

    if verdict != ImprovementVerdict::Collapsed {
        let any_regressed = deltas.values().any(|delta| *delta < -DELTA_EPSILON);
        let any_improved = deltas.values().any(|delta| *delta > DELTA_EPSILON);
        if any_regressed {
            verdict = ImprovementVerdict::Regressed;
            let worst = deltas
                .iter()
                .filter(|(_, d)| **d < -DELTA_EPSILON)
                .min_by(|(_, a), (_, b)| a.total_cmp(b))
                .map(|(k, v)| (k.clone(), *v));
            if let Some((metric, delta)) = worst {
                reasons.push(format!("{metric} regressed by {delta:.4}"));
            }
        } else if !any_improved {
            verdict = ImprovementVerdict::NoImprovement;
            reasons.push("no metric strictly improved over baseline".to_owned());
        }
    }

    let mut evidence = serde_json::to_vec(&serde_json::json!({
        "baseline": baseline,
        "collab": collab,
        "isotropy_threshold": isotropy_threshold,
    }))?;
    evidence.extend_from_slice(&[0]);
    let mut digest = Sha256::new();
    digest.update(&evidence);
    let evidence_sha256 = hex::encode(digest.finalize());

    Ok(ImprovementReport {
        verdict,
        baseline: baseline_output,
        collab: collab_output,
        deltas,
        isotropy: collab.isotropy,
        isotropy_threshold,
        evidence_sha256,
        reasons,
    })
}

/// Parse a metric snapshot from a JSON file or inline JSON string.
pub fn parse_snapshot(value: &str) -> Result<OutcomeSnapshot> {
    let trimmed = value.trim();
    let parsed: OutcomeSnapshot = if trimmed.starts_with('{') {
        serde_json::from_str(trimmed)?
    } else {
        serde_json::from_slice(&std::fs::read(trimmed)?)?
    };
    validate_snapshot(parsed)
}

/// Parse an `OutcomeSnapshot` from captured objective stdout: try the whole
/// buffer as JSON first, then scan lines from the end for the first line that
/// deserializes as a snapshot carrying at least one metric (tolerates log
/// noise around the emitted snapshot).
pub fn parse_snapshot_from_stdout(text: &str) -> Result<OutcomeSnapshot> {
    let trimmed = text.trim();
    if let Ok(snapshot) = serde_json::from_str::<OutcomeSnapshot>(trimmed)
        && !snapshot.metrics.is_empty()
    {
        return validate_snapshot(snapshot);
    }
    for line in trimmed.lines().rev() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        if let Ok(snapshot) = serde_json::from_str::<OutcomeSnapshot>(line)
            && !snapshot.metrics.is_empty()
        {
            return validate_snapshot(snapshot);
        }
    }
    bail!("no outcome snapshot found in objective stdout");
}

fn validate_snapshot(snapshot: OutcomeSnapshot) -> Result<OutcomeSnapshot> {
    if snapshot.metrics.is_empty() {
        bail!("snapshot must contain at least one metric");
    }
    for (metric, value) in &snapshot.metrics {
        if !value.is_finite() {
            bail!("metric {metric} must be finite");
        }
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(metrics: &[(&str, f64)], isotropy: Option<f32>) -> OutcomeSnapshot {
        snap_min(metrics, &[], isotropy)
    }

    fn snap_min(
        metrics: &[(&str, f64)],
        minimize: &[&str],
        isotropy: Option<f32>,
    ) -> OutcomeSnapshot {
        OutcomeSnapshot {
            metrics: metrics.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            minimize: minimize.iter().map(|m| m.to_string()).collect(),
            isotropy,
        }
    }

    #[test]
    fn improved_when_all_metrics_at_least_equal_and_one_strictly_better() {
        let base = snap_min(
            &[("quality", 0.8), ("latency_ms", 100.0)],
            &["latency_ms"],
            Some(0.5),
        );
        let collab = snap_min(
            &[("quality", 0.9), ("latency_ms", 90.0)],
            &["latency_ms"],
            Some(0.4),
        );
        let report = evaluate_improvement(&base, &collab, 2.0).unwrap();
        assert_eq!(report.verdict, ImprovementVerdict::Improved);
        assert_eq!(report.evidence_sha256.len(), 64);
    }

    #[test]
    fn no_improvement_when_identical() {
        let base = snap(&[("quality", 0.8)], Some(0.5));
        let collab = snap(&[("quality", 0.8)], Some(0.5));
        let report = evaluate_improvement(&base, &collab, 2.0).unwrap();
        assert_eq!(report.verdict, ImprovementVerdict::NoImprovement);
    }

    #[test]
    fn regressed_when_any_metric_drops() {
        let base = snap_min(&[("quality", 0.8), ("cost", 1.0)], &["cost"], Some(0.5));
        let collab = snap_min(&[("quality", 0.9), ("cost", 2.0)], &["cost"], Some(0.4));
        let report = evaluate_improvement(&base, &collab, 2.0).unwrap();
        assert_eq!(report.verdict, ImprovementVerdict::Regressed);
    }

    #[test]
    fn collapsed_blocks_improvement_claim() {
        let base = snap(&[("quality", 0.8)], Some(0.5));
        let collab = snap(&[("quality", 0.95)], Some(5.0));
        let report = evaluate_improvement(&base, &collab, 2.0).unwrap();
        assert_eq!(report.verdict, ImprovementVerdict::Collapsed);
    }

    #[test]
    fn parse_rejects_non_finite_metrics() {
        assert!(parse_snapshot(r#"{"metrics":{"m":null}}"#).is_err());
        assert!(parse_snapshot(r#"{"metrics":{}}"#).is_err());
    }

    #[test]
    fn parse_snapshot_from_stdout_finds_snapshot_amid_logs() {
        let stdout = "compiling...\n{\"metrics\":{\"quality\":0.9},\"isotropy\":0.4}\ndone\n";
        let snapshot = parse_snapshot_from_stdout(stdout).unwrap();
        assert_eq!(snapshot.metrics["quality"], 0.9);
        assert_eq!(snapshot.isotropy, Some(0.4));
    }

    #[test]
    fn parse_snapshot_from_stdout_rejects_no_snapshot() {
        assert!(parse_snapshot_from_stdout("no metrics here\n").is_err());
    }
}
