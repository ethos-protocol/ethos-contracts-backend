#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{self, StellarAssetClient},
    Address, Env,
};

const RATE_BPS: u32 = 500; // 5% of coverage per year
const YEAR: u64 = 365 * 86_400;
const START: u64 = 1_000;

struct Ctx {
    env: Env,
    admin: Address,
    token: Address,
    id: Address,
}

impl Ctx {
    fn new(rate_bps: u32) -> Self {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|l| l.timestamp = START);

        let token_admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let admin = Address::generate(&env);
        let id = env.register_contract(None, InsurancePool);
        InsurancePoolClient::new(&env, &id).initialize(&admin, &token, &rate_bps);

        Self {
            env,
            admin,
            token,
            id,
        }
    }

    fn client(&self) -> InsurancePoolClient<'_> {
        InsurancePoolClient::new(&self.env, &self.id)
    }

    fn token(&self) -> token::Client<'_> {
        token::Client::new(&self.env, &self.token)
    }

    fn mint(&self, to: &Address, amount: i128) {
        StellarAssetClient::new(&self.env, &self.token).mint(to, &amount);
    }

    fn set_time(&self, timestamp: u64) {
        self.env.ledger().with_mut(|l| l.timestamp = timestamp);
    }

    /// Funds the reserve directly from a fresh funder account.
    fn fund(&self, amount: i128) {
        let funder = Address::generate(&self.env);
        self.mint(&funder, amount);
        self.client().fund_reserve(&funder, &amount);
    }

    /// Buys coverage for a fresh owner who is minted enough to pay the premium.
    fn buy(&self, vault_id: u64, coverage: i128, duration: u64) -> Address {
        let owner = Address::generate(&self.env);
        self.mint(&owner, coverage);
        self.client()
            .purchase_coverage(&owner, &vault_id, &coverage, &duration);
        owner
    }
}

// ── premium ──────────────────────────────────────────────────────────────────

#[test]
fn quote_premium_is_pro_rata_to_amount_and_duration() {
    let ctx = Ctx::new(RATE_BPS);
    let client = ctx.client();

    assert_eq!(client.quote_premium(&1_000_000, &YEAR), 50_000);
    assert_eq!(client.quote_premium(&1_000_000, &(YEAR / 2)), 25_000);
    assert_eq!(client.quote_premium(&2_000_000, &YEAR), 100_000);
}

#[test]
fn quote_premium_rounds_up_so_small_coverage_is_never_free() {
    let ctx = Ctx::new(RATE_BPS);
    assert_eq!(ctx.client().quote_premium(&1, &1), 1);
}

#[test]
fn zero_rate_quotes_zero_premium() {
    let ctx = Ctx::new(0);
    assert_eq!(ctx.client().quote_premium(&1_000_000, &YEAR), 0);
}

#[test]
fn quote_premium_rejects_zero_amount_and_duration() {
    let ctx = Ctx::new(RATE_BPS);
    let client = ctx.client();

    let err = client.try_quote_premium(&0, &YEAR).unwrap_err().unwrap();
    assert_eq!(err, InsuranceError::InvalidAmount);

    let err = client.try_quote_premium(&1_000, &0).unwrap_err().unwrap();
    assert_eq!(err, InsuranceError::InvalidDuration);
}

// ── admin and setup ──────────────────────────────────────────────────────────

#[test]
fn initialize_twice_is_rejected() {
    let ctx = Ctx::new(RATE_BPS);
    let err = ctx
        .client()
        .try_initialize(&ctx.admin, &ctx.token, &RATE_BPS)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, InsuranceError::AlreadyInitialized.into());
}

#[test]
fn initialize_rejects_rate_above_maximum() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let id = env.register_contract(None, InsurancePool);
    let client = InsurancePoolClient::new(&env, &id);

    let err = client
        .try_initialize(&admin, &token, &(MAX_PREMIUM_RATE_BPS + 1))
        .unwrap_err()
        .unwrap();
    assert_eq!(err, InsuranceError::InvalidRate.into());
}

#[test]
fn only_admin_can_change_premium_rate() {
    let ctx = Ctx::new(RATE_BPS);
    let stranger = Address::generate(&ctx.env);

    let err = ctx
        .client()
        .try_set_premium_rate(&stranger, &100)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, InsuranceError::NotAdmin);

    ctx.client().set_premium_rate(&ctx.admin, &100);
    assert_eq!(ctx.client().get_premium_rate(), 100);
    assert_eq!(ctx.client().quote_premium(&1_000_000, &YEAR), 10_000);
}

#[test]
fn set_premium_rate_rejects_rate_above_maximum() {
    let ctx = Ctx::new(RATE_BPS);
    let err = ctx
        .client()
        .try_set_premium_rate(&ctx.admin, &(MAX_PREMIUM_RATE_BPS + 1))
        .unwrap_err()
        .unwrap();
    assert_eq!(err, InsuranceError::InvalidRate);
    assert_eq!(ctx.client().get_premium_rate(), RATE_BPS);
}

