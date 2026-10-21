#![cfg(test)]

use super::*;
use soroban_sdk::{testutils::Address as _, vec};

fn setup() -> (Env, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let owner = Address::generate(&env);
    let contract_id = env.register_contract(None, SbtContract);
    SbtContractClient::new(&env, &contract_id).initialize(&admin);
    (env, contract_id, owner)
}

fn mint(client: &SbtContractClient, owner: &Address) -> u64 {
    client.mint(owner, &String::from_str(client.env(), "token"))
}

#[test]
fn composition_resolution_is_bounded_and_round_trips() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let root = mint(&client, &owner);
    let child = mint(&client, &owner);
    let leaf = mint(&client, &owner);

    client.set_composition_components(&root, &vec![&env, child]);
    client.set_composition_components(&child, &vec![&env, leaf]);

    assert_eq!(client.resolve_composition(&root), vec![&env, child, leaf]);
}

#[test]
fn composition_rejects_self_and_indirect_cycles() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let first = mint(&client, &owner);
    let second = mint(&client, &owner);

    assert!(client
        .try_set_composition_components(&first, &vec![&env, first])
        .is_err());
    client.set_composition_components(&first, &vec![&env, second]);
    assert!(client
        .try_set_composition_components(&second, &vec![&env, first])
        .is_err());
}

// ---- issue #782: component count cap ----

/// Exactly MAX_COMPOSITION_COMPONENTS direct children must be accepted.
#[test]
fn set_composition_components_accepts_exactly_at_cap() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let root = mint(&client, &owner);

    let mut components = Vec::new(&env);
    for _ in 0..MAX_COMPOSITION_COMPONENTS {
        components.push_back(mint(&client, &owner));
    }

    // Should not panic — exactly at the cap.
    client.set_composition_components(&root, &components);
    assert_eq!(
        client.get_composition_components(&root).unwrap().len(),
        MAX_COMPOSITION_COMPONENTS,
    );
}

/// One component over the cap must be rejected with TooManyComponents.
#[test]
fn set_composition_components_rejects_above_cap() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let root = mint(&client, &owner);

    let mut components = Vec::new(&env);
    for _ in 0..=MAX_COMPOSITION_COMPONENTS {
        // MAX + 1 entries
        components.push_back(mint(&client, &owner));
    }

    let err = client
        .try_set_composition_components(&root, &components)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, SbtError::TooManyComponents);
}

// ---- issue #782: nesting depth cap ----

/// A chain of exactly MAX_COMPOSITION_DEPTH levels must resolve without error.
#[test]
fn resolve_composition_accepts_chain_at_max_depth() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);

    // Build a linear chain: ids[0] -> ids[1] -> ... -> ids[MAX_COMPOSITION_DEPTH - 1]
    // That is MAX_COMPOSITION_DEPTH - 1 edges, so resolve from ids[0]
    // recurses to depth MAX_COMPOSITION_DEPTH - 1 (< MAX), which is allowed.
    let mut ids: Vec<u64> = Vec::new(&env);
    for _ in 0..MAX_COMPOSITION_DEPTH {
        ids.push_back(mint(&client, &owner));
    }

    for i in 0..(MAX_COMPOSITION_DEPTH - 1) as usize {
        let parent = ids.get(i as u32).unwrap();
        let child = ids.get(i as u32 + 1).unwrap();
        client.set_composition_components(&parent, &vec![&env, child]);
    }

    // resolve from the root — must not panic.
    let root = ids.get(0).unwrap();
    let resolved = client.resolve_composition(&root);
    // All nodes except the root are in the resolved list.
    assert_eq!(resolved.len(), MAX_COMPOSITION_DEPTH - 1);
}

/// A chain one level deeper than MAX_COMPOSITION_DEPTH must be rejected.
#[test]
fn resolve_composition_rejects_chain_exceeding_max_depth() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);

    // MAX_COMPOSITION_DEPTH + 1 nodes creates MAX_COMPOSITION_DEPTH edges,
    // pushing the last recursion to depth == MAX_COMPOSITION_DEPTH which panics.
    let mut ids: Vec<u64> = Vec::new(&env);
    for _ in 0..=(MAX_COMPOSITION_DEPTH) {
        ids.push_back(mint(&client, &owner));
    }

    for i in 0..MAX_COMPOSITION_DEPTH as usize {
        let parent = ids.get(i as u32).unwrap();
        let child = ids.get(i as u32 + 1).unwrap();
        client.set_composition_components(&parent, &vec![&env, child]);
    }

    let root = ids.get(0).unwrap();
    assert!(client.try_resolve_composition(&root).is_err());
}
