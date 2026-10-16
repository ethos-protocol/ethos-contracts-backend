//! Uptime monitoring (issue #601: "Implement Uptime Monitoring").
//!
//! Provides:
//! - Uptime checks recorded from multiple probe locations
//!   (`POST /uptime/checks`), or run locally via [`probe`].
//! - Uptime percentage calculation, overall and per location.
//! - Downtime incident detection (contiguous failing checks).
//! - Downtime alerts when a location has failed N consecutive checks.
//! - Uptime reports over a time window (`GET /uptime/report`).
//!
//! The backend never probes URLs supplied by API callers. Probes are run by
//! a trusted worker (see [`probe`]) and their results are pushed here, which
//! keeps the HTTP API free of server-side request forgery surface.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};

/// Maximum number of checks retained per target. Older checks are dropped.
pub const MAX_CHECKS_PER_TARGET: usize = 10_000;

/// Window used by reports when the caller does not specify one (24 hours).
pub const DEFAULT_REPORT_WINDOW_MINUTES: i64 = 24 * 60;

/// Consecutive failures required before a downtime alert fires by default.
pub const DEFAULT_CONSECUTIVE_FAILURES: u32 = 3;

/// A single availability check of one target from one probe location.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UptimeCheck {
    pub target: String,
    pub location: String,
    pub timestamp: DateTime<Utc>,
    pub up: bool,
    pub latency_ms: Option<u64>,
    pub status_code: Option<u16>,
}

/// A contiguous run of failing checks from one location.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DowntimeIncident {
    pub location: String,
    pub started_at: DateTime<Utc>,
    /// `None` while the incident is still open (latest check failed).
    pub resolved_at: Option<DateTime<Utc>>,
    pub failed_checks: usize,
}

/// Alert raised when a location has failed `consecutive_failures` checks in a row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DowntimeAlert {
    pub target: String,
    pub location: String,
    pub consecutive_failures: u32,
    pub last_checked_at: DateTime<Utc>,
}

/// Uptime figures for a single location.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LocationUptime {
    pub location: String,
    pub total_checks: usize,
    pub successful_checks: usize,
    pub uptime_percent: f64,
}

/// Uptime report for one target over a time window.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UptimeReport {
    pub target: String,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub total_checks: usize,
    pub successful_checks: usize,
    /// `None` when the window contains no checks.
    pub uptime_percent: Option<f64>,
    pub per_location: Vec<LocationUptime>,
    pub incidents: Vec<DowntimeIncident>,
}

/// Percentage of checks that were up. `None` when there are no checks.
pub fn uptime_percent(checks: &[UptimeCheck]) -> Option<f64> {
    if checks.is_empty() {
        return None;
    }
    let up = checks.iter().filter(|c| c.up).count();
    Some(percent(up, checks.len()))
}

fn percent(successful: usize, total: usize) -> f64 {
    if total == 0 {
        return 100.0;
    }
    successful as f64 / total as f64 * 100.0
}

/// Group checks by location, each group sorted oldest-first.
fn group_by_location(checks: &[UptimeCheck]) -> BTreeMap<&str, Vec<&UptimeCheck>> {
    let mut groups: BTreeMap<&str, Vec<&UptimeCheck>> = BTreeMap::new();
    for check in checks {
        groups
            .entry(check.location.as_str())
            .or_default()
            .push(check);
    }
    for group in groups.values_mut() {
        group.sort_by_key(|c| c.timestamp);
    }
    groups
}

