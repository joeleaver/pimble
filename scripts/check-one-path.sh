#!/usr/bin/env bash
# The no-second-path check (docs/ONE_CODEBASE_CONTRACT.md section 8).
#
# Since wave 5 the browser runs the same Pimble server as the desktop, in a
# worker, and `web/src` holds only adapters and the page's entry. A change to
# how stores, sync or keys work is a change to the server. This fails when
# `web/src` grows a second path again: its own documents or tree, a vault
# client, a `Backend` of its own, an RPC path to any server but its own (a
# `Router`, an RPC backend or a client over anything but the worker's message
# port, `pimble_server` linked into the page), or the old path's switch.
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
    hits="$(grep -rnHE --exclude-dir=target --exclude-dir=dist --include='*.rs' --include='*.js' --include='*.html' --include='*.md' --include='*.toml' -e "$pattern" "$@" || true)"
    if [[ -n "$hits" ]]; then
        echo "check-one-path: $what" >&2
        echo "$hits" | sed "s|^$root/|  |" >&2
        failed=1
    fi
}

# ban_joined <description> <perl regex> <path>...
# The same as `ban`, over each .rs file whole, so a declaration wrapped across
# lines is caught: `\s` and negated classes match newlines. Reported at the
# line the match starts on, with its whitespace collapsed.
ban_joined() {
    local what="$1" pattern="$2"
    shift 2
    local hits
    hits="$(find "$@" -type f -name '*.rs' -not -path '*/target/*' -not -path '*/dist/*' -print0 |
        PATTERN="$pattern" xargs -0 -r perl -0777 -ne '
            while (/$ENV{PATTERN}/g) {
                my $line = (substr($_, 0, $-[0]) =~ tr/\n//) + 1;
                (my $match = $&) =~ s/\s+/ /g;
                print "$ARGV:$line: $match\n";
            }' || true)"
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
ban_joined "web/src implements a pimble_api Backend (the page's one backend is its server)" \
    '\bimpl\b[^{;]*\bBackend\s+for\b' "$src"

# An RPC path to any server but the page's own. The one API is a `Pimble`
# over pimble-api's RPC backend whose connector is `LocalConnector`, a
# client over the worker's message port (`web/src/worker_mode/backend.rs`).
ban_joined "web/src builds a pimble_api Router (stores routed to another server)" \
    '\bpimble_api\s*::\s*(?:\{[^}]*)?\bRouter\b|\bRouter\s*::' "$src"
ban_joined "web/src makes an RPC backend over anything but LocalConnector (the page's one server is its worker)" \
    '\bRpcBackend\s*::\s*(?!new\s*\(\s*LocalConnector\b)\w+' "$src"
ban_joined "web/src implements a pimble_api Connector other than LocalConnector (the page's one server is its worker)" \
    '\bimpl\b[^{;]*\bConnector\s+for\s+(?!LocalConnector\b)\w+' "$src"
ban_joined "web/src opens a PimbleClient other than over_message_port (the page's one server is its worker)" \
    '\bPimbleClient\s*::\s*(?!over_message_port\b)\w+|\bPimbleClient\s+as\b' "$src"
# The server is the worker crate's (web/worker), never linked into the page.
ban "web/src names pimble_server (the server runs in the worker)" '\bpimble_server\b' "$src"
ban "web/Cargo.toml depends on pimble-server (the server runs in the worker)" \
    '^[[:space:]]*pimble-server\b' "$web/Cargo.toml"

# The old path's modules and switch.
ban "web/src declares an old-path module" '\bmod[[:space:]]+(vault|vault_backend|routed|endpoints)\b' "$src"
ban "web/ mentions the old path's switch" 'server=old' "$web"

if [[ "$failed" -ne 0 ]]; then
    echo "check-one-path: failed; see docs/ONE_CODEBASE_CONTRACT.md section 8" >&2
    exit 1
fi
echo "check-one-path: web/src holds one path"
