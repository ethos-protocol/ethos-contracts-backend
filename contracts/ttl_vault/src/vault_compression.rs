/// Issue #558 — Vault compression for inactive accounts
///
/// A vault that has not been checked in for `get_inactivity_threshold` seconds
/// (default `DEFAULT_INACTIVITY_SECONDS`, 90 days) can be compressed: its
/// `DataKey::Vault` entry is replaced by a `CompressedVault` holding the
/// zero-run RLE of the vault's XDR (see `diff_codec`). Vault XDR is dominated
/// by fixed-width integers and zero padding, so this typically shrinks the
/// entry substantially; compression is skipped when it would not save space.
///
/// Decompression happens **on demand**: the contract's vault loaders fall back
/// to `load` here when no plain entry exists, so every read keeps working
/// unchanged. The first write (`save_vault`) stores the vault uncompressed
/// again and drops the compressed record. `decompress_vault` does this
/// explicitly. A SHA-256 of the original XDR is stored and verified on every
/// decompression so a corrupted record is detected rather than returned.
use soroban_sdk::{
    contracttype, panic_with_error, symbol_short,
    xdr::{FromXdr, ToXdr},
    Bytes, BytesN, Env, Symbol,
};

use crate::diff_codec;
use crate::types::{DataKey, Vault};
use crate::ContractError;

/// Default inactivity period (90 days) after which a vault may be compressed.
pub const DEFAULT_INACTIVITY_SECONDS: u64 = 7_776_000;

pub const VAULT_COMPRESSED_TOPIC: Symbol = symbol_short!("v_cmp");
pub const VAULT_DECOMPRESSED_TOPIC: Symbol = symbol_short!("v_dcmp");

#[contracttype]
#[derive(Clone)]
pub enum CompressionKey {
    /// Compressed vault body by vault id.
    Vault(u64),
    /// Admin-configured inactivity threshold in seconds.
    InactivityThreshold,
}

/// Compressed form of a vault stored in place of `DataKey::Vault`.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct CompressedVault {
    pub vault_id: u64,
    /// RLE(xdr(vault)).
    pub data: Bytes,
    /// Length of the uncompressed vault XDR.
    pub original_size: u32,
    pub compressed_size: u32,
    /// SHA-256 of the uncompressed vault XDR.
    pub state_hash: BytesN<32>,
    /// Check-in interval, kept uncompressed to derive the entry TTL.
    pub check_in_interval: u64,
    pub compressed_at: u64,
}

/// Summary returned by `get_vault_compression_info`.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct CompressionInfo {
    pub original_size: u32,
    pub compressed_size: u32,
    pub compressed_at: u64,
}

pub fn get_inactivity_threshold(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&CompressionKey::InactivityThreshold)
        .unwrap_or(DEFAULT_INACTIVITY_SECONDS)
}

pub fn set_inactivity_threshold(env: &Env, seconds: u64) -> Result<(), ContractError> {
    if seconds == 0 {
        return Err(ContractError::InvalidConfig);
    }
    env.storage()
        .instance()
        .set(&CompressionKey::InactivityThreshold, &seconds);
    Ok(())
}

fn get_record(env: &Env, vault_id: u64) -> Option<CompressedVault> {
    env.storage()
        .persistent()
        .get(&CompressionKey::Vault(vault_id))
}

pub fn is_compressed(env: &Env, vault_id: u64) -> bool {
    env.storage()
        .persistent()
        .has(&CompressionKey::Vault(vault_id))
}

pub fn get_info(env: &Env, vault_id: u64) -> Option<CompressionInfo> {
    get_record(env, vault_id).map(|r| CompressionInfo {
        original_size: r.original_size,
        compressed_size: r.compressed_size,
        compressed_at: r.compressed_at,
    })
}

fn decode(env: &Env, record: &CompressedVault) -> Vault {
    let xdr = diff_codec::decompress(env, &record.data)
        .unwrap_or_else(|| panic_with_error!(env, ContractError::VaultCompressionCorrupted));
    let hash: BytesN<32> = env.crypto().sha256(&xdr).into();
    if xdr.len() != record.original_size || hash != record.state_hash {
        panic_with_error!(env, ContractError::VaultCompressionCorrupted);
    }
    Vault::from_xdr(env, &xdr)
        .unwrap_or_else(|_| panic_with_error!(env, ContractError::VaultCompressionCorrupted))
}

/// Decodes a compressed vault without touching storage layout. Used by the
/// contract's vault loaders as the fallback when no plain entry exists.
pub fn load(env: &Env, vault_id: u64) -> Option<Vault> {
    get_record(env, vault_id).map(|record| decode(env, &record))
}

/// Compresses the vault if it is stored uncompressed, has been inactive for
/// at least the configured threshold, and compression actually saves space.
/// Returns `true` when the vault was compressed.
pub fn compress(env: &Env, vault_id: u64) -> Result<bool, ContractError> {
    let key = DataKey::Vault(vault_id);
    let vault: Vault = match env.storage().persistent().get(&key) {
        Some(v) => v,
        None if is_compressed(env, vault_id) => return Ok(false),
        None => return Err(ContractError::VaultNotFound),
    };

    let now = env.ledger().timestamp();
    if now.saturating_sub(vault.last_check_in) < get_inactivity_threshold(env) {
        return Ok(false);
    }

    let xdr = vault.clone().to_xdr(env);
    let data = diff_codec::compress(env, &xdr);
    if data.len() >= xdr.len() {
        return Ok(false);
    }

    let record = CompressedVault {
        vault_id,
        original_size: xdr.len(),
        compressed_size: data.len(),
        state_hash: env.crypto().sha256(&xdr).into(),
        data,
        check_in_interval: vault.check_in_interval,
        compressed_at: now,
    };
    let ckey = CompressionKey::Vault(vault_id);
    env.storage().persistent().set(&ckey, &record);
    env.storage().persistent().extend_ttl(
        &ckey,
        crate::VAULT_TTL_THRESHOLD,
        crate::vault_ttl_ledgers(vault.check_in_interval),
    );
    env.storage().persistent().remove(&key);

    env.events().publish(
        (VAULT_COMPRESSED_TOPIC, vault_id),
        (record.original_size, record.compressed_size),
    );
    Ok(true)
}

/// Drops the compressed record once the vault has been written back
/// uncompressed. Called from `save_vault`; a no-op for plain vaults.
pub fn clear(env: &Env, vault_id: u64) {
    let ckey = CompressionKey::Vault(vault_id);
    if env.storage().persistent().has(&ckey) {
        env.storage().persistent().remove(&ckey);
        env.events().publish(
            (VAULT_DECOMPRESSED_TOPIC, vault_id),
            env.ledger().timestamp(),
        );
    }
}

/// Extends the TTL of a compressed record. Returns `false` if there is none.
pub fn extend_ttl(env: &Env, vault_id: u64) -> bool {
    match get_record(env, vault_id) {
        Some(record) => {
            env.storage().persistent().extend_ttl(
                &CompressionKey::Vault(vault_id),
                crate::VAULT_TTL_THRESHOLD,
                crate::vault_ttl_ledgers(record.check_in_interval),
            );
            true
        }
        None => false,
    }
}
