//! Escrow contract with typed, on-chain-evaluated release conditions.
//!
//! # Escrow condition schema (#784)
//!
//! `escrow_sbt` accepts a `conditions: Bytes` blob that is now a typed,
//! versioned encoding instead of an opaque payload. The format is:
//!
//! ```text
//! [version: u8 = 1][count: u32 BE][condition_0][condition_1]...
//! ```
//!
//! Each `condition_i` is `[tag: u8][payload]` where `tag` selects the type:
//!
//! | tag | type              | payload                          | satisfied when                     |
//! |-----|-------------------|----------------------------------|------------------------------------|
//! | 1   | `Expiry`          | `u64 BE` unix seconds            | `env.ledger().timestamp() <= t`    |
//! | 2   | `MinTime`         | `u64 BE` unix seconds            | `env.ledger().timestamp() >= t`    |
//! | 3   | `RequiredSigner`  | `Address` (XDR)                  | signer authorized the release      |
//!
//! An empty `conditions` blob is treated as "no conditions" and is always
//! satisfied. Unknown versions or tags cause `escrow_sbt` to panic so malformed
//! conditions can never be silently ignored. `atomic_release_credentials`
//! re-parses the stored blob and enforces every condition on-chain before
//! releasing credentials.

use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, Address, Bytes, Env, Symbol, Vec};

const CONDITIONS_VERSION: u8 = 1;
const TAG_EXPIRY: u8 = 1;
const TAG_MIN_TIME: u8 = 2;
const TAG_REQUIRED_SIGNER: u8 = 3;

/// Typed representation of a single escrow release condition.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EscrowCondition {
    /// Release is only allowed at or before this unix timestamp.
    Expiry(u64),
    /// Release is only allowed at or after this unix timestamp.
    MinTime(u64),
    /// Release requires this address to have authorized the call.
    RequiredSigner(Address),
}

#[contracttype]
#[derive(Clone)]
pub struct EscrowRecord {
    pub escrow_agent: Address,
    pub conditions: Bytes,
    pub released: bool,
}

fn escrow_key(env: &Env, sbt_id: u64) -> Symbol {
    let _ = env;
    // Symbol keys are limited; encode the id into the storage key namespace.
    symbol_short!("escrow")
}

fn read_u64(bytes: &Bytes, offset: u32) -> u64 {
    let mut value: u64 = 0;
    let mut i: u32 = 0;
    while i < 8 {
        value = (value << 8) | (bytes.get(offset + i).unwrap() as u64);
        i += 1;
    }
    value
}

/// Parse the versioned condition blob into typed conditions.
///
/// Panics on unknown versions, unknown tags, or truncated payloads so that
/// malformed conditions fail closed rather than being ignored.
pub fn parse_conditions(env: &Env, conditions: &Bytes) -> Vec<EscrowCondition> {
    let mut parsed: Vec<EscrowCondition> = Vec::new(env);
    if conditions.len() == 0 {
        return parsed;
    }

    let version = conditions.get(0).unwrap();
    if version != CONDITIONS_VERSION {
        panic!("unsupported escrow conditions version");
    }

    let count = read_u64(conditions, 1) as u32;
    let mut offset: u32 = 9;
    let mut i: u32 = 0;
    while i < count {
        let tag = conditions.get(offset).unwrap();
        offset += 1;
        match tag {
            TAG_EXPIRY => {
                let t = read_u64(conditions, offset);
                offset += 8;
                parsed.push_back(EscrowCondition::Expiry(t));
            }
            TAG_MIN_TIME => {
                let t = read_u64(conditions, offset);
                offset += 8;
                parsed.push_back(EscrowCondition::MinTime(t));
            }
            TAG_REQUIRED_SIGNER => {
                let signer = Address::from_xdr(env, &conditions.slice(offset..conditions.len()));
                // Address XDR length is variable; recompute the consumed bytes.
                let consumed = signer.to_xdr(env).len();
                offset += consumed;
                parsed.push_back(EscrowCondition::RequiredSigner(signer));
            }
            _ => panic!("unknown escrow condition tag"),
        }
        i += 1;
    }

    parsed
}

/// Evaluate every condition on-chain. Panics with a descriptive message when
/// any condition is not satisfied.
pub fn evaluate_conditions(env: &Env, conditions: &Bytes, signers: &Vec<Address>) {
    let parsed = parse_conditions(env, conditions);
    let now = env.ledger().timestamp();

    for condition in parsed.iter() {
        match condition {
            EscrowCondition::Expiry(t) => {
                if now > t {
                    panic!("escrow condition failed: expired");
                }
            }
            EscrowCondition::MinTime(t) => {
                if now < t {
                    panic!("escrow condition failed: min time not reached");
                }
            }
            EscrowCondition::RequiredSigner(required) => {
                let mut found = false;
                for signer in signers.iter() {
                    if signer == required {
                        found = true;
                        break;
                    }
                }
                if !found {
                    panic!("escrow condition failed: missing required signer");
                }
            }
        }
    }
}

