// axum's Handler trait is only implemented for async fns (or functions returning
// a Future); every handler in this file is registered on a Router (see
// main.rs::build_router), except `unsubscribe`, which is documented in
// docs/backend-api.md as a live endpoint but is not currently wired up there.
// None of these bodies await anything today, but the signature is fixed by axum.
#![allow(clippy::unused_async)]

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Html,
    Json,
};
use serde::Deserialize;

use crate::{
    db::{AppState, Db},
    error::AppError,
    handlers::{parse_scenario_types, simulate_release_handler},
    models::{
        ReminderPreferences, SetPreferencesRequest, SetSubscriptionRequest, SimulateReleaseQuery,
        SimulateReleaseResponse, Subscription,
    },
};

pub const PLAYGROUND_HTML: &str = include_str!("../simulator.html");

/// Returns `true` when the development-only `/playground` route should be mounted.
/// Controlled by the `ENABLE_PLAYGROUND` environment variable (`true` or `1`).
pub fn is_playground_enabled() -> bool {
    std::env::var("ENABLE_PLAYGROUND")
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false)
}

/// `GET /playground` — serve the self-contained Release Simulator / Playground UI.
/// Only mounted when `ENABLE_PLAYGROUND=true` (development-only).
pub async fn playground_handler() -> Html<&'static str> {
    Html(PLAYGROUND_HTML)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemindersQuery {
    pub include_deleted: Option<bool>,
}

pub async fn list_vault_reminders(
    State(state): State<Arc<AppState>>,
    Path(vault_id): Path<u64>,
    Query(query): Query<RemindersQuery>,
) -> Result<Json<Vec<ReminderPreferences>>, AppError> {
    let db = &state.db;
    let records = if query.include_deleted.unwrap_or(false) {
        db.all_reminders_including_deleted(vault_id)?
    } else {
        match db.get(vault_id) {
            Ok(p) => vec![p],
            Err(_) => vec![],
        }
    };
    Ok(Json(records))
}

pub async fn delete_preferences(
    State(state): State<Arc<AppState>>,
    Path(vault_id): Path<u64>,
) -> Result<StatusCode, AppError> {
    state.db.soft_delete_reminder(vault_id)?;
    state.query_cache.invalidate_preferences(&vault_id.to_string());
    state.query_cache.invalidate_vault(&vault_id.to_string());
    Ok(StatusCode::NO_CONTENT)
}

pub async fn set_preferences(
    State(state): State<Arc<AppState>>,
    Path(vault_id): Path<u64>,
    headers: HeaderMap,
    Json(body): Json<SetPreferencesRequest>,
) -> Result<(StatusCode, Json<ReminderPreferences>), AppError> {
    let db = &state.db;
    if body.channels.is_empty() {
        return Err(AppError::InvalidInput("channels must not be empty".into()));
    }
    if body.hours_before_expiry == 0 {
        return Err(AppError::InvalidInput(
            "hours_before_expiry must be > 0".into(),
        ));
    }

    // #825: Idempotency key support
    let idem_key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok());

    if let Some(idem_key) = idem_key {
        if let Some(cached) = db.check_idempotency(idem_key) {
            // #858: A corrupt cached body must not panic the request task.
            // Log the error, evict the bad record so the next request
            // recomputes it, and surface a 500 to the caller.
            match serde_json::from_str::<ReminderPreferences>(&cached.response_body) {
                Ok(cached_prefs) => return Ok((StatusCode::OK, Json(cached_prefs))),
                Err(e) => {
                    eprintln!(
                        "idempotency cache deserialization failed for key {idem_key}: {e}"
                    );
                    if let Err(del_err) = db.delete_idempotency(idem_key) {
                        eprintln!(
                            "failed to evict corrupt idempotency record for key {idem_key}: {del_err}"
                        );
                    }
                    return Err(AppError::DatabaseError);
                }
            }
        }
    }

    let prefs = ReminderPreferences {
        vault_id,
        channels: body.channels,
        hours_before_expiry: body.hours_before_expiry,
        frequency: body.frequency,
        deleted_at: None,
    };

    // #859: Store the idempotency key in the same transaction as the mutation
    // so concurrent requests with the same key cannot both run the mutation.
    // A unique constraint on the key makes the duplicate insert fail, and the
    // error is propagated instead of being discarded.
    match idem_key {
        Some(idem_key) => {
            let body_json = serde_json::to_string(&prefs).map_err(|_| AppError::DatabaseError)?;
            db.upsert_with_idempotency(&prefs, idem_key, 200, &body_json)?;
        }
        None => db.upsert(&prefs)?,
    }

    state.query_cache.invalidate_preferences(&vault_id.to_string());
    state.query_cache.invalidate_vault(&vault_id.to_string());

    Ok((StatusCode::OK, Json(prefs)))
}

