/// Compliance subsystem for the TTL vault contract.
///
/// Covers four related features:
///
/// - **Issue #552 — Compliance audit trail.** Every compliance-related action
///   (AML checks, KYC verifications, blacklist decisions, SAR filings, report
///   generation/signing and regulatory changes) is appended to a per-address
///   audit trail, readable via `get_compliance_audit_trail`.
/// - **Issue #553 — Suspicious Activity Reporting (SAR).** Admins file SARs
///   against a vault (`file_sar`), track their filing status through a
///   forward-only state machine, and generate a hashed SAR report for
///   submission to the regulator. AML checks that exceed the configured risk
///   threshold automatically file a SAR when a vault is referenced.
/// - **Issue #554 — Compliance report generation.** Compliance activity and
///   vault transactions are aggregated into daily buckets. A report over a
///   period sums those buckets, is content-hashed, and can be signed with an
///   ed25519 key whose signature is verified on-chain.
/// - **Issue #555 — Regulatory change management.** Regulatory requirements are
///   versioned; every amendment keeps the previous version in history, tracks
///   effective and implementation dates, and publishes a change notice that
///   users can list and acknowledge.
///
/// Raw PII is never stored: subjects are addressed by their Stellar `Address`
/// and free-form fields (`reason`, `provider`, `description`) are expected to
/// be short codes or hashes of off-chain documents.
///
/// Authorization is enforced by the wrappers in `lib.rs`; functions in this
/// module assume the caller has already been authorized.
use soroban_sdk::{
    contracttype, symbol_short, xdr::ToXdr, Address, Bytes, BytesN, Env, Symbol, Vec,
};

use crate::ContractError;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum audit entries retained per address; oldest entries are dropped
/// first. Full history remains available from emitted `cmp_aud` events.
pub const MAX_AUDIT_TRAIL_ENTRIES: u32 = 200;

/// Seconds per daily aggregation bucket.
pub const SECONDS_PER_DAY: u64 = 86_400;

/// Maximum span of a compliance report period (one leap year of buckets).
pub const MAX_REPORT_PERIOD_DAYS: u64 = 366;

/// Default AML risk score (0–100) at or above which a check is flagged.
pub const DEFAULT_AML_RISK_THRESHOLD: u32 = 70;

/// Maximum AML risk score.
pub const MAX_AML_RISK_SCORE: u32 = 100;

/// Maximum length (bytes) of a SAR reason.
pub const MAX_SAR_REASON_LEN: u32 = 1_024;

/// Maximum length (bytes) of short labels (provider, code, jurisdiction,
/// submission references, blacklist reasons).
pub const MAX_LABEL_LEN: u32 = 128;

/// Maximum length (bytes) of a regulatory requirement description.
pub const MAX_DESCRIPTION_LEN: u32 = 1_024;

/// Maximum number of regulatory requirements tracked.
pub const MAX_REQUIREMENTS: u32 = 500;

/// Maximum notices returned by a single notice query.
pub const MAX_NOTICE_PAGE: u32 = 50;

/// Number of most-recent audit entries embedded in a SAR report.
pub const SAR_REPORT_AUDIT_ENTRIES: u32 = 20;

/// Reason recorded on SARs filed automatically by a flagged AML check.
const AUTO_SAR_REASON: &[u8] = b"auto: AML risk threshold exceeded";

const PERSISTENT_TTL_THRESHOLD: u32 = crate::VAULT_TTL_THRESHOLD;
const PERSISTENT_TTL_LEDGERS: u32 = crate::VAULT_TTL_LEDGERS;

// ── Event topics ─────────────────────────────────────────────────────────────

pub const COMPLIANCE_AUDIT_TOPIC: Symbol = symbol_short!("cmp_aud");
pub const AML_CHECK_TOPIC: Symbol = symbol_short!("aml_chk");
pub const KYC_UPDATED_TOPIC: Symbol = symbol_short!("kyc_upd");
pub const BLACKLIST_TOPIC: Symbol = symbol_short!("blacklist");
pub const SAR_FILED_TOPIC: Symbol = symbol_short!("sar_file");
pub const SAR_STATUS_TOPIC: Symbol = symbol_short!("sar_stat");
pub const REPORT_GENERATED_TOPIC: Symbol = symbol_short!("rpt_gen");
pub const REPORT_SIGNED_TOPIC: Symbol = symbol_short!("rpt_sign");
pub const REGULATORY_CHANGE_TOPIC: Symbol = symbol_short!("reg_chg");
pub const REGULATORY_ACK_TOPIC: Symbol = symbol_short!("reg_ack");

