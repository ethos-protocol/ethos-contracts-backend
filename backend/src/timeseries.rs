//! Timeseries storage and query primitives.
//!
//! Samples are stored as raw points. To keep long-term storage bounded we
//! support *downsampling*: samples older than a configurable age are
//! aggregated into coarser buckets that preserve the min/max/avg of the
//! original window. See `docs/timeseries-optimizations.md` for retention tiers.

use std::collections::HashMap;

/// A single raw sample at a point in time.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub timestamp: i64,
    pub value: f64,
}

/// An aggregated bucket produced by downsampling.
///
/// `min`, `max` and `avg` are preserved for the whole bucket window so that
/// queries against downsampled data remain compatible with raw queries.
#[derive(Debug, Clone, PartialEq)]
pub struct DownsampledBucket {
    /// Start of the bucket window (inclusive).
    pub start: i64,
    /// End of the bucket window (exclusive).
    pub end: i64,
    pub min: f64,
    pub max: f64,
    pub avg: f64,
    /// Number of raw samples aggregated into this bucket.
    pub count: u64,
}

/// Configuration for the downsampling job.
#[derive(Debug, Clone)]
pub struct DownsamplingConfig {
    /// Samples older than this many days are eligible for downsampling.
    pub retention_days: i64,
    /// Width of the coarse bucket, in seconds.
    pub bucket_seconds: i64,
}

impl Default for DownsamplingConfig {
    fn default() -> Self {
        // 30 days of raw retention, aggregated into 1-hour buckets.
        Self {
            retention_days: 30,
            bucket_seconds: 3600,
        }
    }
}

const SECONDS_PER_DAY: i64 = 86_400;

/// Aggregate samples older than `config.retention_days` into coarse buckets.
///
/// Samples at or newer than the cutoff are returned untouched as `recent`.
/// Older samples are grouped by `config.bucket_seconds` and each bucket keeps
/// its min/max/avg plus the sample count. Buckets are returned sorted by
/// `start`.
///
/// `now` is the reference timestamp (seconds since epoch) used to compute the
/// cutoff, which keeps the job deterministic and testable.
pub fn downsample(
    samples: &[Sample],
    now: i64,
    config: &DownsamplingConfig,
) -> (Vec<Sample>, Vec<DownsampledBucket>) {
    let cutoff = now - config.retention_days * SECONDS_PER_DAY;
    let bucket_seconds = config.bucket_seconds.max(1);

    let mut recent: Vec<Sample> = Vec::new();
    // Preserve insertion order of buckets by tracking first-seen order.
    let mut order: Vec<i64> = Vec::new();
    let mut buckets: HashMap<i64, DownsampledBucket> = HashMap::new();

    for sample in samples {
        if sample.timestamp >= cutoff {
            recent.push(sample.clone());
            continue;
        }

        let start = sample.timestamp.div_euclid(bucket_seconds) * bucket_seconds;
        let end = start + bucket_seconds;

        match buckets.get_mut(&start) {
            Some(bucket) => {
                bucket.min = bucket.min.min(sample.value);
                bucket.max = bucket.max.max(sample.value);
                // Running mean: avg = avg + (x - avg) / count.
                bucket.count += 1;
                bucket.avg += (sample.value - bucket.avg) / bucket.count as f64;
            }
            None => {
                order.push(start);
                buckets.insert(
                    start,
                    DownsampledBucket {
                        start,
                        end,
                        min: sample.value,
                        max: sample.value,
                        avg: sample.value,
                        count: 1,
                    },
                );
            }
        }
    }

    order.sort_unstable();
    let downsampled = order
        .into_iter()
        .filter_map(|start| buckets.remove(&start))
        .collect();

    (recent, downsampled)
}

