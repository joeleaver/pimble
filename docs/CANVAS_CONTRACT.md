# Canvas contract: boards are canvases, and every picture is one

Status: **built, all three parts (branch `canvas`)**. Written 2026-10-23 from Joe's
decisions of that day; Joe answered the six decisions the same day ("Decisions" at the
end), and part 3 was built to them ("Built", after them).

- Boards are called **canvases** in the interface. Stored keys and code keep "board".
- There is **no image node** any more, not even as a word in the app. Adding a picture
  always makes a canvas with the picture as its first element, locked in its own layer.
- That covers **pictures in text** too: a picture put into text is a canvas from the start.

Builds on docs/IMAGE_MARKUP_CONTRACT.md (the board model, the document, the views, boards in
text) and docs/IMAGES_CONTRACT.md (blobs, `pimble-blob:` URLs, the loader). Where this
contract and those disagree, this one wins.

Also decided for later, out of scope here: canvases are the spatial surface (whiteboards,
diagrams with connectors, markup). Charts and timelines will be views of data held in sheets
or node fields, never canvas geometry. Diagrams will be canvas features (a shape library,
routed connectors, snapping, one-shot auto-layout), not a separate node type.

## Glossary

**A canvas in the interface is a board in the code and in storage.** The node type stays
`board`, a canvas node's document keeps `data.board`, a picture in text keeps
`data["board:<id>"]` and the atom's `data-board`, embeds keep `pimble-board:` URLs, and the
crate, modules, types, CSS classes and RPC fields keep their names. Renaming stored data
would need a one-way migration on every device and on the hosted server, and the word buys
nothing there. Only words a person (or an assistant, through the MCP) reads change.

