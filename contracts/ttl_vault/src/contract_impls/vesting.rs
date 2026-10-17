#[contractimpl]
impl TtlVaultContract {
    // --- Task 5: vesting schedules ---

    /// Attaches a vesting schedule to a vault.
    ///
    /// Once set, a portion of the vault's balance (`amount`) is released to the
    /// beneficiary (or beneficiaries) in `num_installments` equal tranches.
    /// Each tranche becomes claimable every `interval` seconds starting from
    /// `start_time`.
    ///
    /// Multiple independent schedules may be attached to the same vault, each
    /// with its own amount, timing, and cliff. The sum of all schedule amounts
    /// must not exceed the vault's current balance.
    ///
    /// The vault must be released (`trigger_release`) before installments can
    /// be claimed. The schedule is set by the owner while the vault is still
    /// `Locked`.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault to attach the schedule to
    /// * `caller` - Must be the vault owner
    /// * `start_time` - Unix timestamp of the first claimable installment
    /// * `interval` - Seconds between installments (must be > 0)
    /// * `num_installments` - Number of tranches (must be > 0)
    /// * `amount` - Amount in stroops allocated to this schedule (must be > 0)
    /// * `cliff_period` - Seconds after `start_time` before any installment can be claimed (0 = no cliff)
    ///
    /// # Returns
    /// The index of the newly created schedule (0-based).
    ///
    /// # Errors
    /// * `ContractError::Paused` - If the contract is paused
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - If vault is not Locked
    /// * `ContractError::InvalidInterval` - If interval or num_installments is 0
    /// * `ContractError::EmptyVault` - If vault balance is 0
    /// * `ContractError::InvalidAmount` - If amount is <= 0
    /// * `ContractError::InsufficientBalance` - If total scheduled amount would exceed vault balance
    /// * `ContractError::VaultCapacityExceeded` - If max schedules per vault reached
    pub fn set_vesting_schedule(
        env: Env,
        vault_id: u64,
        caller: Address,
        start_time: u64,
        interval: u64,
        num_installments: u32,
        amount: i128,
        cliff_period: u64,
    ) -> Result<u32, ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        if interval == 0 || num_installments == 0 {
            return Err(ContractError::InvalidInterval);
        }
        let end_time = start_time + (interval as u64 * num_installments as u64);
        if end_time <= start_time {
            return Err(ContractError::InvalidVestingSchedule);
        }
        if vault.balance == 0 {
            return Err(ContractError::EmptyVault);
        }
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }

        // Compute the sum of all existing schedule amounts
        let count_key = DataKey::VestingScheduleCount(vault_id);
        let count: u32 = env.storage().persistent().get(&count_key).unwrap_or(0);
        let mut total_scheduled = amount;
        for i in 0..count {
            if let Some(s) = env
                .storage()
                .persistent()
                .get::<DataKey, VestingSchedule>(&DataKey::VestingSchedule(vault_id, i))
            {
                total_scheduled = total_scheduled
                    .checked_add(s.total_amount)
                    .unwrap_or_else(|| panic_with_error!(&env, ContractError::BalanceOverflow));
            }
        }
        if total_scheduled > vault.balance {
            return Err(ContractError::InsufficientBalance);
        }
        if count >= MAX_VESTING_SCHEDULES {
            return Err(ContractError::VaultCapacityExceeded);
        }

        let schedule_index = count;
        let schedule = VestingSchedule {
            start_time,
            interval,
            num_installments,
            claimed_installments: 0,
            total_amount: amount,
            cliff_period,
        };
        let key = DataKey::VestingSchedule(vault_id, schedule_index);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, &schedule);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);

        // Update schedule count
        let count_key = DataKey::VestingScheduleCount(vault_id);
        env.storage().persistent().set(&count_key, &(count + 1));
        env.storage()
            .persistent()
            .extend_ttl(&count_key, VAULT_TTL_THRESHOLD, ttl);

        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (SET_VESTING_TOPIC, vault_id),
            (
                start_time,
                interval,
                num_installments,
                amount,
                cliff_period,
                schedule_index,
            ),
        );
        env.events().publish(
            (VESTING_SCHEDULE_ADDED_TOPIC, vault_id),
            (schedule_index, amount),
        );
        Ok(schedule_index)
    }

    /// Returns the vesting schedule for a vault, if one exists.
    pub fn get_vesting_schedule(env: Env, vault_id: u64) -> Option<VestingSchedule> {
        env.storage()
            .persistent()
            .get(&DataKey::VestingSchedule(vault_id, 0))
    }

    /// Cancels a vesting schedule and returns unclaimed vested amounts to vault balance.
    /// Owner-only. Refunds the unclaimed portion to the vault.
    pub fn cancel_vesting_schedule(
        env: Env,
        vault_id: u64,
        schedule_id: u32,
        caller: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let mut vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }

        let schedule_key = DataKey::VestingSchedule(vault_id, schedule_id);
        let schedule = env
            .storage()
            .persistent()
            .get::<DataKey, VestingSchedule>(&schedule_key)
            .ok_or(ContractError::VestingNotFound)?;

        // Calculate unclaimed amount
        let per_installment = schedule.total_amount / (schedule.num_installments as i128);
        let claimed_amount = per_installment * (schedule.claimed_installments as i128);
        let unclaimed = schedule.total_amount.saturating_sub(claimed_amount);

        // Return unclaimed to vault
        vault.balance = vault.balance.saturating_add(unclaimed);
        Self::save_vault(&env, vault_id, &vault);

        // Remove schedule
        env.storage().persistent().remove(&schedule_key);

        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (VESTING_CANCELLED_TOPIC, vault_id),
            (schedule_id, unclaimed),
        );
        Ok(())
    }

    /// Sets a late-claim penalty for a vault's vesting schedule.
    ///
    /// If a beneficiary claims an installment more than `grace_period_seconds` after
    /// it unlocked, the payout is reduced by `penalty_bps` basis points (e.g. 500 = 5%).
    ///
    /// # Arguments
    /// * `vault_id`             - The vault with an attached vesting schedule
    /// * `caller`               - Must be the vault owner
    /// * `penalty_bps`          - Penalty in basis points (1–10000)
    /// * `grace_period_seconds` - Seconds after unlock before the penalty applies
    ///
    /// # Errors
    /// * `ContractError::NotOwner`         - Caller is not the vault owner
    /// * `ContractError::VestingNotFound`  - No vesting schedule exists
    /// * `ContractError::InvalidBps`       - penalty_bps is 0 or > 10000
    pub fn set_vesting_penalty(
        env: Env,
        vault_id: u64,
        caller: Address,
        penalty_bps: u32,
        grace_period_seconds: u64,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if !env
            .storage()
            .persistent()
            .has(&DataKey::VestingSchedule(vault_id, 0))
        {
            return Err(ContractError::VestingNotFound);
        }
        if penalty_bps == 0 || penalty_bps > 10_000 {
            return Err(ContractError::InvalidBps);
        }
        let config = VestingPenaltyConfig {
            penalty_bps,
            grace_period_seconds,
        };
        let key = DataKey::VestingPenalty(vault_id, 0);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, &config);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (VESTING_PENALTY_TOPIC, vault_id),
            (penalty_bps, grace_period_seconds),
        );
        Ok(())
    }

    /// Returns the vesting penalty config for a vault, if one has been set.
    pub fn get_vesting_penalty(env: Env, vault_id: u64) -> Option<VestingPenaltyConfig> {
        env.storage()
            .persistent()
            .get(&DataKey::VestingPenalty(vault_id, 0))
    }

    // --- Issue #548: Vesting Reversal ---

    /// Initiates a reversible vesting claim, holding tokens in escrow for a grace period.
    ///
    /// Unlike `claim_vested_installment` (which transfers immediately), this function
    /// calculates the claimable amount and stores a `VestingPendingClaim` record without
    /// transferring any tokens. The vault owner may call `reverse_vesting_claim` within
    /// `reversal_window_seconds` to cancel. After the window, anyone may call
    /// `finalize_vesting_claim` to complete the transfer.
    ///
    /// Only one pending claim may exist per vault at a time; subsequent calls fail until
    /// the pending claim is finalized or reversed.
    ///
    /// # Arguments
    /// * `vault_id`               - The released vault with a vesting schedule
    /// * `reversal_window_seconds`- Grace period in seconds (must be > 0)
    ///
    /// # Returns
    /// The escrowed amount (not yet transferred).
    ///
    /// # Errors
    /// * `ContractError::Paused`              - Contract is paused
    /// * `ContractError::VestingNotFound`     - No schedule on this vault
    /// * `ContractError::NothingToClaimYet`   - No installments available
    /// * `ContractError::VestingAlreadyComplete` - All installments claimed
    /// * `ContractError::InvalidInterval`     - reversal_window_seconds is 0
    /// * `ContractError::InvalidAmount`       - A pending claim already exists
    pub fn initiate_vesting_claim(
        env: Env,
        vault_id: u64,
        reversal_window_seconds: u64,
    ) -> Result<i128, ContractError> {
        Self::assert_not_paused(&env);
        if reversal_window_seconds == 0 {
            return Err(ContractError::InvalidInterval);
        }
        // Block if a pending claim already exists.
        if env
            .storage()
            .persistent()
            .has(&DataKey::VestingPendingClaim(vault_id, 0))
        {
            return Err(ContractError::InvalidAmount);
        }

        let vault = Self::load_vault(&env, vault_id);
        if vault.status != ReleaseStatus::Released {
            return Err(ContractError::AlreadyReleased);
        }

        let mut schedule: VestingSchedule = env
            .storage()
            .persistent()
            .get(&DataKey::VestingSchedule(vault_id, 0))
            .ok_or(ContractError::VestingNotFound)?;

        if schedule.claimed_installments >= schedule.num_installments {
            return Err(ContractError::VestingAlreadyComplete);
        }

        let now = env.ledger().timestamp();
        if now < schedule.start_time {
            return Err(ContractError::NothingToClaimYet);
        }

        let elapsed = now - schedule.start_time;
        let unlocked =
            ((elapsed / schedule.interval) + 1).min(schedule.num_installments as u64) as u32;
        let claimable = unlocked.saturating_sub(schedule.claimed_installments);
        if claimable == 0 {
            return Err(ContractError::NothingToClaimYet);
        }

        let per_installment = schedule.total_amount / schedule.num_installments as i128;
        let amount = if unlocked >= schedule.num_installments {
            vault.balance
        } else {
            per_installment * claimable as i128
        };

        let beneficiary = if vault.beneficiaries.is_empty() {
            vault.beneficiary.clone()
        } else {
            vault.beneficiaries.get(0).unwrap().address.clone()
        };

        let prev_installments_claimed = schedule.claimed_installments;

        // Advance the schedule counter so a second call is blocked until this claim
        // is finalized or reversed.
        schedule.claimed_installments = unlocked;
        let sched_key = DataKey::VestingSchedule(vault_id, 0);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&sched_key, &schedule);
        env.storage()
            .persistent()
            .extend_ttl(&sched_key, VAULT_TTL_THRESHOLD, ttl);

        let pending = VestingPendingClaim {
            amount,
            beneficiary,
            initiated_at: now,
            reversal_deadline: now + reversal_window_seconds,
            new_installments_claimed: unlocked,
            prev_installments_claimed,
        };
        let pk = DataKey::VestingPendingClaim(vault_id, 0);
        env.storage().persistent().set(&pk, &pending);
        env.storage()
            .persistent()
            .extend_ttl(&pk, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        Ok(amount)
    }

    /// Reverses a pending vesting claim within the reversal window.
    ///
    /// Only the vault owner may call this. The pending claim record is cleared and
    /// the schedule counter is rolled back so the installments become claimable again.
    ///
    /// # Errors
    /// * `ContractError::NotOwner`            - Caller is not the vault owner
    /// * `ContractError::VestingReversalNotFound` - No pending claim on this vault
    /// * `ContractError::VestingReversalExpired`  - Reversal window has closed
    pub fn reverse_vesting_claim(
        env: Env,
        vault_id: u64,
        caller: Address,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }

        let pk = DataKey::VestingPendingClaim(vault_id, 0);
        let pending: VestingPendingClaim = env
            .storage()
            .persistent()
            .get(&pk)
            .ok_or(ContractError::VestingReversalNotFound)?;

        let now = env.ledger().timestamp();
        if now > pending.reversal_deadline {
            return Err(ContractError::VestingReversalExpired);
        }

        // Roll back the schedule counter so these installments can be claimed again.
        let sched_key = DataKey::VestingSchedule(vault_id, 0);
        if let Some(mut schedule) = env
            .storage()
            .persistent()
            .get::<DataKey, VestingSchedule>(&sched_key)
        {
            schedule.claimed_installments = pending.prev_installments_claimed;
            let ttl = vault_ttl_ledgers(vault.check_in_interval);
            env.storage().persistent().set(&sched_key, &schedule);
            env.storage()
                .persistent()
                .extend_ttl(&sched_key, VAULT_TTL_THRESHOLD, ttl);
        }

        env.storage().persistent().remove(&pk);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((VESTING_REVERSED_TOPIC, vault_id), pending.amount);
        Ok(())
    }

    /// Finalizes a pending vesting claim after the reversal window has closed.
    ///
    /// Anyone may call this. Transfers the escrowed amount to the beneficiary and
    /// clears the pending claim record.
    ///
    /// # Errors
    /// * `ContractError::VestingReversalNotFound` - No pending claim on this vault
    /// * `ContractError::InvalidInterval`         - Reversal window has not yet closed
    pub fn finalize_vesting_claim(env: Env, vault_id: u64) -> Result<i128, ContractError> {
        Self::assert_not_paused(&env);

        let pk = DataKey::VestingPendingClaim(vault_id, 0);
        let pending: VestingPendingClaim = env
            .storage()
            .persistent()
            .get(&pk)
            .ok_or(ContractError::VestingReversalNotFound)?;

        let now = env.ledger().timestamp();
        if now <= pending.reversal_deadline {
            return Err(ContractError::InvalidInterval);
        }

        let mut vault = Self::load_vault(&env, vault_id);
        if vault.balance < pending.amount {
            return Err(ContractError::InsufficientBalance);
        }

        let token_client = token::Client::new(&env, &vault.token_address);
        crate::aml::require_compliant(&env, &pending.beneficiary);
        token_client.transfer(
            &env.current_contract_address(),
            &pending.beneficiary,
            &pending.amount,
        );

        vault.balance -= pending.amount;
        Self::save_vault(&env, vault_id, &vault);
        env.storage().persistent().remove(&pk);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((VESTING_FINALIZED_TOPIC, vault_id), pending.amount);
        Ok(pending.amount)
    }

    /// Returns the pending vesting claim for a vault, if one exists.
    pub fn get_pending_vesting_claim(env: Env, vault_id: u64) -> Option<VestingPendingClaim> {
        env.storage()
            .persistent()
            .get(&DataKey::VestingPendingClaim(vault_id, 0))
    }

    /// Claims all vested installments that have become available since the last claim.
    ///
    /// The vault must have been released (trigger_release called) and a vesting schedule
    /// must be attached. The beneficiary (or any caller) can invoke this once the vault
    /// is Released and at least one installment window has elapsed since `start_time`.
    ///
    /// Funds are distributed to the primary beneficiary, or split among multi-beneficiaries
    /// using the same BPS logic as `trigger_release`.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault to claim from
    ///
    /// # Returns
    /// The total amount transferred in this call
    ///
    /// # Errors
    /// * `ContractError::Paused` - If the contract is paused
    /// * `ContractError::VestingNotFound` - If no vesting schedule exists
    /// * `ContractError::NothingToClaimYet` - If no new installments are available
    /// * `ContractError::VestingAlreadyComplete` - If all installments have been claimed
    /// * `ContractError::InsufficientBalance` - If vault balance is insufficient
    pub fn claim_vested_installment(env: Env, vault_id: u64) -> Result<i128, ContractError> {
        Self::assert_not_paused(&env);
        let mut vault = Self::load_vault(&env, vault_id);

        // Cannot claim from cancelled vault
        if vault.status == ReleaseStatus::Cancelled {
            return Err(ContractError::AlreadyReleased);
        }

        // Vault must be Released for vesting claims
        if vault.status != ReleaseStatus::Released {
            return Err(ContractError::AlreadyReleased);
        }

        let mut schedule: VestingSchedule = env
            .storage()
            .persistent()
            .get(&DataKey::VestingSchedule(vault_id, 0))
            .ok_or(ContractError::VestingNotFound)?;

        if schedule.claimed_installments >= schedule.num_installments {
            return Err(ContractError::VestingAlreadyComplete);
        }

        let now = env.ledger().timestamp();
        if now < schedule.start_time {
            return Err(ContractError::NothingToClaimYet);
        }

        // Enforce cliff: no claims until start_time + cliff_period has elapsed
        if schedule.cliff_period > 0 && now < schedule.start_time + schedule.cliff_period {
            return Err(ContractError::CliffNotReached);
        }

        // Emit cliff reached event on the first claim after cliff (cliff_period > 0 and no prior claims)
        let cliff_just_reached = schedule.cliff_period > 0 && schedule.claimed_installments == 0;
        if cliff_just_reached {
            env.events()
                .publish((CLIFF_REACHED_TOPIC, vault_id), (now,));
        }

        // Issue #543: check if vesting is accelerated
        let accelerated = env
            .storage()
            .persistent()
            .get::<DataKey, VestingAccelerationConfig>(&DataKey::VestingAcceleration(vault_id))
            .is_some_and(|c| c.accelerated);

        // How many installments are unlocked so far?
        let unlocked = if accelerated {
            schedule.num_installments
        } else {
            let elapsed = now.saturating_sub(schedule.start_time);
            ((elapsed / schedule.interval) + 1).min(schedule.num_installments as u64) as u32
        };

        let claimable = unlocked.saturating_sub(schedule.claimed_installments);
        if claimable == 0 {
            // Even if claimable is 0, we might have rolled over amounts to claim
            let rollover_cfg = env
                .storage()
                .persistent()
                .get::<DataKey, VestingRolloverConfig>(&DataKey::VestingRollover(vault_id));
            if rollover_cfg
                .as_ref()
                .is_none_or(|c| !c.enabled || c.rolled_amount == 0)
            {
                return Err(ContractError::NothingToClaimYet);
            }
        }

        // Calculate payout: each installment = total / num_installments,
        // last installment absorbs remainder.
        let per_installment = schedule.total_amount / schedule.num_installments as i128;
        let mut base_amount = if unlocked >= schedule.num_installments {
            vault.balance
        } else {
            per_installment * claimable as i128
        };

        // Issue #541: add rolled over amounts if enabled
        if let Some(mut cfg) = env
            .storage()
            .persistent()
            .get::<DataKey, VestingRolloverConfig>(&DataKey::VestingRollover(vault_id))
        {
            if cfg.enabled && cfg.rolled_amount > 0 {
                base_amount += cfg.rolled_amount;
                cfg.rolled_amount = 0;
                env.storage()
                    .persistent()
                    .set(&DataKey::VestingRollover(vault_id), &cfg);
            }
        }

        // Issue #547: apply late-claim penalty to installments claimed after grace period.
        let amount = if let Some(penalty_cfg) = env
            .storage()
            .persistent()
            .get::<DataKey, VestingPenaltyConfig>(&DataKey::VestingPenalty(vault_id, 0))
        {
            // Count how many of the claimable installments are "late".
            // Installment i (1-based) unlocks at start_time + (i-1)*interval.
            let late_cutoff = now.saturating_sub(penalty_cfg.grace_period_seconds);
            let mut late_count = 0u32;
            for i in (schedule.claimed_installments + 1)..=unlocked {
                let unlock_time = schedule
                    .start_time
                    .saturating_add((i as u64 - 1).saturating_mul(schedule.interval));
                if unlock_time < late_cutoff {
                    late_count += 1;
                }
            }
            if late_count > 0 {
                let on_time_count = claimable.saturating_sub(late_count);
                let on_time_amount = per_installment * on_time_count as i128;
                let late_amount = per_installment * late_count as i128;
                let penalty = late_amount * penalty_cfg.penalty_bps as i128 / 10_000;
                let penalized = on_time_amount + late_amount - penalty;
                // For the final batch keep vault.balance as ceiling to avoid dust mismatch.
                if unlocked >= schedule.num_installments {
                    penalized.min(vault.balance)
                } else {
                    penalized
                }
            } else {
                base_amount
            }
        } else {
            base_amount
        };

        if vault.balance < amount {
            return Err(ContractError::InsufficientBalance);
        }

        let token_client = token::Client::new(&env, &vault.token_address);

        if vault.beneficiaries.is_empty() {
            crate::aml::require_compliant(&env, &vault.beneficiary);
            token_client.transfer(&env.current_contract_address(), &vault.beneficiary, &amount);
            env.events().publish(
                (CLAIM_VEST_TOPIC, vault_id),
                (vault.beneficiary.clone(), amount, unlocked),
            );
        } else {
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
                    (CLAIM_VEST_TOPIC, vault_id),
                    (entry.address.clone(), share, unlocked),
                );
            }
        }

        vault.balance -= amount;
        schedule.claimed_installments = unlocked;
        Self::save_vault(&env, vault_id, &vault);

        // Emit penalty event if the payout was reduced.
        if amount < base_amount {
            env.events().publish(
                (VESTING_PENALTY_TOPIC, vault_id),
                (base_amount - amount, amount),
            );
        }

        let sched_key = DataKey::VestingSchedule(vault_id, 0);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&sched_key, &schedule);
        env.storage()
            .persistent()
            .extend_ttl(&sched_key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        Ok(amount)
    }

    /// Allows the vault owner to claw back unvested funds from a released vault.
    ///
    /// After a vault has been released (`trigger_release` called) with one or more
    /// vesting schedules attached, the owner may reclaim funds that have not yet
    /// vested — i.e., installments that have not been claimed by the beneficiary.
    /// All vesting schedules on the vault are marked as fully claimed so no
    /// further beneficiary claims are possible.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault to claw back from
    /// * `caller` - Must be the vault owner
    ///
    /// # Returns
    /// The total amount clawed back (in stroops)
    ///
    /// # Errors
    /// * `ContractError::Paused` - If the contract is paused
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - If vault is not in Released status
    /// * `ContractError::VestingNotFound` - If no vesting schedule exists on the vault
    /// * `ContractError::NothingToClawback` - If all installments have already been claimed
    /// * `ContractError::InsufficientBalance` - If vault balance is less than unvested amount
    pub fn clawback_unvested(
        env: Env,
        vault_id: u64,
        caller: Address,
    ) -> Result<i128, ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        let mut vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Released {
            return Err(ContractError::AlreadyReleased);
        }

        // Collect all vesting schedules on this vault
        let count_key = DataKey::VestingScheduleCount(vault_id);
        let count: u32 = env.storage().persistent().get(&count_key).unwrap_or(0);
        if count == 0 {
            return Err(ContractError::VestingNotFound);
        }

        // Calculate total unvested = sum of unclaimed installment amounts
        let mut total_unvested: i128 = 0;
        let ttl = vault_ttl_ledgers(vault.check_in_interval);

        for i in 0..count {
            let sched_key = DataKey::VestingSchedule(vault_id, i);
            if let Some(schedule) = env
                .storage()
                .persistent()
                .get::<DataKey, VestingSchedule>(&sched_key)
            {
                let remaining = schedule.num_installments - schedule.claimed_installments;
                if remaining > 0 {
                    let per_installment = schedule.total_amount / schedule.num_installments as i128;
                    total_unvested += per_installment * remaining as i128;
                }
            }
        }

        if total_unvested == 0 {
            return Err(ContractError::NothingToClawback);
        }

        if vault.balance < total_unvested {
            return Err(ContractError::InsufficientBalance);
        }

        // Transfer unvested funds to owner
        let token_client = token::Client::new(&env, &vault.token_address);
        token_client.transfer(
            &env.current_contract_address(),
            &vault.owner,
            &total_unvested,
        );

        // Update vault balance
        vault.balance -= total_unvested;

        // Second pass: mark all schedules as fully claimed
        for i in 0..count {
            let sched_key = DataKey::VestingSchedule(vault_id, i);
            if let Some(mut schedule) = env
                .storage()
                .persistent()
                .get::<DataKey, VestingSchedule>(&sched_key)
            {
                schedule.claimed_installments = schedule.num_installments;
                env.storage().persistent().set(&sched_key, &schedule);
                env.storage()
                    .persistent()
                    .extend_ttl(&sched_key, VAULT_TTL_THRESHOLD, ttl);
            }
        }

        Self::save_vault(&env, vault_id, &vault);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (CLAWBACK_UNVESTED_TOPIC, vault_id),
            (total_unvested, vault.balance),
        );
        Ok(total_unvested)
    }

    // --- milestone-based vesting ---

    /// Sets a milestone-based vesting schedule for a vault.
    ///
    /// Instead of releasing funds on a time-based schedule, funds are unlocked
    /// when external milestones (e.g., company revenue targets) are fulfilled.
    /// An oracle address is designated to report progress on each milestone.
    ///
    /// The vault must be Locked, have a non-zero balance, and must not already
    /// have a time-based vesting schedule attached.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault to attach the milestone schedule to
    /// * `caller` - Must be the vault owner
    /// * `milestones` - Vector of MilestoneEntry defining each milestone (target, BPS, label)
    /// * `oracle` - Address authorized to report milestone progress
    ///
    /// # Errors
    /// * `ContractError::Paused` - If the contract is paused
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::AlreadyReleased` - If vault is not Locked
    /// * `ContractError::EmptyVault` - If vault balance is 0
    /// * `ContractError::InvalidBps` - If milestone BPS does not sum to 10_000
    /// * `ContractError::VestingAlreadySet` - If a time-based vesting schedule exists
    pub fn set_milestone_vesting(
        env: Env,
        vault_id: u64,
        caller: Address,
        milestones: Vec<MilestoneEntry>,
        oracle: Address,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        if vault.balance == 0 {
            return Err(ContractError::EmptyVault);
        }
        // Cannot have both time-based and milestone-based vesting
        if env
            .storage()
            .persistent()
            .has(&DataKey::VestingSchedule(vault_id, 0))
        {
            return Err(ContractError::AlreadyReleased);
        }
        if milestones.is_empty() {
            return Err(ContractError::InvalidBps);
        }
        let total_bps: u32 = milestones.iter().map(|m| m.bps).sum();
        if total_bps != 10_000 {
            return Err(ContractError::InvalidBps);
        }
        // Ensure no milestone is pre-fulfilled
        for milestone in milestones.iter() {
            if milestone.is_fulfilled {
                return Err(ContractError::InvalidAmount);
            }
        }

        let schedule = MilestoneVestingSchedule {
            total_amount: vault.balance,
            milestones,
            claimed_amount: 0,
            oracle,
            paused: false,
        };
        let key = DataKey::MilestoneVestingSchedule(vault_id);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, &schedule);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (MILESTONE_VEST_TOPIC, vault_id),
            (schedule.total_amount, schedule.oracle.clone()),
        );
        Ok(())
    }

    /// Returns the milestone vesting schedule for a vault, if one exists.
    pub fn get_milestone_vesting_schedule(
        env: Env,
        vault_id: u64,
    ) -> Option<MilestoneVestingSchedule> {
        env.storage()
            .persistent()
            .get(&DataKey::MilestoneVestingSchedule(vault_id))
    }

    /// Pauses a milestone vesting schedule, blocking progress reporting and claiming.
    ///
    /// Only the vault owner can call this. The vesting state and milestone data are
    /// preserved and can be resumed later via `resume_milestone_vesting`.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault ID
    /// * `caller` - The address of the caller (must be vault owner)
    ///
    /// # Errors
    /// * `ContractError::Paused` - If the contract is paused
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::VestingNotFound` - If no milestone vesting schedule exists
    pub fn pause_milestone_vesting(
        env: Env,
        vault_id: u64,
        caller: Address,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }

        let key = DataKey::MilestoneVestingSchedule(vault_id);
        let mut schedule: MilestoneVestingSchedule = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::VestingNotFound)?;

        if schedule.paused {
            return Err(ContractError::Paused);
        }
        schedule.paused = true;

        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, &schedule);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((MILESTONE_PAUSE_TOPIC, vault_id), true);
        Ok(())
    }

    /// Resumes a paused milestone vesting schedule, re-enabling progress reporting and claiming.
    ///
    /// Only the vault owner can call this. All milestone progress and state is preserved
    /// from before the pause.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault ID
    /// * `caller` - The address of the caller (must be vault owner)
    ///
    /// # Errors
    /// * `ContractError::Paused` - If the contract is paused
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::VestingNotFound` - If no milestone vesting schedule exists
    pub fn resume_milestone_vesting(
        env: Env,
        vault_id: u64,
        caller: Address,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }

        let key = DataKey::MilestoneVestingSchedule(vault_id);
        let mut schedule: MilestoneVestingSchedule = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::VestingNotFound)?;

        if !schedule.paused {
            return Err(ContractError::Paused);
        }
        schedule.paused = false;

        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, &schedule);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((MILESTONE_RESUME_TOPIC, vault_id), false);
        Ok(())
    }

    // --- Issue #541: Vesting Rollover ---

    /// Enables or disables vesting rollover for a vault.
    /// When enabled, unclaimed installments roll over and accumulate.
    pub fn set_vesting_rollover(
        env: Env,
        vault_id: u64,
        caller: Address,
        enabled: bool,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        let config = VestingRolloverConfig {
            enabled,
            rolled_amount: 0,
        };
        let key = DataKey::VestingRollover(vault_id);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, &config);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.events()
            .publish((VESTING_ROLLOVER_TOPIC, vault_id), enabled);
        Ok(())
    }

    /// Returns the vesting rollover configuration for a vault.
    pub fn get_vesting_rollover(env: Env, vault_id: u64) -> Option<VestingRolloverConfig> {
        env.storage()
            .persistent()
            .get(&DataKey::VestingRollover(vault_id))
    }

    // --- Issue #542: Vesting Forfeiture ---

    /// Sets the forfeiture recipient for a vault's vesting schedule.
    /// If the beneficiary declines their role, unvested funds are sent to this address.
    pub fn set_vesting_forfeiture(
        env: Env,
        vault_id: u64,
        caller: Address,
        forfeiture_recipient: Address,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        let config = VestingForfeitureConfig {
            forfeiture_recipient,
            forfeited: false,
        };
        let key = DataKey::VestingForfeiture(vault_id);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, &config);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.events().publish(
            (VESTING_FORFEITURE_TOPIC, vault_id),
            config.forfeiture_recipient.clone(),
        );
        Ok(())
    }

    /// Returns the vesting forfeiture configuration for a vault.
    pub fn get_vesting_forfeiture(env: Env, vault_id: u64) -> Option<VestingForfeitureConfig> {
        env.storage()
            .persistent()
            .get(&DataKey::VestingForfeiture(vault_id))
    }

    // --- Issue #543: Vesting Acceleration ---

    /// Sets the oracle for vesting acceleration.
    /// The oracle can trigger immediate release of all remaining installments.
    pub fn set_vesting_acceleration(
        env: Env,
        vault_id: u64,
        caller: Address,
        oracle: Address,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        let config = VestingAccelerationConfig {
            oracle,
            accelerated: false,
        };
        let key = DataKey::VestingAcceleration(vault_id);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, &config);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.events()
            .publish((VESTING_ACCELERATED_TOPIC, vault_id), config.oracle.clone());
        Ok(())
    }

    /// Returns the vesting acceleration configuration for a vault.
    pub fn get_vesting_acceleration(env: Env, vault_id: u64) -> Option<VestingAccelerationConfig> {
        env.storage()
            .persistent()
            .get(&DataKey::VestingAcceleration(vault_id))
    }

    /// Returns the total amount still locked in vesting schedules for a vault.
    pub fn get_total_unvested_amount(env: Env, vault_id: u64) -> i128 {
        let count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::VestingScheduleCount(vault_id))
            .unwrap_or(0);

        let mut total = 0i128;
        for i in 0..count {
            if let Some(schedule) = env
                .storage()
                .persistent()
                .get::<DataKey, VestingSchedule>(&DataKey::VestingSchedule(vault_id, i))
            {
                let per_installment = schedule.total_amount / (schedule.num_installments as i128);
                let claimed_amount = per_installment * (schedule.claimed_installments as i128);
                let unclaimed = schedule.total_amount.saturating_sub(claimed_amount);
                total = total.saturating_add(unclaimed);
            }
        }
        total
    }

    /// Triggers immediate acceleration of all remaining vesting installments.
    /// Can only be called by the designated oracle.
    pub fn accelerate_vesting(
        env: Env,
        vault_id: u64,
        caller: Address,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        let key = DataKey::VestingAcceleration(vault_id);
        let mut config: VestingAccelerationConfig = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::VestingNotFound)?;
        if caller != config.oracle {
            return Err(ContractError::NotOwner);
        }
        if config.accelerated {
            return Err(ContractError::VestingAlreadyAccelerated);
        }
        config.accelerated = true;
        env.storage().persistent().set(&key, &config);
        env.events()
            .publish((VESTING_ACCELERATED_TOPIC, vault_id), true);
        Ok(())
    }

    // --- Issue #544: Vesting Staggering ---

    /// Sets a staggered vesting schedule for multiple beneficiaries.
    pub fn set_vesting_stagger(
        env: Env,
        vault_id: u64,
        caller: Address,
        entries: Vec<VestingStaggerEntry>,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }
        let total_bps: u32 = entries.iter().map(|e| e.bps).sum();
        if total_bps != 10_000 {
            return Err(ContractError::InvalidBps);
        }
        let key = DataKey::VestingStagger(vault_id);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, &entries);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.events()
            .publish((VESTING_STAGGER_TOPIC, vault_id), entries.len());
        Ok(())
    }

    /// Returns the staggered vesting schedule for a vault.
    pub fn get_vesting_stagger(env: Env, vault_id: u64) -> Vec<VestingStaggerEntry> {
        env.storage()
            .persistent()
            .get(&DataKey::VestingStagger(vault_id))
            .unwrap_or(Vec::new(&env))
    }

    /// Claims available installments from a staggered vesting schedule.
    pub fn claim_staggered_vesting(
        env: Env,
        vault_id: u64,
        caller: Address,
    ) -> Result<i128, ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        let mut vault = Self::load_vault(&env, vault_id);
        if vault.status != ReleaseStatus::Released {
            return Err(ContractError::AlreadyReleased);
        }
        let key = DataKey::VestingStagger(vault_id);
        let mut entries: Vec<VestingStaggerEntry> = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::VestingStaggerNotFound)?;

        let mut total_amount: i128 = 0;
        let now = env.ledger().timestamp();
        let mut entry_found = false;

        for i in 0..entries.len() {
            let mut entry = entries.get(i).unwrap();
            if entry.beneficiary == caller {
                entry_found = true;
                if entry.claimed_installments >= entry.num_installments {
                    continue; // Skip already completed ones for this beneficiary
                }
                if now < entry.start_time {
                    continue;
                }
                let elapsed = now - entry.start_time;
                let unlocked =
                    ((elapsed / entry.interval) + 1).min(entry.num_installments as u64) as u32;
                let claimable = unlocked.saturating_sub(entry.claimed_installments);
                if claimable == 0 {
                    continue;
                }

                // Stagger amount is based on total vault balance at the time of release.
                // For simplicity here, we assume the BPS applies to the current balance + what was already claimed.
                // But since we don't track original balance, we'll just use BPS * (current_balance / remaining_bps).
                // Better: assume vault.balance is the total pool for staggered vesting if no other vesting is set.
                let per_installment =
                    (vault.balance * entry.bps as i128 / 10_000) / entry.num_installments as i128;
                let amount = if unlocked >= entry.num_installments {
                    // Last installment takes the remaining share for this beneficiary
                    vault.balance * entry.bps as i128 / 10_000
                } else {
                    per_installment * claimable as i128
                };

                if amount > 0 {
                    let token_client = token::Client::new(&env, &vault.token_address);
                    crate::aml::require_compliant(&env, &caller);
                    token_client.transfer(&env.current_contract_address(), &caller, &amount);
                    vault.balance -= amount;
                    entry.claimed_installments = unlocked;
                    entries.set(i, entry);
                    total_amount += amount;
                }
            }
        }

        if !entry_found {
            return Err(ContractError::NotBeneficiary);
        }
        if total_amount == 0 {
            return Err(ContractError::NothingToClaimYet);
        }

        Self::save_vault(&env, vault_id, &vault);
        env.storage().persistent().set(&key, &entries);
        env.events()
            .publish((CLAIM_VEST_TOPIC, vault_id), (caller, total_amount));
        Ok(total_amount)
    }

    /// Adjusts milestone targets to slow down vesting.
    ///
    /// Only the vault owner can call this. Increases the `target_value` of unfulfilled
    /// milestones, making them harder to reach. Already-fulfilled milestones are
    /// unaffected. This allows the owner to respond to changing circumstances without
    /// cancelling the vesting schedule.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault ID
    /// * `caller` - The address of the caller (must be vault owner)
    /// * `adjustments` - Vector of `(milestone_index, new_target_value)` pairs
    ///
    /// # Errors
    /// * `ContractError::Paused` - If the contract is paused
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::VestingNotFound` - If no milestone vesting schedule exists
    /// * `ContractError::InvalidAmount` - If any milestone is already fulfilled
    pub fn adjust_milestone_targets(
        env: Env,
        vault_id: u64,
        caller: Address,
        adjustments: Vec<(u32, i128)>,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }

        let key = DataKey::MilestoneVestingSchedule(vault_id);
        let mut schedule: MilestoneVestingSchedule = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::VestingNotFound)?;

        for (idx, new_target) in adjustments.iter() {
            if idx >= schedule.milestones.len() {
                return Err(ContractError::InvalidAmount);
            }
            let milestone = schedule.milestones.get(idx).unwrap();
            if milestone.is_fulfilled {
                return Err(ContractError::InvalidAmount);
            }
            schedule.milestones.set(
                idx,
                MilestoneEntry {
                    label: milestone.label,
                    target_value: new_target,
                    current_value: milestone.current_value,
                    bps: milestone.bps,
                    is_fulfilled: milestone.is_fulfilled,
                    claimed: milestone.claimed,
                },
            );
        }

        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, &schedule);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish((MILESTONE_ADJUST_TOPIC, vault_id), ());
        Ok(())
    }

    /// Emergency-fulfills one or more milestones, bypassing the oracle.
    ///
    /// Only the vault owner can call this. Marks the specified milestones as fulfilled,
    /// making their funds available for claiming immediately. This is intended for
    /// emergency situations where the oracle cannot or should not be relied upon.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault ID
    /// * `caller` - The address of the caller (must be vault owner)
    /// * `milestone_indices` - Indices of milestones to mark as fulfilled
    ///
    /// # Errors
    /// * `ContractError::Paused` - If the contract is paused
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::VestingNotFound` - If no milestone vesting schedule exists
    /// * `ContractError::InvalidAmount` - If any milestone is already fulfilled or invalid
    pub fn emergency_fulfill_milestones(
        env: Env,
        vault_id: u64,
        caller: Address,
        milestone_indices: Vec<u32>,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }

        let key = DataKey::MilestoneVestingSchedule(vault_id);
        let mut schedule: MilestoneVestingSchedule = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::VestingNotFound)?;

        let mut any_updated = false;
        for idx in milestone_indices.iter() {
            if idx >= schedule.milestones.len() {
                return Err(ContractError::InvalidAmount);
            }
            let milestone = schedule.milestones.get(idx).unwrap();
            if milestone.is_fulfilled {
                return Err(ContractError::InvalidAmount);
            }
            schedule.milestones.set(
                idx,
                MilestoneEntry {
                    label: milestone.label,
                    target_value: milestone.target_value,
                    current_value: milestone.current_value.max(milestone.target_value),
                    bps: milestone.bps,
                    is_fulfilled: true,
                    claimed: milestone.claimed,
                },
            );
            any_updated = true;
        }

        if !any_updated {
            return Err(ContractError::InvalidAmount);
        }

        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, &schedule);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((MILESTONE_EMERGENCY_TOPIC, vault_id), true);
        Ok(())
    }

    /// Reports progress on one or more milestones.
    ///
    /// Only the designated oracle address can call this function. When a milestone's
    /// `current_value` reaches or exceeds its `target_value`, it is marked as fulfilled
    /// and its funds become available for claiming.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault ID
    /// * `milestone_indices` - Indices of milestones to update
    /// * `current_values` - New current values for each milestone (must match indices length)
    ///
    /// # Errors
    /// * `ContractError::MilestoneVestingNotFound` - If no milestone vesting schedule exists
    /// * `ContractError::NotMilestoneOracle` - If caller is not the designated oracle
    /// * `ContractError::InvalidAmount` - If indices and values length mismatch
    /// * `ContractError::MilestoneAlreadyFulfilled` - If a milestone is already fulfilled
    pub fn report_milestone_progress(
        env: Env,
        vault_id: u64,
        milestone_indices: Vec<u32>,
        current_values: Vec<i128>,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        if milestone_indices.len() != current_values.len() {
            return Err(ContractError::InvalidAmount);
        }

        let mut schedule: MilestoneVestingSchedule = env
            .storage()
            .persistent()
            .get(&DataKey::MilestoneVestingSchedule(vault_id))
            .ok_or(ContractError::VestingNotFound)?;

        if schedule.paused {
            return Err(ContractError::Paused);
        }

        schedule.oracle.require_auth();

        let mut any_updated = false;
        for (idx, value) in milestone_indices.iter().zip(current_values.iter()) {
            if idx >= schedule.milestones.len() {
                return Err(ContractError::InvalidAmount);
            }
            let milestone = &schedule.milestones.get(idx).unwrap();
            if milestone.is_fulfilled {
                return Err(ContractError::InvalidAmount);
            }
            let was_fulfilled = milestone.current_value >= milestone.target_value;
            let new_value = value.max(0);
            let mut updated = milestone.clone();
            updated.current_value = new_value;
            if new_value >= updated.target_value && !was_fulfilled {
                updated.is_fulfilled = true;
            } else if new_value < updated.target_value {
                updated.is_fulfilled = false;
            }
            schedule.milestones.set(idx, updated);
            any_updated = true;
        }

        if !any_updated {
            return Ok(());
        }

        let key = DataKey::MilestoneVestingSchedule(vault_id);
        let ttl = vault_ttl_ledgers(Self::load_vault(&env, vault_id).check_in_interval);
        env.storage().persistent().set(&key, &schedule);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        // Emit progress event for each updated milestone
        for idx in milestone_indices.iter() {
            let m = &schedule.milestones.get(idx).unwrap();
            env.events().publish(
                (MILESTONE_PROGRESS_TOPIC, vault_id),
                (idx, m.current_value, m.target_value, m.is_fulfilled),
            );
        }
        Ok(())
    }

    /// Claims all vested installments from fulfilled milestones.
    ///
    /// The vault must have been released (`trigger_release`) and a milestone vesting
    /// schedule must be attached. Funds from all milestones that are fulfilled but
    /// not yet claimed are distributed to the vault's beneficiaries using the same
    /// BPS split logic as `trigger_release`.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `vault_id` - The vault to claim from
    ///
    /// # Returns
    /// The total amount transferred in this call
    ///
    /// # Errors
    /// * `ContractError::Paused` - If the contract is paused
    /// * `ContractError::AlreadyReleased` - If vault is not in Released status
    /// * `ContractError::MilestoneVestingNotFound` - If no milestone vesting schedule exists
    /// * `ContractError::NoClaimableMilestones` - If no milestones are ready to claim
    /// * `ContractError::InsufficientBalance` - If vault balance is insufficient
    pub fn claim_milestone_installments(env: Env, vault_id: u64) -> Result<i128, ContractError> {
        Self::assert_not_paused(&env);
        let mut vault = Self::load_vault(&env, vault_id);

        if vault.status != ReleaseStatus::Released {
            return Err(ContractError::AlreadyReleased);
        }

        let mut schedule: MilestoneVestingSchedule = env
            .storage()
            .persistent()
            .get(&DataKey::MilestoneVestingSchedule(vault_id))
            .ok_or(ContractError::VestingNotFound)?;

        if schedule.paused {
            return Err(ContractError::Paused);
        }

        // Calculate claimable amounts from fulfilled milestones
        let mut total_claimable = 0i128;
        let mut any_claimable = false;
        let num_milestones = schedule.milestones.len();

        let num_milestones_usize = num_milestones as usize;
        for (i, milestone) in schedule.milestones.iter().enumerate() {
            if milestone.is_fulfilled && !milestone.claimed {
                let is_last = i == num_milestones_usize - 1;
                let share = if is_last {
                    schedule.total_amount - total_claimable - schedule.claimed_amount
                } else {
                    schedule.total_amount * (milestone.bps as i128) / 10_000
                };
                total_claimable += share;
                any_claimable = true;
            }
        }

        if !any_claimable {
            return Err(ContractError::NothingToClaimYet);
        }

        if vault.balance < total_claimable {
            return Err(ContractError::InsufficientBalance);
        }

        // Distribute to beneficiaries
        let token_client = token::Client::new(&env, &vault.token_address);

        if vault.beneficiaries.is_empty() {
            crate::aml::require_compliant(&env, &vault.beneficiary);
            token_client.transfer(
                &env.current_contract_address(),
                &vault.beneficiary,
                &total_claimable,
            );
            env.events().publish(
                (MILESTONE_CLAIM_TOPIC, vault_id),
                (
                    vault.beneficiary.clone(),
                    total_claimable,
                    schedule.claimed_amount + total_claimable,
                ),
            );
        } else {
            let mut distributed: i128 = 0;
            let last_idx = vault.beneficiaries.len() - 1;
            for (i, entry) in vault.beneficiaries.iter().enumerate() {
                let share = if i as u32 == last_idx {
                    total_claimable - distributed
                } else {
                    total_claimable * (entry.bps as i128) / 10_000
                };
                if share > 0 {
                    crate::aml::require_compliant(&env, &entry.address);
                    token_client.transfer(&env.current_contract_address(), &entry.address, &share);
                }
                distributed += share;
                env.events().publish(
                    (MILESTONE_CLAIM_TOPIC, vault_id),
                    (
                        entry.address.clone(),
                        share,
                        schedule.claimed_amount + total_claimable,
                    ),
                );
            }
        }

        // Update state
        vault.balance -= total_claimable;
        let new_claimed = schedule.claimed_amount + total_claimable;

        // Mark milestones as claimed
        let mut updated_milestones = Vec::new(&env);
        for milestone in schedule.milestones.iter() {
            if milestone.is_fulfilled && !milestone.claimed {
                let mut m = milestone.clone();
                m.claimed = true;
                updated_milestones.push_back(m);
            } else {
                updated_milestones.push_back(milestone);
            }
        }
        schedule.milestones = updated_milestones;
        schedule.claimed_amount = new_claimed;

        Self::save_vault(&env, vault_id, &vault);

        let sched_key = DataKey::MilestoneVestingSchedule(vault_id);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&sched_key, &schedule);
        env.storage()
            .persistent()
            .extend_ttl(&sched_key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        Ok(total_claimable)
    }
}
