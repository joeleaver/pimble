# Deploying Pimble

Everything server-side runs on [jkbase](https://github.com/joeleaver/jkbase), Joe's own
platform. This is the operator's guide: standing the project up, the secrets it needs, and
how to run the whole stack on your own machine first. The shape being deployed (identity
model, endpoints, ownership) is `docs/CLOUD_CONTRACT.md`; this file is just the mechanics.

## One-time project setup

```bash
jkbase login --token YOUR_JKBASE_TOKEN     # or pipe the token on stdin
jkbase project create pimble
jkbase project info pimble                 # prints the project id -- you need it below
```

`jkbase.toml` at the repo root already declares the four things this project serves (the
static `site/`, the trunk-built `web/` at `/app`, and the two source-built servers
`crates/pimble-cloud` and `crates/pimble-cli` at `/api/*` and `/rpc`) plus the managed
RhypeDB schema. Nothing there needs editing to deploy; it's committed.

## Custom domain (pimble.app)

The project already has `pimble.app` and `www.pimble.app` attached (`jkbase domain list
--project pimble`). Custom hosts get a per-host certificate over HTTP-01 after the TXT
verification passes, so the records point straight at the platform host with Cloudflare's
proxy **off** (DNS only): jkbase terminates TLS itself. Records in the Cloudflare zone:

| Type | Name | Content |
| --- | --- | --- |
| A | `pimble.app` | `54.39.17.150` |
| CNAME | `www` | `pimble.app` |
| TXT | `_jkbase-challenge` | the token `jkbase domain list --project pimble` shows for `pimble.app` |
| TXT | `_jkbase-challenge.www` | the token it shows for `www.pimble.app` |

All four records were created and both hosts verified on 2026-09-15; Let's Encrypt
certificates for `pimble.app` and `www.pimble.app` are issued. For a new host: `jkbase
domain add`, add its records, then `jkbase domain verify <host>`; the
certificate follows within a minute or two (`domain list` shows `tls: provisioning` until
then). `.app` is on the HSTS preload list, so the site is only ever reachable over HTTPS.

## jkbase-Auth issuer key

The accounts service (`pimble-cloud`) mints per-user tokens by presenting a `jkbk_…` issuer
key to jkbase-Auth:

```bash
jkbase auth key create --label pimble-cloud   # prints a jkbk_... key -- shown once, save it
```

That key is the `JKBASE_AUTH_KEY` secret below. `JKBASE_AUTH_ISSUER_URL` is
`https://auth.jkbase.app/v1/projects/pimble` (the project id is `pimble`, from `project info`).

## Secrets

Set these before the first deploy (`jkbase secret set NAME=value`); a later change takes
effect on the next `jkbase deploy`, or immediately with `jkbase restart` (no rebuild):

| Secret | Value |
| --- | --- |
| `PIMBLE_SERVER_TOKEN` | A long random string. This is the static service-principal token: generate one (e.g. `openssl rand -hex 32`) and set the *same* value for both servers -- `pimble-cloud` presents it to `pimble-cli server` as its `Authorization: Bearer`, and `pimble-cli server` accepts it as its own `--token-file`/`PIMBLE_TOKEN`-equivalent static token. |
| `JKBASE_AUTH_ISSUER_URL` | `https://auth.jkbase.app/v1/projects/pimble` |
| `JKBASE_AUTH_KEY` | The `jkbk_…` key from `jkbase auth key create` above |
| `PIMBLE_JWKS_URL` | `https://auth.jkbase.app/v1/projects/pimble/.well-known/jwks.json` -- what `pimble-cli server` fetches to verify user JWTs |
| `PIMBLE_JWT_ISSUER` | Same as `JKBASE_AUTH_ISSUER_URL` -- the `iss` claim `pimble-cli server` checks |
| `PIMBLE_ALLOW_ORIGINS` | `https://pimble.app,https://www.pimble.app,https://pimble.jkbase.app` |
| `RESEND_API_KEY` | (phase 2) Resend key for `m.pimble.app`, the sending domain for verification and invitation email |
| `PIMBLE_CLOUD_PUBLIC_URL` | `https://pimble.app` -- used for the session cookie's `Secure` flag and, in development-signing mode only, as the fallback token issuer |
| `PIMBLE_STORES_DIR` | `/app/data/stores` -- must match the path `pimble-cli server --stores-dir` is given in `jkbase.toml`; `pimble-cloud` uses this same value when it asks the server to create a store |

`pimble-cloud`'s own `RHYPEDB_ADDR` (`127.0.0.1:4201`) and `PIMBLE_SERVER_URL`
(`http://127.0.0.1:7462`) are the loopback defaults baked into the same VM and don't need a
secret unless something is moved off those ports.

## The two lock files must agree on wasm-bindgen

jkbase's trunk buildpack provisions the `wasm-bindgen` CLI for the offline build from the
`Cargo.lock` at the **build context root** (the repo root, because `web/` needs
`context = "."` for its path dependencies), not from `web/Cargo.lock`. If the two locks
pin different `wasm-bindgen` versions the web target fails after cargo finishes, with no
useful error in the tail (first deploy, 2026-09-15: root 0.2.108, web 0.2.128). Keep them
equal: after `cargo update` in either workspace, run
`cargo update -p wasm-bindgen -p js-sys -p web-sys -p wasm-bindgen-futures` in both and
check `grep -A1 'name = "wasm-bindgen"' Cargo.lock web/Cargo.lock`. The right fix is in
jkbase (read the lock of the cargo workspace that contains `source`, walking up from it).

## Push to deploy

The repo has a second git remote, `jkbase` (`jkbase repo connect`, added 2026-09-15; the
push token lives only in `.git/config`, nothing tracked). The platform builds whatever
lands on its `main` branch, and that name is fixed on the platform side, while this repo's
default branch is `master`. The remote's push refspec is set to `HEAD:refs/heads/main`, so
from any branch:

```bash
git push jkbase          # pushes the current HEAD to the platform's main: build + deploy
```

Watch it with `jkbase deployments` and `jkbase logs -f`. `jkbase repo token` re-mints the
push token (revoking the old one); `jkbase repo disconnect` removes it. If another machine
needs the remote, run `jkbase repo connect` there and set the same refspec:
`git config remote.jkbase.push 'HEAD:refs/heads/main'`.

## Deploy

```bash
jkbase deploy
```

This builds `site/` (committed, served as-is), `web/` (trunk, server-side), and both
`crates/pimble-cloud` and `crates/pimble-cli` (the rust buildpack, offline `cargo build
--release` after an online `cargo fetch`) and provisions the managed RhypeDB from
`crates/pimble-cloud/schema.rhype`. The whole project is one microVM at
`https://pimble.jkbase.app`.

```bash
jkbase logs -f --service cloud     # tail the accounts service
jkbase logs -f --service pimble    # tail the hosted Pimble server
jkbase deployments                 # history; jkbase rollback --version N to revert
```

## Running the full stack locally

Four pieces, four terminals. None of this touches jkbase.

**1. The hosted Pimble server, in JWT mode, on loopback:**

```bash
cargo run -p pimble-cli --release -- server \
  --addr 127.0.0.1:7462 \
  --stores-dir ./dev-stores \
  --jwks http://127.0.0.1:8080/api/v1/.well-known/jwks.json \
  --issuer http://127.0.0.1:8080/api/v1 \
  --token-file ./dev-service-token.txt \
  --allow-origin http://127.0.0.1:8000
```

(`./dev-service-token.txt` holds whatever string you also set as `PIMBLE_SERVER_TOKEN`
below; create it once with `echo dev-secret > dev-service-token.txt`.)

**2. A local RhypeDB** (`rhypedb-server`, listening on `127.0.0.1:4201`/`:4200` -- see that
repo's own docs for running it standalone; the managed instance jkbase provisions in
production doesn't exist locally).

**3. `pimble-cloud`:**

```bash
RHYPEDB_ADDR=127.0.0.1:4201 \
PIMBLE_SERVER_URL=http://127.0.0.1:7462 \
PIMBLE_SERVER_TOKEN=dev-secret \
PIMBLE_STORES_DIR=./dev-stores \
PIMBLE_CLOUD_PUBLIC_URL=http://127.0.0.1:8080 \
PORT=8080 \
cargo run -p pimble-cloud --release
```

Leaving `JKBASE_AUTH_ISSUER_URL` unset puts `pimble-cloud` in development-signing mode: it
signs tokens itself with an Ed25519 key it generates (`PIMBLE_CLOUD_DEV_SIGNING_SEED` pins
it across restarts) and serves its own JWKS at `/api/v1/.well-known/jwks.json`, which is
what `--jwks`/`--issuer` above point at -- no jkbase account needed to develop against.

**4. The site and the web app:**

```bash
cd site && python3 -m http.server 8000
cd web && trunk serve   # or `trunk build` and serve dist/ at /app with any static server
```

Open `http://127.0.0.1:8000`. Signup, login and the account page talk to `pimble-cloud` on
`:8080`; wire a reverse proxy (or just open `pimble-cloud` directly at `:8080/api/v1/...`
while developing) if you want the same-origin path layout jkbase gives you in production.
The web app at `/app` fetches its own token from `pimble-cloud` and connects to
`pimble-cli server`'s WebSocket directly.
