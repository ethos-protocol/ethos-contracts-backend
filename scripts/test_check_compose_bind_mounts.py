#!/usr/bin/env python3
"""Tests for scripts/check_compose_bind_mounts.py (#693)."""

import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import check_compose_bind_mounts as check  # noqa: E402

SCRIPT = os.path.join(HERE, "check_compose_bind_mounts.py")
REPO_ROOT = os.path.dirname(HERE)


def _backend(volumes):
    return {"services": {"backend": {"image": "x", "volumes": volumes}}}


class FindSourceBindMountsTests(unittest.TestCase):
    def test_short_syntax_source_bind_mount_is_rejected(self):
        found = check.find_source_bind_mounts(_backend(["./backend/src:/app/src"]))
        self.assertEqual(len(found), 1)

    def test_long_syntax_source_bind_mount_is_rejected(self):
        volume = {"type": "bind", "source": "./backend/src", "target": "/app/src"}
        self.assertEqual(len(check.find_source_bind_mounts(_backend([volume]))), 1)

    def test_bind_into_app_src_target_is_rejected_even_from_other_host_path(self):
        found = check.find_source_bind_mounts(_backend(["./mycode:/app/src"]))
        self.assertEqual(len(found), 1)

    def test_named_volumes_and_non_source_bind_mounts_are_allowed(self):
        volumes = [
            "postgres_data:/var/lib/postgresql/data",
            "./monitoring/prometheus.yml:/etc/prometheus/prometheus.yml:ro",
            {"type": "volume", "source": "data", "target": "/data"},
        ]
        self.assertEqual(check.find_source_bind_mounts(_backend(volumes)), [])

    def test_service_without_volumes_is_allowed(self):
        compose = {"services": {"redis": {"image": "redis:7-alpine"}}}
        self.assertEqual(check.find_source_bind_mounts(compose), [])


class RepositoryComposeFilesTests(unittest.TestCase):
    def test_production_compose_file_has_no_source_bind_mounts(self):
        status = subprocess.run(
            [sys.executable, SCRIPT, os.path.join(REPO_ROOT, "docker-compose.yml")],
            capture_output=True,
            text=True,
        )
        self.assertEqual(status.returncode, 0, status.stdout + status.stderr)

    def test_script_fails_on_a_production_file_with_source_mount(self):
        with tempfile.NamedTemporaryFile("w", suffix=".yml", delete=False) as fh:
            fh.write("services:\n  backend:\n    image: x\n    volumes:\n      - ./backend/src:/app/src\n")
            path = fh.name
        try:
            status = subprocess.run(
                [sys.executable, SCRIPT, path], capture_output=True, text=True
            )
            self.assertEqual(status.returncode, 1)
            self.assertIn("source bind mount", status.stdout)
        finally:
            os.unlink(path)


if __name__ == "__main__":
    unittest.main()
