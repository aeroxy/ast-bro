.PHONY: build release release-windows check run test clean bump-patch bump-minor bump-major update-formula publish-npm publish-pypi setup-pypi

PYTHON ?= python3

## Build the project (debug)
build:
	cargo build

## Release build — use this before manual testing
release:
	cargo build --release
	$(PYTHON) scripts/package_release.py --target aarch64-apple-darwin --binary-dir target/release

## Build and package on a Windows x64 host (or use the commands in docs/releasing.md)
release-windows:
	cargo build --release --locked --bins --target x86_64-pc-windows-msvc
	$(PYTHON) scripts/package_release.py --target x86_64-pc-windows-msvc

## Type-check without producing a binary
check:
	cargo check

## Run the CLI; pass subcommands via ARGS
##   make run ARGS="list-pages"
##   make run ARGS="screenshot --output out.png"
run:
	cargo run -- $(ARGS)

## Run tests
test:
	cargo test

## Remove build artifacts
clean:
	cargo clean

## Bump the patch version (0.1.3 → 0.1.4) and update all version references
bump-patch:
	$(PYTHON) scripts/bump_version.py patch

## Bump the minor version (0.1.4 → 0.2.0) and update all version references
bump-minor:
	$(PYTHON) scripts/bump_version.py minor

## Bump the major version (0.1.4 → 1.0.0) and update all version references
bump-major:
	$(PYTHON) scripts/bump_version.py major

## Update Formula/ast-bro.rb SHA256 from local release zip (run after release-macos, before upload)
##   make update-formula
update-formula:
	@mac_zip="target/release/ast-bro-macos-arm64.zip"; \
	echo "Computing macOS SHA256 …"; \
	mac_sha=$$(shasum -a 256 "$$mac_zip" | cut -d' ' -f1); \
	echo "macOS SHA256: $$mac_sha"; \
	sed -i '' "s/sha256 \"[a-f0-9]*\"/sha256 \"$$mac_sha\"/" Formula/ast-bro.rb; \
	echo "Formula/ast-bro.rb updated"

## Publish @ast-bro/cli to npm
publish-npm:
	cd cli-typescript && npm publish --access public

## Setup Python venv for cli-python (run once)
setup-pypi:
	python3 -m venv cli-python/venv
	cli-python/venv/bin/pip install build twine httpx

## Build and publish ast-bro to PyPI
publish-pypi:
	cd cli-python && rm -rf dist && venv/bin/python -m build && venv/bin/twine upload dist/*
