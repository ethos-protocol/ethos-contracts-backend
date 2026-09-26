//! Tests for multi-signature withdrawal approval — Issue #514.
//!
//! This module verifies:
//!   1. A large withdrawal threshold can be configured on a vault.
//!   2. Withdrawals below the threshold execute immediately.
//!   3. Large withdrawals (above threshold) require multi-sig approval.
//!   4. Multiple signers can approve a large withdrawal.
//!   5. A time window (24 hours) is enforced for collecting signatures.
//!   6. Expired multi-sig proposals are automatically rejected.

#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    vec, Address, Env,
};

fn setup() -> (
    Env,
    Address, // owner
    Address, // beneficiary
    Address, // signer1
    Address, // signer2
    Address, // contract address
    TtlVaultContractClient<'static>,
    u64,     // vault_id
) {
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let signer1 = Address::generate(&env);
    let signer2 = Address::generate(&env);
    let admin = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let token_address = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    StellarAssetClient::new(&env, &token_address).mint(&owner, &50_000_000);

    let contract_address = env.register_contract(None, TtlVaultContract);
    let client = TtlVaultContractClient::new(&env, &contract_address);
    client.initialize(&token_address, &admin);

    let vault_id = client.create_vault(&owner, &beneficiary, &604_800u64, &None);

    // Configure multi-sig with 2 signers
    client.configure_multisig(
        &vault_id,
        &owner,
        &vec![&env, signer1.clone(), signer2.clone()],
        &2u32,
    );

    let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, owner, beneficiary, signer1, signer2, contract_address, client, vault_id)
}

/// Setting a large withdrawal threshold stores the configuration.
#[test]
fn test_set_large_withdrawal_threshold() {
    let (_env, owner, _beneficiary, _s1, _s2, _ca, client, vault_id) = setup();

    // Set threshold to 5000 tokens
    client.set_large_withdrawal_threshold(&vault_id, &owner, &5000i128);

    // Verify the threshold is stored
    let vault = client.get_vault(&vault_id);
    assert_eq!(vault.large_withdrawal_threshold, 5000);
}

/// A withdrawal below the threshold should execute immediately without multi-sig.
#[test]
fn test_small_withdrawal_executes_immediately() {
    let (_env, owner, beneficiary, _s1, _s2, _ca, client, vault_id) = setup();

    // Set threshold to 5000 tokens
    client.set_large_withdrawal_threshold(&vault_id, &owner, &5000i128);

    // Deposit 10000 tokens
    client.deposit(&owner, &vault_id, &10000i128);

    // Beneficiary withdraws 1000 tokens (below threshold)
    client.withdraw(&beneficiary, &vault_id, &1000i128, &beneficiary);

    // Verify withdrawal executed immediately
    assert_eq!(client.get_vault(&vault_id).balance, 9000);
}

/// A withdrawal above the threshold should require multi-sig approval.
#[test]
fn test_large_withdrawal_requires_multisig() {
    let (_env, owner, beneficiary, signer1, signer2, _ca, client, vault_id) = setup();

    // Set threshold to 5000 tokens
    client.set_large_withdrawal_threshold(&vault_id, &owner, &5000i128);

    // Deposit 20000 tokens
    client.deposit(&owner, &vault_id, &20000i128);

    // Beneficiary requests a withdrawal of 10000 tokens (above threshold)
    let withdrawal_id = client.propose_large_withdrawal(&beneficiary, &vault_id, &10000i128, &beneficiary);

    // Withdrawal should be pending, not executed
    let vault = client.get_vault(&vault_id);
    assert_eq!(vault.balance, 20000);

    // Signer1 approves the withdrawal
    client.approve_large_withdrawal(&vault_id, &signer1, &withdrawal_id);

    // Signer2 approves the withdrawal
    client.approve_large_withdrawal(&vault_id, &signer2, &withdrawal_id);

    // After sufficient approvals, withdrawal should execute
    let vault = client.get_vault(&vault_id);
    assert_eq!(vault.balance, 10000);
}

/// A withdrawal requires enough signatures to meet the threshold.
#[test]
#[should_panic]
fn test_insufficient_signatures_blocks_withdrawal() {
    let (_env, owner, beneficiary, signer1, _s2, _ca, client, vault_id) = setup();

    // Set threshold to 5000 tokens with 2-of-2 multi-sig requirement
    client.set_large_withdrawal_threshold(&vault_id, &owner, &5000i128);

    // Deposit 20000 tokens
    client.deposit(&owner, &vault_id, &20000i128);

    // Beneficiary requests a withdrawal of 10000 tokens
    let withdrawal_id = client.propose_large_withdrawal(&beneficiary, &vault_id, &10000i128, &beneficiary);

    // Only signer1 approves (need 2 signatures)
    client.approve_large_withdrawal(&vault_id, &signer1, &withdrawal_id);

    // Try to execute with only 1 of 2 signatures — should fail
    client.execute_large_withdrawal(&vault_id, &beneficiary, &withdrawal_id);
}

