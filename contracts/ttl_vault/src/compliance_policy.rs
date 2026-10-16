/// Issue #556 — Compliance policy versioning
///
/// The compliance configuration (KYC high-value threshold, allowlist mode and
/// reporting thresholds — see `compliance`) used to be overwritten in place,
/// leaving no record of which rules applied at a given time. This module
/// keeps an append-only history of policy versions:
///
/// - Every admin change through the existing compliance setters records a new
///   version, effective immediately (`record_live_change`).
/// - The admin can also publish a complete policy with a future
///   **effective date** (`publish_policy`). It becomes the active policy once
///   its date is reached; `sync_active_policy` (callable by anyone) applies it
///   to the live configuration.
/// - `get_policy_version(timestamp)` answers "which policy was in force at
///   this time?" for audit purposes: the version with the greatest
///   `effective_from <= timestamp`, ties broken by the higher version number.
use soroban_sdk::{contracttype, symbol_short, Address, Env, IntoVal, Symbol, Val, Vec};

use crate::compliance::{self, ThresholdConfig};
use crate::ContractError;

pub const POLICY_VERSIONED_TOPIC: Symbol = symbol_short!("pol_ver");
pub const POLICY_APPLIED_TOPIC: Symbol = symbol_short!("pol_app");

/// Reason recorded when a version comes from a direct setter call.
pub const REASON_KYC_THRESHOLD: Symbol = symbol_short!("kyc_thr");
pub const REASON_ALLOWLIST_MODE: Symbol = symbol_short!("al_mode");
pub const REASON_REPORT_THRESHOLDS: Symbol = symbol_short!("rpt_thr");

#[contracttype]
#[derive(Clone)]
pub enum PolicyKey {
    /// Compact index of (effective_from, version) pairs, in version order.
    Index,
    /// Full policy by version number (1-based).
    Version(u32),
    /// Version currently applied to the live compliance configuration.
    Applied,
}

/// A versioned snapshot of the compliance rules.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Policy {
    pub version: u32,
    /// Timestamp from which this policy is in force.
    pub effective_from: u64,
    /// Timestamp at which this version was recorded.
    pub created_at: u64,
    pub created_by: Address,
    /// Short tag describing the change (e.g. `kyc_thr`, `publish`).
    pub reason: Symbol,
    /// Amount at or above which operations require KYC (0 = disabled).
    pub kyc_high_value_threshold: i128,
    pub allowlist_enforced: bool,
    pub reporting_thresholds: Option<ThresholdConfig>,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PolicyIndexEntry {
    pub version: u32,
    pub effective_from: u64,
}

fn persist<V: IntoVal<Env, Val>>(env: &Env, key: &PolicyKey, value: &V) {
    env.storage().persistent().set(key, value);
    env.storage().persistent().extend_ttl(
        key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );
}

fn load_index(env: &Env) -> Vec<PolicyIndexEntry> {
    env.storage()
        .persistent()
        .get(&PolicyKey::Index)
        .unwrap_or_else(|| Vec::new(env))
}

pub fn get_version_count(env: &Env) -> u32 {
    load_index(env).len()
}

pub fn get_policy(env: &Env, version: u32) -> Option<Policy> {
    env.storage().persistent().get(&PolicyKey::Version(version))
}

pub fn get_applied_version(env: &Env) -> u32 {
    env.storage()
        .persistent()
        .get(&PolicyKey::Applied)
        .unwrap_or(0)
}

fn append(env: &Env, mut policy: Policy) -> u32 {
    let mut index = load_index(env);
    let version = index.len() + 1;
    policy.version = version;
    index.push_back(PolicyIndexEntry {
        version,
        effective_from: policy.effective_from,
    });
    persist(env, &PolicyKey::Version(version), &policy);
    persist(env, &PolicyKey::Index, &index);
    env.events().publish(
        (POLICY_VERSIONED_TOPIC, version),
        (policy.effective_from, policy.reason),
    );
    version
}