// ── Storage keys ─────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
pub enum ComplianceKey {
    /// Per-address audit trail (`Vec<ComplianceAuditEntry>`).
    AuditTrail(Address),
    /// Monotonic audit entry id counter.
    AuditSeq,
    /// Current KYC record per address.
    Kyc(Address),
    /// Running totals of addresses per KYC status.
    KycTotals,
    /// Most recent AML check per address.
    LastAml(Address),
    /// AML flagging threshold (risk score).
    AmlThreshold,
    /// Blacklist decision per address.
    Blacklist(Address),
    /// Daily aggregated compliance statistics, keyed by day index.
    Daily(u64),
    /// SAR id counter.
    SarSeq,
    /// SAR record by id.
    Sar(u64),
    /// SAR ids filed against a vault.
    VaultSars(u64),
    /// Compliance report id counter.
    ReportSeq,
    /// Compliance report by id.
    Report(u64),
    /// Regulatory requirement id counter.
    RequirementSeq,
    /// Current version of a regulatory requirement.
    Requirement(u64),
    /// Historical version `(requirement_id, version)`.
    RequirementVersion(u64, u32),
    /// All requirement ids.
    RequirementIds,
    /// Regulatory change notice id counter.
    NoticeSeq,
    /// Regulatory change notice by id.
    Notice(u64),
    /// Highest notice id acknowledged by a user.
    NoticeAck(Address),
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// Kinds of compliance actions recorded in the audit trail.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComplianceAction {
    AmlCheck,
    KycVerification,
    BlacklistAdded,
    BlacklistRemoved,
    SarFiled,
    SarStatusUpdated,
    ReportGenerated,
    ReportSigned,
    RequirementChanged,
    RegulatoryAcknowledged,
}

/// One entry in a compliance audit trail.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComplianceAuditEntry {
    pub id: u64,
    pub action: ComplianceAction,
    /// The address the action concerns.
    pub subject: Address,
    /// The address that performed the action.
    pub actor: Address,
    /// Vault involved, if any.
    pub vault_id: Option<u64>,
    /// Related record id (SAR id, report id, requirement id, notice id), 0 if none.
    pub reference_id: u64,
    /// Action-specific detail (provider, reason, status code…).
    pub detail: Bytes,
    pub timestamp: u64,
    pub ledger: u32,
}

#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KycStatus {
    Unverified,
    Pending,
    Verified,
    Rejected,
    Expired,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KycRecord {
    pub address: Address,
    pub status: KycStatus,
    pub provider: Bytes,
    /// Timestamp of the last transition into `Verified` (0 if never verified).
    pub verified_at: u64,
    /// Verification expiry timestamp (0 = no expiry).
    pub expires_at: u64,
    pub updated_at: u64,
}

/// Number of addresses currently in each KYC status.
#[contracttype]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct KycTotals {
    pub pending: u32,
    pub verified: u32,
    pub rejected: u32,
    pub expired: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AmlCheckRecord {
    pub address: Address,
    pub vault_id: Option<u64>,
    pub risk_score: u32,
    pub flagged: bool,
    pub provider: Bytes,
    pub checked_at: u64,
    /// SAR automatically filed because of this check, if any.
    pub sar_id: Option<u64>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlacklistRecord {
    pub address: Address,
    pub blacklisted: bool,
    pub reason: Bytes,
    pub decided_by: Address,
    pub decided_at: u64,
}

/// Compliance activity aggregated for a single day.
#[contracttype]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DailyComplianceStats {
    pub aml_checks: u32,
    pub aml_flagged: u32,
    pub kyc_verified: u32,
    pub kyc_rejected: u32,
    pub blacklist_added: u32,
    pub blacklist_removed: u32,
    pub sars_filed: u32,
    pub deposit_count: u32,
    pub withdrawal_count: u32,
    pub deposit_volume: i128,
    pub withdrawal_volume: i128,
}

// ── SAR types ────────────────────────────────────────────────────────────────

/// SAR filing status. Transitions are forward-only:
/// `Filed → UnderReview → Submitted → Closed` (steps may be skipped, never
/// reversed); `Closed` is terminal.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub enum SarStatus {
    Filed,
    UnderReview,
    Submitted,
    Closed,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SarRecord {
    pub id: u64,
    pub vault_id: u64,
    /// Vault owner at filing time.
    pub subject: Address,
    pub reason: Bytes,
    pub status: SarStatus,
    pub filed_by: Address,
    pub filed_at: u64,
    pub updated_at: u64,
    /// Regulator-assigned reference once submitted (empty until then).
    pub submission_ref: Bytes,
    /// `true` when filed automatically by a flagged AML check.
    pub auto_generated: bool,
}

/// SAR package prepared for submission to a regulator.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SarReport {
    pub sar: SarRecord,
    pub vault_owner: Address,
    pub vault_balance: i128,
    pub vault_last_check_in: u64,
    pub vault_check_in_interval: u64,
    pub subject_kyc: KycStatus,
    pub subject_blacklisted: bool,
    pub subject_last_aml: Option<AmlCheckRecord>,
    /// Most recent compliance audit entries for the subject.
    pub recent_audit: Vec<ComplianceAuditEntry>,
    pub generated_at: u64,
    /// sha256 of the XDR of this report with `digest` zeroed.
    pub digest: BytesN<32>,
}

// ── Report types ─────────────────────────────────────────────────────────────

/// Reporting period `[start, end)` in ledger seconds. Aggregation is at daily
/// granularity: every day bucket overlapping the period is included.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompliancePeriod {
    pub start: u64,
    pub end: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComplianceReport {
    pub id: u64,
    pub period: CompliancePeriod,
    pub aml_checks: u32,
    pub aml_flagged: u32,
    pub kyc_verified: u32,
    pub kyc_rejected: u32,
    /// Snapshot of KYC status totals at generation time.
    pub kyc_totals: KycTotals,
    pub blacklist_added: u32,
    pub blacklist_removed: u32,
    pub sars_filed: u32,
    pub deposit_count: u32,
    pub withdrawal_count: u32,
    pub deposit_volume: i128,
    pub withdrawal_volume: i128,
    pub generated_at: u64,
    pub generated_by: Address,
    /// sha256 over the report content (see `report_digest`).
    pub digest: BytesN<32>,
    /// ed25519 public key that signed `digest`, once signed.
    pub signer: Option<BytesN<32>>,
    pub signature: Option<BytesN<64>>,
    pub signed_at: u64,
}

