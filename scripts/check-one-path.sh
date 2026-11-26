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

# report <description> <hits>
report() {
    if [[ -n "$2" ]]; then
        echo "check-one-path: $1" >&2
        echo "$2" | sed "s|^$root/|  |" >&2
        failed=1
    fi
}

# broken <description> <tool>: a check that could not run. Never treated as
# "nothing found": a ban whose pattern does not compile would be off.
broken() {
    echo "check-one-path: $2 failed on the check \"$1\"; the check could not run" >&2
    exit 2
}

# ban <description> <extended regex> <path>...
ban() {
    local what="$1" pattern="$2"
    shift 2
    local hits status=0
    hits="$(grep -rnHE --exclude-dir=target --exclude-dir=dist --include='*.rs' --include='*.js' --include='*.html' --include='*.md' --include='*.toml' -e "$pattern" "$@")" || status=$?
    # grep: 0 found, 1 nothing found, anything else an error.
    [[ "$status" -le 1 ]] || broken "$what" grep
    report "$what" "$hits"
}

# ban_joined <description> <perl regex> <path>...
# The same as `ban`, over each .rs file whole, so a declaration wrapped across
# lines is caught: `\s` and negated classes match newlines. Reported at the
# line the match starts on, with its whitespace collapsed.
ban_joined() {
    local what="$1" pattern="$2"
    shift 2
    ban_joined_where "$what" '(?:)' "$pattern" "$@"
}

# ban_joined_where <description> <perl regex for the file> <perl regex> <path>...
# `ban_joined` over only the files the first regex matches somewhere. Never
# pass an empty regex: perl reads an empty pattern as "the last one matched".
ban_joined_where() {
    local what="$1" guard="$2" pattern="$3"
    shift 3
    # Compile both first, so a broken pattern fails even with no files to read.
    GUARD="$guard" PATTERN="$pattern" perl -e 'qr/$ENV{GUARD}/; qr/$ENV{PATTERN}/' ||
        broken "$what" perl
    local hits
    # perl prints nothing and exits 0 when nothing matches; xargs exits
    # non-zero when any perl did (a pattern that fails at run time).
    hits="$(find "$@" -type f -name '*.rs' -not -path '*/target/*' -not -path '*/dist/*' -print0 |
        GUARD="$guard" PATTERN="$pattern" xargs -0 -r perl -0777 -ne '
            next unless /$ENV{GUARD}/;
            while (/$ENV{PATTERN}/g) {
                my $line = (substr($_, 0, $-[0]) =~ tr/\n//) + 1;
                (my $match = $&) =~ s/\s+/ /g;
                print "$ARGV:$line: $match\n";
            }')" || broken "$what" perl
    report "$what" "$hits"
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
# Any `Router` in a file that names pimble_api, however it is imported (a
# nested use tree, an alias, a path), and a `Router::` path anywhere.
ban_joined_where "web/src builds a pimble_api Router (stores routed to another server)" \
    '\bpimble_api\b' '\bRouter\b' "$src"
ban_joined "web/src builds a Router (stores routed to another server)" '\bRouter\s*::' "$src"
# `RpcBackend` anywhere in web/src, in any form (a turbofish, an alias, a type
# named elsewhere), but the one constructor over `LocalConnector` and a plain
# import (a name followed by `,`, `}` or `;`).
ban_joined "web/src makes an RPC backend over anything but LocalConnector (the page's one server is its worker)" \
    '\bRpcBackend\b(?!\s*::\s*new\s*\(\s*LocalConnector\b)(?!\s*[,};])' "$src"
# A type alias is a name followed by `;` too, so it is banned on its own.
ban_joined "web/src aliases RpcBackend (the page's one server is its worker)" \
    '\btype\b[^;={}]*=\s*(?:[\w:]*::)?RpcBackend\b' "$src"
ban_joined "web/src implements a pimble_api Connector other than LocalConnector (the page's one server is its worker)" \
    '\bimpl\b[^{;]*\bConnector\s+for\s+(?!LocalConnector\b)\w+' "$src"
ban_joined "web/src opens a PimbleClient other than over_message_port (the page's one server is its worker)" \
    '\bPimbleClient\s*::\s*(?!over_message_port\b)\w+|\bPimbleClient\s+as\b' "$src"
# The server is the worker crate's (web/worker), never linked into the page.
ban "web/src names pimble_server (the server runs in the worker)" '\bpimble_server\b' "$src"
# In any form: a key, a renamed dependency's `package = "pimble-server"`, a
# `[dependencies.pimble-server]` table. Every manifest of the web workspace
# but the worker's (which links the server through pimble-worker).
manifests=()
while IFS= read -r -d '' manifest; do
    manifests+=("$manifest")
done < <(find "$web" -name Cargo.toml -not -path "$web/worker/*" -not -path '*/target/*' -not -path '*/dist/*' -print0)
# With no manifest, grep would search the working directory instead.
if [[ ${#manifests[@]} -eq 0 ]]; then
    echo "check-one-path: no Cargo.toml found under $web; the manifest check could not run" >&2
    exit 2
fi
ban "the page's Cargo.toml names pimble-server (the server runs in the worker)" \
    'pimble[-_]server' "${manifests[@]}"

# The old path's modules and switch.
ban "web/src declares an old-path module" '\bmod[[:space:]]+(vault|vault_backend|routed|endpoints)\b' "$src"
ban "web/ mentions the old path's switch" 'server=old' "$web"

if [[ "$failed" -ne 0 ]]; then
    echo "check-one-path: failed; see docs/ONE_CODEBASE_CONTRACT.md section 8" >&2
    exit 1
fi
echo "check-one-path: web/src holds one path"
