# Properties contract: a node's tags, fields and context in one panel

Status: drafted 2026-11-04 from Joe's request ("should we have a dedicated pane, maybe a
side panel, for editing key/value pairs and tags?"), with his decisions (section 1).
The decisions of section 7 taken as recommended ("looks good", 2026-11-04). Waves 1 and 2
built 2026-11-04 (section 8); waves 3 and 4 not started.

## 1. What Joe decided (2026-11-04)

1. **One panel, following the focused pane**, as the one toolbar does (Joe, 2026-10-08):
   not one per pane.
2. **It replaces the fields band** under the pane's title: fields have one home.
3. **It is called Properties.**
4. **Tags and fields first; then the context** (where a node lives, which Metanodes show it,
   what links to it, table rows that point to it).

## 2. Where it stands

- **Tags** are a comma-separated box in the Appearance... dialog (`app.rs`,
  `appearance_tags_text`), applied on Enter or Done. Nothing else in the app shows a node's
  tags.
- **Fields** are a band under the pane's title strip (`fields.rs`, `render_fields`), drawn
  while the node has any or one is being added (Edit > "Add Field"). Each field is one key
  of the node's `node` root, written as a small update through `applyEdit`
  (`fields::write`).
- Both now decide what a Metanode finds (docs/QUERY_METANODES_CONTRACT.md), which is why
  they want a place of their own.

## 3. The panel

- **Where**: on the right of the pane area, beside the panes and the one toolbar's width,
  the full height under the toolbar. Its width is dragged like a divider and remembered on
  this device (`views.rs`), as is whether it is open.
- **What it shows**: the node of the focused pane (`AppStore::focused_pane`, as the
  toolbar's `TARGET` follows it): its name and icon at the top. A pane with no node says
  "Choose a node to see its properties." A Metanode's own properties (its tags and fields),
  not its setup.
- **Opening it**: View > "Properties" (checked while open), `Alt+Enter` (decision 1), and
  Edit > "Add Field", which opens it with a new field's name box taking the keyboard.
- **Desktop and browser** alike, from pimble-app.
- **Read-only**: on a node this device may only read, tags and fields show without their
  controls, under the access note the menus use.
- **A node shown through a mount or found by a Metanode** is the real node: its properties
  are its own, edited in place.

## 4. Wave 1: tags and fields

- **Tags**: chips, each with a remove button; a box after them takes a new tag on Enter or
  comma, suggesting the tags already used in the node's store (from the documents the API
  holds, `hold_store`), case-insensitively, as you type. Adding or removing one is one edit
  of the node's tags through the one write (`Pimble::edit`, as Appearance's tags are now).
  Tags leave the Appearance dialog, which keeps icon and colors.
- **Fields**: one row per field, the name and the value, each edited in place (Enter or
  leaving the box writes it, through `fields::write`, unchanged); a remove button per row;
  "+ Add field" at the end. Values are kept as typed, as now; formulas and Metanodes read
  them as they do.
- **The fields band goes**: `fields::render_fields` and its slot in the pane are removed;
  Edit > "Add Field" opens the panel instead.
- **Live**: both follow edits from anywhere (another window, the CLI, an agent) through the
  node's signal, as the band does.

## 5. Wave 2: the context

Read-only sections under the fields, each collapsed to one line when empty:

- **Lives in**: the node's place (Store / Parent / ...), each step opening it; said when the
  focused pane reached it through a mount or a Metanode.
- **Shown in**: the Metanodes whose queries find it ("Waiting", "Doing stuff"), each opening
  the Metanode. Evaluated by the API over the store's query Metanodes (each is a walk of the
  store, docs/QUERY_METANODES_CONTRACT.md "Built"), debounced, and only while the panel shows
  this section.
- **Links here**: the nodes whose text links to it. The search index has them
  (`pimble_search::Index::backlinks`); a `backlinks` RPC and `Pimble::backlinks` expose
  them (the browser's encrypted stores search client-side, as they do now).
- **Rows pointing here**: the table rows whose link columns choose it, grouped by table
  (docs/SHEETS_CONTRACT.md section 8). The MCP computes this today
  (`rows_pointing_here`); its walk moves into `pimble_api` so the app and the MCP share it.
- **Created and changed**: the node's timestamps, in the person's local time.

## 6. Waves

1. The panel, tags, fields; the band and Appearance's tags removed; View > Properties,
   `Alt+Enter`, Edit > "Add Field".
2. Lives in, created and changed (no new queries).
3. Shown in, Links here (the `backlinks` RPC), Rows pointing here (shared with the MCP).
4. Checked in the release desktop app and the browser: a plain store, an encrypted one, a
   read-only share, a node through a mount, a found node.

## 7. Decisions (taken as recommended, 2026-11-04)

1. **The shortcut**: `Alt+Enter` (recommended: the "properties" key on Windows, and free in
   the editor), or none.
2. **Open at first**: closed until opened (recommended: the panes keep their width), or open.

## 8. Built: waves 1 and 2 (2026-11-04)

**What and where.**

- `crates/pimble-app/src/properties.rs`: the panel, rendered once by `pane_view::render_panes`
  in a `pimble-panes-row` beside `.pimble-panes` (under the toolbar; the panes keep their
  percent layout inside what is left). It shows `pane_view::pane_document` of the focused
  pane, with the pane strip's own icon and title (`pane_icon`, `pane_title`, now
  `pub(crate)`). Width dragged by its left edge (200 to 560 px), open/closed and width in
  `views.rs` (`Views::properties`, `PanelView`; `views.json` / `localStorage`), restored at
  start in `app::build_view`; closed until first opened.