/// Result of `verify_compliance_report`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportVerification {
    /// Stored digest matches the recomputed content digest.
    pub digest_valid: bool,
    /// Report carries a signature that was verified against `digest`.
    pub signed: bool,
    pub signer: Option<BytesN<32>>,
}

// ── Regulatory change management types ───────────────────────────────────────

#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequirementStatus {
    Active,
    Superseded,
    Retired,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegulatoryRequirement {
    pub id: u64,
    /// Short rule code, e.g. `AML-5`.
    pub code: Bytes,
    /// Jurisdiction label, e.g. `EU`, `US-FINCEN`.
    pub jurisdiction: Bytes,
    pub description: Bytes,
    /// Hash of the full rule text / compliance rule set for this version.
    pub rule_hash: BytesN<32>,
    pub version: u32,
    pub status: RequirementStatus,
    /// Date from which this version is mandatory.
    pub effective_date: u64,
    /// Date this version was implemented by the protocol, if it has been.
    pub implemented_at: Option<u64>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegulatoryChangeKind {
    Introduced,
    Amended,
    Implemented,
    Retired,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegulatoryChangeNotice {
    pub id: u64,
    pub requirement_id: u64,
    pub version: u32,
    pub kind: RegulatoryChangeKind,
    pub code: Bytes,
    pub effective_date: u64,
    pub published_at: u64,
}

// ── Storage helpers ──────────────────────────────────────────────────────────

fn persist<V: soroban_sdk::IntoVal<Env, soroban_sdk::Val>>(
    env: &Env,
    key: &ComplianceKey,
    value: &V,
) {
    let storage = env.storage().persistent();
    storage.set(key, value);
    storage.extend_ttl(key, PERSISTENT_TTL_THRESHOLD, PERSISTENT_TTL_LEDGERS);
}

fn next_id(env: &Env, key: &ComplianceKey) -> u64 {
    let id: u64 = env.storage().instance().get(key).unwrap_or(0) + 1;
    env.storage().instance().set(key, &id);
    id
}

fn check_label(label: &Bytes, allow_empty: bool) -> Result<(), ContractError> {
    if (!allow_empty && label.is_empty()) || label.len() > MAX_LABEL_LEN {
        return Err(ContractError::InvalidComplianceInput);
    }
    Ok(())
}

fn day_of(timestamp: u64) -> u64 {
    timestamp / SECONDS_PER_DAY
}

fn update_daily(env: &Env, f: impl FnOnce(&mut DailyComplianceStats)) {
    let key = ComplianceKey::Daily(day_of(env.ledger().timestamp()));
    let mut stats: DailyComplianceStats = env.storage().persistent().get(&key).unwrap_or_default();
    f(&mut stats);
    persist(env, &key, &stats);
}

// ── Audit trail (Issue #552) ─────────────────────────────────────────────────

/// Append an entry to `subject`'s compliance audit trail and emit an event.
pub fn log_action(
    env: &Env,
    action: ComplianceAction,
    subject: &Address,
    actor: &Address,
    vault_id: Option<u64>,
    reference_id: u64,
    detail: Bytes,
) -> u64 {
    let id = next_id(env, &ComplianceKey::AuditSeq);
    let entry = ComplianceAuditEntry {
        id,
        action,
        subject: subject.clone(),
        actor: actor.clone(),
        vault_id,
        reference_id,
        detail,
        timestamp: env.ledger().timestamp(),
        ledger: env.ledger().sequence(),
    };
    let key = ComplianceKey::AuditTrail(subject.clone());
    let mut trail: Vec<ComplianceAuditEntry> = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| Vec::new(env));
    while trail.len() >= MAX_AUDIT_TRAIL_ENTRIES {
        trail.pop_front();
    }
    trail.push_back(entry);
    persist(env, &key, &trail);
    env.events()
        .publish((COMPLIANCE_AUDIT_TOPIC, subject.clone()), (id, action));
    id
}

pub fn get_audit_trail(env: &Env, address: &Address) -> Vec<ComplianceAuditEntry> {
    env.storage()
        .persistent()
        .get(&ComplianceKey::AuditTrail(address.clone()))
        .unwrap_or_else(|| Vec::new(env))
}

// ── AML / KYC / blacklist ────────────────────────────────────────────────────

pub fn get_aml_threshold(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&ComplianceKey::AmlThreshold)
        .unwrap_or(DEFAULT_AML_RISK_THRESHOLD)
}

pub fn set_aml_threshold(env: &Env, threshold: u32) -> Result<(), ContractError> {
    if threshold == 0 || threshold > MAX_AML_RISK_SCORE {
        return Err(ContractError::InvalidComplianceInput);
    }
    env.storage()
        .instance()
        .set(&ComplianceKey::AmlThreshold, &threshold);
    Ok(())
}

