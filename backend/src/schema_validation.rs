//! Issue #347 — OpenAPI schema validation.
//!
//! `docs/openapi.yaml` is the published contract for this API, but nothing has
//! kept it in sync with the handlers in [`crate::routes`]. This module loads
//! the spec at startup and exposes:
//!
//! * [`OpenApiSpec`] — a lightweight, dependency-free view of the declared
//!   paths and their operations.
//! * [`openapi_validation_middleware`] — an Axum middleware that checks every
//!   incoming request against the spec and rejects method mismatches on
//!   declared paths with `405 Method Not Allowed`.
//! * [`OpenApiSpec::declared_operations`] — used by the CI drift check
//!   (`.github/workflows/openapi-drift.yml`) to fail the build when the spec
//!   and the router disagree.
//!
//! Full JSON-Schema body validation is intentionally out of scope here (it
//! would pull in a schema-validation crate); this layer enforces the
//! path/method surface, which is what clients most often break on.

use std::collections::BTreeSet;

/// The bundled OpenAPI document, compiled into the binary so validation needs
/// no filesystem access at runtime.
pub const BUNDLED_OPENAPI_YAML: &str = include_str!("../../docs/openapi.yaml");

const HTTP_METHODS: [&str; 7] = ["get", "put", "post", "delete", "options", "head", "patch"];

/// One declared path template and the set of HTTP methods defined for it.
#[derive(Debug, Clone)]
pub struct PathSpec {
    /// The raw OpenAPI path template, e.g. `/api/vaults/{vault_id}/export`.
    pub template: String,
    /// Lower-case HTTP methods declared under this path.
    pub methods: BTreeSet<String>,
    /// Pre-split segments of `template` for matching.
    segments: Vec<String>,
}

impl PathSpec {
    fn new(template: String, methods: BTreeSet<String>) -> Self {
        let segments = split_segments(&template);
        Self {
            template,
            methods,
            segments,
        }
    }

    /// Does this template match a concrete request `path`?
    /// `{param}` segments match any single non-empty segment.
    pub fn matches(&self, path: &str) -> bool {
        let parts = split_segments(path);
        if parts.len() != self.segments.len() {
            return false;
        }
        self.segments
            .iter()
            .zip(parts.iter())
            .all(|(tpl, actual)| (tpl.starts_with('{') && tpl.ends_with('}')) || tpl == actual)
    }
}

