# Structured JSON Logging for Ethos-Protocol

## Overview

Ethos-Protocol now includes a comprehensive structured logging system that provides:

- **JSON-formatted logs** with rich metadata (timestamp, level, module, correlation IDs)
- **5 log levels** with strict ordering (TRACE < DEBUG < INFO < WARN < ERROR)
- **Module-based filtering** for controlling log verbosity
- **Log aggregation** with automatic batching and flushing
- **Fast log search** with multiple index types (by module, level, correlation ID)
- **Request tracing** via correlation and trace IDs
- **Multiple sink targets** (stdout, file, memory, extensible to remote)

## Quick Start

### Basic Logging

```rust
use ethos_protocol_backend::structured_logging::{StructuredLogger, LogLevel};
use serde_json::json;

let logger = StructuredLogger::new();

// Log at INFO level
logger.info("vault_manager", "Vault created successfully");

// Log with fields
logger.log(
    logger.info("vault_manager", "Vault created")
        .with_field("vault_id", json!("42"))
        .with_field("owner", json!("alice"))
);

// Log with correlation ID for tracing
logger.log(
    logger.info("checkin_handler", "Check-in processed")
        .with_correlation_id("req-12345")
);
```

### JSON Output Format

```json
{
  "timestamp": "2026-09-28T18:44:31.663Z",
  "level": "INFO",
  "module": "vault_manager",
  "message": "Vault created successfully",
  "fields": {
    "vault_id": "42",
    "owner": "alice"
  },
  "correlation_id": "req-12345",
  "trace_id": null
}
```

## Log Levels

The system provides 5 log levels with strict ordering:

| Level | Severity | Use Case |
|-------|----------|----------|
| **TRACE** | Lowest | Detailed diagnostic info, rarely needed |
| **DEBUG** | Low | Debugging info during development |
| **INFO** | Medium | Significant events (default minimum) |
| **WARN** | High | Warnings about potential issues |
| **ERROR** | Highest | Error conditions requiring attention |

```rust
logger.trace("module", "Detailed trace info");
logger.debug("module", "Debug information");
logger.info("module", "Important event");
logger.warn("module", "Warning about something");
logger.error("module", "Error occurred");
```

## Module Filtering

Control which modules produce logs:

```rust
use ethos_protocol_backend::structured_logging::{StructuredLogger, LogConfig, LogLevel};

let config = LogConfig {
    min_level: LogLevel::Debug,
    buffer_size: 100,
    included_modules: vec!["vault_manager".to_string()],  // Only log this module
    excluded_modules: vec!["health_check".to_string()],   // Exclude this module
};

let logger = StructuredLogger::with_config(config);
```

### Filter Strategies

**Log all modules:**
```rust
let config = LogConfig {
    included_modules: vec![],  // Empty = include all
    excluded_modules: vec![],
    ..Default::default()
};
```

**Log only important modules:**
```rust
let config = LogConfig {
    included_modules: vec![
        "vault_manager".to_string(),
        "checkin_handler".to_string(),
        "payment_processor".to_string(),
    ],
    ..Default::default()
};
```

**Log everything except noisy modules:**
```rust
let config = LogConfig {
    excluded_modules: vec![
        "health_check".to_string(),
        "metrics_collector".to_string(),
    ],
    ..Default::default()
};
```

## Log Aggregation

The aggregation system batches logs for efficient processing:

```rust
use ethos_protocol_backend::log_aggregation::{LogAggregator, AggregationConfig};

let config = AggregationConfig {
    batch_size: 50,              // Flush after 50 entries
    flush_interval_secs: 5,      // Or after 5 seconds
    max_buffer_size: 1000,       // Max entries in memory
    auto_flush_enabled: true,
};

let aggregator = LogAggregator::with_config(config);

// Add entries
aggregator.add(entry);

// Get statistics
let stats = aggregator.stats();
println!("Total entries: {}", stats.total_entries);
println!("Entries flushed: {}", stats.entries_flushed);
println!("Average batch size: {}", stats.average_batch_size);
```

### Batching Features

- **Automatic size-based flushing**: Flushes when batch reaches configured size
- **Time-based flushing**: Flushes partial batches after configured interval
- **Statistics tracking**: Peak buffer size, average batch size, flush times
- **Error/warning detection**: Quick filtering of batches with issues

```rust
// Filter batches with errors
let error_batches = BatchProcessor::filter_errors(&batches);

// Get only warning/error level logs
let issue_batches = BatchProcessor::filter_issues(&batches);
```

## Log Search and Queries

Fast searching with multiple index types:

