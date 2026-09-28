/// Issue #556 — Compliance policy versioning tests
///
/// ⚠️ WARNING: These tests are implementation-only and have **not** been run
/// as part of this change. Do not add integration/end-to-end assertions here.
#![cfg(test)]

extern crate alloc;

use super::*;
use crate::compliance::ThresholdConfig;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Env,
};

fn setup() -> (Env, Address, TtlVaultContractClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_address = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();

    let contract_address = env.register_contract(None, TtlVaultContract);
    let client = TtlVaultContractClient::new(&env, &contract_address);
    client.initialize(&token_address, &admin);

    let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };
    env.ledger().with_mut(|l| l.timestamp = 1_000);
    (env, admin, client)
}

#[test]
fn test_no_policy_before_first_change() {
    let (_env, _, client) = setup();
    assert_eq!(client.get_policy_version_count(), 0);
    let err = client.try_get_policy_version(&1_000).unwrap_err().unwrap();
    assert_eq!(
        err,
        soroban_sdk::Error::from_contract_error(ContractError::PolicyVersionNotFound as u32)
    );
}

#[test]
fn test_setter_changes_record_versions() {
    let (env, admin, client) = setup();
    client.set_kyc_high_value_threshold(&5_000);
    env.ledger().with_mut(|l| l.timestamp = 2_000);
    client.set_allowlist_enforced(&true);

    assert_eq!(client.get_policy_version_count(), 2);
    let v1 = client.get_policy_by_version(&1).unwrap();
    assert_eq!(v1.kyc_high_value_threshold, 5_000);
    assert!(!v1.allowlist_enforced);
    assert_eq!(v1.effective_from, 1_000);
    assert_eq!(v1.created_by, admin);

    let v2 = client.get_policy_by_version(&2).unwrap();
    assert_eq!(v2.kyc_high_value_threshold, 5_000);
    assert!(v2.allowlist_enforced);
    assert_eq!(client.get_applied_policy_version(), 2);
}

#[test]
fn test_get_policy_version_by_timestamp() {
    let (env, _, client) = setup();
    client.set_kyc_high_value_threshold(&100);
    env.ledger().with_mut(|l| l.timestamp = 2_000);
    client.set_kyc_high_value_threshold(&200);
    env.ledger().with_mut(|l| l.timestamp = 3_000);
    client.set_kyc_high_value_threshold(&300);

    assert_eq!(client.get_policy_version(&1_000).version, 1);
    assert_eq!(client.get_policy_version(&1_999).kyc_high_value_threshold, 100);
    assert_eq!(client.get_policy_version(&2_500).kyc_high_value_threshold, 200);
    assert_eq!(client.get_policy_version(&9_999).kyc_high_value_threshold, 300);
    assert!(client.try_get_policy_version(&999).is_err());
}

#[test]
fn test_same_timestamp_changes_latest_version_wins() {
    let (_env, _, client) = setup();
    client.set_kyc_high_value_threshold(&100);
    client.set_kyc_high_value_threshold(&150);
    assert_eq!(client.get_policy_version(&1_000).version, 2);
}

#[test]
fn test_publish_policy_effective_now_applies_immediately() {
    let (_env, _, client) = setup();
    let thresholds = ThresholdConfig {
        single_tx_threshold: 10_000,
        cumulative_threshold: 50_000,
        window_seconds: 86_400,
    };
    let version =
        client.publish_compliance_policy(&7_000, &true, &Some(thresholds.clone()), &1_000);

    assert_eq!(client.get_applied_policy_version(), version);
    assert_eq!(client.get_kyc_high_value_threshold(), 7_000);
    assert!(client.is_allowlist_enforced());
    assert_eq!(client.get_reporting_thresholds(), Some(thresholds));
}

#[test]
fn test_scheduled_policy_applies_after_effective_date() {
    let (env, _, client) = setup();
    client.set_kyc_high_value_threshold(&100);
    let scheduled = client.publish_compliance_policy(&900, &false, &None, &5_000);

    // Not yet effective: live config unchanged, history reflects schedule.
    assert_eq!(client.get_kyc_high_value_threshold(), 100);
    assert_eq!(client.sync_compliance_policy(), None);
    assert_eq!(client.get_policy_version(&4_999).version, 1);
    assert_eq!(client.get_policy_version(&5_000).version, scheduled);

    env.ledger().with_mut(|l| l.timestamp = 5_000);
    assert_eq!(client.sync_compliance_policy(), Some(scheduled));
    assert_eq!(client.get_kyc_high_value_threshold(), 900);
    assert_eq!(client.get_applied_policy_version(), scheduled);
    assert_eq!(client.sync_compliance_policy(), None);
}

#[test]
fn test_publish_policy_rejects_past_effective_date() {
    let (_env, _, client) = setup();
    let err = client
        .try_publish_compliance_policy(&0, &false, &None, &999)
        .unwrap_err()
        .unwrap();
    assert_eq!(
        err,
        soroban_sdk::Error::from_contract_error(ContractError::InvalidEffectiveDate as u32)
    );
}

#[test]
fn test_publish_policy_rejects_invalid_thresholds() {
    let (_env, _, client) = setup();
    let bad = ThresholdConfig {
        single_tx_threshold: 0,
        cumulative_threshold: 10,
        window_seconds: 0,
    };
    assert!(client
        .try_publish_compliance_policy(&0, &false, &Some(bad), &1_000)
        .is_err());
    assert_eq!(client.get_policy_version_count(), 0);
}
