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
