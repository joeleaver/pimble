# Drawing contract: vector and paint tools, and the canvas redesigned around them

Status: **decided; waves 1, 2 and 3 built (model and UI)** (see the end). Written 2026-11-14 from Joe's answers of that day
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

