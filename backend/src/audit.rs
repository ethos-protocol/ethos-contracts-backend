use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    middleware::Next,
    response::Response,
};
use chrono::Utc;

use crate::{db::Db, error::ApiError, models::AuditLogEntry};

/// Axum middleware that logs every API request to the audit log.
pub async fn audit_middleware(
    State(db): State<Arc<Db>>,
    req: Request<Body>,
    next: Next,
) -> Response<Body> {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri.path().to_string();

    // Skip internal routes to reduce noise and avoid recursion
    if path == "/health" || path == "/ready" || path.starts_with("/api/audit-logs") {
        return next.run(req).await;
    }

    let ip = extract_client_ip(req.headers());
    let user_id = extract_user_id(req.headers());

    let response = next.run(req).await;
    let status = response.status();

    let result = if status.is_success() {
        "success"
    } else if status.is_server_error() {
        "error"
    } else {
        "failure"
    };

    // Only log API routes
    if path.starts_with("/api/") {
        let entry = AuditLogEntry {
            id: 0,
            timestamp: Utc::now(),
            user_id,
            action: method.to_string(),
            resource: path,
            result: result.to_string(),
            ip_address: ip,
            details: Some(serde_json::json!({
                "status_code": status.as_u16(),
            })),
        };

        if let Err(e) = db.insert_audit_log(&entry) {
            tracing::error!(error = %e, "failed to persist audit log entry");
        }
    }

    response
}

/// Helper: write a structured audit entry for state modifications.
pub fn log_state_modification(
    db: &Arc<Db>,
    action: &str,
    resource: &str,
    result: &str,
    headers: &HeaderMap,
    details: Option<serde_json::Value>,
) {
    let entry = AuditLogEntry {
        id: 0,
        timestamp: Utc::now(),
        user_id: extract_user_id(headers),
        action: action.to_string(),
        resource: resource.to_string(),
        result: result.to_string(),
        ip_address: extract_client_ip(headers),
        details,
    };
    if let Err(e) = db.insert_audit_log(&entry) {
        tracing::error!(error = %e, "failed to persist audit log entry");
    }
}

/// Check that the request carries a valid admin API key.
///
/// Fails closed: when `ADMIN_API_KEY` is unset, every request is rejected
/// unless `ALLOW_INSECURE_ADMIN=true` is set explicitly (#700). That flag is
/// the opt-in for local development only.
pub fn authorize_admin(headers: &HeaderMap) -> Result<(), ApiError> {
    let api_key = std::env::var("ADMIN_API_KEY").unwrap_or_default();
    let allow_insecure = std::env::var("ALLOW_INSECURE_ADMIN")
        .map(|v| v.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    check_admin_access(&api_key, allow_insecure, headers)
}

/// Decide admin access from an explicit key and opt-in flag (#700).
///
/// Separated from [`authorize_admin`] so the rules can be tested without
/// touching process-wide environment variables.
pub fn check_admin_access(
    expected_key: &str,
    allow_insecure: bool,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    if expected_key.is_empty() {
        if allow_insecure {
            return Ok(());
        }
        return Err(admin_unauthorized());
    }
    let auth_header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    match auth_header.strip_prefix("Bearer ") {
        Some(token) if token == expected_key => Ok(()),
        _ => Err(admin_unauthorized()),
    }
}

fn admin_unauthorized() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "valid admin API key required",
    )
}

#[cfg(test)]
mod admin_auth_tests {
    use super::*;
    use axum::http::HeaderValue;

    fn bearer(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        headers
    }

    #[test]
    fn empty_key_rejects_requests_without_credentials() {
        let err = check_admin_access("", false, &HeaderMap::new()).unwrap_err();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn empty_key_rejects_requests_even_with_a_bearer_token() {
        // With no key configured there is nothing to match against.
        assert!(check_admin_access("", false, &bearer("anything")).is_err());
        assert!(check_admin_access("", false, &bearer("")).is_err());
    }

    #[test]
    fn empty_key_allows_requests_only_with_explicit_insecure_flag() {
        assert!(check_admin_access("", true, &HeaderMap::new()).is_ok());
    }

    #[test]
    fn configured_key_requires_matching_bearer_token() {
        assert!(check_admin_access("s3cret", false, &bearer("s3cret")).is_ok());
        assert!(check_admin_access("s3cret", false, &bearer("wrong")).is_err());
        assert!(check_admin_access("s3cret", false, &HeaderMap::new()).is_err());
    }

    #[test]
    fn insecure_flag_does_not_bypass_a_configured_key() {
        assert!(check_admin_access("s3cret", true, &HeaderMap::new()).is_err());
    }
}

fn extract_client_ip(headers: &HeaderMap) -> String {
    if let Some(val) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        return val
            .split(',')
            .next()
            .unwrap_or("unknown")
            .trim()
            .to_string();
    }
    if let Some(val) = headers.get("x-real-ip").and_then(|v| v.to_str().ok()) {
        return val.to_string();
    }
    "unknown".to_string()
}

fn extract_user_id(headers: &HeaderMap) -> String {
    headers
        .get("x-user-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}
