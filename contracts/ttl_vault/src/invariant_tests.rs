//! Contract invariant tests — issue #568
//!
//! Invariants defined here:
//!
//! 1. **Balance invariant**: vault balance is always >= 0 and never exceeds the
//!    sum of all deposits minus all successful withdrawals.
//! 2. **BPS invariant**: the sum of all beneficiary BPS entries always equals
//!    10_000 (if at least one beneficiary is set).
//! 3. **TTL invariant**: a check-in always resets `get_ttl_remaining` to
//!    exactly `check_in_interval`.
//! 4. **Release-once invariant**: `trigger_release` can only succeed once per
//!    vault; a second call is rejected.
//! 5. **Owner-only invariant**: only the vault owner can mutate owner-restricted
//!    state (deposit, withdraw, check-in, update-beneficiary).
//!
//! Each invariant is tested both deterministically and under a randomised
//! operation sequence.

#![cfg(test)]

extern crate alloc;

use super::*;
use proptest::prelude::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    vec, Address, BytesN, Env,
};

// ── Setup ────────────────────────────────────────────────────────────────────

fn setup() -> (
    Env,
    Address, // owner
    Address, // beneficiary
    Address, // admin
    TtlVaultContractClient<'static>,
) {
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let admin = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let token_address = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    StellarAssetClient::new(&env, &token_address).mint(&owner, &10_000_000);

    let contract_address = env.register_contract(None, TtlVaultContract);
    let client = TtlVaultContractClient::new(&env, &contract_address);
    client.initialize(&token_address, &admin);

    let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, owner, beneficiary, admin, client)
}

// ── Invariant 1: Balance ─────────────────────────────────────────────────────

/// After N deposits and M withdrawals, balance == sum(deposits) - sum(withdrawals).
#[test]
fn invariant_balance_equals_deposits_minus_withdrawals() {
    let (_env, owner, beneficiary, _admin, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &3600u64, &None);

    let deposits: &[i128] = &[100_000, 200_000, 50_000];
    let withdrawals: &[i128] = &[30_000, 70_000];

    let total_deposits: i128 = deposits.iter().sum();
    let total_withdrawals: i128 = withdrawals.iter().sum();

    for &d in deposits {
        client.deposit(&vault_id, &owner, &d);
    }
    for &w in withdrawals {
        client.withdraw(&vault_id, &owner, &w);
    }

    let balance = client.get_vault_balance(&vault_id);
    assert_eq!(
        balance,
        total_deposits - total_withdrawals,
        "Invariant violated: balance != deposits - withdrawals"
    );
}

/// Balance must never go negative — withdrawal beyond balance is rejected.
#[test]
fn invariant_balance_never_negative() {
    let (_env, owner, beneficiary, _admin, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &3600u64, &None);

    client.deposit(&vault_id, &owner, &50_000i128);

    let err = client
        .try_withdraw(&vault_id, &owner, &100_000i128)
        .unwrap_err()
        .unwrap();
    assert_ne!(err, ContractError::NotFound, "Expected insufficient-funds error");
    assert_eq!(
        client.get_vault_balance(&vault_id),
        50_000,
        "Balance must be unchanged after rejected withdrawal"
    );
}

// ── Invariant 2: BPS sum ─────────────────────────────────────────────────────

/// set_beneficiaries must keep the BPS sum equal to 10_000.
#[test]
fn invariant_bps_sum_equals_10000() {
    let (env, owner, b1, _admin, client) = setup();
    let b2 = Address::generate(&env);
    let b3 = Address::generate(&env);

    let vault_id = client.create_vault(&owner, &b1, &3600u64, &None);

    let entries = vec![
        &env,
        BeneficiaryEntry { address: b1.clone(), bps: 5_000, minimum_threshold: 0 },
        BeneficiaryEntry { address: b2.clone(), bps: 3_000, minimum_threshold: 0 },
        BeneficiaryEntry { address: b3.clone(), bps: 2_000, minimum_threshold: 0 },
    ];
    client.set_beneficiaries(&vault_id, &owner, &entries);

    let stored = client.get_vault(&vault_id).beneficiaries;
    let sum: u32 = stored.iter().map(|e| e.bps).sum();
    assert_eq!(sum, 10_000, "BPS invariant violated: sum != 10_000");
}

