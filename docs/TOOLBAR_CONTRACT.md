# The main toolbar (2026-11-21)

Joe, 2026-11-21, on 0.28.18: "Embedded canvases need the same export ability and their own
toolbar. Done could move up into that same toolbar and replace edit on the toolbar. Let's make it
nicer, and consistent. The main toolbar should always follow the selected item." And: "not sure
we even need or want that floating bar." This file was docs/CANVAS_TOOLBAR_CONTRACT.md; its
earlier sections are kept below under "History".

## Which toolbar (the one rule)

There is ONE toolbar, above the panes, for the focused pane (Joe, 2026-10-08). It shows the
toolbar of what is selected in that pane. One function decides, `crate::toolbar::which`; nothing
else chooses (`pane_view` shows what it names):

1. A pane showing a discussion: the text groups, dimmed (a discussion has its own message box).
2. A pane showing a canvas (a canvas node, or a picture's canvas being edited over its note):
   the canvas groups.
3. A pane showing a sheet node: the sheet groups.
4. A pane showing a metanode, or nothing: the text groups, dimmed (nothing to act on).
5. A note: a cell of a sheet in its text being typed in or chosen: the sheet groups. Otherwise
   what its editor's selection is exactly (`PaneState::item`, written from the editor's own
   selection callback, `toolbar::item_changed`, a turn after it fires; no polling):
   - a picture, a canvas that began empty, or an embedded canvas node: the canvas groups;
   - a table of the note's sheets, or an embedded sheet node: the sheet groups (the selected
     table is the one its buttons act on);
   - anything else, a caret or text: the text groups (Table group while in a text table).

Inventory of what else can be selected or focused in a pane, and where it goes: a text table
(rinch's): text groups with Table; a web picture (not Pimble's): text; an inline discussion:
text (moving or deleting it is a text edit; it has its own message box); a field: fields live in
the Properties panel, not in panes; a hashtag or a link: text; a margin discussion: text; a
plugin's view: it is a metanode pane (rule 4). A press in a note outside a table in its text
(the editor keeps its presses on the desktop, so the pane hears the release) ends that table's
cell, so the toolbar follows the press back to the text (`sheets::pressed_outside`). The sheet
groups in a note's text leave out Sheet (Add Table, Add Summary Table): a table goes into text
with Insert > Sheet, where the caret is.

## Edit and Done

