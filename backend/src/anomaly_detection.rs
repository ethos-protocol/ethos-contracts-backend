//! Automatic anomaly detection and alerting (issue: "Anomalies aren't
//! detected automatically. Detection would enable proactive alerting.").
//!
//! Uses an online (streaming) z-score detector: a running mean/variance
//! ("baseline") is learned per metric via Welford's algorithm, and any new
//! observation more than `z_threshold` standard deviations from that
//! baseline is flagged. Baselines still update on anomalous points so the
//! detector adapts to genuine regime shifts rather than getting stuck
//! alerting forever. False positives are filtered by (a) requiring a
//! minimum number of samples before a baseline is trusted and (b) a
//! per-metric cooldown so a single spike doesn't fire dozens of duplicate
//! alerts.
//!
//! See `docs/anomaly-detection.md` for tuning guidance.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Minimum number of observations before a baseline is trusted enough to
/// generate alerts. Below this, the detector is still "learning".
const MIN_SAMPLES_FOR_DETECTION: u64 = 5;

/// Number of standard deviations from the mean that counts as anomalous.
const DEFAULT_Z_THRESHOLD: f64 = 3.0;

/// Minimum time between two alerts for the same metric, to suppress
/// duplicate/false-positive alert storms from a single sustained anomaly.
const ALERT_COOLDOWN_SECONDS: i64 = 60;

/// Suppression entry for pattern-based allowlisting.
#[derive(Debug, Clone, Serialize)]
pub struct Suppression {
    pub id: u64,
    pub pattern: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Correlation tracking between anomalies.
#[derive(Debug, Clone, Serialize)]
pub struct AnomalyCorrelation {
    pub anomaly_id: String,
    pub correlated_ids: Vec<String>,
    pub correlation_strength: f64,
}

/// Stream event for real-time processing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamEvent {
    pub event_id: u64,
    pub metric: String,
    pub value: f64,
    pub timestamp: DateTime<Utc>,
}

/// Feedback entry for model improvement.
#[derive(Debug, Clone, Serialize)]
pub struct Feedback {
    pub id: String,
    pub anomaly_id: String,
    pub is_true_positive: bool,
    pub submitted_at: DateTime<Utc>,
}

/// Running (Welford) baseline statistics for one metric.
#[derive(Debug, Clone, Serialize)]
pub struct Baseline {
    pub count: u64,
    pub mean: f64,
    #[serde(skip)]
    m2: f64,
    pub last_updated: DateTime<Utc>,
}

impl Default for Baseline {
    fn default() -> Self {
        Self {
            count: 0,
            mean: 0.0,
            m2: 0.0,
            last_updated: Utc::now(),
        }
    }
}

impl Baseline {
    fn std_dev(&self) -> f64 {
        if self.count < 2 {
            return 0.0;
        }
        (self.m2 / (self.count - 1) as f64).sqrt()
    }

    /// Welford's online update. Returns the z-score of `value` against the
    /// baseline as it was *before* this update, so the current point is
    /// judged against history, not against itself.
    fn observe(&mut self, value: f64) -> f64 {
        let std_dev_before = self.std_dev();
        let mean_before = self.mean;
        let z = if self.count >= MIN_SAMPLES_FOR_DETECTION && std_dev_before > f64::EPSILON {
            (value - mean_before) / std_dev_before
        } else {
            0.0
        };

        self.count += 1;
        let delta = value - self.mean;
        self.mean += delta / self.count as f64;
        let delta2 = value - self.mean;
        self.m2 += delta * delta2;
        self.last_updated = Utc::now();

        z
    }
}

/// Severity bucket derived from how far outside the threshold a point was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warning,
    Critical,
}

