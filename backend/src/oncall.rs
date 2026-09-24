//! Automated on-call schedule management.
//!
//! On-call schedules were previously managed by hand in a spreadsheet, which
//! led to gaps in coverage and missed handoffs. This module implements
//! rotation scheduling, handoff notifications, and tiered escalation policies
//! so the on-call roster can be generated and maintained automatically.
//!
//! # Architecture
//!
//! ```text
//! POST /admin/on-call-schedule        → create_on_call_schedule (builds a rotation)
//! GET  /admin/on-call-schedule        → list_on_call_schedules
//! GET  /admin/on-call-schedule/:id    → get_on_call_schedule
//! POST /admin/on-call-schedule/:id/escalate → trigger_escalation
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

/// A single engineer eligible to be placed on the rotation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnCallParticipant {
    pub id: String,
    pub name: String,
    pub contact: String,
}

/// One scheduled on-call shift produced by the rotation algorithm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnCallShift {
    pub id: String,
    pub participant: OnCallParticipant,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

/// A tiered escalation level: if the primary on-call does not acknowledge in
/// time, the next level's contacts are notified.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationLevel {
    pub level: u32,
    pub delay_minutes: i64,
    pub contacts: Vec<String>,
}

/// The full escalation policy attached to a schedule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationPolicy {
    pub levels: Vec<EscalationLevel>,
}

impl Default for EscalationPolicy {
    fn default() -> Self {
        Self {
            levels: vec![
                EscalationLevel {
                    level: 1,
                    delay_minutes: 5,
                    contacts: vec![],
                },
                EscalationLevel {
                    level: 2,
                    delay_minutes: 15,
                    contacts: vec![],
                },
                EscalationLevel {
                    level: 3,
                    delay_minutes: 30,
                    contacts: vec![],
                },
            ],
        }
    }
}

/// A record of a handoff notification sent between rotations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffNotification {
    pub id: String,
    pub schedule_id: String,
    pub outgoing: OnCallParticipant,
    pub incoming: OnCallParticipant,
    pub sent_at: DateTime<Utc>,
}

/// A generated on-call schedule: a rotation of shifts plus its escalation
/// policy and the handoff notifications produced when shifts change hands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnCallSchedule {
    pub id: String,
    pub name: String,
    pub rotation_hours: i64,
    pub shifts: Vec<OnCallShift>,
    pub escalation_policy: EscalationPolicy,
    pub handoffs: Vec<HandoffNotification>,
    pub created_at: DateTime<Utc>,
}

/// Request body for `POST /admin/on-call-schedule`.
#[derive(Debug, Deserialize)]
pub struct CreateOnCallScheduleRequest {
    pub name: String,
    pub participants: Vec<OnCallParticipant>,
    /// Length of each rotation shift, in hours.
    pub rotation_hours: i64,
    /// How many shifts to generate up front.
    pub shift_count: u32,
    pub escalation_policy: Option<EscalationPolicy>,
}

/// Request body for triggering an escalation manually (e.g. from an alert
/// that has gone unacknowledged).
#[derive(Debug, Deserialize)]
pub struct TriggerEscalationRequest {
    pub reason: String,
    pub current_level: u32,
}

/// Response describing which escalation level fired and who was notified.
#[derive(Debug, Serialize)]
pub struct EscalationResult {
    pub schedule_id: String,
    pub level_notified: u32,
    pub contacts_notified: Vec<String>,
    pub reason: String,
}

pub type OnCallStore = Arc<Mutex<HashMap<String, OnCallSchedule>>>;

pub fn create_on_call_store() -> OnCallStore {
    Arc::new(Mutex::new(HashMap::new()))
}

#[derive(Clone)]
pub struct OnCallState {
    pub store: OnCallStore,
}

impl OnCallState {
    pub fn new() -> Self {
        Self {
            store: create_on_call_store(),
        }
    }
}

impl Default for OnCallState {
    fn default() -> Self {
        Self::new()
    }
}