fn split_segments(path: &str) -> Vec<String> {
    path.trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Parsed, matchable view of `docs/openapi.yaml`.
#[derive(Debug, Clone, Default)]
pub struct OpenApiSpec {
    paths: Vec<PathSpec>,
}

/// Result of checking a request against the spec.
#[derive(Debug, PartialEq, Eq)]
pub enum ValidationOutcome {
    /// Path is declared and the method is allowed.
    Ok,
    /// Path is declared but this method is not — the allowed methods follow.
    MethodNotAllowed(Vec<String>),
    /// Path is not part of the spec (internal/undeclared route). Passed through.
    Undeclared,
}

impl OpenApiSpec {
    /// Parse the bundled `docs/openapi.yaml`.
    pub fn load_bundled() -> Self {
        Self::parse(BUNDLED_OPENAPI_YAML)
    }

    /// Parse an OpenAPI 3.x YAML document.
    ///
    /// This is a deliberately small indentation-aware reader for the
    /// `paths:` block rather than a full YAML parser: it needs only the path
    /// templates and the method keys beneath each of them.
    pub fn parse(yaml: &str) -> Self {
        let mut paths: Vec<PathSpec> = Vec::new();
        let mut in_paths = false;
        let mut current: Option<(String, BTreeSet<String>)> = None;

        for raw in yaml.lines() {
            let line = raw.trim_end();
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let indent = line.len() - line.trim_start().len();

            // Top-level key.
            if indent == 0 {
                if let Some((tpl, methods)) = current.take() {
                    paths.push(PathSpec::new(tpl, methods));
                }
                in_paths = line.starts_with("paths:");
                continue;
            }
            if !in_paths {
                continue;
            }

            let trimmed = line.trim_start();

            // Path template key: `  /foo/{bar}:`
            if indent == 2 && trimmed.starts_with('/') && trimmed.ends_with(':') {
                if let Some((tpl, methods)) = current.take() {
                    paths.push(PathSpec::new(tpl, methods));
                }
                let tpl = trimmed.trim_end_matches(':').to_string();
                current = Some((tpl, BTreeSet::new()));
                continue;
            }

            // Operation key beneath a path: `    get:`
            if indent == 4 && trimmed.ends_with(':') {
                let key = trimmed.trim_end_matches(':').to_ascii_lowercase();
                if HTTP_METHODS.contains(&key.as_str()) {
                    if let Some((_, methods)) = current.as_mut() {
                        methods.insert(key);
                    }
                }
            }
        }

        if let Some((tpl, methods)) = current.take() {
            paths.push(PathSpec::new(tpl, methods));
        }

        Self { paths }
    }

    /// Number of declared path templates.
    pub fn path_count(&self) -> usize {
        self.paths.len()
    }

    /// Every declared `(METHOD, path-template)` pair, sorted. Consumed by the
    /// CI drift check.
    pub fn declared_operations(&self) -> Vec<(String, String)> {
        let mut ops: Vec<(String, String)> = self
            .paths
            .iter()
            .flat_map(|p| {
                p.methods
                    .iter()
                    .map(move |m| (m.to_ascii_uppercase(), p.template.clone()))
            })
            .collect();
        ops.sort();
        ops
    }

    /// Check a concrete request against the spec.
    pub fn validate(&self, method: &str, path: &str) -> ValidationOutcome {
        let method = method.to_ascii_lowercase();
        let mut matched_any = false;
        for spec in &self.paths {
            if spec.matches(path) {
                matched_any = true;
                if spec.methods.contains(&method) {
                    return ValidationOutcome::Ok;
                }
            }
        }
        if matched_any {
            let mut allowed: Vec<String> = self
                .paths
                .iter()
                .filter(|s| s.matches(path))
                .flat_map(|s| s.methods.iter().map(|m| m.to_ascii_uppercase()))
                .collect();
            allowed.sort();
            allowed.dedup();
            ValidationOutcome::MethodNotAllowed(allowed)
        } else {
            ValidationOutcome::Undeclared
        }
    }
}

// ── Axum middleware ─────────────────────────────────────────────────────────

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

/// Middleware: validate the request path/method against `docs/openapi.yaml`.
///
/// * Declared path + allowed method → forwarded unchanged.
/// * Declared path + wrong method → `405` with an `Allow` header.
/// * Undeclared path → forwarded (internal routes are not in the public spec).
pub async fn openapi_validation_middleware(
    State(spec): State<Arc<OpenApiSpec>>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method().as_str().to_string();
    let path = request.uri().path().to_string();

    match spec.validate(&method, &path) {
        ValidationOutcome::Ok | ValidationOutcome::Undeclared => next.run(request).await,
        ValidationOutcome::MethodNotAllowed(allowed) => (
            StatusCode::METHOD_NOT_ALLOWED,
            [(header::ALLOW, allowed.join(", "))],
            axum::Json(serde_json::json!({
                "error": "method_not_allowed",
                "message": format!(
                    "{method} {path} is not defined in the OpenAPI schema; allowed: {}",
                    allowed.join(", ")
                ),
            })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_spec_parses_with_paths() {
        let spec = OpenApiSpec::load_bundled();
        assert!(
            spec.path_count() >= 20,
            "expected the bundled spec to declare many paths, got {}",
            spec.path_count()
        );
    }

    #[test]
    fn declared_operations_are_non_empty_and_sorted() {
        let ops = OpenApiSpec::load_bundled().declared_operations();
        assert!(ops.contains(&("GET".to_string(), "/health".to_string())));
        let mut sorted = ops.clone();
        sorted.sort();
        assert_eq!(ops, sorted);
    }

    fn fixture() -> OpenApiSpec {
        OpenApiSpec::parse(
            "openapi: 3.1.0\n\
             paths:\n\
             \x20\x20/health:\n\
             \x20\x20\x20\x20get:\n\
             \x20\x20\x20\x20\x20\x20summary: health\n\
             \x20\x20/api/vaults/{vault_id}/export:\n\
             \x20\x20\x20\x20parameters:\n\
             \x20\x20\x20\x20\x20\x20- name: vault_id\n\
             \x20\x20\x20\x20get:\n\
             \x20\x20\x20\x20post:\n\
             components:\n\
             \x20\x20schemas: {}\n",
        )
    }

    #[test]
    fn valid_request_passes() {
        assert_eq!(fixture().validate("GET", "/health"), ValidationOutcome::Ok);
        assert_eq!(
            fixture().validate("POST", "/api/vaults/abc-123/export"),
            ValidationOutcome::Ok
        );
    }

    #[test]
    fn method_mismatch_on_declared_path_is_rejected() {
        match fixture().validate("DELETE", "/api/vaults/abc/export") {
            ValidationOutcome::MethodNotAllowed(allowed) => {
                assert_eq!(allowed, vec!["GET".to_string(), "POST".to_string()]);
            }
            other => panic!("expected MethodNotAllowed, got {other:?}"),
        }
    }

    #[test]
    fn parameters_key_is_not_treated_as_a_method() {
        let ops = fixture().declared_operations();
        assert!(!ops.iter().any(|(m, _)| m == "PARAMETERS"));
    }

    #[test]
    fn undeclared_path_passes_through() {
        assert_eq!(
            fixture().validate("GET", "/internal/metrics"),
            ValidationOutcome::Undeclared
        );
    }

    #[test]
    fn path_param_matches_single_segment_only() {
        let spec = fixture();
        assert_eq!(
            spec.validate("GET", "/api/vaults/a/b/export"),
            ValidationOutcome::Undeclared
        );
    }
}
