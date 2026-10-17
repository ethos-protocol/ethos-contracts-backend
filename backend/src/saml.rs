//! SAML 2.0 service provider for enterprise single sign-on.
//!
//! The backend previously accepted only its own credentials, which blocks
//! enterprise customers whose identity lives in an IdP (Okta, Entra ID,
//! Ping). This module implements the service provider half of the SAML 2.0
//! web browser SSO profile: SP metadata, an `AuthnRequest` initiator, an
//! assertion consumer service (ACS) endpoint, assertion validation, and
//! configurable attribute mapping onto internal user fields.
//!
//! # Architecture
//!
//! ```text
//! GET  /saml/metadata → sp_metadata          (XML the IdP is configured with)
//! GET  /saml/login    → initiate_login       (AuthnRequest + RelayState)
//! POST /saml/acs      → assertion_consumer_service
//!                         ├─ base64-decode SAMLResponse
//!                         ├─ parse_response    → SamlAssertion
//!                         ├─ validate_assertion (issuer, audience, window,
//!                         │                      InResponseTo, replay, signer)
//!                         └─ map_attributes     → SamlUser → session
//! ```
//!
//! # Signature handling
//!
//! Assertions must carry a `<Signature>` whose `<X509Certificate>` matches the
//! certificate pinned in [`SamlConfig`], so an assertion from an unknown
//! signer is rejected. Full XML-DSig RSA digest verification needs an XML
//! security library and is not performed here; deployments must therefore
//! terminate the ACS endpoint over TLS and pin the IdP certificate.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use axum::{extract::State, http::StatusCode, Form, Json};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::{DateTime, Duration, Utc};
use samael::crypto::{
    AllowedSignatureAlgorithm, CertificateDer, Crypto, CryptoProvider, ReduceMode,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

// === Configuration

/// Service provider configuration, plus the pinned IdP signing certificate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamlConfig {
    /// SP entity ID, echoed as the expected assertion audience.
    pub entity_id: String,
    /// Absolute URL of this service's ACS endpoint.
    pub acs_url: String,
    /// IdP entity ID, matched against the assertion issuer.
    pub idp_entity_id: String,
    /// IdP single sign-on URL that `AuthnRequest`s are sent to.
    pub idp_sso_url: String,
    /// Base64 body of the IdP signing certificate, without PEM armor.
    pub idp_certificate: String,
    /// Tolerance applied to `NotBefore` / `NotOnOrAfter`, for clock drift.
    pub clock_skew_secs: i64,
}

impl SamlConfig {
    /// Read the required SAML settings from the environment. No configuration
    /// disables SAML; a partially configured integration is a startup error.
    pub fn from_env() -> Result<Option<Self>, String> {
        let variables = [
            ("SAML_SP_ENTITY_ID", "entity_id"),
            ("SAML_SP_ACS_URL", "acs_url"),
            ("SAML_IDP_ENTITY_ID", "idp_entity_id"),
            ("SAML_IDP_SSO_URL", "idp_sso_url"),
            ("SAML_IDP_CERTIFICATE", "idp_certificate"),
        ];
        let values: Vec<_> = variables
            .iter()
            .map(|(key, _)| (*key, std::env::var(key).ok()))
            .collect();
        if values.iter().all(|(_, value)| value.is_none()) {
            return Ok(None);
        }

        let missing: Vec<_> = values
            .iter()
            .filter_map(|(key, value)| value.is_none().then_some(*key))
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "SAML is partially configured; set all required variables (missing: {})",
                missing.join(", ")
            ));
        }
        if values.iter().any(|(_, value)| {
            value
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
        }) {
            return Err("SAML configuration values must not be empty".to_string());
        }

        let values: HashMap<_, _> = values
            .into_iter()
            .filter_map(|(key, value)| value.map(|value| (key, value)))
            .collect();
        let certificate = normalize_certificate(
            values
                .get("SAML_IDP_CERTIFICATE")
                .expect("required SAML certificate was checked"),
        );
        let decoded_certificate = STANDARD
            .decode(&certificate)
            .map_err(|_| "SAML_IDP_CERTIFICATE must be a base64 DER certificate".to_string())?;
        openssl::x509::X509::from_der(&decoded_certificate)
            .map_err(|_| "SAML_IDP_CERTIFICATE is not a valid DER certificate".to_string())?;
        let clock_skew_secs = std::env::var("SAML_CLOCK_SKEW_SECS")
            .unwrap_or_else(|_| "60".to_string())
            .parse()
            .map_err(|_| "SAML_CLOCK_SKEW_SECS must be a non-negative integer".to_string())?;
        if clock_skew_secs < 0 {
            return Err("SAML_CLOCK_SKEW_SECS must be a non-negative integer".to_string());
        }

        Ok(Some(Self {
            entity_id: values["SAML_SP_ENTITY_ID"].clone(),
            acs_url: values["SAML_SP_ACS_URL"].clone(),
            idp_entity_id: values["SAML_IDP_ENTITY_ID"].clone(),
            idp_sso_url: values["SAML_IDP_SSO_URL"].clone(),
            idp_certificate: certificate,
            clock_skew_secs,
        }))
    }
}

/// Maps IdP attribute names onto the fields this backend needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributeMapping {
    pub email: String,
    pub display_name: String,
    pub groups: String,
}

