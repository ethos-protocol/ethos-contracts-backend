#[contractimpl]
impl TtlVaultContract {
    // --- Task 3: set_beneficiaries ---

    /// Sets multiple beneficiaries with basis point (BPS) allocations.
    ///
    /// The sum of all BPS values must equal 10,000 (100%). When the vault is
    /// released, funds are split according to these allocations.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `beneficiaries` - Vector of BeneficiaryEntry structs with addresses and BPS values
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    /// * `ContractError::InvalidBps` - If BPS sum is not 10,000
    pub fn set_beneficiaries(
        env: Env,
        vault_id: u64,
        caller: Address,
        beneficiaries: Vec<BeneficiaryEntry>,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        if beneficiaries.is_empty() {
            return Err(ContractError::InvalidBps);
        }
        let mut vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        let total_bps: u32 = beneficiaries.iter().map(|e| e.bps).sum();
        if total_bps != 10_000 {
            return Err(ContractError::InvalidBps);
        }
        for entry in beneficiaries.iter() {
            if entry.address == vault.owner {
                return Err(ContractError::InvalidBeneficiary);
            }
            Self::assert_not_zero_address(&env, &entry.address);
            if !aml::check_compliance(&env, &entry.address) {
                return Err(ContractError::AmlFlaggedAddress);
            }
        }
        vault.beneficiaries = beneficiaries.clone();
        Self::save_vault(&env, vault_id, &vault);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((SET_BENEFICIARIES_TOPIC, vault_id), beneficiaries);
        Ok(())
    }

