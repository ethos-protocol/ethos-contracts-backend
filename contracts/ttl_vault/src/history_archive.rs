/// Issue #557 — Vault history archival
///
/// A vault's state-transition history (`DataKey::StateTransitionLog`) is a
/// single on-chain `Vec` that grows without bound; every append rewrites the
/// whole entry and its rent grows with it. Archival moves old entries out of
/// that hot log into fixed-size **archive pages**:
///
/// - Only entries older than `ARCHIVE_MIN_AGE_SECONDS` are archived, and only
///   in whole pages of `ARCHIVE_PAGE_SIZE`, so recent history stays hot.
/// - Every archived page is emitted in full as an `hist_arc` event so
///   off-chain indexers can keep the canonical cold copy.
/// - The page body is also kept in persistent storage as cold data: it is
///   written once and never TTL-extended, so the network may archive it
///   off the live ledger over time. `get_archived_history` returns it while
///   it is live; once evicted, the off-chain copy can be verified against the
///   on-chain digest via `verify_archived_page`.
/// - **Integrity**: each page carries
///   `hash = sha256(prev_hash || vault_id || page || xdr(entries))`, forming a
///   hash chain. The small per-page digest and the chain head are kept
///   on-chain (TTL-extended), so archived data can always be verified even
///   after the body is gone.
use soroban_sdk::{
    contracttype, symbol_short, xdr::ToXdr, Bytes, BytesN, Env, IntoVal, Symbol, Val, Vec,
};

use crate::types::{DataKey, StateTransitionEntry};

/// Number of history entries per archive page.
pub const ARCHIVE_PAGE_SIZE: u32 = 25;

/// Entries younger than this (30 days) are never archived.
pub const ARCHIVE_MIN_AGE_SECONDS: u64 = 2_592_000;

/// Upper bound on pages archived per call, bounding per-invocation cost.
pub const MAX_PAGES_PER_CALL: u32 = 8;

/// Short TTL (in ledgers, ~1 day) given to cold page bodies when written.
pub const COLD_PAGE_TTL_LEDGERS: u32 = 17_280;

pub const HISTORY_ARCHIVED_TOPIC: Symbol = symbol_short!("hist_arc");

#[contracttype]
#[derive(Clone)]
pub enum ArchiveKey {
    /// Archive metadata for a vault.
    Meta(u64),
    /// Cold page body by (vault_id, page).
    Page(u64, u32),
    /// On-chain integrity digest by (vault_id, page).
    Digest(u64, u32),
}

/// Per-vault archive bookkeeping.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ArchiveMeta {
    /// Number of pages archived so far.
    pub page_count: u32,
    /// Total entries across all archived pages.
    pub archived_entries: u64,
    /// Hash of the most recent page (all zeros when empty).
    pub head_hash: BytesN<32>,
    /// Timestamp of the last archival run.
    pub last_archived_at: u64,
}

/// On-chain digest of an archived page, kept after the body goes cold.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ArchivePageDigest {
    pub page: u32,
    pub entry_count: u32,
    pub first_timestamp: u64,
    pub last_timestamp: u64,
    pub prev_hash: BytesN<32>,
    pub hash: BytesN<32>,
}

/// An archived page of history returned by `get_archived_history`.
#[contracttype]
#[derive(Clone)]
pub struct ArchivedHistoryPage {
    pub vault_id: u64,
    pub page: u32,
    pub entries: Vec<StateTransitionEntry>,
    pub prev_hash: BytesN<32>,
    pub hash: BytesN<32>,
    pub archived_at: u64,
}

fn persist<V: IntoVal<Env, Val>>(env: &Env, key: &ArchiveKey, value: &V) {
    env.storage().persistent().set(key, value);
    env.storage().persistent().extend_ttl(
        key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );
}

fn zero_hash(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[0u8; 32])
}

pub fn get_meta(env: &Env, vault_id: u64) -> ArchiveMeta {
    env.storage()
        .persistent()
        .get(&ArchiveKey::Meta(vault_id))
        .unwrap_or(ArchiveMeta {
            page_count: 0,
            archived_entries: 0,
            head_hash: zero_hash(env),
            last_archived_at: 0,
        })
}

pub fn get_digest(env: &Env, vault_id: u64, page: u32) -> Option<ArchivePageDigest> {
    env.storage()
        .persistent()
        .get(&ArchiveKey::Digest(vault_id, page))
}

