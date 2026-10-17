#[contractimpl]
impl TtlVaultContract {
    // --- compliance (Issues #548, #549, #550, #551) ---

    /// Blacklist `address` with a recorded `reason` (admin only) — Issue #551.
    ///
    /// Blacklisted addresses cannot create vaults, deposit, or withdraw.
    ///
    /// # Errors
    /// * `ContractError::InvalidBlacklistReason` - reason is empty or longer
    ///   than `compliance::MAX_REASON_LEN` bytes
    pub fn blacklist_address(
        env: Env,
        address: Address,
        reason: String,
    ) -> Result<(), ContractError> {
        Self::require_admin(&env);
        let admin = Self::load_admin(&env);
        compliance::blacklist_address(&env, &address, reason, &admin)
    }

    /// Remove `address` from the blacklist (admin only). Returns `false` if
    /// the address was not blacklisted.
    pub fn remove_from_blacklist(env: Env, address: Address) -> bool {
        Self::require_admin(&env);
        compliance::remove_from_blacklist(&env, &address)
    }

    /// Returns the blacklist entry (reason, author, timestamp) for `address`.
    pub fn get_blacklist_entry(env: Env, address: Address) -> Option<compliance::BlacklistEntry> {
        compliance::get_blacklist_entry(&env, &address)
    }

    /// Add `address` to the allowlist (admin only) — Issue #551.
    pub fn add_to_allowlist(env: Env, address: Address) {
        Self::require_admin(&env);
        compliance::allowlist_address(&env, &address);
    }

    /// Remove `address` from the allowlist (admin only). Returns `false` if
    /// the address was not allowlisted.
    pub fn remove_from_allowlist(env: Env, address: Address) -> bool {
        Self::require_admin(&env);
        compliance::remove_from_allowlist(&env, &address)
    }

    /// Returns whether `address` is on the allowlist.
    pub fn is_allowlisted(env: Env, address: Address) -> bool {
        compliance::is_allowlisted(&env, &address)
    }

    /// Enable or disable allowlist mode (admin only). While enabled, only
    /// allowlisted addresses are compliant.
    pub fn set_allowlist_enforced(env: Env, enforced: bool) {
        Self::require_admin(&env);
        compliance::set_allowlist_enforced(&env, enforced);
        let admin = Self::load_admin(&env);
        compliance_policy::record_live_change(
            &env,
            &admin,
            compliance_policy::REASON_ALLOWLIST_MODE,
        );
    }

    /// Returns whether allowlist mode is enabled.
    pub fn is_allowlist_enforced(env: Env) -> bool {
        compliance::is_allowlist_enforced(&env)
    }

    /// Returns `true` when `address` is not blacklisted and, if allowlist
    /// mode is enabled, is allowlisted — Issue #551.
    pub fn is_address_compliant(env: Env, address: Address) -> bool {
        compliance::is_address_compliant(&env, &address)
    }

    /// Register the address of the KYC provider allowed to attest
    /// verifications (admin only) — Issue #548.
    pub fn set_kyc_provider(env: Env, provider: Address) {
        Self::require_admin(&env);
        compliance::set_kyc_provider(&env, &provider);
    }

    /// Returns the registered KYC provider, if any.
    pub fn get_kyc_provider(env: Env) -> Option<Address> {
        compliance::get_kyc_provider(&env)
    }

    /// Record the KYC provider's verification of `address` — Issue #548.
    ///
    /// Requires the registered provider's auth. Returns `false` without
    /// storing anything when `kyc_data` is unusable (zero level, zero
    /// reference hash, or `expires_at` not in the future).
    ///
    /// # Errors
    /// * `ContractError::KycProviderNotSet` - no provider has been registered
    pub fn verify_kyc(
        env: Env,
        address: Address,
        kyc_data: compliance::KycData,
    ) -> Result<bool, ContractError> {
        compliance::verify_kyc(&env, &address, kyc_data)
    }