    /// Schedule a beneficiary rotation for a vault.
    ///
    /// The caller must be the vault owner. The `new_beneficiaries` vector must
    /// be non-empty and sum to 10,000 BPS. The rotation will be applied
    /// automatically when the effective timestamp is reached (e.g., on next
    /// check-in or other mutation) and emits a `BEN_ROTATION_TOPIC` event.
    pub fn schedule_beneficiary_rotation(
        env: Env,
        vault_id: u64,
        caller: Address,
        effective_timestamp: u64,
        new_beneficiaries: Vec<BeneficiaryEntry>,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        if new_beneficiaries.is_empty() {
            return Err(ContractError::InvalidBps);
        }
        let total_bps: u32 = new_beneficiaries.iter().map(|e| e.bps).sum();
        if total_bps != 10_000 {
            return Err(ContractError::InvalidBps);
        }
        for entry in new_beneficiaries.iter() {
            if entry.address == vault.owner {
                return Err(ContractError::InvalidBeneficiary);
            }
        }
        let rot_key = DataKey::BeneficiaryRotationSchedule(vault_id);
        let mut schedule: Vec<BeneficiaryRotationEntry> = env
            .storage()
            .persistent()
            .get::<DataKey, Vec<BeneficiaryRotationEntry>>(&rot_key)
            .unwrap_or_else(|| Vec::new(&env));
        schedule.push_back(BeneficiaryRotationEntry {
            effective_timestamp,
            new_beneficiaries: new_beneficiaries.clone(),
        });
        env.storage().persistent().set(&rot_key, &schedule);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (BENEFICIARY_TRIGGER_SET_TOPIC, vault_id),
            (effective_timestamp, new_beneficiaries),
        );
        Ok(())
    }

    /// Adds a single beneficiary to a vault's multi-beneficiary split.
    ///
    /// This function adds a new beneficiary with the specified BPS allocation.
    /// The total BPS across all beneficiaries must not exceed 10,000 (100%).
    /// If adding this beneficiary would exceed 10,000 BPS, the operation fails.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `caller` - The address of the caller (must be the vault owner)
    /// * `address` - The beneficiary address to add
    /// * `percentage` - The BPS allocation (0-10000)
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    /// * `ContractError::InvalidBps` - If total BPS would exceed 10,000
    /// * `ContractError::InvalidBeneficiary` - If address is the vault owner
    pub fn add_beneficiary(
        env: Env,
        vault_id: u64,
        caller: Address,
        address: Address,
        percentage: u32,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let mut vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        if address == vault.owner {
            return Err(ContractError::InvalidBeneficiary);
        }
        if !aml::check_compliance(&env, &address) {
            return Err(ContractError::AmlFlaggedAddress);
        }

        // Check if beneficiary already exists
        for entry in vault.beneficiaries.iter() {
            if entry.address == address {
                return Err(ContractError::InvalidBeneficiary);
            }
        }

        // Calculate total BPS after adding new beneficiary
        let current_total: u32 = vault.beneficiaries.iter().map(|e| e.bps).sum();
        if current_total + percentage > 10_000 {
            return Err(ContractError::InvalidBps);
        }

        vault.beneficiaries.push_back(BeneficiaryEntry {
            address: address.clone(),
            bps: percentage,
            minimum_threshold: 0,
        });

        Self::save_vault(&env, vault_id, &vault);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((BENEFICIARY_UPDATED_TOPIC, vault_id), (address, percentage));
        Ok(())
    }

    /// Removes a beneficiary from a vault's multi-beneficiary split.
    ///
    /// This function removes an existing beneficiary from the vault's beneficiaries list.
    /// If the beneficiary is not found, the operation fails.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `caller` - The address of the caller (must be the vault owner)
    /// * `address` - The beneficiary address to remove
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    /// * `ContractError::InvalidBeneficiary` - If beneficiary is not found
    pub fn remove_beneficiary(
        env: Env,
        vault_id: u64,
        caller: Address,
        address: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let mut vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }

        let mut found = false;
        let mut new_beneficiaries = Vec::new(&env);
        for entry in vault.beneficiaries.iter() {
            if entry.address == address {
                found = true;
            } else {
                new_beneficiaries.push_back(entry);
            }
        }

        if !found {
            return Err(ContractError::InvalidBeneficiary);
        }

        vault.beneficiaries = new_beneficiaries;
        Self::save_vault(&env, vault_id, &vault);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((BENEFICIARY_UPDATED_TOPIC, vault_id), address);
        Ok(())
    }

    // --- Issue #512: Beneficiary Minimum Threshold ---

    /// Updates the minimum threshold for a specific beneficiary.
    ///
    /// The minimum threshold is the minimum amount (in stroops) that a beneficiary must
    /// receive. If their calculated share is below this threshold, they receive nothing
    /// and the funds are redistributed to other qualifying beneficiaries.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `caller` - The address of the caller (must be the vault owner)
    /// * `beneficiary_address` - The address of the beneficiary to update
    /// * `minimum_threshold` - The minimum amount in stroops (0 to disable)
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    /// * `ContractError::InvalidBeneficiary` - If beneficiary is not found
    pub fn set_ben_min_threshold(
        env: Env,
        vault_id: u64,
        caller: Address,
        beneficiary_address: Address,
        minimum_threshold: i128,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let mut vault = Self::load_vault(&env, vault_id);

        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        if minimum_threshold < 0 {
            return Err(ContractError::InvalidAmount);
        }

        // Find and update the beneficiary
        let mut found = false;
        let mut updated_beneficiaries = Vec::new(&env);
        for entry in vault.beneficiaries.iter() {
            if entry.address == beneficiary_address {
                let mut updated_entry = entry.clone();
                updated_entry.minimum_threshold = minimum_threshold;
                updated_beneficiaries.push_back(updated_entry);
                found = true;
            } else {
                updated_beneficiaries.push_back(entry);
            }
        }

        if !found {
            return Err(ContractError::InvalidBeneficiary);
        }

        vault.beneficiaries = updated_beneficiaries;
        Self::save_vault(&env, vault_id, &vault);
        Self::append_activity_log(&env, vault_id, "set_ben_min_threshold", &caller, "");
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (MIN_THRESHOLD_SET_TOPIC, vault_id),
            (beneficiary_address, minimum_threshold),
        );
        Ok(())
    }

    /// Gets the minimum threshold for a specific beneficiary.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `beneficiary_address` - The address of the beneficiary to query
    ///
    /// # Returns
    /// `Some(minimum_threshold)` if the beneficiary exists, `None` otherwise
    pub fn get_ben_min_threshold(
        env: Env,
        vault_id: u64,
        beneficiary_address: Address,
    ) -> Option<i128> {
        if let Some(vault) = Self::try_load_vault(&env, vault_id) {
            for entry in vault.beneficiaries.iter() {
                if entry.address == beneficiary_address {
                    return Some(entry.minimum_threshold);
                }
            }
        }
        None
    }

    /// Gets all beneficiaries and their minimum thresholds for a vault.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    ///
    /// # Returns
    /// Vector of BeneficiaryEntry structs with addresses, BPS, and minimum thresholds
    pub fn get_bens_with_thresholds(env: Env, vault_id: u64) -> Option<Vec<BeneficiaryEntry>> {
        Self::try_load_vault(&env, vault_id).map(|vault| vault.beneficiaries.clone())
    }

    // --- Task 4: update_metadata ---

    /// Updates the metadata string associated with a vault.
    ///
    /// This can be used to store a label, IPFS hash, or other reference data.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The unique identifier of the vault
    /// * `metadata` - The metadata string to attach
    ///
    /// # Returns
    /// `Ok(())` on success, `Err` on failure
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    pub fn update_metadata(
        env: Env,
        vault_id: u64,
        caller: Address,
        metadata: String,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        if metadata.len() > MAX_METADATA_LEN {
            return Err(ContractError::InvalidAmount);
        }
        let mut vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        vault.metadata = metadata.clone();
        Self::save_vault(&env, vault_id, &vault);
        Self::log_audit_entry(&env, vault_id, "update_metadata", &caller, "");
        Self::append_activity_log(&env, vault_id, "update_metadata", &caller, "");
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((UPDATE_METADATA_TOPIC, vault_id), metadata);
        Ok(())
    }

    /// Sets the vault name, description, and notes fields.
    ///
    /// # Arguments
    /// * `vault_id` - The vault ID
    /// * `caller` - The address calling this function (must be vault owner)
    /// * `name` - Vault name/title (max 64 chars)
    /// * `description` - Vault description (max 512 chars)
    /// * `notes` - Notes/instructions for beneficiary (max 1024 chars)
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - If vault is not in Locked status
    /// * `ContractError::InvalidAmount` - If any field exceeds size limits
    pub fn set_vault_notes(
        env: Env,
        vault_id: u64,
        caller: Address,
        name: String,
        description: String,
        notes: String,
    ) -> Result<(), ContractError> {
        caller.require_auth();

        // Validate field lengths
        if name.len() > MAX_NAME_LEN {
            return Err(ContractError::InvalidAmount);
        }
        if description.len() > MAX_DESCRIPTION_LEN {
            return Err(ContractError::InvalidAmount);
        }
        if notes.len() > MAX_NOTES_LEN {
            return Err(ContractError::InvalidAmount);
        }

        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }

        Self::append_activity_log(&env, vault_id, "update_metadata", &caller, "");
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        Ok(())
    }

    /// Gets the vault metadata fields.
    ///
    /// # Arguments
    /// * `vault_id` - The vault ID
    ///
    /// # Returns
    /// The vault metadata string
    pub fn get_vault_notes(env: Env, vault_id: u64) -> String {
        let vault = Self::load_vault(&env, vault_id);
        vault.metadata
    }
}
