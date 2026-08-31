# Ethos-Protocol Backend API Reference

Base URL: `http://localhost:3000`

---

## Reminder Preferences

### POST `/api/vaults/{vault_id}/reminder-preferences`

Create or update reminder preferences for a vault.

**Path Parameters**

| Name       | Type   | Description          |
|------------|--------|----------------------|
| `vault_id` | uint64 | On-chain vault ID    |

**Request Body** (`application/json`)

| Field                  | Type            | Required | Description                                              |
|------------------------|-----------------|----------|----------------------------------------------------------|
| `channels`             | array of string | Yes      | One or more of: `"email"`, `"sms"`, `"push"`            |
| `hours_before_expiry`  | integer (> 0)   | Yes      | Hours before TTL expiry to send the first reminder       |
| `frequency`            | string          | Yes      | How often to repeat: `"once"`, `"daily"`, `"hourly"`    |

**Example Request**

```json
{
  "channels": ["email", "sms"],
  "hours_before_expiry": 48,
  "frequency": "daily"
}
```

**Responses**

| Status | Description                              |
|--------|------------------------------------------|
| 200    | Preferences saved; returns saved object  |
| 422    | Validation error (empty channels, zero hours) |
| 500    | Internal server error                    |

**Example Response (200)**

```json
{
  "vault_id": 42,
  "channels": ["email", "sms"],
  "hours_before_expiry": 48,
  "frequency": "daily"
}
```

---

### GET `/api/vaults/{vault_id}/reminder-preferences`

Retrieve current reminder preferences for a vault.

**Path Parameters**

| Name       | Type   | Description       |
|------------|--------|-------------------|
| `vault_id` | uint64 | On-chain vault ID |

**Responses**

| Status | Description                                  |
|--------|----------------------------------------------|
| 200    | Returns the stored preferences               |
| 404    | No preferences found for this vault          |
| 500    | Internal server error                        |

**Example Response (200)**

```json
{
  "vault_id": 42,
  "channels": ["email", "sms"],
  "hours_before_expiry": 48,
  "frequency": "daily"
}
```

---

## Request Queue & Backpressure (Issue #866)

The API server queues all Soroban RPC-bound operations through an internal
bounded channel (`MAX_DEPTH = 256` slots).  When the queue is full, new
requests are rejected with HTTP 429 until the worker drains enough capacity.

### Realistic Soroban RPC Latency Expectations

| Scenario                        | Typical Latency   | Notes                                      |
|---------------------------------|-------------------|--------------------------------------------|
| Testnet — healthy               | 200 – 600 ms      | Expected baseline                          |
| Testnet — moderate congestion   | 600 ms – 1.5 s    | Submit/retry loop or resource-intensive ops|
| Testnet — peak congestion       | 1.5 s – 2 s       | Near the documented 2 s ceiling            |
| Mainnet — healthy               | 200 – 800 ms      | Varies by RPC provider                     |
| Mainnet — congestion            | up to 2 s         | Upper bound for queue/timeout sizing       |

**Consequences for queue depth:**

- At 200 ms average latency the worker processes ~5 items/second.
  With a `MAX_DEPTH` of 256 the queue absorbs ~51 seconds of burst before
  backpressure kicks in.
- At 2 s average latency throughput drops to ~0.5 items/second.
  The same 256-slot queue fills in ~512 seconds of sustained overload.

### Backpressure Response

When the queue is at capacity the server returns:

```http
HTTP/1.1 429 Too Many Requests
Content-Type: application/json

{
  "error": "request queue full (backpressure active)",
  "retry_after_ms": 500
}
```

Clients should honour `retry_after_ms` and apply exponential back-off.

---

The background scheduler polls every 60 seconds. For each vault with stored preferences it:

1. Fetches the vault's TTL remaining (hours) from the Stellar RPC.
2. Compares against `hours_before_expiry`.
3. Fires reminders on the configured channels according to `frequency`:
   - `once` — fires exactly once when TTL enters the window.
   - `daily` — fires every 24 hours while inside the window.
   - `hourly` — fires every hour while inside the window.

Preferences are stored off-chain in the backend SQLite database and are never written to the Soroban contract.


---

## Configurable Countdown Notifications

### `set_countdown_config`

```rust
set_countdown_config(vault_id: u64, caller: Address, thresholds: Vec<u64>)
```

Sets the countdown notification thresholds for a vault. Only the vault owner may call this. Each threshold is a number of seconds before expiry at which `check_countdown` will emit a `cd_notif` event. Pass an empty vec to disable notifications. Calling this also clears any previously fired threshold flags, so all thresholds will fire fresh on the next countdown cycle.

**Errors**

| Error     | Code | Condition                    |
|-----------|------|------------------------------|
| `NotOwner`| 6    | Caller is not the vault owner|

**Event emitted:** `set_cd` — `(thresholds)`

---

### `get_countdown_config`

```rust
get_countdown_config(vault_id: u64) -> CountdownConfig
```

Returns the countdown config for a vault. If not explicitly set, returns the default thresholds: 604800 (7 days), 259200 (3 days), 86400 (1 day).

---

### `check_countdown`

```rust
check_countdown(vault_id: u64) -> u64
```

Checks the vault's remaining TTL against its configured thresholds and emits a `cd_notif` event for each threshold that has been crossed since the last check-in or config reset. Each threshold fires **at most once** per countdown cycle. Fired flags are cleared automatically when the owner calls `check_in` or `set_countdown_config`.

Can be called by anyone — intended for off-chain keepers, cron jobs, or reminder services.

**Returns** the remaining TTL in seconds (0 if expired or vault is not Locked).

**Event emitted:** `cd_notif` — `(threshold_seconds, ttl_remaining)` — one per newly crossed threshold.

---

### `CountdownConfig` type

```rust
pub struct CountdownConfig {
    /// Sorted descending list of thresholds in seconds.
    /// Default: [604800, 259200, 86400] (7d, 3d, 1d)
    pub thresholds: Vec<u64>,
}
```

### Integration example

An off-chain keeper calls `check_countdown` on a schedule (e.g. every hour). When the vault TTL drops below a threshold, the contract emits a `cd_notif` event. The keeper reads the event and dispatches email/SMS/push reminders via the backend reminder service.
