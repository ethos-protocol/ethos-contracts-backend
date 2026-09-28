/// Issues #548, #549, #550, #551 — Regulatory compliance controls
///
/// This module groups the vault's compliance primitives:
///
/// - **Blacklist / allowlist (#551)** — addresses can be blacklisted with a
///   recorded reason, and an optional allowlist mode restricts vault activity
///   to explicitly approved addresses. `is_address_compliant` combines both.
/// - **KYC verification (#548)** — an admin-registered KYC provider attests to
///   an address' verification via `verify_kyc`. Only an opaque hash of the
///   provider's reference is stored on-chain; raw PII never is. High-value
///   operations at or above a configurable threshold require a valid KYC
///   record.
/// - **Transaction reporting (#549)** — deposits and withdrawals are appended
///   to a timestamp-ordered transaction log that can be exported as CSV for a
///   date range. A registered ed25519 report signer can sign the SHA-256
///   digest of an export, producing a verifiable record for regulatory
///   submission.
/// - **Threshold monitoring (#550)** — configurable single-transaction and
///   rolling-window cumulative thresholds flag reportable transactions and
///   raise compliance alerts.
///
/// Auth for admin-only configuration is enforced by the outer wrappers in
/// `lib.rs`; the KYC provider's auth is enforced here since the provider is a
/// compliance-specific role.
use soroban_sdk::{
    contracttype, symbol_short, xdr::ToXdr, Address, Bytes, BytesN, Env, IntoVal, String, Symbol,
    Val,
};

use crate::ContractError;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum number of rows a single `export_transactions` call returns. Callers
/// exporting a busier period should split the date range; `count_transactions`
/// reports how many rows a range contains.
pub const MAX_EXPORT_ROWS: u32 = 200;

/// Maximum length (bytes) of a blacklist reason.
pub const MAX_REASON_LEN: u32 = 256;

/// CSV header emitted as the first line of every export.
pub const CSV_HEADER: &str = "tx_id,timestamp,kind,vault_id,from,to,token,amount,flagged\n";

// ── Event topics ─────────────────────────────────────────────────────────────

pub const BLACKLIST_ADDED_TOPIC: Symbol = symbol_short!("bl_add");
pub const BLACKLIST_REMOVED_TOPIC: Symbol = symbol_short!("bl_rem");
pub const ALLOWLIST_ADDED_TOPIC: Symbol = symbol_short!("al_add");
pub const ALLOWLIST_REMOVED_TOPIC: Symbol = symbol_short!("al_rem");
pub const ALLOWLIST_MODE_TOPIC: Symbol = symbol_short!("al_mode");
pub const KYC_VERIFIED_TOPIC: Symbol = symbol_short!("kyc_ok");
pub const KYC_REJECTED_TOPIC: Symbol = symbol_short!("kyc_rej");
pub const KYC_REVOKED_TOPIC: Symbol = symbol_short!("kyc_rev");
pub const TX_RECORDED_TOPIC: Symbol = symbol_short!("tx_rec");
pub const REPORT_SIGNED_TOPIC: Symbol = symbol_short!("rpt_sign");
pub const COMPLIANCE_ALERT_TOPIC: Symbol = symbol_short!("cmp_alrt");

// ── Storage keys ─────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
pub enum ComplianceKey {
    /// Blacklist entry for an address.
    Blacklist(Address),
    /// Allowlist entry for an address.
    Allowlist(Address),
    /// Whether allowlist mode is enforced (bool).
    AllowlistEnforced,
    /// Address of the registered KYC provider.
    KycProvider,
    /// KYC record for an address.
    Kyc(Address),
    /// Amount at or above which operations require KYC (i128, 0 = disabled).
    KycHighValueThreshold,
    /// Number of transactions recorded (next transaction id).
    TxCount,
    /// Transaction record by id.
    Tx(u64),
    /// ed25519 public key authorized to sign compliance reports.
    ReportSigner,
    /// Number of signed reports (next report id).
    ReportCount,
    /// Signed report by id.
    SignedReport(u64),
    /// Reporting threshold configuration.
    ThresholdConfig,
    /// Rolling cumulative volume for an address.
    Cumulative(Address),
    /// Number of alerts raised (next alert id).
    AlertCount,
    /// Compliance alert by id.
    Alert(u64),
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// Why and when an address was blacklisted.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct BlacklistEntry {
    pub reason: String,
    pub added_by: Address,
    pub added_at: u64,
}

