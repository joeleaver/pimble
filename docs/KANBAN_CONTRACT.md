# Kanban contract: the first viewer for metanodes, built as a plugin

Status: drafted by the PM on 2026-11-02 from Joe's design (memory
`project_views_and_plugins_2026_11_02`). Wave 1 built the same day (section 7), with
open decisions 1-5 taken as recommended; drags built the same day against rinch #2093
(unmerged, section 7 "Drags"). Kanban is **a viewer, built as a
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

Taken as recommended on 2026-11-02 (wave 1): 1, a card shows its label, icon, colors and a
subtask count; 2, a column's own text is hidden; 3, non-kanban children of a laned kanban
form one unnamed top lane (lanes are wave 2); 4, "New Kanban" makes To do, Doing, Done;
5, nothing for Done piling up yet.


1. **What a card shows:** label, icon, colors and a subtask count (recommended), or also
   the first line of its text, its tags, a discussion badge.
2. **A column's own text:** hidden (recommended), or shown under the header (a WIP limit,
   what "done" means).
3. **Non-kanban children of a laned kanban:** one unnamed lane at the top (recommended), or
   left off the board.
4. **New Kanban's columns:** To do, Doing, Done (recommended), or none.
5. **Done piling up:** nothing in wave 1 (recommended), later an "Archive" that moves old
   done cards under a child of Done, or a per-device filter.

## 7. Built: wave 1 (2026-11-02, branch `api`)

`plugins/kanban-viewer` (`kanban_viewer.wasm`, about 7 MB: the whole Tabler set rides with
`IconGlyph`). Manifest: viewer `kanban` taking `SourceShape::Subtree { depth: 2 }`, recipe
`new-kanban` ("New Kanban": a metanode titled Kanban, icon `layout-kanban`, its subtree
two levels deep, children To do, Doing, Done), asks to read and change the stores chosen
(`StoreAsk::Chosen`, write).

- **The board** (`board.rs`, pure and tested natively): `board_of(forest)` makes columns
  (the forest's first level) of cards (the second), in the tree's order; a card is its
  label (`display_label`, the explorer's), its icon (`pimble_ui::appearance::node_icon`),
  its text and row colors drawn with the app's `display_color`/`row_background` for the
  theme the host says, and a subtask count (its children). A column's own text is not
  shown. Writability is each node's `access` and the metanode's: a node the person may
  only read shows no gestures (no add, rename, move or delete; Open stays).
- **Moves, one function.** Every move is a `Target { parent, place }` with a tree
  `Place`, never an index, and `Kanban::move_to` is the only thing that moves: it lists
  the stores again, judges in the metanode (`Pimble::judge_in_metanode`), shows a refusal
  as `plan`'s sentence with nothing sent, holds a warning (another store, leaving a share,
  leaving the view) for "Move anyway" / "Cancel", and sends the plan
  (`Pimble::move_nodes`), keeping the card chosen under a new id when the move gave it one.
  The card's "Move to ▸ <column>" buttons go to the bottom of that column; the keyboard is
  the explorer's, Alt+Shift+arrows (Alt+Left and Alt+Right alone are the app's Back and
  Forward): up and down one within the column, left and right into the neighbor column at
  the same height; a column chosen by its header moves along the board with
  Alt+Shift+Left/Right. Plain arrows choose (cards, and a column's header above its first
  card), Enter opens, F2 renames, Delete deletes, Escape lets go.
- **Drags** are rinch's sortable lists (see "Drags" below): a card drags whole, a
  column by its header; a drop is judged and sent by `move_to` like a menu item or a key.
