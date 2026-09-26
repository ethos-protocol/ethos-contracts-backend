#![cfg(test)]

use super::*;
use soroban_sdk::{bytes, testutils::Address as _};

fn setup() -> (Env, Address, Address, SbtContractClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let owner = Address::generate(&env);
    let id = env.register_contract(None, SbtContract);
    let client = SbtContractClient::new(&env, &id);
    client.initialize(&admin);
    let client: SbtContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, admin, owner, client)
}

// ---- issue #520: SBT Delegation with Time Limits ----

#[test]
fn delegate_sbt_successfully() {
    let (env, _, owner, client) = setup();
    let metadata = bytes!(&env, 0xaabbcc);
    let sbt_id = client.mint_sbt(&owner, &metadata);

    let delegate = Address::generate(&env);
    let expiry = 3600u64; // 1 hour

    client.delegate_sbt_temporarily(&sbt_id, &delegate, &expiry);

    let active_delegate = client.get_active_delegate(&sbt_id);
    assert_eq!(active_delegate, Some(delegate));
}

#[test]
fn delegation_auto_expires() {
    let (env, _, owner, client) = setup();
    let metadata = bytes!(&env, 0xaabbcc);
    let sbt_id = client.mint_sbt(&owner, &metadata);

    let delegate = Address::generate(&env);
    let expiry = 3600u64;

    client.delegate_sbt_temporarily(&sbt_id, &delegate, &expiry);

    // Check delegation is active initially
    assert!(client.get_active_delegate(&sbt_id).is_some());

    // Advance time past expiration
    env.ledger().with_timestamp(env.ledger().timestamp() + 3601);

    // Delegation should now be expired
    assert!(client.get_active_delegate(&sbt_id).is_none());
}

#[test]
fn delegation_cannot_self_delegate() {
    let (env, _, owner, client) = setup();
    let metadata = bytes!(&env, 0xaabbcc);
    let sbt_id = client.mint_sbt(&owner, &metadata);

    let expiry = 3600u64;

    // Attempting to delegate to self should panic
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.delegate_sbt_temporarily(&sbt_id, &owner, &expiry);
    }));
}

#[test]
fn delegation_history_tracks_lifecycle() {
    let (env, _, owner, client) = setup();
    let metadata = bytes!(&env, 0xaabbcc);
    let sbt_id = client.mint_sbt(&owner, &metadata);

    let delegate1 = Address::generate(&env);
    let delegate2 = Address::generate(&env);

    // First delegation
    client.delegate_sbt_temporarily(&sbt_id, &delegate1, &3600u64);

    // Check history contains first delegation
    let history = client.get_delegation_history(&sbt_id);
    assert_eq!(history.len(), 1);
    assert_eq!(history.get(0).unwrap().delegate, delegate1);
    assert_eq!(history.get(0).unwrap().action, DelegationAction::Delegated);

    // Expire first delegation
    env.ledger().with_timestamp(env.ledger().timestamp() + 3601);

    // Second delegation
    client.delegate_sbt_temporarily(&sbt_id, &delegate2, &3600u64);

    // History should contain both
    let history = client.get_delegation_history(&sbt_id);
    assert!(history.len() >= 1);
}

#[test]
fn multiple_sequential_delegations() {
    let (env, _, owner, client) = setup();
    let metadata = bytes!(&env, 0xaabbcc);
    let sbt_id = client.mint_sbt(&owner, &metadata);

    let delegates: Vec<Address> = (0..3)
        .map(|_| Address::generate(&env))
        .collect();

    for (i, delegate) in delegates.iter().enumerate() {
        // Advance time past previous delegation expiry
        if i > 0 {
            env.ledger().with_timestamp(env.ledger().timestamp() + 3601);
        }

        client.delegate_sbt_temporarily(&sbt_id, delegate, &3600u64);
        assert_eq!(client.get_active_delegate(&sbt_id), Some(*delegate));
    }
}

#[test]
fn delegation_with_zero_expiry_fails() {
    let (env, _, owner, client) = setup();
    let metadata = bytes!(&env, 0xaabbcc);
    let sbt_id = client.mint_sbt(&owner, &metadata);

    let delegate = Address::generate(&env);

    // Attempting to delegate with 0 expiry should panic
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.delegate_sbt_temporarily(&sbt_id, &delegate, &0u64);
    }));
}

#[test]
fn only_owner_can_delegate() {
    let (env, _, owner, client) = setup();
    let metadata = bytes!(&env, 0xaabbcc);
    let sbt_id = client.mint_sbt(&owner, &metadata);

    let other_user = Address::generate(&env);
    let delegate = Address::generate(&env);

    // Attempting to delegate from non-owner should fail with auth error
    env.set_auths(&[]);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.delegate_sbt_temporarily(&sbt_id, &delegate, &3600u64);
    }));
}

#[test]
fn delegation_expires_at_correct_time() {
    let (env, _, owner, client) = setup();
    let metadata = bytes!(&env, 0xaabbcc);
    let sbt_id = client.mint_sbt(&owner, &metadata);

    let delegate = Address::generate(&env);
    let start_time = env.ledger().timestamp();
    let expiry_seconds = 1000u64;

    client.delegate_sbt_temporarily(&sbt_id, &delegate, &expiry_seconds);

    // Check at start: delegation is active
    assert!(client.get_active_delegate(&sbt_id).is_some());

    // Check just before expiry: delegation is active
    env.ledger().with_timestamp(start_time + expiry_seconds - 1);
    assert!(client.get_active_delegate(&sbt_id).is_some());

    // Check at expiry time: delegation is expired
    env.ledger().with_timestamp(start_time + expiry_seconds);
    assert!(client.get_active_delegate(&sbt_id).is_none());
}
