//! `/health` endpoint handler with Soroban RPC reachability checking.
//!
//! Issue #867: the previous `/health` endpoint reported only the API process's
//! own liveness. Operators need to know whether the Soroban RPC dependency is
//! also reachable so they can distinguish two distinct failure modes:
//!
//! | Scenario                                  | `status` field | HTTP  |
//! |-------------------------------------------|----------------|-------|
//! | API process alive, RPC reachable (or N/A) | `"ok"`         | 200   |
//! | API process alive, RPC unreachable/slow   | `"degraded"`   | 200   |
//! | API process itself failing                | (never reached)| 5xx   |
//!
//! The `"degraded"` state does **not** return a non-2xx status code so that
//! load-balancer health checks continue to route traffic to the server (other
//! endpoints that do not require RPC calls remain available).  Callers that
//! need strict RPC availability should inspect `rpc.reachable` in the
//! response body.
//!
//! # Architecture
//!
//! ```text
//! GET /health
//!   │
//!   ├─ check_rpc_reachability(RPC_ENDPOINT env var)
//!   │     ├─ empty endpoint  → ("not_configured", true)   [ok]
//!   │     ├─ 2xx response    → ("ok", true)               [ok]
//!   │     ├─ non-2xx         → ("error_response", false)  [degraded]
//!   │     └─ network error   → ("unreachable", false)     [degraded]
//!   │
//!   └─ JSON response: { status, version, rpc: { endpoint, status, reachable } }
//! ```
//!
//! # Configuration
//!
//! | Environment variable | Description                           |
//! |----------------------|---------------------------------------|
//! | `RPC_ENDPOINT`       | Soroban RPC base URL to probe.        |
//! |                      | Leave empty to skip the RPC probe.    |
//! | `HEALTH_RPC_TIMEOUT_SECS` | TCP+request timeout for the probe (default 5 s). |

use std::time::Duration;

use axum::Json;

// ── RPC reachability ──────────────────────────────────────────────────────────

/// Result of a single RPC health probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcHealthResult {
    /// Short status string embedded in the `/health` response.
    pub status: &'static str,
    /// `true` when the RPC endpoint is considered reachable.
    pub reachable: bool,
}

/// Probe the Soroban RPC endpoint and return a health result.
///
/// A custom `client` is accepted to allow injection in unit tests
/// (e.g. with `mockito`).  Pass `None` to have the function build a
/// default short-timeout client.
pub async fn check_rpc_reachability(endpoint: &str) -> RpcHealthResult {
    check_rpc_reachability_with_timeout(endpoint, Duration::from_secs(5)).await
}

/// Same as [`check_rpc_reachability`] but with a configurable timeout,
/// useful for tests that want to drive timeout behaviour.
pub async fn check_rpc_reachability_with_timeout(
    endpoint: &str,
    timeout: Duration,
) -> RpcHealthResult {
    if endpoint.is_empty() {
        return RpcHealthResult {
            status: "not_configured",
            reachable: true,
        };
    }

    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build();

    let client = match client {
        Ok(c) => c,
        Err(_) => {
            return RpcHealthResult {
                status: "client_build_error",
                reachable: false,
            }
        }
    };

    match client.get(endpoint).send().await {
        Ok(resp) if resp.status().is_success() => RpcHealthResult {
            status: "ok",
            reachable: true,
        },
        Ok(_) => RpcHealthResult {
            status: "error_response",
            reachable: false,
        },
        Err(_) => RpcHealthResult {
            status: "unreachable",
            reachable: false,
        },
    }
}

// ── Handler ───────────────────────────────────────────────────────────────────

