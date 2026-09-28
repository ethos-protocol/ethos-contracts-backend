/// Issue #563 — Cursor-based pagination tests
///
/// ⚠️ WARNING: These tests are implementation-only and do **not** run the full
/// contract binary.  Do not add integration/end-to-end assertions here.
#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Env,
};

fn setup() -> (
    Env,
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
    StellarAssetClient::new(&env, &token_address).mint(&owner, &10_000_000);

    let contract_address = env.register_contract(None, TtlVaultContract);
    let client = TtlVaultContractClient::new(&env, &contract_address);
    client.initialize(&token_address, &admin);

    let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, owner, beneficiary, admin, client)
}

// ── list_vaults_by_owner ──────────────────────────────────────────────────────

#[test]
fn test_list_vaults_by_owner_first_page() {
    let (env, owner, beneficiary, _, client) = setup();

    // Create 5 vaults
    let mut vault_ids: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
    for _ in 0..5 {
        vault_ids.push(client.create_vault(&owner, &beneficiary, &1000u64, &None));
    }

    let page = client.list_vaults_by_owner(
        &owner,
        &None,
        &3u32,
        &VaultSortField::CreatedAt,
        &None,
    );

    assert_eq!(page.items.len(), 3);
    assert!(page.next_cursor.is_some(), "expected a next cursor");
    assert_eq!(page.total, 5);
    let _ = env;
}

#[test]
fn test_list_vaults_by_owner_second_page() {
    let (env, owner, beneficiary, _, client) = setup();

    for _ in 0..5 {
        client.create_vault(&owner, &beneficiary, &1000u64, &None);
    }

    let first = client.list_vaults_by_owner(
        &owner,
        &None,
        &3u32,
        &VaultSortField::CreatedAt,
        &None,
    );
    let cursor = first.next_cursor;

    let second = client.list_vaults_by_owner(
        &owner,
        &cursor,
        &3u32,
        &VaultSortField::CreatedAt,
        &None,
    );

    assert_eq!(second.items.len(), 2);
    assert!(second.next_cursor.is_none(), "no more pages expected");
    let _ = env;
}

#[test]
fn test_list_vaults_by_owner_exact_page_size() {
    let (env, owner, beneficiary, _, client) = setup();

    for _ in 0..4 {
        client.create_vault(&owner, &beneficiary, &1000u64, &None);
    }

    let page = client.list_vaults_by_owner(
        &owner,
        &None,
        &4u32,
        &VaultSortField::CreatedAt,
        &None,
    );

    assert_eq!(page.items.len(), 4);
    assert!(page.next_cursor.is_none(), "no more pages when items == limit");
    let _ = env;
}

#[test]
fn test_list_vaults_by_owner_empty() {
    let (env, owner, _, _, client) = setup();

    let page = client.list_vaults_by_owner(
        &owner,
        &None,
        &10u32,
        &VaultSortField::CreatedAt,
        &None,
    );

    assert_eq!(page.items.len(), 0);
    assert!(page.next_cursor.is_none());
    assert_eq!(page.total, 0);
    let _ = env;
}

#[test]
fn test_list_vaults_by_owner_sort_by_last_check_in() {
    let (env, owner, beneficiary, _, client) = setup();

    let v1 = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let _v2 = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let v3 = client.create_vault(&owner, &beneficiary, &1000u64, &None);

    // Advance time and check in v3 so it has a newer last_check_in
    env.ledger().with_mut(|l| l.timestamp += 500);
    let dummy_passkey = soroban_sdk::BytesN::from_array(&env, &[0u8; 32]);
    client.check_in(&v3, &owner, &dummy_passkey, &1u64).unwrap();

    // v1 still has original last_check_in
    let page = client.list_vaults_by_owner(
        &owner,
        &None,
        &3u32,
        &VaultSortField::LastCheckIn,
        &None,
    );

    // v3 should appear last (sorted ascending, newest at end)
    assert_eq!(page.items.len(), 3);
    assert_eq!(page.items.get(2).unwrap(), v3);
    let _ = env;
}

