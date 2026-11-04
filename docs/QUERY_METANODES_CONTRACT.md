# Query metanodes contract: nodes that gather other nodes

Status: drafted 2026-11-04 from Joe's design in conversation; Joe: "build it", the
decisions of section 9 taken as recommended (Smart List; its own store; a drop makes an own
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

1. **What New calls it**: "Smart List" (recommended: says it gathers, not a folder you put
   things in), "Smart Folder", "Query", or "Collection".
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
  `pimble-mcp` `a_smart_list_is_set_and_lists_its_finds`.

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
