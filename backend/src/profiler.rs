//! Continuous performance profiling.
//!
//! Manual investigation of performance bottlenecks doesn't scale. This
//! module provides always-on, low-overhead profiling so regressions are
//! caught automatically instead of relying on someone noticing slowness.
//!
//! # Components
//!
//! - [`ProfilerState`] — in-memory ring buffer of recorded samples
//! - [`profile_operation`] — wraps an async operation, recording its stack
//!   label and duration (continuous profiling hook)
//! - [`generate_flamegraph`] — aggregates samples into the standard
//!   "folded stack" format consumed by flamegraph renderers
//!   (e.g. `inferno-flamegraph`, Brendan Gregg's `flamegraph.pl`)
//! - [`detect_regressions`] — compares recent sample averages per operation
//!   against a recorded baseline and flags operations that got slower by
//!   more than a configurable threshold
//!
//! # API
//!
//! - `GET /admin/profiler/samples` — recent raw samples
//! - `GET /admin/profiler/flamegraph` — folded-stack flame graph data
//! - `POST /admin/profiler/baseline` — record current averages as baseline
//! - `GET /admin/profiler/regressions` — operations that regressed vs baseline
//! - `GET /admin/profiler/overhead` — current profiler overhead % and sample rate
//!
//! Recording overhead is tracked continuously; if it exceeds
//! [`DEFAULT_OVERHEAD_THRESHOLD_PCT`] of profiled operation time, the sample
//! rate is automatically halved (down to a floor) so the profiler cannot
//! itself become a production bottleneck.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::{extract::State, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Maximum number of samples retained before the oldest are evicted.
const MAX_SAMPLES: usize = 5_000;

/// Default threshold (%) above baseline before an operation is flagged as
/// a performance regression.
const DEFAULT_REGRESSION_THRESHOLD_PCT: f64 = 20.0;

/// If the profiler's own recording overhead exceeds this percentage of the
/// time spent in profiled operations, the sample rate is automatically
/// reduced so profiling doesn't itself become a bottleneck.
const DEFAULT_OVERHEAD_THRESHOLD_PCT: f64 = 5.0;

/// Sample rate is tracked in parts-per-thousand: 1000 = sample every call.
const INITIAL_SAMPLE_RATE_PER_MILLE: u64 = 1000;

/// Floor below which the auto-throttle will not reduce the sample rate
/// further, so profiling never goes fully blind.
const MIN_SAMPLE_RATE_PER_MILLE: u64 = 10;

/// A single recorded profiling sample.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSample {
    pub operation: String,
    /// Call-stack labels, root-first (e.g. `["handler", "db", "query"]`).
    pub stack: Vec<String>,
    pub duration_ms: f64,
    pub recorded_at: DateTime<Utc>,
}

/// Aggregated performance regression finding.
#[derive(Debug, Clone, Serialize)]
pub struct RegressionFinding {
    pub operation: String,
    pub baseline_avg_ms: f64,
    pub current_avg_ms: f64,
    pub percent_change: f64,
    pub sample_count: usize,
}

/// Running totals used to compute the profiler's own overhead as a
/// percentage of profiled operation time.
#[derive(Default)]
struct OverheadTracker {
    overhead_ms_sum: f64,
    operation_ms_sum: f64,
}

/// Continuous profiling state shared across the application.
pub struct ProfilerState {
    samples: Mutex<Vec<ProfileSample>>,
    /// Recorded baseline average duration (ms) per operation.
    baseline: Mutex<HashMap<String, f64>>,
    /// Current sample rate, in parts-per-thousand (1000 = sample every call).
    /// Reduced automatically when recording overhead exceeds the configured
    /// threshold.
    sample_rate_per_mille: AtomicU64,
    /// Monotonic counter used to decide, per call, whether to sample.
    call_counter: AtomicU64,
    overhead: Mutex<OverheadTracker>,
}

