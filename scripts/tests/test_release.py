"""Exercise version bumps with a real Cargo resolver and disposable release metadata."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import bump_version
import package_release


class ReleaseTests(unittest.TestCase):
    """A dependency-free crate makes locked-build regressions fast and offline."""

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.old = package_release.release_version()
        for filename in (
            "cli-python/pyproject.toml",
            "cli-python/ast_bro_cli/__init__.py",
            "cli-typescript/package.json",
            "cli-typescript/bin/install.js",
            "Formula/ast-bro.rb",
            "scripts/bump_version.py",
            "scripts/package_release.py",
        ):
            destination = self.root / filename
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(package_release.ROOT / filename, destination)
        (self.root / "src").mkdir()
        (self.root / "src/lib.rs").write_text("pub fn hello() {}\n", encoding="utf-8")
        (self.root / "Cargo.toml").write_text(
            f'[package]\nname = "ast-bro"\nversion = "{self.old}"\nedition = "2021"\n',
            encoding="utf-8",
        )
        (self.root / "Cargo.lock").write_text(
            f'version = 4\n\n[[package]]\nname = "ast-bro"\nversion = "{self.old}"\n',
            encoding="utf-8",
        )

    def test_stale_lockfile_is_rejected_before_packaging(self) -> None:
        lockfile = self.root / "Cargo.lock"
        lockfile.write_text(lockfile.read_text().replace(self.old, "0.0.0"), encoding="utf-8")
        with patch.object(package_release, "ROOT", self.root), self.assertRaisesRegex(
            ValueError, "Package versions disagree"
        ):
            package_release.release_version()

    @unittest.skipUnless(shutil.which("cargo"), "Rust is required for the release workflow test")
    def test_each_version_bump_produces_a_buildable_lockfile(self) -> None:
        major, minor, patch_number = (int(number) for number in self.old.split("."))
        expected = {
            "patch": f"{major}.{minor}.{patch_number + 1}",
            "minor": f"{major}.{minor + 1}.0",
            "major": f"{major + 1}.0.0",
        }
        snapshots = {path: path.read_bytes() for path in self.root.rglob("*") if path.is_file()}
        env = dict(os.environ, CARGO_NET_OFFLINE="true", CARGO_TARGET_DIR=str(self.root / "target"))
        for part, version in expected.items():
            with self.subTest(part=part):
                for path, content in snapshots.items():
                    path.write_bytes(content)
                result = subprocess.run(
                    [sys.executable, str(self.root / "scripts/bump_version.py"), part],
                    cwd=self.root,
                    env=env,
                    capture_output=True,
                    text=True,
                    check=False,
                    timeout=30,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                with patch.object(package_release, "ROOT", self.root):
                    self.assertEqual(package_release.release_version(), version)
                result = subprocess.run(
                    ["cargo", "build", "--release", "--locked", "--offline"],
                    cwd=self.root,
                    env=env,
                    capture_output=True,
                    text=True,
                    check=False,
                    timeout=60,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                formula = (self.root / "Formula/ast-bro.rb").read_text(encoding="utf-8")
                self.assertIn(f"/download/{version}/", formula)
                package = json.loads(
                    (self.root / "cli-typescript/package.json").read_text(encoding="utf-8")
                )
                self.assertEqual(package["version"], version)

    def test_failed_lockfile_refresh_restores_all_versions(self) -> None:
        snapshots = {path: path.read_bytes() for path in self.root.rglob("*") if path.is_file()}
        with patch.object(package_release, "ROOT", self.root), patch.object(
            bump_version, "ROOT", self.root
        ), patch(
            "bump_version.subprocess.run", side_effect=subprocess.CalledProcessError(101, "cargo")
        ), self.assertRaises(subprocess.CalledProcessError):
            bump_version.bump_version("patch")
        for path, content in snapshots.items():
            self.assertEqual(path.read_bytes(), content, str(path))


if __name__ == "__main__":
    unittest.main()
