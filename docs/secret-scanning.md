# Secret Scanning

Ethos-Protocol uses [gitleaks](https://github.com/gitleaks/gitleaks) to automatically scan for secrets (API keys, tokens, private keys, passwords, etc.) at two points in the development workflow: locally before every commit, and in CI before any code reaches `main`.

---

## How It Works

### CI Scanning (always-on)

Every push to `main` and every pull request targeting `main` triggers a secret scan as part of the CI pipeline (`.github/workflows/ci.yml`).

The step installs gitleaks from the official GitHub release and runs:

```bash
gitleaks detect --source . --config .gitleaks.toml --redact --exit-code 1
```

This scans the **entire repository history** that is checked out by the runner. If any secret is detected the CI job fails and the pull request cannot be merged until the finding is resolved.

### Pre-Commit Hook (local, optional)

`scripts/pre-commit-secret-scan.sh` is a git pre-commit hook that runs gitleaks on **staged changes only** before each `git commit`:

```bash
gitleaks protect --staged --config .gitleaks.toml --redact
```

Catching secrets locally — before they are committed — is faster and safer than relying solely on CI because:

- The secret never enters git history in the first place.
- You get immediate, interactive feedback in your terminal.
- No waiting for a CI pipeline to surface the problem.

If gitleaks is not installed the hook warns but exits `0`, so developers without the tool are never blocked. CI remains the safety net.

---

## Installing the Pre-Commit Hook

Run the installer once after cloning the repository:

```bash
./scripts/install-hooks.sh
```

The script:

1. Locates the repo root via `git rev-parse --show-toplevel`.
2. Writes a thin wrapper to `.git/hooks/pre-commit` that delegates to `scripts/pre-commit-secret-scan.sh`.
3. Makes both files executable (`chmod +x`).
4. Backs up any pre-existing `.git/hooks/pre-commit` to `.git/hooks/pre-commit.bak.<timestamp>` rather than silently overwriting it.
5. Is idempotent — safe to run multiple times.

After installation, every `git commit` will trigger the scan automatically. No further configuration is needed.

### Verifying the installation

```bash
cat .git/hooks/pre-commit   # should reference pre-commit-secret-scan.sh
```

### Installing gitleaks

The hook warns and exits `0` when gitleaks is absent. To enable local scanning, install it:

```bash
# macOS
brew install gitleaks

# Linux (x86_64) — replace version as needed
GITLEAKS_VERSION=8.21.2
curl -sSfL \
  "https://github.com/gitleaks/gitleaks/releases/download/v${GITLEAKS_VERSION}/gitleaks_${GITLEAKS_VERSION}_linux_x64.tar.gz" \
  | tar xz -C /usr/local/bin gitleaks

# Windows
winget install gitleaks
```

Verify the installation:

```bash
gitleaks version
```

---

## What Gets Scanned

### Pre-commit hook (local)

Only the **staged diff** — the exact changes you are about to commit. This is the minimal, fast scan that runs on every commit.

### CI scan

The **full working tree** as checked out by the runner, covering all tracked files in the repository snapshot. This is a broader scan that catches anything that may have slipped through.

### Detection rules

gitleaks ships with a large built-in ruleset that detects secrets from hundreds of providers (AWS, GCP, GitHub, Stripe, Twilio, SendGrid, and many more). The `.gitleaks.toml` configuration file at the repository root extends this with project-specific allowlists.

---

## Handling False Positives

Not every match is a real secret. Test fixtures, documentation examples, and placeholder values can trigger gitleaks rules. There are two ways to suppress false positives.

### 1. Allowlist in `.gitleaks.toml`

The `.gitleaks.toml` file at the repository root contains an `[allowlist]` section. You can allow specific:

**File paths** (regex patterns):

```toml
[allowlist]
  paths = [
    '''\.env\.example$''',
    '''README\.md$''',
    '''docs/''',
  ]
```

Any file whose path matches one of these patterns is entirely excluded from scanning.

**Specific values** (regex patterns):

```toml
[allowlist]
  regexes = [
    "test-secret-key-for-unit-tests",
    "tok-secret",
    "REMINDER_ENCRYPTION_SECRET=",
  ]
```

Any line containing a matching substring is excluded, regardless of which file it appears in.

After editing `.gitleaks.toml`, stage the change and commit. CI will pick up the updated allowlist automatically.

### 2. Inline suppression

For a single line that cannot be removed, add an inline comment to suppress it:

```rust
let api_key = "example-only-not-real"; // gitleaks:allow
```

Use this sparingly — prefer the allowlist for patterns that recur across multiple files.

### Guidelines for allowlisting

| Acceptable to allowlist | Do NOT allowlist |
|---|---|
| `.env.example` placeholder values | Real API keys or tokens |
| Documentation / README examples | Production credentials |
| Unit-test fixture strings that are clearly fake | Anything that looks like a real credential |
| CI/CD environment variable references (e.g. `$MY_SECRET`) | Hardcoded passwords |

---

## Secret Management Best Practices

### Never commit real secrets

- Use `.env` for local secrets — it is git-ignored by `.gitignore`.
- Copy `.env.example` as your starting point: `cp .env.example .env`, then fill in real values.
- Use your CI provider's secret store (e.g. GitHub Actions Secrets) to inject credentials at build time — reference them as `${{ secrets.MY_SECRET }}`, never hardcode them.

### Rotate secrets that were committed

If a real secret was committed — even momentarily — treat it as compromised:

1. **Rotate the secret immediately** with the issuing service (revoke the old key, generate a new one).
2. Notify your security team.
3. Clean the git history using [`git filter-repo`](https://github.com/newren/git-filter-repo) to remove the secret from all commits. Note: this rewrites history and requires force-pushing.
4. Ensure all collaborators re-clone or reset to the cleaned history.

History rewriting does not un-expose a secret that was ever pushed to a remote. Rotation is always required.

### Use a secrets manager

For production workloads use a dedicated secrets manager rather than environment variables:

- AWS Secrets Manager / Parameter Store
- HashiCorp Vault
- Doppler
- 1Password Secrets Automation

These provide audit trails, rotation policies, and fine-grained access control.

### Principle of least privilege

Issue service credentials with the minimum permissions required. If a scoped token is available (e.g. a GitHub fine-grained PAT instead of a classic PAT), use it.

---

## What Happens When a Secret Is Detected

### Pre-commit hook

The hook prints a clear error message and exits `1`, blocking the commit:

```
╔══════════════════════════════════════════════════════════════╗
║            🚨 SECRET DETECTED — COMMIT BLOCKED 🚨            ║
╚══════════════════════════════════════════════════════════════╝

gitleaks found one or more potential secrets in your staged changes.
The commit has been blocked to protect your credentials.
```

It then prints actionable instructions: how to remove the secret, how to add a false-positive to the allowlist, and how to use `.env` instead.

You can bypass the hook with `git commit --no-verify`, but CI will still block the pull request.

### CI

The `Scan for secrets with Gitleaks` step fails, marking the entire CI job as failed. GitHub blocks merging the pull request until all required status checks pass. The pull request author must:

1. Remove the secret from the branch (amend or add a new commit).
2. If the finding is a false positive, update `.gitleaks.toml` with an appropriate allowlist entry.
3. Push the fix — CI will re-run automatically.

Secret values are redacted in CI output (`--redact` flag) so they are not visible in public pipeline logs.

---

## Pattern Coverage Audit (2026-08-30)

`.gitleaks.toml` previously defined only an `[allowlist]` with no `[[rules]]`
and no `[extend]` block. Per gitleaks' config semantics, a config with no
`[extend]` block does **not** fall back to the built-in ruleset — it
replaces it. With zero `[[rules]]` defined, gitleaks was effectively
scanning with **zero detection rules**, in CI and in the pre-commit hook
alike: every run reported "no secrets found" regardless of what was staged
or committed. `[extend]\n  useDefault = true` was added to restore the
~180-rule built-in ruleset.

On top of the built-in ruleset, `.gitleaks.toml` defines custom rules for
secret formats specific to this repo's own dependencies/integrations that
the built-in ruleset does not cover:

| Rule ID | What it catches | Why it's repo-specific |
|---|---|---|
| `stellar-secret-seed` | A Stellar/Soroban StrKey secret seed (`S` + 55 base32 chars) | This is a Stellar/Soroban project (`STELLAR_IDENTITY`, `backend/src/consensus.rs`, `contracts/`); the built-in ruleset has no Stellar-specific rule. The `G...` public address form is *not* matched — it isn't a secret. |
| `database-connection-string-with-credentials` | A `postgres(ql)/mysql/mongodb(+srv)/redis(s)://user:pass@host` URI with embedded credentials | Used for the Postgres (`rusqlite`... `docker-compose.yml`) and Redis (`redis` crate, distributed consensus cache) connections configured via `DATABASE_URL`/`REDIS_URL`. |
| `abuseipdb-api-key` | An 80-char hex key near the word "abuseipdb" | `ABUSEIPDB_API_KEY`, consumed by `backend/src/ip_reputation.rs`'s AbuseIPDB integration (#96). |
| `fcm-legacy-server-key` | A legacy Firebase Cloud Messaging server key (`AAAA...:...`) | `FCM_SERVER_KEY`, consumed by `backend/src/notifications.rs`'s push-notification integration. |

Plus the built-in rules most relevant to this codebase:

| Built-in rule | Relevant to |
|---|---|
| `generic-api-key` | `REMINDER_EMAIL_API_KEY`, `REMINDER_SMS_API_KEY`, `RECAPTCHA_SECRET_KEY`, `FIELD_ENCRYPTION_KEY_*`, and any other `*_KEY=`/`*_SECRET=`/`*_TOKEN=` assignment |
| `private-key` | PEM-encoded key material (relevant given `ciborium`, `p256`, `ecdsa`, `ed25519-dalek`, and `ring` in `backend/Cargo.toml` for WebAuthn signature verification) |
| `jwt` | Tokens issued via the `jsonwebtoken` crate (WebSocket auth) |

An allowlist entry (`'''\w+_user:\w+_password@'''`) was also added for the
intentionally simple local-dev-only Postgres credentials in
`docker-compose.yml` (and its pre-Ethos-rename equivalent found in git
history) — these are never used outside a developer's machine.

### Fixture-based regression coverage

`scripts/secret-scan-fixtures/{positive,negative}/` holds one fixture file
per pattern above (plus `generic-api-key` and `private-key`): each positive
fixture contains a fake-but-correctly-shaped secret that must be detected;
each negative fixture contains a similar-looking value or empty placeholder
that must **not** be. The **Secret Scan Fixture Coverage** CI job
(`.github/workflows/security.yml`) runs gitleaks against every fixture file
individually — using `scripts/secret-scan-fixtures/gitleaks-fixtures.toml`,
which mirrors the root config minus the fixtures-path allowlist — and fails
if a positive fixture goes undetected or a negative fixture is flagged. This
turns "does our pattern list still catch X" into an automated regression
check instead of a one-time manual audit.

The fixtures directory itself is excluded from the repo-wide scan (see the
`scripts/secret-scan-fixtures/positive/` path allowlist above) so the
intentionally secret-shaped positive fixtures don't trip the main
`secret-scan` job.

## File Reference

| File | Purpose |
|---|---|
| `.gitleaks.toml` | gitleaks configuration and project-specific allowlists |
| `scripts/pre-commit-secret-scan.sh` | Pre-commit hook script |
| `scripts/install-hooks.sh` | Installs the pre-commit hook into `.git/hooks/` |
| `.github/workflows/ci.yml` | CI pipeline — includes the `Scan for secrets with Gitleaks` step |
| `.github/workflows/security.yml` | Security workflow — `secret-scan` (repo-wide) and `secret-scan-fixture-coverage` (pattern regression) jobs |
| `scripts/secret-scan-fixtures/` | Positive/negative fixtures exercising each custom + relevant built-in rule |
| `scripts/secret-scan-fixtures/gitleaks-fixtures.toml` | gitleaks config used only for fixture-coverage testing (no fixtures-path allowlist) |
| `.env.example` | Template for local environment variables (safe to commit) |
| `.env` | Actual local secrets — git-ignored, never committed |
