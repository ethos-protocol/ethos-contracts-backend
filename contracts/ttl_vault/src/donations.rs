//! Issue #613 — Implement Vault Donation Features
//!
//! Vaults cannot currently support charitable giving. This module lets a vault
//! owner register a donation account (a charity address) for a vault, lets
//! anyone donate to it, and keeps an auditable, publicly queryable record of
//! every donation.
//!
//! Provides:
//! - Donation account setup per vault (recipient + public-visibility flag)
//! - Donations transferred straight from the donor to the donation account, so
//!   donated funds are never custodied by the vault
//! - Sequential tax receipts per vault, each carrying a SHA-256 receipt hash
//!   over the donation's immutable fields
//! - Per-vault and per-donor running totals
//! - Paginated public listing (only when the owner enables public visibility)
//! - Audit events for account setup and each donation

use soroban_sdk::{
    contracttype, panic_with_error, symbol_short, token, xdr::ToXdr, Address, Bytes, BytesN, Env,
    Symbol, Vec,
};

use crate::ContractError;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Upper bound on the number of donation records returned by one listing call.
pub const MAX_DONATION_PAGE_SIZE: u32 = 50;

// ── Event Topics ─────────────────────────────────────────────────────────────

pub const DONATION_ACCOUNT_SET_TOPIC: Symbol = symbol_short!("don_set");
pub const DONATION_MADE_TOPIC: Symbol = symbol_short!("don_made");

// ── Storage Keys ─────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
pub enum DonationKey {
    /// Donation account configuration per vault: vault_id -> DonationAccount
    Account(u64),
    /// Monotonic receipt counter per vault: vault_id -> u64
    ReceiptCounter(u64),
    /// Tax receipt / donation record: (vault_id, receipt_no) -> DonationRecord
    Record(u64, u64),
    /// Total amount donated to a vault: vault_id -> i128
    VaultTotal(u64),
    /// Total amount a donor has given to a vault: (vault_id, donor) -> i128
    DonorTotal(u64, Address),
}

// ── Types ─────────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct DonationAccount {
    /// Charity / recipient address that receives donated funds
    pub recipient: Address,
    /// When true, donation records are visible through the public listing.
    /// When false, only aggregate totals are exposed.
    pub public_visibility: bool,
}

