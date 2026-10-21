# Discussions contract: threaded conversations anywhere

Status: **waves 1 to 5 built** (branch `discussions`, not merged; the "Built in ..." sections at the end say what exists, how it was checked and what is missing; margin markers wait for rinch #1683, the commenter role is wave 6); every question asked so far is answered. Written on 2026-10-21 from a conversation with Joe: "a
new node/embed type, discussions. A little like comments, but a way to put a threaded,
replyable discussion anywhere." His answers that day are listed under "Decisions" at the
end. The questions still open are his and are listed under "Open decisions".

Builds on docs/NODE_DOCUMENT_CONTRACT.md (the `data` root, sharing and `Reach`),
docs/SHEETS_CONTRACT.md (the same two placements: a node, and an embed in text through
rinch node views), docs/IMAGE_MARKUP_CONTRACT.md (account attribution, board elements;
its "Later" list has "comment pins with threads", which this contract covers),
docs/LINKS_CONTRACT.md (`pimble:` links, `[[`) and docs/AGENTS_CONTRACT.md (agents as
authors).

## The idea

A **discussion** is a conversation that belongs to one place. It has messages, and a
message can reply to any other message, to any depth. It lives where the thing being
discussed lives:

| Where | How | Wave |
| --- | --- | --- |
| **A discussion node** | `node_type = "discussion"`, a row in the tree, opens in a pane | 1 |
| **On a node** | attached to it: listed in its Discussions panel | 1 |
| **In text** | attached at a point in the text: a marker in the margin; editors can also place it inline (an `embed` atom) | 2 |
| **On a board** | attached at a point on the board: a pin; the thread opens beside it | 3 |
| On a sheet row, on a range of text | later (see "Later") | - |

Comments are not a second feature. When comments come, a comment is a discussion
anchored to a range. There is one model, and each new anchor is just another place for
it.

## 1. A discussion is its own document

Every discussion is its own node document, including the ones shown in text or on a
board. It is never stored as a key in the host node's `data`, the way a table in text is.
There are three reasons:

- **Its permissions can differ from the host's.** Replying to a note you may only read is
  the usual case for discussion. Its own document can carry a role of its own (section 7).
  Inside the host's document, only the host's editors could reply.
- **A long conversation stays out of the note's history.** A note's document isn't
  weighed down by a thread that runs to hundreds of messages.
- **It has a node id.** Links, backlinks, search hits, unread state and MCP tools can all
  name it.

**Attached, and the host is never edited to attach it** (Joe, 2026-10-21: commenters
can start discussions). A discussion on a node names its host in its own `node` root
(`attached_to`, written in the transaction that initialises the document) and its
place there in `data.discussion.anchor` (section 2). The host's document doesn't change:
no `children` entry, no atom and no board element is needed. So anyone who may write
the discussion may start one, and a commenter never writes the host.

- **Not in the tree.** An attached discussion is in no children list and has no
  `parent_id`. `pimble_crdt::Tree` calls it an **attachment**: a document whose
  `node` root names a host (`attached_to`). The key is general: nothing in the tree
  knows it is a discussion. `Tree::repair`, `validate_tree` and `listDeleted`'s
  "nodes no list names" skip attachments, and `Tree` keeps a reverse index from host to
  attachments that every reader of a host's discussions uses. The explorer shows a count
  badge on the host's row. A standalone discussion node ("New Discussion" in a row's
  menu) has no `on` and is an ordinary tree node.
- **It goes where its host goes.** Moving the host within its store changes nothing,
  because the discussion names the host's id. A move that is a transplant (out of a
  share, or between stores, docs/MOVE_CONTRACT.md) makes the host again under a new
  id, so `Tree::take_cutting` takes the attachments of every node it cuts, and
  `Tree::plant` makes them again as attachments of the new nodes, with fresh
  documents, like the rest of the subtree. The originals stay with the tombstoned
  host, so Put Back brings them back with it. A deleted host hides its
  discussions, and Put Back brings them back with it.
- **Its scope is its host's.** A discussion is reachable by exactly the scopes that
  reach its host (section 7).

## 2. The discussion document

The rule of the sheets contract applies here as well: no container is ever created on
first use. The node is created with every container it needs.

```
data.discussion: Map                       created with the node
  v:          1
  anchor:     Map | absent                  absent: the node as a whole (or a standalone one)
    kind:     "text" | "board"
    text:     Bytes                         rinch's sticky bytes for the host's `content` (opaque)
    board:    String                        "board" or "board:<id>" (a picture in text)
    x, y:     Number                        board coordinates
  resolved:   { at, by: Author? } | absent  the whole discussion (section 4)
  messages:   Map<message id, Map>          created with the node, never replaced
    <id>: Map
      reply_to:   String | absent           a message id; absent at top level
      created:    String                    RFC 3339 UTC with milliseconds, the shape boards use
      author_id, author_name: String | absent   section 3
      body:       Text                      formatted (section 5)
      edited:     String | absent           last edit's time
      deleted_at: String | absent           a tombstone (section 4)
```

