# Notifications contract: hearing that someone wrote to you

Status: **design agreed 2026-11-23; waves 0 to 3 (mentions as text, the inbox and the
running app, email and web push, watches) built the same day**, see "Built in wave 0" to
"Built in wave 3" at the end. Written from a conversation with Joe that
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

## Built in wave 3 (2026-11-23, branch `notifications`)

### Decided here (small; Joe has not reviewed them)

- **Servers report writes, the accounts service decides who hears.** A server that
  verifies tokens (the hosted server, an owner's relay face) records each write a
  `Principal::User` makes to a node's document (`vaultAppend` of `VaultDocId::Node`,
  `applyEdit` that changed something): store, document, writer, nothing of the content,
  gathered 2 s quiet / 10 s longest per (store, document, writer) and told as
  `WriteSeen`. The accounts service follows the hosted server's `subscribeWrites` over its
  own connection; for a relayed store the owner's server hears its relay face's writes
  in-process and posts them (`POST /api/v1/watches/writes`, the store's owner only,
  relay-tier stores only).
- **The hosted server tells only watched documents.** The service names them
  (`setWriteInterest`, Service-only): per store the opaque document ids registered there
  and the roots of the shares their watchers hold the store through, whole when it starts
  following and per store as they change (documents registered, the watches switch, and
  every grant change: one made, a role or scope changed, one removed, a store deleted or
  revived, an account deleted). A store whose watched notes cannot be read is read again
  with backoff (1 s doubling to a minute), not at once, at a deadline set when it starts
  waiting: the writes that keep arriving meanwhile never put it off (until 2026-11-23 the
  follower's loop restarted the wait on every write, so under steady writing the store
  was never read again). The server tells nothing else, so the service never hears of a write to a
  note nobody watches; it learns which documents are watched, never by whom. A relay face
  is the owner's own server and tells every write; the owner's posts are not filtered
  (filtering them would tell the owner's machine which of its documents others watch).
- **A write names the roots that reach it.** Each `WriteSeen` carries the shared roots
  whose scope set contains its document (among the roots named, or every root a relay
  face's store publishes), judged once per gathering by `scope_set_of`, the same judgement
  every scoped read uses. The service gathers a write for a watcher only when they hold
  the store whole or hold a share whose root is among them, keeps that root with the
  batch, and judges again when the notice is queued (grants change in between: a share
  moved or ended tells nothing). Section 3 said the owner's machine "posts the notices": it
  cannot, as only the service knows who watches what, so it posts the writes and the
  service evaluates them exactly as for a hosted store. The operator's own writes
  (`Principal::Service`: a desktop's embedded server, the CLI) are never recorded.
- **Two things kept per person, apart.** *The choices* (what the person chose: store,
  node, with or without its subtree) are account data like the personal document: one
  blob sealed with `personal_key` under the associated data `pimble-watches/{user}`
  (`pimble_crypto::watches_aad`), versioned, written by compare-and-swap (409, then read,
  change again and retry, four times at most). *The registered documents* are the opaque
  ids the choices reach, per store, which the service matches writes against. Every put
  of a store's documents names the choices' version it was computed from, and one from
  older choices is refused (409), so two devices cannot leave a stale set behind.
- **The registered set** of a store is the chosen nodes that are live, and with a subtree
  everything live under them, less attachments and discussions (`docs_for`); recomputed
  2 s after any structural change (created, deleted, moved, tree structure, shares ended,
  sync state) and at every sign-in check, and put whole.
- **Watching is refused on a store only on this computer** ("Only notes other people can
  change are watched: this store is on this computer alone. Host it on Pimble Cloud or
  share it first."): nobody else can write there, and the service could not hear it.
- **"Unwatch" on a node** takes its own watch, or else the nearest watch with a subtree
  above it (so Unwatch on a watch notice for a note under a watched folder stops the
  folder's watch). The row menu offers Unwatch when a subtree watch in the store might
  reach the node (the explorer does not hold every ancestor); the server says "This note
  is not watched." when none does.
- **The notice**: `NoticeKind::Watch`, sealed by the service to the watcher's notice key,
  from the writer, the node changed, no snippet, `count` spells of editing: writes 30 s
  or more apart (`pimble_core::watch::SPELL_GAP_MS`) count once each, so a minute's
  typing is one, not a hundred appends. Writes are told in any order (a relay's report
  can arrive late), so a batch keeps its spells' spans (`pimble_core::watch::Spells`,
  `WatchBatch.spells`), not only its first and last write: a write is judged against the
  writes it falls between, and one that closes the gap between two spells makes them
  one; the same write told twice adds nothing. The app says "Joe edited a
  note you watch" ("... 3 times" for three spells) and shows the note's name, which only
  the watcher's device knows; email and push say "Joe edited a note you watch in Garden"
  ("... (3 times)"; several notes of one writer in one email: "Joe edited 2 notes you
  watch in Garden"). Its item's root is the scope the watcher holds the store through, so the
  share's name is the one the watcher knows.
- **The watcher must still hold the store** when a write is gathered and when its notice
  is queued, through the whole store or a share whose root reaches the document (above);
  a member who left, or whose share no longer reaches the note, hears nothing more.
- **A relayed store's writer is a member, inside their share.** The owner's report names
  writers; each must hold the store whole or a share whose root is among the roots the
  write names as reaching its document, and a write naming anyone else, or a member for
  a document outside what they hold, is refused (counted in the answer's `refused`) and
  gathers nothing.
- **Unsent relay reports are kept.** What the owner's server could not post waits in the
  store's `sync-state.json` (section `relay_writes`, written whole to a temporary and
  renamed into place), writes of one spell merged, at most 20,000 kept (the oldest go
  first), and is sent 500 at a time when the service answers again (backoff 2 s to 5
  minutes, and at once when the relay serves again). A part the service refuses as
  malformed or not the owner's (400, 403, 404) is dropped; an expired session waits for
  the next.
- **Each relay report is taken once.** A part is sealed as a report with an id of its own
  (`report`, a UUID) and kept so before it is posted; nothing is added to it after, and
  it is sent with that id until the service takes it. The service remembers the ids it
  took for 30 days (`RelayReport`, forgotten hourly) and answers one sent again
  `{ again: true }`, gathering nothing, so a report whose answer was lost (the app quit,
  the connection broke after the post) is not told twice. Reports are taken one at a
  time (`relay_reports_lock`). The report is remembered after its writes are gathered,
  so a report whose remembering failed is gathered again when it is sent again; each
  batch holds the writes of reports gathered into it, each named by its report's id and
  its place in the report (`WatchBatch.reports`, `<report>#<place>`, written in the same
  query as the write), so none of their writes is gathered twice, even one whose notice
  was queued in between, and one a failed sending did not reach is gathered when the
  report is sent again. A write, not a report, is what a batch holds: one report can name
  the same writer and document twice (two spells of editing, the second after a gap of
  `SPELL_GAP_MS` or more, as after an outage), and both are counted: a pass takes such a batch's spells and keeps the row,
  with no spells, until its reports are forgotten (30 days, hourly, with the reports). A
  batch's `spells` are read as stored and never rebuilt from its first and last write
  (that would lose its count); no batch without them exists, as `WatchBatch` never
  shipped before them.
- **Quitting loses no relay write.** At stop the relay face stops first, telling what it
  gathered and had not told yet, and then its reports are asked to stop: they keep all of
  it on disk and end, and are waited for, never aborted. A post under way is let go;
  every write is on disk before a post carries it, and the report is sent again with its
  id the next time.
- **Gathering and taking batches are one lock** (`watches_lock`): a write gathered while a
  pass takes the batches is never lost or told twice.
- **The switch**: `watches` beside `on`, `email` and `push` in `PUT /me/notifications`
  and `setNotifications`; on by default, nothing watched by default. Off (or everything
  off) deletes the registered documents and what was gathered, empties the choices and
  moves their version on (the row is kept as a tombstone, so the version never goes back
  and a device still holding older choices gets 409 and reads them again), refuses new
  ones (403) and the device forgets its copy.

### What was built, and where

- `pimble-core/src/watch.rs`: `Window` (`REPORT`, `NOTICE`), `Gathered`, `Spells`,
  `Batcher`, `SPELL_GAP_MS`, `watch_title`.
- `pimble-rpc/src/notices.rs`: `NoticeKind::Watch`, `Notice.count`, `WatchEntry`,
  `WatchNodeRequest`, `UnwatchNodeRequest`, `ListWatchesResponse`, `WriteSeen` (with
  `roots`, `last_ms`), `WriteInterest`, `SetWriteInterestRequest`,
  `SetNotificationsRequest.watches`, `ListNoticesResponse.watches`; methods `watchNode`,
  `unwatchNode`, `listWatches`, `setWriteInterest` (Service-only) and the subscription
  `subscribeWrites` (Service-only); `pimble-client` wraps them.
- `pimble-server/src/writes.rs` (`Writes`: interest, record, gather, tell, `roots_reaching`;
  nothing kept with nobody listening), `relay_writes.rs` (the owner's report of its relay
  face's writes, for the stores its tunnel serves, its `Outbox` in `sync-state.json` with
  sealed reports, `Owner` what it needs of the owner's server, `run` with its stop;
  `relay_face.rs` stops the face before its reports and waits for them),
  `handler.rs` `note_write` and the five RPCs.
- `pimble-server/src/notices/` (split from the old `notices.rs`): `watches.rs` (choices
  sealed and kept in step, `docs_for`, `register_all`, `follow_structure`, watch and
  unwatch), `kept.rs` (the device's copy), `run.rs` (refreshed with the account,
  registered after structural changes); `cloud/notices.rs` the service's endpoints.
- `pimble-cloud`: `schema.rhype` (`User.notify_watches_off`, `InboxItem.count`,
  `WatchChoices`, `WatchedDoc`, `WatchBatch` with its `root` and `spells`,
  `RelayReport`), `src/db/` (split from
  `db.rs`; `watches.rs`), `src/routes/watches.rs` (`GET /me/watches`, `PUT
  /me/watches/choices`, `PUT /me/watches/stores/{store}`, `POST /watches/writes` with the
  report id and the writer's grant judged against the write's roots), `src/watches.rs`
  (`record_writes`, `watch_pass(state, now_ms)` with the caller's clock,
  `interest_of`/`all_interest`, `InterestChanges`, `spawn`: a pass every 30 s, relay
  reports forgotten after 30 days, and the hosted server's writes followed with backoff,
  its interest sent whole on every connect and per store as it changes, an unreadable
  store read again with backoff), `mail.rs` and `notify.rs` (the watch line, spells
  summed per email line), `routes/inbox.rs` (the switch), `routes/stores.rs`,
  `routes/members.rs`, `routes/accounts.rs` (every grant change changes the store's
  interest; `routes/stores.rs` was split into `stores.rs`, `members.rs` and
  `store_keys.rs`, each under 700 lines).
- `pimble-app`: `watches.rs` (state, the row menu's states, the switch, the Inbox's
  "Watching" list with "Unwatch"), the row menu's "Watch" submenu (Watch, Watch with
  Subtree, Unwatch) in `app.rs`, the switch in the Account modal and Settings, watch
  notices in `inbox.rs` (title, the note's name as the system notification's body,
  "Unwatch" on the row), `protocol.rs`/`commands.rs`/`events.rs`. One code base for the
  desktop and the browser; `web/build/sw.template.js` titles a pushed watch notice.

### Verified

- `pimble-cloud` integration against a real `rhypedb-server` (`tests/integration/watches.rs`):
  `writes_to_a_watched_note_become_one_notice_after_five_quiet_minutes` (choices and
  documents round-trip, stale versions 409, bad ids 400; the writer's own writes and
  unwatched documents gather nothing; nothing at four quiet minutes, one notice after
  five, told once, its item's root the share; it opens with the notice key to a `Watch`
  notice with count 2 for two spells of four writes and no snippet; its email says
  "olga.watch edited a note you watch in Recipes" and not the store's own name),
  `a_scoped_watcher_hears_only_of_what_their_share_reaches` (no roots or another share's
  gather nothing; a batch gathered through a share that then moves is not told; through
  the new share it is), `the_hosted_server_is_told_only_the_watched_documents_and_their_watchers_shares`
  (documents once each, a scoped watcher's root, none for a whole-store watcher, nothing
  for a relayed store, the switch taking a watcher's root away),
  `writes_gathered_while_a_pass_takes_batches_are_all_told` (eight gathers at once make
  one batch; 30 spells gathered while passes run are all told, once),
  `watches_need_a_grant`, `the_watches_switch_deletes_the_watches_and_stops_gathering`
  (off empties documents and batches, the choices' version moves to 2 and older versions
  are 409 once back on), `only_a_relayed_stores_owner_reports_its_writes_and_only_its_members`
  (a writer never shared the store is refused; bad ids, roots and report ids 400),
  `a_relay_report_is_taken_once_and_names_a_member_only_inside_their_share` (a member
  named for a document outside his share, or through a root he does not hold, is
  refused; the same report sent again is `again: true` and gathers nothing; every
  report is remembered and forgotten when old), `a_late_write_between_two_spells_is_counted_where_it_falls`,
  `every_grant_change_tells_the_hosted_server_again` (a role changed, a whole-store
  grant, a share removed, a member removed, a store deleted). Lib (19): the watch
  lines' words, and an unreadable store waiting longer each time up to a minute. All
  of `pimble-cloud` passes (19 lib, 98 integration) after `routes/stores.rs` was split.
- `pimble-server/tests/watches.rs` (2), on the sharing harness with the stub service's
  new watch endpoints: nothing watched by default; watching a folder with its subtree
  writes sealed choices (version 1, no node id in the clear) and registers the folder
  and what is under it but not the diary; a node made under it is registered a moment
  later; once the watched documents are named with `setWriteInterest`, a member's write
  reaches the hosted server's `subscribeWrites` naming the document, the writer and only
  the share's root that reaches it, and a write to a document not named is never told;
  Unwatch on a note under the folder takes the folder's watch
  and empties the set; a store on this computer alone is refused. `relay.rs` (9) and
  `notices.rs` (2) pass beside it.
- `pimble-server` lib: `Writes` gathering per document and writer and telling once
  quiet, and at stop, only named documents, everything on a relay face; the relay
  `Outbox` keeping what the service did not take on disk and sending it later, merging a
  spell into one entry, dropping a refused part, sending many writes a part at a time,
  a report whose answer was lost sent again with its id and taken once, a post under way
  let go at stop with its writes kept, and what the face told as it stopped kept
  (`run` over a fake owner and service); `docs_for` (subtrees, discussions and deleted nodes left out);
  unwatch taking the own watch or the nearest subtree watch above.
- `pimble-app` lib (358 passed): the row menu's states (nothing watched by default),
  a watch notice's title and where it opens. `pimble-crypto`: the chosen watches do not
  open as the personal document.
- `pimble-core`: `Spells` (a late write inside a span judged by the writes around it,
  one closing a gap joining two spells, the same write twice counted once).
- Builds: `cargo check --workspace --tests`, `web/` for wasm32.
- **Not seen running**: no desktop or browser walk-through; the hosted server's
  `subscribeWrites` followed by the service's own loop and the relay face's report were
  not run end to end (each half is under test up to its HTTP or RPC edge).

### Not done in wave 3

- Writes made while the service is not following the hosted server (a deploy, a dropped
  connection) are not told; a relayed store's writes are told only while the owner's
  machine is on (kept on disk until the service takes them, above).
- The service judges a share by the roots the server reports; it does not know the tree.
  A relayed store's roots come from the owner's machine, which could name any root: the
  owner already holds every note of the store, so a lie only misleads its own members.
- Per-kind switches (mentions, replies, watched notes apart from the account switch);
  watching from the MCP tools.
- `pimble-server/src/handler.rs`, the app's `app.rs`, `events.rs`, `state.rs`,
  `commands.rs` and `pimble-cloud/tests/integration.rs` were already far over 700 lines
  and are not split here (other sessions work in them on `master`; a split is its own
  change).

## Built in wave 4 (2026-11-23, branch `notifications`)

The tray, desktop only (`native`); the browser is unchanged. Everything comes from rinch
`main` as it is: `rinch::tray` (its `system-tray` feature, now on in `native`: a
StatusNotifierItem through ksni on Linux, tray-icon on Windows), `on_close_requested`,
`hide_current_window`/`show_current_window`, `exit()` and `rinch::notification`.

### Decided here

- **One window process owns the tray and the notifications.** Two app processes on one
  server are the two-window setup, so a second icon and every notice twice had to be
  ruled out. The owner (the primary) holds an exclusive OS lock (`flock` on Linux,
  `LockFileEx` on Windows) on `<config dir>/pimble/app-<server address>.lock`; it goes
  with the process, a crash included, so nothing goes stale. The address is normalized
  first, once per process (the loopback names `localhost`, `127.0.0.1` and `::1` as
  `127.0.0.1`; another loopback or the unspecified address is kept, as it can be a
  server of its own; any other host name resolved), so `localhost:7462` and
  `127.0.0.1:7462` are one lock and no notice shown waits on a name lookup. Any other window process
  shows no icon and no OS notification (its Inbox list and count still fill), waits on
  the lock in a thread, and takes the tray when the owner quits. A primary that quits
  (tray Quit, File > Quit, a close that quits) gives the lock up first, then writes what
  it remembers.
- **No notice is lost in a takeover, and none is shown twice.** Every show asks the
  record first and writes it down in the same step (`shown_notices::first_show`), so a
  notice the old primary showed is not shown again when this window's own copy of it
  arrives only after the takeover. The primary writes the id
  of each notice it shows beside the lock (`app-<server address>.shown`, the last 200);
  a window that is not the primary keeps the ids it did not show. The window taking
  over shows the ones it kept that the old primary never wrote and the person has not
  opened. The file is the record of one computer's windows between themselves; nothing
  syncs it.
- **A plain launch while Pimble runs brings it to the front** and stops: it hands
  `pimble:` (`app_links::COME_TO_FRONT`) through `openInApp` with its XDG activation
  token (rinch #2880). The hand-over is acknowledged: `openInApp` with `confirm`
  answers `delivered` only once the window says, from its own thread, that it came to
  the front and opened the link (`appLinkTaken` with the link's `ack`), and `false`
  after 2 s (`APP_LINK_CONFIRM_WAIT_MS`); the launch then opens a window of its own, so a
  hung primary, or one partway through Quit, never swallows it. A link handed over
  from `pimble <link>` is confirmed the same way. A second window is asked for with `pimble --new-window`; a link
  no running window took also opens its own window, as before. The updater's restart
  waits for the old process before it claims, so it becomes the primary.
- **Closing the window with the icon up hides it**; closing with no icon up quits, so a
  window never vanishes with nothing to bring it back. No icon is: the setting off,
  another window owning the tray, or a desktop with no tray host (vanilla GNOME). The
  last is judged once at start (rinch has no live "is the host there" query). The icon
  is made once the first window is up, on the next turn of the event loop: asking the
  tray host can take up to 5 s when it hangs, and the window must not wait on that.
- **File > Quit (Ctrl+Q) replaces File > Exit (Alt+F4)**: Alt+F4 is the system's close,
  which now hides. Tray "Quit" and File > Quit write what the device remembers of how
  things looked and exit, the window shown or hidden.
- The setting, "Keep running in the tray when the window closes", is on by default and
  lives in Settings > General ("When the window closes"), in `state.json` as
  `tray_keep_running`; turning it off takes the icon down at once, in whichever window it
  is turned: every window process watches the state file, the owner puts its icon up or
  takes it down when the stored value changes, and a window that does not own the tray
  says what is true there ("Closing this window closes only this one. Another Pimble
  window shows the notifications, and with this off, closing that one quits it."). The
  one notice ("Pimble
  is still running") is sent the first time a close hides the window, and
  `tray_notice_shown` records it.

### What was built, and where

- `crates/pimble-app/src/instance.rs`: `WindowLock` with `claim` (:56; a lock file that
  cannot be opened makes this process the primary, with a warning), `wait` (:80) and
  `resign` (:110); `launch` (:135, the whole decision, the hand-over passed in);
  `lock_key` (:177, the address normalized), `lock_name`, `lock_path`.
- `crates/pimble-app/src/shown_notices.rs` (new): `held_back` (:34), `showing` (:39),
  `take_over` (:48) and `to_show` (:55), the `.shown` record (`read`, `record`).
- `crates/pimble-app/src/tray.rs`: `TraySettings` (`from_state`/`write_into`),
  `on_close` (the one decision), `TrayStatus` and `status_line` (:100, Settings'
  sentence), `start` (:152), `became_primary` (:205, the icon and the notices held
  back), `set_keep_running` (:213), `setting_changed` and `after_setting` (:223, :243),
  `follow_setting` (:271, the state file watched), `quit` (:312), `close_requested`
  (:323), `tell_once` (:343).
- `crates/pimble-app/src/desktop.rs`: `bring_to_front` (:17), `run` (:32: the tray
  armed once the window is built (:62), the wait for the lock, the setting followed,
  `on_close_requested`), `remember_views`, `close_window` (:136, the titlebar's close
  asks the same question).
- **Fixes after the review and the walk-through** (2026-11-23):
  - `shown_notices.rs:42` `first_show`: the one check and record every show makes,
    called first in `inbox.rs` `show_system_notification` (desktop).
  - `tray.rs:200` `window_up`: armed from `desktop.rs` once the first window's view is
    built; puts the icon up, then starts the wait for the lock, so a takeover always
    finds the window and the app store (`desktop.rs:42` sets the store at start, not
    only once a note opens); a primary that quits during startup leaves the lock free
    and the wait takes it then, the notices held back meanwhile kept.
  - `instance.rs:243` `lock_path` computed once (`OnceLock`); `lock_key` (:203) folds
    only the loopback names. `hold` (:76) and `resign` (:123) judge under one guard, so
    a lock the waiting thread takes after a resign is let go; `is_quitting` (:191).
  - `app_links.rs:91` `handed` (out of `events.rs`): a quitting window neither comes to
    the front, opens the link, nor sends `appLinkTaken`; `backend.rs` `pass_links` drops
    the link subscription when quitting, so the server hands the next link elsewhere.
    Both under test (`a_quitting_window_neither_opens_nor_confirms_a_handed_link`,
    `a_quitting_window_stops_hearing_links`).
  - `inbox.rs:534` the Inbox's relative times tick each minute (`set_interval`, only
    the time labels read it, stopped with the Inbox).
  - `inbox.rs:60` `ask_for_names`: why the browser said "Untitled": both builds name a
    notice's note from `AppStore::node_data`, which holds only notes some view showed,
    and the desktop had shown the watched note while the browser had not. Every
    notice's note the app does not hold is now read from the device's own documents
    through the shared command path (`BackendCommand::NameNodes`, `commands.rs:246`),
    answered with names alone (`BackendEvent::NotesNamed`, `None` for a note this
    device cannot read, kept in `AppStore::notice_names`; nothing of the tree moves),
    and asked for once: not again while asked, nor on a reconnect once named (a note
    not read is asked again with the next listing). An ask whose answer never comes
    (a backend error, a lost connection, or 30 s with none: `answers_lost`,
    `NAME_ANSWER_WAIT_MS`) is waited on no longer, so the next notice or listing asks
    again. A name read for a notice is read again when the note changes
    (`MetadataUpdated` or `ContentUpdated` on the store: `note_changed`), so a note
    renamed elsewhere shows its new name; a note a view holds is named from the view.
    The Inbox's "where" line and the
    system notification's body come from one place (`inbox.rs` `where_of`: the name,
    or "A note you watch" for a watch notice whose note cannot be read here). A watch
    notice's system notification waits up to 3 s for the name (`notice_show.rs`, out
    of `inbox.rs`: `show`, `names_arrived`), shown once by whichever comes first, and
    asks again when it goes (`still_shows`): a window that resigned the tray or is
    quitting, or a notice read on another device during the wait, shows nothing.
  - `inbox.rs:76` `unread_line`: the status bar words the count for what it counts
    (mentions, replies, changes to notes you watch, or notices when mixed).
  - `notification_settings.rs` (new, out of `app.rs`, one component for the desktop's
    and the browser's Account page): with Notifications off, the Email, browser and
    watched-notes switches show unticked and cannot be changed, and the line under them
    says notifications are off.
- `crates/pimble-app/src/inbox.rs:133`: the OS notification is gated on
  `notifies_here()` (:162 native `instance::is_primary()`, :167 the browser always); a
  notice not shown is held back (:137); `took_over` (:146).
- `crates/pimble-app/src/events.rs:1520`: `BackendEvent::AppLink` comes to the front,
  opens the link, then sends `BackendCommand::AppLinkTaken`.
- `crates/pimble-rpc/src/app_links.rs` (new, out of `types.rs`): `OpenInAppRequest.confirm`,
  `HandedLink.ack`, `AppLinkTakenRequest`, `APP_LINK_CONFIRM_WAIT_MS`;
  `crates/pimble-server/src/app_links.rs` (new): `AppLinks::hand` (:39) and `taken`
  (:63), behind `openInApp` and `appLinkTaken` in `handler.rs`;
  `PimbleClient::open_in_app_confirmed`, `app_link_taken`.
- `crates/pimble-app/src/main.rs:176`: claim, then `instance::launch`; `hand_to_open_window`
  (:194) asks with `confirm`; `--new-window`.
- `crates/pimble-app/src/app_links.rs`: `AppLink::Window` and `COME_TO_FRONT`;
  `menus.rs:203` Quit; `settings.rs:214` the switch; `persistence.rs:95`
  `load_tray_settings`/`save_tray_settings`.
- `links.rs` was 899 lines once touched and is split by responsibility: `app_links.rs`
  (links from outside the app) and `web_links.rs` (web URLs and autolinking).

### Verified

- Tests: `tray.rs` (7: on by default, the round trip keeping the rest of the state,
  hide and tell only the first time, the setting off quits, no icon quits, the owner
  follows the setting and another window says so, every status has a sentence),
  `instance.rs` (8: a plain launch stops when the primary came and opens its own when
  none did, the primary and a new window ask nobody, one lock whatever the address's
  spelling, the lock's name, one primary at a time, a waiting window takes over when the
  primary goes and not before, a window that resigned while waiting lets the lock go),
  `shown_notices.rs` (4: what nobody showed or opened is shown, once; the record keeps
  the last 200; what the old primary wrote down is not shown again), `main.rs`
  (and after the walk-through: `instance.rs` only the loopback names are one host, a
  lock taken after the resign is not held; `shown_notices.rs` a notice shown before
  the takeover is not shown when it arrives after; `inbox.rs` the status bar says
  what it counts, the notes not held are asked for once; `notification_settings.rs`
  with notifications off the channels show off; the fixes were not seen on screen),
  (`--new-window` beside the renderer), `app_links.rs` (`pimble:` and `pimble://` are
  the window), `pimble-server/tests/app_links.rs` (3 more: the `ack` on the wire, a
  confirmed link delivered when the window says so, one no window acts on answers
  `false` after the wait and at once with nobody listening).
- **Seen on screen** (KDE Plasma on Wayland, an app on its own port and scratch dirs):
  the StatusNotifierItem registered; the close button hid the window and the process
  stayed; "Pimble is still running" sent once and `tray_notice_shown` saved, a second
  close sent nothing; the icon brought the window back; a plain second launch brought it
  to the front and exited; a `--new-window` process showed no icon, and took the tray
  when the owner quit from the tray menu; Settings showed the switch, and turning it off
  took the icon down and saved it; with it off, closing the window exited the app and its
  embedded server.
- **Seen again after the review fixes** (same setup): the icon came up 0.4 s after the
  window was built; a plain second launch exited in 17 ms once the window said it came
  to the front; with the owner stopped (`SIGSTOP`), a plain launch opened a window of its
  own after 5 s instead of exiting; writing `tray_keep_running: false` into `state.json`
  (what another window's switch does) took the owner's icon down within 2 s, and `true`
  put it back; with the owner killed, the `--new-window` process took the tray and a
  plain launch came to it. The takeover's notices were not staged on screen (it needs a
  signed-in account and a notice timed into the gap); the tests cover it.

### Not done in wave 4

- Not run on Windows (built for it through rinch's tray-icon backend; CI builds it).
- No macOS tray (the app is not built for macOS).
- A tray host that appears after Pimble starts (a panel restarted, an extension turned on)
  is not noticed until the next start. It needs a general rinch mechanism, with no Pimble
  concept in it (a rinch PR, not proposed yet): `rinch::tray::host_present(timeout) ->
  Option<bool>`, callable from any thread (on Linux: is `org.kde.StatusNotifierWatcher`
  owned and `IsStatusNotifierHostRegistered` true; `None` when the watcher does not
  answer in time; elsewhere always `Some(true)`), and `rinch::tray::on_host_changed(f)`,
  called on the main thread when a host appears or goes. The first would also take the
  icon's build off the main thread's path entirely: it is made after the first window is
  up now, but a hung host still holds the main thread for up to 5 s then (the window is
  drawn, and stalls once).
- **The icon's build still runs on the main thread**, so a hung tray host freezes the
  open window for up to 5 s. rinch's tray API cannot do it elsewhere: `TrayIconBuilder`
  holds the menu's and `on_activate`'s `Rc` callbacks, `TrayIcon` is `!Send`, and
  `build()` blocks on the host's registration (rinch `tray.rs`, `BUILD_WAIT`). What
  rinch would need, general and with no Pimble concept in it:
  `TrayIconBuilder::build_in_background(self, done: impl FnOnce(TrayResult<TrayIcon>) +
  'static)`, which registers the callbacks on the calling (main) thread, runs the host
  wait and the D-Bus registration on a thread of rinch's own, and calls `done` on the
  main thread with the icon or the error (`NoHost` included) when it is settled; the
  caller never blocks. With it, `tray::start` would set `TrayStatus` from `done`.
- A mention whose quote starts at an earlier "@" within about 40 characters still opens
  at that earlier "@": telling them apart needs the mention's position in the sealed
  notice, a format decision for Joe.
- A hung primary that recovers after the acknowledgement's wait also comes to the
  front, so both windows show.
- A hung primary's own server (the embedded one, in the same process) makes a launch
  wait for its connection to time out (5 s) rather than the 2 s of the acknowledgement.
- The notices held back while not the primary are the ones heard live; a notice drained
  at a reconnect during a takeover (the old primary's embedded server gone) is listed in
  the Inbox, as at any start, and not shown.
- `handler.rs`, `pimble-rpc`'s `types.rs` and `pimble-client`'s `client.rs` are over 700
  lines; only the app-link pieces moved out of them here (another session was editing the
  server's watch and relay code).
- A real crash of the owner was not staged; the lock's release is the OS's (covered by
  the test that drops the holder).
- `app.rs`, `events.rs` and `state.rs` are still far over 700 lines (see wave 3), and
  so are `commands.rs` and `protocol.rs`; the fixes after the walk-through moved
  pieces out of `app.rs` and `events.rs` (`notification_settings.rs`,
  `app_links::handed`) and added one command to the last two, splitting none of them.
