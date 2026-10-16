//! Custom metric collection and Grafana dashboard support (issue: "Only
//! standard metrics are tracked. Custom dashboards would enable business
//! insights.").
//!
//! `metrics.rs` exposes a fixed set of Prometheus counters/gauges. This
//! module lets callers define custom metrics (`counter` or `gauge`), record
//! arbitrary named, tagged metric points at runtime, aggregate them
//! (sum/avg/min/max/count), export everything in the Prometheus text format
//! (`GET /metrics/custom/export`) so it can be scraped into existing
//! dashboards, and ships ready-made Grafana dashboard templates plus a
//! lightweight sharing mechanism so a generated dashboard link can be handed
//! to a teammate.
//!
//! See `docs/custom-dashboards.md` for end-to-end usage.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, RwLock};

use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A single recorded data point for a custom metric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricPoint {
    pub timestamp: DateTime<Utc>,
    pub value: f64,
    #[serde(default)]
    pub tags: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct RecordMetricRequest {
    pub name: String,
    pub value: f64,
    #[serde(default)]
    pub tags: HashMap<String, String>,
}

/// Supported aggregation functions for custom metric queries.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Aggregation {
    Sum,
    Avg,
    Min,
    Max,
    Count,
}

impl Aggregation {
    fn apply(self, points: &[MetricPoint]) -> f64 {
        if points.is_empty() {
            return 0.0;
        }
        match self {
            Aggregation::Sum => points.iter().map(|p| p.value).sum(),
            Aggregation::Avg => points.iter().map(|p| p.value).sum::<f64>() / points.len() as f64,
            Aggregation::Min => points.iter().map(|p| p.value).fold(f64::INFINITY, f64::min),
            Aggregation::Max => points
                .iter()
                .map(|p| p.value)
                .fold(f64::NEG_INFINITY, f64::max),
            Aggregation::Count => points.len() as f64,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct AggregateQuery {
    #[serde(default = "default_aggregation")]
    pub agg: Aggregation,
}

fn default_aggregation() -> Aggregation {
    Aggregation::Avg
}

#[derive(Debug, Serialize)]
pub struct AggregateResponse {
    pub name: String,
    pub aggregation: String,
    pub value: f64,
    pub sample_count: usize,
}

/// How a defined metric's points are exported to Prometheus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MetricKind {
    /// Monotonic total. Exported as the sum of recorded values per label set.
    Counter,
    /// Point-in-time value. Exported as the most recent value per label set.
    Gauge,
}

impl MetricKind {
    fn prometheus_type(self) -> &'static str {
        match self {
            MetricKind::Counter => "counter",
            MetricKind::Gauge => "gauge",
        }
    }
}

/// Declared metric: its name, kind, and human-readable metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricDefinition {
    pub name: String,
    pub kind: MetricKind,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub unit: Option<String>,
}

/// Prometheus metric name: `[a-zA-Z_:][a-zA-Z0-9_:]*`.
pub fn is_valid_metric_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_' || c == ':')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

/// Prometheus label name: `[a-zA-Z_][a-zA-Z0-9_]*`, without the reserved `__` prefix.
pub fn is_valid_label_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.starts_with("__")
}

/// Check a point before it is stored. Rejects invalid names, non-finite
/// values (which Prometheus cannot represent), and invalid label keys.
pub fn validate_point(
    name: &str,
    value: f64,
    tags: &HashMap<String, String>,
) -> Result<(), String> {
    if !is_valid_metric_name(name) {
        return Err(format!("invalid metric name '{name}'"));
    }
    if !value.is_finite() {
        return Err(format!("value for '{name}' must be finite"));
    }
    if let Some(bad) = tags.keys().find(|k| !is_valid_label_name(k)) {
        return Err(format!("invalid label name '{bad}' on '{name}'"));
    }
    Ok(())
}

/// Escape a label value or help text for the Prometheus text format.
fn escape_prometheus(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}

