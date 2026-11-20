//! Vault yield generation (#608).
//!
//! Idle vault funds are placed into a yield protocol. The protocol sits behind
//! [`YieldProtocol`], so a live adapter (for example a Soroban lending or
//! staking pool) can be plugged in without changing the accounting here.
//!
//! Accounting, in the smallest token unit:
//! * `total_principal` is the sum of every vault's deposited principal.
//! * `outstanding` is yield credited to vaults but not yet claimed.
//! * `unallocated` is protocol value that arrived while no principal was
//!   deposited. No vault has a claim on it, so it is never credited later.
//! * A harvest compares the protocol value with
//!   `total_principal + outstanding + unallocated`. Any surplus is credited to
//!   vaults pro rata to principal through a cumulative index. A vault's share
//!   therefore never depends on when other vaults act.
//! * Harvesting runs before every principal change, so a new deposit does not
//!   earn yield that accrued before it arrived.
//! * A protocol loss (value below the backing) credits nothing. It is not
//!   socialised here: claims fail with [`YieldError::InsufficientLiquidity`]
//!   until the protocol recovers.
//! * Rounding is always downward. Per-vault shares sum to at most
//!   `outstanding`, and the difference is dust that stays in the protocol.

use std::collections::BTreeMap;
use std::fmt;

const BPS_DENOMINATOR: u128 = 10_000;
const SECONDS_PER_YEAR: u128 = 365 * 86_400;
/// Fixed-point scale for the per-unit-principal yield index.
const INDEX_SCALE: u128 = 1_000_000_000_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YieldError {
    ZeroAmount,
    UnknownVault,
    InsufficientPrincipal,
    InsufficientYield,
    InsufficientLiquidity,
    Overflow,
}

impl fmt::Display for YieldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::ZeroAmount => "amount must be greater than zero",
            Self::UnknownVault => "vault has no yield position",
            Self::InsufficientPrincipal => "withdrawal exceeds deposited principal",
            Self::InsufficientYield => "claim exceeds accrued yield",
            Self::InsufficientLiquidity => "protocol cannot pay the requested amount",
            Self::Overflow => "arithmetic overflow",
        };
        f.write_str(message)
    }
}

impl std::error::Error for YieldError {}

/// A yield-generating venue that holds vault funds.
///
/// Implementations report the value of the whole position, including yield
/// they have earned, so the manager can measure gains without knowing how the
/// protocol produces them.
pub trait YieldProtocol {
    /// Adds `amount` to the position at time `now`.
    fn deposit(&mut self, now: u64, amount: u128) -> Result<(), YieldError>;

    /// Removes `amount` from the position. Fails if the position holds less.
    fn withdraw(&mut self, now: u64, amount: u128) -> Result<(), YieldError>;

    /// Value of the whole position at time `now`, including accrued yield.
    fn value(&mut self, now: u64) -> Result<u128, YieldError>;
}

/// Simple-interest protocol at a fixed annual rate.
///
/// Stands in for a live adapter in tests and simulations.
#[derive(Debug, Clone)]
pub struct FixedRateProtocol {
    rate_bps: u32,
    value: u128,
    last_update: u64,
}

impl FixedRateProtocol {
    /// Creates an empty position that starts accruing at `now`.
    pub fn new(rate_bps: u32, now: u64) -> Self {
        Self {
            rate_bps,
            value: 0,
            last_update: now,
        }
    }

    fn accrue(&mut self, now: u64) -> Result<(), YieldError> {
        let elapsed = u128::from(now.saturating_sub(self.last_update));
        let gain = self
            .value
            .checked_mul(u128::from(self.rate_bps))
            .and_then(|v| v.checked_mul(elapsed))
            .ok_or(YieldError::Overflow)?
            / (BPS_DENOMINATOR * SECONDS_PER_YEAR);
        self.value = self.value.checked_add(gain).ok_or(YieldError::Overflow)?;
        self.last_update = self.last_update.max(now);
        Ok(())
    }
}

impl YieldProtocol for FixedRateProtocol {
    fn deposit(&mut self, now: u64, amount: u128) -> Result<(), YieldError> {
        self.accrue(now)?;
        self.value = self.value.checked_add(amount).ok_or(YieldError::Overflow)?;
        Ok(())
    }

