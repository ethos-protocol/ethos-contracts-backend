/// Issue #557 — Vault history archival tests
///
/// ⚠️ WARNING: These tests are implementation-only and have **not** been run
/// as part of this change. Do not add integration/end-to-end assertions here.
#![cfg(test)]

extern crate alloc;

use super::*;
use crate::history_archive::{ARCHIVE_MIN_AGE_SECONDS, ARCHIVE_PAGE_SIZE};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Env,
};

fn setup() -> (Env, Address, Address, Address, TtlVaultContractClient<'static>) {
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

/// Seeds `count` state-transition entries one second apart starting at `start`.
fn seed_history(env: &Env, client: &TtlVaultContractClient, vault_id: u64, start: u64, count: u32) {
    let actor = Address::generate(env);
    env.as_contract(&client.address, || {
        let key = DataKey::StateTransitionLog(vault_id);
        let mut log: Vec<StateTransitionEntry> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(env));
        for i in 0..count {
            log.push_back(StateTransitionEntry {
                from_status: ReleaseStatus::Locked,
                to_status: ReleaseStatus::Locked,
                actor: actor.clone(),
                timestamp: start + u64::from(i),
            });
        }
        env.storage().persistent().set(&key, &log);
    });
}

fn age_past_cutoff(env: &Env) {
    env.ledger()
        .with_mut(|l| l.timestamp = 10_000 + ARCHIVE_MIN_AGE_SECONDS);
}

#[test]
fn test_archive_moves_whole_old_pages_only() {
    let (env, owner, beneficiary, _, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &86_400u64, &None);
    seed_history(&env, &client, vault_id, 1_000, ARCHIVE_PAGE_SIZE * 2 + 5);
    age_past_cutoff(&env);

    let pages = client.archive_vault_history(&vault_id, &owner);
    assert_eq!(pages, 2);
    assert_eq!(client.get_state_transition_log(&vault_id).len(), 5);

    let info = client.get_history_archive_info(&vault_id);
    assert_eq!(info.page_count, 2);
    assert_eq!(info.archived_entries, u64::from(ARCHIVE_PAGE_SIZE * 2));
}

#[test]
fn test_recent_entries_are_not_archived() {
    let (env, owner, beneficiary, _, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &86_400u64, &None);
    env.ledger().with_mut(|l| l.timestamp = 50_000);
    seed_history(&env, &client, vault_id, 49_000, ARCHIVE_PAGE_SIZE * 3);

    assert_eq!(client.archive_vault_history(&vault_id, &owner), 0);
    assert_eq!(
        client.get_state_transition_log(&vault_id).len(),
        ARCHIVE_PAGE_SIZE * 3
    );
}

#[test]
fn test_get_archived_history_returns_pages_in_order() {
    let (env, owner, beneficiary, _, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &86_400u64, &None);
    seed_history(&env, &client, vault_id, 1_000, ARCHIVE_PAGE_SIZE * 2);
    age_past_cutoff(&env);
    client.archive_vault_history(&vault_id, &owner);

    let p0 = client.get_archived_history(&vault_id, &0);
    let p1 = client.get_archived_history(&vault_id, &1);
    assert_eq!(p0.entries.len(), ARCHIVE_PAGE_SIZE);
    assert_eq!(p0.entries.get(0).unwrap().timestamp, 1_000);
    assert_eq!(
        p1.entries.get(0).unwrap().timestamp,
        1_000 + u64::from(ARCHIVE_PAGE_SIZE)
    );
    assert_eq!(p1.prev_hash, p0.hash);
}

#[test]
fn test_get_archived_history_missing_page_errors() {
    let (_env, owner, beneficiary, _, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &86_400u64, &None);
    let err = client
        .try_get_archived_history(&vault_id, &0)
        .unwrap_err()
        .unwrap();
    assert_eq!(
        err,
        soroban_sdk::Error::from_contract_error(ContractError::ArchivePageNotFound as u32)
    );
}

#[test]
fn test_archive_integrity_verifies_and_detects_tampering() {
    let (env, owner, beneficiary, _, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &86_400u64, &None);
    seed_history(&env, &client, vault_id, 1_000, ARCHIVE_PAGE_SIZE * 2);
    age_past_cutoff(&env);
    client.archive_vault_history(&vault_id, &owner);

    client.verify_history_archive(&vault_id);

    let page = client.get_archived_history(&vault_id, &0);
    assert!(client.verify_archived_history_page(&vault_id, &0, &page.entries));

    // Tamper with an off-chain copy.
    let mut tampered = page.entries.clone();
    let mut e = tampered.get(0).unwrap();
    e.timestamp += 1;
    tampered.set(0, e);
    assert!(!client.verify_archived_history_page(&vault_id, &0, &tampered));

    // Tamper with the stored body.
    env.as_contract(&client.address, || {
        let mut body = page.clone();
        body.entries = tampered.clone();
        env.storage()
            .persistent()
            .set(&history_archive::ArchiveKey::Page(vault_id, 0), &body);
    });
    assert!(client.try_verify_history_archive(&vault_id).is_err());
}

#[test]
fn test_archive_appends_across_multiple_runs() {
    let (env, owner, beneficiary, admin, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &86_400u64, &None);
    seed_history(&env, &client, vault_id, 1_000, ARCHIVE_PAGE_SIZE);
    age_past_cutoff(&env);
    assert_eq!(client.archive_vault_history(&vault_id, &owner), 1);

    seed_history(&env, &client, vault_id, 2_000, ARCHIVE_PAGE_SIZE);
    assert_eq!(client.archive_vault_history(&vault_id, &admin), 1);

    assert_eq!(client.get_history_archive_info(&vault_id).page_count, 2);
    assert_eq!(
        client.get_archived_history(&vault_id, &1).prev_hash,
        client.get_archived_history(&vault_id, &0).hash
    );
    client.verify_history_archive(&vault_id);
}

#[test]
fn test_archive_rejects_unrelated_caller() {
    let (env, owner, beneficiary, _, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &86_400u64, &None);
    let stranger = Address::generate(&env);
    let err = client
        .try_archive_vault_history(&vault_id, &stranger)
        .unwrap_err()
        .unwrap();
    assert_eq!(
        err,
        soroban_sdk::Error::from_contract_error(ContractError::NotOwner as u32)
    );
}
