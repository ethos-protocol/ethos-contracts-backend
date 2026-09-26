use serde_json::Value;

#[derive(Debug, Clone)]
pub enum BulkOperationRequest {
    CheckIns(Vec<Value>),
    Withdrawals(Vec<Value>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkOperationStatus {
    Success,
    PartialSuccess,
    Failed,
}

#[derive(Debug, Clone)]
pub struct BulkOperationResponse {
    pub status: BulkOperationStatus,
    pub total_operations: usize,
    pub failed_operations: usize,
    pub operation_ids: Vec<String>,
    pub results: Vec<Value>,
    pub error_messages: Vec<String>,
}

pub struct BulkOperationsHandler;

impl BulkOperationsHandler {
    pub fn new() -> Self {
        Self
    }

    pub async fn process_bulk_check_ins(
        &self,
        request: &BulkOperationRequest,
    ) -> Result<BulkOperationResponse, String> {
        match request {
            BulkOperationRequest::CheckIns(items) => {
                if items.is_empty() {
                    return Err("Empty request".to_string());
                }
                if items.len() > 1000 {
                    return Err("Request exceeds maximum size of 1000".to_string());
                }

                let mut failed = 0;
                let mut operation_ids = Vec::new();
                let mut results = Vec::new();
                let mut errors = Vec::new();

                for (i, item) in items.iter().enumerate() {
                    if item.get("user_id").is_none() || item.get("timestamp").is_none() {
                        failed += 1;
                        errors.push(format!("Item {} missing required fields", i));
                    } else {
                        operation_ids.push(format!("op_{}", i));
                        results.push(item.clone());
                    }
                }

                let status = if failed == 0 {
                    BulkOperationStatus::Success
                } else if failed < items.len() {
                    BulkOperationStatus::PartialSuccess
                } else {
                    BulkOperationStatus::Failed
                };

                Ok(BulkOperationResponse {
                    status,
                    total_operations: items.len(),
                    failed_operations: failed,
                    operation_ids,
                    results,
                    error_messages: errors,
                })
            }
            _ => Err("Invalid request type".to_string()),
        }
    }

    pub async fn process_bulk_withdrawals(
        &self,
        request: &BulkOperationRequest,
    ) -> Result<BulkOperationResponse, String> {
        match request {
            BulkOperationRequest::Withdrawals(items) => {
                if items.is_empty() {
                    return Err("Empty request".to_string());
                }
                if items.len() > 1000 {
                    return Err("Request exceeds maximum size of 1000".to_string());
                }

                let mut failed = 0;
                let mut operation_ids = Vec::new();
                let mut results = Vec::new();
                let mut errors = Vec::new();

                for (i, item) in items.iter().enumerate() {
                    let user_valid = item.get("user_id").is_some();
                    let amount = item.get("amount").and_then(|v| v.as_str());
                    let amount_valid = amount.map(|a| a.parse::<f64>().ok().map(|x| x > 0.0)).flatten().unwrap_or(false);

                    if !user_valid || !amount_valid {
                        failed += 1;
                        errors.push(format!("Item {} has invalid user or amount", i));
                    } else {
                        operation_ids.push(format!("op_{}", i));
                        results.push(item.clone());
                    }
                }

                let status = if failed == 0 {
                    BulkOperationStatus::Success
                } else if failed < items.len() {
                    BulkOperationStatus::PartialSuccess
                } else {
                    BulkOperationStatus::Failed
                };

                Ok(BulkOperationResponse {
                    status,
                    total_operations: items.len(),
                    failed_operations: failed,
                    operation_ids,
                    results,
                    error_messages: errors,
                })
            }
            _ => Err("Invalid request type".to_string()),
        }
    }

    pub async fn process_with_atomic_transaction(
        &self,
        request: &BulkOperationRequest,
    ) -> Result<BulkOperationResponse, String> {
        match request {
            BulkOperationRequest::CheckIns(items) => self.process_bulk_check_ins(request).await,
            BulkOperationRequest::Withdrawals(items) => self.process_bulk_withdrawals(request).await,
        }
    }
}

impl Default for BulkOperationsHandler {
    fn default() -> Self {
        Self::new()
    }
}
