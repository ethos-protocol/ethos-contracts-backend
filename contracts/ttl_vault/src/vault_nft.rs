//! Issue #614 — Add Vault NFT Minting
//!
//! Vaults are invisible to the outside world. Minting an NFT per vault gives the
//! owner a tradeable, on-chain proof of the vault they hold.
//!
//! Provides:
//! - NFT minting on vault creation (one token per vault, owned by the vault owner)
//! - Ownership tracking: the current holder is updated on every secondary sale
//! - Owner-managed metadata storage (bounded length)
//! - Secondary sales: the buyer pays the current holder in the vault's token and
//!   the NFT changes hands, with each sale recorded
//! - Paginated sales history and audit events
//!
//! The NFT is a proof and a tradeable claim to the vault's record. It does not
//! grant control over the vault: withdrawals, check-ins and governance remain
//! with the vault owner recorded in the vault itself.

use soroban_sdk::{
    contracttype, panic_with_error, symbol_short, token, Address, Env, String, Symbol, Vec,
};

use crate::ContractError;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum byte length of NFT metadata.
pub const MAX_NFT_METADATA_LEN: u32 = 256;

/// Upper bound on the number of sale records returned by one listing call.
pub const MAX_NFT_SALES_PAGE_SIZE: u32 = 50;

// ── Event Topics ─────────────────────────────────────────────────────────────

pub const NFT_MINTED_TOPIC: Symbol = symbol_short!("nft_mint");
pub const NFT_METADATA_SET_TOPIC: Symbol = symbol_short!("nft_meta");
pub const NFT_SOLD_TOPIC: Symbol = symbol_short!("nft_sold");

// ── Storage Keys ─────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
pub enum VaultNftKey {
    /// Last token id issued; ids are 1-based and monotonic across all vaults.
    Counter,
    /// NFT record for a vault: vault_id -> VaultNft
    ByVault(u64),
    /// Reverse lookup from token id to vault: token_id -> vault_id
    VaultOfToken(u64),
    /// Number of secondary sales recorded for a token: token_id -> u64
    SaleCount(u64),
    /// Sale record: (token_id, sale_no) -> NftSale, with sale_no 1-based
    Sale(u64, u64),
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// The NFT attached to a vault.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VaultNft {
    pub token_id: u64,
    pub vault_id: u64,
    /// Original minter (the vault owner at creation).
    pub minter: Address,
    /// Current holder. Changes on every secondary sale.
    pub owner: Address,
    /// Owner-managed metadata (label, IPFS hash, or similar).
    pub metadata: String,
    pub minted_at: u64,
    /// Price paid by the most recent sale, if any.
    pub last_sale_price: Option<i128>,
}