/// Record an AML screening result. A check whose `risk_score` meets the
/// threshold is flagged; when it references a vault, a SAR is filed
/// automatically against that vault.
pub fn record_aml_check(
    env: &Env,
    actor: &Address,
    address: &Address,
    vault: Option<(u64, Address)>,
    risk_score: u32,
    provider: Bytes,
) -> Result<AmlCheckRecord, ContractError> {
    if risk_score > MAX_AML_RISK_SCORE {
        return Err(ContractError::InvalidComplianceInput);
    }
    check_label(&provider, false)?;

    let flagged = risk_score >= get_aml_threshold(env);
    let vault_id = vault.as_ref().map(|(id, _)| *id);

    log_action(
        env,
        ComplianceAction::AmlCheck,
        address,
        actor,
        vault_id,
        u64::from(risk_score),
        provider.clone(),
    );
    update_daily(env, |s| {
        s.aml_checks += 1;
        if flagged {
            s.aml_flagged += 1;
        }
    });

    let sar_id = match (&vault, flagged) {
        (Some((id, owner)), true) => Some(file_sar(
            env,
            actor,
            *id,
            owner,
            Bytes::from_slice(env, AUTO_SAR_REASON),
            true,
        )?),
        _ => None,
    };

    let record = AmlCheckRecord {
        address: address.clone(),
        vault_id,
        risk_score,
        flagged,
        provider,
        checked_at: env.ledger().timestamp(),
        sar_id,
    };
    persist(env, &ComplianceKey::LastAml(address.clone()), &record);
    env.events()
        .publish((AML_CHECK_TOPIC, address.clone()), (risk_score, flagged));
    Ok(record)
}

pub fn get_last_aml_check(env: &Env, address: &Address) -> Option<AmlCheckRecord> {
    env.storage()
        .persistent()
        .get(&ComplianceKey::LastAml(address.clone()))
}

fn adjust_kyc_totals(totals: &mut KycTotals, status: KycStatus, increment: bool) {
    let slot = match status {
        KycStatus::Pending => &mut totals.pending,
        KycStatus::Verified => &mut totals.verified,
        KycStatus::Rejected => &mut totals.rejected,
        KycStatus::Expired => &mut totals.expired,
        KycStatus::Unverified => return,
    };
    *slot = if increment {
        slot.saturating_add(1)
    } else {
        slot.saturating_sub(1)
    };
}

pub fn record_kyc_verification(
    env: &Env,
    actor: &Address,
    address: &Address,
    status: KycStatus,
    provider: Bytes,
    expires_at: u64,
) -> Result<KycRecord, ContractError> {
    check_label(&provider, false)?;
    let now = env.ledger().timestamp();
    if expires_at != 0 && expires_at <= now {
        return Err(ContractError::InvalidComplianceInput);
    }

    let key = ComplianceKey::Kyc(address.clone());
    let previous: Option<KycRecord> = env.storage().persistent().get(&key);
    let previous_status = previous
        .as_ref()
        .map_or(KycStatus::Unverified, |r| r.status);

    let mut totals: KycTotals = env
        .storage()
        .instance()
        .get(&ComplianceKey::KycTotals)
        .unwrap_or_default();
    adjust_kyc_totals(&mut totals, previous_status, false);
    adjust_kyc_totals(&mut totals, status, true);
    env.storage()
        .instance()
        .set(&ComplianceKey::KycTotals, &totals);

    let verified_at = if status == KycStatus::Verified {
        now
    } else {
        previous.as_ref().map_or(0, |r| r.verified_at)
    };
    let record = KycRecord {
        address: address.clone(),
        status,
        provider: provider.clone(),
        verified_at,
        expires_at,
        updated_at: now,
    };
    persist(env, &key, &record);

    log_action(
        env,
        ComplianceAction::KycVerification,
        address,
        actor,
        None,
        status as u64,
        provider,
    );
    update_daily(env, |s| match status {
        KycStatus::Verified => s.kyc_verified += 1,
        KycStatus::Rejected => s.kyc_rejected += 1,
        _ => {}
    });
    env.events()
        .publish((KYC_UPDATED_TOPIC, address.clone()), status);
    Ok(record)
}

/// Current KYC record, with `Verified` reported as `Expired` once the
/// verification's expiry has passed.
pub fn get_kyc_record(env: &Env, address: &Address) -> KycRecord {
    let mut record: KycRecord = env
        .storage()
        .persistent()
        .get(&ComplianceKey::Kyc(address.clone()))
        .unwrap_or_else(|| KycRecord {
            address: address.clone(),
            status: KycStatus::Unverified,
            provider: Bytes::new(env),
            verified_at: 0,
            expires_at: 0,
            updated_at: 0,
        });
    if record.status == KycStatus::Verified
        && record.expires_at != 0
        && env.ledger().timestamp() >= record.expires_at
    {
        record.status = KycStatus::Expired;
    }
    record
}

