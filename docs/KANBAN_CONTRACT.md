# Kanban contract: the first viewer for metanodes, built as a plugin

Status: drafted by the PM on 2026-11-02 from Joe's design (memory
`project_views_and_plugins_2026_11_02`). Wave 1 built the same day (section 7), with
open decisions 1-5 taken as recommended; drags built the same day against rinch #2093
(merged to rinch main 2026-11-03, section 7 "Drags"). Kanban is **a viewer, built as a
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

## 3. Lanes (wave 2, built 2026-11-04; see section 8)

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
  (The card's "Move to ▸ <column>" buttons were removed on 2026-11-04: Joe, dragging does
  it.) The keyboard is
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

### Drags (built 2026-11-02 against joeleaver/rinch#2093, merged 2026-11-03)

Built against #2093's branch at `44e6448f` (its fix round; first built on `b63871e6`)
through a `[patch]` in `plugins/Cargo.toml`. #2093 merged to rinch main on 2026-11-03
(batch #2096); the patch is gone and the root, `web/` and `plugins/` locks all pin rinch
main at `8421ff53`, so the app carries the sortable stylesheet too.

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
    re-rendered away (its `data-ondragend` goes to the gone node). The app registers one
    `rinch::core::on_element_drag_end` listener when the plugins first load
    (`crates/pimble-app/src/plugins.rs` `loaded`) that calls `drag_ended` on every
    running instance (2026-11-03).
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

**Not done / known limits:** the app on rinch main with the drag-end listener (2026-11-03)
is built and its tests pass but was not yet driven by hand; a refusal box at a column cut off by the board's scroll edge is
clipped; "Move anyway" is tested end to end but was not
clicked in the app; a column from a store the plugin was not allowed reads as "not open
here"; a card's menu is inline buttons under the chosen card, not a popup menu (portals
into a plugin's body are untried, wave 4); the browser is checked under docs/PLUGINS_CONTRACT.md wave 3 (cards added, moved by key and by drag, in a plain and an encrypted store); a
reader's board (no gestures) was not tried with a real share; plugin markup is styled
inline (a plugin cannot add a stylesheet), so it has no hover states.

## 8. Built: wave 2, lanes (2026-11-04, branch `master`)

Joe, 2026-11-04: "sub-kanbans should show as lanes, not columns", and the card's "Move to"
buttons go ("we can drag").

- **What a lane is.** A top of the kanban's forest that is itself a kanban
  (`metanode.viewer == "kanban"`), or a mount whose source is one, is a lane. The viewer
  reads each lane's own forest (`Pimble::metanode_source` of the lane, or of the mount's
  source) beside the board's, so a lane keeps its own source and the board's stays two
  levels deep; a change touching any of the forests reads them all again. The other tops
  form the unnamed lane at the top (open decision 3), which is the whole board when there
  are no lanes and is not drawn when there are lanes and no other tops. Lanes nested in a
  lane are not lanes of the board: inside a lane, a kanban child is a column.
- **Model** (`board.rs`): `Board { lanes: Vec<Lane> }`, `Lane { metanode, node, label,
  color, writable, columns }`; `find_card` answers (lane, column, index), `lane_for_parent`
  the lane a move lands in, `matching_column` the column of another lane that lines up
  (same name, case and spacing ignored, else the same place).
