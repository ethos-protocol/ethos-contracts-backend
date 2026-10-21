//! Readiness probe for `GET /ready` (#698).
//!
//! Each subsystem is checked independently under a timeout, so one hung
//! dependency cannot stall the probe. A failed *required* check turns the
//! response into `503` so the load balancer stops routing to this instance.
//! Non-required checks are still reported, but do not evict the instance.

use std::{future::Future, sync::Arc, time::Duration, time::Instant};

use axum::{http::StatusCode, Json};
use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value};

use crate::{
    consensus::NodeCache, db::AppState, rpc_pool::RpcPool, scheduler, webhook::WebhookState,
};

/// Per-check time budget. Keeps `/ready` responsive even when a dependency hangs.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(2);

/// The scheduler ticks once a minute; three missed ticks means it is dead.
pub const SCHEDULER_MAX_AGE_SECS: i64 = 180;

/// Outcome of one subsystem check. Error strings are fixed messages so that
/// internal error details are never exposed on an unauthenticated endpoint.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CheckResult {
    pub required: bool,
    /// `"ok"`, `"failed"`, or `"timeout"`.
    pub status: &'static str,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<&'static str>,
}

/// Run one check under `timeout`, mapping its outcome to a [`CheckResult`].
pub async fn run_check<F>(required: bool, timeout: Duration, check: F) -> CheckResult
where
    F: Future<Output = Result<(), &'static str>>,
{
    let start = Instant::now();
    let (status, error) = match tokio::time::timeout(timeout, check).await {
        Ok(Ok(())) => ("ok", None),
        Ok(Err(msg)) => ("failed", Some(msg)),
        Err(_) => ("timeout", Some("check timed out")),
    };
    CheckResult {
        required,
        status,
        latency_ms: start.elapsed().as_millis() as u64,
        error,
    }
}

/// Run a synchronous check on the blocking pool, so a contended mutex cannot
/// hold up the async runtime.
async fn blocking<F>(check: F) -> Result<(), &'static str>
where
    F: FnOnce() -> Result<(), &'static str> + Send + 'static,
{
    match tokio::task::spawn_blocking(check).await {
        Ok(result) => result,
        Err(_) => Err("check task panicked"),
    }
}

/// `Ok` when the scheduler has ticked within `max_age` seconds of `now`.
pub fn scheduler_fresh(last_tick: i64, now: i64, max_age: i64) -> Result<(), &'static str> {
    if last_tick == 0 {
        return Err("scheduler has not ticked");
    }
    if now.saturating_sub(last_tick) > max_age {
        return Err("scheduler tick is stale");
    }
    Ok(())
}

/// The webhook delivery path depends on the registry and the dead-letter
/// store. Deliveries run inline rather than from a queue, so this checks that
/// both shared stores are usable (not poisoned) by a previous panic.
fn webhook_state_ok(webhooks: &WebhookState) -> Result<(), &'static str> {
    webhooks
        .store
        .lock()
        .map_err(|_| "webhook registry unavailable")?;
    webhooks
        .dlq_state
        .store
        .lock()
        .map_err(|_| "dead-letter store unavailable")?;
    Ok(())
}

async fn rpc_reachable(pool: Arc<RpcPool>) -> Result<(), &'static str> {
    match pool.health_check().await {
        Ok(true) => Ok(()),
        Ok(false) => Err("rpc endpoint returned non-2xx"),
        Err(_) => Err("rpc endpoint unreachable"),
    }
}

/// Run every subsystem check concurrently. Returned in a stable name order.
pub async fn run_checks(state: &AppState, timeout: Duration) -> Vec<(&'static str, CheckResult)> {
    let db = Arc::clone(&state.db);
    let consensus: Arc<NodeCache> = Arc::clone(&state.consensus);
    let webhooks = Arc::clone(&state.webhook_state);
    let rpc_pool = Arc::clone(&state.rpc_pool);

    let (database, cache, notifications, webhook_queue, rpc) = tokio::join!(
        run_check(
            true,
            timeout,
            blocking(move || { db.check_connectivity().map_err(|_| "database unreachable") }),
        ),
        run_check(
            true,
            timeout,
            blocking(move || consensus.probe().map_err(|_| "cache backend unreachable")),
        ),
        run_check(true, timeout, async {
            scheduler_fresh(
                scheduler::last_heartbeat(),
                Utc::now().timestamp(),
                SCHEDULER_MAX_AGE_SECS,
            )
        },),
        run_check(true, timeout, blocking(move || webhook_state_ok(&webhooks)),),
        // Not required: an outage of the external RPC must not take every
        // instance out of rotation at once.
        run_check(false, timeout, rpc_reachable(rpc_pool)),
    );

    vec![
        ("database", database),
        ("consensus_cache", cache),
        ("notification_scheduler", notifications),
        ("webhook_queue", webhook_queue),
        ("rpc_pool", rpc),
    ]
}

