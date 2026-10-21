//! Synthetic monitoring for the vault lifecycle (#606).
//!
//! Real monitoring only reports failures that users have already hit. This
//! crate runs the critical vault flow on a schedule, step by step, and raises
//! an alert as soon as a step fails or hangs.
//!
//! * [`run_flow`] executes an ordered list of [`FlowStep`]s through a
//!   [`StepExecutor`]. It stops at the first failure, because later steps
//!   depend on earlier ones, and it bounds each step with a timeout so a hang
//!   counts as a failure.
//! * [`SyntheticMonitor`] binds a flow to an executor and an [`AlertSink`].
//! * [`GraphqlExecutor`] drives the steps that the backend's GraphQL API
//!   exposes (vault creation and check-in). Steps without an API endpoint,
//!   such as deposit and withdraw, need a contract-level executor and are not
//!   part of [`GRAPHQL_FLOW`].

use std::future::Future;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// One step of the vault lifecycle that the monitor exercises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowStep {
    CreateVault,
    Deposit,
    CheckIn,
    Withdraw,
}

impl FlowStep {
    /// Stable identifier used in reports and alerts.
    pub fn name(self) -> &'static str {
        match self {
            Self::CreateVault => "create_vault",
            Self::Deposit => "deposit",
            Self::CheckIn => "check_in",
            Self::Withdraw => "withdraw",
        }
    }
}

/// Full critical path: vault creation through withdrawal.
pub const CRITICAL_PATH: [FlowStep; 4] = [
    FlowStep::CreateVault,
    FlowStep::Deposit,
    FlowStep::CheckIn,
    FlowStep::Withdraw,
];

/// The subset of the critical path that the GraphQL API can exercise.
pub const GRAPHQL_FLOW: [FlowStep; 2] = [FlowStep::CreateVault, FlowStep::CheckIn];

/// Performs one step against a real or simulated system.
pub trait StepExecutor {
    /// Run `step`. An `Err` means the step failed, with a message for the alert.
    fn execute(&self, step: FlowStep) -> impl Future<Output = Result<(), String>> + Send;
}

/// Receives an alert for each failed flow run.
pub trait AlertSink {
    fn alert(&self, alert: &SyntheticAlert);
}

/// Raised when a flow run fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntheticAlert {
    pub failed_step: FlowStep,
    pub error: String,
    /// Steps that completed successfully before the failure.
    pub completed: Vec<FlowStep>,
}

/// Outcome of one step within a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepResult {
    pub step: FlowStep,
    pub latency: Duration,
    pub error: Option<String>,
}

/// Result of one flow run. Steps after the first failure are not executed and
/// do not appear in `steps`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowReport {
    pub steps: Vec<StepResult>,
    pub healthy: bool,
}

/// Run `steps` in order, stopping at the first failure. Each step is bounded
/// by `step_timeout`, and a timeout counts as a failure. A failed run triggers
/// exactly one alert on `sink`.
pub async fn run_flow<E, A>(
    executor: &E,
    sink: &A,
    steps: &[FlowStep],
    step_timeout: Duration,
) -> FlowReport
where
    E: StepExecutor,
    A: AlertSink,
{
    let mut results = Vec::with_capacity(steps.len());
    let mut completed = Vec::new();

    for &step in steps {
        let started = Instant::now();
        let outcome = match tokio::time::timeout(step_timeout, executor.execute(step)).await {
            Ok(result) => result,
            Err(_) => Err(format!("timed out after {}ms", step_timeout.as_millis())),
        };
        let latency = started.elapsed();

        match outcome {
            Ok(()) => {
                completed.push(step);
                results.push(StepResult {
                    step,
                    latency,
                    error: None,
                });
            }
            Err(error) => {
                results.push(StepResult {
                    step,
                    latency,
                    error: Some(error.clone()),
                });
                sink.alert(&SyntheticAlert {
                    failed_step: step,
                    error,
                    completed,
                });
                return FlowReport {
                    steps: results,
                    healthy: false,
                };
            }
        }
    }

    FlowReport {
        steps: results,
        healthy: true,
    }
}

/// Binds a flow to an executor, an alert sink and a per-step timeout.
pub struct SyntheticMonitor<E, A> {
    executor: E,
    sink: A,
    steps: Vec<FlowStep>,
    step_timeout: Duration,
}

impl<E: StepExecutor, A: AlertSink> SyntheticMonitor<E, A> {
    pub fn new(executor: E, sink: A, steps: Vec<FlowStep>, step_timeout: Duration) -> Self {
        Self {
            executor,
            sink,
            steps,
            step_timeout,
        }
    }

