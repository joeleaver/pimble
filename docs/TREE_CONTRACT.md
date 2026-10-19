# Tree contract: the explorer you can arrange by hand and by keyboard

Status: drafted by the PM on 2026-10-19 from Joe's request ("easier to reorder, outdent,
see where nodes are going to end up ... I want to _really_ have the best tree UX") and a
review of the code at `sheets` (`7d02a93`, rinch `2ae3577`). Joe answered the open
questions on 2026-10-19 ("Answers" at the end): arrows open as they go, Alt+Shift+arrows
move, a click keeps the keyboard in the tree, outdent leaves the following siblings alone,
one refusal refuses a multi-move, and `Place` replaces `position` outright. Wave 1 started
on 2026-10-19.

## Where it stands (the review, 2026-10-19)

- rinch's `Tree` (`rinch-components/src/tree.rs`) draws nested `li` rows, owns expanded and
  selected sets (`UseTreeReturn`), and hands each row to the app (`render_node`). It has no
  keyboard handling, no drag and drop, no drop positions, and no virtualization.
  `UseTreeOptions::multiple` exists; nothing reads modifiers on a click.
- Pimble's row (`app.rs`, `render_node_fn`) adds drag and drop on `DragContext`. A drop is
  always **into** the row: `decide_drop` sends `MoveNode { position: None }` (last
  child). `Tree::move_node`, `moveNode` and `transplantNode` already take a position; no
  caller passes one.
- So today: no reorder, no outdent, no keyboard (arrows do nothing), one node at a time,
  no hover-to-expand, no auto-scroll, no Esc to cancel a drag, no undo of a move. The
  only drop feedback is a `--rinch-color-blue-1` fill that washes the label out on the
  dark theme. A refused drop (read-only, mount) is found out only after letting go.
- Every row's style closure reads `store.drop_target`, so each dragenter re-runs one
  effect per drawn row.

## The decisions (proposed)

1. **A drop has a place, not just a parent.** Every drop lands before, inside or after a
   row. Dragging toward the left at the end of a group lands it at a shallower depth, so a
   drag can outdent.
2. **Where it will land is shown before you let go**, in two ways: a line or an outline
   in the tree, and one sentence at the foot of the explorer that names the destination
   ("Into MEDICAL, after Boo Kitty Medical"). A drop that would be refused is shown as
   refused, with its reason, while dragging.
3. **Positions travel as an anchor, never as an index.** `Place::{First, Last,
   After(NodeId)}` replaces `position: Option<usize>` everywhere (RPC, `BackendCommand`,
   `Tree`, the web vault client, the CLI).
4. **The tree has the keyboard when a row was the last thing pressed or focused**, and
   then it follows the WAI-ARIA tree pattern, with Alt+Shift+arrows to move nodes: Left
   and Right outdent and indent, Up and Down reorder (Joe, 2026-10-19). Tab keeps moving
   focus, as everywhere else.
