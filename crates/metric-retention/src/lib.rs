//! Metric retention policies (#605).
//!
//! Metrics grow without bound unless old samples are compacted or removed.
//! This crate defines a retention policy per metric type and enforces it over
//! a series of samples:
//!
//! * samples newer than `raw_secs` are kept at full resolution,
//! * older samples are downsampled into hourly buckets until `hourly_secs`,
//! * then into daily buckets until `daily_secs`,
//! * anything older is removed, after being handed to an [`Archive`] when the
//!   policy asks for archiving.
//!
//! Archiving happens before removal, and if the archive write fails
//! [`enforce`] returns an error without producing a compacted series, so the
//! caller keeps the original samples. Retention never drops unarchived data.
//!
//! Downsampling is type-aware. Counters are cumulative, so a bucket keeps its
//! last value. Gauges and histograms are sampled, so a bucket keeps the mean.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fmt::Write as _;
use std::io::{self, Write};

const HOUR_SECS: i64 = 3_600;
const DAY_SECS: i64 = 86_400;

/// The kind of metric a series holds. Each kind has its own retention policy
/// and downsampling rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetricType {
    /// Monotonically increasing total. Downsampled by keeping the last value.
    Counter,
    /// Point-in-time reading. Downsampled by averaging.
    Gauge,
    /// Distribution summary. Downsampled by averaging.
    Histogram,
}

/// One observation. `timestamp` is unix seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub timestamp: i64,
    pub value: f64,
}

/// Age windows for one metric type, in seconds. Invariant:
/// `0 < raw_secs <= hourly_secs <= daily_secs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub raw_secs: i64,
    pub hourly_secs: i64,
    pub daily_secs: i64,
    /// Hand expired samples to an [`Archive`] before removing them.
    pub archive: bool,
}

/// Reasons a [`RetentionPolicy`] cannot be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyError {
    /// A window was zero or negative.
    NonPositiveWindow,
    /// Windows were not in `raw <= hourly <= daily` order.
    WindowsNotOrdered,
}

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonPositiveWindow => write!(f, "retention windows must be positive"),
            Self::WindowsNotOrdered => {
                write!(f, "retention windows must satisfy raw <= hourly <= daily")
            }
        }
    }
}

impl std::error::Error for PolicyError {}

impl RetentionPolicy {
    /// Build a policy from whole-day windows, validating the ordering.
    pub fn from_days(
        raw_days: u32,
        hourly_days: u32,
        daily_days: u32,
        archive: bool,
    ) -> Result<Self, PolicyError> {
        if raw_days == 0 || hourly_days == 0 || daily_days == 0 {
            return Err(PolicyError::NonPositiveWindow);
        }
        if !(raw_days <= hourly_days && hourly_days <= daily_days) {
            return Err(PolicyError::WindowsNotOrdered);
        }
        Ok(Self {
            raw_secs: i64::from(raw_days) * DAY_SECS,
            hourly_secs: i64::from(hourly_days) * DAY_SECS,
            daily_secs: i64::from(daily_days) * DAY_SECS,
            archive,
        })
    }
}

/// Retention policy per metric type. Types without an explicit entry use the
/// built-in defaults.
#[derive(Debug, Clone, Default)]
pub struct RetentionConfig {
    overrides: HashMap<MetricType, RetentionPolicy>,
}

impl RetentionConfig {
    /// Empty configuration: every type uses the built-in defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the policy for one metric type.
    pub fn set(&mut self, metric: MetricType, policy: RetentionPolicy) {
        self.overrides.insert(metric, policy);
    }

    /// Effective policy for a metric type.
    pub fn policy_for(&self, metric: MetricType) -> RetentionPolicy {
        self.overrides
            .get(&metric)
            .copied()
            .unwrap_or_else(|| default_policy(metric))
    }
}

/// Built-in defaults. Counters are archived because they are the audit trail
/// for totals. Gauges and histograms are operational and expire.
pub fn default_policy(metric: MetricType) -> RetentionPolicy {
    match metric {
        MetricType::Counter => RetentionPolicy {
            raw_secs: 7 * DAY_SECS,
            hourly_secs: 30 * DAY_SECS,
            daily_secs: 365 * DAY_SECS,
            archive: true,
        },
        MetricType::Gauge => RetentionPolicy {
            raw_secs: 3 * DAY_SECS,
            hourly_secs: 14 * DAY_SECS,
            daily_secs: 90 * DAY_SECS,
            archive: false,
        },
        MetricType::Histogram => RetentionPolicy {
            raw_secs: 2 * DAY_SECS,
            hourly_secs: 14 * DAY_SECS,
            daily_secs: 180 * DAY_SECS,
            archive: false,
        },
    }
}

