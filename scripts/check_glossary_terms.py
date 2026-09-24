#!/usr/bin/env python3
"""Flag glossary terms used inconsistently across docs/.

For each glossary term (and any documented alternate/variant spellings), scans
every markdown file under docs/ and reports files that mix multiple variants
of the same term (e.g. "slice" and "vault slice") rather than sticking to one
form. This is a lightweight heuristic, not a full style linter: it prints
findings for humans to review, it does not auto-fix anything.

Usage:
    python3 scripts/check_glossary_terms.py [--strict]

--strict causes the script to exit with status 1 if any inconsistency is
found, which makes it usable as a CI gate.
"""

import argparse
import pathlib
import re
import sys

DOCS_DIR = pathlib.Path(__file__).resolve().parent.parent / "docs"

# Term families: canonical term -> list of variant spellings that should not
# be mixed within the same document. Extend this list as new glossary terms
# with known variant spellings are added.
TERM_VARIANTS = {
    "slice": ["vault slice", "slice"],
    "check-in": ["check-in", "checkin", "check in"],
    "webauthn": ["webauthn", "web authn", "web-authn"],
    "attestor": ["attestor", "attester"],
}


def find_variants(text: str, variants: list[str]) -> dict[str, int]:
    counts = {}
    for variant in variants:
        pattern = re.compile(r"(?<![a-zA-Z-])" + re.escape(variant) + r"(?![a-zA-Z-])", re.IGNORECASE)
        matches = pattern.findall(text)
        if matches:
            counts[variant] = len(matches)
    return counts


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--strict", action="store_true", help="exit 1 if any inconsistency is found")
    args = parser.parse_args()

    if not DOCS_DIR.is_dir():
        print(f"docs directory not found at {DOCS_DIR}", file=sys.stderr)
        return 2

    findings = []
    for md_file in sorted(DOCS_DIR.glob("*.md")):
        text = md_file.read_text(encoding="utf-8", errors="ignore")
        for canonical, variants in TERM_VARIANTS.items():
            counts = find_variants(text, variants)
            # More than one distinct variant present in the same file = inconsistency.
            if len(counts) > 1:
                findings.append((md_file.name, canonical, counts))

    if not findings:
        print("No glossary term inconsistencies found.")
        return 0

    print("Glossary term inconsistencies found:\n")
    for filename, canonical, counts in findings:
        variant_summary = ", ".join(f'"{v}" x{c}' for v, c in counts.items())
        print(f"  {filename}: term family '{canonical}' — mixed usage: {variant_summary}")

    print(
        f"\n{len(findings)} inconsistency(ies) found. "
        "Pick one variant per document and update docs/glossary.md if a new "
        "term family needs to be tracked."
    )

    return 1 if args.strict else 0


if __name__ == "__main__":
    sys.exit(main())
