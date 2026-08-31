# WASM Size Budget

## Overview

The TTL-Vault contract WASM size is enforced in CI to prevent regressions that
would impact deployment costs and instruction budget constraints on Soroban.

> **CI enforcement (Issue #425):** the `Check WASM size budget` step in
> `.github/workflows/ci.yml` fails the build whenever a contract exceeds its
> hard limit.  The `Build WASM artifact` + `Check WASM size budget` steps are
> **required** branch-protection checks — a PR cannot be merged if they fail.
> Both thresholds below must be kept in sync with the values in ci.yml.

## Budget Thresholds

| Contract   | Warning threshold | Hard limit | Notes                                |
|------------|:-----------------:|:----------:|--------------------------------------|
| `ttl_vault` | **460 KB**       | **512 KB** | Build fails above hard limit; warning printed above warning threshold |

### Why two thresholds?

- **Warning threshold (460 KB):** gives early notice that the contract is
  approaching the hard limit, so engineers can start optimizing before the
  build breaks.
- **Hard limit (512 KB):** the maximum allowed size.  CI exits non-zero when
  this is exceeded, blocking the PR.

### Rationale for 512 KB

- **Deployment Cost**: Larger WASM files require more XLM to upload to Soroban.
- **Instruction Budget**: Soroban imposes limits on code size; staying well
  below 1 MB provides safety margin.
- **Performance**: Smaller WASM loads faster and uses less memory during
  contract execution.

## Monitoring

The CI pipeline checks the WASM size on every push and pull request:

```bash
# Build step
cargo build --package ttl-vault --target wasm32-unknown-unknown --release

# Size check step (simplified)
WASM_BYTES=$(stat -c%s target/wasm32-unknown-unknown/release/ttl_vault.wasm)
```

See `.github/workflows/ci.yml` → `Check WASM size budget` for the exact
thresholds used (they mirror the table above).

## Optimization Strategies

If the WASM size grows beyond the threshold, consider these optimizations:

### 1. Enable LTO (Link-Time Optimization)

Edit `contracts/ttl_vault/Cargo.toml`:

```toml
[profile.release]
lto = true
codegen-units = 1
```

### 2. Strip Unnecessary Dependencies

Review `Cargo.toml` for unused or redundant dependencies. Audit transitive
dependencies:

```bash
cargo tree --package ttl-vault --duplicates
```

### 3. Reduce Debug Symbols

Ensure `strip = true` in release profile:

```toml
[profile.release]
strip = true
```

### 4. Use `wasm-opt`

After building, optimize with `binaryen`:

```bash
npm install -g binaryen
wasm-opt -Oz target/wasm32-unknown-unknown/release/ttl_vault.wasm -o ttl_vault.wasm
```

### 5. Refactor Large Functions

Break monolithic functions into smaller, modular components to improve compiler
optimization.

## Updating the Thresholds

If legitimate growth requires increasing either threshold:

1. Justify the increase in the PR description.
2. Update **both** `.github/workflows/ci.yml` (the `Check WASM size budget`
   step) **and** the table in this document in the same commit.
3. Ensure the new hard limit still leaves adequate margin below Soroban's
   limits.
4. Tag as a breaking change if it affects the deployment pipeline.

## CI Output Examples

### Within budget

```
ttl_vault WASM size: 310 KB (317440 bytes)
  Warning threshold : 460 KB
  Hard limit        : 512 KB
✅ ttl_vault WASM size is within budget.
```

### Warning zone (between 460 KB and 512 KB)

```
ttl_vault WASM size: 475 KB (486400 bytes)
  Warning threshold : 460 KB
  Hard limit        : 512 KB
⚠️  ttl_vault WASM is within 15 KB of the hard limit — consider optimizing.
   See docs/wasm-size-budget.md for optimization strategies.
```

### Over hard limit (build fails)

```
ttl_vault WASM size: 562 KB (575488 bytes)
  Warning threshold : 460 KB
  Hard limit        : 512 KB
❌ ttl_vault WASM exceeds hard limit by 50 KB (562 KB > 512 KB)
   See docs/wasm-size-budget.md for optimization strategies.
Error: Process completed with exit code 1.
```
