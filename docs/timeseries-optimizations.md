# Timeseries Optimizations

This document describes the storage and query optimizations used by the
timeseries subsystem (`backend/src/timeseries.rs`).

## Overview

Raw samples are written at full resolution and retained for a short window.
To keep long-term storage bounded, samples older than a configurable age are
aggregated (downsampled) into coarser buckets. Each downsampled bucket
preserves the `min`, `max`, and `avg` of the raw samples it replaces, so
queries over historical windows remain compatible with queries over recent
raw data.

## Retention Tiers

Retention is organized into tiers. Each tier defines the maximum age of data
it covers, the bucket width used for aggregation, and the aggregate values
that are preserved.

| Tier | Age range (older than) | Bucket width | Preserved values |
| ---- | ---------------------- | ------------ | ---------------- |
| Raw  | 0 days (recent)        | 1 sample     | full sample      |
| Tier 1 | `N` days             | 5 minutes    | min / max / avg  |
| Tier 2 | `4 * N` days         | 1 hour       | min / max / avg  |
| Tier 3 | `16 * N` days        | 1 day        | min / max / avg  |

`N` is the configurable downsampling age (in days). It is read from the
timeseries configuration and defaults to a conservative value so that recent
data is never aggregated prematurely.

## Downsampling Job

The downsampling job runs periodically and:

1. Selects raw samples whose timestamp is older than `N` days.
2. Groups those samples into buckets of the configured width.
3. Computes `min`, `max`, and `avg` for each bucket.
4. Writes the aggregated bucket and removes the raw samples it replaced.

Because every bucket carries `min`, `max`, and `avg`, range queries can be
answered from downsampled data without changing their semantics: callers that
request an aggregate over a historical window receive the same aggregate
shape they would receive over raw data.

## Query Compatibility

Queries are expected to be compatible across tiers:

- Aggregations (`min`, `max`, `avg`) over a window are computed from whichever
  tier covers that window.
- Windows that span a tier boundary combine raw and downsampled buckets.
- Point lookups within the raw retention window are unaffected.

## Tests

Tests cover:

- **Downsampling correctness**: raw samples older than `N` days are aggregated
  into the expected buckets, and each bucket's `min`, `max`, and `avg` match
  the values computed directly from the raw samples.
- **Query compatibility**: aggregate queries return consistent results whether
  they read raw samples or downsampled buckets, including windows that span a
  tier boundary.
