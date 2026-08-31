use soroban_sdk::{contracttype, symbol_short, Address, Env, Map, Vec};

pub const RANKING_SET_TOPIC: soroban_sdk::Symbol = symbol_short!("rank_set");
pub const DISTRIBUTED_BY_RANK_TOPIC: soroban_sdk::Symbol = symbol_short!("dist_rank");

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RankedBeneficiary {
    pub address: Address,
    pub priority: u32,
    pub allocation_bps: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct RankingSetEvent {
    pub beneficiary: Address,
    pub priority: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct DistributedByRankEvent {
    pub beneficiary: Address,
    pub priority: u32,
    pub amount: i128,
}

#[contracttype]
#[derive(Clone)]
pub enum RankingKey {
    BeneficiaryRanks,
    /// `true` when the cached rank positions may be out of date and must be
    /// recomputed on the next `get_rank` call.
    RankStale,
    /// Cached 1-based rank position for a beneficiary (1 == highest priority).
    CachedRank(Address),
}

pub fn set_rank(env: &Env, caller: &Address, beneficiary: Address, priority: u32) {
    caller.require_auth();

    let mut ranks: Map<Address, RankedBeneficiary> = env
        .storage()
        .persistent()
        .get(&RankingKey::BeneficiaryRanks)
        .unwrap_or_else(|| Map::new(env));

    let allocation_bps = ranks
        .get(beneficiary.clone())
        .map_or(10_000, |b| b.allocation_bps);

    ranks.set(
        beneficiary.clone(),
        RankedBeneficiary {
            address: beneficiary.clone(),
            priority,
            allocation_bps,
        },
    );
    env.storage()
        .persistent()
        .set(&RankingKey::BeneficiaryRanks, &ranks);

    // A priority change reorders the field — cached ranks are now stale.
    invalidate_ranks(env);

    env.events().publish(
        (RANKING_SET_TOPIC,),
        RankingSetEvent {
            beneficiary,
            priority,
        },
    );
}

/// Mark the cached rank positions as stale.
///
/// Call this from any path whose output feeds ranking (e.g. recording a slice
/// performance sample) so the next [`get_rank`] recomputes instead of returning
/// a stale position.
pub fn invalidate_ranks(env: &Env) {
    env.storage().persistent().set(&RankingKey::RankStale, &true);
}

/// `true` when the cached rank positions are stale.
pub fn ranks_are_stale(env: &Env) -> bool {
    env.storage()
        .persistent()
        .get(&RankingKey::RankStale)
        .unwrap_or(false)
}

/// Recompute and cache the 1-based rank position of every ranked beneficiary,
/// ordered by ascending `priority` (ties broken by map iteration order), then
/// clear the stale flag.
pub fn recompute_ranks(env: &Env) {
    let ranks: Map<Address, RankedBeneficiary> = env
        .storage()
        .persistent()
        .get(&RankingKey::BeneficiaryRanks)
        .unwrap_or_else(|| Map::new(env));

    let mut ordered = Vec::new(env);
    for (_, beneficiary) in ranks.iter() {
        ordered.push_back(beneficiary);
    }

    // Bubble sort by priority ascending — same ordering as `distribute_by_rank`.
    let len = ordered.len();
    for i in 0..len {
        for j in 0..len.saturating_sub(i + 1) {
            if ordered.get(j).unwrap().priority > ordered.get(j + 1).unwrap().priority {
                let left = ordered.get(j).unwrap();
                let right = ordered.get(j + 1).unwrap();
                ordered.set(j, right);
                ordered.set(j + 1, left);
            }
        }
    }

    let mut position = 1u32;
    for beneficiary in ordered.iter() {
        env.storage()
            .persistent()
            .set(&RankingKey::CachedRank(beneficiary.address.clone()), &position);
        position = position.saturating_add(1);
    }

    env.storage().persistent().set(&RankingKey::RankStale, &false);
}

/// Return the cached 1-based rank position for `beneficiary`.
///
/// If the cache is stale, all ranks are recomputed first (lazy recomputation).
/// Returns `None` when the beneficiary has no ranking entry.
pub fn get_rank(env: &Env, beneficiary: &Address) -> Option<u32> {
    if ranks_are_stale(env) {
        recompute_ranks(env);
    }
    env.storage()
        .persistent()
        .get(&RankingKey::CachedRank(beneficiary.clone()))
}

pub fn distribute_by_rank(env: &Env, total_amount: i128) -> Map<Address, i128> {
    let ranks: Map<Address, RankedBeneficiary> = env
        .storage()
        .persistent()
        .get(&RankingKey::BeneficiaryRanks)
        .unwrap_or_else(|| Map::new(env));

    let mut ordered = Vec::new(env);
    for (_, beneficiary) in ranks.iter() {
        ordered.push_back(beneficiary);
    }

    let len = ordered.len();
    for i in 0..len {
        for j in 0..len.saturating_sub(i + 1) {
            if ordered.get(j).unwrap().priority > ordered.get(j + 1).unwrap().priority {
                let left = ordered.get(j).unwrap();
                let right = ordered.get(j + 1).unwrap();
                ordered.set(j, right);
                ordered.set(j + 1, left);
            }
        }
    }

    let mut distributions = Map::new(env);
    let mut remaining = total_amount;
    let mut i = 0;

    while i < ordered.len() && remaining > 0 {
        let priority = ordered.get(i).unwrap().priority;
        let mut tier = Vec::new(env);
        let mut j = i;
        while j < ordered.len() && ordered.get(j).unwrap().priority == priority {
            tier.push_back(ordered.get(j).unwrap());
            j += 1;
        }

        let mut total_bps = 0u32;
        for beneficiary in tier.iter() {
            total_bps = total_bps.saturating_add(beneficiary.allocation_bps);
        }

        // Distribute proportionally to `allocation_bps` within this tier. `total_bps` is
        // decremented alongside `remaining` as each beneficiary is paid, so the ratio
        // applied to what's left stays consistent instead of compounding shrinkage.
        let mut tier_remaining_bps = total_bps;
        for beneficiary in tier.iter() {
            if remaining <= 0 {
                break;
            }
            let amount = if tier_remaining_bps == 0 {
                0
            } else {
                (remaining * beneficiary.allocation_bps as i128) / tier_remaining_bps as i128
            };
            let amount = amount.min(remaining);
            distributions.set(beneficiary.address.clone(), amount);
            remaining -= amount;
            tier_remaining_bps = tier_remaining_bps.saturating_sub(beneficiary.allocation_bps);
            env.events().publish(
                (DISTRIBUTED_BY_RANK_TOPIC,),
                DistributedByRankEvent {
                    beneficiary: beneficiary.address,
                    priority: beneficiary.priority,
                    amount,
                },
            );
        }

        i = j;
    }

    distributions
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        testutils::{Address as _, Events},
        Address, Env, IntoVal, TryIntoVal, Val,
    };

    #[test]
    fn higher_priority_receives_before_lower_priority() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TtlVaultContract);
        let admin = Address::generate(&env);
        let alice = Address::generate(&env);
        let bob = Address::generate(&env);

        env.as_contract(&contract_id, || set_rank(&env, &admin, alice.clone(), 1));
        env.as_contract(&contract_id, || set_rank(&env, &admin, bob.clone(), 2));
        let result = env.as_contract(&contract_id, || distribute_by_rank(&env, 500));

        assert_eq!(result.get(alice).unwrap(), 500);
        assert_eq!(result.get(bob).unwrap_or(0), 0);
    }

    #[test]
    fn same_priority_splits_by_allocation_bps() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TtlVaultContract);
        let admin = Address::generate(&env);
        let alice = Address::generate(&env);
        let bob = Address::generate(&env);

        env.as_contract(&contract_id, || set_rank(&env, &admin, alice.clone(), 1));
        env.as_contract(&contract_id, || set_rank(&env, &admin, bob.clone(), 1));
        let result = env.as_contract(&contract_id, || distribute_by_rank(&env, 1_000));

        assert_eq!(result.get(alice).unwrap(), 500);
        assert_eq!(result.get(bob).unwrap(), 500);
    }

    #[test]
    fn get_rank_returns_none_for_unranked_beneficiary() {
        let env = Env::default();
        let contract_id = env.register_contract(None, crate::TtlVaultContract);
        let stranger = Address::generate(&env);
        env.as_contract(&contract_id, || {
            assert_eq!(get_rank(&env, &stranger), None);
        });
    }

    #[test]
    fn invalidate_ranks_sets_the_stale_flag() {
        let env = Env::default();
        let contract_id = env.register_contract(None, crate::TtlVaultContract);
        env.as_contract(&contract_id, || {
            assert!(!ranks_are_stale(&env));
            invalidate_ranks(&env);
            assert!(ranks_are_stale(&env));
        });
    }

    #[test]
    fn get_rank_recomputes_lazily_when_stale() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TtlVaultContract);
        let admin = Address::generate(&env);
        let alice = Address::generate(&env);
        let bob = Address::generate(&env);

        env.as_contract(&contract_id, || set_rank(&env, &admin, alice.clone(), 5));
        env.as_contract(&contract_id, || set_rank(&env, &admin, bob.clone(), 10));

        // set_rank left the cache stale; the first get_rank recomputes it.
        env.as_contract(&contract_id, || {
            assert!(ranks_are_stale(&env));
            assert_eq!(get_rank(&env, &alice), Some(1));
            assert_eq!(get_rank(&env, &bob), Some(2));
            assert!(!ranks_are_stale(&env));
        });

        // Reordering bob ahead of alice must invalidate and reflect on next read.
        env.as_contract(&contract_id, || set_rank(&env, &admin, bob.clone(), 1));
        env.as_contract(&contract_id, || {
            assert!(ranks_are_stale(&env));
            assert_eq!(get_rank(&env, &bob), Some(1));
            assert_eq!(get_rank(&env, &alice), Some(2));
        });
    }

    #[test]
    fn recording_slice_performance_invalidates_cached_rank() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TtlVaultContract);
        let admin = Address::generate(&env);
        let alice = Address::generate(&env);
        let attestor = Address::generate(&env);

        env.as_contract(&contract_id, || set_rank(&env, &admin, alice.clone(), 1));
        env.as_contract(&contract_id, || {
            let _ = get_rank(&env, &alice);
            assert!(!ranks_are_stale(&env));
        });

        env.as_contract(&contract_id, || {
            crate::slice_performance::record_attestor_performance(&env, 1, &attestor, true, 100);
            assert!(ranks_are_stale(&env));
        });
    }

    #[test]
    fn ranking_set_event_is_emitted() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::TtlVaultContract);
        let admin = Address::generate(&env);
        let alice = Address::generate(&env);

        env.as_contract(&contract_id, || {
            set_rank(&env, &admin, alice, 1);
        });

        assert!(env.events().all().iter().any(|event| {
            let topics: Vec<Val> = event.1.clone().into_val(&env);
            topics
                .get(0)
                .and_then(|topic| topic.try_into_val(&env).ok())
                .is_some_and(|topic: soroban_sdk::Symbol| topic == RANKING_SET_TOPIC)
        }));
    }
}