impl ProfilerState {
    pub fn new() -> Self {
        Self {
            samples: Mutex::new(Vec::new()),
            baseline: Mutex::new(HashMap::new()),
            sample_rate_per_mille: AtomicU64::new(INITIAL_SAMPLE_RATE_PER_MILLE),
            call_counter: AtomicU64::new(0),
            overhead: Mutex::new(OverheadTracker::default()),
        }
    }

    /// Decide whether the current call should be sampled, given the current
    /// (possibly auto-throttled) sample rate.
    pub fn should_sample(&self) -> bool {
        let rate = self.sample_rate_per_mille.load(Ordering::Relaxed);
        if rate >= 1000 {
            return true;
        }
        let n = self.call_counter.fetch_add(1, Ordering::Relaxed);
        (n % 1000) < rate
    }

    /// Current sample rate, in parts-per-thousand.
    pub fn current_sample_rate_per_mille(&self) -> u64 {
        self.sample_rate_per_mille.load(Ordering::Relaxed)
    }

    /// Record how long recording itself took (`overhead_ms`) relative to the
    /// operation it was profiling (`operation_ms`), and auto-throttle the
    /// sample rate if the accumulated overhead exceeds the threshold.
    fn note_overhead(&self, overhead_ms: f64, operation_ms: f64) {
        let mut tracker = self.overhead.lock().unwrap();
        tracker.overhead_ms_sum += overhead_ms;
        tracker.operation_ms_sum += operation_ms;

        if tracker.operation_ms_sum > 0.0 {
            let pct = (tracker.overhead_ms_sum / tracker.operation_ms_sum) * 100.0;
            if pct > DEFAULT_OVERHEAD_THRESHOLD_PCT {
                self.throttle();
            }
        }
    }

    /// Halve the sample rate, down to `MIN_SAMPLE_RATE_PER_MILLE`.
    fn throttle(&self) {
        let current = self.sample_rate_per_mille.load(Ordering::Relaxed);
        let reduced = (current / 2).max(MIN_SAMPLE_RATE_PER_MILLE);
        self.sample_rate_per_mille.store(reduced, Ordering::Relaxed);
    }

    /// Current profiler recording overhead as a percentage of total profiled
    /// operation time, for exposure as a metric.
    pub fn current_overhead_pct(&self) -> f64 {
        let tracker = self.overhead.lock().unwrap();
        if tracker.operation_ms_sum <= 0.0 {
            return 0.0;
        }
        (tracker.overhead_ms_sum / tracker.operation_ms_sum) * 100.0
    }

    /// Record a completed profiling sample, evicting the oldest sample if
    /// the ring buffer is full.
    pub fn record(&self, sample: ProfileSample) {
        let mut samples = self.samples.lock().unwrap();
        if samples.len() >= MAX_SAMPLES {
            samples.remove(0);
        }
        samples.push(sample);
    }

    pub fn snapshot(&self) -> Vec<ProfileSample> {
        self.samples.lock().unwrap().clone()
    }

    /// Compute the mean duration per operation across all retained samples.
    pub fn averages(&self) -> HashMap<String, f64> {
        let samples = self.samples.lock().unwrap();
        let mut sums: HashMap<String, (f64, usize)> = HashMap::new();
        for s in samples.iter() {
            let entry = sums.entry(s.operation.clone()).or_insert((0.0, 0));
            entry.0 += s.duration_ms;
            entry.1 += 1;
        }
        sums.into_iter()
            .map(|(op, (sum, count))| (op, sum / count as f64))
            .collect()
    }

    /// Snapshot current per-operation averages as the new baseline.
    pub fn set_baseline_from_current(&self) -> HashMap<String, f64> {
        let averages = self.averages();
        let mut baseline = self.baseline.lock().unwrap();
        *baseline = averages.clone();
        averages
    }

    pub fn baseline_snapshot(&self) -> HashMap<String, f64> {
        self.baseline.lock().unwrap().clone()
    }

