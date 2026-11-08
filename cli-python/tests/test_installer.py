"""Platform selection, cache upgrades, and failed-download regressions."""

from __future__ import annotations

import io
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

import httpx

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import ast_bro_cli as cli


class InstallerTests(unittest.TestCase):
    """Keep all network requests and cached files isolated from the user's setup."""

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.cache = Path(self.temp.name) / "User's cache & [files]"
        for target, value in (
            ("ast_bro_cli.platform.system", "Windows"),
            ("ast_bro_cli.platform.machine", "AMD64"),
            ("ast_bro_cli.get_cache_dir", self.cache),
        ):
            patcher = patch(target, return_value=value)
            patcher.start()
            self.addCleanup(patcher.stop)

    def response(self, names: tuple[str, ...]) -> httpx.Response:
        """Create a release ZIP without invoking a compiler."""
        data = io.BytesIO()
        with zipfile.ZipFile(data, "w") as archive:
            for name in names:
                archive.writestr(name, b"binary")
        return httpx.Response(
            200, content=data.getvalue(), request=httpx.Request("GET", "https://example.test")
        )

    def test_platforms(self) -> None:
        self.assertEqual(cli.get_platform(), ("windows", "x86_64"))
        with patch("ast_bro_cli.platform.machine", return_value="ARM64"):
            with self.assertRaisesRegex(RuntimeError, "windows-arm64"):
                cli.get_platform()
            with patch("ast_bro_cli.platform.system", return_value="Darwin"):
                self.assertEqual(cli.get_platform(), ("macos", "arm64"))

    def test_download_cache_and_stderr(self) -> None:
        response = self.response(("ast-bro.exe", "ast-outline.exe", "sb.exe"))
        stdout = io.StringIO()
        with patch("httpx.Client.get", return_value=response) as download, patch(
            "sys.stdout", stdout
        ):
            binary = cli.get_binary_path()
            self.assertEqual(binary, self.cache / "ast-bro.exe")
            self.assertEqual(cli.get_binary_path(), binary)
            download.assert_called_once_with(
                f"https://github.com/aeroxy/ast-bro/releases/download/{cli.VERSION}/ast-bro-windows-x86_64.zip"
            )
        self.assertEqual(stdout.getvalue(), "")
        self.assertEqual(len(list(self.cache.iterdir())), 3)

    def test_incomplete_archive_is_not_cached(self) -> None:
        with patch(
            "httpx.Client.get", return_value=self.response(("ast-bro.exe",))
        ), self.assertRaisesRegex(RuntimeError, "missing ast-outline.exe"):
            cli.download_binary()
        self.assertEqual(list(self.cache.iterdir()), [])

    def test_http_error_is_not_cached(self) -> None:
        response = httpx.Response(404, request=httpx.Request("GET", "https://example.test"))
        with patch("httpx.Client.get", return_value=response), self.assertRaises(
            httpx.HTTPStatusError
        ):
            cli.download_binary()
        self.assertEqual(list(self.cache.iterdir()), [])


class CacheVersionTests(unittest.TestCase):
    """Upgrades must not accidentally execute the previous release."""

    def test_windows_cache_is_versioned_and_has_a_fallback(self) -> None:
        with patch("sys.platform", "win32"), patch.dict(
            "os.environ", {"LOCALAPPDATA": "cache-root"}
        ):
            first = cli.get_cache_dir()
            with patch("ast_bro_cli.VERSION", "999.0.0"):
                self.assertNotEqual(first, cli.get_cache_dir())
            self.assertEqual(first, Path("cache-root") / "ast-bro" / "Cache" / cli.VERSION)
        with patch("sys.platform", "win32"), patch.dict("os.environ", {"LOCALAPPDATA": ""}):
            self.assertEqual(
                cli.get_cache_dir(), Path.home() / "AppData/Local/ast-bro/Cache" / cli.VERSION
            )


if __name__ == "__main__":
    unittest.main()
