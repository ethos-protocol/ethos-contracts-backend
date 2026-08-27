# Vault Activity History

Every write-path operation on a vault appends an `AuditEntry` to that vault's
activity log.  The log is permanently stored on-chain and can be read by
anyone.

## Operations that generate log entries

| Action string            | Triggered by                              |
|--------------------------|-------------------------------------------|
| `create_vault`           | `create_vault`                            |
| `check_in`               | `check_in`                                |
| `deposit`                | `deposit`                                 |
| `withdraw`               | `withdraw`                                |
| `partial_liquidate`      | `partial_liquidate`                       |
| `trigger_release`        | `trigger_release`                         |
| `set_ben_min_threshold`  | `set_beneficiary_min_threshold`           |
| `update_metadata`        | `update_metadata` / `update_vault_metadata` |
| `apply_beneficiary_update` | `apply_beneficiary_update`              |
| `update_check_in_interval` | `update_check_in_interval`             |
| `cancel_vault`           | `cancel_vault`                            |
| `transfer_ownership`     | `transfer_ownership`                      |
| `clone_vault`            | `clone_vault`                             |
| `clone_vault_with_overrides` | `clone_vault_with_overrides`         |
| `merge_vaults_source`    | `merge_vaults` (source vault)             |
| `merge_vaults_target`    | `merge_vaults` (target vault)             |

## AuditEntry fields

```rust
pub struct AuditEntry {
    pub action:    String,   // one of the action strings above
    pub caller:    Address,  // address that initiated the operation
    pub timestamp: u64,      // ledger timestamp at time of the call
    pub operation: String,   // alias for action (kept for backward compat)
    pub actor:     Address,  // alias for caller (kept for backward compat)
    pub details:   String,   // optional human-readable detail string
}
```

## Storage layout (paginated — v2)

To keep per-operation write cost bounded, the activity log is split into
fixed-size pages stored under separate persistent storage keys.

| Key                                    | Type            | Description                                |
|----------------------------------------|-----------------|--------------------------------------------|
| `VaultAuditLogLen(vault_id)`           | `u32`           | Total number of entries written (new layout) |
| `VaultAuditLogPage(vault_id, page_idx)` | `Vec<AuditEntry>` | Up to `PAGE_SIZE = 50` entries for page `page_idx` |
| `VaultAuditLog(vault_id)` *(legacy)*   | `Vec<AuditEntry>` | Flat vector from the pre-v2 layout; read-only after upgrade |

### Write cost

Each `append_activity_log` call reads and writes only the current tail page
(≤ 50 entries), so write cost is O(PAGE_SIZE) — **constant with respect to
total history length** — regardless of how many operations a vault has
accumulated.

### Page assignment

```
page_index = total_entries / 50
```

When a page is full (50 entries written) the next append automatically starts
a new page.

## Reading the log

### Full log (all entries)

```rust
// Returns all entries, oldest-first.
// Transparently reads legacy flat entries first, then paginated entries.
get_vault_audit_log(vault_id: u64) -> Vec<AuditEntry>

// Alias for get_vault_audit_log.
get_vault_activity_log(vault_id: u64) -> Vec<AuditEntry>
```

Both functions are migration-aware: if a vault still has entries under the
legacy `VaultAuditLog` key those are prepended before any pages from the new
layout, so old vaults continue to work transparently without a migration step.

### Single page

```rust
// Returns at most `page_size` entries from page `page`.
// `page_size` is capped internally at PAGE_SIZE (50).
get_vault_audit_log_page(vault_id: u64, page: u32, page_size: u32) -> Vec<AuditEntry>
```

Use this for paginated UI display or when you need only a recent slice of the
log without deserialising the entire history.

## Migration / backward compatibility

Vaults created before the v2 paginated layout was deployed have all their
history stored under the legacy `VaultAuditLog(vault_id)` key.  **No on-chain
migration transaction is required.**

- All read functions (`get_vault_audit_log`, `get_vault_activity_log`,
  `get_vault_audit_log_page`) check the legacy key first and prepend any
  legacy entries before returning new-layout pages.
- Subsequent writes to a legacy vault automatically create new-layout page
  keys; the legacy entries remain readable in place.
- The legacy flat key is never written again after the upgrade; it is purely
  a read-only archive.

## Related documentation

- [Architecture Overview](architecture.md)
- [API Reference](api-reference.md)
- [TTL & State Archival Logic](ttl-logic.md)
- [Withdrawal Features](withdrawal-features.md)
