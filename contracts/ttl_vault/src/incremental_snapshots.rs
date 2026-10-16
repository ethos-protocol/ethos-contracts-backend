/// Issue #559 — Incremental vault state snapshots
///
/// `create_vault_snapshot` stores a complete copy of the vault every time it
/// is called, which is expensive for vaults that change little between
/// snapshots. This module stores snapshots as a chain:
///
/// - Every `FULL_SNAPSHOT_INTERVAL`-th snapshot (sequence numbers `0, 16, 32,
///   ...`) is a **checkpoint** holding every vault field.
/// - All other snapshots are **deltas** holding only the fields that changed
///   since the previous snapshot.
///
/// Each vault field is serialized independently as XDR. A changed field is
/// stored with **differential compression**: its new encoding is XORed
/// against the previous encoding (unchanged bytes become zero) and the result
/// is zero-run-length encoded (see `diff_codec`). Checkpoints use the same
/// encoding against an empty previous value, i.e. plain RLE.
///
/// A full vault state is reconstructed by replaying the chain from the
/// nearest checkpoint at or before the requested sequence number. Every
/// snapshot records the SHA-256 of the complete vault XDR, which is verified
/// after reconstruction so corruption is detected rather than silently
/// returned.
use soroban_sdk::{
    contracttype, panic_with_error, symbol_short,
    xdr::{FromXdr, ToXdr},
    Bytes, BytesN, Env, IntoVal, Symbol, TryFromVal, Val, Vec,
};

use crate::diff_codec;
use crate::types::Vault;
use crate::ContractError;

/// A checkpoint (full snapshot) is written every this many snapshots, bounding
/// reconstruction cost to at most this many chain entries.
pub const FULL_SNAPSHOT_INTERVAL: u32 = 16;

/// Number of independently-diffed fields in `Vault`.
pub const VAULT_FIELD_COUNT: u32 = 21;

pub const INCR_SNAPSHOT_TOPIC: Symbol = symbol_short!("snap_inc");

#[contracttype]
#[derive(Clone)]
pub enum SnapshotKey {
    /// Number of incremental snapshots taken for a vault (next sequence).
    Count(u64),
    /// Snapshot entry by (vault_id, sequence).
    Entry(u64, u32),
}

/// A single changed field, differentially compressed against its previous
/// encoding.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct FieldDelta {
    /// Index of the field in `Vault` declaration order.
    pub field: u32,
    /// Length of the field's new XDR encoding.
    pub len: u32,
    /// RLE(XOR(previous encoding, new encoding)).
    pub data: Bytes,
}

/// One link in a vault's snapshot chain.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct IncrementalSnapshot {
    pub vault_id: u64,
    pub sequence: u32,
    pub timestamp: u64,
    /// `true` for checkpoints holding every field.
    pub is_full: bool,
    /// Changed fields only (all fields for checkpoints).
    pub deltas: Vec<FieldDelta>,
    /// SHA-256 of the complete vault XDR at this snapshot.
    pub state_hash: BytesN<32>,
    /// Size of the complete vault XDR, for overhead reporting.
    pub full_size: u32,
    /// Total bytes of stored delta payloads.
    pub stored_size: u32,
}

// ── Field (de)serialization ──────────────────────────────────────────────────

fn enc<T: IntoVal<Env, Val>>(env: &Env, value: T) -> Bytes {
    value.to_xdr(env)
}

fn dec<T: TryFromVal<Env, Val>>(env: &Env, fields: &Vec<Bytes>, index: u32) -> T {
    let bytes = fields
        .get(index)
        .unwrap_or_else(|| panic_with_error!(env, ContractError::SnapshotCorrupted));
    T::from_xdr(env, &bytes)
        .unwrap_or_else(|_| panic_with_error!(env, ContractError::SnapshotCorrupted))
}

/// Serializes each vault field independently, in declaration order.
pub fn encode_fields(env: &Env, v: &Vault) -> Vec<Bytes> {
    let mut f = Vec::new(env);
    f.push_back(enc(env, v.owner.clone()));
    f.push_back(enc(env, v.beneficiary.clone()));
    f.push_back(enc(env, v.balance));
    f.push_back(enc(env, v.check_in_interval));
    f.push_back(enc(env, v.last_check_in));
    f.push_back(enc(env, v.created_at));
    f.push_back(enc(env, v.status.clone()));
    f.push_back(enc(env, v.beneficiaries.clone()));
    f.push_back(enc(env, v.metadata.clone()));
    f.push_back(enc(env, v.token_address.clone()));
    f.push_back(enc(env, v.custom_metadata.clone()));
    f.push_back(enc(env, v.is_paused));
    f.push_back(enc(env, v.release_condition.clone()));
    f.push_back(enc(env, v.parent_vault_id));
    f.push_back(enc(env, v.passkey_hash.clone()));
    f.push_back(enc(env, v.max_deposit_amount));
    f.push_back(enc(env, v.withdrawal_approval_threshold));
    f.push_back(enc(env, v.spending_limit));
    f.push_back(enc(env, v.inactivity_penalty_bps));
    f.push_back(enc(env, v.burn_percentage));
    f.push_back(enc(env, v.penalty_recipient.clone()));
    f
}

