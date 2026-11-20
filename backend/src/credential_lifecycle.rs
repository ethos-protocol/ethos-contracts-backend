//! Credential lifecycle state machine.
//!
//! This module owns the credential lifecycle transitions and records
//! observability metrics for every state change. The
//! `ethos_protocol_credential_lifecycle_transitions_total` counter is
//! incremented at each transition site so the Grafana credential lifecycle
//! dashboard panels populate.

use std::sync::Arc;

use crate::metrics::Metrics;

/// The lifecycle states a credential can be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CredentialState {
    /// Credential has been issued but not yet activated.
    Issued,
    /// Credential is active and usable.
    Active,
    /// Credential has been suspended.
    Suspended,
    /// Credential has been revoked.
    Revoked,
    /// Credential has expired.
    Expired,
}

impl CredentialState {
    /// Returns the canonical label used for metrics.
    pub fn as_label(&self) -> &'static str {
        match self {
            CredentialState::Issued => "issued",
            CredentialState::Active => "active",
            CredentialState::Suspended => "suspended",
            CredentialState::Revoked => "revoked",
            CredentialState::Expired => "expired",
        }
    }
}

impl std::fmt::Display for CredentialState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_label())
    }
}

/// Errors that can occur during a credential lifecycle transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleError {
    /// The requested transition is not permitted from the current state.
    InvalidTransition {
        from: CredentialState,
        to: CredentialState,
    },
    /// The credential could not be found.
    NotFound(String),
    /// Any other lifecycle failure.
    Other(String),
}

impl std::fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LifecycleError::InvalidTransition { from, to } => {
                write!(f, "invalid credential transition from {from} to {to}")
            }
            LifecycleError::NotFound(id) => write!(f, "credential not found: {id}"),
            LifecycleError::Other(msg) => write!(f, "credential lifecycle error: {msg}"),
        }
    }
}

impl std::error::Error for LifecycleError {}

/// Returns whether a transition from `from` to `to` is permitted.
pub fn is_valid_transition(from: CredentialState, to: CredentialState) -> bool {
    use CredentialState::*;
    matches!(
        (from, to),
        (Issued, Active)
            | (Issued, Revoked)
            | (Issued, Expired)
            | (Active, Suspended)
            | (Active, Revoked)
            | (Active, Expired)
            | (Suspended, Active)
            | (Suspended, Revoked)
            | (Suspended, Expired)
    )
}

/// Records a successful credential lifecycle transition.
///
/// Increments `ethos_protocol_credential_lifecycle_transitions_total` with
/// `from_state` / `to_state` labels so the Grafana dashboard panels populate.
pub fn record_transition(
    metrics: &Arc<Metrics>,
    from: CredentialState,
    to: CredentialState,
) {
    metrics
        .credential_lifecycle_transitions_total
        .with_label_values(&[from.as_label(), to.as_label()])
        .inc();
}

/// Records a failed credential lifecycle transition.
///
/// Increments the lifecycle errors counter so failure paths are observable.
pub fn record_transition_error(
    metrics: &Arc<Metrics>,
    from: CredentialState,
    to: CredentialState,
    reason: &str,
) {
    metrics
        .credential_lifecycle_errors_total
        .with_label_values(&[from.as_label(), to.as_label(), reason])
        .inc();
}

/// Performs a credential lifecycle transition, validating it and recording
/// the appropriate metrics.
///
/// On success the transitions counter is incremented. On failure the errors
/// counter is incremented and the error is returned.
pub fn transition(
    metrics: &Arc<Metrics>,
    from: CredentialState,
    to: CredentialState,
) -> Result<CredentialState, LifecycleError> {
    if !is_valid_transition(from, to) {
        record_transition_error(metrics, from, to, "invalid_transition");
        return Err(LifecycleError::InvalidTransition { from, to });
    }

    record_transition(metrics, from, to);
    Ok(to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_transitions_are_accepted() {
        assert!(is_valid_transition(
            CredentialState::Issued,
            CredentialState::Active
        ));
        assert!(is_valid_transition(
            CredentialState::Active,
            CredentialState::Suspended
        ));
        assert!(is_valid_transition(
            CredentialState::Suspended,
            CredentialState::Active
        ));
        assert!(is_valid_transition(
            CredentialState::Active,
            CredentialState::Revoked
        ));
    }

    #[test]
    fn invalid_transitions_are_rejected() {
        assert!(!is_valid_transition(
            CredentialState::Revoked,
            CredentialState::Active
        ));
        assert!(!is_valid_transition(
            CredentialState::Expired,
            CredentialState::Active
        ));
        assert!(!is_valid_transition(
            CredentialState::Issued,
            CredentialState::Suspended
        ));
    }

    #[test]
    fn state_labels_are_stable() {
        assert_eq!(CredentialState::Issued.as_label(), "issued");
        assert_eq!(CredentialState::Active.as_label(), "active");
        assert_eq!(CredentialState::Suspended.as_label(), "suspended");
        assert_eq!(CredentialState::Revoked.as_label(), "revoked");
        assert_eq!(CredentialState::Expired.as_label(), "expired");
    }
}