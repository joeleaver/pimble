#!/usr/bin/env bash
# The no-second-path check (docs/ONE_CODEBASE_CONTRACT.md section 8).
#
# Since wave 5 the browser runs the same Pimble server as the desktop, in a
# worker, and `web/src` holds only adapters and the page's entry. A change to
# how stores, sync or keys work is a change to the server. This fails when
# `web/src` grows a second path again: its own documents or tree, a vault
# client, a `Backend` of its own, or the old path's switch.
#
# Run from anywhere: `scripts/check-one-path.sh`. CI runs it in the check job.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
src="$root/web/src"
web="$root/web"

if [[ ! -d "$src" ]]; then
    echo "check-one-path: $src is missing" >&2
    exit 2
fi

failed=0

# ban <description> <extended regex> <path>...
ban() {
    local what="$1" pattern="$2"
    shift 2
    local hits
    hits="$(grep -rnE --exclude-dir=target --exclude-dir=dist --include='*.rs' --include='*.js' --include='*.html' --include='*.md' --include='*.toml' -e "$pattern" "$@" || true)"
    if [[ -n "$hits" ]]; then
        echo "check-one-path: $what" >&2
        echo "$hits" | sed "s|^$root/|  |" >&2
        failed=1
    fi
}

# Documents and the tree are the server's.
ban "web/src names pimble_crdt (node documents belong to the server)" '\bpimble_crdt\b' "$src"
ban "web/src names pimble_store (stores belong to the server)" '\bpimble_store\b' "$src"
ban "web/src names Tree (the tree of documents belongs to the server)" '\bTree\b' "$src"

# A vault client: the encrypted-store RPCs are the server's vault link's, in
# Rust (PimbleClient's methods) and on the wire (their JSON-RPC names).
ban "web/src calls a vault RPC (the vault link is the server's)" \
    '\bvault_(append[a-z_]*|fetch[a-z_]*|list_docs[a-z_]*|snapshot[a-z_]*|reports?|set_doc_keys)\b' "$src"
ban "web/src names a vault RPC on the wire (the vault link is the server's)" \
    'vault(Append[A-Za-z]*|Fetch[A-Za-z]*|ListDocs[A-Za-z]*|Snapshot[A-Za-z]*|Reports?|SetDocKeys)\b' "$src"

# A backend of the page's own beside the API's one over the server.
ban "web/src implements a pimble_api Backend (the page's one backend is its server)" \
    '\bimpl\b[^{;]*\bBackend[[:space:]]+for\b' "$src"

# The old path's modules and switch.
ban "web/src declares an old-path module" '\bmod[[:space:]]+(vault|vault_backend|routed|endpoints)\b' "$src"
ban "web/ mentions the old path's switch" 'server=old' "$web"

if [[ "$failed" -ne 0 ]]; then
    echo "check-one-path: failed; see docs/ONE_CODEBASE_CONTRACT.md section 8" >&2
    exit 1
fi
echo "check-one-path: web/src holds one path"
