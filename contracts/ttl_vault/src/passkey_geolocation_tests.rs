#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    vec, Address, BytesN, Env, IntoVal, String as SorobanString,
};

fn setup() -> (Env, Address, Address, TtlVaultContractClient<'static>) {
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
    (env, owner, beneficiary, client)
}

/// Test: Set geolocation restriction with single country
#[test]
fn test_set_single_geolocation_restriction() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash = BytesN::<32>::from_array(&env, &[1u8; 32]);
    client.add_passkey(&vault_id, &owner, &hash);

    let countries = vec![&env, SorobanString::from_str(&env, "US")];
    client.set_passkey_geolocation(&vault_id, &owner, &hash, &countries);

    let restrictions = client.get_passkey_geolocation(&vault_id, &hash);
    assert_eq!(restrictions.len(), 1);
    assert_eq!(restrictions.get(0).unwrap(), &SorobanString::from_str(&env, "US"));
}

/// Test: Set geolocation restriction with multiple countries
#[test]
fn test_set_multiple_geolocation_restrictions() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash = BytesN::<32>::from_array(&env, &[1u8; 32]);
    client.add_passkey(&vault_id, &owner, &hash);

    let countries = vec![
        &env,
        SorobanString::from_str(&env, "US"),
        SorobanString::from_str(&env, "CA"),
        SorobanString::from_str(&env, "MX"),
    ];
    client.set_passkey_geolocation(&vault_id, &owner, &hash, &countries);

    let restrictions = client.get_passkey_geolocation(&vault_id, &hash);
    assert_eq!(restrictions.len(), 3);
}

/// Test: Owner-only can set geolocation restrictions
#[test]
fn test_geolocation_owner_only() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash = BytesN::<32>::from_array(&env, &[1u8; 32]);
    let non_owner = Address::generate(&env);

    client.add_passkey(&vault_id, &owner, &hash);

    let countries = vec![&env, SorobanString::from_str(&env, "US")];
    let result = client.try_set_passkey_geolocation(&vault_id, &non_owner, &hash, &countries);
    assert!(result.is_err());
}

/// Test: Check-in from allowed country succeeds
#[test]
fn test_checkin_from_allowed_country() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash = BytesN::<32>::from_array(&env, &[1u8; 32]);
    client.add_passkey(&vault_id, &owner, &hash);

    let countries = vec![&env, SorobanString::from_str(&env, "US")];
    client.set_passkey_geolocation(&vault_id, &owner, &hash, &countries);

    // Check-in from allowed location should succeed
    client.check_in_with_location(&vault_id, &owner, &hash, &0, &SorobanString::from_str(&env, "US"));

    let vault = client.get_vault(&vault_id);
    assert!(!vault.is_paused);
}

/// Test: Check-in from disallowed country fails
#[test]
fn test_checkin_from_disallowed_country() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash = BytesN::<32>::from_array(&env, &[1u8; 32]);
    client.add_passkey(&vault_id, &owner, &hash);

    let countries = vec![&env, SorobanString::from_str(&env, "US")];
    client.set_passkey_geolocation(&vault_id, &owner, &hash, &countries);

    // Check-in from disallowed location should fail
    let result = client.try_check_in_with_location(
        &vault_id,
        &owner,
        &hash,
        &0,
        &SorobanString::from_str(&env, "CN"),
    );
    assert!(result.is_err());
}

/// Test: Multiple passkeys can have different geolocation restrictions
#[test]
fn test_different_geolocation_per_passkey() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash1 = BytesN::<32>::from_array(&env, &[1u8; 32]);
    let hash2 = BytesN::<32>::from_array(&env, &[2u8; 32]);

    client.add_passkey(&vault_id, &owner, &hash1);
    client.add_passkey(&vault_id, &owner, &hash2);

    // Hash1 restricted to US
    let us_only = vec![&env, SorobanString::from_str(&env, "US")];
    client.set_passkey_geolocation(&vault_id, &owner, &hash1, &us_only);

    // Hash2 restricted to EU
    let eu_only = vec![&env, SorobanString::from_str(&env, "DE"), SorobanString::from_str(&env, "FR")];
    client.set_passkey_geolocation(&vault_id, &owner, &hash2, &eu_only);

    let restrictions1 = client.get_passkey_geolocation(&vault_id, &hash1);
    let restrictions2 = client.get_passkey_geolocation(&vault_id, &hash2);

    assert_eq!(restrictions1.len(), 1);
    assert_eq!(restrictions2.len(), 2);
}

