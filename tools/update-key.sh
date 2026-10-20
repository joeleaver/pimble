#!/usr/bin/env bash
# Makes the key pair release packages are signed with (docs/UPDATE_CONTRACT.md):
# the private half goes into the repository's PIMBLE_UPDATE_SIGNING_KEY secret,
# where only the Release workflow reads it, and the public half into
# crates/pimble-app/update-key.pub, which every build compiles in. Run it once
# and commit update-key.pub. Running it again replaces the key: copies built
# with the old one then refuse every update until they are reinstalled.
#
# Needs openssl 3 and an authenticated gh.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

key="$(mktemp)"
trap 'rm -f "$key"' EXIT
openssl genpkey -algorithm ed25519 -out "$key"
gh secret set PIMBLE_UPDATE_SIGNING_KEY < "$key"
# The DER public key ends with the raw 32 bytes.
openssl pkey -in "$key" -pubout -outform DER | tail -c 32 | base64 > crates/pimble-app/update-key.pub
echo "Stored the private key as the PIMBLE_UPDATE_SIGNING_KEY secret (it is not kept here)."
echo "Wrote crates/pimble-app/update-key.pub: $(cat crates/pimble-app/update-key.pub). Commit it."
