//! Tests for Vault Lending Capability (Issue #609)

#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env,
};

use crate::lending::{
    accrued_interest, amount_owed, borrow, create_pool, deposit_liquidity, get_lender_shares,
    get_loan, get_pool, liquidate_loan, repay_loan, set_pool_terms, shares_value,
    withdraw_liquidity, LoanStatus, SECONDS_PER_YEAR,
};
use crate::ContractError;

// ── Test Contract Mock ────────────────────────────────────────────────────────

#[contract]
pub struct MockLendingContract;

#[contractimpl]
impl MockLendingContract {
    pub fn init_test(_env: Env) {}
}

// ── Helpers ──────────────────────────────────────────────────────────────────

const INTEREST_BPS: u32 = 1_000; // 10% APR
const MIN_COLLATERAL_BPS: u32 = 15_000; // 150%

struct Setup {
    env: Env,
    contract_id: Address,
    token: Address,
    admin: Address,
    lender: Address,
    borrower: Address,
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000);

    let contract_id = env.register_contract(None, MockLendingContract);
    let token_admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    let admin = Address::generate(&env);
    let lender = Address::generate(&env);
    let borrower = Address::generate(&env);

    let minter = StellarAssetClient::new(&env, &token);
    minter.mint(&lender, &10_000_000);
    minter.mint(&borrower, &10_000_000);

    env.as_contract(&contract_id, || {
        create_pool(&env, &admin, &token, INTEREST_BPS, MIN_COLLATERAL_BPS);
    });

    Setup {
        env,
        contract_id,
        token,
        admin,
        lender,
        borrower,
    }
}

// ── Pool Management ──────────────────────────────────────────────────────────

#[test]
fn test_create_pool_stores_terms() {
    let s = setup();
    s.env.as_contract(&s.contract_id, || {
        let pool = get_pool(&s.env, &s.token).expect("pool exists");
        assert_eq!(pool.admin, s.admin);
        assert_eq!(pool.interest_bps, INTEREST_BPS);
        assert_eq!(pool.min_collateral_bps, MIN_COLLATERAL_BPS);
        assert_eq!(pool.cash, 0);
        assert_eq!(pool.total_shares, 0);
    });
}

#[test]
#[should_panic]
fn test_create_pool_twice_fails() {
    let s = setup();
    s.env.as_contract(&s.contract_id, || {
        create_pool(&s.env, &s.admin, &s.token, INTEREST_BPS, MIN_COLLATERAL_BPS);
    });
}

#[test]
#[should_panic]
fn test_create_pool_rejects_undercollateralized_terms() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, MockLendingContract);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    let admin = Address::generate(&env);
    env.as_contract(&contract_id, || {
        create_pool(&env, &admin, &token, INTEREST_BPS, 9_999);
    });
}

#[test]
#[should_panic]
fn test_set_pool_terms_requires_pool_admin() {
    let s = setup();
    let impostor = Address::generate(&s.env);
    s.env.as_contract(&s.contract_id, || {
        set_pool_terms(&s.env, &impostor, &s.token, 500, MIN_COLLATERAL_BPS);
    });
}

#[test]
fn test_set_pool_terms_updates_rate() {
    let s = setup();
    s.env.as_contract(&s.contract_id, || {
        set_pool_terms(&s.env, &s.admin, &s.token, 2_000, 20_000);
        let pool = get_pool(&s.env, &s.token).expect("pool exists");
        assert_eq!(pool.interest_bps, 2_000);
        assert_eq!(pool.min_collateral_bps, 20_000);
    });
}

// ── Liquidity ────────────────────────────────────────────────────────────────

#[test]
fn test_first_deposit_mints_shares_one_to_one() {
    let s = setup();
    s.env.as_contract(&s.contract_id, || {
        let shares = deposit_liquidity(&s.env, &s.token, &s.lender, 1_000_000);
        assert_eq!(shares, 1_000_000);
        assert_eq!(get_lender_shares(&s.env, &s.token, &s.lender), 1_000_000);
        assert_eq!(get_pool(&s.env, &s.token).unwrap().cash, 1_000_000);
    });
    assert_eq!(
        TokenClient::new(&s.env, &s.token).balance(&s.contract_id),
        1_000_000
    );
}

