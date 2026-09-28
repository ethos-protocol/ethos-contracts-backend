#!/usr/bin/env python3
"""Validate docs/api-documentation.md against the backend source.

Checks:
  1. The endpoint index (between the endpoint-index markers) lists exactly
     the (method, path) pairs registered with `.route(...)` in
     backend/src/main.rs (commented-out routes are ignored).
  2. Every `curl -X METHOD <url>` example targets a registered route with an
     allowed method.
  3. Every `-d '<json>'` body parses, only uses fields declared on the Rust
     request struct for that route, and includes every required field
     (fields that are neither `Option<...>` nor `#[serde(default)]`).
  4. Every ```json block parses.
  5. Every ```python block is syntactically valid.
  6. Every error code in the error-code table (between the error-codes
     markers) exists as a string literal in backend/src, and codes produced
     by `AppError` map to the HTTP status the table claims.

Stdlib only. Exit code 0 on success, 1 on failure.
"""
import json
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DOC_PATH = REPO_ROOT / "docs" / "api-documentation.md"
BACKEND_SRC = REPO_ROOT / "backend" / "src"
MAIN_RS = BACKEND_SRC / "main.rs"
ERROR_RS = BACKEND_SRC / "error.rs"

HTTP_METHODS = ("get", "post", "put", "delete", "patch")

# Route (method, path template) -> (source file, request struct name).
REQUEST_STRUCTS = {
    ("POST", "/api/vaults/:vault_id/reminder-preferences"): ("models.rs", "SetPreferencesRequest"),
    ("POST", "/api/vaults/:vault_id/subscriptions"): ("models.rs", "SetSubscriptionRequest"),
    ("POST", "/webhooks"): ("webhook.rs", "RegisterWebhookRequest"),
    ("POST", "/webhooks/verify"): ("webhook.rs", "VerifyWebhookRequest"),
    ("POST", "/admin/flags"): ("feature_flags.rs", "UpsertFlagRequest"),
    ("POST", "/admin/flags/:key/evaluate"): ("feature_flags.rs", "EvaluateFlagRequest"),
    ("POST", "/admin/capabilities"): ("degradation.rs", "SetCapabilityRequest"),
    ("POST", "/capabilities/negotiate"): ("degradation.rs", "NegotiateRequest"),
    ("POST", "/anomaly/observe"): ("anomaly_detection.rs", "ObserveRequest"),
    ("POST", "/anomaly/seasonality"): ("anomaly_detection.rs", "SeasonalityRequest"),
    ("PUT", "/anomaly/seasonality/threshold"): ("anomaly_detection.rs", "SeasonalThresholdRequest"),
    ("POST", "/anomaly/investigations/:anomaly_id"): ("anomaly_detection.rs", "InvestigationRequest"),
    ("POST", "/aml/flags"): ("aml.rs", "FlagRequest"),
    ("POST", "/webauthn/register/begin"): ("webauthn.rs", "BeginRegistrationRequest"),
    ("POST", "/webauthn/register/complete"): ("webauthn.rs", "CompleteRegistrationRequest"),
    ("POST", "/webauthn/authenticate/begin"): ("webauthn.rs", "BeginAuthenticationRequest"),
    ("POST", "/webauthn/authenticate/complete"): ("webauthn.rs", "CompleteAuthenticationRequest"),
}

STATUS_CODES = {
    "BAD_REQUEST": 400, "UNAUTHORIZED": 401, "FORBIDDEN": 403, "NOT_FOUND": 404,
    "METHOD_NOT_ALLOWED": 405, "CONFLICT": 409, "UNPROCESSABLE_ENTITY": 422,
    "TOO_MANY_REQUESTS": 429, "INTERNAL_SERVER_ERROR": 500, "SERVICE_UNAVAILABLE": 503,
}

FAILURES: list[str] = []


def check(condition: bool, message: str) -> None:
    print(f"  {'ok  ' if condition else 'FAIL'} {message}")
    if not condition:
        FAILURES.append(message)