fn format_labels(tags: &HashMap<String, String>) -> String {
    if tags.is_empty() {
        return String::new();
    }
    let sorted: BTreeMap<&String, &String> = tags.iter().collect();
    let body = sorted
        .iter()
        .map(|(k, v)| format!("{k}=\"{}\"", escape_prometheus(v)))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{body}}}")
}

/// A saved dashboard share: a stable token pointing at a named dashboard so
/// the underlying template/metric selection can change without breaking
/// links already handed out.
#[derive(Debug, Clone, Serialize)]
pub struct DashboardShare {
    pub token: String,
    pub dashboard: String,
    pub created_at: DateTime<Utc>,
    pub created_by: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateShareRequest {
    pub dashboard: String,
    #[serde(default)]
    pub created_by: Option<String>,
}

#[derive(Default)]
struct Inner {
    series: HashMap<String, Vec<MetricPoint>>,
    definitions: HashMap<String, MetricDefinition>,
    shares: HashMap<String, DashboardShare>,
}

/// Shared store for custom metric series and dashboard shares.
#[derive(Default)]
pub struct CustomMetricsStore {
    inner: RwLock<Inner>,
}

impl CustomMetricsStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn record(&self, name: &str, value: f64, tags: HashMap<String, String>) {
        let mut inner = self.inner.write().expect("metrics lock poisoned");
        inner
            .series
            .entry(name.to_string())
            .or_default()
            .push(MetricPoint {
                timestamp: Utc::now(),
                value,
                tags,
            });
    }

    /// Declare a metric. Redefining an existing metric updates its metadata,
    /// but its kind is fixed once declared, since changing it would make the
    /// exported time series ambiguous.
    pub fn define_metric(&self, def: MetricDefinition) -> Result<MetricDefinition, String> {
        if !is_valid_metric_name(&def.name) {
            return Err(format!("invalid metric name '{}'", def.name));
        }
        let mut inner = self.inner.write().expect("metrics lock poisoned");
        if let Some(existing) = inner.definitions.get(&def.name) {
            if existing.kind != def.kind {
                return Err(format!(
                    "metric '{}' is already defined as a {}",
                    def.name,
                    existing.kind.prometheus_type()
                ));
            }
        }
        inner.definitions.insert(def.name.clone(), def.clone());
        Ok(def)
    }

    /// Declared metrics, sorted by name.
    pub fn definitions(&self) -> Vec<MetricDefinition> {
        let mut defs: Vec<MetricDefinition> = self
            .inner
            .read()
            .expect("metrics lock poisoned")
            .definitions
            .values()
            .cloned()
            .collect();
        defs.sort_by(|a, b| a.name.cmp(&b.name));
        defs
    }

    /// Render every metric in the Prometheus text exposition format.
    ///
    /// Each distinct label set becomes one sample. Counters export the sum of
    /// their values; gauges export the value of the newest point. Metrics that
    /// were recorded without a definition are exported as `untyped`.
    pub fn export_prometheus(&self) -> String {
        let inner = self.inner.read().expect("metrics lock poisoned");
        let names: BTreeSet<&String> = inner
            .series
            .keys()
            .chain(inner.definitions.keys())
            .collect();

        let mut out = String::new();
        for name in names {
            let def = inner.definitions.get(name);
            if let Some(help) = def.and_then(|d| d.description.as_ref()) {
                let help = help.replace('\\', "\\\\").replace('\n', "\\n");
                out.push_str(&format!("# HELP {name} {help}\n"));
            }
            let kind = def.map_or("untyped", |d| d.kind.prometheus_type());
            out.push_str(&format!("# TYPE {name} {kind}\n"));

            let points: &[MetricPoint] = inner.series.get(name).map_or(&[], Vec::as_slice);
            let mut by_labels: BTreeMap<String, Vec<&MetricPoint>> = BTreeMap::new();
            for point in points {
                by_labels
                    .entry(format_labels(&point.tags))
                    .or_default()
                    .push(point);
            }
            for (labels, group) in by_labels {
                let value = match def.map(|d| d.kind) {
                    Some(MetricKind::Counter) => group.iter().map(|p| p.value).sum(),
                    _ => group
                        .iter()
                        .max_by_key(|p| p.timestamp)
                        .map_or(0.0, |p| p.value),
                };
                out.push_str(&format!("{name}{labels} {value}\n"));
            }
        }
        out
    }

