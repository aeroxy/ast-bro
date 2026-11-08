"""Bump every release version and refresh Cargo.lock without upgrading dependencies."""

from __future__ import annotations

import argparse
import re
import subprocess

from package_release import ROOT, release_version


def bump_version(part: str) -> str:
    """Update release metadata together, rolling back if Cargo cannot refresh its lockfile."""
    old = release_version()
    numbers = [int(number) for number in old.split(".")]
    index = {"major": 0, "minor": 1, "patch": 2}[part]
    numbers[index] += 1
    numbers[index + 1 :] = [0] * (2 - index)
    new = ".".join(str(number) for number in numbers)

    originals = {}
    updates = {}
    for filename, prefix in (
        ("Cargo.toml", "version = "),
        ("cli-python/pyproject.toml", "version = "),
        ("cli-python/ast_bro_cli/__init__.py", "VERSION = "),
        ("cli-typescript/package.json", '  "version": '),
        ("cli-typescript/bin/install.js", "const VERSION = "),
    ):
        path = ROOT / filename
        originals[path] = path.read_bytes()
        updates[path], count = re.subn(
            rf'^{re.escape(prefix)}"{re.escape(old)}"',
            f'{prefix}"{new}"',
            originals[path].decode("utf-8"),
            count=1,
            flags=re.MULTILINE,
        )
        if count != 1:
            raise ValueError(f"Missing version {old} in {filename}")

    formula = ROOT / "Formula/ast-bro.rb"
    originals[formula] = formula.read_bytes()
    updates[formula] = (
        originals[formula].decode("utf-8").replace(f"/download/{old}/", f"/download/{new}/")
    )
    updates[formula] = updates[formula].replace(f'version "{old}"', f'version "{new}"')
    lockfile = ROOT / "Cargo.lock"
    originals[lockfile] = lockfile.read_bytes()
    try:
        for path, content in updates.items():
            path.write_bytes(content.encode("utf-8"))
        subprocess.run(["cargo", "update", "--workspace"], cwd=ROOT, check=True)
        release_version()
    except (OSError, subprocess.CalledProcessError, ValueError):
        for path, content in originals.items():
            path.write_bytes(content)
        raise
    return new


def main() -> None:
    """Run the same bump command on macOS, Linux, or Windows."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("part", choices=("patch", "minor", "major"))
    args = parser.parse_args()
    print(f"Bumped release to {bump_version(args.part)} (including Cargo.lock)")


if __name__ == "__main__":
    main()
