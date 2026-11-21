//! Error enrichment (Issue: "Errors lack context").
//!
//! Adds structured request/user context, correlation IDs, and optional
//! stack traces to error responses so failures are debuggable without
//! having to cross-reference logs by timestamp alone. See
//! `docs/error-format.md` for the resulting JSON shape.

use std::sync::Arc;

use axum::{
    extract::Request,
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use uuid::Uuid;

use crate::error::AppError;

pub const CORRELATION_ID_HEADER: &str = "x-correlation-id";
/// Primary request-correlation header (issue #349). Generated at ingress when
/// the client does not supply one, echoed on every response (including error
/// responses), and propagated into tracing spans, job payloads and queue
/// message headers so a single request can be traced end to end.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// The resolved correlation id for the in-flight request, inserted into the
/// request extensions by [`correlation_id_middleware`] so handlers, jobs and
/// downstream producers can read it with `request.extensions().get::<RequestId>()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(pub String);

impl RequestId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Resolve the correlation id from request headers, preferring `X-Request-Id`
/// and falling back to the legacy `X-Correlation-Id`. Generates a fresh v4 UUID
/// when neither is present.
pub fn resolve_request_id(headers: &HeaderMap) -> String {
    header_str(headers, REQUEST_ID_HEADER)
        .or_else(|| header_str(headers, CORRELATION_ID_HEADER))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| Uuid::new_v4().to_string())
}

/// Snapshot of the HTTP request an error occurred while handling.
#[derive(Debug, Clone, Serialize)]
pub struct RequestContext {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
}

impl RequestContext {
    pub fn from_parts(method: &str, path: &str, query: Option<&str>) -> Self {
        Self {
            method: method.to_string(),
            path: path.to_string(),
            query: query.map(|q| q.to_string()),
        }
    }
}

/// Who was making the request, when known.
#[derive(Debug, Clone, Default, Serialize)]
pub struct UserContext {
    pub user_id: Option<String>,
    pub tenant_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
}

impl UserContext {
    pub fn from_headers(headers: &HeaderMap) -> Self {
        Self {
            user_id: header_str(headers, "x-user-id"),
            tenant_id: header_str(headers, "x-tenant-id"),
            roles: header_str(headers, "x-user-roles")
                .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default(),
        }
    }
}

fn header_str(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// The full context attached to an enriched error: correlation id,
/// timestamp, request info, user info, and (optionally) a captured stack
/// trace.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorContext {
    pub correlation_id: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub request: Option<RequestContext>,
    pub user: Option<UserContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_trace: Option<String>,
}

impl ErrorContext {
    pub fn new() -> Self {
        Self {
            correlation_id: Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            request: None,
            user: None,
            stack_trace: None,
        }
    }

    pub fn with_correlation_id(mut self, id: impl Into<String>) -> Self {
        self.correlation_id = id.into();
        self
    }

    pub fn with_request(mut self, request: RequestContext) -> Self {
        self.request = Some(request);
        self
    }

    pub fn with_user(mut self, user: UserContext) -> Self {
        self.user = Some(user);
        self
    }

    /// Captures the current stack trace. Honors `RUST_BACKTRACE` the same
    /// way panics do: if it isn't set, the captured trace will simply say
    /// "disabled backtrace".
    pub fn capture_stack_trace(mut self) -> Self {
        self.stack_trace = Some(std::backtrace::Backtrace::force_capture().to_string());
        self
    }

    /// Builds an `ErrorContext` directly from an in-flight axum request.
    ///
    /// Prefers the `RequestId` inserted by [`correlation_id_middleware`], then
    /// the `X-Request-Id` / `X-Correlation-Id` headers, generating one only as a
    /// last resort so error responses always carry a correlatable id.
    pub fn from_request(request: &Request) -> Self {
        let correlation_id = request
            .extensions()
            .get::<RequestId>()
            .map(|r| r.0.clone())
            .unwrap_or_else(|| resolve_request_id(request.headers()));

        Self::new()
            .with_correlation_id(correlation_id)
            .with_request(RequestContext::from_parts(
                request.method().as_str(),
                request.uri().path(),
                request.uri().query(),
            ))
            .with_user(UserContext::from_headers(request.headers()))
    }
}

impl Default for ErrorContext {
    fn default() -> Self {
        Self::new()
    }
}

/// An `AppError` enriched with request/user/correlation context, ready to
/// be returned directly from a handler.
///
/// The original `AppError` is kept intact (not just its `Display` string) so
/// the underlying failure — e.g. the `rusqlite::Error` inside `AppError::Db`
/// — stays reachable through [`std::error::Error::source`], [`Self::inner`]
/// and [`Self::into_inner`], and is logged when a 5xx response is rendered.
#[derive(Debug, Serialize)]
pub struct EnrichedError {
    pub code: String,
    pub message: String,
    pub context: ErrorContext,
    #[serde(skip)]
    status: StatusCode,
    /// The wrapped error. Not serialized: internal details (SQL errors etc.)
    /// must not leak into client-facing JSON.
    #[serde(skip)]
    source: AppError,
}

