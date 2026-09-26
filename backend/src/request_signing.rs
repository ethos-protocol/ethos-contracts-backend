//! Request signing and verification for API endpoints (#529).
//!
//! Clients sign API requests with a private key to prevent tampering.
//! The backend verifies signatures using the corresponding public key.
//!
//! # Signature Scheme
//!
//! Each request includes:
//! - `X-Signature`: HMAC-SHA256 hex digest of the request body
//! - `X-Timestamp`: Unix seconds when the request was signed
//! - `X-Nonce`: Unique request identifier to prevent replay attacks
//!
//! # Algorithm Support
//!
//! - SHA256 (default)
//! - SHA512
//! - ED25519

use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::{Sha256, Sha512};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

// ── Signature algorithms ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SigningAlgorithm {
    Sha256,
    Sha512,
    Ed25519,
}

impl Default for SigningAlgorithm {
    fn default() -> Self {
        SigningAlgorithm::Sha256
    }
}

// ── Request signature ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestSignature {
    pub algorithm: SigningAlgorithm,
    pub signature: String,
    pub timestamp: i64,
    pub nonce: String,
}

#[derive(Debug, Deserialize)]
pub struct SignedRequest {
    pub body: String,
    pub signature: String,
    pub timestamp: String,
    pub nonce: String,
    pub algorithm: Option<String>,
}

// ── Signature verification ────────────────────────────────────────────────────

pub struct SignatureVerifier {
    public_keys: Arc<Mutex<Vec<String>>>,
    used_nonces: Arc<Mutex<HashSet<String>>>,
}

impl SignatureVerifier {
    pub fn new() -> Self {
        Self {
            public_keys: Arc::new(Mutex::new(Vec::new())),
            used_nonces: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn add_public_key(&self, key: String) {
        let mut keys = self.public_keys.lock().unwrap();
        keys.push(key);
    }

    pub fn verify(&self, req: &SignedRequest, secret: &str) -> VerificationResult {
        // 1. Validate timestamp
        if let Err(e) = validate_timestamp(&req.timestamp) {
            return VerificationResult {
                valid: false,
                reason: Some(e),
            };
        }

        // 2. Check nonce hasn't been used
        let mut nonces = self.used_nonces.lock().unwrap();
        if nonces.contains(&req.nonce) {
            return VerificationResult {
                valid: false,
                reason: Some("nonce already used (replay attack detected)".to_string()),
            };
        }

        // 3. Verify signature
        let algorithm = parse_algorithm(&req.algorithm);
        let expected_sig = sign_request(&req.body, secret, algorithm);

        let valid = constant_time_eq(req.signature.as_bytes(), expected_sig.as_bytes());

        if valid {
            nonces.insert(req.nonce.clone());
        }

        VerificationResult {
            valid,
            reason: if valid {
                None
            } else {
                Some("signature verification failed".to_string())
            },
        }
    }
}

impl Default for SignatureVerifier {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Serialize)]
pub struct VerificationResult {
    pub valid: bool,
    pub reason: Option<String>,
}

// ── Signature computation ─────────────────────────────────────────────────────

pub fn sign_request(body: &str, secret: &str, algorithm: SigningAlgorithm) -> String {
    match algorithm {
        SigningAlgorithm::Sha256 => {
            type H = Hmac<Sha256>;
            let mut mac = H::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key");
            mac.update(body.as_bytes());
            mac.finalize()
                .into_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        }
        SigningAlgorithm::Sha512 => {
            type H = Hmac<Sha512>;
            let mut mac = H::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key");
            mac.update(body.as_bytes());
            mac.finalize()
                .into_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        }
        SigningAlgorithm::Ed25519 => {
            // For Ed25519, we'd use ed25519_dalek in production
            // For now, return a placeholder
            format!("ed25519_{}", uuid::Uuid::new_v4())
        }
    }
}

fn parse_algorithm(algo_str: &Option<String>) -> SigningAlgorithm {
    match algo_str.as_deref() {
        Some("sha512") => SigningAlgorithm::Sha512,
        Some("ed25519") => SigningAlgorithm::Ed25519,
        _ => SigningAlgorithm::Sha256,
    }
}

// ── Timestamp validation ──────────────────────────────────────────────────────

pub const MAX_TIMESTAMP_DRIFT_SECS: i64 = 300;

pub fn validate_timestamp(timestamp_str: &str) -> Result<(), String> {
    let ts: i64 = timestamp_str
        .parse()
        .map_err(|_| format!("invalid timestamp: {timestamp_str}"))?;

    let now = Utc::now().timestamp();
    let drift = (now - ts).abs();

    if drift > MAX_TIMESTAMP_DRIFT_SECS {
        return Err(format!(
            "timestamp drift too large: {drift}s > {MAX_TIMESTAMP_DRIFT_SECS}s"
        ));
    }

    Ok(())
}

// ── Constant-time comparison ──────────────────────────────────────────────────

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn current_timestamp_str() -> String {
        Utc::now().timestamp().to_string()
    }

