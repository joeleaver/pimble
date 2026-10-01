# Driving pimble-mcp by hand

`drive.py` is a minimal MCP client over stdio: it starts `pimble-mcp`, initializes, lists
the tools and calls each step of a JSON list of `[tool, {args}]` pairs, printing every
answer. Any stdout line that is not a protocol message fails the run (stdout carries the
protocol only, docs/MCP_CONTRACT.md). Two pseudo-steps pace a walk-through against the
app: `["_wait_file", {"path": ...}]` waits until the file exists, `["_sleep", {"s": n}]`.

Always on a store **copy**, its own config and data directories, and its own port, so
neither Joe's server nor his stores are touched (the same rule as `scripts/perf/`):

```bash
export M=/tmp/pimble-mcp-walk
mkdir -p $M/env/config/pimble $M/env/data/pimble
cp -r ~/dev/scrivener_convert/family-management.pimble $M/env/family.pimble
ln -sfn ~/.local/share/pimble/models $M/env/data/pimble/models
printf '{"open_stores":["%s/env/family.pimble"]}\n' $M > $M/env/config/pimble/state.json
cargo build -p pimble-mcp -p pimble-app --release
PIMBLE_APP_ADDR=127.0.0.1:7474 XDG_CONFIG_HOME=$M/env/config XDG_DATA_HOME=$M/env/data \
  python3 scripts/mcp/drive.py target/release/pimble-mcp scripts/mcp/tools.json
```

`tools.json` walks every tool on the family store copy (stderr lands in
`tools.json.stderr`). The handoff walk-through (2026-10-01, passed): start `pimble-mcp`
with a `_wait_file` step so it owns the server, start the app with the same environment
(it logs "Connected to existing server"), type in a note while `pimble-mcp` edits the same
paragraph, let `pimble-mcp` exit (the app logs "Started embedded server" within a second,
nothing lost), then run `pimble-mcp` again (it joins the app's server and reads what was
typed there).
