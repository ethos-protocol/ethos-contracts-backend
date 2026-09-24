//! Webhook system for event push notifications (#65).
//!
//! Clients register a URL (and optional secret) to receive HTTP POST payloads
//! whenever a vault event occurs.  The delivery engine retries failed
//! deliveries with exponential back-off.
//!
//! # Architecture
//!
//! ```text
//! POST /webhooks            → register_webhook  (stores WebhookRegistration)
//! GET  /webhooks            → list_webhooks
//! DELETE /webhooks/:id      → delete_webhook
//! Internal: deliver_event() → called by handlers when events are emitted
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Data types ────────────────────────────────────────────────────────────────

/// Events that can trigger webhook delivery.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebhookEventType {
    VaultCreated,
    VaultCheckedIn,
    VaultReleased,
    VaultDeposit,
    VaultWithdrawal,
    BeneficiaryUpdated,
    VaultPaused,
    VaultResumed,
}

/// A persisted webhook subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookRegistration {
    pub id: String,
    /// The HTTPS (or HTTP for local dev) URL to POST events to.
    pub url: String,
    /// Optional vault filter; `None` = receive events for all vaults.
    pub vault_id: Option<String>,
    /// Event types to deliver; empty = all event types.
    pub event_types: Vec<WebhookEventType>,
    /// HMAC secret used to sign payloads (optional).
    pub secret: Option<String>,
    /// Algorithm used for HMAC signing. Defaults to SHA-256.
    #[serde(default)]
    pub algorithm: SignatureAlgorithm,
    pub created_at: DateTime<Utc>,
    pub active: bool,
}

/// Request body for `POST /webhooks`.
#[derive(Debug, Deserialize)]
pub struct RegisterWebhookRequest {
    pub url: String,
    pub vault_id: Option<String>,
    #[serde(default)]
    pub event_types: Vec<WebhookEventType>,
    pub secret: Option<String>,
    /// Signing algorithm to use when a `secret` is provided. Defaults to SHA-256.
    #[serde(default)]
    pub algorithm: SignatureAlgorithm,
}

/// Payload sent to the registered URL on each event.
#[derive(Debug, Clone, Serialize)]
pub struct WebhookPayload {
    pub id: String,
    pub event_type: WebhookEventType,
    pub vault_id: String,
    pub timestamp: DateTime<Utc>,
    pub data: serde_json::Value,
}

// ── In-memory store ───────────────────────────────────────────────────────────

pub type WebhookStore = Arc<Mutex<HashMap<String, WebhookRegistration>>>;

pub fn create_webhook_store() -> WebhookStore {
    Arc::new(Mutex::new(HashMap::new()))
}

// ── Axum handler state ────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct WebhookState {
    pub store: WebhookStore,
    pub http_client: Client,
    /// Dead-letter queue that exhausted deliveries are routed into.
    pub dlq_state: Arc<crate::dlq::DlqState>,
    /// Health-aware routing weights, keyed by webhook URL.
    pub health_routing_state: Arc<crate::health_routing::HealthRoutingState>,
}

impl WebhookState {
    pub fn new() -> Self {
        Self::with_reliability_state(
            Arc::new(crate::dlq::DlqState::new()),
            Arc::new(crate::health_routing::HealthRoutingState::new()),
        )
    }

    pub fn with_reliability_state(
        dlq_state: Arc<crate::dlq::DlqState>,
        health_routing_state: Arc<crate::health_routing::HealthRoutingState>,
    ) -> Self {
        Self {
            store: create_webhook_store(),
            http_client: Client::new(),
            dlq_state,
            health_routing_state,
        }
    }
}

impl Default for WebhookState {
    fn default() -> Self {
        Self::new()
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// `POST /webhooks` — register a new webhook endpoint.
pub async fn register_webhook(
    State(state): State<Arc<WebhookState>>,
    Json(body): Json<RegisterWebhookRequest>,
) -> Result<(StatusCode, Json<WebhookRegistration>), (StatusCode, Json<serde_json::Value>)> {
    if body.url.is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "url must not be empty" })),
        ));
    }

    let registration = WebhookRegistration {
        id: Uuid::new_v4().to_string(),
        url: body.url,
        vault_id: body.vault_id,
        event_types: body.event_types,
        // #351: every registration is given a signing secret so that *every*
        // outbound payload is HMAC-signed, even when the caller did not supply
        // one. The generated secret is returned in the response body once.
        secret: Some(body.secret.filter(|s| !s.is_empty()).unwrap_or_else(generate_secret)),
        algorithm: body.algorithm,
        created_at: Utc::now(),
        active: true,
    };

    let mut store = state.store.lock().unwrap();
    store.insert(registration.id.clone(), registration.clone());

    Ok((StatusCode::CREATED, Json(registration)))
}