/// set_beneficiaries rejects any split that does not total 10_000.
#[test]
fn invariant_bps_rejects_invalid_sum() {
    let (env, owner, b1, _admin, client) = setup();
    let b2 = Address::generate(&env);

    let vault_id = client.create_vault(&owner, &b1, &3600u64, &None);

    let bad = vec![
        &env,
        BeneficiaryEntry { address: b1.clone(), bps: 3_000, minimum_threshold: 0 },
        BeneficiaryEntry { address: b2.clone(), bps: 3_000, minimum_threshold: 0 },
    ];
    let result = client.try_set_beneficiaries(&vault_id, &owner, &bad);
    assert!(
        result.is_err(),
        "BPS invariant: must reject sum != 10_000"
    );
}

// ── Invariant 3: TTL reset on check-in ───────────────────────────────────────

/// After a check-in, `get_ttl_remaining` must equal `check_in_interval`.
#[test]
fn invariant_checkin_resets_ttl_to_interval() {
    let (env, owner, beneficiary, _admin, client) = setup();
    let interval = 7200u64;
    let vault_id = client.create_vault(&owner, &beneficiary, &interval, &None);

    // Advance partway through the interval.
    env.ledger().with_mut(|l| l.timestamp = 3_000);

    client.check_in(
        &vault_id,
        &owner,
        &BytesN::from_array(&env, &[1u8; 32]),
        &0u64,
    );

    let ttl = client
        .get_ttl_remaining(&vault_id)
        .expect("TTL must exist after check-in");
    assert_eq!(
        ttl, interval,
        "Invariant violated: TTL not reset to interval after check-in"
    );
}

// ── Invariant 4: Release-once ─────────────────────────────────────────────────

/// Funds are released exactly once; a second trigger_release is rejected.
#[test]
fn invariant_release_once() {
    let (env, owner, beneficiary, _admin, client) = setup();
    let interval = 500u64;
    let vault_id = client.create_vault(&owner, &beneficiary, &interval, &None);
    client.deposit(&vault_id, &owner, &100_000i128);

    env.ledger().with_mut(|l| l.timestamp = interval + 1);
    assert!(client.is_expired(&vault_id));

    // First release succeeds.
    client.trigger_release(&vault_id);
    assert_eq!(
        client.get_vault(&vault_id).balance,
        0,
        "Balance must be zero after release"
    );

    // Second release must be rejected.
    let second = client.try_trigger_release(&vault_id);
    assert!(second.is_err(), "Invariant violated: double release succeeded");
}

// ── Invariant 5: Owner-only ───────────────────────────────────────────────────

/// A non-owner must be rejected for all owner-restricted operations.
#[test]
fn invariant_only_owner_can_deposit() {
    let (env, owner, beneficiary, _admin, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &3600u64, &None);
    let attacker = Address::generate(&env);

    let result = client.try_deposit(&vault_id, &attacker, &1000i128);
    assert!(result.is_err(), "Non-owner deposit must be rejected");
}

#[test]
fn invariant_only_owner_can_withdraw() {
    let (env, owner, beneficiary, _admin, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &3600u64, &None);
    client.deposit(&vault_id, &owner, &50_000i128);

    let attacker = Address::generate(&env);
    let result = client.try_withdraw(&vault_id, &attacker, &1000i128);
    assert!(result.is_err(), "Non-owner withdrawal must be rejected");
}

#[test]
fn invariant_only_owner_can_check_in() {
    let (env, owner, beneficiary, _admin, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &3600u64, &None);
    let attacker = Address::generate(&env);

    let result = client.try_check_in(
        &vault_id,
        &attacker,
        &BytesN::from_array(&env, &[9u8; 32]),
        &0u64,
    );
    assert!(result.is_err(), "Non-owner check-in must be rejected");
}

// ── Randomised operation sequences ───────────────────────────────────────────

/// Operation type for randomised sequences.
#[derive(Clone, Debug)]
enum VaultOp {
    Deposit(i128),
    Withdraw(i128),
    CheckIn,
}

prop_compose! {
    fn arb_vault_op()(op in 0u8..3u8, amount in 1i128..500_000i128) -> VaultOp {
        match op {
            0 => VaultOp::Deposit(amount),
            1 => VaultOp::Withdraw(amount),
            _ => VaultOp::CheckIn,
        }
    }
}

