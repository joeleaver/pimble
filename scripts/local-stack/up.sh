#!/bin/bash
# The whole stack on this machine: RhypeDB, the hosted Pimble server (JWT mode),
# the accounts service, and one origin in front of them that also serves web/dist.
# Needs: cargo build --release -p pimble-cli -p pimble-cloud; (cd web && trunk build --release);
# ~/dev/rhypedb/target/release/rhypedb-server; node.
set -u
. "$(dirname "$0")/env.sh"
rm -rf $STACK && mkdir -p $STACK/{accounts,hosted,stores}
: > $STACK/pids
echo "local-stack-service-token" > $STACK/hosted-token

~/dev/rhypedb/target/release/rhypedb-server --schema $REPO/crates/pimble-cloud/schema.rhype --data-dir $STACK/accounts \
  --listen 127.0.0.1:14311 --tcp-listen 127.0.0.1:14312 > $STACK/rhypedb.log 2>&1 &
echo $! >> $STACK/pids

# The hosted server first: the accounts service exits without it.
XDG_CONFIG_HOME=$STACK/hosted/config XDG_DATA_HOME=$STACK/hosted/data RUST_LOG=pimble_server=info \
  $CLI server --addr 127.0.0.1:17490 --stores-dir $STACK/stores --token-file $STACK/hosted-token \
  --jwks http://127.0.0.1:18090/api/v1/.well-known/jwks.json --issuer $EDGE/api/v1 \
  --allow-origin $EDGE --allow-origin http://localhost:18091 --allow-origin http://127.0.0.2:18091 > $STACK/hosted.log 2>&1 &
echo $! >> $STACK/pids
sleep 2

# No RESEND_API_KEY: verification links are logged to $STACK/cloud.log, not mailed.
RUST_LOG=info RHYPEDB_ADDR=127.0.0.1:14312 PIMBLE_SERVER_URL=$HOSTED PIMBLE_SERVER_TOKEN=$(cat $STACK/hosted-token) \
  PIMBLE_STORES_DIR=$STACK/stores PIMBLE_CLOUD_DEV_SIGNING_SEED=$(head -c32 /dev/urandom | xxd -p -c64) \
  PIMBLE_CLOUD_KDF_DECOY_SECRET=$(head -c32 /dev/urandom | xxd -p -c64) PIMBLE_CLOUD_PUBLIC_URL=$EDGE PORT=18090 \
  $REPO/target/release/pimble-cloud > $STACK/cloud.log 2>&1 &
echo $! >> $STACK/pids

node "$(dirname "$0")/edge.js" 18091 18090 17490 $REPO/web/dist > $STACK/edge.log 2>&1 &
echo $! >> $STACK/pids
sleep 3
for p in 14311/health 18091/api/v1/health 18091/app/; do curl -s -o /dev/null -w "$p %{http_code}\n" http://127.0.0.1:$p; done