def strip_line_comments(source: str) -> str:
    return "\n".join(line for line in source.splitlines() if not line.lstrip().startswith("//"))


def registered_routes() -> set[tuple[str, str]]:
    source = strip_line_comments(MAIN_RS.read_text())
    routes: set[tuple[str, str]] = set()
    starts = [m for m in re.finditer(r"\.route\(\s*\"([^\"]+)\"", source)]
    for index, match in enumerate(starts):
        end = starts[index + 1].start() if index + 1 < len(starts) else len(source)
        segment = source[match.end():end]
        # Stop at the next builder call that is not part of this route.
        cut = re.search(r"\.(layer|with_state|merge)\(|;", segment)
        if cut:
            segment = segment[:cut.start()]
        for method in re.findall(rf"\b({'|'.join(HTTP_METHODS)})\(", segment):
            routes.add((method.upper(), match.group(1)))
    return routes


def documented_index(doc: str) -> set[tuple[str, str]]:
    block = re.search(r"<!-- endpoint-index:start -->(.*?)<!-- endpoint-index:end -->", doc, re.DOTALL)
    if not block:
        return set()
    return set(re.findall(r"^\|\s*(GET|POST|PUT|DELETE|PATCH)\s*\|\s*`([^`]+)`", block.group(1), re.MULTILINE))


def match_route(method: str, path: str, routes: set[tuple[str, str]]) -> tuple[str, str] | None:
    parts = path.rstrip("/").split("/") or ["/"]
    for route_method, template in routes:
        tparts = template.rstrip("/").split("/")
        if len(tparts) != len(parts):
            continue
        if all(t.startswith(":") or t == p for t, p in zip(tparts, parts)):
            if route_method == method:
                return (route_method, template)
    return None


def path_exists(path: str, routes: set[tuple[str, str]]) -> bool:
    return any(match_route(m, path, routes) for m in {r[0] for r in routes})


def struct_fields(file_name: str, struct: str) -> tuple[set[str], set[str]] | None:
    source = (BACKEND_SRC / file_name).read_text()
    match = re.search(rf"pub struct {struct}\s*\{{(.*?)\n\}}", source, re.DOTALL)
    if not match:
        return None
    all_fields, required = set(), set()
    pending_default = False
    for line in match.group(1).splitlines():
        stripped = line.strip()
        if stripped.startswith("#[serde(") and "default" in stripped:
            pending_default = True
            continue
        field = re.match(r"pub (\w+):\s*(.+?),?$", stripped)
        if field:
            name, ty = field.groups()
            all_fields.add(name)
            if not pending_default and not ty.startswith("Option<"):
                required.add(name)
            pending_default = False
    return all_fields, required


def curl_commands(doc: str) -> list[str]:
    commands = []
    for block in re.findall(r"```bash\n(.*?)```", doc, re.DOTALL):
        joined = block.replace("\\\n", " ")
        for line in joined.splitlines():
            if "curl " in line:
                commands.append(line[line.index("curl "):])
    return commands


CURL_RE = re.compile(
    r"curl\s+(?:-\w+\s+)*-X\s+(GET|POST|PUT|DELETE|PATCH)\s+['\"]?(?:https?://[^/\s'\"]+)(/[^\s'\"?]*)"
)
BODY_RE = re.compile(r"-d\s+'(\{.*?\})'(?:\s|$)")


def check_index(doc: str, routes: set[tuple[str, str]]) -> None:
    documented = documented_index(doc)
    check(bool(routes), f"parsed {len(routes)} routes from main.rs")
    check(bool(documented), f"parsed {len(documented)} rows from the endpoint index")
    for missing in sorted(routes - documented):
        check(False, f"route {missing[0]} {missing[1]} is registered but not documented")
    for extra in sorted(documented - routes):
        check(False, f"documented endpoint {extra[0]} {extra[1]} is not registered in main.rs")
    if routes == documented:
        check(True, "endpoint index matches main.rs exactly")


