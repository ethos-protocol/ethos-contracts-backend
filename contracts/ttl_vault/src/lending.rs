//! Issue #609 — Implement Vault Lending Capability
//!
//! Vault funds sit idle. Lending lets liquidity providers earn interest on
//! idle tokens while borrowers take over-collateralized loans against assets
//! they already hold.
//!
//! Provides:
//! - Per-token lending pools with an admin-controlled annual interest rate and
//!   minimum collateral ratio
//! - Share-based liquidity deposits and withdrawals (pro-rata claim on pool
//!   assets, so accrued interest flows to lenders)
//! - Loan origination against over-collateralized borrower deposits
//! - Simple interest accrual, realised when the loan is repaid
//! - Loan repayment (principal + interest, collateral returned)
//! - Default handling: once a loan is past due its collateral is seized into
//!   the pool
//!
//! Collateral is transferred into the lending contract at origination rather
//! than read from vault balances, so the lending module can enforce the
//! collateral lock on its own. Integrating vault balances directly would
//! require changing the vault withdrawal and trigger-release paths.

use soroban_sdk::{contracttype, panic_with_error, token, Address, Env};

use crate::ContractError;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Seconds in a (365-day) year, used for annual interest accrual.
pub const SECONDS_PER_YEAR: u64 = 31_536_000;

/// Basis-point denominator (100% = 10,000 bps).
pub const LENDING_BPS_DENOMINATOR: i128 = 10_000;

/// Maximum annual interest rate a pool may be configured with (100% APR).
pub const MAX_INTEREST_BPS: u32 = 10_000;

/// Minimum collateral ratio a pool may require (100% of the loan).
pub const MIN_COLLATERAL_BPS_FLOOR: u32 = 10_000;

// ── Event Topics ─────────────────────────────────────────────────────────────

pub const LEND_POOL_CREATED_TOPIC: soroban_sdk::Symbol = soroban_sdk::symbol_short!("lnd_pool");
pub const LEND_DEPOSIT_TOPIC: soroban_sdk::Symbol = soroban_sdk::symbol_short!("lnd_dep");
pub const LEND_WITHDRAW_TOPIC: soroban_sdk::Symbol = soroban_sdk::symbol_short!("lnd_wd");
pub const LEND_LOAN_OPENED_TOPIC: soroban_sdk::Symbol = soroban_sdk::symbol_short!("lnd_open");
pub const LEND_LOAN_REPAID_TOPIC: soroban_sdk::Symbol = soroban_sdk::symbol_short!("lnd_rep");
pub const LEND_LOAN_DEFAULTED_TOPIC: soroban_sdk::Symbol = soroban_sdk::symbol_short!("lnd_def");

// ── Storage Keys ─────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
pub enum LendingKey {
    /// Pool state per token: token -> LendingPool
    Pool(Address),
    /// Lender share balance per pool: (token, lender) -> shares
    Position(Address, Address),
    /// Loan record: loan_id -> Loan
    Loan(u64),
    /// Monotonic loan id counter
    LoanCounter,
}

// ── Types ─────────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct LendingPool {
    /// Address allowed to change the pool's rate and collateral requirements
    pub admin: Address,
    /// Token the pool lends
    pub token: Address,
    /// Tokens held by the pool that are not currently lent out
    pub cash: i128,
    /// Outstanding principal across all active loans
    pub total_borrowed: i128,
    /// Total outstanding lender shares
    pub total_shares: i128,
    /// Annual interest rate in basis points, snapshotted into each new loan
    pub interest_bps: u32,
    /// Minimum collateral as basis points of the loan principal
    pub min_collateral_bps: u32,
}

