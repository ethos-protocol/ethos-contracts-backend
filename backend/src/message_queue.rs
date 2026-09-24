// #150 — Message Queue Integration (Kafka/RabbitMQ-style)
//
// Provides an in-process, broker-agnostic message queue that mirrors the
// semantics of Kafka/RabbitMQ without requiring an external broker during
// development or test.  A `BrokerAdapter` trait allows swapping in a real
// Kafka or AMQP client at the integration boundary.
//
// Features:
//  - Typed `QueueMessage` envelope with topic, partition key, headers.
//  - Publisher API (`MessagePublisher`) with at-least-once delivery tracking.
//  - Consumer API (`MessageConsumer`) with offset-based acknowledgement.
//  - Partitioning strategies: RoundRobin, HashKey, Manual.
//  - Dead-letter queue (DLQ) for messages that exceed max retries.
//  - Poison-message handling: a per-message delivery-attempt counter, automatic
//    routing to the topic DLQ (and, when configured, to `dlq::DlqStore`) once
//    the attempt limit is hit, and a `PoisonEvent` log for observability.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// Message header carrying the ingress correlation id (issue #349), matching
/// `error_context::REQUEST_ID_HEADER`.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

// ── Message envelope ──────────────────────────────────────────────────────────

/// A single message in the queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueMessage {
    /// Unique message identifier.
    pub id: String,
    /// Logical topic / exchange name (e.g. `"vault.events"`).
    pub topic: String,
    /// Which partition this message was routed to.
    pub partition: u32,
    /// Optional routing / correlation key used by hash-based partitioning.
    pub key: Option<String>,
    /// Arbitrary application headers (e.g. `"schema_version"`, `"source"`).
    pub headers: HashMap<String, String>,
    /// Serialized message payload.
    pub payload: serde_json::Value,
    /// Wall-clock time the message was published.
    pub published_at: DateTime<Utc>,
    /// Number of delivery attempts so far (0 = not yet attempted).
    pub delivery_attempts: u32,
}

impl QueueMessage {
    pub fn new(
        topic: impl Into<String>,
        partition: u32,
        key: Option<String>,
        headers: HashMap<String, String>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            topic: topic.into(),
            partition,
            key,
            headers,
            payload,
            published_at: Utc::now(),
            delivery_attempts: 0,
        }
    }

    /// The ingress correlation id propagated with this message, if any
    /// (issue #349).
    pub fn request_id(&self) -> Option<&str> {
        self.headers.get(REQUEST_ID_HEADER).map(String::as_str)
    }
}

// ── Partitioning strategies ───────────────────────────────────────────────────

/// How incoming messages are assigned to partitions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PartitionStrategy {
    /// Distribute messages evenly across all partitions in round-robin order.
    RoundRobin,
    /// Hash the message key to a consistent partition (sticky routing).
    /// Falls back to RoundRobin when no key is present.
    HashKey,
    /// Caller supplies the partition number explicitly.
    Manual,
}

impl Default for PartitionStrategy {
    fn default() -> Self {
        Self::RoundRobin
    }
}

/// Compute a target partition for a message.
pub fn assign_partition(
    strategy: &PartitionStrategy,
    key: Option<&str>,
    partition_count: u32,
    round_robin_counter: u32,
    manual_partition: Option<u32>,
) -> u32 {
    assert!(partition_count > 0, "partition_count must be > 0");
    match strategy {
        PartitionStrategy::RoundRobin => round_robin_counter % partition_count,
        PartitionStrategy::HashKey => match key {
            Some(k) => {
                // Simple djb2 hash — stable, dependency-free.
                let hash: u32 = k.bytes().fold(5381u32, |acc, b| {
                    acc.wrapping_mul(33).wrapping_add(b as u32)
                });
                hash % partition_count
            }
            None => round_robin_counter % partition_count,
        },
        PartitionStrategy::Manual => manual_partition.unwrap_or(0).min(partition_count - 1),
    }
}

// ── Topic configuration ───────────────────────────────────────────────────────

/// Per-topic configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicConfig {
    pub name: String,
    pub partition_count: u32,
    pub strategy: PartitionStrategy,
    /// Max delivery attempts before a message is moved to the DLQ.
    pub max_retries: u32,
}

