//! SLA Monitoring and Reporting — Issue #600
//!
//! SLA compliance was previously untracked, making SLA enforcement impossible.
//! This module introduces per-service SLA policies, real-time compliance
//! tracking, breach detection, and a reporting API.
//!
//! # Architecture
//!
//! ```text
//! POST /sla/policies                     → create_sla_policy
//! GET  /sla/policies                     → list_sla_policies
//! GET  /sla/policies/:id                 → get_sla_policy
//! PUT  /sla/policies/:id                 → update_sla_policy
//! DELETE /sla/policies/:id               → delete_sla_policy
//! POST /sla/records                      → record_sla_measurement
//! GET  /sla/report                       → get_sla_report
//! GET  /sla/breaches                     → list_sla_breaches
//! GET  /sla/compliance                   → get_compliance_summary
//! ```
//!
//! # SLA Metrics Tracked
//!
//! | Metric            | Description                                     |
//! |-------------------|-------------------------------------------------|
//! | Availability      | % time the service is reachable and healthy     |
//! | Latency (p99)     | 99th-percentile API response time in ms         |
//! | Error Rate        | % of requests that return a 5xx response        |
//! | Throughput        | Minimum acceptable requests per second          |
//! | Check-in Lag      | Max seconds between vault check-in and record   |

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Domain types ──────────────────────────────────────────────────────────────

/// The aspect of service quality that an SLA policy governs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SlaMetricType {
    /// Percentage (0–100) of time the service must be reachable and healthy.
    Availability,
    /// 99th-percentile response latency in milliseconds.
    LatencyP99Ms,
    /// Maximum fraction (0.0–1.0) of requests that may return a 5xx error.
    ErrorRate,
    /// Minimum acceptable request throughput (requests per second).
    Throughput,
    /// Maximum lag in seconds between a vault check-in event and its recording.
    CheckInLagSeconds,
    /// Custom metric with a caller-supplied name.
    Custom(String),
}

/// Whether the SLA target is a maximum or minimum bound.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SlaDirection {
    /// Measured value must stay **at or below** the target (e.g. latency, error rate).
    AtMost,
    /// Measured value must stay **at or above** the target (e.g. availability, throughput).
    AtLeast,
}

/// Severity assigned to a breach of this SLA policy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SlaSeverity {
    Info,
    Warning,
    Critical,
}

/// A configured SLA policy for a service or endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlaPolicy {
    /// Unique identifier (UUID v4).
    pub id: String,
    /// Human-readable name, e.g. "API Availability SLA".
    pub name: String,
    /// Optional description / SLA contract reference.
    pub description: String,
    /// Which service or endpoint this policy applies to.
    pub service: String,
    /// The metric being measured.
    pub metric_type: SlaMetricType,
    /// Numeric target value (units depend on `metric_type`).
    pub target: f64,
    /// Whether the measured value must be at-most or at-least `target`.
    pub direction: SlaDirection,
    /// Severity of a breach.
    pub severity: SlaSeverity,
    /// Rolling window over which compliance is computed, in minutes.
    pub window_minutes: u64,
    /// Whether this policy is active.
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A single observed measurement for an SLA metric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlaMeasurement {
    /// Unique identifier.
    pub id: String,
    /// The policy this measurement is associated with.
    pub policy_id: String,
    /// Observed numeric value.
    pub value: f64,
    /// Optional free-text context (e.g. endpoint, region).
    pub context: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

/// A breach record created when a measurement violates an SLA policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlaBreach {
    /// Unique identifier.
    pub id: String,
    /// The policy that was breached.
    pub policy_id: String,
    /// Name of the breached policy (denormalised for easy querying).
    pub policy_name: String,
    /// Severity of the breach.
    pub severity: SlaSeverity,
    /// The SLA target that was not met.
    pub target: f64,
    /// The observed value that triggered the breach.
    pub observed_value: f64,
    /// Human-readable description of the breach.
    pub message: String,
    pub breached_at: DateTime<Utc>,
}

