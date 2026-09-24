// These handlers use axum extractors (State/Path/Json) but are not yet
// registered on any Router (see main.rs::build_router) or documented in
// docs/openapi.yaml — 2FA is scaffolded but not a live endpoint today. Kept
// async so the signatures need no rework once/if they're wired up.
#![allow(clippy::unused_async)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::Utc;
use hmac::{Hmac, Mac};
use rand::Rng;
use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::{
    db::Db,
    error::AppError,
    models::{
        Enable2FARequest, Enable2FAResponse, TwoFactorConfig, TwoFactorMethod,
        TwoFactorStatusResponse, Verify2FARequest,
    },
};

// ── Global stores ────────────────────────────────────────────────────────────

struct PendingOtp {
    code: String,
    expires_at: u64,
}

static PENDING_OTPS: std::sync::LazyLock<Mutex<HashMap<String, Vec<PendingOtp>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

static SESSION_VERIFIED: std::sync::LazyLock<Mutex<HashMap<String, bool>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Serializes read-check-remove-write of a vault's backup codes so that two
/// concurrent verification attempts for the same code cannot both observe it
/// as unused before either has persisted its removal.
static BACKUP_CODE_CONSUME_LOCK: std::sync::LazyLock<Mutex<()>> =
    std::sync::LazyLock::new(|| Mutex::new(()));

const BACKUP_CODE_COUNT: usize = 10;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Generate a fresh set of plaintext backup codes.
fn generate_backup_codes() -> Vec<String> {
    let mut rng = rand::thread_rng();
    (0..BACKUP_CODE_COUNT)
        .map(|_| format!("{:010}", rng.gen_range(0..10_000_000_000u64)))
        .collect()
}

/// SHA-256 hex digest of a backup code, for storage/comparison without
/// keeping the code itself in recoverable form.
fn hash_backup_code(code: &str) -> String {
    let digest = Sha256::digest(code.trim().as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Atomically check `code` against `vault_id`'s unused backup codes and, if
/// it matches, remove it so it cannot be used again. Returns `true` only for
/// the call that actually consumed the code; under concurrent attempts with
/// the same code, exactly one caller sees `true`.
fn verify_and_consume_backup_code(db: &Db, vault_id: &str, code: &str) -> Result<bool, AppError> {
    let _guard = BACKUP_CODE_CONSUME_LOCK.lock().unwrap();

    let Some(mut config) = db.get_2fa_config(vault_id)? else {
        return Ok(false);
    };
    let hashed = hash_backup_code(code);
    let Some(pos) = config.backup_codes.iter().position(|c| c == &hashed) else {
        return Ok(false);
    };
    config.backup_codes.remove(pos);
    db.upsert_2fa_config(&config)?;
    Ok(true)
}

fn generate_otp_code() -> String {
    let mut rng = rand::thread_rng();
    format!("{:06}", rng.gen_range(0..1_000_000))
}

fn generate_totp_secret() -> String {
    let mut rng = rand::thread_rng();
    let bytes: Vec<u8> = (0..20).map(|_| rng.gen()).collect();
    base32_encode(&bytes)
}

fn base32_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    let mut buffer = 0u64;
    let mut bits = 0;
    for &byte in input {
        buffer = (buffer << 8) | byte as u64;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 0x1F) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 0x1F) as usize] as char);
    }
    out
}

fn generate_provisioning_uri(secret: &str, label: &str) -> String {
    let encoded_label: String = label
        .chars()
        .map(|c| match c {
            ':' | ' ' => '_',
            _ => c,
        })
        .collect();
    format!(
        "otpauth://totp/{encoded_label}?secret={secret}&issuer=Ethos-Protocol&algorithm=SHA1&digits=6&period=30"
    )
}

