//! CDC consumer lag tracking and alerting (#353).
//!
//! `cdc.rs` streams change-data-capture events to consumers, but nothing
//! observes how far behind a consumer is. A stalled consumer silently drifts
//! from the source of truth. This module:
//!
//! * tracks the last-processed offset and timestamp per CDC consumer
//!   ([`ConsumerLagTracker::record_progress`]) plus the source head
//!   ([`ConsumerLagTracker::record_source_offset`]);
//! * computes offset lag and time lag ([`ConsumerLagTracker::lag`]);
//! * renders those as Prometheus metrics in the same text format
//!   `custom_metrics.rs` uses ([`ConsumerLagTracker::render_prometheus`]);
//! * fires an alert through an [`AlertSink`] when lag crosses a threshold
//!   ([`ConsumerLagTracker::evaluate`]). The production sink forwards to
//!   `oncall.rs` / `incidents.rs`; [`LoggingAlertSink`] is the default.

use std::collections::HashMap;
use std::sync::RwLock;

use chrono::{DateTime, Utc};
use serde::Serialize;

/// Per-consumer processing position.
#[derive(Debug, Clone, Copy)]
struct Progress {
    offset: u64,
    at: DateTime<Utc>,
}

/// Lag of one consumer relative to the CDC source head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LagSnapshot {
    /// Events published by the source but not yet processed by the consumer.
    pub offset_lag: u64,
    /// Seconds between the consumer's last-processed event and `now`.
    pub time_lag_seconds: i64,
}

/// An alert raised when a consumer's lag exceeds the configured threshold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LagAlert {
    pub consumer: String,
    pub offset_lag: u64,
    pub time_lag_seconds: i64,
    pub threshold: u64,
}

/// Destination for lag alerts. Implemented by the on-call / incident bridge in
/// production; [`LoggingAlertSink`] otherwise.
pub trait AlertSink {
    fn raise(&self, alert: &LagAlert);
}

/// Default [`AlertSink`] that emits a structured `tracing` warning. Swap for a
/// sink that calls `oncall.rs` / `incidents.rs` when wiring the real
/// escalation path.
pub struct LoggingAlertSink;

impl AlertSink for LoggingAlertSink {
    fn raise(&self, alert: &LagAlert) {
        tracing::warn!(
            consumer = %alert.consumer,
            offset_lag = alert.offset_lag,
            time_lag_seconds = alert.time_lag_seconds,
            threshold = alert.threshold,
            "CDC consumer lag exceeded threshold"
        );
    }
}

/// Tracks CDC progress for every consumer plus the source head.
#[derive(Debug, Default)]
pub struct ConsumerLagTracker {
    source_offset: RwLock<u64>,
    consumers: RwLock<HashMap<String, Progress>>,
}

