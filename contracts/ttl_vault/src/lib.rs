#![no_std]
// Several event-topic constants in `types.rs` correspond to features whose
// implementation was lost to bad merges (see restoration history) and are
// kept as documentation of the intended event schema even though nothing
// currently emits them.
#![allow(dead_code)]

extern crate alloc;

use soroban_sdk::{
    contract, contracterror, contractimpl, panic_with_error, symbol_short, token, vec, xdr::ToXdr,
    Address, Bytes, BytesN, Env, String, Vec,
};

pub mod aml;
pub mod compliance;
pub mod compliance_policy;
#[cfg(test)]
mod compliance_policy_tests;
pub mod composition_rules;
pub mod credential_anchoring;
#[cfg(test)]
mod credential_anchoring_tests;
pub mod credential_lifecycle;
#[cfg(test)]
mod credential_lifecycle_tests;
mod diff_codec;
pub mod history_archive;
#[cfg(test)]
mod history_archive_tests;
pub mod incremental_snapshots;
#[cfg(test)]
mod incremental_snapshots_tests;
mod oracle;
pub mod ranking;
pub mod slice_attribute_matching;
pub mod slice_consensus_voting;
pub mod slice_cost_tracking;
pub mod slice_failover;
pub mod slice_performance;
pub mod template_inheritance;
mod types;
pub mod vault_compression;
#[cfg(test)]
mod vault_compression_tests;
pub mod vault_governance;
#[cfg(test)]
mod vault_governance_tests;
use types::{
    ArchivedVaultInfo, AuditEntry, BackupCode, BeneficiaryCommitment, BeneficiaryEntry,
    BeneficiaryPool, BeneficiaryRotationEntry, BeneficiaryStatus, BridgeConfig,
    CheckInHistoryEntry, CheckInStreak, ConditionalAcceptanceEntry, DataKey, DisputeStatus,
    GeoCheckInEntry, HibernationEntry, IntegrityReport, MetadataVersionEntry, MilestoneEntry,
    MilestoneVestingSchedule, MultiSigConfig, MultiSigOperation, MultiSigProposal, OwnershipProof,
    OwnershipTransferRequest, PasskeyAuditEntry, PasskeyDelegation, PasskeyEscrowRecord,
    PasskeyHash, PasskeyUsageEntry, PauseRecord, PendingBeneficiaryUpdate, ProofOfLifeEntry,
    ProposalStatus, ReleaseCondition, ReleaseEvent, ReleaseStatus, ReleaseVoteEntry,
    StateTransitionEntry, TokenCollateral, TokenConversion, TokenHedge, TokenLending,
    TokenRebalanceConfig, TokenStaking, TokenWeight, TtlBorrowRecord, UpgradeManifest, Vault,
    VaultStatusSummary, VestingBonusConfig, VestingCatchUpConfig, VestingPenaltyConfig,
    VestingPendingClaim, VestingSchedule, WhitelistEntry, WithdrawalAuditEntry, WithdrawalLimit,
    WithdrawalReversal, WithdrawalScheduleEntry, WithdrawalTracker, YieldDistributionConfig,
    YieldDistributionMode, ACCEPTANCE_DEADLINE_EXPIRED_TOPIC, ADD_PASSKEY_TOPIC,
    ADMIN_TRANSFER_COMPLETED_TOPIC, ADMIN_TRANSFER_PROPOSED_TOPIC, BACKUP_CODES_GENERATED_TOPIC,
    BACKUP_CODE_USED_TOPIC, BATCH_CHECKIN_TOPIC, BATCH_STATUS_TOPIC, BENEFICIARY_ACCEPTED_TOPIC,
    BENEFICIARY_CAP_TOPIC, BENEFICIARY_CONDITION_ACCEPTED_TOPIC, BENEFICIARY_DECLINED_TOPIC,
    BENEFICIARY_REBALANCED_TOPIC, BENEFICIARY_TIER_SET_TOPIC, BENEFICIARY_TRIGGER_SET_TOPIC,
    BENEFICIARY_UPDATED_TOPIC, BENEFICIARY_WATERFALL_TOPIC, BEN_ROTATION_TOPIC, CANCEL_TOPIC,
    CHECKIN_GEO_TOPIC, CHECKIN_POW_TOPIC, CHECKIN_RATE_LIMITED_TOPIC, CHECK_IN_TOPIC,
    CLAIM_VEST_TOPIC, CLIFF_REACHED_TOPIC, CONDITIONS_ACCEPTED_TOPIC, CONFLICT_EXPIRED_TOPIC,
    DELEGATE_BENEFICIARY_TOPIC, DELEGATE_CHECKIN_TOPIC, DEPOSIT_TOPIC, DISPUTE_FILED_TOPIC,
    DISPUTE_RESOLVED_TOPIC, DUPLICATE_VAULT_TOPIC, EXPIRY_WARNING_THRESHOLD,
    HIBERNATION_ENTERED_TOPIC, HIBERNATION_EXITED_TOPIC, INACTIVITY_PENALTY_TOPIC,
    INHERITANCE_TOPIC, INTEGRITY_TOPIC, MAX_CUSTOM_METADATA_LEN, MAX_DESCRIPTION_LEN,
    MAX_METADATA_LEN, MAX_NAME_LEN, MAX_NOTES_LEN, META_REVERT_TOPIC, META_VERSION_TOPIC,
    MIN_THRESHOLD_REDISTRIBUTE_TOPIC, MIN_THRESHOLD_SET_TOPIC, MIN_THRESHOLD_SKIP_TOPIC,
    MULTISIG_APPROVED_TOPIC, MULTISIG_CONFIG_TOPIC, MULTISIG_EXECUTED_TOPIC,
    MULTISIG_PROPOSAL_EXPIRY, MULTISIG_PROPOSED_TOPIC, MULTISIG_REJECTED_TOPIC,
    MULTISIG_SIGNER_REMOVED_TOPIC, MULTISIG_VETOED_TOPIC, OWNERSHIP_ACCEPTED_TOPIC,
    OWNERSHIP_CANCELLED_TOPIC, OWNERSHIP_INITIATED_TOPIC, OWNERSHIP_PROOF_TOPIC, OWNERSHIP_TOPIC,
    OWNERSHIP_TRANSFER_EXPIRED_TOPIC, PARTIAL_LIQUIDATE_TOPIC, PASSKEY_AUDIT_TOPIC,
    PASSKEY_DELEGATED_TOPIC, PASSKEY_DELEGATION_REVOKED_TOPIC, PASSKEY_ESCROWED_TOPIC,
    PASSKEY_ESCROW_CANCELLED_TOPIC, PASSKEY_ESCROW_RELEASED_TOPIC, PASSKEY_EXPIRY_EXTENDED_TOPIC,
    PASSKEY_EXPIRY_WARNING_TOPIC, PASSKEY_LOCKOUT_TOPIC, PASSKEY_RECOVERED_TOPIC,
    PASSKEY_RECOVERY_INITIATED_TOPIC, PASSKEY_ROTATION_ENFORCED_TOPIC,
    PASSKEY_ROTATION_REQUIRED_TOPIC, PASSKEY_UNLOCKED_TOPIC, PASSKEY_USAGE_TOPIC, PAUSE_TOPIC,
    PAUSE_VAULT_TOPIC, PING_EXPIRY_TOPIC, POOL_CREATED_TOPIC, PROOF_OF_LIFE_TOPIC,
    RECOVERY_EXTEND_TOPIC, RELEASE_TOPIC, RELEASE_VOTE_PASSED_TOPIC, RELEASE_VOTE_TOPIC,
    REMOVE_PASSKEY_TOPIC, RESTORE_VAULT_TOPIC, RESUME_VAULT_TOPIC, REVOKE_DELEGATE_TOPIC,
    ROTATE_PASSKEY_TOPIC, SET_BENEFICIARIES_TOPIC, SET_BURN_PERCENTAGE_TOPIC, SET_DECAY_RATE_TOPIC,
    SET_MAX_INTERVAL_TOPIC, SET_MAX_TTL_TOPIC, SET_METADATA_TOPIC, SET_MIN_INTERVAL_TOPIC,
    SET_RECOVERY_TOPIC, SET_SPENDING_LIMIT_TOPIC, SET_VESTING_TOPIC, STATE_TRANSITION_TOPIC,
    SYNC_TTL_TOPIC, TOKEN_COLLATERAL_TOPIC, TOKEN_COLLAT_RLSD_TOPIC, TOKEN_CONVERSION_TOPIC,
    TOKEN_HEDGE_CLOSE_TOPIC, TOKEN_HEDGE_TOPIC, TOKEN_LENDING_TOPIC, TOKEN_LEND_REPAY_TOPIC,
    TOKEN_REBALANCED_TOPIC, TOKEN_REBALANCE_TOPIC, TOKEN_STAKING_TOPIC, TOKEN_UNSTAKING_TOPIC,
    TOKEN_WHITELIST_VALIDATED_TOPIC, TTL_ACCELERATE_TOPIC, TTL_BORROW_TOPIC, TTL_DECAY_TOPIC,
    TTL_PREDICTED_TOPIC, TTL_REPAY_TOPIC, UNPAUSE_TOPIC, UPDATE_INTERVAL_TOPIC,
    UPDATE_METADATA_TOPIC, VAULT_ARCHIVED_TOPIC, VAULT_CAP_TOPIC, VAULT_CLONED_OVERRIDE_TOPIC,
    VAULT_CLONED_TOPIC, VAULT_CREATED_TOPIC, VAULT_MERGED_TOPIC, VESTING_BONUS_CLAIMED_TOPIC,
    VESTING_BONUS_SET_TOPIC, VESTING_CANCELLED_TOPIC, VESTING_CATCHUP_CLAIMED_TOPIC,
    VESTING_CATCHUP_SET_TOPIC, WHITELIST_ADDED_TOPIC, WHITELIST_REMOVED_TOPIC,
    WITHDRAWAL_APPROVAL_GRANTED_TOPIC, WITHDRAWAL_APPROVAL_REQUESTED_TOPIC, WITHDRAWAL_AUDIT_TOPIC,
    WITHDRAWAL_EXECUTED_TOPIC, WITHDRAWAL_FAILED_TOPIC, WITHDRAWAL_LIMIT_SET_TOPIC,
    WITHDRAWAL_NOTIF_TOPIC, WITHDRAWAL_REVERSED_TOPIC, WITHDRAWAL_SCHEDULED_TOPIC,
    WITHDRAWAL_VALIDATION_TOPIC, WITHDRAW_TOPIC, WRAPPED_TOKEN_REGISTERED_TOPIC,
    WRAPPED_TOKEN_UNREGISTERED_TOPIC, YIELD_DISTRIBUTED_TOPIC, YIELD_REINVESTED_TOPIC,
};
use types::{
    // Issue #563: cursor pagination
    BatchCheckInResult,
    BeneficiaryAuction,
    BeneficiaryAuctionBid,
    BeneficiaryConditionalAcceptance,
    BeneficiaryConflict,
    BeneficiaryConflictClaim,
    BeneficiaryRebalancedEvent,
    BeneficiaryTierSetEvent,
    BeneficiaryTriggerSetEvent,
    BeneficiaryVestingSchedule,
    BeneficiaryWaterfallEvent,
    ConflictResolution,
    CountdownConfig,
    CustomMetadataEntry,
    // Issue #560: Merkle history proofs
    MerkleLeaf,
    MerkleProof,
    PasskeyLockout,
    PasskeyRecoveryRequest,
    PasskeyRotationPolicy,
    ProtocolConfig,
    ReleaseTrigger,
    TwoFactorConfigData,
    VaultConfigTemplate,
    VaultPage,
    VaultSnapshot,
    VaultSortField,
    VestingAccelerationConfig,
    VestingForfeitureConfig,
    VestingRolloverConfig,
    VestingStaggerEntry,
    WithdrawalApprovalRequest,
    WithdrawalEscrow,
    WithdrawalProof,
    WithdrawalRateLimit,
    WithdrawalRollback,
    AUCTION_BID_TOPIC,
    AUCTION_CREATED_TOPIC,
    AUCTION_FINALIZED_TOPIC,
    BENEFICIARY_CONFLICT_FILED_TOPIC,
    BENEFICIARY_CONFLICT_RESOLVED_TOPIC,
    BEN_COMMITTED_TOPIC,
    BEN_REVEALED_TOPIC,
    BIND_PASSKEY_BIOMETRIC_TOPIC,
    BIO_CHECKIN_TOPIC,
    BURN_EVENT_TOPIC,
    CLAIM_BENEFICIARY_VESTING_TOPIC,
    CLAWBACK_UNVESTED_TOPIC,
    COUNTDOWN_NOTIF_TOPIC,
    EMERGENCY_RECOVERY_GENERATED_TOPIC,
    EMERGENCY_RECOVERY_USED_TOPIC,
    // New event topics
    HISTORY_PROOF_TOPIC,
    HISTORY_ROOT_TOPIC,
    MAX_VESTING_SCHEDULES,
    MILESTONE_ADJUST_TOPIC,
    MILESTONE_CLAIM_TOPIC,
    MILESTONE_EMERGENCY_TOPIC,
    MILESTONE_PAUSE_TOPIC,
    MILESTONE_PROGRESS_TOPIC,
    MILESTONE_RESUME_TOPIC,
    MILESTONE_VEST_TOPIC,
    PASSKEY_COMPROMISED_TOPIC,
    PASSKEY_EXPIRED_TOPIC,
    PROTOCOL_CONFIG_APPLIED_TOPIC,
    PROTOCOL_CONFIG_PROPOSED_TOPIC,
    SET_BENEFICIARY_VESTING_TOPIC,
    SET_COUNTDOWN_TOPIC,
    TWO_FACTOR_DISABLED_TOPIC,
    TWO_FACTOR_ENABLED_TOPIC,
    TWO_FACTOR_VERIFIED_TOPIC,
    UNBIND_PASSKEY_BIOMETRIC_TOPIC,
    VAULT_LIST_TOPIC,
    VAULT_TMPL_REF_TOPIC,
    VAULT_TMPL_REG_TOPIC,
    VESTING_ACCELERATED_TOPIC,
    VESTING_FINALIZED_TOPIC,
    VESTING_FORFEITURE_TOPIC,
    VESTING_PENALTY_TOPIC,
    VESTING_REVERSED_TOPIC,
    VESTING_ROLLOVER_TOPIC,
    VESTING_SCHEDULE_ADDED_TOPIC,
    VESTING_STAGGER_TOPIC,
    WITHDRAWAL_ESCROW_CREATED_TOPIC,
    WITHDRAWAL_ESCROW_VERIFIED_TOPIC,
    WITHDRAWAL_PROOF_TOPIC,
    WITHDRAWAL_RATE_LIMITED_TOPIC,
    WITHDRAWAL_ROLLBACK_TOPIC,
};
#[cfg(all(test, feature = "experimental-tests"))]
mod beneficiary_auction_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod beneficiary_conditional_acceptance_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod beneficiary_dispute_escalation_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod beneficiary_pooling_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod beneficiary_vesting_auction_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod beneficiary_vesting_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod bps_invariant_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod composition_rules_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod conditional_withdrawal_release_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod hibernation_consistency_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod lifecycle_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod multisig_withdrawal_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod passkey_attestation_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod passkey_audit_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod passkey_breach_detection_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod passkey_cap_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod passkey_delegation_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod passkey_escrow_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod passkey_expiry_notification_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod passkey_metadata_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod passkey_risk_scoring_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod regression_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod slice_consensus_voting_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod slice_failover_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod slice_performance_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod upgrade_simulation_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod upgrade_validation_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod withdrawal_escrow_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod withdrawal_notification_confirmation_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod withdrawal_rate_limit_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod withdrawal_rollback_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod withdrawal_whitelist_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod invariant_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod load_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod mutation_tests;
#[cfg(all(test, feature = "experimental-tests"))]
mod performance_regression_tests;