/// Provider-supplied KYC attestation passed to `verify_kyc`.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct KycData {
    /// SHA-256 hash of the provider's internal verification reference.
    pub provider_reference: BytesN<32>,
    /// Verification tier assigned by the provider (must be > 0).
    pub level: u32,
    /// Ledger timestamp after which the verification is no longer valid.
    pub expires_at: u64,
}

#[contracttype]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KycStatus {
    Verified,
    Revoked,
}

/// Stored KYC verification state for an address.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct KycRecord {
    pub status: KycStatus,
    pub level: u32,
    pub provider_reference: BytesN<32>,
    pub verified_by: Address,
    pub verified_at: u64,
    pub expires_at: u64,
}

#[contracttype]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxKind {
    Deposit,
    Withdrawal,
}

/// A single reportable transaction.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct TransactionRecord {
    pub id: u64,
    pub kind: TxKind,
    pub vault_id: u64,
    pub from: Address,
    pub to: Address,
    pub token: Address,
    pub amount: i128,
    pub timestamp: u64,
    /// True when threshold monitoring flagged this transaction as reportable.
    pub flagged: bool,
}

/// Reporting thresholds. A threshold of `0` disables that check.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ThresholdConfig {
    /// Any single transaction at or above this amount is flagged.
    pub single_tx_threshold: i128,
    /// Cumulative volume per address within `window_seconds` at or above
    /// this amount is flagged.
    pub cumulative_threshold: i128,
    /// Length of the rolling cumulative window in seconds.
    pub window_seconds: u64,
}

/// Cumulative transfer volume for an address in the current window.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct CumulativeVolume {
    pub window_start: u64,
    pub total: i128,
    /// Whether the cumulative threshold already fired in this window, so a
    /// single window raises at most one cumulative alert.
    pub alerted: bool,
}

#[contracttype]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertKind {
    SingleTransaction,
    Cumulative,
}

/// A compliance alert raised by threshold monitoring.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ComplianceAlert {
    pub id: u64,
    pub kind: AlertKind,
    pub address: Address,
    pub tx_id: u64,
    pub amount: i128,
    /// Cumulative volume in the window at the time of the alert.
    pub cumulative: i128,
    pub threshold: i128,
    pub created_at: u64,
    pub acknowledged: bool,
}

/// A report whose digest has been signed by the registered report signer.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct SignedReport {
    pub id: u64,
    pub start_date: u64,
    pub end_date: u64,
    pub row_count: u32,
    /// SHA-256 of the exact CSV returned by `export_transactions`.
    pub report_hash: BytesN<32>,
    pub signer: BytesN<32>,
    pub signature: BytesN<64>,
    pub signed_at: u64,
}

// ── Storage helpers ──────────────────────────────────────────────────────────

fn persist<V: IntoVal<Env, Val>>(env: &Env, key: &ComplianceKey, value: &V) {
    env.storage().persistent().set(key, value);
    env.storage().persistent().extend_ttl(
        key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );
}

fn next_counter(env: &Env, key: &ComplianceKey) -> u64 {
    let id: u64 = env.storage().persistent().get(key).unwrap_or(0);
    persist(env, key, &(id + 1));
    id
}

// ── Blacklist / allowlist (#551) ─────────────────────────────────────────────

/// Blacklist `address`, recording `reason`. Re-blacklisting overwrites the
/// previous entry with the new reason.
pub fn blacklist_address(
    env: &Env,
    address: &Address,
    reason: String,
    added_by: &Address,
) -> Result<(), ContractError> {
    if reason.is_empty() || reason.len() > MAX_REASON_LEN {
        return Err(ContractError::InvalidBlacklistReason);
    }
    let entry = BlacklistEntry {
        reason: reason.clone(),
        added_by: added_by.clone(),
        added_at: env.ledger().timestamp(),
    };
    persist(env, &ComplianceKey::Blacklist(address.clone()), &entry);
    env.events()
        .publish((BLACKLIST_ADDED_TOPIC, address.clone()), reason);
    Ok(())
}

/// Remove `address` from the blacklist. Returns `false` if it was not listed.
pub fn remove_from_blacklist(env: &Env, address: &Address) -> bool {
    let key = ComplianceKey::Blacklist(address.clone());
    if !env.storage().persistent().has(&key) {
        return false;
    }
    env.storage().persistent().remove(&key);
    env.events()
        .publish((BLACKLIST_REMOVED_TOPIC, address.clone()), ());
    true
}

