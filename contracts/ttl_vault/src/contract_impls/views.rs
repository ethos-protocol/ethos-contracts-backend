#[contractimpl]
impl TtlVaultContract {
    // --- views ---

    /// Checks if a vault has expired based on the check-in interval.
    ///
    /// A vault is considered expired when the current timestamp is greater than
    /// or equal to the deadline (last_check_in + check_in_interval).
    ///
    /// # Ledger time monotonicity assumption
    /// This function relies on `env.ledger().timestamp()` being monotonically
    /// non-decreasing across ledger closes. The Stellar consensus protocol
    /// guarantees this property: each ledger's close time must be strictly
    /// greater than the previous ledger's close time. Clock skew between
    /// individual validators does not affect this guarantee because the
    /// agreed-upon close time is determined by consensus, not by any single
    /// node's wall clock. Therefore the comparison `now >= deadline` is
    /// reliable and will never produce a false expiry due to time going
    /// backwards.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    ///
    /// # Returns
    /// `true` if the vault has expired, `false` otherwise
    ///
    /// # Panics
    /// Panics if the vault does not exist
    pub fn is_expired(env: Env, vault_id: u64) -> bool {
        let vault = Self::load_vault(&env, vault_id);
        let now = env.ledger().timestamp();
        // Compute how many seconds of hibernation have elapsed (capped at duration).
        let hibernated = if let Some(h) = env
            .storage()
            .persistent()
            .get::<DataKey, HibernationEntry>(&DataKey::Hibernation(vault_id))
        {
            let elapsed = now.saturating_sub(h.started_at).min(h.duration_seconds);
            // If still inside the hibernation window, vault cannot expire.
            if elapsed < h.duration_seconds {
                return false;
            }
            h.duration_seconds
        } else {
            0u64
        };
        now >= vault.last_check_in + vault.check_in_interval + hibernated
    }

    /// Retrieves a vault by its unique identifier.
    ///
    /// This is a pure read-only function. It does **not** extend the vault's
    /// persistent storage TTL. Extending TTL on every read would introduce
    /// unintended side effects: callers (including off-chain indexers) would
    /// inadvertently pay storage fees and mutate ledger state. TTL extension
    /// is intentionally reserved for state-mutating operations such as
    /// `check_in`, `deposit`, and `withdraw`. If a vault is only ever read
    /// and never written to, its storage TTL will eventually lapse and the
    /// entry will be archived by the network.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    ///
    /// # Returns
    /// The `Vault` struct containing all vault data
    ///
    /// # Panics
    /// Panics if the vault does not exist (use `vault_exists` to check first)
    pub fn get_vault(env: Env, vault_id: u64) -> Vault {
        Self::load_vault(&env, vault_id)
    }

    /// Captures the state of a vault at a specific point in time.
    /// Anyone can call this function.
    pub fn create_vault_snapshot(env: Env, vault_id: u64) -> Bytes {
        let vault = Self::load_vault(&env, vault_id);
        let timestamp = env.ledger().timestamp();
        let vault_bytes = vault.clone().to_xdr(&env);
        let content_hash: BytesN<32> = env.crypto().sha256(&vault_bytes).into();

        let snapshot = VaultSnapshot {
            vault,
            timestamp,
            content_hash: content_hash.clone(),
        };

        env.storage()
            .persistent()
            .set(&DataKey::VaultSnapshot(vault_id, timestamp), &snapshot);

        // Update the list of snapshot timestamps for this vault
        let mut timestamps: Vec<u64> = env
            .storage()
            .persistent()
            .get(&DataKey::VaultSnapshotTimestamps(vault_id))
            .unwrap_or_else(|| Vec::new(&env));

        timestamps.push_back(timestamp);

        // Limit snapshots to the last 1000 per vault
        if timestamps.len() > 1000 {
            if let Some(oldest_timestamp) = timestamps.first() {
                env.storage()
                    .persistent()
                    .remove(&DataKey::VaultSnapshot(vault_id, oldest_timestamp));
                timestamps.remove(0);
            }
        }

        env.storage()
            .persistent()
            .set(&DataKey::VaultSnapshotTimestamps(vault_id), &timestamps);

        content_hash.into()
    }

    /// Retrieves a captured vault state at a specific timestamp.
    pub fn get_vault_snapshot(env: Env, vault_id: u64, timestamp: u64) -> Vault {
        let snapshot: VaultSnapshot = env
            .storage()
            .persistent()
            .get(&DataKey::VaultSnapshot(vault_id, timestamp))
            .unwrap_or_else(|| panic_with_error!(&env, ContractError::SnapshotNotFound));
        snapshot.vault
    }

    /// Returns the last check-in timestamp for a vault.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    ///
    /// # Returns
    /// The Unix timestamp of the last check-in
    pub fn get_vault_last_check_in(env: Env, vault_id: u64) -> u64 {
        Self::load_vault(&env, vault_id).last_check_in
    }

    /// Returns the balance of a vault.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    ///
    /// # Returns
    /// The vault balance in stroops
    pub fn get_vault_balance(env: Env, vault_id: u64) -> i128 {
        Self::load_vault(&env, vault_id).balance
    }

    /// Returns the owner address of a vault.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    ///
    /// # Returns
    /// The owner `Address`
    pub fn get_vault_owner(env: Env, vault_id: u64) -> Address {
        Self::load_vault(&env, vault_id).owner
    }

    /// Returns the creation timestamp of a vault.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    ///
    /// # Returns
    /// The Unix timestamp when the vault was created
    pub fn get_vault_created_at(env: Env, vault_id: u64) -> u64 {
        Self::load_vault(&env, vault_id).created_at
    }

    /// Returns the unconditional release time for a vault.
    ///
    /// This is the earliest time the vault will automatically release funds
    /// if no further check-ins occur. Computed as last_check_in + check_in_interval.
    /// Returns 0 if vault is already expired or released.
    pub fn get_unconditional_release_time(env: Env, vault_id: u64) -> u64 {
        let vault = Self::load_vault(&env, vault_id);
        if vault.status != ReleaseStatus::Locked {
            return 0;
        }
        vault.last_check_in + vault.check_in_interval
    }

    /// Sets a spending limit on a vault, capping the amount released per `trigger_release` call.
    ///
    /// Owner-only. Pass `None` to remove the limit.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `limit` - `Some(amount)` to set a limit, `None` to remove it
    ///
    /// # Panics
    /// * Panics if the caller is not the vault owner
    /// * Panics if `limit` is `Some(0)` or negative
    pub fn set_spending_limit(env: Env, vault_id: u64, limit: Option<i128>) {
        let mut vault = Self::load_vault(&env, vault_id);
        vault.owner.require_auth();
        if let Some(l) = limit {
            if l <= 0 {
                panic_with_error!(&env, ContractError::InvalidAmount);
            }
        }
        vault.spending_limit = limit;
        Self::save_vault(&env, vault_id, &vault);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((SET_SPENDING_LIMIT_TOPIC, vault_id), limit);
    }

    /// Sets the burn percentage for a vault (0-10000 basis points).
    /// Owner-only. Pass 0 to disable burning.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `percentage` - Burn percentage in basis points (0-10_000)
    ///
    /// # Panics
    /// * Panics if the caller is not the vault owner
    /// * Panics if `percentage` > 10_000
    pub fn set_burn_percentage(env: Env, vault_id: u64, percentage: u32) {
        let mut vault = Self::load_vault(&env, vault_id);
        vault.owner.require_auth();
        if percentage > 10_000 {
            panic_with_error!(&env, ContractError::InvalidAmount);
        }
        vault.burn_percentage = percentage;
        Self::save_vault(&env, vault_id, &vault);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((SET_BURN_PERCENTAGE_TOPIC, vault_id), percentage);
    }

    /// Checks if a vault exists.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    ///
    /// # Returns
    /// `true` if the vault exists, `false` otherwise
    pub fn vault_exists(env: Env, vault_id: u64) -> bool {
        Self::try_load_vault(&env, vault_id).is_some()
    }

    /// Returns a paginated slice of vault IDs owned by a specific address.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `owner` - The owner address
    /// * `status_filter` - Optional status filter (None returns all vaults, Some(status) returns only vaults with that status)
    /// * `page` - Zero-based page index
    /// * `page_size` - Number of items per page
    ///
    /// # Returns
    /// A vector of vault IDs for the requested page
    pub fn get_vaults_by_owner(
        env: Env,
        owner: Address,
        status_filter: Option<ReleaseStatus>,
        page: u32,
        page_size: u32,
    ) -> Vec<u64> {
        let all = Self::load_owner_vault_ids(&env, &owner);
        let filtered = if let Some(status) = status_filter {
            let mut result = Vec::new(&env);
            for vault_id in all.iter() {
                if let Some(vault) = Self::try_load_vault(&env, vault_id) {
                    if vault.status == status {
                        result.push_back(vault_id);
                    }
                }
            }
            result
        } else {
            all
        };
        Self::paginate(&env, filtered, page, page_size)
    }

