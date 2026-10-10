//! Tests for Vault Governance Features (Issue #615)

#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{testutils::Address as _, testutils::Ledger as _, Address, Bytes, Env, Vec};

use crate::vault_governance::{
    cast_vote, create_proposal, execute_proposal, get_proposal, get_vault_governance_config,
    list_vault_proposals, register_stakeholders, resolve_proposal, set_vault_governance_config,
    GovernanceAction, ProposalStatus, DEFAULT_APPROVAL_THRESHOLD_BPS,
    DEFAULT_GOVERNANCE_MIN_QUORUM, DEFAULT_GOVERNANCE_VOTING_PERIOD,
};
use crate::ContractError;

// ── Test Contract Mock ────────────────────────────────────────────────────────

#[contract]
pub struct MockGovernanceContract;

#[contractimpl]
impl MockGovernanceContract {
    pub fn init_test(_env: Env) {}
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn setup_env() -> (Env, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, MockGovernanceContract);
    (env, contract_id)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[test]
fn test_default_and_custom_governance_config() {
    let (env, contract_id) = setup_env();
    let vault_id = 101u64;

    env.as_contract(&contract_id, || {
        // Default config
        let cfg = get_vault_governance_config(&env, vault_id);
        assert_eq!(cfg.voting_period, DEFAULT_GOVERNANCE_VOTING_PERIOD);
        assert_eq!(cfg.min_quorum, DEFAULT_GOVERNANCE_MIN_QUORUM);
        assert_eq!(cfg.approval_threshold_bps, DEFAULT_APPROVAL_THRESHOLD_BPS);

        // Custom config
        set_vault_governance_config(&env, vault_id, 86_400, 3, 6_000);
        let updated = get_vault_governance_config(&env, vault_id);
        assert_eq!(updated.voting_period, 86_400);
        assert_eq!(updated.min_quorum, 3);
        assert_eq!(updated.approval_threshold_bps, 6_000);
    });
}

#[test]
fn test_proposal_lifecycle_approved_and_executed() {
    let (env, contract_id) = setup_env();
    let vault_id = 202u64;
    let proposer = Address::generate(&env);
    let voter1 = Address::generate(&env);
    let voter2 = Address::generate(&env);

    env.as_contract(&contract_id, || {
        // Set config: quorum = 2, threshold = 50%
        set_vault_governance_config(&env, vault_id, 3600, 2, 5_000);

        let action = GovernanceAction::UpdateReleaseThreshold(5);
        let desc = Bytes::from_slice(&env, b"Increase release threshold to 5");

        let prop_id = create_proposal(&env, vault_id, proposer.clone(), action.clone(), desc);
        assert_eq!(prop_id, 1);

        let proposal = get_proposal(&env, vault_id, prop_id).expect("proposal exists");
        assert_eq!(proposal.status, ProposalStatus::Pending);
        assert_eq!(proposal.yes_votes, 0);
        assert_eq!(proposal.no_votes, 0);

        // Cast 2 yes votes
        let res1 = cast_vote(&env, vault_id, prop_id, voter1.clone(), true);
        assert!(res1.is_ok());

        let res2 = cast_vote(&env, vault_id, prop_id, voter2.clone(), true);
        assert!(res2.is_ok());

        // Resolve proposal
        let status = resolve_proposal(&env, vault_id, prop_id).expect("resolve ok");
        assert_eq!(status, ProposalStatus::Passed);

        // Execute proposal
        let exec_res = execute_proposal(&env, vault_id, prop_id, proposer.clone());
        assert!(exec_res.is_ok());

        let final_prop = get_proposal(&env, vault_id, prop_id).unwrap();
        assert_eq!(final_prop.status, ProposalStatus::Executed);
    });
}

#[test]
fn test_proposal_rejected_when_no_votes_dominate() {
    let (env, contract_id) = setup_env();
    let vault_id = 303u64;
    let proposer = Address::generate(&env);
    let voter1 = Address::generate(&env);
    let voter2 = Address::generate(&env);

    env.as_contract(&contract_id, || {
        set_vault_governance_config(&env, vault_id, 3600, 2, 5_000);

        let action = GovernanceAction::EmergencyFreeze;
        let desc = Bytes::from_slice(&env, b"Emergency freeze test");

        let prop_id = create_proposal(&env, vault_id, proposer, action, desc);

        // 1 yes, 1 no -> 50% approval meets 5000 bps threshold, but let's do 2 no votes
        let _ = cast_vote(&env, vault_id, prop_id, voter1, false);
        let _ = cast_vote(&env, vault_id, prop_id, voter2, false);

        let status = resolve_proposal(&env, vault_id, prop_id).expect("resolve ok");
        assert_eq!(status, ProposalStatus::Rejected);

        // Cannot execute rejected proposal
        let exec_res = execute_proposal(&env, vault_id, prop_id, Address::generate(&env));
        assert_eq!(exec_res, Err(ContractError::ProposalNotApproved));
    });
}

#[test]
fn test_duplicate_vote_rejected() {
    let (env, contract_id) = setup_env();
    let vault_id = 404u64;
    let proposer = Address::generate(&env);
    let voter = Address::generate(&env);

    env.as_contract(&contract_id, || {
        let action = GovernanceAction::Unfreeze;
        let desc = Bytes::from_slice(&env, b"Unfreeze vault");

        let prop_id = create_proposal(&env, vault_id, proposer, action, desc);

        let first_vote = cast_vote(&env, vault_id, prop_id, voter.clone(), true);
        assert!(first_vote.is_ok());

        let second_vote = cast_vote(&env, vault_id, prop_id, voter.clone(), true);
        assert_eq!(second_vote, Err(ContractError::AlreadyVoted));
    });
}

#[test]
fn test_stakeholder_authorization_gate() {
    let (env, contract_id) = setup_env();
    let vault_id = 505u64;
    let authorized_voter = Address::generate(&env);
    let unauthorized_voter = Address::generate(&env);

    env.as_contract(&contract_id, || {
        let mut stakeholders = Vec::new(&env);
        stakeholders.push_back(authorized_voter.clone());
        register_stakeholders(&env, vault_id, stakeholders);

        let action = GovernanceAction::UpdateInactivityPeriod(60);
        let desc = Bytes::from_slice(&env, b"Update inactivity to 60 days");

        let prop_id = create_proposal(&env, vault_id, authorized_voter.clone(), action, desc);

        // Unauthorized voter cannot vote
        let unauth_res = cast_vote(&env, vault_id, prop_id, unauthorized_voter, true);
        assert_eq!(unauth_res, Err(ContractError::NotBeneficiary));

        // Authorized voter can vote
        let auth_res = cast_vote(&env, vault_id, prop_id, authorized_voter, true);
        assert!(auth_res.is_ok());
    });
}

#[test]
fn test_list_vault_proposals() {
    let (env, contract_id) = setup_env();
    let vault_id = 606u64;
    let proposer = Address::generate(&env);

    env.as_contract(&contract_id, || {
        let desc = Bytes::from_slice(&env, b"test");
        let id1 = create_proposal(
            &env,
            vault_id,
            proposer.clone(),
            GovernanceAction::EmergencyFreeze,
            desc.clone(),
        );
        let id2 = create_proposal(
            &env,
            vault_id,
            proposer.clone(),
            GovernanceAction::Unfreeze,
            desc.clone(),
        );

        let list = list_vault_proposals(&env, vault_id);
        assert_eq!(list.len(), 2);
        assert_eq!(list.get(0).unwrap(), id1);
        assert_eq!(list.get(1).unwrap(), id2);
    });
}