def check_curls(doc: str, routes: set[tuple[str, str]]) -> None:
    commands = curl_commands(doc)
    check(len(commands) >= 20, f"found {len(commands)} curl examples")
    for command in commands:
        parsed = CURL_RE.search(command)
        if not parsed:
            check(False, f"curl example is parseable: {command[:80]}")
            continue
        method, path = parsed.groups()
        route = match_route(method, path, routes)
        check(route is not None, f"curl {method} {path} hits a registered route")
        body = BODY_RE.search(command)
        if not body:
            continue
        try:
            payload = json.loads(body.group(1))
        except json.JSONDecodeError as err:
            check(False, f"curl {method} {path} body is valid JSON ({err})")
            continue
        check(True, f"curl {method} {path} body is valid JSON")
        if route is None:
            continue
        spec = REQUEST_STRUCTS.get(route)
        if spec is None:
            check(route[1] == "/graphql", f"request struct known for {method} {route[1]}")
            continue
        fields = struct_fields(*spec)
        if fields is None:
            check(False, f"struct {spec[1]} found in backend/src/{spec[0]}")
            continue
        allowed, required = fields
        unknown = set(payload) - allowed
        missing = required - set(payload)
        check(not unknown, f"{method} {path} body fields ⊆ {spec[1]} (unknown: {sorted(unknown) or 'none'})")
        check(not missing, f"{method} {path} body has all required {spec[1]} fields (missing: {sorted(missing) or 'none'})")


def check_json_blocks(doc: str) -> None:
    blocks = re.findall(r"```json\n(.*?)```", doc, re.DOTALL)
    check(len(blocks) >= 10, f"found {len(blocks)} json examples")
    for index, block in enumerate(blocks, start=1):
        try:
            json.loads(block)
            check(True, f"json example {index} parses")
        except json.JSONDecodeError as err:
            check(False, f"json example {index} parses ({err})")


def check_python_blocks(doc: str) -> None:
    for index, block in enumerate(re.findall(r"```python\n(.*?)```", doc, re.DOTALL), start=1):
        try:
            compile(block, f"<api doc python {index}>", "exec")
            check(True, f"python example {index} compiles")
        except SyntaxError as err:
            check(False, f"python example {index} compiles ({err})")


def check_error_codes(doc: str) -> None:
    block = re.search(r"<!-- error-codes:start -->(.*?)<!-- error-codes:end -->", doc, re.DOTALL)
    check(block is not None, "error-code table present")
    if not block:
        return
    rows = re.findall(r"^\|\s*(\d{3})\s*\|\s*`(\w+)`", block.group(1), re.MULTILINE)
    check(len(rows) >= 8, f"error-code table has {len(rows)} rows")

    all_source = "\n".join(p.read_text() for p in BACKEND_SRC.glob("*.rs"))
    app_error_map = {
        code: STATUS_CODES[status]
        for status, code in re.findall(r"\(StatusCode::(\w+),\s*\"(\w+)\"\)", ERROR_RS.read_text())
        if status in STATUS_CODES
    }
    for status, code in rows:
        check(f'"{code}"' in all_source, f"error code `{code}` exists in backend/src")
        if code in app_error_map:
            check(app_error_map[code] == int(status),
                  f"`{code}` documented as {status}, error.rs maps it to {app_error_map[code]}")
    documented = {code for _, code in rows}
    for code in sorted(set(app_error_map) - documented):
        check(False, f"AppError code `{code}` is not documented")


def main() -> int:
    if not DOC_PATH.exists():
        print(f"missing {DOC_PATH}", file=sys.stderr)
        return 1
    doc = DOC_PATH.read_text()
    routes = registered_routes()

    print("Checking endpoint index...")
    check_index(doc, routes)
    print("Checking curl examples...")
    check_curls(doc, routes)
    print("Checking JSON examples...")
    check_json_blocks(doc)
    print("Checking Python examples...")
    check_python_blocks(doc)
    print("Checking error codes...")
    check_error_codes(doc)

    if FAILURES:
        print(f"\n{len(FAILURES)} check(s) failed.")
        return 1
    print("\nAPI documentation validated.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