- **Renaming** in place (a column by its header's "Rename" or F2, a card by its own): the
  one write, `Pimble::edit` with the title and `explicit_title`; Enter keeps it, Escape
  cancels; the field takes the keyboard when it is drawn.
- **Adding:** "+ Add card" creates under the column, last; "+ Add column" under the
  metanode, last; each with no title (`recipes::document("")`), then its name typed in
  place. **Deleting** a card asks `can_delete` first, then deletes (a tombstone) and says
  "Deleted “X”. View > Recently Deleted puts it back."
- **Opening** a card: Open or Enter asks the host (`Host::open`), which opens it in the
  plugin's pane as an outline row does.
- **Live:** the forest is read again whenever a change touches it
  (`next_change`, `Forest::touched_by`): another window, the CLI, the MCP.
- **Presence** (docs/PRESENCE_CONTRACT.md "Plugins"): choosing a card or column says the
  person is on it; each card and column header shows who else is on it with the app's
  `PersonChip` (up to three, then "+n"), a click on one follows them; a pane following
  someone scrolls to the card they are on and edges it in their color.

**Tests:** kanban-viewer 4 (keys move by place within and across columns, the menu goes
to the bottom, a column moves along the board, the manifest's viewer and recipe);
pimble-plugin-host `tests/kanban.rs` 2, the built plugin over a real in-process server:
the board in the tree's order with a subtask count, choosing tells the host, "Move to"
and Alt+Shift+Up/Left reach the server, Open asks the host, a column renamed in place
with its explicit title, "+ Add card" named in place, Delete, a move from elsewhere shown
live, people drawn with the app's chips and the followed person's card edged, a chip's
click asks to follow, a move into another store's column held for "Move anyway",
`plan`'s "This mount's store isn't open here." with nothing sent; and a plugin allowed
only to read refused its write with the host's sentence.

**Checked in the release desktop app** (port 7491, scratch dirs, a copy of the family
store, the plugin installed with `scripts/plugins/install.sh`): "New Kanban" on a row's
menu made Kanban with To do, Doing, Done; opening it asked to allow the plugin, then
showed the board; "+ Add card" with a name typed in place; Alt+Shift+Up and
Alt+Shift+Right moved it; "Move to ▸ Done" moved it; the Doing column renamed in place to
"In progress" (the explorer followed); a move made through `pimble-mcp` (an agent) showed
live, and the agent's edit of a card showed its robot pill on the card; Open opened the
card in the pane; Delete removed a card with its line; a store mounted as a column from
the CLI appeared live, and a move into it was refused with "This mount's store isn't open
here." (the plugin's approval covered only the family store, so to the plugin that store
is not open: the explorer's sentence, though "not allowed" would say it better; noted
below). Presence with a second app instance: see docs/PRESENCE_CONTRACT.md "Plugins".

### Drags (built 2026-11-02 against joeleaver/rinch#2093, NOT merged)

**Built against an unmerged rinch PR.** The `plugins/` workspace patches every rinch
crate to #2093's branch at `44e6448f` (its fix round; first built on `b63871e6`) (`[patch]` in `plugins/Cargo.toml`, spelled with
`www.github.com` because a patch must name another source); the app and the web stay
on rinch main. When #2093 merges: delete that patch block, `cargo update -p rinch` in
`plugins/` (and in the root and `web/` so the app carries the sortable stylesheet), and
rebuild and reinstall the plugins.

