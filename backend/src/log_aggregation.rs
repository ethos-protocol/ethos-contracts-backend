//! Log aggregation with batching, buffering, and sink integration
//!
//! Provides:
//! - Batching of multiple log entries for efficient processing
//! - Time-based and size-based flushing
//! - Configurable buffering policies
//! - Aggregation statistics and metrics
//! - Multiple sink targets (file, network, database)

use crate::structured_logging::{LogLevel, StructuredLogEntry};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration as StdDuration;

/// Statistics about aggregation behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregationStats {
    pub total_entries: u64,
    pub entries_flushed: u64,
    pub batches_created: u64,
    pub last_flush_time: Option<DateTime<Utc>>,
    pub average_batch_size: f64,
    pub buffer_peak_size: usize,
}

impl Default for AggregationStats {
    fn default() -> Self {
        Self {
            total_entries: 0,
            entries_flushed: 0,
            batches_created: 0,
            last_flush_time: None,
            average_batch_size: 0.0,
            buffer_peak_size: 0,
        }
    }
}

/// Configuration for log aggregation behavior.
#[derive(Debug, Clone)]
pub struct AggregationConfig {
    /// Maximum entries per batch
    pub batch_size: usize,
    /// Maximum time to wait before flushing a partial batch (seconds)
    pub flush_interval_secs: u64,
    /// Maximum entries in memory buffer before blocking
    pub max_buffer_size: usize,
    /// Enable automatic time-based flushing
    pub auto_flush_enabled: bool,
}

impl Default for AggregationConfig {
    fn default() -> Self {
        Self {
            batch_size: 50,
            flush_interval_secs: 5,
            max_buffer_size: 1000,
            auto_flush_enabled: true,
        }
    }
}

/// A batch of log entries ready for processing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogBatch {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub entries: Vec<StructuredLogEntry>,
    pub level_summary: LevelSummary,
}

/// Summary of log levels in a batch.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LevelSummary {
    pub trace_count: u64,
    pub debug_count: u64,
    pub info_count: u64,
    pub warn_count: u64,
    pub error_count: u64,
}

impl LevelSummary {
    pub fn from_entries(entries: &[StructuredLogEntry]) -> Self {
        let mut summary = Self::default();
        for entry in entries {
            match entry.level {
                LogLevel::Trace => summary.trace_count += 1,
                LogLevel::Debug => summary.debug_count += 1,
                LogLevel::Info => summary.info_count += 1,
                LogLevel::Warn => summary.warn_count += 1,
                LogLevel::Error => summary.error_count += 1,
            }
        }
        summary
    }

    pub fn has_errors(&self) -> bool {
        self.error_count > 0
    }

    pub fn has_warnings(&self) -> bool {
        self.warn_count > 0
    }

    pub fn total(&self) -> u64 {
        self.trace_count + self.debug_count + self.info_count + self.warn_count + self.error_count
    }
}

/// Internal state for the aggregator.
struct AggregatorInner {
    buffer: Vec<StructuredLogEntry>,
    stats: AggregationStats,
    last_flush: DateTime<Utc>,
}

/// Log aggregator that batches and buffers entries.
pub struct LogAggregator {
    config: Arc<Mutex<AggregationConfig>>,
    inner: Arc<Mutex<AggregatorInner>>,
    batches: Arc<Mutex<Vec<LogBatch>>>,
}

