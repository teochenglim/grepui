//! The pure decisions of an alert: the comparison, the compare-with-past value, the rolling
//! median / MAD outlier test, dedup fingerprints and the leader lease.

use super::{Anomaly, Op};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub fn breach(op: Op, value: f64, threshold: f64) -> bool {
    match op {
        Op::Gt => value > threshold,
        Op::Ge => value >= threshold,
        Op::Lt => value < threshold,
        Op::Le => value <= threshold,
        Op::Eq => (value - threshold).abs() < f64::EPSILON * value.abs().max(1.0),
        Op::Ne => (value - threshold).abs() >= f64::EPSILON * value.abs().max(1.0),
    }
}

/// The number compared with the threshold when a rule looks at the past: the ratio or the
/// difference of now against then. `None` when there is nothing to compare with (no past value,
/// or a ratio against zero).
pub fn compare_value(mode: &str, now: f64, then: Option<f64>) -> Option<f64> {
    let then = then?;
    match mode {
        "diff" => Some(now - then),
        _ if then == 0.0 => None,
        _ => Some(now / then),
    }
}

pub fn median(v: &[f64]) -> f64 {
    let mut s: Vec<f64> = v.iter().copied().filter(|x| x.is_finite()).collect();
    if s.is_empty() {
        return 0.0;
    }
    s.sort_by(|a, b| a.total_cmp(b));
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    }
}

/// Median absolute deviation.
pub fn mad(v: &[f64]) -> f64 {
    let m = median(v);
    let dev: Vec<f64> = v.iter().map(|x| (x - m).abs()).collect();
    median(&dev)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Not enough baseline points yet.
    NeedsHistory {
        have: usize,
        need: usize,
    },
    Normal {
        median: f64,
        mad: f64,
    },
    Outlier {
        median: f64,
        mad: f64,
        distance: f64,
    },
}

/// Is `value` an outlier against `baseline` (the points before it)? The baseline is the last
/// `window` points; fewer than `min_points` of them is `NeedsHistory`. A flat baseline (MAD of
/// zero) flags any change larger than a tiny fraction of the median.
pub fn judge(baseline: &[f64], value: f64, a: &Anomaly) -> Verdict {
    let start = baseline.len().saturating_sub(a.window);
    let base: Vec<f64> = baseline[start..]
        .iter()
        .copied()
        .filter(|x| x.is_finite())
        .collect();
    if base.len() < a.min_points {
        return Verdict::NeedsHistory {
            have: base.len(),
            need: a.min_points,
        };
    }
    let (m, d) = (median(&base), mad(&base));
    let floor = 1e-9 * m.abs().max(1.0);
    let limit = a.k * d.max(floor);
    let delta = value - m;
    let out = match a.direction.as_str() {
        "high" => delta > limit,
        "low" => -delta > limit,
        _ => delta.abs() > limit,
    };
    if out {
        Verdict::Outlier {
            median: m,
            mad: d,
            distance: delta / d.max(floor),
        }
    } else {
        Verdict::Normal { median: m, mad: d }
    }
}

/// Every point of a series that is an outlier against the points before it (for the preview).
pub fn flag_all(values: &[f64], a: &Anomaly) -> Vec<usize> {
    (0..values.len())
        .filter(|&i| matches!(judge(&values[..i], values[i], a), Verdict::Outlier { .. }))
        .collect()
}

/// A fingerprint of "the same alert": the rule and the chosen label values. Labels the rule does
/// not list do not matter, so two series that differ only there are one alert.
pub fn fingerprint(alert_id: &str, labels: &BTreeMap<String, String>, fields: &[String]) -> String {
    let mut h = Sha256::new();
    h.update(alert_id.as_bytes());
    let mut names: Vec<&String> = fields.iter().collect();
    names.sort();
    for f in names {
        h.update([0]);
        h.update(f.as_bytes());
        h.update([1]);
        h.update(labels.get(f).map(String::as_bytes).unwrap_or_default());
    }
    h.finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Fire,
    Cooldown,
    Duplicate,
}

/// May a breach with this fingerprint notify now? The rule-wide cooldown counts from the last
/// notification of the rule; the dedup window from the last one of this fingerprint.
pub fn gate(
    now_ms: i64,
    last_fire_ms: Option<i64>,
    cooldown_s: i64,
    last_fp_ms: Option<i64>,
    window_s: i64,
) -> Gate {
    if let Some(t) = last_fire_ms
        && now_ms - t < cooldown_s * 1000
    {
        return Gate::Cooldown;
    }
    if let Some(t) = last_fp_ms
        && now_ms - t < window_s * 1000
    {
        return Gate::Duplicate;
    }
    Gate::Fire
}

// ---- leader lease ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Lease {
    pub holder: String,
    pub expires_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseAction {
    /// Another replica holds a lease that has not expired.
    Follow,
    /// This replica holds it and it is not about to run out.
    Hold,
    /// Free, expired or about to expire: write a claim, wait, read it back.
    Claim,
}

pub fn lease_action(cur: Option<&Lease>, me: &str, now_ms: i64, ttl_ms: i64) -> LeaseAction {
    match cur {
        Some(l) if l.expires_ms > now_ms && l.holder != me => LeaseAction::Follow,
        Some(l)
            if l.expires_ms > now_ms && l.holder == me && l.expires_ms - now_ms > ttl_ms / 2 =>
        {
            LeaseAction::Hold
        }
        _ => LeaseAction::Claim,
    }
}

/// Has a real-time rule's last evaluation cost more than its budget?
pub fn over_budget(last_ms: i64, budget_ms: Option<i64>) -> bool {
    budget_ms.is_some_and(|b| last_ms > b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_and_mad() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[1.0, 2.0, 3.0, 4.0]), 2.5);
        assert_eq!(mad(&[1.0, 1.0, 2.0, 2.0, 4.0, 6.0, 9.0]), 1.0);
    }
}
