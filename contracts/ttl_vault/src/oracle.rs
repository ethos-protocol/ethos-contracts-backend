// Minimal oracle module for external release condition queries
use soroban_sdk::{Address, Env, Symbol, Val, Vec};

/// Errors that can occur while reading and validating external oracle data.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum OracleError {
    /// The most recent oracle observation is older than the configured
    /// `max_oracle_staleness_seconds` and must not feed floors/caps or
    /// beneficiary calculations.
    StaleData,
    /// The oracle contract call failed (missing contract, panic, or the
    /// observation timestamp could not be read).
    QueryFailed,
}

/// Configuration governing how oracle reads are validated before use.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleConfig {
    /// Maximum age, in seconds, of an oracle observation before a read is
    /// rejected. `now - oracle_timestamp` greater than this yields
    /// [`OracleError::StaleData`].
    pub max_oracle_staleness_seconds: u64,
}

impl Default for OracleConfig {
    fn default() -> Self {
        // 15 minutes: long enough to tolerate an oracle's publish cadence,
        // short enough that floors/caps never act on badly outdated data.
        Self {
            max_oracle_staleness_seconds: 900,
        }
    }
}

/// Pure staleness predicate shared by [`query_checked`] and the unit tests.
///
/// Returns `Ok(())` when an observation of `age_seconds` is still fresh under
/// `max_staleness`, and [`OracleError::StaleData`] once it is strictly older.
/// An exact-boundary age (`age_seconds == max_staleness`) is still accepted.
pub fn check_staleness(age_seconds: u64, max_staleness: u64) -> Result<(), OracleError> {
    if age_seconds > max_staleness {
        Err(OracleError::StaleData)
    } else {
        Ok(())
    }
}

/// Age, in seconds, of the oracle's latest observation relative to the current
/// ledger timestamp. `None` when the oracle does not expose an
/// `observation_timestamp` function or the call fails.
///
/// A future-dated observation (clock skew) reports an age of `0`.
pub fn observation_age_seconds(env: &Env, address: &Address) -> Option<u64> {
    let func = Symbol::new(env, "observation_timestamp");
    let args: Vec<Val> = Vec::new(env);
    match env.try_invoke_contract::<u64, soroban_sdk::Error>(address, &func, args) {
        Ok(Ok(ts)) => Some(env.ledger().timestamp().saturating_sub(ts)),
        _ => None,
    }
}

pub fn query(env: &Env, address: &Address) -> bool {
    // Expect the external oracle contract to expose a `query_release` function returning a
    // boolean indicating whether the release condition is met. This call may fail (e.g. the
    // oracle contract doesn't exist or panics); treat any failure as "condition not met" to
    // avoid unintended releases.
    let func = Symbol::new(env, "query_release");
    let args: Vec<Val> = Vec::new(env);
    let result = env.try_invoke_contract::<bool, soroban_sdk::Error>(address, &func, args);
    match result {
        Ok(Ok(b)) => b,
        _ => false,
    }
}

/// Staleness-checked variant of [`query`].
///
/// Reads the oracle's `observation_timestamp`, rejects the read with
/// [`OracleError::StaleData`] when it is older than
/// `config.max_oracle_staleness_seconds`, and only then consults
/// `query_release`. Callers that want a fallback rather than a hard error can
/// use `query_checked(..).unwrap_or(false)`, which preserves the existing
/// "treat failure as condition not met" behaviour.
pub fn query_checked(
    env: &Env,
    address: &Address,
    config: &OracleConfig,
) -> Result<bool, OracleError> {
    let age = observation_age_seconds(env, address).ok_or(OracleError::QueryFailed)?;
    check_staleness(age, config.max_oracle_staleness_seconds)?;
    Ok(query(env, address))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: u64 = 900;

    #[test]
    fn fresh_reads_are_accepted() {
        assert_eq!(check_staleness(0, MAX), Ok(()));
        assert_eq!(check_staleness(MAX / 2, MAX), Ok(()));
    }

    #[test]
    fn borderline_read_at_exact_bound_is_accepted() {
        assert_eq!(check_staleness(MAX, MAX), Ok(()));
    }

    #[test]
    fn stale_read_one_second_past_the_bound_is_rejected() {
        assert_eq!(check_staleness(MAX + 1, MAX), Err(OracleError::StaleData));
    }

    #[test]
    fn far_stale_read_is_rejected() {
        assert_eq!(
            check_staleness(MAX * 100, MAX),
            Err(OracleError::StaleData)
        );
    }

    #[test]
    fn default_config_bounds_staleness_at_fifteen_minutes() {
        assert_eq!(OracleConfig::default().max_oracle_staleness_seconds, 900);
    }
}