#[contracttype]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoanStatus {
    Active,
    Repaid,
    Defaulted,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Loan {
    pub id: u64,
    pub borrower: Address,
    pub token: Address,
    pub principal: i128,
    pub collateral: i128,
    /// Annual interest rate in basis points, fixed at origination
    pub interest_bps: u32,
    pub started_at: u64,
    pub due_at: u64,
    pub status: LoanStatus,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn load_pool(env: &Env, token: &Address) -> LendingPool {
    env.storage()
        .persistent()
        .get(&LendingKey::Pool(token.clone()))
        .unwrap_or_else(|| panic_with_error!(env, ContractError::PoolNotFound))
}

fn save_pool(env: &Env, pool: &LendingPool) {
    let key = LendingKey::Pool(pool.token.clone());
    env.storage().persistent().set(&key, pool);
    env.storage().persistent().extend_ttl(
        &key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );
}

fn load_loan(env: &Env, loan_id: u64) -> Loan {
    env.storage()
        .persistent()
        .get(&LendingKey::Loan(loan_id))
        .unwrap_or_else(|| panic_with_error!(env, ContractError::LoanNotFound))
}

fn save_loan(env: &Env, loan: &Loan) {
    let key = LendingKey::Loan(loan.id);
    env.storage().persistent().set(&key, loan);
    env.storage().persistent().extend_ttl(
        &key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );
}

fn position_of(env: &Env, token: &Address, lender: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&LendingKey::Position(token.clone(), lender.clone()))
        .unwrap_or(0)
}

fn save_position(env: &Env, token: &Address, lender: &Address, shares: i128) {
    let key = LendingKey::Position(token.clone(), lender.clone());
    if shares == 0 {
        env.storage().persistent().remove(&key);
    } else {
        env.storage().persistent().set(&key, &shares);
        env.storage().persistent().extend_ttl(
            &key,
            crate::VAULT_TTL_THRESHOLD,
            crate::VAULT_TTL_LEDGERS,
        );
    }
}

fn checked(env: &Env, value: Option<i128>) -> i128 {
    value.unwrap_or_else(|| panic_with_error!(env, ContractError::BalanceOverflow))
}

/// Total pool assets: idle cash plus principal currently lent out.
fn pool_assets(env: &Env, pool: &LendingPool) -> i128 {
    checked(env, pool.cash.checked_add(pool.total_borrowed))
}

// ── Pool Management ──────────────────────────────────────────────────────────

/// Creates a lending pool for `token`. The pool's admin must authorize.
pub fn create_pool(
    env: &Env,
    admin: &Address,
    token: &Address,
    interest_bps: u32,
    min_collateral_bps: u32,
) {
    admin.require_auth();
    validate_config(env, interest_bps, min_collateral_bps);

    let key = LendingKey::Pool(token.clone());
    if env.storage().persistent().has(&key) {
        panic_with_error!(env, ContractError::PoolAlreadyExists);
    }

    let pool = LendingPool {
        admin: admin.clone(),
        token: token.clone(),
        cash: 0,
        total_borrowed: 0,
        total_shares: 0,
        interest_bps,
        min_collateral_bps,
    };
    save_pool(env, &pool);
    env.events().publish(
        (LEND_POOL_CREATED_TOPIC, token.clone()),
        (interest_bps, min_collateral_bps),
    );
}

/// Updates a pool's interest rate and minimum collateral ratio. Existing loans
/// keep the rate they were originated at.
pub fn set_pool_terms(
    env: &Env,
    admin: &Address,
    token: &Address,
    interest_bps: u32,
    min_collateral_bps: u32,
) {
    admin.require_auth();
    validate_config(env, interest_bps, min_collateral_bps);

    let mut pool = load_pool(env, token);
    if pool.admin != *admin {
        panic_with_error!(env, ContractError::NotAdmin);
    }
    pool.interest_bps = interest_bps;
    pool.min_collateral_bps = min_collateral_bps;
    save_pool(env, &pool);
}

fn validate_config(env: &Env, interest_bps: u32, min_collateral_bps: u32) {
    if interest_bps > MAX_INTEREST_BPS || min_collateral_bps < MIN_COLLATERAL_BPS_FLOOR {
        panic_with_error!(env, ContractError::InvalidLendingConfig);
    }
}

pub fn get_pool(env: &Env, token: &Address) -> Option<LendingPool> {
    env.storage()
        .persistent()
        .get(&LendingKey::Pool(token.clone()))
}

/// Shares currently held by `lender` in the pool for `token`.
pub fn get_lender_shares(env: &Env, token: &Address, lender: &Address) -> i128 {
    position_of(env, token, lender)
}

/// Token amount that `shares` are currently worth in the pool.
pub fn shares_value(env: &Env, token: &Address, shares: i128) -> i128 {
    let pool = load_pool(env, token);
    if pool.total_shares == 0 {
        return 0;
    }
    let assets = pool_assets(env, &pool);
    checked(env, shares.checked_mul(assets)) / pool.total_shares
}