impl TopicConfig {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            partition_count: 4,
            strategy: PartitionStrategy::default(),
            max_retries: 3,
        }
    }

    pub fn with_partitions(mut self, n: u32) -> Self {
        self.partition_count = n;
        self
    }

    pub fn with_strategy(mut self, s: PartitionStrategy) -> Self {
        self.strategy = s;
        self
    }

    pub fn with_max_retries(mut self, r: u32) -> Self {
        self.max_retries = r;
        self
    }
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum MessageQueueError {
    #[error("topic not found: {0}")]
    TopicNotFound(String),
    #[error("partition {0} does not exist on topic {1}")]
    InvalidPartition(u32, String),
    #[error("internal lock was poisoned")]
    LockPoisoned,
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("consumer group {0} is not registered")]
    UnknownConsumerGroup(String),
}

// ── Internal partition storage ────────────────────────────────────────────────

#[derive(Debug, Default)]
struct Partition {
    messages: Vec<QueueMessage>,
}

#[derive(Debug, Default)]
struct TopicStore {
    config: Option<TopicConfig>,
    partitions: Vec<Partition>,
    /// Per-(consumer_group, partition) committed offset.
    offsets: HashMap<(String, u32), usize>,
    round_robin_counter: u32,
    /// Dead-letter queue for this topic.
    dlq: Vec<QueueMessage>,
    /// Audit log of every message that was poison-routed to the DLQ.
    poison_events: Vec<PoisonEvent>,
}

// ── Poison-message handling ───────────────────────────────────────────────────

/// Outcome of recording a failed delivery attempt against a stored message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// The message is still within its delivery budget and will be retried.
    Retry { attempts: u32 },
    /// The message exceeded `max_retries` and was routed to the DLQ.
    Poisoned { attempts: u32 },
}

/// Emitted (and logged) whenever a message is automatically routed to the DLQ
/// because it exhausted its delivery-attempt budget.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PoisonEvent {
    pub message_id: String,
    pub topic: String,
    pub partition: u32,
    /// Delivery attempts recorded at the point of poison-routing.
    pub attempts: u32,
    /// The last processing error reported by the consumer.
    pub error: String,
    pub routed_at: DateTime<Utc>,
}

/// Convert a queue message into a `dlq::DlqEntry` for the shared DLQ store.
pub fn poison_to_dlq_entry(msg: &QueueMessage, error: &str, attempts: u32) -> crate::dlq::DlqEntry {
    let now = Utc::now();
    crate::dlq::DlqEntry {
        id: Uuid::new_v4().to_string(),
        source: format!("message_queue:{}:{}", msg.topic, msg.partition),
        target: None,
        payload: msg.payload.clone(),
        error: error.to_string(),
        attempts,
        status: crate::dlq::DlqStatus::Pending,
        created_at: now,
        last_attempt_at: now,
    }
}

// ── Message broker ────────────────────────────────────────────────────────────

/// The central in-memory broker — holds all topics, partitions and offsets.
#[derive(Debug, Clone, Default)]
pub struct MessageBroker {
    topics: Arc<Mutex<HashMap<String, TopicStore>>>,
    /// Optional shared dead-letter store (`dlq.rs`). When set, poison messages
    /// are mirrored into it automatically in addition to the per-topic DLQ.
    dlq_store: Option<crate::dlq::DlqStore>,
}

