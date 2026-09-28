//! Anomaly explanation generation for operators to understand root causes.
//!
//! Provides human-readable explanations of detected anomalies including
//! contributing factors and remediation suggestions.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};

/// Contributing factor to an anomaly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContributingFactor {
    pub factor: String,
    pub severity: String,
    pub description: String,
}

/// Remediation action to resolve an anomaly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemediationSuggestion {
    pub action: String,
    pub priority: String,
    pub estimated_time_minutes: u32,
}

/// Explanation for a detected anomaly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnomalyExplanation {
    pub anomaly_id: String,
    pub metric: String,
    pub explanation: String,
    pub contributing_factors: Vec<ContributingFactor>,
    pub remediation_suggestions: Vec<RemediationSuggestion>,
    pub confidence: f32,
}

#[derive(Debug, Deserialize)]
pub struct ExplainRequest {
    pub metric: String,
    pub value: f64,
    pub baseline_mean: f64,
    pub z_score: f64,
}

#[derive(Default)]
struct Inner {
    explanations: HashMap<String, AnomalyExplanation>,
}

/// Shared anomaly explanation store.
#[derive(Default)]
pub struct ExplanationStore {
    inner: RwLock<Inner>,
}

impl ExplanationStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Generate an explanation for an anomaly.
    pub fn explain_anomaly(
        &self,
        anomaly_id: &str,
        metric: &str,
        value: f64,
        baseline_mean: f64,
        z_score: f64,
    ) -> AnomalyExplanation {
        let explanation = self.generate_explanation(metric, value, baseline_mean, z_score);
        let factors = self.identify_factors(metric, value, baseline_mean, z_score);
        let suggestions = self.suggest_remediation(metric, &factors);
        let confidence = (z_score.abs() / 10.0).min(1.0) as f32;

        let result = AnomalyExplanation {
            anomaly_id: anomaly_id.to_string(),
            metric: metric.to_string(),
            explanation,
            contributing_factors: factors,
            remediation_suggestions: suggestions,
            confidence,
        };

        self.inner
            .write()
            .expect("explanation store lock poisoned")
            .explanations
            .insert(anomaly_id.to_string(), result.clone());

        result
    }

    /// Get a stored explanation by ID.
    pub fn get_explanation(&self, anomaly_id: &str) -> Option<AnomalyExplanation> {
        self.inner
            .read()
            .expect("explanation store lock poisoned")
            .explanations
            .get(anomaly_id)
            .cloned()
    }

    fn generate_explanation(
        &self,
        metric: &str,
        value: f64,
        baseline_mean: f64,
        z_score: f64,
    ) -> String {
        let deviation_pct = ((value - baseline_mean) / baseline_mean.abs() * 100.0).abs();
        let direction = if value > baseline_mean { "increased" } else { "decreased" };

        format!(
            "Metric '{}' {} by {:.1}% (value: {:.2}, baseline: {:.2}, z-score: {:.2})",
            metric, direction, deviation_pct, value, baseline_mean, z_score
        )
    }

    fn identify_factors(
        &self,
        metric: &str,
        value: f64,
        baseline_mean: f64,
        z_score: f64,
    ) -> Vec<ContributingFactor> {
        let mut factors = Vec::new();

        if z_score.abs() > 5.0 {
            factors.push(ContributingFactor {
                factor: "extreme_deviation".to_string(),
                severity: "critical".to_string(),
                description: "Value is extremely far from baseline.".to_string(),
            });
        }

        if metric.contains("cpu") || metric.contains("memory") {
            if value > baseline_mean * 1.5 {
                factors.push(ContributingFactor {
                    factor: "resource_spike".to_string(),
                    severity: "high".to_string(),
                    description: "Resource utilization significantly elevated.".to_string(),
                });
            }
        }

        if metric.contains("latency") || metric.contains("response_time") {
            if value > baseline_mean * 2.0 {
                factors.push(ContributingFactor {
                    factor: "performance_degradation".to_string(),
                    severity: "high".to_string(),
                    description: "Response times have doubled or more.".to_string(),
                });
            }
        }

        if metric.contains("error") {
            factors.push(ContributingFactor {
                factor: "error_rate_increase".to_string(),
                severity: "high".to_string(),
                description: "Error rate has spiked above normal levels.".to_string(),
            });
        }

        factors
    }

    fn suggest_remediation(
        &self,
        metric: &str,
        factors: &[ContributingFactor],
    ) -> Vec<RemediationSuggestion> {
        let mut suggestions = Vec::new();

        if factors.iter().any(|f| f.factor == "resource_spike") {
            suggestions.push(RemediationSuggestion {
                action: "scale_up_resources".to_string(),
                priority: "high".to_string(),
                estimated_time_minutes: 10,
            });
        }

        if factors
            .iter()
            .any(|f| f.factor == "performance_degradation")
        {
            suggestions.push(RemediationSuggestion {
                action: "investigate_slow_queries".to_string(),
                priority: "high".to_string(),
                estimated_time_minutes: 15,
            });
        }

        if factors.iter().any(|f| f.factor == "error_rate_increase") {
            suggestions.push(RemediationSuggestion {
                action: "check_service_logs".to_string(),
                priority: "high".to_string(),
                estimated_time_minutes: 5,
            });
        }

        if factors.iter().any(|f| f.factor == "extreme_deviation") {
            suggestions.push(RemediationSuggestion {
                action: "verify_data_pipeline".to_string(),
                priority: "critical".to_string(),
                estimated_time_minutes: 30,
            });
        }

        suggestions
    }
}