// ── Liquidity ────────────────────────────────────────────────────────────────

/// Deposits `amount` of the pool token as liquidity. Returns shares minted.
pub fn deposit_liquidity(env: &Env, token: &Address, lender: &Address, amount: i128) -> i128 {
    lender.require_auth();
    if amount <= 0 {
        panic_with_error!(env, ContractError::InvalidAmount);
    }

    let mut pool = load_pool(env, token);
    let shares = if pool.total_shares == 0 {
        amount
    } else {
        let assets = pool_assets(env, &pool);
        if assets == 0 {
            panic_with_error!(env, ContractError::InvalidAmount);
        }
        checked(env, amount.checked_mul(pool.total_shares)) / assets
    };
    if shares <= 0 {
        panic_with_error!(env, ContractError::InvalidAmount);
    }

    token::Client::new(env, token).transfer(lender, &env.current_contract_address(), &amount);

    pool.cash = checked(env, pool.cash.checked_add(amount));
    pool.total_shares = checked(env, pool.total_shares.checked_add(shares));
    save_pool(env, &pool);

    let held = checked(env, position_of(env, token, lender).checked_add(shares));
    save_position(env, token, lender, held);

    env.events().publish(
        (LEND_DEPOSIT_TOPIC, token.clone()),
        (lender.clone(), amount),
    );
    shares
}

/// Burns `shares` and pays the lender their pro-rata claim on pool assets.
/// Only idle cash can be withdrawn; principal currently lent out is not.
/// Returns the amount paid out.
pub fn withdraw_liquidity(env: &Env, token: &Address, lender: &Address, shares: i128) -> i128 {
    lender.require_auth();
    if shares <= 0 {
        panic_with_error!(env, ContractError::InvalidAmount);
    }

    let held = position_of(env, token, lender);
    if shares > held {
        panic_with_error!(env, ContractError::InsufficientShares);
    }

    let mut pool = load_pool(env, token);
    let assets = pool_assets(env, &pool);
    let amount = checked(env, shares.checked_mul(assets)) / pool.total_shares;
    if amount > pool.cash {
        panic_with_error!(env, ContractError::InsufficientPoolLiquidity);
    }

    pool.cash -= amount;
    pool.total_shares -= shares;
    save_pool(env, &pool);
    save_position(env, token, lender, held - shares);

    token::Client::new(env, token).transfer(&env.current_contract_address(), lender, &amount);

    env.events().publish(
        (LEND_WITHDRAW_TOPIC, token.clone()),
        (lender.clone(), amount),
    );
    amount
}

// ── Loans ────────────────────────────────────────────────────────────────────

/// Opens a loan of `amount` from the pool, secured by `collateral` posted by
/// the borrower. Returns the new loan id.
pub fn borrow(
    env: &Env,
    token: &Address,
    borrower: &Address,
    collateral: i128,
    amount: i128,
    duration_secs: u64,
) -> u64 {
    borrower.require_auth();
    if amount <= 0 || collateral <= 0 {
        panic_with_error!(env, ContractError::InvalidAmount);
    }
    if duration_secs == 0 {
        panic_with_error!(env, ContractError::InvalidLendingConfig);
    }

    let mut pool = load_pool(env, token);
    if amount > pool.cash {
        panic_with_error!(env, ContractError::InsufficientPoolLiquidity);
    }

    // collateral / amount >= min_collateral_bps / 10_000, in integer form.
    let collateral_scaled = checked(env, collateral.checked_mul(LENDING_BPS_DENOMINATOR));
    let required_scaled = checked(env, amount.checked_mul(i128::from(pool.min_collateral_bps)));
    if collateral_scaled < required_scaled {
        panic_with_error!(env, ContractError::InsufficientCollateral);
    }

    let token_client = token::Client::new(env, token);
    let contract = env.current_contract_address();
    token_client.transfer(borrower, &contract, &collateral);
    token_client.transfer(&contract, borrower, &amount);

    pool.cash -= amount;
    pool.total_borrowed = checked(env, pool.total_borrowed.checked_add(amount));
    save_pool(env, &pool);

    let loan_id = env
        .storage()
        .persistent()
        .get::<_, u64>(&LendingKey::LoanCounter)
        .unwrap_or(0)
        + 1;
    env.storage()
        .persistent()
        .set(&LendingKey::LoanCounter, &loan_id);

    let now = env.ledger().timestamp();
    let loan = Loan {
        id: loan_id,
        borrower: borrower.clone(),
        token: token.clone(),
        principal: amount,
        collateral,
        interest_bps: pool.interest_bps,
        started_at: now,
        due_at: now.saturating_add(duration_secs),
        status: LoanStatus::Active,
    };
    save_loan(env, &loan);

    env.events().publish(
        (LEND_LOAN_OPENED_TOPIC, loan_id),
        (borrower.clone(), amount, collateral),
    );
    loan_id
}

