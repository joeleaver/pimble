# Drawing wave 6: clipping, stamps and symbols, soft edges, map lettering, pictures in paint

Status: **decided** (Joe, 2026-11-18: "Let's take all your recommendations for the first pass"; see "Decisions" at the end). Written
2026-11-18 from the Braynard map session's list: an agent drew an 11.8 × 11 km illustrated
county map with waves 1–5 and clipped, stamped and lettered around what the canvas could not
do. Builds on docs/DRAWING_CONTRACT.md (layers, paths, paint ops, tiles); where they
disagree, this one wins once decided. The session's other items (full geometry reads,
copying between canvases, tiled Flatten, paging, size readouts, the 10 MB load failure, the
3 GB memory peak) are fixes, built apart from this contract.

Everything stays co-edited: only new keys on CRDT maps, and every device renders the same
picture from the same document.

## 1. Clip masks

What the session needed: fields, forests, reeds and hatching kept inside a region, and a whole
map kept inside its frame. It clipped outside Pimble with shapely.

- **A layer or group takes a clip**: `clip: element id` on its layer map. The element is a
  closed path or shape in the same canvas; everything the layer (or group) draws, vector and
  paint, is drawn only inside it. The clip element itself still draws where it is, unless it
  is in a hidden layer (the usual way: a "Clips" layer kept hidden).
- **An element takes a clip too** (`clip` in its own map), for one-off cases.
- Rendering: tiny-skia's `Mask` from the clip's path, applied when the layer's buffer is
  blended down (the isolated-layer path from wave 1 already gives every clipped layer a
  buffer). Paint tiles are clipped at composite time, so the painting itself is kept whole and
  moving the clip moves the edge.
- A clip that names nothing, or a cycle (a clip inside the layer it clips), reads as no clip.
- The panel: "Clip to Selection" and "Release Clip" on a layer; the MCP: `clip` on
  `update_layer` and `update_elements`.

## 2. Stamps and symbols

What the session needed: one tree drawn once and placed thousands of times with jitter in
size, rotation and color. It faked this with 25,000 single-dab strokes, which made the store
unloadable for new clients.

- **A symbol** is a group of elements (vector, and optionally a small picture) kept in the
  canvas as a definition: `symbols: Map<id, { name, elements... }>`, not drawn by itself.
- **An instance field** is one element of kind `scatter`: a symbol id and a packed list of
  placements (x, y, scale, rotation, tint), stored as appendable packed chunks like stroke
  points. 25,000 trees are one element of about 25,000 × 20 bytes, not 25,000 map entries.
  Editing the symbol changes every tree.
- **A stamp brush** paints placements along the pointer (spacing, scatter, size and rotation
  jitter seeded from the element id, tint variation) into a scatter element: the "stamp
  brushes" of painting apps, but the result stays editable vectors. Optionally a paint
  version later (a dab shape from a picture).
- The MCP: `add_symbol`, and `scatter` taking a symbol and either a placement list or a
  region plus density (filled with seeded jitter inside a path, so "forest in this polygon"
  is one call).

## 3. Soft edges

What the session needed: regions and haze without hard edges; radial gradients made
everything a soft blob.

- **Feather** on any filled element and on a layer: a blur radius in canvas units, applied as
  a Gaussian blur of the element's (or layer's) mask when rendered. Cached in tiles like
  paint, since a large blur is expensive.
- **Layer blur** (a non-destructive filter) as the same thing applied to the layer's buffer.

## 4. Map lettering

What the session needed: names that stay legible on busy ground, names along rivers and
roads, letter spacing, and a serif.

- **Halo**: `halo_color`, `halo_width` on text (and sticky and shape text): the glyphs
  stroked behind their fill.
- **Letter spacing**: `tracking` in em, and **text on a path**: `along: element id` (a path
  or line), with `offset` along it and which side.
- **Fonts**: today one font is bundled (Noto Sans Regular, so every device renders the same).
  Adding a few bundled faces keeps that guarantee: Noto Serif, Noto Sans and Serif in bold
  and italic, and a condensed face for labels. About 2–3 MB more in both apps. A `font` key
  names one; anything unknown reads as Noto Sans.

## 5. Pictures in paint layers

What the session needed: code-made relief (hillshade) as the base, hand painting on top,
both kept.

