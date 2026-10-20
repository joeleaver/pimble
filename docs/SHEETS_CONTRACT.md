# Sheets contract: small live tables in notes, with Excel formulas

Status: **waves 1 to 3 built** (branch `sheets`, not merged; the "Built in wave ..."
sections at the end say what exists and how it was checked). Written on 2026-10-19 from a brainstorm with Joe. It
started from "a spreadsheet-like node, but not exactly. What you want isn't a giant sheet
full of data, but a quick little tool, and you end up using just a few little ranges".
Joe's examples were a trip cost calculator, a household budget and a time tracker. Every
decision he made that day is listed in "Decisions" at the end.

The throwaway mockup the design was tried in:
https://claude.ai/artifact/3QBgFyrJb9KmyyyzSEjFT1 (private to Joe, plain JavaScript,
nothing of it ships).

Builds on docs/NODE_DOCUMENT_CONTRACT.md (the `data` root), docs/LINKS_CONTRACT.md (the
`[[` picker, `became`) and the rinch node views being built on rinch branch
`feat/node-views` (section 9).

## The idea

The thing is a **sheet**: a small named table that lives in a note. It is not an
infinite grid of cells. A table has typed columns and ordinary rows, and a formula
belongs to a whole column. A note can hold several tables with prose around them, and
the tables read each other and other notes' tables by name. "Sheet" is a working name.

It has four ideas, and Joe's three examples need nothing else:

| Idea | What it is | Example |
| --- | --- | --- |
| **Table** | named columns of a type, and rows | Vehicles: name, mpg |
| **Formula column** | one Excel formula for the whole column, run per row | Trips' cost: `=[@gas]+[@nights]*[@[hotel rate]]` |
| **Link column** | each cell points at a row of a table, or at a node | a trip's vehicle, an entry's project |
| **Summary** | a table whose rows come from a formula | spending by month: `=GROUPBY(...)` |

