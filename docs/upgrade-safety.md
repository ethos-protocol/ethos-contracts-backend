# Upgrade Safety

`contracts/ttl_vault/src/lib.rs` exposes `upgrade(env, new_wasm_hash)`,
which lets the admin replace the deployed WASM with arbitrary new code.
Historically this only checked that the hash was non-zero — it did not
verify the new code was interface-compatible, storage-compatible, or
error-code-compatible with the running contract. This document describes
the compatibility checks that were added and how to use them.

## What is checked

Because a Soroban contract cannot introspect the raw bytes of a WASM blob
it hasn't deployed yet, compatibility is enforced via an **admin-recorded
manifest** (`UpgradeManifest` in `types.rs`) rather than by disassembling
the new WASM on-chain:

| Field | What it protects against |
|---|---|
| `exported_fn_count` | The new contract exporting fewer public functions than the running one (an accidentally-shrunk interface breaking existing integrators). |
| `error_code_count` | The new contract having fewer `ContractError` variants than the running one (error codes being removed or renumbered, which silently changes the meaning of an error an integrator is already handling). |
| `storage_schema_hash` | The new contract no longer reading/writing a storage key the running contract uses (a backward-incompatible storage migration). |

For strict interface validation, use the `*_with_signatures` entry points.
These store `function_signatures_hash` separately from `UpgradeManifest`.
Compute it from the sorted public function names and their argument and return
types in the candidate WASM contract spec. The fingerprint must remain
identical across an upgrade; merely retaining the same number of exports is
not sufficient to preserve function compatibility.

`validate_upgrade_compatibility` panics with a specific error
(`UpgradeInterfaceShrunk`, `UpgradeErrorCodesReduced`,
`UpgradeStorageSchemaChanged`, `UpgradeManifestNotSet`). The signature-aware
validator also reports `UpgradeFunctionSignaturesChanged` for a fingerprint
mismatch and `UpgradeFunctionSignaturesNotSet` when no strict baseline has
been recorded.

## Admin workflow

1. **After first deploying/initializing the contract**, record a baseline:

   ```
   set_upgrade_manifest_with_signatures(
       exported_fn_count,
       error_code_count,
       storage_schema_hash,
       function_signatures_hash,
   )
   ```

   Compute these values with an off-chain tool that inspects the
   deployed WASM (e.g. `wasm-objdump -x` for the export table count, a
   count of `ContractError` variants from the source, and a hash — e.g.
   SHA-256 — over the sorted list of `DataKey` variant names/tags used by
   the contract and another over the sorted contract-spec function signatures).

2. **Before every upgrade**, compute the same manifest values for the *new*
   candidate WASM and call:

   ```
   validate_upgrade_compatibility_with_signatures(
       new_exported_fn_count,
       new_error_code_count,
       new_storage_schema_hash,
       new_function_signatures_hash,
   )
   ```

   This can be simulated read-only before submitting the real upgrade
   transaction, to fail fast without spending an upgrade attempt.

3. **Perform the upgrade** with the same manifest values via:

   ```
   upgrade_with_manifest_and_signatures(
       new_wasm_hash,
       new_exported_fn_count,
       new_error_code_count,
       new_storage_schema_hash,
       new_function_signatures_hash,
   )
   ```

   This validates the hash (`validate_upgrade`), validates compatibility
   (`validate_upgrade_compatibility_with_signatures`), performs the WASM swap,
   and then records the new values as the baseline for the *next* upgrade
   (incrementing `UpgradeManifest.version`).

The legacy `set_upgrade_manifest`, `validate_upgrade_compatibility`,
`upgrade_with_manifest`, and `upgrade(new_wasm_hash)` entry points remain
available for compatibility with existing tooling. The first three do not
check exact function signatures; new deployments and operational tooling
should prefer their `*_with_signatures` counterparts. A successful legacy
upgrade clears any stored signature fingerprint, so a strict upgrade cannot
reuse a stale baseline; record it again with
`set_upgrade_manifest_with_signatures`.

## Automated simulation

CI builds the deployable WASM, installs it in a Soroban test environment,
initializes the contract, and executes the signature-aware upgrade path against
the candidate artifact. It then verifies the manifest version and stored
configuration remain accessible after the executable is replaced. Native
tests also reject changed function-signature fingerprints even when the export
count is unchanged. To run the WASM simulation locally, build the contract and
set `TTL_VAULT_WASM_PATH` to `target/wasm32-unknown-unknown/release/ttl_vault.wasm`
when running the ignored `upgrade_simulation_tests` test.

## Limitations

- The manifest values are admin-declared, not independently verified
  on-chain — this is a guardrail against accidental interface/storage
  regressions, not a substitute for code review of the new WASM before an
  upgrade is proposed.
- `storage_schema_hash` should be computed by hashing the *sorted* list of
  key names/tags used by the contract (available in `types.rs`'s `DataKey`
  enum) so that reordering the enum's declaration doesn't spuriously
  change the hash. Removing or renaming a variant, however, should.
- If an admin key is compromised, these checks do not prevent a malicious
  upgrade — an attacker with admin authority can compute a manifest for
  their own malicious contract and pass it in. Admin key security (e.g.
  the timelocked admin transfer flow already in this contract) remains the
  primary control. See `docs/runbook-alerts.md#ethoscontractupgradeinprogress`
  for the operational response when an unexpected upgrade is observed.

## Tests

See `contracts/ttl_vault/src/upgrade_validation_tests.rs` for coverage of:
missing manifest, interface shrinkage, error code reduction, storage
schema drift, signature changes, compatible upgrades, and manifest version
incrementing. The WASM-level state-preservation test is in
`contracts/ttl_vault/src/upgrade_simulation_tests.rs`.
