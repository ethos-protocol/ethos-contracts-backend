//! Integration guide for structured logging in Ethos-Protocol backend
//!
//! This module demonstrates how to integrate the structured logging system
//! into the main application. The implementation is done in main.rs and middleware.

/*
INTEGRATION STEPS:

1. In `main.rs` import the structured logging modules:
   ```rust
   use ethos_protocol_backend::{
       structured_logging::{StructuredLogger, LogConfig, LogLevel, MemorySink},
       structured_logging_middleware::{structured_logging_middleware, LoggerState},
       log_aggregation::LogAggregator,
       log_search::LogIndex,
   };
   ```

2. Initialize the logger in your application setup:
   ```rust
   let log_config = LogConfig {
       min_level: LogLevel::Debug,
       buffer_size: 100,
       included_modules: vec![],  // Empty = log all modules
       excluded_modules: vec!["healthcheck".to_string()],  // Skip noisy modules
   };
   
   let logger = StructuredLogger::with_config(log_config);
   ```

3. Add the logger to your AppState:
   ```rust
   pub struct AppState {
       pub db: Arc<Db>,
       pub logger: Arc<StructuredLogger>,
       pub aggregator: Arc<LogAggregator>,
       pub log_index: Arc<LogIndex>,
       // ... other fields
   }
   ```

4. Initialize aggregator and index:
   ```rust
   let aggregator = LogAggregator::new();
   let log_index = LogIndex::new();
   ```

5. Add the middleware to your router:
   ```rust
   let app = Router::new()
       .route("/api/vaults", get(list_vaults))
       .layer(axum::middleware::from_fn_with_state(
           logger.clone(),
           structured_logging_middleware,
       ))
       .with_state(AppState {
           logger: logger.clone(),
           aggregator: aggregator.clone(),
           log_index: log_index.clone(),
           // ... other state
       });
   ```

6. Use logging in your handlers:
   ```rust
   pub async fn list_vaults(
       State(state): State<AppState>,
   ) -> Result<Json<Vec<Vault>>, ApiError> {
       // Log at info level with custom fields
       state.logger.log(
           StructuredLogEntry::new(LogLevel::Info, "vault_handler", "Listing vaults")
               .with_field("request_type", json!("list"))
               .with_correlation_id("req-12345"),
       );
       
       // Your handler logic...
   }
   ```

7. Flush logs periodically (e.g., in a background task):
   ```rust
   tokio::spawn(async move {
       loop {
           tokio::time::sleep(Duration::from_secs(5)).await;
           logger.flush();
           aggregator.flush_if_needed();
       }
   });
   ```

8. Add log search endpoints:
   ```rust
   pub async fn search_logs(
       State(state): State<AppState>,
       Query(query): Query<LogQuery>,
   ) -> Json<SearchResults> {
       Json(state.log_index.search(&query))
   }
   
   pub async fn get_log_stats(
       State(state): State<AppState>,
   ) -> Json<AggregationResults> {
       Json(state.log_index.aggregate(&LogQuery::new()))
   }
   ```

ENVIRONMENT CONFIGURATION:

Set environment variables to configure logging:
```bash
# Minimum log level: TRACE, DEBUG, INFO, WARN, ERROR (default: INFO)
LOG_LEVEL=DEBUG

# Comma-separated list of modules to log (empty = all)
LOG_MODULES=vault_manager,checkin_handler

# Comma-separated list of modules to exclude
LOG_EXCLUDE_MODULES=health_check,metrics_collector

# Log buffer size (entries before auto-flush)
LOG_BUFFER_SIZE=100

# Log batch size for aggregation
LOG_BATCH_SIZE=50

# Structured logging enabled (default: true)
STRUCTURED_LOGGING_ENABLED=true
```

MONITORING AND ANALYSIS:

Query logs via the search API:
```bash
# Get all error logs from past hour
curl "http://localhost:3000/api/logs/search?level=ERROR&from=2026-09-28T17:00:00Z"

# Search by module
curl "http://localhost:3000/api/logs/search?module=vault_manager"

# Trace a request by correlation ID
curl "http://localhost:3000/api/logs/search?correlation_id=req-12345"

# Aggregate statistics
curl "http://localhost:3000/api/logs/stats"
```

PERFORMANCE CONSIDERATIONS:

- Logging is asynchronous and buffered; flush periodically
- LogIndex keeps all logs in memory for fast search; consider retention policies
- Use module filtering to reduce log volume
- Batch aggregation reduces I/O overhead
- Multiple sinks (stdout, file, remote) supported for redundancy

ERROR HANDLING:

If the logger panics due to poisoned locks, it falls back to console output.
Lock poisoning should never happen in production, but defensive code ensures
observability even if logging infrastructure fails.

DISTRIBUTED TRACING:

Use correlation_id and trace_id fields for end-to-end request tracing:
```rust
let entry = StructuredLogEntry::new(LogLevel::Info, "service_a", "Processing")
    .with_correlation_id("req-12345")  // Track across services
    .with_trace_id("trace-456");       // Internal tracing

// Logs with the same correlation_id form a request chain
```
*/

// This file is documentation-only and serves as a reference for integration.
// The actual implementation goes into main.rs during application startup.
