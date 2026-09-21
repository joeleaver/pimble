# Source this. Everything the local stack uses, on ports well away from the
# person's own Pimble app (127.0.0.1:7462, which nothing here ever touches).
STACK=${PIMBLE_STACK_DIR:-/tmp/pimble-local-stack}
REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
CLI=$REPO/target/release/pimble-cli
EDGE=http://127.0.0.1:18091          # the one origin: /app/, /api/, /rpc
HOSTED=http://127.0.0.1:17490        # the hosted Pimble server (static token in $STACK/hosted-token)

# The CLI defaults to 7462. Never call $CLI bare: use one of these.
own()    { PIMBLE_SERVER=http://127.0.0.1:17491 $CLI "$@"; }
bob()    { PIMBLE_SERVER=http://127.0.0.1:17492 $CLI "$@"; }
carol()  { PIMBLE_SERVER=http://127.0.0.1:17493 $CLI "$@"; }
hosted() { PIMBLE_SERVER=$HOSTED PIMBLE_TOKEN=$(cat $STACK/hosted-token) $CLI "$@"; }
