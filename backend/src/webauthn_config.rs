//! Relying-party configuration for WebAuthn (#695).
//!
//! Kept free of crate dependencies so the startup checks can be tested on their own.
//!
//! A passkey is bound to the RP ID and origin it was registered against. If the
//! server silently falls back to `localhost`, a deployment that forgets to set
//! `WEBAUTHN_RP_ID` / `WEBAUTHN_ORIGIN` accepts assertions scoped to the wrong
//! origin. So the defaults apply only when `RUST_ENV=development`. Every other
//! deployment must set both variables, and startup fails if the RP ID is not
//! a registrable suffix of the origin's host.

const ENV_RP_ID: &str = "WEBAUTHN_RP_ID";
const ENV_RP_NAME: &str = "WEBAUTHN_RP_NAME";
const ENV_ORIGIN: &str = "WEBAUTHN_ORIGIN";
const DEV_RP_ID: &str = "localhost";
const DEV_ORIGIN: &str = "http://localhost:3000";
const DEFAULT_RP_NAME: &str = "Ethos Protocol";

/// Startup configuration errors for the WebAuthn relying party.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebAuthnConfigError {
    /// A required variable is unset outside of `RUST_ENV=development`.
    Missing(&'static str),
    /// `WEBAUTHN_ORIGIN` is not a bare `scheme://host[:port]` origin, or uses
    /// plain HTTP for a non-localhost host.
    InvalidOrigin(String),
    /// `WEBAUTHN_RP_ID` is not the origin's host or a parent domain of it.
    RpIdMismatch { rp_id: String, origin_host: String },
}

impl std::fmt::Display for WebAuthnConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(var) => write!(
                f,
                "{var} must be set (localhost defaults only apply when RUST_ENV=development)"
            ),
            Self::InvalidOrigin(origin) => write!(
                f,
                "WEBAUTHN_ORIGIN '{origin}' must be scheme://host[:port] with https (http only for localhost)"
            ),
            Self::RpIdMismatch { rp_id, origin_host } => write!(
                f,
                "WEBAUTHN_RP_ID '{rp_id}' does not match origin host '{origin_host}'"
            ),
        }
    }
}

impl std::error::Error for WebAuthnConfigError {}

/// Validated relying-party settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelyingPartyConfig {
    pub rp_id: String,
    pub rp_name: String,
    pub origin: String,
}