impl Default for AttributeMapping {
    fn default() -> Self {
        Self {
            email: "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/emailaddress".to_string(),
            display_name: "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/displayname"
                .to_string(),
            groups: "http://schemas.xmlsoap.org/claims/Group".to_string(),
        }
    }
}

// === Model

/// The parsed contents of an assertion the SP cares about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SamlAssertion {
    pub id: String,
    pub issuer: String,
    pub subject: String,
    pub audience: Option<String>,
    pub not_before: Option<DateTime<Utc>>,
    pub not_on_or_after: Option<DateTime<Utc>>,
    /// The `AuthnRequest` ID this assertion answers, when present.
    pub in_response_to: Option<String>,
    pub session_index: Option<String>,
    /// Certificate carried by the signature, base64 without PEM armor.
    pub signing_certificate: Option<String>,
    pub attributes: HashMap<String, Vec<String>>,
}

/// An authenticated user, after attribute mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SamlUser {
    pub subject: String,
    pub email: String,
    pub display_name: Option<String>,
    pub groups: Vec<String>,
    pub session_index: Option<String>,
}

/// Every way an assertion can be refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SamlError {
    #[error("SAMLResponse is not valid base64")]
    NotBase64,
    #[error("SAMLResponse is not valid UTF-8 XML")]
    NotUtf8,
    #[error("missing element: {0}")]
    MissingElement(&'static str),
    #[error("IdP reported a non-success status: {0}")]
    StatusNotSuccess(String),
    #[error("unexpected issuer: {0}")]
    IssuerMismatch(String),
    #[error("audience {0:?} does not match this service provider")]
    AudienceMismatch(Option<String>),
    #[error("assertion is outside its validity window")]
    OutsideValidityWindow,
    #[error("assertion is not signed")]
    Unsigned,
    #[error("assertion was signed by an untrusted certificate")]
    UntrustedSigner,
    #[error("SAML response signature verification failed")]
    InvalidSignature,
    #[error("InResponseTo {0:?} does not match a pending AuthnRequest")]
    UnknownRequest(Option<String>),
    #[error("assertion {0} has already been consumed")]
    Replayed(String),
    #[error("required attribute not provided by the IdP: {0}")]
    MissingAttribute(String),
}

// === Parsing

/// Decode and parse a base64 `SAMLResponse` form field into an assertion.
pub fn parse_encoded_response(encoded: &str) -> Result<SamlAssertion, SamlError> {
    let raw = STANDARD
        .decode(encoded.trim())
        .map_err(|_| SamlError::NotBase64)?;
    let xml = String::from_utf8(raw).map_err(|_| SamlError::NotUtf8)?;
    parse_response(&xml)
}

/// Parse a SAML `Response` document, returning the assertion it carries.
pub fn parse_response(xml: &str) -> Result<SamlAssertion, SamlError> {
    let status = element_start_tag(xml, "StatusCode")
        .and_then(|tag| tag_attribute(tag, "Value"))
        .ok_or(SamlError::MissingElement("StatusCode"))?;
    if !status.ends_with(":Success") {
        return Err(SamlError::StatusNotSuccess(status));
    }

    let assertion = element_body(xml, "Assertion").ok_or(SamlError::MissingElement("Assertion"))?;
    let assertion_tag =
        element_start_tag(xml, "Assertion").ok_or(SamlError::MissingElement("Assertion"))?;

    let id = tag_attribute(assertion_tag, "ID").ok_or(SamlError::MissingElement("Assertion/ID"))?;
    let issuer = element_text(assertion, "Issuer")
        .or_else(|| element_text(xml, "Issuer"))
        .ok_or(SamlError::MissingElement("Issuer"))?;
    let subject_scope =
        element_body(assertion, "Subject").ok_or(SamlError::MissingElement("Subject"))?;
    let subject =
        element_text(subject_scope, "NameID").ok_or(SamlError::MissingElement("NameID"))?;

    let conditions_tag = element_start_tag(assertion, "Conditions");
    let audience = element_body(assertion, "AudienceRestriction")
        .and_then(|scope| element_text(scope, "Audience"));

    // The response-level SubjectConfirmationData carries InResponseTo, and its
    // NotOnOrAfter is the tighter of the two windows when both are present.
    let confirmation_tag = element_start_tag(assertion, "SubjectConfirmationData");
    let in_response_to = confirmation_tag.and_then(|tag| tag_attribute(tag, "InResponseTo"));

    let not_before = conditions_tag
        .and_then(|tag| tag_attribute(tag, "NotBefore"))
        .and_then(|value| parse_instant(&value));
    let not_on_or_after = [
        conditions_tag.and_then(|tag| tag_attribute(tag, "NotOnOrAfter")),
        confirmation_tag.and_then(|tag| tag_attribute(tag, "NotOnOrAfter")),
    ]
    .into_iter()
    .flatten()
    .filter_map(|value| parse_instant(&value))
    .min();

    let session_index = element_start_tag(assertion, "AuthnStatement")
        .and_then(|tag| tag_attribute(tag, "SessionIndex"));

    let signing_certificate = element_body(assertion, "Signature")
        .or_else(|| element_body(xml, "Signature"))
        .and_then(|scope| element_text(scope, "X509Certificate"))
        .map(|cert| normalize_certificate(&cert));

    Ok(SamlAssertion {
        id,
        issuer,
        subject,
        audience,
        not_before,
        not_on_or_after,
        in_response_to,
        session_index,
        signing_certificate,
        attributes: parse_attributes(assertion),
    })
}