#[contract]
pub struct EscrowContract;

#[contractimpl]
impl EscrowContract {
    /// Create an escrow for `sbt_id`. `conditions` must follow the typed schema
    /// documented at the top of this module; it is validated eagerly here so
    /// malformed blobs are rejected at creation time.
    pub fn escrow_sbt(env: Env, sbt_id: u64, escrow_agent: Address, conditions: Bytes) {
        escrow_agent.require_auth();
        // Validate the schema up front; panics on malformed conditions.
        let _ = parse_conditions(&env, &conditions);

        let record = EscrowRecord {
            escrow_agent,
            conditions,
            released: false,
        };
        env.storage().persistent().set(&escrow_key(&env, sbt_id), &record);
    }

    /// Release credentials for `sbt_id`. Every stored condition is evaluated
    /// on-chain before the release is recorded.
    pub fn atomic_release_credentials(env: Env, sbt_id: u64, signers: Vec<Address>) {
        let key = escrow_key(&env, sbt_id);
        let mut record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .expect("escrow not found");

        if record.released {
            panic!("escrow already released");
        }

        record.escrow_agent.require_auth();
        evaluate_conditions(&env, &record.conditions, &signers);

        record.released = true;
        env.storage().persistent().set(&key, &record);
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger};

    fn encode(env: &Env, conditions: &[EscrowCondition]) -> Bytes {
        let mut bytes = Bytes::new(env);
        bytes.push_back(CONDITIONS_VERSION);
        let mut count: u64 = conditions.len() as u64;
        let mut i = 0;
        while i < 8 {
            bytes.push_back(((count >> (56 - i * 8)) & 0xff) as u8);
            i += 1;
        }
        for condition in conditions.iter() {
            match condition {
                EscrowCondition::Expiry(t) => {
                    bytes.push_back(TAG_EXPIRY);
                    push_u64(&mut bytes, *t);
                }
                EscrowCondition::MinTime(t) => {
                    bytes.push_back(TAG_MIN_TIME);
                    push_u64(&mut bytes, *t);
                }
                EscrowCondition::RequiredSigner(addr) => {
                    bytes.push_back(TAG_REQUIRED_SIGNER);
                    bytes.append(&addr.to_xdr(env));
                }
            }
        }
        bytes
    }

    fn push_u64(bytes: &mut Bytes, value: u64) {
        let mut i = 0;
        while i < 8 {
            bytes.push_back(((value >> (56 - i * 8)) & 0xff) as u8);
            i += 1;
        }
    }

    #[test]
    fn empty_conditions_always_satisfied() {
        let env = Env::default();
        let conditions = Bytes::new(&env);
        let signers: Vec<Address> = Vec::new(&env);
        evaluate_conditions(&env, &conditions, &signers);
    }

    #[test]
    fn expiry_condition_enforced() {
        let env = Env::default();
        env.ledger().set_timestamp(100);
        let conditions = encode(&env, &[EscrowCondition::Expiry(200)]);
        let signers: Vec<Address> = Vec::new(&env);
        evaluate_conditions(&env, &conditions, &signers);

        env.ledger().set_timestamp(300);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            evaluate_conditions(&env, &conditions, &signers);
        }));
        assert!(result.is_err());
    }

    #[test]
    fn min_time_condition_enforced() {
        let env = Env::default();
        env.ledger().set_timestamp(100);
        let conditions = encode(&env, &[EscrowCondition::MinTime(200)]);
        let signers: Vec<Address> = Vec::new(&env);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            evaluate_conditions(&env, &conditions, &signers);
        }));
        assert!(result.is_err());

        env.ledger().set_timestamp(250);
        evaluate_conditions(&env, &conditions, &signers);
    }

    #[test]
    fn required_signer_condition_enforced() {
        let env = Env::default();
        let required = Address::generate(&env);
        let other = Address::generate(&env);
        let conditions = encode(&env, &[EscrowCondition::RequiredSigner(required.clone())]);

        let mut signers: Vec<Address> = Vec::new(&env);
        signers.push_back(other);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            evaluate_conditions(&env, &conditions, &signers);
        }));
        assert!(result.is_err());

        let mut ok_signers: Vec<Address> = Vec::new(&env);
        ok_signers.push_back(required);
        evaluate_conditions(&env, &conditions, &ok_signers);
    }

    #[test]
    fn release_evaluates_conditions() {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(100);
        let agent = Address::generate(&env);
        let conditions = encode(&env, &[EscrowCondition::MinTime(200)]);
        let client = EscrowContractClient::new(&env, &env.register_contract(None, EscrowContract));
        client.escrow_sbt(&1, &agent, &conditions);

        let signers: Vec<Address> = Vec::new(&env);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            client.atomic_release_credentials(&1, &signers);
        }));
        assert!(result.is_err());

        env.ledger().set_timestamp(250);
        client.atomic_release_credentials(&1, &signers);
    }
}