    /// Run the flow once and return its report.
    pub async fn run_once(&self) -> FlowReport {
        run_flow(&self.executor, &self.sink, &self.steps, self.step_timeout).await
    }
}

/// Identifiers returned by the API and reused by later steps.
#[derive(Debug, Default)]
pub struct VaultContext {
    pub owner: String,
    pub beneficiary: String,
    pub check_in_interval: i64,
    vault_id: Mutex<Option<String>>,
}

impl VaultContext {
    pub fn new(owner: String, beneficiary: String, check_in_interval: i64) -> Self {
        Self {
            owner,
            beneficiary,
            check_in_interval,
            vault_id: Mutex::new(None),
        }
    }

    /// Id of the vault created by this run, if creation has succeeded.
    pub fn vault_id(&self) -> Option<String> {
        self.vault_id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn set_vault_id(&self, id: String) {
        *self
            .vault_id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(id);
    }
}

/// Builds the GraphQL request for a step. Returns `None` for steps that the
/// API does not expose.
pub fn graphql_request(step: FlowStep, ctx: &VaultContext) -> Option<Value> {
    match step {
        FlowStep::CreateVault => Some(json!({
            "query": "mutation CreateVault($input: CreateVaultInput!) { createVault(input: $input) { id } }",
            "variables": {
                "input": {
                    "owner": ctx.owner,
                    "beneficiary": ctx.beneficiary,
                    "checkInInterval": ctx.check_in_interval,
                }
            }
        })),
        FlowStep::CheckIn => {
            let vault_id = ctx.vault_id()?;
            Some(json!({
                "query": "mutation CheckIn($vaultId: String!) { checkIn(vaultId: $vaultId) { id } }",
                "variables": { "vaultId": vault_id }
            }))
        }
        FlowStep::Deposit | FlowStep::Withdraw => None,
    }
}

/// Pulls the first GraphQL error message out of a response body, if any.
pub fn graphql_error(body: &Value) -> Option<String> {
    body.get("errors")?
        .as_array()?
        .first()?
        .get("message")?
        .as_str()
        .map(str::to_string)
}

/// Runs the GraphQL-backed steps against the backend's `/graphql` endpoint.
pub struct GraphqlExecutor {
    client: reqwest::Client,
    endpoint: String,
    ctx: VaultContext,
}

impl GraphqlExecutor {
    /// `base_url` is the backend root, for example `https://api.example.com`.
    pub fn new(base_url: &str, ctx: VaultContext) -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoint: format!("{}/graphql", base_url.trim_end_matches('/')),
            ctx,
        }
    }

    pub fn context(&self) -> &VaultContext {
        &self.ctx
    }
}

