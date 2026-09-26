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

/// Maximum gap between two alerts for them to be considered part of the same
/// correlated incident.
const CORRELATION_GAP_SECONDS: i64 = 300;

/// Correlation groups retained in memory; the oldest are pruned beyond this.
const MAX_CORRELATION_GROUPS: usize = 1_000;

/// Service label used for observations that don't name a service.
const DEFAULT_SERVICE: &str = "default";

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
    correlation_groups: Vec<SystemAnomaly>,
    root_causes: HashMap<RootCause, RootCauseStat>,
    investigations: HashMap<String, Vec<InvestigationEntry>>,
}

impl Inner {
    /// Attach `alert` to a correlation group, creating one if no recent
    /// group is within `CORRELATION_GAP_SECONDS`. Records the root cause the
    /// first time a group becomes systemic.
    fn correlate(&mut self, alert: &Alert) {
        let gap = Duration::seconds(CORRELATION_GAP_SECONDS);
        let idx = self.correlation_groups.iter().rposition(|g| {
            alert.timestamp >= g.started_at - gap && alert.timestamp <= g.last_seen_at + gap
        });

        let idx = match idx {
            Some(i) => {
                self.correlation_groups[i].absorb(alert);
                i
            }
            None => {
                let root_cause = RootCause {
                    service: alert.service.clone(),
                    metric: alert.metric.clone(),
                };
                self.correlation_groups.push(SystemAnomaly {
                    id: Uuid::new_v4().to_string(),
                    services: vec![alert.service.clone()],
                    alert_ids: vec![alert.id.clone()],
                    shared_metrics: Vec::new(),
                    root_cause,
                    max_severity: alert.severity,
                    started_at: alert.timestamp,
                    last_seen_at: alert.timestamp,
                    metrics_by_service: vec![(alert.service.clone(), alert.metric.clone())],
                    counted: false,
                });
                if self.correlation_groups.len() > MAX_CORRELATION_GROUPS {
                    self.correlation_groups.remove(0);
                }
                self.correlation_groups.len() - 1
            }
        };

        let group = &mut self.correlation_groups[idx];
        if group.is_systemic() && !group.counted {
            group.counted = true;
            let stat = self
                .root_causes
                .entry(group.root_cause.clone())
                .or_insert_with(|| RootCauseStat {
                    root_cause: group.root_cause.clone(),
                    occurrences: 0,
                    last_seen_at: group.last_seen_at,
                    system_anomaly_ids: Vec::new(),
                });
            stat.occurrences += 1;
            stat.last_seen_at = stat.last_seen_at.max(group.last_seen_at);
            stat.system_anomaly_ids.push(group.id.clone());
            tracing::warn!(
                system_anomaly_id = %group.id,
                services = ?group.services,
                root_service = %group.root_cause.service,
                root_metric = %group.root_cause.metric,
                "cross-service system anomaly detected"
            );
        }
    }

    fn anomaly_exists(&self, anomaly_id: &str) -> bool {
        self.alerts.iter().any(|a| a.id == anomaly_id)
            || self
                .correlation_groups
                .iter()
                .any(|g| g.id == anomaly_id && g.is_systemic())
    }
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

    // ── Seasonal handling (#544) ────────────────────────────────────────────

    /// Enable seasonal decomposition for `metric` on `service`. Reconfiguring
    /// with a different seasonality discards the previously learned seasons;
    /// the same seasonality is a no-op.
    pub fn configure_seasonality(
        &self,
        service: Option<&str>,
        metric: &str,
        seasonality: Seasonality,
    ) {
        let key = metric_key(&service_label(service), metric);
        let mut inner = self.inner.write().expect("anomaly lock poisoned");
        let replace = inner
            .seasonal
            .get(&key)
            .is_none_or(|p| p.seasonality != seasonality);
        if replace {
            inner.seasonal.insert(key, SeasonalProfile::new(seasonality));
        }
    }

    /// Scale the z threshold for one season, e.g. `1.5` to tolerate more
    /// variance during a known-volatile month.
    pub fn set_seasonal_threshold(
        &self,
        service: Option<&str>,
        metric: &str,
        bucket: u32,
        multiplier: f64,
    ) -> Result<(), SeasonalError> {
        if !multiplier.is_finite() || multiplier <= 0.0 {
            return Err(SeasonalError::InvalidMultiplier);
        }
        let key = metric_key(&service_label(service), metric);
        let mut inner = self.inner.write().expect("anomaly lock poisoned");
        let profile = inner
            .seasonal
            .get_mut(&key)
            .ok_or(SeasonalError::NotConfigured)?;
        let slot = profile
            .threshold_multipliers
            .get_mut(bucket as usize)
            .ok_or(SeasonalError::BucketOutOfRange)?;
        *slot = multiplier;
        Ok(())
    }