Every row is an ordinary row. There is no scratch row and no current row (Joe: "burdensome
and overspecific"): a quick calculation is a row you keep typing over, and a trip worth
keeping is a row you leave alone.

Sheets are separate from the rich text's tables (Joe, 2026-10-19). Text tables are loose
and untyped. Sheets are typed and computed. A later "Convert to Sheet" action can turn
one into the other.

## 1. Where sheets live

Sheets work like pictures and boards (Joe: "they'd work like images and be able to be
used both ways"):

- **A sheet node** (`node_type = "sheet"`) is a node made only of tables, stacked in order
  (each table's `order`, section 2). It has no prose. A row's menu gets "New Sheet".
- **Sheets in text.** A table sits in a text node's content wherever its embed atom is
  (rinch's `embed` with `data-sheet=<table id>`). Pimble renders it there as a live
  component through rinch's node views (section 9). Insert > "Sheet" adds one at the
  caret.
- **Both keep their tables in the same place**: each table is a key `sheet:<table id>` of
  the node's own `data` root (section 2). A table in a text node that no atom names any
  more, e.g. after its atom was deleted, shows in a "Tables no longer in the text" list on
  that node, with "Put Back". Nothing is deleted behind the person's back.

"Convert to Sheet Node" and its reverse move tables between a text node and a new sheet
node, the way "Convert to Image Node" does for boards.

- **A sheet node embedded in text** (Joe, 2026-10-20, as board nodes are): an `embed` atom
  with `data-node` (the node as a `pimble:` link), `data-kind="sheet"`, optionally
  `data-table` (one table) and `data-title` (the node's title when it was embedded). It
  shows the sheet node's tables live, under its title (a link to it); editing them edits
  the sheet node. It is a link to the node (backlinks, `became`, placeholders when it is
  not there). Insert > "Sheet from a Node..." (the picker, sheet nodes only) puts one in;
  Move leaves one where the table was, as "Convert to Image Node" leaves an embed. A
  table of a note stays in the note's document (it goes wherever the note goes); a table
  to share between notes lives in a sheet node that any number of notes embed.

## 2. The sheet document

Everything a person types is co-edited data in the node's `data` root, keyed by id. Ids
never move inside arrays, so concurrent edits to different rows, columns or cells merge.

```
data["sheet:<table id>"]: Map           one key of the node's `data` root per table
  v:       1
  name:    String           unique within the note when set (section 5)
  order:   String           fractional; a sheet node's stacking order
  source:  String | absent  a summary's formula (section 4); absent for a plain table
  headers: Map<int, String> a summary's renamed result columns
  columns: Map<column id, Map>
    name, type, order (fractional),
    target  (link to a row: the table id),
    under   (link to a node: the parent node's (store, node), or absent for anywhere),
    formula (formula column: the stored formula, section 6),
    format  (formula column: automatic | number | money | date | hours | percent)
  rows:    Map<row id, Map>
    order: String (fractional)
    cells: Map<column id, value>
```

- **No container is ever created on first use.** Each table is a key of the `data` root,
  which always exists, and its inner maps are made by whoever makes the table. A map
  created on first use (a `data.sheets` holding the tables) would lose one side's tables
  when two devices each added a note's first table while apart: both would write a new
  map to the same key, and yrs keeps one. Other users of the `data` root (boards,
  plugins) keep their own keys beside these.

- **A cell's value** is plain data: a number, a string, an ISO date string, a link as
  `{ row: <row id> }` or `{ node: [store, node] }`, or the text as typed when it does not
  parse. Text that doesn't parse shows `#VALUE!` and keeps what was typed.
- **Last writer wins per cell, and per formula.** A cell and a formula are each a single
  value in a Map, so two people editing the same one end with one person's whole edit,
  never a character-level blend of two formulas.
- **Computed values are never stored.** Every client computes them from the document.
  Nothing is cached in the document, mirrored to the server or kept in step by anyone
  ("Everything is co-editable"; no projections).

## 3. Column types

| Type | Holds | Takes as typed | Header mark |
| --- | --- | --- | --- |
| Text | a string | anything | none |
| Number | a number | `1234.5`, `1,234.5` | `#` |
| Money | a number | `$1,234.50`, `1234.5` | `$` |
| Date | an ISO date, computed as an Excel serial number | `today`, `yesterday`, `10/14`, `Oct 14`, `2026-10-14` | `date` |
| Hours | a number of hours | `1:30`, `1.5`, `90m`, `1h 30m` | `h` |
| Link to a row of a table | a row id | the row's name | `→ Vehicles` |
| Link to a node | a node | the node's title | `→ Projects` |
| Formula | nothing typed | a formula, starting with `=` | `ƒ` |

The UI calls this the column's **type** (Joe, 2026-10-19). From the mockup:

- **Headers are one line**: the name, then the faint mark. The full type, or the
  formula, is in the tooltip.
- **A link cell opens a picker** listing every choice, with a ✓ on the current one.
  Typing filters it, the arrow keys move through it, Enter picks, and Escape closes it.
  The browser's own autocomplete is wrong for this: it lists only what already matches
  the text in the cell.
- **A link-to-a-node column names where its choices come from**: "Nodes under: Projects",
  or anywhere in the tree. Typing a name that isn't there offers "Create "Deck repair"
  under Projects". The new node gets its siblings' field names, empty.
- **Typing `=` into a cell of an empty column** turns the whole column into that formula,
  as Excel Tables do.
- Tab moves right, Enter moves down, and Enter on the last line starts a new row.
- **The formula bar** shows only for a computed cell or a cell with an error, in the
  table's title row, and only while the table has the focus.

## 4. Summaries

A summary is a table whose `source` is a dynamic-array formula: `GROUPBY`, `FILTER`,
`SORT`, `UNIQUE`, `HSTACK`, `LET`. Its rows and its first columns come from that
formula's result. The person can rename those columns (`headers`) and add formula columns
of their own, which read the result's columns with `[@column]`. A summary has no typed
cells and no new-row line.

```
ByMonth = GROUPBY(HSTACK(TEXT(Transactions[date],"yyyy-mm"), Transactions[category]), Transactions[amount], SUM)
          columns month, category, spent; own columns budget = [@category].[monthly budget], left = [@budget]-[@spent]
```

## 5. Names, and what is unique

- **A table name** is unique within its note when it is set: a second "Expenses" in the
  same note is refused with a sentence. A merge can still produce two (two people adding
  one offline). Nothing breaks, because formulas point at ids. The note says "Two tables
  here are called Expenses" until one is renamed.
- **A column name** is unique within its table, by the same rule.
- **Node titles are never required to be unique** (Joe, 2026-10-19). A personal tree is
  full of "Notes" and "Budget".
- **A name never decides what a stored formula points at.** Names exist for typing and
  showing. Section 6 is how that works.

## 6. Formulas

**Excel's formula language** (Joe, 2026-10-19: "excel-like and excel-compatible
formulas, or at least google-sheet level"), as it works in an Excel Table:

- **Structured references** for this table and others: `[@miles]`, `[@[gas price]]`,
  `Transactions[amount]`, `Entries[[hours]:[billable]]`, `Vehicles` for the whole table.
- **Excel's operators and precedence**, including `&`, `%`, `^`, and `-2^2 = 4`.
- **Excel's functions with Excel's behavior.** Wave 1 has the set the mockup has, and it
  grows by conformance tests:
  - conditional: `IF`, `IFS`, `IFERROR`, `IFNA`, `SWITCH`, `CHOOSE`, `AND`, `OR`, `NOT`, `LET`
  - totals and criteria: the `…IF`/`…IFS` family with Excel criteria (`">100"`,
    `"<>Gas"`, wildcards), `SUM`, `AVERAGE`, `MIN`, `MAX`, `COUNT`, `COUNTA`, `MEDIAN`,
    `SUMPRODUCT`
  - lookups: `XLOOKUP`, `VLOOKUP`, `INDEX`, `MATCH`, `XMATCH`
  - dynamic arrays: `FILTER`, `SORT`, `UNIQUE`, `HSTACK`, `GROUPBY`
  - text: `TEXT` with Excel format codes, `TEXTJOIN`, `CONCAT`, `LEFT`, `RIGHT`, `MID`,
    `LEN`, `UPPER`, `LOWER`, `TRIM`, `VALUE`
  - dates: `TODAY`, `DATE`, `YEAR`, `MONTH`, `DAY`, `WEEKDAY`, `EOMONTH`, `EDATE`, `DAYS`
  - numbers: `ROUND`, `ROUNDUP`, `ROUNDDOWN`, `ABS`, `INT`, `MOD`, `SQRT`, `POWER`
- **Dates are Excel serial numbers**, so `TODAY()-7`, `">"&EOMONTH(TODAY(),-1)` and
  `DATE(2026,13,1)` behave as in Excel.
- **Errors are Excel's**: `#N/A`, `#VALUE!`, `#REF!`, `#NAME?`, `#DIV/0!`, `#SPILL!`
  (a list where one value goes), `#CALC!`, `#CIRC!` (a column that depends on itself).
  Each carries a sentence for the formula bar.
- **One extension: a dot follows a link**, as Excel's linked data types do.
  `[@vehicle].mpg` means `XLOOKUP` on the linked row, and `[@project].rate` reads a field
  of the linked node. It is the one thing that does not paste back into Excel.

**Other notes** (Excel's form for outside the workbook, with a note as the workbook):

| You want | You write |
| --- | --- |
| a table in this note | `Transactions[amount]` |
| a table in another note | `'Household budget'!Transactions[amount]` |
| a title without spaces | `Budget!Transactions[amount]` |
| two notes with one title | `'Home/Budget'!Transactions[amount]` (the shortest unique path) |
| another store, mounts included | `'Family/Household budget'!Transactions[amount]` |
| a field on a node | `'Client website'!rate` |

A bare table name means this note only. There are no global table names. `[[` in a
formula opens the links picker, and choosing a note inserts its `'Title'!`.

**Stored by identity, shown by name.** The stored formula is the Excel text with every
name replaced by an id token: the note's `(store, node)`, the table's id and the column's
id. A same-note reference is stored as "this note", so tables copied together keep
pointing at each other. The formula editor turns ids into the current names to show, and
names into ids on commit. The exact token encoding is wave 1's to settle. As a result:

- Renaming or moving a note, table or column never edits any formula; everyone just sees
  the new name. A tombstoned note is followed through `became`, as links are.
- An ambiguous name is resolved only while typing or pasting: the editor holds the
  formula, underlines the name and opens the picker with the candidates and their paths.
- A path is display only. It can differ per person (a member of a share sees a smaller
  tree), and that is fine.

**When a referenced note is not there:**

| Situation | Cell | Formula bar |
| --- | --- | --- |
| deleted | `#REF!` | "Household budget was deleted" (Put Back brings it back) |
| not readable by this person | `#REF!` | "You can't read Household budget" |
| a mount connecting, or never synced | `Loading…` | "Waiting for Household budget to arrive" |
| offline with a copy | the value | "From your offline copy of Household budget" |

## 7. Computing

- **Computing happens in the client**, desktop and web, from one crate, `pimble-sheet`
  (model, parser, evaluator). It reads the documents the app holds. While a sheet is
  shown, the app holds the documents its formulas read, subscribed as a pane holds a note.
- **Recomputing is incremental**: a dependency graph from cells and columns to what they
  read, so typing in one cell recomputes only what depends on it. A cycle is `#CIRC!` in
  the cells on it, never a hang.
- **The server never computes.** The MCP server is a local client, so it can link
  `pimble-sheet` and answer computed values (section 10).

## 8. Node fields, and rows that point here

- **A node has fields**: named values on the node itself (a project's rate, a person's
  phone). They are co-edited per field. Where they live is open decision 1.
- **A table reads a linked node's fields with the dot**: `[@project].rate`.
- **A node shows the rows that point here**: on its page, every row of every table that
  links to it, grouped by table, with totals for its number columns. It is computed from
  the documents, and nothing is copied into the node. Finding those rows uses the index
  the way backlinks do. A node with children lists them, and says which link columns
  choose from them.
- Later: a field schema for a node's children ("every project has a rate and a client"),
  like Tana's supertags. That turns this into a real personal database. Field values come
  first.

## 9. rinch: node views

Sheets in text, and later boards in text, need the app to render a live component inside
the editor's document. rinch has had no way for an app to do that. Joe made it top
priority on 2026-10-19, generic and reusable. The design was agreed with the Rinch
session that day and is being built as three stacked PRs on rinch branch
`feat/node-views`:

- **A generic `embed` block atom** in the starter kit, carrying `data-*` attributes like
  `image`. It syncs through collaboration as a leaf atom. Every peer needs the build that
  has it before anyone inserts one.
- **`EditorHandle::register_node_view(NodeViewSpec { node_type, matches, render,
  on_caret_enter })`.** The view's context gives attrs, selected, read_only and dark_mode
  as reactive values, plus `pos`, `set_attrs`, `delete`, `select`, `return_focus` and
  `editor`. Every write is an editor transaction, so it is undoable and synced.
- **The view survives unrelated edits.** It remounts on cut, paste and moves, and is never
  handed a different node.
- **The editor keeps its hands off the view:** presses inside it go to the view, and an
  input inside it takes the keyboard.
- **The view gets Escape first.** If it doesn't use it, Escape returns the keyboard to
  the text, with the caret after the node.
- **The view's height follows its content**, and the caret and selection stay right.
- **It can claim `image` atoms too**, which the boards refactor (wave 6) needs.
- **The same API on desktop and in the browser.**

Nothing in rinch knows what a sheet is ("No app concepts in rinch").

## 10. Search, MCP, sharing

- **Search** indexes typed cells as table-row `IndexUnit`s (column names as context), so
  "Lake Tahoe" finds the trip. Computed values are not indexed: they are derived and
  would go stale.
- **The MCP has full support** (Joe, 2026-10-19): anything a person can do with a sheet
  in the app, an LLM can do through `pimble-mcp`. Every write goes through `applyEdit` on
  the node's document, as docs/MCP_CONTRACT.md requires, and touches only what it names:
  a cell edit is one cell's entry, never the table rewritten. Tables, columns and rows are
  named the way people name them (the table's name, the column's name, a row by its first
  column's value or its id), and node names follow that contract's "Naming a node". The
  MCP server links `pimble-sheet`, so it computes the values it reads exactly as the app
  does. The tools, each shipping in the wave that builds what it touches:

  | Tool | Does |
  | --- | --- |
  | `list_tables` | a node's tables (or a store's, or every table that reads a given one): name, kind (table or summary), columns with types, row count, where it sits (sheet node, or in which text) |
  | `get_table` | a table's columns (name, type, link target or "under", formula, format) and rows with typed **and** computed values, plus each error's code and sentence; filters by column value and a row limit for big ones |
  | `evaluate` | a formula run in a table's context (one row, or none) without storing it: for checking a formula before setting it, and for answering questions ("what did we spend on gas in September?") |
  | `create_table` | a new table in a sheet node, or in a text node at a quoted place (the embed atom with it, in one edit), with columns; or a new sheet node |
  | `add_rows`, `update_rows`, `delete_rows` | rows by name or id, values as a person types them (`"$12.50"`, `"yesterday"`, `"Civic"` for a link), parsed by the column's type; a link that matches nothing is refused with a sentence unless asked to keep it as typed |
  | `add_column`, `update_column`, `delete_column` | name, type, target, "under", formula, format; changing a type carries the values over as typed, as the app does |
  | `set_summary` | a summary's formula and its column names |
  | `rename_table`, `delete_table`, `move_table` | a table within its node or into another (Convert to Sheet Node and back) |
  | `get_fields`, `set_fields` | a node's fields (section 8) |
  | `rows_pointing_here` | every row in every table that links to a node |
  | `paste_table` | tab-separated or Markdown-table text as a new table or new rows, with the same column mapping and type guessing as a paste (section 11) |

  **How the MCP writes formulas.** Models already know Excel. The MCP adds what is ours,
  and a way to check before anything is saved:
  - **A guide, read on demand**: "Sheet formulas", published as an MCP resource and as a
    `sheet_formula_guide` tool for clients that ignore resources. It covers Excel Table
    syntax, the dot for links, `'Note'!` references, the supported functions, what
    differs from Excel, and worked examples. The formula-setting tools' descriptions point
    at it.
  - **The table's vocabulary**: `get_table` returns the names, types, link targets and
    existing formulas, which are the best examples there are.
  - **Checking before writing**: `add_column`, `update_column` and `set_summary` refuse a
    formula that does not parse, names something that does not resolve, gives a list
    where one value goes (`#SPILL!`) or depends on itself (`#CIRC!`). When they accept
    one, the answer carries the computed values for the first rows. `evaluate` runs the
    same checks without saving.
  - **Errors that say how to fix them**: "There's no function called NETWORKDAYS() in
    Pimble yet; the date functions are …", "Trips has no column "price"; its columns are
    …". The app shows the same sentences.

  Formulas go in and come out **in their shown form** (`'Household budget'!Transactions[amount]`),
  and are turned into ids on the way in exactly as the formula editor does. An ambiguous
  name is refused with the candidates and their paths, never guessed. `get_node` shows a
  text node's tables as Markdown tables at their places, with their names, so a note reads
  whole. A Markdown table written through the text tools stays a text table: a sheet
  is made only by the sheet tools.
- **Sharing needs nothing new.** A table is part of its node's document, so whoever can
  edit the node can edit its tables, and readers read them. A formula reading a note the
  person cannot read shows `#REF!` for that person only.

## 11. Copy and paste

In layers (Joe, 2026-10-19: "would be nice"):

1. **Values in (wave 4).** Excel and Google Sheets put the cells on the clipboard as
   tab-separated text and an HTML table. Pasting maps the columns, takes a header row as
   names when it looks like one, and guesses each new column's type.
2. **Structured-reference formulas in (wave 4).** Google Sheets puts each cell's formula in
   its copied HTML; what Excel puts there must be checked first. Formulas from an Excel
   Table already use our syntax.
3. **A1 formulas translated in (wave 5).** `B2` in the same row becomes `[@mpg]`, and
   `C2:C40` becomes `Trips[cost]`. Anything outside the table pattern pastes as its value,
   with a sentence saying so.
4. **Out (wave 4).** Values as they are, and formulas with structured references. The dot
   is written out as the `XLOOKUP` it means.

## Waves

1. **`pimble-sheet` and the sheet node**, desktop and web from shared code:
   - the document model, tables, the column types except link-to-a-node, and summaries
   - the parser and evaluator, with Excel conformance tests: expected values checked
     against Excel itself, including its oddities
   - same-note references, stored by id
   - the sheet node, "New Sheet", the picker, the formula bar
   - the MCP tools for tables, columns, rows, summaries and `evaluate`, and the formula guide
2. **Sheets in text** on rinch node views: the embed atom, Insert > "Sheet", "Convert to
   Sheet Node", the "no longer in the text" list.
3. **Nodes in sheets:**
   - link-to-a-node columns with "under" and create
   - node fields (after open decision 1) and the rows that point here
   - cross-note references and their states
   - search units, and the MCP's `get_fields`, `set_fields` and `rows_pointing_here`
4. **Copy and paste**: values in and out, structured-reference formulas in, and the
   MCP's `paste_table`.
5. **A1 formula translation** on paste.
6. **Boards in text onto node views.** A marked-up picture and an embedded board node
   mount the live board view instead of drawing a PNG behind an `image` atom with
   floating controls (`text_boards.rs`). A PNG is still made, on demand, for export,
   Markdown, the MCP and the clipboard. A plain picture stays an `image` atom.

## Open decisions (Joe's)

1. **Where node fields live.** Decided (Joe, 2026-10-20): apart from Pimble's own keys
   in `custom`, one co-edited entry per field. Stored as `node` root keys
   `field:<name>`, not a nested map, by the rule of section 2 ("No container is ever
   created on first use"): two devices giving an old node its first field while apart
   would each make the map, and one device's fields would be lost.
2. **Hours.** Plain hours (`[@hours]*rate` just works), or Excel's fraction of a day (a
   paste from Excel stays exact, but rates need `*24`). The mockup uses plain hours.
3. **A1 references** inside a table (`B2`, `C2:C9`). The mockup has none; names are
   enough there. The question is whether they should exist beyond translating pastes.
4. **One cell with its own formula** in a formula column. Excel Tables allow it and flag
   it. The mockup leaves it out.
5. **`GROUPBY`'s totals row.** Excel adds a grand total by default (`total_depth`). The
   mockup adds none unless asked. Match Excel, or keep it off for small tools?

## Decisions (Joe, 2026-10-19)

- Small named tables, not a grid. Tables, formula columns, link columns and summaries.
- No special scratch or current rows: every row is ordinary.
- Kept separate from the rich text's tables.
- They work like pictures: a sheet node, and sheets inside text.
- A sheet inside text is a live component, rendered through rinch node views. Node views
  are top priority, generic and reusable. Boards in text move onto them later.
- Excel-compatible formulas, or at least Google Sheets level.
- The column setting is called "type".
- Link-to-a-node columns choose from the nodes under a parent, and can create one there.
- Cross-note references use `'Note'!Table[column]`, stored by id and shown by name. Names
  are unique only where they can be, and titles never have to be.
- Copy and paste are wanted, in layers.
- Full MCP support: everything a person can do with a sheet, an LLM can do too.

## Built in wave 2 (2026-10-20, branch `sheets`, on rinch `main` fc14187)

- **The atom.** `pimble_crdt::Block::Embed { attrs }` reads and builds rinch's `embed` by
  its `data-*` attributes, so a note holding a table still reads as blocks, text, index
  units, links and Markdown (the embed as `<div data-pm-type="embed" data-sheet="…">`,
  which rinch's Markdown reads back).
- **The session.** A pane holding a note holds a sheet session over the note's document
  too (`sheets::text_opened`, from `start_editing`; `in_text: true`): the editor's own
  typing reaches it (`sheets::text_delta`, from the outbound closure), its table edits go
  to the editors holding the note, and when it stops it merges its copy into the node
  cache after the editor's. Subscription and reconcile are the editor's.
- **The view.** `sheets/in_text.rs` registers a `NodeViewSpec` for `embed` atoms with a
  `data-sheet` on every pane's editor, drawing the same `table_node` a sheet node uses
  from the pane's `SheetUi`. Keys in a table come through the view's `on_key` (the editor
  offers them there before the keyboard interceptor a sheet node's typing uses): Enter,
  Tab, arrows, Escape (drops the cell's edit and gives the keyboard back to the text,
  caret after the table), Ctrl+Z/Ctrl+Y (the sheet's own undo). A press inside a table
  marks itself (`pane_view::press_in_view`) so the pane does not hand the keyboard to the
  editor.
- **Edit > Insert Sheet**: a table and its atom at the caret, with a paragraph after it
  when it ends the text (with nothing after, Escape node-selects the table and the next
  key types over it).
- **The table's "×" in text** takes its atom out; "Tables no longer in the text", under the
  note, lists every table no atom shows, with Put Back (at the caret) and Delete.
- **Move** (Convert to Sheet Node): `SheetsTxn::copy_table` copies the table, ids kept,
  into a new sheet node under the note; when the node is made, the table's place in the
  text becomes a link to it and the note lets go of the table. A table that reads another
  of the note's tables or is read by one (`TableSnap::tables_read`) is refused with a
  sentence until cross-note references (wave 3). The reverse is not built.
- **MCP**: `create_table` takes a text node (the table and its atom in one edit, after
  `after_text` or at the end); `get_node` renders a text node's tables where their atoms
  are, and lists the rest under "Tables no longer in the text". `move_table` is not built.
- **Checked** in a release build on a scratch store (desktop: insert, typing, a formula
  typed into an empty column, the column form, Escape, the lost list and Put Back,
  Ctrl+Z, a restart, an MCP `create_table` arriving live) and in Chromium on the local
  stack in an encrypted store (insert, typing, rename, a reload and unlock, Move). Tests:
  `sheets::in_text::tests`, `pimble-crdt` `an_embed_reads_back_and_every_reader_copes`,
  `pimble-sheet` `a_copied_table_keeps_its_ids_and_contents` and
  `tables_read_names_this_notes_tables_only`.
- **Gaps**: no summary can be inserted in text (Insert Sheet makes a plain table); a
  table's edits made between pressing Move and the node arriving stay in the note's copy
  only. (The name field and `move_table` were done in wave 3's first commits.)

## Built in wave 3 (2026-10-20, branch `sheets`), all but node fields

- **Moves keep references.** `formula::rehome` rewrites stored formulas when a table moves
  to another note: its own formulas name the note it left (`moved_out`), the tables left
  behind name it in its new note (`left_behind`), and the new note's tables that read it
  there read it as their own (`arrived`); string literals are left alone. A moved table
  and the ones around it compute what they computed before (test
  `a_moved_table_computes_the_same_on_both_sides`). The app's Move and the MCP's
  `move_table` (within a node, or into another node either way, the copy first) both use
  it; only a row link still ties a table to its note.
- **Notes a formula reads** (`sheets/reads.rs`): each resolved once (`resolveLink`); a
  deleted one reads `#REF!` "… was deleted", one this account cannot read "You can't read
  …", one not on this device "Loading…", one that became another (a move out of a share)
  reads the note it became. While read it is subscribed and fetched fresh, counted by
  `AppStore::is_editing`, a peer's update of it is merged into its cached copy so the
  sheets reading it recompute, and it is let go when nothing reads it. A pane's live copy
  of a note beats the cache. Not built: "From your offline copy of …" (the link state is
  not consulted).
- **Link-to-a-node columns**: "Nodes under" in the column form (a title or path, resolved
  as formulas name notes; empty for anywhere); typing in such a cell fetches the parent's
  children for its choices; a name that matches none offers `Create "X" under Parent`,
  which makes the node there and links the cell to it when it arrives (siblings' field
  names wait on node fields). A linked node not held is fetched so its title shows. The
  MCP takes `under` and node values (a pimble: link, or a child's title).
- **Search**: `pimble_sheet::index::index_units`, per table a name unit, a header of its
  typed columns and a row per row; formula columns and summaries stay out. The document
  plugin adds them after a note's text, a new `SheetPlugin` indexes sheet nodes, and the
  browser's search of encrypted stores reads the same units. A table's node links are
  links in the index (`index::linked_nodes`), so backlinks include rows that point here.
- **MCP `rows_pointing_here`**: every row in every open store linking to a node, grouped
  by table, with totals of its number columns (it walks the stores; the backlinks index
  is for the node page, whose UI is deferred with backlinks).
- **Wave 1 bugs found on screen**: link chips and the summary button used
  `--rinch-primary-color-filled`, which rinch does not define (a linked row or node was
  invisible); the table's scroller clipped a link cell's choices.
- **Checked** in a release build on the scratch store (Move with a formula reading the
  moved table, `move_table` back into the text, a node link set by the MCP and shown,
  Create from a cell, the column form) and by tests (`sheets::in_text::tests`, pimble-sheet
  `rehome` and `index`), and in Chromium on the local stack in an encrypted store (a
  link-to-a-node column under an untitled note, its children as choices, Create from a
  cell). That pass found that an untitled note's name (its first words, as the tree shows
  it) was not what formulas and "Nodes under" matched; they now match what the tree shows.
- **Node fields** (decision 1, Joe 2026-10-20): `node` root keys `field:<name>`
  (`pimble_crdt::FIELD_PREFIX`, `NodeDoc::set_field`, `node_fields`, and
  `field_update`, an update made on a blank document so a client that does not hold the
  node can write one), read into `NodeMetadata::fields` and so into every `Node`; a field
  change is `MetadataUpdated`. Formulas read them (`'Packing'!rate`, `[@where].rate`) in
  the app and the MCP. The app shows them in a band under the pane's title strip
  (`fields.rs`: edit in place, Enter writes; ×; "+ Field"; Edit > "Add Field"). A node
  made from a link cell gets its new siblings' field names, empty. Search indexes them
  (`name: value`). MCP `get_fields`, `set_fields`, and `get_node` lists them. Checked on
  the desktop (the band, a rate changed there recomputing `[@where].rate` in another
  note, Add Field) and in Chromium on the local stack in an encrypted store (Add Field,
  a reload and unlock).

## Built after wave 3 (2026-10-20): sheet nodes embedded in text

- The window holds a session per sheet node its texts embed (`sheets.rs` `Slot::Embed`,
  `sync_embeds`), shared by the panes showing it, subscribed unless held already, and
  counted by `AppStore::is_editing`. A pane's tables are its own plus those of the sheet
  nodes its text embeds (`TableView::node` says whose); a table action aims at the slot
  holding its table (`aim`), so edits, the formula bar, typing and undo go to the sheet
  node's document. The table's "×" in an embed takes the embed out of the text (the sheet
  node keeps its tables).
- `pimble-crdt` counts any `embed` with `data-node` as a link to that node.
- The MCP's `get_node` shows an embedded sheet node's tables where it is.
- Found while checking it: a node field written from a blank document did not replace an
  existing value (it merged as concurrent, and the old value can win); the app now
  writes on the node's document as this window has it, and its cache takes the update.
- Checked in a release build: Insert Sheet from a Node, typing in the embed reaching the
  sheet node, a restart, Move leaving an embed (tests), the lost list ignoring an
  embed's tables; and in Chromium on the local stack in an encrypted store (Insert Sheet
  from a Node, a row typed in the embed found in the sheet node). Not built: dropping a sheet node's tree row on the
  text, and pasting its link, as boards have.

