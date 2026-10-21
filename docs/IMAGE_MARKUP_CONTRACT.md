# Boards contract: image nodes, whiteboards, and markup on any picture

Status: **decided, not built**. Written by the PM on 2026-10-09 from Joe's requests of
that day: "image nodes, and image markup: any image, anywhere, embedded in a text
document or as an image node, can be marked up, like Spectacle's annotation, but with
author attribution, layers, etc.", then "this also doubles as a whiteboard, which is
basically an image node without necessarily having an image". Every decision Joe made
that day is listed in "Decisions" at the end.

Builds on docs/IMAGES_CONTRACT.md (blobs, `pimble-blob:` URLs, the loader) and
docs/NODE_DOCUMENT_CONTRACT.md (the `data` root).

## The idea

There is one thing, a **board**: a co-edited vector canvas of elements in layers, each
element stamped with who made it, living in a node document's `data` root like any other
co-edited data, so several people draw on it at once and their edits merge. A picture is
one kind of element on a board, and a board holds any number of them. **Every canvas
is infinite** (Joe, 2026-10-09): its size is never tied to a picture, and what a board
"is" as a picture is **the extent of what is visible on it, trimmed to the visible
pixels**. A page size is an option a board can be given (to print or present), never the
default. The three things Joe asked for are one board each, differing only in how they
start:

| What | Starts as |
| --- | --- |
| An **image node** | an infinite canvas with the picture on it, in its own locked layer; marks beside the picture widen what the board shows, more pictures can go next to it (two screenshots side by side) |
| A **whiteboard** | an infinite canvas with nothing on it; any number of pictures, notes, shapes and connectors |
| **Markup on a picture in text** | a board inside the text's node with that picture on it; the text shows the board's visible extent, so an arrow drawn in from beside the picture shows in the text too |

And a board node can be **embedded in any node's text as a picture** (section 2): the
text shows the board, cropped to its visible pixels, live as people draw on it.

A picture's bytes are never changed. What a board looks like flattened (a marked-up
screenshot as a PNG, a whiteboard frame as a picture) is something Pimble **renders**, for
display in text, export and the clipboard; it is never stored as the truth. One document
model, one renderer and one view component serve all three.

## 1. Board nodes

One node type, `node_type = "board"`. "Image" and "whiteboard" are how a board node is
made, not two kinds (Joe, 2026-10-09): one made from a picture gets a photo icon and the
picture, a new whiteboard a chalkboard icon and nothing, and nothing else differs (the icon
is the ordinary custom icon, so the person can change it). A board opens where this
person last viewed it, or fitted to its visible extent the first time. Its document:

| Root | Holds |
| --- | --- |
| `node` | title (an image's file name without extension, "Picture" for a paste, "Whiteboard" for a new board; editable like any title), timestamps, tags, icon |
| `content` | unused: **a board node is only its board** (Joe, 2026-10-09); a caption is a text element on the board |
| `data.board` | the board document (section 3) |

**Making one:**
- Drop image files on the tree: an image node per file, in order.
- File > Import > "Images..." (a file dialog, multiple selection).
- Paste with a tree row focused and a picture on the clipboard: an image node under it.
- A row's menu: "New Image..." (a file dialog) and "New Whiteboard".
- On a picture in text: "Convert to Image Node" makes an image node with the same blob (no copy of
  the bytes) and a copy of that picture's markup, under the text's node, and **replaces the
  picture in the text with an embed of the new node** (section 2), in one edit, so the
  text still shows it and the markup now lives in one place. Undo takes both back.
- Pictures go on any board, as many as wanted: paste, drop files, or the picture tool.
  They land where they were dropped, or beside what is there for a paste.

**Showing one:** a pane holding a board node shows the board view and nothing else (the
pane slot stays mounted with its editor hidden, so the four editors are still never
re-mounted, and the one toolbar above the panes shows the board tools while a board pane
is focused). The board view: fit (to the visible extent), 1:1, zoom with Ctrl+wheel and
pinch, pan with the hand tool, space-drag or middle-drag, the navigator (section 8), the
tools (section 4), the layers panel (section 5), and the markup toggle every picture has
(section 5, "Showing and hiding markup").

**The visible extent.** Everything that renders a board as a picture (a picture in text, an
embedded board, Copy Picture, export, the MCP, thumbnails) renders its **visible extent**:
the union of the bounds of every element the document shows (layers and elements with
`visible` on, stroke widths and shadows included), then trimmed to the pixels that are
not fully transparent, so a highlighter stroke that leaves the picture or a shadow is
kept and empty space is not. Elements on hidden layers do not widen it. A board with
nothing visible renders as nothing, with a placeholder sentence where it is embedded.

**A page, optionally** (Board > "Page..."): a rectangle `{ x, y, w, h }` in board units and
a fill, for a board meant to be printed or presented: paper and screen sizes (A4, Letter,
16:9), "Fit to Content", or dragged handles. A board with a page renders the page instead
of its visible extent; elements outside it stay in the document and show dimmed while
editing. "Remove Page" goes back to infinite. One property, last writer wins. **Crop**
on an image node is a page the size of the crop (Board > "Crop", the crop tool), so a
cropped screenshot still keeps everything outside the crop for later.

**Replacing a picture** (a picture's menu: "Replace Picture..."): a new blob in that
element, fitted into its rectangle, so the marks stay where they were relative to it; when
the aspect ratio differs, a sentence says the marks may no longer line up. The old blob
stays (blobs are never deleted), so undo brings it back.

**Info:** an image's size, type, pixel dimensions, when added and by whom (section 6); a
whiteboard's element count and authors.

**Search:** the title and every piece of text on the board (text elements,
sticky notes, text inside shapes, connector labels, frame names) as `IndexUnit` fields, so
a word written on a screenshot or a sticky note finds the node. Node cards (section 4) are
links, so the boards that point at a node show in its backlinks. OCR of pictures is in
"Later".

## 2. Pictures in text

A picture in a node's text is rinch's `image` atom. It gains one attribute, `data-board`
(an id, absent until the picture is first marked up). It is an ordinary `data-*`
attribute: rinch keeps any an app sets on an image, round-trips them through copy and
paste, writes them on the `<img>` and merges them per attribute between collaborators
(joeleaver/rinch#1518, which replaced the `board` attribute #1500 had put into rinch;
Joe, 2026-10-10: Pimble concepts stay out of rinch). The board lives in **the host node's**
`data["board:<board id>"]` (one key of the data root per board, never a map made on first use: two devices marking up their first pictures while apart would each make it, and one would be lost), with the picture as an element on it, so it is co-edited
with that node, replicates with it, is shared with whoever may read that node, and is
undone by the same undo.

- **Marking up:** double-click a picture, or its context menu's "Mark Up...", opens the
  board view over the pane (the same component a board node shows, with "Done"). The
  first mark mints the board id and writes it to the atom's attribute in one edit.
- **Showing:** the editor still shows an `<img>` whose `src` is the blob URL. The app's
  `pimble-blob:` loader, asked for a picture that carries a board, answers with the
  rendered board instead of the raw bytes, and calls `rinch::image::reload_image` when the
  board changes (debounced, about 150 ms, so a stroke someone is drawing appears as it is
  drawn without a render per point). The text editor needs no drawing of its own. (How the
  loader learns the board id from the atom is spike 2.)
- **Copy and paste:** pasting a marked-up picture into another node copies its board into
  that node's `data` under a fresh id (the clipboard carries it as a Pimble MIME part
  beside the HTML). Pasting within the same node also mints a fresh id: two copies of a
  picture are two pictures, and marking up one does not mark the other.
- **Moving between stores** (`transplantNode`) carries the `data` root with the content;
  the blobs it names are copied as the images contract already says.
- **Deleting the picture from the text** leaves its board in `data`, so undo and "Put Back"
  restore it marked. Unreferenced boards are swept with unreferenced blobs, later.

### Board nodes embedded in text

A board node shown inside another node's text, as a picture, live (Joe, 2026-10-09).

- **What is in the text**: the same `image` atom, with `src` a new URL scheme,
  `pimble-board:<store uuid>/<node uuid>` (optionally `#frame=<element id>` to show one
  frame of a whiteboard), so it is co-edited, moved, deleted and undone like any picture.
  It names the canonical store and node, as `pimble:` links do, so it works through
  mounts and across stores. `alt` is the board's title when embedded.
- **Making one**: drag a board node from the tree into text; paste a board node's link
  ("Copy Link" on its row) where the clipboard says it is a board; Insert > "Board..."
  (the `[[` picker, limited to board nodes); "Convert to Image Node" on a picture (section 1).
- **Showing**: the app's loader answers `pimble-board:` with the board's visible extent
  rendered (or its page, or the named frame), at the width the text gives it times the
  screen's scale. While the picture is on screen the loader holds a subscription to the
  board node (the same `SubscribeNodeChanges` a pane takes, released when no embed or
  pane shows it) and re-renders on its changes, debounced like a marked-up picture, so
  the text shows a whiteboard as people draw on it. Size: the board's natural size (one
  CSS pixel per board unit) capped at the pane's width, or a width the atom's `width`
  attribute sets (resizing pictures, from the images contract's "Later", applies to both).
- **Clicking** an embedded board opens it in the focused pane; Ctrl+click opens it in
  split view, as links do. "Mark Up..." on an embed opens the board node itself, so there
  is never a second copy of its marks.
- **It is a link**: it follows the board's `became` when the board moves out of a share
  or between stores, counts in the board's backlinks, and is offered when "Recently
  Deleted" lists the board. A board that is deleted, not here yet, or that this person
  may not read (an embed in a share of a board outside it) shows a placeholder with one
  sentence ("This board was deleted.", "This board has not arrived yet.", "You cannot
  see this board."), never someone else's pixels: the loader asks through the server,
  which judges the board node as a read, so `Reach` decides as for any node.
- **Markdown and the MCP**: `![title](pimble-board:...)` both ways; `render_board` serves
  an embed's picture too.
- **Search** indexes the embed as a link (the board's own text is indexed on the board).

## 3. The board document

One schema, as nested yrs types, in all three homes. Everything is keyed by random ids in
Maps, never positions in Arrays, because a yrs Array move is a delete plus an insert and
two concurrent moves duplicate the item.

```
board: Map {
  v: 1,
  page: { x, y, w, h, fill }?,   // optional; absent = infinite, render the visible extent
  markup_visible: bool,          // the floating toggle (section 5); hides every non-picture element
  layers: Map<layer id, Map {
    name: Text,                  // co-editable name
    order: String,               // fractional index; ties broken by id
    visible: bool,               // the document's own visibility, shared
    locked: bool,
    opacity: f64,
  }>
  elements: Map<element id, Map {
    layer: layer id,             // moving an element to another layer is setting this
    order: String,               // z-order within the layer, fractional index
    parent: frame or group id?,  // membership, so moving a frame moves what is in it
    kind: "picture" | "pen" | "highlighter" | "line" | "arrow" | "rect" | "ellipse"
        | "diamond" | "text" | "sticky" | "step" | "callout" | "connector" | "frame"
        | "group" | "card" | "spotlight",
    geom: ...,                   // per kind, below
    text: Text?,                 // text, sticky, shapes, callout, connector label, frame name
    style: Map { stroke, fill, width, opacity, font_size, align, shadow, arrow_heads, dash },
    transform: { tx, ty, rot, sx, sy },
    locked: bool,
    author: Author, created_at,
    edited_by: Author, edited_at,
  }>
}
```

- **Board units.** Every coordinate, stroke width and font size is in board units, one
  space per board, independent of any picture. A picture placed at 100% gets one unit per
  pixel of the picture, so a screenshot shows 1:1 at 100% zoom; after that its
  rectangle is its own and it scales like any element. A page is a rectangle in the
  same units (or absent), so a page, adding pictures, a zoomed view and a rendered copy
  at any size all agree, and nothing is defined relative to a picture.
- **Geometry**: a rectangle for picture (plus its blob URL, its pixel size, crop and rotation), shapes,
  sticky, spotlight, frame and card; two points for line and arrow; an
  anchor for step; for pen and highlighter an Array of packed `f32` point chunks (x, y,
  pressure), appended by the drawer every ~100 ms while drawing so others watch the
  stroke being made, smoothed once on release, and never edited after (moving, rotating
  or scaling a stroke writes its `transform`).
- **Connectors** name the elements they join (`from: { element, side? }`, `to: ...`, or a
  free point) and are routed at render time, so moving a sticky note drags its connectors
  for everyone without anyone writing them. A connector whose element was deleted keeps
  its last point.
- **Text is a yrs `Text`**, so two people can fix the same sticky note at once.
- **Concurrency**: different properties of one element merge; the same property is last
  writer wins (two people dragging one arrow: one position wins, nobody's arrow is
  duplicated). Deleting an element removes its key; a concurrent edit of a deleted
  element is lost, which is the expected outcome. A frame or group deleted while someone
  adds to it leaves the addition with a `parent` that names nothing, which reads as no
  parent.
- **Step numbers** (the 1, 2, 3 badges) are not stored: they are derived at render time
  from the steps' `created_at` and id, per layer, so two people adding step 3 at once get
  3 and 4 instead of two 3s.
- **Eraser** removes whole elements. There is no pixel erasing: there are no pixels.

## 4. Tools

One toolbar, every tool on every board; the defaults differ (an image opens on the arrow,
a whiteboard on select).

- **Marking up (the Spectacle set):** select / move / resize / rotate (multi-select, a
  marquee), pen, highlighter (multiply blend), line, arrow (heads at either end),
  rectangle, ellipse, text, numbered step, callout (a box with a pointer), spotlight (dims everything outside a region), crop (a picture's own
  crop; cropping the board is giving it a page), eraser.
- **Whiteboarding:** hand (pan), sticky note (colored, text grows the note), shapes with
  text inside (rectangle, ellipse, diamond), connector (snaps to an element's sides),
  frame (a named region; frames are listed in the navigator and can be exported or
  presented one by one), picture (insert, paste or drop; any number), **node card** (a
  Pimble node on the board: its icon, color and title, live; double-click opens it in
  the focused pane; dropping a tree row on a board makes one; it is a `pimble:` link, so
  it follows a node's `became` and counts as a backlink), group and ungroup, lock.
- **Arranging:** grid (shown or not, snap or not, per viewer), smart guides, align and
  distribute, bring forward and send back (writes `order`), duplicate.
- **Style:** color (the appearance palette plus a picker), width, fill, opacity, dash,
  font size, alignment, shadow. Shift constrains (45° lines, squares, circles).
  Keyboard: tool letters (Spectacle's where they do not collide with the app's keys).

**Undo is per person**: yrs's `UndoManager` scoped to this client's own origin, so Ctrl+Z
takes back my last edit, never one a collaborator just made.

**No blur or pixelate for now** (Joe, 2026-10-09). If they come later: the picture under
them is never changed, so they would only hide a region on screen, and only an export
would destroy what is underneath.

## 5. Layers

- **Layers are for organization, not attribution** (Joe, 2026-10-09). A board starts with
  one layer, "Layer 1", that everyone draws into; anyone can add layers for a purpose
  ("Issues", "Background", "Measurements") and draw into them. Who made what is on each
  element (section 6), never expressed by layers.
- A layers panel beside the board: rows in order, each with visibility, lock, name
  (renamed in place), opacity and a count of its elements. Drag to reorder (writes
  `order`). "New Layer", "Delete Layer" (with its elements), "Merge Down", "Move
  Selection to Layer". New elements go into the selected layer.
- A locked layer or element refuses edits from the tools for everyone (a shared guard
  against accidents, not a permission: anyone with edit rights can unlock it).
- **Visibility is one thing, shared, on the node** (Joe, 2026-10-09): what a board shows
  is part of its document, the same for everyone who opens the node, in its pane, in text,
  in embeds and in exports. There is no per-person or per-device hiding.

### Showing and hiding markup (Joe, 2026-10-09)

**Every picture that has markup carries small floating controls** to show or hide it:
an image node in its pane, a marked-up picture in text, and a board node embedded in
text. **The setting is the node's, for everyone**: it lives in the board's document, so
whoever hides the markup hides it for every person and device that opens the node, and
it stays that way until someone shows it again.

- **The control**: an eye button in the picture's top-right corner, shown while the
  pointer is over the picture (or after a tap on a touch screen), with a count of the
  marks ("Markup · 12"). Clicking it sets the board's `markup_visible` (section 3): off
  hides every element that is not a picture, so the pictures show as they came in; on
  shows the marks again. A chevron beside it opens the board's layers with a checkbox
  each, which are the layers' own `visible`, for hiding some marks and not others.
- **It is an edit like any other**: co-edited (last writer wins if two people toggle at
  once), undone by the toggler's undo, stamped `edited_by` on the board, carried by sync
  and shares. Someone who may only read the node sees the control's state but cannot
  change it.
- **Where an embed writes**: the control on a board node embedded in text changes the
  board node itself, so hiding its markup there hides it in the board's own pane and in
  every other embed of it. A marked-up picture in text writes the board in its host node.
- **When markup is hidden the control stays visible** (dimmed, "Markup hidden"), so
  nobody forgets that marks exist on a picture that looks clean. A picture without
  markup has no control. A new board shows its markup.
- **Rendering follows the document**: the loader, Copy Picture and export render what the
  board shows; export also offers "with all markup" for a one-off copy that includes the
  hidden layers, without changing the board.
- **rinch**: floating controls over an image inside the editor need the editor to say
  where an image atom is drawn and when the pointer is over it (an `EditorHandle` hook
  giving the atom's rectangle and its attributes, or an overlay slot per image atom). A
  pull request upstream; spike 2 covers it with the atom's `data-board` attribute.

## 6. Attribution

Every element carries `author` and `created_at`, and `edited_by` and
`edited_at` on any change. `Author = { id, name }`.

- **Who is "me" is automatic** (Joe, 2026-10-09): the signed-in Pimble Cloud account,
  never anything a person types. `id` is the account's `sub`, `name` comes from the
  account (the email's local part until accounts have display names). The desktop reads
  both from the keystore's session (`CloudStatus`), the browser from its token; the
  vault client and the desktop stamp elements with it, nobody picks it.
- **Not signed in means no author**: `author` is absent. Several people only ever edit one
  store through Pimble Cloud (a hosted store, a relayed share), and every one of those
  needs an account, so an element without an author was made by the store's only person.
  The one multi-party path without accounts is two servers linked with a static server
  token (`sync.json`, the CLI's `PIMBLE_TOKEN`); that is one operator's devices, not
  people, and their elements are unauthored too. Signing in later does not rewrite old
  elements.
- **Attribution shows only when it says something**: on a board whose elements have one
  author or none, the hover and the layers panel name nobody; with two or more authors,
  every authored element shows its author and an unauthored one shows "before sign-in".
- **The name is stamped**, not looked up: elements made offline, in a store opened from a
  USB stick, or by someone who has since left a share still say who made them. When a
  people directory exists (members seeing each other, which Joe called "later" on
  2026-09-21), the display prefers its current name for a known id and falls back to the
  stamp.
- **What attribution shows**: hovering an element shows "Arrow, by Sam, 2 hours ago
  (moved by Joe, 5 minutes ago)"; "Color by author" (a way of
  looking, per pane, that hides nothing) draws every element in its author's color.
- **It is a label, not proof.** Documents are co-edited directly and an encrypted store's
  server sees only ciphertext, so anyone with edit rights could write any `author` field.
  Signed edits would need per-account signing keys on every edit and a check on every
  device; that is in "Later" and only if Joe wants attribution to be evidence.

The author identity built here is the one text comments, presence and "who changed this
paragraph" would use later; nothing in it is specific to boards.

## 7. Presence: later, for the whole app

Live cursors, others' selections, "Follow" and a laser pointer are **not part of this
work** (Joe, 2026-10-09): presence gets designed once, for text and boards together, as
its own piece of work. What boards do without it: every edit still arrives live (it is
document data), and a stroke being drawn is seen as it is drawn, because its points are
appended to the document every ~100 ms (section 3). What presence will need from boards
when it comes: positions in board units and selections as element ids, both already the
document's terms. It is never document data: cursors, viewports and selections are
ephemeral, never stored, gone when the person leaves.

## 8. Rendering and export

One renderer, `pimble-board` (a new crate, native and wasm, no UI): a board document, the
pictures it names and a view (a rectangle in board units, a scale) in,
an RGBA image out, on `tiny-skia` (pure Rust, runs in wasm), with one bundled font for
board text so a board renders the same on every device. Used by the board view, the
`pimble-blob:` loader for pictures in text, Export, the clipboard, the MCP and thumbnails
later. It keeps a spatial index of elements (rebuilt from the document, never stored) and
renders only what the view rectangle touches, so a whiteboard of thousands of elements
pans at the cost of what is on screen.

The board view is a rinch `RenderSurface` painted by this same renderer, the way rinch's
`paint` example paints its surface (spike 1: a persistent raster drawn into
incrementally, re-rasterized only where something changed), so the live view and every
rendered copy agree by construction. The navigator is a second small
surface: the board's visible extent (or its page), with the visible rectangle and the
frames.

**Export**: "Copy Picture" (flattened PNG on the clipboard: the visible extent or page, a
frame, or the selection), "Save Picture As..." (PNG or JPEG; SVG for a whiteboard or a
frame, since it is vector; what the board shows, or "with all markup"), "Save Original
As..." (a selected picture's bytes), and "Flatten into New Image" (a new blob and a new
image node; the markup is not carried over).

## 9. The MCP

`read_node` on a board node, and on a node whose text has marked-up pictures, describes
the board as a list (kind, position, text, layer, author, connections). A tool returns the
rendered board, or a frame of it, as an image to the model (`render_board`), so an
assistant can see what was drawn. Writing to boards from the MCP (an assistant laying out
notes as stickies, circling something on a screenshot) comes later; its elements would
carry an author of their own ("Claude, via <person>").

## 10. Where this sits against images waves 2 to 5

Today pictures do not replicate (images contract waves 2 and 3). A marked-up picture or a
whiteboard with pictures on it would arrive on another device as marks over nothing.
**Images wave 2 (plain replicas) and wave 3 (encrypted, shares) come before boards are
used across devices**; boards can be built on one machine in parallel with them. A
whiteboard of only drawn elements needs no blobs and works across devices as soon as it
exists, because it is only document data.

## Spikes before building

1. **Drawing in rinch, desktop and browser: follow rinch's `paint` example**
   (`examples/paint`, one library shared by `paint-desktop` and `paint-web`; Joe,
   2026-10-09: it is fast). How it gets its speed, and what the board view copies:
   - **A `RenderSurface`** (`rinch::render_surface`): the app hands it whole RGBA frames
     with `submit_frame`; on the desktop rinch composites them (hole-punch and a WGSL
     compositor, a redraw requested straight from the writer without an event-loop round
     trip), in the browser it is a real `<canvas>` the browser composites. The writer is
     `Send + Sync`, so on the desktop frames can be rendered off the UI thread.
   - **A persistent raster, drawn into incrementally.** Paint keeps one bitmap and a
     pointer move stamps only the new stroke segment into it; a frame is then just a
     resample of that bitmap to the view. The board does the same: a raster cache of the
     board (pictures and settled elements) at the current zoom, with the stroke being
     drawn stamped into it segment by segment as it grows, exactly as paint does. The
     vectors are re-rasterized only for what changed: a remote edit or a selection change
     re-renders the bounds of the elements it touched; a zoom change re-renders the
     view, once, when the zoom settles (until then the cached raster is scaled, as
     paint's view resample does at any zoom).
   - **Dirty flags**: the render callback returns at once unless something changed, and
     the navigator has its own flag.
   - **Frames rinch handles fastest**: rendered at the surface's own physical size
     (`layout_size`) and opaque (the board's background filled), so rinch copies rows
     instead of sampling and skips the premultiply (rinch #361).
   - Also taken: the zoom and pan math (`screen_to_canvas`, wheel zoom about the cursor,
     middle-drag pan), the navigator surface, and DOM overlays positioned over the
     surface with `pointer-events: none` (the text being typed into a sticky or a shape,
     as a real input, and selection handles if they are easier as elements).
   - Not taken: its brush kinds and its whole-canvas undo snapshots (boards are vector,
     and undo is yrs's per-person `UndoManager`).
   - If a 4K pane ever needs more: on the desktop `set_texture_source` hands rinch a wgpu
     texture to composite with no copy, and in the browser `canvas_element` takes a
     WebGL or WebGPU context. Not planned; the paint example does not need it.
   - **rinch gaps, pull requests upstream**: `SurfaceEvent` carries no pen pressure, no
     touch or pinch, and says nothing about keeping the pointer when a drag leaves the
     surface; boards want pointer events with pressure and pointer type, pinch zoom, and
     capture for the length of a drag. Dropping a tree row on a surface (node cards)
     uses its existing drag-and-drop events.
   The spike: run `paint-desktop` and `paint-web` at 4K to see it, then the board view's
   first cut (a picture, a pen, the raster cache) on both.
2. **The image atom's extra attribute** survives the collaboration round trip and
   reaches the app's loader (or the loader is given `src` plus attributes). A rinch pull
   request either way.
3. **`tiny-skia` in wasm** for the re-rasterizing the cache needs: a 4000 × 3000 picture
   with 200 elements in the browser in under 200 ms (a zoom settling, a picture in text
   rendered), and a 5000-element whiteboard's view after a zoom.
## Waves

1. Spikes; `Author` from the signed-in account; board nodes with the board document, the
   renderer, the board view with zoom, pan and the navigator, pictures as elements (any
   number), the visible extent and optional pages, image nodes and every way of making
   one, search, info.
2. Drawing and markup on board nodes: select, pen, highlighter, line, arrow, rectangle,
   ellipse, text, step, eraser, per-person undo, the layers panel, attribution display,
   the markup toggle (remembered per picture), export. Images and whiteboards both get them, since they are the same view.
3. Whiteboarding: sticky notes, shapes with text, connectors, frames, groups, node cards,
   grid and snapping, align and distribute, SVG export.
4. Boards in text: the atom attribute, the loader rendering, "Mark Up...", clipboard and
   transplant carrying boards; board nodes embedded in text (`pimble-board:`, live
   re-render, placeholders, links and backlinks, Markdown); the markup toggle floating over
   pictures in text (the rinch hook); "Convert to Image Node".
5. Spotlight, callout, crop, "Color by author", the MCP's reading and
   `render_board`.

Desktop and browser together in every wave (Joe, 2026-10-06). Each verified on the local
stack with two people on one board at once (desktop and browser), a cut link healing, and
a share member's elements reaching the owner.

## Later

Blur and pixelate (see section 4); thumbnails in the tree and a gallery of a
subtree's pictures; OCR into search; comment pins with threads; presenting frames as
slides; templates (a retro, a kanban, a mind map); signed attribution; boards written by
the MCP; markup on PDFs and other attachments (the same model over a page); measuring
tools (a scale bar and distances).

## Decisions (Joe, 2026-10-09)

1. **Attribution is automatic**, from the signed-in account; multi-user always means
   accounts, so no account means no author (section 6).
2. **A picture-in-text's markup** is a board in the host node's `data`, keyed by an id on
   the picture, adding nothing to the tree; "Convert to Image Node" makes it a board node
   embedded in its place.
3. **No blur or pixelate** for now.
4. **Layers are for organization, not attribution**: one layer to start, shared by
   everyone; attribution is per element.
5. **A board node is only its board**: no rich text under it.
6. **Pictures replicate first**: images waves 2 and 3 come before boards ship across
   devices.
7. **No "New Screenshot..."**: screenshots come from the OS's own tools, by paste and drop.
8. **One node type, `board`**; "image" and "whiteboard" are only how it starts.
9. **Presence later, app-wide**, not in this work (section 7).
10. **Every canvas is infinite**; a board renders as its visible extent trimmed to visible
    pixels; a page is optional.
11. **A board node can be embedded in any text** as a picture, cropped to its visible
    pixels, always live; a frozen copy is "Flatten into New Image".
12. **Showing and hiding markup** is one setting on the node, shared by everyone, never
    per person or per device (section 5).

## Built in wave 1 (pimble-board)

`crates/pimble-board` (2026-10-09, branch `boards`): the board document, the renderer and
the raster cache, native and wasm32, no UI and no rinch. The app is not wired yet.

**The document** (`Board`, `BoardTxn`, `doc.rs`). A `Board` is a handle on where a board
lives: `Board::of_node(&NodeDoc)` (`data.board`), `Board::in_text(&NodeDoc, id)`
(`data["board:<id>"]`), `Board::standalone()`. Writes go through `board.edit(stamp, |t| ...)`,
one transaction answering `Edited { result, update }`: the update is an ordinary yrs update
of the node's document, sent with `applyEdit` (there is no other way in). `BoardTxn`:
`create`, `add_element(NewElement)`, `append_points`, `replace_points`, `set_geom`,
`set_style`, `clear_style`, `set_transform`, `set_text` (the smallest middle edit, so
concurrent typing in one note merges), `set_locked`, `set_parent`, `move_element`,
`delete_element`, `add_layer`, `rename_layer`, `set_layer_visible/locked/opacity`,
`move_layer`, `delete_layer` (with its elements), `merge_layer_down`, `move_to_layer`,
`set_markup_visible`, `set_page`. `Stamp { author: Option<&Author>, now }`. Per-person
undo: `Board::with_origin(o)`, `undo_manager(options)`, `undo(&mut manager, redo)` (answers
the bytes to send). `Board::watch()` reports which elements changed (local and remote), so
`BoardSnapshot::update` / `BoardView::update` re-read only those. `NodeDoc` gained
`ydoc()`, `data_root()` and `board_units()` (search: one `Field("board.<kind>")` unit per
element with text, path `f:board/<id>` or `f:boards/<board>/<id>`).

**Reading and drawing.** `BoardSnapshot` (z-order, members moved with their frame or
group, routed connectors, step numbers, text layout, bounds, `visible_extent()`,
`render_rect()`, `markup_count()`, `authors()`, `texts()`, `hit()`, a spatial grid, a
fingerprint per element). `render(snapshot, pictures, view, w, h, background)`,
`render_extent(snapshot, pictures, scale)` (page, or visible extent trimmed to visible
pixels, as straight RGBA with `encode_png`), `draw_region`. Pictures come from a
`PictureSource` (a closure or a map from `src` to `Arc<Picture>`; `Picture::decode` takes
PNG, JPEG, GIF, WebP; a missing one draws a grey placeholder). `BoardView` is the raster
cache: `set_camera`, `pan_by`, `zoom_at`, `fit`, `screen_to_board`, `update(board,
changes)` or `set_snapshot`, `begin_stroke` / `extend_stroke` / `end_stroke` /
`cancel_stroke`, `settle(pictures)` when a zoom stops, `frame(pictures) -> Option<&[u8]>`
(opaque RGBA at exactly the surface's size, `None` when nothing changed: hand it to
`SurfaceWriter::submit_frame`).

**Decisions where this contract was silent** (Joe may overrule any):
- `geom` and `transform` are each one value written whole (last writer wins as a unit);
  `style` is a map whose keys merge one by one; a picture's rotation is its `transform`
  (no second rotation in `geom`). Transform: translate (`tx`, `ty`), then rotate `rot`
  degrees clockwise and scale (`sx`, `sy`) about the center of the element's own
  untransformed extent. A member's world transform is its parent's then its own, so moving
  a frame moves its members with no writes to them.
- A connector end that names an element also stores its last point, which is where it
  stays if the element is deleted. Connector-to-connector ends use that point.
- An element whose layer was deleted while it was being added is drawn in the top layer; a
  `parent` that names nothing or makes a cycle reads as none. Unknown kinds are kept and
  not drawn. A board keeps at least one layer.
- `created_at` is always written; `author`, `edited_by` and `edited_at` only when someone
  is signed in. Appending points while drawing does not stamp `edited_by` (it is the
  drawing itself); smoothing on release does. Board-level writes (markup toggle, page,
  layers) stamp `edited_by`/`edited_at` on the board. Timestamps are RFC 3339 UTC strings
  and steps sort by them as strings, so every writer must use one shape.
- Ids are 64 random bits as hex. `order` uses Greenspan's fractional-indexing keys in base
  62 with the integer part, so adding on top keeps keys short (2000 appends: 3 characters).
- Creating a board is explicit (`create`). Two devices creating the same board at once
  would leave one board (a yrs map key is last-writer-wins), so a board node's board is
  created with the node, and a picture's board gets a fresh id as section 2 says.
- Rendering: shadows are an offset translucent fill (tiny-skia has no blur); an element's
  opacity applies per part; a pen's width follows each point's pressure (see "Pointer events" below; a highlighter's does not);
  text is Noto Sans Regular only, no shaping or kerning, a box for characters the font lacks
  (CJK and emoji), and bars below 4 px; a spotlight dims the region being drawn and its
  extent is its own rectangle; a card shows its element text (the node's title, written by
  the app) or its link. A stroke stamped live blends segment by segment (a translucent
  highlighter darkens at the joins) until its element arrives and its rectangle is
  re-rendered from the document.
- Elements outside a page are not dimmed yet (the view's job, later).

**Spike 3 numbers** (bench: `cargo run -p pimble-board --release --example bench`; wasm:
build the example for `wasm32-wasip1` and run it in wasmtime, which compiles wasm much as a
browser does). Taken on a 24-core machine with a load average of 25 to 50 from other work,
so treat them as upper bounds; the first, quieter native run was 20 to 40 % faster.

| | native | wasm | wasm + simd128 |
| --- | --- | --- | --- |
| 4000x3000 picture + 200 elements, full render at 1:1 (4000x3000 out) | 84 ms | 101 ms | 72 ms |
| the same fitted into 1920x1080 (scale 0.36) | 51 ms | 130 ms | 68 ms |
| the same fitted into 3840x2160 | 150 ms | 462 ms | 220 ms |
| a 2000-point stroke: stamping one segment into the cache (median) | 0.04 ms | 0.06 ms | 0.05 ms |
| a frame from the cache (1920x1080 row copy) | 2 ms | 1.4 ms | 1.5 ms |
| a frame while a zoom has not settled (nearest resample) | 3 ms | 3 ms | 3 ms |
| 5000-element whiteboard, view settles zoomed out (all 5000 in view) | 102 ms | 134 ms | 109 ms |
| the same, settled at about 1:1 (39 in view) | 61 ms | 82 ms | 68 ms |
| 5000-element whiteboard: one element changes (`BoardView::update`, then the frame) | 24 + 2 ms | 19 + 2 ms | 21 + 3 ms |
| 5000-element whiteboard: a full read and resolve | 64 ms | 53 ms | 56 ms |

The contract's target (a 4000x3000 picture with 200 elements in under 200 ms in the
browser) holds for the view-sized render a zoom settle does, with or without simd128;
only a 4K-wide view of it passes 200 ms without simd128, so the web build should turn on
`+simd128` (every current browser has it). A settle renders the view plus a 25 % margin
each side, which is most of its cost at 1:1.

**What the app needs to wire it** (wave 1 and 2 integration):
- Hold the board node's `NodeDoc` (as the web's vault client does; the desktop through
  the same `getNode`/`syncNodes` bytes), `Board::of_node`, a `Board::watch`, and on every
  local edit or `RemoteChanges` call `BoardView::update(&board, &watch.take())`.
- Send every `Edited.update` (and `Board::undo`'s bytes) as `BackendCommand` `applyEdit`
  of that node; one `UndoManager` per pane session with the device's own origin.
- A `RenderSurface` per board pane: the render callback calls `view.resize(layout_size)`
  and `writer.submit_frame(view.frame(..))`; pointer events map through
  `screen_to_board`; a timer calls `settle` about 150 ms after the last wheel or pinch;
  `begin/extend/end_stroke` while drawing, appending points to the document every ~100 ms
  and `smooth_stroke` + `replace_points` on release.
- A `PictureSource` over the blob cache (`GetBlob`/`BlobLoaded`), decoding with
  `Picture::decode` off the UI thread, and `view.invalidate(rect)` for a picture whose
  bytes arrive late.
- `Author` from `CloudStatus` (desktop) or the token (browser); `None` when signed out.
- Search: call `NodeDoc::board_units()` from the indexer for board nodes (the server picks
  `index_units` by plugin; there is no board plugin yet).
- The `pimble-blob:` loader for marked-up pictures and `pimble-board:` embeds:
  `render_extent` at the text's width times the screen scale.

## Built in wave 1 (app side)

Built 2026-10-09 on branch `boards`, desktop and browser from one module,
`crates/pimble-app/src/boards.rs`, on rinch `main` (3c9958c).

**What exists:**
- **Node type** `pimble_core::node_types::BOARD`. A board node is made with its board in
  the edit that creates it: `createNode` takes an optional `content` (a yrs update merged
  into the new document) and `custom` keys (`BackendCommand::CreateNode`'s `init`,
  `NodeInit`); the vault client does the same for an encrypted store in the browser. The
  icon is the ordinary custom icon, `photo` for an image node and `chalkboard` for a
  whiteboard; the title is explicit.
- **Making one**: a row's menu has "New Whiteboard", "New Image..." (several files; the
  browser's hidden file input) and "Paste Image" (desktop: the clipboard's bitmap as
  PNG). Image files dropped on a tree row (desktop: rinch's `onfiledrop` on the row;
  browser: page-level `dragover`/`drop` listeners finding the row by `data-pimble-row`)
  and, in the browser, Ctrl+V with no editor or field holding the keyboard (the tree's
  selected row is the parent) make an image node per picture, in order: one `PutBlob` at
  a time (`pictures.rs`'s path: scaled to fit, refusals as sentences), then the node.
  `StoredBlob` now carries the stored picture's pixel size. An image node's board is
  "Layer 1" over a locked layer "Picture" holding the picture at one unit per pixel. A
  row on a mount makes the node under the mount's source.
- **Showing one**: a pane holding a board node shows a `RenderSurface` painted by
  `BoardView` (`PaneState::show_board`, `PaneState::board`); its editor stays mounted
  and hidden. The render callback and every change call `present`, which resizes the
  view to `layout_size` and submits a frame only when `frame()` answers one. Fitted to the
  visible extent on first show (never above one unit per CSS pixel), wheel zoom about the
  cursor with `settle` 150 ms after the last step, pan with middle-drag and space-drag,
  Fit and 1:1 in the toolbar. Pictures come from the blob cache (`GetBlob`/`BlobLoaded`,
  `pictures::held_bytes`), decoded on a thread of their own on the desktop and on a
  timer turn in the browser; the rectangles of the elements showing a picture are
  re-rendered when it arrives. Layouts with board panes are remembered and restored like
  documents.
- **Collaboration**: each pane holds its own `NodeDoc` of the node (joined from a sibling
  pane's copy when one holds it), `Board::of_node` with a per-pane origin, a `Board::watch`
  and `BoardView::update` on every change. A local edit is one `Board::edit`, sent as
  `BroadcastChanges` (`applyEdit`, the one write path) and handed to the window's other
  panes on the node a turn later; `RemoteChanges` and the open/reconnect reconcile
  (`ReconcileNodeContent`) apply to every board pane holding the node. One subscription
  per node, counted across editors and boards (`AppStore::is_editing`).
- **Tools** (the one toolbar shows them while a board pane has the focus): select (click,
  drag to move by writing `transform` about every 80 ms and on release, Delete or
  Backspace, Escape), pen (an element at the press, points appended every 100 ms, then
  `smooth_stroke` and `replace_points` on release), six colors and three widths, undo and
  redo per pane (`UndoManager` over the pane's origin, a step per stroke; Ctrl+Z,
  Ctrl+Shift+Z, Ctrl+Y). Locked elements and elements in locked layers cannot be selected.
  The floating markup control sits top-right ("Markup · N", dimmed "Markup hidden") and
  writes `markup_visible`; it is shown whenever the board has marks, not only on hover.
  A node this device may only read refuses tool edits and the toggle with the read-only
  sentence; the editing half of the toolbar is dimmed.
- **Author**: the desktop takes the account from `cloudStatus` (which now answers
  `user_id`), the browser from its unlocked session; the name is the email's local part.
- **Search**: `BoardPlugin` in `pimble-plugins` indexes `NodeDoc::board_units()` for board
  nodes; the browser's vault search reads the same units for an encrypted store.
- **Web build**: `web/.cargo/config.toml` turns on `+simd128` for `wasm32-unknown-unknown`
  (the release wasm carries SIMD instructions; trunk's wasm-opt took it as is).

**Verified (2026-10-09):**
- Tests: `pimble-app` (all 136, including `boards::tests`: two panes in step, a peer's
  update in both, per-pane undo, one subscription; an image board's locked picture
  layer), `pimble-server --test search` (a board node created with its board and found by
  its words), `pimble-plugins`; `cargo check --workspace --all-targets` and the wasm check
  clean.
- Desktop, an isolated release app (own port 7473, own config and data, a scratch store),
  driven over rinch's debug port (`PIMBLE_DEBUG_IMAGES` names the files "New Image..."
  takes instead of the dialog): New Whiteboard and New Image with two PNGs (three rows in
  order, the right icons), the image shown 1:1, a pen stroke, the markup toggle both ways,
  "Open in Split View" with the same board in both panes and a stroke from the second
  appearing in the first, wheel zoom, select and drag, Delete, Ctrl+Z bringing it back, a
  stroke on the whiteboard, then a restart with both panes and both boards back.
- Chromium (Playwright) on the local stack, a plain hosted store, the stack's own made-up
  account: New Whiteboard, a pen stroke in one tab arriving live in a second tab, New
  Image with two files through the file input, a synthetic file drop on the store row and
  a synthetic paste with a row selected (nodes "dropped square" and "Picture"), and the
  stroke's element stamped with the account's id and name on the hosted disk.

**Not exercised / known gaps:**
- Firefox was not run. A real OS drop on the desktop tree and the desktop "Paste Image"
  with a real clipboard bitmap were not driven (the debug port cannot), nor a read-only
  board.
- The desktop has no Ctrl+V on a tree row: rinch gives elements no key events and a menu
  chord with Ctrl would take Ctrl+V from the editor, so it is the row's "Paste Image".
- (Done since: see "Pointer events" below. Ctrl+wheel and a pinch zoom, a plain wheel
  pans.)
- Desktop pointer events are logical pixels and frames physical; the scale is read from
  the surface's box (`bounds_signal`), the browser's from `devicePixelRatio`.
- Hit testing is by element bounds, so a click inside a long diagonal stroke's box selects
  it. No marquee, resize or rotate; the selection outline is a DOM box.
- A board pane does not follow a transplant of its node (documents do); a board in a
  pane whose store closes stays as it was, as documents do.
- Images wave 2 and 3 still apply: pictures on boards do not replicate, and an encrypted
  store in the browser takes no image node ("Pictures in encrypted stores ...").
- (Done since: pen pressure is stored and drawn, "Pointer events" below.)

**For rinch's pointer-event PR (pressure, pinch, capture)**, built since ("Pointer
events" below); what was planned, in
`boards.rs::on_event`: pen pressure into the third point value (it is stored per point
already), pointer type to choose pen vs. touch-pan, a pinch gesture into `zoom_at` plus the
same settle timer, wheel modifiers for Ctrl+wheel versus plain-wheel pan, and pointer
capture on the desktop so a stroke or drag that leaves the surface keeps its moves and
gets its release (the browser already captures on `pointerdown`).

## Built in waves 2 and 3

Built 2026-10-09/10 on branch `boards`, shared `pimble-app` code for the desktop and the
browser (`crates/pimble-app/src/boards.rs` and `boards/{tools,ui,nav,export}.rs`), over
`pimble-board` additions. Presence was left out (Joe deferred it, section 7).

**pimble-board:**
- `Element::hits` / `BoardSnapshot::hit_precise`: hit testing on what an element paints,
  in element space through its world transform (a stroke's or line's distance, an
  unfilled shape's outline, a filled one's or one with text's inside, a frame's border
  and name only, a connector's route and label; groups and spotlights never).
  `within(rect)` for the marquee (wholly inside), `frames()`.
- `ViewOptions` carried by snapshots through `update`: `color_by_author` (every element
  but pictures in `author_color(id)`, ten colors by an FNV hash; unauthored grey; a set
  fill becomes the same color, translucent), `show_all` (every layer and the markup
  drawn, nothing stored changed), `card_labels` (a card's live title and icon color by
  its link). `BoardView::set_options`, `BoardView::set_grid` (lines under the elements,
  multiplied by 5 until 8 px apart).
- `render_area` (a rectangle, optionally only some elements and their members, trimmed or
  not; `render_extent` uses it), `selection_bounds`, `RgbaImage::encode_jpeg`,
  `render_svg` (the same `tiny-skia` paths the raster renderer fills, serialized; pictures
  as base64 data URIs with their crop; text as glyph outlines). `paint.rs` builds every
  part an element paints once, for both.
- `pimble-crdt`: `NodeDoc::links()` also answers the node cards on the node's boards
  (`f:board/<id>`, `f:boards/<board>/<id>`), so the boards that point at a node count in
  its backlinks (the index's `links`); the backlinks UI is still deferred.

**The app:**
- **Tools** (the one toolbar; their letters in the tooltips): select V, hand H, pen P,
  highlighter M, line L, arrow A (heads at the end, the start, both or none), rectangle
  R, ellipse O, diamond D (fill from the style), text T, numbered step N, sticky note S,
  connector C, frame F, eraser E (whole elements, never locked ones). Shift: lines at
  45°, squares and circles, moves along one axis, proportional corner resizes, rotation
  in 15° steps. Shapes, lines and frames are made once the drag passes 3 px and written
  every 80 ms while dragged; a click with a shape tool makes one of the usual size.
  Plain letters reach a board only while it has the keyboard, so none collide with the
  app's own keys. An image opens on the arrow, a whiteboard on select (chosen once its
  board is here).
- **Text** is typed into a real `<textarea>` over the surface, at the element's place
  and size on screen: a new text element is made by its first character, every later
  keystroke is `set_text` (the smallest edit), so two people typing in one sticky note
  merge (verified: two browser tabs typing at opposite ends of one note at once both
  end with both edits). The input is given its text only when typing starts or a
  collaborator's typing merges in, never echoed per keystroke (that dropped fast
  letters in the browser). Escape ends it; an emptied text element goes. Double-click
  types into a text, sticky note, shape (text inside shapes), frame name or connector
  label; double-click on a node card opens its node in the focused pane (`follow_href`).
- **Selection**: hit tests on geometry, outermost group picked as one, shift adds or
  takes out, a marquee (wholly inside; shift adds), Ctrl+A. Outlines per element and
  one box with eight resize handles and a rotate handle (DOM overlays; the press is
  judged in `handle_at`). A plain shape's resize edits its geometry (its outline keeps
  its width); strokes, text, groups and rotated elements scale their transform (rotated:
  uniformly). Rotation turns each element about the selection's center. Arrow keys
  nudge (shift ×10, a grid step when snapping), Ctrl+D duplicates (a group with its
  members), Ctrl+]/[ (with shift: to front/back), Ctrl+G / Ctrl+Shift+G, Ctrl+L lock,
  Delete (a group with its members); the Arrange menu has all of these plus align
  left/centers/right/top/middles/bottom and distribute across/down. Locked elements can
  be selected (to unlock them) but not moved; elements in locked layers are not picked.
- **Style panel** (a side panel per board, toggled from the toolbar): color (the marks'
  red, the appearance palette, black, white, and rinch's `ColorPicker`), fill (none or the
  palette), width, dash, arrow heads, opacity, font size S/M/L/XL, alignment. Selecting
  loads the first element's style; a change writes every selected element (one style key
  each, so concurrent changes of other keys merge). A highlighter left on the first red
  draws in the highlighter's yellow.
- **Layers panel**: rows top first with visibility and lock (shared, undoable), the name,
  the element count; click a row to pick it (new elements go there; a locked one refuses
  them with a sentence), click the picked row's name to rename it (an input over the
  list, Enter keeps, Escape drops), drag a row onto another to reorder (writes `order`),
  the picked layer's opacity slider, New (above the picked), Delete (with its elements;
  never the last), Merge Down, Move Selection Here.
- **Whiteboarding**: sticky notes (grow with their text), text inside shapes,
  connectors from an element (or a point) to an element (or a point), snapping to the
  side whose middle the end is near, routed at render time so they follow what they
  join; frames (named "Frame N", drawn below their contents, adopting what is wholly
  inside when drawn; an element moved into or out of a frame joins or leaves it, its
  transform compensated; moving a frame moves its members); groups; node cards (a tree
  row dropped on the board, desktop through the surface's `Drop`, browser through the
  canvas's `ondrop`; the dropped node's title is stored as the fallback text, the live
  title and color come from the node cache, tracked, so a rename or a color change shows
  at once); grid and snapping per viewer (toolbar; not remembered across restarts);
  the navigator (a second surface: the visible extent and the view, re-rendered 250 ms
  after changes, the view's rectangle over it, press or drag to move the view, the frames
  listed below it, a click fits one).
- **Export** (the toolbar's Export menu; Ctrl+C is Copy Picture): what is rendered is a
  selected frame's rectangle, else just the selected elements trimmed, else the page or
  the visible extent trimmed. Copy Picture (desktop: `rinch::clipboard::copy_image`;
  browser: `navigator.clipboard.write` with a `ClipboardItem` looked up at run time), Save
  as PNG / JPEG / SVG, Save with All Markup (PNG, `show_all`, the board unchanged), Save
  Original As (a selected picture's stored bytes), Flatten into New Image (a PNG of what
  the board shows, a new image node beside the board). Desktop saves through rinch's save
  dialog (`PIMBLE_DEBUG_SAVE_DIR` names a directory instead, for driving it over the
  debug port), the browser downloads. Exports are never colored by author.
- **Attribution**: on a board with two or more authors the element under the resting
  pointer says "Arrow, by bob, 2 hours ago (changed by alice, 5 minutes ago)" ("changed",
  not "moved": the document records who edited last, not what); "before sign-in" for an
  unauthored one. Color by Author is a toolbar toggle for the focused pane only.
- **Markup control**: shown while the pointer is over the board (600 ms grace on leave)
  or over the control itself; always shown, dimmed, while the markup is hidden.

**Verified (2026-10-10):**
- Tests: `pimble-board` (37 incl. hit testing, options, grid, export and SVG),
  `pimble-crdt` (153, incl. a card counted as a link), `pimble-app` (141, incl. typing
  making and editing a text element, group/duplicate/ungroup/delete, a locked layer
  refusing new elements, constraints, resize, rotation, grid snapping, the attribution
  sentence), `pimble-server --test search`; wasm check of `pimble-app --features web`.
- Desktop, isolated release app (port 7473, own config and data, a scratch copy of wave
  1's store), over rinch's debug port: every shape tool, steps, highlighter, a click in
  an unfilled rectangle selecting nothing and on its edge selecting it, the empty
  corner of a diagonal arrow's box selecting nothing, resize and rotate by the handles,
  text and a sticky note typed into, a connector following a moved sticky note, a frame
  moving its text, the eraser, a marquee, group and duplicate, a new layer drawn into,
  renamed and reordered by drag, a layer hidden, Save as PNG / JPEG / SVG / with all
  markup (the SVG rendered with `rsvg-convert` matches the PNG), Copy Picture (the
  clipboard holds `image/png`), Flatten into New Image (a new node), a tree row dropped
  as a node card and double-clicked open, Color by Author, the grid with snapping, undo
  of typed text, a restart. Screenshots in the scratchpad's `tools/` (`02-shapes`,
  `03b-edge-click`, `05-select-resize-rotate`, `06b-text-sticky`, `07-connector-frame`,
  `08-eraser-marquee`, `09-group-dup-layers`, `11-layers`, `14-png-vs-svg`,
  `13-after-export`, `15-card`, `16-card-opened`, `17b-by-author`, `20-reordered`,
  `21-desktop-fast-typing`, `25-undo-text`, `26-grid-snap`).
- Chromium (Playwright) on the local stack, a plain hosted store, fixture accounts
  alice and bob (bob an editor): shapes and an arrow drawn in one tab arriving in a second
  tab, fast typing into a sticky note, two tabs typing into one note at once converging,
  bob's arrow and alice's note each attributed on hover, a tree row dropped as a card
  whose title and icon color follow a rename and an Appearance change, SVG and PNG
  downloads (`web-05-two-tabs`, `web-07-attribution`, `web-08-card`, `web-11-card-live`).

**Not built, or known gaps:**
- Wave 5 kinds (callout, spotlight, crop) have no tools; the renderer draws them. No
  smart guides; no shadow control in the style panel; no endpoint handles on lines and
  arrows (they resize with the box).
- A node card's icon is a square in the node's color, not its Tabler glyph (the renderer
  has no icon outlines).
- Desktop: when typing ends the board hears keys again only after its next press (rinch
  can give a surface the keyboard only through a press; a `RenderSurfaceHandle::focus`
  would fix it upstream). The layer name is edited in one input above the list, not in
  the row (rinch cannot focus an input created inside a reactive list row from code).
- Moving, resizing and grouping treat a frame's or group's transform as a translation:
  members of a rotated or scaled group keep only its offset when ungrouped, and a
  member's move inside a rotated parent is not rotated into it.
- Connectors are straight, from side middles or where the line leaves the box; no
  routing around elements.
- Exports and the navigator draw pictures that have not arrived on this device as
  placeholders. Copy Picture in the browser was not driven (clipboard permission); Firefox
  was not run; a read-only board and an encrypted store's board in the browser were not
  driven with the new tools.
- The markup control's chevron with a checkbox per layer (section 5) is not built (the
  layers panel does it); grid, snapping, the side panel and Color by Author are not
  remembered across restarts.

## Pointer events (pen pressure, touch, pinch, capture)

Built 2026-10-10 on branch `boards`, on rinch `main` at 2c1b02c (joeleaver/rinch#1502
pointer events, #1501, #1500).

**What changed:**
- **The board surface takes pointer events** (`RenderSurfaceHandle::set_pointer_events`,
  in `boards.rs::surface`); `boards/tools.rs` handles `PointerDown/Move/Up/Cancel` and
  `Pinch` (the navigator keeps plain mouse events). One gesture at a time: the pointer and
  button that began it (`Session::driver`) drive it, and only the release of that button
  ends it; other pointers' presses meanwhile, and a chord's other button, start nothing.
- **Pressure**: a pen's (and a finger's, where the screen measures force) `pressure` is
  each point's third value; a mouse always writes 0.5. A release reports no pressure, so
  the last point keeps the last pressure drawn with; a momentary 0 mid-stroke keeps the
  last too.
- **Drawn by pressure** (`pimble-board`, `paint.rs`): a solid pen's width at a point is
  its style's width times `pressure_width(p) = 0.25 + 1.5 p` (exactly the width at 0.5).
  A pen whose points share one pressure strokes its polyline as before; otherwise it is a
  filled outline, the union (nonzero, every part wound the same way) of a disc at each
  point and the band between each pair of neighbors' discs along their outer tangents.
  The same shape is used by the renderer, the live stamps (`BoardView::extend_stroke`,
  segment by segment), the SVG export (a filled `<path>` instead of a stroked one), the
  element's bounds and hit testing (the width interpolated along each segment). A
  highlighter and a dashed pen keep one width.
- **Touch**: one finger acts as the tool does (the pen draws, the hand pans, select
  selects and drags); a second finger cancels what the first began and the two pan (the
  move of their middle) and zoom (`Pinch`) until both are lifted; a third is ignored.
- **The eraser end** (`SurfacePointerKind::Eraser`) erases whatever tool is chosen.
- **Zoom and pan**: `Pinch` (two fingers, a trackpad pinch, Ctrl+wheel) is `zoom_at`
  about its point plus the existing 150 ms settle; a plain wheel pans, vertically, or
  sideways when the event says so (`delta_x`) or shift is held. The per-backend sign of
  the old wheel zoom is gone (rinch's `Pinch` agrees on both); the plain wheel's delta
  sign still differs per backend and `wheel_pan` maps both to "the page would scroll
  that way".
- **Capture and cancel**: a stroke or drag that leaves the surface keeps going and ends
  where it is released (rinch's capture, on both backends now). `PointerCancel` and
  Escape take the gesture back (`cancel_gesture`): a stroke or a shape or connector being
  made is deleted, what a move, resize or rotation wrote is written back, a marquee leaves
  the selection it started from (an erase is not taken back; undo has it). A stroke's
  element is now written with its first append (about 100 ms in) rather than at the
  press, so a stroke cancelled at once, or a finger that becomes the first of a pinch,
  writes nothing at all; a stroke released sooner is written once, whole.

**Verified (2026-10-10):**
- Tests: `pimble-board` (new `tests/pressure.rs`: a stroke at 0.5 matches a plain
  tiny-skia stroke of its polyline pixel for pixel and is a stroked path in SVG; pressure
  widens a pen where it was harder, its bounds and hit testing follow; a zigzag with
  changing pressure has no holes; a highlighter ignores pressure; live stamps of a
  varying stroke are within 40 pixels of its settled render, the existing tolerance), all
  pimble-board tests; `pimble-app` (143, incl. `pointers_draw_with_their_pressure_and_
  cancelled_strokes_leave_nothing`: a pen's pressures stored and written once on release,
  a second pointer ignored mid-stroke, a mouse at 0.5, a cancelled stroke writing
  nothing, a finger then a second finger becoming a pinch that pans and writes nothing,
  the eraser end with the pen tool chosen; and `a_wheel_pans_the_way_the_page_would_
  scroll`); release check of the wasm build.
- Desktop, an isolated release app (port 7473, own config and data, a scratch copy of
  the wave 2/3 store), over rinch's debug port: a plain wheel pans down and sideways
  without zooming, Ctrl+wheel (Ctrl held through the debug port's `mouse_down`
  modifiers) zooms in and out with the point under the cursor fixed, a pen stroke dragged
  off the surface over the side panel and released there keeps drawing and ends there
  (moves over the board afterwards draw nothing), a sticky note dragged off the surface
  and released outside lands there, Escape mid-drag puts the dragged element back.
  Screenshots `d01`-`d11` in the scratchpad's `pointer/`.
- Chromium (Playwright) on the local stack with the stack's made-up account, an
  encrypted store: a CDP pen stroke (`Input.dispatchMouseEvent`, `pointerType: pen`,
  force 0.05 to 1.0) drawn thin to thick beside a mouse stroke of one width; the eraser
  end (a synthetic `PointerEvent` with `buttons: 32`) erasing the mouse stroke with the
  pen tool chosen; with touch emulation, a one-finger stroke drawn, a finger stroke ended
  by `touchCancel` gone, a finger joined by a second spreading and moving (zoomed about
  their middle and panned, the first finger's stroke not written); Ctrl+wheel zooming
  out, a plain wheel panning; a mouse stroke leaving the canvas, released outside, then
  moves over the canvas drawing nothing. Screenshots `web-w06`-`web-w14`.

**Not exercised / known gaps:**
- No real pen, tablet or touch screen was available: pressure, the eraser end, touch and
  the pinch were simulated (CDP in Chromium, synthetic events, unit tests). The desktop's
  pen and touch paths (winit's tablet force, two touches, a trackpad's `PinchGesture`)
  and its capture ending on window blur were not driven; only the mouse and Ctrl+wheel
  were, over the debug port.
- Firefox was not run; its wheel reports lines, so a plain wheel there pans about three
  pixels a notch (rinch converts lines only for Ctrl+wheel; the plain wheel event carries
  no delta mode).
- Smoothing (two passes of a three-point average on release) averages pressure as well as
  position, so a sharp change of pressure softens; a fast zigzag flattens, as before.
- A stroke being drawn appears to collaborators about 100 ms after the press instead of at
  it (its element is written with its first append).
- Cancelling an erase does not bring back what it erased (undo does).


## Built in wave 4 (boards in text)

Built 2026-10-10 on branch `boards`, on rinch `main` at 2b9346e (joeleaver/rinch#1500 the
atom's `board` (since replaced by `data-board`, #1518) and `width`, #1501 `set_image_source` and `on_image_hover`, #1503 attrs
merging one by one). Shared `pimble-app` code for the desktop and the browser, one module,
`crates/pimble-app/src/text_boards.rs`, plus the board view's markup mode in `boards.rs`.

**pimble-core / pimble-crdt / pimble-board:**
- `pimble_core::BoardUrl` (`pimble-board:<store>/<node>[#frame=<element id>]`), one parser
  and printer; `BoardUrl::link()` is the board as a `pimble:` link.
- `NodeDoc::links` counts an `image` atom whose `src` is a `BoardUrl` as a link to the board
  (its `alt` the words), so backlinks and the index's `links` see embeds (search indexes the
  embed as a link; the board's own text stays indexed on the board).
- A transplant carries the `data` root as a fresh copy **with its yrs types**
  (`NodeDoc::fresh_data`/`put_data_copy`, `CuttingNode::data_copy`). Before this a
  transplant went through JSON, which turned a board's whole values into nested maps and its
  point buffers into arrays of numbers: a board node moved between stores lost its geometry,
  and so would a note's marked-up pictures.
- `BoardTxn::copy_from(&RawBoard)`: another board, layers and elements with their ids,
  authors and times, page and markup switch, written into a new board (pasting, converting).

**Marked-up pictures in text:**
- The app installs `set_image_source` once, outside any render (`app::build_view`): a
  `pimble-blob:` picture whose atom has a `data-board` asks for `<blob url>#board=<id>`. The
  `pimble-blob:` loader hands those to `text_boards::load`, which answers with the board
  rendered (`render_extent`, its visible extent trimmed, one unit per CSS pixel, no wider
  than the widest pane's text) or, while it is drawn, with the plain picture.
- The board is read from the note's document as this window holds it (`HOSTS`: loaded when
  a pane opens the note, fed `RemoteChanges`, reconciles and every board edit made here). A
  change re-renders 150 ms later (debounced), off the UI thread on the desktop, and
  `reload_image` swaps the picture; an unchanged rendering is not handed over again.
- **Mark Up...**: the picture's menu (a "⋯" button in the floating controls) or a
  double-click on the picture opens the board view over the pane (`PaneState::markup`; the
  pane's editor stays mounted, hidden; the board toolbar shows) with **Done**. The board is
  `data["board:<id>"]` of the note. A picture with no board gets one in the pane's copy of the
  note when the view opens (the picture at one unit per pixel in a locked "Picture" layer
  and "Layer 1" above, as an image node starts), so opening and closing writes nothing; the
  **first mark** sends that board, then writes the atom's `data-board` (`SetNodeAttrStep`
  through the editor, so it is an ordinary text edit), then the mark. These are three
  updates, in that order, not one: the text's atom lives in the editor's collaboration
  document, the board in the board session's copy, and no transaction spans both. Per-pane
  undo in the board view never takes back the board itself.
- **The floating controls** (contract section 5) over the picture under the pointer
  (`on_image_hover`): the eye with "Markup · N" (or dimmed "Markup hidden", which stays
  while the markup is hidden), writing the board's `markup_visible`, and the "⋯" menu
  (Mark Up..., Convert to Image Node, Open Board, Open Board in Split View). A picture with
  no markup shows only the menu. The controls keep themselves while the pointer is on them
  (their own enter/leave, 300 ms grace), stay inside the text's box when the picture is
  half scrolled out, and hide on a scroll or a key (rinch#1506: the desktop never clears its
  hover after a wheel scroll). A reader sees the state and is refused the toggle with the
  read-only sentence.

**Board nodes embedded in text:**
- `pimble-board:` is a registered image scheme answered by the same module. An embed is
  resolved with `resolveLink` (the server judges the read, `Reach` included, and follows
  `became`), then the window subscribes to the board node (counted in
  `AppStore::is_editing`, so a pane on the same node and an embed share one subscription)
  and fetches it with `ReconcileNodeContent` from an empty state vector; it is drawn live on
  every change (a stroke in another pane or another device shows as it is drawn, debounced).
  `#frame=` draws that frame's rectangle. Placeholders, drawn by `pimble-board` with one
  sentence: "This board was deleted.", "This board has not arrived yet." (also a store not
  here, a missing node), "You cannot see this board.", "This board is empty.". An embed no
  open text shows is let go of (a sweep 1.5 s after an edit, and when a note closes), its
  subscription with it; one that comes back (undo) is held again.
- Making one: Edit > "Insert Board..." (the link picker limited to board nodes; in the
  browser the query may be typed into the note, where it is replaced), a board node's
  `pimble:` link pasted with nothing selected (when this window knows the node is a board),
  a board row dragged from the tree onto the text (goes in at the caret: rinch gives no
  position for a point), and Convert to Image Node.
- Clicking an embed opens the board node in that pane (Ctrl/Cmd+click: split view, the
  pane split to the right), through `follow_href`, so `became` and the sentences are the
  links'. "Mark Up..." on an embed opens the board node itself. The eye on an embed writes
  the board node's own `markup_visible` (hiding it there hides it in its pane and every
  embed).

**Convert to Image Node:** a board node under the note (title the picture's `alt`, or
"Picture"; the photo icon) whose board is a copy of the picture's markup (or a new image
board for an unmarked picture), naming the same blob; `NodeInit::request` /
`NodeCreated::request` say which node was made, and one editor transaction then sets the
atom's `src` to the embed, its `alt` to the title and removes its `data-board`. Undo in the text
takes the embed back to the picture (with its board); it does **not** delete the node made.

**Clipboard and transplant:** `PicturesPlugin::handle_paste` (now per pane) keeps
`pimble-board:` pictures and, for every pasted picture with `data-board`, mints a fresh id
and copies the board (found in any note this window holds) into the pasting pane's note a
turn later; the same note too. A board this window does not hold (copied in another window
or app) cannot come along: the picture is pasted without it. There is no Pimble MIME part:
rinch has no copy hook, so the board travels by id within a window, not on the clipboard.
`transplantNode` carries each `board:<id>` key of `data` with the content (above).

**Verified (2026-10-10):**
- Tests: `pimble-core` (BoardUrl), `pimble-crdt` (155, incl. an embed is a link, a
  transplant keeps the shapes of data), `pimble-board` (incl. a copied board is the same
  board and then its own), `pimble-app` (148, incl. `text_boards::tests`: the first mark
  mints the board and names it on the picture, three updates in order, Done; a pasted
  marked-up picture brings a copy of its board, and one from elsewhere goes in plain;
  converting makes an image node with the markup and embeds it, undo takes the embed back;
  an embed is resolved, held, drawn, a no-access one shows its sentence, and it is let go
  of), `pimble-server --test search --test links`; release check of the wasm build.
- Desktop, an isolated release app (port 7473, own config and data, a scratch store), over
  rinch's debug port: Insert Image, the "⋯" menu, Mark Up..., an arrow drawn from outside
  the picture and a pen stroke, Done: the text shows the visible extent with the arrow;
  the eye hides the marks, a restart keeps them hidden ("Markup hidden"), the eye shows them
  again; Insert Board... with the picker limited to boards; a click on the embed opens the
  whiteboard; split view with the whiteboard in one pane and the note in the other, a pen
  stroke in the board pane appearing in the embed; Convert to Image Node (a child "sunset"
  under the note, the embed with the copied markup); its eye hiding the marks in the embed
  and in the board node's own pane, and the board pane's toggle showing them in the embed;
  a board row dropped on the text; a restart with both panes back. Screenshots `01`-`33`
  in the scratchpad's `text/`.
- Chromium (Playwright) on the local stack, a plain hosted store, the stack's made-up
  account, two tabs: a picture inserted, Mark Up... from the controls, an arrow drawn in
  tab 1 showing in tab 2's text; a synthetic paste of the marked-up picture's HTML in tab 2
  going in with a fresh board, drawn with the copied arrow; a whiteboard embedded with
  Insert Board... (typed into the note), first as "This board is empty.", then redrawn live
  in tab 2 when a stroke was drawn on the whiteboard in tab 1. Screenshots `web-01`-`web-14`.

**Not built, or known gaps:**
- The board and the atom's `data-board` are separate updates (above), so a peer can see the
  board a moment before the picture names it; and undoing the text does not take back a
  board (unreferenced boards stay in `data`, as the contract says, swept later).
- Convert's undo leaves the image node; a drag of a tree row inserts at the caret, not at
  the drop point; dragging a picture inside the editor is rinch's own move (the board id
  goes with it).
- Copying between windows, apps or tabs carries no board (no clipboard part; rinch has no
  copy hook to add one). A real clipboard was not driven on the desktop (the debug port
  cannot read the Wayland clipboard): desktop paste is covered by the unit test only.
- The controls are positioned from the last hover report: an image that grows or moves
  under a resting pointer (its board redrawn, the toolbar changing height) leaves them at
  the old place until the pointer moves. The "⋯" menu is not flipped at the window edge.
- The editor has no context menu, so "its context menu's Mark Up..." is the controls' "⋯"
  menu; a single click on a picture still only selects it.
- In the browser the Board picker's query goes into the note (rinch-web does not hand
  printable keys to `on_key`); it is read back from there and replaced on a pick. The same
  likely holds for the Ctrl+L picker in the browser; not looked at here.
- Rendering in text is at one unit per CSS pixel, not times the screen's scale, so a board
  in text is not sharp on a HiDPI screen; it is re-rendered only when the board changes,
  not when a pane is resized.
- Markdown `![title](pimble-board:...)` both ways, the MCP's `render_board`, and export of a
  picture in text are not built: Markdown import refuses every image today, and
  `render_board` is wave 5. Blobs are still not copied on a transplant between stores
  (images wave 2): the copied board names the source store's blob URL.
- Firefox was not run; an encrypted store in the browser takes no pictures yet (images wave
  3), so marked-up pictures there were not driven.

## Built in "pictures onto a board" (2026-10-20)

Section 1's "Pictures go on any board, as many as wanted: paste, drop files, or the
picture tool" had not been built (Joe, 2026-10-20: "There doesn't seem to be a way to add
an image to a whiteboard."). Built on rinch `main` at 08b8a6f, shared `pimble-app` code
for the desktop and the browser (`boards.rs`, `boards/{tools,ui}.rs`).

**What exists:**
- **Three ways in**, on a whiteboard or an image node alike: Ctrl+V on a board that has
  the keyboard (desktop: `rinch::clipboard::paste_image`, claimed by the surface's key
  handler; browser: the page's `paste` event while a board's surface is focused and no
  text is being typed on it, `keyboard_board`), image files dropped on the board, and the
  toolbar's picture button ("Insert pictures", after the tools; several files; the
  desktop's file dialog, `PIMBLE_DEBUG_IMAGES` when driven over the debug port, the
  browser's hidden file input). A drop of a tree row still makes a node card.
- **One path for every picture added anywhere**: `boards::Dest` says where (`Under` a
  node: image nodes, as before; `Board(pane, point)`), and `add_image_files`,
  `paste_image`, `pick_images` and the browser's `read_files` all take one. Pictures are
  stored through the image-node queue (one `PutBlob` at a time, as a write of the board's
  node, scaled to fit, refusals as sentences); nothing but the `pimble-blob:` URL enters
  the document.
- **Placement**: the pictures of one paste, drop or pick are a batch, placed once the
  last is stored, in **one** `local_edit` (one `applyEdit`, one undo step): side by side
  in order, 16 units apart, each at one unit per pixel, centered on the drop point or the
  middle of the view, and all scaled down together when the row is wider or taller than
  90% of the view. They go in the layer new elements go to (the picked layer, else the
  top unlocked one), not a locked picture layer, so they can be moved, resized and
  deleted like anything else; an image node's own picture stays alone in its locked
  layer. They are selected and the select tool is chosen. A board closed or showing
  another node before its pictures were stored gets none of them, with a sentence.
- **Refusals**: a board this device may only read refuses all three with
  `READ_ONLY_REFUSAL` before anything is stored (`writable_or_say`); a board whose layers
  are all locked says so (`target_layer`).
- An image opens on the arrow tool when a picture sits in a locked layer (an image
  node's), so a whiteboard with pictures put on it still opens on select.

**Verified (2026-10-20):**
- Tests (`pimble-app`, 165): two pictures stored as writes of the board's node and placed side
  by side, centered, at their pixel size, in an unlocked layer, selected, in one
  broadcast, and taken back by one undo; a drop centered on its point beside an image
  node's own picture; a picture larger than the view scaled to 90% of it, its proportions
  kept; a read-only board storing nothing and saying the sentence; a board closed before
  its pictures arrived getting none. wasm check of `pimble-app --features web`.
- Desktop, isolated release app (port 7492, own config and data, a scratch store), over
  rinch's debug port: the picture button placing two files side by side, centered and
  selected; Ctrl+Z taking both back and Ctrl+Shift+Z bringing both; a placed picture
  dragged; Ctrl+V with a PNG on the Wayland clipboard (`wl-copy`) placing it centered on a
  whiteboard and on an image node, above its locked picture; all of it there after a
  restart.
- Chromium (Playwright) on the local stack, a plain hosted store, the stack's made-up
  account: two PNG files dropped (a synthetic `drop` on the board's canvas) landing side
  by side centered on the drop point; a synthetic `paste` with the board focused placing
  its picture in the middle of the view; the picture button's file chooser taking two
  files. A real Ctrl+V there reached the board too, but that test Chromium hands over the
  desktop's clipboard picture as an empty `image.png`, which is refused as not a picture.

**Not built, or known gaps:**
- **A desktop drop lands in the middle of the view, not at the drop point**: rinch hands
  an `onfiledrop` handler the paths alone (its `FileDropped` event has the position and
  drops it). The app takes a point already (`Dest::Board(pane, Some(point))`, as the
  browser's drop does); a rinch mechanism that gives a file drop its position (or a
  surface its own file-drop event) is the fix. A real OS drop was not driven on the
  desktop (the debug port cannot make one); the drop handler is the same
  `add_image_files` the picture button runs.
- Pictures still do not replicate (images waves 2 and 3): a board with pictures put on it
  arrives on another device as placeholders.
- No picture "tool" in the sense of a mode that places where clicked: the toolbar's
  button opens the file dialog and places in the middle of the view.

