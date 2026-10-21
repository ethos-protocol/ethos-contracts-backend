//! Pattern-based log analysis and alerting (issue #603: "Logs aren't analyzed
//! for issues. Analysis would enable proactive alerting.").
//!
//! Builds on [`crate::log_analysis`] for parsing and glob matching. Given a set
//! of [`PatternRule`]s, it:
//! - Counts how often each pattern occurs (`PatternStats`).
//! - Tracks the peak number of occurrences inside a sliding time window.
//! - Raises a [`PatternAlert`] when a pattern reaches its threshold within
//!   its window (proactive alerting before an incident is reported).
//! - Produces a recommendation for each fired alert.
//!
//! Analysis is stateless: callers post the log lines to `POST /logs/analyze`
//! and receive the report. No log data is retained server-side, so the
//! endpoint cannot grow memory beyond the request body size limit.

use std::collections::HashSet;

use axum::{http::StatusCode, response::IntoResponse, Json};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::log_analysis::{matches_pattern, parse_log_line, LogEntry};

/// How urgently a fired pattern alert should be handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

/// A log pattern to watch for, with its alert condition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatternRule {
    pub name: String,
    /// Glob-style pattern, `*` matches any run of characters (case-insensitive).
    pub pattern: String,
    /// Only count entries at this level (case-insensitive). `None` matches any level.
    #[serde(default)]
    pub level: Option<String>,
    /// Occurrences within `window_seconds` that trigger an alert.
    pub threshold: usize,
    pub window_seconds: i64,
    pub severity: Severity,
    /// Suggested action shown when the alert fires.
    #[serde(default)]
    pub recommendation: Option<String>,
}

/// Frequency statistics for one rule across the analysed entries.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PatternStats {
    pub rule: String,
    pub total_matches: usize,
    /// Highest number of matches inside any single sliding window.
    pub peak_in_window: usize,
    pub first_seen: Option<DateTime<Utc>>,
    pub last_seen: Option<DateTime<Utc>>,
}

/// A rule whose peak frequency reached its threshold.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PatternAlert {
    pub rule: String,
    pub severity: Severity,
    pub occurrences: usize,
    pub threshold: usize,
    pub window_seconds: i64,
    pub recommendation: String,
}

/// Result of analysing a batch of log entries.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AnalysisReport {
    pub analyzed_entries: usize,
    pub patterns: Vec<PatternStats>,
    pub alerts: Vec<PatternAlert>,
    /// Unique recommendations for fired alerts, most severe first.
    pub recommendations: Vec<String>,
}

/// Rules shipped with the backend for common operational problems.
pub fn default_rules() -> Vec<PatternRule> {
    vec![
        PatternRule {
            name: "rpc-timeouts".to_string(),
            pattern: "*timeout*".to_string(),
            level: None,
            threshold: 5,
            window_seconds: 300,
            severity: Severity::Warning,
            recommendation: Some(
                "Check RPC node health and consider failing over to a healthy endpoint."
                    .to_string(),
            ),
        },
        PatternRule {
            name: "vault-release-failures".to_string(),
            pattern: "*release*failed*".to_string(),
            level: Some("ERROR".to_string()),
            threshold: 3,
            window_seconds: 600,
            severity: Severity::Critical,
            recommendation: Some(
                "Inspect the failing vault releases and verify beneficiary and contract state."
                    .to_string(),
            ),
        },
        PatternRule {
            name: "rate-limiting".to_string(),
            pattern: "*rate limit*".to_string(),
            level: None,
            threshold: 20,
            window_seconds: 60,
            severity: Severity::Info,
            recommendation: Some(
                "Clients are being throttled; review limits or the caller's request volume."
                    .to_string(),
            ),
        },
    ]
}

fn matches_rule(entry: &LogEntry, rule: &PatternRule) -> bool {
    let level_ok = rule.level.as_ref().is_none_or(|lvl| {
        entry
            .level
            .as_deref()
            .is_some_and(|l| l.eq_ignore_ascii_case(lvl))
    });
    level_ok && matches_pattern(&entry.raw, &rule.pattern)
}

/// Largest number of timestamps that fall inside any window of `window`.
/// `timestamps` must be sorted ascending.
fn peak_in_window(timestamps: &[DateTime<Utc>], window: Duration) -> usize {
    let mut peak = 0;
    let mut start = 0;
    for end in 0..timestamps.len() {
        while timestamps[end] - timestamps[start] > window {
            start += 1;
        }
        peak = peak.max(end - start + 1);
    }
    peak
}