    fn withdraw(&mut self, now: u64, amount: u128) -> Result<(), YieldError> {
        self.accrue(now)?;
        self.value = self
            .value
            .checked_sub(amount)
            .ok_or(YieldError::InsufficientLiquidity)?;
        Ok(())
    }

    fn value(&mut self, now: u64) -> Result<u128, YieldError> {
        self.accrue(now)?;
        Ok(self.value)
    }
}

/// One vault's yield position, as of the last harvest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub principal: u128,
    /// Yield credited to this vault and not yet claimed.
    pub accrued: u128,
    /// Yield this vault has claimed over its lifetime.
    pub claimed: u128,
    index_snapshot: u128,
}

impl Position {
    fn new(index: u128) -> Self {
        Self {
            principal: 0,
            accrued: 0,
            claimed: 0,
            index_snapshot: index,
        }
    }

    /// Credits yield earned since the last snapshot and moves the snapshot up.
    fn settle(&mut self, index: u128) -> Result<(), YieldError> {
        self.accrued = self
            .accrued
            .checked_add(self.earned_since(index)?)
            .ok_or(YieldError::Overflow)?;
        self.index_snapshot = index;
        Ok(())
    }

    fn earned_since(&self, index: u128) -> Result<u128, YieldError> {
        let delta = index
            .checked_sub(self.index_snapshot)
            .ok_or(YieldError::Overflow)?;
        Ok(self
            .principal
            .checked_mul(delta)
            .ok_or(YieldError::Overflow)?
            / INDEX_SCALE)
    }
}

/// Tracks vault principal and yield on top of a [`YieldProtocol`].
#[derive(Debug)]
pub struct YieldManager<P: YieldProtocol> {
    protocol: P,
    positions: BTreeMap<u64, Position>,
    total_principal: u128,
    outstanding: u128,
    unallocated: u128,
    index: u128,
}

impl<P: YieldProtocol> YieldManager<P> {
    pub fn new(protocol: P) -> Self {
        Self {
            protocol,
            positions: BTreeMap::new(),
            total_principal: 0,
            outstanding: 0,
            unallocated: 0,
            index: 0,
        }
    }

    /// Places `amount` of `vault_id`'s funds into the protocol.
    pub fn deposit(&mut self, vault_id: u64, amount: u128, now: u64) -> Result<(), YieldError> {
        if amount == 0 {
            return Err(YieldError::ZeroAmount);
        }
        self.harvest(now)?;
        self.protocol.deposit(now, amount)?;

        let index = self.index;
        let mut position = self
            .positions
            .get(&vault_id)
            .copied()
            .unwrap_or_else(|| Position::new(index));
        position.settle(index)?;
        position.principal = position
            .principal
            .checked_add(amount)
            .ok_or(YieldError::Overflow)?;
        self.total_principal = self
            .total_principal
            .checked_add(amount)
            .ok_or(YieldError::Overflow)?;
        self.positions.insert(vault_id, position);
        Ok(())
    }

    /// Takes `amount` of principal out of the protocol for `vault_id`.
    /// Yield earned so far stays with the vault and can be claimed separately.
    pub fn withdraw(&mut self, vault_id: u64, amount: u128, now: u64) -> Result<(), YieldError> {
        if amount == 0 {
            return Err(YieldError::ZeroAmount);
        }
        self.harvest(now)?;

        let index = self.index;
        let mut position = *self
            .positions
            .get(&vault_id)
            .ok_or(YieldError::UnknownVault)?;
        if position.principal < amount {
            return Err(YieldError::InsufficientPrincipal);
        }
        position.settle(index)?;
        self.protocol.withdraw(now, amount)?;

        position.principal = position
            .principal
            .checked_sub(amount)
            .ok_or(YieldError::InsufficientPrincipal)?;
        self.total_principal = self
            .total_principal
            .checked_sub(amount)
            .ok_or(YieldError::InsufficientPrincipal)?;
        self.positions.insert(vault_id, position);
        Ok(())
    }