/// Per-policy compliance summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlaComplianceSummary {
    pub policy_id: String,
    pub policy_name: String,
    pub service: String,
    pub metric_type: SlaMetricType,
    pub target: f64,
    pub direction: SlaDirection,
    /// Number of measurements in the rolling window.
    pub measurements_in_window: usize,
    /// Number of those measurements that are compliant.
    pub compliant_count: usize,
    /// Compliance percentage (0–100).
    pub compliance_pct: f64,
    /// Most recent observed value (`None` if no measurements yet).
    pub latest_value: Option<f64>,
    /// Whether the policy is currently in breach.
    pub currently_breaching: bool,
    pub window_minutes: u64,
    pub evaluated_at: DateTime<Utc>,
}

/// Overall SLA report for all enabled policies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlaReport {
    pub generated_at: DateTime<Utc>,
    pub total_policies: usize,
    pub breaching_policies: usize,
    pub overall_compliance_pct: f64,
    pub summaries: Vec<SlaComplianceSummary>,
    /// All breaches recorded since the report window started.
    pub recent_breaches: Vec<SlaBreach>,
}

// ── Request / response bodies ─────────────────────────────────────────────────

/// Request body for `POST /sla/policies`.
#[derive(Debug, Deserialize)]
pub struct CreateSlaPolicyRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub service: String,
    pub metric_type: SlaMetricType,
    pub target: f64,
    pub direction: SlaDirection,
    #[serde(default = "default_severity")]
    pub severity: SlaSeverity,
    #[serde(default = "default_window_minutes")]
    pub window_minutes: u64,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_severity() -> SlaSeverity {
    SlaSeverity::Warning
}
fn default_window_minutes() -> u64 {
    60
}
fn default_enabled() -> bool {
    true
}

/// Request body for `PUT /sla/policies/:id`.
#[derive(Debug, Deserialize)]
pub struct UpdateSlaPolicyRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub service: Option<String>,
    pub target: Option<f64>,
    pub direction: Option<SlaDirection>,
    pub severity: Option<SlaSeverity>,
    pub window_minutes: Option<u64>,
    pub enabled: Option<bool>,
}

/// Request body for `POST /sla/records`.
#[derive(Debug, Deserialize)]
pub struct RecordMeasurementRequest {
    pub policy_id: String,
    pub value: f64,
    pub context: Option<String>,
}

/// Query params for `GET /sla/report`.
#[derive(Debug, Deserialize)]
pub struct SlaReportQuery {
    /// Only include policies for this service.
    pub service: Option<String>,
}

/// Query params for `GET /sla/breaches`.
#[derive(Debug, Deserialize)]
pub struct SlaBreachQuery {
    /// Filter by policy id.
    pub policy_id: Option<String>,
    /// Maximum number of results to return (default 50).
    pub limit: Option<usize>,
}

// ── Shared state ──────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct SlaInner {
    pub policies: HashMap<String, SlaPolicy>,
    pub measurements: Vec<SlaMeasurement>,
    pub breaches: Vec<SlaBreach>,
}

pub type SlaStore = Arc<Mutex<SlaInner>>;

/// Shared state handle passed as axum extractor.
#[derive(Clone)]
pub struct SlaState {
    pub store: SlaStore,
}

impl SlaState {
    pub fn new() -> Self {
        let mut inner = SlaInner::default();
        seed_default_policies(&mut inner);
        Self {
            store: Arc::new(Mutex::new(inner)),
        }
    }
}

impl Default for SlaState {
    fn default() -> Self {
        Self::new()
    }
}

