#!/usr/bin/env bash
# Installs a published release on this machine the way a person would get it
# (docs/UPDATE_CONTRACT.md): the release's own Linux package, its signature
# checked against the key builds carry, then `pimble --install` from the
# unpacked package. The home-folder install keeps itself current from then on.
#
# Usage: tools/install-release.sh [vX.Y.Z]   (default: the latest release)
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tag="${1:-$(gh release view --repo joeleaver/pimble --json tagName -q .tagName)}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

gh release download "$tag" --repo joeleaver/pimble --dir "$work" \
  --pattern '*-linux-x86_64.tar.gz' --pattern '*-linux-x86_64.tar.gz.sig'
package="$(ls "$work"/*-linux-x86_64.tar.gz)"

# The key as the app has it (raw 32 bytes, base64), wrapped back into the DER
# form openssl reads: the fixed Ed25519 SubjectPublicKeyInfo prefix, then the key.
pub="$work/key.der"
{ printf '\x30\x2a\x30\x05\x06\x03\x2b\x65\x70\x03\x21\x00'; base64 -d "$repo_root/crates/pimble-app/update-key.pub"; } > "$pub"
openssl pkeyutl -verify -rawin -pubin -keyform DER -inkey "$pub" -in "$package" -sigfile "$package.sig"

mkdir "$work/unpacked"
tar -xzf "$package" -C "$work/unpacked"
"$work/unpacked/pimble" --install