/// Overall readiness, derived from per-check results.
/// Returns `(ready, overall)` where `overall` is `"ok"`, `"degraded"`, or `"unavailable"`.
pub fn aggregate(checks: &[(&'static str, CheckResult)]) -> (bool, &'static str) {
    let ready = checks.iter().all(|(_, c)| !c.required || c.status == "ok");
    let all_ok = checks.iter().all(|(_, c)| c.status == "ok");
    let overall = match (ready, all_ok) {
        (false, _) => "unavailable",
        (true, false) => "degraded",
        (true, true) => "ok",
    };
    (ready, overall)
}

/// Build the `/ready` response: `200` when every required check passes, else `503`.
pub async fn ready_response(state: &AppState) -> (StatusCode, Json<Value>) {
    let checks = run_checks(state, CHECK_TIMEOUT).await;
    let (ready, overall) = aggregate(&checks);

    let database_ok = checks
        .iter()
        .any(|(name, c)| *name == "database" && c.status == "ok");
    let check_map: serde_json::Map<String, Value> = checks
        .iter()
        .map(|(name, c)| {
            (
                name.to_string(),
                serde_json::to_value(c).unwrap_or(Value::Null),
            )
        })
        .collect();

    let body = json!({
        "status": overall,
        "version": env!("CARGO_PKG_VERSION"),
        // Kept for existing dashboards that read this field.
        "database": if database_ok { "connected" } else { "unreachable" },
        "checks": check_map,
    });
    let code = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(required: bool, status: &'static str) -> CheckResult {
        CheckResult {
            required,
            status,
            latency_ms: 0,
            error: None,
        }
    }

    #[test]
    fn scheduler_never_ticked_is_not_fresh() {
        assert!(scheduler_fresh(0, 1_000, SCHEDULER_MAX_AGE_SECS).is_err());
    }

    #[test]
    fn scheduler_recent_tick_is_fresh() {
        assert!(scheduler_fresh(1_000, 1_060, SCHEDULER_MAX_AGE_SECS).is_ok());
    }

    #[test]
    fn scheduler_stale_tick_is_not_fresh() {
        assert!(scheduler_fresh(
            1_000,
            1_000 + SCHEDULER_MAX_AGE_SECS + 1,
            SCHEDULER_MAX_AGE_SECS
        )
        .is_err());
    }

    #[tokio::test]
    async fn hung_check_times_out() {
        let r = run_check(
            true,
            Duration::from_millis(10),
            std::future::pending::<Result<(), &'static str>>(),
        )
        .await;
        assert_eq!(r.status, "timeout");
        assert!(r.required);
    }

    #[tokio::test]
    async fn failed_check_reports_its_fixed_message() {
        let r = run_check(true, Duration::from_secs(1), async {
            Err::<(), _>("database unreachable")
        })
        .await;
        assert_eq!(r.status, "failed");
        assert_eq!(r.error, Some("database unreachable"));
    }

    #[test]
    fn required_failure_makes_service_unavailable() {
        let checks = [
            ("database", result(true, "failed")),
            ("rpc_pool", result(false, "ok")),
        ];
        assert_eq!(aggregate(&checks), (false, "unavailable"));
    }

    #[test]
    fn optional_failure_only_degrades() {
        let checks = [
            ("database", result(true, "ok")),
            ("rpc_pool", result(false, "failed")),
        ];
        assert_eq!(aggregate(&checks), (true, "degraded"));
    }

    #[test]
    fn all_checks_passing_is_ok() {
        let checks = [
            ("database", result(true, "ok")),
            ("rpc_pool", result(false, "ok")),
        ];
        assert_eq!(aggregate(&checks), (true, "ok"));
    }
}