- **Shape.** Each column's cards are a vertical `SortableList` of the group `cards`
  (id: the column's key); the columns are a horizontal `SortableList` of the group
  `columns` (id: the metanode's key), each column a `SortableItem` with `handle: true`
  dragged by its header in a `SortableHandle`. A key is `<store>/<node>`
  (`board::key_of`), so a mounted column's cards keep their own addresses; cards and
  columns are distinct nodes, so no key repeats across the two groups (lanes, wave 2, are
  nodes too). A card the person may only read is `disabled` (it does not drag), and so is
  a column the person may not move; a board the person may only read disables both lists
  (no drag starts or lands). A read-only column on a writable board keeps its list live,
  so a drop into it is judged and says "You can read this, not change it." at the caret.
  Both lists lift with Space only (`lift_keys`), so Enter stays "open the card". The caret
  and the refusal box are drawn in the app's theme colors through rinch's
  `--rinch-sortable-*` properties, set inline on each list (`SORT_THEME`). The lists' layout is also written inline, so an app whose rinch has
  no sortable stylesheet still lays the board out (only the caret and the refusal box
  need the stylesheet).
- **Judge** (`Kanban::judge_drop`): synchronous, for every place a drag offers, `plan`'s
  `plan_move` over the documents the plugin holds (`Pimble::view`): a refusal's sentence
  shows at the caret and the drop is not delivered. The board lists its stores again with
  every read, so a mounted column whose store closed since is refused at the caret.
- **Drop** (`Kanban::drop_move`): `board::drop_of` maps the `SortMove` to the same
  `Target` the menu and the keys make (no `after` is `Place::First`, an empty column
  included; `after(k)` is `Place::After(k)`), chooses what was dragged and calls
  `move_to`, which judges again in full (`judge_in_metanode`, with ancestors held)
  before sending: the place on offer may be stale by the release.
- **A warning-only move is accepted at the caret and then held for "Move anyway"**, as a
  menu's or a key's move is. A warning (a new node in another store, leaving a share,
  leaving the view) is not a refusal, and the sync judge cannot see all of them (a share
  above the metanode is not held), so the drop is accepted and `move_to`'s full
  judgement decides; making a consequential move on a release with only a sentence the
  person may not have read would be the one place the board moves without asking.
- **One at a time:** the board has one choice, not a selection, so a drag carries one card
  or one column (`drag_keys` unused). Alt+Shift+arrows, the menu and plain-arrow choosing
  are unchanged.
- **Host (ABI 4).** Nothing kanban-specific, and no new handler kind (#2093 runs on
  `data-ondrag*` and `data-onkeydown`, already proxied):
  - `Query::BoundingRect`: the painted box in the click context's frame (the SDK's
    `bounding_rect` had answered the parent-relative layout box, which a drag cannot
    compare with the pointer).
  - `Key::target`: the guest node a key was sent to (the document's focused element,
    translated), which the SDK passes as rinch's `KeyEventData::target`, so a Space typed
    in a card's name field does not lift the card.
  - The drag ghost: a proxied `data-ondragstart` whose element (or an ancestor) carries
    `data-drag-ghost="false"` hides the window's ghost before the guest hears the start
    (the guest's own `suppress_drag_ghost` reaches nothing that paints). A rinch whose
    runtime reads the attribute itself (#2093's) makes this a no-op.
  - `exports::DRAG_ENDED` / `Instance::drag_ended`: the guest calls rinch's
    `notify_element_drag_ended`, which ends a drag whose dragged card a co-edit
    re-rendered away (its `data-ondragend` goes to the gone node). **The app does not
    call it yet:** it needs `rinch_core::on_element_drag_end` in the app's own rinch,
    which arrives with #2093; when the app moves to that rinch, register one listener
    in `crates/pimble-app/src/plugins.rs` that calls `drag_ended` on every running
    instance. Until then such a drag ends at the next drag start.
  - `dispatch` answers flags (`dispatched::CONSUMED`; the other bits are free).
- **Not yet:** a lift is not cancelled by focus leaving the item inside a plugin (the
  dismiss stack is not forwarded); Escape, Tab and the arrows work.

**Tests (drags, at `44e6448f`):** kanban-viewer 5
(`a_drop_lands_after_the_item_it_names_or_first`); pimble-plugin-abi 5 (`BoundingRect` and
a key's target round trip); pimble-plugin-host applier 6
(`a_drag_source_marked_ghostless_hides_the_hosts_ghost`: a proxied drag start under
`data-drag-ghost="false"` hides the window's ghost, an unmarked one keeps it, `draggable`
passes through; `a_key_names_the_guest_node_it_was_sent_to`), wasm 3 (the WAT guest
exports `pimble_drag_ended`), and kanban 5, the built plugin over a real server driven only
through the host's proxied `data-ondrag*` and `data-onkeydown` handlers:
`cards_and_columns_drag_through_the_host_and_a_refused_drop_says_why` (Alpha dragged over
Charlie's lower half lands after it in the other column, ghost hidden, caret after
Charlie; the "Write it" column dragged by its header over the left half of "Plan" lands
first; a card dragged into a mounted column whose store is gone shows "This mount's store
isn't open here." in the list's reason slot and the drop sends nothing),
`space_lifts_a_focused_card_but_not_from_a_field_and_enter_opens` (Enter on a focused card
does not lift it; Space does and Escape puts it down; a Space typed in the card's name
field does not), and `a_drag_whose_card_a_remote_edit_redrew_ends_when_the_host_says` (a
rename from elsewhere redraws the dragged card mid-drag; after the leave and
`Instance::drag_ended` the caret is gone and nothing moved; a later drop with no drag
moves nothing).

**Checked in the release desktop app** (port 7491, scratch dirs, the family-store copy,
real pointer drags through rinch's debug port: `mouse_down`, stepped `mouse_move`s with a
frame each, `mouse_up`). First with the app on rinch main (no sortable stylesheet, so no
caret): a card across columns, above another, back within its column, a column to the
front, and a drop into Home (a store the plugin was not allowed) refused. Then, at
`44e6448f`, with the app itself built against #2093 in a scratch worktree (not
committed; the app stays on main until it merges): a card into the empty In progress
(the list outlined in the theme's primary color, the dragged card dimmed, no ghost);
a card after another (a line caret under it); a reorder within the column; the In
progress column dragged by its header to the front (a vertical caret at Done's edge); a
card dragged into Home (a red dashed outline and the red reason box, clipped where
Home ran past the pane's edge) whose release moved nothing. Each move moved the node
and the explorer followed.

**Not done / known limits:** until the app moves to a rinch with #2093 it has no sortable
stylesheet, so it draws no caret (checked with the app built against #2093 in a scratch
worktree, above); a refusal box at a column cut off by the board's scroll edge is
clipped; "Move anyway" is tested end to end but was not
clicked in the app; a column from a store the plugin was not allowed reads as "not open
here"; a card's menu is inline buttons under the chosen card, not a popup menu (portals
into a plugin's body are untried, wave 4); the browser is checked under docs/PLUGINS_CONTRACT.md wave 3 (cards added, moved by key and by drag, in a plain and an encrypted store); a
reader's board (no gestures) was not tried with a real share; plugin markup is styled
inline (a plugin cannot add a stylesheet), so it has no hover states.
