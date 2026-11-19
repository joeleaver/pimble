#!/usr/bin/env bash
# Runs the diagnostics server locally, with its own RhypeDB under
# diagnostics/.data. Send key dev-key, read key dev-read-key:
#   PIMBLE_DIAGNOSTICS_URL=http://127.0.0.1:8788 tools/diagnostics.sh stats
# (with PIMBLE_DIAGNOSTICS_READ_KEY_FILE naming a file that holds dev-read-key).
set -euo pipefail
cd "$(dirname "$0")"

RHYPEDB_BIN=${RHYPEDB_BIN:-$HOME/dev/rhypedb/target/release/rhypedb-server}
PORT=${PORT:-8788}
DB_PORT=${DB_PORT:-4292}
mkdir -p .data

"$RHYPEDB_BIN" --schema schema.rhype --data-dir .data/db \
  --listen "127.0.0.1:$DB_PORT" --tcp-listen "127.0.0.1:$((DB_PORT + 1))" &
db=$!
trap 'kill $db 2>/dev/null' EXIT

PORT=$PORT SEND_KEY=${SEND_KEY:-dev-key} READ_KEY=${READ_KEY:-dev-read-key} RHYPEDB_URL="http://127.0.0.1:$DB_PORT" \
  node server.js
