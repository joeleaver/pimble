# Todo list contract: a subtree shown as a checklist

Status: waves 1 and 2 built 2026-11-05 (branch `todo-list`). Decisions by Joe, 2026-11-05.

A todo list is a metanode shown with the `todo` viewer of the `todo-viewer` plugin
(docs/PLUGINS_CONTRACT.md, docs/METANODES_CONTRACT.md). Its source is its own subtree:
every node is an item, an item's children are its sub-todos, and the tree's order is the
list's order, as a kanban's columns are its first level and their order is priority
(docs/KANBAN_CONTRACT.md). Nothing about an item lives anywhere but in its own node.

## 1. Decisions (Joe, 2026-11-05)

1. **Done is a timestamp** in the node field `done` (the key/value store a node's
   Properties panel shows and formulas read as `'Note'!done`): checking an item writes the
   moment it was checked (`Pimble::edit`'s `now`, RFC 3339 UTC); unchecking removes the
   field. "Done or not" and "done when" are one fact, and a query metanode can find either.
2. **Opening is the kanban's** (docs/KANBAN_CONTRACT.md "Opening"): a click on an item's
   title opens it beside the list (`Host::open_beside`), the list keeping the focus and
   the keyboard; Ctrl (Cmd) + click or Ctrl+Enter opens it in the list's place; a
   double-click or F2 renames.
3. **The look is the kanban's for now**: the same surfaces (`pimble_ui::viewer`), rows a
   step lighter than the list's panel, a grip on every row that drags, a trash can under
   the pointer, no buttons on rows.
4. **The todo list comes before the timeline** (docs/TIMELINE_CONTRACT.md).

Recommended and taken with the plan: a parent shows its progress ("2/5") and never checks
itself when its children are done; "hide done" and which items are folded are this
device's own (`Host::remember`), never in a store.

## 2. What the person sees

- One panel: (once it has items) a line "3 of 7 done" and an icon that hides or shows the done items; then the
  items as an outline, each row indented by its depth: a grip, a chevron when it has
  sub-todos (a click folds it), a checkbox, its title (struck through and dimmed when
  done), its progress when it has sub-todos, its `due` field read as a date ("Today",
  "Tomorrow", "Nov 12", "Jan 3, 2027"; red once it is past and the item not done; text
  that is not a date shown as typed),
  who else is on it, and a trash can while the pointer is over it or it is chosen.
- "+ Add item" at the foot.
- A list the person may only read shows no gestures: no checkbox changes, no add,
  rename, move or delete; opening still works.

## 3. Gestures

- **Checking** an item: a click on its checkbox, or Space on the chosen item. One edit of
  that node's `done` field.
- **Opening, renaming**: section 1, decision 2.
- **Adding**: "+ Add item" makes an untitled item last in the list and starts its name;
  Enter in a name field keeps the name and starts a new item right after it (an outliner's
  Enter); Escape stops. An item made this way and left without a name is deleted again, so
  typing Enter twice leaves nothing behind.
- **The keyboard**: Up and Down choose among the rows shown; Left folds the chosen item or
  goes to its parent, Right unfolds it; Enter opens beside, Ctrl+Enter in place; F2
  renames; Space checks; Delete deletes; Escape lets go.
- **Dragging** a row (rinch's sortable lists, one list of the rows shown): a drop after a
  row whose sub-todos show right below it lands first among them; after any other row,
  right after it, beside it; at the top, first in the list (`outline::drop_target`). A
  drop never guesses a depth: Tab and Shift+Tab change it. While a drag is over a place,
  `plan` judges it over the documents held, so a row dropped into its own sub-todos says
  "A node can't go inside itself." at the caret and sends nothing; a warning waits for
  "Move anyway", as the keys' moves do. Insert lifts a focused row for a keyboard drag
  (Space checks and Enter opens, so neither lifts).
- **Moving**, every move planned and judged in the metanode first as the kanban's are
  (`Pimble::judge_in_metanode`, `plan`'s refusals and warnings, "Move anyway"):
  Alt+Shift+Up/Down past the sibling above or below; Tab or Alt+Shift+Right indents (last
  under the sibling above); Shift+Tab or Alt+Shift+Left outdents (right after its parent).
- **Deleting**: Delete or the trash can; asked of `can_delete` first, then a tombstone,
  and "Deleted “X”. View > Recently Deleted puts it back."

## 4. Waves

1. The outline, checking, opening, renaming, adding, deleting, keyboard moves, folding,
   hide done, presence. Built.
2. Drags, and `due` read as a date, overdue in red. Built. `due` reads as a sheet's date
   column reads what is typed (`pimble_sheet::input`, Gregorian only), so a date means
   the same in a sheet and in a todo list; the timeline's calendars stay inside the
   timeline plugin (Joe, 2026-11-05, docs/TIMELINE_CONTRACT.md).

## 5. How it is checked

pimble-plugin-host's `todo` tests run the built plugin over a real in-process server
(as `kanban.rs` does): the outline in the tree's order, a check writing `done` and an
uncheck removing it, a click opening beside, a double-click renaming, Enter making the next
item, an empty new item removed, Tab and Shift+Tab, a delete; a drag to the top and one
into an open row, a drop into a row's own sub-todos refused at the caret with nothing
sent; a past `due` overdue until the item is done, text that is not a date as typed. Then by hand in the release
desktop app on a store copy.