pub fn set_blacklist(
    env: &Env,
    actor: &Address,
    address: &Address,
    blacklisted: bool,
    reason: Bytes,
) -> Result<BlacklistRecord, ContractError> {
    check_label(&reason, false)?;
    let was_blacklisted = is_blacklisted(env, address);
    let record = BlacklistRecord {
        address: address.clone(),
        blacklisted,
        reason: reason.clone(),
        decided_by: actor.clone(),
        decided_at: env.ledger().timestamp(),
    };
    persist(env, &ComplianceKey::Blacklist(address.clone()), &record);

    let action = if blacklisted {
        ComplianceAction::BlacklistAdded
    } else {
        ComplianceAction::BlacklistRemoved
    };
    log_action(env, action, address, actor, None, 0, reason);
    if blacklisted != was_blacklisted {
        update_daily(env, |s| {
            if blacklisted {
                s.blacklist_added += 1;
            } else {
                s.blacklist_removed += 1;
            }
        });
    }
    env.events()
        .publish((BLACKLIST_TOPIC, address.clone()), blacklisted);
    Ok(record)
}

pub fn get_blacklist_record(env: &Env, address: &Address) -> Option<BlacklistRecord> {
    env.storage()
        .persistent()
        .get(&ComplianceKey::Blacklist(address.clone()))
}

pub fn is_blacklisted(env: &Env, address: &Address) -> bool {
    get_blacklist_record(env, address).is_some_and(|r| r.blacklisted)
}

/// Record a completed vault transaction in the daily compliance aggregates.
pub fn record_transaction(env: &Env, amount: i128, is_deposit: bool) {
    update_daily(env, |s| {
        if is_deposit {
            s.deposit_count += 1;
            s.deposit_volume = s.deposit_volume.saturating_add(amount);
        } else {
            s.withdrawal_count += 1;
            s.withdrawal_volume = s.withdrawal_volume.saturating_add(amount);
        }
    });
}

// ── SAR (Issue #553) ─────────────────────────────────────────────────────────

/// File a SAR against `vault_id`, whose owner is `subject`.
pub fn file_sar(
    env: &Env,
    actor: &Address,
    vault_id: u64,
    subject: &Address,
    reason: Bytes,
    auto_generated: bool,
) -> Result<u64, ContractError> {
    if reason.is_empty() || reason.len() > MAX_SAR_REASON_LEN {
        return Err(ContractError::InvalidSarReason);
    }
    let id = next_id(env, &ComplianceKey::SarSeq);
    let now = env.ledger().timestamp();
    let sar = SarRecord {
        id,
        vault_id,
        subject: subject.clone(),
        reason: reason.clone(),
        status: SarStatus::Filed,
        filed_by: actor.clone(),
        filed_at: now,
        updated_at: now,
        submission_ref: Bytes::new(env),
        auto_generated,
    };
    persist(env, &ComplianceKey::Sar(id), &sar);

    let vault_key = ComplianceKey::VaultSars(vault_id);
    let mut ids: Vec<u64> = env
        .storage()
        .persistent()
        .get(&vault_key)
        .unwrap_or_else(|| Vec::new(env));
    ids.push_back(id);
    persist(env, &vault_key, &ids);

    log_action(
        env,
        ComplianceAction::SarFiled,
        subject,
        actor,
        Some(vault_id),
        id,
        reason,
    );
    update_daily(env, |s| s.sars_filed += 1);
    env.events()
        .publish((SAR_FILED_TOPIC, vault_id), (id, auto_generated));
    Ok(id)
}

pub fn get_sar(env: &Env, sar_id: u64) -> Result<SarRecord, ContractError> {
    env.storage()
        .persistent()
        .get(&ComplianceKey::Sar(sar_id))
        .ok_or(ContractError::SarNotFound)
}

pub fn get_vault_sars(env: &Env, vault_id: u64) -> Vec<u64> {
    env.storage()
        .persistent()
        .get(&ComplianceKey::VaultSars(vault_id))
        .unwrap_or_else(|| Vec::new(env))
}

/// Advance a SAR's filing status. `submission_ref` is required when moving to
/// `Submitted` and otherwise keeps the existing value when empty.
pub fn update_sar_status(
    env: &Env,
    actor: &Address,
    sar_id: u64,
    status: SarStatus,
    submission_ref: Bytes,
) -> Result<SarRecord, ContractError> {
    let mut sar = get_sar(env, sar_id)?;
    if sar.status == SarStatus::Closed || status <= sar.status {
        return Err(ContractError::InvalidSarTransition);
    }
    check_label(&submission_ref, true)?;
    if status == SarStatus::Submitted && submission_ref.is_empty() {
        return Err(ContractError::InvalidComplianceInput);
    }
    if !submission_ref.is_empty() {
        sar.submission_ref = submission_ref;
    }
    sar.status = status;
    sar.updated_at = env.ledger().timestamp();
    persist(env, &ComplianceKey::Sar(sar_id), &sar);

    log_action(
        env,
        ComplianceAction::SarStatusUpdated,
        &sar.subject,
        actor,
        Some(sar.vault_id),
        sar_id,
        sar.submission_ref.clone(),
    );
    env.events().publish((SAR_STATUS_TOPIC, sar_id), status);
    Ok(sar)
}