#[test]
fn test_full_withdrawal_returns_deposit() {
    let s = setup();
    let token = TokenClient::new(&s.env, &s.token);
    let shares = s.env.as_contract(&s.contract_id, || {
        deposit_liquidity(&s.env, &s.token, &s.lender, 1_000_000)
    });
    let paid = s.env.as_contract(&s.contract_id, || {
        withdraw_liquidity(&s.env, &s.token, &s.lender, shares)
    });
    assert_eq!(paid, 1_000_000);
    s.env.as_contract(&s.contract_id, || {
        assert_eq!(get_lender_shares(&s.env, &s.token, &s.lender), 0);
        assert_eq!(get_pool(&s.env, &s.token).unwrap().total_shares, 0);
    });
    assert_eq!(token.balance(&s.lender), 10_000_000);
}

#[test]
#[should_panic]
fn test_withdraw_more_shares_than_owned_fails() {
    let s = setup();
    s.env.as_contract(&s.contract_id, || {
        deposit_liquidity(&s.env, &s.token, &s.lender, 1_000);
        withdraw_liquidity(&s.env, &s.token, &s.lender, 1_001);
    });
}

#[test]
#[should_panic]
fn test_deposit_zero_fails() {
    let s = setup();
    s.env.as_contract(&s.contract_id, || {
        deposit_liquidity(&s.env, &s.token, &s.lender, 0);
    });
}

// ── Loans ────────────────────────────────────────────────────────────────────

#[test]
fn test_borrow_moves_funds_and_locks_collateral() {
    let s = setup();
    let token = TokenClient::new(&s.env, &s.token);
    s.env.as_contract(&s.contract_id, || {
        deposit_liquidity(&s.env, &s.token, &s.lender, 1_000_000);
        let loan_id = borrow(&s.env, &s.token, &s.borrower, 1_500_000, 1_000_000, 86_400);
        assert_eq!(loan_id, 1);

        let loan = get_loan(&s.env, loan_id).expect("loan exists");
        assert_eq!(loan.status, LoanStatus::Active);
        assert_eq!(loan.principal, 1_000_000);
        assert_eq!(loan.collateral, 1_500_000);
        assert_eq!(loan.due_at, 1_000 + 86_400);

        let pool = get_pool(&s.env, &s.token).unwrap();
        assert_eq!(pool.cash, 0);
        assert_eq!(pool.total_borrowed, 1_000_000);
    });
    // Contract holds only the collateral now; the principal went to the borrower.
    assert_eq!(token.balance(&s.contract_id), 1_500_000);
    assert_eq!(
        token.balance(&s.borrower),
        10_000_000 - 1_500_000 + 1_000_000
    );
}

#[test]
#[should_panic]
fn test_borrow_below_collateral_ratio_fails() {
    let s = setup();
    s.env.as_contract(&s.contract_id, || {
        deposit_liquidity(&s.env, &s.token, &s.lender, 1_000_000);
        // 149% collateral is below the 150% minimum.
        borrow(&s.env, &s.token, &s.borrower, 1_490_000, 1_000_000, 86_400);
    });
}

#[test]
#[should_panic]
fn test_borrow_more_than_pool_cash_fails() {
    let s = setup();
    s.env.as_contract(&s.contract_id, || {
        deposit_liquidity(&s.env, &s.token, &s.lender, 500_000);
        borrow(&s.env, &s.token, &s.borrower, 2_000_000, 1_000_000, 86_400);
    });
}

#[test]
fn test_interest_accrues_linearly_over_time() {
    let s = setup();
    s.env.as_contract(&s.contract_id, || {
        deposit_liquidity(&s.env, &s.token, &s.lender, 1_000_000);
        let loan_id = borrow(
            &s.env,
            &s.token,
            &s.borrower,
            1_500_000,
            1_000_000,
            SECONDS_PER_YEAR,
        );

        // Immediately after origination nothing has accrued.
        assert_eq!(amount_owed(&s.env, loan_id), 1_000_000);

        // Half a year at 10% APR on 1,000,000 accrues 50,000.
        s.env.ledger().set_timestamp(1_000 + SECONDS_PER_YEAR / 2);
        let loan = get_loan(&s.env, loan_id).unwrap();
        assert_eq!(accrued_interest(&s.env, &loan), 50_000);
        assert_eq!(amount_owed(&s.env, loan_id), 1_050_000);
    });
}

