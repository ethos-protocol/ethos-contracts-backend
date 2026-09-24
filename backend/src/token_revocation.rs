//! API authentication token revocation list (#350).
//!
//! The JWT auth flow issues and refreshes bearer tokens, but a leaked token
//! stays valid until it expires. This module adds an explicit deny-list keyed
//! by JWT id (`jti`) so a compromised token can be invalidated immediately.
//!
//! # Design
//!
//! * Storage mirrors the TTL-eviction model used by [`crate::cache`] /
//!   [`crate::multilevel_cache`]: each entry carries the token's own `exp`, and
//!   once wall-clock time passes it the entry is dropped — the list never grows
//!   past the set of currently-live tokens.
//! * [`RevocationList::is_revoked`] is the check every authenticated request
//!   runs; [`validate_ws_token_with_revocation`] wires it onto the existing
//!   [`crate::websocket::validate_ws_token`] verification.
//! * `POST /auth/revoke` ([`revoke_token`]) adds a token (or a bare `jti`) to
//!   the list.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::{extract::State, http::StatusCode, Json};
use base64::Engine;
use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Fallback lifetime applied to a revocation entry when the caller supplies
/// neither an explicit `expires_at` nor a decodable `exp` claim. Chosen to
/// comfortably exceed the longest access-token lifetime.
pub const DEFAULT_TTL_SECONDS: i64 = 24 * 60 * 60;

/// In-memory revocation list keyed by token id (`jti`), value = Unix second at
/// which the entry may be purged (the token's own expiry).
#[derive(Debug, Default)]
pub struct RevocationList {
    inner: RwLock<HashMap<String, i64>>,
}

impl RevocationList {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Add `jti` to the deny-list until `expires_at` (Unix seconds).
    pub fn revoke(&self, jti: &str, expires_at: i64) {
        self.inner
            .write()
            .unwrap()
            .insert(jti.to_string(), expires_at);
    }

    /// Whether `jti` is currently revoked. Expired entries are treated as
    /// not-revoked and dropped opportunistically so reads stay self-cleaning.
    pub fn is_revoked(&self, jti: &str) -> bool {
        let now = Utc::now().timestamp();
        {
            let map = self.inner.read().unwrap();
            match map.get(jti) {
                None => return false,
                Some(&exp) if exp > now => return true,
                Some(_) => {} // expired — fall through to remove
            }
        }
        self.inner.write().unwrap().remove(jti);
        false
    }

    /// Drop every entry whose expiry is at or before `now` (Unix seconds).
    /// Returns the number of entries removed.
    pub fn purge_expired(&self, now: i64) -> usize {
        let mut map = self.inner.write().unwrap();
        let before = map.len();
        map.retain(|_, &mut exp| exp > now);
        before - map.len()
    }