    /// Revoke the KYC verification of `address` (KYC provider only).
    ///
    /// # Errors
    /// * `ContractError::KycProviderNotSet` - no provider has been registered
    /// * `ContractError::KycNotFound` - `address` has no KYC record
    pub fn revoke_kyc(env: Env, address: Address) -> Result<(), ContractError> {
        compliance::revoke_kyc_as_provider(&env, &address)
    }

    /// Revoke the KYC verification of `address` (admin only).
    ///
    /// # Errors
    /// * `ContractError::KycNotFound` - `address` has no KYC record
    pub fn admin_revoke_kyc(env: Env, address: Address) -> Result<(), ContractError> {
        Self::require_admin(&env);
        compliance::revoke_kyc(&env, &address)
    }

    /// Returns the stored KYC record for `address`, if any.
    pub fn get_kyc_record(env: Env, address: Address) -> Option<compliance::KycRecord> {
        compliance::get_kyc_record(&env, &address)
    }

    /// Returns `true` when `address` holds a non-revoked, unexpired KYC
    /// verification.
    pub fn is_kyc_verified(env: Env, address: Address) -> bool {
        compliance::is_kyc_verified(&env, &address)
    }

    /// Set the amount at or above which deposits and withdrawals require a
    /// valid KYC verification (admin only). `0` disables the requirement.
    ///
    /// # Errors
    /// * `ContractError::InvalidAmount` - `amount` is negative
    pub fn set_kyc_high_value_threshold(env: Env, amount: i128) -> Result<(), ContractError> {
        Self::require_admin(&env);
        compliance::set_kyc_high_value_threshold(&env, amount)?;
        let admin = Self::load_admin(&env);
        compliance_policy::record_live_change(
            &env,
            &admin,
            compliance_policy::REASON_KYC_THRESHOLD,
        );
        Ok(())
    }

    /// Returns the high-value KYC threshold (`0` = disabled).
    pub fn get_kyc_high_value_threshold(env: Env) -> i128 {
        compliance::get_kyc_high_value_threshold(&env)
    }

    /// Configure regulatory reporting thresholds (admin only) — Issue #550.
    ///
    /// # Errors
    /// * `ContractError::InvalidConfig` - a threshold is negative, or a
    ///   cumulative threshold is set with a zero window
    pub fn set_reporting_thresholds(
        env: Env,
        config: compliance::ThresholdConfig,
    ) -> Result<(), ContractError> {
        Self::require_admin(&env);
        compliance::set_threshold_config(&env, &config)?;
        let admin = Self::load_admin(&env);
        compliance_policy::record_live_change(
            &env,
            &admin,
            compliance_policy::REASON_REPORT_THRESHOLDS,
        );
        Ok(())
    }

    /// Returns the configured reporting thresholds, if any.
    pub fn get_reporting_thresholds(env: Env) -> Option<compliance::ThresholdConfig> {
        compliance::get_threshold_config(&env)
    }

    /// Returns the rolling cumulative transfer volume tracked for `address`.
    pub fn get_cumulative_volume(
        env: Env,
        address: Address,
    ) -> Option<compliance::CumulativeVolume> {
        compliance::get_cumulative_volume(&env, &address)
    }

    /// Returns the compliance alert with `alert_id`, if any.
    pub fn get_compliance_alert(env: Env, alert_id: u64) -> Option<compliance::ComplianceAlert> {
        compliance::get_alert(&env, alert_id)
    }

    /// Returns the number of compliance alerts raised so far.
    pub fn get_compliance_alert_count(env: Env) -> u64 {
        compliance::get_alert_count(&env)
    }

    /// Mark a compliance alert as reviewed (admin only).
    ///
    /// # Errors
    /// * `ContractError::AlertNotFound` - no alert with `alert_id`
    pub fn acknowledge_compliance_alert(env: Env, alert_id: u64) -> Result<(), ContractError> {
        Self::require_admin(&env);
        compliance::acknowledge_alert(&env, alert_id)
    }

    /// Returns the recorded transaction with `tx_id`, if any — Issue #549.
    pub fn get_transaction(env: Env, tx_id: u64) -> Option<compliance::TransactionRecord> {
        compliance::get_transaction(&env, tx_id)
    }

