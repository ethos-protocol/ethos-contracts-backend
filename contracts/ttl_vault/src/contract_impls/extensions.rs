#[contractimpl]
impl TtlVaultContract {
    /// Admin-only. Records the function-signature fingerprint alongside the
    /// legacy upgrade manifest. The fingerprint is computed from the sorted
    /// public function names, argument types, and return types in the WASM spec.
    pub fn set_upgrade_manifest_signed(
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

    /// Returns the recorded public function-signature fingerprint, if set.
    pub fn get_upgrade_sigs_hash(env: Env) -> Option<BytesN<32>> {
        env.storage()
            .instance()
            .get(&DataKey::UpgradeFunctionSignaturesHash)
    }

    /// Validates the legacy manifest and requires an identical public function
    /// signature fingerprint for the proposed contract.
    pub fn validate_upgrade_signed(
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

    /// Admin-only. Explicitly stages a manifest-compatible upgrade.
    pub fn propose_upgrade_with_manifest(
        env: Env,
        new_wasm_hash: BytesN<32>,
        new_exported_fn_count: u32,
        new_error_code_count: u32,
        new_storage_schema_hash: BytesN<32>,
    ) {
        Self::propose_upgrade_internal(
            env,
            new_wasm_hash,
            new_exported_fn_count,
            new_error_code_count,
            new_storage_schema_hash,
            None,
        );
    }

    /// Admin-only. Validates and stages a signature-aware upgrade.
    pub fn propose_legacy_signed_upgrade(
        env: Env,
        new_wasm_hash: BytesN<32>,
        new_exported_fn_count: u32,
        new_error_code_count: u32,
        new_storage_schema_hash: BytesN<32>,
        new_function_signatures_hash: BytesN<32>,
    ) {
        Self::propose_upgrade_internal(
            env,
            new_wasm_hash,
            new_exported_fn_count,
            new_error_code_count,
            new_storage_schema_hash,
            Some(new_function_signatures_hash),
        );
    }

    /// Admin-only. Stages a signature-aware upgrade for delayed execution.
    pub fn propose_signed_upgrade(
        env: Env,
        new_wasm_hash: BytesN<32>,
        new_exported_fn_count: u32,
        new_error_code_count: u32,
        new_storage_schema_hash: BytesN<32>,
        new_function_signatures_hash: BytesN<32>,
    ) {
        Self::propose_upgrade_internal(
            env,
            new_wasm_hash,
            new_exported_fn_count,
            new_error_code_count,
            new_storage_schema_hash,
            Some(new_function_signatures_hash),
        );
    }

    /// Admin-only. Executes the pending manifest-validated upgrade after the
    /// 24-hour timelock has elapsed.
    pub fn execute_upgrade(env: Env) {
        Self::require_admin(&env);
        let proposal: UpgradeProposal = env
            .storage()
            .instance()
            .get(&DataKey::PendingUpgrade)
            .unwrap_or_else(|| panic_with_error!(&env, ContractError::NoPendingUpgrade));
        if env.ledger().timestamp() < proposal.proposed_at.saturating_add(UPGRADE_TIMELOCK) {
            panic_with_error!(&env, ContractError::UpgradeTimeLocked);
        }

        Self::validate_upgrade(env.clone(), proposal.wasm_hash.clone());
        match proposal
            .function_signatures_hash
            .clone()
            .map(|bytes| Self::signatures_hash_from_bytes(&env, &bytes))
        {
            Some(signatures_hash) => Self::validate_upgrade_signed(
                env.clone(),
                proposal.exported_fn_count,
                proposal.error_code_count,
                proposal.storage_schema_hash.clone(),
                signatures_hash,
            ),
            None => Self::validate_upgrade_compatibility(
                env.clone(),
                proposal.exported_fn_count,
                proposal.error_code_count,
                proposal.storage_schema_hash.clone(),
            ),
        }

        let version = env
            .storage()
            .instance()
            .get::<DataKey, UpgradeManifest>(&DataKey::UpgradeManifest)
            .unwrap_or_else(|| panic_with_error!(&env, ContractError::UpgradeManifestNotSet))
            .version
            .saturating_add(1);
        env.deployer()
            .update_current_contract_wasm(proposal.wasm_hash);
        let manifest = UpgradeManifest {
            exported_fn_count: proposal.exported_fn_count,
            error_code_count: proposal.error_code_count,
            storage_schema_hash: proposal.storage_schema_hash,
            version,
        };
        env.storage()
            .instance()
            .set(&DataKey::UpgradeManifest, &manifest);
        match proposal
            .function_signatures_hash
            .map(|bytes| Self::signatures_hash_from_bytes(&env, &bytes))
        {
            Some(signatures_hash) => env
                .storage()
                .instance()
                .set(&DataKey::UpgradeFunctionSignaturesHash, &signatures_hash),
            None => env
                .storage()
                .instance()
                .remove(&DataKey::UpgradeFunctionSignaturesHash),
        }
        env.storage()
            .instance()
            .remove(&DataKey::PendingUpgrade);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
    }

    /// Converts a stored signatures fingerprint back into a 32-byte hash.
    fn signatures_hash_from_bytes(env: &Env, bytes: &Bytes) -> BytesN<32> {
        if bytes.len() != 32 {
            panic_with_error!(env, ContractError::UpgradeFunctionSignaturesChanged);
        }
        let mut buf = [0u8; 32];
        bytes.copy_into_slice(&mut buf);
        BytesN::from_array(env, &buf)
    }

    fn propose_upgrade_internal(
        env: Env,
        new_wasm_hash: BytesN<32>,
        new_exported_fn_count: u32,
        new_error_code_count: u32,
        new_storage_schema_hash: BytesN<32>,
        new_function_signatures_hash: Option<BytesN<32>>,
    ) {
        Self::require_admin(&env);
        Self::validate_upgrade(env.clone(), new_wasm_hash.clone());

        match new_function_signatures_hash.clone() {
            Some(signatures_hash) => Self::validate_upgrade_signed(
                env.clone(),
                new_exported_fn_count,
                new_error_code_count,
                new_storage_schema_hash.clone(),
                signatures_hash,
            ),
            None => {
                if env
                    .storage()
                    .instance()
                    .has(&DataKey::UpgradeFunctionSignaturesHash)
                {
                    panic_with_error!(&env, ContractError::UpgradeFunctionSignaturesRequired);
                }
                Self::validate_upgrade_compatibility(
                    env.clone(),
                    new_exported_fn_count,
                    new_error_code_count,
                    new_storage_schema_hash.clone(),
                );
            }
        }

        let proposal = UpgradeProposal {
            wasm_hash: new_wasm_hash,
            exported_fn_count: new_exported_fn_count,
            error_code_count: new_error_code_count,
            storage_schema_hash: new_storage_schema_hash,
            function_signatures_hash: new_function_signatures_hash
                .map(|hash| Bytes::from_array(&env, &hash.to_array())),
            proposed_at: env.ledger().timestamp(),
        };
        env.storage()
            .instance()
            .set(&DataKey::PendingUpgrade, &proposal);
    }

    /// Returns a paginated slice of vault IDs where the given address is a beneficiary.
    pub fn get_vaults_as_beneficiary_page(
        env: Env,
        beneficiary: Address,
        page: u32,
        page_size: u32,
    ) -> Vec<u64> {
        let all = Self::load_beneficiary_vault_ids(&env, &beneficiary);
        Self::paginate(&env, all, page, page_size)
    }

    /// Owner-only. Configure or replace the donation account for `vault_id`.
    ///
    /// `public_visibility` controls whether donation records are listed
    /// publicly via `list_donations`; totals are always public.
    pub fn set_donation_account(
        env: Env,
        vault_id: u64,
        caller: Address,
        recipient: Address,
        public_visibility: bool,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        donations::set_donation_account(&env, vault_id, recipient, public_visibility);
        Ok(())
    }

    /// Donate `amount` of the vault's token to its donation account.
    ///
    /// Funds move directly from `donor` to the donation account, and the call
    /// returns the issued tax receipt. The donor must pass AML screening.
    pub fn donate(
        env: Env,
        vault_id: u64,
        donor: Address,
        amount: i128,
    ) -> donations::DonationRecord {
        donor.require_auth();
        Self::assert_compliant(&env, &donor);
        let vault = Self::load_vault(&env, vault_id);
        donations::donate(&env, vault_id, &donor, &vault.token_address, amount)
    }

    /// Return the donation account configured for `vault_id`, if any.
    pub fn get_donation_account(env: Env, vault_id: u64) -> Option<donations::DonationAccount> {
        donations::get_donation_account(&env, vault_id)
    }

    /// Return the tax receipt for `receipt_no` on `vault_id`, if it exists.
    pub fn get_donation_receipt(
        env: Env,
        vault_id: u64,
        receipt_no: u64,
    ) -> Option<donations::DonationRecord> {
        donations::get_donation(&env, vault_id, receipt_no)
    }

    /// Total amount donated to `vault_id` across all donors.
    pub fn get_donation_total(env: Env, vault_id: u64) -> i128 {
        donations::get_vault_donation_total(&env, vault_id)
    }

    /// Total amount `donor` has donated to `vault_id`.
    pub fn get_donor_total(env: Env, vault_id: u64, donor: Address) -> i128 {
        donations::get_donor_donation_total(&env, vault_id, &donor)
    }

    /// Paginated public listing of donation records, starting at
    /// `start_receipt_no` (1-based). Requires public visibility to be enabled.
    pub fn list_donations(
        env: Env,
        vault_id: u64,
        start_receipt_no: u64,
        limit: u32,
    ) -> Vec<donations::DonationRecord> {
        donations::list_donations(&env, vault_id, start_receipt_no, limit)
    }

    /// Return the NFT minted for `vault_id` on creation, if any.
    pub fn get_vault_nft(env: Env, vault_id: u64) -> Option<vault_nft::VaultNft> {
        vault_nft::get_vault_nft(&env, vault_id)
    }

    /// Return the NFT with `token_id`, if it exists.
    pub fn get_nft_by_token(env: Env, token_id: u64) -> Option<vault_nft::VaultNft> {
        vault_nft::get_nft_by_token(&env, token_id)
    }

    /// Current holder of the NFT for `vault_id`.
    pub fn get_nft_owner(env: Env, vault_id: u64) -> Option<Address> {
        vault_nft::get_nft_owner(&env, vault_id)
    }

    /// Holder-only. Set the metadata stored on the NFT for `vault_id`.
    pub fn set_nft_metadata(
        env: Env,
        vault_id: u64,
        caller: Address,
        metadata: String,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        match vault_nft::get_nft_owner(&env, vault_id) {
            None => Err(ContractError::NftNotFound),
            Some(holder) if holder != caller => Err(ContractError::NotOwner),
            Some(_) => {
                vault_nft::set_nft_metadata(&env, vault_id, metadata);
                Ok(())
            }
        }
    }

    /// Buy the NFT `token_id` from its current holder for `price` of the
    /// vault's token. Funds go directly to the seller and the sale is recorded.
    /// The buyer must pass AML screening.
    pub fn buy_nft(env: Env, token_id: u64, buyer: Address, price: i128) -> vault_nft::NftSale {
        buyer.require_auth();
        Self::assert_compliant(&env, &buyer);
        let nft = vault_nft::get_nft_by_token(&env, token_id)
            .unwrap_or_else(|| panic_with_error!(&env, ContractError::NftNotFound));
        let vault = Self::load_vault(&env, nft.vault_id);
        vault_nft::buy_nft(&env, token_id, &buyer, &vault.token_address, price)
    }

    /// Paginated sales history for the NFT `token_id`, starting at
    /// `start_sale_no` (1-based).
    pub fn list_nft_sales(
        env: Env,
        token_id: u64,
        start_sale_no: u64,
        limit: u32,
    ) -> Vec<vault_nft::NftSale> {
        vault_nft::list_nft_sales(&env, token_id, start_sale_no, limit)
    }
}
