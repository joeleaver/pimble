# Menus contract: the explorer's row menus and the "+" button

Status: drafted 2026-11-04 from Joe's request ("Our context menu has gotten out of hand,
meanwhile our 'new node' + button only makes text nodes, we need a plan to fix both";
"If we don't support submenus we should"). Decisions taken the same day (section 6): all
as recommended, and Joe's idea for copying (section 3.1). Submenus asked of rinch
(#2214).

## 1. Where it stands

- **A node's row menu has 19 items** in one flat list, no dividers: Open in Split View,
  New Node, New Canvas, New Sheet, New Discussion, New Kanban (and any other plugin
  recipe), Start a Discussion, New Canvas from Picture..., Paste Picture, Mount Store...,
  Mount Remote Store Here..., Copy Link, Copy as Mount Source, Paste Mount Here, Rename,
  Share..., Appearance..., Show as Outline (metanodes), Delete.
- **A store's row menu has 21**: the same "New" items, Paste Picture, the four mount
  items, Copy Link, then Link to Remote..., Host on Pimble Cloud..., Share..., Stop
  Sharing from This Computer..., Unlink from Remote, Remove Replica..., Recently
  Deleted..., Appearance..., Close Store. Several are greyed out on most stores (Stop
  Sharing, Unlink, Remove Replica, Paste Mount Here) and still take a line.
- Two items look alike and are not: "New Discussion" (a discussion node, a child) and
  "Start a Discussion" (a discussion on this node). Three "paste"-like items each paste
  one kind of thing (Paste Picture, Paste Mount Here, and Edit's paste of cut nodes).
- **The explorer's "+"** makes an empty text node under the root of the selected row's
  store (`app.rs` `on_new_node`). Canvases, sheets, discussions and kanbans exist only in
  the row menu.
- The New items are written out twice, once per menu (`app.rs`), with the plugins'
  recipes (`AppStore::recipes`) appended to each.
- **rinch**: the app menu bar and the tray have submenus; `DropdownMenu` and
  `ContextMenu` do not (only a separate dropdown opened by click inside one). Cascading
  submenus were asked of the Rinch master session on 2026-11-04 (hover, click and
  ArrowRight open a flyout; flips at the window's edge; choosing an item closes the whole
  tree; desktop and web).

## 2. Principles

1. **One list of what can be made.** A `creating` module answers the kinds a person can
   make here, in order: Text, Canvas, Sheet, Discussion, then each plugin recipe (Kanban),
   then Canvas from Picture...; each with its label, icon, and what it creates
   (`recipes::*` or the plugin's recipe). The "+" button, the row menus' New, and File >
   New all read it, so a new kind (a plugin's) shows up everywhere at once.
2. **Show what applies, hide what cannot.** An item that can never apply to this row
   (Unlink on a store with no link, Remove Replica on a store that is not one, Stop
   Sharing on a store not shared) is not shown. An item that applies but is blocked now
   (New under a node this device may only read) stays, greyed, with the reason as its
   tooltip.
3. **Group by what the person is doing**, frequent first, with dividers: open and make;
   name and look; copy, paste and share; delete last.
4. **Rare and related actions go in a submenu**, not a dialog: they stay one gesture
   away and the top level stays short.
5. Web and desktop draw the same menus from the same code.

## 3. The menus, proposed

A node:

```
Open in Split View
New ▸                 Text · Canvas · Sheet · Discussion · ─ · Kanban · ─ · Canvas from Picture...
Discuss               (a discussion on this node; was "Start a Discussion")
─────────
Rename
Appearance...
View as ▸             Outline · Kanban ...   (a metanode; replaces "Show as Outline")
─────────
Cut
Copy
Paste                 (section 3.1)
Mount ▸               Mount Store Here... · Mount Remote Store Here...
Share...
─────────
Delete
```

12 lines at the top level instead of 19.

A store:

```
New ▸
Copy
Paste
─────────
Share...
Sync ▸                Link to Remote... · Host on Pimble Cloud... · Unlink from Remote ·
                      Stop Sharing from This Computer... · Remove Replica...   (only those that apply)
Mount ▸
─────────
Appearance...
Recently Deleted...
─────────
Close Store
```

10 lines instead of 21.

A mount row stays as it is (New, Share, Appearance, Delete), with New ▸ making in the
mount's source as it does now.

### 3.1 Copy and Paste (Joe, 2026-11-04: "'copy' copies everything, paste could choose what to paste")

- **Copy** takes the row (or the selected rows): their `pimble:` links go on the system
  clipboard, as "Copy Link" did, and the app remembers the nodes, as "Copy as Mount
  Source" did. "Copy Link" and "Copy as Mount Source" are gone; Ctrl+C in the explorer is
  Copy.
- **Cut** marks the rows to move (Ctrl+X, as now).
- **Paste** is one item. What it can do is judged when it is chosen, from what is held
  then (a menu is drawn before anyone opens it, and cannot know what the system clipboard
  holds): move the cut rows here (inside the row, last); mount the copied node here; make
  a canvas of the picture on the clipboard (desktop; the browser pastes a picture with
  Ctrl+V on a selected row, as now). One choice is done at once; several are offered in a
  small chooser naming each ("Move “Plan” here", "Mount “Apex Kanban” here", "The picture
  as a canvas"); none says "There is nothing here to paste." Paste a duplicate of a copied
  node is not offered: Pimble has no duplicate yet.

### 3.2 The "+" button

**The "+" button** opens the same New list as a dropdown, and makes the chosen kind where
the person is (decision 1). File > New ▸ in the menu bar offers the same list for the
selected row.

## 4. What does not change

Every action keeps its code path and its judgement (`plan::create_parent`, the access
guards, the refusals); this moves and groups them. Keyboard creation in the explorer
(Enter, Insert, Tab; `tree_keys.rs`) keeps making text nodes.

## 5. Waves

1. **Without submenus** (no rinch dependency): the `creating` list; "+" opens it as a
   dropdown; File > New ▸ in the menu bar (which has submenus); hide what cannot apply;
   one Paste; "Discuss"; dividers and the new order. The New items stay listed flat
   in the row menus until wave 2.
2. **With rinch's submenus**: New ▸, Mount ▸, Sync ▸, View as ▸ in the row menus.
3. **Checked** in the release desktop app and the browser: each menu on a plain node, a
   metanode, a mount, a plain store, a linked store, a replica, a shared store, a
   read-only share; "+" making each kind.

## 6. Decisions (taken 2026-11-04: all as recommended)

1. **Where "+" makes the new node.** (a) Beside the selected row, after it, as Enter in
   the explorer does (recommended: where you are looking; under the store's root when a
   store row is selected); (b) inside the selected row, as its last child; (c) under the
   store's root, as now.
2. **What a click on "+" does.** (a) Opens the New list (recommended: one click more for
   text, nothing hidden); (b) a split button: the "+" makes text, a small arrow beside it
   opens the list.
3. **Hidden or greyed** for items that can never apply: hidden (recommended), or greyed as
   now.
4. **The store's sync and hosting items**: a Sync ▸ submenu (recommended), or a "Store
   Settings..." dialog holding them with the store's sync state.
5. **One Paste** for cut nodes, a picture and a mount source (recommended), or keep three.

## 7. Built: wave 1 (2026-11-04)

- **`creating.rs`**: `kinds` (Text, Canvas, Sheet, Discussion, each plugin recipe, Canvas
  from Picture...), `make` at a `Place` (the canvas, sheet, discussion and recipe paths
  take one now; `MakeFromRecipe` carries it), `make_in_row` (a row menu's New: inside the
  row, last), `where_plus` and `make_from_plus` (beside the selected row, after it; a store
  row: under its root; nothing selected: the first store's root). A text node made by "+"
  or File > New is named in place.
- **"+"** opens the list as a dropdown (`AppStore::new_menu_open`); with no store open the
  browser offers a new store as before.
- **Row menus** regrouped with dividers as section 3 shows, New listed flat until rinch's
  submenus (#2214); "Discuss"; "Mount Store Here..."; a store's Link to Remote, Host, Stop
  Sharing, Unlink and Remove Replica shown only when they apply; New, Mount and
  Appearance greyed (not hidden) where the person may only read. Node menu 19 items to 17
  at the top level today (12 with submenus); store menu 21 to 16 at most.
- **Copy, Cut, Paste** (`pasting.rs`, section 3.1): Copy puts the rows' links on the
  clipboard and remembers the first for a mount (`AppStore::mount_source`, not for a row of
  an encrypted store); "Copy Link", "Copy as Mount Source" and "Paste Mount Here" are gone,
  and with them the tree rebuild a copy used to need. Paste judges its choices when chosen
  and offers several in a chooser ("Move “Sheet” here", "Mount “Done” here", "The picture,
  as a canvas").
- **Ctrl+V in the explorer is a straight paste** (Joe, 2026-11-04): what was cut, moved
  after the cursor row (Ctrl+Shift+V into it), as before; with nothing cut, no chooser: the
  picture on the clipboard as a canvas if there is one, else a mount of the copied node.
  Ctrl+C is Copy.
- **The menu bar** (Joe: "the top system menu should have the same behavior"): File > New ▸
  (Text, Canvas, Sheet, Discussion, More..., Canvas from Picture...; a menu bar is built
  before the plugins load, so "More..." opens the "+" list, which has their kinds);
  Edit > Cut Node, Copy Node, Paste into Node... on the explorer's selected row, without
  shortcuts (Ctrl+X, C and V belong to the text in a note and to the explorer in the
  explorer); Edit > "Discuss" for "Start a Discussion".
- **Checked in the release desktop app** (port 7473, scratch dirs): the node and store
  menus as above; "+" listed every kind including Kanban and made a Sheet right after the
  selected To do; Cut "Sheet", Copy "Done", Paste on Work offered both, and Move put Sheet
  last in Work; File > New ▸ Text made a node after the selected row and took its name in
  place. pimble-app's 206 tests pass. Not tried: the browser; a picture paste from the
  menu.
