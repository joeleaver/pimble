# Properties contract: a node's tags, fields and context in one panel

Status: drafted 2026-11-04 from Joe's request ("should we have a dedicated pane, maybe a
side panel, for editing key/value pairs and tags?"), with his decisions (section 1).
Nothing built. The decisions of section 7 taken as recommended ("looks good", 2026-11-04).

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