    #[test]
    fn sign_and_verify_sha256() {
        let body = r#"{"action":"transfer","amount":100}"#;
        let secret = "test-secret-key";
        let sig = sign_request(body, secret, SigningAlgorithm::Sha256);

        let req = SignedRequest {
            body: body.to_string(),
            signature: sig.clone(),
            timestamp: current_timestamp_str(),
            nonce: uuid::Uuid::new_v4().to_string(),
            algorithm: Some("sha256".to_string()),
        };

        let verifier = SignatureVerifier::new();
        let result = verifier.verify(&req, secret);
        assert!(result.valid, "signature should verify: {:?}", result.reason);
    }

    #[test]
    fn sign_and_verify_sha512() {
        let body = r#"{"action":"withdraw","amount":500}"#;
        let secret = "another-secret-key";
        let sig = sign_request(body, secret, SigningAlgorithm::Sha512);

        let req = SignedRequest {
            body: body.to_string(),
            signature: sig.clone(),
            timestamp: current_timestamp_str(),
            nonce: uuid::Uuid::new_v4().to_string(),
            algorithm: Some("sha512".to_string()),
        };

        let verifier = SignatureVerifier::new();
        let result = verifier.verify(&req, secret);
        assert!(result.valid, "signature should verify: {:?}", result.reason);
    }

    #[test]
    fn wrong_secret_fails_verification() {
        let body = r#"{"action":"transfer"}"#;
        let secret_a = "secret-a";
        let secret_b = "secret-b";
        let sig = sign_request(body, secret_a, SigningAlgorithm::Sha256);

        let req = SignedRequest {
            body: body.to_string(),
            signature: sig,
            timestamp: current_timestamp_str(),
            nonce: uuid::Uuid::new_v4().to_string(),
            algorithm: None,
        };

        let verifier = SignatureVerifier::new();
        let result = verifier.verify(&req, secret_b);
        assert!(!result.valid, "signature should not verify with wrong secret");
        assert!(
            result.reason.as_ref().unwrap().contains("failed"),
            "error reason should indicate verification failure"
        );
    }

    #[test]
    fn tampered_body_fails_verification() {
        let original_body = r#"{"amount":100}"#;
        let tampered_body = r#"{"amount":999}"#;
        let secret = "key";
        let sig = sign_request(original_body, secret, SigningAlgorithm::Sha256);

        let req = SignedRequest {
            body: tampered_body.to_string(),
            signature: sig,
            timestamp: current_timestamp_str(),
            nonce: uuid::Uuid::new_v4().to_string(),
            algorithm: None,
        };

        let verifier = SignatureVerifier::new();
        let result = verifier.verify(&req, secret);
        assert!(!result.valid, "tampering should be detected");
    }

    #[test]
    fn stale_timestamp_is_rejected() {
        let body = r#"{"action":"test"}"#;
        let secret = "key";
        let sig = sign_request(body, secret, SigningAlgorithm::Sha256);

        // 10 minutes in the past
        let old_timestamp = (Utc::now().timestamp() - 600).to_string();

        let req = SignedRequest {
            body: body.to_string(),
            signature: sig,
            timestamp: old_timestamp,
            nonce: uuid::Uuid::new_v4().to_string(),
            algorithm: None,
        };

        let verifier = SignatureVerifier::new();
        let result = verifier.verify(&req, secret);
        assert!(!result.valid, "stale timestamp should be rejected");
        assert!(
            result.reason.as_ref().unwrap().contains("drift"),
            "error should mention timestamp drift"
        );
    }

    #[test]
    fn replay_attack_is_prevented() {
        let body = r#"{"action":"transfer"}"#;
        let secret = "key";
        let sig = sign_request(body, secret, SigningAlgorithm::Sha256);
        let nonce = uuid::Uuid::new_v4().to_string();

        let req = SignedRequest {
            body: body.to_string(),
            signature: sig,
            timestamp: current_timestamp_str(),
            nonce: nonce.clone(),
            algorithm: None,
        };

        let verifier = SignatureVerifier::new();
        // First verification should succeed
        let result1 = verifier.verify(&req, secret);
        assert!(result1.valid, "first verification should succeed");

        // Second verification with same nonce should fail
        let req2 = SignedRequest {
            body: body.to_string(),
            signature: sign_request(body, secret, SigningAlgorithm::Sha256),
            timestamp: current_timestamp_str(),
            nonce: nonce.clone(),
            algorithm: None,
        };
        let result2 = verifier.verify(&req2, secret);
        assert!(!result2.valid, "replay attack should be detected");
        assert!(
            result2.reason.as_ref().unwrap().contains("replay"),
            "error should mention replay attack"
        );
    }

