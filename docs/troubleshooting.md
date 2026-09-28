# Troubleshooting Guide

How to diagnose and fix the problems people most often hit when building on,
deploying or operating Ethos-Protocol: contract errors, deployment script
failures, backend startup problems, API errors, webhooks, passkeys and ZK
proofs.

Start with the [diagnosis tree](#1-diagnosis-tree), jump to the matching
section, and use the log examples to confirm you are looking at the right
problem.

| Related document | Covers |
|---|---|
| [api-documentation.md](api-documentation.md) | HTTP endpoints and the full API error-code table. |
| [deployment-runbook.md](deployment-runbook.md) | Standard deploy, verification and rollback steps. |
| [zk-proof-verification-guide.md](zk-proof-verification-guide.md) | Groth16 proof format and verification. |
| [log-format.md](log-format.md) | Structured log line format. |
| [runbook-alerts.md](runbook-alerts.md) | What to do when a monitoring alert fires. |
| [incident-response.md](incident-response.md) | Escalation when a problem is user-impacting. |

This guide is checked by
[`scripts/test_troubleshooting_guide.py`](../scripts/test_troubleshooting_guide.py):
every contract error code must match its Rust enum, every API error code and
log signature must exist in the source, every log example must follow the
documented log format, and every diagnosis-tree link must resolve to a
section of this guide.

---

## Contents

1. [Diagnosis tree](#1-diagnosis-tree)
2. [Reading errors](#2-reading-errors)
3. [Contract errors: `ttl_vault`](#3-contract-errors-ttl_vault)
4. [Contract errors: `zk_verifier`](#4-contract-errors-zk_verifier)
5. [Contract errors: `sbt`](#5-contract-errors-sbt)
6. [Deployment script problems](#6-deployment-script-problems)
7. [Backend startup problems](#7-backend-startup-problems)
8. [API errors](#8-api-errors)
9. [Webhook problems](#9-webhook-problems)
10. [Passkey / WebAuthn problems](#10-passkey--webauthn-problems)
11. [ZK proof problems](#11-zk-proof-problems)
12. [Log signature reference](#12-log-signature-reference)
13. [Collecting diagnostics for a bug report](#13-collecting-diagnostics-for-a-bug-report)

---

## 1. Diagnosis tree

```text
Where did the failure appear?
│
├─ A Soroban transaction / contract call failed
│   ├─ Message contains "Error(Contract, #N)"
│   │   ├─ Calling ttl_vault ....................... → §3  (look up N)
│   │   ├─ Calling zk_verifier ..................... → §4  (look up N)
│   │   └─ Calling sbt ............................. → §5  (look up N)
│   ├─ Message contains "Error(Auth, ...)" ......... → §2.2 (missing signature)
│   └─ Message contains "Error(Budget, ...)" ....... → §2.3 (resource limits)
│
├─ A deploy script (scripts/deploy_*.sh, build.sh) failed
│   ├─ Exited before deploying ..................... → §6.1 – §6.3
│   ├─ Deployed but environments.toml wrong ........ → §6.4
│   └─ Re-run skipped a step you expected .......... → §6.5
│
├─ The backend process will not start / keeps restarting
│   ├─ Log: "Contract version check failed" ........ → §7.1
│   ├─ Panic: "failed to open db" / "migration failed" → §7.2
│   ├─ Panic: "failed to build RPC connection pool" → §7.3
│   └─ Bind error on port 3000 ..................... → §7.4
│
├─ An HTTP request returned an error
│   ├─ 401 ......................................... → §8.1
│   ├─ 404 ......................................... → §8.2
│   ├─ 405 ......................................... → §8.3
│   ├─ 422 ......................................... → §8.4
│   ├─ 429 ......................................... → §8.5
│   ├─ 500 ......................................... → §8.6
│   └─ 503 ......................................... → §8.7
│
├─ Webhooks not arriving / signature fails ......... → §9
├─ Passkey registration or login fails ............. → §10
└─ ZK claim does not verify ......................... → §11
```

---

## 2. Reading errors

### 2.1 Contract errors

Soroban reports a contract-defined error as `Error(Contract, #N)`, where `N`
is the `#[repr(u32)]` discriminant of the contract's error enum. **The same
number means different things in different contracts** — `#1` is
`AlreadyInitialized` in `ttl_vault` but `EmptyProof` in `zk_verifier`.
Always check which contract id the failing call targeted.

```log
2026-09-27T10:04:11Z ERROR soroban_rpc: transaction simulation failed contract="CBQHNAXSI55GX2GN6D67GK7BHVPSLJUGZQEU7WJ5LKR5PNUCGLIMAO4K" fn="check_in" error="HostError: Error(Contract, #6)"
```

→ `ttl_vault` error `#6` = `NotOwner` (see §3).

### 2.2 `Error(Auth, ...)`

The call needs a signature from an address that did not sign. Typical
causes: invoking an admin function (`pause`, `upgrade`, `register_oracle`)
with `--source` set to a non-admin identity, or a vault owner function
signed by a different account. Fix: pass `--source` as the identity whose
address the function authorizes (`stellar keys address <identity>` to
compare).

### 2.3 `Error(Budget, ...)`

The transaction exceeded CPU/memory limits. Usual culprits are very large
batches (batch check-in, `verify_credentials_consistent`) or long credential
chains. Split the batch, or check the limits in
[wasm-size-budget.md](wasm-size-budget.md) and
[performance-optimization-runbook.md](performance-optimization-runbook.md).

---

## 3. Contract errors: `ttl_vault`

The most common `ContractError` values. The full enum is in
`contracts/ttl_vault/src/lib.rs`.

<!-- errors:ttl_vault:start -->
| # | Error | Likely cause | Solution |
|---:|---|---|---|
| 1 | `AlreadyInitialized` | `initialize` called on a contract that is already set up (often a re-run of a deploy). | Nothing to do; verify with `get_admin`. Never re-initialize. |
| 2 | `InvalidInterval` | Check-in interval is zero or malformed. | Pass a positive interval in seconds. |
| 3 | `VaultNotFound` | Wrong `vault_id`, or wrong contract id / network. | Confirm the id and that `CONTRACT_TTL_VAULT` matches `environments.toml`. |
| 4 | `EmptyVault` | Release/withdraw on a vault with zero balance. | Deposit first, or skip the action. |
| 5 | `InvalidAmount` | Amount is zero or negative. | Send a positive amount in stroops. |
| 6 | `NotOwner` | Caller is not the vault owner. | Sign with the owner's identity. |
| 7 | `AlreadyReleased` | Vault already released to beneficiaries. | Terminal state; create a new vault. |
| 8 | `InsufficientBalance` | Withdrawal exceeds balance. | Query the balance and reduce the amount. |
| 9 | `NotAdmin` | Admin-only function called by non-admin. | Use the admin identity (`get_admin`). |
| 10 | `Paused` | Contract is paused (incident or maintenance). | Check `get_pause_record`; wait for `unpause`. |
| 12 | `InvalidBps` | Beneficiary basis points don't sum to 10000 or exceed it. | Make shares sum to exactly 10000 bps. |
| 14 | `IntervalTooLow` | Check-in interval below the minimum. | Increase the interval. |
| 15 | `IntervalTooHigh` | Check-in interval above the maximum. | Decrease the interval. |
| 16 | `NotExpired` | Release attempted before the TTL elapsed. | Wait; use `GET /api/vaults/:vault_id/simulate-release` to see when. |
| 17 | `InvalidBeneficiary` | Beneficiary is the owner, duplicated, or malformed. | Fix the beneficiary list. |
| 19 | `VaultExpired` | Owner action (e.g. check-in) after expiry. | Vault can only be released now. |
| 21 | `NotInitialized` | Contract deployed but `initialize` never ran. | Run the initialize step of the runbook (§3.4). |
| 25 | `MaxTtlExceeded` | Requested TTL above the protocol maximum. | Shorten the TTL. |
| 37 | `UpgradeInvalidHash` | `upgrade` given an all-zero or unknown WASM hash. | Upload the WASM first (`stellar contract upload`) and pass its hash. |
| 38 | `DepositLimitExceeded` | Deposit above the configured limit. | Split the deposit or ask the admin to raise the limit. |
| 49 | `VaultCapacityExceeded` | Vault reached its capacity cap. | Use another vault or raise the cap. |
| 57 | `DuplicateVault` | Identical vault already exists for this owner. | Reuse the existing vault id. |
| 58 | `CheckInTooFrequent` | Check-ins rate-limited. | Wait before checking in again. |
| 59 | `VaultFrozen` | Vault frozen by compliance controls. | Contact compliance; see [vault-compliance-controls.md](vault-compliance-controls.md). |
<!-- errors:ttl_vault:end -->

---

## 4. Contract errors: `zk_verifier`

<!-- errors:zk_verifier:start -->
| # | Error | Likely cause | Solution |
|---:|---|---|---|
| 1 | `EmptyProof` | `proof` bytes empty. | Send the encoded proof (256 bytes for Groth16). |
| 2 | `EmptyClaim` | `claim` bytes empty. | Send the encoded public inputs. |
| 3 | `ProofTooLarge` | Proof > `MAX_PROOF_SIZE` (4096 bytes). | Check you are not sending hex text or JSON instead of raw bytes. |
| 4 | `ClaimTooLarge` | Claim > `MAX_CLAIM_SIZE` (1024 bytes, i.e. > 32 public inputs). | Hash large public data into one input. |
| 5 | `AlreadyInitialized` | `initialize` called twice. | Nothing to do. |
| 6 | `NotInitialized` | Admin function before `initialize`. | Initialize the contract. |
| 7 | `OracleNotFound` | `attest` from an address that is not a registered oracle (or was revoked). | Admin must `register_oracle`; check `is_oracle`. |
| 8 | `MalformedConditionalProof` | `verify_conditional_proof` input isn't a valid XDR `ConditionalProof`. | Re-encode with the contract's `ConditionalProof` type. |
| 9 | `BatchConsistencyError` | Credentials in a batch conflict. | Inspect each pair individually. |
| 10 | `EmptyBatchIds` | Batch called with no ids. | Pass at least one id. |
| 11 | `MismatchedBatchLengths` | `proofs` and `claims` differ in length. | Pair every proof with exactly one claim. |
| 12 | `CredentialNotFound` | Parent credential was never attested. | Attest the parent first. |
| 13 | `SelfReferentialParent` | Derived credential would be its own parent. | Use a different `(proof, claim)` or parent. |
| 14 | `ParentAlreadySet` | This pair was already derived from a different parent. | A credential's parent is fixed; attest a new pair. |
| 15 | `CredentialChainTooDeep` | Ancestor chain > `MAX_CREDENTIAL_CHAIN_DEPTH` (32). | Flatten the hierarchy. |
| 16 | `ParentCredentialInvalid` | An ancestor was invalidated by an upheld dispute. | Resolve/replace the invalid ancestor. |
| 17 | `InvalidLatticeProof` | Missing `LATTICE_V1` header or bad checksum. | See the lattice format in [zk-verifier.md](zk-verifier.md). |
| 18 | `ExternalFormatTooLarge` | Exported/masked proof > 8192 bytes. | Reduce proof size. |
| 19 | `InvalidMaskSpec` | Empty or out-of-range `fields_to_mask`. | Pass valid field indices. |
| 20 | `MaskedVerificationFailed` | Masked proof not attested by a registered oracle for this claim. | Have an oracle attest it. |
| 21 | `AccessDenied` | Caller may not view this credential at its privacy level. | Use an authorized requester or ask the admin to change the level. |
<!-- errors:zk_verifier:end -->

> `verify_claim` returning **`false`** is not an error — see §11.

---

## 5. Contract errors: `sbt`

<!-- errors:sbt:start -->
| # | Error | Likely cause | Solution |
|---:|---|---|---|
| 1 | `AlreadyInitialized` | `initialize` called twice. | Nothing to do. |
| 2 | `NotInitialized` | Call before `initialize`. | Initialize the contract. |
| 3 | `TokenNotFound` | Unknown token id. | Check the id / contract. |
| 4 | `EmptyMetadata` | Mint/update with empty metadata. | Provide metadata. |
| 5 | `AlreadyComposed` | Token already part of a composition. | Decompose first. |
| 6 | `NotComposed` | Decompose on a token that is not composed. | Nothing to decompose. |
| 7 | `NftOwnershipMismatch` | Caller does not own the referenced NFT. | Sign with the owner. |
| 8 | `InvalidDuration` | Zero or out-of-range delegation duration. | Pass a positive duration. |
| 9 | `NoActiveDelegation` | Revoke/use of a delegation that doesn't exist. | Check delegations first. |
| 10 | `MismatchedBatchLengths` | Batch argument lists differ in length. | Align the lists. |
<!-- errors:sbt:end -->

---

## 6. Deployment script problems

### 6.1 `STELLAR_MAINNET_RPC_URL must be set`

```log
./scripts/deploy_mainnet.sh: line 12: STELLAR_MAINNET_RPC_URL: STELLAR_MAINNET_RPC_URL must be set
```

**Cause:** `deploy_mainnet.sh` requires the RPC URL. **Fix:**
`export STELLAR_MAINNET_RPC_URL="https://soroban-rpc.mainnet.stellar.org"`.

### 6.2 `Aborted.` / `Force redeploy not confirmed. Aborted.`

**Cause:** the typed confirmation did not match. Mainnet requires typing
`mainnet`; `--force` requires `FORCE-REDEPLOY-<NETWORK>` exactly
(upper-case network). **Fix:** re-run and type the phrase exactly — or drop
`--force` if you did not mean to redeploy.

### 6.3 Toolchain warning or build failure

```log
Warning: rustc 1.97.0 is active, but this project pins 1.96.1.
```

**Cause:** local toolchain differs from the pin, so WASM hashes will not
match CI. **Fix:** `rustup install 1.96.1 && rustup override set 1.96.1`.
If `cargo build --target wasm32-unknown-unknown` fails with
"can't find crate for `core`", run
`rustup target add wasm32-unknown-unknown`.

### 6.4 `environments.toml` not updated or wrong network updated

**Cause:** `contract_ttl_vault` is not the first key under the network
section — the scripts parse it positionally. **Fix:** move
`contract_ttl_vault` directly under the `[network]` header and re-run.

### 6.5 "Contract already deployed ... Skipping deploy step."

```log
✓ Contract already deployed to testnet (see marker). Skipping deploy step.
```

**Cause:** `environments.toml` holds a real id or
`.deploy-state/<network>.state` contains `contract_deployed`. This is the
idempotency guard working as intended. **Fix:** if you really want a new
contract, pass `--force`; for a clean testnet slate, delete
`.deploy-state/testnet.state` (see
[deployment-runbook.md](deployment-runbook.md) §6.5).

---

## 7. Backend startup problems

### 7.1 Contract version check failed

```log
2026-09-27T10:00:00Z ERROR ethos_protocol_backend: Contract version check failed: Unable to reach contract to verify version: connection refused
```

**Cause:** the backend could not read the on-chain contract version, or it
is below `MIN_CONTRACT_VERSION`; the process exits with status 1.
**Fix:** check the RPC URL and network, confirm the contract is deployed
and upgraded, and that `MIN_CONTRACT_VERSION` is not set higher than the
deployed version. Deploy contracts **before** the backend.

### 7.2 `failed to open db` / `migration failed`

**Cause:** the database pool could not be created or a migration failed
(bad schema state, disk full, permissions). **Fix:** check pool env vars
(see [configuration-reference.md](configuration-reference.md)), free disk
space, and inspect the migration error in the panic message; for broken
migrations see [migration-testing.md](migration-testing.md).

### 7.3 `failed to build RPC connection pool`

**Cause:** invalid RPC pool configuration (e.g. zero or non-numeric
timeouts). **Fix:** correct the RPC pool env vars and restart.

### 7.4 Port 3000 already in use

```log
thread 'main' panicked at backend/src/main.rs: called `Result::unwrap()` on an `Err` value: Os { code: 98, kind: AddrInUse, message: "Address already in use" }
```

**Fix:** stop the other process (`lsof -i :3000`) or the old container
(`docker compose down`).

### 7.5 Healthy startup (for comparison)

```log
2026-09-27T10:00:00Z INFO ethos_protocol_backend: Contract version 1 (minimum required: 1)
2026-09-27T10:00:00Z INFO ethos_protocol_backend: RPC connection pool initialized max_idle_per_host=32 idle_timeout_secs=90
2026-09-27T10:00:01Z INFO ethos_protocol_backend: consensus cache initialized node_id="backend-primary" strategy=LastWriteWins
2026-09-27T10:00:01Z INFO ethos_protocol_backend: listening on 0.0.0.0:3000
```

---

## 8. API errors

Error bodies use either `code` or `error` as the key — see
[api-documentation.md §14](api-documentation.md#14-error-codes).

### 8.1 401 `unauthorized` / `two_factor_required`

* `unauthorized` — admin route without `Authorization: Bearer <ADMIN_API_KEY>`,
  or the key is wrong. Check for trailing whitespace/newlines in the env var.
* `two_factor_required` — retry with a TOTP code.
* `POST /webhooks/verify` returns `401` with `"valid": false` when the
  signature does not match — see §9.

### 8.2 404 `not_found`

Wrong id, or the resource was soft-deleted
(`GET /api/vaults/:vault_id/reminders?include_deleted=true` shows deleted
reminder records). For `/capabilities/:name/fallback`, `404` means the
capability is healthy (`full`) and no fallback is needed.

### 8.3 405 `method_not_allowed`

The path is declared in `docs/openapi.yaml` but not for this method. The
`Allow` response header lists valid methods. Common case: `PUT` instead of
`POST` on `reminder-preferences`.

### 8.4 422 `invalid_input`

Validation failed; `message` names the field.

```json
{ "code": "invalid_input", "message": "invalid input: channels must not be empty", "details": null }
```

Frequent causes: empty `channels`, `hours_before_expiry` of `0`, an unknown
enum value (`"frequency": "yearly"`), or no valid `scenarios` for
`simulate-release`.

### 8.5 429 `rate_limit_exceeded` / `priority_limit_exceeded` / `too_many_requests`

```json
{ "error": "rate_limit_exceeded", "message": "rate limit exceeded", "retry_after_secs": 12 }
```

**Fix:** wait `Retry-After` seconds. Unauthenticated callers get the
strictest tier — send `Authorization` (and `X-User-Tier` if applicable).
Behind a proxy, make sure `X-Forwarded-For` carries the real client IP or all
clients will share one bucket.

### 8.6 500 `internal_error`

Database or unexpected server failure. Check `/ready`, backend logs, and DB
health; retry with backoff.

### 8.7 503 `load_shed` or `/ready` failing

```json
{ "code": "load_shed", "message": "request shed due to high load", "priority": "low" }
```

**Cause:** the server is shedding load (lowest `X-Priority` first). **Fix:**
retry with backoff; for critical traffic send `X-Priority: high`. If `/ready`
returns `503`, the database is unreachable — see §7.2.
See [load-shedding.md](load-shedding.md).

---

## 9. Webhook problems

| Symptom | Cause | Fix |
|---|---|---|
| No deliveries at all | Registration filtered by `vault_id` or `event_types` that never fire; or `active: false`. | `GET /webhooks` and check filters. |
| `"reason": "signature mismatch"` | Receiver computed HMAC over re-serialized JSON, wrong secret, or wrong algorithm. | HMAC the **raw** body bytes with the registered secret and the algorithm in the header prefix. |
| `"reason": "missing X-Ethos-Signature header"` | Proxy stripped custom headers. | Allow `X-Ethos-*` headers through the proxy. |
| `"reason": "malformed signature header (expected '<alg>=<hex>')"` | Header value lacks the `sha256=` prefix. | Pass the header value unchanged. |
| `timestamp out of tolerance` | Receiver clock skew > 300 s, or a replayed delivery. | Sync clocks with NTP; reject genuinely old deliveries. |

---

## 10. Passkey / WebAuthn problems

| Error (`{"error": ...}`) | Cause | Fix |
|---|---|---|
| `unknown or expired session` | `session_id` from *begin* is wrong, reused or expired. | Restart the ceremony with a fresh *begin* call. |
| `registration challenge expired` | User took too long between *begin* and *complete*. | Restart the ceremony. |
| `invalid credential_id encoding` | Standard base64 sent instead of base64url. | Encode with base64url, no padding. |
| `client_data_json is not valid JSON` | Wrong field encoded, or double-encoded. | Base64url-encode the raw `clientDataJSON` bytes once. |
| `user_id must not be empty` | Missing `user_id`. | Send it. |

Also check that the relying-party id matches the page origin — see
[webauthn-setup.md](webauthn-setup.md).

---

## 11. ZK proof problems

`verify_claim` returns `false` (no panic) when the pair was never attested,
the attesting oracle was revoked, or the credential was invalidated by a
dispute. Work through this list:

1. **Was it attested?** The oracle must call `attest` with the *exact* same
   `proof` and `claim` bytes. Any byte difference (hex vs raw, trailing
   newline, different public-input order) yields a different SHA-256 digest.
2. **Is the oracle still registered?** `is_oracle(oracle)` must be `true`.
3. **Was it invalidated?** `is_credential_invalidated(credential_id)`.
4. **Off-chain verification failing before attestation?** The usual causes
   are the `𝔾₂` coordinate order (`c1` before `c0`), public inputs `≥ r`, a
   verifying key from a different circuit, or public inputs in the wrong
   order. See [zk-proof-verification-guide.md §8](zk-proof-verification-guide.md#8-proof-and-verifying-key-byte-format).

```log
2026-09-27T10:12:40Z WARN oracle: groth16 verification failed circuit="age_over_18" public_inputs=1 reason="pairing check failed"
```

---

## 12. Log signature reference

Exact strings to search for in logs, and where they come from.

<!-- log-signatures:start -->
| Log text | Source file | Section |
|---|---|---|
| `Contract version check failed` | `backend/src/main.rs` | §7.1 |
| `Unable to reach contract to verify version` | `backend/src/contract_version_check.rs` | §7.1 |
| `failed to open db` | `backend/src/main.rs` | §7.2 |
| `migration failed` | `backend/src/main.rs` | §7.2 |
| `failed to build RPC connection pool` | `backend/src/main.rs` | §7.3 |
| `listening on` | `backend/src/main.rs` | §7.5 |
| `request shed due to high load` | `backend/src/load_shedding.rs` | §8.7 |
| `priority concurrency limit exceeded` | `backend/src/load_shedding.rs` | §8.5 |
| `signature mismatch` | `backend/src/webhook.rs` | §9 |
| `timestamp out of tolerance` | `backend/src/webhook.rs` | §9 |
| `unknown or expired session` | `backend/src/webauthn.rs` | §10 |
| `registration challenge expired` | `backend/src/webauthn.rs` | §10 |
| `STELLAR_MAINNET_RPC_URL must be set` | `scripts/deploy_mainnet.sh` | §6.1 |
| `Force redeploy not confirmed. Aborted.` | `scripts/deploy_mainnet.sh` | §6.2 |
| `Skipping deploy step.` | `scripts/deploy_testnet.sh` | §6.5 |
| `but this project pins` | `scripts/build.sh` | §6.3 |
<!-- log-signatures:end -->

---

## 13. Collecting diagnostics for a bug report

```bash
git rev-parse HEAD
rustc --version && stellar --version
cat target/wasm-hashes.txt
cat .deploy-state/*.state
curl -s http://localhost:3000/health
curl -s http://localhost:3000/ready
docker compose logs --since 30m backend > backend.log
```

Include: the failing command or request (with secrets redacted), the full
error message including any `Error(Contract, #N)`, the contract id and
network, and the output above. Never paste secret keys, `ADMIN_API_KEY` or
webhook secrets — see [SECURITY.md](../SECURITY.md) for reporting
vulnerabilities privately.
