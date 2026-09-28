//! Load tests — issue #571
//!
//! ## Purpose
//!
//! These tests exercise the contract at scale to reveal bottlenecks that only
//! appear when handling thousands of vaults or tens of thousands of withdrawal
//! operations. Because Soroban runs deterministically on a simulated environment,
//! these tests are inherently single-threaded; "concurrency" means that all
//! operations are issued against the same contract instance within a single
//! simulated ledger context, which is the unit of parallelism on-chain.
//!
//! ## Scenarios
//!
//! | Test                                  | Scale             | Bottleneck targeted                  |
//! |---------------------------------------|-------------------|--------------------------------------|
//! | `load_1000_concurrent_vaults`         | 1 000 vaults      | vault storage iteration / ID counter |
//! | `load_10000_withdrawal_operations`    | 10 000 ops        | withdrawal processing loop           |
//! | `load_vault_check_in_throughput`      | 500 check-ins     | TTL update throughput                |
//! | `load_deposit_throughput`             | 1 000 deposits    | balance accumulation                 |
//! | `load_release_many_vaults`            | 200 releases      | release loop + token transfers       |
//!
//! ## Identifying bottlenecks
//!
//! Each test prints the total Soroban CPU and memory budget consumed so that
//! growth trends can be compared across releases:
//!
//! ```bash
//! cargo test -p ttl-vault load_ -- --nocapture
//! ```

#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, BytesN, Env,
};

// ── Shared setup ──────────────────────────────────────────────────────────────

struct LoadEnv {
    env: Env,
    token_address: Address,
    admin: Address,
    client: TtlVaultContractClient<'static>,
}

fn load_setup(mint_per_owner: i128) -> (LoadEnv, Address /* owner */, Address /* beneficiary */) {
    let env = Env::default();
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();

    let admin = Address::generate(&env);
    let owner = Address::generate(&env);
    let beneficiary = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let token_address = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    StellarAssetClient::new(&env, &token_address).mint(&owner, &mint_per_owner);

    let contract_address = env.register_contract(None, TtlVaultContract);
    let client = TtlVaultContractClient::new(&env, &contract_address);
    client.initialize(&token_address, &admin);

    let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };
    (LoadEnv { env, token_address, admin, client }, owner, beneficiary)
}

// ── Scenario 1: 1 000 concurrent vaults ──────────────────────────────────────

/// Create 1 000 vaults in a single contract instance and verify they are all
/// independently accessible. This stresses vault ID generation and storage.
#[test]
fn load_1000_concurrent_vaults() {
    const N: usize = 1_000;

    let (le, owner, beneficiary, ..) = load_setup(0);
    let env = &le.env;

    let mut vault_ids: alloc::vec::Vec<u64> = alloc::vec::Vec::with_capacity(N);

    for _ in 0..N {
        let owner_i = Address::generate(env);
        let beneficiary_i = Address::generate(env);
        // Mint tokens for each owner.
        StellarAssetClient::new(env, &le.token_address).mint(&owner_i, &10_000);

        let id = le.client.create_vault(&owner_i, &beneficiary_i, &3600u64, &None);
        vault_ids.push(id);
    }

    // All vault IDs must be unique.
    vault_ids.sort_unstable();
    vault_ids.dedup();
    assert_eq!(vault_ids.len(), N, "Vault IDs must all be unique for {} vaults", N);

    // Spot-check: every vault was created with zero balance.
    for &id in vault_ids.iter().step_by(100) {
        assert_eq!(
            le.client.get_vault_balance(&id),
            0,
            "Vault {} should have zero balance after creation",
            id
        );
    }

    let (cpu, mem) = print_budget(env, "load_1000_concurrent_vaults");
    // Soft budget assertions — values should grow at most linearly with N.
    assert!(cpu > 0, "CPU budget must be nonzero");
    assert!(mem > 0, "Memory budget must be nonzero");

    let _ = (owner, beneficiary); // suppress unused warnings
}

// ── Scenario 2: 10 000 withdrawal operations ──────────────────────────────────

/// Issue 10 000 withdrawal operations against a single vault to stress the
/// withdrawal processing path.
#[test]
fn load_10000_withdrawal_operations() {
    const N: u32 = 10_000;
    const DEPOSIT_PER_OP: i128 = 100;

    let (le, owner, beneficiary) = load_setup(DEPOSIT_PER_OP * N as i128);
    let env = &le.env;
    let vault_id = le.client.create_vault(&owner, &beneficiary, &3600u64, &None);

    // Fund the vault with enough for all withdrawals.
    le.client
        .deposit(&vault_id, &owner, &(DEPOSIT_PER_OP * N as i128));

    let balance_before = le.client.get_vault_balance(&vault_id);
    assert_eq!(balance_before, DEPOSIT_PER_OP * N as i128);

    for _ in 0..N {
        le.client.withdraw(&vault_id, &owner, &DEPOSIT_PER_OP);
    }

    let balance_after = le.client.get_vault_balance(&vault_id);
    assert_eq!(
        balance_after, 0,
        "All {} withdrawals must drain vault balance to zero",
        N
    );

    print_budget(env, "load_10000_withdrawal_operations");
}

