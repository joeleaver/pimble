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

## Built: pictures in paint layers

Built 2026-11-18 on `master` (part 2 of the order in section 6), in `crates/pimble-board`,
`crates/pimble-app` (both apps, one code path: `boards/paint_select.rs`) and `pimble-mcp`.

### Stored keys

One new key, on a `pixels` operation's map (`paint:<op id>`): `transform`, `Any [a, b, c, d,
e, f]` in the `move` operation's order (board units, x' = a x + c y + e, y' = b x + d y + f),
written only when it is not the identity. The picture lies over `rect` and is put through
it. A malformed one, or one that flattens the picture, reads as none (the picture lies as
its rectangle says), which is also how an older app draws a placed picture: unturned and
unscaled by the handles, over its rectangle.

### Model (`pimble_board`)

- `PixelsOp.transform: Option<PixelTransform>`, `PixelsOp::new(src, rect)`,
  `PixelsOp::bounds()` (the rectangle through the transform's box: what the tiles, the view's
  redraws and the layer's painted area use). The fingerprint folds the transform in, so
  a placed picture's tiles are its own.
- `BoardTxn::add_placed_pixels(layer, &[(src, rect)], Option<PixelTransform>) -> Vec<id>`:
  one `pixels` operation per piece, on top of the layer in the order given, sharing the
  transform, in the caller's one transaction. Refused, writing nothing: no pieces, a piece
  with no area or no picture, a transform that flattens it (`paint_ops::PIXELS_FLAT`), a
  vector layer, a locked layer. `add_paint_body` refuses a flattening transform too.
- **Pieces** (`raster`): `needs_pieces(w, h)` (more than `RASTER_TILE_PX`, 2,048 pixels, on
  a side) and `picture_pieces(&Picture, rect) -> Vec<PicturePiece { bytes, rect, pixel_size }>`:
  a grid of at most 2,048-pixel pieces row by row, each encoded on its own (JPEG at
  `PIECE_JPEG_QUALITY` 90 when opaque, PNG otherwise), its rectangle its share of the whole
  rectangle with edges worked out from the whole picture's, so neighbours meet exactly; at
  most `MAX_RASTER_TILES` (256) pieces, else a sentence. So no blob holds more than one
  tile's pixels and a view decodes only the pieces it shows, as Flatten's tiles.
- **Drawing** (`tiles.rs`): a `pixels` operation with a transform, or one whose rectangle is
  off the paint pixel grid, is drawn through `render::draw_placed_picture`: bilinear, from
  the picture's nearest smaller level, its outline grown by half an output pixel so pieces
  that meet leave no antialiased seam (the edge pixels spread into it). Flatten's
  operations (on the grid, no transform) draw exactly as before.
- **The overlay**: `EditOverlay.picture: Option<OverlayPicture { picture, rect, transform }>`,
  drawn under the marks, its corners counted in the overlay's box, so the view redraws only
  where it was and is while it is dragged.
- **Clip masks** need nothing new: a placed picture is paint of its layer, kept whole and
  clipped when the layer is composited. Flatten folds it in like any operation.

### UI (desktop and browser)

- **Where a picture goes**: a paste (Ctrl+V on the desktop, the page's `paste` in the
  browser), files dropped on the canvas and the rail's Insert pictures button all reach
  `boards::add_to_board`, which now asks `paint_select::paints_into` first: when the layers
  panel's pick is a paint layer (that layer), or a paint tool is in hand (Brush, Highlighter,
  Eraser, Fill, Smudge, Blur, the selections, the Eyedropper: the layer the brush would
  paint into, `paint_layer_for` at commit, made then when there is none), the picture is
  placed as pixels; otherwise it is a picture element as before.
- **Placing**: the picture is decoded (off the UI thread on the desktop) and shown in the
  edit overlay with wave 5's selection handles and a still dashed outline (the ants do not
  march over a placement: it would redraw the picture five times a second), centered where
  it was dropped or on the view, at one unit per pixel or scaled down to fit 90% of the
  view (the same `lay_out` as picture elements). It is a `PaintSel` with `placing` set: a
  drag inside moves it (Shift: along one axis), a side or corner handle scales it (Shift
  keeps the proportions; a turned placement scales along its own sides), the rotate handle
  turns it (Shift: 15 degrees). **Nothing is written or stored while it is placed.** While
  there is one it takes every left press, whatever the tool.
- **Commit**: Enter, a press outside it (which does nothing more) or another tool. The
  pieces go to be stored (`PutBlob`, request ids from `1 << 42`; a picture up to 2,048
  pixels on a side is one piece stored as it came, a larger one is cut off the UI thread on
  the desktop); the layers panel says "Placing the picture..." meanwhile. When the last is
  stored: one edit (one undo step) making the paint layer when needed and laying a `pixels`
  operation per piece with the placement's rectangle and transform; the layer is picked. A
  one-piece picture stored at its own size is seeded into the window's picture cache so it
  shows at once. A piece that fails to store ends it with its sentence and writes nothing;
  a canvas closed meanwhile changes nothing and says so.
- **Drop**: Escape and Delete drop the placement; undo drops it and does nothing more;
  closing the canvas drops it. Several pictures at once: the first is placed, with a
  sentence ("A paint layer takes one picture at a time, so the first was taken.").
- Painting continues above it in the same layer; Flatten folds it in.

### MCP

`paint` takes `pictures` (laid first, under the call's strokes and fills): `src` (a
`pimble-blob:` URL) or `file` (fitted with `pimble_image::fit`, as `add_elements` and
`insert_canvas` take one), `x`, `y`, `w`, `h` (both left out: one unit per pixel; one: the
proportions), `rotation` (degrees clockwise about the center). A picture past 2,048 pixels
is stored in pieces, each its own blob; a `src` within the limit is laid as it is.
docs/MCP_CONTRACT.md, "Wave 6, pictures in paint layers".

### Tests

`crates/pimble-board/tests/placed_pixels.rs`: the transform stored and read, the bounds where
it shows, a quarter turn drawing each half where it went, the identity storing nothing; every
refusal writing nothing (a flattening transform's sentence, no pieces, no area, a vector
layer, a lock, one bad piece among good ones); a 2,500-pixel picture cut into 2,048 and 452
pixels that meet exactly, JPEG when opaque and PNG with transparency, laid down turned 30
degrees with no seam along the cut; a stroke painted above it, a clip hiding it outside,
Flatten folding picture and stroke into pixels that look the same; two devices reading the
same operation, fingerprint and colors. `pimble-app` (`boards.rs`): a 4,000-pixel picture
pasted with the brush in hand placed fitted to the view with handles and its overlay, moved
and scaled with nothing written or stored, Enter storing two pieces (`1 << 42` ids) and
nothing laid until both are stored, then two `pixels` operations sharing the transform whose
rectangles meet, a paint layer made and picked, a stroke painted over it in that layer, and
the picture with its layer one undo step; with a paint layer picked and the Select tool,
Escape and undo dropping placements with nothing stored or written, a drop centered where it
fell at one unit per pixel, a quarter turn kept by a press outside, the one piece stored as it
came, in the picked layer, shown at once turned, one undo step. `pimble-mcp`
(`paint_lays_pictures_into_a_paint_layer`, against a real server): a file with `w` and a
rotation, a 2,100-pixel file in two pieces, strokes above them, a `src` laid again at an `h`,
the rendering showing it, and the refusals.

### Checked in the test app

Desktop, scratch store, its own port (7483): a 2,600 × 1,400 picture inserted from the rail
with the Brush in hand, placed with handles fitted to the view (`w6p_1_placing.png`), moved,
scaled by a corner and turned (`w6p_3_moved.png`, `w6p_4_scaled.png`, `w6p_5_rotated.png`),
laid down by a press outside into the canvas's clipped "Paint 1", where the clip hid it
(9 operations to 11: two pieces, `w6p_7_committed.png`), and one undo taking both back
(`w6p_8_crop.png`); on the other canvas into a picked "Paint 2", scaled by its sides and
turned (`w6p_11_scaled.png`, `w6p_12_rotated.png`), Enter laying it down looking as placed
(`w6p_13_committed.png`), a brush stroke over it in the same layer (`w6p_14_painted_over.png`),
no seam where the pieces meet (`w6p_15_panels_hidden.png`, `w6p_15_seam_crop.png`), undo
taking back the stroke and then the picture in one step (`w6p_16_undo_stroke.png`,
`w6p_17_undo_picture.png`); Escape dropping a placement (`w6p_20_escaped_after_press.png`).

### Deviations

- The pixels op stores a full **transform** rather than a rotation: the handles' moves,
  scales (in a turned placement's own frame) and turns compose into an affine map, which
  the `move` operation already stores the same way. The MCP's `rotation` is that transform about
  the rectangle's center.
- **Keys after inserting from the rail or a drop from the file manager**: on the desktop the
  board hears Enter and Escape only once it has the keyboard, which a press on it gives
  (rinch cannot give a surface the keyboard from code, as the text tool's line says). A
  press on the placement (a drag of it, or a click inside) gives it; a paste with Ctrl+V
  already has it. A press outside commits regardless.
- **One picture at a time** into a paint layer; the rest of a multi-picture drop is left
  out with a sentence.
- The placement shows over everything (the overlay), and is clipped only once it is laid
  down; the picked layer's place in the stack and its clip apply from then on.
- The fit uses the whole view, panels included, as picture elements' does.

### Not built

- Placing several pictures at once; a placement's own context bar (numeric size and angle,
  flip buttons, opacity or blend before commit).
- Copying a paint selection's pixels to a placement (copy and paste inside a paint layer).
- An MCP `blend` or `opacity` for a placed picture (Merge Down's `blend` key already exists
  on `pixels`).

## Built: map lettering

Built 2026-11-18 on `master` (part 3 of the order in section 6), in `crates/pimble-board`
(`text.rs`, the new `along.rs`, `snapshot.rs`, `paint.rs`, `hit.rs`, `doc.rs`), the context
bar (`crates/pimble-app/src/boards/rail.rs` and `tools.rs`, both apps) and `pimble-mcp`.

### Fonts

Ten faces, all SIL OFL 1.1 (`crates/pimble-board/fonts/OFL.txt` says where each came from;
no Reserved Font Name): Noto Sans Regular (the full font, unchanged, so every older board
draws pixel for pixel as before), Noto Sans Bold, Italic and Bold Italic, Noto Serif
Regular, Bold, Italic and Bold Italic (Noto 2.004), and Noto Sans Condensed Regular and Bold
(instances at width 75 of the variable Noto Sans 2.015). The nine new faces are subset by
`fonts/subset.py` to Latin (Basic, Latin-1, Extended A, B and Additional), IPA, combining
marks, punctuation, super- and subscripts, currency, letterlike symbols, number forms,
arrows and a few math signs, with hinting and the layout tables dropped (nothing reads
them: no shaping, no kerning). A character a face lacks is drawn from Noto Sans Regular (a
Greek letter in a serif label). Condensed italic is the condensed face slanted about 12
degrees (no condensed italic is bundled). Every face shares Noto Sans's vertical metrics,
so line height and baselines do not change with the font.

**Size**: the new faces are 1,004,940 bytes (about 0.96 MiB; 100 to 126 KB each), embedded
with `include_bytes!` in both apps. Desktop `pimble` release binary: 188,063,936 bytes before,
189,183,808 after (+1,119,872, about 1.07 MiB). The browser's wasm carries the same
1,004,940 bytes verbatim as data, plus a few KB of code (not measured against a build of
the previous version).

### Stored keys

All new keys of an element's `style` map, each its own last-writer-wins value (one person
making a label serif while another makes it bold keeps both); an older app ignores them and
draws the text in Noto Sans Regular in its own box:

| Key | Value | Absent |
| --- | --- | --- |
| `font` | `"sans"`, `"serif"`, `"condensed"` | sans; an unknown name reads as sans |
| `bold`, `italic` | bool | false |
| `halo_color` | `#rrggbb[aa]` | white |
| `halo_width` | board units past the glyphs | none, or a tenth of the font size when only `halo_color` is set |
| `tracking` | em after each character but a line's last (drawn clamped to -0.5..2) | 0 |
| `along` | an element id | plain text |
| `along_offset` | 0..1 of the line's length | by alignment: left 0, center 0.5, right 1 |
| `along_side` | `"above"`, `"on"`, `"below"` | above |

