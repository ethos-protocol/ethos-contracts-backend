//! Conditional completion for batch operations.
//!
//! Issue #868: Add missing test coverage for the batch conditional-completion
//! module.  This module provides a generic [`BatchConditionalProcessor`] that
//! evaluates a caller-supplied predicate on every item in a batch and:
//!
//! - Marks items as `completed` when the condition evaluates to `true`.
//! - Marks items as `skipped` (with a reason) when the condition evaluates to
//!   `false`.
//! - Marks items as `invalid` (with a validation error) when the condition
//!   itself cannot be evaluated (malformed input).
//!
//! The batch returns partial success: valid items are always attempted even
//! when earlier items fail condition evaluation.
//!
//! # Architecture
//!
//! ```text
//! BatchConditionalProcessor::process(items, condition_fn)
//!   │
//!   ├─ condition_fn(item) → Ok(true)   → ConditionalOutcome::Completed
//!   ├─ condition_fn(item) → Ok(false)  → ConditionalOutcome::Skipped
//!   └─ condition_fn(item) → Err(e)     → ConditionalOutcome::Invalid
//!
//! BatchConditionalResult { total, completed, skipped, invalid, items }
//! ```
//!
//! # Conventions (matching existing batch module patterns)
//!
//! - Each item is keyed by a caller-supplied string identifier.
//! - Results carry per-item `ConditionalOutcome` tagged-enum payloads
//!   (serialised with `#[serde(tag = "status")]`).
//! - The aggregate result includes counters (`completed`, `skipped`,
//!   `invalid`) and a `completion_rate` in `[0.0, 1.0]`.

use serde::{Deserialize, Serialize};

// ── Per-item outcome ──────────────────────────────────────────────────────────

/// The outcome of evaluating the completion condition for a single item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ConditionalOutcome {
    /// The condition evaluated to `true`; the item is considered complete.
    Completed,
    /// The condition evaluated to `false`; the item was intentionally skipped.
    Skipped { reason: String },
    /// The condition could not be evaluated (e.g. invalid/missing fields).
    Invalid { error: String },
}

/// Result for a single item within the batch, keyed by the caller-supplied id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConditionalItemResult {
    pub key: String,
    #[serde(flatten)]
    pub outcome: ConditionalOutcome,
}

// ── Aggregate result ──────────────────────────────────────────────────────────

/// Aggregate result for a conditional-completion batch run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchConditionalResult {
    pub total: usize,
    pub completed: usize,
    pub skipped: usize,
    pub invalid: usize,
    /// Fraction of items that completed: `completed / total` in `[0.0, 1.0]`.
    /// `1.0` when `total == 0`.
    pub completion_rate: f64,
    pub items: Vec<ConditionalItemResult>,
}

// ── Processor ─────────────────────────────────────────────────────────────────

/// Processes a batch of items by evaluating `condition_fn` on each one.
///
/// # Type Parameters
///
/// - `T`: The item type.  Items are consumed by the processor.
/// - `F`: A function `Fn(&T) -> Result<bool, String>`.
///   - `Ok(true)`  → item completes.
///   - `Ok(false)` → item is skipped; `skip_reason` provides a human-readable
///     message (defaults to `"condition not met"`).
///   - `Err(msg)`  → item is invalid; `msg` is embedded in the result.
pub struct BatchConditionalProcessor;

impl BatchConditionalProcessor {
    /// Process `items`, evaluating `condition_fn` on each, and return a
    /// [`BatchConditionalResult`] with per-item outcomes plus aggregate
    /// statistics.
    ///
    /// `skip_reason_fn` is an optional callback that provides a human-readable
    /// explanation for `Ok(false)` outcomes.  Pass `None` to use the default
    /// message `"condition not met"`.
    pub fn process<T, F, R>(
        items: impl IntoIterator<Item = (String, T)>,
        condition_fn: F,
        skip_reason_fn: Option<R>,
    ) -> BatchConditionalResult
    where
        F: Fn(&T) -> Result<bool, String>,
        R: Fn(&T) -> String,
    {
        let mut results: Vec<ConditionalItemResult> = Vec::new();

        for (key, item) in items {
            let outcome = match condition_fn(&item) {
                Ok(true) => ConditionalOutcome::Completed,
                Ok(false) => {
                    let reason = skip_reason_fn
                        .as_ref()
                        .map(|f| f(&item))
                        .unwrap_or_else(|| "condition not met".to_string());
                    ConditionalOutcome::Skipped { reason }
                }
                Err(e) => ConditionalOutcome::Invalid { error: e },
            };
            results.push(ConditionalItemResult {
                key,
                outcome,
            });
        }

        let total = results.len();
        let completed = results
            .iter()
            .filter(|r| r.outcome == ConditionalOutcome::Completed)
            .count();
        let skipped = results
            .iter()
            .filter(|r| matches!(r.outcome, ConditionalOutcome::Skipped { .. }))
            .count();
        let invalid = results
            .iter()
            .filter(|r| matches!(r.outcome, ConditionalOutcome::Invalid { .. }))
            .count();
        let completion_rate = if total == 0 {
            1.0
        } else {
            completed as f64 / total as f64
        };

        BatchConditionalResult {
            total,
            completed,
            skipped,
            invalid,
            completion_rate,
            items: results,
        }
    }