/// Test: Update geolocation restrictions
#[test]
fn test_update_geolocation_restrictions() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash = BytesN::<32>::from_array(&env, &[1u8; 32]);
    client.add_passkey(&vault_id, &owner, &hash);

    // Set initial restrictions
    let countries1 = vec![&env, SorobanString::from_str(&env, "US")];
    client.set_passkey_geolocation(&vault_id, &owner, &hash, &countries1);

    // Update restrictions
    let countries2 = vec![
        &env,
        SorobanString::from_str(&env, "US"),
        SorobanString::from_str(&env, "CA"),
    ];
    client.set_passkey_geolocation(&vault_id, &owner, &hash, &countries2);

    let restrictions = client.get_passkey_geolocation(&vault_id, &hash);
    assert_eq!(restrictions.len(), 2);
}

/// Test: Remove geolocation restrictions
#[test]
fn test_remove_geolocation_restrictions() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash = BytesN::<32>::from_array(&env, &[1u8; 32]);
    client.add_passkey(&vault_id, &owner, &hash);

    let countries = vec![&env, SorobanString::from_str(&env, "US")];
    client.set_passkey_geolocation(&vault_id, &owner, &hash, &countries);

    // Remove restrictions (empty vector)
    let empty = vec![&env];
    client.set_passkey_geolocation(&vault_id, &owner, &hash, &empty);

    let restrictions = client.get_passkey_geolocation(&vault_id, &hash);
    assert_eq!(restrictions.len(), 0);
}

/// Test: Geolocation location tracking on check-in
#[test]
fn test_checkin_location_logged() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash = BytesN::<32>::from_array(&env, &[1u8; 32]);
    client.add_passkey(&vault_id, &owner, &hash);

    // Check-in from US
    client.check_in_with_location(
        &vault_id,
        &owner,
        &hash,
        &0,
        &SorobanString::from_str(&env, "US"),
    );

    let locations = client.get_checkin_locations(&vault_id);
    assert!(locations.len() >= 1);
    assert_eq!(locations.get(0).unwrap(), &SorobanString::from_str(&env, "US"));
}

/// Test: Multiple check-ins from different locations
#[test]
fn test_multiple_checkins_different_locations() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash = BytesN::<32>::from_array(&env, &[1u8; 32]);
    client.add_passkey(&vault_id, &owner, &hash);

    let countries = vec![
        &env,
        SorobanString::from_str(&env, "US"),
        SorobanString::from_str(&env, "CA"),
    ];
    client.set_passkey_geolocation(&vault_id, &owner, &hash, &countries);

    // Check-in from US
    client.check_in_with_location(&vault_id, &owner, &hash, &0, &SorobanString::from_str(&env, "US"));

    // Check-in from CA
    client.check_in_with_location(&vault_id, &owner, &hash, &0, &SorobanString::from_str(&env, "CA"));

    let locations = client.get_checkin_locations(&vault_id);
    assert_eq!(locations.len(), 2);
}

/// Test: Geolocation anomaly detection
#[test]
fn test_geolocation_anomaly_detection() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash = BytesN::<32>::from_array(&env, &[1u8; 32]);
    client.add_passkey(&vault_id, &owner, &hash);

    let countries = vec![&env, SorobanString::from_str(&env, "US")];
    client.set_passkey_geolocation(&vault_id, &owner, &hash, &countries);

    // Check-in from US
    client.check_in_with_location(&vault_id, &owner, &hash, &0, &SorobanString::from_str(&env, "US"));

    // Next check-in from same location
    client.check_in_with_location(&vault_id, &owner, &hash, &0, &SorobanString::from_str(&env, "US"));

    // Should not trigger anomaly
    let anomalies = client.get_geolocation_anomalies(&vault_id);
    assert_eq!(anomalies.len(), 0);
}

