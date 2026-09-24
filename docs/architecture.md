# Architecture

## Distributed Tracing Sampling

The tracing layer uses an **adaptive sampling rate** rather than a fixed rate.
The effective rate scales inversely with the current request volume:

```
rate = clamp(base_rate * REFERENCE_VOLUME / volume, MIN_RATE, MAX_RATE)
```

- `base_rate` is the configured rate (default `0.1`).
- `REFERENCE_VOLUME` (100 requests/window) is the volume at which the
  configured base rate is used unchanged.
- `MIN_RATE` / `MAX_RATE` bound the rate so it never collapses to zero or
  exceeds full sampling.

### Behavior

- **Traffic spikes:** as volume rises above the reference, the rate drops,
  protecting the tracing backend from being overwhelmed.
- **Low traffic:** as volume falls below the reference, the rate rises so
  interesting events are not under-sampled.
- **Errors:** requests that result in an error are **always sampled**,
  regardless of the configured or adaptive rate.

Request volume is measured over a rolling window (`SamplingConfig::window`,
default 1s) and reset on window rollover. The adaptive strategy is implemented
in `backend/src/tracing_sampling.rs` and covered by unit tests that simulate
load and verify rate adjustment.