`font` + `bold` + `italic` are three keys rather than one `serif-bold-italic` value so they
merge independently; the MCP takes either form. `along*` live in the style map with the
rest, so `set_style`/`clear_style`, duplicate and copy carry them; duplicating or copying a
text with its path points the copy at the copied path.

### Model

- `text::layout_styled(text, size, wrap, &TextStyle { font: FontChoice { family, bold,
  italic }, tracking })`; `text::layout` is it with the default. `Glyph` carries its face and
  advance. Tracking is in the layout, so measuring, wrapping, bounds, hit testing, alignment
  and export all see it.
- `ResolvedStyle.text`, `.halo: Option<Halo { color, width }>`, `.along`, `.along_offset`,
  `.along_side`; `StyleKey::{Font, Bold, Italic, HaloColor, HaloWidth, Tracking, Along,
  AlongOffset, AlongSide}`.
- **Halo**: `paint::text_ops` strokes the glyph outlines behind their fill, twice the width,
  round joins and caps, in the element's opacity, for every kind with text (text, sticky
  notes, shapes, callouts, frames' names, cards, steps, connector labels). The element's
  bounds grow by the width. Bars drawn for text too small to read carry no halo.
- **Text on a path** (`crate::along`, read in the snapshot once every element is placed):
  the target must be a path (its first subpath, curves flattened in 24 steps), line, arrow,
  pen or highlighter stroke, taken through its world transform (a group's move moves it).
  Anything else, nothing, or the text itself (the only cycle: a text is never a line) reads
  as plain text, the same on every device. Each glyph's middle is placed at its distance
  along the line and turned to its direction; past an end, straight on. Text reads left to
  right: when the stretch it covers runs leftward the line is read from the other end (a
  river drawn east to west is labelled upright). Above: the baseline a quarter em above the
  line; on: centered across it; below: the capitals' tops a quarter em below; more lines
  stack away from it. While it applies, the text's own box and transform are ignored:
  `Element::along` holds the glyphs' outlines, their turned boxes (`quads`, what hit testing
  reads) and a hash folded into the fingerprint, so the raster cache redraws the text when
  its path moves or is reshaped; the element's `world` is the identity and its boxes are
  the glyphs' in board units. A text box's fill is not drawn on a path.