"Picture" stays the word for an image file or a picture element, as it is today ("Insert
pictures", "Copy Picture"). "Image node" goes; "Image" stays only in Edit > "Insert
Image..." (Joe's name for it, decision 4).

## 1. The interface rename (built)

Every user-visible word, in the desktop app and the browser (shared `pimble-app` code), the
MCP, the CLI, the site and the docs a person reads:

| Was | Is |
| --- | --- |
| Row menu "New Whiteboard" | "New Canvas" (title "Canvas", the chalkboard icon) |
| Row menu "New Image..." (dialog "New Image") | "New Canvas from Picture..." (dialog "New Canvas from Picture") |
| Edit / toolbar "Insert Board..." (embedded an existing board node) | "Insert Canvas": an empty canvas in the text (decision 4) |
| Edit / toolbar "Insert Image..." | "Insert Image...", unchanged: a canvas holding the picture (decision 4) |
| Picture menu "Convert to Image Node" | "Convert to Node" (decision 3) |
| Picture menu "Open Board", "Open Board in Split View" | "Open Canvas", "Open Canvas in Split View" |
| Canvas menu "Flatten into New Image" | "Flatten into New Canvas" |
| Picker "Board…", "Type to find a board", "No boards match" | gone: Insert Canvas no longer picks a node |
| Embed placeholders "This board was deleted." etc. | "This canvas was deleted.", "This canvas has not arrived yet.", "You cannot see this canvas.", "This canvas is empty." |
| Notices and refusals naming a board or an image node | the same sentences with "canvas" |
| `pimble-board` error messages (they reach notices) | "canvas" |
| Tooltips ("...drop them on the board", "Copy and save the board as a picture") | "canvas" |
| CLI presence line ", on the board" | ", on the canvas" |
| MCP `get_node` / `list_children` `type: board` | `type: canvas` (the stored type is still `board`) |
| Site: "Whiteboards and markup", "an infinite board" | "Canvases and markup", "an infinite canvas" |

The default title of a canvas made from a pasted picture stays "Picture"; one made from a
file keeps the file's name without its extension. An existing node titled "Whiteboard" keeps
its title (titles are the person's).

## 2. No image node (built)

Adding pictures in the explorer makes a canvas whose first contents are the picture:

- the row menu's "New Canvas from Picture...", files dropped on a row, Ctrl+V on a focused
  row with a picture on the clipboard, and "Flatten into New Canvas" (`boards/export.rs`);
- each makes one canvas node per picture, titled after the file, with the photo icon: the
  picture at one unit per pixel in a locked layer "Picture", and "Layer 1" above it to draw
  in. This is exactly what `add_image_nodes` (`crates/pimble-app/src/boards.rs`) did for
  image nodes; only the words changed, and the code keeps its names.

A canvas that began from a picture still opens on the arrow tool (a picture in a locked
layer), and one that began empty on select. Nothing else tells them apart, as before.

## 3. Pictures in text are canvases from the start (built)

Today a picture in text is a bare `image` atom; it gets a board, in the host note's
`data["board:<id>"]`, only when its first mark is drawn (three updates: the board, the
atom's `data-board`, the mark). From now on a picture is a canvas the moment it goes in.

### What the image atom holds

Unchanged in shape: `src` (the `pimble-blob:` URL of the picture's bytes), `alt` (the file
name, or empty for a pasted bitmap), `width` (when resized) and `data-board` (the canvas's
id in the host note's `data`). What changes is that `data-board` is **always present** on a
picture that Pimble put into text, from the first moment.

A canvas that began empty (Insert Canvas) is the same atom with `src`
`pimble-board:in-text` (`text_boards::IN_TEXT_SRC`): there is no picture, and `data-board`
names the canvas. It is not a `pimble-board:<store>/<node>` embed and is not a link.

### Putting a picture in

Insert Image..., paste and drop in text all end in `pictures::add_picture`, and when
`BlobStored` brings the URL, `pictures::stored` now:

1. mints the canvas id and builds the canvas in the pane's copy of the note: the picture at
   one unit per pixel in a locked layer "Picture", "Layer 1" above it (the same shape the
   markup path makes today, `boards::start_markup`, and the same as a canvas node from a
   picture), and sends it with `applyEdit`;
2. inserts the atom with `data-board` already set, in one editor transaction over the place
   the picture was aimed at (`text_boards::insert_canvas`, which Insert Canvas uses too).

These are two updates, in that order, for the reason the first mark is three today: the
atom lives in the editor's collaboration document, the canvas in the note's `data`, and no
transaction spans both. Sending the canvas first means a peer never sees a picture naming
a canvas it has not got; at worst it holds a canvas no picture names yet, which is what an
unreferenced canvas already is (kept, swept later). Undo of the insertion takes the picture
out and leaves its canvas in `data`, as deleting a picture does today.

The first mark is then one update (the mark), and "Mark Up..." on such a picture builds
nothing. `start_markup`'s path that builds a canvas stays only for pictures that lack one
(below).

Pasting a picture that already names a canvas copies that canvas under a fresh id, as
today. A pasted `pimble-blob:` picture with no `data-board` (copied from text written
before this change) gets a new canvas on the way in, the same as an inserted one.

### Showing a picture whose canvas has no marks

Today a picture with a board is rendered by `text_boards::load` (the board's visible extent,
one unit per CSS pixel, no wider than the widest pane's text), and one without is shown from
the blob's bytes. With every picture a canvas, every picture would go through the renderer:
slower to first paint, re-encoded, and no longer the original bytes at the screen's scale.
**Decided**: a canvas whose only element is its first picture, untouched (not moved,
cropped or resized, no page), is shown from the blob as a plain picture is today; the
renderer takes over at the first mark (`text_boards::shows_only_its_picture`). (Decision 2.)

The floating controls are unchanged: the eye shows only once there are marks, and the "⋯"
menu is always there.

### Pictures already in text with no `data-board`

Every note written before this change has bare pictures. **Decided**: they stay as they
are, and become canvases lazily, exactly as before, at their first mark (or "Convert to
Node"). To the person nothing differs: every picture has "Mark Up...", the same menu and the
same controls. Nothing is written when a note is opened.

Why not convert them when a note opens: a note open on two devices would mint two canvases
for one picture, and the atom's `data-board` would be last-writer-wins between them, leaving
an orphan; a reader cannot write at all, so their copies would never convert; and opening a
note would become a write, which the sync and presence layers treat as an edit. (Decision 1.)

### Markdown export and the MCP

`pimble_crdt::markdown::render` (rinch's `doc_to_markdown`) writes a picture as
`![alt](pimble-blob:<store>/<hash>)` and drops `data-board`, so an assistant reading a note
through the MCP (`get_node`, every tool answer that shows content) sees the picture as the
person first put it in, never the marks; Markdown going in refuses every image, so no tool
can add or replace a picture and edits that touch only named blocks leave pictures alone.

**Decided** (decision 5): the canvas's id goes into the URL. `markdown::render`
(`pimble-crdt`) writes `![alt](pimble-blob:<store>/<hash>#canvas=<id>)` for a canvas in
text, and `![alt](pimble-board:in-text#canvas=<id>)` for one that began empty, so the id
round-trips once Markdown going in accepts images. A picture with no `data-board` (from
before) is written as before, with no fragment. The MCP shows content through this, so
`get_node` shows the same. Reading the marks themselves and seeing the rendered picture
are the boards contract's wave 5 (`render_board` and the element list), still not built.

The loader's own sources use the same fragment (`<src>#canvas=<id>`, what
`set_image_source` asks for); `#board=`, the fragment before 2026-10-23, is still read,
and the stored attribute stays `data-board`.

`replace_content` on a note with pictures keeps refusing Markdown that contains them; a
rewrite that leaves them out removes them (as before). Unchanged, and noted only so it is
not mistaken for something this work broke.

**Since 2026-10-29** Markdown going in accepts the addresses it writes out, so pictures
survive a rewrite: `#canvas=<id>` goes back into `data-board`, and a picture may name only
a canvas its note holds, once (docs/MCP_CONTRACT.md, "Pictures are canvases"). The MCP's
canvas tools read and write the canvases themselves (docs/MCP_CONTRACT.md "Canvases").

### "Convert to Node"

Unchanged in behavior ("Convert to Image Node" until now): a canvas node under the note
whose canvas is a copy of the one in the text, naming the same blob, and the canvas in the
text becomes an embed of it. A canvas that began empty converts the same way (titled
"Canvas", the chalkboard icon). An existing canvas node goes into text by dragging its row
onto the text, or by pasting its link; Insert Canvas no longer embeds one.

## Decisions (Joe, 2026-10-23)

1. **Old pictures in text** without `data-board` stay lazy: nothing is written on open;
   they become canvases at their first mark.
2. **A canvas with no marks shows the original picture** from the blob; the renderer takes
   over at the first mark.
3. The picture menu's item is **"Convert to Node"**.
4. **Two items with different workflows**: "Insert Image..." opens a file dialog and puts
   in a canvas holding that picture (locked picture layer, draw layer above); "Insert
   Canvas" puts in an empty canvas and no longer embeds an existing canvas node. Dragging a
   canvas row onto text stays the way to embed one. Both in the desktop and the browser.
5. **The canvas id goes into the URL** in Markdown export and the MCP
   (`pimble-blob:...#canvas=<id>`), so it round-trips once Markdown accepts images; readers
   of the old `#board=` fragment keep working.
6. **Two edits, canvas first**, is the correct way to put a canvas into text.

## Built (2026-10-23, branch `canvas`)

Shared `pimble-app` code, so the desktop and the browser alike.

- `text_boards::insert_canvas` is the one way a new canvas goes into text: it builds the
  canvas in the host note's `data` (`boards::build_board`, the same shape every canvas
  starts with), sends it, then inserts the atom naming it over the aimed-at place, keeping
  the person's selection. Insert Image..., paste and drop of picture files (all through
  `pictures::stored`) and Insert Canvas use it. A stored picture whose size could not be
  read goes in bare, as one from before, and becomes a canvas at its first mark.
- Pasted pictures (`rebind_pasted_boards`): one naming a canvas this window holds brings a
  copy; a store's picture with none, or naming a canvas not held here, gets a new canvas
  when this window has its bytes (else it goes in bare); an empty canvas not held here
  comes in as a new empty one.
- The loader: `Source::Marked { blob: Option<BlobUrl>, board }`; an empty canvas draws a
  480 × 200 placeholder, "An empty canvas. Double-click to draw on it.", until something
  is drawn, then its drawing trimmed to the visible pixels.
- Mark Up... and Convert to Node work on a canvas that began empty; marking one up opens
  on the select tool, a picture's on the arrow.
- The link picker's board kind is gone (nothing opens it).
- `markdown::render` writes `#canvas=<id>` (above); docs/MCP_CONTRACT.md says so.

**Verified:**
- Tests (`pimble-app` 213, `pimble-crdt` 174): a picture put into text is a canvas from the
  start (the canvas is the first broadcast and exists in it, then the text; "Picture"
  locked under "Layer 1"; shown as the picture itself; the first mark is one update and
  hands it to the renderer); Insert Canvas puts in an empty canvas, drawn as a placeholder,
  marked up, then drawn; a pasted bare picture gets a canvas at its pixel size; sources
  with `#canvas=` and the old `#board=` both parse; Markdown writes `#canvas=` and leaves a
  picture from before alone. The web build (`web/`, wasm32, release) checks.
- Desktop, an isolated release app (port 7473, its own config and data, a scratch store),
  over rinch's debug port: Edit > Insert Image... put in the picture with its canvas (the
  note's document holds `board:<id>`, `data-board` and "Layer 1"); the controls showed only
  the "⋯" menu (Mark Up..., Convert to Node); Mark Up... and an arrow drawn from outside
  the picture, Done: the text showed the arrow beside the picture; Edit > Insert Canvas put
  in the empty-canvas placeholder; a double-click opened it, a rectangle drawn, Done: the
  text showed the rectangle; Convert to Node made a child "Canvas" with the chalkboard icon
  and the text became its embed.

**Not built, or known gaps:**
- A pasted picture's canvas follows the paste's text update a turn later (the paste is
  mid-dispatch), so for that moment a peer sees a picture naming a canvas it has not got;
  it is drawn as the plain picture meanwhile.
- Insert Canvas puts the canvas in and stops; it does not open it for drawing.
- An empty canvas's eye hides every mark, which leaves nothing to show; it then shows the
  placeholder sentence.
- Insert Image... in the browser was not driven here (the web build checks; the path is
  the shared `pictures::stored`).