    pub fn aggregate(&self, name: &str, agg: Aggregation) -> Option<AggregateResponse> {
        let inner = self.inner.read().expect("metrics lock poisoned");
        let points = inner.series.get(name)?;
        Some(AggregateResponse {
            name: name.to_string(),
            aggregation: format!("{agg:?}").to_lowercase(),
            value: agg.apply(points),
            sample_count: points.len(),
        })
    }

    pub fn list_metric_names(&self) -> Vec<String> {
        self.inner
            .read()
            .expect("metrics lock poisoned")
            .series
            .keys()
            .cloned()
            .collect()
    }

    pub fn create_share(&self, req: CreateShareRequest) -> DashboardShare {
        let share = DashboardShare {
            token: Uuid::new_v4().to_string(),
            dashboard: req.dashboard,
            created_at: Utc::now(),
            created_by: req.created_by,
        };
        self.inner
            .write()
            .expect("metrics lock poisoned")
            .shares
            .insert(share.token.clone(), share.clone());
        share
    }

    pub fn resolve_share(&self, token: &str) -> Option<DashboardShare> {
        self.inner
            .read()
            .expect("metrics lock poisoned")
            .shares
            .get(token)
            .cloned()
    }
}

/// Built-in Grafana dashboard templates. These are minimal but valid
/// dashboard JSON models (schema v36) covering the business metrics teams
/// most commonly ask for: vault lifecycle throughput and custom metric
/// exploration. Import them directly in Grafana ("Import dashboard from
/// JSON") or provision them via the Grafana provisioning API.
pub fn grafana_dashboard_templates() -> HashMap<&'static str, serde_json::Value> {
    let mut templates = HashMap::new();

    templates.insert(
        "vault-lifecycle",
        serde_json::json!({
            "title": "Ethos Protocol - Vault Lifecycle",
            "schemaVersion": 36,
            "panels": [
                {
                    "type": "timeseries",
                    "title": "Vaults created / released",
                    "targets": [
                        {"expr": "rate(ethos_protocol_vaults_total[5m])"},
                        {"expr": "rate(ethos_protocol_releases_total[5m])"}
                    ]
                },
                {
                    "type": "stat",
                    "title": "Active vaults",
                    "targets": [{"expr": "ethos_protocol_active_vaults"}]
                }
            ]
        }),
    );

    templates.insert(
        "custom-metric-explorer",
        serde_json::json!({
            "title": "Ethos Protocol - Custom Metric Explorer",
            "schemaVersion": 36,
            "templating": {
                "list": [{"name": "metric_name", "type": "textbox"}]
            },
            "panels": [
                {
                    "type": "timeseries",
                    "title": "$metric_name over time",
                    "targets": [{"expr": "custom_metric{name=\"$metric_name\"}"}]
                }
            ]
        }),
    );

    templates
}

/// `POST /metrics/custom` - record a business metric point.
pub async fn record_custom_metric(
    State(store): State<Arc<CustomMetricsStore>>,
    Json(req): Json<RecordMetricRequest>,
) -> impl IntoResponse {
    if let Err(msg) = validate_point(&req.name, req.value, &req.tags) {
        return (StatusCode::BAD_REQUEST, msg).into_response();
    }
    store.record(&req.name, req.value, req.tags);
    StatusCode::ACCEPTED.into_response()
}

/// `GET /metrics/custom` - list known custom metric names.
pub async fn list_custom_metrics(
    State(store): State<Arc<CustomMetricsStore>>,
) -> impl IntoResponse {
    Json(store.list_metric_names())
}

