"""Package all three CLI executables in the archive expected by both installers."""

from __future__ import annotations

import argparse
import json
import re
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TARGETS = {
    "aarch64-apple-darwin": ("macos-arm64", ""),
    "x86_64-pc-windows-msvc": ("windows-x86_64", ".exe"),
}


def release_version() -> str:
    """Reject mismatched Rust, npm, and PyPI versions before packaging."""
    versions = {
        "package.json": json.loads(
            (ROOT / "cli-typescript/package.json").read_text(encoding="utf-8")
        )["version"],
    }
    for filename, pattern in (
        ("Cargo.toml", r'^version = "([^"]+)"'),
        ("cli-python/pyproject.toml", r'^version = "([^"]+)"'),
        ("cli-python/ast_bro_cli/__init__.py", r'^VERSION = "([^"]+)"'),
        ("cli-typescript/bin/install.js", r'^const VERSION = "([^"]+)"'),
    ):
        match = re.search(pattern, (ROOT / filename).read_text(encoding="utf-8"), re.MULTILINE)
        if match is None:
            raise ValueError(f"Missing version in {filename}")
        versions[filename] = match.group(1)
    if len(set(versions.values())) != 1:
        raise ValueError(f"Package versions disagree: {versions}")
    return versions["Cargo.toml"]


def main() -> None:
    """Create a flat ZIP from a completed native release build."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, choices=TARGETS)
    parser.add_argument("--binary-dir", type=Path)
    parser.add_argument("--output-dir", type=Path, default=ROOT / "target/release")
    parser.add_argument("--tag", help="Require this release tag to match package versions")
    args = parser.parse_args()
    version = release_version()
    if args.tag and args.tag != version:
        parser.error(f"Release tag {args.tag!r} does not match package version {version!r}")

    platform, suffix = TARGETS[args.target]
    binary_dir = args.binary_dir or ROOT / "target" / args.target / "release"
    binaries = [binary_dir / f"{name}{suffix}" for name in ("ast-bro", "ast-outline", "sb")]
    for binary in binaries:
        if not binary.is_file():
            parser.error(f"Missing release executable: {binary}")

    args.output_dir.mkdir(parents=True, exist_ok=True)
    archive = args.output_dir / f"ast-bro-{platform}.zip"
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as output:
        for binary in binaries:
            output.write(binary, arcname=binary.name)
        output.write(ROOT / "LICENSE", arcname="LICENSE")
    print(archive)


if __name__ == "__main__":
    main()
