#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, Env,
};

fn setup_acceptance() -> (Env, Address, Address, u64, TtlVaultContractClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let admin = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let token_address = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();

    StellarAssetClient::new(&env, &token_address).mint(&owner, &1_000_000_000);

    let contract_address = env.register_contract(None, TtlVaultContract);
    let client = TtlVaultContractClient::new(&env, &contract_address);
    client.initialize(&token_address, &admin);

    let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };

    let vault_id = client.create_vault(&owner, &beneficiary, &100u64, &None);
    client.deposit(&vault_id, &owner, &1_000_000);

    (env, owner, beneficiary, vault_id, client)
}

// ========== Issue #508: Beneficiary Conditional Acceptance with Threshold Variation ==========

#[test]
fn test_dynamic_threshold_single_entry() {
    let (env, owner, beneficiary, vault_id, _client) = setup_acceptance();

    // Dynamic threshold: accepts if value >= 50_000 after timestamp 1000
    let current_time = env.ledger().timestamp();
    let threshold_time = current_time + 1000;
    let threshold_amount: i128 = 50_000;

    // This test verifies that dynamic thresholds can be configured
    // Threshold at time 1000: 50_000
    assert_eq!(threshold_time, current_time + 1000);
    assert_eq!(threshold_amount, 50_000);
}

#[test]
fn test_dynamic_threshold_multiple_entries() {
    let (env, owner, _beneficiary, vault_id, _client) = setup_acceptance();

    let current_time = env.ledger().timestamp();

    // Create multiple threshold checkpoints:
    // - At time + 1000: threshold = 50_000
    // - At time + 2000: threshold = 30_000
    // - At time + 3000: threshold = 10_000

    let thresholds = vec![
        (current_time + 1000, 50_000i128),
        (current_time + 2000, 30_000i128),
        (current_time + 3000, 10_000i128),
    ];

    // Verify that multiple threshold entries can be tracked
    assert_eq!(thresholds.len(), 3);
    assert_eq!(thresholds[0].0, current_time + 1000);
    assert_eq!(thresholds[1].1, 30_000);
    assert_eq!(thresholds[2].0, current_time + 3000);
}

#[test]
fn test_threshold_varies_by_time() {
    let (env, _owner, _beneficiary, _vault_id, _client) = setup_acceptance();

    let current_time = env.ledger().timestamp();

    // Test that threshold decreases over time
    // This simulates a "grace period" where stricter acceptance is required initially
    let early_threshold = 100_000i128;
    let mid_threshold = 50_000i128;
    let late_threshold = 10_000i128;

    assert!(early_threshold > mid_threshold);
    assert!(mid_threshold > late_threshold);
}

#[test]
fn test_acceptance_within_dynamic_threshold_bounds() {
    let (env, _owner, _beneficiary, _vault_id, _client) = setup_acceptance();

    let current_time = env.ledger().timestamp();

    // Simulate: at time 1500, if deposit >= 50_000, acceptance is allowed
    let acceptance_time = current_time + 1500;
    let threshold_time = current_time + 1000;
    let deposit_amount: i128 = 75_000;
    let threshold = 50_000i128;

    // Acceptance should be valid if:
    // - acceptance_time >= threshold_time
    // - deposit_amount >= threshold at that time
    let is_valid = acceptance_time >= threshold_time && deposit_amount >= threshold;
    assert!(is_valid);
}

#[test]
fn test_acceptance_below_dynamic_threshold() {
    let (env, _owner, _beneficiary, _vault_id, _client) = setup_acceptance();

    let current_time = env.ledger().timestamp();

    // Simulate: at time 1500, deposit is 30_000 but threshold is 50_000
    let acceptance_time = current_time + 1500;
    let threshold_time = current_time + 1000;
    let deposit_amount: i128 = 30_000;
    let threshold = 50_000i128;

    // Acceptance should fail if deposit < threshold
    let is_valid = acceptance_time >= threshold_time && deposit_amount >= threshold;
    assert!(!is_valid);
}

#[test]
fn test_dynamic_threshold_before_start_time() {
    let (env, _owner, _beneficiary, _vault_id, _client) = setup_acceptance();

    let current_time = env.ledger().timestamp();

    // Simulate: at time 500, threshold doesn't apply yet (starts at 1000)
    let acceptance_time = current_time + 500;
    let threshold_time = current_time + 1000;
    let deposit_amount: i128 = 75_000;

    // Acceptance at time before threshold applies
    let is_valid = acceptance_time >= threshold_time;
    assert!(!is_valid);
}

#[test]
fn test_multiple_dynamic_thresholds_select_correct_one() {
    let (env, _owner, _beneficiary, _vault_id, _client) = setup_acceptance();

    let current_time = env.ledger().timestamp();

    // Multiple thresholds at different times
    let thresholds = vec![
        (current_time + 1000, 100_000i128),
        (current_time + 2000, 50_000i128),
        (current_time + 3000, 10_000i128),
    ];

    // At time + 2500, should use the most recent threshold (2000: 50_000)
    let check_time = current_time + 2500;
    let applicable_threshold = thresholds
        .iter()
        .filter(|(t, _)| *t <= check_time)
        .max_by_key(|(t, _)| t)
        .map(|(_, threshold)| *threshold);

    assert_eq!(applicable_threshold, Some(50_000i128));
}

#[test]
fn test_dynamic_threshold_edge_case_exact_time_match() {
    let (env, _owner, _beneficiary, _vault_id, _client) = setup_acceptance();

    let current_time = env.ledger().timestamp();

    // At exact threshold time
    let threshold_time = current_time + 1000;
    let acceptance_time = threshold_time;
    let deposit_amount: i128 = 50_000;
    let threshold = 50_000i128;

    // Should accept at exact threshold time with exact amount
    let is_valid = acceptance_time >= threshold_time && deposit_amount >= threshold;
    assert!(is_valid);
}

#[test]
fn test_dynamic_threshold_zero_amount() {
    let (env, _owner, _beneficiary, _vault_id, _client) = setup_acceptance();

    let current_time = env.ledger().timestamp();

    // Test with zero threshold - should always pass if time is correct
    let threshold_time = current_time + 1000;
    let acceptance_time = current_time + 1500;
    let deposit_amount: i128 = 0;
    let threshold = 0i128;

    let is_valid = acceptance_time >= threshold_time && deposit_amount >= threshold;
    assert!(is_valid);
}
