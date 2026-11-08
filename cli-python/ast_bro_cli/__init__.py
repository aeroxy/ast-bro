"""ast-bro CLI installer — downloads the Rust binary on first run."""

from __future__ import annotations

import os
import platform
import shutil
import stat
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

import httpx

VERSION = "4.2.0"
GITHUB_REPO = "aeroxy/ast-bro"
BINARY_NAME = "ast-bro"


def get_cache_dir() -> Path:
    """Keep each release separate so upgrades cannot reuse an older binary."""
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Caches" / "ast-bro" / VERSION
    if sys.platform == "win32":
        local = Path(os.environ.get("LOCALAPPDATA") or Path.home() / "AppData" / "Local")
        return local / "ast-bro" / "Cache" / VERSION
    return Path.home() / ".cache" / "ast-bro" / VERSION


def get_platform() -> tuple[str, str]:
    """Return (os, arch) for download URL."""
    system = platform.system().lower()
    machine = platform.machine().lower()

    os_map = {"darwin": "macos", "linux": "linux", "windows": "windows"}
    arch_map = {"arm64": "arm64", "aarch64": "arm64", "x86_64": "x86_64", "amd64": "x86_64"}

    os_name = os_map.get(system)
    arch = arch_map.get(machine)

    if not os_name or not arch:
        raise RuntimeError(
            f"No pre-built binary for {system}/{machine}. Build from source: cargo install ast-bro"
        )

    available = {("macos", "arm64"), ("windows", "x86_64")}
    if (os_name, arch) not in available:
        raise RuntimeError(
            f"No pre-built binary for {os_name}-{arch} yet "
            f"(available: macos-arm64, windows-x86_64). "
            f"Build from source: cargo install ast-bro"
        )

    return os_name, arch


def download_binary() -> Path:
    """Download the ast-bro binary to cache dir."""
    cache_dir = get_cache_dir()
    cache_dir.mkdir(parents=True, exist_ok=True)

    os_name, arch = get_platform()
    binary_ext = ".exe" if os_name == "windows" else ""
    binary_path = cache_dir / f"{BINARY_NAME}{binary_ext}"

    # Check if already downloaded
    if binary_path.exists():
        return binary_path

    url = f"https://github.com/{GITHUB_REPO}/releases/download/{VERSION}/{BINARY_NAME}-{os_name}-{arch}.zip"
    print(f"Downloading ast-bro {VERSION} for {os_name}-{arch}...", file=sys.stderr)

    # Extract only the expected executables and publish after all are complete.
    with tempfile.TemporaryDirectory(dir=cache_dir) as staging:
        archive_path = Path(staging) / "archive.zip"
        with httpx.Client(follow_redirects=True, timeout=60.0) as client:
            resp = client.get(url)
            resp.raise_for_status()
            archive_path.write_bytes(resp.content)

        names = [f"{name}{binary_ext}" for name in ("ast-bro", "ast-outline", "sb")]
        with zipfile.ZipFile(archive_path) as zf:
            for name in names:
                if name not in zf.namelist():
                    raise RuntimeError(f"Release archive is missing {name}")
                destination = Path(staging) / name
                with zf.open(name) as source, destination.open("wb") as output:
                    shutil.copyfileobj(source, output)
                if os_name != "windows":
                    destination.chmod(
                        destination.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH
                    )

        for name in names:
            (Path(staging) / name).replace(cache_dir / name)

    print(f"Installed ast-bro to {binary_path}", file=sys.stderr)
    return binary_path


def get_binary_path() -> Path:
    """Get path to the ast-bro binary, downloading if needed."""
    cache_dir = get_cache_dir()
    os_name, _ = get_platform()
    ext = ".exe" if os_name == "windows" else ""
    binary_path = cache_dir / f"{BINARY_NAME}{ext}"

    if not binary_path.exists():
        return download_binary()
    return binary_path


def main() -> None:
    """CLI entry point — forwards to the Rust binary."""
    try:
        binary = get_binary_path()
    except (OSError, RuntimeError, httpx.HTTPError, zipfile.BadZipFile) as error:
        print(f"Failed to install ast-bro binary: {error}", file=sys.stderr)
        sys.exit(1)
    args = sys.argv[1:]
    result = subprocess.run([str(binary)] + args, cwd=os.getcwd(), check=False)
    sys.exit(result.returncode)
