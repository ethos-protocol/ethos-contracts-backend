#!/usr/bin/env python3
"""
check_allowlist_expiry.py — CI check for vulnerability allowlist expiry dates.

Reads .cargo/audit.toml and deny.toml, extracts `expires = "YYYY-MM-DD"` comments
adjacent to each allowlisted advisory, and fails if any entry is past its expiry date.

Usage:
    python3 scripts/check_allowlist_expiry.py

Exit codes:
    0  — All entries are within their expiry window (or have no expiry set yet,
         which is treated as a WARNING only if --warn-missing is passed).
    1  — One or more entries have expired; re-review or remove them.
"""

import re
import sys
from datetime import date, datetime
from pathlib import Path

# Files to scan for allowlist entries
ALLOWLIST_FILES = [
    Path(".cargo/audit.toml"),
    Path("deny.toml"),
]

# Regex to extract an expiry date from a comment line immediately before or
# on the same block as an advisory ID.
# Matches: # expires = "YYYY-MM-DD"  (with optional surrounding whitespace)
EXPIRY_RE = re.compile(r'#\s*expires\s*=\s*"(\d{4}-\d{2}-\d{2})"')

# Regex to extract an advisory ID (RUSTSEC-YYYY-NNNN)
ADVISORY_RE = re.compile(r'"(RUSTSEC-\d{4}-\d{4})"')


def parse_entries(path: Path) -> list[dict]:
    """
    Parse a TOML file and return a list of dicts:
      { "advisory": str, "expiry": date | None, "line": int, "file": str }
    """
    entries = []
    text = path.read_text()
    lines = text.splitlines()
    pending_expiry = None

    for lineno, line in enumerate(lines, start=1):
        expiry_match = EXPIRY_RE.search(line)
        if expiry_match:
            try:
                pending_expiry = datetime.strptime(expiry_match.group(1), "%Y-%m-%d").date()
            except ValueError:
                print(f"  WARNING: Invalid date format on {path}:{lineno}: {line.strip()}")
                pending_expiry = None
            continue

        advisory_match = ADVISORY_RE.search(line)
        if advisory_match:
            advisory_id = advisory_match.group(1)
            entries.append(
                {
                    "advisory": advisory_id,
                    "expiry": pending_expiry,
                    "line": lineno,
                    "file": str(path),
                }
            )
            pending_expiry = None  # consume the pending expiry

    return entries


def main() -> int:
    today = date.today()
    warn_missing = "--warn-missing" in sys.argv

    all_entries = []
    for f in ALLOWLIST_FILES:
        if f.exists():
            all_entries.extend(parse_entries(f))
        else:
            print(f"  WARNING: Allowlist file not found: {f}")

    if not all_entries:
        print("No allowlist entries found. Nothing to check.")
        return 0

    expired = []
    missing_expiry = []

    for entry in all_entries:
        advisory = entry["advisory"]
        expiry = entry["expiry"]
        location = f"{entry['file']}:{entry['line']}"

        if expiry is None:
            missing_expiry.append((advisory, location))
        elif today > expiry:
            expired.append((advisory, expiry, location))

    # Report results
    print(f"Checking allowlist expiry as of {today.isoformat()} ...")
    print(f"  Found {len(all_entries)} allowlisted entries across {len(ALLOWLIST_FILES)} files.")

    if missing_expiry:
        label = "ERROR" if not warn_missing else "WARNING"
        for advisory, location in missing_expiry:
            print(
                f"  [{label}] {advisory} at {location}: missing expiry date. "
                f'Add a comment: # expires = "YYYY-MM-DD" (max 90 days from today: '
                f'{(today.replace(year=today.year) if False else today).__class__.__name__})'
            )
            # Calculate and show the maximum allowed expiry
            from datetime import timedelta
            max_expiry = today + timedelta(days=90)
            print(f"           Suggested expiry: {max_expiry.isoformat()}")

    if expired:
        for advisory, expiry, location in expired:
            days_over = (today - expiry).days
            print(
                f"  [EXPIRED] {advisory} at {location}: expired {expiry.isoformat()} "
                f"({days_over} day(s) ago). Re-review and update the expiry date or "
                f"remove the entry."
            )

    # Determine exit code
    has_errors = bool(expired) or (bool(missing_expiry) and not warn_missing)

    if has_errors:
        print(
            "\nFAILED: Expired or undated allowlist entries found. "
            "See docs/vulnerability-scanning.md for the review process."
        )
        return 1

    if missing_expiry and warn_missing:
        print(
            "\nWARNING: Some entries are missing expiry dates. "
            "Add `# expires = \"YYYY-MM-DD\"` comments before each advisory ID."
        )
    else:
        print("\nAll allowlist entries are within their expiry window. ✓")

    return 0


if __name__ == "__main__":
    sys.exit(main())