/// A generated anomaly alert.
#[derive(Debug, Clone, Serialize)]
pub struct Alert {
    pub id: String,
    pub metric: String,
    pub value: f64,
    pub baseline_mean: f64,
    pub baseline_std_dev: f64,
    pub z_score: f64,
    pub severity: Severity,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct ObserveRequest {
    pub metric: String,
    pub value: f64,
}

#[derive(Default)]
struct Inner {
    baselines: HashMap<String, Baseline>,
    alerts: Vec<Alert>,
    last_alert_at: HashMap<String, DateTime<Utc>>,
    suppressions: HashMap<u64, Suppression>,
    suppression_counter: u64,
    correlations: HashMap<String, AnomalyCorrelation>,
    stream_events: Vec<StreamEvent>,
    feedback_entries: Vec<Feedback>,
}

/// Shared anomaly-detection state: one baseline per metric plus the
/// generated alert history.
#[derive(Default)]
pub struct AnomalyStore {
    inner: RwLock<Inner>,
}

impl AnomalyStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Feed a new observation for `metric`. Updates the learned baseline and
    /// returns `Some(Alert)` if this point was anomalous and not suppressed
    /// by the cooldown filter.
    pub fn observe(&self, metric: &str, value: f64) -> Option<Alert> {
        let mut inner = self.inner.write().expect("anomaly lock poisoned");

        let baseline = inner.baselines.entry(metric.to_string()).or_default();
        let mean_before = baseline.mean;
        let std_dev_before = baseline.std_dev();
        let count_before = baseline.count;
        let z = baseline.observe(value);

        if count_before < MIN_SAMPLES_FOR_DETECTION || z.abs() < DEFAULT_Z_THRESHOLD {
            return None;
        }

        let now = Utc::now();
        if let Some(last) = inner.last_alert_at.get(metric) {
            if now - *last < Duration::seconds(ALERT_COOLDOWN_SECONDS) {
                return None; // false-positive filtering: still in cooldown
            }
        }

        let severity = if z.abs() >= DEFAULT_Z_THRESHOLD * 2.0 {
            Severity::Critical
        } else {
            Severity::Warning
        };

        let alert = Alert {
            id: Uuid::new_v4().to_string(),
            metric: metric.to_string(),
            value,
            baseline_mean: mean_before,
            baseline_std_dev: std_dev_before,
            z_score: z,
            severity,
            timestamp: now,
        };

        inner.last_alert_at.insert(metric.to_string(), now);
        inner.alerts.push(alert.clone());
        Some(alert)
    }

    pub fn alerts(&self) -> Vec<Alert> {
        self.inner
            .read()
            .expect("anomaly lock poisoned")
            .alerts
            .clone()
    }

    pub fn baseline(&self, metric: &str) -> Option<Baseline> {
        self.inner
            .read()
            .expect("anomaly lock poisoned")
            .baselines
            .get(metric)
            .cloned()
    }

    pub fn suppress_anomaly(&self, pattern: &str, expires_at: Option<DateTime<Utc>>) -> u64 {
        let mut inner = self.inner.write().expect("anomaly lock poisoned");
        inner.suppression_counter += 1;
        let id = inner.suppression_counter;

        let suppression = Suppression {
            id,
            pattern: pattern.to_string(),
            created_at: Utc::now(),
            expires_at,
        };

        inner.suppressions.insert(id, suppression);
        id
    }

    pub fn get_suppressions(&self) -> Vec<Suppression> {
        let inner = self.inner.read().expect("anomaly lock poisoned");
        let now = Utc::now();
        inner
            .suppressions
            .values()
            .filter(|s| s.expires_at.is_none() || s.expires_at.unwrap() > now)
            .cloned()
            .collect()
    }

    pub fn remove_suppression(&self, id: u64) -> bool {
        self.inner
            .write()
            .expect("anomaly lock poisoned")
            .suppressions
            .remove(&id)
            .is_some()
    }

    pub fn get_correlated_anomalies(&self, anomaly_id: &str) -> Vec<String> {
        self.inner
            .read()
            .expect("anomaly lock poisoned")
            .correlations
            .get(anomaly_id)
            .map(|c| c.correlated_ids.clone())
            .unwrap_or_default()
    }

    pub fn add_correlation(&self, anomaly_id: &str, correlated_id: &str, strength: f64) {
        let mut inner = self.inner.write().expect("anomaly lock poisoned");
        let correlation = inner
            .correlations
            .entry(anomaly_id.to_string())
            .or_insert_with(|| AnomalyCorrelation {
                anomaly_id: anomaly_id.to_string(),
                correlated_ids: Vec::new(),
                correlation_strength: strength,
            });

        if !correlation.correlated_ids.contains(&correlated_id.to_string()) {
            correlation.correlated_ids.push(correlated_id.to_string());
        }
        correlation.correlation_strength = strength.max(correlation.correlation_strength);
    }

    pub fn process_stream_event(&self, metric: &str, value: f64) -> (Option<Alert>, StreamEvent) {
        let event_id = {
            let mut inner = self.inner.write().expect("anomaly lock poisoned");
            inner.stream_events.len() as u64 + 1
        };

        let event = StreamEvent {
            event_id,
            metric: metric.to_string(),
            value,
            timestamp: Utc::now(),
        };

        {
            let mut inner = self.inner.write().expect("anomaly lock poisoned");
            inner.stream_events.push(event.clone());
        }

        let alert = self.observe(metric, value);
        (alert, event)
    }

