//! Structured JSON logging for Ethos-Protocol backend
//!
//! Provides:
//! - JSON-formatted structured logs with timestamp, level, module, message, and fields
//! - Support for multiple log levels (TRACE, DEBUG, INFO, WARN, ERROR)
//! - Module-based log filtering and tracking
//! - Log aggregation with batching and buffering
//! - Search and query capabilities
//! - Sink integration for different output targets
//!
//! Example:
//! ```ignore
//! let logger = StructuredLogger::new();
//! logger.info("vault_created", json!({"vault_id": "42", "owner": "alice"}));
//! logger.error("checkin_failed", json!({"reason": "timeout", "attempt": 3}));
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

/// Log level hierarchy: TRACE < DEBUG < INFO < WARN < ERROR
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum LogLevel {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Trace => "TRACE",
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_uppercase().as_str() {
            "TRACE" => Some(Self::Trace),
            "DEBUG" => Some(Self::Debug),
            "INFO" => Some(Self::Info),
            "WARN" => Some(Self::Warn),
            "ERROR" => Some(Self::Error),
            _ => None,
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// A structured log entry with metadata and fields.
///
/// Serializes to JSON with the following format:
/// ```json
/// {
///   "timestamp": "2026-09-28T18:44:31.663Z",
///   "level": "INFO",
///   "module": "vault_manager",
///   "message": "Vault created successfully",
///   "fields": {
///     "vault_id": "42",
///     "owner": "alice",
///     "amount": "1000.50"
///   },
///   "correlation_id": "req-123-456",
///   "trace_id": "trace-789"
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuredLogEntry {
    pub timestamp: DateTime<Utc>,
    pub level: LogLevel,
    pub module: String,
    pub message: String,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub fields: HashMap<String, JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

impl StructuredLogEntry {
    /// Create a new structured log entry.
    pub fn new(level: LogLevel, module: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            timestamp: Utc::now(),
            level,
            module: module.into(),
            message: message.into(),
            fields: HashMap::new(),
            correlation_id: None,
            trace_id: None,
        }
    }

    /// Add a field to the log entry.
    pub fn with_field(mut self, key: impl Into<String>, value: JsonValue) -> Self {
        self.fields.insert(key.into(), value);
        self
    }

    /// Add multiple fields.
    pub fn with_fields(mut self, fields: HashMap<String, JsonValue>) -> Self {
        self.fields.extend(fields);
        self
    }

    /// Set correlation ID for request tracing.
    pub fn with_correlation_id(mut self, id: impl Into<String>) -> Self {
        self.correlation_id = Some(id.into());
        self
    }

    /// Set trace ID for distributed tracing.
    pub fn with_trace_id(mut self, id: impl Into<String>) -> Self {
        self.trace_id = Some(id.into());
        self
    }

    /// Serialize to JSON string.
    pub fn to_json_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            // Fallback: manually construct JSON if serialization fails
            format!(
                r#"{{"timestamp":"{}","level":"{}","module":"{}","message":"{}"}}"#,
                self.timestamp.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                self.level,
                self.module,
                self.message.replace('"', "\\\"")
            )
        })
    }
}

/// Configuration for structured logging behavior.
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// Minimum log level to emit (logs below this are dropped)
    pub min_level: LogLevel,
    /// Maximum number of entries to buffer before flushing
    pub buffer_size: usize,
    /// Modules to include (empty = all modules)
    pub included_modules: Vec<String>,
    /// Modules to exclude
    pub excluded_modules: Vec<String>,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            min_level: LogLevel::Info,
            buffer_size: 100,
            included_modules: Vec::new(),
            excluded_modules: Vec::new(),
        }
    }
}

/// Log sink — where logs are sent (e.g., stdout, file, remote endpoint).
pub trait LogSink: Send + Sync {
    fn write(&self, entry: &StructuredLogEntry);
    fn flush(&self);
}

/// Standard output sink (prints to stdout).
pub struct StdoutSink;

impl LogSink for StdoutSink {
    fn write(&self, entry: &StructuredLogEntry) {
        println!("{}", entry.to_json_string());
    }

    fn flush(&self) {
        // stdout is typically unbuffered or line-buffered
    }
}

/// File sink (writes to a file).
pub struct FileSink {
    path: String,
}

impl FileSink {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
        }
    }
}

impl LogSink for FileSink {
    fn write(&self, entry: &StructuredLogEntry) {
        use std::fs::OpenOptions;
        use std::io::Write;

        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(file, "{}", entry.to_json_string());
        }
    }

    fn flush(&self) {
        // File will be flushed on close
    }
}

/// In-memory sink for testing and analysis.
pub struct MemorySink {
    entries: Arc<Mutex<Vec<StructuredLogEntry>>>,
}

impl MemorySink {
    pub fn new() -> Self {
        Self {
            entries: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn entries(&self) -> Vec<StructuredLogEntry> {
        self.entries.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn clear(&self) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.clear();
        }
    }
}

impl Default for MemorySink {
    fn default() -> Self {
        Self::new()
    }
}

impl LogSink for MemorySink {
    fn write(&self, entry: &StructuredLogEntry) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.push(entry.clone());
        }
    }

    fn flush(&self) {
        // In-memory sink doesn't need flushing
    }
}