- `BoardTxn::put_on_path(text, line)` (refused with `along::NEEDS_TEXT_AND_PATH` or
  `along::NOT_A_LINE`, writing nothing) and `release_from_path(text)` (clears the three
  `along` keys).
- **SVG export**: glyphs as paths, as all board text already was (no `<text>`, no
  `<textPath>`): the curved glyphs are exactly what the raster draws, and no user text goes
  into the file; the halo is a round-joined stroke of the same outlines before the fill.

### UI (desktop and browser, the same `pimble-app` code)

The text picker in the context bar (the size button, now showing size and family, in bold
or italic when they are on) opens rows instead of the four sizes alone: **Size** (S, M, L,
XL), **Font** (Sans, Serif, Condensed, then Bold and Italic toggles), **Spacing** (Tight
-0.05, Normal, Wide 0.1, Wider 0.25, Widest 0.5 em), **Halo** (Off, S, M, L: 0.06, 0.12,
0.2 of each selected element's font size, written in board units; and white, black and
paper swatches, a swatch turning the halo on), and, when the selection is a text on a path,
**Path** (Above, On, Below, and a slider for the place along it). The bar itself grows only
by the wider face of that one button. New text, sticky notes and shapes take the bar's
font, spacing and halo; only values that are not the default are written. The selection's
"more" menu has **Put Text on Path** (one text and one path, line or stroke selected; the
text stays selected) and **Release from Path**; each is one undo step, refusals said.

### MCP

`add_elements` and `update_elements` take `font` (`"serif bold italic"` or the parts),
`bold`, `italic`, `halo_color`, `halo_width`, `tracking`, `along` (an id or a ref of the same
call; `""` releases), `along_offset`, `along_side` in `style`, and the same names in
`clear_style`; `get_canvas` and `get_elements` show them. docs/MCP_CONTRACT.md, "Wave 6,
map lettering".

### Tests

`crates/pimble-board/src/text.rs`: every face parses and draws, bold wider and condensed
narrower, an unknown name sans, Greek in serif from Noto Sans, tracking's widths and its
effect on wrapping. `crates/pimble-board/tests/lettering.rs`: font keys round-trip and a
`font` written by a newer app (`"fraktur"`) draws pixel for pixel as Noto Sans Regular;
halo pixels around dark text on a dark ground with the fill still on top, bounds grown,
the color-only default, shape and sticky text haloed; tracking's width (nine gaps of half
an em), bounds and a hit on the tracked-out end; text along an arching path (glyphs rising,
level, falling, centered, hit on glyphs and not between), the path moved and reshaped
moving them (fingerprint changed); offset, side and a right-to-left line read upright; a
pen stroke in a moved group; nothing, itself, another text, a shape, two texts naming each
other and a deleted path all reading as plain text, the refusals' sentences; release back
to its box; SVG with no text nodes, the halo stroke before the fill; the raster cache equal
to a full render after the path moves; two people's font and bold-plus-halo edits merging.
`pimble-mcp`: lettering in through `add_elements` with `along` naming a ref, read back by
`get_canvas`, written back verbatim from `get_elements` (along resolved to the copy),
changed and released through `update_elements`, refusals. `pimble-app` (`boards.rs`): Put
Text on Path refused with one selected and done with two, the options writing font, bold,
tracking, side and a per-size halo, a duplicate of both following its copied line, Release
from Path keeping the lettering, one undo putting it back.

