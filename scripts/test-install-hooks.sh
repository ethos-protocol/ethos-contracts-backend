#!/usr/bin/env bash
# scripts/test-install-hooks.sh
#
# Validates that scripts/install-hooks.sh works correctly against a clean
# checkout: a fresh git worktree with no .git/hooks customization. This
# guards against regressions like partial installs silently succeeding.
#
# Usage:
#   ./scripts/test-install-hooks.sh

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
TMP_CLONE="$(mktemp -d)"
trap 'rm -rf "${TMP_CLONE}"' EXIT

echo "Creating clean checkout at ${TMP_CLONE}..."
git clone --quiet "${REPO_ROOT}" "${TMP_CLONE}"

cd "${TMP_CLONE}"

echo "Running install-hooks.sh against clean checkout..."
if ! ./scripts/install-hooks.sh; then
  echo "FAIL: install-hooks.sh exited non-zero on a clean checkout." >&2
  exit 1
fi

echo "Checking that pre-commit hook exists and is executable..."
HOOK="${TMP_CLONE}/.git/hooks/pre-commit"

if [[ ! -f "${HOOK}" ]]; then
  echo "FAIL: ${HOOK} was not created." >&2
  exit 1
fi

if [[ ! -x "${HOOK}" ]]; then
  echo "FAIL: ${HOOK} exists but is not executable." >&2
  exit 1
fi

echo "Checking idempotent re-run (should not fail or duplicate work)..."
if ! ./scripts/install-hooks.sh; then
  echo "FAIL: install-hooks.sh is not idempotent (second run failed)." >&2
  exit 1
fi

echo "Simulating a partial install (hook file present, exec bit stripped)..."
chmod -x "${HOOK}"
if ! ./scripts/install-hooks.sh; then
  echo "FAIL: install-hooks.sh did not repair a partial install (stripped exec bit)." >&2
  exit 1
fi

if [[ ! -x "${HOOK}" ]]; then
  echo "FAIL: exec bit was not restored on re-run." >&2
  exit 1
fi

echo "PASS: install-hooks.sh installs, verifies, and repairs hooks correctly."
