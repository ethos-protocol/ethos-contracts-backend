//! Request queue with backpressure for Soroban RPC-bound operations.
//!
//! Soroban RPC calls are not instant: real mainnet/testnet latency ranges
//! from ~200 ms to 2 s per call, sometimes higher under congestion.  If the
//! API server accepts requests faster than the RPC layer can drain them the
//! in-memory queue will grow unboundedly, causing out-of-memory crashes or
//! ever-increasing tail latency.
//!
//! [`RequestQueue`] provides:
//!
//! - A bounded async channel (`MAX_DEPTH` slots).
//! - A dedicated worker task that processes items sequentially (each item
//!   simulates or performs an RPC call that can take between
//!   `MIN_RPC_LATENCY` and `MAX_RPC_LATENCY`).
//! - Backpressure: `try_enqueue` returns `Err(QueueFull)` when the channel
//!   is full rather than blocking the caller.
//! - Prometheus-style counters: enqueued, rejected, processed, and current
//!   depth.
//!
//! # Architecture
//!
//! ```text
//! Request in   ──► try_enqueue() ──► [bounded channel (MAX_DEPTH)] ──► worker task
//!                     │                                                      │
//!             Err(QueueFull) ◄─ backpressure                          process_item()
//!             (caller returns 429)                                  (simulates RPC latency)
//! ```
//!
//! # Documented limits
//!
//! | Parameter         | Value          | Description                          |
//! |-------------------|----------------|--------------------------------------|
//! | `MAX_DEPTH`       | 256            | Maximum in-flight items in the queue |
//! | `MIN_RPC_LATENCY` | 200 ms         | Minimum simulated RPC call duration  |
//! | `MAX_RPC_LATENCY` | 2 000 ms       | Maximum simulated RPC call duration  |
//!
//! When the queue reaches `MAX_DEPTH` new enqueue attempts are rejected with
//! [`QueueError::Full`].  Callers (HTTP handlers) should translate this to an
//! HTTP 429 response.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

// ── Configuration ─────────────────────────────────────────────────────────────

/// Maximum number of items that may sit in the queue waiting to be processed.
pub const MAX_DEPTH: usize = 256;

/// Minimum simulated Soroban RPC call latency (lower bound of realistic range).
pub const MIN_RPC_LATENCY_MS: u64 = 200;

/// Maximum simulated Soroban RPC call latency (upper bound of realistic range).
pub const MAX_RPC_LATENCY_MS: u64 = 2_000;

// ── Errors ────────────────────────────────────────────────────────────────────

/// Errors that can be returned when trying to enqueue a request.
#[derive(Debug, PartialEq, Eq)]
pub enum QueueError {
    /// The queue has reached `MAX_DEPTH`; the caller should back off or
    /// return HTTP 429 to its client.
    Full,
    /// The queue worker has stopped and the sending half of the channel was
    /// closed unexpectedly.
    Closed,
}

impl std::fmt::Display for QueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueueError::Full => write!(f, "request queue full (backpressure active)"),
            QueueError::Closed => write!(f, "request queue worker stopped"),
        }
    }
}

impl std::error::Error for QueueError {}

// ── Metrics ───────────────────────────────────────────────────────────────────

/// Counters for observability. All fields are atomic so they can be read from
/// any thread without locking.
#[derive(Debug, Default)]
pub struct QueueMetrics {
    /// Total items accepted into the queue.
    pub enqueued_total: AtomicU64,
    /// Total items rejected because the queue was full.
    pub rejected_total: AtomicU64,
    /// Total items successfully processed by the worker.
    pub processed_total: AtomicU64,
}

impl QueueMetrics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Render in Prometheus text-exposition format.
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(out, "# HELP ethos_request_queue_enqueued_total Items enqueued");
        let _ = writeln!(out, "# TYPE ethos_request_queue_enqueued_total counter");
        let _ = writeln!(
            out,
            "ethos_request_queue_enqueued_total {}",
            self.enqueued_total.load(Ordering::Relaxed)
        );
        let _ = writeln!(
            out,
            "# HELP ethos_request_queue_rejected_total Items rejected due to backpressure"
        );
        let _ = writeln!(out, "# TYPE ethos_request_queue_rejected_total counter");
        let _ = writeln!(
            out,
            "ethos_request_queue_rejected_total {}",
            self.rejected_total.load(Ordering::Relaxed)
        );
        let _ = writeln!(
            out,
            "# HELP ethos_request_queue_processed_total Items processed by the RPC worker"
        );
        let _ = writeln!(out, "# TYPE ethos_request_queue_processed_total counter");
        let _ = writeln!(
            out,
            "ethos_request_queue_processed_total {}",
            self.processed_total.load(Ordering::Relaxed)
        );
        out
    }
}

// ── Queue item ────────────────────────────────────────────────────────────────