/// Collect `<Attribute Name="…"><AttributeValue>` pairs, keeping every value
/// so multi-valued attributes such as group membership survive.
fn parse_attributes(assertion: &str) -> HashMap<String, Vec<String>> {
    let mut attributes: HashMap<String, Vec<String>> = HashMap::new();
    let mut cursor = 0usize;

    while let Some((start, body_start)) = find_start_tag(&assertion[cursor..], "Attribute") {
        let absolute_start = cursor + start;
        let absolute_body = cursor + body_start;
        let tag = &assertion[absolute_start..absolute_body];
        let Some(name) = tag_attribute(tag, "Name") else {
            cursor = absolute_body;
            continue;
        };

        let body = match find_end_tag(&assertion[absolute_body..], "Attribute") {
            Some(end) => &assertion[absolute_body..absolute_body + end],
            None => &assertion[absolute_body..],
        };

        let values = element_texts(body, "AttributeValue");
        attributes.entry(name).or_default().extend(values);
        cursor = absolute_body;
    }

    attributes
}

fn parse_instant(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|instant| instant.with_timezone(&Utc))
}

fn normalize_certificate(certificate: &str) -> String {
    certificate.split_whitespace().collect::<String>()
}

/// SHA-256 fingerprint of a certificate body, for config and audit logs.
pub fn certificate_fingerprint(certificate: &str) -> String {
    let digest = Sha256::digest(normalize_certificate(certificate).as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

// === Validation

/// Validated request/replay state, shared across requests.
#[derive(Debug, Default)]
pub struct SamlSecurityState {
    /// IDs of `AuthnRequest`s this SP issued and has not yet consumed.
    pub pending_requests: Mutex<HashSet<String>>,
    /// IDs of assertions already consumed, mapped to the instant after which
    /// the entry may be purged. The TTL matches the assertion's own
    /// `NotOnOrAfter` (plus clock skew), since an assertion can never be
    /// replayed successfully once it is outside its own validity window
    /// anyway — keeping the entry longer would only grow this set forever.
    pub consumed_assertions: Mutex<HashMap<String, DateTime<Utc>>>,
}

/// Validate an assertion against SP configuration and request state.
pub fn validate_assertion(
    assertion: &SamlAssertion,
    config: &SamlConfig,
    security: &SamlSecurityState,
    now: DateTime<Utc>,
) -> Result<(), SamlError> {
    if assertion.issuer != config.idp_entity_id {
        return Err(SamlError::IssuerMismatch(assertion.issuer.clone()));
    }

    if assertion.audience.as_deref() != Some(config.entity_id.as_str()) {
        return Err(SamlError::AudienceMismatch(assertion.audience.clone()));
    }

    let skew = Duration::seconds(config.clock_skew_secs);
    if let Some(not_before) = assertion.not_before {
        if now + skew < not_before {
            return Err(SamlError::OutsideValidityWindow);
        }
    }
    if let Some(not_on_or_after) = assertion.not_on_or_after {
        if now - skew >= not_on_or_after {
            return Err(SamlError::OutsideValidityWindow);
        }
    }

    match &assertion.signing_certificate {
        None => return Err(SamlError::Unsigned),
        Some(certificate) => {
            if certificate != &normalize_certificate(&config.idp_certificate) {
                return Err(SamlError::UntrustedSigner);
            }
        }
    }

    // Unsolicited (IdP-initiated) assertions carry no InResponseTo; SP-initiated
    // ones must match a request this SP actually issued.
    if let Some(request_id) = &assertion.in_response_to {
        let mut pending = security.pending_requests.lock().unwrap();
        if !pending.remove(request_id) {
            return Err(SamlError::UnknownRequest(Some(request_id.clone())));
        }
    }

    // The TTL for the replay guard is the assertion's own validity window
    // (widened by clock skew), so the set only ever holds entries for
    // assertions that could plausibly still be replayed.
    let guard_expiry = assertion.not_on_or_after.unwrap_or(now) + skew;

    let mut consumed = security.consumed_assertions.lock().unwrap();
    consumed.retain(|_, expiry| *expiry > now);
    if consumed.contains_key(&assertion.id) {
        return Err(SamlError::Replayed(assertion.id.clone()));
    }
    consumed.insert(assertion.id.clone(), guard_expiry);

    Ok(())
}

/// Project assertion attributes onto internal user fields.
pub fn map_attributes(
    assertion: &SamlAssertion,
    mapping: &AttributeMapping,
) -> Result<SamlUser, SamlError> {
    let email = assertion
        .attributes
        .get(&mapping.email)
        .and_then(|values| values.first())
        .cloned()
        // NameID is an email address in most enterprise configurations, so it
        // is a safe fallback when the IdP omits the claim.
        .or_else(|| {
            assertion
                .subject
                .contains('@')
                .then(|| assertion.subject.clone())
        })
        .ok_or_else(|| SamlError::MissingAttribute(mapping.email.clone()))?;

    Ok(SamlUser {
        subject: assertion.subject.clone(),
        email,
        display_name: assertion
            .attributes
            .get(&mapping.display_name)
            .and_then(|values| values.first())
            .cloned(),
        groups: assertion
            .attributes
            .get(&mapping.groups)
            .cloned()
            .unwrap_or_default(),
        session_index: assertion.session_index.clone(),
    })
}

// === HTTP surface

#[derive(Clone)]
pub struct SamlState {
    pub config: SamlConfig,
    pub mapping: AttributeMapping,
    pub security: Arc<SamlSecurityState>,
    pub enabled: bool,
    /// Users authenticated through SSO, most recent last.
    pub sessions: Arc<Mutex<Vec<SamlUser>>>,
}

impl SamlState {
    pub fn new(config: SamlConfig, mapping: AttributeMapping) -> Self {
        Self {
            config,
            mapping,
            security: Arc::new(SamlSecurityState::default()),
            enabled: true,
            sessions: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn disabled() -> Self {
        Self {
            config: SamlConfig {
                entity_id: String::new(),
                acs_url: String::new(),
                idp_entity_id: String::new(),
                idp_sso_url: String::new(),
                idp_certificate: String::new(),
                clock_skew_secs: 60,
            },
            mapping: AttributeMapping::default(),
            security: Arc::new(SamlSecurityState::default()),
            enabled: false,
            sessions: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

/// Form body posted by the browser to the ACS endpoint.
#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
pub struct AcsForm {
    /// Field name is fixed by the SAML HTTP POST binding.
    pub SAMLResponse: String,
    pub RelayState: Option<String>,
}

/// `GET /saml/metadata` returns the SP metadata document to hand to the IdP.
pub fn sp_metadata(config: &SamlConfig) -> String {
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<md:EntityDescriptor xmlns:md="urn:oasis:names:tc:SAML:2.0:metadata" entityID="{entity}">"#,
            r#"<md:SPSSODescriptor AuthnRequestsSigned="false" WantAssertionsSigned="true""#,
            r#" protocolSupportEnumeration="urn:oasis:names:tc:SAML:2.0:protocol">"#,
            r#"<md:NameIDFormat>urn:oasis:names:tc:SAML:1.1:nameid-format:emailAddress</md:NameIDFormat>"#,
            r#"<md:AssertionConsumerService Binding="urn:oasis:names:tc:SAML:2.0:bindings:HTTP-POST""#,
            r#" Location="{acs}" index="0" isDefault="true"/>"#,
            r#"</md:SPSSODescriptor></md:EntityDescriptor>"#
        ),
        entity = config.entity_id,
        acs = config.acs_url
    )
}

/// Build an `AuthnRequest` and register its ID as pending.
pub fn build_authn_request(state: &SamlState, request_id: &str) -> String {
    state
        .security
        .pending_requests
        .lock()
        .unwrap()
        .insert(request_id.to_string());

    format!(
        concat!(
            r#"<samlp:AuthnRequest xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol""#,
            r#" ID="{id}" Version="2.0" IssueInstant="{instant}" Destination="{destination}""#,
            r#" ProtocolBinding="urn:oasis:names:tc:SAML:2.0:bindings:HTTP-POST""#,
            r#" AssertionConsumerServiceURL="{acs}">"#,
            r#"<saml:Issuer xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion">{entity}</saml:Issuer>"#,
            r#"</samlp:AuthnRequest>"#
        ),
        id = request_id,
        instant = Utc::now().to_rfc3339(),
        destination = state.config.idp_sso_url,
        acs = state.config.acs_url,
        entity = state.config.entity_id
    )
}

/// `GET /saml/metadata` handler.
pub async fn get_metadata(State(state): State<Arc<SamlState>>) -> (StatusCode, String) {
    if !state.enabled {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "SAML is not configured".to_string(),
        );
    }
    (StatusCode::OK, sp_metadata(&state.config))
}

/// `GET /saml/login` starts SP-initiated SSO, returning the request the
/// browser must POST to the IdP.
pub async fn initiate_login(
    State(state): State<Arc<SamlState>>,
) -> (StatusCode, Json<serde_json::Value>) {
    if !state.enabled {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "SAML is not configured" })),
        );
    }
    let request_id = format!("_{}", uuid::Uuid::new_v4());
    let authn_request = build_authn_request(&state, &request_id);
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "request_id": request_id,
            "destination": state.config.idp_sso_url,
            "saml_request": STANDARD.encode(authn_request),
        })),
    )
}

