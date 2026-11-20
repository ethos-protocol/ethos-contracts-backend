#![cfg(test)]

use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, BytesN, Env, Vec,
};

use crate::{
    errors::ContractError,
    types::{PasskeyCredential, RecoveryRequest, RecoveryStatus},
    TtlVault, TtlVaultClient,
};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn setup() -> (Env, TtlVaultClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, TtlVault);
    let client = TtlVaultClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    (env, client, owner)
}

fn dummy_passkey(env: &Env, seed: u8) -> PasskeyCredential {
    let mut key = [0u8; 32];
    key[0] = seed;
    PasskeyCredential {
        public_key: BytesN::from_array(env, &key),
        credential_id: BytesN::from_array(env, &key),
    }
}

fn dummy_guardian(env: &Env, seed: u8) -> Address {
    let _ = seed;
    Address::generate(env)
}

/// Snapshot of all storage entries relevant to the recovery flow so that a
/// failed call can be proven to be failure-atomic.
#[derive(Clone, Debug, PartialEq)]
struct StorageSnapshot {
    account_owner: Option<Address>,
    passkeys: Vec<PasskeyCredential>,
    recovery_request: Option<RecoveryRequest>,
    recovery_status: Option<RecoveryStatus>,
    guardians: Vec<Address>,
}

fn snapshot(env: &Env, client: &TtlVaultClient, owner: &Address) -> StorageSnapshot {
    let _ = env;
    StorageSnapshot {
        account_owner: client.try_get_owner().ok().and_then(|r| r.ok()),
        passkeys: client
            .try_get_passkeys(owner)
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_else(|| Vec::new(env)),
        recovery_request: client
            .try_get_recovery_request(owner)
            .ok()
            .and_then(|r| r.ok()),
        recovery_status: client
            .try_get_recovery_status(owner)
            .ok()
            .and_then(|r| r.ok()),
        guardians: client
            .try_get_guardians(owner)
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_else(|| Vec::new(env)),
    }
}

// ---------------------------------------------------------------------------
// Failure-atomicity tests
// ---------------------------------------------------------------------------

#[test]
fn failed_recovery_with_no_request_returns_typed_error_and_changes_no_state() {
    let (env, client, owner) = setup();
    client.initialize(&owner);

    let before = snapshot(&env, &client, &owner);

    // No recovery request was ever initiated; completing recovery must fail
    // with a typed error rather than panicking.
    let result = client.try_complete_recovery(&owner);
    assert_eq!(result, Err(Ok(ContractError::RecoveryNotFound)));

    let after = snapshot(&env, &client, &owner);
    assert_eq!(before, after, "failed recovery mutated storage");
}

#[test]
fn failed_recovery_with_insufficient_approvals_returns_typed_error_and_changes_no_state() {
    let (env, client, owner) = setup();
    client.initialize(&owner);

    let g1 = dummy_guardian(&env, 1);
    let g2 = dummy_guardian(&env, 2);
    let mut guardians = Vec::new(&env);
    guardians.push_back(g1.clone());
    guardians.push_back(g2.clone());
    client.set_guardians(&owner, &guardians);

    let new_passkey = dummy_passkey(&env, 9);
    client.initiate_recovery(&owner, &new_passkey);

    let before = snapshot(&env, &client, &owner);

    // Only one of two required approvals; completion must fail with a typed
    // error and leave state untouched.
    client.approve_recovery(&g1, &owner);
    let result = client.try_complete_recovery(&owner);
    assert_eq!(result, Err(Ok(ContractError::InsufficientApprovals)));

    let after = snapshot(&env, &client, &owner);
    assert_eq!(before, after, "failed recovery mutated storage");
}

#[test]
fn failed_recovery_after_timelock_not_elapsed_returns_typed_error_and_changes_no_state() {
    let (env, client, owner) = setup();
    client.initialize(&owner);

    let g1 = dummy_guardian(&env, 1);
    let mut guardians = Vec::new(&env);
    guardians.push_back(g1.clone());
    client.set_guardians(&owner, &guardians);

    let new_passkey = dummy_passkey(&env, 7);
    client.initiate_recovery(&owner, &new_passkey);
    client.approve_recovery(&g1, &owner);

    let before = snapshot(&env, &client, &owner);

    // Timelock has not elapsed yet.
    let result = client.try_complete_recovery(&owner);
    assert_eq!(result, Err(Ok(ContractError::TimelockNotElapsed)));

    let after = snapshot(&env, &client, &owner);
    assert_eq!(before, after, "failed recovery mutated storage");
}