    /// The learned seasonal pattern for a metric, if seasonality is enabled.
    pub fn seasonal_pattern(&self, service: Option<&str>, metric: &str) -> Option<SeasonalPattern> {
        let service = service_label(service);
        let key = metric_key(&service, metric);
        let inner = self.inner.read().expect("anomaly lock poisoned");
        let profile = inner.seasonal.get(&key)?;
        let global = inner.baselines.get(&key).cloned().unwrap_or_default();
        let components = profile
            .buckets
            .iter()
            .zip(&profile.threshold_multipliers)
            .enumerate()
            .map(|(i, (b, &mult))| SeasonalComponent {
                bucket: i as u32,
                count: b.count,
                mean: b.mean,
                std_dev: b.std_dev(),
                seasonal_offset: if b.count > 0 { b.mean - global.mean } else { 0.0 },
                threshold_multiplier: mult,
                effective_threshold: DEFAULT_Z_THRESHOLD * mult,
                trained: b.count >= MIN_SAMPLES_FOR_DETECTION,
            })
            .collect();
        Some(SeasonalPattern {
            service,
            metric: metric.to_string(),
            seasonality: profile.seasonality,
            global_mean: global.mean,
            global_std_dev: global.std_dev(),
            components,
        })
    }

    // ── Cross-service correlation (#545) ────────────────────────────────────

    /// System anomalies (correlated alerts spanning 2+ services) whose most
    /// recent alert falls within `time_window` of now, newest first.
    pub fn get_system_anomalies(&self, time_window: Duration) -> Vec<SystemAnomaly> {
        self.system_anomalies_at(time_window, Utc::now())
    }

    /// As [`Self::get_system_anomalies`], relative to an explicit `now`.
    pub fn system_anomalies_at(
        &self,
        time_window: Duration,
        now: DateTime<Utc>,
    ) -> Vec<SystemAnomaly> {
        let cutoff = now - time_window;
        let inner = self.inner.read().expect("anomaly lock poisoned");
        inner
            .correlation_groups
            .iter()
            .rev()
            .filter(|g| g.is_systemic() && g.last_seen_at >= cutoff && g.started_at <= now)
            .cloned()
            .collect()
    }

    /// Root causes of system anomalies, most frequent first.
    pub fn root_cause_stats(&self) -> Vec<RootCauseStat> {
        let inner = self.inner.read().expect("anomaly lock poisoned");
        let mut stats: Vec<RootCauseStat> = inner.root_causes.values().cloned().collect();
        stats.sort_by(|a, b| {
            b.occurrences
                .cmp(&a.occurrences)
                .then(b.last_seen_at.cmp(&a.last_seen_at))
        });
        stats
    }

    // ── Investigation audit trail (#546) ────────────────────────────────────

    /// Append an investigation action to `anomaly_id`'s audit trail.
    /// `anomaly_id` may be an alert id or a system anomaly id.
    ///
    /// Lifecycle: the first action must be `opened`; a closed investigation
    /// accepts only `reopened`. `decision_recorded` and `closed` require a
    /// decision; `assigned` requires an assignee.
    pub fn record_investigation(
        &self,
        anomaly_id: &str,
        req: InvestigationRequest,
    ) -> Result<InvestigationEntry, InvestigationError> {
        let investigator = req.investigator.trim().to_string();
        if investigator.is_empty() {
            return Err(InvestigationError::MissingInvestigator);
        }

        let mut inner = self.inner.write().expect("anomaly lock poisoned");
        if !inner.anomaly_exists(anomaly_id) {
            return Err(InvestigationError::AnomalyNotFound);
        }

        let history = inner
            .investigations
            .entry(anomaly_id.to_string())
            .or_default();
        let current = history.last().map(|e| e.status);

        let status = match (current, req.action) {
            (None, InvestigationAction::Opened) => InvestigationStatus::Open,
            (None, _) => return Err(InvestigationError::NotOpened),
            (Some(InvestigationStatus::Open), InvestigationAction::Opened)
            | (Some(InvestigationStatus::Open), InvestigationAction::Reopened) => {
                return Err(InvestigationError::AlreadyOpen)
            }
            (Some(InvestigationStatus::Closed), InvestigationAction::Reopened) => {
                InvestigationStatus::Open
            }
            (Some(InvestigationStatus::Closed), _) => return Err(InvestigationError::Closed),
            (Some(InvestigationStatus::Open), InvestigationAction::Closed) => {
                InvestigationStatus::Closed
            }
            (Some(InvestigationStatus::Open), _) => InvestigationStatus::Open,
        };

        match req.action {
            InvestigationAction::DecisionRecorded | InvestigationAction::Closed
                if req.decision.is_none() =>
            {
                return Err(InvestigationError::MissingDecision)
            }
            InvestigationAction::Assigned
                if req.assignee.as_deref().is_none_or(|a| a.trim().is_empty()) =>
            {
                return Err(InvestigationError::MissingAssignee)
            }
            _ => {}
        }

        let entry = InvestigationEntry {
            id: Uuid::new_v4().to_string(),
            anomaly_id: anomaly_id.to_string(),
            sequence: history.len() as u64 + 1,
            investigator,
            action: req.action,
            decision: req.decision,
            assignee: req.assignee.map(|a| a.trim().to_string()),
            notes: req.notes,
            status,
            timestamp: Utc::now(),
        };

        tracing::info!(
            anomaly_id = %entry.anomaly_id,
            investigator = %entry.investigator,
            action = ?entry.action,
            decision = ?entry.decision,
            status = ?entry.status,
            "anomaly investigation action recorded"
        );

        history.push(entry.clone());
        Ok(entry)
    }

    /// Full, ordered investigation history for `anomaly_id` (empty if none).
    pub fn get_investigation_history(&self, anomaly_id: &str) -> Vec<InvestigationEntry> {
        self.inner
            .read()
            .expect("anomaly lock poisoned")
            .investigations
            .get(anomaly_id)
            .cloned()
            .unwrap_or_default()
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
}
