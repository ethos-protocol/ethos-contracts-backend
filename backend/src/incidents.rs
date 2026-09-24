//! Incident response workflow.
//!
//! Incident response was previously ad-hoc: engineers coordinated over chat
//! with no consistent record of severity, ownership, or timeline. This
//! module implements structured incident tracking with severity
//! classification, an escalation workflow, and a per-incident timeline so
//! response follows a consistent process end to end.
//!
//! # Architecture
//!
//! ```text
//! POST /incidents                     → create_incident
//! GET  /incidents                     → list_incidents
//! GET  /incidents/:id                 → get_incident
//! POST /incidents/:id/timeline        → add_timeline_entry
//! POST /incidents/:id/status          → update_incident_status
//! POST /incidents/:id/escalate        → escalate_incident
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Standard severity classification for an incident.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum IncidentSeverity {
    /// Full outage or data loss; page immediately.
    Sev1,
    /// Major functionality degraded for many users.
    Sev2,
    /// Minor functionality degraded or workaround available.
    Sev3,
    /// Cosmetic or low-impact issue.
    Sev4,
}

impl IncidentSeverity {
    /// Maximum time, in minutes, before an unresolved incident of this
    /// severity should be escalated to the next tier.
    pub fn escalation_sla_minutes(&self) -> i64 {
        match self {
            IncidentSeverity::Sev1 => 10,
            IncidentSeverity::Sev2 => 30,
            IncidentSeverity::Sev3 => 120,
            IncidentSeverity::Sev4 => 480,
        }
    }

    /// Maximum time, in minutes, the primary on-call has to acknowledge an
    /// incident of this severity before it is escalated to the next tier.
    pub fn acknowledgment_timeout_minutes(&self) -> i64 {
        match self {
            IncidentSeverity::Sev1 => 5,
            IncidentSeverity::Sev2 => 15,
            IncidentSeverity::Sev3 => 60,
            IncidentSeverity::Sev4 => 240,
        }
    }
}

/// Lifecycle status of an incident.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IncidentStatus {
    Open,
    Investigating,
    Mitigated,
    Resolved,
    Closed,
}

/// A single entry in an incident's timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEntry {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub actor: String,
    pub note: String,
}

/// A tracked incident.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Incident {
    pub id: String,
    pub title: String,
    pub description: String,
    pub severity: IncidentSeverity,
    pub status: IncidentStatus,
    pub escalation_level: u32,
    pub assigned_to: Option<String>,
    pub timeline: Vec<TimelineEntry>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// When the current tier acknowledged the incident, if it has.
    pub acknowledged_at: Option<DateTime<Utc>>,
    /// Deadline by which the current tier must acknowledge before escalation.
    pub acknowledgment_deadline: Option<DateTime<Utc>>,
}

/// Request body for `POST /incidents`.
#[derive(Debug, Deserialize)]
pub struct CreateIncidentRequest {
    pub title: String,
    pub description: String,
    pub severity: IncidentSeverity,
    pub assigned_to: Option<String>,
}

/// Request body for `POST /incidents/:id/timeline`.
#[derive(Debug, Deserialize)]
pub struct AddTimelineEntryRequest {
    pub actor: String,
    pub note: String,
}

/// Request body for `POST /incidents/:id/status`.
#[derive(Debug, Deserialize)]
pub struct UpdateStatusRequest {
    pub status: IncidentStatus,
    pub actor: String,
}

/// Request body for `POST /incidents/:id/escalate`.
#[derive(Debug, Deserialize)]
pub struct EscalateIncidentRequest {
    pub reason: String,
    pub actor: String,
}

/// Request body for `POST /incidents/:id/acknowledge`.
#[derive(Debug, Deserialize)]
pub struct AcknowledgeIncidentRequest {
    pub actor: String,
}

pub type IncidentStore = Arc<Mutex<HashMap<String, Incident>>>;

pub fn create_incident_store() -> IncidentStore {
    Arc::new(Mutex::new(HashMap::new()))
}

#[derive(Clone)]
pub struct IncidentState {
    pub store: IncidentStore,
}

impl IncidentState {
    pub fn new() -> Self {
        Self {
            store: create_incident_store(),
        }
    }
}

impl Default for IncidentState {
    fn default() -> Self {
        Self::new()
    }
}