#[test]
fn failed_recovery_with_unknown_guardian_returns_typed_error_and_changes_no_state() {
    let (env, client, owner) = setup();
    client.initialize(&owner);

    let g1 = dummy_guardian(&env, 1);
    let mut guardians = Vec::new(&env);
    guardians.push_back(g1.clone());
    client.set_guardians(&owner, &guardians);

    let new_passkey = dummy_passkey(&env, 3);
    client.initiate_recovery(&owner, &new_passkey);

    let before = snapshot(&env, &client, &owner);

    let stranger = Address::generate(&env);
    let result = client.try_approve_recovery(&stranger, &owner);
    assert_eq!(result, Err(Ok(ContractError::Unauthorized)));

    let after = snapshot(&env, &client, &owner);
    assert_eq!(before, after, "failed recovery mutated storage");
}

#[test]
fn failed_recovery_with_duplicate_approval_returns_typed_error_and_changes_no_state() {
    let (env, client, owner) = setup();
    client.initialize(&owner);

    let g1 = dummy_guardian(&env, 1);
    let mut guardians = Vec::new(&env);
    guardians.push_back(g1.clone());
    client.set_guardians(&owner, &guardians);

    let new_passkey = dummy_passkey(&env, 4);
    client.initiate_recovery(&owner, &new_passkey);
    client.approve_recovery(&g1, &owner);

    let before = snapshot(&env, &client, &owner);

    let result = client.try_approve_recovery(&g1, &owner);
    assert_eq!(result, Err(Ok(ContractError::AlreadyApproved)));

    let after = snapshot(&env, &client, &owner);
    assert_eq!(before, after, "failed recovery mutated storage");
}

#[test]
fn failed_recovery_with_invalid_passkey_returns_typed_error_and_changes_no_state() {
    let (env, client, owner) = setup();
    client.initialize(&owner);

    let before = snapshot(&env, &client, &owner);

    // All-zero passkey is rejected by validation.
    let invalid = PasskeyCredential {
        public_key: BytesN::from_array(&env, &[0u8; 32]),
        credential_id: BytesN::from_array(&env, &[0u8; 32]),
    };
    let result = client.try_initiate_recovery(&owner, &invalid);
    assert_eq!(result, Err(Ok(ContractError::InvalidPasskey)));

    let after = snapshot(&env, &client, &owner);
    assert_eq!(before, after, "failed recovery mutated storage");
}

#[test]
fn failed_recovery_with_expired_request_returns_typed_error_and_changes_no_state() {
    let (env, client, owner) = setup();
    client.initialize(&owner);

    let g1 = dummy_guardian(&env, 1);
    let mut guardians = Vec::new(&env);
    guardians.push_back(g1.clone());
    client.set_guardians(&owner, &guardians);

    let new_passkey = dummy_passkey(&env, 5);
    client.initiate_recovery(&owner, &new_passkey);
    client.approve_recovery(&g1, &owner);

    // Advance well past the recovery expiry window.
    env.ledger().with_mut(|l| {
        l.timestamp += 60 * 60 * 24 * 365;
    });

    let before = snapshot(&env, &client, &owner);

    let result = client.try_complete_recovery(&owner);
    assert_eq!(result, Err(Ok(ContractError::RecoveryExpired)));

    let after = snapshot(&env, &client, &owner);
    assert_eq!(before, after, "failed recovery mutated storage");
}

// ---------------------------------------------------------------------------
// Regression test
// ---------------------------------------------------------------------------

/// Regression: previously the recovery path used `unwrap`/`expect`/`panic!`
/// and would abort the host on a missing recovery request. This test proves
/// the call now returns a typed error and leaves all storage untouched.
#[test]
fn regression_missing_recovery_request_does_not_panic() {
    let (env, client, owner) = setup();
    client.initialize(&owner);

    let before = snapshot(&env, &client, &owner);

    // Under the old implementation this would panic inside the contract.
    let result = client.try_complete_recovery(&owner);

    // Must be a typed error, not a host panic.
    assert_eq!(result, Err(Ok(ContractError::RecoveryNotFound)));

    let after = snapshot(&env, &client, &owner);
    assert_eq!(before, after, "failed recovery mutated storage");
}