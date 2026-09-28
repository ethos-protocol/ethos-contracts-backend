# Structured Logging - Practical Examples

This document provides practical examples for using the structured logging system.

## Example 1: Basic Logging in a Handler

```rust
use ethos_protocol_backend::structured_logging::{StructuredLogger, LogLevel, StructuredLogEntry};
use serde_json::json;
use axum::{extract::State, Json};

pub async fn create_vault(
    State(logger): State<Arc<StructuredLogger>>,
    Json(payload): Json<CreateVaultRequest>,
) -> Result<Json<VaultResponse>, ApiError> {
    // Log incoming request
    logger.log(
        StructuredLogEntry::new(LogLevel::Debug, "vault_handler", "Creating vault")
            .with_field("owner", json!(&payload.owner))
    );

    // Create vault...
    let vault = create_vault_internal(&payload)?;

    // Log success
    logger.info("vault_handler", "Vault created successfully");
    
    Ok(Json(VaultResponse { id: vault.id }))
}
```

## Example 2: Error Logging with Context

```rust
pub async fn process_checkin(
    State(logger): State<Arc<StructuredLogger>>,
    State(db): State<Arc<Db>>,
    vault_id: String,
) -> Result<Json<CheckinResponse>, ApiError> {
    match db.get_vault(&vault_id) {
        Ok(vault) => {
            logger.debug("checkin_handler", "Vault found");
            // Process checkin...
        }
        Err(e) => {
            logger.log(
                StructuredLogEntry::new(LogLevel::Error, "checkin_handler", "Vault not found")
                    .with_field("vault_id", json!(&vault_id))
                    .with_field("error", json!(e.to_string()))
            );
            return Err(ApiError::NotFound);
        }
    }
    Ok(Json(CheckinResponse { success: true }))
}
```

## Example 3: Performance Monitoring

```rust
use std::time::Instant;

pub async fn list_vaults(
    State(logger): State<Arc<StructuredLogger>>,
    State(db): State<Arc<Db>>,
) -> Result<Json<Vec<Vault>>, ApiError> {
    let start = Instant::now();
    
    let vaults = db.list_vaults()?;
    
    let duration_ms = start.elapsed().as_millis();
    
    logger.log(
        StructuredLogEntry::new(LogLevel::Info, "vault_handler", "Listed vaults")
            .with_field("count", json!(vaults.len()))
            .with_field("duration_ms", json!(duration_ms))
            .with_field("vaults_per_sec", json!(vaults.len() as f64 / (duration_ms as f64 / 1000.0)))
    );
    
    Ok(Json(vaults))
}
```

## Example 4: Request Tracing with Correlation ID

```rust
use axum::http::HeaderMap;

pub async fn handle_withdrawal(
    State(logger): State<Arc<StructuredLogger>>,
    headers: HeaderMap,
    Json(payload): Json<WithdrawalRequest>,
) -> Result<Json<WithdrawalResponse>, ApiError> {
    // Extract correlation ID from headers
    let correlation_id = headers
        .get("x-correlation-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");

    // All logs in this request will have the same correlation_id
    logger.log(
        StructuredLogEntry::new(LogLevel::Info, "withdrawal", "Processing withdrawal")
            .with_field("vault_id", json!(&payload.vault_id))
            .with_field("amount", json!(&payload.amount))
            .with_correlation_id(correlation_id)
    );

    // Later, trace the entire request:
    // index.by_correlation_id(correlation_id)
    
    Ok(Json(WithdrawalResponse { success: true }))
}
```

## Example 5: Module-Specific Logging

```rust
use ethos_protocol_backend::structured_logging::LogConfig;

pub async fn setup_logging() -> Arc<StructuredLogger> {
    let config = LogConfig {
        min_level: LogLevel::Debug,
        buffer_size: 100,
        // Only log these modules
        included_modules: vec![
            "vault_manager".to_string(),
            "checkin_handler".to_string(),
            "payment_processor".to_string(),
        ],
        excluded_modules: vec![],
    };
    
    StructuredLogger::with_config(config)
}
```

## Example 6: Multiple Sinks for Different Outputs

