# Incident Response Procedures

This document describes the incident response workflow implemented in
`backend/src/incidents.rs`, replacing the previous ad-hoc, chat-coordinated
response process with structured tracking.

## Why

Without a consistent workflow, incidents were tracked informally in chat
threads, making severity, ownership, and history hard to reconstruct after
the fact. This module gives every incident a consistent lifecycle, a
severity classification, an escalation path, and an auditable timeline.

## Severity classification

| Severity | Meaning                                       | Escalation SLA |
|----------|------------------------------------------------|-----------------|
| Sev1     | Full outage or data loss — page immediately    | 10 minutes      |
| Sev2     | Major functionality degraded for many users    | 30 minutes      |
| Sev3     | Minor functionality degraded / workaround exists | 2 hours       |
| Sev4     | Cosmetic or low-impact issue                    | 8 hours         |

`IncidentSeverity::escalation_sla_minutes` encodes these thresholds;
`is_past_escalation_sla` checks whether an open incident has exceeded them.

## Lifecycle

Incidents move through `Open → Investigating → Mitigated → Resolved →
Closed` (`IncidentStatus`). Every status transition is recorded as a
timeline entry automatically.

## API

- `POST /incidents` — open an incident with `title`, `description`,
  `severity`, and optional `assigned_to`. Automatically seeds the timeline
  with an "incident opened" entry.
- `GET /incidents` — list all tracked incidents.
- `GET /incidents/:id` — fetch a single incident, including its full
  timeline.
- `POST /incidents/:id/timeline` — append a free-form timeline entry
  (`actor`, `note`) — used for investigation notes, mitigations applied, etc.
- `POST /incidents/:id/status` — transition status; records a timeline entry
  describing the before/after state.
- `POST /incidents/:id/escalate` — manually escalate (e.g. once
  `is_past_escalation_sla` returns true), incrementing `escalation_level`
  and logging the reason.

## Escalation workflow

Each incident tracks an `escalation_level`, starting at 0. Escalating
increments the level and appends a timeline entry noting the reason. A
scheduled job (see `scheduler.rs` for the existing polling pattern used
elsewhere in this backend) can call `is_past_escalation_sla` periodically
against open incidents to trigger automatic escalation before a human
notices the SLA has been missed.

## Acknowledgment timeout and automatic escalation

Escalation is enforced automatically rather than relying on manual
follow-up. When an incident is opened (or escalated), the on-call tier that
was paged is recorded along with the time it was paged. If that tier does
not acknowledge the page within the acknowledgment timeout, the incident is
escalated to the next on-call tier and the next tier is paged.

- The acknowledgment timeout is derived from the incident severity via
  `IncidentSeverity::ack_timeout_minutes` (Sev1: 5 minutes, Sev2: 15
  minutes, Sev3: 30 minutes, Sev4: 60 minutes).
- `Incident::is_ack_timed_out` reports whether the currently paged tier has
  exceeded its acknowledgment timeout without an acknowledgment.
- `Incident::acknowledge` records the acknowledgment, stopping further
  automatic escalation for that tier.
- `Incident::escalate` advances `escalation_level`, resolves the next tier
  through the `oncall` module's rotation lookup, and re-arms the
  acknowledgment timer for the newly paged tier.

A scheduled job (see `scheduler.rs` for the existing polling pattern used
elsewhere in this backend) calls `is_ack_timed_out` periodically against
open incidents and triggers `escalate` for any incident whose paged tier has
not acknowledged in time. This closes the loop between incident severity and
who gets paged: escalation walks the on-call rotation tier by tier until
someone acknowledges.

## Operational notes

- Incidents are stored in-memory for now
  (`Arc<Mutex<HashMap<String, Incident>>>`); persisting to the existing
  SQLite store (`db.rs`) is a natural follow-up so history survives
  restarts.
- Escalation logs via `tracing` and pages the next tier through the
  `oncall` module's rotation lookup, so incident severity and the on-call
  rotation stay in sync.
