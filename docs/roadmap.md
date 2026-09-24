# Roadmap

> **Status legend**: 🟢 Done · 🟡 In progress · ⚪ Not started
>
> Each milestone below links to the issue/PR that implements it where one is
> tracked. Cross-checked against [docs/issues-32-38-39-40.md](issues-32-38-39-40.md)
> for already-addressed items (see [Cross-Check Notes](#cross-check-notes)).

## v1.0 (Current)

| Status | Item | Issue/PR |
|---|---|---|
| 🟢 | XLM vault creation and management | #1 |
| 🟢 | TTL-based expiry and release | #2 |
| 🟢 | Multi-beneficiary support with BPS splits | #12 |
| 🟢 | Admin controls (pause, config) | #5 |
| 🟢 | Comprehensive test coverage | #6 |

## v1.1 (Q2 2026)

| Status | Item | Issue/PR |
|---|---|---|
| ⚪ | Custom token support (USDC, EURC, etc.) | #101 |
| ⚪ | Vault metadata and notes | #102 |
| ⚪ | Batch operations (multi-vault check-in) | #103 |

## v2.0 (Q3 2026)

| Status | Item | Issue/PR |
|---|---|---|
| 🟡 | Passkey authentication integration | #201 (see [docs/passkeys.md](passkeys.md)) |
| ⚪ | Frontend dashboard (React + Freighter) | #202 |
| ⚪ | Reminder service (encrypted email/SMS) | #203 |
| ⚪ | Event indexing and history | #204 |

## v2.1 (Q4 2026)

| Status | Item | Issue/PR |
|---|---|---|
| ⚪ | Conditional release logic (time + conditions) | #301 |
| ⚪ | Partial release scheduling | #302 |
| ⚪ | Vault transfer/ownership change | #303 |

## v3.0 (2027)

| Status | Item | Issue/PR |
|---|---|---|
| ⚪ | Mobile app (iOS/Android) | #401 |
| ⚪ | Push notification reminders | #402 (see [docs/push-notifications.md](push-notifications.md)) |
| 🟡 | Multi-signature vault support | #403 (see [docs/multi-sig.md](multi-sig.md)) |
| ⚪ | Testamentary message storage | #404 |

## v4.0 (Future)

| Status | Item | Issue/PR |
|---|---|---|
| ⚪ | Fiat on/off-ramps | #501 |
| ⚪ | Legal document anchoring | #502 |
| ⚪ | Cross-chain bridge support | #503 |
| ⚪ | DAO governance for protocol upgrades | #504 |

## Cross-Check Notes

The following items landed via [docs/issues-32-38-39-40.md](issues-32-38-39-40.md)
and are not yet reflected as dedicated roadmap line items. They are recorded
here so progress auditing doesn't miss work that shipped outside the
milestone list above:

- Issue #32 — Credential anchoring to external systems (🟢 done)
- Issue #38 — Slice composition cost tracking (🟢 done)
- Issue #39 — Slice consensus voting (🟢 done)
- Issue #40 — Slice attribute-based matching (🟢 done)

These are protocol/slice-layer features rather than vault-release milestones,
so they are tracked here rather than folded into the version table above.

## Issue-Closing Checklist Addition

When closing an issue that implements a roadmap item:

1. Update the relevant milestone row's **Status** column above.
2. Add or update the **Issue/PR** reference for that row.
3. If the issue introduces work not yet represented on the roadmap, add a new
   row (or a Cross-Check Notes entry, for non-milestone protocol work).
4. Link back to this file from the issue/PR description so reviewers can
   confirm the roadmap was updated.

## Community Contributions

We welcome contributions at any stage. See [CONTRIBUTING.md](../CONTRIBUTING.md) for guidelines.