- Pasting or dropping a picture into a paint layer, or the MCP's `paint` with a picture and a
  rectangle, makes a `pixels` operation (the op kind already exists, from wave 4), placed and
  scaled before it is committed (the wave 5 selection handles). Painting continues above it
  in the same layer; Flatten folds it in.

## 6. Order of building

1. Clip masks (unblocks the most, small renderer change).
2. Pictures in paint layers (the op exists; the placing UI is wave 5's).
3. Map lettering: halo, tracking, fonts; then text on a path.
4. Stamps and symbols (the biggest: a new kind, a brush, the MCP's region fill).
5. Soft edges (feather and layer blur, with their tile cache).

## Questions for Joe

1. **Clip masks**: on layers and groups (recommended), on single elements too, or both?
2. **Symbols**: editable vector symbols with a scatter element (recommended: compact, stays
   editable, changing the tree changes the forest), or paint-only stamp brushes (simpler,
   pixels)?
3. **Fonts**: bundle a small set (Noto Serif and Sans with bold and italic, plus a condensed
   face; about 2–3 MB, the same on every device; recommended), or allow fonts installed on
   the computer (more choice, but a canvas looks different on a device without them)?
4. **Soft edges**: is feather on elements and blur on layers worth it now, or later after
   the rest?
5. **Order**: is the order above right?

## Decisions (Joe, 2026-11-18)

All five recommendations, for the first pass:

1. Clip masks on **layers and groups** (not single elements yet).
2. **Editable vector symbols** with a `scatter` element (not paint-only stamp brushes).
3. **A small bundled font set**: Noto Serif and Noto Sans with bold and italic, and a condensed
   face; the same on every device.
4. **Soft edges last** in this wave.
5. The order in section 6: clip masks, pictures in paint layers, map lettering, symbols and
   scatter, soft edges.

## Built: clip masks

Built 2026-11-18 on `master` (part 1 of the order in section 6), in `crates/pimble-board`,
the layers panel (`crates/pimble-app/src/boards/layers.rs`, both apps) and `pimble-mcp`.

### Stored keys

One new key, on a layer's map (layers and groups alike): `clip`, the id of an element of the
same canvas. Nothing else is written; an older version ignores the key and draws the layer
unclipped. Last writer wins, like every key of a layer's map. `copy_from` (Convert to Node)
copies it.

### How it reads (`pimble_board::clip`, the one place)

- A clip is **in effect** when the element it names exists, is closed
  (`clip::is_closed`: a rect, ellipse, diamond, polygon, star or frame, or a path whose every
  subpath is closed with at least two anchors), and is not drawn inside the layer or group it
  clips (its layer, after the usual fallback for a layer that is gone, is not that entry or
  inside it). Otherwise it reads as no clip, on every device the same: deleting the shape
  (undo brings the clip back with it), moving it into the layer it clips, reshaping it into
  something open. Nothing is rewritten.
- The outline is the element's fill outline (a live shape's `shape_path`, a frame's
  rectangle, a path's subpaths with its fill rule) through its world transform, so moving,
  rotating, scaling or reshaping it moves the edge. A clip shape inside a clip shape's group
  element moves with that group.
- A clipped layer or group counts as **isolated**: it draws into a buffer of its own, which is
  blended down through a tiny-skia `Mask` of the outline. The mask is the size of the target
  being drawn (a view, a 256-pixel paint tile's composite, a 2,048-pixel flatten tile, an
  export), never of the canvas, so a map-sized canvas costs one target-sized byte mask per
  clipped layer while it is drawn.
- **Paint** is kept whole: its operations and tiles are untouched, and clipped when its
  layer's buffer is composited. Moving the clip shows paint that was hidden.
- **Nesting**: a clipped layer inside a clipped group is masked by its own clip into the
  group's buffer, which is masked by the group's: they intersect.
- **The raster cache**: each layer's composite hash (`composite_of`) folds in the outline's
  hash (`Clip::hash`) of every clip on its chain, so a clip that moves changes the
  fingerprints of every element it clips and the paint marks of every paint layer it clips,
  and the view redraws them.
- **Hit testing**: `Element::hits` (and `BoardSnapshot::hit`) refuse a point outside any clip
  the element draws inside (`Element::inside_clips`); a marquee does not take an element its
  clips leave nothing of. The visible extent (what an export or "fit" shows) is cut to the
  clips (`Element::shown_bounds`, `BoardSnapshot::clip_bounds`).
- **SVG export**: a clipped layer's `<g>` carries `clip-path="url(#pimble-clip-N)"`, its
  `<clipPath clipPathUnits="userSpaceOnUse">` holding the outline in board units (with
  `clip-rule="evenodd"` for an even-odd path) just before it.
- **Rasterizing**: Flatten keeps the layer, so its clip stays a key and the pixels are drawn
  whole; Merge Down of a clipped layer onto the paint layer below takes the clip into the
  pixels (only what showed is merged); Merge Group draws each clipped child through its clip,
  and the group's own clip stays on the layer the group becomes.

### API

- `BoardTxn::set_layer_clip(layer, Option<&str>)`: `None` releases. Refused with a sentence
  when the layer is locked, itself or by a group (`BoardError::LayerLocked`); when the canvas
  has no such element ("This canvas has no element ... to clip to."); when it is not closed
  (`clip::NOT_CLOSED`); when it is inside the layer or group it would clip
  (`clip::CLIP_CYCLE`, which suggests a hidden "Clips" layer).
- `RawLayer.clip` (as stored); `Layer.clip` (in effect) and `Layer.clip_named` (as stored);
  `BoardSnapshot::clip_of(at)`, `clip_bounds(at, rect)`; `Element::inside_clips`,
  `is_clipped`, `shown_bounds`; `clip::Clip` (outline, rule, bounds, hash, `contains`),
  `clip::is_closed`, `clip::unmet` (why a named clip is not in effect, in words),
  `clip::describe` (how the panel names a clip element: its note's first line, else its kind
  and the start of its id).

### UI (desktop and browser, the same `pimble-app` code)

Layer thumbnails show what a layer holds, unclipped (`render_area_unclipped` for a vector
layer; a paint layer's thumbnail was never composited); the clip icon says it is clipped.

The layers panel's button row gains **Clip to Selection** (mask icon: the picked layer or
group clipped to the one element selected on the canvas; nothing selected or several
selected says so; the model's refusals are said as sentences) and **Release Clip** (mask-off
icon, shown when the picked layer names a clip). A row that names a clip shows a small mask
icon after its name; its tooltip names the shape ("Clipped to the star 0da15e11: this layer
shows only inside it"), dimmed with the reason when the clip is not in effect.

### MCP

`add_layer` and `update_layer` take `clip` (an element id; in an update null or `""`
releases it, left out keeps it); `get_canvas` says `clipped to <id>`, or `clip <id> not in
effect (<why>)`. docs/MCP_CONTRACT.md "Clip masks".

### Tests

`crates/pimble-board/tests/clip.rs`: the stored key; pixels inside drawn and outside not, for
a vector layer and a paint layer; a visible clip still drawing; moving and reshaping the clip
moving the edge (and the clipped element's fingerprint changing); nested clips intersecting;
hit testing inside and outside; every refusal's sentence and a locked layer; a deleted clip
and a clip moved into its own layer reading as none; SVG `<clipPath>`; the raster cache's
frame after the clip moves matching a full render; Merge Down taking the clip, Flatten
keeping it, a copy keeping the key; two devices clipping and moving at once reading the same
picture. `pimble-mcp`: `layers_take_a_clip_and_release_it` against a real server.

### Deviations

- A clipped group that passes through at full opacity is isolated by its clip, so blend
  modes of the layers inside it blend with the group's own buffer rather than with what is
  below the group (as any isolated group's do). Painting apps differ here; this is the one
  rule the renderer already had.
- The pen's live stroke (the stamps drawn into the view while the pointer is down,
  `BoardView::extend_stroke`) is not clipped; the stroke is clipped once it is an element
  (on release). Paint strokes are clipped while they are drawn (they go through the
  snapshot).
- Merge Group of a group of vector layers only (no paint, no pixels) moves the children's
  elements into the group's layer and drops the children's own clips, as it already drops
  their opacity and blend.
- The MCP's `copy_layer` between canvases does not copy a clip (the copied elements get
  new ids); set it again on the copy with `update_layer`.

### Not built

- Clips on single elements (Joe's decision 1: layers and groups first).
- A clip made from text, a stroke, a picture's outline or a paint layer's pixels (an alpha
  mask).
- Inverted clips ("everything outside the shape").
- Showing the clip outline while the layer is picked.