/// Query helper that keeps raw and downsampled data compatible.
///
/// Returns the min/max/avg over the requested `[from, to)` window, combining
/// any downsampled buckets that overlap the window with raw samples that fall
/// inside it. Returns `None` when no data is present.
pub fn query_range(
    recent: &[Sample],
    downsampled: &[DownsampledBucket],
    from: i64,
    to: i64,
) -> Option<DownsampledBucket> {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut sum = 0.0;
    let mut count: u64 = 0;

    for bucket in downsampled {
        if bucket.end <= from || bucket.start >= to {
            continue;
        }
        min = min.min(bucket.min);
        max = max.max(bucket.max);
        sum += bucket.avg * bucket.count as f64;
        count += bucket.count;
    }

    for sample in recent {
        if sample.timestamp < from || sample.timestamp >= to {
            continue;
        }
        min = min.min(sample.value);
        max = max.max(sample.value);
        sum += sample.value;
        count += 1;
    }

    if count == 0 {
        return None;
    }

    Some(DownsampledBucket {
        start: from,
        end: to,
        min,
        max,
        avg: sum / count as f64,
        count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(timestamp: i64, value: f64) -> Sample {
        Sample { timestamp, value }
    }

    #[test]
    fn recent_samples_are_left_untouched() {
        let now = 100 * SECONDS_PER_DAY;
        let config = DownsamplingConfig::default();
        let samples = vec![sample(now - 10, 1.0), sample(now - 5, 2.0)];

        let (recent, downsampled) = downsample(&samples, now, &config);

        assert_eq!(recent, samples);
        assert!(downsampled.is_empty());
    }

    #[test]
    fn old_samples_are_aggregated_into_buckets() {
        let now = 100 * SECONDS_PER_DAY;
        let config = DownsamplingConfig {
            retention_days: 30,
            bucket_seconds: 3600,
        };
        // Two samples in the same hour bucket, one in the next.
        let base = now - 60 * SECONDS_PER_DAY;
        let samples = vec![
            sample(base, 10.0),
            sample(base + 60, 20.0),
            sample(base + 3600, 5.0),
        ];

        let (recent, downsampled) = downsample(&samples, now, &config);

        assert!(recent.is_empty());
        assert_eq!(downsampled.len(), 2);

        let first = &downsampled[0];
        assert_eq!(first.count, 2);
        assert_eq!(first.min, 10.0);
        assert_eq!(first.max, 20.0);
        assert_eq!(first.avg, 15.0);

        let second = &downsampled[1];
        assert_eq!(second.count, 1);
        assert_eq!(second.min, 5.0);
        assert_eq!(second.max, 5.0);
        assert_eq!(second.avg, 5.0);
    }

    #[test]
    fn buckets_are_sorted_by_start() {
        let now = 100 * SECONDS_PER_DAY;
        let config = DownsamplingConfig::default();
        let base = now - 60 * SECONDS_PER_DAY;
        let samples = vec![
            sample(base + 7200, 1.0),
            sample(base, 2.0),
            sample(base + 3600, 3.0),
        ];

        let (_, downsampled) = downsample(&samples, now, &config);

        let starts: Vec<i64> = downsampled.iter().map(|b| b.start).collect();
        let mut sorted = starts.clone();
        sorted.sort_unstable();
        assert_eq!(starts, sorted);
    }

    #[test]
    fn query_range_combines_raw_and_downsampled() {
        let now = 100 * SECONDS_PER_DAY;
        let config = DownsamplingConfig::default();
        let base = now - 60 * SECONDS_PER_DAY;
        let samples = vec![
            sample(base, 10.0),
            sample(base + 60, 20.0),
            sample(now - 10, 30.0),
        ];

        let (recent, downsampled) = downsample(&samples, now, &config);
        let result = query_range(&recent, &downsampled, base, now).unwrap();

        assert_eq!(result.count, 3);
        assert_eq!(result.min, 10.0);
        assert_eq!(result.max, 30.0);
        assert_eq!(result.avg, 20.0);
    }

    #[test]
    fn query_range_returns_none_when_empty() {
        let result = query_range(&[], &[], 0, 100);
        assert!(result.is_none());
    }
}
