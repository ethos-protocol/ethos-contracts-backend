//! Timestamp edge-case tests for vesting schedules.
//!
//! All ledger timestamps used by the contract are UNIX seconds (u64). These
//! tests confirm that:
//!
//!  - Installments unlock at *exact* boundary timestamps (off-by-one safety).
//!  - Cliff enforcement works when `now == start_time + cliff_period` exactly.
//!  - Schedules that span a leap-year boundary (366-day year) behave correctly:
//!    the interval is pure seconds arithmetic and does not assume a fixed
//!    year length.
//!  - No wall-clock or calendar assumptions are baked in.
//!
//! UNIX epoch references used in these tests:
//!
//! | Date             | UNIX timestamp  |
//! |------------------|-----------------|
//! | 1970-01-01 00:00 |               0 |
//! | 1972-01-01 00:00 |    63_072_000   |  (first Gregorian leap year after epoch)
//! | 1972-12-31 23:59 |    94_607_940   |
//! | 2000-01-01 00:00 |   946_684_800   |  (Y2K / also a leap year)
//! | 2000-02-29 00:00 |   951_696_000   |  (leap day in year 2000)
//! | 2000-03-01 00:00 |   951_782_400   |  (day after leap day)
//! | 2004-01-01 00:00 | 1_072_915_200   |
//!
//! Year lengths in seconds:
//!   Regular year : 365 * 86_400 = 31_536_000 s
//!   Leap year    : 366 * 86_400 = 31_622_400 s

#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, Env,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Seconds in a regular (non-leap) year.
const REGULAR_YEAR_SECS: u64 = 31_536_000;
/// Seconds in a leap year.
const LEAP_YEAR_SECS: u64 = 31_622_400;
/// Seconds in one day.
const ONE_DAY_SECS: u64 = 86_400;

/// UNIX timestamp for 2000-01-01 00:00:00 UTC.
const UNIX_Y2K: u64 = 946_684_800;
/// UNIX timestamp for 2000-02-29 00:00:00 UTC (leap day in year 2000).
const UNIX_2000_FEB_29: u64 = 951_696_000;
/// UNIX timestamp for 2000-03-01 00:00:00 UTC (day after leap day).
const UNIX_2000_MAR_01: u64 = 951_782_400;
/// UNIX timestamp for 2004-01-01 00:00:00 UTC (next leap year start after 2000).
const UNIX_2004_JAN_01: u64 = 1_072_915_200;

fn setup_vesting_env() -> (Env, Address, Address, u64, TtlVaultContractClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let owner = Address::generate(&env);
    let beneficiary = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let token_address = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    // Mint a large supply to owner for deposit operations.
    StellarAssetClient::new(&env, &token_address).mint(&owner, &2_000_000_000i128);

    let contract_address = env.register_contract(None, TtlVaultContract);
    let client = TtlVaultContractClient::new(&env, &contract_address);
    client.initialize(&token_address, &admin);

    let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };

    let vault_id = client.create_vault(&owner, &beneficiary, &100u64, &None);
    client.deposit(&vault_id, &owner, &1_200_000_000i128);

    (env, owner, beneficiary, vault_id, client)
}

// ---------------------------------------------------------------------------
// Section 1 — Exact boundary: installment unlocks AT start_time (no off-by-one)
// ---------------------------------------------------------------------------

/// An installment whose `start_time` equals the current ledger timestamp must
/// be immediately claimable (elapsed = 0, installments_available = 1).
#[test]
fn test_installment_claimable_at_exact_start_timestamp() {
    let (env, owner, beneficiary, vault_id, client) = setup_vesting_env();

    let start = 10_000_000u64;
    env.ledger().set_timestamp(start);

    // Schedule: 4 installments, 1-day interval, no cliff.
    client
        .set_beneficiary_vesting(&vault_id, &owner, &beneficiary, &start, &ONE_DAY_SECS, &4u32, &0u64)
        .unwrap();

    // Expire vault and release.
    env.ledger().with_mut(|l| l.timestamp += 200);
    client.trigger_release(&vault_id);

    // Wind clock back to exactly start_time.
    env.ledger().set_timestamp(start);
    let claimed = client.claim_beneficiary_vesting(&vault_id, &beneficiary);

    assert!(
        claimed > 0,
        "installment at exact start_time must be claimable; got {}",
        claimed
    );
}

