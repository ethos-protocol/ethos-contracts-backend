#[contractimpl]
impl TtlVaultContract {
    // --- vault lifecycle ---

    /// Creates a new time-locked vault.
    ///
    /// The vault starts with a zero balance and must be funded via `deposit`
    /// or `batch_deposit` before it can hold assets.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `owner` - The address of the vault owner (must authorize)
    /// * `beneficiary` - The address that will receive funds when the vault expires
    /// * `check_in_interval` - Time interval in seconds between required check-ins
    ///
    /// # Returns
    /// The unique vault ID
    ///
    /// # Panics
    /// * Panics if `check_in_interval` is 0
    /// * Panics if `check_in_interval` is outside the configured min/max bounds
    pub fn create_vault(
        env: Env,
        owner: Address,
        beneficiary: Address,
        check_in_interval: u64,
        token_address: Option<Address>,
    ) -> u64 {
        owner.require_auth();
        Self::require_initialized(&env);
        // Issue #811: admin must not own vaults to prevent self-dealing on releases
        if owner == Self::load_admin(&env) {
            panic_with_error!(&env, ContractError::AdminCannotOwnVault);
        }
        Self::assert_not_blacklisted(&env, &owner);
        if check_in_interval == 0 {
            panic_with_error!(&env, ContractError::InvalidInterval);
        }

        Self::assert_interval_in_bounds(&env, check_in_interval);

        if owner == beneficiary {
            panic_with_error!(&env, ContractError::InvalidBeneficiary);
        }
        // Issue #547: screen the beneficiary against AML/sanctions data.
        aml::require_compliant(&env, &beneficiary);

        // Issue #551: owner and beneficiary must pass blacklist/allowlist screening
        Self::assert_compliant(&env, &owner);
        Self::assert_compliant(&env, &beneficiary);

        // Detect duplicate: same (owner, beneficiary, check_in_interval) already Locked
        let dup_key =
            DataKey::VaultDuplicate(owner.clone(), beneficiary.clone(), check_in_interval);
        if env.storage().persistent().has(&dup_key) {
            let existing_id: u64 = env
                .storage()
                .persistent()
                .get(&dup_key)
                .expect("just checked has(&dup_key) above");
            env.events().publish(
                (DUPLICATE_VAULT_TOPIC,),
                (owner, beneficiary, check_in_interval, existing_id),
            );
            panic_with_error!(&env, ContractError::DuplicateVault);
        }

        // Issue #470: enforce per-owner vault capacity limit
        let limit: u32 = env
            .storage()
            .instance()
            .get(&DataKey::OwnerVaultCount(env.current_contract_address()))
            .unwrap_or(0u32);
        if limit > 0 {
            let current_count = Self::load_owner_vault_ids(&env, &owner).len() as u32;
            if current_count >= limit {
                panic_with_error!(&env, ContractError::VaultCapacityExceeded);
            }
        }

        // Enforce per-beneficiary vault capacity limit
        Self::assert_beneficiary_capacity(&env, &beneficiary);

        // Use provided token or default to contract's XLM token
        let vault_token = match token_address {
            Some(addr) => {
                // Validate token is whitelisted
                Self::assert_token_whitelisted(&env, &addr);
                addr
            }
            None => Self::load_token(&env),
        };

        let vault_id = Self::vault_count(env.clone()) + 1;
        let timestamp = env.ledger().timestamp();
        let metadata = String::from_str(&env, "");
        Self::assert_metadata_len(&env, &metadata);
        let vault = Vault {
            owner: owner.clone(),
            beneficiary: beneficiary.clone(),
            balance: 0,
            check_in_interval,
            last_check_in: timestamp,
            created_at: timestamp,
            status: ReleaseStatus::Locked,
            beneficiaries: Vec::new(&env),
            metadata,
            token_address: vault_token,
            custom_metadata: Bytes::new(&env),
            is_paused: false,
            release_condition: ReleaseCondition::TTLExpiry,
            parent_vault_id: None,
            passkey_hash: None,
            max_deposit_amount: None,
            withdrawal_approval_threshold: None,
            spending_limit: None,
            burn_percentage: 0,
            inactivity_penalty_bps: None,
            penalty_recipient: None,
        };
        Self::save_vault(&env, vault_id, &vault);
        Self::add_owner_vault_id(&env, &owner, vault_id, check_in_interval);
        Self::add_beneficiary_vault_id(&env, &beneficiary, vault_id, check_in_interval);
        // Initialize empty passkeys and backup codes
        let empty_passkeys: Vec<PasskeyHash> = Vec::new(&env);
        let empty_codes: Vec<BackupCode> = Vec::new(&env);
        env.storage()
            .persistent()
            .set(&DataKey::VaultPasskeys(vault_id), &empty_passkeys);
        env.storage()
            .persistent()
            .set(&DataKey::BackupCodes(vault_id), &empty_codes);
        let ttl = vault_ttl_ledgers(check_in_interval);
        env.storage().persistent().extend_ttl(
            &DataKey::VaultPasskeys(vault_id),
            VAULT_TTL_THRESHOLD,
            ttl,
        );
        env.storage().persistent().extend_ttl(
            &DataKey::BackupCodes(vault_id),
            VAULT_TTL_THRESHOLD,
            ttl,
        );
        // VaultCount is an incrementing generation ID and must be updated
        // only after the vault and its owner/beneficiary indexes are persisted.
        //
        // Ordering guarantee:
        //  1) Compute next ID from current vault count
        //  2) Persist the vault and owner/beneficiary indexes
        //  3) Persist VaultCount only after the vault is fully saved
        // If any prior call (save_vault/add_owner_vault_id/add_beneficiary_vault_id)
        // panics, VaultCount remains unchanged and consumers cannot observe
        // a hole in the sequence.
        let key = DataKey::VaultCount;
        env.storage().persistent().set(&key, &vault_id);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, VAULT_TTL_LEDGERS);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        Self::append_activity_log(&env, vault_id, "create_vault", &owner, "");
        // Store duplicate key to prevent duplicate creation
        env.storage().persistent().set(&dup_key, &vault_id);
        env.storage().persistent().extend_ttl(
            &dup_key,
            VAULT_TTL_THRESHOLD,
            vault_ttl_ledgers(check_in_interval),
        );
        env.events().publish(
            (VAULT_CREATED_TOPIC,),
            (vault_id, owner, beneficiary, check_in_interval, timestamp),
        );
        vault_id
    }

    /// Records a check-in to reset the vault's expiry timer.
    ///
    /// The caller must be the vault owner. This function resets the `last_check_in`
    /// timestamp, extending the vault's TTL.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `caller` - The address of the caller (must be the vault owner)
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    ///
    /// # Errors
    /// * `ContractError::Paused` - If the contract is paused
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    pub fn check_in(
        env: Env,
        vault_id: u64,
        caller: Address,
        passkey_hash: BytesN<32>,
        nonce: u64,
    ) -> Result<(), ContractError> {
        if Self::load_paused(&env) {
            return Err(ContractError::Paused);
        }
        caller.require_auth();
        let mut vault = Self::load_vault(&env, vault_id);
        if vault.is_paused {
            return Err(ContractError::Paused);
        }
        let is_delegate =
            caller != vault.owner && Self::is_check_in_delegate(&env, vault_id, &caller);
        // Issue #557: a caller holding a valid, non-expired delegation for this specific
        // passkey may also check in on the owner's behalf. This is a separate, simpler
        // mechanism than the general check-in delegate list above and is not subject to
        // its nonce-replay protection, since the passkey hash itself is already validated
        // below via `validate_passkey_for_checkin`.
        let is_passkey_delegate = caller != vault.owner
            && !is_delegate
            && Self::is_valid_passkey_delegate(&env, vault_id, &passkey_hash, &caller);
        if caller != vault.owner && !is_delegate && !is_passkey_delegate {
            return Err(ContractError::NotOwner);
        }
        // Enforce per-delegation nonce to prevent replay attacks
        if is_delegate {
            let nonce_key = DataKey::DelegateNonce(vault_id, caller.clone());
            let expected: u64 = env.storage().persistent().get(&nonce_key).unwrap_or(0);
            if nonce != expected {
                return Err(ContractError::InvalidNonce);
            }
            env.storage().persistent().set(&nonce_key, &(expected + 1));
            let ttl = vault_ttl_ledgers(vault.check_in_interval);
            env.storage()
                .persistent()
                .extend_ttl(&nonce_key, VAULT_TTL_THRESHOLD, ttl);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }

        // Save original state for rollback on failure - Issue #391
        let original_last_check_in = vault.last_check_in;

        let now = env.ledger().timestamp();

        // Rate limiting: enforce minimum cooldown between check-ins
        let cooldown: u64 = env
            .storage()
            .instance()
            .get(&DataKey::MinCheckInCooldown)
            .unwrap_or(DEFAULT_MIN_CHECKIN_COOLDOWN);
        if cooldown > 0 {
            if let Some(last) = env
                .storage()
                .persistent()
                .get::<DataKey, u64>(&DataKey::LastCheckInTime(vault_id))
            {
                if now < last + cooldown {
                    return Err(ContractError::InvalidInterval);
                }
            }
        }

        // Issue #549: enforce passkey registration and expiry.
        Self::validate_passkey_for_checkin(&env, vault_id, &vault, &passkey_hash, now)?;

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

        // Inactivity penalty: deduct per missed check-in interval
        if let (Some(penalty_bps), Some(recipient)) = (
            vault.inactivity_penalty_bps,
            vault.penalty_recipient.clone(),
        ) {
            let elapsed = now.saturating_sub(original_last_check_in);
            let missed = (elapsed / vault.check_in_interval).saturating_sub(1);
            if missed > 0 && vault.balance > 0 {
                let penalty_per = vault.balance * (penalty_bps as i128) / 10_000;
                let total_penalty = (penalty_per * missed as i128).min(vault.balance);
                if total_penalty > 0 {
                    let token_client = token::Client::new(&env, &vault.token_address);
                    crate::aml::require_compliant(&env, &recipient);
                    token_client.transfer(
                        &env.current_contract_address(),
                        &recipient,
                        &total_penalty,
                    );
                    vault.balance -= total_penalty;
                    env.events().publish(
                        (INACTIVITY_PENALTY_TOPIC, vault_id),
                        (total_penalty, recipient),
                    );
                }
            }
        }

        // Cap TTL at max_ttl_seconds
        let max_ttl = Self::get_max_ttl_seconds(env.clone());
        let deadline = now + vault.check_in_interval;
        let max_deadline = now + max_ttl;
        if deadline > max_deadline {
            // Rollback on failure - Issue #391
            let _ = original_last_check_in;
            return Err(ContractError::MaxTtlExceeded);
        }

        // Attempt to save vault - if this fails, TTL is not extended
        Self::save_vault(&env, vault_id, &vault);
        let owner_ids = Self::load_owner_vault_ids(&env, &vault.owner);
        Self::save_owner_vault_ids(&env, &vault.owner, &owner_ids, vault.check_in_interval);

        // Log passkey usage - Issue #395
        Self::log_passkey_usage(&env, vault_id, &passkey_hash, now);
        // Issue #558: passkey audit trail
        Self::log_passkey_audit_entry(&env, vault_id, "use", &caller, &passkey_hash);

        // Persist last check-in time for rate limiting
        let lci_key = DataKey::LastCheckInTime(vault_id);
        let lci_ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&lci_key, &now);
        env.storage()
            .persistent()
            .extend_ttl(&lci_key, VAULT_TTL_THRESHOLD, lci_ttl);

        // Issue #478: record history for adaptive interval
        Self::record_check_in_history(&env, vault_id, now);
        // Issue #479: update streak
        Self::update_check_in_streak(&env, vault_id, &vault, now);

        Self::log_audit_entry(&env, vault_id, "check_in", &caller, "");
        Self::append_activity_log(&env, vault_id, "check_in", &caller, "");
        // Reset countdown fired flags so thresholds fire again on the new cycle
        env.storage()
            .persistent()
            .remove(&DataKey::CountdownFired(vault_id));
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((CHECK_IN_TOPIC, vault_id), vault.last_check_in);
        Ok(())
    }
    ///
    /// Transfers tokens from the caller to the contract and increases the vault's balance.
    /// The vault must be in Locked status.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `from` - The address depositing funds (must authorize)
    /// * `amount` - Amount to deposit in stroops (1 XLM = 10,000,000 stroops)
    ///
    /// # Panics
    /// * Panics if the contract is paused
    /// * Panics if `amount` is not positive
    /// * Panics if the vault is not in Locked status
    pub fn deposit(env: Env, vault_id: u64, from: Address, amount: i128) {
        Self::assert_not_paused(&env);
        Self::require_initialized(&env);
        if amount <= 0 {
            panic_with_error!(&env, ContractError::InvalidAmount);
        }
        from.require_auth();
        let mut vault = Self::load_vault(&env, vault_id);
        if from != vault.owner {
            panic_with_error!(&env, ContractError::UnauthorizedDepositor);
        }
        Self::assert_not_blacklisted(&env, &from);
        if vault.is_paused {
            panic_with_error!(&env, ContractError::Paused);
        }
        // Issues #551 / #548: compliance screening and high-value KYC gate
        Self::assert_compliant(&env, &from);
        if let Err(e) = compliance::check_kyc_for_amount(&env, &from, amount) {
            panic_with_error!(&env, e);
        }
        if vault.status != ReleaseStatus::Locked {
            panic_with_error!(&env, ContractError::AlreadyReleased);
        }

        let now = env.ledger().timestamp();
        if now >= vault.last_check_in + vault.check_in_interval {
            panic_with_error!(&env, ContractError::VaultExpired);
        }

        // Check deposit limit - Issue #403
        if let Some(max_deposit) = vault.max_deposit_amount {
            let new_balance = vault
                .balance
                .checked_add(amount)
                .unwrap_or_else(|| panic_with_error!(&env, ContractError::BalanceOverflow));
            if new_balance > max_deposit {
                panic_with_error!(&env, ContractError::DepositLimitExceeded);
            }
        }

        // Use vault's token instead of default XLM
        let token_client = token::Client::new(&env, &vault.token_address);
        token_client.transfer(&from, &env.current_contract_address(), &amount);
        vault.balance = vault
            .balance
            .checked_add(amount)
            .unwrap_or_else(|| panic_with_error!(&env, ContractError::BalanceOverflow));
        Self::save_vault(&env, vault_id, &vault);
        // Issues #549 / #550: record for reporting and run threshold monitoring
        compliance::record_transaction(
            &env,
            compliance::TxKind::Deposit,
            vault_id,
            &from,
            &env.current_contract_address(),
            &vault.token_address,
            amount,
            &from,
        );
        Self::log_audit_entry(&env, vault_id, "deposit", &from, "");
        Self::append_activity_log(&env, vault_id, "deposit", &from, "");
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((DEPOSIT_TOPIC, vault_id), (amount, vault.balance));
    }

    /// Deposits funds into multiple vaults in a single transfer.
    ///
    /// This is more efficient than calling `deposit` multiple times as it only
    /// requires one token transfer. All vaults must be in Locked status and not expired.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `from` - The address depositing funds (must authorize)
    /// * `deposit` - Vector of (vault_id, amount) pairs where amount is in stroops (1 XLM = 10,000,000 stroops)
    ///
    /// # Panics
    /// * Panics if the contract is paused
    /// * Panics if any amount is not positive
    /// * Panics if any vault is not in Locked status
    /// * Panics if any vault has expired
    /// * Panics if any vault uses a non-whitelisted token
    /// * Panics if the total amount overflows
    pub fn batch_deposit(env: Env, from: Address, deposits: Vec<(u64, i128)>) {
        Self::assert_not_paused(&env);
        from.require_auth();

        let mut validated = Vec::new(&env);
        let mut total_amount = 0i128;
        let default_token = Self::load_token(&env);

        for deposit in deposits.iter() {
            let (vault_id, amount) = deposit;
            if amount <= 0 {
                panic_with_error!(&env, ContractError::InvalidAmount);
            }

            let vault = Self::load_vault(&env, vault_id);
            if vault.status != ReleaseStatus::Locked {
                panic_with_error!(&env, ContractError::AlreadyReleased);
            }

            let now = env.ledger().timestamp();
            if now >= vault.last_check_in + vault.check_in_interval {
                panic_with_error!(&env, ContractError::VaultExpired);
            }

            // Issue #582: Validate token whitelist
            Self::assert_token_whitelisted(&env, &vault.token_address);

            total_amount = total_amount
                .checked_add(amount)
                .unwrap_or_else(|| panic_with_error!(&env, ContractError::InvalidAmount));
            validated.push_back((vault_id, vault, amount));
        }

        if total_amount == 0 {
            env.storage()
                .instance()
                .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
            return;
        }

        // Note: batch_deposit now requires all vaults to use the same token (default XLM)
        // For multi-token support, use individual deposit calls
        let token_client = token::Client::new(&env, &default_token);
        token_client.transfer(&from, &env.current_contract_address(), &total_amount);

        for validated_deposit in validated.iter() {
            let (vault_id, mut vault, amount) = validated_deposit;
            // Verify vault uses default token
            if vault.token_address != default_token {
                panic_with_error!(&env, ContractError::InvalidAmount);
            }
            vault.balance = vault
                .balance
                .checked_add(amount)
                .unwrap_or_else(|| panic_with_error!(&env, ContractError::BalanceOverflow));
            Self::save_vault(&env, vault_id, &vault);

            // Issue #582: Emit token whitelist validation event
            env.events().publish(
                (TOKEN_WHITELIST_VALIDATED_TOPIC, vault_id),
                (&vault.token_address, amount),
            );
        }
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
    }

    /// Records a withdrawal attempt (successful or failed) to the per-vault audit
    /// trail and emits the corresponding audit/failure events - Issue #569.
    fn record_withdrawal_audit(
        env: &Env,
        vault_id: u64,
        caller: &Address,
        amount: i128,
        success: bool,
        error_reason: &str,
    ) {
        let timestamp = env.ledger().timestamp();
        let audit_entry = WithdrawalAuditEntry {
            vault_id,
            caller: caller.clone(),
            amount,
            timestamp,
            success,
            error_reason: String::from_str(env, error_reason),
        };

        let key = DataKey::WithdrawalAuditLog(vault_id);
        let mut audit_log: Vec<WithdrawalAuditEntry> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(env));

        audit_log.push_back(audit_entry);

        let ttl = vault_ttl_ledgers(Self::load_vault(env, vault_id).check_in_interval);
        env.storage().persistent().set(&key, &audit_log);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);

        env.events().publish(
            (WITHDRAWAL_AUDIT_TOPIC, vault_id),
            (caller, amount, success, timestamp),
        );

        if !success {
            env.events().publish(
                (WITHDRAWAL_FAILED_TOPIC, vault_id),
                (caller, amount, String::from_str(env, error_reason)),
            );
        }
    }

    /// Owner withdraws from the vault.
    ///
    /// This function is owner-only and is unaffected by any multi-beneficiary
    /// split configured via `set_beneficiaries`. Beneficiary splits only apply
    /// during `trigger_release` and `partial_release`; `withdraw` always sends
    /// funds directly back to the vault owner regardless of the beneficiaries list.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `amount` - Amount to withdraw in stroops (1 XLM = 10,000,000 stroops)
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    ///
    /// # Errors
    /// * `ContractError::Paused` - If the contract is paused
    /// * `ContractError::InvalidAmount` - If amount is not positive
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    /// * `ContractError::InsufficientBalance` - If vault balance is less than amount
    pub fn withdraw(
        env: Env,
        vault_id: u64,
        caller: Address,
        amount: i128,
    ) -> Result<(), ContractError> {
        if Self::load_paused(&env) {
            let _timestamp = env.ledger().timestamp();
            Self::record_withdrawal_audit(
                &env,
                vault_id,
                &caller,
                amount,
                false,
                "Contract paused",
            );
            return Err(ContractError::Paused);
        }
        if amount <= 0 {
            let _timestamp = env.ledger().timestamp();
            Self::record_withdrawal_audit(&env, vault_id, &caller, amount, false, "Invalid amount");
            return Err(ContractError::InvalidAmount);
        }
        caller.require_auth();
        let mut vault = Self::load_vault(&env, vault_id);
        if vault.is_paused {
            Self::record_withdrawal_audit(&env, vault_id, &caller, amount, false, "Vault paused");
            return Err(ContractError::Paused);
        }
        if caller != vault.owner {
            Self::record_withdrawal_audit(&env, vault_id, &caller, amount, false, "Not owner");
            return Err(ContractError::NotOwner);
        }
        if vault.status == ReleaseStatus::EmergencyFrozen {
            Self::record_withdrawal_audit(&env, vault_id, &caller, amount, false, "Vault frozen");
            return Err(ContractError::VaultFrozen);
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
        if vault.owner == env.current_contract_address() {
            Self::record_withdrawal_audit(
                &env,
                vault_id,
                &caller,
                amount,
                false,
                "Invalid withdraw destination",
            );
            return Err(ContractError::InvalidWithdrawDestination);
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

        // Check withdrawal approval threshold - Issue #404
        if let Some(threshold) = vault.withdrawal_approval_threshold {
            if amount > threshold {
                Self::record_withdrawal_audit(
                    &env,
                    vault_id,
                    &caller,
                    amount,
                    false,
                    "Withdrawal not approved",
                );
                return Err(ContractError::WithdrawalNotApproved);
            }
        }

        // Check withdrawal limits - Issue #566
        Self::check_withdrawal_limits(&env, vault_id, amount)?;

        // Check whitelist - Issue #567
        if !Self::is_whitelisted(&env, vault_id, &vault.owner) {
            return Err(ContractError::WithdrawalDestinationNotWhitelisted);
        }

        // Issues #551 / #548: compliance screening and high-value KYC gate
        if let Err(e) = compliance::check_address(&env, &vault.owner)
            .and_then(|()| compliance::check_kyc_for_amount(&env, &vault.owner, amount))
        {
            Self::record_withdrawal_audit(
                &env,
                vault_id,
                &caller,
                amount,
                false,
                "Compliance check failed",
            );
            return Err(e);
        }

        let token_client = token::Client::new(&env, &vault.token_address);
        token_client.transfer(&env.current_contract_address(), &vault.owner, &amount);
        vault.balance -= amount;

        // Issues #549 / #550: record for reporting and run threshold monitoring
        compliance::record_transaction(
            &env,
            compliance::TxKind::Withdrawal,
            vault_id,
            &env.current_contract_address(),
            &vault.owner,
            &vault.token_address,
            amount,
            &vault.owner,
        );

        // Record withdrawal for reversal - Issue #568 (grace period: 24 hours)
        Self::record_withdrawal_for_reversal(&env, vault_id, amount, 86_400);

        Self::save_vault(&env, vault_id, &vault);
        Self::log_audit_entry(&env, vault_id, "withdraw", &caller, "");
        Self::append_activity_log(&env, vault_id, "withdraw", &caller, "");

        // Issue #569: Record successful withdrawal in audit trail
        Self::record_withdrawal_audit(&env, vault_id, &caller, amount, true, "");

        // Issue #571: Emit withdrawal notification event
        env.events().publish(
            (WITHDRAWAL_NOTIF_TOPIC, vault_id),
            (&caller, amount, env.ledger().timestamp()),
        );

        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((WITHDRAW_TOPIC, vault_id), (amount, vault.balance));
        Ok(())
    }
}
