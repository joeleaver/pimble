# Presence contract: who is here, and where

Status: **proposed, 2026-10-22, not built.** Written from Joe's answers that day (listed
under "Decisions" at the end). The points under "To confirm" are design choices made here
that Joe has not seen yet. Section 3 (names per share) was added later the same day.

Builds on docs/IMAGE_MARKUP_CONTRACT.md sections 6 and 7 (identity is the signed-in
account; presence is designed once, for text and boards together, and is never document
data), docs/NODE_DOCUMENT_CONTRACT.md (`Reach`, scoped grants, data keys),
docs/RELAY_CONTRACT.md, docs/SYNC_CONTRACT.md (links), docs/DISCUSSIONS_CONTRACT.md
(sticky anchors, the editor overlay, @mentions waiting on this) and
docs/AGENTS_CONTRACT.md.

## The idea

When someone else has a note open, you can see it: their initials on the pane, a dot on
the tree row, their caret and selection in the text, their pointer and selection on a
board. You can follow them, and then your pane goes where theirs goes. Agents show up the
same way while they edit. People include you on your other devices.

**Presence is never stored.** It is not written to a document, a log, a snapshot, `state.json`
or any file. A server holds the latest frame of each session in memory so that someone who
arrives late sees everyone at once. The frame expires when its session leaves or goes
quiet. Nothing in presence goes through `applyEdit`.

## 1. Frames and sessions

A **session** is one pane or one agent holding one document. Two panes on the same note
are two sessions of one person. Its id is `{client_uuid}:{pane}`; an agent's is
`agent:{id}:{node}`.

A **frame** is the whole current state of one session in one document. It never sends a
difference, so a lost frame costs nothing.

```text
PresenceFrame {
  store_id, doc_id,            // in the clear: the server routes on them
  session, seq,                // seq increases per session; older or equal frames are dropped
  who: Option<Who>,            // stamped by a server (section 3), never by the sender
  body: bytes,                 // PresenceBody, sealed under the document's data key on a vault store
  ttl_ms,                      // 30 000
}

PresenceBody {
  device: String,              // "Joe's laptop", "Web": the person's own label, section 3
  focused: bool,               // this pane has the focus in its window
  left: bool,                  // a goodbye: drop the session now
  text: Option<TextPresence>,  // caret, selection, top of the viewport (section 4)
  board: Option<BoardPresence>,// pointer, selected element ids, viewport (section 5)
}
```

- A session sends a frame when its state changes, at most every 50 ms; frames in between
  are coalesced into the latest. It sends a heartbeat every 10 s and a `left` frame when
  the pane lets the document go, the window closes, or the app quits.
- Every holder drops a session 30 s after its last frame. A server also drops, at once,
  every session that arrived on a connection or link that has just closed.
- The `body` is only meaningful inside the document it names. Where someone is in the
  tree is just which documents their sessions name.

## 2. Transport

One new RPC and one notification kind, beside the change notifications. They need no new
subscription.

- `publishPresence(frame)`. It authorizes `Read` on `(store, doc)`, so a reader or a
  commenter is present too. It is allowed on every store kind, vault stores included.
  The server stamps `who` (section 3), keeps the frame in its in-memory table keyed by
  `(store, doc, session)`, and broadcasts `StoreChangeKind::Presence { doc_id, frame }` on
  `storeChanged`.
- `getPresence(store_id) -> [frame]` returns the live frames of every document the caller
  can read. It is called when a store is subscribed, and again on every reconnect.
- Delivery uses `delivery_for` with the frame's document as `Named::Document`, so a scoped
  member receives presence only for documents in their share, exactly as with edits.
  `getPresence` is filtered the same way.
- **Links** forward presence both ways, the way they forward edits.
  - In `sync_link.rs`, `vault_link.rs` and the relay link, a local `Presence` goes to the
    remote's `publishPresence`, and a remote one goes to the handler's `publish_presence_from(link_id)`.
  - Links skip frames whose source is the link itself.
  - The `seq` check is what stops echoes around a chain, where a no-op merge does that for
    edits.
  - When a link connects it sends the local live frames and fetches `getPresence`. When it
    drops, the sessions it brought expire.