- **Opening**: View > "Properties", a checked item (`MenuEntry::Check` over
  `AppStore::properties_open`, rinch's `MenuItem::checkbox`) with the shortcut `Alt+Enter`.
  rinch tries menu chords before the app hears the key and swallows a chord with Alt even
  in a text target, so neither the editor nor the explorer sees it. Edit > "Add Field" (and
  the toolbar's) is `properties::add_field`: opens the panel with the new field's name box
  focused (`request_focus` on the desktop, the element's `focus()` in the browser).
- **Tags**: chips with ×, a box that takes a tag on Enter or comma (`split_typed`), with up
  to six suggestions as you type from the tags used in the store (`suggestions`: starts-with
  first, then contains, case-insensitive, the node's own left out; a typed tag takes the
  store's spelling). Each change is one `SetNodeAppearance` whose `tags` is now a
  `TagEdit::{Add, Remove}`, applied inside `Pimble::edit` to the tags the document has, so a
  tag someone else added meanwhile stays. The Appearance dialog's tags box, its
  `appearance_tags_text` and the replace-all tags path are gone.
- **Fields**: a row per field, name and value each a box written on Enter or leaving it
  (`onchange`) through `fields::write`, unchanged; a rename is the new name written with the
  value, then the old removed (a change of case only is ignored, as `fields::write` treats it
  as the same field); × removes; "+ Add field" (Escape cancels). `fields::render_fields`, its
  slot under the pane title, `PaneState::adding_field` and `fields::add_field` are removed.
- **Read-only**: on a node this device may only read, the access note
  (`state::access_note`) at the top, chips without ×, fields as text, no boxes.
- **Context (wave 2)**: "Lives in" is the store's name then the ancestors under the top,
  each opening that node in the focused pane (`AppStore::link_open`); "Found by <Metanode>."
  when the pane's row is a found row, "Shown through the mount <name>." (or "Shown by the
  Metanode <name>.") when its tree value crossed one. "Created ... · Changed ..." in local
  time (`chrono::Local`, `wasmbind` in the browser).
- **Backend**: `BackendCommand::GetProperties` answers `PropertiesLoaded { lives_in,
  store_tags }` from `Pimble::ancestors` and the new `Pimble::tags_in(store)` (holds the
  store, reads every held document's tags, not deleted, once per case). The panel asks when
  it opens, when the node it shows changes, and when that node's parent or tags change
  (deduplicated, so typing in the note asks nothing). Routed in the browser by
  `vault::store_id_of`; a read, so not gated.
- **Live**: tags, fields, icon, title and times read the node's signal, so edits from
  anywhere show as they land.

**Tests**: `properties::tests` (suggestions, comma splitting, `TagEdit` keeps one of each
case, local time), `views` round trip with the panel; `pimble-app --lib` 216 pass; the web
build compiles (`wasm32`, only the pre-existing warnings).

**Checked in the release desktop app** (scratch environment, port 7473): Alt+Enter opened
the panel from the editor and closed it again without a line break in the note; View >
"Properties" opens it and shows the check while open; typing "d" offered "done", a click
added it, "urgent," added on the comma, "home" on Enter, × removed one; a found node went
out of and back into the "Waiting" Metanode as its tags changed. "+ Add field" put the
keyboard in the name box; "rate"/"25" added, the value edited to 250 on leaving the box,
the name renamed to "rates" on Enter, × removed a field. An agent's `set_tags` and
`set_fields` (pimble-mcp) showed in the open panel at once. Splitting, the empty pane shows
"Choose a node to see its properties."; focusing either pane switched the panel; a node
opened under a mount said "Shown through the mount Kanban." and lives in "Other / Kanban";
a found node said "Found by Waiting."; Edit > "Add Field" with the panel closed opened it
with the name box focused; Appearance... has no tags and an icon chosen there showed in
the panel's head; light and dark themes both read; the width drag and open state survived
a restart. A first cut of "Lives in" laid its steps out inline and a click on one did
nothing (rinch hit-tests boxes, not inline text runs); they are flex items now, and a click
opened the step.

**Not done / not checked**: the read-only face was not seen in the app (no shared store in
the scratch environment); the browser was compiled but not driven. "Found by" is read from
the explorer's found rows, so it is missing after a restart (the restored pane has no row
yet) and stays after the node stops being found until the pane opens something else.
Ancestors renamed elsewhere show their new name only when the panel asks again. Waves 3
(Shown in, Links here, Rows pointing here) and 4 (the full check matrix) remain.