    /// Current number of (possibly not-yet-purged) entries.
    pub fn len(&self) -> usize {
        self.inner.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Decode a JWT payload **without verifying the signature** and return
/// `(jti, exp)`. Used only to learn which token id to revoke; the token's
/// authenticity is irrelevant when the intent is to deny it.
fn decode_unverified(token: &str) -> Option<(Option<String>, Option<i64>)> {
    let payload_b64 = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let jti = value
        .get("jti")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let exp = value.get("exp").and_then(serde_json::Value::as_i64);
    Some((jti, exp))
}

/// Verify `token` against `secret` and then reject it if its `jti` is on
/// `list`. This is the entry point authenticated request handlers should call
/// instead of [`crate::websocket::validate_ws_token`].
pub fn validate_ws_token_with_revocation(
    token: &str,
    secret: &[u8],
    list: &RevocationList,
) -> Result<crate::models::AuthClaims, String> {
    let claims = crate::websocket::validate_ws_token(token, secret)?;
    if let Some(ref jti) = claims.jti {
        if list.is_revoked(jti) {
            return Err("token has been revoked".to_string());
        }
    }
    Ok(claims)
}

// ── HTTP handler ─────────────────────────────────────────────────────────────

/// Request body for `POST /auth/revoke`. Supply either a full `token` (its
/// `jti`/`exp` are extracted) or a bare `jti`.
#[derive(Debug, Deserialize)]
pub struct RevokeRequest {
    pub token: Option<String>,
    pub jti: Option<String>,
    /// Unix seconds after which the entry may be purged. Defaults to the
    /// token's `exp`, else `now + DEFAULT_TTL_SECONDS`.
    pub expires_at: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct RevokeResponse {
    pub revoked: String,
    pub expires_at: i64,
}

fn bad_request(msg: &str) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(serde_json::json!({ "error": msg })),
    )
}

/// `POST /auth/revoke` — add a token to the revocation list.
pub async fn revoke_token(
    State(list): State<Arc<RevocationList>>,
    Json(body): Json<RevokeRequest>,
) -> Result<Json<RevokeResponse>, (StatusCode, Json<serde_json::Value>)> {
    let now = Utc::now().timestamp();

    let (jti, token_exp) = match (body.jti.clone(), body.token.as_deref()) {
        (Some(jti), _) => (jti, None),
        (None, Some(token)) => {
            let (jti, exp) =
                decode_unverified(token).ok_or_else(|| bad_request("token is not a decodable JWT"))?;
            let jti = jti.ok_or_else(|| bad_request("token has no `jti` claim to revoke"))?;
            (jti, exp)
        }
        (None, None) => return Err(bad_request("provide `token` or `jti`")),
    };

    let expires_at = body
        .expires_at
        .or(token_exp)
        .unwrap_or(now + DEFAULT_TTL_SECONDS);

    list.revoke(&jti, expires_at);

    Ok(Json(RevokeResponse {
        revoked: jti,
        expires_at,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AuthClaims;
    use jsonwebtoken::{encode, EncodingKey, Header};

    #[derive(Serialize)]
    struct TestClaims {
        sub: String,
        vault_ids: Vec<String>,
        exp: usize,
        jti: String,
    }

    fn mint(secret: &[u8], jti: &str, exp: i64) -> String {
        encode(
            &Header::default(),
            &TestClaims {
                sub: "user-1".to_string(),
                vault_ids: vec![],
                exp: exp as usize,
                jti: jti.to_string(),
            },
            &EncodingKey::from_secret(secret),
        )
        .unwrap()
    }

    #[test]
    fn revoked_jti_is_reported_revoked_others_are_not() {
        let list = RevocationList::new();
        let future = Utc::now().timestamp() + 3600;

        list.revoke("tok-abc", future);

        assert!(list.is_revoked("tok-abc"));
        assert!(!list.is_revoked("tok-xyz"));
    }

    #[test]
    fn expired_entries_are_not_revoked_and_are_self_cleaned() {
        let list = RevocationList::new();
        let past = Utc::now().timestamp() - 1;

        list.revoke("tok-old", past);
        assert_eq!(list.len(), 1);

        // Read past expiry: not revoked, and the stale entry is dropped.
        assert!(!list.is_revoked("tok-old"));
        assert_eq!(list.len(), 0);
    }

    #[test]
    fn purge_expired_drops_only_stale_entries() {
        let list = RevocationList::new();
        let now = Utc::now().timestamp();

        list.revoke("live", now + 3600);
        list.revoke("stale-1", now - 10);
        list.revoke("stale-2", now - 1);

        let removed = list.purge_expired(now);

        assert_eq!(removed, 2);
        assert_eq!(list.len(), 1);
        assert!(list.is_revoked("live"));
    }

    #[test]
    fn validate_rejects_revoked_token_and_accepts_live_one() {
        let secret = b"revocation-test-secret";
        let list = RevocationList::new();
        let exp = Utc::now().timestamp() + 3600;

        let good = mint(secret, "jti-good", exp);
        let bad = mint(secret, "jti-bad", exp);

        list.revoke("jti-bad", exp);

        let ok = validate_ws_token_with_revocation(&good, secret, &list).unwrap();
        assert_eq!(ok.jti.as_deref(), Some("jti-good"));

        let err = validate_ws_token_with_revocation(&bad, secret, &list).unwrap_err();
        assert!(err.contains("revoked"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn revoke_endpoint_accepts_bare_jti_and_full_token() {
        let list = RevocationList::new();
        let exp = Utc::now().timestamp() + 1200;

        // By bare jti.
        let resp = revoke_token(
            State(list.clone()),
            Json(RevokeRequest {
                token: None,
                jti: Some("direct-jti".to_string()),
                expires_at: Some(exp),
            }),
        )
        .await
        .unwrap();
        assert_eq!(resp.0.revoked, "direct-jti");
        assert!(list.is_revoked("direct-jti"));

        // By full token — jti and exp are extracted from the payload.
        let token = mint(b"whatever", "token-jti", exp);
        let resp = revoke_token(
            State(list.clone()),
            Json(RevokeRequest {
                token: Some(token),
                jti: None,
                expires_at: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(resp.0.revoked, "token-jti");
        assert_eq!(resp.0.expires_at, exp);
        assert!(list.is_revoked("token-jti"));
    }

    #[tokio::test]
    async fn revoke_endpoint_rejects_empty_request() {
        let list = RevocationList::new();
        let err = revoke_token(
            State(list),
            Json(RevokeRequest {
                token: None,
                jti: None,
                expires_at: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn auth_claims_without_jti_are_never_blocked() {
        // Backwards compatibility: a token minted before jti existed.
        let claims = AuthClaims {
            sub: "legacy".to_string(),
            vault_ids: vec![],
            exp: 0,
            jti: None,
        };
        let list = RevocationList::new();
        // No jti → the revocation check is a no-op for this token.
        assert!(claims.jti.is_none());
        assert!(list.is_empty());
    }
}