    /// Returns a paginated slice of vault IDs where a specific address is the beneficiary.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `beneficiary` - The beneficiary address
    /// * `status_filter` - Optional status filter (None returns all vaults, Some(status) returns only vaults with that status)
    /// * `page` - Zero-based page index
    /// * `page_size` - Number of items per page
    /// Returns all vault IDs associated with a beneficiary, including released and cancelled vaults.
    /// Use `get_active_vaults_by_beneficiary` to retrieve only locked (active) vaults.
    ///
    /// # Returns
    /// A vector of vault IDs for the requested page
    pub fn get_vaults_by_beneficiary(
        env: Env,
        beneficiary: Address,
        status_filter: Option<ReleaseStatus>,
        page: u32,
        page_size: u32,
    ) -> Vec<u64> {
        let all = Self::load_beneficiary_vault_ids(&env, &beneficiary);
        let filtered = if let Some(status) = status_filter {
            let mut result = Vec::new(&env);
            for vault_id in all.iter() {
                if let Some(vault) = Self::try_load_vault(&env, vault_id) {
                    if vault.status == status {
                        result.push_back(vault_id);
                    }
                }
            }
            result
        } else {
            all
        };
        Self::paginate(&env, filtered, page, page_size)
    }

    /// Returns only active (Locked) vault IDs for a beneficiary, excluding released and cancelled vaults.
    pub fn get_active_vaults_by_beneficiary(
        env: Env,
        beneficiary: Address,
        page: u32,
        page_size: u32,
    ) -> Vec<u64> {
        let all = Self::load_beneficiary_vault_ids(&env, &beneficiary);
        let mut active = Vec::new(&env);
        for id in all.iter() {
            if let Some(v) = Self::try_load_vault(&env, id) {
                if v.status == ReleaseStatus::Locked {
                    active.push_back(id);
                }
            }
        }
        Self::paginate(&env, active, page, page_size)
    }

    /// Returns the remaining time-to-live (TTL) for a vault in seconds.
    ///
    /// The TTL is calculated as the time remaining until the vault expires
    /// based on the last check-in time and the check-in interval.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    ///
    /// # Returns
    /// `Some(seconds)` with the remaining time in seconds if the vault exists and has not expired,
    /// `None` if the vault does not exist or the TTL has already lapsed.
    pub fn get_ttl_remaining(env: Env, vault_id: u64) -> Option<u64> {
        let vault = Self::try_load_vault(&env, vault_id)?;
        let deadline = vault.last_check_in + vault.check_in_interval;
        let now = env.ledger().timestamp();
        if now >= deadline {
            None
        } else {
            Some(deadline - now)
        }
    }

    /// Returns the current release status of a vault.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    ///
    /// # Returns
    /// The `ReleaseStatus` enum value (Locked, Released, or Cancelled)
    ///
    /// # Panics
    /// Panics if the vault does not exist
    pub fn get_release_status(env: Env, vault_id: u64) -> ReleaseStatus {
        Self::load_vault(&env, vault_id).status
    }

    /// Returns the total number of vaults created.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    ///
    /// # Returns
    /// The total vault count
    pub fn vault_count(env: Env) -> u64 {
        env.storage()
            .persistent()
            .get(&DataKey::VaultCount)
            .unwrap_or(0u64)
    }

    /// Returns the address of the XLM token used by this contract.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    ///
    /// # Returns
    /// The token contract address
    pub fn get_contract_token(env: Env) -> Address {
        Self::load_token(&env)
    }

    /// Returns all vault IDs where the given address is a beneficiary - Issue #398
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `beneficiary` - The beneficiary address
    ///
    /// # Returns
    /// A vector of vault IDs where the address is a beneficiary
    pub fn get_vaults_as_beneficiary(env: Env, beneficiary: Address) -> Vec<u64> {
        Self::load_beneficiary_vault_ids(&env, &beneficiary)
    }

    /// Returns passkey usage history for a vault - Issue #395
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault ID
    ///
    /// # Returns
    /// A vector of PasskeyUsageEntry records
    pub fn get_passkey_usage(env: Env, vault_id: u64) -> Vec<PasskeyUsageEntry> {
        env.storage()
            .persistent()
            .get(&DataKey::PasskeyUsage(vault_id))
            .unwrap_or(Vec::new(&env))
    }

    /// Accepts the beneficiary role for a vault - Issue #397
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault ID
    /// * `caller` - The beneficiary address (must authorize)
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    pub fn veto_release_condition(
        env: Env,
        vault_id: u64,
        caller: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        // Only primary beneficiary (vault.beneficiary)
        if caller != vault.beneficiary {
            return Err(ContractError::NotBeneficiary);
        }
        // Only before expiry
        if Self::is_expired(env.clone(), vault_id) {
            return Err(ContractError::NotExpired);
        }
        env.storage()
            .persistent()
            .set(&DataKey::BeneficiaryReleaseConditionVeto(vault_id), &true);
        // keep storage alive at least as long as the vault
        env.storage().persistent().extend_ttl(
            &DataKey::BeneficiaryReleaseConditionVeto(vault_id),
            VAULT_TTL_THRESHOLD,
            VAULT_TTL_LEDGERS,
        );
        Ok(())
    }

    pub fn revoke_veto_release_condition(
        env: Env,
        vault_id: u64,
        caller: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.beneficiary {
            return Err(ContractError::NotBeneficiary);
        }
        env.storage()
            .persistent()
            .remove(&DataKey::BeneficiaryReleaseConditionVeto(vault_id));
        Ok(())
    }