/// Assemble a SAR submission package with a content digest.
pub fn generate_sar_report(
    env: &Env,
    sar_id: u64,
    vault: &crate::types::Vault,
) -> Result<SarReport, ContractError> {
    let sar = get_sar(env, sar_id)?;
    let trail = get_audit_trail(env, &sar.subject);
    let start = trail.len().saturating_sub(SAR_REPORT_AUDIT_ENTRIES);
    let recent_audit = trail.slice(start..trail.len());

    let mut report = SarReport {
        subject_kyc: get_kyc_record(env, &sar.subject).status,
        subject_blacklisted: is_blacklisted(env, &sar.subject),
        subject_last_aml: get_last_aml_check(env, &sar.subject),
        sar,
        vault_owner: vault.owner.clone(),
        vault_balance: vault.balance,
        vault_last_check_in: vault.last_check_in,
        vault_check_in_interval: vault.check_in_interval,
        recent_audit,
        generated_at: env.ledger().timestamp(),
        digest: BytesN::from_array(env, &[0u8; 32]),
    };
    report.digest = env.crypto().sha256(&report.clone().to_xdr(env)).into();
    Ok(report)
}

// ── Compliance reports (Issue #554) ──────────────────────────────────────────

/// Content digest of a report: sha256 of its XDR with the digest and
/// signature fields cleared.
pub fn report_digest(env: &Env, report: &ComplianceReport) -> BytesN<32> {
    let mut body = report.clone();
    body.digest = BytesN::from_array(env, &[0u8; 32]);
    body.signer = None;
    body.signature = None;
    body.signed_at = 0;
    env.crypto().sha256(&body.to_xdr(env)).into()
}

pub fn generate_report(
    env: &Env,
    actor: &Address,
    period: CompliancePeriod,
) -> Result<ComplianceReport, ContractError> {
    if period.end <= period.start {
        return Err(ContractError::InvalidCompliancePeriod);
    }
    let first_day = day_of(period.start);
    let last_day = day_of(period.end - 1);
    if last_day - first_day >= MAX_REPORT_PERIOD_DAYS {
        return Err(ContractError::InvalidCompliancePeriod);
    }

    let mut totals = DailyComplianceStats::default();
    for day in first_day..=last_day {
        let stats: Option<DailyComplianceStats> =
            env.storage().persistent().get(&ComplianceKey::Daily(day));
        if let Some(s) = stats {
            totals.aml_checks += s.aml_checks;
            totals.aml_flagged += s.aml_flagged;
            totals.kyc_verified += s.kyc_verified;
            totals.kyc_rejected += s.kyc_rejected;
            totals.blacklist_added += s.blacklist_added;
            totals.blacklist_removed += s.blacklist_removed;
            totals.sars_filed += s.sars_filed;
            totals.deposit_count += s.deposit_count;
            totals.withdrawal_count += s.withdrawal_count;
            totals.deposit_volume = totals.deposit_volume.saturating_add(s.deposit_volume);
            totals.withdrawal_volume = totals.withdrawal_volume.saturating_add(s.withdrawal_volume);
        }
    }

    let id = next_id(env, &ComplianceKey::ReportSeq);
    let mut report = ComplianceReport {
        id,
        period,
        aml_checks: totals.aml_checks,
        aml_flagged: totals.aml_flagged,
        kyc_verified: totals.kyc_verified,
        kyc_rejected: totals.kyc_rejected,
        kyc_totals: env
            .storage()
            .instance()
            .get(&ComplianceKey::KycTotals)
            .unwrap_or_default(),
        blacklist_added: totals.blacklist_added,
        blacklist_removed: totals.blacklist_removed,
        sars_filed: totals.sars_filed,
        deposit_count: totals.deposit_count,
        withdrawal_count: totals.withdrawal_count,
        deposit_volume: totals.deposit_volume,
        withdrawal_volume: totals.withdrawal_volume,
        generated_at: env.ledger().timestamp(),
        generated_by: actor.clone(),
        digest: BytesN::from_array(env, &[0u8; 32]),
        signer: None,
        signature: None,
        signed_at: 0,
    };
    report.digest = report_digest(env, &report);
    persist(env, &ComplianceKey::Report(id), &report);

    log_action(
        env,
        ComplianceAction::ReportGenerated,
        actor,
        actor,
        None,
        id,
        Bytes::from_array(env, &report.digest.to_array()),
    );
    env.events()
        .publish((REPORT_GENERATED_TOPIC, id), report.digest.clone());
    Ok(report)
}

pub fn get_report(env: &Env, report_id: u64) -> Result<ComplianceReport, ContractError> {
    env.storage()
        .persistent()
        .get(&ComplianceKey::Report(report_id))
        .ok_or(ContractError::ComplianceReportNotFound)
}

/// Attach an ed25519 signature over the report digest. The signature is
/// verified on-chain; an invalid signature aborts the transaction.
pub fn sign_report(
    env: &Env,
    actor: &Address,
    report_id: u64,
    signer: BytesN<32>,
    signature: BytesN<64>,
) -> Result<ComplianceReport, ContractError> {
    let mut report = get_report(env, report_id)?;
    if report.signature.is_some() {
        return Err(ContractError::ReportAlreadySigned);
    }
    if report_digest(env, &report) != report.digest {
        return Err(ContractError::ReportDigestMismatch);
    }
    let message = Bytes::from_array(env, &report.digest.to_array());
    env.crypto().ed25519_verify(&signer, &message, &signature);

    report.signer = Some(signer.clone());
    report.signature = Some(signature);
    report.signed_at = env.ledger().timestamp();
    persist(env, &ComplianceKey::Report(report_id), &report);

    log_action(
        env,
        ComplianceAction::ReportSigned,
        actor,
        actor,
        None,
        report_id,
        Bytes::from_array(env, &signer.to_array()),
    );
    env.events()
        .publish((REPORT_SIGNED_TOPIC, report_id), signer);
    Ok(report)
}

