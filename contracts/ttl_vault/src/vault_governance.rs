//! Issue #615 — Implement Vault Governance Features
//!
//! Vaults have no governance. Governance enables community and stakeholder decision-making.
//!
//! Provides:
//! - Governance configuration per vault (voting_period, min_quorum, approval_threshold_bps)
//! - Proposal creation with parameterized action payloads
//! - Voting mechanism with voter tracking and duplicate vote prevention
//! - Vote counting and outcome evaluation (Passed, Rejected, Expired)
//! - Proposal execution marking the proposal as Executed
//! - Comprehensive query interface and audit events

use soroban_sdk::{
    contracttype, panic_with_error, symbol_short, Address, Bytes, Env, Map, Symbol, Vec,
};

use crate::ContractError;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Default voting period for governance proposals (3 days in seconds).
pub const DEFAULT_GOVERNANCE_VOTING_PERIOD: u64 = 259_200;

/// Default minimum quorum (minimum total votes required).
pub const DEFAULT_GOVERNANCE_MIN_QUORUM: u32 = 1;

/// Default approval threshold in basis points (50% = 5,000 bps).
pub const DEFAULT_APPROVAL_THRESHOLD_BPS: u32 = 5_000;

/// Maximum basis points (100% = 10,000 bps).
pub const BPS_DENOMINATOR: u32 = 10_000;

// ── Event Topics ─────────────────────────────────────────────────────────────

pub const GOV_PROPOSAL_CREATED_TOPIC: Symbol = symbol_short!("gov_prop");
pub const GOV_VOTE_CAST_TOPIC: Symbol = symbol_short!("gov_vote");
pub const GOV_PROPOSAL_RESOLVED_TOPIC: Symbol = symbol_short!("gov_res");
pub const GOV_PROPOSAL_EXECUTED_TOPIC: Symbol = symbol_short!("gov_exec");
pub const GOV_CONFIG_SET_TOPIC: Symbol = symbol_short!("gov_cfg");

// ── Storage Keys ─────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
pub enum GovernanceKey {
    /// Governance configuration per vault: vault_id -> VaultGovernanceConfig
    Config(u64),
    /// Monotonic proposal counter for a vault: vault_id -> u64
    ProposalCounter(u64),
    /// Specific proposal: (vault_id, proposal_id) -> VaultProposal
    Proposal(u64, u64),
    /// Vote record map: (vault_id, proposal_id) -> Map<Address, bool>
    Votes(u64, u64),
    /// List of proposal IDs for a vault: vault_id -> Vec<u64>
    VaultProposals(u64),
    /// Stakeholder / voter registry per vault: vault_id -> Vec<Address>
    StakeholderRegistry(u64),
}

