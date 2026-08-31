#!/usr/bin/env bash
# scripts/test.sh
#
# Runs the Ethos-Protocol test suite and, when requested, generates a code
# coverage report using cargo-llvm-cov.
#
# Usage:
#   ./scripts/test.sh                 # run tests only (fast path, unchanged behavior)
#   COVERAGE=1 ./scripts/test.sh      # run tests + generate coverage report
#
# Coverage reports are written to target/coverage/ as both an lcov file
# (for CI archival / external tooling) and an HTML report (for local viewing).
#
# See docs/best-practices.md "Coverage Expectations" for the minimum
# coverage threshold policy.

set -e

MIN_COVERAGE="${MIN_COVERAGE:-70}"
COVERAGE_DIR="target/coverage"

echo "Running Ethos-Protocol tests..."

if [ "${COVERAGE:-0}" = "1" ]; then
  if ! command -v cargo-llvm-cov &> /dev/null; then
    echo "cargo-llvm-cov not found — installing..."
    cargo install cargo-llvm-cov --locked
  fi

  mkdir -p "${COVERAGE_DIR}"

  echo "Running tests under cargo-llvm-cov (threshold: ${MIN_COVERAGE}%)..."

  # lcov output for CI archival / codecov-style tooling.
  cargo llvm-cov --manifest-path contracts/ttl_vault/Cargo.toml \
    --lcov --output-path "${COVERAGE_DIR}/lcov.info"

  # Human-readable HTML report for local inspection.
  cargo llvm-cov --manifest-path contracts/ttl_vault/Cargo.toml \
    --html --output-dir "${COVERAGE_DIR}/html"

  # Summary + threshold gate. `--fail-under-lines` makes cargo-llvm-cov exit
  # non-zero if aggregate line coverage drops below MIN_COVERAGE, which is
  # what CI uses to block merges on under-tested changes.
  echo "Checking coverage threshold..."
  cargo llvm-cov --manifest-path contracts/ttl_vault/Cargo.toml \
    --fail-under-lines "${MIN_COVERAGE}" \
    --summary-only

  echo "Coverage report written to ${COVERAGE_DIR}/ (lcov.info + html/)."
else
  cargo test --manifest-path contracts/ttl_vault/Cargo.toml
fi

echo "All tests passed."