/// An ad-hoc alert raised against a schedule's primary escalation contacts,
/// outside the normal `trigger_escalation` HTTP flow — used by other
/// subsystems (e.g. connection-pool leak detection) that need to page
/// on-call without going through a pre-defined escalation level.
#[derive(Debug, Clone)]
pub struct AlertRecord {
    pub schedule_id: String,
    pub source: String,
    pub message: String,
    pub contacts_notified: Vec<String>,
}

/// Raise an alert against `schedule_id`'s primary (level-1) escalation
/// contacts and log it. Returns `None` if the schedule doesn't exist, in
/// which case the alert is only logged, not attributed to any contacts.
pub fn raise_alert(
    state: &OnCallState,
    schedule_id: &str,
    source: &str,
    message: &str,
) -> Option<AlertRecord> {
    let contacts = {
        let store = state.store.lock().unwrap();
        store
            .get(schedule_id)
            .and_then(|schedule| schedule.escalation_policy.levels.first())
            .map(|level| level.contacts.clone())
    };

    tracing::error!(
        schedule_id = %schedule_id,
        source = %source,
        message = %message,
        contacts = ?contacts,
        "alert raised"
    );

    contacts.map(|contacts_notified| AlertRecord {
        schedule_id: schedule_id.to_string(),
        source: source.to_string(),
        message: message.to_string(),
        contacts_notified,
    })
}

/// Build a round-robin rotation of `shift_count` shifts across
/// `participants`, each `rotation_hours` long, starting now.
fn build_rotation(
    participants: &[OnCallParticipant],
    rotation_hours: i64,
    shift_count: u32,
) -> Vec<OnCallShift> {
    if participants.is_empty() || shift_count == 0 {
        return vec![];
    }

    let mut shifts = Vec::with_capacity(shift_count as usize);
    let mut cursor = Utc::now();

    for i in 0..shift_count {
        let participant = participants[(i as usize) % participants.len()].clone();
        let starts_at = cursor;
        let ends_at = cursor + Duration::hours(rotation_hours);
        shifts.push(OnCallShift {
            id: Uuid::new_v4().to_string(),
            participant,
            starts_at,
            ends_at,
        });
        cursor = ends_at;
    }

    shifts
}

/// Produce handoff notifications for each consecutive pair of shifts.
fn build_handoffs(schedule_id: &str, shifts: &[OnCallShift]) -> Vec<HandoffNotification> {
    let mut handoffs = Vec::new();
    for window in shifts.windows(2) {
        let outgoing = &window[0];
        let incoming = &window[1];
        handoffs.push(HandoffNotification {
            id: Uuid::new_v4().to_string(),
            schedule_id: schedule_id.to_string(),
            outgoing: outgoing.participant.clone(),
            incoming: incoming.participant.clone(),
            sent_at: outgoing.ends_at,
        });
    }
    handoffs
}

/// The on-call tier that should be paged for a schedule at `now`, given the
/// time the current alert was raised and whether it has been acknowledged.
///
/// This is the rotation-aware escalation lookup: it walks the schedule's
/// escalation policy in order and returns the first level whose
/// `delay_minutes` window has elapsed since `raised_at`. If the alert was
/// acknowledged, no escalation is needed and `None` is returned. If every
/// level's window has elapsed, the last (highest) tier is returned so the
/// page never silently stops.
pub fn next_escalation_level(
    schedule: &OnCallSchedule,
    raised_at: DateTime<Utc>,
    acknowledged: bool,
    now: DateTime<Utc>,
) -> Option<&EscalationLevel> {
    if acknowledged {
        return None;
    }

    let elapsed = now.signed_duration_since(raised_at);
    let mut levels = schedule.escalation_policy.levels.iter().peekable();
    let mut last = None;

    while let Some(level) = levels.next() {
        if elapsed >= Duration::minutes(level.delay_minutes) {
            last = Some(level);
            if levels.peek().is_none() {
                return last;
            }
        } else {
            return last;
        }
    }

    last
}