    /// Returns the number of transactions in the given inclusive date range
    /// (unix seconds).
    ///
    /// # Errors
    /// * `ContractError::InvalidReportRange` - `start_date > end_date`
    pub fn count_transactions(
        env: Env,
        start_date: u64,
        end_date: u64,
    ) -> Result<u64, ContractError> {
        compliance::count_transactions(&env, start_date, end_date)
    }

    /// Export transactions in the inclusive date range (unix seconds) as a
    /// UTF-8 CSV document — Issue #549.
    ///
    /// Columns: `tx_id,timestamp,kind,vault_id,from,to,token,amount,flagged`.
    /// Addresses are hex-encoded XDR `ScAddress` values. At most
    /// `compliance::MAX_EXPORT_ROWS` rows are returned; use
    /// `count_transactions` to detect ranges that need splitting.
    ///
    /// # Errors
    /// * `ContractError::InvalidReportRange` - `start_date > end_date`
    pub fn export_transactions(
        env: Env,
        start_date: u64,
        end_date: u64,
    ) -> Result<Bytes, ContractError> {
        compliance::export_transactions(&env, start_date, end_date).map(|(csv, _)| csv)
    }

    /// Returns the SHA-256 digest of the CSV `export_transactions` produces
    /// for the range; this is the message the report signer signs.
    ///
    /// # Errors
    /// * `ContractError::InvalidReportRange` - `start_date > end_date`
    pub fn compliance_report_digest(
        env: Env,
        start_date: u64,
        end_date: u64,
    ) -> Result<BytesN<32>, ContractError> {
        compliance::report_digest(&env, start_date, end_date)
    }

    /// Register the ed25519 public key allowed to sign compliance reports
    /// (admin only) — Issue #549.
    pub fn set_report_signer(env: Env, public_key: BytesN<32>) {
        Self::require_admin(&env);
        compliance::set_report_signer(&env, &public_key);
    }

    /// Returns the registered report signer public key, if any.
    pub fn get_report_signer(env: Env) -> Option<BytesN<32>> {
        compliance::get_report_signer(&env)
    }

    /// Store a signed compliance report for regulatory submission.
    ///
    /// `signature` must be the registered signer's ed25519 signature over
    /// `compliance_report_digest(start_date, end_date)`; an invalid signature
    /// aborts the invocation. Returns the new report id.
    ///
    /// # Errors
    /// * `ContractError::ReportSignerNotSet` - no signer has been registered
    /// * `ContractError::InvalidReportRange` - `start_date > end_date`
    pub fn sign_compliance_report(
        env: Env,
        start_date: u64,
        end_date: u64,
        signature: BytesN<64>,
    ) -> Result<u64, ContractError> {
        compliance::sign_report(&env, start_date, end_date, signature)
    }

    /// Returns the signed report with `report_id`, if any.
    pub fn get_signed_report(env: Env, report_id: u64) -> Option<compliance::SignedReport> {
        compliance::get_signed_report(&env, report_id)
    }

    // --- Issue #556: compliance policy versioning ---

    /// Publish a complete compliance policy that takes effect at
    /// `effective_from` (admin only). A policy effective now is applied to the
    /// live configuration immediately; a future one is applied by
    /// `sync_compliance_policy` once its date is reached. Returns the new
    /// version number.
    ///
    /// # Errors
    /// * `ContractError::InvalidEffectiveDate` - `effective_from` is in the past
    /// * `ContractError::InvalidAmount` - negative KYC threshold
    /// * `ContractError::InvalidConfig` - invalid reporting thresholds
    pub fn publish_compliance_policy(
        env: Env,
        kyc_high_value_threshold: i128,
        allowlist_enforced: bool,
        reporting_thresholds: Option<compliance::ThresholdConfig>,
        effective_from: u64,
    ) -> Result<u32, ContractError> {
        Self::require_admin(&env);
        let admin = Self::load_admin(&env);
        compliance_policy::publish_policy(
            &env,
            &admin,
            kyc_high_value_threshold,
            allowlist_enforced,
            reporting_thresholds,
            effective_from,
        )
    }

