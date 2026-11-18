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
