//! Issue #616 — Add Vault AI Assistant
//!
//! Users need guidance on vault decisions. The assistant runs off-chain in the
//! backend because a Soroban contract cannot call an LLM API.
//!
//! Provides:
//! - An `LlmClient` abstraction with an Anthropic Messages API implementation
//! - Natural-language command parsing into a small allowlist of typed commands.
//!   The model's reply is validated before anything is returned, so unknown
//!   actions and out-of-range values are rejected instead of executed.
//! - Deterministic, rule-based recommendations over a vault summary. These do
//!   not depend on the model, so their output is stable and testable.
//! - An optional model-written explanation of the recommendations.

use std::{future::Future, pin::Pin, time::Duration};

use serde::Deserialize;

// ── Constants ─────────────────────────────────────────────────────────────────

pub const ANTHROPIC_MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Default model for the assistant. Override with `ANTHROPIC_MODEL`.
pub const DEFAULT_ASSISTANT_MODEL: &str = "claude-sonnet-5-5";
pub const LLM_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
pub const MAX_RESPONSE_TOKENS: u32 = 512;

/// Bounds for a check-in interval set through a natural-language command.
pub const MIN_CHECK_IN_DAYS: u32 = 1;
pub const MAX_CHECK_IN_DAYS: u32 = 3_650;

/// Balances at or above this amount are treated as high value for advice.
pub const HIGH_VALUE_BALANCE: i128 = 10_000_000;

const SECONDS_PER_DAY: u64 = 86_400;
const SHORT_INTERVAL_DAYS: u64 = 7;
const LONG_INTERVAL_DAYS: u64 = 365;

const SYSTEM_PROMPT: &str = "You translate a user's request about a dead-man's-switch vault \
into one JSON object and nothing else. Allowed shapes:\n\
{\"action\":\"set_check_in_days\",\"days\":<integer 1-3650>}\n\
{\"action\":\"show_recommendations\"}\n\
{\"action\":\"explain_status\"}\n\
If the request is anything else, reply {\"action\":\"unsupported\"}.";

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssistantError {
    /// The LLM provider call failed (network, HTTP status, or missing API key).
    Llm(String),
    /// The model reply was not the expected JSON.
    MalformedResponse(String),
    /// The model asked for an action outside the allowlist.
    UnsupportedCommand,
    /// The model asked for a value outside the allowed range.
    OutOfRange { min: u32, max: u32, got: u32 },
}

impl std::fmt::Display for AssistantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Llm(msg) => write!(f, "LLM request failed: {msg}"),
            Self::MalformedResponse(msg) => write!(f, "malformed model reply: {msg}"),
            Self::UnsupportedCommand => write!(f, "that request is not supported"),
            Self::OutOfRange { min, max, got } => {
                write!(f, "value {got} is outside the allowed range {min}-{max}")
            }
        }
    }
}

impl std::error::Error for AssistantError {}

// ── LLM abstraction ───────────────────────────────────────────────────────────

pub type LlmFuture<'a> = Pin<Box<dyn Future<Output = Result<String, AssistantError>> + Send + 'a>>;

/// A text-completion provider. Implementations return the model's text reply.
pub trait LlmClient: Send + Sync {
    fn complete<'a>(&'a self, system: &'a str, user: &'a str) -> LlmFuture<'a>;
}

/// Client for the Anthropic Messages API.
#[derive(Debug, Clone)]
pub struct AnthropicClient {
    api_key: String,
    model: String,
    base_url: String,
    http: reqwest::Client,
}

impl AnthropicClient {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::with_base_url(api_key, model, ANTHROPIC_MESSAGES_URL)
    }

    /// Construct with a custom endpoint, for proxies and tests.
    pub fn with_base_url(
        api_key: impl Into<String>,
        model: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(LLM_REQUEST_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            api_key: api_key.into(),
            model: model.into(),
            base_url: base_url.into(),
            http,
        }
    }

    /// Build a client from `ANTHROPIC_API_KEY` (required) and `ANTHROPIC_MODEL`
    /// (optional). Returns `None` when no key is configured, so callers can fall
    /// back to rule-based behaviour.
    pub fn from_env() -> Option<Self> {
        let key = std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())?;
        let model =
            std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| DEFAULT_ASSISTANT_MODEL.to_owned());
        Some(Self::new(key, model))
    }
}