/// `GET /webhooks` — list all registered webhooks.
pub async fn list_webhooks(
    State(state): State<Arc<WebhookState>>,
) -> Json<Vec<WebhookRegistration>> {
    let store = state.store.lock().unwrap();
    let webhooks: Vec<WebhookRegistration> = store.values().cloned().collect();
    Json(webhooks)
}

/// `DELETE /webhooks/:id` — deactivate a webhook.
pub async fn delete_webhook(
    State(state): State<Arc<WebhookState>>,
    Path(id): Path<String>,
) -> StatusCode {
    let mut store = state.store.lock().unwrap();
    if let Some(wh) = store.get_mut(&id) {
        wh.active = false;
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

// ── Event delivery ────────────────────────────────────────────────────────────

/// Deliver `payload` to all matching active webhook registrations.
///
/// Each delivery is attempted asynchronously.  A failed request is retried up
/// to `MAX_RETRIES` times with exponential back-off before being dropped.
///
/// This function is fire-and-forget: it spawns a Tokio task per matching
/// webhook so that the calling handler is never blocked.
pub fn deliver_event(
    state: Arc<WebhookState>,
    event_type: WebhookEventType,
    vault_id: String,
    data: serde_json::Value,
) {
    let payload = WebhookPayload {
        id: Uuid::new_v4().to_string(),
        event_type: event_type.clone(),
        vault_id: vault_id.clone(),
        timestamp: Utc::now(),
        data,
    };

    let registrations: Vec<WebhookRegistration> = {
        let store = state.store.lock().unwrap();
        store
            .values()
            .filter(|wh| {
                if !wh.active {
                    return false;
                }
                // Filter by vault_id if specified.
                if let Some(ref wh_vault) = wh.vault_id {
                    if *wh_vault != vault_id {
                        return false;
                    }
                }
                // Filter by event type if a non-empty list is configured.
                if !wh.event_types.is_empty() && !wh.event_types.contains(&event_type) {
                    return false;
                }
                true
            })
            .cloned()
            .collect()
    };

    for registration in registrations {
        // Health-aware routing (#4): skip endpoints that have been failing
        // consistently rather than continuing to hammer them.
        if !crate::health_routing::should_route(&state.health_routing_state, &registration.url) {
            tracing::warn!(
                webhook_id = %registration.id,
                url = %registration.url,
                "skipping delivery — endpoint is routed as unhealthy"
            );
            continue;
        }

        let client = state.http_client.clone();
        let payload_clone = payload.clone();
        let state_clone = Arc::clone(&state);

        tokio::spawn(async move {
            attempt_delivery(&client, &registration, &payload_clone, &state_clone).await;
        });
    }
}

// ── Internal delivery with retry ──────────────────────────────────────────────

const MAX_RETRIES: u32 = 4;
const BASE_DELAY_MS: u64 = 250;

async fn attempt_delivery(
    client: &Client,
    registration: &WebhookRegistration,
    payload: &WebhookPayload,
    state: &WebhookState,
) {
    let body = match serde_json::to_string(payload) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(webhook_id = %registration.id, "Failed to serialize webhook payload: {e}");
            return;
        }
    };

    for attempt in 0..=MAX_RETRIES {
        if attempt > 0 {
            let delay_ms = BASE_DELAY_MS * (1 << (attempt - 1)); // 250, 500, 1000, 2000 ms
            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
        }

        let mut req = client
            .post(&registration.url)
            .header("Content-Type", "application/json")
            .header("X-Ethos-Event", format!("{:?}", payload.event_type))
            .header("X-Ethos-Delivery", &payload.id)
            .body(body.clone());

        // Add HMAC signature + timestamp headers. Every registration carries a
        // secret (see `register_webhook`), so this branch is always taken for
        // registrations created through the API; the `if let` only guards
        // hand-constructed registrations used in tests.
        if let Some(ref secret) = registration.secret {
            let (signature, timestamp) =
                build_signature_headers(&body, secret, registration.algorithm);
            req = req
                .header("X-Ethos-Signature", &signature)
                .header("X-Ethos-Timestamp", timestamp)
                // #351: standard header carrying the canonical HMAC-SHA256
                // digest ("sha256=<hex>"), independent of the registration's
                // configured algorithm, for receivers that expect it.
                .header("X-Signature-256", standard_signature_256(&body, secret));
        }

        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                tracing::debug!(
                    webhook_id = %registration.id,
                    status = %resp.status(),
                    attempt,
                    "Webhook delivery succeeded"
                );
                crate::health_routing::record_outcome(
                    &state.health_routing_state,
                    &registration.url,
                    true,
                );
                return;
            }
            Ok(resp) => {
                tracing::warn!(
                    webhook_id = %registration.id,
                    status = %resp.status(),
                    attempt,
                    "Webhook delivery failed — will retry"
                );
                crate::health_routing::record_outcome(
                    &state.health_routing_state,
                    &registration.url,
                    false,
                );
            }
            Err(e) => {
                tracing::warn!(
                    webhook_id = %registration.id,
                    attempt,
                    "Webhook delivery error: {e} — will retry"
                );
                crate::health_routing::record_outcome(
                    &state.health_routing_state,
                    &registration.url,
                    false,
                );
            }
        }
    }

    tracing::error!(
        webhook_id = %registration.id,
        "Webhook delivery exhausted all retries — dropping"
    );

    // Dead-letter the payload (#2) instead of discarding it so it can be
    // inspected and replayed once the endpoint recovers.
    crate::dlq::route_to_dlq(
        &state.dlq_state,
        format!("webhook:{}", registration.id),
        Some(registration.url.clone()),
        serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
        "exhausted all delivery retries",
        MAX_RETRIES + 1,
    );
}