/// Seed sensible out-of-the-box SLA policies covering the most important
/// Ethos-Protocol service dimensions.
fn seed_default_policies(inner: &mut SlaInner) {
    let now = Utc::now();

    let defaults = vec![
        SlaPolicy {
            id: Uuid::new_v4().to_string(),
            name: "API Availability".into(),
            description: "The backend API must be reachable ≥ 99.9% of the time.".into(),
            service: "backend-api".into(),
            metric_type: SlaMetricType::Availability,
            target: 99.9,
            direction: SlaDirection::AtLeast,
            severity: SlaSeverity::Critical,
            window_minutes: 60,
            enabled: true,
            created_at: now,
            updated_at: now,
        },
        SlaPolicy {
            id: Uuid::new_v4().to_string(),
            name: "API p99 Latency".into(),
            description: "99th-percentile response time must stay below 500 ms.".into(),
            service: "backend-api".into(),
            metric_type: SlaMetricType::LatencyP99Ms,
            target: 500.0,
            direction: SlaDirection::AtMost,
            severity: SlaSeverity::Warning,
            window_minutes: 60,
            enabled: true,
            created_at: now,
            updated_at: now,
        },
        SlaPolicy {
            id: Uuid::new_v4().to_string(),
            name: "API Error Rate".into(),
            description: "5xx error rate must remain below 1% of requests.".into(),
            service: "backend-api".into(),
            metric_type: SlaMetricType::ErrorRate,
            target: 0.01,
            direction: SlaDirection::AtMost,
            severity: SlaSeverity::Critical,
            window_minutes: 30,
            enabled: true,
            created_at: now,
            updated_at: now,
        },
        SlaPolicy {
            id: Uuid::new_v4().to_string(),
            name: "Vault Check-in Lag".into(),
            description: "Check-in events must be recorded within 60 seconds.".into(),
            service: "vault-check-in".into(),
            metric_type: SlaMetricType::CheckInLagSeconds,
            target: 60.0,
            direction: SlaDirection::AtMost,
            severity: SlaSeverity::Warning,
            window_minutes: 60,
            enabled: true,
            created_at: now,
            updated_at: now,
        },
    ];

    for p in defaults {
        inner.policies.insert(p.id.clone(), p);
    }
}

// ── Core compliance logic ─────────────────────────────────────────────────────

/// Determine whether a single measurement satisfies the SLA target.
pub fn is_compliant(value: f64, target: f64, direction: SlaDirection) -> bool {
    match direction {
        SlaDirection::AtLeast => value >= target,
        SlaDirection::AtMost => value <= target,
    }
}

/// Compute the compliance summary for one policy given all stored measurements.
pub fn compute_summary(
    policy: &SlaPolicy,
    measurements: &[SlaMeasurement],
    now: DateTime<Utc>,
) -> SlaComplianceSummary {
    let window_start = now - chrono::Duration::minutes(policy.window_minutes as i64);

    let in_window: Vec<&SlaMeasurement> = measurements
        .iter()
        .filter(|m| m.policy_id == policy.id && m.recorded_at >= window_start)
        .collect();

    let total = in_window.len();
    let compliant = in_window
        .iter()
        .filter(|m| is_compliant(m.value, policy.target, policy.direction))
        .count();

    let compliance_pct = if total == 0 {
        100.0 // no data → assume compliant (optimistic default)
    } else {
        (compliant as f64 / total as f64) * 100.0
    };

    let latest_value = in_window
        .iter()
        .max_by_key(|m| m.recorded_at)
        .map(|m| m.value);

    let currently_breaching = latest_value
        .map(|v| !is_compliant(v, policy.target, policy.direction))
        .unwrap_or(false);

    SlaComplianceSummary {
        policy_id: policy.id.clone(),
        policy_name: policy.name.clone(),
        service: policy.service.clone(),
        metric_type: policy.metric_type.clone(),
        target: policy.target,
        direction: policy.direction,
        measurements_in_window: total,
        compliant_count: compliant,
        compliance_pct,
        latest_value,
        currently_breaching,
        window_minutes: policy.window_minutes,
        evaluated_at: now,
    }
}

// ── HTTP handlers ─────────────────────────────────────────────────────────────

/// `POST /sla/policies` — create a new SLA policy.
pub async fn create_sla_policy(
    State(state): State<Arc<SlaState>>,
    Json(body): Json<CreateSlaPolicyRequest>,
) -> Result<(StatusCode, Json<SlaPolicy>), (StatusCode, Json<serde_json::Value>)> {
    if body.name.trim().is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "name must not be empty"})),
        ));
    }
    if body.service.trim().is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "service must not be empty"})),
        ));
    }
    if body.window_minutes == 0 {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "window_minutes must be > 0"})),
        ));
    }

    let now = Utc::now();
    let policy = SlaPolicy {
        id: Uuid::new_v4().to_string(),
        name: body.name,
        description: body.description,
        service: body.service,
        metric_type: body.metric_type,
        target: body.target,
        direction: body.direction,
        severity: body.severity,
        window_minutes: body.window_minutes,
        enabled: body.enabled,
        created_at: now,
        updated_at: now,
    };

    let mut inner = state.store.lock().expect("sla store poisoned");
    inner.policies.insert(policy.id.clone(), policy.clone());
    Ok((StatusCode::CREATED, Json(policy)))
}

