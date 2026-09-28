# Deployment Runbook

The standard, step-by-step procedure for deploying Ethos-Protocol contracts
(`ttl_vault`, `zk_verifier`, `sbt`) and the backend service to **testnet** and
**mainnet**, including the pre-deployment checklist, verification steps and
rollback procedures.

Follow this runbook top to bottom for every deployment. Do not skip steps;
if a step fails, stop and go to [Rollback](#6-rollback-procedures) or
[troubleshooting.md](troubleshooting.md).

| Related document | Covers |
|---|---|
| [deployment-guide.md](deployment-guide.md) | Background: key management, configuration, security rationale. |
| [upgrade-safety.md](upgrade-safety.md) | Upgrade manifests and compatibility rules for `upgrade_with_manifest`. |
| [disaster-recovery-runbook.md](disaster-recovery-runbook.md) | Data loss / region failure. |
| [incident-response.md](incident-response.md) | Escalation and communication during incidents. |
| [canary-deployments.md](canary-deployments.md) | Backend canary rollout. |

The facts in this runbook (script names, flags, confirmation phrases,
toolchain version, WASM paths, network URLs, contract function names and
arguments) are validated against the repository by
[`scripts/test_deployment_runbook.py`](../scripts/test_deployment_runbook.py).
Update that script's expectations whenever a script or contract entrypoint
changes.

---

## Contents

1. [Roles and prerequisites](#1-roles-and-prerequisites)
2. [Pre-deployment checklist](#2-pre-deployment-checklist)
3. [Deploy to testnet](#3-deploy-to-testnet)
4. [Deploy to mainnet](#4-deploy-to-mainnet)
5. [Deploy the backend](#5-deploy-the-backend)
6. [Rollback procedures](#6-rollback-procedures)
7. [Post-deployment verification](#7-post-deployment-verification)
8. [Deployment record template](#8-deployment-record-template)

---

## 1. Roles and prerequisites

### Roles

| Role | Responsibility |
|---|---|
| **Deployer** | Runs the scripts, owns the deployment record. |
| **Reviewer** | Second person; confirms checklist items and every typed confirmation on mainnet. |
| **On-call** | Watches dashboards/alerts during and 60 minutes after the deploy ([on-call-rotation.md](on-call-rotation.md)). |

Mainnet deployments **require** a deployer and a reviewer on a call together.

### Tooling

| Tool | Version | Check |
|---|---|---|
| Rust | `1.96.1` (pinned in `rust-toolchain.toml` and `scripts/build.sh`) | `rustc --version` |
| `wasm32-unknown-unknown` target | — | `rustup target list --installed` |
| Stellar CLI | same version as CI (`.github/workflows/testnet-smoke.yml`) | `stellar --version` |
| Docker + Compose | for the backend | `docker compose version` |

### Identities

| Network | Stellar CLI identity | Notes |
|---|---|---|
| testnet | `deployer` | Hard-coded in `scripts/deploy_testnet.sh`. |
| mainnet | `deployer-mainnet` | Default in `scripts/deploy_mainnet.sh`; override with `DEPLOYER_IDENTITY`. |

```bash
stellar keys address deployer
stellar keys address deployer-mainnet
```

### Networks (`environments.toml`)

| Network | RPC URL | Passphrase |
|---|---|---|
| testnet | `https://soroban-testnet.stellar.org` | `Test SDF Network ; September 2015` |
| mainnet | `https://soroban-rpc.mainnet.stellar.org` | `Public Global Stellar Network ; September 2015` |
| futurenet | `https://rpc-futurenet.stellar.org` | `Test SDF Future Network ; October 2022` |
| standalone | `http://localhost:8000/soroban/rpc` | `Standalone Network ; February 2017` |

`contract_ttl_vault` must stay the **first key** under each section — the
deploy scripts parse it positionally.

---

## 2. Pre-deployment checklist

Copy this checklist into the deployment record (§8) and tick each item. Any
unticked item blocks the deploy.

### 2.1 Code

- [ ] The release commit is merged to `main` and CI is green (tests, fmt, clippy, audit, gitleaks, docs-drift, WASM size budget).
- [ ] `CHANGELOG.md` has an entry for this version and it matches the version in `contracts/ttl_vault/src/lib.rs` (CI "Check version consistency").
- [ ] The reproducible-build workflow passed for this commit.
- [ ] Any storage-layout or interface change has an upgrade manifest reviewed per [upgrade-safety.md](upgrade-safety.md).
- [ ] Security review completed for contract changes ([security-audit-checklist.md](security-audit-checklist.md)).

### 2.2 Build

- [ ] Local toolchain is `1.96.1` (`rustc --version`).
- [ ] `./scripts/build.sh` completes and writes `target/wasm-hashes.txt`.
- [ ] The hashes in `target/wasm-hashes.txt` match the CI reproducible-build artefact for the same commit.

### 2.3 Accounts and configuration

- [ ] Deployer identity exists (`stellar keys address ...`) and is funded (≥ 10 XLM on mainnet).
- [ ] Admin address for `initialize` is confirmed by the reviewer, character by character.
- [ ] `STELLAR_MAINNET_RPC_URL` is exported (mainnet only).
- [ ] `.deploy-state/<network>.state` reviewed: you know which steps are already recorded as done.
- [ ] `environments.toml` reviewed: the current `contract_ttl_vault` for the target network is recorded in the deployment record (needed for rollback).
- [ ] Backend env vars set: `MIN_CONTRACT_VERSION`, `ADMIN_API_KEY`, `DATABASE_URL`, `REDIS_URL`, and optionally `CHAINALYSIS_API_KEY`.

### 2.4 Rollback readiness

- [ ] Previous WASM hash for each contract recorded (§8) so `upgrade` can restore it.
- [ ] Previous backend image tag recorded.
- [ ] Database backup taken and validated ([backup-validation.md](backup-validation.md)).
- [ ] On-call engineer acknowledged the deploy window.

### 2.5 Communication

- [ ] Deploy window announced to the team channel.
- [ ] Status page / integrators notified for mainnet deploys with user-visible changes.

---

## 3. Deploy to testnet

Always deploy the exact same commit to testnet before mainnet.

### Step 3.1 — Build and record hashes

```bash
git checkout main && git pull --ff-only
./scripts/build.sh
cat target/wasm-hashes.txt
```

**Verify:** three `.wasm` files are listed (`ttl_vault.wasm`,
`zk_verifier.wasm`, `sbt.wasm`) and no toolchain warning was printed.

### Step 3.2 — Dry run

```bash
./scripts/deploy_testnet.sh --dry-run
```

**Verify:** every action is printed with a `[dry-run]` prefix, no files under
`.deploy-state/` or `environments.toml` change (`git status` is clean).

### Step 3.3 — Deploy

```bash
./scripts/deploy_testnet.sh
```

The script is idempotent. If `environments.toml` already holds a real
contract id for testnet, or `.deploy-state/testnet.state` contains
`contract_deployed`, the deploy step is **skipped**. To intentionally
redeploy, pass `--force` and type the confirmation phrase
`FORCE-REDEPLOY-TESTNET` when prompted.

**Verify:** the script prints `✓ Contract deployed: C...`, and
`environments.toml` now contains that id under `[testnet]`.

```bash
CONTRACT_ID=$(grep -A 1 '\[testnet\]' environments.toml | grep contract_ttl_vault | cut -d'"' -f2)
echo "$CONTRACT_ID"
```

### Step 3.4 — Initialize

The deploy script only records `admin_initialized` in the marker file;
`initialize` is invoked here, explicitly, so the admin address is reviewed.

```bash
XLM_TOKEN=$(stellar contract id asset --asset native --network testnet)
ADMIN_ADDR=$(stellar keys address deployer)

stellar contract invoke \
  --id "$CONTRACT_ID" \
  --source deployer \
  --network testnet \
  -- initialize \
  --xlm_token "$XLM_TOKEN" \
  --admin "$ADMIN_ADDR"
```

**Verify:**

```bash
stellar contract invoke --id "$CONTRACT_ID" --source deployer --network testnet -- get_admin
stellar contract invoke --id "$CONTRACT_ID" --source deployer --network testnet -- is_paused
```

`get_admin` returns `ADMIN_ADDR`; `is_paused` returns `false`. A second
`initialize` must fail with `Error(Contract, #1)` (`AlreadyInitialized`).

### Step 3.5 — Smoke test

Trigger the **Testnet Smoke Test** workflow (`.github/workflows/testnet-smoke.yml`)
or run the equivalent vault lifecycle by hand (create vault → deposit →
check in → cancel). All steps must succeed before continuing to mainnet.

---

## 4. Deploy to mainnet

> ⚠️ Mainnet deploys are irreversible on-chain. Deployer and reviewer must
> both be present. Read each prompt aloud before answering it.

### Step 4.1 — Environment

```bash
export STELLAR_MAINNET_RPC_URL="https://soroban-rpc.mainnet.stellar.org"
export DEPLOYER_IDENTITY="deployer-mainnet"
stellar keys address "$DEPLOYER_IDENTITY"
```

`deploy_mainnet.sh` exits immediately if `STELLAR_MAINNET_RPC_URL` is unset.

### Step 4.2 — Dry run

```bash
./scripts/deploy_mainnet.sh --dry-run
```

**Verify:** the banner shows `Network : mainnet`, the expected identity and
RPC URL, and `Mode : DRY RUN`. If it reports an existing contract, stop and
confirm with the reviewer that a redeploy is really intended.

### Step 4.3 — Deploy

```bash
./scripts/deploy_mainnet.sh
```

When prompted, type `mainnet` to confirm. For an intentional redeploy over an
existing contract, pass `--force`; you will additionally be asked to type
`FORCE-REDEPLOY-MAINNET`.

**Verify:** `✓ Contract deployed: C...` is printed and `environments.toml`
`[mainnet]` holds the new id. Commit the `environments.toml` change in a PR
referencing the deployment record.

### Step 4.4 — Initialize

Same as §3.4 with `--network mainnet --source deployer-mainnet
--rpc-url "$STELLAR_MAINNET_RPC_URL"`, and with the **reviewer-confirmed**
admin address (normally a multisig, not the deployer key):

```bash
XLM_TOKEN=$(stellar contract id asset --asset native --network mainnet)

stellar contract invoke \
  --id "$CONTRACT_ID" \
  --source deployer-mainnet \
  --network mainnet \
  --rpc-url "$STELLAR_MAINNET_RPC_URL" \
  -- initialize \
  --xlm_token "$XLM_TOKEN" \
  --admin "$MAINNET_ADMIN_ADDR"
```

**Verify:** run §7 in full.

### Step 4.5 — Upgrading an existing mainnet contract (instead of redeploying)

For code changes to an already-deployed contract, prefer an in-place upgrade,
which keeps the contract id and all storage:

```bash
NEW_WASM_HASH=$(stellar contract upload \
  --wasm target/wasm32-unknown-unknown/release/ttl_vault.wasm \
  --source deployer-mainnet \
  --network mainnet \
  --rpc-url "$STELLAR_MAINNET_RPC_URL")

stellar contract invoke \
  --id "$CONTRACT_ID" \
  --source "$ADMIN_IDENTITY" \
  --network mainnet \
  --rpc-url "$STELLAR_MAINNET_RPC_URL" \
  -- upgrade \
  --new_wasm_hash "$NEW_WASM_HASH"
```

Use `upgrade_with_manifest` when a manifest baseline exists — see
[upgrade-safety.md](upgrade-safety.md). Record both the old and new WASM
hashes in the deployment record.

---

## 5. Deploy the backend

The backend checks the on-chain contract version at startup and **exits** if
it is lower than `MIN_CONTRACT_VERSION`, so deploy contracts first, then the
backend.

### Step 5.1 — Build and start

```bash
docker compose build backend
docker compose up -d db redis
docker compose up -d backend
```

For production orchestrators, build from `backend/Dockerfile`, tag the image
with the commit SHA and roll it out behind a canary
([canary-deployments.md](canary-deployments.md)).

### Step 5.2 — Verify

```bash
curl -sf http://localhost:3000/health
curl -sf http://localhost:3000/ready
curl -sf http://localhost:3000/health/consensus
docker compose logs --tail=50 backend
```

**Verify:** `/health` → `{"status":"ok",...}`; `/ready` →
`"database":"connected"`; `/health/consensus` → `"status":"ok"`; logs
contain `listening on 0.0.0.0:3000` and no `Contract version check failed`.

---

## 6. Rollback procedures

Decide quickly. Roll back if any §7 check fails, error rates rise above the
alert thresholds in [runbook-alerts.md](runbook-alerts.md), or funds-handling
behaviour is in doubt.

### Decision table

| Symptom | Action |
|---|---|
| Contract misbehaving, funds at risk | **6.1 Pause** immediately, then 6.2 |
| New contract logic wrong, storage fine | **6.2 Upgrade back** to the previous WASM hash |
| Fresh deploy (new contract id) is wrong | **6.3 Revert** `environments.toml` to the previous id |
| Backend errors / crash loop | **6.4 Backend rollback** |
| Script failed midway | **6.5 Partial deploy recovery** |

### 6.1 Pause the contract (seconds)

```bash
stellar contract invoke \
  --id "$CONTRACT_ID" \
  --source "$ADMIN_IDENTITY" \
  --network "$NETWORK" \
  -- pause \
  --reason 696e636964656e74
```

`--reason` is hex-encoded bytes (`696e636964656e74` = `incident`).
**Verify:** `is_paused` returns `true`. Unpause with `-- unpause` once resolved.

### 6.2 Upgrade back to the previous WASM (minutes)

The previous WASM is still installed on-chain; re-point the contract to it
using the hash recorded in the deployment record:

```bash
stellar contract invoke \
  --id "$CONTRACT_ID" \
  --source "$ADMIN_IDENTITY" \
  --network "$NETWORK" \
  -- upgrade \
  --new_wasm_hash "$PREVIOUS_WASM_HASH"
```

**Verify:** run §7. If the failed version wrote storage in a new layout, the
old code may not read it — consult [upgrade-safety.md](upgrade-safety.md)
before rolling back, and keep the contract paused until confirmed.

### 6.3 Revert to the previous contract id

A fresh deploy creates a new contract; the previous one is untouched. Point
clients back to it:

```bash
git revert --no-edit "$ENVIRONMENTS_COMMIT"
```

Then redeploy the backend (and clients) with `CONTRACT_TTL_VAULT` set to the
previous id. Do **not** delete `.deploy-state/<network>.state` unless you
intend the next script run to redeploy.

### 6.4 Backend rollback

```bash
docker compose stop backend
docker tag "ethos-protocol-backend:$PREVIOUS_TAG" ethos-protocol-backend:latest
docker compose up -d backend
curl -sf http://localhost:3000/ready
```

If a database migration shipped with the release, restore from the §2.4
backup ([disaster-recovery-runbook.md](disaster-recovery-runbook.md)); see
[migration-testing.md](migration-testing.md) for reversible migrations.

### 6.5 Partial deploy recovery

The deploy scripts record completed steps in `.deploy-state/<network>.state`
(`contract_deployed`, `admin_initialized`). After fixing the cause, simply
re-run the same script — completed steps are skipped. To start completely
fresh for a network (e.g. a failed testnet deploy you want to discard):

```bash
rm -f .deploy-state/testnet.state
./scripts/deploy_testnet.sh --dry-run
```

Never clear mainnet state without the reviewer's agreement.

---

## 7. Post-deployment verification

Run all of these after every deploy, upgrade or rollback. Record results in
the deployment record.

| # | Check | Command | Expected |
|---|---|---|---|
| V1 | Contract id recorded | `grep -A 1 "\[$NETWORK\]" environments.toml` | New (or intended) id |
| V2 | Admin correct | `-- get_admin` | Reviewer-confirmed admin |
| V3 | Not paused | `-- is_paused` | `false` (unless intentionally paused) |
| V4 | WASM hash matches build | `stellar contract info` / compare with `target/wasm-hashes.txt` | Identical |
| V5 | Vault lifecycle | Smoke test (§3.5) — testnet only | All steps succeed |
| V6 | Backend health | `curl -sf .../health` | `status: ok` |
| V7 | Backend readiness | `curl -sf .../ready` | `database: connected` |
| V8 | Cache consensus | `curl -sf .../health/consensus` | `status: ok` |
| V9 | Metrics scraping | `curl -sf .../metrics` | Prometheus text, non-empty |
| V10 | Alerts quiet | Dashboards / `GET /anomaly/alerts` | No new alerts for 60 min |

```bash
curl -sf http://localhost:3000/metrics | head -20
curl -sf http://localhost:3000/anomaly/alerts
```

---

## 8. Deployment record template

Create one record per deployment (e.g. in the release PR description).

```text
Deployment: <version> to <network>
Date/time (UTC):
Deployer:                Reviewer:              On-call:
Commit SHA:
WASM hashes (target/wasm-hashes.txt):
Previous contract id (environments.toml):
New contract id:
Previous WASM hash:      New WASM hash:
Previous backend tag:    New backend tag:
Checklist §2: all ticked? (Y/N)
Verification §7: V1..V10 results:
Rollback performed? (Y/N, which procedure, why):
```
