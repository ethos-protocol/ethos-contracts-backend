# Ethos-Protocol Backend API Documentation

Complete reference for the HTTP API served by the `ethos-protocol-backend`
crate (`backend/src/main.rs`), with request/response examples, error codes and
common usage patterns.

| Related document | Covers |
|---|---|
| [openapi.yaml](openapi.yaml) | Machine-readable schema; the server rejects wrong methods on declared paths with `405`. |
| [api-reference.md](api-reference.md) | **Smart-contract** functions (generated, drift-checked in CI). |
| [backend-api.md](backend-api.md) | Background notes on individual backend features. |
| [error-format.md](error-format.md) | Full error envelope specification. |
| [troubleshooting.md](troubleshooting.md) | Diagnosing errors returned by this API. |

Every example in this document is checked by
[`scripts/test_api_docs.py`](../scripts/test_api_docs.py): the endpoint index
must match the routes registered in `backend/src/main.rs`, every `curl`
example must hit a real route with an allowed method, every JSON example must
parse, request bodies must only use fields the Rust request struct declares
(and include all required ones), and every error code listed must exist in
the backend source.

---

## Contents

1. [Basics](#1-basics)
2. [Endpoint index](#2-endpoint-index)
3. [Health & observability](#3-health--observability)
4. [Vault reminders & subscriptions](#4-vault-reminders--subscriptions)
5. [Release simulation](#5-release-simulation)
6. [Webhooks](#6-webhooks)
7. [Feature flags](#7-feature-flags)
8. [Capabilities & graceful degradation](#8-capabilities--graceful-degradation)
9. [Streaming](#9-streaming)
10. [GraphQL](#10-graphql)
11. [Anomaly detection](#11-anomaly-detection)
12. [AML screening](#12-aml-screening)
13. [WebAuthn / passkeys](#13-webauthn--passkeys)
14. [Error codes](#14-error-codes)
15. [Usage patterns](#15-usage-patterns)

---

## 1. Basics

| Item | Value |
|---|---|
| Base URL (local) | `http://localhost:3000` (the server binds `0.0.0.0:3000`) |
| Content type | `application/json` for request and response bodies unless noted |
| Timestamps | RFC 3339 / ISO-8601 UTC, e.g. `2026-09-27T12:00:00Z` |
| Vault IDs | Unsigned 64-bit integers in `/api/vaults/:vault_id/...` paths |

### Request headers

| Header | Purpose |
|---|---|
| `Authorization: Bearer <token>` | Identifies the caller for rate limiting. On admin endpoints (AML flags, anomaly seasonality/investigations) the token must equal `ADMIN_API_KEY` when that env var is set. |
| `X-User-Tier: free \| pro \| enterprise \| admin` | Rate-limit tier. Only honoured when `Authorization` is present; otherwise the caller is `Unauthenticated`. |
| `X-Priority: low \| normal \| high \| critical` | Admission priority under load (default `normal`). Low-priority requests are shed first. |
| `Idempotency-Key: <string>` | Makes `POST /api/vaults/:vault_id/reminder-preferences` safe to retry; repeated keys replay the first response. |
| `X-Forwarded-For` | Used as the rate-limit identity for anonymous callers. |
| `Accept: application/x-ndjson` | Opt into NDJSON streaming on `/stream/*`. |

### Middleware applied to every route

Requests pass through, outermost first: CORS → rate limiting (`429`) →
OpenAPI method validation (`405`) → load shedding / priority admission
(`503` / `429`) → handler.

---

## 2. Endpoint index

<!-- endpoint-index:start -->
| Method | Path | Section |
|---|---|---|
| GET | `/health` | [3](#3-health--observability) |
| GET | `/health/consensus` | [3](#3-health--observability) |
| GET | `/ready` | [3](#3-health--observability) |
| GET | `/metrics` | [3](#3-health--observability) |
| POST | `/api/vaults/:vault_id/reminder-preferences` | [4](#4-vault-reminders--subscriptions) |
| GET | `/api/vaults/:vault_id/reminder-preferences` | [4](#4-vault-reminders--subscriptions) |
| DELETE | `/api/vaults/:vault_id/reminder-preferences` | [4](#4-vault-reminders--subscriptions) |
| POST | `/api/vaults/:vault_id/subscriptions` | [4](#4-vault-reminders--subscriptions) |
| DELETE | `/api/vaults/:vault_id/subscriptions` | [4](#4-vault-reminders--subscriptions) |
| GET | `/api/vaults/:vault_id/reminders` | [4](#4-vault-reminders--subscriptions) |
| GET | `/api/vaults/:vault_id/simulate-release` | [5](#5-release-simulation) |
| POST | `/webhooks` | [6](#6-webhooks) |
| GET | `/webhooks` | [6](#6-webhooks) |
| DELETE | `/webhooks/:id` | [6](#6-webhooks) |
| POST | `/webhooks/verify` | [6](#6-webhooks) |
| POST | `/admin/flags` | [7](#7-feature-flags) |
| GET | `/admin/flags` | [7](#7-feature-flags) |
| GET | `/admin/flags/:key` | [7](#7-feature-flags) |
| POST | `/admin/flags/:key/evaluate` | [7](#7-feature-flags) |
| POST | `/admin/capabilities` | [8](#8-capabilities--graceful-degradation) |
| GET | `/admin/capabilities` | [8](#8-capabilities--graceful-degradation) |
| POST | `/capabilities/negotiate` | [8](#8-capabilities--graceful-degradation) |
| GET | `/capabilities/:name/fallback` | [8](#8-capabilities--graceful-degradation) |
| GET | `/stream/vaults` | [9](#9-streaming) |
| GET | `/stream/events` | [9](#9-streaming) |
| POST | `/graphql` | [10](#10-graphql) |
| GET | `/graphql/playground` | [10](#10-graphql) |
| POST | `/anomaly/observe` | [11](#11-anomaly-detection) |
| GET | `/anomaly/alerts` | [11](#11-anomaly-detection) |
| GET | `/anomaly/baseline/:metric` | [11](#11-anomaly-detection) |
| POST | `/anomaly/seasonality` | [11](#11-anomaly-detection) |
| GET | `/anomaly/seasonality` | [11](#11-anomaly-detection) |
| PUT | `/anomaly/seasonality/threshold` | [11](#11-anomaly-detection) |
| GET | `/anomaly/system` | [11](#11-anomaly-detection) |
| GET | `/anomaly/root-causes` | [11](#11-anomaly-detection) |
| POST | `/anomaly/investigations/:anomaly_id` | [11](#11-anomaly-detection) |
| GET | `/anomaly/investigations/:anomaly_id` | [11](#11-anomaly-detection) |
| GET | `/aml/check/:address` | [12](#12-aml-screening) |
| POST | `/aml/flags` | [12](#12-aml-screening) |
| GET | `/aml/flags` | [12](#12-aml-screening) |
| DELETE | `/aml/flags/:address` | [12](#12-aml-screening) |
| POST | `/webauthn/register/begin` | [13](#13-webauthn--passkeys) |
| POST | `/webauthn/register/complete` | [13](#13-webauthn--passkeys) |
| POST | `/webauthn/authenticate/begin` | [13](#13-webauthn--passkeys) |
| POST | `/webauthn/authenticate/complete` | [13](#13-webauthn--passkeys) |
| GET | `/webauthn/credentials/:user_id` | [13](#13-webauthn--passkeys) |
| DELETE | `/webauthn/credentials/:user_id/:cred_id` | [13](#13-webauthn--passkeys) |
| POST | `/webauthn/credentials/:user_id/:cred_id/backup` | [13](#13-webauthn--passkeys) |
<!-- endpoint-index:end -->

---

## 3. Health & observability

### `GET /health`

Liveness probe. Always `200` while the process is serving.

```bash
curl -X GET http://localhost:3000/health
```

```json
{ "status": "ok", "version": "0.1.0" }
```

### `GET /ready`

Readiness probe. Checks database connectivity.

| Status | Meaning |
|---|---|
| `200` | Ready: `{"status":"ok","version":"0.1.0","database":"connected"}` |
| `503` | Database unreachable; take the instance out of rotation. |

```bash
curl -X GET http://localhost:3000/ready
```

### `GET /health/consensus`

Multi-node cache consistency check (Redis-backed `NodeCache`).

```bash
curl -X GET http://localhost:3000/health/consensus
```

```json
{
  "status": "ok",
  "cache_consistent": true,
  "node_id": "node-a",
  "strategy": "last_write_wins",
  "conflicts_detected": 0,
  "conflicts_resolved": 0,
  "keys_checked": 12
}
```

`status` is `"degraded"` when conflicts were found; `503` when the check
itself failed.

### `GET /metrics`

Prometheus text exposition format (not JSON): base counters plus load
shedding, adaptive batching and predictive scaling metrics.

```bash
curl -X GET http://localhost:3000/metrics
```

---

## 4. Vault reminders & subscriptions

### `POST /api/vaults/:vault_id/reminder-preferences`

Create or replace reminder preferences for a vault. Supports
`Idempotency-Key`.

| Field | Type | Required | Notes |
|---|---|---|---|
| `channels` | array of `"email" \| "sms" \| "push"` | yes | Must not be empty. |
| `hours_before_expiry` | integer (u32) | yes | Must be `> 0`. |
| `frequency` | `"once" \| "hourly" \| "daily" \| "weekly" \| "monthly"` | yes | |

```bash
curl -X POST http://localhost:3000/api/vaults/42/reminder-preferences \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: 7f9c2e1a-reminders-42' \
  -d '{"channels": ["email", "push"], "hours_before_expiry": 48, "frequency": "daily"}'
```

`200 OK`:

```json
{
  "vault_id": 42,
  "channels": ["email", "push"],
  "hours_before_expiry": 48,
  "frequency": "daily"
}
```

Errors: `422 invalid_input` (`"channels must not be empty"`,
`"hours_before_expiry must be > 0"`), `500 internal_error`.

### `GET /api/vaults/:vault_id/reminder-preferences`

```bash
curl -X GET http://localhost:3000/api/vaults/42/reminder-preferences
```

Returns the same body as above, or `404 not_found`.

### `DELETE /api/vaults/:vault_id/reminder-preferences`

Soft-deletes the preferences (they remain visible via
`GET /api/vaults/:vault_id/reminders?include_deleted=true`). Returns `204`.

```bash
curl -X DELETE http://localhost:3000/api/vaults/42/reminder-preferences
```

### `GET /api/vaults/:vault_id/reminders`

Lists reminder records for a vault. Query: `include_deleted=true` to include
soft-deleted records (each then carries a `deleted_at` timestamp).

```bash
curl -X GET 'http://localhost:3000/api/vaults/42/reminders?include_deleted=true'
```

```json
[
  {
    "vault_id": 42,
    "channels": ["email"],
    "hours_before_expiry": 24,
    "frequency": "once",
    "deleted_at": "2026-09-20T08:15:00Z"
  }
]
```

### `POST /api/vaults/:vault_id/subscriptions`

Subscribe an owner to vault notifications.

| Field | Type | Required |
|---|---|---|
| `owner` | string (Stellar address) | yes |
| `channels` | array of `"email" \| "sms" \| "webhook"` (non-empty) | yes |
| `frequency` | `"once" \| "hourly" \| "daily" \| "weekly" \| "monthly"` | yes |

```bash
curl -X POST http://localhost:3000/api/vaults/42/subscriptions \
  -H 'Content-Type: application/json' \
  -d '{"owner": "GBRPYHIL2CI3FNQ4BXLFMNDLFJUNPU2HY3ZMFSHONUCEOASW7QC7OX2H", "channels": ["email", "webhook"], "frequency": "weekly"}'
```

`200 OK`:

```json
{
  "vault_id": 42,
  "owner": "GBRPYHIL2CI3FNQ4BXLFMNDLFJUNPU2HY3ZMFSHONUCEOASW7QC7OX2H",
  "channels": ["email", "webhook"],
  "frequency": "weekly"
}
```

### `DELETE /api/vaults/:vault_id/subscriptions`

```bash
curl -X DELETE http://localhost:3000/api/vaults/42/subscriptions
```

Returns `204`.

---

## 5. Release simulation

### `GET /api/vaults/:vault_id/simulate-release`

Projects when a vault would release under different check-in behaviours.

| Query | Default | Notes |
|---|---|---|
| `scenarios` | all three | Comma-separated: `no_check_ins`, `consistent_check_ins`, `missed_check_in_dates`. |
| `missed_count` | `1` | Consecutive missed check-ins for `missed_check_in_dates`. |

```bash
curl -X GET 'http://localhost:3000/api/vaults/42/simulate-release?scenarios=no_check_ins,missed_check_in_dates&missed_count=2'
```

```json
{
  "vault_id": "42",
  "current_ttl_remaining": 518400,
  "check_in_interval": 604800,
  "last_check_in": "2026-09-26T10:00:00Z",
  "scenarios": [
    {
      "scenario": "no_check_ins",
      "description": "Owner never checks in again",
      "projected_release_at": "2026-10-03T10:00:00Z",
      "seconds_until_release": 518400,
      "confidence": "high",
      "notes": ""
    }
  ],
  "simulated_at": "2026-09-27T10:00:00Z"
}
```

Errors: `422 invalid_input` when no valid scenario is requested,
`404 not_found` for an unknown vault.

---

## 6. Webhooks

### `POST /webhooks`

| Field | Type | Required | Notes |
|---|---|---|---|
| `url` | string | yes | Non-empty; HTTPS in production. |
| `vault_id` | string | no | Omit to receive events for all vaults. |
| `event_types` | array | no | Omit/empty = all. Values: `vault_created`, `vault_checked_in`, `vault_released`, `vault_deposit`, `vault_withdrawal`, `beneficiary_updated`, `vault_paused`, `vault_resumed`. |
| `secret` | string | no | Enables HMAC signing. |
| `algorithm` | `"sha256" \| "sha1" \| "sha512"` | no | Default `sha256`. |

```bash
curl -X POST http://localhost:3000/webhooks \
  -H 'Content-Type: application/json' \
  -d '{"url": "https://hooks.example.com/ethos", "vault_id": "42", "event_types": ["vault_released", "vault_checked_in"], "secret": "whsec_example", "algorithm": "sha256"}'
```

`201 Created`:

```json
{
  "id": "b3f1c2d4-5e6f-4a7b-8c9d-0e1f2a3b4c5d",
  "url": "https://hooks.example.com/ethos",
  "vault_id": "42",
  "event_types": ["vault_released", "vault_checked_in"],
  "secret": "whsec_example",
  "algorithm": "sha256",
  "created_at": "2026-09-27T10:00:00Z",
  "active": true
}
```

Deliveries carry `X-Ethos-Signature: sha256=<hex>` and `X-Ethos-Timestamp`
(Unix seconds). Receivers should reject timestamps more than 300 seconds
(`TIMESTAMP_TOLERANCE_SECS`) away from their clock.

### `GET /webhooks` / `DELETE /webhooks/:id`

```bash
curl -X GET http://localhost:3000/webhooks
curl -X DELETE http://localhost:3000/webhooks/b3f1c2d4-5e6f-4a7b-8c9d-0e1f2a3b4c5d
```

`DELETE` returns `204`, or `404` for an unknown id.

### `POST /webhooks/verify`

Verifies a received delivery signature — useful when debugging receivers.

```bash
curl -X POST http://localhost:3000/webhooks/verify \
  -H 'Content-Type: application/json' \
  -d '{"body": "{\"event\":\"vault_released\"}", "secret": "whsec_example", "signature": "sha256=3b1f0c", "timestamp": "1790503200"}'
```

`200` when valid, `401` when not:

```json
{ "valid": false, "algorithm": "sha256", "reason": "signature mismatch" }
```

---

## 7. Feature flags

### `POST /admin/flags`

| Field | Type | Required | Notes |
|---|---|---|---|
| `key` | string | yes | Non-empty. |
| `description` | string | no | |
| `enabled` | bool | no | Default `false`. |
| `rollout_percentage` | integer 0–100 | no | Default `100`. |
| `updated_by` | string | no | Recorded in history. |

```bash
curl -X POST http://localhost:3000/admin/flags \
  -H 'Content-Type: application/json' \
  -d '{"key": "batch-check-in", "description": "Batch check-in endpoint", "enabled": true, "rollout_percentage": 25, "updated_by": "ops@ethos"}'
```

`200 OK` returns the flag with `version`, `created_at`, `updated_at` and a
`history` array. Errors: `422` `{"error":"key must not be empty"}` or
`{"error":"rollout_percentage must be 0-100"}`.

### `GET /admin/flags` / `GET /admin/flags/:key`

```bash
curl -X GET http://localhost:3000/admin/flags
curl -X GET http://localhost:3000/admin/flags/batch-check-in
```

### `POST /admin/flags/:key/evaluate`

Deterministically evaluates a flag for a subject (same subject → same answer).

```bash
curl -X POST http://localhost:3000/admin/flags/batch-check-in/evaluate \
  -H 'Content-Type: application/json' \
  -d '{"subject_id": "user-1234"}'
```

```json
{
  "key": "batch-check-in",
  "subject_id": "user-1234",
  "enabled": true,
  "reason": "gradual rollout at 25%",
  "flag_version": 3
}
```

`404` for an unknown flag.

---

## 8. Capabilities & graceful degradation

### `POST /admin/capabilities`

```bash
curl -X POST http://localhost:3000/admin/capabilities \
  -H 'Content-Type: application/json' \
  -d '{"name": "push_notifications", "level": "degraded", "reason": "FCM latency", "fallback_available": true}'
```

`level` is `full`, `degraded` or `unavailable`. Returns the stored
`CapabilityStatus` (adds `updated_at`).

### `POST /capabilities/negotiate`

```bash
curl -X POST http://localhost:3000/capabilities/negotiate \
  -H 'Content-Type: application/json' \
  -d '{"requested": ["push_notifications", "simulate_release"]}'
```

```json
{
  "capabilities": [
    { "name": "push_notifications", "level": "degraded", "reason": "FCM latency", "use_fallback": true },
    { "name": "simulate_release", "level": "full", "reason": null, "use_fallback": false }
  ],
  "can_proceed": true
}
```

### `GET /capabilities/:name/fallback`

`200` with a reduced-functionality body, `404` if the capability is at `full`,
`503` if no fallback exists.

```bash
curl -X GET http://localhost:3000/capabilities/push_notifications/fallback
```

---

## 9. Streaming

`GET /stream/vaults` and `GET /stream/events` page through large result sets.

| Query | Notes |
|---|---|
| `cursor` | Opaque token from the previous page. |
| `limit` | Default 50, max 500. |
| `vault_id` | `/stream/events` only. |
| `owner` | Filter vaults by owner. |

With `Accept: application/x-ndjson` each line is one JSON record and the last
line is a cursor envelope:

```bash
curl -X GET 'http://localhost:3000/stream/events?vault_id=42&limit=100' -H 'Accept: application/x-ndjson'
```

```json
{"cursor": "eyJvZmZzZXQiOjEwMH0", "has_more": true}
```

---

## 10. GraphQL

`POST /graphql` accepts standard GraphQL JSON; `GET /graphql/playground`
serves an interactive IDE for local development.

```bash
curl -X POST http://localhost:3000/graphql \
  -H 'Content-Type: application/json' \
  -d '{"query": "{ __schema { queryType { name } } }"}'
```

---

## 11. Anomaly detection

### `POST /anomaly/observe`

```bash
curl -X POST http://localhost:3000/anomaly/observe \
  -H 'Content-Type: application/json' \
  -d '{"service": "backend", "metric": "check_in_latency_ms", "value": 950.0}'
```

```json
{ "alert": null }
```

`alert` is populated (id, service, metric, value, baseline_mean,
baseline_std_dev, z_score, threshold, …) when the value is anomalous.

### Other anomaly endpoints

| Endpoint | Notes |
|---|---|
| `GET /anomaly/alerts` | All alerts raised so far. |
| `GET /anomaly/baseline/:metric` | Learned baseline; `404` if the metric has no data. |
| `POST /anomaly/seasonality` | **Admin.** Body `{service?, metric, seasonality}` where `seasonality` is `hour_of_day`, `day_of_week` or `month_of_year`. |
| `GET /anomaly/seasonality` | Current seasonal pattern. |
| `PUT /anomaly/seasonality/threshold` | Body `{service?, metric, bucket, multiplier}`. |
| `GET /anomaly/system` | Cross-service correlated anomalies. |
| `GET /anomaly/root-causes` | Ranked root-cause candidates. |
| `POST /anomaly/investigations/:anomaly_id` | **Admin.** Append to the audit trail. |
| `GET /anomaly/investigations/:anomaly_id` | Investigation history. |

```bash
curl -X POST http://localhost:3000/anomaly/seasonality \
  -H "Authorization: Bearer $ADMIN_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"metric": "check_in_latency_ms", "seasonality": "hour_of_day"}'
```

```bash
curl -X POST http://localhost:3000/anomaly/investigations/alert-123 \
  -H "Authorization: Bearer $ADMIN_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"investigator": "oncall@ethos", "action": "decision_recorded", "decision": "false_positive", "notes": "deploy spike"}'
```

`action` ∈ `opened`, `assigned`, `note_added`, `decision_recorded`, `closed`,
`reopened`; `decision` ∈ `true_positive`, `false_positive`,
`expected_behavior`, `escalated`.

---

## 12. AML screening

### `GET /aml/check/:address`

Screens a Stellar address against the manual flag list and (when
`CHAINALYSIS_API_KEY` is set) Chainalysis.

```bash
curl -X GET http://localhost:3000/aml/check/GBRPYHIL2CI3FNQ4BXLFMNDLFJUNPU2HY3ZMFSHONUCEOASW7QC7OX2H
```

```json
{
  "address": "GBRPYHIL2CI3FNQ4BXLFMNDLFJUNPU2HY3ZMFSHONUCEOASW7QC7OX2H",
  "compliant": true,
  "source": "manual_list_only",
  "identifications": [],
  "reason": null,
  "checked_at": "2026-09-27T10:00:00Z"
}
```

`source` ∈ `manual_flag`, `chainalysis`, `cache`, `manual_list_only`.
Errors: `400` `{"error":"invalid address"}` or a provider failure (unless
`AML_FAIL_OPEN=true`).

### `POST /aml/flags` (admin)

```bash
curl -X POST http://localhost:3000/aml/flags \
  -H "Authorization: Bearer $ADMIN_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"address": "GBRPYHIL2CI3FNQ4BXLFMNDLFJUNPU2HY3ZMFSHONUCEOASW7QC7OX2H", "reason": "court order 2026-114", "flagged_by": "compliance@ethos"}'
```

`201 Created` with `{address, reason, flagged_by, flagged_at}`.
`GET /aml/flags` lists flags; `DELETE /aml/flags/:address` returns `204` or
`404`. All three return `401 unauthorized` without a valid admin key.

---

## 13. WebAuthn / passkeys

Two-step ceremonies; `session_id` from *begin* must be echoed to *complete*.
All binary values are base64url-encoded. See [passkeys.md](passkeys.md) and
[webauthn-setup.md](webauthn-setup.md) for the client-side flow.

```bash
curl -X POST http://localhost:3000/webauthn/register/begin \
  -H 'Content-Type: application/json' \
  -d '{"user_id": "user-1234", "user_name": "alice@example.com", "label": "YubiKey 5C"}'
```

```bash
curl -X POST http://localhost:3000/webauthn/register/complete \
  -H 'Content-Type: application/json' \
  -d '{"session_id": "sess-abc", "credential_id": "Y3JlZC0x", "client_data_json": "eyJ0eXBlIjoid2ViYXV0aG4uY3JlYXRlIn0", "attestation_object": "o2NmbXRkbm9uZQ", "label": "YubiKey 5C"}'
```

```bash
curl -X POST http://localhost:3000/webauthn/authenticate/begin \
  -H 'Content-Type: application/json' \
  -d '{"user_id": "user-1234"}'
```

```bash
curl -X POST http://localhost:3000/webauthn/authenticate/complete \
  -H 'Content-Type: application/json' \
  -d '{"session_id": "sess-def", "credential_id": "Y3JlZC0x", "client_data_json": "eyJ0eXBlIjoid2ViYXV0aG4uZ2V0In0", "authenticator_data": "SZYN5YgO", "signature": "MEUCIQ"}'
```

`authenticate/complete` → `200`:

```json
{
  "user_id": "user-1234",
  "credential_id": "Y3JlZC0x",
  "sign_count": 7,
  "authenticated_at": "2026-09-27T10:00:00Z"
}
```

Credential management: `GET /webauthn/credentials/:user_id`,
`DELETE /webauthn/credentials/:user_id/:cred_id`,
`POST /webauthn/credentials/:user_id/:cred_id/backup`.

Errors are `400`/`422` with `{"error": "<message>"}`, e.g.
`"unknown or expired session"`, `"registration challenge expired"`,
`"user_id must not be empty"`.

---

## 14. Error codes

### 14.1 Envelopes

Most handlers return the unified `ApiError` envelope
([error-format.md](error-format.md)):

```json
{ "code": "invalid_input", "message": "invalid input: channels must not be empty", "details": null }
```

Middleware and some feature modules return a simpler envelope keyed by
`error`:

```json
{ "error": "rate_limit_exceeded", "message": "rate limit exceeded", "retry_after_secs": 12 }
```

Clients should read `code` first and fall back to `error`.

### 14.2 Code table

<!-- error-codes:start -->
| HTTP | Code | Source | Meaning | Client action |
|---:|---|---|---|---|
| 400 | `two_factor_not_enabled` | `AppError::TwoFactorNotEnabled` | 2FA operation on an account without 2FA. | Enroll 2FA first. |
| 401 | `two_factor_required` | `AppError::TwoFactorRequired` | Operation requires a second factor. | Retry with a TOTP code. |
| 401 | `unauthorized` | `audit::authorize_admin` | Missing/invalid `Authorization: Bearer <ADMIN_API_KEY>` on an admin route. | Send the admin key. |
| 404 | `not_found` | `AppError::NotFound` | Resource does not exist. | Check the id; don't retry. |
| 405 | `method_not_allowed` | OpenAPI middleware | Path is declared in `openapi.yaml` but not for this method. The `Allow` header lists valid methods. | Use an allowed method. |
| 422 | `invalid_input` | `AppError::InvalidInput` | Request failed validation; `message` says which field. | Fix the request; don't retry unchanged. |
| 429 | `too_many_requests` | `AppError::TooManyRequests` | Handler-level throttling. | Back off and retry. |
| 429 | `rate_limit_exceeded` | Rate-limit middleware | Per-tier/endpoint quota exhausted. `Retry-After` and `X-RateLimit-Tier` headers are set. | Wait `Retry-After` seconds. |
| 429 | `priority_limit_exceeded` | Priority admission | Concurrency limit for this `X-Priority` class reached. | Retry with backoff. |
| 500 | `internal_error` | `AppError::Db` / `DatabaseError` | Database or unexpected server error. | Retry with backoff; report if persistent. |
| 503 | `load_shed` | Load shedder | Request dropped under high load (lowest priorities first). | Retry with backoff or raise `X-Priority`. |
<!-- error-codes:end -->

### 14.3 Retry guidance

| Retry? | Statuses |
|---|---|
| Never (fix the request) | `400`, `401`, `404`, `405`, `422` |
| After `Retry-After` | `429` |
| With exponential backoff + jitter | `500`, `503` |

---

## 15. Usage patterns

### 15.1 Safe retries with idempotency keys

Generate one key per logical operation and reuse it on every retry. The
server replays the first successful response for 24 hours.

```bash
KEY=$(uuidgen)
for attempt in 1 2 3; do
  curl -sf -X POST http://localhost:3000/api/vaults/42/reminder-preferences \
    -H 'Content-Type: application/json' \
    -H "Idempotency-Key: $KEY" \
    -d '{"channels": ["email"], "hours_before_expiry": 24, "frequency": "once"}' && break
  sleep $((attempt * 2))
done
```

### 15.2 Honouring rate limits

```python
import time
import requests

def call_with_backoff(method, url, max_attempts=5, **kwargs):
    for attempt in range(max_attempts):
        resp = requests.request(method, url, timeout=10, **kwargs)
        if resp.status_code == 429:
            time.sleep(int(resp.headers.get("Retry-After", 2 ** attempt)))
            continue
        if resp.status_code in (500, 503):
            time.sleep(min(2 ** attempt, 30))
            continue
        return resp
    return resp
```

### 15.3 Paging a stream to completion

```python
import json
import requests

def stream_all_events(base, vault_id):
    cursor = None
    while True:
        params = {"vault_id": vault_id, "limit": 500}
        if cursor:
            params["cursor"] = cursor
        resp = requests.get(f"{base}/stream/events", params=params,
                            headers={"Accept": "application/x-ndjson"}, timeout=30)
        lines = [json.loads(l) for l in resp.text.splitlines() if l.strip()]
        envelope = lines.pop()  # last line is the cursor envelope
        yield from lines
        if not envelope.get("has_more"):
            return
        cursor = envelope["cursor"]
```

### 15.4 Verifying webhook deliveries in your receiver

```python
import hashlib
import hmac

def verify_ethos_signature(raw_body: bytes, header: str, secret: str) -> bool:
    algo, _, received = header.partition("=")
    digest = {"sha256": hashlib.sha256, "sha1": hashlib.sha1, "sha512": hashlib.sha512}[algo]
    expected = hmac.new(secret.encode(), raw_body, digest).hexdigest()
    return hmac.compare_digest(expected, received)
```

Always verify against the **raw** request bytes, before JSON parsing.

### 15.5 Gradual rollouts

1. `POST /admin/flags` with `enabled: true, rollout_percentage: 5`.
2. Clients call `POST /admin/flags/:key/evaluate` with a stable `subject_id`.
3. Watch `/metrics` and `/anomaly/alerts`; raise the percentage in steps.
4. Roll back instantly with `enabled: false` — every flag change is versioned
   in `history`.

### 15.6 Health-checking a deployment

```bash
curl -sf http://localhost:3000/health >/dev/null && \
curl -sf http://localhost:3000/ready  >/dev/null && \
echo "backend healthy"
```

See [deployment-runbook.md](deployment-runbook.md) for the full post-deploy
verification sequence.
