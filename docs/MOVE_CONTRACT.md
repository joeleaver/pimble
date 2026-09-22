# Move contract: a node never leaves a share; moving it out is a delete there and a new node elsewhere

Status: written by the PM on 2026-09-21 from Joe's decision of that day. Waves 1 to 3 built on
2026-09-22 (commits 312ddd9 to ac0ddcc on `node-document`, after v0.2.0): pimble-crdt, the
server, the share upkeep and CLI, the web vault client and the shared UI, with the repair
rule narrowed to what converges ("Repair" below, `placed_under`). Wave 4, the PM's
walk-through on the local stack, is what `docs/NEXT_SESSION.md` records. Not merged to
`master` or deployed yet.

## The decision

Joe, 2026-09-21: "in a shared node, if someone moves a node out of the share, it needs to
count as an undoable delete", and, when the PM claimed a member has nowhere outside a share
to move anything: "both the desktop and the web app should allow for multiple stores to be
open at once." Both are true of the apps today, and the second makes the first unavoidable:
a member's own stores sit beside the share on their screen, and two shares of one store sit
side by side in one tree.

What is wrong today, checked in the code on 2026-09-21:
- A drag from one store onto another is ignored with a log line and nothing on screen
  (`crates/pimble-app/src/app.rs`, "cross-store move not supported yet").
- A member who holds two shares of one store can move a node from one into the other
  (`moveNode` judges the node, the list it leaves and the list it joins; all three are theirs
  to write). For the first share's other members the node vanishes and nobody can undo it.
  The owner can do the same into the unshared part of the store.
- A member's tampered client can point a node's `parent_id` at a node outside the share. The
  hosted server sees ciphertext; the owner's devices complete the move (repair lists the node
  under that parent): a member putting a node into the owner's private tree, out of every
  member's reach.

## The rule

**Inside one share a move is a move. A move that would take a node out of a share is, for
that share, an ordinary delete, and where it lands a new node is made.** The delete is the
one Pimble already has: a tombstone on the node and its subtree, which stays in the share's
scope, which every member holds, and which any editor of the share can undo
(`undeleteNode`). The new node is made once and never kept in step with the old one, so it is
not a mirror, a projection or a copy that someone maintains (the second rule of `CLAUDE.md`
forbids those; a person duplicating a note is not one). If a member undoes the delete, the
two exist side by side and go their own ways; that is the honest outcome, since the members'
document was never taken from them.

What follows from it:
- **Nothing ever leaves a scope while its share stands.** No data key ever needs rotating
  because a node left (the known limit "no data-key rotation when a node leaves" is gone:
  the new node outside has a new key by being a new document). A published scope set only
  grows until its share is stopped.
- **A move between stores is always this**, because two stores hold different documents.
  That gives the drag between stores its meaning in both apps.
- Tampering has nothing to aim at: see "Repair".

### What "out of a share" means

