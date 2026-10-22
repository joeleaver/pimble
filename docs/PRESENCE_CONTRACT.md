# Presence contract: who is here, and where

Status: **waves 0 (names), 1 (transport, who is here) and 2 (carets and selections in text) built on branch `presence`, 2026-10-22; waves 3 to 5 not built.** Written from Joe's answers that day (listed
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

## Built in wave 0 (names), 2026-10-22, branch `presence`

**Accounts service** (`crates/pimble-cloud`):
- `User.display_name` is optional, and `UserRow::default_name` falls back to the
  email's local part.
- `ShareName { user, store, root, name }` is keyed per (user, store, root), where `root`
  is `""` for a whole store. Rows are deleted with the store and with the account.
  `routes/people.rs` serves:
  - `PUT /me` (`display_name`)
  - `GET /me/shares`, which lists owned and joined shares with `my_name` and
    `appears_as`
  - `PUT /me/shares/{store}` (`root`, `name`; empty clears)
- `GET /me` answers `display_name` and `name`.
- `GET /stores/{id}/members` answers:
  - `name` on each member, and `email` only to an owner (`""` otherwise, so older
    clients still parse);
  - `people`, everyone reaching the scope, owners included, as id, name and role.
- `GET /stores`' `shared_by` is the owner's name in that share.
- Names are at most 80 characters, on one line.
- Tested by `names_per_share_and_no_emails_between_members`, against a real
  rhypedb.

**The desktop's server** (`pimble-server`):
- `crate::people` keeps `<store>/people.json`: per scope the store is held through,
  each person's id and name, plus `my_name` and `my_name_chosen`. It is derived data,
  never synced.
- `people.json` is refreshed:
  - at every vault-link connect and on its two-minute grant check (spawned, so it does
    not hold the link up);
  - on `cloudPeople`, which answers the cache when the service cannot be reached;
  - after a name is set.
- A refresh that changes anything sends `StoreChangeKind::PeopleChanged`, which links
  never forward.
- New Service-only RPCs:
  - `cloudListShares`, whose rows carry `here`;
  - `cloudSetDisplayName`;
  - `cloudSetShareName`;
  - `cloudPeople`.
- `ShareMember` carries `name`. The Share dialog's owner row is found by user id, no
  longer by email.
- Tested by `members_go_by_the_names_they_choose_and_see_no_emails` and the updated
  `a_member_can_neither_share_from_a_share_nor_change_it`, against the stub accounts
  service. `sharing_against_the_real_accounts_service` passes too.

**The app** (both targets, `crates/pimble-app/src/shares.rs`):
- The Shares dialog is Account > "Shares..." on the desktop and File > "Shares..." in
  the browser. It has "Your name" (applied when the field commits), one row per share
  with "Appear as", and Open (follows a `pimble:` link to the share's root) or Add Here
  (`CloudAddHostedStore`).
- The one-line question opens when a share somebody else made arrives with
  `my_name_chosen` false, once per share per run. Done saves the name as typed
  (pre-filled with the default), so the question is not asked again on another device.
- Who is who is kept in `SharesUi::people`. `Names::of(store, node)` resolves an id
  through the share the node is in (`scope_root_of` walks up to a scope root).
  Discussion threads (`relabel`) and the board hover ("by Sam") show current names.
- Stamps:
  - `author_in(store, node)` stamps board elements, pictures in text and discussion
    messages with the name used in that share.
  - Outside shares the stamp is the default name (`set_author_name`, from the first
    `CloudSharesListed` after sign-in). The email's local part is only used until that
    first list arrives.
- Share dialog rows read "Name (email)" for an owner and "Name" for a member.
- The browser answers the four commands from the accounts service itself
  (`web/src/backend.rs` `names_command`) under the same events.

**Gaps:**
- The browser learns other people's new names only when it asks again (a store
  registered, its own name set). There is no `PeopleChanged` there.
- The discussions list row ("Alice: the latest message") shows the stamped name.
- The invitation and "shared with you" mails still name the inviter by email; that is
  for Joe to decide.
- Stamps written before this wave keep the email's local part in the document. Only
  the display is corrected, for people who are still members.
- The owner is never asked the one-line question; their default name applies until
  they set one in Shares.

**Checked on the local stack** (2026-10-22): Alice hosted `Family`, shared `Holiday` as
"Holiday Plans" and invited Bob.
- `people.json` on Bob's disk names "alice" and "bob" and holds no email.
- Bob's desktop app (over the debug port) asked the question on the first open; "Bobby"
  was saved.
- Account > "Shares..." listed "Holiday Plans · shared by alice", Editor, "Bobby". A
  default name ("Robert") applied on Enter, and Open opened the share.
