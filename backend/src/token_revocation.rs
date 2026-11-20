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
//! * Revocations are persisted to a JSON file (when a path is configured) so
//!   they survive a restart or redeploy; the file is loaded at startup and
//!   expired entries are pruned on load and on every write.
//! * [`RevocationList::is_revoked`] is the check every authenticated request
//!   runs; [`validate_ws_token_with_revocation`] wires it onto the existing
//!   [`crate::websocket::validate_ws_token`] verification.
//! * Admin-only `POST /auth/revoke` ([`revoke_token`]) adds a token (or a bare
//!   `jti`) to the list.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use base64::Engine;
use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Fallback lifetime applied to a revocation entry when the caller supplies
/// neither an explicit `expires_at` nor a decodable `exp` claim. Chosen to
/// comfortably exceed the longest access-token lifetime.
pub const DEFAULT_TTL_SECONDS: i64 = 24 * 60 * 60;

/// Revocation list keyed by token id (`jti`), value = Unix second at which the
/// entry may be purged (the token's own expiry). Backed by an optional JSON
/// file so revocations survive process restarts.
#[derive(Debug, Default)]
pub struct RevocationList {
    inner: RwLock<HashMap<String, i64>>,
    /// When set, the map is persisted here on every mutation.
    path: Option<PathBuf>,
}

impl RevocationList {
    /// In-memory-only list (no persistence).
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Build a list persisted at `path`, loading any existing revocations and
    /// pruning entries whose tokens have already expired.
    pub fn with_persistence(path: impl Into<PathBuf>) -> Arc<Self> {
        let path = path.into();
        let now = Utc::now().timestamp();
        let mut map = load_from_disk(&path).unwrap_or_default();
        map.retain(|_, &mut exp| exp > now);
        let list = Self {
            inner: RwLock::new(map),
            path: Some(path),
        };
        // Persist the pruned set so stale entries don't linger on disk.
        list.flush();
        Arc::new(list)
    }

    /// Add `jti` to the deny-list until `expires_at` (Unix seconds).
    pub fn revoke(&self, jti: &str, expires_at: i64) {
        self.inner
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(jti.to_string(), expires_at);
        self.flush();
    }

    /// Whether `jti` is currently revoked. Expired entries are treated as
    /// not-revoked and dropped opportunistically so reads stay self-cleaning.
    pub fn is_revoked(&self, jti: &str) -> bool {
        let now = Utc::now().timestamp();
        {
            let map = self.inner.read().unwrap_or_else(|p| p.into_inner());
            match map.get(jti) {
                None => return false,
                Some(&exp) if exp > now => return true,
                Some(_) => {} // expired — fall through to remove
            }
        }
        self.inner.write().unwrap_or_else(|p| p.into_inner()).remove(jti);
        self.flush();
        false
    }

    /// Drop every entry whose expiry is at or before `now` (Unix seconds).
    /// Returns the number of entries removed.
    pub fn purge_expired(&self, now: i64) -> usize {
        let removed = {
            let mut map = self.inner.write().unwrap_or_else(|p| p.into_inner());
            let before = map.len();
            map.retain(|_, &mut exp| exp > now);
            before - map.len()
        };
        if removed > 0 {
            self.flush();
        }
        removed
    }

    /// Current number of (possibly not-yet-purged) entries.
    pub fn len(&self) -> usize {
        self.inner.read().unwrap_or_else(|p| p.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Write the current map to disk (no-op when no path is configured).
    /// Failures are logged and swallowed so a disk hiccup never breaks auth.
    fn flush(&self) {
        let Some(ref path) = self.path else {
            return;
        };
        let snapshot = self
            .inner
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Err(error) = save_to_disk(path, &snapshot) {
            eprintln!("failed to persist token revocation list to {path:?}: {error}");
        }
    }
}

/// Read a persisted revocation map from `path`. A missing file yields an empty
/// map; a corrupt file is treated as empty (fail-closed on the safe side is
/// not possible here, so we log and start fresh).
fn load_from_disk(path: &Path) -> Option<HashMap<String, i64>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(HashMap::new()),
        Err(error) => {
            eprintln!("failed to read token revocation list {path:?}: {error}");
            return None;
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(map) => Some(map),
        Err(error) => {
            eprintln!("failed to parse token revocation list {path:?}: {error}");
            None
        }
    }
}

/// Atomically write `map` to `path` (temp file + rename).
fn save_to_disk(path: &Path, map: &HashMap<String, i64>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let bytes = serde_json::to_vec(map)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, path)
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
#[serde(deny_unknown_fields)]
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

/// Admin-only `POST /auth/revoke` — add a token to the revocation list.
pub async fn revoke_token(
    State(list): State<Arc<RevocationList>>,
    headers: HeaderMap,
    Json(body): Json<RevokeRequest>,
) -> Result<Json<RevokeResponse>, (StatusCode, Json<serde_json::Value>)> {
    if let Err(error) = crate::audit::authorize_admin(&headers) {
        return Err((
            error.status(),
            Json(serde_json::json!({ "error": error.message })),
        ));
    }

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
    fn purge_expired_removes_only_stale_entries() {
        let list = RevocationList::new();
        let now = Utc::now().timestamp();

        list.revoke("tok-live", now + 3600);
        list.revoke("tok-dead", now - 1);

        assert_eq!(list.purge_expired(now), 1);
        assert_eq!(list.len(), 1);
        assert!(list.is_revoked("tok-live"));
    }

    #[test]
    fn revocations_survive_reload_and_expired_are_pruned() {
        let dir = std::env::temp_dir().join(format!(
            "revocation-test-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let path = dir.join("revocations.json");
        let now = Utc::now().timestamp();

        {
            let list = RevocationList::with_persistence(&path);
            list.revoke("tok-live", now + 3600);
            list.revoke("tok-dead", now - 1);
        }

        // A fresh instance (simulating a restart) loads the persisted entries
        // and prunes the already-expired one.
        let reloaded = RevocationList::with_persistence(&path);
        assert!(reloaded.is_revoked("tok-live"));
        assert!(!reloaded.is_revoked("tok-dead"));
        assert_eq!(reloaded.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_rejects_revoked_token() {
        let secret = b"test-secret";
        let list = RevocationList::new();
        let exp = Utc::now().timestamp() + 3600;
        let token = mint(secret, "tok-1", exp);

        let claims: AuthClaims = validate_ws_token_with_revocation(&token, secret, &list).unwrap();
        assert_eq!(claims.sub, "user-1");

        list.revoke("tok-1", exp);
        assert!(validate_ws_token_with_revocation(&token, secret, &list).is_err());
    }
}
