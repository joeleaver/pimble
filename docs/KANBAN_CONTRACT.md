# Kanban contract: the first viewer for metanodes, built as a plugin

Status: drafted by the PM on 2026-11-02 from Joe's design (memory
`project_views_and_plugins_2026_11_02`). Nothing is built. Kanban is **a viewer, built as a
plugin** with the plugin system (`docs/PLUGINS_CONTRACT.md`), for **metanodes**
(`docs/METANODES_CONTRACT.md`); it is the first consumer of both and waits on them. It declares
the source shape it needs: a tree two levels deep. Open decisions at the end.

**Naming:** "board" already means a canvas in the code; this is a **kanban** in code and in
the interface.

## 1. What Joe decided

- Cards are ordinary nodes (notes or one-liners).
- A kanban is a metanode whose viewer is `kanban` and whose source is the node's children,
  two levels deep (a forest, `docs/METANODES_CONTRACT.md` section 1.1). The kanban viewer reads
  that forest its own way: **columns are the first level, cards the second.** Grouping is
  the viewer's, not core's.
- **Position in a column is priority**: the top of "To do" is what you pull next.
- **Swimlanes are a tree of kanban trees**: a kanban whose children are kanbans shows them
  as lanes, columns lined up by name; a lane can be a mount.

## 2. The viewer

- Columns in order; each header is the column node's label, renamed in place (a rename
  through `Pimble`).
- Cards in order, top first; a card shows its label, icon and colors and a subtask count
  (open decision 1). Opening a card opens it in a pane (a host call).
- Drag a card within or across columns (a move under the column, after the card above),
  drag a column (a move of the column node), "+ Add card" at a column's foot (a create
  under the column, last), "+ Add column", Delete. Refusals are the explorer's sentences,
  because core judges every edit through `plan` (`docs/METANODES_CONTRACT.md` section 1.3).
- Read-only nodes show the board with no gestures. Live: the source's forest follows `Pimble`'s changes.
- "New Kanban" creates a metanode with viewer `kanban`, source its children two levels
  deep, and columns To do, Doing, Done (open decision 4), through a recipe the plugin
  registers.

## 3. Lanes (wave 2)

- With lanes the source is three levels deep. Kanban children of a kanban are lanes, top to bottom; their children are their columns.
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