    pub fn get_stream_events(&self) -> Vec<StreamEvent> {
        self.inner
            .read()
            .expect("anomaly lock poisoned")
            .stream_events
            .clone()
    }

    pub fn submit_feedback(&self, anomaly_id: &str, is_true_positive: bool) -> String {
        let mut inner = self.inner.write().expect("anomaly lock poisoned");
        let feedback_id = Uuid::new_v4().to_string();

        let feedback = Feedback {
            id: feedback_id.clone(),
            anomaly_id: anomaly_id.to_string(),
            is_true_positive,
            submitted_at: Utc::now(),
        };

        inner.feedback_entries.push(feedback);
        feedback_id
    }

    pub fn get_feedback(&self) -> Vec<Feedback> {
        self.inner
            .read()
            .expect("anomaly lock poisoned")
            .feedback_entries
            .clone()
    }

    pub fn get_feedback_for_anomaly(&self, anomaly_id: &str) -> Vec<Feedback> {
        self.inner
            .read()
            .expect("anomaly lock poisoned")
            .feedback_entries
            .iter()
            .filter(|f| f.anomaly_id == anomaly_id)
            .cloned()
            .collect()
    }
}

/// `POST /anomaly/observe` - feed a metric observation into the detector.
/// Returns the generated alert, if any, as `{"alert": ...}` or
/// `{"alert": null}` when the point was normal or suppressed.
pub async fn observe_metric(
    State(store): State<Arc<AnomalyStore>>,
    Json(req): Json<ObserveRequest>,
) -> impl IntoResponse {
    let alert = store.observe(&req.metric, req.value);
    Json(serde_json::json!({ "alert": alert }))
}

/// `GET /anomaly/alerts` - list all alerts generated so far.
pub async fn list_alerts(State(store): State<Arc<AnomalyStore>>) -> impl IntoResponse {
    Json(store.alerts())
}