/// Compute HMAC-SHA256 hex signature over `body` using `secret`.
///
/// The signature is placed in the `X-Ethos-Signature: sha256=<hex>` header so
/// that receivers can verify authenticity using the shared secret.
fn sign_payload(body: &str, secret: &str) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    type HmacSha256 = Hmac<Sha256>;

    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(body.as_bytes());
    let result = mac.finalize().into_bytes();

    result.iter().map(|b| format!("{b:02x}")).collect()
}

// ── Signature algorithms (#149) ───────────────────────────────────────────────

/// HMAC algorithm used for webhook payload signing and verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SignatureAlgorithm {
    /// HMAC-SHA256 (default, most widely supported).
    #[default]
    Sha256,
    /// HMAC-SHA1 (legacy compatibility — prefer SHA-256 for new integrations).
    Sha1,
    /// HMAC-SHA512 (highest security).
    Sha512,
}

impl SignatureAlgorithm {
    /// Returns the prefix used in the `X-Ethos-Signature` header value,
    /// e.g. `"sha256"`.
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Sha1 => "sha1",
            Self::Sha512 => "sha512",
        }
    }
}

// ── Multi-algorithm signing ───────────────────────────────────────────────────

/// Compute an HMAC hex-digest over `body` using the requested `algorithm`.
///
/// Returns `"<algorithm>=<hex-digest>"`, matching the format expected in the
/// `X-Ethos-Signature` header.
pub fn sign_payload_with_algorithm(
    body: &str,
    secret: &str,
    algorithm: SignatureAlgorithm,
) -> String {
    use hmac::{Hmac, Mac};
    use sha1::Sha1 as HmacSha1Inner;
    use sha2::{Sha256, Sha512};

    let hex_digest = match algorithm {
        SignatureAlgorithm::Sha256 => {
            type H = Hmac<Sha256>;
            let mut mac = H::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key");
            mac.update(body.as_bytes());
            mac.finalize()
                .into_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        }
        SignatureAlgorithm::Sha1 => {
            type H = Hmac<HmacSha1Inner>;
            let mut mac = H::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key");
            mac.update(body.as_bytes());
            mac.finalize()
                .into_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        }
        SignatureAlgorithm::Sha512 => {
            type H = Hmac<Sha512>;
            let mut mac = H::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key");
            mac.update(body.as_bytes());
            mac.finalize()
                .into_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        }
    };

    format!("{}={}", algorithm.prefix(), hex_digest)
}

// ── Timestamp validation (#149) ───────────────────────────────────────────────

/// Maximum age (seconds) of a webhook timestamp before it is rejected.
/// Protects against replay attacks.
pub const TIMESTAMP_TOLERANCE_SECS: i64 = 300;

