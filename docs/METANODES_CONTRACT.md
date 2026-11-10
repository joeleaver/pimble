# Metanodes contract: a node that shows other nodes, a source and a viewer

Status: drafted by the PM on 2026-11-02 from the design conversation with Joe the same day
(memory `project_views_and_plugins_2026_11_02`) and the API refactor before it
(`docs/API_CONTRACT.md`, waves 0-5 on branch `api`). Revised the same day on Joe's model:
a metanode says **what** nodes it shows (a source: a query with filters) and **which
viewer** shows them; grouping and sorting are the viewer's, not core's. Named "metanode"
the same day (Joe: a working name; "view" is overloaded in the code: `views.rs`, rinch's
editor node views, `pimble-api`'s `view.rs`; the keys say `metanode:` because node
documents already have a `meta` root). Nothing is built. Open decisions at the end.

**Three things, kept apart (Joe, 2026-11-02):**
- **Metanodes** (this contract): a generic, reusable concept in Pimble's core.
- **The plugin system** (`docs/PLUGINS_CONTRACT.md`): how viewers (and other things) are
  provided, in plain rinch, over `pimble-api`.
- **Kanban** (`docs/KANBAN_CONTRACT.md`): the first viewer, built as a plugin. Not a
  built-in.

## 1. What a metanode is

A metanode is two things:

1. **A source: which nodes.** A query with filters, evaluated by core over the API's held
   documents:
   - this node's children, or its subtree to a depth;
   - nodes with a tag; a search; the nodes linking to a note (backlinks);
   - nodes with a field set, or a field matching a condition;
   - filters on top (not done, modified this week, of a type), and a limit that picks
     ("the 20 most recently modified": selection needs an order to choose by, so a limit
     names one; the viewer still shows what was picked however it likes).
