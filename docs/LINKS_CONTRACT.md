# Links contract: node links and deep links, made in the text and followed with a click

Status: written by the PM, 2026-09-23, from Joe's decisions of that day; approved by him the
same day ("Ok, go for it"). Wave 1 in progress on branch `links`. The rinch section
comes from a survey of rinch `main` (7bdc352) and lists the six upstream PRs this needs.

## The decisions (Joe, 2026-09-23)

1. **Making a link:** typing `[[` opens a search-as-you-type picker that inserts a link
   titled with the target's name; Ctrl+L (Cmd+L) links the selected text through the same
   picker (Ctrl+K stays the search box's: Joe, 2026-09-23); a tree row's menu has
   "Copy Link", and pasting a Pimble link over selected text links it.
2. **Following a link:** Ctrl+click (Cmd+click) opens the target. Hovering a link shows the
   target's title and the modifier hint. A plain click places the caret, as today.
3. **Deep links** (to a spot inside a document) are built in the same wave as node links.
4. **Backlinks have no UI yet.** They wait for a metadata section or view that is still to
   be designed. The index does learn every node's links (below), so that view has its data
   the day it is built.
5. **A link follows a moved node.** A node that leaves a share or its store becomes a new
   node (`docs/MOVE_CONTRACT.md`). Its tombstone records what it became, and a link to it
   follows that. A reader who cannot reach the new place gets a sentence saying so: the
   link breaks for them, correctly.

## Where a link lives: in the text, and nowhere else

A link is rinch's `link` mark on a run of a node's text, with `href` a Pimble URL. It is
in the collaboration scope already, so a link is co-edited, merged, undone and moved with
the words it sits on, by the one write path there is (`applyEdit`). There is no second
list of a node's links to keep in step with the text.

- `Node.links` / `NodeLink` / `LinkTarget` (`pimble-core`, never written by anything) are
  removed. What a node links to is **derived from its content** by `pimble-crdt`:
  `NodeDoc::links() -> Vec<LinkRef>` (and `links_of(bytes)`), each `LinkRef { target:
  PimbleUrl, path: String, text: String }`, where `path` is the block's `"b:{ordinal}"`
  (the same locator as its index units) and `text` the linked words. It reads the
  `content` root whatever the node's type, so a plugin's units and a node's links never
  disagree about where the text is.
- `build_index_node` (server) feeds the index's `links` edges from `NodeDoc::links`,
  keeping only targets in the same store and not the node itself (the index is per
  store; links into other stores wait for the backlinks view, see "Later").
- A link to the web (`http:`/`https:`) is the same `link` mark with that href, made,
  shown, followed and edited the same way as a Pimble link (Joe, 2026-09-23: "it would be
  nice if the user experience was unified"): see "One experience for both kinds". Any
  other scheme is left as it is: shown, never followed.

## The URL

```
pimble:<store uuid>/<node uuid>
pimble:<store uuid>/<node uuid>#<anchor>
```

- Always the canonical `(StoreId, NodeId)`, never a tree path: a link made through a
  mount names the source store's node, exactly as every address in the app does.
- `<anchor>` (deep links): `p=<sticky>&q=<quote>`. `sticky` is a yrs `StickyIndex` into
  the target block's text, v1-encoded, base64url without padding; it names a character's
  identity, not an offset, so it survives any edit that keeps that character or its
  neighbours. `quote` is up to 48 characters of text starting at the spot,
  percent-encoded: the fallback when the sticky position cannot be resolved (its block
  was deleted, or the node was transplanted and its content has a new history).
- Parsing and printing is one type, `pimble_core::PimbleUrl { store, node, anchor:
  Option<Anchor> }` with `Display`/`FromStr`; nothing else builds or splits the string.
  An `href` that does not parse as one is treated as an external URL.

## Following a link

`follow_link(url)` in the app (one implementation, desktop and web) resolves in order:

1. **The store.** Open here: go on. Known here but closed (registry, a replica, a mount's
   `source_path`): open it as a mount resolution does. Otherwise: "This link is to a store
   that isn't on this device."
2. **The node.** `getNode`. If it is tombstoned and records `became`, follow that (one
   hop at a time, at most 8, a cycle stops). If it is tombstoned with no `became`: open it
   read-only with the one-line notice "This note was deleted." and "Put Back" where the
   reader may. Denied (`-32004`), not in a share the reader holds, or unknown: "You don't
   have access to where this link points." / "This note no longer exists." The last hop
   decides the sentence: a `became` that the reader cannot reach reads as no access.
3. **Open it** through `open_node` (the one way to open a node; it fetches a node the tree
   has not loaded, as a search hit does). With an anchor, once the editor session is up,
   `NodeDoc::resolve_anchor`: the sticky index when the quote still reads there; else
   the first place the quote reads (the anchored characters were deleted and the words
   written elsewhere: a paragraph cut and pasted, or rewritten wholesale, and yrs
   resolves a deleted character to where it was); else the sticky index (the words were
   edited in place); else the top. Put the caret there, scroll it
   into view, and highlight the block's line briefly (about 1.5 s).

Following never writes to the target: a reader can deep-link into a node they may only
read.

### `became`

`Tree::remove_transplanted` (the second half of every transplant, within a store or
between two) writes `became = "pimble:<store>/<node>"` into the tombstoned original's
`node` root (a field beside `deleted_at`) in the same transaction as the tombstone, for
every node of the cut subtree, each naming its own new id (`Tree::plant` answers the
pairs as `Planted`). `Node.became` carries it to whoever reads the node. It is an id only: nothing of the new
place's title, content or path. `undeleteNode` ("Put Back") leaves `became` alone; a live
node's `became` is never followed, so a put-back original is simply itself again. The web
vault client transplants through the same `Tree` code, so it writes the same field.

## One experience for both kinds

Every rule below applies to a Pimble link and a web link alike; only what "open" means and
what the tooltip names differ.

| | Pimble link | Web link |
|---|---|---|
| Make it | `[[`, Ctrl+L picker, paste | Ctrl+L picker (type or paste a URL), paste, and typing a URL followed by a space |
| Hover | the target's title and store, or why it can't be reached | the URL (its host in bold) |
| Ctrl/Cmd+click | `follow_link` in the app | the system browser (desktop: `open::that`), a new tab (web: `window.open`, `noopener`) |
| Ctrl+L inside it | Edit Link (the picker again), Remove Link | the same |
| Look | the link colour, underline on hover | the same, plus a small "opens outside" arrow after the words |

The picker takes both: while what is typed parses as an `http(s)` URL (or looks like a
bare domain, `example.com/x`, which becomes `https://`), its first row is "Link to
<url>"; the node search rows follow. A plain click on either kind places the caret; nothing
ever leaves the app without the modifier.

## Making a link

- **`[[`**: typing `[[` opens the picker at the caret. What follows is the query (search as
  you type, the existing `search` RPC across open stores, titles first); Up/Down move,
  Enter or a click picks, Escape or a caret leaving the `[[` run closes it and leaves the
  text as typed. Picking replaces `[[query` with the target's title linked to it, in one
  editor transaction (one undo step). Holding Alt (Option) while picking inserts only the
  link over the query text as typed.
- **Ctrl+L** with a selection opens the same picker (query empty) and links the selection
  to a node or a URL;
  with the caret inside a link it offers "Remove Link" and "Edit Link" (the picker again).
  With nothing selected and not in a link it does nothing.
- **Copy Link** on a tree row puts `pimble:<store>/<node>` on the system clipboard.
  **Copy Link to Here** (editor context menu and Ctrl+Shift+L) copies a deep link to the
  caret.
- **Paste**: plain text that parses as a Pimble URL or an `http(s)` URL, pasted over a
  selection, links the selection. With no selection, a Pimble URL inserts the target's
  title (or the quote, for a deep link) linked to it, and a web URL inserts itself,
  linked.
- **Typing a web URL** followed by a space or Enter links it (an input rule on the text
  before the caret); Ctrl+Z right after takes the link back and keeps the text.
- Linking needs write access to the node being edited, as any edit does. It needs nothing
  of the target.

## What a link looks like

The link colour (`--rinch-primary-color-4`), no underline until hover; a web link also
carries a small arrow after its words (CSS on `a[href^="http"]`, not text). Hover after
400 ms: a tooltip with the target's title and store name (or the sentence from "Following"
when it cannot be reached), or a web link's URL, and "Ctrl+click to open". A link whose target is known to be missing or
unreachable is not restyled in the text (that would be a second source of truth about the
target); the tooltip says so.

## rinch

Surveyed on rinch `main` (7bdc352), 2026-09-23. **What is there already:**
- The `link` mark renders as `<a data-pm-mark="link" href=…>`, and `is_safe_url` keeps a
  `pimble:` href through HTML, markdown and collaboration.
- Ctrl+L needs nothing new: a pimble editor plugin's `keymap()` binds `Mod-l`, which is
  unbound today. In the browser Ctrl+L is the address bar's, so the web editor must
  `preventDefault` a key its keymap handled; wave 3 checks that it does.
- Replacing `[[query` with linked text is one `handle.update` transaction (`replace_with`
  plus `set_stored_marks(Some(vec![]))`, so the next typed character is not linked).
- The `[[` trigger is an `InputRule` on `\[\[$`.
- The brief highlight is an inline `Decoration` from a pimble plugin (#847 is on `main`).

**What pimble must respect:**
- `add_plugin` resets history, so plugins are added before content loads.
- Plugin callbacks run while the handle is borrowed. Anything that touches the handle is
  deferred with `set_timeout(0, ..)`.
- `on_change` is a single slot.

**Missing, as six PRs upstream, one at a time.** Pimble points at each branch until it
merges.

1. **Link activation and hover.** Add `EditorHandle::link_at(pos) -> Option<(href, from, to)>`,
   `on_link_click(href, range, modifiers)`, and `on_link_hover(Option<(href, rect)>)`.
   - On the desktop, `try_new_editor_click` places the caret only and knows no links.
   - On the web, a click on an editor `<a href>` is probably followed by the browser today:
     the mousedown is prevented, the click is not. Check this first. The PR prevents it
     for `a[href]` inside the editor.
2. **Caret geometry, editor key interception, selection changes.**
   - `caret_rect(pos)` places the picker. Both platforms compute it privately today.
   - An editor-level key interceptor runs before cursor and keymap handling. Without it the
     web editor consumes arrows and Enter before the app sees them, and the picker could
     not be driven from the keyboard in the browser.
   - `on_selection_change` closes the picker when the caret leaves the `[[` run.
3. **A paste hook in the core.** `Plugin::transform_pasted` (or `handle_paste`), consulted
   in `replace_selection_with_html`/`_text`, so both platforms get it from one place. Today
   neither platform lets an app see an editor paste.
4. **Sticky positions in collaboration.**
   - `EditorHandle::collab_sticky_index(pos, assoc) -> Vec<u8>` and
     `collab_resolve_sticky(bytes) -> Option<Pos>`, over `CollabDoc`, which handles the
     UTF-16 offsets itself.
   - Pimble also resolves anchors without an editor (in `pimble-crdt`, for a node not open).
     That uses yrs directly on the same `content` root, so the encoding must be a plain yrs
     `StickyIndex` v1, not a rinch wrapper.
5. **Programmatic focus and scroll.** `EditorHandle::focus()`, and `scroll_into_view(pos or
   range)` that works without focus and for a range, including a caret refresh on the web
   outside input events. Today only the focused editor gets a caret pass, focus comes only
   from a click, and a range never scrolls.
6. **A link mark that does not grow.** Marks behave as inclusive, so typing just after a
   link extends it, and a caret just past a link reports that link. The PR adds
   `inclusive: false` to mark specs and sets it on `link`.

Opening an external URL is pimble's own business, not rinch's: `open::that` on the desktop,
`window.open` in the browser.

## Waves

1. `pimble-core`/`pimble-crdt`: `PimbleUrl`, `NodeDoc::links`, `became` in transplant,
   sticky anchor encode/resolve on a `NodeDoc`, removal of `NodeLink`; index fed from
   content. Tests: URL round trips, links derived from marks, `became` on every node of a
   transplanted subtree and kept through Put Back, a sticky anchor surviving edits before
   and after it and falling back to the quote after its block is deleted and after a
   transplant.
2. The six rinch PRs of the section above, one at a time, in the order listed. Wave 1
   needs none of them: it resolves anchors with yrs directly.
3. The app, desktop and web: picker, Ctrl+L, Copy Link, Copy Link to Here, paste, hover,
   Ctrl+click, `follow_link`. Steps 1 and 2 of "Following" are one RPC, `resolveLink {
   url } -> Live { store, node } | Deleted { store, node } | NoAccess | Missing |
   StoreNotHere`: `getNode` refuses a tombstone, and only the server can judge access at
   each `became` hop (the web vault client answers it in the page from the documents it
   holds, as it answers every tree command). Server-boundary tests for each answer.
4. The PM's walk-through: both apps, a link across stores, across a mount, into a share a
   second account holds and one it does not, through a transplant, a deep link surviving
   edits by the other window.

## Later

- The backlinks view, with the metadata section (decision 4). Cross-store backlinks need
  either each store's index to keep incoming links from other stores, or a query across
  every open store's index; decided with that view.
- Registering `pimble:` with the operating system, so a link in an email opens the app.
- Link titles that follow a renamed target (today the words are what the writer typed or
  picked, and stay so).