    /// Compare current averages against the recorded baseline and return
    /// operations whose average duration regressed by more than
    /// `threshold_pct` percent.
    pub fn detect_regressions(&self, threshold_pct: f64) -> Vec<RegressionFinding> {
        let baseline = self.baseline.lock().unwrap().clone();
        let current = self.averages();
        let samples = self.samples.lock().unwrap();

        let mut findings = Vec::new();
        for (operation, baseline_avg) in baseline.iter() {
            if let Some(current_avg) = current.get(operation) {
                if *baseline_avg <= 0.0 {
                    continue;
                }
                let percent_change = ((current_avg - baseline_avg) / baseline_avg) * 100.0;
                if percent_change > threshold_pct {
                    let sample_count = samples.iter().filter(|s| &s.operation == operation).count();
                    findings.push(RegressionFinding {
                        operation: operation.clone(),
                        baseline_avg_ms: *baseline_avg,
                        current_avg_ms: *current_avg,
                        percent_change,
                        sample_count,
                    });
                }
            }
        }
        findings.sort_by(|a, b| b.percent_change.partial_cmp(&a.percent_change).unwrap());
        findings
    }

    /// Aggregate samples into folded-stack flame graph format:
    /// `frame1;frame2;frame3 <count>` per line, where `<count>` is the
    /// total accumulated milliseconds spent in that exact stack, rounded
    /// to the nearest integer (flamegraph tools treat this as weight).
    pub fn flamegraph_folded(&self) -> String {
        let samples = self.samples.lock().unwrap();
        let mut folded: HashMap<String, f64> = HashMap::new();
        for s in samples.iter() {
            let key = s.stack.join(";");
            *folded.entry(key).or_insert(0.0) += s.duration_ms;
        }
        let mut lines: Vec<String> = folded
            .into_iter()
            .map(|(stack, total_ms)| format!("{stack} {}", total_ms.round() as u64))
            .collect();
        lines.sort();
        lines.join("\n")
    }
}

impl Default for ProfilerState {
    fn default() -> Self {
        Self::new()
    }
}

/// Time an async operation and record the sample under `operation`/`stack`.
///
/// This is the hook call sites use for continuous profiling, e.g.:
///
/// ```ignore
/// let result = profile_operation(&profiler, "vault.create", &["handler", "db"], || async {
///     do_work().await
/// }).await;
/// ```
pub async fn profile_operation<F, Fut, T>(
    state: &ProfilerState,
    operation: &str,
    stack: &[&str],
    f: F,
) -> T
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let start = Instant::now();
    let result = f().await;
    let duration_ms = start.elapsed().as_secs_f64() * 1000.0;

    if state.should_sample() {
        let record_start = Instant::now();
        state.record(ProfileSample {
            operation: operation.to_string(),
            stack: stack.iter().map(|s| s.to_string()).collect(),
            duration_ms,
            recorded_at: Utc::now(),
        });
        let overhead_ms = record_start.elapsed().as_secs_f64() * 1000.0;
        state.note_overhead(overhead_ms, duration_ms);
    }

    result
}

// ── HTTP handlers ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RegressionQuery {
    pub threshold_pct: Option<f64>,
}

/// `GET /admin/profiler/samples` — recent raw profiling samples.
pub async fn list_samples(State(state): State<Arc<ProfilerState>>) -> Json<Vec<ProfileSample>> {
    Json(state.snapshot())
}

/// `GET /admin/profiler/flamegraph` — folded-stack flame graph data.
pub async fn get_flamegraph(State(state): State<Arc<ProfilerState>>) -> String {
    state.flamegraph_folded()
}

/// `POST /admin/profiler/baseline` — snapshot current averages as baseline.
pub async fn set_baseline(State(state): State<Arc<ProfilerState>>) -> Json<HashMap<String, f64>> {
    Json(state.set_baseline_from_current())
}

