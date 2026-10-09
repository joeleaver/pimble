#!/bin/bash
# Run a heavy command (a workspace build, the test suite) inside a memory-capped
# systemd scope, at low priority, with a bounded number of cargo jobs.
#
#   scripts/guarded.sh cargo test --workspace --release
#
# This machine is shared with other sessions' workloads, and on 2026-10-08 it ran out
# of memory with a workspace test build in flight (the kernel killed another project's
# 17 GB python3, and everything else went down with the desktop). Inside the scope the
# worst this command can do is get itself killed: it can never take the machine with it.
# Override the cap or the job count with PIMBLE_GUARD_MEM (default 16G) and
# CARGO_BUILD_JOBS (default 8).
set -eu
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-8}"
exec systemd-run --user --scope --quiet --collect \
  -p MemoryMax="${PIMBLE_GUARD_MEM:-16G}" -p MemorySwapMax=1G -p CPUWeight=50 \
  nice -n 10 "$@"