5. **Three distinct things**: the *open row* (the focused pane's document, as today), the
   *selection* (a set, for acting on several at once) and the *cursor* (the row the
   keyboard is on). A plain click sets all three, and so does an arrow: **the document
   follows the cursor** (Joe, 2026-10-19, as Scrivener's binder does). Enter hands the
   keyboard to the document.
6. **Several rows move as one.** Dragging a selected row drags the whole selection, in
   tree order. A selected node whose ancestor is also selected travels with that ancestor
   and is not moved on its own.
7. **Every tree action can be undone** with Ctrl+Z while the tree has the keyboard.
8. **General mechanisms go to rinch; Pimble's meaning stays in Pimble** (Joe, 2026-10-10).
   Rinch's Tree learns keyboard navigation, a cursor, multi-select and drop positions. It
   asks the app whether a drop is allowed and what to call it, and never knows about
   stores, shares or mounts.
9. **The desktop and the browser are the same** (Joe, 2026-10-06): everything here lives
   in shared `pimble-app` code and rinch's Tree, which both backends draw.

## The drop model

### Zones

While dragging over a row, its height is split into three bands (the row's bounds come
from the `ondragover` click context on both backends):

| Pointer in the row | Place | Shown as |
| --- | --- | --- |
| top quarter | before the row | a line above the row, at the row's depth |
| middle half | inside the row, last | an outline around the row |
| bottom quarter | after the row (see below) | a line below the row, at the chosen depth |

- **"After" on an expanded row with children** means *first inside it*: the line under
  the parent row is drawn above its first child, so that is where it goes.
- **"After" on the last row of a group** offers every depth that ends there. If the next
  visible row is shallower than this one, the line's depth follows the pointer's x
  position, from this row's depth up to the next row's depth, one indent per step. The
  place is `After(ancestor at that depth)`. This is the drag that outdents.
- **"Before" a row** is the same spot as "after" the previous visible row, and the two
  resolve to the same `Place`, so the indicator never jumps between them.
- **A store row** takes only "inside", which means the root's last child. Reordering the
  stores themselves is out of scope.
- **The dragged rows and their subtrees** are not targets. Nothing is drawn over them.

### Feedback

- **The indicator is one element** drawn by the Tree over its container: a 2 px line in
  the primary color with a small circle at its left end, starting at the target depth's
  indent; or a 1.5 px primary outline for "inside". It is moved by changing its style, so
  rows never re-render during a drag (no per-row `drop_target` effects). It uses rinch's
  semantic color variables only, so both themes read (the review's washed-out label is a
  bug today).
- **The destination sentence** sits in a strip at the foot of the explorer for the whole
  drag, written by Pimble:
  - "Into MEDICAL" / "Into MEDICAL, first" / "In MEDICAL, after Boo Kitty Medical" /
    "Top of Family Management".
  - Several rows: "3 items into MEDICAL".
  - Another store: "To Other Store, as a new node. It leaves Family Management (Put Back
    in Recently Deleted)." (docs/MOVE_CONTRACT.md: a move between stores is a delete and a
    new node.)
  - Leaving a share: "Leaves the share Taxes 2025: its members will see it deleted and can
    put it back." (MOVE_CONTRACT "What 'out of a share' means".)
  - Refused, in place of the sentence and with the indicator in the error color: the
    refusal sentences the app already has (`StoreAccess::READ_ONLY_REFUSAL`), plus "A node
    can't go inside itself." and "Drop inside the mounted store instead: a mount holds no
    nodes of its own." Letting go over a refused place does nothing and shows no notice
    (the strip already said why).
  - As built (wave 1): a drop into a mount, or among the rows drawn under it, lands in
    the node it mounts (a move in that store, or a transplant from another), since those
    are the children drawn there; only a mount whose store is not open here refuses ("This
    mount's store isn't open here."). Beside a store row: "Nodes go inside a store, not
    beside it." A drop exactly where the node is says "Where it is" and sends nothing.
    Store rows do not drag.
- **The dragged rows** stay at 0.4 opacity in place. A count badge follows the pointer
  when more than one row is dragged.

### Getting there

- **Hover to expand**: resting in the "inside" band of a collapsed row with children for
  600 ms expands it. Rows expanded this way stay expanded after the drop (the person can see
  where the node went).
- **Auto-scroll**: within 32 px of the tree's top or bottom edge, the tree scrolls, faster
  the closer the pointer gets.
- **Esc cancels the drag**, and so does letting go outside the tree. Nothing is sent.
- **After a drop** the moved rows are selected, the cursor is on the first one, and it is
  scrolled into view. A move that becomes a transplant selects the new node(s) by the ids
  `moveNode`/`transplantNode` answer.

### What a drop sends

`TreeDrop { values: Vec<String>, target: String, place: DropPlace }` comes out of rinch
(tree values, not Pimble ids). Pimble resolves each value with `parse_tree_value`, makes
the selection normal (decision 6), and judges each move with today's `decide_drop` grown
into `judge_drop`. That function answers `Allowed { sentence }` or `Refused(sentence)` for
the strip during the drag, and the commands on drop. Several nodes go as one
`moveNodes` request (below), so one undo step and one repair cover them.

## Places on the wire

```rust
pub enum Place { First, Last, After(NodeId) }   // serde: "first" | "last" | { "after": id }
```

- `MoveNodeRequest`, `TransplantNodeRequest`, `BackendCommand::{MoveNode, TransplantNode}`,
  `Tree::{move_node, move_or_transplant, plant}` and the web vault client take
  `place: Place` (default `Last`) in place of `position`. The CLI's `move-node` gains
  `--first` and `--after <id>`.
- **Resolved where the edit is made**, against the list as it is then: `After(x)` inserts
  right after `x`, and after `x`'s last entry if a damaged list names it twice. `After(x)`
  with `x` no longer in the list means `Last`: someone moved `x` away meanwhile, and the end
  is the honest place. In a move within one parent, the node is taken out first and the
  anchor is found afterward, so there is no off-by-one.
- A partial replica's list may name nodes it does not hold. An anchor is an id, so that
  changes nothing.
- **`moveNodes { store_id, node_ids, new_parent_id, place }`**: moves in the given order,
  each one after the previous (the first at `place`). It answers per node, like `moveNode`
  (the id it has now, the shares it left). Every node is judged before any is moved; one
  refusal refuses all, with that node's sentence. A selection that spans stores is sent as
  one `moveNodes` per source store, plus transplants for nodes going to another store.

## The keyboard

**Who has it.** A press on a row focuses the row (rows already carry `tabindex`), and the
Tree holds a `push_key_handler` on its container. That is the mechanism rinch's `Select`
uses: keys reach the tree only while focus is inside it, so an editor or a field keeps its
own keys. View > "Focus Explorer" (Ctrl+Shift+E) gives the tree the keyboard from
anywhere. Esc from the tree with nothing to cancel gives the keyboard back to the focused
pane's editor (`editor::give_keyboard_to`). A click on a row keeps the keyboard in the
tree (Joe, 2026-10-19); Enter or a click in the editor moves it.

**The document follows the cursor.** Up, Down, Home, End, PageUp, PageDown, Left to the
parent, Right to the first child and type-ahead each open the row they land on in the
focused pane, once the cursor has rested for 120 ms. Holding Down through forty rows
therefore opens one document, not forty; the row highlights at once and the document
follows. A folder opens as a folder does on a click. The keyboard stays in the tree
throughout. Shift+arrows only extend the selection and open nothing.

| Key | Does |
| --- | --- |
| Up / Down | cursor to the previous / next visible row, which becomes the selection and opens |
| Shift+Up / Shift+Down | extend the selection from the anchor |
| Home / End, PageUp / PageDown | first / last visible row; a page of rows |
| Right | expand; if already expanded, cursor to the first child |
| Left | collapse; if collapsed or a leaf, cursor to the parent |
| `*` | expand every sibling of the cursor row (ARIA) |
| Enter | give the keyboard to the cursor row's document (opening it first if the 120 ms has not passed) |
| Ctrl+Enter / Alt+Enter | open in split view |
| F2 | rename (also double-click, as today) |
| Ctrl+Shift+Enter / Insert | new sibling after the cursor, renaming at once |
| Ctrl+Alt+Enter | new child (last) of the cursor, renaming at once |
| Alt+Shift+Right / Alt+Shift+Left | indent / outdent the selection |
| Alt+Shift+Up / Alt+Shift+Down | move the selection up / down among its siblings |
| Delete | delete the selection (tombstones, undoable) |
| Ctrl+X, then Ctrl+V | cut, then paste after the cursor (Ctrl+Shift+V: into it, last) |
| Ctrl+C | copy link(s), as "Copy Link" |
| Ctrl+A | select every visible row at the cursor's level under its parent |
| Ctrl+Z / Ctrl+Shift+Z | undo / redo the last tree action |
| Esc | cancel rename, drag or cut; else clear a multi-selection; else back to the editor |
| letters | type-ahead: the next visible row whose title starts with what was typed in the last 700 ms |

**What the moves mean** (all of them are `Place` moves, judged by `judge_drop` like a drag,
and refused with the same sentences):

- **Indent**: the node becomes the last child of its previous sibling, which expands. With
  no previous sibling, nothing happens.
- **Outdent**: the node becomes the next sibling of its parent (`After(parent)` in the
  grandparent), and its own children go with it. The siblings after it stay under the old
  parent; they do not become its children (Joe, 2026-10-19). At the top level of a store,
  nothing happens.
- **Alt+Shift+Up / Alt+Shift+Down**: swaps with the previous / next sibling and never leaves the
  parent. At either end, nothing happens.
- Several selected nodes do it together and keep their order. Indent goes under the
  previous sibling of the first one.
- **Through a mount** the tree as displayed is what counts. A move that would cross the
  mount's edge (outdenting the top level of a mounted subtree) is refused with "This is the
  top of a mounted store: drag it to move it to another store." Inside the mount, moves act
  on the source store as everywhere else.

## Multi-select

- Click: select only this row, cursor here, anchor here, open it (as today).
- Ctrl+click: add or remove this row; cursor and anchor here; opens nothing.
- Shift+click: select the visible rows from the anchor to here; opens nothing.
- The open row keeps today's accent fill. Other selected rows get a lighter fill; the
  cursor gets a focus ring. With one row selected and opened, it looks as it does today.
- The context menu on a selected row acts on the whole selection where that makes sense
  (Delete, Appearance, Copy Link) and shows the count ("Delete 3 Nodes"). Items that act on
  one node (Rename, Share, Open in Split View) are disabled with several selected.
- **Delete** of several is one request per store and one undo step.
- The selection is per window and is not saved. Collapsing a parent drops its hidden
  descendants from the selection.

## Undo

A per-window stack of the last 50 tree actions (moves, indents, outdents, reorders,
deletes, creates, renames from the tree), cleared on restart.

- A move undoes as a move back to `(old parent, After(old previous sibling))`, or `First`.
- A delete undoes as `undeleteNode`.
- A create undoes as a delete.
- A move that became a transplant (it left a share or went to another store) undoes as
  `undeleteNode` of the original plus a delete of the new node.
- An undo that can no longer apply (someone else deleted or moved the node since) does
  nothing and says so in one sentence. It never guesses.
- Undo while the tree has the keyboard is the tree's. In an editor it stays the editor's.

## Seeing the tree

- **Indent guides**: one faint vertical line per level, at the chevron's center. The
  guide of the cursor row's parent is a little stronger (VS Code's active guide).
