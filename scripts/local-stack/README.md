# The local stack

Everything Pimble Cloud is in production, on this machine: RhypeDB, the accounts
service, a hosted Pimble server in JWT mode, the web app, and any number of
"desktops" (a Pimble server each, with its own keystore and replicas). It is how
sharing is verified end to end. Nothing here touches the person's own app on
`127.0.0.1:7462`, its config or its data; the scripts stop only the pids they started.

```bash
cargo build --release -p pimble-cli -p pimble-cloud -p pimble-app
(cd web && trunk build --release)
(cd scripts/local-stack/signup-helper && cargo build --release)
scripts/local-stack/up.sh                      # ports 14311/14312, 17490, 18090, 18091
scripts/local-stack/desk.sh owner 17491
scripts/local-stack/desk.sh bob 17492
scripts/local-stack/desk.sh carol 17493
. scripts/local-stack/env.sh                   # own, bob, carol, hosted: the CLI per machine
```

`pimble-cli` talks to 7462 unless told otherwise. Use the functions from `env.sh`, never
the bare binary.

## Accounts

```bash
H=scripts/local-stack/signup-helper/target/release/signup-helper
for who in alice bob carol; do $H $EDGE $who@example.com "pw-$who-0123456789"; done
grep -o "$EDGE[^ \"]*verify[^ \"]*" $STACK/cloud.log | sort -u | xargs -n1 curl -s -o /dev/null -L
PIMBLE_CLOUD_PASSWORD=pw-alice-0123456789 own cloud-sign-in $EDGE alice@example.com
```

## The sharing walk-through (what the PM ran on 2026-09-21)

1. `own create-store $STACK/owner/family.pimble Family`, a few folders and notes
   (`create-node`, `set-node-text`).
2. `own cloud-share <store> <folder>` before hosting answers, word for word, "Sharing needs
   this store hosted on Pimble Cloud, or the relay, which is not built yet. Nothing was
   uploaded." and `$STACK/stores` stays empty.
3. `own cloud-host-store <store>`, `own cloud-share <store> <folder> --name "..."`,
   `own cloud-share-invite <store> <folder> bob@example.com editor` (and carol; a second
   folder with carol as `reader`).
4. bob and carol: `cloud-sign-in`, `cloud-list-hosted` (the share's name and "shared by",
   never the owner's store name), `cloud-add-hosted <store>`.
5. **Stop the owner's server** (`kill $(cat $STACK/owner.pid)`). bob creates a folder and a
   note in it, moves a node, writes text; carol sees all of it, creates, moves, deletes;
   bob sees that. A reader's write answers "You can read this, not change it."
6. `grep -rl <a typed word> $STACK/stores` finds nothing; nothing of the owner's unshared
   folders is on bob's or carol's disk.
7. Owner back (`desk.sh owner 17491 $STACK/owner/family.pimble`): its tree has everything
   the members did. A note it moves into the share reaches them, keys and all; moved back
   out, its later edits do not.
8. A role changed by the owner, a second share of the same store, a removal, a stopped
   share: each reaches a member's machine within two minutes (the link asks the accounts
   service every two minutes and connects again with a fresh token).

## The apps on the same identities

Stop the headless desktop for that name, write
`$STACK/<name>/config/pimble/state.json` (`{"open_stores": ["<path>"]}`), then:

```bash
XDG_CONFIG_HOME=$STACK/owner/config XDG_DATA_HOME=$STACK/owner/data \
  PIMBLE_APP_ADDR=127.0.0.1:17491 target/release/pimble
```

A sign-in made through the CLI lives in that identity's keystore, so the app starts signed
in. The web app is at `http://127.0.0.1:18091/app/` (it asks for the password: keys live in
memory only).

## Two or three accounts in one browser

Cookies are per host, so each account gets a host of its own: `127.0.0.1`, `localhost`
(the same listener) and `127.0.0.2` (a second edge:
`node scripts/local-stack/edge.js 18091 18090 17490 web/dist 127.0.0.2 &`). The hosted
server has to admit each as an origin: start it with `--allow-origin` for all three
(`up.sh` does). The token's issuer and the
`rpc_url` stay `127.0.0.1` whichever host the page came from, which is fine: a WebSocket
to another origin is allowed by the browser and judged by the server's allowlist.

`scripts/local-stack/down.sh` stops everything.
