# Hashtags: tags from a note's text (draft, 2026-11-20)

Joe, 2026-11-20: "it would be nice if #hashtags in note text were automatically added (and
removed when no longer in the text) to the tags list for the note, without messing with the
ones manually entered."

Status: **decided** (Joe, 2026-11-20: the × on a text tag removes it from the text). Not built;
queued after convergence wave 3, which is editing the Properties panel.

## The rule

**A note's tags are the tags entered for it plus the #hashtags in its text.** The text tags are
worked out from the text whenever the node is read; they are never stored. That keeps one copy of
each fact: the text is the only place a hashtag lives, so it appears when it is typed, goes when it
is deleted, and merges with everyone's edits like the rest of the text (CLAUDE.md: no mirrors). The
entered tags (`node.tags`, the existing Array) are untouched by anything in the text.

## What counts as a hashtag

- `#` at the start of a text run or after whitespace or opening punctuation (`(`, `[`, `"`, ...),
  then a letter (any script), then letters, digits, `_`, `-` and `/`. A trailing `-` or `/` is not
  part of it. `#1`, `#!`, `a#b` and a lone `#` are not hashtags.
- In paragraphs, headings, list items, quotes and table cells. Not in inline code or code blocks.
  A link's text counts; its address is not text.
- The tag is the word without `#`, as first written in the note. Text tags and entered tags are
  matched without regard to case: `#Pimble` and an entered `pimble` are one tag, shown once, as
  entered.
- Text only, for now: canvases, sheets' cells and plugin data are not read for hashtags.

## Where it shows

- `Node.tags` is the union (entered first, then text tags in the order they first appear), so
  everything that reads tags (search, metanode queries and grouping, the MCP's `get_node`, the
  web) sees them with no change of its own. `Node.text_tags` (new, serde default) says which came
  from the text.
- Properties: a text tag is shown with a small mark, tooltip "From the text: #pimble (2 places).
  × removes it from the text."

## Removing one

- **× on a text tag removes it from the text** (Joe): every occurrence of that hashtag in the
  note is deleted, with one adjacent space so no double space is left, as one `applyEdit` of the
  content, merging with anyone typing. A tag that is both entered and in the text loses both.
- Setting a note's tags (Properties, the MCP's `set_tags`) writes the entered tags only: a text
  tag in the list given is not copied into `node.tags`, and a text tag left out of the list is
  removed from the text, by the same rule as the ×.
- Adding a tag in Properties adds an entered tag, never text.

## To build

- `pimble-crdt`: `NodeDoc::text_tags()` (a walk of the content's text runs, skipping code), the
  union in `NodeFields::into_node`, and `NodeDoc::remove_hashtag(tag)` producing the content edit.
- The writers of tags (`Tree::set_tags` callers, Properties, MCP `set_tags`) split the list.
- Search: the index's tags follow `Node.tags`; reindex when content changes (already debounced).
- Tests: the syntax cases above; code skipped; case matching; a hashtag typed on one device
  appears as a tag on another; × removes every occurrence and merges with a concurrent edit;
  entered tags never change because of text.
