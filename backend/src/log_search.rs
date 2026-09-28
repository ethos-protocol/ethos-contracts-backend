//! Log search and query functionality for structured logs
//!
//! Provides:
//! - Field-based search with wildcards and regex
//! - Time range queries
//! - Level and module filtering
//! - Aggregation queries (count, group-by)
//! - Stored log index for fast retrieval

use crate::structured_logging::{LogLevel, StructuredLogEntry};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// A query for searching logs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogQuery {
    pub min_level: Option<LogLevel>,
    pub max_level: Option<LogLevel>,
    pub module: Option<String>,
    pub message_pattern: Option<String>,
    pub field_filters: HashMap<String, String>,
    pub from_time: Option<DateTime<Utc>>,
    pub to_time: Option<DateTime<Utc>>,
    pub correlation_id: Option<String>,
    pub trace_id: Option<String>,
    pub limit: usize,
    pub offset: usize,
}

impl Default for LogQuery {
    fn default() -> Self {
        Self {
            min_level: None,
            max_level: None,
            module: None,
            message_pattern: None,
            field_filters: HashMap::new(),
            from_time: None,
            to_time: None,
            correlation_id: None,
            trace_id: None,
            limit: 100,
            offset: 0,
        }
    }
}

impl LogQuery {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_min_level(mut self, level: LogLevel) -> Self {
        self.min_level = Some(level);
        self
    }

    pub fn with_module(mut self, module: impl Into<String>) -> Self {
        self.module = Some(module.into());
        self
    }

    pub fn with_message_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.message_pattern = Some(pattern.into());
        self
    }

    pub fn with_field_filter(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.field_filters.insert(key.into(), value.into());
        self
    }

    pub fn with_time_range(mut self, from: DateTime<Utc>, to: DateTime<Utc>) -> Self {
        self.from_time = Some(from);
        self.to_time = Some(to);
        self
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    pub fn with_offset(mut self, offset: usize) -> Self {
        self.offset = offset;
        self
    }

    /// Check if an entry matches all query criteria.
    pub fn matches(&self, entry: &StructuredLogEntry) -> bool {
        // Level range check
        if let Some(min) = self.min_level {
            if entry.level < min {
                return false;
            }
        }
        if let Some(max) = self.max_level {
            if entry.level > max {
                return false;
            }
        }

        // Module filter
        if let Some(ref module_pattern) = self.module {
            if !matches_pattern(&entry.module, module_pattern) {
                return false;
            }
        }

        // Message pattern
        if let Some(ref pattern) = self.message_pattern {
            if !matches_pattern(&entry.message, pattern) {
                return false;
            }
        }

        // Field filters
        for (key, value_pattern) in &self.field_filters {
            match entry.fields.get(key) {
                Some(json_val) => {
                    let val_str = json_val.to_string();
                    if !matches_pattern(&val_str, value_pattern) {
                        return false;
                    }
                }
                None => return false,
            }
        }

        // Time range
        if let Some(from) = self.from_time {
            if entry.timestamp < from {
                return false;
            }
        }
        if let Some(to) = self.to_time {
            if entry.timestamp > to {
                return false;
            }
        }

        // Correlation ID
        if let Some(ref corr_id) = self.correlation_id {
            if entry.correlation_id.as_ref() != Some(corr_id) {
                return false;
            }
        }

        // Trace ID
        if let Some(ref trace_id) = self.trace_id {
            if entry.trace_id.as_ref() != Some(trace_id) {
                return false;
            }
        }

        true
    }
}

/// Results from a log search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResults {
    pub total: usize,
    pub entries: Vec<StructuredLogEntry>,
    pub query_time_ms: u128,
}

/// Aggregation results (group-by, counts, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregationResults {
    pub total_entries: usize,
    pub by_level: HashMap<String, usize>,
    pub by_module: HashMap<String, usize>,
    pub error_count: usize,
    pub warning_count: usize,
}

/// Log index for fast searching.
pub struct LogIndex {
    entries: RwLock<Vec<StructuredLogEntry>>,
    by_module: RwLock<HashMap<String, Vec<usize>>>,
    by_level: RwLock<HashMap<String, Vec<usize>>>,
    by_correlation_id: RwLock<HashMap<String, Vec<usize>>>,
}

