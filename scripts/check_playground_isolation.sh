#!/usr/bin/env bash
# CI guard: verify the Playground's configured network cannot resolve to
# production (mainnet) endpoints or credentials.
#
# The Playground (docs/playground.md, backend/simulator.html) is only ever
# meant to run against `testnet` or `standalone` in environments.toml. This
# script fails the build if:
#   1. PLAYGROUND_NETWORK (or the default) is set to "mainnet".
#   2. The playground's resolved rpc_url / contract id matches the
#      [mainnet] section in environments.toml (a copy-paste config mistake).
#
# Usage: scripts/check_playground_isolation.sh [path-to-environments.toml]

set -euo pipefail

ENV_FILE="${1:-environments.toml}"
PLAYGROUND_NETWORK="${PLAYGROUND_NETWORK:-testnet}"

if [ ! -f "$ENV_FILE" ]; then
  echo "environments.toml not found at $ENV_FILE" >&2
  exit 2
fi

if [ "$PLAYGROUND_NETWORK" = "mainnet" ]; then
  echo "FAIL: PLAYGROUND_NETWORK is set to 'mainnet'. The playground must" >&2
  echo "      never target production. Use 'testnet' or 'standalone'." >&2
  exit 1
fi

extract_field() {
  local section="$1" field="$2"
  awk -v section="[$section]" -v field="$field" '
    $0 == section { in_section = 1; next }
    /^\[/ { in_section = 0 }
    in_section && $0 ~ "^"field" *=" {
      sub(/^[^=]+= */, "");
      gsub(/"/, "");
      print;
      exit
    }
  ' "$ENV_FILE"
}

MAINNET_RPC=$(extract_field "mainnet" "rpc_url")
MAINNET_CONTRACT=$(extract_field "mainnet" "contract_ttl_vault")
PLAYGROUND_RPC=$(extract_field "$PLAYGROUND_NETWORK" "rpc_url")
PLAYGROUND_CONTRACT=$(extract_field "$PLAYGROUND_NETWORK" "contract_ttl_vault")

if [ -n "$MAINNET_RPC" ] && [ "$MAINNET_RPC" = "$PLAYGROUND_RPC" ]; then
  echo "FAIL: playground network '$PLAYGROUND_NETWORK' rpc_url matches [mainnet] rpc_url ($MAINNET_RPC)." >&2
  echo "      This would let playground traffic hit production infrastructure." >&2
  exit 1
fi

if [ -n "$MAINNET_CONTRACT" ] && [ "$MAINNET_CONTRACT" != "<your-contract-id>" ] \
   && [ "$MAINNET_CONTRACT" = "$PLAYGROUND_CONTRACT" ]; then
  echo "FAIL: playground network '$PLAYGROUND_NETWORK' contract_ttl_vault matches [mainnet] contract id." >&2
  echo "      Playground writes would land in the production contract's storage." >&2
  exit 1
fi

echo "OK: playground network '$PLAYGROUND_NETWORK' is isolated from [mainnet] in $ENV_FILE."
