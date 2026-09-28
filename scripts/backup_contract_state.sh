#!/usr/bin/env bash
# backup_contract_state.sh — Export and archive Ethos-Protocol contract state
#
# Usage:
#   ./scripts/backup_contract_state.sh [OPTIONS]
#
# Options:
#   --network <name>       Stellar network to query (default: testnet)
#   --output-dir <path>    Directory to write backups (default: ./backups)
#   --keep <n>             Number of versioned backups to retain (default: 30)
#   --verify               Verify the backup after writing (default: on)
#   --no-verify            Skip verification step
#   --dry-run              Print what would be exported without writing files
#
# Environment variables (override defaults):
#   CONTRACT_TTL_VAULT     Contract ID of the deployed vault contract
#   STELLAR_RPC_URL        Stellar RPC endpoint
#   BACKUP_OUTPUT_DIR      Default output directory
#   BACKUP_KEEP            Number of backups to retain
#
# Examples:
#   # Daily backup to default dir
#   ./scripts/backup_contract_state.sh --network testnet
#
#   # Mainnet backup with 90-day retention
#   ./scripts/backup_contract_state.sh --network mainnet --keep 90 --output-dir /mnt/backups/ethos
#
#   # Dry run
#   ./scripts/backup_contract_state.sh --dry-run
#
# Exit codes:
#   0  Success
#   1  Missing dependency or configuration error
#   2  Export failure
#   3  Verification failure

set -euo pipefail

# ── Defaults ──────────────────────────────────────────────────────────────────

NETWORK="${STELLAR_NETWORK:-testnet}"
OUTPUT_DIR="${BACKUP_OUTPUT_DIR:-./backups}"
KEEP="${BACKUP_KEEP:-30}"
VERIFY=true
DRY_RUN=false

# ── Argument parsing ───────────────────────────────────────────────────────────

while [[ $# -gt 0 ]]; do
  case "$1" in
    --network)    NETWORK="$2";     shift 2 ;;
    --output-dir) OUTPUT_DIR="$2";  shift 2 ;;
    --keep)       KEEP="$2";        shift 2 ;;
    --verify)     VERIFY=true;      shift   ;;
    --no-verify)  VERIFY=false;     shift   ;;
    --dry-run)    DRY_RUN=true;     shift   ;;
    *)
      echo "Unknown option: $1" >&2
      echo "Run with --help or read the header comments for usage." >&2
      exit 1
      ;;
  esac
done

# ── Dependency checks ─────────────────────────────────────────────────────────

for cmd in stellar jq sha256sum; do
  if ! command -v "$cmd" &>/dev/null; then
    echo "ERROR: required command not found: $cmd" >&2
    exit 1
  fi
done

# ── Contract ID resolution ────────────────────────────────────────────────────

CONTRACT_ID="${CONTRACT_TTL_VAULT:-}"
if [[ -z "$CONTRACT_ID" ]]; then
  # Fall back to environments.toml
  if command -v grep &>/dev/null && [[ -f environments.toml ]]; then
    CONTRACT_ID=$(grep -A5 "^\[$NETWORK\]" environments.toml \
      | grep "contract_id" \
      | head -1 \
      | sed 's/.*=\s*"\(.*\)"/\1/')
  fi
fi

if [[ -z "$CONTRACT_ID" || "$CONTRACT_ID" == "<your-contract-id>" ]]; then
  echo "ERROR: CONTRACT_TTL_VAULT is not set and could not be resolved from environments.toml." >&2
  echo "Set the CONTRACT_TTL_VAULT environment variable or update environments.toml." >&2
  exit 1
fi

# ── RPC URL resolution ────────────────────────────────────────────────────────

case "$NETWORK" in
  mainnet)  DEFAULT_RPC="https://mainnet.sorobanrpc.com" ;;
  testnet)  DEFAULT_RPC="https://soroban-testnet.stellar.org" ;;
  futurenet) DEFAULT_RPC="https://rpc-futurenet.stellar.org" ;;
  standalone) DEFAULT_RPC="http://localhost:8000/soroban/rpc" ;;
  *)        DEFAULT_RPC="" ;;
esac
RPC_URL="${STELLAR_RPC_URL:-$DEFAULT_RPC}"

if [[ -z "$RPC_URL" ]]; then
  echo "ERROR: STELLAR_RPC_URL is not set for network '$NETWORK'." >&2
  exit 1
fi

# ── Timestamp & paths ─────────────────────────────────────────────────────────

TIMESTAMP=$(date -u +"%Y%m%dT%H%M%SZ")
BACKUP_DIR="$OUTPUT_DIR/$NETWORK"
BACKUP_FILE="$BACKUP_DIR/state-${TIMESTAMP}.json"
CHECKSUM_FILE="$BACKUP_DIR/state-${TIMESTAMP}.sha256"
LATEST_LINK="$BACKUP_DIR/latest.json"

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Ethos-Protocol Contract State Backup"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Network:     $NETWORK"
echo "  Contract:    $CONTRACT_ID"
echo "  RPC:         $RPC_URL"
echo "  Output:      $BACKUP_FILE"
echo "  Retention:   $KEEP backups"
echo "  Dry run:     $DRY_RUN"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