fn default_recommendation(rule: &PatternRule) -> String {
    rule.recommendation.clone().unwrap_or_else(|| {
        format!(
            "Investigate recurring '{}' log pattern ({} occurrences in {}s).",
            rule.name, rule.threshold, rule.window_seconds
        )
    })
}

/// Analyse `entries` against `rules`.
///
/// Entries without a timestamp still count toward `total_matches`, but cannot
/// fall inside a time window, so they never contribute to `peak_in_window` or
/// alerts.
pub fn analyze(entries: &[LogEntry], rules: &[PatternRule]) -> AnalysisReport {
    let mut patterns = Vec::with_capacity(rules.len());
    let mut alerts = Vec::new();

    for rule in rules {
        let mut timestamps: Vec<DateTime<Utc>> = Vec::new();
        let mut total = 0usize;
        let mut first_seen: Option<DateTime<Utc>> = None;
        let mut last_seen: Option<DateTime<Utc>> = None;

        for entry in entries.iter().filter(|e| matches_rule(e, rule)) {
            total += 1;
            if let Some(ts) = entry.timestamp {
                timestamps.push(ts);
                first_seen = Some(first_seen.map_or(ts, |f| f.min(ts)));
                last_seen = Some(last_seen.map_or(ts, |l| l.max(ts)));
            }
        }
        timestamps.sort();

        let window = Duration::seconds(rule.window_seconds.max(0));
        let peak = peak_in_window(&timestamps, window);
        if rule.threshold > 0 && peak >= rule.threshold {
            alerts.push(PatternAlert {
                rule: rule.name.clone(),
                severity: rule.severity,
                occurrences: peak,
                threshold: rule.threshold,
                window_seconds: rule.window_seconds,
                recommendation: default_recommendation(rule),
            });
        }

        patterns.push(PatternStats {
            rule: rule.name.clone(),
            total_matches: total,
            peak_in_window: peak,
            first_seen,
            last_seen,
        });
    }

    alerts.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.rule.cmp(&b.rule))
    });

    let mut seen = HashSet::new();
    let recommendations = alerts
        .iter()
        .filter(|a| seen.insert(a.recommendation.clone()))
        .map(|a| a.recommendation.clone())
        .collect();

    AnalysisReport {
        analyzed_entries: entries.len(),
        patterns,
        alerts,
        recommendations,
    }
}

/// Request body for `POST /logs/analyze`.
#[derive(Debug, Deserialize)]
pub struct AnalyzeRequest {
    pub lines: Vec<String>,
    /// Rules to evaluate. When omitted or empty, [`default_rules`] are used.
    #[serde(default)]
    pub rules: Vec<PatternRule>,
}