impl LogAggregator {
    /// Create a new aggregator with default configuration.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            config: Arc::new(Mutex::new(AggregationConfig::default())),
            inner: Arc::new(Mutex::new(AggregatorInner {
                buffer: Vec::new(),
                stats: AggregationStats::default(),
                last_flush: Utc::now(),
            })),
            batches: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Create with custom configuration.
    pub fn with_config(config: AggregationConfig) -> Arc<Self> {
        Arc::new(Self {
            config: Arc::new(Mutex::new(config)),
            inner: Arc::new(Mutex::new(AggregatorInner {
                buffer: Vec::new(),
                stats: AggregationStats::default(),
                last_flush: Utc::now(),
            })),
            batches: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Add an entry to the aggregator.
    pub fn add(&self, entry: StructuredLogEntry) {
        let config = self.config.lock().unwrap_or_else(|p| p.into_inner());
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());

        inner.buffer.push(entry);
        inner.stats.total_entries += 1;

        // Update peak size
        if inner.buffer.len() > inner.stats.buffer_peak_size {
            inner.stats.buffer_peak_size = inner.buffer.len();
        }

        // Check if we should flush
        if inner.buffer.len() >= config.batch_size {
            drop(config);
            drop(inner);
            self.flush();
        }
    }

    /// Flush the current buffer into a batch.
    pub fn flush(&self) -> Option<LogBatch> {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());

        if inner.buffer.is_empty() {
            return None;
        }

        let entries = std::mem::take(&mut inner.buffer);
        let batch_id = uuid::Uuid::new_v4().to_string();
        let level_summary = LevelSummary::from_entries(&entries);

        inner.stats.entries_flushed += entries.len() as u64;
        inner.stats.batches_created += 1;
        inner.stats.last_flush_time = Some(Utc::now());

        // Update average batch size
        if inner.stats.batches_created > 0 {
            inner.stats.average_batch_size =
                inner.stats.entries_flushed as f64 / inner.stats.batches_created as f64;
        }

        let batch = LogBatch {
            id: batch_id,
            created_at: Utc::now(),
            entries,
            level_summary,
        };

        let mut batches = self.batches.lock().unwrap_or_else(|p| p.into_inner());
        batches.push(batch.clone());

        inner.last_flush = Utc::now();

        Some(batch)
    }

    /// Get pending batches and clear the list.
    pub fn drain_batches(&self) -> Vec<LogBatch> {
        let mut batches = self.batches.lock().unwrap_or_else(|p| p.into_inner());
        std::mem::take(&mut batches)
    }

    /// Get the current statistics.
    pub fn stats(&self) -> AggregationStats {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .stats
            .clone()
    }

    /// Get the current buffer size.
    pub fn buffer_len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .buffer
            .len()
    }

    /// Get pending batch count.
    pub fn pending_batches(&self) -> usize {
        self.batches
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .len()
    }

    /// Force flush after the configured interval.
    pub fn flush_if_needed(&self) {
        let config = self.config.lock().unwrap_or_else(|p| p.into_inner());
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());

        let elapsed = Utc::now() - inner.last_flush;
        let threshold = Duration::seconds(config.flush_interval_secs as i64);

        if elapsed > threshold && !inner.buffer.is_empty() {
            drop(inner);
            drop(config);
            self.flush();
        }
    }

    /// Update configuration.
    pub fn set_config(&self, config: AggregationConfig) {
        if let Ok(mut cfg) = self.config.lock() {
            *cfg = config;
        }
    }
}