#[derive(Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

impl LlmClient for AnthropicClient {
    fn complete<'a>(&'a self, system: &'a str, user: &'a str) -> LlmFuture<'a> {
        Box::pin(async move {
            let body = serde_json::json!({
                "model": self.model,
                "max_tokens": MAX_RESPONSE_TOKENS,
                "system": system,
                "messages": [{ "role": "user", "content": user }],
            });
            let resp = self
                .http
                .post(&self.base_url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .json(&body)
                .send()
                .await
                .map_err(|e| AssistantError::Llm(e.to_string()))?;

            let status = resp.status();
            if !status.is_success() {
                return Err(AssistantError::Llm(format!("HTTP {status}")));
            }
            let parsed: MessagesResponse = resp
                .json()
                .await
                .map_err(|e| AssistantError::MalformedResponse(e.to_string()))?;
            parsed
                .content
                .into_iter()
                .filter(|block| block.kind == "text")
                .map(|block| block.text)
                .next()
                .ok_or_else(|| AssistantError::MalformedResponse("no text block".into()))
        })
    }
}

// ── Commands ──────────────────────────────────────────────────────────────────

/// The actions the assistant is allowed to take on a user's behalf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssistantCommand {
    SetCheckInDays(u32),
    ShowRecommendations,
    ExplainStatus,
}

#[derive(Deserialize)]
struct RawCommand {
    action: String,
    #[serde(default)]
    days: Option<u32>,
}

/// Parse a model reply into a validated command. Only the first JSON object in
/// the reply is used, and every field is checked against the allowlist.
pub fn parse_command_reply(reply: &str) -> Result<AssistantCommand, AssistantError> {
    let start = reply
        .find('{')
        .ok_or_else(|| AssistantError::MalformedResponse("no JSON object".into()))?;
    let end = reply
        .rfind('}')
        .filter(|end| *end > start)
        .ok_or_else(|| AssistantError::MalformedResponse("unterminated JSON".into()))?;
    let raw: RawCommand = serde_json::from_str(&reply[start..=end])
        .map_err(|e| AssistantError::MalformedResponse(e.to_string()))?;

    match raw.action.as_str() {
        "set_check_in_days" => {
            let days = raw
                .days
                .ok_or_else(|| AssistantError::MalformedResponse("missing days".into()))?;
            if !(MIN_CHECK_IN_DAYS..=MAX_CHECK_IN_DAYS).contains(&days) {
                return Err(AssistantError::OutOfRange {
                    min: MIN_CHECK_IN_DAYS,
                    max: MAX_CHECK_IN_DAYS,
                    got: days,
                });
            }
            Ok(AssistantCommand::SetCheckInDays(days))
        }
        "show_recommendations" => Ok(AssistantCommand::ShowRecommendations),
        "explain_status" => Ok(AssistantCommand::ExplainStatus),
        _ => Err(AssistantError::UnsupportedCommand),
    }
}

/// Turn a natural-language request into a validated command using `llm`.
pub async fn parse_command(
    llm: &dyn LlmClient,
    request: &str,
) -> Result<AssistantCommand, AssistantError> {
    let reply = llm.complete(SYSTEM_PROMPT, request).await?;
    parse_command_reply(&reply)
}

// ── Recommendations ───────────────────────────────────────────────────────────

/// The facts about a vault that the advice is based on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultSummary {
    pub check_in_interval_secs: u64,
    pub beneficiary_count: u32,
    pub balance: i128,
    pub has_backup_codes: bool,
    pub governance_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recommendation {
    pub code: &'static str,
    pub severity: Severity,
    pub message: &'static str,
}