pub fn get_blacklist_entry(env: &Env, address: &Address) -> Option<BlacklistEntry> {
    env.storage()
        .persistent()
        .get(&ComplianceKey::Blacklist(address.clone()))
}

pub fn is_blacklisted(env: &Env, address: &Address) -> bool {
    env.storage()
        .persistent()
        .has(&ComplianceKey::Blacklist(address.clone()))
}

pub fn allowlist_address(env: &Env, address: &Address) {
    persist(
        env,
        &ComplianceKey::Allowlist(address.clone()),
        &env.ledger().timestamp(),
    );
    env.events()
        .publish((ALLOWLIST_ADDED_TOPIC, address.clone()), ());
}

/// Remove `address` from the allowlist. Returns `false` if it was not listed.
pub fn remove_from_allowlist(env: &Env, address: &Address) -> bool {
    let key = ComplianceKey::Allowlist(address.clone());
    if !env.storage().persistent().has(&key) {
        return false;
    }
    env.storage().persistent().remove(&key);
    env.events()
        .publish((ALLOWLIST_REMOVED_TOPIC, address.clone()), ());
    true
}

pub fn is_allowlisted(env: &Env, address: &Address) -> bool {
    env.storage()
        .persistent()
        .has(&ComplianceKey::Allowlist(address.clone()))
}

/// Enable or disable allowlist mode. While enabled, only allowlisted
/// addresses are compliant.
pub fn set_allowlist_enforced(env: &Env, enforced: bool) {
    persist(env, &ComplianceKey::AllowlistEnforced, &enforced);
    env.events().publish((ALLOWLIST_MODE_TOPIC,), enforced);
}

pub fn is_allowlist_enforced(env: &Env) -> bool {
    env.storage()
        .persistent()
        .get(&ComplianceKey::AllowlistEnforced)
        .unwrap_or(false)
}

/// Returns the reason `address` is not compliant, if any. The blacklist
/// always takes precedence over the allowlist.
pub fn check_address(env: &Env, address: &Address) -> Result<(), ContractError> {
    if is_blacklisted(env, address) {
        return Err(ContractError::AddressBlacklisted);
    }
    if is_allowlist_enforced(env) && !is_allowlisted(env, address) {
        return Err(ContractError::AddressNotAllowlisted);
    }
    Ok(())
}

/// An address is compliant when it is not blacklisted and, if allowlist mode
/// is enforced, it is allowlisted.
pub fn is_address_compliant(env: &Env, address: &Address) -> bool {
    check_address(env, address).is_ok()
}

// ── KYC verification (#548) ──────────────────────────────────────────────────

pub fn set_kyc_provider(env: &Env, provider: &Address) {
    persist(env, &ComplianceKey::KycProvider, provider);
}

pub fn get_kyc_provider(env: &Env) -> Option<Address> {
    env.storage().persistent().get(&ComplianceKey::KycProvider)
}

fn require_kyc_provider(env: &Env) -> Result<Address, ContractError> {
    let provider = get_kyc_provider(env).ok_or(ContractError::KycProviderNotSet)?;
    provider.require_auth();
    Ok(provider)
}

/// Record the provider's KYC attestation for `address`. Requires the
/// registered provider's auth. Returns `false` (and stores nothing) when the
/// attestation is unusable: zero level, zero reference, or already expired.
pub fn verify_kyc(
    env: &Env,
    address: &Address,
    kyc_data: KycData,
) -> Result<bool, ContractError> {
    let provider = require_kyc_provider(env)?;
    let now = env.ledger().timestamp();
    let empty_ref = BytesN::from_array(env, &[0u8; 32]);
    if kyc_data.level == 0 || kyc_data.expires_at <= now || kyc_data.provider_reference == empty_ref
    {
        env.events()
            .publish((KYC_REJECTED_TOPIC, address.clone()), kyc_data.level);
        return Ok(false);
    }
    let record = KycRecord {
        status: KycStatus::Verified,
        level: kyc_data.level,
        provider_reference: kyc_data.provider_reference,
        verified_by: provider,
        verified_at: now,
        expires_at: kyc_data.expires_at,
    };
    persist(env, &ComplianceKey::Kyc(address.clone()), &record);
    env.events().publish(
        (KYC_VERIFIED_TOPIC, address.clone()),
        (record.level, record.expires_at),
    );
    Ok(true)
}

