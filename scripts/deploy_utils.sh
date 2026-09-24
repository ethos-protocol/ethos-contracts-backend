#!/usr/bin/env bash
# Shared utility functions for deployment scripts
#
# --- Idempotent deploy support (Issue: safe re-run after partial failure) ---
#
# Deploy scripts sourcing this file get:
#   - A deployed-state marker file per network under .deploy-state/<network>.state
#     that records which steps ("contract_deployed", "admin_initialized") have
#     already completed, so re-running a script after a partial failure does
#     not redeploy a contract or double-initialize admin state.
#   - DRY_RUN mode: export DRY_RUN=1 or pass --dry-run to any deploy script to
#     print every action (marker checks, stellar CLI invocations, state writes)
#     without executing them or mutating any files.
#   - FORCE mode: pass --force to intentionally redeploy/reinitialize. Force is
#     gated by an explicit typed confirmation (not just yes/no) to avoid
#     accidental redeploys from muscle-memory "yes" answers.
#
# Usage in a deploy script:
#   source "$(dirname "$0")/deploy_utils.sh"
#   parse_common_flags "$@"
#   if ! step_done "$NETWORK" "contract_deployed" && ! $FORCE_DEPLOY; then
#     ... deploy ...
#     mark_step_done "$NETWORK" "contract_deployed"
#   else
#     echo "Skipping contract deploy: already deployed (use --force to redeploy)"
#   fi

DEPLOY_STATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/.deploy-state"

DRY_RUN=false
FORCE_DEPLOY=false

# Parse --dry-run / --force from a script's positional args. Safe to call
# with "$@" from the top-level deploy script. Leaves DRY_RUN/FORCE_DEPLOY set
# as globals for the rest of the script to consult.
parse_common_flags() {
  for arg in "$@"; do
    case "$arg" in
      --dry-run)
        DRY_RUN=true
        ;;
      --force)
        FORCE_DEPLOY=true
        ;;
    esac
  done

  if [[ "${DRY_RUN_ENV:-${DRY_RUN:-}}" == "1" || "${DRY_RUN}" == "true" && -n "${DRY_RUN_ENV:-}" ]]; then
    DRY_RUN=true
  fi
  if [[ "${DRY_RUN}" != "true" && "${DRY_RUN}" != "false" ]]; then
    DRY_RUN=false
  fi
}

# Run a command, or just print it when DRY_RUN is active.
run_step() {
  if [[ "$DRY_RUN" == "true" ]]; then
    echo "[dry-run] would run: $*"
    return 0
  fi
  "$@"
}

state_file() {
  local network=$1
  echo "${DEPLOY_STATE_DIR}/${network}.state"
}

# Returns 0 (true) if the given step name has already completed for network.
step_done() {
  local network=$1
  local step=$2
  local file
  file=$(state_file "$network")
  [[ -f "$file" ]] && grep -qx "$step" "$file"
}

# Records that a step has completed for network, so subsequent runs skip it.
mark_step_done() {
  local network=$1
  local step=$2
  local file
  file=$(state_file "$network")

  if [[ "$DRY_RUN" == "true" ]]; then
    echo "[dry-run] would mark step '$step' complete for $network in $file"
    return 0
  fi

  mkdir -p "$DEPLOY_STATE_DIR"
  touch "$file"
  if ! grep -qx "$step" "$file" 2>/dev/null; then
    echo "$step" >> "$file"
  fi
}

# Clears all recorded state for a network (used when starting fully fresh).
clear_deploy_state() {
  local network=$1
  local file
  file=$(state_file "$network")
  if [[ "$DRY_RUN" == "true" ]]; then
    echo "[dry-run] would clear deploy state for $network ($file)"
    return 0
  fi
  rm -f "$file"
}

# Gate for destructive/force actions: requires the caller to type an
# unambiguous, network-specific phrase rather than a generic yes/no, so a
# reflexive "yes" doesn't trigger a redeploy or double-initialization.
confirm_force() {
  local network=$1
  local expected="FORCE-REDEPLOY-${network^^}"
  local response

  if [[ "$DRY_RUN" == "true" ]]; then
    echo "[dry-run] would require typed confirmation '$expected' to proceed with --force"
    return 0
  fi

  echo "--force was specified for network '$network'."
  echo "This may redeploy an already-deployed contract and/or re-run admin"
  echo "initialization. This action is not reversible on-chain."
  read -r -p "Type '${expected}' to confirm: " response
  [[ "$response" == "$expected" ]]
}

# Parse contract address from environments.toml for given network
get_contract_address() {
  local network=$1
  grep -A 1 "\[${network}\]" environments.toml | grep "contract_ttl_vault" | cut -d'"' -f2
}

# Update contract address in environments.toml for given network
set_contract_address() {
  local network=$1
  local contract_id=$2

  if [[ "$DRY_RUN" == "true" ]]; then
    echo "[dry-run] would set contract_ttl_vault = \"$contract_id\" for [$network] in environments.toml"
    return 0
  fi

  local temp_file=$(mktemp)

  awk -v net="$network" -v cid="$contract_id" '
    BEGIN { found = 0 }
    /^\['"$network"'\]/ { found = 1; print; next }
    found && /^contract_ttl_vault/ { print "contract_ttl_vault = \"" cid "\""; found = 0; next }
    found && /^\[/ { found = 0 }
    { print }
  ' environments.toml > "$temp_file"

  mv "$temp_file" environments.toml
}

# Prompt user for confirmation (returns 0 if yes, 1 if no)
confirm() {
  local prompt=$1
  local response
  read -p "$prompt (yes/no): " response
  [[ "$response" =~ ^[Yy][Ee][Ss]$ ]]
}