/// Recompute the report digest and re-check the stored signature.
pub fn verify_report(env: &Env, report_id: u64) -> Result<ReportVerification, ContractError> {
    let report = get_report(env, report_id)?;
    let digest_valid = report_digest(env, &report) == report.digest;
    let signed = match (&report.signer, &report.signature) {
        (Some(signer), Some(signature)) if digest_valid => {
            // Panics (aborting the call) if the signature does not verify.
            let message = Bytes::from_array(env, &report.digest.to_array());
            env.crypto().ed25519_verify(signer, &message, signature);
            true
        }
        _ => false,
    };
    Ok(ReportVerification {
        digest_valid,
        signed,
        signer: report.signer,
    })
}

// ── Regulatory change management (Issue #555) ───────────────────────────────

fn publish_notice(
    env: &Env,
    actor: &Address,
    requirement: &RegulatoryRequirement,
    kind: RegulatoryChangeKind,
) -> u64 {
    let id = next_id(env, &ComplianceKey::NoticeSeq);
    let notice = RegulatoryChangeNotice {
        id,
        requirement_id: requirement.id,
        version: requirement.version,
        kind,
        code: requirement.code.clone(),
        effective_date: requirement.effective_date,
        published_at: env.ledger().timestamp(),
    };
    persist(env, &ComplianceKey::Notice(id), &notice);
    log_action(
        env,
        ComplianceAction::RequirementChanged,
        actor,
        actor,
        None,
        requirement.id,
        requirement.code.clone(),
    );
    env.events().publish(
        (REGULATORY_CHANGE_TOPIC, requirement.id),
        (id, kind, requirement.version, requirement.effective_date),
    );
    id
}

fn save_requirement(env: &Env, requirement: &RegulatoryRequirement) {
    persist(
        env,
        &ComplianceKey::Requirement(requirement.id),
        requirement,
    );
    persist(
        env,
        &ComplianceKey::RequirementVersion(requirement.id, requirement.version),
        requirement,
    );
}

pub fn get_requirement(
    env: &Env,
    requirement_id: u64,
) -> Result<RegulatoryRequirement, ContractError> {
    env.storage()
        .persistent()
        .get(&ComplianceKey::Requirement(requirement_id))
        .ok_or(ContractError::RequirementNotFound)
}

fn get_active_requirement(
    env: &Env,
    requirement_id: u64,
) -> Result<RegulatoryRequirement, ContractError> {
    let requirement = get_requirement(env, requirement_id)?;
    if requirement.status == RequirementStatus::Retired {
        return Err(ContractError::RequirementRetired);
    }
    Ok(requirement)
}

fn check_requirement_fields(description: &Bytes) -> Result<(), ContractError> {
    if description.is_empty() || description.len() > MAX_DESCRIPTION_LEN {
        return Err(ContractError::InvalidComplianceInput);
    }
    Ok(())
}

pub fn register_requirement(
    env: &Env,
    actor: &Address,
    code: Bytes,
    jurisdiction: Bytes,
    description: Bytes,
    rule_hash: BytesN<32>,
    effective_date: u64,
) -> Result<RegulatoryRequirement, ContractError> {
    check_label(&code, false)?;
    check_label(&jurisdiction, false)?;
    check_requirement_fields(&description)?;

    let mut ids = list_requirements(env);
    if ids.len() >= MAX_REQUIREMENTS {
        return Err(ContractError::InvalidComplianceInput);
    }
    let id = next_id(env, &ComplianceKey::RequirementSeq);
    let now = env.ledger().timestamp();
    let requirement = RegulatoryRequirement {
        id,
        code,
        jurisdiction,
        description,
        rule_hash,
        version: 1,
        status: RequirementStatus::Active,
        effective_date,
        implemented_at: None,
        created_at: now,
        updated_at: now,
    };
    save_requirement(env, &requirement);
    ids.push_back(id);
    persist(env, &ComplianceKey::RequirementIds, &ids);
    publish_notice(env, actor, &requirement, RegulatoryChangeKind::Introduced);
    Ok(requirement)
}

/// Publish a new version of a requirement. The previous version is kept in
/// history as `Superseded`; the new version starts unimplemented.
pub fn amend_requirement(
    env: &Env,
    actor: &Address,
    requirement_id: u64,
    description: Bytes,
    rule_hash: BytesN<32>,
    effective_date: u64,
) -> Result<RegulatoryRequirement, ContractError> {
    check_requirement_fields(&description)?;
    let mut previous = get_active_requirement(env, requirement_id)?;
    let now = env.ledger().timestamp();

    previous.status = RequirementStatus::Superseded;
    previous.updated_at = now;
    persist(
        env,
        &ComplianceKey::RequirementVersion(requirement_id, previous.version),
        &previous,
    );

    let requirement = RegulatoryRequirement {
        version: previous.version + 1,
        description,
        rule_hash,
        status: RequirementStatus::Active,
        effective_date,
        implemented_at: None,
        updated_at: now,
        ..previous
    };
    save_requirement(env, &requirement);
    publish_notice(env, actor, &requirement, RegulatoryChangeKind::Amended);
    Ok(requirement)
}