/// Main structured logger with buffering and multiple sinks.
pub struct StructuredLogger {
    config: Arc<Mutex<LogConfig>>,
    buffer: Arc<Mutex<Vec<StructuredLogEntry>>>,
    sinks: Arc<Mutex<Vec<Arc<dyn LogSink>>>>,
}

impl StructuredLogger {
    /// Create a new logger with default configuration.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            config: Arc::new(Mutex::new(LogConfig::default())),
            buffer: Arc::new(Mutex::new(Vec::new())),
            sinks: Arc::new(Mutex::new(vec![Arc::new(StdoutSink)])),
        })
    }

    /// Create a logger with custom configuration.
    pub fn with_config(config: LogConfig) -> Arc<Self> {
        Arc::new(Self {
            config: Arc::new(Mutex::new(config)),
            buffer: Arc::new(Mutex::new(Vec::new())),
            sinks: Arc::new(Mutex::new(vec![Arc::new(StdoutSink)])),
        })
    }

    /// Add a sink to the logger.
    pub fn add_sink(&self, sink: Arc<dyn LogSink>) {
        if let Ok(mut sinks) = self.sinks.lock() {
            sinks.push(sink);
        }
    }

    /// Update the log configuration.
    pub fn set_config(&self, config: LogConfig) {
        if let Ok(mut cfg) = self.config.lock() {
            *cfg = config;
        }
    }

    /// Log an entry at the given level.
    pub fn log(&self, entry: StructuredLogEntry) {
        let config = self.config.lock().unwrap_or_else(|p| p.into_inner());

        // Check if this log level and module should be emitted
        if entry.level < config.min_level {
            return;
        }

        if !config.included_modules.is_empty()
            && !config.included_modules.contains(&entry.module)
        {
            return;
        }

        if config.excluded_modules.contains(&entry.module) {
            return;
        }

        // Add to buffer
        if let Ok(mut buffer) = self.buffer.lock() {
            buffer.push(entry);

            // Flush if buffer is full
            if buffer.len() >= config.buffer_size {
                drop(config); // Release config lock before flush
                self.flush();
                return;
            }
        }
    }

    /// Log at TRACE level.
    pub fn trace(&self, module: impl Into<String>, message: impl Into<String>) {
        self.log(StructuredLogEntry::new(LogLevel::Trace, module, message));
    }

    /// Log at DEBUG level.
    pub fn debug(&self, module: impl Into<String>, message: impl Into<String>) {
        self.log(StructuredLogEntry::new(LogLevel::Debug, module, message));
    }

    /// Log at INFO level.
    pub fn info(&self, module: impl Into<String>, message: impl Into<String>) {
        self.log(StructuredLogEntry::new(LogLevel::Info, module, message));
    }

    /// Log at WARN level.
    pub fn warn(&self, module: impl Into<String>, message: impl Into<String>) {
        self.log(StructuredLogEntry::new(LogLevel::Warn, module, message));
    }

    /// Log at ERROR level.
    pub fn error(&self, module: impl Into<String>, message: impl Into<String>) {
        self.log(StructuredLogEntry::new(LogLevel::Error, module, message));
    }

    /// Flush buffered logs to all sinks.
    pub fn flush(&self) {
        let mut buffer = self.buffer.lock().unwrap_or_else(|p| p.into_inner());
        let sinks = self.sinks.lock().unwrap_or_else(|p| p.into_inner());

        for entry in buffer.drain(..) {
            for sink in sinks.iter() {
                sink.write(&entry);
            }
        }

        // Flush all sinks
        for sink in sinks.iter() {
            sink.flush();
        }
    }

    /// Get the current buffer size.
    pub fn buffer_len(&self) -> usize {
        self.buffer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .len()
    }
}