impl EnrichedError {
    pub fn new(error: AppError, context: ErrorContext) -> Self {
        let (status, code) = classify(&error);
        Self {
            code,
            message: error.to_string(),
            context,
            status,
            source: error,
        }
    }

    /// HTTP status this error renders as.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The original, unmodified error this context wraps.
    pub fn inner(&self) -> &AppError {
        &self.source
    }

    /// Unwrap back into the original error, discarding the context.
    pub fn into_inner(self) -> AppError {
        self.source
    }
}

impl std::fmt::Display for EnrichedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} (code={}, correlation_id={})",
            self.message, self.code, self.context.correlation_id
        )
    }
}

impl std::error::Error for EnrichedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn classify(error: &AppError) -> (StatusCode, String) {
    match error {
        AppError::NotFound => (StatusCode::NOT_FOUND, "not_found".to_string()),
        AppError::InvalidInput(_) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_input".to_string(),
        ),
        AppError::Db(_) | AppError::DatabaseError => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error".to_string(),
        ),
        AppError::TwoFactorRequired => {
            (StatusCode::UNAUTHORIZED, "two_factor_required".to_string())
        }
        AppError::TwoFactorNotEnabled => (
            StatusCode::BAD_REQUEST,
            "two_factor_not_enabled".to_string(),
        ),
        AppError::TooManyRequests(_) => (
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_requests".to_string(),
        ),
        // Wrapped ApiErrors already carry their own status and code.
        AppError::Api(api) => (api.status(), api.code().to_string()),
    }
}

impl IntoResponse for EnrichedError {
    fn into_response(self) -> Response {
        let status = self.status;
        if status.is_server_error() {
            // The JSON body deliberately omits internal details, so record the
            // full wrapped error here; otherwise the root cause would be lost.
            tracing::error!(
                correlation_id = %self.context.correlation_id,
                code = %self.code,
                error = ?self.source,
                "request failed"
            );
        }
        (status, Json(self)).into_response()
    }
}

/// Convenience extension for attaching context to any `AppError` at the
/// point it's returned from a handler, e.g.
/// `db.get(id).map_err(|_| AppError::NotFound.enrich_from(&request))`.
pub trait EnrichExt {
    fn enrich(self, context: ErrorContext) -> EnrichedError;
}

impl EnrichExt for AppError {
    fn enrich(self, context: ErrorContext) -> EnrichedError {
        EnrichedError::new(self, context)
    }
}

/// Axum middleware that ensures every request/response pair carries a stable
/// correlation id (issue #349).
///
/// - Generates an `X-Request-Id` at ingress when the client did not supply one
///   (accepting a legacy `X-Correlation-Id` as an alias).
/// - Inserts a [`RequestId`] extension and normalises both headers on the
///   request so handlers, job payloads and queue producers can propagate it.
/// - Runs the downstream stack inside a `tracing` span carrying `request_id`,
///   so every log line emitted while handling the request includes it.
/// - Echoes the id on `X-Request-Id` (and `X-Correlation-Id`) of the response,
///   including error responses, which flow through this same layer.
pub async fn correlation_id_middleware(mut request: Request, next: Next) -> Response {
    let resolved = resolve_request_id(request.headers());

    // A client-supplied id that is not a valid header value is replaced with
    // a fresh UUID, so the id in the extension, the tracing span and the
    // response headers always agree (previously the extension kept the bad
    // value while the headers said "invalid").
    let (request_id, header_value) = match HeaderValue::from_str(&resolved) {
        Ok(value) => (resolved, value),
        Err(_) => {
            let generated = Uuid::new_v4().to_string();
            let value = HeaderValue::from_str(&generated)
                .unwrap_or_else(|_| HeaderValue::from_static("invalid"));
            (generated, value)
        }
    };
    request
        .headers_mut()
        .insert(REQUEST_ID_HEADER, header_value.clone());
    request
        .headers_mut()
        .insert(CORRELATION_ID_HEADER, header_value.clone());
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));

    let span = tracing::info_span!("http_request", request_id = %request_id);
    let mut response = tracing::Instrument::instrument(next.run(request), span).await;
    response
        .headers_mut()
        .insert(REQUEST_ID_HEADER, header_value.clone());
    response
        .headers_mut()
        .insert(CORRELATION_ID_HEADER, header_value);
    response
}

