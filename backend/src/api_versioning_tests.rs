#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use axum::http::StatusCode;
    use serde_json::json;

    use crate::api_versioning::{
        ApiVersion, VersionCompatibility, VersionRouter, DeprecationSchedule,
    };

    #[test]
    fn test_api_version_routing_v1() {
        let router = VersionRouter::new();
        let request_version = ApiVersion::V1;

        let handler = router.get_handler(&request_version);
        assert!(handler.is_some());
    }

    #[test]
    fn test_api_version_routing_v2() {
        let router = VersionRouter::new();
        let request_version = ApiVersion::V2;

        let handler = router.get_handler(&request_version);
        assert!(handler.is_some());
    }

    #[test]
    fn test_version_compatibility_layer_v1_to_v2() {
        let compat = VersionCompatibility::new();
        let v1_response = json!({
            "id": "123",
            "name": "test",
            "status": "active"
        });

        let v2_response = compat.convert_v1_to_v2(&v1_response);
        assert!(v2_response.get("id").is_some());
        assert!(v2_response.get("name").is_some());
    }

    #[test]
    fn test_version_compatibility_layer_v2_to_v1() {
        let compat = VersionCompatibility::new();
        let v2_response = json!({
            "id": "123",
            "name": "test",
            "metadata": {
                "status": "active"
            }
        });

        let v1_response = compat.convert_v2_to_v1(&v2_response);
        assert!(v1_response.get("id").is_some());
        assert!(v1_response.get("name").is_some());
    }

    #[test]
    fn test_api_version_detection_from_header() {
        let router = VersionRouter::new();
        let version = router.detect_version_from_header("v1");
        assert_eq!(version, ApiVersion::V1);

        let version = router.detect_version_from_header("v2");
        assert_eq!(version, ApiVersion::V2);
    }

    #[test]
    fn test_api_version_default_fallback() {
        let router = VersionRouter::new();
        let version = router.detect_version_from_header("invalid");
        assert_eq!(version, ApiVersion::V1);
    }

    #[test]
    fn test_deprecation_schedule_v1() {
        let schedule = DeprecationSchedule::new();
        let deprecation_info = schedule.get_deprecation_info(&ApiVersion::V1);

        assert!(deprecation_info.is_some());
        assert!(deprecation_info.unwrap().deprecated);
    }

    #[test]
    fn test_deprecation_schedule_v2() {
        let schedule = DeprecationSchedule::new();
        let deprecation_info = schedule.get_deprecation_info(&ApiVersion::V2);

        assert!(deprecation_info.is_some());
        assert!(!deprecation_info.unwrap().deprecated);
    }

    #[test]
    fn test_deprecation_schedule_sunset_date() {
        let schedule = DeprecationSchedule::new();
        let deprecation_info = schedule.get_deprecation_info(&ApiVersion::V1);

        assert!(deprecation_info.is_some());
        assert!(deprecation_info.unwrap().sunset_date.is_some());
    }

    #[test]
    fn test_deprecation_warning_headers() {
        let schedule = DeprecationSchedule::new();
        let headers = schedule.get_warning_headers(&ApiVersion::V1);

        assert!(!headers.is_empty());
        assert!(headers.iter().any(|h| h.0.contains("deprecation")));
    }

    #[test]
    fn test_version_specific_endpoint_routing() {
        let router = VersionRouter::new();
        let path_v1 = router.route_endpoint("/api/v1/check-ins");
        let path_v2 = router.route_endpoint("/api/v2/check-ins");

        assert_ne!(path_v1, path_v2);
    }

    #[test]
    fn test_backward_compatibility_v1_field_support() {
        let compat = VersionCompatibility::new();
        let v1_request = json!({
            "user_id": "123",
            "action": "check-in"
        });

        assert!(compat.is_v1_compatible(&v1_request));
    }

    #[test]
    fn test_forward_compatibility_v2_field_support() {
        let compat = VersionCompatibility::new();
        let v2_request = json!({
            "user_id": "123",
            "action": "check-in",
            "metadata": {
                "source": "mobile"
            }
        });

        assert!(compat.is_v2_compatible(&v2_request));
    }

    #[test]
    fn test_version_specific_error_messages() {
        let router = VersionRouter::new();
        let v1_error = router.format_error(&ApiVersion::V1, "Invalid request");
        let v2_error = router.format_error(&ApiVersion::V2, "Invalid request");

        assert!(v1_error.contains("Invalid request"));
        assert!(v2_error.contains("Invalid request"));
    }

    #[test]
    fn test_multiple_version_support() {
        let router = VersionRouter::new();
        let versions = router.get_supported_versions();

        assert!(versions.contains(&ApiVersion::V1));
        assert!(versions.contains(&ApiVersion::V2));
    }

    #[test]
    fn test_content_negotiation_by_version() {
        let router = VersionRouter::new();
        let content_type_v1 = router.get_content_type(&ApiVersion::V1);
        let content_type_v2 = router.get_content_type(&ApiVersion::V2);

        assert_eq!(content_type_v1, "application/json");
        assert_eq!(content_type_v2, "application/json");
    }

    #[test]
    fn test_version_migration_guide() {
        let schedule = DeprecationSchedule::new();
        let migration_guide = schedule.get_migration_guide(&ApiVersion::V1);

        assert!(migration_guide.is_some());
        assert!(!migration_guide.unwrap().is_empty());
    }

    #[test]
    fn test_v1_check_in_response_format() {
        let compat = VersionCompatibility::new();
        let response = json!({
            "id": "check-in-123",
            "user_id": "user-456",
            "timestamp": "2023-09-25T10:30:00Z",
            "status": "completed"
        });

        let v1_response = compat.format_response(&response, &ApiVersion::V1);
        assert!(v1_response.get("id").is_some());
        assert!(v1_response.get("status").is_some());
    }

    #[test]
    fn test_v2_check_in_response_format() {
        let compat = VersionCompatibility::new();
        let response = json!({
            "id": "check-in-123",
            "user_id": "user-456",
            "timestamp": "2023-09-25T10:30:00Z",
            "status": "completed",
            "metadata": {
                "location": "building-a",
                "device": "mobile"
            }
        });

        let v2_response = compat.format_response(&response, &ApiVersion::V2);
        assert!(v2_response.get("id").is_some());
        assert!(v2_response.get("metadata").is_some());
    }

    #[test]
    fn test_version_routing_with_path_parameters() {
        let router = VersionRouter::new();
        let path = "/api/v1/users/:id/profile";
        let parsed = router.parse_versioned_path(path);

        assert_eq!(parsed.version, ApiVersion::V1);
        assert!(parsed.path.contains("users"));
    }

    #[test]
    fn test_version_compatibility_field_mapping() {
        let compat = VersionCompatibility::new();
        let v1_data = json!({
            "check_in_id": "123",
            "user_id": "456"
        });

        let mapped = compat.map_v1_to_v2_fields(&v1_data);
        assert!(mapped.get("id").is_some() || mapped.get("check_in_id").is_some());
    }

    #[test]
    fn test_version_deprecation_notice_in_response() {
        let schedule = DeprecationSchedule::new();
        let mut response = json!({"data": "test"});

        schedule.add_deprecation_notice(&mut response, &ApiVersion::V1);
        assert!(response.get("_deprecation_notice").is_some());
    }

    #[test]
    fn test_api_version_header_parsing() {
        let router = VersionRouter::new();
        let version = router.parse_version_header("application/vnd.api+json;version=v2");
        assert_eq!(version, ApiVersion::V2);
    }

    #[test]
    fn test_concurrent_version_requests() {
        let router = std::sync::Arc::new(VersionRouter::new());
        let mut handles = vec![];

        for i in 0..10 {
            let router_clone = std::sync::Arc::clone(&router);
            let handle = std::thread::spawn(move || {
                let version = if i % 2 == 0 {
                    ApiVersion::V1
                } else {
                    ApiVersion::V2
                };
                router_clone.get_handler(&version)
            });
            handles.push(handle);
        }

        for handle in handles {
            let result = handle.join();
            assert!(result.is_ok());
            assert!(result.unwrap().is_some());
        }
    }
}