/// `POST /saml/acs` consumes an IdP assertion and establishes a session.
pub async fn assertion_consumer_service(
    State(state): State<Arc<SamlState>>,
    Form(body): Form<AcsForm>,
) -> (StatusCode, Json<serde_json::Value>) {
    if !state.enabled {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "SAML is not configured" })),
        );
    }

    let outcome =
        parse_verified_response(&body.SAMLResponse, &state.config).and_then(|assertion| {
            validate_assertion(&assertion, &state.config, &state.security, Utc::now())?;
            map_attributes(&assertion, &state.mapping)
        });

    match outcome {
        Ok(user) => {
            tracing::info!(subject = %user.subject, "SAML assertion accepted");
            state.sessions.lock().unwrap().push(user.clone());
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "user": user,
                    "relay_state": body.RelayState,
                })),
            )
        }
        Err(error) => {
            tracing::warn!(error = %error, "SAML assertion rejected");
            (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
        }
    }
}

fn parse_verified_response(encoded: &str, config: &SamlConfig) -> Result<SamlAssertion, SamlError> {
    let raw = STANDARD
        .decode(encoded.trim())
        .map_err(|_| SamlError::NotBase64)?;
    let xml = String::from_utf8(raw).map_err(|_| SamlError::NotUtf8)?;
    let certificate = STANDARD
        .decode(normalize_certificate(&config.idp_certificate))
        .map_err(|_| SamlError::InvalidSignature)?;
    let certificate = CertificateDer::from(certificate);
    let allowed_algorithms = [
        AllowedSignatureAlgorithm::RsaSha256,
        AllowedSignatureAlgorithm::RsaSha384,
        AllowedSignatureAlgorithm::RsaSha512,
        AllowedSignatureAlgorithm::EcdsaSha256,
        AllowedSignatureAlgorithm::EcdsaSha384,
        AllowedSignatureAlgorithm::EcdsaSha512,
    ];
    // Requiring the Response digest prevents accepting an assertion-only signature.
    let verified_xml = Crypto::reduce_xml_to_signed_with_allowed_algorithms(
        &xml,
        &[certificate],
        ReduceMode::PreDigest,
        Some(&allowed_algorithms),
    )
    .map_err(|_| SamlError::InvalidSignature)?;
    let mut assertion = parse_response(&verified_xml)?;
    if assertion.signing_certificate.is_none() {
        assertion.signing_certificate = Some(normalize_certificate(&config.idp_certificate));
    }
    Ok(assertion)
}

