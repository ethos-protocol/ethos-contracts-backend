use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Default maximum number of distinct values allowed per metric label.
pub const DEFAULT_LABEL_CARDINALITY_LIMIT: usize = 100;

/// Upper bounds (in seconds) for the HTTP request duration histogram.
/// These match the standard Prometheus bucket layout used by the dashboards
/// and the `EthosHighApiLatencyP99` alert.
pub const HTTP_REQUEST_DURATION_BUCKETS: [f64; 12] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

/// A cumulative histogram of HTTP request durations, in seconds.
#[derive(Debug)]
pub struct Histogram {
    /// Cumulative count of observations less than or equal to each bucket
    /// upper bound in `HTTP_REQUEST_DURATION_BUCKETS`.
    buckets: [AtomicU64; HTTP_REQUEST_DURATION_BUCKETS.len()],
    /// Sum of all observed durations, stored as the bit pattern of an `f64`.
    sum_bits: AtomicU64,
    /// Total number of observations.
    count: AtomicU64,
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            sum_bits: AtomicU64::new(0f64.to_bits()),
            count: AtomicU64::new(0),
        }
    }
}

impl Histogram {
    /// Record a single observation, in seconds.
    pub fn observe(&self, seconds: f64) {
        for (i, bound) in HTTP_REQUEST_DURATION_BUCKETS.iter().enumerate() {
            if seconds <= *bound {
                self.buckets[i].fetch_add(1, Ordering::Relaxed);
            }
        }
        self.count.fetch_add(1, Ordering::Relaxed);

        // Update the sum with a compare-and-swap loop over the f64 bit pattern.
        let mut current = self.sum_bits.load(Ordering::Relaxed);
        loop {
            let new_sum = f64::from_bits(current) + seconds;
            match self.sum_bits.compare_exchange_weak(
                current,
                new_sum.to_bits(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
    }

    /// Total number of observations.
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// Sum of all observed durations, in seconds.
    pub fn sum(&self) -> f64 {
        f64::from_bits(self.sum_bits.load(Ordering::Relaxed))
    }

    /// Cumulative bucket counts paired with their upper bounds.
    pub fn buckets(&self) -> impl Iterator<Item = (f64, u64)> + '_ {
        HTTP_REQUEST_DURATION_BUCKETS
            .iter()
            .copied()
            .zip(self.buckets.iter().map(|b| b.load(Ordering::Relaxed)))
    }
}

/// Shared metrics state for the Ethos-Protocol backend.
pub struct Metrics {
    pub vaults_total: AtomicU64,
    pub checkins_total: AtomicU64,
    pub releases_total: AtomicU64,
    pub active_vaults: AtomicI64,
    pub request_errors_total: AtomicU64,
    pub http_requests_total: AtomicU64,
    pub contract_paused: AtomicU64,
    /// Count of on-chain `upgrade()` events observed by the event indexer.
    /// Feeds the `EthosContractUpgradeInProgress` alert — see
    /// docs/monitoring-guide.md and monitoring/alert_rules.yml.
    pub contract_upgrade_events_total: AtomicU64,
    /// Count of failed credential lifecycle transitions. Feeds the
    /// `EthosCredentialLifecycleAnomalies` alert.
    pub credential_lifecycle_errors_total: AtomicU64,
    /// Count of successful credential lifecycle state transitions. Feeds the
    /// credential lifecycle dashboard.
    pub credential_lifecycle_transitions_total: AtomicU64,
    /// Histogram of HTTP request durations in seconds. Feeds the
    /// `EthosHighApiLatencyP99` alert and the API latency dashboards.
    pub http_request_duration_seconds: Histogram,
    /// Blue-green slot health, keyed by `(slot, deployment_id, health)`.
    /// Feeds the `EthosBlueGreenSlotUnhealthy` alert.
    bluegreen_slot_health: Mutex<HashMap<(String, String, String), u64>>,
    /// Blue-green rollbacks, keyed by `service`.
    /// Feeds the `EthosBlueGreenRollbackTriggered` alert.
    bluegreen_rollbacks_total: Mutex<HashMap<String, u64>>,
    /// Distinct values registered per label, enforced by `register_label_value`.
    label_values: Mutex<HashMap<String, Vec<String>>>,
    /// Maximum distinct values allowed per label before new ones are dropped.
    label_cardinality_limit: usize,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            vaults_total: AtomicU64::new(0),
            checkins_total: AtomicU64::new(0),
            releases_total: AtomicU64::new(0),
            active_vaults: AtomicI64::new(0),
            request_errors_total: AtomicU64::new(0),
            http_requests_total: AtomicU64::new(0),
            contract_paused: AtomicU64::new(0),
            contract_upgrade_events_total: AtomicU64::new(0),
            credential_lifecycle_errors_total: AtomicU64::new(0),
            credential_lifecycle_transitions_total: AtomicU64::new(0),
            http_request_duration_seconds: Histogram::default(),
            bluegreen_slot_health: Mutex::new(HashMap::new()),
            bluegreen_rollbacks_total: Mutex::new(HashMap::new()),
            label_values: Mutex::new(HashMap::new()),
            label_cardinality_limit: DEFAULT_LABEL_CARDINALITY_LIMIT,
        }
    }
}