/// `GET /sla/policies` — list all SLA policies.
pub async fn list_sla_policies(
    State(state): State<Arc<SlaState>>,
) -> Json<Vec<SlaPolicy>> {
    let inner = state.store.lock().expect("sla store poisoned");
    let mut policies: Vec<SlaPolicy> = inner.policies.values().cloned().collect();
    policies.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    Json(policies)
}

/// `GET /sla/policies/:id` — retrieve a single SLA policy.
pub async fn get_sla_policy(
    State(state): State<Arc<SlaState>>,
    Path(id): Path<String>,
) -> Result<Json<SlaPolicy>, (StatusCode, Json<serde_json::Value>)> {
    let inner = state.store.lock().expect("sla store poisoned");
    inner
        .policies
        .get(&id)
        .cloned()
        .map(Json)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "policy not found"})),
            )
        })
}

/// `PUT /sla/policies/:id` — update an existing SLA policy.
pub async fn update_sla_policy(
    State(state): State<Arc<SlaState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateSlaPolicyRequest>,
) -> Result<Json<SlaPolicy>, (StatusCode, Json<serde_json::Value>)> {
    let mut inner = state.store.lock().expect("sla store poisoned");
    let policy = inner.policies.get_mut(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "policy not found"})),
        )
    })?;

    if let Some(name) = body.name {
        policy.name = name;
    }
    if let Some(description) = body.description {
        policy.description = description;
    }
    if let Some(service) = body.service {
        policy.service = service;
    }
    if let Some(target) = body.target {
        policy.target = target;
    }
    if let Some(direction) = body.direction {
        policy.direction = direction;
    }
    if let Some(severity) = body.severity {
        policy.severity = severity;
    }
    if let Some(window_minutes) = body.window_minutes {
        if window_minutes == 0 {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({"error": "window_minutes must be > 0"})),
            ));
        }
        policy.window_minutes = window_minutes;
    }
    if let Some(enabled) = body.enabled {
        policy.enabled = enabled;
    }
    policy.updated_at = Utc::now();

    Ok(Json(policy.clone()))
}

/// `DELETE /sla/policies/:id` — delete an SLA policy.
pub async fn delete_sla_policy(
    State(state): State<Arc<SlaState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let mut inner = state.store.lock().expect("sla store poisoned");
    if inner.policies.remove(&id).is_some() {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "policy not found"})),
        ))
    }
}

/// `POST /sla/records` — record a new SLA measurement and check for breaches.
pub async fn record_sla_measurement(
    State(state): State<Arc<SlaState>>,
    Json(body): Json<RecordMeasurementRequest>,
) -> Result<(StatusCode, Json<SlaMeasurement>), (StatusCode, Json<serde_json::Value>)> {
    let mut inner = state.store.lock().expect("sla store poisoned");

    // Validate that the referenced policy exists and is enabled.
    let policy = inner
        .policies
        .get(&body.policy_id)
        .cloned()
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "policy not found"})),
            )
        })?;

    if !policy.enabled {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "policy is disabled"})),
        ));
    }

    let measurement = SlaMeasurement {
        id: Uuid::new_v4().to_string(),
        policy_id: body.policy_id.clone(),
        value: body.value,
        context: body.context,
        recorded_at: Utc::now(),
    };

    inner.measurements.push(measurement.clone());

    // Breach detection: if the new measurement violates the target, record a breach.
    if !is_compliant(body.value, policy.target, policy.direction) {
        let direction_word = match policy.direction {
            SlaDirection::AtLeast => "below minimum",
            SlaDirection::AtMost => "above maximum",
        };
        let breach = SlaBreach {
            id: Uuid::new_v4().to_string(),
            policy_id: policy.id.clone(),
            policy_name: policy.name.clone(),
            severity: policy.severity,
            target: policy.target,
            observed_value: body.value,
            message: format!(
                "SLA breach for '{}' on service '{}': observed {:.4} is {} target {:.4}",
                policy.name, policy.service, body.value, direction_word, policy.target
            ),
            breached_at: Utc::now(),
        };
        inner.breaches.push(breach);
    }

    Ok((StatusCode::CREATED, Json(measurement)))
}