// ── Scenario 3: check-in throughput ───────────────────────────────────────────

/// Issue 500 check-ins against 10 different vaults (50 each) to stress TTL
/// update throughput and detect serialisation bottlenecks.
#[test]
fn load_vault_check_in_throughput() {
    const VAULTS: usize = 10;
    const CHECKINS_PER_VAULT: usize = 50;

    let (le, _, _) = load_setup(0);
    let env = &le.env;
    let interval = 3_600u64;

    let mut vault_ids = alloc::vec::Vec::with_capacity(VAULTS);
    let mut owners = alloc::vec::Vec::with_capacity(VAULTS);

    for _ in 0..VAULTS {
        let owner = Address::generate(env);
        let beneficiary = Address::generate(env);
        StellarAssetClient::new(env, &le.token_address).mint(&owner, &1_000);
        let id = le.client.create_vault(&owner, &beneficiary, &interval, &None);
        vault_ids.push(id);
        owners.push(owner);
    }

    let passkey = BytesN::from_array(env, &[1u8; 32]);

    for round in 0..CHECKINS_PER_VAULT {
        // Advance time so each check-in is within the interval.
        let t = (round as u64) * (interval / 2);
        env.ledger().with_mut(|l| l.timestamp = t);

        for (i, &vid) in vault_ids.iter().enumerate() {
            le.client.check_in(&vid, &owners[i], &passkey, &0u64);
        }
    }

    // Verify each vault is still alive.
    for &vid in &vault_ids {
        assert!(
            !le.client.is_expired(&vid),
            "Vault {} must still be active after repeated check-ins",
            vid
        );
    }

    print_budget(env, "load_vault_check_in_throughput");
}

// ── Scenario 4: deposit throughput ───────────────────────────────────────────

/// Issue 1 000 deposits against a single vault to stress the balance
/// accumulation path.
#[test]
fn load_deposit_throughput() {
    const N: u32 = 1_000;
    const AMOUNT: i128 = 1_000;

    let (le, owner, beneficiary) = load_setup(AMOUNT * N as i128);
    let env = &le.env;
    let vault_id = le.client.create_vault(&owner, &beneficiary, &3600u64, &None);

    for _ in 0..N {
        le.client.deposit(&vault_id, &owner, &AMOUNT);
    }

    let expected: i128 = AMOUNT * N as i128;
    assert_eq!(
        le.client.get_vault_balance(&vault_id),
        expected,
        "{} deposits of {} must accumulate to {}",
        N, AMOUNT, expected
    );

    print_budget(env, "load_deposit_throughput");
}

// ── Scenario 5: release many vaults ──────────────────────────────────────────

/// Create 200 expired vaults and trigger_release on each of them.  This
/// stresses the token-transfer and vault-state paths under sustained load.
#[test]
fn load_release_many_vaults() {
    const N: usize = 200;
    const DEPOSIT: i128 = 1_000;

    let (le, _, _) = load_setup(0);
    let env = &le.env;
    let interval = 500u64;

    let mut vault_ids = alloc::vec::Vec::with_capacity(N);
    let mut owners = alloc::vec::Vec::with_capacity(N);
    let mut beneficiaries = alloc::vec::Vec::with_capacity(N);

    for _ in 0..N {
        let owner = Address::generate(env);
        let beneficiary = Address::generate(env);
        StellarAssetClient::new(env, &le.token_address).mint(&owner, &DEPOSIT);

        let id = le.client.create_vault(&owner, &beneficiary, &interval, &None);
        le.client.deposit(&id, &owner, &DEPOSIT);

        vault_ids.push(id);
        owners.push(owner);
        beneficiaries.push(beneficiary);
    }

    // Expire all vaults.
    env.ledger().with_mut(|l| l.timestamp = interval + 1);

    for &id in &vault_ids {
        assert!(le.client.is_expired(&id), "Vault {} must be expired", id);
        le.client.trigger_release(&id);
        assert_eq!(
            le.client.get_vault_balance(&id),
            0,
            "Vault {} must have zero balance after release",
            id
        );
    }

    print_budget(env, "load_release_many_vaults");
}

// ── Budget helper ─────────────────────────────────────────────────────────────

fn print_budget(env: &Env, label: &str) -> (u64, u64) {
    let cpu = env.cost_estimate().budget().cpu_instruction_cost();
    let mem = env.cost_estimate().budget().mem_byte_cost();
    println!(
        "[load] {}: cpu={} mem={}",
        label, cpu, mem
    );
    (cpu, mem)
}