/// Minimum TTL (in ledgers) before a persistent entry is eligible for extension.
/// At ~5 s/ledger this is ~83 minutes.
pub const VAULT_TTL_THRESHOLD: u32 = 1000;

/// Default persistent storage TTL for vault entries, in ledgers.
/// 200_000 ledgers × 5 s/ledger ≈ 11.6 days.
/// Used as the floor in `vault_ttl_ledgers`; long-interval vaults get a larger value.
pub const VAULT_TTL_LEDGERS: u32 = 200_000;

/// Minimum TTL (in ledgers) before instance storage is eligible for extension.
/// At ~5 s/ledger this is ~83 minutes.
pub const INSTANCE_TTL_THRESHOLD: u32 = 1000;

/// TTL for instance storage entries, in ledgers.
/// 200_000 ledgers × 5 s/ledger ≈ 11.6 days.
/// Extended on every state-mutating call to keep contract instance alive.
pub const INSTANCE_TTL_LEDGERS: u32 = 200_000;

/// Approximate ledger close time in seconds (Stellar mainnet ~5s).
const LEDGER_SECOND: u32 = 5;
/// Soroban maximum persistent entry TTL in ledgers (~180 days at 5s/ledger).
const MAX_PERSISTENT_TTL: u32 = 3_110_400;

