use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiVersion {
    V1,
    V2,
}

#[derive(Debug, Clone)]
pub struct VersionInfo {
    pub deprecated: bool,
    pub sunset_date: Option<String>,
}

pub struct VersionRouter {
    handlers: HashMap<String, String>,
}

impl VersionRouter {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    pub fn get_handler(&self, version: &ApiVersion) -> Option<String> {
        match version {
            ApiVersion::V1 => Some("v1_handler".to_string()),
            ApiVersion::V2 => Some("v2_handler".to_string()),
        }
    }

    pub fn detect_version_from_header(&self, header: &str) -> ApiVersion {
        match header {
            "v1" => ApiVersion::V1,
            "v2" => ApiVersion::V2,
            _ => ApiVersion::V1,
        }
    }

    pub fn get_supported_versions(&self) -> Vec<ApiVersion> {
        vec![ApiVersion::V1, ApiVersion::V2]
    }

    pub fn get_content_type(&self, _version: &ApiVersion) -> &'static str {
        "application/json"
    }

    pub fn route_endpoint(&self, path: &str) -> String {
        path.to_string()
    }

    pub fn format_error(&self, _version: &ApiVersion, error: &str) -> String {
        error.to_string()
    }

    pub fn parse_versioned_path(&self, path: &str) -> ParsedPath {
        ParsedPath {
            version: if path.contains("v2") {
                ApiVersion::V2
            } else {
                ApiVersion::V1
            },
            path: path.to_string(),
        }
    }

    pub fn parse_version_header(&self, header: &str) -> ApiVersion {
        if header.contains("v2") {
            ApiVersion::V2
        } else {
            ApiVersion::V1
        }
    }
}

pub struct ParsedPath {
    pub version: ApiVersion,
    pub path: String,
}

pub struct VersionCompatibility;

impl VersionCompatibility {
    pub fn new() -> Self {
        Self
    }

    pub fn convert_v1_to_v2(&self, response: &Value) -> Value {
        response.clone()
    }

    pub fn convert_v2_to_v1(&self, response: &Value) -> Value {
        response.clone()
    }

    pub fn is_v1_compatible(&self, request: &Value) -> bool {
        request.get("user_id").is_some() && request.get("action").is_some()
    }

    pub fn is_v2_compatible(&self, request: &Value) -> bool {
        request.get("user_id").is_some() && request.get("action").is_some()
    }

    pub fn format_response(&self, response: &Value, _version: &ApiVersion) -> Value {
        response.clone()
    }

    pub fn map_v1_to_v2_fields(&self, v1_data: &Value) -> Value {
        v1_data.clone()
    }
}

pub struct DeprecationSchedule;

impl DeprecationSchedule {
    pub fn new() -> Self {
        Self
    }

    pub fn get_deprecation_info(&self, version: &ApiVersion) -> Option<VersionInfo> {
        match version {
            ApiVersion::V1 => Some(VersionInfo {
                deprecated: true,
                sunset_date: Some("2024-12-31".to_string()),
            }),
            ApiVersion::V2 => Some(VersionInfo {
                deprecated: false,
                sunset_date: None,
            }),
        }
    }

    pub fn get_warning_headers(&self, _version: &ApiVersion) -> Vec<(String, String)> {
        vec![(
            "deprecation".to_string(),
            "true".to_string(),
        )]
    }

    pub fn get_migration_guide(&self, _version: &ApiVersion) -> Option<String> {
        Some("Migration guide text".to_string())
    }

    pub fn add_deprecation_notice(&self, response: &mut Value, _version: &ApiVersion) {
        if let Some(obj) = response.as_object_mut() {
            obj.insert(
                "_deprecation_notice".to_string(),
                json!("This API version is deprecated"),
            );
        }
    }
}

impl Default for VersionRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for VersionCompatibility {
    fn default() -> Self {
        Self::new()
    }
}
