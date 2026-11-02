# Kanban contract: the first plugin, a layout for view nodes

Status: drafted by the PM on 2026-11-02 from Joe's design (memory
`project_views_and_plugins_2026_11_02`). Nothing is built. Kanban is **a plugin** built with
the plugin system (`docs/PLUGINS_CONTRACT.md`) on **view nodes** (`docs/VIEWS_CONTRACT.md`);
it is the first consumer of both and waits on them. Open decisions at the end.

**Naming:** "board" already means a canvas in the code; this is a **kanban** in code and in
the interface.

## 1. What Joe decided

- Cards are ordinary nodes (notes or one-liners).
- A kanban is a view node with layout `kanban`, its source the node's children, grouped by
  parent: **columns are the node's children, cards the columns' children.**
- **Position in a column is priority**: the top of "To do" is what you pull next.
- **Swimlanes are a tree of kanban trees**: a kanban whose children are kanbans shows them
  as lanes, columns lined up by name; a lane can be a mount.

## 2. The layout

- Columns in order; each header is the column node's label, renamed in place (intent
  "rename group").
- Cards in order, top first; a card shows its label, icon and colors and a subtask count
  (open decision 1). Opening a card opens it in a pane (intent "open item").
- Drag a card within or across columns (intent "put item in group after ..."), drag a
  column (a move of the column node), "+ Add card" at a column's foot, "+ Add column",
  Delete. Refusals are the explorer's sentences, because core judges the intents through
  `plan` (`docs/VIEWS_CONTRACT.md` section 1).
- Read-only nodes show the board with no gestures. Live through the view model.
- "New Kanban" creates a view node with layout `kanban` and columns To do, Doing, Done
  (open decision 4), through a recipe the plugin registers.

## 3. Lanes (wave 2)

- Kanban children of a kanban are lanes, top to bottom; their children are their columns.
- Columns line up by name (case- and space-insensitive) when every lane agrees; otherwise
  each lane draws its own headers.
- A drag into another lane goes to its same-named column; a lane from another store makes
  it a transplant, with the cross-store warning.
- A new lane copies the column names above it; "Rename in every lane" is several renames.

## 4. As a plugin

- Written in plain rinch against the plugin API (`pimble-api` through the host), so it runs
  on the desktop and in the browser alike.
- Dragging across several lists needs a general rinch mechanism if rinch lacks one (a drag
  source and drop targets with a judge callback, the shape `Tree`'s `judge_drop` has);
  that goes upstream with nothing kanban- or Pimble-specific in it.
- Without the plugin, a kanban node is an outline of its columns and cards.

## 5. Waves

1. Columns and cards, opening, adding, renaming, deleting, drags; "New Kanban".
2. Lanes.

## 6. Open decisions for Joe

1. **What a card shows:** label, icon, colors and a subtask count (recommended), or also
   the first line of its text, its tags, a discussion badge.
2. **A column's own text:** hidden (recommended), or shown under the header (a WIP limit,
   what "done" means).
3. **Non-kanban children of a laned kanban:** one unnamed lane at the top (recommended), or
   left off the board.
4. **New Kanban's columns:** To do, Doing, Done (recommended), or none.
5. **Done piling up:** nothing in wave 1 (recommended), later an "Archive" that moves old
   done cards under a child of Done, or a per-device filter.