/// `POST /metrics/custom/definitions` - declare a counter or gauge.
pub async fn define_custom_metric(
    State(store): State<Arc<CustomMetricsStore>>,
    Json(def): Json<MetricDefinition>,
) -> impl IntoResponse {
    match store.define_metric(def) {
        Ok(saved) => (StatusCode::CREATED, Json(saved)).into_response(),
        Err(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
    }
}

/// `GET /metrics/custom/definitions` - list declared metrics.
pub async fn list_custom_metric_definitions(
    State(store): State<Arc<CustomMetricsStore>>,
) -> impl IntoResponse {
    Json(store.definitions())
}

/// `GET /metrics/custom/export` - all custom metrics in Prometheus text format.
pub async fn export_custom_metrics(
    State(store): State<Arc<CustomMetricsStore>>,
) -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        store.export_prometheus(),
    )
}

/// `GET /metrics/custom/:name/aggregate?agg=avg|sum|min|max|count`.
pub async fn aggregate_custom_metric(
    State(store): State<Arc<CustomMetricsStore>>,
    Path(name): Path<String>,
    Query(q): Query<AggregateQuery>,
) -> impl IntoResponse {
    match store.aggregate(&name, q.agg) {
        Some(resp) => (StatusCode::OK, Json(resp)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `GET /dashboards/templates` - list built-in Grafana dashboard templates.
pub async fn list_dashboard_templates() -> impl IntoResponse {
    Json(grafana_dashboard_templates())
}

/// `POST /dashboards/share` - create a shareable link for a dashboard.
pub async fn create_dashboard_share(
    State(store): State<Arc<CustomMetricsStore>>,
    Json(req): Json<CreateShareRequest>,
) -> impl IntoResponse {
    (StatusCode::CREATED, Json(store.create_share(req)))
}

/// `GET /dashboards/shared/:token` - resolve a shared dashboard link.
pub async fn get_shared_dashboard(
    State(store): State<Arc<CustomMetricsStore>>,
    Path(token): Path<String>,
) -> impl IntoResponse {
    match store.resolve_share(&token) {
        Some(share) => (StatusCode::OK, Json(share)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_sum_avg_min_max_count() {
        let store = CustomMetricsStore::default();
        for v in [1.0, 2.0, 3.0, 4.0] {
            store.record("checkout_latency_ms", v, HashMap::new());
        }

        assert_eq!(
            store
                .aggregate("checkout_latency_ms", Aggregation::Sum)
                .unwrap()
                .value,
            10.0
        );
        assert_eq!(
            store
                .aggregate("checkout_latency_ms", Aggregation::Avg)
                .unwrap()
                .value,
            2.5
        );
        assert_eq!(
            store
                .aggregate("checkout_latency_ms", Aggregation::Min)
                .unwrap()
                .value,
            1.0
        );
        assert_eq!(
            store
                .aggregate("checkout_latency_ms", Aggregation::Max)
                .unwrap()
                .value,
            4.0
        );
        assert_eq!(
            store
                .aggregate("checkout_latency_ms", Aggregation::Count)
                .unwrap()
                .sample_count,
            4
        );
    }

    #[test]
    fn unknown_metric_returns_none() {
        let store = CustomMetricsStore::default();
        assert!(store
            .aggregate("does-not-exist", Aggregation::Avg)
            .is_none());
    }

    #[test]
    fn dashboard_templates_are_present_and_valid_json() {
        let templates = grafana_dashboard_templates();
        assert!(templates.contains_key("vault-lifecycle"));
        assert!(templates.contains_key("custom-metric-explorer"));
    }

    #[test]
    fn dashboard_share_round_trips() {
        let store = CustomMetricsStore::default();
        let share = store.create_share(CreateShareRequest {
            dashboard: "vault-lifecycle".to_string(),
            created_by: Some("alice".to_string()),
        });
        let resolved = store
            .resolve_share(&share.token)
            .expect("share should resolve");
        assert_eq!(resolved.dashboard, "vault-lifecycle");
    }

    fn definition(name: &str, kind: MetricKind) -> MetricDefinition {
        MetricDefinition {
            name: name.to_string(),
            kind,
            description: None,
            unit: None,
        }
    }

    fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn metric_names_and_label_names_follow_prometheus_rules() {
        assert!(is_valid_metric_name("vault_releases_total"));
        assert!(is_valid_metric_name("ns:rate"));
        assert!(!is_valid_metric_name("9starts_with_digit"));
        assert!(!is_valid_metric_name("has-dash"));
        assert!(!is_valid_metric_name(""));

        assert!(is_valid_label_name("region"));
        assert!(!is_valid_label_name("__reserved"));
        assert!(!is_valid_label_name("bad-label"));
    }

    #[test]
    fn validate_point_rejects_non_finite_values_and_bad_labels() {
        assert!(validate_point("ok_metric", 1.0, &HashMap::new()).is_ok());
        assert!(validate_point("ok_metric", f64::NAN, &HashMap::new()).is_err());
        assert!(validate_point("ok_metric", f64::INFINITY, &HashMap::new()).is_err());
        assert!(validate_point("ok_metric", 1.0, &tags(&[("bad-key", "x")])).is_err());
        assert!(validate_point("bad name", 1.0, &HashMap::new()).is_err());
    }

    #[test]
    fn defined_metric_round_trips_and_lists_sorted() {
        let store = CustomMetricsStore::default();
        store
            .define_metric(definition("zeta_gauge", MetricKind::Gauge))
            .unwrap();
        store
            .define_metric(definition("alpha_total", MetricKind::Counter))
            .unwrap();

        let names: Vec<String> = store.definitions().into_iter().map(|d| d.name).collect();
        assert_eq!(names, vec!["alpha_total", "zeta_gauge"]);
    }

    #[test]
    fn redefining_with_a_different_kind_is_rejected() {
        let store = CustomMetricsStore::default();
        store
            .define_metric(definition("payouts", MetricKind::Counter))
            .unwrap();
        let err = store
            .define_metric(definition("payouts", MetricKind::Gauge))
            .unwrap_err();
        assert!(err.contains("already defined as a counter"));
    }

    #[test]
    fn invalid_definition_name_is_rejected() {
        let store = CustomMetricsStore::default();
        assert!(store
            .define_metric(definition("not valid", MetricKind::Gauge))
            .is_err());
    }

    #[test]
    fn export_renders_counter_sum_and_gauge_latest_per_label_set() {
        let store = CustomMetricsStore::default();
        store
            .define_metric(MetricDefinition {
                name: "vault_releases_total".to_string(),
                kind: MetricKind::Counter,
                description: Some("Releases per region".to_string()),
                unit: None,
            })
            .unwrap();
        store
            .define_metric(definition("vault_balance", MetricKind::Gauge))
            .unwrap();

        store.record("vault_releases_total", 2.0, tags(&[("region", "eu")]));
        store.record("vault_releases_total", 3.0, tags(&[("region", "eu")]));
        store.record("vault_releases_total", 1.0, tags(&[("region", "us")]));
        store.record("vault_balance", 10.0, HashMap::new());
        store.record("vault_balance", 42.0, HashMap::new());

        let text = store.export_prometheus();
        assert!(text.contains("# HELP vault_releases_total Releases per region\n"));
        assert!(text.contains("# TYPE vault_releases_total counter\n"));
        assert!(text.contains("vault_releases_total{region=\"eu\"} 5\n"));
        assert!(text.contains("vault_releases_total{region=\"us\"} 1\n"));
        assert!(text.contains("# TYPE vault_balance gauge\n"));
        assert!(text.contains("vault_balance 42\n"));
    }

    #[test]
    fn export_marks_undefined_metrics_untyped_and_escapes_label_values() {
        let store = CustomMetricsStore::default();
        store.record("adhoc_metric", 7.0, tags(&[("note", "say \"hi\"\\")]));
        let text = store.export_prometheus();
        assert!(text.contains("# TYPE adhoc_metric untyped\n"));
        assert!(text.contains(r#"adhoc_metric{note="say \"hi\"\\"} 7"#));
    }

    #[test]
    fn export_of_empty_store_is_empty() {
        assert!(CustomMetricsStore::default().export_prometheus().is_empty());
    }
}
