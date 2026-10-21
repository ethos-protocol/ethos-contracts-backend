# Threat Model & Security

## Threat Vectors

### 1. Owner Key Compromise

**Risk**: Attacker gains access to owner's private key

**Mitigations**:
- Passkey authentication (planned) eliminates seed phrase exposure
- Owner can update beneficiary before attacker triggers release
- Pause mechanism allows admin to freeze contract

### 2. Premature Release

**Risk**: Beneficiary triggers release before owner is deceased

**Mitigations**:
- `is_expired()` check enforces TTL expiry
- Returns `ContractError::NotExpired` if triggered early
- Owner can check in to reset countdown

### 3. Admin Abuse

**Risk**: The contract stores one admin address, and that address alone
authorizes pausing, token whitelisting, and contract upgrades. A compromised or
malicious admin can disrupt service, change accepted tokens, or replace the
contract logic and its security guarantees. The contract does not require
threshold or multi-party approval for these operations.

**Mitigations**:
- The admin cannot directly call owner-only vault operations or transfer vault
  funds under the current implementation; however, its upgrade authority can
  replace those protections.
- `propose_admin` and `accept_admin` provide a two-step, time-locked admin
  transfer. This does not delay or require additional approval for actions by
  the current admin.
- Use a multisig or governance-controlled address for the admin role and
  document who controls it operationally. The contract itself does not enforce
  this recommendation.
- At initialization, the token address is checked for the standard token
  interface. This is a compatibility check, not proof that it is the
  canonical XLM contract; deployment operators must verify the network-specific
  address from a trusted source.
- Admin actions are published on-chain.

### 4. Re-initialization Attack

**Risk**: Attacker re-initializes contract with new admin

**Mitigations**:
- `initialize()` checks for existing admin/token
- Returns `ContractError::AlreadyInitialized`
- Tested in `test_initialize_guard_against_double_init`

### 5. Beneficiary Manipulation

**Risk**: Owner sets self as beneficiary to bypass release logic

**Mitigations**:
- `create_vault` rejects owner == beneficiary
- `set_beneficiaries` rejects owner in beneficiary list
- Returns `ContractError::InvalidBeneficiary`

## Security Best Practices

- All owner actions require `owner.require_auth()`
- Structured error handling via ContractError enum
- Comprehensive test coverage for edge cases
- State validation before mutations
- TTL extension on all storage operations

## IP Reputation Score Decay

`backend/src/ip_reputation.rs` scores IPs on a `0.0..=100.0` abuse-confidence
scale (via AbuseIPDB, or a local penalty recorded by other subsystems — see
below). Without decay, a temporarily bad-behaving IP — e.g. a shared NAT
gateway where one client misbehaved — would stay flagged indefinitely.

### Model

`apply_score_decay` pulls a score toward a configurable neutral baseline
(`IpReputationConfig::decay_baseline`, default `0.0`) at a rate proportional
to elapsed wall-clock time since the score was last checked or updated
(`IpReputationScore::last_checked`):

- **Above baseline** (the common case — an IP has accumulated risk): the
  score decreases toward baseline at `decay_rate_down_per_hour`
  (default `5.0`/hour).
- **Below baseline**: the score increases toward baseline at
  `decay_rate_up_per_hour` (default `1.0`/hour).

The two rates are configured independently so, for example, risk can be shed
faster than it recovers toward a stricter-than-zero baseline. Decay never
overshoots the baseline in either direction, and can be disabled entirely via
`decay_enabled: false` (scores then only change on an explicit check or
penalty).

### Where decay is applied

- `GET /admin/ip-reputation` decays the cached score before returning it
  whenever there's no authoritative upstream check for the request (AbuseIPDB
  disabled, or no `ABUSEIPDB_API_KEY` configured) — otherwise a locally
  penalized IP would reset to a flat score on every lookup instead of
  actually decaying.
- `apply_local_penalty` (used by the CAPTCHA bypass-detection integration
  below) decays the existing cached score before adding a new penalty, so a
  penalty applied after a long idle period doesn't stack on top of a stale
  value.

## CAPTCHA Bypass Detection

`backend/src/captcha.rs` tracks consecutive CAPTCHA verification failures
per IP (`record_captcha_failure` / `record_captcha_success`), since repeated,
rapid failures suggest automated bypass attempts rather than genuine user
error:

- After `BACKOFF_FAILURE_THRESHOLD` (3) consecutive failures, the IP enters a
  progressive backoff window: each further failure doubles the block
  duration (base 5s, capped at 15 minutes). `POST /captcha/verify` rejects
  requests from a backed-off IP with `429 Too Many Requests` before even
  attempting verification.
- After `REPUTATION_FLAG_THRESHOLD` (5) consecutive failures, the IP is
  flagged in the IP-reputation subsystem via `ip_reputation::apply_local_penalty`,
  which raises its cached reputation score — subject to the decay model
  above, so the flag fades if the IP stops failing CAPTCHAs.
- A successful verification clears both the failure count and the backoff
  window for that IP.

## Audit Status

Not yet audited. Community review welcome.

## ACL Permission Resolution Order

The dynamic ACL store (`backend/src/acl.rs`) uses the following resolution order
for every access-control check:

```
is_allowed(subject, resource, action)
    │
    ├─ 1. Collect all rules where:
    │       rule.subject == "*"  OR  rule.subject == subject
    │       rule.resource == "*" OR  resource.starts_with(rule.resource)
    │       rule.action   == "*" OR  rule.action == action   (case-insensitive)
    │
    ├─ 2. If ANY matching rule has effect = Deny  →  DENY  (deny always wins)
    │
    └─ 3. Otherwise  →  ALLOW  (including the case where no rules match at all)
```

**Key properties:**

- **Deny beats allow**: A single matching Deny rule blocks access regardless of
  how many Allow rules are present for the same principal, resource, or action.
- **Wildcard subjects model inheritance**: A rule with `subject = "*"` acts as a
  "parent role" that applies to all principals. A more-specific rule
  (e.g., `subject = "alice"`) acts as the child override, but it cannot lift a
  deny that comes from a wildcard rule.
- **Default allow**: When no rules match the request at all, access is permitted.
  This preserves backwards compatibility with the previous static-allow behavior.
- **Immediate effect**: Rules take effect on the very next request — there is no
  reload step or caching layer between a mutation and enforcement.

**Implications for operators:**

| Scenario | Result |
|---|---|
| Wildcard allow + per-user deny | Deny wins — user is blocked |
| Wildcard deny + per-user allow | Deny wins — allow is ignored |
| No matching rules | Default allow |
| Role with zero rules | Default allow for all its requests |
| Removing the only deny rule | Access immediately restored |

See `backend/src/acl.rs` (tests section, `#[cfg(test)]`) for a full suite of
inheritance and resolution-order tests covering these cases.
