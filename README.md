# Pimble

An offline-first personal information manager.

- Nodes in a tree inside a store (a `.pimble` directory on disk).
- Every node's content is a CRDT document, so devices and people edit concurrently and
  merge without conflicts.
- Mounts: any subtree of any store can appear in any other store's tree, locally or from a
  remote Pimble server.
- Search: keyword and semantic, over a local index that is rebuilt on demand.
- Import from Scrivener, formatting included.

Pimble is a Rust workspace. The desktop app is built on the [rinch](https://github.com/joeleaver/rinch)
UI framework and runs on Linux and Windows. A CLI (`pimble-cli`) drives a Pimble server
headlessly, and the same server hosts stores for the web app and for replicas.

## Build and run

```bash
cargo build -p pimble-app --release
cargo run -p pimble-app --release
```

Always use `--release`; debug builds are unusably slow. The first run downloads the
embedding model for semantic search into your data directory; without it the app runs with
keyword search only.

## Layout

| Path | What |
| --- | --- |
| `crates/pimble-core` | Node, Store, MountRef types |
| `crates/pimble-crdt` | the two yrs documents: per-node content and the store tree |
| `crates/pimble-store` | on-disk store, `store.yrs` plus `nodes/{id}.yrs` |
| `crates/pimble-server`, `-rpc`, `-client` | JSON-RPC over WebSocket between UI and store, replica sync, auth |
| `crates/pimble-search` | rhypedb index per store |
| `crates/pimble-app` | the desktop app (a library plus the `pimble` binary) |
| `web/` | the same UI built for the browser |
| `crates/pimble-cli` | headless server and store administration |
| `crates/pimble-cloud` | accounts service for Pimble Cloud |
| `crates/pimble-import` | Scrivener and RTF import |
| `site/` | the website |

Design documents live in `docs/`; start with `docs/ARCHITECTURE.md`. Deployment is
described in `docs/DEPLOY.md`.

## License

MIT, see `LICENSE`.
