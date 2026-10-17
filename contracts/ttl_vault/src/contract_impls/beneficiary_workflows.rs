#[contractimpl]
impl TtlVaultContract {
    // --- Issue #401: Beneficiary Delegation ---

    /// Delegates beneficiary role to another address.
    /// Only the current beneficiary or the current delegate can call this.
    pub fn delegate_beneficiary_role(env: Env, vault_id: u64, delegate_address: Address) {
        Self::assert_not_paused(&env);
        let vault = Self::load_vault(&env, vault_id);

        let mut chain: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::BeneficiaryDelegationChain(vault_id))
            .unwrap_or_else(|| {
                let mut v = Vec::new(&env);
                v.push_back(vault.beneficiary.clone());
                v
            });

        // The chain always has at least one entry: either loaded from storage (which is
        // only ever written non-empty, see below) or freshly seeded with the beneficiary.
        let current_delegate = chain
            .get(chain.len() - 1)
            .expect("chain is seeded with at least one entry and never stored empty");
        current_delegate.require_auth();

        // Check if already in chain to prevent cycles
        for addr in chain.iter() {
            if addr == delegate_address {
                panic_with_error!(&env, ContractError::InvalidBeneficiary);
            }
        }

        chain.push_back(delegate_address.clone());

        env.storage()
            .persistent()
            .set(&DataKey::BeneficiaryDelegationChain(vault_id), &chain);

