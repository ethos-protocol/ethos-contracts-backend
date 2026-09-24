use soroban_sdk::{contracttype, panic_with_error, symbol_short, Address, Env, Map};

pub const CAP_SET_TOPIC: soroban_sdk::Symbol = symbol_short!("cap_set");
pub const CAP_ENFORCED_TOPIC: soroban_sdk::Symbol = symbol_short!("cap_enf");

#[contracttype]
#[derive(Clone, Debug)]
pub struct CapSetEvent {
    pub beneficiary: Address,
    pub cap: i128,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct CapEnforcedEvent {
    pub beneficiary: Address,
    pub requested: i128,
    pub capped_at: i128,
    pub excess: i128,
}

#[contracttype]
#[derive(Clone)]
pub enum CapsKey {
    BeneficiaryCaps,
}

pub fn set_cap(env: &Env, caller: &Address, beneficiary: Address, cap: i128) {
    caller.require_auth();
    if cap <= 0 {
        panic_with_error!(&env, crate::ContractError::InvalidAmount);
    }

    // Issue #345: a cap below its matching floor is an unsatisfiable range.
    // Cross-check against any floor already configured for this beneficiary,
    // including on an update that follows an earlier set.
    let floors: Map<Address, i128> = env
        .storage()
        .persistent()
        .get(&crate::floors::FloorsKey::BeneficiaryFloors)
        .unwrap_or_else(|| Map::new(env));
    if let Some(floor) = floors.get(beneficiary.clone()) {
        if crate::range_check::ensure_floor_within_cap(floor, cap).is_err() {
            panic_with_error!(&env, crate::ContractError::FloorExceedsCap);
        }
    }

    let mut caps: Map<Address, i128> = env
        .storage()
        .persistent()
        .get(&CapsKey::BeneficiaryCaps)
        .unwrap_or_else(|| Map::new(env));

    caps.set(beneficiary.clone(), cap);
    env.storage()
        .persistent()
        .set(&CapsKey::BeneficiaryCaps, &caps);

    env.events()
        .publish((CAP_SET_TOPIC,), CapSetEvent { beneficiary, cap });
}

pub fn apply_caps(env: &Env, distributions: Map<Address, i128>) -> (Map<Address, i128>, i128) {
    let caps: Map<Address, i128> = env
        .storage()
        .persistent()
        .get(&CapsKey::BeneficiaryCaps)
        .unwrap_or_else(|| Map::new(env));

    let mut capped = Map::new(env);
    let mut excess = 0i128;

    for (addr, amount) in distributions.iter() {
        let cap = caps.get(addr.clone()).unwrap_or(i128::MAX);
        let actual = amount.min(cap);
        capped.set(addr.clone(), actual);

        if amount > actual {
            let overage = amount - actual;
            excess += overage;
            env.events().publish(
                (CAP_ENFORCED_TOPIC,),
                CapEnforcedEvent {
                    beneficiary: addr,
                    requested: amount,
                    capped_at: actual,
                    excess: overage,
                },
            );
        }
    }

    redistribute_excess(env, &caps, &mut capped, excess)
}

fn redistribute_excess(
    _env: &Env,
    caps: &Map<Address, i128>,
    capped: &mut Map<Address, i128>,
    mut excess: i128,
) -> (Map<Address, i128>, i128) {
    while excess > 0 {
        let mut total_weight = 0i128;
        let mut eligible_count = 0i128;

        for (addr, amount) in capped.iter() {
            let cap = caps.get(addr).unwrap_or(i128::MAX);
            if amount < cap {
                total_weight += amount.max(0);
                eligible_count += 1;
            }
        }

        if eligible_count == 0 {
            break;
        }

        let mut distributed_this_round = 0i128;
        for (addr, amount) in capped.iter() {
            if excess <= 0 {
                break;
            }

            let cap = caps.get(addr.clone()).unwrap_or(i128::MAX);
            if amount >= cap {
                continue;
            }

            let room = cap - amount;
            let weight = if total_weight > 0 { amount.max(0) } else { 1 };
            let denominator = if total_weight > 0 {
                total_weight
            } else {
                eligible_count
            };
            let share = ((excess * weight) / denominator)
                .max(1)
                .min(room)
                .min(excess);

            capped.set(addr, amount + share);
            excess -= share;
            distributed_this_round += share;
        }

        if distributed_this_round == 0 {
            break;
        }
    }

    (capped.clone(), excess)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        testutils::{Address as _, Events},
        Address, Env, IntoVal, TryIntoVal, Val,
    };

