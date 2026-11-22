# Measure contract: a scale for a canvas, and real measurements while drawing

Status: **written 2026-11-22 from Joe's request and decisions; not built.**

Joe: "the canvas [should have] tools for unit-based measuring and drawing ... you set a
'scale' for the drawing, then use the drawing tools as normal, but now they have
measurement data ... For maps, diagrams, plans." And: "there probably needs to be a
more-precise way of setting scale than drawing a line."

Decisions (Joe, 2026-11-22):

1. **One scale per canvas.** (Per-frame scales may come later without breaking this:
   a frame would carry its own `scale` key with the same shape.)
2. **Both**: a Measure tool that draws dimension lines, and a per-shape "Show dimensions"
   switch that labels a shape's sides and area.
3. **Both unit systems** from the start, chosen per canvas: metric (mm, cm, m, km) and
   imperial (in, ft, yd, mi), with feet and inches written `12′ 6″`.
4. **Measuring is a switch, off on every canvas until turned on** ("it might be good to be
   able to disable/enable scale-mode, artists won't want it"). Off keeps the scale (stored
   `off: true` in the `scale` map, absent is on) and nothing measures: no labels, readouts,
   unit grid, Measure tool, or dimension and area switches. The canvas toolbar's Measure
   group is then one unlit ruler, folding into More first; clicking it turns measuring on
   (opening the Scale panel when the canvas never had a scale), and lit it turns it off.
   Applying a scale, here or through the MCP, turns it on. `BoardSnapshot::measuring()` is
   the scale in effect; `scale` is what is stored. The MCP's `set_canvas` takes
   `measuring`.
5. **Labels follow the theme on screen** (`ViewOptions::dark`): labels and dimensions in
   the default color read light over a dark halo on the dark theme; exports and the MCP's
   renders keep dark on white. A color the element names stays.

Builds on docs/IMAGE_MARKUP_CONTRACT.md, docs/CANVAS_CONTRACT.md, docs/DRAWING_CONTRACT.md,
docs/TOOLBAR_CONTRACT.md. Words a person reads say "canvas"; code and storage say "board".
American spelling. The web and the desktop are the same `pimble-app` code.

## 1. The scale (stored)

A new key on the board map, `scale`, an `Any::Map`, written whole (like `page`):

| Key | Value |
| --- | --- |
| `unit` | `"mm" | "cm" | "m" | "km" | "in" | "ft" | "yd" | "mi"`: the unit measurements are shown in |
| `per_unit` | f64 > 0: board units in one `unit` (the one number everything is computed from) |
| `label` | optional string: how the person wrote it (`"1/4\" = 1'"`, `"1:100"`), shown back in the dialog and the toolbar; never parsed again |
| `precision` | optional: metric, decimals (0-3, default chosen per unit: mm 0, cm 1, m 2, km 3); imperial, the smallest inch fraction as a denominator (1, 2, 4, 8, 16; default 1 for `ft`/`yd`/`mi`-scale work shown as feet-inches, 16 for `in`) |

Absent `scale` means no scale: nothing shows measurements, and the Measure tool asks for a
scale first. Removing the scale is `board.remove("scale")`. A change of scale re-labels
everything at once, because no measurement is ever stored as text: labels are computed
from geometry at render time.

**Paper.** A board unit at 100% zoom is one CSS pixel, 1/96 inch, which is also what export
at 1x means. So a ratio `1:N` means one board unit is `N/96` inches in the world, and
`a = b` (`1 in = 10 ft`) means `96 * a_in` board units are `b`. Both reduce to `per_unit`.

`pimble_board::measure` (new, pure, the one place for units): `Unit`, `Scale` (read and
write through `BoardTxn::set_scale(Option<&Scale>)` and `RawBoard`/`BoardSnapshot`),
`parse_length("12'6\"" | "12 ft 6 in" | "3.5 m" | "3/8\"" | "150", default unit)`,
`format_length`, `format_area` (`120 sq ft`, `11.15 m²`; large areas in acres / hectares
optional later), `parse_scale("1:100" | "1 in = 10 ft" | "1/4\" = 1'" | "1 cm = 2.5 km")`,
presets, and an element's measurements (below). Thoroughly unit-tested, round trips
included.

## 2. Setting the scale precisely (the Scale dialog)

