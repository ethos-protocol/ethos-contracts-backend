use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{ConnectInfo, State},
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

    let peer_ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let ip = extract_client_ip(req.headers(), peer_ip);
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
        ip_address: extract_client_ip(headers, None),
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
        Some(token) if constant_time_eq(token.as_bytes(), expected_key.as_bytes()) => Ok(()),
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

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut difference = a.len() ^ b.len();
    for index in 0..a.len().max(b.len()) {
        let left = a.get(index).copied().unwrap_or_default();
        let right = b.get(index).copied().unwrap_or_default();
        difference |= (left ^ right) as usize;
    }
    difference == 0
}

pub fn extract_client_ip(headers: &HeaderMap, peer_ip: Option<IpAddr>) -> String {
    let trusted_proxies = std::env::var("TRUSTED_PROXY_IPS")
        .unwrap_or_default()
        .split(',')
        .filter_map(|value| value.trim().parse::<IpAddr>().ok())
        .collect::<Vec<_>>();

    extract_client_ip_from_trusted_proxies(headers, peer_ip, &trusted_proxies)
}

fn extract_client_ip_from_trusted_proxies(
    headers: &HeaderMap,
    peer_ip: Option<IpAddr>,
    trusted_proxies: &[IpAddr],
) -> String {
    let Some(peer_ip) = peer_ip else {
        return "unknown".to_string();
    };

    if trusted_proxies.contains(&peer_ip) {
        let forwarded = headers
            .get_all("x-forwarded-for")
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .ok()?
                    .split(',')
                    .map(str::trim)
                    .map(str::parse::<IpAddr>)
                    .collect::<Result<Vec<_>, _>>()
                    .ok()
            })
            .collect::<Option<Vec<_>>>()
            .and_then(|chain| {
                chain
                    .into_iter()
                    .flatten()
                    .rev()
                    .find(|ip| !trusted_proxies.contains(ip))
            });
        if let Some(ip) = forwarded {
            return ip.to_string();
        }

        if let Some(ip) = headers
            .get("x-real-ip")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<IpAddr>().ok())
        {
            return ip.to_string();
        }
    }

    peer_ip.to_string()
}

fn extract_user_id(headers: &HeaderMap) -> String {
    headers
        .get("x-user-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

/// Compute a tamper-evident checkpoint hash for a retention purge (#875).
///
/// The hash covers the purge cutoff, the number of purged rows, and the
/// per-row digests of the purged entries so an auditor can verify that a gap
/// in `audit_logs` corresponds to a recorded retention purge rather than
/// tampering. The result is a hex-encoded SHA-256 digest.
pub fn purge_checkpoint_hash(
    cutoff: &str,
    purged_count: usize,
    purged_digests: &[String],
) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(b"audit-purge-checkpoint-v1");
    hasher.update(cutoff.as_bytes());
    hasher.update(b"\x1f");
    hasher.update(purged_count.to_string().as_bytes());
    for digest in purged_digests {
        hasher.update(b"\x1e");
        hasher.update(digest.as_bytes());
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Build a stable per-row digest for a purged audit entry (#875).
///
/// Used as input to [`purge_checkpoint_hash`] so the checkpoint commits to the
/// exact set of purged rows.
pub fn audit_entry_digest(entry: &AuditLogEntry) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(entry.id.to_string().as_bytes());
    hasher.update(b"\x1f");
    hasher.update(entry.timestamp.to_rfc3339().as_bytes());
    hasher.update(b"\x1f");
    hasher.update(entry.user_id.as_bytes());
    hasher.update(b"\x1f");
    hasher.update(entry.action.as_bytes());
    hasher.update(b"\x1f");
    hasher.update(entry.resource.as_bytes());
    hasher.update(b"\x1f");
    hasher.update(entry.result.as_bytes());
    hasher.update(b"\x1f");
    hasher.update(entry.ip_address.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_comparison_requires_exact_match() {
        assert!(constant_time_eq(b"admin-key", b"admin-key"));
        assert!(!constant_time_eq(b"Xadmin-key", b"admin-key"));
        assert!(!constant_time_eq(b"admin-key", b"admin-keyX"));
        assert!(!constant_time_eq(b"", b"admin-key"));
    }

    #[test]
    fn purge_checkpoint_hash_is_deterministic_and_covers_inputs() {
        let digests = vec!["aa".to_string(), "bb".to_string()];
        let base = purge_checkpoint_hash("2024-01-01T00:00:00Z", 2, &digests);
        assert_eq!(base, purge_checkpoint_hash("2024-01-01T00:00:00Z", 2, &digests));
        assert_eq!(base.len(), 64);

        // Changing the cutoff, count, or purged rows changes the checkpoint.
        assert_ne!(base, purge_checkpoint_hash("2024-02-01T00:00:00Z", 2, &digests));
        assert_ne!(base, purge_checkpoint_hash("2024-01-01T00:00:00Z", 3, &digests));
        assert_ne!(
            base,
            purge_checkpoint_hash("2024-01-01T00:00:00Z", 2, &["aa".to_string()])
        );
    }

    #[test]
    fn purge_leaves_a_verifiable_marker() {
        // Simulate a purge: build the marker from the purged rows, then verify
        // that recomputing the checkpoint from the same inputs matches.
        let purged = vec![
            AuditLogEntry {
                id: 1,
                timestamp: Utc::now(),
                user_id: "u1".to_string(),
                action: "GET".to_string(),
                resource: "/api/things".to_string(),
                result: "success".to_string(),
                ip_address: "127.0.0.1".to_string(),
                details: None,
            },
            AuditLogEntry {
                id: 2,
                timestamp: Utc::now(),
                user_id: "u2".to_string(),
                action: "POST".to_string(),
                resource: "/api/things".to_string(),
                result: "success".to_string(),
                ip_address: "127.0.0.2".to_string(),
                details: None,
            },
        ];
        let digests: Vec<String> = purged.iter().map(audit_entry_digest).collect();
        let cutoff = "2024-01-01T00:00:00Z";
        let marker = purge_checkpoint_hash(cutoff, purged.len(), &digests);

        // An auditor recomputing from the recorded marker inputs gets the same hash.
        assert_eq!(marker, purge_checkpoint_hash(cutoff, purged.len(), &digests));
        // Tampering with the purged set invalidates the marker.
        let mut tampered = digests.clone();
        tampered[0] = "00".to_string();
        assert_ne!(marker, purge_checkpoint_hash(cutoff, purged.len(), &tampered));
    }
}