impl RelyingPartyConfig {
    /// Read the configuration from the process environment.
    pub fn from_env() -> Result<Self, WebAuthnConfigError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Read the configuration through `lookup`, so tests need not touch the environment.
    pub fn from_lookup(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, WebAuthnConfigError> {
        let development = lookup("RUST_ENV").is_some_and(|v| v.eq_ignore_ascii_case("development"));
        let dev_default = |value: &str| development.then(|| value.to_string());

        let rp_id = lookup(ENV_RP_ID)
            .or_else(|| dev_default(DEV_RP_ID))
            .ok_or(WebAuthnConfigError::Missing(ENV_RP_ID))?;
        let origin = lookup(ENV_ORIGIN)
            .or_else(|| dev_default(DEV_ORIGIN))
            .ok_or(WebAuthnConfigError::Missing(ENV_ORIGIN))?;
        let rp_name = lookup(ENV_RP_NAME).unwrap_or_else(|| DEFAULT_RP_NAME.to_string());

        validate_rp_binding(&rp_id, &origin)?;
        Ok(Self {
            rp_id,
            rp_name,
            origin,
        })
    }
}

/// Check that the RP ID is the origin's host or a parent domain of it, the rule
/// WebAuthn clients enforce, so a mismatch is caught at startup.
fn validate_rp_binding(rp_id: &str, origin: &str) -> Result<(), WebAuthnConfigError> {
    let invalid = || WebAuthnConfigError::InvalidOrigin(origin.to_string());
    let (scheme, rest) = origin.split_once("://").ok_or_else(invalid)?;
    if rest.is_empty() || rest.contains(['/', '?', '#', '@', '[']) {
        return Err(invalid());
    }
    let host = rest
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if host.is_empty() {
        return Err(invalid());
    }
    let is_localhost = host == "localhost";
    if scheme != "https" && !(scheme == "http" && is_localhost) {
        return Err(invalid());
    }

    let rp_id = rp_id.to_ascii_lowercase();
    let mismatch = || WebAuthnConfigError::RpIdMismatch {
        rp_id: rp_id.clone(),
        origin_host: host.clone(),
    };
    if rp_id.is_empty() || rp_id.contains(['/', ':', '@', ' ']) {
        return Err(mismatch());
    }
    if host == rp_id || host.ends_with(&format!(".{rp_id}")) {
        Ok(())
    } else {
        Err(mismatch())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    fn production(rp_id: &str, origin: &str) -> Result<RelyingPartyConfig, WebAuthnConfigError> {
        RelyingPartyConfig::from_lookup(lookup(&[
            ("WEBAUTHN_RP_ID", rp_id),
            ("WEBAUTHN_ORIGIN", origin),
        ]))
    }

    #[test]
    fn production_config_with_matching_rp_id_is_accepted() {
        let cfg =
            production("example.com", "https://app.example.com").expect("valid production config");
        assert_eq!(cfg.rp_id, "example.com");
        assert_eq!(cfg.origin, "https://app.example.com");
        assert_eq!(cfg.rp_name, "Ethos Protocol");
    }

    #[test]
    fn rp_name_can_be_overridden() {
        let cfg = RelyingPartyConfig::from_lookup(lookup(&[
            ("WEBAUTHN_RP_ID", "example.com"),
            ("WEBAUTHN_ORIGIN", "https://example.com"),
            ("WEBAUTHN_RP_NAME", "Acme"),
        ]))
        .expect("valid config");
        assert_eq!(cfg.rp_name, "Acme");
    }

    #[test]
    fn unset_variables_fail_outside_development() {
        let err = RelyingPartyConfig::from_lookup(lookup(&[])).unwrap_err();
        assert_eq!(err, WebAuthnConfigError::Missing(ENV_RP_ID));

        let err = RelyingPartyConfig::from_lookup(lookup(&[("WEBAUTHN_RP_ID", "example.com")]))
            .unwrap_err();
        assert_eq!(err, WebAuthnConfigError::Missing(ENV_ORIGIN));
    }

    #[test]
    fn localhost_defaults_apply_only_in_development() {
        let cfg = RelyingPartyConfig::from_lookup(lookup(&[("RUST_ENV", "development")]))
            .expect("development defaults");
        assert_eq!(cfg.rp_id, "localhost");
        assert_eq!(cfg.origin, "http://localhost:3000");

        let err =
            RelyingPartyConfig::from_lookup(lookup(&[("RUST_ENV", "production")])).unwrap_err();
        assert_eq!(err, WebAuthnConfigError::Missing(ENV_RP_ID));
    }

    #[test]
    fn rp_id_must_be_a_label_aligned_suffix_of_origin_host() {
        assert!(matches!(
            production("evil.com", "https://app.example.com"),
            Err(WebAuthnConfigError::RpIdMismatch { .. })
        ));
        // "xample.com" is a string suffix of "example.com" but not a label-aligned one.
        assert!(matches!(
            production("xample.com", "https://example.com"),
            Err(WebAuthnConfigError::RpIdMismatch { .. })
        ));
        assert!(matches!(
            production("", "https://example.com"),
            Err(WebAuthnConfigError::RpIdMismatch { .. })
        ));
    }

    #[test]
    fn parent_domain_rp_id_is_accepted() {
        assert!(production("example.com", "https://a.b.example.com:8443").is_ok());
    }

    #[test]
    fn production_rejects_insecure_or_malformed_origins() {
        for origin in [
            "http://app.example.com",
            "https://app.example.com/login",
            "https://app.example.com?x=1",
            "https://user@app.example.com",
            "app.example.com",
            "https://",
        ] {
            assert!(
                matches!(
                    production("example.com", origin),
                    Err(WebAuthnConfigError::InvalidOrigin(_))
                ),
                "expected {origin} to be rejected"
            );
        }
    }

    #[test]
    fn localhost_over_http_is_allowed_for_local_testing() {
        production("localhost", "http://localhost:3000")
            .expect("localhost over http is a secure context");
    }
}
