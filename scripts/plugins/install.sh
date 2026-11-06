#!/bin/bash
# Build Pimble plugins and install them into this device's plugins folder
# (docs/PLUGINS_CONTRACT.md decision 3: a per-device folder, installed by hand).
#
#   scripts/plugins/install.sh                 # the kanban, todo list and timeline viewers
#   scripts/plugins/install.sh kanban-viewer hello-viewer
#
# The folder is <data dir>/pimble/plugins/: ~/.local/share/pimble/plugins on
# Linux ($XDG_DATA_HOME when set), ~/Library/Application Support/pimble/plugins
# on macOS. PIMBLE_DATA_DIR names another data dir (a scratch app's, for
# testing). A running Pimble watches the folder and takes a plugin installed
# here at once (docs/SETTINGS_CONTRACT.md section 3); the first time a plugin is
# shown it asks to be allowed what it needs (Settings > Plugins lists them and
# takes approvals back).
#
# The build runs under scripts/guarded.sh (a shared machine: memory-capped, low
# priority, CARGO_BUILD_JOBS jobs, 4 by default here).
set -euo pipefail
here="$(cd "$(dirname "$0")/../.." && pwd)"
plugins=("$@")
if [ ${#plugins[@]} -eq 0 ]; then
  plugins=(kanban-viewer todo-viewer timeline-viewer)
fi

if [ -n "${PIMBLE_DATA_DIR:-}" ]; then
  data="$PIMBLE_DATA_DIR"
elif [ "$(uname)" = "Darwin" ]; then
  data="$HOME/Library/Application Support"
else
  data="${XDG_DATA_HOME:-$HOME/.local/share}"
fi
dest="$data/pimble/plugins"

packages=()
for p in "${plugins[@]}"; do
  packages+=(-p "$p")
done
(cd "$here/plugins" && CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" "$here/scripts/guarded.sh" \
  cargo build --release --target wasm32-wasip1 "${packages[@]}")

mkdir -p "$dest"
for p in "${plugins[@]}"; do
  file="${p//-/_}.wasm"
  # Copied beside it and renamed over it, so a running Pimble never reads
  # half a plugin.
  cp "$here/plugins/target/wasm32-wasip1/release/$file" "$dest/$file.partial"
  mv "$dest/$file.partial" "$dest/$file"
  echo "installed $file into $dest"
done
echo "A running Pimble takes them at once."
