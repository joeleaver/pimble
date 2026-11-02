# Views contract: view nodes, a generic way of showing nodes

Status: drafted by the PM on 2026-11-02 from the design conversation with Joe the same day
(memory `project_views_and_plugins_2026_11_02`) and the API refactor before it
(`docs/API_CONTRACT.md`, waves 0-5 on branch `api`). Nothing is built. Open decisions at
the end.

**Three things, kept apart (Joe, 2026-11-02):**
- **View nodes** (this contract): a generic, reusable concept in Pimble's core. A view
  node says which nodes it shows and how they are grouped and ordered, and names a layout.
  Nothing here is specific to any one layout.
- **The plugin system** (`docs/PLUGINS_CONTRACT.md`): how layouts (and other things) are
  provided from outside the app, in plain rinch, over `pimble-api`.
- **Kanban** (`docs/KANBAN_CONTRACT.md`): the first consumer of view nodes, built as a
  plugin with the plugin system. Not a built-in.

## 1. What a view node is

- A view node shows other nodes. It stores **how to show** them, never the nodes: every
  gesture made through a view becomes an ordinary edit of real nodes (a move, a rename, a
  field, a tag) or is refused with a sentence.
- It has three generic settings that core owns and a **layout** that it names:

| Setting | First value (with the first consumer) | Later values |
|---|---|---|
| source | the view node's children | its subtree; a query (tag, search, backlinks) |
| grouping | by parent: each source node is a group, its children the items; nested once more for a second level | by field, by tag |
| order | tree position | a field |
| layout | a name, e.g. `kanban`, provided by a plugin | any plugin's; a few built-ins later if ever warranted |

- **Core generalizes source, grouping and ordering; layout is not generalized** (Joe). A
  layout is opinionated and lives in a plugin.
- **What a gesture means comes from the settings**, and `pimble_core::plan` judges it:
  - grouped by parent: a drop into a group is `plan_move` to (group, `Place`);
  - ordered by tree position: a reorder is a move with `After(sibling)`;
  - adding to a group is `create_parent` with `Place::Last`;
  - grouped by field: a drop sets the field; by tag: swaps the tag (later values);
  - ordered by a field: a reorder has no honest meaning and is refused;
  - a query source needs "new items go under ..." or adding is refused.
- **A layout gets a model, not the store.** Core hands it the groups, the items in each in
  order, and what each item is (label, icon, colors, access, counts), and takes back
  **intents** in the model's terms ("put item X in group B after Y", "add an item to group
  A", "rename group B", "open item X"). Core judges an intent through the settings and
  `plan`, and turns it into `Pimble` calls, or answers the refusal's sentence. A layout may
  also call `Pimble` directly (plugins may write; `docs/PLUGINS_CONTRACT.md`): intents are
  a convenience offering the explorer's judgement, not a fence.
- **Fallback:** a view node whose layout this app does not have shows as an outline of the
  same nodes, with one line saying which layout it names. Old apps, the web without the
  plugin, and the MCP all see an ordinary node with ordinary children.

## 2. Where settings live

- On the view node, as `node` root keys like fields (`field:<name>`; no container is made
  on first use, memory `project_node_fields_decision`): `view:layout`, later
  `view:source`, `view:group`, `view:order`. Co-edited like any key: everyone who opens the
  node sees the same layout.
- A layout's own shared settings (a plugin's options for this node) go in the node's
  `data` under the plugin's id, as plugin JSON already does.
- What is one person's (a collapsed group, a size, the scroll) is this device's, in
  `views.rs` beside expanded rows and canvas cameras, and never leaves the device.

## 3. In the app, the MCP and the API

- **Opening:** a pane that opens a node with `view:layout` asks the plugin host for that
  layout; if it is there, the pane shows the plugin's view of the model, else the outline
  fallback.
- **The model is live:** it is built from the API's held documents and updated from
  `Pimble`'s changes (`BackendEvent::Shown`, wave 5), so another device's move shows at
  once.
- **pimble-api:** `Pimble::view_model(node)` (source, grouping, order applied over held
  documents) and `Pimble::intend(node, intent)`, both pure judgement over `plan` plus the
  calls they produce, so the app, plugins and the MCP share them.
- **MCP:** `get_node` says "a view showing ... as <layout>, groups A, B, C"; the existing
  `move_node`, `create_node` and `rename` already make every grouped-by-parent gesture; a
  `set_view` tool sets or clears the settings.
- **Explorer:** a view node's row shows its children as an outline, as today, with the
  layout's icon. "Show as ..." / "Show as Outline" on the row menu sets or clears
  `view:layout` (listing the layouts installed plugins provide).

## 4. Waves

1. **The concept in core:** the `view:*` keys and their reading (`pimble-core`), the
   model and intents over `plan` in `pimble-api` with tests, the outline fallback in the
   app, the MCP's description and `set_view`. Usable on its own (an outline that says
   which layout it names) and the base the plugin host and kanban build on.
2. **Hosting layouts:** the seam where a pane hands a view node's model to a layout and
   takes intents back, filled by plugins (`docs/PLUGINS_CONTRACT.md`).
3. **More settings:** subtree and query sources, grouping by field and tag, ordering by a
   field, each with its gesture meanings.

## 5. Open decisions for Joe

1. **A view node is a node with `view:layout`** (recommended: any node can become a view
   and go back to an outline with nothing lost), or a node type of its own (`view`).
2. **Query sources** (later): a view node whose source is a query has no children of its
   own; is it still an ordinary node in the tree (recommended), or something else.
