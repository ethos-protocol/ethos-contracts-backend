//! Tests for Vault Donation Features (Issue #613)

#![cfg(test)]

use super::*;
use soroban_sdk::{testutils::Address as _, token, token::StellarAssetClient, Address, Env};

use crate::{TtlVaultContract, TtlVaultContractClient};

// ── Helpers ──────────────────────────────────────────────────────────────────

struct Fixture {
    env: Env,
    contract_id: Address,
    token: Address,
    owner: Address,
    donor: Address,
    recipient: Address,
    vault_id: u64,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let admin = Address::generate(&env);
    let donor = Address::generate(&env);
    let recipient = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    StellarAssetClient::new(&env, &token).mint(&owner, &10_000_000);
    StellarAssetClient::new(&env, &token).mint(&donor, &10_000_000);

    let contract_id = env.register_contract(None, TtlVaultContract);
    let client = TtlVaultContractClient::new(&env, &contract_id);
    client.initialize(&token, &admin);
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);
    client.deposit(&vault_id, &owner, &1_000_000);

    Fixture {
        env,
        contract_id,
        token,
        owner,
        donor,
        recipient,
        vault_id,
    }
}

// ── Donation account setup ───────────────────────────────────────────────────

#[test]
fn test_owner_can_set_donation_account() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);

    assert!(client.get_donation_account(&f.vault_id).is_none());

    client.set_donation_account(&f.vault_id, &f.owner, &f.recipient, &true);

    let account = client
        .get_donation_account(&f.vault_id)
        .expect("account should be configured");
    assert_eq!(account.recipient, f.recipient);
    assert!(account.public_visibility);
}

#[test]
fn test_non_owner_cannot_set_donation_account() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);

    let result = client.try_set_donation_account(&f.vault_id, &f.donor, &f.recipient, &true);
    assert!(result.is_err());
    assert!(client.get_donation_account(&f.vault_id).is_none());
}

#[test]
fn test_set_donation_account_replaces_existing() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);
    let other = Address::generate(&f.env);

    client.set_donation_account(&f.vault_id, &f.owner, &f.recipient, &true);
    client.set_donation_account(&f.vault_id, &f.owner, &other, &false);

    let account = client.get_donation_account(&f.vault_id).unwrap();
    assert_eq!(account.recipient, other);
    assert!(!account.public_visibility);
}

// ── Donations and tax receipts ───────────────────────────────────────────────

#[test]
fn test_donation_goes_to_charity_not_vault() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);
    let token_client = token::Client::new(&f.env, &f.token);
    client.set_donation_account(&f.vault_id, &f.owner, &f.recipient, &true);

    let vault_before = client.get_vault(&f.vault_id).balance;
    let donor_before = token_client.balance(&f.donor);

    client.donate(&f.vault_id, &f.donor, &250_000);

    assert_eq!(token_client.balance(&f.recipient), 250_000);
    assert_eq!(token_client.balance(&f.donor), donor_before - 250_000);
    assert_eq!(client.get_vault(&f.vault_id).balance, vault_before);
}

#[test]
fn test_donation_issues_sequential_receipts_with_distinct_hashes() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);
    client.set_donation_account(&f.vault_id, &f.owner, &f.recipient, &true);

    let first = client.donate(&f.vault_id, &f.donor, &100);
    let second = client.donate(&f.vault_id, &f.donor, &200);

    assert_eq!(first.receipt_no, 1);
    assert_eq!(second.receipt_no, 2);
    assert_eq!(first.amount, 100);
    assert_eq!(first.donor, f.donor);
    assert_eq!(first.recipient, f.recipient);
    assert_eq!(first.token_address, f.token);
    assert_ne!(first.receipt_hash, second.receipt_hash);

    let stored = client
        .get_donation_receipt(&f.vault_id, &2)
        .expect("receipt should be stored");
    assert_eq!(stored, second);
    assert!(client.get_donation_receipt(&f.vault_id, &3).is_none());
}

#[test]
fn test_totals_track_vault_and_donor() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);
    let other_donor = Address::generate(&f.env);
    client.set_donation_account(&f.vault_id, &f.owner, &f.recipient, &true);

    client.donate(&f.vault_id, &f.donor, &300);
    client.donate(&f.vault_id, &f.donor, &700);
    client.donate(&f.vault_id, &other_donor, &50);

    assert_eq!(client.get_donation_total(&f.vault_id), 1_050);
    assert_eq!(client.get_donor_total(&f.vault_id, &f.donor), 1_000);
    assert_eq!(client.get_donor_total(&f.vault_id, &other_donor), 50);
    assert_eq!(
        client.get_donor_total(&f.vault_id, &Address::generate(&f.env)),
        0
    );
}

#[test]
fn test_donation_without_account_is_rejected() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);

    let result = client.try_donate(&f.vault_id, &f.donor, &100);
    assert!(result.is_err());
    assert_eq!(client.get_donation_total(&f.vault_id), 0);
}

#[test]
fn test_non_positive_donation_is_rejected() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);
    client.set_donation_account(&f.vault_id, &f.owner, &f.recipient, &true);

    assert!(client.try_donate(&f.vault_id, &f.donor, &0).is_err());
    assert!(client.try_donate(&f.vault_id, &f.donor, &-5).is_err());
    assert_eq!(client.get_donation_total(&f.vault_id), 0);
}

// ── Public visibility and listing ────────────────────────────────────────────

#[test]
fn test_list_donations_paginates_and_caps_page_size() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);
    client.set_donation_account(&f.vault_id, &f.owner, &f.recipient, &true);
    for amount in 1..=5i128 {
        client.donate(&f.vault_id, &f.donor, &amount);
    }

    let page = client.list_donations(&f.vault_id, &1, &2);
    assert_eq!(page.len(), 2);
    assert_eq!(page.get(0).unwrap().receipt_no, 1);
    assert_eq!(page.get(1).unwrap().receipt_no, 2);

    let tail = client.list_donations(&f.vault_id, &4, &100);
    assert_eq!(tail.len(), 2);
    assert_eq!(tail.get(0).unwrap().receipt_no, 4);
    assert_eq!(tail.get(1).unwrap().receipt_no, 5);

    // A start past the last receipt returns an empty page.
    assert_eq!(client.list_donations(&f.vault_id, &9, &10).len(), 0);
}

#[test]
fn test_list_donations_requires_public_visibility() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);
    client.set_donation_account(&f.vault_id, &f.owner, &f.recipient, &false);
    client.donate(&f.vault_id, &f.donor, &100);

    // Totals stay public even when records are hidden.
    assert_eq!(client.get_donation_total(&f.vault_id), 100);
    assert!(client.try_list_donations(&f.vault_id, &1, &10).is_err());
}

#[test]
fn test_list_donations_without_account_is_rejected() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);

    assert!(client.try_list_donations(&f.vault_id, &1, &10).is_err());
}
