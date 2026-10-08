# Split view contract: up to four tiled panes beside the explorer

Status: written by the PM on 2026-10-06 from Joe's request and answers of that day. Built
on 2026-10-06 and 07 on branch `split-view` (not merged). Verification items 1 to 5 and 7
passed on the desktop, and item 6 in Chromium on the local stack, except a read-only
document in the browser, which was not run (it needs a second account and a reader's
share); Firefox was not run. Where the build differs from the text, the section says so in
a line marked "As built".

## The decisions (Joe, 2026-10-06)

1. **Free tiling, up to four panes.** Any pane can be split right or down; the dividers
   between panes drag, like the explorer's edge; closing a pane gives its space back to its
   neighbour. That covers side by side, stacked, one beside two, and two by two.
2. **"Open in Split View"** in a tree row's menu is how a document gets into a new pane. A
   plain click keeps opening in the focused pane.
3. **One toolbar, above the panes, for the focused pane** (Joe, 2026-10-08, replacing "each
   pane has its own toolbar" of 2026-10-06): it shows and acts on the focused pane's tools,
   so a pane holding another node type later swaps what it shows. See "One toolbar".
4. **The same note may be open in two panes**, each with its own caret, edits appearing in
   both as they are typed.
5. **The layout is remembered** across restarts: the panes, their sizes, their documents.
6. **The browser matches the desktop**, now and from now on: one code base in `pimble-app`,
   with only platform plumbing behind `native`/`web`.

## What the person sees

- The area right of the explorer is a tiling of one to four **panes**. Each pane has, top
  to bottom: a **title strip** (the document's icon and title, then "split right", "split
  down" and "close" buttons), the read-only sentence when its document is one this device
  may only read, and its document. A pane with no document shows the empty state the single pane shows today.
- One pane is **focused**: the one last clicked in or opened into. Its title strip carries
  the accent colour. The tree's selected row is the focused pane's document. Everything
  that today acts on "the editor" acts on the focused pane: a click in the tree, a search
  result, the Edit menu, the link picker, Copy Link to Here.
- **Splitting** (a title-strip button, or View > "Split Right" / "Split Down") halves the
  focused pane and puts an empty pane in the new half, focused. **"Open in Split View"**
  on a tree row splits the focused pane to the right and opens that document there. With
  four panes open, the split buttons and the menu items are disabled.
- **Closing** a pane (its button, or View > "Close Pane") gives its space to the sibling
  it was split from. The last pane cannot be closed; closing it empties it instead.
- **Dividers** drag, and keep each side at least 200 px wide and 120 px tall.
- A link followed from a pane opens in that pane. A followed deep link places its spot in
  that pane.
- A document deleted, transplanted or no longer shared closes or follows in every pane
  that holds it, exactly as the single pane does today.

## The model

```rust
pub struct PaneId(u8);                       // 0..4, a slot; stable for the pane's life

pub enum Tiling {                            // a binary tree, at most four leaves
    Pane(PaneId),
    Split { direction: Direction, ratio: f32, first: Box<Tiling>, second: Box<Tiling> },
}
pub enum Direction { Right, Down }           // second is right of, or below, first
```

`AppStore` holds `tiling: Signal<Tiling>`, `focused_pane: Signal<PaneId>`, and per pane
what it holds one of today: the active edit (`(StoreId, NodeId)` or none), the read-only
judgement, the link tooltip and picker state where those are per editor. `active_edit`
as a single value goes away; "the active edit" is the focused pane's.

Pure functions on `Tiling`, unit-tested: `split(pane, direction) -> new PaneId` (refused at
four), `close(pane)`, `rects() -> [(PaneId, Rect)]` in fractions of the area, `dividers()`
(each with its rect and the split it drags), `set_ratio`, and serde for persistence.

As built: `drag_ratio` beside `set_ratio` (the pixel floors need the area's size); a pane
also holds the tree value it was opened on (`selected`, which a mount path needs) and the
document it is waiting to restore; the link tooltip and the link picker stay one each and
belong to the focused pane (the picker closes when the focus leaves its pane).

## How it is drawn (the part that must not be done another way)

**Four pane slots and three divider slots are rendered once and never re-parented.** A
slot is absolutely positioned inside the area by an effect over `tiling.rects()` (left,
top, width, height in percent) and hidden when its `PaneId` is not in the tiling. Splitting
and closing only change styles. Nothing ever moves an editor's DOM node to a new parent,
so an editor is never re-mounted by a layout change, keeps its caret and scroll position,
and rinch's `Editor {}` component is mounted exactly four times in the app's life.

## Editors and collaboration (CLAUDE.md "Collaboration shape", restated for panes)

- **One `EditorHandle` per pane slot**, created lazily, held in a thread-local table keyed
  by `PaneId`. `editor()` with no argument goes away: callers name a pane or ask for the
  focused one.
- Each pane that holds a document has its own collaboration session, started and stopped
  exactly as today (`start_editing`/`stop_editing`, now per pane): guest from the cached
  bytes or host of an empty document, reconcile, subscribe, read-only switch, outbound
  guard. Never `load_html`/`load_doc` on a collaborating editor.
- **Local edits**: a pane's outbound delta goes to `BackendCommand::BroadcastChanges` as
  today **and** to `collab_receive` of every other pane in this window that holds the same
  node (the server relays to other clients, not back to the sender, so the window does its
  own fan-out; a pane never receives its own delta). As built: the other panes are handed
  the delta a turn later (`set_timeout(0)`), not from inside the edit. Handed over
  mid-dispatch, the second key typed in the browser panicked on a `RefCell` in rinch's
  editor handle.
- **Remote changes** (`BackendEvent::RemoteChanges`) go to every pane holding that node.
- `SubscribeNodeChanges` once per node, however many panes hold it; unsubscribe when the
  last one lets go. As built: `BackendCommand::UnsubscribeNodeChanges` is new, and
  `BackendEvent::RemoteChanges` now names its store and node (it named neither, and every
  node ever opened stayed subscribed). On the desktop the subscription task ends at the
  next notification it is handed after the unsubscribe, and forwards nothing meanwhile.
- The label refresh, the node cache write-back at `stop_editing`, and reconcile are per
  pane; the debounced label refresh is per node.

## Persistence

`state.json` gains `panes`: the `Tiling` and, per pane, the canonical `(store id, node id)`
it held. The browser keeps the same JSON in `localStorage`. At start the tiling is
restored at once; a pane opens its document when its store is open and the node is known
(a store that never opens leaves the pane empty; a node that is gone leaves it empty).
Saved on every change of tiling, ratio (at the end of a drag) and pane document.

As built: `panes` is `{ tiling, focused, documents: [{ pane, store_id, node_id }] }`; the
focus is remembered too. "The node is known" is asked with `resolveLink`, which answers
live, deleted or missing in one go; a pane whose store has not opened keeps its document
in what is saved.

## Out of scope

Dragging a tree row onto a pane; Ctrl+click to open in a split; moving a pane; tabs inside
a pane; node types other than documents (the one toolbar shows the focused pane's tools, so
they can come).

## Verification (the bar)

1. Unit tests for `Tiling`: every way to reach four panes, close in every order, rects
   tile the area exactly, ratios clamp, serde round trip.
2. In the desktop app over the debug port (an isolated instance, its own port and
   scratch store): split to four, type in each, drag a divider, close panes in a different
   order, with screenshots; the caret and scroll of an untouched pane unchanged by a split.
3. The same note in two panes: type in one, it appears in the other; a second window (or
   `pimble-mcp`/the CLI) editing the same note reaches both; a read-only document is
   locked in every pane.
4. Restart: the layout and documents come back.
5. A link followed in pane B opens in pane B; a document deleted elsewhere closes in every
   pane holding it.
6. The browser on the local stack (`scripts/local-stack/`): 2, 3 and 4 again.
7. `cargo test --workspace --release`, the wasm check, zero warnings.

As built, two things in "What the person sees" differ. A native menu item cannot be greyed
out reactively, so with four panes open View > "Split Right" / "Split Down" answer "Four
panes are open. Close one to split again." in the status bar; the title-strip buttons and
"Open in Split View" are disabled as written.

## One toolbar (Joe, 2026-10-08)

One toolbar sits across the top of the pane area (not over the explorer). Its buttons show
the formatting at the caret of the focused pane's editor and act on that editor; when the
focus moves to another pane they follow it at once. While the focused pane holds no
document, or one this device may only read, the toolbar is dimmed and takes no presses
(the pane itself shows the read-only sentence). Pressing a button never moves the pane
focus or the keyboard. Built in `toolbar.rs` (one set of button signals, `TARGET` kept in
step with `focused_pane` by an effect) and `pane_view::render_panes`. It replaced one
toolbar per pane, which wrapped onto two or three rows in a narrow pane and took the
keyboard from the editor when another pane's button was pressed.

## The keyboard follows the pane focus (Joe, 2026-10-08: "Focus should work as expected")

When the focused pane changes (a press in it, a split, a close, the View menu), the
keyboard goes to that pane's editor if it holds a document, and to no editor if it holds
none: typing with an empty pane focused changes nothing anywhere. A press anywhere in a
pane does the same even when the pane already had the focus (its title strip, after the
keyboard went to the tree). Opening a note from the tree leaves the keyboard on the tree.
Built as `editor::give_keyboard_to` (an effect over `focused_pane` in
`pane_view::render_panes`, and the pane's press handler) on rinch's `EditorHandle::focus`
and `EditorHandle::blur` (joeleaver/rinch#1481), which releases the keyboard only from an
editor that holds it.