/// Detect downtime incidents: each run of consecutive failed checks from one
/// location becomes one incident. Checks may be passed in any order.
pub fn detect_incidents(checks: &[UptimeCheck]) -> Vec<DowntimeIncident> {
    let mut incidents = Vec::new();
    for (location, group) in group_by_location(checks) {
        let mut open: Option<(DateTime<Utc>, usize)> = None;
        for check in group {
            if check.up {
                if let Some((started_at, failed_checks)) = open.take() {
                    incidents.push(DowntimeIncident {
                        location: location.to_string(),
                        started_at,
                        resolved_at: Some(check.timestamp),
                        failed_checks,
                    });
                }
            } else {
                match open.as_mut() {
                    Some((_, count)) => *count += 1,
                    None => open = Some((check.timestamp, 1)),
                }
            }
        }
        if let Some((started_at, failed_checks)) = open {
            incidents.push(DowntimeIncident {
                location: location.to_string(),
                started_at,
                resolved_at: None,
                failed_checks,
            });
        }
    }
    incidents
}

/// Evaluate downtime alerts. A location alerts when its most recent
/// `consecutive_failures` or more checks all failed.
pub fn evaluate_downtime_alerts(
    checks: &[UptimeCheck],
    consecutive_failures: u32,
) -> Vec<DowntimeAlert> {
    let threshold = consecutive_failures.max(1) as usize;
    let mut alerts = Vec::new();
    for (location, group) in group_by_location(checks) {
        let trailing = group.iter().rev().take_while(|c| !c.up).count();
        if trailing >= threshold {
            if let Some(last) = group.last() {
                alerts.push(DowntimeAlert {
                    target: last.target.clone(),
                    location: location.to_string(),
                    consecutive_failures: trailing as u32,
                    last_checked_at: last.timestamp,
                });
            }
        }
    }
    alerts
}

/// Build an uptime report from checks that already fall inside the window.
pub fn build_report(
    target: &str,
    checks: &[UptimeCheck],
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
) -> UptimeReport {
    let per_location = group_by_location(checks)
        .into_iter()
        .map(|(location, group)| {
            let successful = group.iter().filter(|c| c.up).count();
            LocationUptime {
                location: location.to_string(),
                total_checks: group.len(),
                successful_checks: successful,
                uptime_percent: percent(successful, group.len()),
            }
        })
        .collect();

    let successful_checks = checks.iter().filter(|c| c.up).count();
    UptimeReport {
        target: target.to_string(),
        window_start,
        window_end,
        total_checks: checks.len(),
        successful_checks,
        uptime_percent: uptime_percent(checks),
        per_location,
        incidents: detect_incidents(checks),
    }
}

#[derive(Default)]
struct Inner {
    checks: HashMap<String, Vec<UptimeCheck>>,
}

/// Shared in-memory store of uptime checks, keyed by target.
#[derive(Default)]
pub struct UptimeMonitor {
    inner: RwLock<Inner>,
}

impl UptimeMonitor {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Record one check, dropping the oldest once the per-target cap is hit.
    pub fn record(&self, check: UptimeCheck) {
        let mut inner = self.inner.write().expect("uptime lock poisoned");
        let series = inner.checks.entry(check.target.clone()).or_default();
        series.push(check);
        if series.len() > MAX_CHECKS_PER_TARGET {
            let excess = series.len() - MAX_CHECKS_PER_TARGET;
            series.drain(..excess);
        }
    }

