#!/usr/bin/env bash
# Attach tested archives from release-artifacts/ to the version tag's draft.
set -euo pipefail

: "${RELEASE_TAG:?Set RELEASE_TAG to the version tag}"

if release=$(gh release view "$RELEASE_TAG" --json isDraft,assets); then
  if [[ $(jq -r '.isDraft' <<<"$release") != true ]]; then
    echo "Refusing to upload to published release $RELEASE_TAG" >&2
    exit 1
  fi
  existing_assets=$(mktemp -d)
  trap 'rm -rf "$existing_assets"' EXIT
  for archive in release-artifacts/*.zip; do
    name=${archive##*/}
    if jq -e --arg name "$name" '.assets | any(.name == $name)' <<<"$release" >/dev/null; then
      gh release download "$RELEASE_TAG" --pattern "$name" --dir "$existing_assets"
      if ! cmp -s "$archive" "$existing_assets/$name"; then
        echo "Existing draft asset $name differs from the tested archive; refusing to reuse it" >&2
        exit 1
      fi
      echo "Verified existing release asset $name"
    else
      gh release upload "$RELEASE_TAG" "$archive"
    fi
  done
else
  gh release create "$RELEASE_TAG" release-artifacts/*.zip --draft --verify-tag --title "ast-bro v$RELEASE_TAG" --generate-notes
fi
