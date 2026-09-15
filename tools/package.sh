#!/usr/bin/env bash
# Packages the built pimble-app desktop binary into the tar.gz that
# .github/workflows/release.yml attaches to a GitHub release, so the same
# packaging step can be run and inspected locally.
#
# Usage:
#   tools/package.sh [version] [binary-path] [out-dir]
#
#   version      Defaults to `git describe --tags --always`, falling back to
#                the workspace's [workspace.package] version in Cargo.toml.
#   binary-path  Defaults to target/release/pimble (the pimble-app binary;
#                see crates/pimble-app/Cargo.toml's [[bin]] name = "pimble").
#                Not built by this script -- run
#                `cargo build -p pimble-app --release` first.
#   out-dir      Defaults to dist/.
#
# Produces <out-dir>/pimble-<version>-linux-x86_64.tar.gz containing the
# `pimble` binary plus LICENSE and README.md, when those are present at the
# repo root (as of 2026-09-15 neither is committed yet, so the archive may
# ship with only the binary -- add them at the root and re-run to include
# them; nothing here requires it).
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

version="${1:-}"
if [ -z "$version" ]; then
  version="$(git describe --tags --always 2>/dev/null || true)"
fi
if [ -z "$version" ]; then
  version="$(grep -A5 '^\[workspace.package\]' Cargo.toml | grep '^version' | head -1 | sed -E 's/version *= *"([^"]+)".*/\1/')"
fi
version="${version#v}"
if [ -z "$version" ]; then
  echo "package.sh: could not determine a version (pass one explicitly)" >&2
  exit 1
fi

binary_path="${2:-target/release/pimble}"
out_dir="${3:-dist}"

if [ ! -f "$binary_path" ]; then
  echo "package.sh: no binary at $binary_path -- run 'cargo build -p pimble-app --release' first" >&2
  exit 1
fi

archive_name="pimble-${version}-linux-x86_64.tar.gz"
stage_dir="$(mktemp -d)"
trap 'rm -rf "$stage_dir"' EXIT

cp "$binary_path" "$stage_dir/pimble"
chmod 755 "$stage_dir/pimble"
[ -f LICENSE ] && cp LICENSE "$stage_dir/"
[ -f README.md ] && cp README.md "$stage_dir/"

mkdir -p "$out_dir"
tar -C "$stage_dir" -czf "$out_dir/$archive_name" .

echo "Wrote $out_dir/$archive_name"
tar -tzvf "$out_dir/$archive_name"