- **The relay** needs nothing of its own. The relay face is an ordinary server, and the
  tunnel pipes any RPC.
- **The browser**'s vault client publishes to the store's endpoint and reads
  `Presence` notifications on the store subscription it already has.
- **Rate.** Everything rides the store channel in the first version. Frames are small
  (under 200 bytes sealed), sessions send at most 20 a second while moving, and a share
  has a handful of people. An interest filter, where carets go only to those holding the
  document, is a later step if measurement asks for one.

## 3. Who, and the name you go by in each share

### Who a frame is from

- **`who` is the account `sub`, stamped by the server that first authenticates the
  sender.** It is `Who { sub, agent: Option<String> }` and never carries a name: names
  are resolved by whoever shows the frame (below).
  - The hosted server and the relay face stamp `sub` from the JWT.
  - The app's own embedded server sees `Principal::Service`, so it stamps from its
    keystore's signed-in account. Signed out, it leaves `who` empty.
  - An agent's frames carry the `sub` of the account whose server it runs on, plus the
    agent's name in `agent`. It shows as "Claude (Sam's agent)" with Sam's name in that
    share.
  - A frame forwarded on a link keeps the `who` it has. A server receiving through a link
    authenticated as user U accepts a frame whose `who` is empty or names U. Any other
    frame is accepted only from a `Service` link (one operator's devices). So a member
    cannot appear as another member on the hosted server.
- **Colors** come from `pimble_board::author_color(sub)`, so a person has the same color on
  board elements, discussions and presence. A session with no `who` shows as its `device`
  in gray.
- **You, elsewhere.** Sessions with your own `sub` from another device show with your
  initials and the device label ("you, on Web"). Your own panes in this window show
  nothing: the app drops frames whose `client_uuid` is its own.
- **Device label.** It is the hostname on the desktop and "Web" in the browser, sealed
  inside the body.

### The name you go by (Joe, 2026-10-22)

Everyone chooses, per share they take part in, the name they appear as there. Inside a
share, that name is all the other members see of them. **Members never see each other's
emails.** Only the owner does, because the owner manages the share and invites by email.

- **Where names live.** They live in the accounts service, in the clear, like the share's
  own name. It has two parts:
  - `User.display_name` is the **default name**, used in every share that has no name of
    its own, and on your own devices.
  - A new type, `ShareName { user, store, root, name }`, is one row per (account, share).
    It is a separate row and not a field on `Grant`, because the owner holds the store
    through a whole-store grant but has a name per share too.
  - **Until you choose anything**, you show as the local part of your email. The first time
    you open a share you take part in, a one-line prompt asks "How should the people in
    <share> see you?", pre-filled with your default name.
- **Who sees what.** `GET /stores/{id}/members?root=` answers each member's name for that
  share. It answers the email only to the owner, and owners see both. Today every member
  gets every email, so that changes.
  - "Shared by <owner>", the name of a member's replica, uses the owner's name in that
    share and stops using the owner's email (`cloud.rs`, `share.rs`).
- **How a name reaches a screen.** Frames, discussion messages and board elements name
  people by `sub`. A device turns a `sub` into a name through the member list of the share
  it reaches that document through:
  - It fetches the list at connect and again whenever the grant shape changes (the
    two-minute `grant_shape` poll).
  - It caches the list beside the replica's manifest, as derived data that is rebuilt
    freely, so names still show offline.
  - A document in two nested shares shows each viewer the name used in *their* share.
  - The JWT changes nothing, so the roles model and token claims stay as they are. The
    accounts service is the authority for names. Nobody can claim another member's name,
    because a name is looked up by the `sub` a server stamped.
- **Everything in the share uses it.** That covers presence, discussion message authors,
  board author stamps ("Arrow, by Sam") and the member list.
  - Stamps written into documents (`Author { id, name }`) keep their `id`. The `name`
    written from now on is the name in effect for that document's innermost share when you
    write it, and never anything derived from an email.
  - The display already prefers the current name for a known `id` over the stamp
    (IMAGE_MARKUP_CONTRACT section 6), so renaming yourself relabels everything you made
    in that share.
  - Stamps written before this, which hold the email's local part, keep showing whatever
    they say only where the `id` is not a member any more.
- **Your own store, no share.** Your own devices show you as "you". An agent shows by its
  name.

### The Shares dialog

There is a new item, Account > "Shares...", on the desktop and in the browser.

- At the top is **Your name**, the default name field. It applies on Enter or when the
  field loses focus, as the appearance picker's tags do.
- Below it, one row per share you take part in, **shares you own included**:
  - the share's name, its owner's name, and your role;
  - an **Appear as** field, empty to use the default name;
  - **Open**, which selects the share's root in the explorer, or **Add Here** when the
    share is not on this device. Add Here is the command "Add Hosted Store..." already
    runs (`AddHostedStore`). "Add Hosted Store..." stays, for whole stores.
- It reads from a new `GET /api/v1/me/shares`, which lists owned and joined shares with
  your name in each, and writes through `PUT /api/v1/me` (`display_name`) and
  `PUT /api/v1/me/shares/{store}/{root}` (`name`). Both are account calls and go through
  the embedded server's Service-only `cloud*` RPCs on the desktop.
- Leaving a share is not part of it.

## 4. Text

- **Positions are yrs sticky indexes** into the node's `content`, the same as discussion
  anchors and personal places. Frames carry `caret`, `anchor` and `viewport_top`
  (`EditorHandle::collab_sticky_index_with`), so they survive edits that cross them in
  flight.
  - The receiving pane resolves them with `collab_locate_sticky_many`.
  - A position that no longer resolves (its text was deleted) falls back to the nearest
    one, and the caret dims until the next frame.
- **Drawing.**
  - The caret is a 2 px bar in the person's color with their initials in a small flag. It
    is drawn in the editor's overlay slot (`Editor { .. }` children, as the margin
    markers are) at `caret_rect`. Its rect is read again whenever the layout reports (the
    margin layout callback) and when a frame arrives.
  - The selection is an `Inline` decoration with a class per palette color
    (`pimble-presence--c0..c9`), from a Pimble editor plugin (`add_plugin`). Both are
    there in rinch today.
  - If a caret has to follow reflow that the margin callback does not report, ask the
    Rinch session for a general "layout changed" hook or a widget decoration that renders
    an element. Never ask for a presence concept in rinch.
