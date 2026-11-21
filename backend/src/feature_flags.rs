//! Feature flag storage and evaluation (trunk-based development support).
//!
//! Features are built directly on `main` behind flags instead of long-lived
//! branches. This module provides:
//!
//! - Flag storage (SQL-backed via [`crate::db::Db`], shared across all
//!   instances, durable across restarts)
//! - Flag evaluation (global on/off + percentage-based gradual rollout)
//! - `POST /admin/flags` to create/update a flag
//! - `GET /admin/flags` to list all flags
//! - `GET /admin/flags/:key` to fetch a single flag
//! - `POST /admin/flags/:key/evaluate` to evaluate a flag for a given subject
//!
//! # Gradual rollout
//!
//! Each flag has a `rollout_percentage` (0-100). Evaluation hashes the
//! `(flag_key, subject_id)` pair into a stable bucket in `[0, 100)` so the
//! same subject always gets the same result for a given rollout percentage,
//! and increasing the percentage only ever adds subjects (never removes
//! ones that were previously enabled).
//!
//! # Versioning
//!
//! Every update to a flag increments its `version` and writes the previous
//! state to the `feature_flag_history` SQL table, so changes can be audited
//! or rolled back across process restarts and load-balanced instances.
//!
//! # Fallback on error
//!
//! Flag evaluation never panics. If the flag store cannot be read, a flag
//! evaluates to [`FALLBACK_ENABLED`] (disabled) with the reason
//! [`FALLBACK_REASON`], so a storage failure turns a feature off instead of
//! failing the request that checked it.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::db::Db;

/// A single historical snapshot of a flag, recorded before each update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlagVersionSnapshot {
    pub version: u32,
    pub enabled: bool,
    pub rollout_percentage: u8,
    pub updated_at: DateTime<Utc>,
    pub updated_by: Option<String>,
}

/// A feature flag definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureFlag {
    pub key: String,
    pub description: Option<String>,
    pub enabled: bool,
    /// Percentage (0-100) of subjects that should see the flag as enabled
    /// when `enabled` is true. 100 means fully rolled out.
    pub rollout_percentage: u8,
    pub version: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Ordered list of prior states (oldest first), loaded from
    /// `feature_flag_history` on every read.
    pub history: Vec<FlagVersionSnapshot>,
}

/// Request body for `POST /admin/flags`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpsertFlagRequest {
    pub key: String,
    pub description: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_rollout")]
    pub rollout_percentage: u8,
    pub updated_by: Option<String>,
}

fn default_rollout() -> u8 {
    100
}

/// Request body for `POST /admin/flags/:key/evaluate`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluateFlagRequest {
    pub subject_id: String,
}

/// Result of evaluating a flag for a subject.
#[derive(Debug, Serialize)]
pub struct FlagEvaluation {
    pub key: String,
    pub subject_id: String,
    pub enabled: bool,
    pub reason: String,
    pub flag_version: u32,
}

/// Shared handle to the SQL-backed flag store.
///
/// All HTTP handlers receive an `Arc<FlagState>` extracted from [`AppState`]
/// via the [`axum::extract::FromRef`] impl in `db.rs`.  Because every
/// operation goes through `self.db` (a shared [`Arc<Db>`]), any update made
/// on one instance is immediately visible to all other instances that share
/// the same database file.
#[derive(Clone)]
pub struct FlagState {
    pub db: Arc<Db>,
}

impl FlagState {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }

    /// Return whether `key` is enabled for `subject_id`.
    ///
    /// Unknown flags and store errors both resolve to [`FALLBACK_ENABLED`],
    /// so callers can gate code on a flag without handling errors themselves.
    pub fn is_enabled(&self, key: &str, subject_id: &str) -> bool {
        match self.db.get_feature_flag(key) {
            Ok(Some(flag)) => evaluate_flag(&flag, subject_id).enabled,
            Ok(None) => FALLBACK_ENABLED,
            Err(err) => {
                tracing::warn!(
                    flag = %key,
                    error = %err,
                    "feature flag lookup failed; using fallback"
                );
                FALLBACK_ENABLED
            }
        }
    }
}