```rust
use ethos_protocol_backend::log_search::{LogIndex, LogQuery, LogLevel};

let index = LogIndex::new();

// Add logs to index
index.add(entry1);
index.add(entry2);
index.add(entry3);

// Search by level
let query = LogQuery::new()
    .with_min_level(LogLevel::Warn);
let results = index.search(&query);

// Search by module
let query = LogQuery::new()
    .with_module("vault_manager");
let results = index.search(&query);

// Search by message pattern
let query = LogQuery::new()
    .with_message_pattern("*failed*");
let results = index.search(&query);

// Search by time range
let query = LogQuery::new()
    .with_time_range(from_time, to_time);
let results = index.search(&query);

// Trace a request
let logs = index.by_correlation_id("req-12345");

// Get logs by module
let logs = index.by_module("vault_manager");

// Get logs by level
let logs = index.by_level(LogLevel::Error);
```

### Query Builder Methods

```rust
let query = LogQuery::new()
    .with_min_level(LogLevel::Warn)           // Level threshold
    .with_module("*_handler")                 // Pattern matching
    .with_message_pattern("*timeout*")        // Message wildcard
    .with_field_filter("vault_id", "42")      // Custom field
    .with_time_range(from, to)                // Temporal filtering
    .with_correlation_id("req-12345")         // Request tracing
    .with_trace_id("trace-456")               // Distributed tracing
    .with_limit(100)                          // Result limit
    .with_offset(10);                         // Pagination
```

### Aggregation Queries

```rust
let query = LogQuery::new().with_min_level(LogLevel::Warn);
let agg = index.aggregate(&query);

println!("Total entries: {}", agg.total_entries);
println!("By level: {:?}", agg.by_level);
println!("By module: {:?}", agg.by_module);
println!("Error count: {}", agg.error_count);
println!("Warning count: {}", agg.warning_count);
```

## Sink Integration

Multiple sink targets for flexibility:

```rust
use ethos_protocol_backend::structured_logging::{
    StructuredLogger, StdoutSink, FileSink, MemorySink,
};
use std::sync::Arc;

let logger = StructuredLogger::new();

// Add file sink
logger.add_sink(Arc::new(FileSink::new("/var/log/ethos.log")));

// Add memory sink for testing
let mem_sink = Arc::new(MemorySink::new());
logger.add_sink(mem_sink.clone());

logger.info("app", "Starting application");
logger.flush();

// Retrieve logs from memory sink
let entries = mem_sink.entries();
for entry in entries {
    println!("{}", entry.to_json_string());
}
```

### Built-in Sinks

| Sink | Purpose | Example |
|------|---------|---------|
| **StdoutSink** | Print to stdout | Console output during development |
| **FileSink** | Write to file | Production log files |
| **MemorySink** | In-memory storage | Testing and analysis |

## Request Tracing

Track requests across services using correlation IDs:

```rust
use axum::http::HeaderMap;

// Extract from request headers
let correlation_id = extract_from_header(&headers, "x-correlation-id");

// Or generate new
let correlation_id = format!("req-{}", uuid::Uuid::new_v4());

// Log with correlation ID
logger.log(
    entry.with_correlation_id(&correlation_id)
);

// Later, find all logs for this request
let request_logs = index.by_correlation_id(&correlation_id);
```

## Middleware Integration

Automatic logging of HTTP requests and responses:

```rust
use ethos_protocol_backend::structured_logging_middleware::structured_logging_middleware;

let app = Router::new()
    .route("/api/vaults", get(list_vaults))
    .layer(axum::middleware::from_fn_with_state(
        logger.clone(),
        structured_logging_middleware,
    ));
```

The middleware automatically:
- Logs all HTTP requests with method, path, and correlation ID
- Logs responses with status code and response time
- Skips internal routes (/health, /ready, /metrics)
- Generates unique correlation IDs if not present
- Categorizes status codes to appropriate log levels

## Configuration via Environment

```bash
# Minimum log level (default: INFO)
export LOG_LEVEL=DEBUG

# Modules to include (empty = all)
export LOG_MODULES=vault_manager,checkin_handler

# Modules to exclude
export LOG_EXCLUDE_MODULES=health_check,metrics

# Buffer size before auto-flush
export LOG_BUFFER_SIZE=100

# Batch size for aggregation
export LOG_BATCH_SIZE=50

# Enable/disable structured logging
export STRUCTURED_LOGGING_ENABLED=true
```

## Performance Tuning

### Buffering Strategy

**Low latency (real-time logging):**
```rust
let config = LogConfig {
    buffer_size: 1,  // Flush immediately
    ..Default::default()
};
```

**High throughput (batch logging):**
```rust
let config = LogConfig {
    buffer_size: 1000,  // Accumulate before flushing
    ..Default::default()
};
```

### Aggregation Strategy

**Real-time analytics:**
```rust
let config = AggregationConfig {
    batch_size: 10,
    flush_interval_secs: 1,
    ..Default::default()
};
```

**High-volume environments:**
```rust
let config = AggregationConfig {
    batch_size: 500,
    flush_interval_secs: 30,
    max_buffer_size: 10000,
    ..Default::default()
};
```

## Testing with Structured Logs

