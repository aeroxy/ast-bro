"""Exercise the release upload script without contacting GitHub."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


def find_bash() -> str | None:
    """Use Git Bash on Windows instead of a WSL launcher earlier on PATH."""
    if os.name != "nt":
        return shutil.which("bash")
    git = shutil.which("git")
    if not git:
        return None
    result = subprocess.run(
        [git, "--exec-path"], capture_output=True, text=True, timeout=10, check=True
    )
    git_root = Path(result.stdout.strip()).parents[2]
    bash = git_root / "bin/bash.exe"
    return str(bash) if bash.is_file() else None


BASH = find_bash()


@unittest.skipUnless(
    BASH and shutil.which("jq"), "Requires Git Bash (Windows) or bash, and jq"
)
class ReleaseWorkflowTests(unittest.TestCase):
    """Only drafts may receive new assets, and reruns must preserve existing assets."""

    def test_reuses_only_matching_draft_assets(self) -> None:
        """Reject untested bytes while allowing matching assets and missing-asset uploads."""
        root = Path(__file__).resolve().parents[2]
        script = root / "scripts/upload_release.sh"
        fake_gh = """
gh() {
  printf '%s\\n' "$*" >> "$RELEASE_TEST_LOG"
  if [[ "$1 $2" == "$RELEASE_TEST_FAIL_COMMAND" ]]; then return 7; fi
  if [[ "$1 $2" == 'release view' ]]; then
    if [[ "$RELEASE_TEST_STATE" == missing ]]; then return 1; fi
    printf '%s\\n' "$RELEASE_TEST_STATE"
  elif [[ "$1 $2" == 'release download' ]]; then
    cp "$RELEASE_TEST_ASSETS/$5" "$7/$5"
  fi
}
export -f gh
exec "$BASH" "$1"
"""
        names = ["ast-bro-macos-arm64.zip", "ast-bro-windows-x86_64.zip"]
        cases = [
            ("missing", None, b"tested archive", 0, ""),
            ("empty draft", {"isDraft": True, "assets": []}, b"tested archive", 0, ""),
            (
                "partial draft",
                {"isDraft": True, "assets": [{"name": names[0]}]},
                b"tested archive",
                0,
                "",
            ),
            (
                "complete draft",
                {"isDraft": True, "assets": [{"name": n} for n in names]},
                b"tested archive",
                0,
                "",
            ),
            (
                "mismatched draft",
                {"isDraft": True, "assets": [{"name": names[0]}]},
                b"untested archive",
                1,
                "",
            ),
            ("published", {"isDraft": False, "assets": []}, b"tested archive", 1, ""),
            ("failed create", None, b"tested archive", 7, "release create"),
            (
                "failed download",
                {"isDraft": True, "assets": [{"name": names[0]}]},
                b"tested archive",
                7,
                "release download",
            ),
            (
                "failed upload",
                {"isDraft": True, "assets": []},
                b"tested archive",
                7,
                "release upload",
            ),
        ]
        for label, release, existing_bytes, expected, failure in cases:
            with self.subTest(case=label), tempfile.TemporaryDirectory() as directory:
                temp = Path(directory)
                artifacts = temp / "release-artifacts"
                artifacts.mkdir()
                existing = temp / "existing-assets"
                existing.mkdir()
                downloads_dir = temp / "downloads"
                downloads_dir.mkdir()
                for name in names:
                    (artifacts / name).write_bytes(b"tested archive")
                    (existing / name).write_bytes(existing_bytes)
                log = temp / "gh.log"
                env = dict(
                    os.environ,
                    RELEASE_TAG="4.2.1",
                    RELEASE_TEST_LOG=str(log),
                    RELEASE_TEST_STATE="missing"
                    if release is None
                    else json.dumps(release),
                    RELEASE_TEST_ASSETS=str(existing),
                    RELEASE_TEST_FAIL_COMMAND=failure,
                    TMPDIR=downloads_dir.as_posix(),
                )
                result = subprocess.run(
                    [BASH, "-c", fake_gh, "release-upload-test", script.as_posix()],
                    cwd=temp,
                    env=env,
                    capture_output=True,
                    text=True,
                    timeout=10,
                    check=False,
                )
                self.assertEqual(
                    result.returncode, expected, result.stdout + result.stderr
                )
                calls = log.read_text().splitlines()
                mutations = [
                    call
                    for call in calls
                    if call.startswith(("release create", "release upload"))
                ]
                downloads = [
                    call for call in calls if call.startswith("release download")
                ]
                self.assertEqual(
                    list(downloads_dir.iterdir()),
                    [],
                    "Temporary downloads must be removed",
                )
                for name in names:
                    self.assertEqual((existing / name).read_bytes(), existing_bytes)
                if failure:
                    self.assertTrue(calls[-1].startswith(failure))
                    self.assertEqual(
                        len(calls), 2, "Stop at the first failed upload or download"
                    )
                    continue
                if release is None:
                    self.assertEqual(len(mutations), 1)
                    self.assertTrue(mutations[0].startswith("release create 4.2.1 "))
                    self.assertIn(" --draft ", mutations[0])
                    self.assertIn(" --verify-tag ", mutations[0])
                    for name in names:
                        self.assertIn(f"release-artifacts/{name}", mutations[0])
                else:
                    existing_names = {asset["name"] for asset in release["assets"]}
                    missing = [name for name in names if name not in existing_names]
                    self.assertEqual(
                        mutations,
                        [
                            f"release upload 4.2.1 release-artifacts/{name}"
                            for name in missing
                        ]
                        if release["isDraft"] and expected == 0
                        else [],
                    )
                    self.assertEqual(
                        len(downloads), len(existing_names) if release["isDraft"] else 0
                    )
                    if label == "mismatched draft":
                        self.assertIn("differs from the tested archive", result.stderr)


if __name__ == "__main__":
    unittest.main()
