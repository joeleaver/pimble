# Node document contract: every node is one co-authored document, and sharing on top of it

Status: approved by Joe 2026-09-18 with the six decisions answered (section 6); wave 1 may
start. Written 2026-09-17. Replaces the sharing
design of `docs/SHARING_CONTRACT.md` (cut 1 was built on `cloud/phase-2b` and rejected the
same day: recipients could edit note text but not the tree, because the share's tree was a
one-way projection of the owner's). The questions in "Decisions for Joe" are questions.

## The tenet (now a rule in `CLAUDE.md`)

Everything is co-editable. Every piece of a person's data lives in a CRDT document that
everyone with edit rights edits directly, and edits merge on their own: a node's text, its
place in the tree, its title and metadata, a plugin's JSON. No projections, mirrors, op
logs, copies kept in step by someone's device, or paths where one party's device
interprets another's edits. A feature that seems to need one has the wrong design.

## Why the store document has to change

Today the tree is one yrs document per store (`store.yrs`: every node's title, metadata,
parent and children) and each node's text is its own yrs document (`nodes/{id}.yrs`). Text
is co-authored across a share because it is the same document on both sides. The tree
cannot be, because giving a recipient the store document gives them every title in the
store, and any second document derived from it is by nature one-way. So the tree has to be
stored the way text already is: **per node**. Then a shared subtree is the same set of
documents for everyone who holds its key, and there is nothing to project.

## 1. The node document

One yrs `Doc` per node, `nodes/{id}.yrs`, UTF-16 offsets, with these named roots:

| Root | Type | Owner | Holds |
| --- | --- | --- | --- |
| `content` | Array | rinch-editor-collab | the rich text, as today |
| `meta` | Map | rinch-editor-collab | its format tag (`format`), as today; Pimble writes nothing here |
| `node` | Map | Pimble | `node_type`, `title`, `parent_id` (absent on the root), `created_at`, `modified_at`, `deleted_at` (absent unless deleted), `tags` (Array of String), `custom` (Map of String: JSON text, as today's `custom`) |
| `children` | Array | Pimble | child node ids, in order |
| `data` | Map | a plugin | reserved now, used later: arbitrary co-edited JSON for plugin node types, as nested yrs Maps, Arrays and Texts so two people can edit one field at once |

rinch-editor-collab tolerates roots it does not own (`CollabDoc::load` checks only its own),
and the app never writes `node`/`children`/`data` through the editor: those go through
the server's tree RPCs, which edit the same `Doc`. A remote update to `node` reaching an
open editor through `collab_receive` changes nothing the editor projects.

`pimble_crdt::NodeDoc` replaces `ContentDoc` (same content methods: `from_plain_text`,
`from_blocks`, `text`, `units`, `save`, `load`, `state_vector`, `diff_since`,
`apply_update`, `merge_if_changed`) and gains the structural ones (`info() -> NodeInfo`,
`set_title`, `set_tags`, `set_custom`, `remove_custom`, `set_node_type`, `set_parent`,
`children`, `insert_child(at, id)`, `remove_child(id)`, `mark_deleted`, `touch_modified`,
`data()` for the plugin root). A `StoreDocument` no longer exists; store-level facts
(name, root id, kind) stay in `manifest.json`, which is not co-edited (a store's name is
visible metadata and the accounts service holds it for hosted stores; renaming a store is
`updateStore`, not a CRDT edit).

## 2. The tree over documents

`pimble_crdt::Tree` is a set of `NodeDoc`s with the operations the store document had
(`add_node`, `move_node`, `remove_node`, `get_children`, `get_node_info`, `list_node_ids`,
`validate_tree`, `repair`). A tree operation edits the documents it touches, each in its
own transaction, and reports which:

- create `X` under `P` at `i`: `X` (new doc: `node`, `parent_id = P`) and `P` (`children`).
- move `X` from `P` to `Q` at `i`: `P.children` (remove), `Q.children` (insert),
  `X.node.parent_id`.
- rename, retag, appearance, type: `X.node`.
- delete `X` and its subtree: for each node, `node.deleted_at = now` (a tombstone; the
  document stays, its content is not read back), and `P.children` (remove `X`).

**Effective parent and repair** are decision 9 of `docs/history/HARDENING_CONTRACT.md`,
unchanged, over the documents a device holds: a node's effective parent is its `parent_id`
when that names a held, undeleted document other than itself, else the root; cycles are
broken at the smallest id; every children list is made to hold exactly the undeleted nodes
whose effective parent is its owner, first occurrence kept, missing ones appended in id
order. Repair runs at open and after every merged update that touched `node` or
`children`. A deletion-only repair keeps the clock rule from cut 1 (it rewrites a
`modified_at` with its own value in the same transaction). (Not needed: that rule
served the mirror's causal guard, which no longer exists; plain sync links reconcile
deletions through `diff_if_peer_lacks_it`, which reads delete sets. Dropped.)

**Concurrency** is what it is today, spread over documents: a move is three updates that
can arrive in any order, so a peer can briefly see a node in two lists or none; `parent_id`
is authoritative and repair settles it. The scripted two-document scenarios of decision 9
(a node moved to two parents, A under B and B under A, delete a folder while adding to it,
delete while moving) are ported to `Tree` and must converge within two rounds as before.

**Tombstones** make deletion converge (a document whose file is simply gone would come
back from any peer that still holds it) and give a trash for free later: `deleted_at` is
last-writer-wins, "undelete" clears it and re-adds the node to its parent's list. A
tombstoned document is never indexed, listed or shown; its bytes stay on disk until a
later "empty trash" removes the file everywhere (every replica does it for itself once
the tombstone is older than N days; a deleted document that never comes back is the
CRDT's business, a purge is housekeeping).

**Notifications** are derived by the server from what an update changed: `NodeCreated`,
`NodeDeleted`, `NodeMoved`, `MetadataUpdated`, `ContentUpdated` keep their meaning and
their parent ids; `TreeStructure { node_ids }` names the documents a merged batch or a
repair touched. `applyStoreUpdate` and `syncStoreDocument` go away.

## 3. Storage and migration

- `<store>/manifest.json` (as today, `version` bumped), `<store>/nodes/{id}.yrs` (the node
  document), no `store.yrs`. `sync.json`, `vault-link.json`, `index/` as today.
- **Migration** at open, once per replica, when `store.yrs` exists: for every entry in it,
  load `nodes/{id}.yrs` (or make an empty content document for a node that never had
  content) and write the `node` and `children` roots from the entry; the root node gets no
  `parent_id`; then rename `store.yrs` to `store.yrs.migrated` and never read it again. A
  document that already has a `node` root is left alone (idempotent). Two replicas that
  migrate the same store independently write the same values with different yrs client
  ids: titles and fields merge to the same value, children lists come out with each
  replica's run of entries one after the other, and the first repair keeps the first
  occurrences, so the trees converge; this is the ordinary concurrent-edit case and needs
  no fixed client id (a fixed client id would make two replicas that migrated *different*
  states produce structs with the same id and different content, which yrs cannot merge;
  never do that). A migrated store's hosted twin keeps its old `tree` vault document,
  which nobody reads any more; the migrated node documents reach it as ordinary updates.
- Joe's stores from before are migrated, not re-imported: this is a change of layout, not
  of format.

## 4. Sync: one shape for everything

- **Plain sync links** reconcile documents only: `syncNodes { store_id, known: [{node_id,
  state_vector}] } -> { diffs: [{node_id, diff, state_vector}], unknown: [{node_id,
  state}] }` (the remote answers with what the caller lacks of every document it named, and
  the whole state of every document the caller did not name), then the caller pushes back
  what the remote lacks (`diff_if_peer_lacks_it` per document, as today for content). Live:
  `applyEdit { store_id, node_id, update }` carries any update to a node's document,
  content or structure; the server merges, persists (the flush debounce as today), relays,
  and derives the notifications. `createNode`, `moveNode`, `deleteNode`,
  `updateNodeMetadata` stay as the thin clients' way of making tree edits; the server makes
  the document edits and broadcasts them as `applyEdit` would have.
- **Vault**: `VaultDocId` is a node id; the `tree` document is gone. The vault link and the
  web vault client hold node documents and build the tree from them (the manifest's root
  id is where the tree starts). `vault_log.rs` is unchanged.
- **Search**: the indexer reads `title` and `text` from the node document (`units` covers
  both).
- **Mounts**: unchanged in shape (a mount is a node whose `node.custom` holds the
  `MountRef`; `getChildren` on it resolves the source store's root document).

## 5. Sharing on node documents

A share is a grant with a scope: `(user, store, root node, role)`. The shared documents
live in exactly one place on Pimble Cloud, the owner's hosted store `S`, and nowhere else.

- **Token**: `stores: { S: "editor" }` stays for a whole-store grant; shares are
  `stores: { S: { roots: { "<R1>": "reader", "<R2>": "editor" } } }`, **a role per shared
  root** (Joe, 2026-09-21: one role per store would have made a reader of one folder and
  editor of another the lesser of the two on both). A whole-store grant on the same store
  wins over any share. An older Pimble server cannot parse the object and drops the grant
  (fails closed). A document in two of a member's scopes takes the wider role.
  *(Decision 1 below.)*
- **A share has a name of its own** (Joe, 2026-09-21): the owner types it in the Share
  dialog, the accounts service keeps it on the scoped grant and the invitation, and it is
  what a recipient's store list shows ("<share name>, shared by <email>"). The owner's
  store name never reaches a recipient. Pimble Cloud sees the share's name, as it sees a
  store's; the marker in the node's document carries it too.
- **Scope sets on the hosted server**: `<store>/scopes.json` maps each scope root to the
  set of document ids under it. It is authorization metadata, not data: the owner's
  devices publish it (`setScope { store_id, root, docs }`, owner role) whenever the subtree
  under `R` changes on their side, and the server extends it by itself when a scoped
  member creates a document (an `applyEdit`/`vaultAppend` for an id the store does not
  have yet, whose request names the parent, is allowed when the parent is in the member's
  scope, and the new id joins that scope). A scoped principal may read and write exactly
  the documents in its roots' sets; `vaultListDocs`, `syncNodes` and every notification
  are filtered to them, so a recipient never learns another document's id. Moving a node
  out of a share is the owner removing it from the set; the server refuses a scoped
  member's write that would move a node to a parent outside the scope, because that
  parent is not theirs to edit.
- **The scope set and the tree stay in sync by construction, and drift heals** (Joe's
  question on decision 1). Only two kinds of edit change which documents are under `R`:
  an owner's move, create or delete (owner clients, desktop and web alike, publish the set
  in the same breath as the tree edit, from their own view of the subtree), and a scoped
  member's create (the server extends the set itself, from the parent named in the
  request). A scoped member's move stays inside the scope (both parents are in the set)
  and a scoped member's delete is a tombstone (the document stays in the set). So no edit
  can leave the set stale except transiently, while an owner client's tree edit and its
  set publish are in flight, or while two owner clients disagree about the subtree
  because their trees have not converged yet; both are seconds, and every owner client
  reconciles the set against its converged subtree at every connect and after every
  merged update that touched the subtree, last publisher wins. A document that is in the
  set but not (yet) under `R` in a recipient's tree is unreachable to them and harmless; a
  document under `R` but not yet in the set is a fetch that fails and is retried on the
  next set publish. Nothing in it needs an owner device for anyone's *edit* to reach
  anyone; it needs one for the owner's own *move into or out of* the share to take
  effect for recipients, which is the owner's edit anyway.
- **Keys**: every node document has its own random data key (DEK), and the blob header's
  key id names it. The DEK is stored on the hosted server beside the document, wrapped
  under each scope key that may read it: the store key, and the share key of each share
  the node is under (`pimble_crypto::wrap_dek/unwrap_dek`: XChaCha20-Poly1305 under the
  scope key, aad `"{store}/{doc}/dek"`). Sharing a node wraps the DEKs of every document
  under it under the share key: any device with the store key does it, at share time and
  as nodes enter the subtree; a recipient that creates a document makes its DEK and
  wraps it under the share key, and the owner's devices wrap it under the store key when
  they see it. Nested and overlapping shares are just more wraps. A node leaving a share
  gets a fresh DEK for what comes after (the recipients keep what they already had, as in
  any sharing system; the server stops them fetching more). Blobs from phase 2a whose key
  id names the store key itself keep decrypting: "look the key id up among the document's
  wrapped DEKs, then among the scope keys this device holds". *(Decision 2.)*
- **Nothing is hosted unless the person asked for it** (Joe, decision 3 and again on
  2026-09-18: "we really shouldn't host anything unless it's specifically been asked to be
  hosted"; now a rule in `CLAUDE.md`). Only "Host on Pimble Cloud..." uploads a store, and
  sharing never does. Where `S`'s scoped documents are served from is the store's tier:
  *hosted*, Pimble Cloud holds the ciphertext and serves it; *relay*, Pimble Cloud holds
  nothing and the owner's local server serves the same documents through a reverse tunnel
  (section 5b). Recipients' clients do not know which: `POST /api/v1/token` names the
  endpoint per store, as the web app already expects. Until the relay wave lands,
  "Share..." on an unhosted store says that sharing needs the store hosted or the relay,
  and does nothing.
- **The recipient's replica** (desktop) is a partial replica of `S`: the store's id, only
  the documents in scope, `manifest.scope_roots = [R, ...]` and `Store.roots`. The
  explorer shows each scope root under the store row (`Store.root_node_id` stays the first
  root for older callers). Mounting the shared node into one's own store is the existing
  mount of `(S, R)`, resolved through the partial replica. The web vault client holds the
  same documents and shows the same roots.
- **Roles**: owner, editor, reader, as decided. An editor with a scope edits everything in
  the scope: text, titles, structure, JSON. A reader reads. `StoreAccess::Content` is
  removed; `Read` stays; `Full` means "unscoped or everything in scope".
- **What the owner's side keeps doing**: the key handover (wrap the share key to each new
  member; wrap DEKs as nodes enter), publishing the scope set, and hosting. None of it is
  in the path of anyone's edit reaching anyone else: two recipients editing the same
  shared folder see each other live with every owner device switched off, and so does the
  owner's web app.
### 5b. The relay tier

Pimble Cloud stores nothing for a relayed store, not even ciphertext, and keeps no queue.
The owner's local server, when signed in and told a store is shared by relay, opens one
outbound WebSocket to `wss://pimble.app/relay` authenticated with its account token and
announces the store ids it serves; the relay records "store `S` is behind this
connection" in memory only. A recipient's token names `wss://pimble.app/relay/<store>` as
the store's `rpc_url`; the relay verifies their token as the hosted server does, then
pipes their JSON-RPC connection to the owner's server, which sees an ordinary
authenticated connection with a scoped principal and answers with the same handler code
the hosted server runs: scopes, DEK wraps, `vaultListDocs/Fetch/Append/Snapshot`. The
owner's server keeps, beside its plain local store, a per-document ciphertext log for the
shared documents (`<store>/relay/{doc}/log`, DEK-encrypted; derived from its own
documents like the search index is, disposable, rebuilt from a snapshot when missing), so
a late-joining recipient fetches from a sequence number as it would from the hosted
server, and a recipient's append is decrypted and merged into the plain document at once.
When the owner's server is off, recipients read their cache and their appends wait in
their own clients (`unsent` in the web client, the vault link's dirty set on desktops);
two recipients do not see each other until the owner's server is back. That is the tier's
promise and its price, both stated in the Share dialog. The relay drops a connection whose
token expires and never buffers a byte across a reconnect.

## 6. Decisions for Joe

Answered 2026-09-18: 1 yes (and the sync question is answered in section 5); 2 yes; 3 no
hosting of an unhosted store, the relay serves it (section 5b); 4 yes; 5 yes; 6 agreed.

1. The token's scoped-grant shape (a role per root since 2026-09-21: `stores: { S: { roots: { R: role } } }`), and that the hosted
   server holds the scope sets published by the owner's devices and extended on create.
2. Per-document data keys wrapped under scope keys (against re-encrypting a document under
   the share key when it enters a share, which stalls recipients on blobs they cannot read
   and needs a key hierarchy for nested shares).
3. Sharing from an unhosted store: Joe, "not supposed to be hosted, that's what the relay
   is for". Section 5b is the relay; nothing of an unhosted store is uploaded.
4. Deletion as a tombstone in the node document (against deleting the file, which does not
   converge), with trash and undelete as a later feature and purge as housekeeping.
5. Migration of existing stores at open, idempotent per replica, as in section 3 (against
   "does not open, re-import").
6. `cloud/phase-2b` is not merged as is. What survives is listed below; the rest is
   removed on a new branch started from `master`, cherry-picking the surviving commits.

## 7. What survives from cut 1, what goes

Survives (with edits): the accounts service (invitations, members with key status,
signers, sharing mails, `deleteVaultStore`; a share becomes a scoped grant on the owner's
store instead of a store of its own, so `POST /stores { share }` goes and `PUT members`
takes a `root`), `vault_log.rs`, the key sweep, the desktop Share dialog and badge
(members and roles unchanged; "can edit text" becomes "can edit"), the store row's
"shared by" and the reader refusals, the web recipient plumbing and its two bug fixes
(the tree's root from the documents, subscriptions restored on every connection),
`PIMBLE_APP_ADDR`, `PimbleServer::stop` stopping links, `StoreAccess::Read`,
`ShareMarker` on the shared node (it names the scope; recipients see it as the shared
root's own metadata, which is fine).

Goes: `pimble_crdt::share_mirror` and its tests, the projection half of `share_link.rs`,
`StoreAccess::Content`, the `contributor` role (already reverted), `VaultDocId::Tree`,
`StoreDocument`, `store.yrs`, `applyStoreUpdate`, `syncStoreDocument`, "the tree is
managed by its owner" and everything that greys out tree actions for an editor.

## 8. Work, in waves (no agent starts before Joe approves this document)

1. **`pimble-crdt`**: `NodeDoc` (content methods moved from `ContentDoc`, the `node`,
   `children`, `data` roots), `Tree` over `NodeDoc`s with the decision-9 repair and the
   ported concurrency scenarios, tombstones, a randomized convergence test over N replicas
   exchanging per-document updates in random order (per sender in order).
2. **`pimble-store` + `pimble-server` + `pimble-client` + `pimble-cli`**: the layout,
   migration, `syncNodes`, `applyEdit` carrying structure, derived notifications, tree
   RPCs over documents, the vault without `tree`, sync links and vault links over
   documents only, scope sets and scoped authorization, DEK wraps in the vault API, the
   partial vault link, partial replicas with `scope_roots`; every existing server test
   ported.
3. **`pimble-cloud`**: scoped grants, the token shape, `PUT members { email, role, root }`,
   share = scope, and the desktop's `cloudShare*` bodies against it.
4. **`pimble-app` + `web/`**: tree from node documents in the web vault client, multiple
   scope roots per store row, editors edit everything, plugin `data` untouched for now.
5. **The relay** (`pimble-cloud` or a small `pimble-relay` service on jkbase, the owner
   server's tunnel client, the token's per-store `rpc_url`, the owner server's ciphertext
   logs, the Share dialog's tier wording).
6. **PM**: the migration of Joe's real stores on a copy first; headless, GUI and browser
   verification with two recipients editing the same shared folder with every owner device
   off (hosted), and with the owner's server on and off (relay); production deploy with a
   desktop release.

## 9. Verification (the bar)

Two recipients (an editor in the web app, an editor on a desktop) and the owner's web app
rename, move, create and delete inside one shared folder at the same time, with every
desktop of the owner's switched off; all three trees converge and validate clean, and the
owner's desktops converge to the same tree when switched on. A node moved out of the share
disappears for recipients and its later edits never reach them. A reader reads and cannot
write. A migrated copy of Joe's family store opens, indexes, syncs to its hosted twin and
its replica, and every node is where it was. Nothing on the hosted disk is readable.