/// Simple interest accrued on a loan up to the current ledger timestamp:
/// `principal * interest_bps * elapsed / (10_000 * SECONDS_PER_YEAR)`.
pub fn accrued_interest(env: &Env, loan: &Loan) -> i128 {
    let elapsed = env.ledger().timestamp().saturating_sub(loan.started_at);
    let numerator = checked(
        env,
        loan.principal
            .checked_mul(i128::from(loan.interest_bps))
            .and_then(|v| v.checked_mul(i128::from(elapsed))),
    );
    numerator / (LENDING_BPS_DENOMINATOR * i128::from(SECONDS_PER_YEAR))
}

/// Total amount owed on an active loan (principal plus accrued interest).
pub fn amount_owed(env: &Env, loan_id: u64) -> i128 {
    let loan = load_loan(env, loan_id);
    checked(
        env,
        loan.principal.checked_add(accrued_interest(env, &loan)),
    )
}

/// Repays an active loan in full. Interest accrues to the pool, and the
/// collateral is returned to the borrower. Returns the amount repaid.
pub fn repay_loan(env: &Env, loan_id: u64, borrower: &Address) -> i128 {
    borrower.require_auth();
    let mut loan = load_loan(env, loan_id);
    if loan.borrower != *borrower {
        panic_with_error!(env, ContractError::NotOwner);
    }
    if loan.status != LoanStatus::Active {
        panic_with_error!(env, ContractError::LoanNotActive);
    }

    let interest = accrued_interest(env, &loan);
    let owed = checked(env, loan.principal.checked_add(interest));

    let token_client = token::Client::new(env, &loan.token);
    let contract = env.current_contract_address();
    token_client.transfer(borrower, &contract, &owed);
    token_client.transfer(&contract, borrower, &loan.collateral);

    let mut pool = load_pool(env, &loan.token);
    pool.cash = checked(env, pool.cash.checked_add(owed));
    pool.total_borrowed -= loan.principal;
    save_pool(env, &pool);

    loan.status = LoanStatus::Repaid;
    save_loan(env, &loan);

    env.events().publish(
        (LEND_LOAN_REPAID_TOPIC, loan_id),
        (borrower.clone(), owed, interest),
    );
    owed
}

/// Marks a loan past its due date as defaulted and moves its collateral into
/// the pool. Anyone may call this once the loan is due. Returns the collateral
/// seized.
pub fn liquidate_loan(env: &Env, loan_id: u64, caller: &Address) -> i128 {
    caller.require_auth();
    let mut loan = load_loan(env, loan_id);
    if loan.status != LoanStatus::Active {
        panic_with_error!(env, ContractError::LoanNotActive);
    }
    if env.ledger().timestamp() <= loan.due_at {
        panic_with_error!(env, ContractError::LoanNotDue);
    }

    let mut pool = load_pool(env, &loan.token);
    pool.total_borrowed -= loan.principal;
    pool.cash = checked(env, pool.cash.checked_add(loan.collateral));
    save_pool(env, &pool);

    loan.status = LoanStatus::Defaulted;
    save_loan(env, &loan);

    env.events().publish(
        (LEND_LOAN_DEFAULTED_TOPIC, loan_id),
        (loan.borrower.clone(), loan.collateral),
    );
    loan.collateral
}

pub fn get_loan(env: &Env, loan_id: u64) -> Option<Loan> {
    env.storage().persistent().get(&LendingKey::Loan(loan_id))
}