    pub fn get_veto_release_condition(env: Env, vault_id: u64) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::BeneficiaryReleaseConditionVeto(vault_id))
            .unwrap_or(false)
    }

    pub fn accept_beneficiary_role(
        env: Env,
        vault_id: u64,
        caller: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.beneficiary {
            return Err(ContractError::NotOwner);
        }
        env.storage().persistent().set(
            &DataKey::BeneficiaryStatus(vault_id),
            &BeneficiaryStatus::Accepted,
        );
        env.storage().persistent().extend_ttl(
            &DataKey::BeneficiaryStatus(vault_id),
            VAULT_TTL_THRESHOLD,
            VAULT_TTL_LEDGERS,
        );
        env.events()
            .publish((BENEFICIARY_ACCEPTED_TOPIC, vault_id), caller);
        Ok(())
    }

    /// Declines the beneficiary role for a vault - Issue #397
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault ID
    /// * `caller` - The beneficiary address (must authorize)
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    pub fn decline_beneficiary_role(
        env: Env,
        vault_id: u64,
        caller: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        let is_primary = caller == vault.beneficiary;
        let is_listed = vault.beneficiaries.iter().any(|e| e.address == caller);
        if !is_primary && !is_listed {
            return Err(ContractError::NotBeneficiary);
        }
        // Per-beneficiary status for waterfall/rebalancing logic
        let ben_key = DataKey::BeneficiaryStatusEntry(vault_id, caller.clone());
        env.storage()
            .persistent()
            .set(&ben_key, &BeneficiaryStatus::Declined);
        env.storage()
            .persistent()
            .extend_ttl(&ben_key, VAULT_TTL_THRESHOLD, VAULT_TTL_LEDGERS);
        // Vault-level status for single-beneficiary vaults
        if is_primary && vault.beneficiaries.is_empty() {
            env.storage().persistent().set(
                &DataKey::BeneficiaryStatus(vault_id),
                &BeneficiaryStatus::Declined,
            );
            env.storage().persistent().extend_ttl(
                &DataKey::BeneficiaryStatus(vault_id),
                VAULT_TTL_THRESHOLD,
                VAULT_TTL_LEDGERS,
            );
        }
        // Issue #542: handle vesting forfeiture if configured
        if let Some(mut forfeit_cfg) = env
            .storage()
            .persistent()
            .get::<DataKey, VestingForfeitureConfig>(&DataKey::VestingForfeiture(vault_id))
        {
            if !forfeit_cfg.forfeited {
                if let Some(schedule) = env
                    .storage()
                    .persistent()
                    .get::<DataKey, VestingSchedule>(&DataKey::VestingSchedule(vault_id, 0))
                {
                    let total_vested = schedule.total_amount;
                    let per_inst = total_vested / schedule.num_installments as i128;
                    let claimed_amount = per_inst * schedule.claimed_installments as i128;
                    let unvested = total_vested - claimed_amount;

                    if unvested > 0 {
                        let token_client = token::Client::new(&env, &vault.token_address);
                        crate::aml::require_compliant(&env, &forfeit_cfg.forfeiture_recipient);
                        token_client.transfer(
                            &env.current_contract_address(),
                            &forfeit_cfg.forfeiture_recipient,
                            &unvested,
                        );
                        let mut mut_vault = vault.clone();
                        mut_vault.balance -= unvested;
                        forfeit_cfg.forfeited = true;
                        env.storage()
                            .persistent()
                            .set(&DataKey::VestingForfeiture(vault_id), &forfeit_cfg);
                        Self::save_vault(&env, vault_id, &mut_vault);
                        env.events()
                            .publish((VESTING_FORFEITURE_TOPIC, vault_id), unvested);
                    }
                }
            }
        }

        env.events()
            .publish((BENEFICIARY_DECLINED_TOPIC, vault_id), caller);
        Ok(())
    }

    /// Gets the beneficiary status for a vault - Issue #397
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault ID
    ///
    /// # Returns
    /// The BeneficiaryStatus (defaults to Pending if not set)
    pub fn get_beneficiary_status(env: Env, vault_id: u64) -> BeneficiaryStatus {
        env.storage()
            .persistent()
            .get(&DataKey::BeneficiaryStatus(vault_id))
            .unwrap_or(BeneficiaryStatus::Pending)
    }

    pub fn set_beneficiary_release_triggers(
        env: Env,
        vault_id: u64,
        caller: Address,
        beneficiary: Address,
        triggers: Vec<ReleaseTrigger>,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        if !vault
            .beneficiaries
            .iter()
            .any(|entry| entry.address == beneficiary)
        {
            return Err(ContractError::InvalidBeneficiary);
        }
        if triggers.is_empty() {
            return Err(ContractError::InvalidAmount);
        }

        let mappings: Vec<(Address, Vec<ReleaseTrigger>)> = env
            .storage()
            .persistent()
            .get(&DataKey::BeneficiaryReleaseTriggers(vault_id))
            .unwrap_or(Vec::new(&env));
        let mut updated: Vec<(Address, Vec<ReleaseTrigger>)> = Vec::new(&env);
        for mapping in mappings.iter() {
            if mapping.0 != beneficiary {
                updated.push_back(mapping.clone());
            }
        }
        updated.push_back((beneficiary.clone(), triggers.clone()));
        env.storage()
            .persistent()
            .set(&DataKey::BeneficiaryReleaseTriggers(vault_id), &updated);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (BENEFICIARY_TRIGGER_SET_TOPIC, vault_id),
            BeneficiaryTriggerSetEvent {
                vault_id,
                beneficiary,
                triggers,
            },
        );
        Ok(())
    }

    pub fn get_beneficiary_release_triggers(
        env: Env,
        vault_id: u64,
        beneficiary: Address,
    ) -> Vec<ReleaseTrigger> {
        let mappings: Vec<(Address, Vec<ReleaseTrigger>)> = env
            .storage()
            .persistent()
            .get(&DataKey::BeneficiaryReleaseTriggers(vault_id))
            .unwrap_or(Vec::new(&env));
        for mapping in mappings.iter() {
            if mapping.0 == beneficiary {
                return mapping.1.clone();
            }
        }
        let mut default_triggers = Vec::new(&env);
        default_triggers.push_back(ReleaseTrigger::Expiry);
        default_triggers.push_back(ReleaseTrigger::Manual);
        default_triggers
    }

    pub fn set_beneficiary_tier_threshold(
        env: Env,
        vault_id: u64,
        caller: Address,
        beneficiary: Address,
        tier_threshold: i128,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        if !vault
            .beneficiaries
            .iter()
            .any(|entry| entry.address == beneficiary)
        {
            return Err(ContractError::InvalidBeneficiary);
        }
        if tier_threshold < 0 {
            return Err(ContractError::InvalidAmount);
        }

        env.storage().persistent().set(
            &DataKey::BeneficiaryTierThreshold(vault_id, beneficiary.clone()),
            &tier_threshold,
        );
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (BENEFICIARY_TIER_SET_TOPIC, vault_id),
            BeneficiaryTierSetEvent {
                vault_id,
                beneficiary,
                tier_threshold,
            },
        );
        Ok(())
    }

    pub fn get_beneficiary_tier_threshold(env: Env, vault_id: u64, beneficiary: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::BeneficiaryTierThreshold(vault_id, beneficiary))
            .unwrap_or(0)
    }

    /// Set a beneficiary-specific vesting schedule - Issue #525
    /// Different beneficiaries can have different vesting timelines.
    pub fn set_beneficiary_vesting(
        env: Env,
        vault_id: u64,
        caller: Address,
        beneficiary: Address,
        start_time: u64,
        interval: u64,
        num_installments: u32,
        cliff_period: u64,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if interval == 0 || num_installments == 0 {
            return Err(ContractError::InvalidAmount);
        }
        let schedule = BeneficiaryVestingSchedule {
            beneficiary: beneficiary.clone(),
            vault_id,
            start_time,
            interval,
            num_installments,
            claimed_installments: 0,
            total_amount: vault.balance,
            cliff_period,
        };
        env.storage().persistent().set(
            &DataKey::BeneficiaryVestingSchedule(vault_id, beneficiary.clone()),
            &schedule,
        );
        env.storage().persistent().extend_ttl(
            &DataKey::BeneficiaryVestingSchedule(vault_id, beneficiary.clone()),
            VAULT_TTL_THRESHOLD,
            VAULT_TTL_LEDGERS,
        );
        env.events().publish(
            (SET_BENEFICIARY_VESTING_TOPIC, vault_id),
            (beneficiary, start_time, interval, num_installments),
        );
        Ok(())
    }

    /// Get beneficiary-specific vesting schedule - Issue #525
    pub fn get_beneficiary_vesting(
        env: Env,
        vault_id: u64,
        beneficiary: Address,
    ) -> Option<BeneficiaryVestingSchedule> {
        env.storage()
            .persistent()
            .get(&DataKey::BeneficiaryVestingSchedule(vault_id, beneficiary))
    }

    /// Claim available vesting installments for a specific beneficiary - Issue #525
    pub fn claim_beneficiary_vesting(
        env: Env,
        vault_id: u64,
        beneficiary: Address,
    ) -> Result<i128, ContractError> {
        let mut vault = Self::load_vault(&env, vault_id);
        if vault.status != ReleaseStatus::Released {
            return Err(ContractError::AlreadyReleased);
        }
        let mut schedule = env
            .storage()
            .persistent()
            .get::<_, BeneficiaryVestingSchedule>(&DataKey::BeneficiaryVestingSchedule(
                vault_id,
                beneficiary.clone(),
            ))
            .ok_or(ContractError::VestingNotFound)?;

        let now = env.ledger().timestamp() as u64;
        if now < schedule.start_time + schedule.cliff_period {
            return Err(ContractError::CliffNotReached);
        }

        let elapsed = now.saturating_sub(schedule.start_time);
        let available_installments =
            ((elapsed / schedule.interval) + 1).min(schedule.num_installments as u64) as u32;

        if available_installments <= schedule.claimed_installments {
            return Err(ContractError::NothingToClaimYet);
        }

        let per_installment = schedule.total_amount / (schedule.num_installments as i128);
        let new_claims = available_installments - schedule.claimed_installments;
        let mut amount = per_installment * (new_claims as i128);

        if available_installments == schedule.num_installments {
            amount = vault.balance;
        }

        if amount <= 0 || amount > vault.balance {
            return Err(ContractError::InvalidAmount);
        }

        vault.balance -= amount;
        schedule.claimed_installments = available_installments;

        // Checks-Effects-Interactions: mutate state before the external token transfer.
        Self::save_vault(&env, vault_id, &vault);
        env.storage().persistent().set(
            &DataKey::BeneficiaryVestingSchedule(vault_id, beneficiary.clone()),
            &schedule,
        );
        env.storage().persistent().extend_ttl(
            &DataKey::BeneficiaryVestingSchedule(vault_id, beneficiary.clone()),
            VAULT_TTL_THRESHOLD,
            VAULT_TTL_LEDGERS,
        );

        let token_client = token::Client::new(&env, &vault.token_address);
        crate::aml::require_compliant(&env, &beneficiary);
        token_client.transfer(&env.current_contract_address(), &beneficiary, &amount);

        env.events().publish(
            (CLAIM_BENEFICIARY_VESTING_TOPIC, vault_id),
            (beneficiary, amount),
        );
        Ok(amount)
    }

    /// Create a beneficiary auction - Issue #527
    /// Allow beneficiaries to bid for larger allocations.
    pub fn create_beneficiary_auction(
        env: Env,
        vault_id: u64,
        caller: Address,
        start_time: u64,
        end_time: u64,
        total_allocation_bps: u32,
        minimum_bid: i128,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if env
            .storage()
            .persistent()
            .has(&DataKey::BeneficiaryAuction(vault_id))
        {
            return Err(ContractError::AuctionAlreadyExists);
        }

        let auction_id = env
            .storage()
            .persistent()
            .get::<_, u64>(&DataKey::BeneficiaryAuctionCount)
            .unwrap_or(0)
            + 1;

        let auction = BeneficiaryAuction {
            auction_id,
            vault_id,
            start_time,
            end_time,
            total_allocation_bps,
            minimum_bid,
            bids: vec![&env],
            finalized: false,
            winner: None,
        };

        env.storage()
            .persistent()
            .set(&DataKey::BeneficiaryAuction(vault_id), &auction);
        env.storage()
            .persistent()
            .set(&DataKey::BeneficiaryAuctionCount, &auction_id);
        env.storage().persistent().extend_ttl(
            &DataKey::BeneficiaryAuction(vault_id),
            VAULT_TTL_THRESHOLD,
            VAULT_TTL_LEDGERS,
        );

        env.events().publish(
            (AUCTION_CREATED_TOPIC, vault_id),
            (auction_id, start_time, end_time),
        );
        Ok(())
    }

    /// Place a bid in a beneficiary auction - Issue #527
    pub fn place_auction_bid(
        env: Env,
        vault_id: u64,
        bidder: Address,
        bid_amount: i128,
        desired_allocation_bps: u32,
    ) -> Result<(), ContractError> {
        bidder.require_auth();

        let mut auction = env
            .storage()
            .persistent()
            .get::<_, BeneficiaryAuction>(&DataKey::BeneficiaryAuction(vault_id))
            .ok_or(ContractError::AuctionNotFound)?;

        let now = env.ledger().timestamp() as u64;
        if now < auction.start_time || now >= auction.end_time {
            return Err(ContractError::AuctionEnded);
        }

        if bid_amount < auction.minimum_bid {
            return Err(ContractError::InvalidAmount);
        }

        let bid = BeneficiaryAuctionBid {
            auction_id: auction.auction_id,
            bidder: bidder.clone(),
            bid_amount,
            desired_allocation_bps,
            bid_timestamp: now,
            accepted: false,
        };

        auction.bids.push_back(bid);
        env.storage()
            .persistent()
            .set(&DataKey::BeneficiaryAuction(vault_id), &auction);
        env.storage().persistent().extend_ttl(
            &DataKey::BeneficiaryAuction(vault_id),
            VAULT_TTL_THRESHOLD,
            VAULT_TTL_LEDGERS,
        );

        env.events()
            .publish((AUCTION_BID_TOPIC, vault_id), (bidder, bid_amount));
        Ok(())
    }

    /// Get beneficiary auction - Issue #527
    pub fn get_beneficiary_auction(env: Env, vault_id: u64) -> Option<BeneficiaryAuction> {
        env.storage()
            .persistent()
            .get(&DataKey::BeneficiaryAuction(vault_id))
    }

    /// Get auction bids - Issue #527
    pub fn get_beneficiary_auction_bids(env: Env, vault_id: u64) -> Vec<BeneficiaryAuctionBid> {
        env.storage()
            .persistent()
            .get::<_, BeneficiaryAuction>(&DataKey::BeneficiaryAuction(vault_id))
            .map_or_else(|| vec![&env], |a| a.bids)
    }

    /// Finalize a beneficiary auction - Issue #527
    pub fn finalize_beneficiary_auction(env: Env, vault_id: u64) -> Result<(), ContractError> {
        let mut auction = env
            .storage()
            .persistent()
            .get::<_, BeneficiaryAuction>(&DataKey::BeneficiaryAuction(vault_id))
            .ok_or(ContractError::AuctionNotFound)?;

        let now = env.ledger().timestamp() as u64;
        if now < auction.end_time {
            return Err(ContractError::AuctionNotEnded);
        }

        if auction.bids.is_empty() {
            auction.finalized = true;
            env.storage()
                .persistent()
                .set(&DataKey::BeneficiaryAuction(vault_id), &auction);
            return Ok(());
        }

        // Find highest bidder
        let winner_idx = auction
            .bids
            .iter()
            .enumerate()
            .max_by_key(|(_, bid)| bid.bid_amount)
            .map(|(idx, _)| idx)
            .ok_or(ContractError::AuctionNotFound)?;

        let winner = auction
            .bids
            .get(winner_idx.try_into().unwrap())
            .unwrap()
            .bidder
            .clone();
        auction.winner = Some(winner.clone());
        auction.finalized = true;

        env.storage()
            .persistent()
            .set(&DataKey::BeneficiaryAuction(vault_id), &auction);
        env.storage().persistent().extend_ttl(
            &DataKey::BeneficiaryAuction(vault_id),
            VAULT_TTL_THRESHOLD,
            VAULT_TTL_LEDGERS,
        );

        env.events()
            .publish((AUCTION_FINALIZED_TOPIC, vault_id), winner);
        Ok(())
    }

    /// Extends passkey expiry - Issue #396
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault ID
    /// * `caller` - The vault owner (must authorize)
    /// * `passkey_hash` - The passkey hash to extend
    /// * `new_expiry` - New expiry timestamp
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    pub fn extend_passkey_expiry(
        env: Env,
        vault_id: u64,
        caller: Address,
        passkey_hash: BytesN<32>,
        new_expiry: u64,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        env.storage().persistent().set(
            &DataKey::PasskeyExpiry(vault_id, passkey_hash.clone()),
            &new_expiry,
        );
        env.storage().persistent().extend_ttl(
            &DataKey::PasskeyExpiry(vault_id, passkey_hash.clone()),
            VAULT_TTL_THRESHOLD,
            VAULT_TTL_LEDGERS,
        );
        env.events().publish(
            (PASSKEY_EXPIRY_EXTENDED_TOPIC, vault_id),
            (passkey_hash, new_expiry),
        );
        Ok(())
    }

    /// Gets passkey expiry timestamp - Issue #396
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault ID
    /// * `passkey_hash` - The passkey hash
    ///
    /// # Returns
    /// The expiry timestamp, or None if not set
    pub fn get_passkey_expiry(env: Env, vault_id: u64, passkey_hash: BytesN<32>) -> Option<u64> {
        env.storage()
            .persistent()
            .get(&DataKey::PasskeyExpiry(vault_id, passkey_hash))
    }

    /// Updates the primary beneficiary of a vault.
    ///
    /// This function allows the vault owner to change the beneficiary who will
    /// receive the funds when the vault expires. The vault must still be in
    /// Locked status (not yet released).
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `caller` - The address of the caller (must be the vault owner)
    /// * `new_beneficiary` - The new beneficiary address
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    ///
    /// Initiates a beneficiary update. Requires a timelock before completion.
    /// Only the vault owner can call this.
    pub fn update_beneficiary(
        env: Env,
        vault_id: u64,
        caller: Address,
        new_beneficiary: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status == ReleaseStatus::EmergencyFrozen {
            return Err(ContractError::VaultFrozen);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }

        if vault.owner == new_beneficiary {
            return Err(ContractError::InvalidBeneficiary);
        }
        Self::assert_not_zero_address(&env, &new_beneficiary);
        if !aml::check_compliance(&env, &new_beneficiary) {
            return Err(ContractError::AmlFlaggedAddress);
        }

        let now = env.ledger().timestamp();
        // Timelock: 24 hours
        let timelock: u64 = 86_400;

        let pending = PendingBeneficiaryUpdate {
            new_beneficiary: new_beneficiary.clone(),
            initiated_at: now,
            unlocks_at: now + timelock,
        };

        env.storage()
            .persistent()
            .set(&DataKey::PendingBeneficiaryUpdate(vault_id), &pending);

        env.storage().persistent().extend_ttl(
            &DataKey::PendingBeneficiaryUpdate(vault_id),
            VAULT_TTL_THRESHOLD,
            vault_ttl_ledgers(vault.check_in_interval),
        );

        env.events().publish(
            (symbol_short!("ben_init"), vault_id),
            (new_beneficiary, now + timelock),
        );

        Ok(())
    }

    /// Completes a previously initiated beneficiary update after the timelock.
    /// Only the vault owner can call this.
    pub fn apply_beneficiary_update(
        env: Env,
        vault_id: u64,
        caller: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let mut vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }

        let pending = env
            .storage()
            .persistent()
            .get::<DataKey, PendingBeneficiaryUpdate>(&DataKey::PendingBeneficiaryUpdate(vault_id))
            .ok_or(ContractError::NoPendingAdmin)?; // Or a more appropriate error

        let now = env.ledger().timestamp();
        if now < pending.unlocks_at {
            return Err(ContractError::OwnershipTransferTimeLocked);
        }

        let old_beneficiary = vault.beneficiary.clone();
        let new_beneficiary = pending.new_beneficiary.clone();

        // Enforce beneficiary capacity only when the beneficiary actually changes
        if old_beneficiary != new_beneficiary {
            Self::assert_beneficiary_capacity(&env, &new_beneficiary);
        }

        vault.beneficiary = new_beneficiary.clone();
        Self::save_vault(&env, vault_id, &vault);

        if old_beneficiary != new_beneficiary {
            Self::remove_beneficiary_vault_id(
                &env,
                &old_beneficiary,
                vault_id,
                vault.check_in_interval,
            );
            Self::add_beneficiary_vault_id(
                &env,
                &new_beneficiary,
                vault_id,
                vault.check_in_interval,
            );
        }

        env.storage()
            .persistent()
            .remove(&DataKey::PendingBeneficiaryUpdate(vault_id));

        Self::append_activity_log(&env, vault_id, "apply_beneficiary_update", &caller, "");
        env.events().publish(
            (BENEFICIARY_UPDATED_TOPIC, vault_id),
            (old_beneficiary, new_beneficiary),
        );
        Ok(())
    }

    /// Updates the check-in interval for a vault.
    ///
    /// The new interval must be within the configured min/max bounds.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `new_interval` - New interval in seconds (must be > 0)
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    ///
    /// # Errors
    /// * `ContractError::Paused` - If the contract is paused
    /// * `ContractError::InvalidInterval` - If new_interval is 0
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    /// * `ContractError::IntervalTooLow` - If new_interval is below minimum
    /// * `ContractError::IntervalTooHigh` - If new_interval exceeds maximum
    pub fn update_check_in_interval(
        env: Env,
        vault_id: u64,
        new_interval: u64,
    ) -> Result<(), ContractError> {
        if Self::load_paused(&env) {
            return Err(ContractError::Paused);
        }
        if new_interval == 0 {
            return Err(ContractError::InvalidInterval);
        }
        Self::assert_interval_in_bounds(&env, new_interval);
        let mut vault = Self::load_vault(&env, vault_id);
        vault.owner.require_auth();
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        let old_interval = vault.check_in_interval;
        vault.check_in_interval = new_interval;
        vault.last_check_in = env.ledger().timestamp();
        Self::save_vault(&env, vault_id, &vault);
        // Explicitly re-extend the vault's persistent TTL using the new (potentially
        // longer) interval so storage outlives the updated check-in deadline.
        let new_ttl = vault_ttl_ledgers(new_interval);
        env.storage().persistent().extend_ttl(
            &DataKey::Vault(vault_id),
            VAULT_TTL_THRESHOLD,
            new_ttl,
        );
        Self::log_audit_entry(&env, vault_id, "update_check_in_interval", &vault.owner, "");
        Self::append_activity_log(&env, vault_id, "update_check_in_interval", &vault.owner, "");
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (UPDATE_INTERVAL_TOPIC, vault_id),
            (old_interval, new_interval),
        );
        Ok(())
    }

    /// Cancels a vault and refunds the balance to the owner.
    ///
    /// This permanently marks the vault as Cancelled and transfers any
    /// remaining balance back to the owner.
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
    pub fn cancel_vault(env: Env, vault_id: u64, caller: Address) -> Result<(), ContractError> {
        if Self::load_paused(&env) {
            return Err(ContractError::Paused);
        }
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status == ReleaseStatus::EmergencyFrozen {
            return Err(ContractError::VaultFrozen);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        // If multi-sig is configured, require proposal instead
        if env
            .storage()
            .persistent()
            .has(&DataKey::MultiSigConfig(vault_id))
        {
            return Err(ContractError::MultiSigRequired);
        }
        let refund_amount = vault.balance;
        if refund_amount > 0 {
            let token_client = token::Client::new(&env, &vault.token_address);
            token_client.transfer(
                &env.current_contract_address(),
                &vault.owner,
                &refund_amount,
            );
        }
        let mut v = vault.clone();
        v.balance = 0;
        v.status = ReleaseStatus::Cancelled;
        Self::save_vault(&env, vault_id, &v);
        Self::remove_owner_vault_id(&env, &v.owner, vault_id, v.check_in_interval);
        Self::remove_beneficiary_vault_id(&env, &v.beneficiary, vault_id, v.check_in_interval);
        // Remove duplicate key so the same parameters can be reused
        let dup_key =
            DataKey::VaultDuplicate(v.owner.clone(), v.beneficiary.clone(), v.check_in_interval);
        env.storage().persistent().remove(&dup_key);
        Self::record_state_transition(
            &env,
            vault_id,
            ReleaseStatus::Locked,
            ReleaseStatus::Cancelled,
            &caller,
        );
        Self::log_audit_entry(&env, vault_id, "cancel_vault", &caller, "");
        Self::append_activity_log(&env, vault_id, "cancel_vault", &caller, "");
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((CANCEL_TOPIC, vault_id), (v.owner, refund_amount));
        Ok(())
    }

    /// Initiates a vault ownership transfer to a new address.
    ///
    /// This is step 1 of a 2-step ownership transfer with a 24-hour time-lock.
    /// The new owner must call `accept_ownership_transfer` after the time-lock
    /// expires to complete the transfer. The request expires after 7 days if
    /// not accepted.
    ///
    /// Only one pending transfer can exist per vault at a time. Calling this
    /// again replaces any existing pending request.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `caller` - The current owner (must authorize)
    /// * `new_owner` - The proposed new owner address
    ///
    /// # Returns
    /// `Ok(unlocks_at)` — the timestamp when the new owner may accept
    ///
    /// # Errors
    /// * `ContractError::Paused` - If the contract is paused
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    /// * `ContractError::InvalidBeneficiary` - If new_owner equals the vault beneficiary
    pub fn initiate_ownership_transfer(
        env: Env,
        vault_id: u64,
        caller: Address,
        new_owner: Address,
    ) -> Result<u64, ContractError> {
        if Self::load_paused(&env) {
            return Err(ContractError::Paused);
        }
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        if new_owner == vault.beneficiary {
            return Err(ContractError::InvalidBeneficiary);
        }
        if new_owner == vault.owner {
            return Err(ContractError::AlreadyOwner);
        }

        let now = env.ledger().timestamp();
        let unlocks_at = now + OWNERSHIP_TRANSFER_TIMELOCK;
        let expires_at = now + OWNERSHIP_TRANSFER_EXPIRY;

        let request = OwnershipTransferRequest {
            new_owner: new_owner.clone(),
            initiated_at: now,
            unlocks_at,
            expires_at,
        };
        let key = DataKey::PendingOwnership(vault_id);
        env.storage().persistent().set(&key, &request);
        env.storage().persistent().extend_ttl(
            &key,
            VAULT_TTL_THRESHOLD,
            vault_ttl_ledgers(vault.check_in_interval),
        );

        Self::log_audit_entry(&env, vault_id, "initiate_ownership_transfer", &caller, "");
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (OWNERSHIP_INITIATED_TOPIC, vault_id),
            (caller, new_owner, unlocks_at),
        );
        Ok(unlocks_at)
    }

    /// Accepts a pending ownership transfer (step 2).
    ///
    /// The new owner calls this after the 24-hour time-lock has passed.
    /// On success, vault ownership is transferred immediately.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `new_owner` - The new owner accepting the transfer (must authorize)
    ///
    /// # Returns
    /// `Ok(())` on success
    ///
    /// # Errors
    /// * `ContractError::Paused` - If the contract is paused
    /// * `ContractError::NoPendingOwnershipTransfer` - If no pending request exists
    /// * `ContractError::NotOwner` - If caller is not the designated new owner
    /// * `ContractError::OwnershipTransferTimeLocked` - If the time-lock has not yet elapsed
    /// * `ContractError::OwnershipTransferExpired` - If the request has expired
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    pub fn accept_ownership_transfer(
        env: Env,
        vault_id: u64,
        new_owner: Address,
    ) -> Result<(), ContractError> {
        if Self::load_paused(&env) {
            return Err(ContractError::Paused);
        }
        new_owner.require_auth();

        let key = DataKey::PendingOwnership(vault_id);
        let request = env
            .storage()
            .persistent()
            .get::<DataKey, OwnershipTransferRequest>(&key)
            .ok_or(ContractError::NoPendingOwnershipTransfer)?;

        if new_owner != request.new_owner {
            return Err(ContractError::NotOwner);
        }

        let now = env.ledger().timestamp();
        if now < request.unlocks_at {
            return Err(ContractError::OwnershipTransferTimeLocked);
        }
        if now > request.expires_at {
            return Err(ContractError::OwnershipTransferExpired);
        }

        let mut vault = Self::load_vault(&env, vault_id);
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }

        let old_owner = vault.owner.clone();
        if old_owner != new_owner {
            Self::remove_owner_vault_id(&env, &old_owner, vault_id, vault.check_in_interval);
            Self::add_owner_vault_id(&env, &new_owner, vault_id, vault.check_in_interval);
        }
        vault.owner = new_owner.clone();
        Self::save_vault(&env, vault_id, &vault);

        // Clear the pending request
        env.storage().persistent().remove(&key);

        Self::log_audit_entry(&env, vault_id, "accept_ownership_transfer", &new_owner, "");
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (OWNERSHIP_ACCEPTED_TOPIC, vault_id),
            (old_owner.clone(), new_owner.clone()),
        );
        // Backwards-compatible event for consumers watching OWNERSHIP_TOPIC
        env.events()
            .publish((OWNERSHIP_TOPIC, vault_id), (old_owner, new_owner));
        Ok(())
    }

    /// Cancels a pending ownership transfer.
    ///
    /// Only the current vault owner can cancel. This removes the pending request
    /// and the proposed new owner can no longer accept.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `caller` - The current owner (must authorize)
    ///
    /// # Returns
    /// `Ok(())` on success
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::NoPendingOwnershipTransfer` - If no pending request exists
    pub fn cancel_ownership_transfer(
        env: Env,
        vault_id: u64,
        caller: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);

        let key = DataKey::PendingOwnership(vault_id);
        if !env.storage().persistent().has(&key) {
            return Err(ContractError::NoPendingOwnershipTransfer);
        }
        let request = env
            .storage()
            .persistent()
            .get::<DataKey, OwnershipTransferRequest>(&key)
            .expect("just checked has(&key) above");

        let reason = if caller == vault.owner {
            String::from_str(&env, "cancelled")
        } else if caller == request.new_owner {
            String::from_str(&env, "declined")
        } else {
            return Err(ContractError::NotOwner);
        };

        let cancelled_new_owner = request.new_owner.clone();
        env.storage().persistent().remove(&key);

        Self::log_audit_entry(&env, vault_id, "cancel_ownership_transfer", &caller, "");
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (OWNERSHIP_CANCELLED_TOPIC, vault_id),
            (caller, cancelled_new_owner, reason),
        );
        Ok(())
    }

    /// Expires a stale ownership transfer request after the 7-day expiry window.
    ///
    /// Callable by anyone once the request has passed its `expires_at` timestamp.
    /// Emits `OWNERSHIP_TRANSFER_EXPIRED_TOPIC` and removes the pending request.
    pub fn expire_ownership_transfer(env: Env, vault_id: u64) -> Result<(), ContractError> {
        let key = DataKey::PendingOwnership(vault_id);
        let request = env
            .storage()
            .persistent()
            .get::<DataKey, OwnershipTransferRequest>(&key)
            .ok_or(ContractError::NoPendingOwnershipTransfer)?;

        if env.ledger().timestamp() <= request.expires_at {
            return Err(ContractError::NotExpired);
        }

        env.storage().persistent().remove(&key);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (OWNERSHIP_TRANSFER_EXPIRED_TOPIC, vault_id),
            (request.new_owner,),
        );
        Ok(())
    }

    /// Returns the pending ownership transfer request for a vault, if any.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    ///
    /// # Returns
    /// `Some(OwnershipTransferRequest)` if a pending transfer exists, `None` otherwise
    pub fn get_pending_ownership_transfer(
        env: Env,
        vault_id: u64,
    ) -> Option<OwnershipTransferRequest> {
        env.storage()
            .persistent()
            .get::<DataKey, OwnershipTransferRequest>(&DataKey::PendingOwnership(vault_id))
    }

    // --- Issue #378: Vault Metadata ---

    /// Sets custom metadata for a vault (max 2KB).
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `caller` - The address of the caller (must be the vault owner)
    /// * `metadata` - Custom metadata as bytes (max 2KB)
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    /// * `ContractError::InvalidAmount` - If metadata exceeds 2KB
    pub fn set_vault_metadata(
        env: Env,
        vault_id: u64,
        caller: Address,
        metadata: Bytes,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        if metadata.len() > MAX_CUSTOM_METADATA_LEN {
            return Err(ContractError::InvalidAmount);
        }
        Self::require_utf8_metadata_bytes(&env, &metadata)?;
        let mut vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        // Append to custom metadata history (limit to last 100 entries)
        let key = DataKey::CustomMetadataHistory(vault_id);
        let mut history: Vec<CustomMetadataEntry> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env));

        let now = env.ledger().timestamp();
        history.push_back(CustomMetadataEntry {
            metadata: metadata.clone(),
            timestamp: now,
        });

        // Trim to last 100 entries if necessary
        if history.len() > 100 {
            let total = history.len();
            let start = total - 100;
            let mut trimmed: Vec<CustomMetadataEntry> = Vec::new(&env);
            for i in start..total {
                trimmed.push_back(history.get(i).unwrap());
            }
            let ttl = vault_ttl_ledgers(vault.check_in_interval);
            env.storage().persistent().set(&key, &trimmed);
            env.storage()
                .persistent()
                .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        } else {
            let ttl = vault_ttl_ledgers(vault.check_in_interval);
            env.storage().persistent().set(&key, &history);
            env.storage()
                .persistent()
                .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        }

        vault.custom_metadata = metadata.clone();
        Self::save_vault(&env, vault_id, &vault);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((SET_METADATA_TOPIC, vault_id), metadata);
        Ok(())
    }

    /// Returns the custom metadata history (bytes + timestamp) for a vault.
    pub fn get_custom_metadata_history(env: Env, vault_id: u64) -> Vec<CustomMetadataEntry> {
        env.storage()
            .persistent()
            .get(&DataKey::CustomMetadataHistory(vault_id))
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Gets custom metadata for a vault.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    ///
    /// # Returns
    /// The custom metadata bytes
    pub fn get_vault_metadata(env: Env, vault_id: u64) -> Bytes {
        let vault = Self::load_vault(&env, vault_id);
        vault.custom_metadata
    }

    // --- Issue #380: Vault Pause/Freeze ---

    /// Pauses a vault, preventing all operations until resumed.
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
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    pub fn pause_vault(env: Env, vault_id: u64, caller: Address) -> Result<(), ContractError> {
        caller.require_auth();
        let mut vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        vault.is_paused = true;
        Self::save_vault(&env, vault_id, &vault);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish((PAUSE_VAULT_TOPIC, vault_id), true);
        Ok(())
    }

    /// Resumes a paused vault, allowing operations to resume.
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
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    pub fn resume_vault(env: Env, vault_id: u64, caller: Address) -> Result<(), ContractError> {
        caller.require_auth();
        let mut vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        vault.is_paused = false;
        Self::save_vault(&env, vault_id, &vault);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish((RESUME_VAULT_TOPIC, vault_id), false);
        Ok(())
    }

    /// Checks if a vault is paused.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    ///
    /// # Returns
    /// `true` if the vault is paused, `false` otherwise
    pub fn is_vault_paused(env: Env, vault_id: u64) -> bool {
        if let Some(vault) = Self::try_load_vault(&env, vault_id) {
            vault.is_paused
        } else {
            false
        }
    }

    // --- Issue #379: Conditional Release Logic ---

    /// Sets the release condition for a vault.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `caller` - The address of the caller (must be the vault owner)
    /// * `condition` - The release condition
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    pub fn set_release_condition(
        env: Env,
        vault_id: u64,
        caller: Address,
        condition: ReleaseCondition,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let mut vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        vault.release_condition = condition;
        Self::save_vault(&env, vault_id, &vault);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        Ok(())
    }

    /// Gets the release condition for a vault.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    ///
    /// # Returns
    /// The release condition
    pub fn get_release_condition(env: Env, vault_id: u64) -> ReleaseCondition {
        let vault = Self::load_vault(&env, vault_id);
        vault.release_condition
    }

    /// Sets multiple release conditions for a vault.
    /// Replaces any existing conditions.
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault identifier
    /// * `caller` - Must be the vault owner
    /// * `conditions` - Vector of `ReleaseCondition`s
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    pub fn set_release_conditions(
        env: Env,
        vault_id: u64,
        caller: Address,
        conditions: Vec<ReleaseCondition>,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        env.storage()
            .persistent()
            .set(&DataKey::ReleaseConditions(vault_id), &conditions);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        Ok(())
    }

    /// Helper to retrieve stored release conditions.
    ///
    /// Falls back to the vault's single `release_condition` field when no explicit
    /// multi-condition set has been configured via `set_release_conditions`, so vaults
    /// created before (or without ever using) the multi-condition feature keep releasing
    /// under their original single condition.
    fn get_release_conditions(env: &Env, vault_id: u64) -> Vec<ReleaseCondition> {
        env.storage()
            .persistent()
            .get(&DataKey::ReleaseConditions(vault_id))
            .unwrap_or_else(|| {
                let vault = Self::load_vault(env, vault_id);
                let mut conditions = Vec::new(env);
                conditions.push_back(vault.release_condition);
                conditions
            })
    }

    /// Owner‑initiated panic release.
    /// Allows the owner to manually release a vault regardless of other conditions.
    pub fn panic_release(env: Env, vault_id: u64, caller: Address) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        Self::trigger_release_internal(env, vault_id, ReleaseTrigger::Manual);
        Ok(())
    }

    // --- Issue #381: Vault Inheritance Chain ---

    /// Creates a new vault from inherited funds (beneficiary-only).
    ///
    /// The beneficiary of a released vault can create a new vault with the inherited funds,
    /// establishing an inheritance chain for multi-generational wealth transfer.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `parent_vault_id` - The ID of the parent vault (must be released)
    /// * `caller` - The address of the caller (must be the beneficiary of parent vault)
    /// * `new_beneficiary` - The beneficiary for the new vault
    /// * `check_in_interval` - Check-in interval for the new vault
    /// * `token_address` - Optional token address for the new vault
    ///
    /// # Returns
    /// The ID of the newly created vault
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the beneficiary of parent vault
    /// * `ContractError::NotExpired` - If parent vault is not released
    /// * `ContractError::InvalidInterval` - If check_in_interval is invalid
    pub fn create_vault_from_inheritance(
        env: Env,
        parent_vault_id: u64,
        caller: Address,
        new_beneficiary: Address,
        check_in_interval: u64,
        token_address: Option<Address>,
    ) -> u64 {
        caller.require_auth();
        Self::require_initialized(&env);

        let parent_vault = Self::load_vault(&env, parent_vault_id);
        if caller != parent_vault.beneficiary {
            panic_with_error!(&env, ContractError::NotOwner);
        }
        if parent_vault.status != ReleaseStatus::Released {
            panic_with_error!(&env, ContractError::NotExpired);
        }
        if check_in_interval == 0 {
            panic_with_error!(&env, ContractError::InvalidInterval);
        }
        Self::assert_interval_in_bounds(&env, check_in_interval);
        if caller == new_beneficiary {
            panic_with_error!(&env, ContractError::InvalidBeneficiary);
        }

        let vault_token = match token_address {
            Some(addr) => {
                Self::assert_token_whitelisted(&env, &addr);
                addr
            }
            None => Self::load_token(&env),
        };

        let vault_id = Self::vault_count(env.clone()) + 1;
        let timestamp = env.ledger().timestamp();
        let metadata = String::from_str(&env, "");
        let new_vault = Vault {
            owner: caller.clone(),
            beneficiary: new_beneficiary.clone(),
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
            parent_vault_id: Some(parent_vault_id),
            passkey_hash: None,
            max_deposit_amount: None,
            withdrawal_approval_threshold: None,
            spending_limit: None,
            burn_percentage: 0,
            inactivity_penalty_bps: None,
            penalty_recipient: None,
        };

        Self::save_vault(&env, vault_id, &new_vault);
        Self::add_owner_vault_id(&env, &caller, vault_id, check_in_interval);
        Self::add_beneficiary_vault_id(&env, &new_beneficiary, vault_id, check_in_interval);

        let key = DataKey::VaultCount;
        env.storage().persistent().set(&key, &vault_id);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, VAULT_TTL_LEDGERS);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        env.events().publish(
            (INHERITANCE_TOPIC,),
            (
                parent_vault_id,
                vault_id,
                caller,
                new_beneficiary,
                check_in_interval,
            ),
        );
        vault_id
    }

    /// Gets the parent vault ID for an inherited vault.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    ///
    /// # Returns
    /// `Some(parent_vault_id)` if this is an inherited vault, `None` otherwise
    pub fn get_parent_vault(env: Env, vault_id: u64) -> Option<u64> {
        if let Some(vault) = Self::try_load_vault(&env, vault_id) {
            vault.parent_vault_id
        } else {
            None
        }
    }

    // --- Issue #443: Vault Archival and Restoration API ---

    /// Restores an archived vault's persistent storage entry by re-extending its TTL.
    ///
    /// Soroban archives persistent entries when their TTL expires. This function
    /// restores the vault entry so it becomes accessible again. Anyone can call this
    /// to unblock a beneficiary from triggering release on an archived vault.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault to restore
    ///
    /// # Panics
    /// Panics if the vault does not exist (was never created or has been permanently deleted)
    pub fn restore_vault(env: Env, vault_id: u64) {
        // Extending TTL on an archived entry restores it. If the entry no longer
        // exists at all, load_vault will panic with VaultNotFound. Re-saving also
        // re-materializes a compressed vault (Issue #558) and extends its TTL.
        let vault = Self::load_vault(&env, vault_id);
        Self::save_vault(&env, vault_id, &vault);
        // Clear any stale archived-info snapshot now that the vault is live again.
        env.storage()
            .persistent()
            .remove(&DataKey::ArchivedVault(vault_id));
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((RESTORE_VAULT_TOPIC, vault_id), vault_id);
    }

    /// Returns archived vault metadata if a snapshot was saved before archival.
    ///
    /// When a vault's TTL is about to lapse, operators can snapshot its state via
    /// off-chain tooling. This function queries that snapshot. Returns `None` if no
    /// snapshot exists (vault is live or was never snapshotted).
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    ///
    /// # Returns
    /// `Some(ArchivedVaultInfo)` if a snapshot exists, `None` otherwise
    pub fn get_archived_vault_info(env: Env, vault_id: u64) -> Option<ArchivedVaultInfo> {
        env.storage()
            .persistent()
            .get(&DataKey::ArchivedVault(vault_id))
    }

    /// Internal helper: if an archived-info snapshot exists for the vault, restore
    /// the vault entry's TTL so `load_vault` can succeed in `trigger_release`.
    fn try_restore_archived_vault(env: &Env, vault_id: u64) {
        // Only attempt restoration if a snapshot is present (vault may be archived).
        if env
            .storage()
            .persistent()
            .has(&DataKey::ArchivedVault(vault_id))
        {
            let key = DataKey::Vault(vault_id);
            if let Some(vault) = env.storage().persistent().get::<DataKey, Vault>(&key) {
                let ttl = vault_ttl_ledgers(vault.check_in_interval);
                env.storage()
                    .persistent()
                    .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
                env.storage()
                    .persistent()
                    .remove(&DataKey::ArchivedVault(vault_id));
                env.events()
                    .publish((RESTORE_VAULT_TOPIC, vault_id), vault_id);
            }
        }
    }

    // ── Beneficiary Anonymity (ZK commitment) ────────────────────────────────

    /// Commits to an anonymous beneficiary using a hash commitment.
    ///
    /// The owner stores `sha256(beneficiary_address_bytes)` instead of the
    /// plaintext address. The actual beneficiary remains hidden until
    /// `reveal_beneficiary` is called at release time.
    ///
    /// A vault may have at most one active commitment. Call again to replace.
    ///
    /// # Arguments
    /// * `vault_id`   - The vault to attach the commitment to
    /// * `caller`     - Must be the vault owner (requires auth)
    /// * `commitment` - SHA-256 hash of the beneficiary address bytes
    ///
    /// # Errors
    /// * `NotOwner`              - caller is not the vault owner
    /// * `AlreadyReleased`       - vault is not Locked
    /// * `CommitmentAlreadySet`  - a commitment already exists (must reveal first)
    pub fn commit_beneficiary(
        env: Env,
        vault_id: u64,
        caller: Address,
        commitment: BytesN<32>,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        let key = DataKey::BeneficiaryCommitment(vault_id);
        if env.storage().persistent().has(&key) {
            return Err(ContractError::CommitmentAlreadySet);
        }
        let entry = BeneficiaryCommitment {
            commitment: commitment.clone(),
            committed_at: env.ledger().timestamp(),
        };
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, &entry);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((BEN_COMMITTED_TOPIC, vault_id), commitment);
        Ok(())
    }

    /// Reveals the anonymous beneficiary and releases vault funds.
    ///
    /// Anyone may call this once the vault has expired. The `proof` bytes must
    /// be the raw address bytes of the beneficiary such that
    /// `sha256(proof) == stored_commitment`. On success the revealed address
    /// receives the vault balance and the vault is marked Released.
    ///
    /// # Arguments
    /// * `vault_id` - The vault to release
    /// * `proof`    - Raw bytes whose SHA-256 equals the stored commitment
    /// * `claim`    - The `Address` being claimed (must match `sha256(proof)`)
    ///
    /// # Errors
    /// * `CommitmentNotFound`        - no commitment was stored for this vault
    /// * `BeneficiaryAlreadyRevealed`- funds already released via this path
    /// * `NotExpired`                - vault has not yet expired
    /// * `EmptyVault`                - vault balance is zero
    /// * `InvalidZkProof`            - sha256(proof) does not match commitment
    pub fn reveal_beneficiary(
        env: Env,
        vault_id: u64,
        proof: Bytes,
        claim: Address,
    ) -> Result<(), ContractError> {
        let com_key = DataKey::BeneficiaryCommitment(vault_id);
        let entry: BeneficiaryCommitment = env
            .storage()
            .persistent()
            .get(&com_key)
            .ok_or(ContractError::CommitmentNotFound)?;

        if env
            .storage()
            .persistent()
            .has(&DataKey::RevealedBeneficiary(vault_id))
        {
            return Err(ContractError::BeneficiaryAlreadyRevealed);
        }

        let mut vault = Self::load_vault(&env, vault_id);
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::BeneficiaryAlreadyRevealed);
        }
        if !Self::is_expired(env.clone(), vault_id) {
            return Err(ContractError::NotExpired);
        }
        if vault.balance == 0 {
            return Err(ContractError::EmptyVault);
        }

        // Verify: sha256(proof) must equal the stored commitment
        let proof_hash: BytesN<32> = env.crypto().sha256(&proof).into();
        if proof_hash != entry.commitment {
            return Err(ContractError::InvalidZkProof);
        }

        let amount = vault.balance;
        let token_client = token::Client::new(&env, &vault.token_address);
        crate::aml::require_compliant(&env, &claim);
        token_client.transfer(&env.current_contract_address(), &claim, &amount);

        vault.balance = 0;
        vault.status = ReleaseStatus::Released;
        Self::save_vault(&env, vault_id, &vault);

        // Mark as revealed so it cannot be called again
        let rev_key = DataKey::RevealedBeneficiary(vault_id);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&rev_key, &claim);
        env.storage()
            .persistent()
            .extend_ttl(&rev_key, VAULT_TTL_THRESHOLD, ttl);

        Self::record_state_transition(
            &env,
            vault_id,
            ReleaseStatus::Locked,
            ReleaseStatus::Released,
            &claim,
        );
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((BEN_REVEALED_TOPIC, vault_id), (claim, amount));
        Ok(())
    }

    /// Returns the beneficiary commitment for a vault, if one exists.
    pub fn get_beneficiary_commitment(env: Env, vault_id: u64) -> Option<BeneficiaryCommitment> {
        env.storage()
            .persistent()
            .get(&DataKey::BeneficiaryCommitment(vault_id))
    }

    /// Returns the revealed beneficiary address for a vault, if already revealed.
    pub fn get_revealed_beneficiary(env: Env, vault_id: u64) -> Option<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::RevealedBeneficiary(vault_id))
    }

    // --- helpers ---

    #[allow(dead_code)]
    fn extend_vault_ttl(env: &Env, vault_id: u64, check_in_interval: u64) {
        let key = DataKey::Vault(vault_id);
        let ttl = vault_ttl_ledgers(check_in_interval);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
    }

    /// Returns the vault activity log (alias for get_vault_audit_log).
    pub fn get_vault_activity_log(env: Env, vault_id: u64) -> Vec<AuditEntry> {
        Self::get_vault_audit_log(env, vault_id)
    }

    /// Transfers vault ownership immediately (single-step, backwards-compatible).
    pub fn transfer_ownership(
        env: Env,
        vault_id: u64,
        caller: Address,
        new_owner: Address,
    ) -> Result<(), ContractError> {
        if Self::load_paused(&env) {
            return Err(ContractError::Paused);
        }
        caller.require_auth();
        let mut vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        if new_owner == vault.beneficiary {
            return Err(ContractError::InvalidBeneficiary);
        }
        let old_owner = vault.owner.clone();
        if old_owner != new_owner {
            Self::remove_owner_vault_id(&env, &old_owner, vault_id, vault.check_in_interval);
            Self::add_owner_vault_id(&env, &new_owner, vault_id, vault.check_in_interval);
        }
        vault.owner = new_owner.clone();
        Self::save_vault(&env, vault_id, &vault);
        Self::log_audit_entry(&env, vault_id, "transfer_ownership", &caller, "");
        Self::append_activity_log(&env, vault_id, "transfer_ownership", &caller, "");
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((OWNERSHIP_TOPIC, vault_id), (old_owner, new_owner));
        Ok(())
    }

    // ── Issue: TTL Borrowing ──────────────────────────────────────────────────

    /// Temporarily borrows TTL from a lender vault to extend a borrower vault's expiry.
    ///
    /// The lender's remaining TTL is reduced by `borrow_seconds`; the borrower's
    /// `last_check_in` is extended by the same amount. Only the borrower vault owner
    /// may call this. A `TtlBorrowRecord` is stored for auditability and repayment.
    ///
    /// # Errors
    /// * `Paused`              - contract is paused
    /// * `NotOwner`            - caller is not borrower vault owner
    /// * `AlreadyReleased`     - either vault is not Locked
    /// * `InvalidAmount`       - borrow_seconds is 0 or vaults are the same
    /// * `InsufficientBalance` - lender does not have enough remaining TTL
    pub fn borrow_ttl(
        env: Env,
        borrower_vault_id: u64,
        lender_vault_id: u64,
        caller: Address,
        borrow_seconds: u64,
    ) -> Result<(), ContractError> {
        if Self::load_paused(&env) {
            return Err(ContractError::Paused);
        }
        caller.require_auth();
        if borrow_seconds == 0 || borrower_vault_id == lender_vault_id {
            return Err(ContractError::InvalidAmount);
        }
        let mut borrower = Self::load_vault(&env, borrower_vault_id);
        if caller != borrower.owner {
            return Err(ContractError::NotOwner);
        }
        if borrower.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        let mut lender = Self::load_vault(&env, lender_vault_id);
        if lender.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        let now = env.ledger().timestamp();
        let lender_deadline = lender.last_check_in + lender.check_in_interval;
        let lender_remaining = lender_deadline.saturating_sub(now);
        if lender_remaining <= borrow_seconds {
            return Err(ContractError::InsufficientBalance);
        }
        borrower.last_check_in = borrower.last_check_in.saturating_add(borrow_seconds);
        lender.last_check_in = lender.last_check_in.saturating_sub(borrow_seconds);
        let record = TtlBorrowRecord {
            lender_vault_id,
            borrower_vault_id,
            borrowed_seconds: borrow_seconds,
            borrowed_at: now,
            repaid: false,
        };
        let key = DataKey::TtlBorrow(borrower_vault_id);
        let ttl = vault_ttl_ledgers(borrower.check_in_interval);
        env.storage().persistent().set(&key, &record);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        Self::save_vault(&env, borrower_vault_id, &borrower);
        Self::save_vault(&env, lender_vault_id, &lender);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (TTL_BORROW_TOPIC, borrower_vault_id),
            (lender_vault_id, borrow_seconds, now),
        );
        Ok(())
    }

    /// Repays a TTL borrow, restoring the lender vault's TTL.
    ///
    /// # Errors
    /// * `NotOwner`               - caller is not borrower vault owner
    /// * `TtlBorrowNotFound`      - no borrow record exists
    /// * `TtlBorrowAlreadyRepaid` - borrow was already repaid
    pub fn repay_ttl_borrow(
        env: Env,
        borrower_vault_id: u64,
        caller: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let borrower = Self::load_vault(&env, borrower_vault_id);
        if caller != borrower.owner {
            return Err(ContractError::NotOwner);
        }
        let key = DataKey::TtlBorrow(borrower_vault_id);
        let mut record: TtlBorrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::TtlBorrowNotFound)?;
        if record.repaid {
            return Err(ContractError::TtlBorrowAlreadyRepaid);
        }
        let mut lender = Self::load_vault(&env, record.lender_vault_id);
        lender.last_check_in = lender.last_check_in.saturating_add(record.borrowed_seconds);
        Self::save_vault(&env, record.lender_vault_id, &lender);
        record.repaid = true;
        env.storage().persistent().set(&key, &record);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (TTL_REPAY_TOPIC, borrower_vault_id),
            (record.lender_vault_id, record.borrowed_seconds),
        );
        Ok(())
    }

    /// Returns the active TTL borrow record for a vault, if any.
    pub fn get_ttl_borrow(env: Env, borrower_vault_id: u64) -> Option<TtlBorrowRecord> {
        env.storage()
            .persistent()
            .get(&DataKey::TtlBorrow(borrower_vault_id))
    }

    // ── Issue: Check-in Rate Limiting ─────────────────────────────────────────

    /// Sets the minimum cooldown (seconds) between consecutive check-ins.
    ///
    /// Admin-only. Prevents owners from spamming check-ins to waste storage.
    /// Set to 0 to disable rate limiting entirely.
    pub fn set_min_checkin_cooldown(env: Env, cooldown_seconds: u64) {
        Self::require_admin(&env);
        env.storage()
            .instance()
            .set(&DataKey::MinCheckInCooldown, &cooldown_seconds);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((CHECKIN_RATE_LIMITED_TOPIC,), cooldown_seconds);
    }

    /// Returns the configured minimum check-in cooldown in seconds.
    /// Defaults to `DEFAULT_MIN_CHECKIN_COOLDOWN` (60s) if not set.
    pub fn get_min_checkin_cooldown(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::MinCheckInCooldown)
            .unwrap_or(DEFAULT_MIN_CHECKIN_COOLDOWN)
    }

    /// Returns the timestamp of the most recent check-in for a vault.
    /// Returns `None` if no check-in has been recorded yet.
    pub fn get_last_checkin_time(env: Env, vault_id: u64) -> Option<u64> {
        env.storage()
            .persistent()
            .get(&DataKey::LastCheckInTime(vault_id))
    }

    // ── Issue: Accelerated TTL Decay ──────────────────────────────────────────

    /// Allows the vault owner to accelerate TTL decay, making the vault expire sooner.
    ///
    /// Reduces `last_check_in` by `accelerate_by_seconds`, moving the expiry deadline
    /// forward. Capped at `MAX_ACCELERATE_SECONDS` (30 days) per call. Cannot push
    /// the deadline to the current time or past (must leave ≥ 1 second remaining).
    ///
    /// # Arguments
    /// * `vault_id`              - The vault to accelerate
    /// * `caller`                - Must be the vault owner
    /// * `accelerate_by_seconds` - Seconds to shorten the remaining TTL by
    ///
    /// # Errors
    /// * `Paused`                    - contract is paused
    /// * `NotOwner`                  - caller is not vault owner
    /// * `AlreadyReleased`           - vault is not Locked
    /// * `InvalidAmount`             - accelerate_by_seconds is 0 or exceeds cap
    /// * `InsufficientTtlToAccelerate` - would push expiry to now or past
    pub fn accelerate_ttl_decay(
        env: Env,
        vault_id: u64,
        caller: Address,
        accelerate_by_seconds: u64,
    ) -> Result<(), ContractError> {
        if Self::load_paused(&env) {
            return Err(ContractError::Paused);
        }
        caller.require_auth();
        if accelerate_by_seconds == 0 || accelerate_by_seconds > MAX_ACCELERATE_SECONDS {
            return Err(ContractError::InvalidAmount);
        }
        let mut vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        let now = env.ledger().timestamp();
        let current_deadline = vault.last_check_in + vault.check_in_interval;
        let remaining = current_deadline.saturating_sub(now);
        // Must leave at least 1 second of TTL remaining
        if remaining <= accelerate_by_seconds {
            return Err(ContractError::InsufficientTtlToAccelerate);
        }
        vault.last_check_in = vault.last_check_in.saturating_sub(accelerate_by_seconds);
        Self::save_vault(&env, vault_id, &vault);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (TTL_ACCELERATE_TOPIC, vault_id),
            (accelerate_by_seconds, remaining - accelerate_by_seconds),
        );
        Ok(())
    }

    // ── Issue: Geographic Check-in Tracking ───────────────────────────────────

    /// Records a check-in with geographic location metadata for security and anomaly detection.
    ///
    /// Delegates to the standard `check_in` for all vault validations (owner auth,
    /// rate limiting, passkey expiry, TTL cap). On success, appends a `GeoCheckInEntry`
    /// to the vault's persistent geo log and emits a `ci_geo` event.
    ///
    /// # Arguments
    /// * `vault_id`        - The vault to check in
    /// * `caller`          - Must be the vault owner
    /// * `passkey_hash`    - Passkey used for this check-in
    /// * `latitude_micro`  - Latitude in microdegrees (e.g. 37_422_000 = 37.422°)
    /// * `longitude_micro` - Longitude in microdegrees
    /// * `country_code`    - ISO 3166-1 alpha-2 country code (e.g. "US")
    ///
    /// # Errors
    /// Same as `check_in`.
    pub fn check_in_with_geo(
        env: Env,
        vault_id: u64,
        caller: Address,
        passkey_hash: BytesN<32>,
        latitude_micro: i64,
        longitude_micro: i64,
        country_code: String,
    ) -> Result<(), ContractError> {
        // Delegate to standard check_in for all validations (owner call; nonce unused)
        Self::check_in(env.clone(), vault_id, caller, passkey_hash, 0)?;

        let now = env.ledger().timestamp();
        let entry = GeoCheckInEntry {
            latitude_micro,
            longitude_micro,
            country_code: country_code.clone(),
            timestamp: now,
        };

        let key = DataKey::CheckInGeoLog(vault_id);
        let mut log: Vec<GeoCheckInEntry> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env));
        log.push_back(entry);

        let vault = Self::load_vault(&env, vault_id);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, &log);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);

        env.events().publish(
            (CHECKIN_GEO_TOPIC, vault_id),
            (latitude_micro, longitude_micro, country_code, now),
        );
        Ok(())
    }

    /// Returns the full geographic check-in history for a vault.
    pub fn get_geo_checkin_log(env: Env, vault_id: u64) -> Vec<GeoCheckInEntry> {
        env.storage()
            .persistent()
            .get(&DataKey::CheckInGeoLog(vault_id))
            .unwrap_or_else(|| Vec::new(&env))
    }
}