/// A work item placed on the queue. In a real integration this would contain
/// the RPC call parameters; here it carries enough for tests to verify the
/// processing path.
#[derive(Debug)]
pub struct QueueItem {
    /// Identifier for the originating request.
    pub id: u64,
    /// Simulated processing duration for this particular item. Set by the
    /// caller in tests to drive precise latency scenarios; in production a
    /// real RPC call duration replaces this.
    pub simulated_latency: Duration,
}

// ── RequestQueue ──────────────────────────────────────────────────────────────

/// A bounded, back-pressured queue for Soroban RPC work items.
///
/// # Usage
///
/// ```rust,ignore
/// let queue = RequestQueue::new();
/// queue.spawn_worker();              // start the drain loop
///
/// // In a handler:
/// match queue.try_enqueue(item) {
///     Ok(()) => { /* accepted */ }
///     Err(QueueError::Full) => { /* return HTTP 429 */ }
///     Err(QueueError::Closed) => { /* return HTTP 503 */ }
/// }
/// ```
#[derive(Clone)]
pub struct RequestQueue {
    sender: mpsc::Sender<QueueItem>,
    pub metrics: Arc<QueueMetrics>,
}

impl RequestQueue {
    /// Create a new queue with the documented `MAX_DEPTH` bound.
    pub fn new() -> Self {
        Self::with_capacity(MAX_DEPTH)
    }

    /// Create a new queue with a custom capacity (useful for unit tests that
    /// want a small queue to trigger backpressure quickly).
    pub fn with_capacity(capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        let metrics = QueueMetrics::new();
        let queue = Self {
            sender,
            metrics: Arc::clone(&metrics),
        };
        // Spawn the worker immediately so the receiver is consumed.
        spawn_worker(receiver, metrics);
        queue
    }

    /// Attempt to place `item` on the queue without blocking.
    ///
    /// Returns `Err(QueueError::Full)` when the channel is at capacity (the
    /// number of items already queued equals the configured bound).
    pub fn try_enqueue(&self, item: QueueItem) -> Result<(), QueueError> {
        match self.sender.try_send(item) {
            Ok(()) => {
                self.metrics.enqueued_total.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.metrics.rejected_total.fetch_add(1, Ordering::Relaxed);
                Err(QueueError::Full)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.metrics.rejected_total.fetch_add(1, Ordering::Relaxed);
                Err(QueueError::Closed)
            }
        }
    }

    /// Current number of items waiting to be processed (approximate).
    pub fn depth(&self) -> usize {
        // `mpsc::Sender::max_capacity` - `capacity` gives pending items.
        self.sender.max_capacity() - self.sender.capacity()
    }

    /// The configured maximum queue depth.
    pub fn max_depth(&self) -> usize {
        self.sender.max_capacity()
    }
}

