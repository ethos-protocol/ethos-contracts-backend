# Configuration Reference Guide

This guide documents all configuration options for Ethos-Protocol, including environment variables, contract parameters, network settings, and backend configuration. Each option includes its impact, recommended values, validation rules, and precedence information.

> **Authoritative variable list**: The set of environment variables read by the code is generated from source by `scripts/check_env_vars.py`, which compares it against `.env.example` in CI. If this document and `.env.example` ever disagree, `scripts/check_env_vars.py` and `.env.example` are the source of truth. Run `python scripts/check_env_vars.py` locally to see any variables that are read but undocumented.

## Table of Contents

- [Configuration Precedence](#configuration-precedence)
- [Environment Variables](#environment-variables)
  - [Stellar Network](#stellar-network)
  - [Contract Addresses](#contract-addresses)
  - [Backend Service](#backend-service)
  - [Notification Services](#notification-services)
  - [Database](#database)
  - [Read Replicas](#read-replicas)
  - [Redis & Consensus](#redis--consensus)
  - [RPC Pool](#rpc-pool)
  - [Tracing & Logging](#tracing--logging)
  - [Security & AML](#security--aml)
  - [Reverse Proxy](#reverse-proxy)
  - [Encryption & Secrets](#encryption--secrets)
  - [Authentication](#authentication)
  - [Monitoring](#monitoring)
- [Contract Parameters](#contract-parameters)
  - [Vault Parameters](#vault-parameters)
  - [TTL & Check-in Parameters](#ttl--check-in-parameters)
  - [Beneficiary Parameters](#beneficiary-parameters)
  - [Vesting Parameters](#vesting-parameters)
- [Network Configurations (environments.toml)](#network-configurations-environmentstoml)
- [Docker Configuration](#docker-configuration)
- [Configuration Validation](#configuration-validation)
- [Configuration Examples](#configuration-examples)

---

## Configuration Precedence

Configuration values are resolved in the following order (highest to lowest priority):

1. **Runtime environment variables** — set in the shell or Docker environment
2. **`.env` file** — loaded at startup from the project root
3. **`environments.toml`** — network-specific defaults
4. **Built-in contract defaults** — compiled into the smart contract
5. **Soroban network defaults** — provided by the Stellar network

When the same option is set in multiple places, the higher-priority source wins.

> **Note**: `.env.local` overrides `.env` for local development. Never commit `.env` or `.env.local` to source control.

---

## Environment Variables

Copy `.env.example` to `.env` to get started:

```bash
cp .env.example .env
```

The tables below use the following status values:

- **Required** — the backend refuses to start (or the feature fails) without it.
- **Optional** — has a safe default; set only when you need to override it.
- **Conditional** — required only when the related feature is enabled (e.g. Redis when consensus is enabled).

### Stellar Network

| Variable | Type | Default | Status | Description |
|---|---|---|---|---|
| `STELLAR_NETWORK` | `string` | `testnet` | Required | Target Stellar network (`testnet`, `mainnet`, `futurenet`, `standalone`) |
| `STELLAR_RPC_URL` | `string` | *(see environments.toml)* | Required | Soroban RPC endpoint URL |
| `STELLAR_MAINNET_RPC_URL` | `string` | — | Conditional | Mainnet-specific RPC URL (required for mainnet deploys) |
| `STELLAR_HORIZON_URL` | `string` | *(network default)* | Optional | Horizon API URL for balance/account queries |
| `NETWORK_PASSPHRASE` | `string` | *(network default)* | Optional | Stellar network passphrase |
| `RPC_ENDPOINT` | `string` | *(falls back to `STELLAR_RPC_URL`)* | Optional | Explicit Soroban RPC endpoint used by the backend RPC client; overrides `STELLAR_RPC_URL` when set |

**Impact**: Incorrect network settings will cause all contract calls to fail or target the wrong chain.

**Validation**: `STELLAR_NETWORK` must be one of the four supported network names. `STELLAR_RPC_URL` and `RPC_ENDPOINT` must be valid `http://` or `https://` URLs.

**Recommended values**:

```env
# Testnet
STELLAR_NETWORK=testnet
STELLAR_RPC_URL=https://soroban-testnet.stellar.org

# Mainnet
STELLAR_NETWORK=mainnet
STELLAR_RPC_URL=https://mainnet.sorobanrpc.com
```

---

### Contract Addresses

| Variable | Type | Default | Status | Description |
|---|---|---|---|---|
| `CONTRACT_TTL_VAULT` | `string` | — | Required | Deployed `ttl_vault` contract ID (Strkey format) |
| `CONTRACT_SBT` | `string` | — | Optional | Deployed `sbt` contract ID |
| `CONTRACT_ZK_VERIFIER` | `string` | — | Optional | Deployed `zk_verifier` contract ID |
| `RESTORE_VAULT_CALLER` | `string` | — | Optional | Owner/admin address used by disaster-recovery automation; must match `DEPLOYER_IDENTITY` |

**Impact**: If these are missing or incorrect, all vault operations will fail.

**Validation**: Must be valid Stellar contract IDs (56-character Strkey starting with `C`).

```env
CONTRACT_TTL_VAULT=CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4
```

---

### Backend Service

| Variable | Type | Default | Status | Description |
|---|---|---|---|---|
| `BACKEND_HOST` | `string` | `0.0.0.0` | Optional | Address the backend HTTP server binds to |
| `BACKEND_PORT` | `integer` | `3000` | Optional | Port the backend listens on |
| `NODE_ID` | `string` | *(hostname)* | Optional | Unique identifier for this backend instance; used in logs, metrics labels, and consensus leader election. Must be unique per replica |
| `RUST_LOG` | `string` | `info` | Optional | Log level (`error`, `warn`, `info`, `debug`, `trace`) |
| `RUST_BACKTRACE` | `string` | `0` | Optional | Enable backtraces (`0`, `1`, `full`) |
| `JWT_SECRET` | `string` | — | Required | Secret key for signing JWT tokens (min 32 bytes) |
| `SESSION_TTL_SECONDS` | `integer` | `3600` | Optional | Session expiry in seconds |

**Impact**: `JWT_SECRET` must be set for authenticated endpoints to work. Use a cryptographically random value. `NODE_ID` must be unique across replicas or consensus coordination will misbehave.

**Validation**: `BACKEND_PORT` must be `1`–`65535`. `NODE_ID` must be a non-empty string matching `[A-Za-z0-9._-]+`. `JWT_SECRET` must be at least 32 bytes.

**Recommended**:

```bash
# Generate a secure JWT secret
openssl rand -hex 32
```

```env
BACKEND_PORT=3000
NODE_ID=ethos-backend-1
RUST_LOG=info
JWT_SECRET=<output-from-openssl>
```

---

### Notification Services

| Variable | Type | Default | Status | Description |
|---|---|---|---|---|
| `REMINDER_EMAIL_API_KEY` | `string` | — | Optional | API key for email reminder service |
| `REMINDER_SMS_API_KEY` | `string` | — | Optional | API key for SMS reminder service |
| `REMINDER_EMAIL_FROM` | `string` | — | Optional | Sender email address for reminders |
| `REMINDER_LEAD_TIME_HOURS` | `integer` | `72` | Optional | Hours before TTL expiry to send first reminder |
| `REMINDER_SECOND_LEAD_TIME_HOURS` | `integer` | `24` | Optional | Hours before expiry for second reminder |
| `PUSH_NOTIFICATION_KEY` | `string` | — | Optional | API key for push notification provider |

**Impact**: Without these keys, owners will not receive expiry reminders and may miss check-in deadlines.

**Validation**: `REMINDER_LEAD_TIME_HOURS` and `REMINDER_SECOND_LEAD_TIME_HOURS` must be positive integers, and the second lead time must be smaller than the first. `REMINDER_EMAIL_FROM` must be a valid email address.

**Recommended**:

```env
REMINDER_LEAD_TIME_HOURS=72
REMINDER_SECOND_LEAD_TIME_HOURS=24
```

---

### Database

| Variable | Type | Default | Status | Description |
|---|---|---|---|---|
| `DATABASE_URL` | `string` | — | Required | PostgreSQL connection string |
| `DB_MAX_CONNECTIONS` | `integer` | `10` | Optional | Maximum connection pool size |
| `DB_MIN_CONNECTIONS` | `integer` | `1` | Optional | Minimum idle connections |
| `DB_CONNECT_TIMEOUT_SECONDS` | `integer` | `30` | Optional | Connection timeout |
| `DB_IDLE_TIMEOUT_SECONDS` | `integer` | `600` | Optional | Idle connection timeout |
| `DB_POOL_MIN` | `integer` | `2` | Optional | Read connections opened at startup (`db_read_pool.rs`); capped at `DB_POOL_MAX` |
| `DB_POOL_MAX` | `integer` | `10` | Optional | Maximum read connections (`db_read_pool.rs`). In-memory databases use 1 regardless. See [architecture](architecture.md#database-concurrency-model) |
| `DB_POOL_MAX_CHECKOUT_SECS` | `integer` | `60` | Optional | How long a connection may stay checked out before being flagged as a suspected leak |

**Validation**: `DATABASE_URL` must be a valid PostgreSQL URI. The schema must be initialized before starting the backend. `DB_MIN_CONNECTIONS` must be ≤ `DB_MAX_CONNECTIONS`; `DB_POOL_MIN` must be ≤ `DB_POOL_MAX`. All timeout values must be positive integers.

**Recommended**:

```env
DATABASE_URL=postgres://user:password@localhost:5432/ethos
DB_MAX_CONNECTIONS=10
```

### Connection leak detection

`backend/src/pool_optimizer.rs`'s adaptive connection pool tracks a checkout
timestamp for every connection it hands out. A connection still checked out
longer than `DB_POOL_MAX_CHECKOUT_SECS` (default `60`) is treated as a
suspected leak — code that acquired a connection and never returned it
(e.g. via an early `return` that skips dropping the guard, or a stuck
downstream call holding it indefinitely).

- `OptimizedConnectionPool::detect_leaks()` returns the list of suspected
  leaks (connection ID + how long it's been held).
- `PoolMetrics::suspected_leaks` exposes the current count for dashboards.
- `OptimizedConnectionPool::check_for_leaks_and_alert(oncall_state,
  schedule_id)` logs an error for each suspected leak and pages the given
  on-call schedule via `oncall::raise_alert` (`backend/src/oncall.rs`). Call
  this periodically (e.g. alongside `maintain()`) from a background task.

Set `DB_POOL_MAX_CHECKOUT_SECS` above your slowest expected query/transaction
duration to avoid false positives; lower it in environments where connection
leaks are a known risk (e.g. after a recent incident) to catch them sooner.

---

### Read Replicas

| Variable | Type | Default | Status | Description |
|---|---|---|---|---|
| `READ_REPLICA_URLS` | `string` | *(empty)* | Optional | Comma-separated list of PostgreSQL read-replica connection strings. When empty, all reads go to `DATABASE_URL` |
| `READ_REPLICA_MAX_LAG_SECONDS` | `integer` | `5` | Optional | Maximum acceptable replication lag before a replica is skipped for reads |
| `READ_REPLICA_HEALTH_CHECK_SECONDS` | `integer` | `30` | Optional | Interval between replica health checks |

**Impact**: When configured, read-only queries are load-balanced across replicas, reducing load on the primary. If a replica exceeds `READ_REPLICA_MAX_LAG_SECONDS` it is temporarily removed from rotation.

**Validation**: Each entry in `READ_REPLICA_URLS` must be a valid PostgreSQL URI. Lag and health-check values must be positive integers.

```env
READ_REPLICA_URLS=postgres://user:pass@replica1:5432/ethos,postgres://user:pass@replica2:5432/ethos
READ_REPLICA_MAX_LAG_SECONDS=5
```

---

### Redis & Consensus

| Variable | Type | Default | Status | Description |
|---|---|---|---|---|
| `REDIS_URL` | `string` | — | Conditional | Redis connection string. Required when consensus or distributed locking is enabled |
| `CONSENSUS_STRATEGY` | `string` | `single` | Optional | Consensus strategy for multi-node coordination (`single`, `raft`, `quorum`) |
| `CONSENSUS_QUORUM_SIZE` | `integer` | `2` | Conditional | Number of nodes that must agree; required when `CONSENSUS_STRATEGY=quorum` |
| `CONSENSUS_ELECTION_TIMEOUT_MS` | `integer` | `5000` | Optional | Leader-election timeout in milliseconds |
| `REDIS_POOL_SIZE` | `integer` | `10` | Optional | Maximum Redis connection pool size |
| `REDIS_COMMAND_TIMEOUT_MS` | `integer` | `1000` | Optional | Per-command Redis timeout in milliseconds |

**Impact**: `REDIS_URL` is required for any multi-node deployment. With `CONSENSUS_STRATEGY=single` the backend runs standalone and Redis is only used for caching/locking.

**Validation**: `REDIS_URL` must be a valid `redis://` or `rediss://` URI. `CONSENSUS_STRATEGY` must be one of the listed values. `CONSENSUS_QUORUM_SIZE` must be ≥ 1 and ≤ the number of configured nodes. Timeouts must be positive integers.

```env
REDIS_URL=redis://localhost:6379
CONSENSUS_STRATEGY=quorum
CONSENSUS_QUORUM_SIZE=2
NODE_ID=ethos-backend-1
```

---

### RPC Pool

| Variable | Type | Default | Status | Description |
|---|---|---|---|---|
| `RPC_POOL_SIZE` | `integer` | `4` | Optional | Number of concurrent RPC connections kept open |
| `RPC_MAX_RETRIES` | `integer` | `3` | Optional | Retry attempts for failed RPC calls |
| `RPC_RETRY_BACKOFF_MS` | `integer` | `500` | Optional | Base backoff between RPC retries in milliseconds |
| `RPC_REQUEST_TIMEOUT_MS` | `integer` | `10000` | Optional | Per-request RPC timeout in milliseconds |
| `RPC_FALLBACK_ENDPOINTS` | `string` | *(empty)* | Optional | Comma-separated fallback RPC URLs used when the primary endpoint fails |

**Impact**: Tuning these controls resilience against RPC outages and rate limits. Fallback endpoints are tried in order after the primary fails.

**Validation**: All numeric values must be positive integers. Each fallback endpoint must be a valid `http://` or `https://` URL.

```env
RPC_POOL_SIZE=4
RPC_MAX_RETRIES=3
RPC_FALLBACK_ENDPOINTS=https://backup1.sorobanrpc.com,https://backup2.sorobanrpc.com
```

---

### Tracing & Logging

| Variable | Type | Default | Status | Description |
|---|---|---|---|---|
| `TRACE_ENABLED` | `bool` | `false` | Optional | Enable OpenTelemetry trace export |
| `TRACE_ENDPOINT` | `string` | — | Conditional | OTLP collector endpoint; required when `TRACE_ENABLED=true` |
| `TRACE_SAMPLE_RATE` | `float` | `0.1` | Optional | Fraction of traces to sample (`0.0`–`1.0`) |
| `LOG_FORMAT` | `string` | `json` | Optional | Log output format (`json`, `pretty`, `compact`) |
| `LOG_LEVEL` | `string` | *(falls back to `RUST_LOG`)* | Optional | Explicit log level override |

**Impact**: Enabling tracing without a valid `TRACE_ENDPOINT` causes startup failure. High `TRACE_SAMPLE_RATE` values increase overhead and collector load.

**Validation**: `TRACE_ENABLED` must be `true`/`false`. `TRACE_SAMPLE_RATE` must be between `0.0` and `1.0`. `LOG_FORMAT` must be one of the listed values.

```env
TRACE_ENABLED=true
TRACE_ENDPOINT=http://otel-collector:4317
TRACE_SAMPLE_RATE=0.1
LOG_FORMAT=json
```

---

### Security & AML

| Variable | Type | Default | Status | Description |
|---|---|---|---|---|
| `AML_ENABLED` | `bool` | `true` | Optional | Enable anti-money-laundering screening on transfers |
| `AML_FAIL_OPEN` | `bool` | `false` | Optional | When `true`, transfers proceed if the AML provider is unreachable; when `false`, they are blocked. Keep `false` in production |
| `AML_PROVIDER_URL` | `string` | — | Conditional | AML screening provider endpoint; required when `AML_ENABLED=true` |
| `AML_API_KEY` | `string` | — | Conditional | API key for the AML provider |
| `AML_RISK_THRESHOLD` | `integer` | `70` | Optional | Risk score (`0`–`100`) above which a transfer is flagged |
| `RATE_LIMIT_REQUESTS_PER_MINUTE` | `integer` | `60` | Optional | Per-client request rate limit |
| `CORS_ALLOWED_ORIGINS` | `string` | *(empty)* | Optional | Comma-separated list of allowed CORS origins |

**Impact**: `AML_FAIL_OPEN=true` weakens compliance guarantees — only use it in non-production environments. `AML_FAIL_OPEN` defaults to `false` so that an unreachable provider blocks transfers rather than silently allowing them.

**Validation**: Boolean values must be `true`/`false`. `AML_RISK_THRESHOLD` must be `0`–`100`. `AML_PROVIDER_URL` must be a valid URL. `CORS_ALLOWED_ORIGINS` entries must be valid origins.

```env
AML_ENABLED=true
AML_FAIL_OPEN=false
AML_PROVIDER_URL=https://aml.example.com/screen
AML_RISK_THRESHOLD=70
```

---

### Reverse Proxy

| Variable | Type | Default | Status | Description |
|---|---|---|---|---|
| `TRUSTED_PROXY_IPS` | `string` | *(empty)* | Optional | Comma-separated list of proxy IPs/CIDRs whose `X-Forwarded-For` headers are trusted. When empty, forwarded headers are ignored |
| `TRUST_PROXY_HEADERS` | `bool` | `false` | Optional | Whether to honor `X-Forwarded-For`/`X-Forwarded-Proto` headers at all |

**Impact**: Only list proxies you control. Trusting arbitrary proxies allows clients to spoof their source IP, defeating rate limiting and audit logging.

**Validation**: Each entry must be a valid IPv4/IPv6 address or CIDR block.

```env
TRUST_PROXY_HEADERS=true
TRUSTED_PROXY_IPS=10.0.0.0/8,192.168.1.1
```

---

### Encryption & Secrets

| Variable | Type | Default | Status | Description |
|---|---|---|---|---|
| `ENCRYPTION_KEY` | `string` | — | Conditional | 32-byte key (hex or base64) used to encrypt sensitive fields at rest; required when field encryption is enabled |
| `ENCRYPTION_KEY_ID` | `string` | `default` | Optional | Identifier for the active encryption key, used during key rotation |
| `SECRETS_BACKEND` | `string` | `env` | Optional | Secrets source (`env`, `vault`, `aws-secrets-manager`) |

**Impact**: Losing `ENCRYPTION_KEY` makes encrypted data unrecoverable. Rotate keys using `ENCRYPTION_KEY_ID` so old data can still be decrypted.

**Validation**: `ENCRYPTION_KEY` must decode to exactly 32 bytes. `SECRETS_BACKEND` must be one of the listed values.

```bash
# Generate a 32-byte encryption key
openssl rand -hex 32
```

```env
ENCRYPTION_KEY=<output-from-openssl>
ENCRYPTION_KEY_ID=2024-06
```

---

### Authentication

| Variable | Type | Default | Status | Description |
|---|---|---|---|---|
| `PASSKEY_RP_ID` | `string` | — | Required | Relying Party ID for WebAuthn (typically your domain) |
| `PASSKEY_RP_ORIGIN` | `string` | — | Required | Origin URL where Passkey authentication occurs |
| `PASSKEY_TIMEOUT_MS` | `integer` | `60000` | Optional | WebAuthn ceremony timeout in milliseconds |
| `WEBAUTHN_ALLOWED_ORIGINS` | `string` | — | Optional | Comma-separated list of allowed WebAuthn origins |
| `TWO_FACTOR_ISSUER` | `string` | `Ethos-Protocol` | Optional | TOTP issuer name shown in authenticator apps |

**Impact**: Incorrect `PASSKEY_RP_ID` or `PASSKEY_RP_ORIGIN` will cause all Passkey authentication to fail.

**Validation**: `PASSKEY_RP_ID` must be a bare hostname (no scheme or path). `PASSKEY_RP_ORIGIN` must be a valid origin URL. `PASSKEY_TIMEOUT_MS` must be a positive integer.

**Recommended** (local development):

```env
PASSKEY_RP_ID=localhost
PASSKEY_RP_ORIGIN=http://localhost:3000
```

**Recommended** (production):

```env
PASSKEY_RP_ID=yourdomain.com
PASSKEY_RP_ORIGIN=https://yourdomain.com
```

---

### Monitoring

| Variable | Type | Default | Status | Description |
|---|---|---|---|---|
| `METRICS_ENABLED` | `bool` | `true` | Optional | Enable Prometheus metrics endpoint |
| `METRICS_PORT` | `integer` | `9090` | Optional | Port for metrics scraping |
| `METRICS_PATH` | `string` | `/metrics` | Optional | Path for metrics endpoint |
| `SENTRY_DSN` | `string` | — | Optional | Sentry error reporting DSN (optional) |
| `TRACING_ENABLED` | `bool` | `false` | Optional | Enable distributed tracing (legacy alias for `TRACE_ENABLED`) |

**Validation**: `METRICS_PORT` must be `1`–`65535`. `METRICS_PATH` must start with `/`. `SENTRY_DSN` must be a valid DSN URL.

**Recommended** (production):

```env
METRICS_ENABLED=true
METRICS_PORT=9090
SENTRY_DSN=https://your-dsn@sentry.io/project
```

---

## Contract Parameters

These parameters are set via smart contract function calls, not environment variables.

### Vault Parameters

| Parameter | Type | Default | Validation | Description |
|---|---|---|---|---|
| `check_in_interval` | `u64` | — | > 0, ≤ `MAX_CHECK_IN_INTERVAL` | Seconds between required check-ins |
| `initial_balance` | `i128` | `0` | ≥ 0 | Initial XLM deposit on vault creation |
| `beneficiary` | `Address` | — | Valid Stellar address | Address to receive funds on release |

**Recommended check-in intervals**:

| Use Case | Interval |
|---|---|
| Dead man's switch (urgent) | 7 days (`604800`) |
| Personal emergency fund | 90 days (`7776000`) |
| Long-term inheritance | 180–365 days |

---

### TTL & Check-in Parameters

| Parameter | Function | Default | Description |
|---|---|---|---|
| `min_checkin_cooldown` | `set_min_checkin_cooldown()` | `60` | Minimum seconds between check-ins (anti-spam) |
| `max_accelerate_seconds` | Built-in constant | `2592000` (30 days) | Max TTL reduction per `accelerate_ttl_decay()` call |
| `borrow_seconds` | `borrow_ttl()` | — | Seconds of TTL to borrow from another vault |

```rust
// Set minimum check-in cooldown (admin only)
contract.set_min_checkin_cooldown(300); // 5 minutes minimum between check-ins
```

---

### Beneficiary Parameters

| Parameter | Function | Default | Description |
|---|---|---|---|
| `minimum_threshold` | `set_beneficiary_minimum_threshold()` | `0` | Minimum vault balance for beneficiary to accept role |
| `acceptance_window` | — | — | Time window for beneficiary to accept role |
| `conflict_resolution_period` | — | — | Duration of the conflict resolution window |

```rust
// Require at least 100 XLM before beneficiary can accept
contract.set_beneficiary_minimum_threshold(vault_id, 100_0000000_i128);
```

---

### Vesting Parameters

| Parameter | Function | Validation | Description |
|---|---|---|---|
| `start_timestamp` | `set_vesting_schedule()` | ≥ current timestamp | When vesting begins |
| `duration_seconds` | `set_vesting_schedule()` | > 0 | Total vesting duration |
| `cliff_seconds` | `set_vesting_schedule()` | < `duration_seconds` | Time before any tokens vest |
| `total_amount` | `set_vesting_schedule()` | ≤ vault balance | Total amount to vest |

For full details, see [Vesting Schedules](vesting-schedules.md).

---

## Network Configurations (environments.toml)

The `environments.toml` file in the project root defines per-network RPC and passphrase defaults:

```toml
[testnet]
rpc_url = "https://soroban-testnet.stellar.org"
network_passphrase = "Test SDF Network ; September 2015"

[mainnet]
rpc_url = "https://mainnet.sorobanrpc.com"
network_passphrase = "Public Global Stellar Network ; September 2015"

[futurenet]
rpc_url = "https://rpc-futurenet.stellar.org"
network_passphrase = "Test SDF Future Network ; October 2022"

[standalone]
rpc_url = "http://localhost:8000/soroban/rpc"
network_passphrase = "Standalone Network ; February 2017"
```

**Precedence**: Values from `environments.toml` are overridden by environment variables (`STELLAR_RPC_URL`, `NETWORK_PASSPHRASE`).

---

## Docker Configuration

### docker-compose.yml

The base compose file defines production-like services:

| Service | Default Port | Description |
|---|---|---|
| `postgres` | `5432` | PostgreSQL database |
| `backend` | `3000` | Ethos backend API |
| `stellar-quickstart` | `8000` | Local Stellar node |

### docker-compose.override.yml

Override file applies development-specific settings automatically when running `docker-compose up`:

- Uses `standalone` Stellar network
- Mounts source code as a volume for live-reload
- Relaxes authentication requirements
- Enables debug logging

**To run production mode without overrides**:

```bash
docker-compose -f docker-compose.yml up -d
```

---

## Configuration Validation

The backend validates required configuration at startup. Missing or invalid values produce a clear error:

```
ERROR ethos_backend: Missing required config: CONTRACT_TTL_VAULT
ERROR ethos_backend: Invalid DATABASE_URL: connection refused
```

### Keeping `.env.example` in sync

`scripts/check_env_vars.py` scans the backend source for every environment variable the code reads and compares that list against `.env.example`. It runs in CI and fails the build when a variable is read but undocumented, or documented but unused. This document mirrors `.env.example`; if they diverge, treat `scripts/check_env_vars.py` as authoritative and update both files.

```bash
# Report variables read by the code but missing from .env.example
python scripts/check_env_vars.py
```

### Required Variables Checklist

Before deploying, verify these are set:

- [ ] `STELLAR_NETWORK`
- [ ] `STELLAR_RPC_URL`
- [ ] `CONTRACT_TTL_VAULT`
- [ ] `DATABASE_URL`
- [ ] `JWT_SECRET` (min 32 characters)
- [ ] `PASSKEY_RP_ID`
- [ ] `PASSKEY_RP_ORIGIN`

### Conditional (required when the related feature is enabled)

- [ ] `REDIS_URL` — when `CONSENSUS_STRATEGY` is not `single`
- [ ] `CONSENSUS_QUORUM_SIZE` — when `CONSENSUS_STRATEGY=quorum`
- [ ] `TRACE_ENDPOINT` — when `TRACE_ENABLED=true`
- [ ] `AML_PROVIDER_URL`, `AML_API_KEY` — when `AML_ENABLED=true`
- [ ] `ENCRYPTION_KEY` — when field encryption is enabled
- [ ] `STELLAR_MAINNET_RPC_URL` — when deploying to mainnet

### Optional but Recommended

- [ ] `NODE_ID` (unique per replica)
- [ ] `READ_REPLICA_URLS`
- [ ] `RPC_FALLBACK_ENDPOINTS`
- [ ] `TRUSTED_PROXY_IPS` (behind a reverse proxy)
- [ ] `REMINDER_EMAIL_API_KEY`
- [ ] `REMINDER_SMS_API_KEY`
- [ ] `SENTRY_DSN`
- [ ] `METRICS_ENABLED=true`

---

## Configuration Examples

### Testnet Development

```env
STELLAR_NETWORK=testnet
STELLAR_RPC_URL=https://soroban-testnet.stellar.org
CONTRACT_TTL_VAULT=<testnet-contract-id>
DATABASE_URL=postgres://ethos:ethos@localhost:5432/ethos_dev
JWT_SECRET=dev-secret-not-for-production-use-only
PASSKEY_RP_ID=localhost
PASSKEY_RP_ORIGIN=http://localhost:3000
NODE_ID=ethos-dev-1
RUST_LOG=debug
METRICS_ENABLED=false
```

### Production Mainnet

```env
STELLAR_NETWORK=mainnet
STELLAR_RPC_URL=https://mainnet.sorobanrpc.com
CONTRACT_TTL_VAULT=<mainnet-contract-id>
DATABASE_URL=postgres://ethos:strongpassword@db.internal:5432/ethos_prod
READ_REPLICA_URLS=postgres://ethos:strongpassword@replica1:5432/ethos_prod
JWT_SECRET=<32-byte-random-hex>
PASSKEY_RP_ID=yourdomain.com
PASSKEY_RP_ORIGIN=https://yourdomain.com
NODE_ID=ethos-prod-1
REDIS_URL=redis://redis.internal:6379
CONSENSUS_STRATEGY=quorum
CONSENSUS_QUORUM_SIZE=2
AML_ENABLED=true
AML_FAIL_OPEN=false
AML_PROVIDER_URL=https://aml.example.com/screen
TRUST_PROXY_HEADERS=true
TRUSTED_PROXY_IPS=10.0.0.0/8
ENCRYPTION_KEY=<32-byte-random-hex>
REMINDER_EMAIL_API_KEY=<key>
REMINDER_SMS_API_KEY=<key>
RUST_LOG=warn
METRICS_ENABLED=true
SENTRY_DSN=https://your-dsn@sentry.io/project
```

### Docker Compose Local

```env
STELLAR_NETWORK=standalone
STELLAR_RPC_URL=http://stellar-quickstart:8000/soroban/rpc
DATABASE_URL=postgres://ethos:ethos@postgres:5432/ethos
PASSKEY_RP_ID=localhost
PASSKEY_RP_ORIGIN=http://localhost:3000
NODE_ID=ethos-local-1
RUST_LOG=debug