impl MessageBroker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach a shared `dlq::DlqStore` so poison messages are also captured in
    /// the operator-facing dead-letter queue.
    pub fn with_dlq_store(mut self, store: crate::dlq::DlqStore) -> Self {
        self.dlq_store = Some(store);
        self
    }

    /// Register a topic with the given configuration.  Idempotent.
    pub fn create_topic(&self, config: TopicConfig) -> Result<(), MessageQueueError> {
        let mut topics = self
            .topics
            .lock()
            .map_err(|_| MessageQueueError::LockPoisoned)?;
        topics.entry(config.name.clone()).or_insert_with(|| {
            let n = config.partition_count as usize;
            TopicStore {
                config: Some(config),
                partitions: (0..n).map(|_| Partition::default()).collect(),
                ..Default::default()
            }
        });
        Ok(())
    }

    /// Publish a message.  Partition is assigned according to the topic strategy.
    pub fn publish(
        &self,
        topic: &str,
        key: Option<String>,
        headers: HashMap<String, String>,
        payload: serde_json::Value,
        manual_partition: Option<u32>,
    ) -> Result<String, MessageQueueError> {
        let mut topics = self
            .topics
            .lock()
            .map_err(|_| MessageQueueError::LockPoisoned)?;
        let store = topics
            .get_mut(topic)
            .ok_or_else(|| MessageQueueError::TopicNotFound(topic.to_string()))?;

        let cfg = store.config.as_ref().unwrap();
        let partition = assign_partition(
            &cfg.strategy,
            key.as_deref(),
            cfg.partition_count,
            store.round_robin_counter,
            manual_partition,
        );
        store.round_robin_counter = store.round_robin_counter.wrapping_add(1);

        let msg = QueueMessage::new(topic, partition, key, headers, payload);
        let id = msg.id.clone();
        store.partitions[partition as usize].messages.push(msg);
        Ok(id)
    }

    /// Poll up to `max_messages` unacknowledged messages for a consumer group
    /// from the given topic and partition.
    pub fn poll(
        &self,
        topic: &str,
        partition: u32,
        consumer_group: &str,
        max_messages: usize,
    ) -> Result<Vec<QueueMessage>, MessageQueueError> {
        let topics = self
            .topics
            .lock()
            .map_err(|_| MessageQueueError::LockPoisoned)?;
        let store = topics
            .get(topic)
            .ok_or_else(|| MessageQueueError::TopicNotFound(topic.to_string()))?;

        if partition as usize >= store.partitions.len() {
            return Err(MessageQueueError::InvalidPartition(
                partition,
                topic.to_string(),
            ));
        }

        let offset = store
            .offsets
            .get(&(consumer_group.to_string(), partition))
            .copied()
            .unwrap_or(0);

        let msgs: Vec<QueueMessage> = store.partitions[partition as usize]
            .messages
            .iter()
            .skip(offset)
            .take(max_messages)
            .cloned()
            .collect();

        Ok(msgs)
    }

    /// Commit the offset for a consumer group on a partition (acknowledge
    /// processing up to `offset`).
    pub fn commit_offset(
        &self,
        topic: &str,
        partition: u32,
        consumer_group: &str,
        offset: usize,
    ) -> Result<(), MessageQueueError> {
        let mut topics = self
            .topics
            .lock()
            .map_err(|_| MessageQueueError::LockPoisoned)?;
        let store = topics
            .get_mut(topic)
            .ok_or_else(|| MessageQueueError::TopicNotFound(topic.to_string()))?;

        store
            .offsets
            .insert((consumer_group.to_string(), partition), offset);
        Ok(())
    }

    /// Move a message to the dead-letter queue for its topic.
    pub fn send_to_dlq(&self, topic: &str, mut msg: QueueMessage) -> Result<(), MessageQueueError> {
        let mut topics = self
            .topics
            .lock()
            .map_err(|_| MessageQueueError::LockPoisoned)?;
        let store = topics
            .get_mut(topic)
            .ok_or_else(|| MessageQueueError::TopicNotFound(topic.to_string()))?;
        msg.headers
            .insert("dlq_reason".into(), "max_retries_exceeded".into());
        store.dlq.push(msg);
        Ok(())
    }

    /// Return all messages currently in the DLQ for a topic.
    pub fn list_dlq(&self, topic: &str) -> Result<Vec<QueueMessage>, MessageQueueError> {
        let topics = self
            .topics
            .lock()
            .map_err(|_| MessageQueueError::LockPoisoned)?;
        let store = topics
            .get(topic)
            .ok_or_else(|| MessageQueueError::TopicNotFound(topic.to_string()))?;
        Ok(store.dlq.clone())
    }

    /// Record a failed processing attempt for a stored message.
    ///
    /// Increments the message's persistent `delivery_attempts` counter. Once the
    /// counter reaches the topic's `max_retries`, the message is routed to the
    /// per-topic DLQ (and to the shared `dlq::DlqStore` when one is attached), a
    /// [`PoisonEvent`] is logged and recorded, and [`DeliveryOutcome::Poisoned`]
    /// is returned. Otherwise [`DeliveryOutcome::Retry`] is returned so the
    /// caller knows the message should be redelivered.
    pub fn record_failed_delivery(
        &self,
        topic: &str,
        partition: u32,
        message_id: &str,
        error: &str,
    ) -> Result<DeliveryOutcome, MessageQueueError> {
        let mut topics = self
            .topics
            .lock()
            .map_err(|_| MessageQueueError::LockPoisoned)?;
        let store = topics
            .get_mut(topic)
            .ok_or_else(|| MessageQueueError::TopicNotFound(topic.to_string()))?;

        if partition as usize >= store.partitions.len() {
            return Err(MessageQueueError::InvalidPartition(
                partition,
                topic.to_string(),
            ));
        }

        let max_retries = store.config.as_ref().map(|c| c.max_retries).unwrap_or(3);

        let idx = store.partitions[partition as usize]
            .messages
            .iter()
            .position(|m| m.id == message_id)
            .ok_or_else(|| {
                MessageQueueError::TopicNotFound(format!(
                    "{topic}: message {message_id} not found in partition {partition}"
                ))
            })?;

        let attempts = {
            let m = &mut store.partitions[partition as usize].messages[idx];
            m.delivery_attempts += 1;
            m.delivery_attempts
        };

        if attempts < max_retries {
            return Ok(DeliveryOutcome::Retry { attempts });
        }

        // ── Poison: route to the DLQ ─────────────────────────────────────────
        let mut msg = store.partitions[partition as usize].messages[idx].clone();
        msg.headers
            .insert("dlq_reason".into(), "max_delivery_attempts_exceeded".into());
        msg.headers.insert("dlq_error".into(), error.to_string());
        store.dlq.push(msg.clone());

        let event = PoisonEvent {
            message_id: message_id.to_string(),
            topic: topic.to_string(),
            partition,
            attempts,
            error: error.to_string(),
            routed_at: Utc::now(),
        };
        tracing::warn!(
            message_id = %event.message_id,
            topic = %event.topic,
            partition = event.partition,
            attempts = event.attempts,
            error = %event.error,
            "poison message routed to dead-letter queue"
        );
        store.poison_events.push(event);

        if let Some(ref dlq_store) = self.dlq_store {
            let entry = poison_to_dlq_entry(&msg, error, attempts);
            dlq_store
                .lock()
                .map_err(|_| MessageQueueError::LockPoisoned)?
                .insert(entry.id.clone(), entry);
        }

        Ok(DeliveryOutcome::Poisoned { attempts })
    }

    /// Return the poison-routing audit log for a topic.
    pub fn list_poison_events(&self, topic: &str) -> Result<Vec<PoisonEvent>, MessageQueueError> {
        let topics = self
            .topics
            .lock()
            .map_err(|_| MessageQueueError::LockPoisoned)?;
        let store = topics
            .get(topic)
            .ok_or_else(|| MessageQueueError::TopicNotFound(topic.to_string()))?;
        Ok(store.poison_events.clone())
    }

    /// Return the current committed offset for a consumer group / partition.
    pub fn current_offset(
        &self,
        topic: &str,
        partition: u32,
        consumer_group: &str,
    ) -> Result<usize, MessageQueueError> {
        let topics = self
            .topics
            .lock()
            .map_err(|_| MessageQueueError::LockPoisoned)?;
        let store = topics
            .get(topic)
            .ok_or_else(|| MessageQueueError::TopicNotFound(topic.to_string()))?;
        Ok(store
            .offsets
            .get(&(consumer_group.to_string(), partition))
            .copied()
            .unwrap_or(0))
    }
}

