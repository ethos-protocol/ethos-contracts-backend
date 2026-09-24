# Health-Based Routing

## Overview

`webhook::deliver_event` sends to every matching registration regardless of
how often that endpoint has recently failed, so requests keep going to
targets that are known to be unhealthy. `health_routing` tracks a rolling
health score per delivery target (identified by URL) and uses it to skip or
de-prioritize unhealthy endpoints, with a slow-start ramp for newly
(re)registered ones.

## How Scoring Works

Each delivery attempt updates the target's `EndpointHealth` via
`record_outcome`:

- **Success-rate EWMA** — an exponential moving average
  (`alpha = 0.3`) of success (1.0) / failure (0.0) outcomes, so recent
  behavior dominates the score without a single blip causing a swing.
- **Consecutive failures / successes** — `consecutive_failures` resets to 0
  on any success and `consecutive_successes` resets to 0 on any failure.
- **Slow start** — a target's first `SLOW_START_REQUESTS` (10) attempts ramp
  linearly from 10% to 100% weight, so a newly registered or just-recovered
  endpoint is exercised cautiously rather than immediately taking full
  traffic.

Effective `weight = slow_start_ramp × health_factor`, where `health_factor`
is the success-rate EWMA if the endpoint is healthy, or `0.0` if it isn't.

## Flapping Prevention (Hysteresis)

Whether an endpoint is "healthy" is a sticky `EndpointHealth.healthy` flag,
not something recomputed from `consecutive_failures` on every read. It only
flips at two separate thresholds, forming a hysteresis band:

- **Mark unhealthy** — a healthy endpoint is marked unhealthy once
  `consecutive_failures` reaches `UNHEALTHY_THRESHOLD` (5).
- **Mark healthy again** — an unhealthy endpoint is only marked healthy once
  it has posted `RECOVERY_THRESHOLD` (3) *consecutive* successes, not just
  one.

Without this, a single success immediately after crossing
`UNHEALTHY_THRESHOLD` would flip the endpoint back to healthy right away
(since `consecutive_failures` resets to 0 on any success), and a single
subsequent failure would flip it unhealthy again — the endpoint would flap in
and out of rotation instead of settling. Requiring `RECOVERY_THRESHOLD`
consecutive healthy checks before re-adding an endpoint to rotation damps
that oscillation. When an endpoint recovers, it re-enters through slow-start
as usual, so it still ramps up gradually rather than taking full traffic the
moment it's marked healthy.

## Delivery Integration

Before `webhook::deliver_event` spawns a delivery task for a registration,
it calls `health_routing::should_route(state, &registration.url)`; if the
weight is `0.0` the delivery is skipped entirely and logged, rather than
sending a request that's very likely to fail again. Every attempt
(`attempt_delivery`) reports its outcome back via `record_outcome`.

## Inspecting Routing State

```
GET /admin/routing/health
```

Returns the per-endpoint `EndpointHealth` snapshot: EWMA success rate,
totals, consecutive failures, slow-start progress, and current weight.

```
GET /admin/routing/metrics
```

Returns an aggregate view:

```json
{
  "total_endpoints": 4,
  "healthy_endpoints": 3,
  "unhealthy_endpoints": 1,
  "endpoints_in_slow_start": 1,
  "average_success_rate": 0.87
}
```

## Testing Routing Decisions

```
POST /admin/routing/test
Content-Type: application/json

{ "endpoint": "https://hooks.example.com/primary" }
```

Returns whether that endpoint would currently receive traffic, its weight,
and a human-readable reason (no history yet / in slow-start / marked
unhealthy / healthy with an EWMA success rate), without performing any real
delivery. This is the primary tool for validating routing behavior — e.g.
after a target starts failing, poll `/admin/routing/test` to confirm it gets
routed around once it crosses `UNHEALTHY_THRESHOLD`, and that it ramps back
up through slow-start once it starts succeeding again.

## Storage

Health records live in an in-memory store (`health_routing::HealthStore`)
scoped to the process and shared with `webhook::WebhookState` via
`Arc<HealthRoutingState>`.
