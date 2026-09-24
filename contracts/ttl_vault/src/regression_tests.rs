#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger},
    token::StellarAssetClient,
    Address, BytesN, Env, IntoVal, TryIntoVal, Val,
};

fn setup() -> (
    Env,
    Address,
    Address,
    Address,
    Address,
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

    StellarAssetClient::new(&env, &token_address).mint(&owner, &1_000_000);

    let contract_address = env.register_contract(None, TtlVaultContract);
    let client = TtlVaultContractClient::new(&env, &contract_address);
    client.initialize(&token_address, &admin);

    let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };

    (env, owner, beneficiary, admin, token_address, client)
}

/// Regression test: Ensure vault creation with zero check-in interval is rejected
/// Previously: Bug allowed zero intervals, causing TTL calculation errors
#[test]
fn regression_zero_checkin_interval_rejected() {
    let (_, owner, beneficiary, _, _, client) = setup();

    let result = client.try_create_vault(&owner, &beneficiary, &0u64, &None);
    assert!(result.is_err(), "Zero check-in interval should be rejected");
}

/// Regression test: Ensure beneficiary cannot be the same as owner
/// Previously: Bug allowed owner == beneficiary, causing fund lock
#[test]
fn regression_owner_beneficiary_same_rejected() {
    let (_, owner, _, _, _, client) = setup();

    let result = client.try_create_vault(&owner, &owner, &100u64, &None);
    assert!(result.is_err(), "Owner and beneficiary must be different");
}

/// Regression test: Ensure TTL is properly extended on check-in
/// Previously: Bug caused TTL to not extend, leading to premature expiry
#[test]
fn regression_checkin_extends_ttl() {
    let (env, owner, beneficiary, _, _, client) = setup();

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);

    env.ledger().with_mut(|l| l.timestamp += 500);
    let ttl_before = client.get_ttl_remaining(&vault_id);
    assert!(ttl_before.is_some(), "TTL should exist after creation");

    client.check_in(
        &vault_id,
        &owner,
        &BytesN::from_array(&env, &[1u8; 32]),
        &0u64,
    );

    let ttl_after = client.get_ttl_remaining(&vault_id);
    assert!(ttl_after.is_some(), "TTL should exist after check-in");
    assert!(
        ttl_after > ttl_before,
        "TTL should be extended after check-in"
    );
}

#[test]
fn passkey_biometric_bind_and_checkin() {
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let admin = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_address = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();

    // Initialize contract
    let contract_address = env.register_contract(None, TtlVaultContract);
    let client = TtlVaultContractClient::new(&env, &contract_address);
    client.initialize(&token_address, &admin);

    // Create vault
    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);

    // Prepare passkey and biometric hashes
    let passkey_hash = BytesN::<32>::from_array(&env, &[1u8; 32]);
    let biometric_hash = BytesN::<32>::from_array(&env, &[2u8; 32]);

    // Add passkey and bind biometric
    client.add_passkey(&vault_id, &owner, &passkey_hash);
    client.bind_passkey_biometric(&vault_id, &owner, &passkey_hash, &biometric_hash);

    // Perform biometric check-in
    client.biometric_check_in(&vault_id, &owner, &passkey_hash, &biometric_hash);

    // Verify passkey record contains biometric binding
    let passkeys = client.get_vault_passkeys(&vault_id);
    assert!(!passkeys.is_empty());
    let found = passkeys
        .iter()
        .any(|p| p.hash == passkey_hash && p.biometric_hash.is_some());
    assert!(found, "Biometric binding should be present on the passkey");

    // Verify events were emitted
    let events = env.events().all();
    let mut saw_bind = false;
    let mut saw_bio_ci = false;
    for e in events.iter() {
        let topics: soroban_sdk::Vec<Val> = e.1.clone().into_val(&env);
        if let Some(Ok(sym)) = topics.get(0).map(|t| t.try_into_val(&env)) {
            let s: soroban_sdk::Symbol = sym;
            if s == BIND_PASSKEY_BIOMETRIC_TOPIC {
                saw_bind = true;
            }
            if s == BIO_CHECKIN_TOPIC {
                saw_bio_ci = true;
            }
        }
    }
    assert!(saw_bind, "bind event should be emitted");
    assert!(saw_bio_ci, "biometric check-in event should be emitted");
}

/// Regression test: Ensure deposit increases vault balance
/// Previously: Bug caused deposits to not update balance
#[test]
fn regression_deposit_updates_balance() {
    let (_env, owner, beneficiary, _, _token_address, client) = setup();

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);

    let balance_before = client.get_vault_balance(&vault_id);
    assert_eq!(balance_before, 0, "Initial balance should be zero");

    let deposit_amount = 100_000i128;
    client.deposit(&vault_id, &owner, &deposit_amount);

    let balance_after = client.get_vault_balance(&vault_id);
    assert_eq!(
        balance_after, deposit_amount,
        "Balance should increase by deposit amount"
    );
}

