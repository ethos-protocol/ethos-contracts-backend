//! Real-time metric dashboards (issue #602: "Metrics are historical. Real-time
//! dashboards would enable live monitoring.").
//!
//! Provides:
//! - Metric sample ingestion (`POST /dashboard/metrics`).
//! - Live streaming of every ingested sample over Server-Sent Events
//!   (`GET /dashboard/stream`), fed by a `tokio` broadcast channel.
//! - A dashboard snapshot with the latest value, min/max/avg over the retained
//!   window, and any metric alert rules currently breached
//!   (`GET /dashboard/snapshot`).
//! - Drill-down into the retained samples of one metric over a time range
//!   (`GET /dashboard/metrics/:name?start=&end=`).
//!
//! Samples are kept in memory with a bounded window per metric, so memory use
//! is capped regardless of ingest volume.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::convert::Infallible;
use std::sync::{Arc, RwLock};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    Json,
};
use chrono::{DateTime, Utc};
use futures::stream::{self, Stream};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Samples retained per metric when the caller does not configure a size.
pub const DEFAULT_RETENTION_PER_METRIC: usize = 1_000;

/// Number of samples the broadcast channel buffers for slow subscribers.
pub const DEFAULT_STREAM_CAPACITY: usize = 256;

/// One observed value of a named metric.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricSample {
    pub name: String,
    pub value: f64,
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub tags: HashMap<String, String>,
}

/// Direction in which a threshold rule breaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparison {
    Above,
    Below,
}

/// Threshold alert on the value of a metric.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricAlertRule {
    pub metric: String,
    pub threshold: f64,
    pub comparison: Comparison,
}

impl MetricAlertRule {
    /// Whether `value` breaches this rule.
    pub fn is_breached_by(&self, value: f64) -> bool {
        match self.comparison {
            Comparison::Above => value > self.threshold,
            Comparison::Below => value < self.threshold,
        }
    }
}

/// A breached alert rule, with the value that triggered it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetricAlert {
    pub metric: String,
    pub value: f64,
    pub threshold: f64,
    pub comparison: Comparison,
    pub timestamp: DateTime<Utc>,
}

/// Summary of one metric across its retained window.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetricSnapshot {
    pub name: String,
    pub latest: f64,
    pub latest_at: DateTime<Utc>,
    pub sample_count: usize,
    pub min: f64,
    pub max: f64,
    pub avg: f64,
}

/// Point-in-time view of every metric on the dashboard.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DashboardSnapshot {
    pub generated_at: DateTime<Utc>,
    pub metrics: Vec<MetricSnapshot>,
    pub alerts: Vec<MetricAlert>,
}

/// Summarise a non-empty set of samples for one metric. Returns `None` when
/// there are no samples. "Latest" is the sample with the newest timestamp, so
/// out-of-order ingestion still reports the true current value.
pub fn summarize(name: &str, samples: &VecDeque<MetricSample>) -> Option<MetricSnapshot> {
    let latest = samples.iter().max_by_key(|s| s.timestamp)?;
    let (min, max, sum) = samples.iter().fold(
        (f64::INFINITY, f64::NEG_INFINITY, 0.0_f64),
        |(min, max, sum), s| (min.min(s.value), max.max(s.value), sum + s.value),
    );
    Some(MetricSnapshot {
        name: name.to_string(),
        latest: latest.value,
        latest_at: latest.timestamp,
        sample_count: samples.len(),
        min,
        max,
        avg: sum / samples.len() as f64,
    })
}

#[derive(Default)]
struct Inner {
    series: HashMap<String, VecDeque<MetricSample>>,
    rules: Vec<MetricAlertRule>,
}

/// Live dashboard state: bounded per-metric history plus a broadcast feed.
pub struct RealtimeDashboard {
    inner: RwLock<Inner>,
    retention: usize,
    tx: broadcast::Sender<MetricSample>,
}

impl RealtimeDashboard {
    pub fn new() -> Arc<Self> {
        Self::with_limits(DEFAULT_RETENTION_PER_METRIC, DEFAULT_STREAM_CAPACITY)
    }

    /// `retention` samples are kept per metric; `stream_capacity` samples are
    /// buffered for each subscriber before it starts lagging.
    pub fn with_limits(retention: usize, stream_capacity: usize) -> Arc<Self> {
        let (tx, _) = broadcast::channel(stream_capacity.max(1));
        Arc::new(Self {
            inner: RwLock::new(Inner::default()),
            retention: retention.max(1),
            tx,
        })
    }

