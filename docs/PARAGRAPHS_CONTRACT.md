# Paragraph spacing

Status: built 2026-11-13 (Joe agreed the design the same day).

Some notes are documents (a novel, a report), where a gap between paragraphs reads
well. Most are quick notes, where Enter should just start the next line. So each note
says which it is.

## The setting

- `custom["paragraphs"]` in the node's document (`pimble_core::custom_keys::PARAGRAPHS`):
  `"spaced"` (a gap after each paragraph) or `"compact"` (none). Absent is compact.
  It lives beside the title, so everyone who edits the node shares it, it replicates
  and moves with the node, and an edit is an `applyEdit` like any other key.
- It is how the note is drawn, never what it holds. Enter makes a paragraph in both
  (Shift+Enter is a line break inside one, as before). Switching back and forth
  changes no text, and search, links, discussions on a passage and Markdown read
  the same paragraphs either way.
- Only paragraphs change. Headings, lists' items (their paragraphs follow the
  note), quotes, code blocks and tables keep their own margins.

## Where a note's setting comes from

- A new node copies its parent's value when it is made (`Tree::add_node` and
  `add_node_with_doc`, from `custom_keys::INHERITED`), in its own first update, so
  every way of making a node (the app, the browser's vault, the MCP, the CLI, a
  plugin) does it. Copied, not looked up: changing a folder later changes none of
  the notes already in it, and a note never needs its ancestors to be drawn (a
  share's member may hold none of them).
- A store's default is its root node's value: set it on the store row.
- Notes from before this have no value and are compact. Imports (Scrivener, Word,
  RTF) are writing, so the importer marks every node it makes spaced.

## Where it is set

- Edit > "Compact Paragraphs": a check item for the focused pane's note, checked
  while compact (`AppStore::compact_paragraphs`, kept in step with the note by
  `pane_view::follow_paragraphs`). With no note there, or one this device may only
  read, the check goes back to what the note says.
- "Appearance..." (any row's menu, a store row's too): a "Paragraphs" row,
  Compact or Spaced, applied at once like the rest of the picker.
- Both write through `BackendCommand::SetNodeAppearance { paragraphs }`.

## How it is drawn

`pane_view.rs` gives the pane's `.pimble-editor__content-wrap` the class
`--spaced` when its note is spaced; the stylesheet sets `p { margin: 0 }` in the
editor and restores rinch's `0 0 0.75em` under `--spaced`. Desktop and browser alike.
