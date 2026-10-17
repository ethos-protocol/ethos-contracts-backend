#[contractimpl]
impl TtlVaultContract {
    // --- admin/config ---

    /// Initializes the contract with the XLM token address and admin address.
    ///
    /// This function must be called once before any other contract operations.
    /// It sets up the initial configuration and stores the admin address.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `xlm_token` - The address of the XLM token contract
    /// * `admin` - The address of the contract administrator
    ///
    /// # Panics
    /// Panics if the contract has already been initialized
    pub fn initialize(env: Env, xlm_token: Address, admin: Address) {
        if env.storage().instance().has(&DataKey::TokenAddress)
            || env.storage().instance().has(&DataKey::Admin)
        {
            panic_with_error!(&env, ContractError::AlreadyInitialized);
        }
        if xlm_token == admin {
            panic_with_error!(&env, ContractError::InvalidAdmin);
        }
        admin.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::TokenAddress, &xlm_token);
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Paused, &false);
        env.storage()
            .instance()
            .set(&DataKey::Version, &String::from_str(&env, "1.0.0"));
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
    }

    /// Pauses the contract, blocking all state-changing operations (Issue #814).
    ///
    /// Stores a `PauseRecord` with the caller, reason, and timestamp.
    /// Only the admin can call this function.
    ///
    /// # Panics
    /// Panics if the caller is not the admin
    pub fn pause(env: Env, reason: Bytes) {
        Self::require_admin(&env);
        let paused_by = Self::load_admin(&env);
        let record = PauseRecord {
            paused_by: paused_by.clone(),
            reason: reason.clone(),
            paused_at: env.ledger().timestamp(),
        };
        env.storage().instance().set(&DataKey::Paused, &true);
        env.storage().instance().set(&DataKey::PauseRecord, &record);
        env.events()
            .publish((PAUSE_TOPIC,), (true, paused_by, reason));
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
    }

    /// Returns the pause record if the contract is currently paused (Issue #814).
    pub fn get_pause_record(env: Env) -> Option<PauseRecord> {
        env.storage().instance().get(&DataKey::PauseRecord)
    }

    /// Unpauses the contract, allowing all operations to resume.
    ///
    /// Clears the stored pause record.
    /// Only the admin can call this function.
    ///
    /// # Panics
    /// Panics if the caller is not the admin
    pub fn unpause(env: Env) {
        Self::require_admin(&env);
        env.storage().instance().set(&DataKey::Paused, &false);
        env.storage().instance().remove(&DataKey::PauseRecord);
        env.events().publish((UNPAUSE_TOPIC,), false);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
    }

    /// Sets the minimum allowed check-in interval for vaults.
    ///
    /// This constraint applies to both new vaults and interval updates.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `min_interval` - Minimum interval in seconds (must be > 0)
    ///
    /// # Panics
    /// * Panics if the caller is not the admin
    /// * Panics if `min_interval` is 0
    pub fn set_min_check_in_interval(env: Env, min_interval: u64) {
        Self::require_admin(&env);
        if min_interval == 0 {
            panic_with_error!(&env, ContractError::InvalidConfig);
        }
        if let Some(max) = env
            .storage()
            .instance()
            .get::<DataKey, u64>(&DataKey::MaxCheckInInterval)
        {
            if min_interval > max {
                panic_with_error!(&env, ContractError::InvalidConfig);
            }
        }
        env.storage()
            .instance()
            .set(&DataKey::MinCheckInInterval, &min_interval);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((SET_MIN_INTERVAL_TOPIC,), min_interval);
    }

    /// Sets the maximum allowed check-in interval for vaults.
    ///
    /// This constraint applies to both new vaults and interval updates.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `max_interval` - Maximum interval in seconds (must be > 0)
    ///
    /// # Panics
    /// * Panics if the caller is not the admin
    /// * Panics if `max_interval` is 0
    pub fn set_max_check_in_interval(env: Env, max_interval: u64) {
        Self::require_admin(&env);
        if max_interval == 0 {
            panic_with_error!(&env, ContractError::InvalidConfig);
        }
        if let Some(min) = env
            .storage()
            .instance()
            .get::<DataKey, u64>(&DataKey::MinCheckInInterval)
        {
            if max_interval < min {
                panic_with_error!(&env, ContractError::InvalidConfig);
            }
        }
        env.storage()
            .instance()
            .set(&DataKey::MaxCheckInInterval, &max_interval);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((SET_MAX_INTERVAL_TOPIC,), max_interval);
    }

    /// Returns the minimum check-in interval if set.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    ///
    /// # Returns
    /// `Some(seconds)` with the minimum interval, or `None` if not set
    pub fn get_min_check_in_interval(env: Env) -> Option<u64> {
        env.storage().instance().get(&DataKey::MinCheckInInterval)
    }

    /// Returns the maximum check-in interval if set.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    ///
    /// # Returns
    /// `Some(seconds)` with the maximum interval, or `None` if not set
    pub fn get_max_check_in_interval(env: Env) -> Option<u64> {
        env.storage().instance().get(&DataKey::MaxCheckInInterval)
    }

    /// Sets the maximum TTL (time-to-live) for vaults in seconds.
    ///
    /// This prevents vaults from becoming permanent by capping the maximum
    /// TTL that can be set during check-in. Default is 10 years (315,360,000 seconds).
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `max_ttl` - Maximum TTL in seconds (must be > 0)
    ///
    /// # Panics
    /// * Panics if the caller is not the admin
    /// * Panics if `max_ttl` is 0
    pub fn set_max_ttl_seconds(env: Env, max_ttl: u64) {
        Self::require_admin(&env);
        if max_ttl == 0 {
            panic_with_error!(&env, ContractError::InvalidInterval);
        }
        env.storage()
            .instance()
            .set(&DataKey::MaxTtlSeconds, &max_ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish((SET_MAX_TTL_TOPIC,), max_ttl);
    }

    /// Returns the maximum TTL in seconds.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    ///
    /// # Returns
    /// The maximum TTL in seconds, or 10 years if not set
    pub fn get_max_ttl_seconds(env: Env) -> u64 {
        // Default: 10 years in seconds
        env.storage()
            .instance()
            .get(&DataKey::MaxTtlSeconds)
            .unwrap_or(315_360_000)
    }

    /// Sets the TTL decay rate as a percentage per month.
    ///
    /// If check-ins become infrequent (no check-in for 30 days), the TTL is reduced
    /// by this rate. For example, 100 = 1% per month.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `decay_rate` - Decay rate in basis points (1-10000, where 100 = 1%)
    ///
    /// # Panics
    /// * Panics if the caller is not the admin
    /// * Panics if `decay_rate` is 0 or > 10000
    pub fn set_ttl_decay_rate(env: Env, decay_rate: u32) {
        Self::require_admin(&env);
        if decay_rate == 0 || decay_rate > 10_000 {
            panic_with_error!(&env, ContractError::InvalidBps);
        }
        env.storage()
            .instance()
            .set(&DataKey::TtlDecayRate, &decay_rate);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish((SET_DECAY_RATE_TOPIC,), decay_rate);
    }

    /// Returns the TTL decay rate in basis points.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    ///
    /// # Returns
    /// The decay rate in basis points (0 if not set, meaning no decay)
    pub fn get_ttl_decay_rate(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::TtlDecayRate)
            .unwrap_or(0)
    }

    /// Adds a token to the whitelist, allowing it to be used in vaults.
    ///
    /// # Arguments
    /// * `token_address` - The token contract address to whitelist
    ///
    /// # Panics
    /// * Panics if the caller is not the admin
    pub fn whitelist_token(env: Env, token_address: Address) {
        Self::require_admin(&env);
        let key = DataKey::TokenWhitelist(token_address.clone());
        env.storage().persistent().set(&key, &true);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, VAULT_TTL_LEDGERS);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
    }

    /// Removes a token from the whitelist.
    ///
    /// # Arguments
    /// * `token_address` - The token contract address to remove
    ///
    /// # Panics
    /// * Panics if the caller is not the admin
    pub fn remove_token_whitelist(env: Env, token_address: Address) {
        Self::require_admin(&env);
        let key = DataKey::TokenWhitelist(token_address);
        env.storage().persistent().remove(&key);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
    }

    /// Gets the canonical token address for a registered wrapped token.
    pub fn get_wrapped_token(env: Env, wrapped_token_address: Address) -> Option<Address> {
        let key = DataKey::WrappedToken(wrapped_token_address);
        env.storage().persistent().get(&key)
    }

    /// Resolves a token to its canonical equivalent if it is a wrapped token.
    pub fn resolve_canonical_token(env: Env, token_address: Address) -> Address {
        Self::get_wrapped_token(env.clone(), token_address.clone()).unwrap_or(token_address)
    }

    /// Checks if a token is whitelisted.
    ///
    /// # Arguments
    /// * `token_address` - The token contract address to check
    ///
    /// # Returns
    /// `true` if the token is whitelisted, is the default XLM token, or is a registered wrapped token whose canonical token is whitelisted.
    pub fn is_token_whitelisted(env: Env, token_address: Address) -> bool {
        let default_token = Self::load_token(&env);
        if token_address == default_token {
            return true;
        }

        let key = DataKey::TokenWhitelist(token_address.clone());
        if env
            .storage()
            .persistent()
            .get::<DataKey, bool>(&key)
            .unwrap_or(false)
        {
            return true;
        }

        if let Some(canonical_token) = Self::get_wrapped_token(env.clone(), token_address.clone()) {
            if canonical_token == default_token {
                return true;
            }

            let canonical_key = DataKey::TokenWhitelist(canonical_token);
            env.storage()
                .persistent()
                .get::<DataKey, bool>(&canonical_key)
                .unwrap_or(false)
        } else {
            false
        }
    }

    /// Registers a wrapped token for cross-chain compatibility.
    ///
    /// Wrapped tokens are accepted as whitelisted if their canonical token is whitelisted.
    pub fn register_wrapped_token(
        env: Env,
        wrapped_token_address: Address,
        canonical_token_address: Address,
    ) {
        Self::require_admin(&env);
        if wrapped_token_address == canonical_token_address {
            panic_with_error!(&env, ContractError::InvalidAmount);
        }

        Self::assert_token_whitelisted(&env, &canonical_token_address);

        let key = DataKey::WrappedToken(wrapped_token_address.clone());
        env.storage()
            .persistent()
            .set(&key, &canonical_token_address);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, VAULT_TTL_LEDGERS);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (WRAPPED_TOKEN_REGISTERED_TOPIC,),
            (&wrapped_token_address, &canonical_token_address),
        );
    }

    /// Unregisters a wrapped token mapping.
    pub fn unregister_wrapped_token(env: Env, wrapped_token_address: Address) {
        Self::require_admin(&env);
        let key = DataKey::WrappedToken(wrapped_token_address.clone());
        env.storage().persistent().remove(&key);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((WRAPPED_TOKEN_UNREGISTERED_TOPIC,), wrapped_token_address);
    }

    // --- Cross-Chain Bridge Support (Issue #366) ---

    /// Registers a bridge for cross-chain support.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `chain_id` - The target chain ID
    /// * `bridge_address` - The bridge contract address
    ///
    /// # Panics
    /// * Panics if the caller is not the admin
    pub fn register_bridge(env: Env, chain_id: u32, bridge_address: Address) {
        Self::require_admin(&env);
        let config = BridgeConfig {
            chain_id,
            bridge_address: bridge_address.clone(),
            is_active: true,
        };
        let key = DataKey::BridgeConfig(chain_id);
        env.storage().persistent().set(&key, &config);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, VAULT_TTL_LEDGERS);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((symbol_short!("br_reg"),), (chain_id, bridge_address));
    }

    /// Deactivates a bridge.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `chain_id` - The chain ID to deactivate
    ///
    /// # Panics
    /// * Panics if the caller is not the admin
    pub fn deactivate_bridge(env: Env, chain_id: u32) {
        Self::require_admin(&env);
        let key = DataKey::BridgeConfig(chain_id);
        if let Some(mut config) = env
            .storage()
            .persistent()
            .get::<DataKey, BridgeConfig>(&key)
        {
            config.is_active = false;
            env.storage().persistent().set(&key, &config);
            env.storage()
                .persistent()
                .extend_ttl(&key, VAULT_TTL_THRESHOLD, VAULT_TTL_LEDGERS);
        }
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish((symbol_short!("br_deact"),), chain_id);
    }

    /// Gets the bridge configuration for a chain.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `chain_id` - The chain ID
    ///
    /// # Returns
    /// The bridge configuration if it exists
    pub fn get_bridge_config(env: Env, chain_id: u32) -> Option<BridgeConfig> {
        let key = DataKey::BridgeConfig(chain_id);
        env.storage().persistent().get(&key)
    }

    /// Checks if a bridge is active.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `chain_id` - The chain ID
    ///
    /// # Returns
    /// `true` if the bridge is active, `false` otherwise
    pub fn is_bridge_active(env: Env, chain_id: u32) -> bool {
        if let Some(config) = Self::get_bridge_config(env, chain_id) {
            config.is_active
        } else {
            false
        }
    }

    /// Validates that a new WASM hash is safe to upgrade to.
    ///
    /// Checks:
    /// - The new hash is not the zero hash (i.e., not a null/empty deployment)
    ///
    /// # Errors
    /// - `UpgradeInvalidHash` if the hash is all-zeros
    pub fn validate_upgrade(env: Env, new_wasm_hash: BytesN<32>) {
        let zero: BytesN<32> = BytesN::from_array(&env, &[0u8; 32]);
        if new_wasm_hash == zero {
            panic_with_error!(&env, ContractError::UpgradeInvalidHash);
        }
    }

    /// Admin-only. Records the interface/storage fingerprint of the
    /// currently running contract. This must be called once after
    /// `initialize` (or after each successful `upgrade_with_manifest`) so
    /// the *next* upgrade has a baseline to validate against.
    ///
    /// See `UpgradeManifest` in `types.rs` and `docs/upgrade-safety.md` for
    /// how `exported_fn_count`, `error_code_count`, and
    /// `storage_schema_hash` should be computed out-of-band before calling
    /// this.
    pub fn set_upgrade_manifest(
        env: Env,
        exported_fn_count: u32,
        error_code_count: u32,
        storage_schema_hash: BytesN<32>,
    ) {
        Self::require_admin(&env);

        let version = env
            .storage()
            .instance()
            .get::<DataKey, UpgradeManifest>(&DataKey::UpgradeManifest)
            .map(|m| m.version + 1)
            .unwrap_or(1);

        let manifest = UpgradeManifest {
            exported_fn_count,
            error_code_count,
            storage_schema_hash,
            version,
        };
        env.storage()
            .instance()
            .set(&DataKey::UpgradeManifest, &manifest);
    }

    /// Admin-only. Records the function-signature fingerprint alongside the
    /// legacy upgrade manifest. The fingerprint is computed from the sorted
    /// public function names, argument types, and return types in the WASM spec.
    pub fn set_up_manifest_sigs(
        env: Env,
        exported_fn_count: u32,
        error_code_count: u32,
        storage_schema_hash: BytesN<32>,
        function_signatures_hash: BytesN<32>,
    ) {
        Self::set_upgrade_manifest(
            env.clone(),
            exported_fn_count,
            error_code_count,
            storage_schema_hash,
        );
        env.storage().instance().set(
            &DataKey::UpgradeFunctionSignaturesHash,
            &function_signatures_hash,
        );
    }

    /// Returns the currently recorded upgrade manifest, if any.
    pub fn get_upgrade_manifest(env: Env) -> Option<UpgradeManifest> {
        env.storage().instance().get(&DataKey::UpgradeManifest)
    }

    /// Returns the recorded public function-signature fingerprint, if set.
    pub fn get_upgrade_sig_hash(env: Env) -> Option<BytesN<32>> {
        env.storage()
            .instance()
            .get(&DataKey::UpgradeFunctionSignaturesHash)
    }

    /// Validates that a proposed upgrade is backward-compatible with the
    /// manifest recorded via `set_upgrade_manifest`, in addition to the
    /// basic hash check in `validate_upgrade`.
    ///
    /// Checks (see `docs/upgrade-safety.md` for rationale and how callers
    /// are expected to compute the new-contract values off-chain before
    /// submitting the upgrade transaction):
    /// - Same exported function count (interface not shrunk)
    /// - Same or greater error code count (error codes not removed/renumbered)
    /// - Same storage schema hash (no storage keys dropped or repurposed)
    ///
    /// # Errors
    /// - `UpgradeManifestNotSet` if no baseline has been recorded yet
    /// - `UpgradeInterfaceShrunk` if the new contract exports fewer functions
    /// - `UpgradeErrorCodesReduced` if the new contract has fewer error codes
    /// - `UpgradeStorageSchemaChanged` if the storage schema hash differs
    pub fn validate_upgrade_compatibility(
        env: Env,
        new_exported_fn_count: u32,
        new_error_code_count: u32,
        new_storage_schema_hash: BytesN<32>,
    ) {
        let manifest: UpgradeManifest = env
            .storage()
            .instance()
            .get(&DataKey::UpgradeManifest)
            .unwrap_or_else(|| panic_with_error!(&env, ContractError::UpgradeManifestNotSet));

        if new_exported_fn_count < manifest.exported_fn_count {
            panic_with_error!(&env, ContractError::UpgradeInterfaceShrunk);
        }
        if new_error_code_count < manifest.error_code_count {
            panic_with_error!(&env, ContractError::UpgradeErrorCodesReduced);
        }
        if new_storage_schema_hash != manifest.storage_schema_hash {
            panic_with_error!(&env, ContractError::UpgradeStorageSchemaChanged);
        }
    }

    /// Validates the legacy manifest and requires an identical public function
    /// signature fingerprint for the proposed contract.
    pub fn validate_upgrade_sig_compat(
        env: Env,
        new_exported_fn_count: u32,
        new_error_code_count: u32,
        new_storage_schema_hash: BytesN<32>,
        new_function_signatures_hash: BytesN<32>,
    ) {
        Self::validate_upgrade_compatibility(
            env.clone(),
            new_exported_fn_count,
            new_error_code_count,
            new_storage_schema_hash,
        );
        let current_function_signatures_hash: BytesN<32> = env
            .storage()
            .instance()
            .get(&DataKey::UpgradeFunctionSignaturesHash)
            .unwrap_or_else(|| {
                panic_with_error!(&env, ContractError::UpgradeFunctionSignaturesNotSet)
            });

        if new_function_signatures_hash != current_function_signatures_hash {
            panic_with_error!(&env, ContractError::UpgradeFunctionSignaturesChanged);
        }
    }

    /// Admin-only. Validates and upgrades the contract to a new WASM hash.
    ///
    /// This only performs the basic non-zero-hash check. Prefer
    /// `upgrade_with_manifest` for upgrades where a baseline has been
    /// recorded via `set_upgrade_manifest`, since it additionally enforces
    /// interface, storage, and error-code compatibility.
    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) {
        Self::require_admin(&env);
        Self::validate_upgrade(env.clone(), new_wasm_hash.clone());
        env.deployer().update_current_contract_wasm(new_wasm_hash);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.storage()
            .instance()
            .remove(&DataKey::UpgradeFunctionSignaturesHash);
    }

    /// Admin-only. Full upgrade-safety path: validates the hash, validates
    /// interface/storage/error-code compatibility against the recorded
    /// manifest, performs the upgrade, and then bumps the manifest version
    /// so the new contract becomes the baseline for the *next* upgrade.
    ///
    /// See `docs/upgrade-safety.md` for the recommended admin workflow.
    pub fn upgrade_with_manifest(
        env: Env,
        new_wasm_hash: BytesN<32>,
        new_exported_fn_count: u32,
        new_error_code_count: u32,
        new_storage_schema_hash: BytesN<32>,
    ) {
        Self::require_admin(&env);
        Self::validate_upgrade(env.clone(), new_wasm_hash.clone());
        Self::validate_upgrade_compatibility(
            env.clone(),
            new_exported_fn_count,
            new_error_code_count,
            new_storage_schema_hash.clone(),
        );

        env.deployer().update_current_contract_wasm(new_wasm_hash);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.storage()
            .instance()
            .remove(&DataKey::UpgradeFunctionSignaturesHash);

        Self::set_upgrade_manifest(
            env,
            new_exported_fn_count,
            new_error_code_count,
            new_storage_schema_hash,
        );
    }

    /// Admin-only. Performs an upgrade after validating the full manifest,
    /// including an exact public function-signature fingerprint.
    pub fn upgrade_manifest_with_sigs(
        env: Env,
        new_wasm_hash: BytesN<32>,
        new_exported_fn_count: u32,
        new_error_code_count: u32,
        new_storage_schema_hash: BytesN<32>,
        new_function_signatures_hash: BytesN<32>,
    ) {
        Self::require_admin(&env);
        Self::validate_upgrade(env.clone(), new_wasm_hash.clone());
        Self::validate_upgrade_sig_compat(
            env.clone(),
            new_exported_fn_count,
            new_error_code_count,
            new_storage_schema_hash.clone(),
            new_function_signatures_hash.clone(),
        );

        env.deployer().update_current_contract_wasm(new_wasm_hash);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        Self::set_up_manifest_sigs(
            env,
            new_exported_fn_count,
            new_error_code_count,
            new_storage_schema_hash,
            new_function_signatures_hash,
        );
    }

    // --- Issue #581: Token Conversion ---

    /// Enables token conversion for a vault.
    /// Allows converting vault tokens to different tokens before release.
    ///
    /// # Arguments
    /// * `vault_id` - The vault ID
    /// * `from_token` - The source token address
    /// * `to_token` - The target token address
    /// * `conversion_rate` - Conversion rate in basis points (10000 = 1:1)
    ///
    /// # Panics
    /// * Panics if caller is not the vault owner
    /// * Panics if target token is not whitelisted
    pub fn enable_token_conversion(
        env: Env,
        vault_id: u64,
        from_token: Address,
        to_token: Address,
        conversion_rate: i128,
    ) {
        let vault = Self::load_vault(&env, vault_id);
        vault.owner.require_auth();

        // Validate tokens are whitelisted
        Self::assert_token_whitelisted(&env, &from_token);
        Self::assert_token_whitelisted(&env, &to_token);

        let conversion = TokenConversion {
            vault_id,
            from_token: from_token.clone(),
            to_token: to_token.clone(),
            conversion_rate,
            enabled: true,
            created_at: env.ledger().timestamp(),
        };

        let key = DataKey::TokenConversion(vault_id);
        env.storage().persistent().set(&key, &conversion);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, VAULT_TTL_LEDGERS);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        env.events().publish(
            (TOKEN_CONVERSION_TOPIC, vault_id),
            (&from_token, &to_token, conversion_rate),
        );
    }

    /// Gets the token conversion configuration for a vault.
    pub fn get_token_conversion(env: Env, vault_id: u64) -> Option<TokenConversion> {
        let key = DataKey::TokenConversion(vault_id);
        env.storage().persistent().get(&key)
    }

    // --- Issue #583: Token Staking ---

    /// Enables token staking for a vault.
    /// Allows vault tokens to be staked for yield while locked.
    ///
    /// # Arguments
    /// * `vault_id` - The vault ID
    /// * `staking_pool` - The staking pool contract address
    /// * `annual_yield_bps` - Annual yield in basis points (e.g., 500 = 5%)
    ///
    /// # Panics
    /// * Panics if caller is not the vault owner
    pub fn enable_token_staking(
        env: Env,
        vault_id: u64,
        staking_pool: Address,
        annual_yield_bps: u32,
    ) {
        let vault = Self::load_vault(&env, vault_id);
        vault.owner.require_auth();

        let staking = TokenStaking {
            vault_id,
            staking_pool: staking_pool.clone(),
            staked_amount: 0,
            staking_start: env.ledger().timestamp(),
            annual_yield_bps,
            is_active: true,
        };

        let key = DataKey::TokenStaking(vault_id);
        env.storage().persistent().set(&key, &staking);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, VAULT_TTL_LEDGERS);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        env.events().publish(
            (TOKEN_STAKING_TOPIC, vault_id),
            (&staking_pool, annual_yield_bps),
        );
    }

    /// Disables token staking for a vault.
    pub fn disable_token_staking(env: Env, vault_id: u64) {
        let vault = Self::load_vault(&env, vault_id);
        vault.owner.require_auth();

        let key = DataKey::TokenStaking(vault_id);
        if let Some(mut staking) = env
            .storage()
            .persistent()
            .get::<DataKey, TokenStaking>(&key)
        {
            staking.is_active = false;
            env.storage().persistent().set(&key, &staking);
            env.storage()
                .persistent()
                .extend_ttl(&key, VAULT_TTL_THRESHOLD, VAULT_TTL_LEDGERS);
        }
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        env.events()
            .publish((TOKEN_UNSTAKING_TOPIC, vault_id), vault_id);
    }

    /// Gets the token staking configuration for a vault.
    pub fn get_token_staking(env: Env, vault_id: u64) -> Option<TokenStaking> {
        let key = DataKey::TokenStaking(vault_id);
        env.storage().persistent().get(&key)
    }

    // --- Issue #584: Token Yield Distribution ---

    /// Configures yield distribution for a vault.
    /// Determines how staking yield is distributed or reinvested.
    ///
    /// # Arguments
    /// * `vault_id` - The vault ID
    /// * `mode` - The yield distribution mode
    ///
    /// # Panics
    /// * Panics if caller is not the vault owner
    pub fn set_yield_distribution(env: Env, vault_id: u64, mode: YieldDistributionMode) {
        let vault = Self::load_vault(&env, vault_id);
        vault.owner.require_auth();

        let config = YieldDistributionConfig {
            vault_id,
            mode: mode.clone(),
            last_distribution: env.ledger().timestamp(),
            total_distributed: 0,
            total_reinvested: 0,
        };

        let key = DataKey::YieldDistributionConfig(vault_id);
        env.storage().persistent().set(&key, &config);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, VAULT_TTL_LEDGERS);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        env.events()
            .publish((YIELD_DISTRIBUTED_TOPIC, vault_id), vault_id);
    }

    /// Gets the yield distribution configuration for a vault.
    pub fn get_yield_distribution(env: Env, vault_id: u64) -> Option<YieldDistributionConfig> {
        let key = DataKey::YieldDistributionConfig(vault_id);
        env.storage().persistent().get(&key)
    }

    /// Distributes accumulated yield to beneficiary or reinvests it.
    /// This function calculates yield based on staking configuration and distributes
    /// according to the configured mode.
    ///
    /// # Arguments
    /// * `vault_id` - The vault ID
    ///
    /// # Panics
    /// * Panics if vault has no staking or yield distribution configured
    pub fn distribute_yield(env: Env, vault_id: u64) {
        let mut vault = Self::load_vault(&env, vault_id);

        let staking_key = DataKey::TokenStaking(vault_id);
        let staking = env
            .storage()
            .persistent()
            .get::<DataKey, TokenStaking>(&staking_key)
            .unwrap_or_else(|| panic_with_error!(&env, ContractError::InvalidAmount));

        let yield_key = DataKey::YieldDistributionConfig(vault_id);
        let mut yield_config = env
            .storage()
            .persistent()
            .get::<DataKey, YieldDistributionConfig>(&yield_key)
            .unwrap_or_else(|| panic_with_error!(&env, ContractError::InvalidAmount));

        if !staking.is_active {
            panic_with_error!(&env, ContractError::InvalidAmount);
        }

        // Calculate yield: (staked_amount * annual_yield_bps * time_elapsed) / (10000 * 365 * 86400)
        let now = env.ledger().timestamp();
        let time_elapsed = now.saturating_sub(staking.staking_start);
        let yield_amount = (staking.staked_amount as i128)
            .checked_mul(staking.annual_yield_bps as i128)
            .and_then(|v| v.checked_mul(time_elapsed as i128))
            .and_then(|v| v.checked_div(10000 * 365 * 86400))
            .unwrap_or(0);

        if yield_amount <= 0 {
            return;
        }

        match &yield_config.mode {
            YieldDistributionMode::DistributeToBeneficiary => {
                // Transfer yield to beneficiary
                let token_client = token::Client::new(&env, &vault.token_address);
                crate::aml::require_compliant(&env, &vault.beneficiary);
                token_client.transfer(
                    &env.current_contract_address(),
                    &vault.beneficiary,
                    &yield_amount,
                );
                yield_config.total_distributed = yield_config
                    .total_distributed
                    .checked_add(yield_amount)
                    .unwrap_or(yield_config.total_distributed);
                env.events()
                    .publish((YIELD_DISTRIBUTED_TOPIC, vault_id), yield_amount);
            }
            YieldDistributionMode::Reinvest => {
                // Reinvest yield back into vault
                vault.balance = vault
                    .balance
                    .checked_add(yield_amount)
                    .unwrap_or(vault.balance);
                yield_config.total_reinvested = yield_config
                    .total_reinvested
                    .checked_add(yield_amount)
                    .unwrap_or(yield_config.total_reinvested);
                env.events()
                    .publish((YIELD_REINVESTED_TOPIC, vault_id), yield_amount);
            }
            YieldDistributionMode::Split(beneficiary_bps) => {
                // Split yield between beneficiary and reinvestment
                let beneficiary_amount = (yield_amount as i128)
                    .checked_mul(*beneficiary_bps as i128)
                    .and_then(|v| v.checked_div(10000))
                    .unwrap_or(0);
                let reinvest_amount = yield_amount.saturating_sub(beneficiary_amount);

                if beneficiary_amount > 0 {
                    let token_client = token::Client::new(&env, &vault.token_address);
                    crate::aml::require_compliant(&env, &vault.beneficiary);
                    token_client.transfer(
                        &env.current_contract_address(),
                        &vault.beneficiary,
                        &beneficiary_amount,
                    );
                    yield_config.total_distributed = yield_config
                        .total_distributed
                        .checked_add(beneficiary_amount)
                        .unwrap_or(yield_config.total_distributed);
                    env.events()
                        .publish((YIELD_DISTRIBUTED_TOPIC, vault_id), beneficiary_amount);
                }

                if reinvest_amount > 0 {
                    vault.balance = vault
                        .balance
                        .checked_add(reinvest_amount)
                        .unwrap_or(vault.balance);
                    yield_config.total_reinvested = yield_config
                        .total_reinvested
                        .checked_add(reinvest_amount)
                        .unwrap_or(yield_config.total_reinvested);
                    env.events()
                        .publish((YIELD_REINVESTED_TOPIC, vault_id), reinvest_amount);
                }
            }
        }

        yield_config.last_distribution = now;
        env.storage().persistent().set(&yield_key, &yield_config);
        env.storage()
            .persistent()
            .extend_ttl(&yield_key, VAULT_TTL_THRESHOLD, VAULT_TTL_LEDGERS);
        Self::save_vault(&env, vault_id, &vault);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
    }

    /// Returns whether the contract is currently paused.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    ///
    /// # Returns
    /// `true` if the contract is paused, `false` otherwise
    pub fn is_paused(env: Env) -> bool {
        Self::load_paused(&env)
    }

    /// Returns the current admin address.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    ///
    /// # Returns
    /// The admin address
    ///
    /// # Panics
    /// Panics if the contract is not initialized
    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, ContractError::NotInitialized))
    }

    // --- AML screening (Issue #547) ---

    /// Returns `true` when `address` passes AML screening: it is not on the
    /// local flag list and, if a sanctions oracle is configured, the oracle
    /// does not report it as sanctioned (oracle failures fail closed).
    pub fn check_aml_compliance(env: Env, address: Address) -> bool {
        aml::check_compliance(&env, &address)
    }

    /// Admin-only: configure (or clear with `None`) the on-chain sanctions
    /// oracle, which must expose `is_sanctioned(Address) -> bool`.
    pub fn set_aml_oracle(env: Env, admin: Address, oracle: Option<Address>) {
        aml::set_oracle(&env, &admin, oracle);
    }

    /// Returns the configured sanctions oracle, if any.
    pub fn get_aml_oracle(env: Env) -> Option<Address> {
        aml::get_oracle(&env)
    }

    /// Admin-only: configure (or clear) the AML reporter — typically the
    /// backend service that screens addresses with an AML provider and
    /// mirrors hits on-chain.
    pub fn set_aml_reporter(env: Env, admin: Address, reporter: Option<Address>) {
        aml::set_reporter(&env, &admin, reporter);
    }

    /// Returns the configured AML reporter, if any.
    pub fn get_aml_reporter(env: Env) -> Option<Address> {
        aml::get_reporter(&env)
    }

    /// Admin or AML reporter: flag `address`, blocking it from being added as
    /// a beneficiary and from receiving any transfer out of a vault.
    pub fn flag_aml_address(env: Env, caller: Address, address: Address, reason: String) {
        aml::flag_address(&env, &caller, address, reason);
    }

    /// Admin or AML reporter: remove a flag (e.g. after a false positive is
    /// cleared by compliance review).
    pub fn unflag_aml_address(env: Env, caller: Address, address: Address) {
        aml::unflag_address(&env, &caller, address);
    }

    /// Returns the flag record for `address`, if it is locally flagged.
    pub fn get_aml_flag(env: Env, address: Address) -> Option<aml::AmlFlag> {
        aml::get_flag(&env, &address)
    }

    /// Returns the current protocol-level configuration as a typed struct — Issue #810.
    pub fn get_protocol_config(env: Env) -> ProtocolConfig {
        ProtocolConfig {
            min_check_in_interval: env.storage().instance().get(&DataKey::MinCheckInInterval),
            max_check_in_interval: env.storage().instance().get(&DataKey::MaxCheckInInterval),
            max_ttl_seconds: env
                .storage()
                .instance()
                .get(&DataKey::MaxTtlSeconds)
                .unwrap_or(315_360_000),
            ttl_decay_rate: env
                .storage()
                .instance()
                .get(&DataKey::TtlDecayRate)
                .unwrap_or(0),
            require_utf8_metadata: env
                .storage()
                .instance()
                .get(&DataKey::RequireUtf8Metadata)
                .unwrap_or(false),
            max_vaults_per_owner: env
                .storage()
                .instance()
                .get(&DataKey::MaxVaultsPerOwner)
                .unwrap_or(0),
        }
    }

    /// Proposes a new protocol configuration subject to a 24-hour timelock — Issue #809.
    /// Admin-only. Stores the pending config; call `apply_protocol_config` after the delay.
    pub fn propose_protocol_config(env: Env, config: ProtocolConfig) {
        Self::require_admin(&env);
        if let Some(min) = config.min_check_in_interval {
            if min == 0 {
                panic_with_error!(&env, ContractError::InvalidInterval);
            }
        }
        if let Some(max) = config.max_check_in_interval {
            if max == 0 {
                panic_with_error!(&env, ContractError::InvalidInterval);
            }
        }
        if let (Some(min), Some(max)) = (config.min_check_in_interval, config.max_check_in_interval)
        {
            if min > max {
                panic_with_error!(&env, ContractError::InvalidInterval);
            }
        }
        if config.max_ttl_seconds == 0 {
            panic_with_error!(&env, ContractError::InvalidInterval);
        }
        if config.ttl_decay_rate > 10_000 {
            panic_with_error!(&env, ContractError::InvalidBps);
        }
        let now = env.ledger().timestamp();
        env.storage()
            .instance()
            .set(&DataKey::PendingProtocolConfig, &config);
        env.storage()
            .instance()
            .set(&DataKey::ProtocolConfigProposedAt, &now);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish((PROTOCOL_CONFIG_PROPOSED_TOPIC,), now);
    }

    /// Applies the pending protocol configuration after the 24-hour timelock — Issue #809.
    /// Admin-only.
    pub fn apply_protocol_config(env: Env) {
        Self::require_admin(&env);
        let config: ProtocolConfig = env
            .storage()
            .instance()
            .get(&DataKey::PendingProtocolConfig)
            .unwrap_or_else(|| panic_with_error!(&env, ContractError::NoPendingProtocolConfig));
        let proposed_at: u64 = env
            .storage()
            .instance()
            .get(&DataKey::ProtocolConfigProposedAt)
            .unwrap_or_else(|| panic_with_error!(&env, ContractError::NoPendingProtocolConfig));
        let now = env.ledger().timestamp();
        if now < proposed_at + PROTOCOL_CONFIG_TIMELOCK {
            panic_with_error!(&env, ContractError::ProtocolConfigTimeLocked);
        }
        if let Some(min) = config.min_check_in_interval {
            env.storage()
                .instance()
                .set(&DataKey::MinCheckInInterval, &min);
        } else {
            env.storage()
                .instance()
                .remove(&DataKey::MinCheckInInterval);
        }
        if let Some(max) = config.max_check_in_interval {
            env.storage()
                .instance()
                .set(&DataKey::MaxCheckInInterval, &max);
        } else {
            env.storage()
                .instance()
                .remove(&DataKey::MaxCheckInInterval);
        }
        env.storage()
            .instance()
            .set(&DataKey::MaxTtlSeconds, &config.max_ttl_seconds);
        env.storage()
            .instance()
            .set(&DataKey::TtlDecayRate, &config.ttl_decay_rate);
        env.storage()
            .instance()
            .remove(&DataKey::PendingProtocolConfig);
        env.storage()
            .instance()
            .remove(&DataKey::ProtocolConfigProposedAt);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish((PROTOCOL_CONFIG_APPLIED_TOPIC,), now);
    }

    /// Returns the contract version string set during initialization.
    pub fn get_contract_version(env: Env) -> String {
        env.storage()
            .instance()
            .get(&DataKey::Version)
            .unwrap_or_else(|| panic_with_error!(&env, ContractError::NotInitialized))
    }

    /// Proposes a new admin with a 24-hour timelock (Issue #813).
    /// Only the current admin can call this. The proposed admin must call
    /// `accept_admin` after the timelock has elapsed.
    pub fn propose_new_admin(env: Env, new_admin: Address) {
        Self::require_admin(&env);
        let proposed_at = env.ledger().timestamp();
        env.storage()
            .instance()
            .set(&DataKey::PendingAdmin, &new_admin);
        env.storage()
            .instance()
            .set(&DataKey::AdminTransferProposedAt, &proposed_at);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((ADMIN_TRANSFER_PROPOSED_TOPIC,), (new_admin, proposed_at));
    }

    /// Backward-compatible alias for `propose_new_admin`.
    pub fn propose_admin(env: Env, new_admin: Address) {
        Self::propose_new_admin(env, new_admin);
    }

    /// Returns the pending admin address, if any.
    pub fn get_pending_admin(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::PendingAdmin)
    }

    /// Accepts a pending admin transfer. Must be called by the pending admin
    /// after the 24-hour timelock has elapsed (Issue #813).
    pub fn accept_admin(env: Env) {
        let pending: Address = env
            .storage()
            .instance()
            .get(&DataKey::PendingAdmin)
            .unwrap_or_else(|| panic_with_error!(&env, ContractError::NoPendingAdmin));
        pending.require_auth();
        let proposed_at: u64 = env
            .storage()
            .instance()
            .get(&DataKey::AdminTransferProposedAt)
            .unwrap_or(0);
        if env.ledger().timestamp() < proposed_at + ADMIN_TRANSFER_TIMELOCK {
            panic_with_error!(&env, ContractError::AdminTransferTimeLocked);
        }
        env.storage().instance().set(&DataKey::Admin, &pending);
        env.storage().instance().remove(&DataKey::PendingAdmin);
        env.storage()
            .instance()
            .remove(&DataKey::AdminTransferProposedAt);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((ADMIN_TRANSFER_COMPLETED_TOPIC,), pending);
    }
}