    /// Returns the compliance policy that was in force at `timestamp`.
    ///
    /// # Errors
    /// * `ContractError::PolicyVersionNotFound` - no policy was in force yet
    pub fn get_policy_version(
        env: Env,
        timestamp: u64,
    ) -> Result<compliance_policy::Policy, ContractError> {
        compliance_policy::get_policy_version(&env, timestamp)
    }

    /// Returns a compliance policy by its version number.
    pub fn get_policy_by_version(env: Env, version: u32) -> Option<compliance_policy::Policy> {
        compliance_policy::get_policy(&env, version)
    }

    /// Returns the number of recorded compliance policy versions.
    pub fn get_policy_version_count(env: Env) -> u32 {
        compliance_policy::get_version_count(&env)
    }

    /// Returns the policy version currently applied to the live configuration
    /// (`0` if none has been recorded).
    pub fn get_applied_policy_version(env: Env) -> u32 {
        compliance_policy::get_applied_version(&env)
    }

    /// Applies a scheduled compliance policy whose effective date has been
    /// reached. Anyone may call this. Returns the applied version, or `None`
    /// if the live configuration was already current.
    pub fn sync_compliance_policy(env: Env) -> Result<Option<u32>, ContractError> {
        compliance_policy::sync_active_policy(&env)
    }

    // --- Issue #557: vault history archival ---

