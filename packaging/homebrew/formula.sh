#!/usr/bin/env bash
# Write a Homebrew formula for the jjfx release archives in a dist directory.
#
# Usage:
#   packaging/homebrew/formula.sh [dist-dir]
#
# Reads jjfx-<version>-<target>.tar.gz and the matching .sha256 sidecars that
# `mise run release` writes. Download URLs use $GITHUB_REF_NAME when it is a
# version tag (the tag-push workflow). The dispatch Release workflow runs on
# the default branch, so a non-tag ref falls back to v<version> from the
# archive name, which is the tag that workflow is about to push. Prints the
# formula path on stdout.
set -euo pipefail

dist=${1:-dist}
repo=Jarvvski/jjfx

if [[ ! -d "$dist" ]]; then
  echo "error: dist directory not found: $dist" >&2
  exit 1
fi

sha_of() {
  local sidecar="$1.sha256"
  if [[ ! -f "$sidecar" ]]; then
    echo "error: missing checksum sidecar: $sidecar" >&2
    exit 1
  fi
  awk 'NR == 1 { print $1 }' "$sidecar"
}

# Archive names are jjfx-<semver>-<target>.tar.gz. The target itself contains
# hyphens, so the version is the leading dotted number.
version=""
for archive in "$dist"/jjfx-*.tar.gz; do
  [[ -e "$archive" ]] || continue
  base=$(basename "$archive" .tar.gz)
  if [[ "$base" =~ ^jjfx-([0-9]+\.[0-9]+\.[0-9]+)-(.+)$ ]]; then
    version="${BASH_REMATCH[1]}"
    break
  fi
done
if [[ -z "$version" ]]; then
  echo "error: no jjfx-<version>-<target>.tar.gz archives in $dist" >&2
  exit 1
fi

require() {
  local target=$1
  local archive="$dist/jjfx-$version-$target.tar.gz"
  if [[ ! -f "$archive" ]]; then
    echo "error: missing archive for $target ($archive)" >&2
    exit 1
  fi
  sha_of "$archive"
}

arm_sha=$(require aarch64-apple-darwin)
intel_sha=$(require x86_64-apple-darwin)
linux_sha=$(require x86_64-unknown-linux-musl)

if [[ ${GITHUB_REF_NAME:-} == v[0-9]* ]]; then
  tag=$GITHUB_REF_NAME
else
  tag=v$version
fi
asset() {
  printf 'https://github.com/%s/releases/download/%s/jjfx-%s-%s.tar.gz' \
    "$repo" "$tag" "$version" "$1"
}

out="$dist/jjfx.rb"
cat >"$out" <<EOF
class Jjfx < Formula
  desc "A terminal UI for Jujutsu workspaces and the coding agents editing them"
  homepage "https://github.com/${repo}"
  version "${version}"
  license "GPL-3.0-or-later"

  on_macos do
    on_arm do
      url "$(asset aarch64-apple-darwin)"
      sha256 "${arm_sha}"
    end
    on_intel do
      url "$(asset x86_64-apple-darwin)"
      sha256 "${intel_sha}"
    end
  end

  on_linux do
    on_intel do
      url "$(asset x86_64-unknown-linux-musl)"
      sha256 "${linux_sha}"
    end
  end

  def install
    bin.install "bin/jjfx"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/jjfx --version")
  end
end
EOF

echo "$out"