/// Marker type so `Arc<()>`-style shared state isn't needed just to mount
/// the middleware above via `from_fn` (kept for symmetry with the other
/// admin modules, which take an explicit state).
pub type SharedNothing = Arc<()>;

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn user_context_parses_roles_header() -> TestResult {
        let mut headers = HeaderMap::new();
        headers.insert("x-user-roles", "admin, operator".parse()?);
        let ctx = UserContext::from_headers(&headers);
        assert_eq!(ctx.roles, vec!["admin".to_string(), "operator".to_string()]);
        Ok(())
    }

    #[test]
    fn enriched_error_preserves_status_mapping() {
        let ctx = ErrorContext::new();
        let err = EnrichedError::new(AppError::NotFound, ctx);
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert_eq!(err.code, "not_found");
    }

    #[test]
    fn enriched_error_keeps_wrapped_error_intact() {
        use std::error::Error as _;

        let db_err = rusqlite::Error::InvalidQuery;
        let expected = db_err.to_string();
        let err = EnrichedError::new(AppError::Db(db_err), ErrorContext::new());

        assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
        // The original AppError is reachable, not just its message…
        assert!(matches!(err.inner(), AppError::Db(rusqlite::Error::InvalidQuery)));
        // …and the full source chain down to the root cause is preserved.
        let root = err.source().and_then(|e| e.source()).map(ToString::to_string);
        assert_eq!(root.as_deref(), Some(expected.as_str()));
        assert!(matches!(err.into_inner(), AppError::Db(_)));
    }

    #[test]
    fn enriched_error_does_not_serialize_wrapped_error() -> TestResult {
        let err = EnrichedError::new(
            AppError::Db(rusqlite::Error::InvalidQuery),
            ErrorContext::new(),
        );
        let json = serde_json::to_value(&err)?;
        assert!(json.get("source").is_none());
        assert_eq!(json["code"], "internal_error");
        Ok(())
    }

    #[test]
    fn resolve_request_id_prefers_request_id_then_correlation_id() -> TestResult {
        let mut headers = HeaderMap::new();
        assert_ne!(resolve_request_id(&headers), resolve_request_id(&headers)); // generated

        headers.insert(CORRELATION_ID_HEADER, "corr-1".parse()?);
        assert_eq!(resolve_request_id(&headers), "corr-1");

        headers.insert(REQUEST_ID_HEADER, "req-1".parse()?);
        assert_eq!(resolve_request_id(&headers), "req-1");
        Ok(())
    }

    // ── End-to-end middleware tests (issue #349) ────────────────────────────

    use axum::{body::Body, extract::Request as AxumRequest, routing::get, Router};
    use tower::ServiceExt;

    fn test_app() -> Router {
        Router::new()
            .route(
                "/echo",
                get(|req: AxumRequest| async move {
                    // Handler observes the id the middleware resolved.
                    req.extensions()
                        .get::<RequestId>()
                        .map(|r| r.0.clone())
                        .unwrap_or_default()
                }),
            )
            .route(
                "/boom",
                get(|| async { AppError::NotFound.into_response() }),
            )
            .layer(axum::middleware::from_fn(correlation_id_middleware))
    }

    /// Read a response header as a string, failing the test if it is absent.
    fn header<'a>(
        resp: &'a Response,
        name: &str,
    ) -> Result<&'a str, Box<dyn std::error::Error>> {
        Ok(resp
            .headers()
            .get(name)
            .ok_or_else(|| format!("missing {name} response header"))?
            .to_str()?)
    }

    async fn body_string(resp: Response) -> Result<String, Box<dyn std::error::Error>> {
        let body = axum::body::to_bytes(resp.into_body(), 1 << 16).await?;
        Ok(String::from_utf8(body.to_vec())?)
    }

    #[tokio::test]
    async fn generates_request_id_at_ingress_when_absent() -> TestResult {
        let resp = test_app()
            .oneshot(Request::builder().uri("/echo").body(Body::empty())?)
            .await?;

        let header_id = header(&resp, REQUEST_ID_HEADER)?.to_string();
        assert!(!header_id.is_empty());
        // Legacy alias is echoed too.
        assert_eq!(header(&resp, CORRELATION_ID_HEADER)?, header_id);

        // Handler saw the same id the response advertises: propagated end to end.
        assert_eq!(body_string(resp).await?, header_id);
        Ok(())
    }

    #[tokio::test]
    async fn preserves_client_supplied_request_id() -> TestResult {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/echo")
                    .header(REQUEST_ID_HEADER, "client-abc-123")
                    .body(Body::empty())?,
            )
            .await?;

        assert_eq!(header(&resp, REQUEST_ID_HEADER)?, "client-abc-123");
        assert_eq!(body_string(resp).await?, "client-abc-123");
        Ok(())
    }

    #[tokio::test]
    async fn error_responses_also_carry_the_request_id() -> TestResult {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/boom")
                    .header(REQUEST_ID_HEADER, "trace-err-9")
                    .body(Body::empty())?,
            )
            .await?;

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(header(&resp, REQUEST_ID_HEADER)?, "trace-err-9");
        Ok(())
    }
}
