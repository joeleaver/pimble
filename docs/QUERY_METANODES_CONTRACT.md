# Query metanodes contract: nodes that gather other nodes

Status: drafted 2026-11-04 from Joe's design in conversation; nothing built. This is
`docs/METANODES_CONTRACT.md` wave 3 ("Query sources"), shaped by what Joe decided on
2026-11-04 (section 1). Open decisions in section 9.

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

## 9. Open decisions for Joe

1. **What New calls it**: "Smart List" (recommended: says it gathers, not a folder you put
   things in), "Smart Folder", "Query", or "Collection".
2. **Where it looks by default**: its own store (recommended), or everywhere.
3. **A node dropped onto a query metanode**: becomes one of its own children (recommended:
   one meaning for a drop everywhere), or is changed to match (given the tag).
4. **Do found rows expand** to show their own children: yes (recommended: they are the real
   nodes), or found rows are leaves.
