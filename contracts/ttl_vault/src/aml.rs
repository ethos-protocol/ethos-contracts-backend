//! Issue #547: Anti-Money Laundering (AML) screening.
//!
//! Beneficiary addresses are screened against sanctions data before they are
//! registered on a vault and again immediately before any token transfer to
//! them. Two sources are consulted:
//!
//! 1. **Local flag list** — addresses flagged by the admin or by the
//!    configured AML reporter (the off-chain backend, which screens addresses
//!    against an AML provider such as Chainalysis and mirrors hits on-chain).
//! 2. **Sanctions oracle** — an optional on-chain oracle contract exposing
//!    `is_sanctioned(addr: Address) -> bool`, matching the interface of
//!    Chainalysis' on-chain sanctions list.
//!
//! Oracle reads fail closed: if an oracle is configured but the call fails,
//! the address is treated as non-compliant so a broken or malicious oracle
//! can never silently let funds through. The admin can clear the oracle to
//! restore releases while the integration is repaired.

use soroban_sdk::{
    contracttype, panic_with_error, symbol_short, Address, Env, IntoVal, String, Symbol, Val, Vec,
};

use crate::ContractError;

pub const AML_ORACLE_SET_TOPIC: Symbol = symbol_short!("aml_orcl");
pub const AML_REPORTER_SET_TOPIC: Symbol = symbol_short!("aml_rptr");
pub const AML_FLAGGED_TOPIC: Symbol = symbol_short!("aml_flag");
pub const AML_UNFLAGGED_TOPIC: Symbol = symbol_short!("aml_unflg");
pub const AML_BLOCKED_TOPIC: Symbol = symbol_short!("aml_block");

#[contracttype]
#[derive(Clone)]
pub enum AmlKey {
    /// Optional sanctions oracle contract address.
    Oracle,
    /// Optional address (besides the admin) allowed to flag/unflag addresses.
    Reporter,
    /// Flag record for a single address.
    Flag(Address),
}

/// Why and by whom an address was flagged.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AmlFlag {
    pub address: Address,
    pub reason: String,
    pub flagged_by: Address,
    pub flagged_at: u64,
}

/// Emitted when a transfer or beneficiary registration is rejected.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AmlBlockedEvent {
    pub address: Address,
    pub timestamp: u64,
}

fn require_admin(env: &Env, caller: &Address) {
    caller.require_auth();
    let admin: Address = env
        .storage()
        .instance()
        .get(&crate::DataKey::Admin)
        .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));
    if *caller != admin {
        panic_with_error!(env, ContractError::NotAdmin);
    }
}

fn require_admin_or_reporter(env: &Env, caller: &Address) {
    caller.require_auth();
    let admin: Address = env
        .storage()
        .instance()
        .get(&crate::DataKey::Admin)
        .unwrap_or_else(|| panic_with_error!(env, ContractError::NotInitialized));
    if *caller == admin {
        return;
    }
    let reporter: Option<Address> = env.storage().instance().get(&AmlKey::Reporter);
    if reporter.as_ref() != Some(caller) {
        panic_with_error!(env, ContractError::NotAdmin);
    }
}

pub fn set_oracle(env: &Env, admin: &Address, oracle: Option<Address>) {
    require_admin(env, admin);
    match &oracle {
        Some(addr) => env.storage().instance().set(&AmlKey::Oracle, addr),
        None => env.storage().instance().remove(&AmlKey::Oracle),
    }
    env.events().publish((AML_ORACLE_SET_TOPIC,), oracle);
}

pub fn get_oracle(env: &Env) -> Option<Address> {
    env.storage().instance().get(&AmlKey::Oracle)
}

pub fn set_reporter(env: &Env, admin: &Address, reporter: Option<Address>) {
    require_admin(env, admin);
    match &reporter {
        Some(addr) => env.storage().instance().set(&AmlKey::Reporter, addr),
        None => env.storage().instance().remove(&AmlKey::Reporter),
    }
    env.events().publish((AML_REPORTER_SET_TOPIC,), reporter);
}

pub fn get_reporter(env: &Env) -> Option<Address> {
    env.storage().instance().get(&AmlKey::Reporter)
}

pub fn flag_address(env: &Env, caller: &Address, address: Address, reason: String) {
    require_admin_or_reporter(env, caller);
    let flag = AmlFlag {
        address: address.clone(),
        reason,
        flagged_by: caller.clone(),
        flagged_at: env.ledger().timestamp(),
    };
    env.storage()
        .persistent()
        .set(&AmlKey::Flag(address.clone()), &flag);
    env.events().publish((AML_FLAGGED_TOPIC, address), flag);
}

pub fn unflag_address(env: &Env, caller: &Address, address: Address) {
    require_admin_or_reporter(env, caller);
    env.storage()
        .persistent()
        .remove(&AmlKey::Flag(address.clone()));
    env.events()
        .publish((AML_UNFLAGGED_TOPIC, address), caller.clone());
}

pub fn get_flag(env: &Env, address: &Address) -> Option<AmlFlag> {
    env.storage()
        .persistent()
        .get(&AmlKey::Flag(address.clone()))
}

/// Queries the sanctions oracle. Returns `true` when the address is
/// sanctioned **or** the oracle call fails (fail closed).
fn oracle_flags(env: &Env, oracle: &Address, address: &Address) -> bool {
    let func = Symbol::new(env, "is_sanctioned");
    let mut args: Vec<Val> = Vec::new(env);
    args.push_back(address.into_val(env));
    match env.try_invoke_contract::<bool, soroban_sdk::Error>(oracle, &func, args) {
        Ok(Ok(sanctioned)) => sanctioned,
        _ => true,
    }
}

/// Returns `true` when `address` is clear to receive funds.
pub fn check_compliance(env: &Env, address: &Address) -> bool {
    if env
        .storage()
        .persistent()
        .has(&AmlKey::Flag(address.clone()))
    {
        return false;
    }
    if let Some(oracle) = get_oracle(env) {
        if oracle_flags(env, &oracle, address) {
            return false;
        }
    }
    true
}

/// Panics with [`ContractError::AmlFlaggedAddress`] when `address` fails
/// AML screening. Used to gate beneficiary registration and every outgoing
/// transfer to a beneficiary.
pub fn require_compliant(env: &Env, address: &Address) {
    if !check_compliance(env, address) {
        // Note: the panic below rolls back this event along with the rest of
        // the invocation; it is published for simulation/preflight visibility.
        env.events().publish(
            (AML_BLOCKED_TOPIC, address.clone()),
            AmlBlockedEvent {
                address: address.clone(),
                timestamp: env.ledger().timestamp(),
            },
        );
        panic_with_error!(env, ContractError::AmlFlaggedAddress);
    }
}