- **The topic is the node's title** (Joe, 2026-10-21: yes to a topic line). There is no
  second name: the title in the `node` root is the line above the thread, the tree row of
  a standalone discussion, and the search result's name. It may be empty. An empty topic
  shows as "Discussion" in the tree and as a "Topic" placeholder in the thread. It isn't
  auto-titled from the first message.
- **Messages are keyed by id, never kept in an Array.** Two people replying at once each
  add a key, and nothing moves.
- **Order is computed and never stored.** Sibling messages (same `reply_to`) sort by
  `(created, id)`. A reply whose parent isn't there yet (it arrived first in a merge) is
  shown at the top level with "replying to a message not here yet" until the parent
  arrives. A `reply_to` that names its own descendant can only come from tampering: the
  cycle is broken at the message with the smallest id, which is shown at the top level.
  Repair writes nothing. The view decides this the same way on every device.
- **Only the document API writes it**: a `pimble-discussion` crate shaped like
  `pimble-board` (`Discussion::of_node(&NodeDoc)`, `edit(Stamp, |DiscussionTxn|)`
  returning the yrs update sent through `applyEdit`, and `DiscussionSnapshot` as the
  thread tree for views, search and MCP). There is one way to write it, as with node
  content.

## 3. Authors

Authorship works as it does for board markup (Joe, 2026-10-09): `author` is the
signed-in account's `sub` and the name it had when the message was written, stamped
automatically. Nobody types a name. Signed out, a message has no author. That only
happens when one person is using the store, so the UI hides names when every message is
unauthored.

An agent writing through `pimble-mcp` is stamped `{ id: "agent:<agent id>", name:
<the agent's name> }`, the identity docs/AGENTS_CONTRACT.md already gives its edits.
Its messages carry a small "agent" mark.

## 4. Editing, deleting, resolving

**Decision 3a (Joe, 2026-10-21): the document stays co-editable, and the UI is polite.**
The app offers Edit and Delete only on your own messages, and on unauthored ones. The
document doesn't enforce this, and in a vault store the server can't, because it holds
ciphertext. That's accepted. A later signed-attribution feature could show tampering,
but it isn't planned.

- **Edit** rewrites the message's `body` Text in place (the smallest diff, so two of
  your devices editing one message merge) and sets `edited`. The message shows
  "edited".
- **Delete** is a tombstone (`deleted_at`) and nothing is removed. A deleted message
  with replies shows as "Message deleted", and its replies stay where they are. A
  deleted message with no replies disappears from the view. "Recently deleted messages"
  in the discussion's menu lists them with Put Back.
- **Resolve** closes the whole discussion: it collapses to its topic and first message,
  with "Resolved by ... · Reopen". Anyone who may reply (commenters included) may resolve or reopen. Resolving
  single branches isn't in wave 1.
- **Deleting a discussion** is deleting its node: a tombstone on the node, with
  "Recently Deleted..." and Put Back as for any node. Deleting an inline atom doesn't
  delete it (section 6).

## 5. Message bodies

Joe, 2026-10-21: bold, italic, links and emoji.

- **Stored** as one yrs `Text` with formatting attributes (`bold`, `italic`, `link` with
  an `href`), which merges character by character like any Text.
- **Links** are web links and `pimble:` node links. `[[` opens the same picker it does
  in text, and node links count as backlinks and follow `became` (docs/LINKS_CONTRACT.md).
- **Emoji** is plain Unicode. The composer has an emoji button with a small picker, and
  `:name:` autocompletes. Rendering depends on the font: on the desktop, rinch's text
  stack must draw color emoji (to be checked in wave 1; see the rinch section).
- **The composer.** The app mounts rinch's `Editor` exactly four times and never more
  (CLAUDE.md, "Collaboration shape"), so the composer isn't a fifth editor. It is a
  multi-line input with Markdown shortcuts (`**bold**`, `*italic*`, `[text](url)`,
  `[[`, a bare URL becomes a link) that become attributes when the message is sent. The
  same input edits an existing message, starting from its body written back as Markdown.
  Enter sends, Shift+Enter starts a new line, and Escape cancels an edit. Formatted
  display is a read-only render of the Text.

## 6. Anchors

**In text (wave 2): a marker in the margin.** The anchor is a yrs sticky index into the
host's `content` (it stays put while text around it changes, and lands at the nearest
place when its text is deleted). The view draws a marker beside the line that holds it,
with the count and an unread dot. Clicking it opens the thread in a popover beside the
text (a sheet at phone width). "Start a Discussion Here" is in the text's context menu
and on the toolbar, and it is available to commenters, because it writes only the new
discussion. A marker never moves the text around it.

