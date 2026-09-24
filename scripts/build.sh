#!/usr/bin/env bash
set -e

# ─── Reproducible build pinning ───────────────────────────────────────────────
# Pinned explicitly (not "stable") so that two builds of the same commit, on
# different machines or at different times, use an identical toolchain.
# Keep this in sync with the rust-toolchain.toml pin and the CI toolchain pin
# in .github/workflows/ci.yml / reproducible-build.yml.
EXPECTED_RUST_VERSION="1.96.1"

if command -v rustc &> /dev/null; then
  ACTUAL_RUST_VERSION="$(rustc --version | awk '{print $2}')"
  if [ "${ACTUAL_RUST_VERSION}" != "${EXPECTED_RUST_VERSION}" ]; then
    echo "Warning: rustc ${ACTUAL_RUST_VERSION} is active, but this project pins ${EXPECTED_RUST_VERSION}."
    echo "Build output may not be byte-identical to release artifacts. Install with:"
    echo "  rustup install ${EXPECTED_RUST_VERSION} && rustup override set ${EXPECTED_RUST_VERSION}"
  fi
fi

# Normalize the environment so two independent builds of the same commit
# produce byte-identical WASM. Rustc embeds the working directory and commit
# metadata into debug info unless told not to; these flags strip that out.
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-0}"
export CARGO_INCREMENTAL=0
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$(pwd)=. -C metadata="

# Load environment variables from .env if it exists
if [ -f .env ]; then
  # Use grep/xargs to avoid exporting comments or malformed lines
  export $(grep -v '^#' .env | xargs)
fi

# Audit required environment variables
# Note: build.sh itself doesn't strictly need these for compilation, but we check them
# because they are essential for the overall Ethos-Protocol setup as per README.
REQUIRED_VARS=("STELLAR_NETWORK" "STELLAR_RPC_URL" "REMINDER_EMAIL_API_KEY" "REMINDER_SMS_API_KEY")

for var in "${REQUIRED_VARS[@]}"; do
  if [ -z "${!var}" ]; then
    echo "Warning: Required environment variable '$var' is not set. Check your .env file."
  fi
done

echo "Building Ethos-Protocol contracts (rustc pinned to ${EXPECTED_RUST_VERSION})..."
cargo build --target wasm32-unknown-unknown --release --manifest-path contracts/ttl_vault/Cargo.toml
cargo build --target wasm32-unknown-unknown --release --manifest-path contracts/zk_verifier/Cargo.toml
cargo build --target wasm32-unknown-unknown --release --manifest-path contracts/sbt/Cargo.toml
echo "Build complete."

# ─── Emit build artifact hashes ───────────────────────────────────────────────
# Used by the reproducible-build CI job to diff two independent builds of the
# same commit. Kept here (rather than duplicated in CI) so local builds can
# also be checked with `sha256sum -c` against a known-good hash file.
WASM_DIR="target/wasm32-unknown-unknown/release"
HASH_FILE="target/wasm-hashes.txt"

if [ -d "${WASM_DIR}" ]; then
  echo "Recording WASM artifact hashes to ${HASH_FILE}..."
  find "${WASM_DIR}" -maxdepth 1 -name '*.wasm' -type f -print0 \
    | sort -z \
    | xargs -0 sha256sum > "${HASH_FILE}"
  cat "${HASH_FILE}"
fi