/// Value a flag evaluates to when the flag store cannot be read.
pub const FALLBACK_ENABLED: bool = false;

/// `reason` reported by an evaluation that used [`FALLBACK_ENABLED`].
pub const FALLBACK_REASON: &str = "flag store unavailable; using fallback";

/// Build the evaluation returned when the flag store cannot be read.
pub fn fallback_evaluation(key: &str, subject_id: &str) -> FlagEvaluation {
    FlagEvaluation {
        key: key.to_string(),
        subject_id: subject_id.to_string(),
        enabled: FALLBACK_ENABLED,
        reason: FALLBACK_REASON.to_string(),
        flag_version: 0,
    }
}

// ── Evaluation logic (unchanged from original) ────────────────────────────────

/// Deterministically hash `(key, subject_id)` into a bucket in `[0, 100)`.
///
/// Uses a simple FNV-1a style hash so evaluation has no external
/// dependencies and is stable across process restarts.
fn bucket_for(key: &str, subject_id: &str) -> u8 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in key
        .as_bytes()
        .iter()
        .chain(b":")
        .chain(subject_id.as_bytes())
    {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    (hash % 100) as u8
}

/// Evaluate whether `flag` is enabled for `subject_id`.
///
/// The hashing/bucketing logic is identical to the original in-memory
/// implementation — only the storage backing has changed.
pub fn evaluate_flag(flag: &FeatureFlag, subject_id: &str) -> FlagEvaluation {
    let enabled = if !flag.enabled {
        false
    } else if flag.rollout_percentage >= 100 {
        true
    } else if flag.rollout_percentage == 0 {
        false
    } else {
        bucket_for(&flag.key, subject_id) < flag.rollout_percentage
    };

    let reason = if !flag.enabled {
        "flag disabled".to_string()
    } else if flag.rollout_percentage >= 100 {
        "fully rolled out".to_string()
    } else {
        format!("gradual rollout at {}%", flag.rollout_percentage)
    };

    FlagEvaluation {
        key: flag.key.clone(),
        subject_id: subject_id.to_string(),
        enabled,
        reason,
        flag_version: flag.version,
    }
}

// ── HTTP handlers ─────────────────────────────────────────────────────────────

/// `POST /admin/flags` — create or update a feature flag.
pub async fn upsert_flag(
    State(state): State<Arc<FlagState>>,
    Json(body): Json<UpsertFlagRequest>,
) -> Result<(StatusCode, Json<FeatureFlag>), (StatusCode, Json<serde_json::Value>)> {
    if body.key.trim().is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "key must not be empty" })),
        ));
    }
    if body.rollout_percentage > 100 {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "rollout_percentage must be 0-100" })),
        ));
    }

    state
        .db
        .upsert_feature_flag(&body)
        .map(|flag| (StatusCode::OK, Json(flag)))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
        })
}

/// `GET /admin/flags` — list all flags.
pub async fn list_flags(
    State(state): State<Arc<FlagState>>,
) -> Result<Json<Vec<FeatureFlag>>, (StatusCode, Json<serde_json::Value>)> {
    state.db.list_feature_flags().map(Json).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
    })
}

