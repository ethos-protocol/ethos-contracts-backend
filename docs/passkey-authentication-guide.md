# Passkey Authentication Guide

**Issue**: #578  
**Status**: Implemented  
**Last Updated**: 2026-09-28

## Overview

Ethos-Protocol uses Passkeys (WebAuthn) to authenticate vault owners without seed phrases. This guide explains the WebAuthn flow, the challenge-response mechanism, the on-chain security model, and how to integrate passkey authentication into a client application.

## Why Passkeys?

Traditional Stellar authentication requires a raw ed25519 private key. Passkeys replace the key with a hardware-backed credential stored on the user's device (Secure Enclave, TPM, or FIDO2 hardware key):

| Property | Seed Phrase | Passkey |
|----------|-------------|---------|
| Storage | User-managed (paper, password manager) | Device hardware |
| Phishing resistance | None — phrase can be copied | High — credential bound to origin |
| Loss recovery | Re-enter phrase | Social recovery / backup codes |
| Biometrics | Not applicable | Fingerprint, Face ID |
| On-chain identifier | Full public key | `BytesN<32>` hash commitment |

## WebAuthn Flow

### Registration

```
Client                          Authenticator              Contract
  │                                   │                       │
  │── navigator.credentials.create ──►│                       │
  │   (challenge, rpId, userId)        │                       │
  │                                   │── user gesture ──────►│
  │                                   │   (biometric/PIN)     │
  │◄── credential (publicKey, id) ────│                       │
  │                                   │                       │
  │── hash(publicKey) ───────────────────────────────────────►│
  │                                   │          add_passkey(vault_id, owner, hash)
  │                                   │                       │
  │◄─────────────────────────────── Ok(()) ──────────────────│
```

1. The client calls `navigator.credentials.create()` with a server-generated challenge.
2. The authenticator performs a biometric check and generates a new key pair.
3. The public key is returned to the client.
4. The client computes `SHA-256(public_key_bytes)` to produce a 32-byte `passkey_hash`.
5. The client calls `add_passkey(vault_id, owner, passkey_hash)` on the contract.
6. The contract stores the hash; the raw public key never touches the chain.

### Authentication (Check-In)

```
Client                          Authenticator              Contract
  │                                   │                       │
  │── navigator.credentials.get ─────►│                       │
  │   (challenge, rpId, allowList)    │                       │
  │                                   │── user gesture ──────►│
  │                                   │   (biometric/PIN)     │
  │◄── assertion (signature, hash) ───│                       │
  │                                   │                       │
  │── check_in(vault_id, owner, ─────────────────────────────►│
  │           passkey_hash, nonce)    │    verify hash in VaultPasskeys
  │                                   │    verify nonce not replayed
  │◄─────────────────────────────── Ok(()) ──────────────────│
```

1. The client calls `navigator.credentials.get()` with a fresh challenge.
2. The authenticator signs the challenge with the stored private key.
3. The client extracts the `credentialId` and maps it to the stored `passkey_hash`.
4. The client calls `check_in(vault_id, owner, passkey_hash, nonce)`.
5. The contract verifies the hash is registered and the passkey has not expired.

## Challenge-Response Explanation

The WebAuthn challenge prevents replay attacks at the transport layer. On-chain, Ethos-Protocol adds a second layer via the `nonce` parameter:

```rust
check_in(vault_id: u64, caller: Address, passkey_hash: BytesN<32>, nonce: u64)
```

- `nonce` must be strictly increasing per vault to prevent replay of an old check-in call.
- `passkey_hash` must match a hash registered in `VaultPasskeys` (or `vault.passkey_hash` for legacy single-passkey vaults).
- If the passkey has an expiry set via `extend_passkey_expiry`, the current ledger time must be before the expiry timestamp.

## Security Properties

### Hash Commitment Model

Raw public keys are never stored on-chain. The contract stores only:

```rust
BytesN<32>  // SHA-256(webauthn_public_key_bytes)
```

This means:
- A database leak of contract storage reveals no usable credentials.
- Key rotation requires only registering a new hash and removing the old one.

### Passkey Validation Order

On every `check_in`, `check_in_with_pow`, and `batch_check_in_v2`, the contract enforces:

1. **Registration check** — hash must be in `VaultPasskeys` (or match legacy `passkey_hash`).
2. **Expiry check** — if an expiry was set, current time must be before it; otherwise `PasskeyExpired` (error 59) is returned.
3. **Compromise check** — if the passkey was flagged via `report_passkey_compromise`, `PasskeyCompromised` (error 62) is returned.

### Automatic Compromise Detection

On every check-in the contract inspects the last 5 usage entries. If 3 or more consecutive entries used **different** passkey hashes, a `pk_comp` advisory event is emitted. The check-in is not blocked — owners should monitor for this event and rotate suspected passkeys.

## Passkey Lifecycle API

### Register a Passkey

```rust
add_passkey(vault_id: u64, caller: Address, passkey_hash: BytesN<32>) -> Result<(), ContractError>
```

Registers an additional passkey for a vault. Only the vault owner can call this.

**Example**:
```rust
// Derive passkey_hash from WebAuthn public key bytes on the client
let passkey_hash = BytesN::from_array(&env, &sha256(webauthn_public_key_bytes));
client.add_passkey(&vault_id, &owner, &passkey_hash)?;
```

### Remove a Passkey

```rust
remove_passkey(vault_id: u64, caller: Address, passkey_hash: BytesN<32>) -> Result<(), ContractError>
```

Removes a registered passkey. At least one passkey must remain if authentication is required.

### Rotate a Passkey