impl StepExecutor for GraphqlExecutor {
    fn execute(&self, step: FlowStep) -> impl Future<Output = Result<(), String>> + Send {
        let request = graphql_request(step, &self.ctx);
        let client = self.client.clone();
        let endpoint = self.endpoint.clone();
        let ctx_vault = &self.ctx;
        async move {
            let body =
                request.ok_or_else(|| format!("step `{}` has no GraphQL endpoint", step.name()))?;
            let response = client
                .post(&endpoint)
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("request failed: {e}"))?;
            let status = response.status();
            let payload: Value = response
                .json()
                .await
                .map_err(|e| format!("invalid response body: {e}"))?;
            if !status.is_success() {
                return Err(format!("HTTP {status}"));
            }
            if let Some(message) = graphql_error(&payload) {
                return Err(message);
            }
            if step == FlowStep::CreateVault {
                let id = payload
                    .pointer("/data/createVault/id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "createVault response missing id".to_string())?;
                ctx_vault.set_vault_id(id.to_string());
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Executor whose outcome per step is scripted by the test.
    struct Scripted {
        fail_at: Option<FlowStep>,
        error: &'static str,
        delay: Duration,
        calls: Mutex<Vec<FlowStep>>,
    }

    impl Scripted {
        fn ok() -> Self {
            Self {
                fail_at: None,
                error: "",
                delay: Duration::ZERO,
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl StepExecutor for Scripted {
        fn execute(&self, step: FlowStep) -> impl Future<Output = Result<(), String>> + Send {
            self.calls.lock().unwrap().push(step);
            let fail = self.fail_at == Some(step);
            let error = self.error.to_string();
            let delay = self.delay;
            async move {
                tokio::time::sleep(delay).await;
                if fail {
                    Err(error)
                } else {
                    Ok(())
                }
            }
        }
    }

    #[derive(Default)]
    struct CountingSink {
        alerts: Mutex<Vec<SyntheticAlert>>,
        count: AtomicUsize,
    }

    impl AlertSink for &CountingSink {
        fn alert(&self, alert: &SyntheticAlert) {
            self.count.fetch_add(1, Ordering::SeqCst);
            self.alerts.lock().unwrap().push(alert.clone());
        }
    }

    const TIMEOUT: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn healthy_run_executes_every_step_without_alerting() {
        let exec = Scripted::ok();
        let sink = CountingSink::default();
        let report = run_flow(&exec, &&sink, &CRITICAL_PATH, TIMEOUT).await;

        assert!(report.healthy);
        assert_eq!(report.steps.len(), CRITICAL_PATH.len());
        assert!(report.steps.iter().all(|s| s.error.is_none()));
        assert_eq!(sink.count.load(Ordering::SeqCst), 0);
        assert_eq!(*exec.calls.lock().unwrap(), CRITICAL_PATH.to_vec());
    }

    #[tokio::test]
    async fn failure_halts_the_flow_and_alerts_once() {
        let exec = Scripted {
            fail_at: Some(FlowStep::Deposit),
            error: "insufficient balance",
            ..Scripted::ok()
        };
        let sink = CountingSink::default();
        let report = run_flow(&exec, &&sink, &CRITICAL_PATH, TIMEOUT).await;

        assert!(!report.healthy);
        // create_vault succeeds, deposit fails, and check_in and withdraw never run.
        assert_eq!(report.steps.len(), 2);
        assert_eq!(report.steps[1].step, FlowStep::Deposit);
        assert_eq!(
            report.steps[1].error.as_deref(),
            Some("insufficient balance")
        );
        assert_eq!(
            *exec.calls.lock().unwrap(),
            vec![FlowStep::CreateVault, FlowStep::Deposit]
        );

        assert_eq!(sink.count.load(Ordering::SeqCst), 1);
        let alerts = sink.alerts.lock().unwrap();
        assert_eq!(alerts[0].failed_step, FlowStep::Deposit);
        assert_eq!(alerts[0].completed, vec![FlowStep::CreateVault]);
    }

    #[tokio::test]
    async fn a_hung_step_counts_as_a_timeout_failure() {
        let exec = Scripted {
            fail_at: None,
            delay: Duration::from_millis(500),
            ..Scripted::ok()
        };
        let sink = CountingSink::default();
        let report = run_flow(
            &exec,
            &&sink,
            &[FlowStep::CreateVault],
            Duration::from_millis(20),
        )
        .await;

        assert!(!report.healthy);
        let error = report.steps[0].error.as_deref().unwrap();
        assert!(error.starts_with("timed out after"), "{error}");
        assert_eq!(sink.count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn monitor_run_once_uses_configured_steps() {
        let sink = CountingSink::default();
        let monitor = SyntheticMonitor::new(Scripted::ok(), &sink, GRAPHQL_FLOW.to_vec(), TIMEOUT);
        let report = monitor.run_once().await;
        assert!(report.healthy);
        assert_eq!(report.steps.len(), GRAPHQL_FLOW.len());
    }

    #[test]
    fn create_vault_request_uses_schema_field_names() {
        let ctx = VaultContext::new("GOWNER".into(), "GBEN".into(), 86_400);
        let body = graphql_request(FlowStep::CreateVault, &ctx).unwrap();
        let input = &body["variables"]["input"];
        assert_eq!(input["owner"], "GOWNER");
        assert_eq!(input["beneficiary"], "GBEN");
        assert_eq!(input["checkInInterval"], 86_400);
    }

    #[test]
    fn check_in_request_needs_a_created_vault() {
        let ctx = VaultContext::new("o".into(), "b".into(), 60);
        assert!(graphql_request(FlowStep::CheckIn, &ctx).is_none());
        ctx.set_vault_id("vault-1".into());
        let body = graphql_request(FlowStep::CheckIn, &ctx).unwrap();
        assert_eq!(body["variables"]["vaultId"], "vault-1");
    }

    #[test]
    fn deposit_and_withdraw_have_no_graphql_request() {
        let ctx = VaultContext::new("o".into(), "b".into(), 60);
        assert!(graphql_request(FlowStep::Deposit, &ctx).is_none());
        assert!(graphql_request(FlowStep::Withdraw, &ctx).is_none());
    }

    #[test]
    fn graphql_error_extracts_first_message() {
        let body = json!({"errors": [{"message": "vault not found"}]});
        assert_eq!(graphql_error(&body).as_deref(), Some("vault not found"));
        assert!(graphql_error(&json!({"data": {}})).is_none());
    }
}
