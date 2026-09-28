#!/usr/bin/env python3
"""Validate docs/troubleshooting.md against the source tree.

Checks:
  1. Every row in each contract error table (errors:<contract> markers) has
     the same number and name as the contract's `#[contracterror]` enum.
  2. Every API error code quoted in a JSON example exists in backend/src.
  3. Every log signature (log-signatures markers) exists verbatim in the
     source file it is attributed to.
  4. Every structured log line in a ```log block (one starting with an
     RFC 3339 timestamp) follows docs/log-format.md:
       <timestamp> <LEVEL> <target>: <message> [key=value ...]
  5. Every `→ §N` / `→ §N.M` reference in the diagnosis tree resolves to a
     heading in this guide.
  6. Every JSON example parses, every relative markdown link resolves.

Stdlib only. Exit code 0 on success, 1 on failure.
"""
import json
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
GUIDE = REPO_ROOT / "docs" / "troubleshooting.md"
BACKEND_SRC = REPO_ROOT / "backend" / "src"

CONTRACT_ENUMS = {
    "ttl_vault": (REPO_ROOT / "contracts" / "ttl_vault" / "src" / "lib.rs", "ContractError"),
    "zk_verifier": (REPO_ROOT / "contracts" / "zk_verifier" / "src" / "lib.rs", "VerifierError"),
    "sbt": (REPO_ROOT / "contracts" / "sbt" / "src" / "lib.rs", "SbtError"),
}

# Minimum rows each table must keep, so the guide can't silently shrink.
MIN_ROWS = {"ttl_vault": 20, "zk_verifier": 21, "sbt": 10}

LOG_LINE_RE = re.compile(
    r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z\s+"
    r"(TRACE|DEBUG|INFO|WARN|ERROR)\s+"
    r"[\w:]+:\s+\S.*$"
)

FAILURES: list[str] = []


def check(condition: bool, message: str) -> None:
    print(f"  {'ok  ' if condition else 'FAIL'} {message}")
    if not condition:
        FAILURES.append(message)


def enum_variants(path: Path, enum: str) -> dict[str, int]:
    source = path.read_text()
    match = re.search(rf"pub enum {enum}\s*\{{(.*?)\n\}}", source, re.DOTALL)
    if not match:
        return {}
    return {name: int(num) for name, num in re.findall(r"^\s*(\w+)\s*=\s*(\d+),", match.group(1), re.MULTILINE)}


def check_contract_tables(doc: str) -> None:
    for contract, (path, enum) in CONTRACT_ENUMS.items():
        variants = enum_variants(path, enum)
        check(bool(variants), f"parsed {len(variants)} {enum} variants from {path.relative_to(REPO_ROOT)}")
        block = re.search(rf"<!-- errors:{contract}:start -->(.*?)<!-- errors:{contract}:end -->", doc, re.DOTALL)
        check(block is not None, f"{contract} error table present")
        if not block:
            continue
        rows = re.findall(r"^\|\s*(\d+)\s*\|\s*`(\w+)`", block.group(1), re.MULTILINE)
        check(len(rows) >= MIN_ROWS[contract], f"{contract} table has {len(rows)} rows (min {MIN_ROWS[contract]})")
        seen = set()
        for num, name in rows:
            check(name not in seen, f"{contract} `{name}` listed once")
            seen.add(name)
            check(variants.get(name) == int(num),
                  f"{contract} #{num} `{name}` matches {enum} (source: {variants.get(name)})")


def check_json_and_api_codes(doc: str) -> None:
    all_source = "\n".join(p.read_text() for p in BACKEND_SRC.glob("*.rs"))
    blocks = re.findall(r"```json\n(.*?)```", doc, re.DOTALL)
    check(len(blocks) >= 3, f"found {len(blocks)} json examples")
    for index, block in enumerate(blocks, start=1):
        try:
            payload = json.loads(block)
        except json.JSONDecodeError as err:
            check(False, f"json example {index} parses ({err})")
            continue
        check(True, f"json example {index} parses")
        code = payload.get("code") or payload.get("error")
        if code:
            check(f'"{code}"' in all_source, f"API error code `{code}` exists in backend/src")

    for code in sorted(set(re.findall(r"^### 8\.\d+ \d{3} (.+)$", doc, re.MULTILINE))):
        for name in re.findall(r"`(\w+)`", code):
            check(f'"{name}"' in all_source, f"§8 heading code `{name}` exists in backend/src")


def check_log_signatures(doc: str) -> None:
    block = re.search(r"<!-- log-signatures:start -->(.*?)<!-- log-signatures:end -->", doc, re.DOTALL)
    check(block is not None, "log signature table present")
    if not block:
        return
    rows = re.findall(r"^\|\s*`([^`]+)`\s*\|\s*`([^`]+)`", block.group(1), re.MULTILINE)
    check(len(rows) >= 10, f"log signature table has {len(rows)} rows")
    for text, file_name in rows:
        path = REPO_ROOT / file_name
        check(path.is_file() and text in path.read_text(), f"`{text}` found in {file_name}")


def check_log_examples(doc: str) -> None:
    structured = 0
    for block in re.findall(r"```log\n(.*?)```", doc, re.DOTALL):
        for line in block.strip().splitlines():
            if re.match(r"^\d{4}-\d{2}-\d{2}T", line):
                structured += 1
                check(LOG_LINE_RE.match(line) is not None, f"log line follows log-format.md: {line[:70]}")
    check(structured >= 6, f"guide has {structured} structured log examples")


def check_tree_references(doc: str) -> None:
    tree = re.search(r"## 1\. Diagnosis tree\s*```text\n(.*?)```", doc, re.DOTALL)
    check(tree is not None, "diagnosis tree present")
    if not tree:
        return
    headings = set(re.findall(r"^#{2,3} (\d+(?:\.\d+)?)[. ]", doc, re.MULTILINE))
    refs = re.findall(r"→ §(\d+(?:\.\d+)?)(?:\s*–\s*§(\d+(?:\.\d+)?))?", tree.group(1))
    check(len(refs) >= 15, f"diagnosis tree has {len(refs)} references")
    for start, end in refs:
        for ref in filter(None, (start, end)):
            check(ref in headings, f"tree reference §{ref} resolves to a heading")


def check_links(doc: str) -> None:
    for link in sorted(set(re.findall(r"\]\(((?:\.\./)?[\w./-]+\.md)(?:#[\w-]+)?\)", doc))):
        check((GUIDE.parent / link).resolve().is_file(), f"link {link} resolves")


def main() -> int:
    if not GUIDE.exists():
        print(f"missing {GUIDE}", file=sys.stderr)
        return 1
    doc = GUIDE.read_text()
    for title, fn in [
        ("contract error tables", check_contract_tables),
        ("JSON examples and API codes", check_json_and_api_codes),
        ("log signatures", check_log_signatures),
        ("log examples", check_log_examples),
        ("diagnosis tree", check_tree_references),
        ("links", check_links),
    ]:
        print(f"Checking {title}...")
        fn(doc)

    if FAILURES:
        print(f"\n{len(FAILURES)} check(s) failed.")
        return 1
    print("\nTroubleshooting guide validated.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