impl Default for StructuredLogger {
    fn default() -> Self {
        Self {
            config: Arc::new(Mutex::new(LogConfig::default())),
            buffer: Arc::new(Mutex::new(Vec::new())),
            sinks: Arc::new(Mutex::new(vec![Arc::new(StdoutSink)])),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_level_ordering() {
        assert!(LogLevel::Trace < LogLevel::Debug);
        assert!(LogLevel::Debug < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Warn);
        assert!(LogLevel::Warn < LogLevel::Error);
    }

    #[test]
    fn test_log_level_from_str() {
        assert_eq!(LogLevel::from_str("INFO"), Some(LogLevel::Info));
        assert_eq!(LogLevel::from_str("error"), Some(LogLevel::Error));
        assert_eq!(LogLevel::from_str("unknown"), None);
    }

    #[test]
    fn test_structured_log_entry_serialization() {
        let entry = StructuredLogEntry::new(LogLevel::Info, "vault_manager", "Vault created");
        let json_str = entry.to_json_string();
        assert!(json_str.contains("\"level\":\"INFO\""));
        assert!(json_str.contains("\"module\":\"vault_manager\""));
        assert!(json_str.contains("\"message\":\"Vault created\""));
    }

    #[test]
    fn test_log_entry_with_fields() {
        let entry = StructuredLogEntry::new(LogLevel::Info, "test", "Test message")
            .with_field("vault_id", json!("42"))
            .with_field("owner", json!("alice"));

        let json_str = entry.to_json_string();
        assert!(json_str.contains("\"vault_id\":\"42\""));
        assert!(json_str.contains("\"owner\":\"alice\""));
    }

    #[test]
    fn test_log_entry_with_trace_ids() {
        let entry = StructuredLogEntry::new(LogLevel::Info, "test", "Test")
            .with_correlation_id("req-123")
            .with_trace_id("trace-456");

        let json_str = entry.to_json_string();
        assert!(json_str.contains("\"correlation_id\":\"req-123\""));
        assert!(json_str.contains("\"trace_id\":\"trace-456\""));
    }

    #[test]
    fn test_memory_sink_captures_logs() {
        let sink = Arc::new(MemorySink::new());
        let logger = StructuredLogger::new();
        logger.sinks.lock().unwrap().clear();
        logger.add_sink(sink.clone());

        let entry = StructuredLogEntry::new(LogLevel::Info, "test", "Test message");
        logger.log(entry);
        logger.flush();

        let entries = sink.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].level, LogLevel::Info);
        assert_eq!(entries[0].message, "Test message");
    }

    #[test]
    fn test_buffer_flushes_automatically() {
        let sink = Arc::new(MemorySink::new());
        let config = LogConfig {
            min_level: LogLevel::Info,
            buffer_size: 2,
            included_modules: Vec::new(),
            excluded_modules: Vec::new(),
        };

        let logger = StructuredLogger::with_config(config);
        logger.sinks.lock().unwrap().clear();
        logger.add_sink(sink.clone());

        // Add two entries (should trigger flush)
        logger.log(StructuredLogEntry::new(LogLevel::Info, "test", "Entry 1"));
        logger.log(StructuredLogEntry::new(LogLevel::Info, "test", "Entry 2"));

        let entries = sink.entries();
        assert!(entries.len() >= 2);
    }

    #[test]
    fn test_log_level_filtering() {
        let sink = Arc::new(MemorySink::new());
        let config = LogConfig {
            min_level: LogLevel::Warn,
            buffer_size: 100,
            included_modules: Vec::new(),
            excluded_modules: Vec::new(),
        };

        let logger = StructuredLogger::with_config(config);
        logger.sinks.lock().unwrap().clear();
        logger.add_sink(sink.clone());

        logger.debug("test", "Debug message");
        logger.warn("test", "Warn message");
        logger.error("test", "Error message");
        logger.flush();

        let entries = sink.entries();
        assert_eq!(entries.len(), 2); // Only WARN and ERROR
        assert!(entries[0].level >= LogLevel::Warn);
    }

    #[test]
    fn test_module_filtering() {
        let sink = Arc::new(MemorySink::new());
        let config = LogConfig {
            min_level: LogLevel::Info,
            buffer_size: 100,
            included_modules: vec!["vault_manager".to_string()],
            excluded_modules: Vec::new(),
        };

        let logger = StructuredLogger::with_config(config);
        logger.sinks.lock().unwrap().clear();
        logger.add_sink(sink.clone());

        logger.info("vault_manager", "Vault message");
        logger.info("other_module", "Other message");
        logger.flush();

        let entries = sink.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].module, "vault_manager");
    }

    #[test]
    fn test_module_exclusion() {
        let sink = Arc::new(MemorySink::new());
        let config = LogConfig {
            min_level: LogLevel::Info,
            buffer_size: 100,
            included_modules: Vec::new(),
            excluded_modules: vec!["noisy_module".to_string()],
        };

        let logger = StructuredLogger::with_config(config);
        logger.sinks.lock().unwrap().clear();
        logger.add_sink(sink.clone());

        logger.info("important_module", "Important message");
        logger.info("noisy_module", "Noisy message");
        logger.flush();

        let entries = sink.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].module, "important_module");
    }

    #[test]
    fn test_multiple_sinks() {
        let sink1 = Arc::new(MemorySink::new());
        let sink2 = Arc::new(MemorySink::new());
        let logger = StructuredLogger::new();
        logger.sinks.lock().unwrap().clear();
        logger.add_sink(sink1.clone());
        logger.add_sink(sink2.clone());

        logger.info("test", "Test message");
        logger.flush();

        assert_eq!(sink1.entries().len(), 1);
        assert_eq!(sink2.entries().len(), 1);
    }

    #[test]
    fn test_convenience_methods() {
        let sink = Arc::new(MemorySink::new());
        let logger = StructuredLogger::new();
        logger.sinks.lock().unwrap().clear();
        logger.add_sink(sink.clone());

        logger.trace("test", "Trace message");
        logger.debug("test", "Debug message");
        logger.info("test", "Info message");
        logger.warn("test", "Warn message");
        logger.error("test", "Error message");
        logger.flush();

        let entries = sink.entries();
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0].level, LogLevel::Trace);
        assert_eq!(entries[4].level, LogLevel::Error);
    }
}
