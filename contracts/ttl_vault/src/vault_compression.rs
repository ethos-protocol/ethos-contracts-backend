/// Issue #558 — Vault compression for inactive accounts
///
/// Vaults whose owner has not checked in for `N` days (default
/// `DEFAULT_INACTIVITY_DAYS`, admin-configurable) can be compressed: the
/// vault's XDR is zero-run-length encoded (see `diff_codec`), stored under a
/// compact `CompressionKey::Compressed` entry, and the uncompressed
/// `DataKey::Vault` entry is removed.
///
/// Decompression is on demand and transparent:
///
/// - `load_vault` / `try_load_vault` fall back to decoding the compressed
///   entry in memory, so every read path keeps working unchanged.
/// - `save_vault` discards any compressed copy, so the first state-mutating
///   operation re-materializes the vault uncompressed.
/// - `decompress_vault` restores a vault explicitly.
///
/// A SHA-256 of the original XDR is stored with the compressed payload and
/// checked on every decode.
use soroban_sdk::{
    contracttype, panic_with_error, symbol_short,
    xdr::{FromXdr, ToXdr},
    Bytes, BytesN, Env, Symbol,
};

use crate::diff_codec;
use crate::types::{DataKey, Vault};
use crate::ContractError;

/// Default number of days without a check-in before a vault may be compressed.
pub const DEFAULT_INACTIVITY_DAYS: u32 = 90;

/// Upper bound for the configurable inactivity period (10 years).
pub const MAX_INACTIVITY_DAYS: u32 = 3_650;

const SECONDS_PER_DAY: u64 = 86_400;

pub const VAULT_COMPRESSED_TOPIC: Symbol = symbol_short!("v_cmp");
pub const VAULT_DECOMPRESSED_TOPIC: Symbol = symbol_short!("v_dcmp");

#[contracttype]
#[derive(Clone)]
pub enum CompressionKey {
    /// Compressed vault payload by vault id.
    Compressed(u64),
    /// Days of inactivity before compression is allowed (u32).
    InactivityDays,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct CompressedVault {
    /// RLE-encoded vault XDR.
    pub data: Bytes,
    pub original_size: u32,
    pub compressed_size: u32,
    pub compressed_at: u64,
    /// SHA-256 of the original vault XDR.
    pub content_hash: BytesN<32>,
    /// Persistent TTL (ledgers) derived from the vault's check-in interval.
    pub ttl_ledgers: u32,
}

/// Summary returned to callers inspecting a compressed vault.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct CompressionInfo {
    pub original_size: u32,
    pub compressed_size: u32,
    pub compressed_at: u64,
}

pub fn get_inactivity_days(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&CompressionKey::InactivityDays)
        .unwrap_or(DEFAULT_INACTIVITY_DAYS)
}

pub fn set_inactivity_days(env: &Env, days: u32) -> Result<(), ContractError> {
    if days == 0 || days > MAX_INACTIVITY_DAYS {
        return Err(ContractError::InvalidConfig);
    }
    env.storage()
        .instance()
        .set(&CompressionKey::InactivityDays, &days);
    Ok(())
}

fn load_compressed(env: &Env, vault_id: u64) -> Option<CompressedVault> {
    env.storage()
        .persistent()
        .get(&CompressionKey::Compressed(vault_id))
}

pub fn is_compressed(env: &Env, vault_id: u64) -> bool {
    env.storage()
        .persistent()
        .has(&CompressionKey::Compressed(vault_id))
}

pub fn get_info(env: &Env, vault_id: u64) -> Option<CompressionInfo> {
    load_compressed(env, vault_id).map(|c| CompressionInfo {
        original_size: c.original_size,
        compressed_size: c.compressed_size,
        compressed_at: c.compressed_at,
    })
}

fn decode(env: &Env, c: &CompressedVault) -> Vault {
    let xdr = diff_codec::decompress(env, &c.data)
        .unwrap_or_else(|| panic_with_error!(env, ContractError::CompressionCorrupted));
    let hash: BytesN<32> = env.crypto().sha256(&xdr).into();
    if xdr.len() != c.original_size || hash != c.content_hash {
        panic_with_error!(env, ContractError::CompressionCorrupted);
    }
    Vault::from_xdr(env, &xdr)
        .unwrap_or_else(|_| panic_with_error!(env, ContractError::CompressionCorrupted))
}

/// Decodes a compressed vault in memory without touching storage. Used by
/// `load_vault` so reads never need an explicit decompression step.
pub fn load_decompressed(env: &Env, vault_id: u64) -> Option<Vault> {
    load_compressed(env, vault_id).map(|c| decode(env, &c))
}

/// Drops the compressed copy once the vault has been re-materialized.
pub fn discard(env: &Env, vault_id: u64) {
    let key = CompressionKey::Compressed(vault_id);
    if env.storage().persistent().has(&key) {
        env.storage().persistent().remove(&key);
        env.events()
            .publish((VAULT_DECOMPRESSED_TOPIC, vault_id), vault_id);
    }
}

/// Compresses the vault if it has been inactive for at least the configured
/// number of days. Returns `true` if the vault was compressed, `false` if it
/// is still active, already compressed, or would not shrink.
pub fn compress(env: &Env, vault_id: u64, ttl_ledgers: u32) -> bool {
    if is_compressed(env, vault_id) {
        return false;
    }
    let vault_key = DataKey::Vault(vault_id);
    let Some(vault) = env.storage().persistent().get::<DataKey, Vault>(&vault_key) else {
        panic_with_error!(env, ContractError::VaultNotFound);
    };

    let now = env.ledger().timestamp();
    let min_idle = u64::from(get_inactivity_days(env)) * SECONDS_PER_DAY;
    if now.saturating_sub(vault.last_check_in) < min_idle {
        return false;
    }

    let xdr = vault.to_xdr(env);
    let data = diff_codec::compress(env, &xdr);
    if data.len() >= xdr.len() {
        return false;
    }

    let record = CompressedVault {
        original_size: xdr.len(),
        compressed_size: data.len(),
        compressed_at: now,
        content_hash: env.crypto().sha256(&xdr).into(),
        ttl_ledgers,
        data,
    };
    let key = CompressionKey::Compressed(vault_id);
    env.storage().persistent().set(&key, &record);
    env.storage()
        .persistent()
        .extend_ttl(&key, crate::VAULT_TTL_THRESHOLD, ttl_ledgers);
    env.storage().persistent().remove(&vault_key);

    env.events().publish(
        (VAULT_COMPRESSED_TOPIC, vault_id),
        (record.original_size, record.compressed_size),
    );
    true
}
