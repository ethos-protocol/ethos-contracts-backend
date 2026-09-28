# Vault Storage Reduction and Compliance Policy Versioning

Issues #556, #557, #558, #559 for the `ttl_vault` contract. The byte-level
codec shared by compression and incremental snapshots lives in
`contracts/ttl_vault/src/diff_codec.rs` (XOR delta + zero-run RLE).

## Compliance policy versioning (#556)

Implemented in `compliance_policy.rs`. Every change through
`set_kyc_high_value_threshold`, `set_allowlist_enforced` and
`set_reporting_thresholds` records a new policy version effective
immediately. The admin can also schedule a complete policy for a future date.

| Function | Auth | Description |
|---|---|---|
| `publish_compliance_policy(kyc_threshold, allowlist_enforced, thresholds, effective_from)` | admin | Append a policy; applied now if `effective_from` is now, otherwise once due. |
| `sync_compliance_policy()` | — | Apply the policy in force now if it is not applied yet. |
| `get_policy_version(timestamp)` | — | The policy in force at `timestamp` (latest `effective_from <= timestamp`, higher version wins ties). |
| `get_compliance_policy(version)` / `get_policy_version_count()` / `get_applied_policy_version()` | — | Inspect the history. |

Errors: `InvalidEffectiveDate` (past date), `PolicyVersionNotFound`.

## Vault history archival (#557)

Implemented in `history_archive.rs`. `archive_vault_history(vault_id)` (anyone)
moves state-transition entries older than 30 days out of the hot log in pages
of 25 (at most 8 pages per call). Each page is:

- emitted in full as a `hist_arc` event for off-chain storage;
- kept as a cold persistent entry that is never TTL-extended, so the network
  can evict it;
- hash-chained: `hash = sha256(prev_hash || vault_id || page || xdr(entries))`,
  with the small per-page digest and chain head kept on-chain.

| Function | Description |
|---|---|
| `get_archived_history(vault_id, page)` | Page body while it is still live. |
| `get_history_archive_meta(vault_id)` / `get_archive_page_digest(vault_id, page)` | Bookkeeping and digests. |
| `verify_archived_page(vault_id, page, entries)` | Check an off-chain copy against the on-chain digest. |
| `verify_history_archive(vault_id)` | Verify the whole chain and every live body. |

`get_state_transition_log` now returns only entries that have not been archived.

## Vault compression for inactive accounts (#558)

Implemented in `vault_compression.rs`. `compress_vault(vault_id) -> bool`
(anyone) replaces the vault entry with an RLE-compressed XDR record once the
vault has not been checked in for the inactivity period (default 90 days,
`set_compression_inactivity` / `get_compression_inactivity`, admin). It
returns `false` when the vault is not eligible, already compressed, or would
not shrink.

Decompression is on demand: `load_vault` / `try_load_vault` fall back to the
compressed record, so every read keeps working. The next write stores the
vault uncompressed again; `decompress_vault(vault_id)` does this explicitly.
A SHA-256 of the original XDR is verified on every decode
(`VaultCompressionCorrupted` on mismatch). `is_vault_compressed` and
`get_vault_compression_info` report status and sizes.

## Incremental state snapshots (#559)

Implemented in `incremental_snapshots.rs`, alongside the existing full
`create_vault_snapshot`. `create_incremental_snapshot(vault_id)` stores a full
checkpoint every 16 snapshots and otherwise only the vault fields that changed.
Each changed field is stored differentially compressed (XOR against the
previous encoding, then RLE).

| Function | Description |
|---|---|
| `reconstruct_vault_snapshot(vault_id, sequence)` | Replay from the nearest checkpoint; verified against the stored state hash. |
| `reconstruct_vault_at(vault_id, timestamp)` | Reconstruct the latest snapshot at or before `timestamp`. |
| `get_incremental_snapshot(vault_id, sequence)` / `get_incremental_snapshot_count(vault_id)` | Raw chain entries, including `full_size` vs `stored_size`. |

Errors: `SnapshotNotFound`, `SnapshotCorrupted`.
