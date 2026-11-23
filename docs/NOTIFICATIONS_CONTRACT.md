# Notifications contract: hearing that someone wrote to you

Status: **design agreed 2026-11-23; wave 0 (mentions as text) and wave 1 (the inbox and
the running app) built the same day**, see "Built in wave 0" and "Built in wave 1" at the
end. Written from a conversation with Joe that
day; his decisions are listed at the end. The details not named there are design choices
made here that he has not reviewed yet.

Will asked for it on 11/19: "Figure out how to make Mister Pimble alert you when you have
a message" (the Bear's board, "Feature: tell me when someone writes to me").

Builds on docs/DISCUSSIONS_CONTRACT.md (section 8, unread and the personal document;
"Later", @mentions), docs/PRESENCE_CONTRACT.md (section 3, the name you go by in each
share; agents), docs/CRYPTO_CONTRACT.md (account keys, sealed envelopes),
docs/NODE_DOCUMENT_CONTRACT.md (vault documents, `Reach`) and docs/RELAY_CONTRACT.md.

## The idea

A notification is **unread state plus a way of hearing about it**. Pimble already has the
first half for discussions: per-person read state in the personal document, which follows
you across your devices. This contract widens what can become unread and adds the second
half, including **when Pimble is not running** (Joe, 2026-11-23: "we have to have an
out-of-band mechanism for notifications for this to work right").

What can become unread, in the order it is built:

1. **A mention of you**, in a discussion or in a note's text.
2. **A reply in a discussion you took part in** (started it, or wrote in it): an implicit
   watch, nothing to set up.
3. **A change to a note you watch** (explicit "Watch", optionally its subtree).

Pimble Cloud never reads a word of any of it.

## 1. Mentions

- **Who can be mentioned: the people who can open the note.** Pimble has no global
  handles and no directory (Joe, 2026-11-23, agreed). Typing `@` in a note offers the
  members of the share the note is in, under the name each goes by there
  (`GET /stores/{id}/members`, presence section 3); members never see each other's
  emails. A person outside the share cannot be mentioned: the picker offers "Share this
  with someone..." instead (the existing invite by email). In a note nobody else can
  open, `@` offers nobody.
- **The same person may have different names in different shares**, and each note shows
  the name from its own share (Joe, 2026-11-23: "that's right").
- **A mention is an inline atom** in the text, like a node link: `account` (the
  account's `sub`, stable) and `name` (the name when it was written). It shows the
  person's current name from the member list, so a rename follows everywhere; `name` is
  what Markdown export, search and an offline device show. A mention of someone who has
  left the share is drawn greyed under its old name and notifies nobody.
- **The same atom in discussions and in notes.** In a discussion it means "answer me"; in
  a note "look at this spot". Both notify, and both open at the mention.
- **Detected by the writer's device, once**: an atom naming account A that appeared in an
  edit made on this device, where A is not the writer. Undo and redo of the same atom do
  not notify again (the atom carries an id).
- Markdown: `[@Will](pimble-person:<sub>)`. The MCP tools write the same, so agents can
  mention people.
- **Supersedes** DISCUSSIONS_CONTRACT "Later": "nothing is sent anywhere" is no longer
  true of mentions (section 2).

## 2. The inbox: notices sealed by the sender

Only the writer's device knows a mention happened, because only it has the plaintext. So
it tells the mentioned person directly:

```text
Notice {                         // sealed to the recipient's notice key (below)
  id, kind: Mention | Reply,
  from: sub, from_name,          // the name the sender goes by in that share
  store, node, discussion?, anchor,
  snippet,                       // about 120 characters around the mention
  at,
}
```

- **The notice key.** Every account has an X25519 key pair used for notices and nothing
  else. Its public half is published beside the account's other public keys (`GET
  /users/lookup`, and with each member in `GET /stores/{id}/members`). Its private half
  is wrapped under the account key like the store keys; each device unwraps it once and
  keeps it: the desktop in `keys.json`, a browser as a non-extractable WebCrypto key in
  IndexedDB, so that the web app's service worker can open a notice without the account
  being unlocked (section 4, web push). A stolen device can read the notices it was sent,
  never a note. Rotated with "Sign out everywhere".
- The device seals the notice to the recipient's notice key and posts it to the accounts
  service: `POST /api/v1/inbox/{recipient sub}`, with the share (store and root) in the
  clear. The service checks that the sender and the recipient are both in that share (no
  inbox spam from strangers), rate-limits per sender, queues it, and wakes the
  recipient's channels (section 4).
- **What the service learns:** that `from` notified `to` at `at`, about something in a
  share they are both in. Not which note, not what was said. It already knows both of
  their names in that share and the share's name.
- A reply in a discussion notifies its other participants the same way (the writer's
  device knows who wrote in the thread). That covers the implicit watch of section 3 for
  discussions without any server-side watching.
- An offline sender sends when it reconnects; a notice older than a day is dropped
  rather than delivered late.

## 3. Watches: evaluated by the server, blind

A watched note cannot be told by its writers (they do not know who watches it, and watch
lists are private). On a hosted store the server already sees every append: which
encrypted document, which account, when. So:

- **Your device registers opaque document ids** (`PUT /api/v1/me/watches`: store, doc ids)
  and the service puts a notice in your inbox when **an account other than yours** appends
  to one of them. Your own other devices never notify you, with no per-edit authorship
  needed in the document.
- **Batched**: someone typing sends hundreds of appends. Appends to watched documents are
  gathered per (watcher, document, writer) and one notice is queued after five quiet
  minutes: "3 changes in a note you watch". The notice has no title or text; the
  recipient's device fills those in when it opens it.
- **What the service learns:** which documents you watch. New metadata, no content.
- Watching a note may include its subtree; the device keeps the registered set in step
  as nodes come and go (it knows the tree; the server does not).
- **Nothing is watched by default** (Joe, 2026-11-23), not even a share someone shared
  with you: until you choose Watch, you hear about mentions and replies only.
- **Relayed stores** (Joe, 2026-11-23): the owner's machine runs the same evaluation on
  its relay face and posts the notices, so watches work only while that machine is on.
  Nothing is missed by it: while the owner is away nobody edits through the relay, and
  edits made offline reach the relay, and the watchers, when it is back.

## 4. Delivery: every channel reads the same inbox

- **Pimble running** (desktop and browser): drains the inbox at connect and when nudged
  over its existing connection, decrypts, and shows an OS notification with the snippet
  (rinch's `notifications` feature on the desktop; the Notifications API in the
  browser). Clicking it opens the note at the mention.
- **The tray** (desktop; Joe, 2026-11-23: on by default): closing the last window leaves
  Pimble running in the system tray, so notifications arrive while you work elsewhere.
  The tray menu has Open Pimble and Quit (File > Quit quits too), and a setting ("Keep
  running in the tray when the window closes") turns it off. The first time a window
  closes into the tray, a notification says so once.
- **Web push** (browser and phone, through the web app's service worker; Joe,
  2026-11-23: the full notice): the push payload is the sealed notice itself, inside Web
  Push's own encryption to the browser (RFC 8291), so neither the push service nor
  Pimble Cloud can read it. The service worker opens it with the notice key from
  IndexedDB and shows it whole: "Joe: @Will can you check the…". Tapping it opens the web
  app at the mention (unlocking first if needed). A watch notice, which the service
  cannot fill in, shows "3 changes in a note you watch, in Shared with Will".
- **Email, the fallback** (Joe, 2026-11-23: who and which share): an item no device has
  acknowledged within 15 minutes is emailed through Resend (`m.pimble.app`), at most one
  email per hour per person: "Joe mentioned you in Shared with Will", or for several,
  one line each, and a link. Only what the service already knows (the names people go by
  in the share, the share's name); never a word of a note. Devices acknowledge items as
  they drain them.
- **Read anywhere clears everywhere**: opening a notice writes its id into the personal
  document (DISCUSSIONS section 8), which syncs to the person's other devices; the
  service deletes delivered items after seven days.

## 5. The inbox in the app

View > "Discussions..." becomes **"Inbox..."**: mentions, replies and watched changes,
newest first, unread on top. The status bar shows a count. Each row opens where it
points and has "Unwatch" for watched changes.

## 6. Turning it off

Signing in turns notifications on (Decisions, the hosting rule); the Account modal says
so in one line ("People who share notes with you can notify you; Pimble Cloud cannot read
what they say") beside its switch.

Per person, in Settings and in the Account modal: mentions, replies, watched notes, each
on or off; per channel: OS notifications, web push, email. "Unwatch" from any watched
notice. Turning everything off deletes the registered watches and stops queuing.

## 7. Agents

An agent writing through the MCP tools is a writer like any other: its mentions notify,
and its appends count for watches under the account it acts for. Joe's Robots writing in
Will's note is exactly what Will should hear about.

## 8. Waves

0. **Mentions as text**: the atom, the `@` picker over a share's members, Markdown and
   MCP. No delivery yet; the person's unread view lifts mentions of them.
1. **The inbox and the running app**: the notice key, sealed notices for mentions and
   replies, the accounts service's inbox, draining on connect, OS notifications, View > "Inbox...".
2. **Out of band**: acknowledgements, email after 15 minutes, web push.
3. **Watches**: registration, the server's batching, "Watch" and "Unwatch", subtrees,
   the relay face.
4. **The tray** on the desktop, on by default.

## Decisions (Joe, 2026-11-23)

- Notifications need an out-of-band path: hearing only when Pimble is next opened is
  not enough.
- No global handles. You mention the people who can open the note, by the name they go
  by in that share; the same person can have different names in different shares.
- Mentions first; mentions in discussions are the clearest case, and mentions in notes
  use the same atom.
- **Signing in implies it** (the hosting rule). The sealed inbox and the list of watched
  document ids are the person's own account data, as read state is: signing in to Pimble
  Cloud turns them on, for hosted, relayed and unhosted stores alike, with a switch in
  the Account modal and in Settings (section 6). Turning it off deletes the registered
  watches and the queued notices and stops both. No store content is ever uploaded for
  it: a notice is sealed to the recipient, and a watch names opaque document ids.
- **Email says who and which share**, and nothing of the note: "Joe mentioned you in
  Shared with Will" and a link (section 4).
- **Web push shows the full notice**, sealed end to end and opened by the service worker
  with a per-account notice key kept on each device (sections 2 and 4).
- **Watches on relayed stores** work while the owner's machine is on (section 3).
- **Nothing is watched by default**; shares shared with you included (section 3).
- **The tray is on by default** on the desktop, with a setting to turn it off (section 4).

## Built in wave 0 (2026-11-23, branch `notifications`)

### The representation: a link, not a new inline atom

Section 1 asked for an inline atom. **A mention is rinch's `link` mark** over the words
`@Name` (the name as written), whose href is `pimble-person:<sub>?m=<id>`. Why:

- rinch's collaboration (`rinch-editor-collab` at `e8e04cc`) carries any schema's inline
  atom (`is_inline_atom` is a predicate on the node type), but the schema is rinch's
  `Schema::starter_kit()`, built in rinch and used as is by the editor, `pimble-crdt`,
  Markdown and the browser. Its only inline atoms are `image` and `hard_break`, and the
  only app-defined node is the block `embed`. A `mention` node would need rinch to take
  an app's inline node type (a general mechanism: an inline counterpart of `embed`,
  `data-*` attributes, an app-drawn view), and every peer to run that build before anyone
  writes one: an older peer's session is poisoned by an atom type it does not know
  (rinch #196).
- A link mark needs nothing upstream and no coordinated upgrade: an older build, Markdown
  export, search, the MCP and another app all read it as `@Will`, a link.
- The mention's id rides in the href (`?m=`), so undo, redo, copy and a Markdown round
  trip all keep it. The link's `title` attribute would have held it too, but Markdown
  writes a title as `"..."` after the URL, which an agent rewriting a note is likelier to
  drop.

What the link cannot do: **the note's own words keep the name as written.** rinch has
no decoration that replaces text (a `Widget` is the placeholder only), so in a note a
rename shows in the tooltip ("A mention of William (written as Will)"), and the words
change when someone retypes them. In a discussion message, which the app draws itself,
the current name shows. Greying works everywhere (an `Inline` decoration with a class).
If Joe wants the current name in the text too, the general rinch mechanism is either an
app inline atom (above) or a decoration that draws a widget in place of a range.

### What was built, and where

- `pimble_core::mention` (`crates/pimble-core/src/mention.rs`): `Mention { account, id,
  name }`, the href (`href`, `parse_href`, `is_mention_href`, `new_id`: 12 hex digits),
  and **`added(before, after, writer)`, the detection hook**: the mentions in `after`
  not in `before`, by id (an id-less mention by account and ordinal), never the writer's
  own account, a pasted copy (same id) once. Undo then redo is "added" again by design;
  the notifier keeps the ids it has sent (wave 1).
- `pimble_crdt::mentions` (`crates/pimble-crdt/src/mentions.rs`): `of_model` (every
  mention in an editor model, a run split by other marks one mention), `added_between`
  (two models), `with_ids` (a mention written without an id gets one).
  `NodeDoc::mentions()` and **`NodeDoc::mentions_added_by(update, writer)`**: the mentions
  a yrs update would add, without changing the document. Wave 1 calls it on the updates
  this device's own edits make (the outbound path, `doc_session::send`).
- Markdown (`crates/pimble-crdt/src/markdown.rs`): `[@Will](pimble-person:<sub>)` in,
  written out with its `?m=`; a mention written without an id is given one in `strict`
  (`mentions::with_ids`); a `pimble-person:` href naming no account is refused with a
  sentence. rinch's Markdown already carries the link both ways.
- Discussion messages (`crates/pimble-discussion/src/body.rs`): the same href as a link
  span; `Body::mentions()`, ids given in `from_markdown`, and
  `Body::with_picked_mentions` (a name picked in the message box becomes a mention when
  the message is sent). Detection for a message is `mention::added(&old.mentions(),
  &new.mentions(), writer)`.
- `StorePeople::members_of(chain)` (`crates/pimble-rpc/src/types.rs`): the people who
  can open a node, from the store's people (`cloudPeople`, the member list the app
  already caches per store) and the node's ancestry: every share rooted at the node or
  above it plus the whole store's, each person once by the name of the innermost share.
- The app (`crates/pimble-app/src/mentions.rs`, `link_picker.rs`, `links.rs`):
  - Typing `@` at the start of a word (not in inline code; an email address is left
    alone) opens the link picker's new `PickerKind::Mention`: the people who can open the
    note, never this account, filtered by name (a word of it first). A pick replaces
    `@query` with the linked `@Name` and a space, one undo step. Words nobody is called
    close the picker, so `@home` is just typed. With nobody to offer it says "Only you
    can open this note" and, on the desktop in one's own store, offers "Share this with
    someone..." (the existing Share dialog), which Enter takes only when the arrows chose
    it, so `@` then Enter is a new line.
  - `MentionsPlugin` (one per pane): an `Inline` decoration `pimble-mention` over each
    mention, plus `pimble-mention--gone` when the store's people are known and the
    person is not among those who can open the note. The people reach it through a
    meta transaction from an effect per pane (`follow_members`, on the pane's note and
    the people); unknown people never grey anything.
  - The link tooltip of a mention says the current name, or that the person can no
    longer open the note, and drops "Ctrl+click to open" (`follow_href` ignores the
    scheme, so nothing opens).
  - Discussions: a message shows a mention as `@` and the current name (greyed when the
    person has left); the message box offers people for the `@` it ends in (chips under
    it; a click, Tab or Enter picks the first) and links the picked names on send.
- MCP (`crates/pimble-mcp/src/tools.rs`, `docs/MCP_CONTRACT.md`): the instructions
  describe mentions where links are described, and say to keep a mention's href; a new
  tool **`list_people`** names who can open a node, each with the Markdown that
  mentions them (an agent has no other way to learn a `sub`).

### Verified

- Unit tests: `pimble-core` (mention hrefs, `added`), `pimble-crdt` (reading mentions,
  split runs, ids given by Markdown and kept by a round trip, `added_between`,
  `NodeDoc::mentions_added_by` on real updates), `pimble-discussion` (body mentions, ids
  kept by editing, picked names), `pimble-rpc` (`members_of` with nested and whole-store
  shares), `pimble-app` (who `@` offers, the picker's rows and pick, stepping aside,
  the nobody case and its Enter, the query, undo/redo keeping the id, the plugin's
  classes, the message box's `@`).
- Runs: `pimble-crdt` lib 222 passed; `pimble-core`, `pimble-discussion`, `pimble-rpc` lib
  all passed; `pimble-app` lib filtered to `link_picker mentions links:: hashtags
  discussions` 31 passed; `pimble-mcp` 53 passed.
- The browser build checks (`--features web --target wasm32-unknown-unknown`).
- Not seen in a running app: a store with members needs the cloud stack, so the picker
  was tested against a fake member list.

### Not done in wave 0

- The person's own unread view does not lift mentions of them (optional; nothing reads
  "unread" for notes yet, and discussions' unread counts are untouched).
- The note's words do not follow a rename (above).
- A pasted mention keeps its id, so pasting a mention of Will into another note is not
  news to Will in wave 1. Re-minting ids on paste is a small paste-hook change if wanted.
- Nothing calls the detection hook yet; wave 1 does, from the outbound edit path and
  from posting or editing a message.
- Editing a message shows its mentions as Markdown (`[@Will](pimble-person:...)`), as it
  already shows links.

## Built in wave 1 (2026-11-23, branch `notifications`)

### Decided here (small; Joe has not reviewed them)

- **The notice key's envelope is the account's own `KeyEnvelope`**: the secret sealed to
  the account's X25519 key and signed by its Ed25519 key, context `notice:<sub>`, key id
  nil (`pimble_crypto::wrap_notice_key`/`unwrap_notice_key`). Unwrapping checks the
  signature, the context and that the secret matches the published public half, so a
  substituted envelope is refused. A second, different key is refused by the service
  (409) and the device that lost the race fetches the first.
- **A sealed notice** (`pimble_crypto::seal_notice`): version byte, an ephemeral X25519
  key, a 24-byte nonce, XChaCha20-Poly1305 under HKDF-SHA256 (`"pimble-notice"`, salted
  with both public keys), associated data `pimble-notice/<recipient sub>`. Unsigned: the
  accounts service stamps the sender it authenticated beside the item, and the
  recipient's server drops a notice whose own `from` differs.
- **`anchor`** is the mention's id for a mention in a note's text and the message's id
  for a discussion; **`node`** is the note, or a discussion's host (the discussion itself
  when standalone), and **`discussion`** names the discussion.
- **Which share addresses a notice**: the innermost share of the node's ancestry (a
  discussion stands under its host) whose people name the recipient, else the whole
  store's; `from_name` is what the sender goes by there (`ScopePeople.my_name`).
- **Replies**: "the thread" is the whole discussion. A new message notifies everyone else
  who wrote a message in it (agents' messages notify nobody, being nobody's inbox); a
  person the message mentions gets the mention instead. An edited message notifies only
  the mentions it added.
- **What was notified** is remembered per `(store, node or discussion, mention id or
  message id, recipient)` for 180 days (at most 20,000), in the account's own file. Undo
  and redo, a restart, a re-sent edit: nobody again. The same mention pasted into another
  note: notified (its key names the node). The wave 0 note that "pasting a mention of
  Will into another note is not news to Will" is therefore superseded.
- **The inbox API** (accounts service, `crates/pimble-cloud/src/routes/inbox.rs`):
  `POST /api/v1/inbox/{sub} { store_id, root?, sealed }` (both accounts must reach that
  share: the whole store, or that root; 60 per minute per sender; a recipient who turned
  notifications off is answered `queued: false` and nothing is kept),
  `GET /api/v1/inbox?after=<seq>&wait=<s>` (the long poll, at most 25 s: the items after
  the cursor, `seq` being the row id), `POST /api/v1/inbox/ack { ids }` (deleted at once),
  `DELETE /api/v1/inbox`, `PUT /api/v1/me/notice-key`, `PUT /api/v1/me/notifications
  { on }`. Undelivered items older than a day are dropped, delivered ones seven days after
  their first delivery (checked whenever the recipient drains). `GET /me` answers
  `notifications`; `GET /me/keys` answers `notice_key { public_key, envelope }`;
  `GET /users/lookup` and every person and member of `GET /stores/{id}/members` carry
  `notice_key` (to every member: it is a public key).
- **The long poll, not SSE**: the desktop's HTTP client has a 15 s timeout, so the server
  asks with `wait=10`; each run starts from cursor 0 and takes each item once by id.
- **Acknowledged = deleted.** Every device of the account drains every unacknowledged
  item, so each shows it; the first one opened acknowledges it, and the personal document
  clears it on the others.
- **The switch is the account's**, kept by the accounts service (`User.notifications_off`)
  and read by every device's server at the start of each run. The Account modal is
  Settings > Account now, so the switch and its line live there once.

### What was built, and where

- `pimble-crypto`: `NoticeKey`, `wrap_notice_key`, `unwrap_notice_key`,
  `notice_key_context`, `seal_notice`, `open_notice`.
- `pimble-cloud`: `schema.rhype` (`User.notice_public_key`, `notice_key_envelope`,
  `notifications_off`; `type InboxItem`), `db.rs`, `routes/inbox.rs`, the fields above on
  `me`, `me/keys`, `users/lookup` and members, the inbox emptied with a deleted account,
  a waker per recipient (`AppState::wake_inbox`).
- `pimble-rpc`: `Notice`, `NoticeKind`, `listNotices`, `acknowledgeNotices`,
  `setNotifications`, `subscribeNotices` (all `Service`-only), `Person.notice_key`;
  `pimble-client` wrappers.
- `pimble-server`:
  - `keystore.rs`: the notice key with the account (`notice_key`, `set_notice_key`);
    saves are now one at a time (two at once, a store key and the notice key, lost the
    shared temporary file: found by the end-to-end test).
  - `notices.rs`: `candidates` (the notices a write calls for, from the document before
    and after), the per-account file `<data dir>/pimble/notices/<sub>.json` (what was
    told, the outbox, what arrived, acknowledgements owed), `ensure_notice_key` (fetch, or
    make and publish: existing accounts get one from their next signed-in device), and
    the run while signed in and on: the outbox (sealed and posted, retried every 30 s,
    dropped after a day or when the recipient cannot open the note) beside the inbox's
    long poll.
  - `handler.rs`: `applyEdit` (and `createNode`/`createAttachment` with content) hands
    the write over when it is this device's (`Principal::Service` or an agent's
    connection, never a `sync-link:`/`vault-link:` client, never a member's own token) and
    could matter (the update carries `pimble-person:`, or the document is a discussion).
    The detection runs off the request's path. Started with the server (not on a relay
    face or a JWT-verifying server), again at sign-in, sign-out and the switch.
  - The browser's server is the same code in its worker, so the browser sends and
    receives exactly as the desktop does.
- `pimble-discussion`: the personal document's `notices` root (`notice_read`,
  `mark_notice_read`; forgotten after two months).
- `pimble-app` (`inbox.rs`, both targets): `NoticeReceived`/`NoticesListed`, an OS
  notification per new unread notice (rinch `notifications` on the desktop, the
  browser's Notifications API, permission asked when the Inbox opens or the switch is
  turned on), View > "Inbox..." (mentions and replies, unread first, then the unread
  discussions as before; "Mark All Read"), a status bar count, the switch in Settings >
  Account. Opening a notice marks it read in the personal document, acknowledges it and
  opens the note at the mention (a deep link's quote: the snippet from the `@`) or the
  discussion.

### Verified

- `pimble-crypto` 33 passed (two new: sealing, the envelope).
- `pimble-cloud` against a real `rhypedb-server`:
  `notices_go_between_members_of_a_share_only_sealed_to_the_notice_key` (the key's rules,
  publication, a long poll woken by a post, opening, cursor, acknowledgement, outsiders
  and other shares refused, the switch) and `a_sender_is_rate_limited`.
- `pimble-server` lib: `notices` (5) and `keystore` (7) passed.
- `pimble-server/tests/notices.rs` on the sharing harness (its stub gained the inbox,
  notice keys and the switch): `a_mention_reaches_the_member_once_sealed_and_opened_by_their_server`
  (Alice mentions Bob; Bob's server opens it; nothing of the note on the service;
  undo/redo, the edit reaching Bob over his link and a restart of Alice's server notify
  nobody again; the same mention in another note does; acknowledged; a mention of someone
  outside the share tells nobody; switched off, nothing goes) and
  `a_reply_in_a_discussion_tells_the_others_who_wrote_in_it`.
- `pimble-server` `share` (20), `share_upkeep`, `startup`, `test_isolation`, `vault_link`
  (15) all passed with the notices running beside them.
- `pimble-discussion` `personal` (4, one new); `pimble-app` lib filtered to `inbox personal
  discussions mentions menus events::`, 53 passed; the browser builds
  (`--features web --target wasm32-unknown-unknown`, `web/`, `pimble-worker`).
- Not seen in a running app: the OS notification, the Inbox window and the status bar
  were not driven by hand on the local stack.

### Not done in wave 1

- The browser's notice key rests inside the server's keystore, which rests sealed under
  the remembered device's non-extractable WebCrypto key in IndexedDB
  (docs/ONE_CODEBASE_CONTRACT.md section 7). A key a service worker can open on its own
  (section 2) is wave 2's, with web push.
- A notice opens a discussion, not scrolled to the message (`anchor` names it).
- No per-kind or per-channel switches (section 6): one switch for the account.
- Out of band (email, push, the 15-minute acknowledgement rule) is wave 2.
- Seen while testing, not investigated: a discussion the **owner** started on a note in a
  share did not reach the member's replica within 30 seconds in the harness (one a member
  started reaches the owner at once, as `share.rs`'s commenter test shows), so the reply
  test has the member start the thread.

## Built in wave 2 (2026-11-23, branch `notifications`)

### Decided here (small; Joe has not reviewed them)

- **Seen and read.** An item is *seen* once a running device drained it (wave 1's
  `delivered`) or a browser's service worker showed its push, and *read* once a device
  acknowledged it (deleted). Email goes only for an item neither seen nor read, so "no
  device acknowledged within 15 minutes" reads as "no device so much as showed it": a
  notice that popped up on a running desktop is not emailed again a quarter of an hour
  later. Read on one device, an item is gone, so it stops its email and its push
  everywhere; email and push read the same rows and agree on what is outstanding.
- **A shown push says so without a session**: the push carries
  `{public_url}/api/v1/inbox/seen/{item}?t=<HMAC>` (HMAC-SHA256 under the service's
  existing server secret, over `pimble-inbox-seen:<item>`); `POST` to it marks that item
  seen and nothing else (403 on a wrong token). The service worker has no session.
- **The email**: one pass a minute (`notify::email_pass(state, now_ms)`, the clock the
  caller's, so tests move it). Due items are those created at least 15 minutes ago, not
  seen, not yet emailed; at most one email per person per hour (`User.notice_emailed_at`),
  and items due inside that hour wait for the next email rather than being lost. Lines
  that say the same thing are merged with a count ("Joe mentioned you in Shared with Will
  (3 times)"), at most 20 lines and "and N more". The link is `{public_url}/app/?inbox`,
  which opens the Inbox. An item whose switch was off when it fell due is marked emailed,
  so turning email back on never sends old news. Undelivered items older than a day are
  dropped by the pass as a drain drops them.
- **The words**: "X mentioned you in S", "X replied in a discussion in S", from the name
  X goes by in that share (the share's `ShareName`, else their default name) and the
  share's name as the recipient's own grant names it. To say mention or reply, the sender
  now posts `kind` beside the sealed notice (`POST /inbox/{sub} { ..., kind }`,
  sanitized to a short lowercase word). **That is a small metadata leak**: the service
  learns whether a notice is a mention or a reply, which it could not before. The
  alternative was "X wrote to you in S" for both.
- **The push payload** is `{ v: 1, id, from, to, kind, line, seen, sealed }`, inside RFC
  8291 (`aes128gcm`), with VAPID (RFC 8292, ES256), `TTL` one day and `Urgency: high`.
  `line` is the email's line, shown when the sealed notice cannot be opened; `sealed` is
  dropped when the payload would pass 3,800 bytes (push services take about 4 KB). A
  subscription a push service answers 404 or 410 is deleted.
- **Subscriptions**: `POST /api/v1/me/push { endpoint, p256dh, auth }` (upsert by
  endpoint, at most 20 per account), `DELETE /api/v1/me/push { endpoint }`. The endpoint
  must be one of the browsers' push services (`fcm.googleapis.com`,
  `push.services.mozilla.com`, `push.apple.com`, `notify.windows.com`, over https on
  443), so nobody can make the service post to an address of their choosing.
  `GET /me` answers `notification_email`, `notification_push` and `push_key` (the VAPID
  public key, absent when push is not configured). Deleting an account deletes its
  subscriptions.
- **The switches are the account's**, like wave 1's: `PUT /me/notifications { on?,
  email?, push? }` (`User.notify_email_off`, `notify_push_off`), shown in Settings >
  Account on both targets ("Email me what I miss"; "Notify this browser when it is
  closed" in a browser, "Notify my browsers when Pimble is closed" on the desktop). Push
  off stops pushing to every browser of the account. The RPCs are `setNotifications {
  on?, email?, push? }`, `registerPush`, `unregisterPush`; `listNotices` answers the
  channels and the push key.
- **The service worker's notice key**: the browser's own server, when the account is
  remembered on this browser, writes the notice key's X25519 secret into IndexedDB
  (`pimble-device`, `keys`, `notice` = `{ sub, key, public }`) as a **non-extractable**
  WebCrypto key (PKCS#8 import, `deriveBits` only), so script can derive with it and never
  read it. Sign-out and a sign-in as someone else delete it. A browser that does not
  remember the account, or has no X25519 in WebCrypto, shows `line`. The worker opens the
  sealed notice with `web/build/notice.js` (XChaCha20-Poly1305 written out in JavaScript,
  WebCrypto for X25519 and HKDF), concatenated in front of `sw.js` by `post_build.sh`,
  and shows it only when its `from` is the sender the service stamped, as the server
  checks.
- **A click** focuses an open Pimble tab and tells it (`postMessage`, heard by hub.js as
  the window event `pimble-open-notice`), or opens `/app/?notice=<id>` (`?inbox` for a
  push it could not open); the app opens that notice once the inbox holds it, else the
  Inbox.
- **Sign-out unsubscribes**: the server remembers the endpoints this browser registered
  and deletes them at sign-out (and at a sign-in as another account) before the session
  ends.

### What was built, and where

- `pimble-cloud`: `schema.rhype` (`User.notify_email_off`, `notify_push_off`,
  `notice_emailed_at`; `InboxItem.kind`, `emailed`; `type PushSubscription`), `db.rs`,
  `webpush.rs` (VAPID keys and headers, RFC 8291 encryption, `--generate-vapid-key`),
  `notify.rs` (the endpoint allowlist, the seen token, `push_item`, `email_pass` and its
  job, started by `build_router`), `mail.rs` (`NoticeLine`, `notices_email`),
  `routes/inbox.rs` (`post_push`, `delete_push`, `seen`, `names_for`, `kind`, the
  channels on the switch), `routes/accounts.rs` (the channels and `push_key` on `me`),
  `config.rs` (`PIMBLE_VAPID_PRIVATE_KEY`, `PIMBLE_VAPID_SUBJECT`).
- `pimble-rpc`, `pimble-client`: the RPCs above. `pimble-server`: `cloud.rs` (the HTTP
  calls), `notices.rs` (channels kept with the run, `register_push`, `unregister_push`,
  `signing_out`, `kind` on the outbox), `keystore.rs` (`publish_notice_key`, resting the
  key through `KeyRest::notice_key`), `handler.rs`. `pimble-host`: `KeyRest::notice_key`
  (a default that does nothing: the desktop keeps no such key). `pimble-worker`
  `device.rs`: the browser's implementation.
- `pimble-app`: the two switches and their line, `inbox::channels`, `browser_push`
  (subscribing through `PimbleHub.pushSubscribe` with the VAPID key, unsubscribing at
  sign-out, hearing `?notice=`, `?inbox` and the click event). `web/`: `hub.js`
  (`pushSubscribe`, `pushUnsubscribe`, the click relay), `build/notice.js`,
  `build/sw.template.js` (`push`, `notificationclick`), `build/post_build.sh`,
  `src/app_page.rs` and `src/worker_mode/` (hear clicks; unsubscribe on leaving).

### Verified

- `pimble-cloud` whole suite: lib 17 passed, integration 88 passed against a real
  `rhypedb-server` (none skipped). New in lib: the notice email's words, the RFC 8291
  example decrypting, a push opening with its subscription's keys and no others, a VAPID
  header verifying, the endpoint allowlist).
- The new integration tests:
  `an_unseen_notice_is_emailed_after_fifteen_minutes_at_most_hourly` (nothing at 14
  minutes, one email at 15 naming the sender and share and no note text, nothing more
  inside the hour, the item that fell due inside it in the next hour's email; merging
  alike lines is in the code but not under a test),
  `a_seen_or_read_notice_is_not_emailed` (a drained item, an acknowledged one, one a
  push marked seen; a wrong seen token is 403), `the_email_switch_stops_the_email` (and
  switched back on, old items stay unsent), `a_queued_notice_is_pushed_encrypted_to_the_subscription`
  (bad keys refused, subscribing again replaces; a stand-in push service receives one
  `aes128gcm` body that opens with the subscription's keys to the payload above, with
  the sealed notice inside; the VAPID header's claims; the `seen` link stops the email;
  push off and unsubscribing send nothing; a 410 deletes the subscription). The endpoint
  allowlist is a unit test (the integration run allows a loopback push service).
- `pimble-server/tests/notices.rs` (2): the stub service records `kind` ("mention"),
  the channels round-trip, `registerPush`/`unregisterPush`, and Bob's sign-out deletes
  his subscriptions. `pimble-server` lib `notices keystore` 12 passed.
- `web/build/notice.test.mjs` (node): RFC 8439's Poly1305 vector, the XChaCha draft's
  HChaCha20 vector, and a notice `pimble_crypto::seal_notice` sealed opening with the key
  imported exactly as the worker imports it; another account, a tampered byte and another
  key are refused. The assembled `sw.js` parses.
- Builds: `pimble-app` native (release), `--features web` for wasm32, `web/` for wasm32,
  `pimble-worker` (with `entry`) for wasm32. `pimble-app` lib `inbox menus events::` 47
  passed.
- **Not seen in a real browser**: no push went through a real push service (FCM,
  Mozilla) and no service worker showed a notification; the headless walk-through on the
  local stack was not done. Real push services need a real VAPID key and an https origin
  (or localhost) with the permission granted.

### Not done in wave 2

- Watches (wave 3), so a watch notice's line ("3 changes in a note you watch") is not
  produced yet; the worker shows whatever `line` says.
- Per-kind switches (mentions, replies, watched notes) and per-device push switches: the
  switches are the account's.
- The desktop has no push (it has the running app and, in wave 4, the tray).
- Safari's push service needs the page installed to the home screen on iOS; not tried.
- The email has no unsubscribe link or `List-Unsubscribe` header; the switch is in
  Settings.