/// At `start_time - 1` the vault is unreleased, but let's confirm that just
/// before start_time no installment is available.
#[test]
fn test_nothing_claimable_one_second_before_start() {
    let (env, owner, beneficiary, vault_id, client) = setup_vesting_env();

    let start = 20_000_000u64;
    env.ledger().set_timestamp(start);

    client
        .set_beneficiary_vesting(
            &vault_id,
            &owner,
            &beneficiary,
            &(start + 100), // start is 100 s in the future
            &ONE_DAY_SECS,
            &4u32,
            &0u64,
        )
        .unwrap();

    // Expire & release at start + 200 (past the check_in_interval of 100).
    env.ledger().with_mut(|l| l.timestamp += 200);
    client.trigger_release(&vault_id);

    // Now move to exactly (start + 100 - 1) — one second before the schedule start.
    env.ledger().set_timestamp(start + 100 - 1);
    let result = client.try_claim_beneficiary_vesting(&vault_id, &beneficiary);

    // The contract should return an error (CliffNotReached or NothingToClaimYet).
    assert!(
        result.is_err(),
        "no installment must be claimable one second before start_time"
    );
}

// ---------------------------------------------------------------------------
// Section 2 — Exact cliff boundary
// ---------------------------------------------------------------------------

/// When `now == start_time + cliff_period` exactly, the cliff is just reached
/// and the first installment must be claimable.
#[test]
fn test_claim_succeeds_at_exact_cliff_boundary() {
    let (env, owner, beneficiary, vault_id, client) = setup_vesting_env();

    let start = 1_000_000u64;
    let cliff  = REGULAR_YEAR_SECS; // 1-year cliff

    env.ledger().set_timestamp(start);

    client
        .set_beneficiary_vesting(
            &vault_id,
            &owner,
            &beneficiary,
            &start,
            &ONE_DAY_SECS,
            &365u32, // 365 daily installments
            &cliff,
        )
        .unwrap();

    // Expire vault (check_in_interval is 100 s from setup).
    env.ledger().with_mut(|l| l.timestamp += 200);
    client.trigger_release(&vault_id);

    // Move to exactly start + cliff (the boundary second).
    let cliff_boundary = start + cliff;
    env.ledger().set_timestamp(cliff_boundary);

    let result = client.try_claim_beneficiary_vesting(&vault_id, &beneficiary);
    assert!(
        result.is_ok(),
        "claim at exact cliff boundary (now == start + cliff_period) must succeed; err: {:?}",
        result.err()
    );
}

/// One second before the cliff boundary must still fail with CliffNotReached.
#[test]
fn test_claim_fails_one_second_before_cliff_boundary() {
    let (env, owner, beneficiary, vault_id, client) = setup_vesting_env();

    let start = 2_000_000u64;
    let cliff  = REGULAR_YEAR_SECS;

    env.ledger().set_timestamp(start);

    client
        .set_beneficiary_vesting(
            &vault_id,
            &owner,
            &beneficiary,
            &start,
            &ONE_DAY_SECS,
            &365u32,
            &cliff,
        )
        .unwrap();

    env.ledger().with_mut(|l| l.timestamp += 200);
    client.trigger_release(&vault_id);

    // One second before the cliff boundary.
    env.ledger().set_timestamp(start + cliff - 1);

    let result = client.try_claim_beneficiary_vesting(&vault_id, &beneficiary);
    assert!(
        result.is_err(),
        "claim one second before cliff boundary must fail"
    );
    let err = result.unwrap_err().unwrap();
    assert_eq!(
        err,
        soroban_sdk::Error::from_contract_error(ContractError::CliffNotReached as u32),
        "error must be CliffNotReached"
    );
}

// ---------------------------------------------------------------------------
// Section 3 — Exact installment boundary (interval off-by-one safety)
// ---------------------------------------------------------------------------

