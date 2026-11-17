# Drawing contract: vector and paint tools, and the canvas redesigned around them

Status: **decided; all five waves built (model and UI), loose ends closed 2026-11-17** (see "Loose ends closed" at the end for what remains; wave 5's open questions for Joe are in its model section). Written 2026-11-14 from Joe's answers of that day
("Decisions" below). Builds on docs/IMAGE_MARKUP_CONTRACT.md (the board document, layers,
locks, undo, attribution) and docs/CANVAS_CONTRACT.md (every picture is a canvas). Where this
contract and those disagree, this one wins.

A canvas today is a whiteboard: freehand pen and highlighter, fixed shapes, sticky notes,
connectors, steps, frames, cards. This contract adds two real tool suites, **vector
drawing** and **painting**, lets both live in one canvas on layers of their own, and
redesigns the canvas tab around them: the tools leave the app's toolbar and float over the
canvas.

## Decisions (Joe, 2026-11-14)

1. **The whiteboard tools stay** as a third group, Diagram. Real diagramming tools come
   later, in that group.
2. **Vector and paint mix in one canvas**, layer by layer ("that's the fun").
3. **Today's pen becomes the vector Pencil; the highlighter becomes a paint brush preset.**
   Markup on pictures keeps working with both.
4. **Paint layers hold pixels at a fixed density**: 2 pixels per board unit (twice the
   screen at 100% zoom). Zooming far in shows pixels, as in any painting app.
5. **The canvas tab is redesigned** with floating toolbars and panels ("overdue anyway").

Wave 3 (Joe, 2026-11-17, accepting the recommendations as built):

6. **Subtract keeps the back shape**: the bottom operand minus everything in front, in the
   bottom operand's place and style; union, intersect and exclude keep the top operand's.
7. **Flipping text moves it to its mirrored place and never mirrors the glyphs.**
8. **A new gradient fades the current color to transparent.**
9. **Lines count as zero area when combining**; an "Outline Stroke" that makes a stroke a
   filled path may come later.

Already settled by earlier contracts and kept: everything is co-edited through the board's
yrs document, undo is per person, a lock is a shared guard, visibility is the node's.

## 1. Layers have a kind

A layer is a **vector layer** (it holds elements, as every layer does today) or a **paint
layer** (it holds paint operations). The layer map gains:

```
layers: Map<layer id, Map {
  ...name, order, visible, locked, opacity, note, field:*  (unchanged)
  kind: "vector" | "paint",      // absent = "vector": every layer that exists today
  blend: "normal" | "multiply" | "screen" | "overlay" | "darken" | "lighten" | ...,
}>
```

- Blend mode and opacity apply to the layer as a whole, composited in layer order. Vector
  layers take a blend mode too.
- **Tools pick their layer.** A vector tool draws into the selected layer if it is a
  vector layer, else into the nearest unlocked vector layer above it, else into a new one
  made above it ("Vector 1"). A paint tool does the same with paint layers ("Paint 1").
  Nobody has to make a layer before drawing; the layers panel shows where it went.
- The highlighter preset on a picture makes (or reuses) a paint layer named "Highlights"
  above the picture's layer, blended multiply.
- `markup_visible` off hides paint layers as it hides every element that is not a picture.
- Merge Down of a vector layer onto a paint layer rasterizes it there (one `pixels`
  operation); a paint layer onto a vector layer is refused with a sentence.

### Layer groups

A group is a layer entry of `kind: "group"` that holds other layers, groups included, to
any depth. Every layer entry gains a `parent`:

```
layers: Map<layer id, Map {
  ...
  kind: "vector" | "paint" | "group",
  parent: group id?,             // absent = top level; moving into a group is setting this
  order: String,                 // now the order among its siblings
  blend: ... | "pass-through",   // groups only: the default
}>
```

- **Hiding or locking a group hides or locks everything in it**, for everyone, as a layer's
  own flags do; the children keep their own flags, which apply again when the group is shown
  or unlocked. Tools never pick a layer inside a hidden or locked group.
- **Blending**: a group in `pass-through` at 100% opacity draws its children as if they
  were not grouped. Any other blend mode or opacity draws the group into its own buffer
  first and then blends that buffer, as painting apps do.
- **Concurrency**: `parent` is last writer wins like `layer` on an element. A group deleted
  while someone moves a layer into it leaves a `parent` naming nothing, which reads as top
  level. Two people nesting A into B and B into A at once make a cycle; it is read
  deterministically: walking up from each layer, the first entry met twice, the one with
  the smaller id, is read as top level. Nothing is rewritten, so every device reads the
  same tree.
- **Deleting a group** deletes what is in it (one undo), with the same lock rules as
  deleting a layer; **Ungroup** moves its children up to its place, in order.
- **Merge Group**: all vector children become one vector layer; any paint child makes it one
  paint layer, with the vector layers rasterized into it.
- **The panel**: groups fold and unfold (that is each viewer's own state, not the
  document's), layers drag into and out of groups and between them, "New Group" and "Group
  Layers" (Ctrl+G while the layers panel has the focus; on the canvas Ctrl+G still groups
  elements) put the picked layers in a new group at the topmost one's place.
- Older versions draw the layers inside groups as top-level layers in their order and skip
  the group entries; the groups' own visibility and opacity are lost there.

## 2. Vector drawing

### The path element

A new kind, **`path`**: one or more subpaths of cubic Bézier anchors.

```
geom: {
  subpaths: [ { closed: bool, anchors: [x, y, in_dx, in_dy, out_dx, out_dy, mode] ... } ],
  rule: "nonzero" | "evenodd",   // holes
}
```

- `mode` per anchor: corner, smooth (handles collinear), symmetric (collinear and equal).
- The geometry is **one value, last writer wins**, like every element's geometry today:
  two people dragging points of the same path at once keep one person's path, never a
  broken one. Different properties (geometry, style, text, transform) still merge.
- A path is drawn with tiny-skia, as every element is, on the desktop and in the browser.

### Fill and stroke

`style` gains real paints and stroke detail:

- `fill` and `stroke` each: none, a solid color, or a gradient (`linear` or `radial`, its
  stops, and its handles in the element's own space so it moves and scales with it).
- `width`, `cap` (butt, round, square), `join` (miter, round, bevel), `miter`, `dash`
  (the existing presets plus a custom pattern), `opacity`.
- These apply to every closed shape (rect, ellipse, diamond, polygon, star, path, sticky)
  where they make sense; old elements read their present style unchanged.

### Tools

- **Pen** (P): click for a corner, drag for a curve, click the first anchor to close,
  Enter or Escape to finish an open path. Alt-drag a handle to break it.
- **Direct select** (A): move anchors and handles; double-click a segment to add an anchor;
  Delete removes the selected anchors; double-click an anchor switches corner and smooth.
  Marquee selects anchors.
- **Pencil** (N): freehand, fitted to Béziers on release (the error tolerance is a
  setting), so the line stays smooth and editable. While drawing it streams its raw
  points as today's pen does, so others watch it being drawn.
- **Shapes** (U, a flyout): rectangle (with corner radius), ellipse, polygon (sides),
  star (points, inner radius), line, arrow. **Shapes stay live**: a rectangle remains a
  rectangle with a radius you can change until someone edits one of its points with direct
  select, which turns it into a path in that same edit.
- **Combining** (with two or more selected): union, subtract, intersect, exclude. The result
  is one path in the top operand's place and style; the operands go, in one transaction
  (one undo).
- **Arranging**: align and distribute (exists, moves to the context bar), flip horizontal
  and vertical, bring forward and send back, group (exists).
- **SVG in**: pasting SVG markup or dropping an `.svg` file makes paths, groups and simple
  text. **SVG out** (exists) writes paths with their gradients and embeds paint layers as
  pictures.

Today's `pen` elements stay `pen` and draw as they do; the Pencil makes `path` elements.

## 3. Painting

### Paint is stored as operations, not pixels

Two people painting the same area would overwrite each other's pixels, and that breaks the
rule that everything is co-edited. So **a paint layer is an ordered list of operations**,
and every device replays the list to get the image:

```
paint: Map<op id, Map {
  layer: layer id,
  order: String,                 // fractional index within the layer
  kind: "stroke" | "fill" | "smudge" | "blur" | "move" | "pixels",
  ...per kind (below),
  author, created_at,
}>
```

- **stroke**: brush preset name and its settings (size, hardness, spacing, flow, opacity,
  pressure and tilt curves, jitter), color, blend (`normal` or `erase`: the eraser is a
  stroke that removes), and its points as appendable packed chunks (x, y, pressure, tilt),
  streamed while drawing exactly as pen points are today. Jitter is seeded from the op id,
  so every replay draws the same dabs.
- **fill**: seed point, tolerance, color, and the clip rectangle the filler's device
  computed when it filled (so an infinite canvas never fills forever, and every device
  fills the same area).
- **smudge** and **blur**: brush settings and points, like a stroke; they read the pixels
  under them at their place in the order.
- **move**: a selection region (a lasso polygon or a rectangle) and a transform: the pixels
  inside are lifted and put down transformed.
- **pixels**: a picture (a `pimble-blob:` blob) over a rectangle. Flatten makes one, and so
  do Merge Down from a vector layer and pasting an image into a paint layer.

Two people painting at once both appear, ordered by their `order`. Undo removes the person's
own last operation. Replay is deterministic on one device; between the desktop and the
browser a fill's edge may differ by a pixel where colors sit at the tolerance boundary,
which is accepted.

### Rendering and its cache

- A paint layer is drawn into **256 × 256 pixel tiles** at 2 pixels per board unit.
- Each device keeps the rendered tiles in a **local cache**, derived and disposable like the
  search index: memory first, then the user's cache directory on the desktop and IndexedDB
  in the browser, keyed by the layer and the operations that touch the tile. An appended
  stroke redraws only the tiles under its new points.
- **Flatten Layer** (the layers panel) replaces a layer's whole history with one `pixels`
  operation, in one transaction: for paintings whose history has grown long.
- Pictures in text and embedded canvases render paint layers the same way, on the
  background renderer.

### Tools

- **Brush** (B) with presets: round, pencil, ink, airbrush, marker, charcoal, watercolor-ish
  wet edge, and **highlighter** (multiply). Presets are data in `pimble-board`; settings
  can be changed per stroke from the context bar. `[` and `]` resize, pen pressure and tilt
  drive size and opacity per the preset's curves.
- **Eraser** (E): a brush that erases, in paint layers. In a vector layer it removes whole
  elements, as today.
- **Eyedropper** (I, or hold Alt with a paint tool): picks the color under the pointer from
  the composited canvas.
- **Fill** (G): flood fill with tolerance.
- **Smudge** (R) and **Blur** (Shift+R).
- **Select** (M rectangle, L lasso) in a paint layer: move, scale, rotate the pixels
  inside; Delete clears them (a `move` that lifts the pixels and puts nothing down).

## 4. The canvas tab, redesigned

The tools leave the app's toolbar. **Everything floats over the canvas**, so the canvas is
the whole pane and nothing pushes it around:

```
┌──────────────────────────────────────────────────────────────┐
│            [ context bar: options for the tool or selection ] │
│ ┌──┐                                              ┌─────────┐ │
│ │▲ │ select                                       │ Layers  │ │
│ │A │ direct                                       │ Meaning │ │
│ │✋│ hand                                         │ Frames  │ │
│ ├──┤                                              └─────────┘ │
│ │✒ │ pen   ┐                                                  │
│ │✎ │ pencil│ Draw                                             │
│ │▭◢│ shapes┘                                                  │
│ ├──┤                                                          │
│ │🖌◢│ brush ┐                                                  │
│ │⌫ │ eraser│ Paint                                            │
│ │🪣◢│ fill  ┘                                                  │
│ ├──┤                                                          │
│ │T │ text  ┐                                                  │
│ │▢◢│ sticky│ Diagram                                          │
│ │⤳◢│ more  ┘                                                  │
│ └──┘                                                          │
│ (●) color well                              [ − 100% + ] [map]│
└──────────────────────────────────────────────────────────────┘
```

- **Tool rail** (left, vertical): Select, Direct select, Hand; then Draw, Paint and Diagram
  groups. A button with a corner mark is a **flyout** (press and hold, or click the mark):
  it shows its siblings and remembers the last one used. Every tool has a letter; hovering
  shows its name and letter.
- **Context bar** (top center): the current tool's options, or the selection's when
  something is selected: stroke, fill, width, dash, caps for vector; preset, size, opacity,
  flow, hardness for a brush; tolerance for fill; font and alignment for text; combine,
  align and distribute for a multiple selection. It replaces the style section of today's
  side panel.
- **Color well** (bottom left): fill and stroke (or the brush color), a swap (X), recent
  colors, and a picker (hue ring and square, hex, the appearance palette).
- **Panels** (right, floating): Layers (with kind, blend, opacity, visibility, lock and a
  thumbnail per layer), Meaning, Frames. Each folds to its icon; the stack hides with Tab.
- **Zoom and map** (bottom right): today's navigator.
- **Small panes**: in a quarter of a split view the panels start folded and the context bar
  scrolls; the rail always shows.
- **Keys**: tool letters, Space held pans, Shift constrains, Alt picks colors while
  painting, X swaps colors, `[`/`]` brush size, Tab hides panels. The app's own shortcuts
  win where they collide.
- **The app toolbar** shows nothing of the canvas; it dims as it does for a pane with no
  document. Discussions' pin moves to the Diagram group.
- Both apps, from the same `pimble-app` code (the web matches the desktop).

The look is settled with a mockup before the build (wave 1).

## 5. The MCP

- `add_elements` takes `path` elements, written as an SVG path `d` string (what models
  already write well) plus fill and stroke; `get_canvas` returns paths that way.
- A `paint` tool adds strokes (preset, color, size, point lists) and fills to a paint layer,
  making the layer if it is not named. `render_canvas` shows paint layers.
- Layers report and take `kind`, `blend` and `parent`; `add_layer` makes groups too.

## 6. Waves

1. **The canvas UI shell**: a mockup for Joe first, then the floating rail with flyouts,
   context bar, color well, panels and keys, carrying today's tools, and the layers panel
   rebuilt with layer groups and blend modes. Nothing new to draw yet.
2. **Paths**: the `path` element, Pen, Direct select, Pencil, live shapes (polygon, star,
   corner radius), fill and stroke detail. MCP paths.
3. **Combining, gradients, flip, SVG in**.
4. **Paint layers**: layer kind and blend, the operation list, tile renderer and cache, the
   brush engine and presets, eraser, eyedropper, highlighter preset. MCP `paint`.
5. **Fill, smudge, blur, paint selections, Flatten, Merge Down across kinds**.

Each wave ships when it is done, in both apps, with its MCP part and tests.

## Spikes before building

- **Combining paths**: choose a Bézier boolean library that runs in wasm (`linesweeper`,
  `flo_curves`, or flattening through `i_overlay` and refitting) and test it on
  coincident edges and self-intersections.
- **Brush engine speed**: stamping dabs into tiles while drawing a long pressure stroke at
  4K, on the desktop and in the browser.
- **Replay cost**: opening a paint layer with 5,000 strokes cold, and with the cache.

## Built in wave 1 (model)

Built 2026-11-14 in `crates/pimble-board` and `crates/pimble-mcp`, no UI (the layers panel
still lists every entry flat, groups included, until the canvas UI shell is built).

- **Storage**: three new keys on a layer's map, nothing else changed: `kind` (`vector`,
  `paint`, `group`; absent reads as vector; written on every new layer), `blend` (the twelve
  modes plus `pass-through`; absent or unknown reads as the kind's default, pass-through on
  a layer that is not a group reads as normal) and `parent` (a group's id; `order` is then
  among that group's layers). `RawLayer` carries them, `copy_from` (Convert to Node) copies
  them. A paint layer stores nothing else yet and draws nothing. Search indexes layers'
  meaning as before; the new keys add nothing to it.
- **One resolution**: `pimble_board::layer_tree::resolve(&[RawLayer]) -> LayerTree`, pure
  and unit-tested, is what the snapshot, the renderer, the SVG export, the locks and
  `BoardTxn` all read: dangling parents and parents that are not groups read as top level,
  each cycle's smallest id reads as top level, siblings by order then id, draw order depth
  first with a group right after its children, inherited visibility and locks (`locked_by`
  names the lock that applies: the nearest locked group, else the layer's own), and
  isolation (a blend that is not normal or opacity below 1; a group not in pass-through at
  full opacity).
- **Rendering**: the fast path is unchanged (elements of layers that are not isolated draw
  straight into the target). An isolated layer or group gets a buffer of its own, opened at
  its first element in the drawing order and blended down with its opacity and
  `tiny_skia::BlendMode` when the order leaves it; one stack of buffers covers any nesting.
  **Deviation**: a layer's opacity below 1 now fades the layer as a whole (overlapping
  elements no longer show through each other), and an element's `opacity` is its style's
  alone. SVG export wraps an isolated layer's elements in `<g opacity mix-blend-mode>`.
  A layer's composite (its chain's ids, opacities, blends) and its place in the draw order
  are folded into its elements' fingerprints, so the raster cache redraws them when either
  changes.
- **Where new elements go**: `add_element` with no layer, and
  `BoardSnapshot::layer_for_new(None)` / `default_layer()`, take the top vector layer in
  draw order that is not effectively locked and not inside a hidden group; a layer hidden
  by its own flag is still picked, as before groups (the contract's rule is about groups).
  A layer named explicitly is refused when it is locked, itself or by a group
  (`LayerLocked` naming the group), or is not a vector layer (`NotAVectorLayer`). A board
  with no vector layer refuses with `NoVectorLayer`; the model does not make "Vector 1" by
  itself (the tools will, wave 2).
- **Edits**: `add_layer` (vector, unchanged signature), `add_layer_in(name, kind, parent,
  placement)`, `add_group`, `set_layer_blend` (pass-through refused on a layer that is not a
  group), `move_layer` (among siblings; above or below a layer in another group moves it
  there), `move_layer_into` / `set_layer_parent` (refused into a non-group, itself or a
  group it holds), `group_layers(ids, name)` (a picked layer whose group is also picked
  stays in it), `ungroup` (refused on a locked group: ungrouping would unlock its layers),
  `delete_layer` (a group with everything in it; refused when the group is locked, a group
  above it is, or any layer inside it is; a board keeps one layer that is not a group),
  `merge_group` (in place: the group entry becomes the vector layer, keeping its id, name,
  visibility, opacity and blend, pass-through dropped; refused with a paint layer inside),
  `merge_layer_down` (the sibling just below; a group, a group below, and any paint layer
  refused with a sentence until wave 4). Moving layers is open on locked layers, as before.
  **Deviation**: merging a group does not bake the children's own opacity, blend or
  visibility into their elements (hidden children's elements show once merged).
- **Snapshot**: `layers` is every entry in draw order bottom to top (reversed, a group
  comes before what it holds). `Layer` gains `kind`, `blend`, `parent` (effective),
  `depth`, `effective_visible`, `effective_locked`, `locked_by`, `isolated`; `count` stays.
  New reads: `layer_tree()`, `children_of(id)`, `default_layer()`, `layer_lock(id)`;
  `top_layer()` is now the top vector layer.
- **MCP**: `get_canvas` lists the tree (group headings, depth by heading level, kind,
  blend, inherited flags); `add_layer` takes `kind`, `parent`, `blend`; `update_layer`
  takes `blend` and `parent`; `delete_layer` deletes a group with its contents and
  `merge_down` on a group merges it.

## Built in wave 1 (UI)

Built 2026-11-14 in `crates/pimble-app` (both apps, one code path), on the model above.
The canvas fills the pane and everything floats over it; the app's toolbar shows nothing of
the canvas and dims for a canvas pane as for a pane with no document (`pane_view.rs`: the
board toolbar is gone, `usable`/`commentable` are false while the focused pane shows a
board).

- **Files**: `boards/rail.rs` (the tool rail and flyouts, the context bar, the color well,
  the zoom corner, the chrome's keys), `boards/layers.rs` (the layers panel), `boards/ui.rs`
  (the surface and its overlays, the panel stack, Meaning, Frames). State: `StyleUi` gained
  `map` (was `panel`), `faces`, `front`, `recent`; `PaneUi` gained `selected_kinds`, `zoom`,
  `popup` (one `Popup` open at a time; a press on the canvas closes it), `panels_hidden`,
  `folded` (per pane), `folded_groups`, `picked_layers`, `keys`, `drop_at`. `LayerRow`
  carries kind, blend, parent, depth, the inherited flags and a child count.
- **Tool rail**: Select, Hand | Pen, Highlighter (still in Draw: Paint does not exist
  yet), the shapes flyout (Rectangle, Ellipse, Diamond, Line, Arrow), Eraser | Text,
  Sticky note, the diagram flyout (Connector, Numbered step, Frame), Discussion pin |
  Insert pictures. Every `Tool` is on it exactly once (a test). Direct select is left out
  until paths (wave 2). A flyout button shows the tool last used from it (`StyleUi::faces`,
  for the session; a tool picked by its letter becomes its flyout's face too); its corner
  mark opens the flyout, and so does a press held 350 ms. **Deviation**: on the desktop a
  press on a flyout button also uses its face tool at once (rinch delivers the click with
  the press), so holding opens the siblings with the face already picked.
- **Context bar**: the tool's name (or the selection's: its kind, or "N selected"), then
  only what applies (`rail::controls_for` over the selected kinds, else the tool's kind):
  stroke and fill swatches (they open the color well's picker on that square), then one
  picker button each for width, dash, arrow heads, font size, text alignment and opacity:
  its face is the current choice and a press opens the choices in a row under the bar
  (Joe, 2026-11-14: the first cut showed every choice inline and scrolled sideways at
  1200 pixels); a hint for tools with nothing to set; for a selection Duplicate, Align and
  distribute (a menu, two or more), Group (two or more), Ungroup (a group selected), Lock,
  Delete, and a "more" menu with Bring Forward, Send Backward, Bring to Front and Send to
  Back; Undo and Redo always. "Done" leads it while marking up a picture in text. A pen
  stroke selected is about 370 pixels wide; a pane narrower than the bar scrolls it.
- **Color well**: the stroke (a ring) and fill squares, the front one where a picked color
  goes (a click brings a square to the front, a second click opens the picker); swap (X;
  with no fill, the stroke color becomes the fill too and the stroke stays, since a stroke
  cannot be nothing); the picker is rinch's `ColorPicker` with the appearance palette and a
  hex box, "No fill" for the fill, and up to eight recent colors. Picking writes to the
  selection as the old style section did.
- **Panels**: Layers, Meaning, Frames, each folding to its title row; Tab hides the stack.
  The layers panel is the tree: indented by depth, groups fold (this viewer's own), eye
  and lock (dimmed when a group's flag decides), a kind icon, the name (click a picked
  row's name to rename it in place), the element count (a group's: its layers). The picked
  layer's blend (a list of `Blend::ALL` by label; Pass Through only on a group) and opacity
  are under the list, then New Layer, New Group, Group Layers, Ungroup, Merge Down (a
  group: Merge Group), Move Selection to Layer, Delete. Ctrl or Shift clicks pick several
  rows for Group Layers (Ctrl+G while the panel has the focus). Rows drag above, below and
  into groups (`layers::zone_at`, `layers::drop_move`, tested: below an open group that
  holds layers is its top; a group never goes inside itself), through `move_layer_into`.
  Refusals the model makes are said (`layer_edit`), not only logged.
- **Zoom corner**: the map (the navigator, toggled), zoom out, the zoom in percent (a click
  is actual size), zoom in, fit; the grid, snapping and Color by Author (each only here),
  and the export menu.
- **Keys**: tool letters, Space held pans, Shift constrains, X swaps, Tab hides the panels,
  Ctrl+G groups layers after a press in the layers panel. A press on the floating chrome
  takes the keyboard from the canvas's surface (rinch gives it back only on a press on the
  surface), so the chrome listens through the keyboard interceptor (`rail::keys_to`,
  `rail::key_act`, tested): it answers only while the last press on the board was on the
  chrome, its pane is focused and nothing else holds the focus (a field, the editor, the
  tree, the surface itself); everything with Ctrl or Alt is the app's. Alt-picking colors
  and `[`/`]` wait for paint layers.
- **Small panes**: a pane less than about half the window wide is narrow (the context bar
  takes the whole top, the panels start below it); a quarter or less is small (the panels
  fold when it becomes small, the rail tightens, the map shrinks and the well's squares
  hide; the context bar's swatches still open the picker).
- **Theme**: floating chrome is `.pimble-float` (the body color, a hairline border, a 6 px
  radius, a faint shadow), rinch's semantic variables only, both schemes.
- **Not built**: layer thumbnails (the contract's panel line); panels folding to an icon
  (they fold to their title row); the hue ring (rinch's picker has a square and a hue
  bar). Embedded canvases in text are unchanged; marking up a picture in text opens this
  same view.

## Built in wave 2 (model)

Built 2026-11-16 in `crates/pimble-board` and `crates/pimble-mcp`, no UI: the Pen, Direct
select, Pencil and shape tools, the context bar's new controls and the rail entries are the
UI half. Gradients, combining and SVG import are wave 3. `crates/pimble-app` gained only
the match arms the new kinds need to compile (`boards/rail.rs` `controls_of` and
`kind_word`: polygon and star as shapes, a path with stroke, fill, width, dash and opacity;
`boards/tools.rs` `kind_label`).

- **Storage**: only new keys and one new kind's value; every element that exists reads and
  draws as before.
  - Kind `path`: `geom` is one `Any` value, written whole like every geometry: `{ subpaths:
    [{ closed: bool, anchors: [x, y, in_dx, in_dy, out_dx, out_dy, mode, ...] }], rule:
    "nonzero" | "evenodd" }`. Handles are offsets from their anchor. `mode` is stored as a
    number (0 corner, 1 smooth, 2 symmetric); a reader also takes the names. A malformed
    anchor (a value that is not a finite number) is skipped on read, a missing `rule` reads
    as nonzero.
  - Kinds `polygon` and `star`: `geom` is a rectangle (their box), as a rect's. The shape is
    stretched to fill the box exactly (a pentagon's box is the element's box, so selection
    and resizing need nothing new).
  - **Deviation**: the live shapes' parameters are **style keys**, not geometry keys:
    `radius` (rect corner radius, board units, absent 0), `sides` (polygon, absent 6),
    `points` and `inner` (star tips, absent 5; inner radius as a fraction of the outer,
    absent 0.4). Geometry is written whole on every move and resize, so parameters inside it
    would be lost to the app's resize (which writes `Geom::Rect`) and would collide with a
    concurrent resize; as style keys each merges on its own, like width and color.
  - Stroke detail, style keys: `cap` (`butt`/`round`/`square`), `join`
    (`miter`/`round`/`bevel`), `miter` (absent 4), `dash_pattern` (text, lengths in board
    units, `"6 3 1 3"`; when set it wins over the `dash` preset). Absent keys read as
    today's look: round caps (butt for the dashed preset), round joins, so no existing
    element changes. These apply to paths, rect, ellipse, diamond, polygon, star, sticky,
    frame, line, arrow and connector. **Not** to pen and highlighter strokes, which keep
    their pressure outline and round ends; nor to the callout's doubled outline.
  - Fill: paths, polygons and stars fill as rect, ellipse and diamond already did (`fill`);
    a path fills by its `rule`, open subpaths filling as if closed (SVG's rule).
- **An older app** reads `path`, `polygon` and `star` as `Kind::Other`: kept, never drawn,
  never hit, not in its bounds (a path's geometry has no `x`/`y`, so it reads as no
  geometry; a polygon or star reads its box). It ignores the new style keys, so a rounded
  rect is square there and a custom dash is solid.
- **Rendering, bounds, hit, export**: tiny-skia draws paths and the shapes through the same
  op list as every element (`paint.rs`), so SVG export writes them as `<path d=...>` with
  `fill-rule`, `stroke-linecap`, `stroke-linejoin`, `stroke-miterlimit` (when not 4) and
  `stroke-dasharray` (an odd custom list repeats, as SVG reads it). A path's extent is its
  curves' exact extent (cubic extrema); the painted extent grows by
  `ResolvedStyle::stroke_reach` (half the width, times the miter limit for a mitered join
  and √2 for square caps), which every kind now uses. Hit testing: on the stroke within the
  tolerance, inside the fill (by the rule) when filled; a rounded rect, polygon and star hit
  on their real outline. The spatial index and the raster cache take paths through bounds
  and fingerprints (the anchors, the rule and every new style key are fingerprinted).
- **Moving and resizing** work as for every element: moving writes the transform; the app's
  resize scales the transform of kinds its geometric resize does not name, which includes
  `Geom::Path`, and resizes polygon and star geometrically (they are `Geom::Rect`). For the
  UI to resize a path geometrically (outline width kept), `Geom::mapped(f)` maps any
  geometry through an affine map (a path's handles as `f(p + h) - f(p)`), and
  `PathGeom::rescaled(from, to)` maps one box onto another.
- **The `path` module** (`pimble_board::path`, types re-exported at the crate root), pure:
  - Types: `PathGeom { subpaths, rule }`, `Subpath { closed, anchors }`, `Anchor { p, h_in,
    h_out, mode }` (`Anchor::corner(p)`, `in_point()`, `out_point()`), `AnchorMode
    { Corner, Smooth, Symmetric }`, `PathRule { NonZero, EvenOdd }`, `AnchorRef { subpath,
    index }`, `HandleSide { In, Out }`, `HandleRef { anchor, side }`, `SegmentHit { subpath,
    segment, t, point, distance }`, `PathError { Syntax { at, message }, Empty }`.
  - Building: `PathGeom::from_anchors(anchors, closed)`, `PathGeom::polyline(points,
    closed)`, `PathGeom::new(subpaths, rule)`; `NewElement::path(geom)`.
  - Reading: `bounds()`, `segments()` (subpath, segment, cubic), `Subpath::segment(i)`,
    `Subpath::is_line(i)`, `flatten(tolerance)`, `to_tiny()`, `anchor(at)`,
    `anchor_mut(at)`, `anchor_refs()`, `anchor_count()`, `is_empty()`, `contains(p)` (fill,
    by rule), `distance_to(p)` (outline); free `cubic_at(c, t)`, `split_cubic(c, t)`,
    `infer_mode(h_in, h_out)`.
  - Hitting (board units, in the element's own space: the UI maps the pointer through the
    inverse of `Element::world` first): `hit_anchor(p, tol) -> Option<AnchorRef>`,
    `hit_handle(p, tol, only: Option<&[AnchorRef]>) -> Option<HandleRef>` (zero handles
    are not there; `only` limits it to the selected anchors' handles), `hit_segment(p,
    tol) -> Option<SegmentHit>`.
  - Editing (in memory; the caller writes the result with `set_geom`):
    `insert_anchor(subpath, segment, t) -> Option<AnchorRef>` (de Casteljau, shape
    unchanged; a straight segment gets a corner), `delete_anchors(&[AnchorRef]) -> bool`
    (true when the path is emptied: delete the element; a subpath left with fewer than two
    anchors goes), `toggle_smooth(at) -> Option<AnchorMode>` (a corner becomes smooth with
    collinear handles, made a third of the way toward each neighbor along the line between
    the neighbors when both were zero; smooth or symmetric becomes a corner with its handles
    pulled in), `set_mode(at, mode)`, `move_anchor(at, to)`, `move_anchors(refs, dx, dy)`
    (handles follow), `move_handle(handle, to, break_link)` (corner: alone; smooth: the
    other turns to stay opposite keeping its length; symmetric: mirrored; `break_link`, the
    Alt-drag, makes the anchor a corner first), `close(subpath)`, `map(f)`, `translated(dx,
    dy)`, `rescaled(from, to)`.
  - Live shapes: `rect(r, radius)` (eight anchors when rounded), `ellipse(r)`,
    `diamond(r)`, `polygon(r, sides)`, `star(r, points, inner)`, `shape(kind, r,
    resolved_style)`, and `to_path(kind, geom, style, points) -> Option<PathGeom>` for
    rect, ellipse, diamond, polygon, star, a line with no arrow heads, a pen stroke (fitted
    within `STROKE_TO_PATH_TOLERANCE`, 0.5; closed when it has a fill) and a path; `None`
    for the rest (arrows, connectors, text, sticky notes, pictures...).
  - Fitting: `fit(points: &[Point], tolerance) -> PathGeom` and `fit_stroke(&[[f32; 3]],
    tolerance)` (Schneider's algorithm in f64; every point within `tolerance` of the curve;
    turns sharper than about 60 degrees become corners, found over neighbors about twice the
    tolerance away so jitter does not make corners; joins inside a run are smooth; straight
    pieces get zero handles; one open subpath; one point gives a dot of two anchors).
    Tested on a circle (within tolerance both ways, at most 16 anchors at 0.5), a zigzag
    (corner anchors at its corners) and a straight line (two anchors, a plain line).
  - SVG path data: `to_d(&PathGeom) -> String` (absolute `M L C Z`, three decimals) and
    `parse_d(&str) -> Result<PathGeom, PathError>` (`M L H V C S Q T A Z`, absolute and
    relative, implicit repeats, numbers run together as SVG allows, arc flags without
    separators; quadratics exactly and arcs as at most quarter-turn cubics; a closing point
    equal to the start merges into the first anchor; modes inferred from the handles;
    single-point subpaths dropped; nothing drawn is `Empty`). Round-trip tested.
- **Edits** (`BoardTxn`, each judged by locks as every edit is):
  - `convert_to_path(id) -> Result<PathGeom>`: turns a shape, a line without heads or a pen
    stroke into a `path` **in the same transaction**, keeping its id, layer, order, parent,
    style, text, meaning and lock, and adjusting its transform so it stays exactly where it
    was (a path pivots on its own extent; a pen's pivot was its points' box). Direct select
    on a live shape calls it first, edits the answered path, then `set_geom`, all in one
    `Board::edit` (one undo, tested). A path answers itself; other kinds are refused with a
    sentence. A rect's text stays stored but a path does not draw text.
  - `stroke_to_path(id, points, tolerance) -> Result<PathGeom>`: the Pencil's release. The
    Pencil streams as today's pen does (a `pen` element and `append_points`, so others watch
    it drawn); on release this replaces that pen's kind and point array with the fitted
    path in one write, keeping id, layer, order and style. `add_fitted_path(points,
    tolerance, element) -> Result<String>`: a Pencil stroke released before anything was
    streamed. The tolerance is the caller's (the contract's setting).
  - `set_geom` takes `Geom::Path`; `add_element` and `set_geom` refuse a path's geometry on
    another kind, another geometry on a path, and anything but a box on polygon and star.
  - Style: `set_style` with the new `Style` keys (`cap`, `join`, `miter`, `dash_pattern`,
    `radius`, `sides`, `points`, `inner`) and `clear_style` with the new `StyleKey`s
    (`Cap`, `Join`, `Miter`, `DashPattern`, `Radius`, `Sides`, `Points`, `Inner`). A UI
    picking a dash preset while a custom pattern is set clears `DashPattern` (the MCP does).
    `ResolvedStyle` carries all of them resolved (`cap`, `join`, `miter`, `dash_pattern`,
    `radius`, `sides`, `points`, `inner`) plus `is_dashed()` and `stroke_reach()`; constants
    `DEFAULT_MITER`, `DEFAULT_SIDES`, `DEFAULT_STAR_POINTS`, `DEFAULT_STAR_INNER`; helpers
    `parse_dash_pattern`, `dash_pattern_text`; `Cap::ALL`, `Join::ALL` for menus;
    `Kind::is_shape()`.
- **MCP**: see docs/MCP_CONTRACT.md "Paths, live shapes and stroke detail": `add_elements`
  takes `path` with `d` and `rule`, `polygon`, `star`, and the new style keys (`dash` takes
  a preset or a pattern); `get_canvas` writes paths as `d`; `update_elements` takes `d` and
  `rule` (and `d` turns a shape, line or pen stroke into a path); `render_canvas` draws them.
- **Not built**: the UI (Pen, Direct select with marquee of anchors, Pencil and its
  tolerance setting, the shapes flyout's polygon and star, the context bar's cap, join,
  dash pattern, radius, sides, points and inner controls); a path does not draw text;
  converting an arrow, connector or a pen's pressure widths to a path; stroke detail on pen
  and highlighter strokes; gradients, combining, flip and SVG in (wave 3).

## Built in wave 2 (UI)

Built 2026-11-16 in `crates/pimble-app` (both apps, one code path), on the model above, with
two small additions to `crates/pimble-board`: `BoardView::set_overlay` (an `EditOverlay` of
outlines, handle arms, anchors and handle tips, drawn over each frame and never into the
cache, at a constant size on screen) and `repivot` made public.

- **Files**: `boards/points.rs` (new: the Pen and Direct select, their pure state machines,
  the overlay), `boards/tools.rs` (the Pencil's release, the tools' routing, the layer rule,
  styles, the fill rule), `boards/rail.rs` (the rail, the flyout, the context bar's new
  pickers), `boards.rs` (`Tool`, `StyleUi`, `Popup`, the gestures and the session's `pen`,
  `points`, `alt` and held undo clock), `boards/ui.rs` (an effect per pane: another tool
  finishes the Pen's path and lets go of Direct select's points, run on the next turn),
  `styles.rs`. **Fixed on the way**: `after_change` set the pins signal while it held the
  session, so an effect run by that set and reading the session panicked ("RefCell already
  borrowed", found switching tools in the test app); the pins are set after it lets go.
  A context-bar picker whose face is words or a number now has a width class
  (`picker_button`'s `size`): the button did not grow with its face, and "100%" ran
  into the next one.
- **Tools and letters**: the rail is Select, Direct select (A), Hand | Pen (P), Pencil (N),
  Highlighter, the shapes flyout (Rectangle, Ellipse, Diamond, Polygon, Star, Line,
  Arrow), Eraser | Text, Sticky note, the diagram flyout, Discussion pin | pictures. Every
  `Tool` is on it exactly once and has its own letter (tests). **Deviation**: P, A and N
  were the old pen, the arrow and the numbered step, which moved: Arrow is W, Numbered
  step I; Polygon is G and Star K (the shapes flyout's own letter, U, is not bound: every
  tool has one).
- **Pen**: a click places a corner, a press-drag a symmetric anchor whose handles follow
  the drag (Alt held moves the outgoing handle alone and makes a corner), a press on the
  last anchor gives it a new outgoing handle alone (a cusp), a click on the first anchor
  closes the path and finishes it, Enter or Escape finishes an open one (a path of one
  anchor was never written), so does picking another tool, and so does undo (which then
  takes it back whole). The element is written when the second anchor goes down and
  rewritten with each anchor and about every 80 ms while a handle is dragged, so others
  watch it drawn. **One undo step** for the whole path: the session holds the undo
  manager's clock while it is drawn (`undo_options`: every edit then reads the same time
  and joins one step), and a new step starts when it is finished. The overlay shows the
  path, its anchors (the last filled), the handles being dragged and a dashed segment to
  the pointer; the first anchor is drawn larger when a click would close the path.
- **Direct select**: a click on a path or a live shape (rectangle, ellipse, diamond,
  polygon, star, line without heads, pen stroke) shows its anchors (inside groups too: it
  picks the element, not its group), and a press on an anchor takes it at once; Shift
  adds or takes back anchors; a press on a segment takes its two anchors; a drag moves the
  selected anchors (several together when the pressed one was selected); the selected
  anchors' handles show and drag (Alt breaks the link); a marquee selects anchors of the
  element shown, or, when none is shown, shows the topmost element with anchors inside
  it; a click on empty board lets go; a double-click on an anchor switches corner and
  smooth, on a segment adds an anchor; Delete or Backspace removes the selected anchors
  (an emptied path deletes the element). Everything is worked in element space (the
  pointer through the inverse of `Element::world`, the reach divided by its scale). The
  first write of a live shape calls `convert_to_path` in the same edit; every write of new
  geometry also writes the transform `repivot` gives, so a turned or scaled path keeps the
  rest of itself where it was as its extent changes. A drag is one undo step (the held
  clock again). The selection box and outline are not drawn over the element whose points
  show; the context bar shows its style.
- **Pencil**: streams as the pen did (a `pen` element and `append_points`), and on release
  `stroke_to_path` with the "Smoothing" setting (Low, Medium, High: 0.75, 2 and 5 logical
  pixels at the zoom it is drawn at, so a stroke drawn zoomed in is fitted as finely);
  released before anything streamed, `add_fitted_path`; should fitting be refused it stays
  a pen stroke. The setting is in `StyleUi` with the other tool options (for the session,
  as they are). Markup on pictures in text uses the same tools, so the Pencil marks
  pictures there too. The highlighter is unchanged.
- **Shapes**: Polygon and Star in the flyout, dragged out (Shift: square) or clicked like
  the other shapes. The context bar's shape picker: a rectangle's corner radius (a
  slider, 0 to 100), a polygon's sides (3 to 12), a star's points (4 to 12) and inner
  radius (a slider); new shapes take them, and changing them writes the selected shapes
  that have them (each key only on its own kind) live.
- **Stroke detail**: one picker for ends (butt, round, square), corners (miter, round,
  bevel) and, for a selected path, the fill rule (nonzero, even-odd, written as the path's
  geometry). **Deviation**: one picker for the three instead of one each, so the bar still
  fits at 1200 pixels. Ends show for paths, shapes, lines, arrows and connectors, corners
  for paths and shapes; never for pen and highlighter strokes. New elements take the
  chosen ends and corners; unchosen they keep the kind's own (absent keys). Picking a dash
  preset clears a custom `dash_pattern` (tested). "No fill" is now judged as an empty
  style (the old check listed keys and would have cleared the fill on every new key).
- **Tools pick their layer** (`tools::target_layer`, every tool): the picked layer when it
  is a vector layer, else the nearest vector layer above it that is shown and not locked,
  else the topmost that takes elements; with no vector layer at all a new "Vector 1" (or
  the next free number) is made above the picked layer and picked (tested).
- **Resizing paths**: a path that is not turned or scaled is resized geometrically
  (`PathGeom::map` through the box's map, as rectangles are), so its outline keeps its
  width; a turned or scaled one scales its transform, as every element does.
- **Keys**: P, A, N through `Tool::from_key` (the surface) and `rail::key_act` (the
  chrome), tested; Enter is the board's while a Pen path is drawn, Delete and Backspace
  while anchors are selected, Alt while the Pen or Direct select works (its state is
  tracked from the surface's keys; pointer events carry no modifiers).
- **Tests**: the Pen's state machine, Direct select's press priorities (a selected
  anchor's handle, an anchor, a segment), shift selection, anchors through a turned
  transform (`points.rs`); a closed curve drawn by pointer events in one undo step, Escape
  finishing an open path, the Pencil streaming a pen stroke and leaving a path (and one
  released at once), Direct select turning a rectangle into a path with one undo for both,
  deleting an anchor, live star and rectangle parameters, the dash pattern cleared, the
  Pen making "Vector 1" (`boards.rs`); letters, controls and the rail (`rail.rs`).
- **Not built**: anchors of several elements at once (Direct select shows one element's
  points; a marquee picks one); snapping or Shift constraints while dragging points;
  a miter limit or custom dash pattern control; converting arrows and connectors;
  pressure widths survive the Pencil only while it streams (the path has one width).

## Built in wave 3 (model)

Built 2026-11-17 in `crates/pimble-board` and `crates/pimble-mcp`, no UI: the context bar's
combine and flip buttons, the color well's gradient editor and on-canvas gradient handles,
and paste or drop of SVG are the UI half. `crates/pimble-app` is not touched.

### Spike: combining

Tried in a scratch crate on the contract's cases, release build, measuring areas against
the exact ones: two overlapping circles (all four operations), two rectangles sharing an
edge, the same rectangle twice, a figure eight crossing itself, a square with an even-odd
hole, a five-point star minus a circle, and two closed curves of 200 curved anchors each.

- **linesweeper** 0.5 (MIT or Apache-2.0, pure Rust over `kurbo`): every case right
  (a shared edge disappears into one outline, identical shapes subtract to nothing, the
  figure eight and the hole fill by their rules, the star keeps its cut-out middle); output
  is cubic curves, not a polygon. 200 anchors against 200: 2.4 to 2.6 ms per operation.
  Each operand can have its own fill rule (`Topology::contours` takes the inside test).
- **flo_curves** 0.8 (Apache-2.0): the circles and rectangles right and about 0.3 ms at 200
  anchors, but the figure eight came out at 100 square units instead of 5,100, the
  even-odd hole was filled (12,600 instead of 7,600), the star minus the circle kept the
  circle (14,583 instead of 8,928), and the 200-anchor subtract lost 4% of its area.
- **i_overlay** (MIT or Apache-2.0) after flattening (0.05 units): right areas and the
  fastest (0.23 ms), but a polygon of 400 to 800 points, to be refitted
  (`path::fit`) into curves that would no longer meet exactly where the inputs did.

Chosen: **linesweeper**, the one that was right on every case and answers curves; a few
milliseconds is nothing for a click. It and `kurbo` build for wasm32. The spike's cases
are `combine.rs`'s tests.

### Stored keys

Only new keys; everything that exists reads and draws as before.

- Style keys `fill_gradient` and `stroke_gradient`, each one `Any` value written whole (last
  writer wins, like geometry): `{ type: "linear" | "radial", x1, y1, x2, y2, stops: [{ offset,
  color: "#rrggbb(aa)" }] }`. A missing or unknown `type`, or no stops, reads as no gradient.
  **The solid `fill` / `stroke` key stays beside it as its fallback** (the first stop's color,
  written with the gradient): an older app reads its solid key as it always did and draws
  that, and ignores the gradient key. `ResolvedStyle.fill` is that fallback (so hit testing
  and "has a fill" are unchanged), `ResolvedStyle.fill_gradient` / `stroke_gradient` the
  gradients (stops clamped and sorted).
- **Handles are in the element's box units** ((0, 0) the top-left of its own extent before
  its transform, `Element::pivot_bounds`; (1, 1) its bottom-right), so a gradient moves,
  turns and scales with its transform and stretches when the geometry is resized. The
  paint itself is laid out in element space from the mapped handles: linear from `from` to
  `to`, colors padding beyond; radial a **circle** centered on `from` through `to` (in
  element space, so a circle stays one in a long box; turning or scaling the element
  carries it). A box with no height or width (a horizontal line) is given the other side's
  length (`Gradient::box_for`), so a line's stroke can run a gradient along it.
- Combining, flipping and SVG in write only what exists: kind, geometry, transform,
  parent, and new elements.

### Public API

- **Gradients** (`model.rs`, at the crate root): `Gradient { kind: GradientKind, from: Point,
  to: Point, stops: Vec<GradientStop> }`, `GradientKind { Linear, Radial }` (`as_str`,
  `parse`), `GradientStop { offset, color }` (`new`); `Gradient::linear(from, to, stops)`,
  `radial(center, edge, stops)`, `default_linear(color)` / `default_radial(color)` (the
  color to the same color transparent: left to right across the box; center to the right
  edge), `is_valid()`, `radius()`, `normalized()`, `fallback() -> Color`, `color_at(t)`,
  `mirrored(horizontal)`, `box_for(Rect)`. `Style` and `ResolvedStyle` gain
  `fill_gradient` and `stroke_gradient`; `StyleKey` gains `FillGradient` and
  `StrokeGradient`. `Paint { None, Solid(Color), Gradient(Gradient) }`.
- **Setting paints** (`BoardTxn`): `set_fill_paint(id, &Paint) -> Result<()>` (None clears
  the fill and its gradient; Solid writes the color and removes a gradient; Gradient writes
  it with its fallback), `set_stroke_paint(id, &Paint) -> Result<()>` (`Paint::None` refused:
  "A stroke cannot be none; set its width to 0 instead."). **The color well must use
  these**: `set_style` only sets keys, so a solid color written with it under a gradient
  leaves the gradient winning. `set_style` with a gradient writes the fallback too when the
  `Style` names no solid color. `clear_style(Fill)` / `clear_style(Stroke)` now clear the
  key's gradient as well (the app's "No fill" clears `Fill`).
- **Combining** (`pimble_board::combine`, `CombineOp` at the root): `CombineOp { Union,
  Subtract, Intersect, Exclude }` (`ALL`, `as_str`, `parse`, `label`); pure
  `combine(&[PathGeom], CombineOp) -> Option<PathGeom>` (operands in one space, bottom to
  top, each by its own rule; nonzero result; `None` when nothing is left) and
  `area(&PathGeom) -> f32`. The edit: `BoardTxn::combine(ids: &[String], op: CombineOp) ->
  Result<String>` answers the result's id.
- **Flip**: `BoardTxn::flip(ids: &[String], axis: FlipAxis) -> Result<()>`, `FlipAxis {
  Horizontal, Vertical }` at the root.
- **SVG in** (`pimble_board::svg_in`, types at the root): `parse(svg: &str, place:
  &SvgPlacement) -> Result<SvgImport, SvgError>`; `SvgPlacement { at: Point, center: bool,
  fit: Option<f32> }` (`Default`: top-left at 0, 0, one SVG pixel per board unit; `fit`
  scales the longer side to that many units); `SvgImport { items: Vec<SvgItem>, warnings:
  Vec<String>, bounds: Option<Rect> }`; `SvgItem { element: NewElement, group: Option<usize>
  }` (`group` indexes an earlier item, a `Kind::Group`); `SvgError { Parse(String), Empty }`.
  The edit: `BoardTxn::import_svg(&SvgImport, layer: Option<&str>) -> Result<Vec<String>>`
  (ids in `items` order). `text::first_baseline(font_size)` is public (where a text box's
  first baseline sits).
- Also new: `Kind::with_article()` ("a sticky note"), used by refusals.

### What each does

- **Combine**: every element is read through `path::to_path` (shapes with their radius,
  sides and points, lines without heads, pen strokes, paths) and mapped through its
  `world` (its transform and its group's) into board space; operands go bottom to top by
  z-order; open subpaths fill as if closed. Union, intersect and exclude fold left to
  right (exclude keeps what an odd number cover). **Deviation**: subtract is the
  **bottom-most operand minus everything above it, and the result keeps the bottom one's
  place and style** (Illustrator's "Minus Front"), where the contract says the top
  operand's style for every operation; the other three keep the top operand's. The kept
  element keeps its id, layer, order, group, style (gradients included), text and meaning;
  it becomes `kind: path`, its geometry written in its group's space and its transform
  removed; the other operands are deleted; one transaction, so one undo (tested). Locks
  are judged on every operand first. Refusals write nothing: fewer than two elements
  ("Combining takes two or more elements."), kinds with no outline ("Only shapes, lines
  without arrow heads, pen strokes and paths combine: a text and a sticky note do not."),
  nothing left ("The shapes do not overlap, so intersecting them leaves nothing.";
  "Nothing is left: the shapes in front cover the one at the back.").
- **Flip** about the center of the given elements' combined bounds. Paths, shapes without
  text, lines, arrows, pen and highlighter strokes and pictures are
  **mirrored through the transform** (one axis of its scale negated, its rotation
  reflected, its translation mirrored, all worked in its group's space), so geometry,
  gradient handles and anchors are untouched and flipping twice gives back the transform
  exactly (tested through a turned, scaled path). **Decided**: text, sticky notes, cards,
  steps, callouts, frames, spotlights and shapes that hold text are **moved only**: what
  they paint lands at its mirrored place, with their rotation kept, so words never read
  backwards. A group flips its members (the group entry is not written; a turned or scaled
  group is repivoted so it stays put); members of a flipped frame ride with the frame.
  Connectors mirror their free ends. A locked element refuses the whole flip.
- **Gradients draw** with tiny-skia's linear and radial shaders in element space (the
  renderer's transform carries them), on the fills of rect, ellipse, diamond, polygon,
  star, path, card and sticky note, and the strokes of those and of lines and arrows (head
  included). Connectors keep a solid stroke. **SVG out** writes each as `<defs><linearGradient
  | radialGradient gradientUnits="userSpaceOnUse">` just before the path that uses it
  (`pimble-gradient-N` ids, stop opacity times the element's), in the element's own user
  space. Gradients are fingerprinted, so the raster cache redraws on a change.
- **SVG in** parses with `usvg` 0.49 (no default features: no text shaping, no system
  fonts; MIT or Apache-2.0): CSS in `<style>` and classes, `<use>`, units, `viewBox` and
  `preserveAspectRatio` all resolved, every shape a path. Each path becomes a `path`
  element in board units (its transforms applied to its geometry, no transform of its
  own), with fill (color and opacity, fill rule, linear and radial gradients through
  `<defs>` references, in its box units), stroke (color or gradient, width scaled by the
  transform, caps, joins, a miter limit that is not 4, dashes) and the opacity of the groups
  above it folded into its own. No fill reads as none, no stroke as width 0. A `<g>`
  holding two or more things becomes a board `group` of them; a group holding the whole
  drawing (usvg's `viewBox` group, a file's wrapping `<g>`) does not. `<text>` is read off
  the markup with `svgtypes` (usvg drops text without its text feature): one text element
  per `<text>`, tspans that set `y`/`dy` starting new lines, position from its baseline
  (`text::first_baseline`), size (px, pt, em, % resolved up the tree), fill color,
  `text-anchor`, the transforms above it (turned about its center so its baseline start
  stays put). **Deviation**: text goes on top of the drawing, not between the shapes it sat
  between. Placement maps the drawing's extent to `SvgPlacement` (box units make gradients
  scale for free; widths and dashes scale). Warnings, once each: filters, masks, clip paths
  (shapes come in unclipped), pictures, patterns (filled grey), blend modes, repeating
  gradients (they pad), text on a path, text styled only by a class. `import_svg` puts the
  items in, in order, into the named layer or where `add_element` puts new elements, and
  sets the groups, in one transaction (one undo, tested); a board with no vector layer
  refuses with `NoVectorLayer`, as for any new element, so the UI makes "Vector 1" first
  (`tools::target_layer`, as wave 2's tools do).
- **Size**: `usvg`, `svgtypes`, `roxmltree`, `simplecss`, `kurbo`, `linesweeper` and
  `tiny-skia-path` 0.12 (usvg's; the renderer stays on tiny-skia 0.11) are new to the web
  build.

### MCP

docs/MCP_CONTRACT.md "Combining, gradients, flip, SVG in": `combine_elements`,
`flip_elements`, `import_svg`; `fill_gradient` and `stroke_gradient` in every style
(`add_elements`, `update_elements`, where a solid color replaces a gradient), in
`clear_style`, and in `get_canvas`'s style line; `render_canvas` draws them.

### Tests

`combine.rs` (the spike's cases: circles all four ways, a shared edge, the same shape twice,
a figure eight, a hole, a star minus a circle, three operands, 200 anchors), `svg_in.rs`
(shapes, groups with transforms and `viewBox`, gradients by reference and a CSS class, text
and warnings, placement, refusals), `tests/wave3.rs` (combine through transforms with one
undo, subtract keeping the back, refusals that write nothing, a combined group member,
pens and holes; gradients stored with their fallback, replaced and cleared, drawn across an
element and moved and stretched with it, radial and stroke gradients drawn and exported;
flips of a turned path and back, text moved unmirrored, a group, a lock; SVG in as one undo
with its groups, and `NoVectorLayer`), and the MCP's gradient round trip.

### Not built

The UI. A radial gradient's focal point and a second radius (imported focal gradients lose
their focus), repeating and reflected gradients, gradients on pen and highlighter strokes,
connectors and text; patterns. Combining keeps nothing of a pen stroke's pressure widths
(it is its fitted outline's fill) and does not outline strokes (a line's stroke is not
area: it combines as a zero-area shape). SVG in: pictures, clip paths and masks as paths,
markers (arrow heads on SVG lines), text between shapes, multi-run text styling, fonts.
Flip has no "flip about a point" or per-element pivot.

## Built in wave 3 (UI)

Built 2026-11-17 in `crates/pimble-app` (both apps, one code path), on the model above;
`crates/pimble-board` is not touched (the gradient handles are drawn with wave 2's
`EditOverlay`).

- **Files**: `boards/gradient.rs` (new: the well's gradients, the stop bar's pure math,
  the CSS swatches show a gradient with, the handles on the canvas and their drag),
  `boards/svg_import.rs` (new: SVG in from a paste, a drop and "Import SVG..."),
  `boards/tools.rs` (`apply_paint`, `takes_gradient`, `combine`, `flip`, the keys, the
  well's gradients in `new_style` and `load_selection_style`), `boards/rail.rs` (the
  Combine picker, the flips in the "more" menu, the well's switch, stop bar and second
  picker, gradient swatches), `boards.rs` (`StyleUi.fill_gradient`, `stroke_gradient`,
  `stop`; `Popup::Combine`; `Gesture::GradientHandle`; `local_edit_said`, which says every
  refusal), `boards/points.rs` (the overlay shows the handles), `boards/ui.rs` (an effect
  per pane redraws the overlay when the picker opens or the gradient changes), `styles.rs`.
- **Combine**: with two or more selected, the context bar shows a Combine picker (Tabler's
  `layers-union`, `layers-subtract`, `layers-intersect`, `layers-difference`): Union,
  Subtract ("the back one minus the rest", the model's rule), Intersect, Exclude. The
  result is selected; one undo brings the operands back. Refusals (a text, a sticky note,
  nothing left, a lock) are the board's sentences in the notice line
  (`local_edit_said`), never only logged.
- **Flip**: "Flip Horizontal (Shift+H)" and "Flip Vertical (Shift+V)" in the selection's
  "more" menu, after the arrange items. **Keys**: Shift+H and Shift+V flip the selection
  on the canvas and from the chrome (`rail::key_act`'s `KeyAct::Flip`); with nothing
  selected they are still the Hand and Select letters. They were taken only with Shift,
  which no tool letter uses (letters ignore case), so plain H and V are unchanged.
- **The color well's gradients**: the picker popup has a Solid / Linear / Radial switch for
  the front square. A new gradient is `Gradient::default_linear` / `default_radial` of
  the square's color; switching kind keeps the stops (and the picked stop) and takes the
  kind's default handles. With a gradient the popup shows the stop bar (the gradient left
  to right, a mark per stop, the picked one outlined): a press picks the stop within 4%
  of it and drags it along the bar, a press elsewhere adds a stop there in the color the
  gradient has there and drags it; a stop dragged past others is kept in order; the x
  removes the picked stop (at least two stay, else a sentence). The picker below edits
  the picked stop's color **with alpha** (a second `ColorPicker` with `alpha`, `hexa`);
  the solid picker shows only for Solid. A stop drag writes about every 80 ms and on
  release, one undo step (the held undo clock). Recent colors pick into the stop too.
- **Writing paints**: everything the well writes goes through `tools::apply_paint`, which
  calls `set_fill_paint` / `set_stroke_paint` for every selected element that has that
  paint (a sticky note's color is its fill), never `set_style` of the solid key alone: a
  solid color picked over a gradient takes the gradient away (tested), "No fill" is
  `Paint::None`, and X swaps the gradients with their colors. Where the model draws no
  gradient (pen and highlighter strokes, connectors, text) a gradient is written as its
  first color. **With nothing selected**, the well's gradients are the tool's style: new
  rectangles, ellipses, diamonds, polygons, stars and paths take the fill gradient (when
  there is a fill) and the stroke gradient, lines and arrows the stroke gradient; selecting
  an element loads its gradients into the well, as its colors are.
- **Swatches**: the well's squares and the context bar's two swatches show a gradient as a
  CSS `linear-gradient` (its handles' direction) or `radial-gradient(circle, ...)`; a
  gradient stroke paints its swatch whole with a hole in the panel's color over the middle
  (rinch draws no `border-image`).
- **Handles on the canvas**: while the well's picker is open on a square whose gradient the
  one selected element has, the overlay shows an arm from `from` (drawn as an anchor) to
  `to` (a handle tip), mapped from box units through `Gradient::box_for(pivot_bounds)` and
  the element's world transform. A press near either (9 logical pixels, the end first)
  with any tool but the Pen, Direct select, the Hand and the eraser drags it and keeps the
  picker open (a press elsewhere closes it, as every popup); the start drags the whole
  gradient along, the end alone moves the end. The overlay follows at once, the document
  about every 80 ms and on release, one undo step; Escape or a cancelled pointer puts the
  gradient back.
- **SVG in**: SVG markup pasted while a canvas has the keyboard (desktop: Ctrl+V reads
  the clipboard's text first, then its picture; browser: the page's `paste` reads
  `text/plain` first, then files), an `.svg` file dropped on a canvas (desktop: by
  extension or content, beside pictures in the same drop; browser: by name or
  `image/svg+xml`), and "Import SVG..." at the end of the zoom corner's export menu
  (desktop: the file dialog, `PIMBLE_DEBUG_SVG` names a file instead for driving the app;
  browser: a hidden file input) all go through `svg_in::parse` and `import_svg`: centered
  at the drop point or the view's middle, scaled down (`fit`) when larger than 90% of the
  view, into `tools::target_layer` (a new "Vector 1" when the model would say
  `NoVectorLayer`), one undo step, the drawing's top level selected and the Select tool
  picked. What the model warns about is one notice: "Imported 4 shapes; skipped: masks."
  (things not imported under "skipped", things changed under "simplified"). Paste
  detection: text that starts with `<svg`, or with `<?xml`, a doctype or a comment and
  holds `<svg`.
- **Tests**: stop bar math (offsets, picking, adding in order with the color there, moving
  past others, removing down to two), the switch, CSS, the handle mapping
  (`gradient.rs`); paste and file detection and the notice (`svg_import.rs`); Shift+H/V
  from the chrome (`rail.rs`); combining with the result selected, one undo and a refusal
  said; the well writing gradients, stops and the radial switch, new shapes taking the
  gradient, a solid color replacing it; a handle dragged by pointer events in one undo
  step; flips there and back; SVG pasted onto a board with no vector layer, centered,
  grouped, selected, its notice, one undo (`boards.rs`).
- **Checked in the test app** (desktop, scratch store): two overlapping circles combined
  each of the four ways and undone, the crescent flipped from the menu and a star with
  Shift+V, a rectangle given a linear then radial gradient of three stops with each stop's
  color picked and a handle dragged (both kinds), the test SVG (a gradient rectangle, a
  turned group of a path and a circle, a masked line) imported from the menu and dropped
  at a point, with its notice.
- **Not built**: deleting a stop with the Delete key (the popup's keys are its fields';
  the x does it); handles for a gradient on several selected elements at once; the focal
  point of a radial gradient; gradients on sticky notes from the well (they take the
  first color); a visual check of SVG pasted on the desktop (it would overwrite the
  clipboard; the import path is the one the drop and the menu use, and it is tested) and
  of the browser build (it compiles from the same code).

## Built in wave 4 (model)

Built 2026-11-17 in `crates/pimble-board` and `crates/pimble-mcp`, no UI: the Brush,
Eraser and Eyedropper tools, the context bar's preset, size, opacity, flow and hardness, `[`
and `]`, Alt picking colors, layer thumbnails and the browser's IndexedDB tile store are the
UI half. `crates/pimble-app` is not touched (it compiles as it was: `Changes` gained a field,
read through `Default` there).

### Spike: brush engine

`examples/paint_bench.rs` (native: `cargo run -p pimble-board --release --example
paint_bench`; the browser's estimate: the same code built for `wasm32-wasip1` and run under
`wasmtime`, which runs scalar Rust about as a browser does, so those are measurements of
wasm, not of a browser). **Measured under heavy load**: the machine was shared with other
builds (`uptime` load average 25.0 / 26.1 / 28.4 on 24 cores at 09:57 while measuring, 63.4 /
48.4 / 40.6 at 10:15 just after), so every number here is pessimistic, the spread between
runs is mostly the load, and single slow appends are noise. **Re-measure on an idle machine**
before tuning anything on these numbers.

- **Per dab**, stamped into one 256-pixel tile's f32 mask (falloff, flow) and laid down:
  0.3 to 0.5 µs for a 4-pixel pencil dab, 1.0 to 1.4 µs at 16 px, 6 to 7 µs at 48 px, 60 µs at
  120 px, 210 µs at 240 px (an airbrush that wide). wasm: 0.5, 2.4, 18, 38 and 211 µs. Cost
  follows the dab's area, as expected.
- **A long pressure stroke across a 4K view** (a `BoardView` of 3840 × 2160 pixels at 2 px
  per board unit, a wandering stroke of 4,000 points over 1,900 × 1,060 units, appended four
  points at a time as pointer events arrive, each append taken through `Board::append_stroke`,
  `BoardView::update` and `frame()`, so the per-append time includes the snapshot update,
  the tiles, the view's dirty re-render and copying a 33 MB 4K frame): round 24 units, 11,208
  dabs: median 5.2 to 5.7 ms per append, p95 about 10 ms; airbrush 80 units: median 6.8 ms,
  p95 11 ms; charcoal 16 units (grain and jitter): median 5.8 to 6.4 ms, p95 11 to 13 ms.
  wasm: 6.1, 19.6 and 9.1 ms medians, p95 13.5, 42 and 17 ms. Single appends of 20 to 95 ms
  showed up in both, not where tiles were first touched, and moved between runs: the busy
  machine. The cache stamped only new dabs (1,235 to 4,312 tile updates, one replay per tile
  the stroke first reaches, never a replay of a tile it already painted).
- **Chosen**: dabs go into a **per-stroke f32 coverage mask** and the stroke is laid onto the
  layer's **premultiplied RGBA8** tile once per change: the mask keeps a low-flow soft brush
  (the airbrush's 0.06 per dab) from banding as u8 accumulation would and lets a stroke cap
  its own opacity (crossing itself never darkens past what its pressure asks for); the tile
  stays u8 premultiplied because that is what tiny-skia composites with the layer's blend
  and opacity, what a tile costs in memory (256 KiB), and what goes to disk. A tile topped by
  one of the layer's last four strokes keeps that stroke's mask and the pixels under it
  (another 512 KiB), so appending stamps only the new dabs and lays down only the pixels they
  touched.

### Spike: replay cost

5,000 strokes of 30 points (all brushes but the highlighter, sizes 4 to 30 units, random
colors) over 1,920 × 1,080 units, 205 tiles at 2 px per unit:

| | native | wasm |
| --- | --- | --- |
| read the board (snapshot) | 31 to 39 ms | 95 ms |
| every tile replayed, cold | 1.86 to 2.27 s | 5.6 s |
| the whole 1920 × 1080 view at 100%, cold | 1.6 to 2.6 s | 4.2 s |
| every tile again, from memory | 146 to 197 ms (composing a 3840 × 2160 picture) | 918 ms |
| the view at 100% from memory | 50 to 76 ms | 545 ms |
| a new memory cache, tiles from the disk cache (17.8 MB, PNG) | 312 to 370 ms | (no disk in the test) |
| cold, saving every tile to disk | 2.0 to 2.6 s | |

A profile of the cold replay: 42% stamping dabs, 25% laying strokes down, the rest
compositing. **Cold replay is over a second**, so the design leans on the caches: the view
replays only the tiles it shows (a painting seen at 100% on a laptop is a fraction of these
205 tiles), every tile is kept in memory once replayed (256 MB budget by default, least
recently used out first), the disk cache brings a reopened painting back in about a fifth
of a cold replay, and Flatten (wave 5) is the answer for a history that has grown long. The
stamp loop already only visits each row's span inside the circle; a falloff table and SIMD
are what is left to try if real paintings show it is needed. **Margin**: the cold numbers
are about twice the one-second threshold under a load average of 25 on 24 cores; unloaded
they may come near it on the desktop (not in wasm, at 5.6 s). The choice does not hang on
it, since the caches are wanted for 10,000 strokes or a slower laptop either way, but
"cold replay is too slow on the desktop" should be re-measured idle before Flatten's
priority is argued from it.

### Stored keys

Only new keys; a board made before painting needs nothing, and an older app ignores them.

- **Each paint operation is its own key of the board map**, `paint:<op id>`, a `Map { layer,
  order, kind, author?, created_at, ... }`. **Deviation**: the contract drew one `paint` map
  holding the operations; a map made on first use would be made by two devices painting
  their first strokes while apart, and yrs keeps one of the two maps and drops the other's
  strokes (the rule node fields and `board:<id>` keys already follow). `order` is a
  fractional index among the operations of its layer; the replay order is (order, id).
- **stroke**: `preset` (the name), `color` (`#rrggbb(aa)`), `blend` (`normal` | `erase`),
  `brush` (one `Any` map, written once: `size`, `hardness`, `spacing`, `flow`, `opacity`,
  `pressure_size`, `pressure_opacity`, `tilt_size`, `tilt_opacity` (each `[at0, at1,
  gamma]`), `jitter_size`, `jitter_opacity`, `jitter_position`, `grain`, `wet_edge`) and
  `points`, an `Array` of `Buffer` chunks of little-endian f32 quadruples (x, y, pressure,
  tilt), appended while drawing as a pen's points are. The settings are stored whole so a
  preset changed later never redraws an old painting differently; a setting missing reads
  as the named preset's (or Round's).
- **pixels**: `src` (a `pimble-blob:` URL) and `rect` (`Any { x, y, w, h }`). Stored and
  drawn now; nothing makes one yet but `add_pixels` (wave 5's Flatten, Merge Down, paste).
- Any other `kind` (fill, smudge, blur, move) reads as `PaintBody::Other` and is skipped:
  the rest of the layer draws.
- Pressure is 0 to 1, **1 for an input without pressure** (a brush's size is its size at
  full pressure; a mouse sends 1, not the pen tool's 0.5). Tilt is 0 upright to 1 flat, 0
  without tilt.

### Public API (`pimble_board`)

- **Brush engine** (`brush`): `BrushSettings { size, hardness, spacing, flow, opacity,
  pressure_size, pressure_opacity, tilt_size, tilt_opacity, jitter, grain, wet_edge }`
  (`sanitized()`, `reach()`, `with_size(f32)`), `Curve { at0, at1, gamma }` (`at(t)`, `FLAT`),
  `Jitter { size, opacity, position }`, `StrokeBlend { Normal, Erase }`, `Preset { name,
  label, settings, color, layer_blend }`, `PRESETS: [Preset; 8]` (round, pencil, ink,
  airbrush, marker, charcoal, watercolor, highlighter, in menu order; consts `brush::ROUND`
  ... `brush::HIGHLIGHTER`), `brush::preset(name) -> Option<&'static Preset>` (name or
  label, any case), `BrushPoint = [f32; 4]`, `PX_PER_UNIT = 2.0`; `brush::dabs(settings,
  seed, points)`, `DabWalker`, `brush::seed_of(op_id)` (jitter is seeded from the op id and
  the dab's index, so appending never changes a dab already made).
- **Operations** (`paint_ops`): `RawPaintOp { id, layer, order, body: PaintBody, author,
  created_at }` (`kind()`, `bounds()`), `PaintBody { Stroke(Arc<StrokeOp>),
  Pixels(PixelsOp), Other(String) }`, `StrokeOp { preset, settings, color, blend, points }`,
  `PixelsOp { src, rect }`, `PaintOp { raw, bounds, fingerprint }`, `NewStroke { layer,
  preset, settings, color, blend, points }` with `NewStroke::new(layer, &Preset, Color)`,
  `.erasing()`, `.with_size(f32)`, `.with_settings(BrushSettings)`, `.with_points(Vec<BrushPoint>)`;
  `paint_ops::PAINT_PREFIX`, `paint_key(id)`. `RawBoard.paint: Vec<RawPaintOp>`;
  `Changes.paint: HashSet<String>` (op ids; `Board::watch` reports them, so an append is
  neither `board` nor `elements`); `Board::read_paint_some(&ids)`.
- **Writing** (`BoardTxn`; each judged by locks; refusals write nothing):
  - `paint_layer_for(picked: Option<&str>) -> Result<String>`: the contract's rule (the
    picked layer when it is a paint layer not locked and not in a hidden or locked group;
    else the nearest such paint layer above it that shows; else a new "Paint N", the first
    free number, just above the pick in its group, or above the top-level layer holding it
    when that group is hidden or locked; nothing picked: the topmost such paint layer that
    shows, else a new one on top). **The model makes "Paint N"**, unlike wave 2's "Vector 1"
    (which the UI makes): call it in the same `Board::edit` as `begin_stroke` and the layer
    and the first stroke are one undo step; then pick the answered layer in the panel.
  - `highlights_layer(picture: Option<&str>) -> Result<String>`: the paint layer named
    "Highlights" (`HIGHLIGHTS_LAYER_NAME`) just above the picture's layer (its next sibling),
    else one made there, blend multiply (`Preset::layer_blend`); with `None` the bottom of
    the canvas (reused when the bottom layer is one). Refused (`LayerLocked`) when that
    Highlights layer is locked. The UI calls it for the highlighter preset when the press
    lands on a picture (a canvas's picture element, which a picture in text also is).
  - `begin_stroke(NewStroke) -> Result<String>` (on top of its layer; answers the op id),
    `append_stroke(id, &[BrushPoint]) -> Result<()>` (one chunk; `Board::append_stroke(stamp,
    id, points)` too), `end_stroke(id) -> Result<bool>` (removes a stroke that never got a
    point; answers whether it stays), `add_stroke(NewStroke) -> Result<String>` (a whole
    stroke, refused with no points), `add_pixels(layer, src, Rect) -> Result<String>`,
    `delete_paint_op(id) -> Result<()>`. New errors: `BoardError::NotAPaintLayer { id, kind
    }` (`locks::not_a_paint_layer(kind)` is its sentence), `BoardError::NoPaintOp(id)`.
  - **The eraser** is a stroke with `blend: Erase` (`NewStroke::erasing()`), in a paint
    layer; in a vector layer the eraser tool still deletes elements.
  - **Undo** needs nothing new: the board's per-person `UndoManager` tracks the board map,
    so undo removes the person's own last operation, never a collaborator's (tested). Hold
    the undo clock across a stroke's begin, appends and end (wave 2's `undo_options`), so a
    stroke is one step.
  - `delete_layer` deletes the layer's operations with it; `copy_from` copies them. Merge
    Down and Merge Group across kinds are still refused, now "is not built yet" (wave 5).
- **Reading** (`BoardSnapshot`): `paint: Vec<PaintOp>` (by layer, order, id),
  `paint_ops(layer) -> &[PaintOp]`, `paint_bounds(layer) -> Option<Rect>`, `paint_op(id)`,
  `paint_shown(layer index) -> bool` (the layer and its groups shown and the markup shown,
  or `show_all`), `paint_layers_in(&Rect) -> Vec<usize>`, `paint_marks() -> Vec<PaintMark>`
  and `paint_tail_bounds(index, from) -> Option<Rect>` (what a raster cache follows),
  `picture_srcs() -> Vec<String>` (pictures and `pixels` sources: **the app's
  `boards::pictures_of` should use it** so `pixels` operations load), `paint_cache()`,
  `with_paint_cache(Arc<PaintCache>)`. `Layer::count` is a paint layer's operation count.
  `visible_extent()` includes shown paint; `markup_visible` off hides paint layers (as it
  hides every element that is not a picture).
- **Tiles and cache** (`tiles`): `TILE_PX = 256`, `TILE_UNITS = 128.0`, `tile_rect(tx, ty)`,
  `tiles_of(&Rect)`; `PaintCache::new() -> Arc<PaintCache>` (memory, 256 MB),
  `PaintCache::with_store(Option<Arc<dyn TileStore>>, budget_bytes)`, `stats() ->
  CacheStats { replayed, appended, stacked, loaded, hits }`, `memory_bytes()`, `clear()`,
  `tile_pixels(&snapshot, layer, tx, ty, &pictures) -> Option<Vec<u8>>`. **The disk hook**:
  `trait TileStore: Send + Sync { fn load(&self, &TileKey) -> Option<Vec<u8>>; fn
  save(&self, &TileKey, &[u8]); }`, pixels 256 × 256 × 4 premultiplied; `TileKey(u128)`
  (`hex()`) hashes the layer, the tile's place and every (operation, fingerprint) touching
  it, so a stale tile is never found. **Desktop**: `DirTileStore::new(dir)` (not on wasm),
  one PNG per tile under two-hex-digit folders, `prune(max_bytes)` removes the oldest; the
  app wires `<cache dir>/pimble/tiles`. **Browser**: an IndexedDB `TileStore` is the app's
  (IndexedDB is async; `load` must answer at once, so it would answer from what it has
  prefetched, or the browser stays on memory alone). A tile that needed a picture not yet
  loaded is drawn without it and drawn again next time.
- **Rendering**: paint layers composite at their place in the layer order through wave 1's
  buffers (blend, opacity, groups) in `draw_region`, so `BoardView`, `render`,
  `render_area`, `render_extent`, the navigator, export, pictures in text and embedded
  canvases (`text_boards.rs` draws through `render_area`/`render_extent`) all draw them.
  Zoomed in to 2 screen pixels per board unit or more a paint layer shows its pixels
  (nearest); below, it is sampled bilinear from half-size copies. Tiles are drawn without
  antialiased edges, so they meet with no seam at any zoom (tested). `render_paint_layer(&
  snapshot, layer, &pictures, scale, within: Option<Rect>) -> Option<Rendered>` is one layer
  alone (a layers-panel thumbnail). **SVG export** embeds each paint layer as one PNG
  `<image>` at 2 px per unit over what it paints, inside its layer's `<g opacity
  mix-blend-mode>`.
- **BoardView**: owns a `PaintCache` every snapshot it shows draws from
  (`set_paint_cache(Arc<PaintCache>)` hands it a disk-backed one, `paint_cache()`); a
  stroke that only grew re-renders only where its new points reach (`PaintMark::look` and
  `points`); `color_at(sx, sy) -> Option<Color>` reads the view's cache (what the person
  sees, background included).
- **Eyedropper**: `sample_color(&snapshot, &pictures, at: Point, background: Option<Color>)
  -> Color`, the composited canvas at a board point at the paint density, straight alpha
  (over `background` when given). The UI uses `BoardView::color_at` while the pointer is on
  the view, `sample_color` elsewhere.
- **Drawing a stroke, for the UI**: on press, one `Board::edit` with `paint_layer_for` (or
  `highlights_layer`) and `begin_stroke` with the first point; on each pointer event
  `append_stroke` (the tile work per append is only the new dabs); send the updates batched
  (about every 100 ms, `yrs::merge_updates_v1`) as the pen does; on release `end_stroke`.
  Between appends nothing else is needed: `BoardView::update` with the watch's `Changes`
  takes only the paint path (no element is resolved again).

### MCP

docs/MCP_CONTRACT.md "Painting": the `paint` tool (strokes with `preset`, `color`, `size`,
`opacity`, `erase`, `points` with optional pressure; `layer`, or `highlight_picture`, or
the brush's own rule making "Paint 1"); `get_canvas` summarizes each paint layer in a line
(kinds and counts, brushes, colors, where it paints, who painted), never its points;
`render_canvas` draws paint layers and fetches `pixels` pictures.

### Tests

`brush.rs` (appending never changes dabs already made, for every preset; spacing; jitter
seeded by the op id; pressure and tilt through the curves; a stroke crossing itself does not
darken; presets by name). `tests/paint.rs`: storage as `paint:<id>` keys with brush and
points, an empty stroke ended goes; a stroke draws and the eraser takes it away; hidden
layer, hidden markup and `show_all`; paint between vector layers, normal and multiply; a
stroke appended through a `BoardView` stamps only new dabs and matches a cold replay byte
for byte; tiles unchanged are hits, a stroke on top is laid over, a change under it
replays; no seams at five zooms; two people painting at once converge in one order; undo
takes my stroke, not yours; the layer rule ("Paint 1" with the first stroke, a locked one
passed over, "Paint 2"), refusals, a deleted layer takes its paint; the highlighter's
"Highlights" layer above a picture, multiplying it, reused; the eyedropper; `pixels` drawn
and redrawn once their picture arrives; an unknown kind skipped; extent, PNG, SVG,
thumbnail and Convert to Node carry paint; disk tiles come back the same; a snapshot
brought up to date keeps its cache. The MCP: strokes from specs, refusals, the summary.

### Deviations

- Operations are board-map keys `paint:<id>`, not entries of a `paint` map (above).
- `markup_visible` off hides paint layers whole, the highlighter's "Highlights" included
  (the contract: every element that is not a picture).
- A paint layer is not drawn when only some elements are rendered (`render_area` with
  `only`, a frame's export): paint belongs to no element.
- The highlighter on a canvas with no picture is the UI's choice: `highlights_layer(None)`
  gives a bottom multiply layer, `paint_layer_for` a normal one (the preset still paints
  its yellow at full opacity there).

### Not built

The UI (tools, context bar, keys, thumbnails, the IndexedDB store, wiring `DirTileStore`
and `picture_srcs` into the app). Fill, smudge, blur, move, paint selections, Flatten,
Merge Down across kinds and Merge Group with paint (wave 5). Dab shapes other than round
(no angle or roundness from tilt), textures beyond the board-fixed grain, a stroke's own
smoothing, a live preview of the stroke between appends (each append goes through the
document). Pictures in text replay their paint on every render (each render builds its
snapshot, so its cache is new); a cache shared per board on that thread would keep it.

## Built in wave 4 (UI)

Built 2026-11-17 in `crates/pimble-app` (both apps, one code path), on the model above;
`crates/pimble-board` is not touched.

Wave 4 (defaults the PM chose on 2026-11-17 so the UI could proceed; Joe has not confirmed them yet):

10. **The highlighter always paints into a multiply "Highlights" layer**
    (`highlights_layer`), over a picture or not.
11. **Hiding markup hides the Highlights layer too** (the model's rule for every paint
    layer).
12. **The tile cache**: 256 MB in memory; on the desktop a disk cache under the user's
    cache folder, pruned to 512 MB when the app starts.

- **Files**: `boards/paint.rs` (new: presets and settings, pressure, `[`/`]`, a stroke's
  press, appends, release and cancel, the held outbox, the eyedropper, the brush cursor,
  the shared tile cache, the layers' thumbnails), `boards.rs` (`Tool::Brush`,
  `Tool::Eyedropper`, `StyleUi.brush`/`brushes`/`eraser_size`, `Popup::Preset`,
  `BrushSize`, `BrushOpacity`, `Flow`, `Hardness`, `PaneUi.thumbs`, `Gesture::Paint` and
  `Eyedrop`, the session's `outbox`, `version`, `paint_pressure`; `pictures_of` reads
  `picture_srcs()`, so `pixels` operations load their pictures, and a picture that
  arrives redraws them), `boards/tools.rs` (routing, pressure, keys), `boards/rail.rs`
  (the Paint group, the context bar's paint pickers, `[`/`]` from the chrome),
  `boards/layers.rs` (thumbnails, New Paint Layer), `boards/points.rs` (the cursor in the
  edit overlay), `text_boards.rs` (the shared cache), `styles.rs`.
- **Tools and letters**: the rail is Select (V), Direct select (A), Hand (H) | Pen (P),
  Pencil (N), the shapes flyout (Rectangle U, Ellipse O, Diamond D, Polygon Y, Star K,
  Line L, Arrow W) | the Paint flyout (Brush B, Highlighter M), Eraser (E), Eyedropper (I) |
  Text (T), Sticky note (S), the diagram flyout (Connector C, Numbered step J, Frame F),
  Discussion pin (Q) | pictures. **G and R are unbound**, kept for wave 5's Fill and Smudge
  (Shift+R Blur). **Moved**: Rectangle R to U (the contract's shapes letter), Polygon G to
  Y, Numbered step I to J. Every tool still has its own letter (tested). Wave 5's M and L
  (paint selections) collide with the Highlighter and the Line; that wave decides.
- **The Highlighter is the brush's highlighter preset** (decision 3): a tool of its own
  only so the Paint flyout can show it as a face and M can pick it. It paints with the
  `highlighter` preset into `highlights_layer(picture)`: the picture under the press (a
  picture in a locked layer counts), else, while marking up a picture in text, that
  picture, else `None` (the bottom of the canvas). It leaves the layers panel's pick
  alone. Picking "Highlighter" in the brush's preset picker picks the tool; picking any
  other preset there picks the Brush. Old vector highlighter strokes stay elements and
  are selected and styled as before; nothing makes new ones.
- **A stroke** (Brush, Highlighter, Eraser in a paint layer), the model's recommended
  flow: on the press one edit with the layer (`paint_layer_for(picked)` for the brush,
  which picks the answered layer in the panel, so "Paint 1" made by the first stroke is
  picked) and `begin_stroke` with the first point, sent at once; each pointer move
  appends its point to the document here at once (`append_stroke`; the view redraws only
  the new dabs) and the updates are kept in the session's outbox and sent merged
  (`yrs::merge_updates_v1`) about every 100 ms (a timer sends what a still pointer left);
  the release appends its point and `end_stroke`s, and sends the rest. The undo clock is
  held from the press to the release: **a stroke is one undo step**, and a stroke that
  made its layer takes the layer back with it (tested). A cancelled pointer, the first
  finger of a pinch or Escape deletes the operation (unlike the vector pen, whose element
  is written only after its first 100 ms, the stroke's begin was already sent, so a
  cancel sends a delete). Closing the pane mid-stroke ends the stroke.
- **Settings**: the color is the color well's **front square** (a gradient's first color;
  the highlighter left on the marks' red paints its own yellow); each preset keeps its own
  size, opacity, flow and hardness for the session (`StyleUi.brushes`, starting from the
  preset); the stroke stores them whole (`NewStroke::with_settings`). The eraser is the
  round brush at its own size (`eraser_size`, 24 units first).
- **Pressure**: a pen's (and a pen's eraser end's) pressure as rinch reports it, a
  moment of none mid-stroke keeping the last; a mouse and a finger paint at 1 (a touch
  screen without force sensing reports 0.5 whatever the touch). **Tilt is always 0**:
  rinch's `SurfacePointer` carries no tilt (a rinch mechanism to add, not done here).
- **Context bar** for the Brush and the Highlighter: the brush color (a disc; a click opens
  the well), then pickers in the `picker_button` style: the preset (its label; the eight
  presets as chips), the size (a logarithmic slider, 1 to 250 units, halves below ten),
  the opacity, "Flow N%", "Hard N%" (each a slider with its value). The Eraser shows its
  size only; the Eyedropper a hint. The bar is about 460 pixels wide for the brush.
- **Keys**: B, M, E, I; `[` and `]` make the brush (or the eraser) about a fifth smaller
  or larger, at least a unit (`paint::next_size`, tested), on the canvas and from the
  chrome (`KeyAct::Bracket`); Ctrl+`[`/`]` still order the selection. **Alt held with the
  Brush or the Highlighter picks colors**: a press is the eyedropper's (the surface
  tracks Alt from its keys, pointer events carry no modifiers).
- **Eraser**: by the picked layer's kind: a paint layer is erased with an erasing stroke
  (`NewStroke::erasing()`, the eraser's size), a locked one is refused with the layer's
  sentence; a vector layer loses whole elements as before. **With nothing picked**: an
  element under the press is erased as before, else the topmost shown, unlocked paint
  layer painting there is erased with a stroke (a pen's eraser end does the same with any
  tool).
- **Eyedropper**: a press (and a drag, and the release) reads `BoardView::color_at`
  (what the person sees, background included), `sample_color` over the background when
  the view has no settled pixels there; the color goes into the well's front square
  (its gradient cleared) and the recent colors. **Deviation**: it does not write the
  selection's style as picking a color in the well does: picking is looking.
- **Brush cursor**: a circle of the brush's (or eraser's) size at full pressure where the
  pointer is, drawn as four cubic curves of wave 2's edit overlay (at least 3 logical
  pixels across), hidden while panning, while Alt picks, and when the pointer leaves.
- **The tile cache**: one `PaintCache` for the app (`boards::shared_paint_cache`):
  every board pane (`BoardView::set_paint_cache`), the navigator, exports, the thumbnails
  and pictures in text (`text_boards::draw` hands each render's snapshot the shared
  cache, so a picture in text keeps its tiles between renders and shares them with a
  pane showing the same canvas: the model's "not built" line). Tiles are keyed by layer
  and operations, so one cache for every board is safe; 256 MB. **Desktop**: a
  `DirTileStore` at `<cache dir>/pimble/tiles` (one folder for every board, as
  `DirTileStore` keys tiles), pruned to 512 MB on a background thread when the cache is
  first made (the first canvas shown in a run), not again per store opened. **Browser**:
  memory only; **the IndexedDB tile store is not built** (IndexedDB is asynchronous and
  `TileStore::load` must answer at once, so it needs a prefetch layer: not small). Tests
  use memory only.
- **Layers panel**: a **thumbnail per layer** between the kind icon and the name (48 ×
  36 pixels shown at 24 × 18, a `data:` PNG in an `<img>`): a paint layer through
  `render_paint_layer` (from the shared tiles), a vector layer through `render_area`
  limited to its elements; groups have none. They are drawn a moment (600 ms) after the
  board stops changing and no gesture is under way, only for layers whose operations or
  elements' fingerprints changed, off the UI thread on the desktop; never per frame. The
  paint layer's row counts its operations. **New Paint Layer** (a brush icon) beside New
  Layer makes "Paint N" above the picked layer, in its group, and picks it.
- **Tests**: the preset picker's data and the highlighter as its own tool, pressure (a
  mouse 1, a pen its own, a pen's nothing kept), `[`/`]` steps, the cursor's circle, the
  thumbnail box (`paint.rs`); the Paint group's flyout and faces, letters (G and R free),
  the paint tools' controls, the logarithmic size slider, `[`/`]` from the chrome
  (`rail.rs`); pointer events on the highlighter (pressure stored, a mouse at 1, the
  Highlights layer multiply and reused, appends batched, a cancel and a pinch leave
  nothing, the pen's eraser end erasing paint with a stroke); the brush making a picked
  "Paint 1" with its preset, color and settings, `]` sizing the next stroke, the
  eyedropper picking the stroke's green, the eraser on the picked paint layer, undo stroke
  by stroke and the layer with the first; the highlighter over a picture making
  "Highlights" right above the picture's layer and hidden with the markup, Alt with the
  brush picking instead of painting (`boards.rs`); the highlighter on a picture in text
  minting the board and painting into its Highlights (`text_boards.rs`).
- **Checked in the test app** (desktop, scratch store, a 1200-pixel window): strokes with
  Round, Watercolor, Charcoal and Airbrush (the context bar fits with room to spare), the
  eraser across three of them with `]` twice (24 to 35), the eyedropper taking a blue off
  a gradient rectangle and an Ink stroke in it, that stroke undone whole, "Paint 1" made
  and picked with its thumbnail, a picture inserted and highlighted twice from the Paint
  flyout's Highlighter (a "Highlights" layer right above the picture's, Multiply, its
  yellow multiplied to green over blue), the brush cursor at each size; the app
  restarted twice: tiles written under `<cache dir>/pimble/tiles` by the first run were
  neither written again nor added to by the second reopen (the painting came from disk).
  Alt picking and pictures in text are covered by tests only (the driver cannot hold
  Alt; the markup view is the same session).
- **Not built**: tilt (rinch has none); the browser's IndexedDB tile store; a live
  preview between pointer events (each event goes through the document, as the model
  says); pruning the disk cache when a store opens (only at the first canvas of a run);
  thumbnails while the panel is folded are still drawn; a brush cursor that shows the
  pressure's size. **Rough edges**: the cursor can stay drawn where the pointer left the
  canvas for the floating chrome (the surface hears no leave there); deep layer groups
  squeeze a row's name now that a thumbnail sits beside it.

## Built in wave 5 (model)

Built 2026-11-17 in `crates/pimble-board` and `crates/pimble-mcp`, no UI: the Fill (G),
Smudge (R) and Blur (Shift+R) tools, paint selections (rectangle and lasso, their handles,
Delete), Flatten Layer in the layers panel and the blob flow for it and the merges are the
UI half. `crates/pimble-app` is not touched (it compiles as it was: its matches on
`PaintBody` have a fallback arm).

### Stored keys

Only new keys and new values of `kind`, on the `paint:<op id>` maps wave 4 made; an older
app reads every new kind as an operation of a later version and skips it (the rest of the
layer draws).

- **fill**: `seed` (`Any { x, y }`), `color` (`#rrggbb(aa)`), `tolerance` (a number, 0 to 1),
  `clip` (`Any { x, y, w, h }`), `contiguous` (`false` only when it is off; absent reads
  true). A fill missing its seed, clip or color reads as a kind this version cannot draw.
- **smudge** and **blur** (`kind` is the effect): `preset` and `brush` exactly as a stroke's,
  `points` (the same `Array` of `Buffer` chunks, appended while drawing), `strength` (0 to
  1, absent 0.5).
- **move**: `selection` (`Any { kind: "rect", x, y, w, h }` or `Any { kind: "lasso", points:
  [x0, y0, x1, y1, ...] }`, board units) and `transform` (`Any [a, b, c, d, e, f]`, board
  units, SVG's matrix order: x' = a x + c y + e, y' = b x + d y + f). **No `transform` is a
  Delete**: the selection's pixels are lifted and nothing is put down. The transform is one
  value, last writer wins, like an element's geometry.
- **pixels** gain `blend` (written only when not normal): how the picture is laid onto the
  layer's own pixels. Merge Down of a layer that blends keeps its blend here. An older app
  draws such pixels normally.

### How they draw

- **Fill**: the layer's pixels as they are at the fill's place in the order (the layer's
  own pixels, not the composited canvas: see "Not built"), premultiplied RGBA at 2 pixels
  per unit; a pixel fills when each of its four channels is within `tolerance × 255` of the
  seed pixel's; contiguous fills are 4-connected (a scanline flood), the rest take every
  matching pixel in the clip; the coverage is then **grown by one pixel** (0.5 units) so the
  fill reaches under a soft brush edge instead of leaving a light halo; the fill color is
  laid over the covered pixels as a stroke is (source-over). Nothing outside the clip
  fills, so a fill on the infinite canvas ends. The flood reads only the tiles it reaches.
- **The clip** (`raster::fill_clip(snapshot, layer, seed, view)`), what the filler's device
  computes: what the filler sees (`view`, the visible board rectangle; with none, the
  canvas's visible extent grown by `FILL_MARGIN`, 64 units, or 1,024 units around the seed
  on an empty canvas) together with the layer's painting grown by 64 units (fills aside,
  so fills do not grow each other's areas), so an enclosed shape partly off screen fills
  whole; at most `MAX_FILL_UNITS` (2,048 units, 4,096 pixels) on a side, the window placed
  around the seed; on whole paint pixels. `add_fill` refuses a clip larger than that, a
  seed outside it, and anything but an unlocked paint layer.
- **Smudge**: the brush's dabs (as a stroke's: spacing, pressure, jitter seeded by the op
  id) over the layer's pixels below it: the first dab loads the brush with the pixels under
  it; each later dab mixes the pixels under it toward what the brush carries by `strength ×
  coverage` (coverage: the brush's falloff and hardness times the dab's alpha), and the
  brush picks up what was there by `(1 − strength) × coverage`. Worked in f32, laid back as
  premultiplied bytes.
- **Blur**: each dab replaces the pixels under it with a box blur of them (radius a quarter
  of the dab's, 1 to 8 pixels) by `strength × coverage`; dabs overlapping blur again, so a
  slower, denser stroke blurs more.
- **Move**: the pixels whose centers are inside the selection (a lasso by the nonzero rule;
  a rectangle's right and bottom edges outside) are lifted from the layer as it is below the
  move, cleared, and drawn back through the transform (nearest when it is a shift by whole
  pixels, else bilinear) over what is left. With no transform, only cleared.
- Every one is deterministic on one device (no randomness but the op id's seed); the
  contract's accepted difference between the desktop and the browser stands.

### Tiles and the cache

- A fill, smudge, blur or move crosses tiles, so the cache works each out **once over its
  whole area** and lays the result into each tile it touches: a fill's coverage per tile,
  a smudge's or blur's finished pixels over its box (the box's pixels replace the tile's
  there), a move's lifted pixels. These "derived" results are kept in the memory cache with
  the tiles (counted in its budget, least recently used out first).
- They read **the layer below them**: tiles replayed from the operations below the one
  that reads (kept apart from the layer's own tiles, keyed by the reading operation, never
  saved to disk), which in turn may need what an earlier fill or smudge made of the
  pixels below it (worked out first, recursively).
- **Effective fingerprints**: `PaintOp.effective` is an operation's fingerprint and, for one
  that reads the pixels below it, the effective fingerprints of every operation below it in
  its layer that reaches where it reads (`RawPaintOp::read_bounds`: a fill's clip, a
  smudge's box, a blur's box grown by its widest kernel, a move's selection box). Tiles (in
  memory and on disk, `TileKey`) and the view's `PaintMark`s are keyed by effective
  fingerprints, so **a change below a fill (a wall of its area taken away, an undone
  stroke) replays every tile the fill touches, and the view redraws its whole area**, not
  only the changed stroke's (tested). A move's lifted pixels depend only on its selection
  and what is below, so dragging a move on (`set_move_transform`) re-lays its tiles without
  lifting again.
- **Bounds**: a fill touches its clip, a smudge or blur its points grown by the brush's
  reach and a unit, a move its selection's box together with where the transform puts it.
- **Streaming**: a smudge or blur appended re-works its whole box (not only the new dabs:
  not built) and the tiles under it replay; the view redraws only where the new points
  reach (unless something below changed). The incremental stamping of wave 4 stays for
  strokes.
- A derived result that needed a `pixels` picture not loaded yet is worked out again next
  time, as a tile is.
- **Limit**: a smudge, blur or move whose box passes 4,096 × 4,096 pixels (2,048 units on a
  side) draws nothing (`MAX_WORK_PX`). Working out what is below nests as deep as the
  fills, smudges, blurs and moves stacked over one another (each level a call deeper).

### Public API (`pimble_board`)

- **Types** (`paint_ops`, at the root): `PaintBody` gains `Fill(FillOp)`,
  `Retouch(Arc<RetouchOp>)`, `Move(MoveOp)` (`kind()` names them; `is_local()` is false for
  the three). `FillOp { seed: Point, color, tolerance, clip: Rect, contiguous }`; `Retouch {
  Smudge, Blur }` (`as_str`, `parse`); `RetouchOp { effect, preset, settings, strength,
  points }`; `MoveOp { selection: Selection, transform: Option<PixelTransform> }`;
  `PixelsOp` gains `blend: Blend`. `RawPaintOp::read_bounds() -> Option<Rect>`. `PaintOp`
  gains `effective: u64`. Constants `paint_ops::DEFAULT_TOLERANCE` (0.15),
  `paint_ops::DEFAULT_STRENGTH` (0.5).
- **Selections, pure** (for the UI's marquee, lasso and handles): `Selection { Rect(Rect),
  Lasso(Vec<Point>) }` with `bounds() -> Option<Rect>`, `is_valid()`, `contains(Point) ->
  bool` (what a pixel center is tested with), `outline() -> Vec<Point>`,
  `outline_through(&PixelTransform) -> Vec<Point>` (where the moved selection lands).
  `PixelTransform { a, b, c, d, e, f }` with `IDENTITY`, `translate(dx, dy)`, `scale(sx, sy,
  about)`, `rotate(degrees, about)` (clockwise, the y axis down), `then(&next)` (this, then
  `next`), `apply(Point)`, `inverse() -> Option<_>`, `map_rect(&Rect) -> Rect`,
  `is_finite()`, `is_identity()`, `to_array()`, `from_array([f32; 6])`.
- **New operations** (`NewFill { layer, seed, color, tolerance, clip, contiguous }` with
  `NewFill::new(layer, seed, color, clip)`, `.with_tolerance(f32)`, `.everywhere()`;
  `NewRetouch { layer, effect, preset, settings, strength, points }` with
  `NewRetouch::new(layer, Retouch, &Preset)`, `.with_size`, `.with_settings`,
  `.with_strength`, `.with_points`). `BoardTxn`, each judged by locks, refusals writing
  nothing:
  - `add_fill(NewFill) -> Result<String>`.
  - `begin_retouch(NewRetouch) -> Result<String>`; `append_stroke(id, &[BrushPoint])` and
    `end_stroke(id)` take a smudge or blur as they take a stroke (stream it the same way,
    the undo clock held from press to release); `add_retouch(NewRetouch)` (a whole one,
    refused with no points).
  - `add_move(layer, Selection, Option<PixelTransform>) -> Result<String>` (refused with a
    selection that encloses nothing or a transform that flattens it);
    `set_move_transform(id, Option<PixelTransform>) -> Result<()>` (the UI's drag, scale and
    turn after the first write; `None` makes it a Delete).
  - `add_pixels_blended(layer, src, Rect, Blend) -> Result<String>`.
  - `apply_raster(&LayerRaster, src: Option<&str>) -> Result<Option<String>>` (below).
- **Flatten and merges as pixels** (`raster`, at the root): `RasterJob { Flatten(layer),
  MergeDown(layer), MergeGroup(group) }`; `rasterize(&snapshot, &dyn PictureSource,
  RasterJob) -> Result<LayerRaster, BoardError>`; `LayerRaster { job, png: Option<Vec<u8>>,
  rect: Rect, pixel_size: (u32, u32), blend: Blend, ops: Vec<String>, onto: Option<String>
  }`; `merge_needs_pixels(&snapshot, layer) -> bool` (Merge Down onto a paint layer, or a
  group holding one); `fill_clip(&snapshot, layer, seed, view: Option<Rect>) -> Rect`;
  constants `raster::MAX_FILL_UNITS`, `raster::FILL_MARGIN`, `raster::MAX_RASTER_PX`
  (8,192 pixels). New errors: `BoardError::NeedsPixels(sentence)` (what `merge_layer_down`
  and `merge_group` now answer where pixels are needed), `BoardError::PicturesNotLoaded(n)`;
  `PAINT_ONTO_VECTOR` is the refusal of a paint layer merged onto a vector layer.

### The blob flow, for the UI (and what the MCP does)

The model cannot store a blob, so Flatten Layer, Merge Down onto a paint layer and Merge
Group with a paint layer in it are two steps:

1. With the board's current snapshot and the pictures it draws loaded (the app's
   `pictures_of`, which reads `picture_srcs()`), call `rasterize(&snapshot, &pictures, job)`.
   A refusal is a sentence to show (paint onto vector, a hidden layer merged, a picture not
   loaded yet, more than 4,096 units across). For a merge, call it only when
   `merge_needs_pixels` says so; otherwise call `merge_layer_down` / `merge_group` as before
   (they answer `NeedsPixels` if called anyway).
2. When `png` is `Some`, put it as a blob of the node's store as a picture is put
   (`BackendCommand::PutBlob`; `pimble_image::fit` leaves a PNG under 20 MiB as it is, and
   one it shrinks is drawn stretched over `rect`, so the result is only softer). When the
   blob's URL comes back (`BlobStored`), make one `Board::edit` with `apply_raster(&raster,
   Some(&url))`; with no `png`, `apply_raster(&raster, None)` at once. One undo step.

What `apply_raster` writes: **Flatten** deletes the operations the raster drew (`ops`) and
puts one `pixels` operation at the lowest one's order, so operations painted while the
blob was stored stay above it (tested); the layer keeps its own opacity and blend.
**Merge Down** puts the pixels on top of the paint layer below with the merged layer's
blend (its opacity is in the pixels), then deletes the merged layer with its elements or
operations; it is refused when the layer below is no longer `onto`. **Merge Group** deletes
everything inside the group and makes the group entry a paint layer (keeping its id, name,
visibility, opacity and blend; pass-through reads as normal) holding the pixels.

### Deviations

- **Merge Group rasterizes everything in the group into one `pixels` operation**, paint
  layers included, where the contract says the vector layers are rasterized into the paint
  one: moving a paint child's operations would lose that child's opacity and blend, and a
  fill or smudge moved under another child's pixels would read different pixels. The
  result looks as the group did.
- **A hidden layer is not merged**: Merge Down of a hidden layer, or Merge Group with a
  hidden layer inside, is refused with a sentence (it would merge as nothing and its
  content would go). Painting apps often discard hidden layers here; asking first was the
  safe default.
- **Merge Down onto a paint layer works from a paint layer too** (the same flow), which the
  contract did not list; a paint layer onto a vector layer is refused, as it says.
- A fill reads its own layer only (see "Not built"), and grows by one pixel.
- Merge Down bakes the merged layer's opacity into the pixels and keeps its blend on the
  `pixels` operation, so the blend now applies against the paint layer's own pixels rather
  than everything below (as painting apps do); an older app draws those pixels normal.
- The PNG round trip (premultiplied to straight alpha and back) moves faint pixels by a few
  levels (at most 4 of 255 over white in the tests).

### Measured (under load)

Release build, `tests/wave5.rs` probe, the machine shared with other builds (load average
53 to 60 on 24 cores): a 1,920 × 1,080-unit view of 200 hard strokes and two fills whose
clips were that view, cold: 1.26 s (every tile, the tiles below each fill, two floods over
8 million pixels); Flatten of that layer from warm tiles, PNG included: 223 ms (4,046 ×
2,256 pixels, 194 KB); a 30-unit smudge streamed four points at a time through a 1,920 ×
1,080 `BoardView`: median 11.7 ms per append, p95 17.9 ms. Re-measure idle before tuning.

### Tests

`retouch.rs` (a fill inside a ring, grown a pixel, ended by its clip, not contiguous;
across tiles reading each once; blur and smudge, deterministic), `tests/wave5.rs`: a fill
stored and refused; a fill in a ring and, the ring deleted, the same fill redrawn over its
whole clip by a `BoardView` and matching a cold render; a fill and a concurrent stroke
below it the same on two devices, and through the disk cache; a smudge streamed through a
view matching a cold render; a blur softening an edge; moves (shift, dragged on, Delete, a
lasso turned 90 degrees) and refusals; transforms composed and inverted; Flatten in one
undo, matching the history's pixels, refused for a missing picture, keeping what was
painted meanwhile; Merge Down of a vector layer with opacity and multiply matching before,
paint onto vector refused, `NeedsPixels`; hidden layers and a changed layer below refused;
Merge Group of a paint and a vector layer matching before; a broken fill skipped. The
older layer tests now expect `NeedsPixels`. The MCP: fill specs and the summary's new kinds.

### Not built

The UI. **"Sample all layers"** for a fill (filling against the composited canvas): it
would make a fill depend on every layer below it, so its tiles would replay on any change
anywhere beneath; the cheap way is the filler's device computing the fill against the
canvas and storing the result as a `pixels` operation (frozen, not live): Joe to choose.
Incremental smudge and blur while streaming (each append re-works the whole stroke's box);
a smudge, blur or move larger than 2,048 units; soft (antialiased) selection edges and
feathering; a fill's own "grow" or "gap closing" settings; an MCP tool for smudge, blur or
moves; pasting an image into a paint layer (`add_pixels` is ready for it).

### Open questions for Joe

1. Sample all layers for fills: not at all, frozen as pixels, or live?
2. Hidden layers in a merge: refuse (built) or discard as painting apps do?
3. The fill clip: the view together with the layer's painting (built), or the view alone?
4. Fill tolerance 0.15 and smudge/blur strength 0.5 as the defaults.

## Built in wave 5 (UI)

Built 2026-11-17 in `crates/pimble-app` (both apps, one code path), on the model above.
`crates/pimble-board` gained only `EditOverlay.dashed` and `dash_phase` (closed outlines
drawn as marching ants: a white line under a dashed one).

- **Files**: `boards/paint_select.rs` (new: the paint selections, their drags, the ants),
  `boards/paint.rs` (the Fill, Smudge and Blur, their sizes and pickers' data),
  `boards/layers.rs` (Flatten Layer, the merges as pixels, the blob flow), `boards.rs`
  (the tools and their keys, `StyleUi.tolerance`/`contiguous`/`retouch_size`/`strength`,
  `Popup::Tolerance`/`FillMode`/`Strength`, `PaneUi.rasterizing`, `Gesture::PaintSelect`
  and `PaintSelDrag`, the session's `paint_sel` and `ants`, `seed_picture`, the handles of
  a paint selection), `boards/tools.rs` (routing, keys), `boards/rail.rs` (the rail, the
  context bar), `boards/points.rs` (the overlay), `styles.rs`.
- **The rail's Paint group**: the brush flyout (Brush, Highlighter), Eraser, a **fill
  flyout** (Fill, Smudge, Blur), a **selection flyout** (Rectangle select, Lasso select),
  Eyedropper. Each flyout remembers its face as the others do.
- **Letters** (every tool still has its own key, tested): Select V, Direct select A, Hand
  H | Pen P, Pencil N, the shapes flyout (Rectangle U, Ellipse O, Diamond D, Polygon Y,
  Star K, **Line Shift+W**, Arrow W) | Brush B, **Highlighter Shift+B**, Eraser E, **Fill G,
  Smudge R, Blur Shift+R**, **Rectangle select M, Lasso select L**, Eyedropper I | Text
  T, Sticky note S, Connector C, Numbered step J, Frame F, Discussion pin Q. **Moved**: the
  Highlighter from M and the Line from L, which the selections take as the contract says.
  No plain letter was free but Z, so a key may now be a letter with Shift
  (`Tool::shifted`, `Tool::from_chord`): Shift with a letter picks that letter's shifted
  tool, a variant of the plain one (the Highlighter is the brush's preset, a line is an
  arrow with no head), and otherwise changes nothing (Shift+N is still the Pencil; Shift+H
  and Shift+V still flip a selection). Tooltips and flyout menus say "Shift+R".
- **Fill**: a press fills at the point with the color well's front square (the brush's
  color), into the paint layer the brush would paint into (`paint_layer_for` of the
  layers panel's pick: "Paint N" made when needed, and picked), clipped by
  `fill_clip(snapshot, layer, seed, view)` with `view` the board rectangle the pane shows.
  The layer (when made) and the fill are two writes under the held undo clock: **one undo
  step**. Context bar: the color disc, "Tol N%" (chips 0, 5, 15, 30, 50 and 80%; 15% the
  model's default) and "Contiguous" / "Everywhere". The tolerance and the reach are the
  session's, like every tool option.
- **Smudge and Blur**: brushes on the pixels already painted. They work the picked layer
  when it is a paint layer (a locked one refused with its sentence), else the topmost
  shown, unlocked paint layer painting under the press, else the topmost such layer with
  any painting; a canvas with none says "Smudge and Blur work on paint, and this canvas has
  no painting yet." Both use the round preset at a size of their own (30 units first;
  `[`/`]` and the size picker change it) and the strength (chips 10, 25, 50, 75 and 100%;
  50% first). They stream exactly as the brush does (`begin_retouch` on the press, appends
  sent merged about every 100 ms, `end_stroke` on release, the undo clock held: one undo
  step; a cancel deletes the operation), with the brush cursor. Alt does not pick colors
  with them (nothing to pick for).
- **Paint selections**: M or L, then a drag (a rectangle) or a drawn loop (a lasso, a point
  every 2 logical pixels, closed through where it was let go) over a paint layer chosen as
  for Smudge; smaller than 3 logical pixels either way it is no selection. It shows as
  marching ants in the edit overlay (a timer moves them every 200 ms while there is a
  selection) and the element selection's own handles on the box of where its pixels are
  now. A drag inside moves the pixels (Shift: along one axis), a side or corner handle
  scales them (Shift keeps the proportions, `resized_bounds` as for elements), the rotate
  handle turns them about the box's center (Shift: 15 degrees); each drag composes onto
  the transform the last left (`selection_from`, `dragged`, tested). **Live**: the first
  drag writes `add_move(layer, selection, Some(t))` (in a new undo step, the undo clock
  held from then on), every later write is `set_move_transform`, about every 80 ms and on
  release; between writes only the ants follow the pointer. **Enter**, a press outside the
  selection (which starts the next one), another tool, undo and closing the pane keep the
  pixels where they are and let the clock go: **the whole lift and place, however many
  drags, is one undo step** (tested). **Escape** deletes the move (the pixels go back) as
  wave 4 cancels a stroke; a cancelled pointer mid-drag puts the pixels back where that
  drag found them (deleting the move if that drag made it). **Delete/Backspace** clears the
  selected pixels: `set_move_transform(id, None)` when they were moved, else `add_move(...,
  None)`, in the selection's undo step, and the selection ends. A selection is this pane's
  own: nothing of it is written until its pixels move, so others see nothing of it until
  then.
- **Layers panel**: **Flatten Layer** (a stack icon, shown when the picked layer is a paint
  layer), and **Merge Down / Merge Group through the blob flow** when
  `merge_needs_pixels` says so (otherwise `merge_layer_down` / `merge_group` as before).
  The flow (`layers::rasterize_layer`): `rasterize` from a plain snapshot of the board
  (never the view's: color by author must not be baked in) with the shared tile cache and
  the pictures `pictures_of` has, off the UI thread on the desktop (inline in the browser),
  the PNG decoded there too; a refusal is said as a sentence (a hidden layer, a picture not
  here yet, too large, paint onto vector); a raster with no picture is laid down at once;
  else `PutBlob` with request ids from `1 << 41` (clear of image nodes' and text
  pictures'), and on `BlobStored` (`boards::stored` asks `layers::raster_stored` first)
  one `layer_edit` with `apply_raster(&raster, Some(&url))`: **one undo step**; a Merge
  Down then picks the layer it went into. When the blob's size is the raster's (it was not
  scaled to fit), the decoded picture and its bytes are put in this window's picture cache
  under the URL (`seed_picture`), so the pixels show at once and are never fetched back.
  Meanwhile the panel says "Flattening..." or "Merging...", its buttons are dimmed and take
  no presses, and another such job is refused with a sentence. A canvas closed before the
  blob came back changes nothing and says so.
- **Tests**: the rail (the Paint group's flyouts and faces, every tool its own key with
  the shifted ones, tooltips, the Fill's and the retouch tools' controls, the tolerance
  and strength chips' data, the chrome's keys), `paint_select.rs` (a drag as a rectangle
  or lasso and the ones too small or flat; moves, Shift on an axis, a second drag going on
  from the first; handle scaling with and without Shift; turning about the center, Shift
  by 15 degrees), `boards.rs`: a fill inside a painted ring (blue inside, nothing outside,
  the clip holding the ring, the tolerance stored, Everywhere not contiguous, each fill one
  undo step); a fill making "Paint 1" in its undo step; Smudge by R and Blur by Shift+R on
  the layer painted there, streamed, with their size after `]` and the strength, one undo
  each; rectangle select, two drags (one move, its transform the sum), a corner handle
  doubling it, Enter, the whole thing one undo step, Escape removing the move, a lasso and
  Delete clearing the pixels inside and keeping those outside, another tool keeping a
  move, a canvas with no paint refusing; Flatten through the blob flow ("Flattening..."
  meanwhile, nothing changed before the blob, one `pixels` operation looking the same, one
  undo step) and Merge Down of a vector layer onto a paint layer (the rectangle now pixels,
  the vector layer gone, the paint layer picked).
- **Checked in the test app** (desktop, scratch store, a 1200-pixel window): a ring
  painted in a new "Paint 2" and filled inside at 30% tolerance with a blue picked in the
  well (`w5_2_ring.png`, `w5_4_fill.png`); the Fill's context bar and tolerance chips
  (`w5_3_fill_bar.png`); Smudge at 100% dragged across the ring's top twice and Blur
  across its right edge (`w5_5_smudge_bar.png`, `w5_6_smudge_blur.png`); a rectangle
  selection's ants and handles, moved, turned about 45 degrees and kept with Enter
  (`w5_7_rect_select.png` to `w5_10_committed.png`); a lasso and Delete
  (`w5_11_lasso.png`, `w5_12_lasso_deleted.png`); Flatten taking "Paint 2" from 8
  operations to 1, looking the same (`w5_14_flattened.png`); a gradient rectangle in a new
  vector layer merged down onto "Paint 2" (the layer gone, 2 operations, the same look,
  `w5_15_vector_above.png`, `w5_16_merged_down.png`); the bars after widening the longer
  pickers (`w5_bars.png`). Screenshots are in the session's scratchpad.
- **Not built**: pasting an image into a paint layer (it needs the clipboard's picture
  routed to a paint layer instead of becoming a picture element, and a choice of where it
  lands and how it is moved before it is placed: more than a small change; `add_pixels`
  after `PutBlob` is ready for it). Copying or cutting a selection's pixels; a selection
  across layers; feathered or antialiased edges (the model's); adding to or subtracting
  from a selection (Shift/Alt with M or L); the selection's own context bar (flip, numeric
  transforms); a progress figure for a long Flatten (only the words); IndexedDB for tiles
  (wave 4's line stands). **Rough edges**: the smudge at full strength started off the
  paint drags transparency in (the model's smudge carries what it first loads, as painting
  apps do), which surprised in the check; a non-uniform scale of a turned selection shears
  it (elements keep proportions when turned; pixels follow the handles); the ants re-present
  the frame every 200 ms while a selection exists.

## Loose ends closed

Done 2026-11-17 on `master`, from the "Not built" and "Rough edges" lines above. Desktop and
browser share the code; checked in the desktop test app (screenshots `le_*.png` in the
session's scratchpad).

1. **Direct select: done.** Shift keeps a dragged anchor at a multiple of 45° from where it
   was, and a handle tip at a multiple of 45° from its anchor; with snapping on, the anchor
   pressed (a segment's first end) or the handle tip lands on the grid (both: the grid
   point projected onto the 45° direction; `points::constrained`, tested). **Anchors of
   several elements**: a marquee picks the anchors of every shown, editable element inside
   it (Shift adds to what was picked), and Shift-click on another element's anchor adds it;
   `PointEdit.others` holds the other elements' selected anchors, drawn as outlines and
   anchors (handles show on the element pressed last only). A drag of anchors moves all of
   them by the same board offset (each element through its own transform; live shapes turn
   into paths in the same edit), Delete removes them from every element, each one undo step
   (tested: `direct_select_constrains_and_snaps_points`,
   `direct_select_moves_anchors_of_several_elements`; `le_2_marquee_two.png`,
   `le_3_moved_two.png`). A plain press on an anchor not selected, or on a segment, keeps
   only that element's.
2. **Stroke controls: done.** The ends and corners picker has a **Miter** row (1, 2, 4, 8,
   10; shown while corners are mitered) and a **Dashes** text field ("e.g. 6 3 1 3", outlined
   red while it does not read as a pattern) applied as soon as it parses
   (`parse_dash_pattern`), cleared from the selection when emptied, and emptied when a dash
   preset is picked. New elements take both (the miter only with mitered corners and not
   the default); selecting shows them (`StyleUi.miter`, `dash_text`; tested;
   `le_4_detail.png`, `le_5_dashes_miter.png`).
3. **Outline Stroke (decision 9): done.** `BoardTxn::outline_stroke(ids)`: the stroke
   (width, ends, corners, miter limit, dashes) is outlined with tiny-skia's stroker (dashes
   first) at 4 pixels a unit and read back as anchors (`PathGeom::from_tiny`: quadratics
   become their cubics), filled nonzero with the stroke's color or gradient, width 0, under
   a transform that keeps it in place. An element with no fill and no text **becomes** its
   outline (its id, layer, order, group, meaning kept); one with a fill or text keeps them,
   loses its stroke, and the outline is a new path right above it. One transaction;
   refusals are sentences (no outline, no stroke, locked). In the selection's "more" menu
   ("Outline Stroke"), and the MCP's `outline_stroke` tool (docs/MCP_CONTRACT.md). Tested
   (`tests/loose_ends.rs`: a butt-capped line's area and look, a turned filled rectangle
   looking the same, five dashes, refusals; the app's one undo step; `le_6_more_menu.png`,
   `le_7_outlined.png`). Deviation: a stroke gradient moves to the fill with the same
   handles, laid over the outline's box, which is wider than the path's by half the stroke,
   so it shifts slightly.
4. **Pencil pressure: left.** The fitted path is a centerline with one width; carrying
   pressure means turning a pressure-varied Pencil stroke into a filled outline (the union
   of the discs and bands the pen draws, cleaned up by a boolean union and refitted). That
   is not cheap (the union of hundreds of circles through linesweeper, then fitting each
   contour) and it changes what the Pencil makes for tablet users: a blob whose anchors are
   the outline's, no longer an editable centerline. That trade is Joe's call; Outline Stroke
   now gives a filled outline of a constant-width path on request.
5. **Gradients: done (Delete); focal handles skipped.** A press on the well's stop bar gives
   it the keys (`Keys::Stops`); Delete or Backspace then removes the picked stop (at least
   two stay, as the x button). A focused field (the hex box) keeps its own keys, as
   everywhere (tested in `key_act`; not driven in the test app). The radial focal point has
   no model, so it has no handles.
6. **Layers panel: done.** Thumbnails are not drawn while the panel is folded or hidden by
   Tab; unfolding it draws what changed meanwhile. A row's indent is 14 pixels a level for
   three levels, 6 after, never more than 64 (`layers::indent_px`, tested), and a name keeps
   at least 48 pixels with an ellipsis (`le_0.png`). **Each panel folds to its icon**
   (Layers a stack, Meaning tags, Frames a frame): folded panels make a small column of
   icons at the right edge, a click unfolds (`le_1_folded.png`, `le_14_unfolded_thumbs.png`).
7. **Brush cursor: done.** The rail, the well, the context bar, the panels and the zoom
   corner hear the pointer coming onto them (`onmouseenter`) and take it off the board
   (`tools::pointer_off`, also what the surface's own leave does): the cursor, the Pen's
   segment to the pointer and this person's pointer for others go (`le_8`, `le_9`).
8. **Selections: done.** `BoardView` redraws only under the old and the new overlay when
   nothing but the overlay changed (pixels restored from the cache, the overlay drawn over
   them; a test checks the patched frame equals a whole compose exactly); the ants still
   march every 200 ms, but only while their pane has the focus. The frame is still handed
   to the surface whole (rinch has no partial submit; a rinch mechanism to propose if it
   shows). **A turned selection scales in its own frame**: a side or corner handle of the
   box is mapped to the selection's own side facing the same way, the drag is read in the
   selection's own space, and the scale is applied before its turn, so it never shears
   (`paint_select::scaled_in_own_frame`, tested; `le_11_turned.png`,
   `le_12_scaled_turned.png`). An unturned selection scales as before. The handles still sit
   on the box around the turned pixels.
9. **Smudge off the paint: done.** Each spot of the smudge brush loads the first time it
   passes over paint (at least 8/255 opaque) and lays nothing down before; a smudge started
   off the paint does nothing until it reaches some, and drags in no transparency
   (deterministic; tested). This changes how smudge operations replay: tiles cached on disk
   by an earlier build of wave 5 keep the old look until their operations change (the tile
   key has no renderer version; nothing before wave 5 had smudges).

**Still open** (as before): pasting an image into a paint layer, copying a selection's
pixels, selections across layers, feathered edges, adding to or subtracting from a
selection, the selection's own context bar, IndexedDB tiles, tilt, the focal point of a
radial gradient, Pencil pressure (item 4), handles drawn along a turned selection's own
sides.

## Paint spikes re-measured on a quiet machine (2026-11-17)

`cargo run -p pimble-board --release --example paint_bench` at load average about 5 on 24
cores (the wave 4 numbers were taken at 25 to 63). Native only; wasm was not re-measured.

- Dabs: 0.3 us (4 px), 0.9 us (16 px), 4.8 us (48 px), 37 us (120 px), 108 us (240 px).
- A long pressure stroke across a 4K view, four points per append: median 3.2 ms (round),
  4.4 ms (airbrush), 3.7 ms (charcoal); p95 5.5 to 7.0 ms; worst 9.5 ms.
- 5,000 strokes: cold, every tile 1.28 s; the view at 100% cold 1.16 s; from memory 137 ms
  (the view 37 ms); from the disk cache 258 ms.

Cold replay stays just over a second, so the design's lean on the tile caches and on
Flatten holds; drawing is comfortably inside a frame.