```rust
#[test]
fn test_vault_creation_with_logging() {
    let mem_sink = Arc::new(MemorySink::new());
    let logger = StructuredLogger::new();
    logger.add_sink(mem_sink.clone());
    
    // Execute test
    create_vault(&logger);
    logger.flush();
    
    // Assert logs
    let entries = mem_sink.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].level, LogLevel::Info);
    assert_eq!(entries[0].module, "vault_manager");
}
```

## Error Handling

Structured logging includes defensive error handling:

```rust
// Lock poisoning fallback
let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());

// JSON serialization fallback
let json_str = entry.to_json_string();  // Never panics

// Buffer overflow handling
if buffer.len() >= config.buffer_size {
    self.flush();  // Auto-flush prevents overflow
}
```

## Observability Patterns

### Pattern 1: Request Tracing
```rust
let entry = StructuredLogEntry::new(LogLevel::Info, "handler", "Processing request")
    .with_correlation_id("req-12345")
    .with_field("user_id", json!("alice"));

// All logs with same correlation_id can be traced end-to-end
```

### Pattern 2: Error Aggregation
```rust
// Query for all errors in the last hour
let query = LogQuery::new()
    .with_min_level(LogLevel::Error)
    .with_time_range(now - 1h, now);
let errors = index.search(&query);
```

### Pattern 3: Performance Monitoring
```rust
logger.log(
    entry.with_field("operation", json!("database_query"))
         .with_field("duration_ms", json!(42))
         .with_field("affected_rows", json!(1000))
);
```

### Pattern 4: Distributed Tracing
```rust
// Service A
logger.log(entry.with_trace_id("trace-xyz"));

// Service B (receives trace_id in header)
logger.log(entry.with_trace_id("trace-xyz"));

// Later: get all logs with same trace_id across services
```

## Best Practices

1. **Use consistent module names** for easier filtering
2. **Include context fields** for debugging (IDs, amounts, users)
3. **Use correlation IDs** for all customer-facing requests
4. **Flush periodically** to ensure logs aren't lost
5. **Set appropriate log levels** (DEBUG for dev, INFO for production)
6. **Exclude noisy modules** to reduce log volume
7. **Archive old logs** to manage storage
8. **Monitor aggregation stats** for system health

## Troubleshooting

### Logs not appearing?
- Check `min_level` setting
- Verify module not in `excluded_modules`
- Call `logger.flush()` to ensure logs are written

### Search is slow?
- Use more specific queries with `with_limit()`
- Consider module-based filtering
- Archive old logs to reduce index size

### High memory usage?
- Reduce `buffer_size` in LogConfig
- Reduce `max_buffer_size` in AggregationConfig
- Increase flush frequency

## API Reference

### StructuredLogEntry
```rust
pub struct StructuredLogEntry {
    pub timestamp: DateTime<Utc>,
    pub level: LogLevel,
    pub module: String,
    pub message: String,
    pub fields: HashMap<String, JsonValue>,
    pub correlation_id: Option<String>,
    pub trace_id: Option<String>,
}
```

### StructuredLogger
```rust
impl StructuredLogger {
    pub fn new() -> Arc<Self>
    pub fn with_config(config: LogConfig) -> Arc<Self>
    pub fn add_sink(&self, sink: Arc<dyn LogSink>)
    pub fn set_config(&self, config: LogConfig)
    pub fn log(&self, entry: StructuredLogEntry)
    pub fn trace/debug/info/warn/error(&self, module, message)
    pub fn flush(&self)
    pub fn buffer_len(&self) -> usize
}
```

### LogAggregator
```rust
impl LogAggregator {
    pub fn new() -> Arc<Self>
    pub fn with_config(config: AggregationConfig) -> Arc<Self>
    pub fn add(&self, entry: StructuredLogEntry)
    pub fn flush(&self) -> Option<LogBatch>
    pub fn drain_batches(&self) -> Vec<LogBatch>
    pub fn stats(&self) -> AggregationStats
    pub fn flush_if_needed(&self)
}
```

### LogIndex
```rust
impl LogIndex {
    pub fn new() -> Arc<Self>
    pub fn add(&self, entry: StructuredLogEntry)
    pub fn search(&self, query: &LogQuery) -> SearchResults
    pub fn by_correlation_id(&self, id: &str) -> Vec<StructuredLogEntry>
    pub fn by_module(&self, module: &str) -> Vec<StructuredLogEntry>
    pub fn by_level(&self, level: LogLevel) -> Vec<StructuredLogEntry>
    pub fn aggregate(&self, query: &LogQuery) -> AggregationResults
}
```

## See Also

- [Logging Modules Documentation](../backend/src/structured_logging.rs)
- [Log Aggregation Implementation](../backend/src/log_aggregation.rs)
- [Log Search Implementation](../backend/src/log_search.rs)
- [Middleware Integration](../backend/src/structured_logging_middleware.rs)
