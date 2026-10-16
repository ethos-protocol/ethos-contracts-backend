#!/usr/bin/env python3
"""CI guard (#693): production compose files must not bind-mount source code.

A bind mount of `backend/src` (or any other source directory) into a service
means a host edit changes the running code, and the running image no longer
matches the one that was built and scanned. Source bind mounts belong in
`docker-compose.override.yml`, which is only used for local development.

Usage:
    scripts/check_compose_bind_mounts.py [compose-file ...]

With no arguments, checks `docker-compose.yml` and any
`docker-compose.prod*.yml` / `docker-compose.production*.yml` file.
Exits 1 when a source bind mount is found in a checked file.
"""

from __future__ import annotations

import glob
import os
import sys

import yaml

# Host paths (relative to the repo root) that hold source code.
SOURCE_DIRS = ("backend", "contracts", "clients")
# Container paths that hold source code inside the Dockerfile's image.
SOURCE_TARGETS = ("/app/src", "/app/backend", "/app/contracts")

DEFAULT_GLOBS = (
    "docker-compose.yml",
    "docker-compose.prod*.yml",
    "docker-compose.production*.yml",
)


def _normalize(path: str) -> str:
    norm = os.path.normpath(path).replace(os.sep, "/")
    return norm[2:] if norm.startswith("./") else norm


def _is_source_host_path(host: str) -> bool:
    norm = _normalize(host)
    if norm == "src" or norm.endswith("/src"):
        return True
    return any(norm == d or norm.startswith(d + "/") for d in SOURCE_DIRS)


def _is_source_target(target: str) -> bool:
    norm = target.rstrip("/")
    return any(norm == t or norm.startswith(t + "/") for t in SOURCE_TARGETS)


def find_source_bind_mounts(compose: dict) -> list[str]:
    """Return a description of every source bind mount in a parsed compose file."""
    violations: list[str] = []
    services = compose.get("services") or {}
    for name, service in services.items():
        for volume in (service or {}).get("volumes") or []:
            host, target = _split_volume(volume)
            if host is None:
                continue
            if _is_source_host_path(host) or _is_source_target(target or ""):
                violations.append(f"service '{name}': {host!r} -> {target!r}")
    return violations


def _split_volume(volume: object) -> tuple[str | None, str | None]:
    """Return (host_path, container_path) for bind mounts, or (None, None) otherwise."""
    if isinstance(volume, dict):
        if volume.get("type") != "bind":
            return None, None
        return str(volume.get("source", "")), str(volume.get("target", ""))
    if not isinstance(volume, str):
        return None, None
    parts = volume.split(":")
    if len(parts) < 2:
        return None, None  # anonymous or named volume without a host path
    host, target = parts[0], parts[1]
    if not (host.startswith((".", "/", "~")) or "/" in host):
        return None, None  # named volume such as `postgres_data`
    return host, target


def check_files(paths: list[str]) -> int:
    failures = 0
    for path in paths:
        with open(path, encoding="utf-8") as fh:
            compose = yaml.safe_load(fh) or {}
        for violation in find_source_bind_mounts(compose):
            print(f"FAIL: {path}: source bind mount in production compose file: {violation}")
            failures += 1
    if failures:
        print(
            "\nSource code must reach production via the built image. Move bind mounts "
            "of source directories to docker-compose.override.yml (#693).",
            file=sys.stderr,
        )
    return 1 if failures else 0


def main(argv: list[str]) -> int:
    if argv:
        paths = argv
    else:
        paths = sorted({p for pattern in DEFAULT_GLOBS for p in glob.glob(pattern)})
    if not paths:
        print("No production compose files found to check.", file=sys.stderr)
        return 2
    status = check_files(paths)
    if status == 0:
        print(f"OK: no source bind mounts in {', '.join(paths)}")
    return status


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