/// Rule-based recommendations for `vault`, most severe first.
pub fn recommend(vault: &VaultSummary) -> Vec<Recommendation> {
    let mut out = Vec::new();
    let interval_days = vault.check_in_interval_secs / SECONDS_PER_DAY;

    if vault.beneficiary_count == 0 {
        out.push(Recommendation {
            code: "no_beneficiary",
            severity: Severity::High,
            message: "No beneficiary is configured, so the vault has nowhere to release funds.",
        });
    }
    if !vault.has_backup_codes {
        out.push(Recommendation {
            code: "no_backup_codes",
            severity: Severity::Medium,
            message: "Generate backup codes so you can recover access if you lose your device.",
        });
    }
    if interval_days > LONG_INTERVAL_DAYS {
        out.push(Recommendation {
            code: "long_check_in_interval",
            severity: Severity::Medium,
            message: "The check-in interval is over a year. A shorter interval detects inactivity sooner.",
        });
    }
    if vault.check_in_interval_secs > 0 && interval_days < SHORT_INTERVAL_DAYS {
        out.push(Recommendation {
            code: "short_check_in_interval",
            severity: Severity::Info,
            message: "The check-in interval is under a week. Make sure you can check in reliably.",
        });
    }
    if vault.balance >= HIGH_VALUE_BALANCE && !vault.governance_enabled {
        out.push(Recommendation {
            code: "enable_governance",
            severity: Severity::Medium,
            message: "This vault holds a high balance. Consider enabling governance for shared decisions.",
        });
    }

    out.sort_by(|a, b| b.severity.cmp(&a.severity));
    out
}