- The flag shows the name for 3 s after the caret moves, then shrinks to initials.
  Hovering it shows the name and device.
- A read-only pane shows other people's carets. Its own frame carries a caret only when
  there is a selection to show.

## 5. Boards

- `BoardPresence { pointer: Option<Point>, selection: Vec<ElementId>, viewport: Rect }`
  is all in board units, the document's own terms (IMAGE_MARKUP_CONTRACT section 7).
  `pointer` is `None` when the pointer leaves the surface.
- Pointers are DOM siblings of the `RenderSurface`, placed with `board_to_screen` the way
  discussion pins are (`boards/ui.rs`). Each is an arrow in the person's color with a
  name tag. Someone else's selection is an outline in their color, drawn with the
  existing outline layer.
- A stroke being drawn is already live document data (sent every ~100 ms), so presence
  only shows the pointer.

## 6. Who is here: panes and tree rows

- **Pane title strip** (`pane_view.rs`, `pimble-pane__title`): a row of initials circles
  after the name, one per person holding the document (sessions merged by `sub`). It
  shows up to four, then "+n". Hovering one names the person and their device, and
  clicking one follows them (section 7).
- **Tree rows**: a small dot in the person's color beside the row of every node someone
  else has open, in the same indicator place as `pimble-tree-discussions`. Several people
  show as a stack of up to three dots. A collapsed row whose subtree has someone in it
  shows a hollow dot.
  - Rows snapshot at render time, so the row's `TreeNodeData` label carries the presence
    key, the way it carries the icon and paste flag.
