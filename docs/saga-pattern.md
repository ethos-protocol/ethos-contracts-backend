# Transaction Compensation (Saga Pattern)

Implements #125. Code lives in `backend/src/saga.rs`.

## Why

Some backend workflows touch more than one system that can't be committed
atomically (e.g. reserving a vault slot, then registering a webhook, then
charging a fee). If a later step fails, earlier steps need to be undone
explicitly — there's no distributed transaction to roll back. The saga
pattern gives us a structured way to define that undo path per step instead
of ad hoc cleanup code scattered through handlers.

## Model

- **`CompensationRegistry`** maps a step name to its compensation
  (undo) action, kept independent of the forward actions so compensations
  can be registered, inspected, or swapped without touching step logic.
- **`Saga`** (built via `Saga::builder(name)`) holds an ordered list of
  named steps. Each step has a forward action and a `max_retries` count.
- **`Saga::execute()`** runs steps in order:
  - **Forward recovery** — if a step's action fails, it is retried up to
    `max_retries` additional times before being considered failed.
  - **Backward recovery** — once a step fails outright, every
    previously-completed step is compensated in reverse order, using the
    output each step produced (so, e.g., a compensation can look up the
    reservation ID it needs to release).
- Compensation failures are recorded (`SagaStepStatus::CompensationFailed`)
  but do not stop the rest of the rollback — every completed step still
  gets a compensation attempt.
- A step with no registered compensation is simply left as `Completed`
  during rollback (nothing to undo).

## Example

```rust
use ethos_protocol_backend::saga::Saga;

let saga = Saga::builder("release-vault")
    .step("mark-released", 0, || { /* ... */ Ok(serde_json::json!({"vault_id": "v1"})) })
    .step("notify-beneficiaries", 2, || { /* may fail transiently, retried twice */ Ok(serde_json::json!({}))  })
    .compensate("mark-released", |output| { /* revert the release using output */ Ok(()) })
    .build();

let execution = saga.execute();
// execution.status: Completed | Compensated | CompensationFailed
// execution.steps: per-step status, attempt count, output, and error
```

`SagaExecution` is `Serialize`, so it can be persisted as an audit trail or
returned from a status endpoint.

## Testing

`backend/src/saga.rs` includes scenario tests: full success, failure
triggering compensation of prior steps, forward-recovery retry succeeding
on a later attempt, a compensation itself failing (and the rollback
continuing regardless), and a step with no registered compensation.

---

# Distributed Transaction Coordinator Recovery

Implements #354. Code lives in `backend/src/distributed_tx.rs`.

## Why

The saga pattern above covers workflows that call several *external*
systems. `distributed_tx.rs` handles the other case: a single logical write
fanned out across multiple SQLite **shards**, committed with two-phase
commit (2PC). The failure mode 2PC is famous for is a **coordinator crash
between the prepare and commit phases** — participants sit in the
`prepared` state holding locks, and nothing knows whether the transaction
should ultimately commit or roll back. This section defines the recovery
protocol that resolves those in-flight transactions on coordinator restart.

## Durable decision log

`CoordinatorLog` is a small SQLite database owned by the coordinator,
**separate from the shards** so it survives a coordinator process crash
independently. It holds one row per transaction:

| Column | Meaning |
|---|---|
| `tx_id` | Globally unique transaction id |
| `state` | `preparing` \| `prepared` \| `committing` \| `committed` \| `aborting` \| `aborted` |
| `participants` | Comma-separated shard indices in the transaction |
| `updated_at` | RFC 3339 timestamp of the last transition |

Configure its location with `DB_COORDINATOR_LOG_PATH` (a file path makes it
crash-recoverable; the `:memory:` default does not and is for tests only).

## Write ordering (the commit point)

`DistributedTxCoordinator::execute` persists each state transition **before**
issuing the participant RPCs that transition authorises:

```text
 1. record(Preparing)                 ← before contacting any shard
 2. for each shard: prepare()
 3. record(Prepared)
 4. record(Committing)                ← THE COMMIT POINT (durable decision)
 5. for each shard: commit()
 6. record(Committed)                 ← terminal

 on any ABORT vote in step 2:
    record(Aborting) → rollback() every prepared shard → record(Aborted)
```

The durable write of `committing` in step 4 is the atomic commit decision.
Everything is presumed-abort until that row hits disk, and presumed-commit
after.

## Recovery routine

Call `DistributedTxCoordinator::recover()` once on startup, before serving
traffic. It scans the log for every row whose state is **not** terminal
(`committed` / `aborted`) and resolves each one:

| Durable state on restart | Decision was made? | Recovery action | Outcome |
|---|---|---|---|
| `committing`, `committed` | yes | re-drive `commit()` on **every** participant | `ResumedCommit` |
| `preparing`, `prepared`, `aborting` | no | `rollback()` **every** participant (presumed abort) | `Compensated` |

Then it writes the terminal state (`committed` / `aborted`) back to the log.

### Idempotency

Recovery is safe to run repeatedly and safe to run after a partial commit:

- A shard that already committed pre-crash has cleared its
  `prepared_transactions` row, so a repeat `commit()` returns
  `TransactionNotFound`, which recovery treats as "already done".
- A shard `commit()` re-applies its prepared operations inside
  `INSERT OR REPLACE` / delete statements and records the commit via
  `INSERT OR REPLACE INTO committed_transactions`, so replaying it is a
  no-op.
- `rollback()` just discards the `prepared_transactions` row and is
  naturally idempotent.

## Operational runbook

1. Coordinator process dies mid-transaction.
2. On restart, construct the coordinator with the **same**
   `DB_COORDINATOR_LOG_PATH` and shard set.
3. Call `recover()`. Inspect the returned `Vec<RecoveryOutcome>` — each
   entry names a `tx_id` and whether it was `ResumedCommit` or
   `Compensated`.
4. Resume serving traffic.

If the decision log itself is lost, every in-flight transaction is
unresolvable from the coordinator alone; fall back to reconciling the
shards' `prepared_transactions` tables manually (any `tx_id` present in
`committed_transactions` on *any* shard must be committed everywhere;
otherwise roll back).

## Testing

`backend/src/distributed_tx.rs` includes crash-simulation tests:
`execute` records a terminal state; recovery **resumes the commit** when
`committing` was durable and one shard had already committed before the
crash; recovery **aborts** when only `prepared` was durable; and recovery
is idempotent across repeated calls.
