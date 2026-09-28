# Contract State Backup Automation

This document describes the contract state backup system for Ethos-Protocol, implemented in `scripts/backup_contract_state.sh`.

## Why back up contract state?

Soroban contract state lives on-chain and is durable by design. However, an off-chain backup provides:

- **Disaster recovery**: If a contract must be redeployed (e.g., after a critical upgrade), an off-chain snapshot can be used to reconstruct state.
- **Audit trail**: Time-stamped snapshots of vault entries provide a verifiable history of state changes.
- **Monitoring input**: Backup diffs can surface unexpected state mutations between snapshots.
- **Compliance**: Regulated deployments may require periodic evidence of data integrity.

## Quick start

```bash
# Testnet backup (default)
./scripts/backup_contract_state.sh

# Mainnet backup with 90-day retention
CONTRACT_TTL_VAULT=<contract-id> \
  ./scripts/backup_contract_state.sh --network mainnet --keep 90 --output-dir /mnt/backups/ethos

# Dry run — see what would happen without writing files
./scripts/backup_contract_state.sh --dry-run
```

## Options

| Flag | Default | Description |
|---|---|---|
| `--network <name>` | `testnet` | Stellar network to query |
| `--output-dir <path>` | `./backups` | Directory to write backup files |
| `--keep <n>` | `30` | Number of versioned backups to retain |
| `--verify` | on | Verify the backup after writing |
| `--no-verify` | — | Skip verification step |
| `--dry-run` | — | Print what would happen without writing |

## Environment variables

| Variable | Description |
|---|---|
| `CONTRACT_TTL_VAULT` | Contract ID (required if not in `environments.toml`) |
| `STELLAR_RPC_URL` | Override the RPC endpoint for the chosen network |
| `BACKUP_OUTPUT_DIR` | Default output directory (overridden by `--output-dir`) |
| `BACKUP_KEEP` | Default retention count (overridden by `--keep`) |

## Output format

Each backup is a JSON file with the following envelope:

```json
{
  "schema_version": "1",
  "network": "testnet",
  "contract_id": "C...",
  "rpc_url": "https://soroban-testnet.stellar.org",
  "exported_at": "20260927T105500Z",
  "ledger_sequence": "12345678",
  "ledger_closed_at": "2026-09-27T10:55:00Z",
  "entries": [ ... ]
}
```

A SHA-256 checksum file (`state-<timestamp>.sha256`) is written alongside each backup. The `latest.json` symlink always points to the most recent backup.

```
backups/
└── testnet/
    ├── state-20260927T105500Z.json
    ├── state-20260927T105500Z.sha256
    ├── state-20260926T105500Z.json
    ├── state-20260926T105500Z.sha256
    └── latest.json -> state-20260927T105500Z.json
```

## Verification

After writing, the script verifies:

1. **Checksum integrity** — `sha256sum --check` on the backup file.
2. **JSON validity** — `jq empty` confirms the file is well-formed.
3. **Schema version** — confirms `schema_version` is `"1"`.
4. **Entry count** — logs the number of ledger entries exported.

Skip verification with `--no-verify` for performance in high-frequency automation.

## Retention and pruning

The script retains the `--keep` most recent backups and deletes older ones automatically. Both the `.json` and `.sha256` files are pruned together. The default is 30 backups; for daily runs that is 30 days of history.

## Restoring from a backup

State restoration must be done carefully and is a manual process:

1. Identify the target backup:
   ```bash
   ls -lh backups/mainnet/
   cat backups/mainnet/latest.json | jq '{exported_at, ledger_sequence, entries: (.entries | length)}'
   ```

2. Verify the checksum:
   ```bash
   sha256sum --check backups/mainnet/state-<timestamp>.sha256
   ```

3. Inspect the entries to understand what state needs to be re-applied.

4. Use `stellar contract invoke` to re-write individual entries as needed, or work with the core team to plan a coordinated restore.

> **Important**: Do not blindly replay all entries from a backup into a live contract without understanding the current on-chain state. Always compare the backup against the current ledger state first.

See also [`docs/disaster-recovery-runbook.md`](disaster-recovery-runbook.md) for the full DR process.

## Scheduling automated backups

### Cron (Linux/macOS)

Add a crontab entry to run a daily backup at 02:00 UTC:

```cron
0 2 * * * cd /path/to/ethos-contracts-backend && \
  CONTRACT_TTL_VAULT=<id> \
  ./scripts/backup_contract_state.sh --network mainnet --keep 90 \
  >> /var/log/ethos-backup.log 2>&1
```

### GitHub Actions

Add a scheduled workflow to run backups from CI:

```yaml
name: Contract State Backup

on:
  schedule:
    - cron: '0 2 * * *'   # daily at 02:00 UTC
  workflow_dispatch:        # allow manual trigger

jobs:
  backup:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Install Stellar CLI
        run: cargo install stellar-cli --locked
      - name: Run backup
        env:
          CONTRACT_TTL_VAULT: ${{ secrets.CONTRACT_TTL_VAULT }}
          STELLAR_RPC_URL: ${{ secrets.STELLAR_RPC_URL }}
        run: |
          ./scripts/backup_contract_state.sh \
            --network mainnet \
            --output-dir ./backups \
            --keep 90
      - name: Upload backup artifact
        uses: actions/upload-artifact@v4
        with:
          name: contract-state-${{ github.run_id }}
          path: backups/
          retention-days: 90
```

## Dependencies

The script requires:

- `stellar` CLI (Stellar CLI with Soroban support)
- `jq` (JSON processor)
- `sha256sum` (part of GNU coreutils)

---

*Last updated: 2026-09-27*