#[test]
fn test_repay_returns_collateral_and_pays_interest_to_pool() {
    let s = setup();
    let token = TokenClient::new(&s.env, &s.token);
    s.env.as_contract(&s.contract_id, || {
        deposit_liquidity(&s.env, &s.token, &s.lender, 1_000_000);
    });
    let loan_id = s.env.as_contract(&s.contract_id, || {
        borrow(
            &s.env,
            &s.token,
            &s.borrower,
            1_500_000,
            1_000_000,
            SECONDS_PER_YEAR,
        )
    });
    s.env.ledger().set_timestamp(1_000 + SECONDS_PER_YEAR);
    let owed = s
        .env
        .as_contract(&s.contract_id, || repay_loan(&s.env, loan_id, &s.borrower));
    assert_eq!(owed, 1_100_000);
    s.env.as_contract(&s.contract_id, || {
        let loan = get_loan(&s.env, loan_id).unwrap();
        assert_eq!(loan.status, LoanStatus::Repaid);

        let pool = get_pool(&s.env, &s.token).unwrap();
        assert_eq!(pool.total_borrowed, 0);
        assert_eq!(pool.cash, 1_100_000);
    });
    // Collateral is back with the borrower; the contract holds only pool cash.
    assert_eq!(token.balance(&s.contract_id), 1_100_000);
    // Borrower net: +1,000,000 principal - 1,100,000 repaid = 100,000 less than start.
    assert_eq!(token.balance(&s.borrower), 10_000_000 - 100_000);
}

#[test]
fn test_lender_withdrawal_includes_interest_earned() {
    let s = setup();
    let token = TokenClient::new(&s.env, &s.token);
    let shares = s.env.as_contract(&s.contract_id, || {
        deposit_liquidity(&s.env, &s.token, &s.lender, 1_000_000)
    });
    let loan_id = s.env.as_contract(&s.contract_id, || {
        borrow(
            &s.env,
            &s.token,
            &s.borrower,
            1_500_000,
            1_000_000,
            SECONDS_PER_YEAR,
        )
    });
    s.env.ledger().set_timestamp(1_000 + SECONDS_PER_YEAR);
    s.env.as_contract(&s.contract_id, || {
        repay_loan(&s.env, loan_id, &s.borrower);
    });

    // Sole lender owns all shares, so they receive principal plus interest.
    s.env.as_contract(&s.contract_id, || {
        assert_eq!(shares_value(&s.env, &s.token, shares), 1_100_000);
    });
    let paid = s.env.as_contract(&s.contract_id, || {
        withdraw_liquidity(&s.env, &s.token, &s.lender, shares)
    });
    assert_eq!(paid, 1_100_000);
    assert_eq!(token.balance(&s.lender), 10_000_000 + 100_000);
}

#[test]
#[should_panic]
fn test_withdraw_blocked_while_funds_are_lent_out() {
    let s = setup();
    s.env.as_contract(&s.contract_id, || {
        let shares = deposit_liquidity(&s.env, &s.token, &s.lender, 1_000_000);
        borrow(&s.env, &s.token, &s.borrower, 1_500_000, 1_000_000, 86_400);
        // All cash is lent out, so any withdrawal must fail.
        withdraw_liquidity(&s.env, &s.token, &s.lender, shares);
    });
}

#[test]
#[should_panic]
fn test_repay_by_non_borrower_fails() {
    let s = setup();
    let other = Address::generate(&s.env);
    s.env.as_contract(&s.contract_id, || {
        deposit_liquidity(&s.env, &s.token, &s.lender, 1_000_000);
        let loan_id = borrow(&s.env, &s.token, &s.borrower, 1_500_000, 1_000_000, 86_400);
        repay_loan(&s.env, loan_id, &other);
    });
}