The Canvas group is the first canvas group, shown for a canvas that lives in a note's text or
was opened from one, and while a symbol is being edited. It holds ONE button: "Edit Canvas" (a
pencil) while the canvas is only selected; "Done" (a check, the primary color) while it is
being edited. Edit Canvas is what a double-click does: a picture's canvas opens over the note,
an embedded canvas node opens in the pane (and Done then goes back to the note, through the
pane's history). Done while editing a symbol finishes the symbol. Escape with nothing selected
and no gesture under way also finishes marking up a picture (until now only Done did; Escape
already finished a symbol). After Done the item stays selected, so the toolbar stays on the
canvas groups with Edit Canvas back.

## No floating bar

The bar over the top of a canvas is gone. What it carried:

- **Done** (marking up a picture) and **Symbol "name" Done**: the Canvas group's one button.
- **The selection's or the tool's controls** (stroke, fill and brush swatches; width, line
  style, arrow heads, text, alignment, opacity and feather, shape, stroke detail, smoothing,
  brush preset, size, opacity, flow, hardness, fill tolerance and reach, strength, the Stamp's
  symbol and settings; duplicate, combine, align, group, ungroup, lock, delete, and More with
  forward, backward, front, back, flip, outline stroke, text on a path, symbols, scatter): the
  contextual group after the Canvas group, labeled with what it acts on ("Rectangle", "3
  Selected", or the tool in hand, "Brush"); their pickers open under it, over the canvas. While
  it shows, View is the percentage alone and its menu holds Zoom In, Zoom Out and the minimap,
  so the toolbar stays one row. Nothing was dropped: the layers panel's lock, delete, opacity
  and blend act on layers, these on elements.
- **The label and the hint** ("Select: click to select, drag to select several"): the label is
  the contextual group's; the hint goes to the status bar's left, small and muted, while a
  canvas is focused and the toolbar has nothing for the tool. Kept rather than dropped: the
  rail's tooltips name the tool and its key, not what it does.
- **Undo and redo**: already the History group's.
- The canvas's other floating things stay: the tool rail, the color well, the panels, the
  minimap, the markup control, the text being typed, and the picture's own controls in a note.

## Embedded canvases

An embedded canvas node selected in a note gets the whole canvas toolbar. Export renders the
node's canvas as this window holds it (its one frame when the embed shows one); Save Original
saves its picture when it holds exactly one and otherwise dims with its reason; Import SVG dims
("Edit the canvas to import SVG into it."); Flatten puts the new canvas beside the canvas node;
file names are the canvas node's title. View and People open it in the pane first. History is
the note's while it is only selected.

## File names

No two exports default to the same name: "<title>.png", "<title>.jpg", "<title>.svg",
"<title> (all markup).png", and the original "<title> (original).<its own extension>".

## One family

The canvas and sheet groups' buttons have the text groups' size (30 px) and colors (the toolbar
icon color, the active border and fill), every toolbar dims a group or button to 40%, and every
group has the same small uppercase label. Tooltips are title case like the text toolbar's. The
text toolbar's contents are unchanged.

## Built (2026-11-21, v2)

`toolbar.rs` (`Item`, `Picked`, `which`, `item_changed`), `state/pane_state.rs` (`PaneState::item`),
`editor.rs` (the selection callback), `text_boards.rs` (`selected_item`, `embed_raw`,
`mark_up_at` for embeds), `pane_view/mod.rs` (one rule), `pane_view/pane.rs` (the release that ends a text table's
cell), `boards/toolbar.rs` (Canvas group, contextual groups, embeds), `boards/rail.rs`
(`render_context_bar` answers the controls and their pickers; `context_label`, `context_shown`,
`status_hint`), `boards/ui.rs` (no bar), `boards/tools.rs` (Escape), `boards/export.rs`
(embeds, names), `sheets.rs` (`pressed_outside`), `sheets/toolbar.rs` (in-text sheets, the
selected table), `sheets/in_text.rs`, `app/shell.rs` (the status bar hint), `styles.rs`.

Tests: the rule over every pane kind and item; a selection of exactly a picture or an embedded
canvas node (not a caret beside it); export names all different; the opening zoom; the groups.
Walked on the desktop in both themes (scratchpad `ctb/shots/v2-*`): caret in text, picture
selected, picture being edited (Done in the toolbar, Escape back to the picture selected), a
cell of a sheet in the text (sheet groups; a click in the text back to the text groups), an
embedded canvas selected and its export (PNG and the original written), the embedded canvas
opened by Edit Canvas (Done shown), a canvas pane, a shape selected (its group, a picker open
under it).

Limits: Done after opening an embedded canvas was shown but not pressed in the walk; it steps
the pane's history back. A window too narrow for one row wraps the toolbar, which moves the
canvas down (the selection outline was once seen offset until the next change). The picture's
own controls in a note can sit over a toolbar dropdown while the pointer is still over the
picture. The browser was not walked: the web build compiles, and its editor keeps its presses
differently, so ending a text table's cell by a click in the text is unverified there.

## Icons or words, never codes (2026-11-21, v3)

Every control in a canvas's contextual group is an icon or a word, with a tooltip naming it and
its value; the value itself is in the tooltip and the picker, never a bare number or an
abbreviation on the button. Opacity (shapes and brushes) is a droplet ("Opacity: 80%", and the
feather when there is one), so the toolbar's only bare percentage is the canvas zoom ("Canvas
Zoom"). Text is a text-size icon ("Text: size M, Sans, bold"); a shape's parameters are a
corner-radius, polygon or star icon ("Corner radius: 12", "Sides: 6", "Points: 5, inner 50%");
line width and brush size are a dot drawn at their size ("Line width: 3", "Brush size: 12");
flow, hardness, tolerance and strength are those words ("Flow: 50% ..."); a smoothing, a
preset or a fill mode is its word; the Stamp's settings are an adjustments icon with its
spacing and jitter in the tooltip. Kinds and tools looked over: rectangle, ellipse, diamond,
polygon, star, line, arrow, connector, pen, pencil, path, text, sticky, step, frame (no
controls), picture (no style controls: duplicate, lock, delete, More), group, several selected,
brush, highlighter, eraser, fill, smudge, blur, stamp, the paint selections, eyedropper.

The status bar says what the tool in hand does whenever a canvas is focused (a canvas pane, or a
canvas being edited over its note), for every tool: until v3 it said so only for the tools with
nothing to set and with nothing selected, so most of the time it said nothing.

## One row (2026-11-21, v4)

Joe: "fix the toolbar wrapping in narrow windows." Every toolbar (text, canvas, sheet) is one row
at any width (`toolbar_fit.rs`). Widths are measured with rinch's bounds signals on both the
desktop and the browser: the row's own box, and each group's box as it last showed, with its
label and without. As the room shrinks, first every group's label goes (the row gets shorter),
then whole groups fold, lowest priority first, into a trailing More (three dots) dropdown that
lists each folded group's label over the same buttons, acting and showing state as the row's do
(a second copy of the group's buttons over the same state). The rule is `toolbar_fit::fit`
(tested); 6 px are kept spare so a group does not flicker in and out.

Priorities (first to fold first):
- Text: Insert (Edit > Insert has all of it), Discuss, History (Ctrl+Z), Table, Paragraph,
  Text last.
- Canvas: People, Snap, Export, History, View last. Canvas (Edit/Done) and the selection's
  own group never fold. While the selection's group shows, View is also the percentage alone
  (its menu holds every View command), as before.
- Sheet: Sheet (Add Table), History, Table, Column, Row last.

Checked at window widths 700 (text, canvas and sheet fold; More opened on text and canvas; H2
and the grid toggled from inside More), 900, 1100, and 1100 at 150% zoom (one row, labels on).
The More menu stacks each group's label over its buttons so it stays no wider than the widest
group: the panes' area clips what reaches past its left edge.

## Embedded items (2026-11-21, v4)

Joe: "for the canvas and other embedded things, remove/hide buttons that don't make sense for
the embedded view. Like the grid options, and maybe People." An item selected inside a note and
not being edited shows only what acts on it where it sits:

- A picture or an embedded canvas selected: Canvas (Edit Canvas), Export, History (the note's).
  View, Snap and People show only on an open canvas; the v2 behavior of opening the canvas
  when one of them was clicked is gone (they are not there to click). Once it is being edited,
  the full canvas set shows, as before.
- A sheet selected in a note (its atom, or a cell): Row, Column, Table, History. The Sheet group
  (Add Table, Add Summary Table) is the only part that makes sense only in a sheet node's pane
  and is hidden in text (since v2): a table goes into text with Insert > Sheet.

## History: the toolbar for canvases (2026-11-21; built the same day)

Joe, 2026-11-21, pointing at a canvas whose toolbar showed the text buttons dimmed while the
canvas's own controls sat in its bottom-right corner: "What do you think about moving this lower
control section into being the main toolbar for canvases?" Agreed; queued after
docs/ZOOM_CONTRACT.md (both touch the menus and the status bar).

### The rule

There is still ONE toolbar, above the panes, for the focused pane (Joe, 2026-10-08). What it
shows follows what that pane holds: the text groups for a note, the canvas groups for a canvas
(a board node, or a picture in a note's text while its markup is being edited: back to the text
groups on Done). A split with a note and a canvas switches as the focus moves.

### Canvas groups (labeled like the text groups)

- **View**: minimap toggle, zoom out, the canvas zoom percentage (a menu: Fit, 100%), zoom in, fit.
- **Snap**: grid, magnet.
- **People**: what the bottom-right people button does today.
- **Export**: one dropdown with everything in today's export menu (Copy Picture, Save as
  PNG/JPEG/SVG, Save with All Markup, Save Original As, Flatten into New Canvas, Import SVG).
- **History**: undo and redo, moved from the floating "Done / Select" bar, where text keeps them.

### Stays on the canvas

The tool column on the left, the Layers / Meaning / Frames panels, the minimap itself, the color
swatch, "Markup · N". The floating bar keeps "Done" and the tool hint. The bottom-right control
strip goes.

### Notes

- The toolbar's percentage is the canvas's zoom; the status bar's is the window zoom
  (docs/ZOOM_CONTRACT.md). Keep the two labeled apart.
- Web and desktop alike, from shared `pimble-app` code (`toolbar.rs`, `boards/`).
- Read-only canvases: View and Export stay live, Snap and History dim.

### Built (2026-11-21)

- `crates/pimble-app/src/boards/toolbar.rs`: `render_canvas_toolbar`, rendered once and
  following `focused_pane`, like the sheet's toolbar. `pane_view/mod.rs` shows it instead of the
  text toolbar while the focused pane shows a board (`show_board`, so a picture in text being
  marked up counts; Done puts the text groups back) and no discussion. The text toolbar is
  hidden there, not dimmed.
- View: minimap toggle (active while shown), zoom out, the percentage (tooltip "Canvas zoom";
  a dropdown with Fit and 100%), zoom in, fit. The percentage reads the pane's
  `board_ui.zoom`, which every view change publishes (buttons, Ctrl+wheel, pinch, follow), and
  is shown rounded to a whole percent. The window zoom stays in the status bar only.
- Snap: grid and magnet (active while on; per viewer, every canvas, as before). People: color
  by author (this pane). Export: one dropdown with the eight items of the old menu, acting on
  the focused pane. History: undo and redo (`tools::undo`).
- Read-only: View, People and Export stay live; Snap and History dim. People was not named
  in the contract: it hides nothing and writes nothing, so it stays live with View.
- On the canvas: the bottom-right corner holds the minimap only (`rail::render_map_corner`,
  hidden with it); the floating bar lost undo and redo and keeps Done, the tool label and
  hint, and the selection's own controls. `Popup::Export` is gone.
- A press on the canvas toolbar hands the canvas's letter keys back to its chrome
  (`keys_to(.., Keys::Chrome)`), as a press on the old strip did.
- Tests: the group order and which groups dim read-only, the percentage label. Walked on the
  desktop (scratch store, port 7483): every button, both dropdowns, Flatten, undo after a
  delete, a split switching with the focus, markup in text and back, both themes.
- Limits: a read-only canvas was not walked (no cheap fixture); Ctrl+wheel was not driven
  (the debug port has no Ctrl for a wheel), but it moves the same signal the buttons do. The
  web build compiles (shared code); it was not opened in a browser.

### Selected picture (Joe, 2026-11-21; built the same day)

Joe: "It would be nice if when we selected a picture embedded in a note, we also got the same
toolbar." While the focused note's selection is exactly a canvas in its text (a picture of the
store's, or a canvas that began empty; an embedded board node is not one, and keeps the text
groups), the canvas groups show instead of the text groups, before any markup; back to the text
groups when the selection leaves it.

- **Canvas** (a first group, only for a canvas in a note's text): "Edit Canvas", what a
  double-click does (`text_boards::mark_up_at`); active while the markup is open, and pressed
  then it is Done.
- **Export** acts on the picture without opening its canvas: the board in the note, or for a
  picture not marked up yet the board its markup would start as, rendered whole (no selection
  inside it). Save Original As saves the picture's own bytes and dims, with its reason as the
  tooltip, on a canvas that began empty; Import SVG dims ("Edit the canvas to import SVG into
  it."), since it writes into the open canvas. File names are the note's title.
- **Snap** toggles the per-viewer settings directly.
- **View** and **People** opened the canvas first (until v4, "Embedded items": now they
  show only on an open canvas). The percentage shows the zoom the canvas would open at: fitted into the
  pane's text area with a 24 pixel margin, never above 100% (`boards::present`'s rule);
  "Fit" while that cannot be told (the picture not here yet, the pane with no size yet).
- **History** is the note's undo and redo while the picture is only selected (deleting or
  moving it is an edit of the text), the canvas's once it is open.
- **Read-only note**: Export stays live; Snap, History and Edit Canvas dim, and so do View and
  People, because on a selected picture they open the canvas and a reader cannot open one for
  markup (the read-only refusal). On a read-only canvas that shows, View and People stay live
  as before.

Built: `crates/pimble-app/src/boards/toolbar.rs` (`Aim::Board` or `Aim::Picked`),
`toolbar.rs` (`Picked`, `picked()`, refreshed by the toolbar's watcher from the focused pane's
editor), `text_boards.rs` (`selected_canvas`, `raw_board`, `mark_up_at`), `boards/export.rs`
(`copy_picked`, `save_picked`, `save_picked_original`, `flatten_picked`), `pane_view/`.
Tests: a selection of exactly a picture (not a caret beside it, not an embed), the opening zoom.
Walked on the desktop in both themes: selecting the picture, Save as PNG / SVG / Original
from it (files right), zoom in from it (the canvas opens at 125%, Edit Canvas lit), Edit Canvas
pressed again (back to the note, the picture still selected and the canvas groups still
shown), the note's undo from the canvas History.

Limits: after Done the picture is usually still selected, so the canvas groups stay until the
selection moves (the rule, applied). A selected picture is noticed by the toolbar's 120 ms
watcher, not at once. Save as PNG and Save Original As of a picture name the same file. The
read-only case and the browser were not walked.
