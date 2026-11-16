# Drawing contract: vector and paint tools, and the canvas redesigned around them

Status: **decided; wave 1 built (model and UI), wave 2's model built** (see the end). Written 2026-11-14 from Joe's answers of that day
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
