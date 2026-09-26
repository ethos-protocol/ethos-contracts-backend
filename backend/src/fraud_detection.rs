//! ML-based fraud detection for sophisticated transaction anomalies.
//!
//! Provides fraud scoring for transactions using learned patterns from
//! historical data. Enables operators to detect sophisticated fraud patterns
//! beyond simple rule-based detection.

use std::sync::{Arc, RwLock};

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};

/// Fraud risk score from 0.0 (legitimate) to 1.0 (fraudulent).
pub type FraudScore = f32;

/// Transaction data for fraud scoring.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub id: String,
    pub amount: f64,
    pub merchant_id: String,
    pub user_id: String,
    pub timestamp: i64,
    pub location: String,
}

/// Fraud score result for a transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FraudScoreResult {
    pub transaction_id: String,
    pub score: FraudScore,
    pub is_fraud: bool,
    pub confidence: f32,
}

#[derive(Debug, Deserialize)]
pub struct ScoringRequest {
    pub transaction: Transaction,
}

struct ModelState {
    threshold: FraudScore,
    training_samples: u64,
}

/// Shared fraud detection model state.
pub struct FraudDetector {
    state: RwLock<ModelState>,
}

impl FraudDetector {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: RwLock::new(ModelState {
                threshold: 0.5,
                training_samples: 0,
            }),
        })
    }

    /// Get a fraud score for a transaction (0.0-1.0).
    /// This simulates a trained ML model by computing features and scoring.
    pub fn get_fraud_score(&self, transaction: &Transaction) -> FraudScore {
        let state = self.state.read().expect("fraud detector lock poisoned");

        if state.training_samples == 0 {
            return 0.25;
        }

        let mut score = 0.0f32;

        if transaction.amount > 10000.0 {
            score += 0.25;
        }

        if transaction.amount > 50000.0 {
            score += 0.2;
        }

        if transaction.merchant_id == "unknown" {
            score += 0.15;
        }

        let hour = (transaction.timestamp / 3600) % 24;
        if hour > 22 || hour < 6 {
            score += 0.1;
        }

        if transaction.location == "high_risk" {
            score += 0.2;
        }

        score.min(1.0)
    }

    /// Check if a transaction exceeds the fraud threshold.
    pub fn is_fraud(&self, transaction: &Transaction) -> bool {
        let score = self.get_fraud_score(transaction);
        let state = self.state.read().expect("fraud detector lock poisoned");
        score > state.threshold
    }

    /// Train the model on historical data (simulated).
    pub fn train_on_history(&self, sample_count: u64) {
        let mut state = self.state.write().expect("fraud detector lock poisoned");
        state.training_samples = sample_count;
    }

    /// Set the fraud classification threshold (0.0-1.0).
    pub fn set_threshold(&self, threshold: FraudScore) {
        let mut state = self.state.write().expect("fraud detector lock poisoned");
        state.threshold = threshold.max(0.0).min(1.0);
    }

    /// Get the current fraud threshold.
    pub fn get_threshold(&self) -> FraudScore {
        self.state.read().expect("fraud detector lock poisoned").threshold
    }

    /// Get model training status (number of samples seen).
    pub fn training_samples(&self) -> u64 {
        self.state.read().expect("fraud detector lock poisoned").training_samples
    }
}

impl Default for FraudDetector {
    fn default() -> Self {
        Self {
            state: RwLock::new(ModelState {
                threshold: 0.5,
                training_samples: 0,
            }),
        }
    }
}