// === XML helpers
//
// A lexical reader is enough here: only well-known elements and attributes are
// read, and namespace prefixes are matched loosely by local name.

/// Byte offsets of a start tag: where `<` sits, and where its body begins.
fn find_start_tag(xml: &str, local_name: &str) -> Option<(usize, usize)> {
    let mut cursor = 0usize;
    while let Some(offset) = xml[cursor..].find('<') {
        let start = cursor + offset;
        let after = &xml[start + 1..];
        cursor = start + 1;

        let name_end = after
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .unwrap_or(after.len());
        let qualified = &after[..name_end];
        let local = qualified.rsplit(':').next().unwrap_or(qualified);
        if local != local_name {
            continue;
        }

        let close = after.find('>')? + start + 1;
        // Self-closing tags have no body; report an empty one.
        return Some((start, close + 1));
    }
    None
}

fn find_end_tag(xml: &str, local_name: &str) -> Option<usize> {
    let mut cursor = 0usize;
    while let Some(offset) = xml[cursor..].find("</") {
        let start = cursor + offset;
        let after = &xml[start + 2..];
        cursor = start + 2;

        let name_end = after
            .find(|c: char| c.is_whitespace() || c == '>')
            .unwrap_or(after.len());
        let qualified = &after[..name_end];
        let local = qualified.rsplit(':').next().unwrap_or(qualified);
        if local == local_name {
            return Some(start);
        }
    }
    None
}

/// The start tag text of the first matching element, including attributes.
fn element_start_tag<'a>(xml: &'a str, local_name: &str) -> Option<&'a str> {
    let (start, body) = find_start_tag(xml, local_name)?;
    Some(&xml[start..body])
}

/// Everything between an element's start and end tag.
fn element_body<'a>(xml: &'a str, local_name: &str) -> Option<&'a str> {
    let (_, body_start) = find_start_tag(xml, local_name)?;
    let end = find_end_tag(&xml[body_start..], local_name)?;
    Some(&xml[body_start..body_start + end])
}

fn element_text(xml: &str, local_name: &str) -> Option<String> {
    element_body(xml, local_name).map(|body| body.trim().to_string())
}

fn element_texts(xml: &str, local_name: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut cursor = 0usize;
    while let Some((_, body_start)) = find_start_tag(&xml[cursor..], local_name) {
        let absolute = cursor + body_start;
        match find_end_tag(&xml[absolute..], local_name) {
            Some(end) => {
                values.push(xml[absolute..absolute + end].trim().to_string());
                cursor = absolute + end;
            }
            None => break,
        }
    }
    values
}

fn tag_attribute(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let end = tag[start..].find('"')? + start;
    Some(tag[start..end].to_string())
}

// === Tests

#[cfg(test)]
mod tests {
    use super::*;
    use openssl::{
        asn1::Asn1Time,
        hash::MessageDigest,
        pkey::PKey,
        rsa::Rsa,
        x509::{X509NameBuilder, X509},
    };

    const CERT: &str = "MIIDpDCCAoygAwIBAgIGAV2ka+4iMA0GCSqGSIb3DQEBCwUAMIGSMQsw";

    fn config() -> SamlConfig {
        SamlConfig {
            entity_id: "https://api.ethos.example/saml".to_string(),
            acs_url: "https://api.ethos.example/saml/acs".to_string(),
            idp_entity_id: "http://www.okta.com/exk1234".to_string(),
            idp_sso_url: "https://ethos.okta.com/app/sso/saml".to_string(),
            idp_certificate: CERT.to_string(),
            clock_skew_secs: 60,
        }
    }