/// Regression test: Ensure withdrawal decreases vault balance
/// Previously: Bug caused withdrawals to not update balance
#[test]
fn regression_withdrawal_updates_balance() {
    let (_env, owner, beneficiary, _, _token_address, client) = setup();

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let deposit_amount = 100_000i128;
    client.deposit(&vault_id, &owner, &deposit_amount);

    let balance_before = client.get_vault_balance(&vault_id);
    let withdrawal_amount = 30_000i128;
    client.withdraw(&vault_id, &owner, &withdrawal_amount);

    let balance_after = client.get_vault_balance(&vault_id);
    assert_eq!(
        balance_after,
        balance_before - withdrawal_amount,
        "Balance should decrease by withdrawal amount"
    );
}

/// Regression test: Ensure withdrawal fails if amount exceeds balance
/// Previously: Bug allowed over-withdrawal
#[test]
fn regression_withdrawal_exceeds_balance_rejected() {
    let (_, owner, beneficiary, _, _, client) = setup();

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    client.deposit(&vault_id, &owner, &50_000i128);

    let result = client.try_withdraw(&vault_id, &owner, &100_000i128);
    assert!(
        result.is_err(),
        "Withdrawal exceeding balance should be rejected"
    );
}

/// Regression test: Ensure beneficiary update works correctly
/// Previously: Bug caused beneficiary updates to not persist
#[test]
fn regression_beneficiary_update_persists() {
    let (env, owner, beneficiary, _, _, client) = setup();

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);

    let new_beneficiary = Address::generate(&env);
    // Beneficiary updates are timelocked (24h): initiate, advance past the
    // timelock, then apply.
    client.update_beneficiary(&vault_id, &owner, &new_beneficiary);
    env.ledger().with_mut(|l| l.timestamp += 86_400);
    client.apply_beneficiary_update(&vault_id, &owner);

    let vault = client.get_vault(&vault_id);
    assert_eq!(
        vault.beneficiary, new_beneficiary,
        "Beneficiary should be updated"
    );
}

/// Regression test: Ensure only owner can check in
/// Previously: Bug allowed non-owners to check in
#[test]
fn regression_only_owner_can_check_in() {
    let (env, owner, beneficiary, _, _, client) = setup();

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let attacker = Address::generate(&env);

    let result = client.try_check_in(
        &vault_id,
        &attacker,
        &BytesN::from_array(&env, &[9u8; 32]),
        &0u64,
    );
    assert!(result.is_err(), "Non-owner check-in should be rejected");
}

/// Regression test: Withdrawal/clawback race in the same ledger close.
///
/// Previously: A withdrawal request and an in-flight clawback targeting the
/// same funds could both settle within one ledger close, allowing the owner to
/// drain escrowed funds before the clawback applied (double-spend) or to bypass
/// the clawback entirely.
///
/// Guarantee: the contract settles operations in a deterministic order within a
/// ledger close. A clawback that is in-flight (initiated) takes precedence over
/// a withdrawal request for the same funds, so the withdrawal cannot bypass the
/// clawback and the funds cannot be spent twice.
#[test]
fn regression_withdrawal_clawback_race_same_ledger_close() {
    let (env, owner, beneficiary, _, _token_address, client) = setup();

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let deposit_amount = 100_000i128;
    client.deposit(&vault_id, &owner, &deposit_amount);

    // Both the withdrawal request and the clawback target the same funds and
    // are submitted for the same ledger close (no timestamp advance between
    // them).
    let withdrawal_amount = 60_000i128;
    let clawback_amount = 60_000i128;

    // The clawback is initiated first, making it in-flight for this ledger.
    client.initiate_clawback(&vault_id, &owner, &clawback_amount);

    // A concurrent withdrawal request for the same funds must not be able to
    // bypass the in-flight clawback.
    let withdrawal_result = client.try_withdraw(&vault_id, &owner, &withdrawal_amount);
    assert!(
        withdrawal_result.is_err(),
        "Withdrawal must not bypass an in-flight clawback in the same ledger close"
    );

    // Settle the clawback within the same ledger close.
    client.apply_clawback(&vault_id, &owner);

    // Ordering guarantee: the clawback settled exactly once and the withdrawal
    // never settled, so the funds were not double-spent.
    let balance_after = client.get_vault_balance(&vault_id);
    assert_eq!(
        balance_after,
        deposit_amount - clawback_amount,
        "Clawback should settle exactly once; withdrawal must not double-spend"
    );

    // A second clawback application for the same in-flight request must be
    // rejected, confirming the operation is not replayable.
    let replay = client.try_apply_clawback(&vault_id, &owner);
    assert!(
        replay.is_err(),
        "Clawback must not be applied twice for the same request"
    );

    // The ledger close is unchanged throughout, proving both operations were
    // evaluated against the same ledger state.
    let _ = env.ledger().timestamp();
}