    /// Moves state-transition history entries older than
    /// `history_archive::ARCHIVE_MIN_AGE_SECONDS` out of the hot on-chain log
    /// into hash-chained archive pages (vault owner or admin). Returns the
    /// number of pages archived.
    ///
    /// After archival, `get_state_transition_log` returns only the recent
    /// (hot) entries; older ones are available via `get_archived_history`.
    pub fn archive_vault_history(
        env: Env,
        vault_id: u64,
        caller: Address,
    ) -> Result<u32, ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner && caller != Self::load_admin(&env) {
            return Err(ContractError::NotOwner);
        }
        Ok(history_archive::archive_history(&env, vault_id))
    }

    /// Returns archived history page `page` for a vault.
    ///
    /// # Errors
    /// * `ContractError::ArchivePageNotFound` - the page was never archived, or
    ///   its cold body has been evicted from the live ledger (use the
    ///   `hist_arc` event copy with `verify_archived_history_page`)
    pub fn get_archived_history(
        env: Env,
        vault_id: u64,
        page: u32,
    ) -> Result<history_archive::ArchivedHistoryPage, ContractError> {
        history_archive::get_archived_page(&env, vault_id, page)
            .ok_or(ContractError::ArchivePageNotFound)
    }

    /// Returns archive bookkeeping (page count, archived entries, chain head).
    pub fn get_history_archive_info(env: Env, vault_id: u64) -> history_archive::ArchiveMeta {
        history_archive::get_meta(&env, vault_id)
    }

    /// Verifies externally-held entries for an archived page against the
    /// on-chain integrity digest.
    pub fn verify_archived_history_page(
        env: Env,
        vault_id: u64,
        page: u32,
        entries: Vec<StateTransitionEntry>,
    ) -> bool {
        history_archive::verify_page(&env, vault_id, page, &entries)
    }

    /// Verifies the full archive hash chain for a vault.
    ///
    /// # Errors
    /// * `ContractError::ArchiveIntegrityViolation` - the chain is broken
    pub fn verify_history_archive(env: Env, vault_id: u64) -> Result<(), ContractError> {
        if history_archive::verify_integrity(&env, vault_id) {
            Ok(())
        } else {
            Err(ContractError::ArchiveIntegrityViolation)
        }
    }

    // --- Issue #558: vault compression for inactive accounts ---

    /// Compresses a vault that has been inactive (no check-in) for at least
    /// the configured number of days. Anyone may call this; it changes only
    /// the storage representation, never the vault's state. Returns `true` if
    /// the vault was compressed.
    ///
    /// Compressed vaults are decompressed transparently on read, and restored
    /// uncompressed by the next state-changing operation.
    pub fn compress_vault(env: Env, vault_id: u64) -> bool {
        let ttl = Self::try_load_vault(&env, vault_id)
            .map(|v| vault_ttl_ledgers(v.check_in_interval))
            .unwrap_or_else(|| panic_with_error!(&env, ContractError::VaultNotFound));
        vault_compression::compress(&env, vault_id, ttl)
    }

    /// Restores a compressed vault to its uncompressed storage form. Returns
    /// `false` if the vault was not compressed.
    pub fn decompress_vault(env: Env, vault_id: u64) -> bool {
        match vault_compression::load_decompressed(&env, vault_id) {
            Some(vault) => {
                Self::save_vault(&env, vault_id, &vault);
                true
            }
            None => false,
        }
    }

    /// Returns whether a vault is currently stored compressed.
    pub fn is_vault_compressed(env: Env, vault_id: u64) -> bool {
        vault_compression::is_compressed(&env, vault_id)
    }

    /// Returns size information for a compressed vault, if compressed.
    pub fn get_vault_compression_info(
        env: Env,
        vault_id: u64,
    ) -> Option<vault_compression::CompressionInfo> {
        vault_compression::get_info(&env, vault_id)
    }

    /// Sets the number of inactive days after which vaults may be compressed
    /// (admin only, `1..=vault_compression::MAX_INACTIVITY_DAYS`).
    pub fn set_compression_inactivity_days(env: Env, days: u32) -> Result<(), ContractError> {
        Self::require_admin(&env);
        vault_compression::set_inactivity_days(&env, days)
    }

    /// Returns the inactivity period (days) required before compression.
    pub fn get_compression_inactivity_days(env: Env) -> u32 {
        vault_compression::get_inactivity_days(&env)
    }

    // --- Issue #559: incremental state snapshots ---

    /// Takes an incremental snapshot of the vault. Only fields changed since
    /// the previous snapshot are stored (differentially compressed), with a
    /// full checkpoint every `incremental_snapshots::FULL_SNAPSHOT_INTERVAL`
    /// snapshots. Anyone may call this. Returns the snapshot sequence number.
    pub fn create_incremental_snapshot(env: Env, vault_id: u64) -> u32 {
        let vault = Self::load_vault(&env, vault_id);
        incremental_snapshots::create_snapshot(&env, vault_id, &vault)
    }

    /// Reconstructs the full vault state at snapshot `sequence`.
    ///
    /// # Errors
    /// * `ContractError::SnapshotNotFound` - no snapshot with that sequence
    /// * `ContractError::SnapshotCorrupted` - reconstruction failed hash check
    pub fn reconstruct_vault_snapshot(
        env: Env,
        vault_id: u64,
        sequence: u32,
    ) -> Result<Vault, ContractError> {
        incremental_snapshots::reconstruct(&env, vault_id, sequence)
    }

    /// Reconstructs the vault state as of the latest incremental snapshot
    /// taken at or before `timestamp`.
    ///
    /// # Errors
    /// * `ContractError::SnapshotNotFound` - no snapshot at or before `timestamp`
    pub fn reconstruct_vault_snapshot_at(
        env: Env,
        vault_id: u64,
        timestamp: u64,
    ) -> Result<Vault, ContractError> {
        let sequence = incremental_snapshots::find_sequence_at(&env, vault_id, timestamp)
            .ok_or(ContractError::SnapshotNotFound)?;
        incremental_snapshots::reconstruct(&env, vault_id, sequence)
    }

    /// Returns the raw stored snapshot entry (deltas and sizes).
    pub fn get_incremental_snapshot(
        env: Env,
        vault_id: u64,
        sequence: u32,
    ) -> Option<incremental_snapshots::IncrementalSnapshot> {
        incremental_snapshots::get_snapshot(&env, vault_id, sequence)
    }

    /// Returns how many incremental snapshots exist for a vault.
    pub fn get_incremental_snapshot_count(env: Env, vault_id: u64) -> u32 {
        incremental_snapshots::get_snapshot_count(&env, vault_id)
    }
}