**In text: inline.** An editor can also show a discussion as a block in the text: an
`embed` atom with `data-node` (the discussion's `pimble:` link) and
`data-kind="discussion"`, rendered live through a node view, the way sheet nodes
embedded in text are. It shows the topic, the thread and a composer. More than about six
messages collapse behind "Show all N", and a resolved discussion collapses to one line.
Insert > "Discussion" creates an attached discussion anchored at the caret and inserts
its atom there, in one action that can be undone. A discussion shown inline isn't also
drawn in the margin. Removing the atom loses nothing: the discussion goes back to being
a margin marker at its anchor. Any discussion node's link can be embedded this way,
standalone ones included. Placing an atom edits the text, so commenters can't.

**On a board (wave 3): pins.** The anchor is a point on a board. The board view draws a
pin for each discussion attached to it, as an overlay read from the host's attachments.
A pin is not a board element, so the board's document isn't edited and commenters can
add them. It is a numbered marker, fixed in screen size, with an unread dot. Selecting it
opens the thread in a popover anchored to the marker. The Pin tool (for commenters too)
creates a discussion at the point clicked. Dragging a pin rewrites the discussion's
anchor. On a picture in text, the anchor names the picture's board (`board:<id>`) and
the host is the text node. "Show markup" doesn't hide pins. The board's own menu has
"Show discussions", per viewer.

**On the node as a whole, and the Discussions panel.** "Start a Discussion" in a row's
context menu or the pane's menu creates one with no anchor. Every node with attached
discussions has a "Discussions (N)" panel (from the row's badge, the pane's menu and
View) that lists all of them: anchored or not, inline or in the margin, with unread
counts. Choosing one scrolls to its place.

## 7. Who may reply: the `commenter` role

Decision (Joe, 2026-10-21): a third role, **`commenter`**, between reader and editor.

| Role | Notes, boards, sheets | Discussions in scope |
| --- | --- | --- |
| reader | read | read |
| **commenter** | read | read; start one (on a node, in the margin, as a pin), reply, edit and delete their own messages, resolve and reopen |
| editor, owner | read and write | everything, including placing one inline in the text |

- **Where it appears.** The Share dialog offers Reader, Commenter and Editor. The token's
  grant shapes take the value `"commenter"` wherever they take `"reader"` or `"editor"`
  (a whole-store grant or a scoped root), and `pimble-server` reads it as
  `Role::Commenter`. `pimble-cloud`'s grants and members routes accept it.
  docs/CLOUD_CONTRACT.md and docs/NODE_DOCUMENT_CONTRACT.md section 5 get the new row.
  Agents' grants (docs/AGENTS_CONTRACT.md) may be commenter too: an agent that reads and
  answers in discussions, and touches nothing else.
- **How the server judges it.** `Reach` (the one place scoped writes are judged) lets a
  commenter write a document only if it is a discussion attached to a host in their
  scope, and create one only attached to a host they may read. In a plain store the
  server reads the document's `node_type` and `on`. In a vault store it can't
  (ciphertext), so `scopes.json` gains a **discussion set** (`discussions: { <id>: <host
  id> }`, which only grows, like the scope set). The owner's share upkeep publishes it
  from the plaintext it holds. When a scoped member creates a discussion, its first
  `vaultAppend` names the host in clear, as a new document's parent is named today, and
  the server adds the discussion to the set and to every scope that reaches the host.
  The hosted server and the relay face judge against it. Every other write is refused
  with "You can comment here, not change it." (`StoreAccess::COMMENT_ONLY_REFUSAL`), and
  the app locks the host's editor read-only exactly as for a reader.
- **Accepted residuals, the same kind as 3a.** A commenter may write anything inside a
  discussion document they can reach: other people's messages, the topic, its anchor.
  The UI only offers what the table says. In a vault store, the server can't check that
  a document a commenter creates "as a discussion" really is one, so a commenter could
  store other content in their share that way. Every app only ever shows such a document
  as a discussion. Nothing a commenter writes can reach the host's own document.
- **On a member's desktop**, `Store.access` and `sync.json`'s per-root roles gain
  `commenter`. The link refuses locally what the server would refuse, so a commenter's
  edits never sit pending.

## 8. Unread

Decision 4 (Joe, 2026-10-21): read state is personal and never goes in the shared
document, because a glance must not be an edit that everyone syncs.

Decision (Joe, 2026-10-21): **read state follows you across your devices.**

- **What it is.** One yrs document per account, the **personal document**, edited only
  by that person's devices. It holds read state (below) and your place in every node
  (section 8b):
  ```
  read: Map<"<store id>/<discussion id>", Map>
    watermark: String           RFC 3339; every message created at or before it is read
    seen:      Map<message id, true>   read messages newer than the watermark
  ```
  Seen ids merge as keys. When every message up to some time has been seen, the
  watermark moves there and the keys at or below it are dropped. Two devices moving the
  watermark at once resolve last-writer-wins, and a watermark that loses can only mark a
  few messages unread again, never read.
- **A message is seen** once it has been on screen for about a second, in a pane, an
  embed, a pin's popover or the Discussions list.
- **Where it lives.** Read state is account data, not store data, and sharing carries
  none of it to anyone. With nobody signed in, it stays on the device
  (`<data dir>/pimble/personal.yrs`; in the browser, IndexedDB). Signed in, it is
  encrypted end to end like a vault document. Its data key is derived from the account's
  key material (HKDF, `"pimble personal"`), so only the person's own devices can open
  it. It is held by the accounts service as an append-only log plus a snapshot, under
  `/api/v1/me/personal` with the vault RPCs' shape and limits. Pimble Cloud holds
  ciphertext and learns nothing, not even which discussions exist.
- **Signing in is the asking** (Joe, 2026-10-21). Read state is the person's own account
  data, encrypted so only their devices can read it, so signing in to Pimble Cloud turns
  the sync on; there is no separate switch to find first. The Account modal says so in
  one line ("What you've read in discussions stays in step across your signed-in
  devices") and has a switch to turn it off. Turning it off stops the uploads and deletes
  the hosted copy. Signing out stops the uploads and keeps a local copy on the device.
  This is the one exception to the hosting rule, and it covers only this account-level
  read state: no store, node or discussion content is ever uploaded because someone
  signed in. When someone signs in, the device's signed-out document merges into the
  account's.
- **Shown as:** an unread dot on the embed, the pin and the host row's badge; a "N new"
  divider in the thread; and View > "Discussions..." listing every discussion with
  unread messages across the open stores, newest first. Your own messages are read as
  you send them.

## 8b. Your place in every node

Joe, 2026-10-21: the sticky positions discussions use also remember where you were in
every node. This isn't a discussion feature, but it lives in the same personal document
and is built with wave 4.

- **What is kept, per node you've had open:** your selection as two sticky positions
  (anchor and head, the same opaque rinch bytes as section 2) and the first visible line
  as a third, so coming back shows the same view even when the caret was off screen.
  ```
  places: Map<"<store id>/<node id>", Map>
    anchor, head: Bytes          the selection (equal for a plain caret)
    top:          Bytes          the first visible line
    at:           String         when it was last written
  ```
- **Restored** when a node opens in a pane and its collaboration session is up, from
  `top` and then the selection, unless the opening already names a place (a link to a
  heading, a search hit, a discussion, a back/forward step that carries its own). Because
  the positions are sticky, the place survives other people's edits while you were away,
  and it lands nearby (`displaced`) when its text was deleted. Being on a different
  device doesn't matter: the personal document follows you.
- **Written sparingly**, never per keystroke or caret move: when a pane leaves the
  node, when the window loses focus or closes, and after about five seconds of quiet.
  Each write replaces that node's entry.
- **Bounded**: only the 2,000 most recently written entries are kept (by `at`); older
  ones are dropped by whichever device notices first, which is a deterministic rule
  every device applies the same way.
- **Boards and sheets later**: a board's viewport (pan and zoom) and a sheet's selected
  cell would be entries of the same shape under the same key.
- Read-only nodes remember a place too: nothing in the node's document changes.

## 9. Search, MCP, sharing, sync

- **Search.** A discussion's `index_units` are its topic and one unit per message (body
  plus author name). A hit opens the discussion scrolled to that message, at its place in
  its host when it is attached. The search results row says "in a discussion on <host title>".
- **MCP** (full support, as for sheets): `list_discussions(node)` (attached plus
  standalone, with counts and unread), `read_discussion(id)` (the thread as nested
  Markdown, with ids, authors and times), `start_discussion(node, anchor?, inline?)`
  (anchor `after_text <quote>`, `block <n>` or `board_point x,y`; `inline` for editors), `reply(discussion, reply_to?,
  markdown)`, `edit_message`, `delete_message`, `resolve`/`reopen`. An agent's grants
  judge these as they judge every other tool.
- **Sharing and sync**: a discussion is a node document, synced by the links that sync
  every document and encrypted in vault stores like everything else. What is new is the
  reach: plain stores compute a scope from the tree plus its attachments, and vault
  stores use the discussion set (section 7). Creating, changing or deleting an
  attachment is announced as `StoreChangeKind::AttachmentChanged { host_id, node_id }`
  (from `derive_kinds`), so a host's open views refresh their markers, pins and badge.
  Nothing is hosted as a side effect.
- **Desktop and web together** in every wave (Joe, 2026-10-06), from shared `pimble-app`
  code.

## 10. rinch

Most of it exists: node views (`embed`, `NodeViewSpec`) for discussions inline, the
board surface for pins, and inputs for the composer. Two things are needed for margin
markers, each a general mechanism with no Pimble concepts in it (Joe, 2026-10-10):

- **Sticky positions for the collaborative editor**: rinch-editor-collab encodes a yrs
  sticky index for a document position, and resolves one back to a position after any
  edit. Nothing Pimble-specific: "remember this place".
- **Margin decorations**: the app gives the editor a list of `(position, key)`, and the
  editor reports each one's on-screen line box as it lays out (or renders a component for
  each in a gutter), so the app can draw markers beside lines without editing the
  document.

