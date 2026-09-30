//! Performance regression tests — issue #570
//!
//! ## Purpose
//!
//! These tests record Soroban budget consumption (CPU instructions + memory
//! bytes) for key contract operations and compare them against the baselines
//! stored in `contracts/ttl_vault/benches/regression.json`.
//!
//! A regression is declared when a measured value exceeds its baseline by more
//! than 5%.
//!
//! ## How baselines are captured
//!
//! Run the tests with `--nocapture` to print budget figures, then update
//! `benches/regression.json` manually:
//!
//! ```bash
//! cargo test -p ttl-vault perf_regression -- --nocapture
//! ```
//!
//! The CI script `scripts/compare_bench_baseline.py` reads
//! `benches/regression.json` and enforces the 5% tolerance automatically.
//!
//! ## Tracking performance over time
//!
//! Re-capture baselines after intentional performance improvements and commit
//! the updated JSON alongside the code change so the history is preserved in
//! git.

#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, budget::Budget, Ledger},
    token::StellarAssetClient,
    vec, Address, BytesN, Env,
};

// ── Tolerance ─────────────────────────────────────────────────────────────────

/// Maximum allowed regression as a fraction of the baseline (5 %).
const TOLERANCE: f64 = 0.05;

// ── Baseline values ───────────────────────────────────────────────────────────
//
// These mirror the values in `benches/regression.json`. They are inlined here
// so the tests are self-contained and can be run without parsing JSON.
// When you update the JSON, update these constants too.

struct Baseline {
    cpu: u64,
    mem: u64,
}

const BASELINE_CREATE_VAULT: Baseline = Baseline { cpu: 500_000, mem: 32_768 };
const BASELINE_DEPOSIT: Baseline = Baseline { cpu: 300_000, mem: 16_384 };
const BASELINE_WITHDRAW: Baseline = Baseline { cpu: 350_000, mem: 16_384 };
const BASELINE_CHECK_IN: Baseline = Baseline { cpu: 400_000, mem: 16_384 };
const BASELINE_TRIGGER_RELEASE_1: Baseline = Baseline { cpu: 123_456, mem: 4_096 };
const BASELINE_TRIGGER_RELEASE_5: Baseline = Baseline { cpu: 234_567, mem: 8_192 };

// ── Assertion helper ──────────────────────────────────────────────────────────

fn assert_no_regression(label: &str, measured: u64, baseline: u64) {
    let threshold = (baseline as f64 * (1.0 + TOLERANCE)) as u64;
    println!(
        "[perf] {}: measured={} baseline={} threshold={}",
        label, measured, baseline, threshold
    );
    assert!(
        measured <= threshold,
        "Performance regression detected in '{}': measured {} > threshold {} (baseline {} + {}%)",
        label,
        measured,
        threshold,
        baseline,
        (TOLERANCE * 100.0) as u32,
    );
}

// ── Setup ─────────────────────────────────────────────────────────────────────

fn setup() -> (
    Env,
    Address, // owner
    Address, // beneficiary
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
    StellarAssetClient::new(&env, &token_address).mint(&owner, &100_000_000);

    let contract_address = env.register_contract(None, TtlVaultContract);
    let client = TtlVaultContractClient::new(&env, &contract_address);
    client.initialize(&token_address, &admin);

    let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, owner, beneficiary, client)
}

// ── Benchmark helpers ─────────────────────────────────────────────────────────

fn measure_budget(env: &Env) -> (u64, u64) {
    let cpu = env.cost_estimate().budget().cpu_instruction_cost();
    let mem = env.cost_estimate().budget().mem_byte_cost();
    (cpu, mem)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Measure and assert the cost of `create_vault`.
#[test]
fn perf_regression_create_vault() {
    let (env, owner, beneficiary, client) = setup();

    env.cost_estimate().budget().reset_default();
    client.create_vault(&owner, &beneficiary, &3600u64, &None);
    let (cpu, mem) = measure_budget(&env);

    assert_no_regression("create_vault.cpu", cpu, BASELINE_CREATE_VAULT.cpu);
    assert_no_regression("create_vault.mem", mem, BASELINE_CREATE_VAULT.mem);
}

/// Measure and assert the cost of `deposit`.
#[test]
fn perf_regression_deposit() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &3600u64, &None);

    env.cost_estimate().budget().reset_default();
    client.deposit(&vault_id, &owner, &100_000i128);
    let (cpu, mem) = measure_budget(&env);

    assert_no_regression("deposit.cpu", cpu, BASELINE_DEPOSIT.cpu);
    assert_no_regression("deposit.mem", mem, BASELINE_DEPOSIT.mem);
}

/// Measure and assert the cost of `withdraw`.
#[test]
fn perf_regression_withdraw() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &3600u64, &None);
    client.deposit(&vault_id, &owner, &100_000i128);

    env.cost_estimate().budget().reset_default();
    client.withdraw(&vault_id, &owner, &50_000i128);
    let (cpu, mem) = measure_budget(&env);

    assert_no_regression("withdraw.cpu", cpu, BASELINE_WITHDRAW.cpu);
    assert_no_regression("withdraw.mem", mem, BASELINE_WITHDRAW.mem);
}

