//! Tests for Vault NFT Minting (Issue #614)

#![cfg(test)]

use crate::vault_nft::MAX_NFT_METADATA_LEN;
use crate::{TtlVaultContract, TtlVaultContractClient};
use soroban_sdk::{testutils::Address as _, token::StellarAssetClient, Address, Env, String};

// ── Helpers ──────────────────────────────────────────────────────────────────

struct Fixture {
    env: Env,
    contract_id: Address,
    owner: Address,
    buyer: Address,
    vault_id: u64,
    token_id: u64,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let admin = Address::generate(&env);
    let buyer = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    StellarAssetClient::new(&env, &token).mint(&buyer, &10_000_000);
    StellarAssetClient::new(&env, &token).mint(&owner, &10_000_000);

    let contract_id = env.register_contract(None, TtlVaultContract);
    let client = TtlVaultContractClient::new(&env, &contract_id);
    client.initialize(&token, &admin);
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);
    let token_id = client
        .get_vault_nft(&vault_id)
        .expect("NFT should be minted on vault creation")
        .token_id;

    Fixture {
        env,
        contract_id,
        owner,
        buyer,
        vault_id,
        token_id,
    }
}

// ── Minting ──────────────────────────────────────────────────────────────────

#[test]
fn test_vault_creation_mints_nft_to_owner() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);

    let nft = client.get_vault_nft(&f.vault_id).expect("nft exists");
    assert_eq!(nft.vault_id, f.vault_id);
    assert_eq!(nft.minter, f.owner);
    assert_eq!(nft.owner, f.owner);
    assert_eq!(nft.metadata, String::from_str(&f.env, ""));
    assert!(nft.last_sale_price.is_none());
    assert_eq!(client.get_nft_owner(&f.vault_id), Some(f.owner.clone()));
}

#[test]
fn test_nft_is_retrievable_by_token_id() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);

    let nft = client.get_nft_by_token(&f.token_id).expect("nft exists");
    assert_eq!(nft.vault_id, f.vault_id);
    assert!(client.get_nft_by_token(&(f.token_id + 100)).is_none());
}

#[test]
fn test_each_vault_gets_a_distinct_nft() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);

    let other_owner = Address::generate(&f.env);
    let other_beneficiary = Address::generate(&f.env);
    let second_vault = client.create_vault(&other_owner, &other_beneficiary, &2_000u64, &None);
    let second = client.get_vault_nft(&second_vault).expect("nft exists");

    assert_ne!(second.token_id, f.token_id);
    assert_eq!(second.owner, other_owner);
}

// ── Metadata ─────────────────────────────────────────────────────────────────

#[test]
fn test_holder_can_set_metadata() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);
    let metadata = String::from_str(&f.env, "ipfs://QmExampleCid");

    client.set_nft_metadata(&f.vault_id, &f.owner, &metadata);

    assert_eq!(
        client.get_vault_nft(&f.vault_id).expect("nft").metadata,
        metadata
    );
}

#[test]
fn test_non_holder_cannot_set_metadata() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);
    let result =
        client.try_set_nft_metadata(&f.vault_id, &f.buyer, &String::from_str(&f.env, "hijack"));
    assert!(result.is_err());
}

#[test]
fn test_metadata_over_limit_is_rejected() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);
    let too_long = "a".repeat(MAX_NFT_METADATA_LEN as usize + 1);

    let result =
        client.try_set_nft_metadata(&f.vault_id, &f.owner, &String::from_str(&f.env, &too_long));
    assert!(result.is_err());
}

#[test]
fn test_metadata_for_missing_vault_nft_is_rejected() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);
    let result =
        client.try_set_nft_metadata(&(f.vault_id + 50), &f.owner, &String::from_str(&f.env, "x"));
    assert!(result.is_err());
}

// ── Secondary sales and ownership ────────────────────────────────────────────