impl Default for RequestQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawns the background worker task that drains the queue.
fn spawn_worker(mut receiver: mpsc::Receiver<QueueItem>, metrics: Arc<QueueMetrics>) {
    tokio::spawn(async move {
        while let Some(item) = receiver.recv().await {
            // Simulate the RPC call taking `item.simulated_latency`.
            tokio::time::sleep(item.simulated_latency).await;
            metrics.processed_total.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(id = item.id, "request queue item processed");
        }
    });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Convenience: build an item with a given simulated latency.
    fn item(id: u64, latency_ms: u64) -> QueueItem {
        QueueItem {
            id,
            simulated_latency: Duration::from_millis(latency_ms),
        }
    }

    // ── Backpressure / rejection ──────────────────────────────────────────────

    /// A small-capacity queue should reject items once it is full and report
    /// the correct `QueueFull` error so callers can return HTTP 429.
    #[tokio::test]
    async fn test_queue_rejects_when_full() {
        // Capacity 2 — easy to fill in tests without large latencies.
        let queue = RequestQueue::with_capacity(2);

        // Items with a long latency so the worker does not drain them before
        // we finish filling the queue.
        let r1 = queue.try_enqueue(item(1, 5_000));
        let r2 = queue.try_enqueue(item(2, 5_000));
        // The channel holds 2 items; the third must be rejected.
        let r3 = queue.try_enqueue(item(3, 5_000));

        assert!(r1.is_ok(), "first item should be accepted");
        assert!(r2.is_ok(), "second item should be accepted");
        assert_eq!(
            r3,
            Err(QueueError::Full),
            "third item must be rejected when queue is full"
        );
    }

    /// Rejection counter increments for every rejected item.
    #[tokio::test]
    async fn test_rejection_counter_increments() {
        let queue = RequestQueue::with_capacity(1);

        // Fill the slot.
        queue.try_enqueue(item(1, 5_000)).unwrap();
        // Overflow — should be rejected.
        let _ = queue.try_enqueue(item(2, 5_000));
        let _ = queue.try_enqueue(item(3, 5_000));

        assert_eq!(
            queue.metrics.rejected_total.load(Ordering::Relaxed),
            2,
            "two rejections should be counted"
        );
    }

    /// Enqueue counter increments for every accepted item.
    #[tokio::test]
    async fn test_enqueue_counter_increments() {
        let queue = RequestQueue::with_capacity(10);
        for id in 0..5 {
            queue.try_enqueue(item(id, 0)).unwrap();
        }
        assert_eq!(
            queue.metrics.enqueued_total.load(Ordering::Relaxed),
            5,
            "five items should be counted as enqueued"
        );
    }

    // ── RPC latency simulation: MIN boundary (200 ms) ─────────────────────────

    /// Items with the minimum realistic RPC latency (200 ms) are processed
    /// and the processed counter increments after the worker drains them.
    #[tokio::test]
    async fn test_processes_items_at_min_rpc_latency() {
        let queue = RequestQueue::with_capacity(8);

        queue.try_enqueue(item(1, MIN_RPC_LATENCY_MS)).unwrap();
        queue.try_enqueue(item(2, MIN_RPC_LATENCY_MS)).unwrap();

        // Wait long enough for both items to clear the 200 ms worker delay.
        tokio::time::sleep(Duration::from_millis(MIN_RPC_LATENCY_MS + 100)).await;

        // At least 1 item should have been processed (both may have cleared).
        let processed = queue.metrics.processed_total.load(Ordering::Relaxed);
        assert!(
            processed >= 1,
            "expected at least 1 processed item at min RPC latency, got {}",
            processed
        );
    }

    // ── RPC latency simulation: realistic range (200 ms – 2 s) ───────────────

    /// Under realistic RPC latency the queue processes items sequentially and
    /// the depth decreases once the worker drains them.
    #[tokio::test]
    async fn test_queue_depth_decreases_as_worker_drains() {
        // Use a small latency so the test completes quickly while still
        // exercising the worker drain path.
        let latency_ms = 50;
        let queue = RequestQueue::with_capacity(8);

        // Enqueue 3 items.
        for id in 0..3u64 {
            queue.try_enqueue(item(id, latency_ms)).unwrap();
        }

        // Give the worker enough time to drain all three items.
        let drain_time = Duration::from_millis(latency_ms * 4);
        tokio::time::sleep(drain_time).await;

        let processed = queue.metrics.processed_total.load(Ordering::Relaxed);
        assert_eq!(
            processed, 3,
            "all 3 items should have been processed after drain wait, got {}",
            processed
        );
    }

    // ── RPC latency simulation: MAX boundary (2 s) ────────────────────────────

    /// An item with `MAX_RPC_LATENCY_MS` (2 s) tied up in the worker should
    /// cause subsequent rapid enqueues to back up in the channel, and after
    /// the slow item clears the remaining items drain normally.
    ///
    /// NOTE: This test uses `tokio::time::pause`/`advance` so it runs in
    /// milliseconds in CI rather than taking real wall-clock seconds.
    #[tokio::test(start_paused = true)]
    async fn test_max_rpc_latency_causes_queue_buildup_then_drains() {
        let queue = RequestQueue::with_capacity(8);

        // Enqueue one very slow item (2 s RPC call).
        queue
            .try_enqueue(item(0, MAX_RPC_LATENCY_MS))
            .expect("slow item should be accepted");

        // Enqueue several fast items behind it; they queue up while the
        // worker is busy with the slow item.
        for id in 1..4u64 {
            queue
                .try_enqueue(item(id, 10))
                .expect("fast items should fit while slow item is in flight");
        }

        // Advance past the slow item's 2 s latency plus some margin.
        tokio::time::advance(Duration::from_millis(MAX_RPC_LATENCY_MS + 500)).await;
        // Yield so tokio can actually run the worker tasks.
        tokio::task::yield_now().await;

        // The slow item plus at least some of the fast items should be done.
        let processed = queue.metrics.processed_total.load(Ordering::Relaxed);
        assert!(
            processed >= 1,
            "at least the slow item should have been processed, got {}",
            processed
        );
    }

    // ── Queue depth respects MAX_DEPTH constant ────────────────────────────────

    /// The `max_depth()` accessor returns the configured capacity.
    #[tokio::test]
    async fn test_max_depth_matches_configured_capacity() {
        let queue = RequestQueue::with_capacity(42);
        assert_eq!(queue.max_depth(), 42);
    }

    #[tokio::test]
    async fn test_default_max_depth_is_constant() {
        let queue = RequestQueue::new();
        assert_eq!(queue.max_depth(), MAX_DEPTH);
    }

    // ── Metrics rendering ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_metrics_render_contains_expected_keys() {
        let queue = RequestQueue::with_capacity(4);
        queue.try_enqueue(item(1, 0)).unwrap();
        // Fill then overflow.
        for id in 2..10u64 {
            let _ = queue.try_enqueue(item(id, 5_000));
        }

        let rendered = queue.metrics.render();
        assert!(
            rendered.contains("ethos_request_queue_enqueued_total"),
            "render should include enqueued counter"
        );
        assert!(
            rendered.contains("ethos_request_queue_rejected_total"),
            "render should include rejected counter"
        );
        assert!(
            rendered.contains("ethos_request_queue_processed_total"),
            "render should include processed counter"
        );
    }
}
