#!/usr/bin/env bash
# Attach the archives, checksums, and Homebrew formula in dist/ to the GitHub
# Release for the current tag. Creates the Release when the tag does not have
# one yet. Used by the tag-push workflow; the dispatch Release workflow creates
# its Release inline and does not call this script.
set -euo pipefail

tag=${GITHUB_REF_NAME:?tag-push workflow must set GITHUB_REF_NAME}
: "${GH_TOKEN:?GH_TOKEN is required}"

shopt -s nullglob
files=("$PWD"/dist/*.tar.gz "$PWD"/dist/*.tar.gz.sha256 "$PWD"/dist/jjfx.rb)
if [[ ${#files[@]} -lt 3 ]]; then
  echo "error: dist/ is missing archives or jjfx.rb" >&2
  exit 1
fi

if gh release view "$tag" >/dev/null 2>&1; then
  gh release upload "$tag" "${files[@]}" --clobber
  exit 0
fi

notes=$(mktemp)
ver=${tag#v}
if [[ -f CHANGELOG.md ]]; then
  awk -v ver="$ver" '
    $0 ~ "^## \\[" ver "\\]" { found = 1; next }
    found && /^## \[/ { exit }
    found { print }
  ' CHANGELOG.md >"$notes"
fi

if [[ -s "$notes" ]]; then
  gh release create "$tag" "${files[@]}" --title "$tag" --notes-file "$notes"
else
  gh release create "$tag" "${files[@]}" --title "$tag" --generate-notes
fi