2. **A viewer: how they are shown.** Which viewer (a plugin's), and that viewer's own
   settings (a calendar's date field, a table's sort column, a kanban's card size), stored
   on the node under the viewer's id. Grouping and sorting are the viewer's (Joe,
   2026-11-02: neither belongs with the source).

It stores **how to show** nodes, never the nodes: every gesture made through a metanode
becomes an ordinary edit of real nodes or is refused with a sentence.

### 1.1 A source answers a forest

A source's result keeps the tree relations among what it found: each node with its
parent and its children that are also in the result, **in the tree's own order**. That
order is data, not a setting: a parent's list is co-edited, and a card's place in its
column is its priority, so no source chooses it and no viewer can lose it. A viewer that
sorts (a calendar by date, a table by a column) sorts for display only. A kanban reads "this
node's children, two levels deep" as columns and cards; a calendar or a table flattens the
same answer. Without the forest, a tree-shaped viewer would have to rebuild what the source
already knew.

### 1.2 Viewers declare the sources they can use

A viewer says what shape of source it needs: a kanban, a tree two levels deep; a calendar,
nodes with a date field; a table, anything. The node's menu offers only the viewers that
fit its source, and a node whose source and viewer no longer fit (the source was changed)
shows the outline with a line saying why.

### 1.3 Core judges edits, the viewer decides what a gesture means

Grouping and layout are the viewer's: a kanban's drop means "move under this column,
after that card"; a calendar's means "set `due` to the 14th"; a table's cell edit means
"set this field". The viewer turns a gesture into an ordinary edit through `Pimble`, and
core judges it with the shared rules (`pimble_core::plan`: into itself, read-only, mount
not open, leaving a share, cross-store), so every viewer gives the explorer's refusals and
sentences. Because sources can be queries, core also says when an edit would take a node
**out of the metanode's source** ("This card won't match this metanode's filter any
more."), as a warning the viewer shows, like the leaving-a-share warning.

### 1.4 Fallback

A metanode whose viewer this app does not have shows as an outline of its source's
forest, with one line naming the viewer. Old apps, the web without the plugin, and the MCP
see an ordinary node.

## 2. Where it lives

- On the metanode, as `node` root keys like fields (`field:<name>`; no container is made
  on first use, memory `project_node_fields_decision`): `metanode:viewer` (the viewer's id) and
  `metanode:source` (the query, as a small serialized structure). Co-edited like any key:
  everyone who opens the node sees the same view.
- The viewer's shared settings go in the node's `data` under the viewer's id, as plugin
  JSON already does.
- What is one person's (a collapsed column, a size, the scroll, a calendar's month) is this
  device's, in `views.rs` beside expanded rows and canvas cameras, and never leaves it.
- A metanode whose source is "my children" is a node with children, as any node. A view
  node whose source is a query is a saved search with a viewer; it may have no children of
  its own (open decision 2).

## 3. In the API, the app and the MCP

- **pimble-api:** `Source` (the query and filter types) and `Pimble::source(node) ->
  Forest`, evaluated over held documents (holding what a query needs, as `hold_store` does
  for name lookups), kept live from `Pimble`'s changes; `Pimble::judge(edit, view)` adds the
  leaves-the-source warning to `plan`'s judgement. Shared by the app, plugins and the MCP.
- **App:** a pane opening a metanode asks the plugin host for its viewer and hands it the
  forest and the viewer's settings; the viewer calls `Pimble`. Without the viewer, the
  outline fallback. The explorer shows a metanode's own children as today, with the
  viewer's icon; "Show as ..." and "Source ..." on the row menu set the keys.
- **MCP:** `get_node` describes a metanode ("shows the children of Work, two levels, as
  kanban"); a `get_view` tool returns the source's forest; `set_view` sets the source and
  viewer. The existing `move_node`, `create_node`, `set_fields`, `set_tags` already make
  every edit a viewer makes.

## 4. Waves

1. **The concept in core:** the `metanode:*` keys; `Source` with the first values (children,
   subtree to a depth) and its forest, live; the judgement with the leaves-the-source
   warning; the outline fallback in the app; the MCP's description, `get_metanode`, `set_metanode`.
   This is what the kanban viewer needs.

   Built 2026-11-02 on branch `api`, on the recommended answers to the open decisions
   (any node with `metanode:viewer` is a metanode; a query metanode may have children).
   **Keys and types:** `pimble_core::metanode` (`VIEWER_KEY` `metanode:viewer`,
   `SOURCE_KEY` `metanode:source`, `Metanode { viewer, source }`, `Source { query,
   later }`, `Query::{Children, Subtree { depth }, Later}`). The source is stored as JSON
   text, `{"query":{"kind":"subtree","depth":2}}`; an unknown query kind reads as
   `Query::Later` and unknown keys beside `query` (filters, a limit) are kept in `later`, so
   an older app shows the children with the sentence `NEWER_SOURCE` instead of failing, and
   rewriting the source loses nothing. Depth is clamped to 1..=16. `NodeFields` reads both
   keys and `NodeFields::into_node` puts the `Metanode` on `NodeMetadata::metanode`, so
   every client (server answers, held documents, the vault) sees it; a change of either
   key is a `MetadataUpdated`. `NodeDoc::set_metanode_viewer`/`set_metanode_source` write
   one string key each (no container; setting what is there writes nothing).
   **The forest:** `Pimble::metanode_source(addr) -> Forest` (`crates/pimble-api/src/
   metanode.rs`): depth first in the tree's order, each `ForestEntry` with its address,
   node, parent, children in the forest and depth, through mounts, read with
   `Pimble::children` (held documents; a held forest is read again with no fetch).
   `Forest::touched(store, &Affected)`, `touched_by(&Change)` and `touched_by_view(&View)`
   say whether a change may have changed it: its ids against the metanode, the nodes found
   and the lists read. `Pimble::set_metanode(addr, viewer, source)` is the one write.
   **Judgement:** `Pimble::judge_in_metanode(metanode, MetanodeEdit::{Move, Delete})`
   answers `Judged { plan, warnings }`: `plan`'s refusals as errors, its warning first, then
   `leaves_metanode_warning` ("This won't be in this view any more." / "These won't ...")
   when a node of the forest lands in a list the source does not read, or is deleted.
   **App (desktop and web, shared code):** a pane opening a metanode shows
   `crate::metanodes`'s outline (icons, colors, labels, indented by depth; a click opens the
   node in the pane) under the line "Shown as an outline: the <viewer> viewer isn't
   installed." (`outline_line`), plus `NEWER_SOURCE` when it applies; the forest comes from
   `BackendCommand::GetMetanode` (`BackendEvent::MetanodeRead`) and is asked again when a
   `Shown` touches it, so another device's, the CLI's or the MCP's moves show live. A node
   that becomes a metanode, or stops being one, while a pane shows it switches in place.
   The explorer row is an ordinary row; its menu has "Show as Outline" on a metanode
   (`BackendCommand::SetMetanode` with no viewer, through `Pimble::edit`; the source is
   kept). `metanodes::set_metanode` is the code path a viewer menu will take.
   **MCP:** `get_node` says "(a metanode: the children of X, two levels, shown with the
   kanban viewer; ...)"; `get_metanode` answers the forest as an outline with links;
   `set_metanode { node, viewer?, depth? }` sets or clears.
   **Tests:** pimble-core 4 (`metanode::tests`: stored shape, a newer query, newer parts
   kept, depth), pimble-crdt 2 (the keys read into the node, a merge from another device),
   pimble-api 2 (`a_metanode_source_answers_a_forest_in_the_trees_order`: children and
   two levels, order kept after a reorder elsewhere, the live signal for a touching and an
   unrelated change, no fetch for a held forest, the leaves-the-view warning for a move out,
   a move three levels down and a delete, `plan`'s refusal kept, the source kept when
   cleared; `a_metanode_source_reads_through_mounts`), pimble-mcp 1 (the three tools).
   Checked in the release desktop app on a copy of the family store (port 7491): a folder
   set as a kanban metanode two levels deep through the MCP opened as the outline with the
   line; a move from the CLI showed in it live; "Show as Outline" turned it back (the
   folder's empty state); setting it again from the MCP switched the open pane back to the
   outline with its two-level source kept; a row opened its note in the pane. Not checked:
   the browser (it compiles for wasm and runs the same `pimble-app` code), an encrypted
   store's forest.
   Not built: "Source ..." on the menu, an app test of the outline (a `Forest` is made
   only by the API). The viewer's icon on the explorer row was built 2026-11-10: a
   metanode with no icon of its own shows the icon its viewer's plugin recipe makes
   nodes with (`AppStore::viewer_icon`, `AppStore::node_icon`), so a node turned into a
   kanban by `set_metanode` or "Show as ..." looks like one made with New > Kanban. A
   chosen icon still wins, a query metanode keeps its own, and nothing is written to the
   node.
2. **Hosting viewers:** the seam where a pane hands a metanode's forest and settings to a
   viewer and the viewer acts through `Pimble`, filled by plugins
   (`docs/PLUGINS_CONTRACT.md`); viewers' declared source shapes and the menu that offers
   only fitting ones.
3. **Query sources:** tags, search, backlinks, field conditions, filters, limits.

## 5. Open decisions for Joe

1. **A metanode is any node with `metanode:viewer`** (recommended: a folder can become a
   kanban and go back to an outline with nothing lost), or a node type of its own (`view`).
   *Taken as recommended in the data; in the app (Joe, 2026-11-04) there is no menu that
   turns a node into a metanode or back: one is made by a recipe (New Kanban), and the MCP's
   `set_metanode` is the way to change one (docs/MENUS_CONTRACT.md section 8).*
2. **A query metanode's own children:** allowed and shown in the explorer as any node's
   (recommended: it is still an ordinary node), or a query metanode takes no children.
3. **Where a query's new items go** (a viewer's "add" on a query source): the viewer's
   setting "new items go under ..." (recommended), or adding is refused on query sources.
