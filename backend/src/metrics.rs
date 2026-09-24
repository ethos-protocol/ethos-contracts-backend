use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Default maximum number of distinct values allowed per metric label.
pub const DEFAULT_LABEL_CARDINALITY_LIMIT: usize = 100;

/// Shared metrics state for the Ethos-Protocol backend.
#[derive(Default)]
pub struct Metrics {
    pub vaults_total: AtomicU64,
    pub checkins_total: AtomicU64,
    pub releases_total: AtomicU64,
    pub active_vaults: AtomicI64,
    pub request_errors_total: AtomicU64,
    pub http_requests_total: AtomicU64,
    pub contract_paused: AtomicU64,
    /// Per-label cardinality guard: tracks the distinct values seen for each
    /// label name so unbounded label cardinality cannot blow up the backend.
    label_values: Mutex<HashMap<String, Vec<String>>>,
    /// Maximum number of distinct values allowed per label.
    label_cardinality_limit: usize,
}

impl Metrics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            label_cardinality_limit: DEFAULT_LABEL_CARDINALITY_LIMIT,
            ..Self::default()
        })
    }

    /// Create a `Metrics` instance with a custom per-label cardinality limit.
    pub fn with_label_cardinality_limit(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            label_cardinality_limit: limit,
            ..Self::default()
        })
    }

    /// Register a label value for a metric label, enforcing the cardinality
    /// limit. Returns `true` when the value is accepted, or `false` when it is
    /// dropped because the label already reached its cardinality limit. A
    /// warning is logged whenever a value is dropped.
    pub fn register_label_value(&self, label: &str, value: &str) -> bool {
        let mut labels = self
            .label_values
            .lock()
            .expect("metrics label registry poisoned");
        let values = labels.entry(label.to_string()).or_default();

        if values.iter().any(|existing| existing == value) {
            return true;
        }

        if values.len() >= self.label_cardinality_limit {
            tracing::warn!(
                label,
                value,
                limit = self.label_cardinality_limit,
                "dropping metric label value: cardinality limit reached"
            );
            return false;
        }

        values.push(value.to_string());
        true
    }

    /// Number of distinct values currently tracked for a label.
    pub fn label_cardinality(&self, label: &str) -> usize {
        self.label_values
            .lock()
            .expect("metrics label registry poisoned")
            .get(label)
            .map(|values| values.len())
            .unwrap_or(0)
    }

    /// Render all metrics in Prometheus text exposition format.
    pub fn render(&self) -> String {
        let mut out = String::new();

        push_counter(
            &mut out,
            "ethos_protocol_vaults_total",
            "Total vaults created",
            self.vaults_total.load(Ordering::Relaxed),
        );
        push_counter(
            &mut out,
            "ethos_protocol_checkins_total",
            "Total check-ins performed",
            self.checkins_total.load(Ordering::Relaxed),
        );
        push_counter(
            &mut out,
            "ethos_protocol_releases_total",
            "Total vault releases triggered",
            self.releases_total.load(Ordering::Relaxed),
        );
        push_gauge_i64(
            &mut out,
            "ethos_protocol_active_vaults",
            "Currently active (non-released) vaults",
            self.active_vaults.load(Ordering::Relaxed),
        );
        push_counter(
            &mut out,
            "ethos_protocol_request_errors_total",
            "Total API errors",
            self.request_errors_total.load(Ordering::Relaxed),
        );
        push_counter(
            &mut out,
            "ethos_protocol_http_requests_total",
            "Total HTTP requests",
            self.http_requests_total.load(Ordering::Relaxed),
        );
        push_gauge(
            &mut out,
            "ethos_protocol_contract_paused",
            "1 if contract is paused, 0 otherwise",
            self.contract_paused.load(Ordering::Relaxed),
        );

        out
    }
}

fn push_counter(out: &mut String, name: &str, help: &str, value: u64) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} counter");
    let _ = writeln!(out, "{name} {value}");
}

fn push_gauge(out: &mut String, name: &str, help: &str, value: u64) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} gauge");
    let _ = writeln!(out, "{name} {value}");
}

fn push_gauge_i64(out: &mut String, name: &str, help: &str, value: i64) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} gauge");
    let _ = writeln!(out, "{name} {value}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_contains_all_metrics() {
        let m = Metrics::new();
        m.vaults_total.store(5, Ordering::Relaxed);
        m.checkins_total.store(10, Ordering::Relaxed);
        m.contract_paused.store(1, Ordering::Relaxed);

        let output = m.render();
        assert!(output.contains("ethos_protocol_vaults_total 5"));
        assert!(output.contains("ethos_protocol_checkins_total 10"));
        assert!(output.contains("ethos_protocol_contract_paused 1"));
    }

    #[test]
    fn test_render_prometheus_format() {
        let m = Metrics::new();
        let output = m.render();
        assert!(output.contains("# HELP ethos_protocol_vaults_total"));
        assert!(output.contains("# TYPE ethos_protocol_vaults_total counter"));
        assert!(output.contains("# TYPE ethos_protocol_active_vaults gauge"));
    }

    #[test]
    fn test_label_value_within_limit_is_accepted() {
        let m = Metrics::with_label_cardinality_limit(3);
        assert!(m.register_label_value("user_id", "user-1"));
        assert!(m.register_label_value("user_id", "user-2"));
        assert_eq!(m.label_cardinality("user_id"), 2);
    }

    #[test]
    fn test_duplicate_label_value_is_idempotent() {
        let m = Metrics::with_label_cardinality_limit(2);
        assert!(m.register_label_value("user_id", "user-1"));
        assert!(m.register_label_value("user_id", "user-1"));
        assert_eq!(m.label_cardinality("user_id"), 1);
    }

    #[test]
    fn test_label_value_beyond_limit_is_dropped() {
        let m = Metrics::with_label_cardinality_limit(2);
        assert!(m.register_label_value("user_id", "user-1"));
        assert!(m.register_label_value("user_id", "user-2"));
        assert!(!m.register_label_value("user_id", "user-3"));
        assert_eq!(m.label_cardinality("user_id"), 2);
    }

    #[test]
    fn test_cardinality_limit_is_per_label() {
        let m = Metrics::with_label_cardinality_limit(1);
        assert!(m.register_label_value("user_id", "user-1"));
        assert!(!m.register_label_value("user_id", "user-2"));
        assert!(m.register_label_value("vault_id", "vault-1"));
        assert_eq!(m.label_cardinality("vault_id"), 1);
    }
}
