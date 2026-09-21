#!/bin/bash
# desk.sh <name> <port> [store to open]: a headless "desktop": a Pimble server
# with its own keystore (config) and replicas (data), as one person's machine has.
# For the real app on the same identity, stop this one and run:
#   XDG_CONFIG_HOME=$STACK/<name>/config XDG_DATA_HOME=$STACK/<name>/data \
#     PIMBLE_APP_ADDR=127.0.0.1:<port> target/release/pimble
# (a sign-in made through the CLI is in the keystore, so the app starts signed in;
# list the stores to open in $STACK/<name>/config/pimble/state.json: {"open_stores": ["<path>"]}).
. "$(dirname "$0")/env.sh"
name=$1; port=$2
mkdir -p $STACK/$name
XDG_CONFIG_HOME=$STACK/$name/config XDG_DATA_HOME=$STACK/$name/data RUST_LOG=pimble_server=info \
  $CLI server --addr 127.0.0.1:$port ${3:+--open $3} >> $STACK/$name.log 2>&1 &
echo $! > $STACK/$name.pid
echo $! >> $STACK/pids
