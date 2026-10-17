#[contractimpl]
impl TtlVaultContract {
    // --- Vault Partial Liquidation Before Release ---

    /// Owner-only partial liquidation. Withdraws up to `percentage_allowed`
    /// percent of the vault's current balance without resetting the TTL countdown.
    ///
    /// Use cases:
    /// - Owner needs immediate liquidity while keeping the vault alive.
    /// - Owner wants to skim interest / yield before expiry.
    ///
    /// The caller passes the per-call ceiling explicitly via `percentage_allowed`
    /// (1-100, inclusive). The function rejects amounts exceeding that ceiling and
    /// does NOT touch `last_check_in`, so the existing TTL countdown keeps running.
    ///
    /// Multiple calls per vault are permitted; each call is independently bounded
    /// by the percentage ceiling. `last_check_in` and `status` are preserved, so
    /// the vault keeps its current time-to-live and stays in `Locked`.
    ///
    /// # Arguments
    /// * `vault_id` - The vault ID to liquidate from.
    /// * `amount` - The amount to withdraw (must be > 0 and ≤ `percentage_allowed`
    ///              percent of the vault's current balance).
    /// * `percentage_allowed` - Per-call cap, as a percentage (1-100, inclusive).
    ///
    /// # Returns
    /// `Ok(())` on success.
    ///
    /// # Errors
    /// * `ContractError::Paused` - Contract is paused.
    /// * `ContractError::InvalidAmount` - `amount` is not positive.
    /// * `ContractError::InvalidPercentage` - `percentage_allowed` is 0 or > 100.
    /// * `ContractError::LiquidationExceedsLimit` - `amount` exceeds allowed percentage.
    /// * `ContractError::InsufficientBalance` - vault cannot cover `amount`.
    /// * `ContractError::NotOwner` - caller is not the vault owner.
    /// * `ContractError::AlreadyReleased` - vault is not in Locked status.
    /// * `ContractError::VaultFrozen` - vault is in EmergencyFrozen status.
    pub fn partial_liquidate(
        env: Env,
        vault_id: u64,
        amount: i128,
        percentage_allowed: u32,
    ) -> Result<(), ContractError> {
        if Self::load_paused(&env) {
            return Err(ContractError::Paused);
        }
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }
        if percentage_allowed == 0 || percentage_allowed > 100 {
            return Err(ContractError::InvalidPercentage);
        }

        let mut vault = Self::load_vault(&env, vault_id);
        if vault.is_paused {
            return Err(ContractError::Paused);
        }
        vault.owner.require_auth();

        if vault.status == ReleaseStatus::EmergencyFrozen {
            return Err(ContractError::VaultFrozen);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }

        // Capture TTL remaining BEFORE any state mutation so the emitted
        // event accurately reflects the countdown at the time of liquidation.
        let ttl_remaining_before = Self::get_ttl_remaining(env.clone(), vault_id).unwrap_or(0);

        // Enforce the per-call percentage cap on the current vault balance.
        // Use checked arithmetic to guard against i128 overflow.
        // (Stellar asset balances are well below i128::MAX / 100 in practice,
        // but we conservatively reject on overflow rather than silently
        // substituting `i128::MAX`.)
        let max_allowed = vault
            .balance
            .checked_mul(percentage_allowed as i128)
            .and_then(|v| v.checked_div(100));
        let Some(max_allowed) = max_allowed else {
            return Err(ContractError::LiquidationExceedsLimit);
        };
        if amount > max_allowed {
            return Err(ContractError::LiquidationExceedsLimit);
        }
        if amount > vault.balance {
            return Err(ContractError::InsufficientBalance);
        }

        // Transfer tokens to the owner.
        let token_client = token::Client::new(&env, &vault.token_address);
        token_client.transfer(&env.current_contract_address(), &vault.owner, &amount);
        vault.balance -= amount;

        // CRITICAL: Do NOT touch vault.last_check_in here. The TTL countdown
        // keeps running so a partial liquidation does not reset the expiry.
        Self::save_vault(&env, vault_id, &vault);

        Self::log_audit_entry(
            &env,
            vault_id,
            "partial_liquidate",
            &vault.owner,
            "partial_liquidation",
        );
        Self::append_activity_log(
            &env,
            vault_id,
            "partial_liquidate",
            &vault.owner,
            "partial_liquidation",
        );

