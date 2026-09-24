# Continuous Profiling

Manual performance investigation doesn't scale — this module makes profiling
an always-on background activity instead of something engineers do reactively
after users complain.

## Recording samples

`ProfilerState` (`backend/src/profiler.rs`) holds an in-memory ring buffer
(capped at 5,000 samples) of `ProfileSample { operation, stack, duration_ms,
recorded_at }`.

Wrap any operation you want profiled with `profile_operation`:

```rust
let vault = profile_operation(&state.profiler_state, "vault.create", &["handler", "db", "insert"], || async {
    db.create_vault(&input).await
}).await;
```

This is the continuous profiling hook — every call records a sample with no
manual toggling required.

## API

- `GET /admin/profiler/samples` — recent raw samples (JSON)
- `GET /admin/profiler/flamegraph` — folded-stack format
  (`frame1;frame2;frame3 <weight>` per line), directly consumable by
  flame graph renderers such as `inferno-flamegraph` or Brendan Gregg's
  `flamegraph.pl`
- `POST /admin/profiler/baseline` — snapshot current per-operation average
  durations as the new baseline
- `GET /admin/profiler/regressions?threshold_pct=20` — operations whose
  current average duration exceeds the baseline by more than
  `threshold_pct` (default 20%)

## Flame graph generation

```
curl http://localhost:3000/admin/profiler/flamegraph > out.folded
flamegraph.pl out.folded > flamegraph.svg
```

Each line's weight is the cumulative milliseconds spent in that exact call
stack across all recorded samples, so wider frames represent more total time
spent, matching standard flame graph semantics.

## Overhead-based sample throttling

Profiling isn't free — recording a sample (locking + pushing into the ring
buffer) has its own cost. If profiling were left always-on at 100% sampling
in a very hot, very cheap operation, that recording cost could itself become
a measurable fraction of request time.

`ProfilerState` tracks the ratio of recording overhead to profiled operation
time on every call. If that ratio exceeds **5%**, the sample rate is
automatically halved (starting from 100%, i.e. every call), down to a floor
of **1%**, so the profiler backs off under pressure instead of adding to it.

Check current overhead and sample rate at any time:

```
GET /admin/profiler/overhead
```

```json
{ "overhead_pct": 1.8, "sample_rate_per_mille": 1000 }
```

`sample_rate_per_mille` is parts-per-thousand (`1000` = sample every call,
`10` = the floor of 1%). Safe sampling rates depend on operation volume and
duration — for high-throughput, sub-millisecond operations, expect the
auto-throttle to reduce sampling; for slower operations (tens of
milliseconds or more) recording overhead is normally negligible and sampling
stays at 100%.

## Performance regression detection

1. Establish a baseline after a known-good deploy: `POST /admin/profiler/baseline`.
2. Traffic continues to record samples.
3. Periodically (e.g. from CI or a cron job) call
   `GET /admin/profiler/regressions` — any operation whose average duration
   grew by more than the threshold is returned with `baseline_avg_ms`,
   `current_avg_ms`, and `percent_change`, sorted worst-first.
4. Wire this into alerting (e.g. fail a CI job or page on-call) if the
   response is non-empty.
