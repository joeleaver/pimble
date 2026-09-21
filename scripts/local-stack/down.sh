#!/bin/bash
# Stops exactly the processes up.sh and desk.sh started (by pid, never by name).
. "$(dirname "$0")/env.sh"
for pid in $(cat $STACK/pids 2>/dev/null); do kill $pid 2>/dev/null; done
