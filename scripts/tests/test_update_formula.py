"""Keep Homebrew's checksum tied to uploaded bytes, never a separate local build."""

from __future__ import annotations

import hashlib
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import update_formula


class FormulaTests(unittest.TestCase):
    """Exercise checksum updates without changing the real formula or GitHub release."""

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.formula = self.root / "Formula/ast-bro.rb"
        self.formula.parent.mkdir()
        self.version = "4.2.1"
        self.original = (
            "class AstBro < Formula\n"
            f'  url "https://github.com/aeroxy/ast-bro/releases/download/{self.version}/ast-bro-macos-arm64.zip"\n'
            f'  sha256 "{"0" * 64}"\n'
            "end\n"
        ).encode()
        self.formula.write_bytes(self.original)
        self.addCleanup(patch.stopall)
        patch.object(update_formula, "ROOT", self.root).start()
        patch.object(
            update_formula, "release_version", return_value=self.version
        ).start()

    def test_hashes_the_release_asset_instead_of_a_local_build(self) -> None:
        """The formula must accept CI's archive even when a different local ZIP exists."""
        local = self.root / "target/release/ast-bro-macos-arm64.zip"
        local.parent.mkdir(parents=True)
        local.write_bytes(b"a different local build")
        released = b"the exact archive attached to the draft release"
        directories = []

        def download(command: list[str], *, check: bool) -> None:
            self.assertTrue(check)
            self.assertEqual(command[:4], ["gh", "release", "download", self.version])
            self.assertEqual(command[command.index("--repo") + 1], "aeroxy/ast-bro")
            self.assertEqual(command[command.index("--pattern") + 1], local.name)
            directory = Path(command[command.index("--dir") + 1])
            directories.append(directory)
            (directory / local.name).write_bytes(released)

        with patch("update_formula.subprocess.run", side_effect=download):
            digest = update_formula.update_formula()
        self.assertEqual(digest, hashlib.sha256(released).hexdigest())
        self.assertEqual(
            self.formula.read_bytes(), self.original.replace(b"0" * 64, digest.encode())
        )
        self.assertEqual(local.read_bytes(), b"a different local build")
        self.assertTrue(all(not directory.exists() for directory in directories))

    def test_failed_download_preserves_the_formula(self) -> None:
        """A missing draft asset or authentication failure cannot write a bogus checksum."""
        with patch(
            "update_formula.subprocess.run",
            side_effect=subprocess.CalledProcessError(1, "gh"),
        ), self.assertRaises(subprocess.CalledProcessError):
            update_formula.update_formula()
        self.assertEqual(self.formula.read_bytes(), self.original)

    def test_missing_download_preserves_the_formula(self) -> None:
        """Do not fall back to a local archive when the expected download is absent."""
        with patch("update_formula.subprocess.run"), self.assertRaises(
            FileNotFoundError
        ):
            update_formula.update_formula()
        self.assertEqual(self.formula.read_bytes(), self.original)

    def test_invalid_formula_is_rejected_before_downloading(self) -> None:
        """Reject stale URLs and missing or ambiguous checksum fields without edits."""
        for content in (
            self.original.replace(b"/4.2.1/", b"/4.2.0/"),
            self.original.replace(b"sha256", b"missing"),
            self.original + f'  sha256 "{"0" * 64}"\n'.encode(),
        ):
            with self.subTest(content=content):
                self.formula.write_bytes(content)
                with patch(
                    "update_formula.subprocess.run"
                ) as download, self.assertRaises(ValueError):
                    update_formula.update_formula()
                download.assert_not_called()
                self.assertEqual(self.formula.read_bytes(), content)


if __name__ == "__main__":
    unittest.main()
