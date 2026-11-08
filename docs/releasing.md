# Release packaging

The `Release packages` workflow builds and tests these archives on native runners:

| Rust target | Archive |
| --- | --- |
| `aarch64-apple-darwin` | `ast-bro-macos-arm64.zip` |
| `x86_64-pc-windows-msvc` | `ast-bro-windows-x86_64.zip` |

Each ZIP contains `ast-bro`, `ast-outline`, `sb` (with `.exe` on Windows), and `LICENSE` at its root. Windows builds statically link the MSVC runtime and reserve an 8 MiB stack, so users do not need a separate Visual C++ runtime installation. Windows ARM64 and Linux do not yet have pre-built archives.

## Validate a Windows package locally

Building requires stable Rust with the MSVC C++ build tools and Windows SDK. Packaging and smoke tests use Python 3.8+ and Node.js 18+. Run from the repository root:

```powershell
cargo test --locked
cargo build --locked --release --bins --target x86_64-pc-windows-msvc
uv run --with build --with httpx python scripts/package_release.py --target x86_64-pc-windows-msvc
node --test cli-typescript/test/install.test.js
uv run --with httpx python -m unittest discover -s cli-python/tests
uv run python -m unittest discover -s scripts/tests
uv run --with build --with httpx python scripts/smoke_packages.py --archive target/release/ast-bro-windows-x86_64.zip
```

For macOS ARM64, use `aarch64-apple-darwin` and `ast-bro-macos-arm64.zip`. The existing `make release` remains available on macOS; Windows users with Make can use `make release-windows PYTHON=python`.

The smoke test packs an npm tarball and a Python wheel, installs each into a disposable directory, and runs all three installed commands. It checks a cold download, HTTP redirects, cached execution, paths containing spaces and shell metacharacters, JSON output, exit-code forwarding, and npm's skipped-postinstall fallback. Test-only transport hooks redirect the normal GitHub release URL to the locally built archive; production installer files are not patched. Nothing is installed globally into the developer's own environment.

The release regression tests exercise all three version bumps with real offline, locked Cargo builds and check rollback after a failed lockfile refresh.

## Publish a release

1. Bump all package versions together with `make bump-patch` (or minor/major), or `uv run python scripts/bump_version.py patch` on any platform. The shared helper refreshes `Cargo.lock` with `cargo update --workspace`, preserving locked dependency versions, and restores the original files if the refresh fails. Commit the updated manifests, wrappers, formula, and `Cargo.lock` together. Packaging rejects a stale lockfile or inconsistent Rust, npm, and Python versions.
2. Push a matching version tag such as `4.2.1` (without a `v` prefix). The workflow builds both platforms, runs the package tests, and attaches both ZIPs to a draft GitHub release. Pull requests and manual workflow runs build/test and retain artifacts without publishing a release.
3. Review the draft and update the Homebrew formula with the macOS ZIP's SHA256. Publish the GitHub release so the versioned download URLs are accessible.
4. Publish the matching npm and PyPI packages with the existing publishing commands. Publish the binary assets first: npm downloads during installation, and Python downloads on first use.
5. On a clean Windows x64 environment, verify `npm install -g @ast-bro/cli` or `uv tool install ast-bro`, followed by `ast-bro --version`.

The existing published `4.2.0` packages still reject Windows. Windows support becomes available through the package registries only after a new version containing these installer changes and both release archives is published.