    fn response_xml(
        not_before: &str,
        not_on_or_after: &str,
        in_response_to: &str,
        certificate: &str,
    ) -> String {
        format!(
            r#"<?xml version="1.0"?>
<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion">
  <samlp:Status><samlp:StatusCode Value="urn:oasis:names:tc:SAML:2.0:status:Success"/></samlp:Status>
  <saml:Assertion ID="_assert-1" IssueInstant="2026-07-30T10:00:00Z">
    <saml:Issuer>http://www.okta.com/exk1234</saml:Issuer>
    <ds:Signature xmlns:ds="http://www.w3.org/2000/09/xmldsig#">
      <ds:KeyInfo><ds:X509Data><ds:X509Certificate>{certificate}</ds:X509Certificate></ds:X509Data></ds:KeyInfo>
    </ds:Signature>
    <saml:Subject>
      <saml:NameID Format="urn:oasis:names:tc:SAML:1.1:nameid-format:emailAddress">ada@corp.example</saml:NameID>
      <saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">
        <saml:SubjectConfirmationData InResponseTo="{in_response_to}" NotOnOrAfter="{not_on_or_after}" Recipient="https://api.ethos.example/saml/acs"/>
      </saml:SubjectConfirmation>
    </saml:Subject>
    <saml:Conditions NotBefore="{not_before}" NotOnOrAfter="{not_on_or_after}">
      <saml:AudienceRestriction><saml:Audience>https://api.ethos.example/saml</saml:Audience></saml:AudienceRestriction>
    </saml:Conditions>
    <saml:AuthnStatement AuthnInstant="2026-07-30T10:00:00Z" SessionIndex="idx-99"/>
    <saml:AttributeStatement>
      <saml:Attribute Name="http://schemas.xmlsoap.org/ws/2005/05/identity/claims/emailaddress">
        <saml:AttributeValue>ada@corp.example</saml:AttributeValue>
      </saml:Attribute>
      <saml:Attribute Name="http://schemas.xmlsoap.org/ws/2005/05/identity/claims/displayname">
        <saml:AttributeValue>Ada Lovelace</saml:AttributeValue>
      </saml:Attribute>
      <saml:Attribute Name="http://schemas.xmlsoap.org/claims/Group">
        <saml:AttributeValue>engineering</saml:AttributeValue>
        <saml:AttributeValue>vault-admins</saml:AttributeValue>
      </saml:Attribute>
    </saml:AttributeStatement>
  </saml:Assertion>
</samlp:Response>"#
        )
    }

    fn valid_response() -> String {
        response_xml(
            "2026-07-30T09:59:00Z",
            "2026-07-30T10:05:00Z",
            "_req-1",
            CERT,
        )
    }

