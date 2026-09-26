# Vault Compliance Controls

Issues #548, #549, #550, #551 — on-chain compliance primitives for the
`ttl_vault` contract, implemented in `contracts/ttl_vault/src/compliance.rs`.

## Blacklist / allowlist (#551)

| Function | Auth | Description |
|---|---|---|
| `blacklist_address(address, reason)` | admin | Blacklist an address with a reason (1–256 bytes). |
| `remove_from_blacklist(address)` | admin | Remove an entry; returns `false` if absent. |
| `get_blacklist_entry(address)` | — | Reason, author and timestamp. |
| `add_to_allowlist(address)` / `remove_from_allowlist(address)` | admin | Manage the allowlist. |
| `set_allowlist_enforced(bool)` | admin | While enabled, only allowlisted addresses are compliant. |
| `is_address_compliant(address)` | — | Not blacklisted and (allowlist off or allowlisted). |

`create_vault` screens the owner and beneficiary; `deposit` and `withdraw`
screen the owner. Failures return `AddressBlacklisted` or
`AddressNotAllowlisted`. The blacklist always wins over the allowlist.

## KYC verification (#548)

The admin registers a KYC provider with `set_kyc_provider`. The provider calls
`verify_kyc(address, KycData { provider_reference, level, expires_at })`,
which requires the provider's auth. Only a SHA-256 reference hash is stored,
never raw PII. `verify_kyc` returns `false` without storing anything when the
data has a zero level, a zero reference, or an expiry that is not in the
future.

`revoke_kyc` (provider) and `admin_revoke_kyc` (admin) mark a record revoked.
`is_kyc_verified` is true only for a non-revoked, unexpired record.

`set_kyc_high_value_threshold(amount)` (admin) makes deposits and withdrawals
of at least `amount` fail with `KycRequired` unless the owner is verified.
`0` disables the gate.

## Transaction reporting (#549)

Every successful `deposit` and `withdraw` is appended to a timestamp-ordered
transaction log (`get_transaction`, `count_transactions`).

`export_transactions(start_date, end_date)` returns a CSV (UTF-8 `Bytes`) of
the transactions whose timestamp falls in the inclusive range:

```
tx_id,timestamp,kind,vault_id,from,to,token,amount,flagged
```

Addresses are hex-encoded XDR `ScAddress` values, which decode losslessly to
strkeys off-chain. An export returns at most `MAX_EXPORT_ROWS` (200) rows;
split the range when `count_transactions` reports more.

### Report signing

1. Admin registers an ed25519 public key with `set_report_signer`.
2. The signer fetches `compliance_report_digest(start, end)` (SHA-256 of the
   CSV) and signs it off-chain.
3. Anyone submits `sign_compliance_report(start, end, signature)`. The
   contract re-derives the digest, verifies the signature (an invalid one
   aborts the call) and stores a `SignedReport` retrievable with
   `get_signed_report`.

## Threshold monitoring (#550)

`set_reporting_thresholds(ThresholdConfig { single_tx_threshold,
cumulative_threshold, window_seconds })` (admin). A threshold of `0`
disables that check.

- A transaction at or above `single_tx_threshold` raises a
  `SingleTransaction` alert.
- Each address' volume is summed over a rolling window of `window_seconds`
  (`get_cumulative_volume`). The first transaction that brings the window
  total to `cumulative_threshold` or above raises one `Cumulative` alert; the
  window resets once it elapses.

Flagged transactions carry `flagged = true` in the log and CSV. Alerts are
emitted as `cmp_alrt` events and stored (`get_compliance_alert`,
`get_compliance_alert_count`); the admin marks them reviewed with
`acknowledge_compliance_alert`.
