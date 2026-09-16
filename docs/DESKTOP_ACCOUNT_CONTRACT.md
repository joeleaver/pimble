# Desktop account contract: sign-in, hosting and hosted stores in the app

Status: written 2026-09-16 by the PM; built the same day by one agent and verified by the
PM in the GUI against the local four-process stack (sign-in, restart, "Add Hosted Store...",
"Host on Pimble Cloud...", the badge). Two server bugs the verification found are fixed
beside it: an adopted vault replica also got a plain sync link (`adopt_newly_opened` now
dispatches on `sync.json`'s mode, `ensure_link_started` refuses a store with a vault link),
and a vault replica's manifest kept its placeholder root after the pull
(`adopt_document_root`, called after every tree update and at vault-link start). Known
deviations: Enter in the password field does not submit (rinch's `PasswordInput` has no
submit prop); the hosted modal closes on `StoreOpened` only when the opened id is the
selected one; a successful sign-in clears the hint line.

Phase 2a (`docs/CRYPTO_CONTRACT.md`) built every
server piece the desktop needs and left one line for the UI: "The app: 'Account' in the
menu with Sign in (URL, email, password), Sign out, status in the status bar; store row
menu 'Host on Pimble Cloud…' and 'Add hosted store…'; the sync badge shows 'encrypted'
for vault links." This contract is that line, decided.

Everything here is `crates/pimble-app`, `native` only where it shows. Nothing in
`pimble-rpc`, `pimble-server` or `pimble-client` changes: the RPCs exist
(`cloudSignIn`, `cloudSignOut`, `cloudStatus`, `cloudHostStore`, `cloudListHostedStores`,
`cloudAddHostedStore`, all `Service`-only, which the app's tokenless embedded server is)
and `PimbleClient` wraps every one (`crates/pimble-client/src/client.rs`, `cloud_*`), and
`get_store_sync_with_mode` returns the link's `sync_mode`. The embedded server already
opens the default keystore (`<config dir>/pimble/keys.json`), so a sign-in survives an
app restart and `cloudStatus` reports it on the next start.

## Decisions

1. **One "Account..." modal, two faces.** A native menu cannot re-label items reactively
   and the spec forbids dead labels, so the Account menu is `Account...` and
   `Add Hosted Store...`. The modal shows the sign-in form (URL, email, password) when
   nothing is signed in and the signed-in view (email, service URL, a "Sign Out" button)
   when something is. The URL field defaults to `https://pimble.app` and remembers the
   last value typed in this run (a signal, not persisted; the keystore holds the
   signed-in URL).
2. **"Add Hosted Store..." lives in the Account menu**, not on a store row: it is about
   the account, not any store already open. The store row keeps "Host on Pimble Cloud...".
3. **Errors go to the modal that asked.** No pending-flag claim on the generic `Error`
   event this time: every cloud event names its operation, so `events.rs` routes without
   guessing (`CloudOp` below).
4. **The badge is the server's `sync_mode`.** `GetStoreSync` uses
   `get_store_sync_with_mode`; `StoreSyncChanged` carries `sync_mode`; the handler writes
   it into the store's `Store.sync_mode` in `store_data`. The row's badge reads
   `sync_mode` reactively: a vault link shows `encrypted · synced` / `encrypted · syncing`
   / `encrypted · offline`; a plain link shows the old `synced` / `syncing` / `offline`.
5. **Hosting needs a sign-in.** "Host on Pimble Cloud..." with no account signed in opens
   the Account modal instead (with a one-line hint "Sign in to host a store"). Signed in,
   it opens a small confirmation: store name, the account email, "Host" / "Cancel". The
   row item is `disabled` for a linked store, a replica, or a vault-kind store, snapshotted
   at render like `is_linked_now` (rinch #714). After a successful host the app sends
   `GetStoreSync` for the store (badge and mode) and `bump_tree_structure` (the row's
   disabled states flip).
6. **Adding a hosted store** lists the account's stores of kind `vault` whose id is not
   already open here (an open one is refused by the server anyway), in a `Select`
   (`name` as the label, `store_id` as the value); "Add" sends `CloudAddHostedStore`; the
   store arrives as `StoreOpened` exactly as `AddRemoteStore`'s does (the replica is in
   the replicas directory, "Remove Replica..." applies), and the modal closes on it.
7. **Status bar.** After the address: `Signed in as <email>` when signed in, nothing
   otherwise. Clicking it opens the Account modal. The app sends `CloudStatus` once on
   every `Connected`.
8. **Web build unchanged.** `process_command` handles the new commands on both targets
   (the wrappers compile for wasm); the menu entries, the row item, the modals and the
   status-bar text are `native` only. The browser signs in through its own pages.

## Protocol (`crates/pimble-app/src/protocol.rs`)

```rust
// BackendCommand
CloudStatus,
CloudSignIn { url: String, email: String, password: String },
CloudSignOut,
CloudHostStore { store_id: StoreId },
CloudListHostedStores,
CloudAddHostedStore { store_id: StoreId },
GetStoreSync { store_id: StoreId },            // unchanged; now answers with sync_mode

// BackendEvent
/// Which cloud request an outcome belongs to.
CloudOp { Status, SignIn, SignOut, HostStore, ListHostedStores, AddHostedStore }
CloudStatusChanged { signed_in: bool, email: Option<String>, url: Option<String> },
    // answers CloudStatus, CloudSignIn (signed_in: true), CloudSignOut (false)
CloudError { op: CloudOp, message: String },
CloudHostedStoresListed { stores: Vec<pimble_rpc::CloudHostedStoreInfo> },
CloudStoreHosted { store_id: StoreId },
StoreOpened { store },                          // answers CloudAddHostedStore
StoreSyncChanged { store_id, remote, state, sync_mode: pimble_core::StoreKind },
```

`SetStoreSync` answers through the same `StoreSyncChanged`; `set_store_sync` in the
client returns `GetStoreSyncResponse` fields, so the mode comes for free (add a
`set_store_sync_with_mode` to `pimble-client` only if the existing wrapper drops it, and
keep the old wrapper).

## State (`crates/pimble-app/src/state.rs`)

```rust
pub cloud_signed_in: Signal<bool>,
pub cloud_email: Signal<String>,
pub cloud_url: Signal<String>,

pub account_modal_open: Signal<bool>,
pub account_modal_url: Signal<String>,        // "https://pimble.app" when first opened
pub account_modal_email: Signal<String>,
pub account_modal_password: Signal<String>,   // cleared on close and on success
pub account_modal_password_visible: Signal<bool>,
pub account_modal_busy: Signal<bool>,
pub account_modal_error: Signal<String>,
pub account_modal_hint: Signal<String>,       // "Sign in to host a store", or empty

pub host_modal_store: Signal<Option<StoreId>>,
pub host_modal_busy: Signal<bool>,
pub host_modal_error: Signal<String>,

pub hosted_modal_open: Signal<bool>,
pub hosted_modal_stores: Signal<Vec<pimble_rpc::CloudHostedStoreInfo>>,
pub hosted_modal_selected: Signal<String>,    // store_id as text, like connect_modal_selected
pub hosted_modal_busy: Signal<bool>,
pub hosted_modal_error: Signal<String>,
```

`CloudHostedStoreInfo` is `Clone` but not `PartialEq`; if a reactive `for` needs it,
resolve rows to a local `PartialEq` struct as `search_rows` does.

## Events (`crates/pimble-app/src/events.rs`)

- `CloudStatusChanged`: set the three `cloud_*` signals. If the account modal is busy
  (a sign-in or sign-out just answered): busy false, error empty, password cleared; on a
  sign-in the modal stays open showing the signed-in face (the person sees it worked),
  on a sign-out it stays open showing the form.
- `CloudError { op, .. }`: `SignIn`/`SignOut`/`Status` to the account modal's error line
  (`Status` only logs when the modal is closed); `HostStore` to the host modal;
  `ListHostedStores`/`AddHostedStore` to the hosted modal. Each clears its own busy flag.
- `CloudHostedStoresListed`: fill `hosted_modal_stores` (already filtered to vault kind
  and not-open by the handler), select the first, busy false.
- `CloudStoreHosted`: close the host modal, `GetStoreSync { store_id }`,
  `bump_tree_structure`.
- `StoreOpened` while `hosted_modal_busy`: close the hosted modal, busy false.
- `StoreSyncChanged`: as now, plus write `sync_mode` into the store's `Store` signal
  (`store_data`) when it differs.
- `Connected`: also send `CloudStatus`.

## Menus (`crates/pimble-app/src/menus.rs`)

A new `MenuSection { title: "Account", .. }` between File and Edit, `native` only:
`Account...` (opens the modal), `Add Hosted Store...` (opens the hosted modal, which
sends `CloudListHostedStores` at once; not signed in: the hosted modal shows
"Sign in first" with a button that opens the Account modal). Update the test's
minimum count if it matters.

## App (`crates/pimble-app/src/app.rs`)

- `open_account_modal(store, hint: &str)`, `open_host_modal(store, store_id)`,
  `open_hosted_modal(store)`; the three modals rendered beside the existing ones (both
  `root` blocks list every modal; add the new ones to both so the web build compiles,
  even though the web menu never opens them).
- Store row context menu (store roots): `Host on Pimble Cloud...` with
  `TablerIcon::CloudUpload` (or `CloudLock` if it exists in `rinch-tabler-icons`), after
  "Link to Remote...", `disabled: is_linked_now || is_replica_now || is_vault_now`,
  `CAN_ADMINISTER_STORES` only.
- Badge text: a `sync_badge_text(state, mode)` returning the decision 4 strings.
- Status bar: the `Signed in as` span after the address, `native` only, `cursor: pointer`,
  onclick opens the Account modal.
- Tests: `menus::tests::every_item_has_an_action` still passes; add a unit test for
  `sync_badge_text` covering both modes.

## Verification (the PM does these)

1. `cargo check --workspace --all-targets` zero warnings; `cargo test -p pimble-app
   --release`; `cd web && trunk build --release` (the web build still compiles with the
   new commands and events).
2. Against the local four-process stack (`crates/pimble-cloud/README.md`): Account... >
   sign in with a signed-up account; the status bar reads `Signed in as`; restart the app
   and it still does. A wrong password shows the service's error in the modal.
3. Right-click a local store > Host on Pimble Cloud... > Host: the badge reads
   `encrypted · synced` within a few seconds; `pimble-cli cloud-list-hosted` (with
   `PIMBLE_SERVER=http://127.0.0.1:7462`) lists it as `kind=vault`; the hosted server's
   directory holds only `vault/` blobs for it.
4. A second app instance (temp `XDG_CONFIG_HOME`/`XDG_DATA_HOME`, another port), signed
   in as the same account: Account > Add Hosted Store... lists that store; Add; it opens
   with the same nodes; an edit in one appears in the other.
5. Sign Out: the status bar text goes; Host on Pimble Cloud... opens the Account modal
   with the hint.
