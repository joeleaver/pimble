# Canvas contract: boards are canvases, and every picture is one

Status: **parts 1 and 2 built (branch `canvas`); part 3 waits on Joe's answers** in
"Decisions" at the end. Written 2026-10-23 from Joe's decisions of that day:

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
pictures", "Copy Picture"). "Image" and "image node" go.

## 1. The interface rename (built)

Every user-visible word, in the desktop app and the browser (shared `pimble-app` code), the
MCP, the CLI, the site and the docs a person reads:

| Was | Is |
| --- | --- |
| Row menu "New Whiteboard" | "New Canvas" (title "Canvas", the chalkboard icon) |
| Row menu "New Image..." (dialog "New Image") | "New Canvas from Picture..." (dialog "New Canvas from Picture") |
| Edit / toolbar "Insert Board..." | "Insert Canvas..." |
| Edit / toolbar "Insert Image..." | "Insert Picture..." (it inserts a picture into text; see decision 4) |
| Picture menu "Convert to Image Node" | "Make Canvas Node" (decision 3) |
| Picture menu "Open Board", "Open Board in Split View" | "Open Canvas", "Open Canvas in Split View" |
| Canvas menu "Flatten into New Image" | "Flatten into New Canvas" |
| Picker "Board…", "Type to find a board", "No boards match" | "Canvas…", "Type to find a canvas", "No canvases match" |
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

## 3. Pictures in text are canvases from the start (not built: decisions first)

Today a picture in text is a bare `image` atom; it gets a board, in the host note's
`data["board:<id>"]`, only when its first mark is drawn (three updates: the board, the
atom's `data-board`, the mark). From now on a picture is a canvas the moment it goes in.

### What the image atom holds

Unchanged in shape: `src` (the `pimble-blob:` URL of the picture's bytes), `alt` (the file
name, or empty for a pasted bitmap), `width` (when resized) and `data-board` (the canvas's
id in the host note's `data`). What changes is that `data-board` is **always present** on a
picture that Pimble put into text, from the first moment.

### Putting a picture in

Insert Picture..., paste and drop in text all end in `pictures::add_picture`, and when
`BlobStored` brings the URL, `pictures::stored` now:

1. mints the canvas id and builds the canvas in the pane's copy of the note: the picture at
   one unit per pixel in a locked layer "Picture", "Layer 1" above it (the same shape the
   markup path makes today, `boards::start_markup`, and the same as a canvas node from a
   picture), and sends it with `applyEdit`;
2. inserts the atom with `data-board` already set (`EditorHandle::insert_image_at` taking
   attributes, or an insert followed by `SetNodeAttrStep` in the same editor transaction).

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
**The proposal**: a canvas whose only visible element is its first picture, untouched
(not moved, cropped or resized), is shown from the blob as a plain picture is today; the
renderer takes over at the first mark. (Decision 2.)

The floating controls are unchanged: the eye shows only once there are marks, and the "⋯"
menu is always there.

### Pictures already in text with no `data-board`

Every note written before this change has bare pictures. **The proposal**: they stay as
they are, and become canvases lazily, exactly as today, at their first mark (or "Make
Canvas Node"). To the person nothing differs: every picture has "Mark Up...", the same menu
and the same controls. Nothing is written when a note is opened.

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

**The proposal**, the smallest that tells an assistant the truth without a new rinch hook:
- Markdown keeps `![alt](pimble-blob:...)` for every picture. The canvas id is not written:
  nothing outside Pimble can use it, and Markdown going in still refuses images.
- `get_node` adds one line after the content when the text has pictures with marks:
  `canvases in the text: 2 pictures with marks (alt "sunset": 3 marks; alt "": 1 mark)`.
  Reading the marks themselves and seeing the rendered picture are the boards contract's
  wave 5 (`render_board` and the element list), still not built.
- `replace_content` on a note with pictures keeps refusing Markdown that contains them; a
  rewrite that leaves them out removes them (as today). This is unchanged and noted only so
  it is not mistaken for something this work broke.

(Decision 5.)

### "Make Canvas Node"

Unchanged in behavior ("Convert to Image Node" until now): a canvas node under the note
whose canvas is a copy of the picture's, naming the same blob, and the picture in the text
becomes an embed of it. With every picture a canvas, there is always a canvas to copy.

### Tests to add with part 3

- an inserted, pasted and dropped picture each carries `data-board`, and its canvas is in
  the note's `data` with the picture locked in "Picture" and "Layer 1" above, sent before
  the atom;
- the first mark on such a picture is one update;
- a pasted bare `pimble-blob:` picture gets a canvas; a pasted picture with one gets a copy;
- a canvas with no marks is shown from the blob; the first mark switches it to the renderer;
- an old bare picture is not written when its note opens, and is marked up as today;
- undo of an insertion leaves the canvas in `data`.

## Decisions (for Joe)

1. **Old pictures in text** (section 3): keep the lazy path for pictures without
   `data-board` (nothing written on open; they become canvases at their first mark), rather
   than converting them when a note opens? Recommended: yes, lazy.
2. **A canvas with no marks shows the original picture** (from the blob, sharp at the
   screen's scale), and the renderer takes over at the first mark? Recommended: yes.
3. **The name of "Convert to Image Node"**: "Make Canvas Node" (built that way for now),
   or another wording ("Move to Its Own Node", "Open as Separate Canvas")?
4. **"Insert Image..." in text** became "Insert Picture...", keeping "Insert Canvas..." for
   embedding an existing canvas node. Or should "Insert Canvas..." do both (a picture from a
   file, or an existing canvas), leaving one item?
5. **Markdown and the MCP** (section 3): Markdown unchanged (`![alt](pimble-blob:...)`, no
   canvas id), plus a one-line count of marked-up pictures in `get_node`? Or write the
   canvas id into the URL (`pimble-blob:...#canvas=<id>`), which would round-trip once
   Markdown accepts images?
6. **The two-update insertion** (section 3): accept the canvas and the atom as two updates
   (canvas first), as the first mark is today? A single update would need rinch to let an
   editor transaction carry a change to another root of the same yrs document, which is a
   general mechanism worth asking for only if the gap ever shows.