/// `POST /logs/analyze` - parse the posted log lines and report pattern alerts.
pub async fn analyze_logs(Json(req): Json<AnalyzeRequest>) -> impl IntoResponse {
    if let Some(bad) = req
        .rules
        .iter()
        .find(|r| r.name.trim().is_empty() || r.pattern.is_empty())
    {
        return (
            StatusCode::BAD_REQUEST,
            format!("rule '{}' needs a name and a pattern", bad.name),
        )
            .into_response();
    }

    let entries: Vec<LogEntry> = req.lines.iter().map(|l| parse_log_line(l)).collect();
    let rules = if req.rules.is_empty() {
        default_rules()
    } else {
        req.rules
    };
    Json(analyze(&entries, &rules)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike};

    fn line(second: u32, level: &str, message: &str) -> String {
        format!("2026-07-01T10:00:{second:02}Z {level} svc: {message}")
    }

    fn entries(lines: &[String]) -> Vec<LogEntry> {
        lines.iter().map(|l| parse_log_line(l)).collect()
    }

    fn rule(pattern: &str, threshold: usize, window_seconds: i64) -> PatternRule {
        PatternRule {
            name: format!("rule:{pattern}"),
            pattern: pattern.to_string(),
            level: None,
            threshold,
            window_seconds,
            severity: Severity::Warning,
            recommendation: None,
        }
    }

    #[test]
    fn counts_total_matches_and_first_last_seen() {
        let lines = vec![
            line(0, "WARN", "rpc timeout"),
            line(5, "INFO", "all good"),
            line(9, "WARN", "rpc timeout again"),
        ];
        let report = analyze(&entries(&lines), &[rule("*timeout*", 10, 60)]);
        let stats = &report.patterns[0];
        assert_eq!(stats.total_matches, 2);
        assert_eq!(report.analyzed_entries, 3);
        assert_eq!(stats.first_seen.unwrap().second(), 0);
        assert_eq!(stats.last_seen.unwrap().second(), 9);
    }

    #[test]
    fn alert_fires_when_threshold_reached_inside_window() {
        let lines: Vec<String> = (0..5).map(|s| line(s, "WARN", "rpc timeout")).collect();
        let report = analyze(&entries(&lines), &[rule("*timeout*", 5, 60)]);
        assert_eq!(report.alerts.len(), 1);
        assert_eq!(report.alerts[0].occurrences, 5);
        assert_eq!(report.patterns[0].peak_in_window, 5);
    }

    #[test]
    fn spread_out_occurrences_do_not_alert() {
        // Four matches, but no 10-second window contains more than two of them.
        let lines = vec![
            line(0, "WARN", "rpc timeout"),
            line(1, "WARN", "rpc timeout"),
            line(30, "WARN", "rpc timeout"),
            line(31, "WARN", "rpc timeout"),
        ];
        let report = analyze(&entries(&lines), &[rule("*timeout*", 3, 10)]);
        assert!(report.alerts.is_empty());
        assert_eq!(report.patterns[0].total_matches, 4);
        assert_eq!(report.patterns[0].peak_in_window, 2);
    }

    #[test]
    fn level_filter_restricts_matches() {
        let lines = vec![
            line(0, "INFO", "release failed for vault"),
            line(1, "ERROR", "release failed for vault"),
        ];
        let mut r = rule("*release*failed*", 1, 60);
        r.level = Some("error".to_string());
        let report = analyze(&entries(&lines), &[r]);
        assert_eq!(report.patterns[0].total_matches, 1);
    }

    #[test]
    fn entries_without_timestamps_count_but_never_alert() {
        let lines = vec!["plain timeout line".to_string(); 10];
        let report = analyze(&entries(&lines), &[rule("*timeout*", 2, 60)]);
        assert_eq!(report.patterns[0].total_matches, 10);
        assert_eq!(report.patterns[0].peak_in_window, 0);
        assert!(report.alerts.is_empty());
    }

    #[test]
    fn alerts_are_ordered_by_severity_and_recommendations_deduplicated() {
        let lines: Vec<String> = (0..4)
            .map(|s| line(s, "ERROR", "release failed timeout"))
            .collect();
        let mut info = rule("*timeout*", 2, 60);
        info.severity = Severity::Info;
        info.recommendation = Some("same advice".to_string());
        let mut critical = rule("*failed*", 2, 60);
        critical.severity = Severity::Critical;
        critical.recommendation = Some("same advice".to_string());

        let report = analyze(&entries(&lines), &[info, critical]);
        assert_eq!(report.alerts[0].severity, Severity::Critical);
        assert_eq!(report.alerts.len(), 2);
        assert_eq!(report.recommendations, vec!["same advice".to_string()]);
    }

    #[test]
    fn alert_without_custom_recommendation_gets_default_text() {
        let lines: Vec<String> = (0..3).map(|s| line(s, "WARN", "rpc timeout")).collect();
        let report = analyze(&entries(&lines), &[rule("*timeout*", 3, 60)]);
        assert!(report.alerts[0].recommendation.contains("rule:*timeout*"));
        assert_eq!(report.recommendations.len(), 1);
    }

    #[test]
    fn peak_window_handles_boundary_exactly() {
        let base = Utc.with_ymd_and_hms(2026, 7, 1, 10, 0, 0).unwrap();
        let stamps = vec![
            base,
            base + Duration::seconds(10),
            base + Duration::seconds(20),
        ];
        // Span from first to last is exactly 20s, so a 20s window holds all three.
        assert_eq!(peak_in_window(&stamps, Duration::seconds(20)), 3);
        assert_eq!(peak_in_window(&stamps, Duration::seconds(19)), 2);
    }

    #[test]
    fn default_rules_cover_expected_operational_patterns() {
        let rules = default_rules();
        let names: Vec<&str> = rules.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"rpc-timeouts"));
        assert!(names.contains(&"vault-release-failures"));
        assert!(names.contains(&"rate-limiting"));
    }
}