/// A recorded secondary sale of a vault NFT.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct NftSale {
    pub token_id: u64,
    pub sale_no: u64,
    pub seller: Address,
    pub buyer: Address,
    pub price: i128,
    pub sold_at: u64,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct NftMintedEvent {
    pub token_id: u64,
    pub vault_id: u64,
    pub owner: Address,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct NftSoldEvent {
    pub token_id: u64,
    pub sale_no: u64,
    pub seller: Address,
    pub buyer: Address,
    pub price: i128,
}

// ── Minting ───────────────────────────────────────────────────────────────────

/// Mint the NFT for a freshly created vault and return its token id.
///
/// Called from `create_vault` once the vault is stored. The vault owner is the
/// initial holder. Metadata starts empty and can be set by the holder.
pub fn mint_for_vault(env: &Env, vault_id: u64, owner: &Address) -> u64 {
    let token_id = next_token_id(env);
    let nft = VaultNft {
        token_id,
        vault_id,
        minter: owner.clone(),
        owner: owner.clone(),
        metadata: String::from_str(env, ""),
        minted_at: env.ledger().timestamp(),
        last_sale_price: None,
    };
    store_nft(env, &nft);
    env.storage()
        .persistent()
        .set(&VaultNftKey::VaultOfToken(token_id), &vault_id);
    extend(env, &VaultNftKey::VaultOfToken(token_id));

    env.events().publish(
        (NFT_MINTED_TOPIC, vault_id),
        NftMintedEvent {
            token_id,
            vault_id,
            owner: owner.clone(),
        },
    );
    token_id
}

// ── Queries ───────────────────────────────────────────────────────────────────

/// The NFT attached to `vault_id`, if one was minted.
pub fn get_vault_nft(env: &Env, vault_id: u64) -> Option<VaultNft> {
    env.storage()
        .persistent()
        .get(&VaultNftKey::ByVault(vault_id))
}

/// The NFT with `token_id`, if it exists.
pub fn get_nft_by_token(env: &Env, token_id: u64) -> Option<VaultNft> {
    let vault_id: u64 = env
        .storage()
        .persistent()
        .get(&VaultNftKey::VaultOfToken(token_id))?;
    get_vault_nft(env, vault_id)
}

/// Current holder of the NFT for `vault_id`.
pub fn get_nft_owner(env: &Env, vault_id: u64) -> Option<Address> {
    get_vault_nft(env, vault_id).map(|nft| nft.owner)
}

/// Paginated sales history for `token_id`, starting at `start_sale_no`
/// (1-based, inclusive). `limit` is capped at `MAX_NFT_SALES_PAGE_SIZE`.
pub fn list_nft_sales(env: &Env, token_id: u64, start_sale_no: u64, limit: u32) -> Vec<NftSale> {
    let mut out: Vec<NftSale> = Vec::new(env);
    let last = sale_count(env, token_id);
    let capped = limit.min(MAX_NFT_SALES_PAGE_SIZE) as u64;
    let mut sale_no = start_sale_no.max(1);
    while sale_no <= last && (out.len() as u64) < capped {
        if let Some(sale) = env
            .storage()
            .persistent()
            .get(&VaultNftKey::Sale(token_id, sale_no))
        {
            out.push_back(sale);
        }
        sale_no += 1;
    }
    out
}

// ── Mutations ─────────────────────────────────────────────────────────────────

/// Set the metadata of the NFT for `vault_id`. Authorization is enforced by the
/// caller, who must be the current NFT holder.
pub fn set_nft_metadata(env: &Env, vault_id: u64, metadata: String) {
    if metadata.len() > MAX_NFT_METADATA_LEN {
        panic_with_error!(env, ContractError::NftMetadataTooLong);
    }
    let mut nft = get_vault_nft(env, vault_id)
        .unwrap_or_else(|| panic_with_error!(env, ContractError::NftNotFound));
    nft.metadata = metadata;
    store_nft(env, &nft);

    env.events()
        .publish((NFT_METADATA_SET_TOPIC, vault_id), nft.token_id);
}

/// Buy the NFT `token_id` from its current holder for `price` of `token_address`.
///
/// Funds move directly from the buyer to the seller. The buyer becomes the new
/// holder and the sale is recorded. Authorization is enforced by the caller
/// (the buyer).
pub fn buy_nft(
    env: &Env,
    token_id: u64,
    buyer: &Address,
    token_address: &Address,
    price: i128,
) -> NftSale {
    if price <= 0 {
        panic_with_error!(env, ContractError::InvalidAmount);
    }
    let mut nft = get_nft_by_token(env, token_id)
        .unwrap_or_else(|| panic_with_error!(env, ContractError::NftNotFound));
    if nft.owner == *buyer {
        panic_with_error!(env, ContractError::CannotBuyOwnNft);
    }

    let seller = nft.owner.clone();
    token::Client::new(env, token_address).transfer(buyer, &seller, &price);

    let sale_no = sale_count(env, token_id) + 1;
    let sale = NftSale {
        token_id,
        sale_no,
        seller: seller.clone(),
        buyer: buyer.clone(),
        price,
        sold_at: env.ledger().timestamp(),
    };
    let sale_key = VaultNftKey::Sale(token_id, sale_no);
    env.storage().persistent().set(&sale_key, &sale);
    extend(env, &sale_key);

    let count_key = VaultNftKey::SaleCount(token_id);
    env.storage().persistent().set(&count_key, &sale_no);
    extend(env, &count_key);

    nft.owner = buyer.clone();
    nft.last_sale_price = Some(price);
    store_nft(env, &nft);

    env.events().publish(
        (NFT_SOLD_TOPIC, nft.vault_id),
        NftSoldEvent {
            token_id,
            sale_no,
            seller,
            buyer: buyer.clone(),
            price,
        },
    );
    sale
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn next_token_id(env: &Env) -> u64 {
    let next = env
        .storage()
        .persistent()
        .get::<_, u64>(&VaultNftKey::Counter)
        .unwrap_or(0)
        + 1;
    env.storage().persistent().set(&VaultNftKey::Counter, &next);
    extend(env, &VaultNftKey::Counter);
    next
}

fn sale_count(env: &Env, token_id: u64) -> u64 {
    env.storage()
        .persistent()
        .get(&VaultNftKey::SaleCount(token_id))
        .unwrap_or(0)
}

fn store_nft(env: &Env, nft: &VaultNft) {
    let key = VaultNftKey::ByVault(nft.vault_id);
    env.storage().persistent().set(&key, nft);
    extend(env, &key);
}

fn extend(env: &Env, key: &VaultNftKey) {
    env.storage().persistent().extend_ttl(
        key,
        crate::VAULT_TTL_THRESHOLD,
        crate::VAULT_TTL_LEDGERS,
    );
}
