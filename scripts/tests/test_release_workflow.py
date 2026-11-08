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

    def test_uploads_only_missing_draft_assets(self) -> None:
        """Cover new, empty, partial, complete, and already-published releases."""
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
  fi
}
"""
        names = ["ast-bro-macos-arm64.zip", "ast-bro-windows-x86_64.zip"]
        cases = [
            ("missing", None, 0),
            ("empty draft", {"isDraft": True, "assets": []}, 0),
            ("partial draft", {"isDraft": True, "assets": [{"name": names[0]}]}, 0),
            ("complete draft", {"isDraft": True, "assets": [{"name": n} for n in names]}, 0),
            ("published", {"isDraft": False, "assets": []}, 1),
        ]
        for label, release, expected in cases:
            with self.subTest(case=label), tempfile.TemporaryDirectory() as directory:
                temp = Path(directory)
                artifacts = temp / "release-artifacts"
                artifacts.mkdir()
                for name in names:
                    (artifacts / name).write_bytes(b"tested archive")
                log = temp / "gh.log"
                env = dict(
                    os.environ,
                    RELEASE_TAG="4.2.1",
                    RELEASE_TEST_LOG=str(log),
                    RELEASE_TEST_STATE="missing" if release is None else json.dumps(release),
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
                mutations = log.read_text().splitlines()[1:]
                if release is None:
                    self.assertEqual(len(mutations), 1)
                    self.assertTrue(mutations[0].startswith("release create 4.2.1 "))
                    self.assertIn(" --draft ", mutations[0])
                    for name in names:
                        self.assertIn(f"release-artifacts/{name}", mutations[0])
                else:
                    existing = {asset["name"] for asset in release["assets"]}
                    missing = [name for name in names if name not in existing]
                    self.assertEqual(
                        mutations,
                        [f"release upload 4.2.1 release-artifacts/{name}" for name in missing]
                        if release["isDraft"]
                        else [],
                    )


if __name__ == "__main__":
    unittest.main()