/// Validates that `timestamp` is within [`TIMESTAMP_TOLERANCE_SECS`] of now.
///
/// Returns `Ok(())` when the timestamp is acceptable.
/// Returns `Err(reason)` when it is absent, unparseable, or too old/future.
pub fn validate_timestamp(timestamp: Option<&str>) -> Result<(), String> {
    let ts_str = timestamp.ok_or_else(|| "missing X-Ethos-Timestamp header".to_string())?;

    let ts_secs: i64 = ts_str
        .parse()
        .map_err(|_| format!("unparseable timestamp: {ts_str}"))?;

    let now_secs = Utc::now().timestamp();
    let diff = (now_secs - ts_secs).abs();

    if diff > TIMESTAMP_TOLERANCE_SECS {
        return Err(format!(
            "timestamp out of tolerance: |now({now_secs}) - ts({ts_secs})| = {diff}s > {TIMESTAMP_TOLERANCE_SECS}s"
        ));
    }

    Ok(())
}

// ── Signature verification (#149) ────────────────────────────────────────────

/// The result of verifying an incoming webhook request.
#[derive(Debug, Serialize)]
pub struct VerificationResult {
    pub valid: bool,
    /// Algorithm detected from the `X-Ethos-Signature` header prefix.
    pub algorithm: Option<String>,
    /// Reason for failure when `valid` is false.
    pub reason: Option<String>,
}

/// Verify an incoming webhook request's signature and timestamp.
///
/// # Parameters
/// - `body`: the raw request body bytes as a UTF-8 string.
/// - `secret`: the shared HMAC secret configured for this webhook.
/// - `signature_header`: value of the `X-Ethos-Signature` header
///   (format: `"<algorithm>=<hex-digest>"`).
/// - `timestamp_header`: optional value of the `X-Ethos-Timestamp` header
///   (Unix seconds as a decimal string).
///
/// The function performs constant-time comparison to prevent timing attacks.
pub fn verify_webhook_signature(
    body: &str,
    secret: &str,
    signature_header: Option<&str>,
    timestamp_header: Option<&str>,
) -> VerificationResult {
    // 1. Validate timestamp first (cheapest check).
    if let Err(reason) = validate_timestamp(timestamp_header) {
        return VerificationResult {
            valid: false,
            algorithm: None,
            reason: Some(reason),
        };
    }

    // 2. Parse the signature header.
    let sig_value = match signature_header {
        Some(v) => v,
        None => {
            return VerificationResult {
                valid: false,
                algorithm: None,
                reason: Some("missing X-Ethos-Signature header".into()),
            }
        }
    };

    let (prefix, received_hex) = match sig_value.split_once('=') {
        Some(parts) => parts,
        None => {
            return VerificationResult {
                valid: false,
                algorithm: None,
                reason: Some("malformed signature header (expected '<alg>=<hex>')".into()),
            }
        }
    };

    let algorithm = match prefix {
        "sha256" => SignatureAlgorithm::Sha256,
        "sha1" => SignatureAlgorithm::Sha1,
        "sha512" => SignatureAlgorithm::Sha512,
        other => {
            return VerificationResult {
                valid: false,
                algorithm: Some(other.to_string()),
                reason: Some(format!("unsupported algorithm: {other}")),
            }
        }
    };

    // 3. Compute expected signature.
    let expected = sign_payload_with_algorithm(body, secret, algorithm);
    let expected_hex = expected.split_once('=').map(|(_, h)| h).unwrap_or("");

    // 4. Constant-time comparison.
    let valid = constant_time_eq(received_hex.as_bytes(), expected_hex.as_bytes());

    VerificationResult {
        valid,
        algorithm: Some(prefix.to_string()),
        reason: if valid {
            None
        } else {
            Some("signature mismatch".into())
        },
    }
}

/// Constant-time byte-slice comparison (prevents timing side-channels).
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

// ── Verification request body ─────────────────────────────────────────────────

/// Request body for the `POST /webhooks/verify` endpoint.
#[derive(Debug, Deserialize)]
pub struct VerifyWebhookRequest {
    /// Raw payload body that was received.
    pub body: String,
    /// Shared secret to verify against.
    pub secret: String,
    /// Value of the `X-Ethos-Signature` header.
    pub signature: String,
    /// Value of the `X-Ethos-Timestamp` header (Unix seconds).
    pub timestamp: Option<String>,
}