/// Destination for samples that have aged out of the daily window.
pub trait Archive {
    /// Persist `samples` for `series`. Returning an error means nothing was
    /// durably written, and the caller must not remove the samples.
    fn write(&mut self, series: &str, samples: &[Sample]) -> Result<(), ArchiveError>;
}

/// An archive write failed.
#[derive(Debug)]
pub struct ArchiveError(pub String);

impl fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "archive write failed: {}", self.0)
    }
}

impl std::error::Error for ArchiveError {}

impl From<io::Error> for ArchiveError {
    fn from(err: io::Error) -> Self {
        Self(err.to_string())
    }
}

/// In-memory archive, mainly for tests and dry runs.
#[derive(Debug, Default)]
pub struct MemoryArchive {
    pub entries: Vec<(String, Sample)>,
}

impl Archive for MemoryArchive {
    fn write(&mut self, series: &str, samples: &[Sample]) -> Result<(), ArchiveError> {
        self.entries
            .extend(samples.iter().map(|s| (series.to_string(), *s)));
        Ok(())
    }
}

/// Archive that appends one JSON object per sample to any writer, such as a
/// file or an object-store upload buffer.
pub struct JsonLinesArchive<W: Write> {
    writer: W,
}

impl<W: Write> JsonLinesArchive<W> {
    pub fn new(writer: W) -> Self {
        Self { writer }
    }

    /// Recover the underlying writer.
    pub fn into_inner(self) -> W {
        self.writer
    }
}

impl<W: Write> Archive for JsonLinesArchive<W> {
    fn write(&mut self, series: &str, samples: &[Sample]) -> Result<(), ArchiveError> {
        for sample in samples {
            // JSON has no NaN or infinity, so non-finite values are written as null.
            let value = if sample.value.is_finite() {
                sample.value.to_string()
            } else {
                "null".to_string()
            };
            writeln!(
                self.writer,
                "{{\"series\":\"{}\",\"timestamp\":{},\"value\":{}}}",
                escape_json(series),
                sample.timestamp,
                value
            )?;
        }
        self.writer.flush()?;
        Ok(())
    }
}

