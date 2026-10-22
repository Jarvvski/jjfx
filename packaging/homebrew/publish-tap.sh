#!/usr/bin/env bash
# Push dist/jjfx.rb to github.com/Jarvvski/homebrew-jjfx (Formula/jjfx.rb).
#
# No-op when HOMEBREW_TAP_TOKEN is unset, so a release still ships archives
# and the formula asset without a tap. The token needs contents:write on
# that repository. This script does not create the tap repository.
set -euo pipefail

if [[ -z "${HOMEBREW_TAP_TOKEN:-}" ]]; then
  echo "HOMEBREW_TAP_TOKEN is unset; leaving the tap unchanged"
  exit 0
fi

formula=${1:-dist/jjfx.rb}
if [[ ! -f "$formula" ]]; then
  echo "error: formula not found: $formula" >&2
  exit 1
fi

version=$(sed -n 's/^  version "\(.*\)"/\1/p' "$formula" | head -1)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The token stays in an http header so it is not written into the remote URL.
git -c http.extraheader="AUTHORIZATION: bearer ${HOMEBREW_TAP_TOKEN}" \
  clone --depth 1 https://github.com/Jarvvski/homebrew-jjfx.git "$work"

mkdir -p "$work/Formula"
cp "$formula" "$work/Formula/jjfx.rb"
git -C "$work" add Formula/jjfx.rb
if git -C "$work" diff --cached --quiet; then
  echo "tap formula already matches $formula"
  exit 0
fi

git -C "$work" \
  -c user.name="github-actions[bot]" \
  -c user.email="41898282+github-actions[bot]@users.noreply.github.com" \
  commit -m "jjfx ${version:-release}"
git -C "$work" -c http.extraheader="AUTHORIZATION: bearer ${HOMEBREW_TAP_TOKEN}" \
  push origin HEAD
