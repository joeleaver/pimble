# Notifications contract: hearing that someone wrote to you

Status: **draft, 2026-11-23, nothing built.** Written from a conversation with Joe that day
(listed under "Decisions" at the end). The points under "Open" are his to answer before
wave 1 starts; the rest are design choices made here that he has not reviewed yet.

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
Notice {                         // sealed to the recipient's X25519 account key
  id, kind: Mention | Reply,
  from: sub, from_name,          // the name the sender goes by in that share
  store, node, discussion?, anchor,
  snippet,                       // about 120 characters around the mention
  at,
}
```

- The device seals it with the recipient's public key (already held: sharing looked it
  up) and posts it to the accounts service: `POST /api/v1/inbox/{recipient sub}`. The
  service checks that the sender and the recipient share a store (no inbox spam from
  strangers), rate-limits per sender, queues it, and wakes the recipient's channels
  (section 4).
- **What the service learns:** that `from` notified `to` at `at`, about something in a
  store they share. Not where, not what.
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
- Shares shared with you: watched by default? (Open.)
- **Relayed stores**: the owner's machine runs the same evaluation on its relay face, so
  it works only while that machine is on. (Open: acceptable, or hosted stores only.)

## 4. Delivery: every channel reads the same inbox

- **Pimble running** (desktop and browser): drains the inbox at connect and when nudged
  over its existing connection, decrypts, and shows an OS notification with the snippet
  (rinch's `notifications` feature on the desktop; the Notifications API in the
  browser). Clicking it opens the note at the mention.
- **The tray** (desktop, optional): Pimble can keep running with no window open, so
  notifications arrive while you work elsewhere. (Open: on by default?)
- **Web push** (browser and phone, through the web app's service worker): the push cannot
  decrypt, because the keys live only in memory after unlocking, so it says "Joe
  mentioned you in Pimble" using `from_name`. The name has to be sent in the clear for
  that; or the push says only "You have a new notification in Pimble". (Open.)
  Tapping it opens the web app, which unlocks and shows the notice.
- **Email, the fallback**: an item no device has acknowledged within 15 minutes is
  emailed through Resend (`m.pimble.app`), at most one email per hour per person, with a
  link and no content. Devices acknowledge items as they drain them.
- **Read anywhere clears everywhere**: opening a notice writes its id into the personal
  document (DISCUSSIONS section 8), which syncs to the person's other devices; the
  service deletes delivered items after seven days.

## 5. The inbox in the app

View > "Discussions..." becomes **"Inbox..."**: mentions, replies and watched changes,
newest first, unread on top. The status bar shows a count. Each row opens where it
points and has "Unwatch" for watched changes.

## 6. Turning it off

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
1. **The inbox and the running app**: sealed notices for mentions and replies, the
   accounts service's inbox, draining on connect, OS notifications, View > "Inbox...".
2. **Out of band**: acknowledgements, email after 15 minutes, web push.
3. **Watches**: registration, the server's batching, "Watch" and "Unwatch", subtrees,
   the relay face.
4. **The tray** on the desktop.

## Decisions (Joe, 2026-11-23)

- Notifications need an out-of-band path: hearing only when Pimble is next opened is
  not enough.
- No global handles. You mention the people who can open the note, by the name they go
  by in that share; the same person can have different names in different shares.
- Mentions first; mentions in discussions are the clearest case, and mentions in notes
  use the same atom.

## Open (Joe's to answer)

1. **The hosting rule.** Do sealed notices to someone's account, and a list of watched
   document ids, count as account data that signing in implies (like read state, with a
   switch), or must a person opt in to Pimble Cloud before notifications work? Both
   apply to relayed and unhosted stores too.
2. **Email content.** Content-free with a link (this draft), or may the sender's device
   include a snippet in the clear?
3. **Web push text.** `from_name` in the clear ("Joe mentioned you"), or nothing at all?
4. **Watches on relayed stores.** Only while the owner's machine is on, or hosted stores
   only for now?
5. **Default watches.** Are shares shared with you watched by default?
6. **The tray.** Should Pimble keep running in the tray with no window open by default
   (needed for desktop notifications without the browser)?
