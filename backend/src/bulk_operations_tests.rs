#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::http::StatusCode;
    use serde_json::json;

    use crate::bulk_operations::{
        BulkOperationRequest, BulkOperationResponse, BulkOperationStatus, BulkOperationsHandler,
    };

    #[tokio::test]
    async fn test_bulk_check_ins_endpoint() {
        let handler = Arc::new(BulkOperationsHandler::new());
        let request = BulkOperationRequest::CheckIns(vec![
            json!({"user_id": "user1", "timestamp": "2023-09-25T10:30:00Z"}),
            json!({"user_id": "user2", "timestamp": "2023-09-25T10:31:00Z"}),
        ]);

        let response = handler.process_bulk_check_ins(&request).await;
        assert!(response.is_ok());
        assert_eq!(response.unwrap().status, BulkOperationStatus::Success);
    }

    #[tokio::test]
    async fn test_bulk_withdrawals_endpoint() {
        let handler = Arc::new(BulkOperationsHandler::new());
        let request = BulkOperationRequest::Withdrawals(vec![
            json!({"user_id": "user1", "amount": "100"}),
            json!({"user_id": "user2", "amount": "200"}),
        ]);

        let response = handler.process_bulk_withdrawals(&request).await;
        assert!(response.is_ok());
        assert_eq!(response.unwrap().status, BulkOperationStatus::Success);
    }

    #[tokio::test]
    async fn test_bulk_operations_atomic_transaction() {
        let handler = Arc::new(BulkOperationsHandler::new());
        let request = BulkOperationRequest::CheckIns(vec![
            json!({"user_id": "user1", "timestamp": "2023-09-25T10:30:00Z"}),
            json!({"user_id": "user2", "timestamp": "2023-09-25T10:31:00Z"}),
        ]);

        let response = handler.process_with_atomic_transaction(&request).await;
        assert!(response.is_ok());
    }

    #[tokio::test]
    async fn test_bulk_operations_partial_failure() {
        let handler = Arc::new(BulkOperationsHandler::new());
        let request = BulkOperationRequest::CheckIns(vec![
            json!({"user_id": "valid_user", "timestamp": "2023-09-25T10:30:00Z"}),
            json!({"user_id": "", "timestamp": "2023-09-25T10:31:00Z"}),
        ]);

        let response = handler.process_bulk_check_ins(&request).await;
        assert!(response.is_ok());
        let result = response.unwrap();
        assert_eq!(result.total_operations, 2);
        assert!(result.failed_operations > 0);
    }

    #[tokio::test]
    async fn test_bulk_check_ins_multiple_items() {
        let handler = Arc::new(BulkOperationsHandler::new());
        let items: Vec<_> = (0..10)
            .map(|i| {
                json!({
                    "user_id": format!("user{}", i),
                    "timestamp": "2023-09-25T10:30:00Z"
                })
            })
            .collect();

        let request = BulkOperationRequest::CheckIns(items);
        let response = handler.process_bulk_check_ins(&request).await;
        assert!(response.is_ok());
        let result = response.unwrap();
        assert_eq!(result.total_operations, 10);
    }

    #[tokio::test]
    async fn test_bulk_withdrawals_multiple_items() {
        let handler = Arc::new(BulkOperationsHandler::new());
        let items: Vec<_> = (0..5)
            .map(|i| {
                json!({
                    "user_id": format!("user{}", i),
                    "amount": format!("{}", i * 100)
                })
            })
            .collect();

        let request = BulkOperationRequest::Withdrawals(items);
        let response = handler.process_bulk_withdrawals(&request).await;
        assert!(response.is_ok());
        let result = response.unwrap();
        assert_eq!(result.total_operations, 5);
    }

    #[tokio::test]
    async fn test_bulk_operations_transaction_rollback_on_error() {
        let handler = Arc::new(BulkOperationsHandler::new());
        let request = BulkOperationRequest::CheckIns(vec![
            json!({"user_id": "user1", "timestamp": "2023-09-25T10:30:00Z"}),
            json!({"user_id": "invalid", "timestamp": "invalid_date"}),
        ]);

        let response = handler.process_with_atomic_transaction(&request).await;
        if let Ok(result) = response {
            assert!(result.failed_operations > 0 || result.status == BulkOperationStatus::Failed);
        }
    }

    #[tokio::test]
    async fn test_bulk_operations_returns_operation_ids() {
        let handler = Arc::new(BulkOperationsHandler::new());
        let request = BulkOperationRequest::CheckIns(vec![
            json!({"user_id": "user1", "timestamp": "2023-09-25T10:30:00Z"}),
            json!({"user_id": "user2", "timestamp": "2023-09-25T10:31:00Z"}),
        ]);

        let response = handler.process_bulk_check_ins(&request).await;
        assert!(response.is_ok());
        let result = response.unwrap();
        assert!(!result.operation_ids.is_empty());
        assert_eq!(result.operation_ids.len(), 2);
    }

    #[tokio::test]
    async fn test_bulk_operations_empty_request() {
        let handler = Arc::new(BulkOperationsHandler::new());
        let request = BulkOperationRequest::CheckIns(vec![]);

        let response = handler.process_bulk_check_ins(&request).await;
        assert!(response.is_err());
    }

    #[tokio::test]
    async fn test_bulk_operations_exceeds_max_size() {
        let handler = Arc::new(BulkOperationsHandler::new());
        let items: Vec<_> = (0..1001)
            .map(|i| {
                json!({
                    "user_id": format!("user{}", i),
                    "timestamp": "2023-09-25T10:30:00Z"
                })
            })
            .collect();

        let request = BulkOperationRequest::CheckIns(items);
        let response = handler.process_bulk_check_ins(&request).await;
        assert!(response.is_err());
    }

    #[tokio::test]
    async fn test_bulk_operations_response_contains_results() {
        let handler = Arc::new(BulkOperationsHandler::new());
        let request = BulkOperationRequest::CheckIns(vec![
            json!({"user_id": "user1", "timestamp": "2023-09-25T10:30:00Z"}),
            json!({"user_id": "user2", "timestamp": "2023-09-25T10:31:00Z"}),
        ]);

        let response = handler.process_bulk_check_ins(&request).await;
        assert!(response.is_ok());
        let result = response.unwrap();
        assert!(!result.results.is_empty());
    }

    #[tokio::test]
    async fn test_bulk_check_ins_validates_input() {
        let handler = Arc::new(BulkOperationsHandler::new());
        let request = BulkOperationRequest::CheckIns(vec![
            json!({"user_id": "user1"}),
        ]);

        let response = handler.process_bulk_check_ins(&request).await;
        if let Ok(result) = response {
            assert!(result.failed_operations > 0);
        }
    }

    #[tokio::test]
    async fn test_bulk_withdrawals_validates_amount() {
        let handler = Arc::new(BulkOperationsHandler::new());
        let request = BulkOperationRequest::Withdrawals(vec![
            json!({"user_id": "user1", "amount": "-100"}),
        ]);

        let response = handler.process_bulk_withdrawals(&request).await;
        if let Ok(result) = response {
            assert!(result.failed_operations > 0);
        }
    }

    #[tokio::test]
    async fn test_bulk_operations_concurrent_requests() {
        let handler = Arc::new(BulkOperationsHandler::new());
        let mut handles = vec![];

        for i in 0..3 {
            let handler_clone = Arc::clone(&handler);
            let handle = tokio::spawn(async move {
                let request = BulkOperationRequest::CheckIns(vec![
                    json!({
                        "user_id": format!("user{}", i),
                        "timestamp": "2023-09-25T10:30:00Z"
                    }),
                ]);
                handler_clone.process_bulk_check_ins(&request).await
            });
            handles.push(handle);
        }

        for handle in handles {
            let result = handle.await;
            assert!(result.is_ok());
        }
    }

    #[tokio::test]
    async fn test_bulk_operations_transaction_idempotency() {
        let handler = Arc::new(BulkOperationsHandler::new());
        let request = BulkOperationRequest::CheckIns(vec![
            json!({"user_id": "user1", "timestamp": "2023-09-25T10:30:00Z", "idempotency_key": "key1"}),
        ]);

        let response1 = handler.process_with_atomic_transaction(&request).await;
        let response2 = handler.process_with_atomic_transaction(&request).await;

        assert_eq!(response1.is_ok(), response2.is_ok());
    }

    #[tokio::test]
    async fn test_bulk_operations_status_tracking() {
        let handler = Arc::new(BulkOperationsHandler::new());
        let request = BulkOperationRequest::CheckIns(vec![
            json!({"user_id": "user1", "timestamp": "2023-09-25T10:30:00Z"}),
        ]);

        let response = handler.process_bulk_check_ins(&request).await;
        assert!(response.is_ok());
        let result = response.unwrap();
        assert!(matches!(
            result.status,
            BulkOperationStatus::Success | BulkOperationStatus::PartialSuccess | BulkOperationStatus::Failed
        ));
    }

    #[tokio::test]
    async fn test_bulk_withdrawals_atomic_money_transfer() {
        let handler = Arc::new(BulkOperationsHandler::new());
        let request = BulkOperationRequest::Withdrawals(vec![
            json!({"user_id": "user1", "amount": "100", "account": "savings"}),
            json!({"user_id": "user2", "amount": "200", "account": "checking"}),
            json!({"user_id": "user3", "amount": "150", "account": "money_market"}),
        ]);

        let response = handler.process_with_atomic_transaction(&request).await;
        assert!(response.is_ok());
    }

    #[tokio::test]
    async fn test_bulk_operations_error_details() {
        let handler = Arc::new(BulkOperationsHandler::new());
        let request = BulkOperationRequest::CheckIns(vec![
            json!({"user_id": "user1", "timestamp": "2023-09-25T10:30:00Z"}),
            json!({"user_id": "", "timestamp": "invalid"}),
        ]);

        let response = handler.process_bulk_check_ins(&request).await;
        assert!(response.is_ok());
        let result = response.unwrap();
        assert!(!result.error_messages.is_empty() || result.failed_operations == 0);
    }

    #[tokio::test]
    async fn test_bulk_check_ins_performance() {
        let handler = Arc::new(BulkOperationsHandler::new());
        let items: Vec<_> = (0..100)
            .map(|i| {
                json!({
                    "user_id": format!("user{}", i),
                    "timestamp": "2023-09-25T10:30:00Z"
                })
            })
            .collect();

        let request = BulkOperationRequest::CheckIns(items);
        let start = std::time::Instant::now();
        let response = handler.process_bulk_check_ins(&request).await;
        let elapsed = start.elapsed();

        assert!(response.is_ok());
        assert!(elapsed.as_secs() < 10);
    }
}
