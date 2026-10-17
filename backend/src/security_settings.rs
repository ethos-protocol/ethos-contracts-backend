use std::collections::HashMap;

pub fn is_production() -> bool {
    ["APP_ENV", "ENVIRONMENT", "NODE_ENV"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .any(|value| value.eq_ignore_ascii_case("production"))
}

pub fn validate_startup() -> Result<(), String> {
    if !is_production() {
        return Ok(());
    }

    let mut settings: HashMap<String, String> = [
        "ADMIN_API_KEY",
        "RECAPTCHA_SECRET_KEY",
        "FIELD_ENCRYPTION_KEY_VERSION",
        "WEBAUTHN_RP_ID",
        "WEBAUTHN_ORIGIN",
    ]
    .iter()
    .filter_map(|name| std::env::var(name).ok().map(|value| ((*name).to_string(), value)))
    .collect();
    let active_version = settings
        .get("FIELD_ENCRYPTION_KEY_VERSION")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(1);
    let active_key = format!("FIELD_ENCRYPTION_KEY_{active_version}");
    if let Ok(value) = std::env::var(&active_key) {
        settings.insert(active_key, value);
    }
    validate_production_settings(&settings)
}

fn validate_production_settings(settings: &HashMap<String, String>) -> Result<(), String> {
    let active_version = settings
        .get("FIELD_ENCRYPTION_KEY_VERSION")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(1);
    let required = [
        "ADMIN_API_KEY".to_string(),
        "RECAPTCHA_SECRET_KEY".to_string(),
        format!("FIELD_ENCRYPTION_KEY_{active_version}"),
        "WEBAUTHN_RP_ID".to_string(),
        "WEBAUTHN_ORIGIN".to_string(),
    ];
    let missing: Vec<_> = required
        .iter()
        .filter(|name| {
            settings
                .get(name.as_str())
                .is_none_or(|value| value.trim().is_empty())
        })
        .cloned()
        .collect();

    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "required production security settings are missing or empty: {}",
            missing.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_requires_each_security_setting() {
        let error = validate_production_settings(&HashMap::new()).unwrap_err();
        for name in [
            "ADMIN_API_KEY",
            "RECAPTCHA_SECRET_KEY",
            "FIELD_ENCRYPTION_KEY_1",
            "WEBAUTHN_RP_ID",
            "WEBAUTHN_ORIGIN",
        ] {
            assert!(error.contains(name), "missing {name} in error: {error}");
        }
    }

    #[test]
    fn production_requires_key_for_active_encryption_version() {
        let settings = HashMap::from([
            ("ADMIN_API_KEY".into(), "admin-secret".into()),
            ("RECAPTCHA_SECRET_KEY".into(), "captcha-secret".into()),
            ("FIELD_ENCRYPTION_KEY_VERSION".into(), "3".into()),
            ("FIELD_ENCRYPTION_KEY_1".into(), "old-key".into()),
            ("WEBAUTHN_RP_ID".into(), "example.com".into()),
            ("WEBAUTHN_ORIGIN".into(), "https://example.com".into()),
        ]);

        let error = validate_production_settings(&settings).unwrap_err();
        assert!(error.contains("FIELD_ENCRYPTION_KEY_3"));
    }

    #[test]
    fn production_security_settings_accept_complete_configuration() {
        let settings = HashMap::from([
            ("ADMIN_API_KEY".into(), "admin-secret".into()),
            ("RECAPTCHA_SECRET_KEY".into(), "captcha-secret".into()),
            ("FIELD_ENCRYPTION_KEY_VERSION".into(), "2".into()),
            ("FIELD_ENCRYPTION_KEY_2".into(), "base64-key".into()),
            ("WEBAUTHN_RP_ID".into(), "example.com".into()),
            ("WEBAUTHN_ORIGIN".into(), "https://example.com".into()),
        ]);

        assert!(validate_production_settings(&settings).is_ok());
    }
}