"Scale..." in the canvas toolbar group (and the canvas's menu) opens one dialog with a unit
picker, a precision picker, and four ways in; each fills in the same preview line ("1 in on
paper = 10 ft; 1 ft = 9.6 px at 100%") before Apply:

1. **Presets**: architectural (1/16″, 1/8″, 1/4″, 1/2″, 1″ = 1′), engineering (1″ = 10′, 20′,
   50′, 100′), metric (1:1, 1:5, 1:10, 1:20, 1:50, 1:100, 1:200, 1:500, 1:1000), map
   (1:10,000, 1:24,000, 1:25,000, 1:50,000, 1:100,000).
2. **Typed**: a ratio or an equation in a text box, parsed by `parse_scale`, with a
   sentence when it cannot be read.
3. **From a selected element**: when one line, arrow, dimension, rectangle-like shape or
   picture is selected, "This is ___ long" (a line's length; a box's width, with a
   width/height choice). Exact: nothing is drawn freehand.
4. **Calibrate by drawing**: a temporary two-point line drawn on the canvas whose ends snap
   to element points (path anchors, box corners and edge midpoints, line ends) and to the
   grid when snapping is on; its ends stay draggable at any zoom until Apply; the dialog
   shows the line's length in board units and takes the real length typed.

Plus, for scanned plans: **picture at scale.** With a picture selected, "Size to scale"
takes the scan's DPI (default from the file when known, else typed) and the scale it was
drawn at (a ratio or equation), and resizes the picture so its scale matches the canvas's.
One edit, undoable.

Setting, changing or removing the scale is one undoable edit.

## 3. Measurements of an element

`measure::of(element, snapshot) -> Measures` in board units, converted by the scale for
showing; computed after the element's transform (a rotated or scaled shape measures as
seen):

| Kind | Measures |
| --- | --- |
| line, arrow, connector (straight), dimension | length, angle (0° east, counter-clockwise, as drafting does) |
| rect, diamond, polygon, star, frame, picture, sticky | width, height, perimeter, area (the shape's own: an ellipse's is πab, a polygon's by shoelace) |
| ellipse | width, height (diameters), perimeter (Ramanujan), area |
| path | length when open; perimeter and area when closed (flattened; subpath areas by the fill rule) |
| pen, highlighter | length |

## 4. Live readouts while drawing and editing

With a scale set, while a line, shape or path is drawn, moved by a handle or resized, a
small label near the pointer shows the measures that change (`14′ 6″  32°`,
`12′ × 10′`). It is drawn in the view's overlay in screen pixels (not part of the board),
in both themes. The status bar's tool hint keeps its place.

**Typed sizes.** While drawing a line or a box (after the first press, before release, or
straight after it), typing digits opens a small entry at the pointer: a line takes a
length (and optionally `@ angle`), a box takes `w x h`; Enter applies it along the drawn
direction from the start point, Escape leaves the drawn size.

**The selection's group.** With a scale set and one element selected, the contextual
toolbar group (docs/TOOLBAR_CONTRACT.md) shows its measures; width, height and length are
editable (typed in units, parsed by `parse_length`; a box resizes from its top-left, a
line from its start), perimeter and area read only. These fold into More like any group.

## 5. Showing measurements on the canvas

**Dimension elements** (the Measure tool, key `M`): a new kind `dimension`,
`Geom::Segment { a, b }` (the measured points), plus style keys `dim_offset` (f32, board
units, signed: how far the dimension line sits from the measured points, perpendicular;
dragging the drawn line changes it) and the existing `font_size`, `stroke`, `width`.
Drawn as extension lines, a dimension line with ticks (architectural, imperial default) or
arrows (metric default; `arrow_heads` overrides), and the computed length centered on it,
kept upright. The element's `text`, when not empty, replaces the label (`{}` inside it is
the computed length: `"{} clear"`). Ends snap like calibration ends.
An older app keeps it as `Other` and draws nothing; acceptable.

**Show dimensions on a shape**: style key `dims` (bool): lines label their length, boxes
label width (above) and height (beside), polygons with straight sides label each side,
closed paths and ellipses label nothing per side. Style key `area` (bool): closed shapes
show their area at their center, under any text they hold. Both are switches in the
selection's group and apply to every selected element. Without a scale both draw
nothing.

Labels are drawn by the renderer (`render.rs`) and the SVG export (`svg.rs`) alike, in
board units (they scale with the canvas and export with it), default size 12, the stroke
color of the element (dimension) or the theme's text color, with a halo (the map-lettering
halo) so they read over pictures. Text sits outside a closed shape's edge.