    fn has_topic(env: &Env, topic: soroban_sdk::Symbol) -> bool {
        env.events().all().iter().any(|event| {
            let topics: soroban_sdk::Vec<Val> = event.1.clone().into_val(env);
            topics
                .get(0)
                .and_then(|topic| topic.try_into_val(env).ok())
                .map(|actual: soroban_sdk::Symbol| actual == topic)
                .unwrap_or(false)
        })
    }

    #[test]
    fn amount_within_cap_is_unchanged() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let alice = Address::generate(&env);

        set_cap(&env, &admin, alice.clone(), 1_000);

        let mut dist = Map::new(&env);
        dist.set(alice.clone(), 800);

        let (capped, excess) = apply_caps(&env, dist);

        assert_eq!(capped.get(alice).unwrap(), 800);
        assert_eq!(excess, 0);
    }

    #[test]
    fn over_cap_amount_is_redistributed() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let alice = Address::generate(&env);
        let bob = Address::generate(&env);

        set_cap(&env, &admin, alice.clone(), 500);

        let mut dist = Map::new(&env);
        dist.set(alice.clone(), 900);
        dist.set(bob.clone(), 100);

        let (capped, excess) = apply_caps(&env, dist);

        assert_eq!(capped.get(alice).unwrap(), 500);
        assert_eq!(capped.get(bob).unwrap(), 500);
        assert_eq!(excess, 0);
    }

    #[test]
    fn returns_excess_when_no_beneficiary_has_room() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let alice = Address::generate(&env);

        set_cap(&env, &admin, alice.clone(), 500);

        let mut dist = Map::new(&env);
        dist.set(alice.clone(), 900);

        let (capped, excess) = apply_caps(&env, dist);

        assert_eq!(capped.get(alice).unwrap(), 500);
        assert_eq!(excess, 400);
    }

    #[test]
    fn cap_enforced_event_is_emitted() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let alice = Address::generate(&env);

        set_cap(&env, &admin, alice.clone(), 100);

        let mut dist = Map::new(&env);
        dist.set(alice, 500);
        apply_caps(&env, dist);

        assert!(has_topic(&env, CAP_ENFORCED_TOPIC));
    }

    #[test]
    #[should_panic]
    fn rejects_zero_cap() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let alice = Address::generate(&env);

        set_cap(&env, &admin, alice, 0);
    }

    // ── Issue #345: floors/caps boundary consistency ─────────────────────────

    #[test]
    fn cap_above_existing_floor_is_accepted() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let alice = Address::generate(&env);

        crate::floors::set_floor(&env, &admin, alice.clone(), 200);
        set_cap(&env, &admin, alice.clone(), 500);

        let caps: Map<Address, i128> = env
            .storage()
            .persistent()
            .get(&CapsKey::BeneficiaryCaps)
            .unwrap();
        assert_eq!(caps.get(alice).unwrap(), 500);
    }

    #[test]
    #[should_panic]
    fn cap_below_existing_floor_is_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let alice = Address::generate(&env);

        crate::floors::set_floor(&env, &admin, alice.clone(), 400);
        set_cap(&env, &admin, alice, 300);
    }

    #[test]
    #[should_panic]
    fn cap_update_after_initial_set_that_undercuts_floor_is_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let alice = Address::generate(&env);

        // Valid initial configuration.
        crate::floors::set_floor(&env, &admin, alice.clone(), 400);
        set_cap(&env, &admin, alice.clone(), 800);

        // Regression: a later update must be re-checked against the floor.
        set_cap(&env, &admin, alice, 100);
    }
}