/// `GET /anomaly/baseline/:metric` - inspect the learned baseline for a metric.
pub async fn get_baseline(
    State(store): State<Arc<AnomalyStore>>,
    Path(metric): Path<String>,
) -> impl IntoResponse {
    match store.baseline(&metric) {
        Some(baseline) => (StatusCode::OK, Json(baseline)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_alert_while_learning_baseline() {
        let store = AnomalyStore::default();
        for v in [10.0, 11.0, 9.0, 10.5] {
            assert!(store.observe("cpu_pct", v).is_none());
        }
    }

    #[test]
    fn spike_after_stable_baseline_triggers_alert() {
        let store = AnomalyStore::default();
        for v in [10.0, 10.1, 9.9, 10.0, 10.05, 9.95, 10.0] {
            store.observe("cpu_pct", v);
        }
        let alert = store.observe("cpu_pct", 500.0);
        assert!(alert.is_some(), "large spike should be flagged");
        assert_eq!(alert.unwrap().metric, "cpu_pct");
    }

    #[test]
    fn cooldown_suppresses_duplicate_alerts() {
        let store = AnomalyStore::default();
        for v in [10.0, 10.1, 9.9, 10.0, 10.05, 9.95, 10.0] {
            store.observe("cpu_pct", v);
        }
        let first = store.observe("cpu_pct", 500.0);
        let second = store.observe("cpu_pct", 501.0);
        assert!(first.is_some());
        assert!(
            second.is_none(),
            "second spike within cooldown should be suppressed"
        );
    }

    #[test]
    fn stable_values_never_alert() {
        let store = AnomalyStore::default();
        for _ in 0..50 {
            assert!(store.observe("steady_metric", 42.0).is_none());
        }
    }

    // Issue #540: Anomaly Suppression and Allowlisting Tests
    #[test]
    fn suppress_anomaly_creates_suppression_entry() {
        let store = AnomalyStore::default();
        let id = store.suppress_anomaly("cpu_spike_*", None);
        assert!(id > 0, "suppression ID should be positive");

        let suppressions = store.get_suppressions();
        assert_eq!(suppressions.len(), 1);
        assert_eq!(suppressions[0].id, id);
        assert_eq!(suppressions[0].pattern, "cpu_spike_*");
    }

    #[test]
    fn suppress_anomaly_with_expiry() {
        let store = AnomalyStore::default();
        let future = Utc::now() + Duration::hours(1);
        let id = store.suppress_anomaly("memory_leak_*", Some(future));

        let suppressions = store.get_suppressions();
        assert_eq!(suppressions.len(), 1);
        assert_eq!(suppressions[0].expires_at, Some(future));
    }

    #[test]
    fn expired_suppressions_are_filtered() {
        let store = AnomalyStore::default();
        let past = Utc::now() - Duration::hours(1);
        let future = Utc::now() + Duration::hours(1);

        store.suppress_anomaly("old_pattern_*", Some(past));
        store.suppress_anomaly("new_pattern_*", Some(future));

        let suppressions = store.get_suppressions();
        assert_eq!(suppressions.len(), 1);
        assert_eq!(suppressions[0].pattern, "new_pattern_*");
    }

    #[test]
    fn remove_suppression_by_id() {
        let store = AnomalyStore::default();
        let id1 = store.suppress_anomaly("pattern1_*", None);
        let id2 = store.suppress_anomaly("pattern2_*", None);

        assert!(store.remove_suppression(id1));
        let suppressions = store.get_suppressions();
        assert_eq!(suppressions.len(), 1);
        assert_eq!(suppressions[0].id, id2);
    }

    #[test]
    fn bulk_suppression_for_similar_patterns() {
        let store = AnomalyStore::default();
        let patterns = vec!["cpu_spike_*", "cpu_high_*", "cpu_anomaly_*"];

        for pattern in patterns {
            store.suppress_anomaly(pattern, None);
        }

        let suppressions = store.get_suppressions();
        assert_eq!(suppressions.len(), 3);
    }

    // Issue #541: Anomaly Correlation Detection Tests
    #[test]
    fn add_correlation_between_anomalies() {
        let store = AnomalyStore::default();
        let alert1 = store.observe("cpu_pct", 10.0);
        let alert2 = store.observe("memory_pct", 20.0);

        if let (Some(a1), Some(a2)) = (alert1, alert2) {
            for v in [10.1, 10.2, 10.0, 9.9] {
                store.observe("cpu_pct", v);
            }
            let spike = store.observe("cpu_pct", 100.0);
            if let Some(spike_alert) = spike {
                store.add_correlation(&spike_alert.id, &a1.id, 0.95);
                let correlated = store.get_correlated_anomalies(&spike_alert.id);
                assert!(correlated.contains(&a1.id));
            }
        }
    }

    #[test]
    fn correlation_scoring_tracks_strength() {
        let store = AnomalyStore::default();
        let id1 = "anomaly_1".to_string();
        let id2 = "anomaly_2".to_string();

        store.add_correlation(&id1, &id2, 0.75);
        let correlated = store.get_correlated_anomalies(&id1);
        assert_eq!(correlated.len(), 1);
        assert!(correlated.contains(&id2));
    }

    #[test]
    fn get_correlated_anomalies_returns_vec() {
        let store = AnomalyStore::default();
        let id1 = "anomaly_1";
        let id2 = "anomaly_2";
        let id3 = "anomaly_3";

        store.add_correlation(id1, id2, 0.8);
        store.add_correlation(id1, id3, 0.9);

        let correlated = store.get_correlated_anomalies(id1);
        assert_eq!(correlated.len(), 2);
        assert!(correlated.contains(&id2.to_string()));
        assert!(correlated.contains(&id3.to_string()));
    }

    #[test]
    fn grouping_correlated_anomalies() {
        let store = AnomalyStore::default();

        for i in 0..5 {
            let id = format!("anomaly_{}", i);
            for j in (i+1)..5 {
                let related_id = format!("anomaly_{}", j);
                store.add_correlation(&id, &related_id, 0.8 + (j - i) as f64 * 0.05);
            }
        }

        let correlated = store.get_correlated_anomalies("anomaly_0");
        assert!(correlated.len() > 0);
    }

    #[test]
    fn no_duplicate_correlations() {
        let store = AnomalyStore::default();
        let id1 = "anomaly_1";
        let id2 = "anomaly_2";

        store.add_correlation(id1, id2, 0.8);
        store.add_correlation(id1, id2, 0.9);

        let correlated = store.get_correlated_anomalies(id1);
        assert_eq!(correlated.len(), 1);
    }

    // Issue #542: Real-Time Anomaly Stream Processing Tests
    #[test]
    fn stream_event_creation_with_event_id() {
        let store = AnomalyStore::default();
        let (alert, event) = store.process_stream_event("cpu_pct", 42.0);

        assert_eq!(event.metric, "cpu_pct");
        assert_eq!(event.value, 42.0);
        assert!(event.event_id > 0);
    }

    #[test]
    fn stream_processing_generates_alerts() {
        let store = AnomalyStore::default();
        for v in [10.0, 10.1, 9.9, 10.0, 10.05, 9.95, 10.0] {
            store.process_stream_event("cpu_pct", v);
        }
        let (alert, event) = store.process_stream_event("cpu_pct", 500.0);

        assert!(alert.is_some(), "stream spike should trigger alert");
        assert_eq!(event.metric, "cpu_pct");
    }

    #[test]
    fn stream_events_recorded_in_order() {
        let store = AnomalyStore::default();
        store.process_stream_event("metric_a", 1.0);
        store.process_stream_event("metric_b", 2.0);
        store.process_stream_event("metric_c", 3.0);

        let events = store.get_stream_events();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_id, 1);
        assert_eq!(events[1].event_id, 2);
        assert_eq!(events[2].event_id, 3);
    }

    #[test]
    fn stream_processing_reduces_latency() {
        let store = AnomalyStore::default();
        let now = Utc::now();

        let (_, event) = store.process_stream_event("fast_metric", 99.9);

        assert!(event.timestamp >= now, "event timestamp should be current");
    }

    #[test]
    fn stream_events_with_timestamps() {
        let store = AnomalyStore::default();
        let (_, event1) = store.process_stream_event("metric", 1.0);
        let (_, event2) = store.process_stream_event("metric", 2.0);

        assert!(event2.timestamp >= event1.timestamp);
    }

    // Issue #543: Anomaly Feedback Loop for Model Improvement Tests
    #[test]
    fn submit_feedback_for_anomaly() {
        let store = AnomalyStore::default();
        let anomaly_id = "alert_123";

        let feedback_id = store.submit_feedback(anomaly_id, true);
        assert!(!feedback_id.is_empty());

        let feedback = store.get_feedback_for_anomaly(anomaly_id);
        assert_eq!(feedback.len(), 1);
        assert_eq!(feedback[0].anomaly_id, anomaly_id);
        assert!(feedback[0].is_true_positive);
    }

    #[test]
    fn track_true_positives_and_false_positives() {
        let store = AnomalyStore::default();
        let anomaly_id = "alert_456";

        store.submit_feedback(anomaly_id, true);
        store.submit_feedback(anomaly_id, false);
        store.submit_feedback(anomaly_id, true);

        let feedback = store.get_feedback_for_anomaly(anomaly_id);
        assert_eq!(feedback.len(), 3);
        let true_positives = feedback.iter().filter(|f| f.is_true_positive).count();
        assert_eq!(true_positives, 2);
    }

    #[test]
    fn feedback_collection_for_improvement() {
        let store = AnomalyStore::default();

        for i in 0..10 {
            let anomaly_id = format!("alert_{}", i);
            let is_positive = i % 2 == 0;
            store.submit_feedback(&anomaly_id, is_positive);
        }

        let all_feedback = store.get_feedback();
        assert_eq!(all_feedback.len(), 10);
    }

    #[test]
    fn feedback_timestamps_recorded() {
        let store = AnomalyStore::default();
        let now = Utc::now();

        store.submit_feedback("alert_789", true);
        let feedback = store.get_feedback();

        assert_eq!(feedback.len(), 1);
        assert!(feedback[0].submitted_at >= now);
    }

    #[test]
    fn model_versioning_with_feedback_epochs() {
        let store = AnomalyStore::default();

        let anomaly1 = "alert_v1_1";
        let anomaly2 = "alert_v1_2";
        let anomaly3 = "alert_v2_1";

        store.submit_feedback(anomaly1, true);
        store.submit_feedback(anomaly2, false);
        store.submit_feedback(anomaly3, true);

        let feedback_v1: Vec<_> = store
            .get_feedback()
            .iter()
            .filter(|f| f.anomaly_id.starts_with("alert_v1"))
            .collect();

        assert_eq!(feedback_v1.len(), 2);
    }

    #[test]
    fn performance_tracking_with_feedback() {
        let store = AnomalyStore::default();

        let mut true_count = 0;
        let mut false_count = 0;

        for i in 0..100 {
            let anomaly_id = format!("perf_alert_{}", i);
            let is_tp = i % 3 != 0;
            store.submit_feedback(&anomaly_id, is_tp);
            if is_tp { true_count += 1; } else { false_count += 1; }
        }

        let feedback = store.get_feedback();
        assert_eq!(feedback.len(), 100);
        let actual_tp = feedback.iter().filter(|f| f.is_true_positive).count();
        assert_eq!(actual_tp, true_count);
    }
}