/// Time-lock delay for ownership transfers in seconds (24 hours).
/// The new owner cannot accept until this many seconds have elapsed after initiation.
const OWNERSHIP_TRANSFER_TIMELOCK: u64 = 86_400;

/// Expiry window for pending ownership transfer requests in seconds (7 days).
/// If the new owner does not accept within this window, the request expires.
const OWNERSHIP_TRANSFER_EXPIRY: u64 = 604_800;

/// Minimum seconds between consecutive check-ins (default: 60 seconds).
const DEFAULT_MIN_CHECKIN_COOLDOWN: u64 = 60;

/// Maximum seconds an owner can accelerate TTL decay per call (30 days).
const MAX_ACCELERATE_SECONDS: u64 = 2_592_000;

/// Time-lock delay for admin transfers in seconds (24 hours) — Issue #813.
const ADMIN_TRANSFER_TIMELOCK: u64 = 86_400;

/// Time-lock delay before a proposed protocol config takes effect (24 hours) — Issue #809.
const PROTOCOL_CONFIG_TIMELOCK: u64 = 86_400;

/// Maximum number of beneficiaries allowed per vault — Issue #872.
/// Derived from benchmark data: 20 beneficiaries stays safely below the 100M
/// Soroban instruction limit; 50 approaches it. Capped at 20 for headroom.
pub const MAX_BENEFICIARIES: u32 = 20;