/// Inverse of [`encode_fields`].
pub fn decode_fields(env: &Env, f: &Vec<Bytes>) -> Vault {
    Vault {
        owner: dec(env, f, 0),
        beneficiary: dec(env, f, 1),
        balance: dec(env, f, 2),
        check_in_interval: dec(env, f, 3),
        last_check_in: dec(env, f, 4),
        created_at: dec(env, f, 5),
        status: dec(env, f, 6),
        beneficiaries: dec(env, f, 7),
        metadata: dec(env, f, 8),
        token_address: dec(env, f, 9),
        custom_metadata: dec(env, f, 10),
        is_paused: dec(env, f, 11),
        release_condition: dec(env, f, 12),
        parent_vault_id: dec(env, f, 13),
        passkey_hash: dec(env, f, 14),
        max_deposit_amount: dec(env, f, 15),
        withdrawal_approval_threshold: dec(env, f, 16),
        spending_limit: dec(env, f, 17),
        inactivity_penalty_bps: dec(env, f, 18),
        burn_percentage: dec(env, f, 19),
        penalty_recipient: dec(env, f, 20),
    }
}

fn empty_fields(env: &Env) -> Vec<Bytes> {
    let mut f = Vec::new(env);
    for _ in 0..VAULT_FIELD_COUNT {
        f.push_back(Bytes::new(env));
    }
    f
}

fn state_hash(env: &Env, vault: &Vault) -> (BytesN<32>, u32) {
    let xdr = vault.clone().to_xdr(env);
    (env.crypto().sha256(&xdr).into(), xdr.len())
}

// ── Chain storage ────────────────────────────────────────────────────────────

fn persist<V: IntoVal<Env, Val>>(env: &Env, key: &SnapshotKey, value: &V) {
    env.storage().persistent().set(key, value);
    env.storage().persistent().extend_ttl(
        key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );
}

pub fn get_snapshot_count(env: &Env, vault_id: u64) -> u32 {
    env.storage()
        .persistent()
        .get(&SnapshotKey::Count(vault_id))
        .unwrap_or(0)
}

pub fn get_snapshot(env: &Env, vault_id: u64, sequence: u32) -> Option<IncrementalSnapshot> {
    env.storage()
        .persistent()
        .get(&SnapshotKey::Entry(vault_id, sequence))
}

/// Replays the chain from the nearest checkpoint up to `sequence` and
/// returns the per-field encodings at that point.
fn replay_fields(env: &Env, vault_id: u64, sequence: u32) -> Vec<Bytes> {
    let checkpoint = sequence - (sequence % FULL_SNAPSHOT_INTERVAL);
    let mut fields = empty_fields(env);
    for seq in checkpoint..=sequence {
        let snap = get_snapshot(env, vault_id, seq)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::SnapshotNotFound));
        if seq == checkpoint && !snap.is_full {
            panic_with_error!(env, ContractError::SnapshotCorrupted);
        }
        for delta in snap.deltas.iter() {
            let prev = fields
                .get(delta.field)
                .unwrap_or_else(|| panic_with_error!(env, ContractError::SnapshotCorrupted));
            let next = diff_codec::diff_decompress(env, &prev, &delta.data, delta.len)
                .unwrap_or_else(|| panic_with_error!(env, ContractError::SnapshotCorrupted));
            fields.set(delta.field, next);
        }
    }
    fields
}

/// Takes an incremental snapshot of the vault's current state and returns
/// its sequence number.
pub fn create_snapshot(env: &Env, vault_id: u64, vault: &Vault) -> u32 {
    let sequence = get_snapshot_count(env, vault_id);
    let is_full = sequence.is_multiple_of(FULL_SNAPSHOT_INTERVAL);

    let current = encode_fields(env, vault);
    let previous = if is_full {
        empty_fields(env)
    } else {
        replay_fields(env, vault_id, sequence - 1)
    };

    let mut deltas = Vec::new(env);
    let mut stored_size: u32 = 0;
    for i in 0..VAULT_FIELD_COUNT {
        let next = current.get(i).unwrap();
        let prev = previous.get(i).unwrap_or_else(|| Bytes::new(env));
        if !is_full && next == prev {
            continue;
        }
        let data = diff_codec::diff_compress(env, &prev, &next);
        stored_size = stored_size.saturating_add(data.len());
        deltas.push_back(FieldDelta {
            field: i,
            len: next.len(),
            data,
        });
    }

    let (hash, full_size) = state_hash(env, vault);
    let snapshot = IncrementalSnapshot {
        vault_id,
        sequence,
        timestamp: env.ledger().timestamp(),
        is_full,
        deltas,
        state_hash: hash,
        full_size,
        stored_size,
    };
    persist(env, &SnapshotKey::Entry(vault_id, sequence), &snapshot);
    persist(env, &SnapshotKey::Count(vault_id), &(sequence + 1));

    env.events().publish(
        (INCR_SNAPSHOT_TOPIC, vault_id),
        (sequence, is_full, full_size, stored_size),
    );
    sequence
}

/// Reconstructs the full vault state at `sequence`, verifying it against the
/// state hash recorded when the snapshot was taken.
pub fn reconstruct(env: &Env, vault_id: u64, sequence: u32) -> Result<Vault, ContractError> {
    let snap = get_snapshot(env, vault_id, sequence).ok_or(ContractError::SnapshotNotFound)?;
    let fields = replay_fields(env, vault_id, sequence);
    let vault = decode_fields(env, &fields);
    let (hash, _) = state_hash(env, &vault);
    if hash != snap.state_hash {
        return Err(ContractError::SnapshotCorrupted);
    }
    Ok(vault)
}

/// Returns the sequence number of the latest snapshot taken at or before
/// `timestamp`, if any.
pub fn find_sequence_at(env: &Env, vault_id: u64, timestamp: u64) -> Option<u32> {
    let count = get_snapshot_count(env, vault_id);
    // Sequences are taken in ledger order, so timestamps are non-decreasing:
    // binary search for the last snapshot with `timestamp <= target`.
    let (mut lo, mut hi) = (0u32, count);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let ts = get_snapshot(env, vault_id, mid).map_or(u64::MAX, |s| s.timestamp);
        if ts <= timestamp {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo.checked_sub(1)
}
