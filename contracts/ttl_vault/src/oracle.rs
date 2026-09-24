// Minimal oracle module for external release condition queries
use soroban_sdk::{Address, Env, Symbol, Val, Vec};

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

/// Query the oracle for the price of a Stellar asset, normalized to a common base
/// currency (e.g. USD equivalent). The oracle contract is expected to expose a
/// `query_price` function taking the asset address and returning the price as an
/// `i128` scaled by `PRICE_SCALE`.
///
/// Any failure (missing contract, unsupported asset, panic) is treated as a price of
/// `0`, which callers must interpret as "no price available" rather than a free asset.
pub const PRICE_SCALE: i128 = 10_000_000; // 7 decimal places, matching Stellar asset precision

pub fn query_price(env: &Env, oracle: &Address, asset: &Address) -> i128 {
    let func = Symbol::new(env, "query_price");
    let mut args: Vec<Val> = Vec::new(env);
    args.push_back(asset.clone().into_val(env));
    let result = env.try_invoke_contract::<i128, soroban_sdk::Error>(oracle, &func, args);
    match result {
        Ok(Ok(price)) if price > 0 => price,
        _ => 0,
    }
}

/// Convert an `amount` of an asset into the normalized base currency using the
/// provided `conversion_rate` (scaled by `PRICE_SCALE`). Returns `0` when the rate
/// is non-positive so that invalid conversions cannot inflate deposit value.
pub fn convert_to_base(amount: i128, conversion_rate: i128) -> i128 {
    if amount <= 0 || conversion_rate <= 0 {
        return 0;
    }
    amount.saturating_mul(conversion_rate) / PRICE_SCALE
}
