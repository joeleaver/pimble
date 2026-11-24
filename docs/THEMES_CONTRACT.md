# Themes contract: how the app looks, and how a tree reads

Status: proposed 2026-11-24, nothing built. Asked for by Seth; sketched by Joe the same day
(two kinds, any node can have one, themes live in the store and copy easily between open
stores, plugins themed, about a dozen built in, maybe replacing the paragraph setting).
Written against the code at `ed48772` and rinch `main` `e8e04ccc`. The decisions Joe must
take are numbered in section 12, each with a recommendation; nothing is built until he
has answered them. All answered by Joe on 2026-11-24: decisions 1 to 12 and 8b.
Ready to build, wave by wave (section 13).

**Two kinds, kept apart:**
- **App themes** dress the chrome: the explorer, the panes' frames, menus, dialogs, the
  toolbar, the status bar, plugins' viewers. They are **personal**: what one person picks
  changes nothing anyone else sees.
- **Document themes** decide how text reads: one tree like a book, another like code,
  another like plain Markdown. They are **shared**: a theme applied to a tree is part of
  that tree, and everyone who opens it sees it the same way.

## 1. What exists today

- **The scheme.** `app/theme.rs`: `theme_props(dark_mode)` gives rinch a primary color
  (`"blue"`), the scheme and a radius; `theme_css` appends Pimble's own variables
  (`styles::LIGHT_THEME_VARS` redefines rinch's semantic variables, `--rinch-color-body`,
  `-surface`, `-text`, `-dimmed`, `-border`, the primary ramp `--rinch-primary-color-0..9`,
  the title bar; `DARK_THEME_VARS` adds the toolbar's). The result is put in force with
  `rinch::core::set_current_theme_css`, the one sheet rinch replaces on a switch. The
  editor follows with `EditorHandle::set_dark_mode` (its `data-pm-theme` attribute).
  `state.json` `dark_mode` (`localStorage` in the browser) remembers the choice; Settings >
  General holds it and View > "Toggle Dark Mode" is its shortcut.
- **The app's stylesheets** (`crates/pimble-app/src/styles/*.css`, 4,600 lines) use rinch's
  semantic variables and Pimble's `--pimble-*` ones, never the dark palette directly
  (CLAUDE.md "Tree appearance"). An app theme can therefore restyle the chrome by
  redefining variables, which is exactly how the light scheme was made on 2026-10-20.
- **Editor content** is styled by `styles/editor_content.css` under the pane's wrapper,
  `.pimble-editor__content-wrap > [data-pm-editor] ...`: 15px, line height 1.7, heading
  colors, links, hashtags, mentions, text color fixes. The font is rinch's default.
- **Paragraph spacing**, the "collapse paragraphs" feature (`docs/PARAGRAPHS_CONTRACT.md`,
  built 2026-11-13). Each node says `custom["paragraphs"]` = `"spaced"` or `"compact"`
  (absent is compact). A new node **copies** its parent's value when it is made
  (`custom_keys::INHERITED`), so changing a folder later changes none of the notes in it,
  and a note never needs its ancestors to be drawn: a share's member may hold none of them.
  Edit > "Compact Paragraphs" and the Appearance picker set it; the importers mark what
  they make spaced. It is drawn by one class, `pimble-editor__content-wrap--spaced`, on the
  pane's wrapper (`pane_view/pane.rs:85`), and `editor_content.css` sets `p { margin: 0 }`
  unless that class is there. Only paragraphs change.
- **Row appearance** (icon, text color, background) lives in `metadata.custom` keys, and
  `pimble_ui::appearance::display_color` lifts a dark stored color on the dark scheme and
  caps a light one on the light scheme at render time; the stored value is never altered.
- **Plugins** draw with inline styles only (a plugin cannot add a stylesheet,
  `pimble_ui::viewer`), and their DOM is applied by the host into the app's own document
  (`docs/PLUGINS_CONTRACT.md` 3.2), so inherited custom properties reach them. The viewer
  look uses semantic variables for text and accents, but its surfaces
  (`viewer::surfaces(dark)`) name palette steps: `--rinch-color-dark-4..6` in the dark
  scheme, `--rinch-color-gray-1/3` and `white` in the light one. The ABI tells a plugin
  only `HostState.dark` (ABI 3).
- **Nothing** today lets anyone choose a font, a measure, a size or a color for text.

## 2. Format: tokens, never CSS

A theme is a **fixed, versioned set of typed tokens**. There is no field that holds CSS,
selectors or URLs. Each token has a type, and the app checks every value against it when
it reads a theme; a value that fails is ignored (the base's value stands in) and the theme
editor says which token and why.

Why tokens:
1. **Themes travel in shared stores.** A document theme is store content and reaches
   every member of a share. Free CSS from someone else is untrusted input to the renderer:
   it could hide text, overlay the chrome, fetch remote URLs (in the browser), or exhaust
   layout. Typed values cannot.
2. **Two renderers.** rinch's desktop engine and the browser (rinch-web, real DOM) must draw
   the same theme the same way. A token maps to properties both are known to support
   (section 7); arbitrary CSS would draw differently on each.
3. **Tokens outlive rinch's markup.** The editor's DOM and class names are rinch's and
   change. Tokens are Pimble's, and one stylesheet in Pimble maps them onto whatever rinch
   draws today.
4. **The MCP and the theme editor can read and write them** field by field, and two people
   editing different tokens of one theme merge.

Token types: `color` (`#rrggbb`), `font` (one of the font stacks in section 5, by name),
`length` (a number and a unit, `px` or `em`, within a range per token), `number` (line
height, a ratio, within a range), `enum` (a fixed set of words), `bool`.

How tokens reach the screen: the app turns a theme into a string of custom properties
(`--pd-font-body: ...; --pd-p-spacing: ...`) written onto the element it applies to (the
pane's wrapper for a document theme, `:root` through the theme sheet for an app theme). The
values are produced by Pimble from checked typed values, never copied from the document,
so nothing a theme holds can end a declaration or open a rule. One stylesheet
(`styles/document_theme.css`, new) consumes them with `var(--pd-..., <default>)`.

### 2.1 Document theme tokens

| Group | Tokens |
| --- | --- |
| Page | `measure` (max line length: `none` or 40–100 `em`, centered when set), `page_padding` (length); the colors are the palette below |
| Body | `font_body`, `size` (12–24 px), `line_height` (1.0–2.2), `letter_spacing` (optional), `align` (`start`, `justify`) |
| Paragraph | `spacing` (`compact`, `spaced`, `indented`: a book's first-line indent and no gap, the first paragraph after a heading not indented), `indent` (length, for `indented`), `gap` (length, for `spaced`) |
| Headings 1–6 | per level: `font` (default: `font_heading`), `size` (ratio of body), `weight` (`normal`, `medium`, `bold`), `case` (`none`, `upper`), `tracking` (letter spacing), `align` (`start`, `center`), `space_before`, `space_after`, `color` (optional), `rule_below` (bool) |
| Quote | `style` (`bar`, `indent`, `italic`, `boxed`), `font` (optional), `color` (optional) |
| Code | `font_code`, `size` (ratio), `background` (color, optional), `wrap` (bool) for code blocks; inline code shares font and size |
| Lists | `marker_color` (optional), `item_spacing` (length) |
| Tables | `style` (`grid`, `rows`, `minimal`), `header` (`bold`, `shaded`), `font` (optional), `size` (ratio) |
| Rule | `style` (`line`, `ornament`: a centered short rule, `space`) |

**The palette** (decided, Joe 2026-11-24, decision 7): a fixed set of named color slots,
each a **pair**, one value for the light scheme and one for the dark:

| Slot | Used for |
| --- | --- |
| `page` | the page behind the text |
| `text` | body text and headings without their own color |
| `dimmed` | h5/h6, captions, a quote's text when its style dims it |
| `accent` | links, hashtags, mentions, list markers without their own color |
| `rule` | horizontal rules, table lines, a quote's bar |
| `code_bg` | code blocks and inline code |
| `highlight` | the highlight mark's default |

Per-block `color` tokens in the groups above name a slot (`accent`, `dimmed`, ...) or give
their own pair. Every slot is optional: unset, it comes from the base (section 3.1), and
where no theme in the chain sets it, from the app theme, so a theme that sets no color
reads in dark and light alike. A pair is set as a pair: the editor shows both values side by
side, and setting one offers the other filled in ("Suggest the dark one": the light value
through the same lift `display_color` applies to row colors), which the person may keep or
change. The values drawn are the ones chosen; `display_color` is not applied to them.
Built-ins come with their palettes filled in, so picking Paper or Book gives both schemes
without anyone choosing a color. Members who view a shared tree in the other scheme see
the theme's values for that scheme, not their own app theme's.

### 2.2 App theme tokens

The app theme is the set of variables the chrome already reads, nothing new to style:
rinch's semantic colors (`body`, `surface`, `text`, `dimmed`, `border`, `default`,
`default-hover`, `filled`, `filled-hover`, `placeholder`, `option-hover`,
`option-selected`), the primary ramp (given as one color; Pimble derives the ten steps, as
the light scheme's ramp around `#3f6491` was derived), the title bar's two, Pimble's own
(`--pimble-toolbar-*`, `--pimble-sheet-ghost`, the viewer surfaces of section 8), the
radius (`xs`..`xl`), the UI font (a stack from section 5) and the UI size (13–16 px).
**Every theme has its own light and dark** (Joe, 2026-11-24). An app theme is never "a dark
theme" or "a light theme": every color token above is a pair, one value for each scheme,
like a document theme's palette (section 2.1), and the primary ramp is derived per scheme
from that scheme's primary. Radius, UI font and UI size are shared by both. Which scheme
shows is a separate preference (section 3.3), so switching dark and light keeps the theme
and shows its other half. The scheme still drives the editor's `set_dark_mode`,
`display_color` and every color Pimble derives, so the existing dark/light logic stays the
one place that judges legibility. A theme with a base inherits each half of each pair it
does not set, so a tweak to only the dark side of Paper is one token.

## 3. Where themes live

### 3.1 Document themes are nodes

A document theme is a **node of the store**, `node_type = "theme"`, its tokens a yrs map in
`data.theme` (each token one key, so concurrent edits of different tokens merge, and two
edits of one token resolve as any map key does). Being a node, it is co-edited by everyone
who may edit it, replicates, encrypts in a vault store, is undeletable from Recently
Deleted, and has a link (CLAUDE.md "Everything is co-editable": no copy kept in step, no
projection). Its title is the theme's name; its content is free text, and the theme pane
shows a live sample under the token editor (section 9).

**Every theme inherits** (Joe, 2026-11-24: people should not have to build a whole theme
at once; they tweak a few things). A theme node names a **base**: `data.theme.base` = a
built-in's id (`builtin:book`) or another theme node of the same store, and **sets only the
tokens it changes**; every other token, palette slot and half of a pair comes from the
base, its base, and so on, then the built-in Default. Bases chain to any depth (a cycle is
refused when it is made and, if concurrent edits make one, broken at the theme with the
smallest id, which then reads its base as Default). A change to a base shows in every theme
built on it, because nothing is copied: "Book, but 17 px and a wider measure" is two
tokens, and it follows Book as Book is tuned.

The usual way to make a theme is therefore **"Customize..."** in the Theme picker, on
whatever theme is in force: it makes a new theme node whose base is that theme and which
sets nothing, applies it where the old one was applied (on the node the picker is for, or,
asked, in the old theme's place on the node that named it), and opens it in the editor.
Customizing a built-in leaves the built-in as it is. Customizing a theme from another open
store first copies that theme into this store (section 6), then bases the new one on the
copy. A base in another store is not allowed (it would make one store's look depend on
another being open).

### 3.1a Internal nodes, hidden by kind

Decided (Joe, 2026-11-24, decision 12): theme nodes are nodes of the tree, and they are
**internal**: hidden by their type, with each kind of internal node shown or hidden on its
own. Pimble will have several kinds (themes now; templates, saved queries and the like
later), so this is a general mechanism, not a theme feature:

- **A node type says whether it is internal.** `pimble_core` keeps the list of internal
  kinds (`NodeType::internal_kind() -> Option<InternalKind>`; `InternalKind::Theme` first),
  with each kind's name for menus ("Themes"). Nothing is set on the node: a theme node is
  internal because it is a theme. A plugin's node types (later) may declare a kind of their
  own in their manifest.
- **Hidden means not shown, never not there.** An internal node, and anything under it, is
  left out of the explorer, search results, metanode sources, the Properties panel's
  references and the MCP's `list_children` and `search`, unless its kind is shown. It
  still replicates, encrypts, is reached by shares (section 3.2), opens by link, and is
  listed by the tools made for it (the Theme picker, `list_themes`).
- **Show or hide each kind.** View > "Show Internal" is a submenu with one check item per
  kind ("Themes", ...), all off by default. A shown kind's nodes appear where they are in the
  tree, dimmed and marked with the kind's icon, and can be moved, renamed and deleted like
  any node. The choice is a view preference kept per device with the explorer's expanded
  rows (`views.json`, `localStorage` in the browser), because it is how one window shows
  the tree, not anything about the store. The MCP takes `include_internal: [kinds]` on its
  listing tools.
- **Where new themes go.** Anywhere: "New Theme", "Customize..." and a copy make the theme
  node as a child of the store root by default, since it is hidden there and a root is in
  every store; a person who shows Themes may move one anywhere (into a share, to give its
  members the theme with the share's own access). There is no Themes folder: hiding by
  kind does what the folder was for.
- **Reaching a hidden theme.** The Theme picker lists every theme in the store wherever it
  sits; "Edit Theme..." beside one opens its node in the focused pane. A link to a theme
  node opens it whether or not Themes are shown.
- **Counting.** The store row's tooltip says how many internal nodes are hidden ("12 themes
  hidden"), so a store is never silently larger than it looks.


### 3.2 Applying a document theme

A node names a theme with one key, `custom["theme"]` (`pimble_core::custom_keys::THEME`):
a built-in id (`builtin:book`) or a theme node's id in the same store. It is an
`applyEdit` of that key like any other appearance key (`BackendCommand::SetNodeAppearance`
grows `theme`), so applying is shared: everyone sees the book layout.

**The theme in force** for a node is the one named by the node itself or its nearest
ancestor that names one; with none, Default. Looked up, not copied, which is the reverse
of what paragraphs chose (decided, Joe 2026-11-24, decision 3; any node with children is a
"folder" here, nothing special marks one): "this tree reads like a
book" means changing the theme on the tree's root changes the whole tree, including notes
already in it.

Lookup has two edges the copy rule never had:

- **Shares.** A member's partial replica holds the share's root and below, not its
  ancestors, and may not hold the theme node either (it may sit outside
  the share). **Decided (Joe, 2026-11-24, decision 4): a scope's reach includes the
  theme documents in force in it**: the theme named by the share root's nearest themed
  ancestor, every theme named inside the share, and their bases. The owner's share upkeep
  already publishes scope sets as a union that only grows (`share.rs`,
  `docs/MOVE_CONTRACT.md`); it adds these, and the reach rule (`Reach` in `handler.rs`)
  grants them at the share's **theme access**. The share root's inherited theme is recorded
  on the share (the accounts service already stores a share's name), so a member resolves
  "the theme in force above my root" without holding the ancestors.

  **Theme access is chosen when the share is made** (Joe, 2026-11-24), in the Share dialog,
  and can be changed later from Shares...:
  1. **Read-only theme** (the default). Members see the theme and cannot change it; they may
     still apply another one (a built-in, or a theme inside the share) to nodes they may edit.
  2. **Write.** Members may edit the theme documents in force in the share, as the share's
     own editors. Their edits are edits of the owner's theme, so every other tree using that
     theme changes too; the dialog says so in a sentence naming how many other places use
     it ("Book is also used by 3 other trees in this store; members' changes show there
     too."). Offered only to a share whose role is editor; a reader share is always
     read-only.
  3. **Copy a theme for this share.** The theme in force is copied once (section 6's copy,
     within the store) into a new theme node, "<Theme> for <share name>", made where new
     themes are made (section 3.1a); the share root names the copy; members get write on the copy only.
     Members restyle their share freely and nothing else in the owner's store changes. The
     owner sees the copy's look on that subtree too, because it is applied there.

  The access is a property of the share's grant (`(user, store, root node, role)` gains
  nothing: the share itself records `theme_access: read | write`, the same for every member
  of the share), judged by `Reach` like any other document. A theme a member can only read
  takes no edits with `StoreAccess::READ_ONLY_REFUSAL`'s sentence. The owner's devices hold
  the whole picture, so nothing here puts a theme on Pimble Cloud that was not already
  there: a hosted or relayed store's theme nodes are store documents like the rest.
- **Mounts. Decided (Joe, 2026-11-24, decision 5: "mounting a node anywhere should
  preserve its native look"):** the theme in force is resolved in the source store's own
  tree, and the mounting store's chain is ignored. A node is addressed everywhere by its
  canonical `(StoreId, NodeId)`, and it reads the same wherever it is opened: through a
  mount, directly, or in two panes by both paths at once. A mounted subtree of code notes
  stays code-like inside a book. A theme set on a mount node itself would apply to nothing
  (a mount has no content of its own), so the Theme picker on a mount row acts on the
  source node, as every other edit through a mount does, judged by the access to the source
  store.

Resolution is a pure function of the chain's documents (a pane's node and its ancestors,
which the tree has loaded to show it; wave 1 checks that the API holds the chain for a node
opened by link with its ancestors collapsed, and asks for it if not), memoized
per `(store, node)` and invalidated by `NodeMoved`, a change to any `custom["theme"]` on
the chain, and a change to the theme document or its base.

### 3.3 App themes

**Decided (Joe, 2026-11-24, decision 6): app themes follow the account.** They are never
store content: a store holds how its trees read, not how one person's window looks, and a
theme kept in a store would leave the chrome depending on that store being open.

- **Where.** The personal document (`docs/DISCUSSIONS_CONTRACT.md` section 8,
  `crates/pimble-app/src/personal.rs`), the one yrs document per account edited only by
  that person's devices, gains two entries:
  ```
  app_themes: Map<theme id, Map>   a person's own app themes: name, base, tokens (color pairs)
  app_theme:  String               the one in use: a built-in id or an app_themes id
  ```
  Signed out, it stays on the device (`personal.yrs`, IndexedDB in the browser), as read
  state does. Signed in, it is encrypted for the person's own devices and synced, under
  the switch that already governs the personal document (`personal_sync`). This is the
  kind of account data CLAUDE.md allows ("a person's own state ... account data encrypted
  for their own devices"); it adds no store content to Pimble Cloud. CLAUDE.md's rule
  names this addition when it is built.
- **Same model as document themes.** An app theme has a base (a built-in or another of the
  person's app themes), sets only what it changes, and is edited in the same theme editor
  with "Customize..." (section 3.1); its tokens are section 2.2's. A personal app theme is
  edited by one person's devices only, so concurrent edits are that person's own on two
  devices, merged as any map.
- **No fallback puzzle.** The personal document is always present on every device that
  runs Pimble, so the theme in use is always there. The only fallback left is a built-in
  that a newer release removed, or a base deleted on another device: the chain then reads
  Default for what is missing.
- **Dark and light** stay a preference of their own, beside the theme: `dark_mode` as today
  (`state.json`, `localStorage` in the browser), Settings > General and View > "Toggle Dark
  Mode". Toggling keeps the theme and shows its other half, for the chrome and for every
  document theme on screen at once. The first start after this ships sets `app_theme` to
  `builtin:pimble`, today's look in both schemes, so nothing changes on screen.
- **Giving an app theme to someone else** is not in the first waves: there is no store to
  carry it. Later: "Copy as text" and "Paste theme" (the tokens as JSON, checked by type on
  the way in), which needs no new sync path.
- **A device of its own.** Should one device ever want a different theme from the account
  (a projector, a work screen), a per-device override in `state.json` can be added
  later; it is not in the first waves.

### 3.4 Built-ins

Built-ins ship with the app (`crates/pimble-themes`, a small crate of data: token tables
and their ids, both targets) and are referenced by id; they are never copied into a store
to be used. A store theme may derive from one (`base`). Built-ins are versioned with the
app: a later release may tune `builtin:book`, and every tree that uses it follows. A
person who wants it frozen makes a theme with that base and sets every token.

## 4. Paragraph spacing: subsumed

A document theme subsumes the paragraph setting. `spacing` (`compact`, `spaced`,
`indented`) is a token, so a book reads with indents, a report with gaps and a code tree
compact, and the theme says so for the whole tree.

Keeping both would be two settings for one margin, and the copy rule makes them collide:
every node under a folder made spaced before themes carries a copied `"spaced"`, so a book
theme applied to that folder would be overridden everywhere if the per-node key won.

Decided (Joe, 2026-11-24, decision 2: "it replaces compact/spaced"):
- **No new writes of `custom["paragraphs"]`.** Edit > "Compact Paragraphs" and the
  Appearance picker's Paragraphs row are replaced by the Theme item (section 9), and
  `custom_keys::INHERITED` drops it.
- **Read-time fallback, no migration writes.** When no node on the chain names a theme,
  a node whose own `paragraphs` is `"spaced"` draws with `builtin:document` (Default with
  `spacing = spaced`); otherwise Default. Every existing note looks exactly as it does today,
  nothing is rewritten in anyone's store (a migration that edits every node of every store,
  read-only shares included, is the kind of write CLAUDE.md warns about), and the first
  theme applied anywhere above a note replaces the fallback.
- **Importers** set `custom["theme"] = "builtin:document"` on the import's top node instead
  of `paragraphs` on every node.
- The per-note distinction Joe made on 2026-11-13 ("most notes are quick notes") survives:
  a quick-notes tree and a writing tree each get their theme, and a single note can still
  name its own.

## 5. Fonts

A theme names fonts by **family**, never by file or URL, from a fixed list Pimble provides on
both targets, all under the SIL Open Font License (Pimble ships no font today: rinch ships
none, and the wordmark's Manrope is drawn as outlines in `brand.rs`). Decided (Joe,
2026-11-24, decision 8): only these, no installed system fonts, and **more of them** than
the first proposal's six. The list (a proposal; add or cut freely):

| Kind | Families |
| --- | --- |
| Sans | Inter, Source Sans 3, IBM Plex Sans, Manrope, Work Sans |
| Readable | Atkinson Hyperlegible (low vision), Lexend, OpenDyslexic |
| Serif | Source Serif 4, Literata (made for long reading on screens), Merriweather, Lora, Crimson Pro, Libre Baskerville |
| Book | EB Garamond, Cormorant Garamond |
| Slab | Roboto Slab, Zilla Slab |
| Mono | JetBrains Mono, IBM Plex Mono, Fira Code, Courier Prime (manuscripts) |
| Writer | iA Writer Quattro / Duo (duospaced; OFL, from IBM Plex) |
| Hand | Caveat, Patrick Hand |
| System | none bundled: the platform's UI font, for a theme that wants to look native |

Each family names its own fallbacks by kind (serif, sans-serif, monospace), so text draws
while a face is not yet loaded.

How they ship (decision 8b, decided by Joe 2026-11-24 as recommended): about thirty families at a few weights each is 20–40 MB,
too much to put in every desktop binary and web bundle. *Recommendation:* a **core set is
bundled** (Inter, Source Serif 4, EB Garamond, JetBrains Mono, Atkinson Hyperlegible:
enough for every built-in theme), and the rest form a **font library** on pimble.app,
signed like the plugin library (`docs/SETTINGS_CONTRACT.md` decision 2) and fetched on
first use: by the theme editor when someone picks one, and by any device that opens a tree
whose theme names one it lacks. Fetched faces are kept in the user's data dir
(`pimble/fonts/`, the browser's cache) and work offline from then on. Until a face arrives,
its fallback draws; a device that is offline draws the fallback and fetches later. A
download is not hosting: nothing of the person's goes anywhere (CLAUDE.md "Nothing is
hosted unless asked"), and the font's name is all the request carries.

Bundling or fetching from one list is what makes "everyone sees the book layout" true: a
font named by the person's own system would draw differently on every device.

## 6. Copying themes between stores

Joe asked that themes be easy to copy from any other open store, "seamlessly in the
background". What can and cannot be seamless:

- **Copying is explicit and one time.** Picking a theme from another open store in the
  Theme picker copies it into this store (a new theme node made where new themes are made, section 3.1a,
  its tokens and its title, with `data.theme.copied_from = { store, node, at }` for
  provenance), then applies the copy. One click; no "import" step to think about. A base
  that is a built-in carries over by id; a base that is another theme node of the source
  store is flattened into the copy's own tokens.
- **What cannot be seamless: keeping the copy in step.** A copy that follows its original
  in the background is a mirror: one store's content rewritten by a device interpreting
  another store's edits, which is exactly what CLAUDE.md forbids ("Everything is
  co-editable": no mirrors, no copies someone keeps in step). It would also leak: edits in a
  private store would flow into a shared one without anyone choosing it.
- **What can feel seamless instead:**
  1. The picker lists every open store's themes beside this store's and the built-ins, so
     a theme from elsewhere is one click away, and applying it copies on first use.
  2. Copying the same original twice does not make a second copy: a theme in this store
     whose `copied_from` names the same original is reused.
  3. A copy shows "Copied from <theme> in <store>" and, when the original has changed
     since, an explicit **"Update from original"** action that copies its tokens again, as
     one edit the person chose (decision 9).
- **Referencing a theme in another store is not offered.** A cross-store reference would
  make one store's look depend on another being open, reachable and readable by every
  member of every share; mounts already show what that costs.

## 7. Drawing a document theme per pane, and rinch

What the app does (no rinch change needed for the core of it):
- `pane_view/pane.rs` already sets the wrapper's class per pane from the note
  (`pane_spaced`). It becomes `pane_theme(store, pane)`: the resolved theme's custom
  properties written as the wrapper's inline `style`, plus a class per enum token that
  changes structure rather than a value (`--para-indented`, `--quote-boxed`,
  `--table-rows`, `--rule-ornament`).
- `styles/document_theme.css` maps every token onto the editor's elements under
  `.pimble-editor__content-wrap > [data-pm-editor]`, with `var(--pd-..., <today's value>)`
  so Default is today's look to the pixel. `editor_content.css`'s spacing rules move there.
- Inline custom properties inherited by descendants already work on both targets: the
  plugin viewers rely on them (`pimble_ui::viewer::surfaces`, set on a viewer's root).

What rinch gives today (read at `e8e04ccc`, paths under `crates/`):
- **Desktop** cascades with Stylo: inline `style` custom properties are parsed and
  inherited by descendants (`rinch-dom/src/stylo_impl.rs:659`,
  `computed_style/from_stylo/mod.rs:320`), and `var(..., fallback)` in a stylesheet rule
  resolves them. The browser is real DOM, so all of CSS applies there.
- **Supported on the desktop:** `font-family`, `font-size`, `line-height`,
  `letter-spacing`, `word-spacing`, `text-transform`, `text-align: justify`, `text-indent`,
  `max-width`, `:first-child` and `p + p` (Stylo's selectors). That covers every token in
  section 2.1.
- **Not supported on the desktop:** `font-variant: small-caps`, `hyphens: auto` (no
  dictionary), multi-column layout, `::first-letter`. So no token offers small caps, drop
  caps or columns, and `justify` comes without hyphenation (decent at a book's measure,
  ragged gaps at narrow widths; the Book theme justifies only when `measure` is set).
  `text-indent` and `hyphens` go through a custom-property rewrite in rinch
  (`from_stylo/mod.rs:32-44`, `:239`, rinch #2870), so `var()` inside `text-indent` needs a
  test on both targets in wave 1.
- **Fonts:** the desktop takes bundled faces through `App::fonts(&[AppFont])` and
  `register_font_data` (`rinch/src/app_builder.rs:322`, `rinch/src/font.rs:260`), live after
  mount; there is no `@font-face` on the desktop. The browser ignores `App::fonts` and uses
  `@font-face`. So each stack in section 5 is registered once per target at start, by the
  same family name, and the tokens name only family names.
- **The theme sheet:** `ThemeProviderProps` has six fields (a built-in palette name for the
  primary, radius, `font_family`, `font_family_monospace`, scheme, primary shade); custom
  colors are only possible by overriding `--rinch-*` variables in the sheet, which is what
  `theme_css` already does. App themes keep doing that, and use `font_family` for the UI
  font.
- **Per editor:** `Editor {}` takes no class or style, but none is needed: Pimble's own
  wrapper already carries per-pane state, and rules under it outrank the editor's
  `[data-pm-editor]` defaults.

So the core needs **no rinch change**. The four gaps are filed as joeleaver/rinch#3038
(Joe, 2026-11-24: "a big deal"); tokens for small caps, drop caps and columns join
section 2.1 when it lands. Of them, two matter most: `font-variant-caps: small-caps`
(synthesized when the face has no `smcp`) in the desktop renderer, which would let
headings use small caps; and `hyphens: auto` with a dictionary, which would make justified
text good at any width. Until either lands, no token depends on it.

## 8. Plugins

- **App themes reach plugins without an ABI change**, once one thing is fixed in Pimble:
  `pimble_ui::viewer::surfaces(dark)` names palette steps (`--rinch-color-dark-6`,
  `--rinch-color-gray-1`, ...), which an app theme does not redefine, so a themed app would
  show stock-gray kanban columns. The fix is new semantic variables the app theme sets
  (`--pimble-panel`, `--pimble-panel-border`, `--pimble-item`, `--pimble-item-border`,
  defaulting to today's values in the two stock themes) and `surfaces` reading them.
  That is a `pimble-ui` change; plugins pick it up when rebuilt, and the library updates
  them on its own (`docs/SETTINGS_CONTRACT.md` decision 3). `HostState.dark` stays: it
  tells a plugin the scheme, which the app theme still has.
- **Document themes reach a plugin's host slots for free**: an `editor:<link>` slot is the
  host's own Editor in a host wrapper, themed as a pane is.
- **Plugins that draw text themselves** (a kanban card's title, a todo row) could read the
  document theme's fonts and colors too. Two ways (decision 10):
  1. **No ABI change** (recommended): the host writes the plugin's node's resolved
     document-theme properties on the plugin mount's root, and `pimble_ui::viewer` uses
     `var(--pd-font-body, inherit)` and friends where it draws node text. Plugins that want
     it use the variables; plugins that do not are unaffected.
  2. **ABI 7**: `HostState` gains `document_theme: Option<ThemeTokens>` (the resolved tokens
     of the plugin's node, typed, from `pimble-themes`), for a plugin that computes with
     them (a timeline sizing its rows to the body size). Only worth it when a plugin needs
     the values in code, not just in styles.
- A plugin may not define or apply themes through the ABI beyond what `Pimble` already lets
  it write (a plugin with write access could set `custom["theme"]` like any key, judged as
  any edit is).

## 9. The interface

- **Format > Theme...** (both targets; Edit > "Compact Paragraphs" leaves): the picker for
  the focused pane's node. Three groups: This store's themes, Built-in, and From other open
  stores (section 6). Each row draws a small sample in that theme. A line says what is in
  force and where it comes from ("Book, from 'Novel'", "Book, set here"), and "Use the
  parent's" clears the key. The store row's menu and "Appearance..." get the same item.
- **A theme node opens as the theme editor**: the tokens in groups (section 2.1), each with
  a control fitting its type; a token it does not set shows the inherited value greyed with
  where it comes from ("17 px, from Book"), a token it sets has a reset that clears it back
  to inherited, and a "Show only what this theme changes" switch lists the tweaks alone; a
  sample text
  under it drawn live, and a scheme switch to preview dark and light. Edits are `applyEdit`s
  of `data.theme`, so two people editing a theme see each other's changes as they land.
- **Settings > General > Theme**: the app theme, from the built-ins and the person's own
  app themes, with "Customize..." (a new personal app theme based on the one in use, opened
  in the theme editor).
- **Read-only**: a node this device may only read shows the picker disabled with
  `StoreAccess::READ_ONLY_REFUSAL`'s sentence, as every other edit does.

## 10. Web, MCP, export

- **Web parity.** One code base: the resolution, the CSS mapping, the picker and the editor
  are all in `pimble-app` and `pimble-themes`, both targets. The browser loads the bundled
  font faces lazily as `@font-face` from `/app/fonts/` on first use. A vault store's theme
  nodes are documents like any other; the vault client decrypts and serves them.
- **MCP** (`docs/MCP_CONTRACT.md` gains a section): `list_themes` (built-ins, and each open
  store's themes with links and kinds), `get_theme` (a theme's tokens, its base, what it
  inherits, and where it is applied, by the nodes naming it), `set_theme` (apply a theme to
  a node, or clear it: one `custom["theme"]` edit; a theme from another store is copied as
  the picker does), `create_theme` / `update_theme` (tokens by name, checked by type; a
  bad value refused with a sentence naming the token and its range). `get_node` adds one
  line: the theme in force and where from. Building and editing themes through the MCP
  is a requirement, not a nicety (Joe, 2026-11-24): an agent must be able to make a
  theme from a description, adjust it token by token, and do the same for app themes
  (the account's, in the personal document), so `create_theme` / `update_theme` take
  `kind: document | app`, `list_themes` lists both, and `delete_theme` removes a store
  theme (refused, with the nodes that name it, while any does).
- **Export, later.** A document theme is the natural style sheet for exporting a tree:
  Markdown export ignores it (Markdown has no styling), HTML export writes the same CSS
  the panes use, and a future PDF/EPUB export of a tree (a manuscript to a book) maps the
  tokens onto print: `measure` and `page_padding` to page size and margins, `spacing =
  indented` to typeset paragraphs, heading tokens to chapter openings, `rule.ornament` to
  section breaks. Tokens for print only (page size, running heads, chapter breaks) would be
  a group added then, ignored on screen. Nothing here is built for export now; the token
  set is chosen so it will not need redesigning for it.

## 11. The starting dozen

Document themes (all set no colors unless said, so they read in both schemes):
1. **Default**: today's look exactly (15 px sans, line height 1.7, compact).
2. **Document**: Default with spaced paragraphs; what spaced notes fall back to (section 4).
3. **Book**: serif body (`book`), 18 px, line height 1.55, 36 em measure centered, indented
   paragraphs, centered upper-case tracked headings, ornament rules, justified.
4. **Manuscript**: `serif` 16 px, double-spaced, indented paragraphs, wide margins, plain
   headings: a submission draft.
5. **Code**: `mono` body 14 px, compact, headings bold mono, code blocks shaded and
   unwrapped, tables in a grid: notes that are mostly code.
6. **Markdown**: plain `mono` text the size of the body, headings the same size but bold
   with no extra space, quotes as a bar: reads like the source.
7. **Report**: `sans` 15 px, spaced, numbered-feeling headings with rules below, tables with
   shaded headers.
8. **Journal**: `serif` 17 px, generous line height, 34 em measure, spaced: diary entries.
9. **Compact**: `sans` 13 px, line height 1.4, tight lists: dense reference notes.

App themes:
Each app theme has a light and a dark half:
10. **Pimble**: today's light scheme (the brand slate) and today's dark scheme.
11. **Paper**: warm off-white surfaces and sepia text in light, warm brown-black and parchment
    text in dark, `serif` UI font: pairs with Book.
12. **Contrast**: pure white or near-black surfaces, strong borders, saturated accent: for
    bright rooms, projectors and low vision.
13. **Dusk**: muted, low-contrast, cool grays in both halves: long sessions, late at night.
14. **Grove**: green-based (Joe, 2026-11-24: "some people just love green"): sage-tinted
    surfaces and a forest-green accent in light, deep green-black surfaces and a soft
    leaf-green accent in dark.

That is nine document themes and five app themes (fourteen); decision 11 asks which to
cut or add.

## 12. Decisions for Joe

1. **Tokens, not CSS. Decided (Joe, 2026-11-24):** a theme is a fixed set of typed tokens
   (section 2); there is no way to write CSS.
2. **Paragraph spacing. Decided (Joe, 2026-11-24):** themes replace Compact/Spaced: no new
   writes of `custom["paragraphs"]`, a read-time fallback to `builtin:document` so every
   existing note looks the same, importers name `builtin:document`, and Edit > "Compact
   Paragraphs" becomes Format > Theme (section 4).
3. **Looked up, not copied. Decided (Joe, 2026-11-24):** the theme in force is the one named
   by the node or its nearest ancestor, so a theme on any node with children applies to its
   whole subtree, notes already there included (section 3.2).
4. **Shares. Decided (Joe, 2026-11-24):** a scope's reach includes the theme documents in
   force in it, at a theme access chosen when the share is made: read-only (the default),
   write, or a copy of the theme made for this share that members may edit (section 3.2).
   Open detail: *recommendation:* write is offered only to editor shares, and the copy
   is made under the store root, outside the shared subtree; being internal (section 3.1a),
   it would not show in members' trees anyway.
5. **Mounts. Decided (Joe, 2026-11-24):** a mounted node keeps its native look: its theme
   is resolved in its own store, and the picker on a mount row acts on the source node.
6. **App themes. Decided (Joe, 2026-11-24):** they follow the account: a person's app themes
   and the one in use live in the personal document, synced to their own devices when
   signed in, never in a store (section 3.3). Every theme, app or document, has its own
   light and dark; the scheme stays a preference of its own (section 2.2).
7. **Colors in document themes. Decided (Joe, 2026-11-24):** optional, as a palette of
   named slots, each a light and dark pair, the values drawn as chosen (section 2.1).
   Themes inherit from a base and set only what they change; "Customize..." is how one is
   made (section 3.1).
8. **Fonts. Decided (Joe, 2026-11-24):** only Pimble's own list, no installed system fonts,
   and a longer list than first proposed (section 5). **8b, decided the same day:** a core
   set bundled, the rest a signed font library on pimble.app, fetched on first use and kept
   offline. Joe is edging toward supporting fonts installed on the desktop some day ("but...
   no, for now"); if that comes, it needs an answer for a shared theme naming a font other
   devices lack (the family's fallback draws there, and the theme editor says which devices
   will not see it), and it is a change to section 5 only.
9. **"Update from original". Decided (Joe, 2026-11-24):** yes, an explicit one-time re-copy,
   never automatic, in a later wave (section 6).
10. **Plugins. Decided (Joe, 2026-11-24, "for now"):** no ABI change (section 8); revisit
    when a plugin needs theme values in code.
11. **The starting set. Decided (Joe, 2026-11-24):** section 11's nine document themes and
    five app themes, Grove (green) added at his request.
12. **Where theme nodes live. Decided (Joe, 2026-11-24):** in the tree, as internal nodes
    hidden by their kind, each kind shown or hidden on its own from View > "Show Internal"
    (section 3.1a). No Themes folder.

## 13. Waves

1. **Tokens and drawing.** `pimble-themes` (tokens, types, checks, built-ins, CSS
   generation), `custom_keys::THEME`, resolution (chain, base), `document_theme.css`, the
   pane wrapper, bundled fonts, the paragraphs fallback, Format > Theme with built-ins only.
   Default and Document are pixel-identical to today (checked by screenshot on both
   targets).
2. **Theme nodes.** `node_type = "theme"`, internal kinds and View > "Show Internal"
   (section 3.1a), the theme editor pane, New Theme, bases, the picker's store group,
   copying from other open stores.
3. **App themes.** The app token set, the personal document's `app_themes` and
   `app_theme`, Settings > General, the viewer surface variables, Pimble (today's two
   schemes as one theme), Paper, Contrast and Dusk.
4. **Shares and mounts** per decisions 4 and 5, with the sharing harness
   (`crates/pimble-server/tests/common`) covering a member who holds neither the
   ancestors nor the theme node, at each of the three theme accesses; the Share dialog's
   theme access choice.
5. **MCP** tools, and "Update from original".

## 14. How it is checked

- Unit: every token's checks (ranges, enums, colors), CSS generation never emits anything
  but declarations of `--pd-*`/`--pimble-*` properties, resolution over chains with bases,
  moves and deletions of a theme node (a node naming a deleted theme falls back to the
  base chain above it, then Default, and says so in the picker).
- Convergence: two devices editing different tokens of one theme, and applying different
  themes to one node, converge (`three_replicas_converge` style).
- Screens: Default and Document identical to today; each built-in on desktop (GPU and
  `--cpu`) and in the browser side by side.
- Sharing: a member sees the owner's book layout with neither the ancestors nor the Themes
  folder in their replica; a read-only member's theme edit refused; a write member's edit
  reaching the owner; a copy-for-share member's edit changing only the copy.