`shares(X)`: the nodes carrying a share marker (`custom["share"]`) among `X` and its
ancestors, read off the documents a device holds. Moving `X` under `P` **leaves a share**
when some `R` in `shares(X)` is not in `shares(P)` and `R` is not `X` itself or under `X` (a
share's own root, and the shares inside the subtree, travel with it). Entering a share is a
plain move (whoever moves a node in means its history to be read there, and the owner's
upkeep wraps its key for the share as it does today). A device judges with what it holds; a
share it holds nothing of is one it cannot write outside of either.

Consequence to know: a shared folder `R2` nested in a shared folder `R0` cannot be moved out
of `R0` as itself. Its owner stops sharing `R2`, moves it, and shares it again, or accepts
that `R2`'s members see it deleted (and can put it back) while a new, unshared folder lands
outside.

## The operation: transplant

`Tree::transplant(node, new_parent, position, now, new_id)` in `pimble-crdt`, used by the
server and by the web vault client alike (one implementation of the rule):
1. For `node` and its live subtree, in preorder, a new document each, with a fresh id: the
   same type, title, tags, custom fields, plugin `data` and content (every block and mark
   rinch's collaboration scope knows; a test per kind), created-at kept, modified now. **No
   share marker survives** on any of them (a share is its root document, its key and its
   grants; none of that is duplicated). A mount node is copied as the reference it is.
2. The new root is listed under `new_parent`; then `remove_node(node)` tombstones the
   original subtree. Created first, deleted second: a failure between the two leaves both,
   never neither.
3. Within one store it is one `TreeEdit`. Between stores it is two, one per store.

`moveNode` decides: it transplants when the move leaves a share and moves otherwise, and
**answers the id the node has now** (`MoveNodeResponse.node_id`, the old id for a plain
move). Notifications need nothing new: the server derives `NodeCreated` and `NodeDeleted`
from the documents as it does for any edit.

Between stores: `transplantNode { from_store_id, node_id, to_store_id, new_parent_id,
position }`. The caller needs `Write` on the node and the list it leaves in the first store
and on the new parent in the second, each judged in its own store (through a mount: the
source store, as everywhere). On the desktop both stores are the local server's. In the
browser the vault client does it in the page, between two stores it holds, whichever
endpoints serve them; a plain store on the hosted server and a vault store in the page is
refused with a sentence until someone needs it; two plain stores are the server's, and
two plain stores served by different servers are refused with a sentence too (a server
plants only into a store it holds; today no page sees such a pair, since a store served
from its owner's computer is always encrypted).

## Repair

`parent_id` stays the truth of where a node is, with one exception that closes the hole:
**a list that still names a node wins over a `parent_id` that was written on its own and
would take it out of a share.** Every operation that places a node (a create, a move, an
undelete, a plant, a repair's own rewrite) writes `parent_id` and, in the same
transaction, `placed_under` with the same value (`docs/NODE_DOCUMENT_CONTRACT.md`, the
`node` root). A `parent_id` that `placed_under` agrees with was placed by an operation
that also edited the lists, and every device honours it whatever other lists say; so is
one whose named parent's own list already names the node (a document from before
`placed_under` existed, or a placement whose list edit arrived first). Neither counts
for a node whose stored parent is deleted or not held: repair sends such a node to the
root, and that is judged by the lists like any other. Only a `parent_id` written on its
own, whose parent does not list the node, is judged: if
`F` lists `X`, `X.parent_id` is `Q`, `X.placed_under` is not `Q`, `Q`'s list does not
name `X`, and being under `Q` leaves a share that being under `F` is in, repair rewrites
`X.parent_id` to `F`, placed, instead of unlisting `X`. Every device that holds `F`, `X`
and the marker decides the same, from the same documents, and the rewrite is honoured by
all of them afterwards.

That bound is what keeps repair convergent, and it was learned the hard way: the first
cut judged every `parent_id` by the lists, and the randomized convergence test
(2026-09-22, seeds 76 and 1) found two devices that had nested two shared folders into
each other by concurrent moves each sitting in a consistent tree of its own and rewriting
the other's rewrite for ever, since each judged by the ancestry of the destination and by
lists that the other's repair was changing at the same time. A judgement must be made
from what is in the node's own document and be final once made.

What remains is a tampered client that also unlists `X` from `F`, or that also writes
`placed_under`, or that also appends `X` to `Q`'s list. Then the owner's devices adopt
`X` under `Q` as they do today. Because a scope set only grows, `X` is still in the
share's scope: every member still holds it and may write it, so any editor can put it
back (below), and the owner can drag it back. It is vandalism an editor could always
commit by deleting, made undoable. Joe, 2026-09-17: "if you share something, it can get
vandalized."

Share upkeep (`crates/pimble-server/src/share.rs`): a published scope set is the union of
what it was and what the root reaches now. Only stopping the share empties it.

## Seeing and undoing what was removed

There is no surface for `undeleteNode` in either app today, so "undoable" is a promise
nobody can use. Both apps get **View > "Recently Deleted..."**: per open store, the
top-most tombstones and the nodes no list names (title, the folder it was in when that is
known, when), each with "Put Back" (`undeleteNode`; for a node no list names, a move under
the share's root). A scoped member sees their scope's only. Behind it:
`listDeleted { store_id } -> { nodes: [DeletedNode { node, parent_title, deleted_at }] }`,
judged like `getChildren`; the web vault client answers it from its own tree.

After a transplant the person who did it reads one line: `"<title>" was moved out of
"<share's name>". The people it is shared with see it as deleted and can put it back.`
The open editor follows the node to its new id.

## What is built, in waves

1. `pimble-crdt`: `shares`, "leaves a share", `Tree::transplant`, the repair exception,
   content fidelity tests, and the randomized convergence test extended with transplants
   and tampered `parent_id`s.
2. `pimble-store`, `pimble-rpc`, `pimble-server`, `pimble-client`, `pimble-cli`: `moveNode`
   deciding and answering the id, `transplantNode`, `listDeleted`, scope sets that only
   grow, search index following the new ids, tests on the sharing harness (two members, a
   move between two shares: the first share's other member sees a delete and undoes it).
3. `pimble-app` and `web/`: the drag between stores and between shares, the notice, the
   editor following the id, "Recently Deleted...", the web vault client's `transplant` and
   `listDeleted`.
4. PM: the walk-through on the local stack, headless, in the desktop app and in the browser.

## Verification (the bar)

A member who holds two shares drags a note from one to the other: the first share's other
member sees it deleted, opens "Recently Deleted...", puts it back, and both notes now
exist; the owner sees the same. The owner drags a note out of a shared folder into a private
one: members see a delete they can undo; the private note has a new id and a key no member
holds. A member drags a shared note into their own store, in the desktop app and in the
browser: it lands there, and the share shows it deleted. A client that writes a `parent_id`
pointing out of the share while the folder still lists the node is corrected by every
device; one that also unlists it is undone by any editor from "Recently Deleted...". Text
with every mark and list kind survives a transplant. Nothing of a transplanted private note
is readable with a share's key.
