# Splitting a note in two, and joining two into one

Status: built 2026-11-06 on branch `settings`. Joe asked for it the same day: "a function
and a shortcut key for splitting the current node into two nodes (use InfoSelect's or
Scrivener's shortcut key for that if we can, by default, configurable in settings) and one
to join it (if possible) from the peer node below it". He agreed to the behavior below, and
to Focus Search moving to Ctrl+Shift+F so split could take Ctrl+K.

## What a person does

- **Edit > Split Note at Caret (Ctrl+K)**, in a text note. The text from the caret to the
  end becomes a new text note right below this one, under the same parent, and it opens
  in the pane.
  - With words selected, the split is where they start, and they become the new note's
    title. This is Scrivener's "Split with Selection as Title": its first line, at most
    120 characters.
  - Without a selection, the new note is named from its text, as any new note is.
- **Edit > Join Note Below (Ctrl+M)**, in a text note. The note right below it, its next
  sibling, joins it:
  - its text goes after this note's text;
  - its children go after this note's children;
  - it is deleted as an undoable delete, shown in Recently Deleted;
  - every link to it leads here, through `became` (docs/LINKS_CONTRACT.md).
- **The keys are Scrivener's** on Windows and Linux: Ctrl+K to split, Ctrl+M to join.
  InfoSelect's were not known, so they were not considered. **Settings > Keyboard** changes
  them, along with the other shortcuts listed there (`crate::shortcuts`). The menus take a
  changed key the next time Pimble starts (the next load in the browser), because rinch
  builds the menu bar once.

## Where a split falls

`NodeDoc::split_off`, given the sticky index the editor made at its caret:

- **In a paragraph or heading, at the caret.** At a block's very start or end it moves to
  the edge between blocks, so neither note begins or ends with an empty paragraph.
- **Inside a table, before the whole table.** A table never splits; the new note begins
  with it.
- **Inside a list or a quote, at the caret.** The structure is cut as the editor cuts a
  selection, so each note keeps its part of the list.
- **Refused**, with a sentence and nothing written, when nothing comes before the caret,
  when nothing comes after it, when the note is not a text note, and on a store's top
  node.

## What travels

- **Text.** The new note's content is a fresh projection of the text after the caret, with
  a history of its own (`content_doc::fresh_snapshot`'s reasoning, docs/MOVE_CONTRACT.md).
  The first note keeps its own history. Its edit is recorded as a deletion of what moved,
  so an editor holding it takes it as any remote change.
- **Canvases and sheets in the moved text.** Their data lives in the note, under
  `data["board:<id>"]` and `data["sheet:<id>"]`, and moves with them: it is copied into the
  new note with its yrs types, and removed from the first note when nothing left there
  names it. A join copies the data of the note joined in the same way.
- **Children** stay with the first note on a split. On a join, the children of the note
  joined in move under this one, after its own.
- **Order of writes.**
  - A split makes the new note first. A refusal there leaves the first note as it was.
    Then the new note gets its data, then the first note is cut.
  - A join appends the text first, then moves the children, then marks the note joined in
    with `became`, then deletes it.

  Each step is an ordinary API write (`Pimble::split_node`, `Pimble::join_next` in
  `crates/pimble-api/src/split.rs`): a create, `applyEdit`s, moves and a delete. There is
  no new RPC and no second way to write content.

## Known limits

- **Margin discussions** anchored in the moved text stay on the first note. Their anchors
  point at text that left it, so they show as they do when their text is deleted.
- **Undoing a join** with Put Back brings the joined note back with its text, but this
  note keeps the appended copy too. Undoing a split is a join.
- **A shared note** splits within its share. The new note is made under the same parent,
  so it is in the same share.

## Checked

- `pimble-crdt` `split_tests` (7):
  - inside a paragraph;
  - at a paragraph's start, which leaves no empty paragraph;
  - nothing before the caret, which is refused and writes nothing;
  - a canvas and its data travel with its text;
  - inside a table, which splits before the table;
  - a join, including into a blank note;
  - a split then a join, which gives the text back.
- `pimble-api` `a_note_splits_in_two_and_joins_back`, over a real server:
  - the new note lands right below, titled from the selection;
  - the first note keeps its children;
  - a join puts back the text, moves a child of the joined note under this one, and leads
    the joined note's link here;
  - a join of the last note is refused.
- `pimble-app`: `title_of` and the shortcut spelling (`normalize`); 221 tests in all.
  `pimble-crdt` 195, `pimble-api` api 16.
- **In the release app** on the scratch copy, with three paragraphs typed into a note:
  - Edit > "Split Note at Caret" with the caret before "Chapter Two" made "Chapter Two
    starts with this line. / And it goes on." a new note right below, opened in the pane.
    The first note kept "The first chapter begins here."
  - Edit > "Join Note Below" from the first note put the text back in the open editor, and
    the second note left the explorer.
  - With "Chapter Two" selected, the split titled the new note "Chapter Two". The first
    try showed the text-made title: `EXPLICIT_TITLE` was missing, now set and tested.
  - Settings > Keyboard listed the six shortcuts. Typing Ctrl+M for split said "Ctrl+M is
    already Join Note Below." and saved nothing; Reset left `shortcuts` empty.
  - The keys themselves were not pressed: the debug driver's key presses do not reach menu
    chords.