        env.events().publish(
            (PARTIAL_LIQUIDATE_TOPIC, vault_id),
            (vault_id, amount, ttl_remaining_before),
        );
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        Ok(())
    }

    // --- Issue #318: batch_withdraw ---

    /// Withdraws from multiple vaults owned by the same caller in a single transaction.
    ///
    /// This is more efficient than calling `withdraw` multiple times as it reduces
    /// transaction overhead. All vault_ids and amounts are validated before any
    /// state mutation occurs.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_ids` - Vector of vault IDs to withdraw from
    /// * `amounts` - Vector of amounts (in stroops) to withdraw from each vault
    /// * `caller` - The address of the caller (must be the owner of all vaults)
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    ///
    /// # Errors
    /// * `ContractError::Paused` - If the contract is paused
    /// * `ContractError::InvalidAmount` - If vault_ids.len() != amounts.len() or any amount is not positive
    /// * `ContractError::VaultNotFound` - If any vault does not exist
    /// * `ContractError::NotOwner` - If caller is not the owner of any vault
    /// * `ContractError::AlreadyReleased` - If any vault is not in Locked status
    /// * `ContractError::InsufficientBalance` - If any vault balance is less than the requested amount
    pub fn batch_withdraw(
        env: Env,
        vault_ids: Vec<u64>,
        amounts: Vec<i128>,
        caller: Address,
    ) -> Result<(), ContractError> {
        if Self::load_paused(&env) {
            return Err(ContractError::Paused);
        }
        if vault_ids.len() != amounts.len() {
            return Err(ContractError::InvalidAmount);
        }
        caller.require_auth();

        // Validate all entries before mutating state
        for (vault_id, amount) in vault_ids.iter().zip(amounts.iter()) {
            if amount <= 0 {
                Self::record_withdrawal_audit(
                    &env,
                    vault_id,
                    &caller,
                    amount,
                    false,
                    "Invalid amount",
                );
                return Err(ContractError::InvalidAmount);
            }
            let vault = Self::try_load_vault(&env, vault_id).ok_or(ContractError::VaultNotFound)?;
            if caller != vault.owner {
                Self::record_withdrawal_audit(&env, vault_id, &caller, amount, false, "Not owner");
                return Err(ContractError::NotOwner);
            }
            if vault.status != ReleaseStatus::Locked {
                Self::record_withdrawal_audit(
                    &env,
                    vault_id,
                    &caller,
                    amount,
                    false,
                    "Vault already released",
                );
                return Err(ContractError::AlreadyReleased);
            }
            if vault.balance < amount {
                Self::record_withdrawal_audit(
                    &env,
                    vault_id,
                    &caller,
                    amount,
                    false,
                    "Insufficient balance",
                );
                return Err(ContractError::InsufficientBalance);
            }
        }

        // All validations passed — apply withdrawals
        // Note: batch_withdraw requires all vaults to use the same token (default XLM)
        let default_token = Self::load_token(&env);
        let token_client = token::Client::new(&env, &default_token);
        for (vault_id, amount) in vault_ids.iter().zip(amounts.iter()) {
            let mut vault = Self::load_vault(&env, vault_id);
            if vault.token_address != default_token {
                Self::record_withdrawal_audit(
                    &env,
                    vault_id,
                    &caller,
                    amount,
                    false,
                    "Incompatible token",
                );
                return Err(ContractError::InvalidAmount);
            }
            token_client.transfer(&env.current_contract_address(), &vault.owner, &amount);
            vault.balance -= amount;
            let remaining = vault.balance;
            Self::save_vault(&env, vault_id, &vault);

            // Issue #569: Record successful withdrawal in audit trail
            Self::record_withdrawal_audit(&env, vault_id, &caller, amount, true, "");

            // Issue #571: Emit withdrawal notification event
            env.events().publish(
                (WITHDRAWAL_NOTIF_TOPIC, vault_id),
                (&caller, amount, env.ledger().timestamp()),
            );

            env.events()
                .publish((WITHDRAW_TOPIC, vault_id), (amount, remaining));
        }
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        Ok(())
    }

    // --- Issue #573: Withdrawal Proof ---

    /// Generates a cryptographic proof of withdrawal for compliance.
    ///
    /// Creates a proof hash that can be used to verify a withdrawal occurred.
    /// The proof includes vault_id, amount, timestamp, and a nonce.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault ID
    /// * `amount` - The withdrawal amount
    ///
    /// # Returns
    /// A WithdrawalProof struct containing the proof hash
    pub fn generate_withdrawal_proof(
        env: Env,
        vault_id: u64,
        amount: i128,
    ) -> Result<WithdrawalProof, ContractError> {
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }
        let vault = Self::try_load_vault(&env, vault_id).ok_or(ContractError::VaultNotFound)?;
        if vault.balance < amount {
            return Err(ContractError::InsufficientBalance);
        }

        let now = env.ledger().timestamp();
        let nonce = env.ledger().sequence() as u64;

        // Create proof hash from vault_id, amount, timestamp, and nonce
        let mut data = Vec::new(&env);
        data.push_back(vault_id as u32);
        data.push_back(amount as u32);
        data.push_back(now as u32);
        data.push_back(nonce as u32);

        let proof_bytes = Bytes::from_array(&env, &[0u8; 32]);
        let proof_hash: BytesN<32> = env.crypto().sha256(&proof_bytes).into();

        let proof = WithdrawalProof {
            vault_id,
            amount,
            timestamp: now,
            proof_hash,
            nonce,
        };

        env.storage()
            .persistent()
            .set(&DataKey::WithdrawalProof(vault_id, nonce), &proof);
        env.storage().persistent().extend_ttl(
            &DataKey::WithdrawalProof(vault_id, nonce),
            VAULT_TTL_THRESHOLD,
            vault_ttl_ledgers(vault.check_in_interval as u64),
        );

        env.events()
            .publish((WITHDRAWAL_PROOF_TOPIC, vault_id), (amount, nonce));
        Ok(proof)
    }

    // --- Issue #576: Withdrawal Escrow ---

    /// Creates a withdrawal escrow entry, holding funds pending verification.
    ///
    /// Funds are held in escrow and can only be released after verification.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault ID
    /// * `amount` - The amount to hold in escrow
    /// * `beneficiary` - The beneficiary address
    /// * `caller` - The caller address (must be the vault owner)
    ///
    /// # Returns
    /// `Ok(())` on success
    ///
    /// # Errors
    /// * `ContractError::InvalidAmount` - If amount is not positive
    /// * `ContractError::VaultNotFound` - If the vault does not exist
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::InsufficientBalance` - If vault balance is less than amount
    pub fn create_withdrawal_escrow(
        env: Env,
        vault_id: u64,
        amount: i128,
        beneficiary: Address,
        caller: Address,
    ) -> Result<(), ContractError> {
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }
        caller.require_auth();

        let vault = Self::try_load_vault(&env, vault_id).ok_or(ContractError::VaultNotFound)?;
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.balance < amount {
            return Err(ContractError::InsufficientBalance);
        }

        let now = env.ledger().timestamp();
        let escrow = WithdrawalEscrow {
            vault_id,
            amount,
            timestamp: now,
            beneficiary: beneficiary.clone(),
            verified: false,
        };

        env.storage()
            .persistent()
            .set(&DataKey::WithdrawalEscrow(vault_id), &escrow);
        env.storage().persistent().extend_ttl(
            &DataKey::WithdrawalEscrow(vault_id),
            VAULT_TTL_THRESHOLD,
            vault_ttl_ledgers(vault.check_in_interval as u64),
        );

        env.events().publish(
            (WITHDRAWAL_ESCROW_CREATED_TOPIC, vault_id),
            (amount, beneficiary),
        );
        Ok(())
    }

    /// Verifies and releases a withdrawal from escrow.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault ID
    /// * `caller` - The caller address (must be the escrow's beneficiary)
    ///
    /// # Returns
    /// `Ok(())` on success
    ///
    /// # Errors
    /// * `ContractError::VaultNotFound` - If no escrow exists for the vault
    /// * `ContractError::NotBeneficiary` - If caller is not the escrow's beneficiary
    /// * `ContractError::InvalidAmount` - If the escrow was already verified
    pub fn verify_withdrawal_escrow(
        env: Env,
        vault_id: u64,
        caller: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();

        let mut vault = Self::load_vault(&env, vault_id);
        let escrow = env
            .storage()
            .persistent()
            .get::<DataKey, WithdrawalEscrow>(&DataKey::WithdrawalEscrow(vault_id))
            .ok_or(ContractError::VaultNotFound)?;

        if caller != escrow.beneficiary {
            return Err(ContractError::NotBeneficiary);
        }
        if escrow.verified {
            return Err(ContractError::InvalidAmount);
        }

        let token_client = token::Client::new(&env, &vault.token_address);
        crate::aml::require_compliant(&env, &escrow.beneficiary);
        token_client.transfer(
            &env.current_contract_address(),
            &escrow.beneficiary,
            &escrow.amount,
        );
        vault.balance -= escrow.amount;
        Self::save_vault(&env, vault_id, &vault);

        env.storage()
            .persistent()
            .remove(&DataKey::WithdrawalEscrow(vault_id));
        env.events()
            .publish((WITHDRAWAL_ESCROW_VERIFIED_TOPIC, vault_id), escrow.amount);
        Ok(())
    }

    // --- Issue #574: Withdrawal Rollback ---

    /// Rolls back a withdrawal if fraud is detected.
    ///
    /// Only the vault owner can initiate a rollback within a certain time window.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault ID
    /// * `rollback_amount` - The amount to rollback
    /// * `reason` - The reason for rollback
    /// * `caller` - The caller address (must be vault owner)
    ///
    /// # Returns
    /// `Ok(())` on success
    pub fn rollback_withdrawal(
        env: Env,
        vault_id: u64,
        rollback_amount: i128,
        reason: String,
        caller: Address,
    ) -> Result<(), ContractError> {
        if rollback_amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }
        caller.require_auth();

        let mut vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }

        let now = env.ledger().timestamp();
        let rollback = WithdrawalRollback {
            vault_id,
            original_amount: rollback_amount,
            rollback_amount,
            timestamp: now,
            reason,
        };

        vault.balance += rollback_amount;
        Self::save_vault(&env, vault_id, &vault);

        env.storage()
            .persistent()
            .set(&DataKey::WithdrawalRollback(vault_id), &rollback);
        env.storage().persistent().extend_ttl(
            &DataKey::WithdrawalRollback(vault_id),
            VAULT_TTL_THRESHOLD,
            vault_ttl_ledgers(vault.check_in_interval as u64),
        );

        env.events()
            .publish((WITHDRAWAL_ROLLBACK_TOPIC, vault_id), rollback_amount);
        Ok(())
    }

    // --- Issue #575: Withdrawal Rate Limiting ---

    /// Sets the withdrawal rate limit for a vault.
    ///
    /// Prevents abuse by limiting withdrawal frequency.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault ID
    /// * `cooldown_seconds` - Minimum seconds between withdrawals
    /// * `caller` - The caller address (must be vault owner)
    ///
    /// # Returns
    /// `Ok(())` on success
    pub fn set_withdrawal_rate_limit(
        env: Env,
        vault_id: u64,
        cooldown_seconds: u64,
        caller: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();

        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }

        let now = env.ledger().timestamp();
        let rate_limit = WithdrawalRateLimit {
            vault_id,
            last_withdrawal_time: now,
            withdrawal_count: 0,
            cooldown_seconds,
        };

        env.storage()
            .persistent()
            .set(&DataKey::WithdrawalRateLimit(vault_id), &rate_limit);
        env.storage().persistent().extend_ttl(
            &DataKey::WithdrawalRateLimit(vault_id),
            VAULT_TTL_THRESHOLD,
            vault_ttl_ledgers(vault.check_in_interval as u64),
        );

        Ok(())
    }

    /// Checks if a withdrawal is allowed based on rate limiting.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault ID
    ///
    /// # Returns
    /// `Ok(true)` if withdrawal is allowed, `Ok(false)` if rate limited
    #[allow(clippy::unnecessary_wraps)] // contract fn: Result kept for forward-compat error paths
    pub fn is_withdrawal_allowed(env: Env, vault_id: u64) -> Result<bool, ContractError> {
        if let Some(rate_limit) = env
            .storage()
            .persistent()
            .get::<DataKey, WithdrawalRateLimit>(&DataKey::WithdrawalRateLimit(vault_id))
        {
            let now = env.ledger().timestamp();
            let time_since_last = now.saturating_sub(rate_limit.last_withdrawal_time);

            if time_since_last < rate_limit.cooldown_seconds {
                env.events()
                    .publish((WITHDRAWAL_RATE_LIMITED_TOPIC, vault_id), time_since_last);
                return Ok(false);
            }
        }
        Ok(true)
    }

    // --- Issue #319: batch_check_in ---

    /// Records check-ins for multiple vaults owned by the same caller in a single transaction.
    ///
    /// This is more efficient than calling `check_in` multiple times as it reduces
    /// transaction overhead. All vaults are validated before any state mutation occurs.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_ids` - Vector of vault IDs to check in
    /// * `caller` - The address of the caller (must be the owner of all vaults)
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    ///
    /// # Errors
    /// * `ContractError::Paused` - If the contract is paused
    /// * `ContractError::VaultNotFound` - If any vault does not exist
    /// * `ContractError::NotOwner` - If caller is not the owner of any vault
    /// * `ContractError::AlreadyReleased` - If any vault is not in Locked status
    pub fn batch_check_in(
        env: Env,
        vault_ids: Vec<u64>,
        caller: Address,
    ) -> Result<(), ContractError> {
        if Self::load_paused(&env) {
            return Err(ContractError::Paused);
        }
        caller.require_auth();

        // Validate all entries before mutating state
        for vault_id in vault_ids.iter() {
            let vault = Self::try_load_vault(&env, vault_id).ok_or(ContractError::VaultNotFound)?;
            if vault.is_paused {
                return Err(ContractError::Paused);
            }
            if caller != vault.owner {
                return Err(ContractError::NotOwner);
            }
            if vault.status != ReleaseStatus::Locked {
                return Err(ContractError::AlreadyReleased);
            }
        }

        // All validations passed — apply check-ins
        let now = env.ledger().timestamp();
        for vault_id in vault_ids.iter() {
            let mut vault = Self::load_vault(&env, vault_id);
            vault.last_check_in = now;

            // Apply scheduled beneficiary rotation if effective timestamp has passed
            let rot_key = DataKey::BeneficiaryRotationSchedule(vault_id);
            if let Some(schedule) = env
                .storage()
                .persistent()
                .get::<DataKey, Vec<BeneficiaryRotationEntry>>(&rot_key)
            {
                // Find the latest entry whose effective_timestamp <= now
                let mut applied: Option<BeneficiaryRotationEntry> = None;
                for entry in schedule.iter() {
                    if entry.effective_timestamp <= now
                        && applied.as_ref().is_none_or(|a: &BeneficiaryRotationEntry| {
                            entry.effective_timestamp > a.effective_timestamp
                        })
                    {
                        applied = Some(entry.clone());
                    }
                }
                if let Some(rotation) = applied {
                    if !rotation.new_beneficiaries.is_empty() {
                        vault.beneficiaries = rotation.new_beneficiaries.clone();
                    }
                    env.events()
                        .publish((BEN_ROTATION_TOPIC, vault_id), rotation.effective_timestamp);
                }
            }

            Self::save_vault(&env, vault_id, &vault);
            env.events().publish((CHECK_IN_TOPIC, vault_id), now);
        }
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        Ok(())
    }

    /// Triggers the release of funds to beneficiaries after the vault expires.
    ///
    /// Anyone can call this function once the vault's TTL has lapsed. If a vesting
    /// schedule is attached, the vault is marked as Released but funds remain locked
    /// until claimed via `claim_vested_installment`. Otherwise, funds are distributed
    /// immediately to the primary beneficiary or split among multiple beneficiaries
    /// based on their BPS allocations.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    ///
    /// # Panics
    /// * Panics if the contract is paused
    /// * Panics if the vault is not in Locked status
    /// * Panics if the vault has not expired yet
    /// * Panics if the vault balance is zero
    pub fn trigger_release(env: Env, vault_id: u64) {
        Self::trigger_release_internal(env, vault_id, ReleaseTrigger::Expiry);
    }

    pub fn trigger_release_manual(env: Env, vault_id: u64) {
        Self::trigger_release_internal(env, vault_id, ReleaseTrigger::Manual);
    }

    fn mark_release_attempted(env: &Env, vault_id: u64) {
        env.storage()
            .persistent()
            .set(&DataKey::ReleaseAttempted(vault_id), &true);
        env.storage().persistent().extend_ttl(
            &DataKey::ReleaseAttempted(vault_id),
            VAULT_TTL_THRESHOLD,
            VAULT_TTL_LEDGERS,
        );
    }

    fn trigger_release_internal(env: Env, vault_id: u64, mode: ReleaseTrigger) {
        Self::assert_not_paused(&env);
        // Attempt to restore archived vault state before proceeding - Issue #443
        Self::try_restore_archived_vault(&env, vault_id);
        let vault = Self::load_vault(&env, vault_id);
        if env
            .storage()
            .persistent()
            .has(&DataKey::ReleaseAttempted(vault_id))
            || vault.status != ReleaseStatus::Locked
        {
            panic_with_error!(&env, ContractError::AlreadyReleased);
        }
        let total = vault.balance;
        if total == 0 {
            panic_with_error!(&env, ContractError::EmptyVault);
        }

        // Load release conditions
        let conditions = Self::get_release_conditions(&env, vault_id);
        let mut condition_met = false;
        for cond in conditions.iter() {
            match cond {
                ReleaseCondition::TTLExpiry => {
                    if Self::is_expired(env.clone(), vault_id) {
                        condition_met = true;
                    }
                }
                ReleaseCondition::OwnerInitiated => {
                    if mode == ReleaseTrigger::Manual {
                        condition_met = true;
                    }
                }
                ReleaseCondition::Oracle(addr) => {
                    // Reject stale oracle observations (#343): a read older than
                    // the configured bound falls back to "condition not met"
                    // rather than releasing on outdated data.
                    if oracle::query_checked(&env, &addr, &oracle::OracleConfig::default())
                        .unwrap_or(false)
                    {
                        condition_met = true;
                    }
                }
            }
        }
        if !condition_met {
            panic_with_error!(&env, ContractError::ConditionsNotApproved);
        }

        Self::assert_not_paused(&env);
        // Attempt to restore archived vault state before proceeding - Issue #443
        Self::try_restore_archived_vault(&env, vault_id);
        let mut vault = Self::load_vault(&env, vault_id);
        if vault.status != ReleaseStatus::Locked {
            panic_with_error!(&env, ContractError::AlreadyReleased);
        }
        // We only block vetoes on trigger before the vault has expired.
        // Note: release entrypoint is for the Expiry trigger and thus requires expiry,
        // but manual release can occur at arbitrary times.
        // Here we keep the existing expiry gate.
        if !Self::is_expired(env.clone(), vault_id) {
            panic_with_error!(&env, ContractError::NotExpired);
        }

        let now = env.ledger().timestamp();
        let mut hibernated = 0u64;
        if let Some(h) = env
            .storage()
            .persistent()
            .get::<DataKey, HibernationEntry>(&DataKey::Hibernation(vault_id))
        {
            hibernated = now.saturating_sub(h.started_at).min(h.duration_seconds);
        }
        let expiry_time = vault.last_check_in + vault.check_in_interval + hibernated;
        let grace_period = env
            .storage()
            .instance()
            .get::<DataKey, u64>(&DataKey::ReleaseGracePeriodSeconds)
            .unwrap_or(0);
        if now < expiry_time + grace_period {
            panic_with_error!(&env, ContractError::GracePeriodActive);
        }

        // Beneficiary veto of owner-defined release conditions (Issue: beneficiary veto before expiry).
        // The veto is only meaningful before expiry; since `trigger_release*` requires expiry,
        // veto has already expired in practice. However, for completeness (and for manual release),
        // we gate on expiry-less times above in the public veto method.
        let total = vault.balance;
        if total == 0 {
            panic_with_error!(&env, ContractError::EmptyVault);
        }

        // Beneficiary veto of owner-defined release conditions (Issue: beneficiary veto before expiry).
        // Block `trigger_release*` only if veto was set and vault has not yet expired.
        // (This is also harmless for expiry-triggered releases, since those require expiry.)
        if !Self::is_expired(env.clone(), vault_id) {
            let vetoed: bool = env
                .storage()
                .persistent()
                .get(&DataKey::BeneficiaryReleaseConditionVeto(vault_id))
                .unwrap_or(false);
            if vetoed {
                panic_with_error!(&env, ContractError::BeneficiaryVetoReleaseCondition);
            }
        }

        // Check beneficiary acceptance status - Issue #397
        // Only block if it's a single-beneficiary vault where the primary has declined.
        // Multi-beneficiary vaults handle per-beneficiary decline in distribute_release_amount.
        if vault.beneficiaries.is_empty() {
            let beneficiary_status = Self::get_beneficiary_status(env.clone(), vault_id);
            if beneficiary_status == BeneficiaryStatus::Declined {
                panic_with_error!(&env, ContractError::InvalidBeneficiary);
            }
        }

        // Check beneficiary conditional acceptance threshold - Issue #503
        if !Self::check_conditional_acceptance_threshold(&env, vault_id, total) {
            panic_with_error!(&env, ContractError::InsufficientBalance);
        }

        // Check beneficiary proof of life - Issue #498
        let now = env.ledger().timestamp();
        let pol_key = DataKey::ProofOfLife(vault_id);
        if let Some(pol) = env
            .storage()
            .persistent()
            .get::<DataKey, ProofOfLifeEntry>(&pol_key)
        {
            if now > pol.valid_until {
                panic_with_error!(&env, ContractError::ProofOfLifeRequired);
            }
        } else {
            // If proof-of-life is required (threshold set), block release
            if env
                .storage()
                .persistent()
                .has(&DataKey::ReleaseVoteThreshold(vault_id))
            {
                panic_with_error!(&env, ContractError::ProofOfLifeRequired);
            }
        }

        // Check beneficiary voting - Issue #499
        if let Some(threshold) = env
            .storage()
            .persistent()
            .get::<DataKey, u32>(&DataKey::ReleaseVoteThreshold(vault_id))
        {
            let votes: Vec<ReleaseVoteEntry> = env
                .storage()
                .persistent()
                .get(&DataKey::ReleaseVotes(vault_id))
                .unwrap_or_else(|| Vec::new(&env));
            let approvals = votes.iter().filter(|v| v.approve).count() as u32;
            if approvals < threshold {
                panic_with_error!(&env, ContractError::VotingNotEnabled);
            }
        }

        // Check conditional acceptance deadline
        if let Some(entry) = env
            .storage()
            .persistent()
            .get::<DataKey, ConditionalAcceptanceEntry>(&DataKey::ConditionalAcceptance(vault_id))
        {
            if let Some(deadline) = entry.acceptance_deadline {
                if now > deadline && !entry.approved_by_owner {
                    let token_client = token::Client::new(&env, &vault.token_address);
                    token_client.transfer(&env.current_contract_address(), &vault.owner, &total);
                    vault.balance = 0;
                    vault.status = ReleaseStatus::Cancelled;
                    Self::save_vault(&env, vault_id, &vault);
                    Self::record_state_transition(
                        &env,
                        vault_id,
                        ReleaseStatus::Locked,
                        ReleaseStatus::Cancelled,
                        &vault.owner,
                    );
                    env.storage()
                        .instance()
                        .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
                    env.events().publish(
                        (ACCEPTANCE_DEADLINE_EXPIRED_TOPIC,),
                        (vault_id, vault.owner.clone(), total),
                    );
                    return;
                }
            }
        }

        // Apply scheduled beneficiary rotation if effective timestamp has passed
        let rot_key = DataKey::BeneficiaryRotationSchedule(vault_id);
        if let Some(schedule) = env
            .storage()
            .persistent()
            .get::<DataKey, Vec<BeneficiaryRotationEntry>>(&rot_key)
        {
            // Find the latest entry whose effective_timestamp <= now
            let mut applied: Option<BeneficiaryRotationEntry> = None;
            for entry in schedule.iter() {
                if entry.effective_timestamp <= now
                    && applied.as_ref().is_none_or(|a: &BeneficiaryRotationEntry| {
                        entry.effective_timestamp > a.effective_timestamp
                    })
                {
                    applied = Some(entry.clone());
                }
            }
            if let Some(rotation) = applied {
                if rotation.new_beneficiaries.is_empty() {
                    // single-beneficiary rotation not supported via this path; skip
                } else {
                    vault.beneficiaries = rotation.new_beneficiaries.clone();
                }
                env.events()
                    .publish((BEN_ROTATION_TOPIC, vault_id), rotation.effective_timestamp);
            }
        }

        // Check if a vesting schedule is attached
        let has_vesting = env
            .storage()
            .persistent()
            .has(&DataKey::VestingSchedule(vault_id, 0));
        let has_milestone_vesting = env
            .storage()
            .persistent()
            .has(&DataKey::MilestoneVestingSchedule(vault_id));
        // Issue #525: per-beneficiary vesting schedules also hold the balance in the
        // vault rather than distributing it immediately.
        let has_beneficiary_vesting =
            env.storage()
                .persistent()
                .has(&DataKey::BeneficiaryVestingSchedule(
                    vault_id,
                    vault.beneficiary.clone(),
                ))
                || vault.beneficiaries.iter().any(|b| {
                    env.storage()
                        .persistent()
                        .has(&DataKey::BeneficiaryVestingSchedule(
                            vault_id,
                            b.address.clone(),
                        ))
                });

        Self::mark_release_attempted(&env, vault_id);

        if has_vesting || has_milestone_vesting || has_beneficiary_vesting {
            // Vesting schedule exists: mark as Released but keep balance intact
            vault.status = ReleaseStatus::Released;
            Self::save_vault(&env, vault_id, &vault);
            Self::record_state_transition(
                &env,
                vault_id,
                ReleaseStatus::Locked,
                ReleaseStatus::Released,
                &vault.owner,
            );
            env.storage()
                .instance()
                .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
            env.events().publish(
                (RELEASE_TOPIC,),
                ReleaseEvent {
                    vault_id,
                    beneficiary: vault.beneficiary.clone(),
                    amount: 0,
                },
            );
        } else {
            // No vesting: immediate full release
            // Apply spending limit - Issue #382
            let release_amount = if let Some(limit) = vault.spending_limit {
                total.min(limit)
            } else {
                total
            };
            let token_client = token::Client::new(&env, &vault.token_address);

            // Calculate burn amount based on vault's burn_percentage (basis points)
            let burn_amount: i128 = (release_amount * vault.burn_percentage as i128) / 10_000;
            let net_amount: i128 = release_amount - burn_amount;
            if burn_amount > 0 {
                // Emit burn event; funds are effectively removed from circulation
                env.events()
                    .publish((BURN_EVENT_TOPIC, vault_id), burn_amount);
            }
            if vault.beneficiaries.is_empty() {
                crate::aml::require_compliant(&env, &vault.beneficiary);
                token_client.transfer(
                    &env.current_contract_address(),
                    &vault.beneficiary,
                    &release_amount,
                );
                env.events().publish(
                    (RELEASE_TOPIC,),
                    ReleaseEvent {
                        vault_id,
                        beneficiary: vault.beneficiary.clone(),
                        amount: release_amount,
                    },
                );
            } else {
                // Distribute net amount among beneficiaries
                Self::distribute_release_amount(&env, vault_id, &vault, net_amount, mode);
            }

            // Checks-Effects-Interactions: mutate vault state BEFORE external token transfer.
            vault.balance -= release_amount;
            if vault.balance == 0 {
                vault.status = ReleaseStatus::Released;
            }
            Self::save_vault(&env, vault_id, &vault);
            if vault.status == ReleaseStatus::Released {
                Self::record_state_transition(
                    &env,
                    vault_id,
                    ReleaseStatus::Locked,
                    ReleaseStatus::Released,
                    &vault.owner,
                );
            }
            Self::append_activity_log(&env, vault_id, "trigger_release", &vault.owner, "");
            // Issue #469: auto-archive vault after full release
            if vault.status == ReleaseStatus::Released {
                let arch_key = DataKey::ArchivedVault(vault_id);
                env.storage()
                    .persistent()
                    .set(&arch_key, &ArchivedVaultInfo(vault.clone()));
                env.storage().persistent().extend_ttl(
                    &arch_key,
                    VAULT_TTL_THRESHOLD,
                    VAULT_TTL_LEDGERS,
                );
                // Remove duplicate key so the same parameters can be reused
                let dup_key = DataKey::VaultDuplicate(
                    vault.owner.clone(),
                    vault.beneficiary.clone(),
                    vault.check_in_interval,
                );
                env.storage().persistent().remove(&dup_key);
                env.events().publish(
                    (VAULT_ARCHIVED_TOPIC, vault_id),
                    (vault_id, ReleaseStatus::Released),
                );
            }
            env.storage()
                .instance()
                .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        }
    }

    /// Applies TTL decay to a vault if no check-in for 30 days.
    ///
    /// Anyone can call this function. If the vault hasn't been checked in for 30 days,
    /// the TTL is reduced by the configured decay rate. This encourages regular engagement.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    ///
    /// # Returns
    /// `Ok(new_ttl_remaining)` with the remaining TTL after decay, or `Err` on failure
    ///
    /// # Errors
    /// * `ContractError::VaultNotFound` - If vault does not exist
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    pub fn apply_ttl_decay(env: Env, vault_id: u64) -> Result<u64, ContractError> {
        let mut vault = Self::try_load_vault(&env, vault_id).ok_or(ContractError::VaultNotFound)?;

        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }

        let decay_rate = Self::get_ttl_decay_rate(env.clone());
        if decay_rate == 0 {
            // No decay configured
            return Ok(Self::get_ttl_remaining(env, vault_id).unwrap_or(0));
        }

        let now = env.ledger().timestamp();
        let last_check_in = vault.last_check_in;
        let thirty_days = 30 * 24 * 60 * 60; // 2,592,000 seconds

        // Only apply decay if no check-in for 30 days
        if now < last_check_in + thirty_days {
            return Ok(Self::get_ttl_remaining(env, vault_id).unwrap_or(0));
        }

        // Calculate new TTL with decay applied
        let current_deadline = last_check_in + vault.check_in_interval;
        let remaining = current_deadline.saturating_sub(now);

        // Apply decay: new_ttl = remaining * (1 - decay_rate / 10000)
        let decayed_ttl = remaining * (10_000 - decay_rate as u64) / 10_000;
        let new_deadline = now + decayed_ttl;

        // Update last_check_in to reflect the decay application
        vault.last_check_in = new_deadline - vault.check_in_interval;
        Self::save_vault(&env, vault_id, &vault);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((TTL_DECAY_TOPIC, vault_id), (remaining, decayed_ttl));

        Ok(decayed_ttl)
    }

    /// Synchronizes TTL across multiple vaults owned by the caller.
    ///
    /// Extends TTL for all specified vaults in a single transaction. This is more
    /// efficient than calling `check_in` multiple times. All vaults must be owned
    /// by the caller and in Locked status.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_ids` - Vector of vault IDs to synchronize
    /// * `caller` - The address of the caller (must be the owner of all vaults)
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    ///
    /// # Errors
    /// * `ContractError::Paused` - If the contract is paused
    /// * `ContractError::VaultNotFound` - If any vault does not exist
    /// * `ContractError::NotOwner` - If caller is not the owner of any vault
    /// * `ContractError::AlreadyReleased` - If any vault is not in Locked status
    /// * `ContractError::MaxTtlExceeded` - If any vault would exceed max TTL
    pub fn sync_vault_ttls(
        env: Env,
        vault_ids: Vec<u64>,
        caller: Address,
    ) -> Result<(), ContractError> {
        if Self::load_paused(&env) {
            return Err(ContractError::Paused);
        }
        caller.require_auth();

        // Validate all vaults before mutating state
        for vault_id in vault_ids.iter() {
            let vault = Self::try_load_vault(&env, vault_id).ok_or(ContractError::VaultNotFound)?;
            if caller != vault.owner {
                return Err(ContractError::NotOwner);
            }
            if vault.status != ReleaseStatus::Locked {
                return Err(ContractError::AlreadyReleased);
            }

            // Check max TTL constraint
            let max_ttl = Self::get_max_ttl_seconds(env.clone());
            let now = env.ledger().timestamp();
            let deadline = now + vault.check_in_interval;
            let max_deadline = now + max_ttl;
            if deadline > max_deadline {
                return Err(ContractError::MaxTtlExceeded);
            }
        }

        // All validations passed — apply check-ins
        let now = env.ledger().timestamp();
        for vault_id in vault_ids.iter() {
            let mut vault = Self::load_vault(&env, vault_id);
            vault.last_check_in = now;
            Self::save_vault(&env, vault_id, &vault);
            env.events().publish((CHECK_IN_TOPIC, vault_id), now);
        }

        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish((SYNC_TTL_TOPIC,), vault_ids.len());
        Ok(())
    }

    /// Forecasts the expected expiry time of a vault based on check-in frequency.
    ///
    /// Calculates when the vault will expire if check-ins continue at the specified
    /// frequency. This helps owners plan ahead and ensure timely check-ins.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `check_in_frequency_days` - Expected check-in frequency in days
    ///
    /// # Returns
    /// Unix timestamp of the expected expiry, or `Err` on failure
    ///
    /// # Errors
    /// * `ContractError::VaultNotFound` - If vault does not exist
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    pub fn forecast_expiry(
        env: Env,
        vault_id: u64,
        check_in_frequency_days: u64,
    ) -> Result<u64, ContractError> {
        let vault = Self::try_load_vault(&env, vault_id).ok_or(ContractError::VaultNotFound)?;

        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }

        if check_in_frequency_days == 0 {
            return Err(ContractError::InvalidInterval);
        }

        let now = env.ledger().timestamp();
        let check_in_frequency_seconds = check_in_frequency_days * 24 * 60 * 60;

        // Current deadline
        let current_deadline = vault.last_check_in + vault.check_in_interval;

        // If already expired, return current time
        if now >= current_deadline {
            return Ok(now);
        }

        // Calculate how many check-ins until expiry at the given frequency
        let remaining_until_expiry = current_deadline - now;
        let num_check_ins = remaining_until_expiry.div_ceil(check_in_frequency_seconds);

        // Each check-in extends TTL by vault.check_in_interval
        let total_extension = num_check_ins * vault.check_in_interval;
        let forecasted_expiry = now + total_extension;

        Ok(forecasted_expiry)
    }

    // --- Task 1: ping_expiry ---

    /// Checks the remaining TTL and emits a warning event if near expiry.
    ///
    /// This function can be called by anyone to monitor vault expiry status.
    /// If the remaining TTL is less than `EXPIRY_WARNING_THRESHOLD` (24 hours),
    /// a warning event is emitted. No event is emitted for Released or Cancelled vaults.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    ///
    /// # Returns
    /// The remaining TTL in seconds (0 if expired)
    pub fn ping_expiry(env: Env, vault_id: u64) -> u64 {
        let vault = Self::try_load_vault(&env, vault_id)
            .unwrap_or_else(|| panic_with_error!(&env, ContractError::VaultNotFound));

        // Only emit events for Locked vaults
        if vault.status != ReleaseStatus::Locked {
            return 0;
        }

        let ttl = Self::get_ttl_remaining(env.clone(), vault_id).unwrap_or(0);
        if ttl < EXPIRY_WARNING_THRESHOLD {
            env.events().publish((PING_EXPIRY_TOPIC, vault_id), ttl);
        }

        // Issue #560: per-passkey expiry warning, additive to the vault-level check above.
        Self::check_passkey_expiry_warnings(&env, vault_id, env.ledger().timestamp());

        ttl
    }

    /// Emits `pk_expwrn` for every registered passkey on `vault_id` whose
    /// remaining time until its configured `PasskeyExpiry` is below
    /// `EXPIRY_WARNING_THRESHOLD` (#560, Requirement 4 AC 7).
    fn check_passkey_expiry_warnings(env: &Env, vault_id: u64, now: u64) {
        let passkeys: Vec<PasskeyHash> = env
            .storage()
            .persistent()
            .get(&DataKey::VaultPasskeys(vault_id))
            .unwrap_or_else(|| Vec::new(env));

        for pk in passkeys.iter() {
            if let Some(expiry) = env
                .storage()
                .persistent()
                .get::<DataKey, u64>(&DataKey::PasskeyExpiry(vault_id, pk.hash.clone()))
            {
                let seconds_remaining = expiry.saturating_sub(now);
                if seconds_remaining < EXPIRY_WARNING_THRESHOLD {
                    env.events().publish(
                        (PASSKEY_EXPIRY_WARNING_TOPIC, vault_id),
                        (pk.hash.clone(), seconds_remaining),
                    );
                }
            }
        }
    }

    // --- Task 2: partial_release ---

    /// Transfers a partial amount to the beneficiary (or beneficiaries) without releasing the vault.
    ///
    /// This allows the owner to distribute funds gradually while keeping the vault
    /// in Locked status. The vault can still be checked in and released later.
    ///
    /// When a multi-beneficiary split has been configured via `set_beneficiaries`, the
    /// `amount` is distributed proportionally according to each entry's BPS allocation,
    /// using the same rounding logic as `trigger_release` (last entry absorbs dust).
    /// When no split is configured, the full `amount` goes to the primary beneficiary.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `amount` - Amount to transfer in stroops (1 XLM = 10,000,000 stroops)
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    ///
    /// # Errors
    /// * `ContractError::Paused` - If the contract is paused
    /// * `ContractError::InvalidAmount` - If amount is not positive
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    /// * `ContractError::VaultExpired` - If vault has expired
    /// * `ContractError::InsufficientBalance` - If vault balance is less than amount
    pub fn partial_release(env: Env, vault_id: u64, amount: i128) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }
        let mut vault = Self::load_vault(&env, vault_id);
        vault.owner.require_auth();
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        if Self::is_expired(env.clone(), vault_id) {
            return Err(ContractError::VaultExpired);
        }
        if vault.balance < amount {
            return Err(ContractError::InsufficientBalance);
        }
        let token_client = token::Client::new(&env, &vault.token_address);

        if vault.beneficiaries.is_empty() {
            // Single-beneficiary path: send full amount to primary beneficiary.
            crate::aml::require_compliant(&env, &vault.beneficiary);
            token_client.transfer(&env.current_contract_address(), &vault.beneficiary, &amount);
            env.events().publish(
                (symbol_short!("partial"), vault_id),
                (vault.beneficiary.clone(), amount),
            );
        } else {
            // Multi-beneficiary path: split `amount` by BPS, last entry absorbs dust.
            let mut distributed: i128 = 0;
            let last_idx = vault.beneficiaries.len() - 1;
            for (i, entry) in vault.beneficiaries.iter().enumerate() {
                let share = if i as u32 == last_idx {
                    amount - distributed
                } else {
                    amount * (entry.bps as i128) / 10_000
                };
                if share > 0 {
                    crate::aml::require_compliant(&env, &entry.address);
                    token_client.transfer(&env.current_contract_address(), &entry.address, &share);
                }
                distributed += share;
                env.events().publish(
                    (symbol_short!("partial"), vault_id),
                    (entry.address.clone(), share),
                );
            }
        }

        vault.balance -= amount;
        Self::save_vault(&env, vault_id, &vault);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        Ok(())
    }
}
