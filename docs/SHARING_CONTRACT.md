# Sharing contract, phase 2b: share a node in place, end to end encrypted

Status: SUPERSEDED. Cut 1 was built and verified on `cloud/phase-2b` on 2026-09-17 and
rejected by Joe the same evening: a share whose tree only the owner's devices write is not
co-authored, and everything must be (`CLAUDE.md`, "Everything is co-editable"). The
replacement is `docs/NODE_DOCUMENT_CONTRACT.md` (every node is one document; sharing is a
scoped grant on the same documents). Kept for what survives (section 7 there) and as the
record of the mirror design and why it was wrong. Builds on `docs/CLOUD_CONTRACT.md`
("Phase 2: sharing", decided with Joe 2026-09-15) and `docs/CRYPTO_CONTRACT.md` (phase 2a).
Signatures below are fixed. Deviations are reported, not improvised.

**Assumptions Joe has not overruled** (each is a PM decision with its reason; say so and
the contract changes):

1. **A share is its own small vault store, the "share mirror"**, not a subtree grant on the
   owner's store. The reasons are under "Why a mirror". The owner's store is untouched:
   nothing moves, nothing is restructured.
2. **This cut: recipients read, and editors edit node content. The tree of a share is
   written by the owner's side only** (create, rename, move, delete, appearance). A
   recipient's structural edits and the "anyone with the link" share are the next cut
   ("Cut 2" at the end); nothing here has to be redone for them.
3. Sharing needs the owner signed in on a desktop, not the store hosted. A hosted store and
   a purely local one share the same way.
4. The share's display name is typed by the owner in the Share dialog (default: the node's
   title) and is visible to Pimble Cloud and in the invitation mail, like a store's name.

## Goal

Joe right-clicks a node, picks "Share...", names the share, adds `ann@example.com` as an
editor and `bob@example.com` as a reader. Ann has an account: within seconds the share is in
her web app and in her desktop's "Add Hosted Store..." list; she opens a note and types, and
Joe sees the words arrive in his editor, in place, in his own tree. Bob has no account: he
gets a mail, signs up, and the share opens once one of Joe's desktops has been online to
hand him the key. Joe renames a note or drags a new one into the shared folder and both see
it. Pimble Cloud holds ciphertext under a key it never sees, and neither Ann nor Bob can
decrypt or even fetch anything of Joe's outside the shared node.

## Why a mirror

The tree of a store is one yrs document encrypted under the store key, so a recipient can
never be given it: a diff of it carries every title in the store. Whatever else is decided,
the recipient's view of the subtree's structure has to be a second document under a second
key, kept up to date by devices that hold both: the "share mirror" of the 2026-09-15 build
order. Given that, putting the shared nodes' content beside it, in the same second vault,
is what makes everything else fall out:

- **The hosted server does not change.** A share is a vault store with its own id, its own
  key and ordinary `(user, store, role)` grants. No per-member node-id lists, no
  ancestor walks over a tree the server cannot read, no new roles or JWT claims.
- **Recipients reuse phase 2a whole.** The web app opens a share as it opens any vault
  store. The desktop adds it with "Add Hosted Store..." and links it with `VaultLink`; the
  replica is a store, so mounting it somewhere in one's own tree is the existing mount.
- **A node never changes keys.** Moving a node into or out of a share adds it to or removes
  it from the mirror; no document is re-encrypted, and no recipient stalls on a blob under
  a key they do not hold.
- **Nested and overlapping shares are independent mirrors.** No key hierarchy.
- **The relay tier is the same shape**: the owner's server presents the mirror itself
  instead of the hosted server storing it (`docs/CRYPTO_CONTRACT.md`, "Endpoint-agnostic").

What it costs: a shared node's ciphertext exists twice on Pimble Cloud when the owner's
store is hosted too, and changes cross between the owner's store and the mirror only while
one of the owner's desktops is running (recipients still read, and edit among themselves,
while the owner is away). The web app as a bridge is a follow-up; the projection is pure
Rust and builds for wasm so that it can be.

## The model

```
owner's store S (plain, local)            share vault H (kind vault, id = share id)
  ...                                        tree  = mirror StoreDocument, root = R
  R  "Recipes"  custom.share = marker  <->   R, a, b  = the same ContentDocs, same NodeIds
    a                                        every blob under the share key,
    b                                        aad "{share id}/{doc id}"
  ...
        ShareLink (owner's desktops)         VaultLink (recipient desktops) / web vault client
```