Agreed with the Rinch session (2026-10-21), and in progress there:

- `EditorHandle::collab_sticky_index(pos)`, `collab_resolve_sticky(bytes)` and the
  headless `CollabDoc::sticky_index`/`resolve_sticky` already exist (plain yrs
  `StickyIndex::encode_v1` bytes). The PR adds selectable assoc, a fallback to the
  nearest surviving place when the anchor's whole block is deleted, a headless resolve
  to a block path, and a test that encoding works headless (for MCP).
- `EditorHandle::set_margin_anchors(Vec<MarginAnchor { key, at, assoc }>)` and
  `on_margin_layout(|&[MarginLine { key, top, height }]|)`: line boxes in the editor's
  content coordinates, which don't depend on scrolling. Pimble draws the markers itself.
  `Editor { ... }` children become an overlay layer that scrolls with the content (not
  part of the model, the diff, the caret map or the accessibility text). Anchors in
  table cells report their line, anchors in node views may be omitted, and the text
  column's right edge is reported if cheap. (Pimble's own `overflow` is what makes the
  editor scroll today; rinch's stylesheet doesn't.)
- Sticky positions are merged (joeleaver/rinch#1679, rinch `main` 2a42fa7a; the pin moves there with wave 2):
  `StickyAssoc { After, Before }`, `CollabDoc::sticky_index_with`, `locate_sticky`,
  headless `locate_sticky_in(schema, bytes)` answering `StickyLocation { pos, path,
  displaced }`, and the same on `CollabSession` and `EditorHandle`. The bytes are a yrs
  `StickyIndex` plus a versioned trailer: Pimble stores them as opaque and never decodes
  them. A deleted block resolves nearby with `displaced: true`, which the margin marker
  can show ("the text this was on was deleted").
  Also: `locate_sticky_many` (one pass for many anchors, for the margin), `None`
  means "not locatable right now" and can change, and an anchor is displaced for
  good after an undone deletion or a list toggle (the block is a new CRDT item), or
  when the document is re-projected from the model (never done while collaborating).
- Issues and PRs: joeleaver/rinch#1676 (sticky positions), #1677 (margin anchors and
  the overlay slot), #1678 (raw input keydown/keyup with modifiers and a flag to prevent
  the default, a selection read in UTF-16 units, `NodeHandle::caret_rect()`). Color emoji
  and scrolling inside a node view haven't been checked in a running app yet.

