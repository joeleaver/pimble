# Hashtags: tags from a note's text (draft, 2026-11-20)

Joe, 2026-11-20: "it would be nice if #hashtags in note text were automatically added (and
removed when no longer in the text) to the tags list for the note, without messing with the
ones manually entered."

Status: **built 2026-11-20** (Joe, 2026-11-20: the × on a text tag removes it from the text). Not
merged or released; see "Built (2026-11-20)" at the end for what was decided while building and the
known limits.

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

## Built (2026-11-20)

- **`pimble-crdt`** `hashtags.rs`: the scanner (`hashtags_in`, the rule above), the walk of the
  content's yrs text (every textblock at any depth, table cells by live row then live column, code
  blocks skipped, characters under the `code` mark masked), `union`, `split_wanted`, and `remove`.
  `NodeDoc::text_tags`, `text_tag_counts`, `remove_hashtag(tag)`, `set_all_tags(wanted, now)`
  (`Tree::set_all_tags` the same in a tree). `NodeFields.text_tags` is filled by `NodeDoc::fields`
  and `Tree::live_fields`; `NodeFields::into_node` puts the union in `metadata.tags`;
  `NodeFields::all_tags` is the union for code that reads fields.
- **`pimble-core`**: `NodeMetadata.text_tags` (serde default, skipped when empty).
- **Writers**: Properties (`SetNodeAppearance` with a `TagEdit`, applied to every tag and written
  with `set_all_tags`), the MCP's `set_tags` (says "Removed from the text: #x." when it removed
  any), `LocalStore::update_node_metadata` (the whole metadata: written with `set_all_tags` when the
  union differs) and `LocalStore::create_node` (a tag that is a hashtag of the new node's content is
  not entered).
- **Readers**: metanode tag conditions (`Matcher::matches` reads the text tags only when the entered
  ones did not answer), and a match with a tag condition counts content edits (`reads_content`);
  the store's tags Properties suggests (`tags_in`) include text tags; the MCP's `get_node` adds a line
  `tags from the text: #a, #b`; the search index's tags follow `Node.tags`, so the debounced reindex
  after a content edit brings them.
- **Properties**: a text tag's chip is drawn with a dimmed `#` before it, its tooltip is "From the
  text: #pimble (2 places). × removes it from the text.", and its × says "Remove it from the text".
  This window's own typing is not shown back to it, so once typing rests (the label refresh's 300 ms)
  the node's signal takes its tags, text tags and content from the editor's document
  (`editor::refresh_text_tags`); nothing is read while neither the text nor the node has a `#`.
- Tests: the syntax cases, masked code, union and split (`pimble-crdt` `hashtags::tests`: every kind
  of block, code skipped, case matching, union order, × removing every occurrence with one space and
  keeping marks, merging with a concurrent edit from another document, a hashtag typed on one device a
  tag on another, entered tags unaffected by text edits, `set_all_tags` leaving one out);
  `pimble-api` a tag condition finding a hashtag; `pimble-server` the index node's tags;
  `pimble-app` Properties chips and their tooltips; `pimble-mcp` `get_node`/`set_tags`/`search`
  end to end.

### Decided here

- **`text_tags` lives on `NodeMetadata`** beside `tags` (the contract's `Node.tags` is
  `node.metadata.tags`). It lists every hashtag in the text, including one that is also entered, so
  Properties can mark it and its × removes both.
- **Removal is plain yrs deletions** of the textblocks' `Text`s (never a model rewrite), back to
  front per textblock. The space taken is the one right after the hashtag, else the one right before
  it (`Then #bob, and` becomes `Then, and`); a space is never taken twice.
- **What may precede `#`**: start of a textblock, whitespace, an inline atom (image, hard break),
  inline code, or `( [ { " ' “ ‘ « ‹ < ¿ ¡`. A hashtag running into inline code stops there.
- **Adding a tag that is already a text tag is a no-op** (it is a tag already); it is not entered
  as well. If the hashtag is later deleted from the text, the tag goes with it.
- **`NodeDoc::stored_fields`** reads the `node` root without the text: the tree's walks, repair,
  `derive_changes` (so a typed hashtag is a content change, never a metadata one) and the metanode
  scan use it; `fields()` and everything that builds a `Node` read the text tags.
- `set_all_tags` writes nothing when nothing differs, so the MCP's `set_tags` with the tags a note
  already has is refused as "That would change nothing", like its other tools.

### Known limits

- A table cell that an earlier merged cell covers (hidden, inert in the CRDT) is still read for
  hashtags; rare, and only after concurrent merges.
- In Properties, a tag typed in the text shows once typing rests in this window (300 ms), and its
  count of places follows only when the set of text tags or their counts change.
- The count of places is read from the node's document as the window's node signal holds it (the
  editor's document after this window's typing rests, the API's after another device's edit).
- Canvases, sheets' cells and plugin data are not read for hashtags (as the contract says).


## Next: hashtags look like hashtags in the text (Joe, 2026-11-20)

Joe: "hashtags need some formatting or something. I'm afraid to pill-ize them because that
changes inline editing." So no pill and nothing in the document: a rinch editor plugin
(`Plugin::decorations`, inline decorations, as `presence/text.rs` draws carets) marks each
hashtag's range with a class, found by the same scanner as the tags (`pimble_crdt::hashtags`, code
skipped), recomputed per changed block. The style changes color only (the theme's accent, both
schemes), never padding, border, weight or size, so the text lays out exactly as before and the
caret, selection, typing and deleting are unchanged. Ctrl+click on a hashtag searches for that tag
(as Ctrl+click follows a link). Desktop and browser alike. Queued after docs/STARTUP_CONTRACT.md.