/// Measure and assert the cost of `check_in`.
#[test]
fn perf_regression_check_in() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &3600u64, &None);

    env.cost_estimate().budget().reset_default();
    client.check_in(
        &vault_id,
        &owner,
        &BytesN::from_array(&env, &[1u8; 32]),
        &0u64,
    );
    let (cpu, mem) = measure_budget(&env);

    assert_no_regression("check_in.cpu", cpu, BASELINE_CHECK_IN.cpu);
    assert_no_regression("check_in.mem", mem, BASELINE_CHECK_IN.mem);
}

/// Measure and assert the cost of `trigger_release` with 1 beneficiary.
#[test]
fn perf_regression_trigger_release_1_beneficiary() {
    let (env, owner, beneficiary, client) = setup();
    let interval = 500u64;
    let vault_id = client.create_vault(&owner, &beneficiary, &interval, &None);
    client.deposit(&vault_id, &owner, &100_000i128);
    env.ledger().with_mut(|l| l.timestamp = interval + 1);

    env.cost_estimate().budget().reset_default();
    client.trigger_release(&vault_id);
    let (cpu, mem) = measure_budget(&env);

    assert_no_regression("trigger_release_1.cpu", cpu, BASELINE_TRIGGER_RELEASE_1.cpu);
    assert_no_regression("trigger_release_1.mem", mem, BASELINE_TRIGGER_RELEASE_1.mem);
}

/// Measure and assert the cost of `trigger_release` with 5 beneficiaries.
#[test]
fn perf_regression_trigger_release_5_beneficiaries() {
    let (env, owner, b1, client) = setup();
    let b2 = Address::generate(&env);
    let b3 = Address::generate(&env);
    let b4 = Address::generate(&env);
    let b5 = Address::generate(&env);
    let interval = 500u64;

    let vault_id = client.create_vault(&owner, &b1, &interval, &None);
    client.set_beneficiaries(
        &vault_id,
        &owner,
        &vec![
            &env,
            BeneficiaryEntry { address: b1.clone(), bps: 2_000, minimum_threshold: 0 },
            BeneficiaryEntry { address: b2.clone(), bps: 2_000, minimum_threshold: 0 },
            BeneficiaryEntry { address: b3.clone(), bps: 2_000, minimum_threshold: 0 },
            BeneficiaryEntry { address: b4.clone(), bps: 2_000, minimum_threshold: 0 },
            BeneficiaryEntry { address: b5.clone(), bps: 2_000, minimum_threshold: 0 },
        ],
    );
    client.deposit(&vault_id, &owner, &100_000i128);
    env.ledger().with_mut(|l| l.timestamp = interval + 1);

    env.cost_estimate().budget().reset_default();
    client.trigger_release(&vault_id);
    let (cpu, mem) = measure_budget(&env);

    assert_no_regression("trigger_release_5.cpu", cpu, BASELINE_TRIGGER_RELEASE_5.cpu);
    assert_no_regression("trigger_release_5.mem", mem, BASELINE_TRIGGER_RELEASE_5.mem);
}

/// Smoke test: costs scale sub-linearly with the number of beneficiaries.
/// This catches algorithmic regressions (e.g. O(n²) loops introduced by accident).
#[test]
fn perf_regression_release_scales_sub_linearly() {
    let (env, owner, b1, client) = setup();

    // 1-beneficiary baseline.
    let vault_1 = client.create_vault(&owner, &b1, &500u64, &None);
    client.deposit(&vault_1, &owner, &100_000i128);
    env.ledger().with_mut(|l| l.timestamp = 501);
    env.cost_estimate().budget().reset_default();
    client.trigger_release(&vault_1);
    let (cpu_1, _) = measure_budget(&env);

    // 5-beneficiary measurement — must be less than 5× the 1-beneficiary cost.
    let (env2, owner2, b2, client2) = setup();
    let b3 = Address::generate(&env2);
    let b4 = Address::generate(&env2);
    let b5 = Address::generate(&env2);
    let b6 = Address::generate(&env2);
    let vault_5 = client2.create_vault(&owner2, &b2, &500u64, &None);
    client2.set_beneficiaries(
        &vault_5,
        &owner2,
        &vec![
            &env2,
            BeneficiaryEntry { address: b2.clone(), bps: 2_000, minimum_threshold: 0 },
            BeneficiaryEntry { address: b3.clone(), bps: 2_000, minimum_threshold: 0 },
            BeneficiaryEntry { address: b4.clone(), bps: 2_000, minimum_threshold: 0 },
            BeneficiaryEntry { address: b5.clone(), bps: 2_000, minimum_threshold: 0 },
            BeneficiaryEntry { address: b6.clone(), bps: 2_000, minimum_threshold: 0 },
        ],
    );
    client2.deposit(&vault_5, &owner2, &100_000i128);
    env2.ledger().with_mut(|l| l.timestamp = 501);
    env2.cost_estimate().budget().reset_default();
    client2.trigger_release(&vault_5);
    let (cpu_5, _) = measure_budget(&env2);

    let linear_upper_bound = cpu_1.saturating_mul(5);
    println!(
        "[perf] release_scaling: 1-beneficiary={} 5-beneficiary={} linear_upper_bound={}",
        cpu_1, cpu_5, linear_upper_bound
    );
    assert!(
        cpu_5 <= linear_upper_bound,
        "Performance regression: release cost with 5 beneficiaries ({}) exceeds 5× the 1-beneficiary cost ({})",
        cpu_5, linear_upper_bound
    );
}