/// Records the live compliance configuration as a new version effective now.
/// Called after every direct change through the compliance setters.
pub fn record_live_change(env: &Env, actor: &Address, reason: Symbol) -> u32 {
    let now = env.ledger().timestamp();
    let version = append(
        env,
        Policy {
            version: 0,
            effective_from: now,
            created_at: now,
            created_by: actor.clone(),
            reason,
            kyc_high_value_threshold: compliance::get_kyc_high_value_threshold(env),
            allowlist_enforced: compliance::is_allowlist_enforced(env),
            reporting_thresholds: compliance::get_threshold_config(env),
        },
    );
    persist(env, &PolicyKey::Applied, &version);
    version
}

/// Publishes a complete policy effective from `effective_from` (which must
/// not be in the past). A policy effective now is applied immediately.
pub fn publish_policy(
    env: &Env,
    actor: &Address,
    kyc_high_value_threshold: i128,
    allowlist_enforced: bool,
    reporting_thresholds: Option<ThresholdConfig>,
    effective_from: u64,
) -> Result<u32, ContractError> {
    let now = env.ledger().timestamp();
    if effective_from < now {
        return Err(ContractError::InvalidEffectiveDate);
    }
    // Validate up front with the same rules as the live setters so an invalid
    // scheduled policy is rejected now rather than failing when applied.
    if kyc_high_value_threshold < 0 {
        return Err(ContractError::InvalidAmount);
    }
    if let Some(cfg) = &reporting_thresholds {
        if cfg.single_tx_threshold < 0
            || cfg.cumulative_threshold < 0
            || (cfg.cumulative_threshold > 0 && cfg.window_seconds == 0)
        {
            return Err(ContractError::InvalidConfig);
        }
    }

    let version = append(
        env,
        Policy {
            version: 0,
            effective_from,
            created_at: now,
            created_by: actor.clone(),
            reason: symbol_short!("publish"),
            kyc_high_value_threshold,
            allowlist_enforced,
            reporting_thresholds,
        },
    );
    sync_active_policy(env)?;
    Ok(version)
}

/// Returns the policy that was in force at `timestamp`.
pub fn get_policy_version(env: &Env, timestamp: u64) -> Result<Policy, ContractError> {
    let index = load_index(env);
    let mut best: Option<PolicyIndexEntry> = None;
    for entry in index.iter() {
        if entry.effective_from > timestamp {
            continue;
        }
        // Later versions win ties because the index is in version order.
        if best
            .as_ref()
            .is_none_or(|b| entry.effective_from >= b.effective_from)
        {
            best = Some(entry);
        }
    }
    let best = best.ok_or(ContractError::PolicyVersionNotFound)?;
    get_policy(env, best.version).ok_or(ContractError::PolicyVersionNotFound)
}

/// Applies the policy in force now to the live compliance configuration if it
/// is not already applied. Returns the applied version, or `None` when there
/// was nothing to do.
pub fn sync_active_policy(env: &Env) -> Result<Option<u32>, ContractError> {
    let active = match get_policy_version(env, env.ledger().timestamp()) {
        Ok(policy) => policy,
        Err(ContractError::PolicyVersionNotFound) => return Ok(None),
        Err(e) => return Err(e),
    };
    if active.version == get_applied_version(env) {
        return Ok(None);
    }

    compliance::set_kyc_high_value_threshold(env, active.kyc_high_value_threshold)?;
    if compliance::is_allowlist_enforced(env) != active.allowlist_enforced {
        compliance::set_allowlist_enforced(env, active.allowlist_enforced);
    }
    match &active.reporting_thresholds {
        Some(cfg) => compliance::set_threshold_config(env, cfg)?,
        None => compliance::clear_threshold_config(env),
    }

    persist(env, &PolicyKey::Applied, &active.version);
    env.events().publish(
        (POLICY_APPLIED_TOPIC, active.version),
        active.effective_from,
    );
    Ok(Some(active.version))
}