- **Indent** 12 px per level (today it is `xs`), so the depths a drag offers are big enough
  to aim at.
- **Full titles**: a clipped label shows the whole title on hover (a `title` attribute).
- View > "Collapse All" and a store row's "Collapse All" / "Expand All" (one level of
  fetches at a time).
- View > "Reveal Open Document" (Alt+L) runs `reveal_in_tree` on the focused pane's
  document, which already exists for links.

## Rinch and Pimble

**rinch (pull requests on `joeleaver/rinch`; general, no Pimble names):**

- R1 **Cursor and keyboard**: `UseTreeReturn.cursor: Signal<Option<String>>`, roving
  `tabindex`, the ARIA keys (arrows, Home/End, PageUp/Down, Right/Left, `*`, type-ahead)
  through `push_key_handler` on the container, and a hook
  `onkey: Fn(&TreeKey { value, key, modifiers }) -> bool` that runs first, so an app binds
  its own keys (Alt+Shift+arrows, F2, Delete) without the Tree knowing what they mean.
  `oncursor(value)` once the cursor has rested (the delay is a prop, 120 ms by default),
  which is what an app that wants the document to follow the cursor listens to.
  `controller.visible_values()` (visible order from the data and the expanded set).
- R2 **Multi-select**: click modifiers (`ClickContext.modifiers`) with an anchor, Shift
  ranges in visible order, `onselectionchange(Vec<String>)`. The existing `onselect`
  stays a plain click.