// ── Publisher ─────────────────────────────────────────────────────────────────

/// High-level publisher.  Wraps the broker and provides a convenient API for
/// emitting vault domain events.
#[derive(Clone)]
pub struct MessagePublisher {
    broker: Arc<MessageBroker>,
    default_topic: String,
}

impl MessagePublisher {
    pub fn new(broker: Arc<MessageBroker>, default_topic: impl Into<String>) -> Self {
        Self {
            broker,
            default_topic: default_topic.into(),
        }
    }

    /// Publish a vault event with an optional routing key.
    pub fn publish_vault_event(
        &self,
        vault_id: &str,
        event_type: &str,
        payload: serde_json::Value,
    ) -> Result<String, MessageQueueError> {
        let mut headers = HashMap::new();
        headers.insert("event_type".into(), event_type.into());
        headers.insert("source".into(), "ethos-backend".into());
        self.broker.publish(
            &self.default_topic,
            Some(vault_id.to_string()),
            headers,
            payload,
            None,
        )
    }

    /// Publish a vault event tagged with the originating request's correlation
    /// id (issue #349).
    ///
    /// The id is written to the `x-request-id` message header so a consumer
    /// processing this event asynchronously can join its logs back to the HTTP
    /// request that produced it. Consumers should call
    /// [`QueueMessage::request_id`] to read it.
    pub fn publish_vault_event_correlated(
        &self,
        request_id: &str,
        vault_id: &str,
        event_type: &str,
        payload: serde_json::Value,
    ) -> Result<String, MessageQueueError> {
        let mut headers = HashMap::new();
        headers.insert("event_type".into(), event_type.into());
        headers.insert("source".into(), "ethos-backend".into());
        headers.insert(REQUEST_ID_HEADER.into(), request_id.to_string());
        self.broker.publish(
            &self.default_topic,
            Some(vault_id.to_string()),
            headers,
            payload,
            None,
        )
    }