/// Test: Events emitted for geolocation check-in
#[test]
fn test_geolocation_checkin_event() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash = BytesN::<32>::from_array(&env, &[1u8; 32]);
    client.add_passkey(&vault_id, &owner, &hash);

    client.check_in_with_location(
        &vault_id,
        &owner,
        &hash,
        &0,
        &SorobanString::from_str(&env, "US"),
    );

    let events = env.events().all();
    let found = events.iter().any(|e| {
        let topics: soroban_sdk::Vec<soroban_sdk::Val> = e.1.clone().into_val(&env);
        topics
            .get(0)
            .and_then(|v| v.try_into_val::<soroban_sdk::Symbol>(&env).ok())
            .is_some_and(|s| s == soroban_sdk::symbol_short!("geo_ci"))
    });
    assert!(found, "expected geolocation_checkin event to be emitted");
}

/// Test: Geolocation blocking after failed restriction
#[test]
fn test_geolocation_blocking_alert() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash = BytesN::<32>::from_array(&env, &[1u8; 32]);
    client.add_passkey(&vault_id, &owner, &hash);

    let countries = vec![&env, SorobanString::from_str(&env, "US")];
    client.set_passkey_geolocation(&vault_id, &owner, &hash, &countries);

    // Try check-in from disallowed location
    let _result = client.try_check_in_with_location(
        &vault_id,
        &owner,
        &hash,
        &0,
        &SorobanString::from_str(&env, "CN"),
    );

    // Should generate alert
    let alerts = client.get_geolocation_alerts(&vault_id);
    assert!(alerts.len() > 0);
}

/// Test: Passkey removal clears geolocation data
#[test]
fn test_passkey_removal_clears_geolocation() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash = BytesN::<32>::from_array(&env, &[1u8; 32]);
    client.add_passkey(&vault_id, &owner, &hash);

    let countries = vec![&env, SorobanString::from_str(&env, "US")];
    client.set_passkey_geolocation(&vault_id, &owner, &hash, &countries);

    client.remove_passkey(&vault_id, &owner, &hash);

    let restrictions = client.get_passkey_geolocation(&vault_id, &hash);
    assert_eq!(restrictions.len(), 0);
}

/// Test: Geolocation with ISO country codes
#[test]
fn test_geolocation_iso_country_codes() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash = BytesN::<32>::from_array(&env, &[1u8; 32]);
    client.add_passkey(&vault_id, &owner, &hash);

    let countries = vec![
        &env,
        SorobanString::from_str(&env, "US"),
        SorobanString::from_str(&env, "GB"),
        SorobanString::from_str(&env, "JP"),
    ];
    client.set_passkey_geolocation(&vault_id, &owner, &hash, &countries);

    let restrictions = client.get_passkey_geolocation(&vault_id, &hash);
    assert_eq!(restrictions.len(), 3);
}

/// Test: Cannot set empty geolocation list for restricted passkey
#[test]
fn test_geolocation_empty_list_removes_restriction() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash = BytesN::<32>::from_array(&env, &[1u8; 32]);
    client.add_passkey(&vault_id, &owner, &hash);

    // Set restrictions
    let countries = vec![&env, SorobanString::from_str(&env, "US")];
    client.set_passkey_geolocation(&vault_id, &owner, &hash, &countries);

    // Clear restrictions with empty list
    let empty = vec![&env];
    client.set_passkey_geolocation(&vault_id, &owner, &hash, &empty);

    // Check-in should work from any location
    client.check_in_with_location(&vault_id, &owner, &hash, &0, &SorobanString::from_str(&env, "CN"));

    let vault = client.get_vault(&vault_id);
    assert!(!vault.is_paused);
}