```rust
use ethos_protocol_backend::structured_logging::{StructuredLogger, FileSink, MemorySink};
use std::sync::Arc;

pub async fn setup_logging_with_sinks() -> Arc<StructuredLogger> {
    let logger = StructuredLogger::new();
    
    // Write to file for production
    logger.add_sink(Arc::new(FileSink::new("/var/log/ethos/app.log")));
    
    // Also keep in memory for quick analysis
    let mem_sink = Arc::new(MemorySink::new());
    logger.add_sink(mem_sink.clone());
    
    // Later, analyze logs from memory sink
    // let entries = mem_sink.entries();
    
    logger
}
```

## Example 7: Log Aggregation and Statistics

```rust
use ethos_protocol_backend::log_aggregation::{LogAggregator, AggregationConfig};
use std::time::Duration;

pub async fn run_log_aggregation(aggregator: Arc<LogAggregator>) {
    // Configure aggregation
    let config = AggregationConfig {
        batch_size: 50,           // Batch every 50 logs
        flush_interval_secs: 5,   // Or every 5 seconds
        max_buffer_size: 1000,
        auto_flush_enabled: true,
    };
    aggregator.set_config(config);

    // Periodically check stats
    loop {
        tokio::time::sleep(Duration::from_secs(10)).await;
        
        let stats = aggregator.stats();
        println!("Log Statistics:");
        println!("  Total entries: {}", stats.total_entries);
        println!("  Flushed: {}", stats.entries_flushed);
        println!("  Batches: {}", stats.batches_created);
        println!("  Avg batch size: {:.2}", stats.average_batch_size);
        println!("  Peak buffer: {}", stats.buffer_peak_size);
    }
}
```

## Example 8: Searching and Filtering Logs

```rust
use ethos_protocol_backend::log_search::{LogIndex, LogQuery, LogLevel};
use chrono::{Utc, Duration};

pub async fn analyze_errors(index: Arc<LogIndex>) {
    // Search for all errors in the last hour
    let one_hour_ago = Utc::now() - Duration::hours(1);
    
    let query = LogQuery::new()
        .with_min_level(LogLevel::Error)
        .with_time_range(one_hour_ago, Utc::now());
    
    let results = index.search(&query);
    
    println!("Found {} errors in the last hour", results.total);
    println!("Query took {}ms", results.query_time_ms);
    
    for entry in results.entries {
        println!("  [{}] {}: {}", entry.timestamp, entry.module, entry.message);
        for (k, v) in &entry.fields {
            println!("    {}: {}", k, v);
        }
    }
}
```

## Example 9: Aggregated Analysis

```rust
pub async fn get_system_health(index: Arc<LogIndex>) {
    let query = LogQuery::new();
    let agg = index.aggregate(&query);
    
    println!("System Health Report:");
    println!("  Total log entries: {}", agg.total_entries);
    println!("  Errors: {}", agg.error_count);
    println!("  Warnings: {}", agg.warning_count);
    println!("\nBy Level:");
    for (level, count) in &agg.by_level {
        println!("  {}: {}", level, count);
    }
    println!("\nBy Module:");
    for (module, count) in &agg.by_module {
        println!("  {}: {}", module, count);
    }
}
```

## Example 10: Finding Slow Operations

```rust
pub async fn find_slow_operations(index: Arc<LogIndex>, threshold_ms: u64) {
    let query = LogQuery::new()
        .with_field_filter("duration_ms", &format!(">{}", threshold_ms));
    
    let results = index.search(&query);
    
    println!("Found {} operations taking > {}ms:", results.entries.len(), threshold_ms);
    
    for entry in results.entries {
        if let Some(duration) = entry.fields.get("duration_ms") {
            println!("  {} - {}: {}ms", 
                entry.timestamp, 
                entry.module, 
                duration
            );
        }
    }
}
```