/// Revoke an address' KYC verification. Requires the registered provider's
/// auth; the admin path is handled by the wrapper in `lib.rs`.
pub fn revoke_kyc(env: &Env, address: &Address) -> Result<(), ContractError> {
    let key = ComplianceKey::Kyc(address.clone());
    let mut record: KycRecord = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(ContractError::KycNotFound)?;
    record.status = KycStatus::Revoked;
    persist(env, &key, &record);
    env.events()
        .publish((KYC_REVOKED_TOPIC, address.clone()), ());
    Ok(())
}

pub fn revoke_kyc_as_provider(env: &Env, address: &Address) -> Result<(), ContractError> {
    require_kyc_provider(env)?;
    revoke_kyc(env, address)
}

pub fn get_kyc_record(env: &Env, address: &Address) -> Option<KycRecord> {
    env.storage()
        .persistent()
        .get(&ComplianceKey::Kyc(address.clone()))
}

/// True when `address` holds a non-revoked, unexpired KYC verification.
pub fn is_kyc_verified(env: &Env, address: &Address) -> bool {
    match get_kyc_record(env, address) {
        Some(r) => r.status == KycStatus::Verified && r.expires_at > env.ledger().timestamp(),
        None => false,
    }
}

pub fn set_kyc_high_value_threshold(env: &Env, amount: i128) -> Result<(), ContractError> {
    if amount < 0 {
        return Err(ContractError::InvalidAmount);
    }
    persist(env, &ComplianceKey::KycHighValueThreshold, &amount);
    Ok(())
}

pub fn get_kyc_high_value_threshold(env: &Env) -> i128 {
    env.storage()
        .persistent()
        .get(&ComplianceKey::KycHighValueThreshold)
        .unwrap_or(0)
}

/// Fails with `KycRequired` when `amount` meets the high-value threshold and
/// `address` is not KYC verified.
pub fn check_kyc_for_amount(
    env: &Env,
    address: &Address,
    amount: i128,
) -> Result<(), ContractError> {
    let threshold = get_kyc_high_value_threshold(env);
    if threshold > 0 && amount >= threshold && !is_kyc_verified(env, address) {
        return Err(ContractError::KycRequired);
    }
    Ok(())
}

// ── Threshold monitoring (#550) ──────────────────────────────────────────────

pub fn set_threshold_config(env: &Env, config: &ThresholdConfig) -> Result<(), ContractError> {
    if config.single_tx_threshold < 0
        || config.cumulative_threshold < 0
        || (config.cumulative_threshold > 0 && config.window_seconds == 0)
    {
        return Err(ContractError::InvalidConfig);
    }
    persist(env, &ComplianceKey::ThresholdConfig, config);
    Ok(())
}

pub fn get_threshold_config(env: &Env) -> Option<ThresholdConfig> {
    env.storage().persistent().get(&ComplianceKey::ThresholdConfig)
}

pub fn get_cumulative_volume(env: &Env, address: &Address) -> Option<CumulativeVolume> {
    env.storage()
        .persistent()
        .get(&ComplianceKey::Cumulative(address.clone()))
}

fn raise_alert(
    env: &Env,
    kind: AlertKind,
    address: &Address,
    tx_id: u64,
    amount: i128,
    cumulative: i128,
    threshold: i128,
) {
    let id = next_counter(env, &ComplianceKey::AlertCount);
    let alert = ComplianceAlert {
        id,
        kind,
        address: address.clone(),
        tx_id,
        amount,
        cumulative,
        threshold,
        created_at: env.ledger().timestamp(),
        acknowledged: false,
    };
    persist(env, &ComplianceKey::Alert(id), &alert);
    env.events().publish(
        (COMPLIANCE_ALERT_TOPIC, address.clone()),
        (id, kind, tx_id, amount, threshold),
    );
}

/// Add `amount` to `address`' rolling volume and raise alerts for any
/// threshold crossed. Returns whether the transaction is reportable.
fn monitor_thresholds(env: &Env, address: &Address, tx_id: u64, amount: i128) -> bool {
    let Some(config) = get_threshold_config(env) else {
        return false;
    };
    let now = env.ledger().timestamp();
    let mut flagged = false;

    if config.single_tx_threshold > 0 && amount >= config.single_tx_threshold {
        flagged = true;
        raise_alert(
            env,
            AlertKind::SingleTransaction,
            address,
            tx_id,
            amount,
            amount,
            config.single_tx_threshold,
        );
    }

    if config.cumulative_threshold > 0 {
        let key = ComplianceKey::Cumulative(address.clone());
        let mut volume = match get_cumulative_volume(env, address) {
            Some(v) if now < v.window_start.saturating_add(config.window_seconds) => v,
            _ => CumulativeVolume {
                window_start: now,
                total: 0,
                alerted: false,
            },
        };
        volume.total = volume.total.saturating_add(amount);
        if !volume.alerted && volume.total >= config.cumulative_threshold {
            volume.alerted = true;
            flagged = true;
            raise_alert(
                env,
                AlertKind::Cumulative,
                address,
                tx_id,
                amount,
                volume.total,
                config.cumulative_threshold,
            );
        }
        persist(env, &key, &volume);
    }

    flagged
}

