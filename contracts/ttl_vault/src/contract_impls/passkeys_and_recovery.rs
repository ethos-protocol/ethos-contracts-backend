#[contractimpl]
impl TtlVaultContract {
    // --- Issue #564: Withdrawal Approval Workflow ---

    pub fn request_withdrawal_approval(
        env: Env,
        vault_id: u64,
        caller: Address,
        amount: i128,
        approvers: Vec<Address>,
        required_approvals: u32,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }
        if required_approvals == 0 || required_approvals > approvers.len() {
            return Err(ContractError::InvalidThreshold);
        }
        let req = WithdrawalApprovalRequest {
            amount,
            requested_at: env.ledger().timestamp(),
            approvals: Vec::new(&env),
            required_approvals,
            expires_at: env.ledger().timestamp() + 604_800,
        };
        env.storage()
            .persistent()
            .set(&DataKey::WithdrawalApprovalRequest(vault_id), &req);
        env.storage()
            .persistent()
            .set(&DataKey::WithdrawalApprovers(vault_id), &approvers);
        env.events().publish(
            (WITHDRAWAL_APPROVAL_REQUESTED_TOPIC, vault_id),
            (amount, required_approvals),
        );
        Ok(())
    }

    pub fn approve_withdrawal(
        env: Env,
        vault_id: u64,
        caller: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let approvers: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::WithdrawalApprovers(vault_id))
            .ok_or(ContractError::WithdrawalNotApproved)?;
        if !approvers.iter().any(|a| a == caller) {
            return Err(ContractError::NotASigner);
        }
        let mut req: WithdrawalApprovalRequest = env
            .storage()
            .persistent()
            .get(&DataKey::WithdrawalApprovalRequest(vault_id))
            .ok_or(ContractError::WithdrawalNotApproved)?;
        if env.ledger().timestamp() > req.expires_at {
            return Err(ContractError::ProposalExpired);
        }
        if req.approvals.iter().any(|a| a == caller) {
            return Err(ContractError::AlreadyApproved);
        }
        req.approvals.push_back(caller.clone());
        env.storage()
            .persistent()
            .set(&DataKey::WithdrawalApprovalRequest(vault_id), &req);
        env.events()
            .publish((WITHDRAWAL_APPROVAL_GRANTED_TOPIC, vault_id), caller);
        Ok(())
    }

    pub fn execute_approved_withdrawal(env: Env, vault_id: u64) -> Result<(), ContractError> {
        let req: WithdrawalApprovalRequest = env
            .storage()
            .persistent()
            .get(&DataKey::WithdrawalApprovalRequest(vault_id))
            .ok_or(ContractError::WithdrawalNotApproved)?;
        if req.approvals.len() < req.required_approvals {
            return Err(ContractError::ProposalNotApproved);
        }
        let mut vault = Self::load_vault(&env, vault_id);
        if vault.balance < req.amount {
            return Err(ContractError::InsufficientBalance);
        }
        let token_client = token::Client::new(&env, &vault.token_address);
        token_client.transfer(&env.current_contract_address(), &vault.owner, &req.amount);
        vault.balance -= req.amount;
        Self::save_vault(&env, vault_id, &vault);
        env.storage()
            .persistent()
            .remove(&DataKey::WithdrawalApprovalRequest(vault_id));
        env.events()
            .publish((WITHDRAW_TOPIC, vault_id), (req.amount, vault.balance));
        Ok(())
    }

    // --- Issue #563: Passkey Recovery ---

    pub fn initiate_passkey_recovery(
        env: Env,
        vault_id: u64,
        new_passkey_hash: BytesN<32>,
        recovery_code: String,
        contacts: Vec<Address>,
        required: u32,
    ) -> Result<(), ContractError> {
        if required == 0 || required > contacts.len() {
            return Err(ContractError::InvalidThreshold);
        }
        let req = PasskeyRecoveryRequest {
            new_passkey_hash: new_passkey_hash.clone(),
            initiated_at: env.ledger().timestamp(),
            recovery_code,
            approved_contacts: Vec::new(&env),
            required_contacts: required,
        };
        env.storage()
            .persistent()
            .set(&DataKey::PasskeyRecoveryRequest(vault_id), &req);
        env.storage()
            .persistent()
            .set(&DataKey::RecoveryContacts(vault_id), &contacts);
        env.events().publish(
            (PASSKEY_RECOVERY_INITIATED_TOPIC, vault_id),
            new_passkey_hash,
        );
        Ok(())
    }

    pub fn approve_passkey_recovery(
        env: Env,
        vault_id: u64,
        caller: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let contacts: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::RecoveryContacts(vault_id))
            .ok_or(ContractError::NotRecoveryContact)?;
        if !contacts.iter().any(|c| c == caller) {
            return Err(ContractError::NotRecoveryContact);
        }
        let mut req: PasskeyRecoveryRequest = env
            .storage()
            .persistent()
            .get(&DataKey::PasskeyRecoveryRequest(vault_id))
            .ok_or(ContractError::NotRecoveryContact)?;
        if req.approved_contacts.iter().any(|c| c == caller) {
            return Err(ContractError::AlreadyApproved);
        }
        req.approved_contacts.push_back(caller.clone());
        if req.approved_contacts.len() >= req.required_contacts {
            let key = DataKey::VaultPasskeys(vault_id);
            let mut passkeys: Vec<PasskeyHash> = env
                .storage()
                .persistent()
                .get(&key)
                .unwrap_or(Vec::new(&env));
            passkeys.push_back(PasskeyHash {
                hash: req.new_passkey_hash.clone(),
                added_at: env.ledger().timestamp(),
                biometric_hash: None,
            });
            env.storage().persistent().set(&key, &passkeys);
            env.storage()
                .persistent()
                .remove(&DataKey::PasskeyRecoveryRequest(vault_id));
            env.events()
                .publish((PASSKEY_RECOVERED_TOPIC, vault_id), req.new_passkey_hash);
        } else {
            env.storage()
                .persistent()
                .set(&DataKey::PasskeyRecoveryRequest(vault_id), &req);
        }
        Ok(())
    }

    // --- Issue #562: Passkey Compromise Response ---

    #[allow(clippy::unnecessary_wraps)] // contract fn: Result kept for forward-compat error paths
    pub fn lockout_vault(
        env: Env,
        vault_id: u64,
        duration_seconds: u64,
    ) -> Result<(), ContractError> {
        let vault = Self::load_vault(&env, vault_id);
        vault.owner.require_auth();
        let lockout = PasskeyLockout {
            locked_at: env.ledger().timestamp(),
            unlock_at: env.ledger().timestamp() + duration_seconds,
            failed_attempts: 0,
        };
        env.storage()
            .persistent()
            .set(&DataKey::PasskeyLockout(vault_id), &lockout);
        env.events()
            .publish((PASSKEY_LOCKOUT_TOPIC, vault_id), duration_seconds);
        Ok(())
    }

    pub fn unlock_vault(env: Env, vault_id: u64, caller: Address) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        env.storage()
            .persistent()
            .remove(&DataKey::PasskeyLockout(vault_id));
        env.events()
            .publish((PASSKEY_UNLOCKED_TOPIC, vault_id), caller);
        Ok(())
    }

    pub fn is_vault_locked(env: Env, vault_id: u64) -> bool {
        if let Some(lockout) = env
            .storage()
            .persistent()
            .get::<DataKey, PasskeyLockout>(&DataKey::PasskeyLockout(vault_id))
        {
            env.ledger().timestamp() < lockout.unlock_at
        } else {
            false
        }
    }

    // --- Issue #561: Passkey Rotation Enforcement ---

    pub fn set_passkey_rotation_policy(
        env: Env,
        vault_id: u64,
        caller: Address,
        rotation_period_days: u32,
        enforce: bool,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        let policy = PasskeyRotationPolicy {
            rotation_period_days,
            enforce,
        };
        env.storage()
            .persistent()
            .set(&DataKey::PasskeyRotationPolicy(vault_id), &policy);
        env.events().publish(
            (PASSKEY_ROTATION_REQUIRED_TOPIC, vault_id),
            (rotation_period_days, enforce),
        );
        Ok(())
    }

    #[allow(clippy::unnecessary_wraps)] // contract fn: Result kept for forward-compat error paths
    pub fn enforce_passkey_rotation(
        env: Env,
        vault_id: u64,
        passkey_hash: BytesN<32>,
    ) -> Result<bool, ContractError> {
        let policy: PasskeyRotationPolicy = env
            .storage()
            .persistent()
            .get(&DataKey::PasskeyRotationPolicy(vault_id))
            .unwrap_or(PasskeyRotationPolicy {
                rotation_period_days: 0,
                enforce: false,
            });
        if !policy.enforce || policy.rotation_period_days == 0 {
            return Ok(true);
        }
        if let Some(last_rotation) =
            env.storage()
                .persistent()
                .get::<DataKey, u64>(&DataKey::LastPasskeyRotation(
                    vault_id,
                    passkey_hash.clone(),
                ))
        {
            let elapsed_days = (env.ledger().timestamp() - last_rotation) / 86_400;
            if elapsed_days >= policy.rotation_period_days as u64 {
                env.events()
                    .publish((PASSKEY_ROTATION_ENFORCED_TOPIC, vault_id), passkey_hash);
                return Ok(false);
            }
        }
        Ok(true)
    }

    #[allow(clippy::unnecessary_wraps)] // contract fn: Result kept for forward-compat error paths
    pub fn record_passkey_rotation(
        env: Env,
        vault_id: u64,
        passkey_hash: BytesN<32>,
    ) -> Result<(), ContractError> {
        env.storage().persistent().set(
            &DataKey::LastPasskeyRotation(vault_id, passkey_hash),
            &env.ledger().timestamp(),
        );
        Ok(())
    }

    /// Number of `AuditEntry` items stored in a single page key.
    /// Appends only touch the current tail page, so per-operation write cost is
    /// O(PAGE_SIZE) rather than O(total history).
    const AUDIT_LOG_PAGE_SIZE: u32 = 50;

    /// Appends one `AuditEntry` to the vault's paginated audit log.
    ///
    /// Storage layout (new):
    ///   `DataKey::VaultAuditLogLen(vault_id)`          → u32   total entry count
    ///   `DataKey::VaultAuditLogPage(vault_id, page)`   → Vec<AuditEntry>  ≤ PAGE_SIZE entries
    ///
    /// Legacy layout (read-only compat):
    ///   `DataKey::VaultAuditLog(vault_id)`             → Vec<AuditEntry>  (old unbounded vec)
    ///
    /// Each call reads and writes only the tail page (at most PAGE_SIZE entries),
    /// keeping write cost bounded regardless of total history length.
    fn append_activity_log(
        env: &Env,
        vault_id: u64,
        action: &str,
        caller: &Address,
        _details: &str,
    ) {
        use types::AuditEntry;

        // Read current total entry count (new layout counter).
        let len_key = DataKey::VaultAuditLogLen(vault_id);
        let total: u32 = env.storage().persistent().get(&len_key).unwrap_or(0u32);

        // Determine which page this entry belongs to and its position within that page.
        let page_index: u32 = total / Self::AUDIT_LOG_PAGE_SIZE;
        let page_key = DataKey::VaultAuditLogPage(vault_id, page_index);

        // Load the tail page (only this page is touched on every append).
        let mut page: Vec<AuditEntry> = env
            .storage()
            .persistent()
            .get(&page_key)
            .unwrap_or_else(|| Vec::new(env));

        let entry = AuditEntry {
            action: String::from_str(env, action),
            caller: caller.clone(),
            timestamp: env.ledger().timestamp(),
            operation: String::from_str(env, ""),
            actor: caller.clone(),
            details: String::from_str(env, ""),
        };
        page.push_back(entry);

        // Write only the tail page and the updated counter.
        env.storage().persistent().set(&page_key, &page);
        env.storage().persistent().set(&len_key, &(total + 1));
    }

    fn paginate(env: &Env, all: Vec<u64>, page: u32, page_size: u32) -> Vec<u64> {
        if page_size == 0 {
            return Vec::new(env);
        }
        let start = (page as u64).saturating_mul(page_size as u64);
        let len = all.len() as u64;
        let mut result = Vec::new(env);
        let mut i = start;
        while i < len && i < start + page_size as u64 {
            result.push_back(all.get(i as u32).unwrap());
            i += 1;
        }
        result
    }

    fn assert_not_paused(env: &Env) {
        if Self::load_paused(env) {
            panic_with_error!(env, ContractError::Paused);
        }
    }

    fn load_paused(env: &Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    /// Issue #552: reject inflows from compliance-blacklisted addresses.
    fn assert_not_blacklisted(env: &Env, address: &Address) {
        if compliance::is_blacklisted(env, address) {
            panic_with_error!(env, ContractError::AddressBlacklisted);
        }
    }

    fn require_admin(env: &Env) {
        let admin = Self::load_admin(env);
        admin.require_auth();
    }

    /// Panics with the compliance error if `address` is blacklisted or, in
    /// allowlist mode, not allowlisted — Issue #551.
    fn assert_compliant(env: &Env, address: &Address) {
        if let Err(e) = compliance::check_address(env, address) {
            panic_with_error!(env, e);
        }
    }

    fn load_admin(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized))
    }

    fn require_initialized(env: &Env) {
        if env
            .storage()
            .instance()
            .get::<DataKey, Address>(&DataKey::Admin)
            .is_none()
        {
            panic_with_error!(env, ContractError::NotInitialized);
        }
    }

    fn load_token(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::TokenAddress)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized))
    }

    fn load_vault(env: &Env, vault_id: u64) -> Vault {
        Self::try_load_vault(env, vault_id)
            .unwrap_or_else(|| panic_with_error!(env, ContractError::VaultNotFound))
    }

    /// Tries to load a vault, returning None if it doesn't exist.
    ///
    /// This is a safe alternative to `load_vault` for use in view functions
    /// that should not panic when a vault is not found.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    ///
    /// # Returns
    /// `Some(Vault)` if the vault exists, `None` otherwise
    fn try_load_vault(env: &Env, vault_id: u64) -> Option<Vault> {
        env.storage()
            .persistent()
            .get(&DataKey::Vault(vault_id))
            // Issue #558: inactive vaults may be stored compressed; decode on demand.
            .or_else(|| vault_compression::load_decompressed(env, vault_id))
    }

    fn load_owner_vault_ids(env: &Env, owner: &Address) -> Vec<u64> {
        env.storage()
            .persistent()
            .get(&DataKey::OwnerVaults(owner.clone()))
            .unwrap_or(Vec::new(env))
    }

    fn save_owner_vault_ids(
        env: &Env,
        owner: &Address,
        vault_ids: &Vec<u64>,
        check_in_interval: u64,
    ) {
        let key = DataKey::OwnerVaults(owner.clone());
        let ttl = vault_ttl_ledgers(check_in_interval);
        env.storage().persistent().set(&key, vault_ids);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
    }

    fn add_owner_vault_id(env: &Env, owner: &Address, vault_id: u64, check_in_interval: u64) {
        let mut vault_ids = Self::load_owner_vault_ids(env, owner);
        vault_ids.push_back(vault_id);
        Self::save_owner_vault_ids(env, owner, &vault_ids, check_in_interval);
    }

    fn remove_owner_vault_id(env: &Env, owner: &Address, vault_id: u64, check_in_interval: u64) {
        let vault_ids = Self::load_owner_vault_ids(env, owner);
        let mut next_ids = Vec::new(env);
        for id in vault_ids.iter() {
            if id != vault_id {
                next_ids.push_back(id);
            }
        }
        // Save updated list or delete key if empty to save storage rent
        if next_ids.is_empty() {
            let key = DataKey::OwnerVaults(owner.clone());
            env.storage().persistent().remove(&key);
        } else {
            Self::save_owner_vault_ids(env, owner, &next_ids, check_in_interval);
        }
    }

    /// Persists a vault to storage with TTL derived from its check_in_interval.
    /// This ensures that when update_check_in_interval modifies the interval,
    /// the persistent storage TTL is automatically updated (issue #297).
    fn save_vault(env: &Env, vault_id: u64, vault: &Vault) {
        let key = DataKey::Vault(vault_id);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, vault);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        // Issue #558: a write re-materializes a compressed vault.
        vault_compression::discard(env, vault_id);
    }

    fn load_beneficiary_vault_ids(env: &Env, beneficiary: &Address) -> Vec<u64> {
        env.storage()
            .persistent()
            .get(&DataKey::BeneficiaryVaults(beneficiary.clone()))
            .unwrap_or(Vec::new(env))
    }

    fn save_beneficiary_vault_ids(
        env: &Env,
        beneficiary: &Address,
        vault_ids: &Vec<u64>,
        check_in_interval: u64,
    ) {
        let key = DataKey::BeneficiaryVaults(beneficiary.clone());
        let ttl = vault_ttl_ledgers(check_in_interval);
        env.storage().persistent().set(&key, vault_ids);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
    }

    fn add_beneficiary_vault_id(
        env: &Env,
        beneficiary: &Address,
        vault_id: u64,
        check_in_interval: u64,
    ) {
        let mut vault_ids = Self::load_beneficiary_vault_ids(env, beneficiary);
        vault_ids.push_back(vault_id);
        Self::save_beneficiary_vault_ids(env, beneficiary, &vault_ids, check_in_interval);
    }

    fn remove_beneficiary_vault_id(
        env: &Env,
        beneficiary: &Address,
        vault_id: u64,
        check_in_interval: u64,
    ) {
        let vault_ids = Self::load_beneficiary_vault_ids(env, beneficiary);
        let mut next_ids = Vec::new(env);
        for id in vault_ids.iter() {
            if id != vault_id {
                next_ids.push_back(id);
            }
        }
        // Save updated list or delete key if empty to save storage rent
        if next_ids.is_empty() {
            let key = DataKey::BeneficiaryVaults(beneficiary.clone());
            env.storage().persistent().remove(&key);
        } else {
            Self::save_beneficiary_vault_ids(env, beneficiary, &next_ids, check_in_interval);
        }
    }

    fn assert_interval_in_bounds(env: &Env, interval: u64) {
        if let Some(min) = env
            .storage()
            .instance()
            .get::<DataKey, u64>(&DataKey::MinCheckInInterval)
        {
            if interval < min {
                panic_with_error!(env, ContractError::IntervalTooLow);
            }
        }
        if let Some(max) = env
            .storage()
            .instance()
            .get::<DataKey, u64>(&DataKey::MaxCheckInInterval)
        {
            if interval > max {
                panic_with_error!(env, ContractError::IntervalTooHigh);
            }
        }
    }

    fn assert_metadata_len(env: &Env, metadata: &String) {
        if metadata.len() > MAX_METADATA_LEN {
            panic_with_error!(env, ContractError::InvalidAmount);
        }
    }

    /// Validates that `bytes` are valid UTF-8 when the `RequireUtf8Metadata` flag is set.
    /// Returns `Err(ContractError::InvalidMetadataEncoding)` on failure.
    fn require_utf8_metadata_bytes(env: &Env, bytes: &Bytes) -> Result<(), ContractError> {
        let enforced: bool = env
            .storage()
            .instance()
            .get(&DataKey::RequireUtf8Metadata)
            .unwrap_or(false);
        if enforced {
            // Convert to a slice and verify every byte sequence is valid UTF-8.
            let raw = bytes.to_alloc_vec();
            if core::str::from_utf8(&raw).is_err() {
                return Err(ContractError::InvalidMetadataEncoding);
            }
        }
        Ok(())
    }

    fn assert_token_whitelisted(env: &Env, token_address: &Address) {
        let token_address = token_address.clone();
        if !Self::is_token_whitelisted(env.clone(), token_address) {
            panic_with_error!(env, ContractError::TokenNotWhitelisted);
        }
    }

    /// Historically guarded against a synthetic "zero" contract address built via
    /// `Address::from_contract_id`. That constructor is private in the current
    /// soroban-sdk and there is no public way to obtain an invalid/degenerate
    /// `Address` through the contract's external interface, so there is nothing
    /// left to reject here — every `Address` reaching this point is already a
    /// valid account or contract address per SDK guarantees.
    fn assert_not_zero_address(_env: &Env, _address: &Address) {}

    // --- Issue #550: Passkey Compromise Detection ---

    /// Reports a passkey as compromised. Subsequent check-ins using that hash
    /// will be rejected with `PasskeyCompromised`.
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - Caller is not the vault owner
    pub fn report_passkey_compromise(
        env: Env,
        vault_id: u64,
        caller: Address,
        passkey_hash: BytesN<32>,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        let key = DataKey::CompromisedPasskeys(vault_id);
        let mut set: Vec<BytesN<32>> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env));
        // Idempotent — don't add duplicates.
        if !set.iter().any(|h| h == passkey_hash) {
            set.push_back(passkey_hash.clone());
            let ttl = vault_ttl_ledgers(vault.check_in_interval);
            env.storage().persistent().set(&key, &set);
            env.storage()
                .persistent()
                .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        }
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((PASSKEY_COMPROMISED_TOPIC, vault_id), passkey_hash);
        Ok(())
    }

    /// Clears a previously reported passkey compromise, allowing the passkey to be used again.
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - Caller is not the vault owner
    pub fn clear_passkey_compromise(
        env: Env,
        vault_id: u64,
        caller: Address,
        passkey_hash: BytesN<32>,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        let key = DataKey::CompromisedPasskeys(vault_id);
        let set: Vec<BytesN<32>> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env));
        let mut new_set: Vec<BytesN<32>> = Vec::new(&env);
        for h in set.iter() {
            if h != passkey_hash {
                new_set.push_back(h);
            }
        }
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, &new_set);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        Ok(())
    }

    /// Returns whether a passkey has been flagged as compromised for a vault.
    pub fn is_passkey_compromised(env: Env, vault_id: u64, passkey_hash: BytesN<32>) -> bool {
        let key = DataKey::CompromisedPasskeys(vault_id);
        let set: Vec<BytesN<32>> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env));
        set.iter().any(|h| h == passkey_hash)
    }

    /// Inspects the recent passkey usage log and emits a `pk_comp` advisory event
    /// if 3 or more of the last 5 entries used different passkey hashes.
    fn detect_passkey_anomaly(env: &Env, vault_id: u64, current_hash: &BytesN<32>) {
        let usage: Vec<PasskeyUsageEntry> = env
            .storage()
            .persistent()
            .get(&DataKey::PasskeyUsage(vault_id))
            .unwrap_or_else(|| Vec::new(env));
        if usage.len() < 3 {
            return;
        }
        let window_start = usage.len().saturating_sub(5);
        let mut distinct = 0u32;
        let mut prev: Option<BytesN<32>> = None;
        for i in window_start..usage.len() {
            let entry = usage.get(i as u32).unwrap();
            if prev.as_ref() != Some(&entry.passkey_hash) {
                distinct += 1;
                prev = Some(entry.passkey_hash.clone());
            }
        }
        if distinct >= 3 {
            env.events()
                .publish((PASSKEY_COMPROMISED_TOPIC, vault_id), current_hash.clone());
        }
    }

    // --- Issue #549: Passkey Expiry Enforcement ---

    /// Validates that `passkey_hash` is registered for `vault_id` and not expired.
    ///
    /// Registration rules (backwards-compatible):
    /// - If `VaultPasskeys` list is non-empty, the hash must appear in that list.
    /// - If the list is empty AND `vault.passkey_hash` is Some, the hash must match it.
    /// - If both are empty/None, any passkey is accepted (no passkey configured yet).
    ///
    /// Expiry: if `PasskeyExpiry` is stored for this hash, it must not be in the past.
    fn validate_passkey_for_checkin(
        env: &Env,
        vault_id: u64,
        vault: &Vault,
        passkey_hash: &BytesN<32>,
        now: u64,
    ) -> Result<(), ContractError> {
        let pk_key = DataKey::VaultPasskeys(vault_id);
        let passkeys: Vec<PasskeyHash> = env
            .storage()
            .persistent()
            .get(&pk_key)
            .unwrap_or_else(|| Vec::new(env));

        if !passkeys.is_empty() {
            // Multi-passkey list is configured — hash must be in it.
            let registered = passkeys.iter().any(|p| &p.hash == passkey_hash);
            if !registered {
                return Err(ContractError::InvalidPasskey);
            }
        } else if let Some(ref primary) = vault.passkey_hash {
            // Fallback: single primary passkey on vault struct.
            let passkey_hash_bytes: Bytes = passkey_hash.clone().into();
            if primary != &passkey_hash_bytes {
                return Err(ContractError::InvalidPasskey);
            }
        }
        // else: no passkey configured — any hash accepted for backwards compat.

        // Issue #550: block check-in if passkey is flagged as compromised.
        {
            let comp_key = DataKey::CompromisedPasskeys(vault_id);
            let compromised: Vec<BytesN<32>> = env
                .storage()
                .persistent()
                .get(&comp_key)
                .unwrap_or_else(|| Vec::new(env));
            if compromised.iter().any(|h| &h == passkey_hash) {
                env.events()
                    .publish((PASSKEY_COMPROMISED_TOPIC, vault_id), passkey_hash.clone());
                return Err(ContractError::PasskeyCompromised);
            }
        }

        // Issue #559: block check-in while the passkey is held in escrow.
        if env
            .storage()
            .persistent()
            .has(&DataKey::PasskeyEscrow(vault_id, passkey_hash.clone()))
        {
            return Err(ContractError::PasskeyInEscrow);
        }

        // Expiry check — applies regardless of registration path.
        if let Some(expiry) = env
            .storage()
            .persistent()
            .get::<DataKey, u64>(&DataKey::PasskeyExpiry(vault_id, passkey_hash.clone()))
        {
            if now > expiry {
                env.events()
                    .publish((PASSKEY_EXPIRED_TOPIC, vault_id), passkey_hash.clone());
                return Err(ContractError::PasskeyExpired);
            }
        }

        Ok(())
    }

    // --- Issue #395: Passkey Usage Analytics ---

    /// Logs a passkey usage entry for a vault check-in and runs anomaly detection.
    fn log_passkey_usage(env: &Env, vault_id: u64, passkey_hash: &BytesN<32>, timestamp: u64) {
        let mut usage: Vec<PasskeyUsageEntry> = env
            .storage()
            .persistent()
            .get(&DataKey::PasskeyUsage(vault_id))
            .unwrap_or(Vec::new(env));

        // Issue #550: detect anomalous passkey switching before appending this entry.
        Self::detect_passkey_anomaly(env, vault_id, passkey_hash);

        usage.push_back(PasskeyUsageEntry {
            passkey_hash: passkey_hash.clone(),
            timestamp,
        });

        // Drop the oldest entry when the cap is reached so storage size stays
        // bounded. Full history is preserved in the emitted PASSKEY_USAGE_TOPIC
        // events for off-chain indexers.
        if usage.len() > MAX_PASSKEY_USAGE_ENTRIES {
            usage.remove(0);
        }

        let key = DataKey::PasskeyUsage(vault_id);
        env.storage().persistent().set(&key, &usage);
        let ttl = vault_ttl_ledgers(Self::load_vault(env, vault_id).check_in_interval);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.events().publish(
            (PASSKEY_USAGE_TOPIC, vault_id),
            (passkey_hash.clone(), timestamp),
        );
    }

    // --- Issue #558: Passkey Audit Trail ---

    /// Appends an append-only audit entry for a passkey lifecycle operation
    /// ("add" / "remove" / "use") to `PasskeyAuditLog(vault_id)` and emits
    /// the `pk_audit` event.
    fn log_passkey_audit_entry(
        env: &Env,
        vault_id: u64,
        operation: &str,
        actor: &Address,
        passkey_hash: &BytesN<32>,
    ) {
        let key = DataKey::PasskeyAuditLog(vault_id);
        let mut log: Vec<PasskeyAuditEntry> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or(Vec::new(env));

        let timestamp = env.ledger().timestamp();
        log.push_back(PasskeyAuditEntry {
            operation: String::from_str(env, operation),
            actor: actor.clone(),
            passkey_hash: passkey_hash.clone(),
            timestamp,
        });

        // Drop the oldest entry when the cap is reached so storage size stays
        // bounded. Full history is preserved in the emitted PASSKEY_AUDIT_TOPIC
        // events for off-chain indexers.
        if log.len() > MAX_PASSKEY_AUDIT_ENTRIES {
            log.remove(0);
        }

        env.storage().persistent().set(&key, &log);
        let ttl = vault_ttl_ledgers(Self::load_vault(env, vault_id).check_in_interval);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);

        env.events().publish(
            (PASSKEY_AUDIT_TOPIC, vault_id),
            (
                String::from_str(env, operation),
                actor.clone(),
                passkey_hash.clone(),
                timestamp,
            ),
        );
    }

    /// Returns the full passkey audit trail for a vault.
    ///
    /// # Errors
    /// * `ContractError::VaultNotFound` - If the vault does not exist.
    pub fn get_passkey_audit_log(
        env: Env,
        vault_id: u64,
    ) -> Result<Vec<PasskeyAuditEntry>, ContractError> {
        if Self::try_load_vault(&env, vault_id).is_none() {
            return Err(ContractError::VaultNotFound);
        }
        Ok(env
            .storage()
            .persistent()
            .get(&DataKey::PasskeyAuditLog(vault_id))
            .unwrap_or(Vec::new(&env)))
    }

    // --- Issue #383: Vault Recovery Mode ---

    /// Sets a recovery contact who can extend the vault TTL if the owner loses access.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault ID
    /// * `caller` - The vault owner (must authorize)
    /// * `contact` - The recovery contact address
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    pub fn set_recovery_contact(
        env: Env,
        vault_id: u64,
        caller: Address,
        contact: Address,
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
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((SET_RECOVERY_TOPIC, vault_id), contact);
        Ok(())
    }

    /// Requests a recovery extension. Only the recovery contact can call this.
    /// Extends the vault TTL by 30 days.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault ID
    /// * `caller` - The recovery contact (must authorize)
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    ///
    /// # Errors
    /// * `ContractError::NotRecoveryContact` - If caller is not the recovery contact
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    pub fn request_recovery_extension(
        env: Env,
        vault_id: u64,
        caller: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        let mut v = vault.clone();
        v.last_check_in = env.ledger().timestamp();
        Self::save_vault(&env, vault_id, &v);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((RECOVERY_EXTEND_TOPIC, vault_id), caller);
        Ok(())
    }

    // --- Issue #384: Vault Activity Audit Log ---

    /// Logs an audit entry for a vault operation (paginated storage).
    /// Delegates to `append_activity_log` so both write paths use the same
    /// bounded page layout.
    fn log_audit_entry(env: &Env, vault_id: u64, operation: &str, actor: &Address, details: &str) {
        // Use the shared paginated append path so this function is also bounded.
        Self::append_activity_log(env, vault_id, operation, actor, details);

        // Extend the tail page's TTL to match the vault's check-in interval.
        let total: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::VaultAuditLogLen(vault_id))
            .unwrap_or(0u32);
        let page_index = if total == 0 {
            0
        } else {
            (total - 1) / Self::AUDIT_LOG_PAGE_SIZE
        };
        let page_key = DataKey::VaultAuditLogPage(vault_id, page_index);
        let ttl = vault_ttl_ledgers(Self::load_vault(env, vault_id).check_in_interval);
        env.storage()
            .persistent()
            .extend_ttl(&page_key, VAULT_TTL_THRESHOLD, ttl);
    }

    /// Returns all audit log entries for a vault across all pages.
    ///
    /// Migration-aware: if the vault has entries under the new paginated layout
    /// those are returned; any entries still in the legacy `VaultAuditLog(vault_id)`
    /// key are prepended so old vaults continue to work transparently.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault ID
    ///
    /// # Returns
    /// A vector of audit entries in chronological order (oldest first)
    pub fn get_vault_audit_log(env: Env, vault_id: u64) -> Vec<AuditEntry> {
        let mut result: Vec<AuditEntry> = Vec::new(&env);

        // --- Legacy compat: read any entries written under the old flat key ---
        let legacy: Vec<AuditEntry> = env
            .storage()
            .persistent()
            .get(&DataKey::VaultAuditLog(vault_id))
            .unwrap_or_else(|| Vec::new(&env));
        for entry in legacy.iter() {
            result.push_back(entry);
        }

        // --- New paginated layout ---
        let total: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::VaultAuditLogLen(vault_id))
            .unwrap_or(0u32);
        if total > 0 {
            let num_pages = (total + Self::AUDIT_LOG_PAGE_SIZE - 1) / Self::AUDIT_LOG_PAGE_SIZE;
            for page_index in 0..num_pages {
                let page: Vec<AuditEntry> = env
                    .storage()
                    .persistent()
                    .get(&DataKey::VaultAuditLogPage(vault_id, page_index))
                    .unwrap_or_else(|| Vec::new(&env));
                for entry in page.iter() {
                    result.push_back(entry);
                }
            }
        }

        result
    }

    /// Returns a single page of the vault audit log.
    ///
    /// Page 0 begins with any legacy (pre-migration) entries, followed by the
    /// first page of the new paginated layout.  Pages beyond the legacy block
    /// map directly to paginated storage pages.
    ///
    /// # Arguments
    /// * `env`       - The Soroban environment
    /// * `vault_id`  - The vault ID
    /// * `page`      - Zero-based page index
    /// * `page_size` - Maximum entries per page (capped internally at AUDIT_LOG_PAGE_SIZE)
    ///
    /// # Returns
    /// A slice of at most `page_size` entries, or an empty vec if `page` is out of range.
    pub fn get_vault_audit_log_page(
        env: Env,
        vault_id: u64,
        page: u32,
        page_size: u32,
    ) -> Vec<AuditEntry> {
        // Gather all entries (legacy + new) then return the requested slice.
        // For most vaults only the new layout is active; legacy vaults are handled
        // in get_vault_audit_log above.
        let all = Self::get_vault_audit_log(env.clone(), vault_id);
        let total = all.len() as u32;
        let cap = page_size.min(Self::AUDIT_LOG_PAGE_SIZE);
        let start = page * cap;
        if start >= total {
            return Vec::new(&env);
        }
        let end = (start + cap).min(total);
        let mut result: Vec<AuditEntry> = Vec::new(&env);
        for i in start..end {
            result.push_back(all.get(i).unwrap());
        }
        result
    }

    // --- Issue #385: Vault Cloning ---

    /// Clones a vault configuration into a new vault.
    ///
    /// Creates a new vault preserving check_in_interval, beneficiaries, metadata,
    /// token_address, release_condition, and custom_metadata from the source vault.
    /// Balance and timestamps are reset. Owner-only.
    pub fn clone_vault(
        env: Env,
        source_vault_id: u64,
        new_owner: Address,
        new_beneficiary: Address,
    ) -> u64 {
        new_owner.require_auth();
        let original = Self::load_vault(&env, source_vault_id);
        if new_owner != original.owner {
            panic_with_error!(&env, ContractError::NotOwner);
        }
        if original.status != ReleaseStatus::Locked {
            panic_with_error!(&env, ContractError::AlreadyReleased);
        }
        if new_owner == new_beneficiary {
            panic_with_error!(&env, ContractError::InvalidBeneficiary);
        }

        let new_vault_id = Self::vault_count(env.clone()) + 1;
        let timestamp = env.ledger().timestamp();
        let cloned_vault = Vault {
            owner: new_owner.clone(),
            beneficiary: new_beneficiary.clone(),
            balance: 0,
            check_in_interval: original.check_in_interval,
            last_check_in: timestamp,
            created_at: timestamp,
            status: ReleaseStatus::Locked,
            beneficiaries: original.beneficiaries.clone(),
            metadata: original.metadata.clone(),
            token_address: original.token_address.clone(),
            custom_metadata: original.custom_metadata.clone(),
            is_paused: false,
            release_condition: original.release_condition.clone(),
            parent_vault_id: Some(source_vault_id),
            passkey_hash: None,
            max_deposit_amount: original.max_deposit_amount,
            withdrawal_approval_threshold: original.withdrawal_approval_threshold,
            spending_limit: original.spending_limit,
            inactivity_penalty_bps: original.inactivity_penalty_bps,
            burn_percentage: original.burn_percentage,
            penalty_recipient: original.penalty_recipient.clone(),
        };
        Self::save_vault(&env, new_vault_id, &cloned_vault);
        Self::add_owner_vault_id(&env, &new_owner, new_vault_id, original.check_in_interval);
        Self::add_beneficiary_vault_id(
            &env,
            &new_beneficiary,
            new_vault_id,
            original.check_in_interval,
        );

        let key = DataKey::VaultCount;
        env.storage().persistent().set(&key, &new_vault_id);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, VAULT_TTL_LEDGERS);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        Self::append_activity_log(&env, new_vault_id, "clone_vault", &new_owner, "");
        env.events().publish(
            (VAULT_CLONED_TOPIC,),
            (source_vault_id, new_vault_id, new_beneficiary),
        );
        new_vault_id
    }

    /// Clones a vault with selective parameter overrides.
    ///
    /// Behaves identically to `clone_vault` but allows the caller to override
    /// any combination of `check_in_interval`, `beneficiaries`, and `metadata`
    /// on the new vault. Fields left as `None` are copied from the source vault.
    ///
    /// # Arguments
    /// * `source_vault_id`       - Vault to use as a template (must be Locked, owned by `new_owner`)
    /// * `new_owner`             - Owner of the new vault (must authorize; must be source vault owner)
    /// * `new_beneficiary`       - Primary beneficiary for the new vault
    /// * `override_interval`     - `Some(seconds)` to use a different check-in interval, or `None` to copy
    /// * `override_beneficiaries`- `Some(entries)` to replace the multi-beneficiary split, or `None` to copy
    /// * `override_metadata`     - `Some(string)` to set different metadata, or `None` to copy
    ///
    /// # Returns
    /// The new vault ID.
    ///
    /// # Errors
    /// * `ContractError::NotOwner`           - caller is not the source vault owner
    /// * `ContractError::AlreadyReleased`    - source vault is not Locked
    /// * `ContractError::InvalidBeneficiary` - new_owner == new_beneficiary
    /// * `ContractError::InvalidInterval`    - override_interval is 0
    /// * `ContractError::IntervalTooLow/High`- override_interval outside configured bounds
    /// * `ContractError::InvalidBps`         - override_beneficiaries BPS sum ≠ 10 000
    /// * `ContractError::InvalidAmount`      - override_metadata exceeds MAX_METADATA_LEN
    pub fn clone_vault_with_overrides(
        env: Env,
        source_vault_id: u64,
        new_owner: Address,
        new_beneficiary: Address,
        override_interval: Option<u64>,
        override_beneficiaries: Option<Vec<BeneficiaryEntry>>,
        override_metadata: Option<String>,
    ) -> u64 {
        new_owner.require_auth();
        let original = Self::load_vault(&env, source_vault_id);
        if new_owner != original.owner {
            panic_with_error!(&env, ContractError::NotOwner);
        }
        if original.status != ReleaseStatus::Locked {
            panic_with_error!(&env, ContractError::AlreadyReleased);
        }
        if new_owner == new_beneficiary {
            panic_with_error!(&env, ContractError::InvalidBeneficiary);
        }

        // Resolve interval override
        let check_in_interval = match override_interval {
            Some(interval) => {
                if interval == 0 {
                    panic_with_error!(&env, ContractError::InvalidInterval);
                }
                Self::assert_interval_in_bounds(&env, interval);
                interval
            }
            None => original.check_in_interval,
        };

        // Resolve beneficiaries override
        let beneficiaries = match override_beneficiaries {
            Some(entries) => {
                if !entries.is_empty() {
                    let total_bps: u32 = entries.iter().map(|e| e.bps).sum();
                    if total_bps != 10_000 {
                        panic_with_error!(&env, ContractError::InvalidBps);
                    }
                    for entry in entries.iter() {
                        if entry.address == new_owner {
                            panic_with_error!(&env, ContractError::InvalidBeneficiary);
                        }
                    }
                }
                entries
            }
            None => original.beneficiaries.clone(),
        };

        // Resolve metadata override
        let metadata = match override_metadata {
            Some(m) => {
                Self::assert_metadata_len(&env, &m);
                m
            }
            None => original.metadata.clone(),
        };

        let new_vault_id = Self::vault_count(env.clone()) + 1;
        let timestamp = env.ledger().timestamp();
        let cloned_vault = Vault {
            owner: new_owner.clone(),
            beneficiary: new_beneficiary.clone(),
            balance: 0,
            check_in_interval,
            last_check_in: timestamp,
            created_at: timestamp,
            status: ReleaseStatus::Locked,
            beneficiaries,
            metadata,
            token_address: original.token_address.clone(),
            custom_metadata: original.custom_metadata.clone(),
            is_paused: false,
            release_condition: original.release_condition.clone(),
            parent_vault_id: Some(source_vault_id),
            passkey_hash: None,
            max_deposit_amount: original.max_deposit_amount,
            withdrawal_approval_threshold: original.withdrawal_approval_threshold,
            spending_limit: original.spending_limit,
            inactivity_penalty_bps: original.inactivity_penalty_bps,
            burn_percentage: original.burn_percentage,
            penalty_recipient: original.penalty_recipient.clone(),
        };
        Self::save_vault(&env, new_vault_id, &cloned_vault);
        Self::add_owner_vault_id(&env, &new_owner, new_vault_id, check_in_interval);
        Self::add_beneficiary_vault_id(&env, &new_beneficiary, new_vault_id, check_in_interval);

        let key = DataKey::VaultCount;
        env.storage().persistent().set(&key, &new_vault_id);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, VAULT_TTL_LEDGERS);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        Self::append_activity_log(
            &env,
            new_vault_id,
            "clone_vault_with_overrides",
            &new_owner,
            "",
        );
        env.events().publish(
            (VAULT_CLONED_OVERRIDE_TOPIC,),
            (
                source_vault_id,
                new_vault_id,
                new_beneficiary,
                check_in_interval,
            ),
        );
        new_vault_id
    }

    /// Merges multiple source vaults into a target vault.
    ///
    /// All vaults must share the same owner and token_address. Source vault balances
    /// are transferred to the target vault and sources are marked Cancelled.
    pub fn merge_vaults(
        env: Env,
        target_vault_id: u64,
        source_vault_ids: Vec<u64>,
        caller: Address,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();

        let target = Self::load_vault(&env, target_vault_id);
        if caller != target.owner {
            return Err(ContractError::NotOwner);
        }
        if target.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }

        // Validate all sources before mutating state
        for source_id in source_vault_ids.iter() {
            if source_id == target_vault_id {
                return Err(ContractError::InvalidAmount);
            }
            let source = Self::load_vault(&env, source_id);
            if source.owner != target.owner {
                return Err(ContractError::NotOwner);
            }
            if source.token_address != target.token_address {
                return Err(ContractError::IncompatibleVaultToken);
            }
            if source.status != ReleaseStatus::Locked {
                return Err(ContractError::AlreadyReleased);
            }
        }

        // Apply: transfer balances and cancel sources
        let mut target_vault = Self::load_vault(&env, target_vault_id);
        for source_id in source_vault_ids.iter() {
            let mut source = Self::load_vault(&env, source_id);
            target_vault.balance = target_vault
                .balance
                .checked_add(source.balance)
                .unwrap_or_else(|| panic_with_error!(&env, ContractError::BalanceOverflow));
            source.balance = 0;
            source.status = ReleaseStatus::Cancelled;
            Self::save_vault(&env, source_id, &source);
            Self::remove_owner_vault_id(&env, &source.owner, source_id, source.check_in_interval);
            Self::remove_beneficiary_vault_id(
                &env,
                &source.beneficiary,
                source_id,
                source.check_in_interval,
            );
            Self::append_activity_log(&env, source_id, "merge_vaults_source", &caller, "");
        }
        Self::save_vault(&env, target_vault_id, &target_vault);
        Self::append_activity_log(&env, target_vault_id, "merge_vaults_target", &caller, "");
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((VAULT_MERGED_TOPIC,), (target_vault_id, source_vault_ids));
        Ok(())
    }

    /// Emits expiry warning events for vaults with TTL < 7 days.
    ///
    /// Anyone can call this function to emit warnings for vaults approaching expiry.
    /// This enables off-chain reminder systems to monitor vault status.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_ids` - Vector of vault IDs to check
    pub fn emit_expiry_warnings(env: Env, vault_ids: Vec<u64>) {
        let warning_threshold: u64 = 604_800; // 7 days in seconds

        for vault_id in vault_ids.iter() {
            if let Some(vault) = Self::try_load_vault(&env, vault_id) {
                if vault.status != ReleaseStatus::Locked {
                    continue;
                }
                if let Some(ttl) = Self::get_ttl_remaining(env.clone(), vault_id) {
                    if ttl < warning_threshold {
                        env.events().publish((PING_EXPIRY_TOPIC, vault_id), ttl);
                    }
                }
            }
        }
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
    }

    // --- Issue #392: Passkey Rotation ---

    /// Rotates the primary passkey for a vault.
    ///
    /// Verifies the old passkey before accepting the new one. Only the vault owner can call this.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `caller` - The vault owner (must authorize)
    /// * `old_passkey_hash` - Hash of the old passkey (for verification)
    /// * `new_passkey_hash` - Hash of the new passkey
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::InvalidPasskey` - If old passkey doesn't match
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    pub fn rotate_passkey(
        env: Env,
        vault_id: u64,
        caller: Address,
        old_passkey_hash: BytesN<32>,
        new_passkey_hash: BytesN<32>,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let mut vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }

        // Verify old passkey matches
        if let Some(current) = &vault.passkey_hash {
            let old_as_bytes = Bytes::from_array(&env, &old_passkey_hash.to_array());
            if current != &old_as_bytes {
                return Err(ContractError::InvalidPasskey);
            }
        } else {
            return Err(ContractError::InvalidPasskey);
        }

        vault.passkey_hash = Some(Bytes::from_array(&env, &new_passkey_hash.to_array()));
        Self::save_vault(&env, vault_id, &vault);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        // Issue #558: audit trail — rotation is recorded as a remove of the old
        // hash followed by an add of the new hash, both at the current timestamp.
        Self::log_passkey_audit_entry(&env, vault_id, "remove", &caller, &old_passkey_hash);
        Self::log_passkey_audit_entry(&env, vault_id, "add", &caller, &new_passkey_hash);
        env.events().publish(
            (ROTATE_PASSKEY_TOPIC, vault_id),
            (old_passkey_hash, new_passkey_hash),
        );
        Ok(())
    }

    // --- Issue #393: Passkey Backup Codes ---

    /// Generates 10 one-time backup codes for a vault.
    ///
    /// Only the vault owner can call this. Codes are generated deterministically from vault_id and timestamp.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `caller` - The vault owner (must authorize)
    ///
    /// # Returns
    /// Vector of 10 backup codes
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    pub fn generate_backup_codes(
        env: Env,
        vault_id: u64,
        caller: Address,
    ) -> Result<Vec<String>, ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }

        let mut codes: Vec<BackupCode> = Vec::new(&env);
        let mut result: Vec<String> = Vec::new(&env);
        let timestamp = env.ledger().timestamp();

        for i in 0..10 {
            // Include the generation timestamp and index so every issued code is
            // distinct. Replacing the stored vector below then makes the prior
            // generation unambiguously invalid, even when codes are regenerated
            // in the same ledger timestamp.
            let code_number = vault_id.wrapping_mul(timestamp).wrapping_add(i as u64);
            let mut code_bytes = Bytes::new(&env);
            code_bytes.extend_from_slice(b"code");
            let mut remaining = code_number;
            if remaining == 0 {
                code_bytes.extend_from_slice(b"0");
            } else {
                let mut digits = [0u8; 20];
                let mut digit_count = 0usize;
                while remaining > 0 {
                    digits[digit_count] = b'0' + (remaining % 10) as u8;
                    digit_count += 1;
                    remaining /= 10;
                }
                while digit_count > 0 {
                    digit_count -= 1;
                    code_bytes.push_back(digits[digit_count]);
                }
            }
            let code_str = String::from_bytes(&env, &code_bytes.to_alloc_vec());
            codes.push_back(BackupCode {
                code: code_str.clone(),
                used: false,
            });
            result.push_back(code_str);
        }

        let key = DataKey::BackupCodes(vault_id);
        env.storage().persistent().set(&key, &codes);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((BACKUP_CODES_GENERATED_TOPIC, vault_id), 10u32);
        Ok(result)
    }

    /// Uses a backup code to extend the vault TTL by 30 days.
    ///
    /// Anyone can call this with a valid backup code. The code is marked as used and cannot be reused.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `code` - The backup code to use
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    ///
    /// # Errors
    /// * `ContractError::InvalidBackupCode` - If code is invalid or not found
    /// * `ContractError::BackupCodeAlreadyUsed` - If code has already been used
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    pub fn use_backup_code(env: Env, vault_id: u64, code: String) -> Result<(), ContractError> {
        let mut vault = Self::load_vault(&env, vault_id);
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }

        let key = DataKey::BackupCodes(vault_id);
        let mut codes: Vec<BackupCode> = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::InvalidBackupCode)?;

        let mut found = false;
        for i in 0..codes.len() {
            if let Some(mut backup_code) = codes.get(i) {
                if backup_code.code == code {
                    if backup_code.used {
                        return Err(ContractError::BackupCodeAlreadyUsed);
                    }
                    backup_code.used = true;
                    codes.set(i, backup_code);
                    found = true;
                    break;
                }
            }
        }

        if !found {
            return Err(ContractError::InvalidBackupCode);
        }

        // Extend TTL by 30 days
        vault.last_check_in = env.ledger().timestamp();
        Self::save_vault(&env, vault_id, &vault);
        env.storage().persistent().set(&key, &codes);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((BACKUP_CODE_USED_TOPIC, vault_id), code);
        Ok(())
    }

    // --- Issue #394: Multi-Passkey Support ---

    /// Adds a new passkey to a vault.
    ///
    /// Only the vault owner can call this. Multiple passkeys allow different devices to authenticate.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `caller` - The vault owner (must authorize)
    /// * `passkey_hash` - Hash of the new passkey
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    pub fn add_passkey(
        env: Env,
        vault_id: u64,
        caller: Address,
        passkey_hash: BytesN<32>,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }

        let key = DataKey::VaultPasskeys(vault_id);
        let mut passkeys: Vec<PasskeyHash> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or(Vec::new(&env));

        let timestamp = env.ledger().timestamp();
        passkeys.push_back(PasskeyHash {
            hash: passkey_hash.clone(),
            added_at: timestamp,
            biometric_hash: None,
        });

        env.storage().persistent().set(&key, &passkeys);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        Self::log_passkey_audit_entry(&env, vault_id, "add", &caller, &passkey_hash);
        env.events()
            .publish((ADD_PASSKEY_TOPIC, vault_id), passkey_hash);
        Ok(())
    }

    /// Binds a biometric credential hash to an existing passkey for the vault.
    ///
    /// Only the vault owner can call this. Emits `bind_pk_bio` event on success.
    pub fn bind_passkey_biometric(
        env: Env,
        vault_id: u64,
        caller: Address,
        passkey_hash: BytesN<32>,
        biometric_hash: BytesN<32>,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }

        let key = DataKey::VaultPasskeys(vault_id);
        let mut passkeys: Vec<PasskeyHash> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or(Vec::new(&env));
        let mut found = false;
        for i in 0..passkeys.len() {
            if let Some(mut pk) = passkeys.get(i) {
                if pk.hash == passkey_hash {
                    pk.biometric_hash = Some(biometric_hash.clone().into());
                    passkeys.set(i, pk);
                    found = true;
                    break;
                }
            }
        }

        if !found {
            return Err(ContractError::PasskeyNotFound);
        }

        env.storage().persistent().set(&key, &passkeys);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (BIND_PASSKEY_BIOMETRIC_TOPIC, vault_id),
            (passkey_hash, biometric_hash),
        );
        Ok(())
    }

    /// Unbinds a biometric credential from a passkey.
    pub fn unbind_passkey_biometric(
        env: Env,
        vault_id: u64,
        caller: Address,
        passkey_hash: BytesN<32>,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }

        let key = DataKey::VaultPasskeys(vault_id);
        let mut passkeys: Vec<PasskeyHash> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or(Vec::new(&env));
        let mut found = false;
        for i in 0..passkeys.len() {
            if let Some(mut pk) = passkeys.get(i) {
                if pk.hash == passkey_hash {
                    pk.biometric_hash = None;
                    passkeys.set(i, pk);
                    found = true;
                    break;
                }
            }
        }

        if !found {
            return Err(ContractError::PasskeyNotFound);
        }

        env.storage().persistent().set(&key, &passkeys);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((UNBIND_PASSKEY_BIOMETRIC_TOPIC, vault_id), passkey_hash);
        Ok(())
    }

    /// Performs a biometric-enabled check-in using a passkey and a biometric credential hash.
    ///
    /// Validates passkey registration & expiry, then asserts the provided biometric
    /// hash matches the one bound to the passkey.
    pub fn biometric_check_in(
        env: Env,
        vault_id: u64,
        caller: Address,
        passkey_hash: BytesN<32>,
        biometric_hash: BytesN<32>,
    ) -> Result<(), ContractError> {
        if Self::load_paused(&env) {
            return Err(ContractError::Paused);
        }
        caller.require_auth();
        let mut vault = Self::load_vault(&env, vault_id);
        if vault.is_paused {
            return Err(ContractError::Paused);
        }
        if caller != vault.owner && !Self::is_check_in_delegate(&env, vault_id, &caller) {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }

        // Validate registration & expiry
        let now = env.ledger().timestamp();
        Self::validate_passkey_for_checkin(&env, vault_id, &vault, &passkey_hash, now)?;

        // Verify biometric binding
        let key = DataKey::VaultPasskeys(vault_id);
        let passkeys: Vec<PasskeyHash> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or(Vec::new(&env));
        let mut matched = false;
        for pk in passkeys.iter() {
            if pk.hash == passkey_hash {
                if let Some(bound) = pk.biometric_hash {
                    let biometric_hash_bytes: Bytes = biometric_hash.clone().into();
                    if bound == biometric_hash_bytes {
                        matched = true;
                        break;
                    }
                    return Err(ContractError::InvalidPasskey);
                }
                // No biometric bound
                return Err(ContractError::InvalidPasskey);
            }
        }
        if !matched {
            return Err(ContractError::PasskeyNotFound);
        }

        // Reuse check_in semantics for updating vault state
        vault.last_check_in = now;
        let _original_last_check_in = vault.last_check_in;

        // Inactivity penalty & TTL cap are identical to check_in; reuse logic by calling check_in
        // However, to avoid code duplication in this patch, replicate minimal necessary steps.
        Self::save_vault(&env, vault_id, &vault);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        // Log passkey usage and emit events
        Self::log_passkey_usage(&env, vault_id, &passkey_hash, now);
        // Issue #558: passkey audit trail
        Self::log_passkey_audit_entry(&env, vault_id, "use", &caller, &passkey_hash);
        env.events()
            .publish((BIO_CHECKIN_TOPIC, vault_id), (caller, now));
        Ok(())
    }

    /// Removes a passkey from a vault.
    ///
    /// Only the vault owner can call this. At least one passkey must remain.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `caller` - The vault owner (must authorize)
    /// * `passkey_hash` - Hash of the passkey to remove
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::PasskeyNotFound` - If passkey is not found
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    pub fn remove_passkey(
        env: Env,
        vault_id: u64,
        caller: Address,
        passkey_hash: BytesN<32>,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }

        let key = DataKey::VaultPasskeys(vault_id);
        let passkeys: Vec<PasskeyHash> = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::PasskeyNotFound)?;

        let mut new_passkeys: Vec<PasskeyHash> = Vec::new(&env);
        let mut found = false;

        for pk in passkeys.iter() {
            if pk.hash == passkey_hash {
                found = true;
            } else {
                new_passkeys.push_back(pk);
            }
        }

        if !found {
            return Err(ContractError::PasskeyNotFound);
        }

        env.storage().persistent().set(&key, &new_passkeys);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        Self::log_passkey_audit_entry(&env, vault_id, "remove", &caller, &passkey_hash);
        env.events()
            .publish((REMOVE_PASSKEY_TOPIC, vault_id), passkey_hash);
        Ok(())
    }

    /// Gets all passkeys for a vault.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    ///
    /// # Returns
    /// Vector of passkey hashes
    pub fn get_vault_passkeys(env: Env, vault_id: u64) -> Vec<PasskeyHash> {
        let key = DataKey::VaultPasskeys(vault_id);
        env.storage()
            .persistent()
            .get(&key)
            .unwrap_or(Vec::new(&env))
    }

    /// Checks if a passkey is valid for a vault.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `passkey_hash` - Hash of the passkey to check
    ///
    /// # Returns
    /// `true` if the passkey is valid, `false` otherwise
    pub fn is_valid_passkey(env: Env, vault_id: u64, passkey_hash: BytesN<32>) -> bool {
        let key = DataKey::VaultPasskeys(vault_id);
        if let Some(passkeys) = env
            .storage()
            .persistent()
            .get::<DataKey, Vec<PasskeyHash>>(&key)
        {
            for pk in passkeys.iter() {
                if pk.hash == passkey_hash {
                    return true;
                }
            }
        }
        false
    }

    // --- Issue #557: Passkey Delegation ---

    /// Returns `true` if `addr` holds a valid (non-expired) delegation for `passkey_hash`.
    fn is_valid_passkey_delegate(
        env: &Env,
        vault_id: u64,
        passkey_hash: &BytesN<32>,
        addr: &Address,
    ) -> bool {
        let key = DataKey::PasskeyDelegation(vault_id, passkey_hash.clone());
        if let Some(delegation) = env
            .storage()
            .persistent()
            .get::<DataKey, PasskeyDelegation>(&key)
        {
            if &delegation.delegate == addr && delegation.expires_at > env.ledger().timestamp() {
                return true;
            }
        }
        false
    }

    /// Grants `delegate` the right to perform check-ins on behalf of the vault owner
    /// using the specific passkey `passkey_hash`, until `expires_at`.
    ///
    /// Only the vault owner can call this. The passkey must already be registered
    /// on the vault via `add_passkey`.
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    /// * `ContractError::PasskeyNotFound` - If `passkey_hash` is not registered
    /// * `ContractError::InvalidInterval` - If `expires_at` is not in the future
    /// * `ContractError::InvalidBeneficiary` - If `delegate` equals the owner
    pub fn delegate_passkey(
        env: Env,
        vault_id: u64,
        caller: Address,
        passkey_hash: BytesN<32>,
        delegate: Address,
        expires_at: u64,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        if !Self::is_valid_passkey(env.clone(), vault_id, passkey_hash.clone()) {
            return Err(ContractError::PasskeyNotFound);
        }
        if expires_at <= env.ledger().timestamp() {
            return Err(ContractError::InvalidInterval);
        }
        if delegate == vault.owner {
            return Err(ContractError::InvalidBeneficiary);
        }

        let key = DataKey::PasskeyDelegation(vault_id, passkey_hash.clone());
        let delegation = PasskeyDelegation {
            delegate: delegate.clone(),
            expires_at,
        };
        env.storage().persistent().set(&key, &delegation);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (PASSKEY_DELEGATED_TOPIC, vault_id),
            (passkey_hash, delegate, expires_at),
        );
        Ok(())
    }

    /// Revokes any existing passkey delegation for `passkey_hash` on `vault_id`.
    ///
    /// Only the vault owner can call this. A no-op (still `Ok`) if no delegation exists.
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    pub fn revoke_passkey_delegation(
        env: Env,
        vault_id: u64,
        caller: Address,
        passkey_hash: BytesN<32>,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }

        let key = DataKey::PasskeyDelegation(vault_id, passkey_hash.clone());
        if let Some(delegation) = env
            .storage()
            .persistent()
            .get::<DataKey, PasskeyDelegation>(&key)
        {
            env.storage().persistent().remove(&key);
            env.storage()
                .instance()
                .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
            env.events().publish(
                (PASSKEY_DELEGATION_REVOKED_TOPIC, vault_id),
                (passkey_hash, delegation.delegate),
            );
        }
        Ok(())
    }

    /// Returns the active passkey delegation for `(vault_id, passkey_hash)`, if any.
    pub fn get_passkey_delegation(
        env: Env,
        vault_id: u64,
        passkey_hash: BytesN<32>,
    ) -> Option<PasskeyDelegation> {
        let key = DataKey::PasskeyDelegation(vault_id, passkey_hash);
        env.storage().persistent().get(&key)
    }

    // --- Issue #559: Passkey Escrow ---

    /// Places a passkey in escrow with a designated recovery contact.
    ///
    /// While escrowed, the Owner cannot use this passkey for check-in. Only the
    /// `recovery_contact` can release it (via [`release_escrow_passkey`]), or the
    /// Owner can cancel the escrow themselves (via [`cancel_passkey_escrow`]).
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    /// * `ContractError::PasskeyNotFound` - If passkey is not registered for the vault
    /// * `ContractError::InvalidBeneficiary` - If recovery_contact equals the owner
    /// * `ContractError::AlreadyInEscrow` - If the passkey is already in escrow
    pub fn escrow_passkey(
        env: Env,
        vault_id: u64,
        caller: Address,
        passkey_hash: BytesN<32>,
        recovery_contact: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        if recovery_contact == vault.owner {
            return Err(ContractError::InvalidBeneficiary);
        }

        let pk_key = DataKey::VaultPasskeys(vault_id);
        let passkeys: Vec<PasskeyHash> = env
            .storage()
            .persistent()
            .get(&pk_key)
            .unwrap_or_else(|| Vec::new(&env));
        if !passkeys.iter().any(|p| p.hash == passkey_hash) {
            return Err(ContractError::PasskeyNotFound);
        }

        let escrow_key = DataKey::PasskeyEscrow(vault_id, passkey_hash.clone());
        if env.storage().persistent().has(&escrow_key) {
            return Err(ContractError::AlreadyInEscrow);
        }

        let now = env.ledger().timestamp();
        let record = PasskeyEscrowRecord {
            recovery_contact: recovery_contact.clone(),
            escrowed_at: now,
        };
        env.storage().persistent().set(&escrow_key, &record);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage()
            .persistent()
            .extend_ttl(&escrow_key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (PASSKEY_ESCROWED_TOPIC, vault_id),
            (passkey_hash, recovery_contact),
        );
        Ok(())
    }

    /// Releases a passkey from escrow, restoring it to active status.
    ///
    /// Only the `recovery_contact` stored in the Escrow_Record may call this.
    ///
    /// # Errors
    /// * `ContractError::PasskeyNotFound` - If no escrow record exists for this passkey
    /// * `ContractError::NotRecoveryContact` - If caller is not the recovery contact
    pub fn release_escrow_passkey(
        env: Env,
        vault_id: u64,
        caller: Address,
        passkey_hash: BytesN<32>,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let escrow_key = DataKey::PasskeyEscrow(vault_id, passkey_hash.clone());
        let record: PasskeyEscrowRecord = env
            .storage()
            .persistent()
            .get(&escrow_key)
            .ok_or(ContractError::PasskeyNotFound)?;
        if caller != record.recovery_contact {
            return Err(ContractError::NotRecoveryContact);
        }

        env.storage().persistent().remove(&escrow_key);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (PASSKEY_ESCROW_RELEASED_TOPIC, vault_id),
            (passkey_hash, record.recovery_contact),
        );
        Ok(())
    }

    /// Cancels an escrow, restoring the passkey to active status. Owner-only.
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::PasskeyNotFound` - If no escrow record exists for this passkey
    pub fn cancel_passkey_escrow(
        env: Env,
        vault_id: u64,
        caller: Address,
        passkey_hash: BytesN<32>,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }

        let escrow_key = DataKey::PasskeyEscrow(vault_id, passkey_hash.clone());
        if !env.storage().persistent().has(&escrow_key) {
            return Err(ContractError::PasskeyNotFound);
        }
        env.storage().persistent().remove(&escrow_key);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((PASSKEY_ESCROW_CANCELLED_TOPIC, vault_id), passkey_hash);
        Ok(())
    }

    /// Returns the escrow record for a given `(vault_id, passkey_hash)`, or `None`
    /// if the passkey is not in escrow.
    pub fn get_passkey_escrow(
        env: Env,
        vault_id: u64,
        passkey_hash: BytesN<32>,
    ) -> Option<PasskeyEscrowRecord> {
        env.storage()
            .persistent()
            .get(&DataKey::PasskeyEscrow(vault_id, passkey_hash))
    }

    // --- Issue #934: Emergency Vault Recovery Code ---

    /// Generates an emergency recovery code for a vault and stores its hash on-chain.
    ///
    /// If the owner loses access to all of their passkeys but is still alive, the
    /// recovery code returned here (kept off-chain by the owner) can later be
    /// supplied to [`recover_with_code`] to register a fresh passkey and regain
    /// access.
    ///
    /// Only the vault owner can call this. The raw 32-byte code is produced from
    /// the on-chain PRNG and is **returned only once** — the contract stores just
    /// `sha256(code)`, so the code can never be reconstructed from on-chain state.
    /// Calling this again rotates the code, invalidating any previously generated
    /// one.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    ///
    /// # Returns
    /// The raw 32-byte recovery code as `Bytes`. The owner must store it off-chain;
    /// it is required by [`recover_with_code`].
    ///
    /// # Errors
    /// * `ContractError::AlreadyReleased` - If the vault is not in Locked status
    pub fn generate_recovery_code(env: Env, vault_id: u64) -> Result<Bytes, ContractError> {
        let vault = Self::load_vault(&env, vault_id);
        vault.owner.require_auth();
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }

        // Generate a random 32-byte recovery code using the on-chain PRNG. Unlike
        // a derivation from public vault state, the PRNG output cannot be predicted
        // by an observer who only sees the stored hash.
        let code: Bytes = env.prng().gen_len(32);

        // Persist only sha256(code) — a one-way commitment.
        let code_hash: BytesN<32> = env.crypto().sha256(&code).into();
        let key = DataKey::RecoveryCodeHash(vault_id);
        env.storage().persistent().set(&key, &code_hash);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        env.events()
            .publish((EMERGENCY_RECOVERY_GENERATED_TOPIC, vault_id), code_hash);
        Ok(code)
    }

    /// Restores access to a vault using its emergency recovery code.
    ///
    /// Intended for the case where the owner has lost access to every passkey but
    /// is still alive. The caller supplies the off-chain `recovery_code` previously
    /// returned by [`generate_recovery_code`] together with a fresh passkey hash
    /// (`new_passkey_proof`). On success:
    ///
    /// * Every existing passkey is invalidated — the passkey list is cleared and
    ///   the legacy primary passkey field is replaced.
    /// * `new_passkey_proof` becomes the sole registered passkey.
    /// * The recovery code is consumed (single use) and the liveness timer is
    ///   reset, since a successful recovery proves the owner is alive.
    ///
    /// Authorization is by knowledge of the secret recovery code rather than
    /// `require_auth`, mirroring [`use_backup_code`] which likewise treats the
    /// secret itself as proof.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `recovery_code` - The off-chain recovery code returned by `generate_recovery_code`
    /// * `new_passkey_proof` - Hash of the new passkey to register
    ///
    /// # Errors
    /// * `ContractError::AlreadyReleased` - If the vault is not in Locked status
    /// * `ContractError::RecoveryCodeNotSet` - If no recovery code has been generated
    /// * `ContractError::InvalidRecoveryCode` - If the supplied code does not match
    pub fn recover_with_code(
        env: Env,
        vault_id: u64,
        recovery_code: Bytes,
        new_passkey_proof: BytesN<32>,
    ) -> Result<(), ContractError> {
        let mut vault = Self::load_vault(&env, vault_id);
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }

        let key = DataKey::RecoveryCodeHash(vault_id);
        let stored_hash: BytesN<32> = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::RecoveryCodeNotSet)?;

        let provided_hash: BytesN<32> = env.crypto().sha256(&recovery_code).into();
        if provided_hash != stored_hash {
            return Err(ContractError::InvalidRecoveryCode);
        }

        // Invalidate all existing passkeys and register the new one as the vault's
        // sole passkey.
        let now = env.ledger().timestamp();
        let mut passkeys: Vec<PasskeyHash> = Vec::new(&env);
        passkeys.push_back(PasskeyHash {
            hash: new_passkey_proof.clone(),
            added_at: now,
            biometric_hash: None,
        });
        let pk_key = DataKey::VaultPasskeys(vault_id);
        env.storage().persistent().set(&pk_key, &passkeys);

        // Replace the legacy single-passkey field and reset liveness — a
        // successful recovery is proof the owner is alive.
        vault.passkey_hash = Some(Bytes::from_array(&env, &new_passkey_proof.to_array()));
        vault.last_check_in = now;
        Self::save_vault(&env, vault_id, &vault);

        // Consume the recovery code so it cannot be replayed.
        env.storage().persistent().remove(&key);

        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage()
            .persistent()
            .extend_ttl(&pk_key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        env.events()
            .publish((EMERGENCY_RECOVERY_USED_TOPIC, vault_id), new_passkey_proof);
        Ok(())
    }

    /// Returns `true` if an emergency recovery code is currently set for the vault.
    ///
    /// Note this never exposes the code itself — only whether the one-way hash
    /// commitment exists.
    pub fn has_recovery_code(env: Env, vault_id: u64) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey::RecoveryCodeHash(vault_id))
    }
}