```rust
rotate_passkey(vault_id: u64, caller: Address, old_hash: BytesN<32>, new_hash: BytesN<32>) -> Result<(), ContractError>
```

Atomically swaps an old passkey hash for a new one. Use this when a device is replaced.

### Set Passkey Expiry

```rust
extend_passkey_expiry(vault_id: u64, caller: Address, passkey_hash: BytesN<32>, new_expiry: u64) -> Result<(), ContractError>
```

Sets an expiry timestamp (Unix seconds) for a specific passkey. After this time, check-ins using that hash are rejected with `PasskeyExpired`.

### Report Compromise

```rust
report_passkey_compromise(vault_id: u64, caller: Address, passkey_hash: BytesN<32>) -> Result<(), ContractError>
```

Manually flags a passkey as compromised. Blocks future check-ins with that hash.

```rust
clear_passkey_compromise(vault_id: u64, caller: Address, passkey_hash: BytesN<32>) -> Result<(), ContractError>
```

Clears a compromise flag once the owner has confirmed the credential is safe.

### Biometric Binding

```rust
bind_passkey_biometric(vault_id, caller, passkey_hash, credential_hash) -> Result<(), ContractError>
biometric_check_in(vault_id, caller, passkey_hash, credential_hash) -> Result<(), ContractError>
```

Binds a biometric credential hash to a registered passkey. The raw biometric data never leaves the device — only a `SHA-256` commitment is stored on-chain.

## Error Reference

| Code | Name | Cause |
|------|------|-------|
| 26 | `InvalidPasskey` | Hash not registered for this vault |
| 59 | `PasskeyExpired` | Passkey registration has expired |
| 62 | `PasskeyCompromised` | Passkey was flagged as compromised |

## On-Chain Log Bounds

Both the passkey usage log and the passkey audit log are **capped at 1,000 entries** per vault:

| Log | Query function | Cap |
|-----|---------------|-----|
| Usage | `get_passkey_usage(vault_id)` | 1,000 entries |
| Audit | `get_passkey_audit_log(vault_id)` | 1,000 entries |

Older entries are pruned from on-chain storage but are always available via the event stream (`pk_usage`, `pk_audit` topics) for off-chain indexers.

## Events

| Topic | Emitted when |
|-------|-------------|
| `add_pk` | Passkey registered |
| `rm_pk` | Passkey removed |
| `rot_pk` | Passkey rotated |
| `pk_exp` | Passkey expiry timestamp set |
| `pk_expwrn` | Passkey expiry is approaching |
| `pk_expd` | Expired passkey used in check-in |
| `pk_comp` | Compromise detected or reported |
| `pk_usage` | Every check-in (full event history) |
| `pk_audit` | Every passkey add/remove/use lifecycle event |
| `bind_pk` | Biometric bound to passkey |
| `bio_ci` | Biometric check-in performed |

## Code Examples

### Registering a Vault with a Passkey (Client)

```typescript
// 1. Create WebAuthn credential
const credential = await navigator.credentials.create({
  publicKey: {
    challenge: serverChallenge,
    rp: { id: "app.ethos.finance", name: "Ethos Protocol" },
    user: { id: userId, name: userEmail, displayName: userName },
    pubKeyCredParams: [{ type: "public-key", alg: -7 }], // ES256
  },
});

// 2. Derive passkey_hash = SHA-256(publicKey)
const pubKeyBytes = new Uint8Array(credential.response.getPublicKey());
const hashBuffer = await crypto.subtle.digest("SHA-256", pubKeyBytes);
const passkeyHash = new Uint8Array(hashBuffer); // 32 bytes

// 3. Call contract add_passkey
await contract.add_passkey(vaultId, ownerAddress, passkeyHash);
```

### Checking In with a Passkey (Client)

```typescript
// 1. Assert with registered credential
const assertion = await navigator.credentials.get({
  publicKey: {
    challenge: serverChallenge,
    allowCredentials: [{ id: credentialId, type: "public-key" }],
  },
});

// 2. Map credentialId → stored passkeyHash (local storage or backend)
const passkeyHash = lookupPasskeyHash(assertion.id);

// 3. Call contract check_in
const nonce = Date.now(); // or a server-issued nonce
await contract.check_in(vaultId, ownerAddress, passkeyHash, nonce);
```

### Handling Passkey Expiry (Rust)

```rust
match client.try_check_in(&vault_id, &owner, &passkey_hash, &nonce) {
    Ok(_) => { /* check-in succeeded */ }
    Err(Ok(ContractError::PasskeyExpired)) => {
        // Rotate to a fresh passkey before the vault expires
        let new_hash = generate_new_passkey_hash();
        client.rotate_passkey(&vault_id, &owner, &passkey_hash, &new_hash)?;
    }
    Err(Ok(ContractError::PasskeyCompromised)) => {
        // Use backup codes or social recovery
    }
    Err(e) => return Err(e),
}
```

## Testing

Passkey-related tests are in `contracts/ttl_vault/src/`:

- `passkey_audit_tests.rs` — audit log correctness
- `passkey_cap_tests.rs` — max passkey count enforcement
- `passkey_breach_detection_tests.rs` — automatic compromise detection
- `passkey_delegation_tests.rs` — delegated passkey usage
- `passkey_escrow_tests.rs` — passkey escrow and release
- `passkey_expiry_notification_tests.rs` — expiry warning events
- `passkey_risk_scoring_tests.rs` — risk scoring model
- `passkey_social_recovery_tests.rs` — social recovery flow

## Related Documentation

- [Passkey Integration](passkeys.md)
- [WebAuthn Setup](webauthn-setup.md)
- [Security Threat Model](security.md)
- [Vault Lifecycle](vault-lifecycle.md)