/// `GET /admin/flags/:key` — fetch a single flag.
pub async fn get_flag(
    State(state): State<Arc<FlagState>>,
    Path(key): Path<String>,
) -> Result<Json<FeatureFlag>, StatusCode> {
    match state.db.get_feature_flag(&key) {
        Ok(Some(flag)) => Ok(Json(flag)),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// `POST /admin/flags/:key/evaluate` — evaluate a flag for a subject.
///
/// If the flag store cannot be read the response is the documented fallback
/// (see [`fallback_evaluation`]) rather than an error.
pub async fn evaluate_flag_handler(
    State(state): State<Arc<FlagState>>,
    Path(key): Path<String>,
    Json(body): Json<EvaluateFlagRequest>,
) -> Result<Json<FlagEvaluation>, StatusCode> {
    match state.db.get_feature_flag(&key) {
        Ok(Some(flag)) => Ok(Json(evaluate_flag(&flag, &body.subject_id))),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(err) => {
            tracing::warn!(flag = %key, error = %err, "feature flag lookup failed; using fallback");
            Ok(Json(fallback_evaluation(&key, &body.subject_id)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn sample_flag(rollout: u8) -> FeatureFlag {
        FeatureFlag {
            key: "new-checkout".to_string(),
            description: None,
            enabled: true,
            rollout_percentage: rollout,
            version: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            history: Vec::new(),
        }
    }

    #[test]
    fn disabled_flag_never_enabled() {
        let mut flag = sample_flag(100);
        flag.enabled = false;
        let eval = evaluate_flag(&flag, "user-1");
        assert!(!eval.enabled);
    }

    #[test]
    fn full_rollout_always_enabled() {
        let flag = sample_flag(100);
        for i in 0..50 {
            let eval = evaluate_flag(&flag, &format!("user-{i}"));
            assert!(eval.enabled);
        }
    }

    #[test]
    fn zero_rollout_never_enabled() {
        let flag = sample_flag(0);
        let eval = evaluate_flag(&flag, "user-1");
        assert!(!eval.enabled);
    }

    #[test]
    fn fallback_evaluation_is_disabled() {
        let eval = fallback_evaluation("new-checkout", "user-1");
        assert_eq!(eval.enabled, FALLBACK_ENABLED);
        assert!(!eval.enabled);
        assert_eq!(eval.reason, FALLBACK_REASON);
    }

    #[test]
    fn evaluation_is_deterministic() {
        let flag = sample_flag(50);
        let first = evaluate_flag(&flag, "user-42");
        let second = evaluate_flag(&flag, "user-42");
        assert_eq!(first.enabled, second.enabled);
    }

    // ── SQL-backed storage (#274) ──────────────────────────────────────────────

    use crate::db::Db as TestDb;
    use std::sync::Arc as TestArc;

    fn temp_db_path(tag: &str) -> String {
        std::env::temp_dir()
            .join(format!(
                "ethos_flags_{tag}_{}.sqlite3",
                uuid::Uuid::new_v4()
            ))
            .to_string_lossy()
            .into_owned()
    }

    fn upsert_req(key: &str, enabled: bool, rollout: u8) -> UpsertFlagRequest {
        UpsertFlagRequest {
            key: key.to_string(),
            description: Some(format!("flag {key}")),
            enabled,
            rollout_percentage: rollout,
            updated_by: Some("tester".to_string()),
        }
    }

    /// Regression: create/update/list/fetch behave the same as the original
    /// in-memory store from a caller's perspective.
    #[test]
    fn sql_store_matches_previous_single_instance_behavior() -> TestResult {
        let path = temp_db_path("regression");
        let db = TestDb::open(&path)?;
        db.migrate()?;

        // Unknown key fetches as None (was: not found).
        assert!(db.get_feature_flag("nope")?.is_none());
        assert!(db.list_feature_flags()?.is_empty());

        // Create → version 1, empty history.
        let created = db
            .upsert_feature_flag(&upsert_req("new-checkout", true, 25))?;
        assert_eq!(created.version, 1);
        assert!(created.history.is_empty());
        assert_eq!(created.rollout_percentage, 25);
        assert!(created.enabled);

        // Update → version 2, previous state snapshotted into history.
        let updated = db
            .upsert_feature_flag(&upsert_req("new-checkout", true, 50))?;
        assert_eq!(updated.version, 2);
        assert_eq!(updated.created_at, created.created_at);
        assert!(updated.updated_at >= created.updated_at);
        assert_eq!(updated.history.len(), 1);
        let snapshot = &updated.history[0];
        assert_eq!(snapshot.version, 1);
        assert_eq!(snapshot.rollout_percentage, 25);
        assert!(snapshot.enabled);
        assert_eq!(snapshot.updated_by.as_deref(), Some("tester"));

        // Description omitted on update keeps the previous one.
        let mut req = upsert_req("new-checkout", false, 0);
        req.description = None;
        let kept = db.upsert_feature_flag(&req)?;
        assert_eq!(kept.version, 3);
        assert_eq!(kept.description.as_deref(), Some("flag new-checkout"));

        // List returns every flag; get-by-key matches list contents.
        db.upsert_feature_flag(&upsert_req("beta-ui", true, 100))?;
        let flags = db.list_feature_flags()?;
        let keys: Vec<&str> = flags.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(keys, vec!["beta-ui", "new-checkout"]);
        for flag in &flags {
            let fetched = db.get_feature_flag(&flag.key)?.ok_or("flag missing")?;
            assert_eq!(fetched.version, flag.version);
            // History is loaded on every read, oldest first.
            assert_eq!(fetched.history.len(), flag.history.len());
        }

        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    /// Two independent `Db`/`FlagState` handles against the same backing
    /// store must both observe an update made through either handle.
    #[test]
    fn update_visible_across_independent_handles() -> TestResult {
        let path = temp_db_path("multi");

        // Simulate two instances behind a load balancer sharing one database.
        let state_a = FlagState::new(TestArc::new(TestDb::open(&path)?));
        state_a.db.migrate()?;
        let state_b = FlagState::new(TestArc::new(TestDb::open(&path)?));
        state_b.db.migrate()?;

        // Instance A creates the flag…
        state_a
            .db
            .upsert_feature_flag(&upsert_req("gradual-rollout", true, 10))?;

        // …and instance B immediately sees it, including via evaluation.
        let seen_by_b = state_b.db.get_feature_flag("gradual-rollout")?;
        assert!(seen_by_b.is_some());
        let eval_b = evaluate_flag(&seen_by_b.ok_or("flag missing")?, "user-7");
        assert_eq!(eval_b.flag_version, 1);

        // Instance B updates it…
        let updated = state_b
            .db
            .upsert_feature_flag(&upsert_req("gradual-rollout", true, 90))?;
        assert_eq!(updated.version, 2);

        // …and instance A observes B's new version and history.
        let seen_by_a = state_a
            .db
            .get_feature_flag("gradual-rollout")?
            .ok_or("flag missing")?;
        assert_eq!(seen_by_a.version, 2);
        assert_eq!(seen_by_a.rollout_percentage, 90);
        assert_eq!(seen_by_a.history.len(), 1);
        assert_eq!(state_a.db.list_feature_flags()?.len(), 1);

        // A subject's evaluation on either instance agrees at the same version.
        let eval_a = evaluate_flag(&seen_by_a, "user-7");
        assert_eq!(eval_a.flag_version, eval_b.flag_version + 1);

        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    /// Flag state and version history must survive a process restart
    /// (drop every handle, then re-open the backing store).
    #[test]
    fn flag_state_and_history_survive_restart() -> TestResult {
        let path = temp_db_path("restart");
        {
            let db = TestDb::open(&path)?;
            db.migrate()?;
            db.upsert_feature_flag(&upsert_req("durable-flag", true, 5))?;
            db.upsert_feature_flag(&upsert_req("durable-flag", true, 40))?;
            db.upsert_feature_flag(&upsert_req("durable-flag", true, 80))?;
        }

        // Re-open against the same file, simulating a restart.
        let db = TestDb::open(&path)?;
        db.migrate()?;
        let flag = db
            .get_feature_flag("durable-flag")
            .expect("read after restart")
            .expect("flag should survive restart");
        assert_eq!(flag.version, 3);
        assert_eq!(flag.rollout_percentage, 80);
        assert_eq!(flag.history.len(), 2);
        let versions: Vec<u32> = flag.history.iter().map(|h| h.version).collect();
        assert_eq!(versions, vec![1, 2]);
        let rollouts: Vec<u8> = flag.history.iter().map(|h| h.rollout_percentage).collect();
        assert_eq!(rollouts, vec![5, 40]);
        // Evaluation still works identically after the restart.
        assert_eq!(evaluate_flag(&flag, "user-1").flag_version, 3);

        let _ = std::fs::remove_file(&path);
        Ok(())
    }
}
