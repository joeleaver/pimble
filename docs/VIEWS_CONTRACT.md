# Views contract: view nodes, a source and a viewer

Status: drafted by the PM on 2026-11-02 from the design conversation with Joe the same day
(memory `project_views_and_plugins_2026_11_02`) and the API refactor before it
(`docs/API_CONTRACT.md`, waves 0-5 on branch `api`). Revised the same day on Joe's model:
a view node says **what** nodes it shows (a source: a query with filters) and **which
viewer** shows them; grouping is the viewer's, not core's. Nothing is built. Open decisions
at the end.

**Three things, kept apart (Joe, 2026-11-02):**
- **View nodes** (this contract): a generic, reusable concept in Pimble's core.
- **The plugin system** (`docs/PLUGINS_CONTRACT.md`): how viewers (and other things) are
  provided, in plain rinch, over `pimble-api`.
- **Kanban** (`docs/KANBAN_CONTRACT.md`): the first viewer, built as a plugin. Not a
  built-in.

## 1. What a view node is

A view node is two things:

1. **A source: which nodes.** A query with filters, evaluated by core over the API's held
   documents:
   - this node's children, or its subtree to a depth;
   - nodes with a tag; a search; the nodes linking to a note (backlinks);
   - nodes with a field set, or a field matching a condition;
   - filters on top (not done, modified this week, of a type), and an order (the tree's own
     order, or a field).
2. **A viewer: how they are shown.** Which viewer (a plugin's), and that viewer's own
   settings (a calendar's date field, a kanban's card size), stored on the node under the
   viewer's id.

It stores **how to show** nodes, never the nodes: every gesture made through a view
becomes an ordinary edit of real nodes or is refused with a sentence.

### 1.1 A source answers a forest

A source's result keeps the tree relations among what it found: each node with its
parent, its children that are also in the result, and their order. A kanban reads "this
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
**out of the view's source** ("This card won't match the view's filter any more."), as a
warning the viewer shows, like the leaving-a-share warning.

### 1.4 Fallback

A view node whose viewer this app does not have shows as an outline of its source's
forest, with one line naming the viewer. Old apps, the web without the plugin, and the MCP
see an ordinary node.

## 2. Where it lives

- On the view node, as `node` root keys like fields (`field:<name>`; no container is made
  on first use, memory `project_node_fields_decision`): `view:viewer` (the viewer's id) and
  `view:source` (the query, as a small serialized structure). Co-edited like any key:
  everyone who opens the node sees the same view.
- The viewer's shared settings go in the node's `data` under the viewer's id, as plugin
  JSON already does.
- What is one person's (a collapsed column, a size, the scroll, a calendar's month) is this
  device's, in `views.rs` beside expanded rows and canvas cameras, and never leaves it.
- A view node whose source is "my children" is a node with children, as any node. A view
  node whose source is a query is a saved search with a viewer; it may have no children of
  its own (open decision 2).

## 3. In the API, the app and the MCP

- **pimble-api:** `Source` (the query and filter types) and `Pimble::source(node) ->
  Forest`, evaluated over held documents (holding what a query needs, as `hold_store` does
  for name lookups), kept live from `Pimble`'s changes; `Pimble::judge(edit, view)` adds the
  leaves-the-source warning to `plan`'s judgement. Shared by the app, plugins and the MCP.
- **App:** a pane opening a view node asks the plugin host for its viewer and hands it the
  forest and the viewer's settings; the viewer calls `Pimble`. Without the viewer, the
  outline fallback. The explorer shows a view node's own children as today, with the
  viewer's icon; "Show as ..." and "Source ..." on the row menu set the keys.
- **MCP:** `get_node` describes a view node ("shows the children of Work, two levels, as
  kanban"); a `get_view` tool returns the source's forest; `set_view` sets the source and
  viewer. The existing `move_node`, `create_node`, `set_fields`, `set_tags` already make
  every edit a viewer makes.

## 4. Waves

1. **The concept in core:** the `view:*` keys; `Source` with the first values (children,
   subtree to a depth) and its forest, live; the judgement with the leaves-the-source
   warning; the outline fallback in the app; the MCP's description, `get_view`, `set_view`.
   This is what the kanban viewer needs.
2. **Hosting viewers:** the seam where a pane hands a view node's forest and settings to a
   viewer and the viewer acts through `Pimble`, filled by plugins
   (`docs/PLUGINS_CONTRACT.md`); viewers' declared source shapes and the menu that offers
   only fitting ones.
3. **Query sources:** tags, search, backlinks, field conditions, filters, field order.

## 5. Open decisions for Joe

1. **A view node is any node with `view:viewer`** (recommended: a folder can become a
   kanban and go back to an outline with nothing lost), or a node type of its own (`view`).
2. **A query view node's own children:** allowed and shown in the explorer as any node's
   (recommended: it is still an ordinary node), or a query view node takes no children.
3. **Where a query's new items go** (a viewer's "add" on a query source): the viewer's
   setting "new items go under ..." (recommended), or adding is refused on query sources.