/// Ask the model to explain `recommendations` in plain language. The rules
/// remain the source of truth; this only rewrites them for the user.
pub async fn explain_recommendations(
    llm: &dyn LlmClient,
    recommendations: &[Recommendation],
) -> Result<String, AssistantError> {
    if recommendations.is_empty() {
        return Ok("No issues found with this vault's configuration.".to_owned());
    }
    let list = recommendations
        .iter()
        .map(|r| format!("- {}", r.message))
        .collect::<Vec<_>>()
        .join("\n");
    llm.complete(
        "Explain the following vault recommendations to a non-expert in two or three sentences. Do not add new advice.",
        &list,
    )
    .await
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Returns canned replies in order, so each test controls the model output.
    struct ScriptedLlm {
        replies: Mutex<Vec<Result<String, AssistantError>>>,
    }

    impl ScriptedLlm {
        fn new(replies: Vec<Result<String, AssistantError>>) -> Self {
            Self {
                replies: Mutex::new(replies.into_iter().rev().collect()),
            }
        }
    }

    impl LlmClient for ScriptedLlm {
        fn complete<'a>(&'a self, _system: &'a str, _user: &'a str) -> LlmFuture<'a> {
            let next = self
                .replies
                .lock()
                .expect("lock")
                .pop()
                .expect("scripted reply");
            Box::pin(async move { next })
        }
    }

    fn healthy_vault() -> VaultSummary {
        VaultSummary {
            check_in_interval_secs: 30 * SECONDS_PER_DAY,
            beneficiary_count: 1,
            balance: 1_000,
            has_backup_codes: true,
            governance_enabled: false,
        }
    }

    // ── Command parsing accuracy ──

    #[tokio::test]
    async fn natural_language_maps_to_check_in_command() {
        let llm = ScriptedLlm::new(vec![Ok(
            r#"{"action":"set_check_in_days","days":45}"#.to_owned()
        )]);
        let cmd = parse_command(&llm, "make me check in every six weeks")
            .await
            .expect("parsed");
        assert_eq!(cmd, AssistantCommand::SetCheckInDays(45));
    }

    #[tokio::test]
    async fn reply_wrapped_in_prose_is_still_parsed() {
        let reply = "Sure! Here you go: {\"action\":\"explain_status\"} hope that helps";
        assert_eq!(
            parse_command_reply(reply).expect("parsed"),
            AssistantCommand::ExplainStatus
        );
    }

    #[test]
    fn unknown_action_is_rejected() {
        let reply = r#"{"action":"transfer_funds","amount":5}"#;
        assert_eq!(
            parse_command_reply(reply),
            Err(AssistantError::UnsupportedCommand)
        );
    }

    #[test]
    fn explicit_unsupported_reply_is_rejected() {
        assert_eq!(
            parse_command_reply(r#"{"action":"unsupported"}"#),
            Err(AssistantError::UnsupportedCommand)
        );
    }

    #[test]
    fn out_of_range_days_are_rejected() {
        assert_eq!(
            parse_command_reply(r#"{"action":"set_check_in_days","days":0}"#),
            Err(AssistantError::OutOfRange {
                min: MIN_CHECK_IN_DAYS,
                max: MAX_CHECK_IN_DAYS,
                got: 0
            })
        );
        assert!(matches!(
            parse_command_reply(r#"{"action":"set_check_in_days","days":99999}"#),
            Err(AssistantError::OutOfRange { .. })
        ));
    }

    #[test]
    fn missing_days_is_malformed() {
        assert!(matches!(
            parse_command_reply(r#"{"action":"set_check_in_days"}"#),
            Err(AssistantError::MalformedResponse(_))
        ));
    }

    #[test]
    fn non_json_reply_is_malformed() {
        assert!(matches!(
            parse_command_reply("I cannot help with that."),
            Err(AssistantError::MalformedResponse(_))
        ));
    }

    #[tokio::test]
    async fn llm_failure_is_surfaced() {
        let llm = ScriptedLlm::new(vec![Err(AssistantError::Llm("HTTP 503".into()))]);
        assert_eq!(
            parse_command(&llm, "anything").await,
            Err(AssistantError::Llm("HTTP 503".into()))
        );
    }

    // ── Recommendation accuracy ──

    #[test]
    fn healthy_vault_gets_no_recommendations() {
        assert!(recommend(&healthy_vault()).is_empty());
    }

    #[test]
    fn missing_beneficiary_is_highest_severity_and_sorted_first() {
        let vault = VaultSummary {
            beneficiary_count: 0,
            has_backup_codes: false,
            ..healthy_vault()
        };
        let recs = recommend(&vault);
        assert_eq!(recs[0].code, "no_beneficiary");
        assert_eq!(recs[0].severity, Severity::High);
        assert!(recs.iter().any(|r| r.code == "no_backup_codes"));
    }

    #[test]
    fn long_interval_is_flagged_and_short_interval_is_informational() {
        let long = VaultSummary {
            check_in_interval_secs: 400 * SECONDS_PER_DAY,
            ..healthy_vault()
        };
        assert!(recommend(&long)
            .iter()
            .any(|r| r.code == "long_check_in_interval"));

        let short = VaultSummary {
            check_in_interval_secs: 2 * SECONDS_PER_DAY,
            ..healthy_vault()
        };
        let recs = recommend(&short);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].severity, Severity::Info);
    }

    #[test]
    fn high_balance_without_governance_suggests_governance() {
        let vault = VaultSummary {
            balance: HIGH_VALUE_BALANCE,
            governance_enabled: false,
            ..healthy_vault()
        };
        assert!(recommend(&vault)
            .iter()
            .any(|r| r.code == "enable_governance"));

        let governed = VaultSummary {
            governance_enabled: true,
            ..vault
        };
        assert!(!recommend(&governed)
            .iter()
            .any(|r| r.code == "enable_governance"));
    }

    #[tokio::test]
    async fn explanation_uses_model_reply_but_skips_call_when_nothing_to_explain() {
        let llm = ScriptedLlm::new(vec![Ok("All good.".to_owned())]);
        assert_eq!(
            explain_recommendations(&llm, &[]).await.expect("ok"),
            "No issues found with this vault's configuration."
        );

        let recs = recommend(&VaultSummary {
            has_backup_codes: false,
            ..healthy_vault()
        });
        assert_eq!(
            explain_recommendations(&llm, &recs).await.expect("ok"),
            "All good."
        );
    }

    #[test]
    fn anthropic_client_reads_api_key_from_env_only() {
        // Without a key the client must not be constructed, so no request can
        // be made with an empty credential.
        std::env::remove_var("ANTHROPIC_API_KEY");
        assert!(AnthropicClient::from_env().is_none());
    }
}