    fn signed_response() -> (String, SamlConfig) {
        let key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
        let mut name = X509NameBuilder::new().unwrap();
        name.append_entry_by_text("CN", "SAML test IdP").unwrap();
        let name = name.build();
        let mut cert_builder = X509::builder().unwrap();
        cert_builder.set_version(2).unwrap();
        cert_builder
            .set_serial_number(
                &openssl::bn::BigNum::from_u32(1)
                    .unwrap()
                    .to_asn1_integer()
                    .unwrap(),
            )
            .unwrap();
        cert_builder.set_subject_name(&name).unwrap();
        cert_builder.set_issuer_name(&name).unwrap();
        cert_builder.set_pubkey(&key).unwrap();
        cert_builder
            .set_not_before(&Asn1Time::days_from_now(0).unwrap())
            .unwrap();
        cert_builder
            .set_not_after(&Asn1Time::days_from_now(1).unwrap())
            .unwrap();
        cert_builder.sign(&key, MessageDigest::sha256()).unwrap();
        let certificate = cert_builder.build().to_der().unwrap();

        let placeholder = format!(
            r#"    <ds:Signature xmlns:ds="http://www.w3.org/2000/09/xmldsig#">
      <ds:KeyInfo><ds:X509Data><ds:X509Certificate>{CERT}</ds:X509Certificate></ds:X509Data></ds:KeyInfo>
    </ds:Signature>"#
        );
        let signature_template = r##"<ds:Signature xmlns:ds="http://www.w3.org/2000/09/xmldsig#">
  <ds:SignedInfo>
    <ds:CanonicalizationMethod Algorithm="http://www.w3.org/2001/10/xml-exc-c14n#"/>
    <ds:SignatureMethod Algorithm="http://www.w3.org/2001/04/xmldsig-more#rsa-sha256"/>
    <ds:Reference URI="#_response-1">
      <ds:Transforms>
        <ds:Transform Algorithm="http://www.w3.org/2000/09/xmldsig#enveloped-signature"/>
        <ds:Transform Algorithm="http://www.w3.org/2001/10/xml-exc-c14n#"/>
      </ds:Transforms>
      <ds:DigestMethod Algorithm="http://www.w3.org/2001/04/xmlenc#sha256"/>
      <ds:DigestValue></ds:DigestValue>
    </ds:Reference>
  </ds:SignedInfo>
  <ds:SignatureValue></ds:SignatureValue>
</ds:Signature>"##;
        let unsigned_xml = valid_response()
            .replace(&placeholder, "")
            .replace("<samlp:Response ", r#"<samlp:Response ID="_response-1" "#)
            .replace(
                "</samlp:Status>",
                &format!("</samlp:Status>{signature_template}"),
            );
        let private_key = key.private_key_to_der().unwrap();
        let signed_xml = Crypto::sign_xml(unsigned_xml.as_bytes(), &private_key).unwrap();
        let mut config = config();
        config.idp_certificate = STANDARD.encode(certificate);
        (STANDARD.encode(signed_xml.as_bytes()), config)
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-07-30T10:01:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn security_with_pending(request_id: &str) -> SamlSecurityState {
        let security = SamlSecurityState::default();
        security
            .pending_requests
            .lock()
            .unwrap()
            .insert(request_id.to_string());
        security
    }

    #[test]
    fn parse_response_extracts_core_fields() {
        let assertion = parse_response(&valid_response()).unwrap();
        assert_eq!(assertion.id, "_assert-1");
        assert_eq!(assertion.issuer, "http://www.okta.com/exk1234");
        assert_eq!(assertion.subject, "ada@corp.example");
        assert_eq!(
            assertion.audience.as_deref(),
            Some("https://api.ethos.example/saml")
        );
        assert_eq!(assertion.in_response_to.as_deref(), Some("_req-1"));
        assert_eq!(assertion.session_index.as_deref(), Some("idx-99"));
        assert_eq!(assertion.signing_certificate.as_deref(), Some(CERT));
    }

    #[test]
    fn response_signature_is_verified_against_the_pinned_certificate() {
        let (encoded, config) = signed_response();
        let assertion = parse_verified_response(&encoded, &config).unwrap();
        assert_eq!(assertion.subject, "ada@corp.example");
        assert_eq!(
            assertion.signing_certificate.as_deref(),
            Some(config.idp_certificate.as_str())
        );
    }

    #[test]
    fn tampered_response_is_rejected_even_when_the_certificate_is_trusted() {
        let (encoded, config) = signed_response();
        let xml = String::from_utf8(STANDARD.decode(encoded).unwrap()).unwrap();
        let tampered = STANDARD.encode(xml.replace("ada@corp.example", "mallory@corp.example"));
        assert_eq!(
            parse_verified_response(&tampered, &config),
            Err(SamlError::InvalidSignature)
        );
    }

    #[test]
    fn parse_response_keeps_multivalued_attributes() {
        let assertion = parse_response(&valid_response()).unwrap();
        assert_eq!(
            assertion.attributes["http://schemas.xmlsoap.org/claims/Group"],
            vec!["engineering".to_string(), "vault-admins".to_string()]
        );
    }

    #[test]
    fn parse_response_rejects_failure_status() {
        let xml = valid_response().replace(
            "urn:oasis:names:tc:SAML:2.0:status:Success",
            "urn:oasis:names:tc:SAML:2.0:status:Requester",
        );
        assert!(matches!(
            parse_response(&xml),
            Err(SamlError::StatusNotSuccess(_))
        ));
    }

    #[test]
    fn parse_encoded_response_rejects_non_base64() {
        assert_eq!(
            parse_encoded_response("!!!not base64!!!"),
            Err(SamlError::NotBase64)
        );
    }

    #[test]
    fn parse_encoded_response_accepts_base64_post_binding() {
        let encoded = STANDARD.encode(valid_response());
        assert_eq!(parse_encoded_response(&encoded).unwrap().id, "_assert-1");
    }

    #[test]
    fn validate_assertion_accepts_a_good_assertion() {
        let assertion = parse_response(&valid_response()).unwrap();
        let security = security_with_pending("_req-1");
        assert_eq!(
            validate_assertion(&assertion, &config(), &security, now()),
            Ok(())
        );
    }

    #[test]
    fn validate_assertion_rejects_wrong_issuer() {
        let mut assertion = parse_response(&valid_response()).unwrap();
        assertion.issuer = "http://evil.example".to_string();
        let security = security_with_pending("_req-1");
        assert!(matches!(
            validate_assertion(&assertion, &config(), &security, now()),
            Err(SamlError::IssuerMismatch(_))
        ));
    }

    #[test]
    fn validate_assertion_rejects_wrong_audience() {
        let mut assertion = parse_response(&valid_response()).unwrap();
        assertion.audience = Some("https://other.example".to_string());
        let security = security_with_pending("_req-1");
        assert!(matches!(
            validate_assertion(&assertion, &config(), &security, now()),
            Err(SamlError::AudienceMismatch(_))
        ));
    }

    #[test]
    fn validate_assertion_rejects_expired_window() {
        let assertion = parse_response(&valid_response()).unwrap();
        let security = security_with_pending("_req-1");
        let later = DateTime::parse_from_rfc3339("2026-07-30T11:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            validate_assertion(&assertion, &config(), &security, later),
            Err(SamlError::OutsideValidityWindow)
        );
    }

    #[test]
    fn validate_assertion_tolerates_clock_skew() {
        let assertion = parse_response(&valid_response()).unwrap();
        let security = security_with_pending("_req-1");
        // 30s before NotBefore, inside the configured 60s skew.
        let early = DateTime::parse_from_rfc3339("2026-07-30T09:58:30Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            validate_assertion(&assertion, &config(), &security, early),
            Ok(())
        );
    }

    #[test]
    fn validate_assertion_rejects_unsigned_and_untrusted() {
        let security = security_with_pending("_req-1");
        let mut unsigned = parse_response(&valid_response()).unwrap();
        unsigned.signing_certificate = None;
        assert_eq!(
            validate_assertion(&unsigned, &config(), &security, now()),
            Err(SamlError::Unsigned)
        );

        let other = parse_response(&response_xml(
            "2026-07-30T09:59:00Z",
            "2026-07-30T10:05:00Z",
            "_req-1",
            "AAAAdifferentcert",
        ))
        .unwrap();
        assert_eq!(
            validate_assertion(&other, &config(), &security, now()),
            Err(SamlError::UntrustedSigner)
        );
    }

    #[test]
    fn validate_assertion_rejects_unknown_request_id() {
        let assertion = parse_response(&valid_response()).unwrap();
        let security = SamlSecurityState::default();
        assert!(matches!(
            validate_assertion(&assertion, &config(), &security, now()),
            Err(SamlError::UnknownRequest(_))
        ));
    }

    #[test]
    fn validate_assertion_rejects_replay() {
        let assertion = parse_response(&valid_response()).unwrap();
        let security = security_with_pending("_req-1");
        assert_eq!(
            validate_assertion(&assertion, &config(), &security, now()),
            Ok(())
        );

        // A replay reuses a consumed request ID too, so re-arm it to prove the
        // assertion ID check is what rejects the second attempt.
        security
            .pending_requests
            .lock()
            .unwrap()
            .insert("_req-1".to_string());
        assert!(matches!(
            validate_assertion(&assertion, &config(), &security, now()),
            Err(SamlError::Replayed(_))
        ));
    }

    #[test]
    fn validate_assertion_purges_expired_replay_guard_entries() {
        let assertion = parse_response(&valid_response()).unwrap();
        let security = security_with_pending("_req-1");
        // Simulate a replay-guard entry left over from a much earlier
        // assertion whose own validity window has since elapsed; it must not
        // block a legitimately new assertion that happens to reuse the ID
        // space (or, more importantly, must not accumulate unboundedly).
        security
            .consumed_assertions
            .lock()
            .unwrap()
            .insert(assertion.id.clone(), now() - Duration::seconds(1));
        assert_eq!(
            validate_assertion(&assertion, &config(), &security, now()),
            Ok(())
        );
        assert!(security
            .consumed_assertions
            .lock()
            .unwrap()
            .contains_key(&assertion.id));
    }

    #[test]
    fn map_attributes_projects_configured_claims() {
        let assertion = parse_response(&valid_response()).unwrap();
        let user = map_attributes(&assertion, &AttributeMapping::default()).unwrap();
        assert_eq!(
            user,
            SamlUser {
                subject: "ada@corp.example".to_string(),
                email: "ada@corp.example".to_string(),
                display_name: Some("Ada Lovelace".to_string()),
                groups: vec!["engineering".to_string(), "vault-admins".to_string()],
                session_index: Some("idx-99".to_string()),
            }
        );
    }

    #[test]
    fn map_attributes_honors_custom_mapping() {
        let mut assertion = parse_response(&valid_response()).unwrap();
        assertion
            .attributes
            .insert("mail".to_string(), vec!["ada@alt.example".to_string()]);
        let mapping = AttributeMapping {
            email: "mail".to_string(),
            display_name: "cn".to_string(),
            groups: "memberOf".to_string(),
        };

        let user = map_attributes(&assertion, &mapping).unwrap();
        assert_eq!(user.email, "ada@alt.example");
        assert_eq!(user.display_name, None);
        assert!(user.groups.is_empty());
    }

    #[test]
    fn map_attributes_falls_back_to_email_name_id() {
        let mut assertion = parse_response(&valid_response()).unwrap();
        assertion.attributes.clear();
        let user = map_attributes(&assertion, &AttributeMapping::default()).unwrap();
        assert_eq!(user.email, "ada@corp.example");
    }

    #[test]
    fn map_attributes_fails_without_any_email() {
        let mut assertion = parse_response(&valid_response()).unwrap();
        assertion.attributes.clear();
        assertion.subject = "opaque-subject-id".to_string();
        assert!(matches!(
            map_attributes(&assertion, &AttributeMapping::default()),
            Err(SamlError::MissingAttribute(_))
        ));
    }

    #[test]
    fn sp_metadata_advertises_the_acs_endpoint() {
        let metadata = sp_metadata(&config());
        assert!(metadata.contains(r#"entityID="https://api.ethos.example/saml""#));
        assert!(metadata.contains(r#"Location="https://api.ethos.example/saml/acs""#));
        assert!(metadata.contains("WantAssertionsSigned=\"true\""));
    }

    #[test]
    fn build_authn_request_registers_pending_id() {
        let state = SamlState::new(config(), AttributeMapping::default());
        let request = build_authn_request(&state, "_req-42");
        assert!(request.contains(r#"ID="_req-42""#));
        assert!(request.contains("https://ethos.okta.com/app/sso/saml"));
        assert!(state
            .security
            .pending_requests
            .lock()
            .unwrap()
            .contains("_req-42"));
    }

    #[test]
    fn certificate_fingerprint_ignores_whitespace() {
        assert_eq!(
            certificate_fingerprint(CERT),
            certificate_fingerprint(&format!("  {CERT}\n"))
        );
    }
}