- The store row shows nothing in the first version.

## 7. Follow

- Click a person's initials, on the pane or in the flag of their caret, to make the
  focused pane follow them. The pane's border takes their color and its title strip says
  "Following Sam · Stop".
- While following, the pane opens the document of their `focused` session whenever it
  changes. They only appear in documents you can read, so you can never be led somewhere
  you cannot go. The pane also scrolls to their `viewport_top` in text, or takes their
  board `viewport`.
- Following stops when you type, click or scroll in that pane, press Escape, or click
  Stop, or when they leave (the strip says "Sam left" for 3 s).
- You cannot follow yourself on this device. Following yourself on another device is
  allowed: it is how to watch your laptop from the desktop.

## 8. Agents

- An agent is present while it works. `pimble-mcp` publishes a frame through the local
  server after each tool call that reads or writes a node (`get_node`, every edit), with
  the caret at the end of what it wrote (`NodeDoc::sticky_at_quote` and friends) and
  `ttl_ms` 15 000. There is no heartbeat, so it fades 15 s after its last call.
- It shows like a person, in its own color, with its name and a small robot glyph in
  place of the device label. It cannot be followed in the first version.

## 9. Encryption

- On a vault store (hosted or relayed), `body` is sealed with `Blob::encrypt` under the
  document's data key (`seal_plan`'s key, so every member who can read the document can
  read where people are in it), with associated data `"{store_id}/{doc_id}/presence"`.
  The server sees which document, which session and who, but no positions, selections or
  device labels. It already sees the first three for edits.
- On a plain store the body is plain. Plain links connect one operator's servers.
- A frame whose body does not decrypt is dropped without a word. That is the frame of a
  document whose keys have not arrived yet.

## 10. Waves

0. **Names**: `display_name`, `ShareName`, the member list answering names and not
   emails to members, `GET /me/shares`, the Shares dialog, the first-open prompt, names
   resolved by `sub` everywhere a person is named (discussions, board stamps, "Shared
   by"). This wave comes first, so presence never shows an email.
1. **Transport and who is here.** `publishPresence`, `getPresence`, the `Presence`
   notification, the in-memory table with `seq` and expiry, stamping, forwarding in all
   three links, sealing. The app (desktop and web) gets the pane initials and the tree
   dots. CLI `presence <store>` lists and watches frames. Tests use the share harness
   (`tests/common/mod.rs`):
   - a member sees the owner's session through the hosted server and through the relay;
   - a scoped member sees nothing outside the share;
   - a session expires on quit, on a dropped connection and on silence;
   - a frame loop around a chain of links stops;
   - only ciphertext reaches the hosted server.
2. **Text**: carets, selections and flags.
3. **Boards**: pointers and selections.
4. **Follow.**
5. **Agents**: `pimble-mcp` publishes.

Each wave is checked in the desktop app and in the browser on the local stack
(`scripts/local-stack/`), with two accounts and a share.

## Decisions (Joe, 2026-10-22)

1. What shows: who has a note open (pane initials, tree dots), carets and selections in
   text, pointers and selections on boards, and follow mode.
2. Tiers: the same person's other devices, and share members through the hosted server
   and through the relay.
3. Agents show as present while they edit.
4. No opt-out: presence is always on.
5. Per share, everyone chooses the name they appear as. Members never see each other's
   emails (the owner does). The name applies to everything in the share. It is kept in
   the accounts service in the clear. A new Account > "Shares..." dialog holds a
   default name and one row per share, owned or joined, with Open / Add Here. There is no
   "Leave" button.

## To confirm

- Until someone chooses a name, they show as their email's local part, and a one-line
  prompt asks for a name the first time they open a share. The device label is the
  hostname.
- Names reach screens through the member list (looked up by `sub`), not through the JWT,
  so token claims do not change.
- Your own sessions on other devices show as "you, on <device>".
- Agents cannot be followed in the first version.
- Everything goes on the store channel in the first version, with no interest filter.
- The server keeps live frames in memory to answer late arrivals. They are never written
  anywhere.