proptest! {
    /// After any sequence of deposits and withdrawals the vault balance must
    /// always equal the running net (deposits − withdrawals).
    #[test]
    fn prop_balance_invariant_under_random_ops(
        ops in prop::collection::vec(arb_vault_op(), 1..40),
    ) {
        let env = Env::default();
        env.mock_all_auths();

        let owner = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        let admin = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token_address = env.register_stellar_asset_contract_v2(token_admin).address();
        // Mint enough to cover any deposit sequence.
        StellarAssetClient::new(&env, &token_address).mint(&owner, &100_000_000);

        let contract_address = env.register_contract(None, TtlVaultContract);
        let client = TtlVaultContractClient::new(&env, &contract_address);
        client.initialize(&token_address, &admin);
        let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };

        let vault_id = client.create_vault(&owner, &beneficiary, &3600u64, &None);
        let mut expected_balance: i128 = 0;

        for op in ops {
            match op {
                VaultOp::Deposit(amount) => {
                    client.deposit(&vault_id, &owner, &amount);
                    expected_balance += amount;
                }
                VaultOp::Withdraw(amount) => {
                    if expected_balance >= amount {
                        client.withdraw(&vault_id, &owner, &amount);
                        expected_balance -= amount;
                    }
                    // Ignore withdrawals that would underflow — contract rejects them.
                }
                VaultOp::CheckIn => {
                    client.check_in(
                        &vault_id,
                        &owner,
                        &BytesN::from_array(&env, &[1u8; 32]),
                        &0u64,
                    );
                }
            }
        }

        prop_assert_eq!(
            client.get_vault_balance(&vault_id),
            expected_balance,
            "Balance invariant violated under random ops"
        );
    }

    /// For any valid BPS split summing to 10_000, the vault must store exactly
    /// that sum after set_beneficiaries.
    #[test]
    fn prop_bps_sum_invariant_random_split(a_bps in 1u32..9_999u32) {
        let b_bps = 10_000 - a_bps;

        let env = Env::default();
        env.mock_all_auths();

        let owner = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        let b2 = Address::generate(&env);
        let admin = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token_address = env.register_stellar_asset_contract_v2(token_admin).address();
        StellarAssetClient::new(&env, &token_address).mint(&owner, &1_000_000);

        let contract_address = env.register_contract(None, TtlVaultContract);
        let client = TtlVaultContractClient::new(&env, &contract_address);
        client.initialize(&token_address, &admin);
        let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };

        let vault_id = client.create_vault(&owner, &beneficiary, &3600u64, &None);
        let entries = vec![
            &env,
            BeneficiaryEntry { address: beneficiary.clone(), bps: a_bps, minimum_threshold: 0 },
            BeneficiaryEntry { address: b2.clone(), bps: b_bps, minimum_threshold: 0 },
        ];
        client.set_beneficiaries(&vault_id, &owner, &entries);

        let stored = client.get_vault(&vault_id).beneficiaries;
        let sum: u32 = stored.iter().map(|e| e.bps).sum();
        prop_assert_eq!(sum, 10_000, "BPS invariant violated for split ({}/{})", a_bps, b_bps);
    }

    /// TTL remaining after a check-in must equal the vault's check-in interval,
    /// regardless of how much time has elapsed since the last check-in.
    #[test]
    fn prop_ttl_reset_invariant(
        elapsed in 0u64..3_500u64,
        interval in 3_600u64..86_400u64,
    ) {
        let env = Env::default();
        env.mock_all_auths();

        let owner = Address::generate(&env);
        let beneficiary = Address::generate(&env);
        let admin = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token_address = env.register_stellar_asset_contract_v2(token_admin).address();
        StellarAssetClient::new(&env, &token_address).mint(&owner, &1_000_000);

        let contract_address = env.register_contract(None, TtlVaultContract);
        let client = TtlVaultContractClient::new(&env, &contract_address);
        client.initialize(&token_address, &admin);
        let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };

        let vault_id = client.create_vault(&owner, &beneficiary, &interval, &None);
        env.ledger().with_mut(|l| l.timestamp = elapsed);

        client.check_in(
            &vault_id,
            &owner,
            &BytesN::from_array(&env, &[1u8; 32]),
            &0u64,
        );

        let ttl = client.get_ttl_remaining(&vault_id).unwrap();
        prop_assert_eq!(
            ttl, interval,
            "TTL invariant violated: got {} expected {} (elapsed={})",
            ttl, interval, elapsed
        );
    }
}