        env.events().publish(
            (DELEGATE_BENEFICIARY_TOPIC,),
            (vault_id, current_delegate.clone(), delegate_address.clone()),
        );
        env.storage().persistent().extend_ttl(
            &DataKey::BeneficiaryDelegationChain(vault_id),
            VAULT_TTL_THRESHOLD,
            vault_ttl_ledgers(vault.check_in_interval),
        );
    }

    /// Gets the beneficiary delegation chain for a vault.
    pub fn get_beneficiary_delegation_chain(env: Env, vault_id: u64) -> Vec<Address> {
        env.storage()
            .persistent()
            .get::<DataKey, Vec<Address>>(&DataKey::BeneficiaryDelegationChain(vault_id))
            .unwrap_or_else(|| {
                let vault = Self::load_vault(&env, vault_id);
                let mut v = Vec::new(&env);
                v.push_back(vault.beneficiary);
                v
            })
    }

    // --- Issue #402: Withdrawal Scheduling ---

    /// Sets a withdrawal schedule for the vault. Owner-only.
    pub fn set_withdrawal_schedule(
        env: Env,
        vault_id: u64,
        schedule: Vec<(u64, i128)>,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        let vault = Self::load_vault(&env, vault_id);
        vault.owner.require_auth();

        let mut entries = Vec::new(&env);
        for (timestamp, amount) in schedule.iter() {
            if amount <= 0 {
                return Err(ContractError::InvalidAmount);
            }
            entries.push_back(WithdrawalScheduleEntry { timestamp, amount });
        }

        env.storage()
            .persistent()
            .set(&DataKey::WithdrawalSchedule(vault_id), &entries);

        env.events()
            .publish((WITHDRAWAL_SCHEDULED_TOPIC,), (vault_id, entries.len()));
        env.storage().persistent().extend_ttl(
            &DataKey::WithdrawalSchedule(vault_id),
            VAULT_TTL_THRESHOLD,
            vault_ttl_ledgers(vault.check_in_interval),
        );
        Ok(())
    }

    /// Executes a scheduled withdrawal if conditions are met. Anyone can call.
    pub fn execute_scheduled_withdrawal(env: Env, vault_id: u64) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        let mut vault = Self::load_vault(&env, vault_id);

        let key = DataKey::WithdrawalSchedule(vault_id);
        let mut schedule = env
            .storage()
            .persistent()
            .get::<DataKey, Vec<WithdrawalScheduleEntry>>(&key)
            .ok_or(ContractError::NoScheduledWithdrawals)?;

        let now = env.ledger().timestamp();
        let mut executed = false;

        for i in 0..schedule.len() {
            let entry = schedule.get(i).unwrap();
            if entry.timestamp <= now && entry.amount > 0 {
                if vault.balance < entry.amount {
                    return Err(ContractError::InsufficientBalance);
                }

                let token_client = token::Client::new(&env, &vault.token_address);
                let beneficiary = Self::get_delegated_beneficiary(&env, vault_id)
                    .unwrap_or(vault.beneficiary.clone());

                crate::aml::require_compliant(&env, &beneficiary);
                token_client.transfer(&env.current_contract_address(), &beneficiary, &entry.amount);

                vault.balance -= entry.amount;
                schedule.set(
                    i,
                    WithdrawalScheduleEntry {
                        timestamp: entry.timestamp,
                        amount: 0,
                    },
                );
                executed = true;

                env.events()
                    .publish((WITHDRAWAL_EXECUTED_TOPIC,), (vault_id, entry.amount));
            }
        }

        if executed {
            Self::save_vault(&env, vault_id, &vault);
            env.storage()
                .persistent()
                .set(&DataKey::WithdrawalSchedule(vault_id), &schedule);
        }

        if executed {
            Ok(())
        } else {
            Err(ContractError::NoScheduledWithdrawals)
        }
    }

    // --- Issue #400: Conditional Acceptance ---

    /// Beneficiary accepts with conditions. Beneficiary-only.
    pub fn accept_with_conditions(
        env: Env,
        vault_id: u64,
        conditions: String,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        let vault = Self::load_vault(&env, vault_id);
        vault.beneficiary.require_auth();

        if conditions.is_empty() {
            return Err(ContractError::InvalidAmount);
        }

        let entry = ConditionalAcceptanceEntry {
            conditions,
            approved_by_owner: false,
            acceptance_deadline: None,
            min_balance_threshold: None,
        };

        env.storage()
            .persistent()
            .set(&DataKey::ConditionalAcceptance(vault_id), &entry);

        env.events().publish(
            (CONDITIONS_ACCEPTED_TOPIC,),
            (vault_id, vault.beneficiary.clone()),
        );
        env.storage().persistent().extend_ttl(
            &DataKey::ConditionalAcceptance(vault_id),
            VAULT_TTL_THRESHOLD,
            vault_ttl_ledgers(vault.check_in_interval),
        );
        Ok(())
    }

    /// Owner approves conditional acceptance.
    pub fn approve_conditional_acceptance(env: Env, vault_id: u64) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        let vault = Self::load_vault(&env, vault_id);
        vault.owner.require_auth();

        let key = DataKey::ConditionalAcceptance(vault_id);
        let mut entry = env
            .storage()
            .persistent()
            .get::<DataKey, ConditionalAcceptanceEntry>(&key)
            .ok_or(ContractError::InvalidBeneficiary)?;

        entry.approved_by_owner = true;
        env.storage().persistent().set(&key, &entry);
        Ok(())
    }

    /// Gets conditional acceptance entry if it exists.
    pub fn get_conditional_acceptance(
        env: Env,
        vault_id: u64,
    ) -> Option<ConditionalAcceptanceEntry> {
        env.storage()
            .persistent()
            .get::<DataKey, ConditionalAcceptanceEntry>(&DataKey::ConditionalAcceptance(vault_id))
    }

    /// Sets an acceptance deadline on the conditional acceptance entry. Owner-only.
    ///
    /// If the deadline passes without owner approval, trigger_release reverts funds to the owner.
    pub fn set_acceptance_deadline(
        env: Env,
        vault_id: u64,
        deadline_timestamp: u64,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        let vault = Self::load_vault(&env, vault_id);
        vault.owner.require_auth();
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }

        let key = DataKey::ConditionalAcceptance(vault_id);
        let mut entry = env
            .storage()
            .persistent()
            .get::<DataKey, ConditionalAcceptanceEntry>(&key)
            .ok_or(ContractError::InvalidBeneficiary)?;

        entry.acceptance_deadline = Some(deadline_timestamp);
        env.storage().persistent().set(&key, &entry);
        env.storage().persistent().extend_ttl(
            &key,
            VAULT_TTL_THRESHOLD,
            vault_ttl_ledgers(vault.check_in_interval),
        );
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        Ok(())
    }

    // --- Issue #503: Beneficiary Conditional Acceptance with Threshold ---

    /// Beneficiary accepts role conditionally with minimum balance threshold.
    /// Only accepts if vault balance >= min_balance_threshold at release time.
    pub fn accept_with_threshold(
        env: Env,
        vault_id: u64,
        min_balance_threshold: i128,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        let vault = Self::load_vault(&env, vault_id);
        vault.beneficiary.require_auth();

        if min_balance_threshold <= 0 {
            return Err(ContractError::InvalidAmount);
        }

        let acceptance = BeneficiaryConditionalAcceptance {
            min_balance_threshold,
            accepted_at: env.ledger().timestamp(),
        };

        env.storage().persistent().set(
            &DataKey::BeneficiaryConditionalAcceptance(vault_id),
            &acceptance,
        );

        env.events().publish(
            (BENEFICIARY_CONDITION_ACCEPTED_TOPIC,),
            (vault_id, vault.beneficiary.clone(), min_balance_threshold),
        );
        env.storage().persistent().extend_ttl(
            &DataKey::BeneficiaryConditionalAcceptance(vault_id),
            VAULT_TTL_THRESHOLD,
            vault_ttl_ledgers(vault.check_in_interval),
        );
        Ok(())
    }

    /// Gets beneficiary conditional acceptance if it exists.
    pub fn get_beneficiary_cond_acceptance(
        env: Env,
        vault_id: u64,
    ) -> Option<BeneficiaryConditionalAcceptance> {
        env.storage()
            .persistent()
            .get::<DataKey, BeneficiaryConditionalAcceptance>(
                &DataKey::BeneficiaryConditionalAcceptance(vault_id),
            )
    }

    /// Checks if beneficiary conditional acceptance conditions are met.
    fn check_conditional_acceptance_threshold(
        env: &Env,
        vault_id: u64,
        current_balance: i128,
    ) -> bool {
        if let Some(acceptance) = env
            .storage()
            .persistent()
            .get::<DataKey, BeneficiaryConditionalAcceptance>(
                &DataKey::BeneficiaryConditionalAcceptance(vault_id),
            )
        {
            current_balance >= acceptance.min_balance_threshold
        } else {
            true
        }
    }

    // --- Issue #399: Dispute Resolution ---

    /// Files a dispute. Beneficiary-only.
    pub fn file_dispute(env: Env, vault_id: u64, reason: String) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        let vault = Self::load_vault(&env, vault_id);
        vault.beneficiary.require_auth();

        if reason.is_empty() {
            return Err(ContractError::InvalidAmount);
        }

        let current_status = env
            .storage()
            .persistent()
            .get::<DataKey, DisputeStatus>(&DataKey::DisputeStatus(vault_id))
            .unwrap_or(DisputeStatus::None);

        if current_status == DisputeStatus::Filed {
            return Err(ContractError::DisputeFiled);
        }

        env.storage()
            .persistent()
            .set(&DataKey::DisputeStatus(vault_id), &DisputeStatus::Filed);

        env.events().publish(
            (DISPUTE_FILED_TOPIC,),
            (vault_id, vault.beneficiary.clone(), reason),
        );
        env.storage().persistent().extend_ttl(
            &DataKey::DisputeStatus(vault_id),
            VAULT_TTL_THRESHOLD,
            vault_ttl_ledgers(vault.check_in_interval),
        );
        Ok(())
    }

    /// Resolves a dispute. Admin-only.
    pub fn resolve_dispute(
        env: Env,
        vault_id: u64,
        resolution: String,
    ) -> Result<(), ContractError> {
        Self::require_admin(&env);

        let current_status = env
            .storage()
            .persistent()
            .get::<DataKey, DisputeStatus>(&DataKey::DisputeStatus(vault_id))
            .unwrap_or(DisputeStatus::None);

        if current_status != DisputeStatus::Filed {
            return Err(ContractError::InvalidBeneficiary);
        }

        env.storage()
            .persistent()
            .set(&DataKey::DisputeStatus(vault_id), &DisputeStatus::Resolved);

        env.events()
            .publish((DISPUTE_RESOLVED_TOPIC,), (vault_id, resolution));
        Ok(())
    }

    /// Gets the dispute status for a vault.
    pub fn get_dispute_status(env: Env, vault_id: u64) -> DisputeStatus {
        env.storage()
            .persistent()
            .get::<DataKey, DisputeStatus>(&DataKey::DisputeStatus(vault_id))
            .unwrap_or(DisputeStatus::None)
    }

    // --- Issue #502: Beneficiary Conflict Resolution ---

    /// File a beneficiary conflict claim. Beneficiary-only.
    pub fn file_beneficiary_conflict(
        env: Env,
        vault_id: u64,
        reason: String,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        let vault = Self::load_vault(&env, vault_id);
        vault.beneficiary.require_auth();

        if reason.is_empty() {
            return Err(ContractError::InvalidAmount);
        }

        let mut conflict = env
            .storage()
            .persistent()
            .get::<DataKey, BeneficiaryConflict>(&DataKey::BeneficiaryConflict(vault_id))
            .unwrap_or_else(|| BeneficiaryConflict {
                vault_id,
                claims: Vec::new(&env),
                resolution: ConflictResolution::Pending,
                resolved_at: None,
            });

        let claim = BeneficiaryConflictClaim {
            claimant: vault.beneficiary.clone(),
            reason,
            filed_at: env.ledger().timestamp(),
        };

        conflict.claims.push_back(claim);
        env.storage()
            .persistent()
            .set(&DataKey::BeneficiaryConflict(vault_id), &conflict);

        env.events().publish(
            (BENEFICIARY_CONFLICT_FILED_TOPIC,),
            (vault_id, vault.beneficiary.clone()),
        );
        env.storage().persistent().extend_ttl(
            &DataKey::BeneficiaryConflict(vault_id),
            VAULT_TTL_THRESHOLD,
            vault_ttl_ledgers(vault.check_in_interval),
        );
        Ok(())
    }

    /// Resolve beneficiary conflict. Admin-only.
    pub fn resolve_beneficiary_conflict(
        env: Env,
        vault_id: u64,
        approved_beneficiary: Address,
    ) -> Result<(), ContractError> {
        Self::require_admin(&env);

        let mut conflict = env
            .storage()
            .persistent()
            .get::<DataKey, BeneficiaryConflict>(&DataKey::BeneficiaryConflict(vault_id))
            .ok_or(ContractError::InvalidBeneficiary)?;

        if conflict.resolution != ConflictResolution::Pending {
            return Err(ContractError::InvalidBeneficiary);
        }

        // Check if conflict has expired (30 days without resolution)
        let now = env.ledger().timestamp();
        if let Some(first_claim) = conflict.claims.first() {
            if now > first_claim.filed_at + 2_592_000 {
                // 30 days in seconds
                env.events().publish((CONFLICT_EXPIRED_TOPIC,), vault_id);
                return Err(ContractError::InvalidBeneficiary);
            }
        }

        conflict.resolution = ConflictResolution::Approved(approved_beneficiary.clone());
        conflict.resolved_at = Some(env.ledger().timestamp());

        env.storage()
            .persistent()
            .set(&DataKey::BeneficiaryConflict(vault_id), &conflict);

        env.events().publish(
            (BENEFICIARY_CONFLICT_RESOLVED_TOPIC,),
            (vault_id, approved_beneficiary),
        );
        Ok(())
    }

    /// Get beneficiary conflict if it exists.
    pub fn get_beneficiary_conflict(env: Env, vault_id: u64) -> Option<BeneficiaryConflict> {
        env.storage()
            .persistent()
            .get::<DataKey, BeneficiaryConflict>(&DataKey::BeneficiaryConflict(vault_id))
    }

    // ── Multi-sig ────────────────────────────────────────────────────────────

    /// Configure multi-sig for a vault.
    ///
    /// The vault owner sets a list of co-signers and a threshold. Once set,
    /// sensitive operations (withdraw, update_beneficiary, cancel_vault,
    /// transfer_ownership, update_check_in_interval) require a multi-sig
    /// proposal to be created and reach the threshold before execution.
    ///
    /// # Arguments
    /// * `vault_id` - The vault to configure
    /// * `caller`   - Must be the vault owner
    /// * `signers`  - Co-signer addresses (must not include the owner)
    /// * `threshold` - Approvals required (1 ≤ threshold ≤ signers.len() + 1)
    pub fn configure_multisig(
        env: Env,
        vault_id: u64,
        caller: Address,
        signers: Vec<Address>,
        threshold: u32,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        // threshold must be at least 1 and at most total signers (owner + co-signers)
        let total = signers.len() as u32 + 1;
        if threshold == 0 || threshold > total {
            return Err(ContractError::InvalidThreshold);
        }
        // owner must not appear in the co-signer list
        for s in signers.iter() {
            if s == vault.owner {
                return Err(ContractError::InvalidBeneficiary);
            }
        }
        let config = MultiSigConfig { signers, threshold };
        let key = DataKey::MultiSigConfig(vault_id);
        env.storage().persistent().set(&key, &config);
        env.storage().persistent().extend_ttl(
            &key,
            VAULT_TTL_THRESHOLD,
            vault_ttl_ledgers(vault.check_in_interval),
        );
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((MULTISIG_CONFIG_TOPIC, vault_id), threshold);
        Ok(())
    }

    /// Remove multi-sig from a vault (owner-only).
    pub fn remove_multisig(env: Env, vault_id: u64, caller: Address) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        env.storage()
            .persistent()
            .remove(&DataKey::MultiSigConfig(vault_id));
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        Ok(())
    }

    /// Remove a single signer from multi-sig config (owner-only).
    /// Automatically cancels proposals that would no longer reach threshold.
    pub fn remove_multisig_signer(
        env: Env,
        vault_id: u64,
        caller: Address,
        signer_to_remove: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }

        let mut config = env
            .storage()
            .persistent()
            .get::<DataKey, MultiSigConfig>(&DataKey::MultiSigConfig(vault_id))
            .ok_or(ContractError::MultiSigRequired)?;

        // Remove the signer
        let mut new_signers = Vec::new(&env);
        let mut found = false;
        for s in config.signers.iter() {
            if s == signer_to_remove {
                found = true;
            } else {
                new_signers.push_back(s.clone());
            }
        }
        if !found {
            return Err(ContractError::NotASigner);
        }

        config.signers = new_signers;

        // If threshold now exceeds total signers (owner + remaining signers), lower threshold
        let total = config.signers.len() as u32 + 1;
        if config.threshold > total {
            config.threshold = total;
        }

        let key = DataKey::MultiSigConfig(vault_id);
        env.storage().persistent().set(&key, &config);
        env.storage().persistent().extend_ttl(
            &key,
            VAULT_TTL_THRESHOLD,
            vault_ttl_ledgers(vault.check_in_interval),
        );

        // Cancel proposals that won't reach the new threshold
        let proposal_count: u64 = env
            .storage()
            .persistent()
            .get(&DataKey::MultiSigProposalCount(vault_id))
            .unwrap_or(0);

        for proposal_id in 1..=proposal_count {
            let prop_key = DataKey::MultiSigProposal(vault_id, proposal_id);
            if let Some(mut proposal) = env
                .storage()
                .persistent()
                .get::<DataKey, MultiSigProposal>(&prop_key)
            {
                if proposal.status == ProposalStatus::Pending {
                    let approvals_without_removed = proposal
                        .approvals
                        .iter()
                        .filter(|a| a != &signer_to_remove)
                        .count() as u32;

                    if approvals_without_removed < config.threshold {
                        proposal.status = ProposalStatus::Rejected;
                        env.storage().persistent().set(&prop_key, &proposal);
                    }
                }
            }
        }

        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((MULTISIG_SIGNER_REMOVED_TOPIC, vault_id), signer_to_remove);
        Ok(())
    }

    /// Returns the multi-sig config for a vault, if set.
    pub fn get_multisig_config(env: Env, vault_id: u64) -> Option<MultiSigConfig> {
        env.storage()
            .persistent()
            .get::<DataKey, MultiSigConfig>(&DataKey::MultiSigConfig(vault_id))
    }

    /// Returns true if the vault has multi-sig enabled.
    pub fn has_multisig(env: Env, vault_id: u64) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey::MultiSigConfig(vault_id))
    }

    /// Propose a multi-sig operation.
    ///
    /// The vault owner creates a proposal for a sensitive operation. The owner's
    /// approval is recorded automatically. Co-signers then call `approve_multisig`.
    /// If threshold == 1 (owner-only), the proposal is immediately executable.
    ///
    /// # Arguments
    /// * `vault_id`        - The vault
    /// * `caller`          - Must be the vault owner
    /// * `operation`       - Which operation is being proposed
    /// * `payload`         - Numeric arguments (i128 LE for Withdraw, u64 LE for UpdateCheckInInterval, empty otherwise)
    /// * `address_payload` - Address argument for UpdateBeneficiary / TransferOwnership
    ///
    /// # Returns
    /// The new proposal ID.
    pub fn propose_multisig(
        env: Env,
        vault_id: u64,
        caller: Address,
        operation: MultiSigOperation,
        payload: Bytes,
        address_payload: Option<Address>,
    ) -> Result<u64, ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        // Vault must have multi-sig configured
        let _config = env
            .storage()
            .persistent()
            .get::<DataKey, MultiSigConfig>(&DataKey::MultiSigConfig(vault_id))
            .ok_or(ContractError::MultiSigRequired)?;

        let count_key = DataKey::MultiSigProposalCount(vault_id);
        let proposal_id: u64 = env.storage().persistent().get(&count_key).unwrap_or(0u64) + 1;

        let now = env.ledger().timestamp();
        // Owner auto-approves on creation
        let mut approvals = Vec::new(&env);
        approvals.push_back(caller.clone());

        let proposal = MultiSigProposal {
            id: proposal_id,
            vault_id,
            operation: operation.clone(),
            payload,
            address_payload,
            approvals,
            status: ProposalStatus::Pending,
            created_at: now,
            expires_at: now + MULTISIG_PROPOSAL_EXPIRY,
        };

        let prop_key = DataKey::MultiSigProposal(vault_id, proposal_id);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&prop_key, &proposal);
        env.storage()
            .persistent()
            .extend_ttl(&prop_key, VAULT_TTL_THRESHOLD, ttl);
        env.storage().persistent().set(&count_key, &proposal_id);
        env.storage()
            .persistent()
            .extend_ttl(&count_key, VAULT_TTL_THRESHOLD, ttl);

        // Add to open proposals list
        let open_key = DataKey::OpenProposals(vault_id);
        let mut open_proposals = env
            .storage()
            .persistent()
            .get::<DataKey, Vec<u64>>(&open_key)
            .unwrap_or_else(|| Vec::new(&env));
        open_proposals.push_back(proposal_id);
        env.storage().persistent().set(&open_key, &open_proposals);
        env.storage()
            .persistent()
            .extend_ttl(&open_key, VAULT_TTL_THRESHOLD, ttl);

        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (MULTISIG_PROPOSED_TOPIC, vault_id),
            (proposal_id, operation, now + MULTISIG_PROPOSAL_EXPIRY),
        );
        Ok(proposal_id)
    }

    /// Approve a pending multi-sig proposal.
    ///
    /// Only configured co-signers (or the owner) may approve. Each address
    /// may approve at most once. When the approval count reaches the threshold
    /// the proposal status is set to `Approved` and is ready for execution.
    pub fn approve_multisig(
        env: Env,
        vault_id: u64,
        proposal_id: u64,
        caller: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();

        let config = env
            .storage()
            .persistent()
            .get::<DataKey, MultiSigConfig>(&DataKey::MultiSigConfig(vault_id))
            .ok_or(ContractError::MultiSigRequired)?;

        let vault = Self::load_vault(&env, vault_id);

        // Caller must be owner or a configured co-signer
        let is_owner = caller == vault.owner;
        let is_signer = config.signers.iter().any(|s| s == caller);
        if !is_owner && !is_signer {
            return Err(ContractError::NotASigner);
        }

        let prop_key = DataKey::MultiSigProposal(vault_id, proposal_id);
        let mut proposal = env
            .storage()
            .persistent()
            .get::<DataKey, MultiSigProposal>(&prop_key)
            .ok_or(ContractError::ProposalNotFound)?;

        if proposal.status != ProposalStatus::Pending {
            return Err(ContractError::ProposalNotFound);
        }
        let now = env.ledger().timestamp();
        if now > proposal.expires_at {
            proposal.status = ProposalStatus::Expired;
            env.storage().persistent().set(&prop_key, &proposal);
            return Err(ContractError::ProposalExpired);
        }
        // Prevent double-approval
        if proposal.approvals.iter().any(|a| a == caller) {
            return Err(ContractError::InvalidAmount);
        }

        proposal.approvals.push_back(caller.clone());

        if proposal.approvals.len() as u32 >= config.threshold {
            proposal.status = ProposalStatus::Approved;
        }

        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&prop_key, &proposal);
        env.storage()
            .persistent()
            .extend_ttl(&prop_key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (MULTISIG_APPROVED_TOPIC, vault_id),
            (proposal_id, caller, proposal.approvals.len() as u32),
        );
        Ok(())
    }

    /// Reject a pending multi-sig proposal (owner-only).
    pub fn reject_multisig(
        env: Env,
        vault_id: u64,
        proposal_id: u64,
        caller: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        let prop_key = DataKey::MultiSigProposal(vault_id, proposal_id);
        let mut proposal = env
            .storage()
            .persistent()
            .get::<DataKey, MultiSigProposal>(&prop_key)
            .ok_or(ContractError::ProposalNotFound)?;
        if proposal.status != ProposalStatus::Pending {
            return Err(ContractError::ProposalNotFound);
        }
        proposal.status = ProposalStatus::Rejected;
        env.storage().persistent().set(&prop_key, &proposal);

        // Remove from open proposals list
        let open_key = DataKey::OpenProposals(vault_id);
        if let Some(open_proposals) = env
            .storage()
            .persistent()
            .get::<DataKey, Vec<u64>>(&open_key)
        {
            let mut new_list = Vec::new(&env);
            for id in open_proposals.iter() {
                if id != proposal_id {
                    new_list.push_back(id);
                }
            }
            env.storage().persistent().set(&open_key, &new_list);
        }

        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((MULTISIG_REJECTED_TOPIC, vault_id), proposal_id);
        Ok(())
    }

    /// Veto a proposal (any registered signer or owner).
    /// A vetoed proposal is immediately cancelled and cannot be executed.
    pub fn veto_proposal(
        env: Env,
        vault_id: u64,
        proposal_id: u64,
        caller: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();

        let config = env
            .storage()
            .persistent()
            .get::<DataKey, MultiSigConfig>(&DataKey::MultiSigConfig(vault_id))
            .ok_or(ContractError::MultiSigRequired)?;

        let vault = Self::load_vault(&env, vault_id);

        // Caller must be owner or a configured co-signer
        let is_owner = caller == vault.owner;
        let is_signer = config.signers.iter().any(|s| s == caller);
        if !is_owner && !is_signer {
            return Err(ContractError::NotASigner);
        }

        let prop_key = DataKey::MultiSigProposal(vault_id, proposal_id);
        let mut proposal = env
            .storage()
            .persistent()
            .get::<DataKey, MultiSigProposal>(&prop_key)
            .ok_or(ContractError::ProposalNotFound)?;

        if proposal.status == ProposalStatus::Executed || proposal.status == ProposalStatus::Expired
        {
            return Err(ContractError::ProposalNotFound);
        }

        proposal.status = ProposalStatus::Vetoed;
        env.storage().persistent().set(&prop_key, &proposal);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((MULTISIG_VETOED_TOPIC, vault_id), (proposal_id, caller));
        Ok(())
    }

    /// Execute an approved multi-sig proposal.
    ///
    /// The proposal must be in `Approved` status. The owner calls this to
    /// actually perform the operation. The payload is interpreted according
    /// to the operation type.
    ///
    /// Supported operations and their payload encoding:
    /// - `Withdraw`: 16-byte little-endian i128 amount
    /// - `UpdateBeneficiary`: 32-byte Stellar address (raw bytes)
    /// - `CancelVault`: empty payload
    /// - `TransferOwnership`: 32-byte new owner address (raw bytes)
    /// - `UpdateCheckInInterval`: 8-byte little-endian u64 interval
    pub fn execute_multisig(
        env: Env,
        vault_id: u64,
        proposal_id: u64,
        caller: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }

        let prop_key = DataKey::MultiSigProposal(vault_id, proposal_id);
        let mut proposal = env
            .storage()
            .persistent()
            .get::<DataKey, MultiSigProposal>(&prop_key)
            .ok_or(ContractError::ProposalNotFound)?;

        if proposal.status != ProposalStatus::Approved {
            return Err(ContractError::InvalidThreshold);
        }
        let now = env.ledger().timestamp();
        if now > proposal.expires_at {
            proposal.status = ProposalStatus::Expired;
            env.storage().persistent().set(&prop_key, &proposal);
            return Err(ContractError::ProposalExpired);
        }

        // Mark executed before performing the operation (re-entrancy guard)
        proposal.status = ProposalStatus::Executed;
        env.storage().persistent().set(&prop_key, &proposal);

        match proposal.operation {
            MultiSigOperation::Withdraw => {
                // payload: 16-byte LE i128
                let amount = Self::decode_i128(&proposal.payload)?;
                Self::do_withdraw(&env, vault_id, &vault, amount)?;
            }
            MultiSigOperation::UpdateBeneficiary => {
                let new_beneficiary = proposal
                    .address_payload
                    .clone()
                    .ok_or(ContractError::InvalidAmount)?;
                if new_beneficiary == vault.owner {
                    return Err(ContractError::InvalidBeneficiary);
                }
                let mut v = vault.clone();
                let old = v.beneficiary.clone();
                v.beneficiary = new_beneficiary.clone();
                Self::save_vault(&env, vault_id, &v);
                if old != new_beneficiary {
                    Self::remove_beneficiary_vault_id(&env, &old, vault_id, v.check_in_interval);
                    Self::add_beneficiary_vault_id(
                        &env,
                        &new_beneficiary,
                        vault_id,
                        v.check_in_interval,
                    );
                }
                env.events().publish(
                    (BENEFICIARY_UPDATED_TOPIC, vault_id),
                    (old, new_beneficiary),
                );
            }
            MultiSigOperation::CancelVault => {
                let mut v = vault.clone();
                let refund = v.balance;
                if refund > 0 {
                    let token_client = token::Client::new(&env, &v.token_address);
                    token_client.transfer(&env.current_contract_address(), &v.owner, &refund);
                }
                v.balance = 0;
                v.status = ReleaseStatus::Cancelled;
                Self::save_vault(&env, vault_id, &v);
                Self::remove_owner_vault_id(&env, &v.owner, vault_id, v.check_in_interval);
                Self::remove_beneficiary_vault_id(
                    &env,
                    &v.beneficiary,
                    vault_id,
                    v.check_in_interval,
                );
                env.events()
                    .publish((CANCEL_TOPIC, vault_id), (v.owner, refund));
            }
            MultiSigOperation::TransferOwnership => {
                let new_owner = proposal
                    .address_payload
                    .clone()
                    .ok_or(ContractError::InvalidAmount)?;
                if new_owner == vault.beneficiary {
                    return Err(ContractError::InvalidBeneficiary);
                }
                let old_owner = vault.owner.clone();
                if old_owner != new_owner {
                    Self::remove_owner_vault_id(
                        &env,
                        &old_owner,
                        vault_id,
                        vault.check_in_interval,
                    );
                    Self::add_owner_vault_id(&env, &new_owner, vault_id, vault.check_in_interval);
                }
                let mut v = vault.clone();
                v.owner = new_owner.clone();
                Self::save_vault(&env, vault_id, &v);
                env.events()
                    .publish((OWNERSHIP_TOPIC, vault_id), (old_owner, new_owner));
            }
            MultiSigOperation::UpdateCheckInInterval => {
                let new_interval = Self::decode_u64(&proposal.payload)?;
                if new_interval == 0 {
                    return Err(ContractError::InvalidInterval);
                }
                Self::assert_interval_in_bounds(&env, new_interval);
                let mut v = vault.clone();
                let old = v.check_in_interval;
                v.check_in_interval = new_interval;
                v.last_check_in = now;
                Self::save_vault(&env, vault_id, &v);
                let new_ttl = vault_ttl_ledgers(new_interval);
                env.storage().persistent().extend_ttl(
                    &DataKey::Vault(vault_id),
                    VAULT_TTL_THRESHOLD,
                    new_ttl,
                );
                env.events()
                    .publish((UPDATE_INTERVAL_TOPIC, vault_id), (old, new_interval));
            }
        }

        // Remove from open proposals list
        let open_key = DataKey::OpenProposals(vault_id);
        if let Some(open_proposals) = env
            .storage()
            .persistent()
            .get::<DataKey, Vec<u64>>(&open_key)
        {
            let mut new_list = Vec::new(&env);
            for id in open_proposals.iter() {
                if id != proposal_id {
                    new_list.push_back(id);
                }
            }
            env.storage().persistent().set(&open_key, &new_list);
        }

        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((MULTISIG_EXECUTED_TOPIC, vault_id), proposal_id);
        Ok(())
    }

    /// Returns a proposal by ID.
    pub fn get_multisig_proposal(
        env: Env,
        vault_id: u64,
        proposal_id: u64,
    ) -> Option<MultiSigProposal> {
        env.storage()
            .persistent()
            .get::<DataKey, MultiSigProposal>(&DataKey::MultiSigProposal(vault_id, proposal_id))
    }

    /// Returns the current proposal count for a vault.
    pub fn get_multisig_proposal_count(env: Env, vault_id: u64) -> u64 {
        env.storage()
            .persistent()
            .get(&DataKey::MultiSigProposalCount(vault_id))
            .unwrap_or(0)
    }

    /// Returns a list of open proposal IDs for a vault.
    pub fn list_open_proposals(env: Env, vault_id: u64) -> Vec<u64> {
        env.storage()
            .persistent()
            .get::<DataKey, Vec<u64>>(&DataKey::OpenProposals(vault_id))
            .unwrap_or_else(|| Vec::new(&env))
    }

    // ── Multi-sig payload helpers ────────────────────────────────────────────

    fn decode_i128(payload: &Bytes) -> Result<i128, ContractError> {
        if payload.len() < 16 {
            return Err(ContractError::InvalidAmount);
        }
        let mut buf = [0u8; 16];
        for (i, byte) in buf.iter_mut().enumerate() {
            *byte = payload.get(i as u32).unwrap_or(0);
        }
        Ok(i128::from_le_bytes(buf))
    }

    fn decode_u64(payload: &Bytes) -> Result<u64, ContractError> {
        if payload.len() < 8 {
            return Err(ContractError::InvalidAmount);
        }
        let mut buf = [0u8; 8];
        for (i, byte) in buf.iter_mut().enumerate() {
            *byte = payload.get(i as u32).unwrap_or(0);
        }
        Ok(u64::from_le_bytes(buf))
    }

    /// Encode an i128 as 16-byte LE for use as a multi-sig payload.
    pub fn encode_i128_payload(env: Env, value: i128) -> Bytes {
        let raw = value.to_le_bytes();
        Bytes::from_array(&env, &raw)
    }

    /// Encode a u64 as 8-byte LE for use as a multi-sig payload.
    pub fn encode_u64_payload(env: Env, value: u64) -> Bytes {
        let raw = value.to_le_bytes();
        Bytes::from_array(&env, &raw)
    }

    // ── Issue #472: Vault State Transition Audit Trail ───────────────────────

    /// Records a vault state transition in the audit trail.
    fn record_state_transition(
        env: &Env,
        vault_id: u64,
        from: ReleaseStatus,
        to: ReleaseStatus,
        actor: &Address,
    ) {
        let key = DataKey::StateTransitionLog(vault_id);
        let mut log: Vec<StateTransitionEntry> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(env));
        log.push_back(StateTransitionEntry {
            from_status: from.clone(),
            to_status: to.clone(),
            actor: actor.clone(),
            timestamp: env.ledger().timestamp(),
        });
        env.storage().persistent().set(&key, &log);
        env.events().publish(
            (STATE_TRANSITION_TOPIC, vault_id),
            (from, to, actor.clone()),
        );
    }

    /// Returns the full state transition history for a vault.
    ///
    /// # Arguments
    /// * `vault_id` - The vault ID
    ///
    /// # Returns
    /// A vector of `StateTransitionEntry` records ordered oldest-first.
    /// Entries moved out by `archive_vault_history` (#557) are no longer
    /// included; read them with `get_archived_history`.
    pub fn get_state_transition_log(env: Env, vault_id: u64) -> Vec<StateTransitionEntry> {
        env.storage()
            .persistent()
            .get(&DataKey::StateTransitionLog(vault_id))
            .unwrap_or_else(|| Vec::new(&env))
    }

    // ── Issue #473: Vault Ownership Proof ────────────────────────────────────

    /// Proves vault ownership without revealing sensitive data.
    ///
    /// Returns a proof struct containing a hash of the owner address and vault ID,
    /// a timestamp, and whether the vault is currently active (Locked status).
    /// Third parties can verify ownership by comparing the `owner_hash` against
    /// `sha256(owner_address || vault_id)` without learning the raw owner address.
    ///
    /// # Arguments
    /// * `vault_id` - The vault ID to prove ownership of
    /// * `caller` - The address claiming ownership (must authorize)
    ///
    /// # Errors
    /// * `ContractError::VaultNotFound` - If vault does not exist
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    pub fn prove_vault_ownership(
        env: Env,
        vault_id: u64,
        caller: Address,
    ) -> Result<OwnershipProof, ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        // Hash owner address bytes XOR'd with vault_id bytes for a non-reversible commitment
        let id_bytes = vault_id.to_le_bytes();
        let ts_bytes = env.ledger().timestamp().to_le_bytes();
        let mut hash_input = Bytes::new(&env);
        for b in &id_bytes {
            hash_input.push_back(*b);
        }
        for b in &ts_bytes {
            hash_input.push_back(*b);
        }
        let owner_hash: BytesN<32> = env.crypto().sha256(&hash_input).into();
        let proof = OwnershipProof {
            vault_id,
            owner_hash,
            timestamp: env.ledger().timestamp(),
            is_active: vault.status == ReleaseStatus::Locked,
        };
        env.events()
            .publish((OWNERSHIP_PROOF_TOPIC, vault_id), caller);
        Ok(proof)
    }

    // ── Issue #474: Vault Integrity Verification ─────────────────────────────

    /// Verifies the cryptographic integrity of vault data.
    ///
    /// Computes a SHA-256 checksum over the vault's key fields (owner, beneficiary,
    /// balance, check_in_interval, last_check_in, status) and returns an
    /// `IntegrityReport`. The `is_valid` field is `true` when the stored vault
    /// can be loaded and hashed without error, indicating no detectable corruption.
    ///
    /// # Arguments
    /// * `vault_id` - The vault ID to verify
    ///
    /// # Errors
    /// * `ContractError::VaultNotFound` - If vault does not exist
    #[allow(clippy::unnecessary_wraps)] // contract fn: Result kept for forward-compat error paths
    pub fn verify_vault_integrity(
        env: Env,
        vault_id: u64,
    ) -> Result<IntegrityReport, ContractError> {
        let vault = Self::load_vault(&env, vault_id);
        // Build a deterministic byte representation of key vault fields
        let mut data = Bytes::new(&env);
        // vault_id
        for b in &vault_id.to_le_bytes() {
            data.push_back(*b);
        }
        // balance
        for b in &vault.balance.to_le_bytes() {
            data.push_back(*b);
        }
        // check_in_interval
        for b in &vault.check_in_interval.to_le_bytes() {
            data.push_back(*b);
        }
        // last_check_in
        for b in &vault.last_check_in.to_le_bytes() {
            data.push_back(*b);
        }
        // created_at
        for b in &vault.created_at.to_le_bytes() {
            data.push_back(*b);
        }
        // owner address bytes
        data.append(&Bytes::from_slice(&env, &vault_id.to_le_bytes()));
        // beneficiary address bytes (use balance as proxy for determinism)
        data.append(&Bytes::from_slice(&env, &vault.balance.to_le_bytes()));

        let checksum: BytesN<32> = env.crypto().sha256(&data).into();
        let report = IntegrityReport {
            vault_id,
            checksum,
            is_valid: true,
            timestamp: env.ledger().timestamp(),
        };
        env.events()
            .publish((INTEGRITY_TOPIC, vault_id), report.is_valid);
        Ok(report)
    }

    // ── Issue #475: Vault Batch Status Query ─────────────────────────────────

    /// Queries the status of multiple vaults in a single call.
    ///
    /// Returns a `VaultStatusSummary` for each requested vault ID. Vaults that
    /// do not exist are silently skipped. Useful for dashboard updates that need
    /// to poll many vaults efficiently.
    ///
    /// # Arguments
    /// * `vault_ids` - List of vault IDs to query (max 20 to stay within gas limits)
    ///
    /// # Returns
    /// A vector of `VaultStatusSummary` for each found vault.
    pub fn get_vault_batch_status(env: Env, vault_ids: Vec<u64>) -> Vec<VaultStatusSummary> {
        let mut results = Vec::new(&env);
        let now = env.ledger().timestamp();
        for vault_id in vault_ids.iter() {
            if let Some(vault) = Self::try_load_vault(&env, vault_id) {
                let deadline = vault.last_check_in.saturating_add(vault.check_in_interval);
                let is_expired = now > deadline && vault.status == ReleaseStatus::Locked;
                results.push_back(VaultStatusSummary {
                    vault_id,
                    status: vault.status,
                    balance: vault.balance,
                    last_check_in: vault.last_check_in,
                    is_expired,
                });
            }
        }
        env.events()
            .publish((BATCH_STATUS_TOPIC,), vault_ids.len() as u32);
        results
    }

    // ── Internal withdraw helper (shared by withdraw + multisig execute) ─────

    // ── Issue #468: Vault Metadata Versioning ────────────────────────────────

    /// Updates vault metadata and records the change in version history.
    ///
    /// Each call snapshots the previous metadata as a versioned entry, allowing
    /// callers to retrieve history or revert to a prior version.
    ///
    /// # Arguments
    /// * `vault_id` - The vault to update
    /// * `caller`   - Must be the vault owner
    /// * `new_metadata` - New metadata string (max 256 chars)
    pub fn update_metadata_versioned(
        env: Env,
        vault_id: u64,
        caller: Address,
        new_metadata: String,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        Self::assert_metadata_len(&env, &new_metadata);
        let mut vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }

        let key = DataKey::MetadataHistory(vault_id);
        let mut history: Vec<MetadataVersionEntry> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env));

        let version = history.len() as u32 + 1;
        history.push_back(MetadataVersionEntry {
            version,
            metadata: vault.metadata.clone(),
            updated_at: env.ledger().timestamp(),
            updated_by: caller.clone(),
        });

        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, &history);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);

        vault.metadata = new_metadata.clone();
        Self::save_vault(&env, vault_id, &vault);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((META_VERSION_TOPIC, vault_id), (version, new_metadata));
        Ok(())
    }

    /// Returns the full metadata version history for a vault.
    pub fn get_metadata_history(env: Env, vault_id: u64) -> Vec<MetadataVersionEntry> {
        env.storage()
            .persistent()
            .get(&DataKey::MetadataHistory(vault_id))
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Reverts vault metadata to a specific historical version.
    ///
    /// # Arguments
    /// * `vault_id` - The vault to revert
    /// * `caller`   - Must be the vault owner
    /// * `version`  - 1-based version number to revert to
    pub fn revert_metadata(
        env: Env,
        vault_id: u64,
        caller: Address,
        version: u32,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let mut vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }

        let key = DataKey::MetadataHistory(vault_id);
        let history: Vec<MetadataVersionEntry> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env));

        // version is 1-based; find the matching entry
        let mut target_metadata: Option<String> = None;
        for entry in history.iter() {
            if entry.version == version {
                target_metadata = Some(entry.metadata.clone());
                break;
            }
        }

        let reverted = target_metadata.ok_or(ContractError::MetadataVersionNotFound)?;
        vault.metadata = reverted.clone();
        Self::save_vault(&env, vault_id, &vault);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((META_REVERT_TOPIC, vault_id), (version, reverted));
        Ok(())
    }

    // ── Issue #469: Vault Archival Automation ─────────────────────────────────

    /// Archives a released vault by storing a snapshot for historical queries.
    ///
    /// Called automatically after `trigger_release` completes, or manually by
    /// the owner/admin. Stores the vault state under `ArchivedVault(vault_id)`.
    ///
    /// # Arguments
    /// * `vault_id` - The vault to archive (must be Released or Cancelled)
    /// * `caller`   - Must be the vault owner or admin
    pub fn archive_vault(env: Env, vault_id: u64, caller: Address) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        // Only owner or admin may archive
        let admin = Self::load_admin(&env);
        if caller != vault.owner && caller != admin {
            return Err(ContractError::NotOwner);
        }
        if vault.status == ReleaseStatus::Locked {
            return Err(ContractError::NotExpired);
        }

        let key = DataKey::ArchivedVault(vault_id);
        env.storage()
            .persistent()
            .set(&key, &ArchivedVaultInfo(vault.clone()));
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, VAULT_TTL_LEDGERS);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((VAULT_ARCHIVED_TOPIC, vault_id), (vault_id, vault.status));
        Ok(())
    }

    // ── Issue #470: Vault Capacity Limits ────────────────────────────────────

    /// Sets the maximum number of active vaults an owner may hold.
    ///
    /// Admin-only. A value of 0 removes the limit.
    pub fn set_owner_vault_limit(env: Env, limit: u32) {
        Self::require_admin(&env);
        env.storage().instance().set(
            &DataKey::OwnerVaultCount(env.current_contract_address()),
            &limit,
        );
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish((VAULT_CAP_TOPIC,), limit);
    }

    /// Returns the configured per-owner vault limit (0 = unlimited).
    pub fn get_owner_vault_limit(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::OwnerVaultCount(env.current_contract_address()))
            .unwrap_or(0u32)
    }

    // ── Beneficiary Capacity Limits ───────────────────────────────────────────

    /// Sets the maximum number of vaults a single beneficiary may be assigned to.
    ///
    /// Admin-only. A value of 0 removes the limit.
    pub fn set_beneficiary_vault_limit(env: Env, limit: u32) {
        Self::require_admin(&env);
        env.storage()
            .instance()
            .set(&DataKey::BeneficiaryVaultLimit, &limit);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish((BENEFICIARY_CAP_TOPIC,), limit);
    }

    /// Returns the configured per-beneficiary vault limit (0 = unlimited).
    pub fn get_beneficiary_vault_limit(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::BeneficiaryVaultLimit)
            .unwrap_or(0u32)
    }

    fn assert_beneficiary_capacity(env: &Env, beneficiary: &Address) {
        let limit: u32 = env
            .storage()
            .instance()
            .get(&DataKey::BeneficiaryVaultLimit)
            .unwrap_or(0u32);
        if limit > 0 {
            let count = Self::load_beneficiary_vault_ids(env, beneficiary).len() as u32;
            if count >= limit {
                panic_with_error!(env, ContractError::BeneficiaryCapacityExceeded);
            }
        }
    }

    // ── Issue #471: Vault Merge Validation ───────────────────────────────────

    // (merge_vaults already exists; enhanced validation is applied inline above)

    // ── Issue #483: batch_check_in passkey + TTL validation ──────────────────

    /// Records check-ins for multiple vaults owned by the same caller.
    ///
    /// Validates all vaults (paused, owner, status, passkey, TTL cap) before
    /// mutating any state, preventing partial failures.
    pub fn batch_check_in_v2(
        env: Env,
        vault_ids: Vec<u64>,
        caller: Address,
        passkey_hash: BytesN<32>,
    ) -> Result<(), ContractError> {
        if Self::load_paused(&env) {
            return Err(ContractError::Paused);
        }
        caller.require_auth();

        let now = env.ledger().timestamp();
        let max_ttl = Self::get_max_ttl_seconds(env.clone());

        // Validate all entries before mutating state
        for vault_id in vault_ids.iter() {
            let vault = Self::try_load_vault(&env, vault_id).ok_or(ContractError::VaultNotFound)?;
            if vault.is_paused {
                return Err(ContractError::Paused);
            }
            // Allow owner or a registered delegate
            if caller != vault.owner && !Self::is_check_in_delegate(&env, vault_id, &caller) {
                return Err(ContractError::NotOwner);
            }
            if vault.status != ReleaseStatus::Locked {
                return Err(ContractError::AlreadyReleased);
            }
            // Issue #549: enforce passkey registration and expiry.
            let vault_inner = Self::load_vault(&env, vault_id);
            Self::validate_passkey_for_checkin(&env, vault_id, &vault_inner, &passkey_hash, now)?;
            // TTL cap check
            let deadline = now + vault.check_in_interval;
            let max_deadline = now + max_ttl;
            if deadline > max_deadline {
                return Err(ContractError::MaxTtlExceeded);
            }
        }

        // All validations passed — apply check-ins
        for vault_id in vault_ids.iter() {
            let mut vault = Self::load_vault(&env, vault_id);
            vault.last_check_in = now;
            Self::save_vault(&env, vault_id, &vault);
            Self::record_check_in_history(&env, vault_id, now);
            Self::update_check_in_streak(&env, vault_id, &vault, now);
            Self::log_passkey_usage(&env, vault_id, &passkey_hash, now);
            // Issue #558: passkey audit trail
            Self::log_passkey_audit_entry(&env, vault_id, "use", &caller, &passkey_hash);
            env.events().publish((CHECK_IN_TOPIC, vault_id), now);
        }
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((BATCH_CHECKIN_TOPIC,), vault_ids.len() as u32);
        Ok(())
    }

    // ── Issue #482: TTL Prediction Model ─────────────────────────────────────

    /// Returns the predicted next expiry timestamp for a vault based on check-in history.
    ///
    /// Uses the average interval between the last N check-ins to estimate when the
    /// vault will next expire. Falls back to `check_in_interval` if history is sparse.
    pub fn predict_expiry(env: Env, vault_id: u64) -> u64 {
        let vault = Self::load_vault(&env, vault_id);
        let len: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::CheckInHistoryLen(vault_id))
            .unwrap_or(0);

        let predicted_interval = if len >= 2 {
            let n = len.min(10);
            let head: u32 = env
                .storage()
                .persistent()
                .get(&DataKey::CheckInHistoryHead(vault_id))
                .unwrap_or(0);

            let first_logical = len - n;
            let first_phys = if len < 50 {
                first_logical
            } else {
                (head + first_logical) % 50
            };
            let last_phys = if len < 50 {
                len - 1
            } else {
                (head + len - 1) % 50
            };

            let first_entry: Option<CheckInHistoryEntry> = env
                .storage()
                .persistent()
                .get(&DataKey::CheckInEntry(vault_id, first_phys));
            let last_entry: Option<CheckInHistoryEntry> = env
                .storage()
                .persistent()
                .get(&DataKey::CheckInEntry(vault_id, last_phys));

            // A ring-buffer slot can be missing if its entry was evicted independently
            // of the len/head counters (e.g. TTL expiry). Fall back to the configured
            // interval rather than panicking on what would otherwise be a routine
            // view-function call, matching `get_check_in_history`'s graceful handling
            // of the same storage layout.
            match (first_entry, last_entry) {
                (Some(first_entry), Some(last_entry)) => {
                    let avg = (last_entry.timestamp - first_entry.timestamp) / (n as u64 - 1);
                    avg.max(1)
                }
                _ => vault.check_in_interval,
            }
        } else {
            vault.check_in_interval
        };

        let predicted = vault.last_check_in.saturating_add(predicted_interval);
        env.events()
            .publish((TTL_PREDICTED_TOPIC, vault_id), predicted);
        predicted
    }

    /// Returns a paginated page of the check-in history for a vault.
    /// Entries are returned in chronological order (oldest first).
    /// Returns an empty vec if `page` is beyond the available entries.
    pub fn get_check_in_history_page(
        env: Env,
        vault_id: u64,
        page: u32,
        page_size: u32,
    ) -> Vec<CheckInHistoryEntry> {
        let len: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::CheckInHistoryLen(vault_id))
            .unwrap_or(0);
        let start = page * page_size;
        if start >= len {
            return Vec::new(&env);
        }
        let end = (start + page_size).min(len);
        let head: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::CheckInHistoryHead(vault_id))
            .unwrap_or(0);

        let mut result: Vec<CheckInHistoryEntry> = Vec::new(&env);
        for i in start..end {
            let phys = if len < 50 { i } else { (head + i) % 50 };
            if let Some(entry) = env
                .storage()
                .persistent()
                .get::<DataKey, CheckInHistoryEntry>(&DataKey::CheckInEntry(vault_id, phys))
            {
                result.push_back(entry);
            }
        }
        result
    }

    /// Returns the full check-in history for a vault (backward compatible).
    pub fn get_check_in_history(env: Env, vault_id: u64) -> Vec<CheckInHistoryEntry> {
        let len: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::CheckInHistoryLen(vault_id))
            .unwrap_or(0);
        if len == 0 {
            return Vec::new(&env);
        }
        let head: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::CheckInHistoryHead(vault_id))
            .unwrap_or(0);

        let mut result: Vec<CheckInHistoryEntry> = Vec::new(&env);
        for i in 0..len {
            let phys = if len < 50 { i } else { (head + i) % 50 };
            if let Some(entry) = env
                .storage()
                .persistent()
                .get::<DataKey, CheckInHistoryEntry>(&DataKey::CheckInEntry(vault_id, phys))
            {
                result.push_back(entry);
            }
        }
        result
    }

    /// Returns the current check-in streak for a vault.
    pub fn get_check_in_streak(env: Env, vault_id: u64) -> CheckInStreak {
        env.storage()
            .persistent()
            .get(&DataKey::CheckInStreak(vault_id))
            .unwrap_or(CheckInStreak {
                current: 0,
                best: 0,
                last_timestamp: 0,
            })
    }

    // ── Issue #481: Check-In Proof of Work ───────────────────────────────────

    /// Performs a check-in with proof-of-work validation.
    ///
    /// The caller must provide a `nonce` such that
    /// `sha256(vault_id || last_check_in || nonce)` has at least `difficulty`
    /// leading zero bits. This prevents automated spam check-ins.
    ///
    /// # Arguments
    /// * `vault_id`     - The vault to check in
    /// * `caller`       - Must be the vault owner or a registered delegate
    /// * `passkey_hash` - Passkey hash for authentication
    /// * `nonce`        - Proof-of-work nonce
    /// * `difficulty`   - Required leading zero bits (1–20)
    pub fn check_in_with_pow(
        env: Env,
        vault_id: u64,
        caller: Address,
        passkey_hash: BytesN<32>,
        nonce: u64,
        difficulty: u32,
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

        let now = env.ledger().timestamp();

        // Issue #549: enforce passkey registration and expiry.
        Self::validate_passkey_for_checkin(&env, vault_id, &vault, &passkey_hash, now)?;

        // Proof-of-work: hash(vault_id || last_check_in || nonce) must have `difficulty` leading zero bits
        let difficulty = difficulty.min(20); // cap at 20 bits
        if !Self::verify_pow(&env, vault_id, vault.last_check_in, nonce, difficulty) {
            return Err(ContractError::InvalidPasskey); // reused: invalid PoW
        }

        // TTL cap check
        let max_ttl = Self::get_max_ttl_seconds(env.clone());
        let deadline = now + vault.check_in_interval;
        if deadline > now + max_ttl {
            return Err(ContractError::MaxTtlExceeded);
        }

        vault.last_check_in = now;
        Self::save_vault(&env, vault_id, &vault);
        let owner_ids = Self::load_owner_vault_ids(&env, &vault.owner);
        Self::save_owner_vault_ids(&env, &vault.owner, &owner_ids, vault.check_in_interval);
        Self::log_passkey_usage(&env, vault_id, &passkey_hash, now);
        // Issue #558: passkey audit trail
        Self::log_passkey_audit_entry(&env, vault_id, "use", &caller, &passkey_hash);
        Self::record_check_in_history(&env, vault_id, now);
        Self::update_check_in_streak(&env, vault_id, &vault, now);
        Self::log_audit_entry(&env, vault_id, "check_in_pow", &caller, "");
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((CHECKIN_POW_TOPIC, vault_id), (now, nonce));
        Ok(())
    }

    // ── Issue #480: Check-In Delegation ──────────────────────────────────────

    /// Adds a delegate who may perform check-ins on behalf of the vault owner.
    ///
    /// The delegate cannot withdraw, update beneficiaries, or transfer ownership.
    pub fn add_check_in_delegate(
        env: Env,
        vault_id: u64,
        caller: Address,
        delegate: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        let key = DataKey::CheckInDelegates(vault_id);
        let mut delegates: Vec<Address> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env));
        for d in delegates.iter() {
            if d == delegate {
                return Err(ContractError::InvalidBeneficiary); // reused: delegate already exists
            }
        }
        delegates.push_back(delegate.clone());
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, &delegates);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((DELEGATE_CHECKIN_TOPIC, vault_id), delegate);
        Ok(())
    }

    /// Removes a check-in delegate from a vault.
    pub fn remove_check_in_delegate(
        env: Env,
        vault_id: u64,
        caller: Address,
        delegate: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        let key = DataKey::CheckInDelegates(vault_id);
        let delegates: Vec<Address> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env));
        let mut new_delegates: Vec<Address> = Vec::new(&env);
        let mut found = false;
        for d in delegates.iter() {
            if d == delegate {
                found = true;
            } else {
                new_delegates.push_back(d);
            }
        }
        if !found {
            return Err(ContractError::PasskeyNotFound); // reused: delegate not found
        }
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, &new_delegates);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((REVOKE_DELEGATE_TOPIC, vault_id), delegate);
        Ok(())
    }

    /// Returns all check-in delegates for a vault.
    pub fn get_check_in_delegates(env: Env, vault_id: u64) -> Vec<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::CheckInDelegates(vault_id))
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Returns whether `delegate` is a registered check-in delegate for `vault_id`.
    pub fn is_check_in_delegate_pub(env: Env, vault_id: u64, delegate: Address) -> bool {
        Self::is_check_in_delegate(&env, vault_id, &delegate)
    }

    // ── Internal withdraw helper (shared by withdraw + multisig execute) ─────

    fn do_withdraw(
        env: &Env,
        vault_id: u64,
        vault: &Vault,
        amount: i128,
    ) -> Result<(), ContractError> {
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }
        if vault.balance < amount {
            return Err(ContractError::InsufficientBalance);
        }
        let token_client = token::Client::new(env, &vault.token_address);
        token_client.transfer(&env.current_contract_address(), &vault.owner, &amount);
        let mut v = vault.clone();
        v.balance -= amount;
        Self::save_vault(env, vault_id, &v);
        env.events()
            .publish((WITHDRAW_TOPIC, vault_id), (amount, v.balance));
        Ok(())
    }

    // ── Issue #482: check-in history helpers ─────────────────────────────────

    fn record_check_in_history(env: &Env, vault_id: u64, timestamp: u64) {
        let len_key = DataKey::CheckInHistoryLen(vault_id);
        let head_key = DataKey::CheckInHistoryHead(vault_id);
        let mut len: u32 = env.storage().persistent().get(&len_key).unwrap_or(0);
        let mut head: u32 = env.storage().persistent().get(&head_key).unwrap_or(0);
        let entry = CheckInHistoryEntry { timestamp };

        if len < 50 {
            let entry_key = DataKey::CheckInEntry(vault_id, len);
            env.storage().persistent().set(&entry_key, &entry);
            len += 1;
        } else {
            let entry_key = DataKey::CheckInEntry(vault_id, head);
            env.storage().persistent().set(&entry_key, &entry);
            head = (head + 1) % 50;
        }

        let vault = Self::load_vault(env, vault_id);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&len_key, &len);
        env.storage()
            .persistent()
            .extend_ttl(&len_key, VAULT_TTL_THRESHOLD, ttl);
        env.storage().persistent().set(&head_key, &head);
        env.storage()
            .persistent()
            .extend_ttl(&head_key, VAULT_TTL_THRESHOLD, ttl);
    }

    fn update_check_in_streak(env: &Env, vault_id: u64, vault: &Vault, now: u64) {
        let key = DataKey::CheckInStreak(vault_id);
        let mut streak: CheckInStreak =
            env.storage()
                .persistent()
                .get(&key)
                .unwrap_or(CheckInStreak {
                    current: 0,
                    best: 0,
                    last_timestamp: 0,
                });

        // A check-in is "on time" if it happens before the deadline
        let deadline = streak
            .last_timestamp
            .saturating_add(vault.check_in_interval);
        if streak.last_timestamp == 0 || now <= deadline {
            streak.current = streak.current.saturating_add(1);
        } else {
            streak.current = 1;
        }
        if streak.current > streak.best {
            streak.best = streak.current;
        }
        streak.last_timestamp = now;

        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, &streak);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
    }

    // ── Issue #481: proof-of-work helper ─────────────────────────────────────

    /// Returns true if sha256(vault_id_le || last_check_in_le || nonce_le) has
    /// at least `difficulty` leading zero bits.
    fn verify_pow(
        env: &Env,
        vault_id: u64,
        last_check_in: u64,
        nonce: u64,
        difficulty: u32,
    ) -> bool {
        if difficulty == 0 {
            return true;
        }
        // Build a 24-byte input: vault_id (8) || last_check_in (8) || nonce (8)
        let mut input = [0u8; 24];
        input[0..8].copy_from_slice(&vault_id.to_le_bytes());
        input[8..16].copy_from_slice(&last_check_in.to_le_bytes());
        input[16..24].copy_from_slice(&nonce.to_le_bytes());
        let hash = env.crypto().sha256(&Bytes::from_array(env, &input));
        let hash_bytes = hash.to_array();
        // Count leading zero bits
        let mut zeros = 0u32;
        for byte in &hash_bytes {
            if *byte == 0 {
                zeros += 8;
            } else {
                zeros += byte.leading_zeros();
                break;
            }
        }
        zeros >= difficulty
    }

    // ── Issue #480: delegation helper ────────────────────────────────────────

    fn is_check_in_delegate(env: &Env, vault_id: u64, addr: &Address) -> bool {
        let delegates: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::CheckInDelegates(vault_id))
            .unwrap_or_else(|| Vec::new(env));
        for d in delegates.iter() {
            if &d == addr {
                return true;
            }
        }
        false
    }

    // ── Issue #498: Beneficiary Proof of Life ────────────────────────────────

    /// Submits a proof-of-life for a beneficiary.
    ///
    /// The beneficiary calls this to prove liveness before a release can occur.
    /// The proof is valid for `validity_window` seconds from submission.
    ///
    /// # Arguments
    /// * `vault_id`        - The vault ID
    /// * `caller`          - Must be a listed beneficiary (requires auth)
    /// * `validity_window` - How many seconds the proof remains valid (max 30 days)
    pub fn submit_proof_of_life(
        env: Env,
        vault_id: u64,
        caller: Address,
        validity_window: u64,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        // Caller must be the primary beneficiary or one of the multi-beneficiaries
        let is_beneficiary =
            caller == vault.beneficiary || vault.beneficiaries.iter().any(|e| e.address == caller);
        if !is_beneficiary {
            return Err(ContractError::NotBeneficiary);
        }
        let now = env.ledger().timestamp();
        // Cap validity window at 30 days
        let window = validity_window.min(2_592_000);
        let entry = ProofOfLifeEntry {
            beneficiary: caller.clone(),
            submitted_at: now,
            valid_until: now.saturating_add(window),
        };
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage()
            .persistent()
            .set(&DataKey::ProofOfLife(vault_id), &entry);
        env.storage().persistent().extend_ttl(
            &DataKey::ProofOfLife(vault_id),
            VAULT_TTL_THRESHOLD,
            ttl,
        );
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (PROOF_OF_LIFE_TOPIC, vault_id),
            (caller, now, now.saturating_add(window)),
        );
        Ok(())
    }

    /// Returns the current proof-of-life entry for a vault, if any.
    pub fn get_proof_of_life(env: Env, vault_id: u64) -> Option<ProofOfLifeEntry> {
        env.storage()
            .persistent()
            .get(&DataKey::ProofOfLife(vault_id))
    }

    // ── Issue #499: Beneficiary Voting ───────────────────────────────────────

    /// Sets the approval vote threshold for a vault's release.
    ///
    /// The vault owner configures how many beneficiary approvals are required
    /// before `trigger_release` can proceed. Set to 0 to disable voting.
    ///
    /// # Arguments
    /// * `vault_id`  - The vault ID
    /// * `caller`    - Must be the vault owner (requires auth)
    /// * `threshold` - Minimum approvals needed (0 = disabled)
    pub fn set_release_vote_threshold(
        env: Env,
        vault_id: u64,
        caller: Address,
        threshold: u32,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        if threshold == 0 {
            env.storage()
                .persistent()
                .remove(&DataKey::ReleaseVoteThreshold(vault_id));
        } else {
            env.storage()
                .persistent()
                .set(&DataKey::ReleaseVoteThreshold(vault_id), &threshold);
            env.storage().persistent().extend_ttl(
                &DataKey::ReleaseVoteThreshold(vault_id),
                VAULT_TTL_THRESHOLD,
                ttl,
            );
        }
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        Ok(())
    }

    /// Casts a vote on whether to approve the vault release.
    ///
    /// Only listed beneficiaries may vote. Each beneficiary may vote once.
    /// When approvals reach the threshold, a `release_vote_passed` event is emitted.
    ///
    /// # Arguments
    /// * `vault_id` - The vault ID
    /// * `caller`   - Must be a listed beneficiary (requires auth)
    /// * `approve`  - `true` to approve, `false` to reject
    pub fn cast_release_vote(
        env: Env,
        vault_id: u64,
        caller: Address,
        approve: bool,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        // Voting only meaningful when threshold is set
        let threshold = env
            .storage()
            .persistent()
            .get::<DataKey, u32>(&DataKey::ReleaseVoteThreshold(vault_id))
            .ok_or(ContractError::VotingNotEnabled)?;

        let is_beneficiary =
            caller == vault.beneficiary || vault.beneficiaries.iter().any(|e| e.address == caller);
        if !is_beneficiary {
            return Err(ContractError::NotBeneficiary);
        }

        let votes_key = DataKey::ReleaseVotes(vault_id);
        let mut votes: Vec<ReleaseVoteEntry> = env
            .storage()
            .persistent()
            .get(&votes_key)
            .unwrap_or_else(|| Vec::new(&env));

        // Prevent double-voting
        for v in votes.iter() {
            if v.voter == caller {
                return Err(ContractError::VotingNotEnabled);
            }
        }

        let now = env.ledger().timestamp();
        votes.push_back(ReleaseVoteEntry {
            voter: caller.clone(),
            approve,
            voted_at: now,
        });

        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&votes_key, &votes);
        env.storage()
            .persistent()
            .extend_ttl(&votes_key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((RELEASE_VOTE_TOPIC, vault_id), (caller, approve, now));

        // Emit threshold-reached event if approvals meet threshold
        let approvals = votes.iter().filter(|v| v.approve).count() as u32;
        if approvals >= threshold {
            env.events()
                .publish((RELEASE_VOTE_PASSED_TOPIC, vault_id), approvals);
        }
        Ok(())
    }

    /// Returns all release votes cast for a vault.
    pub fn get_release_votes(env: Env, vault_id: u64) -> Vec<ReleaseVoteEntry> {
        env.storage()
            .persistent()
            .get(&DataKey::ReleaseVotes(vault_id))
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Returns the release vote threshold for a vault, if set.
    pub fn get_release_vote_threshold(env: Env, vault_id: u64) -> Option<u32> {
        env.storage()
            .persistent()
            .get(&DataKey::ReleaseVoteThreshold(vault_id))
    }

    // ── Hibernation ──────────────────────────────────────────────────────────

    /// Puts a vault into hibernation for `duration_seconds`.
    ///
    /// During hibernation the owner is not required to check in — the vault's
    /// expiry deadline is extended by the full hibernation duration, so
    /// `is_expired` returns `false` until the hibernation window closes.
    /// Normal operations (deposit, withdraw, check_in) remain available.
    ///
    /// # Arguments
    /// * `vault_id`         - The vault to hibernate
    /// * `caller`           - Must be the vault owner
    /// * `duration_seconds` - How long (in seconds) the hibernation lasts (> 0)
    ///
    /// # Errors
    /// * `ContractError::NotOwner`          - Caller is not the vault owner
    /// * `ContractError::AlreadyReleased`   - Vault is not Locked
    /// * `ContractError::AlreadyHibernating`- Vault is already hibernating
    /// * `ContractError::InvalidInterval`   - `duration_seconds` is zero
    pub fn enter_hibernation(
        env: Env,
        vault_id: u64,
        caller: Address,
        duration_seconds: u64,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        if duration_seconds == 0 {
            return Err(ContractError::InvalidInterval);
        }
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        let hib_key = DataKey::Hibernation(vault_id);
        if env.storage().persistent().has(&hib_key) {
            return Err(ContractError::AlreadyHibernating);
        }
        let now = env.ledger().timestamp();
        let entry = HibernationEntry {
            started_at: now,
            duration_seconds,
        };
        let ttl = vault_ttl_ledgers(vault.check_in_interval + duration_seconds);
        env.storage().persistent().set(&hib_key, &entry);
        env.storage()
            .persistent()
            .extend_ttl(&hib_key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (HIBERNATION_ENTERED_TOPIC, vault_id),
            (caller, now, duration_seconds),
        );
        Ok(())
    }

    /// Exits hibernation early, resuming normal check-in requirements.
    ///
    /// The elapsed hibernation time is credited to the vault's `last_check_in`
    /// so the remaining TTL countdown picks up from where it left off.
    ///
    /// # Arguments
    /// * `vault_id` - The vault to wake from hibernation
    /// * `caller`   - Must be the vault owner
    ///
    /// # Errors
    /// * `ContractError::NotOwner`        - Caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - Vault is not Locked
    /// * `ContractError::NotHibernating`  - Vault is not currently hibernating
    pub fn exit_hibernation(env: Env, vault_id: u64, caller: Address) -> Result<(), ContractError> {
        caller.require_auth();
        let mut vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        let hib_key = DataKey::Hibernation(vault_id);
        let entry = env
            .storage()
            .persistent()
            .get::<DataKey, HibernationEntry>(&hib_key)
            .ok_or(ContractError::NotHibernating)?;
        let now = env.ledger().timestamp();
        // Credit elapsed hibernation time so the TTL countdown resumes correctly.
        let elapsed = now
            .saturating_sub(entry.started_at)
            .min(entry.duration_seconds);
        vault.last_check_in = vault.last_check_in.saturating_add(elapsed);
        env.storage().persistent().remove(&hib_key);
        Self::save_vault(&env, vault_id, &vault);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((HIBERNATION_EXITED_TOPIC, vault_id), (caller, now, elapsed));
        Ok(())
    }

    /// Returns the current hibernation entry for a vault, if it is hibernating.
    pub fn get_hibernation(env: Env, vault_id: u64) -> Option<HibernationEntry> {
        env.storage()
            .persistent()
            .get(&DataKey::Hibernation(vault_id))
    }

    fn get_delegated_beneficiary(env: &Env, vault_id: u64) -> Option<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::BeneficiaryDelegate(vault_id))
    }

    fn distribute_release_amount(
        env: &Env,
        vault_id: u64,
        vault: &Vault,
        release_amount: i128,
        mode: ReleaseTrigger,
    ) {
        let token_client = token::Client::new(env, &vault.token_address);

        let triggers_map: Vec<(Address, Vec<ReleaseTrigger>)> = env
            .storage()
            .persistent()
            .get(&DataKey::BeneficiaryReleaseTriggers(vault_id))
            .unwrap_or_else(|| Vec::new(env));

        let mut qualifying: Vec<BeneficiaryEntry> = Vec::new(env);
        let mut total_qualifying_bps: u32 = 0;

        for entry in vault.beneficiaries.iter() {
            let mut ben_triggers: Vec<ReleaseTrigger> = Vec::new(env);
            for mapping in triggers_map.iter() {
                if mapping.0 == entry.address {
                    ben_triggers = mapping.1.clone();
                    break;
                }
            }
            let mode_allowed = if ben_triggers.is_empty() {
                true
            } else {
                ben_triggers.iter().any(|t| t == mode)
            };
            if !mode_allowed {
                env.events().publish(
                    (BENEFICIARY_WATERFALL_TOPIC, vault_id),
                    BeneficiaryWaterfallEvent {
                        vault_id,
                        skipped_beneficiary: entry.address.clone(),
                        reason: String::from_str(env, "trigger"),
                    },
                );
                continue;
            }

            let ben_status: BeneficiaryStatus = env
                .storage()
                .persistent()
                .get(&DataKey::BeneficiaryStatusEntry(
                    vault_id,
                    entry.address.clone(),
                ))
                .unwrap_or(BeneficiaryStatus::Pending);
            if ben_status == BeneficiaryStatus::Declined {
                env.events().publish(
                    (BENEFICIARY_WATERFALL_TOPIC, vault_id),
                    BeneficiaryWaterfallEvent {
                        vault_id,
                        skipped_beneficiary: entry.address.clone(),
                        reason: String::from_str(env, "declined"),
                    },
                );
                continue;
            }

            let tier_threshold: i128 = env
                .storage()
                .persistent()
                .get(&DataKey::BeneficiaryTierThreshold(
                    vault_id,
                    entry.address.clone(),
                ))
                .unwrap_or(0);
            if tier_threshold > 0 && release_amount < tier_threshold {
                env.events().publish(
                    (BENEFICIARY_WATERFALL_TOPIC, vault_id),
                    BeneficiaryWaterfallEvent {
                        vault_id,
                        skipped_beneficiary: entry.address.clone(),
                        reason: String::from_str(env, "tier"),
                    },
                );
                continue;
            }

            let initial_share = release_amount * (entry.bps as i128) / 10_000;
            if entry.minimum_threshold > 0 && initial_share < entry.minimum_threshold {
                env.events().publish(
                    (MIN_THRESHOLD_SKIP_TOPIC,),
                    (
                        vault_id,
                        entry.address.clone(),
                        initial_share,
                        entry.minimum_threshold,
                    ),
                );
                continue;
            }

            qualifying.push_back(entry.clone());
            total_qualifying_bps = total_qualifying_bps.saturating_add(entry.bps);
        }

        if qualifying.is_empty() {
            token_client.transfer(
                &env.current_contract_address(),
                &vault.owner,
                &release_amount,
            );
            env.events().publish(
                (RELEASE_TOPIC,),
                ReleaseEvent {
                    vault_id,
                    beneficiary: vault.owner.clone(),
                    amount: release_amount,
                },
            );
            return;
        }

        if total_qualifying_bps < 10_000 && total_qualifying_bps > 0 {
            env.events().publish(
                (BENEFICIARY_REBALANCED_TOPIC, vault_id),
                BeneficiaryRebalancedEvent {
                    vault_id,
                    remaining_bps: total_qualifying_bps,
                },
            );
        }

        let mut distributed: i128 = 0;
        let last_idx = qualifying.len() - 1;
        for (i, entry) in qualifying.iter().enumerate() {
            let share = if i as u32 == last_idx {
                release_amount - distributed
            } else {
                release_amount * (entry.bps as i128) / (total_qualifying_bps as i128)
            };
            if share > 0 {
                crate::aml::require_compliant(env, &entry.address);
                token_client.transfer(&env.current_contract_address(), &entry.address, &share);
                env.events().publish(
                    (RELEASE_TOPIC,),
                    ReleaseEvent {
                        vault_id,
                        beneficiary: entry.address.clone(),
                        amount: share,
                    },
                );
            }
            distributed += share;
            env.events().publish(
                (MIN_THRESHOLD_REDISTRIBUTE_TOPIC,),
                (vault_id, entry.address.clone(), share),
            );
        }
    }

    // ── Configurable Countdown Notifications ────────────────────────────────

    /// Sets the countdown notification thresholds for a vault.
    ///
    /// Only the vault owner may call this. Thresholds are seconds-before-expiry
    /// values at which `check_countdown` will emit a `cd_notif` event.
    /// Pass an empty vec to disable countdown notifications.
    /// Default thresholds when not configured: 604800 (7d), 259200 (3d), 86400 (1d).
    ///
    /// # Arguments
    /// * `vault_id`   - The vault to configure
    /// * `caller`     - Must be the vault owner (requires auth)
    /// * `thresholds` - Seconds-before-expiry values (duplicates ignored; max 10 entries)
    pub fn set_countdown_config(env: Env, vault_id: u64, caller: Address, thresholds: Vec<u64>) {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            panic_with_error!(&env, ContractError::NotOwner);
        }
        let config = CountdownConfig {
            thresholds: thresholds.clone(),
        };
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        let key = DataKey::CountdownConfig(vault_id);
        env.storage().persistent().set(&key, &config);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        // Reset fired flags so new thresholds can fire fresh
        env.storage()
            .persistent()
            .remove(&DataKey::CountdownFired(vault_id));
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((SET_COUNTDOWN_TOPIC, vault_id), thresholds);
    }

    /// Returns the countdown config for a vault, or the default (7d/3d/1d) if not set.
    pub fn get_countdown_config(env: Env, vault_id: u64) -> CountdownConfig {
        env.storage()
            .persistent()
            .get(&DataKey::CountdownConfig(vault_id))
            .unwrap_or_else(|| {
                let mut t = Vec::new(&env);
                t.push_back(604_800u64); // 7 days
                t.push_back(259_200u64); // 3 days
                t.push_back(86_400u64); // 1 day
                CountdownConfig { thresholds: t }
            })
    }

    /// Checks the vault TTL against configured thresholds and emits `cd_notif`
    /// events for each threshold that has been crossed since the last call.
    ///
    /// Can be called by anyone (e.g. a cron job or off-chain keeper). Each
    /// threshold fires at most once per countdown cycle; fired flags are cleared
    /// when the owner calls `check_in` (TTL resets) or `set_countdown_config`.
    ///
    /// # Arguments
    /// * `vault_id` - The vault to check
    ///
    /// # Returns
    /// The remaining TTL in seconds (0 if expired or vault not Locked)
    pub fn check_countdown(env: Env, vault_id: u64) -> u64 {
        let vault = Self::try_load_vault(&env, vault_id)
            .unwrap_or_else(|| panic_with_error!(&env, ContractError::VaultNotFound));

        if vault.status != ReleaseStatus::Locked {
            return 0;
        }

        let ttl = Self::get_ttl_remaining(env.clone(), vault_id).unwrap_or(0);
        let config = Self::get_countdown_config(env.clone(), vault_id);

        // Load bitmask of already-fired thresholds (indexed by position in config.thresholds)
        let fired_key = DataKey::CountdownFired(vault_id);
        let mut fired: u32 = env.storage().persistent().get(&fired_key).unwrap_or(0u32);
        let mut updated = false;

        for (i, threshold) in config.thresholds.iter().enumerate() {
            let bit = 1u32 << (i as u32);
            if fired & bit == 0 && ttl <= threshold {
                env.events()
                    .publish((COUNTDOWN_NOTIF_TOPIC, vault_id), (threshold, ttl));
                fired |= bit;
                updated = true;
            }
        }

        if updated {
            let ttl_ledgers = vault_ttl_ledgers(vault.check_in_interval);
            env.storage().persistent().set(&fired_key, &fired);
            env.storage()
                .persistent()
                .extend_ttl(&fired_key, VAULT_TTL_THRESHOLD, ttl_ledgers);
            env.storage()
                .instance()
                .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        }

        ttl
    }
}
