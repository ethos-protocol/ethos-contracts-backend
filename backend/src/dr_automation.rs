//! Disaster-recovery runbook automation hooks.
//!
//! `docs/disaster-recovery-runbook.md` documents manual `stellar contract
//! invoke` steps for pausing the contract, restoring an archived vault, and
//! so on. Running these by hand during an actual incident is error-prone —
//! a mistyped vault id or a skipped verification step compounds an already
//! bad situation. This module exposes the highest-risk manual steps as
//! admin-only HTTP endpoints, gated by a two-phase confirmation token for
//! anything destructive, so the exact commands from the runbook can be
//! triggered programmatically instead of retyped under pressure.
//!
//! # Architecture
//!
//! ```text
//! POST /admin/dr/actions               → request_dr_action  (phase 1: non-destructive steps run immediately; destructive steps return a confirmation token)
//! POST /admin/dr/actions/:token/confirm → confirm_dr_action  (phase 2: executes a previously requested destructive step)
//! ```

use std::collections::HashMap;
use std::process::Command;
use std::sync::{Arc, Mutex};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// How long a requested destructive action's confirmation token remains
/// valid before it must be re-requested.
const CONFIRMATION_TTL_MINUTES: i64 = 5;

/// One of the manual runbook steps exposed for automation. Names and
/// underlying `stellar contract invoke` commands mirror
/// `docs/disaster-recovery-runbook.md`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DrStep {
    /// Runbook §1 — Emergency Contract Pause.
    PauseContract,
    /// Runbook §1 — resume normal operation after a pause.
    UnpauseContract,
    /// Runbook §3 — Archived Vault Recovery (`restore_vault`).
    RestoreVault,
}

impl DrStep {
    /// Every step here mutates on-chain state, so all of them require a
    /// confirmation token before executing.
    fn is_destructive(self) -> bool {
        true
    }
}

/// `POST /admin/dr/actions` request body.
#[derive(Debug, Clone, Deserialize)]
pub struct RequestDrActionRequest {
    pub step: DrStep,
    /// Required for `RestoreVault`; ignored otherwise.
    pub vault_id: Option<u64>,
    /// Operator requesting the action, recorded for the audit log.
    pub requested_by: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RequestDrActionResponse {
    pub step: DrStep,
    pub confirmation_token: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DrActionResult {
    pub step: DrStep,
    pub success: bool,
    pub output: String,
    pub executed_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct PendingAction {
    step: DrStep,
    vault_id: Option<u64>,
    requested_by: String,
    expires_at: DateTime<Utc>,
}

type PendingActionStore = Arc<Mutex<HashMap<String, PendingAction>>>;

#[derive(Clone)]
pub struct DrAutomationState {
    pending: PendingActionStore,
}

impl DrAutomationState {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Default for DrAutomationState {
    fn default() -> Self {
        Self::new()
    }
}

/// `POST /admin/dr/actions` — phase 1: request a destructive DR step. Always
/// returns a confirmation token that must be replayed to
/// `POST /admin/dr/actions/:token/confirm` within `CONFIRMATION_TTL_MINUTES`
/// for the step to actually execute.
pub async fn request_dr_action(
    State(state): State<Arc<DrAutomationState>>,
    Json(body): Json<RequestDrActionRequest>,
) -> Result<Json<RequestDrActionResponse>, (StatusCode, String)> {
    if matches!(body.step, DrStep::RestoreVault) && body.vault_id.is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            "vault_id is required for restore_vault".to_string(),
        ));
    }

    let token = Uuid::new_v4().to_string();
    let expires_at = Utc::now() + Duration::minutes(CONFIRMATION_TTL_MINUTES);

    state.pending.lock().unwrap().insert(
        token.clone(),
        PendingAction {
            step: body.step,
            vault_id: body.vault_id,
            requested_by: body.requested_by,
            expires_at,
        },
    );

    tracing::warn!(step = ?body.step, "DR automation step requested, awaiting confirmation");

    Ok(Json(RequestDrActionResponse {
        step: body.step,
        confirmation_token: token,
        expires_at,
    }))
}

/// `POST /admin/dr/actions/:token/confirm` — phase 2: execute a previously
/// requested destructive DR step, provided its confirmation token hasn't
/// expired. Tokens are single-use: they're removed as soon as they're read,
/// whether or not the underlying command succeeds.
pub async fn confirm_dr_action(
    State(state): State<Arc<DrAutomationState>>,
    Path(token): Path<String>,
) -> Result<Json<DrActionResult>, (StatusCode, String)> {
    let pending = state.pending.lock().unwrap().remove(&token);

    let pending = match pending {
        Some(p) if p.expires_at > Utc::now() => p,
        Some(_) => {
            return Err((
                StatusCode::GONE,
                "confirmation token expired; request the action again".to_string(),
            ))
        }
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                "unknown or already-used confirmation token".to_string(),
            ))
        }
    };

    let result = execute_step(pending.step, pending.vault_id);

    tracing::warn!(
        step = ?pending.step,
        requested_by = %pending.requested_by,
        vault_id = ?pending.vault_id,
        success = result.success,
        "DR automation step executed"
    );

    Ok(Json(result))
}

/// Runs the `stellar contract invoke` command for `step`, mirroring the
/// commands documented in `docs/disaster-recovery-runbook.md`. Contract id,
/// network, and signing identity are read from the same environment
/// variables the runbook's manual commands reference
/// (`CONTRACT_TTL_VAULT`, `STELLAR_NETWORK`, `DEPLOYER_IDENTITY`).
fn execute_step(step: DrStep, vault_id: Option<u64>) -> DrActionResult {
    let executed_at = Utc::now();

    debug_assert!(step.is_destructive());

    let args: Vec<String> = match step {
        DrStep::PauseContract => vec!["pause".to_string()],
        DrStep::UnpauseContract => vec!["unpause".to_string()],
        DrStep::RestoreVault => vec![
            "restore_vault".to_string(),
            "--vault_id".to_string(),
            vault_id.unwrap_or_default().to_string(),
        ],
    };

    let contract_id = std::env::var("CONTRACT_TTL_VAULT").unwrap_or_default();
    let network = std::env::var("STELLAR_NETWORK").unwrap_or_default();
    let source = std::env::var("DEPLOYER_IDENTITY").unwrap_or_default();

    let output = Command::new("stellar")
        .arg("contract")
        .arg("invoke")
        .arg("--id")
        .arg(&contract_id)
        .arg("--network")
        .arg(&network)
        .arg("--source")
        .arg(&source)
        .arg("--")
        .args(&args)
        .output();

    match output {
        Ok(out) if out.status.success() => DrActionResult {
            step,
            success: true,
            output: String::from_utf8_lossy(&out.stdout).to_string(),
            executed_at,
        },
        Ok(out) => DrActionResult {
            step,
            success: false,
            output: String::from_utf8_lossy(&out.stderr).to_string(),
            executed_at,
        },
        Err(e) => DrActionResult {
            step,
            success: false,
            output: format!("failed to invoke stellar CLI: {e}"),
            executed_at,
        },
    }
}