    /// Publish to an explicit topic with a manual partition.
    pub fn publish_to(
        &self,
        topic: &str,
        key: Option<String>,
        headers: HashMap<String, String>,
        payload: serde_json::Value,
        partition: Option<u32>,
    ) -> Result<String, MessageQueueError> {
        self.broker.publish(topic, key, headers, payload, partition)
    }
}

// ── Consumer ──────────────────────────────────────────────────────────────────

/// High-level consumer.  Tracks its own consumer-group name and the partitions
/// it has been assigned.
#[derive(Clone)]
pub struct MessageConsumer {
    broker: Arc<MessageBroker>,
    consumer_group: String,
    assigned_partitions: Vec<(String, u32)>, // (topic, partition)
}

impl MessageConsumer {
    pub fn new(
        broker: Arc<MessageBroker>,
        consumer_group: impl Into<String>,
        assigned_partitions: Vec<(String, u32)>,
    ) -> Self {
        Self {
            broker,
            consumer_group: consumer_group.into(),
            assigned_partitions,
        }
    }

    /// Poll all assigned partitions and return pending messages (up to
    /// `max_per_partition` per partition).
    pub fn poll_all(
        &self,
        max_per_partition: usize,
    ) -> Result<Vec<QueueMessage>, MessageQueueError> {
        let mut all = Vec::new();
        for (topic, partition) in &self.assigned_partitions {
            let msgs =
                self.broker
                    .poll(topic, *partition, &self.consumer_group, max_per_partition)?;
            all.extend(msgs);
        }
        Ok(all)
    }

    /// Acknowledge all messages up to `offset` on the given topic/partition.
    pub fn ack(&self, topic: &str, partition: u32, offset: usize) -> Result<(), MessageQueueError> {
        self.broker
            .commit_offset(topic, partition, &self.consumer_group, offset)
    }

    /// Process messages with a handler, auto-acking on success and routing a
    /// message to the DLQ once its persistent delivery-attempt counter reaches
    /// the topic's `max_retries` (see [`MessageBroker::record_failed_delivery`]).
    ///
    /// `_max_retries` is retained for call-site compatibility; the authoritative
    /// limit is the topic configuration.
    pub fn process<F>(
        &self,
        topic: &str,
        partition: u32,
        max_messages: usize,
        _max_retries: u32,
        mut handler: F,
    ) -> Result<ProcessSummary, MessageQueueError>
    where
        F: FnMut(&QueueMessage) -> Result<(), String>,
    {
        let msgs = self
            .broker
            .poll(topic, partition, &self.consumer_group, max_messages)?;

        let mut processed = 0usize;
        let mut failed = 0usize;
        let mut dlq_count = 0usize;
        let start_offset = self
            .broker
            .current_offset(topic, partition, &self.consumer_group)?;
        let mut last_good_offset = start_offset;

        for (i, msg) in msgs.iter().enumerate() {
            match handler(msg) {
                Ok(()) => {
                    last_good_offset = start_offset + i + 1;
                    processed += 1;
                }
                Err(e) => {
                    failed += 1;
                    match self
                        .broker
                        .record_failed_delivery(topic, partition, &msg.id, &e)?
                    {
                        DeliveryOutcome::Poisoned { .. } => {
                            // Skip past the poison message so it is not
                            // redelivered forever.
                            dlq_count += 1;
                            last_good_offset = start_offset + i + 1;
                        }
                        DeliveryOutcome::Retry { .. } => {
                            // Stop processing; message will be retried next poll.
                            break;
                        }
                    }
                }
            }
        }

        if last_good_offset > start_offset {
            self.broker
                .commit_offset(topic, partition, &self.consumer_group, last_good_offset)?;
        }

        Ok(ProcessSummary {
            processed,
            failed,
            dlq_count,
        })
    }
}