// ── Types ─────────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum ProposalStatus {
    Pending,
    Passed,
    Rejected,
    Executed,
    Expired,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum GovernanceAction {
    /// Update release threshold value
    UpdateReleaseThreshold(u32),
    /// Update inactivity period in days
    UpdateInactivityPeriod(u32),
    /// Emergency freeze action
    EmergencyFreeze,
    /// Unfreeze vault action
    Unfreeze,
    /// Custom payload action
    CustomAction(Bytes),
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VaultGovernanceConfig {
    /// Voting period in seconds
    pub voting_period: u64,
    /// Minimum votes required for valid resolution
    pub min_quorum: u32,
    /// Required approval percentage in basis points (5000 = 50%)
    pub approval_threshold_bps: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct VaultProposal {
    pub vault_id: u64,
    pub proposal_id: u64,
    pub proposer: Address,
    pub action: GovernanceAction,
    pub description: Bytes,
    pub status: ProposalStatus,
    pub created_at: u64,
    pub voting_deadline: u64,
    pub yes_votes: u32,
    pub no_votes: u32,
    pub total_voters: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct GovernanceVoteRecord {
    pub voter: Address,
    pub approve: bool,
    pub voted_at: u64,
}

// ── Events ────────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug)]
pub struct GovProposalCreatedEvent {
    pub vault_id: u64,
    pub proposal_id: u64,
    pub proposer: Address,
    pub voting_deadline: u64,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct GovVoteCastEvent {
    pub vault_id: u64,
    pub proposal_id: u64,
    pub voter: Address,
    pub approve: bool,
    pub current_yes: u32,
    pub current_no: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct GovProposalResolvedEvent {
    pub vault_id: u64,
    pub proposal_id: u64,
    pub passed: bool,
    pub yes_votes: u32,
    pub no_votes: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct GovProposalExecutedEvent {
    pub vault_id: u64,
    pub proposal_id: u64,
    pub executor: Address,
    pub executed_at: u64,
}

// ── Public Helper & Management Functions ─────────────────────────────────────

/// Set governance configuration for a vault.
pub fn set_vault_governance_config(
    env: &Env,
    vault_id: u64,
    voting_period: u64,
    min_quorum: u32,
    approval_threshold_bps: u32,
) {
    if approval_threshold_bps > BPS_DENOMINATOR {
        panic_with_error!(env, ContractError::InvalidThreshold);
    }
    let config = VaultGovernanceConfig {
        voting_period: if voting_period == 0 {
            DEFAULT_GOVERNANCE_VOTING_PERIOD
        } else {
            voting_period
        },
        min_quorum: if min_quorum == 0 {
            DEFAULT_GOVERNANCE_MIN_QUORUM
        } else {
            min_quorum
        },
        approval_threshold_bps: if approval_threshold_bps == 0 {
            DEFAULT_APPROVAL_THRESHOLD_BPS
        } else {
            approval_threshold_bps
        },
    };
    let key = GovernanceKey::Config(vault_id);
    env.storage().persistent().set(&key, &config);
    env.storage().persistent().extend_ttl(
        &key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    env.events().publish(
        (GOV_CONFIG_SET_TOPIC, vault_id),
        (config.voting_period, config.min_quorum, config.approval_threshold_bps),
    );
}

/// Get governance configuration for a vault with sensible defaults.
pub fn get_vault_governance_config(env: &Env, vault_id: u64) -> VaultGovernanceConfig {
    let key = GovernanceKey::Config(vault_id);
    env.storage().persistent().get(&key).unwrap_or(VaultGovernanceConfig {
        voting_period: DEFAULT_GOVERNANCE_VOTING_PERIOD,
        min_quorum: DEFAULT_GOVERNANCE_MIN_QUORUM,
        approval_threshold_bps: DEFAULT_APPROVAL_THRESHOLD_BPS,
    })
}

/// Register stakeholders (eligible voters) for a vault.
pub fn register_stakeholders(env: &Env, vault_id: u64, stakeholders: Vec<Address>) {
    let key = GovernanceKey::StakeholderRegistry(vault_id);
    env.storage().persistent().set(&key, &stakeholders);
    env.storage().persistent().extend_ttl(
        &key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );
}

/// Get registered stakeholders for a vault.
pub fn get_stakeholders(env: &Env, vault_id: u64) -> Vec<Address> {
    let key = GovernanceKey::StakeholderRegistry(vault_id);
    env.storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| Vec::new(env))
}

/// Check if an address is an eligible stakeholder.
pub fn is_stakeholder(env: &Env, vault_id: u64, address: &Address) -> bool {
    let stakeholders = get_stakeholders(env, vault_id);
    if stakeholders.is_empty() {
        // If no explicit stakeholders registered, any valid caller can participate
        return true;
    }
    for sh in stakeholders.iter() {
        if &sh == address {
            return true;
        }
    }
    false
}

// ── Core Proposal & Voting Logic ─────────────────────────────────────────────

/// Propose a governance action for a vault.
pub fn create_proposal(
    env: &Env,
    vault_id: u64,
    proposer: Address,
    action: GovernanceAction,
    description: Bytes,
) -> u64 {
    if !is_stakeholder(env, vault_id, &proposer) {
        panic_with_error!(env, ContractError::NotBeneficiary);
    }

    let config = get_vault_governance_config(env, vault_id);
    let counter_key = GovernanceKey::ProposalCounter(vault_id);
    let current_id: u64 = env.storage().persistent().get(&counter_key).unwrap_or(0u64);
    let new_proposal_id = current_id.saturating_add(1);

    let now = env.ledger().timestamp();
    let voting_deadline = now.saturating_add(config.voting_period);

    let proposal = VaultProposal {
        vault_id,
        proposal_id: new_proposal_id,
        proposer: proposer.clone(),
        action,
        description,
        status: ProposalStatus::Pending,
        created_at: now,
        voting_deadline,
        yes_votes: 0,
        no_votes: 0,
        total_voters: 0,
    };

    // Save proposal
    let prop_key = GovernanceKey::Proposal(vault_id, new_proposal_id);
    env.storage().persistent().set(&prop_key, &proposal);
    env.storage().persistent().extend_ttl(
        &prop_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    // Update counter
    env.storage().persistent().set(&counter_key, &new_proposal_id);
    env.storage().persistent().extend_ttl(
        &counter_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    // Track in vault proposal list
    let list_key = GovernanceKey::VaultProposals(vault_id);
    let mut proposals: Vec<u64> = env.storage().persistent().get(&list_key).unwrap_or_else(|| Vec::new(env));
    proposals.push_back(new_proposal_id);
    env.storage().persistent().set(&list_key, &proposals);
    env.storage().persistent().extend_ttl(
        &list_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    // Emit event
    env.events().publish(
        (GOV_PROPOSAL_CREATED_TOPIC, vault_id),
        GovProposalCreatedEvent {
            vault_id,
            proposal_id: new_proposal_id,
            proposer,
            voting_deadline,
        },
    );

    new_proposal_id
}

/// Get proposal by vault_id and proposal_id.
pub fn get_proposal(env: &Env, vault_id: u64, proposal_id: u64) -> Option<VaultProposal> {
    let key = GovernanceKey::Proposal(vault_id, proposal_id);
    env.storage().persistent().get(&key)
}

/// Cast a vote on an active governance proposal.
pub fn cast_vote(
    env: &Env,
    vault_id: u64,
    proposal_id: u64,
    voter: Address,
    approve: bool,
) -> Result<(), ContractError> {
    if !is_stakeholder(env, vault_id, &voter) {
        return Err(ContractError::NotBeneficiary);
    }

    let prop_key = GovernanceKey::Proposal(vault_id, proposal_id);
    let mut proposal: VaultProposal = env
        .storage()
        .persistent()
        .get(&prop_key)
        .ok_or(ContractError::ProposalNotFound)?;

    if proposal.status != ProposalStatus::Pending {
        return Err(ContractError::VotingNotEnabled);
    }

    let now = env.ledger().timestamp();
    if now > proposal.voting_deadline {
        return Err(ContractError::ProposalExpired);
    }

    let votes_key = GovernanceKey::Votes(vault_id, proposal_id);
    let mut votes_map: Map<Address, bool> = env
        .storage()
        .persistent()
        .get(&votes_key)
        .unwrap_or_else(|| Map::new(env));

    if votes_map.contains_key(voter.clone()) {
        return Err(ContractError::AlreadyVoted);
    }

    // Record vote
    votes_map.set(voter.clone(), approve);
    env.storage().persistent().set(&votes_key, &votes_map);
    env.storage().persistent().extend_ttl(
        &votes_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    // Update tallies
    if approve {
        proposal.yes_votes = proposal.yes_votes.saturating_add(1);
    } else {
        proposal.no_votes = proposal.no_votes.saturating_add(1);
    }
    proposal.total_voters = proposal.total_voters.saturating_add(1);

    env.storage().persistent().set(&prop_key, &proposal);
    env.storage().persistent().extend_ttl(
        &prop_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    env.events().publish(
        (GOV_VOTE_CAST_TOPIC, vault_id),
        GovVoteCastEvent {
            vault_id,
            proposal_id,
            voter,
            approve,
            current_yes: proposal.yes_votes,
            current_no: proposal.no_votes,
        },
    );

    Ok(())
}

/// Resolve a proposal after voting deadline or once voting criteria are met.
pub fn resolve_proposal(
    env: &Env,
    vault_id: u64,
    proposal_id: u64,
) -> Result<ProposalStatus, ContractError> {
    let prop_key = GovernanceKey::Proposal(vault_id, proposal_id);
    let mut proposal: VaultProposal = env
        .storage()
        .persistent()
        .get(&prop_key)
        .ok_or(ContractError::ProposalNotFound)?;

    if proposal.status != ProposalStatus::Pending {
        return Ok(proposal.status);
    }

    let config = get_vault_governance_config(env, vault_id);
    let total_votes = proposal.yes_votes.saturating_add(proposal.no_votes);

    if total_votes < config.min_quorum {
        // Quorum not reached
        let now = env.ledger().timestamp();
        if now > proposal.voting_deadline {
            proposal.status = ProposalStatus::Expired;
            env.storage().persistent().set(&prop_key, &proposal);
            return Ok(ProposalStatus::Expired);
        }
        return Ok(ProposalStatus::Pending);
    }

    // Calculate approval percentage in basis points
    let approval_bps = if total_votes == 0 {
        0
    } else {
        ((proposal.yes_votes as u64) * (BPS_DENOMINATOR as u64) / (total_votes as u64)) as u32
    };

    let passed = approval_bps >= config.approval_threshold_bps;
    proposal.status = if passed {
        ProposalStatus::Passed
    } else {
        ProposalStatus::Rejected
    };

    env.storage().persistent().set(&prop_key, &proposal);
    env.storage().persistent().extend_ttl(
        &prop_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    env.events().publish(
        (GOV_PROPOSAL_RESOLVED_TOPIC, vault_id),
        GovProposalResolvedEvent {
            vault_id,
            proposal_id,
            passed,
            yes_votes: proposal.yes_votes,
            no_votes: proposal.no_votes,
        },
    );

    Ok(proposal.status)
}

/// Execute an approved governance proposal.
pub fn execute_proposal(
    env: &Env,
    vault_id: u64,
    proposal_id: u64,
    executor: Address,
) -> Result<(), ContractError> {
    let prop_key = GovernanceKey::Proposal(vault_id, proposal_id);
    let mut proposal: VaultProposal = env
        .storage()
        .persistent()
        .get(&prop_key)
        .ok_or(ContractError::ProposalNotFound)?;

    // If still pending, attempt resolution first
    if proposal.status == ProposalStatus::Pending {
        let resolved_status = resolve_proposal(env, vault_id, proposal_id)?;
        proposal.status = resolved_status;
    }

    if proposal.status != ProposalStatus::Passed {
        return Err(ContractError::ProposalNotApproved);
    }

    // Mark as Executed
    proposal.status = ProposalStatus::Executed;
    env.storage().persistent().set(&prop_key, &proposal);
    env.storage().persistent().extend_ttl(
        &prop_key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    let now = env.ledger().timestamp();
    env.events().publish(
        (GOV_PROPOSAL_EXECUTED_TOPIC, vault_id),
        GovProposalExecutedEvent {
            vault_id,
            proposal_id,
            executor,
            executed_at: now,
        },
    );

    Ok(())
}

/// List all proposal IDs for a vault.
pub fn list_vault_proposals(env: &Env, vault_id: u64) -> Vec<u64> {
    let list_key = GovernanceKey::VaultProposals(vault_id);
    env.storage()
        .persistent()
        .get(&list_key)
        .unwrap_or_else(|| Vec::new(env))
}