// ── reserve ──────────────────────────────────────────────────────────────────

#[test]
fn funding_increases_reserve_and_token_balance_together() {
    let ctx = Ctx::new(RATE_BPS);
    ctx.fund(400);
    ctx.fund(100);

    assert_eq!(ctx.client().get_reserve(), 500);
    assert_eq!(ctx.token().balance(&ctx.id), 500);
}

#[test]
fn funding_rejects_non_positive_amount() {
    let ctx = Ctx::new(RATE_BPS);
    let funder = Address::generate(&ctx.env);
    let err = ctx
        .client()
        .try_fund_reserve(&funder, &0)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, InsuranceError::InvalidAmount);
}

// ── coverage purchase ────────────────────────────────────────────────────────

#[test]
fn purchase_charges_premium_into_reserve_and_records_coverage() {
    let ctx = Ctx::new(RATE_BPS);
    let owner = Address::generate(&ctx.env);
    ctx.mint(&owner, 100_000);

    let premium = ctx
        .client()
        .purchase_coverage(&owner, &1, &1_000_000, &YEAR);

    assert_eq!(premium, 50_000);
    assert_eq!(ctx.client().get_reserve(), 50_000);
    assert_eq!(ctx.token().balance(&ctx.id), 50_000);
    assert_eq!(ctx.token().balance(&owner), 50_000);

    let coverage = ctx.client().get_coverage(&1).unwrap();
    assert_eq!(coverage.owner, owner);
    assert_eq!(coverage.limit, 1_000_000);
    assert_eq!(coverage.remaining, 1_000_000);
    assert_eq!(coverage.pending, 0);
    assert_eq!(coverage.premium_paid, 50_000);
    assert_eq!(coverage.starts_at, START);
    assert_eq!(coverage.expires_at, START + YEAR);
}

#[test]
fn zero_rate_purchase_takes_no_premium() {
    let ctx = Ctx::new(0);
    let owner = Address::generate(&ctx.env);

    assert_eq!(ctx.client().purchase_coverage(&owner, &1, &1_000, &YEAR), 0);
    assert_eq!(ctx.client().get_reserve(), 0);
    assert_eq!(ctx.token().balance(&owner), 0);
}

#[test]
fn active_coverage_cannot_be_replaced() {
    let ctx = Ctx::new(RATE_BPS);
    let owner = ctx.buy(1, 1_000, YEAR);

    let err = ctx
        .client()
        .try_purchase_coverage(&owner, &1, &1_000, &YEAR)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, InsuranceError::CoverageExists);
}

#[test]
fn expired_coverage_without_pending_claims_can_be_replaced() {
    let ctx = Ctx::new(RATE_BPS);
    let owner = ctx.buy(1, 1_000, 100);

    ctx.set_time(START + 101);
    ctx.mint(&owner, 1_000);
    ctx.client().purchase_coverage(&owner, &1, &2_000, &YEAR);

    let coverage = ctx.client().get_coverage(&1).unwrap();
    assert_eq!(coverage.limit, 2_000);
    assert_eq!(coverage.starts_at, START + 101);
}

#[test]
fn pending_claim_blocks_replacement_even_after_expiry() {
    let ctx = Ctx::new(RATE_BPS);
    let owner = ctx.buy(1, 1_000, 100);
    ctx.client().file_claim(&owner, &1, &500);

    ctx.set_time(START + 101);
    ctx.mint(&owner, 1_000);
    let err = ctx
        .client()
        .try_purchase_coverage(&owner, &1, &2_000, &YEAR)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, InsuranceError::CoverageExists);
}

// ── claims ───────────────────────────────────────────────────────────────────

#[test]
fn claim_requires_coverage_owner() {
    let ctx = Ctx::new(RATE_BPS);
    ctx.buy(1, 1_000, YEAR);
    let stranger = Address::generate(&ctx.env);

    let err = ctx
        .client()
        .try_file_claim(&stranger, &1, &100)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, InsuranceError::NotCoverageOwner);
}

#[test]
fn claim_on_vault_without_coverage_is_rejected() {
    let ctx = Ctx::new(RATE_BPS);
    let owner = Address::generate(&ctx.env);

    let err = ctx
        .client()
        .try_file_claim(&owner, &9, &100)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, InsuranceError::CoverageNotFound);
}

#[test]
fn claim_after_expiry_is_rejected() {
    let ctx = Ctx::new(RATE_BPS);
    let owner = ctx.buy(1, 1_000, 100);

    ctx.set_time(START + 101);
    let err = ctx
        .client()
        .try_file_claim(&owner, &1, &100)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, InsuranceError::CoverageExpired);
}

