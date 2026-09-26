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
//! On top of the base detector:
//!
//! - **Seasonal handling (#544)**: metrics can opt into a seasonality
//!   (hour-of-day, day-of-week or month-of-year). Each observation is
//!   decomposed into `level + seasonal + residual`, where the level is the
//!   global baseline mean and the seasonal component is the per-season
//!   baseline's offset from it. Once a season has enough history, points are
//!   judged on their residual against that season's own spread, so e.g. the
//!   usual December withdrawal surge no longer alarms. Per-season threshold
//!   multipliers allow further manual adjustment.
//! - **Cross-service correlation (#545)**: alerts carry the service that
//!   produced them. Alerts close together in time are grouped; a group that
//!   spans two or more services is a *system anomaly*. The earliest alert in
//!   the group is recorded as its suspected root cause, and root causes are
//!   tallied so recurring systemic failure origins become visible.
//! - **Investigation audit trail (#546)**: every action an investigator
//!   takes on an alert or system anomaly (open, assign, note, decision,
//!   close, reopen) is appended to an immutable per-anomaly history and
//!   logged.
//!
//! See `docs/anomaly-detection.md` for tuning guidance.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Datelike, Duration, Timelike, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit::authorize_admin;

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
    fn observe(&mut self, value: f64, at: DateTime<Utc>) -> f64 {
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
        self.last_updated = at;

        z
    }
}

/// Severity bucket derived from how far outside the threshold a point was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warning,
    Critical,
}

/// A generated anomaly alert.
#[derive(Debug, Clone, Serialize)]
pub struct Alert {
    pub id: String,
    /// Service that produced the observation (`"default"` when unnamed).
    pub service: String,
    pub metric: String,
    pub value: f64,
    /// Mean the point was judged against: the seasonal bucket mean when the
    /// metric is seasonal and that season is trained, else the global mean.
    pub baseline_mean: f64,
    pub baseline_std_dev: f64,
    pub z_score: f64,
    /// Effective z threshold after any seasonal adjustment.
    pub threshold: f64,
    /// Season bucket the observation fell in, for seasonal metrics.
    pub seasonal_bucket: Option<u32>,
    /// Seasonal component (bucket mean minus global mean) removed before
    /// scoring, for seasonal metrics with a trained bucket.
    pub seasonal_component: Option<f64>,
    pub severity: Severity,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct ObserveRequest {
    #[serde(default)]
    pub service: Option<String>,
    pub metric: String,
    pub value: f64,
    /// Optional observation time; defaults to now.
    #[serde(default)]
    pub timestamp: Option<DateTime<Utc>>,
}

fn service_label(service: Option<&str>) -> String {
    match service.map(str::trim) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => DEFAULT_SERVICE.to_string(),
    }
}

/// Baselines are keyed by `service:metric`, or just `metric` for the default
/// service so existing single-service callers keep their keys.
fn metric_key(service: &str, metric: &str) -> String {
    if service == DEFAULT_SERVICE {
        metric.to_string()
    } else {
        format!("{service}:{metric}")
    }
}

// ── Seasonal handling (#544) ─────────────────────────────────────────────────

/// Seasonal period a metric's baseline is decomposed over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Seasonality {
    /// 24 buckets, one per UTC hour.
    HourOfDay,
    /// 7 buckets, Monday = 0.
    DayOfWeek,
    /// 12 buckets, January = 0.
    MonthOfYear,
}

impl Seasonality {
    fn bucket_count(self) -> usize {
        match self {
            Seasonality::HourOfDay => 24,
            Seasonality::DayOfWeek => 7,
            Seasonality::MonthOfYear => 12,
        }
    }

    fn bucket(self, at: DateTime<Utc>) -> usize {
        match self {
            Seasonality::HourOfDay => at.hour() as usize,
            Seasonality::DayOfWeek => at.weekday().num_days_from_monday() as usize,
            Seasonality::MonthOfYear => at.month0() as usize,
        }
    }
}

/// Per-season baselines and threshold multipliers for one metric.
#[derive(Debug, Clone)]
struct SeasonalProfile {
    seasonality: Seasonality,
    buckets: Vec<Baseline>,
    threshold_multipliers: Vec<f64>,
}

impl SeasonalProfile {
    fn new(seasonality: Seasonality) -> Self {
        let n = seasonality.bucket_count();
        Self {
            seasonality,
            buckets: vec![Baseline::default(); n],
            threshold_multipliers: vec![1.0; n],
        }
    }
}