## 6. Grid and snapping in units

With a scale set, the grid (still per viewer, still switched by the toolbar) is spaced in
"nice" real steps so lines are 16-60 screen pixels apart at the current zoom: metric 1, 2,
5 × 10ⁿ of the unit; imperial 1/16″, 1/8″, 1/4″, 1/2″, 1″, 3″, 6″, 1′, 2′, 5′, 10′, 25′,
50′, 100′, ¼ mi, ½ mi, 1 mi. Snapping lands on that grid. Without a scale the grid stays
what it is (`GRID`, 20). The status bar says the grid step when it is shown (`grid 1′`).

## 7. MCP

- `get_canvas` reports the scale (`unit`, `per_unit`, `label`) and, with one, each
  element's measures in units.
- `set_canvas` takes `scale` (a ratio or equation string, or `null` to remove) and `unit`.
- `add_elements`/`update_elements` accept any coordinate or size as a length string
  (`"12 ft"`, `"3.5 m"`) when the canvas has a scale, measured from the board origin; and
  `dims`, `area`, `dim_offset`, kind `dimension`.
- The tool descriptions say so, so an assistant can draw "a 12 × 14 ft room with a 3 ft
  door on the south wall".

## 8. Embedded canvases

A canvas in a note has its own `scale` like any board, and its readouts and labels work
the same while it is edited. Selected but not edited, it shows only Edit, Export and
History (docs/TOOLBAR_CONTRACT.md "Embedded items"): no Scale button there.

## 9. Order of building

1. `measure` module: units, parse/format, scale, element measures; tests.
2. Stored `scale`, the Scale dialog (all four ways and picture at scale), undo.
3. Dimension elements and the Measure tool; `dims`/`area` labels in render and SVG.
4. Live readouts, typed sizes, the selection group's measures.
5. Grid in units and snapping.
6. MCP.

## Not decided here

- Per-frame scales (decision 1 leaves room).
- Angles between two lines, radius/diameter callouts, chained dimensions.
- Acres and hectares for large areas.

## Built (2026-11-22)

All six steps of section 9, on `master` (uncommitted), in `crates/pimble-board`,
`crates/pimble-app` (desktop and browser alike, the same code) and `crates/pimble-mcp`.

### Stored keys

- Board map: `scale: Any { unit, per_unit, label?, precision? }`, written whole
  (`BoardTxn::set_scale(Option<&Scale>)`, removed with `None`; refused, writing nothing,
  when `per_unit` is not finite and positive). Read into `RawBoard::scale` and
  `BoardSnapshot::scale`; copied by `copy_from`. A stored unit this version does not know,
  or a `per_unit` that is not positive, reads as no scale.
- Element style map: `dims` (bool), `area` (bool), `dim_offset` (f64, board units,
  signed). `StyleKey::{Dims, Area, DimOffset}`; `ResolvedStyle::{dims, area, dim_offset}`.
- New kind `dimension` (`Kind::Dimension`), geometry `Geom::Segment { a, b }` (judged:
  a dimension takes only a segment). An older app reads it as `Other` and draws nothing.
- Nothing measured is stored: every label is computed when drawn.

### Model (`pimble_board::measure`, the one place for units)

- `Unit` (mm, cm, m, km, in, ft, yd, mi: `parse` takes symbols, words, either spelling of
  meter and primes; `meters`, `default_precision`, `precisions`, `precision_label`).