impl LogIndex {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            entries: RwLock::new(Vec::new()),
            by_module: RwLock::new(HashMap::new()),
            by_level: RwLock::new(HashMap::new()),
            by_correlation_id: RwLock::new(HashMap::new()),
        })
    }

    /// Add an entry to the index.
    pub fn add(&self, entry: StructuredLogEntry) {
        let mut entries = self.entries.write().unwrap_or_else(|p| p.into_inner());
        let idx = entries.len();
        entries.push(entry.clone());

        // Update indices
        let mut by_module = self.by_module.write().unwrap_or_else(|p| p.into_inner());
        by_module
            .entry(entry.module.clone())
            .or_insert_with(Vec::new)
            .push(idx);

        let mut by_level = self.by_level.write().unwrap_or_else(|p| p.into_inner());
        by_level
            .entry(entry.level.to_string())
            .or_insert_with(Vec::new)
            .push(idx);

        if let Some(corr_id) = entry.correlation_id {
            let mut by_corr = self
                .by_correlation_id
                .write()
                .unwrap_or_else(|p| p.into_inner());
            by_corr
                .entry(corr_id)
                .or_insert_with(Vec::new)
                .push(idx);
        }
    }

    /// Execute a search query against the index.
    pub fn search(&self, query: &LogQuery) -> SearchResults {
        let start = std::time::Instant::now();

        let entries = self.entries.read().unwrap_or_else(|p| p.into_inner());
        let results: Vec<StructuredLogEntry> = entries
            .iter()
            .filter(|e| query.matches(e))
            .skip(query.offset)
            .take(query.limit)
            .cloned()
            .collect();

        let total = entries.iter().filter(|e| query.matches(e)).count();

        SearchResults {
            total,
            entries: results,
            query_time_ms: start.elapsed().as_millis(),
        }
    }

    /// Get logs by correlation ID.
    pub fn by_correlation_id(&self, corr_id: &str) -> Vec<StructuredLogEntry> {
        let by_corr = self
            .by_correlation_id
            .read()
            .unwrap_or_else(|p| p.into_inner());
        let entries = self.entries.read().unwrap_or_else(|p| p.into_inner());

        by_corr
            .get(corr_id)
            .map(|indices| {
                indices
                    .iter()
                    .filter_map(|&idx| entries.get(idx).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get logs by module.
    pub fn by_module(&self, module: &str) -> Vec<StructuredLogEntry> {
        let by_module = self.by_module.read().unwrap_or_else(|p| p.into_inner());
        let entries = self.entries.read().unwrap_or_else(|p| p.into_inner());

        by_module
            .get(module)
            .map(|indices| {
                indices
                    .iter()
                    .filter_map(|&idx| entries.get(idx).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get logs by level.
    pub fn by_level(&self, level: LogLevel) -> Vec<StructuredLogEntry> {
        let by_level = self.by_level.read().unwrap_or_else(|p| p.into_inner());
        let entries = self.entries.read().unwrap_or_else(|p| p.into_inner());

        by_level
            .get(level.as_str())
            .map(|indices| {
                indices
                    .iter()
                    .filter_map(|&idx| entries.get(idx).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Compute aggregation statistics.
    pub fn aggregate(&self, query: &LogQuery) -> AggregationResults {
        let entries = self.entries.read().unwrap_or_else(|p| p.into_inner());
        let filtered: Vec<_> = entries.iter().filter(|e| query.matches(e)).collect();

        let mut by_level: HashMap<String, usize> = HashMap::new();
        let mut by_module: HashMap<String, usize> = HashMap::new();
        let mut error_count = 0;
        let mut warning_count = 0;

        for entry in filtered {
            *by_level.entry(entry.level.to_string()).or_insert(0) += 1;
            *by_module.entry(entry.module.clone()).or_insert(0) += 1;

            if entry.level == LogLevel::Error {
                error_count += 1;
            }
            if entry.level == LogLevel::Warn {
                warning_count += 1;
            }
        }

        AggregationResults {
            total_entries: entries.iter().filter(|e| query.matches(e)).count(),
            by_level,
            by_module,
            error_count,
            warning_count,
        }
    }

    /// Clear all entries.
    pub fn clear(&self) {
        self.entries.write().unwrap_or_else(|p| p.into_inner()).clear();
        self.by_module.write().unwrap_or_else(|p| p.into_inner()).clear();
        self.by_level.write().unwrap_or_else(|p| p.into_inner()).clear();
        self.by_correlation_id
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }

    /// Get total entry count.
    pub fn len(&self) -> usize {
        self.entries.read().unwrap_or_else(|p| p.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for LogIndex {
    fn default() -> Self {
        Self {
            entries: RwLock::new(Vec::new()),
            by_module: RwLock::new(HashMap::new()),
            by_level: RwLock::new(HashMap::new()),
            by_correlation_id: RwLock::new(HashMap::new()),
        }
    }
}

/// Match text against a pattern with wildcards and case-insensitive matching.
fn matches_pattern(text: &str, pattern: &str) -> bool {
    let text_lower = text.to_lowercase();
    let pattern_lower = pattern.to_lowercase();

    if !pattern_lower.contains('*') && !pattern_lower.contains('?') {
        return text_lower.contains(&pattern_lower);
    }

    // Simple glob matching
    let segments: Vec<&str> = pattern_lower.split('*').collect();
    let mut cursor = 0;

    for (i, seg) in segments.iter().enumerate() {
        if seg.is_empty() {
            continue;
        }
        match text_lower[cursor..].find(seg) {
            Some(pos) => {
                if i == 0 && pos != 0 && !pattern_lower.starts_with('*') {
                    return false;
                }
                cursor += pos + seg.len();
            }
            None => return false,
        }
    }

    if let Some(last) = segments.last() {
        if !pattern_lower.ends_with('*') && !last.is_empty() && !text_lower.ends_with(last) {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_log_query_builder() {
        let query = LogQuery::new()
            .with_min_level(LogLevel::Warn)
            .with_module("vault_manager")
            .with_message_pattern("*failed*")
            .with_limit(50);

        assert_eq!(query.min_level, Some(LogLevel::Warn));
        assert_eq!(query.module, Some("vault_manager".to_string()));
        assert_eq!(query.limit, 50);
    }

    #[test]
    fn test_query_matches_entry() {
        let query = LogQuery::new()
            .with_min_level(LogLevel::Warn)
            .with_module("test");

        let entry = StructuredLogEntry::new(LogLevel::Error, "test", "message");
        assert!(query.matches(&entry));

        let entry_low = StructuredLogEntry::new(LogLevel::Debug, "test", "message");
        assert!(!query.matches(&entry_low));
    }

    #[test]
    fn test_query_matches_message_pattern() {
        let query = LogQuery::new().with_message_pattern("*created*");

        let entry = StructuredLogEntry::new(LogLevel::Info, "test", "Vault created successfully");
        assert!(query.matches(&entry));

        let entry_nomatch = StructuredLogEntry::new(LogLevel::Info, "test", "Vault deleted");
        assert!(!query.matches(&entry_nomatch));
    }

    #[test]
    fn test_log_index_add_and_search() {
        let index = LogIndex::new();

        index.add(StructuredLogEntry::new(LogLevel::Info, "module_a", "msg1"));
        index.add(StructuredLogEntry::new(LogLevel::Error, "module_b", "msg2"));
        index.add(StructuredLogEntry::new(LogLevel::Warn, "module_a", "msg3"));

        assert_eq!(index.len(), 3);
    }

    #[test]
    fn test_index_search_by_level() {
        let index = LogIndex::new();

        index.add(StructuredLogEntry::new(LogLevel::Info, "test", "msg1"));
        index.add(StructuredLogEntry::new(LogLevel::Error, "test", "msg2"));
        index.add(StructuredLogEntry::new(LogLevel::Warn, "test", "msg3"));

        let query = LogQuery::new().with_min_level(LogLevel::Warn);
        let results = index.search(&query);

        assert_eq!(results.entries.len(), 2); // WARN and ERROR
    }

    #[test]
    fn test_index_by_module() {
        let index = LogIndex::new();

        index.add(StructuredLogEntry::new(LogLevel::Info, "vault", "msg1"));
        index.add(StructuredLogEntry::new(LogLevel::Info, "checkin", "msg2"));
        index.add(StructuredLogEntry::new(LogLevel::Info, "vault", "msg3"));

        let vault_logs = index.by_module("vault");
        assert_eq!(vault_logs.len(), 2);
    }

    #[test]
    fn test_index_by_level() {
        let index = LogIndex::new();

        index.add(StructuredLogEntry::new(LogLevel::Info, "test", "msg1"));
        index.add(StructuredLogEntry::new(LogLevel::Error, "test", "msg2"));
        index.add(StructuredLogEntry::new(LogLevel::Error, "test", "msg3"));

        let error_logs = index.by_level(LogLevel::Error);
        assert_eq!(error_logs.len(), 2);
    }

    #[test]
    fn test_index_aggregation() {
        let index = LogIndex::new();

        index.add(StructuredLogEntry::new(LogLevel::Info, "module_a", "msg1"));
        index.add(StructuredLogEntry::new(LogLevel::Error, "module_b", "msg2"));
        index.add(StructuredLogEntry::new(LogLevel::Error, "module_a", "msg3"));

        let query = LogQuery::new();
        let agg = index.aggregate(&query);

        assert_eq!(agg.total_entries, 3);
        assert_eq!(agg.error_count, 2);
        assert_eq!(agg.warning_count, 0);
        assert_eq!(agg.by_level.get("INFO"), Some(&1));
        assert_eq!(agg.by_level.get("ERROR"), Some(&2));
    }

    #[test]
    fn test_matches_pattern() {
        assert!(matches_pattern("vault created", "vault*"));
        assert!(matches_pattern("ERROR timeout", "*timeout"));
        assert!(matches_pattern("check in failed", "*check*failed*"));
        assert!(!matches_pattern("all good", "*failed*"));
    }

    #[test]
    fn test_correlation_id_search() {
        let index = LogIndex::new();

        let entry1 = StructuredLogEntry::new(LogLevel::Info, "test", "msg1")
            .with_correlation_id("req-123");
        let entry2 = StructuredLogEntry::new(LogLevel::Info, "test", "msg2")
            .with_correlation_id("req-456");

        index.add(entry1);
        index.add(entry2);

        let logs = index.by_correlation_id("req-123");
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].message, "msg1");
    }
}