#[test]
fn test_sale_transfers_ownership_and_pays_seller() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);
    let vault_token = client.get_vault(&f.vault_id).token_address;
    let token_client = soroban_sdk::token::Client::new(&f.env, &vault_token);
    let seller_before = token_client.balance(&f.owner);

    let sale = client.buy_nft(&f.token_id, &f.buyer, &500_000);

    assert_eq!(sale.seller, f.owner);
    assert_eq!(sale.buyer, f.buyer);
    assert_eq!(sale.price, 500_000);
    assert_eq!(sale.sale_no, 1);
    assert_eq!(token_client.balance(&f.owner), seller_before + 500_000);

    let after = client.get_nft_by_token(&f.token_id).expect("nft");
    assert_eq!(after.owner, f.buyer);
    assert_eq!(after.last_sale_price, Some(500_000));
    assert_eq!(after.minter, f.owner);
    assert_eq!(client.get_nft_owner(&f.vault_id), Some(f.buyer.clone()));
}

#[test]
fn test_new_holder_can_set_metadata_after_sale() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);
    client.buy_nft(&f.token_id, &f.buyer, &1_000);

    let metadata = String::from_str(&f.env, "after-sale");
    client.set_nft_metadata(&f.vault_id, &f.buyer, &metadata);

    assert!(client
        .try_set_nft_metadata(&f.vault_id, &f.owner, &metadata)
        .is_err());
}

#[test]
fn test_previous_holder_can_buy_back_as_regular_buyer() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);
    client.buy_nft(&f.token_id, &f.buyer, &1_000);

    // The original owner is now a regular buyer and can buy the NFT back.
    let sale = client.buy_nft(&f.token_id, &f.owner, &2_000);
    assert_eq!(sale.sale_no, 2);
    assert_eq!(
        client.get_nft_by_token(&f.token_id).expect("nft").owner,
        f.owner
    );
}

#[test]
fn test_holder_cannot_buy_own_nft() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);
    let result = client.try_buy_nft(&f.token_id, &f.owner, &1_000);
    assert!(result.is_err());
}

#[test]
fn test_non_positive_price_is_rejected() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);
    assert!(client.try_buy_nft(&f.token_id, &f.buyer, &0).is_err());
    assert!(client.try_buy_nft(&f.token_id, &f.buyer, &-5).is_err());
}

#[test]
fn test_buying_unknown_nft_is_rejected() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);
    assert!(client
        .try_buy_nft(&(f.token_id + 100), &f.buyer, &1_000)
        .is_err());
}

// ── Sales history ────────────────────────────────────────────────────────────

#[test]
fn test_sales_history_is_paginated_and_capped() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);

    // Ping-pong the NFT between the owner and buyer to create 3 sales.
    client.buy_nft(&f.token_id, &f.buyer, &100);
    client.buy_nft(&f.token_id, &f.owner, &200);
    client.buy_nft(&f.token_id, &f.buyer, &300);

    let all = client.list_nft_sales(&f.token_id, &1u64, &10u32);
    assert_eq!(all.len(), 3);
    assert_eq!(all.get(0).expect("first").price, 100);
    assert_eq!(all.get(2).expect("last").price, 300);

    let tail = client.list_nft_sales(&f.token_id, &2u64, &1u32);
    assert_eq!(tail.len(), 1);
    assert_eq!(tail.get(0).expect("second").price, 200);

    assert_eq!(client.list_nft_sales(&f.token_id, &10u64, &10u32).len(), 0);
}

#[test]
fn test_sales_history_page_size_is_capped() {
    let f = setup();
    let client = TtlVaultContractClient::new(&f.env, &f.contract_id);
    // Alternate holders so every purchase is from the current holder.
    for i in 0..51u32 {
        let buyer = if i % 2 == 0 { &f.buyer } else { &f.owner };
        client.buy_nft(&f.token_id, buyer, &1);
    }

    let page = client.list_nft_sales(&f.token_id, &1u64, &1_000u32);
    assert_eq!(page.len(), crate::vault_nft::MAX_NFT_SALES_PAGE_SIZE);
}
