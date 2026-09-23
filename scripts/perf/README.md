# Profiling the desktop app

How the 2026-09-22/23 performance work was measured (`docs/NEXT_SESSION.md`, v0.4.0).
Use the same method before and after any change that claims to make typing faster, and
compare like with like: same store copy, same window size, same steps.

## 1. A profiling build and an isolated app

A release build with line tables and frame pointers, in its own target directory so it
does not disturb `target/`:

```bash
export P=/tmp/pimble-perf            # any scratch directory
CARGO_TARGET_DIR=$P/target CARGO_PROFILE_RELEASE_DEBUG=line-tables-only \
  RUSTFLAGS="-C force-frame-pointers=yes" cargo build -p pimble-app --release
```

Run it on a **copy** of a real store with its own config and data directories, so your
real stores and `state.json` are never touched (typing into the copy edits it):

```bash
mkdir -p $P/env/config/pimble $P/env/data/pimble
cp -r ~/dev/scrivener_convert/family-management.pimble $P/env/family.pimble
ln -sfn ~/.local/share/pimble/models $P/env/data/pimble/models   # skip the model download
printf '{"open_stores":["%s/env/family.pimble"]}\n' $P > $P/env/config/pimble/state.json
XDG_CONFIG_HOME=$P/env/config XDG_DATA_HOME=$P/env/data RINCH_PERF=1 \
  $P/target/release/pimble > $P/app.log 2>&1 &
```

The embedded server takes `127.0.0.1:7462`, so quit your own Pimble first. Find the
process with `pgrep -x pimble` (never `pgrep -f`: it matches your own shell).

- `RINCH_PERF=1`: rinch logs every frame's resolve (style, layout, `build_ifc`,
  `taffy_compute`) and paint times.
- `RINCH_RENDERER=cpu` or `--cpu`: the software renderer instead of the GPU.
- The window opens at 1200x800; its maximize button is at `(1121, 17)`
  (`python3 rd.py $PID click '{"x":1121,"y":17}'`) for a 4K window.

## 2. Drive it

`rd.py` talks to rinch's debug port; `scenario.py` runs steps and sums the `[PERF]` lines
each one caused. Coordinates below are the family store's layout at 1200x800:

```bash
cd scripts/perf
python3 scenario.py $PID $P/app.log '[["click",25,213,"expand MEDICAL"],
  ["click",145,367,"open a doc"],["click",600,140,"focus"],["type","hello world"],
  ["click",200,52,"search box"],["type","Open SRS"],["click",60,122,"open the long doc"],
  ["click",700,300,"click in text"],["type","hello world"],["shot","/tmp/after.png"]]'
```

"Open SRS" is the store's longest document (~9,500 words). `ui-busy` includes the driver's
own round trips per character; it is an upper bound.

## 3. Measure what the UI thread really spends

Per thread **id** (two threads are named `pimble`; matching by name mixes them up):

```bash
snap() { for t in /proc/$PID/task/*; do echo "$(basename $t) $(cat $t/comm | tr ' ' _) \
  $(awk '{print $14+$15}' $t/stat)"; done | sort; }
snap > before; python3 scenario.py $PID $P/app.log '[["type","thirty two characters of typing."]]'
snap > after; join before after | awk '{print $1, $2, ($5-$3)*10 " ms"}' | sort -k3 -n -r | head
```

The row whose id is `$PID` is the UI thread. At v0.4.0 on a 4K window: ~12 ms per
keystroke in a short document, ~22 ms in the long one.

`perf` needs root here (`perf_event_paranoid` is 4): `sudo perf record -F 2000 -g -t $PID`.
On GPU builds report with `perf report --no-inline` (resolving inlined frames in the
Vulkan driver stalls for minutes).

## 4. What each keystroke writes to the page

`rinch-domlog.patch` (against rinch `main` at 4200dda) makes rinch log, with
`RINCH_DOMLOG=1`, every DOM write with the element it lands on and whether it changed
anything (`set_text(same)`, `set_attr(same)`), and `[PAINTED] nodes=.. text_layouts=..`
per painted frame. That census is what found the toolbar and tree-label rewrites.

Never patch `/home/joe/dev/rinch`. Clone rinch into a scratch directory, apply the patch,
and point a build at it with a cargo config file (pimble's `Cargo.toml` stays untouched):

```bash
git clone https://github.com/joeleaver/rinch.git $P/rinch && git -C $P/rinch apply \
  $PWD/rinch-domlog.patch
{ echo '[patch."https://github.com/joeleaver/rinch.git"]'
  for c in $(awk '/^name = "rinch/{n=$3} /^source = "git\+https:\/\/github.com\/joeleaver\/rinch.git/{print n}' \
      ../../Cargo.lock | tr -d '"' | sort -u); do
    echo "$c = { path = \"$P/rinch/crates/$c\" }"; done; } > $P/rinch-patch.toml
CARGO_TARGET_DIR=$P/target-domlog cargo build -p pimble-app --release --config $P/rinch-patch.toml
git checkout Cargo.lock    # the patch build rewrites it
```

Every rinch crate in `Cargo.lock` has to be patched, or two copies of one crate meet.