/// `GET /sla/report` — generate a full SLA compliance report.
pub async fn get_sla_report(
    State(state): State<Arc<SlaState>>,
    Query(query): Query<SlaReportQuery>,
) -> Json<SlaReport> {
    let inner = state.store.lock().expect("sla store poisoned");
    let now = Utc::now();

    let policies: Vec<&SlaPolicy> = inner
        .policies
        .values()
        .filter(|p| p.enabled)
        .filter(|p| {
            query
                .service
                .as_ref()
                .map_or(true, |svc| &p.service == svc)
        })
        .collect();

    let summaries: Vec<SlaComplianceSummary> = policies
        .iter()
        .map(|p| compute_summary(p, &inner.measurements, now))
        .collect();

    let total_policies = summaries.len();
    let breaching_policies = summaries.iter().filter(|s| s.currently_breaching).count();
    let overall_compliance_pct = if total_policies == 0 {
        100.0
    } else {
        summaries.iter().map(|s| s.compliance_pct).sum::<f64>() / total_policies as f64
    };

    // Include breaches from within the widest window used by any policy.
    let max_window = policies
        .iter()
        .map(|p| p.window_minutes)
        .max()
        .unwrap_or(60);
    let breach_start = now - chrono::Duration::minutes(max_window as i64);
    let recent_breaches: Vec<SlaBreach> = inner
        .breaches
        .iter()
        .filter(|b| b.breached_at >= breach_start)
        .filter(|b| {
            query.service.as_ref().map_or(true, |svc| {
                inner
                    .policies
                    .get(&b.policy_id)
                    .map_or(false, |p| &p.service == svc)
            })
        })
        .cloned()
        .collect();

    Json(SlaReport {
        generated_at: now,
        total_policies,
        breaching_policies,
        overall_compliance_pct,
        summaries,
        recent_breaches,
    })
}

/// `GET /sla/breaches` — list recorded SLA breaches.
pub async fn list_sla_breaches(
    State(state): State<Arc<SlaState>>,
    Query(query): Query<SlaBreachQuery>,
) -> Json<Vec<SlaBreach>> {
    let inner = state.store.lock().expect("sla store poisoned");
    let limit = query.limit.unwrap_or(50);

    let mut breaches: Vec<SlaBreach> = inner
        .breaches
        .iter()
        .filter(|b| {
            query
                .policy_id
                .as_ref()
                .map_or(true, |pid| &b.policy_id == pid)
        })
        .cloned()
        .collect();

    // Return most-recent breaches first.
    breaches.sort_by(|a, b| b.breached_at.cmp(&a.breached_at));
    breaches.truncate(limit);
    Json(breaches)
}