- `Scale { unit, per_unit, label, precision }`: `board_per_meter`, `to_meters`,
  `to_board`, `length` (`12′ 6″`, `3.50 m`), `area` (`120 sq ft`, `11.15 m²`),
  `parse_board`, `describe` (the panel's preview line: "1 in on paper = 10′; 1 ft = 9.6 px
  at 100%"), `name` (the person's label, else `1:N` metric or `1 in = …` imperial).
- `parse_length` / `parse_length_unit` (`12'6"`, `12′ 6″`, `12' 6`, `12'-6"`,
  `12 ft 6 in`, `3.5 m`, `3 m 20` (cm), `3/8"`, `1 1/2"`, `1-1/2 in`, `1,250`, bare numbers
  in the default unit), `format_length`, `format_area` (halves round away from zero,
  thousands grouped), `parse_scale` (`1:100`, `1 : 24,000`, `1 in = 10 ft`, `1/4" = 1'`,
  `1 cm = 2.5 km`; refusals are sentences), `PRESETS` (the contract's four groups),
  `scale_from_length`, `picture_units_per_pixel` (Size to Scale), `grid_step` /
  `grid_label` (section 6), `drafting_angle`.
- `measure::of(&Element) -> Measures { length, angle, width, height, perimeter, area }`
  in board units after the element's world transform (a rotated, scaled box measures as
  seen; ellipse by Ramanujan and πab; polygons, stars, diamonds and rounded rectangles by
  their flattened outline and the shoelace; closed paths per subpath by the fill rule,
  open paths and pen strokes by length; connectors by their route).
- Marks (`Element::marks`, built by the snapshot after elements are placed, in board
  units; their bounds join the element's and their hash its fingerprint, so a new scale
  relabels through the raster cache): a dimension's extension lines, dimension line
  `dim_offset` off (positive is left of a→b, above a line drawn east), ticks (imperial
  default, `arrow_heads: none`) or filled arrows (metric default, any other
  `arrow_heads`), and the length centered over it, kept upright, its `text` replacing the
  label with `{}` for the length; `dims` labels (a line's, stroke's or open path's length
  at its middle, a box's width above and height beside, outside the edge; each side of a
  diamond, polygon or star; nothing per side for ellipses and paths) and `area` (at the
  center, under any text). Size 12 board units for shape labels (a dimension's own
  `font_size`, default 12), halo white at 0.15 em. Drawn by `paint::element_ops`, so the
  renderer and the SVG export (glyphs as paths, as all board text) draw the same.
- A dimension is hit on its lines and its label.

### UI

- **Measure** group in the canvas toolbar (after Snap, folds with Snap's priority, dims
  read-only, only on an open canvas so an embedded canvas only selected has no Scale
  button): one ruler button, lit while the canvas has a scale, its tooltip the scale's
  name; it opens the **Scale panel**.
- **Scale panel** (`boards/measure.rs`), floating over the top of the canvas and not modal:
  five ways as word chips (Preset, Typed, From Selection, Calibrate, Picture), a unit
  picker and a precision picker (metric decimals or inch fractions), the preview line (or
  the sentence saying what is wrong), Remove Scale, Cancel, Apply. Calibrate: drag on the
  canvas (the panel takes the presses while that way is chosen); the line is an edit
  overlay, not an element, its ends snap to element points (line ends, box corners and edge
  midpoints, path anchors, stroke ends, connector ends) and the grid, and stay draggable
  until Apply; the panel shows its length in board units and takes the real length.
  Picture: dpi (default 300) and the scale it was drawn at; **Size to Scale** resizes the
  selected picture from its top-left (crop and the element's own scale allowed for), one
  undoable edit. Setting, changing and removing the scale are each one undo step.
- **Measure tool** (`Tool::Measure`, in the diagram flyout beside Connector, Step and
  Frame; Shift+M): without a scale it says so and opens the panel; with one it drags out a
  dimension whose ends snap to element points (then the grid), Shift for 45° steps.
- **Readouts**: with a scale, a small box near the pointer while a line, shape, dimension
  or connector is drawn, a single element moved, resized or turned, or a path's points
  dragged (`14′ 6″  32°`, `12′ × 10′`); a DOM box in logical pixels over the canvas,
  themed by the app's colors.
- **Typed sizes**: digits while a line, arrow, dimension or box is being dragged (or
  straight after, while it alone is selected) start an entry shown in the readout box; a
  line takes a length and optionally `@ angle` (drafting degrees), a box `w x h` (one
  number: a square); Enter applies it from the start point along the drawn direction,
  Escape keeps the drawn size, Backspace edits. Without a scale numbers are board units.
- **The selection's group** (with a scale): Show Dimensions (dimensions icon) and Show
  Area (square-dot icon), switches over every selected element that takes them (off
  clears the key); Measures (ruler icon, its tooltip listing every measure) opens a picker
  with Length / Width / Height to type (Enter applies: a line from its start, a box from
  its top-left, turned and scaled shapes kept in place through `repivot`), Offset for a
  dimension, and Angle, Perimeter, Area read only. A dimension's own controls: color,
  ends (none = ticks), opacity.
- **Grid in units**: with a scale the grid's step is `measure::grid_step` at the zoom in
  CSS pixels (16 px or more apart, nice real steps); snapping (shapes, moves, resizes,
  arrow-key nudges, Direct select, the Measure tool's ends) lands on it; the status bar
  adds `grid 1′` beside the tool hint while the grid shows.

### MCP

- `set_canvas` takes `scale` (ratio or equation; `null` removes), `unit` (alone: the
  scale shown in another unit) and `precision`, and answers with the scale's preview line.
- `get_canvas` says `scale: …` (or `scale: none`) and, with one, `· measures …` on every
  element (length and angle, width × height, perimeter, area, in the canvas's units).
- `add_elements` / `update_elements` take any `x`, `y`, `w`, `h`, point, `tip`,
  `from_point`, `to_point` as a number or a length string (`"12 ft"`, `"3.5 m"`,
  `"12' 6\""`; a numeric string is canvas units); a length on a canvas with no scale is
  refused with a sentence. Kind `dimension` (two points), style `dims`, `area`,
  `dim_offset` (and in `clear_style`); `get_elements` writes them back. The tool
  descriptions give the "12 x 14 ft room with a 3 ft door" recipe.

### Tests

- `pimble-board` `measure::tests` (12): units, lengths parsed (every form above and
  refusals), formatted, round trips for every unit at its default precision, areas,
  scales parsed (ratios, equations, refusals, every preset), `describe`/`name`,
  `scale_from_length`, picture to scale, nice grids, drafting angles, areas by fill rule.
- `pimble-board` `tests/measure.rs` (8): the scale stored, read back, copied, undone and
  redone, refused when not positive, two people's scales converging to one; elements
  measured after rotation and scale (line, rect, ellipse, diamond, pen, closed path); a
  dimension labeled in board units then `12′ 6″`, its fingerprint changing with the scale,
  its bounds, hit on its line, `{}` text; ticks for imperial, arrows for metric,
  `arrow_heads` none, a zero-length dimension rendered and exported without panicking;
  dims/area nothing without a scale, then `12′`, `10′`, `120 sq ft`, a label per hexagon
  side, the SVG export carrying more glyph paths and the halo, dims cleared; the raster
  relabeling on a new scale.
- `pimble-app` (`boards.rs`, 3; `boards/measure.rs`, 3; the toolbar's group test): the
  Measure tool refusing without a scale and opening the panel, then a dimension with both
  ends snapped to a box's corners labeled `10′`, its length typed from its start, the
  scale undone in one step; typed sizes after a line (`10'` east) and a box (`5' x 2'`),
  Escape keeping the drawn size; calibration by drawing (no element made, its length read,
  the panel's candidate), Show Dimensions and Show Area written and cleared, the grid in
  units; the panel's judgement of each way; typed geometry following the drawn direction;
  a scale shown in another unit.
- `pimble-mcp` `a_room_drawn_in_feet`: lengths refused without a scale; `1/4" = 1'` set;
  a room, a door and a dimension in feet; get_canvas's scale line and measures; a width
  changed in feet; the unit switched to inches; a metric ratio; an unreadable scale
  refused; the scale removed.

### Deviations

- The Measure tool is **Shift+M**: M is the rectangle paint selection's.
- Miles are shown as decimal miles (`0.35 mi`, precision = decimals), not feet and inches.
- Readouts are a DOM box over the canvas (logical pixels, themed), not drawn into the
  view's raster overlay; the effect is the one the contract asks for.
- Labels of `dims`/`area` are a fixed dark gray (`#333333`) over a white halo rather than
  the theme's text color: they are part of the canvas and export with it, so they cannot
  follow one viewer's theme.
- Without a scale a dimension still draws, labeled with its length in board units.
- The precision picker offers decimals 0–3 for metric and miles and 1/1–1/16 inch for
  inches, feet and yards.
- "Dragging the drawn line changes `dim_offset`" is an **Offset** field in the Measures
  picker instead (a drag of a dimension moves it like any element).

### Not built

- The scan's dpi read from the file (PNG `pHYs`, JPEG density): the panel defaults to 300.
- Acres and hectares; per-frame scales; angles between lines, radius callouts, chained
  dimensions (all "Not decided here").
- Snapping to element points for anything but the Measure tool and calibration.

### Not yet walked

The app was not run for this build: everything above is checked by the tests named and
by compiling the desktop (`--release`) and the browser (`web/`, wasm32) builds. The Scale
panel's layout, the readout's place, the pickers and both themes still want a walk on the
desktop (scratch store, port 7483) and in the browser.