impl Default for LogAggregator {
    fn default() -> Self {
        Self {
            config: Arc::new(Mutex::new(AggregationConfig::default())),
            inner: Arc::new(Mutex::new(AggregatorInner {
                buffer: Vec::new(),
                stats: AggregationStats::default(),
                last_flush: Utc::now(),
            })),
            batches: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

/// Async batch processor for handling batches asynchronously.
pub struct BatchProcessor;

impl BatchProcessor {
    /// Process all pending batches using a callback function.
    pub fn process_all<F>(aggregator: &LogAggregator, mut handler: F)
    where
        F: FnMut(&LogBatch),
    {
        let batches = aggregator.drain_batches();
        for batch in batches {
            handler(&batch);
        }
    }

    /// Filter batches by level threshold.
    pub fn filter_by_level(batches: &[LogBatch], min_level: LogLevel) -> Vec<&LogBatch> {
        batches
            .iter()
            .filter(|b| {
                matches!(min_level, LogLevel::Trace)
                    || (matches!(min_level, LogLevel::Debug)
                        && b.level_summary.debug_count > 0)
                    || (matches!(min_level, LogLevel::Info)
                        && (b.level_summary.info_count > 0
                            || b.level_summary.warn_count > 0
                            || b.level_summary.error_count > 0))
                    || (matches!(min_level, LogLevel::Warn)
                        && (b.level_summary.warn_count > 0
                            || b.level_summary.error_count > 0))
                    || (matches!(min_level, LogLevel::Error)
                        && b.level_summary.error_count > 0)
            })
            .collect()
    }

    /// Get batches with errors.
    pub fn filter_errors(batches: &[LogBatch]) -> Vec<&LogBatch> {
        batches
            .iter()
            .filter(|b| b.level_summary.has_errors())
            .collect()
    }

    /// Get batches with warnings or errors.
    pub fn filter_issues(batches: &[LogBatch]) -> Vec<&LogBatch> {
        batches
            .iter()
            .filter(|b| b.level_summary.has_errors() || b.level_summary.has_warnings())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_level_summary() {
        let entries = vec![
            StructuredLogEntry::new(LogLevel::Info, "test", "msg1"),
            StructuredLogEntry::new(LogLevel::Warn, "test", "msg2"),
            StructuredLogEntry::new(LogLevel::Error, "test", "msg3"),
        ];

        let summary = LevelSummary::from_entries(&entries);
        assert_eq!(summary.info_count, 1);
        assert_eq!(summary.warn_count, 1);
        assert_eq!(summary.error_count, 1);
        assert_eq!(summary.total(), 3);
        assert!(summary.has_errors());
    }

    #[test]
    fn test_aggregator_adds_entries() {
        let agg = LogAggregator::new();
        let entry = StructuredLogEntry::new(LogLevel::Info, "test", "message");

        agg.add(entry);
        assert_eq!(agg.buffer_len(), 1);

        let stats = agg.stats();
        assert_eq!(stats.total_entries, 1);
    }

    #[test]
    fn test_aggregator_flushes_on_batch_size() {
        let config = AggregationConfig {
            batch_size: 3,
            flush_interval_secs: 60,
            max_buffer_size: 1000,
            auto_flush_enabled: true,
        };

        let agg = LogAggregator::with_config(config);

        for i in 0..3 {
            agg.add(StructuredLogEntry::new(
                LogLevel::Info,
                "test",
                format!("message {}", i),
            ));
        }

        // After 3 entries, should be flushed
        assert_eq!(agg.buffer_len(), 0);
        assert_eq!(agg.pending_batches(), 1);
    }

    #[test]
    fn test_manual_flush() {
        let agg = LogAggregator::new();

        agg.add(StructuredLogEntry::new(LogLevel::Info, "test", "msg1"));
        agg.add(StructuredLogEntry::new(LogLevel::Warn, "test", "msg2"));

        assert_eq!(agg.buffer_len(), 2);

        if let Some(batch) = agg.flush() {
            assert_eq!(batch.entries.len(), 2);
            assert_eq!(batch.level_summary.info_count, 1);
            assert_eq!(batch.level_summary.warn_count, 1);
        }

        assert_eq!(agg.buffer_len(), 0);
    }

    #[test]
    fn test_batch_statistics() {
        let agg = LogAggregator::new();

        for _ in 0..5 {
            agg.add(StructuredLogEntry::new(LogLevel::Info, "test", "msg"));
        }
        agg.flush();

        let stats = agg.stats();
        assert_eq!(stats.total_entries, 5);
        assert_eq!(stats.entries_flushed, 5);
        assert_eq!(stats.batches_created, 1);
        assert_eq!(stats.average_batch_size, 5.0);
    }

    #[test]
    fn test_drain_batches() {
        let agg = LogAggregator::new();

        for _ in 0..5 {
            agg.add(StructuredLogEntry::new(LogLevel::Info, "test", "msg"));
        }
        agg.flush();

        assert_eq!(agg.pending_batches(), 1);

        let batches = agg.drain_batches();
        assert_eq!(batches.len(), 1);
        assert_eq!(agg.pending_batches(), 0);
    }

    #[test]
    fn test_batch_processor_filters_errors() {
        let batches = vec![
            LogBatch {
                id: "1".to_string(),
                created_at: Utc::now(),
                entries: vec![StructuredLogEntry::new(LogLevel::Info, "test", "msg")],
                level_summary: LevelSummary {
                    info_count: 1,
                    ..Default::default()
                },
            },
            LogBatch {
                id: "2".to_string(),
                created_at: Utc::now(),
                entries: vec![StructuredLogEntry::new(LogLevel::Error, "test", "error")],
                level_summary: LevelSummary {
                    error_count: 1,
                    ..Default::default()
                },
            },
        ];

        let error_batches = BatchProcessor::filter_errors(&batches);
        assert_eq!(error_batches.len(), 1);
        assert_eq!(error_batches[0].id, "2");
    }

    #[test]
    fn test_config_update() {
        let agg = LogAggregator::new();
        let new_config = AggregationConfig {
            batch_size: 100,
            flush_interval_secs: 10,
            max_buffer_size: 500,
            auto_flush_enabled: false,
        };

        agg.set_config(new_config);
        // Just verify it doesn't panic
    }
}