/// Summary returned by `MessageConsumer::process`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ProcessSummary {
    pub processed: usize,
    pub failed: usize,
    pub dlq_count: usize,
}

// ── Shared state ──────────────────────────────────────────────────────────────

/// All message-queue state bundled for injection into `AppState`.
#[derive(Clone)]
pub struct MessageQueueState {
    pub broker: Arc<MessageBroker>,
    pub publisher: MessagePublisher,
}

impl MessageQueueState {
    /// Create state with a default `"vault.events"` topic (4 partitions,
    /// hash-key routing).
    pub fn new() -> Result<Self, MessageQueueError> {
        let broker = Arc::new(MessageBroker::new());
        broker.create_topic(
            TopicConfig::new("vault.events")
                .with_partitions(4)
                .with_strategy(PartitionStrategy::HashKey)
                .with_max_retries(3),
        )?;
        let publisher = MessagePublisher::new(Arc::clone(&broker), "vault.events");
        Ok(Self { broker, publisher })
    }

    /// Build a consumer for a given group assigned to all partitions of a topic.
    pub fn consumer(
        &self,
        consumer_group: &str,
        topic: &str,
        partition_count: u32,
    ) -> MessageConsumer {
        let partitions = (0..partition_count)
            .map(|p| (topic.to_string(), p))
            .collect();
        MessageConsumer::new(Arc::clone(&self.broker), consumer_group, partitions)
    }
}