/// `POST /webhooks/verify` — verify a received webhook signature.
///
/// Consumers can call this endpoint to check whether an incoming webhook
/// request is authentic before processing it.
pub async fn verify_webhook(
    Json(body): Json<VerifyWebhookRequest>,
) -> (StatusCode, Json<VerificationResult>) {
    let result = verify_webhook_signature(
        &body.body,
        &body.secret,
        Some(&body.signature),
        body.timestamp.as_deref(),
    );

    let status = if result.valid {
        StatusCode::OK
    } else {
        StatusCode::UNAUTHORIZED
    };

    (status, Json(result))
}

// ── Update `attempt_delivery` to use multi-algorithm signing ─────────────────
// Note: `attempt_delivery` above calls the old `sign_payload` helper directly.
// The following public re-export lets callers use the new versioned helper.
// The internal `sign_payload` is kept for backwards compat with existing tests.

/// Build the `X-Ethos-Signature` and `X-Ethos-Timestamp` headers for a
/// webhook delivery.  Call this from `attempt_delivery` when a secret is set.
pub fn build_signature_headers(
    body: &str,
    secret: &str,
    algorithm: SignatureAlgorithm,
) -> (String, String) {
    let signature = sign_payload_with_algorithm(body, secret, algorithm);
    let timestamp = Utc::now().timestamp().to_string();
    (signature, timestamp)
}

/// Canonical value for the standard `X-Signature-256` header: the HMAC-SHA256
/// digest of `body` under `secret`, formatted as `"sha256=<hex>"`.
///
/// Always SHA-256 regardless of the registration's configured algorithm, so
/// receivers that only understand `X-Signature-256` have a stable contract.
pub fn standard_signature_256(body: &str, secret: &str) -> String {
    sign_payload_with_algorithm(body, secret, SignatureAlgorithm::Sha256)
}

