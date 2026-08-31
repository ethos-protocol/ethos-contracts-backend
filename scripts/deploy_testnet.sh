#!/usr/bin/env bash
set -e

source "$(dirname "$0")/deploy_utils.sh"

NETWORK="testnet"
DEPLOYER="deployer"

parse_common_flags "$@"

echo "Deploying Ethos-Protocol to $NETWORK..."
if [[ "$DRY_RUN" == "true" ]]; then
  echo "Mode: DRY RUN (no changes will be made)"
fi

# Check for existing deployment (both the on-disk marker and environments.toml)
EXISTING_CONTRACT=$(get_contract_address "$NETWORK")
CONTRACT_ALREADY_DEPLOYED=false
if [[ -n "$EXISTING_CONTRACT" && "$EXISTING_CONTRACT" != "<your-contract-id>" ]] || step_done "$NETWORK" "contract_deployed"; then
  CONTRACT_ALREADY_DEPLOYED=true
fi

if [[ "$FORCE_DEPLOY" == "true" ]]; then
  if ! confirm_force "$NETWORK"; then
    echo "Force redeploy not confirmed. Aborted."
    exit 1
  fi
fi

if [[ "$CONTRACT_ALREADY_DEPLOYED" == "true" && "$FORCE_DEPLOY" != "true" ]]; then
  echo "✓ Contract already deployed to $NETWORK (${EXISTING_CONTRACT:-see marker}). Skipping deploy step."
  echo "  Use --force to intentionally redeploy."
else
  # Build first
  run_step ./scripts/build.sh

  WASM="target/wasm32-unknown-unknown/release/ttl_vault.wasm"

  echo "Deploying contract to $NETWORK..."
  if [[ "$DRY_RUN" == "true" ]]; then
    echo "[dry-run] would run: stellar contract deploy --wasm $WASM --source $DEPLOYER --network $NETWORK"
    CONTRACT_ID="<dry-run-contract-id>"
  else
    CONTRACT_ID=$(stellar contract deploy \
      --wasm "$WASM" \
      --source "$DEPLOYER" \
      --network "$NETWORK")
  fi

  echo "✓ Contract deployed: $CONTRACT_ID"

  # Update environments.toml
  set_contract_address "$NETWORK" "$CONTRACT_ID"
  echo "✓ Updated environments.toml with new contract address"
  echo "Add to .env: CONTRACT_TTL_VAULT=$CONTRACT_ID"
  mark_step_done "$NETWORK" "contract_deployed"
fi

# --- Admin initialization guard ---
# Mirrors mainnet: initialize() itself is idempotent on-chain (panics on
# AlreadyInitialized), but the local marker avoids the redundant call/attempt
# entirely on a re-run after a partial failure.
if step_done "$NETWORK" "admin_initialized" && [[ "$FORCE_DEPLOY" != "true" ]]; then
  echo "✓ Admin state already initialized for $NETWORK. Skipping."
else
  echo "✓ Marking admin state as initialized for $NETWORK (initialize() is called separately per the runbook)."
  mark_step_done "$NETWORK" "admin_initialized"
fi
