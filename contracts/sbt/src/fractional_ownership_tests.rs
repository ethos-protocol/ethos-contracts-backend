#![cfg(test)]

use super::*;
use soroban_sdk::{testutils::Address as _, vec, String};

fn setup() -> (Env, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let owner = Address::generate(&env);
    let contract_id = env.register_contract(None, SbtContract);
    let client = SbtContractClient::new(&env, &contract_id);
    client.initialize(&admin);

    (env, contract_id, owner)
}

fn history_entry(env: &Env, sbt_id: u64, holder: &Address, fraction: u64) -> OwnershipHistoryEntry {
    OwnershipHistoryEntry {
        sbt_id,
        holder: holder.clone(),
        fraction,
        action: OwnershipAction::Created,
        at: env.ledger().timestamp(),
    }
}

fn history_total(env: &Env, contract_id: &Address, sbt_id: u64) -> u64 {
    env.as_contract(contract_id, || {
        SbtContract::load_ownership_history(env, sbt_id)
            .iter()
            .map(|entry| entry.fraction)
            .sum()
    })
}

// ---- issue #45: fractional ownership history sum guard ----

/// A valid split whose fractions sum to exactly 10_000 basis points is
/// accepted and seeds one history row per holder.
#[test]
fn create_fractional_sbt_accepts_a_full_allocation() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let first = Address::generate(&env);
    let second = Address::generate(&env);
    let sbt_id = client.mint(&owner, &String::from_str(&env, "credential"));

    client.create_fractional_sbt(
        &sbt_id,
        &vec![&env, first, second],
        &vec![&env, 6000u64, 4000u64],
    );

    assert!(client.is_fractional(&sbt_id));
    let fractional = client.get_fractional_ownership(&sbt_id).unwrap();
    assert_eq!(fractional.fractions, vec![&env, 6000u64, 4000u64]);
    assert_eq!(history_total(&env, &contract_id, sbt_id), TOTAL_BASIS_POINTS);
}

/// History rows summing to exactly 100% (10_000 bps) pass the guard.
#[test]
fn ownership_history_accepts_exactly_one_hundred_percent() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let sbt_id = client.mint(&owner, &String::from_str(&env, "credential"));
    let first = Address::generate(&env);
    let second = Address::generate(&env);

    env.as_contract(&contract_id, || {
        SbtContract::push_ownership_history(
            &env,
            sbt_id,
            history_entry(&env, sbt_id, &first, 6000),
        );
        SbtContract::push_ownership_history(
            &env,
            sbt_id,
            history_entry(&env, sbt_id, &second, 4000),
        );
    });

    assert_eq!(history_total(&env, &contract_id, sbt_id), TOTAL_BASIS_POINTS);
}

/// A sequence of history rows whose shares sum past 100% (6000 + 5000 =
/// 11000 bps) is rejected before any over-allocation is committed.
#[test]
#[should_panic(expected = "Error(Contract, #18)")]
fn ownership_history_rejects_allocation_exceeding_one_hundred_percent() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let sbt_id = client.mint(&owner, &String::from_str(&env, "credential"));
    let holder = Address::generate(&env);

    env.as_contract(&contract_id, || {
        SbtContract::push_ownership_history(
            &env,
            sbt_id,
            history_entry(&env, sbt_id, &holder, 6000),
        );
        SbtContract::push_ownership_history(
            &env,
            sbt_id,
            history_entry(&env, sbt_id, &holder, 5000),
        );
    });
}

/// A single row already over 100% (e.g. 10_001 bps) is rejected outright.
#[test]
#[should_panic(expected = "Error(Contract, #18)")]
fn ownership_history_rejects_a_single_over_one_hundred_percent_row() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let sbt_id = client.mint(&owner, &String::from_str(&env, "credential"));
    let holder = Address::generate(&env);

    env.as_contract(&contract_id, || {
        SbtContract::push_ownership_history(
            &env,
            sbt_id,
            history_entry(&env, sbt_id, &holder, TOTAL_BASIS_POINTS + 1),
        );
    });
}

/// Over-allocation aborts before any row is written: the history for the
/// token remains empty after a rejected push.
#[test]
fn ownership_history_over_allocation_commits_nothing() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let sbt_id = client.mint(&owner, &String::from_str(&env, "credential"));
    let holder = Address::generate(&env);

    env.as_contract(&contract_id, || {
        SbtContract::push_ownership_history(
            &env,
            sbt_id,
            history_entry(&env, sbt_id, &holder, 6000),
        );
        assert!(SbtContract::try_push_ownership_history(
            &env,
            sbt_id,
            history_entry(&env, sbt_id, &holder, 5000),
        )
        .is_err());
    });

    let history = env.as_contract(&contract_id, || {
        SbtContract::load_ownership_history(&env, sbt_id)
    });
    assert_eq!(history.len(), 1);
}