- **Share id** is a fresh `StoreId`. **Share key**: a fresh `SymmetricKey` with a fresh key
  id, envelopes with context `"share:<share id>"`, stored in the keystore under
  `(share id, key id)` like any store key.
- **Marker**: the shared node carries `metadata.custom["share"]`
  (`pimble_core::custom_keys::SHARE`, `pimble_core::ShareMarker`), written through
  `updateNodeMetadata` like any metadata. It replicates with the store, which is how the
  owner's other devices learn of the share and how the tree shows the badge.
- **Content** is the same yrs document on both sides, so bridging it is one more hop of the
  chain that sync links already are: any number of owner devices may bridge at once, a
  repeated update merges to nothing.
- **The mirror tree** is a projection: a deterministic function of the owner's subtree.
  It is written only by the owner's side. Recipients never append to `tree`.

### Projection rules (`pimble_crdt::share_mirror`, agent M)

- Mirrored nodes: `R` and every descendant, except nodes of type `mount` (a mount names
  other stores and local paths) .
- Mirrored per node: `node_type`, `title`, `tags`, `custom` minus
  `MIRROR_EXCLUDED_CUSTOM_KEYS` (`share`), `created_at` (when the node is added), the
  parent, and the children in the owner's order. **Not `modified_at`**: every keystroke
  moves it in the owner's document, and a recipient's replica keeps its own as content
  arrives.
- In the mirror `R` has no parent and `meta.root_node_id = R`; `meta.name` is the share's
  name.
- **Surgical edits only**: add what is missing, remove what is extra, set what differs, and
  fix a children list by removing extra indices (keeping the first of a duplicated id) and
  inserting missing ones. Never clear and rebuild: two bridges projecting the same state
  must write nothing the second time.
- **Causal guard**: every changing projection records the owner document's state vector in
  the mirror (`meta.share_owner_sv`, base64). A bridge whose owner document does not cover
  the recorded vector is behind the last projector and answers `Projection::Behind` without
  writing. This is what stops a device that was offline from projecting last week's titles.
- A bridge re-runs the projection (debounced one second) when the owner's tree changes and
  when another bridge's mirror update arrives; an unchanged projection writes nothing, so
  two bridges cannot fight.
