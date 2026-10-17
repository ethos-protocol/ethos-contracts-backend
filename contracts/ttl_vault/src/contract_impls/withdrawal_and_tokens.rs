#[contractimpl]
impl TtlVaultContract {
    // --- Issue #565: Withdrawal Scheduling Validation ---

    /// Validates withdrawal schedules to prevent overlapping or conflicting withdrawals.
    /// Returns true if the schedule is valid, false otherwise.
    fn validate_withdrawal_schedule(
        env: &Env,
        vault_id: u64,
        new_timestamp: u64,
        _new_amount: i128,
    ) -> bool {
        let key = DataKey::WithdrawalScheduleValidation(vault_id);
        if let Some(schedules) = env
            .storage()
            .persistent()
            .get::<DataKey, Vec<WithdrawalScheduleEntry>>(&key)
        {
            for schedule in schedules.iter() {
                // Check for overlapping withdrawals (within 1 hour window)
                if (new_timestamp as i128 - schedule.timestamp as i128).abs() < 3600 {
                    return false;
                }
            }
        }
        true
    }

    /// Schedules a withdrawal with validation to prevent conflicts.
    /// Only the vault owner can call this.
    pub fn schedule_withdrawal(
        env: Env,
        vault_id: u64,
        caller: Address,
        timestamp: u64,
        amount: i128,
    ) -> Result<(), ContractError> {
        if Self::load_paused(&env) {
            return Err(ContractError::Paused);
        }
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if vault.status != ReleaseStatus::Locked {
            return Err(ContractError::AlreadyReleased);
        }

        // Validate schedule
        if !Self::validate_withdrawal_schedule(&env, vault_id, timestamp, amount) {
            return Err(ContractError::ConflictingWithdrawalSchedule);
        }

        let key = DataKey::WithdrawalScheduleValidation(vault_id);
        let mut schedules = env
            .storage()
            .persistent()
            .get::<DataKey, Vec<WithdrawalScheduleEntry>>(&key)
            .unwrap_or_else(|| Vec::new(&env));

        schedules.push_back(WithdrawalScheduleEntry { timestamp, amount });

        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, &schedules);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        env.events()
            .publish((WITHDRAWAL_VALIDATION_TOPIC, vault_id), (timestamp, amount));
        Ok(())
    }

    // --- Issue #566: Withdrawal Limits by Time ---

    /// Sets daily, weekly, and monthly withdrawal limits for a vault.
    /// Only the vault owner can call this.
    pub fn set_withdrawal_limits(
        env: Env,
        vault_id: u64,
        caller: Address,
        daily_limit: i128,
        weekly_limit: i128,
        monthly_limit: i128,
    ) -> Result<(), ContractError> {
        if Self::load_paused(&env) {
            return Err(ContractError::Paused);
        }
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }

        let limits = WithdrawalLimit {
            daily_limit,
            weekly_limit,
            monthly_limit,
        };

        let key = DataKey::WithdrawalLimit(vault_id);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, &limits);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        env.events().publish(
            (WITHDRAWAL_LIMIT_SET_TOPIC, vault_id),
            (daily_limit, weekly_limit, monthly_limit),
        );
        Ok(())
    }

    /// Gets the current withdrawal limits for a vault.
    pub fn get_withdrawal_limits(env: Env, vault_id: u64) -> Option<WithdrawalLimit> {
        env.storage()
            .persistent()
            .get(&DataKey::WithdrawalLimit(vault_id))
    }

    /// Checks if a withdrawal would exceed time-based limits.
    fn check_withdrawal_limits(
        env: &Env,
        vault_id: u64,
        amount: i128,
    ) -> Result<(), ContractError> {
        if let Some(limits) = env
            .storage()
            .persistent()
            .get::<DataKey, WithdrawalLimit>(&DataKey::WithdrawalLimit(vault_id))
        {
            let tracker_key = DataKey::WithdrawalTracker(vault_id);
            let now = env.ledger().timestamp();

            let mut tracker = env
                .storage()
                .persistent()
                .get::<DataKey, WithdrawalTracker>(&tracker_key)
                .unwrap_or_else(|| WithdrawalTracker {
                    daily_withdrawn: 0,
                    daily_reset_at: now + 86_400,
                    weekly_withdrawn: 0,
                    weekly_reset_at: now + 604_800,
                    monthly_withdrawn: 0,
                    monthly_reset_at: now + 2_592_000,
                });

            // Reset trackers if periods have passed
            if now >= tracker.daily_reset_at {
                tracker.daily_withdrawn = 0;
                tracker.daily_reset_at = now + 86_400;
            }
            if now >= tracker.weekly_reset_at {
                tracker.weekly_withdrawn = 0;
                tracker.weekly_reset_at = now + 604_800;
            }
            if now >= tracker.monthly_reset_at {
                tracker.monthly_withdrawn = 0;
                tracker.monthly_reset_at = now + 2_592_000;
            }

            // Check limits
            if tracker.daily_withdrawn + amount > limits.daily_limit {
                return Err(ContractError::DailyWithdrawalLimitExceeded);
            }
            if tracker.weekly_withdrawn + amount > limits.weekly_limit {
                return Err(ContractError::WeeklyWithdrawalLimitExceeded);
            }
            if tracker.monthly_withdrawn + amount > limits.monthly_limit {
                return Err(ContractError::MonthlyWithdrawalLimitExceeded);
            }

            // Update tracker
            tracker.daily_withdrawn += amount;
            tracker.weekly_withdrawn += amount;
            tracker.monthly_withdrawn += amount;

            let vault = Self::load_vault(env, vault_id);
            let ttl = vault_ttl_ledgers(vault.check_in_interval);
            env.storage().persistent().set(&tracker_key, &tracker);
            env.storage()
                .persistent()
                .extend_ttl(&tracker_key, VAULT_TTL_THRESHOLD, ttl);
        }
        Ok(())
    }

    // --- Issue #567: Withdrawal Destination Whitelist ---

    /// Adds an address to the withdrawal whitelist for a vault.
    /// Only the vault owner can call this.
    pub fn add_whitelist_address(
        env: Env,
        vault_id: u64,
        caller: Address,
        address: Address,
        label: String,
    ) -> Result<(), ContractError> {
        if Self::load_paused(&env) {
            return Err(ContractError::Paused);
        }
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }

        let key = DataKey::WithdrawalWhitelist(vault_id);
        let mut whitelist = env
            .storage()
            .persistent()
            .get::<DataKey, Vec<WhitelistEntry>>(&key)
            .unwrap_or_else(|| Vec::new(&env));

        let entry = WhitelistEntry {
            address: address.clone(),
            added_at: env.ledger().timestamp(),
            label,
        };
        whitelist.push_back(entry);

        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, &whitelist);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        env.events()
            .publish((WHITELIST_ADDED_TOPIC, vault_id), address);
        Ok(())
    }

    /// Removes an address from the withdrawal whitelist for a vault.
    pub fn remove_whitelist_address(
        env: Env,
        vault_id: u64,
        caller: Address,
        address: Address,
    ) -> Result<(), ContractError> {
        if Self::load_paused(&env) {
            return Err(ContractError::Paused);
        }
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }

        let key = DataKey::WithdrawalWhitelist(vault_id);
        if let Some(whitelist) = env
            .storage()
            .persistent()
            .get::<DataKey, Vec<WhitelistEntry>>(&key)
        {
            let mut new_whitelist = Vec::new(&env);
            for entry in whitelist.iter() {
                if entry.address != address {
                    new_whitelist.push_back(entry);
                }
            }
            let ttl = vault_ttl_ledgers(vault.check_in_interval);
            env.storage().persistent().set(&key, &new_whitelist);
            env.storage()
                .persistent()
                .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
            env.storage()
                .instance()
                .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        }

        env.events()
            .publish((WHITELIST_REMOVED_TOPIC, vault_id), address);
        Ok(())
    }

    /// Gets the withdrawal whitelist for a vault.
    pub fn get_whitelist(env: Env, vault_id: u64) -> Option<Vec<WhitelistEntry>> {
        env.storage()
            .persistent()
            .get(&DataKey::WithdrawalWhitelist(vault_id))
    }

    /// Checks if an address is whitelisted for withdrawals.
    fn is_whitelisted(env: &Env, vault_id: u64, address: &Address) -> bool {
        if let Some(whitelist) = env
            .storage()
            .persistent()
            .get::<DataKey, Vec<WhitelistEntry>>(&DataKey::WithdrawalWhitelist(vault_id))
        {
            for entry in whitelist.iter() {
                if entry.address == *address {
                    return true;
                }
            }
            false
        } else {
            true // No whitelist means all addresses allowed
        }
    }

    // --- Issue #568: Withdrawal Reversal ---

    /// Records a withdrawal for potential reversal within a grace period.
    fn record_withdrawal_for_reversal(
        env: &Env,
        vault_id: u64,
        amount: i128,
        grace_period_seconds: u64,
    ) {
        let counter_key = DataKey::WithdrawalReversalCounter(vault_id);
        let withdrawal_id: u64 = env.storage().persistent().get(&counter_key).unwrap_or(0u64);

        let reversal = WithdrawalReversal {
            withdrawal_id,
            amount,
            withdrawn_at: env.ledger().timestamp(),
            grace_period_until: env.ledger().timestamp() + grace_period_seconds,
            reversed: false,
        };

        let key = DataKey::WithdrawalReversal(vault_id, withdrawal_id);
        let vault = Self::load_vault(env, vault_id);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);

        env.storage().persistent().set(&key, &reversal);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .persistent()
            .set(&counter_key, &(withdrawal_id + 1));
        env.storage()
            .persistent()
            .extend_ttl(&counter_key, VAULT_TTL_THRESHOLD, ttl);
    }

    /// Reverses a withdrawal within the grace period.
    /// Only the vault owner can call this.
    pub fn reverse_withdrawal(
        env: Env,
        vault_id: u64,
        caller: Address,
        withdrawal_id: u64,
    ) -> Result<(), ContractError> {
        if Self::load_paused(&env) {
            return Err(ContractError::Paused);
        }
        caller.require_auth();
        let mut vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }

        let key = DataKey::WithdrawalReversal(vault_id, withdrawal_id);
        let mut reversal = env
            .storage()
            .persistent()
            .get::<DataKey, WithdrawalReversal>(&key)
            .ok_or(ContractError::VaultNotFound)?;

        if reversal.reversed {
            return Err(ContractError::WithdrawalAlreadyReversed);
        }

        let now = env.ledger().timestamp();
        if now > reversal.grace_period_until {
            return Err(ContractError::WithdrawalReversalGracePeriodExpired);
        }

        // Restore funds to vault
        vault.balance += reversal.amount;
        reversal.reversed = true;

        Self::save_vault(&env, vault_id, &vault);
        env.storage().persistent().set(&key, &reversal);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        env.events().publish(
            (WITHDRAWAL_REVERSED_TOPIC, vault_id),
            (withdrawal_id, reversal.amount),
        );
        Ok(())
    }

    /// Gets a withdrawal reversal record.
    pub fn get_withdrawal_reversal(
        env: Env,
        vault_id: u64,
        withdrawal_id: u64,
    ) -> Option<WithdrawalReversal> {
        env.storage()
            .persistent()
            .get(&DataKey::WithdrawalReversal(vault_id, withdrawal_id))
    }

    // --- Issue #545: Vesting Catch-Up ---

    /// Configures catch-up vesting for a vault.
    ///
    /// When enabled, a beneficiary who missed claiming periods can catch up and claim
    /// all accumulated missed installments in a single `claim_catchup_vesting` call.
    ///
    /// # Arguments
    /// * `vault_id` - The vault to configure
    /// * `caller` - Must be the vault owner
    /// * `enabled` - Whether catch-up claiming is enabled
    /// * `max_catchup_installments` - Max missed installments claimable at once (0 = unlimited)
    pub fn set_vesting_catchup(
        env: Env,
        vault_id: u64,
        caller: Address,
        enabled: bool,
        max_catchup_installments: u32,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        let config = VestingCatchUpConfig {
            enabled,
            max_catchup_installments,
        };
        let key = DataKey::VestingCatchUp(vault_id);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, &config);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (VESTING_CATCHUP_SET_TOPIC, vault_id),
            (enabled, max_catchup_installments),
        );
        Ok(())
    }

    /// Returns the catch-up vesting configuration for a vault.
    pub fn get_vesting_catchup(env: Env, vault_id: u64) -> Option<VestingCatchUpConfig> {
        env.storage()
            .persistent()
            .get(&DataKey::VestingCatchUp(vault_id))
    }

    /// Claims all accumulated missed vesting installments in one call.
    ///
    /// Requires catch-up to be enabled via `set_vesting_catchup`. Transfers all
    /// unlocked-but-unclaimed installments up to `max_catchup_installments` (if set).
    ///
    /// # Returns
    /// Total amount transferred to the beneficiary.
    ///
    /// # Errors
    /// * `ContractError::CatchUpNotEnabled` - If catch-up is not configured or disabled
    /// * `ContractError::VestingNotFound` - If no vesting schedule exists
    /// * `ContractError::AlreadyReleased` - If vault is not in Released status
    /// * `ContractError::NothingToClaimYet` - If no missed installments to catch up on
    /// * `ContractError::VestingAlreadyComplete` - If all installments already claimed
    pub fn claim_catchup_vesting(env: Env, vault_id: u64) -> Result<i128, ContractError> {
        Self::assert_not_paused(&env);
        let mut vault = Self::load_vault(&env, vault_id);

        if vault.status != ReleaseStatus::Released {
            return Err(ContractError::AlreadyReleased);
        }

        let catchup_cfg: VestingCatchUpConfig = env
            .storage()
            .persistent()
            .get(&DataKey::VestingCatchUp(vault_id))
            .ok_or(ContractError::CatchUpNotEnabled)?;

        if !catchup_cfg.enabled {
            return Err(ContractError::CatchUpNotEnabled);
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

        // Enforce cliff
        if schedule.cliff_period > 0 && now < schedule.start_time + schedule.cliff_period {
            return Err(ContractError::CliffNotReached);
        }

        let elapsed = now.saturating_sub(schedule.start_time);
        let unlocked =
            ((elapsed / schedule.interval) + 1).min(schedule.num_installments as u64) as u32;

        let mut claimable = unlocked.saturating_sub(schedule.claimed_installments);
        if claimable == 0 {
            return Err(ContractError::NothingToClaimYet);
        }

        // Apply max catch-up limit if set
        if catchup_cfg.max_catchup_installments > 0 {
            claimable = claimable.min(catchup_cfg.max_catchup_installments);
        }

        let new_claimed = schedule.claimed_installments + claimable;
        let per_installment = schedule.total_amount / schedule.num_installments as i128;
        let amount = if new_claimed >= schedule.num_installments {
            vault.balance
        } else {
            per_installment * claimable as i128
        };

        if vault.balance < amount {
            return Err(ContractError::InsufficientBalance);
        }

        let token_client = token::Client::new(&env, &vault.token_address);

        if vault.beneficiaries.is_empty() {
            crate::aml::require_compliant(&env, &vault.beneficiary);
            token_client.transfer(&env.current_contract_address(), &vault.beneficiary, &amount);
            env.events().publish(
                (VESTING_CATCHUP_CLAIMED_TOPIC, vault_id),
                (vault.beneficiary.clone(), amount, claimable),
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
                    (VESTING_CATCHUP_CLAIMED_TOPIC, vault_id),
                    (entry.address.clone(), share, claimable),
                );
            }
        }

        vault.balance -= amount;
        schedule.claimed_installments = new_claimed;
        Self::save_vault(&env, vault_id, &vault);

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

    // --- Issue #546: Vesting Bonus ---

    /// Configures a bonus for on-time vesting claims.
    ///
    /// When configured, beneficiaries who claim within `on_time_window_seconds` of an
    /// installment unlocking receive a bonus of `bonus_bps` basis points on top of
    /// their normal installment amount.
    ///
    /// # Arguments
    /// * `vault_id` - The vault to configure
    /// * `caller` - Must be the vault owner
    /// * `bonus_bps` - Bonus in basis points (e.g., 100 = 1%)
    /// * `on_time_window_seconds` - Seconds after unlock within which claim is "on time"
    pub fn set_vesting_bonus(
        env: Env,
        vault_id: u64,
        caller: Address,
        bonus_bps: u32,
        on_time_window_seconds: u64,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if bonus_bps == 0 || bonus_bps > 10_000 {
            return Err(ContractError::InvalidBps);
        }
        let config = VestingBonusConfig {
            bonus_bps,
            on_time_window_seconds,
        };
        let key = DataKey::VestingBonus(vault_id);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&key, &config);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events().publish(
            (VESTING_BONUS_SET_TOPIC, vault_id),
            (bonus_bps, on_time_window_seconds),
        );
        Ok(())
    }

    /// Returns the vesting bonus configuration for a vault.
    pub fn get_vesting_bonus(env: Env, vault_id: u64) -> Option<VestingBonusConfig> {
        env.storage()
            .persistent()
            .get(&DataKey::VestingBonus(vault_id))
    }

    /// Claims vesting installments with an on-time bonus applied if eligible.
    ///
    /// Behaves like `claim_vested_installment` but additionally checks if a bonus
    /// config exists. If all claimable installments are claimed within the on-time
    /// window, the bonus is applied on top of the base payout.
    ///
    /// # Returns
    /// Total amount transferred (base + bonus if on time).
    ///
    /// # Errors
    /// * `ContractError::BonusNotEnabled` - If no bonus config is set
    /// * `ContractError::VestingNotFound` - If no vesting schedule exists
    /// * `ContractError::AlreadyReleased` - If vault is not in Released status
    /// * `ContractError::NothingToClaimYet` - If no installments available
    /// * `ContractError::VestingAlreadyComplete` - If all installments already claimed
    /// * `ContractError::InsufficientBalance` - If vault balance is too low for bonus
    pub fn claim_with_bonus(env: Env, vault_id: u64) -> Result<i128, ContractError> {
        Self::assert_not_paused(&env);
        let mut vault = Self::load_vault(&env, vault_id);

        if vault.status != ReleaseStatus::Released {
            return Err(ContractError::AlreadyReleased);
        }

        let bonus_cfg: VestingBonusConfig = env
            .storage()
            .persistent()
            .get(&DataKey::VestingBonus(vault_id))
            .ok_or(ContractError::BonusNotEnabled)?;

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

        // Enforce cliff
        if schedule.cliff_period > 0 && now < schedule.start_time + schedule.cliff_period {
            return Err(ContractError::CliffNotReached);
        }

        let elapsed = now.saturating_sub(schedule.start_time);
        let unlocked =
            ((elapsed / schedule.interval) + 1).min(schedule.num_installments as u64) as u32;

        let claimable = unlocked.saturating_sub(schedule.claimed_installments);
        if claimable == 0 {
            return Err(ContractError::NothingToClaimYet);
        }

        let per_installment = schedule.total_amount / schedule.num_installments as i128;
        let base_amount = if unlocked >= schedule.num_installments {
            vault.balance
        } else {
            per_installment * claimable as i128
        };

        // Determine if all claimable installments are on time.
        // An installment i (1-based from claimed+1) is on time if:
        //   now <= unlock_time + on_time_window_seconds
        let mut all_on_time = true;
        for i in (schedule.claimed_installments + 1)..=unlocked {
            let unlock_time = schedule
                .start_time
                .saturating_add((i as u64 - 1).saturating_mul(schedule.interval));
            if now > unlock_time + bonus_cfg.on_time_window_seconds {
                all_on_time = false;
                break;
            }
        }

        let bonus = if all_on_time {
            base_amount * bonus_cfg.bonus_bps as i128 / 10_000
        } else {
            0
        };

        let total_amount = base_amount + bonus;

        if vault.balance < total_amount {
            // If vault can't cover the bonus, pay base only
            let fallback = base_amount.min(vault.balance);
            if vault.balance < fallback {
                return Err(ContractError::InsufficientBalance);
            }
            // Pay base without bonus
            let token_client = token::Client::new(&env, &vault.token_address);
            if vault.beneficiaries.is_empty() {
                crate::aml::require_compliant(&env, &vault.beneficiary);
                token_client.transfer(
                    &env.current_contract_address(),
                    &vault.beneficiary,
                    &fallback,
                );
                env.events().publish(
                    (VESTING_BONUS_CLAIMED_TOPIC, vault_id),
                    (vault.beneficiary.clone(), fallback, 0i128),
                );
            } else {
                let mut distributed: i128 = 0;
                let last_idx = vault.beneficiaries.len() - 1;
                for (i, entry) in vault.beneficiaries.iter().enumerate() {
                    let share = if i as u32 == last_idx {
                        fallback - distributed
                    } else {
                        fallback * (entry.bps as i128) / 10_000
                    };
                    if share > 0 {
                        crate::aml::require_compliant(&env, &entry.address);
                        token_client.transfer(
                            &env.current_contract_address(),
                            &entry.address,
                            &share,
                        );
                    }
                    distributed += share;
                    env.events().publish(
                        (VESTING_BONUS_CLAIMED_TOPIC, vault_id),
                        (entry.address.clone(), share, 0i128),
                    );
                }
            }
            vault.balance -= fallback;
            schedule.claimed_installments = unlocked;
            Self::save_vault(&env, vault_id, &vault);
            let sched_key = DataKey::VestingSchedule(vault_id, 0);
            let ttl = vault_ttl_ledgers(vault.check_in_interval);
            env.storage().persistent().set(&sched_key, &schedule);
            env.storage()
                .persistent()
                .extend_ttl(&sched_key, VAULT_TTL_THRESHOLD, ttl);
            env.storage()
                .instance()
                .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
            return Ok(fallback);
        }

        let token_client = token::Client::new(&env, &vault.token_address);

        if vault.beneficiaries.is_empty() {
            crate::aml::require_compliant(&env, &vault.beneficiary);
            token_client.transfer(
                &env.current_contract_address(),
                &vault.beneficiary,
                &total_amount,
            );
            env.events().publish(
                (VESTING_BONUS_CLAIMED_TOPIC, vault_id),
                (vault.beneficiary.clone(), total_amount, bonus),
            );
        } else {
            let mut distributed: i128 = 0;
            let last_idx = vault.beneficiaries.len() - 1;
            for (i, entry) in vault.beneficiaries.iter().enumerate() {
                let share = if i as u32 == last_idx {
                    total_amount - distributed
                } else {
                    total_amount * (entry.bps as i128) / 10_000
                };
                if share > 0 {
                    crate::aml::require_compliant(&env, &entry.address);
                    token_client.transfer(&env.current_contract_address(), &entry.address, &share);
                }
                distributed += share;
                let entry_bonus = if all_on_time {
                    share * bonus_cfg.bonus_bps as i128 / (10_000 + bonus_cfg.bonus_bps as i128)
                } else {
                    0
                };
                env.events().publish(
                    (VESTING_BONUS_CLAIMED_TOPIC, vault_id),
                    (entry.address.clone(), share, entry_bonus),
                );
            }
        }

        vault.balance -= total_amount;
        schedule.claimed_installments = unlocked;
        Self::save_vault(&env, vault_id, &vault);

        let sched_key = DataKey::VestingSchedule(vault_id, 0);
        let ttl = vault_ttl_ledgers(vault.check_in_interval);
        env.storage().persistent().set(&sched_key, &schedule);
        env.storage()
            .persistent()
            .extend_ttl(&sched_key, VAULT_TTL_THRESHOLD, ttl);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        Ok(total_amount)
    }

    // --- Issue #585: Token Lending ---

    /// Enables token lending for a vault, allowing vault tokens to be lent out for interest income.
    ///
    /// # Arguments
    /// * `vault_id` - The vault ID
    /// * `caller` - Must be the vault owner
    /// * `borrower` - Address of the borrower
    /// * `amount` - Amount to lend in stroops
    /// * `interest_rate_bps` - Annual interest rate in basis points (e.g., 500 = 5%)
    /// * `duration_seconds` - Loan duration in seconds
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::InsufficientBalance` - If vault balance is insufficient
    pub fn enable_token_lending(
        env: Env,
        vault_id: u64,
        caller: Address,
        borrower: Address,
        amount: i128,
        interest_rate_bps: u32,
        duration_seconds: u64,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if amount <= 0 || vault.balance < amount {
            return Err(ContractError::InsufficientBalance);
        }

        let now = env.ledger().timestamp();
        let lending = TokenLending {
            vault_id,
            borrower: borrower.clone(),
            amount,
            interest_rate_bps,
            lent_at: now,
            due_at: now.saturating_add(duration_seconds),
            repaid: false,
            interest_earned: 0,
        };

        let key = DataKey::TokenLending(vault_id);
        env.storage().persistent().set(&key, &lending);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, VAULT_TTL_LEDGERS);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        env.events().publish(
            (TOKEN_LENDING_TOPIC, vault_id),
            (&borrower, amount, interest_rate_bps, duration_seconds),
        );
        Ok(())
    }

    /// Repays a token loan and records the interest earned.
    ///
    /// # Arguments
    /// * `vault_id` - The vault ID
    /// * `caller` - Must be the vault owner
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::InvalidAmount` - If no active lending exists or already repaid
    pub fn repay_token_loan(
        env: Env,
        vault_id: u64,
        caller: Address,
    ) -> Result<i128, ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }

        let key = DataKey::TokenLending(vault_id);
        let mut lending: TokenLending = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::InvalidAmount)?;

        if lending.repaid {
            return Err(ContractError::InvalidAmount);
        }

        // Calculate accrued interest: amount * rate * elapsed / (10000 * 365 * 86400)
        let now = env.ledger().timestamp();
        let elapsed = now.saturating_sub(lending.lent_at);
        let interest = (lending.amount as i128)
            .checked_mul(lending.interest_rate_bps as i128)
            .and_then(|v| v.checked_mul(elapsed as i128))
            .and_then(|v| v.checked_div(10000 * 365 * 86400))
            .unwrap_or(0);

        lending.repaid = true;
        lending.interest_earned = interest;
        env.storage().persistent().set(&key, &lending);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, VAULT_TTL_LEDGERS);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        env.events().publish(
            (TOKEN_LEND_REPAY_TOPIC, vault_id),
            (lending.borrower.clone(), lending.amount, interest),
        );
        Ok(interest)
    }

    /// Returns the token lending record for a vault.
    pub fn get_token_lending(env: Env, vault_id: u64) -> Option<TokenLending> {
        env.storage()
            .persistent()
            .get(&DataKey::TokenLending(vault_id))
    }

    // --- Issue #586: Token Collateral ---

    /// Configures vault tokens as collateral for a loan.
    ///
    /// # Arguments
    /// * `vault_id` - The vault ID
    /// * `caller` - Must be the vault owner
    /// * `collateral_amount` - Amount of vault tokens used as collateral
    /// * `loan_amount` - Amount of tokens borrowed against the collateral
    /// * `collateral_ratio_bps` - Required collateral ratio in bps (e.g., 15000 = 150%)
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::InsufficientBalance` - If vault balance is insufficient
    /// * `ContractError::InvalidBps` - If collateral ratio is below 10000 (100%)
    pub fn set_token_collateral(
        env: Env,
        vault_id: u64,
        caller: Address,
        collateral_amount: i128,
        loan_amount: i128,
        collateral_ratio_bps: u32,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if collateral_amount <= 0 || vault.balance < collateral_amount {
            return Err(ContractError::InsufficientBalance);
        }
        if collateral_ratio_bps < 10_000 {
            return Err(ContractError::InvalidBps);
        }

        let collateral = TokenCollateral {
            vault_id,
            collateral_amount,
            loan_amount,
            collateral_ratio_bps,
            active: true,
            created_at: env.ledger().timestamp(),
        };

        let key = DataKey::TokenCollateral(vault_id);
        env.storage().persistent().set(&key, &collateral);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, VAULT_TTL_LEDGERS);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        env.events().publish(
            (TOKEN_COLLATERAL_TOPIC, vault_id),
            (collateral_amount, loan_amount, collateral_ratio_bps),
        );
        Ok(())
    }

    /// Releases the collateral position for a vault.
    ///
    /// # Arguments
    /// * `vault_id` - The vault ID
    /// * `caller` - Must be the vault owner
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::InvalidAmount` - If no active collateral exists
    pub fn release_token_collateral(
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

        let key = DataKey::TokenCollateral(vault_id);
        let mut collateral: TokenCollateral = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::InvalidAmount)?;

        if !collateral.active {
            return Err(ContractError::InvalidAmount);
        }

        collateral.active = false;
        env.storage().persistent().set(&key, &collateral);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, VAULT_TTL_LEDGERS);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        env.events().publish(
            (TOKEN_COLLAT_RLSD_TOPIC, vault_id),
            (collateral.collateral_amount, collateral.loan_amount),
        );
        Ok(())
    }

    /// Returns the token collateral configuration for a vault.
    pub fn get_token_collateral(env: Env, vault_id: u64) -> Option<TokenCollateral> {
        env.storage()
            .persistent()
            .get(&DataKey::TokenCollateral(vault_id))
    }

    // --- Issue #587: Token Hedging ---

    /// Enables a hedge position to protect against vault token price risk.
    ///
    /// # Arguments
    /// * `vault_id` - The vault ID
    /// * `caller` - Must be the vault owner
    /// * `hedge_token` - Token used for hedging (e.g., a stablecoin)
    /// * `notional_amount` - Notional value of the hedge position
    /// * `strike_price_bps` - Strike price in basis points relative to current price
    /// * `expiry` - Unix timestamp when the hedge expires
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::InvalidAmount` - If notional amount is not positive
    pub fn enable_token_hedge(
        env: Env,
        vault_id: u64,
        caller: Address,
        hedge_token: Address,
        notional_amount: i128,
        strike_price_bps: u32,
        expiry: u64,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if notional_amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }

        let now = env.ledger().timestamp();
        let hedge = TokenHedge {
            vault_id,
            hedge_token: hedge_token.clone(),
            notional_amount,
            strike_price_bps,
            expiry,
            active: true,
            created_at: now,
        };

        let key = DataKey::TokenHedge(vault_id);
        env.storage().persistent().set(&key, &hedge);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, VAULT_TTL_LEDGERS);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        env.events().publish(
            (TOKEN_HEDGE_TOPIC, vault_id),
            (&hedge_token, notional_amount, strike_price_bps, expiry),
        );
        Ok(())
    }

    /// Closes an active hedge position for a vault.
    ///
    /// # Arguments
    /// * `vault_id` - The vault ID
    /// * `caller` - Must be the vault owner
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::InvalidAmount` - If no active hedge exists
    pub fn close_token_hedge(
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

        let key = DataKey::TokenHedge(vault_id);
        let mut hedge: TokenHedge = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::InvalidAmount)?;

        if !hedge.active {
            return Err(ContractError::InvalidAmount);
        }

        hedge.active = false;
        env.storage().persistent().set(&key, &hedge);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, VAULT_TTL_LEDGERS);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        env.events().publish(
            (TOKEN_HEDGE_CLOSE_TOPIC, vault_id),
            (hedge.notional_amount, hedge.strike_price_bps),
        );
        Ok(())
    }

    /// Returns the token hedge configuration for a vault.
    pub fn get_token_hedge(env: Env, vault_id: u64) -> Option<TokenHedge> {
        env.storage()
            .persistent()
            .get(&DataKey::TokenHedge(vault_id))
    }

    // --- Issue #588: Token Rebalancing ---

    /// Configures automatic portfolio rebalancing based on target token weights.
    ///
    /// All `target_bps` values across the weights must sum to 10000.
    ///
    /// # Arguments
    /// * `vault_id` - The vault ID
    /// * `caller` - Must be the vault owner
    /// * `target_weights` - Per-token target allocations (basis points, must sum to 10000)
    /// * `rebalance_threshold_bps` - Drift tolerance before triggering rebalance (e.g., 500 = 5%)
    ///
    /// # Errors
    /// * `ContractError::NotOwner` - If caller is not the vault owner
    /// * `ContractError::InvalidBps` - If weights do not sum to 10000
    pub fn set_token_rebalance(
        env: Env,
        vault_id: u64,
        caller: Address,
        target_weights: Vec<TokenWeight>,
        rebalance_threshold_bps: u32,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }

        // Validate weights sum to 10000
        let total_bps: u32 = target_weights.iter().map(|w| w.target_bps).sum();
        if total_bps != 10_000 {
            return Err(ContractError::InvalidBps);
        }

        let config = TokenRebalanceConfig {
            vault_id,
            target_weights,
            last_rebalance: env.ledger().timestamp(),
            rebalance_threshold_bps,
            total_rebalances: 0,
        };

        let key = DataKey::TokenRebalance(vault_id);
        env.storage().persistent().set(&key, &config);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, VAULT_TTL_LEDGERS);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        env.events().publish(
            (TOKEN_REBALANCE_TOPIC, vault_id),
            (rebalance_threshold_bps,),
        );
        Ok(())
    }

    /// Triggers a portfolio rebalance for a vault.
    ///
    /// Records the rebalance event and updates the last rebalance timestamp and counter.
    ///
    /// # Arguments
    /// * `vault_id` - The vault ID
    ///
    /// # Errors
    /// * `ContractError::InvalidAmount` - If no rebalance config exists
    pub fn trigger_rebalance(env: Env, vault_id: u64) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        let key = DataKey::TokenRebalance(vault_id);
        let mut config: TokenRebalanceConfig = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::InvalidAmount)?;

        let now = env.ledger().timestamp();
        config.last_rebalance = now;
        config.total_rebalances = config.total_rebalances.saturating_add(1);

        env.storage().persistent().set(&key, &config);
        env.storage()
            .persistent()
            .extend_ttl(&key, VAULT_TTL_THRESHOLD, VAULT_TTL_LEDGERS);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        env.events().publish(
            (TOKEN_REBALANCED_TOPIC, vault_id),
            (config.total_rebalances, now),
        );
        Ok(())
    }

    /// Returns the token rebalancing configuration for a vault.
    pub fn get_token_rebalance(env: Env, vault_id: u64) -> Option<TokenRebalanceConfig> {
        env.storage()
            .persistent()
            .get(&DataKey::TokenRebalance(vault_id))
    }

    /// Creates a beneficiary pool from registered vault beneficiaries - Issue #529.
    ///
    /// Validates that all `members` appear in the vault's beneficiary list, sums their
    /// individual BPS allocations into `pooledAllocation[pool_id]`, and persists the pool.
    ///
    /// # Arguments
    /// * `vault_id` - The vault whose beneficiary list is used for validation
    /// * `caller`   - Must be the vault owner
    /// * `pool_id`  - Unique pool identifier chosen by the caller
    /// * `members`  - Addresses that must each be registered beneficiaries of the vault
    ///
    /// # Errors
    /// * `ContractError::NotOwner`         - caller is not the vault owner
    /// * `ContractError::InvalidBeneficiary` - a member is not in the vault's beneficiary list
    pub fn create_pool(
        env: Env,
        vault_id: u64,
        caller: Address,
        pool_id: u64,
        members: Vec<Address>,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        Self::assert_not_paused(&env);
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }

        let mut total_bps: u32 = 0;
        for member in members.iter() {
            let mut found = false;
            for entry in vault.beneficiaries.iter() {
                if entry.address == member {
                    total_bps = total_bps.saturating_add(entry.bps);
                    found = true;
                    break;
                }
            }
            if !found {
                return Err(ContractError::InvalidBeneficiary);
            }
        }

        let pool = BeneficiaryPool {
            pool_id,
            members: members.clone(),
            total_bps,
        };
        env.storage()
            .persistent()
            .set(&DataKey::BeneficiaryPool(pool_id), &pool);
        env.storage().persistent().extend_ttl(
            &DataKey::BeneficiaryPool(pool_id),
            VAULT_TTL_THRESHOLD,
            VAULT_TTL_LEDGERS,
        );
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        env.events()
            .publish((POOL_CREATED_TOPIC, pool_id), (members, total_bps));
        Ok(())
    }

    /// Returns the pool record for `pool_id`, if it exists.
    pub fn get_pool(env: Env, pool_id: u64) -> Option<BeneficiaryPool> {
        env.storage()
            .persistent()
            .get(&DataKey::BeneficiaryPool(pool_id))
    }

    // ── Issue #965: Two-Factor Authentication ────────────────────────────────

    /// Enable 2FA for a vault with the given auth method.
    pub fn enable_2fa(
        env: Env,
        vault_id: u64,
        caller: Address,
        method: u32,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        let config = TwoFactorConfigData {
            enabled: true,
            method,
        };
        env.storage()
            .persistent()
            .set(&DataKey::TwoFactorConfig(vault_id), &config);
        env.storage().persistent().extend_ttl(
            &DataKey::TwoFactorConfig(vault_id),
            VAULT_TTL_THRESHOLD,
            VAULT_TTL_LEDGERS,
        );
        env.events()
            .publish((TWO_FACTOR_ENABLED_TOPIC, vault_id), (method,));
        Ok(())
    }

    /// Disable 2FA for a vault.
    pub fn disable_2fa(env: Env, vault_id: u64, caller: Address) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        env.storage()
            .persistent()
            .remove(&DataKey::TwoFactorConfig(vault_id));
        env.storage()
            .persistent()
            .remove(&DataKey::TwoFactorVerified(vault_id));
        env.events()
            .publish((TWO_FACTOR_DISABLED_TOPIC, vault_id), ());
        Ok(())
    }

    /// Mark 2FA as verified for this vault (called by backend after OTP validation).
    /// Requires owner auth. The verification expires after 1 hour.
    pub fn confirm_2fa(env: Env, vault_id: u64, caller: Address) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        if !Self::is_2fa_enabled(&env, vault_id) {
            return Err(ContractError::TwoFactorNotEnabled);
        }
        let now = env.ledger().timestamp();
        env.storage()
            .persistent()
            .set(&DataKey::TwoFactorVerified(vault_id), &now);
        env.storage().persistent().extend_ttl(
            &DataKey::TwoFactorVerified(vault_id),
            VAULT_TTL_THRESHOLD,
            VAULT_TTL_LEDGERS,
        );
        env.events()
            .publish((TWO_FACTOR_VERIFIED_TOPIC, vault_id), (now,));
        Ok(())
    }

    /// Check if 2FA is enabled for a vault (public view function).
    pub fn is_2fa_enabled(env: &Env, vault_id: u64) -> bool {
        env.storage()
            .persistent()
            .get::<DataKey, TwoFactorConfigData>(&DataKey::TwoFactorConfig(vault_id))
            .is_some_and(|c| c.enabled)
    }

    /// Check if 2FA has been verified within the last hour (public view function).
    pub fn is_2fa_verified(env: &Env, vault_id: u64) -> bool {
        let verified_at = env
            .storage()
            .persistent()
            .get::<DataKey, u64>(&DataKey::TwoFactorVerified(vault_id));
        match verified_at {
            Some(ts) => {
                let now = env.ledger().timestamp();
                let one_hour: u64 = 3600;
                now <= ts + one_hour
            }
            None => false,
        }
    }

    // ── Issue #36: Slice Performance-Based Weighting ─────────────────────────

    /// Record one performance observation for `attestor` on `slice_id`.
    ///
    /// Only the vault owner may record performance data.
    ///
    /// - `success` — `true` if the attestor responded correctly.
    /// - `response_time_ms` — observed round-trip latency in milliseconds.
    pub fn record_attestor_performance(
        env: Env,
        vault_id: u64,
        caller: Address,
        slice_id: u64,
        attestor: Address,
        success: bool,
        response_time_ms: u64,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        slice_performance::record_attestor_performance(
            &env,
            slice_id,
            &attestor,
            success,
            response_time_ms,
        );
        Ok(())
    }

    /// Return the accumulated `PerformanceMetrics` for `attestor` on `slice_id`.
    ///
    /// Returns `None` when no observations have been recorded yet.
    pub fn get_attestor_performance(
        env: Env,
        slice_id: u64,
        attestor: Address,
    ) -> Option<slice_performance::PerformanceMetrics> {
        slice_performance::get_attestor_performance(&env, slice_id, &attestor)
    }

    /// Compute optimal BPS weights for `attestors` on `slice_id` based on
    /// stored performance data, without persisting the result.
    ///
    /// Use `reweight_slice` to both compute and persist in one call.
    pub fn calculate_optimal_weights(
        env: Env,
        slice_id: u64,
        attestors: Vec<Address>,
    ) -> Vec<slice_performance::AttestorWeight> {
        slice_performance::calculate_optimal_weights(&env, slice_id, &attestors)
    }

    /// Compute optimal weights for `attestors` on `slice_id` and persist them.
    ///
    /// Only the vault owner may trigger a reweight.
    pub fn reweight_slice(
        env: Env,
        vault_id: u64,
        caller: Address,
        slice_id: u64,
        attestors: Vec<Address>,
    ) -> Result<Vec<slice_performance::AttestorWeight>, ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        let weights = slice_performance::calculate_optimal_weights(&env, slice_id, &attestors);
        slice_performance::reweight_slice(&env, slice_id, weights.clone());
        Ok(weights)
    }

    /// Return the latest persisted BPS weights for `slice_id`.
    pub fn get_slice_weights(
        env: Env,
        slice_id: u64,
    ) -> Option<Vec<slice_performance::AttestorWeight>> {
        slice_performance::get_slice_weights(&env, slice_id)
    }

    // ── Issue #41: Slice Reputation Decay ────────────────────────────────────

    /// Apply reputation decay to an attestor on a slice due to degraded performance.
    ///
    /// Only the vault owner may apply decay.
    ///
    /// - `decay_rate_bps` — decay percentage in basis points (0-10000)
    ///   - 10000 = preserve reputation (no decay)
    ///   - 5000 = apply 50% decay
    ///   - 0 = complete decay (reputation → 0)
    /// - `reason` — descriptive reason for the decay
    ///
    /// Returns the new reputation factor (0-10000).
    pub fn apply_reputation_decay(
        env: Env,
        vault_id: u64,
        caller: Address,
        slice_id: u64,
        attestor: Address,
        decay_rate_bps: u32,
        reason: String,
    ) -> Result<u32, ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }

        // Validate decay_rate is in valid BPS range.
        if decay_rate_bps > 10_000u32 {
            return Err(ContractError::InvalidBps);
        }

        let new_reputation = slice_performance::apply_reputation_decay(
            &env,
            slice_id,
            &attestor,
            decay_rate_bps,
            reason,
        );
        Ok(new_reputation)
    }

    /// Recover reputation for an attestor when performance improves.
    ///
    /// Only the vault owner may apply recovery.
    ///
    /// - `improvement_rate_bps` — recovery percentage in basis points (0-10000)
    ///   - 10000 = maximum recovery rate
    ///   - 5000 = recover at 50% rate
    ///   - 0 = no recovery
    ///
    /// Returns the new reputation factor (0-10000).
    pub fn apply_reputation_recovery(
        env: Env,
        vault_id: u64,
        caller: Address,
        slice_id: u64,
        attestor: Address,
        improvement_rate_bps: u32,
    ) -> Result<u32, ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }

        // Validate improvement_rate is in valid BPS range.
        if improvement_rate_bps > 10_000u32 {
            return Err(ContractError::InvalidBps);
        }

        let new_reputation = slice_performance::apply_reputation_recovery(
            &env,
            slice_id,
            &attestor,
            improvement_rate_bps,
        );
        Ok(new_reputation)
    }

    /// Get the current reputation factor for an attestor on a slice.
    /// Returns 10000 if the attestor has full reputation.
    pub fn get_reputation_factor(env: Env, slice_id: u64, attestor: Address) -> u32 {
        slice_performance::get_reputation_factor(&env, slice_id, &attestor)
    }

    /// Get decay history for an attestor on a slice.
    /// Returns up to `limit` entries, most recent first.
    pub fn get_decay_history(
        env: Env,
        slice_id: u64,
        attestor: Address,
        limit: u64,
    ) -> Vec<slice_performance::DecayHistoryEntry> {
        slice_performance::get_decay_history(&env, slice_id, &attestor, limit)
    }

    // ── Issue #44: Slice Composition Validation Rules Engine ─────────────────

    /// Register a new composition rule and return its auto-assigned `rule_id`.
    ///
    /// Only the vault admin may register global rules.
    ///
    /// - `rule_bytes` — opaque policy payload.
    /// - `priority` — lower value == higher priority (0 == highest).
    /// - `tag` — numeric label/category for the rule.
    pub fn register_composition_rule(
        env: Env,
        caller: Address,
        rule_bytes: Bytes,
        priority: u32,
        tag: u32,
    ) -> Result<u64, ContractError> {
        caller.require_auth();
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, ContractError::NotInitialized));
        if caller != admin {
            return Err(ContractError::NotAdmin);
        }
        composition_rules::register_composition_rule(&env, rule_bytes, priority, tag)
            .map_err(|_| ContractError::InvalidConfig)
    }

    /// Enable or disable an existing composition rule.
    pub fn set_rule_enabled(
        env: Env,
        caller: Address,
        rule_id: u64,
        enabled: bool,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, ContractError::NotInitialized));
        if caller != admin {
            return Err(ContractError::NotAdmin);
        }
        // Verify the rule exists before mutating.
        if composition_rules::get_rule(&env, rule_id).is_none() {
            return Err(ContractError::RuleNotFound);
        }
        composition_rules::set_rule_enabled(&env, rule_id, enabled);
        Ok(())
    }

    /// Associate an ordered list of rule IDs with `slice_id`.
    ///
    /// Only the vault owner may configure which rules apply to a slice.
    pub fn set_slice_rules(
        env: Env,
        vault_id: u64,
        caller: Address,
        slice_id: u64,
        rule_ids: Vec<u64>,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        let vault = Self::load_vault(&env, vault_id);
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        composition_rules::set_slice_rules(&env, slice_id, rule_ids);
        Ok(())
    }

    /// Validate `slice_id` against all its associated enabled rules.
    ///
    /// `slice_data` is the raw bytes representation of the slice composition
    /// being validated.  Returns a `ValidationResult` with per-rule outcomes,
    /// any detected conflicts, and an `overall_valid` flag.
    pub fn validate_slice_with_rules(
        env: Env,
        slice_id: u64,
        slice_data: Bytes,
    ) -> composition_rules::ValidationResult {
        composition_rules::validate_slice_with_rules(&env, slice_id, &slice_data)
    }

    /// Retrieve a single composition rule by ID.
    pub fn get_composition_rule(
        env: Env,
        rule_id: u64,
    ) -> Option<composition_rules::CompositionRule> {
        composition_rules::get_rule(&env, rule_id)
    }

    /// Retrieve the rule IDs associated with `slice_id`.
    pub fn get_slice_rule_ids(env: Env, slice_id: u64) -> Vec<u64> {
        composition_rules::get_slice_rules(&env, slice_id)
    }

    // ── Issue #32: Credential Anchoring to External Systems ───────────────────

    /// Anchor `credential_id` to `external_id` within `system`.
    ///
    /// This contract does not track credential ownership, so any caller may
    /// register an anchor as long as they authorize the call themselves.
    ///
    /// # Errors
    /// * `InvalidExternalId` — `external_id` is empty or exceeds
    ///   `credential_anchoring::MAX_EXTERNAL_ID_LEN`.
    /// * `InvalidAnchorSystem` — `system` is empty or exceeds
    ///   `credential_anchoring::MAX_SYSTEM_LEN`.
    /// * `AnchorAlreadyExists` — an anchor for `(external_id, system)` already
    ///   exists; remove it first via `remove_credential_anchor`.
    pub fn create_credential_anchor(
        env: Env,
        caller: Address,
        credential_id: u64,
        external_id: Bytes,
        system: Bytes,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        if external_id.is_empty() || external_id.len() > credential_anchoring::MAX_EXTERNAL_ID_LEN {
            return Err(ContractError::InvalidExternalId);
        }
        if system.is_empty() || system.len() > credential_anchoring::MAX_SYSTEM_LEN {
            return Err(ContractError::InvalidAnchorSystem);
        }
        if credential_anchoring::create_credential_anchor(&env, credential_id, external_id, system)
        {
            Ok(())
        } else {
            Err(ContractError::AnchorAlreadyExists)
        }
    }

    /// Look up the credential ID anchored to `(external_id, system)`, if any.
    /// Read-only; no authorization required.
    pub fn verify_external_anchor(env: Env, external_id: Bytes, system: Bytes) -> Option<u64> {
        credential_anchoring::verify_external_anchor(&env, &external_id, &system)
    }

    /// Remove the anchor for `(external_id, system)` from `credential_id`.
    ///
    /// # Errors
    /// * `AnchorNotFound` — no anchor exists for `(external_id, system)`.
    pub fn remove_credential_anchor(
        env: Env,
        caller: Address,
        credential_id: u64,
        external_id: Bytes,
        system: Bytes,
    ) -> Result<(), ContractError> {
        caller.require_auth();
        if credential_anchoring::remove_credential_anchor(
            &env,
            credential_id,
            &external_id,
            &system,
        ) {
            Ok(())
        } else {
            Err(ContractError::AnchorNotFound)
        }
    }

    /// Retrieve all anchors registered for `credential_id`.
    /// Read-only; no authorization required.
    pub fn get_credential_anchors(
        env: Env,
        credential_id: u64,
    ) -> Vec<credential_anchoring::CredentialAnchor> {
        credential_anchoring::get_credential_anchors(&env, credential_id)
    }

    // ── Issue #269: Credential Lifecycle State Machine ────────────────────────

    /// Initialize a credential with Draft state.
    ///
    /// No-op if the credential already has a state, so an existing (including
    /// Revoked/Archived) credential can never be reset back to Draft.
    ///
    /// # Arguments
    /// * `credential_id` - The credential ID to initialize
    ///
    /// # Panics
    /// * Panics if the contract is paused
    pub fn init_credential(env: Env, credential_id: u64) {
        Self::assert_not_paused(&env);
        credential_lifecycle::init_credential_state(&env, credential_id);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
    }

    /// Get the current lifecycle state of a credential.
    ///
    /// # Arguments
    /// * `credential_id` - The credential ID to query
    ///
    /// # Returns
    /// The current CredentialState (Draft, Active, Suspended, Revoked, Expired, or Archived)
    pub fn get_credential_state(
        env: Env,
        credential_id: u64,
    ) -> credential_lifecycle::CredentialState {
        credential_lifecycle::get_credential_state(&env, credential_id)
    }

    /// Activate a credential (transition from Draft to Active).
    ///
    /// Only the contract admin can activate credentials.
    ///
    /// # Arguments
    /// * `caller` - The address attempting to activate the credential
    /// * `credential_id` - The credential ID to activate
    ///
    /// # Returns
    /// `Ok(())` if successful, `Err(ContractError)` if transition is invalid
    ///
    /// # Panics
    /// * Panics if caller is not the admin
    /// * Panics if the contract is paused
    pub fn activate_credential(
        env: Env,
        caller: Address,
        credential_id: u64,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        Self::require_admin(&env);

        let success = credential_lifecycle::transition_credential_state(
            &env,
            credential_id,
            credential_lifecycle::CredentialState::Active,
        );

        if !success {
            return Err(ContractError::InvalidStateTransition);
        }

        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        Ok(())
    }

    /// Suspend a credential (transition to Suspended state).
    ///
    /// Only the contract admin can suspend credentials.
    /// Suspended credentials can be reactivated later.
    ///
    /// # Arguments
    /// * `caller` - The address attempting to suspend the credential
    /// * `credential_id` - The credential ID to suspend
    ///
    /// # Returns
    /// `Ok(())` if successful, `Err(ContractError)` if transition is invalid
    ///
    /// # Panics
    /// * Panics if caller is not the admin
    /// * Panics if the contract is paused
    pub fn suspend_credential(
        env: Env,
        caller: Address,
        credential_id: u64,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        Self::require_admin(&env);

        let success = credential_lifecycle::transition_credential_state(
            &env,
            credential_id,
            credential_lifecycle::CredentialState::Suspended,
        );

        if !success {
            return Err(ContractError::InvalidStateTransition);
        }

        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        Ok(())
    }

    /// Revoke a credential (transition to Revoked state).
    ///
    /// Only the contract admin can revoke credentials.
    /// Revoked credentials cannot be reactivated - this is a terminal state.
    ///
    /// # Arguments
    /// * `caller` - The address attempting to revoke the credential
    /// * `credential_id` - The credential ID to revoke
    ///
    /// # Returns
    /// `Ok(())` if successful, `Err(ContractError)` if transition is invalid
    ///
    /// # Panics
    /// * Panics if caller is not the admin
    /// * Panics if the contract is paused
    pub fn revoke_credential(
        env: Env,
        caller: Address,
        credential_id: u64,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        Self::require_admin(&env);

        let success = credential_lifecycle::transition_credential_state(
            &env,
            credential_id,
            credential_lifecycle::CredentialState::Revoked,
        );

        if !success {
            return Err(ContractError::InvalidStateTransition);
        }

        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        Ok(())
    }

    /// Archive a credential (transition to Archived state).
    ///
    /// Only the contract admin can archive credentials.
    /// Archived credentials are read-only and cannot transition to other states.
    ///
    /// # Arguments
    /// * `caller` - The address attempting to archive the credential
    /// * `credential_id` - The credential ID to archive
    ///
    /// # Returns
    /// `Ok(())` if successful, `Err(ContractError)` if transition is invalid
    ///
    /// # Panics
    /// * Panics if caller is not the admin
    /// * Panics if the contract is paused
    pub fn archive_credential(
        env: Env,
        caller: Address,
        credential_id: u64,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        Self::require_admin(&env);

        let success = credential_lifecycle::transition_credential_state(
            &env,
            credential_id,
            credential_lifecycle::CredentialState::Archived,
        );

        if !success {
            return Err(ContractError::InvalidStateTransition);
        }

        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        Ok(())
    }

    /// Mark a credential as expired (transition to Expired state).
    ///
    /// Only the contract admin can mark credentials as expired.
    ///
    /// # Arguments
    /// * `caller` - The address attempting to expire the credential
    /// * `credential_id` - The credential ID to expire
    ///
    /// # Returns
    /// `Ok(())` if successful, `Err(ContractError)` if transition is invalid
    ///
    /// # Panics
    /// * Panics if caller is not the admin
    /// * Panics if the contract is paused
    pub fn expire_credential(
        env: Env,
        caller: Address,
        credential_id: u64,
    ) -> Result<(), ContractError> {
        Self::assert_not_paused(&env);
        caller.require_auth();
        Self::require_admin(&env);

        let success = credential_lifecycle::transition_credential_state(
            &env,
            credential_id,
            credential_lifecycle::CredentialState::Expired,
        );

        if !success {
            return Err(ContractError::InvalidStateTransition);
        }

        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);
        Ok(())
    }

    // ── Issue #35: Slice Failover Mechanism ────────────────────────────────────

    /// Register a backup slice for a primary slice.
    ///
    /// # Errors
    /// * `ContractError::VaultNotFound` - vault does not exist
    /// * `ContractError::NotOwner` - caller is not the vault owner
    /// * `ContractError::InvalidSlice` - primary and backup slice IDs are equal
    pub fn register_backup_slice(
        env: Env,
        vault_id: u64,
        caller: Address,
        primary_slice_id: u64,
        backup_slice_id: u64,
        failure_threshold: u32,
    ) -> Result<u64, ContractError> {
        caller.require_auth();
        let vault = Self::try_load_vault(&env, vault_id).ok_or(ContractError::VaultNotFound)?;
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        Ok(slice_failover::register_backup_slice(
            &env,
            primary_slice_id,
            backup_slice_id,
            failure_threshold,
        ))
    }

    /// Get the list of backup slices registered for a primary slice.
    /// Read-only; no authorization required.
    pub fn get_backup_slices(env: Env, primary_slice_id: u64) -> Vec<u64> {
        slice_failover::get_backup_slices(&env, primary_slice_id)
    }

    /// Get the currently active slice for a primary slice (itself if no failover).
    /// Read-only; no authorization required.
    pub fn get_active_slice(env: Env, slice_id: u64) -> u64 {
        slice_failover::get_active_slice(&env, slice_id)
    }

    /// Get the current recorded failure count for a slice.
    /// Read-only; no authorization required.
    pub fn get_failure_count(env: Env, slice_id: u64) -> u32 {
        slice_failover::get_failure_count(&env, slice_id)
    }

    /// Record a failure for a slice. Automatically activates failover to the
    /// registered backup once the configured failure threshold is reached.
    ///
    /// # Errors
    /// * `ContractError::VaultNotFound` - vault does not exist
    /// * `ContractError::NotOwner` - caller is not the vault owner
    pub fn record_slice_failure(
        env: Env,
        vault_id: u64,
        caller: Address,
        primary_slice_id: u64,
        reason: slice_failover::FailoverReason,
    ) -> Result<bool, ContractError> {
        caller.require_auth();
        let vault = Self::try_load_vault(&env, vault_id).ok_or(ContractError::VaultNotFound)?;
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        Ok(slice_failover::record_slice_failure(
            &env,
            primary_slice_id,
            reason,
        ))
    }

    /// Explicitly activate failover from a primary slice to a registered backup.
    ///
    /// # Errors
    /// * `ContractError::VaultNotFound` - vault does not exist
    /// * `ContractError::NotOwner` - caller is not the vault owner
    pub fn activate_failover(
        env: Env,
        vault_id: u64,
        caller: Address,
        primary_slice_id: u64,
        backup_slice_id: u64,
        reason: slice_failover::FailoverReason,
    ) -> Result<bool, ContractError> {
        caller.require_auth();
        let vault = Self::try_load_vault(&env, vault_id).ok_or(ContractError::VaultNotFound)?;
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        Ok(slice_failover::activate_failover(
            &env,
            primary_slice_id,
            backup_slice_id,
            reason,
        ))
    }

    /// Revert an active failover, restoring the primary slice and resetting
    /// its failure counter.
    ///
    /// # Errors
    /// * `ContractError::VaultNotFound` - vault does not exist
    /// * `ContractError::NotOwner` - caller is not the vault owner
    pub fn revert_failover(
        env: Env,
        vault_id: u64,
        caller: Address,
        primary_slice_id: u64,
        backup_slice_id: u64,
    ) -> Result<bool, ContractError> {
        caller.require_auth();
        let vault = Self::try_load_vault(&env, vault_id).ok_or(ContractError::VaultNotFound)?;
        if caller != vault.owner {
            return Err(ContractError::NotOwner);
        }
        Ok(slice_failover::revert_failover(
            &env,
            primary_slice_id,
            backup_slice_id,
        ))
    }
}
