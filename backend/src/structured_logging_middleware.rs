//! Middleware for integrating structured logging into Axum request/response pipeline
//!
//! Provides:
//! - Automatic structured logging of all HTTP requests and responses
//! - Request correlation IDs for distributed tracing
//! - Response time tracking
//! - Error logging and status code categorization
//! - Per-module log level configuration

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    middleware::Next,
    response::Response,
};
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use std::time::Instant;

use crate::structured_logging::{LogLevel, StructuredLogger, StructuredLogEntry};

/// HTTP request/response logging middleware for structured logs.
pub async fn structured_logging_middleware(
    State(logger): State<Arc<StructuredLogger>>,
    req: Request<Body>,
    next: Next,
) -> Response<Body> {
    let method = req.method().to_string();
    let uri = req.uri().to_string();
    let path = uri.split('?').next().unwrap_or(&uri).to_string();

    // Skip health checks and non-API routes
    if is_internal_route(&path) {
        return next.run(req).await;
    }

    // Extract or generate correlation ID
    let correlation_id = extract_correlation_id(req.headers());
    
    // Log incoming request
    let start = Instant::now();
    logger.log(
        StructuredLogEntry::new(LogLevel::Debug, "http_request", "Incoming request")
            .with_field("method", json!(&method))
            .with_field("path", json!(&path))
            .with_field("correlation_id", json!(&correlation_id))
            .with_correlation_id(&correlation_id),
    );

    // Execute the handler
    let response = next.run(req).await;

    // Calculate response time
    let elapsed_ms = start.elapsed().as_millis();
    let status = response.status();

    // Determine log level based on status code
    let (level, event_name) = categorize_response(status);

    // Log outgoing response
    logger.log(
        StructuredLogEntry::new(level, "http_response", event_name)
            .with_field("method", json!(&method))
            .with_field("path", json!(&path))
            .with_field("status", json!(status.as_u16()))
            .with_field("response_time_ms", json!(elapsed_ms))
            .with_correlation_id(&correlation_id),
    );

    response
}

/// Extract correlation ID from request headers or generate a new one.
fn extract_correlation_id(headers: &HeaderMap) -> String {
    headers
        .get("x-correlation-id")
        .or_else(|| headers.get("X-Correlation-ID"))
        .or_else(|| headers.get("x-request-id"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            // Generate a new correlation ID if not present
            format!("req-{}", uuid::Uuid::new_v4())
        })
}

/// Check if the route is internal and should be skipped from logging.
fn is_internal_route(path: &str) -> bool {
    matches!(
        path,
        "/health" | "/ready" | "/metrics" | "/health/consensus"
    ) || path.starts_with("/admin/")
}

/// Categorize response status code to determine logging level.
fn categorize_response(status: StatusCode) -> (LogLevel, &'static str) {
    match status.as_u16() {
        200..=299 => (LogLevel::Debug, "Request successful"),
        300..=399 => (LogLevel::Debug, "Request redirected"),
        400..=499 => (LogLevel::Warn, "Client error"),
        500..=599 => (LogLevel::Error, "Server error"),
        _ => (LogLevel::Info, "Request completed"),
    }
}

/// Logger state for use in app state.
pub struct LoggerState {
    pub logger: Arc<StructuredLogger>,
}

impl LoggerState {
    pub fn new(logger: Arc<StructuredLogger>) -> Self {
        Self { logger }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_correlation_id_from_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-correlation-id", "req-12345".parse().unwrap());
        
        let corr_id = extract_correlation_id(&headers);
        assert_eq!(corr_id, "req-12345");
    }

    #[test]
    fn test_extract_correlation_id_generates_new() {
        let headers = HeaderMap::new();
        let corr_id = extract_correlation_id(&headers);
        
        assert!(corr_id.starts_with("req-"));
        assert!(corr_id.len() > 10);
    }

    #[test]
    fn test_is_internal_route() {
        assert!(is_internal_route("/health"));
        assert!(is_internal_route("/ready"));
        assert!(is_internal_route("/metrics"));
        assert!(!is_internal_route("/api/vaults"));
    }

    #[test]
    fn test_categorize_response_success() {
        let (level, msg) = categorize_response(StatusCode::OK);
        assert_eq!(level, LogLevel::Debug);
        assert_eq!(msg, "Request successful");
    }

    #[test]
    fn test_categorize_response_client_error() {
        let (level, msg) = categorize_response(StatusCode::BAD_REQUEST);
        assert_eq!(level, LogLevel::Warn);
        assert_eq!(msg, "Client error");
    }

    #[test]
    fn test_categorize_response_server_error() {
        let (level, msg) = categorize_response(StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(level, LogLevel::Error);
        assert_eq!(msg, "Server error");
    }
}