#[test]
fn test_list_vaults_by_owner_sort_by_balance() {
    let (env, owner, beneficiary, _, client) = setup();

    let v1 = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let v2 = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let _v3 = client.create_vault(&owner, &beneficiary, &1000u64, &None);

    // Deposit into v1 and v2 so they have higher balances
    client.deposit(&v1, &owner, &500_000i128);
    client.deposit(&v2, &owner, &200_000i128);

    let page = client.list_vaults_by_owner(
        &owner,
        &None,
        &3u32,
        &VaultSortField::Balance,
        &None,
    );

    // Balance sort is descending; v1 should be first
    assert_eq!(page.items.len(), 3);
    assert_eq!(page.items.get(0).unwrap(), v1);
    assert_eq!(page.items.get(1).unwrap(), v2);
    let _ = env;
}

#[test]
fn test_list_vaults_by_owner_status_filter() {
    let (env, owner, beneficiary, _, client) = setup();

    let _v1 = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let _v2 = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let v3 = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    client.cancel_vault(&v3, &owner).unwrap();

    let locked_page = client.list_vaults_by_owner(
        &owner,
        &None,
        &10u32,
        &VaultSortField::CreatedAt,
        &Some(ReleaseStatus::Locked),
    );
    assert_eq!(locked_page.items.len(), 2);

    let cancelled_page = client.list_vaults_by_owner(
        &owner,
        &None,
        &10u32,
        &VaultSortField::CreatedAt,
        &Some(ReleaseStatus::Cancelled),
    );
    assert_eq!(cancelled_page.items.len(), 1);
    assert_eq!(cancelled_page.items.get(0).unwrap(), v3);
    let _ = env;
}

#[test]
fn test_list_vaults_by_owner_limit_clamped_to_100() {
    let (env, owner, beneficiary, _, client) = setup();

    for _ in 0..5 {
        client.create_vault(&owner, &beneficiary, &1000u64, &None);
    }

    // Passing limit=200 should be clamped to 100 (returns all 5 since 5 < 100)
    let page = client.list_vaults_by_owner(
        &owner,
        &None,
        &200u32,
        &VaultSortField::CreatedAt,
        &None,
    );
    assert_eq!(page.items.len(), 5);
    let _ = env;
}

// ── list_vaults_by_beneficiary ────────────────────────────────────────────────

#[test]
fn test_list_vaults_by_beneficiary_pagination() {
    let (env, owner, beneficiary, _, client) = setup();

    for _ in 0..6 {
        client.create_vault(&owner, &beneficiary, &1000u64, &None);
    }

    let first = client.list_vaults_by_beneficiary(
        &beneficiary,
        &None,
        &4u32,
        &VaultSortField::CreatedAt,
        &None,
    );
    assert_eq!(first.items.len(), 4);
    assert!(first.next_cursor.is_some());
    assert_eq!(first.total, 6);

    let second = client.list_vaults_by_beneficiary(
        &beneficiary,
        &first.next_cursor,
        &4u32,
        &VaultSortField::CreatedAt,
        &None,
    );
    assert_eq!(second.items.len(), 2);
    assert!(second.next_cursor.is_none());
    let _ = env;
}

#[test]
fn test_list_vaults_by_beneficiary_no_duplicates_across_pages() {
    let (env, owner, beneficiary, _, client) = setup();

    for _ in 0..7 {
        client.create_vault(&owner, &beneficiary, &1000u64, &None);
    }

    let mut seen: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
    let mut cursor: Option<u64> = None;
    loop {
        let page = client.list_vaults_by_beneficiary(
            &beneficiary,
            &cursor,
            &3u32,
            &VaultSortField::CreatedAt,
            &None,
        );
        for id in page.items.iter() {
            assert!(seen.insert(id), "duplicate vault id {id} returned");
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen.len(), 7);
    let _ = env;
}
