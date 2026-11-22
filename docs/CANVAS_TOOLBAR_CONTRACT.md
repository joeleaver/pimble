# The toolbar for canvases (draft, 2026-11-21)

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