- **Moves** are judged in the lane they land in (`judge_in_metanode` of the lane's
  kanban), so a card carried into another lane is not warned about as leaving the view.
  Drags work across lanes as they do across columns (one `cards` group; each lane's
  columns are a `columns` list of their own, keyed by the lane's node). Alt+Shift+Up at a
  column's top and Down at its bottom carry a card to the lined-up column of the lane
  above (its bottom) or below (its top); the plain arrows walk the same way.
- **Drawing.** Lanes stack top to bottom in one scrolled area, so columns of equal width
  line up across lanes. A named lane has a header (the kanban icon, its name renamed in
  place by Rename or F2, its people) and its own "+ Add column". "+ Add lane" under the
  last lane makes a kanban (the "New Kanban" recipe's metanode, no title) whose columns
  copy the lane above it (To do, Doing, Done when that has none), then its name is typed
  in place.
- **Not built:** one shared header row when every lane's columns agree (each lane draws
  its own headers; equal widths keep them lined up); dragging a lane to reorder it (the
  explorer moves it); "Rename in every lane"; the cross-store warning wording for a
  mounted lane was not tried.
- **Tests:** kanban-viewer 8: `a_card_steps_into_the_lined_up_column_of_the_lane_above_or_below`,
  `a_move_is_judged_in_the_lane_it_lands_in`, `column_names_match_without_case_or_spacing`,
  and the wave 1 tests over a one-lane board.
- **Checked in the release desktop app** (port 7473, scratch dirs): "+ Add lane" made Work
  with To do, Doing, Done under the top lane's; Bravo dragged from the top lane's Doing
  into Work's Doing moved with no warning; Alt+Shift+Up carried it back to the bottom of
  the top lane's Doing; the card's actions show Open, Rename, Delete only.
- **Asked of rinch (Joe, 2026-11-04):** smooth drags with a ghost of the card following the
  pointer and the list opening a gap where it lands. Today `SortableItem` hides the
  window's ghost and dims the item in place with a caret; the request went to the Rinch
  master session.

## 9. Built: from kobayban (2026-11-04)

Joe asked what the kanban could take from kobayban (`~/dev/kobayban`, the board he uses
every day) and chose four (2026-11-04): one header row with lane names down the left,
three lane sizes and folded columns, Done no taller than its row, and "Add card" on hover.

- **One header row.** When every lane has the same columns (by name, case and spacing
  ignored: `Board::aligned`), the board draws one header row above the lanes, each
  header counting its cards in every lane, and each lane is a row: its name's cell on the
  left (on a board with named lanes; the unnamed lane reads "Other cards"), then a cell
  per column. A header renames its column in every lane (one rename each), and a header
  dragged or moved with Alt+Shift+Left/Right moves that column in every lane
  (`columns_everywhere`); "+ Add column" there adds one to every lane. On an aligned
  board the plain arrows walk a column header first, then its cards lane after lane.
  Lanes whose columns differ draw their own header rows, as before.
- **Sizes and folds** are how this person has the board on this device (Joe's 2026-10-23
  rule: how things look is each client's own), kept through the host
  (`Host::remember`, plugin ABI 5, `docs/PLUGINS_CONTRACT.md`) in the app's `views.rs`,
  never in the store: folding a lane for oneself folds it for no one else. A lane is
  collapsed (one bar with its name and count; a click shows it), minimized (about three
  cards a column, 220 px, the rest scrolled) or expanded; a board with lanes starts
  minimized, a board without them is shown whole. The name cell has show all / show
  about three and collapse; "Expand all" and "Reset lanes" sit above a board with lanes.
  A column folds by its name (in every lane) to a 44 px strip with its name turned on
  its side and its count; a click unfolds it.
- **Done no taller than its row.** The last column's cell (where finished cards pile up,
  open decision 5) is stretched to the row's tallest cell and its list scrolls inside it
  (`position: absolute` in a stretched flex cell), so a long Done never makes its lane
  taller.
- **Add card on hover**: a small pill at the foot of a column's cell while the pointer is
  over it (`onmouseenter`/`onmouseleave`, proxied by the host), not a line in every column.
- A lane's row is drawn from the lane without its columns (rinch's `for` draws a row again
  when its item changes): its cells follow the board on their own, so an edit to one card
  redraws only its column's cell, never a card being dragged in another.
- **Tests:** kanban-viewer 10 (`a_board_is_aligned_when_every_lane_has_the_same_columns`,
  `the_look_is_remembered_as_json_and_read_back`); pimble-plugin-host kanban 5, updated
  for the keyboard in place of "Move to", "Add card" on hover, and a drop into a mounted
  column in place of its button.
- **Checked in the release desktop app** (port 7473, scratch dirs): the shared header row
  over "Other cards" and Work; "Add card" on hover; Doing folded to a strip in both lanes;
  Work collapsed to a bar; both still so after the app restarted; seven cards added to Done
  from the CLI stayed inside a cell as tall as its row and scrolled there.
- **Not built:** the milestone party (petals every ten done; plugins cannot animate yet);
  assignee and tag filters; a folded column takes no drops; the browser was not tried.
- **A mounted kanban from a store the plugin was not allowed** (Joe, 2026-11-04) showed as a
  column: the plugin could not read the mount's source to see it is a kanban. The app now
  asks for that store above the board (`docs/PLUGINS_CONTRACT.md`, ABI 5 note); allowed,
  the mount is a lane. Checked in the release desktop app with a kanban in a second store
  pasted as a mount.

