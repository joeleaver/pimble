# Notifications contract: hearing that someone wrote to you

Status: **design agreed 2026-11-23; wave 0 (mentions as text) built the same day**, see
"Built in wave 0" at the end. Written from a conversation with Joe that
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