    /// Measures the protocol value and credits any surplus to vaults.
    /// Returns the amount credited to vaults in this call.
    pub fn harvest(&mut self, now: u64) -> Result<u128, YieldError> {
        let value = self.protocol.value(now)?;
        let backing = self
            .total_principal
            .checked_add(self.outstanding)
            .and_then(|v| v.checked_add(self.unallocated))
            .ok_or(YieldError::Overflow)?;
        let gain = value.saturating_sub(backing);
        if gain == 0 {
            return Ok(0);
        }
        if self.total_principal == 0 {
            self.unallocated = self
                .unallocated
                .checked_add(gain)
                .ok_or(YieldError::Overflow)?;
            return Ok(0);
        }

        let index_delta =
            gain.checked_mul(INDEX_SCALE).ok_or(YieldError::Overflow)? / self.total_principal;
        let distributed = index_delta
            .checked_mul(self.total_principal)
            .ok_or(YieldError::Overflow)?
            / INDEX_SCALE;

        self.index = self
            .index
            .checked_add(index_delta)
            .ok_or(YieldError::Overflow)?;
        self.outstanding = self
            .outstanding
            .checked_add(distributed)
            .ok_or(YieldError::Overflow)?;
        Ok(distributed)
    }

    /// Pays out `amount` of accrued yield to `vault_id`.
    ///
    /// Authorization is the caller's responsibility. The payout is withdrawn
    /// from the protocol, and any failure leaves the position unchanged.
    pub fn claim_yield(
        &mut self,
        vault_id: u64,
        amount: u128,
        now: u64,
    ) -> Result<u128, YieldError> {
        if amount == 0 {
            return Err(YieldError::ZeroAmount);
        }
        self.harvest(now)?;

        let index = self.index;
        let mut position = *self
            .positions
            .get(&vault_id)
            .ok_or(YieldError::UnknownVault)?;
        position.settle(index)?;
        if position.accrued < amount {
            return Err(YieldError::InsufficientYield);
        }
        self.protocol.withdraw(now, amount)?;

        position.accrued = position
            .accrued
            .checked_sub(amount)
            .ok_or(YieldError::InsufficientYield)?;
        position.claimed = position
            .claimed
            .checked_add(amount)
            .ok_or(YieldError::Overflow)?;
        self.outstanding = self
            .outstanding
            .checked_sub(amount)
            .ok_or(YieldError::InsufficientYield)?;
        self.positions.insert(vault_id, position);
        Ok(amount)
    }

    /// Position as of the last harvest. Call [`Self::harvest`] first for a
    /// current figure.
    pub fn position(&self, vault_id: u64) -> Option<Position> {
        self.positions.get(&vault_id).copied()
    }

    /// Yield credited to `vault_id` as of the last harvest, claimed or not.
    pub fn pending_yield(&self, vault_id: u64) -> Result<u128, YieldError> {
        let position = self
            .positions
            .get(&vault_id)
            .ok_or(YieldError::UnknownVault)?;
        position
            .accrued
            .checked_add(position.earned_since(self.index)?)
            .ok_or(YieldError::Overflow)
    }

    pub fn total_principal(&self) -> u128 {
        self.total_principal
    }

    /// Yield credited to vaults and not yet claimed.
    pub fn outstanding_yield(&self) -> u128 {
        self.outstanding
    }

    /// Protocol value that no vault can claim.
    pub fn unallocated(&self) -> u128 {
        self.unallocated
    }