/// `GET /health` — process liveness + RPC dependency health.
///
/// Always returns HTTP 200 so that load-balancer probes continue to route
/// traffic to the process. Inspect `status` and `rpc.reachable` for
/// fine-grained dependency health.
pub async fn health_handler() -> Json<serde_json::Value> {
    let rpc_endpoint = std::env::var("RPC_ENDPOINT").unwrap_or_default();
    let timeout_secs: u64 = std::env::var("HEALTH_RPC_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);

    let rpc = check_rpc_reachability_with_timeout(
        &rpc_endpoint,
        Duration::from_secs(timeout_secs),
    )
    .await;

    let overall_status = if rpc.reachable { "ok" } else { "degraded" };

    Json(serde_json::json!({
        "status": overall_status,
        "version": env!("CARGO_PKG_VERSION"),
        "rpc": {
            "endpoint": if rpc_endpoint.is_empty() { "<not configured>".to_string() } else { rpc_endpoint },
            "status": rpc.status,
            "reachable": rpc.reachable,
        }
    }))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── check_rpc_reachability: not configured ────────────────────────────────

    /// When `RPC_ENDPOINT` is empty the probe skips the network and reports
    /// healthy — local/test environments without an RPC node should not be
    /// treated as degraded.
    #[tokio::test]
    async fn test_rpc_not_configured_reports_ok() {
        let result = check_rpc_reachability("").await;
        assert_eq!(result.status, "not_configured");
        assert!(result.reachable, "empty endpoint should be treated as reachable");
    }

    // ── check_rpc_reachability: 2xx response ──────────────────────────────────

    /// A 2xx response from the RPC endpoint reports `("ok", true)`.
    #[tokio::test]
    async fn test_rpc_reachable_2xx() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/")
            .with_status(200)
            .with_body("{}")
            .create_async()
            .await;

        let result = check_rpc_reachability(&server.url()).await;
        assert_eq!(result.status, "ok");
        assert!(result.reachable);
    }

    // ── check_rpc_reachability: non-2xx response ──────────────────────────────

    /// A non-2xx response (e.g. 503) reports `("error_response", false)`.
    /// The overall health status should be `"degraded"`.
    #[tokio::test]
    async fn test_rpc_error_response_is_degraded() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/")
            .with_status(503)
            .with_body("Service Unavailable")
            .create_async()
            .await;

        let result = check_rpc_reachability(&server.url()).await;
        assert_eq!(result.status, "error_response");
        assert!(!result.reachable, "503 response should mark RPC as unreachable");
    }

    // ── check_rpc_reachability: network error / unreachable ───────────────────

    /// A connection to a port nothing is listening on should resolve to
    /// `("unreachable", false)`.
    #[tokio::test]
    async fn test_rpc_unreachable_network_error() {
        // Nothing listens on port 1; the connection will be refused quickly.
        let result = check_rpc_reachability_with_timeout(
            "http://127.0.0.1:1",
            Duration::from_millis(500),
        )
        .await;
        assert_eq!(result.status, "unreachable");
        assert!(!result.reachable, "refused connection should mark RPC as unreachable");
    }

    // ── health_handler response shape ─────────────────────────────────────────

    /// When RPC_ENDPOINT is not set the handler should return status "ok"
    /// and include the rpc.status field.
    #[tokio::test]
    async fn test_health_handler_no_rpc_endpoint_is_ok() {
        // Ensure env var is absent for this test.
        std::env::remove_var("RPC_ENDPOINT");

        let Json(body) = health_handler().await;
        assert_eq!(body["status"], "ok");
        assert!(body["version"].is_string());
        assert_eq!(body["rpc"]["status"], "not_configured");
        assert_eq!(body["rpc"]["reachable"], true);
    }

    /// When RPC_ENDPOINT points to a live mock that returns 200 the handler
    /// should report `"ok"` overall with `rpc.reachable = true`.
    #[tokio::test]
    async fn test_health_handler_rpc_up() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/")
            .with_status(200)
            .with_body("{}")
            .create_async()
            .await;

        std::env::set_var("RPC_ENDPOINT", server.url());

        let Json(body) = health_handler().await;

        std::env::remove_var("RPC_ENDPOINT");

        assert_eq!(body["status"], "ok", "full body: {:?}", body);
        assert_eq!(body["rpc"]["reachable"], true);
        assert_eq!(body["rpc"]["status"], "ok");
    }

    /// When RPC_ENDPOINT points to a mock that returns 503 the handler should
    /// report `"degraded"` with `rpc.reachable = false`.
    #[tokio::test]
    async fn test_health_handler_rpc_degraded() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/")
            .with_status(503)
            .with_body("down")
            .create_async()
            .await;

        std::env::set_var("RPC_ENDPOINT", server.url());

        let Json(body) = health_handler().await;

        std::env::remove_var("RPC_ENDPOINT");

        assert_eq!(
            body["status"], "degraded",
            "503 from RPC should degrade overall status, full body: {:?}",
            body
        );
        assert_eq!(body["rpc"]["reachable"], false);
        assert_eq!(body["rpc"]["status"], "error_response");
    }

    /// When the RPC endpoint is unreachable the handler reports `"degraded"`.
    #[tokio::test]
    async fn test_health_handler_rpc_unreachable() {
        std::env::set_var("RPC_ENDPOINT", "http://127.0.0.1:1");
        std::env::set_var("HEALTH_RPC_TIMEOUT_SECS", "1");

        let Json(body) = health_handler().await;

        std::env::remove_var("RPC_ENDPOINT");
        std::env::remove_var("HEALTH_RPC_TIMEOUT_SECS");

        assert_eq!(
            body["status"], "degraded",
            "unreachable RPC should degrade overall status"
        );
        assert_eq!(body["rpc"]["reachable"], false);
        assert_eq!(body["rpc"]["status"], "unreachable");
    }
}