/// Multi-sig approval requests expire after 24 hours.
#[test]
#[should_panic]
fn test_multisig_approval_expires_after_24_hours() {
    let (env, owner, beneficiary, signer1, signer2, _ca, client, vault_id) = setup();

    // Set threshold to 5000 tokens
    client.set_large_withdrawal_threshold(&vault_id, &owner, &5000i128);

    // Deposit 20000 tokens
    client.deposit(&owner, &vault_id, &20000i128);

    // Beneficiary requests a withdrawal of 10000 tokens
    let withdrawal_id = client.propose_large_withdrawal(&beneficiary, &vault_id, &10000i128, &beneficiary);

    // Signer1 and Signer2 approve
    client.approve_large_withdrawal(&vault_id, &signer1, &withdrawal_id);
    client.approve_large_withdrawal(&vault_id, &signer2, &withdrawal_id);

    // Advance time by 24+ hours (86400 seconds)
    let current_timestamp = env.ledger().timestamp();
    env.ledger().set_timestamp(current_timestamp + 86401);

    // Attempt to execute the withdrawal should fail (approval window expired)
    client.execute_large_withdrawal(&vault_id, &beneficiary, &withdrawal_id);
}

/// A signer can reject a multi-sig withdrawal approval.
#[test]
#[should_panic]
fn test_signer_rejection_blocks_withdrawal() {
    let (_env, owner, beneficiary, _signer1, signer2, _ca, client, vault_id) = setup();

    // Set threshold to 5000 tokens with 2-of-2 multi-sig requirement
    client.set_large_withdrawal_threshold(&vault_id, &owner, &5000i128);

    // Deposit 20000 tokens
    client.deposit(&owner, &vault_id, &20000i128);

    // Beneficiary requests a withdrawal of 10000 tokens
    let withdrawal_id = client.propose_large_withdrawal(&beneficiary, &vault_id, &10000i128, &beneficiary);

    // Signer2 rejects the withdrawal
    client.reject_large_withdrawal(&vault_id, &signer2, &withdrawal_id);

    // Try to execute — should fail due to rejection
    client.execute_large_withdrawal(&vault_id, &beneficiary, &withdrawal_id);
}

/// Multiple large withdrawals can have independent approval processes.
#[test]
fn test_multiple_concurrent_large_withdrawals() {
    let (_env, owner, beneficiary, signer1, signer2, _ca, client, vault_id) = setup();

    // Set threshold to 5000 tokens
    client.set_large_withdrawal_threshold(&vault_id, &owner, &5000i128);

    // Deposit 40000 tokens
    client.deposit(&owner, &vault_id, &40000i128);

    // Beneficiary requests first withdrawal of 10000 tokens
    let withdrawal_id1 = client.propose_large_withdrawal(&beneficiary, &vault_id, &10000i128, &beneficiary);

    // Beneficiary requests second withdrawal of 10000 tokens
    let withdrawal_id2 = client.propose_large_withdrawal(&beneficiary, &vault_id, &10000i128, &beneficiary);

    // Approve and execute first withdrawal
    client.approve_large_withdrawal(&vault_id, &signer1, &withdrawal_id1);
    client.approve_large_withdrawal(&vault_id, &signer2, &withdrawal_id1);
    client.execute_large_withdrawal(&vault_id, &beneficiary, &withdrawal_id1);

    // Approve and execute second withdrawal
    client.approve_large_withdrawal(&vault_id, &signer1, &withdrawal_id2);
    client.approve_large_withdrawal(&vault_id, &signer2, &withdrawal_id2);
    client.execute_large_withdrawal(&vault_id, &beneficiary, &withdrawal_id2);

    // Verify both withdrawals executed
    assert_eq!(client.get_vault(&vault_id).balance, 20000);
}

/// Changing the threshold should affect future withdrawals.
#[test]
fn test_changing_threshold_affects_future_withdrawals() {
    let (_env, owner, beneficiary, signer1, signer2, _ca, client, vault_id) = setup();

    // Set initial threshold to 5000 tokens
    client.set_large_withdrawal_threshold(&vault_id, &owner, &5000i128);

    // Deposit 20000 tokens
    client.deposit(&owner, &vault_id, &20000i128);

    // Withdraw 2000 tokens (below 5000 threshold) — should execute immediately
    client.withdraw(&beneficiary, &vault_id, &2000i128, &beneficiary);
    assert_eq!(client.get_vault(&vault_id).balance, 18000);

    // Raise threshold to 15000 tokens
    client.set_large_withdrawal_threshold(&vault_id, &owner, &15000i128);

    // Withdraw 10000 tokens (now below new threshold) — should execute immediately
    client.withdraw(&beneficiary, &vault_id, &10000i128, &beneficiary);
    assert_eq!(client.get_vault(&vault_id).balance, 8000);
}

/// Only configured signers can approve large withdrawals.
#[test]
#[should_panic]
fn test_unauthorized_signer_cannot_approve() {
    let (env, owner, beneficiary, signer1, _s2, _ca, client, vault_id) = setup();
    let unauthorized = Address::generate(&env);

    // Set threshold to 5000 tokens
    client.set_large_withdrawal_threshold(&vault_id, &owner, &5000i128);

    // Deposit 20000 tokens
    client.deposit(&owner, &vault_id, &20000i128);

    // Beneficiary requests a withdrawal of 10000 tokens
    let withdrawal_id = client.propose_large_withdrawal(&beneficiary, &vault_id, &10000i128, &beneficiary);

    // Unauthorized signer attempts to approve
    client.approve_large_withdrawal(&vault_id, &unauthorized, &withdrawal_id);
}