    pub fn protocol(&self) -> &P {
        &self.protocol
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const YEAR: u64 = 365 * 86_400;

    fn manager(rate_bps: u32) -> YieldManager<FixedRateProtocol> {
        YieldManager::new(FixedRateProtocol::new(rate_bps, 0))
    }

    /// Protocol whose value is set directly, to model gains and losses.
    #[derive(Debug, Default)]
    struct StaticProtocol {
        value: u128,
    }

    impl YieldProtocol for StaticProtocol {
        fn deposit(&mut self, _now: u64, amount: u128) -> Result<(), YieldError> {
            self.value = self.value.checked_add(amount).ok_or(YieldError::Overflow)?;
            Ok(())
        }

        fn withdraw(&mut self, _now: u64, amount: u128) -> Result<(), YieldError> {
            self.value = self
                .value
                .checked_sub(amount)
                .ok_or(YieldError::InsufficientLiquidity)?;
            Ok(())
        }

        fn value(&mut self, _now: u64) -> Result<u128, YieldError> {
            Ok(self.value)
        }
    }

    #[test]
    fn deposit_moves_principal_into_protocol() -> Result<(), YieldError> {
        let mut m = manager(500);
        m.deposit(1, 1_000, 0)?;

        assert_eq!(m.total_principal(), 1_000);
        assert_eq!(
            m.position(1).expect("position must exist after deposit").principal,
            1_000
        );
        assert_eq!(m.protocol().value, 1_000);
        Ok(())
    }

    #[test]
    fn zero_amounts_are_rejected() -> Result<(), YieldError> {
        let mut m = manager(500);
        assert_eq!(m.deposit(1, 0, 0), Err(YieldError::ZeroAmount));

        m.deposit(1, 100, 0)?;
        assert_eq!(m.withdraw(1, 0, 0), Err(YieldError::ZeroAmount));
        assert_eq!(m.claim_yield(1, 0, 0), Err(YieldError::ZeroAmount));
        Ok(())
    }

    #[test]
    fn yield_accrues_pro_rata_to_principal() -> Result<(), YieldError> {
        let mut m = manager(500);
        m.deposit(1, 1_000, 0)?;
        m.deposit(2, 3_000, 0)?;

        // 4_000 at 5% for a year earns 200.
        assert_eq!(m.harvest(YEAR)?, 200);
        assert_eq!(m.pending_yield(1)?, 50);
        assert_eq!(m.pending_yield(2)?, 150);
        assert_eq!(m.outstanding_yield(), 200);
        Ok(())
    }

    #[test]
    fn late_depositor_does_not_earn_earlier_yield() -> Result<(), YieldError> {
        let mut m = manager(500);
        m.deposit(1, 1_000, 0)?;

        // Vault 2 joins after the first year's yield has accrued.
        m.deposit(2, 1_000, YEAR)?;
        assert_eq!(m.pending_yield(2)?, 0);
        assert_eq!(m.pending_yield(1)?, 50);

        // Second year: 2_050 earns 102 (rounded down), split 51 and 51.
        m.harvest(2 * YEAR)?;
        assert_eq!(m.pending_yield(1)?, 101);
        assert_eq!(m.pending_yield(2)?, 51);
        Ok(())
    }

    #[test]
    fn withdraw_settles_yield_before_reducing_principal() -> Result<(), YieldError> {
        let mut m = manager(500);
        m.deposit(1, 1_000, 0)?;

        m.withdraw(1, 600, YEAR)?;
        let position = m.position(1).expect("position must exist after withdraw");
        assert_eq!(position.principal, 400);
        assert_eq!(position.accrued, 50);

        // Later yield is earned on the reduced principal only: 450 earns 22.
        m.harvest(2 * YEAR)?;
        assert_eq!(m.pending_yield(1)?, 50 + 22);
        Ok(())
    }

    #[test]
    fn withdraw_rejects_unknown_vault_and_excess_principal() -> Result<(), YieldError> {
        let mut m = manager(500);
        assert_eq!(m.withdraw(9, 1, 0), Err(YieldError::UnknownVault));

        m.deposit(1, 100, 0)?;
        assert_eq!(
            m.withdraw(1, 101, 0),
            Err(YieldError::InsufficientPrincipal)
        );
        assert_eq!(
            m.position(1).expect("position must exist after deposit").principal,
            100
        );
        Ok(())
    }

    #[test]
    fn claim_pays_out_and_updates_per_vault_totals() -> Result<(), YieldError> {
        let mut m = manager(500);
        m.deposit(1, 1_000, 0)?;
        m.harvest(YEAR)?;

        assert_eq!(m.claim_yield(1, 30, YEAR)?, 30);
        let position = m.position(1).expect("position must exist after claim");
        assert_eq!(position.accrued, 20);
        assert_eq!(position.claimed, 30);
        assert_eq!(m.outstanding_yield(), 20);
        assert_eq!(m.protocol().value, 1_020);
        Ok(())
    }

    #[test]
    fn claim_rejects_more_than_accrued() -> Result<(), YieldError> {
        let mut m = manager(500);
        m.deposit(1, 1_000, 0)?;

        assert_eq!(
            m.claim_yield(1, 51, YEAR),
            Err(YieldError::InsufficientYield)
        );
        assert_eq!(m.claim_yield(9, 1, YEAR), Err(YieldError::UnknownVault));
        assert_eq!(m.pending_yield(1)?, 50);
        Ok(())
    }

    #[test]
    fn protocol_loss_credits_nothing_and_recovery_credits_only_new_gain() -> Result<(), YieldError>
    {
        let mut m = YieldManager::new(StaticProtocol::default());
        m.deposit(1, 1_000, 0)?;

        m.protocol.value = 900;
        assert_eq!(m.harvest(1)?, 0);
        assert_eq!(m.outstanding_yield(), 0);

        // Value must climb back above principal before anything is credited.
        m.protocol.value = 1_100;
        assert_eq!(m.harvest(2)?, 100);
        assert_eq!(m.pending_yield(1)?, 100);
        Ok(())
    }

    #[test]
    fn claim_fails_when_protocol_cannot_pay() -> Result<(), YieldError> {
        let mut m = YieldManager::new(StaticProtocol::default());
        m.deposit(1, 1_000, 0)?;
        m.protocol.value = 1_050;
        m.harvest(1)?;

        m.protocol.value = 30;
        assert_eq!(
            m.claim_yield(1, 50, 2),
            Err(YieldError::InsufficientLiquidity)
        );
        // The failed claim leaves the position and the outstanding total alone.
        assert_eq!(m.pending_yield(1)?, 50);
        assert_eq!(
            m.position(1).expect("position must exist after deposit").claimed,
            0
        );
        assert_eq!(m.outstanding_yield(), 50);
        Ok(())
    }

    #[test]
    fn value_arriving_with_no_principal_is_not_credited_to_later_depositors(
    ) -> Result<(), YieldError> {
        let mut m = YieldManager::new(StaticProtocol::default());
        m.protocol.value = 100;
        assert_eq!(m.harvest(1)?, 0);
        assert_eq!(m.unallocated(), 100);

        m.deposit(1, 1_000, 2)?;
        assert_eq!(m.harvest(3)?, 0);
        assert_eq!(m.pending_yield(1)?, 0);
        assert_eq!(m.unallocated(), 100);
        Ok(())
    }

    #[test]
    fn rounding_dust_never_exceeds_outstanding_yield() -> Result<(), YieldError> {
        let mut m = manager(333);
        m.deposit(1, 333, 0)?;
        m.deposit(2, 333, 0)?;
        m.deposit(3, 334, 0)?;
        m.harvest(YEAR / 3)?;
        m.deposit(4, 7, YEAR / 2)?;
        m.harvest(YEAR)?;

        let mut sum: u128 = 0;
        for id in 1..=4 {
            sum = sum
                .checked_add(m.pending_yield(id)?)
                .ok_or(YieldError::Overflow)?;
        }
        let outstanding = m.outstanding_yield();
        assert!(sum <= outstanding);
        // Each vault can lose less than one unit per harvest (3 harvests, 4 vaults).
        assert!(outstanding - sum <= 12);
        Ok(())
    }

    #[test]
    fn yield_is_claimable_after_principal_is_fully_withdrawn() -> Result<(), YieldError> {
        let mut m = manager(500);
        m.deposit(1, 1_000, 0)?;
        m.withdraw(1, 1_000, YEAR)?;

        assert_eq!(m.claim_yield(1, 50, YEAR)?, 50);
        assert_eq!(
            m.position(1).expect("position must exist after claim").principal,
            0
        );
        assert_eq!(m.total_principal(), 0);
        Ok(())
    }

    #[test]
    fn pending_yield_rejects_unknown_vault() {
        let m = manager(500);
        assert_eq!(m.pending_yield(42), Err(YieldError::UnknownVault));
    }

    #[test]
    fn fixed_rate_protocol_reports_overflow() {
        let mut protocol = FixedRateProtocol::new(u32::MAX, 0);
        protocol.value = u128::MAX;
        assert_eq!(protocol.value(YEAR), Err(YieldError::Overflow));
    }

    #[test]
    fn fixed_rate_protocol_rejects_over_withdrawal() {
        let mut protocol = FixedRateProtocol::new(0, 0);
        assert_eq!(
            protocol.withdraw(0, 1),
            Err(YieldError::InsufficientLiquidity)
        );
    }

    #[test]
    fn harvest_reports_overflow_when_index_delta_overflows() {
        let mut m = YieldManager::new(StaticProtocol::default());
        m.deposit(1, 1, 0)?;
        m.protocol.value = u128::MAX;
        assert_eq!(m.harvest(1), Err(YieldError::Overflow));
        Ok::<(), YieldError>(())
    }
}