pub fn get_alert(env: &Env, alert_id: u64) -> Option<ComplianceAlert> {
    env.storage().persistent().get(&ComplianceKey::Alert(alert_id))
}

pub fn get_alert_count(env: &Env) -> u64 {
    env.storage()
        .persistent()
        .get(&ComplianceKey::AlertCount)
        .unwrap_or(0)
}

pub fn acknowledge_alert(env: &Env, alert_id: u64) -> Result<(), ContractError> {
    let key = ComplianceKey::Alert(alert_id);
    let mut alert: ComplianceAlert = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(ContractError::AlertNotFound)?;
    alert.acknowledged = true;
    persist(env, &key, &alert);
    Ok(())
}

// ── Transaction reporting (#549) ─────────────────────────────────────────────

/// Append a transaction to the compliance log and run threshold monitoring
/// against `monitored` (the end user party of the transfer). Returns the new
/// transaction id.
pub fn record_transaction(
    env: &Env,
    kind: TxKind,
    vault_id: u64,
    from: &Address,
    to: &Address,
    token: &Address,
    amount: i128,
    monitored: &Address,
) -> u64 {
    let id = next_counter(env, &ComplianceKey::TxCount);
    let flagged = monitor_thresholds(env, monitored, id, amount);
    let record = TransactionRecord {
        id,
        kind,
        vault_id,
        from: from.clone(),
        to: to.clone(),
        token: token.clone(),
        amount,
        timestamp: env.ledger().timestamp(),
        flagged,
    };
    persist(env, &ComplianceKey::Tx(id), &record);
    env.events()
        .publish((TX_RECORDED_TOPIC, vault_id), (id, kind, amount, flagged));
    id
}

pub fn get_transaction(env: &Env, tx_id: u64) -> Option<TransactionRecord> {
    env.storage().persistent().get(&ComplianceKey::Tx(tx_id))
}

pub fn get_transaction_count(env: &Env) -> u64 {
    env.storage()
        .persistent()
        .get(&ComplianceKey::TxCount)
        .unwrap_or(0)
}

fn tx_timestamp(env: &Env, id: u64) -> u64 {
    get_transaction(env, id).map_or(u64::MAX, |r| r.timestamp)
}

