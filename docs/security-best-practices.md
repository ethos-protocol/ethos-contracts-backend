# Security Best Practices Guide

This guide documents security best practices for developing, deploying, and operating Ethos-Protocol vaults. Follow these guidelines to minimise risk to vault owners, beneficiaries, and the protocol itself.

## Table of Contents

- [Key Management](#key-management)
- [Vault Configuration](#vault-configuration)
- [Authentication & Passkeys](#authentication--passkeys)
- [Smart Contract Security](#smart-contract-security)
- [Backend & API Security](#backend--api-security)
- [Deployment Security](#deployment-security)
- [Risk Mitigation Strategies](#risk-mitigation-strategies)
- [Threat Scenarios & Mitigations](#threat-scenarios--mitigations)
- [Incident Response](#incident-response)

---

## Key Management

### Never expose seed phrases

Ethos-Protocol is designed around Passkey (WebAuthn) authentication specifically to eliminate seed phrase exposure. Do not work around this by using raw key pairs in production.

```bash
# ❌ Bad: seed phrase stored in shell history or env vars
export VAULT_OWNER_SEED="SABC123..."

# ✅ Good: use Stellar CLI named keys stored in the OS keychain
stellar keys generate vault-owner --network testnet
```

### Separate deployment keys from operational keys

Use distinct key identities for:

| Purpose | Key Name (example) | Permissions |
|---|---|---|
| Contract deployment | `deployer` / `deployer-mainnet` | Deploy only — rotate after each deploy |
| Admin operations | `admin-ops` | Propose admin changes only |
| Monitoring (read-only) | `monitor-ro` | No signing authority |

### Rotate secrets regularly

Follow the rotation schedule defined in [`docs/secret-rotation-policy.md`](secret-rotation-policy.md):

- API keys: every 90 days
- Deploy keys: after each mainnet deployment
- Reminder service keys (`REMINDER_EMAIL_API_KEY`, `REMINDER_SMS_API_KEY`): every 180 days

### Store secrets in a secrets manager, not in files

```bash
# ❌ Bad: secrets committed to repo or stored in plain .env on server
echo "REMINDER_EMAIL_API_KEY=sk_live_..." >> .env

# ✅ Good: inject at runtime from a secrets manager (AWS SSM, Vault, etc.)
export REMINDER_EMAIL_API_KEY=$(aws ssm get-parameter --name /ethos/reminder-email-key --with-decryption --query Parameter.Value --output text)
```

Run `scripts/pre-commit-secret-scan.sh` (or install hooks via `scripts/install-hooks.sh`) to catch secrets before they reach version control.

---

## Vault Configuration

### Set a realistic check-in interval

Choose a `check_in_interval` that reflects your actual availability. An interval that is too short risks accidental TTL expiry; one that is too long reduces the protocol's responsiveness.

| Scenario | Recommended interval |
|---|---|
| Daily active user | 7–14 days |
| Periodic user | 30–60 days |
| Long-term holder | 90–180 days |

### Always designate a trusted beneficiary before depositing funds

Do not deposit assets into a vault that has no beneficiary configured. If the TTL lapses without a valid beneficiary, funds may be permanently locked.

### Verify the beneficiary address before depositing

Stellar addresses are case-sensitive. Always verify the beneficiary address on-chain before making a large deposit:

```bash
stellar contract invoke --id $CONTRACT_TTL_VAULT -- get_vault --vault_id <id>
```

Confirm the `beneficiary` field matches the intended address exactly.

### Use the minimum deposit needed for your use case

Start with a small test deposit to verify the full vault lifecycle (check-in, TTL extension, release) before committing large balances.

---

## Authentication & Passkeys

### Register at least two Passkey devices

A single device is a single point of failure. Register a primary device and a backup (e.g., a hardware security key or a second phone) so that losing one device does not lock you out.

### Never bypass WebAuthn with fallback passwords

The Passkey design eliminates shared-secret vulnerabilities. Introducing a password fallback reintroduces phishing risk. If you lose all registered devices, use the account-recovery flow documented in [`docs/webauthn-setup.md`](webauthn-setup.md).

### Keep Passkey device firmware updated

OS and browser vendors regularly patch WebAuthn implementations. Apply security updates promptly to all devices used for vault authentication.

---

## Smart Contract Security

### Always call `require_auth()` before mutating state

Every owner-only function must call `owner.require_auth()` as its first statement. Never perform state changes before authentication:

```rust
// ✅ Correct order: authenticate, then mutate
pub fn check_in(env: Env, vault_id: u64, caller: Address) {
    caller.require_auth();
    // ... extend TTL ...
}

// ❌ Incorrect: state mutated before auth check
pub fn check_in(env: Env, vault_id: u64, caller: Address) {
    update_ttl(&env, vault_id); // auth hasn't been verified yet!
    caller.require_auth();
}
```

### Validate all inputs before storage

Check bounds and invariants on every externally supplied value:

```rust
// Check interval is within acceptable range
if check_in_interval == 0 || check_in_interval > MAX_INTERVAL_SECONDS {
    return Err(ContractError::InvalidInterval);
}

// Reject owner-as-beneficiary
if beneficiary == caller {
    return Err(ContractError::InvalidBeneficiary);
}
```

### Use `ContractError` enum for all error paths

Return structured errors rather than panicking. Panics consume more compute budget and provide no actionable error code to callers.

### Keep wasm binary size within budget

Monitor contract size against the limit defined in [`docs/wasm-size-budget.md`](wasm-size-budget.md). Oversized contracts increase deployment cost and may fail to deploy.

### Test all access-control paths

Every permission boundary must have a corresponding test that verifies the rejection case:

```rust
#[test]
fn test_non_owner_cannot_check_in() {
    // ...
    let result = client.try_check_in(&vault_id, &attacker);
    assert_eq!(result, Err(Ok(ContractError::Unauthorized)));
}
```

---

## Backend & API Security

### Enforce rate limiting on all public endpoints

The rate-limiting middleware in `backend/src/rate_limit.rs` must be applied to all unauthenticated routes. Review [`docs/cache-and-rate-limiting.md`](cache-and-rate-limiting.md) for per-endpoint limits.

### Validate webhook signatures

All incoming webhooks must be verified using the signature scheme in `backend/src/webhook.rs`. Reject any request that fails signature verification before processing its payload. See [`docs/webhook-signature-verification.md`](webhook-signature-verification.md).

### Never log sensitive fields

Do not log private keys, session tokens, Passkey credential IDs, or beneficiary personal information. Use structured logging with explicit field allowlists.

```rust
// ❌ Bad: logs the full request including auth headers
tracing::debug!("Incoming request: {:?}", request);

// ✅ Good: log only safe, non-sensitive fields
tracing::info!(vault_id = %vault_id, action = "check_in", "Owner checked in");
```

### Use parameterised queries

All database access via `backend/src/db.rs` must use parameterised queries. Never interpolate user input directly into SQL strings.

---

## Deployment Security

### Confirm the target network before deploying

The `scripts/deploy_mainnet.sh` script requires you to type `mainnet` at the confirmation prompt. Never skip or automate this prompt — it is a deliberate human checkpoint.

### Pin dependency versions

All Cargo dependencies use pinned versions in `Cargo.lock`. Do not add open-range (`*`) dependencies. Run `cargo deny check` (configured in `deny.toml`) to audit for known vulnerabilities before each release.

### Review CI security gates before merging

The CI pipeline (`.github/workflows/ci.yml`) runs:

- `cargo audit` — known vulnerability scan
- `cargo deny` — licence and advisory checks
- `gitleaks` — secret scanning (`.gitleaks.toml`)
- Clippy with security lints (`.clippy.toml`)

All gates must pass before merging to `main`.

### Use Docker image digest pinning in production

Pin base images in `backend/Dockerfile` to a specific digest, not a mutable tag, to prevent supply-chain injection:

```dockerfile
# ❌ Mutable tag — can change under you
FROM rust:1.70

# ✅ Pinned digest — immutable
FROM rust@sha256:<digest>
```

---

## Risk Mitigation Strategies

### Enable check-in reminders

Configure `REMINDER_EMAIL_API_KEY` and/or `REMINDER_SMS_API_KEY` in your `.env` to receive automated reminders before your TTL lapses. The reminder service sends notifications at configurable intervals before expiry.

### Test the full release cycle on testnet first

Before depositing real funds, simulate the full lifecycle on testnet:

1. Create vault → deposit → check in → verify TTL extension
2. Let TTL lapse → call `trigger_release` → verify beneficiary receives funds
3. Test recovery from a missed check-in

### Monitor vault TTL continuously

Set up the Prometheus/Grafana stack (see [`docs/monitoring-guide.md`](monitoring-guide.md)) and configure the `vault_ttl_remaining` alert in `monitoring/alert_rules.yml` to fire before TTL reaches a critical threshold.

### Maintain an off-chain backup of vault IDs and configuration

Contract state is on-chain but your local record of vault IDs, beneficiary addresses, and check-in schedules should also be backed up off-chain. Use the automation in `scripts/backup_contract_state.sh` (see [#588](../tasks.md)).

---

## Threat Scenarios & Mitigations

| Threat | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Owner key compromise | Medium | Critical | Passkey auth; no seed phrases in production |
| Accidental TTL expiry | Medium | High | Check-in reminders; realistic interval; TTL monitoring |
| Malicious beneficiary claim | Low | High | `is_expired()` enforced on-chain; grace period |
| Admin account takeover | Low | Medium | Two-step admin transfer; admin cannot access funds |
| Re-initialisation attack | Very Low | High | `initialize()` guards against double-init |
| Supply-chain dependency attack | Low | High | `cargo audit`; pinned deps; `cargo deny` |
| Secret leakage via logs | Medium | High | Structured logging with allowlists; pre-commit scan |
| Phishing / social engineering | Medium | High | WebAuthn is phishing-resistant by design |
| Contract storage loss | Low | Critical | Periodic state backup automation |

For the full threat model, see [`docs/security.md`](security.md).

---

## Incident Response

If you suspect a security incident affecting a vault:

1. **Do not check in** — avoid extending TTL if the owner account may be compromised.
2. **Contact the admin** — the admin can pause the contract to freeze state while the incident is investigated.
3. **File a private disclosure** — follow the process in [`SECURITY.md`](../SECURITY.md) for responsible vulnerability disclosure.
4. **Preserve evidence** — keep all logs, transaction IDs, and timestamps before taking remediation steps.
5. **Follow the incident runbook** — see [`docs/incident-response.md`](incident-response.md) for step-by-step guidance.

---

*Last updated: 2026-09-27*
