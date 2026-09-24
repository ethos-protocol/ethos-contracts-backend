# Runbook: Common Alerts

Companion to [`docs/monitoring-guide.md`](./monitoring-guide.md) and
`monitoring/alert_rules.yml`. Each section below matches an `alert:` name.

## EthosApiDown

**Meaning:** Prometheus failed to scrape the backend `/metrics` endpoint for
over a minute — the API is likely down or unreachable.

1. Check process/container status for the backend service.
2. Check recent deploys — a bad rollout is the most common cause.
3. Check load balancer / ingress health checks for the API.
4. If the process is up but unreachable, check network/security group
   changes.
5. If down, restart the service; if a bad deploy, roll back to the last
   known-good build.

## EthosHighErrorRate

**Meaning:** More than 5% of API requests are erroring over a 5 minute
window (`ethos_protocol_request_errors_total` vs
`ethos_protocol_http_requests_total`).

1. Check application logs for the dominant error type/status code.
2. Check whether errors correlate with a specific endpoint or a specific
   upstream (contract RPC calls, database).
3. Check `EthosContractPaused` and `EthosContractUpgradeInProgress` — a
   paused or upgrading contract will surface as API errors.
4. If caused by a bad deploy, roll back.
5. If caused by an upstream RPC provider outage, fail over to a backup RPC
   endpoint if configured.

## EthosHighApiLatencyP99

**Meaning:** p99 request latency has exceeded 2 seconds for 5 minutes.

1. Check CPU/memory saturation on the backend host.
2. Check for slow on-chain RPC calls (network latency spikes to the Soroban
   RPC endpoint show up here).
3. Check database/query latency if the slow paths involve persistence.
4. Scale out the backend if the bottleneck is compute, or switch RPC
   providers if the bottleneck is upstream.

## EthosContractPaused

**Meaning:** The `ttl_vault` contract's `Paused` state has been `true` for
over a minute. This is often deliberate (an admin-initiated pause) but must
be confirmed.

1. Check the admin action log / `get_pause_record` for who paused the
   contract and why.
2. If unplanned, treat as a security incident: verify the admin key was not
   compromised (see `docs/upgrade-safety.md` for related admin-key
   guidance).
3. If planned (e.g. ahead of an upgrade), acknowledge and note the expected
   unpause time.
4. Unpause via the admin `unpause()` call once the underlying issue is
   resolved.

## EthosContractUpgradeInProgress

**Meaning:** An `upgrade()` call was observed on-chain in the last 10
minutes.

1. Confirm the upgrade was authorized — cross-check against the planned
   change log / deploy ticket.
2. If unauthorized, treat as a critical security incident: the admin key may
   be compromised. Follow the incident response process to rotate the admin
   key and, if available, pause the contract.
3. If authorized, verify `validate_upgrade` checks passed (see
   `docs/upgrade-safety.md`) and monitor `EthosApiDown` /
   `EthosHighErrorRate` closely for the next 30 minutes for regressions
   introduced by the new WASM.

## EthosCredentialLifecycleAnomalies

**Meaning:** More than 10 credential lifecycle transition errors in 15
minutes (`ethos_protocol_credential_lifecycle_errors_total`).

1. Check which transition is failing (issued → active → revoked → expired,
   etc.) via logs from `credential_lifecycle.rs`.
2. Check for a recent contract upgrade that may have changed lifecycle
   validation rules.
3. Check for a client-side integration bug (a caller attempting an invalid
   transition repeatedly).
4. Escalate to the credential lifecycle owning team if the error rate keeps
   climbing after 30 minutes.

## Escalation

If an alert cannot be resolved within its expected window (15 minutes for
warning-severity, 5 minutes for critical-severity), escalate per your
organization's on-call policy. Always leave a note in the incident channel
linking back to the specific alert and the runbook section used.
