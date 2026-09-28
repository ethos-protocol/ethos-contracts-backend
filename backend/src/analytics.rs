//! API usage analytics and reporting for operators.
//!
//! Tracks endpoint usage statistics, error rates, and latencies to enable
//! performance optimization and capacity planning.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Usage statistics for a single endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointStats {
    pub endpoint: String,
    pub method: String,
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub average_latency_ms: f64,
    pub max_latency_ms: f64,
    pub min_latency_ms: f64,
    pub last_updated: DateTime<Utc>,
}

/// Query parameters for filtering analytics data.
#[derive(Debug, Deserialize)]
pub struct AnalyticsQuery {
    pub endpoint: Option<String>,
    pub method: Option<String>,
}

#[derive(Debug, Default)]
struct EndpointMetrics {
    total_requests: u64,
    successful_requests: u64,
    failed_requests: u64,
    total_latency_ms: f64,
    max_latency_ms: f64,
    min_latency_ms: f64,
}

#[derive(Default)]
struct Inner {
    metrics: HashMap<String, EndpointMetrics>,
}

/// Shared analytics state tracking API usage patterns.
#[derive(Default)]
pub struct AnalyticsStore {
    inner: RwLock<Inner>,
}

impl AnalyticsStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Record a request to an endpoint.
    pub fn record_request(
        &self,
        endpoint: &str,
        method: &str,
        latency_ms: f64,
        success: bool,
    ) {
        let key = format!("{} {}", method, endpoint);
        let mut inner = self.inner.write().expect("analytics lock poisoned");
        let metrics = inner.metrics.entry(key).or_default();

        metrics.total_requests += 1;
        if success {
            metrics.successful_requests += 1;
        } else {
            metrics.failed_requests += 1;
        }

        metrics.total_latency_ms += latency_ms;
        if latency_ms > metrics.max_latency_ms {
            metrics.max_latency_ms = latency_ms;
        }
        if metrics.min_latency_ms == 0.0 || latency_ms < metrics.min_latency_ms {
            metrics.min_latency_ms = latency_ms;
        }
    }

    /// Get statistics for all endpoints, optionally filtered.
    pub fn get_stats(&self, endpoint: Option<&str>, method: Option<&str>) -> Vec<EndpointStats> {
        let inner = self.inner.read().expect("analytics lock poisoned");
        inner
            .metrics
            .iter()
            .filter_map(|(key, metrics)| {
                let parts: Vec<&str> = key.split(' ').collect();
                if parts.len() != 2 {
                    return None;
                }
                let ep_method = parts[0];
                let ep_name = parts[1];

                if let Some(filter_method) = method {
                    if ep_method != filter_method {
                        return None;
                    }
                }

                if let Some(filter_endpoint) = endpoint {
                    if !ep_name.contains(filter_endpoint) {
                        return None;
                    }
                }

                let avg_latency = if metrics.total_requests > 0 {
                    metrics.total_latency_ms / metrics.total_requests as f64
                } else {
                    0.0
                };

                Some(EndpointStats {
                    endpoint: ep_name.to_string(),
                    method: ep_method.to_string(),
                    total_requests: metrics.total_requests,
                    successful_requests: metrics.successful_requests,
                    failed_requests: metrics.failed_requests,
                    average_latency_ms: avg_latency,
                    max_latency_ms: metrics.max_latency_ms,
                    min_latency_ms: metrics.min_latency_ms,
                    last_updated: Utc::now(),
                })
            })
            .collect()
    }

    /// Get error rate for an endpoint as a percentage.
    pub fn error_rate(&self, endpoint: &str, method: &str) -> Option<f64> {
        let key = format!("{} {}", method, endpoint);
        let inner = self.inner.read().expect("analytics lock poisoned");
        inner.metrics.get(&key).and_then(|m| {
            if m.total_requests == 0 {
                return None;
            }
            Some((m.failed_requests as f64 / m.total_requests as f64) * 100.0)
        })
    }
}

/// `GET /analytics/usage` - retrieve usage statistics and metrics.
pub async fn get_usage_analytics(
    State(store): State<Arc<AnalyticsStore>>,
    Query(params): Query<AnalyticsQuery>,
) -> impl IntoResponse {
    let stats = store.get_stats(params.endpoint.as_deref(), params.method.as_deref());
    Json(serde_json::json!({ "stats": stats }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_increments_total_requests() {
        let store = AnalyticsStore::default();
        store.record_request("/users", "GET", 42.0, true);
        store.record_request("/users", "GET", 45.0, true);

        let stats = store.get_stats(None, None);
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].total_requests, 2);
    }

    #[test]
    fn successful_and_failed_requests_tracked() {
        let store = AnalyticsStore::default();
        store.record_request("/users", "GET", 50.0, true);
        store.record_request("/users", "GET", 60.0, false);

        let stats = store.get_stats(None, None);
        assert_eq!(stats[0].successful_requests, 1);
        assert_eq!(stats[0].failed_requests, 1);
    }

    #[test]
    fn average_latency_calculated() {
        let store = AnalyticsStore::default();
        store.record_request("/api", "POST", 100.0, true);
        store.record_request("/api", "POST", 200.0, true);

        let stats = store.get_stats(None, None);
        assert_eq!(stats[0].average_latency_ms, 150.0);
    }

    #[test]
    fn max_and_min_latency_tracked() {
        let store = AnalyticsStore::default();
        store.record_request("/data", "GET", 30.0, true);
        store.record_request("/data", "GET", 100.0, true);
        store.record_request("/data", "GET", 50.0, true);

        let stats = store.get_stats(None, None);
        assert_eq!(stats[0].max_latency_ms, 100.0);
        assert_eq!(stats[0].min_latency_ms, 30.0);
    }

    #[test]
    fn error_rate_calculated_correctly() {
        let store = AnalyticsStore::default();
        store.record_request("/api", "GET", 50.0, true);
        store.record_request("/api", "GET", 50.0, true);
        store.record_request("/api", "GET", 50.0, false);

        let rate = store.error_rate("/api", "GET").expect("should have rate");
        assert_eq!(rate, 100.0 / 3.0);
    }

    #[test]
    fn filter_by_endpoint() {
        let store = AnalyticsStore::default();
        store.record_request("/users", "GET", 50.0, true);
        store.record_request("/posts", "GET", 50.0, true);

        let stats = store.get_stats(Some("users"), None);
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].endpoint, "/users");
    }

    #[test]
    fn filter_by_method() {
        let store = AnalyticsStore::default();
        store.record_request("/users", "GET", 50.0, true);
        store.record_request("/users", "POST", 50.0, true);

        let stats = store.get_stats(None, Some("GET"));
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].method, "GET");
    }
}
