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
  headers: Map<"k:<key>" | "<index>", String>  a summary's renamed result columns, by key (section 4)
  columns: Map<column id, Map>
    name, type, order (fractional),
    target  (link to a row: the table id),
    under   (link to a node: the parent node's (store, node), or absent for anywhere),
    formula (formula column: the stored formula, section 6),
    format  (formula column: automatic | number | money | date | hours | percent),
    width   (Number | absent: the column's width in pixels, section 4d)
  "width:<result key>": Number | absent   a summary's result column's width (section 4d)
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
- **A sheet node has its own toolbar** (Joe, 2026-10-22: not the text's), in the text
  toolbar's place while the focused pane shows one, as a board's does
  (`sheets/toolbar.rs`). Sheet: Add Table, Add Summary Table. Row: Add Row (the caret in
  the new-row line), Move Row Up, Move Row Down, Delete Row. Column: Add Column, Column
  Settings, Group by This Column, Move Column Left, Move Column Right, Delete Column. Table: Add Summary Row, Rotate, Collapse or Expand All Groups,
  Delete Table. History: Undo, Redo. The row, column and table buttons act on the cell
  last pressed, or on the sheet's only plain table before any is; a button with nothing
  to act on is dimmed. A text node's tables keep the text toolbar.
- **Rows and columns move by dragging** (Joe, 2026-10-22, the Google Sheets way, with a
  grip on both): a row's grip shows in a narrow gutter before the first column while
  the pointer is over the row, a column's at the top of its header while the pointer is
  over the header; a plain table's row then follows the pointer, lifted in the ghost's color, and the rows
  it passes slide aside to make room (every row is 31px, so where it lands is reckoned
  from the pointer, `moving::RowDrag`); a column (and a rotated table's record or field)
  shows in the ghost's color with a line where it will land. The drop writes its
  `order` (`move_row`, `move_column`). No grip shows while a drag is under way. A rotated
  table's record grip is beside its delete button, a field's before its name. A row
  moves only among the rows of its own group (a drag never changes a cell), one row or
  column at a time. The toolbar's Move Row Up/Down and Move Column Left/Right step the
  last pressed cell's row or column one place (`sheets/moving.rs`). Clicking a cell
  still types in it and clicking a header still opens its settings.
- **Typing happens in the cell** (Joe, 2026-10-21: no box that jumps out and resizes
  things): a click puts the caret in the cell's own text, which stays where it was; the
  cell takes a faint tint, and nothing changes size, so the rows and columns stay put.
  The edit box lies over the cell with the cell's text hidden under it.
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

**A result column's key** (Joe, 2026-10-20). A result column has no id: it is whatever
the formula gives. So it is known by its **key**, the name the formula gives it before any
rename: `category` for `Transactions[category]`, `amount` for a `GROUPBY`'s values, `name 2`
for a second column of one name, `#<position>` for one with no name. Stored formulas name
a result column by key (`{rk:<key>}`, section 6) and `headers` rename it by key
(`k:<key>`). Reshaping the formula keeps every column it still gives:
`GROUPBY(Expenses[at], …)` becoming `GROUPBY(HSTACK(Expenses[date], Expenses[at]), …)`
leaves an own column `=[@stop].[day total]` (with `stop` the rename of `at`) reading the
stop. A reference to a column the new formula no longer gives is `#REF!` ("This formula
reads the result column that came from "amount", and ByMonth's formula no longer gives
one"), and it comes back if the formula gives the column again.

- **Before keys** (wave 1) result columns were stored by position (`{r:<index>}`,
  `headers` keyed `"<index>"`), and a reshape silently pointed them at whatever column
  took that place. Those still read by position. Before any change to a summary's formula
  (the app's formula box, the MCP's `set_summary`) the note's own formulas and headers
  that still use positions are re-stored by key against the formula as it is
  (`formula::rekey_note`), in the same edit. Another note's position references are moved
  only when that note's own summary formula changes or the formula is edited; until then
  they read by position.
- An app from before keys reads `{rk:…}` as an unknown token: every device moves to this
  version together.

## 4b. Summary rows and groups (Joe, 2026-10-21)

A plain table can sum itself up: **summary rows** under its rows, and **groups** with a
subtotal under each. Joe, 2026-10-21: "regular sheets could have summary rows, instead of
needing a summary sheet"; several rows, not only Excel's one Total Row; a menu or a formula
per cell; other formulas read them Excel's way; subtotals by group too. And "if we have
this, I'm not sure we need summary tables": summaries (section 4) stay for now, since they
exist in stores and the MCP uses them, and are retired later if grouping covers what they
are used for, each converted to a grouped table where it can be.

**In the document**, each summary row is a key of its table's map (never a container made
on first use, section 2, so two devices adding a table's first summary row apart keep
both):

```
data["sheet:<table id>"]
  sum:<id>:  Map             one key per summary row
    name:    String          unique within the table, shown at its left ("Total")
    order:   String          fractional, top to bottom
    cells:   Map<column id, String>   what each column shows; absent is nothing
  group_by:  String | absent  the column id rows are grouped by
```

A summary cell is one of `sum`, `average`, `count`, `min`, `max`, or `=` and a stored
formula (section 6). The words are the menu; the formula is anything else. Each is one value
in a Map, so last writer wins per cell, as cells do.

- **What a summary row sums** is the rows above it: the whole table for a row at the
  bottom, one group for a subtotal. `count` counts the cells that are not blank; the others
  take the column's numbers and skip the rest, as Excel's `SUBTOTAL` does. A result keeps
  the column's format (`sum` of money is money; `count` is a plain number).
- **In a summary cell's formula**, `[amount]` is the column's values in that scope (the
  whole table, or the group), so `=SUM([amount])/COUNT([nights])` works at the bottom and
  in every subtotal. There is no `[@…]`: a summary row has no row of its own ("A summary row
  sums up rows; it has no row of its own to read"). Other tables and notes are read as
  anywhere else.
- **Adding a summary row** names it "Total" (then "Total 2", and so on), with `sum` in every
  number, money and hours column and in formula columns that show numbers. Rename it,
  reorder it, change any cell from the menu, or remove it.
- **Grouping**: a column's menu has "Group by This Column". Rows then show under a header
  per value of that column (the value as shown: a link's label, a date as a date), groups in
  ascending order with blanks last as "(blank)", and the rows of a group in their own order.
  Under each group come the summary rows, computed over that group (the subtotals); under
  the last group come the same rows over every row (the grand totals). A table with no
  summary rows shows the group headers alone. Any column can group, formula columns
  included: a column `=TEXT([@date],"yyyy-mm")` groups by month. "Ungroup" takes it off.
  The grouping is the table's, so everyone sees it; a new row's line stays at the bottom,
  and a row typed there goes to its group.
- **Collapsing** a group (its header's chevron) shows only its header and subtotals;
  "Collapse All" turns the table into its summary. What is collapsed is the viewer's own,
  per pane and for as long as the pane shows the table, never in the document.
- **Reading them from formulas**, by name: `Trips[[#Total],[cost]]` is the cost cell of the
  summary row named Total, `Trips[[#Average],[cost]]` the one named Average (a `#` part that is
  not one of Excel's: `#All`, `#Data`, `#Headers`, `#This Row`). Excel's own `[#Totals]` (its
  one Total Row) reads the summary row named Total, so a formula pasted from Excel works;
  where there is none it says so. Never "the first summary row": reordering the rows changes
  no formula (Joe, 2026-10-21, "I don't know if #Totals makes any sense at all"). Stored by id, `{s:<id>}`, so renaming the row edits no
  formula. Only the bottom rows are read this way; a subtotal is the `SUMIFS` it stands for.
  A summary row may read another summary row; one that reads itself is `#CIRC!`.
- **Summary rows are not data rows**: `COUNTA(Trips[name])`, `FILTER`, a link column's
  picker and the MCP's rows never see them. Copy (section 11) copies data rows only.
- **Old apps** ignore keys they do not know: they show the table ungrouped and without its
  summary rows. A formula that reads a summary row (`{s:<id>}`) is an unknown token there,
  so every device moves to the version that has them together.
- **MCP** (section 10): `add_summary_row`, `update_summary_row` (name, cells, position),
  `remove_summary_row`, `group_table` (a column, or none); `get_table` ends with the summary
  rows (each cell's value and what it is) and, for a grouped table, each group's subtotals.

**Built (2026-10-21, branch `summary-rows`).** The document and engine (`pimble-sheet`:
`SummaryCell`, `SummaryRowSnap`, `SheetsTxn::add_summary_row` and kin, `set_group_by`;
`ComputedTable::summary_rows`, `groups`; `[#Totals]`/`[#Name]` in the parser, `names.rs`
and the engine), the app on both targets ("+ Summary Row" on a table's title, a summary row's
settings from a press on any of its cells, "Group by This Column"/"Ungroup" in a column's
settings, group headers that collapse, "Collapse All"), and the four MCP tools with the
formula guide's section. Checked in the release desktop app on a scratch store and in the
browser on the local stack (an encrypted store): totals, count, a formula over each scope,
grouping with subtotals, collapsing.

## 4c. Rotation (Joe, 2026-10-21)

Some data reads better with its headings along the rows: few records with many fields (two
trips compared, one person's details, a budget's months). A table can be **rotated**: drawn
with its fields down the left and each record as a column. Joe's agent had built one by
hand, a "criterion" column with a `SWITCH`/`INDEX` formula per cell, which is what this
replaces.

- **Only the drawing turns.** Underneath, a table keeps its fields and records: `[@miles]`
  is still this record's miles, `Trips[miles]` all of them, and links, search, sync and the
  MCP's rows are unchanged.
- **The table's setting** (Joe): `rotated: true` on the table's map, absent otherwise, one
  value with the last writer winning, so everyone sees it rotated, as with grouping. "Rotate
  Table" on the table's title row turns it on and off. Old apps ignore the key and draw the
  table unrotated.
- **Along the top**, each record's first field (Joe): its first field's row is the header
  row, the same label a link to the record shows, edited in place. Down the left, the field
  names, a press on one opening that field's settings as a header does now.
- **Adding**: a record is the "+" column at the right (its first cell "Add a record…"); a
  field is the "+" at the bottom of the names.
- **Formula fields** are shaded rows; **summary rows** are summary columns at the right,
  their name in the header row; **groups** are bands of columns under a header that spans
  them, each followed by its subtotal columns, collapsing the same way.
- **Keys**: Enter (and ↓) goes to the record's next field, Tab (and →) to the next record's
  same field, as the screen reads.
- **Copy** (section 11) copies what is seen: a rotated selection pastes rotated.
- **MCP**: `rotate_table` (on or off); `get_table` says a table is drawn rotated, and still
  lists records as rows.

**Built (2026-10-21, branch `summary-rows`).** `TableSnap::rotated`,
`SheetsTxn::set_rotated`; "Rotate Table"/"Unrotate" on a table's title row; the rotated grid
(`sheets/ui.rs` `rotated_grid`: a fixed first column for the field names and implicit columns
after it, every cell pinned to its field's grid row, so a collapsed group's hidden record
columns leave no gaps); keys swapped as the screen reads; `rotate_table` in the MCP and the
guide. Checked in the release desktop app (records as columns, summary columns, a new
record typed in with Enter going to its next field, grouped and collapsed, unrotated again)
and in the browser on the local stack (rotated, a summary column). Copy waits for copy itself
(section 11, wave 4).

## 4d. Layout (2026-10-21)

- **A column's width is the table's**, so everyone sees the same table: a plain number of
  pixels, `width`, on the column's map (no container made on first use; last writer wins
  per column, so two people resizing one column apart end with one of their widths, and
  resizing different columns keeps both). A summary's result columns have no map of their
  own: theirs is `width:<result key>` on the table's map (`k:<key>`, or a position for a
  column written before keys), the same key a rename uses. Whole pixels, 40 to 2000. A
  column with no width is as wide as its contents, at least 90px (what every table did
  before widths). `SheetsTxn::set_column_width` / `set_result_width`; `None` clears it.
- **Resizing**: drag a header's right edge (a 7px grip; a press on the rest of the
  header still opens the column's settings). The column follows the pointer while the
  drag lasts, in this pane only (`SheetUi::resizing`), and the width is written once,
  when the drag ends, as one undoable edit. rinch's absolute `Drag` on both backends; the
  starting width is the header cell's bounds in the press's click context.
- **A cell's text stays in its cell**: a cell, a header, a summary row's name and value
  and a group's label are one line, cut short with an ellipsis, never drawn over the next
  column; the cell's tooltip holds the whole text (or its error). The cell itself does
  not clip, so a link cell's choices still hang below it.
- **The title row is one line.** The name, the kind, the row count and the buttons keep
  their size and place; the formula bar (and a summary's formula) takes the width that is
  left and scrolls inside itself, so a long formula never wraps the row, pushes the grid
  or draws over it. The formula bar's sentence is cut short too, all of it in its tooltip.
  The grid scrolls sideways inside the table when its columns are wider than the pane.

- **A rotated table** (section 4c) is drawn as wide as its contents for now: a field's width
  is a column's, and rotated the record columns have none of their own yet, so resizing
  waits for a width that belongs to the rotated drawing.

## 5. Names, and what is unique

- **A table name** is unique within its note when it is set: a second "Expenses" in the
  same note is refused with a sentence. A merge can still produce two (two people adding
  one offline). Nothing breaks, because formulas point at ids. The note says "Two tables
  here are called Expenses" until one is renamed.
- **A column name** is unique within its table, by the same rule.
- **A summary row's name** is unique within its table, by the same rule (section 4b), and
  none is one of Excel's names for a part of a table. A merge can leave two of one name; a
  formula that names a row points at its id, so it keeps reading its row, and the table
  says "Two summary rows here are called Total; rename one". `[#Totals]`, which names the
  row called Total, answers that sentence while there are two, rather than guess.
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
    `LEN`, `UPPER`, `LOWER`, `TRIM`, `VALUE`, `REPT` (2026-10-20: text bars, `REPT("▇", n)`)
  - dates: `TODAY`, `DATE`, `YEAR`, `MONTH`, `DAY`, `WEEKDAY`, `EOMONTH`, `EDATE`, `DAYS`
  - numbers: `ROUND`, `ROUNDUP`, `ROUNDDOWN`, `ABS`, `INT`, `MOD`, `SQRT`, `POWER`
- **Dates are Excel serial numbers**, so `TODAY()-7`, `">"&EOMONTH(TODAY(),-1)` and
  `DATE(2026,13,1)` behave as in Excel.
- **An automatic formula column takes its format from its formula** (2026-10-20), as
  Excel gives a new formula the format of what it refers to (`formula/infer.rs`): a
  column shows its own format, a field the one its text was typed in (an ISO date,
  `$120`, `15%`), and a row link's `.column` that column's. A date plus or minus a number
  stays a date (`'Trip'!trip_start + [@day] - 1`, `[@start] + 7`); a date minus a date is
  the days between, a plain number. `*` and `/` keep money, and a side's format when the
  other is a plain number or a percent. The date functions answer dates, and `SUM`,
  `MIN`, `MAX`, `ROUND`, `IF`, `IFERROR`, `XLOOKUP`, `INDEX`, the `…IFS` family and
  their kin pass on the format of the values they return. Anything else, and a column
  that reads itself, shows a plain number. A column with a format chosen keeps it.
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
names into ids on commit. Tokens: `{n:<store>/<node>}!` a note, `{t:<id>}` a table,
`{c:<id>}` a column, `{rk:<key>}` a summary's result column by key (section 4; characters
that could end the token are written `%XX`), and `{r:<index>}` one by position, from
before keys. As a result:

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

