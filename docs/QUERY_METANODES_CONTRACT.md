# Query metanodes contract: nodes that gather other nodes

Status: drafted 2026-11-04 from Joe's design in conversation; Joe: "build it", the
decisions of section 9 taken as recommended (its own store; a drop makes an own
child; found rows expand). This is `docs/METANODES_CONTRACT.md` wave 3 ("Query sources"),
shaped by what Joe decided on 2026-11-04 (section 1).

## 1. What Joe decided (2026-11-04)

- "I picture generic metanodes as containing linked nodes as their children."
- **Both in one**: a metanode has children of its own (made or dropped there, as a
  kanban's columns are) and the nodes its query finds, shown with them.
- **A found node's place cannot be changed from there**: Delete, Cut and the like are greyed
  out on it; nothing is dropped into it ("an invalid target").
- **A new node made inside a metanode lives inside it**: a real child.
- **The pane shows the metanode's setup.**
- **Metanodes look different** from linked nodes: "maybe purple" (section 6 works it through).

## 2. The model

A query metanode is an ordinary node with `metanode:viewer = "list"` (a viewer built into
the app, section 5) and a source that is a query. It has two kinds of children:

- **Its own**: stored in its `children` list like any node's, in their order. Made there,
  dropped there, moved, deleted, cut and pasted as anywhere.
- **Found**: the nodes the query matches, evaluated live by the API from what it holds and
  the search index, never stored. No list, no copy, no link node per match: the rule that
  nothing is "a copy someone keeps in step" (CLAUDE.md) is why. Each is the real node,
  addressed as the node it is, so opening, editing and renaming it change the original.

A node that is both its own child and found shows once, as its own. A metanode never finds
itself or a node above it. A found node that is itself a query metanode shows its own
children and its finds one level down only (no recursion through finds).

## 3. The query

The source (`metanode:source`, JSON, co-edited like any key) gains one query kind beside
`children` and `subtree`:

```
{ "kind": "match",
  "within": { "store": "<id>" } | { "under": "<store>/<node>" } | "everywhere",
  "all": [ <condition>, ... ],          // every condition holds
  "order": "title" | "modified" | "created" | { "field": "<name>" },
  "descending": false,
  "limit": 50 }
```

Conditions, in waves (section 8):

- `{ "tag": "waiting" }`, `{ "no_tag": "done" }`
- `{ "text": "invoice" }`: the search index's keyword match (title and text, stemmed)
- `{ "field": "Due", "is": "<"|"<="|"="|">="|">"|"set"|"empty", "value": "Friday" }`, values
  read as a person types them (`pimble-sheet`'s parsing: dates, numbers, text)
- `{ "links_to": "<store>/<node>" }`: backlinks
- `{ "type": "document" | "canvas" | "sheet" | "discussion" | "kanban" }`

`within` defaults to the metanode's store. An older Pimble reads `match` as `Query::Later`
and shows the metanode's own children with `NEWER_SOURCE`, as today.

**Evaluation** is `Pimble::metanode_source`, extended: the forest's entries gain
`found: bool`. Text and backlinks come from the search index (`pimble-search` already holds
title, text, tags and `links`); tags and types from it too; fields need indexing first
(section 8, wave B). In the browser an encrypted store is searched client-side as it is now.
A member of a share finds only what they can read. The result is kept live as forests are
now (`Forest::touched_by`), widened: a change to any node's tags, fields, title, text or
links in `within` reads the query again, debounced.

## 4. In the explorer

- The metanode's row expands to its own children, then its finds in the query's order.
- A found row's tree value is a path address through the metanode, as a mount's children's
  are (`node_{s}_{n}/{ms}_{mn}`), so the same node can be found by several metanodes and
  sit in its own place, each its own row; the row's data marks it found.
- **What a found row allows** (one rule, in `pimble_core::plan`, so the app, plugins and the
  MCP agree): the node itself changes; its place does not.
  - Allowed: open, edit, Rename, Appearance, Discuss, Copy, New (inside it: a real child of
    the real node), and everything below it (its own children are ordinary).
  - Refused, greyed in the menu with the reason as the menu's top line: Delete, Cut, a drag
    of it, a drop into it or beside it, Alt+Shift moves, Paste into it. The line: "Shown
    here because it is tagged #waiting. It lives in Personal / Projects."
- Dropping a node onto the metanode itself makes it one of its own children (a move), as
  for any node. (Making a dropped node match instead, adding the tag, is section 9, decision 3.)

## 5. In the pane: the setup

Opening a query metanode shows its setup, built into the app (both targets), not a plugin:

- **Show nodes**: one line per condition, each a small sentence with fields: "tagged
  [waiting]", "not tagged [done]", "containing [invoice]", "with [Due] [before] [Friday]",
  "linking to [Pick a node...]", "that are [canvases]"; "+ Add a condition".
- **From**: this store, under [a node], or everywhere.
- **Order**: by title, last changed, created, or a field; ascending or not; at most [N].
- **What it finds now**: the count and the first rows, live, each opening its node.
- Every change writes the source through the one write (`Pimble::edit` of
  `metanode:source`), so two people editing a setup merge like any edit.

A kanban metanode's pane stays its board; the setup is for the `list` viewer.

## 6. How they look (Joe: "maybe purple"; worked through 2026-11-04)

Three things to tell apart: a metanode, a node found in it, and a mount.

- **A metanode is purple.** Its icon (Tabler `filter-search` by default) in the theme's violet
  (`--rinch-color-violet-6` light, a lifted violet dark, through `display_color` as every
  color is), so purple means "gathered here", as blue means "mounted here". A color the person
  sets on it wins, as on any node, and a small violet corner mark stays on its icon.
- **A found node keeps its own icon and color**: it is the real node and looks like itself.
  Two violet cues tie it to its metanode: an arrow (Tabler `arrow-up-right`) at the row's
  end, which says "lives elsewhere", with a tooltip saying where and why; and the tree's
  guide line beside the finds drawn violet, so they read as one group under their own
  children. No heading row.
- **A mount stays as it is**, its link icon in blue.
- **Not by color alone**: the arrow and the line are shapes; the purple adds to them.
- Both themes; nothing purple is stored on any node (render-time only).

## 7. In the API, the MCP and elsewhere

- `pimble_core::metanode`: `Query::Match` and the condition types; `Source::describe` says it
  in words ("the nodes tagged waiting in Personal, newest first, at most 50").
- `pimble_api`: the evaluation; `Forest` entries' `found`; `plan`'s found-row rule.
- MCP: `set_metanode` takes a query (`within`, `all`, `order`, `limit`); `get_metanode`
  lists finds after own children, each marked found.
- New ▸ gains the kind (section 9, decision 1 names it); a new one opens its setup.

## 8. Waves

A. **Core and MCP**: `Query::Match` with tags, no-tags, text, type and links-to; evaluation
   from the search index; `found` entries; the MCP. Tested against a store with tags.
B. **Fields**: indexing node fields (`field:<name>`) in `pimble-search`, and field conditions
   and order.
C. **Explorer**: found rows, the rule (greyed items and the line), the violet icon, arrow and
   guide line, liveness.
D. **Setup pane and New ▸**, both targets.
E. **Checked** in the release desktop app and the browser: a plain store, an encrypted one,
   a shared one as a member.

## 9. Decisions (taken as recommended, 2026-11-04)

1. **What New calls it**: "Metanode" (Joe, 2026-11-04: "we agreed on terminology earlier,
   it isn't 'smart list', it's just Metanode"). The options drafted were "Smart List",
   "Smart Folder", "Query" and "Collection"; none of them is used.
2. **Where it looks by default**: its own store (recommended), or everywhere.
3. **A node dropped onto a query metanode**: becomes one of its own children (recommended:
   one meaning for a drop everywhere), or is changed to match (given the tag).
4. **Do found rows expand** to show their own children: yes (recommended: they are the real
   nodes), or found rows are leaves.

## Built: waves A and B (2026-11-04)

What is built, on `master`:

- **Core** (`crates/pimble-core/src/metanode.rs`): `Query::Match { within, all, order,
  descending, limit }` beside `Children` and `Subtree`, in the section 3 JSON shape. `Within`
  (`{"store"}`, `{"under"}`, `"everywhere"`; absent is the metanode's store), `Condition`
  (untagged: `Tag`, `NoTag`, `Text`, `Field { field, is: FieldOp, value }`, `LinksTo`,
  `Type`, and `Later(Value)` for a condition from a newer Pimble, kept as written), `FieldOp`
  (`<`, `<=`, `=`, `>=`, `>`, `set`, `empty`), `Order` (`"title"`, `"modified"`,
  `"created"`, `{"field"}`), `DEFAULT_LIMIT` 50, `MAX_LIMIT` 500. `Source::matching`,
  `is_match`, `limit`, `describe_named` (names through a callback: "the nodes tagged waiting
  in Personal, newest first, at most 50"), `found_reason` ("it is tagged #waiting and it
  isn't tagged #done"), `parse_node_ref` (`<store>/<node>` or a `pimble:` link). `Query` is
  no longer `Copy`/`Eq`. An older Pimble reads a match as `Query::Later`; a match holding an
  unknown condition finds nothing here and says `NEWER_SOURCE`.
- **Plan rule** (`crates/pimble-core/src/plan/found.rs`): `FoundRowAction` (`allowed()`:
  open, edit, rename, appearance, discuss, copy, new inside; not delete, cut, drag, drop
  into or beside, keyboard move, paste into), `lives_in(&[titles])` ("Personal /
  Projects"), `found_refusal(reason, lives_in)` ("Shown here because it is tagged #waiting.
  It lives in Personal / Projects.").
- **Evaluation** (`crates/pimble-api/src/metanode.rs`, `matching.rs`): `Pimble::metanode_source`
  answers the own children (depth 1, `found: false`) and then the finds (`found: true`, depth
  1, no parent, no children in the forest). Finds are judged locally over held documents,
  not through the search RPC: a store is held whole (`hold_store`) and walked from its
  shown tops (or the `under` node) through each live list, a store's own nodes only (a mount
  is judged as a node and not entered); in the browser an encrypted store is walked through
  its backend. Text is every word of the condition, any case, in the title and plain text
  (`NodeDoc::text`, substring, so "invoice" finds "invoices"; not stemmed); links are
  `NodeDoc::links` (content and canvas cards); field values are read as a sheet reads typing
  (`pimble_sheet::input::parse_input`: a number, else a date relative to today, else text
  in any case), and values of different kinds never compare; tags compare without `#` in any
  case; `canvas` is the `board` type (and the old `canvas`, `image`), `kanban` a metanode
  with that viewer. Never found: tombstones, the metanode, its ancestors, its own children
  (a node that is both shows once, as its own). Sorted by the order (a field order puts
  nodes without the field last either way), then title, then id; cut to the limit.
  `Forest::found()`, `is_found`, `reason`; `Pimble::describe_source`, `lives_in`,
  `found_refusal(&forest, addr)`; `judge_in_metanode` refuses a found node as moved,
  deleted or as a move's target with the refusal sentence.
- **Liveness**: `Forest::touched` counts any document named in a store the match looked in;
  `touched_by` skips a `ContentUpdated` there unless the match reads content (a text or
  links condition, or the last-changed order) or the node is shown. A viewer debounces.
- **MCP**: `set_metanode` takes `query` (viewer `list` unless named); `get_metanode` lists
  own children, then finds marked `[found, in Store / Parent]` (docs/MCP_CONTRACT.md).
- **Tests**: `pimble-core` `metanode::tests` (`a_match_round_trips_in_the_contracts_shape`,
  `a_match_says_itself_in_words`, `an_older_reader_sees_a_match_as_a_newer_query`,
  `a_newer_condition_is_kept_and_named`, `node_refs_read_both_ways`) and
  `plan::found::tests::a_found_row_changes_itself_not_its_place`; `pimble-api`
  `matching::tests::values_read_as_typed` and `tests/api.rs`
  `a_query_metanode_finds_nodes_after_its_own_children` (every condition kind, the four
  orders, descending, limit, under / another store / everywhere, the exclusions, own first
  and once, a newer condition, the refusal sentence and `judge_in_metanode`, liveness);
  `pimble-mcp` `a_query_metanode_is_set_and_lists_its_finds`.

Not done, or done differently from sections 3 and 8:

- Section 3 says text, backlinks, tags and types come from the search index, and wave B
  indexes fields in `pimble-search`. Evaluation reads the held documents instead: the index
  is server-side, keyword-only through `search`, and absent for an encrypted store in the
  browser, while every client already holds the documents, so one evaluation serves every
  backend. The index is untouched. A very large store pays a walk per evaluation (the text
  of a node is read only when its cheaper conditions hold).
- Text matching is a case-insensitive substring per word, not stemmed.
- A found node that is itself a query metanode is shown as a plain entry; the
  "one level down" rule (section 2) is the explorer's, wave C.
- Store roots and the `under` node itself are never found (the walk starts below them).

## Built: waves C and D (2026-11-04)

What is built, on `master`. The person sees one word for it: **Metanode** (decision 1).

- **Explorer** (`crates/pimble-app/src/state.rs` `structural_node`, `metanodes.rs`): a query
  metanode's row has its own children, then its finds (`AppStore::finds`). A found row's tree
  value is a path address through the metanode, as a mount child's is
  (`node_{s}_{n}/{ms}_{mn}`); its own children carry the found row too
  (`.../{ms}_{mn}/{s}_{n}`), so a node can sit in its own place, in several metanodes, and
  below a found row without two rows sharing a value. Each build registers its found rows
  (`state::found_row(value)`, `first_found`: the metanode, its row, where the node lives and
  the refusal), and the row's `TreeNodeData` label carries `found: <sentence>` so it
  re-renders when that changes. Found rows expand to their own children (decision 4); a
  found query metanode shows its own finds one level down, and a metanode found two finds
  down shows none (`finds_crossed`). The forest is asked when a query metanode's row loads
  its children (`metanodes::children_loaded`), kept with the panes' forests, and read again
  when `Forest::touched_by_view` says so, debounced 300 ms (`REREAD_MS`); a node whose source
  becomes a match starts showing finds. `BackendEvent::MetanodeRead` carries `lives`, where
  each found node lives (`Pimble::lives_in`), so the sentence is the API's
  `found_refusal`'s.
- **Rules on found rows** (all through `pimble_core::plan::FoundRowAction`): the row menu's
  top line (`pimble-menu-note`) is the sentence ("Shown here because it is tagged #waiting
  and it isn't tagged #done. It lives in Desk test / Kanban / Done."), and Cut, Paste, Mount
  Store Here..., Mount Remote Store Here... and Delete are greyed. `tree_drop::found_refusal`
  (inside `plan_drop`, the one judgement of a drop) refuses a found row dragged, a drop into
  one and a drop beside one (`TreeSlot::After(found)`), with its sentence in the strip; a
  drop onto the metanode is an ordinary move to an own child. Keyboard: Alt+Shift moves go
  through `plan_drop`; Delete (`tree_view::deletable`), Ctrl+X and Edit > Cut Node
  (`tree_keys::set_cut`), and Paste / Ctrl+V into one (`pasting::paste`, `paste_straight`)
  say the sentence as a notice. "+", File > New and Ctrl+Shift+Enter beside a found row make
  an own child of its metanode, last (nothing is put at a found row's place); New inside a
  found row makes a real child of the real node.
- **Look** (section 6): a query metanode's icon is Tabler `filter-search` (also
  `pimble_ui::appearance::node_icon`) in violet (`gathered_violet`: `--rinch-color-violet-6`
  light, `--rinch-color-violet-4` dark), unless the person chose an icon; a color the person
  sets wins, and then a small violet dot sits on the icon (`pimble-query-mark`). A found row
  keeps its icon and color and gets a violet `arrow-up-right` at its end, titled "Lives in
  Desk test / Kanban / Done". rinch's Tree draws one guide line per subtree for every child
  alike and cannot style it per row, so the violet line beside the finds is the app's own:
  a 2px span in each found row at the parent's guide column
  (`calc(var(--tree-level-offset) * (level - 1) + var(--tree-guide-offset))`), drawn over the
  Tree's guide. rinch is unchanged. Nothing is stored.
- **Setup pane** (`crates/pimble-app/src/metanode_setup.rs`, both targets): a metanode
  with the `list` viewer shows its setup in place of the outline (`MetanodeUi::setup`). Show
  nodes: one line per condition ("tagged [ ]", "not tagged [ ]", "containing [ ]", "with
  [field] [op] [value]" (the value hidden for set / empty), "linking to <node> [Pick a
  node...]", "that are [kind]"), each with a remove button, and "+ Add a condition" (a menu
  of the six kinds). From: this store, under a node (with the picker), everywhere (and
  "In <store>" when the source names another store). Order: title, last changed, created,
  a field (named in a field); ascending or descending; at most N (1 to 500). Then "It finds
  N nodes now." and the first ten finds, each opening its node in the pane. The node
  picker is the setup's own: a field searching through `Search` (`SearchPurpose::MetanodeSetup(pane)`)
  and a list of hits; the editor's link picker is tied to an editor's caret. Every change
  writes the source through `BackendCommand::SetMetanode` (`Pimble::set_metanode`, an edit
  of `metanode:source`); typing is written once it rests 400 ms, choices at once, and a
  source arriving while this pane's own write is on its way does not overwrite what is
  being typed (taken once it matches, or after 3 s: someone else's write won).
- **A blank condition finds nothing** (`Condition::is_blank`, `Prepared::Nothing` in
  `pimble-api` `matching.rs`): a line still being filled in, so a new metanode does not
  list its whole store. The setup says "Fill in each condition to find nodes." A match with
  no conditions at all still finds every node (a "recently changed" list by order and limit).
- **New ▸ "Metanode"** (`creating::Kind::Metanode`, so "+", the row menus' New and File >
  New): `pimble_api::recipes::query_metanode`, a metanode with viewer `list` and a match
  over its own store (`within` absent, which section 3 defines as its own store, so it keeps
  meaning that if the node moves to another store) whose one condition is a blank "tagged"
  line; named "Metanode", renamed in place wherever it is made (the row menus' New too),
  and opened in the focused pane (`state::open_when_created`), which shows its setup.
- **Tests**: `pimble-app` `state::tests::a_query_metanode_shows_its_finds_after_its_own_children`,
  `a_found_rows_children_are_ordinary`; `tree_drop::tests::a_found_rows_place_is_not_changed_by_a_drop`;
  `metanode_setup::tests` (`a_setup_edits_its_source_as_a_match`,
  `from_order_and_limit_read_as_chosen`, `the_count_line`); `pimble-api` `tests/api.rs`
  `a_query_metanode_finds_nodes_after_its_own_children` gains blank conditions.
  `pimble-app --lib`: 212 pass; `pimble-core --lib` and `pimble-api` pass; the web build
  compiles.
- **Checked in the release desktop app** (port 7473, scratch dirs, four nodes tagged
  `waiting` through a scratch agent's `pimble-mcp`): "+" listed Metanode; it made a violet
  `filter-search` row "Metanode" under the store row and renamed it in place ("Waiting");
  the setup showed "tagged [a tag]" and "Fill in each condition to find nodes."; typing
  "waiting" found Bravo, Delta, Echo and Sheet; adding "not tagged done" dropped Echo; the
  expanded row showed Bravo, Delta and Sheet with violet arrows and a violet guide line; the
  found row's menu had the sentence on top and Cut, Paste, both Mounts and Delete greyed; a
  drag of Delta onto Doing showed the sentence in red in the strip and moved nothing; Delete
  on the selected found row said the sentence as a notice and deleted nothing; tagging
  Charlie through MCP brought it into the row and the setup within about a second; the
  light theme drew the same violet; "Linking to a node...", "Pick a node...", typing
  "Bravo" and picking it named Bravo on the line and the count said "It finds nothing now."

Not done, or done differently:

- A new metanode starts with one blank "tagged" line, not with no conditions (a match with
  none finds everything, which for a new node would be its whole store).
- The arrow's "why" is in the menu's top line and the refusals; the tooltip says where it
  lives only.
- Not tried in the app: the corner mark on a colored metanode, a found metanode's own finds
  and the one-level rule (unit-tested), Alt+Shift moves and Ctrl+X on a found row (they go
  through `plan_drop` and `set_cut`, unit-tested for drops only), the browser (wave E), an
  encrypted or shared store.
- The pane's title bar still draws the node's type icon (a document) for a metanode.
- A query metanode with no own children whose conditions are blank has nothing to show,
  so its row has no expander until it finds something.
