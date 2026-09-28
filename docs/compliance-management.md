# On-chain Compliance Management

Issues #552, #553, #554, #555 — compliance audit trail, Suspicious Activity
Reporting (SAR), compliance report generation and regulatory change management
for the `ttl_vault` contract. Implementation: `contracts/ttl_vault/src/compliance.rs`,
exposed through `TtlVaultContract` in `lib.rs`.

All state-changing entry points are **admin-only** except
`acknowledge_regulatory_notices`, which requires the user's own auth. No raw
PII is stored: subjects are Stellar addresses, and free-form fields (`provider`,
`reason`, `description`) should hold short codes or hashes of off-chain documents.

## Compliance audit trail (#552)

Every compliance action is appended to the subject address's trail and emitted
as a `cmp_aud` event.

| Action | Recorded by |
|---|---|
| `AmlCheck` | `record_aml_check` |
| `KycVerification` | `record_kyc_verification` |
| `BlacklistAdded` / `BlacklistRemoved` | `set_compliance_blacklist` |
| `SarFiled` / `SarStatusUpdated` | `file_sar` / `update_sar_status` (subject = vault owner) |
| `ReportGenerated` / `ReportSigned` | report functions (subject = admin) |
| `RequirementChanged` | regulatory functions (subject = admin) |
| `RegulatoryAcknowledged` | `acknowledge_regulatory_notices` (subject = user) |

`get_compliance_audit_trail(address) -> Vec<ComplianceAuditEntry>` returns the
latest 200 entries, oldest first. Older entries remain available from events.

Supporting functions: `record_aml_check`, `get_last_aml_check`,
`set_aml_risk_threshold` / `get_aml_risk_threshold` (default 70 of 100),
`record_kyc_verification`, `get_kyc_status` (a `Verified` record past
`expires_at` reads as `Expired`), `set_compliance_blacklist`,
`is_compliance_blacklisted`, `get_blacklist_record`.

Blacklisted addresses cannot `create_vault` or `deposit` (`AddressBlacklisted`).

## Suspicious Activity Reporting (#553)

- `file_sar(vault_id, reason) -> u64` files a SAR against a vault; the vault
  owner becomes the subject.
- An AML check that meets the risk threshold **and** references a vault files
  a SAR automatically (`auto_generated = true`).
- `update_sar_status(sar_id, status, submission_ref)` moves forward only:
  `Filed → UnderReview → Submitted → Closed` (steps may be skipped).
  `Submitted` requires the regulator's `submission_ref`.
- `get_sar`, `get_vault_sars(vault_id)`.
- `generate_sar_report(sar_id) -> SarReport` builds the submission package:
  the SAR, a vault snapshot, the subject's KYC/blacklist/last AML state and the
  20 most recent audit entries, sealed with a sha256 `digest`.

## Compliance reports (#554)

AML checks, KYC outcomes, blacklist changes, SAR filings and vault
deposits/withdrawals are aggregated into daily buckets.

- `generate_compliance_report(period) -> ComplianceReport` sums every day bucket
  overlapping `[start, end)` (max 366 days), snapshots current KYC totals, and
  stores the report with a sha256 `digest` of its content.
- `sign_compliance_report(report_id, signer, signature)` attaches an ed25519
  signature over the digest; the signature is verified on-chain and a report
  can be signed only once.
- `verify_compliance_report(report_id) -> ReportVerification` recomputes the
  digest and re-verifies the signature.
- `get_compliance_report(report_id)`.

## Regulatory change management (#555)

Requirements are versioned records (`code`, `jurisdiction`, `description`,
`rule_hash`, `effective_date`, `implemented_at`).

- `register_regulatory_requirement` creates version 1.
- `amend_regulatory_requirement` creates version N+1. The previous version is
  kept as `Superseded`, and the new version starts unimplemented.
- `mark_requirement_implemented` records the implementation date.
- `retire_regulatory_requirement` retires the requirement.
- Queries: `get_regulatory_requirement`, `get_requirement_version`,
  `get_requirement_history`, `list_regulatory_requirements`,
  `get_pending_implementations`, `is_requirement_effective`.

Each change publishes a `RegulatoryChangeNotice` and a `reg_chg` event for
off-chain notification. Users can page through notices with
`get_regulatory_notices(after_id, limit)`, list unread ones with
`get_pending_regulatory_notices(user)`, and acknowledge up to a notice id with
`acknowledge_regulatory_notices(user, notice_id)`.

## Error codes

| Code | Error |
|---|---|
| 131 | `InvalidComplianceInput` |
| 132 | `AddressBlacklisted` |
| 133 | `SarNotFound` |
| 134 | `InvalidSarReason` |
| 135 | `InvalidSarTransition` |
| 136 | `InvalidCompliancePeriod` |
| 137 | `ComplianceReportNotFound` |
| 138 | `ReportAlreadySigned` |
| 139 | `ReportDigestMismatch` |
| 140 | `RequirementNotFound` |
| 141 | `RequirementRetired` |
| 142 | `RequirementAlreadyImplemented` |
| 143 | `NoticeNotFound` |