To check in wave 1, and to become PRs if they fall short:

- Color emoji in the desktop text stack (font fallback to an emoji font).
- A multi-line raw input that reports Shift+Enter apart from Enter, and caret and
  selection access for the Markdown shortcuts and the `[[` picker.
- A node view hosting a scrolling list with inputs inside the document, where focus
  passes in and out with `return_focus` (sheets do this already).

## Waves

1. **The model, discussion nodes, and discussions on a node.** The `pimble-discussion`
   crate (document, edit, snapshot, thread order and its tie-breaks, Markdown in and out
   of bodies, tests including concurrent replies, edits and deletes merging on three
   replicas), attachments in `Tree` (the reverse index; repair, validation and
   `listDeleted` skipping them; `take_cutting` carrying them), `AttachmentChanged`, "New
   Discussion" and "Start a Discussion", the discussion view in a pane, the Discussions
   panel and the row badge, the composer with formatting, links and emoji, edit, delete
   and resolve, search units, and the MCP tools.
2. **Discussions in text.** The two rinch mechanisms (section 10), margin markers and
   their popover, "Start a Discussion Here", the inline node view, and Insert >
   "Discussion".
3. **Pins on boards.** The overlay, the Pin tool, dragging, the popover, and pins on
   pictures in text.
4. **Unread, and your place in every node.** The personal document on the device, section 8b, the dots and badges, the "N new"
   divider, View > "Discussions...".
5. **Personal state across devices.** The encrypted personal log in the accounts service, the
   account switch, and the merge when it is turned on.