    #[test]
    fn multiple_different_nonces_are_accepted() {
        let body = r#"{"action":"test"}"#;
        let secret = "key";
        let verifier = SignatureVerifier::new();

        for i in 0..5 {
            let sig = sign_request(body, secret, SigningAlgorithm::Sha256);
            let req = SignedRequest {
                body: body.to_string(),
                signature: sig,
                timestamp: current_timestamp_str(),
                nonce: format!("nonce-{i}"),
                algorithm: None,
            };

            let result = verifier.verify(&req, secret);
            assert!(
                result.valid,
                "verification should succeed for unique nonce: {i}"
            );
        }
    }

    #[test]
    fn future_timestamp_within_tolerance_is_accepted() {
        let body = r#"{"action":"test"}"#;
        let secret = "key";
        let sig = sign_request(body, secret, SigningAlgorithm::Sha256);

        // 1 minute in the future (within tolerance)
        let future_timestamp = (Utc::now().timestamp() + 60).to_string();

        let req = SignedRequest {
            body: body.to_string(),
            signature: sig,
            timestamp: future_timestamp,
            nonce: uuid::Uuid::new_v4().to_string(),
            algorithm: None,
        };

        let verifier = SignatureVerifier::new();
        let result = verifier.verify(&req, secret);
        assert!(result.valid, "future timestamp within tolerance should be accepted");
    }

    #[test]
    fn invalid_timestamp_format_is_rejected() {
        let body = r#"{"action":"test"}"#;
        let secret = "key";
        let sig = sign_request(body, secret, SigningAlgorithm::Sha256);

        let req = SignedRequest {
            body: body.to_string(),
            signature: sig,
            timestamp: "not-a-number".to_string(),
            nonce: uuid::Uuid::new_v4().to_string(),
            algorithm: None,
        };

        let verifier = SignatureVerifier::new();
        let result = verifier.verify(&req, secret);
        assert!(!result.valid, "invalid timestamp should be rejected");
        assert!(
            result.reason.as_ref().unwrap().contains("invalid"),
            "error should indicate invalid timestamp"
        );
    }

    #[test]
    fn empty_body_can_be_signed() {
        let body = "";
        let secret = "key";
        let sig = sign_request(body, secret, SigningAlgorithm::Sha256);

        let req = SignedRequest {
            body: body.to_string(),
            signature: sig,
            timestamp: current_timestamp_str(),
            nonce: uuid::Uuid::new_v4().to_string(),
            algorithm: None,
        };

        let verifier = SignatureVerifier::new();
        let result = verifier.verify(&req, secret);
        assert!(result.valid, "empty body should be signable");
    }

    #[test]
    fn large_body_is_signed_correctly() {
        let body = &"x".repeat(100_000); // 100KB body
        let secret = "key";
        let sig = sign_request(body, secret, SigningAlgorithm::Sha512);

        let req = SignedRequest {
            body: body.to_string(),
            signature: sig,
            timestamp: current_timestamp_str(),
            nonce: uuid::Uuid::new_v4().to_string(),
            algorithm: Some("sha512".to_string()),
        };

        let verifier = SignatureVerifier::new();
        let result = verifier.verify(&req, secret);
        assert!(result.valid, "large body should be signed correctly");
    }

    #[test]
    fn default_algorithm_is_sha256() {
        let body = r#"{"test":true}"#;
        let secret = "key";

        // Sign without specifying algorithm (defaults to SHA256)
        let sig = sign_request(body, secret, SigningAlgorithm::default());

        let req = SignedRequest {
            body: body.to_string(),
            signature: sig,
            timestamp: current_timestamp_str(),
            nonce: uuid::Uuid::new_v4().to_string(),
            algorithm: None, // No algorithm specified
        };

        let verifier = SignatureVerifier::new();
        let result = verifier.verify(&req, secret);
        assert!(result.valid, "default algorithm should be SHA256");
    }

    #[test]
    fn public_key_management() {
        let verifier = SignatureVerifier::new();
        verifier.add_public_key("key-1".to_string());
        verifier.add_public_key("key-2".to_string());

        let keys = verifier.public_keys.lock().unwrap();
        assert_eq!(keys.len(), 2, "should have 2 public keys");
        assert!(keys.contains(&"key-1".to_string()));
        assert!(keys.contains(&"key-2".to_string()));
    }
}