/// Installment N must be claimable at exactly `start_time + N * interval`.
#[test]
fn test_installment_claimable_at_exact_interval_boundary() {
    let (env, owner, beneficiary, vault_id, client) = setup_vesting_env();

    let start    = 5_000_000u64;
    let interval = ONE_DAY_SECS; // 1 day
    let n = 3u32; // test the third installment boundary

    env.ledger().set_timestamp(start);

    client
        .set_beneficiary_vesting(
            &vault_id,
            &owner,
            &beneficiary,
            &start,
            &interval,
            &6u32, // 6 total installments
            &0u64, // no cliff
        )
        .unwrap();

    env.ledger().with_mut(|l| l.timestamp += 200);
    client.trigger_release(&vault_id);

    // Claim installments 1 and 2 first.
    env.ledger().set_timestamp(start);
    client.claim_beneficiary_vesting(&vault_id, &beneficiary); // installment 1

    env.ledger().set_timestamp(start + interval);
    client.claim_beneficiary_vesting(&vault_id, &beneficiary); // installment 2

    // Now set time to exactly start + n * interval (the boundary for installment n).
    env.ledger().set_timestamp(start + (n as u64) * interval);
    let result = client.try_claim_beneficiary_vesting(&vault_id, &beneficiary);
    assert!(
        result.is_ok(),
        "installment {} must be claimable at exact boundary timestamp start + {} * interval",
        n,
        n
    );
}

/// One second *before* an installment boundary means that installment is not
/// yet available (previous already claimed, next not unlocked yet).
#[test]
fn test_no_installment_one_second_before_interval_boundary() {
    let (env, owner, beneficiary, vault_id, client) = setup_vesting_env();

    let start    = 6_000_000u64;
    let interval = ONE_DAY_SECS;

    env.ledger().set_timestamp(start);

    client
        .set_beneficiary_vesting(
            &vault_id,
            &owner,
            &beneficiary,
            &start,
            &interval,
            &4u32,
            &0u64,
        )
        .unwrap();

    env.ledger().with_mut(|l| l.timestamp += 200);
    client.trigger_release(&vault_id);

    // Claim installment 1 at start.
    env.ledger().set_timestamp(start);
    client.claim_beneficiary_vesting(&vault_id, &beneficiary);

    // Position one second before the second installment boundary.
    env.ledger().set_timestamp(start + interval - 1);
    let result = client.try_claim_beneficiary_vesting(&vault_id, &beneficiary);
    assert!(
        result.is_err(),
        "no installment must be available one second before the next interval boundary"
    );
}

// ---------------------------------------------------------------------------
// Section 4 — Leap-year boundary crossing
//
// Year 2000 is a Gregorian leap year (divisible by 400). A 1-year vesting
// interval expressed in *regular-year seconds* (31_536_000 s) must NOT skip
// or double-count an installment when the schedule spans Feb 29.
//
// Key property: the contract uses pure UNIX-second arithmetic, so leap days
// are transparent — there is no special handling needed and none is expected.
// These tests confirm that assumption is upheld.
// ---------------------------------------------------------------------------

/// A schedule whose `start_time` is 2000-01-01 and whose interval is one
/// regular year (31_536_000 s) crosses the Feb-29 leap day.
/// The installment must be claimable at start + interval regardless of the
/// calendar interpretation.
#[test]
fn test_vesting_interval_crossing_leap_year_2000() {
    let (env, owner, beneficiary, vault_id, client) = setup_vesting_env();

    // start = 2000-01-01 00:00:00 UTC
    let start    = UNIX_Y2K;
    let interval = REGULAR_YEAR_SECS; // plain seconds; crosses leap day Feb 29 2000

    env.ledger().set_timestamp(start);

    // 2 annual installments; no cliff.
    client
        .set_beneficiary_vesting(
            &vault_id,
            &owner,
            &beneficiary,
            &start,
            &interval,
            &2u32,
            &0u64,
        )
        .unwrap();

    env.ledger().with_mut(|l| l.timestamp += 200);
    client.trigger_release(&vault_id);

    // First installment: at start (elapsed = 0 → 1 installment unlocked).
    env.ledger().set_timestamp(start);
    let first = client.claim_beneficiary_vesting(&vault_id, &beneficiary);
    assert!(first > 0, "first installment must be > 0; got {}", first);

    // Second installment: at start + 1 regular year.
    // This point in time is 2001-01-01 (365 days after 2000-01-01),
    // having crossed the leap day. Pure-second arithmetic means it lands
    // at a valid UNIX timestamp regardless.
    let second_unlock = start + interval;
    env.ledger().set_timestamp(second_unlock);
    let second = client.claim_beneficiary_vesting(&vault_id, &beneficiary);
    assert!(second > 0, "second installment must be > 0; got {}", second);

    // All installments consumed — further claims must fail.
    env.ledger().with_mut(|l| l.timestamp += interval);
    let result = client.try_claim_beneficiary_vesting(&vault_id, &beneficiary);
    assert!(
        result.is_err(),
        "no further installments must be available after all are claimed"
    );
}

