#!/usr/bin/env python3
"""Check that environment variables read by the code are documented in .env.example.

Scans:
  - backend/src/**/*.rs for std::env::var / env::var_os / std::env::var_os
    and helper wrappers (env_var(, from_env config constructors).
  - backend/Dockerfile for ENV keys.
  - docker-compose*.yml for `environment:` keys.

Compares against keys documented in .env.example (lines matching
`^[A-Z][A-Z0-9_]*=`).

Exit non-zero with a clear diff listing undocumented variables.

Usage:
  scripts/check_env_vars.py            # check; exit 1 if undocumented vars found
  scripts/check_env_vars.py --list     # print authoritative sorted list
  scripts/check_env_vars.py --self-test  # run embedded fixture tests
"""

from __future__ import annotations

import argparse
import re
import sys
import tempfile
from pathlib import Path
from typing import Iterable, Set

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

REPO_ROOT = Path(__file__).resolve().parent.parent
BACKEND_SRC = REPO_ROOT / "backend" / "src"
DOCKERFILE = REPO_ROOT / "backend" / "Dockerfile"
ENV_EXAMPLE = REPO_ROOT / ".env.example"

# Variables that are test-only, internal, or provided by the toolchain and
# therefore need not be documented in .env.example.
ALLOWLIST_PATTERNS = [
    re.compile(r"^CARGO_"),
    re.compile(r"^RUST_"),
    re.compile(r"^TEST_"),
    re.compile(r"^CI$"),
    re.compile(r"^GITHUB_"),
    re.compile(r"^HOME$"),
    re.compile(r"^PATH$"),
    re.compile(r"^PWD$"),
    re.compile(r"^TMPDIR$"),
    re.compile(r"^USER$"),
    re.compile(r"^SHELL$"),
    re.compile(r"^LANG$"),
    re.compile(r"^LC_"),
    re.compile(r"^TERM$"),
]

# ---------------------------------------------------------------------------
# Rust scanning
# ---------------------------------------------------------------------------

# Direct reads: std::env::var("X"), env::var("X"), std::env::var_os("X"),
# env::var_os("X"), and helper wrappers env_var("X").
RUST_ENV_PATTERNS = [
    re.compile(r'\b(?:std::)?env::var(?:_os)?\s*\(\s*"([A-Za-z_][A-Za-z0-9_]*)"'),
    re.compile(r'\benv_var\s*\(\s*"([A-Za-z_][A-Za-z0-9_]*)"'),
    # from_env config constructors that take a literal name, e.g.
    #   Foo::from_env("X")  /  from_env("X")
    re.compile(r'\bfrom_env\s*\(\s*"([A-Za-z_][A-Za-z0-9_]*)"'),
]

# Detect #[cfg(test)] blocks so we can skip test-only reads.
CFG_TEST_RE = re.compile(r"#\[cfg\(\s*test\s*\)\]")


def _strip_cfg_test_blocks(source: str) -> str:
    """Remove the contents of #[cfg(test)] modules/blocks from Rust source.

    This is a lightweight brace-matching heuristic: when we see
    #[cfg(test)] we find the next `{` and remove everything up to the
    matching closing brace.
    """
    out = []
    i = 0
    n = len(source)
    while i < n:
        m = CFG_TEST_RE.search(source, i)
        if not m:
            out.append(source[i:])
            break
        out.append(source[i:m.start()])
        # Find the opening brace after the attribute.
        brace = source.find("{", m.end())
        if brace == -1:
            # No block; skip just the attribute.
            i = m.end()
            continue
        depth = 0
        j = brace
        while j < n:
            c = source[j]
            if c == "{":
                depth += 1
            elif c == "}":
                depth -= 1
                if depth == 0:
                    j += 1
                    break
            j += 1
        i = j
    return "".join(out)


def scan_rust_file(path: Path) -> Set[str]:
    try:
        source = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return set()
    source = _strip_cfg_test_blocks(source)
    found: Set[str] = set()
    for pat in RUST_ENV_PATTERNS:
        for m in pat.finditer(source):
            found.add(m.group(1))
    return found