/// `POST /anomalies/explain` - generate explanation for an anomaly.
pub async fn explain_anomaly_handler(
    State(store): State<Arc<ExplanationStore>>,
    Json(req): Json<ExplainRequest>,
) -> impl IntoResponse {
    let anomaly_id = uuid::Uuid::new_v4().to_string();
    let explanation =
        store.explain_anomaly(&anomaly_id, &req.metric, req.value, req.baseline_mean, req.z_score);
    (StatusCode::OK, Json(explanation))
}

/// `GET /anomalies/:id/explanation` - retrieve a stored explanation.
pub async fn get_explanation(
    State(store): State<Arc<ExplanationStore>>,
    Path(anomaly_id): Path<String>,
) -> impl IntoResponse {
    match store.get_explanation(&anomaly_id) {
        Some(explanation) => (StatusCode::OK, Json(explanation)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explain_generates_description() {
        let store = ExplanationStore::default();
        let explanation =
            store.explain_anomaly("anom1", "cpu_pct", 85.0, 45.0, 4.0);

        assert_eq!(explanation.anomaly_id, "anom1");
        assert_eq!(explanation.metric, "cpu_pct");
        assert!(!explanation.explanation.is_empty());
    }

    #[test]
    fn extreme_deviation_identified_as_factor() {
        let store = ExplanationStore::default();
        let explanation = store.explain_anomaly("anom2", "memory_mb", 9000.0, 1000.0, 8.0);

        let factors = explanation.contributing_factors;
        assert!(
            factors.iter().any(|f| f.factor == "extreme_deviation"),
            "should identify extreme deviation"
        );
    }

    #[test]
    fn resource_spike_identified_for_cpu() {
        let store = ExplanationStore::default();
        let explanation = store.explain_anomaly("anom3", "cpu_pct", 90.0, 40.0, 5.0);

        let factors = explanation.contributing_factors;
        assert!(
            factors.iter().any(|f| f.factor == "resource_spike"),
            "should identify resource spike for CPU"
        );
    }

    #[test]
    fn performance_degradation_identified_for_latency() {
        let store = ExplanationStore::default();
        let explanation = store.explain_anomaly("anom4", "latency_ms", 2000.0, 500.0, 3.0);

        let factors = explanation.contributing_factors;
        assert!(
            factors
                .iter()
                .any(|f| f.factor == "performance_degradation"),
            "should identify performance degradation for latency"
        );
    }

    #[test]
    fn error_rate_factor_for_error_metric() {
        let store = ExplanationStore::default();
        let explanation = store.explain_anomaly("anom5", "error_rate", 5.0, 0.1, 4.9);

        let factors = explanation.contributing_factors;
        assert!(
            factors.iter().any(|f| f.factor == "error_rate_increase"),
            "should identify error rate increase"
        );
    }

    #[test]
    fn remediation_suggests_scaling_for_resource_spike() {
        let store = ExplanationStore::default();
        let explanation = store.explain_anomaly("anom6", "cpu_pct", 95.0, 40.0, 5.5);

        let suggestions = explanation.remediation_suggestions;
        assert!(
            suggestions.iter().any(|s| s.action == "scale_up_resources"),
            "should suggest scaling for resource spike"
        );
    }

    #[test]
    fn confidence_based_on_z_score() {
        let store = ExplanationStore::default();
        let exp1 = store.explain_anomaly("anom7", "metric", 100.0, 50.0, 2.0);
        let exp2 = store.explain_anomaly("anom8", "metric", 100.0, 50.0, 8.0);

        assert!(exp2.confidence > exp1.confidence, "higher z-score should have higher confidence");
    }

    #[test]
    fn retrieved_explanation_matches_stored() {
        let store = ExplanationStore::default();
        let explanation =
            store.explain_anomaly("anom9", "cpu_pct", 85.0, 45.0, 4.0);

        let retrieved = store.get_explanation("anom9").expect("should retrieve");
        assert_eq!(retrieved.anomaly_id, explanation.anomaly_id);
        assert_eq!(retrieved.metric, explanation.metric);
    }

    #[test]
    fn nonexistent_explanation_returns_none() {
        let store = ExplanationStore::default();
        assert!(store.get_explanation("nonexistent").is_none());
    }
}
