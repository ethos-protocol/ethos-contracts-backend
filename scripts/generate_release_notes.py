#!/usr/bin/env python3
"""Generate a Keep a Changelog section from merged commit subjects.

Squash-merged PRs land on `main` as one commit whose subject is the PR title,
so the subject history is the list of merged PRs. This script turns that
history into a changelog section grouped by change type.

Usage:
    scripts/generate_release_notes.py [--since-ref REF] [--version X.Y.Z | --unreleased] [REV]

    REV            revision to read history up to (default: HEAD)
    --since-ref    only include commits newer than REF's commit time
                   (use the commit that added the previous changelog entry)
    --version      emit `## [X.Y.Z] - <today>` instead of `## [Unreleased]`

Example (backfill everything merged after the 1.0.0 entry):
    scripts/generate_release_notes.py --since-ref 6a3fc51 --unreleased

Output goes to stdout; review it before pasting it into CHANGELOG.md.
"""

import argparse
import datetime
import re
import subprocess
import sys

# Conventional-commit types and how they map to changelog sections.
# `None` means the commit is noise for end users and is dropped.
TYPE_TO_SECTION = {
    "feat": "Added",
    "fix": "Fixed",
    "security": "Security",
    "sec": "Security",
    "perf": "Changed",
    "refactor": None,
    "chore": None,
    "docs": None,
    "doc": None,
    "test": None,
    "tests": None,
    "ci": None,
    "build": None,
    "style": None,
}

# For subjects without a conventional prefix, classify by leading verb.
FIXED_VERBS = {"fix", "fixes", "harden", "prevent", "correct", "resolve", "restore"}
ADDED_VERBS = {"add", "adds", "implement", "implements", "wire", "introduce", "export"}

CONVENTIONAL = re.compile(r"^(?P<type>[A-Za-z]+)(?:\([^)]*\))?!?:\s*(?P<text>.+)$")
SECTION_ORDER = ["Added", "Changed", "Fixed", "Security"]


def commit_subjects(rev: str, since_ts: int | None) -> list[str]:
    cmd = ["git", "log", "--no-merges", "--format=%s", rev]
    if since_ts is not None:
        cmd.insert(3, f"--after={since_ts}")
    out = subprocess.run(cmd, check=True, capture_output=True, text=True).stdout
    return [line.strip() for line in out.splitlines() if line.strip()]


def classify(subject: str) -> tuple[str, str] | None:
    """Return (section, text) for a subject, or None if it should be dropped."""
    if subject.startswith("Merge "):
        return None

    match = CONVENTIONAL.match(subject)
    if match:
        kind = match.group("type").lower()
        if kind not in TYPE_TO_SECTION:
            return "Changed", match.group("text")
        section = TYPE_TO_SECTION[kind]
        return (section, match.group("text")) if section else None

    first_word = subject.split(None, 1)[0].lower().rstrip(":")
    if first_word in FIXED_VERBS:
        return "Fixed", subject
    if first_word in ADDED_VERBS:
        return "Added", subject
    return "Changed", subject


def render(sections: dict[str, list[str]], header: str) -> str:
    lines = [header, ""]
    for name in SECTION_ORDER:
        entries = sections.get(name)
        if not entries:
            continue
        lines.append(f"### {name}")
        lines.extend(f"- {entry[:1].upper()}{entry[1:]}" for entry in entries)
        lines.append("")
    return "\n".join(lines).rstrip() + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("rev", nargs="?", default="HEAD")
    parser.add_argument("--since-ref", help="only include commits newer than this ref's commit time")
    group = parser.add_mutually_exclusive_group()
    group.add_argument("--version", help="release version, e.g. 1.1.0")
    group.add_argument("--unreleased", action="store_true", help="emit an [Unreleased] heading (default)")
    args = parser.parse_args()

    since_ts = None
    if args.since_ref:
        since_ts = int(
            subprocess.run(
                ["git", "log", "-1", "--format=%ct", args.since_ref],
                check=True,
                capture_output=True,
                text=True,
            ).stdout.strip()
        )

    sections: dict[str, list[str]] = {}
    for subject in commit_subjects(args.rev, since_ts):
        classified = classify(subject)
        if classified is None:
            continue
        section, text = classified
        # Keep each entry once even if the subject repeats across cherry-picks.
        bucket = sections.setdefault(section, [])
        if text not in bucket:
            bucket.append(text)

    if args.version:
        today = datetime.date.today().isoformat()
        header = f"## [{args.version}] - {today}"
    else:
        header = "## [Unreleased]"

    sys.stdout.write(render(sections, header))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