- In the browser as Bob, File > "Shares..." showed the same values and asked no question.
  "Bob B." typed there reached Alice's `cloud-share-info` as `Bob B. <bob@example.com>`.
  She sees the email because she is the owner.
- The question now names the share by its own name (`ScopePeople.share_name`), no longer
  by its root note's label.
- Not tried by hand: discussion authors and the board hover under the new names.

## Built in wave 1 (transport, who is here), 2026-10-22, branch `presence`

**RPC** (`pimble-rpc`):
- `PresenceFrame` (`store_id`, `doc_id`, `session`, `seq`, `who`, `left`, `body`,
  `sealed`, `ttl_ms`) and `PresenceBody { device, focused }`, base64url JSON.
- `PresenceWho { sub, agent }`.
- `publishPresence { frame, client_id }`, `getPresence(store)`, and a subscription of
  its own, `subscribePresence(store, client_id)` => `presenceChanged`. It is separate
  from `storeChanged`, so older clients never see a kind they cannot parse.
- `PRESENCE_HEARTBEAT_MS` is 10 s and `PRESENCE_TTL_MS` is 30 s.

**Server** (`crate::presence`):
- The hub holds each session's latest frame in memory. It drops a frame not newer
  than the one held (`seq`), which is what stops loops through links. A `ttl_ms` is
  capped at 60 s, and a sweep every 5 s ends quiet sessions with a goodbye in their
  name.
- Delivery goes to each subscriber whose grant reaches the document (`scope_set`),
  never back to the client it came from.
- A subscriber's sink is owned by one task, fed by a channel: jsonrpsee deregisters
  a subscription when any clone of its sink drops. When the subscription closes,
  every session its client brought into that store ends at once.
- Origins are keyed `sub/client` for users, so one account cannot end another's
  sessions.
- Stamping: `Service` keeps `who`, or stamps the keystore's account. An agent is
  stamped as the account plus its name. A user is stamped as themself, and a frame
  naming someone else is refused.
- `crate::presence_link` runs beside each connected sync link (plain) and vault link
  (sealed). It sends what is held here and fetches what is there on connect, then
  carries frames both ways.
  - On a vault link it seals bodies under the document's data key (asked of the
    remote with `vaultFetch` after `u64::MAX`, cached per connection) with
    `pimble_crypto::presence_aad`, and opens them on the way in.
  - Dropping it with the connection ends the sessions it brought.
- Tested by `tests/presence.rs`, which runs a member against the hosted server
  through sealed vault links:
  - presence crosses, stamped and opened;
  - a note outside the share never arrives;
  - nothing is written on the hosted disk;
  - a dropped client sends a goodbye;
  - an older frame goes nowhere.
  The hub has unit tests too.

**App** (`crates/pimble-app/src/presence.rs`, both targets):
- Each pane holding a document is a session `{client}:{pane}`. An effect over the
  panes and the focus sends a frame on change and a goodbye on letting go, and a
  heartbeat every 10 s resends and prunes. `frame_for` builds the frame, with `seq`
  from the clock.
- `AppStore::presence` holds others' sessions, never this client's own (the backend
  filters by client).
- `people_in` merges sessions by account. Names come from section 3: "You" on
  another device, an agent by name, someone signed out by device. The color is
  `author_color(sub)`.
- The pane strip shows up to four initials with "+n"; each has a hover with the name
  and devices. The tree row shows up to three dots.
- The browser's vault client subscribes to presence with each store (so it is renewed
  with every new socket), seals with `encrypt_key` and opens in `pump`. A store whose
  owner is offline takes presence silently.
- CLI: `presence <store> [--watch]`.

**Checked on the local stack**: Alice's desktop app and Bob's browser in a shared note.
- Alice's pane was published from her machine as `on joe-laptop, focused`; the hosted
  server held it `(sealed)`.
- Bob's tree showed a dot on the note, and his pane showed "A" with the hover "alice,
  on joe-laptop".
- After Bob answered the question with "Bobby", Alice's pane and tree row showed "B".
- Closing Bob's tab removed him from Alice's view within a second.

**Gaps:**
- No hollow dot on a collapsed row with someone below it.
- The browser's plain (non-encrypted) stores do not renew their presence subscription
  when the page's socket is replaced.
- A document from before data keys carries no presence over an encrypted link.
- The plain sync link and the relay path are not tried by hand. The relay face is an
  ordinary server, and the plain link shares the vault link's code path except for
  sealing.
- The pane's initials sit at the right end of the strip, beside the split buttons,
  because the name takes the free width.

## Built in wave 2 (carets and selections in text), 2026-10-22, branch `presence`

**Wire** (`pimble-rpc`):
- `PresenceBody.text: Option<TextPresence>` (serde default, skipped when `None`, so wave 1
  bodies parse and wave 1 readers ignore it).