/// Record the implementation date of the current requirement version.
pub fn mark_requirement_implemented(
    env: &Env,
    actor: &Address,
    requirement_id: u64,
) -> Result<RegulatoryRequirement, ContractError> {
    let mut requirement = get_active_requirement(env, requirement_id)?;
    if requirement.implemented_at.is_some() {
        return Err(ContractError::RequirementAlreadyImplemented);
    }
    let now = env.ledger().timestamp();
    requirement.implemented_at = Some(now);
    requirement.updated_at = now;
    save_requirement(env, &requirement);
    publish_notice(env, actor, &requirement, RegulatoryChangeKind::Implemented);
    Ok(requirement)
}

pub fn retire_requirement(
    env: &Env,
    actor: &Address,
    requirement_id: u64,
) -> Result<RegulatoryRequirement, ContractError> {
    let mut requirement = get_active_requirement(env, requirement_id)?;
    requirement.status = RequirementStatus::Retired;
    requirement.updated_at = env.ledger().timestamp();
    save_requirement(env, &requirement);
    publish_notice(env, actor, &requirement, RegulatoryChangeKind::Retired);
    Ok(requirement)
}

pub fn get_requirement_version(
    env: &Env,
    requirement_id: u64,
    version: u32,
) -> Result<RegulatoryRequirement, ContractError> {
    env.storage()
        .persistent()
        .get(&ComplianceKey::RequirementVersion(requirement_id, version))
        .ok_or(ContractError::RequirementNotFound)
}

/// All versions of a requirement, oldest first.
pub fn get_requirement_history(
    env: &Env,
    requirement_id: u64,
) -> Result<Vec<RegulatoryRequirement>, ContractError> {
    let current = get_requirement(env, requirement_id)?;
    let mut history = Vec::new(env);
    for version in 1..=current.version {
        if let Ok(entry) = get_requirement_version(env, requirement_id, version) {
            history.push_back(entry);
        }
    }
    Ok(history)
}

pub fn list_requirements(env: &Env) -> Vec<u64> {
    env.storage()
        .persistent()
        .get(&ComplianceKey::RequirementIds)
        .unwrap_or_else(|| Vec::new(env))
}

/// Active requirements whose current version has not been implemented yet.
pub fn get_pending_implementations(env: &Env) -> Vec<RegulatoryRequirement> {
    let mut pending = Vec::new(env);
    for id in list_requirements(env).iter() {
        if let Ok(requirement) = get_requirement(env, id) {
            if requirement.status == RequirementStatus::Active
                && requirement.implemented_at.is_none()
            {
                pending.push_back(requirement);
            }
        }
    }
    pending
}

/// `true` when the requirement is active and its effective date has passed.
pub fn is_requirement_effective(env: &Env, requirement_id: u64) -> Result<bool, ContractError> {
    let requirement = get_requirement(env, requirement_id)?;
    Ok(requirement.status == RequirementStatus::Active
        && env.ledger().timestamp() >= requirement.effective_date)
}

pub fn get_notice(env: &Env, notice_id: u64) -> Result<RegulatoryChangeNotice, ContractError> {
    env.storage()
        .persistent()
        .get(&ComplianceKey::Notice(notice_id))
        .ok_or(ContractError::NoticeNotFound)
}

/// Up to `limit` (capped at `MAX_NOTICE_PAGE`) notices with id greater than
/// `after_id`, oldest first.
pub fn get_notices(env: &Env, after_id: u64, limit: u32) -> Vec<RegulatoryChangeNotice> {
    let latest: u64 = env
        .storage()
        .instance()
        .get(&ComplianceKey::NoticeSeq)
        .unwrap_or(0);
    let limit = limit.min(MAX_NOTICE_PAGE);
    let mut notices = Vec::new(env);
    let mut id = after_id.saturating_add(1);
    while id <= latest && notices.len() < limit {
        if let Ok(notice) = get_notice(env, id) {
            notices.push_back(notice);
        }
        id += 1;
    }
    notices
}

pub fn get_last_acknowledged(env: &Env, user: &Address) -> u64 {
    env.storage()
        .persistent()
        .get(&ComplianceKey::NoticeAck(user.clone()))
        .unwrap_or(0)
}

/// Notices `user` has not acknowledged yet (oldest first, one page).
pub fn get_pending_notices(env: &Env, user: &Address) -> Vec<RegulatoryChangeNotice> {
    get_notices(env, get_last_acknowledged(env, user), MAX_NOTICE_PAGE)
}

/// Acknowledge every notice up to and including `notice_id`.
pub fn acknowledge_notices(env: &Env, user: &Address, notice_id: u64) -> Result<(), ContractError> {
    get_notice(env, notice_id)?;
    if notice_id <= get_last_acknowledged(env, user) {
        return Ok(());
    }
    persist(env, &ComplianceKey::NoticeAck(user.clone()), &notice_id);
    log_action(
        env,
        ComplianceAction::RegulatoryAcknowledged,
        user,
        user,
        None,
        notice_id,
        Bytes::new(env),
    );
    env.events()
        .publish((REGULATORY_ACK_TOPIC, user.clone()), notice_id);
    Ok(())
}