fn verify_totp_code(secret: &str, code: &str) -> bool {
    let Some(secret_bytes) = base32_decode(secret) else {
        return false;
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let time_step = now / 30;

    for offset in [0u64, 1, u64::MAX] {
        let counter = if offset == u64::MAX {
            if time_step == 0 {
                continue;
            }
            time_step - 1
        } else {
            time_step + offset
        };

        let counter_be = counter.to_be_bytes();
        let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(&secret_bytes) else {
            return false;
        };
        mac.update(&counter_be);
        let result = mac.finalize();
        let hash = result.into_bytes();

        let offset = (hash[19] & 0x0F) as usize;
        let binary = ((hash[offset] & 0x7F) as u32) << 24
            | (hash[offset + 1] as u32) << 16
            | (hash[offset + 2] as u32) << 8
            | (hash[offset + 3] as u32);
        let totp = binary % 1_000_000;

        if format!("{totp:06}") == code {
            return true;
        }
    }
    false
}

fn base32_decode(input: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let cleaned: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    let cleaned = cleaned.to_uppercase();

    let mut out = Vec::new();
    let mut buffer = 0u64;
    let mut bits = 0;

    for c in cleaned.chars() {
        let val = match ALPHABET.iter().position(|&a| a as char == c) {
            Some(v) => v as u64,
            None => return None,
        };
        buffer = (buffer << 5) | val;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn verify_pending_otp(vault_id: &str, code: &str) -> bool {
    let mut store = PENDING_OTPS.lock().unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    if let Some(codes) = store.get_mut(vault_id) {
        codes.retain(|otp| otp.expires_at > now);
        if let Some(pos) = codes.iter().position(|otp| otp.code == code) {
            codes.remove(pos);
            return true;
        }
    }
    false
}

// ── Route handlers ───────────────────────────────────────────────────────────

/// GET /api/vaults/{vault_id}/2fa/status
pub async fn get_2fa_status(
    State(db): State<Arc<Db>>,
    Path(vault_id): Path<String>,
) -> Result<Json<TwoFactorStatusResponse>, AppError> {
    let config = db.get_2fa_config(&vault_id)?;
    let session_verified = SESSION_VERIFIED
        .lock()
        .unwrap()
        .get(&vault_id)
        .copied()
        .unwrap_or(false);

    match config {
        Some(cfg) => Ok(Json(TwoFactorStatusResponse {
            vault_id: cfg.vault_id,
            enabled: cfg.enabled,
            method: Some(cfg.method),
            verified: session_verified,
            phone: cfg.phone,
            email: cfg.email,
        })),
        None => Ok(Json(TwoFactorStatusResponse {
            vault_id,
            enabled: false,
            method: None,
            verified: false,
            phone: None,
            email: None,
        })),
    }
}

/// POST /api/vaults/{vault_id}/2fa/enable
pub async fn enable_2fa(
    State(db): State<Arc<Db>>,
    Path(vault_id): Path<String>,
    Json(body): Json<Enable2FARequest>,
) -> Result<Json<Enable2FAResponse>, AppError> {
    match &body.method {
        TwoFactorMethod::Sms => {
            if body.phone.as_ref().is_none_or(|p| p.trim().is_empty()) {
                return Err(AppError::InvalidInput(
                    "phone is required for SMS 2FA".into(),
                ));
            }
        }
        TwoFactorMethod::Email => {
            if body.email.as_ref().is_none_or(|e| e.trim().is_empty()) {
                return Err(AppError::InvalidInput(
                    "email is required for Email 2FA".into(),
                ));
            }
        }
        TwoFactorMethod::Totp => {}
    }

    match &body.method {
        TwoFactorMethod::Totp => {
            let secret = generate_totp_secret();
            let provisioning_uri = generate_provisioning_uri(&secret, &vault_id);
            let backup_codes = generate_backup_codes();
            let hashed_backup_codes = backup_codes.iter().map(|c| hash_backup_code(c)).collect();

            let config = TwoFactorConfig {
                vault_id: vault_id.clone(),
                method: TwoFactorMethod::Totp,
                enabled: false,
                secret: Some(secret.clone()),
                phone: None,
                email: None,
                created_at: Utc::now(),
                verified_at: None,
                backup_codes: hashed_backup_codes,
            };
            db.upsert_2fa_config(&config)?;

            Ok(Json(Enable2FAResponse {
                vault_id,
                method: TwoFactorMethod::Totp,
                secret: Some(secret),
                provisioning_uri: Some(provisioning_uri),
                backup_codes: Some(backup_codes),
            }))
        }
        TwoFactorMethod::Sms => {
            let phone = body.phone.unwrap_or_default();
            let code = generate_otp_code();
            let expires_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 300;

            PENDING_OTPS
                .lock()
                .unwrap()
                .entry(vault_id.clone())
                .or_default()
                .push(PendingOtp {
                    code: code.clone(),
                    expires_at,
                });

            let config = TwoFactorConfig {
                vault_id: vault_id.clone(),
                method: TwoFactorMethod::Sms,
                enabled: false,
                secret: None,
                phone: Some(phone.clone()),
                email: None,
                created_at: Utc::now(),
                verified_at: None,
                backup_codes: Vec::new(),
            };
            db.upsert_2fa_config(&config)?;

            tracing::info!(vault_id, phone, code, "SMS OTP sent");

            Ok(Json(Enable2FAResponse {
                vault_id,
                method: TwoFactorMethod::Sms,
                secret: None,
                provisioning_uri: None,
                backup_codes: None,
            }))
        }
        TwoFactorMethod::Email => {
            let email = body.email.unwrap_or_default();
            let code = generate_otp_code();
            let expires_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 300;

            PENDING_OTPS
                .lock()
                .unwrap()
                .entry(vault_id.clone())
                .or_default()
                .push(PendingOtp {
                    code: code.clone(),
                    expires_at,
                });

            let config = TwoFactorConfig {
                vault_id: vault_id.clone(),
                method: TwoFactorMethod::Email,
                enabled: false,
                secret: None,
                phone: None,
                email: Some(email.clone()),
                created_at: Utc::now(),
                verified_at: None,
                backup_codes: Vec::new(),
            };
            db.upsert_2fa_config(&config)?;

            tracing::info!(vault_id, email, code, "Email OTP sent");

            Ok(Json(Enable2FAResponse {
                vault_id,
                method: TwoFactorMethod::Email,
                secret: None,
                provisioning_uri: None,
                backup_codes: None,
            }))
        }
    }
}

/// POST /api/vaults/{vault_id}/2fa/verify
pub async fn verify_2fa(
    State(db): State<Arc<Db>>,
    Path(vault_id): Path<String>,
    Json(body): Json<Verify2FARequest>,
) -> Result<StatusCode, AppError> {
    let config = db.get_2fa_config(&vault_id)?.ok_or(AppError::NotFound)?;

    let mut used_backup_code = false;
    let valid = match &config.method {
        TwoFactorMethod::Totp => {
            let secret = config
                .secret
                .as_ref()
                .ok_or_else(|| AppError::InvalidInput("TOTP secret not found".into()))?;
            if verify_totp_code(secret, &body.otp) {
                true
            } else if verify_and_consume_backup_code(&db, &vault_id, &body.otp)? {
                used_backup_code = true;
                true
            } else {
                false
            }
        }
        TwoFactorMethod::Sms | TwoFactorMethod::Email => verify_pending_otp(&vault_id, &body.otp),
    };

    if !valid {
        return Err(AppError::InvalidInput("Invalid or expired OTP".into()));
    }

    // If a backup code was just consumed, re-read the config so this update
    // doesn't overwrite `backup_codes` with the stale pre-consumption list.
    let base = if used_backup_code {
        db.get_2fa_config(&vault_id)?.unwrap_or(config)
    } else {
        config
    };

    let updated = TwoFactorConfig {
        enabled: true,
        verified_at: Some(Utc::now()),
        ..base
    };
    db.upsert_2fa_config(&updated)?;

    SESSION_VERIFIED.lock().unwrap().insert(vault_id, true);

    Ok(StatusCode::OK)
}

/// POST /api/vaults/{vault_id}/2fa/disable
pub async fn disable_2fa(
    State(db): State<Arc<Db>>,
    Path(vault_id): Path<String>,
) -> Result<StatusCode, AppError> {
    db.delete_2fa_config(&vault_id)?;
    SESSION_VERIFIED.lock().unwrap().remove(&vault_id);
    PENDING_OTPS.lock().unwrap().remove(&vault_id);
    Ok(StatusCode::NO_CONTENT)
}

/// POST /api/vaults/{vault_id}/2fa/challenge
pub async fn challenge_2fa(
    State(db): State<Arc<Db>>,
    Path(vault_id): Path<String>,
) -> Result<Json<TwoFactorStatusResponse>, AppError> {
    let config = db.get_2fa_config(&vault_id)?;
    let session_verified = SESSION_VERIFIED
        .lock()
        .unwrap()
        .get(&vault_id)
        .copied()
        .unwrap_or(false);

    match config {
        Some(cfg) => {
            let requires_2fa = cfg.enabled && !session_verified;
            Ok(Json(TwoFactorStatusResponse {
                vault_id: cfg.vault_id,
                enabled: cfg.enabled,
                method: Some(cfg.method),
                verified: !requires_2fa,
                phone: cfg.phone,
                email: cfg.email,
            }))
        }
        None => Ok(Json(TwoFactorStatusResponse {
            vault_id,
            enabled: false,
            method: None,
            verified: true,
            phone: None,
            email: None,
        })),
    }
}

/// POST /api/vaults/{vault_id}/2fa/session/clear
pub async fn clear_2fa_session(Path(vault_id): Path<String>) -> Result<StatusCode, AppError> {
    SESSION_VERIFIED.lock().unwrap().remove(&vault_id);
    Ok(StatusCode::OK)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    fn config_with_codes(vault_id: &str, codes: &[&str]) -> TwoFactorConfig {
        TwoFactorConfig {
            vault_id: vault_id.to_string(),
            method: TwoFactorMethod::Totp,
            enabled: true,
            secret: Some(generate_totp_secret()),
            phone: None,
            email: None,
            created_at: Utc::now(),
            verified_at: Some(Utc::now()),
            backup_codes: codes.iter().map(|c| hash_backup_code(c)).collect(),
        }
    }

    #[test]
    fn generate_backup_codes_are_unique_and_well_formed() {
        let codes = generate_backup_codes();
        assert_eq!(codes.len(), BACKUP_CODE_COUNT);
        let unique: std::collections::HashSet<_> = codes.iter().collect();
        assert_eq!(unique.len(), codes.len());
        for code in &codes {
            assert_eq!(code.len(), 10);
            assert!(code.chars().all(|c| c.is_ascii_digit()));
        }
    }

    #[test]
    fn backup_code_is_single_use() {
        let db = Db::open(":memory:").unwrap();
        db.migrate().unwrap();
        let config = config_with_codes("vault-1", &["1111111111", "2222222222"]);
        db.upsert_2fa_config(&config).unwrap();

        // First use succeeds and removes the code.
        assert!(verify_and_consume_backup_code(&db, "vault-1", "1111111111").unwrap());
        let after_first = db.get_2fa_config("vault-1").unwrap().unwrap();
        assert_eq!(after_first.backup_codes.len(), 1);

        // Reusing the same code is rejected.
        assert!(!verify_and_consume_backup_code(&db, "vault-1", "1111111111").unwrap());

        // The other, untouched code still works.
        assert!(verify_and_consume_backup_code(&db, "vault-1", "2222222222").unwrap());
        let after_second = db.get_2fa_config("vault-1").unwrap().unwrap();
        assert!(after_second.backup_codes.is_empty());
    }

    #[test]
    fn unknown_backup_code_is_rejected() {
        let db = Db::open(":memory:").unwrap();
        db.migrate().unwrap();
        let config = config_with_codes("vault-1", &["1111111111"]);
        db.upsert_2fa_config(&config).unwrap();

        assert!(!verify_and_consume_backup_code(&db, "vault-1", "9999999999").unwrap());
        assert_eq!(
            db.get_2fa_config("vault-1").unwrap().unwrap().backup_codes.len(),
            1
        );
    }

    /// Regression test: two threads racing to consume the same backup code
    /// must not both succeed. Without the atomic read-check-remove-write
    /// under `BACKUP_CODE_CONSUME_LOCK`, both could observe the code as
    /// unused before either persisted its removal.
    #[test]
    fn concurrent_verification_consumes_backup_code_exactly_once() {
        let db = Arc::new(Db::open(":memory:").unwrap());
        db.migrate().unwrap();
        let config = config_with_codes("vault-race", &["5555555555"]);
        db.upsert_2fa_config(&config).unwrap();

        let barrier = Arc::new(Barrier::new(4));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let db = Arc::clone(&db);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    verify_and_consume_backup_code(&db, "vault-race", "5555555555").unwrap()
                })
            })
            .collect();

        let successes = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|&ok| ok)
            .count();

        assert_eq!(successes, 1, "exactly one concurrent attempt should consume the code");
        assert!(db
            .get_2fa_config("vault-race")
            .unwrap()
            .unwrap()
            .backup_codes
            .is_empty());
    }
}