#[test]
fn claim_is_capped_at_available_coverage() {
    let ctx = Ctx::new(RATE_BPS);
    let owner = ctx.buy(1, 1_000, YEAR);

    let first = ctx.client().file_claim(&owner, &1, &1_500);
    assert_eq!(first, 1);
    assert_eq!(ctx.client().get_claim(&1).unwrap().requested, 1_000);
    assert_eq!(ctx.client().get_coverage(&1).unwrap().pending, 1_000);

    let err = ctx
        .client()
        .try_file_claim(&owner, &1, &1)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, InsuranceError::CoverageExhausted);
}

#[test]
fn approved_claim_pays_owner_and_reduces_remaining() {
    let ctx = Ctx::new(RATE_BPS);
    ctx.fund(10_000);
    let owner = ctx.buy(1, 1_000, YEAR);
    let balance_before = ctx.token().balance(&owner);

    let id = ctx.client().file_claim(&owner, &1, &600);
    let paid = ctx.client().process_claim(&ctx.admin, &id, &true);

    assert_eq!(paid, 600);
    assert_eq!(ctx.token().balance(&owner), balance_before + 600);
    let claim = ctx.client().get_claim(&id).unwrap();
    assert_eq!(claim.status, ClaimStatus::Paid);
    assert_eq!(claim.paid, 600);
    let coverage = ctx.client().get_coverage(&1).unwrap();
    assert_eq!(coverage.remaining, 400);
    assert_eq!(coverage.pending, 0);
    assert_eq!(ctx.client().get_reserve(), ctx.token().balance(&ctx.id));
}

#[test]
fn rejected_claim_releases_pending_amount() {
    let ctx = Ctx::new(RATE_BPS);
    let owner = ctx.buy(1, 1_000, YEAR);

    let id = ctx.client().file_claim(&owner, &1, &600);
    let paid = ctx.client().process_claim(&ctx.admin, &id, &false);

    assert_eq!(paid, 0);
    assert_eq!(
        ctx.client().get_claim(&id).unwrap().status,
        ClaimStatus::Rejected
    );
    let coverage = ctx.client().get_coverage(&1).unwrap();
    assert_eq!(coverage.remaining, 1_000);
    assert_eq!(coverage.pending, 0);

    // The full limit is claimable again after a rejection.
    ctx.client().file_claim(&owner, &1, &1_000);
}

#[test]
fn payout_is_capped_by_reserve_and_stays_payable() {
    let ctx = Ctx::new(RATE_BPS);
    // Premium on 1_000_000 for a year at 5% is 50_000, which is the whole reserve.
    let owner = ctx.buy(1, 1_000_000, YEAR);
    assert_eq!(ctx.client().get_reserve(), 50_000);

    let id = ctx.client().file_claim(&owner, &1, &1_000_000);
    let paid = ctx.client().process_claim(&ctx.admin, &id, &true);

    assert_eq!(paid, 50_000);
    let claim = ctx.client().get_claim(&id).unwrap();
    assert_eq!(claim.status, ClaimStatus::PartiallyPaid);
    assert_eq!(claim.paid, 50_000);
    let coverage = ctx.client().get_coverage(&1).unwrap();
    assert_eq!(coverage.remaining, 950_000);
    assert_eq!(coverage.pending, 0);
    assert_eq!(ctx.client().get_reserve(), 0);

    // The unpaid remainder is still claimable under the same coverage.
    ctx.fund(950_000);
    let second = ctx.client().file_claim(&owner, &1, &950_000);
    assert_eq!(
        ctx.client().process_claim(&ctx.admin, &second, &true),
        950_000
    );
    assert_eq!(ctx.client().get_coverage(&1).unwrap().remaining, 0);
}

#[test]
fn only_admin_can_process_claims() {
    let ctx = Ctx::new(RATE_BPS);
    let owner = ctx.buy(1, 1_000, YEAR);
    let id = ctx.client().file_claim(&owner, &1, &100);
    let stranger = Address::generate(&ctx.env);

    let err = ctx
        .client()
        .try_process_claim(&stranger, &id, &true)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, InsuranceError::NotAdmin);
}

#[test]
fn processing_unknown_or_decided_claim_is_rejected() {
    let ctx = Ctx::new(RATE_BPS);
    let owner = ctx.buy(1, 1_000, YEAR);
    let id = ctx.client().file_claim(&owner, &1, &100);
    ctx.client().process_claim(&ctx.admin, &id, &true);

    let err = ctx
        .client()
        .try_process_claim(&ctx.admin, &id, &true)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, InsuranceError::ClaimNotPending);

    let err = ctx
        .client()
        .try_process_claim(&ctx.admin, &99, &true)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, InsuranceError::ClaimNotFound);
}
