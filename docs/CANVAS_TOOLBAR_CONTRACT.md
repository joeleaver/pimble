# The toolbar for canvases (2026-11-21; built the same day)

Joe, 2026-11-21, pointing at a canvas whose toolbar showed the text buttons dimmed while the
canvas's own controls sat in its bottom-right corner: "What do you think about moving this lower
control section into being the main toolbar for canvases?" Agreed; queued after
docs/ZOOM_CONTRACT.md (both touch the menus and the status bar).

## The rule

There is still ONE toolbar, above the panes, for the focused pane (Joe, 2026-10-08). What it
shows follows what that pane holds: the text groups for a note, the canvas groups for a canvas
(a board node, or a picture in a note's text while its markup is being edited: back to the text
groups on Done). A split with a note and a canvas switches as the focus moves.

## Canvas groups (labeled like the text groups)

- **View**: minimap toggle, zoom out, the canvas zoom percentage (a menu: Fit, 100%), zoom in, fit.
- **Snap**: grid, magnet.
- **People**: what the bottom-right people button does today.
- **Export**: one dropdown with everything in today's export menu (Copy Picture, Save as
  PNG/JPEG/SVG, Save with All Markup, Save Original As, Flatten into New Canvas, Import SVG).
- **History**: undo and redo, moved from the floating "Done / Select" bar, where text keeps them.

## Stays on the canvas

The tool column on the left, the Layers / Meaning / Frames panels, the minimap itself, the color
swatch, "Markup · N". The floating bar keeps "Done" and the tool hint. The bottom-right control
strip goes.

## Notes

- The toolbar's percentage is the canvas's zoom; the status bar's is the window zoom
  (docs/ZOOM_CONTRACT.md). Keep the two labeled apart.
- Web and desktop alike, from shared `pimble-app` code (`toolbar.rs`, `boards/`).
- Read-only canvases: View and Export stay live, Snap and History dim.

## Built (2026-11-21)

- `crates/pimble-app/src/boards/toolbar.rs`: `render_canvas_toolbar`, rendered once and
  following `focused_pane`, like the sheet's toolbar. `pane_view.rs` shows it instead of the
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