### Deviations

- `along_offset` is a fraction of the line's length only (not a distance), and the text's
  alignment chooses which part of it sits there.
- A third side, **on** (centered on the line), beside above and below: a road's name in
  the road.
- Text always reads left to right on a path (the line is read backwards where it runs
  leftward), so `along_side` is judged in the reading direction.
- Export draws the glyphs as paths, never `<textPath>` (faithful, and the SVG export already
  writes no text).
- Halo widths in the bar are steps relative to the font size, stored in board units, as the
  contract asked.

### Not built

- Dragging a text on a path along it (the slider in the text options moves it; a drag,
  resize or turn of the text alone writes its own transform, which is ignored until it is
  released from the path). Editing its words opens the editor over the curved text's box,
  straight.
- Kerning and shaping (ligatures, marks placed by GPOS): the layout stays per character.
- SVG import reading `font-family`, `font-weight`, `font-style` and `letter-spacing`, and
  `<textPath>`.
- Fonts installed on the computer (decided against: the same picture on every device).

## Built: symbols and scatter

Built 2026-11-18 on `master` (part 4 of the order in section 6), in `crates/pimble-board`
(the new `scatter.rs`; `doc.rs`, `snapshot.rs`, `render.rs`, `hit.rs`, `svg.rs`, `view.rs`,
`clip.rs`), the canvas (`crates/pimble-app/src/boards/stamp.rs`, the rail, the context bar
and the selection's menu; both apps) and `pimble-mcp`.

### Stored keys

- **A symbol** is one key of the board map, `symbol:<id>`: a `Map { name, created_at,
  author?, elements: Map<element id, element Map> }`, the key and its `elements` map made
  together in one transaction (never a container made on first use). Its elements have
  the shape of the board's own (kind, geom, style, transform, text, parent, note, fields)
  without a `layer`; their coordinates are the symbol's own, (0, 0) being where a
  placement puts it. Nothing draws them by themselves. An older app ignores the key.
- **A scatter** is an element of `kind: "scatter"` with two new keys of its map: `symbol`
  (the symbol's id, last writer wins) and its `geom`, an `Array` of `Buffer` chunks, as a
  stroke's points are. A placement is 16 bytes, little-endian: `x`, `y` (`f32`, the
  scatter's own space), the scale (`u16`, 1/1024 steps up to 64), the rotation (`u16`,
  1/65,536 of a turn), a tint (four bytes: red, green, blue and how far the symbol's
  colors move toward it, 0 none). Chunks are at most 256 placements when many are written
  at once (a region fill sorts its placements into 512-unit cells first, so each chunk
  covers a small area); a stamp stroke appends a chunk per append. A scatter takes at most
  100,000 placements. An older app reads the kind as one it does not know and draws
  nothing.
- **A tint over a whole scatter**: the style key `tint` (`#rrggbbaa`, the alpha the
  amount), applied before each placement's own.

**25,000 placements**: a region fill of an 11,800 × 11,000 polygon at a 62-unit jittered
grid made 25,320 placements, **405,996 bytes** of document (16.0 per placement; the
25,000 single-dab strokes of the map session were 14.8 MB). Measured in
`twenty_five_thousand_placements_are_one_small_element` (release build, the machine
shared): the fill 4.5 to 6.6 ms, a snapshot of the canvas 1.4 to 2.1 ms, a render fitted to
the whole map at 1,920 × 1,080 26 to 35 ms cold and 22 to 35 ms warm, a 1:1 view of a part
16 to 24 ms, sprites held 91 KB (before the far-off dots below: 95 and 85 ms fitted, 31 ms
1:1). In the test app, a canvas of 28,968 placements (a 24,909 one tinted, 457 KB of
document): each zoom step redrawn in 50 to 140 ms, the app at 340 MB resident.

### How it draws (`pimble_board::scatter`)

- Each symbol is resolved as a board of its own, in one layer named after it, before the
  board's elements (`SymbolDef`: its elements resolved, what they paint, a hash of their
  fingerprints, the pictures it shows). A symbol never holds a scatter (refused; one
  written anyway is left out), so this never recurses.
- A scatter's extent is its placements' boxes (the symbol's bounds through each
  placement), kept per chunk (`ResolvedScatter.chunks`). Drawing visits only the chunks,
  and in them the placements, whose boxes meet the region drawn.