impl ConsumerLagTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that the CDC source has produced up to `offset` (monotonic; a
    /// lower value is ignored so out-of-order updates cannot rewind the head).
    pub fn record_source_offset(&self, offset: u64) {
        let mut head = self.source_offset.write().unwrap();
        if offset > *head {
            *head = offset;
        }
    }

    /// Record that `consumer` has processed through `offset` at `at`.
    pub fn record_progress(&self, consumer: &str, offset: u64, at: DateTime<Utc>) {
        self.consumers
            .write()
            .unwrap()
            .insert(consumer.to_string(), Progress { offset, at });
    }

    /// Convenience wrapper stamping progress at the current time.
    pub fn record_progress_now(&self, consumer: &str, offset: u64) {
        self.record_progress(consumer, offset, Utc::now());
    }

    /// Current lag for `consumer`, or `None` if it has never reported progress.
    pub fn lag(&self, consumer: &str) -> Option<LagSnapshot> {
        self.lag_at(consumer, Utc::now())
    }

    /// [`lag`](Self::lag) evaluated against an explicit `now` (for tests).
    pub fn lag_at(&self, consumer: &str, now: DateTime<Utc>) -> Option<LagSnapshot> {
        let progress = *self.consumers.read().unwrap().get(consumer)?;
        let head = *self.source_offset.read().unwrap();
        Some(LagSnapshot {
            offset_lag: head.saturating_sub(progress.offset),
            time_lag_seconds: (now - progress.at).num_seconds().max(0),
        })
    }

    /// Lag for every known consumer.
    pub fn all_lags(&self) -> HashMap<String, LagSnapshot> {
        let now = Utc::now();
        self.consumers
            .read()
            .unwrap()
            .keys()
            .filter_map(|c| self.lag_at(c, now).map(|l| (c.clone(), l)))
            .collect()
    }

    /// Return a [`LagAlert`] for every consumer whose `offset_lag` exceeds
    /// `threshold`.
    pub fn breaches(&self, threshold: u64) -> Vec<LagAlert> {
        let now = Utc::now();
        let mut out: Vec<LagAlert> = self
            .consumers
            .read()
            .unwrap()
            .keys()
            .filter_map(|consumer| {
                let lag = self.lag_at(consumer, now)?;
                (lag.offset_lag > threshold).then(|| LagAlert {
                    consumer: consumer.clone(),
                    offset_lag: lag.offset_lag,
                    time_lag_seconds: lag.time_lag_seconds,
                    threshold,
                })
            })
            .collect();
        out.sort_by(|a, b| a.consumer.cmp(&b.consumer));
        out
    }

    /// Evaluate all consumers against `threshold` and forward each breach to
    /// `sink`. Returns the alerts raised.
    pub fn evaluate(&self, threshold: u64, sink: &dyn AlertSink) -> Vec<LagAlert> {
        let alerts = self.breaches(threshold);
        for alert in &alerts {
            sink.raise(alert);
        }
        alerts
    }

    /// Prometheus text-format exposition, matching the style of
    /// [`crate::custom_metrics`].
    pub fn render_prometheus(&self) -> String {
        let mut out = String::new();
        out.push_str("# HELP cdc_consumer_offset_lag Events produced but not yet processed.\n");
        out.push_str("# TYPE cdc_consumer_offset_lag gauge\n");
        let lags = self.all_lags();
        let mut names: Vec<&String> = lags.keys().collect();
        names.sort();
        for name in &names {
            let lag = lags[*name];
            out.push_str(&format!(
                "cdc_consumer_offset_lag{{consumer=\"{name}\"}} {}\n",
                lag.offset_lag
            ));
        }
        out.push_str("# HELP cdc_consumer_time_lag_seconds Seconds since the last processed event.\n");
        out.push_str("# TYPE cdc_consumer_time_lag_seconds gauge\n");
        for name in &names {
            let lag = lags[*name];
            out.push_str(&format!(
                "cdc_consumer_time_lag_seconds{{consumer=\"{name}\"}} {}\n",
                lag.time_lag_seconds
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use std::sync::Mutex;

    struct CapturingSink(Mutex<Vec<LagAlert>>);
    impl AlertSink for CapturingSink {
        fn raise(&self, alert: &LagAlert) {
            self.0.lock().unwrap().push(alert.clone());
        }
    }

    #[test]
    fn lag_is_none_until_the_consumer_reports() {
        let t = ConsumerLagTracker::new();
        t.record_source_offset(100);
        assert!(t.lag("c1").is_none());
    }

    #[test]
    fn offset_and_time_lag_are_computed_from_source_head() {
        let t = ConsumerLagTracker::new();
        let now = Utc::now();
        t.record_source_offset(1_000);
        t.record_progress("c1", 940, now - Duration::seconds(30));

        let lag = t.lag_at("c1", now).unwrap();
        assert_eq!(lag.offset_lag, 60);
        assert_eq!(lag.time_lag_seconds, 30);
    }

    #[test]
    fn a_caught_up_consumer_has_zero_lag() {
        let t = ConsumerLagTracker::new();
        let now = Utc::now();
        t.record_source_offset(500);
        t.record_progress("c1", 500, now);

        let lag = t.lag_at("c1", now).unwrap();
        assert_eq!(lag.offset_lag, 0);
        assert_eq!(lag.time_lag_seconds, 0);
    }

    #[test]
    fn source_offset_never_rewinds() {
        let t = ConsumerLagTracker::new();
        t.record_source_offset(200);
        t.record_source_offset(150); // stale/out-of-order — ignored
        t.record_progress_now("c1", 100);
        assert_eq!(t.lag("c1").unwrap().offset_lag, 100);
    }

    #[test]
    fn threshold_breach_only_fires_above_the_limit() {
        let t = ConsumerLagTracker::new();
        let now = Utc::now();
        t.record_source_offset(1_000);
        t.record_progress("healthy", 990, now); // lag 10
        t.record_progress("stalled", 400, now - Duration::seconds(120)); // lag 600

        let breaches = t.breaches(100);
        assert_eq!(breaches.len(), 1);
        assert_eq!(breaches[0].consumer, "stalled");
        assert_eq!(breaches[0].offset_lag, 600);
        assert_eq!(breaches[0].threshold, 100);
    }

    #[test]
    fn evaluate_forwards_every_breach_to_the_sink() {
        let t = ConsumerLagTracker::new();
        let now = Utc::now();
        t.record_source_offset(1_000);
        t.record_progress("a", 100, now);
        t.record_progress("b", 200, now);
        t.record_progress("c", 999, now); // under threshold

        let sink = CapturingSink(Mutex::new(Vec::new()));
        let raised = t.evaluate(500, &sink);

        assert_eq!(raised.len(), 2);
        let captured = sink.0.lock().unwrap();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0].consumer, "a");
        assert_eq!(captured[1].consumer, "b");
    }

    #[test]
    fn prometheus_output_lists_every_consumer() {
        let t = ConsumerLagTracker::new();
        let now = Utc::now();
        t.record_source_offset(50);
        t.record_progress("c1", 40, now);

        let text = t.render_prometheus();
        assert!(text.contains("cdc_consumer_offset_lag{consumer=\"c1\"} 10"));
        assert!(text.contains("cdc_consumer_time_lag_seconds{consumer=\"c1\"} 0"));
        assert!(text.contains("# TYPE cdc_consumer_offset_lag gauge"));
    }
}
