# Monitoring Guide

This guide ties together contract health, API availability, and error-rate
monitoring across `backend/src/metrics.rs`, `backend/src/custom_metrics.rs`,
and the on-chain `ttl_vault` event streams, so operators can detect issues
before users report them.

## Architecture

```
 ttl_vault contract events ──> event indexer ──> /metrics (Prometheus text)
 backend API (metrics.rs, custom_metrics.rs) ──> /metrics (Prometheus text)
                                                        │
                                                        ▼
                                                  Prometheus
                                                 (monitoring/prometheus.yml)
                                                        │
                                            ┌───────────┴───────────┐
                                            ▼                       ▼
                                        Grafana                Alertmanager
                                  (monitoring/grafana-*.json)  (monitoring/alert_rules.yml)
```

## 1. Prometheus metrics collection

The backend exposes metrics in Prometheus text-exposition format via the
`Metrics::render()` / custom metrics registry in `backend/src/metrics.rs` and
`backend/src/custom_metrics.rs`. Key series:

| Metric | Source | Meaning |
|---|---|---|
| `ethos_protocol_vaults_total` | metrics.rs | Counter of vaults created |
| `ethos_protocol_checkins_total` | metrics.rs | Counter of check-ins |
| `ethos_protocol_releases_total` | metrics.rs | Counter of vault releases |
| `ethos_protocol_active_vaults` | metrics.rs | Gauge of currently active vaults |
| `ethos_protocol_request_errors_total` | metrics.rs | Counter of API request errors |
| `ethos_protocol_http_requests_total` | metrics.rs | Counter of all API requests |
| `ethos_protocol_contract_paused` | metrics.rs | Gauge, 1 if contract is paused |
| `ethos_protocol_credential_lifecycle_transitions_total` | custom_metrics.rs | Counter of credential lifecycle state transitions |
| `ethos_protocol_credential_lifecycle_errors_total` | custom_metrics.rs | Counter of failed lifecycle transitions |
| `ethos_protocol_contract_upgrade_events_total` | event indexer | Counter of on-chain `upgrade()` events observed |
| `ethos_protocol_http_request_duration_seconds` | custom_metrics.rs | Histogram of API latency |

To start local collection:

```bash
prometheus --config.file=monitoring/prometheus.yml
```

Set `BACKEND_HOST`, `BACKEND_PORT`, `EVENT_INDEXER_HOST`, `EVENT_INDEXER_PORT`,
and `DEPLOY_ENV` in the environment before starting Prometheus, or edit the
static targets directly in `monitoring/prometheus.yml`.

## 2. Grafana dashboards

Import the following dashboards (Grafana > Dashboards > Import > Upload JSON):

- `monitoring/grafana-dashboard-vault-activity.json` — vault creation rate,
  active vault count, check-in and release rates, contract paused state.
- `monitoring/grafana-dashboard-credential-lifecycle.json` — credential
  lifecycle transition/error rates, HTTP error rate, and API latency
  percentiles (p50/p95/p99), covering the "error trends" requirement.

Point the Grafana datasource at the same Prometheus instance configured in
step 1.

## 3. Alerting rules

`monitoring/alert_rules.yml` defines the following alerts, loaded by
Prometheus via `rule_files` and routed through Alertmanager:

- `EthosApiDown` — API has not been scraped successfully for 1 minute.
- `EthosHighErrorRate` — API error rate above 5% for 5 minutes.
- `EthosHighApiLatencyP99` — p99 API latency above 2s for 5 minutes.
- `EthosContractPaused` — the `ttl_vault` contract has been paused for 1 minute.
- `EthosContractUpgradeInProgress` — an `upgrade()` event was observed
  on-chain in the last 10 minutes.
- `EthosCredentialLifecycleAnomalies` — more than 10 credential lifecycle
  transition errors in 15 minutes.

Each alert's `annotations.runbook` links to the matching section of
[`docs/runbook-alerts.md`](./runbook-alerts.md).

## 4. Wiring an Alertmanager

Point Prometheus at an Alertmanager instance (see the `alerting:` block in
`monitoring/prometheus.yml`) and configure receivers (Slack/PagerDuty/email)
per your organization's on-call tooling. This guide does not prescribe a
specific receiver so it stays portable across environments.

## 5. Runbook

See [`docs/runbook-alerts.md`](./runbook-alerts.md) for the step-by-step
response for each alert above.