    /// All checks for `target` with timestamps in `[start, end]`.
    pub fn checks_between(
        &self,
        target: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Vec<UptimeCheck> {
        self.inner
            .read()
            .expect("uptime lock poisoned")
            .checks
            .get(target)
            .map(|series| {
                series
                    .iter()
                    .filter(|c| c.timestamp >= start && c.timestamp <= end)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Report for the trailing `window` ending now.
    pub fn report(&self, target: &str, window: ChronoDuration) -> UptimeReport {
        let end = Utc::now();
        let start = end - window;
        let checks = self.checks_between(target, start, end);
        build_report(target, &checks, start, end)
    }

    /// Downtime alerts for `target` across all locations.
    pub fn alerts(&self, target: &str, consecutive_failures: u32) -> Vec<DowntimeAlert> {
        let checks = self.checks_between(target, DateTime::<Utc>::MIN_UTC, Utc::now());
        evaluate_downtime_alerts(&checks, consecutive_failures)
    }

    /// Targets that have at least one recorded check.
    pub fn targets(&self) -> Vec<String> {
        let mut targets: Vec<String> = self
            .inner
            .read()
            .expect("uptime lock poisoned")
            .checks
            .keys()
            .cloned()
            .collect();
        targets.sort();
        targets
    }
}

/// Run one availability probe against `target` from `location`.
///
/// A check is "up" when the target answers with a 2xx or 3xx status within
/// `timeout`. Connection errors and timeouts are recorded as down. Intended
/// for a trusted scheduler running in each probe location; the result should
/// be recorded via [`UptimeMonitor::record`] or `POST /uptime/checks`.
pub async fn probe(
    client: &reqwest::Client,
    target: &str,
    location: &str,
    timeout: Duration,
) -> UptimeCheck {
    let started = Instant::now();
    let response = client.get(target).timeout(timeout).send().await;
    let latency_ms = u64::try_from(started.elapsed().as_millis()).ok();

    let (up, status_code) = match response {
        Ok(resp) => (
            resp.status().is_success() || resp.status().is_redirection(),
            Some(resp.status().as_u16()),
        ),
        Err(_) => (false, None),
    };

    UptimeCheck {
        target: target.to_string(),
        location: location.to_string(),
        timestamp: Utc::now(),
        up,
        latency_ms,
        status_code,
    }
}

/// `POST /uptime/checks` - record a check reported by a probe location.
pub async fn record_uptime_check(
    State(monitor): State<Arc<UptimeMonitor>>,
    Json(check): Json<UptimeCheck>,
) -> impl IntoResponse {
    monitor.record(check);
    StatusCode::ACCEPTED
}

#[derive(Debug, Deserialize)]
pub struct ReportQuery {
    pub target: String,
    #[serde(default = "default_window_minutes")]
    pub window_minutes: i64,
}

fn default_window_minutes() -> i64 {
    DEFAULT_REPORT_WINDOW_MINUTES
}

/// `GET /uptime/report?target=&window_minutes=` - uptime report for a target.
pub async fn get_uptime_report(
    State(monitor): State<Arc<UptimeMonitor>>,
    Query(q): Query<ReportQuery>,
) -> impl IntoResponse {
    if q.window_minutes <= 0 {
        return (StatusCode::BAD_REQUEST, "window_minutes must be positive").into_response();
    }
    let report = monitor.report(&q.target, ChronoDuration::minutes(q.window_minutes));
    Json(report).into_response()
}

#[derive(Debug, Deserialize)]
pub struct AlertsQuery {
    pub target: String,
    #[serde(default = "default_consecutive")]
    pub consecutive: u32,
}

fn default_consecutive() -> u32 {
    DEFAULT_CONSECUTIVE_FAILURES
}

/// `GET /uptime/alerts?target=&consecutive=` - locations currently down.
pub async fn get_downtime_alerts(
    State(monitor): State<Arc<UptimeMonitor>>,
    Query(q): Query<AlertsQuery>,
) -> impl IntoResponse {
    Json(monitor.alerts(&q.target, q.consecutive))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 1, 10, minute, 0).unwrap()
    }

    fn check(location: &str, minute: u32, up: bool) -> UptimeCheck {
        UptimeCheck {
            target: "https://api.example".to_string(),
            location: location.to_string(),
            timestamp: at(minute),
            up,
            latency_ms: Some(12),
            status_code: Some(if up { 200 } else { 503 }),
        }
    }

    #[test]
    fn uptime_percent_is_none_without_checks() {
        assert_eq!(uptime_percent(&[]), None);
    }

    #[test]
    fn uptime_percent_counts_successful_checks() {
        let checks = vec![
            check("eu", 0, true),
            check("eu", 1, true),
            check("eu", 2, true),
            check("eu", 3, false),
        ];
        assert_eq!(uptime_percent(&checks), Some(75.0));
    }

    #[test]
    fn per_location_uptime_is_computed_independently() {
        let checks = vec![
            check("eu", 0, true),
            check("eu", 1, true),
            check("us", 0, true),
            check("us", 1, false),
        ];
        let report = build_report("t", &checks, at(0), at(59));
        let eu = report
            .per_location
            .iter()
            .find(|l| l.location == "eu")
            .unwrap();
        let us = report
            .per_location
            .iter()
            .find(|l| l.location == "us")
            .unwrap();
        assert_eq!(eu.uptime_percent, 100.0);
        assert_eq!(us.uptime_percent, 50.0);
        assert_eq!(report.total_checks, 4);
        assert_eq!(report.successful_checks, 3);
        assert_eq!(report.uptime_percent, Some(75.0));
    }

    #[test]
    fn contiguous_failures_form_one_incident() {
        let checks = vec![
            check("eu", 0, true),
            check("eu", 1, false),
            check("eu", 2, false),
            check("eu", 3, true),
            check("eu", 4, false),
        ];
        let incidents = detect_incidents(&checks);
        assert_eq!(incidents.len(), 2);
        assert_eq!(incidents[0].started_at, at(1));
        assert_eq!(incidents[0].resolved_at, Some(at(3)));
        assert_eq!(incidents[0].failed_checks, 2);
        assert_eq!(
            incidents[1].resolved_at, None,
            "still open at the latest check"
        );
        assert_eq!(incidents[1].failed_checks, 1);
    }

    #[test]
    fn incidents_accept_unsorted_input() {
        let checks = vec![
            check("eu", 2, false),
            check("eu", 0, true),
            check("eu", 1, false),
        ];
        let incidents = detect_incidents(&checks);
        assert_eq!(incidents.len(), 1);
        assert_eq!(incidents[0].started_at, at(1));
        assert_eq!(incidents[0].failed_checks, 2);
    }

    #[test]
    fn alert_fires_only_after_consecutive_failures() {
        let checks = vec![
            check("eu", 0, false),
            check("eu", 1, false),
            check("eu", 2, false),
        ];
        let alerts = evaluate_downtime_alerts(&checks, 3);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].consecutive_failures, 3);
        assert_eq!(alerts[0].location, "eu");
        assert!(evaluate_downtime_alerts(&checks, 4).is_empty());
    }