## Example 11: Testing with Memory Sink

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vault_creation_logging() {
        // Setup
        let mem_sink = Arc::new(MemorySink::new());
        let logger = StructuredLogger::new();
        logger.add_sink(mem_sink.clone());

        // Execute
        create_vault_test(&logger);
        logger.flush();

        // Verify
        let entries = mem_sink.entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].level, LogLevel::Debug);
        assert_eq!(entries[0].module, "vault_handler");
        assert_eq!(entries[1].message, "Vault created successfully");
    }
}
```

## Example 12: Tracing Distributed Requests

```rust
pub async fn distributed_operation(
    State(logger): State<Arc<StructuredLogger>>,
    correlation_id: String,
) {
    // Service A: Process
    logger.log(
        StructuredLogEntry::new(LogLevel::Info, "service_a", "Starting operation")
            .with_correlation_id(&correlation_id)
    );

    // Call Service B
    call_service_b(&correlation_id).await;

    // Service A: Complete
    logger.log(
        StructuredLogEntry::new(LogLevel::Info, "service_a", "Operation complete")
            .with_correlation_id(&correlation_id)
    );
}

pub async fn call_service_b(correlation_id: &str) {
    // Service B: Receive
    let logger = get_logger();
    logger.log(
        StructuredLogEntry::new(LogLevel::Info, "service_b", "Received request")
            .with_correlation_id(correlation_id)
    );

    // Service B: Process
    process_in_service_b(&logger, correlation_id).await;

    // Later: Trace entire request
    // let logs = index.by_correlation_id(correlation_id);
    // -> All logs from both services with same correlation_id
}
```

## Example 13: Configuration via Environment

```bash
# Setup environment
export LOG_LEVEL=DEBUG
export LOG_MODULES=vault_manager,checkin_handler,payment_processor
export LOG_EXCLUDE_MODULES=health_check,metrics
export LOG_BUFFER_SIZE=100
export LOG_BATCH_SIZE=50
```

```rust
pub fn load_log_config() -> LogConfig {
    let min_level = std::env::var("LOG_LEVEL")
        .ok()
        .and_then(|s| LogLevel::from_str(&s))
        .unwrap_or(LogLevel::Info);

    let modules_str = std::env::var("LOG_MODULES").unwrap_or_default();
    let included_modules: Vec<String> = modules_str
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let exclude_str = std::env::var("LOG_EXCLUDE_MODULES").unwrap_or_default();
    let excluded_modules: Vec<String> = exclude_str
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let buffer_size = std::env::var("LOG_BUFFER_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);

    LogConfig {
        min_level,
        buffer_size,
        included_modules,
        excluded_modules,
    }
}
```

## Example 14: Background Flush Task

```rust
pub async fn start_log_flush_task(logger: Arc<StructuredLogger>, aggregator: Arc<LogAggregator>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            
            // Flush both logger and aggregator
            logger.flush();
            aggregator.flush_if_needed();
            
            // Optionally process batches
            let batches = aggregator.drain_batches();
            if !batches.is_empty() {
                println!("Processed {} batches", batches.len());
            }
        }
    });
}
```

## Example 15: API Endpoints for Log Analysis

```rust
use axum::{extract::Query, Json};

// Search logs endpoint
pub async fn search_logs(
    State(index): State<Arc<LogIndex>>,
    Query(query): Query<LogQuery>,
) -> Json<SearchResults> {
    Json(index.search(&query))
}

// Get statistics endpoint
pub async fn get_log_stats(
    State(index): State<Arc<LogIndex>>,
) -> Json<AggregationResults> {
    let query = LogQuery::new();
    Json(index.aggregate(&query))
}

// Get logs by correlation ID endpoint
pub async fn trace_request(
    State(index): State<Arc<LogIndex>>,
    correlation_id: String,
) -> Json<Vec<StructuredLogEntry>> {
    Json(index.by_correlation_id(&correlation_id))
}

// Add to router:
// .route("/api/logs/search", post(search_logs))
// .route("/api/logs/stats", get(get_log_stats))
// .route("/api/logs/trace/:id", get(trace_request))
```

## Summary

These examples demonstrate:
1. Basic logging in handlers
2. Error logging with context
3. Performance monitoring
4. Request tracing with correlation IDs
5. Module-specific filtering
6. Multiple sink configurations
7. Log aggregation and statistics
8. Searching and filtering
9. Aggregated analysis
10. Finding slow operations
11. Testing with memory sink
12. Distributed request tracing
13. Configuration via environment
14. Background flush tasks
15. Log analysis API endpoints

For more information, see:
- `docs/structured-logging.md` - Comprehensive guide
- `backend/src/structured_logging.rs` - Core implementation
- `backend/src/log_aggregation.rs` - Aggregation module
- `backend/src/log_search.rs` - Search module