impl Metrics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
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
            .unwrap_or_else(|p| p.into_inner());
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

    /// Record the health of a blue-green slot. `health` is typically `"1"`
    /// for healthy and `"0"` for unhealthy.
    pub fn set_bluegreen_slot_health(&self, slot: &str, deployment_id: &str, health: &str) {
        let mut map = self
            .bluegreen_slot_health
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        *map.entry((slot.to_string(), deployment_id.to_string(), health.to_string()))
            .or_insert(0) = 1;
    }

    /// Record a blue-green rollback for the given service.
    pub fn inc_bluegreen_rollback(&self, service: &str) {
        let mut map = self
            .bluegreen_rollbacks_total
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        *map.entry(service.to_string()).or_insert(0) += 1;
    }

    /// Number of distinct values currently tracked for a label.
    pub fn label_cardinality(&self, label: &str) -> usize {
        self.label_values
            .lock()
            .unwrap_or_else(|p| p.into_inner())
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
        push_counter(
            &mut out,
            "ethos_protocol_contract_upgrade_events_total",
            "Total on-chain upgrade() events observed",
            self.contract_upgrade_events_total.load(Ordering::Relaxed),
        );
        push_counter(
            &mut out,
            "ethos_protocol_credential_lifecycle_errors_total",
            "Total failed credential lifecycle transitions",
            self.credential_lifecycle_errors_total.load(Ordering::Relaxed),
        );
        push_counter(
            &mut out,
            "ethos_protocol_credential_lifecycle_transitions_total",
            "Total credential lifecycle state transitions",
            self.credential_lifecycle_transitions_total.load(Ordering::Relaxed),
        );

        push_histogram(
            &mut out,
            "ethos_protocol_http_request_duration_seconds",
            "HTTP request duration in seconds",
            &self.http_request_duration_seconds,
        );

        {
            let map = self
                .bluegreen_slot_health
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let _ = writeln!(
                out,
                "# HELP ethos_protocol_bluegreen_slot_health 1 if the blue-green slot is healthy, 0 otherwise"
            );
            let _ = writeln!(out, "# TYPE ethos_protocol_bluegreen_slot_health gauge");
            for ((slot, deployment_id, health), value) in map.iter() {
                let _ = writeln!(
                    out,
                    "ethos_protocol_bluegreen_slot_health{{slot=\"{slot}\",deployment_id=\"{deployment_id}\",health=\"{health}\"}} {value}"
                );
            }
        }

        {
            let map = self
                .bluegreen_rollbacks_total
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let _ = writeln!(
                out,
                "# HELP ethos_protocol_bluegreen_rollbacks_total Total blue-green rollbacks triggered"
            );
            let _ = writeln!(out, "# TYPE ethos_protocol_bluegreen_rollbacks_total counter");
            for (service, value) in map.iter() {
                let _ = writeln!(
                    out,
                    "ethos_protocol_bluegreen_rollbacks_total{{service=\"{service}\"}} {value}"
                );
            }
        }

        out
    }
}

