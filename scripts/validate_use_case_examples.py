#!/usr/bin/env python3
"""Validate example request payloads in docs/use-cases.md against docs/openapi.yaml.

Extracts every ```bash code fence containing a `curl ... -d '<json>'` call,
matches the URL path against a path defined in openapi.yaml, and checks that
the JSON payload's top-level keys are a subset of the matching request
schema's declared properties (and that all required properties are present).

This is intentionally lightweight (regex + PyYAML, no full OpenAPI request
validation) so it can run in CI without extra service dependencies. See the
"Validating use-cases.md examples" section in docs/use-cases.md for the
process this script implements.

Exit code 0 if every extracted example matches a known path/schema, 1 if any
example references an undefined path, uses fields not in the schema, or is
missing required fields.
"""
import json
import re
import sys
from pathlib import Path

try:
    import yaml
except ImportError:
    print("PyYAML is required: pip install pyyaml", file=sys.stderr)
    sys.exit(2)

REPO_ROOT = Path(__file__).resolve().parent.parent
USE_CASES_PATH = REPO_ROOT / "docs" / "use-cases.md"
OPENAPI_PATH = REPO_ROOT / "docs" / "openapi.yaml"

CURL_BLOCK_RE = re.compile(r"```bash\n(.*?)```", re.DOTALL)
CURL_CALL_RE = re.compile(
    r"curl\s+-X\s+(?P<method>GET|POST|PUT|PATCH|DELETE)\s+"
    r"\S*?(?P<path>/[^\s'\"]+)"
    r".*?-d\s+'(?P<body>\{.*?\})'",
    re.DOTALL,
)


def load_openapi(path: Path) -> dict:
    with path.open() as f:
        return yaml.safe_load(f)


def resolve_ref(spec: dict, ref: str) -> dict:
    assert ref.startswith("#/")
    node = spec
    for part in ref.lstrip("#/").split("/"):
        node = node[part]
    return node


def find_path_item(spec: dict, path: str) -> str | None:
    """Match a concrete request path (e.g. /api/vaults/42/export) to an
    OpenAPI templated path (e.g. /api/vaults/{vault_id}/export)."""
    for template in spec.get("paths", {}):
        pattern = re.sub(r"\{[^}]+\}", r"[^/]+", template)
        if re.fullmatch(pattern, path):
            return template
    return None


def request_schema_for(spec: dict, template: str, method: str) -> dict | None:
    op = spec["paths"][template].get(method.lower())
    if not op:
        return None
    body = op.get("requestBody")
    if not body:
        return None
    schema = body["content"]["application/json"]["schema"]
    if "$ref" in schema:
        schema = resolve_ref(spec, schema["$ref"])
    return schema


def main() -> int:
    spec = load_openapi(OPENAPI_PATH)
    text = USE_CASES_PATH.read_text()

    failures = []
    checked = 0

    for block in CURL_BLOCK_RE.findall(text):
        match = CURL_CALL_RE.search(block)
        if not match:
            continue
        checked += 1
        method = match.group("method")
        path = match.group("path")
        try:
            body = json.loads(match.group("body"))
        except json.JSONDecodeError as exc:
            failures.append(f"{method} {path}: example body is not valid JSON ({exc})")
            continue

        template = find_path_item(spec, path)
        if template is None:
            failures.append(f"{method} {path}: no matching path in docs/openapi.yaml")
            continue

        schema = request_schema_for(spec, template, method)
        if schema is None:
            failures.append(f"{method} {path}: no {method} request body schema for {template}")
            continue

        properties = set(schema.get("properties", {}).keys())
        required = set(schema.get("required", []))
        provided = set(body.keys())

        unknown = provided - properties
        missing = required - provided
        if unknown:
            failures.append(f"{method} {path}: example uses unknown field(s) {sorted(unknown)}")
        if missing:
            failures.append(f"{method} {path}: example missing required field(s) {sorted(missing)}")

    if failures:
        print("docs/use-cases.md example validation FAILED:")
        for f in failures:
            print(f"  - {f}")
        return 1

    print(f"docs/use-cases.md: {checked} example request(s) validated against docs/openapi.yaml OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