/// A schedule using a leap-year-length interval (31_622_400 s) must behave
/// identically — the contract does not special-case it.
#[test]
fn test_vesting_interval_uses_leap_year_length() {
    let (env, owner, beneficiary, vault_id, client) = setup_vesting_env();

    let start    = UNIX_2000_FEB_29; // start on the leap day itself
    let interval = LEAP_YEAR_SECS;   // 366 days

    env.ledger().set_timestamp(start);

    client
        .set_beneficiary_vesting(
            &vault_id,
            &owner,
            &beneficiary,
            &start,
            &interval,
            &2u32,
            &0u64,
        )
        .unwrap();

    env.ledger().with_mut(|l| l.timestamp += 200);
    client.trigger_release(&vault_id);

    // Claim immediately at start.
    env.ledger().set_timestamp(start);
    let first = client.claim_beneficiary_vesting(&vault_id, &beneficiary);
    assert!(first > 0);

    // Claim at start + leap year.
    env.ledger().set_timestamp(start + interval);
    let second = client.claim_beneficiary_vesting(&vault_id, &beneficiary);
    assert!(second > 0);

    // Verify total = vault balance (rounding to last installment covered).
    assert_eq!(first + second, 1_200_000_000i128);
}

/// A cliff that spans a leap year: cliff_period = LEAP_YEAR_SECS.
/// Claim at start + cliff must succeed; one second before must fail.
#[test]
fn test_cliff_spanning_leap_year_boundary() {
    let (env, owner, beneficiary, vault_id, client) = setup_vesting_env();

    // start just before the leap day so the cliff crosses it.
    let start = UNIX_2000_FEB_29 - ONE_DAY_SECS; // 2000-02-28
    let cliff  = LEAP_YEAR_SECS;

    env.ledger().set_timestamp(start);

    client
        .set_beneficiary_vesting(
            &vault_id,
            &owner,
            &beneficiary,
            &start,
            &ONE_DAY_SECS,
            &10u32,
            &cliff,
        )
        .unwrap();

    env.ledger().with_mut(|l| l.timestamp += 200);
    client.trigger_release(&vault_id);

    // One second before cliff end.
    env.ledger().set_timestamp(start + cliff - 1);
    let before = client.try_claim_beneficiary_vesting(&vault_id, &beneficiary);
    assert!(before.is_err(), "must fail one second before cliff ends");

    // Exactly at cliff boundary.
    env.ledger().set_timestamp(start + cliff);
    let at_cliff = client.try_claim_beneficiary_vesting(&vault_id, &beneficiary);
    assert!(
        at_cliff.is_ok(),
        "claim at exact leap-year cliff boundary must succeed; err: {:?}",
        at_cliff.err()
    );
}

// ---------------------------------------------------------------------------
// Section 5 — Multi-year schedule spanning multiple leap years
// ---------------------------------------------------------------------------