- R3 **Drop positions**: `draggable` rows on the Tree itself; `DropPlace::{Before, Inside,
  After { depth }}` computed from the drag-over bounds and pointer x; the one-element
  indicator; hover-to-expand; auto-scroll of the tree's scroll container; Esc cancels (a
  general "cancel the active drag" in `rinch-core` if there is none); the hooks
  `judge_drop: Fn(&[String], &str, DropPlace) -> DropVerdict { Allowed, Refused }` and
  `ondrop(TreeDrop)`. The app draws its own text (the strip); the Tree only colors the
  indicator by the verdict.
- R4 **Small things**: indent guides in the Tree's stylesheet, a `level_offset` in pixels,
  and `title` on clipped labels.
- Later, only if a measured tree needs it: windowed rendering (only the visible rows
  drawn), which needs the Tree to draw a flat list rather than nested `li`.

**Pimble:**

- P1 `Place` through `pimble-rpc`, `pimble-crdt` (`Tree`), `pimble-server`, `protocol.rs`,
  `commands.rs`, `web/src/vault.rs` and `pimble-cli`, plus `moveNodes`.
- P2 `judge_drop` (from `decide_drop`): access, self-or-descendant, mounts, stores,
  leaving a share, with the sentences above; pure and unit-tested like `decide_drop`.