6. **The commenter role** (section 7): token claims, `pimble-cloud` grants, `Reach`
   (attachments in plain stores' scopes), the discussion set in vault stores and its
   extension by members, the Share dialog, refusals, members' replicas, and the
   sharing harness tests (`crates/pimble-server/tests/common`).

Each wave is checked on the local stack with two people in one discussion at once
(desktop and browser), replying to the same message concurrently, and with a share
member's reply reaching the owner.

## Later

- **@mentions**, once people and presence exist (Joe, 2026-10-21: wanted, deferred until
  presence). A mention is a node-link-like atom naming an account `sub`. The mentioned
  person's unread view lifts it, and nothing is sent anywhere.
- **Comments on a range of text**: an anchor with two sticky positions instead of one,
  shown as a highlight and the margin marker (it needs a third general rinch mechanism:
  decorations over a range that aren't marks in the document).
- **Discussions on sheet rows**: an anchor naming a table and a row id.
- **Reactions** (a Map of emoji to account ids per message).
- Resolving a single branch; turning a message into a new note or task; signed
  attribution.

## Open decisions (Joe's)

None right now.

## Decisions (Joe, 2026-10-21)

- A discussion is a node type and an embed, and can be put anywhere. Comments become one
  of its anchors rather than a second feature.
- Editing others' messages: the document stays co-editable, and the UI offers Edit and
  Delete only on your own messages (option 3a).
- Read state is personal and never goes in the shared document.
- Pins on boards are an anchor (a good use for pins).
- Replies to any depth. The view indents five levels, then shows deeper replies flat
  with "replying to <name>".
- Mentions are wanted, deferred until presence exists.
- Bodies have bold, italic, links and emoji.
- A `commenter` role: reads, and writes discussions only (section 7). Commenters can
  start discussions too, so a discussion records where it belongs in its own document
  and the host is never edited to attach it (section 1).
- Read state follows the person across their devices (section 8), and signing in counts
  as asking for that.
- A topic line, which is the discussion node's title.
- The same sticky positions remember your place in every node, in the personal
  document, following you across devices (section 8b).

## Built in wave 1 (2026-10-21, branch `discussions`, rinch `main` 1378e13)

What exists:

- **`pimble-discussion`** (`doc.rs`, `body.rs`, `thread.rs`): the document under
  `data.discussion` with its containers made by `new_content` at creation; `post`,
  `edit_message` (common prefix and suffix kept, then every span formatted again),
  `delete_message`/`restore_message`, `resolve`/`reopen`, `set_anchor`; bodies as a yrs
  Text with `bold`/`italic`/`link` attributes and the Markdown they are typed as (a bare
  `http(s)://` or `pimble:` URL is a link; `_` inside a word is text); the thread in
  `(created, id)` order with replies waiting for a parent not here yet, cycles cut at the
  smallest id, and a deleted message shown only while a reply is; index units and the
  Markdown MCP reads. 19 tests, three replicas converging among them.
- **Attachments in `pimble_crdt::Tree`**: `node.attached_to` (`NodeDoc::init_attached`),
  `Tree::add_attachment`, `attachments_of`, `attachment_counts`, `is_attachment`; repair
  and validation skip them; an attachment's delete and undelete touch no list; a deleted
  host hides its attachments; `take_cutting`/`plant` make a transplanted node's
  attachments again under the new node. 7 tests; 600 convergence seeds pass.
- **Server**: `createAttachment`, `listAttachments`, `whoAmI` (an agent's id and name),
  `Node.attachments` (a count reported with `getNode`/`getChildren`, never stored),
  `StoreChangeKind::AttachmentChanged { node_id, host_id, live }` from `derive_kinds`,
  plain scopes and vault keys reaching an attachment through its host, `listDeleted`
  not calling a live attachment lost, and a `DiscussionPlugin` indexing messages.
- **App (desktop and web)**: `discussions.rs` and `discussions/ui.rs`: a pane holding a
  discussion (session, reconcile, peers' updates), the topic line (the title), the thread
  indented five levels, Reply / Edit / Delete (your own and unauthored only), "N deleted
  messages" with Put Back, Resolve / Reopen with the collapse and "Show all N
  messages", the message box (Enter sends, Shift+Enter, Escape, emoji picker), the
  "Discussions (N)" list under a note (in its text) or under a board or sheet, the row's
  count, "New Discussion" and "Start a Discussion" in row menus and Edit > "Start a
  Discussion", opening beside the note (an empty or discussion pane first, else a split),
  and the layout remembering discussion (and, newly, sheet) panes. The web's vault client
  creates, lists and counts attachments, derives `AttachmentChanged`, and wraps a new
  discussion under its host's share keys.
- **MCP**: `list_discussions`, `read_discussion`, `start_discussion` (`on` or `parent`),
  `reply`, `edit_message`, `delete_message` (the agent's own messages only: an
  unauthored message is the person's), `resolve_discussion`; messages stamped with the
  agent's id and name.

How it was checked:

- `cargo test --workspace --release`: 1055 passed.
- The release desktop app on a scratch store on its own port (driven over rinch's debug
  port): start a discussion on a note, post with Enter, bold and emoji (drawn in color by
  the GPU renderer), reply, set the topic, edit by keyboard, delete and put back, resolve
  and the collapse, restart (everything and the layout back), a standalone discussion
  from the store row, Shift+Enter. An agent through `pimble-mcp` (scratch agent key):
  `list_discussions`, `read_discussion`, `reply` and `start_discussion`, each shown live
  in the open app with the agent's name and mark.
- The browser on the local stack, in an encrypted store (made-up `alice@example.com`):
  start a discussion, post, reload and unlock (it comes back), two tabs replying live to
  each other with the note's list following, and no typed word on the hosted disk.

Found and fixed while checking: rinch cancels a timer, and drops a keyboard interceptor,
that a component armed when it unmounts, and Edit rebuilds the message its button is in
(the box's keys and focus are now armed with `unowned`); the node cache a note's list
reads was not refreshed by a pane's own or received edits; an agent could edit an
unauthored message; the saved layout dropped sheet panes.

Not done or not seen:

- `[[` links and `:name:` emoji completion in the message box (links are typed as
  `[text](pimble:...)` or pasted); the box hears Enter through the keyboard interceptor
  until joeleaver/rinch#1678, and a press in the tree does not release it.
- Two devices on different servers at once (only two tabs on one hosted server, and the
  app with an agent on one local server); a share member's reply reaching the owner.
- Readers see no message box; commenters do not exist yet (wave 6).
- Color emoji with `--cpu` (rinch #804: the software renderer draws no bitmap color font).

## Built in waves 2 to 5 (2026-10-21, branch `discussions`, rinch `main` 4c5b5fb)

**Wave 2, discussions in text (all but the margin markers).** The app's discussion module
holds a discussion per *slot*: a pane, or an inline atom in a pane's text
(`discussions::Slot`), so any number show at once and stay in step. An `embed` atom with
`data-kind="discussion"` and `data-node` is drawn by a rinch node view as the pane's view
in a box (topic, thread scrolling inside, message box, "Open" beside the text). Edit >
"Insert Discussion" anchors a new attached discussion at the caret (rinch's
`collab_sticky_index_with`, `StickyAssoc::After`) and inserts its atom where that anchor
resolves when the discussion is made. MCP: `start_discussion` takes `after_text` (a quote:
`NodeDoc::sticky_at_quote` makes a plain yrs sticky index, which rinch resolves too) and
`inline` (writes the atom after the quoted block; the strict Markdown reader takes it only
with attributes in rinch's own order); `get_node` names each inline discussion, a node's
count and a discussion's thread. Checked in the release app: insert, post inline, Open
beside and replies both ways, an agent's inline discussion arriving live in an open note.
**Not built:** margin markers and "Start a Discussion Here" (waiting for
joeleaver/rinch#1683, margin anchors and the overlay slot), and the message box's own keys
(joeleaver/rinch#1686; it uses the keyboard interceptor until then).

**Wave 3, pins.** Board `Tool::Pin` (`q`) makes a discussion attached to the board node
with an `Anchor::Board` at the point pressed and opens it beside; the board view draws a
numbered pin per discussion anchored to its own board (`discussions::board_pins`,
`boards::PinView`), following the camera, with an unread dot; a press opens its discussion.
**Not built:** the popover (a pin opens its discussion in a pane), dragging a pin, pins on
pictures in text.

**Wave 4, read state and places on this device.** `pimble_discussion::Personal` (the
personal document: `read` with a watermark and seen ids, compacted; `places` bounded at
2,000), saved by the app a second after a change (`<data dir>/pimble/personal.yrs`, the
browser's `localStorage` `pimble.personal`). A slot's unread messages are marked new (one
"N new" line) and stay marked while it shows them; they count as read once it has been on
screen a second. Unread counts on a note's list, a dot on its tree row and on pins, View >
"Discussions..." (`listDiscussions`, every store open) listing what is unread, newest
first. One's own messages, and every unauthored one, are never unread. Places: the
selection as two sticky indexes, written when a pane leaves a note and five seconds after
the caret rests, put back when it opens with nothing else naming a place. Checked: counts,
dots, the list, opening from it, marking read, read state surviving a restart, a place
written. **Not seen:** a place put back (the debug port cannot read the editor's selection);
`top` (the first visible line) is not kept.

**Wave 5, the personal document across devices.** Sealed with `pimble_crypto::personal_key`
(HKDF of the account's encryption secret) and `personal_aad(user)`; the accounts service
holds it as one `PersonalDoc` row (blob, version) under `GET`/`PUT`/`DELETE
/api/v1/me/personal`, a `PUT` naming the version it replaces (409 makes the writer fetch,
merge and write again). The desktop's local server does that for the app
(`cloudPersonalSync`, `Service`-only); the browser does it itself (`web/src/personal_sync.rs`).
Signing in syncs at once; changes go five seconds later; every device asks every minute.
**Off is the account's**: the switch (the Account window, the browser's account page)
empties the hosted copy, every device that meets the empty copy turns its own switch off,
and a device that is turned on writes over it, which turns the others on again. Checked on
the local stack with the desktop app and the browser as one account: the desktop's read
state reaching the browser, off from the browser reaching the desktop within a minute, and
on again. **Deploy note:** the accounts service's schema has a new type (`PersonalDoc`).

## Built in wave 6 (2026-10-21, branch `discussions`, rinch `main` 4c5b5fb)

**The commenter role.** `commenter` is a role in the accounts service (`ROLES`, token
claims), `MemberRole::Commenter` in the Share dialog, the CLI's `cloud-share-invite` and
the stub accounts service. On a server, `Role::Commenter` reads like a reader and
writes as `Access::Comment`: `Reach` has a `commentable` set; `require_doc_write` takes a
commenter's write of a discussion they reach (`judge_access` answers `Full` for a
discussion document, `Comment` for every other one); a new discussion is a
`createAttachment` (plain stores) or a `vaultAppend` with `discussion: true` naming the
host (vault stores, which keep a discussion set in `scopes.json`, only grown, which the
owner's upkeep publishes with `setScope`'s `discussions`). Refusals: "You can comment here,
not change it." (`StoreAccess::COMMENT_ONLY_REFUSAL`; `StoreAccess::refusal` picks the
sentence), and a replica answers it for any write it refuses where commenting is allowed
(`write_refusal`).

A member's replica: `sync.json` carries `comment_roots` beside `read_only_roots`
(`HeldAs::from_rows`; access is `Comment` when no root is edited and some are commented),
`LocalStore::comment_refused` judges discussions, `scope_roots_above` follows
`attached_to`, and the vault link pushes a commenter's discussions and holds everything
else back, marked (before this, a commenter's discussion stayed on their device). The
browser's vault client judges the same way (`AccountStore::access`/`node_access`,
`VaultStore::access_of`, `refuse_write` letting through a new discussion and a
discussion's edits). The app: a note a commenter reaches is locked and says the
commenter's sentence; the row menu offers "Start a Discussion" and nothing else that
writes; "Insert Discussion" (in the text) is an editor's.

Tests: `share.rs` `a_commenter_starts_a_discussion_and_changes_nothing_else` (refusals, a
discussion and a message from the commenter reaching the owner, the owner's answer
reaching the commenter), the browser client's
`a_commenter_writes_discussions_and_nothing_else`, the vault discussion set. Checked on
the local stack: alice shares a folder with bob as a commenter; in the browser bob's note
is locked with the commenter's sentence, he starts a discussion and posts, and the
message is on alice's machine (her search finds it) and nowhere in clear on the hosted
disk. **Not checked on screen:** a commenter on the desktop app (the same replica code the
test drives, and the same UI). **Not built:** a commenter role for agents.

## Built: margin markers (2026-10-21, branch `discussions`, rinch `main` 7c964285)

`discussions/margin.rs`: each pane gives its editor the discussions on its note that are
anchored in the text and not shown inline in that pane (rinch `set_margin_anchors`,
`MarginAnchor::sticky` with the discussion's own sticky index, keyed by node id, set only
when the set changes) and hears their line boxes (`on_margin_layout`,
`margin_text_right`). The markers are the `Editor`'s overlay slot children, in a 34px
gutter right of the text (`.pimble-margin-marker`): one per line, the count of live
messages, an unread dot, the topic as its title; a press opens the first unread (or the
first) discussion beside the note (`open_beside`). "Start a Discussion Here" (Edit menu;
the toolbar's Discuss group, which stays usable for a commenter while the rest is dimmed)
anchors a new attached discussion at the caret and opens it beside. Refusals use
`StoreAccess::refusal`. Checked in the browser on the local stack (an encrypted store):
the marker beside the line, its count after a post, following its line when a line is
added above and the gutter when the pane widens, a press opening the discussion, an
inline discussion drawing none. **Desktop:** the markers are laid out but not painted:
rinch's painter skips the children of a 0x0 box, which the overlay layer is
(joeleaver/rinch#1696, being fixed in rinch). **Not built:** the popover (a marker opens
its discussion in a pane, as pins do), a context-menu item in the text (the editor has no
context menu of Pimble's yet), a "displaced" look for an anchor whose text was deleted.

**The message boxes' own keys (2026-10-21, rinch `main` 0b3c7b09, joeleaver/rinch#1686).** The
message box, the edit box and the topic line each hear their keys through `onkeydown`
(`discussions::composer_key`): Enter sends, saves or sets the topic, Shift+Enter is a new
line, Escape cancels; the keyboard interceptor is gone. Checked in the desktop app and the
browser. Safari's IME-confirm Enter (keyCode 229, not marked composing) still reaches the
handler; nothing guards it yet.
