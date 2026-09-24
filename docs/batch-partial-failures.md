# Batch Operations & Partial Failure Semantics

## Overview

Batch endpoints previously failed all-or-nothing: an error on one item in a
batch aborted the whole request, and there was no way to tell which items
would have succeeded. `batch::BatchTracker` provides a generic per-item
outcome tracker with aggregate success/failure statistics and retry
guidance, applied to a concrete endpoint:
`POST /api/vaults/batch/reminder-preferences`.

## Standardized response type (`BatchResult<T>`)

Every batch endpoint returns the same shape, `batch::BatchResult<T>` (a
type alias of `BatchResponse<T>`, kept for backwards compatibility):

| Field | Type | Meaning |
|---|---|---|
| `total` | `usize` | Number of input items |
| `succeeded` / `failed` | `usize` | Per-outcome counts (`succeeded + failed == total`) |
| `success_rate` | `f64` | `succeeded / total`, in `[0.0, 1.0]` (`1.0` for an empty batch) |
| `items` | `[BatchItemResult<T>]` | Per-item `Ok`/`Err` entry, input order preserved |
| `retry_guidance` | `string` | Single actionable instruction derived from the failures |

Each `items[]` entry is `{ "key": string, "status": "success" | "failure", … }`:

- `status: "success"` carries `item: T` — the created/updated resource.
- `status: "failure"` carries `error: string` and `retryable: bool`.

`key` echoes the caller's identifier for the item (e.g. `vault_id` as a
string) so failures map back to inputs without re-sending the batch.
`retryable` separates transient failures (DB contention — resubmitting the
same item may succeed) from validation failures (the input is invalid as
sent).

Endpoint loops funnel their natural `Result<T, E>` into this shape with
`BatchTracker::record(key, result, retryable)`; `record_success` /
`record_failure` remain available for finer control.

## Batch Reminder Preferences

```
POST /api/vaults/batch/reminder-preferences
Content-Type: application/json

{
  "items": [
    { "vault_id": 1, "channels": ["email"], "hours_before_expiry": 24, "frequency": "daily" },
    { "vault_id": 2, "channels": [], "hours_before_expiry": 24, "frequency": "once" },
    { "vault_id": 3, "channels": ["sms"], "hours_before_expiry": 0, "frequency": "weekly" }
  ]
}
```

Every item is attempted independently — a validation failure on `vault_id:
2` does not prevent `vault_id: 1` or `3` from being processed.

## Response Shape

```json
{
  "total": 3,
  "succeeded": 1,
  "failed": 2,
  "success_rate": 0.333,
  "items": [
    { "key": "1", "status": "success", "item": { "vault_id": 1, "channels": ["email"], "hours_before_expiry": 24, "frequency": "daily" } },
    { "key": "2", "status": "failure", "error": "channels must not be empty", "retryable": false },
    { "key": "3", "status": "failure", "error": "hours_before_expiry must be > 0", "retryable": false }
  ],
  "retry_guidance": "failed items are not retryable as submitted; fix the reported validation errors before resubmitting those keys"
}
```

- `key` matches the input's `vault_id` (as a string) so failures can be
  mapped back to inputs without re-sending the whole batch.
- `retryable` distinguishes validation errors (not retryable — the input
  itself is invalid) from transient errors like database contention
  (retryable — resubmitting the same item may succeed).
- `retry_guidance` is derived from whether any failure was retryable, so
  callers get a single actionable instruction rather than having to infer
  intent from the per-item errors themselves.

## Reusing `BatchTracker` Elsewhere

`batch::BatchTracker<T>` is generic and not tied to reminder preferences:

```rust
let mut tracker = BatchTracker::new();
for item in items {
    match process(&item) {
        Ok(result) => tracker.record_success(item.key(), result),
        Err(e) => tracker.record_failure(item.key(), e.to_string(), is_retryable(&e)),
    }
}
let response = tracker.finish(); // BatchResponse<T>
```

Any future batch endpoint (bulk vault creation, bulk beneficiary updates,
etc.) can adopt the same partial-success shape by wrapping its per-item loop
this way.

## Tests

`backend/src/batch.rs` covers the three cases every batch endpoint must
handle — **all-success**, **all-failure**, and **mixed** — plus an empty
batch, and asserts the serialized JSON matches the schema documented above
(`status`/`item`/`error`/`retryable` keys, `retry_guidance` string).
