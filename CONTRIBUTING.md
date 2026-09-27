# Contributing to Ethos-Protocol

Thank you for your interest in contributing! This document explains how to get started, what standards to follow, and how the review process works.

## Table of Contents

- [Getting Started](#getting-started)
- [Code Style](#code-style)
- [Git Workflow](#git-workflow)
- [Testing Requirements](#testing-requirements)
- [Pull Request Process](#pull-request-process)
- [Documentation](#documentation)
- [Security Disclosures](#security-disclosures)

---

## Getting Started

### Prerequisites

- Rust (`rustup` — version pinned in `rust-toolchain.toml`)
- Soroban CLI and Stellar CLI
- Docker & Docker Compose (for local PostgreSQL and Stellar Quickstart)
- Node.js 20+ (for TypeScript client development)

### Set up your environment

```bash
# Clone your fork
git clone https://github.com/<your-username>/ethos-contracts-backend.git
cd ethos-contracts-backend

# Install git hooks (secret scanning, formatting checks)
./scripts/install-hooks.sh

# Copy environment template
cp .env.example .env

# Start dependencies
docker-compose up -d

# Build
./scripts/build.sh

# Run tests
./scripts/test.sh
```

### Install git hooks

Always install the pre-commit hooks before your first commit:

```bash
./scripts/install-hooks.sh
```

The hooks run secret scanning (`gitleaks`) and formatting checks. Commits that contain secrets or unformatted code will be rejected locally before they reach CI.

---

## Code Style

### Rust

- Format with `rustfmt` before every commit:
  ```bash
  cargo fmt
  ```
- Pass Clippy with no warnings (configuration in `.clippy.toml`):
  ```bash
  cargo clippy --all-targets --all-features -- -D warnings
  ```
- Use `///` doc comments on all public items (functions, structs, enums, traits).
- Return `Result<T, ContractError>` from all fallible contract functions — do not panic.
- Prefer `require_auth()` as the first statement in any owner-only function.
- Keep functions short and focused; extract helpers rather than nesting deeply.

### TypeScript (client SDK)

- Follow the existing ESLint configuration in `clients/typescript/`.
- Use explicit types — avoid `any`.
- Export only what is part of the public API.

### Documentation

- Use plain Markdown (`.md`) for all documentation under `docs/`.
- Keep line length under 120 characters where practical.
- Add a `Last updated: YYYY-MM-DD` footer to any document you substantially change.

---

## Git Workflow

### Branch naming

| Type | Pattern | Example |
|---|---|---|
| Feature | `feature/<short-description>` | `feature/beneficiary-vesting` |
| Bug fix | `fix/<short-description>` | `fix/ttl-overflow-edge-case` |
| Documentation | `docs/<short-description>` | `docs/update-deployment-guide` |
| Refactor | `refactor/<short-description>` | `refactor/extract-vault-helpers` |
| Chore | `chore/<short-description>` | `chore/bump-soroban-sdk` |

### Commit messages

Follow the [Conventional Commits](https://www.conventionalcommits.org/) format:

```
<type>(<scope>): <short summary>

[optional body — explain WHY, not WHAT]

[optional footer: Closes #<issue-number>]
```

**Types:** `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `perf`, `ci`

**Examples:**

```
feat(vault): add hibernation mode for extended owner absences

Closes #42
```

```
fix(ttl): prevent TTL overflow when interval exceeds u64::MAX / 2
```

```
docs(adr): add ADR-0006 for message queue choice
```

- Keep the subject line under 72 characters.
- Use the imperative mood: "add feature" not "added feature".
- Reference the issue number in the footer when applicable.

### Keeping your branch up to date

Rebase onto `main` rather than merging to keep history clean:

```bash
git fetch origin
git rebase origin/main
```

---

## Testing Requirements

All contributions must include tests. The test coverage expectation depends on the type of change:

### Smart contracts

| Change type | Requirement |
|---|---|
| New public function | Unit tests covering happy path + all error paths |
| Modified function | Update existing tests; add regression test for the bug fixed |
| New error variant | At least one test that exercises the new error |

Run contract tests:

```bash
cargo test -p ttl_vault
cargo test -p zk_verifier
cargo test -p sbt
```

### Backend

| Change type | Requirement |
|---|---|
| New API endpoint | Integration test in `backend/src/tests.rs` |
| New business logic | Unit tests in the relevant module |
| New background job | Test the job logic with a mock scheduler |

Run backend tests:

```bash
cargo test -p backend
```

### General

- Tests must pass with no warnings.
- Do not use `#[ignore]` on tests unless the reason is documented and tracked in an issue.
- Property-based tests (`backend/src/property_tests.rs`) are preferred for functions with complex input spaces.

---

## Pull Request Process

1. **Open the PR against `main`** with a clear title following the commit message convention.
2. **Fill in the PR description** (see template below).
3. **Link the relevant issue(s)** — use `Closes #<number>` in the description so the issue auto-closes on merge.
4. **Ensure all CI checks pass** before requesting review:
   - `cargo fmt --check`
   - `cargo clippy -- -D warnings`
   - `cargo test`
   - `cargo audit`
   - `cargo deny check`
   - Secret scan (gitleaks)
5. **Request at least one reviewer** from the core team.
6. **Address review feedback** — push additional commits to the same branch; do not force-push after review has started.
7. **Squash or rebase** before merge if the commit history is noisy (the maintainer may do this on merge).

### PR description template

```markdown
## Summary

Brief description of what this PR does and why.

## Changes

- List of concrete changes made

## Testing

Describe how you tested these changes.

## Related Issues

Closes #<number>
```

### Review expectations

- Reviewers aim to respond within 2 business days.
- "Request changes" feedback must be resolved before merge.
- Approvals from maintainers with write access are required to merge.

---

## Documentation

- Any new feature must include documentation in `docs/`.
- Any change to a documented behaviour must update the relevant doc.
- New architectural decisions must be captured as an ADR in `docs/adr/` — see [`docs/adr/README.md`](docs/adr/README.md) for the process.
- Public API changes must be reflected in `docs/api-reference.md` and `docs/openapi.yaml`.

---

## Security Disclosures

Do **not** open a public GitHub issue for security vulnerabilities. Follow the responsible disclosure process described in [`SECURITY.md`](SECURITY.md).

---

## Code of Conduct

All contributors are expected to follow the [Code of Conduct](CODE_OF_CONDUCT.md). Be respectful and constructive in all interactions.