/// `GET /sla/compliance` — per-policy compliance summaries (lightweight view,
/// no breach list).
pub async fn get_compliance_summary(
    State(state): State<Arc<SlaState>>,
    Query(query): Query<SlaReportQuery>,
) -> Json<Vec<SlaComplianceSummary>> {
    let inner = state.store.lock().expect("sla store poisoned");
    let now = Utc::now();

    let mut summaries: Vec<SlaComplianceSummary> = inner
        .policies
        .values()
        .filter(|p| p.enabled)
        .filter(|p| {
            query
                .service
                .as_ref()
                .map_or(true, |svc| &p.service == svc)
        })
        .map(|p| compute_summary(p, &inner.measurements, now))
        .collect();

    summaries.sort_by(|a, b| a.policy_name.cmp(&b.policy_name));
    Json(summaries)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_policy(direction: SlaDirection, target: f64) -> SlaPolicy {
        let now = Utc::now();
        SlaPolicy {
            id: "test-policy".into(),
            name: "Test".into(),
            description: String::new(),
            service: "test-svc".into(),
            metric_type: SlaMetricType::LatencyP99Ms,
            target,
            direction,
            severity: SlaSeverity::Warning,
            window_minutes: 60,
            enabled: true,
            created_at: now,
            updated_at: now,
        }
    }

    fn make_measurement(policy_id: &str, value: f64, offset_secs: i64) -> SlaMeasurement {
        SlaMeasurement {
            id: Uuid::new_v4().to_string(),
            policy_id: policy_id.into(),
            value,
            context: None,
            recorded_at: Utc::now() - chrono::Duration::seconds(offset_secs),
        }
    }

    #[test]
    fn test_is_compliant_at_most() {
        assert!(is_compliant(400.0, 500.0, SlaDirection::AtMost));
        assert!(is_compliant(500.0, 500.0, SlaDirection::AtMost));
        assert!(!is_compliant(501.0, 500.0, SlaDirection::AtMost));
    }

    #[test]
    fn test_is_compliant_at_least() {
        assert!(is_compliant(99.9, 99.9, SlaDirection::AtLeast));
        assert!(is_compliant(100.0, 99.9, SlaDirection::AtLeast));
        assert!(!is_compliant(99.8, 99.9, SlaDirection::AtLeast));
    }

    #[test]
    fn test_compute_summary_no_measurements() {
        let policy = make_policy(SlaDirection::AtMost, 500.0);
        let summary = compute_summary(&policy, &[], Utc::now());
        assert_eq!(summary.measurements_in_window, 0);
        // No data is treated as 100% compliant (optimistic default).
        assert!((summary.compliance_pct - 100.0).abs() < f64::EPSILON);
        assert!(!summary.currently_breaching);
        assert!(summary.latest_value.is_none());
    }

    #[test]
    fn test_compute_summary_all_compliant() {
        let policy = make_policy(SlaDirection::AtMost, 500.0);
        let measurements = vec![
            make_measurement("test-policy", 200.0, 100),
            make_measurement("test-policy", 300.0, 200),
            make_measurement("test-policy", 499.0, 300),
        ];
        let summary = compute_summary(&policy, &measurements, Utc::now());
        assert_eq!(summary.measurements_in_window, 3);
        assert_eq!(summary.compliant_count, 3);
        assert!((summary.compliance_pct - 100.0).abs() < f64::EPSILON);
        assert!(!summary.currently_breaching);
    }

    #[test]
    fn test_compute_summary_partial_breach() {
        let policy = make_policy(SlaDirection::AtMost, 500.0);
        let measurements = vec![
            make_measurement("test-policy", 200.0, 300),
            make_measurement("test-policy", 600.0, 200),  // breaching
            make_measurement("test-policy", 600.0, 100),  // breaching (latest)
        ];
        let summary = compute_summary(&policy, &measurements, Utc::now());
        assert_eq!(summary.measurements_in_window, 3);
        assert_eq!(summary.compliant_count, 1);
        // latest value (most recent by recorded_at) is 600 → currently breaching
        assert!(summary.currently_breaching);
    }

    #[test]
    fn test_compute_summary_excludes_old_measurements() {
        let policy = make_policy(SlaDirection::AtMost, 500.0);
        // 120 minutes ago — outside the 60-minute window.
        let old = make_measurement("test-policy", 9000.0, 7201);
        // 10 minutes ago — inside window.
        let recent = make_measurement("test-policy", 100.0, 600);
        let measurements = vec![old, recent];
        let summary = compute_summary(&policy, &measurements, Utc::now());
        assert_eq!(summary.measurements_in_window, 1);
        assert!(!summary.currently_breaching);
    }

    #[test]
    fn test_seed_default_policies() {
        let state = SlaState::new();
        let inner = state.store.lock().unwrap();
        assert!(
            inner.policies.len() >= 4,
            "expected at least 4 default policies, got {}",
            inner.policies.len()
        );
    }

    #[test]
    fn test_breach_message_format() {
        let policy = make_policy(SlaDirection::AtMost, 500.0);
        let mut inner = SlaInner::default();
        inner.policies.insert(policy.id.clone(), policy.clone());

        // Simulate what record_sla_measurement does when a breach is detected.
        let value = 750.0_f64;
        if !is_compliant(value, policy.target, policy.direction) {
            let breach = SlaBreach {
                id: Uuid::new_v4().to_string(),
                policy_id: policy.id.clone(),
                policy_name: policy.name.clone(),
                severity: policy.severity,
                target: policy.target,
                observed_value: value,
                message: format!(
                    "SLA breach for '{}' on service '{}': observed {:.4} is above maximum target {:.4}",
                    policy.name, policy.service, value, policy.target
                ),
                breached_at: Utc::now(),
            };
            inner.breaches.push(breach);
        }

        assert_eq!(inner.breaches.len(), 1);
        assert!(inner.breaches[0].message.contains("above maximum"));
    }
}