/// Maximum number of on-chain passkey usage entries retained per vault.
/// Matches the `timestamps.len() > 1000` cap already applied to vault snapshot
/// timestamps. Once the cap is hit the oldest entry is dropped before writing
/// the new one, keeping the serialized Vec within Soroban's practical per-entry
/// storage limits. Full history remains available from emitted events.
pub const MAX_PASSKEY_USAGE_ENTRIES: u32 = 1000;

/// Maximum number of on-chain passkey audit log entries retained per vault.
/// Matches the same 1000-entry cap used for passkey usage entries above.
/// Full history remains available from emitted `pk_audit` events.
pub const MAX_PASSKEY_AUDIT_ENTRIES: u32 = 1000;

/// Compute a persistent storage TTL (in ledgers) for a vault with the given
/// check-in interval. Applies a 2× safety buffer so storage outlives the
/// interval, capped at the Soroban maximum.
fn vault_ttl_ledgers(check_in_interval: u64) -> u32 {
    // Saturate rather than truncate: `as u32` on a u64 wraps around instead of
    // clamping, which broke monotonicity for check_in_interval > u32::MAX.
    let interval_u32 = check_in_interval.min(u32::MAX as u64) as u32;
    let ledgers = interval_u32.saturating_mul(2).saturating_div(LEDGER_SECOND);
    ledgers.clamp(VAULT_TTL_LEDGERS, MAX_PERSISTENT_TTL)
}

