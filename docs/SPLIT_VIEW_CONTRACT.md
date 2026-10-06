# Split view contract: up to four tiled panes beside the explorer

Status: written by the PM on 2026-10-06 from Joe's request and answers of that day. Not
built.

## The decisions (Joe, 2026-10-06)

1. **Free tiling, up to four panes.** Any pane can be split right or down; the dividers
   between panes drag, like the explorer's edge; closing a pane gives its space back to its
   neighbour. That covers side by side, stacked, one beside two, and two by two.
2. **"Open in Split View"** in a tree row's menu is how a document gets into a new pane. A
   plain click keeps opening in the focused pane.
3. **Each pane has its own toolbar**, because panes will hold different node types later.
4. **The same note may be open in two panes**, each with its own caret, edits appearing in
   both as they are typed.
5. **The layout is remembered** across restarts: the panes, their sizes, their documents.
6. **The browser matches the desktop**, now and from now on: one code base in `pimble-app`,
   with only platform plumbing behind `native`/`web`.

## What the person sees

- The area right of the explorer is a tiling of one to four **panes**. Each pane has, top
  to bottom: a **title strip** (the document's icon and title, then "split right", "split
  down" and "close" buttons), its **toolbar** (or the read-only sentence), and its
  document. A pane with no document shows the empty state the single pane shows today.
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
  own fan-out; a pane never receives its own delta).
- **Remote changes** (`BackendEvent::RemoteChanges`) go to every pane holding that node.
- `SubscribeNodeChanges` once per node, however many panes hold it; unsubscribe when the
  last one lets go.
- The label refresh, the node cache write-back at `stop_editing`, and reconcile are per
  pane; the debounced label refresh is per node.

## Persistence

`state.json` gains `panes`: the `Tiling` and, per pane, the canonical `(store id, node id)`
it held. The browser keeps the same JSON in `localStorage`. At start the tiling is
restored at once; a pane opens its document when its store is open and the node is known
(a store that never opens leaves the pane empty; a node that is gone leaves it empty).
Saved on every change of tiling, ratio (at the end of a drag) and pane document.

## Out of scope

Dragging a tree row onto a pane; Ctrl+click to open in a split; moving a pane; tabs inside
a pane; node types other than documents (the toolbar is per pane so they can come).

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
