"""Exercise the release workflow's shell step without contacting GitHub."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path


@unittest.skipUnless(shutil.which("bash") and shutil.which("jq"), "Requires bash and jq")
class ReleaseWorkflowTests(unittest.TestCase):
    """Only drafts may receive new assets, and reruns must preserve existing assets."""

    def test_reuses_only_matching_draft_assets(self) -> None:
        """Reject untested bytes while allowing matching assets and missing-asset uploads."""
        root = Path(__file__).resolve().parents[2]
        workflow = (root / ".github/workflows/release.yml").read_text(encoding="utf-8")
        step = workflow.split("      - name: Upload release archives\n", 1)[1]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1])
        fake_gh = """
gh() {
  printf '%s\\n' "$*" >> "$RELEASE_TEST_LOG"
  if [[ "$1 $2" == 'release view' ]]; then
    if [[ "$RELEASE_TEST_STATE" == missing ]]; then return 1; fi
    printf '%s\\n' "$RELEASE_TEST_STATE"
  elif [[ "$1 $2" == 'release download' ]]; then
    cp "$RELEASE_TEST_ASSETS/$5" "$7/$5"
  fi
}
"""
        names = ["ast-bro-macos-arm64.zip", "ast-bro-windows-x86_64.zip"]
        cases = [
            ("missing", None, b"tested archive", 0),
            ("empty draft", {"isDraft": True, "assets": []}, b"tested archive", 0),
            (
                "partial draft",
                {"isDraft": True, "assets": [{"name": names[0]}]},
                b"tested archive",
                0,
            ),
            (
                "complete draft",
                {"isDraft": True, "assets": [{"name": n} for n in names]},
                b"tested archive",
                0,
            ),
            (
                "mismatched draft",
                {"isDraft": True, "assets": [{"name": names[0]}]},
                b"untested archive",
                1,
            ),
            ("published", {"isDraft": False, "assets": []}, b"tested archive", 1),
        ]
        for label, release, existing_bytes, expected in cases:
            with self.subTest(case=label), tempfile.TemporaryDirectory() as directory:
                temp = Path(directory)
                artifacts = temp / "release-artifacts"
                artifacts.mkdir()
                existing = temp / "existing-assets"
                existing.mkdir()
                for name in names:
                    (artifacts / name).write_bytes(b"tested archive")
                    (existing / name).write_bytes(existing_bytes)
                log = temp / "gh.log"
                env = dict(
                    os.environ,
                    RELEASE_TAG="4.2.1",
                    RELEASE_TEST_LOG=str(log),
                    RELEASE_TEST_STATE="missing" if release is None else json.dumps(release),
                    RELEASE_TEST_ASSETS=str(existing),
                )
                result = subprocess.run(
                    ["bash", "-eo", "pipefail", "-c", fake_gh + script],
                    cwd=temp,
                    env=env,
                    capture_output=True,
                    text=True,
                    timeout=10,
                    check=False,
                )
                self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                calls = log.read_text().splitlines()
                mutations = [
                    call for call in calls if call.startswith(("release create", "release upload"))
                ]
                downloads = [call for call in calls if call.startswith("release download")]
                for call in downloads:
                    downloaded = Path(call.split(" --dir ", 1)[1])
                    self.assertFalse(downloaded.exists(), "Temporary downloads must be removed")
                if release is None:
                    self.assertEqual(len(mutations), 1)
                    self.assertTrue(mutations[0].startswith("release create 4.2.1 "))
                    self.assertIn(" --draft ", mutations[0])
                    for name in names:
                        self.assertIn(f"release-artifacts/{name}", mutations[0])
                else:
                    existing_names = {asset["name"] for asset in release["assets"]}
                    missing = [name for name in names if name not in existing_names]
                    self.assertEqual(
                        mutations,
                        [f"release upload 4.2.1 release-artifacts/{name}" for name in missing]
                        if release["isDraft"] and expected == 0
                        else [],
                    )
                    self.assertEqual(
                        len(downloads), len(existing_names) if release["isDraft"] else 0
                    )
                    if label == "mismatched draft":
                        self.assertIn("differs from the tested archive", result.stderr)
                for name in names:
                    self.assertEqual((existing / name).read_bytes(), existing_bytes)


if __name__ == "__main__":
    unittest.main()