fn escape_json(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Result of applying a policy to a series.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RetentionOutcome {
    /// Compacted series, sorted by timestamp. Raw samples are kept as-is,
    /// and older ones are replaced by bucket representatives.
    pub retained: Vec<Sample>,
    /// Samples that were removed after being archived. Empty when the policy
    /// does not archive.
    pub archived: Vec<Sample>,
    /// Samples removed without archiving.
    pub dropped: usize,
}

/// Pure planning step: split `samples` by age and compact the older tiers.
/// Does not touch any archive, so it is safe to call for a dry run.
pub fn plan(
    samples: &[Sample],
    metric: MetricType,
    policy: &RetentionPolicy,
    now: i64,
) -> RetentionOutcome {
    let raw_cutoff = now - policy.raw_secs;
    let hourly_cutoff = now - policy.hourly_secs;
    let daily_cutoff = now - policy.daily_secs;

    let mut raw = Vec::new();
    let mut hourly: Vec<Sample> = Vec::new();
    let mut daily: Vec<Sample> = Vec::new();
    let mut expired: Vec<Sample> = Vec::new();

    for sample in samples {
        if sample.timestamp >= raw_cutoff {
            raw.push(*sample);
        } else if sample.timestamp >= hourly_cutoff {
            hourly.push(*sample);
        } else if sample.timestamp >= daily_cutoff {
            daily.push(*sample);
        } else {
            expired.push(*sample);
        }
    }

    let mut retained = raw;
    retained.extend(compact(&hourly, metric, HOUR_SECS));
    retained.extend(compact(&daily, metric, DAY_SECS));
    retained.sort_by_key(|s| s.timestamp);

    let (archived, dropped) = if policy.archive {
        expired.sort_by_key(|s| s.timestamp);
        (expired, 0)
    } else {
        (Vec::new(), expired.len())
    };

    RetentionOutcome {
        retained,
        archived,
        dropped,
    }
}

/// Apply the policy for `metric` to `samples` at time `now`.
///
/// Expired samples go to `archive` first. If the write fails, the error is
/// returned and no compacted series is produced, so the caller keeps its
/// original data. The result is idempotent: applying it again with the same
/// `now` leaves the retained series unchanged.
pub fn enforce(
    series: &str,
    samples: &[Sample],
    metric: MetricType,
    policy: &RetentionPolicy,
    now: i64,
    archive: &mut dyn Archive,
) -> Result<RetentionOutcome, ArchiveError> {
    let outcome = plan(samples, metric, policy, now);
    if !outcome.archived.is_empty() {
        archive.write(series, &outcome.archived)?;
    }
    Ok(outcome)
}

/// Bucket samples by `bucket_secs` and reduce each bucket to one sample.
/// The representative timestamp is the bucket start.
fn compact(samples: &[Sample], metric: MetricType, bucket_secs: i64) -> Vec<Sample> {
    let mut buckets: BTreeMap<i64, Vec<Sample>> = BTreeMap::new();
    for sample in samples {
        let index = sample.timestamp.div_euclid(bucket_secs);
        buckets.entry(index).or_default().push(*sample);
    }

    buckets
        .into_iter()
        .map(|(index, mut bucket)| {
            let value = match metric {
                MetricType::Counter => {
                    bucket.sort_by_key(|s| s.timestamp);
                    bucket.last().map_or(0.0, |s| s.value)
                }
                MetricType::Gauge | MetricType::Histogram => {
                    let sum: f64 = bucket.iter().map(|s| s.value).sum();
                    sum / bucket.len() as f64
                }
            };
            Sample {
                timestamp: index * bucket_secs,
                value,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn policy(archive: bool) -> RetentionPolicy {
        RetentionPolicy::from_days(1, 7, 30, archive).unwrap()
    }

    #[test]
    fn from_days_rejects_zero_window() {
        assert_eq!(
            RetentionPolicy::from_days(0, 7, 30, false),
            Err(PolicyError::NonPositiveWindow)
        );
    }

    #[test]
    fn from_days_rejects_unordered_windows() {
        assert_eq!(
            RetentionPolicy::from_days(7, 1, 30, false),
            Err(PolicyError::WindowsNotOrdered)
        );
        assert_eq!(
            RetentionPolicy::from_days(1, 30, 7, false),
            Err(PolicyError::WindowsNotOrdered)
        );
    }

    #[test]
    fn from_days_accepts_equal_windows() {
        assert!(RetentionPolicy::from_days(5, 5, 5, false).is_ok());
    }

    #[test]
    fn config_uses_defaults_and_honours_overrides() {
        let mut config = RetentionConfig::new();
        assert_eq!(
            config.policy_for(MetricType::Counter),
            default_policy(MetricType::Counter)
        );
        assert!(config.policy_for(MetricType::Counter).archive);
        assert!(!config.policy_for(MetricType::Gauge).archive);

        let custom = RetentionPolicy::from_days(1, 2, 3, false).unwrap();
        config.set(MetricType::Gauge, custom);
        assert_eq!(config.policy_for(MetricType::Gauge), custom);
        assert_eq!(
            config.policy_for(MetricType::Histogram),
            default_policy(MetricType::Histogram)
        );
    }

    #[test]
    fn recent_samples_are_kept_raw() {
        let samples = vec![
            Sample {
                timestamp: NOW - 60,
                value: 1.0,
            },
            Sample {
                timestamp: NOW - 120,
                value: 2.0,
            },
        ];
        let out = plan(&samples, MetricType::Gauge, &policy(false), NOW);
        assert_eq!(out.retained.len(), 2);
        assert_eq!(out.retained[0].timestamp, NOW - 120);
        assert_eq!(out.dropped, 0);
        assert!(out.archived.is_empty());
    }

    #[test]
    fn gauge_samples_in_an_hour_are_averaged() {
        // Two days old, so they fall in the hourly tier. Both land in one hour.
        let base = (NOW - 2 * DAY_SECS).div_euclid(HOUR_SECS) * HOUR_SECS;
        let samples = vec![
            Sample {
                timestamp: base + 10,
                value: 10.0,
            },
            Sample {
                timestamp: base + 20,
                value: 20.0,
            },
            Sample {
                timestamp: base + 30,
                value: 30.0,
            },
        ];
        let out = plan(&samples, MetricType::Gauge, &policy(false), NOW);
        assert_eq!(out.retained.len(), 1);
        assert_eq!(out.retained[0].timestamp, base);
        assert!((out.retained[0].value - 20.0).abs() < 1e-9);
    }

    #[test]
    fn counter_samples_in_an_hour_keep_the_last_value() {
        let base = (NOW - 2 * DAY_SECS).div_euclid(HOUR_SECS) * HOUR_SECS;
        let samples = vec![
            Sample {
                timestamp: base + 10,
                value: 100.0,
            },
            Sample {
                timestamp: base + 50,
                value: 250.0,
            },
            Sample {
                timestamp: base + 30,
                value: 180.0,
            },
        ];
        let out = plan(&samples, MetricType::Counter, &policy(true), NOW);
        assert_eq!(out.retained.len(), 1);
        assert!((out.retained[0].value - 250.0).abs() < 1e-9);
    }

    #[test]
    fn samples_older_than_daily_window_are_archived_when_enabled() {
        let old = NOW - 40 * DAY_SECS;
        let samples = vec![Sample {
            timestamp: old,
            value: 5.0,
        }];
        let out = plan(&samples, MetricType::Counter, &policy(true), NOW);
        assert!(out.retained.is_empty());
        assert_eq!(out.archived, samples);
        assert_eq!(out.dropped, 0);
    }

    #[test]
    fn samples_older_than_daily_window_are_dropped_without_archive() {
        let samples = vec![Sample {
            timestamp: NOW - 40 * DAY_SECS,
            value: 5.0,
        }];
        let out = plan(&samples, MetricType::Gauge, &policy(false), NOW);
        assert!(out.retained.is_empty());
        assert!(out.archived.is_empty());
        assert_eq!(out.dropped, 1);
    }

    #[test]
    fn enforce_archives_before_returning() {
        let samples = vec![Sample {
            timestamp: NOW - 40 * DAY_SECS,
            value: 5.0,
        }];
        let mut archive = MemoryArchive::default();
        let out = enforce(
            "requests",
            &samples,
            MetricType::Counter,
            &policy(true),
            NOW,
            &mut archive,
        )
        .unwrap();
        assert!(out.retained.is_empty());
        assert_eq!(archive.entries.len(), 1);
        assert_eq!(archive.entries[0].0, "requests");
    }

    struct FailingArchive;

    impl Archive for FailingArchive {
        fn write(&mut self, _series: &str, _samples: &[Sample]) -> Result<(), ArchiveError> {
            Err(ArchiveError("disk full".into()))
        }
    }

    #[test]
    fn archive_failure_is_reported_and_no_outcome_is_returned() {
        let samples = vec![
            Sample {
                timestamp: NOW - 40 * DAY_SECS,
                value: 5.0,
            },
            Sample {
                timestamp: NOW - 60,
                value: 1.0,
            },
        ];
        let result = enforce(
            "requests",
            &samples,
            MetricType::Counter,
            &policy(true),
            NOW,
            &mut FailingArchive,
        );
        assert!(result.is_err());
        // The caller still holds `samples`. `enforce` never mutates its input,
        // so the expired point is still present there.
        assert_eq!(samples.len(), 2);
    }

    #[test]
    fn enforce_is_idempotent_for_the_same_clock() {
        let base = NOW - 10 * DAY_SECS;
        let samples: Vec<Sample> = (0..200)
            .map(|i| Sample {
                timestamp: base + i * 600,
                value: i as f64,
            })
            .collect();
        let mut archive = MemoryArchive::default();
        let first = enforce(
            "s",
            &samples,
            MetricType::Gauge,
            &policy(false),
            NOW,
            &mut archive,
        )
        .unwrap();
        let second = enforce(
            "s",
            &first.retained,
            MetricType::Gauge,
            &policy(false),
            NOW,
            &mut archive,
        )
        .unwrap();
        assert_eq!(first.retained, second.retained);
    }

    #[test]
    fn retained_output_is_sorted_by_timestamp() {
        let samples = vec![
            Sample {
                timestamp: NOW - 60,
                value: 1.0,
            },
            Sample {
                timestamp: NOW - 3 * DAY_SECS,
                value: 2.0,
            },
            Sample {
                timestamp: NOW - 10 * DAY_SECS,
                value: 3.0,
            },
        ];
        let out = plan(&samples, MetricType::Gauge, &policy(false), NOW);
        let mut sorted = out.retained.clone();
        sorted.sort_by_key(|s| s.timestamp);
        assert_eq!(out.retained, sorted);
    }

    #[test]
    fn json_lines_archive_writes_one_object_per_sample() {
        let mut archive = JsonLinesArchive::new(Vec::new());
        archive
            .write(
                "vault \"alpha\"",
                &[
                    Sample {
                        timestamp: 10,
                        value: 1.5,
                    },
                    Sample {
                        timestamp: 20,
                        value: f64::NAN,
                    },
                ],
            )
            .unwrap();
        let text = String::from_utf8(archive.into_inner()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            "{\"series\":\"vault \\\"alpha\\\"\",\"timestamp\":10,\"value\":1.5}"
        );
        assert!(lines[1].ends_with("\"value\":null}"));
    }

    #[test]
    fn negative_timestamps_bucket_consistently() {
        let samples = vec![
            Sample {
                timestamp: -1,
                value: 1.0,
            },
            Sample {
                timestamp: -HOUR_SECS + 1,
                value: 3.0,
            },
        ];
        let out = compact(&samples, MetricType::Gauge, HOUR_SECS);
        // -1 and -3599 both fall in the bucket that starts at -3600.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].timestamp, -HOUR_SECS);
        assert!((out[0].value - 2.0).abs() < 1e-9);
    }
}
