"""Update Homebrew's checksum from the release asset produced by CI."""

from __future__ import annotations

import hashlib
import re
import subprocess
import tempfile
from pathlib import Path

from package_release import ROOT, release_version

REPOSITORY = "aeroxy/ast-bro"
ARCHIVE = "ast-bro-macos-arm64.zip"


def update_formula() -> str:
    """Hash the uploaded archive, leaving the formula unchanged if downloading fails."""
    version = release_version()
    formula = ROOT / "Formula/ast-bro.rb"
    original = formula.read_bytes().decode("utf-8")
    url = f"https://github.com/{REPOSITORY}/releases/download/{version}/{ARCHIVE}"
    if re.findall(r'^\s*url "([^"]+)"', original, re.MULTILINE) != [url]:
        raise ValueError(f"Formula/ast-bro.rb must reference {url}")
    checksum = r'(^[ \t]*sha256 ")[a-fA-F0-9]{64}("[ \t]*\r?$)'
    if len(re.findall(checksum, original, re.MULTILINE)) != 1:
        raise ValueError("Formula/ast-bro.rb must contain exactly one SHA256 checksum")

    with tempfile.TemporaryDirectory(prefix="ast-bro-formula-") as directory:
        subprocess.run(
            [
                "gh",
                "release",
                "download",
                version,
                "--repo",
                REPOSITORY,
                "--pattern",
                ARCHIVE,
                "--dir",
                directory,
            ],
            check=True,
        )
        digest = hashlib.sha256((Path(directory) / ARCHIVE).read_bytes()).hexdigest()

    updated = re.sub(
        checksum,
        lambda match: f"{match[1]}{digest}{match[2]}",
        original,
        flags=re.MULTILINE,
    )
    formula.write_bytes(updated.encode("utf-8"))
    return digest


def main() -> None:
    """Use the synchronized package version to select the GitHub release tag."""
    print(f"Formula/ast-bro.rb updated from the release asset: {update_formula()}")


if __name__ == "__main__":
    main()
