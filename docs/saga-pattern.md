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

## Idempotency Guarantee

`CompensationRegistry` tracks, per step name, whether that step's
compensation has already **succeeded** once. If `execute()` is retried on
the same `Saga` instance — e.g. the process crashed mid-rollback and the
caller re-runs the saga, or an external retry layer re-triggers the same
logical operation — any step whose compensation already completed
successfully is *not* invoked again. Instead its `SagaStepRecord` is
reported as `Compensated` with `compensation_already_ran: true`, so a
double-refund or double-release can't happen just because the saga run was
retried.

A compensation that *failed* is deliberately **not** marked done, so a
retry can still attempt it again until it succeeds — idempotency guards
against re-running work that already took effect, not against retrying
work that didn't.

This guard lives on the registry (and therefore the `Saga` instance) rather
than in each individual compensation closure, so a compensation function
itself doesn't need to implement its own idempotency check to get this
guarantee — though a compensation that calls an external system (e.g. a
payment API) should still prefer an idempotent operation there (such as a
stable idempotency key) as defense in depth, since the registry's guard
only protects against re-invocation *within the lifetime of one `Saga`
instance*, not across separate instances built for a retried operation.

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