def scan_rust_tree(root: Path) -> Set[str]:
    found: Set[str] = set()
    if not root.exists():
        return found
    for path in root.rglob("*.rs"):
        found |= scan_rust_file(path)
    return found


# ---------------------------------------------------------------------------
# Dockerfile scanning
# ---------------------------------------------------------------------------

DOCKERFILE_ENV_RE = re.compile(
    r"^\s*ENV\s+([A-Za-z_][A-Za-z0-9_]*)", re.IGNORECASE | re.MULTILINE
)


def scan_dockerfile(path: Path) -> Set[str]:
    if not path.exists():
        return set()
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return set()
    return {m.group(1) for m in DOCKERFILE_ENV_RE.finditer(text)}


# ---------------------------------------------------------------------------
# docker-compose scanning
# ---------------------------------------------------------------------------

# Matches keys under an `environment:` block. Handles both list form
#   - KEY=value
# and map form
#   KEY: value
COMPOSE_ENV_LIST_RE = re.compile(r"^\s*-\s*([A-Za-z_][A-Za-z0-9_]*)=", re.MULTILINE)
COMPOSE_ENV_MAP_RE = re.compile(r"^\s{2,}([A-Za-z_][A-Za-z0-9_]*)\s*:", re.MULTILINE)


def scan_compose_file(path: Path) -> Set[str]:
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return set()
    found: Set[str] = set()
    # Only consider lines inside an `environment:` section.
    lines = text.splitlines()
    in_env = False
    env_indent = 0
    for line in lines:
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        indent = len(line) - len(line.lstrip())
        if re.match(r"^environment\s*:\s*$", stripped):
            in_env = True
            env_indent = indent
            continue
        if in_env:
            if indent <= env_indent and not stripped.startswith("-"):
                in_env = False
                continue
            m = re.match(r"^-\s*([A-Za-z_][A-Za-z0-9_]*)=", stripped)
            if m:
                found.add(m.group(1))
                continue
            m = re.match(r"^([A-Za-z_][A-Za-z0-9_]*)\s*:", stripped)
            if m:
                found.add(m.group(1))
    return found


def scan_compose_files(root: Path) -> Set[str]:
    found: Set[str] = set()
    for path in root.glob("docker-compose*.yml"):
        found |= scan_compose_file(path)
    for path in root.glob("docker-compose*.yaml"):
        found |= scan_compose_file(path)
    return found


# ---------------------------------------------------------------------------
# .env.example parsing
# ---------------------------------------------------------------------------

ENV_EXAMPLE_RE = re.compile(r"^([A-Z][A-Z0-9_]*)=", re.MULTILINE)


def parse_env_example(path: Path) -> Set[str]:
    if not path.exists():
        return set()
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return set()
    return {m.group(1) for m in ENV_EXAMPLE_RE.finditer(text)}


# ---------------------------------------------------------------------------
# Allowlist
# ---------------------------------------------------------------------------

def is_allowlisted(name: str) -> bool:
    return any(p.match(name) for p in ALLOWLIST_PATTERNS)


# ---------------------------------------------------------------------------
# Core logic
# ---------------------------------------------------------------------------

def collect_read_vars(
    src_root: Path = BACKEND_SRC,
    dockerfile: Path = DOCKERFILE,
    repo_root: Path = REPO_ROOT,
) -> Set[str]:
    found: Set[str] = set()
    found |= scan_rust_tree(src_root)
    found |= scan_dockerfile(dockerfile)
    found |= scan_compose_files(repo_root)
    return {v for v in found if not is_allowlisted(v)}


def collect_documented(env_example: Path = ENV_EXAMPLE) -> Set[str]:
    return parse_env_example(env_example)


def undocumented(
    src_root: Path = BACKEND_SRC,
    dockerfile: Path = DOCKERFILE,
    repo_root: Path = REPO_ROOT,
    env_example: Path = ENV_EXAMPLE,
) -> Set[str]:
    read = collect_read_vars(src_root, dockerfile, repo_root)
    documented = collect_documented(env_example)
    return read - documented


# ---------------------------------------------------------------------------
# Self-test
# ---------------------------------------------------------------------------