/// A 4-installment schedule starting at Y2K with a 1-year interval spans
/// 2000, 2001, 2002, 2003 — crossing two leap years (2000 and 2004 is
/// outside the range, but 2000 itself is a leap year). Each installment
/// must become claimable at the expected second offset.
#[test]
fn test_multi_year_schedule_spanning_year_2000_leap() {
    let (env, owner, beneficiary, vault_id, client) = setup_vesting_env();

    let start    = UNIX_Y2K;         // 2000-01-01
    let interval = REGULAR_YEAR_SECS; // 365-day year in seconds
    let n        = 4u32;

    env.ledger().set_timestamp(start);

    client
        .set_beneficiary_vesting(
            &vault_id,
            &owner,
            &beneficiary,
            &start,
            &interval,
            &n,
            &0u64,
        )
        .unwrap();

    env.ledger().with_mut(|l| l.timestamp += 200);
    client.trigger_release(&vault_id);

    let per_installment = 1_200_000_000i128 / n as i128;
    let mut total_claimed = 0i128;

    for i in 0..n {
        let unlock_time = start + (i as u64) * interval;
        env.ledger().set_timestamp(unlock_time);

        let claimed = client.claim_beneficiary_vesting(&vault_id, &beneficiary);
        assert!(
            claimed > 0,
            "installment {} must be > 0 at UNIX timestamp {} (start + {} * interval)",
            i + 1,
            unlock_time,
            i
        );

        // All installments except the last must equal per_installment.
        // The last absorbs any integer-division remainder.
        if i < n - 1 {
            assert_eq!(
                claimed,
                per_installment,
                "installment {} must equal per_installment ({}) except for the last",
                i + 1,
                per_installment
            );
        }

        total_claimed += claimed;
    }

    assert_eq!(
        total_claimed, 1_200_000_000i128,
        "all installments must sum to the full vault balance"
    );

    // Further claims must be blocked.
    env.ledger().with_mut(|l| l.timestamp += interval);
    let extra = client.try_claim_beneficiary_vesting(&vault_id, &beneficiary);
    assert!(extra.is_err(), "no further claims allowed after schedule complete");
}

// ---------------------------------------------------------------------------
// Section 6 — Zero-cliff is truly non-restrictive
// ---------------------------------------------------------------------------

/// When cliff_period = 0, claims must be possible from start_time with no
/// additional barrier, including at the epoch origin (timestamp = 0).
#[test]
fn test_zero_cliff_claimable_from_epoch_zero() {
    let (env, owner, beneficiary, vault_id, client) = setup_vesting_env();

    let start = 0u64; // UNIX epoch

    // Keep ledger at 0 for both setup and initial claim.
    env.ledger().set_timestamp(0);

    client
        .set_beneficiary_vesting(
            &vault_id,
            &owner,
            &beneficiary,
            &start,
            &ONE_DAY_SECS,
            &4u32,
            &0u64, // no cliff
        )
        .unwrap();

    // Expire vault using check_in_interval (100 s from setup).
    env.ledger().with_mut(|l| l.timestamp += 200);
    client.trigger_release(&vault_id);

    // Claim at epoch zero.
    env.ledger().set_timestamp(start);
    let result = client.try_claim_beneficiary_vesting(&vault_id, &beneficiary);
    assert!(
        result.is_ok(),
        "zero-cliff schedule must be claimable at epoch zero; err: {:?}",
        result.err()
    );
}

// ---------------------------------------------------------------------------
// Section 7 — UNIX-second consistency: all arithmetic is in u64 seconds
//
// This test probes that no silent integer truncation or calendar conversion
// occurs for timestamps near u32::MAX (year ~2106 relative to epoch).
// ---------------------------------------------------------------------------

/// A start_time near u32::MAX seconds (≈ year 2106) must be handled without
/// overflow, since the schedule fields are u64.
#[test]
fn test_large_unix_timestamp_no_overflow() {
    let (env, owner, beneficiary, vault_id, client) = setup_vesting_env();

    // u32::MAX = 4_294_967_295 ≈ year 2106 in UNIX time.
    let start: u64 = u32::MAX as u64;
    let interval    = ONE_DAY_SECS;

    env.ledger().set_timestamp(start);

    let result = client.try_set_beneficiary_vesting(
        &vault_id,
        &owner,
        &beneficiary,
        &start,
        &interval,
        &2u32,
        &0u64,
    );

    // The contract should accept the schedule without overflow panics.
    assert!(
        result.is_ok(),
        "set_beneficiary_vesting must not overflow for timestamps near u32::MAX; err: {:?}",
        result.err()
    );

    // Expire vault and release.
    env.ledger().with_mut(|l| l.timestamp += 200);
    client.trigger_release(&vault_id);

    // Claim at the large timestamp.
    env.ledger().set_timestamp(start);
    let claim_result = client.try_claim_beneficiary_vesting(&vault_id, &beneficiary);
    assert!(
        claim_result.is_ok(),
        "claim at timestamp near u32::MAX must succeed; err: {:?}",
        claim_result.err()
    );
}
