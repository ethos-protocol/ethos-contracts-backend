//! Dynamic Access Control List (ACL) management (issue: "ACLs are static").
//!
//! Rules used to be baked into static config. This module replaces that with
//! a runtime-managed store: rules can be added/removed through the admin
//! API, are enforced on the very next request (no restart/reload needed),
//! and every mutation is recorded in an audit trail.

use std::sync::{Arc, RwLock};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The effect of a matching ACL rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AclEffect {
    Allow,
    Deny,
}

/// A single dynamic ACL rule. `"*"` is a wildcard for subject/resource/action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AclRule {
    pub id: String,
    pub subject: String,
    pub resource: String,
    pub action: String,
    pub effect: AclEffect,
    pub created_at: DateTime<Utc>,
    pub created_by: Option<String>,
}

/// One entry in the ACL audit trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AclAuditEntry {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub operation: String,
    pub rule_id: String,
    pub actor: Option<String>,
    pub detail: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateAclRuleRequest {
    pub subject: String,
    pub resource: String,
    pub action: String,
    pub effect: AclEffect,
    #[serde(default)]
    pub actor: Option<String>,
}

#[derive(Default)]
struct AclInner {
    rules: Vec<AclRule>,
    audit_log: Vec<AclAuditEntry>,
}

/// Shared, thread-safe ACL store. Reads/writes take effect immediately -
/// there is no caching layer or reload step between a mutation and
/// enforcement.
#[derive(Default)]
pub struct AclStore {
    inner: RwLock<AclInner>,
}

impl AclStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn add_rule(&self, req: CreateAclRuleRequest) -> AclRule {
        let rule = AclRule {
            id: Uuid::new_v4().to_string(),
            subject: req.subject,
            resource: req.resource,
            action: req.action,
            effect: req.effect,
            created_at: Utc::now(),
            created_by: req.actor.clone(),
        };

        let mut inner = self.inner.write().expect("acl lock poisoned");
        inner.rules.push(rule.clone());
        inner.audit_log.push(AclAuditEntry {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            operation: "add".to_string(),
            rule_id: rule.id.clone(),
            actor: req.actor,
            detail: format!(
                "subject={} resource={} action={} effect={:?}",
                rule.subject, rule.resource, rule.action, rule.effect
            ),
        });
        rule
    }

    pub fn remove_rule(&self, rule_id: &str, actor: Option<String>) -> bool {
        let mut inner = self.inner.write().expect("acl lock poisoned");
        let before = inner.rules.len();
        inner.rules.retain(|r| r.id != rule_id);
        let removed = inner.rules.len() != before;
        if removed {
            inner.audit_log.push(AclAuditEntry {
                id: Uuid::new_v4().to_string(),
                timestamp: Utc::now(),
                operation: "remove".to_string(),
                rule_id: rule_id.to_string(),
                actor,
                detail: format!("rule {rule_id} removed"),
            });
        }
        removed
    }

    pub fn list_rules(&self) -> Vec<AclRule> {
        self.inner.read().expect("acl lock poisoned").rules.clone()
    }

    pub fn audit_trail(&self) -> Vec<AclAuditEntry> {
        self.inner
            .read()
            .expect("acl lock poisoned")
            .audit_log
            .clone()
    }

    /// Evaluate whether `subject` may perform `action` on `resource`.
    ///
    /// Deny rules always win over allow rules, so a freshly added deny is
    /// enforced immediately even if a broader allow rule already exists.
    /// With no matching rule the request is allowed, preserving the
    /// previous (static, permissive) behavior for anything not explicitly
    /// governed.
    pub fn is_allowed(&self, subject: &str, resource: &str, action: &str) -> bool {
        let inner = self.inner.read().expect("acl lock poisoned");
        let matches = |rule: &&AclRule| {
            (rule.subject == "*" || rule.subject == subject)
                && (rule.resource == "*" || resource.starts_with(rule.resource.as_str()))
                && (rule.action == "*" || rule.action.eq_ignore_ascii_case(action))
        };

        !inner
            .rules
            .iter()
            .filter(matches)
            .any(|r| r.effect == AclEffect::Deny)
    }
}

