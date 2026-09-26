//! AML (Anti-Money Laundering) screening — Issue #547.
//!
//! Screens addresses against sanctions data before funds move to them:
//!
//! - **Chainalysis** sanctions screening API
//!   (`GET {CHAINALYSIS_API_URL}/{address}` with an `X-API-Key` header). Any
//!   returned `identifications` entry marks the address as sanctioned.
//! - **Manual flag list** maintained by compliance staff through the admin
//!   API, for addresses flagged by other intelligence sources.
//!
//! Results from the provider are cached for `AML_CACHE_TTL_SECS` to keep
//! latency and API usage bounded. Provider failures **fail closed** (the
//! address is treated as non-compliant) unless `AML_FAIL_OPEN=true`.
//!
//! The on-chain counterpart lives in `contracts/ttl_vault/src/aml.rs`: the
//! backend mirrors positive hits to the contract via `flag_aml_address`
//! (as the configured AML reporter) so the contract blocks transfers to
//! flagged beneficiaries even if the backend is bypassed.
//!
//! Configuration:
//!
//! | Variable              | Default                                         |
//! |-----------------------|-------------------------------------------------|
//! | `CHAINALYSIS_API_KEY` | unset — provider screening disabled             |
//! | `CHAINALYSIS_API_URL` | `https://public.chainalysis.com/api/v1/address` |
//! | `AML_CACHE_TTL_SECS`  | `3600`                                          |
//! | `AML_FAIL_OPEN`       | `false`                                         |

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::audit::authorize_admin;

const DEFAULT_CHAINALYSIS_URL: &str = "https://public.chainalysis.com/api/v1/address";
const DEFAULT_CACHE_TTL_SECS: u64 = 3_600;
const REQUEST_TIMEOUT_SECS: u64 = 10;
const MAX_ADDRESS_LEN: usize = 128;

#[derive(Debug, Clone)]
pub struct AmlConfig {
    pub api_key: Option<String>,
    pub api_url: String,
    pub cache_ttl: Duration,
    pub fail_open: bool,
}

impl AmlConfig {
    pub fn from_env() -> Self {
        Self {
            api_key: std::env::var("CHAINALYSIS_API_KEY")
                .ok()
                .filter(|k| !k.trim().is_empty()),
            api_url: std::env::var("CHAINALYSIS_API_URL")
                .ok()
                .filter(|u| !u.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_CHAINALYSIS_URL.to_string()),
            cache_ttl: Duration::from_secs(
                std::env::var("AML_CACHE_TTL_SECS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(DEFAULT_CACHE_TTL_SECS),
            ),
            fail_open: std::env::var("AML_FAIL_OPEN")
                .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
                .unwrap_or(false),
        }
    }
}

/// One sanctions identification returned by Chainalysis.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SanctionIdentification {
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChainalysisResponse {
    #[serde(default)]
    identifications: Vec<SanctionIdentification>,
}

/// Where a screening verdict came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AmlSource {
    /// Address is on the manual flag list.
    ManualFlag,
    /// Fresh result from the Chainalysis API.
    Chainalysis,
    /// Cached Chainalysis result.
    Cache,
    /// Provider screening is not configured; only the manual list applied.
    ManualListOnly,
    /// Provider call failed; verdict follows the fail-open/closed policy.
    ProviderUnavailable,
}

#[derive(Debug, Clone, Serialize)]
pub struct AmlScreeningResult {
    pub address: String,
    pub compliant: bool,
    pub source: AmlSource,
    pub identifications: Vec<SanctionIdentification>,
    pub reason: Option<String>,
    pub checked_at: DateTime<Utc>,
}

/// A manually flagged address.
#[derive(Debug, Clone, Serialize)]
pub struct AmlFlag {
    pub address: String,
    pub reason: String,
    pub flagged_by: String,
    pub flagged_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct CachedScreening {
    identifications: Vec<SanctionIdentification>,
    fetched_at: std::time::Instant,
}

#[derive(Debug, thiserror::Error)]
pub enum AmlError {
    #[error("invalid address")]
    InvalidAddress,
    #[error("AML provider request failed: {0}")]
    Provider(String),
}

pub struct AmlScreener {
    config: AmlConfig,
    client: reqwest::Client,
    cache: RwLock<HashMap<String, CachedScreening>>,
    flags: RwLock<HashMap<String, AmlFlag>>,
}

fn normalize_address(address: &str) -> Result<String, AmlError> {
    let trimmed = address.trim();
    if trimmed.is_empty()
        || trimmed.len() > MAX_ADDRESS_LEN
        || !trimmed.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return Err(AmlError::InvalidAddress);
    }
    Ok(trimmed.to_string())
}

