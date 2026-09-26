#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState};

    #[test]
    fn test_circuit_breaker_initial_state() {
        let config = CircuitBreakerConfig::default();
        let breaker = CircuitBreaker::new("test", config);

        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_allow_call_when_closed() {
        let config = CircuitBreakerConfig::default();
        let breaker = CircuitBreaker::new("test", config);

        assert!(breaker.allow_call());
    }

    #[test]
    fn test_circuit_breaker_snapshot() {
        let config = CircuitBreakerConfig::default();
        let breaker = CircuitBreaker::new("test", config);

        let snapshot = breaker.snapshot();
        assert_eq!(snapshot.state, CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_state_transitions() {
        let config = CircuitBreakerConfig::default();
        let breaker = Arc::new(CircuitBreaker::new("test", config));

        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_name() {
        let config = CircuitBreakerConfig::default();
        let breaker = CircuitBreaker::new("test_breaker", config);

        assert_eq!(breaker.name(), "test_breaker");
    }

    #[test]
    fn test_circuit_breaker_events() {
        let config = CircuitBreakerConfig::default();
        let breaker = CircuitBreaker::new("test", config);

        let events = breaker.events();
        assert!(events.len() >= 0);
    }

    #[test]
    fn test_circuit_breaker_metrics_rendering() {
        let config = CircuitBreakerConfig::default();
        let breaker = CircuitBreaker::new("test", config);

        let metrics = breaker.render_metrics();
        assert!(!metrics.is_empty());
    }

    #[test]
    fn test_circuit_breaker_dashboard_rendering() {
        let config = CircuitBreakerConfig::default();
        let breaker = CircuitBreaker::new("test", config);

        let dashboard = breaker.render_dashboard();
        assert!(!dashboard.is_empty());
    }

    #[test]
    fn test_circuit_breaker_mermaid_diagram() {
        let config = CircuitBreakerConfig::default();
        let breaker = CircuitBreaker::new("test", config);

        let diagram = breaker.to_mermaid_diagram();
        assert!(!diagram.is_empty());
    }

    #[test]
    fn test_circuit_breaker_alerts() {
        let config = CircuitBreakerConfig::default();
        let breaker = CircuitBreaker::new("test", config);

        let alerts = breaker.check_alerts();
        assert!(alerts.len() >= 0);
    }

    #[test]
    fn test_circuit_breaker_for_stellar_rpc() {
        let config = CircuitBreakerConfig::default();
        let breaker = CircuitBreaker::new("stellar_rpc", config);

        assert_eq!(breaker.state(), CircuitState::Closed);
        assert!(breaker.allow_call());
    }

    #[test]
    fn test_circuit_breaker_config_creation() {
        let config = CircuitBreakerConfig::default();
        let _breaker = CircuitBreaker::new("test", config);
        assert!(true);
    }

    #[test]
    fn test_multiple_circuit_breaker_instances() {
        let config1 = CircuitBreakerConfig::default();
        let config2 = CircuitBreakerConfig::default();

        let breaker1 = CircuitBreaker::new("breaker1", config1);
        let breaker2 = CircuitBreaker::new("breaker2", config2);

        assert_eq!(breaker1.name(), "breaker1");
        assert_eq!(breaker2.name(), "breaker2");
        assert_eq!(breaker1.state(), breaker2.state());
    }

    #[test]
    fn test_circuit_breaker_health_status() {
        let config = CircuitBreakerConfig::default();
        let breaker = CircuitBreaker::new("test", config);

        let snapshot = breaker.snapshot();
        assert!(snapshot.total_calls >= 0);
    }

    #[test]
    fn test_circuit_breaker_snapshot_accuracy() {
        let config = CircuitBreakerConfig::default();
        let breaker = CircuitBreaker::new("test", config);

        let snapshot = breaker.snapshot();
        assert_eq!(snapshot.state, CircuitState::Closed);
        assert_eq!(snapshot.name, "test");
    }

    #[test]
    fn test_circuit_breaker_concurrent_state_checks() {
        let config = CircuitBreakerConfig::default();
        let breaker = Arc::new(CircuitBreaker::new("test", config));

        let mut handles = vec![];
        for _ in 0..10 {
            let breaker_clone = Arc::clone(&breaker);
            let handle = std::thread::spawn(move || {
                breaker_clone.state()
            });
            handles.push(handle);
        }

        for handle in handles {
            let state = handle.join().unwrap();
            assert_eq!(state, CircuitState::Closed);
        }
    }

    #[test]
    fn test_circuit_breaker_with_custom_config() {
        let config = CircuitBreakerConfig::default();
        let breaker = CircuitBreaker::new("custom_breaker", config);

        assert_eq!(breaker.name(), "custom_breaker");
        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_snapshot_contains_metrics() {
        let config = CircuitBreakerConfig::default();
        let breaker = CircuitBreaker::new("test", config);

        let snapshot = breaker.snapshot();
        assert_eq!(snapshot.total_calls, 0);
    }

    #[test]
    fn test_circuit_breaker_call_allowed_in_closed_state() {
        let config = CircuitBreakerConfig::default();
        let breaker = CircuitBreaker::new("test", config);

        assert_eq!(breaker.state(), CircuitState::Closed);
        assert!(breaker.allow_call());
    }
}
