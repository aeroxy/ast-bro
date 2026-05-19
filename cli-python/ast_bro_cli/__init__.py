"""ast-bro CLI installer — downloads the Rust binary on first run."""

import os
import sys
import stat
import platform
import subprocess
from pathlib import Path

import httpx

VERSION = "2.1.0"
GITHUB_REPO = "aeroxy/ast-bro"
BINARY_NAME = "ast-bro"


def get_cache_dir() -> Path:
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Caches" / "ast-bro"
    elif sys.platform == "linux":
        return Path.home() / ".cache" / "ast-bro"
    else:
        return Path.home() / ".cache" / "ast-bro"


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
            f"No pre-built binary for {system}/{machine}. "
            f"Build from source: cargo install ast-bro"
        )

    # Check if pre-built binary exists (only macos-arm64 for now)
    available = {("macos", "arm64")}
    if (os_name, arch) not in available:
        raise RuntimeError(
            f"No pre-built binary for {os_name}-{arch} yet (available: macos-arm64). "
            f"Build from source: cargo install ast-bro"
        )

    return os_name, arch


def download_binary() -> Path:
    """Download the ast-bro binary to cache dir."""
    cache_dir = get_cache_dir()
    cache_dir.mkdir(parents=True, exist_ok=True)

    os_name, arch = get_platform()
    ext = ".zip" if os_name == "windows" else ".tar.gz"
    binary_ext = ".exe" if os_name == "windows" else ""
    binary_path = cache_dir / f"{BINARY_NAME}{binary_ext}"

    # Check if already downloaded
    if binary_path.exists():
        return binary_path

    url = f"https://github.com/{GITHUB_REPO}/releases/download/{VERSION}/{BINARY_NAME}-{os_name}-{arch}{ext}"
    print(f"Downloading ast-bro {VERSION} for {os_name}-{arch}...")
    print(f"  {url}")

    archive_path = cache_dir / f"archive{ext}"

    with httpx.Client(follow_redirects=True) as client:
        resp = client.get(url)
        resp.raise_for_status()
        archive_path.write_bytes(resp.content)

    # Extract
    import tarfile
    import zipfile

    if ext == ".tar.gz":
        with tarfile.open(archive_path, "r:gz") as tar:
            tar.extractall(path=cache_dir)
    else:
        with zipfile.ZipFile(archive_path) as zf:
            zf.extractall(path=cache_dir)

    archive_path.unlink()

    # Make executable
    if sys.platform != "windows":
        binary_path.chmod(binary_path.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)

    print(f"Installed ast-bro to {binary_path}")
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


def main():
    """CLI entry point — forwards to the Rust binary."""
    binary = get_binary_path()
    args = sys.argv[1:]
    result = subprocess.run([str(binary)] + args, cwd=os.getcwd())
    sys.exit(result.returncode)