- A placement smaller than 3 pixels across (`SPLAT_PX`) is a **dot**: the symbol's mean
  color over its bounds (worked out once per symbol from a 32 × 32 drawing, tinted as the
  sprite would be) spread over the pixels its box covers, each taking its share of the
  area. A forest seen from far off (and the navigator, and a layer's thumbnail) is a few
  pixel writes per tree instead of a draw: before it, the test app took 2 to 4 s to redraw
  a zoom step of the 29,000-placement canvas above (the navigator and thumbnails draw the
  whole canvas), 50 to 140 ms after.
- Each larger placement draws the symbol from a **sprite**: the symbol drawn at a half-octave
  size step at least as fine as the placement shows, with its tints, kept in one cache
  for the process with a 64 MB budget (least recently used go), keyed by the symbol's
  hash, the step and the quantized tints (5 bits a channel, 4 for the amount), and drawn
  through the placement's transform, bilinear. A placement larger than 384 pixels on the
  screen is drawn as vectors (through a buffer when tinted or faded). A symbol with a
  picture not loaded yet is drawn but its sprite is not kept.
- **Tint**: every color the symbol paints moves toward the tint's color by its amount,
  each pixel keeping its coverage (the scatter's tint first, then the placement's).
- The scatter is an element of its layer: clips, layer opacity and blends, groups and the
  layer order apply as to any element. The element's own transform moves, turns and
  scales the whole scatter (its pivot is its extent, as a stroke's is its points').
- **The raster cache**: a scatter's fingerprint folds in its symbol's hash, its tint and
  every placement, so editing the symbol redraws every scatter of it. Placements only
  appended (the scatter's look unchanged and more placements) dirty only where the new
  ones are (`ResolvedScatter::look`, `tail_extent`), so stamping into a forest does not
  redraw the forest.
- **Hit testing**: a point on any placement's box hits the scatter (one element: it is
  selected, moved and deleted whole); its bounds are all its placements'.
- **SVG export**: each symbol a `<symbol id="pimble-symbol-<id>" overflow="visible">`
  (one per tint pair it is drawn with, its colors moved toward them as the renderer
  does), written at its first use, and a `<use>` per placement that reaches the area,
  through the scatter's and the placement's transforms.

### Editing

- `BoardTxn::make_symbol(ids, name, keep)`: copies of the elements (with their groups'
  and frames' members, new ids, parents and `along` remapped) become the definition,
  centered on what the top-level ones cover; unless `keep`, the elements are deleted and
  one scatter with one placement at that center takes the topmost one's place in its
  layer, so the canvas looks the same. One transaction, one undo. Refused, writing
  nothing: no elements, a locked one (unless `keep`), a scatter, connector, node card or
  spotlight among them.
- The symbol's elements are edited by the ordinary edits (`set_style`, `set_geom`,
  `set_transform`, `set_text`, `delete_element(s)`): `BoardTxn` finds an element in a
  symbol when the board does not hold it, and judges only its own lock (a symbol is in no
  layer). `add_element` with the layer `symbol:<id>` adds to the symbol.
- `ViewOptions::editing_symbol`: a snapshot that shows the symbol's elements alone, in its
  space, in one layer whose id is `symbol:<id>` (so a tool picking "the layer new elements
  go into" draws into the symbol); such a snapshot is always read again whole.
- `create_symbol`, `rename_symbol`, `delete_symbol` (its scatters draw nothing; undo
  brings it back), `adopt_symbol` (a copy between canvases), `add_scatter`,
  `append_placements`, `set_scatter_symbol`, `scatter_in_shape(shape, symbol, fill,
  layer)`, `scatter_in_region(clip, ...)`; `NewElement::with_scatter` copies one
  (duplicate, copy between canvases).
- **Making placements**: `Jitter` (scale, its jitter, rotation jitter, a tint and its
  jitter: lighter and darker without a tint, around the tint's color with one),
  `StampWalker` (a placement at the first point, then one every `spacing` along the path,
  strayed up to `scatter`; placement `n` varied by `Rng::at(seed, n)`, so a path replayed
  in any pieces places the same), `fill_region(clip, RegionFill)` (Poisson-disk,
  Bridson's, seeded at a coarse grid so every piece of a shape fills; or a jittered grid;
  centers inside the outline by its fill rule), `scatter::seed_of(id)`.

### UI (desktop and browser, the same `pimble-app` code)

- **The Stamp**, Shift+S, on the rail in the Draw group after the Pencil (the rubber stamp
  icon). The letters are now: V Select, A Direct select, H Hand, P Pen, N Pencil,
  **Shift+S Stamp**, U Rectangle, O Ellipse, D Diamond, Y Polygon, K Star, W Arrow,
  Shift+W Line, B Brush, Shift+B Highlighter, E Eraser, G Fill, R Smudge, Shift+R Blur,
  M and L the paint selections, I Eyedropper, T Text, S Sticky note, C Connector, F Frame,
  J Numbered step, Q Discussion pin (X swaps the well's colors; Z is still free). A drag
  places the chosen symbol along the pointer into a new scatter in the layer new elements
  go into, or into the one selected scatter of that symbol; the scatter is selected on
  release. Placements go into the document at once and out about every 100 ms; the
  undo clock is held from press to release (one stroke, one undo step); a cancelled
  stroke's new scatter goes.
- **The context bar** (the Stamp's, and a selected scatter's): the symbol picker (a
  thumbnail and the name; its popup lists the canvas's symbols with thumbnails; picking
  one sets the Stamp's and changes the selected scatters' symbol) and the settings button
  ("Every 1.0× · ±20%"), whose popup has sliders: spacing and scatter in symbol sizes,
  size, size jitter, rotation jitter (degrees either way), tint (toward the well's stroke
  color) and tint jitter. The bar grows by those two buttons.
- **The selection's ··· menu**: Make Symbol (the selection replaced by one placement; the
  Stamp takes the new symbol; a sentence says so), Edit Symbol (one scatter selected),
  Scatter in Shape (one closed shape selected: filled with the Stamp's symbol at its
  spacing and jitter, Poisson-disk, seeded from the shape's id, just above it).
- **Editing a symbol** is an isolated view: the pane shows the symbol alone, fitted, with
  "Symbol "Tree"" and Done at the left of the context bar; every tool draws and edits in
  it, and every placement changes (in this pane when Done is pressed, live in any other
  pane showing the canvas). Done, or Escape with nothing selected, goes back to the camera
  the canvas was left at. The Stamp, Make Symbol and Scatter in Shape say to finish first.

### MCP

`add_symbol` and `scatter`; `get_canvas` lists symbols and scatters compactly,
`get_elements` gives a scatter's symbol, count and bounds; `update_elements` changes a
symbol's elements by id, a scatter's `symbol` and `tint`; copies bring the symbol along.
docs/MCP_CONTRACT.md, "Wave 6, symbols and scatter".

### Tests

`crates/pimble-board/src/scatter.rs`: a placement is 16 bytes and reads back; stamps are
the same whatever pieces the pointer path comes in, and differ for another seed.
`crates/pimble-board/tests/scatter.rs`: Make Symbol replacing the selection with one
placement drawn where it was, the stored keys, `keep`; 1,000 placements under 17.5 KB, the
chunks, appending; instances drawn scaled, turned and tinted, nothing between them,
nothing for a view far away, the scatter's own tint; editing the symbol's element changing
every instance (and the fingerprint), the isolated view and drawing into the symbol
through its layer, deleting from it, a scatter refused in a symbol, a deleted symbol
drawing nothing; hit testing on placements, not between; Poisson and grid fills inside an
ellipse, at least the spacing apart (Poisson), just above the shape, the same for the same
seed, an open line and a tiny shape refused; a clip and a blend applying; SVG with a
`<symbol>` per tint and a `<use>` per placement shown; the raster cache equal to a full
render after placements are appended; two devices appending and recoloring the symbol at
once reading the same; a placement far off drawn as a few greenish pixels; the
25,000-placement forest (size and timings above). `pimble-mcp`
(`symbols_and_scatters_round_trip`, against a real server): a symbol from specs, a
polygon filled at a density by the symbol's name, a list appended into the scatter,
get_canvas and get_elements, the crown recolored by id, a scatter's tint and symbol
changed, a symbol made from an element on the canvas, the SVG, the refusals.
`pimble-app` (`make_a_symbol_stamp_it_edit_it_and_fill_a_shape`): nothing to stamp said,
Make Symbol, a drag stamping about eleven placements sent together, a second drag
appending to the selected scatter and one undo taking only it back, Edit Symbol showing
two elements in one layer, a recolor reaching the symbol, Done back on the canvas, Scatter
in Shape filling a rectangle with every placement inside; `stamp.rs`: Shift+S, beside the
Pencil.

### Checked in the test app

Desktop, scratch store and data (`XDG_*` in the scratchpad), its own port (7483), a fresh
canvas "Forest": a tree drawn with the Ellipse and Rectangle tools and colored from the
well (`w6s_1_tree.png`), both selected (`w6s_2_tree_selected.png`), the ··· menu with Make
Symbol, Edit Symbol and Scatter in Shape (`w6s_3_more_menu.png`), Make Symbol replacing
them with one placement, the context bar showing the symbol's thumbnail and name and the
sentence in the status line (`w6s_4_made_symbol.png`); a 7-sided polygon 16,000 units
across (added through the MCP) selected and Scatter in Shape filling it with 4,040 trees in
0.7 s, Poisson-disk, lighter and darker, sized and turned (`w6s_5_polygon_selected.png`,
`w6s_6_forest.png`, `w6s_7_forest_closer.png`); the Stamp by Shift+S
(`w6s_8_stamp_tool.png`) dragged along an arc in empty space, 18 trees in one scatter,
selected on release (`w6s_9_stamped_row.png`); the symbol picker with its thumbnail
(`w6s_10_symbol_picker.png`); Edit Symbol showing the tree alone, fitted, with "Symbol
"Symbol 1"" and Done (`w6s_11_editing_symbol.png`, `w6s_22_editing_symbol.png`), the crown
selected and filled orange there (`w6s_12_symbol_recolored.png`), Done back at the camera
the canvas was left at with the row and the forest orange (`w6s_13_done_row_orange.png`),
zoomed out over all of it (`w6s_14_zoomed_out.png`, `w6s_15_zoomed_far_out.png`); the
MCP's render of the canvas (`w6s_16_mcp_render.png`); 24,909 more tinted trees placed by
the MCP's `scatter` (a grid over a `d` square, 204 ms), fitted and zoomed in, each step
redrawn in 50 to 140 ms (`w6s_17_25k_fitted.png`, `w6s_18_25k_closer.png`,
`w6s_19_25k_close.png`); the Stamp's bar and its settings popup
(`w6s_20_stamp_bar.png`, `w6s_21_stamp_settings.png`); a star drawn into the symbol while it
was edited (three elements: `w6s_23_drawn_into_symbol.png`), Escape twice back to the
canvas (`w6s_24_forest_with_star.png`).

### Deviations

- **Make Symbol replaces** the selection with one placement (the contract allowed keeping
  it); the MCP's `add_symbol` has `keep`.
- **Edit Symbol is an isolated view** (the symbol alone in its own space), not editing in
  place among the canvas: every tool works there unchanged, and new elements go into the
  symbol through its layer's id.
- A scatter's **pivot is its extent**, so a scatter that is turned or scaled as a whole
  shifts a little when placements are appended outside it (as a turned stroke does when
  it grows). Moving it is exact.
- Tints are **quantized** when drawn (5 bits a channel, 4 for the amount), so placements
  share sprites; the stored tint is exact. The SVG uses the same quantized tints.
- The Stamp's spacing and scatter are in **symbol sizes** (1.0× is one symbol across at
  its size), so the same settings suit a 30-unit tree and a 300-unit house.
- The MCP's `density` maps to a Poisson spacing of √(0.68 × 10,000 / density) (Bridson's
  fill packs about 0.68 / r²), a grid's √(10,000 / density).

### Not built

- Editing a symbol in place among the canvas; renaming or deleting a symbol in the UI
  (the MCP and `BoardTxn` can; a symbol nobody places stays in the document).
- Symbols holding scatters (nested); scatters as clips.
- Erasing or moving single placements (the eraser deletes a scatter whole); a paint
  version of the stamp (a dab shape from a picture).
- Copying a scatter between stores does not copy pictures inside its symbol.
- A coarser level than one dot per placement at extreme zoom-outs (100,000 placements
  are still 100,000 dots).

## Built: soft edges

Built 2026-11-18 on `master` (part 5, the last, of the order in section 6), in
`crates/pimble-board` (the new `soft.rs`; `render.rs`, `snapshot.rs`, `layer_tree.rs`,
`raster.rs`, `svg.rs`, `doc.rs`, `model.rs`, `hit.rs`), the context bar and the layers panel
(`crates/pimble-app/src/boards/rail.rs`, `layers.rs`, `tools.rs`; both apps) and `pimble-mcp`.

### Stored keys

| Where | Key | Value | Absent |
| --- | --- | --- | --- |
| an element's `style` map | `feather` | board units | a hard edge |
| a layer's or group's map | `blur` | board units | none |
| a layer's or group's map | `feather` | board units | none |

Each its own last-writer-wins number. Read as 0 when negative or not a number, at most
`soft::MAX_RADIUS` (10,000). An older app ignores them and draws hard edges. The element's
key lives in its style map (with the halo and the rest), so `set_style`/`clear_style`
(`StyleKey::Feather`), duplicate and every copy carry it; `copy_from` (Convert to Node) and the
MCP's `copy_layer` carry the layer's two.

**The radius**: sigma is half the radius, so an edge fades over about the radius on either
side, and nothing reaches more than three sigma (1.5 times the radius, `soft::reach`) past
the hard edge.

### How it draws (`pimble_board::soft`)

- **The blur**: three box passes each way (Kutskir's box sizes for a Gaussian), sliding sums
  on premultiplied RGBA, transparency outside, the cost per pixel independent of the radius.
- **Density**: a sigma up to `WORK_SIGMA_PX` (6) pixels on the target is blurred at the
  target's density, over the target grown by three sigma. A wider one is worked at the
  power-of-two density where sigma is 3 to 6 pixels and laid onto the target bilinear.
  Worked that way, what is blurred (an element, or what a layer holds) is drawn and blurred
  **whole** once and kept in a process cache (`soft::BUDGET` 160 MB, least recently used
  out; one entry at most 2,048 × 2,048), keyed by what it draws (an element's fingerprint, a
  layer's hash of every element fingerprint and paint operation inside it, and whether it
  is drawn clipped) and the density. Panning and zooming over a large feather or a hazy
  layer reuse it; an entry too large, or something showing a picture still loading, is
  worked for the area drawn only and not kept.
- **Feather keeps the inside**: per pixel, with `M` the sharp alpha and `Mb` the blurred
  one, `out = (1 - M) · blurred + Mb · sharp`, alpha `Mb` (premultiplied). A flat color comes
  out as its plain blur; a picture or text inside keeps its detail and only its rim fades.
- **An element's feather** (`render::draw_element`): drawn hard into a buffer the size of
  its pixels on the target, its blur beside it, combined, laid over. Feathers on elements
  inside a symbol work in its sprites (which are upright); drawn turned (a placement past
  384 pixels, drawn as vectors through a rotation) it is drawn hard.
- **A layer's blur and feather**: a layer or group with either is isolated (it gets a
  buffer). When its buffer is closed the blur replaces it with what the layer holds drawn
  inside it (`render::draw_layer_inside`: its children's buffers, blends, clips and soft
  edges; not its own) and blurred; the feather then works on that, its mask the edge of the
  blurred drawing (two Gaussians, sigma √(b² + f²)). Then the opacity, blend and **clip**
  apply as before: a clip cuts after the blur, and a feathered element in a clipped layer
  is clipped.
- **Where it reaches**: an element's `bounds` grow by its own feather's reach plus its layer
  and groups' (`Element::soft`), and a paint layer's by its chain's
  (`BoardSnapshot::paint_reach`, `soft_reach`), so the spatial index, the visible extent
  (fit, export, thumbnails, the navigator, `render_canvas`), the raster cache's dirty areas
  and Flatten's area see the spread. `Element::hard_bounds` is the box without it: a marquee
  must hold that, and the app's selection box, handles, snapping, alignment, click-inside and
  the outlines others see use it. Hit testing stays on the hard geometry.
- **The raster cache**: the element feather is in its fingerprint; a layer's blur and feather
  are in its composite hash, so changing one redraws every element and paint mark in it,
  over their grown bounds.
- **SVG export**: a feathered element is a `<g filter>`; a blurred or feathered layer's `<g>`
  carries the filter with its clip (SVG applies the filter before clipping and opacity, as
  the renderer does). The filter, in board units (`filterUnits="userSpaceOnUse"`, sRGB):
  `feGaussianBlur` for a blur, and for a feather the same sum as the renderer's: blurred,
  `core = input in blurred`, `rim = blurred out input`, `core + rim` (`feComposite`
  arithmetic).
- **Flatten and merges bake them**: Merge Down draws the merged layer's elements' feathers
  and its own blur and feather into the pixels (the layer goes); Merge Group its children's
  (the group's own keys stay on the layer it becomes, like its clip); **Flatten** of a paint
  layer draws its own blur and feather into the pixels and, in the same edit, takes the two
  keys off the layer (unless someone changed them meanwhile), so the picture looks the same.
  `LayerRaster.baked` says what was drawn in.

### UI (desktop and browser, the same `pimble-app` code)

- **Context bar**: the opacity button became "Opacity and feather"; its popup has the
  opacity slider and, for a selection of filled kinds (shapes, paths, text, callouts, sticky
  notes, scatters; not lines, arrows, pens), a **Feather** slider with the radius beside it
  ("Off", "130"). The slider is logarithmic from 1 to 2,000 units (`rail::slider_to_soft`;
  the left end is off). The button shows "100% ◌" while the selection is feathered, so the
  bar keeps its width. New elements never take a feather.
- **Layers panel**: under the picked layer's blend and opacity, **Blur** and **Feather**
  rows (slider, radius), the same scale.
- **Undo**: a slider drag is one undo step (`boards::slider_dragging` holds the undo clock at
  the drag's first change while changes keep coming within 1.5 s); undo also reloads the
  bar from the selection (the opacity and feather it shows had stayed at the undone value).

### MCP

`feather` in `style` on `add_elements` and `update_elements` (`clear_style` `"feather"`);
`blur` and `feather` on `add_layer` and `update_layer` (0 removes); `get_canvas` says
`feather 60` in an element's style and `blur 40, feather 12.5` in a layer's flags;
`get_elements` gives `feather`; `export_canvas` writes the filters. docs/MCP_CONTRACT.md,
"Wave 6, soft edges".

### Tests

`crates/pimble-board/src/soft.rs`: the three boxes' spread matches sigma; blurred and
feathered pixels stay valid premultiplied; the work density and the radius read.
`crates/pimble-board/tests/soft_edges.rs`: the stored key; a feathered square half covered at
its edge, fading out and in, opaque in the middle, its color in the rim, bounds grown by 1.5
radii, hard bounds for a marquee, the index and extent reaching; cleared back to hard; the
same picture at 1 and 4 pixels per unit and a part drawn alone equal to the whole, and the
lower-density blur kept; a feathered layer keeping a thin line inside sharp; a blurred layer
softening it; a clip cutting the blur's spread and a feathered element inside it; a paint
layer blurred, its reach grown; the raster cache equal to a full render after a move and after
the layer's feather changes; SVG filters; Merge Down's pixels soft at the edge and Flatten
baking a blur and taking the key off, looking the same; refusals and clamping; a copy keeping
the keys; two devices' feather, blur and fill merging into the same pixels; a map-sized canvas
(below). `pimble-mcp` (`soft_edges_round_trip`, against a real server): a layer made with blur
and feather, an element with a feather, read back by `get_canvas` and `get_elements`, the SVG
filters, changed, cleared. `pimble-app` (`feather_a_shape_and_blur_a_layer`): the feather on a
selection of an ellipse and a line (only the ellipse takes it), the bar showing it when
selected, 0 taking it off, undo putting it back with the bar following, the layer's blur and
feather through the panel's edit read by its row, each its own step; `rail.rs`: the slider's
scale and which kinds offer a feather.

**Measured** (release build, the machine shared): `a_big_feather_on_a_map_sized_canvas_renders_fast`,
an 11,800 × 11,000 frame, twelve 2,400 × 3,000 regions feathered 400 in a layer feathered 150,
and a haze layer blurred 600, at 1,920 × 1,080: fitted 121 ms cold, 75 ms warm; at 2 pixels per
unit (sigma 400 pixels on the screen) 140 ms cold, 139 ms warm; 2.6 MB of blurs kept.

### Checked in the test app

Desktop, scratch store and data, its own port (7483), a canvas "Soft" made through the MCP: an
11,800 × 11,000 paint layer "Ground" (a picture laid across it and a blue brush stroke),
"Regions" (a polygon feathered 400, a quadrilateral 120, an ellipse 1,500), "Haze" (two white
ellipses, layer blur 600), "Fields" (a purple rectangle feathered 500, the layer blurred 60 and
clipped to a hidden ellipse): the MCP's render (`w6f_0_mcp_render.png`), the canvas opened
(`w6f_1_opened.png`), zoomed in (`w6f_2_zoomed_in.png`), the clipped feather with its hard clip
edge (`w6f_4_clipped_feather.png`, `w6f_5_clipped_feather_close.png`), inside the 1,500 feather
at 114% (`w6f_6_big_feather_edge.png`), fitted (`w6f_7_fitted.png`), the bar's popup with the
Feather slider (`w6f_8_feather_popup.png`), raised to 1,718 (`w6f_9_feather_raised.png`), the
selection box on the hard edge (`w6f_11_selected_hard_box.png`), a drag from 130 to 1,718 and
one undo back to 130 with the bar following (`w6f_15a_before_drag.png`,
`w6f_15_feather_dragged.png`, `w6f_16_one_undo.png`), the Regions layer feathered from the
panel (`w6f_17_layer_feather.png`), the Haze layer's blur 600 in the panel
(`w6f_18_haze_picked.png`), the light theme without and with the ground (`w6f_19_light.png`,
`w6f_20_light_with_ground.png`), the SVG export of Regions drawn by librsvg
(`w6f_21_svg_export.png`).

Zoom steps with the soft layers shown and the ground hidden: 125 to 130 ms from the click to
the second frame after it (two idle frames take 84 ms here), so about 45 ms of drawing per
step, fitted or at 114% inside the large feather. App resident 650 to 710 MB.

### Deviations

- The element's `feather` is in its **style map**, not on the element's map, so it is a style
  edit (merges, clears, copies and undoes like the rest).
- **Flatten takes the layer's blur and feather off** after drawing them in (a clip stays a key
  on Flatten, part 1); without that the flattened layer would be blurred twice.
- **Feather is centered on the edge** (half the fade outside the shape), not inward only; the
  inside keeps its detail.
- **The radius is twice sigma** (the fade spans about the radius on each side), not sigma as
  in CSS `blur()`; SVG's `stdDeviation` is half the stored radius.
- An element drawn into its own buffer to be feathered loses a highlighter's multiply
  against what is below it (it is laid over).
- Very wide feathers seen close (sigma hundreds of pixels on the screen) can show faint
  steps: an 8-bit gradient stretched over that many pixels, from the lower-density blur.
- librsvg (and browsers) cut a filter at its region, so the SVG's faintest rim, at three
  sigma, may show a hairline where the renderer's fades out.

### Not built

- An inward-only feather, and feathering a picture element (the bar offers feather to kinds
  with a fill; the MCP writes it on any element, and every kind draws it).
- Drop shadows with blur (a shadow is still the offset fill), glow, and other filters.
- A blur that follows a path (motion blur), and per-element blur of the inside.
- A paint layer's own tiles kept blurred: a blurred paint layer is blurred from its tiles as
  drawn, so a view far out over a huge blurred paint layer pays the existing replay cost
  ("Large canvases", "Not done") once per density and edit.

### Seen, not fixed

- A paint layer holding a picture laid across the 11,800-unit canvas makes every zoom step 6
  to 9 s, with or without soft edges (perf: PNG decode and `render::halve` of the picture per
  step, the tiles replayed at full density): the window's picture cache lets the decoded
  picture go after 4 s, and a step takes longer than that. `export_canvas` of that canvas took
  62 s and `render_canvas` 6.5 s for the same reason.
