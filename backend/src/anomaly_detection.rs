//! Anomaly detection for the API server.
//!
//! Computes a rolling [`Baseline`] of request metrics and raises [`Severity`]
//! alerts when observed values deviate from that baseline.
//!
//! ## Baseline drift handling
//!
//! A legitimate, sustained traffic shift (e.g. a new product launch) must not
//! keep alerting forever against a stale baseline. To handle this, the baseline
//! is updated with a *slow-moving* adaptation: only samples that are not
//! themselves anomalous contribute to the baseline, and the baseline moves
//! toward the observed value gradually so that transient spikes are ignored
//! while sustained shifts are absorbed over time.
//!
//! See `docs/anomaly-detection.md` for the full description of the behavior.

use std::collections::VecDeque;

/// Number of samples kept in the rolling window used to compute the baseline.
const WINDOW_SIZE: usize = 60;

/// Weight applied to a new (non-anomalous) sample when adapting the baseline.
///
/// A small value makes the baseline slow-moving: a single sample barely moves
/// it, but a sustained shift accumulates and is eventually absorbed.
const BASELINE_ADAPTATION_RATE: f64 = 0.05;

/// A sample is considered anomalous (and therefore excluded from baseline
/// adaptation) when it deviates from the baseline by more than this many
/// standard deviations.
const ANOMALY_SIGMA_THRESHOLD: f64 = 3.0;

/// Severity of an alert raised against the baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Observed value is within normal bounds.
    Ok,
    /// Observed value deviates moderately from the baseline.
    Warning,
    /// Observed value deviates strongly from the baseline.
    Critical,
}

/// Rolling baseline used to evaluate incoming metric samples.
#[derive(Debug, Clone)]
pub struct Baseline {
    /// Current baseline mean.
    mean: f64,
    /// Current baseline standard deviation.
    stddev: f64,
    /// Recent samples, used to seed the baseline and track variance.
    window: VecDeque<f64>,
}

impl Baseline {
    /// Creates a new baseline seeded with an initial value.
    pub fn new(initial: f64) -> Self {
        let mut window = VecDeque::with_capacity(WINDOW_SIZE);
        window.push_back(initial);
        Self {
            mean: initial,
            stddev: 0.0,
            window,
        }
    }

    /// Returns the current baseline mean.
    pub fn mean(&self) -> f64 {
        self.mean
    }

    /// Returns the current baseline standard deviation.
    pub fn stddev(&self) -> f64 {
        self.stddev
    }

    /// Evaluates a sample against the baseline and returns its severity.
    ///
    /// This does not mutate the baseline; call [`Baseline::observe`] to feed
    /// the sample into the rolling window and adaptation logic.
    pub fn severity(&self, sample: f64) -> Severity {
        let deviation = (sample - self.mean).abs();
        if self.stddev == 0.0 {
            return if deviation == 0.0 {
                Severity::Ok
            } else {
                Severity::Critical
            };
        }
        let sigmas = deviation / self.stddev;
        if sigmas >= ANOMALY_SIGMA_THRESHOLD {
            Severity::Critical
        } else if sigmas >= ANOMALY_SIGMA_THRESHOLD / 2.0 {
            Severity::Warning
        } else {
            Severity::Ok
        }
    }

    /// Records a sample, updating the rolling window and adapting the baseline.
    ///
    /// Transient spikes (samples that are themselves anomalous) are recorded in
    /// the window but do **not** move the baseline. Non-anomalous samples nudge
    /// the baseline toward the observed value by [`BASELINE_ADAPTATION_RATE`],
    /// so a sustained shift is absorbed gradually while a one-off spike is not.
    pub fn observe(&mut self, sample: f64) {
        let severity = self.severity(sample);

        self.window.push_back(sample);
        if self.window.len() > WINDOW_SIZE {
            self.window.pop_front();
        }

        // Only sustained (non-anomalous) samples adapt the baseline. Anomalous
        // samples are treated as transient spikes and leave the baseline alone.
        if severity == Severity::Ok {
            self.mean += BASELINE_ADAPTATION_RATE * (sample - self.mean);
        }

        self.recompute_stddev();
    }

    /// Recomputes the baseline standard deviation from the rolling window.
    fn recompute_stddev(&mut self) {
        if self.window.len() < 2 {
            self.stddev = 0.0;
            return;
        }
        let n = self.window.len() as f64;
        let variance = self
            .window
            .iter()
            .map(|value| {
                let diff = value - self.mean;
                diff * diff
            })
            .sum::<f64>()
            / n;
        self.stddev = variance.sqrt();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_spike_does_not_move_baseline() {
        let mut baseline = Baseline::new(100.0);
        // Seed a little variance so the spike is clearly anomalous.
        for _ in 0..10 {
            baseline.observe(100.0);
        }
        let before = baseline.mean();

        // A single large spike should be flagged and must not adapt the baseline.
        assert_eq!(baseline.severity(500.0), Severity::Critical);
        baseline.observe(500.0);

        assert!(
            (baseline.mean() - before).abs() < 1e-9,
            "transient spike should not move the baseline (before={before}, after={})",
            baseline.mean()
        );
    }

    #[test]
    fn sustained_shift_adapts_baseline() {
        let mut baseline = Baseline::new(100.0);
        for _ in 0..10 {
            baseline.observe(100.0);
        }

        // Simulate a sustained, gradual shift to a new traffic level. Each step
        // is small enough to be non-anomalous, so the baseline should follow.
        let target = 150.0;
        for _ in 0..200 {
            baseline.observe(target);
        }

        assert!(
            (baseline.mean() - target).abs() < 5.0,
            "baseline should adapt toward the sustained shift (mean={})",
            baseline.mean()
        );
    }

    #[test]
    fn sustained_shift_stops_alerting_once_absorbed() {
        let mut baseline = Baseline::new(100.0);
        for _ in 0..10 {
            baseline.observe(100.0);
        }

        // Gradually ramp to the new level so samples stay non-anomalous.
        let target = 150.0;
        let mut value = 100.0;
        while value < target {
            value += 1.0;
            baseline.observe(value);
        }
        for _ in 0..200 {
            baseline.observe(target);
        }

        // Once the baseline has adapted, the sustained level is no longer an alert.
        assert_eq!(baseline.severity(target), Severity::Ok);
    }
}
