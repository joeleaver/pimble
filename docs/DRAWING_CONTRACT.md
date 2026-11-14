# Drawing contract: vector and paint tools, and the canvas redesigned around them

Status: **decided, not built.** Written 2026-11-14 from Joe's answers of that day
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