pub async fn get_preferences(
    State(state): State<Arc<AppState>>,
    Path(vault_id): Path<u64>,
) -> Result<Json<ReminderPreferences>, AppError> {
    let db = &state.db;
    match db.get(vault_id) {
        Ok(prefs) => Ok(Json(prefs)),
        Err(_e) => Err(AppError::NotFound),
    }
}

// ── Unsubscribe endpoint (#828) ─────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnsubscribeQuery {
    pub token: String,
}

pub async fn unsubscribe(
    State(state): State<Arc<AppState>>,
    Query(query): Query<UnsubscribeQuery>,
) -> Result<(StatusCode, String), AppError> {
    let db = &state.db;
    match db.process_unsubscribe(&query.token) {
        Ok(owner) => Ok((
            StatusCode::OK,
            format!("You ({owner}) have been unsubscribed from reminder emails."),
        )),
        Err(_) => Err(AppError::InvalidInput(
            "Invalid or expired unsubscribe token".into(),
        )),
    }
}

// ── Release Simulator endpoint ────────────────────────────────────────────────

/// GET /api/vaults/:vault_id/simulate-release?scenarios=no_check_ins,consistent_check_ins,missed_check_in_dates&missed_count=2
pub async fn simulate_release(
    State(db): State<Arc<Db>>,
    Path(vault_id): Path<String>,
    Query(query): Query<SimulateReleaseQuery>,
) -> Result<Json<SimulateReleaseResponse>, AppError> {
    let scenarios = parse_scenario_types(query.scenarios.as_deref());
    if scenarios.is_empty() {
        return Err(AppError::InvalidInput(
            "No valid scenarios requested. Use: no_check_ins, consistent_check_ins, missed_check_in_dates".into(),
        ));
    }

    let missed_count = query.missed_count.unwrap_or(1);

    let result = simulate_release_handler(&db.vault_store, &vault_id, scenarios, missed_count)
        .map_err(|_| AppError::NotFound)?;

    Ok(Json(result))
}

// ── Vault Notification Subscription Handlers ────────────────────────────────

pub async fn set_subscription(
    State(state): State<Arc<AppState>>,
    Path(vault_id): Path<u64>,
    Json(body): Json<SetSubscriptionRequest>,
) -> Result<(StatusCode, Json<Subscription>), AppError> {
    if body.channels.is_empty() {
        return Err(AppError::InvalidInput("channels must not be empty".into()));
    }

    let sub = Subscription {
        vault_id,
        owner: body.owner,
        channels: body.channels,
        frequency: body.frequency,
    };
    state.db.upsert_subscription(&sub)?;
    state.query_cache.invalidate_subscription(&vault_id.to_string());
    state.query_cache.invalidate_vault(&vault_id.to_string());

    Ok((StatusCode::OK, Json(sub)))
}

pub async fn delete_subscription(
    State(state): State<Arc<AppState>>,
    Path(vault_id): Path<u64>,
) -> Result<StatusCode, AppError> {
    state.db.delete_subscription(vault_id)?;
    state.query_cache.invalidate_subscription(&vault_id.to_string());
    state.query_cache.invalidate_vault(&vault_id.to_string());
    Ok(StatusCode::NO_CONTENT)
}

// ── #68: Idempotency Key Cleanup Endpoint ───────────────────────────────────

pub async fn cleanup_idempotency_keys(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let count = state
        .db
        .cleanup_expired_idempotency_keys()
        .map_err(|_| AppError::DatabaseError)?;
    Ok(Json(serde_json::json!({
        "cleaned_up": count,
        "message": "Idempotency keys older than 24 hours have been removed"
    })))
}

// ── #69: Multi-Tenancy Endpoints ────────────────────────────────────────────

pub async fn create_tenant(
    State(state): State<Arc<AppState>>,
    Json(body): Json<crate::models::CreateTenantRequest>,
) -> Result<(StatusCode, Json<crate::models::Tenant>), AppError> {
    let tenant = crate::models::Tenant {
        id: uuid::Uuid::new_v4(

/* … truncated 796 chars — edit only what you need near the top … */
