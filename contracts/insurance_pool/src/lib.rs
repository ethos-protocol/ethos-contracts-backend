//! Vault insurance pool (#607).
//!
//! Vaults have no protection against loss. This contract lets a vault owner buy
//! coverage for a vault, pay a premium, and file a claim against that coverage.
//! An admin reviews each claim and approves or rejects it.
//!
//! Accounting:
//! * `Reserve` is the pool's capital: funder deposits plus premiums, less
//!   payouts. The contract's token balance always equals `Reserve`.
//! * Premium is charged pro rata to coverage size and duration, at an annual
//!   rate in basis points: `premium = coverage * rate_bps * duration / (10_000 *
//!   SECONDS_PER_YEAR)`, rounded up so a non-zero coverage never costs nothing.
//! * Each coverage tracks `remaining` (still payable) and `pending` (claimed but
//!   not yet decided). A new claim may not exceed `remaining - pending`.
//! * Claims must be filed while coverage is in force. A payout is capped by
//!   the reserve, so a claim can be partially paid. The unpaid part stays
//!   payable under the same coverage until it expires.
//! * A new coverage may replace an old one only after the old one has expired
//!   and no claim on it is pending.

#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, token,
    Address, Env,
};

/// Approximate ledger-independent year used for pro-rata premiums.
const SECONDS_PER_YEAR: i128 = 365 * 86_400;
const BPS_DENOMINATOR: i128 = 10_000;
/// Annual premium rates above 100% are rejected.
pub const MAX_PREMIUM_RATE_BPS: u32 = 10_000;

/// Persistent entries are refreshed when they have fewer than this many ledgers
/// left, and extended to `TTL_EXTEND_TO` ledgers.
const TTL_THRESHOLD: u32 = 518_400; // about 30 days
const TTL_EXTEND_TO: u32 = 2_073_600; // about 120 days

#[contracttype]
#[derive(Clone)]
enum DataKey {
    Admin,
    Token,
    PremiumRateBps,
    Reserve,
    NextClaimId,
    Coverage(u64),
    Claim(u64),
}

/// Coverage held for one vault.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Coverage {
    pub vault_id: u64,
    pub owner: Address,
    /// Coverage limit when purchased.
    pub limit: i128,
    /// Amount still payable under this coverage.
    pub remaining: i128,
    /// Amount requested by claims that are not yet decided.
    pub pending: i128,
    pub premium_paid: i128,
    pub starts_at: u64,
    pub expires_at: u64,
}

#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaimStatus {
    Pending,
    Paid,
    PartiallyPaid,
    Rejected,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Claim {
    pub id: u64,
    pub vault_id: u64,
    pub owner: Address,
    pub requested: i128,
    pub paid: i128,
    pub status: ClaimStatus,
    pub filed_at: u64,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum InsuranceError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    InvalidAmount = 3,
    InvalidDuration = 4,
    InvalidRate = 5,
    CoverageExists = 6,
    CoverageNotFound = 7,
    NotCoverageOwner = 8,
    CoverageExpired = 9,
    CoverageExhausted = 10,
    ClaimNotFound = 11,
    ClaimNotPending = 12,
    NotAdmin = 13,
}

#[contract]
pub struct InsurancePool;