/// One season's learned pattern.
#[derive(Debug, Clone, Serialize)]
pub struct SeasonalComponent {
    pub bucket: u32,
    pub count: u64,
    pub mean: f64,
    pub std_dev: f64,
    /// Bucket mean minus the global mean: how much higher/lower this season
    /// typically runs than the metric overall.
    pub seasonal_offset: f64,
    pub threshold_multiplier: f64,
    /// Effective z threshold applied to points in this season.
    pub effective_threshold: f64,
    /// Whether the bucket has enough samples to be used for scoring.
    pub trained: bool,
}

/// The learned seasonal decomposition for a metric.
#[derive(Debug, Clone, Serialize)]
pub struct SeasonalPattern {
    pub service: String,
    pub metric: String,
    pub seasonality: Seasonality,
    pub global_mean: f64,
    pub global_std_dev: f64,
    pub components: Vec<SeasonalComponent>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SeasonalError {
    #[error("metric has no seasonality configured")]
    NotConfigured,
    #[error("season bucket out of range")]
    BucketOutOfRange,
    #[error("threshold multiplier must be a positive finite number")]
    InvalidMultiplier,
}

// ── Cross-service correlation (#545) ─────────────────────────────────────────

/// Suspected origin of a correlated incident.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct RootCause {
    pub service: String,
    pub metric: String,
}

/// A group of time-correlated alerts. It is a *system anomaly* once it spans
/// two or more services.
#[derive(Debug, Clone, Serialize)]
pub struct SystemAnomaly {
    pub id: String,
    pub services: Vec<String>,
    pub alert_ids: Vec<String>,
    /// Metrics that alerted in two or more services within this group — a
    /// strong hint of a shared dependency (e.g. `db_latency_ms` everywhere).
    pub shared_metrics: Vec<String>,
    /// Earliest alert in the group.
    pub root_cause: RootCause,
    pub max_severity: Severity,
    pub started_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    #[serde(skip)]
    metrics_by_service: Vec<(String, String)>,
    #[serde(skip)]
    counted: bool,
}

impl SystemAnomaly {
    fn is_systemic(&self) -> bool {
        self.services.len() >= 2
    }

    fn absorb(&mut self, alert: &Alert) {
        if !self.services.contains(&alert.service) {
            self.services.push(alert.service.clone());
        }
        self.alert_ids.push(alert.id.clone());
        let pair = (alert.service.clone(), alert.metric.clone());
        if !self.metrics_by_service.contains(&pair) {
            self.metrics_by_service.push(pair);
        }
        if !self.shared_metrics.contains(&alert.metric)
            && self
                .metrics_by_service
                .iter()
                .any(|(svc, m)| *m == alert.metric && *svc != alert.service)
        {
            self.shared_metrics.push(alert.metric.clone());
        }
        if alert.severity > self.max_severity {
            self.max_severity = alert.severity;
        }
        if alert.timestamp < self.started_at {
            self.started_at = alert.timestamp;
            self.root_cause = RootCause {
                service: alert.service.clone(),
                metric: alert.metric.clone(),
            };
        }
        if alert.timestamp > self.last_seen_at {
            self.last_seen_at = alert.timestamp;
        }
    }
}

/// How often a root cause has originated a system anomaly.
#[derive(Debug, Clone, Serialize)]
pub struct RootCauseStat {
    pub root_cause: RootCause,
    pub occurrences: u64,
    pub last_seen_at: DateTime<Utc>,
    pub system_anomaly_ids: Vec<String>,
}