/// `POST /admin/on-call-schedule` — generate a new rotation schedule.
pub async fn create_on_call_schedule(
    State(state): State<Arc<OnCallState>>,
    Json(body): Json<CreateOnCallScheduleRequest>,
) -> Result<(StatusCode, Json<OnCallSchedule>), (StatusCode, Json<serde_json::Value>)> {
    if body.participants.is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "at least one participant is required" })),
        ));
    }
    if body.rotation_hours <= 0 {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "rotation_hours must be positive" })),
        ));
    }

    let id = Uuid::new_v4().to_string();
    let shifts = build_rotation(&body.participants, body.rotation_hours, body.shift_count);
    let handoffs = build_handoffs(&id, &shifts);
    let schedule = OnCallSchedule {
        id: id.clone(),
        name: body.name,
        rotation_hours: body.rotation_hours,
        shifts,
        escalation_policy: body.escalation_policy.unwrap_or_default(),
        handoffs,
        created_at: Utc::now(),
    };

    state.store.lock().unwrap().insert(id, schedule.clone());
    Ok((StatusCode::CREATED, Json(schedule)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule_with_policy() -> OnCallSchedule {
        OnCallSchedule {
            id: "sched-1".to_string(),
            name: "primary".to_string(),
            rotation_hours: 8,
            shifts: vec![],
            escalation_policy: EscalationPolicy {
                levels: vec![
                    EscalationLevel {
                        level: 1,
                        delay_minutes: 5,
                        contacts: vec!["primary@example.com".to_string()],
                    },
                    EscalationLevel {
                        level: 2,
                        delay_minutes: 15,
                        contacts: vec!["secondary@example.com".to_string()],
                    },
                    EscalationLevel {
                        level: 3,
                        delay_minutes: 30,
                        contacts: vec!["manager@example.com".to_string()],
                    },
                ],
            },
            handoffs: vec![],
            created_at: Utc::now(),
        }
    }

    #[test]
    fn acknowledged_alert_does_not_escalate() {
        let schedule = schedule_with_policy();
        let raised_at = Utc::now();
        let now = raised_at + Duration::minutes(60);

        let level = next_escalation_level(&schedule, raised_at, true, now);
        assert!(level.is_none(), "acknowledged alerts must not escalate");
    }

    #[test]
    fn on_time_acknowledgment_stays_at_primary() {
        let schedule = schedule_with_policy();
        let raised_at = Utc::now();
        // Within the level-1 window, before any escalation delay elapses.
        let now = raised_at + Duration::minutes(2);

        let level = next_escalation_level(&schedule, raised_at, false, now);
        assert!(level.is_none(), "no tier should fire before its delay elapses");
    }

    #[test]
    fn timeout_escalates_to_next_tier() {
        let schedule = schedule_with_policy();
        let raised_at = Utc::now();
        // Past level-1 (5m) but before level-2 (15m).
        let now = raised_at + Duration::minutes(6);

        let level = next_escalation_level(&schedule, raised_at, false, now)
            .expect("level 1 should have fired");
        assert_eq!(level.level, 1);
        assert_eq!(level.contacts, vec!["primary@example.com".to_string()]);

        // Past level-2 (15m) but before level-3 (30m).
        let now = raised_at + Duration::minutes(16);
        let level = next_escalation_level(&schedule, raised_at, false, now)
            .expect("level 2 should have fired");
        assert_eq!(level.level, 2);
        assert_eq!(level.contacts, vec!["secondary@example.com".to_string()]);
    }

    #[test]
    fn timeout_past_all_tiers_escalates_to_highest() {
        let schedule = schedule_with_policy();
        let raised_at = Utc::now();
        let now = raised_at + Duration::minutes(120);

        let level = next_escalation_level(&schedule, raised_at, false, now)
            .expect("highest tier should fire");
        assert_eq!(level.level, 3);
        assert_eq!(level.contacts, vec!["manager@example.com".to_string()]);
    }
}