impl AmlScreener {
    pub fn new(config: AmlConfig) -> Arc<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .unwrap_or_default();
        Arc::new(Self {
            config,
            client,
            cache: RwLock::new(HashMap::new()),
            flags: RwLock::new(HashMap::new()),
        })
    }

    pub fn from_env() -> Arc<Self> {
        Self::new(AmlConfig::from_env())
    }

    /// Returns `true` when `address` is clear to receive funds.
    pub async fn check_aml_compliance(&self, address: &str) -> bool {
        match self.screen(address).await {
            Ok(result) => result.compliant,
            Err(_) => false,
        }
    }

    /// Full screening verdict for `address`.
    pub async fn screen(&self, address: &str) -> Result<AmlScreeningResult, AmlError> {
        let address = normalize_address(address)?;
        let now = Utc::now();

        if let Some(flag) = self
            .flags
            .read()
            .expect("aml flag lock poisoned")
            .get(&address)
        {
            return Ok(AmlScreeningResult {
                address,
                compliant: false,
                source: AmlSource::ManualFlag,
                identifications: Vec::new(),
                reason: Some(flag.reason.clone()),
                checked_at: now,
            });
        }

        let Some(api_key) = self.config.api_key.as_deref() else {
            return Ok(AmlScreeningResult {
                address,
                compliant: true,
                source: AmlSource::ManualListOnly,
                identifications: Vec::new(),
                reason: None,
                checked_at: now,
            });
        };

        let cached = self
            .cache
            .read()
            .expect("aml cache lock poisoned")
            .get(&address)
            .filter(|c| c.fetched_at.elapsed() < self.config.cache_ttl)
            .map(|c| c.identifications.clone());
        let (identifications, source) = match cached {
            Some(ids) => (ids, AmlSource::Cache),
            None => match self.query_chainalysis(api_key, &address).await {
                Ok(ids) => {
                    self.cache.write().expect("aml cache lock poisoned").insert(
                        address.clone(),
                        CachedScreening {
                            identifications: ids.clone(),
                            fetched_at: std::time::Instant::now(),
                        },
                    );
                    (ids, AmlSource::Chainalysis)
                }
                Err(e) => {
                    tracing::error!(
                        %address,
                        error = %e,
                        fail_open = self.config.fail_open,
                        "AML screening unavailable"
                    );
                    return Ok(AmlScreeningResult {
                        address,
                        compliant: self.config.fail_open,
                        source: AmlSource::ProviderUnavailable,
                        identifications: Vec::new(),
                        reason: Some(e.to_string()),
                        checked_at: now,
                    });
                }
            },
        };

        let compliant = identifications.is_empty();
        if !compliant {
            tracing::warn!(
                %address,
                hits = identifications.len(),
                "AML screening flagged address"
            );
        }
        Ok(AmlScreeningResult {
            address,
            compliant,
            source,
            reason: identifications
                .first()
                .and_then(|i| i.name.clone().or_else(|| i.category.clone())),
            identifications,
            checked_at: now,
        })
    }

    async fn query_chainalysis(
        &self,
        api_key: &str,
        address: &str,
    ) -> Result<Vec<SanctionIdentification>, AmlError> {
        let url = format!("{}/{}", self.config.api_url.trim_end_matches('/'), address);
        let resp = self
            .client
            .get(&url)
            .header("X-API-Key", api_key)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| AmlError::Provider(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(AmlError::Provider(format!("HTTP {}", resp.status())));
        }
        let body: ChainalysisResponse = resp
            .json()
            .await
            .map_err(|e| AmlError::Provider(e.to_string()))?;
        Ok(body.identifications)
    }

    pub fn flag(
        &self,
        address: &str,
        reason: String,
        flagged_by: String,
    ) -> Result<AmlFlag, AmlError> {
        let address = normalize_address(address)?;
        let flag = AmlFlag {
            address: address.clone(),
            reason,
            flagged_by,
            flagged_at: Utc::now(),
        };
        tracing::warn!(
            %address,
            flagged_by = %flag.flagged_by,
            "address manually flagged for AML"
        );
        self.flags
            .write()
            .expect("aml flag lock poisoned")
            .insert(address, flag.clone());
        Ok(flag)
    }

    pub fn unflag(&self, address: &str) -> Result<bool, AmlError> {
        let address = normalize_address(address)?;
        let removed = self
            .flags
            .write()
            .expect("aml flag lock poisoned")
            .remove(&address)
            .is_some();
        if removed {
            tracing::info!(%address, "AML flag removed");
        }
        Ok(removed)
    }

    pub fn flags(&self) -> Vec<AmlFlag> {
        self.flags
            .read()
            .expect("aml flag lock poisoned")
            .values()
            .cloned()
            .collect()
    }
}

// ── HTTP handlers ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct FlagRequest {
    pub address: String,
    pub reason: String,
    pub flagged_by: String,
}

fn bad_request(message: impl ToString) -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": message.to_string() })),
    )
        .into_response()
}

/// `GET /aml/check/:address` - screen an address.
pub async fn check_address(
    State(screener): State<Arc<AmlScreener>>,
    Path(address): Path<String>,
) -> axum::response::Response {
    match screener.screen(&address).await {
        Ok(result) => Json(result).into_response(),
        Err(e) => bad_request(e),
    }
}

/// `POST /aml/flags` - manually flag an address (admin).
pub async fn flag_address(
    State(screener): State<Arc<AmlScreener>>,
    headers: HeaderMap,
    Json(req): Json<FlagRequest>,
) -> axum::response::Response {
    if let Err(e) = authorize_admin(&headers) {
        return e.into_response();
    }
    match screener.flag(&req.address, req.reason, req.flagged_by) {
        Ok(flag) => (StatusCode::CREATED, Json(flag)).into_response(),
        Err(e) => bad_request(e),
    }
}

/// `DELETE /aml/flags/:address` - remove a manual flag (admin).
pub async fn unflag_address(
    State(screener): State<Arc<AmlScreener>>,
    Path(address): Path<String>,
    headers: HeaderMap,
) -> axum::response::Response {
    if let Err(e) = authorize_admin(&headers) {
        return e.into_response();
    }
    match screener.unflag(&address) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => bad_request(e),
    }
}

/// `GET /aml/flags` - list manual flags (admin).
pub async fn list_flags(
    State(screener): State<Arc<AmlScreener>>,
    headers: HeaderMap,
) -> axum::response::Response {
    if let Err(e) = authorize_admin(&headers) {
        return e.into_response();
    }
    Json(screener.flags()).into_response()
}