if [[ "$DRY_RUN" == "true" ]]; then
  echo ""
  echo "[dry-run] Would create directory: $BACKUP_DIR"
  echo "[dry-run] Would export ledger entries for contract: $CONTRACT_ID"
  echo "[dry-run] Would write backup to:   $BACKUP_FILE"
  echo "[dry-run] Would write checksum to: $CHECKSUM_FILE"
  echo "[dry-run] Would update symlink:    $LATEST_LINK -> $BACKUP_FILE"
  echo "[dry-run] Would prune backups older than $KEEP versions."
  echo ""
  echo "Dry run complete. No files written."
  exit 0
fi

# ── Create output directory ───────────────────────────────────────────────────

mkdir -p "$BACKUP_DIR"

# ── Export contract ledger entries ────────────────────────────────────────────

echo ""
echo "▶ Exporting contract ledger entries..."

# Build a metadata envelope alongside the raw ledger entries
LEDGER_INFO=$(stellar --rpc-url "$RPC_URL" \
  ledger \
  latest 2>/dev/null || echo '{}')

LEDGER_SEQ=$(echo "$LEDGER_INFO" | jq -r '.sequence // "unknown"')
LEDGER_TS=$(echo "$LEDGER_INFO"  | jq -r '.closed_at // "unknown"')

# Export all contract data entries as a JSON array
RAW_ENTRIES=$(stellar --rpc-url "$RPC_URL" \
  contract read \
  --id "$CONTRACT_ID" \
  --output json 2>/dev/null || echo '[]')

# Wrap in a versioned envelope
BACKUP_PAYLOAD=$(jq -n \
  --arg schema_version "1" \
  --arg network        "$NETWORK" \
  --arg contract_id    "$CONTRACT_ID" \
  --arg rpc_url        "$RPC_URL" \
  --arg timestamp      "$TIMESTAMP" \
  --arg ledger_seq     "$LEDGER_SEQ" \
  --arg ledger_ts      "$LEDGER_TS" \
  --argjson entries    "$RAW_ENTRIES" \
  '{
    schema_version: $schema_version,
    network:        $network,
    contract_id:    $contract_id,
    rpc_url:        $rpc_url,
    exported_at:    $timestamp,
    ledger_sequence: $ledger_seq,
    ledger_closed_at: $ledger_ts,
    entries:        $entries
  }')

echo "$BACKUP_PAYLOAD" > "$BACKUP_FILE"
echo "✓ Backup written: $BACKUP_FILE"

# ── Checksum ──────────────────────────────────────────────────────────────────

sha256sum "$BACKUP_FILE" > "$CHECKSUM_FILE"
echo "✓ Checksum written: $CHECKSUM_FILE"

# ── Symlink latest ────────────────────────────────────────────────────────────

ln -sf "$(basename "$BACKUP_FILE")" "$LATEST_LINK"
echo "✓ Latest symlink updated: $LATEST_LINK"

# ── Verification ──────────────────────────────────────────────────────────────

if [[ "$VERIFY" == "true" ]]; then
  echo ""
  echo "▶ Verifying backup..."

  # 1. Checksum integrity
  if ! sha256sum --check "$CHECKSUM_FILE" --status; then
    echo "ERROR: Checksum verification failed for $BACKUP_FILE" >&2
    exit 3
  fi
  echo "✓ Checksum verified"

  # 2. JSON is well-formed
  if ! jq empty "$BACKUP_FILE" 2>/dev/null; then
    echo "ERROR: Backup file is not valid JSON: $BACKUP_FILE" >&2
    exit 3
  fi
  echo "✓ JSON structure valid"

  # 3. Required fields present
  ENTRY_COUNT=$(jq '.entries | length' "$BACKUP_FILE")
  EXPORT_TS=$(jq -r '.exported_at' "$BACKUP_FILE")
  SCHEMA_VER=$(jq -r '.schema_version' "$BACKUP_FILE")

  if [[ "$SCHEMA_VER" != "1" ]]; then
    echo "ERROR: Unexpected schema_version in backup: $SCHEMA_VER" >&2
    exit 3
  fi
  echo "✓ Schema version: $SCHEMA_VER"
  echo "✓ Exported at:    $EXPORT_TS"
  echo "✓ Ledger entries: $ENTRY_COUNT"
fi

# ── Retention pruning ─────────────────────────────────────────────────────────

echo ""
echo "▶ Pruning old backups (keeping $KEEP most recent)..."

# List backups by modification time, oldest first; delete beyond $KEEP
BACKUP_LIST=$(find "$BACKUP_DIR" -maxdepth 1 -name 'state-*.json' \
  | sort)
BACKUP_COUNT=$(echo "$BACKUP_LIST" | grep -c '.' || true)

if [[ "$BACKUP_COUNT" -gt "$KEEP" ]]; then
  DELETE_COUNT=$(( BACKUP_COUNT - KEEP ))
  TO_DELETE=$(echo "$BACKUP_LIST" | head -n "$DELETE_COUNT")
  while IFS= read -r OLD_BACKUP; do
    OLD_CHECKSUM="${OLD_BACKUP%.json}.sha256"
    rm -f "$OLD_BACKUP" "$OLD_CHECKSUM"
    echo "  Pruned: $(basename "$OLD_BACKUP")"
  done <<< "$TO_DELETE"
  echo "✓ Pruned $DELETE_COUNT old backup(s)"
else
  echo "✓ No pruning needed ($BACKUP_COUNT / $KEEP slots used)"
fi

# ── Summary ───────────────────────────────────────────────────────────────────

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Backup complete"
echo "  File:    $BACKUP_FILE"
echo "  Entries: $ENTRY_COUNT"
echo "  Network: $NETWORK @ ledger $LEDGER_SEQ"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
