# Anomaly Detection

`backend/src/anomaly_detection.rs` implements an online anomaly detector
for arbitrary numeric metrics (works well fed from the same values pushed
to `/metrics/custom`, see `docs/custom-dashboards.md`).

## How it works

- **Baseline learning**: a running mean and variance is maintained per
  metric using Welford's algorithm (`AnomalyStore::observe`), so no
  historical buffer needs to be stored.
- **Detection algorithm**: each new value is scored against the *previous*
  baseline as a z-score (`(value - mean) / std_dev`). Anything with
  `|z| >= 3.0` is flagged.
- **False positive filtering**:
  - The first `MIN_SAMPLES_FOR_DETECTION` (5) points for a metric only
    train the baseline - they can never generate an alert.
  - A 60 second cooldown per metric prevents a single sustained spike from
    generating dozens of duplicate alerts.
- **Alert generation**: alerts are labeled `warning` (`|z| >= 3`) or
  `critical` (`|z| >= 6`) and kept in memory for retrieval.

## API

```
POST /anomaly/observe
{"metric": "checkout_latency_ms", "value": 128.4}
=> {"alert": null}                     # normal, or still learning baseline
=> {"alert": {"id": ..., "z_score": 4.2, "severity": "warning", ...}}

GET /anomaly/alerts                    # all alerts generated so far
GET /anomaly/baseline/checkout_latency_ms   # current learned mean/count
```

## Tuning

The z-score threshold, minimum sample count, and cooldown window are
constants at the top of `anomaly_detection.rs`
(`DEFAULT_Z_THRESHOLD`, `MIN_SAMPLES_FOR_DETECTION`,
`ALERT_COOLDOWN_SECONDS`). Widen the threshold or cooldown for noisy
metrics to reduce false positives further.

## Seasonal handling (#544)

Metrics with predictable cycles (e.g. more withdrawals in December) can opt
into a seasonality: `hour_of_day` (24 buckets), `day_of_week` (7, Monday = 0)
or `month_of_year` (12, January = 0). Each observation is decomposed into
`level + seasonal + residual`: the level is the global mean, the seasonal
component is the season bucket's mean minus the global mean. Once a bucket
has `MIN_SAMPLES_FOR_DETECTION` points, new points in that season are scored
on their residual against the bucket's own spread; untrained buckets fall
back to the global baseline. Per-bucket threshold multipliers scale the z
threshold for known-volatile seasons.

```
POST /anomaly/seasonality            {"service": "vaults", "metric": "withdrawals", "seasonality": "month_of_year"}
PUT  /anomaly/seasonality/threshold  {"service": "vaults", "metric": "withdrawals", "bucket": 11, "multiplier": 1.5}
GET  /anomaly/seasonality?service=vaults&metric=withdrawals
```

## Cross-service correlation (#545)

Observations may name a `service`. Alerts within `CORRELATION_GAP_SECONDS`
(300s) of each other are grouped; a group spanning two or more services is a
*system anomaly*. The earliest alert is recorded as the suspected root cause,
metrics that alerted in several services are listed as `shared_metrics`, and
root causes are tallied across incidents.

```
GET /anomaly/system?window_seconds=3600   # get_system_anomalies(time_window)
GET /anomaly/root-causes                  # most frequent root causes first
```

## Investigation audit trail (#546)

Every action on an alert or system anomaly is appended to an immutable,
ordered history and logged. The first action must be `opened`; a closed
investigation only accepts `reopened`; `decision_recorded` and `closed`
require a `decision` (`true_positive`, `false_positive`, `expected_behavior`,
`escalated`); `assigned` requires an `assignee`. Both endpoints require the
admin API key.

```
POST /anomaly/investigations/:anomaly_id  {"investigator": "alice", "action": "opened"}
GET  /anomaly/investigations/:anomaly_id  # get_investigation_history(anomaly_id)
```