/// Computes the chained hash of an archive page.
pub fn page_hash(
    env: &Env,
    prev_hash: &BytesN<32>,
    vault_id: u64,
    page: u32,
    entries: &Vec<StateTransitionEntry>,
) -> BytesN<32> {
    let mut buf = Bytes::new(env);
    buf.extend_from_array(&prev_hash.to_array());
    buf.extend_from_array(&vault_id.to_le_bytes());
    buf.extend_from_array(&page.to_le_bytes());
    buf.append(&entries.clone().to_xdr(env));
    env.crypto().sha256(&buf).into()
}

/// Moves eligible old entries from the hot state-transition log into archive
/// pages. Returns the number of pages archived by this call.
pub fn archive_history(env: &Env, vault_id: u64) -> u32 {
    let log_key = DataKey::StateTransitionLog(vault_id);
    let log: Vec<StateTransitionEntry> = match env.storage().persistent().get(&log_key) {
        Some(log) => log,
        None => return 0,
    };

    let now = env.ledger().timestamp();
    let cutoff = now.saturating_sub(ARCHIVE_MIN_AGE_SECONDS);

    // The log is append-only in ledger order, so eligible entries form a prefix.
    let mut eligible: u32 = 0;
    for entry in log.iter() {
        if entry.timestamp > cutoff {
            break;
        }
        eligible += 1;
    }
    let pages = (eligible / ARCHIVE_PAGE_SIZE).min(MAX_PAGES_PER_CALL);
    if pages == 0 {
        return 0;
    }

    let mut meta = get_meta(env, vault_id);
    for p in 0..pages {
        let start = p * ARCHIVE_PAGE_SIZE;
        let entries = log.slice(start..start + ARCHIVE_PAGE_SIZE);
        let page = meta.page_count;
        let prev_hash = meta.head_hash.clone();
        let hash = page_hash(env, &prev_hash, vault_id, page, &entries);

        let body = ArchivedHistoryPage {
            vault_id,
            page,
            entries: entries.clone(),
            prev_hash: prev_hash.clone(),
            hash: hash.clone(),
            archived_at: now,
        };
        // Cold body: written once with a short TTL and never extended.
        let page_key = ArchiveKey::Page(vault_id, page);
        env.storage().persistent().set(&page_key, &body);
        env.storage()
            .persistent()
            .extend_ttl(&page_key, COLD_PAGE_TTL_LEDGERS, COLD_PAGE_TTL_LEDGERS);

        persist(
            env,
            &ArchiveKey::Digest(vault_id, page),
            &ArchivePageDigest {
                page,
                entry_count: entries.len(),
                first_timestamp: entries.first().map_or(0, |e| e.timestamp),
                last_timestamp: entries.last().map_or(0, |e| e.timestamp),
                prev_hash,
                hash: hash.clone(),
            },
        );

        // Full page to the event stream for off-chain storage.
        env.events()
            .publish((HISTORY_ARCHIVED_TOPIC, vault_id, page), body);

        meta.page_count += 1;
        meta.archived_entries += u64::from(entries.len());
        meta.head_hash = hash;
    }
    meta.last_archived_at = now;
    persist(env, &ArchiveKey::Meta(vault_id), &meta);

    let remaining = log.slice(pages * ARCHIVE_PAGE_SIZE..log.len());
    if remaining.is_empty() {
        env.storage().persistent().remove(&log_key);
    } else {
        env.storage().persistent().set(&log_key, &remaining);
    }
    pages
}

/// Returns an archived page if its body is still on the live ledger.
pub fn get_archived_page(env: &Env, vault_id: u64, page: u32) -> Option<ArchivedHistoryPage> {
    env.storage()
        .persistent()
        .get(&ArchiveKey::Page(vault_id, page))
}

/// Verifies externally-held (e.g. off-chain) page entries against the
/// on-chain digest for that page.
pub fn verify_page(
    env: &Env,
    vault_id: u64,
    page: u32,
    entries: &Vec<StateTransitionEntry>,
) -> bool {
    match get_digest(env, vault_id, page) {
        Some(d) => page_hash(env, &d.prev_hash, vault_id, page, entries) == d.hash,
        None => false,
    }
}

/// Verifies the digest hash chain for every archived page, and every page
/// body that is still live against its digest. Returns `false` on any break.
pub fn verify_integrity(env: &Env, vault_id: u64) -> bool {
    let meta = get_meta(env, vault_id);
    let mut expected_prev = zero_hash(env);
    for page in 0..meta.page_count {
        let Some(digest) = get_digest(env, vault_id, page) else {
            return false;
        };
        if digest.prev_hash != expected_prev {
            return false;
        }
        if let Some(body) = get_archived_page(env, vault_id, page) {
            if body.hash != digest.hash
                || page_hash(env, &digest.prev_hash, vault_id, page, &body.entries) != digest.hash
            {
                return false;
            }
        }
        expected_prev = digest.hash;
    }
    expected_prev == meta.head_hash
}