/// `GET /admin/profiler/regressions` — operations that regressed vs baseline.
pub async fn get_regressions(
    State(state): State<Arc<ProfilerState>>,
    axum::extract::Query(query): axum::extract::Query<RegressionQuery>,
) -> Json<Vec<RegressionFinding>> {
    let threshold = query
        .threshold_pct
        .unwrap_or(DEFAULT_REGRESSION_THRESHOLD_PCT);
    Json(state.detect_regressions(threshold))
}

/// Current profiler recording overhead, exposed for dashboards/alerting.
#[derive(Debug, Serialize)]
pub struct OverheadReport {
    /// Recording overhead as a percentage of total profiled operation time.
    pub overhead_pct: f64,
    /// Current sample rate, in parts-per-thousand (1000 = every call).
    pub sample_rate_per_mille: u64,
}

/// `GET /admin/profiler/overhead` — current profiler overhead and sample rate.
pub async fn get_overhead(State(state): State<Arc<ProfilerState>>) -> Json<OverheadReport> {
    Json(OverheadReport {
        overhead_pct: state.current_overhead_pct(),
        sample_rate_per_mille: state.current_sample_rate_per_mille(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(op: &str, ms: f64) -> ProfileSample {
        ProfileSample {
            operation: op.to_string(),
            stack: vec!["handler".to_string(), op.to_string()],
            duration_ms: ms,
            recorded_at: Utc::now(),
        }
    }

    #[test]
    fn averages_are_computed_per_operation() {
        let state = ProfilerState::new();
        state.record(sample("vault.create", 10.0));
        state.record(sample("vault.create", 20.0));
        let averages = state.averages();
        assert_eq!(averages.get("vault.create"), Some(&15.0));
    }

    #[test]
    fn regression_detected_above_threshold() {
        let state = ProfilerState::new();
        state.record(sample("vault.create", 10.0));
        state.set_baseline_from_current();

        state.record(sample("vault.create", 20.0));
        state.record(sample("vault.create", 20.0));

        let findings = state.detect_regressions(20.0);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].operation, "vault.create");
    }

    #[test]
    fn no_regression_below_threshold() {
        let state = ProfilerState::new();
        state.record(sample("vault.create", 10.0));
        state.set_baseline_from_current();
        state.record(sample("vault.create", 10.5));

        let findings = state.detect_regressions(20.0);
        assert!(findings.is_empty());
    }

    #[test]
    fn flamegraph_folds_matching_stacks() {
        let state = ProfilerState::new();
        state.record(sample("vault.create", 10.0));
        state.record(sample("vault.create", 5.0));
        let folded = state.flamegraph_folded();
        assert!(folded.contains("handler;vault.create 15"));
    }

    #[test]
    fn sample_rate_starts_at_full() {
        let state = ProfilerState::new();
        assert_eq!(state.current_sample_rate_per_mille(), 1000);
        assert!(state.should_sample());
    }

    #[test]
    fn overhead_above_threshold_throttles_sample_rate() {
        let state = ProfilerState::new();
        // Recording cost is 10% of operation time, well above the 5% default
        // threshold, so the sample rate should be reduced.
        state.note_overhead(1.0, 10.0);
        assert!(state.current_sample_rate_per_mille() < 1000);
    }

    #[test]
    fn overhead_below_threshold_does_not_throttle() {
        let state = ProfilerState::new();
        // Recording cost is 1% of operation time, below the 5% threshold.
        state.note_overhead(0.1, 10.0);
        assert_eq!(state.current_sample_rate_per_mille(), 1000);
    }

    #[test]
    fn repeated_high_overhead_throttles_toward_floor() {
        let state = ProfilerState::new();
        for _ in 0..20 {
            state.note_overhead(5.0, 10.0);
        }
        assert_eq!(
            state.current_sample_rate_per_mille(),
            MIN_SAMPLE_RATE_PER_MILLE
        );
    }

    #[test]
    fn current_overhead_pct_reflects_accumulated_ratio() {
        let state = ProfilerState::new();
        state.note_overhead(1.0, 10.0);
        state.note_overhead(1.0, 10.0);
        assert!((state.current_overhead_pct() - 10.0).abs() < 1e-9);
    }
}