/// First transaction id whose timestamp is `>= start`. Ledger timestamps are
/// non-decreasing, so ids are ordered by time and a binary search applies.
fn lower_bound(env: &Env, start: u64) -> u64 {
    let (mut lo, mut hi) = (0u64, get_transaction_count(env));
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if tx_timestamp(env, mid) < start {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

fn validate_range(start_date: u64, end_date: u64) -> Result<(), ContractError> {
    if start_date > end_date {
        return Err(ContractError::InvalidReportRange);
    }
    Ok(())
}

/// Number of transactions with `start_date <= timestamp <= end_date`.
pub fn count_transactions(
    env: &Env,
    start_date: u64,
    end_date: u64,
) -> Result<u64, ContractError> {
    validate_range(start_date, end_date)?;
    let first = lower_bound(env, start_date);
    let after = if end_date == u64::MAX {
        get_transaction_count(env)
    } else {
        lower_bound(env, end_date + 1)
    };
    Ok(after.saturating_sub(first))
}

fn push_str(buf: &mut Bytes, s: &str) {
    buf.extend_from_slice(s.as_bytes());
}

fn push_u64(buf: &mut Bytes, mut v: u64) {
    let mut digits = [0u8; 20];
    let mut i = digits.len();
    loop {
        i -= 1;
        digits[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    buf.extend_from_slice(&digits[i..]);
}

fn push_i128(buf: &mut Bytes, v: i128) {
    let mut digits = [0u8; 40];
    let mut i = digits.len();
    let mut u = v.unsigned_abs();
    loop {
        i -= 1;
        digits[i] = b'0' + (u % 10) as u8;
        u /= 10;
        if u == 0 {
            break;
        }
    }
    if v < 0 {
        i -= 1;
        digits[i] = b'-';
    }
    buf.extend_from_slice(&digits[i..]);
}

/// Addresses are exported as the hex encoding of their XDR `ScAddress`, which
/// is deterministic and decodes losslessly to a strkey off-chain.
fn push_address(env: &Env, buf: &mut Bytes, address: &Address) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let xdr = address.clone().to_xdr(env);
    for b in xdr.iter() {
        buf.push_back(HEX[(b >> 4) as usize]);
        buf.push_back(HEX[(b & 0x0f) as usize]);
    }
}

fn push_row(env: &Env, buf: &mut Bytes, r: &TransactionRecord) {
    push_u64(buf, r.id);
    push_str(buf, ",");
    push_u64(buf, r.timestamp);
    push_str(buf, ",");
    push_str(
        buf,
        match r.kind {
            TxKind::Deposit => "deposit",
            TxKind::Withdrawal => "withdrawal",
        },
    );
    push_str(buf, ",");
    push_u64(buf, r.vault_id);
    push_str(buf, ",");
    push_address(env, buf, &r.from);
    push_str(buf, ",");
    push_address(env, buf, &r.to);
    push_str(buf, ",");
    push_address(env, buf, &r.token);
    push_str(buf, ",");
    push_i128(buf, r.amount);
    push_str(buf, if r.flagged { ",true\n" } else { ",false\n" });
}

/// Export transactions with `start_date <= timestamp <= end_date` (unix
/// seconds, inclusive) as CSV, up to `MAX_EXPORT_ROWS` rows. Returns the CSV
/// and the number of data rows written.
pub fn export_transactions(
    env: &Env,
    start_date: u64,
    end_date: u64,
) -> Result<(Bytes, u32), ContractError> {
    validate_range(start_date, end_date)?;
    let mut csv = Bytes::new(env);
    push_str(&mut csv, CSV_HEADER);
    let count = get_transaction_count(env);
    let mut id = lower_bound(env, start_date);
    let mut rows = 0u32;
    while id < count && rows < MAX_EXPORT_ROWS {
        let Some(record) = get_transaction(env, id) else {
            break;
        };
        if record.timestamp > end_date {
            break;
        }
        push_row(env, &mut csv, &record);
        rows += 1;
        id += 1;
    }
    Ok((csv, rows))
}

/// SHA-256 digest of the CSV `export_transactions` returns for the range.
pub fn report_digest(
    env: &Env,
    start_date: u64,
    end_date: u64,
) -> Result<BytesN<32>, ContractError> {
    let (csv, _) = export_transactions(env, start_date, end_date)?;
    Ok(env.crypto().sha256(&csv).into())
}

pub fn set_report_signer(env: &Env, public_key: &BytesN<32>) {
    persist(env, &ComplianceKey::ReportSigner, public_key);
}

pub fn get_report_signer(env: &Env) -> Option<BytesN<32>> {
    env.storage().persistent().get(&ComplianceKey::ReportSigner)
}

/// Verify `signature` by the registered report signer over the digest of the
/// report for the range and store it as a signed report. The ed25519 check
/// traps the invocation if the signature is invalid.
pub fn sign_report(
    env: &Env,
    start_date: u64,
    end_date: u64,
    signature: BytesN<64>,
) -> Result<u64, ContractError> {
    let signer = get_report_signer(env).ok_or(ContractError::ReportSignerNotSet)?;
    let (csv, row_count) = export_transactions(env, start_date, end_date)?;
    let report_hash: BytesN<32> = env.crypto().sha256(&csv).into();
    env.crypto()
        .ed25519_verify(&signer, &Bytes::from(report_hash.clone()), &signature);

    let id = next_counter(env, &ComplianceKey::ReportCount);
    let report = SignedReport {
        id,
        start_date,
        end_date,
        row_count,
        report_hash: report_hash.clone(),
        signer,
        signature,
        signed_at: env.ledger().timestamp(),
    };
    persist(env, &ComplianceKey::SignedReport(id), &report);
    env.events()
        .publish((REPORT_SIGNED_TOPIC, id), (start_date, end_date, report_hash));
    Ok(id)
}

pub fn get_signed_report(env: &Env, report_id: u64) -> Option<SignedReport> {
    env.storage()
        .persistent()
        .get(&ComplianceKey::SignedReport(report_id))
}
