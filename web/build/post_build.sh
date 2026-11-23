#!/bin/sh
# Trunk's post_build hook (Trunk.toml), run in the staging directory's
# parent with TRUNK_STAGING_DIR set, before the build replaces dist/.
#
# 1. The browser's own server (docs/ONE_CODEBASE_CONTRACT.md wave 4) is built
#    by trunk under fixed names (pimble-worker_loader.js, pimble-worker.js,
#    pimble-worker_bg.wasm): trunk hashes no worker. They are renamed by a
#    hash of their contents, the loader rewritten to load its own pair, and
#    the page told the loader's name (the `pimble-worker-loader` meta tag in
#    index.html, which hub.js reads), so a page always starts the worker of
#    its own build, whatever a cache kept.
# 2. sw.js, the service worker that keeps this build's own files for a page
#    opened with no network, is written from sw.template.js with this build's
#    id and the list of its files.
#
# POSIX sh and coreutils only: the deploy's trunk buildpack runs it too.
set -eu
dir="${TRUNK_STAGING_DIR:?trunk sets TRUNK_STAGING_DIR}"
here="$(cd "$(dirname "$0")" && pwd)"
base="${TRUNK_PUBLIC_URL:-/app/}"

hash_of() { cat "$@" | sha256sum | cut -c1-16; }

if [ -f "$dir/pimble-worker_bg.wasm" ]; then
  h=$(hash_of "$dir/pimble-worker.js" "$dir/pimble-worker_bg.wasm")
  mv "$dir/pimble-worker.js" "$dir/pimble-worker-$h.js"
  mv "$dir/pimble-worker_bg.wasm" "$dir/pimble-worker-${h}_bg.wasm"
  rm -f "$dir/pimble-worker_loader.js"
  printf 'importScripts("./pimble-worker-%s.js");wasm_bindgen("./pimble-worker-%s_bg.wasm");' "$h" "$h" > "$dir/pimble-worker-${h}_loader.js"
  sed -i "s|content=\"pimble-worker_loader.js\"|content=\"pimble-worker-${h}_loader.js\"|" "$dir/index.html"
  grep -q "pimble-worker-${h}_loader.js" "$dir/index.html" || { echo "post_build: index.html names no worker loader" >&2; exit 1; }
fi

# Every file of this build, as the page asks for it, and the build's id: a
# hash of them all, names and bytes, so any change makes a new cache.
files=$(cd "$dir" && find . -type f ! -name sw.js | sed 's|^\./||' | LC_ALL=C sort)
build=$(cd "$dir" && for f in $files; do printf '%s\n' "$f"; cat "$f"; done | sha256sum | cut -c1-16)
list=$(for f in $files; do printf '"%s%s",' "$base" "$f"; done)
sed -e "s|__PIMBLE_BUILD__|$build|" -e "s|\"__PIMBLE_FILES__\"|${list%,}|" -e "s|__PIMBLE_BASE__|$base|g" "$here/sw.template.js" > "$dir/sw.js"
echo "post_build: build $build, $(echo "$files" | wc -l) files kept for offline"