#[contracterror(export = false)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ContractError {
    AlreadyInitialized = 1,
    InvalidInterval = 2,
    VaultNotFound = 3,
    EmptyVault = 4,
    InvalidAmount = 5,
    NotOwner = 6,
    AlreadyReleased = 7,
    InsufficientBalance = 8,
    NotAdmin = 9,
    Paused = 10,
    NoPendingAdmin = 11,
    InvalidBps = 12,
    NotExpiringSoon = 13,
    IntervalTooLow = 14,
    IntervalTooHigh = 15,
    NotExpired = 16,
    InvalidBeneficiary = 17,
    BalanceOverflow = 18,
    VaultExpired = 19,
    InvalidAdmin = 20,
    NotInitialized = 21,
    VestingNotFound = 22,
    NothingToClaimYet = 23,
    VestingAlreadyComplete = 24,
    MaxTtlExceeded = 25,
    InvalidPasskey = 26,
    PasskeyNotFound = 27,
    InvalidBackupCode = 28,
    BackupCodeAlreadyUsed = 29,
    NotBeneficiary = 30,
    DisputeFiled = 31,
    NoScheduledWithdrawals = 32,
    ConditionsNotApproved = 33,
    NoPendingOwnershipTransfer = 34,
    OwnershipTransferExpired = 35,
    OwnershipTransferTimeLocked = 36,
    UpgradeInvalidHash = 37,
    DepositLimitExceeded = 38,
    WithdrawalNotApproved = 39,
    NotRecoveryContact = 40,
    InvalidThreshold = 41,
    MultiSigRequired = 42,
    NotASigner = 43,
    ProposalNotFound = 44,
    ProposalExpired = 45,
    AlreadyApproved = 46,
    ProposalNotApproved = 47,
    MetadataVersionNotFound = 48,
    VaultCapacityExceeded = 49,
    IncompatibleVaultToken = 50,
    ProofOfLifeRequired = 51,
    ProofOfLifeExpired = 52,
    AlreadyVoted = 53,
    VotingNotEnabled = 54,
    AlreadyHibernating = 55,
    NotHibernating = 56,
    DuplicateVault = 57,
    CheckInTooFrequent = 58,
    VaultFrozen = 59,
    CliffNotReached = 60,
    InsufficientTtlToAccelerate = 61,
    TtlBorrowNotFound = 62,
    TtlBorrowAlreadyRepaid = 63,
    // Issue #565: withdrawal scheduling validation
    OverlappingWithdrawalSchedule = 64,
    ConflictingWithdrawalSchedule = 65,
    // Issue #566: withdrawal limits by time
    DailyWithdrawalLimitExceeded = 66,
    WeeklyWithdrawalLimitExceeded = 67,
    MonthlyWithdrawalLimitExceeded = 68,
    // Issue #567: withdrawal destination whitelist
    WithdrawalDestinationNotWhitelisted = 69,
    // Issue #568: withdrawal reversal
    WithdrawalReversalGracePeriodExpired = 70,
    WithdrawalAlreadyReversed = 71,
    // Issue #545: vesting catch-up
    CatchUpNotEnabled = 72,
    // Issue #546: vesting bonus
    BonusNotEnabled = 73,
    TokenNotWhitelisted = 74,
    // Issue #526: post-release clawback
    NotReleased = 75,
    GracePeriodExpired = 76,
    NothingToClawback = 77,
    // Issue #527: beneficiary auction
    AuctionNotFound = 78,
    AuctionAlreadyExists = 79,
    AuctionEnded = 80,
    AuctionNotEnded = 81,
    InvalidVestingSchedule = 82,
    // Per-delegation nonce mismatch (replay attack prevention)
    InvalidNonce = 83,
    // Issue #811: admin self-dealing prevention
    AdminCannotOwnVault = 84,
    // Issue #813: admin transfer timelock
    AdminTransferTimeLocked = 85,
    // Reject no-op ownership transfer
    AlreadyOwner = 86,
    // Beneficiary Anonymity (ZK)
    BeneficiaryAlreadyRevealed = 87,
    // Beneficiary capacity limits
    BeneficiaryCapacityExceeded = 88,
    // Beneficiary veto of owner-defined release conditions before expiry
    BeneficiaryVetoReleaseCondition = 89,
    // Beneficiary Anonymity (ZK)
    CommitmentAlreadySet = 90,
    CommitmentNotFound = 91,
    // Grace period before release is permitted after expiry
    GracePeriodActive = 92,
    // Issue #815: config interval validation
    InvalidConfig = 93,
    // Issue #871: metadata UTF-8 enforcement
    InvalidMetadataEncoding = 94,
    // Issue #934: emergency vault recovery code
    InvalidRecoveryCode = 95,
    // Reject withdraw when destination equals contract address
    InvalidWithdrawDestination = 96,
    // Beneficiary Anonymity (ZK)
    InvalidZkProof = 97,
    // Issue #809: two-step protocol configuration
    NoPendingProtocolConfig = 98,
    // Issue #562: passkey compromise response
    PasskeyCompromised = 99,
    // Issue #549: passkey expired during check-in
    PasskeyExpired = 100,
    ProtocolConfigTimeLocked = 101,
    // Issue #934: emergency vault recovery code
    RecoveryCodeNotSet = 102,
    // Vault state snapshots
    SnapshotNotFound = 103,
    // Issue #965: two-factor authentication
    TwoFactorNotEnabled = 104,
    // Deposit restricted to vault owner
    UnauthorizedDepositor = 105,
    // Issue #543: Vesting Acceleration on Death
    VestingAlreadyAccelerated = 106,
    // Issue #548: post-release vesting reversal
    VestingReversalExpired = 107,
    VestingReversalNotFound = 108,
    // Issue #544: Vesting Staggering
    VestingStaggerNotFound = 109,
    // Vault Partial Liquidation Before Release
    InvalidPercentage = 110,
    // Amount exceeds the per-call percentage limit on partial liquidation.
    LiquidationExceedsLimit = 111,
    // Issue #559: passkey escrow
    AlreadyInEscrow = 112,
    // Issue #559: check-in attempted with a passkey currently held in escrow
    PasskeyInEscrow = 113,
    // Issue #44: composition rule not found
    RuleNotFound = 114,
    // Issue #34: credential lifecycle state machine
    InvalidCredentialState = 115,
    // Issue #269: credential lifecycle state transitions
    InvalidStateTransition = 116,
    // Issue #35: slice failover mechanism
    InvalidSlice = 117,
    FailoverAlreadyActive = 118,
    // Issue #32: credential anchoring to external systems
    AnchorAlreadyExists = 120,
    AnchorNotFound = 121,
    InvalidExternalId = 122,
    InvalidAnchorSystem = 123,
    // Issue #37: template inheritance
    TemplateNotFound = 124,
    InheritanceCycleDetected = 125,
    // Slice consensus voting: finalization attempted below the minimum quorum
    InsufficientQuorum = 126,
    // Upgrade safety validation (see docs/upgrade-safety.md)
    UpgradeInterfaceShrunk = 127,
    UpgradeStorageSchemaChanged = 128,
    UpgradeErrorCodesReduced = 129,
    UpgradeManifestNotSet = 130,
    // Issue #547: AML screening rejected a beneficiary / transfer recipient
    AmlFlaggedAddress = 131,
    // Issue #559: incremental snapshot chain failed to reconstruct / verify
    SnapshotCorrupted = 132,
    // Issue #557: history archive page missing or hash chain broken
    ArchivePageNotFound = 133,
    ArchiveIntegrityViolation = 134,
    // Issue #556: compliance policy versioning
    PolicyVersionNotFound = 135,
    InvalidEffectiveDate = 136,
    // Issue #558: compressed vault payload failed to decode / verify
    CompressionCorrupted = 137,
    // Upgrade safety: a new contract changed a public function signature.
    UpgradeFunctionSignaturesChanged = 138,
    UpgradeFunctionSignaturesNotSet = 139,
    // Compliance / AML / KYC support added via #551/#548/#549.
    InvalidBlacklistReason = 140,
    AddressBlacklisted = 141,
    AddressNotAllowlisted = 142,
    KycProviderNotSet = 143,
    KycNotFound = 144,
    KycRequired = 145,
    AlertNotFound = 146,
    InvalidReportRange = 147,
    ReportSignerNotSet = 148,
}

#[contract]
pub struct TtlVaultContract;

include!("contract_impls/admin.rs");
include!("contract_impls/vault_lifecycle.rs");
include!("contract_impls/withdrawals.rs");
include!("contract_impls/beneficiaries.rs");
include!("contract_impls/vesting.rs");
include!("contract_impls/views.rs");
include!("contract_impls/passkeys_and_recovery.rs");
include!("contract_impls/beneficiary_workflows.rs");
include!("contract_impls/withdrawal_and_tokens.rs");
include!("contract_impls/compliance_and_history.rs");