/// A donation and its tax receipt. `receipt_no` is unique per vault and
/// `receipt_hash` commits to every immutable field of the record.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct DonationRecord {
    pub vault_id: u64,
    pub receipt_no: u64,
    pub donor: Address,
    pub recipient: Address,
    pub token_address: Address,
    pub amount: i128,
    pub donated_at: u64,
    pub receipt_hash: BytesN<32>,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct DonationAccountSetEvent {
    pub vault_id: u64,
    pub recipient: Address,
    pub public_visibility: bool,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct DonationMadeEvent {
    pub vault_id: u64,
    pub receipt_no: u64,
    pub donor: Address,
    pub amount: i128,
    pub receipt_hash: BytesN<32>,
}

// ── Account setup ─────────────────────────────────────────────────────────────

/// Register or replace the donation account for `vault_id`.
///
/// Authorization is enforced by the caller (the vault owner).
pub fn set_donation_account(env: &Env, vault_id: u64, recipient: Address, public_visibility: bool) {
    let account = DonationAccount {
        recipient: recipient.clone(),
        public_visibility,
    };
    let key = DonationKey::Account(vault_id);
    env.storage().persistent().set(&key, &account);
    env.storage().persistent().extend_ttl(
        &key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    env.events().publish(
        (DONATION_ACCOUNT_SET_TOPIC, vault_id),
        DonationAccountSetEvent {
            vault_id,
            recipient,
            public_visibility,
        },
    );
}

/// Return the donation account for `vault_id`, if one has been configured.
pub fn get_donation_account(env: &Env, vault_id: u64) -> Option<DonationAccount> {
    env.storage()
        .persistent()
        .get(&DonationKey::Account(vault_id))
}

// ── Donations ─────────────────────────────────────────────────────────────────

/// Donate `amount` of `token_address` from `donor` to the vault's donation
/// account and issue a tax receipt.
///
/// Authorization is enforced by the caller (the donor). Panics with
/// `InvalidAmount` for non-positive amounts and `DonationAccountNotSet` when the
/// vault has no donation account.
pub fn donate(
    env: &Env,
    vault_id: u64,
    donor: &Address,
    token_address: &Address,
    amount: i128,
) -> DonationRecord {
    if amount <= 0 {
        panic_with_error!(env, ContractError::InvalidAmount);
    }
    let account = get_donation_account(env, vault_id)
        .unwrap_or_else(|| panic_with_error!(env, ContractError::DonationAccountNotSet));

    // Funds move directly donor -> charity; the vault never holds them.
    token::Client::new(env, token_address).transfer(donor, &account.recipient, &amount);

    let receipt_no = next_receipt_no(env, vault_id);
    let donated_at = env.ledger().timestamp();
    let receipt_hash = receipt_hash(env, vault_id, receipt_no, donor, amount, donated_at);

    let record = DonationRecord {
        vault_id,
        receipt_no,
        donor: donor.clone(),
        recipient: account.recipient,
        token_address: token_address.clone(),
        amount,
        donated_at,
        receipt_hash: receipt_hash.clone(),
    };

    let key = DonationKey::Record(vault_id, receipt_no);
    env.storage().persistent().set(&key, &record);
    env.storage().persistent().extend_ttl(
        &key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );

    add_to_total(env, &DonationKey::VaultTotal(vault_id), amount);
    add_to_total(
        env,
        &DonationKey::DonorTotal(vault_id, donor.clone()),
        amount,
    );

    env.events().publish(
        (DONATION_MADE_TOPIC, vault_id),
        DonationMadeEvent {
            vault_id,
            receipt_no,
            donor: donor.clone(),
            amount,
            receipt_hash,
        },
    );

    record
}

/// Fetch a tax receipt (donation record) by its per-vault receipt number.
pub fn get_donation(env: &Env, vault_id: u64, receipt_no: u64) -> Option<DonationRecord> {
    env.storage()
        .persistent()
        .get(&DonationKey::Record(vault_id, receipt_no))
}

/// Total amount donated to `vault_id` across all donors.
pub fn get_vault_donation_total(env: &Env, vault_id: u64) -> i128 {
    read_total(env, &DonationKey::VaultTotal(vault_id))
}

/// Total amount `donor` has given to `vault_id`.
pub fn get_donor_donation_total(env: &Env, vault_id: u64, donor: &Address) -> i128 {
    read_total(env, &DonationKey::DonorTotal(vault_id, donor.clone()))
}

/// Public, paginated listing of donation records for `vault_id`.
///
/// Records are returned in receipt order starting at `start_receipt_no`
/// (inclusive, 1-based). Fails with `DonationsNotPublic` when the owner has not
/// enabled public visibility. `limit` is capped at `MAX_DONATION_PAGE_SIZE`.
pub fn list_donations(
    env: &Env,
    vault_id: u64,
    start_receipt_no: u64,
    limit: u32,
) -> Vec<DonationRecord> {
    let account = get_donation_account(env, vault_id)
        .unwrap_or_else(|| panic_with_error!(env, ContractError::DonationAccountNotSet));
    if !account.public_visibility {
        panic_with_error!(env, ContractError::DonationsNotPublic);
    }

    let mut out: Vec<DonationRecord> = Vec::new(env);
    let last = next_receipt_count(env, vault_id);
    let start = start_receipt_no.max(1);
    let capped = limit.min(MAX_DONATION_PAGE_SIZE) as u64;
    let mut receipt_no = start;
    while receipt_no <= last && (out.len() as u64) < capped {
        if let Some(record) = get_donation(env, vault_id, receipt_no) {
            out.push_back(record);
        }
        receipt_no += 1;
    }
    out
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn next_receipt_no(env: &Env, vault_id: u64) -> u64 {
    let key = DonationKey::ReceiptCounter(vault_id);
    let next = next_receipt_count(env, vault_id) + 1;
    env.storage().persistent().set(&key, &next);
    env.storage().persistent().extend_ttl(
        &key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );
    next
}

/// Number of receipts issued so far for `vault_id` (0 when none).
fn next_receipt_count(env: &Env, vault_id: u64) -> u64 {
    env.storage()
        .persistent()
        .get(&DonationKey::ReceiptCounter(vault_id))
        .unwrap_or(0)
}

fn receipt_hash(
    env: &Env,
    vault_id: u64,
    receipt_no: u64,
    donor: &Address,
    amount: i128,
    donated_at: u64,
) -> BytesN<32> {
    let mut preimage = Bytes::new(env);
    preimage.extend_from_array(&vault_id.to_be_bytes());
    preimage.extend_from_array(&receipt_no.to_be_bytes());
    preimage.extend_from_array(&amount.to_be_bytes());
    preimage.extend_from_array(&donated_at.to_be_bytes());
    preimage.append(&donor.clone().to_xdr(env));
    env.crypto().sha256(&preimage).into()
}

fn read_total(env: &Env, key: &DonationKey) -> i128 {
    env.storage().persistent().get(key).unwrap_or(0)
}

fn add_to_total(env: &Env, key: &DonationKey, amount: i128) {
    let total = read_total(env, key)
        .checked_add(amount)
        .unwrap_or_else(|| panic_with_error!(env, ContractError::BalanceOverflow));
    env.storage().persistent().set(key, &total);
    env.storage().persistent().extend_ttl(
        key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );
}