#[test]
#[should_panic]
fn test_repay_twice_fails() {
    let s = setup();
    s.env.as_contract(&s.contract_id, || {
        deposit_liquidity(&s.env, &s.token, &s.lender, 1_000_000);
        let loan_id = borrow(&s.env, &s.token, &s.borrower, 1_500_000, 1_000_000, 86_400);
        repay_loan(&s.env, loan_id, &s.borrower);
        repay_loan(&s.env, loan_id, &s.borrower);
    });
}

// ── Default ──────────────────────────────────────────────────────────────────

#[test]
fn test_liquidate_overdue_loan_seizes_collateral_to_pool() {
    let s = setup();
    let shares = s.env.as_contract(&s.contract_id, || {
        deposit_liquidity(&s.env, &s.token, &s.lender, 1_000_000)
    });
    let loan_id = s.env.as_contract(&s.contract_id, || {
        borrow(&s.env, &s.token, &s.borrower, 1_500_000, 1_000_000, 86_400)
    });

    s.env.ledger().set_timestamp(1_000 + 86_400 + 1);
    let seized = s.env.as_contract(&s.contract_id, || {
        liquidate_loan(&s.env, loan_id, &s.lender)
    });
    assert_eq!(seized, 1_500_000);

    s.env.as_contract(&s.contract_id, || {
        let loan = get_loan(&s.env, loan_id).unwrap();
        assert_eq!(loan.status, LoanStatus::Defaulted);
        let pool = get_pool(&s.env, &s.token).unwrap();
        assert_eq!(pool.total_borrowed, 0);
        assert_eq!(pool.cash, 1_500_000);
    });

    // Lender can now withdraw the seized collateral as part of their share.
    let paid = s.env.as_contract(&s.contract_id, || {
        withdraw_liquidity(&s.env, &s.token, &s.lender, shares)
    });
    assert_eq!(paid, 1_500_000);
}

#[test]
#[should_panic]
fn test_liquidate_before_due_fails() {
    let s = setup();
    s.env.as_contract(&s.contract_id, || {
        deposit_liquidity(&s.env, &s.token, &s.lender, 1_000_000);
        let loan_id = borrow(&s.env, &s.token, &s.borrower, 1_500_000, 1_000_000, 86_400);
        liquidate_loan(&s.env, loan_id, &s.lender);
    });
}

#[test]
#[should_panic]
fn test_repay_after_default_fails() {
    let s = setup();
    s.env.as_contract(&s.contract_id, || {
        deposit_liquidity(&s.env, &s.token, &s.lender, 1_000_000);
        let loan_id = borrow(&s.env, &s.token, &s.borrower, 1_500_000, 1_000_000, 86_400);
        s.env.ledger().set_timestamp(1_000 + 86_400 + 1);
        liquidate_loan(&s.env, loan_id, &s.lender);
        repay_loan(&s.env, loan_id, &s.borrower);
    });
}

#[test]
fn test_loan_ids_increment() {
    let s = setup();
    s.env.as_contract(&s.contract_id, || {
        deposit_liquidity(&s.env, &s.token, &s.lender, 3_000_000);
    });
    let first = s.env.as_contract(&s.contract_id, || {
        borrow(&s.env, &s.token, &s.borrower, 1_500_000, 1_000_000, 86_400)
    });
    let second = s.env.as_contract(&s.contract_id, || {
        borrow(&s.env, &s.token, &s.borrower, 1_500_000, 1_000_000, 86_400)
    });
    assert_eq!(first, 1);
    assert_eq!(second, 2);
}

#[test]
#[should_panic]
fn test_borrow_with_zero_duration_fails() {
    let s = setup();
    s.env.as_contract(&s.contract_id, || {
        deposit_liquidity(&s.env, &s.token, &s.lender, 1_000_000);
        borrow(&s.env, &s.token, &s.borrower, 1_500_000, 1_000_000, 0);
    });
}

#[test]
fn test_error_codes_are_stable() {
    // Pool and loan errors are appended after the existing range (Issue #609).
    assert_eq!(ContractError::PoolAlreadyExists as u32, 140);
    assert_eq!(ContractError::LoanNotDue as u32, 146);
    assert_eq!(ContractError::InsufficientShares as u32, 148);
}