- P3 the destination strip at the foot of the explorer.
- P4 the key bindings through R1's `onkey`: indent, outdent, reorder, new sibling or child,
  F2, Delete, cut and paste, Ctrl+C, Focus Explorer, Reveal Open Document, Collapse All.
- P5 open row, selection and cursor in `AppStore` (`selected_id` stays the open row:
  `select_in_pane`/`focus_pane` keep writing it), and the context menu over a selection.
- P6 the undo stack.
- P7 the row styles on semantic variables only; drop the per-row `drop_target` effect once
  R3's indicator lands. The dark-theme drop fill can be fixed on its own today.

## Waves

1. **Places and drops** (R3, R4, P1 without `moveNodes`, P2, P3, P7): reorder and outdent by drag, with the
   indicator and the strip. This is the main complaint.
2. **Keyboard** (R1, P4, and P5's cursor).
3. **Multi-select** (R2, `moveNodes`, the rest of P5).
4. **Undo and the rest** (P6, type-ahead, Collapse All, Reveal); windowing only if
   measured.

## Built in wave 1 (2026-10-19, branch `tree`, not merged)

- **`Place`** (`pimble_core::Place`) replaces `position` in `moveNode`, `transplantNode`,
  `Tree::{move_node, move_or_transplant, transplant, plant}`, the store, the client, the
  app's commands, the web vault client, the CLI (`move-node --first | --after <id>`) and
  the MCP `move_node` tool (`after`, `first`). `NodeDoc::list_place` resolves it; after
  itself is where it is. `add_node` (creating) still takes an index; wave 2's "new sibling
  after the cursor" moves it to `Place`. The convergence test moves to every kind of place
  (600 seeds converge).
- **rinch** (`joeleaver/rinch` branch `feat/tree-drop-places`; Pimble's `Cargo.toml`s point
  at it until it merges): `Tree` takes `draggable`, `drag_values`, `judge_drop`,
  `ondropchange`, `ondrop`, `ondragstart`, `ondragend`, `indent`, `hover_expand_ms`,
  `guides`, and reports `TreeDrop { dragged, parent, slot: First | Last | After, over, zone }`.
  The drop is drawn on the row under the pointer as classes and an `::after` at
  `--tree-drop-level` (not one moving element: a row redrawn mid-drag would have taken
  it away, and rinch-web retires a discarded subtree). A dragged row's own top and bottom
  edges are places, so the last row of a group drags out of it over itself. A drop ends
  the drag (the source row a drag's end goes to is often gone by then).
- **Pimble** (`crates/pimble-app/src/tree_drop.rs`): `plan_drop` is the one judgement
  (the Tree's `judge_drop`, the strip, and the release all use it), with `judge` pure and
  tested. The strip is `.pimble-drop-hint` at the foot of the explorer
  (`AppStore::drop_hint`, which replaced `drop_target`). Indent 12 px, guides on. A row
  dragged onto a board still becomes a node card (the Tree's `ondragstart` fills the
  board's drag context).
- **Verified on the desktop** (an isolated app, the family store copy, dark and light
  theme): reorder, outdent by drag (onto the next group's top edge, and over the row's own
  bottom edge), into a collapsed row with hover-to-expand, the indicator surviving the
  row's redraw, a refused drop beside a store row (red line, red sentence, nothing sent),
  "Where it is" sending nothing, Escape cancelling, nothing left dimmed after a drop, a row
  onto a whiteboard.
- **Not built yet**: auto-scroll near the tree's edges (it belongs in rinch's drag
  machinery for every scroll container, a separate PR), the count badge (one row drags
  until wave 3), selecting the moved rows after a drop, and the browser walk-through
  (below, when run).

## Out of scope

Reordering stores in the explorer; duplicating a node (a separate decision: its content
would be a fresh projection, as a transplant's is); dragging rows into an editor as
links; touch-specific gestures beyond what rinch's drag gives a pointer; sorting a folder
by name or date.

## Answers (Joe, 2026-10-19)

1. **Arrows open as they go**: yes. See "The document follows the cursor".
2. **Moving keys**: Alt+Shift+arrows. Tab is left alone.
3. **A click keeps the keyboard in the tree**: yes.
4. **Outdent**: the following siblings stay where they are and do not become the
   outdented node's children.
5. **One refusal refuses the whole multi-move**: Joe asked for a recommendation. *PM
   recommends yes.* A refusal shows in the strip while you are still dragging, so you can
   see it and change the selection before letting go. A partial move would scatter one
   selection between two places with nothing on screen saying which nodes stayed behind,
   and undo is cleaner when a step either happened or did not. A node left out because its
   ancestor is also selected is not a refusal: it travels inside that ancestor. **Decided
   (Joe, 2026-10-19).**
6. **`Place` replaces `position` outright**: Joe did not know. *PM recommends replacing
   it.* Today no one passes a position except the MCP server's `move` tool, where an LLM
   may give an index. The UI always sends `None` and the CLI takes none. The RPC types do
   not deny unknown fields, so a mismatch of versions fails safe:
   - an old client talking to a new server sends `position: null`, which is ignored, and
     the move means `Last`, as it always did;
   - a new client talking to an old server sends `place`, which the old server ignores, so
     the node is appended. A reorder lands at the end until that server is updated, and
     servers and desktops ship together anyway (CLAUDE.md: a desktop release with every
     version).

   The MCP `move` tool trades `position` for `after` (a node, as its other tools take) and
   `first`, which suit an LLM better than counting siblings. Keeping both forms would be
   two ways to say one thing. **Decided (Joe, 2026-10-19).**

## Verification (the bar)

1. Unit tests (pure, in rinch and Pimble): zone and depth from bounds and x; `Place`
   resolution, including an anchor that is gone and a move within one parent; indent and
   outdent targets; selection normalization; visible order; `judge_drop` sentences.
2. `pimble-crdt`: `Place` moves converge under concurrency (the
   `three_replicas_converge` sweep, extended to `After` moves of the same nodes).
3. Desktop, on a store copy (an isolated app on its own port, `scripts/perf/README.md`),
   both themes: arrows open each row they rest on and holding Down opens only the last,
   reorder within a folder, outdent by drag to each offered depth, indent and
   outdent and reorder by keyboard, hover-expand, auto-scroll, Esc cancels, a refused drop
   shows its reason during the drag, a three-node selection moved and undone.
4. Browser on the local stack: the same list, plus an encrypted store and a member's
   read-only share (every move refused, with the sentence, during the drag).
5. Two devices: a reorder made on one appears in place on the other; concurrent reorders
   of one folder end in the same order on both.