#[contractimpl]
impl InsurancePool {
    /// One-time setup. `premium_rate_bps` is the annual premium rate in basis
    /// points, for example 500 means 5% of coverage per year.
    pub fn initialize(env: Env, admin: Address, token: Address, premium_rate_bps: u32) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(&env, InsuranceError::AlreadyInitialized);
        }
        if premium_rate_bps > MAX_PREMIUM_RATE_BPS {
            panic_with_error!(&env, InsuranceError::InvalidRate);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage()
            .instance()
            .set(&DataKey::PremiumRateBps, &premium_rate_bps);
        env.storage().instance().set(&DataKey::Reserve, &0_i128);
        env.storage().instance().set(&DataKey::NextClaimId, &1_u64);
    }

    /// Admin only. Changes the annual premium rate for future purchases.
    pub fn set_premium_rate(
        env: Env,
        admin: Address,
        premium_rate_bps: u32,
    ) -> Result<(), InsuranceError> {
        Self::require_admin(&env, &admin)?;
        if premium_rate_bps > MAX_PREMIUM_RATE_BPS {
            return Err(InsuranceError::InvalidRate);
        }
        env.storage()
            .instance()
            .set(&DataKey::PremiumRateBps, &premium_rate_bps);
        Ok(())
    }

    /// Adds capital to the reserve. Anyone may fund the pool.
    pub fn fund_reserve(env: Env, funder: Address, amount: i128) -> Result<(), InsuranceError> {
        funder.require_auth();
        if amount <= 0 {
            return Err(InsuranceError::InvalidAmount);
        }
        Self::pull_tokens(&env, &funder, amount);
        Self::add_reserve(&env, amount);
        Ok(())
    }

    /// Premium for `coverage_amount` over `duration_secs` at the current rate.
    /// Rounds up so that any non-zero coverage costs at least one unit when the
    /// rate is non-zero.
    pub fn quote_premium(
        env: Env,
        coverage_amount: i128,
        duration_secs: u64,
    ) -> Result<i128, InsuranceError> {
        if coverage_amount <= 0 {
            return Err(InsuranceError::InvalidAmount);
        }
        if duration_secs == 0 {
            return Err(InsuranceError::InvalidDuration);
        }
        Self::premium_for(&env, coverage_amount, duration_secs)
    }

    /// Buys coverage for `vault_id` and returns the premium paid. Replaces an
    /// earlier coverage for the same vault only if that one has expired and has
    /// no pending claim.
    pub fn purchase_coverage(
        env: Env,
        owner: Address,
        vault_id: u64,
        coverage_amount: i128,
        duration_secs: u64,
    ) -> Result<i128, InsuranceError> {
        owner.require_auth();
        if coverage_amount <= 0 {
            return Err(InsuranceError::InvalidAmount);
        }
        if duration_secs == 0 {
            return Err(InsuranceError::InvalidDuration);
        }
        let now = env.ledger().timestamp();
        if let Some(existing) = Self::read_coverage(&env, vault_id) {
            if existing.expires_at > now || existing.pending > 0 {
                return Err(InsuranceError::CoverageExists);
            }
        }

        let premium = Self::premium_for(&env, coverage_amount, duration_secs)?;
        if premium > 0 {
            Self::pull_tokens(&env, &owner, premium);
            Self::add_reserve(&env, premium);
        }

        let coverage = Coverage {
            vault_id,
            owner: owner.clone(),
            limit: coverage_amount,
            remaining: coverage_amount,
            pending: 0,
            premium_paid: premium,
            starts_at: now,
            expires_at: now.saturating_add(duration_secs),
        };
        Self::write_coverage(&env, vault_id, &coverage);

        env.events().publish(
            (symbol_short!("ins_cov"), vault_id),
            (coverage_amount, premium, coverage.expires_at),
        );
        Ok(premium)
    }

    /// Files a claim for a loss on `vault_id`. The requested amount is capped at
    /// the coverage still available (`remaining - pending`). Returns the claim id.
    pub fn file_claim(
        env: Env,
        owner: Address,
        vault_id: u64,
        loss_amount: i128,
    ) -> Result<u64, InsuranceError> {
        owner.require_auth();
        if loss_amount <= 0 {
            return Err(InsuranceError::InvalidAmount);
        }
        let mut coverage =
            Self::read_coverage(&env, vault_id).ok_or(InsuranceError::CoverageNotFound)?;
        if coverage.owner != owner {
            return Err(InsuranceError::NotCoverageOwner);
        }
        let now = env.ledger().timestamp();
        if now > coverage.expires_at {
            return Err(InsuranceError::CoverageExpired);
        }
        let available = coverage.remaining - coverage.pending;
        if available <= 0 {
            return Err(InsuranceError::CoverageExhausted);
        }
        let requested = loss_amount.min(available);

        coverage.pending += requested;
        Self::write_coverage(&env, vault_id, &coverage);

        let id = Self::next_claim_id(&env);
        let claim = Claim {
            id,
            vault_id,
            owner,
            requested,
            paid: 0,
            status: ClaimStatus::Pending,
            filed_at: now,
        };
        Self::write_claim(&env, id, &claim);

        env.events()
            .publish((symbol_short!("ins_clm"), id), (vault_id, requested));
        Ok(id)
    }

    /// Admin only. Decides a pending claim. Approval pays the requested amount
    /// up to the reserve balance. Rejection releases the pending amount.
    /// Returns the amount paid.
    pub fn process_claim(
        env: Env,
        admin: Address,
        claim_id: u64,
        approve: bool,
    ) -> Result<i128, InsuranceError> {
        Self::require_admin(&env, &admin)?;
        let mut claim = Self::read_claim(&env, claim_id).ok_or(InsuranceError::ClaimNotFound)?;
        if claim.status != ClaimStatus::Pending {
            return Err(InsuranceError::ClaimNotPending);
        }
        let mut coverage =
            Self::read_coverage(&env, claim.vault_id).ok_or(InsuranceError::CoverageNotFound)?;
        coverage.pending -= claim.requested;

        if !approve {
            claim.status = ClaimStatus::Rejected;
            Self::write_coverage(&env, claim.vault_id, &coverage);
            Self::write_claim(&env, claim_id, &claim);
            env.events()
                .publish((symbol_short!("ins_rej"), claim_id), claim.vault_id);
            return Ok(0);
        }

        let payout = claim.requested.min(Self::reserve(&env));
        if payout > 0 {
            Self::push_tokens(&env, &claim.owner, payout);
            Self::sub_reserve(&env, payout);
        }
        coverage.remaining -= payout;
        claim.paid = payout;
        claim.status = if payout == claim.requested {
            ClaimStatus::Paid
        } else {
            ClaimStatus::PartiallyPaid
        };

        Self::write_coverage(&env, claim.vault_id, &coverage);
        Self::write_claim(&env, claim_id, &claim);
        env.events()
            .publish((symbol_short!("ins_pay"), claim_id), payout);
        Ok(payout)
    }

    /// Coverage held for `vault_id`, if any.
    pub fn get_coverage(env: Env, vault_id: u64) -> Option<Coverage> {
        Self::read_coverage(&env, vault_id)
    }

    pub fn get_claim(env: Env, claim_id: u64) -> Option<Claim> {
        Self::read_claim(&env, claim_id)
    }

    /// Capital available for payouts. Equals the contract's token balance.
    pub fn get_reserve(env: Env) -> i128 {
        Self::reserve(&env)
    }

    pub fn get_premium_rate(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::PremiumRateBps)
            .unwrap_or(0)
    }

    // ── internals ────────────────────────────────────────────────────────────

    fn require_admin(env: &Env, admin: &Address) -> Result<(), InsuranceError> {
        let stored: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(InsuranceError::NotInitialized)?;
        if stored != *admin {
            return Err(InsuranceError::NotAdmin);
        }
        admin.require_auth();
        Ok(())
    }

    fn premium_for(
        env: &Env,
        coverage_amount: i128,
        duration_secs: u64,
    ) -> Result<i128, InsuranceError> {
        let rate: u32 = env
            .storage()
            .instance()
            .get(&DataKey::PremiumRateBps)
            .ok_or(InsuranceError::NotInitialized)?;
        let numerator = coverage_amount
            .checked_mul(i128::from(rate))
            .and_then(|v| v.checked_mul(i128::from(duration_secs)))
            .ok_or(InsuranceError::InvalidAmount)?;
        let denominator = BPS_DENOMINATOR * SECONDS_PER_YEAR;
        // Round up: (n + d - 1) / d for non-negative n and positive d.
        let rounded = numerator
            .checked_add(denominator - 1)
            .ok_or(InsuranceError::InvalidAmount)?;
        Ok(rounded / denominator)
    }

    fn token_address(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Token)
            .unwrap_or_else(|| panic_with_error!(env, InsuranceError::NotInitialized))
    }

    fn pull_tokens(env: &Env, from: &Address, amount: i128) {
        let client = token::Client::new(env, &Self::token_address(env));
        client.transfer(from, &env.current_contract_address(), &amount);
    }

    fn push_tokens(env: &Env, to: &Address, amount: i128) {
        let client = token::Client::new(env, &Self::token_address(env));
        client.transfer(&env.current_contract_address(), to, &amount);
    }

    fn reserve(env: &Env) -> i128 {
        env.storage().instance().get(&DataKey::Reserve).unwrap_or(0)
    }

    fn add_reserve(env: &Env, amount: i128) {
        let next = Self::reserve(env)
            .checked_add(amount)
            .unwrap_or_else(|| panic_with_error!(env, InsuranceError::InvalidAmount));
        env.storage().instance().set(&DataKey::Reserve, &next);
    }

    fn sub_reserve(env: &Env, amount: i128) {
        env.storage()
            .instance()
            .set(&DataKey::Reserve, &(Self::reserve(env) - amount));
    }

    fn next_claim_id(env: &Env) -> u64 {
        let id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextClaimId)
            .unwrap_or(1);
        env.storage()
            .instance()
            .set(&DataKey::NextClaimId, &(id + 1));
        id
    }

    fn read_coverage(env: &Env, vault_id: u64) -> Option<Coverage> {
        env.storage().persistent().get(&DataKey::Coverage(vault_id))
    }

    fn write_coverage(env: &Env, vault_id: u64, coverage: &Coverage) {
        let key = DataKey::Coverage(vault_id);
        env.storage().persistent().set(&key, coverage);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
    }

    fn read_claim(env: &Env, claim_id: u64) -> Option<Claim> {
        env.storage().persistent().get(&DataKey::Claim(claim_id))
    }

    fn write_claim(env: &Env, claim_id: u64, claim: &Claim) {
        let key = DataKey::Claim(claim_id);
        env.storage().persistent().set(&key, claim);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
    }
}

#[cfg(test)]
mod test;