/// Renders a Prometheus histogram with `_bucket`, `_sum` and `_count` series.
fn push_histogram(out: &mut String, name: &str, help: &str, histogram: &Histogram) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} histogram");
    for (bound, count) in histogram.buckets() {
        let _ = writeln!(out, "{name}_bucket{{le=\"{bound}\"}} {count}");
    }
    let _ = writeln!(out, "{name}_bucket{{le=\"+Inf\"}} {}", histogram.count());
    let _ = writeln!(out, "{name}_sum {}", histogram.sum());
    let _ = writeln!(out, "{name}_count {}", histogram.count());
}

/// Renders a Prometheus counter line. `pub(crate)` so the load shedding
/// (#128), adaptive batching (#131) and predictive scaling (#130) modules
/// can append their own metrics in the same exposition format.
pub(crate) fn push_counter(out: &mut String, name: &str, help: &str, value: u64) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} counter");
    let _ = writeln!(out, "{name} {value}");
}

pub(crate) fn push_gauge(out: &mut String, name: &str, help: &str, value: u64) {
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
    fn test_render_contains_monitoring_metrics() {
        let m = Metrics::new();
        m.contract_upgrade_events_total.store(2, Ordering::Relaxed);
        m.credential_lifecycle_errors_total.store(3, Ordering::Relaxed);

        let output = m.render();
        assert!(output.contains("ethos_protocol_contract_upgrade_events_total 2"));
        assert!(output.contains("ethos_protocol_credential_lifecycle_errors_total 3"));
    }

    #[test]
    fn test_render_contains_credential_lifecycle_transitions() {
        let m = Metrics::new();
        m.credential_lifecycle_transitions_total
            .store(7, Ordering::Relaxed);

        let output = m.render();
        assert!(output.contains("ethos_protocol_credential_lifecycle_transitions_total 7"));
    }

    #[test]
    fn test_render_contains_http_request_duration_histogram() {
        let m = Metrics::new();
        m.http_request_duration_seconds.observe(0.042);
        m.http_request_duration_seconds.observe(0.3);

        let output = m.render();
        assert!(output.contains("# TYPE ethos_protocol_http_request_duration_seconds histogram"));
        assert!(output.contains("ethos_protocol_http_request_duration_seconds_bucket{le=\"0.05\"} 1"));
        assert!(output.contains("ethos_protocol_http_request_duration_seconds_bucket{le=\"0.5\"} 2"));
        assert!(output.contains("ethos_protocol_http_request_duration_seconds_bucket{le=\"+Inf\"} 2"));
        assert!(output.contains("ethos_protocol_http_request_duration_seconds_count 2"));
        assert!(output.contains("ethos_protocol_http_request_duration_seconds_sum"));
    }

    #[test]
    fn test_render_contains_bluegreen_metrics() {
        let m = Metrics::new();
        m.set_bluegreen_slot_health("blue", "deploy-1", "0");
        m.inc_bluegreen_rollback("api");
        m.inc_bluegreen_rollback("api");

        let output = m.render();
        assert!(output.contains("# TYPE ethos_protocol_bluegreen_slot_health gauge"));
        assert!(output.contains(
            "ethos_protocol_bluegreen_slot_health{slot=\"blue\",deployment_id=\"deploy-1\",health=\"0\"} 1"
        ));
        assert!(output.contains("# TYPE ethos_protocol_bluegreen_rollbacks_total counter"));
        assert!(output.contains("ethos_protocol_bluegreen_rollbacks_total{service=\"api\"} 2"));
    }
}
