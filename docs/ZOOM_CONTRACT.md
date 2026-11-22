# Zoom contract

Status (2026-11-21): built, not committed or released. Needs joeleaver/rinch#2861
merged; until then every rinch crate is pinned to rinch's `zoom` branch (all Cargo.toml
files, and the root, `web/` and `plugins/` locks at 197b680a).

Whole-window zoom, for people who read more easily with everything larger: View >
Zoom In, Zoom Out, Actual Size, the way a browser zooms a page.

## Decisions

1. **The zoom is rinch's, not Pimble's.** `rinch::window::set_zoom(z)` multiplies the
   display's scale by `z`: the document is laid out and drawn at `os scale × z` device
   pixels per CSS pixel, so text, icons, pictures and canvases stay sharp. The window
   keeps its size; its CSS viewport shrinks by `z`. Every pointer position Pimble reads
   (clicks, drags, the click context's element bounds, a canvas's surface events) is
   already in the zoomed CSS pixels, so none of Pimble's pixel math changes. No Pimble
   concept went into rinch (the API is a browser's zoom).
2. **Steps** are a browser's: 50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200, 250,
   300%. A zoom between steps (an edited `state.json`) moves to the next step up or
   down. A saved value outside 50..300% is brought inside; one that is not a number is
   100%.
3. **Keys**: Ctrl+= zooms in, Ctrl+- out, Ctrl+0 actual size (the menu items' chords).
   Ctrl and + (Shift and = on a US layout, which the Ctrl+= chord does not match) and
   the keypad's +, - and 0 work too: an app-wide key listener (`zoom::listen`) and the
   editor's own key hook (`editor.rs`, which hears keys before any app-wide listener)
   both ask `zoom::alias`. None of these collided with anything: the editor's keymap
   has no Mod-= / Mod-- / Mod-0, and no Pimble view binds them.
4. **Canvases keep their own zoom.** Ctrl+= / Ctrl+- / Ctrl+0 zoom the window
   everywhere, a focused canvas included (as in a browser, where Ctrl+= is always page
   zoom): the canvas never had keys for its own zoom, and a key that means two things
   depending on focus is the kind of thing a person who zooms for their eyes trips on.
   The canvas zooms as before: Ctrl+wheel, pinch, and the zoom corner's buttons. The
   canvas's surface reads its scale from its own physical and logical size
   (`boards::scale_of`), so it renders sharp and its strokes land under the pointer at
   any window zoom.
5. **Remembered per device**: `state.json` `zoom` (beside `dark_mode`), restored in
   `app::run` before the window is made, so the first frame is drawn at it.
6. **The status bar** shows the zoom ("125%") at its right end while it is not 100%;
   a click goes back to actual size. Nothing shows at 100%.
7. **The web** applies none. rinch-web leaves zoom to the browser (`set_zoom` is a
   no-op there and `zoom()` answers 1.0): the browser's own Ctrl and + / - / 0 zoom the
   page, already reach the app through `devicePixelRatio`, and are remembered by the
   browser per site. Applying CSS `zoom` to the page root from Pimble was rejected: the
   browser's own zoom does the same thing better, and CSS zoom makes element bounds and
   pointer coordinates disagree in ways rinch-web does not account for. So the web's
   View menu has the same three items with no keys (the browser's keys stay the
   browser's), and choosing one says "Your browser zooms Pimble: Ctrl and + or -, and
   Ctrl and 0 for actual size." Nothing is saved in `localStorage`.

## Built

- rinch: `rinch::window::{set_zoom, zoom, ZOOM_MIN, ZOOM_MAX}` (pull request on
  joeleaver/rinch, branch `zoom`; see `docs/src/guide/windows.md` "Zoom" there).
- `crates/pimble-app/src/zoom.rs`: steps, aliases, `set`/`zoom_in`/`zoom_out`/
  `actual_size`, `restore`, `listen`; tests for the step math, the sanitizing of a
  saved value, labels and aliases.
- `boards.rs`: the camera kept across a change of scale (`Session::presented_scale`).
- `menus.rs` (View: Zoom In, Zoom Out, Actual Size), `persistence.rs`
  (`load_zoom`/`save_zoom`), `app.rs` (restore and listener at start, the status bar's
  zoom), `editor.rs` (the aliases in the editor's key hook), `styles.rs`
  (`.pimble-status-bar__zoom`).

## Audit of Pimble's pixel math

All of it is in CSS pixels on both sides, which the zoom keeps consistent: the split
view's divider drags (ratio from the click context's element bounds and pointer),
tree drag-and-drop (row bounds and indent column), the hashtag caret-under-pointer
tolerance (40 CSS px, so physically larger when zoomed, as it should be), popovers,
the link tooltip and picker, context menus. Canvases convert surface events to
physical pixels with their own measured scale.

One thing did misbehave: a canvas's camera zoom is kept in device pixels, so a new
scale (the window's zoom, and before it a move to a display with another scale) left
the canvas content at the same device size while everything around it changed, and
its zoom corner then read 67% where it had read 100%. `boards::present` now remembers
the scale it last presented at and scales the camera by the change about the view's
middle, so a canvas keeps its own zoom as the person reads it.

## Checked on screen (2026-11-21, release build, light theme, 1x display)

Screenshots in the session scratchpad `zoom/shots/`: 100%, 110%, 150% and 75%; the
View menu at 100% and 150% (it scrolls inside the shorter viewport at 150%); a note;
a pencil stroke drawn on a canvas at 150% and at 75% lands exactly under the pointer;
the split divider follows a drag at 150% (and stops at the 200 px pane minimum on
either side); a tree drag-and-drop at 150% shows its line and strip and lands; a saved
150% restored at start; the status bar's "150%" click and Ctrl+0 go back to 100%. The
pointer was driven through rinch's debug port, which speaks CSS pixels; the physical
pointer path (winit's device pixels through the zoomed scale) is pinned by rinch's
`zoom_tests.rs`. The debug port cannot send the `=` key (punctuation arrives there as
an unnamed key), so Ctrl+= itself was not pressed; Ctrl and + (the alias) and Ctrl+0
(a menu chord) were.

## Limits

- The saved explorer width is in CSS pixels, so at a large zoom the explorer takes a
  larger share of the window, as a browser's sidebar would. Drag it narrower.
- rinch's headless `TestApp` does not apply the zoom, so Pimble's tests cover the step
  math and persistence, not the window at a zoom; that was checked on screen.
- In rinch, a hidden window's saved size and a few layer-shell paths were adjusted but
  not exercised by Pimble (it has one ordinary window).