- `TextPresence { caret, anchor, viewport_top }`, each a base64url sticky index from
  rinch `collab_sticky_index_with(_, After)` into the node's `content`. `anchor` is only
  there when something is selected (`TextPresence::new` drops one equal to the caret).
- The app's `BackendCommand::PublishPresence` carries `text`, and `presence::frame_for`
  puts it in the body, so desktop and web build frames on one path. The browser seals
  the whole body as before.
- The CLI's `presence` says "a caret in the text" or "a selection in the text".

**Saying where this window is** (`crates/pimble-app/src/presence/text.rs`):
- `mine(pane)` is the pane's selection as sticky indexes. It is `None` for a pane
  that holds no note, is not collaborating, or may only read it and has nothing
  selected.
- `moved` is called from the editor's `on_selection_change`, and once a note opens.
  It sends a frame only when the text changed, at most one per 50 ms: a move inside
  the gap waits for its end, and the frame then says where the caret is by then.
- `presence::say` and the heartbeat carry the current text too.

**Showing everyone else**:
- `redraw(pane)` resolves the carets and anchors of every session in the pane's note
  with one `collab_locate_sticky_many`.
- `place` turns those answers into positions:
  - a caret whose block is gone is at rinch's nearest place, dimmed;
  - a caret that does not resolve at all (an edit not merged yet) is drawn at the
    last place it resolved in this pane, clamped and dimmed;
  - a caret never seen is not drawn;
  - a selection needs both ends.
- A caret is a 2 px bar in `author_color(sub)`, placed with `position: fixed` at
  `caret_rect` in the window frame, as the picture controls are. Carets scrolled out
  of the text's box are not drawn.
  - The flag above it shows the name for 3 s after the caret moves
    (`Present.moved_ms`, set when a frame brings a different caret or anchor), then
    the initials. Hovering it gives the name and device.
- A selection is an `Inline` decoration with `pimble-presence-selection
  pimble-presence--c{n}`, where `n` is `pimble_board::author_color_index`.
  - It comes from `PresencePlugin`, added to every pane's editor beside the links
    and pictures plugins.
  - Ranges are handed over in a meta-only transaction (`EditorHandle::update`, which
    a read-only editor accepts because it changes nothing). They are mapped through
    every later transaction until the next arrive, and clamped to the document.
- The carets are read again a turn after each of these (`redraw_later`, once however
  many ask):
  - a frame arrives or lapses;
  - a peer's edit to the note is applied (`editor::apply_remote`);
  - the pane's own caret moves (`on_caret_moved`: typing shifts carets along its
    line);
  - the text scrolls;
  - the margin reports.
- The carets are margin anchors (key `presence`, combined with the discussions' in
  `discussions/margin.rs` `give`), so a reflow that moves their lines (a peer's
  edit above, a resize) is reported like a discussion marker's line. The markers
  skip that key.
- No rinch change was needed: `caret_rect`, the margin report, decorations and
  `update` covered it.

**Tests**:
- `presence_body_round_trips_with_text` (`pimble-rpc`): the round trip, a wave 1 body,
  and a selection of nothing.
- `presence::text` tests: `place` for resolved, gone, unseen and half-resolved
  positions.
- `a_peers_caret_and_selection_resolve_and_follow_edits`: a sticky index made by one
  editor is found by a collaborating peer's, follows typing on both sides through a
  body round trip, and the plugin draws the selection given and carries it through an
  edit.

**Checked on the local stack** (2026-10-22): Alice's desktop app (17491) and Bob's
browser in the shared "Holiday Plans" note.
- Bob's page showed Alice's caret with an "A" flag at the start of the line. When she
  selected "Sunday" on the desktop, his page drew `pimble-presence--c7` over "Sunday"
  and her caret at its start; the flag's hover read "alice, on joe-laptop".
- Bob's selection drew in Alice's pane as a brown tint with his caret and a "B" flag.
- Alice typed "Note: " at the start of the line, and Bob's caret moved along with the
  words.
- Bob typed, and Alice's flag read "bob", then "B" about 3 s later.
- When Alice's caret moved after her typing, Bob's page followed it, and her selection
  was gone.
- Not tried by hand: a read-only member's pane, a caret dimmed by a deleted paragraph
  (unit-tested), and two windows of the desktop app.

**Gaps:**
- `viewport_top` is never filled. Finding the first position in view needs a point
  hit-test the editor does not offer. Follow (wave 4) needs it, so it waits for that
  wave.
- Clicking a caret's flag does nothing yet; following is wave 4.
- In the browser the text's box is found by `data-pimble-text` and
  `getBoundingClientRect`, since rinch-web fills no bounds.