    /// Subscribe to every sample ingested from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<MetricSample> {
        self.tx.subscribe()
    }

    pub fn add_alert_rule(&self, rule: MetricAlertRule) {
        self.inner
            .write()
            .expect("dashboard lock poisoned")
            .rules
            .push(rule);
    }

    /// Store a sample, publish it to live subscribers, and return any alert
    /// rules this sample breaches.
    pub fn ingest(&self, sample: MetricSample) -> Vec<MetricAlert> {
        let alerts = {
            let mut inner = self.inner.write().expect("dashboard lock poisoned");
            let series = inner.series.entry(sample.name.clone()).or_default();
            series.push_back(sample.clone());
            while series.len() > self.retention {
                series.pop_front();
            }
            inner
                .rules
                .iter()
                .filter(|r| r.metric == sample.name && r.is_breached_by(sample.value))
                .map(|r| MetricAlert {
                    metric: sample.name.clone(),
                    value: sample.value,
                    threshold: r.threshold,
                    comparison: r.comparison,
                    timestamp: sample.timestamp,
                })
                .collect()
        };
        // An error here only means no subscriber is connected, which is fine.
        let _ = self.tx.send(sample);
        alerts
    }

    /// Samples of `name` with timestamps in `[start, end]`, oldest first.
    pub fn drill_down(
        &self,
        name: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Vec<MetricSample> {
        let inner = self.inner.read().expect("dashboard lock poisoned");
        let mut samples: Vec<MetricSample> = inner
            .series
            .get(name)
            .map(|series| {
                series
                    .iter()
                    .filter(|s| s.timestamp >= start && s.timestamp <= end)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        samples.sort_by_key(|s| s.timestamp);
        samples
    }

    /// Current state of every metric plus the alert rules it breaches now.
    pub fn snapshot(&self) -> DashboardSnapshot {
        let inner = self.inner.read().expect("dashboard lock poisoned");
        let ordered: BTreeMap<&String, &VecDeque<MetricSample>> = inner.series.iter().collect();

        let metrics: Vec<MetricSnapshot> = ordered
            .iter()
            .filter_map(|(name, samples)| summarize(name, samples))
            .collect();

        let alerts = inner
            .rules
            .iter()
            .filter_map(|rule| {
                let latest = metrics.iter().find(|m| m.name == rule.metric)?;
                rule.is_breached_by(latest.latest).then(|| MetricAlert {
                    metric: rule.metric.clone(),
                    value: latest.latest,
                    threshold: rule.threshold,
                    comparison: rule.comparison,
                    timestamp: latest.latest_at,
                })
            })
            .collect();

        DashboardSnapshot {
            generated_at: Utc::now(),
            metrics,
            alerts,
        }
    }
}

/// `POST /dashboard/metrics` - ingest one sample. Responds with any alerts it breaches.
pub async fn ingest_metric(
    State(dashboard): State<Arc<RealtimeDashboard>>,
    Json(sample): Json<MetricSample>,
) -> impl IntoResponse {
    if sample.name.trim().is_empty() || !sample.value.is_finite() {
        return (
            StatusCode::BAD_REQUEST,
            "metric name and finite value are required",
        )
            .into_response();
    }
    Json(dashboard.ingest(sample)).into_response()
}

/// `GET /dashboard/snapshot` - current value and summary for every metric.
pub async fn get_dashboard_snapshot(
    State(dashboard): State<Arc<RealtimeDashboard>>,
) -> impl IntoResponse {
    Json(dashboard.snapshot())
}

/// `POST /dashboard/alerts` - add a threshold alert rule.
pub async fn add_metric_alert_rule(
    State(dashboard): State<Arc<RealtimeDashboard>>,
    Json(rule): Json<MetricAlertRule>,
) -> impl IntoResponse {
    if !rule.threshold.is_finite() {
        return StatusCode::BAD_REQUEST;
    }
    dashboard.add_alert_rule(rule);
    StatusCode::CREATED
}

/// `GET /dashboard/stream` - live metric samples as Server-Sent Events.
///
/// Each event is named `metric` and carries a JSON-encoded [`MetricSample`].
/// A subscriber that falls behind skips the missed samples rather than
/// disconnecting.
pub async fn stream_metrics(
    State(dashboard): State<Arc<RealtimeDashboard>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = dashboard.subscribe();
    let events = stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(sample) => match Event::default().event("metric").json_data(&sample) {
                    Ok(event) => return Some((Ok(event), rx)),
                    Err(_) => continue,
                },
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(events).keep_alive(KeepAlive::default())
}

#[derive(Debug, Deserialize)]
pub struct DrillDownQuery {
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
}

/// `GET /dashboard/metrics/:name?start=&end=` - drill into one metric's history.
pub async fn drill_down_metric(
    State(dashboard): State<Arc<RealtimeDashboard>>,
    Path(name): Path<String>,
    Query(q): Query<DrillDownQuery>,
) -> impl IntoResponse {
    let start = q.start.unwrap_or(DateTime::<Utc>::MIN_UTC);
    let end = q.end.unwrap_or(DateTime::<Utc>::MAX_UTC);
    if start > end {
        return (StatusCode::BAD_REQUEST, "start must not be after end").into_response();
    }
    Json(dashboard.drill_down(&name, start, end)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn at(second: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 1, 10, 0, 0).unwrap() + Duration::seconds(second)
    }

    fn sample(name: &str, value: f64, second: i64) -> MetricSample {
        MetricSample {
            name: name.to_string(),
            value,
            timestamp: at(second),
            tags: HashMap::new(),
        }
    }

    #[test]
    fn snapshot_reports_latest_min_max_and_avg() {
        let dash = RealtimeDashboard::new();
        dash.ingest(sample("latency_ms", 10.0, 0));
        dash.ingest(sample("latency_ms", 30.0, 1));
        dash.ingest(sample("latency_ms", 20.0, 2));

        let snap = dash.snapshot();
        let m = snap
            .metrics
            .iter()
            .find(|m| m.name == "latency_ms")
            .unwrap();
        assert_eq!(m.latest, 20.0);
        assert_eq!(m.latest_at, at(2));
        assert_eq!(m.sample_count, 3);
        assert_eq!(m.min, 10.0);
        assert_eq!(m.max, 30.0);
        assert_eq!(m.avg, 20.0);
    }

    #[test]
    fn latest_uses_newest_timestamp_not_arrival_order() {
        let dash = RealtimeDashboard::new();
        dash.ingest(sample("queue_depth", 5.0, 10));
        dash.ingest(sample("queue_depth", 99.0, 2));

        let snap = dash.snapshot();
        assert_eq!(snap.metrics[0].latest, 5.0);
    }

    #[test]
    fn retention_drops_oldest_samples() {
        let dash = RealtimeDashboard::with_limits(2, 8);
        dash.ingest(sample("x", 1.0, 0));
        dash.ingest(sample("x", 2.0, 1));
        dash.ingest(sample("x", 3.0, 2));

        let snap = dash.snapshot();
        assert_eq!(snap.metrics[0].sample_count, 2);
        assert_eq!(snap.metrics[0].min, 2.0);
    }

    #[test]
    fn threshold_alert_fires_on_breach_only() {
        let dash = RealtimeDashboard::new();
        dash.add_alert_rule(MetricAlertRule {
            metric: "error_rate".to_string(),
            threshold: 0.05,
            comparison: Comparison::Above,
        });

        assert!(dash.ingest(sample("error_rate", 0.01, 0)).is_empty());
        let fired = dash.ingest(sample("error_rate", 0.2, 1));
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].value, 0.2);
        assert_eq!(fired[0].threshold, 0.05);
    }

    #[test]
    fn below_threshold_rule_fires_when_value_drops() {
        let rule = MetricAlertRule {
            metric: "uptime".to_string(),
            threshold: 99.0,
            comparison: Comparison::Below,
        };
        assert!(rule.is_breached_by(98.5));
        assert!(!rule.is_breached_by(99.5));
    }

    #[test]
    fn snapshot_lists_currently_breached_rules() {
        let dash = RealtimeDashboard::new();
        dash.add_alert_rule(MetricAlertRule {
            metric: "cpu".to_string(),
            threshold: 80.0,
            comparison: Comparison::Above,
        });
        dash.ingest(sample("cpu", 95.0, 0));
        assert_eq!(dash.snapshot().alerts.len(), 1);

        dash.ingest(sample("cpu", 40.0, 1));
        assert!(dash.snapshot().alerts.is_empty());
    }

    #[test]
    fn drill_down_returns_window_oldest_first() {
        let dash = RealtimeDashboard::new();
        for second in 0..5 {
            dash.ingest(sample("rps", f64::from(second as i32), second));
        }
        let window = dash.drill_down("rps", at(1), at(3));
        let values: Vec<f64> = window.iter().map(|s| s.value).collect();
        assert_eq!(values, vec![1.0, 2.0, 3.0]);
        assert!(dash.drill_down("unknown", at(0), at(10)).is_empty());
    }

    #[tokio::test]
    async fn subscribers_receive_ingested_samples() {
        let dash = RealtimeDashboard::new();
        let mut rx = dash.subscribe();
        dash.ingest(sample("live", 7.0, 0));
        let received = rx.recv().await.unwrap();
        assert_eq!(received.name, "live");
        assert_eq!(received.value, 7.0);
    }

    #[test]
    fn ingest_without_subscribers_does_not_fail() {
        let dash = RealtimeDashboard::new();
        assert!(dash.ingest(sample("quiet", 1.0, 0)).is_empty());
    }
}
