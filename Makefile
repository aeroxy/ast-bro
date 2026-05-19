.PHONY: build release check run test clean bump-patch bump-minor bump-major update-formula publish-npm publish-pypi setup-pypi

## Build the project (debug)
build:
	cargo build

## Release build — use this before manual testing
release:
	cargo build --release
	zip -j target/release/ast-bro-macos-arm64.zip target/release/ast-bro

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
	@old=$$(grep '^version' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/'); \
	major=$$(echo $$old | cut -d. -f1); \
	minor=$$(echo $$old | cut -d. -f2); \
	patch=$$(echo $$old | cut -d. -f3); \
	new="$$major.$$minor.$$((patch+1))"; \
	sed -i '' "s/^version = \"$$old\"/version = \"$$new\"/" Cargo.toml; \
	sed -i '' "s/version \"$$old\"/version \"$$new\"/" Formula/ast-bro.rb; \
	sed -i '' "s|/$$old/|/$$new/|g" Formula/ast-bro.rb; \
	sed -i '' "s/^version = \"$$old\"/version = \"$$new\"/" cli-python/pyproject.toml; \
	sed -i '' "s/VERSION = \"$$old\"/VERSION = \"$$new\"/" cli-python/ast_outline_cli/__init__.py; \
	sed -i '' "s/\"version\": \"$$old\"/\"version\": \"$$new\"/" cli-typescript/package.json; \
	sed -i '' "s/const VERSION = \"$$old\"/const VERSION = \"$$new\"/" cli-typescript/bin/install.js; \
	echo "$$old → $$new"

## Bump the minor version (0.1.4 → 0.2.0) and update all version references
bump-minor:
	@old=$$(grep '^version' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/'); \
	major=$$(echo $$old | cut -d. -f1); \
	minor=$$(echo $$old | cut -d. -f2); \
	new="$$major.$$((minor+1)).0"; \
	sed -i '' "s/^version = \"$$old\"/version = \"$$new\"/" Cargo.toml; \
	sed -i '' "s/version \"$$old\"/version \"$$new\"/" Formula/ast-bro.rb; \
	sed -i '' "s|/$$old/|/$$new/|g" Formula/ast-bro.rb; \
	sed -i '' "s/^version = \"$$old\"/version = \"$$new\"/" cli-python/pyproject.toml; \
	sed -i '' "s/VERSION = \"$$old\"/VERSION = \"$$new\"/" cli-python/ast_outline_cli/__init__.py; \
	sed -i '' "s/\"version\": \"$$old\"/\"version\": \"$$new\"/" cli-typescript/package.json; \
	sed -i '' "s/const VERSION = \"$$old\"/const VERSION = \"$$new\"/" cli-typescript/bin/install.js; \
	echo "$$old → $$new"

## Bump the major version (0.1.4 → 1.0.0) and update all version references
bump-major:
	@old=$$(grep '^version' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/'); \
	major=$$(echo $$old | cut -d. -f1); \
	new="$$((major+1)).0.0"; \
	sed -i '' "s/^version = \"$$old\"/version = \"$$new\"/" Cargo.toml; \
	sed -i '' "s/version \"$$old\"/version \"$$new\"/" Formula/ast-bro.rb; \
	sed -i '' "s|/$$old/|/$$new/|g" Formula/ast-bro.rb; \
	sed -i '' "s/^version = \"$$old\"/version = \"$$new\"/" cli-python/pyproject.toml; \
	sed -i '' "s/VERSION = \"$$old\"/VERSION = \"$$new\"/" cli-python/ast_outline_cli/__init__.py; \
	sed -i '' "s/\"version\": \"$$old\"/\"version\": \"$$new\"/" cli-typescript/package.json; \
	sed -i '' "s/const VERSION = \"$$old\"/const VERSION = \"$$new\"/" cli-typescript/bin/install.js; \
	echo "$$old → $$new"

## Update Formula/ast-bro.rb SHA256 from local release zip (run after release-macos, before upload)
##   make update-formula
update-formula:
	@mac_zip="target/release/ast-bro-macos-arm64.zip"; \
	echo "Computing macOS SHA256 …"; \
	mac_sha=$$(shasum -a 256 "$$mac_zip" | cut -d' ' -f1); \
	echo "macOS SHA256: $$mac_sha"; \
	sed -i '' "s/sha256 \"[a-f0-9]*\"/sha256 \"$$mac_sha\"/" Formula/ast-bro.rb; \
	echo "Formula/ast-bro.rb updated"

## Publish ast-bro to npm
publish-npm:
	cd cli-typescript && npm publish

## Setup Python venv for cli-python (run once)
setup-pypi:
	python3 -m venv cli-python/venv
	cli-python/venv/bin/pip install build twine httpx

## Build and publish ast-bro to PyPI
publish-pypi:
	cd cli-python && venv/bin/python -m build && venv/bin/twine upload dist/*