    /// Convenience wrapper: no custom `skip_reason_fn`.
    pub fn process_simple<T, F>(
        items: impl IntoIterator<Item = (String, T)>,
        condition_fn: F,
    ) -> BatchConditionalResult
    where
        F: Fn(&T) -> Result<bool, String>,
    {
        Self::process(items, condition_fn, None::<fn(&T) -> String>)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn keyed(items: &[(&str, i64)]) -> Vec<(String, i64)> {
        items
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect()
    }

    // ── Condition-evaluation logic ────────────────────────────────────────────

    /// Items whose condition evaluates to `true` should be marked `Completed`.
    #[test]
    fn test_all_items_complete_when_condition_always_true() {
        let items = keyed(&[("a", 10), ("b", 20), ("c", 30)]);
        let result = BatchConditionalProcessor::process_simple(items, |_v| Ok(true));

        assert_eq!(result.total, 3);
        assert_eq!(result.completed, 3);
        assert_eq!(result.skipped, 0);
        assert_eq!(result.invalid, 0);
        assert!(
            (result.completion_rate - 1.0).abs() < f64::EPSILON,
            "completion_rate should be 1.0"
        );
        for item in &result.items {
            assert_eq!(
                item.outcome,
                ConditionalOutcome::Completed,
                "item {} should be Completed",
                item.key
            );
        }
    }

    /// Items whose condition evaluates to `false` should be marked `Skipped`.
    #[test]
    fn test_all_items_skipped_when_condition_always_false() {
        let items = keyed(&[("a", 1), ("b", 2)]);
        let result =
            BatchConditionalProcessor::process_simple(items, |_v| Ok(false));

        assert_eq!(result.completed, 0);
        assert_eq!(result.skipped, 2);
        assert_eq!(result.invalid, 0);
        assert!(result.completion_rate.abs() < f64::EPSILON);
        for item in &result.items {
            assert!(
                matches!(&item.outcome, ConditionalOutcome::Skipped { reason } if reason == "condition not met"),
                "item {} should be Skipped with default reason",
                item.key
            );
        }
    }

    /// A custom `skip_reason_fn` produces item-specific skip messages.
    #[test]
    fn test_custom_skip_reason_included_in_output() {
        let items = keyed(&[("vault_5", 5)]);
        let result = BatchConditionalProcessor::process(
            items,
            |_v| Ok(false),
            Some(|v: &i64| format!("value {} is below threshold", v)),
        );

        assert_eq!(result.skipped, 1);
        let reason = match &result.items[0].outcome {
            ConditionalOutcome::Skipped { reason } => reason.clone(),
            other => panic!("expected Skipped, got {:?}", other),
        };
        assert_eq!(reason, "value 5 is below threshold");
    }

    // ── Partial-batch success ─────────────────────────────────────────────────

    /// A mixed batch where some items complete, some are skipped, and some are
    /// invalid should have correct per-bucket counts and completion_rate.
    #[test]
    fn test_partial_batch_success_mixed_outcomes() {
        // condition: value >= 10 → complete; value < 0 → invalid; else → skip
        let items = keyed(&[
            ("ok1", 15),  // completed
            ("ok2", 10),  // completed (boundary)
            ("skip1", 5), // skipped
            ("bad1", -1), // invalid
            ("skip2", 0), // skipped
        ]);

        let result = BatchConditionalProcessor::process_simple(items, |v| {
            if *v < 0 {
                Err(format!("negative value {} is not allowed", v))
            } else {
                Ok(*v >= 10)
            }
        });

        assert_eq!(result.total, 5);
        assert_eq!(result.completed, 2, "ok1 and ok2 should complete");
        assert_eq!(result.skipped, 2, "skip1 and skip2 should be skipped");
        assert_eq!(result.invalid, 1, "bad1 should be invalid");
        assert!(
            (result.completion_rate - 2.0 / 5.0).abs() < 1e-9,
            "completion_rate should be 0.4"
        );
    }

    /// A partial failure on one item must not prevent subsequent items from
    /// being evaluated (batch continues unconditionally).
    #[test]
    fn test_invalid_item_does_not_abort_remaining_items() {
        let items = keyed(&[("bad", -99), ("good", 100)]);

        let result = BatchConditionalProcessor::process_simple(items, |v| {
            if *v < 0 {
                Err("negative".to_string())
            } else {
                Ok(true)
            }
        });

        assert_eq!(result.total, 2);
        assert_eq!(result.completed, 1, "'good' should still complete");
        assert_eq!(result.invalid, 1, "'bad' should be invalid");
    }

    // ── Invalid-condition input paths ─────────────────────────────────────────

    /// When the condition function returns `Err`, the item is marked `Invalid`
    /// and the error message is preserved in the result.
    #[test]
    fn test_invalid_condition_error_message_preserved() {
        let items = vec![("item1".to_string(), "not_a_number".to_string())];

        let result = BatchConditionalProcessor::process_simple(items, |s: &String| {
            s.parse::<i64>()
                .map(|v| v > 0)
                .map_err(|e| format!("parse error: {}", e))
        });

        assert_eq!(result.invalid, 1);
        let error = match &result.items[0].outcome {
            ConditionalOutcome::Invalid { error } => error.clone(),
            other => panic!("expected Invalid, got {:?}", other),
        };
        assert!(
            error.contains("parse error"),
            "error message should describe parse failure, got: {}",
            error
        );
    }

    /// An empty batch should return all-zero counters and `completion_rate`
    /// of 1.0 (vacuously true).
    #[test]
    fn test_empty_batch_returns_zeroed_result() {
        let result = BatchConditionalProcessor::process_simple(
            Vec::<(String, i64)>::new(),
            |_v| Ok(true),
        );

        assert_eq!(result.total, 0);
        assert_eq!(result.completed, 0);
        assert_eq!(result.skipped, 0);
        assert_eq!(result.invalid, 0);
        assert!(
            (result.completion_rate - 1.0).abs() < f64::EPSILON,
            "empty batch completion_rate should be 1.0"
        );
    }

    /// A batch with all invalid items should have `completion_rate` of 0.0.
    #[test]
    fn test_all_invalid_completion_rate_is_zero() {
        let items = keyed(&[("a", 1), ("b", 2), ("c", 3)]);
        let result = BatchConditionalProcessor::process_simple(items, |_v| {
            Err("always invalid".to_string())
        });

        assert_eq!(result.invalid, 3);
        assert!(result.completion_rate.abs() < f64::EPSILON);
    }

    // ── Key association ───────────────────────────────────────────────────────

    /// Each result item should carry the same key that was supplied by the
    /// caller.
    #[test]
    fn test_keys_are_preserved_in_results() {
        let items = keyed(&[("vault_1", 1), ("vault_2", 2), ("vault_3", 3)]);
        let result = BatchConditionalProcessor::process_simple(items, |v| Ok(*v > 0));

        let keys: Vec<&str> = result.items.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["vault_1", "vault_2", "vault_3"]);
    }

    // ── Serialisation round-trip ──────────────────────────────────────────────

    /// The result should serialise to JSON and back without loss.
    #[test]
    fn test_result_serialises_to_json() {
        let items = keyed(&[("k1", 5), ("k2", -1)]);
        let result = BatchConditionalProcessor::process_simple(items, |v| {
            if *v < 0 {
                Err("negative".to_string())
            } else {
                Ok(true)
            }
        });

        let json = serde_json::to_string(&result).expect("should serialise");
        assert!(json.contains("\"status\":\"completed\""), "completed item should be in JSON");
        assert!(json.contains("\"status\":\"invalid\""), "invalid item should be in JSON");
        assert!(json.contains("\"error\":\"negative\""), "error message should be in JSON");
    }
}