fn timeline_entry(actor: impl Into<String>, note: impl Into<String>) -> TimelineEntry {
    TimelineEntry {
        id: Uuid::new_v4().to_string(),
        timestamp: Utc::now(),
        actor: actor.into(),
        note: note.into(),
    }
}

fn new_incident(
    title: String,
    description: String,
    severity: IncidentSeverity,
    assigned_to: Option<String>,
) -> Incident {
    let now = Utc::now();
    let deadline = now + Duration::minutes(severity.acknowledgment_timeout_minutes());
    Incident {
        id: Uuid::new_v4().to_string(),
        title,
        description,
        severity,
        status: IncidentStatus::Open,
        escalation_level: 0,
        assigned_to,
        timeline: vec![timeline_entry("system", "incident opened")],
        created_at: now,
        updated_at: now,
        acknowledged_at: None,
        acknowledgment_deadline: Some(deadline),
    }
}

/// Open an incident directly against `store`, bypassing the HTTP layer.
///
/// For callers outside a request context — background jobs and validation
/// routines (e.g. backup checksum verification) — that detect a problem and
/// need to raise an alert without going through `POST /incidents`.
pub fn open_incident(
    store: &IncidentStore,
    title: impl Into<String>,
    description: impl Into<String>,
    severity: IncidentSeverity,
) -> Incident {
    let incident = new_incident(title.into(), description.into(), severity, None);

    tracing::warn!(
        incident_id = %incident.id,
        severity = ?incident.severity,
        "incident opened"
    );

    store.lock().unwrap().insert(incident.id.clone(), incident.clone());
    incident
}

/// `POST /incidents` — open a new incident with severity classification.
pub async fn create_incident(
    State(state): State<Arc<IncidentState>>,
    Json(body): Json<CreateIncidentRequest>,
) -> (StatusCode, Json<Incident>) {
    let mut incident = open_incident(&state.store, body.title, body.description, body.severity);
    incident.assigned_to = body.assigned_to.clone();
    state
        .store
        .lock()
        .unwrap()
        .insert(incident.id.clone(), incident.clone());

    (StatusCode::CREATED, Json(incident))
}

/// `GET /incidents` — list all tracked incidents.
pub async fn list_incidents(State(state): State<Arc<IncidentState>>) -> Json<Vec<Incident>> {
    let store = state.store.lock().unwrap();
    Json(store.values().cloned().collect())
}

/// `GET /incidents/:id` — fetch a single incident.
pub async fn get_incident(
    State(state): State<Arc<IncidentState>>,
    Path(id): Path<String>,
) -> Result<Json<Incident>, StatusCode> {
    let store = state.store.lock().unwrap();
    store.get(&id).cloned().map(Json).ok_or(StatusCode::NOT_FOUND)
}

/// `POST /incidents/:id/timeline` — append an entry to the incident timeline.
pub async fn add_timeline_entry(
    State(state): State<Arc<IncidentState>>,
    Path(id): Path<String>,
    Json(body): Json<AddTimelineEntryRequest>,
) -> Result<Json<Incident>, StatusCode> {
    let mut store = state.store.lock().unwrap();
    let incident = store.get_mut(&id).ok_or(StatusCode::NOT_FOUND)?;
    incident.timeline.push(timeline_entry(body.actor, body.note));
    incident.updated_at = Utc::now();
    Ok(Json(incident.clone()))
}

/// `POST /incidents/:id/acknowledge` — record that the current on-call tier
/// has acknowledged the incident, cancelling the pending escalation.
pub async fn acknowledge_incident(
    State(state): State<Arc<IncidentState>>,
    Path(id): Path<String>,
    Json(body): Json<AcknowledgeIncidentRequest>,
) -> Result<Json<Incident>, StatusCode> {
    let mut store = state.store.lock().unwrap();
    let incident = store.get_mut(&id).ok_or(StatusCode::NOT_FOUND)?;

    let now = Utc::now();
    incident.acknowledged_at = Some(now);
    incident.acknowledgment_deadline = None;
    incident.updated_at = now;
    incident.timeline.push(timeline_entry(
        body.actor,
        format!("acknowledged at escalation tier {}", incident.escalation_level),
    ));

    Ok(Json(incident.clone()))
}

/// `POST /incidents/:id/status` — transition incident status, recording the
/// change in the timeline.
pub async fn update_incident_status(
    State(state): State<Arc<IncidentState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateStatusRequest>,
) -> Result<Json<Incident>, StatusCode> {
    let mut store = state.store.lock().unwrap();
    let incident = store.get_mut(&id).ok_or(StatusCode::NOT_FOUND)?;

    let note = format!("status changed from {:?} to {:?}", incident.status, body.status);
    incident.status = body.status;
    incident.updated_at = Utc::now();
    incident.timeline.push(timeline_entry(body.actor, note));

    Ok(Json(incident.clone()))
}

