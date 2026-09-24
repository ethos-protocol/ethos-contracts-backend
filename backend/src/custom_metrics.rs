//! Custom metrics registry with cardinality guarding.
//!
//! Custom metrics allow operators to register application-specific metrics.
//! Unbounded label cardinality (for example, using a user ID or request ID as a
//! label value) can blow up the metrics backend's memory and cost. To protect
//! against this, every label has a configurable cardinality limit. Label values
//! beyond the limit are dropped and a warning is logged.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use tracing::warn;

/// Default maximum number of distinct values allowed per label.
pub const DEFAULT_LABEL_CARDINALITY_LIMIT: usize = 100;

/// A single custom metric definition.
#[derive(Debug, Clone)]
pub struct CustomMetric {
    pub name: String,
    pub help: String,
    pub labels: Vec<String>,
}

/// Error returned when a metric cannot be registered.
#[derive(Debug, PartialEq, Eq)]
pub enum CustomMetricError {
    /// A metric with the same name is already registered.
    AlreadyRegistered(String),
    /// The metric definition is invalid (e.g. empty name).
    InvalidDefinition(String),
}

impl std::fmt::Display for CustomMetricError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CustomMetricError::AlreadyRegistered(name) => {
                write!(f, "custom metric '{name}' is already registered")
            }
            CustomMetricError::InvalidDefinition(msg) => {
                write!(f, "invalid custom metric definition: {msg}")
            }
        }
    }
}

impl std::error::Error for CustomMetricError {}

/// Tracks the distinct values observed for each label of a metric so that
/// cardinality can be bounded.
#[derive(Debug)]
struct LabelCardinality {
    limit: usize,
    seen: HashMap<String, HashSet<String>>,
}

impl LabelCardinality {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            seen: HashMap::new(),
        }
    }

    /// Returns `true` if the label value is within the cardinality limit and
    /// should be recorded. Returns `false` if the value should be dropped.
    fn admit(&mut self, metric: &str, label: &str, value: &str) -> bool {
        let values = self.seen.entry(label.to_string()).or_default();
        if values.contains(value) {
            return true;
        }
        if values.len() >= self.limit {
            warn!(
                metric = %metric,
                label = %label,
                limit = self.limit,
                "dropping custom metric label value: cardinality limit exceeded"
            );
            return false;
        }
        values.insert(value.to_string());
        true
    }
}

/// Registry of custom metrics with per-label cardinality guarding.
#[derive(Debug)]
pub struct CustomMetricsRegistry {
    metrics: HashMap<String, CustomMetric>,
    cardinality: Mutex<LabelCardinality>,
}

impl Default for CustomMetricsRegistry {
    fn default() -> Self {
        Self::new(DEFAULT_LABEL_CARDINALITY_LIMIT)
    }
}

impl CustomMetricsRegistry {
    /// Creates a registry with the given per-label cardinality limit.
    pub fn new(label_cardinality_limit: usize) -> Self {
        Self {
            metrics: HashMap::new(),
            cardinality: Mutex::new(LabelCardinality::new(label_cardinality_limit)),
        }
    }

    /// Registers a custom metric definition.
    pub fn register(&mut self, metric: CustomMetric) -> Result<(), CustomMetricError> {
        if metric.name.trim().is_empty() {
            return Err(CustomMetricError::InvalidDefinition(
                "metric name must not be empty".to_string(),
            ));
        }
        if self.metrics.contains_key(&metric.name) {
            return Err(CustomMetricError::AlreadyRegistered(metric.name));
        }
        self.metrics.insert(metric.name.clone(), metric);
        Ok(())
    }

    /// Records a metric observation, dropping label values that exceed the
    /// per-label cardinality limit.
    ///
    /// Returns the labels that were admitted (within the limit). Labels whose
    /// values were dropped are omitted from the returned map.
    pub fn record(
        &self,
        metric_name: &str,
        labels: &HashMap<String, String>,
    ) -> HashMap<String, String> {
        let mut admitted = HashMap::new();
        let mut guard = match self.cardinality.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        for (label, value) in labels {
            if guard.admit(metric_name, label, value) {
                admitted.insert(label.clone(), value.clone());
            }
        }
        admitted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metric(name: &str, labels: &[&str]) -> CustomMetric {
        CustomMetric {
            name: name.to_string(),
            help: "test metric".to_string(),
            labels: labels.iter().map(|l| l.to_string()).collect(),
        }
    }

    fn labels(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn registers_metric() {
        let mut registry = CustomMetricsRegistry::default();
        assert!(registry.register(metric("requests_total", &["route"])).is_ok());
    }

    #[test]
    fn rejects_duplicate_metric() {
        let mut registry = CustomMetricsRegistry::default();
        registry.register(metric("requests_total", &["route"])).unwrap();
        let err = registry
            .register(metric("requests_total", &["route"]))
            .unwrap_err();
        assert_eq!(err, CustomMetricError::AlreadyRegistered("requests_total".to_string()));
    }

    #[test]
    fn rejects_empty_metric_name() {
        let mut registry = CustomMetricsRegistry::default();
        let err = registry.register(metric("  ", &[])).unwrap_err();
        assert!(matches!(err, CustomMetricError::InvalidDefinition(_)));
    }

    #[test]
    fn admits_values_within_limit() {
        let registry = CustomMetricsRegistry::new(3);
        for i in 0..3 {
            let admitted = registry.record("requests_total", &labels(&[("route", &format!("/r{i}"))]));
            assert_eq!(admitted.len(), 1);
        }
    }

    #[test]
    fn drops_values_beyond_limit() {
        let registry = CustomMetricsRegistry::new(2);
        assert_eq!(registry.record("requests_total", &labels(&[("user_id", "1")])).len(), 1);
        assert_eq!(registry.record("requests_total", &labels(&[("user_id", "2")])).len(), 1);
        // Third distinct value exceeds the limit and is dropped.
        assert_eq!(registry.record("requests_total", &labels(&[("user_id", "3")])).len(), 0);
    }

    #[test]
    fn repeated_values_do_not_count_against_limit() {
        let registry = CustomMetricsRegistry::new(1);
        assert_eq!(registry.record("requests_total", &labels(&[("user_id", "1")])).len(), 1);
        // Same value again is still admitted.
        assert_eq!(registry.record("requests_total", &labels(&[("user_id", "1")])).len(), 1);
    }

    #[test]
    fn cardinality_is_tracked_per_label() {
        let registry = CustomMetricsRegistry::new(1);
        assert_eq!(
            registry
                .record("requests_total", &labels(&[("route", "/a"), ("method", "GET")]))
                .len(),
            2
        );
        // Each label has its own budget; new values for both are dropped.
        assert_eq!(
            registry
                .record("requests_total", &labels(&[("route", "/b"), ("method", "POST")]))
                .len(),
            0
        );
    }
}