/// Axum middleware that enforces the dynamic ACL store on every request it
/// is applied to. Add it as a `.layer(middleware::from_fn_with_state(...))`
/// on any router that needs runtime-managed access control.
pub async fn acl_enforce_middleware(
    State(store): State<Arc<AclStore>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let subject = req
        .headers()
        .get("x-user-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("anonymous")
        .to_string();
    let resource = req.uri().path().to_string();
    let action = req.method().to_string();

    if !store.is_allowed(&subject, &resource, &action) {
        return (StatusCode::FORBIDDEN, "denied by ACL policy").into_response();
    }

    next.run(req).await
}

/// `POST /admin/acl` - create a new dynamic ACL rule.
pub async fn create_acl_rule(
    State(store): State<Arc<AclStore>>,
    Json(payload): Json<CreateAclRuleRequest>,
) -> impl IntoResponse {
    let rule = store.add_rule(payload);
    (StatusCode::CREATED, Json(rule))
}

/// `GET /admin/acl` - list all currently active ACL rules.
pub async fn list_acl_rules(State(store): State<Arc<AclStore>>) -> impl IntoResponse {
    Json(store.list_rules())
}

/// `DELETE /admin/acl/:id` - remove an ACL rule; takes effect immediately.
pub async fn delete_acl_rule(
    State(store): State<Arc<AclStore>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if store.remove_rule(&id, None) {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

/// `GET /admin/acl/audit` - retrieve the full ACL change audit trail.
pub async fn acl_audit_trail(State(store): State<Arc<AclStore>>) -> impl IntoResponse {
    Json(store.audit_trail())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule_req(
        subject: &str,
        resource: &str,
        action: &str,
        effect: AclEffect,
    ) -> CreateAclRuleRequest {
        CreateAclRuleRequest {
            subject: subject.to_string(),
            resource: resource.to_string(),
            action: action.to_string(),
            effect,
            actor: Some("test-admin".to_string()),
        }
    }

    #[test]
    fn default_allows_when_no_rules() {
        let store = AclStore::default();
        assert!(store.is_allowed("alice", "/api/vaults", "GET"));
    }

    #[test]
    fn deny_rule_blocks_matching_request_immediately() {
        let store = AclStore::default();
        store.add_rule(rule_req("alice", "/api/admin", "*", AclEffect::Deny));
        assert!(!store.is_allowed("alice", "/api/admin/users", "GET"));
        assert!(store.is_allowed("bob", "/api/admin/users", "GET"));
    }

    #[test]
    fn removing_a_rule_restores_default_allow_immediately() {
        let store = AclStore::default();
        let rule = store.add_rule(rule_req("alice", "/api/admin", "*", AclEffect::Deny));
        assert!(!store.is_allowed("alice", "/api/admin", "GET"));

        assert!(store.remove_rule(&rule.id, Some("test-admin".to_string())));
        assert!(store.is_allowed("alice", "/api/admin", "GET"));
    }

    #[test]
    fn audit_trail_records_add_and_remove() {
        let store = AclStore::default();
        let rule = store.add_rule(rule_req("carol", "/api/vaults", "POST", AclEffect::Allow));
        store.remove_rule(&rule.id, Some("root".to_string()));

        let trail = store.audit_trail();
        assert_eq!(trail.len(), 2);
        assert_eq!(trail[0].operation, "add");
        assert_eq!(trail[1].operation, "remove");
    }

    #[test]
    fn unknown_rule_removal_is_not_audited() {
        let store = AclStore::default();
        assert!(!store.remove_rule("does-not-exist", None));
        assert!(store.audit_trail().is_empty());
    }

    // ── Permission inheritance / resolution-order tests (#399) ─────────────
    //
    // The ACL resolution order is:
    //   1. Collect all rules whose subject/resource/action globs match the
    //      request.
    //   2. If ANY matching rule has effect=Deny → DENY (deny always wins).
    //   3. Otherwise → ALLOW (or default-allow when no rules match at all).
    //
    // "Inheritance" in this codebase is modelled via wildcard subjects:
    //   - A rule with subject="*" applies to every principal (base/parent role).
    //   - A rule with subject="alice" applies only to alice (specific override).
    //   - A deny on a more-specific subject overrides a wildcard allow, and
    //     vice-versa (a wildcard deny overrides a specific allow).
    //
    // This mirrors a two-level inheritance chain:
    //   parent role  → wildcard rule  ("*")
    //   child role   → subject rule   ("alice")

    /// A global allow ("*") combined with a per-user deny: deny wins.
    /// This exercises the "explicit deny always overrides inherited allow" rule.
    #[test]
    fn explicit_deny_overrides_inherited_wildcard_allow() {
        let store = AclStore::default();
        // Parent / inherited allow: everyone may GET /api/reports
        store.add_rule(rule_req("*", "/api/reports", "GET", AclEffect::Allow));
        // Per-user deny for alice on the same resource
        store.add_rule(rule_req("alice", "/api/reports", "GET", AclEffect::Deny));

        // alice's explicit deny overrides the wildcard allow
        assert!(!store.is_allowed("alice", "/api/reports", "GET"));
        // bob only has the wildcard allow — should be permitted
        assert!(store.is_allowed("bob", "/api/reports", "GET"));
    }

    /// A per-user allow does NOT save a principal when a wildcard deny is present.
    /// A global deny ("*") cannot be escaped by a subject-specific allow.
    #[test]
    fn wildcard_deny_overrides_specific_subject_allow() {
        let store = AclStore::default();
        // Global deny: nobody may DELETE /api/vaults
        store.add_rule(rule_req("*", "/api/vaults", "DELETE", AclEffect::Deny));
        // Per-user allow: alice explicitly granted DELETE
        store.add_rule(rule_req("alice", "/api/vaults", "DELETE", AclEffect::Allow));

        // Global deny wins even though alice has a specific allow
        assert!(!store.is_allowed("alice", "/api/vaults", "DELETE"));
        assert!(!store.is_allowed("bob", "/api/vaults", "DELETE"));
    }

    /// Two-level inheritance chain:
    ///   Level 1 (grandparent): wildcard allow for everything
    ///   Level 2 (parent role): wildcard deny for admin area
    ///   Level 3 (child role / specific user): explicit allow for one admin path
    ///
    /// Expected: deny at level 2 still wins for alice; bob is also denied.
    #[test]
    fn two_level_inheritance_deny_beats_allow_at_every_level() {
        let store = AclStore::default();
        // Level 1 – everyone may access everything
        store.add_rule(rule_req("*", "/", "GET", AclEffect::Allow));
        // Level 2 – no one may access /api/admin
        store.add_rule(rule_req("*", "/api/admin", "*", AclEffect::Deny));
        // Level 3 – alice has a targeted allow on /api/admin/users
        store.add_rule(rule_req(
            "alice",
            "/api/admin/users",
            "GET",
            AclEffect::Allow,
        ));

        // Level-2 wildcard deny covers /api/admin/* via prefix matching → alice
        // is still denied even with the level-3 allow.
        assert!(!store.is_allowed("alice", "/api/admin/users", "GET"));
        assert!(!store.is_allowed("bob", "/api/admin/users", "GET"));
        // Paths outside /api/admin are unaffected by the level-2 deny
        assert!(store.is_allowed("alice", "/api/vaults", "GET"));
    }

    /// A role with zero assigned rules falls back to the default-allow behaviour.
    #[test]
    fn role_with_no_assigned_rules_defaults_to_allow() {
        let store = AclStore::default();
        // Add some rules for other subjects — carol has none
        store.add_rule(rule_req("alice", "/api/admin", "*", AclEffect::Deny));
        store.add_rule(rule_req("bob", "/api/reports", "GET", AclEffect::Allow));

        // carol has no rules at all → default allow
        assert!(store.is_allowed("carol", "/api/vaults", "GET"));
        assert!(store.is_allowed("carol", "/api/admin", "POST"));
        assert!(store.is_allowed("carol", "/api/reports", "DELETE"));
    }

    /// Three-level wildcard inheritance:
    ///   - Action wildcard ("*") on the parent covers all methods.
    ///   - Resource wildcard ("*") on the grandparent covers all resources.
    ///   - Subject wildcard ("*") on the root covers all principals.
    /// Confirms that a deny propagates correctly through all three wildcard
    /// dimensions simultaneously.
    #[test]
    fn three_level_wildcard_deny_blocks_all_principals_resources_actions() {
        let store = AclStore::default();
        // One absolute deny rule
        store.add_rule(rule_req("*", "*", "*", AclEffect::Deny));

        assert!(!store.is_allowed("anyone", "/any/path", "GET"));
        assert!(!store.is_allowed("anyone", "/any/path", "POST"));
        assert!(!store.is_allowed("root", "/api/admin", "DELETE"));
    }

    /// Removing a deny rule that was "inherited" (wildcard) immediately
    /// restores access for all principals.
    #[test]
    fn removing_inherited_deny_restores_access_for_all_principals() {
        let store = AclStore::default();
        let deny = store.add_rule(rule_req("*", "/api/beta", "*", AclEffect::Deny));

        assert!(!store.is_allowed("alice", "/api/beta/feature", "GET"));
        assert!(!store.is_allowed("bob", "/api/beta/feature", "GET"));

        store.remove_rule(&deny.id, Some("admin".to_string()));

        assert!(store.is_allowed("alice", "/api/beta/feature", "GET"));
        assert!(store.is_allowed("bob", "/api/beta/feature", "GET"));
    }
}