/// Generate an opaque, high-entropy webhook signing secret.
fn generate_secret() -> String {
    format!(
        "whsec_{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

/// `POST /webhooks/:id/rotate-secret` — rotate a registration's signing secret.
///
/// Subsequent deliveries are signed with the new secret; signatures produced
/// under the old secret no longer verify. The new secret is returned once.
pub async fn rotate_webhook_secret(
    State(state): State<Arc<WebhookState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut store = state.store.lock().unwrap();
    match store.get_mut(&id) {
        Some(wh) => {
            let new_secret = generate_secret();
            wh.secret = Some(new_secret.clone());
            Ok(Json(serde_json::json!({
                "id": id,
                "secret": new_secret,
            })))
        }
        None => Err(StatusCode::NOT_FOUND),
    }
}

// ── Tests (#149) ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ts_now() -> String {
        Utc::now().timestamp().to_string()
    }

    fn sample_payload(id: &str) -> WebhookPayload {
        WebhookPayload {
            id: id.to_string(),
            event_type: WebhookEventType::VaultReleased,
            vault_id: "vault-1".to_string(),
            timestamp: Utc::now(),
            data: serde_json::json!({ "ok": true }),
        }
    }

    // ── #351: outbound signing guarantees ────────────────────────────────────

    #[tokio::test]
    async fn test_every_registration_is_assigned_a_signing_secret() {
        let state = Arc::new(WebhookState::new());
        let req = RegisterWebhookRequest {
            url: "https://example.test/hook".to_string(),
            vault_id: None,
            event_types: vec![],
            secret: None,
            algorithm: SignatureAlgorithm::Sha256,
        };

        let (status, Json(reg)) = register_webhook(State(state), Json(req)).await.unwrap();

        assert_eq!(status, StatusCode::CREATED);
        let secret = reg.secret.expect("registration must carry a signing secret");
        assert!(secret.starts_with("whsec_"), "unexpected secret: {secret}");
        assert!(secret.len() > 16);
    }

    #[test]
    fn test_x_signature_256_is_hmac_sha256_of_body() {
        let body = r#"{"event":"vault_released","amount":42}"#;
        let secret = "whsec_deadbeef";

        let header = standard_signature_256(body, secret);

        // Canonical SHA-256 digest, independent of any other configured algorithm.
        assert_eq!(
            header,
            sign_payload_with_algorithm(body, secret, SignatureAlgorithm::Sha256)
        );
        assert!(header.starts_with("sha256="));

        let ts = ts_now();
        let result = verify_webhook_signature(body, secret, Some(&header), Some(&ts));
        assert!(result.valid, "{:?}", result.reason);
    }

    #[tokio::test]
    async fn test_secret_rotation_invalidates_old_signatures() {
        let state = Arc::new(WebhookState::new());
        let req = RegisterWebhookRequest {
            url: "https://example.test/hook".to_string(),
            vault_id: None,
            event_types: vec![],
            secret: Some("original-secret".to_string()),
            algorithm: SignatureAlgorithm::Sha256,
        };
        let (_, Json(reg)) = register_webhook(State(state.clone()), Json(req)).await.unwrap();
        let body = r#"{"event":"vault_created"}"#;
        let old_sig = standard_signature_256(body, reg.secret.as_deref().unwrap());

        let Json(rot) = rotate_webhook_secret(State(state.clone()), Path(reg.id.clone()))
            .await
            .unwrap();
        let new_secret = rot["secret"].as_str().unwrap().to_string();
        assert_ne!(new_secret, "original-secret");

        let stored = state.store.lock().unwrap().get(&reg.id).unwrap().clone();
        assert_eq!(stored.secret.as_deref(), Some(new_secret.as_str()));

        let new_sig = standard_signature_256(body, &new_secret);
        assert_ne!(old_sig, new_sig, "rotation must change the signature");

        let ts = ts_now();
        assert!(
            !verify_webhook_signature(body, &new_secret, Some(&old_sig), Some(&ts)).valid,
            "signature under the retired secret must no longer verify"
        );
        assert!(verify_webhook_signature(body, &new_secret, Some(&new_sig), Some(&ts)).valid);
    }

    #[tokio::test]
    async fn test_delivery_carries_x_signature_256_header() {
        let mut server = mockito::Server::new_async().await;
        let hook = server
            .mock("POST", "/hook")
            .match_header(
                "x-signature-256",
                mockito::Matcher::Regex("^sha256=[0-9a-f]{64}$".to_string()),
            )
            .with_status(200)
            .expect(1)
            .create_async()
            .await;

        let state = WebhookState::new();
        let registration = WebhookRegistration {
            id: "wh-signed".to_string(),
            url: format!("{}/hook", server.url()),
            vault_id: None,
            event_types: vec![],
            // A non-256 algorithm configured: X-Signature-256 must still be SHA-256.
            secret: Some("delivery-secret".to_string()),
            algorithm: SignatureAlgorithm::Sha512,
            created_at: Utc::now(),
            active: true,
        };

        attempt_delivery(
            &state.http_client,
            &registration,
            &sample_payload("d1"),
            &state,
        )
        .await;

        hook.assert_async().await;
        assert_eq!(state.dlq_state.store.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_receiver_rejecting_unsigned_retry_fails_delivery() {
        let mut server = mockito::Server::new_async().await;
        // Signed requests are accepted…
        let signed = server
            .mock("POST", "/hook")
            .match_header("x-signature-256", mockito::Matcher::Any)
            .with_status(200)
            .expect(0)
            .create_async()
            .await;
        // …unsigned requests are rejected on every attempt.
        let unsigned = server
            .mock("POST", "/hook")
            .with_status(401)
            .expect_at_least(2)
            .create_async()
            .await;

        let state = WebhookState::new();
        let registration = WebhookRegistration {
            id: "wh-unsigned".to_string(),
            url: format!("{}/hook", server.url()),
            vault_id: None,
            event_types: vec![],
            secret: None, // hand-built registration: payloads go out unsigned
            algorithm: SignatureAlgorithm::Sha256,
            created_at: Utc::now(),
            active: true,
        };

        attempt_delivery(
            &state.http_client,
            &registration,
            &sample_payload("d2"),
            &state,
        )
        .await;

        signed.assert_async().await; // never hit
        unsigned.assert_async().await; // hit on every retry
        assert_eq!(
            state.dlq_state.store.lock().unwrap().len(),
            1,
            "an unsigned delivery the receiver rejects must be dead-lettered"
        );
    }

    #[test]
    fn test_sign_and_verify_sha256() {
        let body = r#"{"event":"vault_created"}"#;
        let secret = "test-secret";
        let sig = sign_payload_with_algorithm(body, secret, SignatureAlgorithm::Sha256);
        let ts = ts_now();

        let result = verify_webhook_signature(body, secret, Some(&sig), Some(&ts));
        assert!(result.valid, "expected valid: {:?}", result.reason);
        assert_eq!(result.algorithm.as_deref(), Some("sha256"));
    }

    #[test]
    fn test_sign_and_verify_sha1() {
        let body = r#"{"event":"vault_checked_in"}"#;
        let secret = "another-secret";
        let sig = sign_payload_with_algorithm(body, secret, SignatureAlgorithm::Sha1);
        let ts = ts_now();

        let result = verify_webhook_signature(body, secret, Some(&sig), Some(&ts));
        assert!(result.valid, "expected valid: {:?}", result.reason);
        assert_eq!(result.algorithm.as_deref(), Some("sha1"));
    }

    #[test]
    fn test_sign_and_verify_sha512() {
        let body = r#"{"event":"vault_released"}"#;
        let secret = "s3cr3t";
        let sig = sign_payload_with_algorithm(body, secret, SignatureAlgorithm::Sha512);
        let ts = ts_now();

        let result = verify_webhook_signature(body, secret, Some(&sig), Some(&ts));
        assert!(result.valid, "expected valid: {:?}", result.reason);
        assert_eq!(result.algorithm.as_deref(), Some("sha512"));
    }

    #[test]
    fn test_wrong_secret_fails() {
        let body = r#"{"event":"vault_created"}"#;
        let sig = sign_payload_with_algorithm(body, "secret-a", SignatureAlgorithm::Sha256);
        let ts = ts_now();

        let result = verify_webhook_signature(body, "secret-b", Some(&sig), Some(&ts));
        assert!(!result.valid);
        assert_eq!(result.reason.as_deref(), Some("signature mismatch"));
    }

    #[test]
    fn test_tampered_body_fails() {
        let original = r#"{"amount":100}"#;
        let tampered = r#"{"amount":999}"#;
        let secret = "key";
        let sig = sign_payload_with_algorithm(original, secret, SignatureAlgorithm::Sha256);
        let ts = ts_now();

        let result = verify_webhook_signature(tampered, secret, Some(&sig), Some(&ts));
        assert!(!result.valid);
    }

    #[test]
    fn test_missing_timestamp_fails() {
        let body = "hello";
        let secret = "key";
        let sig = sign_payload_with_algorithm(body, secret, SignatureAlgorithm::Sha256);

        let result = verify_webhook_signature(body, secret, Some(&sig), None);
        assert!(!result.valid);
        assert!(result.reason.unwrap().contains("missing"));
    }

    #[test]
    fn test_stale_timestamp_fails() {
        let body = "hello";
        let secret = "key";
        let sig = sign_payload_with_algorithm(body, secret, SignatureAlgorithm::Sha256);
        // Timestamp from 10 minutes ago.
        let old_ts = (Utc::now().timestamp() - 600).to_string();

        let result = verify_webhook_signature(body, secret, Some(&sig), Some(&old_ts));
        assert!(!result.valid);
        assert!(result.reason.unwrap().contains("out of tolerance"));
    }

    #[test]
    fn test_missing_signature_fails() {
        let ts = ts_now();
        let result = verify_webhook_signature("body", "secret", None, Some(&ts));
        assert!(!result.valid);
        assert!(result.reason.unwrap().contains("missing X-Ethos-Signature"));
    }

    #[test]
    fn test_unsupported_algorithm_fails() {
        let ts = ts_now();
        let result = verify_webhook_signature("body", "secret", Some("md5=deadbeef"), Some(&ts));
        assert!(!result.valid);
        assert!(result.reason.unwrap().contains("unsupported algorithm"));
    }

    #[test]
    fn test_malformed_signature_header_fails() {
        let ts = ts_now();
        let result = verify_webhook_signature("body", "secret", Some("nodivider"), Some(&ts));
        assert!(!result.valid);
        assert!(result.reason.unwrap().contains("malformed"));
    }

    #[test]
    fn test_validate_timestamp_ok() {
        let ts = Utc::now().timestamp().to_string();
        assert!(validate_timestamp(Some(&ts)).is_ok());
    }

    #[test]
    fn test_validate_timestamp_future_within_tolerance() {
        // 1 minute in the future — still acceptable.
        let ts = (Utc::now().timestamp() + 60).to_string();
        assert!(validate_timestamp(Some(&ts)).is_ok());
    }

    #[test]
    fn test_validate_timestamp_too_old() {
        let ts = (Utc::now().timestamp() - 600).to_string();
        let err = validate_timestamp(Some(&ts)).unwrap_err();
        assert!(err.contains("out of tolerance"));
    }
}