- **A deletion must move a clock.** yrs deletions move no state vector, so a copy that
  lacked only a deletion would pass the guard and put the node back. Every `StoreDocument`
  mutation that can be deletion-only therefore also stamps a `modified_at` in the same
  transaction (`remove_node` stamps the parent's): the update that carries the deletion
  carries a clock, and a copy without it is `Behind` (M's finding, 2026-09-17).

### Access on the recipient's side

`pimble_core::StoreAccess { Full, Content, Read }` on `Store` (serde default `Full`):

| | tree RPCs | content RPCs |
| --- | --- | --- |
| `Full` (every store until now) | yes | yes |
| `Content` (a share, role editor) | refused | yes |
| `Read` (role reader, share or whole store) | refused | refused |

Refusal is JSON-RPC `-32004` whose message is the sentence alone
(`pimble_core::StoreAccess::{TREE_REFUSAL, CONTENT_REFUSAL}`: "The structure of a shared
folder is managed by its owner." / "You can read this, not change it."), which the UI
shows as it is. A link's own applies (client id `vault-link:*`, `share-link:*`,
`sync-link:*`) are never refused. This keeps an honest client from edits that would go
nowhere. The hosted server's role check (reader or editor, as decided) is the enforcement
for content. It does not stop a share's editor with a patched client from appending to the
mirror's `tree`: see the known limits below for what that can and cannot do.

### The one security rule of the bridge

A `ShareLink` applies an incoming content blob to the owner's store **only if the node is
under `R` in the owner's document at that moment**. Anything else is logged and dropped.
Without this an editor could write any node of the owner's store by appending to
`vault/{that node id}` in the share.

## Visible to Pimble Cloud (added to the crypto contract's list)

That a vault store is a share and who its members and invitees are; the share's display
name; the node ids inside it (as document ids). Not the owner's store id, not the shared
node's place in it, no title other than the chosen name.

Visible to a share's recipients beyond the subtree: the client ids and operation counts of
the owner's store document (the state vector the causal guard records in the mirror), which
say how much has been written in the whole store and by how many devices, and nothing
about what.

Known limits of this cut, stated plainly: an editor of a share can vandalise it, as in any
sharing system, and wiping the notes is the worst of it (that reaches the owner's store,
because content is bridged both ways). Writing the mirror's `tree` with a patched client is
lesser: scribbled structure is seen by other recipients only and is rewritten by the next
projection, and a guard vector no owner copy covers freezes the share's structure updates
until the owner stops sharing and shares again; neither touches the owner's store, since no
bridge reads structure back. A fourth role to forbid it was written and withdrawn the same
day (Joe, 2026-09-17: not worth a role; if the freeze ever matters, the Share dialog can
report a share that stays behind while its store is in sync). Two bridges recording the guard vector at the
same moment keep one of the two (a single last-writer-wins key), and it can be the older
one, which costs one stale projection round before the next projection heals it; a removed member keeps the share key and what they
already synced, and their token stays good for up to an hour (no key rotation, no live
connection drop yet); the ciphertext of a node moved or deleted out of a share stays in the
share vault until "Stop sharing", which deletes the vault from the hosted disk.

## Interfaces landed by the PM (skeleton, commit 1)

`pimble-core`:

```rust
pub enum StoreAccess { Full, Content, Read }            // serde snake_case, Default = Full
impl StoreAccess { pub fn allows_tree(self) -> bool; pub fn allows_content(self) -> bool; }
pub struct Store { /* … */ pub access: StoreAccess, pub shared_by: Option<String> }  // both serde(default)

pub mod custom_keys { pub const SHARE: &str = "share"; }
pub struct ShareMarker { pub v: u8, pub share_id: StoreId, pub key_id: Uuid, pub url: String, pub name: String }
impl NodeMetadata { pub fn share(&self) -> Option<ShareMarker>; pub fn set_share(&mut self, marker: Option<&ShareMarker>); }
```

`pimble-crdt` (`share_mirror.rs`, bodies by M):

```rust
pub const MIRROR_EXCLUDED_CUSTOM_KEYS: &[&str] = &["share"];
pub const MIRROR_EXCLUDED_NODE_TYPES: &[&str] = &["mount"];
pub enum Projection { Behind, Unchanged, Changed }
pub fn subtree_ids(owner: &StoreDocument, root: NodeId) -> Result<Vec<NodeId>>;   // preorder, child order, root first
pub fn project_subtree(owner: &StoreDocument, root: NodeId, name: &str, mirror: &mut StoreDocument) -> Result<Projection>;
pub fn state_vector_covers(local: &[u8], other: &[u8]) -> Result<bool>;          // every clock of other <= local
```

`project_subtree` on an empty mirror builds it. `root` absent from `owner` is an error.

`pimble-rpc`, all Service-only:

| Method | Behaviour |
| --- | --- |
| `deleteVaultStore { store_id }` | hosted side: close a `vault` store and delete its directory; refused for a plain store |
| `cloudShareNode { store_id, node_id, name }` → `CloudShareInfoResponse` | create the share vault through the accounts service, key, self envelope, marker, start the `ShareLink` |
| `cloudShareInfo { store_id, node_id }` → `CloudShareInfoResponse` | `{ share: ShareInfo, members: Vec<ShareMember> }` from the accounts service |
| `cloudShareInvite { store_id, node_id, email, role }` → `CloudShareInfoResponse` | `PUT members`; when the answer carries public keys, wrap and upload the envelope at once |
| `cloudShareRemoveMember { store_id, node_id, email }` → `CloudShareInfoResponse` | a member or an invitation |
| `cloudStopSharing { store_id, node_id }` (`CloudShareRef`, as `cloudShareInfo`) | stop the link, delete the share store, remove the marker, the key and `<store>/shares/<share id>/` |

```rust
pub enum MemberRole { Owner, Editor, Reader }                      // serde lowercase
pub enum ShareMemberStatus { Invited, WaitingForKey, Active }      // serde snake_case
pub struct ShareMember { pub email: String, pub role: MemberRole, pub status: ShareMemberStatus }
pub struct ShareInfo { pub share_id: StoreId, pub store_id: StoreId, pub node_id: NodeId, pub name: String, pub state: SyncState }
StoreChangeKind::ShareStateChanged { node_id: NodeId, state: SyncState }   // on the owner's store; links never forward it
CloudHostedStoreInfo { /* … */ pub share: bool, pub shared_by: Option<String> }   // serde(default)
GetStoreSyncResponse { /* … */ pub access: StoreAccess }                           // serde(default)
```

`pimble-app` `protocol.rs`: `BackendCommand::{CloudShareNode, CloudShareInfo, CloudShareInvite,
CloudShareRemoveMember, CloudStopSharing}`, `BackendEvent::{CloudShareUpdated { store_id,
node_id, share, members }, CloudSharingStopped { store_id, node_id }}`, `CloudOp::{Share,
ShareInfo, ShareInvite, ShareRemoveMember, StopSharing}`, wired through `commands.rs` to
the client by the PM.

## Accounts service (agent C, `crates/pimble-cloud`)

Schema: `HostedStore.share: Bool` (absent on old rows: false). New type `Invitation { store,
store_rid @indexed, store_uuid, email, email_lower @indexed, role, invited_by_rid,
created_at }`, one per (store, email_lower), enforced in the service. Every row read keeps
treating later fields as optional (the rule from the v10 incident).

| Method and path | Auth | Behaviour |
| --- | --- | --- |
| `POST /stores { name, kind, store_id?, share? }` | session | `share: true` needs `kind: vault` |
| `GET /stores` | session | `StoreView` gains `share: bool` and `shared_by: Option<String>` (an owner's email, when the caller is not an owner) |
| `GET /stores/{id}/members` | any grant | `[{ user_id?, email, role, status: "active" \| "invited", has_key, public_keys? }]`. `has_key`: a `KeyGrant` exists for that user and store. Invitations and `public_keys` only when the caller is an owner |
| `PUT /stores/{id}/members { email, role }` | owner | a verified account: grant (as today) and a "shared with you" mail, answer `status: "active"` with `public_keys`. Otherwise: upsert an `Invitation`, send the invitation mail, answer `status: "invited"`. Role `owner` by invitation is refused |
| `DELETE /stores/{id}/members/{user_id}` | owner, or the member themself | also deletes that user's `KeyGrant`s for the store; the last-owner rule stands |
| `DELETE /stores/{id}/invitations/{email}` | owner | 200 also when there was none |
| `GET /stores/{id}/keys` | any grant | adds `signers: [{ user_id, email, public_signing_key }]`, the store's owners |
| `DELETE /stores/{id}` | owner | also deletes key grants and invitations, and for a vault store calls `deleteVaultStore` on the hosted server (a failure is logged; the row is still marked deleted) |

Claiming: when an address becomes verified (`verify`) and at every `login`, that address's
invitations become grants (one already there wins) and are deleted. Mail: `invitation_email`
(link `<public url>/app/signup?email=<address>`, subject "<inviter> invited you to
"<name>" on Pimble") and `shared_with_you_email` (link `<public url>/app/`), both saying
that the notes are end to end encrypted and that the sender's Pimble has to come online
once before they open; one mail per (store, address) per minute, thirty invitations per
inviter per hour, fifty members plus invitations per store. `pimble.rs` gains
`delete_vault_store`. Tests against a real rhypedb-server as today: invite an unknown
address then sign up and verify and find the grant; invite a known one; `has_key` before
and after an envelope; `signers`; self removal; invitation removal; limits; store deletion
reaching the hosted server; rows without `share` read as false.

## Servers, wave 1 (agent B)

Owns `crates/pimble-store`, the existing files of `crates/pimble-server` (`handler.rs`,
`vault_link.rs`, `cloud.rs`, `keystore.rs`), `crates/pimble-client`, and B's CLI commands.

1. `deleteVaultStore`: `StoreManager` closes and removes a vault store's directory (only a
   `vault` kind, only a directory that is a store this server holds open); client wrapper;
   CLI `delete-vault-store`; tests in `tests/vault.rs` (gone from `listStores`, directory
   gone, plain store refused, a user principal refused).
2. `SyncConfig.access: StoreAccess` (serde default `Full`), filled into `Store.access`,
   `Store.shared_by` and `GetStoreSyncResponse.access` wherever the server returns them.
3. Enforcement, one helper `require_access(store_id, Needed::Tree | Needed::Content,
   client_id)` at the top of every RPC that writes: tree for `createNode`, `deleteNode`,
   `moveNode`, `updateNodeMetadata`, `updateNode`, `applyStoreUpdate`, `createMount` and
   the like; content for `applyEdit` and `updateNodeContent`. Through a mount the check is
   against the source store. A client id starting `vault-link:`, `share-link:` or
   `sync-link:` passes. Tests for each row of the access table.
4. `VaultLink` on a store whose access is not `Full` never pushes `tree` (not live, not in
   the reconcile, no tree snapshot), and with `Read` pushes nothing at all. On every
   connect it re-reads the account's `GET /stores` row for this store and rewrites
   `sync.json` when the role changed (editor to reader takes effect at the next connect).
5. `cloud.rs`: `StoreView` gains `share`, `shared_by`; `KeyGrantsResponse` gains `signers`.
   `cloudListHostedStores` passes them through and leaves out shares the account owns
   (they are mirrors of its own nodes). `cloudAddHostedStore` accepts an envelope signed by
   the account itself or by a listed signer, sets `access` from the row (share and editor:
   `Content`; reader: `Read`; else `Full`) and `shared_by`, and answers a clear error when
   the account has a grant but no envelope yet ("waiting for <shared_by>'s Pimble to come
   online to finish sharing").

## Desktop bridge, wave 2 (agent E, after B and M are committed)

Owns all of `crates/pimble-server` once B's work is committed: the new `share_link.rs`,
`share.rs` (the PM's stubs: the `cloudShare*` RPCs already delegate to `RpcHandler::
share_node`, `share_info`, `share_invite`, `share_remove_member`, `stop_sharing`, and
`ensure_share_links` waits for its call sites), the hooks in `handler.rs`, and may refactor
`vault_link.rs` to share code with the share link (cursor, progress file, echo tracking, snapshot rule:
one implementation, not two), `cloud.rs` additions (members, invitations, delete store,
create with `share`), the `cloudShare*` bodies behind the PM's stubs, CLI `cloud-share`,
`cloud-share-info`, `cloud-share-invite`, `cloud-share-remove`, `cloud-stop-sharing`, and
`tests/share_link.rs`.

- `ShareLink::start(handler, store_id, node_id, marker)`: state in
  `<store>/shares/<share id>/` (`mirror.yrs`, `link.json` with per-document cursors, known
  state vectors and dirty set, exactly the vault link's rules including
  `VaultCursor::applied_through` and the `covers_prefix` snapshot rule). Connect as the
  vault link does (mint a token, the `rpc_url` it names). Reconcile: pull `tree` into the
  local mirror, project, push the mirror's diff; then for every id in `subtree_ids` pull
  and push content as the vault link does. Live: a local tree change schedules a
  projection; a local `ContentUpdated` for a node in the subtree is encrypted and appended;
  a remote `tree` append is applied to the local mirror and schedules a projection check; a
  remote node append is applied through `apply_edit` with client id `share-link:<uuid>`
  under the security rule above. State changes notify `ShareStateChanged` on the owner's
  store.
- **Key sweep**: at connect, every 60 seconds while connected, and after `cloudShareInvite`:
  `GET members`, and for every active member without a key, wrap the share key to their
  public keys and `PUT keys`.
- `ensure_share_links(store_id)`: at `openStore`, after sign-in, and when a tree change
  touches a marker: start a link for every marker whose `url` is the signed-in account's
  and whose share this account owns (fetching and unwrapping the envelope when the keystore
  lacks the key; a 403 is remembered until the next sign-in), stop the link of a marker
  that is gone and delete its directory and key.
- `deleteNode` on a subtree holding markers stops those shares first, best effort.
- `cloudShareNode` refuses a store whose access is not `Full`, a `mount` node, and a node
  already shared.
- Tests over real servers (hosted server in JWT mode as `tests/vault_link.rs` does, with the
  cuttable relay): seed and read back through a second account's `VaultLink`; owner rename,
  create, move in, move out, delete reach the recipient; recipient content edit reaches the
  owner's store and the owner's hosted twin; an append for a node outside the subtree is
  dropped; a reader's replica refuses edits locally; two owner devices bridging at once
  converge with no duplicate children; an edit made while the share link is down is pushed
  when it is back; stop sharing removes the hosted directory.

## Apps (agents A and W, wave 2; A may start in wave 1 against the skeleton)

**A, `crates/pimble-app`:** node context menu "Share..." (disabled with a reason on a
store whose access is not `Full` and on mounts; opens the Account modal with a hint when
nothing is signed in). The Share modal: the name field and "Share" when the node is not
shared; then the member list (email, role, status in words: "invited, no account yet",
"waiting for your Pimble to hand over the key", "active"), an email field with an
Editor/Reader choice and "Invite", remove buttons, "Stop sharing" with a confirmation, and
one sentence on what Pimble Cloud can and cannot see. A share badge on a shared node's row
(the row's `TreeNodeData` label must carry it, as icon and colour do). A store with
`shared_by` shows "shared by <email>" and its access ("can edit text" / "read only") in the
row's label or tooltip. Everything that writes the tree is disabled at render time for
`Content` and `Read`, everything that writes content for `Read`; a refused command shows
the server's sentence. **Read-only editor**: rinch has no read-only switch today; A adds one
upstream (a pull request on joeleaver/rinch, branched from `fix/collab-remote-caret` while
#831 is open: local input rejected, `collab_receive` still applied) and the PM moves the
pins. Until then a refused `BroadcastChanges` reopens the node so the typed text does not
linger. "Add Hosted Store..." shows shares as "<name>, shared by <email>".

**W, `web/`:** `learn_kinds` learns `role`, `share`, `shared_by`; a listed vault store
carries `access`, `shared_by` and **the tree document's root as `root_node_id`** (today the
hosted manifest's random root reaches the UI, which is wrong for any store whose tree was
made elsewhere); shares the account owns are not listed; tree commands on a store whose
access is not `Full` and content commands on `Read` answer the same sentences without a
request, and the client never appends to `tree` or snapshots it there; envelopes are
accepted from the account itself or a listed signer; a vault store with a grant and no
envelope yet is listed as waiting (a row that says so) and retried every 30 seconds;
`/app/signup?email=` prefills the address.

## Ownership

| Who | Scope (no one edits another's files) |
| --- | --- |
| PM | this file, the skeleton above, root `Cargo.toml`, `CLAUDE.md`, `docs/NEXT_SESSION.md`, commits, verification |
| M | `crates/pimble-crdt` |
| C | `crates/pimble-cloud` |
| B | `crates/pimble-store`, existing `crates/pimble-server` files, `crates/pimble-client`, its CLI commands |
| E | `crates/pimble-server` after B is committed (`share_link.rs`, `share.rs`, hooks, `vault_link.rs`, `cloud.rs`), its CLI commands, `tests/share_link.rs` |
| A | `crates/pimble-app`, the rinch pull request |
| W | `web/` |

Rules for every agent: no `git commit`, `stash`, `checkout` or `reset`; no formatter over
files you do not own; servers you start listen on your own ports and are killed by port or
pid, never `pkill -f pimble`; `/home/joe/dev/rinch` is off limits.

## Verification

- M: unit tests per rule, and a randomized convergence test: an owner document under random
  edits, two bridges projecting from randomly lagging copies with updates delivered in
  random order, asserting that once everything is delivered the mirror equals the
  projection of the final owner state, has no duplicate child, validates clean, and that a
  lagging bridge never wrote (`Behind`).
- B, C, E: the tests listed in their sections; `cargo test --workspace --release` green,
  `cargo check --workspace --all-targets` with zero warnings.
- PM, headless: the local stack of `docs/NEXT_SESSION.md` plus two desktop servers with
  their own XDG directories, two accounts; share, invite, add, edit both ways, structural
  changes, refusals, reader, an invitee without an account, only `PB` ciphertext and no
  title on the hosted disk, stop sharing.
- PM, GUI and browser: the Share dialog end to end, the recipient in the web app and in a
  second desktop, a mount of the share in the recipient's own store.

## Cut 2 (designed for, not built here)

- **A recipient's structural edits**: an `ops` document in the share vault, an encrypted,
  server-sequenced log of intents (create, rename, move, delete, appearance) that the
  owner's bridges apply to the owner's store as the RPC handler would, the projection
  carrying the result back; the recipient shows its own pending ops optimistically. Needs
  `VaultDocId::Ops` on the hosted server and nothing else.
- **"Anyone with the link"**: `https://pimble.app/s/<token>#<share key>`; the fragment never
  reaches the server; a signed-in visitor claims the token as a grant and wraps the key to
  themself.
- Key rotation on member removal, dropping a removed subject's live connections, the web
  app as a bridge, a recipient's app learning of a new share without a reload, then the
  relay tier.