    #[test]
    fn recovered_location_does_not_alert() {
        let checks = vec![
            check("eu", 0, false),
            check("eu", 1, false),
            check("eu", 2, false),
            check("eu", 3, true),
        ];
        assert!(evaluate_downtime_alerts(&checks, 2).is_empty());
    }

    #[test]
    fn monitor_retains_only_the_newest_checks() {
        let monitor = UptimeMonitor::default();
        for minute in 0..(MAX_CHECKS_PER_TARGET as u32 + 5) {
            let mut c = check("eu", 0, true);
            c.timestamp = at(0) + ChronoDuration::seconds(i64::from(minute));
            monitor.record(c);
        }
        let all = monitor.checks_between(
            "https://api.example",
            DateTime::<Utc>::MIN_UTC,
            DateTime::<Utc>::MAX_UTC,
        );
        assert_eq!(all.len(), MAX_CHECKS_PER_TARGET);
        assert_eq!(all[0].timestamp, at(0) + ChronoDuration::seconds(5));
    }

    #[test]
    fn checks_between_filters_by_window() {
        let monitor = UptimeMonitor::default();
        for minute in 0..10 {
            monitor.record(check("eu", minute, true));
        }
        let window = monitor.checks_between("https://api.example", at(3), at(5));
        assert_eq!(window.len(), 3);
    }

    #[tokio::test]
    async fn probe_marks_unreachable_target_as_down() {
        let client = reqwest::Client::new();
        // Port 1 on loopback is closed, so the connection is refused immediately.
        let result = probe(
            &client,
            "http://127.0.0.1:1/health",
            "local",
            Duration::from_secs(2),
        )
        .await;
        assert!(!result.up);
        assert_eq!(result.status_code, None);
        assert_eq!(result.location, "local");
    }
}
