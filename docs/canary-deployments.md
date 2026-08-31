# Canary Deployment Strategy

This document describes the canary deployment implementation in
`backend/src/canary.rs`, which replaces all-or-nothing deployments with
staged, traffic-split rollouts that progress based on observed metrics and
roll back automatically on regressions.

## Why

Deploying 100% of traffic to a new version immediately means any regression
affects every user at once. Canary deployments limit exposure by ramping
traffic gradually and only progressing when the new version is healthy.

## Stages and traffic splitting

A canary deployment (`CanaryDeployment`) is defined by an ordered list of
`CanaryStage`s, each specifying a `traffic_percent` and a
`min_duration_minutes` to hold at that percentage before it's eligible to
progress. If no stages are supplied, a default ramp of 5% → 25% → 50% →
100% is used (`default_stages`).

## API

### `POST /deployments/canary`

Starts a new canary at the first (smallest) stage:

```json
{
  "service": "vault-api",
  "version": "v1.4.0",
  "stages": [
    { "traffic_percent": 5, "min_duration_minutes": 10 },
    { "traffic_percent": 50, "min_duration_minutes": 15 },
    { "traffic_percent": 100, "min_duration_minutes": 0 }
  ],
  "thresholds": { "max_error_rate": 0.01, "max_latency_p99_ms": 400 }
}
```

### `GET /deployments/canary/:id`

Fetches the deployment, including its current stage, traffic percentage,
status, and history of progression events.

### `POST /deployments/canary/:id/evaluate`

Reports the latest observed metrics for the canary's traffic slice:

```json
{
  "metrics": { "error_rate": 0.004, "latency_p99_ms": 210 },
  "baseline_error_rate": 0.002
}
```

`baseline_error_rate` is the current error rate observed in the *baseline*
(non-canary) cohort at the same point in time — reporting it on every call is
what makes the canary-vs-baseline comparison continuous. It's optional; if
omitted, the deployment keeps using the last baseline error rate it was
told about (or skips the baseline comparison if none has ever been reported).

This is the metric-based progression and automated-rollback entry point:

- If `error_rate` or `latency_p99_ms` breaches `thresholds` (`max_error_rate` /
  `max_latency_p99_ms`), the deployment is immediately marked `rolled_back`.
- **Automatic rollback on error-rate spike relative to baseline**: even if
  `error_rate` doesn't breach the absolute `max_error_rate` threshold, the
  deployment is also rolled back once it exceeds the reported
  `baseline_error_rate` by more than `thresholds.max_error_rate_margin`
  (default `0.01`, i.e. 1 percentage point). This catches regressions that
  are only visible relative to what the rest of the fleet is currently
  experiencing (e.g. during a broader incident where both cohorts' error
  rates are elevated, but the canary's is meaningfully worse).
- Otherwise, once the current stage's `min_duration_minutes` has elapsed,
  the deployment advances to the next stage (or `completed` if it was the
  last stage) — this is **metric-based progression**.

Both the triggering condition and the current baseline are recorded in the
deployment's `history` and returned in `last_baseline_error_rate` /
`last_metrics` on `GET /deployments/canary/:id`.

A monitoring loop (e.g. reusing the polling pattern in `scheduler.rs`)
should call this endpoint periodically with live metrics for both the canary
and baseline cohorts for each active canary.

### Configuring the auto-rollback margin

`thresholds.max_error_rate_margin` is set per-deployment via
`POST /deployments/canary` (`thresholds` field), alongside `max_error_rate`
and `max_latency_p99_ms`. It defaults to `0.01`. Set it tighter for
high-traffic, low-error-rate services where even a small relative regression
is significant, and looser for noisy, low-traffic services where baseline
error rate naturally fluctuates.

### `POST /deployments/canary/:id/rollback`

Forces an immediate rollback regardless of current metrics, for cases where
a human or an external alert (not captured by the configured thresholds)
decides the canary should stop.

## Operational notes

- Deployments are stored in-memory (`Arc<Mutex<HashMap<...>>>`); persisting
  state would let in-flight canaries survive a backend restart.
- Traffic splitting itself (routing `current_traffic_percent()` of real
  requests to the new version) is expected to be enforced by the load
  balancer / ingress layer, which should read `current_traffic_percent()`
  from `GET /deployments/canary/:id`.