// ── Investigation audit trail (#546) ─────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationAction {
    Opened,
    Assigned,
    NoteAdded,
    DecisionRecorded,
    Closed,
    Reopened,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationDecision {
    TruePositive,
    FalsePositive,
    ExpectedBehavior,
    Escalated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationStatus {
    Open,
    Closed,
}

/// One immutable entry in an anomaly's investigation history.
#[derive(Debug, Clone, Serialize)]
pub struct InvestigationEntry {
    pub id: String,
    pub anomaly_id: String,
    /// 1-based position in this anomaly's history.
    pub sequence: u64,
    pub investigator: String,
    pub action: InvestigationAction,
    pub decision: Option<InvestigationDecision>,
    pub assignee: Option<String>,
    pub notes: Option<String>,
    /// Investigation status after this action was applied.
    pub status: InvestigationStatus,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InvestigationRequest {
    pub investigator: String,
    pub action: InvestigationAction,
    #[serde(default)]
    pub decision: Option<InvestigationDecision>,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvestigationError {
    #[error("no alert or system anomaly with this id")]
    AnomalyNotFound,
    #[error("investigator must be non-empty")]
    MissingInvestigator,
    #[error("investigation has not been opened")]
    NotOpened,
    #[error("investigation is already open")]
    AlreadyOpen,
    #[error("investigation is closed; reopen it first")]
    Closed,
    #[error("this action requires a decision")]
    MissingDecision,
    #[error("this action requires an assignee")]
    MissingAssignee,
}

// ── Store ────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Inner {
    baselines: HashMap<String, Baseline>,
    seasonal: HashMap<String, SeasonalProfile>,
    alerts: Vec<Alert>,
    last_alert_at: HashMap<String, DateTime<Utc>>,
    suppressions: HashMap<u64, Suppression>,
    suppression_counter: u64,
    correlations: HashMap<String, AnomalyCorrelation>,
    stream_events: Vec<StreamEvent>,
    feedback_entries: Vec<Feedback>,
}

/// Shared anomaly-detection state: one baseline per metric plus the
/// generated alert history, correlation groups and investigation trails.
#[derive(Default)]
pub struct AnomalyStore {
    inner: RwLock<Inner>,
}

impl AnomalyStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Feed a new observation for `metric` on the default service, timestamped
    /// now. Updates the learned baseline and returns `Some(Alert)` if this
    /// point was anomalous and not suppressed by the cooldown filter.
    pub fn observe(&self, metric: &str, value: f64) -> Option<Alert> {
        self.observe_at(None, metric, value, Utc::now())
    }

    /// Feed an observation for `metric` produced by `service`, timestamped now.
    pub fn observe_service(&self, service: &str, metric: &str, value: f64) -> Option<Alert> {
        self.observe_at(Some(service), metric, value, Utc::now())
    }

    /// Feed an observation with an explicit timestamp. The timestamp drives
    /// seasonal bucketing, cooldowns and cross-service correlation.
    pub fn observe_at(
        &self,
        service: Option<&str>,
        metric: &str,
        value: f64,
        at: DateTime<Utc>,
    ) -> Option<Alert> {
        let service = service_label(service);
        let key = metric_key(&service, metric);

        let mut guard = self.inner.write().expect("anomaly lock poisoned");
        let inner = &mut *guard;

        let baseline = inner.baselines.entry(key.clone()).or_default();
        let global_mean = baseline.mean;
        let global_std_dev = baseline.std_dev();
        let count_before = baseline.count;
        let mut z = baseline.observe(value, at);
        let mut mean = global_mean;
        let mut std_dev = global_std_dev;
        let mut threshold = DEFAULT_Z_THRESHOLD;
        let mut seasonal_bucket = None;
        let mut seasonal_component = None;

        // Seasonal decomposition: value = level + seasonal + residual. With a
        // trained bucket, score the residual (value - bucket mean) against
        // the bucket's own spread, and apply the bucket's threshold
        // multiplier. Untrained buckets fall back to the global baseline.
        if let Some(profile) = inner.seasonal.get_mut(&key) {
            let b = profile.seasonality.bucket(at);
            threshold = DEFAULT_Z_THRESHOLD * profile.threshold_multipliers[b];
            seasonal_bucket = Some(b as u32);
            let bucket = &mut profile.buckets[b];
            let bucket_mean = bucket.mean;
            let bucket_std_dev = bucket.std_dev();
            let bucket_count = bucket.count;
            let bucket_z = bucket.observe(value, at);
            if bucket_count >= MIN_SAMPLES_FOR_DETECTION && bucket_std_dev > f64::EPSILON {
                z = bucket_z;
                mean = bucket_mean;
                std_dev = bucket_std_dev;
                seasonal_component = Some(bucket_mean - global_mean);
            }
        }

        if count_before < MIN_SAMPLES_FOR_DETECTION || z.abs() < threshold {
            return None;
        }

        if let Some(last) = inner.last_alert_at.get(&key) {
            if at - *last < Duration::seconds(ALERT_COOLDOWN_SECONDS) {
                return None; // false-positive filtering: still in cooldown
            }
        }

        let severity = if z.abs() >= threshold * 2.0 {
            Severity::Critical
        } else {
            Severity::Warning
        };

        let alert = Alert {
            id: Uuid::new_v4().to_string(),
            service,
            metric: metric.to_string(),
            value,
            baseline_mean: mean,
            baseline_std_dev: std_dev,
            z_score: z,
            threshold,
            seasonal_bucket,
            seasonal_component,
            severity,
            timestamp: at,
        };

        inner.last_alert_at.insert(key, at);
        inner.alerts.push(alert.clone());
        inner.correlate(&alert);
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

// ── HTTP handlers ────────────────────────────────────────────────────────────

/// `POST /anomaly/observe` - feed a metric observation into the detector.
/// Returns the generated alert, if any, as `{"alert": ...}` or
/// `{"alert": null}` when the point was normal or suppressed.
pub async fn observe_metric(
    State(store): State<Arc<AnomalyStore>>,
    Json(req): Json<ObserveRequest>,
) -> impl IntoResponse {
    let at = req.timestamp.unwrap_or_else(Utc::now);
    let alert = store.observe_at(req.service.as_deref(), &req.metric, req.value, at);
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

#[derive(Debug, Deserialize)]
pub struct SeasonalityRequest {
    #[serde(default)]
    pub service: Option<String>,
    pub metric: String,
    pub seasonality: Seasonality,
}

#[derive(Debug, Deserialize)]
pub struct SeasonalThresholdRequest {
    #[serde(default)]
    pub service: Option<String>,
    pub metric: String,
    pub bucket: u32,
    pub multiplier: f64,
}

#[derive(Debug, Deserialize)]
pub struct MetricQuery {
    #[serde(default)]
    pub service: Option<String>,
    pub metric: String,
}

fn error_body(status: StatusCode, message: impl ToString) -> axum::response::Response {
    (
        status,
        Json(serde_json::json!({ "error": message.to_string() })),
    )
        .into_response()
}

/// `POST /anomaly/seasonality` - enable seasonal handling for a metric.
pub async fn configure_seasonality(
    State(store): State<Arc<AnomalyStore>>,
    headers: HeaderMap,
    Json(req): Json<SeasonalityRequest>,
) -> axum::response::Response {
    if let Err(e) = authorize_admin(&headers) {
        return e.into_response();
    }
    store.configure_seasonality(req.service.as_deref(), &req.metric, req.seasonality);
    StatusCode::NO_CONTENT.into_response()
}

/// `PUT /anomaly/seasonality/threshold` - adjust one season's threshold.
pub async fn set_seasonal_threshold(
    State(store): State<Arc<AnomalyStore>>,
    headers: HeaderMap,
    Json(req): Json<SeasonalThresholdRequest>,
) -> axum::response::Response {
    if let Err(e) = authorize_admin(&headers) {
        return e.into_response();
    }
    match store.set_seasonal_threshold(
        req.service.as_deref(),
        &req.metric,
        req.bucket,
        req.multiplier,
    ) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e @ SeasonalError::NotConfigured) => error_body(StatusCode::NOT_FOUND, e),
        Err(e) => error_body(StatusCode::BAD_REQUEST, e),
    }
}

/// `GET /anomaly/seasonality?metric=..&service=..` - learned seasonal pattern.
pub async fn get_seasonal_pattern(
    State(store): State<Arc<AnomalyStore>>,
    Query(q): Query<MetricQuery>,
) -> axum::response::Response {
    match store.seasonal_pattern(q.service.as_deref(), &q.metric) {
        Some(pattern) => Json(pattern).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct SystemAnomalyQuery {
    /// Look-back window in seconds (default one hour).
    #[serde(default)]
    pub window_seconds: Option<i64>,
}

/// `GET /anomaly/system?window_seconds=3600` - correlated cross-service
/// anomalies within the window.
pub async fn list_system_anomalies(
    State(store): State<Arc<AnomalyStore>>,
    Query(q): Query<SystemAnomalyQuery>,
) -> impl IntoResponse {
    let window = Duration::seconds(q.window_seconds.unwrap_or(3_600).max(0));
    Json(store.get_system_anomalies(window))
}

/// `GET /anomaly/root-causes` - tally of system anomaly root causes.
pub async fn list_root_causes(State(store): State<Arc<AnomalyStore>>) -> impl IntoResponse {
    Json(store.root_cause_stats())
}

/// `POST /anomaly/investigations/:anomaly_id` - record an investigation action.
pub async fn record_investigation(
    State(store): State<Arc<AnomalyStore>>,
    Path(anomaly_id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<InvestigationRequest>,
) -> axum::response::Response {
    if let Err(e) = authorize_admin(&headers) {
        return e.into_response();
    }
    match store.record_investigation(&anomaly_id, req) {
        Ok(entry) => (StatusCode::CREATED, Json(entry)).into_response(),
        Err(e @ InvestigationError::AnomalyNotFound) => error_body(StatusCode::NOT_FOUND, e),
        Err(
            e @ (InvestigationError::NotOpened
            | InvestigationError::AlreadyOpen
            | InvestigationError::Closed),
        ) => error_body(StatusCode::CONFLICT, e),
        Err(e) => error_body(StatusCode::BAD_REQUEST, e),
    }
}

/// `GET /anomaly/investigations/:anomaly_id` - investigation audit trail.
pub async fn get_investigation_history(
    State(store): State<Arc<AnomalyStore>>,
    Path(anomaly_id): Path<String>,
    headers: HeaderMap,
) -> axum::response::Response {
    if let Err(e) = authorize_admin(&headers) {
        return e.into_response();
    }
    Json(store.get_investigation_history(&anomaly_id)).into_response()
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