/// Escalate an incident to the next on-call tier, resetting the
/// acknowledgment deadline for the new tier.
///
/// Returns `true` if the incident was escalated, `false` if it was already
/// acknowledged (and therefore needs no escalation).
fn escalate_to_next_tier(incident: &mut Incident, reason: &str) -> bool {
    if incident.acknowledged_at.is_some() {
        return false;
    }

    let now = Utc::now();
    incident.escalation_level += 1;
    incident.acknowledgment_deadline =
        Some(now + Duration::minutes(incident.severity.acknowledgment_timeout_minutes()));
    incident.updated_at = now;
    incident.timeline.push(timeline_entry(
        "system",
        format!(
            "escalated to tier {}: {}",
            incident.escalation_level, reason
        ),
    ));

    true
}

/// `POST /incidents/:id/escalate` — manually escalate to the next tier.
pub async fn escalate_incident(
    State(state): State<Arc<IncidentState>>,
    Path(id): Path<String>,
    Json(body): Json<EscalateIncidentRequest>,
) -> Result<Json<Incident>, StatusCode> {
    let mut store = state.store.lock().unwrap();
    let incident = store.get_mut(&id).ok_or(StatusCode::NOT_FOUND)?;

    escalate_to_next_tier(incident, &body.reason);
    incident.timeline.push(timeline_entry(body.actor, "requested escalation"));

    Ok(Json(incident.clone()))
}

/// Enforce acknowledgment timeouts across all open incidents.
///
/// Any incident whose current tier has not acknowledged before its
/// `acknowledgment_deadline` is escalated to the next on-call tier. Intended
/// to be driven by a periodic background task so escalation happens
/// automatically rather than relying on manual follow-up.
///
/// Returns the incidents that were escalated.
pub fn enforce_acknowledgment_timeouts(store: &IncidentStore) -> Vec<Incident> {
    let now = Utc::now();
    let mut escalated = Vec::new();
    let mut store = store.lock().unwrap();

    for incident in store.values_mut() {
        if incident.acknowledged_at.is_some() {
            continue;
        }
        if matches!(incident.status, IncidentStatus::Resolved | IncidentStatus::Closed) {
            continue;
        }
        let overdue = incident
            .acknowledgment_deadline
            .map(|deadline| deadline <= now)
            .unwrap_or(false);
        if overdue && escalate_to_next_tier(incident, "acknowledgment timeout") {
            escalated.push(incident.clone());
        }
    }

    escalated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_time_acknowledgment_prevents_escalation() {
        let store = create_incident_store();
        let incident = open_incident(&store, "db down", "primary db unreachable", IncidentSeverity::Sev1);

        // Acknowledge before the deadline elapses.
        {
            let mut guard = store.lock().unwrap();
            let stored = guard.get_mut(&incident.id).unwrap();
            stored.acknowledged_at = Some(Utc::now());
            stored.acknowledgment_deadline = None;
        }

        let escalated = enforce_acknowledgment_timeouts(&store);
        assert!(escalated.is_empty(), "acknowledged incident must not escalate");

        let guard = store.lock().unwrap();
        let stored = guard.get(&incident.id).unwrap();
        assert_eq!(stored.escalation_level, 0);
    }

    #[test]
    fn timeout_escalates_to_next_tier() {
        let store = create_incident_store();
        let incident = open_incident(&store, "api latency", "p99 above threshold", IncidentSeverity::Sev2);

        // Force the deadline into the past to simulate an unacknowledged timeout.
        {
            let mut guard = store.lock().unwrap();
            let stored = guard.get_mut(&incident.id).unwrap();
            stored.acknowledgment_deadline = Some(Utc::now() - Duration::minutes(1));
        }

        let escalated = enforce_acknowledgment_timeouts(&store);
        assert_eq!(escalated.len(), 1);
        assert_eq!(escalated[0].escalation_level, 1);

        let guard = store.lock().unwrap();
        let stored = guard.get(&incident.id).unwrap();
        assert_eq!(stored.escalation_level, 1);
        assert!(stored.acknowledgment_deadline.unwrap() > Utc::now());
    }
}