SELF_TEST_RUST = r'''
use std::env;

fn read_config() {
    let a = env::var("DATABASE_URL").unwrap();
    let b = std::env::var("CONSENSUS_STRATEGY").unwrap();
    let c = std::env::var_os("NODE_ID");
    let d = env_var("REDIS_URL");
    let e = Config::from_env("RPC_ENDPOINT");
    let _ = (a, b, c, d, e);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn t() {
        let _ = env::var("TEST_ONLY_VAR");
        let _ = env::var("CARGO_PKG_NAME");
    }
}
'''

SELF_TEST_DOCKERFILE = """
FROM rust:1.75
ENV AML_FAIL_OPEN=false
ENV TRACE_ENABLED=1
"""

SELF_TEST_COMPOSE = """
services:
  app:
    environment:
      - READ_REPLICA_URLS=http://a,http://b
      TRUSTED_PROXY_IPS: "127.0.0.1"
"""

SELF_TEST_ENV_EXAMPLE = """
# Documented
DATABASE_URL=postgres://localhost/db
CONSENSUS_STRATEGY=simple
"""


def run_self_test() -> int:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        src = root / "backend" / "src"
        src.mkdir(parents=True)
        (src / "lib.rs").write_text(SELF_TEST_RUST, encoding="utf-8")
        dockerfile = root / "backend" / "Dockerfile"
        dockerfile.write_text(SELF_TEST_DOCKERFILE, encoding="utf-8")
        (root / "docker-compose.yml").write_text(SELF_TEST_COMPOSE, encoding="utf-8")
        env_example = root / ".env.example"
        env_example.write_text(SELF_TEST_ENV_EXAMPLE, encoding="utf-8")

        read = collect_read_vars(src, dockerfile, root)
        expected_read = {
            "DATABASE_URL",
            "CONSENSUS_STRATEGY",
            "NODE_ID",
            "REDIS_URL",
            "RPC_ENDPOINT",
            "AML_FAIL_OPEN",
            "TRACE_ENABLED",
            "READ_REPLICA_URLS",
            "TRUSTED_PROXY_IPS",
        }
        if read != expected_read:
            print("SELF-TEST FAILED: read vars mismatch", file=sys.stderr)
            print(f"  expected: {sorted(expected_read)}", file=sys.stderr)
            print(f"  actual:   {sorted(read)}", file=sys.stderr)
            return 1

        documented = collect_documented(env_example)
        if documented != {"DATABASE_URL", "CONSENSUS_STRATEGY"}:
            print("SELF-TEST FAILED: documented vars mismatch", file=sys.stderr)
            print(f"  actual: {sorted(documented)}", file=sys.stderr)
            return 1

        missing = undocumented(src, dockerfile, root, env_example)
        expected_missing = expected_read - documented
        if missing != expected_missing:
            print("SELF-TEST FAILED: undocumented mismatch", file=sys.stderr)
            print(f"  expected: {sorted(expected_missing)}", file=sys.stderr)
            print(f"  actual:   {sorted(missing)}", file=sys.stderr)
            return 1

    print("SELF-TEST PASSED")
    return 0


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main(argv: Iterable[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--list",
        action="store_true",
        help="print the authoritative sorted list of variables read by the code",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="run embedded fixture tests for the checker itself",
    )
    args = parser.parse_args(list(argv) if argv is not None else None)

    if args.self_test:
        return run_self_test()

    read = collect_read_vars()
    documented = collect_documented()

    if args.list:
        for name in sorted(read):
            print(name)
        return 0

    missing = sorted(read - documented)
    if missing:
        print(
            "ERROR: The following environment variables are read by the code "
            "but not documented in .env.example:",
            file=sys.stderr,
        )
        for name in missing:
            print(f"  - {name}", file=sys.stderr)
        print(
            "\nAdd them to .env.example (or to the allowlist in "
            "scripts/check_env_vars.py if they are test-only/internal).",
            file=sys.stderr,
        )
        return 1

    print(f"OK: all {len(read)} environment variables are documented.")
    return 0


if __name__ == "__main__":
    sys.exit(main())