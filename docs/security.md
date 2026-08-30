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

**Risk**: Admin pauses contract or changes configuration maliciously

**Mitigations**:
- Admin cannot access vault funds
- Admin cannot change vault owners or beneficiaries
- Two-step admin transfer with `propose_admin` and `accept_admin`
- Transparent on-chain actions

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