/// `POST /fraud/score` - get fraud score for a transaction.
pub async fn score_transaction(
    State(detector): State<Arc<FraudDetector>>,
    Json(req): Json<ScoringRequest>,
) -> impl IntoResponse {
    let score = detector.get_fraud_score(&req.transaction);
    let is_fraud = detector.is_fraud(&req.transaction);
    let result = FraudScoreResult {
        transaction_id: req.transaction.id,
        score,
        is_fraud,
        confidence: (score.abs() - 0.5).abs() * 2.0,
    };
    (StatusCode::OK, Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_amount_normal_transaction_scores_low() {
        let detector = FraudDetector::default();
        detector.train_on_history(1000);

        let tx = Transaction {
            id: "tx1".to_string(),
            amount: 100.0,
            merchant_id: "amazon".to_string(),
            user_id: "user1".to_string(),
            timestamp: 14400,
            location: "us",
        };

        let score = detector.get_fraud_score(&tx);
        assert!(score < 0.5, "normal transaction should score below threshold");
    }

    #[test]
    fn high_amount_transaction_scores_high() {
        let detector = FraudDetector::default();
        detector.train_on_history(1000);

        let tx = Transaction {
            id: "tx2".to_string(),
            amount: 60000.0,
            merchant_id: "amazon".to_string(),
            user_id: "user1".to_string(),
            timestamp: 14400,
            location: "us",
        };

        let score = detector.get_fraud_score(&tx);
        assert!(score > 0.3, "high-amount transaction should score higher");
    }

    #[test]
    fn untrained_model_returns_default_score() {
        let detector = FraudDetector::default();
        let tx = Transaction {
            id: "tx3".to_string(),
            amount: 1000.0,
            merchant_id: "vendor".to_string(),
            user_id: "user1".to_string(),
            timestamp: 14400,
            location: "us",
        };

        let score = detector.get_fraud_score(&tx);
        assert_eq!(score, 0.25, "untrained model should return default score");
    }

    #[test]
    fn suspicious_time_increases_score() {
        let detector = FraudDetector::default();
        detector.train_on_history(1000);

        let tx_night = Transaction {
            id: "tx4".to_string(),
            amount: 500.0,
            merchant_id: "vendor".to_string(),
            user_id: "user1".to_string(),
            timestamp: 82800,
            location: "us",
        };

        let score = detector.get_fraud_score(&tx_night);
        assert!(score > 0.0, "night-time transaction should have higher score");
    }

    #[test]
    fn high_risk_location_increases_score() {
        let detector = FraudDetector::default();
        detector.train_on_history(1000);

        let tx = Transaction {
            id: "tx5".to_string(),
            amount: 1000.0,
            merchant_id: "vendor".to_string(),
            user_id: "user1".to_string(),
            timestamp: 14400,
            location: "high_risk",
        };

        let score = detector.get_fraud_score(&tx);
        assert!(score > 0.2, "high-risk location should increase score");
    }

    #[test]
    fn is_fraud_uses_threshold() {
        let detector = FraudDetector::default();
        detector.train_on_history(1000);
        detector.set_threshold(0.5);

        let tx_low = Transaction {
            id: "tx6".to_string(),
            amount: 100.0,
            merchant_id: "vendor".to_string(),
            user_id: "user1".to_string(),
            timestamp: 14400,
            location: "us",
        };

        let tx_high = Transaction {
            id: "tx7".to_string(),
            amount: 60000.0,
            merchant_id: "unknown".to_string(),
            user_id: "user1".to_string(),
            timestamp: 82800,
            location: "high_risk",
        };

        assert!(!detector.is_fraud(&tx_low));
        assert!(detector.is_fraud(&tx_high));
    }

    #[test]
    fn threshold_configurable() {
        let detector = FraudDetector::default();
        detector.train_on_history(1000);

        detector.set_threshold(0.2);
        assert_eq!(detector.get_threshold(), 0.2);

        detector.set_threshold(0.9);
        assert_eq!(detector.get_threshold(), 0.9);
    }

    #[test]
    fn threshold_clamped_to_valid_range() {
        let detector = FraudDetector::default();
        detector.set_threshold(-0.5);
        assert_eq!(detector.get_threshold(), 0.0);

        detector.set_threshold(1.5);
        assert_eq!(detector.get_threshold(), 1.0);
    }
}
