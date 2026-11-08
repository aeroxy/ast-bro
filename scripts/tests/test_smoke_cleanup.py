"""Regressions for hung installer launchers and cleanup error handling."""

from __future__ import annotations

import io
import os
import subprocess
import sys
import unittest
from pathlib import Path
from unittest.mock import Mock, call, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import smoke_packages


class SmokeCleanupTests(unittest.TestCase):
    """Cleanup must reach launcher children and preserve the original failure."""

    def test_graceful_exit_does_not_kill_processes(self) -> None:
        process = Mock(pid=1234)
        with patch("smoke_packages.subprocess.run") as run:
            smoke_packages.cleanup_processes([process])
        process.communicate.assert_called_once_with(timeout=30)
        process.kill.assert_not_called()
        run.assert_not_called()

    def test_timeout_kills_windows_process_tree_before_draining_pipes(self) -> None:
        process = Mock(pid=1234)
        process.communicate.side_effect = [subprocess.TimeoutExpired("launcher", 30), ("", "")]
        events = Mock()
        events.attach_mock(process, "process")
        with patch("smoke_packages.sys.platform", "win32"), patch.dict(
            os.environ, {"SystemRoot": r"D:\Windows"}
        ), patch("smoke_packages.subprocess.run") as run:
            events.attach_mock(run, "run")
            smoke_packages.cleanup_processes([process])
        self.assertEqual(
            events.mock_calls,
            [
                call.process.communicate(timeout=30),
                call.run(
                    [
                        str(Path(r"D:\Windows") / "System32/taskkill.exe"),
                        "/PID",
                        "1234",
                        "/T",
                        "/F",
                    ],
                    capture_output=True,
                    timeout=10,
                    check=False,
                ),
                call.process.communicate(timeout=10),
            ],
        )
        process.kill.assert_not_called()

    def test_cleanup_failure_preserves_original_error_and_cleans_other_processes(self) -> None:
        failed = Mock(pid=1234)
        failed.communicate.side_effect = subprocess.TimeoutExpired("launcher", 30)
        other = Mock(pid=5678)
        stderr = io.StringIO()
        with patch("smoke_packages.sys.platform", "win32"), patch(
            "smoke_packages.subprocess.run", side_effect=OSError("taskkill failed")
        ), patch("sys.stderr", stderr), self.assertRaisesRegex(AssertionError, "original failure"):
            try:
                raise AssertionError("original failure")
            finally:
                smoke_packages.cleanup_processes([failed, other])
        other.communicate.assert_called_once_with(timeout=30)
        self.assertIn("taskkill failed", stderr.getvalue())

    def test_cleanup_timeout_after_tree_kill_is_not_silently_ignored(self) -> None:
        failed = Mock(pid=1234)
        failed.communicate.side_effect = subprocess.TimeoutExpired("launcher", 30)
        other = Mock(pid=5678)
        with patch("smoke_packages.sys.platform", "win32"), patch(
            "smoke_packages.subprocess.run"
        ), self.assertRaisesRegex(RuntimeError, "process cleanup failed"):
            smoke_packages.cleanup_processes([failed, other])
        other.communicate.assert_called_once_with(timeout=30)


if __name__ == "__main__":
    unittest.main()
