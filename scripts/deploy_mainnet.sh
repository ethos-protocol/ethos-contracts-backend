#!/usr/bin/env bash
set -e

source "$(dirname "$0")/deploy_utils.sh"

NETWORK="mainnet"
DEPLOYER="${DEPLOYER_IDENTITY:-deployer-mainnet}"

parse_common_flags "$@"

# Required env vars
: "${STELLAR_MAINNET_RPC_URL:?STELLAR_MAINNET_RPC_URL must be set}"

echo "⚠️  You are about to deploy Ethos-Protocol to MAINNET."
echo "    Network : $NETWORK"
echo "    Identity: $DEPLOYER"
echo "    RPC URL : $STELLAR_MAINNET_RPC_URL"
if [[ "$DRY_RUN" == "true" ]]; then
  echo "    Mode    : DRY RUN (no changes will be made)"
fi

# Check for existing deployment (both the on-disk marker and environments.toml)
EXISTING_CONTRACT=$(get_contract_address "$NETWORK")
CONTRACT_ALREADY_DEPLOYED=false
if [[ -n "$EXISTING_CONTRACT" && "$EXISTING_CONTRACT" != "<your-contract-id>" ]] || step_done "$NETWORK" "contract_deployed"; then
  CONTRACT_ALREADY_DEPLOYED=true
fi

if [[ "$CONTRACT_ALREADY_DEPLOYED" == "true" ]]; then
  echo "⚠️  Existing contract found: ${EXISTING_CONTRACT:-<unknown, marker only>}"
  echo ""
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
  echo ""
  read -r -p "Type 'mainnet' to confirm deployment: " CONFIRM
  if [ "$CONFIRM" != "mainnet" ]; then
    echo "Aborted."
    exit 1
  fi

  run_step ./scripts/build.sh

  WASM="target/wasm32-unknown-unknown/release/ttl_vault.wasm"

  echo "Deploying contract to $NETWORK..."
  if [[ "$DRY_RUN" == "true" ]]; then
    echo "[dry-run] would run: stellar contract deploy --wasm $WASM --source $DEPLOYER --network $NETWORK --rpc-url $STELLAR_MAINNET_RPC_URL"
    CONTRACT_ID="<dry-run-contract-id>"
  else
    CONTRACT_ID=$(stellar contract deploy \
      --wasm "$WASM" \
      --source "$DEPLOYER" \
      --network "$NETWORK" \
      --rpc-url "$STELLAR_MAINNET_RPC_URL")
  fi

  echo "✓ Contract deployed: $CONTRACT_ID"

  # Update environments.toml
  set_contract_address "$NETWORK" "$CONTRACT_ID"
  echo "✓ Updated environments.toml with new contract address"
  echo "Add to .env: CONTRACT_TTL_VAULT=$CONTRACT_ID"
  mark_step_done "$NETWORK" "contract_deployed"
fi

# --- Admin initialization guard ---
# initialize() on the contract itself is idempotent (it panics on
# AlreadyInitialized), but we still track it locally so re-runs skip the
# on-chain call entirely instead of relying on that panic path.
if step_done "$NETWORK" "admin_initialized" && [[ "$FORCE_DEPLOY" != "true" ]]; then
  echo "✓ Admin state already initialized for $NETWORK. Skipping."
else
  echo "✓ Marking admin state as initialized for $NETWORK (initialize() is called separately per the runbook)."
  mark_step_done "$NETWORK" "admin_initialized"
fi