impl Default for MessageQueueState {
    fn default() -> Self {
        Self::new().expect("default MessageQueueState creation failed")
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_broker() -> Arc<MessageBroker> {
        let broker = Arc::new(MessageBroker::new());
        broker
            .create_topic(
                TopicConfig::new("test.topic")
                    .with_partitions(4)
                    .with_strategy(PartitionStrategy::HashKey),
            )
            .unwrap();
        broker
    }

    #[test]
    fn publish_and_poll_basic() {
        let broker = setup_broker();
        let id = broker
            .publish(
                "test.topic",
                Some("vault-1".into()),
                HashMap::new(),
                serde_json::json!({"action": "check_in"}),
                None,
            )
            .unwrap();
        assert!(!id.is_empty());

        // Find which partition it landed on
        let mut found = false;
        for p in 0..4u32 {
            let msgs = broker.poll("test.topic", p, "group-a", 10).unwrap();
            if !msgs.is_empty() {
                assert_eq!(msgs[0].payload["action"], "check_in");
                found = true;
                break;
            }
        }
        assert!(found, "message should appear in exactly one partition");
    }

    #[test]
    fn hash_key_is_stable() {
        let broker = setup_broker();
        for _ in 0..5 {
            broker
                .publish(
                    "test.topic",
                    Some("vault-stable".into()),
                    HashMap::new(),
                    serde_json::json!({}),
                    None,
                )
                .unwrap();
        }
        // All 5 messages must land in the same partition
        let counts: Vec<usize> = (0..4u32)
            .map(|p| broker.poll("test.topic", p, "grp", 100).unwrap().len())
            .collect();
        let non_zero: Vec<_> = counts.iter().filter(|&&c| c > 0).collect();
        assert_eq!(non_zero.len(), 1, "hash-key routing must be sticky");
        assert_eq!(*non_zero[0], 5);
    }

    #[test]
    fn round_robin_distributes_across_partitions() {
        let broker = Arc::new(MessageBroker::new());
        broker
            .create_topic(
                TopicConfig::new("rr.topic")
                    .with_partitions(4)
                    .with_strategy(PartitionStrategy::RoundRobin),
            )
            .unwrap();
        for _ in 0..8 {
            broker
                .publish(
                    "rr.topic",
                    None,
                    HashMap::new(),
                    serde_json::json!({}),
                    None,
                )
                .unwrap();
        }
        let counts: Vec<usize> = (0..4u32)
            .map(|p| broker.poll("rr.topic", p, "g", 100).unwrap().len())
            .collect();
        // Each partition should have 2 messages
        assert!(counts.iter().all(|&c| c == 2));
    }

    #[test]
    fn commit_offset_advances_consumer_position() {
        let broker = setup_broker();
        for i in 0..3 {
            broker
                .publish(
                    "test.topic",
                    Some("k".into()),
                    HashMap::new(),
                    serde_json::json!({"i": i}),
                    Some(0),
                )
                .unwrap();
        }
        // Poll all 3
        let msgs = broker.poll("test.topic", 0, "grp", 10).unwrap();
        assert_eq!(msgs.len(), 3);
        // Ack first 2
        broker.commit_offset("test.topic", 0, "grp", 2).unwrap();
        // Poll again — should only see message 3
        let msgs2 = broker.poll("test.topic", 0, "grp", 10).unwrap();
        assert_eq!(msgs2.len(), 1);
    }

    #[test]
    fn dlq_receives_messages_over_max_retries() {
        let broker = Arc::new(MessageBroker::new());
        broker
            .create_topic(TopicConfig::new("dlq.topic").with_partitions(1))
            .unwrap();
        let mut msg = QueueMessage::new(
            "dlq.topic",
            0,
            None,
            HashMap::new(),
            serde_json::json!({"x": 1}),
        );
        msg.delivery_attempts = 5; // already exceeded retries
        broker.send_to_dlq("dlq.topic", msg).unwrap();
        let dlq = broker.list_dlq("dlq.topic").unwrap();
        assert_eq!(dlq.len(), 1);
        assert_eq!(dlq[0].headers["dlq_reason"], "max_retries_exceeded");
    }

    #[test]
    fn manual_partition_strategy() {
        let p = assign_partition(&PartitionStrategy::Manual, None, 4, 0, Some(3));
        assert_eq!(p, 3);
        // Clamp to max partition
        let p2 = assign_partition(&PartitionStrategy::Manual, None, 4, 0, Some(99));
        assert_eq!(p2, 3);
    }

    #[test]
    fn publisher_sets_event_type_header() {
        let broker = Arc::new(MessageBroker::new());
        broker
            .create_topic(
                TopicConfig::new("vault.events")
                    .with_partitions(4)
                    .with_strategy(PartitionStrategy::HashKey),
            )
            .unwrap();
        let publisher = MessagePublisher::new(Arc::clone(&broker), "vault.events");
        publisher
            .publish_vault_event("vault-x", "check_in", serde_json::json!({}))
            .unwrap();

        let mut found_header = false;
        for p in 0..4u32 {
            let msgs = broker.poll("vault.events", p, "test", 10).unwrap();
            for m in &msgs {
                assert_eq!(
                    m.headers.get("event_type").map(|s| s.as_str()),
                    Some("check_in")
                );
                found_header = true;
            }
        }
        assert!(found_header);
    }

    #[test]
    fn delivery_attempts_counter_increments_per_failure() {
        let broker = Arc::new(MessageBroker::new());
        broker
            .create_topic(
                TopicConfig::new("poison.topic")
                    .with_partitions(1)
                    .with_max_retries(3),
            )
            .unwrap();
        let id = broker
            .publish(
                "poison.topic",
                None,
                HashMap::new(),
                serde_json::json!({}),
                Some(0),
            )
            .unwrap();

        assert_eq!(
            broker
                .record_failed_delivery("poison.topic", 0, &id, "boom")
                .unwrap(),
            DeliveryOutcome::Retry { attempts: 1 }
        );
        assert_eq!(
            broker
                .record_failed_delivery("poison.topic", 0, &id, "boom")
                .unwrap(),
            DeliveryOutcome::Retry { attempts: 2 }
        );
        assert_eq!(
            broker
                .record_failed_delivery("poison.topic", 0, &id, "boom")
                .unwrap(),
            DeliveryOutcome::Poisoned { attempts: 3 }
        );

        let dlq = broker.list_dlq("poison.topic").unwrap();
        assert_eq!(dlq.len(), 1);
        assert_eq!(
            dlq[0].headers["dlq_reason"],
            "max_delivery_attempts_exceeded"
        );
        assert_eq!(dlq[0].headers["dlq_error"], "boom");

        let events = broker.list_poison_events("poison.topic").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].attempts, 3);
        assert_eq!(events[0].message_id, id);
    }

    #[test]
    fn consumer_process_auto_routes_poison_message_to_dlq() {
        let broker = Arc::new(MessageBroker::new());
        broker
            .create_topic(
                TopicConfig::new("proc.poison")
                    .with_partitions(1)
                    .with_max_retries(2),
            )
            .unwrap();
        broker
            .publish(
                "proc.poison",
                None,
                HashMap::new(),
                serde_json::json!({"n": 1}),
                Some(0),
            )
            .unwrap();
        broker
            .publish(
                "proc.poison",
                None,
                HashMap::new(),
                serde_json::json!({"n": 2}),
                Some(0),
            )
            .unwrap();

        let consumer =
            MessageConsumer::new(Arc::clone(&broker), "grp", vec![("proc.poison".into(), 0)]);

        // Message 1 always fails. First pass: attempt 1 -> retry, processing halts.
        let s1 = consumer
            .process("proc.poison", 0, 10, 2, |m| {
                if m.payload["n"] == 1 {
                    Err("always fails".into())
                } else {
                    Ok(())
                }
            })
            .unwrap();
        assert_eq!(s1.dlq_count, 0);
        assert_eq!(broker.list_dlq("proc.poison").unwrap().len(), 0);

        // Second pass: attempt 2 == max_retries -> poison-routed, offset advances
        // past it so message 2 is then processed.
        let s2 = consumer
            .process("proc.poison", 0, 10, 2, |m| {
                if m.payload["n"] == 1 {
                    Err("always fails".into())
                } else {
                    Ok(())
                }
            })
            .unwrap();
        assert_eq!(s2.dlq_count, 1);
        assert_eq!(s2.processed, 1);

        assert_eq!(broker.list_dlq("proc.poison").unwrap().len(), 1);
        assert_eq!(broker.list_poison_events("proc.poison").unwrap().len(), 1);
        // Nothing left to redeliver.
        assert_eq!(broker.poll("proc.poison", 0, "grp", 10).unwrap().len(), 0);
    }

    #[test]
    fn poison_message_is_mirrored_into_shared_dlq_store() {
        let dlq_store = crate::dlq::create_dlq_store();
        let broker = Arc::new(MessageBroker::new().with_dlq_store(Arc::clone(&dlq_store)));
        broker
            .create_topic(
                TopicConfig::new("mirror.topic")
                    .with_partitions(1)
                    .with_max_retries(1),
            )
            .unwrap();
        let id = broker
            .publish(
                "mirror.topic",
                None,
                HashMap::new(),
                serde_json::json!({"k": "v"}),
                Some(0),
            )
            .unwrap();

        let outcome = broker
            .record_failed_delivery("mirror.topic", 0, &id, "downstream 500")
            .unwrap();
        assert_eq!(outcome, DeliveryOutcome::Poisoned { attempts: 1 });

        let store = dlq_store.lock().unwrap();
        assert_eq!(store.len(), 1);
        let entry = store.values().next().unwrap();
        assert_eq!(entry.error, "downstream 500");
        assert_eq!(entry.attempts, 1);
        assert_eq!(entry.payload, serde_json::json!({"k": "v"}));
        assert!(entry.source.starts_with("message_queue:mirror.topic:"));
    }

    #[test]
    fn request_id_propagates_through_queue_message_headers() {
        let broker = Arc::new(MessageBroker::new());
        broker
            .create_topic(
                TopicConfig::new("vault.events")
                    .with_partitions(4)
                    .with_strategy(PartitionStrategy::HashKey),
            )
            .unwrap();
        let publisher = MessagePublisher::new(Arc::clone(&broker), "vault.events");
        publisher
            .publish_vault_event_correlated(
                "req-trace-42",
                "vault-9",
                "check_in",
                serde_json::json!({}),
            )
            .unwrap();

        let mut seen = None;
        for p in 0..4u32 {
            for m in broker.poll("vault.events", p, "c", 10).unwrap() {
                seen = Some(m.request_id().unwrap().to_string());
            }
        }
        assert_eq!(seen.as_deref(), Some("req-trace-42"));
    }

    #[test]
    fn consumer_process_auto_acks_successful_messages() {
        let broker = Arc::new(MessageBroker::new());
        broker
            .create_topic(TopicConfig::new("proc.topic").with_partitions(1))
            .unwrap();
        for i in 0..3u32 {
            broker
                .publish(
                    "proc.topic",
                    None,
                    HashMap::new(),
                    serde_json::json!({"i": i}),
                    Some(0),
                )
                .unwrap();
        }
        let consumer =
            MessageConsumer::new(Arc::clone(&broker), "grp", vec![("proc.topic".into(), 0)]);
        let summary = consumer
            .process("proc.topic", 0, 10, 3, |_| Ok(()))
            .unwrap();
        assert_eq!(summary.processed, 3);
        assert_eq!(summary.failed, 0);
        // No more messages after ack
        let remaining = broker.poll("proc.topic", 0, "grp", 10).unwrap();
        assert_eq!(remaining.len(), 0);
    }
}
